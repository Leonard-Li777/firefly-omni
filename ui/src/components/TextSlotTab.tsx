import React, { useState } from 'react'
import {
  FileText,
  Sparkles,
  Copy,
  Check,
  Tag,
  Clock,
  Layers,
  CheckCircle2,
  AlertTriangle,
  RotateCw,
  Hash,
  FileSignature
} from 'lucide-react'

export interface TextKeyword {
  word: string
  score?: number | null
}

export interface ExtractedEntity {
  category: string
  value: string
  confidence?: number
}

export interface StructuredSummary {
  who?: string
  what?: string
  when?: string
  where?: string
  where_loc?: string
  why?: string
  how?: string
  key_points?: string[]
}

export interface TextAnalysisResult {
  title?: string | null
  language: string
  keywords: Array<string | TextKeyword>
  entities: ExtractedEntity[]
  structured_summary: StructuredSummary
  one_sentence_desc?: string | null
  smart_name?: string | null
  name_slots: Record<string, string>
  embedding_dense: number[]
  chunks?: any[]
  duration_ms: number
}

interface PresetSample {
  name: string
  badge: string
  fileName: string
  text: string
}

const PRESET_SAMPLES: PresetSample[] = [
  {
    name: '财务报表',
    badge: 'Finance',
    fileName: '北京华润科技_2025年度财务审计报告_最终版.pdf',
    text: `根据《北京华润科技有限公司2025年度财务收支审计报告》，截至2025年12月31日，公司年度总营收达到人民币 128,500,000.00 元（大写：壹亿贰仟捌佰伍拾万元整）。本次专项审计由立信会计师事务所执行，审计基准日为2025-12-31，出具标准无保留审计意见。涉及应收账款总额 12,300,500.00 元，研发投入累计支出 34,200,000.00 元，净利润较上一年度增长 18.5%。`
  },
  {
    name: '劳动合同',
    badge: 'Legal',
    fileName: '智源未来_劳动合同_张子涵_2025.docx',
    text: `劳动用工合同书。甲方（用人单位）：上海智源未来信息科技有限公司，统一社会信用代码：91310115MA1K45678X；乙方（劳动者）：张子涵，身份证号码：310104199508182312。双方经友好协商，签订本全日制劳动合同。乙方受聘岗位为高级前端架构师，试用期三个月（2025年03月01日至2025年05月31日），基本月薪为人民币 35,000.00 元整。合同履行地为上海市浦东新区张江高科技园区。`
  },
  {
    name: '技术架构设计',
    badge: 'Tech',
    fileName: 'OmniServer_架构设计规范_v2.1.0.md',
    text: `Firefly Omni-Server 离线全模态特征感知引擎技术架构设计规范书 (版本: v2.1.0)。本规范由核心系统架构师李维于2026年3月编写。系统采用纯 CPU 架构运行，整合 fastText 语种分类、KeyBERT 主题无幻觉抽取与 bekko-a8m 384 维密集特征向量模型，单文件处理 SLO 控制在 15ms 以内，基准常驻内存控制在 630MB 阈值之内。`
  },
  {
    name: '随记备忘',
    badge: 'Daily',
    fileName: '周会纪要_20260410_备忘.txt',
    text: `2026年4月10日跨团队周会备忘纪要。参会人：产品总监王敏、算法负责人周波、桌面客户端组长陈浩。核心决议：1. 下周二前完成桌面端单点标签持久化 TagReconciliationArbiter 胶水收拢；2. 虚拟目录一键聚类弹窗默认最大深度限制为 3 层；3. 冷门端口 38200 顺延探测日志需要统一对接 Winston 格式。`
  }
]

