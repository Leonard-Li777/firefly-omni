const fs = require('fs')
const path = require('path')
const https = require('https')
const { Transform } = require('stream')
const axios = require('axios')
const readline = require('readline')
const archiver = require('archiver')

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
 * Omni 静态资源统一上传脚本 (upload-ci-static-resources.js)
 * 目的：将 omni 稳定预设资源打包并按目标分组上传至 GitHub Release
 * (Leonard-Li777/firefly-omni)，作为 CI/CD 与运行时资源回退的稳定发布源。
 *
 * Release 标签分组：
 *   - ci-resources : exiftool / ffmpeg / ffprobe 二进制工具包 (zip)
 *   - geo-data     : omni-geo 地理数据集分发包 (滚动更新 tar.gz)
 */

const REPO_OWNER = process.env.OMNI_REPO_OWNER || 'Leonard-Li777'
const REPO_NAME = process.env.OMNI_REPO_NAME || 'firefly-omni'
const DEFAULT_TAG = 'ci-resources'

const OMNI_ROOT = path.resolve(__dirname, '..')
const ASSETS_DIR = path.join(OMNI_ROOT, 'build', 'presetResources')

// 待上传的资源清单（tag 缺省为 DEFAULT_TAG；contentType 缺省为 application/zip）
const TARGETS = [
  {
    name: 'exiftool-resources.zip',
    source: path.join(ASSETS_DIR, 'exiftool'),
    type: 'directory'
  },
  {
    name: 'ffmpeg-resources.zip',
    source: path.join(ASSETS_DIR, 'ffmpeg'),
    type: 'directory'
  },
  {
    name: 'ffprobe-resources.zip',
    source: path.join(ASSETS_DIR, 'ffprobe'),
    type: 'directory'
  },
  {
    name: 'geonames-resources.tar.gz',
    source: path.join(ASSETS_DIR, 'geo', 'geonames-resources.tar.gz'),
    type: 'file',
    tag: 'geo-data',
    contentType: 'application/gzip',
    releaseName: 'Omni Geo Dataset (geo-data)',
    releaseBody:
      'omni-geo 离线反向地理编码数据集（GeoNames cities500 裁剪版）。\n\n' +
      '包含: geonames-compact.json (运行时紧凑数据集), VERSION (版本号)\n' +
      '许可: 数据源自 GeoNames.org，遵循 CC-BY 4.0 许可。'
  }
]

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

async function requestWithProxy(method, url, data = null, config = {}, retried = false) {
  const doRequest = async proxyUrl => {
    const mergedConfig = { ...config }
    if (proxyUrl) {
      mergedConfig.httpsAgent = createProxyAgent(proxyUrl)
      mergedConfig.proxy = false
    }
    if (data && (method === 'post' || method === 'delete')) {
      return await axios[method](url, data, mergedConfig)
    }
    return await axios[method](url, mergedConfig)
  }

  try {
    return await doRequest()
  } catch (error) {
    if (!retried && isNetworkError(error)) {
      const proxyUrl = detectProxy()
      if (proxyUrl) {
        log('🔄 检测到网络错误，尝试使用代理重试...')
        return await doRequest(proxyUrl)
      }
    }
    throw error
  }
}

const GITHUB_TOKEN = process.env.GITHUB_TOKEN

if (!GITHUB_TOKEN) {
  console.error('❌ 错误: 未找到 GITHUB_TOKEN，请确保在 .env 中配置或注入 GITHUB_TOKEN 环境变量')
  process.exit(1)
}

const proxyUrl = detectProxy()
const apiConfig = createAxiosProxyConfig(proxyUrl, {
  Authorization: `token ${GITHUB_TOKEN}`,
  'User-Agent': 'firefly-omni-ci-uploader'
})

function askQuestion(query) {
  const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout
  })

  return new Promise(resolve =>
    rl.question(query, ans => {
      rl.close()
      resolve(ans)
    })
  )
}

