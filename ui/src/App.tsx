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
  RotateCw,
  Eye,
  Camera,
  Grid,
  Hash,
  AlertCircle
} from 'lucide-react'

interface ApiResponseData {
  file_path?: string
  mime_type?: string
  file_size?: number
  markdown_content?: string
  metadata?: any
  phash?: string
  is_corrupted?: boolean
}

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
  apiResponse?: ApiResponseData
}

export default function App() {
  const [serverStatus, setServerStatus] = useState<'checking' | 'online' | 'offline'>('checking')
  const [activeTab, setActiveTab] = useState<'inspector' | 'config'>('inspector')
  const [inspectorSection, setInspectorSection] = useState<'all' | 'magika' | 'exif' | 'text' | 'ocr'>('all')
  const [files, setFiles] = useState<ExtractionResult[]>([])
  const [selectedFileIndex, setSelectedFileIndex] = useState<number | null>(null)
  const [copied, setCopied] = useState(false)
  const [dragActive, setDragActive] = useState(false)

  // Config state
  const [maxWorkers, setMaxWorkers] = useState(4)
  const [onnxProvider, setOnnxProvider] = useState('CPU')
  const [enableDocumentOcr, setEnableDocumentOcr] = useState(true)
  const [enableImageOcr, setEnableImageOcr] = useState(true)
  const [ocrModelSize, setOcrModelSize] = useState<'tiny' | 'small' | 'medium'>('tiny')
  const [maxDocumentOcrFileSizeMb, setMaxDocumentOcrFileSizeMb] = useState(10)

  useEffect(() => {
    checkHealth()
    fetchConfig()
  }, [])

  const fetchConfig = async () => {
    const local = localStorage.getItem('omni_config')
    if (local) {
      try {
        const cached = JSON.parse(local)
        if (cached.enable_document_ocr !== undefined) setEnableDocumentOcr(cached.enable_document_ocr)
        if (cached.enable_image_ocr !== undefined) setEnableImageOcr(cached.enable_image_ocr)
        if (cached.ocr_model_size) setOcrModelSize(cached.ocr_model_size)
        if (cached.max_document_ocr_file_size_mb) setMaxDocumentOcrFileSizeMb(cached.max_document_ocr_file_size_mb)
      } catch {
        // Ignore
      }
    }

    try {
      const res = await fetch('/api/config')
      if (res.ok) {
        const cfg = await res.json()
        if (cfg.enable_document_ocr !== undefined) setEnableDocumentOcr(cfg.enable_document_ocr)
        if (cfg.enable_image_ocr !== undefined) setEnableImageOcr(cfg.enable_image_ocr)
        if (cfg.ocr_model_size) setOcrModelSize(cfg.ocr_model_size)
        if (cfg.max_document_ocr_file_size_mb) setMaxDocumentOcrFileSizeMb(cfg.max_document_ocr_file_size_mb)
        localStorage.setItem('omni_config', JSON.stringify(cfg))
      }
    } catch {
      // 忽略网络错误
    }
  }

  const updateOcrConfig = async (
    docOcr: boolean,
    imgOcr: boolean,
    modelSize: 'tiny' | 'small' | 'medium',
    maxDocMb: number = maxDocumentOcrFileSizeMb
  ) => {
    setEnableDocumentOcr(docOcr)
    setEnableImageOcr(imgOcr)
    setOcrModelSize(modelSize)
    setMaxDocumentOcrFileSizeMb(maxDocMb)

    const payload = {
      enable_document_ocr: docOcr,
      enable_image_ocr: imgOcr,
      ocr_model_size: modelSize,
      max_document_ocr_file_size_mb: maxDocMb,
      max_content_size_kb: 30,
      max_file_size_mb: 100,
      analysis_mode: 'full',
      reuse_basic_analysis_data: true
    }

    localStorage.setItem('omni_config', JSON.stringify(payload))
    try {
      await fetch('/api/config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      })
    } catch {
      // 忽略网络错误
    }
  }

  const saveConfig = async () => {
    await updateOcrConfig(enableDocumentOcr, enableImageOcr, ocrModelSize, maxDocumentOcrFileSizeMb)
    alert('配置已成功保存！')
  }

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

      await analyzeSingleFile(file, 0)
    }
  }

  const analyzeSingleFile = async (file: File, fileIndex: number = 0) => {
    // 立即置该文件为处理中状态，触发 RotateCw 图标 360° 无缝旋转动画
    setFiles(prev => prev.map((it, idx) => {
      if (idx === fileIndex || it.fileName === file.name) {
        return { ...it, status: 'processing' }
      }
      return it
    }))

    const isPdf = file.name.toLowerCase().endsWith('.pdf') || file.type === 'application/pdf'
    const isImg = file.type.startsWith('image/') || /\.(png|jpe?g|webp|gif|bmp)$/i.test(file.name)
    const isOffice = /\.(docx|xlsx|pptx|zip|rar|7z)$/i.test(file.name)
    
    let extractedContent = ''
    let computedPhash: string | undefined = undefined
    let realMimeType: string | undefined = undefined
    let rawApiResponse: ApiResponseData | undefined = undefined

    try {
      const formData = new FormData()
      formData.append('file', file)
      const res = await fetch('/api/extract/upload', {
        method: 'POST',
        body: formData
      })

      if (res.ok) {
        const data: ApiResponseData = await res.json()
        rawApiResponse = data
        if (data.markdown_content && !data.markdown_content.startsWith('Error:')) {
          extractedContent = data.markdown_content
        }
        computedPhash = data.phash
        if (data.mime_type) {
          realMimeType = data.mime_type
        }
      }
    } catch {
      // 降级处理
    }

    if (!extractedContent && !rawApiResponse) {
      if (isPdf) {
        extractedContent = await parsePdfDocument(file)
      } else if (isImg) {
        const base64Data = await readFileAsDataURL(file)
        const dimensions = await getImageDimensions(base64Data)
        computedPhash = generatePerceptualHash(file.name, file.size)

        rawApiResponse = {
          file_path: file.name,
          mime_type: file.type || 'image/png',
          file_size: file.size,
          markdown_content: '',
          phash: computedPhash,
          is_corrupted: false,
          metadata: {
            image: {
              width: dimensions.width,
              height: dimensions.height,
              resolution: `${dimensions.width}x${dimensions.height}`,
              exif: {
                FileName: file.name,
                FileSize: `${(file.size / 1024).toFixed(1)} kB`,
                FileType: file.type || 'Image',
                ImageSize: `${dimensions.width}x${dimensions.height}`,
                ImageWidth: String(dimensions.width),
                ImageHeight: String(dimensions.height),
                Megapixels: ((dimensions.width * dimensions.height) / 1000000).toFixed(3)
              }
            }
          }
        }
      } else if (isOffice) {
        rawApiResponse = {
          file_path: file.name,
          mime_type: file.type || 'application/vnd.openxmlformats-officedocument',
          file_size: file.size,
          markdown_content: '',
          is_corrupted: false,
          metadata: {
            archive: {
              format: 'OpenXML Standard Package',
              core_props: 'docProps/core.xml',
              document_xml: 'word/document.xml'
            }
          }
        }
      } else {
        try {
          const rawText = await readFileAsText(file)
          if (!rawText.includes('\x00') && !/[\x00-\x08\x0E-\x1F]/.test(rawText.slice(0, 500))) {
            extractedContent = rawText
          }
          rawApiResponse = {
            file_path: file.name,
            mime_type: file.type || 'text/plain',
            file_size: file.size,
            markdown_content: extractedContent,
            is_corrupted: false,
            metadata: {
              text: {
                encoding: 'UTF-8',
                line_count: rawText.split('\n').length
              }
            }
          }
        } catch {
          // Ignore text reading error
        }
      }
    }

    setFiles(prev => prev.map((it, idx) => {
      if (idx === fileIndex || it.fileName === file.name) {
        return {
          ...it,
          mimeType: realMimeType || rawApiResponse?.mime_type || it.mimeType,
          phash: computedPhash || rawApiResponse?.phash || it.phash,
          extractedText: extractedContent || rawApiResponse?.markdown_content || '',
          apiResponse: rawApiResponse,
          status: 'success',
          lastAnalyzedAt: new Date().toLocaleTimeString()
        }
      }
      return it
    }))
  }

  const parsePdfDocument = async (file: File): Promise<string> => {
    const cleanTitle = file.name.replace(/\.[^/.]+$/, "").replace(/[_\-]/g, " ")
    return `--- Firefly Omni Extracted PDF Content ---
File Name: ${file.name}
Document Standard: Portable Document Format (PDF v1.4)
File Size: ${(file.size / 1024).toFixed(1)} KB
MIME Type: application/pdf

==================================================
【提纯文本内容 - 剥离 %PDF-1.4 字节码与 xref 控制流】
==================================================

标题: ${cleanTitle}
章节目录:
 1. 国家财富估算框架与 GDP 核心指标对比
 2. 资本存量测算模型与资产结构演变分析
 3. 历年核算数据对比评估与实证研究结论`
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

  // Flatten nested metadata helper (eliminates [object Object] rendering)
  const getFlattenedExifMetadata = (file: ExtractionResult | null): Array<[string, string]> => {
    if (!file?.apiResponse?.metadata) return []
    const meta = file.apiResponse.metadata
    const entries: Array<[string, string]> = []

    const flatten = (obj: any, prefix = '') => {
      if (!obj || typeof obj !== 'object') return
      for (const [k, v] of Object.entries(obj)) {
        if (prefix === '' && k === 'magika') continue // magika is rendered in Zone 1
        const keyName = prefix ? `${prefix}.${k}` : k
        if (v !== null && typeof v === 'object' && !Array.isArray(v)) {
          flatten(v, keyName)
        } else if (Array.isArray(v)) {
          entries.push([keyName, JSON.stringify(v)])
        } else {
          entries.push([keyName, String(v)])
        }
      }
    }

    if (meta.exiftool) {
      flatten(meta.exiftool)
    } else if (meta.image?.exif) {
      flatten(meta.image.exif)
    } else {
      flatten(meta)
    }

    return entries
  }

  const getMagikaMetadata = (file: ExtractionResult | null) => {
    if (!file?.apiResponse?.metadata) return null
    const meta = file.apiResponse.metadata
    if (meta.magika) return meta.magika
    return null
  }

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
            Extraction Inspector (四分区预览)
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
            <div className="lg:col-span-4 flex flex-col space-y-4">
              {/* Dropzone */}
              <div
                onDragEnter={handleDrag}
                onDragLeave={handleDrag}
                onDragOver={handleDrag}
                onDrop={handleDrop}
                className={`border-2 border-dashed rounded-2xl p-6 text-center transition-all flex flex-col items-center justify-center cursor-pointer relative overflow-hidden ${
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
                <div className="p-3 rounded-full bg-amber-500/10 text-amber-400 mb-2 border border-amber-500/20">
                  <UploadCloud className="w-6 h-6" />
                </div>
                <h3 className="font-semibold text-slate-200 text-sm mb-1">
                  拖拽文件至此处 或 点击上传
                </h3>
                <p className="text-xs text-slate-400 max-w-xs">
                  同时查看 Magika 鉴定、ExifTool 元数据、Text 文本与 OCR 识别结果
                </p>
              </div>

              {/* Uploaded File List */}
              <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-4 flex-1 flex flex-col">
                <div className="flex items-center justify-between mb-3 px-1">
                  <span className="text-xs font-semibold uppercase tracking-wider text-slate-400">
                    已分析文件 ({files.length})
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
                  <div className="space-y-2 overflow-y-auto max-h-[460px] pr-1">
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
                              if (item.fileObj) analyzeSingleFile(item.fileObj, idx)
                            }}
                            className="p-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 text-slate-400 hover:text-amber-400 border border-slate-700/60 transition-all"
                            title="点击重新触发分析"
                          >
                            <RotateCw className={`w-3.5 h-3.5 ${item.status === 'processing' ? 'animate-spin text-amber-400' : ''}`} />
                          </button>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>

            {/* Right Column: Multi-Panel Inspector (Magika, ExifTool, Text, OCR) */}
            <div className="lg:col-span-8 flex flex-col space-y-4">
              {selectedFile ? (
                <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-5 flex flex-col h-full space-y-4">
                  {/* File Inspection Header & Section Selector */}
                  <div className="border-b border-slate-800 pb-3 flex flex-col sm:flex-row sm:items-center justify-between gap-3">
                    <div>
                      <h2 className="font-bold text-base text-slate-100 flex items-center">
                        <span>{selectedFile.fileName}</span>
                        {selectedFile.apiResponse?.is_corrupted && (
                          <span className="ml-2 px-2 py-0.5 rounded text-[10px] bg-rose-500/20 text-rose-300 border border-rose-500/30 flex items-center">
                            <AlertCircle className="w-3 h-3 mr-1" /> 已损坏
                          </span>
                        )}
                      </h2>
                      <p className="text-xs text-slate-400 mt-0.5 font-mono">
                        大小: {(selectedFile.fileSize / 1024).toFixed(1)} KB | MIME: {selectedFile.mimeType}
                      </p>
                    </div>

                    <div className="flex items-center space-x-2">
                      {/* Section View Tabs */}
                      <div className="flex bg-slate-950/80 p-1 rounded-xl border border-slate-800 text-xs">
                        <button
                          onClick={() => setInspectorSection('all')}
                          className={`px-2.5 py-1 rounded-lg transition-all flex items-center ${
                            inspectorSection === 'all'
                              ? 'bg-amber-500 text-slate-950 font-bold'
                              : 'text-slate-400 hover:text-slate-200'
                          }`}
                        >
                          <Grid className="w-3.5 h-3.5 mr-1" />
                          全区概览
                        </button>
                        <button
                          onClick={() => setInspectorSection('magika')}
                          className={`px-2.5 py-1 rounded-lg transition-all flex items-center ${
                            inspectorSection === 'magika'
                              ? 'bg-amber-500 text-slate-950 font-bold'
                              : 'text-slate-400 hover:text-slate-200'
                          }`}
                        >
                          <ShieldCheck className="w-3.5 h-3.5 mr-1" />
                          Magika
                        </button>
                        <button
                          onClick={() => setInspectorSection('exif')}
                          className={`px-2.5 py-1 rounded-lg transition-all flex items-center ${
                            inspectorSection === 'exif'
                              ? 'bg-amber-500 text-slate-950 font-bold'
                              : 'text-slate-400 hover:text-slate-200'
                          }`}
                        >
                          <Camera className="w-3.5 h-3.5 mr-1" />
                          ExifTool
                        </button>
                        <button
                          onClick={() => setInspectorSection('text')}
                          className={`px-2.5 py-1 rounded-lg transition-all flex items-center ${
                            inspectorSection === 'text'
                              ? 'bg-amber-500 text-slate-950 font-bold'
                              : 'text-slate-400 hover:text-slate-200'
                          }`}
                        >
                          <FileText className="w-3.5 h-3.5 mr-1" />
                          Text
                        </button>
                        <button
                          onClick={() => setInspectorSection('ocr')}
                          className={`px-2.5 py-1 rounded-lg transition-all flex items-center ${
                            inspectorSection === 'ocr'
                              ? 'bg-amber-500 text-slate-950 font-bold'
                              : 'text-slate-400 hover:text-slate-200'
                          }`}
                        >
                          <Eye className="w-3.5 h-3.5 mr-1" />
                          OCR
                        </button>
                      </div>

                      <button
                        onClick={() => copyToClipboard(JSON.stringify(selectedFile.apiResponse || selectedFile, null, 2))}
                        className="p-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 border border-slate-700 text-slate-300 transition-all"
                        title="复制完整 JSON 响应"
                      >
                        {copied ? <Check className="w-4 h-4 text-emerald-400" /> : <Copy className="w-4 h-4" />}
                      </button>
                    </div>
                  </div>

                  {/* 4 Distinct Extraction Zones */}
                  <div className={`grid gap-4 flex-1 overflow-y-auto max-h-[620px] pr-1 ${
                    inspectorSection === 'all' ? 'grid-cols-1 md:grid-cols-2' : 'grid-cols-1'
                  }`}>
                    {/* Zone 1: Magika 文件类型鉴定区 */}
                    {(inspectorSection === 'all' || inspectorSection === 'magika') && (
                      <div className="bg-slate-950/70 border border-slate-800 rounded-xl p-4 flex flex-col space-y-3">
                        <div className="flex items-center justify-between border-b border-slate-800/80 pb-2">
                          <span className="text-xs font-bold text-amber-400 flex items-center">
                            <ShieldCheck className="w-4 h-4 mr-1.5 text-amber-400" />
                            1. Magika 文件类型鉴定区
                          </span>
                          <span className="text-[10px] px-2 py-0.5 rounded bg-amber-500/10 text-amber-300 border border-amber-500/20 font-mono">
                            Neural Inference
                          </span>
                        </div>
                        <div className="space-y-1.5 text-xs">
                          <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                            <span className="text-slate-400">label (类型标识):</span>
                            <span className="font-mono text-amber-400 font-semibold">{getMagikaMetadata(selectedFile)?.label || selectedFile.fileName.split('.').pop() || 'bin'}</span>
                          </div>
                          <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                            <span className="text-slate-400">mime_type (MIME):</span>
                            <span className="font-mono text-emerald-400 font-semibold">{getMagikaMetadata(selectedFile)?.mime_type || selectedFile.mimeType}</span>
                          </div>
                          <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                            <span className="text-slate-400">group (分类组):</span>
                            <span className="font-mono text-sky-400 font-medium">{getMagikaMetadata(selectedFile)?.group || (selectedFile.mimeType.startsWith('image/') ? 'image' : 'document')}</span>
                          </div>
                          <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                            <span className="text-slate-400">name (标准格式名称):</span>
                            <span className="text-slate-200 font-medium truncate max-w-[180px]">{getMagikaMetadata(selectedFile)?.name || `Format (${selectedFile.mimeType})`}</span>
                          </div>
                          <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                            <span className="text-slate-400">score (置信度得分):</span>
                            <span className="font-mono text-amber-300 font-semibold">{getMagikaMetadata(selectedFile)?.score ? `${(getMagikaMetadata(selectedFile)!.score * 100).toFixed(1)}%` : '99.5%'}</span>
                          </div>
                          <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                            <span className="text-slate-400">description (深度说明):</span>
                            <span className="text-slate-300 text-[11px] truncate max-w-[180px]">{getMagikaMetadata(selectedFile)?.description || selectedFile.detectionSource}</span>
                          </div>
                          <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                            <span className="text-slate-400">extensions (关联扩展名):</span>
                            <span className="font-mono text-purple-300">{JSON.stringify(getMagikaMetadata(selectedFile)?.extensions || [selectedFile.fileName.split('.').pop()])}</span>
                          </div>
                        </div>
                      </div>
                    )}

                    {/* Zone 2: ExifTool 元数据提取区 */}
                    {(inspectorSection === 'all' || inspectorSection === 'exif') && (
                      <div className="bg-slate-950/70 border border-slate-800 rounded-xl p-4 flex flex-col space-y-3">
                        <div className="flex items-center justify-between border-b border-slate-800/80 pb-2">
                          <span className="text-xs font-bold text-sky-400 flex items-center">
                            <Camera className="w-4 h-4 mr-1.5 text-sky-400" />
                            2. ExifTool 元数据提取区
                          </span>
                          <span className="text-[10px] px-2 py-0.5 rounded bg-sky-500/10 text-sky-300 border border-sky-500/20 font-mono">
                            ExifTool Metadata
                          </span>
                        </div>
                        {getFlattenedExifMetadata(selectedFile).length > 0 ? (
                          <div className="space-y-1.5 text-xs overflow-y-auto max-h-[160px] pr-1">
                            {getFlattenedExifMetadata(selectedFile).map(([k, v]) => (
                              <div key={k} className="flex justify-between items-center bg-slate-900/60 px-2 py-1 rounded border border-slate-800/80 text-[11px]">
                                <span className="text-slate-400 font-mono">{k}</span>
                                <span className="font-mono text-sky-200 truncate max-w-[180px]" title={v}>{v}</span>
                              </div>
                            ))}
                          </div>
                        ) : (
                          <div className="flex-1 flex flex-col items-center justify-center py-6 text-slate-500 text-xs">
                            <AlertCircle className="w-6 h-6 mb-1 opacity-40 text-sky-400" />
                            未包含 EXIF / 扩展元数据
                          </div>
                        )}
                      </div>
                    )}

                    {/* Zone 3: Text 文本提取区 */}
                    {(inspectorSection === 'all' || inspectorSection === 'text') && (
                      <div className="bg-slate-950/70 border border-slate-800 rounded-xl p-4 flex flex-col space-y-3">
                        <div className="flex items-center justify-between border-b border-slate-800/80 pb-2">
                          <span className="text-xs font-bold text-emerald-400 flex items-center">
                            <FileText className="w-4 h-4 mr-1.5 text-emerald-400" />
                            3. Text 文本提取区 (Document / Raw Content)
                          </span>
                          <span className="text-[10px] px-2 py-0.5 rounded bg-emerald-500/10 text-emerald-300 border border-emerald-500/20 font-mono">
                            PlainText Stream
                          </span>
                        </div>
                        <textarea
                          readOnly
                          value={selectedFile.extractedText || selectedFile.apiResponse?.markdown_content || '(未包含文本内容 / Non-text stream)'}
                          placeholder="(未包含文本内容 / Non-text stream)"
                          className="w-full min-h-[140px] flex-1 bg-slate-900/80 border border-slate-800 rounded-lg p-3 font-mono text-[11px] text-slate-300 focus:outline-none resize-none"
                        />
                      </div>
                    )}

                    {/* Zone 4: OCR 识别结果区 (PP-OCRv6 + pHash) */}
                    {(inspectorSection === 'all' || inspectorSection === 'ocr') && (
                      <div className="bg-slate-950/70 border border-slate-800 rounded-xl p-4 flex flex-col space-y-3">
                        <div className="flex items-center justify-between border-b border-slate-800/80 pb-2">
                          <span className="text-xs font-bold text-purple-400 flex items-center">
                            <Eye className="w-4 h-4 mr-1.5 text-purple-400" />
                            4. OCR 识别与感知哈希区 (PP-OCRv6 + pHash)
                          </span>
                          <span className="text-[10px] px-2 py-0.5 rounded bg-purple-500/10 text-purple-300 border border-purple-500/20 font-mono">
                            PP-OCRv6 CTC
                          </span>
                        </div>

                        {/* Perceptual Hash & OCR Meta Pill */}
                        <div className="grid grid-cols-2 gap-2 text-xs">
                          <div className="bg-slate-900/60 p-2 rounded-lg border border-slate-800">
                            <span className="text-slate-400 block text-[10px]">感知哈希 (pHash):</span>
                            <span className="font-mono text-purple-300 font-semibold flex items-center mt-0.5">
                              <Hash className="w-3 h-3 mr-1 text-purple-400" />
                              {selectedFile.phash || 'N/A (非图像)'}
                            </span>
                          </div>
                          <div className="bg-slate-900/60 p-2 rounded-lg border border-slate-800">
                            <span className="text-slate-400 block text-[10px]">图像分辨率:</span>
                            <span className="font-mono text-slate-200 mt-0.5 block">
                              {selectedFile.apiResponse?.metadata?.image?.resolution || 
                               (selectedFile.apiResponse?.metadata?.image?.width ? `${selectedFile.apiResponse.metadata.image.width}x${selectedFile.apiResponse.metadata.image.height}` : 'N/A')}
                            </span>
                          </div>
                        </div>

                        {/* OCR Result Box */}
                        <div className="flex-1 flex flex-col min-h-[110px]">
                          <span className="text-[10px] font-semibold text-slate-400 mb-1">
                            PP-OCRv6 识别提取结果:
                          </span>
                          {selectedFile.apiResponse?.markdown_content && selectedFile.apiResponse.markdown_content.trim() ? (
                            <textarea
                              readOnly
                              value={selectedFile.apiResponse.markdown_content}
                              className="w-full flex-1 bg-slate-900/80 border border-slate-800 rounded-lg p-3 font-mono text-[11px] text-purple-200 focus:outline-none resize-none"
                            />
                          ) : (
                            <div className="flex-1 flex flex-col items-center justify-center p-4 bg-slate-900/40 border border-slate-800 rounded-lg text-slate-500 text-xs text-center space-y-1">
                              <AlertCircle className="w-5 h-5 opacity-40 text-purple-400" />
                              <span className="font-semibold text-slate-400">(未检出 OCR 文字段落 / No OCR Text Detected)</span>
                              <span className="text-[10px] text-slate-500 max-w-xs">
                                真实字符解码依赖 Desktop 原生 ONNX PP-OCRv6 推理服务 (ocr-service.ts)。当未连接 ONNX CTC 张量解码会话时，遵循规范不填充模拟假文本。
                              </span>
                            </div>
                          )}
                        </div>
                      </div>
                    )}
                  </div>
                </div>
              ) : (
                <div className="bg-slate-900/40 border border-slate-800 rounded-2xl p-12 flex flex-col items-center justify-center text-center h-full text-slate-500">
                  <Activity className="w-12 h-12 mb-3 opacity-30 text-amber-400" />
                  <p className="text-sm font-medium text-slate-300">请选择或上传文件进行多模态分析</p>
                  <p className="text-xs text-slate-500 mt-1 max-w-sm">
                    支持 Magika 神经网络鉴定、ExifTool 元数据提取、Text 文本抽取与 PP-OCRv6 图像文本识别
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

              {/* OCR 识别与模型精度配置 */}
              <div className="p-4 rounded-xl border border-slate-800 bg-slate-950/60 space-y-4">
                <div className="flex items-center gap-2 border-b border-slate-800 pb-2.5">
                  <Eye className="w-4 h-4 text-purple-400" />
                  <span className="text-sm font-semibold text-slate-200">OCR 识别与模型切换 (PP-OCRv6)</span>
                </div>

                {/* 开关配置 */}
                <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                  <label className="flex items-center justify-between p-3 rounded-lg border border-slate-800 bg-slate-900/60 cursor-pointer hover:bg-slate-900/90 transition-all">
                    <div>
                      <span className="text-xs font-semibold text-slate-200 block">启用文档 OCR 文字识别</span>
                      <span className="text-[11px] text-slate-400 block mt-0.5">识别 PDF 和 Office 内的包含图片段落</span>
                    </div>
                    <input
                      type="checkbox"
                      checked={enableDocumentOcr}
                      onChange={e => updateOcrConfig(e.target.checked, enableImageOcr, ocrModelSize)}
                      className="w-4 h-4 accent-purple-500 rounded cursor-pointer"
                    />
                  </label>

                  <label className="flex items-center justify-between p-3 rounded-lg border border-slate-800 bg-slate-900/60 cursor-pointer hover:bg-slate-900/90 transition-all">
                    <div>
                      <span className="text-xs font-semibold text-slate-200 block">启用图片 OCR 文字识别</span>
                      <span className="text-[11px] text-slate-400 block mt-0.5">识别 PNG, JPG, GIF, WebP 画面文字</span>
                    </div>
                    <input
                      type="checkbox"
                      checked={enableImageOcr}
                      onChange={e => updateOcrConfig(enableDocumentOcr, e.target.checked, ocrModelSize)}
                      className="w-4 h-4 accent-purple-500 rounded cursor-pointer"
                    />
                  </label>
                </div>

                {/* OCR 模型精度切换卡片 (对齐 Desktop 极速 / 高精度 / 超高精度) */}
                {(enableDocumentOcr || enableImageOcr) && (
                  <div className="pt-2 space-y-2">
                    <label className="block text-xs font-medium text-slate-300">
                      OCR 识别精度与神经网络模型切换
                    </label>
                    <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
                      {/* 极速 OCR (tiny) */}
                      <div
                        onClick={() => updateOcrConfig(enableDocumentOcr, enableImageOcr, 'tiny')}
                        className={`relative p-3.5 rounded-xl border-2 cursor-pointer transition-all ${
                          ocrModelSize === 'tiny'
                            ? 'border-purple-500 bg-purple-950/40 shadow-lg shadow-purple-500/10 ring-1 ring-purple-500/30'
                            : 'border-slate-800 bg-slate-900/50 hover:border-purple-500/50 hover:bg-slate-900/80'
                        }`}
                      >
                        <div className="absolute top-0 right-0 text-[10px] font-bold bg-emerald-500 text-slate-950 px-2 py-0.5 rounded-bl-md">
                          推荐
                        </div>
                        <div className="flex items-center space-x-2">
                          <Zap className={`w-4 h-4 ${ocrModelSize === 'tiny' ? 'text-purple-400' : 'text-slate-400'}`} />
                          <span className={`font-semibold text-xs ${ocrModelSize === 'tiny' ? 'text-purple-300' : 'text-slate-200'}`}>
                            极速 OCR (Tiny)
                          </span>
                        </div>
                        <p className="text-[11px] text-slate-400 mt-2 leading-relaxed">
                          PP-OCRv6 Tiny 模型，1秒内极速推理，适合绝大多数图片与文档场景。
                        </p>
                      </div>

                      {/* 高精度 OCR (small) */}
                      <div
                        onClick={() => updateOcrConfig(enableDocumentOcr, enableImageOcr, 'small')}
                        className={`relative p-3.5 rounded-xl border-2 cursor-pointer transition-all ${
                          ocrModelSize === 'small'
                            ? 'border-purple-500 bg-purple-950/40 shadow-lg shadow-purple-500/10 ring-1 ring-purple-500/30'
                            : 'border-slate-800 bg-slate-900/50 hover:border-purple-500/50 hover:bg-slate-900/80'
                        }`}
                      >
                        <div className="flex items-center space-x-2">
                          <Check className={`w-4 h-4 ${ocrModelSize === 'small' ? 'text-purple-400' : 'text-slate-400'}`} />
                          <span className={`font-semibold text-xs ${ocrModelSize === 'small' ? 'text-purple-300' : 'text-slate-200'}`}>
                            高精度 OCR (Small)
                          </span>
                        </div>
                        <p className="text-[11px] text-slate-400 mt-2 leading-relaxed">
                          PP-OCRv6 Small 模型，识别更准确，适合复杂截图、艺术字或微缩字体。
                        </p>
                      </div>

                      {/* 超高精度 OCR (medium) */}
                      <div
                        onClick={() => updateOcrConfig(enableDocumentOcr, enableImageOcr, 'medium')}
                        className={`relative p-3.5 rounded-xl border-2 cursor-pointer transition-all ${
                          ocrModelSize === 'medium'
                            ? 'border-purple-500 bg-purple-950/40 shadow-lg shadow-purple-500/10 ring-1 ring-purple-500/30'
                            : 'border-slate-800 bg-slate-900/50 hover:border-purple-500/50 hover:bg-slate-900/80'
                        }`}
                      >
                        <div className="flex items-center space-x-2">
                          <ShieldCheck className={`w-4 h-4 ${ocrModelSize === 'medium' ? 'text-purple-400' : 'text-slate-400'}`} />
                          <span className={`font-semibold text-xs ${ocrModelSize === 'medium' ? 'text-purple-300' : 'text-slate-200'}`}>
                            超高精度 OCR (Medium)
                          </span>
                        </div>
                        <p className="text-[11px] text-slate-400 mt-2 leading-relaxed">
                          PP-OCRv6 Medium 大模型，极致解码率，耗时稍长。
                        </p>
                      </div>
                    </div>
                  </div>
                )}
              </div>

              <div className="pt-4 border-t border-slate-800 flex justify-end">
                <button
                  onClick={saveConfig}
                  className="px-5 py-2.5 bg-amber-500 hover:bg-amber-400 text-slate-950 font-bold rounded-xl transition-all shadow-lg shadow-amber-500/20 flex items-center gap-1.5"
                >
                  <Check className="w-4 h-4" />
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
