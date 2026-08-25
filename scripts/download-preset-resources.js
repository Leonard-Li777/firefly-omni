const fs = require('fs')
const path = require('path')
const os = require('os')
const { execSync, spawnSync } = require('child_process')
const axios = require('axios')

// 适配 proxy 工具
let detectProxy, createAxiosProxyConfig, isNetworkError, createProxyAgent
try {
  const proxyUtils = require('../../../scripts/utils/proxy-utils')
  detectProxy = proxyUtils.detectProxy
  createAxiosProxyConfig = proxyUtils.createAxiosProxyConfig
  isNetworkError = proxyUtils.isNetworkError
  createProxyAgent = proxyUtils.createProxyAgent
} catch (e) {
  detectProxy = () => process.env.HTTP_PROXY || process.env.HTTPS_PROXY || process.env.ALL_PROXY || null
  createAxiosProxyConfig = (proxyUrl, headers = {}) => {
    const config = { headers }
    if (proxyUrl) {
      const { HttpsProxyAgent } = require('https-proxy-agent')
      config.httpsAgent = new HttpsProxyAgent(proxyUrl)
      config.proxy = false
    }
    return config
  }
  isNetworkError = err => Boolean(err && (err.code || !err.response))
  createProxyAgent = proxyUrl => {
    const { HttpsProxyAgent } = require('https-proxy-agent')
    return new HttpsProxyAgent(proxyUrl)
  }
}

/**
 * Omni 预设资源自动下载脚本 (download-preset-resources.js)
 * 目的：当本地缺失 stable 静态资源 (exiftool, ffmpeg, ffprobe 预设包) 时，
 * 从 GitHub ci-resources Release 中自动拉取并解压到 presetResources 目录。
 */

const REPO_OWNER = process.env.OMNI_REPO_OWNER || 'Leonard-Li777'
const REPO_NAME = process.env.OMNI_REPO_NAME || 'firefly-omni'
const TAG_NAME = 'ci-resources'

const OMNI_ROOT = path.resolve(__dirname, '..')
const PRESET_DIR = path.join(OMNI_ROOT, 'build', 'presetResources')

// 自动探测并加载 .env
const envCandidates = [
  path.resolve(__dirname, '../../../.env'),
  path.resolve(__dirname, '../../../.env.production'),
  path.join(OMNI_ROOT, '.env')
]

for (const p of envCandidates) {
  if (fs.existsSync(p)) {
    require('dotenv').config({ path: p })
    break
  }
}

function log(msg) {
  console.log(`[${new Date().toLocaleTimeString()}] ${msg}`)
}

function ensureDir(dir) {
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true })
  }
}

async function requestWithProxy(url, config, retried = false) {
  const proxyUrl = detectProxy()
  const mergedConfig = { ...config }
  if (proxyUrl) {
    mergedConfig.httpsAgent = createProxyAgent(proxyUrl)
    mergedConfig.proxy = false
  }

  try {
    return await axios.get(url, mergedConfig)
  } catch (error) {
    if (!retried && isNetworkError(error)) {
      if (proxyUrl) {
        log('🔄 检测到网络错误，尝试使用代理重试...')
        return await axios.get(url, mergedConfig)
      }
    }
    throw error
  }
}

async function downloadFile(url, destPath, token, label = '') {
  const headers = { Accept: 'application/octet-stream' }
  if (token) headers['Authorization'] = `token ${token}`
  const proxyUrl = detectProxy()
  const config = createAxiosProxyConfig(proxyUrl, headers)
  config.responseType = 'arraybuffer'
  config.timeout = 600000

  log(`⬇️ 正在下载 ${label || path.basename(destPath)}...`)
  const response = await axios.get(url, config)
  fs.writeFileSync(destPath, response.data)
  log(`✅ 下载完成: ${path.basename(destPath)} (${(response.data.length / 1024 / 1024).toFixed(2)} MB)`)
}

async function extractZip(zipPath, destDir) {
  ensureDir(destDir)
  log(`📦 解压 ${path.basename(zipPath)} -> ${destDir}...`)
  if (process.platform === 'win32') {
    execSync(`tar -xf "${zipPath}" -C "${destDir}"`, { windowsHide: true })
  } else {
    spawnSync('unzip', ['-q', '-o', zipPath, '-d', destDir])
  }
}

async function downloadPresetResources() {
  ensureDir(PRESET_DIR)
  const exiftoolDir = path.join(PRESET_DIR, 'exiftool')
  const ffmpegDir = path.join(PRESET_DIR, 'ffmpeg')
  const ffprobeDir = path.join(PRESET_DIR, 'ffprobe')

  const hasExiftool = fs.existsSync(exiftoolDir) && fs.readdirSync(exiftoolDir).length > 0
  const hasFfmpeg = fs.existsSync(ffmpegDir) && fs.readdirSync(ffmpegDir).length > 0
  const hasFfprobe = fs.existsSync(ffprobeDir) && fs.readdirSync(ffprobeDir).length > 0

  if (hasExiftool && hasFfmpeg && hasFfprobe) {
    log('✨ 检测到本地 omni 所有 presetResources 预设包均已就绪，跳过下载。')
    return
  }

  const token = process.env.GITHUB_TOKEN
  if (!token) {
    log('⚠️ 未检测到 GITHUB_TOKEN，跳过从 GitHub Release 拉取预设资源。')
    return
  }

  log(`🚀 检查并从 GitHub [${REPO_OWNER}/${REPO_NAME}] Release [${TAG_NAME}] 下载缺失的预设资源...`)

  const apiUrl = `https://api.github.com/repos/${REPO_OWNER}/${REPO_NAME}/releases/tags/${TAG_NAME}`
  const headers = { Accept: 'application/vnd.github.v3+json', Authorization: `token ${token}` }
  const proxyUrl = detectProxy()
  const apiConfig = createAxiosProxyConfig(proxyUrl, headers)

  let release
  try {
    const res = await requestWithProxy(apiUrl, apiConfig)
    release = res.data
  } catch (err) {
    log(`⚠️ 无法获取 Release [${TAG_NAME}]: ${err.message}`)
    return
  }

  const tasks = [
    { name: 'exiftool-resources.zip', destDir: exiftoolDir, needed: !hasExiftool },
    { name: 'ffmpeg-resources.zip', destDir: ffmpegDir, needed: !hasFfmpeg },
    { name: 'ffprobe-resources.zip', destDir: ffprobeDir, needed: !hasFfprobe }
  ]

  for (const t of tasks) {
    if (!t.needed) continue
    const asset = release.assets?.find(a => a.name === t.name)
    if (asset) {
      const tempZip = path.join(os.tmpdir(), `omni-${t.name}`)
      try {
        await downloadFile(asset.url, tempZip, token, asset.name)
        await extractZip(tempZip, t.destDir)
        log(`✅ ${t.name} 恢复完成！`)
      } finally {
        if (fs.existsSync(tempZip)) fs.unlinkSync(tempZip)
      }
    } else {
      log(`ℹ️ Release 中未找到 ${t.name}`)
    }
  }
}

if (require.main === module) {
  downloadPresetResources().catch(err => {
    console.error('❌ 下载预设资源失败:', err)
    process.exit(1)
  })
}

module.exports = { downloadPresetResources }
