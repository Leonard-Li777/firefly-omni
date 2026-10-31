/**
 * OMW 统一标签树前端契约类型
 *
 * 字段命名与 `omni-server` 的 `omw_query.rs` 序列化结果严格对齐：
 * `#[serde(rename_all = "camelCase")]`。
 */

/** `GET /api/v1/omw/tree` 单层子节点 */
export interface TreeNode {
  code: string
  name: string
  parentCodes: string[]
  /** 由 code 前缀推断：builtin / omw / _ext / unknown */
  source: string
  /** 服务端固定 0，前端可依 parentCodes 递归推导 */
  depth: number
}

/** 未映射概念分组计数 */
export interface UnmappedGroup {
  key: string
  count: number
}

/** `GET /api/v1/omw/unmapped-stats` 聚合结果 */
export interface UnmappedStats {
  byLexfile: UnmappedGroup[]
  byTopAncestor: UnmappedGroup[]
}
