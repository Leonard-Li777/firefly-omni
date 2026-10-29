//! 文本清洗与自然分句切片引擎 (PRD-0058 / MOSS-TTS Integration Spec §2.5)
//!
//! 核心特性：
//! 1. 纯 Rust 原生高性能：基于 `jieba-rs` PKU/ICTCLAS 词性标注与 `unicode-segmentation`；
//! 2. 零汉字硬编码：由受控闭类词性（语气词 `y`、时态助词 `ule`/`uguo`、复句连词 `c` 前方）驱动无标点自然换气；
//! 3. 动态滑动窗口：目标切片 ~100 字符，最小聚合 40 字符（合并两字一行短诗/歌词），单句上限 200 字符；
//! 4. 严格 UTF-8 字符字节对齐：杜绝多字节截断 panic 与 Emoji 撕裂；
//! 5. 换行与西文空格防粘连保真：保留换行停顿语义，西文短句拼接自动对齐空格。

use std::sync::OnceLock;
use jieba_rs::Jieba;
use unicode_segmentation::UnicodeSegmentation;

/// 全局静态预热 Jieba 单例
static JIEBA: OnceLock<Jieba> = OnceLock::new();

/// 并列与顺承连词列表（不适合在前方断开换气，亦不适合在弱标点后立即断片将其孤立在下一句开头）
const PARALLEL_CONJUNCTIONS: &[&str] = &["和", "与", "同", "跟", "及", "以及"];
const TIGHT_CONTINUING_CONJUNCTIONS: &[&str] = &["而且", "然后", "并且", "于是", "但是", "然而", "从而", "所以"];

/// MOSS-TTS 阶梯扩容切片参数 (PRD-0058 / MOSS-TTS Low Latency Spec §3.1)
pub const HEAD_CHUNK_MIN_LEN: usize = 8;
pub const HEAD_CHUNK_TARGET_LEN: usize = 12;
pub const HEAD_CHUNK_MAX_LEN: usize = 18;

pub const SECOND_CHUNK_MIN_LEN: usize = 20;
pub const SECOND_CHUNK_TARGET_LEN: usize = 26;
pub const SECOND_CHUNK_MAX_LEN: usize = 32;

pub const BODY_CHUNK_MIN_LEN: usize = 35;
pub const BODY_CHUNK_TARGET_LEN: usize = 50;
pub const BODY_CHUNK_MAX_LEN: usize = 65; // 物理绝对红线，严防 375 帧截断

/// 获取或惰性初始化全局 Jieba 实例（多线程零锁并发借用）
pub fn get_jieba() -> &'static Jieba {
    JIEBA.get_or_init(Jieba::new)
}

/// 分句切片请求参数
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SplitChunksRequest {
    pub text: String,
    pub target_length: Option<usize>,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub language: Option<String>,
}

/// 分句切片响应体
#[derive(Debug, Clone, serde::Serialize)]
pub struct SplitChunksResponse {
    pub chunks: Vec<String>,
}

/// 原子断句单元
#[derive(Debug, Clone)]
struct AtomicClause {
    text: String,
    tail_punct: Option<char>,
}

/// 判断是否为包含中文 CJK 统一表意文字的字符串
pub fn contains_cjk(text: &str) -> bool {
    text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3400}'..='\u{4dbf}').contains(&c))
}

/// 将字符数索引转换为合法的 UTF-8 字节索引偏移量
pub fn char_to_byte_index(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(text.len())
}

