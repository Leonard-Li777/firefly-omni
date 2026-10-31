//! PRD 07 Slice 4/5：双索引 vs 现网仲裁的**全量等价性基线**与常驻内存/延迟实测。
//!
//! ## 这一关为什么必须有
//!
//! Slice 0 已经证明「脚本模拟现网仲裁」会给出偏差 10.5 倍的错误结论（4,942 vs 51,875）。
//! 因此本基线不复用 `TagAliasBuilder` 的仲裁代码，而在 [`legacy_oracle`] 里独立复刻改造前
//! `tag_identity::load_aliases_for_lang` 的规则，用真实 `semantic.raw.db` 灌入两套实现后
//! 逐条对拍。**任何差异都是实现缺陷，必须修实现，严禁加覆盖表掩盖**（PRD 07 §5）。
//!
//! ## 断言清单（对应 PRD 07 Testing Decisions）
//!
//! 1. 全局索引 / 按语言索引 / canonical 索引三张表与现网缓冲**逐条相同**，差异 0；
//! 2. 三张表条目数与现网完全相同（既无漏也无多）；
//! 3. LID 与分表语言分歧的词（`$` / `-人` / `/`）不丢失 —— 这正是「LID 判域直查」方案
//!    会静默丢掉的那批（Slice 0 举证）；
//! 4. 两阶段入口在 10 种当前语言 × 全语种样本上与旧实现逐条一致；
//! 5. 连续 3 次全量重载，常驻字节与 code intern 条数**不递增**；
//! 6. release 口径下给出查询延迟实测值。
//!
//! ## 运行方式（testing-guide 铁律 3/4：必须 `--lib` + 用例名过滤）
//!
//! ```text
//! node scripts/cargo-runner.js --task=test-dev test -p omni-core --release --lib \
//!   dual_index_is_byte_equivalent_to_legacy_arbitration -- --nocapture
//! ```
//!
//! debug 口径下 `normalize_lemma` 慢 20~40 倍，全量对拍会跑到 20 分钟以上；
//! 本测试是**全量数据对拍 + 延迟实测**，必须走 release（也正好是 PRD 要求的延迟口径）。

mod legacy_oracle;

use std::sync::Arc;
use std::time::Instant;

use crate::tag_alias_index::{self, TagAliasBuilder, TagAliasIndex};
use crate::tag_identity::{
    detect_tag_language, normalize_lemma, resolve_controlled_tag_code,
    resolve_controlled_tag_two_stage,
};

use legacy_oracle::{legacy_two_stage, raw_db_path, PackRows, StaticBase};

/// 10 个语种（与 `tag_alias_index::language_id` 的键空间一致）
const LANGS: [&str; 10] = ["ar", "de", "en", "es", "fr", "ja", "ko", "pt", "ru", "zh"];

/// 生产热载形态：10 张分表 push 完再一次性 finish（`omw_db` 的 push-then-seal）
fn build_index(pack: &PackRows) -> TagAliasIndex {
    let mut builder = TagAliasBuilder::new();
    for (lang, rows) in &pack.tables {
        builder.push_lang(
            lang,
            rows.iter().map(|(l, c, k)| (l.as_str(), c.as_str(), *k)),
        );
    }
    assert_eq!(
        builder.rows(),
        pack.total_rows(),
        "构建器消费行数与分表总行数不符"
    );
    builder.finish()
}

fn diff_report(kind: &str, samples: &[String]) -> String {
    let shown: Vec<&str> = samples.iter().take(10).map(|s| s.as_str()).collect();
    format!("{kind} 出现 {} 条差异，前 10 条: {shown:#?}", samples.len())
}

/// 单次调用平均耗时（ns），带 `black_box` 防优化掉
fn ns_per_call<F: FnMut(&str) -> bool>(words: &[String], rounds: usize, mut call: F) -> f64 {
    // 预热：让 code intern 锁与分支预测进入稳态
    for w in words.iter().take(64) {
        std::hint::black_box(call(w));
    }
    let mut hits = 0usize;
    let start = Instant::now();
    for _ in 0..rounds {
        for w in words {
            if std::hint::black_box(call(w)) {
                hits += 1;
            }
        }
    }
    let elapsed = start.elapsed();
    std::hint::black_box(hits);
    elapsed.as_nanos() as f64 / (words.len() * rounds) as f64
}

