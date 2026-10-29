//! MOSS-TTS 离线语音合成与自包含音色容器解析引擎 (PRD-0058 / MOSS-TTS Integration Spec)
//!
//! 核心设计契约：
//! 1. 16 字节定长 Header 防御：Magic: 'MTTS' (4B) | Version (4B uint32) | MetaLength (4B uint32) | TotalFrames (4B uint32)；
//! 2. 读写物理分离：preset_voices.bin 位于安装包目录，严格只读解析；
//! 3. 内存热注册：通过 `register_voice` 动态注入用户特征码，完全消除重整写盘；
//! 4. 48kHz RVQ-16 离散声学还原管线：基于 ONNX Runtime 原生驱动 Prefill + DecodeStep + LocalFixedSampled + CodecDecode。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;
use std::sync::{Arc, Mutex};
use ndarray::{Array1, Array2, Array3, Array4};
use omni_core::{ModelTier, OmniOrtRuntime};
use ort::session::Session;
use tokio::sync::RwLock;
use tokenizers::Tokenizer;

/// 自包含音色归档 16 字节定长 Header 结构 (Little-Endian)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceArchiveHeader {
    pub magic: [u8; 4],
    pub version: u32,
    pub meta_length: u32,
    pub total_frames: u32,
}

pub const ARCHIVE_MAGIC: &[u8; 4] = b"MTTS";
pub const ARCHIVE_VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 16;
pub const RVQ_CODEBOOKS: usize = 16;
pub const BYTES_PER_FRAME: usize = RVQ_CODEBOOKS * 2; // 16 * 2 = 32 bytes

// MOSS-TTS 特殊 Token 约定
const AUDIO_PAD_TOKEN_ID: i32 = 1024;
const AUDIO_START_TOKEN_ID: i32 = 6;
const AUDIO_END_TOKEN_ID: i32 = 7;
const AUDIO_USER_SLOT_TOKEN_ID: i32 = 8;
const AUDIO_ASSISTANT_SLOT_TOKEN_ID: i32 = 9;

// MOSS-TTS 提示模板硬编码常数
const USER_PROMPT_PREFIX_TOKEN_IDS: &[i32] = &[
    600, 289, 10356, 13, 10356, 10425, 1860, 4546, 12907, 10363, 13325, 11492,
];
const USER_PROMPT_AFTER_REFERENCE_TOKEN_IDS: &[i32] = &[
    10356, 10425, 3965, 7738, 11492, 505, 587, 10356, 10425, 352, 500, 856, 11492, 505, 587,
    10356, 10425, 2128, 1247, 594, 11492, 505, 587, 10356, 10425, 348, 909, 561, 3648, 11492,
    505, 587, 10356, 10425, 2818, 1305, 355, 348, 909, 11492, 505, 587, 10356, 10425, 484,
    339, 10367, 783, 11492, 505, 587, 10356, 10425, 2427, 980, 11492,
];
const ASSISTANT_PROMPT_PREFIX_TOKEN_IDS: &[i32] = &[
    10356, 14, 5, 4, 8165, 430,
];

/// 内存常驻音色特征数据
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VoicePromptData {
    pub id: String,
    pub display_name: String,
    pub gender: String,
    pub lang: String,
    #[serde(default)]
    pub sid: Option<i32>,
    pub frames: usize,
    /// 形状: [frames, 16]，包含 16 个码本离散特征值 (int16)
    pub codes: Vec<Vec<i16>>,
}

/// 预置音色库 JSON Schema 单项描述
#[derive(Debug, Clone, serde::Deserialize)]
struct PresetVoiceItemMeta {
    id: String,
    #[serde(rename = "displayName")]
    display_name: String,
    gender: String,
    lang: String,
    #[serde(default)]
    sid: Option<i32>,
    frames: usize,
    offset_bytes: usize,
    length_bytes: usize,
}

/// 预置多音色归档 JSON 头部
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
struct PresetArchiveMeta {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub total_voices: usize,
    pub voices: Vec<PresetVoiceItemMeta>,
}

/// 用户自定义单音色自包含 JSON Schema
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
struct CustomVoiceItemMeta {
    #[serde(default)]
    pub version: u32,
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub gender: String,
    #[serde(default)]
    pub lang: String,
    #[serde(default)]
    pub sid: Option<i32>,
    pub frames: usize,
    #[serde(default)]
    pub offset_bytes: usize,
    #[serde(default)]
    pub length_bytes: usize,
}

/// 解析定长 16 字节二进制 Header
pub fn parse_archive_header(bytes: &[u8]) -> Result<VoiceArchiveHeader, String> {
    if bytes.len() < HEADER_SIZE {
        return Err(format!(
            "Buffer too short for MTTS header: expected >= 16 bytes, got {}",
            bytes.len()
        ));
    }

    let magic: [u8; 4] = bytes[0..4].try_into().unwrap();
    if &magic != ARCHIVE_MAGIC {
        return Err(format!(
            "Invalid magic header: expected 'MTTS', got {:?}",
            magic
        ));
    }

    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != ARCHIVE_VERSION {
        return Err(format!(
            "Unsupported archive version: expected 1, got {}",
            version
        ));
    }

    let meta_length = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let total_frames = u32::from_le_bytes(bytes[12..16].try_into().unwrap());

    let expected_total_size = HEADER_SIZE + (meta_length as usize) + (total_frames as usize * BYTES_PER_FRAME);
    if bytes.len() < expected_total_size {
        return Err(format!(
            "Archive corrupted or truncated: expected >= {} bytes, got {}",
            expected_total_size,
            bytes.len()
        ));
    }

    Ok(VoiceArchiveHeader {
        magic,
        version,
        meta_length,
        total_frames,
    })
}

/// 解析预置多音色只读归档 `preset_voices.bin`
pub fn parse_preset_voices_archive(bin_bytes: &[u8]) -> Result<Vec<VoicePromptData>, String> {
    let header = parse_archive_header(bin_bytes)?;

    let meta_start = HEADER_SIZE;
    let meta_end = meta_start + (header.meta_length as usize);
    let meta_json_slice = &bin_bytes[meta_start..meta_end];

    let meta: PresetArchiveMeta = serde_json::from_slice(meta_json_slice)
        .map_err(|e| format!("Failed to parse preset voices metadata JSON: {}", e))?;

    let data_start = meta_end;
    let data_block = &bin_bytes[data_start..];

    let mut result = Vec::with_capacity(meta.voices.len());

    for v in meta.voices {
        let expected_bytes = v.frames * BYTES_PER_FRAME;
        if v.offset_bytes + v.length_bytes > data_block.len() || v.length_bytes < expected_bytes {
            tracing::warn!(
                "[MOSS-TTS] 音色 {} 数据边界超出或长度不匹配 (offset: {}, len: {}, expected: {})，安全跳过",
                v.id, v.offset_bytes, v.length_bytes, expected_bytes
            );
            continue;
        }

        let slice = &data_block[v.offset_bytes..v.offset_bytes + expected_bytes];
        let mut codes = Vec::with_capacity(v.frames);

        for frame_idx in 0..v.frames {
            let frame_offset = frame_idx * BYTES_PER_FRAME;
            let frame_slice = &slice[frame_offset..frame_offset + BYTES_PER_FRAME];
            let mut row = Vec::with_capacity(RVQ_CODEBOOKS);
            for cb in 0..RVQ_CODEBOOKS {
                let val_bytes = &frame_slice[cb * 2..cb * 2 + 2];
                let val = i16::from_le_bytes([val_bytes[0], val_bytes[1]]);
                row.push(val);
            }
            codes.push(row);
        }

        result.push(VoicePromptData {
            id: v.id,
            display_name: v.display_name,
            gender: v.gender,
            lang: v.lang,
            sid: v.sid,
            frames: v.frames,
            codes,
        });
    }

    Ok(result)
}