async function selectTargets() {
  const isCI = process.env.CI === 'true' || process.argv.includes('--all') || process.argv.includes('-y')
  if (isCI) {
    log('🤖 检测到 CI 或 --all 参数，自动上传全部资源')
    return TARGETS
  }

  console.log('\n=========================================')
  console.log(`请选择要上传到 [${REPO_OWNER}/${REPO_NAME}] 的包：`)
  TARGETS.forEach((target, index) => {
    const status = fs.existsSync(target.source) ? '✅ 本地存在' : '❌ 本地不存在'
    const tag = target.tag || DEFAULT_TAG
    console.log(`[${index + 1}] ${target.name} (${status}, Release: ${tag})`)
  })
  console.log('[A] 全部上传')
  console.log('[Q] 退出')
  console.log('=========================================\n')

  const answer = await askQuestion('请输入选择 (默认: A): ')
  const cleanAnswer = answer.trim().toUpperCase()

  if (cleanAnswer === 'Q') {
    log('🚪 用户退出操作。')
    process.exit(0)
  }

  if (cleanAnswer === 'A' || cleanAnswer === '') {
    return TARGETS
  }

  const selectedIndices = cleanAnswer
    .split(/[\s,，]+/)
    .map(val => parseInt(val, 10) - 1)
    .filter(idx => !isNaN(idx) && idx >= 0 && idx < TARGETS.length)

  if (selectedIndices.length === 0) {
    console.log('⚠️ 无效的选择，请重新选择。')
    return selectTargets()
  }

  return selectedIndices.map(idx => TARGETS[idx])
}

function zipDirectory(source, outPath) {
  return new Promise((resolve, reject) => {
    log(`📦 正在压缩 ${source} -> ${outPath}...`)
    const output = fs.createWriteStream(outPath)
    const archive = archiver('zip', { zlib: { level: 0 } })

    output.on('close', () => {
      log(`✅ 压缩完成: ${outPath} (${archive.pointer()} 字节)`)
      resolve()
    })

    archive.on('error', err => {
      reject(new Error(`压缩失败: ${err.message}`))
    })

    archive.pipe(output)
    archive.directory(source, false)
    archive.finalize()
  })
}

async function uploadAsset(uploadUrl, filePath, name, token, contentType = 'application/zip', maxRetries = 3) {
  const cleanUploadUrl = uploadUrl.replace(/\{(\?name,label)?\}/, '')
  const targetUrl = `${cleanUploadUrl}?name=${encodeURIComponent(name)}`
  const fileSize = fs.statSync(filePath).size

  log(`🚀 开始上传 ${name} (${(fileSize / 1024 / 1024).toFixed(2)} MB)...`)

  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      await new Promise((resolve, reject) => {
        const fileStream = fs.createReadStream(filePath)
        let uploadedBytes = 0
        let lastReportTime = Date.now()

        const progressTracker = new Transform({
          transform(chunk, encoding, callback) {
            uploadedBytes += chunk.length
            const now = Date.now()
            if (now - lastReportTime > 2000 || uploadedBytes === fileSize) {
              lastReportTime = now
              const percent = ((uploadedBytes / fileSize) * 100).toFixed(1)
              const mb = (uploadedBytes / 1024 / 1024).toFixed(1)
              const totalMb = (fileSize / 1024 / 1024).toFixed(1)
              process.stdout.write(`\r   ⏳ 上传进度: ${mb}MB / ${totalMb}MB (${percent}%)`)
            }
            callback(null, chunk)
          }
        })

        const urlObj = new URL(targetUrl)
        const proxyUrl = detectProxy()
        let agent = null
        if (proxyUrl) {
          agent = createProxyAgent(proxyUrl)
        }

        const options = {
          protocol: urlObj.protocol,
          hostname: urlObj.hostname,
          port: urlObj.port || (urlObj.protocol === 'https:' ? 443 : 80),
          path: urlObj.pathname + urlObj.search,
          method: 'POST',
          agent: agent,
          headers: {
            Authorization: `token ${token}`,
            // 按资产类型区分：zip 工具包 / gzip 数据集包
            'Content-Type': contentType,
            'Content-Length': fileSize,
            'User-Agent': 'firefly-omni-ci-uploader',
            Accept: 'application/vnd.github.v3+json'
          },
          timeout: 600000
        }

        const req = https.request(options, res => {
          let body = ''
          res.on('data', chunk => (body += chunk))
          res.on('end', () => {
            process.stdout.write('\n')
            if (res.statusCode >= 200 && res.statusCode < 300) {
              resolve(body)
            } else {
              reject(new Error(`HTTP ${res.statusCode}: ${body}`))
            }
          })
        })

        req.on('error', err => {
          process.stdout.write('\n')
          reject(err)
        })

        fileStream.pipe(progressTracker).pipe(req)
      })

      log(`🎉 ${name} 上传成功！`)
      return
    } catch (err) {
      log(`⚠️ 第 ${attempt}/${maxRetries} 次上传 ${name} 失败: ${err.message}`)
      if (attempt < maxRetries) {
        log('🔄 3 秒后重试...')
        await new Promise(r => setTimeout(r, 3000))
      } else {
        throw new Error(`❌ ${name} 上传失败，已达最大重试次数: ${err.message}`)
      }
    }
  }
}

