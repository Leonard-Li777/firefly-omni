//! GH #724：动态别名字典（`tag_identity` 的进程级 `static RwLock`）用例的**进程隔离**。
//!
//! 背景：`tag_identity::dynamic_aliases*` 是进程级全局可变状态。下列用例会 `clear` + `load`
//! 该字典；若与 lib 单测（同进程、多线程并行）同时运行，会与任何**读取**该字典的用例互踩。
//! 实测症状：
//! - `dynamic_aliases_lifecycle_all_in_one` 自身断言取到被清空/改写的值；
//! - `builtin_aliases_agree_with_tag_identity_json` 出现「伪漂移」——动态别名 omw 仲裁
//!   遮蔽静态 `builtin.*`（`表格` 被包内 `omw.08266235.n` 覆盖 `builtin.sheet`）。
//!
//! 方案：把这些**变更者**用例移入本集成测试（独立二进制 = 独立进程）。lib 单测进程内从此
//! 不再有任何全局字典写入 → 所有读取方（含 `tag_admissibility` 等跨模块用例）天然确定。
//! 无需引入 `serial_test` 等新依赖，也无需逐个标注读取用例。
//!
//! 本文件仅含下列两个用例，二者都会 `clear_dynamic_aliases()`，故在文件内串行即可。
//! 详见 GH #724。

use omni_core::tag_identity::{
    builtin_tag_code, clear_dynamic_aliases, detect_tag_language, load_aliases_for_lang,
    load_aliases_from_entries, normalize_tag_to_code, resolve_controlled_tag_code,
    resolve_controlled_tag_two_stage, tag_display, tag_matches_concept,
};

