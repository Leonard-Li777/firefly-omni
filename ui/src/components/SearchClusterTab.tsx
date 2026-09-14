import React, { useState } from 'react'
import {
  Search,
  FolderTree,
  Database,
  Sparkles,
  Folder,
  FolderOpen,
  FileText,
  ChevronRight,
  ChevronDown,
  CheckCircle2,
  AlertTriangle,
  RotateCw
} from 'lucide-react'

// 混合检索与聚类类型定义
export interface IndexedDocument {
  id: string
  title: string
  searchableText: string
  tags?: string[]
}

export interface FusedResult {
  id?: string
  fingerprint?: string
  title?: string
  snippet?: string
  bm25Score?: number
  denseScore?: number
  bm25Rank?: number | null
  denseRank?: number | null
  rrfScore: number
  finalScore?: number
}

export interface ClusterTreeNode {
  id: string
  name: string
  depth: number
  documentIds: string[]
  children?: ClusterTreeNode[]
}

export interface ClusterTreeResult {
  root: ClusterTreeNode
  totalDocuments: number
  clustersCount: number
}

// 预置 10 篇微文档池
const PRESET_DOCUMENTS: IndexedDocument[] = [
  {
    id: 'doc-1',
    title: '华润科技2025年度财务审计清算报告',
    searchableText: '截至2025年12月31日，公司年度总营收达到人民币 128,500,000.00 元。专项审计由立信会计师事务所执行，涉及应收账款总额 12,300,500.00 元，净利润增长 18.5%。',
    tags: ['财务', '审计', '年报']
  },
  {
    id: 'doc-2',
    title: '北京研发分部差旅与报销发票凭证汇总',
    searchableText: '技术团队赴北京研发中心现场部署产生的差旅机票报销单、酒店发票与餐费凭据，发票总金额 18,450.00 元已审核入账。',
    tags: ['财务', '报销', '发票']
  },
  {
    id: 'doc-3',
    title: '智源未来高级架构师劳动雇佣合同书',
    searchableText: '上海智源未来与张子涵签订全日制劳动合同。聘任岗位为高级前端架构师，试用期三个月，基本月薪为人民币 35,000.00 元整。',
    tags: ['人事', '合同', '法务']
  },
  {
    id: 'doc-4',
    title: '2026年年度薪酬与员工福利关怀确认函',
    searchableText: '人力资源部关于全员公积金缴纳基数调整、补充商业医疗保险以及年终绩效考核等级的正式确认函件。',
    tags: ['人事', '薪酬', '福利']
  },
  {
    id: 'doc-5',
    title: 'Firefly Omni-Server 离线全模态特征引擎架构规范',
    searchableText: '系统采用纯 CPU 架构运行，集成 fastText 语种检测、KeyBERT 主题抽取与 bekko-a8m 384d 特征嵌入向量，SLO 控制在 15ms 内。',
    tags: ['技术', '架构', 'Rust']
  },
  {
    id: 'doc-6',
    title: 'SQLite 数据库单表标签树与递归 CTE 持久化方案',
    searchableText: '创世基线下 file_tags 单表物理树形结构设计，支持 code 路径物化，利用 SQLite 递归公用表表达式下推查询，单事务原子持久化。',
    tags: ['技术', '数据库', '标签树']
  },
  {
    id: 'doc-7',
    title: 'Chromium 远程调试 CDP 与多 Worktree 端口隔离规范',
    searchableText: '为支持多工作树并发开发，高位冷门端口基准 38200 具备自动滑动自增机制，独立调试端口槽位实现开发隔离防误触。',
    tags: ['技术', '运维', '网络']
  },
  {
    id: 'doc-8',
    title: '2026年4月产品总监与算法团队跨部门周会纪要',
    searchableText: '周会纪要：下周二前完成桌面端单点标签持久化 TagReconciliationArbiter 胶水收拢，虚拟目录一键聚类弹窗默认最大深度为 3 层。',
    tags: ['行政', '会议', '纪要']
  },
  {
    id: 'doc-9',
    title: '深圳总部春季团建出游行程安排与安全注意事项',
    searchableText: '行政部组织的深圳大鹏半岛户外团建活动方案，包含大巴发车时刻表、午餐安排、户外拓展运动及安全医疗应急预案。',
    tags: ['行政', '团建', '活动']
  },
  {
    id: 'doc-10',
    title: '办公区固定工位资产与电子设备年度盘点清单',
    searchableText: '研发区与行政区笔记本电脑、4K 显示器、测试工程机及工位办公桌椅等硬件资产的序列号登记与责任人盘点明细表。',
    tags: ['行政', '资产', '硬件']
  }
]

