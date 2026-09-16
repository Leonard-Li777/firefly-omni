import React, { useState } from 'react'
import {
  Tag,
  GitBranch,
  Sparkles,
  ShieldCheck,
  FolderTree,
  RotateCw,
  Copy,
  Check,
  CheckCircle2,
  AlertTriangle,
  Layers,
  Database,
  Zap
} from 'lucide-react'

export interface MaterializedPathItem {
  code_path: string
  name_path: string
}

export interface ResolveParentResponse {
  success: boolean
  parent_code: string
  parent_name: string
  confidence: number
  suggested_depth: number
  materialized_paths: MaterializedPathItem[]
}

interface PresetTestCase {
  label: string
  badge: string
  badgeColor: string
  tagName: string
  contextHint: string
  expectedParent: string
}

const PRESET_TEST_CASES: PresetTestCase[] = [
  {
    label: '人工智能 / 深度学习',
    badge: 'AI & Tech',
    badgeColor: 'bg-indigo-500/10 text-indigo-400 border-indigo-500/30',
    tagName: '深度强化学习',
    contextHint: '计算机前沿算法与大模型神经网络训练文档',
    expectedParent: 'tech.ai (人工智能)'
  },
  {
    label: '财务审计与报表',
    badge: 'Finance',
    badgeColor: 'bg-emerald-500/10 text-emerald-400 border-emerald-500/30',
    tagName: '资产负债与收支报表',
    contextHint: '企业年度决算审计与利润核算总结',
    expectedParent: 'finance.accounting (财务会计)'
  },
  {
    label: '税务申报与发票',
    badge: 'Tax',
    badgeColor: 'bg-amber-500/10 text-amber-400 border-amber-500/30',
    tagName: '增值税进项发票',
    contextHint: '电子完税证明与财务抵扣单据',
    expectedParent: 'finance.tax (税务发票)'
  },
  {
    label: '后端架构与微服务',
    badge: 'Software',
    badgeColor: 'bg-sky-500/10 text-sky-400 border-sky-500/30',
    tagName: '分布式微服务网关',
    contextHint: 'Java Spring Boot 后端 API 接口与负载均衡架构',
    expectedParent: 'tech.software (软件开发)'
  },
  {
    label: '法律合同与用工协议',
    badge: 'Legal',
    badgeColor: 'bg-purple-500/10 text-purple-400 border-purple-500/30',
    tagName: '竞业限制保密合同',
    contextHint: '企业劳动雇佣法务合规与知识产权保护协议',
    expectedParent: 'legal.contract (法律合同)'
  },
  {
    label: 'UI 界面与设计稿',
    badge: 'Design',
    badgeColor: 'bg-rose-500/10 text-rose-400 border-rose-500/30',
    tagName: '移动端交互高保真原型',
    contextHint: 'Figma 界面设计稿组件库与设计规范文档',
    expectedParent: 'design.ui (界面设计)'
  },
  {
    label: '冷门未知字符 (安全降级)',
    badge: 'Fallback',
    badgeColor: 'bg-slate-500/10 text-slate-400 border-slate-500/30',
    tagName: 'xyz987未知冷门编码',
    contextHint: '',
    expectedParent: 'dim.topic (主题内容 - 安全兜底)'
  }
]