export const TextSlotTab: React.FC = () => {
  const [selectedPreset, setSelectedPreset] = useState<number>(0)
  const [inputText, setInputText] = useState<string>(PRESET_SAMPLES[0].text)
  const [inputFileName, setInputFileName] = useState<string>(PRESET_SAMPLES[0].fileName)
  const [isAnalyzing, setIsAnalyzing] = useState<boolean>(false)
  const [result, setResult] = useState<TextAnalysisResult | null>(null)
  const [errorMsg, setErrorMsg] = useState<string | null>(null)
  const [isCopied, setIsCopied] = useState<boolean>(false)

  const handleSelectPreset = (index: number) => {
    setSelectedPreset(index)
    setInputText(PRESET_SAMPLES[index].text)
    setInputFileName(PRESET_SAMPLES[index].fileName)
    setResult(null)
    setErrorMsg(null)
  }

  const handleAnalyze = async () => {
    if (!inputText.trim()) {
      setErrorMsg('请输入待分析的文本内容')
      return
    }
    setIsAnalyzing(true)
    setErrorMsg(null)
    try {
      const res = await fetch('/api/text/analyze', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          text: inputText,
          fileName: inputFileName || undefined,
          mtime: new Date().toISOString()
        })
      })
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}: ${res.statusText}`)
      }
      const data: TextAnalysisResult = await res.json()
      setResult(data)
    } catch (err: any) {
      setErrorMsg(err.message || '文本分析请求失败，请确保 omni-server 已启动')
    } finally {
      setIsAnalyzing(false)
    }
  }

  const handleCopySmartName = () => {
    if (!result?.smart_name) return
    navigator.clipboard.writeText(result.smart_name)
    setIsCopied(true)
    setTimeout(() => setIsCopied(false), 2000)
  }

  const hasNanOrInf = result?.embedding_dense?.some(v => isNaN(v) || !isFinite(v)) ?? false
  const vectorDim = result?.embedding_dense?.length ?? 0

  return (
    <div className="space-y-6">
      {/* 顶部标题与说明 */}
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between pb-4 border-b border-slate-800 gap-2">
        <div>
          <h2 className="text-lg font-semibold text-slate-100 flex items-center gap-2">
            <Sparkles className="w-5 h-5 text-sky-400" />
            Text & Slot 文本特征与确定性智能重命名
          </h2>
          <p className="text-xs text-slate-400 mt-0.5">
            基于 fastText 语种探测、KeyBERT 主题抽取、正则槽位引擎与 384d 嵌入向量，实现毫秒级纯 CPU 离线特征提取
          </p>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs text-slate-400">端点：</span>
          <code className="text-xs bg-slate-800 px-2 py-1 rounded text-sky-300 font-mono">
            POST /api/text/analyze
          </code>
        </div>
      </div>

      {/* 预置样例快捷选择器 */}
      <div>
        <label className="text-xs font-medium text-slate-400 uppercase tracking-wider block mb-2">
          预置典型样例选择
        </label>
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-2">
          {PRESET_SAMPLES.map((sample, idx) => (
            <button
              key={sample.name}
              onClick={() => handleSelectPreset(idx)}
              className={`flex items-center justify-between p-2.5 rounded-lg border text-left transition-all ${
                selectedPreset === idx
                  ? 'border-sky-500 bg-sky-500/10 text-sky-200'
                  : 'border-slate-800 bg-slate-900/60 text-slate-300 hover:border-slate-700 hover:bg-slate-800/50'
              }`}
            >
              <span className="text-xs font-medium truncate">{sample.name}</span>
              <span className="text-[10px] px-1.5 py-0.5 rounded bg-slate-800 text-slate-400 font-mono">
                {sample.badge}
              </span>
            </button>
          ))}
        </div>
      </div>

      {/* 输入区域 */}
      <div className="grid grid-cols-1 lg:grid-cols-12 gap-4">
        <div className="lg:col-span-8 space-y-2">
          <div className="flex items-center justify-between">
            <label className="text-xs font-medium text-slate-300 flex items-center gap-1.5">
              <FileText className="w-3.5 h-3.5 text-slate-400" />
              待分析文本内容 (Text Body)
            </label>
            <span className="text-[11px] text-slate-500 font-mono">{inputText.length} 字符</span>
          </div>
          <textarea
            value={inputText}
            onChange={e => setInputText(e.target.value)}
            rows={7}
            placeholder="粘贴待提取特征的任意文本内容..."
            className="w-full bg-slate-900 border border-slate-800 rounded-lg p-3 text-xs text-slate-200 placeholder-slate-600 focus:outline-none focus:border-sky-500 transition-colors font-sans resize-y"
          />
        </div>

        <div className="lg:col-span-4 space-y-3">
          <div>
            <label className="text-xs font-medium text-slate-300 block mb-1.5">
              关联文件名 (可选，辅助槽位识别)
            </label>
            <input
              type="text"
              value={inputFileName}
              onChange={e => setInputFileName(e.target.value)}
              placeholder="如：report_2025.docx"
              className="w-full bg-slate-900 border border-slate-800 rounded-lg px-3 py-2 text-xs text-slate-200 placeholder-slate-600 focus:outline-none focus:border-sky-500 transition-colors font-mono"
            />
          </div>

          <div className="pt-2">
            <button
              onClick={handleAnalyze}
              disabled={isAnalyzing}
              className="w-full flex items-center justify-center gap-2 px-4 py-2.5 bg-sky-600 hover:bg-sky-500 disabled:bg-slate-800 disabled:text-slate-600 text-white rounded-lg text-xs font-medium transition-colors shadow-lg shadow-sky-950/40"
            >
              {isAnalyzing ? (
                <>
                  <RotateCw className="w-4 h-4 animate-spin text-sky-200" />
                  <span>正在执行 Rust 离线分析...</span>
                </>
              ) : (
                <>
                  <Sparkles className="w-4 h-4 text-sky-200" />
                  <span>开始文本分析与槽位提取</span>
                </>
              )}
            </button>
          </div>

          {errorMsg && (
            <div className="p-3 bg-rose-500/10 border border-rose-500/20 rounded-lg text-rose-300 text-xs flex items-start gap-2">
              <AlertTriangle className="w-4 h-4 text-rose-400 shrink-0 mt-0.5" />
              <span>{errorMsg}</span>
            </div>
          )}
        </div>
      </div>

      {/* 分析结果面板 */}
      {result && (
        <div className="space-y-4 pt-2">
          {/* 智能重命名条幅 */}
          <div className="p-4 rounded-xl border border-emerald-500/30 bg-gradient-to-r from-emerald-950/40 via-emerald-900/20 to-slate-900/40 flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3 shadow-lg">
            <div className="space-y-1">
              <span className="text-[11px] font-semibold text-emerald-400 uppercase tracking-wider flex items-center gap-1.5">
                <FileSignature className="w-4 h-4 text-emerald-400" />
                推荐确定性重命名 (smart_name)
              </span>
              <div className="text-sm sm:text-base font-mono font-medium text-emerald-100 break-all">
                {result.smart_name || '未提取到充分槽位，保留原名'}
              </div>
            </div>
            {result.smart_name && (
              <button
                onClick={handleCopySmartName}
                className="self-start sm:self-auto flex items-center gap-1.5 px-3 py-1.5 bg-emerald-600 hover:bg-emerald-500 text-white rounded-lg text-xs font-medium transition-colors shrink-0"
              >
                {isCopied ? (
                  <>
                    <Check className="w-3.5 h-3.5" />
                    <span>已复制</span>
                  </>
                ) : (
                  <>
                    <Copy className="w-3.5 h-3.5" />
                    <span>复制文件名</span>
                  </>
                )}
              </button>
            )}
          </div>

          {/* 状态徽章条 */}
          <div className="flex flex-wrap items-center gap-2 text-xs">
            <div className="px-2.5 py-1 rounded bg-slate-800/80 border border-slate-700/60 text-slate-300 flex items-center gap-1.5">
              <span className="text-slate-500">检测语言:</span>
              <span className="font-mono text-sky-300 font-semibold uppercase">{result.language || '未知'}</span>
            </div>
            <div className="px-2.5 py-1 rounded bg-slate-800/80 border border-slate-700/60 text-slate-300 flex items-center gap-1.5">
              <Clock className="w-3.5 h-3.5 text-slate-400" />
              <span className="text-slate-500">分析耗时:</span>
              <span className="font-mono text-emerald-300">{result.duration_ms} ms</span>
            </div>
            <div className="px-2.5 py-1 rounded bg-slate-800/80 border border-slate-700/60 text-slate-300 flex items-center gap-1.5">
              <Layers className="w-3.5 h-3.5 text-slate-400" />
              <span className="text-slate-500">向量维度:</span>
              <span className="font-mono text-purple-300 font-medium">{vectorDim}d</span>
              {vectorDim === 384 && !hasNanOrInf ? (
                <CheckCircle2 className="w-3.5 h-3.5 text-emerald-400" />
              ) : (
                <AlertTriangle className="w-3.5 h-3.5 text-amber-400" />
              )}
            </div>
          </div>

          {/* 核心指标网格 */}
          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            {/* KeyBERT 关键词云 */}
            <div className="p-4 rounded-xl bg-slate-900/70 border border-slate-800 space-y-3">
              <h3 className="text-xs font-semibold text-slate-300 flex items-center gap-1.5">
                <Tag className="w-3.5 h-3.5 text-amber-400" />
                KeyBERT 主题词抽取
              </h3>
              {result.keywords && result.keywords.length > 0 ? (
                <div className="flex flex-wrap gap-2">
                  {result.keywords.map((kw, i) => {
                    const word = typeof kw === 'string' ? kw : kw?.word || String(kw)
                    const score =
                      typeof kw === 'object' && kw !== null && typeof kw.score === 'number'
                        ? kw.score
                        : null
                    return (
                      <span
                        key={i}
                        className="px-2.5 py-1 bg-amber-500/10 border border-amber-500/20 rounded-md text-xs text-amber-200 flex items-center gap-1.5"
                      >
                        <span className="font-medium">{word}</span>
                        {score !== null && !isNaN(score) && (
                          <span className="text-[10px] text-amber-400/70 font-mono font-semibold">
                            {(score * 100).toFixed(1)}%
                          </span>
                        )}
                      </span>
                    )
                  })}
                </div>
              ) : (
                <div className="text-xs text-slate-500 italic">未提取到关键词</div>
              )}
            </div>

            {/* 正则槽位提取卡片 */}
            <div className="p-4 rounded-xl bg-slate-900/70 border border-slate-800 space-y-3">
              <h3 className="text-xs font-semibold text-slate-300 flex items-center gap-1.5">
                <Hash className="w-3.5 h-3.5 text-cyan-400" />
                确定性正则槽位与实体 (name_slots & entities)
              </h3>
              {result.name_slots && Object.keys(result.name_slots).length > 0 ? (
                <div className="grid grid-cols-2 gap-2 text-xs">
                  {Object.entries(result.name_slots).map(([key, val]) => (
                    <div key={key} className="p-2 bg-slate-800/40 rounded border border-slate-800">
                      <span className="text-[11px] text-slate-400 block font-mono">{key}</span>
                      <span className="text-xs font-medium text-slate-200 truncate block">{val}</span>
                    </div>
                  ))}
                </div>
              ) : result.entities && result.entities.length > 0 ? (
                <div className="grid grid-cols-2 gap-2 text-xs">
                  {result.entities.map((ent, idx) => (
                    <div key={idx} className="p-2 bg-slate-800/40 rounded border border-slate-800">
                      <span className="text-[11px] text-cyan-400 block font-mono">{ent.category}</span>
                      <span className="text-xs font-medium text-slate-200 truncate block">{ent.value}</span>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="text-xs text-slate-500 italic">未识别到特定命名槽位</div>
              )}
            </div>
          </div>

          {/* 5W 结构化摘要表格 */}
          <div className="p-4 rounded-xl bg-slate-900/70 border border-slate-800 space-y-3">
            <h3 className="text-xs font-semibold text-slate-300 flex items-center gap-1.5">
              <FileText className="w-3.5 h-3.5 text-indigo-400" />
              5W 结构化要素分析 (structured_summary)
            </h3>
            <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-6 gap-2 text-xs">
              {[
                { label: 'Who (责任人/主体)', val: result.structured_summary?.who },
                { label: 'What (核心事项)', val: result.structured_summary?.what },
                { label: 'When (关键时间)', val: result.structured_summary?.when },
                { label: 'Where (地点/机构)', val: result.structured_summary?.where_loc || result.structured_summary?.where },
                { label: 'Why (背景/原因)', val: result.structured_summary?.why },
                { label: 'How (履行/金额)', val: result.structured_summary?.how }
              ].map(item => (
                <div key={item.label} className="p-2.5 bg-slate-800/50 rounded-lg border border-slate-800/80">
                  <span className="text-[10px] text-slate-400 font-medium block truncate mb-1">{item.label}</span>
                  <span className="text-xs text-slate-200 block truncate" title={item.val || '-'}>
                    {item.val || <span className="text-slate-600">-</span>}
                  </span>
                </div>
              ))}
            </div>
          </div>

          {/* 384 维密集向量前瞻条 */}
          <div className="p-4 rounded-xl bg-slate-900/70 border border-slate-800 space-y-2.5">
            <div className="flex items-center justify-between">
              <h3 className="text-xs font-semibold text-slate-300 flex items-center gap-1.5">
                <Layers className="w-3.5 h-3.5 text-purple-400" />
                384 维密集特征向量切片前瞻 (bekko-a8m)
              </h3>
              <div className="flex items-center gap-2 text-[11px] font-mono">
                <span className={vectorDim === 384 ? 'text-emerald-400' : 'text-rose-400'}>
                  维度: {vectorDim} / 384
                </span>
                <span className="text-slate-600">|</span>
                <span className={!hasNanOrInf ? 'text-emerald-400' : 'text-rose-400'}>
                  NaN/Inf: {!hasNanOrInf ? '无' : '存在异常值'}
                </span>
              </div>
            </div>
            {result.embedding_dense && result.embedding_dense.length > 0 ? (
              <div className="p-2 bg-slate-950/80 rounded border border-slate-800/80 font-mono text-[11px] text-purple-300 break-all leading-relaxed">
                [{result.embedding_dense.slice(0, 8).map(v => v.toFixed(6)).join(', ')}, ... 共{' '}
                {result.embedding_dense.length} 维]
              </div>
            ) : (
              <div className="text-xs text-slate-500 italic">无向量输出</div>
            )}
          </div>
        </div>
      )}
    </div>
  )
}
