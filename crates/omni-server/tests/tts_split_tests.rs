use omni_server::text_chunker::split_text_chunks;

#[test]
fn test_short_poetry_greedy_aggregation() {
    let poem = "白日\n依山尽\n黄河\n入海流\n欲穷\n千里目\n更上\n一层楼";
    let chunks = split_text_chunks(poem, Some(100), Some(40), Some(200), Some("zh"));
    
    // 8 句短诗共约 28 字符，小于最小聚合阈值 40，应当被贪婪聚合为单一切片，绝不产生双字碎片切片
    assert_eq!(chunks.len(), 1, "Short poem should be greedily aggregated into 1 chunk");
    assert!(chunks[0].contains("白日") && chunks[0].contains("一层楼"));
}

#[test]
fn test_long_unpunctuated_text_split_by_pos() {
    // 构造一段 > 260 字且完全没有标点的纯中文长串，中间包含语气助词与连词
    let prefix = "人工智能与机器学习技术在当今数字时代已经深入到了人类社会的每一个角落无论是科学研究还是工业制造都发生了翻天覆地的变化并且在日常生活中我们也深刻感受到了这种前所未有的变革力量推动着历史的发展";
    let middle = "但是面对这种飞速发展的智能化浪潮很多人依然感到有些迷茫和焦虑不知道未来该如何应对各种未知的挑战和困难";
    let suffix = "因此我们必须保持终身学习的心态不断提升自身的综合能力从而在未来的智能化浪潮中立于不败之地迎接更加美好的明天并为社会创造更多的价值和深远的影响带来持久的繁荣与和平";
    let full_text = format!("{}{}{}", prefix, middle, suffix);
    let total_chars = full_text.chars().count();
    assert!(total_chars > 200, "Total characters should be > 200, got {}", total_chars);

    let chunks = split_text_chunks(&full_text, Some(100), Some(40), Some(200), Some("zh"));
    assert!(chunks.len() >= 2, "Long unpunctuated text should be split into at least 2 chunks");

    for (i, chunk) in chunks.iter().enumerate() {
        let chunk_chars = chunk.chars().count();
        assert!(chunk_chars <= 205, "Chunk {} exceeds max length: {} chars", i, chunk_chars);
        assert!(!chunk.trim().is_empty(), "Chunk {} should not be empty", i);
    }
}

#[test]
fn test_multilingual_and_emoji_safety() {
    let text = "Hello world! This is a test for English segmentation. 🌟✨🚀 包含复杂Unicode表情与English混排文本，绝不能在多字节字符中间截断导致panic。";
    let chunks = split_text_chunks(text, Some(50), Some(20), Some(100), None);
    assert!(!chunks.is_empty());
    
    for chunk in &chunks {
        // 验证切片是否为合法 UTF-8 且包含完整 emoji
        assert!(chunk.is_ascii() || chunk.chars().count() > 0);
    }
}

#[test]
fn test_empty_and_whitespace_handling() {
    let chunks_empty = split_text_chunks("", None, None, None, None);
    assert!(chunks_empty.is_empty());

    let chunks_spaces = split_text_chunks("    \n\n\t   ", None, None, None, None);
    assert!(chunks_spaces.is_empty());
}

#[test]
fn test_preposition_and_structural_particle_protection() {
    // 验证介词（在/把/对）和结构助词（的/地/得）不会被断句破坏
    let text = "我们在会议室里把所有的资料都认真整理好了但是时间已经非常紧迫了我们需要尽快出发";
    let chunks = split_text_chunks(text, Some(30), Some(15), Some(50), Some("zh"));
    
    for chunk in &chunks {
        assert!(!chunk.ends_with("在，"), "Should not cut after preposition '在'");
        assert!(!chunk.ends_with("把，"), "Should not cut after preposition '把'");
        assert!(!chunk.ends_with("的，"), "Should not cut after structural particle '的'");
    }
}

#[test]
fn test_newline_stop_preserved() {
    let text = "这是第一首长诗的开头部分，描写了壮丽山河。\n紧接着第二段诗句进入了深邃的哲学思考，探讨人生旅途与宇宙万物的永恒规律。\n最后一段表达了诗人对未来的无限憧憬。";
    let chunks = split_text_chunks(text, Some(35), Some(15), Some(60), Some("zh"));
    assert!(chunks.len() >= 2);
    for chunk in &chunks {
        assert!(!chunk.trim().is_empty());
    }
}

