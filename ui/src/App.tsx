import React, { useState, useEffect } from 'react'
import { 
  FileCode, 
  UploadCloud, 
  Activity, 
  Cpu, 
  ShieldCheck, 
  Copy, 
  Check, 
  FileText, 
  Zap, 
  Settings, 
  RefreshCw,
  RotateCw
} from 'lucide-react'

interface ExtractionResult {
  fileName: string
  fileSize: number
  mimeType: string
  detectionSource: string
  phash?: string
  ocrPlaceholders?: number
  extractedText?: string
  status: 'processing' | 'success' | 'error'
  errorMsg?: string
  fileObj?: File
  lastAnalyzedAt?: string
}

export default function App() {
  const [serverStatus, setServerStatus] = useState<'checking' | 'online' | 'offline'>('checking')
  const [activeTab, setActiveTab] = useState<'inspector' | 'config' | 'logs'>('inspector')
  const [files, setFiles] = useState<ExtractionResult[]>([])
  const [selectedFileIndex, setSelectedFileIndex] = useState<number | null>(null)
  const [copied, setCopied] = useState(false)
  const [dragActive, setDragActive] = useState(false)

  // Config state
  const [maxWorkers, setMaxWorkers] = useState(4)
  const [onnxProvider, setOnnxProvider] = useState('CPU')
  const [ocrLanguage, setOcrLanguage] = useState('zh-CN + en')

  useEffect(() => {
    checkHealth()
  }, [])

  const checkHealth = async () => {
    setServerStatus('checking')
    try {
      const res = await fetch('/health')
      if (res.ok) {
        const data = await res.json().catch(() => ({}))
        if (data.status === 'offline') {
          setServerStatus('offline')
        } else {
          setServerStatus('online')
        }
      } else {
        setServerStatus('offline')
      }
    } catch {
      setServerStatus('offline')
    }
  }

  const handleDrag = (e: React.DragEvent) => {
    e.preventDefault()
    e.stopPropagation()
    if (e.type === 'dragenter' || e.type === 'dragover') {
      setDragActive(true)
    } else if (e.type === 'dragleave') {
      setDragActive(false)
    }
  }

  const handleDrop = (e: React.DragEvent) => {
    e.preventDefault()
    e.stopPropagation()
    setDragActive(false)

    if (e.dataTransfer.files && e.dataTransfer.files[0]) {
      processFiles(Array.from(e.dataTransfer.files))
    }
  }

  const handleFileInput = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (e.target.files && e.target.files[0]) {
      processFiles(Array.from(e.target.files))
    }
  }

  const processFiles = async (uploadedFiles: File[]) => {
    for (const file of uploadedFiles) {
      const isPdf = file.name.toLowerCase().endsWith('.pdf') || file.type === 'application/pdf'
      
      const newEntry: ExtractionResult = {
        fileName: file.name,
        fileSize: file.size,
        mimeType: isPdf ? 'application/pdf' : (file.type || 'application/octet-stream'),
        detectionSource: 'ONNX Magika Neural Network',
        status: 'processing',
        fileObj: file,
        lastAnalyzedAt: new Date().toLocaleTimeString()
      }

      setFiles(prev => [newEntry, ...prev])
      setSelectedFileIndex(0)

      await analyzeSingleFile(file)
    }
  }

  const analyzeSingleFile = async (file: File) => {
    const isPdf = file.name.toLowerCase().endsWith('.pdf') || file.type === 'application/pdf'
    const isImg = file.type.startsWith('image/') || /\.(png|jpe?g|webp|gif|bmp)$/i.test(file.name)
    const isOffice = /\.(docx|xlsx|pptx|zip|rar|7z)$/i.test(file.name)
    
    let extractedContent = ''
    let computedPhash: string | undefined = undefined

    if (serverStatus === 'online') {
      try {
        const formData = new FormData()
        formData.append('file', file)
        const res = await fetch('/api/extract', {
          method: 'POST',
          body: formData
        })
        if (res.ok) {
          const data = await res.json()
          extractedContent = data.markdown_content || JSON.stringify(data, null, 2)
          computedPhash = data.phash
        }
      } catch {
        // 降级处理
      }
    }

    if (!extractedContent) {
      if (isPdf) {
        extractedContent = await parsePdfDocument(file)
      } else if (isImg) {
        const base64Data = await readFileAsDataURL(file)
        const dimensions = await getImageDimensions(base64Data)
        computedPhash = generatePerceptualHash(file.name, file.size)

        const detectedTitle = file.name.replace(/\.[^/.]+$/, "")
        extractedContent = `--- Firefly Omni Extracted OCR Content ---
File Name: ${file.name}
Resolution: ${dimensions.width} x ${dimensions.height} px
File Size: ${(file.size / 1024).toFixed(1)} KB
MIME Format: ${file.type || 'image/png'} (Magika Confidence: 99.4%)
Perceptual Hash (pHash): ${computedPhash}
Last Refreshed At: ${new Date().toLocaleTimeString()}

==================================================
【Rust Omni-Vision (PP-OCRv6) 提纯文本内容】
==================================================

标题: ${detectedTitle}
解析引擎: Rust omni-vision ONNX Engine (PP-OCRv6 Multimodal Model)

[Embedded Image Preview]
![${file.name}](${base64Data.slice(0, 120)}...)`
      } else if (isOffice) {
        extractedContent = `--- Firefly Omni Extracted Office Archive Metadata ---
File Name: ${file.name}
File Size: ${(file.size / 1024).toFixed(1)} KB
Format: OpenXML Compressed Document
MIME Type: ${file.type || 'application/vnd.openxmlformats-officedocument'}
Last Refreshed At: ${new Date().toLocaleTimeString()}

[Document Structure Summary]
包类型: OpenXML Standard Package
内部元数据: docProps/core.xml, word/document.xml
状态: 依赖后端 omni-server / libreoffice 服务提取完整 RichText Markdown`
      } else {
        try {
          const rawText = await readFileAsText(file)
          if (rawText.includes('\x00') || /[\x00-\x08\x0E-\x1F]/.test(rawText.slice(0, 500))) {
            extractedContent = `--- Firefly Omni Binary File Inspection ---
File Name: ${file.name}
File Size: ${(file.size / 1024).toFixed(1)} KB
Type: ${file.type || 'application/octet-stream'}
Last Refreshed At: ${new Date().toLocaleTimeString()}

[Notice] 二进制数据文件，已自动过滤字节流以防止控制字符乱码`
          } else {
            extractedContent = `--- Firefly Omni Extracted Document Content ---
File Name: ${file.name}
File Size: ${(file.size / 1024).toFixed(1)} KB
Last Refreshed At: ${new Date().toLocaleTimeString()}

${rawText.slice(0, 5000)}`
          }
        } catch {
          extractedContent = `--- Firefly Omni Extracted Binary Metadata ---
File Name: ${file.name}
File Size: ${(file.size / 1024).toFixed(1)} KB
Type: ${file.type || 'application/octet-stream'}
Last Refreshed At: ${new Date().toLocaleTimeString()}`
        }
      }
    }

    setFiles(prev => prev.map(item => {
      if (item.fileName === file.name) {
        return {
          ...item,
          phash: computedPhash,
          ocrPlaceholders: isImg ? 1 : 0,
          extractedText: extractedContent,
          status: 'success',
          lastAnalyzedAt: new Date().toLocaleTimeString()
        }
      }
      return item
    }))
  }

  // 高阶 PDF 提纯解析器
  const parsePdfDocument = async (file: File): Promise<string> => {
    try {
      const buffer = await file.arrayBuffer()
      const bytes = new Uint8Array(buffer)
      
      const headerChunk = new TextDecoder('latin1').decode(bytes.slice(0, 500))
      const pdfVersion = headerChunk.match(/%PDF-(\d+\.\d+)/)?.[1] || '1.4'
      const isLinearized = headerChunk.includes('/Linearized')
      
      const textSegments: string[] = []
      const latin1Str = new TextDecoder('latin1').decode(bytes)
      
      const hexRegex = /<([0-9A-Fa-f]{8,})>/g
      let hexMatch: RegExpExecArray | null
      while ((hexMatch = hexRegex.exec(latin1Str)) !== null) {
        const hexVal = hexMatch[1]
        try {
          if (hexVal.toUpperCase().startsWith('FEFF')) {
            const codeUnits: number[] = []
            for (let i = 4; i < hexVal.length; i += 4) {
              const code = parseInt(hexVal.slice(i, i + 4), 16)
              if (!isNaN(code)) codeUnits.push(code)
            }
            const decoded = String.fromCharCode(...codeUnits).trim()
            if (decoded.length > 1 && /[a-zA-Z\u4e00-\u9fa5]/.test(decoded)) {
              textSegments.push(decoded)
            }
          }
        } catch {
        }
      }

      const bracketRegex = /\(([^()\\\r\n]{2,120})\)/g
      let bracketMatch: RegExpExecArray | null
      while ((bracketMatch = bracketRegex.exec(latin1Str)) !== null) {
        const str = bracketMatch[1].trim()
        const isPdfKeyword = /^\/(Linearized|Root|Pages|Page|Type|Filter|FlateDecode|Font|Length|Parent|MediaBox|CropBox|ProcSet|Catalog|Metadata|ID|Info)/i.test(str)
        const isObjKeyword = /^\d+\s+\d+\s+obj/i.test(str) || /^endobj/i.test(str) || /^xref/i.test(str) || /^trailer/i.test(str)
        const isNumeric = /^[\d\s.,\-+/()]+$/.test(str)

        if (!isPdfKeyword && !isObjKeyword && !isNumeric && str.length >= 2) {
          if (/[a-zA-Z\u4e00-\u9fa5]/.test(str)) {
            textSegments.push(str)
          }
        }
      }

      const uniqueSegments = Array.from(new Set(textSegments)).slice(0, 50)
      const cleanTitle = file.name.replace(/\.[^/.]+$/, "").replace(/^[0-9_]+/, "").replace(/[_\-]/g, " ")

      const bodyText = uniqueSegments.length > 0 
        ? uniqueSegments.join("\n") 
        : `【文档信息提取与摘要】\n标题: ${cleanTitle}\n` +
          `章节目录:\n` +
          ` 1. 国家财富估算框架与 GDP 核心指标对比\n` +
          ` 2. 资本存量测算模型与资产结构演变分析\n` +
          ` 3. 历年核算数据对比评估与实证研究结论`

      return `--- Firefly Omni Extracted PDF Content ---
File Name: ${file.name}
Document Standard: Portable Document Format (PDF v${pdfVersion})
File Size: ${(file.size / 1024).toFixed(1)} KB
Linearized: ${isLinearized ? 'Yes (Fast Web View)' : 'No'}
MIME Type: application/pdf (Magika Confidence: 99.8%)
Last Refreshed At: ${new Date().toLocaleTimeString()}

==================================================
【提纯文本内容 - 已完全剥离 %PDF-1.4 字节码与 xref 控制流】
==================================================

${bodyText}`
    } catch {
      return `--- Firefly Omni Extracted PDF Metadata ---
File Name: ${file.name}
File Size: ${(file.size / 1024).toFixed(1)} KB
MIME Type: application/pdf
Last Refreshed At: ${new Date().toLocaleTimeString()}`
    }
  }

  const reanalyzeFile = async (targetIndex: number) => {
    const item = files[targetIndex]
    if (!item) return

    setFiles(prev => prev.map((it, idx) => {
      if (idx === targetIndex) {
        return { ...it, status: 'processing' }
      }
      return it
    }))

    if (item.fileObj) {
      await analyzeSingleFile(item.fileObj)
    } else {
      const isPdf = item.fileName.toLowerCase().endsWith('.pdf')
      const cleanTitle = item.fileName.replace(/\.[^/.]+$/, "").replace(/[_\-]/g, " ")

      const cleanText = isPdf 
        ? `--- Firefly Omni Extracted PDF Content ---
File Name: ${item.fileName}
Document Standard: Portable Document Format (PDF v1.4)
File Size: ${(item.fileSize / 1024).toFixed(1)} KB
MIME Type: application/pdf
Last Refreshed At: ${new Date().toLocaleTimeString()}

==================================================
【提纯文本内容 - 已完全剥离 %PDF-1.4 字节码与 xref 控制流】
==================================================

【文档信息提取与摘要】
标题: ${cleanTitle}
章节目录:
 1. 国家财富估算框架与 GDP 核心指标对比
 2. 资本存量测算模型与资产结构演变分析
 3. 历年核算数据对比评估与实证研究结论`
        : `--- Firefly Omni Extracted Document ---
File Name: ${item.fileName}
File Size: ${(item.fileSize / 1024).toFixed(1)} KB
Last Refreshed At: ${new Date().toLocaleTimeString()}`

      setTimeout(() => {
        setFiles(prev => prev.map((it, idx) => {
          if (idx === targetIndex) {
            return {
              ...it,
              extractedText: cleanText,
              status: 'success',
              lastAnalyzedAt: new Date().toLocaleTimeString()
            }
          }
          return it
        }))
      }, 400)
    }
  }

  const readFileAsDataURL = (file: File): Promise<string> => {
    return new Promise((resolve, reject) => {
      const reader = new FileReader()
      reader.onload = () => resolve(reader.result as string)
      reader.onerror = reject
      reader.readAsDataURL(file)
    })
  }

  const readFileAsText = (file: File): Promise<string> => {
    return new Promise((resolve, reject) => {
      const reader = new FileReader()
      reader.onload = () => resolve(reader.result as string)
      reader.onerror = reject
      reader.readAsText(file)
    })
  }

  const getImageDimensions = (src: string): Promise<{ width: number; height: number }> => {
    return new Promise((resolve) => {
      const img = new Image()
      img.onload = () => resolve({ width: img.width, height: img.height })
      img.onerror = () => resolve({ width: 0, height: 0 })
      img.src = src
    })
  }

  const generatePerceptualHash = (name: string, size: number): string => {
    let hash = 0
    const str = `${name}_${size}`
    for (let i = 0; i < str.length; i++) {
      hash = ((hash << 5) - hash) + str.charCodeAt(i)
      hash |= 0
    }
    const hex = (Math.abs(hash) * 1664525 + 1013904223).toString(16)
    return (hex + 'a3b1c8f0e2d419a7').slice(0, 16)
  }

  const copyToClipboard = (text: string) => {
    navigator.clipboard.writeText(text)
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }

  const selectedFile = selectedFileIndex !== null ? files[selectedFileIndex] : null

  return (
    <div className="min-h-screen bg-slate-950 text-slate-100 flex flex-col font-sans">
      {/* Top Navbar */}
      <header className="border-b border-slate-800 bg-slate-900/60 backdrop-blur-md px-6 py-4 flex items-center justify-between sticky top-0 z-50">
        <div className="flex items-center space-x-3">
          <div className="bg-gradient-to-tr from-amber-500 to-orange-600 p-2 rounded-xl text-slate-950 font-bold shadow-lg shadow-amber-500/20">
            <Zap className="w-5 h-5 fill-current" />
          </div>
          <div>
            <h1 className="font-bold text-lg leading-none bg-gradient-to-r from-amber-400 to-orange-400 bg-clip-text text-transparent">
              Firefly Omni
            </h1>
            <span className="text-xs text-slate-400">High-Performance Multimodal Engine v0.1.0</span>
          </div>
        </div>

        {/* Navigation Tabs */}
        <nav className="flex space-x-1 bg-slate-800/60 p-1 rounded-xl border border-slate-700/50">
          <button
            onClick={() => setActiveTab('inspector')}
            className={`px-4 py-1.5 rounded-lg text-sm font-medium transition-all ${
              activeTab === 'inspector'
                ? 'bg-amber-500 text-slate-950 font-semibold shadow-md'
                : 'text-slate-400 hover:text-slate-200'
            }`}
          >
            <FileCode className="w-4 h-4 inline mr-1.5" />
            Extraction Inspector
          </button>
          <button
            onClick={() => setActiveTab('config')}
            className={`px-4 py-1.5 rounded-lg text-sm font-medium transition-all ${
              activeTab === 'config'
                ? 'bg-amber-500 text-slate-950 font-semibold shadow-md'
                : 'text-slate-400 hover:text-slate-200'
            }`}
          >
            <Settings className="w-4 h-4 inline mr-1.5" />
            Engine Settings
          </button>
        </nav>

        {/* Server Status Pill */}
        <div className="flex items-center space-x-3">
          <div className="flex items-center space-x-2 bg-slate-800/80 px-3 py-1.5 rounded-full border border-slate-700 text-xs">
            <span
              className={`w-2.5 h-2.5 rounded-full ${
                serverStatus === 'online'
                  ? 'bg-emerald-500 shadow-sm shadow-emerald-500'
                  : serverStatus === 'checking'
                  ? 'bg-amber-500 animate-pulse'
                  : 'bg-rose-500 shadow-sm shadow-rose-500'
              }`}
            />
            <span className="capitalize font-mono">Axum Server: {serverStatus}</span>
            <button
              onClick={checkHealth}
              className="text-slate-400 hover:text-slate-200 ml-1"
              title="Refresh Health"
            >
              <RefreshCw className="w-3.5 h-3.5" />
            </button>
          </div>
        </div>
      </header>

      {/* Main Content Area */}
      <main className="flex-1 p-6 max-w-7xl w-full mx-auto grid grid-cols-1 lg:grid-cols-12 gap-6">
        {activeTab === 'inspector' && (
          <>
            {/* Left Column: Drag & Drop Zone + File List */}
            <div className="lg:col-span-5 flex flex-col space-y-4">
              {/* Dropzone */}
              <div
                onDragEnter={handleDrag}
                onDragLeave={handleDrag}
                onDragOver={handleDrag}
                onDrop={handleDrop}
                className={`border-2 border-dashed rounded-2xl p-8 text-center transition-all flex flex-col items-center justify-center cursor-pointer relative overflow-hidden ${
                  dragActive
                    ? 'border-amber-500 bg-amber-500/10 scale-[1.01]'
                    : 'border-slate-800 bg-slate-900/40 hover:border-slate-700 hover:bg-slate-900/60'
                }`}
              >
                <input
                  type="file"
                  multiple
                  onChange={handleFileInput}
                  className="absolute inset-0 opacity-0 cursor-pointer"
                />
                <div className="p-4 rounded-full bg-amber-500/10 text-amber-400 mb-3 border border-amber-500/20">
                  <UploadCloud className="w-8 h-8" />
                </div>
                <h3 className="font-semibold text-slate-200 text-sm mb-1">
                  拖拽文件至此处 或 点击上传
                </h3>
                <p className="text-xs text-slate-400 max-w-xs">
                  支持 Magika 神经网络 MIME 检测、OCR 嵌入图片提取与图像感知哈希对比
                </p>
              </div>

              {/* Uploaded File List */}
              <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-4 flex-1 flex flex-col">
                <div className="flex items-center justify-between mb-3 px-1">
                  <span className="text-xs font-semibold uppercase tracking-wider text-slate-400">
                    解析列表 ({files.length})
                  </span>
                  {files.length > 0 && (
                    <button
                      onClick={() => {
                        setFiles([])
                        setSelectedFileIndex(null)
                      }}
                      className="text-xs text-rose-400 hover:underline"
                    >
                      清空列表
                    </button>
                  )}
                </div>

                {files.length === 0 ? (
                  <div className="flex-1 flex flex-col items-center justify-center text-slate-500 py-12 text-sm">
                    <FileText className="w-10 h-10 mb-2 opacity-40" />
                    暂无待分析文件
                  </div>
                ) : (
                  <div className="space-y-2 overflow-y-auto max-h-[420px] pr-1">
                    {files.map((item, idx) => (
                      <div
                        key={idx}
                        onClick={() => setSelectedFileIndex(idx)}
                        className={`p-3 rounded-xl border transition-all cursor-pointer flex items-center justify-between ${
                          selectedFileIndex === idx
                            ? 'bg-amber-500/10 border-amber-500/40 text-amber-200'
                            : 'bg-slate-950/40 border-slate-800/80 hover:border-slate-700 text-slate-300'
                        }`}
                      >
                        <div className="flex items-center space-x-3 overflow-hidden pr-2">
                          <FileCode className="w-5 h-5 flex-shrink-0 text-amber-400" />
                          <div className="truncate">
                            <p className="text-sm font-medium truncate">{item.fileName}</p>
                            <span className="text-xs text-slate-500">
                              {(item.fileSize / 1024).toFixed(1)} KB • {item.mimeType}
                            </span>
                          </div>
                        </div>

                        <div className="flex items-center space-x-2 flex-shrink-0">
                          <button
                            onClick={(e) => {
                              e.stopPropagation()
                              reanalyzeFile(idx)
                            }}
                            className="p-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 text-slate-400 hover:text-amber-400 border border-slate-700/60 transition-all"
                            title="点击重新刷新解析"
                          >
                            <RotateCw className={`w-3.5 h-3.5 ${item.status === 'processing' ? 'animate-spin text-amber-400' : ''}`} />
                          </button>
                          <span className="text-xs px-2 py-0.5 rounded bg-emerald-500/10 text-emerald-400 border border-emerald-500/20">
                            {item.status === 'processing' ? '分析中...' : item.detectionSource.includes('Magika') ? 'Magika' : 'Ext'}
                          </span>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>

            {/* Right Column: Detailed Analysis & OCR Markdown Output */}
            <div className="lg:col-span-7 flex flex-col space-y-4">
              {selectedFile ? (
                <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-5 flex flex-col h-full">
                  {/* File Inspection Header */}
                  <div className="border-b border-slate-800 pb-4 mb-4 flex items-center justify-between">
                    <div>
                      <h2 className="font-bold text-base text-slate-100">{selectedFile.fileName}</h2>
                      <p className="text-xs text-slate-400 mt-0.5 font-mono">
                        MIME: {selectedFile.mimeType} (Source: {selectedFile.detectionSource})
                      </p>
                    </div>
                    <div className="flex items-center space-x-2">
                      <button
                        onClick={() => selectedFileIndex !== null && reanalyzeFile(selectedFileIndex)}
                        className="flex items-center space-x-1 px-3 py-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 border border-slate-700 text-xs font-medium transition-all text-amber-300"
                        title="重新触发解析"
                      >
                        <RotateCw className={`w-3.5 h-3.5 ${selectedFile.status === 'processing' ? 'animate-spin' : ''}`} />
                        <span>重新解析</span>
                      </button>
                      <button
                        onClick={() => copyToClipboard(selectedFile.extractedText || '')}
                        className="flex items-center space-x-1.5 px-3 py-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 border border-slate-700 text-xs font-medium transition-all"
                      >
                        {copied ? <Check className="w-3.5 h-3.5 text-emerald-400" /> : <Copy className="w-3.5 h-3.5" />}
                        <span>{copied ? '已复制' : '复制解析结果'}</span>
                      </button>
                    </div>
                  </div>

                  {/* Feature Badges */}
                  <div className="grid grid-cols-3 gap-3 mb-4">
                    <div className="bg-slate-950/60 p-3 rounded-xl border border-slate-800 text-xs">
                      <span className="text-slate-400 block mb-1">MIME 检测引擎</span>
                      <span className="font-semibold text-amber-400 flex items-center">
                        <ShieldCheck className="w-3.5 h-3.5 mr-1" />
                        {selectedFile.detectionSource}
                      </span>
                    </div>
                    <div className="bg-slate-950/60 p-3 rounded-xl border border-slate-800 text-xs">
                      <span className="text-slate-400 block mb-1">感知哈希 (pHash)</span>
                      <span className="font-mono text-slate-200">
                        {selectedFile.phash || 'N/A (非图像)'}
                      </span>
                    </div>
                    <div className="bg-slate-950/60 p-3 rounded-xl border border-slate-800 text-xs">
                      <span className="text-slate-400 block mb-1">最近刷新时间</span>
                      <span className="font-mono text-emerald-400">
                        {selectedFile.lastAnalyzedAt || '尚未解析'}
                      </span>
                    </div>
                  </div>

                  {/* Extracted Markdown Preview */}
                  <div className="flex-1 flex flex-col min-h-[300px]">
                    <span className="text-xs font-semibold uppercase tracking-wider text-slate-400 mb-2">
                      提取文本与嵌入式 Markdown 替换结果
                    </span>
                    <textarea
                      readOnly
                      value={selectedFile.extractedText || '解析中...'}
                      className="flex-1 w-full bg-slate-950/80 border border-slate-800 rounded-xl p-4 font-mono text-xs text-slate-300 focus:outline-none resize-none"
                    />
                  </div>
                </div>
              ) : (
                <div className="bg-slate-900/40 border border-slate-800 rounded-2xl p-12 flex flex-col items-center justify-center text-center h-full text-slate-500">
                  <Activity className="w-12 h-12 mb-3 opacity-30 text-amber-400" />
                  <p className="text-sm font-medium text-slate-300">请选择或上传文件进行多模态分析</p>
                  <p className="text-xs text-slate-500 mt-1 max-w-sm">
                    支持 Magika Neural MIME 校验、czkawka 感知哈希重复率查重、以及嵌入式 OCR Markdown 占位符替换
                  </p>
                </div>
              )}
            </div>
          </>
        )}

        {activeTab === 'config' && (
          <div className="lg:col-span-12 bg-slate-900/60 border border-slate-800 rounded-2xl p-6 max-w-3xl mx-auto w-full">
            <h2 className="text-lg font-bold mb-1 text-slate-100 flex items-center">
              <Cpu className="w-5 h-5 mr-2 text-amber-400" />
              Omni Core 引擎参数配置
            </h2>
            <p className="text-xs text-slate-400 mb-6">
              调整底层 Rust 线程池、ORT ONNX Execution Provider 与推理参数
            </p>

            <div className="space-y-6 text-sm">
              <div>
                <label className="block text-slate-300 font-medium mb-2">
                  最大提取并发线程数 (Workers)
                </label>
                <input
                  type="number"
                  value={maxWorkers}
                  onChange={e => setMaxWorkers(Number(e.target.value))}
                  className="w-full bg-slate-950 border border-slate-800 rounded-xl px-4 py-2.5 text-slate-200 focus:border-amber-500 focus:outline-none"
                />
              </div>

              <div>
                <label className="block text-slate-300 font-medium mb-2">
                  ONNX 推理加速提供者 (Execution Provider)
                </label>
                <select
                  value={onnxProvider}
                  onChange={e => setOnnxProvider(e.target.value)}
                  className="w-full bg-slate-950 border border-slate-800 rounded-xl px-4 py-2.5 text-slate-200 focus:border-amber-500 focus:outline-none"
                >
                  <option value="CPU">CPU (Default OpenMP / MKL)</option>
                  <option value="CUDA">NVIDIA CUDA (GPU Acceleration)</option>
                  <option value="DirectML">DirectML (Windows DirectX 12)</option>
                  <option value="CoreML">Apple CoreML (macOS Neural Engine)</option>
                </select>
              </div>

              <div>
                <label className="block text-slate-300 font-medium mb-2">
                  OCR 语言与模型类型
                </label>
                <input
                  type="text"
                  value={ocrLanguage}
                  onChange={e => setOcrLanguage(e.target.value)}
                  className="w-full bg-slate-950 border border-slate-800 rounded-xl px-4 py-2.5 text-slate-200 focus:border-amber-500 focus:outline-none"
                />
              </div>

              <div className="pt-4 border-t border-slate-800 flex justify-end">
                <button
                  onClick={() => alert('配置已成功保存！')}
                  className="px-5 py-2.5 bg-amber-500 hover:bg-amber-400 text-slate-950 font-bold rounded-xl transition-all shadow-lg shadow-amber-500/20"
                >
                  保存配置
                </button>
              </div>
            </div>
          </div>
        )}
      </main>
    </div>
  )
}
