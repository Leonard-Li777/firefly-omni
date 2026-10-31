import React, { useCallback, useEffect, useState } from 'react'
import {
  GitBranch,
  Tag,
  ShieldCheck,
  FolderTree,
  RotateCw,
  ChevronRight,
  ChevronDown,
  Database,
  BarChart3,
  AlertTriangle,
  RefreshCw,
  Layers,
  Boxes
} from 'lucide-react'
import type { TreeNode, UnmappedGroup, UnmappedStats } from './taxonomy-types'

/**
 * 全树调试看板仅 Dev/Test 挂载：
 * `import.meta.env.DEV` 在生产构建被静态折叠为 `false`，动态 import 分支成为
 * 死代码，Rollup 不会为该 chunk 生成产物（满足 #650 分发隔离约束）。
 */
const TaxonomyDebugBoard = import.meta.env.DEV
  ? React.lazy(() =>
      import('./TaxonomyDebugBoard').then((m) => ({ default: m.TaxonomyDebugBoard }))
    )
  : null

/** 常用根节点快捷入口（三分区 code 契约示例） */
const ROOT_PRESETS: Array<{ label: string; code: string; badgeColor: string }> = [
  {
    label: '主题内容根',
    code: 'builtin.zhu_ti_nei_rong.13364ec8',
    badgeColor: 'bg-indigo-500/10 text-indigo-400 border-indigo-500/30'
  },
  {
    label: '内容标签根',
    code: 'builtin.nei_rong_biaoqian.230da343',
    badgeColor: 'bg-emerald-500/10 text-emerald-400 border-emerald-500/30'
  },
  {
    label: 'OMW 概念示例',
    code: 'omw.o-dog.n',
    badgeColor: 'bg-amber-500/10 text-amber-400 border-amber-500/30'
  }
]

const sourceBadgeClass = (source: string): string => {
  switch (source) {
    case 'builtin':
      return 'bg-indigo-500/10 text-indigo-300 border-indigo-500/30'
    case 'omw':
      return 'bg-amber-500/10 text-amber-300 border-amber-500/30'
    case '_ext':
      return 'bg-emerald-500/10 text-emerald-300 border-emerald-500/30'
    default:
      return 'bg-slate-500/10 text-slate-400 border-slate-500/30'
  }
}

/** 单层懒加载树节点（展开时才请求 `GET /api/v1/omw/tree?root=<code>`） */
const LazyTreeNode: React.FC<{
  node: TreeNode
  expanded: Record<string, TreeNode[]>
  loadingCode: string | null
  onToggle: (node: TreeNode) => void
}> = ({ node, expanded, loadingCode, onToggle }) => {
  const children = expanded[node.code]
  const isLoading = loadingCode === node.code

  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between bg-slate-950/60 hover:bg-slate-950 border border-slate-800/70 rounded-lg px-2.5 py-1.5 text-[11px] transition-colors">
        <button
          onClick={() => onToggle(node)}
          className="flex items-center min-w-0 flex-1 text-left"
          title="点击展开/收起直接子节点"
        >
          {isLoading ? (
            <RotateCw className="w-3.5 h-3.5 mr-1.5 text-indigo-400 animate-spin flex-shrink-0" />
          ) : children ? (
            <ChevronDown className="w-3.5 h-3.5 mr-1.5 text-slate-400 flex-shrink-0" />
          ) : (
            <ChevronRight className="w-3.5 h-3.5 mr-1.5 text-slate-500 flex-shrink-0" />
          )}
          <span className="text-slate-200 truncate">{node.name}</span>
          <span
            className={`ml-2 px-1.5 py-0.5 rounded border text-[9px] font-mono flex-shrink-0 ${sourceBadgeClass(
              node.source
            )}`}
          >
            {node.source}
          </span>
        </button>
        <code className="text-[10px] text-slate-500 font-mono ml-2 flex-shrink-0">{node.code}</code>
      </div>

      {children && children.length > 0 && (
        <div className="pl-3 border-l border-slate-800 space-y-1">
          {children.map((child) => (
            <LazyTreeNode
              key={child.code}
              node={child}
              expanded={expanded}
              loadingCode={loadingCode}
              onToggle={onToggle}
            />
          ))}
        </div>
      )}

      {children && children.length === 0 && (
        <div className="pl-3 border-l border-slate-800 text-[10px] text-slate-600 py-0.5">
          无子节点
        </div>
      )}
    </div>
  )
}

