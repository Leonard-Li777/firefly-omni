// ---- 离线反向地理编码 (/api/geo/reverse) 相关类型 ----
export interface GeoPointResult {
  found: boolean
  country?: string | null
  province?: string | null
  city?: string | null
  distanceKm?: number | null
}

export interface GeoReverseResponse {
  available: boolean
  datasetVersion?: number
  results?: GeoPointResult[]
  reason?: string
}

export interface TagChainItem {
  code: string
  name: string
  confidence: number
  /** 所属受控父级 code（三分区契约，用于互斥语义判定） */
  parent_code?: string
}

export interface CandidateHypothesisItem {
  hypothesis: string
  score: number
  cosine_global?: number
  cosine_desc?: number
  has_cross_modal?: boolean
  slot_breakdown?: Record<string, string>
}

export interface ApiResponseData {
  file_path?: string
  mime_type?: string
  file_size?: number
  markdown_content?: string
  ocr_text?: string
  metadata?: any
  phash?: string
  is_corrupted?: boolean
  // 三阶段全模态融合结果字段
  fused_tags?: TagChainItem[]
  visual_tags?: TagChainItem[]
  candidate_hypotheses?: CandidateHypothesisItem[]
  winning_hypothesis?: string
  smart_name?: string
  content_description?: string
  photo_type?: string
  quality_score?: number
  aesthetic_score?: number
  has_watermark?: boolean
  watermark_level?: number
  has_mosaic?: boolean
  mosaic_level?: number
  security_level?: string
  workflow_state?: string
  geo_address?: string
  benchmark?: any
}

export interface ExtractionResult {
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

export type PreviewMediaType = 'image' | 'video' | 'audio' | null