/// 解析用户自定义单音色自包含归档 `<userData>/custom_voices/{voice_id}.bin`
pub fn parse_custom_voice_archive(bin_bytes: &[u8]) -> Result<VoicePromptData, String> {
    let header = parse_archive_header(bin_bytes)?;

    let meta_start = HEADER_SIZE;
    let meta_end = meta_start + (header.meta_length as usize);
    let meta_json_slice = &bin_bytes[meta_start..meta_end];

    let meta: CustomVoiceItemMeta = serde_json::from_slice(meta_json_slice)
        .map_err(|e| format!("Failed to parse custom voice metadata JSON: {}", e))?;

    let data_start = meta_end;
    let data_block = &bin_bytes[data_start..];
    let expected_bytes = meta.frames * BYTES_PER_FRAME;

    if data_block.len() < expected_bytes {
        return Err(format!(
            "Custom voice data block too short: expected {} bytes, got {}",
            expected_bytes,
            data_block.len()
        ));
    }

    let mut codes = Vec::with_capacity(meta.frames);
    for frame_idx in 0..meta.frames {
        let frame_offset = frame_idx * BYTES_PER_FRAME;
        let frame_slice = &data_block[frame_offset..frame_offset + BYTES_PER_FRAME];
        let mut row = Vec::with_capacity(RVQ_CODEBOOKS);
        for cb in 0..RVQ_CODEBOOKS {
            let val_bytes = &frame_slice[cb * 2..cb * 2 + 2];
            let val = i16::from_le_bytes([val_bytes[0], val_bytes[1]]);
            row.push(val);
        }
        codes.push(row);
    }

    Ok(VoicePromptData {
        id: meta.id,
        display_name: meta.display_name,
        gender: if meta.gender.is_empty() { "male".to_string() } else { meta.gender },
        lang: if meta.lang.is_empty() { "zh".to_string() } else { meta.lang },
        sid: meta.sid,
        frames: meta.frames,
        codes,
    })
}

/// 将 48000Hz 32-bit float 双声道 PCM 编码为标准 16-bit PCM 立体声 WAV 二进制数据
pub fn encode_stereo_f32_to_wav(samples_left: &[f32], samples_right: &[f32], sample_rate: u32) -> Vec<u8> {
    let num_samples = samples_left.len().min(samples_right.len());
    let num_channels: u16 = 2;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * (num_channels as u32) * (bits_per_sample as u32 / 8);
    let block_align = num_channels * (bits_per_sample / 8);
    let data_chunk_size = (num_samples * num_channels as usize * 2) as u32;
    let file_size = 36 + data_chunk_size;

    let mut wav = Vec::with_capacity(44 + data_chunk_size as usize);
    // RIFF chunk descriptor
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // "fmt " sub-chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // Subchunk1Size (16 for PCM)
    wav.extend_from_slice(&1u16.to_le_bytes());  // AudioFormat (1 for PCM)
    wav.extend_from_slice(&num_channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());

    // "data" sub-chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_chunk_size.to_le_bytes());

    // PCM 数据 (交错左右声道，[-1.0, 1.0] -> [-32768, 32767])
    for i in 0..num_samples {
        let l = (samples_left[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        let r = (samples_right[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        wav.extend_from_slice(&l.to_le_bytes());
        wav.extend_from_slice(&r.to_le_bytes());
    }

    wav
}

/// 基于纯 Rust WSOLA 算法，将已有 16-bit PCM WAV 音频毫秒级派生为目标语速的新 WAV
/// 同时自动识别、更新并保留原有自包含 'meta' Sub-chunk，彻底免除神经网络重复推理
pub fn scale_wav_speed(wav_bytes: &[u8], speed: f32) -> Result<Vec<u8>, String> {
    if wav_bytes.len() < 44 {
        return Err(format!("WAV data too short: expected >= 44 bytes, got {}", wav_bytes.len()));
    }

    if &wav_bytes[0..4] != b"RIFF" || &wav_bytes[8..12] != b"WAVE" {
        return Err("Invalid WAV: missing RIFF/WAVE signature".to_string());
    }

    let mut offset = 12;
    let mut fmt_parsed = false;
    let mut num_channels = 2u16;
    let mut sample_rate = 48000u32;
    let mut bits_per_sample = 16u16;

    let mut data_slice: Option<&[u8]> = None;
    let mut meta_slice: Option<&[u8]> = None;

    while offset + 8 <= wav_bytes.len() {
        let chunk_id = &wav_bytes[offset..offset + 4];
        let chunk_size = u32::from_le_bytes(wav_bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += 8;

        if chunk_id == b"fmt " {
            if offset + 16 > wav_bytes.len() {
                return Err("Truncated fmt chunk".to_string());
            }
            let audio_format = u16::from_le_bytes(wav_bytes[offset..offset + 2].try_into().unwrap());
            if audio_format != 1 {
                return Err(format!("Unsupported audio format: expected 1 (PCM), got {}", audio_format));
            }
            num_channels = u16::from_le_bytes(wav_bytes[offset + 2..offset + 4].try_into().unwrap());
            sample_rate = u32::from_le_bytes(wav_bytes[offset + 4..offset + 8].try_into().unwrap());
            bits_per_sample = u16::from_le_bytes(wav_bytes[offset + 14..offset + 16].try_into().unwrap());
            fmt_parsed = true;
        } else if chunk_id == b"data" {
            let end = (offset + chunk_size).min(wav_bytes.len());
            data_slice = Some(&wav_bytes[offset..end]);
        } else if chunk_id == b"meta" {
            let end = (offset + chunk_size).min(wav_bytes.len());
            meta_slice = Some(&wav_bytes[offset..end]);
        }

        // RIFF chunk 必须 2 字节对齐
        offset += chunk_size + (chunk_size % 2);
    }

    if !fmt_parsed {
        return Err("Missing 'fmt ' chunk in WAV".to_string());
    }
    let data_bytes = data_slice.ok_or_else(|| "Missing 'data' chunk in WAV".to_string())?;

    if bits_per_sample != 16 {
        return Err(format!("Only 16-bit PCM WAV is supported, got {}-bit", bits_per_sample));
    }

    if num_channels != 1 && num_channels != 2 {
        return Err(format!("Only 1 or 2 channels supported, got {}", num_channels));
    }

    // 转换 16-bit PCM 为 f32 [-1.0, 1.0]
    let bytes_per_sample = 2usize;
    let block_align = num_channels as usize * bytes_per_sample;
    let total_samples = data_bytes.len() / block_align;

    let mut samples_left = Vec::with_capacity(total_samples);
    let mut samples_right = Vec::with_capacity(total_samples);

    if num_channels == 2 {
        for i in 0..total_samples {
            let idx = i * 4;
            let l_raw = i16::from_le_bytes(data_bytes[idx..idx + 2].try_into().unwrap());
            let r_raw = i16::from_le_bytes(data_bytes[idx + 2..idx + 4].try_into().unwrap());
            samples_left.push(l_raw as f32 / 32768.0);
            samples_right.push(r_raw as f32 / 32768.0);
        }
    } else {
        // 单声道复制为双声道
        for i in 0..total_samples {
            let idx = i * 2;
            let m_raw = i16::from_le_bytes(data_bytes[idx..idx + 2].try_into().unwrap());
            let val = m_raw as f32 / 32768.0;
            samples_left.push(val);
            samples_right.push(val);
        }
    }

    // 执行纯 Rust WSOLA 变速不变调算法
    let (scaled_left, scaled_right) = if (speed - 1.0).abs() >= 0.01 {
        time_scale_stereo_pcm(&samples_left, &samples_right, speed, sample_rate as usize)
    } else {
        (samples_left, samples_right)
    };

    // 重新编码为标准立体声 WAV
    let mut out_wav = encode_stereo_f32_to_wav(&scaled_left, &scaled_right, sample_rate);

    // 若原文件包含 meta chunk，解析并更新其 speed 字段后追加
    if let Some(meta_raw) = meta_slice {
        if let Ok(mut meta_json) = serde_json::from_slice::<serde_json::Value>(meta_raw) {
            if let Some(obj) = meta_json.as_object_mut() {
                obj.insert("speed".to_string(), serde_json::json!(speed));
            }
            if let Ok(new_meta_bytes) = serde_json::to_vec(&meta_json) {
                let meta_size = new_meta_bytes.len() as u32;
                out_wav.extend_from_slice(b"meta");
                out_wav.extend_from_slice(&meta_size.to_le_bytes());
                out_wav.extend_from_slice(&new_meta_bytes);
                if new_meta_bytes.len() % 2 != 0 {
                    out_wav.push(0);
                }

                // 重新修正 RIFF 头部文件大小字段 (offset 4..8)
                let riff_total_size = (out_wav.len() - 8) as u32;
                out_wav[4..8].copy_from_slice(&riff_total_size.to_le_bytes());
            }
        }
    }

    Ok(out_wav)
}

/// 轻量极速 Xorshift 伪随机数生成器（生成 [0.001, 0.999] 均匀随机浮点数）
struct FastRng {
    state: u64,
}

impl FastRng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x853c49e6748fea9b } else { seed },
        }
    }

    fn next_f32(&mut self) -> f32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        let v = (self.state & 0x7FFFFFFF) as f32 / 2147483648.0;
        v.clamp(0.001, 0.999)
    }
}

