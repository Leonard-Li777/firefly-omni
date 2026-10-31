import React, { useState } from 'react'
import {
  BookOpen,
  Search,
  Sparkles,
  GitFork,
  ArrowRightLeft,
  CheckCircle2,
  XCircle,
  AlertTriangle,
  RotateCw,
  Tag,
  Share2,
  Quote
} from 'lucide-react'

export interface HowNetSlot {
  role: string
  filler: string
}

export interface HowNetDescribeResult {
  word: string
  found: boolean
  is_aligned: boolean
  alignment_level?: string | null
  top_concept?: string | null
  slots: HowNetSlot[]
  synonyms: string[]
  antonyms: string[]
  description: string
}

const POPULAR_WORDS = ['借款', '买', '飞机', '救护车', '发票', '医院']

export const HowNetTab: React.FC = () => {
  const [searchWord, setSearchWord] = useState<string>('借款')
  const [isSearching, setIsSearching] = useState<boolean>(false)
  const [result, setResult] = useState<HowNetDescribeResult | null>(null)
  const [errorMsg, setErrorMsg] = useState<string | null>(null)

  const handleSearch = async (wordToQuery?: string) => {
    const word = (wordToQuery || searchWord).trim()
    if (!word) {
      setErrorMsg('请输入待查询的词汇')
      return
    }
    setSearchWord(word)
    setIsSearching(true)
    setErrorMsg(null)
    try {
      const res = await fetch('/api/hownet/describe', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ word })
      })
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}: ${res.statusText}`)
      }
      const data: HowNetDescribeResult = await res.json()
      setResult(data)
    } catch (err: any) {
      setErrorMsg(err.message || 'HowNet 义原查询请求失败，请确保 omni-server 已启动')
    } finally {
      setIsSearching(false)
    }
  }

  const getAlignmentBadge = () => {
    if (!result?.is_aligned) {
      return (
        <span className="px-2 py-0.5 rounded bg-slate-800 border border-slate-700 text-slate-400 text-xs font-mono flex items-center gap-1">
          <XCircle className="w-3 h-3 text-slate-500" />
          未对齐
        </span>
      )
    }
    const level = result.alignment_level?.toUpperCase() || 'ALIGNED'
    let colorClasses = 'bg-sky-500/10 border-sky-500/30 text-sky-300'
    if (level === 'L1') colorClasses = 'bg-emerald-500/10 border-emerald-500/30 text-emerald-300'
    if (level === 'L2') colorClasses = 'bg-cyan-500/10 border-cyan-500/30 text-cyan-300'
    if (level === 'L3') colorClasses = 'bg-purple-500/10 border-purple-500/30 text-purple-300'

    return (
      <span className={`px-2.5 py-1 rounded-md border text-xs font-mono font-semibold flex items-center gap-1.5 ${colorClasses}`}>
        <CheckCircle2 className="w-3.5 h-3.5" />
        RAM 4404 {level} 对齐
      </span>
    )
  }

  return (
    <div className="space-y-6">
      {/* 顶部标题与说明 */}
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between pb-4 border-b border-slate-800 gap-2">
        <div>
          <h2 className="text-lg font-semibold text-slate-100 flex items-center gap-2">
            <BookOpen className="w-5 h-5 text-indigo-400" />
            HowNet 语义底座与角色槽位挖掘
          </h2>
          <p className="text-xs text-slate-400 mt-0.5">
            基于 OpenHowNet 23.8万条目清洗知识库与 RAM 4404 对齐，提供词汇义原概念、角色槽位与双向近反义词挖掘
          </p>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs text-slate-400">端点：</span>
          <code className="text-xs bg-slate-800 px-2 py-1 rounded text-indigo-300 font-mono">
            POST /api/hownet/describe
          </code>
        </div>
      </div>

      {/* 热门测试词推荐与查询输入 */}
      <div className="space-y-3">
        <div className="flex items-center gap-2 flex-wrap">
          <span className="text-xs text-slate-400">热门测试词组：</span>
          {POPULAR_WORDS.map(word => (
            <button
              key={word}
              onClick={() => handleSearch(word)}
              className="px-2.5 py-1 rounded-md bg-slate-800 hover:bg-slate-700 border border-slate-700/80 text-slate-300 hover:text-white text-xs font-medium transition-colors"
            >
              {word}
            </button>
          ))}
        </div>

        <div className="flex gap-2 max-w-xl">
          <div className="relative flex-1">
            <Search className="w-4 h-4 text-slate-400 absolute left-3 top-1/2 -translate-y-1/2" />
            <input
              type="text"
              value={searchWord}
              onChange={e => setSearchWord(e.target.value)}
              onKeyDown={e => e.key === 'Enter' && handleSearch()}
              placeholder="输入待查询中文单词，如：借款、飞机、发票..."
              className="w-full bg-slate-900 border border-slate-800 rounded-lg pl-9 pr-3 py-2 text-xs text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500 transition-colors font-sans"
            />
          </div>
          <button
            onClick={() => handleSearch()}
            disabled={isSearching}
            className="flex items-center gap-1.5 px-4 py-2 bg-indigo-600 hover:bg-indigo-500 disabled:bg-slate-800 disabled:text-slate-600 text-white rounded-lg text-xs font-medium transition-colors shadow-lg shadow-indigo-950/40 shrink-0"
          >
            {isSearching ? (
              <>
                <RotateCw className="w-4 h-4 animate-spin text-indigo-200" />
                <span>查询中...</span>
              </>
            ) : (
              <>
                <Sparkles className="w-4 h-4 text-indigo-200" />
                <span>查询义原</span>
              </>
            )}
          </button>
        </div>

        {errorMsg && (
          <div className="p-3 bg-rose-500/10 border border-rose-500/20 rounded-lg text-rose-300 text-xs flex items-start gap-2 max-w-xl">
            <AlertTriangle className="w-4 h-4 text-rose-400 shrink-0 mt-0.5" />
            <span>{errorMsg}</span>
          </div>
        )}
      </div>

      {/* 查询结果面板 */}
      {result && (
        <div className="space-y-4 pt-2">
          {/* 状态徽章条 */}
          <div className="p-4 rounded-xl bg-slate-900/80 border border-slate-800 flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3">
            <div className="flex items-center gap-3">
              <span className="text-base font-bold text-slate-100">{result.word}</span>
              {getAlignmentBadge()}
              {result.found ? (
                <span className="px-2 py-0.5 rounded bg-emerald-500/10 text-emerald-400 text-xs border border-emerald-500/20">
                  词库收录
                </span>
              ) : (
                <span className="px-2 py-0.5 rounded bg-amber-500/10 text-amber-400 text-xs border border-amber-500/20">
                  未直接收录（降级生成）
                </span>
              )}
            </div>
            {result.top_concept && (
              <div className="flex items-center gap-1.5 text-xs text-slate-400">
                <GitFork className="w-3.5 h-3.5 text-indigo-400" />
                <span>顶层义原概念:</span>
                <code className="px-2 py-0.5 rounded bg-slate-800 text-indigo-300 font-mono">
                  {result.top_concept}
                </code>
              </div>
            )}
          </div>

          {/* 一句话自然语言语义描述 */}
          <div className="p-4 rounded-xl bg-gradient-to-r from-indigo-950/30 to-slate-900 border border-indigo-500/20 space-y-1.5 shadow-md">
            <div className="text-[11px] font-semibold text-indigo-300 uppercase tracking-wider flex items-center gap-1.5">
              <Quote className="w-3.5 h-3.5 text-indigo-400" />
              HowNet 语义定义与自然语言释义
            </div>
            <p className="text-xs sm:text-sm text-slate-200 leading-relaxed">
              {result.description || '暂无释义'}
            </p>
          </div>

          {/* 角色槽位与近反义词网格 */}
          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            {/* 语义角色槽位表 */}
            <div className="p-4 rounded-xl bg-slate-900/70 border border-slate-800 space-y-3">
              <h3 className="text-xs font-semibold text-slate-300 flex items-center gap-1.5">
                <Share2 className="w-3.5 h-3.5 text-cyan-400" />
                语义角色与语法槽位 (Semantic Roles)
              </h3>
              {result.slots && result.slots.length > 0 ? (
                <div className="space-y-2">
                  {result.slots.map((slot, idx) => (
                    <div
                      key={idx}
                      className="flex items-center justify-between p-2 rounded bg-slate-800/40 border border-slate-800 text-xs"
                    >
                      <span className="font-mono text-cyan-300 font-medium">{slot.role}</span>
                      <span className="text-slate-300 font-sans">{slot.filler}</span>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="text-xs text-slate-500 italic">未匹配到结构化角色槽位</div>
              )}
            </div>

            {/* 近义词与反义词 */}
            <div className="p-4 rounded-xl bg-slate-900/70 border border-slate-800 space-y-4">
              {/* 近义词 */}
              <div className="space-y-2">
                <h4 className="text-xs font-semibold text-slate-300 flex items-center gap-1.5">
                  <Tag className="w-3.5 h-3.5 text-emerald-400" />
                  同义词 / 近义词集合 ({result.synonyms?.length || 0})
                </h4>
                {result.synonyms && result.synonyms.length > 0 ? (
                  <div className="flex flex-wrap gap-1.5">
                    {result.synonyms.map((syn, idx) => (
                      <span
                        key={idx}
                        onClick={() => handleSearch(syn)}
                        className="cursor-pointer px-2 py-0.5 rounded bg-emerald-500/10 hover:bg-emerald-500/20 border border-emerald-500/20 text-emerald-200 text-xs transition-colors"
                        title="点击查询此词"
                      >
                        {syn}
                      </span>
                    ))}
                  </div>
                ) : (
                  <div className="text-xs text-slate-500 italic">无近义词推荐</div>
                )}
              </div>

              {/* 反义词 */}
              <div className="space-y-2 pt-2 border-t border-slate-800/80">
                <h4 className="text-xs font-semibold text-slate-300 flex items-center gap-1.5">
                  <ArrowRightLeft className="w-3.5 h-3.5 text-amber-400" />
                  反义词 / 语义互斥对 ({result.antonyms?.length || 0})
                </h4>
                {result.antonyms && result.antonyms.length > 0 ? (
                  <div className="flex flex-wrap gap-1.5">
                    {result.antonyms.map((ant, idx) => (
                      <span
                        key={idx}
                        onClick={() => handleSearch(ant)}
                        className="cursor-pointer px-2 py-0.5 rounded bg-amber-500/10 hover:bg-amber-500/20 border border-amber-500/20 text-amber-200 text-xs transition-colors"
                        title="点击查询此词"
                      >
                        {ant}
                      </span>
                    ))}
                  </div>
                ) : (
                  <div className="text-xs text-slate-500 italic">无反义词记录</div>
                )}
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