/**
 * 确保指定标签的 Release 存在并返回（带缓存，同一标签只请求一次）
 */
const releaseCache = new Map()
async function getOrCreateRelease(tag, target) {
  if (releaseCache.has(tag)) {
    return releaseCache.get(tag)
  }

  log(`🚀 正在连接 GitHub API (仓库: ${REPO_OWNER}/${REPO_NAME}, 标签: ${tag})...`)
  let release
  try {
    const res = await requestWithProxy(
      'get',
      `https://api.github.com/repos/${REPO_OWNER}/${REPO_NAME}/releases/tags/${tag}`,
      null,
      apiConfig
    )
    release = res.data
    log(`✅ 找到现有 Release: ${release.name || tag} (ID: ${release.id})`)
  } catch (err) {
    if (err.response && err.response.status === 404) {
      log(`ℹ️ 未找到 ${tag} Release，正在创建...`)
      const createRes = await requestWithProxy(
        'post',
        `https://api.github.com/repos/${REPO_OWNER}/${REPO_NAME}/releases`,
        {
          tag_name: tag,
          name: target.releaseName || `Omni CI Preset Resources (${tag})`,
          body: target.releaseBody || 'Omni 核心引擎跨平台预设二进制工具包 (ExifTool, FFmpeg, FFprobe)。',
          draft: false,
          prerelease: false
        },
        apiConfig
      )
      release = createRes.data
      log(`✅ 创建 Release 成功！(ID: ${release.id})`)
    } else {
      throw err
    }
  }

  releaseCache.set(tag, release)
  return release
}

async function main() {
  const selectedTargets = await selectTargets()

  // 按 Release 标签分组：不同目标的资产归属各自独立的滚动 Release
  const groups = new Map()
  for (const target of selectedTargets) {
    const tag = target.tag || DEFAULT_TAG
    if (!groups.has(tag)) {
      groups.set(tag, [])
    }
    groups.get(tag).push(target)
  }

  const tempFiles = []

  try {
    for (const [tag, targets] of groups) {
      // 组内取首个含 Release 元数据的目标作为创建说明来源
      const metaSource = targets.find(t => t.releaseName || t.releaseBody) || targets[0]
      const release = await getOrCreateRelease(tag, metaSource)

      for (const target of targets) {
        log(`\n=========================================`)
        log(`📦 正在处理: ${target.name}`)

        if (!fs.existsSync(target.source)) {
          log(`⚠️ 资源源不存在，跳过: ${target.source}`)
          continue
        }

        let filePathToUpload
        if (target.type === 'directory') {
          const tempZip = path.join(OMNI_ROOT, `temp-${target.name}`)
          await zipDirectory(target.source, tempZip)
          filePathToUpload = tempZip
          tempFiles.push(tempZip)
        } else {
          filePathToUpload = target.source
        }

        // 滚动发布：检查 Release 中是否已存在同名资产，有则先删除
        const existingAsset = release.assets.find(a => a.name === target.name)
        if (existingAsset) {
          log(`🗑️ 发现已存在的旧资产 (ID: ${existingAsset.id})，正在删除...`)
          await requestWithProxy('delete', existingAsset.url, null, apiConfig)
          log(`✅ 旧资产已删除`)
        }

        await uploadAsset(
          release.upload_url,
          filePathToUpload,
          target.name,
          GITHUB_TOKEN,
          target.contentType || 'application/zip'
        )
      }
    }

    log(`\n✨ 所有选定的 omni 预设资源已成功发布到 GitHub Release！`)
  } finally {
    for (const file of tempFiles) {
      if (fs.existsSync(file)) {
        try {
          fs.unlinkSync(file)
        } catch (e) {}
      }
    }
  }
}

if (require.main === module) {
  main().catch(err => {
    console.error('❌ 执行失败:', err)
    process.exit(1)
  })
}

module.exports = { main }