/// 基于 WSOLA (Waveform Similarity Overlap-Add) 实现高质量立体声音频变速不变调
/// 纯 Rust 实现，保持 48kHz 立体声采样率不变、音调不变，彻底废除对 FFmpeg 的依赖
pub fn time_scale_stereo_pcm(
    samples_left: &[f32],
    samples_right: &[f32],
    speed: f32,
    sample_rate: usize,
) -> (Vec<f32>, Vec<f32>) {
    let speed = speed.clamp(0.5, 2.0);
    if (speed - 1.0).abs() < 0.01 || samples_left.is_empty() {
        return (samples_left.to_vec(), samples_right.to_vec());
    }

    let win_size = (sample_rate as f32 * 0.025) as usize; // ~1200 samples (25ms)
    let syn_hop = win_size / 2;                           // ~600 samples
    let ana_hop = ((syn_hop as f32 * speed).round() as usize).max(1);
    let search_range = win_size / 4;

    if samples_left.len() <= win_size + search_range {
        return (samples_left.to_vec(), samples_right.to_vec());
    }

    // 预计算汉宁窗 (Hanning Window)
    let window: Vec<f32> = (0..win_size)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (win_size as f32 - 1.0)).cos()))
        .collect();

    let num_input = samples_left.len().min(samples_right.len());
    let estimated_output_len = ((num_input as f32 / speed) * 1.05) as usize;
    let mut out_left = vec![0.0f32; estimated_output_len + win_size * 2];
    let mut out_right = vec![0.0f32; estimated_output_len + win_size * 2];
    let mut norm = vec![0.0f32; estimated_output_len + win_size * 2];

    let mut syn_pos = 0;
    let mut ana_pos = 0;

    while ana_pos + win_size + search_range < num_input {
        let mut best_offset = 0;
        let mut max_corr = f32::MIN;

        if syn_pos > 0 {
            let overlap_len = win_size - syn_hop;
            for offset in 0..search_range {
                let cand_pos = ana_pos + offset;
                if cand_pos + overlap_len >= num_input {
                    break;
                }
                let mut corr = 0.0f32;
                for k in 0..overlap_len {
                    let prev_sample = out_left[syn_pos + k];
                    let cand_sample = samples_left[cand_pos + k];
                    corr += prev_sample * cand_sample;
                }
                if corr > max_corr {
                    max_corr = corr;
                    best_offset = offset;
                }
            }
        }

        let actual_ana = ana_pos + best_offset;

        for i in 0..win_size {
            let w = window[i];
            let out_idx = syn_pos + i;
            if out_idx < out_left.len() && actual_ana + i < num_input {
                out_left[out_idx] += samples_left[actual_ana + i] * w;
                out_right[out_idx] += samples_right[actual_ana + i] * w;
                norm[out_idx] += w;
            }
        }

        syn_pos += syn_hop;
        ana_pos += ana_hop;
    }

    let actual_out_len = syn_pos + (win_size - syn_hop);
    let final_len = actual_out_len.min(out_left.len());
    let mut res_left = Vec::with_capacity(final_len);
    let mut res_right = Vec::with_capacity(final_len);

    for i in 0..final_len {
        let n = if norm[i] > 1e-4 { norm[i] } else { 1.0 };
        res_left.push(out_left[i] / n);
        res_right.push(out_right[i] / n);
    }

    (res_left, res_right)
}

/// 合成调用序号：用于区分冷启动（首包含 ORT 首跑 / 权重页首次触成本）与稳态耗时。
/// 仅作可观测性计数器，不参与任何判定。
static SYNTH_CALL_SEQ: AtomicU64 = AtomicU64::new(0);

/// MOSS-TTS 原生 ONNX 自回归推理器
pub struct MossTtsEngine {
    prefill_session: Mutex<Session>,
    decode_session: Mutex<Session>,
    local_fixed_session: Mutex<Session>,
    codec_decode_session: Mutex<Session>,
    codec_encode_session: Mutex<Session>,
    tokenizer: Tokenizer,
}

impl MossTtsEngine {
    /// 尝试从指定模型目录加载全套 MOSS-TTS ONNX 模型与分词器
    pub fn try_load(model_dir: &Path) -> Result<Self, String> {
        OmniOrtRuntime::ensure_initialized();

        let prefill_path = model_dir.join("moss_tts_prefill.onnx");
        let decode_path = model_dir.join("moss_tts_decode_step.onnx");
        let local_fixed_path = model_dir.join("moss_tts_local_fixed_sampled_frame.onnx");
        let codec_decode_path = model_dir.join("moss_audio_tokenizer_decode_full.onnx");
        let codec_encode_path = model_dir.join("moss_audio_tokenizer_encode.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !prefill_path.exists()
            || !decode_path.exists()
            || !local_fixed_path.exists()
            || !codec_decode_path.exists()
            || !codec_encode_path.exists()
            || !tokenizer_path.exists()
        {
            return Err(format!(
                "MOSS-TTS 模型或分词器文件不完整: {:?}",
                model_dir
            ));
        }

        let prefill_session = OmniOrtRuntime::build_session(&prefill_path, ModelTier::Tier2)
            .map_err(|e| format!("加载 moss_tts_prefill 失败: {}", e))?;
        let decode_session = OmniOrtRuntime::build_session(&decode_path, ModelTier::Tier2)
            .map_err(|e| format!("加载 moss_tts_decode_step 失败: {}", e))?;
        let local_fixed_session = OmniOrtRuntime::build_session(&local_fixed_path, ModelTier::Tier2)
            .map_err(|e| format!("加载 moss_tts_local_fixed 失败: {}", e))?;
        let codec_decode_session = OmniOrtRuntime::build_session(&codec_decode_path, ModelTier::Tier2)
            .map_err(|e| format!("加载 moss_audio_tokenizer_decode_full 失败: {}", e))?;
        let codec_encode_session = OmniOrtRuntime::build_session(&codec_encode_path, ModelTier::Tier2)
            .map_err(|e| format!("加载 moss_audio_tokenizer_encode 失败: {}", e))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| format!("加载 tokenizer.json 失败: {}", e))?;

        tracing::info!(
            "[MossTtsEngine] 成功加载 MOSS-TTS-Nano 5个核心 ONNX 模型与分词器: {:?}",
            model_dir
        );

        Ok(Self {
            prefill_session: Mutex::new(prefill_session),
            decode_session: Mutex::new(decode_session),
            local_fixed_session: Mutex::new(local_fixed_session),
            codec_decode_session: Mutex::new(codec_decode_session),
            codec_encode_session: Mutex::new(codec_encode_session),
            tokenizer,
        })
    }

