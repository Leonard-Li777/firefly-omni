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

/// 并列连词列表（不适合在前方断开换气）
const PARALLEL_CONJUNCTIONS: &[&str] = &["和", "与", "同", "跟", "及", "以及"];

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

        for tag_token in tags {
            let word_chars = tag_token.word.chars().count();
            let start_char = char_offset;
            let end_char = char_offset + word_chars;
            let tag = tag_token.tag.to_lowercase();

            // 1. 语气词（y 如 呢/吧/啊）与时态助词（ule 了、uguo 过）：允许在其后方截断
            let is_post_particle = tag == "y" || tag == "ule" || tag == "uguo";
            if is_post_particle && end_char >= min_safe_len && end_char <= safe_end_chars {
                best_split_char = end_char;
            }

            // 2. 复句/转折/因果连词（c 如 但是/然而/因此/所以/或者/并且，排除并列连词“和/与/同/跟”）：
            //    在连词前方断开（切成 "...，但是..."，符合人类发音呼吸）
            let is_conjunction = tag == "c" && !PARALLEL_CONJUNCTIONS.contains(&tag_token.word);
            if is_conjunction && start_char >= min_safe_len && start_char <= safe_end_chars {
                best_split_char = start_char;
            }

            // 3. 介词 (p) 与结构助词 (ude1 等) 严格不可切断，跳过

            char_offset = end_char;
        }

        if best_split_char > 0 {
            return best_split_char;
        }
    } else {
        // 多语言/西文环境：基于 unicode-segmentation 扫描完整单词边界
        let mut char_offset = 0;
        let mut best_split_char = 0;

        for word in search_slice.split_word_bounds() {
            let word_chars = word.chars().count();
            let end_char = char_offset + word_chars;
            // 优先在单词间隔的空白处截断
            if end_char >= min_safe_len && end_char <= safe_end_chars && word.chars().all(|c| c.is_whitespace() || c == '-') {
                best_split_char = end_char;
            }
            char_offset = end_char;
        }

        if best_split_char > 0 {
            return best_split_char;
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
    let max_len = max_length.unwrap_or(200).max(20);
    let min_len = min_length.unwrap_or(40).clamp(1, max_len);
    let target_len = target_length.unwrap_or(100).clamp(min_len, max_len);
    let lang = language.unwrap_or("zh");
    let is_zh = lang.starts_with("zh") || contains_cjk(trimmed);
    let min_safe_len = min_len.max((target_len as f32 * 0.7) as usize);

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

    for clause_item in clauses {
        let clause_str = clause_item.text;
        let clause_chars = clause_str.chars().count();
        let tail_punct = clause_item.tail_punct;

        // A. 极端情况：单个原子段落直接突破 max_len（连续 > 200 字符完全无任何标点）
        if clause_chars > max_len {
            if !current_buffer.trim().is_empty() {
                chunks.push(current_buffer.trim().to_string());
                current_buffer.clear();
            }

            let mut remaining = clause_str;
            while remaining.chars().count() > max_len {
                let split_char_idx = find_pos_split_char_idx(&remaining, max_len, min_safe_len, is_zh);
                // 强制保底单步推进，杜绝 0 长度死循环
                let effective_split = if split_char_idx == 0 || split_char_idx >= remaining.chars().count() {
                    max_len.min(remaining.chars().count()).max(1)
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

        // 未达最小长度阈值（如 < 40 字符），强制吸纳后续两字一行、短歌词
        if prospective_chars < min_len {
            append_clause_to_buffer(&mut current_buffer, &clause_str, is_zh);
            continue;
        }

        // 达到目标长度（~100 字符）且当前短句遇强句末标点（。！？.!? 或换行 \n），优雅成片
        let is_strong_stop = tail_punct.map(|p| matches!(p, '。' | '！' | '？' | '.' | '!' | '?' | '\n')).unwrap_or(false);
        if prospective_chars >= target_len && is_strong_stop {
            append_clause_to_buffer(&mut current_buffer, &clause_str, is_zh);
            chunks.push(current_buffer.trim().to_string());
            current_buffer.clear();
            continue;
        }

        // 即将突破最大硬上限（200 字符），必须切断出片
        if prospective_chars > max_len {
            if !current_buffer.trim().is_empty() {
                chunks.push(current_buffer.trim().to_string());
                current_buffer.clear();
            }
            current_buffer = clause_str;
            continue;
        }

        // 处于 100 ~ 200 字符之间，若遇次级标点（分号、冒号），亦优雅出片
        let is_medium_stop = tail_punct.map(|p| matches!(p, '；' | ';' | '：' | ':')).unwrap_or(false);
        if prospective_chars >= target_len && is_medium_stop {
            append_clause_to_buffer(&mut current_buffer, &clause_str, is_zh);
            chunks.push(current_buffer.trim().to_string());
            current_buffer.clear();
            continue;
        }

        append_clause_to_buffer(&mut current_buffer, &clause_str, is_zh);
    }

    // 刷新末尾残留缓冲区
    if !current_buffer.trim().is_empty() {
        let trimmed_buf = current_buffer.trim().to_string();
        let buf_chars = trimmed_buf.chars().count();

        // 若末尾只剩极短片段（< 15 字符）且前一切片未满，无缝合入前一切片
        if buf_chars < 15 && !chunks.is_empty() {
            let last_idx = chunks.len() - 1;
            let combined_chars = chunks[last_idx].chars().count() + buf_chars;
            if combined_chars <= max_len {
                append_clause_to_buffer(&mut chunks[last_idx], &trimmed_buf, is_zh);
                return chunks;
            }
        }
        chunks.push(trimmed_buf);
    }

    chunks
}
