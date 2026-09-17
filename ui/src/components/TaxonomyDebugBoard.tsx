import React, { useState } from 'react'
import { AlertTriangle, FolderTree, RotateCw, Database } from 'lucide-react'
import type { TreeNode } from './taxonomy-types'

/**
 * 全树调试看板（仅 Dev/Test）
 *
 * 按需递归展开子树，供开发者核对三分区 code 契约与 `parent_codes` 建树正确性。
 * 本组件不得进入 Release 产物：`TaxonomyTab` 通过 `import.meta.env.DEV` 守卫 +
 * 动态 import 引用，生产构建时分支被静态折叠、chunk 不会被 Rollup 发射。
 */
export const TaxonomyDebugBoard: React.FC = () => {
  const [rootCode, setRootCode] = useState<string>('builtin.zhu_ti_nei_rong.13364ec8')
  const [maxDepth, setMaxDepth] = useState<number>(3)
  // 已展开节点的直接子节点缓存：code -> TreeNode[]
  const [children, setChildren] = useState<Record<string, TreeNode[]>>({})
  const [loading, setLoading] = useState<boolean>(false)
  const [errorMsg, setErrorMsg] = useState<string | null>(null)
  const [rendered, setRendered] = useState<number>(0)

  const fetchChildren = async (code: string): Promise<TreeNode[]> => {
    const res = await fetch(`/api/v1/omw/tree?root=${encodeURIComponent(code)}&depth=1`)
    if (!res.ok) throw new Error(`HTTP ${res.status}: ${res.statusText}`)
    return (await res.json()) as TreeNode[]
  }

  const handleExpandAll = async () => {
    if (!rootCode.trim()) {
      setErrorMsg('请输入起始根节点 code')
      return
    }
    setLoading(true)
    setErrorMsg(null)
    const cache: Record<string, TreeNode[]> = {}
    let count = 0
    try {
      // 广度优先逐层抓取，硬上限 maxDepth 层，杜绝一次拉爆 11.7 万节点
      let frontier: Array<{ code: string; depth: number }> = [
        { code: rootCode.trim(), depth: 0 }
      ]
      for (let level = 0; level < maxDepth; level++) {
        const next: Array<{ code: string; depth: number }> = []
        for (const item of frontier) {
          const kids = await fetchChildren(item.code)
          cache[item.code] = kids
          count += kids.length
          for (const kid of kids) {
            next.push({ code: kid.code, depth: level + 1 })
          }
        }
        frontier = next
        if (frontier.length === 0) break
      }
      setChildren(cache)
      setRendered(count)
      if (count === 0) setErrorMsg('该根节点下无任何子标签（或根 code 不存在）')
    } catch (err: any) {
      setErrorMsg(err.message || '全树展开失败')
    } finally {
      setLoading(false)
    }
  }

  const renderNode = (code: string, depth: number): React.ReactNode => {
    const kids = children[code]
    if (!kids || depth > maxDepth) return null
    return (
      <div className="pl-3 border-l border-slate-800 space-y-1">
        {kids.map((kid) => (
          <div key={kid.code} className="space-y-1">
            <div className="flex items-center justify-between bg-slate-900/60 px-2 py-1 rounded border border-slate-800/60 text-[11px]">
              <span className="text-slate-200 truncate">{kid.name}</span>
              <code className="text-[10px] text-slate-500 font-mono ml-2 flex-shrink-0">
                {kid.code}
              </code>
            </div>
            {children[kid.code] && renderNode(kid.code, depth + 1)}
          </div>
        ))}
      </div>
    )
  }

  return (
    <div className="bg-slate-900/60 border border-amber-500/30 rounded-2xl p-5 space-y-4">
      <div className="flex items-center justify-between border-b border-slate-800 pb-3">
        <div className="flex items-center space-x-2">
          <span className="p-1.5 rounded-lg bg-amber-500/20 text-amber-400">
            <FolderTree className="w-4 h-4" />
          </span>
          <div>
            <h3 className="text-xs font-bold text-slate-200 flex items-center gap-2">
              全树调试看板
              <span className="px-2 py-0.5 rounded text-[10px] font-mono bg-amber-500/20 text-amber-300 border border-amber-500/40">
                DEV ONLY
              </span>
            </h3>
            <p className="text-[10px] text-slate-500">
              递归广度优先按需抓取，绝不提供全量吐树端点（§8.4 硬约束）
            </p>
          </div>
        </div>
        <Database className="w-4 h-4 text-slate-500" />
      </div>

      <div className="grid grid-cols-1 sm:grid-cols-3 gap-3 text-xs">
        <div className="sm:col-span-2">
          <label className="block text-slate-300 font-semibold mb-1">起始根节点 code</label>
          <input
            type="text"
            value={rootCode}
            onChange={(e) => setRootCode(e.target.value)}
            placeholder="如 builtin.zhu_ti_nei_rong.13364ec8 / omw.o-dog.n"
            className="w-full bg-slate-950/80 border border-slate-800 focus:border-amber-500 rounded-xl px-3 py-2 text-slate-200 focus:outline-none focus:ring-1 focus:ring-amber-500 text-xs font-mono"
          />
        </div>
        <div>
          <label className="block text-slate-300 font-semibold mb-1">最大展开层数</label>
          <select
            value={maxDepth}
            onChange={(e) => setMaxDepth(Number(e.target.value))}
            className="w-full bg-slate-950/80 border border-slate-800 focus:border-amber-500 rounded-xl px-3 py-2 text-slate-200 text-xs"
          >
            {[1, 2, 3, 4, 5].map((d) => (
              <option key={d} value={d}>
                {d} 层
              </option>
            ))}
          </select>
        </div>
      </div>

      {errorMsg && (
        <div className="p-3 rounded-xl bg-rose-500/10 border border-rose-500/30 text-rose-300 text-xs flex items-center">
          <AlertTriangle className="w-4 h-4 mr-2 flex-shrink-0 text-rose-400" />
          {errorMsg}
        </div>
      )}

      <button
        onClick={handleExpandAll}
        disabled={loading || !rootCode.trim()}
        className="w-full py-2.5 px-4 rounded-xl bg-amber-600 hover:bg-amber-500 active:bg-amber-700 disabled:opacity-50 text-slate-950 font-semibold text-xs flex items-center justify-center space-x-2 transition-all"
      >
        {loading ? (
          <>
            <RotateCw className="w-4 h-4 animate-spin" />
            <span>逐层抓取中...</span>
          </>
        ) : (
          <span>展开全树（最多 {maxDepth} 层）</span>
        )}
      </button>

      {rendered > 0 && (
        <div className="text-[11px] text-slate-500">
          已渲染节点数：<span className="text-amber-300 font-mono">{rendered}</span>
        </div>
      )}

      <div className="max-h-96 overflow-y-auto space-y-1 pr-1">
        {renderNode(rootCode.trim(), 0)}
      </div>
    </div>
  )
}