    /// 执行单句 48kHz 离线语音合成
    pub fn synthesize(&self, text: &str, prompt_codes: &[Vec<i16>], speed: f32) -> Result<Vec<u8>, String> {
        // 分段耗时打点：prefill / decode 循环 / codec / 尾部后处理，用于定位首发音瓶颈段位
        let call_seq = SYNTH_CALL_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
        let t_enter = Instant::now();

        let mut clean_text = text.trim().to_string();
        if clean_text.is_empty() {
            return Err("Text is empty".to_string());
        }

        // 官方对齐标点自愈闭合：自回归模型依赖句末标点输出终止符，缺失时自动补齐防止尾部冗余静音或吞音
        let has_cjk = clean_text.chars().any(|c| {
            ('\u{4e00}'..='\u{9fff}').contains(&c)
                || ('\u{3400}'..='\u{4dbf}').contains(&c)
                || ('\u{3040}'..='\u{30ff}').contains(&c)
                || ('\u{ac00}'..='\u{d7af}').contains(&c)
        });
        const END_PUNCTUATION: &[char] = &[
            '.', '!', '?', '。', '！', '？', ';', '；', '…', '\'', '"', '”', '’', ')', '）', '】', '》'
        ];
        if let Some(last_ch) = clean_text.chars().last() {
            if !END_PUNCTUATION.contains(&last_ch) {
                if has_cjk {
                    clean_text.push('。');
                } else {
                    clean_text.push('.');
                }
            }
        }

        // 1. SentencePiece 分词
        let encoding = self
            .tokenizer
            .encode(clean_text.as_str(), false)
            .map_err(|e| format!("分词失败: {}", e))?;
        let text_tokens: Vec<i32> = encoding.get_ids().iter().map(|&id| id as i32).collect();
        if text_tokens.is_empty() {
            return Err("Text produced 0 tokens".to_string());
        }

        // 2. 构造 input_ids 与 attention_mask
        let mut input_rows: Vec<[i32; 17]> = Vec::new();

        // a. 前缀文本行
        for &tok in USER_PROMPT_PREFIX_TOKEN_IDS {
            let mut r = [AUDIO_PAD_TOKEN_ID; 17];
            r[0] = tok;
            input_rows.push(r);
        }
        {
            let mut r = [AUDIO_PAD_TOKEN_ID; 17];
            r[0] = AUDIO_START_TOKEN_ID;
            input_rows.push(r);
        }

        // b. 参考音色声学码（RVQ-16 codes）
        for code_frame in prompt_codes {
            let mut r = [AUDIO_PAD_TOKEN_ID; 17];
            r[0] = AUDIO_USER_SLOT_TOKEN_ID;
            for (cb, &v) in code_frame.iter().take(16).enumerate() {
                r[cb + 1] = v as i32;
            }
            input_rows.push(r);
        }

        // c. 后缀文本行
        {
            let mut r = [AUDIO_PAD_TOKEN_ID; 17];
            r[0] = AUDIO_END_TOKEN_ID;
            input_rows.push(r);
        }
        for &tok in USER_PROMPT_AFTER_REFERENCE_TOKEN_IDS {
            let mut r = [AUDIO_PAD_TOKEN_ID; 17];
            r[0] = tok;
            input_rows.push(r);
        }
        for &tok in &text_tokens {
            let mut r = [AUDIO_PAD_TOKEN_ID; 17];
            r[0] = tok;
            input_rows.push(r);
        }
        for &tok in ASSISTANT_PROMPT_PREFIX_TOKEN_IDS {
            let mut r = [AUDIO_PAD_TOKEN_ID; 17];
            r[0] = tok;
            input_rows.push(r);
        }
        {
            let mut r = [AUDIO_PAD_TOKEN_ID; 17];
            r[0] = AUDIO_START_TOKEN_ID;
            input_rows.push(r);
        }

        let seq_len = input_rows.len();
        let mut flat_input_ids = Vec::with_capacity(seq_len * 17);
        for r in &input_rows {
            flat_input_ids.extend_from_slice(r);
        }

        let input_ids_array = Array3::from_shape_vec((1, seq_len, 17), flat_input_ids)
            .map_err(|e| e.to_string())?;
        let attention_mask_array = Array2::from_shape_vec((1, seq_len), vec![1i32; seq_len])
            .map_err(|e| e.to_string())?;

        let prompt_ms = t_enter.elapsed().as_secs_f64() * 1000.0;

        // 3. Prefill
        let t_prefill = Instant::now();
        let prefill_sess = self.prefill_session.lock().unwrap();
        let prefill_inputs = ort::inputs![
            "input_ids" => input_ids_array,
            "attention_mask" => attention_mask_array,
        ]
        .map_err(|e| format!("Prefill inputs error: {}", e))?;

        let prefill_outputs = prefill_sess
            .run(prefill_inputs)
            .map_err(|e| format!("Prefill run error: {}", e))?;

        let mut global_hidden: Array2<f32>;
        let mut past_keys: Vec<Array4<f32>> = Vec::with_capacity(12);
        let mut past_values: Vec<Array4<f32>> = Vec::with_capacity(12);

        for _ in 0..12 {
            past_keys.push(Array4::zeros((1, 0, 12, 64)));
            past_values.push(Array4::zeros((1, 0, 12, 64)));
        }

        let mut extracted_global_hidden = None;
        for (name, val) in prefill_outputs.into_iter() {
            if name == "global_hidden" {
                if let Ok(t) = val.try_extract_tensor::<f32>() {
                    let shape = t.shape();
                    let s_len = shape[1];
                    let last_slice = t.slice(ndarray::s![0, s_len - 1, ..]);
                    let arr = Array2::from_shape_vec((1, 768), last_slice.to_vec())
                        .map_err(|e| e.to_string())?;
                    extracted_global_hidden = Some(arr);
                }
            } else if let Some(stripped) = name.strip_prefix("present_key_") {
                if let Ok(idx) = stripped.parse::<usize>() {
                    if idx < 12 {
                        if let Ok(t) = val.try_extract_tensor::<f32>() {
                            let s = t.shape();
                            let arr = Array4::from_shape_vec(
                                (s[0], s[1], s[2], s[3]),
                                t.as_slice().unwrap().to_vec(),
                            )
                            .map_err(|e| e.to_string())?;
                            past_keys[idx] = arr;
                        }
                    }
                }
            } else if let Some(stripped) = name.strip_prefix("present_value_") {
                if let Ok(idx) = stripped.parse::<usize>() {
                    if idx < 12 {
                        if let Ok(t) = val.try_extract_tensor::<f32>() {
                            let s = t.shape();
                            let arr = Array4::from_shape_vec(
                                (s[0], s[1], s[2], s[3]),
                                t.as_slice().unwrap().to_vec(),
                            )
                            .map_err(|e| e.to_string())?;
                            past_values[idx] = arr;
                        }
                    }
                }
            }
        }

        drop(prefill_sess);

        global_hidden = extracted_global_hidden
            .ok_or_else(|| "Failed to extract global_hidden from prefill".to_string())?;

        // prefill 耗时含 KV 缓存张量回搬（present_key_* / present_value_* -> ndarray）
        let prefill_ms = t_prefill.elapsed().as_secs_f64() * 1000.0;

        // 4. 自回归单步解码生成
        let mut past_valid_length = seq_len as i32;
        let mut repetition_seen_mask = Array3::<i32>::zeros((1, 16, 1024));
        let mut generated_frames: Vec<Vec<i32>> = Vec::new();

        let local_fixed_sess = self.local_fixed_session.lock().unwrap();
        let decode_sess = self.decode_session.lock().unwrap();

        let max_frames = 375;
        let mut local_ms_acc = 0.0f64;
        let mut step_decode_ms_acc = 0.0f64;
        let t_decode = Instant::now();
        let mut rng = FastRng::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(123456789),
        );