// 预编译受控标签分类树概览（用于右侧底座库速览）
const CONTROLLED_TAXONOMY_TREE = [
  {
    code: 'dim.technology',
    name: '技术维度',
    children: [
      { code: 'tech.ai', name: '人工智能', desc: '机器学习、深度学习、大模型、计算机视觉' },
      { code: 'tech.software', name: '软件开发', desc: '后端API、前端组件、微服务架构、源码实现' },
      { code: 'tech.database', name: '数据库', desc: 'SQL查询、关系型数据库、NoSQL、表结构设计' },
      { code: 'tech.network', name: '网络通信', desc: '网络协议、HTTP通信、TCP套接字、网关安全' },
    ]
  },
  {
    code: 'dim.finance',
    name: '财务维度',
    children: [
      { code: 'finance.accounting', name: '财务会计', desc: '资产负债表、利润核算、报销凭证、审计总结' },
      { code: 'finance.tax', name: '税务发票', desc: '增值税发票、税务申报、进销项抵扣、完税证明' },
    ]
  },
  {
    code: 'dim.legal',
    name: '法律合规',
    children: [
      { code: 'legal.contract', name: '法律合同', desc: '买卖合同、用工协议、保密竞业、授权委托书' },
      { code: 'legal.compliance', name: '合规监管', desc: '知识产权、资质审查、监管审计、行业标准' },
    ]
  },
  {
    code: 'dim.design',
    name: '视觉设计',
    children: [
      { code: 'design.ui', name: '界面设计', desc: 'UI原型、线框图、Sketch/Figma组件、交互设计' },
      { code: 'design.graphic', name: '平面视觉', desc: '海报宣传册、品牌VI、矢量插画、包装设计' },
    ]
  },
  {
    code: 'dim.office',
    name: '行政办公',
    children: [
      { code: 'office.doc', name: '办公文档', desc: '总结报告、会议纪要、工作计划、请批函件' },
      { code: 'office.hr', name: '人事行政', desc: '员工档案、招聘考核、离入职手续、考勤表' },
    ]
  },
  {
    code: 'dim.topic',
    name: '通用主题 (兜底根节点)',
    children: [
      { code: 'dim.content', name: '内容标签', desc: '通用文档与多媒体内容泛分类' },
      { code: 'dim.topic', name: '主题内容', desc: '未明确归类的自然语言主题聚合' },
    ]
  }
]

