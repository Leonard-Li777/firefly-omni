import { PREVIEW_AUDIO_EXTS, PREVIEW_IMAGE_EXTS, PREVIEW_VIDEO_EXTS } from './constants'
import type { PreviewMediaType } from './types'

export const getFileExt = (name: string): string => {
  const idx = name.lastIndexOf('.')
  return idx >= 0 ? name.slice(idx + 1).toLowerCase() : ''
}

export const getPreviewMediaType = (name: string): PreviewMediaType => {
  const ext = getFileExt(name)
  if (PREVIEW_IMAGE_EXTS.includes(ext)) return 'image'
  if (PREVIEW_VIDEO_EXTS.includes(ext)) return 'video'
  if (PREVIEW_AUDIO_EXTS.includes(ext)) return 'audio'
  return null
}

export const getFilePreviewUrl = (path: string): string =>
  `/api/file/preview?path=${encodeURIComponent(path)}`