/** 聚合统计看板（词类 / 顶层抽象分类） */
const StatsPanel: React.FC<{ title: string; icon: React.ReactNode; groups: UnmappedGroup[] }> = ({
  title,
  icon,
  groups
}) => {
  const max = groups.reduce((acc, g) => Math.max(acc, g.count), 0) || 1
  return (
    <div className="bg-slate-950/60 border border-slate-800/80 rounded-xl p-3 space-y-2">
      <div className="flex items-center text-xs font-bold text-slate-300">
        {icon}
        <span className="ml-1.5">{title}</span>
      </div>
      {groups.length === 0 ? (
        <div className="text-[11px] text-slate-500 py-2">暂无未映射概念</div>
      ) : (
        <div className="space-y-1.5">
          {groups.map((g) => (
            <div key={g.key} className="space-y-1">
              <div className="flex items-center justify-between text-[11px]">
                <span className="text-slate-300 truncate font-mono" title={g.key}>
                  {g.key}
                </span>
                <span className="text-slate-400 font-mono flex-shrink-0 ml-2">{g.count}</span>
              </div>
              <div className="w-full bg-slate-800 h-1.5 rounded-full overflow-hidden">
                <div
                  className="h-full rounded-full bg-gradient-to-r from-indigo-500 to-emerald-500"
                  style={{ width: `${Math.max(4, (g.count / max) * 100)}%` }}
                />
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

export const TaxonomyTab: React.FC = () => {
  const [rootInput, setRootInput] = useState<string>(ROOT_PRESETS[0].code)
  const [expanded, setExpanded] = useState<Record<string, TreeNode[]>>({})
  const [loadingCode, setLoadingCode] = useState<string | null>(null)
  const [stats, setStats] = useState<UnmappedStats | null>(null)
  const [statsLoading, setStatsLoading] = useState<boolean>(false)
  const [errorMsg, setErrorMsg] = useState<string | null>(null)

  const fetchChildren = useCallback(async (code: string): Promise<TreeNode[]> => {
    const res = await fetch(`/api/v1/omw/tree?root=${encodeURIComponent(code)}&depth=1`)
    if (!res.ok) throw new Error(`HTTP ${res.status}: ${res.statusText}`)
    return (await res.json()) as TreeNode[]
  }, [])

  const loadRoot = useCallback(
    async (code: string) => {
      const trimmed = code.trim()
      if (!trimmed) {
        setErrorMsg('请输入根节点 code')
        return
      }
      setLoadingCode(trimmed)
      setErrorMsg(null)
      setExpanded({})
      try {
        const children = await fetchChildren(trimmed)
        setExpanded({ [trimmed]: children })
        if (children.length === 0) {
          setErrorMsg('该根节点下无直接子节点（或 code 不存在）')
        }
      } catch (err: any) {
        setErrorMsg(err.message || '单层懒加载失败')
      } finally {
        setLoadingCode(null)
      }
    },
    [fetchChildren]
  )

  const handleToggle = useCallback(
    async (node: TreeNode) => {
      // 已展开则收起
      if (expanded[node.code]) {
        setExpanded((prev) => {
          const next = { ...prev }
          delete next[node.code]
          return next
        })
        return
      }
      setLoadingCode(node.code)
      try {
        const children = await fetchChildren(node.code)
        setExpanded((prev) => ({ ...prev, [node.code]: children }))
      } catch (err: any) {
        setErrorMsg(err.message || `展开 ${node.code} 失败`)
      } finally {
        setLoadingCode(null)
      }
    },
    [expanded, fetchChildren]
  )

  const loadStats = useCallback(async () => {
    setStatsLoading(true)
    try {
      const res = await fetch('/api/v1/omw/unmapped-stats')
      if (!res.ok) throw new Error(`HTTP ${res.status}: ${res.statusText}`)
      setStats((await res.json()) as UnmappedStats)
    } catch (err: any) {
      setErrorMsg(err.message || '未映射统计加载失败')
    } finally {
      setStatsLoading(false)
    }
  }, [])

  useEffect(() => {
    loadRoot(ROOT_PRESETS[0].code)
    loadStats()
  }, [loadRoot, loadStats])

  const rootCode = rootInput.trim()
  const rootChildren = expanded[rootCode] || []

  return (
    <div className="flex-1 flex flex-col min-h-0 space-y-6">
      {/* 顶部标题与架构导引横幅 */}
      <div className="bg-gradient-to-r from-slate-900 via-indigo-950/40 to-slate-900 border border-slate-800/80 rounded-2xl p-6 shadow-xl relative overflow-hidden">
        <div className="absolute right-0 top-0 bottom-0 w-96 bg-gradient-to-l from-indigo-500/10 to-transparent pointer-events-none" />
        <div className="flex flex-col md:flex-row md:items-center justify-between gap-4 relative z-10">
          <div>
            <div className="flex items-center space-x-2.5 mb-2">
              <span className="p-2 rounded-xl bg-indigo-500/20 text-indigo-400 border border-indigo-500/30">
                <GitBranch className="w-5 h-5" />
              </span>
              <h1 className="text-xl font-bold text-slate-100 tracking-tight">
                OMW 统一标签树懒加载与未映射统计看板
              </h1>
              <span className="px-2.5 py-0.5 rounded-full text-xs font-mono font-medium bg-indigo-500/20 text-indigo-300 border border-indigo-500/40">
                ADR-0035 & Task 650
              </span>
            </div>
            <p className="text-xs text-slate-400 max-w-3xl leading-relaxed">
              标签树以 <code className="text-indigo-300 font-mono">parent_codes</code> 语义建树，
              <code className="text-amber-300 font-mono">builtin.*</code> /{' '}
              <code className="text-amber-300 font-mono">omw.*</code> /{' '}
              <code className="text-emerald-300 font-mono">_ext.*</code> 三分区 code 混存。为规避
              11.7 万节点的全量序列化，单层按需展开（<code className="text-emerald-300 font-mono">?depth=1</code>
              ），未映射洞察以聚合计数呈现。
            </p>
          </div>

          <div className="flex items-center space-x-3 flex-shrink-0">
            <div className="text-right hidden sm:block">
              <div className="text-[11px] text-slate-400">全量吐树</div>
              <div className="text-sm font-bold font-mono text-rose-400">已禁用</div>
            </div>
            <div className="h-8 w-px bg-slate-800" />
            <div className="text-right hidden sm:block">
              <div className="text-[11px] text-slate-400">单层响应</div>
              <div className="text-sm font-bold font-mono text-emerald-400">&lt; 50 ms</div>
            </div>
          </div>
        </div>
      </div>

      {/* 主体工作区 */}
      <div className="grid grid-cols-1 lg:grid-cols-12 gap-6 flex-1 min-h-0">
        {/* 左侧：8 列单层懒加载树 */}
        <div className="lg:col-span-7 flex flex-col space-y-6">
          <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-5 space-y-4">
            <div className="flex items-center justify-between border-b border-slate-800 pb-3">
              <span className="text-xs font-bold text-slate-200 flex items-center">
                <FolderTree className="w-4 h-4 mr-1.5 text-indigo-400" />
                单层懒加载标签树 (GET /api/v1/omw/tree)
              </span>
              <span className="text-[11px] font-mono text-slate-500">?depth=1</span>
            </div>

            {/* 根节点快捷入口 */}
            <div className="flex flex-wrap gap-2">
              {ROOT_PRESETS.map((preset) => (
                <button
                  key={preset.code}
                  onClick={() => setRootInput(preset.code)}
                  className={`px-2.5 py-1 rounded-lg border text-[11px] transition-all hover:scale-[1.02] ${
                    rootInput === preset.code
                      ? 'bg-indigo-950/60 border-indigo-500/80 text-indigo-200'
                      : `${preset.badgeColor}`
                  }`}
                >
                  {preset.label}
                </button>
              ))}
            </div>

            <div className="flex gap-2">
              <input
                type="text"
                value={rootInput}
                onChange={(e) => setRootInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') loadRoot(rootInput)
                }}
                placeholder="根节点 code，如 builtin.zhu_ti_nei_rong.13364ec8"
                className="flex-1 bg-slate-950/80 border border-slate-800 focus:border-indigo-500 rounded-xl px-3.5 py-2 text-slate-200 focus:outline-none focus:ring-1 focus:ring-indigo-500 text-xs font-mono transition-all"
              />
              <button
                onClick={() => loadRoot(rootInput)}
                disabled={loadingCode === rootCode}
                className="px-4 py-2 rounded-xl bg-indigo-600 hover:bg-indigo-500 active:bg-indigo-700 disabled:opacity-50 text-white font-semibold text-xs flex items-center space-x-1.5 transition-all"
              >
                {loadingCode === rootCode ? (
                  <RotateCw className="w-3.5 h-3.5 animate-spin" />
                ) : (
                  <RefreshCw className="w-3.5 h-3.5" />
                )}
                <span>加载</span>
              </button>
            </div>

            {errorMsg && (
              <div className="p-3 rounded-xl bg-rose-500/10 border border-rose-500/30 text-rose-300 text-xs flex items-center">
                <AlertTriangle className="w-4 h-4 mr-2 flex-shrink-0 text-rose-400" />
                {errorMsg}
              </div>
            )}

            <div className="max-h-[28rem] overflow-y-auto pr-1 space-y-1">
              {rootChildren.length === 0 && loadingCode !== rootCode ? (
                <div className="text-xs text-slate-500 py-6 text-center">
                  暂无子节点，请输入有效根节点 code
                </div>
              ) : (
                rootChildren.map((node) => (
                  <LazyTreeNode
                    key={node.code}
                    node={node}
                    expanded={expanded}
                    loadingCode={loadingCode}
                    onToggle={handleToggle}
                  />
                ))
              )}
            </div>
          </div>
        </div>

        {/* 右侧：5 列未映射统计看板 */}
        <div className="lg:col-span-5 flex flex-col space-y-6">
          <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-5 space-y-4">
            <div className="flex items-center justify-between border-b border-slate-800 pb-3">
              <div className="flex items-center space-x-2">
                <span className="p-1.5 rounded-lg bg-emerald-500/20 text-emerald-400">
                  <BarChart3 className="w-4 h-4" />
                </span>
                <div>
                  <h3 className="text-xs font-bold text-slate-200">
                    未映射概念聚合统计 (GET /api/v1/omw/unmapped-stats)
                  </h3>
                  <p className="text-[10px] text-slate-500">
                    未被任何 file_tags.parent_codes 引用的 OMW 概念
                  </p>
                </div>
              </div>
              <button
                onClick={loadStats}
                disabled={statsLoading}
                className="p-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 text-slate-300 border border-slate-700 transition-all"
                title="刷新统计"
              >
                <RotateCw className={`w-3.5 h-3.5 ${statsLoading ? 'animate-spin' : ''}`} />
              </button>
            </div>

            <StatsPanel
              title="按词类 / 领域 (lexfile)"
              icon={<Layers className="w-3.5 h-3.5 text-indigo-400" />}
              groups={stats?.byLexfile || []}
            />
            <StatsPanel
              title="按顶层抽象分类 (top ancestor)"
              icon={<Boxes className="w-3.5 h-3.5 text-emerald-400" />}
              groups={stats?.byTopAncestor || []}
            />
          </div>

          {/* 架构说明卡片 */}
          <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-5 space-y-3">
            <div className="flex items-center space-x-2 border-b border-slate-800 pb-3">
              <span className="p-1.5 rounded-lg bg-amber-500/20 text-amber-400">
                <Database className="w-4 h-4" />
              </span>
              <h3 className="text-xs font-bold text-slate-200">只读直连架构 (ADR-0035 §8.4)</h3>
            </div>
            <div className="space-y-2 text-[11px] text-slate-400 leading-relaxed">
              <p className="flex items-start">
                <ShieldCheck className="w-3.5 h-3.5 mr-1.5 text-emerald-400 flex-shrink-0 mt-0.5" />
                Desktop 与 Omni 共享同一 SQLite 文件；Omni 以只读连接池挂载，绝不写入。
              </p>
              <p className="flex items-start">
                <Tag className="w-3.5 h-3.5 mr-1.5 text-indigo-400 flex-shrink-0 mt-0.5" />
                语言切换由 Desktop 发起 <code className="text-slate-300 font-mono">POST /api/reconnect</code> 热重载物理库。
              </p>
            </div>
          </div>

          {/* Dev/Test 专属：全树调试看板（Release 构建剥离） */}
          {TaxonomyDebugBoard && (
            <React.Suspense
              fallback={
                <div className="text-xs text-slate-500 p-4 text-center">调试看板加载中...</div>
              }
            >
              <TaxonomyDebugBoard />
            </React.Suspense>
          )}
        </div>
      </div>
    </div>
  )
}