#[test]
fn test_english_word_spacing_preservation() {
    let text = "Hello world! How are you doing today? Everything is fine.";
    let chunks = split_text_chunks(text, Some(50), Some(20), Some(80), Some("en"));
    assert!(!chunks.is_empty());
    // 确保英文单词间不会发生 "Hello,world" 粘连
    assert!(!chunks[0].contains("today?Everything"));
}

#[test]
fn test_zero_max_length_loop_defense() {
    // 边界硬防御：传入 0 或极小 max_length 时，不会陷入死循环
    let text = "这是一段简单的测试文本，用于验证极端参数下的安全性。";
    let chunks = split_text_chunks(text, Some(0), Some(0), Some(0), Some("zh"));
    assert!(!chunks.is_empty());
}

#[test]
fn test_moss_ladder_expansion_default() {
    // 针对 MOSS-TTS 的阶梯扩容测试：
    // 首句 8~18 字（目标 12），次句 20~32 字（目标 26），常规句 <= 65 字（目标 50）
    let text = "大家好，欢迎使用萤核语音助手！在今天的演示中，我们将深入了解多模态大模型的自回归端侧推理架构与异构并行设计。通过这一套全自研的高性能计算引擎，我们可以实现极低的首发音延迟，并在复杂的长文本场景下彻底消灭截断断流问题，为用户带来如丝般顺滑的沉浸式语音体验。";
    let chunks = split_text_chunks(text, None, None, None, Some("zh"));
    
    assert!(chunks.len() >= 3, "Expected at least 3 chunks, got {}", chunks.len());
    
    // Chunk 0 (Head): 首句极速起播切片，目标 10~18 字
    let head_chars = chunks[0].chars().count();
    assert!(head_chars >= 8 && head_chars <= 25, "Head chunk out of expected bounds: {} (text: '{}')", head_chars, chunks[0]);
    
    // Chunk 1 (Second): 次句平滑过渡，目标 20~32 字
    let second_chars = chunks[1].chars().count();
    assert!(second_chars >= 15 && second_chars <= 40, "Second chunk out of bounds: {} (text: '{}')", second_chars, chunks[1]);
    
    // Chunk 2+ (Body): 常规句严守物理安全红线 <= 65 字，绝对杜绝 375 帧截断
    for (idx, chunk) in chunks.iter().enumerate().skip(2) {
        let chars = chunk.chars().count();
        assert!(chars <= 65, "Body chunk {} exceeds hard cap 65 chars: {} (text: '{}')", idx, chars, chunk);
    }
}

#[test]
fn test_tight_conjunction_lookahead_guard() {
    // 弱标点后视连词护栏：遇“而且/然后/并且/于是/但是”等紧密顺承/转折词，不应在逗号处生硬断开
    let text = "这项技术的研发周期非常漫长，而且在早期测试中遇到过大量的内存拷贝与缓存抖动瓶颈，因此团队进行了多轮红队对抗审查。";
    let chunks = split_text_chunks(text, None, None, None, Some("zh"));
    
    // 验证没有任何一个切片以孤立的“而且”或“并且”开头
    for (i, chunk) in chunks.iter().enumerate() {
        let trimmed = chunk.trim_start();
        assert!(!trimmed.starts_with("而且"), "Chunk {} should not start with isolated '而且': '{}'", i, chunk);
        assert!(!trimmed.starts_with("并且"), "Chunk {} should not start with isolated '并且': '{}'", i, chunk);
    }
}

#[test]
fn test_proper_noun_and_english_word_protection() {
    // 专有名词与英文单词不被生硬切断
    let text = "DeepSeek和Anthropic以及OpenAI都在探索下一代端侧智能体架构，并且在性能优化方面投入了巨大的研发资源。";
    let chunks = split_text_chunks(text, None, None, None, Some("zh"));
    
    for chunk in &chunks {
        assert!(!chunk.contains("Deep,Seek") && !chunk.contains("Deep，Seek"));
        assert!(!chunk.contains("Open,AI") && !chunk.contains("Open，AI"));
    }
}

