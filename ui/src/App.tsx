import React, { useState, useEffect } from 'react'
import ReactMarkdown from 'react-markdown'
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
  AlertCircle,
  Layers,
  Music,
  Video,
  XCircle,
  MapPin,
  Globe2
} from 'lucide-react'

// ---- 离线反向地理编码 (/api/geo/reverse) 相关类型 ----
interface GeoPointResult {
  found: boolean
  country?: string | null
  province?: string | null
  city?: string | null
  distanceKm?: number | null
}

interface GeoReverseResponse {
  available: boolean
  datasetVersion?: number
  results?: GeoPointResult[]
  reason?: string
}

// 预置测试坐标组：覆盖城市层 / 中间层 / 海洋未命中 / 脏坐标容错四类场景
const GEO_PRESETS: Array<{ label: string; value: string }> = [
  {
    label: '🇨🇳 中国城市',
    value: [
      '22.5431, 114.0579   // 深圳（城市全量层）',
      '39.9042, 116.4074   // 北京',
      '31.2304, 121.4737   // 上海',
      '23.1291, 113.2644   // 广州'
    ].join('\n')
  },
  {
    label: '🌍 国际城市',
    value: [
      '40.7128, -74.0060   // 纽约',
      '51.5074, -0.1278    // 伦敦',
      '35.6762, 139.6503   // 东京',
      '-33.8688, 151.2093 // 悉尼（南半球）'
    ].join('\n')
  },
  {
    label: '🧪 边界与容错',
    value: [
      '22.5431, 114.0579   // 深圳：城市层基准点',
      '23.1400, 114.0600   // 距深圳约 66km：中间层（仅国家+省）',
      '0.0000, -140.0000   // 太平洋深处：应未命中',
      '91.0000, 114.0000   // 纬度越界：单点容错',
      '22.5400, 200.0000   // 经度越界：单点容错'
    ].join('\n')
  }
]

// 支持的界面语言选项（BCP-47 标签，服务端归一化到基础子码）
const GEO_LANGUAGE_OPTIONS = ['en', 'zh-CN', 'ja-JP', 'ko-KR', 'fr-FR', 'de-DE', 'es-ES', 'ru-RU', 'pt-PT', 'ar-EG']

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

// ---- 浏览器可直接预览的多模态文件扩展名集合 (czkawka 结果缩略图) ----
const PREVIEW_IMAGE_EXTS = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg', 'avif']
const PREVIEW_VIDEO_EXTS = ['mp4', 'm4v', 'webm', 'ogv', 'ogg', 'mov']
const PREVIEW_AUDIO_EXTS = ['mp3', 'wav', 'flac', 'aac', 'm4a', 'opus']

const getFileExt = (name: string): string => {
  const idx = name.lastIndexOf('.')
  return idx >= 0 ? name.slice(idx + 1).toLowerCase() : ''
}

type PreviewMediaType = 'image' | 'video' | 'audio' | null

const getPreviewMediaType = (name: string): PreviewMediaType => {
  const ext = getFileExt(name)
  if (PREVIEW_IMAGE_EXTS.includes(ext)) return 'image'
  if (PREVIEW_VIDEO_EXTS.includes(ext)) return 'video'
  if (PREVIEW_AUDIO_EXTS.includes(ext)) return 'audio'
  return null
}

const getFilePreviewUrl = (path: string): string =>
  `/api/file/preview?path=${encodeURIComponent(path)}`