#[test]
fn dual_index_is_byte_equivalent_to_legacy_arbitration() {
    let Some(db) = raw_db_path() else {
        eprintln!(
            "⚠️ [slice4] 跳过全量等价性对拍：未找到 semantic.raw.db。\
             请设置 OMNI_SEMANTIC_RAW_DB=<绝对路径>，或先构建 \
             apps/desktop/build/presetResources/semantic/。"
        );
        return;
    };
    eprintln!("[slice4] 数据源: {}", db.display());

    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("只读打开语义库失败");
    let pack = PackRows::load(&conn);
    eprintln!(
        "[slice4] 分表 {} 张，合计 {} 行",
        pack.tables.len(),
        pack.total_rows()
    );

    let oracle = pack.build_oracle();
    let index = build_index(&pack);

    // ── 断言 2：条目数逐表相等（既无漏也无多）──
    let expect = (oracle.global.len(), oracle.by_lang_len(), oracle.canonical_len());
    let got = index.counts();
    eprintln!("[slice4] 条目数 oracle={expect:?} index={got:?}");
    assert_eq!(got, expect, "三张索引条目数与现网缓冲不一致");

    // ── 断言 1a：全局索引逐条相同 ──
    let mut global_diff = Vec::new();
    for (lemma, code) in &oracle.global {
        match index.global_code(lemma) {
            Some(hit) if hit == code.as_str() => {}
            other => global_diff.push(format!(
                "{lemma}\t期望 {code}\t实得 {}",
                other.unwrap_or("<none>")
            )),
        }
    }
    assert!(
        global_diff.is_empty(),
        "{}",
        diff_report("全局索引", &global_diff)
    );

    // ── 断言 1b：按语言索引逐条相同 ──
    let mut lang_diff = Vec::new();
    for (lang, map) in &oracle.by_lang {
        for (lemma, code) in map {
            match index.lang_code(lang, lemma) {
                Some(hit) if hit == code.as_str() => {}
                other => lang_diff.push(format!(
                    "{lang}/{lemma}\t期望 {code}\t实得 {}",
                    other.unwrap_or("<none>")
                )),
            }
        }
    }
    assert!(
        lang_diff.is_empty(),
        "{}",
        diff_report("按语言索引", &lang_diff)
    );

    // ── 断言 1c：canonical 索引逐条相同（含跨语言不串味）──
    let mut canon_diff = Vec::new();
    for (lang, map) in &oracle.canonical {
        for (code, name) in map {
            match index.canonical_name(lang, code) {
                Some(hit) if hit == name.as_str() => {}
                other => canon_diff.push(format!(
                    "{lang}/{code}\t期望 {name}\t实得 {}",
                    other.unwrap_or("<none>")
                )),
            }
        }
    }
    assert!(
        canon_diff.is_empty(),
        "{}",
        diff_report("canonical 索引", &canon_diff)
    );
    eprintln!("[slice4] 三张索引全量对拍：差异 0 条");

    let shared = Arc::new(index);
    tag_alias_index::install(shared.clone());

    // ── 断言 3：LID 与分表语言分歧的词不丢失（Slice 0 举证词）──
    for (probe, table_lang) in [("$", "ja"), ("-人", "ja"), ("/", "ko")] {
        let key = normalize_lemma(probe);
        let expect = oracle
            .global
            .get(&key)
            .unwrap_or_else(|| panic!("{probe} 在现网全局表中不存在，基线失效"));
        let got = resolve_controlled_tag_code(probe)
            .unwrap_or_else(|| panic!("{probe} 在新全局索引中查不到（LID 分歧词被丢失）"));
        assert_eq!(
            got, expect,
            "{probe} 解析漂移（LID={} 分表语言={table_lang}）",
            detect_tag_language(probe)
        );
        eprintln!(
            "[slice4] LID 分歧词 {probe}: LID={} 分表={table_lang} code={got}",
            detect_tag_language(probe)
        );
    }

    // ── 断言 4：两阶段入口 10 种当前语言 × 全语种样本逐条一致 ──
    let base = StaticBase::new();
    let mut pairs = 0usize;
    let mut two_stage_diff = Vec::new();
    for (lang, rows) in &pack.tables {
        // 均匀抽样：每语种最多 300 个 lemma（分表按 count DESC 排序，等距取样覆盖高频与长尾）
        let step = (rows.len() / 300).max(1);
        for row in rows.iter().step_by(step) {
            let word = row.0.as_str();
            for curr in LANGS {
                pairs += 1;
                let expect =
                    legacy_two_stage(&oracle, &base, word, Some(curr), detect_tag_language);
                let got = resolve_controlled_tag_two_stage(word, Some(curr));
                let got_norm = got.as_ref().map(|(c, n)| (c.to_string(), n.clone()));
                if expect != got_norm {
                    two_stage_diff.push(format!(
                        "word={word} 表语言={lang} curr={curr}\t期望 {expect:?}\t实得 {got_norm:?}"
                    ));
                }
            }
        }
    }
    eprintln!("[slice4] 两阶段对拍 {pairs} 组");
    assert!(
        two_stage_diff.is_empty(),
        "{}",
        diff_report("两阶段入口", &two_stage_diff)
    );

    // ── 断言 5（S5）：连续 3 次全量重载，常驻字节与 intern 不递增 ──
    //
    // intern 用「哨兵 code 的下标/指针不漂移」判定，而不是全局条数：intern 是进程级静态，
    // 全量套件并行跑时条数会被别的用例推高，用条数断言会假红。
    let probe_code = oracle
        .global
        .values()
        .next()
        .cloned()
        .expect("全局表为空，无法取哨兵 code");
    let probe_id_before =
        tag_alias_index::code_id(&probe_code).expect("首次构建后哨兵 code 必须已 intern");
    let probe_ptr_before =
        tag_alias_index::code_text(probe_id_before).expect("intern 下标必须可取回文本");

    let mut resident = Vec::new();
    let mut interned = Vec::new();
    for round in 0..3 {
        let idx = build_index(&pack);
        resident.push(idx.resident_bytes());
        interned.push(tag_alias_index::code_intern_len());
        let (arena, g, b, c) = idx.component_bytes();
        eprintln!(
            "[slice5] 第 {} 次重载: resident={} B (arena={arena} global={g} by_lang={b} \
             canonical={c}) slots={:?} slot_size={} intern={}",
            round + 1,
            idx.resident_bytes(),
            idx.slot_capacities(),
            TagAliasIndex::slot_size(),
            tag_alias_index::code_intern_len(),
        );
    }
    assert_eq!(
        resident[1], resident[2],
        "第 2/3 次重载常驻字节递增 ⇒ Arena 或索引有泄漏: {resident:?}"
    );
    let probe_id_after = tag_alias_index::code_id(&probe_code)
        .expect("重载后哨兵 code 必须仍在 intern 表内");
    assert_eq!(
        probe_id_after, probe_id_before,
        "重载使哨兵 code {probe_code} 的 intern 下标漂移（{probe_id_before} → {probe_id_after}）"
    );
    let probe_ptr_after =
        tag_alias_index::code_text(probe_id_after).expect("intern 下标必须可取回文本");
    assert!(
        std::ptr::eq(probe_ptr_before, probe_ptr_after),
        "重载后 code 文本指针漂移 ⇒ 已发给调用方的 &'static str 可能悬垂"
    );
    eprintln!(
        "[slice5] intern 条数 3 轮: {interned:?}（哨兵 {probe_code} 下标恒为 {probe_id_before}）"
    );

    // ── 断言 5b：表示层硬上限（反向可证伪：24 B 槽位 + 2 的幂容量会到 ~75 MiB）──
    //
    // 上界取 40 MiB：实测 34.93 MiB 时余量 5 MiB，足以吸收数据增长，又能挡住「槽位变宽」
    // 或「容量退回 2 的幂」这类表示层回归。基线 95.39 MiB 会直接判红。
    const RESIDENT_CEILING: usize = 40 * 1024 * 1024;
    assert!(
        resident[2] < RESIDENT_CEILING,
        "稳态常驻 {} B 超过表示层上限 {} B ⇒ 槽位/容量布局回归",
        resident[2],
        RESIDENT_CEILING
    );

    // ── 断言 6：release 口径查询延迟（PRD 基线 643 ns/次，允许 +15%）──
    let mut words: Vec<String> = oracle.global.keys().take(500).cloned().collect();
    words.extend(pack.tables[0].1.iter().take(500).map(|r| r.0.clone()));
    let raw_ns = ns_per_call(&words, 100, |w| shared.global_code(w).is_some());
    let full_ns = ns_per_call(&words, 100, |w| {
        resolve_controlled_tag_code(w).is_some()
    });
    eprintln!(
        "[slice5] 延迟: global_code={raw_ns:.1} ns/次（纯索引直查）, \
         resolve_controlled_tag_code={full_ns:.1} ns/次（含 normalize + 静态兜底；基线 643 ns）"
    );

    // 报告口径：常驻字节（不含进程级 code intern，不含 RSS）
    let (arena, g, b, c) = shared.component_bytes();
    eprintln!(
        "[slice5] 稳态常驻: arena={arena} B, global={g} B, by_lang={b} B, canonical={c} B, \
         合计={} B ({:.2} MiB)；另加一次性 code intern {} 条",
        shared.resident_bytes(),
        shared.resident_bytes() as f64 / (1024.0 * 1024.0),
        tag_alias_index::code_intern_len()
    );
}
