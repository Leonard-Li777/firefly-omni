// 预置测试坐标组：覆盖城市层 / 中间层 / 海洋未命中 / 脏坐标容错四类场景
export const GEO_PRESETS: Array<{ label: string; value: string }> = [
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
export const GEO_LANGUAGE_OPTIONS = [
  'en',
  'zh-CN',
  'ja-JP',
  'ko-KR',
  'fr-FR',
  'de-DE',
  'es-ES',
  'ru-RU',
  'pt-PT',
  'ar-EG'
]

// ---- 浏览器可直接预览的多模态文件扩展名集合 (czkawka 结果缩略图) ----
export const PREVIEW_IMAGE_EXTS = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg', 'avif']
export const PREVIEW_VIDEO_EXTS = ['mp4', 'm4v', 'webm', 'ogv', 'ogg', 'mov']
export const PREVIEW_AUDIO_EXTS = ['mp3', 'wav', 'flac', 'aac', 'm4a', 'opus']