/// 在超长无标点（> max_len）片段中，由语法词性或单词边界寻找最佳换气切断点
/// 返回值为截断点对应的字符数偏移（Unicode scalar count）
fn find_pos_split_char_idx(text: &str, max_len: usize, min_safe_len: usize, is_zh: bool) -> usize {
    let total_chars = text.chars().count();
    let safe_end_chars = max_len.min(total_chars);
    let safe_end_byte = char_to_byte_index(text, safe_end_chars);
    let search_slice = &text[..safe_end_byte];

    if is_zh {
        let jieba = get_jieba();
        let tags = jieba.tag(search_slice, true);

        let mut char_offset = 0;
        let mut best_split_char = 0;
        let mut last_safe_word_boundary = 0;

        for tag_token in tags {
            let word_chars = tag_token.word.chars().count();
            let start_char = char_offset;
            let end_char = char_offset + word_chars;
            let tag = tag_token.tag.to_lowercase();

            let next_byte = char_to_byte_index(text, end_char);
            let next_slice = &text[next_byte..];
            let next_is_tight_conj = TIGHT_CONTINUING_CONJUNCTIONS
                .iter()
                .any(|&conj| next_slice.trim_start().starts_with(conj));

            // 记录安全区间内的合法词边界（避免截断在专有名词或汉字词素中间）
            // 护栏：若该边界紧邻后方的词是顺承连词（如“而且/然后/并且/于是/但是/然而/从而/所以”），
            // 禁止在此断开，否则会将连词孤立在下一句句首
            if end_char >= min_safe_len && end_char <= safe_end_chars && !next_is_tight_conj {
                last_safe_word_boundary = end_char;
            }

            // 1. 语气词（y 如 呢/吧/啊）与时态助词（ule 了、uguo 过）：允许在其后方截断（同样受连词护栏约束）
            let is_post_particle = tag == "y" || tag == "ule" || tag == "uguo";
            if is_post_particle && end_char >= min_safe_len && end_char <= safe_end_chars && !next_is_tight_conj {
                best_split_char = end_char;
            }

            // 2. 复句/转折/因果连词（排除并列连词“和/与/同/跟”及紧密顺承连词“而且/然后/并且/于是/但是/然而/从而/所以”）：
            //    在非紧密连词前方断开
            let is_conjunction = tag == "c"
                && !PARALLEL_CONJUNCTIONS.contains(&tag_token.word)
                && !TIGHT_CONTINUING_CONJUNCTIONS.contains(&tag_token.word);
            if is_conjunction && start_char >= min_safe_len && start_char <= safe_end_chars {
                best_split_char = start_char;
            }

            // 3. 介词 (p) 与结构助词 (ude1 等) 严格不可切断，跳过

            char_offset = end_char;
        }

        if best_split_char > 0 {
            return best_split_char;
        }
        if last_safe_word_boundary > 0 {
            return last_safe_word_boundary;
        }
    } else {
        // 多语言/西文环境：基于 unicode-segmentation 扫描完整单词边界
        let mut char_offset = 0;
        let mut best_split_char = 0;
        let mut last_word_boundary = 0;

        for word in search_slice.split_word_bounds() {
            let word_chars = word.chars().count();
            let end_char = char_offset + word_chars;
            if end_char >= min_safe_len && end_char <= safe_end_chars {
                last_word_boundary = end_char;
            }
            // 优先在单词间隔的空白处截断
            if end_char >= min_safe_len && end_char <= safe_end_chars && word.chars().all(|c| c.is_whitespace() || c == '-') {
                best_split_char = end_char;
            }
            char_offset = end_char;
        }

        if best_split_char > 0 {
            return best_split_char;
        }
        if last_word_boundary > 0 {
            return last_word_boundary;
        }
    }

    // 终极回退：向左寻找任意空白
    let mut fallback_split = 0;
    for (c_idx, c) in search_slice.chars().enumerate() {
        if c_idx >= min_safe_len && c_idx <= safe_end_chars && c.is_whitespace() {
            fallback_split = c_idx + 1;
        }
    }
    if fallback_split > 0 {
        return fallback_split;
    }

    // 无法通过词性或空白切分时的安全字符截断
    safe_end_chars
}

/// 辅助：向缓冲区追加子句并保持西文空格保真
fn append_clause_to_buffer(buffer: &mut String, clause: &str, is_zh: bool) {
    if buffer.is_empty() {
        buffer.push_str(clause.trim_start());
        return;
    }
    // 西文环境下若前缓冲区不以空白或标点结尾，且后文不以空白开头，补入空格
    if !is_zh {
        let last_c = buffer.chars().last();
        let need_space = last_c.map(|c| !c.is_whitespace() && c != '\n').unwrap_or(false)
            && !clause.starts_with(' ');
        if need_space {
            buffer.push(' ');
        }
    }
    buffer.push_str(clause);
}