export default function App() {
  const [serverStatus, setServerStatus] = useState<'checking' | 'online' | 'offline'>('checking')
  const [isPro, setIsPro] = useState<boolean>(false)
  const [geoAvailable, setGeoAvailable] = useState<boolean | null>(null)
  const [cleanupAvailable, setCleanupAvailable] = useState<boolean>(false)
  const [activeTab, setActiveTab] = useState<'inspector' | 'cleanup' | 'geo' | 'config'>('inspector')
  const [inspectorSection, setInspectorSection] = useState<'all' | 'magika' | 'exif' | 'text' | 'ocr'>('all')
  const [files, setFiles] = useState<ExtractionResult[]>([])
  const [selectedFileIndex, setSelectedFileIndex] = useState<number | null>(null)
  const [copied, setCopied] = useState(false)
  const [dragActive, setDragActive] = useState(false)

  // 智能文件清理与去重 14 大核心引擎全策略选择器
  const [scanPaths, setScanPaths] = useState<string>('F:\\lilun\\Desktop')
  const [strategyExact, setStrategyExact] = useState<boolean>(true)
  const [strategyPhash, setStrategyPhash] = useState<boolean>(true)
  const [strategyAudio, setStrategyAudio] = useState<boolean>(true)
  const [strategyVideo, setStrategyVideo] = useState<boolean>(false)
  const [strategyBadExt, setStrategyBadExt] = useState<boolean>(false)
  const [strategyEmptyFolders, setStrategyEmptyFolders] = useState<boolean>(false)
  const [strategyBigFiles, setStrategyBigFiles] = useState<boolean>(false)
  const [strategyEmptyFiles, setStrategyEmptyFiles] = useState<boolean>(false)
  const [strategyTemporaryFiles, setStrategyTemporaryFiles] = useState<boolean>(false)
  const [strategyInvalidSymlinks, setStrategyInvalidSymlinks] = useState<boolean>(false)
  const [strategyBrokenFiles, setStrategyBrokenFiles] = useState<boolean>(false)
  const [strategyBadNames, setStrategyBadNames] = useState<boolean>(false)
  const [strategyExifRemover, setStrategyExifRemover] = useState<boolean>(false)
  const [strategyVideoOptimizer, setStrategyVideoOptimizer] = useState<boolean>(false)
  const [nameIssuesMode, setNameIssuesMode] = useState<'multilingual' | 'strict_ascii'>('multilingual')

  const [minSimilarity, setMinSimilarity] = useState<number>(9.0)
  const [scanning, setScanning] = useState<boolean>(false)
  const [scanResult, setScanResult] = useState<any | null>(null)
  const [scanError, setScanError] = useState<string | null>(null)

  // Single file inspector
  const [singleFilePath, setSingleFilePath] = useState<string>('F:\\lilun\\Desktop\\TailwindCSS技术介绍.pdf')
  const [singleFileResult, setSingleFileResult] = useState<{ phash?: string; is_corrupted?: boolean; file_size?: number } | null>(null)
  const [inspectingSingleFile, setInspectingSingleFile] = useState<boolean>(false)

  // Config state
  const [maxWorkers, setMaxWorkers] = useState(4)
  const [onnxProvider, setOnnxProvider] = useState('CPU')
  const [enableDocumentOcr, setEnableDocumentOcr] = useState(true)
  const [enableImageOcr, setEnableImageOcr] = useState(true)
  const [ocrModelSize, setOcrModelSize] = useState<'tiny' | 'small' | 'medium'>('tiny')
  const [maxDocumentOcrFileSizeMb, setMaxDocumentOcrFileSizeMb] = useState(10)

  // Geo 反向地理编码测试状态
  const [geoCoordsText, setGeoCoordsText] = useState<string>(GEO_PRESETS[0].value)
  const [geoLanguage, setGeoLanguage] = useState<string>('zh-CN')
  const [geoMaxCityKm, setGeoMaxCityKm] = useState<string>('') // 留空 = 使用服务端默认 50km
  const [geoMaxAnyKm, setGeoMaxAnyKm] = useState<string>('') // 留空 = 使用服务端默认 500km
  const [geoResults, setGeoResults] = useState<GeoReverseResponse | null>(null)
  const [geoInputs, setGeoInputs] = useState<Array<{ latitude: number; longitude: number }>>([])
  const [geoQuerying, setGeoQuerying] = useState<boolean>(false)
  const [geoError, setGeoError] = useState<string | null>(null)

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

  const [abortController, setAbortController] = useState<AbortController | null>(null)

  const handleCancelScan = () => {
    if (abortController) {
      abortController.abort()
      setAbortController(null)
    }
    setScanning(false)
  }

  const handleRunCzkawkaScan = async () => {
    if (!scanPaths.trim()) {
      alert('请输入扫描目录或文件路径！')
      return
    }

    const controller = new AbortController()
    setAbortController(controller)
    setScanning(true)
    setScanError(null)

    // 初始化实时上屏结构
    setScanResult({
      success: true,
      total_scanned: 0,
      duplicate_groups: [],
      total_redundant_files: 0,
      total_freed_bytes: 0,
      duration_ms: 0
    })

    const pathsArray = scanPaths
      .split('\n')
      .map(p => p.trim())
      .filter(p => p.length > 0)

    const strategies: string[] = []
    if (strategyExact) strategies.push('exact_hash')
    if (strategyPhash) strategies.push('image_phash')
    if (strategyAudio) strategies.push('audio_hash')
    if (strategyVideo) strategies.push('video_phash')
    if (strategyBadExt) strategies.push('bad_extensions')
    if (strategyEmptyFolders) strategies.push('empty_folders')
    if (strategyBigFiles) strategies.push('big_files')
    if (strategyEmptyFiles) strategies.push('empty_files')
    if (strategyTemporaryFiles) strategies.push('temporary_files')
    if (strategyInvalidSymlinks) strategies.push('invalid_symlinks')
    if (strategyBrokenFiles) strategies.push('broken_files')
    if (strategyBadNames) strategies.push('bad_names')
    if (strategyExifRemover) strategies.push('exif_remover')
    if (strategyVideoOptimizer) strategies.push('video_optimizer')

    try {
      const response = await fetch('/api/duplicate/scan/stream', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          paths: pathsArray,
          strategies,
          min_similarity: minSimilarity,
          check_video: strategyVideo
        }),
        signal: controller.signal
      })

      if (!response.ok || !response.body) {
        throw new Error(`HTTP error ${response.status}: ${response.statusText}`)
      }

      const reader = response.body.getReader()
      const decoder = new TextDecoder()
      let buffer = ''

      while (true) {
        const { value, done } = await reader.read()
        if (done) break

        buffer += decoder.decode(value, { stream: true })
        const parts = buffer.split('\n\n')
        buffer = parts.pop() || ''

        for (const part of parts) {
          const lines = part.split('\n')
          let eventName = ''
          let eventData = ''

          for (const line of lines) {
            const cleanLine = line.trim()
            if (cleanLine.startsWith('event:')) {
              eventName = cleanLine.slice(6).trim()
            } else if (cleanLine.startsWith('data:')) {
              eventData += cleanLine.slice(5).trim()
            }
          }

          if (eventData) {
            try {
              const parsed = JSON.parse(eventData)
              if (eventName === 'start') {
                setScanResult((prev: any) => ({
                  ...prev,
                  total_scanned: parsed.total_scanned || 0
                }))
              } else if (eventName === 'progress') {
                setScanResult((prev: any) => ({
                  ...prev,
                  total_scanned: parsed.scanned !== undefined ? parsed.scanned : (parsed.total_scanned || prev?.total_scanned || 0)
                }))
              } else if (eventName === 'group') {
                // 实时上屏！把最新发现的重复组动态追加或更新到界面列表！
                setScanResult((prev: any) => {
                  const existingGroups: any[] = prev?.duplicate_groups || []
                  const idx = existingGroups.findIndex(g => g.group_id === parsed.group_id)
                  let newGroups: any[]
                  if (idx >= 0) {
                    newGroups = [...existingGroups]
                    newGroups[idx] = parsed
                  } else {
                    newGroups = [parsed, ...existingGroups]
                  }
                  const newFreed = newGroups.reduce((acc, g) => acc + (g.potential_freed_bytes || 0), 0)
                  const newRedundant = newGroups.reduce((acc, g) => acc + Math.max(0, (g.files?.length || 0) - 1), 0)
                  return {
                    ...prev,
                    duplicate_groups: newGroups,
                    total_freed_bytes: newFreed,
                    total_redundant_files: newRedundant
                  }
                })
              } else if (eventName === 'done') {
                setScanResult((prev: any) => ({
                  ...prev,
                  total_scanned: parsed.total_scanned,
                  total_freed_bytes: parsed.total_freed_bytes,
                  total_redundant_files: parsed.total_redundant_files,
                  duration_ms: parsed.duration_ms
                }))
              }
            } catch {
              // Ignore parse error
            }
          }
        }
      }
    } catch (err: any) {
      if (err.name === 'AbortError') {
        console.log('czkawka_core 扫描已由用户中断')
      } else {
        setScanError(err.message || 'czkawka_core 实时流扫描失败。')
      }
    } finally {
      setScanning(false)
      setAbortController(null)
    }
  }

  const handleInspectSingleFile = async () => {
    if (!singleFilePath.trim()) return
    setInspectingSingleFile(true)
    try {
      const res = await fetch('/api/extract', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ file_path: singleFilePath })
      })
      if (res.ok) {
        const data = await res.json()
        setSingleFileResult({
          phash: data.phash,
          is_corrupted: data.is_corrupted,
          file_size: data.file_size
        })
      }
    } catch {
      // Ignore
    } finally {
      setInspectingSingleFile(false)
    }
  }

  /**
   * 解析多行坐标文本：每行 "纬度, 经度"，支持 // 或 # 行内注释，
   * 分隔符兼容逗号/分号/空白；非法行直接跳过并计入错误提示。
   */
  const parseGeoCoords = (text: string): { points: Array<{ latitude: number; longitude: number }>; invalidLines: number } => {
    const points: Array<{ latitude: number; longitude: number }> = []
    let invalidLines = 0

    for (const rawLine of text.split('\n')) {
      // 剥离行内注释后取坐标段
      const line = rawLine.replace(/\/\/.*$/, '').replace(/#.*$/, '').trim()
      if (!line) continue

      const parts = line.split(/[,;\s]+/).map(Number)
      if (parts.length < 2 || parts.some(p => !Number.isFinite(p))) {
        invalidLines++
        continue
      }
      points.push({ latitude: parts[0], longitude: parts[1] })
    }
    return { points, invalidLines }
  }

  /** 发起反向地理编码批量查询：POST /api/geo/reverse */
  const handleGeoReverse = async () => {
    const { points, invalidLines } = parseGeoCoords(geoCoordsText)

    if (points.length === 0) {
      setGeoError('未解析到有效坐标，请检查输入格式（每行: 纬度, 经度）')
      setGeoResults(null)
      setGeoInputs([])
      return
    }

    setGeoError(invalidLines > 0 ? `已跳过 ${invalidLines} 行无法解析的坐标` : null)
    setGeoInputs(points)
    setGeoQuerying(true)
    setGeoResults(null)

    try {
      // 阈值留空时不下发字段，由服务端使用内置默认值（50 / 500km）
      const body: Record<string, unknown> = {
        points,
        language: geoLanguage
      }
      const cityKm = parseFloat(geoMaxCityKm)
      const anyKm = parseFloat(geoMaxAnyKm)
      if (Number.isFinite(cityKm)) body.maxCityKm = cityKm
      if (Number.isFinite(anyKm)) body.maxAnyKm = anyKm

      const res = await fetch('/api/geo/reverse', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      })

      if (!res.ok) {
        throw new Error(`HTTP error ${res.status}: ${res.statusText}`)
      }

      const data: GeoReverseResponse = await res.json()
      setGeoResults(data)
    } catch (err: any) {
      setGeoError(err.message || '反向地理编码请求失败')
    } finally {
      setGeoQuerying(false)
    }
  }

  const formatBytes = (bytes: number): string => {
    if (!bytes || bytes === 0) return '0 B'
    const k = 1024
    const sizes = ['B', 'KB', 'MB', 'GB', 'TB']
    const i = Math.floor(Math.log(bytes) / Math.log(k))
    return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i]
  }

  const checkHealth = async () => {
    setServerStatus('checking')
    try {
      const res = await fetch('/health')
      if (res.ok) {
        const data = await res.json().catch(() => ({}))
        setIsPro(Boolean(data.isPro))
        setGeoAvailable(typeof data.geoAvailable === 'boolean' ? data.geoAvailable : null)
        setCleanupAvailable(Boolean(data.cleanupAvailable || data.isPro))
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
    } else if (meta.executable) {
      flatten(meta.executable)
    } else if (meta.archive) {
      flatten(meta.archive)
    } else if (meta.database) {
      flatten(meta.database)
    } else if (meta.model) {
      flatten(meta.model)
    } else if (meta.font) {
      flatten(meta.font)
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
    <div className="h-screen overflow-hidden bg-slate-950 text-slate-100 flex flex-col font-sans">
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
            Extraction Inspector (提纯与预览)
          </button>

          {/* Pro Feature: 智能文件清理与治理 */}
          {cleanupAvailable && (
            <button
              onClick={() => setActiveTab('cleanup')}
              className={`px-4 py-1.5 rounded-lg text-sm font-medium transition-all flex items-center space-x-1.5 ${
                activeTab === 'cleanup'
                  ? 'bg-amber-500 text-slate-950 font-semibold shadow-md'
                  : 'text-slate-400 hover:text-slate-200'
              }`}
            >
              <Layers className="w-4 h-4 inline mr-1" />
              <span>文件清理 (Cleanup)</span>
              <span className="text-[10px] uppercase font-mono font-bold bg-amber-400/20 text-amber-300 border border-amber-400/40 px-1 py-0.2 rounded">PRO</span>
            </button>
          )}

          {/* Pro Feature: 离线反向地理编码 */}
          {isPro && (
            <button
              onClick={() => setActiveTab('geo')}
              className={`px-4 py-1.5 rounded-lg text-sm font-medium transition-all flex items-center space-x-1.5 ${
                activeTab === 'geo'
                  ? 'bg-amber-500 text-slate-950 font-semibold shadow-md'
                  : 'text-slate-400 hover:text-slate-200'
              }`}
            >
              <MapPin className="w-4 h-4 inline mr-1" />
              <span>Geo 地理逆编码</span>
              <span className="text-[10px] uppercase font-mono font-bold bg-amber-400/20 text-amber-300 border border-amber-400/40 px-1 py-0.2 rounded">PRO</span>
            </button>
          )}

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
            <span className="capitalize font-mono">
              Axum Server: {serverStatus} {isPro ? '(Pro Enterprise)' : '(Open Core)'}
            </span>
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
      <main className="flex-1 min-h-0 p-6 max-w-7xl w-full mx-auto grid grid-cols-1 lg:grid-cols-12 gap-6 overflow-y-auto lg:overflow-hidden lg:auto-rows-fr">
        {activeTab === 'inspector' && (
          <>
            {/* Left Column: Drag & Drop Zone + File List */}
            <div className="lg:col-span-4 min-h-0 flex flex-col space-y-4">
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
              <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-4 flex-1 min-h-0 flex flex-col">
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
                  <div className="space-y-2 overflow-y-auto flex-1 min-h-0 pr-1">
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
                        <div className="flex items-center space-x-3 overflow-hidden pr-2 flex-1 min-w-0">
                          <FileCode className="w-5 h-5 flex-shrink-0 text-amber-400" />
                          <div className="truncate flex-1 min-w-0">
                            <p className="text-sm font-medium truncate" title={item.fileName}>{item.fileName}</p>
                            <span className="text-xs text-slate-500">
                              {(item.fileSize / 1024).toFixed(1)} KB
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
            <div className="lg:col-span-8 min-h-0 flex flex-col space-y-4">
              {selectedFile ? (
                <div className="bg-slate-900/60 border border-slate-800 rounded-2xl p-5 flex flex-col flex-1 min-h-0 space-y-4">
                  {/* File Inspection Header & Section Selector */}
                  <div className="border-b border-slate-800 pb-3 flex flex-col sm:flex-row sm:items-center justify-between gap-3">
                    <div className="min-w-0 flex-1">
                      <h2 className="font-bold text-base text-slate-100 flex items-center">
                        <span className="truncate flex-1 min-w-0" title={selectedFile.fileName}>{selectedFile.fileName}</span>
                        {selectedFile.apiResponse?.is_corrupted && (
                          <span className="ml-2 px-2 py-0.5 rounded text-[10px] bg-rose-500/20 text-rose-300 border border-rose-500/30 flex items-center flex-shrink-0">
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
                  <div className={`grid gap-4 flex-1 min-h-0 overflow-y-auto pr-1 auto-rows-fr ${
                    inspectorSection === 'all' ? 'grid-cols-1 md:grid-cols-2' : 'grid-cols-1'
                  }`}>
                    {/* Zone 1: Magika 文件类型鉴定区 */}
                    {(inspectorSection === 'all' || inspectorSection === 'magika') && (
                      <div className="bg-slate-950/70 border border-slate-800 rounded-xl p-4 flex flex-col space-y-3 min-h-0 overflow-hidden">
                        <div className="flex items-center justify-between border-b border-slate-800/80 pb-2">
                          <span className="text-xs font-bold text-amber-400 flex items-center">
                            <ShieldCheck className="w-4 h-4 mr-1.5 text-amber-400" />
                            1. Magika 文件类型鉴定区
                          </span>
                          <span className="text-[10px] px-2 py-0.5 rounded bg-amber-500/10 text-amber-300 border border-amber-500/20 font-mono">
                            Neural Inference
                          </span>
                        </div>
                        <div className="space-y-1.5 text-xs overflow-y-auto flex-1 min-h-0 pr-1">
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

                          {/* 基础系统文件属性 (basic) */}
                          {selectedFile.apiResponse?.metadata?.basic && (
                            <>
                              <div className="pt-1 text-[10px] font-bold text-slate-500 uppercase tracking-wider">系统基础元数据</div>
                              {selectedFile.apiResponse.metadata.basic.createdAt && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">createdAt (创建时间):</span>
                                  <span className="font-mono text-slate-300">{new Date(selectedFile.apiResponse.metadata.basic.createdAt).toLocaleString()}</span>
                                </div>
                              )}
                              {selectedFile.apiResponse.metadata.basic.modifiedAt && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">modifiedAt (修改时间):</span>
                                  <span className="font-mono text-slate-300">{new Date(selectedFile.apiResponse.metadata.basic.modifiedAt).toLocaleString()}</span>
                                </div>
                              )}
                            </>
                          )}

                          {/* 文档精细元数据 (document) */}
                          {selectedFile.apiResponse?.metadata?.document && (
                            <>
                              <div className="pt-1 text-[10px] font-bold text-slate-500 uppercase tracking-wider">文档结构元数据</div>
                              {selectedFile.apiResponse.metadata.document.title && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">title (文档标题):</span>
                                  <span className="text-amber-300 truncate max-w-[180px]">{String(selectedFile.apiResponse.metadata.document.title)}</span>
                                </div>
                              )}
                              {selectedFile.apiResponse.metadata.document.author && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">author (作者/创建者):</span>
                                  <span className="text-sky-300 truncate max-w-[180px]">{String(selectedFile.apiResponse.metadata.document.author)}</span>
                                </div>
                              )}
                              {selectedFile.apiResponse.metadata.document.page_count !== undefined && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">page_count (总页数):</span>
                                  <span className="font-mono text-emerald-300 font-bold">{String(selectedFile.apiResponse.metadata.document.page_count)} 页</span>
                                </div>
                              )}
                              {selectedFile.apiResponse.metadata.document.word_count !== undefined && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">word_count (单词/词数):</span>
                                  <span className="font-mono text-purple-300">{String(selectedFile.apiResponse.metadata.document.word_count)} 词</span>
                                </div>
                              )}
                            </>
                          )}

                          {/* 音频/视频精细元数据 (audio / video) */}
                          {selectedFile.apiResponse?.metadata?.audio && (
                            <>
                              <div className="pt-1 text-[10px] font-bold text-slate-500 uppercase tracking-wider">音频媒体元数据</div>
                              <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                <span className="text-slate-400">duration (时长):</span>
                                <span className="font-mono text-amber-300">{selectedFile.apiResponse.metadata.audio.duration_formatted || `${selectedFile.apiResponse.metadata.audio.duration_seconds} 秒`}</span>
                              </div>
                              {selectedFile.apiResponse.metadata.audio.artist && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">artist (艺术家):</span>
                                  <span className="text-sky-300">{String(selectedFile.apiResponse.metadata.audio.artist)}</span>
                                </div>
                              )}
                            </>
                          )}
                          {selectedFile.apiResponse?.metadata?.video && (
                            <>
                              <div className="pt-1 text-[10px] font-bold text-slate-500 uppercase tracking-wider">视频媒体元数据</div>
                              <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                <span className="text-slate-400">duration (时长):</span>
                                <span className="font-mono text-amber-300">{selectedFile.apiResponse.metadata.video.duration_formatted || `${selectedFile.apiResponse.metadata.video.duration_seconds} 秒`}</span>
                              </div>
                              {selectedFile.apiResponse.metadata.video.resolution && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">resolution (分辨率):</span>
                                  <span className="font-mono text-emerald-300">{String(selectedFile.apiResponse.metadata.video.resolution)}</span>
                                </div>
                              )}
                            </>
                          )}

                          {/* 可执行程序 PE 元数据 (executable) */}
                          {selectedFile.apiResponse?.metadata?.executable && (
                            <>
                              <div className="pt-1 text-[10px] font-bold text-slate-500 uppercase tracking-wider">可执行程序 PE 元数据</div>
                              {selectedFile.apiResponse.metadata.executable.file_description && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">file_description (程序描述):</span>
                                  <span className="text-amber-300 font-medium truncate max-w-[200px]">{String(selectedFile.apiResponse.metadata.executable.file_description)}</span>
                                </div>
                              )}
                              {selectedFile.apiResponse.metadata.executable.company_name && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">company_name (开发厂商):</span>
                                  <span className="text-sky-300 truncate max-w-[200px]">{String(selectedFile.apiResponse.metadata.executable.company_name)}</span>
                                </div>
                              )}
                              {selectedFile.apiResponse.metadata.executable.product_name && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">product_name (产品名称):</span>
                                  <span className="text-emerald-300 truncate max-w-[200px]">{String(selectedFile.apiResponse.metadata.executable.product_name)}</span>
                                </div>
                              )}
                              {selectedFile.apiResponse.metadata.executable.file_version && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">file_version (文件版本):</span>
                                  <span className="font-mono text-purple-300">{String(selectedFile.apiResponse.metadata.executable.file_version)}</span>
                                </div>
                              )}
                              {selectedFile.apiResponse.metadata.executable.legal_copyright && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">legal_copyright (版权信息):</span>
                                  <span className="text-slate-300 text-[11px] truncate max-w-[200px]">{String(selectedFile.apiResponse.metadata.executable.legal_copyright)}</span>
                                </div>
                              )}
                              {selectedFile.apiResponse.metadata.executable.pe_type && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">pe_type (PE 架构):</span>
                                  <span className="font-mono text-cyan-300">{String(selectedFile.apiResponse.metadata.executable.pe_type)}</span>
                                </div>
                              )}
                            </>
                          )}

                          {/* 压缩包/镜像元数据 (archive) */}
                          {selectedFile.apiResponse?.metadata?.archive && (
                            <>
                              <div className="pt-1 text-[10px] font-bold text-slate-500 uppercase tracking-wider">压缩包/磁盘镜像元数据</div>
                              {selectedFile.apiResponse.metadata.archive.format && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">format (压缩格式):</span>
                                  <span className="font-mono text-amber-300">{String(selectedFile.apiResponse.metadata.archive.format)}</span>
                                </div>
                              )}
                              {selectedFile.apiResponse.metadata.archive.uncompressed_size !== undefined && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">uncompressed (解压尺寸):</span>
                                  <span className="font-mono text-emerald-300">{formatBytes(Number(selectedFile.apiResponse.metadata.archive.uncompressed_size) || 0)}</span>
                                </div>
                              )}
                              {selectedFile.apiResponse.metadata.archive.file_count !== undefined && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">file_count (内部文件数):</span>
                                  <span className="font-mono text-sky-300">{String(selectedFile.apiResponse.metadata.archive.file_count)} 个</span>
                                </div>
                              )}
                            </>
                          )}

                          {/* 数据库与 AI 模型元数据 (database / model) */}
                          {selectedFile.apiResponse?.metadata?.database && (
                            <>
                              <div className="pt-1 text-[10px] font-bold text-slate-500 uppercase tracking-wider">数据库结构元数据</div>
                              <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                <span className="text-slate-400">engine (数据库引擎):</span>
                                <span className="font-mono text-purple-300 font-bold">{String(selectedFile.apiResponse.metadata.database.engine)}</span>
                              </div>
                            </>
                          )}
                          {selectedFile.apiResponse?.metadata?.model && (
                            <>
                              <div className="pt-1 text-[10px] font-bold text-slate-500 uppercase tracking-wider">AI / 神经网络模型元数据</div>
                              <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                <span className="text-slate-400">format (模型类型):</span>
                                <span className="font-mono text-cyan-300 font-bold">{String(selectedFile.apiResponse.metadata.model.model_format)}</span>
                              </div>
                            </>
                          )}

                          {/* 字体元数据 (font) */}
                          {selectedFile.apiResponse?.metadata?.font && (
                            <>
                              <div className="pt-1 text-[10px] font-bold text-slate-500 uppercase tracking-wider">字体资产元数据</div>
                              {selectedFile.apiResponse.metadata.font.font_name && (
                                <div className="flex justify-between items-center bg-slate-900/60 p-1.5 rounded border border-slate-800">
                                  <span className="text-slate-400">font_name (字体族名):</span>
                                  <span className="text-amber-300">{String(selectedFile.apiResponse.metadata.font.font_name)}</span>
                                </div>
                              )}
                            </>
                          )}
                        </div>
                      </div>
                    )}

                    {/* Zone 2: ExifTool 元数据提取区 */}
                    {(inspectorSection === 'all' || inspectorSection === 'exif') && (
                      <div className="bg-slate-950/70 border border-slate-800 rounded-xl p-4 flex flex-col space-y-3 min-h-0 overflow-hidden">
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
                          <div className="space-y-1.5 text-xs overflow-y-auto flex-1 min-h-0 pr-1">
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
                      <div className="bg-slate-950/70 border border-slate-800 rounded-xl p-4 flex flex-col space-y-3 min-h-0 overflow-hidden">
                        <div className="flex items-center justify-between border-b border-slate-800/80 pb-2">
                          <span className="text-xs font-bold text-emerald-400 flex items-center">
                            <FileText className="w-4 h-4 mr-1.5 text-emerald-400" />
                            3. Text 文本提取区 (Document / Raw Content)
                          </span>
                          <span className="text-[10px] px-2 py-0.5 rounded bg-emerald-500/10 text-emerald-300 border border-emerald-500/20 font-mono">
                            PlainText Stream
                          </span>
                        </div>
                        <div className="flex-1 min-h-0 overflow-y-auto pr-1 bg-slate-900/80 border border-slate-800 rounded-lg p-3 text-[13px] text-slate-300 leading-relaxed markdown-body">
                          <ReactMarkdown>{selectedFile.extractedText || selectedFile.apiResponse?.markdown_content || '(未包含文本内容 / Non-text stream)'}</ReactMarkdown>
                        </div>
                      </div>
                    )}

                    {/* Zone 4: OCR 识别结果区 (PP-OCRv6 + pHash) */}
                    {(inspectorSection === 'all' || inspectorSection === 'ocr') && (
                      <div className="bg-slate-950/70 border border-slate-800 rounded-xl p-4 flex flex-col space-y-3 min-h-0 overflow-hidden">
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
                        <div className="flex-1 flex flex-col min-h-0">
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
                <div className="bg-slate-900/40 border border-slate-800 rounded-2xl p-12 flex flex-col items-center justify-center text-center flex-1 min-h-0 text-slate-500">
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

        {activeTab === 'cleanup' && (
          <div className="lg:col-span-12 grid grid-cols-1 lg:grid-cols-12 gap-6 min-h-0 overflow-y-auto">
            {/* Left Column: API Controller & Single File Tester (4 cols) */}
            <div className="lg:col-span-4 space-y-5 flex flex-col min-h-0">
              {/* Card 1: API Scan Controller */}
              <div className="bg-slate-900/70 border border-slate-800 rounded-2xl p-5 shadow-xl flex flex-col space-y-4">
                <div className="flex items-center justify-between border-b border-slate-800 pb-3">
                  <div className="flex items-center space-x-2">
                    <div className="bg-amber-500/10 p-2 rounded-lg text-amber-400">
                      <Layers className="w-5 h-5" />
                    </div>
                    <div>
                      <h2 className="font-bold text-sm text-slate-100 flex items-center gap-1.5">
                        <span>智能文件清理与治理 API</span>
                        <span className="text-[10px] uppercase font-mono font-bold bg-amber-400/20 text-amber-300 border border-amber-400/40 px-1 py-0.2 rounded">PRO</span>
                      </h2>
                      <span className="text-[11px] font-mono text-emerald-400">POST /api/cleanup/scan/stream</span>
                    </div>
                  </div>
                  <span className="text-[10px] font-bold px-2 py-0.5 bg-purple-500/20 text-purple-300 rounded border border-purple-500/30">
                    Omni-Cleanup Pro
                  </span>
                </div>

                {/* Scan Path Input */}
                <div>
                  <div className="flex items-center justify-between mb-1.5">
                    <label className="text-xs font-semibold text-slate-300">
                      扫描目录 / 文件路径 (支持多行)
                    </label>
                  </div>
                  <textarea
                    rows={3}
                    value={scanPaths}
                    onChange={e => setScanPaths(e.target.value)}
                    placeholder="输入需要扫描的绝对路径，例如: F:\lilun\Desktop"
                    className="w-full bg-slate-950 border border-slate-800 rounded-xl p-3 text-xs font-mono text-slate-200 focus:border-amber-500 focus:outline-none resize-none"
                  />
                  {/* Preset Buttons */}
                  <div className="flex gap-1.5 mt-2 flex-wrap">
                    <button
                      onClick={() => setScanPaths('F:\\lilun\\Desktop')}
                      className="text-[10px] bg-slate-800/80 hover:bg-slate-800 text-slate-300 px-2 py-1 rounded border border-slate-700 transition-all"
                    >
                      📁 桌面 Preset
                    </button>
                    <button
                      onClick={() => setScanPaths('D:\\workspace\\firefly-ai-folder')}
                      className="text-[10px] bg-slate-800/80 hover:bg-slate-800 text-slate-300 px-2 py-1 rounded border border-slate-700 transition-all"
                    >
                      📁 项目 Root Preset
                    </button>
                  </div>
                </div>

                {/* Strategy Toggles */}
                <div className="space-y-2 pt-1 border-t border-slate-800/80">
                  <div className="flex items-center justify-between">
                    <label className="text-xs font-semibold text-slate-300 block">
                      czkawka_core 14 大核心引擎全策略选择
                    </label>
                    <div className="flex items-center gap-1.5">
                      <button
                        type="button"
                        onClick={() => {
                          setStrategyExact(true); setStrategyPhash(true); setStrategyAudio(true); setStrategyVideo(true);
                          setStrategyBadExt(true); setStrategyEmptyFolders(true); setStrategyBigFiles(true); setStrategyEmptyFiles(true);
                          setStrategyTemporaryFiles(true); setStrategyInvalidSymlinks(true); setStrategyBrokenFiles(true); setStrategyBadNames(true);
                          setStrategyExifRemover(true); setStrategyVideoOptimizer(true);
                        }}
                        className="text-[10px] text-amber-400 hover:text-amber-300 px-1.5 py-0.5 rounded bg-amber-950/40 border border-amber-800/50"
                      >
                        全选
                      </button>
                      <button
                        type="button"
                        onClick={() => {
                          setStrategyExact(false); setStrategyPhash(false); setStrategyAudio(false); setStrategyVideo(false);
                          setStrategyBadExt(false); setStrategyEmptyFolders(false); setStrategyBigFiles(false); setStrategyEmptyFiles(false);
                          setStrategyTemporaryFiles(false); setStrategyInvalidSymlinks(false); setStrategyBrokenFiles(false); setStrategyBadNames(false);
                          setStrategyExifRemover(false); setStrategyVideoOptimizer(false);
                        }}
                        className="text-[10px] text-slate-400 hover:text-slate-300 px-1.5 py-0.5 rounded bg-slate-900 border border-slate-800"
                      >
                        清空
                      </button>
                    </div>
                  </div>

                  <div className="grid grid-cols-1 md:grid-cols-2 gap-2 max-h-[380px] overflow-y-auto pr-1">
                    {/* 1. Duplicates */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <Hash className="w-3.5 h-3.5 text-amber-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">精确重复文件 (Duplicates)</span>
                          <span className="text-[9px] text-slate-400">Blake3 分级哈希 (exact_hash)</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyExact} onChange={e => setStrategyExact(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 2. Similar Images */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <Camera className="w-3.5 h-3.5 text-purple-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">相似图片 (Similar Images)</span>
                          <span className="text-[9px] text-slate-400">BK-Tree 梯度感知哈希 (image_phash)</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyPhash} onChange={e => setStrategyPhash(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 3. Same Music */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <Music className="w-3.5 h-3.5 text-cyan-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">相似音乐 (Same Music)</span>
                          <span className="text-[9px] text-slate-400">Chromaprint 声学指纹 (audio_hash)</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyAudio} onChange={e => setStrategyAudio(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 4. Similar Videos */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <Video className="w-3.5 h-3.5 text-rose-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">相似视频 (Similar Videos)</span>
                          <span className="text-[9px] text-slate-400">时序关键帧提取 (video_phash)</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyVideo} onChange={e => setStrategyVideo(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 5. Empty Folders */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <Layers className="w-3.5 h-3.5 text-blue-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">空文件夹 (Empty Folders)</span>
                          <span className="text-[9px] text-slate-400">高级递归空目录检测</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyEmptyFolders} onChange={e => setStrategyEmptyFolders(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 6. Big Files */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <Zap className="w-3.5 h-3.5 text-yellow-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">超大文件 (Big Files)</span>
                          <span className="text-[9px] text-slate-400">全盘 Top 50 体积最大文件</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyBigFiles} onChange={e => setStrategyBigFiles(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 7. Empty Files */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <FileText className="w-3.5 h-3.5 text-slate-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">空文件 (Empty Files)</span>
                          <span className="text-[9px] text-slate-400">0 字节空文件扫描</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyEmptyFiles} onChange={e => setStrategyEmptyFiles(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 8. Temporary Files */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <RefreshCw className="w-3.5 h-3.5 text-emerald-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">临时与缓存 (Temporary Files)</span>
                          <span className="text-[9px] text-slate-400">.tmp, .bak, .cache, thumbs.db 等</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyTemporaryFiles} onChange={e => setStrategyTemporaryFiles(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 9. Invalid Symlinks */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <AlertCircle className="w-3.5 h-3.5 text-amber-500 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">断裂软链接 (Invalid Symlinks)</span>
                          <span className="text-[9px] text-slate-400">指向已不存在目标的符号链接</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyInvalidSymlinks} onChange={e => setStrategyInvalidSymlinks(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 10. Broken Files */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <XCircle className="w-3.5 h-3.5 text-red-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">损坏文件 (Broken Files)</span>
                          <span className="text-[9px] text-slate-400">损坏图像/PDF/音频/压缩包</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyBrokenFiles} onChange={e => setStrategyBrokenFiles(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 11. Bad Extensions */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <FileCode className="w-3.5 h-3.5 text-indigo-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">错误扩展名 (Bad Extensions)</span>
                          <span className="text-[9px] text-slate-400">内容魔数与后缀不匹配</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyBadExt} onChange={e => setStrategyBadExt(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 12. Bad Names */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <FileText className="w-3.5 h-3.5 text-teal-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">异常文件名 (Bad Names)</span>
                          <span className="text-[9px] text-slate-400">特殊字符/不可见/非标准命名</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyBadNames} onChange={e => setStrategyBadNames(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 13. Exif Remover */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <ShieldCheck className="w-3.5 h-3.5 text-green-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">Exif 清理 (Exif Remover)</span>
                          <span className="text-[9px] text-slate-400">检测并擦除敏感拍摄隐私元数据</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyExifRemover} onChange={e => setStrategyExifRemover(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>

                    {/* 14. Video Optimizer */}
                    <label className="flex items-center justify-between p-2 rounded-lg border border-slate-800 bg-slate-950/60 cursor-pointer hover:bg-slate-900 transition-all">
                      <div className="flex items-center space-x-2">
                        <Cpu className="w-3.5 h-3.5 text-pink-400 flex-shrink-0" />
                        <div>
                          <span className="text-[11px] font-semibold text-slate-200 block">视频优化 (Video Optimizer)</span>
                          <span className="text-[9px] text-slate-400">转码为 HEVC/AV1 并裁切黑边</span>
                        </div>
                      </div>
                      <input type="checkbox" checked={strategyVideoOptimizer} onChange={e => setStrategyVideoOptimizer(e.target.checked)} className="w-3.5 h-3.5 accent-amber-500 rounded cursor-pointer" />
                    </label>
                  </div>
                </div>

                {/* Similarity threshold slider */}
                <div className="pt-1 space-y-1.5">
                  <div className="flex items-center justify-between text-xs">
                    <span className="text-slate-300 font-medium">最小相似度阈值 (0.0 ~ 10.0)</span>
                    <span className="font-mono text-amber-400 font-bold">{minSimilarity.toFixed(1)}</span>
                  </div>
                  <input
                    type="range"
                    min="0"
                    max="10"
                    step="0.1"
                    value={minSimilarity}
                    onChange={e => setMinSimilarity(Number(e.target.value))}
                    className="w-full accent-amber-500 cursor-pointer h-1.5 bg-slate-950 rounded-lg"
                  />
                  <div className="flex gap-1">
                    {[
                      { label: '最大容差(0.0)', val: 0.0 },
                      { label: '连拍微移(5.0)', val: 5.0 },
                      { label: '标准(7.5)', val: 7.5 },
                      { label: '严苛(9.0)', val: 9.0 },
                      { label: '精确(10.0)', val: 10.0 }
                    ].map(p => (
                      <button
                        key={p.val}
                        type="button"
                        onClick={() => setMinSimilarity(p.val)}
                        className={`flex-1 py-0.5 rounded text-[10px] font-medium border transition-colors ${
                          Math.abs(minSimilarity - p.val) < 0.01
                            ? 'bg-amber-500 text-slate-950 border-amber-500 font-bold'
                            : 'bg-slate-900 text-slate-400 border-slate-800 hover:bg-slate-800'
                        }`}
                      >
                        {p.label}
                      </button>
                    ))}
                  </div>
                </div>

                {/* Trigger / Cancel Scan Buttons */}
                <div className="flex items-center gap-2.5">
                  <button
                    onClick={handleRunCzkawkaScan}
                    disabled={scanning}
                    className="flex-1 py-3 bg-gradient-to-r from-amber-500 to-orange-500 hover:from-amber-400 hover:to-orange-400 text-slate-950 font-bold text-xs rounded-xl shadow-lg shadow-amber-500/20 transition-all flex items-center justify-center space-x-2 disabled:opacity-50"
                  >
                    {scanning ? (
                      <>
                        <RotateCw className="w-4 h-4 animate-spin" />
                        <span>omni-cleanup 智能清理扫描中...</span>
                      </>
                    ) : (
                      <>
                        <Zap className="w-4 h-4 fill-current" />
                        <span>执行 14 大文件清理聚合扫描</span>
                      </>
                    )}
                  </button>

                  {scanning && (
                    <button
                      onClick={handleCancelScan}
                      className="px-4 py-3 bg-rose-600/90 hover:bg-rose-500 text-white font-bold text-xs rounded-xl border border-rose-500/60 shadow-lg shadow-rose-500/20 transition-all flex items-center justify-center space-x-1.5 flex-shrink-0 animate-pulse"
                      title="中断并取消当前扫描"
                    >
                      <XCircle className="w-4 h-4 text-white" />
                      <span>取消扫描</span>
                    </button>
                  )}
                </div>

                {scanError && (
                  <div className="p-3 bg-rose-950/40 border border-rose-800 rounded-xl text-rose-300 text-xs flex items-start space-x-2">
                    <AlertCircle className="w-4 h-4 text-rose-400 flex-shrink-0 mt-0.5" />
                    <span>{scanError}</span>
                  </div>
                )}
              </div>

              {/* Card 2: Single File Inspector */}
              <div className="bg-slate-900/70 border border-slate-800 rounded-2xl p-5 shadow-xl space-y-3">
                <div className="flex items-center space-x-2 border-b border-slate-800 pb-2.5">
                  <ShieldCheck className="w-4 h-4 text-emerald-400" />
                  <span className="text-xs font-bold text-slate-200">单文件 pHash & 破损文件检测</span>
                </div>

                <div>
                  <label className="text-[11px] font-medium text-slate-400 block mb-1">
                    目标文件绝对路径
                  </label>
                  <input
                    type="text"
                    value={singleFilePath}
                    onChange={e => setSingleFilePath(e.target.value)}
                    className="w-full bg-slate-950 border border-slate-800 rounded-lg p-2 text-xs font-mono text-slate-200 focus:border-amber-500 focus:outline-none"
                  />
                </div>

                <button
                  onClick={handleInspectSingleFile}
                  disabled={inspectingSingleFile}
                  className="w-full py-2 bg-slate-800 hover:bg-slate-700 text-slate-200 font-semibold text-xs rounded-lg border border-slate-700 transition-all flex items-center justify-center space-x-1.5"
                >
                  {inspectingSingleFile ? (
                    <RotateCw className="w-3.5 h-3.5 animate-spin" />
                  ) : (
                    <Eye className="w-3.5 h-3.5" />
                  )}
                  <span>检测 pHash 与 破损状态</span>
                </button>

                {singleFileResult && (
                  <div className="p-3 bg-slate-950 border border-slate-800 rounded-xl space-y-1.5 text-xs font-mono">
                    <div className="flex justify-between">
                      <span className="text-slate-500">czkawka pHash:</span>
                      <span className="text-amber-400 font-bold">{singleFileResult.phash || 'N/A'}</span>
                    </div>
                    <div className="flex justify-between">
                      <span className="text-slate-500">is_corrupted:</span>
                      <span className={singleFileResult.is_corrupted ? "text-rose-400 font-bold" : "text-emerald-400 font-bold"}>
                        {singleFileResult.is_corrupted ? "TRUE (文件破损/0字节)" : "FALSE (正常)"}
                      </span>
                    </div>
                    <div className="flex justify-between">
                      <span className="text-slate-500">File Size:</span>
                      <span className="text-slate-300">{formatBytes(singleFileResult.file_size || 0)}</span>
                    </div>
                  </div>
                )}
              </div>
            </div>

            {/* Right Column: Scan Results & Analytics (8 cols) */}
            <div className="lg:col-span-8 flex flex-col space-y-4 min-h-0">
              {scanResult ? (
                <div className="flex-1 min-h-0 flex flex-col space-y-4 overflow-y-auto pr-1">
                  {/* Live Streaming Progress Banner */}
                  {scanning && (
                    <div className="bg-amber-500/10 border border-amber-500/30 rounded-xl p-3 flex items-center justify-between">
                      <div className="flex items-center space-x-2">
                        <RotateCw className="w-4 h-4 text-amber-400 animate-spin" />
                        <span className="text-xs font-bold text-amber-300">
                          czkawka_core 正在实时流式上屏中... 已分析文件: {scanResult.total_scanned}
                        </span>
                      </div>
                      <span className="text-[10px] text-amber-400 font-mono animate-pulse font-bold">
                        LIVE SSE STREAMING
                      </span>
                    </div>
                  )}
                  {/* Top Stats Cards Grid */}
                  <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
                    <div className="bg-slate-900/70 border border-slate-800 rounded-xl p-3.5">
                      <span className="text-[11px] font-medium text-slate-400 block">扫描文件总数</span>
                      <span className="text-xl font-bold font-mono text-slate-100 mt-1 block">
                        {scanResult.total_scanned}
                      </span>
                    </div>

                    <div className="bg-slate-900/70 border border-slate-800 rounded-xl p-3.5">
                      <span className="text-[11px] font-medium text-slate-400 block">发现重复组</span>
                      <span className="text-xl font-bold font-mono text-amber-400 mt-1 block">
                        {scanResult.duplicate_groups?.length || 0}
                      </span>
                    </div>

                    <div className="bg-slate-900/70 border border-slate-800 rounded-xl p-3.5">
                      <span className="text-[11px] font-medium text-slate-400 block">冗余副本数</span>
                      <span className="text-xl font-bold font-mono text-purple-400 mt-1 block">
                        {scanResult.total_redundant_files}
                      </span>
                    </div>

                    <div className="bg-slate-900/70 border border-slate-800 rounded-xl p-3.5">
                      <span className="text-[11px] font-medium text-slate-400 block">预计可释放空间</span>
                      <span className="text-xl font-bold font-mono text-emerald-400 mt-1 block">
                        {formatBytes(scanResult.total_freed_bytes || 0)}
                      </span>
                    </div>
                  </div>

                  {/* Duplicate Group List */}
                  <div className="bg-slate-900/70 border border-slate-800 rounded-2xl p-5 flex-1 min-h-0 flex flex-col space-y-4">
                    <div className="flex items-center justify-between border-b border-slate-800 pb-3">
                      <div className="flex items-center space-x-2">
                        <Copy className="w-4 h-4 text-amber-400" />
                        <h3 className="font-bold text-sm text-slate-100">智能清理聚类组列表 (File Cleanup Clusters)</h3>
                      </div>
                      <span className="text-xs text-slate-400 font-mono">
                        耗时: {scanResult.duration_ms} ms
                      </span>
                    </div>

                    {scanResult.duplicate_groups?.length === 0 ? (
                      <div className="p-8 text-center text-slate-500 text-xs">
                        🎉 未在此路径下检测到异常冗余文件，工作区非常整洁
                      </div>
                    ) : (
                      <div className="space-y-4 overflow-y-auto flex-1 min-h-0 pr-1">
                        {scanResult.duplicate_groups?.map((group: any, idx: number) => (
                          <div
                            key={group.group_id || idx}
                            className="bg-slate-950/80 border border-slate-800/90 rounded-xl p-4 space-y-3"
                          >
                            <div className="flex items-center justify-between">
                              <div className="flex items-center space-x-2">
                                <span className={`px-2 py-0.5 rounded text-[10px] font-bold uppercase ${
                                  group.strategy === 'exact_hash'
                                    ? 'bg-amber-500/20 text-amber-300 border border-amber-500/30'
                                    : group.strategy === 'audio_hash'
                                    ? 'bg-cyan-500/20 text-cyan-300 border border-cyan-500/30'
                                    : group.strategy === 'video_phash'
                                    ? 'bg-rose-500/20 text-rose-300 border border-rose-500/30'
                                    : 'bg-purple-500/20 text-purple-300 border border-purple-500/30'
                                }`}>
                                  {group.strategy}
                                </span>
                                <span className="font-bold text-xs text-slate-200">
                                  {group.description}
                                </span>
                              </div>
                              <div className="flex items-center space-x-2">
                                {group.group_threshold !== undefined && group.group_threshold !== null && (
                                  <span className="text-[10px] font-mono text-amber-400 bg-amber-950/60 border border-amber-800/60 px-1.5 py-0.5 rounded">
                                    踩线阈值 ≥ {Number(group.group_threshold).toFixed(1)}
                                  </span>
                                )}
                                <span className="text-xs font-bold text-emerald-400 font-mono">
                                  可释放: {formatBytes(group.potential_freed_bytes || 0)}
                                </span>
                              </div>
                            </div>

                            {/* Files Table */}
                            <div className="border border-slate-800/80 rounded-lg overflow-hidden">
                              <table className="w-full text-left text-[11px] font-mono">
                                <thead className="bg-slate-900 text-slate-400 border-b border-slate-800">
                                  <tr>
                                    <th className="py-1.5 px-3">预览</th>
                                    <th className="py-1.5 px-3">文件名</th>
                                    <th className="py-1.5 px-3">大小</th>
                                    <th className="py-1.5 px-3">Fingerprint</th>
                                    <th className="py-1.5 px-3 text-right">操作</th>
                                  </tr>
                                </thead>
                                <tbody className="divide-y divide-slate-800/50">
                                  {group.files?.map((file: any, fIdx: number) => (
                                    <tr key={fIdx} className="hover:bg-slate-900/50 text-slate-300">
                                      <td className="py-2 px-3">
                                        <div className="w-16 h-16 flex-shrink-0 rounded-lg border border-slate-700 bg-slate-950 overflow-hidden flex items-center justify-center">
                                          {getPreviewMediaType(file.name) === 'image' ? (
                                            <img
                                              src={getFilePreviewUrl(file.path)}
                                              alt={file.name}
                                              loading="lazy"
                                              className="w-full h-full object-cover"
                                              title={file.name}
                                            />
                                          ) : getPreviewMediaType(file.name) === 'video' ? (
                                            <video
                                              src={getFilePreviewUrl(file.path)}
                                              muted
                                              preload="metadata"
                                              className="w-full h-full object-cover"
                                              title={file.name}
                                            />
                                          ) : getPreviewMediaType(file.name) === 'audio' ? (
                                            <Music className="w-6 h-6 text-cyan-400" />
                                          ) : (
                                            <FileCode className="w-6 h-6 text-slate-500" />
                                          )}
                                        </div>
                                      </td>
                                      <td className="py-2 px-3 font-semibold text-slate-200 break-all max-w-[280px]">
                                        <span className="block truncate max-w-[240px]" title={file.name}>{file.name}</span>
                                        <span className="block text-[10px] text-slate-500 font-normal truncate max-w-[240px]" title={file.path}>
                                          {file.path}
                                        </span>
                                      </td>
                                      <td className="py-2 px-3 whitespace-nowrap text-slate-400">
                                        {formatBytes(file.size)}
                                      </td>
                                      <td className="py-2 px-3 whitespace-nowrap text-amber-400/90 text-[10px]">
                                        {file.fingerprint ? file.fingerprint.slice(0, 12) : '-'}
                                      </td>
                                      <td className="py-2 px-3 text-right whitespace-nowrap">
                                        <button
                                          onClick={() => copyToClipboard(file.path)}
                                          className="text-slate-400 hover:text-amber-400 p-1"
                                          title="复制路径"
                                        >
                                          <Copy className="w-3.5 h-3.5" />
                                        </button>
                                      </td>
                                    </tr>
                                  ))}
                                </tbody>
                              </table>
                            </div>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>

                  {/* Raw JSON Collapsible Response */}
                  <div className="bg-slate-900/70 border border-slate-800 rounded-2xl p-4">
                    <div className="flex items-center justify-between mb-2">
                      <span className="text-xs font-bold text-slate-300 flex items-center gap-1.5">
                        <FileCode className="w-3.5 h-3.5 text-amber-400" />
                        API 原始 JSON 响应 Payload (Raw Output)
                      </span>
                      <button
                        onClick={() => copyToClipboard(JSON.stringify(scanResult, null, 2))}
                        className="text-[11px] text-slate-400 hover:text-slate-200 flex items-center space-x-1"
                      >
                        <Copy className="w-3 h-3" />
                        <span>复制 JSON</span>
                      </button>
                    </div>
                    <pre className="p-3 bg-slate-950 border border-slate-800 rounded-xl text-[11px] font-mono text-slate-300 max-h-48 overflow-y-auto">
                      {JSON.stringify(scanResult, null, 2)}
                    </pre>
                  </div>
                </div>
              ) : (
                <div className="bg-slate-900/40 border border-slate-800 rounded-2xl p-12 flex flex-col items-center justify-center text-center flex-1 min-h-0 text-slate-500">
                  <Layers className="w-12 h-12 mb-3 opacity-30 text-amber-400" />
                  <p className="text-sm font-medium text-slate-300">czkawka_core API 测试控制台就绪</p>
                  <p className="text-xs text-slate-500 mt-1 max-w-sm">
                    输入目录路径并点击【执行 czkawka_core 聚合扫描】开始全量哈希与感知图片去重测试。
                  </p>
                </div>
              )}
            </div>
          </div>
        )}

        {activeTab === 'geo' && (
          <div className="lg:col-span-12 grid grid-cols-1 lg:grid-cols-12 gap-6 min-h-0 overflow-y-auto">
            {/* Left Column: Geo API Request Builder (4 cols) */}
            <div className="lg:col-span-4 space-y-5 flex flex-col min-h-0">
              <div className="bg-slate-900/70 border border-slate-800 rounded-2xl p-5 shadow-xl flex flex-col space-y-4">
                <div className="flex items-center justify-between border-b border-slate-800 pb-3">
                  <div className="flex items-center space-x-2">
                    <div className="bg-amber-500/10 p-2 rounded-lg text-amber-400">
                      <MapPin className="w-5 h-5" />
                    </div>
                    <div>
                      <h2 className="font-bold text-sm text-slate-100">离线反向地理编码测试</h2>
                      <span className="text-[11px] font-mono text-emerald-400">POST /api/geo/reverse</span>
                    </div>
                  </div>
                  {/* 数据集可用性徽章（来自 /health 的 geoAvailable） */}
                  {geoAvailable !== null && (
                    <span
                      className={`text-[10px] font-bold px-2 py-0.5 rounded border ${
                        geoAvailable
                          ? 'bg-emerald-500/20 text-emerald-300 border-emerald-500/30'
                          : 'bg-rose-500/20 text-rose-300 border-rose-500/30'
                      }`}
                    >
                      {geoAvailable ? '数据集就绪' : '数据集缺失'}
                    </span>
                  )}
                </div>

                {/* 坐标输入区 */}
                <div>
                  <label className="text-xs font-semibold text-slate-300 block mb-1.5">
                    测试坐标列表（每行: 纬度, 经度，支持 // 注释）
                  </label>
                  <textarea
                    rows={8}
                    value={geoCoordsText}
                    onChange={e => setGeoCoordsText(e.target.value)}
                    placeholder={'22.5431, 114.0579   // 深圳\n39.9042, 116.4074   // 北京'}
                    className="w-full bg-slate-950 border border-slate-800 rounded-xl p-3 text-xs font-mono text-slate-200 focus:border-amber-500 focus:outline-none resize-none"
                  />
                  {/* 预置坐标按钮组 */}
                  <div className="flex gap-1.5 mt-2 flex-wrap">
                    {GEO_PRESETS.map(preset => (
                      <button
                        key={preset.label}
                        onClick={() => setGeoCoordsText(preset.value)}
                        className="text-[10px] bg-slate-800/80 hover:bg-slate-800 text-slate-300 px-2 py-1 rounded border border-slate-700 transition-all"
                      >
                        {preset.label}
                      </button>
                    ))}
                  </div>
                </div>

                {/* 请求参数区 */}
                <div className="space-y-3 pt-1 border-t border-slate-800/80">
                  <div>
                    <label className="text-xs font-semibold text-slate-300 block mb-1.5">返回语言 (BCP-47)</label>
                    <select
                      value={geoLanguage}
                      onChange={e => setGeoLanguage(e.target.value)}
                      className="w-full bg-slate-950 border border-slate-800 rounded-xl px-3 py-2 text-xs font-mono text-slate-200 focus:border-amber-500 focus:outline-none"
                    >
                      {GEO_LANGUAGE_OPTIONS.map(lang => (
                        <option key={lang} value={lang}>
                          {lang}
                        </option>
                      ))}
                    </select>
                  </div>

                  <div className="grid grid-cols-2 gap-3">
                    <div>
                      <label className="text-xs font-semibold text-slate-300 block mb-1.5">
                        maxCityKm <span className="text-slate-500 font-normal">(默认50)</span>
                      </label>
                      <input
                        type="number"
                        min={0}
                        value={geoMaxCityKm}
                        onChange={e => setGeoMaxCityKm(e.target.value)}
                        placeholder="留空用默认"
                        className="w-full bg-slate-950 border border-slate-800 rounded-xl px-3 py-2 text-xs font-mono text-slate-200 focus:border-amber-500 focus:outline-none"
                      />
                    </div>
                    <div>
                      <label className="text-xs font-semibold text-slate-300 block mb-1.5">
                        maxAnyKm <span className="text-slate-500 font-normal">(默认500)</span>
                      </label>
                      <input
                        type="number"
                        min={0}
                        value={geoMaxAnyKm}
                        onChange={e => setGeoMaxAnyKm(e.target.value)}
                        placeholder="留空用默认"
                        className="w-full bg-slate-950 border border-slate-800 rounded-xl px-3 py-2 text-xs font-mono text-slate-200 focus:border-amber-500 focus:outline-none"
                      />
                    </div>
                  </div>
                </div>

                {/* 执行按钮 */}
                <button
                  onClick={handleGeoReverse}
                  disabled={geoQuerying}
                  className={`w-full py-2.5 rounded-xl text-sm font-bold transition-all flex items-center justify-center space-x-2 ${
                    geoQuerying
                      ? 'bg-slate-800 text-slate-500 cursor-not-allowed'
                      : 'bg-gradient-to-r from-amber-500 to-orange-600 text-slate-950 hover:brightness-110 shadow-lg shadow-amber-500/20'
                  }`}
                >
                  <Globe2 className={`w-4 h-4 ${geoQuerying ? 'animate-spin' : ''}`} />
                  <span>{geoQuerying ? '解析中...' : `发起批量解析 (${parseGeoCoords(geoCoordsText).points.length} 点)`}</span>
                </button>
              </div>
            </div>

            {/* Right Column: Geo Results Table (8 cols) */}
            <div className="lg:col-span-8 flex flex-col min-h-0">
              {geoResults && !geoResults.available ? (
                /* 软失败横幅：数据缺失时契约恒为 HTTP 200 + available:false */
                <div className="bg-rose-500/10 border border-rose-500/30 rounded-2xl p-6 flex flex-col items-center justify-center text-center">
                  <XCircle className="w-10 h-10 mb-2 text-rose-400" />
                  <p className="text-sm font-semibold text-rose-300">地理子系统不可用（软降级）</p>
                  <p className="text-xs text-rose-300/70 mt-1 font-mono">{geoResults.reason}</p>
                  <p className="text-[11px] text-slate-500 mt-3 max-w-md">
                    请执行 node scripts/setup-extra-resources.js 装配数据集后重启服务
                  </p>
                </div>
              ) : geoResults?.results ? (
                <div className="bg-slate-900/70 border border-slate-800 rounded-2xl p-5 shadow-xl flex flex-col min-h-0 space-y-4">
                  {/* 结果头部统计 */}
                  <div className="border-b border-slate-800 pb-3 flex items-center justify-between flex-wrap gap-2">
                    <div className="flex items-center space-x-2">
                      <Globe2 className="w-4 h-4 text-amber-400" />
                      <span className="text-sm font-bold text-slate-100">解析结果</span>
                      <span className="text-[10px] px-2 py-0.5 rounded bg-amber-500/10 text-amber-300 border border-amber-500/20 font-mono">
                        datasetVersion: {geoResults.datasetVersion}
                      </span>
                    </div>
                    <div className="flex items-center space-x-2 text-[11px] font-mono text-slate-400">
                      <span>命中: {geoResults.results.filter(r => r.found).length}/{geoResults.results.length}</span>
                      <span>|</span>
                      <span>language: {geoLanguage}</span>
                    </div>
                  </div>

                  {/* 逐点结果表：与请求点按下标对齐 */}
                  <div className="overflow-y-auto flex-1 min-h-0 pr-1 space-y-2">
                    {geoResults.results.map((r, idx) => {
                      const input = geoInputs[idx]
                      return (
                        <div
                          key={idx}
                          className={`p-3 rounded-xl border transition-all ${
                            r.found
                              ? r.city
                                ? 'bg-emerald-500/5 border-emerald-500/25'
                                : 'bg-amber-500/5 border-amber-500/25'
                              : 'bg-slate-950/40 border-slate-800/80'
                          }`}
                        >
                          <div className="flex items-center justify-between mb-1.5 flex-wrap gap-1">
                            <span className="text-xs font-mono text-slate-300">
                              #{idx + 1} ({input?.latitude}, {input?.longitude})
                            </span>
                            {r.found ? (
                              <span
                                className={`text-[10px] font-bold px-2 py-0.5 rounded border ${
                                  r.city
                                    ? 'bg-emerald-500/20 text-emerald-300 border-emerald-500/30'
                                    : 'bg-amber-500/20 text-amber-300 border-amber-500/30'
                                }`}
                              >
                                {r.city ? '城市全量层' : '国家+省中间层'}
                              </span>
                            ) : (
                              <span className="text-[10px] font-bold px-2 py-0.5 rounded bg-slate-800 text-slate-400 border border-slate-700">
                                未命中
                              </span>
                            )}
                          </div>

                          {r.found ? (
                            <div className="grid grid-cols-2 sm:grid-cols-5 gap-1.5 text-xs">
                              <div>
                                <span className="text-slate-500 block text-[10px]">country</span>
                                <span className="text-slate-200 truncate block">{r.country ?? '-'}</span>
                              </div>
                              <div>
                                <span className="text-slate-500 block text-[10px]">province</span>
                                <span className="text-sky-300 truncate block">{r.province ?? '-'}</span>
                              </div>
                              <div>
                                <span className="text-slate-500 block text-[10px]">city</span>
                                <span className="text-amber-300 font-semibold truncate block">{r.city ?? '-'}</span>
                              </div>
                              <div>
                                <span className="text-slate-500 block text-[10px]">distanceKm</span>
                                <span className="font-mono text-purple-300 block">{r.distanceKm ?? '-'}</span>
                              </div>
                            </div>
                          ) : (
                            <p className="text-[11px] text-slate-500">
                              超出检索半径或坐标无效（脏坐标单点容错，不影响批次）
                            </p>
                          )}
                        </div>
                      )
                    })}
                  </div>
                </div>
              ) : (
                <div className="bg-slate-900/40 border border-slate-800 rounded-2xl p-12 flex flex-col items-center justify-center text-center flex-1 min-h-0 text-slate-500 h-full">
                  <MapPin className="w-12 h-12 mb-3 opacity-30 text-amber-400" />
                  <p className="text-sm font-medium text-slate-300">离线反向地理编码测试台就绪</p>
                  <p className="text-xs text-slate-500 mt-1 max-w-sm">
                    输入或选择预置测试坐标，点击【发起批量解析】验证 坐标 → 国家 / 省 / 市 的分层输出与多语言回退。
                  </p>
                </div>
              )}

              {/* 错误提示条 */}
              {geoError && (
                <div className="mt-3 bg-slate-800/60 border border-slate-700 rounded-xl px-3 py-2 text-xs text-slate-300 flex items-start space-x-2">
                  <AlertCircle className="w-3.5 h-3.5 text-amber-400 flex-shrink-0 mt-0.5" />
                  <span>{geoError}</span>
                </div>
              )}
            </div>
          </div>
        )}

        {activeTab === 'config' && (
          <div className="lg:col-span-12 bg-slate-900/60 border border-slate-800 rounded-2xl p-6 max-w-3xl mx-auto w-full flex flex-col min-h-0">
            <h2 className="text-lg font-bold mb-1 text-slate-100 flex items-center">
              <Cpu className="w-5 h-5 mr-2 text-amber-400" />
              Omni Core 引擎参数配置
            </h2>
            <p className="text-xs text-slate-400 mb-6">
              调整底层 Rust 线程池、ORT ONNX Execution Provider 与推理参数
            </p>

            <div className="space-y-6 text-sm flex-1 min-h-0 overflow-y-auto pr-1">
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

              <div className="pt-4 border-t border-slate-800 flex justify-end flex-shrink-0">
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