export const TaxonomyTab: React.FC = () => {
  const [tagName, setTagName] = useState<string>(PRESET_TEST_CASES[0].tagName)
  const [contextHint, setContextHint] = useState<string>(PRESET_TEST_CASES[0].contextHint)
  const [language, setLanguage] = useState<string>('zh-CN')
  const [isLoading, setIsLoading] = useState<boolean>(false)
  const [result, setResult] = useState<ResolveParentResponse | null>(null)
  const [errorMsg, setErrorMsg] = useState<string | null>(null)
  const [copied, setCopied] = useState<boolean>(false)

  const handleApplyPreset = (preset: PresetTestCase) => {
    setTagName(preset.tagName)
    setContextHint(preset.contextHint)
    setResult(null)
    setErrorMsg(null)
  }

  const handleResolveParent = async () => {
    if (!tagName.trim()) {
      setErrorMsg('请输入待测标签名！')
      return
    }

    setIsLoading(true)
    setErrorMsg(null)
    try {
      const res = await fetch('/api/taxonomy/resolve-parent', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          tag_name: tagName.trim(),
          language: language || 'zh-CN',
          context_hint: contextHint.trim() ? contextHint.trim() : undefined
        })
      })

      if (!res.ok) {
        throw new Error(`HTTP ${res.status}: ${res.statusText}`)
      }

      const data: ResolveParentResponse = await res.json()
      setResult(data)
    } catch (err: any) {
      setErrorMsg(err.message || '调用 /api/taxonomy/resolve-parent 失败')
    } finally {
      setIsLoading(false)
    }
  }

  const handleCopyJson = () => {
    if (!result) return
    navigator.clipboard.writeText(JSON.stringify(result, null, 2))
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }

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
                常驻受控标签向量底座与父级求解工作台
              </h1>
              <span className="px-2.5 py-0.5 rounded-full text-xs font-mono font-medium bg-indigo-500/20 text-indigo-300 border border-indigo-500/40">
                PRD §4.3 & Task 626
              </span>
            </div>
            <p className="text-xs text-slate-400 max-w-3xl leading-relaxed">
              基于 <code className="text-indigo-300 font-mono">bekko-a8m</code> 384 维稠密特征向量底座与预编译受控分类知识库（<code className="text-amber-300 font-mono">tags_text_embeddings.bin</code>），为动态提取的新标签自动推导科学父级分类、推荐树层级与双存物化路径（<code className="text-emerald-300 font-mono">code_path</code> & <code className="text-emerald-300 font-mono">name_path</code>），置信度低于 0.65 时智能安全降级回退至根节点。
            </p>
          </div>

          <div className="flex items-center space-x-3 flex-shrink-0">
            <div className="text-right hidden sm:block">
              <div className="text-[11px] text-slate-400">底座常驻维度</div>
              <div className="text-sm font-bold font-mono text-indigo-300">384 维稠密点积</div>
            </div>
            <div className="h-8 w-px bg-slate-800" />
            <div className="text-right hidden sm:block">
              <div className="text-[11px] text-slate-400">平均求解时延</div>
              <div className="text-sm font-bold font-mono text-emerald-400">&lt; 0.5 ms (CPU)</div>
            </div>
          </div>
        </div>
      </div>

      {/* 主体工作区：左侧测试输入与结果，右侧受控知识库速览与三阶段流程 */}
      <div className="grid grid-cols-1 lg:grid-cols-12 gap-6 flex-1 min-h-0">
        {/* 左侧：8 列交互控制面板 */}
        <div className="lg:col-span-7 flex flex-col space-y-6">
          {/* 快速预设用例面板 */}
          <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-5">
            <div className="flex items-center justify-between mb-3">
              <span className="text-xs font-bold text-slate-300 flex items-center">
                <Sparkles className="w-4 h-4 mr-1.5 text-amber-400" />
                标准测试用例集 (覆盖各大主线与容错降级)
              </span>
              <span className="text-[11px] text-slate-500">点击自动填充参数</span>
            </div>
            <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 gap-2">
              {PRESET_TEST_CASES.map((preset, idx) => (
                <button
                  key={idx}
                  onClick={() => handleApplyPreset(preset)}
                  className={`p-2.5 rounded-xl border text-left transition-all hover:scale-[1.02] flex flex-col justify-between ${
                    tagName === preset.tagName
                      ? 'bg-indigo-950/60 border-indigo-500/80 shadow-md shadow-indigo-950/50'
                      : 'bg-slate-950/60 border-slate-800 hover:border-slate-700'
                  }`}
                >
                  <span className={`text-[10px] px-1.5 py-0.5 rounded border self-start mb-1.5 font-medium ${preset.badgeColor}`}>
                    {preset.badge}
                  </span>
                  <div className="text-xs font-semibold text-slate-200 truncate" title={preset.tagName}>
                    {preset.tagName}
                  </div>
                  <div className="text-[10px] text-slate-500 mt-1 truncate" title={preset.expectedParent}>
                    → {preset.expectedParent.split(' ')[0]}
                  </div>
                </button>
              ))}
            </div>
          </div>

          {/* 参数输入表单 */}
          <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-5 space-y-4">
            <div className="flex items-center justify-between border-b border-slate-800 pb-3">
              <span className="text-xs font-bold text-slate-200 flex items-center">
                <Tag className="w-4 h-4 mr-1.5 text-indigo-400" />
                POST /api/taxonomy/resolve-parent 参数配置
              </span>
              <span className="text-[11px] font-mono text-slate-500">HTTP REST Payload</span>
            </div>

            <div className="space-y-3 text-xs">
              <div>
                <label className="block text-slate-300 font-semibold mb-1">
                  待测标签名 (tag_name) <span className="text-rose-400">*</span>
                </label>
                <input
                  type="text"
                  value={tagName}
                  onChange={(e) => setTagName(e.target.value)}
                  placeholder="如：深度强化学习、资产负债表、UI界面设计稿..."
                  className="w-full bg-slate-950/80 border border-slate-800 focus:border-indigo-500 rounded-xl px-3.5 py-2.5 text-slate-200 focus:outline-none focus:ring-1 focus:ring-indigo-500 text-xs transition-all font-mono"
                />
              </div>

              <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
                <div className="sm:col-span-2">
                  <label className="block text-slate-300 font-semibold mb-1">
                    上下文线索 (context_hint) <span className="text-slate-500 font-normal">(可选，辅助稠密向量消歧)</span>
                  </label>
                  <input
                    type="text"
                    value={contextHint}
                    onChange={(e) => setContextHint(e.target.value)}
                    placeholder="如：计算机深度学习论文、企业财务决算审计..."
                    className="w-full bg-slate-950/80 border border-slate-800 focus:border-indigo-500 rounded-xl px-3.5 py-2 text-slate-200 focus:outline-none focus:ring-1 focus:ring-indigo-500 text-xs transition-all"
                  />
                </div>
                <div>
                  <label className="block text-slate-300 font-semibold mb-1">
                    语言 (language)
                  </label>
                  <select
                    value={language}
                    onChange={(e) => setLanguage(e.target.value)}
                    className="w-full bg-slate-950/80 border border-slate-800 focus:border-indigo-500 rounded-xl px-3 py-2 text-slate-200 focus:outline-none text-xs"
                  >
                    <option value="zh-CN">中文 (zh-CN)</option>
                    <option value="en-US">English (en-US)</option>
                    <option value="ja-JP">日本語 (ja-JP)</option>
                    <option value="ko-KR">한국어 (ko-KR)</option>
                    <option value="fr-FR">Français (fr-FR)</option>
                    <option value="de-DE">Deutsch (de-DE)</option>
                  </select>
                </div>
              </div>
            </div>

            {errorMsg && (
              <div className="p-3 rounded-xl bg-rose-500/10 border border-rose-500/30 text-rose-300 text-xs flex items-center">
                <AlertTriangle className="w-4 h-4 mr-2 flex-shrink-0 text-rose-400" />
                {errorMsg}
              </div>
            )}

            <button
              onClick={handleResolveParent}
              disabled={isLoading || !tagName.trim()}
              className="w-full py-2.5 px-4 rounded-xl bg-indigo-600 hover:bg-indigo-500 active:bg-indigo-700 disabled:opacity-50 text-white font-semibold text-xs flex items-center justify-center space-x-2 transition-all shadow-lg shadow-indigo-600/30"
            >
              {isLoading ? (
                <>
                  <RotateCw className="w-4 h-4 animate-spin" />
                  <span>向量底座毫秒级点积求解中...</span>
                </>
              ) : (
                <>
                  <Zap className="w-4 h-4" />
                  <span>执行语义父级求解 (POST /api/taxonomy/resolve-parent)</span>
                </>
              )}
            </button>
          </div>

          {/* 求解结果展示卡片 */}
          {result && (
            <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-5 space-y-4 transition-all animate-fadeIn">
              <div className="flex items-center justify-between border-b border-slate-800 pb-3">
                <div className="flex items-center space-x-2">
                  <span className={`p-1.5 rounded-lg ${
                    result.confidence >= 0.65 ? 'bg-emerald-500/20 text-emerald-400' : 'bg-amber-500/20 text-amber-400'
                  }`}>
                    {result.confidence >= 0.65 ? <CheckCircle2 className="w-4 h-4" /> : <ShieldCheck className="w-4 h-4" />}
                  </span>
                  <div>
                    <h3 className="font-bold text-sm text-slate-100 flex items-center gap-2">
                      <span>父级求解完成: {result.parent_name}</span>
                      <code className="text-xs px-2 py-0.5 rounded bg-slate-800 font-mono text-indigo-300 font-normal">
                        {result.parent_code}
                      </code>
                    </h3>
                  </div>
                </div>

                <button
                  onClick={handleCopyJson}
                  className="p-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 text-slate-300 border border-slate-700 transition-all flex items-center space-x-1 text-xs"
                  title="复制结果 JSON"
                >
                  {copied ? <Check className="w-3.5 h-3.5 text-emerald-400" /> : <Copy className="w-3.5 h-3.5" />}
                  <span className="text-[11px]">{copied ? '已复制' : '复制 JSON'}</span>
                </button>
              </div>

              {/* 核心指标 3 列卡片 */}
              <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
                <div className="bg-slate-950/70 border border-slate-800/80 rounded-xl p-3">
                  <span className="text-[11px] text-slate-400">余弦置信度 (Confidence)</span>
                  <div className="flex items-baseline space-x-2 mt-1">
                    <span className={`text-xl font-bold font-mono ${
                      result.confidence >= 0.85
                        ? 'text-emerald-400'
                        : result.confidence >= 0.65
                        ? 'text-indigo-400'
                        : 'text-amber-400'
                    }`}>
                      {(result.confidence * 100).toFixed(1)}%
                    </span>
                    <span className="text-[10px] text-slate-500">
                      {result.confidence >= 0.65 ? '(>=0.65 命中)' : '(<0.65 降级)'}
                    </span>
                  </div>
                  {/* 进度条 */}
                  <div className="w-full bg-slate-800 h-1.5 rounded-full mt-2 overflow-hidden">
                    <div
                      className={`h-full rounded-full ${
                        result.confidence >= 0.85
                          ? 'bg-emerald-500'
                          : result.confidence >= 0.65
                          ? 'bg-indigo-500'
                          : 'bg-amber-500'
                      }`}
                      style={{ width: `${Math.min(100, Math.max(10, result.confidence * 100))}%` }}
                    />
                  </div>
                </div>

                <div className="bg-slate-950/70 border border-slate-800/80 rounded-xl p-3">
                  <span className="text-[11px] text-slate-400">推荐挂载深度 (Depth)</span>
                  <div className="flex items-baseline space-x-2 mt-1">
                    <span className="text-xl font-bold font-mono text-purple-400">
                      Level {result.suggested_depth}
                    </span>
                    <span className="text-[10px] text-slate-500">层级下钻</span>
                  </div>
                  <p className="text-[10px] text-slate-500 mt-2">继承父节点物化路径</p>
                </div>

                <div className="bg-slate-950/70 border border-slate-800/80 rounded-xl p-3">
                  <span className="text-[11px] text-slate-400">状态判定</span>
                  <div className="mt-1">
                    {result.confidence >= 0.65 ? (
                      <span className="px-2 py-1 rounded text-xs font-semibold bg-emerald-500/20 text-emerald-300 border border-emerald-500/30 inline-flex items-center">
                        <CheckCircle2 className="w-3.5 h-3.5 mr-1" />
                        高置信度树节点推荐
                      </span>
                    ) : (
                      <span className="px-2 py-1 rounded text-xs font-semibold bg-amber-500/20 text-amber-300 border border-amber-500/30 inline-flex items-center">
                        <ShieldCheck className="w-3.5 h-3.5 mr-1" />
                        安全回退至通用语义
                      </span>
                    )}
                  </div>
                  <p className="text-[10px] text-slate-500 mt-2">符合 ADR 0034 规范</p>
                </div>
              </div>

              {/* 物化路径展示 (双存物化路径) */}
              <div className="bg-slate-950/70 border border-slate-800/80 rounded-xl p-4 space-y-2.5">
                <span className="text-xs font-bold text-slate-300 flex items-center">
                  <FolderTree className="w-4 h-4 mr-1.5 text-emerald-400" />
                  双存物化路径 (Materialized Paths)
                </span>
                {result.materialized_paths && result.materialized_paths.length > 0 ? (
                  result.materialized_paths.map((p, idx) => (
                    <div key={idx} className="space-y-1.5 font-mono text-xs bg-slate-900/80 p-2.5 rounded-lg border border-slate-800">
                      <div className="flex items-center space-x-2">
                        <span className="text-slate-500 text-[10px] w-20 flex-shrink-0">code_path:</span>
                        <span className="text-indigo-300 truncate">{p.code_path}</span>
                      </div>
                      <div className="flex items-center space-x-2">
                        <span className="text-slate-500 text-[10px] w-20 flex-shrink-0">name_path:</span>
                        <span className="text-emerald-300 truncate">{p.name_path}</span>
                      </div>
                    </div>
                  ))
                ) : (
                  <div className="text-xs text-slate-500">无物化路径</div>
                )}
              </div>
            </div>
          )}
        </div>

        {/* 右侧：5 列受控分类底座知识库速览与三阶段全模态架构图 */}
        <div className="lg:col-span-5 flex flex-col space-y-6">
          {/* 三阶段全模态异构融合感知机制展示 */}
          <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-5 space-y-4">
            <div className="flex items-center space-x-2 border-b border-slate-800 pb-3">
              <span className="p-1.5 rounded-lg bg-amber-500/20 text-amber-400">
                <Layers className="w-4 h-4" />
              </span>
              <div>
                <h3 className="text-xs font-bold text-slate-200">三阶段全模态融合管道 (Task 626)</h3>
                <p className="text-[10px] text-slate-500">纯 CPU 离线架构，无大模型幻觉</p>
              </div>
            </div>

            <div className="space-y-3 text-xs">
              <div className="bg-slate-950/60 border border-slate-800 p-3 rounded-xl">
                <div className="font-bold text-amber-400 flex items-center mb-1">
                  <span className="w-4 h-4 rounded-full bg-amber-500/20 flex items-center justify-center mr-1.5 text-[10px]">1</span>
                  Phase 1: Magika 类型精准识别
                </div>
                <p className="text-slate-400 text-[11px]">
                  基于神经网络极速判别物理 MIME 类型与文件分类大组，不受伪造后缀影响。
                </p>
              </div>

              <div className="bg-slate-950/60 border border-slate-800 p-3 rounded-xl">
                <div className="font-bold text-sky-400 flex items-center mb-1">
                  <span className="w-4 h-4 rounded-full bg-sky-500/20 flex items-center justify-center mr-1.5 text-[10px]">2</span>
                  Phase 2: 并行特征提取与强类型聚合
                </div>
                <p className="text-slate-400 text-[11px]">
                  并发提取 Markdown 正文、OCR 连通域文本、视觉多模型标签与 ExifTool 元数据，组装为 <code className="text-sky-300 font-mono">MultimodalContext</code>。
                </p>
              </div>

              <div className="bg-slate-950/60 border border-slate-800 p-3 rounded-xl">
                <div className="font-bold text-emerald-400 flex items-center mb-1">
                  <span className="w-4 h-4 rounded-full bg-emerald-500/20 flex items-center justify-center mr-1.5 text-[10px]">3</span>
                  Phase 3: 双锚点几何打分与融合裁决
                </div>
                <p className="text-slate-400 text-[11px] leading-relaxed">
                  1. <strong className="text-slate-200">SlotEngine</strong> 矩阵生成正交候选短语；<br />
                  2. <strong className="text-slate-200">E_global ⊗ E_desc</strong> 双锚点余弦交叉打分；<br />
                  3. 跨模态互证奖励（<span className="text-amber-300 font-semibold">+0.20</span>）；<br />
                  4. 单选互斥裁剪与 MMR 去重，产出终极完美标签集 <code className="text-emerald-300 font-mono">fused_tags</code>。
                </p>
              </div>
            </div>
          </div>

          {/* 常驻受控分类树图谱 */}
          <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-5 flex flex-col flex-1 min-h-0">
            <div className="flex items-center justify-between border-b border-slate-800 pb-3 mb-3">
              <span className="text-xs font-bold text-slate-200 flex items-center">
                <Database className="w-4 h-4 mr-1.5 text-indigo-400" />
                常驻受控标签知识图谱 (Taxonomy Base)
              </span>
              <span className="text-[10px] font-mono text-slate-500">tags_text_embeddings.bin</span>
            </div>

            <div className="space-y-3 overflow-y-auto flex-1 min-h-0 pr-1 text-xs">
              {CONTROLLED_TAXONOMY_TREE.map((cat, idx) => (
                <div key={idx} className="bg-slate-950/50 border border-slate-800/80 rounded-xl p-3">
                  <div className="flex items-center justify-between font-semibold text-indigo-300 mb-2">
                    <span className="flex items-center">
                      <FolderTree className="w-3.5 h-3.5 mr-1 text-indigo-400" />
                      {cat.name}
                    </span>
                    <code className="text-[10px] text-slate-500 font-mono">{cat.code}</code>
                  </div>
                  <div className="space-y-1.5 pl-3 border-l border-slate-800">
                    {cat.children.map((child, cIdx) => (
                      <div key={cIdx} className="bg-slate-900/60 p-1.5 rounded border border-slate-800/60 text-[11px] flex justify-between items-center">
                        <div>
                          <span className="font-medium text-slate-200">{child.name}</span>
                          <span className="text-slate-500 ml-1.5 text-[10px] hidden sm:inline">({child.desc})</span>
                        </div>
                        <code className="text-[10px] text-slate-400 font-mono flex-shrink-0 ml-2">{child.code}</code>
                      </div>
                    ))}
                  </div>
                </div>
              ))}
            </div>
          </div>
        </div>
      </div>
    </div>
  )
}