        for _step in 0..max_frames {
            let assistant_random_u = Array1::from_vec(vec![rng.next_f32()]);
            let mut u2_vals = Vec::with_capacity(16);
            for _ in 0..16 {
                u2_vals.push(rng.next_f32());
            }
            let audio_random_u = Array2::from_shape_vec((1, 16), u2_vals)
                .map_err(|e| e.to_string())?;

            let t_local = Instant::now();
            let local_inputs = ort::inputs![
                "global_hidden" => global_hidden.clone(),
                "repetition_seen_mask" => repetition_seen_mask.clone(),
                "assistant_random_u" => assistant_random_u,
                "audio_random_u" => audio_random_u,
            ]
            .map_err(|e| format!("Local fixed inputs error: {}", e))?;

            let local_outputs = local_fixed_sess
                .run(local_inputs)
                .map_err(|e| format!("Local fixed run error: {}", e))?;

            let mut should_continue = true;
            let mut frame_tokens = Vec::with_capacity(16);

            for (name, val) in local_outputs.into_iter() {
                if name == "should_continue" {
                    if let Ok(t) = val.try_extract_tensor::<i32>() {
                        if let Some(&flag) = t.as_slice().unwrap_or(&[]).first() {
                            if flag <= 0 {
                                should_continue = false;
                            }
                        }
                    }
                } else if name == "frame_token_ids" {
                    if let Ok(t) = val.try_extract_tensor::<i32>() {
                        frame_tokens = t.as_slice().unwrap_or(&[]).to_vec();
                    }
                }
            }

            local_ms_acc += t_local.elapsed().as_secs_f64() * 1000.0;

            if !should_continue || frame_tokens.len() < 16 {
                break;
            }

            for (cb, &tok) in frame_tokens.iter().enumerate() {
                if tok >= 0 && (tok as usize) < 1024 {
                    repetition_seen_mask[[0, cb, tok as usize]] = 1;
                }
            }

            let mut next_row = Array3::<i32>::from_elem((1, 1, 17), AUDIO_PAD_TOKEN_ID);
            next_row[[0, 0, 0]] = AUDIO_ASSISTANT_SLOT_TOKEN_ID;
            for cb in 0..16 {
                next_row[[0, 0, cb + 1]] = frame_tokens[cb];
            }

            generated_frames.push(frame_tokens);

            let past_valid_array = Array1::from_vec(vec![past_valid_length]);
            let t_step = Instant::now();

            let decode_inputs = ort::inputs![
                "input_ids" => next_row,
                "past_valid_lengths" => past_valid_array,
                "past_key_0" => past_keys[0].clone(),
                "past_value_0" => past_values[0].clone(),
                "past_key_1" => past_keys[1].clone(),
                "past_value_1" => past_values[1].clone(),
                "past_key_2" => past_keys[2].clone(),
                "past_value_2" => past_values[2].clone(),
                "past_key_3" => past_keys[3].clone(),
                "past_value_3" => past_values[3].clone(),
                "past_key_4" => past_keys[4].clone(),
                "past_value_4" => past_values[4].clone(),
                "past_key_5" => past_keys[5].clone(),
                "past_value_5" => past_values[5].clone(),
                "past_key_6" => past_keys[6].clone(),
                "past_value_6" => past_values[6].clone(),
                "past_key_7" => past_keys[7].clone(),
                "past_value_7" => past_values[7].clone(),
                "past_key_8" => past_keys[8].clone(),
                "past_value_8" => past_values[8].clone(),
                "past_key_9" => past_keys[9].clone(),
                "past_value_9" => past_values[9].clone(),
                "past_key_10" => past_keys[10].clone(),
                "past_value_10" => past_values[10].clone(),
                "past_key_11" => past_keys[11].clone(),
                "past_value_11" => past_values[11].clone(),
            ]
            .map_err(|e| format!("Decode inputs error: {}", e))?;

            let decode_outputs = decode_sess
                .run(decode_inputs)
                .map_err(|e| format!("Decode step run error: {}", e))?;

            for (name, val) in decode_outputs.into_iter() {
                if name == "global_hidden" {
                    if let Ok(t) = val.try_extract_tensor::<f32>() {
                        let arr = Array2::from_shape_vec((1, 768), t.as_slice().unwrap().to_vec())
                            .map_err(|e| e.to_string())?;
                        global_hidden = arr;
                    }
                } else if let Some(stripped) = name.strip_prefix("present_key_") {
                    if let Ok(idx) = stripped.parse::<usize>() {
                        if idx < 12 {
                            if let Ok(t) = val.try_extract_tensor::<f32>() {
                                let s = t.shape();
                                let arr = Array4::from_shape_vec(
                                    (s[0], s[1], s[2], s[3]),
                                    t.as_slice().unwrap().to_vec(),
                                )
                                .map_err(|e| e.to_string())?;
                                past_keys[idx] = arr;
                            }
                        }
                    }
                } else if let Some(stripped) = name.strip_prefix("present_value_") {
                    if let Ok(idx) = stripped.parse::<usize>() {
                        if idx < 12 {
                            if let Ok(t) = val.try_extract_tensor::<f32>() {
                                let s = t.shape();
                                let arr = Array4::from_shape_vec(
                                    (s[0], s[1], s[2], s[3]),
                                    t.as_slice().unwrap().to_vec(),
                                )
                                .map_err(|e| e.to_string())?;
                                past_values[idx] = arr;
                            }
                        }
                    }
                }
            }

            step_decode_ms_acc += t_step.elapsed().as_secs_f64() * 1000.0;

            past_valid_length += 1;
        }

        let decode_ms = t_decode.elapsed().as_secs_f64() * 1000.0;
        // 帧数触及 max_frames 上限即为静默截断：此时 decode 很快但音频被切断，必须与正常收尾区分
        let hit_frame_cap = generated_frames.len() >= max_frames;

        drop(local_fixed_sess);
        drop(decode_sess);

        if generated_frames.is_empty() {
            return Err("MOSS-TTS 未生成任何有效音频帧".to_string());
        }

        // 5. Codec 解码为 48kHz 立体声 PCM
        let t_codec = Instant::now();
        let total_frames = generated_frames.len();
        let mut flat_codes = Vec::with_capacity(total_frames * 16);
        for frame in &generated_frames {
            flat_codes.extend_from_slice(frame);
        }

        let audio_codes = Array3::from_shape_vec((1, total_frames, 16), flat_codes)
            .map_err(|e| e.to_string())?;
        let audio_code_lengths = Array1::from_vec(vec![total_frames as i32]);

        let codec_sess = self.codec_decode_session.lock().unwrap();
        let codec_inputs = ort::inputs![
            "audio_codes" => audio_codes,
            "audio_code_lengths" => audio_code_lengths,
        ]
        .map_err(|e| format!("Codec inputs error: {}", e))?;

        let codec_outputs = codec_sess
            .run(codec_inputs)
            .map_err(|e| format!("Codec run error: {}", e))?;

        let mut samples_left = Vec::new();
        let mut samples_right = Vec::new();

        for (name, val) in codec_outputs.into_iter() {
            if name == "audio" {
                if let Ok(t) = val.try_extract_tensor::<f32>() {
                    let shape = t.shape();
                    let num_samples = shape[2];
                    let slice = t.as_slice().unwrap();
                    samples_left = slice[0..num_samples].to_vec();
                    samples_right = slice[num_samples..num_samples * 2].to_vec();
                }
            }
        }

        drop(codec_sess);

        if samples_left.is_empty() {
            return Err("Codec 解码音频数据为空".to_string());
        }

        // codec 耗时含离散码张量编组与 PCM 回搬
        let codec_ms = t_codec.elapsed().as_secs_f64() * 1000.0;

        // 6. 原生倍率声学调速 (WSOLA 纯 Rust 变速不变调，彻底废除 FFmpeg 外部进程调用)
        let t_post = Instant::now();
        let (left_final, right_final) = if (speed - 1.0).abs() >= 0.01 {
            time_scale_stereo_pcm(&samples_left, &samples_right, speed, 48000)
        } else {
            (samples_left, samples_right)
        };

        let wav_bytes = encode_stereo_f32_to_wav(&left_final, &right_final, 48000);
        let post_ms = t_post.elapsed().as_secs_f64() * 1000.0;