/// 核心分句切片算法
pub fn split_text_chunks(
    text: &str,
    target_length: Option<usize>,
    min_length: Option<usize>,
    max_length: Option<usize>,
    language: Option<&str>,
) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    // 健壮参数范围保护：防 max_length 为 0 或过小引发死循环
    let is_ladder_mode = target_length.is_none() && min_length.is_none() && max_length.is_none();
    let static_max_len = max_length.unwrap_or(200).max(20);
    let static_min_len = min_length.unwrap_or(40).clamp(1, static_max_len);
    let static_target_len = target_length.unwrap_or(100).clamp(static_min_len, static_max_len);
    let lang = language.unwrap_or("zh");
    let is_zh = lang.starts_with("zh") || contains_cjk(trimmed);

    // 动态阶梯状态机阈值求解闭包 (PRD-0058 / MOSS-TTS Low Latency Spec §3.1)
    let get_thresholds = |chunk_count: usize| -> (usize, usize, usize) {
        if is_ladder_mode {
            match chunk_count {
                0 => (HEAD_CHUNK_MIN_LEN, HEAD_CHUNK_TARGET_LEN, HEAD_CHUNK_MAX_LEN),
                1 => (SECOND_CHUNK_MIN_LEN, SECOND_CHUNK_TARGET_LEN, SECOND_CHUNK_MAX_LEN),
                _ => (BODY_CHUNK_MIN_LEN, BODY_CHUNK_TARGET_LEN, BODY_CHUNK_MAX_LEN),
            }
        } else {
            (static_min_len, static_target_len, static_max_len)
        }
    };

    // 1. 初次原子划分：按标点与换行提取 AtomicClause，保留原始标点与换行信息
    let mut clauses: Vec<AtomicClause> = Vec::new();
    let mut current_buf = String::new();

    let split_punct = ['。', '！', '？', '；', '：', '，', '、', '\n', '\r', '.', '!', '?', ';', ':', ','];

    for c in text.chars() {
        if split_punct.contains(&c) {
            if c == '\n' || c == '\r' {
                let piece = current_buf.trim().to_string();
                if !piece.is_empty() {
                    clauses.push(AtomicClause {
                        text: piece,
                        tail_punct: Some('\n'),
                    });
                }
                current_buf.clear();
            } else {
                current_buf.push(c);
                let piece = current_buf.trim().to_string();
                if !piece.is_empty() {
                    clauses.push(AtomicClause {
                        text: piece,
                        tail_punct: Some(c),
                    });
                }
                current_buf.clear();
            }
        } else {
            current_buf.push(c);
        }
    }

    let last_piece = current_buf.trim().to_string();
    if !last_piece.is_empty() {
        clauses.push(AtomicClause {
            text: last_piece,
            tail_punct: None,
        });
    }

    if clauses.is_empty() {
        return Vec::new();
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current_buffer = String::new();

    for clause_idx in 0..clauses.len() {
        let (min_len, target_len, max_len) = get_thresholds(chunks.len());
        let _min_safe_len = min_len.max((target_len as f32 * 0.7) as usize);

        let clause_item = &clauses[clause_idx];
        let clause_str = clause_item.text.as_str();
        let clause_chars = clause_str.chars().count();
        let tail_punct = clause_item.tail_punct;

        // 后视紧密连词探测（避免在弱标点后截断并将“而且/然后/并且/于是/但是/然而/从而/所以”孤立在下一句句首）
        let next_starts_with_tight_conjunction = if let Some(next_item) = clauses.get(clause_idx + 1) {
            let next_trimmed = next_item.text.trim_start();
            TIGHT_CONTINUING_CONJUNCTIONS.iter().any(|&conj| next_trimmed.starts_with(conj))
        } else {
            false
        };

        // A. 极端情况：单个原子段落直接突破 max_len（连续无标点长串）
        // 首句弹性延展保护：若前 18 字无自然停顿，通过词法边界弹性延展至 22~25 字，严禁在专有名词或英文单词内部硬切补逗号
        let elastic_clause_max = if is_ladder_mode && chunks.is_empty() { 25 } else { max_len };
        if clause_chars > elastic_clause_max {
            let current_starts_with_tight_conj = TIGHT_CONTINUING_CONJUNCTIONS
                .iter()
                .any(|&conj| clause_str.starts_with(conj));

            let mut remaining = if !current_buffer.trim().is_empty() && current_starts_with_tight_conj {
                let combined = format!("{}{}", current_buffer, clause_str);
                current_buffer.clear();
                combined
            } else {
                if !current_buffer.trim().is_empty() {
                    chunks.push(current_buffer.trim().to_string());
                    current_buffer.clear();
                }
                clause_str.to_string()
            };

            while remaining.chars().count() > (if is_ladder_mode && chunks.is_empty() { 25 } else { get_thresholds(chunks.len()).2 }) {
                let (cur_min, cur_target, cur_max) = get_thresholds(chunks.len());
                let cur_elastic_max = if is_ladder_mode && chunks.is_empty() { 25 } else { cur_max };
                let cur_min_safe = cur_min.max((cur_target as f32 * 0.7) as usize);
                let split_char_idx = find_pos_split_char_idx(&remaining, cur_elastic_max, cur_min_safe, is_zh);
                // 强制保底单步推进，杜绝 0 长度死循环
                let effective_split = if split_char_idx == 0 || split_char_idx >= remaining.chars().count() {
                    cur_elastic_max.min(remaining.chars().count()).max(1)
                } else {
                    split_char_idx
                };

                let split_byte = char_to_byte_index(&remaining, effective_split);
                let mut piece = remaining[..split_byte].trim().to_string();

                // 若截断处无标点，补入全角逗号或英文逗号释放换气停顿
                let last_c = piece.chars().last();
                let has_tail_punct = last_c.map(|c| split_punct.contains(&c)).unwrap_or(false);
                if !has_tail_punct {
                    if is_zh {
                        piece.push('，');
                    } else {
                        piece.push_str(", ");
                    }
                }

                chunks.push(piece);
                remaining = remaining[split_byte..].trim().to_string();
            }

            if !remaining.is_empty() {
                current_buffer = remaining;
            }
            continue;
        }

        // B. 常规智能聚合
        let prospective_chars = current_buffer.chars().count() + clause_chars;

        // 1. 未达最小长度阈值且无标点截断诉求，强制吸纳后续两字一行、短歌词
        if prospective_chars < min_len {
            append_clause_to_buffer(&mut current_buffer, clause_str, is_zh);
            continue;
        }

        // 2. 标点特征提取与连词护栏
        let is_strong_stop = tail_punct.map(|p| matches!(p, '。' | '！' | '？' | '.' | '!' | '?' | '\n')).unwrap_or(false);
        let is_medium_stop = tail_punct.map(|p| matches!(p, '；' | ';' | '：' | ':')).unwrap_or(false);
        let is_weak_stop = tail_punct.map(|p| matches!(p, '，' | ',' | '、')).unwrap_or(false);
        let can_split_at_weak_punct = is_weak_stop && !next_starts_with_tight_conjunction;

        // 3. 首句极速起播结算：在首句模式下，若遇到标点且无连词粘连，满足 min_len (>=8) 即可结算出片
        if is_ladder_mode && chunks.is_empty() {
            let can_head_split = is_strong_stop || is_medium_stop || can_split_at_weak_punct;
            if can_head_split && prospective_chars >= min_len {
                append_clause_to_buffer(&mut current_buffer, clause_str, is_zh);
                chunks.push(current_buffer.trim().to_string());
                current_buffer.clear();
                continue;
            }
        }

        // 4. 强句末标点（。！？.!? 或换行 \n）达到目标长度，优雅出片
        if prospective_chars >= target_len && is_strong_stop {
            append_clause_to_buffer(&mut current_buffer, clause_str, is_zh);
            chunks.push(current_buffer.trim().to_string());
            current_buffer.clear();
            continue;
        }

        // 5. 即将突破最大硬上限，必须切断出片
        if prospective_chars > max_len {
            // 若当前缓冲区非空，且当前子句以紧密连词开头（如“而且/然后/并且”），
            // 坚决不可将前缓冲区直接独立出片而将连词孤立在下一句句首；应合并后在安全词法边界断开
            let current_starts_with_tight_conj = TIGHT_CONTINUING_CONJUNCTIONS.iter().any(|&conj| clause_str.starts_with(conj));
            let mut remaining = if !current_buffer.trim().is_empty() && current_starts_with_tight_conj {
                let combined = format!("{}{}", current_buffer, clause_str);
                current_buffer.clear();
                combined
            } else {
                if !current_buffer.trim().is_empty() {
                    chunks.push(current_buffer.trim().to_string());
                    current_buffer.clear();
                }
                clause_str.to_string()
            };

            let (_, _, new_max) = get_thresholds(chunks.len());
            let elastic_limit = if is_ladder_mode && chunks.is_empty() { 25 } else { new_max };
            if remaining.chars().count() > elastic_limit {
                while remaining.chars().count() > (if is_ladder_mode && chunks.is_empty() { 25 } else { get_thresholds(chunks.len()).2 }) {
                    let (cur_min, cur_target, cur_max) = get_thresholds(chunks.len());
                    let cur_elastic_max = if is_ladder_mode && chunks.is_empty() { 25 } else { cur_max };
                    let cur_min_safe = cur_min.max((cur_target as f32 * 0.7) as usize);
                    let split_char_idx = find_pos_split_char_idx(&remaining, cur_elastic_max, cur_min_safe, is_zh);
                    let effective_split = if split_char_idx == 0 || split_char_idx >= remaining.chars().count() {
                        cur_elastic_max.min(remaining.chars().count()).max(1)
                    } else {
                        split_char_idx
                    };
                    let split_byte = char_to_byte_index(&remaining, effective_split);
                    let mut piece = remaining[..split_byte].trim().to_string();
                    let last_c = piece.chars().last();
                    let has_tail_punct = last_c.map(|c| split_punct.contains(&c)).unwrap_or(false);
                    if !has_tail_punct {
                        if is_zh {
                            piece.push('，');
                        } else {
                            piece.push_str(", ");
                        }
                    }
                    chunks.push(piece);
                    remaining = remaining[split_byte..].trim().to_string();
                }
                if !remaining.is_empty() {
                    current_buffer = remaining;
                }
            } else {
                current_buffer = remaining;
            }
            continue;
        }

        // 6. 次级标点（分号、冒号）达到目标长度，优雅出片
        if prospective_chars >= target_len && is_medium_stop {
            append_clause_to_buffer(&mut current_buffer, clause_str, is_zh);
            chunks.push(current_buffer.trim().to_string());
            current_buffer.clear();
            continue;
        }

        // 7. 阶梯模式下次句与常规句在达到目标长度遇逗号切分（且无连词粘连）
        if is_ladder_mode && prospective_chars >= target_len && can_split_at_weak_punct {
            append_clause_to_buffer(&mut current_buffer, clause_str, is_zh);
            chunks.push(current_buffer.trim().to_string());
            current_buffer.clear();
            continue;
        }

        append_clause_to_buffer(&mut current_buffer, clause_str, is_zh);
    }

    // 刷新末尾残留缓冲区
    if !current_buffer.trim().is_empty() {
        let trimmed_buf = current_buffer.trim().to_string();
        let buf_chars = trimmed_buf.chars().count();

        // 若末尾只剩极短片段（< 15 字符）且前一切片未满，无缝合入前一切片
        if buf_chars < 15 && !chunks.is_empty() {
            let last_idx = chunks.len() - 1;
            let (_, _, prev_max) = get_thresholds(last_idx);
            let combined_chars = chunks[last_idx].chars().count() + buf_chars;
            if combined_chars <= prev_max {
                append_clause_to_buffer(&mut chunks[last_idx], &trimmed_buf, is_zh);
                return chunks;
            }
        }
        chunks.push(trimmed_buf);
    }

    chunks
}