export const SearchClusterTab: React.FC = () => {
  // 1. 索引构建状态
  const [documents] = useState<IndexedDocument[]>(PRESET_DOCUMENTS)
  const [isIndexing, setIsIndexing] = useState<boolean>(false)
  const [indexedCount, setIndexedCount] = useState<number | null>(null)
  const [indexDurationMs, setIndexDurationMs] = useState<number | null>(null)
  const [indexError, setIndexError] = useState<string | null>(null)

  // 2. 双轨检索状态
  const [queryText, setQueryText] = useState<string>('报销发票凭证')
  const [topK, setTopK] = useState<number>(5)
  const [isSearching, setIsSearching] = useState<boolean>(false)
  const [searchResults, setSearchResults] = useState<FusedResult[]>([])
  const [searchError, setSearchError] = useState<string | null>(null)

  // 3. HAC 聚类状态
  const [clusterPrompt, setClusterPrompt] = useState<string>('按财务资金、技术研发、人事行政三类整理归类')
  const [distanceThreshold, setDistanceThreshold] = useState<number>(0.4)
  const [maxLeafSize, setMaxLeafSize] = useState<number>(50)
  const [isClustering, setIsClustering] = useState<boolean>(false)
  const [clusterTree, setClusterTree] = useState<ClusterTreeResult | null>(null)
  const [clusterError, setClusterError] = useState<string | null>(null)
  const [expandedNodes, setExpandedNodes] = useState<Record<string, boolean>>({ root: true })

  // 批量构建索引
  const handleBuildIndex = async () => {
    setIsIndexing(true)
    setIndexError(null)
    const t0 = performance.now()
    try {
      const res = await fetch('/api/search/index', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          documents: documents.map(d => ({
            id: d.id,
            title: d.title,
            searchableText: d.searchableText,
            tags: d.tags || [],
            embedding: [] // 由 omni 自动补充 384d 向量
          }))
        })
      })
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}: ${res.statusText}`)
      }
      const data = await res.json()
      if (!data.success) {
        throw new Error(data.error || '索引构建失败')
      }
      setIndexedCount(data.totalIndexed)
      setIndexDurationMs(Math.round(performance.now() - t0))
    } catch (err: any) {
      setIndexError(err.message || '请求索引服务失败')
    } finally {
      setIsIndexing(false)
    }
  }

  // 执行双轨混合检索
  const handleSearch = async (kw?: string) => {
    const text = (kw ?? queryText).trim()
    if (!text) {
      setSearchError('请输入检索关键词')
      return
    }
    setQueryText(text)
    setIsSearching(true)
    setSearchError(null)
    try {
      const res = await fetch('/api/search/hybrid', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          queryText: text,
          topK
        })
      })
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}: ${res.statusText}`)
      }
      const data = await res.json()
      if (!data.success) {
        throw new Error(data.error || '混合检索失败')
      }
      setSearchResults(data.results || [])
    } catch (err: any) {
      setSearchError(err.message || '执行混合检索失败')
    } finally {
      setIsSearching(false)
    }
  }

  // 执行约束层次聚类
  const handleCluster = async () => {
    setIsClustering(true)
    setClusterError(null)
    try {
      const res = await fetch('/api/search/cluster', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          documents: documents.map(d => ({
            id: d.id,
            title: d.title,
            text: d.searchableText,
            embedding: [] // 由 omni 自动补充 384d 向量
          })),
          prompt: clusterPrompt || undefined,
          distanceThreshold,
          maxLeafSize
        })
      })
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}: ${res.statusText}`)
      }
      const data = await res.json()
      if (!data.success) {
        throw new Error(data.error || '聚类失败')
      }
      setClusterTree(data.result)
      // 默认展开所有一级目录
      if (data.result?.root?.id) {
        setExpandedNodes({ [data.result.root.id]: true })
      }
    } catch (err: any) {
      setClusterError(err.message || '执行层次聚类失败')
    } finally {
      setIsClustering(false)
    }
  }

  const toggleNode = (nodeId: string) => {
    setExpandedNodes(prev => ({
      ...prev,
      [nodeId]: !prev[nodeId]
    }))
  }

  // 递归渲染聚类树节点
  const renderTreeNode = (node: ClusterTreeNode) => {
    const isExpanded = !!expandedNodes[node.id]
    const hasChildren = node.children && node.children.length > 0
    const hasDocs = node.documentIds && node.documentIds.length > 0

    return (
      <div key={node.id} className="space-y-1 text-xs">
        <div
          onClick={() => toggleNode(node.id)}
          className="flex items-center gap-2 p-2 rounded-lg hover:bg-slate-800/60 cursor-pointer transition-colors select-none group"
          style={{ paddingLeft: `${node.depth * 16 + 8}px` }}
        >
          {hasChildren ? (
            isExpanded ? (
              <ChevronDown className="w-3.5 h-3.5 text-slate-400 group-hover:text-slate-200" />
            ) : (
              <ChevronRight className="w-3.5 h-3.5 text-slate-400 group-hover:text-slate-200" />
            )
          ) : (
            <span className="w-3.5 h-3.5" />
          )}

          {isExpanded ? (
            <FolderOpen className="w-4 h-4 text-amber-400 shrink-0" />
          ) : (
            <Folder className="w-4 h-4 text-amber-400 shrink-0" />
          )}

          <span className="font-medium text-slate-200 group-hover:text-white">
            {node.name || '建议分类'}
          </span>

          <span className="text-[10px] px-1.5 py-0.2 rounded bg-slate-800 text-slate-400 font-mono">
            {node.documentIds?.length || 0} 篇
          </span>
        </div>

        {isExpanded && (
          <div>
            {/* 子目录 */}
            {hasChildren && node.children!.map(child => renderTreeNode(child))}

            {/* 当前节点归属文档 */}
            {hasDocs &&
              node.documentIds.map(docId => {
                const doc = documents.find(d => d.id === docId)
                return (
                  <div
                    key={docId}
                    className="flex items-center gap-2 py-1.5 px-2 hover:bg-slate-800/30 rounded text-slate-300 transition-colors"
                    style={{ paddingLeft: `${(node.depth + 1) * 16 + 16}px` }}
                  >
                    <FileText className="w-3.5 h-3.5 text-sky-400 shrink-0" />
                    <span className="truncate">{doc?.title || docId}</span>
                    {doc?.tags && (
                      <div className="flex gap-1 ml-auto shrink-0">
                        {doc.tags.map(t => (
                          <span
                            key={t}
                            className="text-[9px] px-1 py-0.2 bg-slate-800 text-slate-500 rounded font-mono"
                          >
                            {t}
                          </span>
                        ))}
                      </div>
                    )}
                  </div>
                )
              })}
          </div>
        )}
      </div>
    )
  }

  return (
    <div className="space-y-6">
      {/* 顶部标题与说明 */}
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between pb-4 border-b border-slate-800 gap-2">
        <div>
          <h2 className="text-lg font-semibold text-slate-100 flex items-center gap-2">
            <FolderTree className="w-5 h-5 text-emerald-400" />
            双轨混合检索与 HAC 约束聚类归档
          </h2>
          <p className="text-xs text-slate-400 mt-0.5">
            USearch 384d 向量与 Tantivy BM25 加权 RRF 融合排序，结合 kodama 提示词引导层次聚类生成建议目录树
          </p>
        </div>
        <div className="flex items-center gap-2 flex-wrap">
          <code className="text-xs bg-slate-800 px-2 py-1 rounded text-emerald-300 font-mono">
            /api/search/index
          </code>
          <code className="text-xs bg-slate-800 px-2 py-1 rounded text-sky-300 font-mono">
            /api/search/hybrid
          </code>
          <code className="text-xs bg-slate-800 px-2 py-1 rounded text-purple-300 font-mono">
            /api/search/cluster
          </code>
        </div>
      </div>

      {/* 模块 1: 内置微文档池与索引构建 */}
      <div className="p-4 rounded-xl bg-slate-900/70 border border-slate-800 space-y-3">
        <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-2">
          <div className="space-y-0.5">
            <h3 className="text-xs font-semibold text-slate-200 flex items-center gap-1.5">
              <Database className="w-3.5 h-3.5 text-sky-400" />
              预置测试微文档池 ({documents.length} 篇覆盖财务/技术/行政)
            </h3>
            <p className="text-[11px] text-slate-500">
              包含发票审计、系统架构规范、周会备忘与人事合同等典型测试样本
            </p>
          </div>
          <div className="flex items-center gap-3">
            {indexedCount !== null && (
              <span className="text-xs text-emerald-400 flex items-center gap-1 font-mono">
                <CheckCircle2 className="w-3.5 h-3.5" />
                已索引 {indexedCount} 篇 ({indexDurationMs} ms)
              </span>
            )}
            <button
              onClick={handleBuildIndex}
              disabled={isIndexing}
              className="flex items-center gap-1.5 px-3 py-1.5 bg-sky-600 hover:bg-sky-500 disabled:bg-slate-800 text-white rounded-lg text-xs font-medium transition-colors shrink-0"
            >
              {isIndexing ? (
                <>
                  <RotateCw className="w-3.5 h-3.5 animate-spin" />
                  <span>正在构建 USearch+Tantivy 索引...</span>
                </>
              ) : (
                <>
                  <Sparkles className="w-3.5 h-3.5" />
                  <span>批量建立双轨混合索引</span>
                </>
              )}
            </button>
          </div>
        </div>

        {indexError && (
          <div className="p-2.5 bg-rose-500/10 border border-rose-500/20 rounded-lg text-rose-300 text-xs flex items-center gap-2">
            <AlertTriangle className="w-4 h-4 text-rose-400 shrink-0" />
            <span>{indexError}</span>
          </div>
        )}
      </div>

      {/* 模块 2 与 模块 3 并排网格 */}
      <div className="grid grid-cols-1 lg:grid-cols-12 gap-6">
        {/* 左侧：双轨混合检索 (5 cols) */}
        <div className="lg:col-span-6 p-4 rounded-xl bg-slate-900/70 border border-slate-800 space-y-4">
          <div className="space-y-1">
            <h3 className="text-xs font-semibold text-slate-200 flex items-center gap-1.5">
              <Search className="w-3.5 h-3.5 text-sky-400" />
              双轨混合检索 (BM25 + 384d Dense + RRF)
            </h3>
            <p className="text-[11px] text-slate-500">
              输入查询短句，对比稀疏关键词匹配与密集语义向量经加权倒数排名融合后的得分
            </p>
          </div>

          <div className="space-y-2">
            <div className="flex gap-2">
              <input
                type="text"
                value={queryText}
                onChange={e => setQueryText(e.target.value)}
                onKeyDown={e => e.key === 'Enter' && handleSearch()}
                placeholder="输入检索词，如：报销凭证、架构规范..."
                className="flex-1 bg-slate-950 border border-slate-800 rounded-lg px-3 py-2 text-xs text-slate-200 placeholder-slate-600 focus:outline-none focus:border-sky-500"
              />
              <select
                value={topK}
                onChange={e => setTopK(Number(e.target.value))}
                className="bg-slate-950 border border-slate-800 rounded-lg px-2 py-2 text-xs text-slate-300 focus:outline-none focus:border-sky-500"
              >
                <option value={3}>Top 3</option>
                <option value={5}>Top 5</option>
                <option value={10}>Top 10</option>
              </select>
              <button
                onClick={() => handleSearch()}
                disabled={isSearching}
                className="px-3 py-2 bg-sky-600 hover:bg-sky-500 disabled:bg-slate-800 text-white rounded-lg text-xs font-medium transition-colors shrink-0"
              >
                {isSearching ? <RotateCw className="w-4 h-4 animate-spin" /> : '检索'}
              </button>
            </div>

            <div className="flex items-center gap-1.5 flex-wrap">
              <span className="text-[11px] text-slate-500">测试短语：</span>
              {['报销发票凭据', '系统架构规范', '人事社保福利', '周会纪要与备忘'].map(phrase => (
                <button
                  key={phrase}
                  onClick={() => handleSearch(phrase)}
                  className="text-[10px] px-2 py-0.5 rounded bg-slate-800 hover:bg-slate-700 text-slate-400 hover:text-slate-200 font-mono transition-colors"
                >
                  {phrase}
                </button>
              ))}
            </div>
          </div>

          {searchError && (
            <div className="p-2.5 bg-rose-500/10 border border-rose-500/20 rounded-lg text-rose-300 text-xs flex items-center gap-2">
              <AlertTriangle className="w-4 h-4 text-rose-400 shrink-0" />
              <span>{searchError}</span>
            </div>
          )}

          {/* 检索结果展示 */}
          <div className="space-y-2 pt-1">
            <span className="text-[11px] text-slate-400 block font-medium">
              命中结果 ({searchResults.length})
            </span>
            {searchResults.length > 0 ? (
              <div className="space-y-2 max-h-[420px] overflow-y-auto pr-1">
                {searchResults.map((item, idx) => {
                  const docId = item.fingerprint || item.id || `hit-${idx}`
                  const doc = documents.find(d => d.id === docId)
                  const title = item.title || doc?.title || docId
                  const snippet = item.snippet || doc?.searchableText || ''
                  const rrfFormatted =
                    typeof item.rrfScore === 'number' && !isNaN(item.rrfScore)
                      ? (item.rrfScore * 100).toFixed(2)
                      : '-'

                  return (
                    <div
                      key={docId}
                      className="p-3 bg-slate-950/70 rounded-lg border border-slate-800/80 space-y-1.5"
                    >
                      <div className="flex items-start justify-between gap-2">
                        <div className="flex items-center gap-1.5 min-w-0">
                          <span className="text-[10px] font-mono px-1.5 py-0.2 rounded bg-slate-800 text-slate-400">
                            #{idx + 1}
                          </span>
                          <span className="text-xs font-medium text-slate-200 truncate">
                            {title}
                          </span>
                        </div>
                        <span className="text-xs font-mono font-bold text-sky-400 shrink-0">
                          RRF: {rrfFormatted}
                        </span>
                      </div>

                      {snippet && (
                        <p className="text-[11px] text-slate-400 line-clamp-2 leading-relaxed">
                          {snippet}
                        </p>
                      )}

                      <div className="flex items-center gap-3 pt-1 text-[10px] font-mono text-slate-500 border-t border-slate-900">
                        <span>BM25: {item.bm25Rank ? `Rank #${item.bm25Rank}` : item.bm25Score !== undefined ? item.bm25Score.toFixed(3) : '-'}</span>
                        <span>Dense: {item.denseRank ? `Rank #${item.denseRank}` : item.denseScore !== undefined ? item.denseScore.toFixed(3) : '-'}</span>
                      </div>
                    </div>
                  )
                })}
              </div>
            ) : (
              <div className="p-8 text-center border border-dashed border-slate-800 rounded-lg text-xs text-slate-500">
                尚未执行检索，或请先点击上方“批量建立双轨混合索引”
              </div>
            )}
          </div>
        </div>

        {/* 右侧：HAC 约束层次聚类 (6 cols) */}
        <div className="lg:col-span-6 p-4 rounded-xl bg-slate-900/70 border border-slate-800 space-y-4">
          <div className="space-y-1">
            <h3 className="text-xs font-semibold text-slate-200 flex items-center gap-1.5">
              <FolderTree className="w-3.5 h-3.5 text-purple-400" />
              自然语言引导约束层次聚类 (HAC)
            </h3>
            <p className="text-[11px] text-slate-500">
              输入自然语言整理 Prompt，由 kodama 凝聚算法聚类生成建议的多级目录树方案
            </p>
          </div>

          <div className="space-y-3">
            <div>
              <label className="text-[11px] text-slate-400 block mb-1">
                整理引导 Prompt (自然语言意图)
              </label>
              <input
                type="text"
                value={clusterPrompt}
                onChange={e => setClusterPrompt(e.target.value)}
                placeholder="如：按财务资金、技术研发、人事行政三类整理"
                className="w-full bg-slate-950 border border-slate-800 rounded-lg px-3 py-2 text-xs text-slate-200 placeholder-slate-600 focus:outline-none focus:border-purple-500"
              />
            </div>

            <div className="grid grid-cols-2 gap-3">
              <div>
                <label className="text-[10px] text-slate-500 block mb-1">
                  距离阈值 (Threshold: {distanceThreshold})
                </label>
                <input
                  type="range"
                  min="0.1"
                  max="0.9"
                  step="0.05"
                  value={distanceThreshold}
                  onChange={e => setDistanceThreshold(Number(e.target.value))}
                  className="w-full accent-purple-500"
                />
              </div>
              <div>
                <label className="text-[10px] text-slate-500 block mb-1">
                  最大叶子大小 (Max Leaf: {maxLeafSize})
                </label>
                <input
                  type="number"
                  min="5"
                  max="200"
                  value={maxLeafSize}
                  onChange={e => setMaxLeafSize(Number(e.target.value))}
                  className="w-full bg-slate-950 border border-slate-800 rounded px-2 py-1 text-xs text-slate-300"
                />
              </div>
            </div>

            <button
              onClick={handleCluster}
              disabled={isClustering}
              className="w-full flex items-center justify-center gap-2 py-2 bg-purple-600 hover:bg-purple-500 disabled:bg-slate-800 text-white rounded-lg text-xs font-medium transition-colors shadow-lg shadow-purple-950/40"
            >
              {isClustering ? (
                <>
                  <RotateCw className="w-3.5 h-3.5 animate-spin" />
                  <span>正在执行层次聚类生成目录树...</span>
                </>
              ) : (
                <>
                  <Sparkles className="w-3.5 h-3.5" />
                  <span>生成建议目录树方案</span>
                </>
              )}
            </button>
          </div>

          {clusterError && (
            <div className="p-2.5 bg-rose-500/10 border border-rose-500/20 rounded-lg text-rose-300 text-xs flex items-center gap-2">
              <AlertTriangle className="w-4 h-4 text-rose-400 shrink-0" />
              <span>{clusterError}</span>
            </div>
          )}

          {/* 聚类目录树呈现 */}
          <div className="space-y-2 pt-1">
            <div className="flex items-center justify-between">
              <span className="text-[11px] text-slate-400 font-medium">
                建议目录树结构预览
              </span>
              {clusterTree && (
                <span className="text-[10px] font-mono text-purple-400">
                  共聚合 {clusterTree.clustersCount} 个建议分类，覆盖 {clusterTree.totalDocuments} 篇文档
                </span>
              )}
            </div>

            {clusterTree?.root ? (
              <div className="p-3 bg-slate-950/80 rounded-lg border border-slate-800/90 max-h-[420px] overflow-y-auto space-y-1">
                {renderTreeNode(clusterTree.root)}
              </div>
            ) : (
              <div className="p-8 text-center border border-dashed border-slate-800 rounded-lg text-xs text-slate-500">
                点击上方“生成建议目录树方案”查看 AI 层次聚类效果
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}