        // 分段耗时汇总：rtf = 总耗时 / 音频时长，用于判断该句是否快于实时消耗
        let audio_sec = left_final.len() as f64 / 48000.0;
        let total_ms = t_enter.elapsed().as_secs_f64() * 1000.0;
        let rtf = if audio_sec > 0.0 { total_ms / 1000.0 / audio_sec } else { 0.0 };
        let steps = generated_frames.len();
        let avg_step_ms = if steps > 0 { (local_ms_acc + step_decode_ms_acc) / steps as f64 } else { 0.0 };
        tracing::info!(
            "[MossTtsPerf] call={} chars={} seq_len={} steps={} hit_cap={} | prompt={:.1}ms prefill={:.1}ms decode={:.1}ms[local={:.1} decode={:.1} avg={:.2}/step] codec={:.1}ms post={:.1}ms | total={:.1}ms audio={:.2}s rtf={:.2} speed={:.2} wav={}B",
            call_seq,
            clean_text.chars().count(),
            seq_len,
            steps,
            hit_frame_cap,
            prompt_ms,
            prefill_ms,
            decode_ms,
            local_ms_acc,
            step_decode_ms_acc,
            avg_step_ms,
            codec_ms,
            post_ms,
            total_ms,
            audio_sec,
            rtf,
            speed,
            wav_bytes.len()
        );

        Ok(wav_bytes)
    }

    /// 将 16kHz 单声道样本重采样为 48kHz Planar 双声道张量 [1, 2, num_samples_48k]
    /// 采用 Catmull-Rom 三次样条插值，避免阶跃并平滑抑制高频失真
    pub fn resample_16k_mono_to_48k_stereo_planar(samples_16k: &[f32]) -> Result<Array3<f32>, String> {
        if samples_16k.is_empty() {
            return Err("Input samples are empty".to_string());
        }

        let n_in = samples_16k.len();
        let n_out = n_in * 3;
        let mut mono_48k = Vec::with_capacity(n_out);

        for i in 0..n_out {
            let t = i as f32 / 3.0;
            let idx = t.floor() as usize;
            let tau = t - (idx as f32);

            let p0 = if idx == 0 { samples_16k[0] } else { samples_16k[idx - 1] };
            let p1 = samples_16k[idx.min(n_in - 1)];
            let p2 = samples_16k[(idx + 1).min(n_in - 1)];
            let p3 = samples_16k[(idx + 2).min(n_in - 1)];

            // Catmull-Rom 插值多项式系数
            let a0 = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
            let a1 = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
            let a2 = -0.5 * p0 + 0.5 * p2;
            let a3 = p1;

            let val = ((a0 * tau + a1) * tau + a2) * tau + a3;
            mono_48k.push(val.clamp(-1.0, 1.0));
        }

        // 构造 Planar 格式 [1, 2, n_out]
        // 内存排布：Channel 0 全部连续在前，Channel 1 全部连续在后
        let mut planar_data = Vec::with_capacity(2 * n_out);
        planar_data.extend_from_slice(&mono_48k); // Channel 0 (left)
        planar_data.extend_from_slice(&mono_48k); // Channel 1 (right)

        Array3::from_shape_vec((1, 2, n_out), planar_data)
            .map_err(|e| format!("Failed to create planar Array3: {}", e))
    }

    /// 执行 48kHz Codec 编码推理，从 16kHz 干声提取 RVQ-16 离散特征码
    pub fn extract_codes(&self, samples_16k: &[f32]) -> Result<Vec<Vec<i16>>, String> {
        if samples_16k.is_empty() {
            return Err("Input vocal stem is empty".to_string());
        }

        let planar_waveform = Self::resample_16k_mono_to_48k_stereo_planar(samples_16k)?;
        let num_samples_48k = samples_16k.len() * 3;
        let input_lengths = Array1::from_vec(vec![num_samples_48k as i32]);

        let encode_sess = self
            .codec_encode_session
            .lock()
            .map_err(|e| format!("Lock codec_encode_session failed: {}", e))?;

        let encode_inputs = ort::inputs![
            "waveform" => planar_waveform,
            "input_lengths" => input_lengths,
        ]
        .map_err(|e| format!("Codec encode inputs error: {}", e))?;

        let encode_outputs = encode_sess
            .run(encode_inputs)
            .map_err(|e| format!("Codec encode run error: {}", e))?;

        let mut valid_len: usize = 0;
        let mut raw_codes_tensor: Option<(Vec<i32>, Vec<usize>)> = None;

        for (name, val) in encode_outputs.into_iter() {
            if name == "audio_code_lengths" {
                if let Ok(t) = val.try_extract_tensor::<i32>() {
                    if let Some(&l) = t.as_slice().unwrap_or(&[]).first() {
                        valid_len = l.max(0) as usize;
                    }
                }
            } else if name == "audio_codes" {
                if let Ok(t) = val.try_extract_tensor::<i32>() {
                    let shape = t.shape().to_vec();
                    let slice = t.as_slice().unwrap_or(&[]).to_vec();
                    raw_codes_tensor = Some((slice, shape));
                }
            }
        }

        let (slice, shape) = raw_codes_tensor
            .ok_or_else(|| "Codec encode output 'audio_codes' missing".to_string())?;

        if shape.len() != 3 || shape[2] != 16 {
            return Err(format!("Unexpected audio_codes shape: {:?}", shape));
        }

        let total_frames = shape[1];
        if slice.len() < total_frames * 16 {
            return Err(format!(
                "Codec encode output tensor data buffer mismatch: expected at least {} elements, got {}",
                total_frames * 16,
                slice.len()
            ));
        }

        let effective_frames = if valid_len > 0 && valid_len <= total_frames {
            valid_len
        } else {
            total_frames
        };

        if effective_frames == 0 {
            return Err("Codec encode extracted 0 frames".to_string());
        }

        let mut result_codes = Vec::with_capacity(effective_frames);
        for frame_idx in 0..effective_frames {
            let offset = frame_idx * 16;
            let mut row = Vec::with_capacity(16);
            for cb in 0..16 {
                let val = slice[offset + cb];
                row.push(val as i16);
            }
            result_codes.push(row);
        }

        tracing::info!(
            "[MossTtsEngine] 成功提取 RVQ-16 声学特征码: 总帧数={}, 有效帧数={}",
            total_frames,
            effective_frames
        );

        Ok(result_codes)
    }
}

/// 全局常驻 MOSS-TTS 音色与状态管理器
pub struct MossTtsManager {
    /// 内存常驻音色库 (内置 51 个 preset + 动态 register 的 custom 音色)
    voices: Arc<RwLock<HashMap<String, VoicePromptData>>>,
    /// MOSS-TTS ONNX 原生推理引擎
    engine: Option<Arc<MossTtsEngine>>,
    /// 单并发互斥信号量 (防止自回归循环打爆 CPU)
    semaphore: Arc<tokio::sync::Semaphore>,
    /// 引擎就绪标志
    is_ready: AtomicBool,
}

impl MossTtsManager {
    /// 实例化并尝试加载 `preset_voices.bin` 与 MOSS-TTS ONNX 引擎
    pub fn new() -> Self {
        let mut initial_map = HashMap::new();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let is_ready = AtomicBool::new(false);

        // 尝试定位并加载预置音色库
        if let Some(bin_path) = Self::resolve_preset_voices_bin() {
            if let Ok(bytes) = std::fs::read(&bin_path) {
                match parse_preset_voices_archive(&bytes) {
                    Ok(parsed_voices) => {
                        let total = parsed_voices.len();
                        for v in parsed_voices {
                            initial_map.insert(v.id.clone(), v);
                        }
                        tracing::info!(
                            "[MossTtsManager] 成功加载预置只读音色库: {:?}, 共 {} 个音色",
                            bin_path,
                            total
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[MossTtsManager] 解析预置音色库失败: {:?}, error: {}",
                            bin_path,
                            e
                        );
                    }
                }
            }
        } else {
            tracing::info!("[MossTtsManager] 未找到 preset_voices.bin，初始化为空音色库");
        }

        // 尝试定位模型目录并初始化 MossTtsEngine
        let engine = if let Some(model_dir) = Self::resolve_moss_model_dir() {
            match MossTtsEngine::try_load(&model_dir) {
                Ok(eng) => {
                    is_ready.store(true, Ordering::SeqCst);
                    tracing::info!("[MossTtsManager] MOSS-TTS 48kHz 原生引擎就绪！");
                    Some(Arc::new(eng))
                }
                Err(err) => {
                    tracing::warn!("[MossTtsManager] 初始化 MOSS-TTS 原生引擎失败: {}", err);
                    None
                }
            }
        } else {
            tracing::info!("[MossTtsManager] 未找到 MOSS-TTS ONNX 模型目录，离线语音合成引擎处于软降级未就绪状态");
            None
        };

        Self {
            voices: Arc::new(RwLock::new(initial_map)),
            engine,
            semaphore,
            is_ready,
        }
    }

