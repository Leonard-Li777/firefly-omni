/**
 * 标签父级互斥语义判定（前端）
 *
 * 与 `apps/omni/omni-pro/crates/omni-text/src/fusion.rs` 的
 * `OmniMultimodalFusionEngine::is_parent_multi_select` 100% 对齐（ADR-0035 三分区 code 契约）。
 *
 * 采用 `builtin.<slug>.` 前缀匹配而非整段 code 全等，以在保持 slug 语义精确的同时
 * 兼容 `{hash8}` 后缀的重新生成（禁止退化为宽泛的 includes 子串匹配）。
 */

/** 单选互斥父级 slug 前缀集（同父兄弟标签只能胜出一个） */
const SINGLE_SELECT_PARENT_PREFIXES: readonly string[] = [
  'builtin.tu_pian_xi_fen.', // 图片细分
  'builtin.sheying_zhaopian.', // 摄影照片细分
  'builtin.jietu.', // 截图细分
  'builtin.hetong_piaoju.', // 合同票据细分
  'builtin.zhengzhao.', // 证照细分
  'builtin.shi_pai.', // 生成载体
  'builtin.te_xie.', // 景别
  'builtin.ri_guang.', // 光照
  'builtin.tou_ming_bei_jing.', // 背景
  'builtin.ping_shi.', // 视角
  'builtin.chun_wen_zi.', // 版面
  'builtin.windows_jietu.', // 系统生态
  'builtin.heng_bing.', // 画幅
  'builtin.dan_ren.', // 主体数量
  'builtin.wei_liang_wen_ben.', // 文字密度
  'builtin.manhua.', // 漫画细分
  'builtin.quan_nian_ling.', // 内容尺度
  'builtin.wu_ma.', // 打码程度
  'builtin.wu_shuiyin.', // 水印程度
  'builtin.gaozhiliang.', // 文件质量
  'builtin.guang_liang_hao.', // 照片质量
  'builtin.xue_xing.', // 血腥细分
  'builtin.she_zheng.', // 涉政细分
  'builtin.wei_gui.', // 违规细分
  'builtin.se_qing.' // 色情细分
]

/**
 * 判断父级是否支持多选。
 * 缺少父级信息时安全回退为「多选并存」，避免误判为互斥胜出。
 */
export function isParentMultiSelect(parentCode?: string): boolean {
  if (!parentCode) return true
  const p = parentCode.trim()
  return !SINGLE_SELECT_PARENT_PREFIXES.some((prefix) => p.startsWith(prefix))
}