/// 文件内串行门：两个用例都会 `clear_dynamic_aliases()`，必须互斥（否则互踩）。
/// 容忍中毒——任一用例 panic 后另一用例仍可运行。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn dynamic_aliases_lifecycle_all_in_one() {
    let _guard = serial();
    // ─── 场景 1：注入与反查 ───
    clear_dynamic_aliases();
    load_aliases_from_entries(vec![
        ("特种发票", "builtin.invoice"),
        ("Special Tax Invoice", "builtin.invoice"),
    ]);
    assert_eq!(builtin_tag_code("特种发票"), Some("builtin.invoice"));
    assert_eq!(builtin_tag_code("Special Tax Invoice"), Some("builtin.invoice"));
    assert_eq!(normalize_tag_to_code("特种发票"), "builtin.invoice");

    // ─── 场景 2：同 lemma omw.* 优先于 builtin.*（双向写入顺序）───
    clear_dynamic_aliases();
    // builtin 先写入，omw 后写入 → 反查必须得到 omw.*
    load_aliases_from_entries(vec![
        ("科幻", "builtin.science_fiction"),
        ("科幻", "omw.00012345.n"),
    ]);
    assert_eq!(resolve_controlled_tag_code("科幻"), Some("omw.00012345.n"));
    // 反向写入顺序：omw 先、builtin 后，仍保持 omw 优先
    clear_dynamic_aliases();
    load_aliases_from_entries(vec![
        ("科幻", "omw.00012345.n"),
        ("科幻", "builtin.science_fiction"),
    ]);
    assert_eq!(resolve_controlled_tag_code("科幻"), Some("omw.00012345.n"));
    // 静态 builtin 词仍可解析
    assert!(resolve_controlled_tag_code("截图").unwrap().starts_with("builtin."));
    // 未知词返回 None，交由调用方派生 _ext
    assert_eq!(resolve_controlled_tag_code("完全未知的新标签XYZQ"), None);

    // ─── 场景 3：两阶段跨语言反查与就地本地化 ───
    clear_dynamic_aliases();

    // 3.1 测试 LID 判定
    assert_eq!(detect_tag_language("Connect"), "en");
    assert_eq!(detect_tag_language("Art"), "en");
    assert_eq!(detect_tag_language("艺术"), "zh");
    assert_eq!(detect_tag_language("アニメ"), "ja");
    assert_eq!(detect_tag_language("그림"), "ko");
    assert_eq!(detect_tag_language("рисунок"), "ru");

    // 3.2 模拟从多语言分表热载入别名
    load_aliases_for_lang("zh", vec![
        ("艺术", "omw.01700688-n", true),
        ("连接", "builtin.connect", true),
        ("发票", "builtin.invoice", true),
    ]);
    load_aliases_for_lang("en", vec![
        ("art", "omw.01700688-n", true),
        ("connect", "builtin.connect", true),
        ("invoice", "builtin.invoice", true),
    ]);

    // 3. 在中文环境下识别英文 "Art" -> 跨语言命中受控 code，并就地本地化为 "艺术"
    let outcome_art = resolve_controlled_tag_two_stage("Art", Some("zh-CN"));
    assert!(outcome_art.is_some());
    let (code, canon_name) = outcome_art.unwrap();
    assert_eq!(code, "omw.01700688-n");
    assert_eq!(canon_name, Some("艺术".to_string()));

    // 4. 在中文环境下识别英文 "Connect" -> 就地本地化为 "连接"
    let outcome_conn = resolve_controlled_tag_two_stage("Connect", Some("zh"));
    assert!(outcome_conn.is_some());
    let (code_conn, canon_conn) = outcome_conn.unwrap();
    assert_eq!(code_conn, "builtin.connect");
    assert_eq!(canon_conn, Some("连接".to_string()));

    // 5. 在中文环境下输入未登录的中文生词 -> 判定为 zh，与当前语言相同，不重复查找，返回 None
    let outcome_zh_unknown = resolve_controlled_tag_two_stage("未知冷门新造词汇", Some("zh"));
    assert_eq!(outcome_zh_unknown, None);

    // 6. 在中文环境下输入未登录的英文生词 -> 两阶段均未查得，返回 None
    let outcome_en_unknown = resolve_controlled_tag_two_stage("NonExistentBrandNameX", Some("zh"));
    assert_eq!(outcome_en_unknown, None);

    // ─── 场景 4：核心视觉画质与尺度受控标签 100% 确定性解析与 _ext 防污染 ───
    assert_eq!(resolve_controlled_tag_code("高质量"), Some("builtin.high_quality"));
    assert_eq!(resolve_controlled_tag_code("曝光正常"), Some("builtin.exposure_is_normal"));
    assert_eq!(resolve_controlled_tag_code("全年龄"), Some("builtin.all_ages"));
    assert_eq!(resolve_controlled_tag_code("有字图"), Some("builtin.text_in_image"));
    assert_eq!(resolve_controlled_tag_code("无字图"), Some("builtin.image_with_no_text"));
    assert_eq!(normalize_tag_to_code("高质量"), "builtin.high_quality");
    assert_eq!(normalize_tag_to_code("曝光正常"), "builtin.exposure_is_normal");
    assert_eq!(normalize_tag_to_code("全年龄"), "builtin.all_ages");
    assert_eq!(normalize_tag_to_code("有字图"), "builtin.text_in_image");

    // 验证非法/污染的 _ext.* 绝对无法覆盖已有的受控代码
    load_aliases_from_entries(vec![
        ("高质量", "_ext.gaozhiliang.4973d142"),
        ("曝光正常", "_ext.guang_zhen_chang.ee8a87d9"),
    ]);
    assert_eq!(resolve_controlled_tag_code("高质量"), Some("builtin.high_quality"));
    assert_eq!(resolve_controlled_tag_code("曝光正常"), Some("builtin.exposure_is_normal"));

    // 验证维度 123 内容尺度全量标签解析
    assert_eq!(resolve_controlled_tag_code("PG-13"), Some("builtin.pg_13"));
    assert_eq!(resolve_controlled_tag_code("pg13"), Some("builtin.pg_13"));
    assert_eq!(resolve_controlled_tag_code("R-15"), Some("builtin.r_15"));
    assert_eq!(resolve_controlled_tag_code("R-18"), Some("builtin.r_18"));
    assert_eq!(resolve_controlled_tag_code("R-18G"), Some("builtin.r_18g"));
    assert_eq!(resolve_controlled_tag_code("软色情"), Some("builtin.soft_porn"));
    assert_eq!(resolve_controlled_tag_code("半肉"), Some("builtin.half_meat"));
    assert_eq!(resolve_controlled_tag_code("纯肉"), Some("builtin.pure_meat"));
    assert_eq!(resolve_controlled_tag_code("特殊XP"), Some("builtin.special_xp"));
    assert_eq!(resolve_controlled_tag_code("重度猎奇"), Some("builtin.extreme_grotesque"));
    assert_eq!(resolve_controlled_tag_code("车震"), Some("builtin.car_sex"));
    assert_eq!(resolve_controlled_tag_code("强暴强奸"), Some("builtin.rape_and_sexual_assault"));
    assert_eq!(resolve_controlled_tag_code("重口变态"), Some("builtin.hardcore_perversion"));
    assert_eq!(resolve_controlled_tag_code("女同百合"), Some("builtin.lesbian_yuri"));
    assert_eq!(resolve_controlled_tag_code("男同耽美"), Some("builtin.gay_yaoi"));
    assert_eq!(resolve_controlled_tag_code("隐私盗摄"), Some("builtin.spy_camera_peeping"));

    // ─── 场景 5：高频英文视觉/文档候选词在中文环境下的受控两阶段反查与就地本地化 ───
    // 裸词归属裁决（见 docs/issues/issue-alias-map-joint-keyspace-conflicts.md）：
    // 裸词 `portrait` / `landscape` 判给**画幅**，与桌面端 controlled-tag-resolver.ts:142-145 一致。
    let outcome_portrait = resolve_controlled_tag_two_stage("portrait", Some("zh"));
    assert_eq!(outcome_portrait, Some(("builtin.vertical_screen", Some("竖屏".to_string()))));

    let outcome_landscape = resolve_controlled_tag_two_stage("landscape", Some("zh"));
    assert_eq!(outcome_landscape, Some(("builtin.horizontal_screen", Some("横屏".to_string()))));

    // 题材概念 `人像写真` 改由专属 en 名 `Portrait Photography` 承载（顺带对齐 AOT 表）。
    let outcome_portrait_photo = resolve_controlled_tag_two_stage("Portrait Photography", Some("zh"));
    assert_eq!(
        outcome_portrait_photo,
        Some(("builtin.portrait_photography", Some("人像写真".to_string())))
    );

    let outcome_draft = resolve_controlled_tag_two_stage("draft", Some("zh"));
    assert_eq!(outcome_draft, Some(("builtin.draft", Some("草稿".to_string()))));

    // 方向 B（见 docs/issues/issue-builtin-aliases-duplicate-keys.md）：裸词 `design` 归**裸概念**
    // `Design`（`builtin.design` / 设计）；复合概念 `Design Draft` 由专属别名 `设计稿` / `design draft` 承载。
    let outcome_design = resolve_controlled_tag_two_stage("design", Some("zh"));
    assert_eq!(outcome_design, Some(("builtin.design", Some("设计".to_string()))));

    let outcome_cartoon = resolve_controlled_tag_two_stage("cartoon", Some("zh"));
    assert_eq!(outcome_cartoon, Some(("builtin.comics", Some("漫画".to_string()))));

    // 同上：裸词 `character` 归 `Character`（`builtin.character` / 角色）；
    // 复合概念 `Human Subject` 由专属别名 `人物主体` / `人物` 承载。
    let outcome_character = resolve_controlled_tag_two_stage("character", Some("zh"));
    assert_eq!(outcome_character, Some(("builtin.character", Some("角色".to_string()))));

    // ─── 场景 6：英文形态学词形还原（复数/分词）两阶段反查与就地本地化 ───
    load_aliases_for_lang("zh", vec![
        ("元素", "omw.05868954.n", true),
        ("表格", "omw.08266235.n", true),
        ("创建", "omw.01617192.v", true),
    ]);
    load_aliases_for_lang("en", vec![
        ("element", "omw.05868954.n", true),
        ("table", "omw.08266235.n", true),
        ("create", "omw.01617192.v", true),
    ]);

    // 复数 Elements -> 原型 element -> omw.05868954.n -> 元素
    let outcome_elements = resolve_controlled_tag_two_stage("Elements", Some("zh"));
    assert_eq!(outcome_elements, Some(("omw.05868954.n", Some("元素".to_string()))));

    // 复数 Tables -> 原型 table -> omw.08266235.n -> 表格
    let outcome_tables = resolve_controlled_tag_two_stage("Tables", Some("zh"));
    assert_eq!(outcome_tables, Some(("omw.08266235.n", Some("表格".to_string()))));

    // 分词 Creating -> 原型 create -> omw.01617192.v -> 创建
    let outcome_creating = resolve_controlled_tag_two_stage("Creating", Some("zh"));
    assert_eq!(outcome_creating, Some(("omw.01617192.v", Some("创建".to_string()))));

    // 过去分词 Connected -> 原型 connect -> builtin.connect -> 连接
    let outcome_connected = resolve_controlled_tag_two_stage("Connected", Some("zh"));
    assert_eq!(outcome_connected, Some(("builtin.connect", Some("连接".to_string()))));

    // ─── 场景 7（#718 R1）：tag_matches_concept 对 code 形态输入的解析通道 ───
    // 复现实测回归面：#718 汇聚侧同源化后 ram 条目以受控 code 进入门禁集合，
    // 而别名表键全为词面（tag_aliases_* 实测 0 条 code 形态 lemma），原实现恒走
    // 串等价 fallback → 互斥/域矩阵/文字存在性门禁对 ram 条目静默失明。
    clear_dynamic_aliases();
    load_aliases_for_lang("zh", vec![
        ("漫画", "omw.06780069.n", true),
        ("特写", "omw.03049695.n", true),
    ]);
    // code 形态 tag vs 词面 canonical：同词面同 code（dynamic_aliases 单一仲裁）必须命中
    assert!(tag_matches_concept("omw.06780069.n", "漫画"));
    assert!(tag_matches_concept("omw.03049695.n", "特写"));
    // 两侧均为 code 形态（同 code）
    assert!(tag_matches_concept("omw.06780069.n", "omw.06780069.n"));
    // 不同概念的 code / code vs 异词面不得误判
    assert!(!tag_matches_concept("omw.06780069.n", "特写"));
    assert!(!tag_matches_concept("omw.06780069.n", "omw.03049695.n"));
    // 词面 vs 词面（原行为保持：两侧归一后 code 相等）
    assert!(tag_matches_concept("漫画", "漫画"));
    // 开放集 _ext.* 无受控语义：保持 fallback 串等价旧行为，不误命中受控概念
    assert!(!tag_matches_concept("_ext.a1b2c3d4", "漫画"));
    // canonical 词面未受控（动态/静态双轨均未命中）→ 受控 code 与词面字面不等 → false
    assert!(!tag_matches_concept("omw.06780069.n", "完全无关词Q"));

    clear_dynamic_aliases();
}