    /// 辅助：定位 MOSS-TTS 模型目录
    fn resolve_moss_model_dir() -> Option<PathBuf> {
        // 1. 优先读取显式环境变量下发路径 (最高优先级)
        if let Ok(env_path) = std::env::var("OMNI_MOSS_MODEL_PATH") {
            let p = PathBuf::from(env_path);
            if p.join("moss_tts_prefill.onnx").exists() {
                return Some(p);
            }
        }

        // 2. 向上回溯探测相对路径
        let search_roots = [
            std::env::current_dir().unwrap_or_default(),
            std::env::current_exe()
                .map(|p| p.parent().unwrap_or(p.as_path()).to_path_buf())
                .unwrap_or_default(),
        ];

        for root in &search_roots {
            let mut cur = root.clone();
            for _ in 0..8 {
                let candidates = [
                    cur.join("apps/desktop/build/extraResources/models/audio/moss"),
                    cur.join("resources/models/audio/moss"),
                    cur.join("models/audio/moss"),
                ];
                for p in &candidates {
                    if p.join("moss_tts_prefill.onnx").exists() {
                        return Some(p.clone());
                    }
                }
                if let Some(parent) = cur.parent() {
                    cur = parent.to_path_buf();
                } else {
                    break;
                }
            }
        }

        // 3. 用户全局缓存目录回退 (防跨 worktree 丢失)
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            let home_cache = PathBuf::from(home)
                .join(".cache/moss_tts_test/models/MOSS-TTS-Nano-100M-ONNX");
            if home_cache.join("moss_tts_prefill.onnx").exists() {
                return Some(home_cache);
            }
        }

        None
    }

    /// 辅助：定位预置音色库路径
    fn resolve_preset_voices_bin() -> Option<PathBuf> {
        // 1. 优先读取显式环境变量下发路径 (最高优先级)
        if let Ok(env_path) = std::env::var("OMNI_MOSS_VOICES_PATH") {
            let p = PathBuf::from(env_path);
            if p.exists() {
                return Some(p);
            }
        }

        // 2. 向上回溯探测相对路径
        let search_roots = [
            std::env::current_dir().unwrap_or_default(),
            std::env::current_exe()
                .map(|p| p.parent().unwrap_or(p.as_path()).to_path_buf())
                .unwrap_or_default(),
        ];

        for root in &search_roots {
            let mut cur = root.clone();
            for _ in 0..8 {
                let candidates = [
                    cur.join("apps/desktop/build/extraResources/models/audio/moss/preset_voices.bin"),
                    cur.join("resources/models/audio/moss/preset_voices.bin"),
                    cur.join("models/audio/moss/preset_voices.bin"),
                    cur.join("preset_voices.bin"),
                ];
                for p in &candidates {
                    if p.exists() {
                        return Some(p.clone());
                    }
                }
                if let Some(parent) = cur.parent() {
                    cur = parent.to_path_buf();
                } else {
                    break;
                }
            }
        }

        // 3. 用户全局缓存目录回退 (防跨 worktree 丢失)
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            let home_cache = PathBuf::from(home).join(".cache/moss_tts_test/test_voices.bin");
            if home_cache.exists() {
                return Some(home_cache);
            }
        }

        None
    }

    /// 执行 48kHz 原生离线语音合成
    pub fn synthesize(&self, text: &str, voice: &VoicePromptData, speed: f32) -> Result<Vec<u8>, String> {
        let engine = self
            .engine
            .as_ref()
            .ok_or_else(|| "MOSS-TTS 原生 ONNX 引擎未就绪或模型文件缺失".to_string())?;

        engine.synthesize(text, &voice.codes, speed)
    }

    /// 内存热注册音色特征码 (支持用户自定义音色零盘刷更新与自愈重放)
    pub async fn register_voice(
        &self,
        id: String,
        display_name: Option<String>,
        gender: Option<String>,
        lang: Option<String>,
        sid: Option<i32>,
        frames: usize,
        codes: Vec<Vec<i16>>,
    ) -> Result<(), String> {
        if id.trim().is_empty() {
            return Err("voice_id cannot be empty".to_string());
        }
        if frames == 0 || codes.is_empty() {
            return Err("codes cannot be empty".to_string());
        }

        let voice = VoicePromptData {
            id: id.clone(),
            display_name: display_name.unwrap_or_else(|| id.clone()),
            gender: gender.unwrap_or_else(|| "male".to_string()),
            lang: lang.unwrap_or_else(|| "zh".to_string()),
            sid,
            frames,
            codes,
        };

        let mut map = self.voices.write().await;
        map.insert(id.clone(), voice);
        tracing::info!("[MossTtsManager] 成功内存热注册音色: {} (总帧数: {})", id, frames);
        Ok(())
    }

    /// 获取指定音色的特征数据（支持前缀容错）
    pub async fn get_voice(&self, id: &str) -> Option<VoicePromptData> {
        let map = self.voices.read().await;
        if let Some(v) = map.get(id) {
            return Some(v.clone());
        }
        if let Some(stripped) = id.strip_prefix("preset_") {
            if let Some(v) = map.get(stripped) {
                return Some(v.clone());
            }
        } else {
            let prefixed = format!("preset_{}", id);
            if let Some(v) = map.get(&prefixed) {
                return Some(v.clone());
            }
        }
        None
    }

    /// 获取当前常驻的全部音色 ID 列表
    pub async fn list_voice_ids(&self) -> Vec<String> {
        let map = self.voices.read().await;
        map.keys().cloned().collect()
    }

    /// 当前音色总数
    pub async fn total_voices(&self) -> usize {
        let map = self.voices.read().await;
        map.len()
    }

    /// 获取引擎是否就绪
    pub fn is_ready(&self) -> bool {
        self.is_ready.load(Ordering::SeqCst) && self.engine.is_some()
    }

    /// 获取并发信号量
    pub fn semaphore(&self) -> Arc<tokio::sync::Semaphore> {
        self.semaphore.clone()
    }

    /// 从 16kHz 干声中提取 48kHz RVQ-16 离散声学特征码
    pub fn extract_voice_codes(&self, samples_16k: &[f32]) -> Result<Vec<Vec<i16>>, String> {
        let engine = self
            .engine
            .as_ref()
            .ok_or_else(|| "MOSS-TTS 原生 ONNX 引擎未就绪或模型文件缺失".to_string())?;

        engine.extract_codes(samples_16k)
    }
}

impl Default for MossTtsManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_real_preset_voices_bin() {
        let bin_path = MossTtsManager::resolve_preset_voices_bin();
        if let Some(path) = bin_path {
            println!("Found preset_voices.bin at {:?}", path);
            let bytes = std::fs::read(&path).expect("Should read preset_voices.bin");
            let voices = parse_preset_voices_archive(&bytes).expect("Should parse preset_voices.bin successfully");
            println!("Parsed {} voices from preset_voices.bin", voices.len());
            assert_eq!(voices.len(), 51, "内置音色库应精准包含 51 个音色");

            let yunxi = voices.iter().find(|v| v.id == "azure_yunxi" || v.id == "preset_azure_yunxi").expect("应包含云希音色");
            assert_eq!(yunxi.display_name, "云希");
            assert_eq!(yunxi.lang, "zh");
            assert_eq!(yunxi.gender, "male");
            assert!(yunxi.frames > 0);
            assert_eq!(yunxi.codes.len(), yunxi.frames);
            assert_eq!(yunxi.codes[0].len(), 16);
        } else {
            println!("Skip test_parse_real_preset_voices_bin: preset_voices.bin not found in workspace");
        }
    }

    #[tokio::test]
    async fn test_moss_tts_manager_register_and_lookup() {
        let manager = MossTtsManager::new();
        let dummy_codes = vec![vec![1i16; 16]; 10];

        manager
            .register_voice(
                "custom_test_1".to_string(),
                Some("测试音色".to_string()),
                Some("female".to_string()),
                Some("zh".to_string()),
                Some(44),
                10,
                dummy_codes,
            )
            .await
            .expect("Should register custom voice");

        let voice = manager.get_voice("custom_test_1").await.expect("Should find registered voice");
        assert_eq!(voice.id, "custom_test_1");
        assert_eq!(voice.display_name, "测试音色");
        assert_eq!(voice.frames, 10);
        assert_eq!(voice.codes.len(), 10);
    }

    #[tokio::test]
    async fn test_moss_tts_end_to_end_synthesis() {
        let manager = MossTtsManager::new();
        if !manager.is_ready() {
            println!("Skip test_moss_tts_end_to_end_synthesis: MOSS-TTS models not found in workspace");
            return;
        }

        let yunxi = manager.get_voice("preset_azure_yunxi").await.expect("Should find preset_azure_yunxi");
        let wav_bytes = manager.synthesize("你好，欢迎使用下一代文件管理系统", &yunxi, 1.0)
            .expect("MOSS-TTS 48kHz 端到端合成应成功");

        println!("Synthesized wav_bytes len: {}", wav_bytes.len());
        assert!(wav_bytes.len() > 44, "WAV output must be longer than 44-byte header");
        assert_eq!(&wav_bytes[0..4], b"RIFF", "Header magic should be RIFF");
        assert_eq!(&wav_bytes[8..12], b"WAVE", "Header format should be WAVE");
        let sample_rate = u32::from_le_bytes(wav_bytes[24..28].try_into().unwrap());
        assert_eq!(sample_rate, 48000, "MOSS-TTS 采样率应为 48000Hz");
        let channels = u16::from_le_bytes(wav_bytes[22..24].try_into().unwrap());
        assert_eq!(channels, 2, "MOSS-TTS 声道数应为 2 (立体声)");
    }

    #[test]
    fn test_time_scale_stereo_pcm() {
        let sample_rate = 48000;
        let num_samples = 48000; // 1秒 440Hz 正弦波
        let mut left = Vec::with_capacity(num_samples);
        let mut right = Vec::with_capacity(num_samples);
        for i in 0..num_samples {
            let t = i as f32 / sample_rate as f32;
            let sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            left.push(sample);
            right.push(sample);
        }

        // 1.0x 语速原样输出
        let (s1_left, s1_right) = time_scale_stereo_pcm(&left, &right, 1.0, sample_rate);
        assert_eq!(s1_left.len(), num_samples);
        assert_eq!(s1_right.len(), num_samples);

        // 1.5x 语速加速输出（长度压缩至 ~67%）
        let (s15_left, s15_right) = time_scale_stereo_pcm(&left, &right, 1.5, sample_rate);
        assert!(s15_left.len() < num_samples);
        let ratio15 = s15_left.len() as f32 / num_samples as f32;
        assert!((ratio15 - 1.0 / 1.5).abs() < 0.1, "1.5x 长度比应逼近 0.67 (实测: {})", ratio15);
        assert_eq!(s15_left.len(), s15_right.len());
        for &s in &s15_left {
            assert!(!s.is_nan() && !s.is_infinite());
        }

        // 0.75x 语速减速输出（长度扩展至 ~133%）
        let (s075_left, s075_right) = time_scale_stereo_pcm(&left, &right, 0.75, sample_rate);
        assert!(s075_left.len() > num_samples);
        let ratio075 = s075_left.len() as f32 / num_samples as f32;
        assert!((ratio075 - 1.0 / 0.75).abs() < 0.1, "0.75x 长度比应逼近 1.33 (实测: {})", ratio075);
        assert_eq!(s075_left.len(), s075_right.len());
        for &s in &s075_right {
            assert!(!s.is_nan() && !s.is_infinite());
        }
    }

    #[test]
    fn test_scale_wav_speed() {
        let sample_rate = 48000;
        let num_samples = 48000; // 1秒 440Hz 正弦波
        let mut left = Vec::with_capacity(num_samples);
        let mut right = Vec::with_capacity(num_samples);
        for i in 0..num_samples {
            let t = i as f32 / sample_rate as f32;
            let sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            left.push(sample);
            right.push(sample);
        }

        let mut wav_1x = encode_stereo_f32_to_wav(&left, &right, sample_rate);
        let meta_obj = serde_json::json!({
            "completed": true,
            "total": 3,
            "done": 3,
            "speed": 1.0,
            "voiceId": "preset_azure_yunxi"
        });
        let meta_bytes = serde_json::to_vec(&meta_obj).unwrap();
        wav_1x.extend_from_slice(b"meta");
        wav_1x.extend_from_slice(&(meta_bytes.len() as u32).to_le_bytes());
        wav_1x.extend_from_slice(&meta_bytes);
        if meta_bytes.len() % 2 != 0 {
            wav_1x.push(0);
        }
        let riff_len = (wav_1x.len() - 8) as u32;
        wav_1x[4..8].copy_from_slice(&riff_len.to_le_bytes());

        // 1.5x 语速派生
        let scaled_15_bytes = scale_wav_speed(&wav_1x, 1.5).expect("1.5x scaling should succeed");
        assert!(scaled_15_bytes.len() < wav_1x.len(), "1.5x WAV 字节数应显著少于 1.0x WAV");
        assert_eq!(&scaled_15_bytes[0..4], b"RIFF");
        assert_eq!(&scaled_15_bytes[8..12], b"WAVE");

        // 验证 meta chunk 存在且 speed 已被更新为 1.5
        let meta_str = String::from_utf8_lossy(&scaled_15_bytes);
        assert!(meta_str.contains("\"speed\":1.5") || meta_str.contains("\"speed\": 1.5"), "meta chunk 应更新 speed 字段为 1.5");
        assert!(meta_str.contains("\"voiceId\":\"preset_azure_yunxi\"") || meta_str.contains("\"voiceId\": \"preset_azure_yunxi\""));

        // 0.75x 语速派生
        let scaled_075_bytes = scale_wav_speed(&wav_1x, 0.75).expect("0.75x scaling should succeed");
        assert!(scaled_075_bytes.len() > wav_1x.len(), "0.75x WAV 字节数应多于 1.0x WAV");
    }

    #[test]
    fn test_resample_16k_mono_to_48k_stereo_planar() {
        let n_16k = 16000; // 1秒 16kHz
        let mut samples_16k = Vec::with_capacity(n_16k);
        for i in 0..n_16k {
            let t = i as f32 / 16000.0;
            samples_16k.push((2.0 * std::f32::consts::PI * 440.0 * t).sin());
        }

        let planar = MossTtsEngine::resample_16k_mono_to_48k_stereo_planar(&samples_16k)
            .expect("重采样应成功");

        let shape = planar.shape();
        assert_eq!(shape, &[1, 2, 48000], "输出张量形状必须是 [1, 2, 48000]");

        // 验证 Planar 数据连续性与左右声道一致性
        let slice = planar.as_slice().unwrap();
        assert_eq!(slice.len(), 96000);
        let left = &slice[0..48000];
        let right = &slice[48000..96000];
        assert_eq!(left, right, "双声道 Planar 左右声道应完全相同");
        for &s in left {
            assert!(s >= -1.0 && s <= 1.0, "样本点幅度应被安全钳位在 [-1.0, 1.0]");
            assert!(!s.is_nan());
        }
    }
}