/// 回归守卫：tag_display 必须能解析 omw.*/hownet.* 的母语规范名。
/// 静态 BUILTIN_ALIASES 只登记 builtin.*，历史上 tag_display 从不查 semantic.pack
/// 热载入轨写入的 dynamic_canonical_by_lang，导致这两层受控概念的展示名被降级成裸 code。
#[test]
fn tag_display_resolves_dynamic_canonical_for_omw_and_hownet() {
    let _guard = serial();
    load_aliases_for_lang(
        "zh-CN",
        vec![("测试心智", "omw.05617606.n", true), ("测试义原", "hownet.9999999", true)],
    );
    // locale 与归一短码都必须命中同一规范名
    assert_eq!(tag_display("omw.05617606.n", "zh-CN"), "测试心智");
    assert_eq!(tag_display("omw.05617606.n", "zh"), "测试心智");
    assert_eq!(tag_display("hownet.9999999", "zh-CN"), "测试义原");

    // 脏别名（词形被冻结成 code）不得被当作规范展示名采纳：
    // tag_display 仍回吐 code，弃用责任在调用方词形闸（fusion.rs 三处 canon != code 过滤）
    load_aliases_for_lang("zh-CN", vec![("omw.00202784.v", "omw.00202784.v", true)]);
    assert_eq!(tag_display("omw.00202784.v", "zh-CN"), "omw.00202784.v");

    // 完全未登录的 code 保持旧行为（原样返回）
    assert_eq!(tag_display("omw.99999999.n", "zh-CN"), "omw.99999999.n");

    clear_dynamic_aliases();
}
