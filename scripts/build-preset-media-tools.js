/**
 * 跨平台预设资源打包生成脚本 (build-preset-media-tools.js)
 * 自动为 Windows (x64), macOS (arm64, x64), Linux (x64, arm64) 构建并生成
 * exiftool, ffmpeg, ffprobe 预设压缩包至 apps/omni/build/presetResources/
 */

const fs = require('fs')
const path = require('path')
const os = require('os')
const { execSync, spawnSync } = require('child_process')
const axios = require('axios')

// 适配从 monorepo root 或 apps/omni 独立运行
let detectProxy, createAxiosProxyConfig
try {
  const proxyUtils = require('../../../scripts/utils/proxy-utils')
  detectProxy = proxyUtils.detectProxy
  createAxiosProxyConfig = proxyUtils.createAxiosProxyConfig
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
}

const OMNI_ROOT = path.resolve(__dirname, '..')
const OMNI_PRESET_DIR = path.join(OMNI_ROOT, 'build', 'presetResources')
const TEMP_DIR = path.join(os.tmpdir(), 'firefly-preset-build')

function ensureDir(dir) {
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true })
  }
}

async function downloadBuffer(url) {
  const proxyUrl = detectProxy()
  const config = createAxiosProxyConfig(proxyUrl, { 'User-Agent': 'NodeJS' })
  config.responseType = 'arraybuffer'
  config.timeout = 60000
  const res = await axios.get(url, config)
  return res.data
}

async function getNpmTarballUrl(pkgName) {
  const proxyUrl = detectProxy()
  const registries = ['https://registry.npmmirror.com', 'https://registry.npmjs.org']

  for (const reg of registries) {
    try {
      const url = `${reg}/${pkgName}`
      const config = createAxiosProxyConfig(proxyUrl, { 'User-Agent': 'NodeJS' })
      config.timeout = 10000
      const res = await axios.get(url, config)
      const latest = res.data['dist-tags']?.latest
      if (latest && res.data.versions?.[latest]?.dist?.tarball) {
        return res.data.versions[latest].dist.tarball
      }
    } catch (e) {}
  }
  throw new Error(`无法获取 npm 包 [${pkgName}] 的 tarball 地址`)
}

async function extractTarGz(tarPath, destDir) {
  ensureDir(destDir)
  if (process.platform === 'win32') {
    execSync(`tar -xzf "${tarPath}" -C "${destDir}"`, { windowsHide: true })
  } else {
    spawnSync('tar', ['-xzf', tarPath, '-C', destDir])
  }
}

function createZipFromDir(sourceDir, destZip) {
  ensureDir(path.dirname(destZip))
  if (fs.existsSync(destZip)) {
    fs.unlinkSync(destZip)
  }
  if (process.platform === 'win32') {
    execSync(
      `powershell -NoProfile -Command "Compress-Archive -Path '${sourceDir}\\*' -DestinationPath '${destZip}' -Force"`,
      { windowsHide: true }
    )
  } else {
    execSync(`cd "${sourceDir}" && zip -r -q "${destZip}" ./*`)
  }
}

function createZipFromFile(filePath, destZip) {
  ensureDir(path.dirname(destZip))
  if (fs.existsSync(destZip)) {
    fs.unlinkSync(destZip)
  }
  if (process.platform === 'win32') {
    execSync(
      `powershell -NoProfile -Command "Compress-Archive -Path '${filePath}' -DestinationPath '${destZip}' -Force"`,
      { windowsHide: true }
    )
  } else {
    const dir = path.dirname(filePath)
    const name = path.basename(filePath)
    execSync(`cd "${dir}" && zip -q "${destZip}" "${name}"`)
  }
}

async function buildFfmpegAndFfprobe() {
  const platforms = [
    { target: 'win-x64', ffmpegPkg: '@ffmpeg-installer/win32-x64', ffprobePkg: '@ffprobe-installer/win32-x64', exeSuffix: '.exe' },
    { target: 'darwin-arm64', ffmpegPkg: '@ffmpeg-installer/darwin-arm64', ffprobePkg: '@ffprobe-installer/darwin-arm64', exeSuffix: '' },
    { target: 'darwin-x64', ffmpegPkg: '@ffmpeg-installer/darwin-x64', ffprobePkg: '@ffprobe-installer/darwin-x64', exeSuffix: '' },
    { target: 'linux-x64', ffmpegPkg: '@ffmpeg-installer/linux-x64', ffprobePkg: '@ffprobe-installer/linux-x64', exeSuffix: '' },
    { target: 'linux-arm64', ffmpegPkg: '@ffmpeg-installer/linux-arm64', ffprobePkg: '@ffprobe-installer/linux-arm64', exeSuffix: '' },
  ]

  const ffmpegPresetDir = path.join(OMNI_PRESET_DIR, 'ffmpeg')
  const ffprobePresetDir = path.join(OMNI_PRESET_DIR, 'ffprobe')
  ensureDir(ffmpegPresetDir)
  ensureDir(ffprobePresetDir)

  for (const item of platforms) {
    console.log(`\n📦 [Preset Build] 正在处理平台 [${item.target}] 的 ffmpeg 与 ffprobe...`)

    // 1. ffmpeg
    const ffmpegDestZip = path.join(ffmpegPresetDir, `ffmpeg-bin-${item.target}.zip`)
    try {
      const tarUrl = await getNpmTarballUrl(item.ffmpegPkg)
      console.log(`   - 下载 ${item.ffmpegPkg} (${tarUrl})...`)
      const buf = await downloadBuffer(tarUrl)
      const tempTar = path.join(TEMP_DIR, `${item.ffmpegPkg.replace('/', '-')}.tgz`)
      const extractFolder = path.join(TEMP_DIR, `${item.ffmpegPkg.replace('/', '-')}-extracted`)
      ensureDir(TEMP_DIR)
      fs.writeFileSync(tempTar, buf)
      await extractTarGz(tempTar, extractFolder)

      const exeName = `ffmpeg${item.exeSuffix}`
      const binaryPath = path.join(extractFolder, 'package', exeName)
      if (fs.existsSync(binaryPath)) {
        createZipFromFile(binaryPath, ffmpegDestZip)
        console.log(`   ✅ 成功生成: ${path.basename(ffmpegDestZip)} (${(fs.statSync(ffmpegDestZip).size / 1024 / 1024).toFixed(2)} MB)`)
      } else {
        console.warn(`   ⚠️ 未在包内找到 ${exeName}: ${binaryPath}`)
      }
    } catch (err) {
      console.error(`   ❌ 处理 ffmpeg [${item.target}] 失败:`, err.message)
    }

    // 2. ffprobe
    const ffprobeDestZip = path.join(ffprobePresetDir, `ffprobe-bin-${item.target}.zip`)
    try {
      const tarUrl = await getNpmTarballUrl(item.ffprobePkg)
      console.log(`   - 下载 ${item.ffprobePkg} (${tarUrl})...`)
      const buf = await downloadBuffer(tarUrl)
      const tempTar = path.join(TEMP_DIR, `${item.ffprobePkg.replace('/', '-')}.tgz`)
      const extractFolder = path.join(TEMP_DIR, `${item.ffprobePkg.replace('/', '-')}-extracted`)
      ensureDir(TEMP_DIR)
      fs.writeFileSync(tempTar, buf)
      await extractTarGz(tempTar, extractFolder)

      const exeName = `ffprobe${item.exeSuffix}`
      const binaryPath = path.join(extractFolder, 'package', exeName)
      if (fs.existsSync(binaryPath)) {
        createZipFromFile(binaryPath, ffprobeDestZip)
        console.log(`   ✅ 成功生成: ${path.basename(ffprobeDestZip)} (${(fs.statSync(ffprobeDestZip).size / 1024 / 1024).toFixed(2)} MB)`)
      } else {
        console.warn(`   ⚠️ 未在包内找到 ${exeName}: ${binaryPath}`)
      }
    } catch (err) {
      console.error(`   ❌ 处理 ffprobe [${item.target}] 失败:`, err.message)
    }
  }
}

async function buildExifTool() {
  const exiftoolPresetDir = path.join(OMNI_PRESET_DIR, 'exiftool')
  ensureDir(exiftoolPresetDir)

  console.log(`\n📦 [Preset Build] 正在处理 ExifTool 跨平台预设资源...`)

  // 1. Windows x64: 已有 exiftool-bin-win-x64.zip
  const winZip = path.join(exiftoolPresetDir, 'exiftool-bin-win-x64.zip')
  if (fs.existsSync(winZip)) {
    console.log(`   ℹ️ [跳过已存在]: exiftool-bin-win-x64.zip (${(fs.statSync(winZip).size / 1024 / 1024).toFixed(2)} MB)`)
  }

  // 2. macOS & Linux: 下载 exiftool-vendored.pl (含 Unix Perl 核心与标准 lib)
  try {
    const pkgName = 'exiftool-vendored.pl'
    const tarUrl = await getNpmTarballUrl(pkgName)
    console.log(`   - 下载 ${pkgName} (${tarUrl})...`)
    const buf = await downloadBuffer(tarUrl)
    const tempTar = path.join(TEMP_DIR, `${pkgName}.tgz`)
    const extractFolder = path.join(TEMP_DIR, `${pkgName}-extracted`)
    ensureDir(TEMP_DIR)
    fs.writeFileSync(tempTar, buf)
    await extractTarGz(tempTar, extractFolder)

    // package/bin/exiftool 与 package/bin/lib
    const packageBinDir = path.join(extractFolder, 'package', 'bin')
    if (fs.existsSync(packageBinDir)) {
      const unixTargets = [
        'exiftool-bin-darwin-arm64.zip',
        'exiftool-bin-darwin-x64.zip',
        'exiftool-bin-mac-arm64.zip',
        'exiftool-bin-mac-x64.zip',
        'exiftool-bin-linux-x64.zip',
        'exiftool-bin-linux-arm64.zip',
      ]

      for (const targetZipName of unixTargets) {
        const destZip = path.join(exiftoolPresetDir, targetZipName)
        createZipFromDir(packageBinDir, destZip)
        console.log(`   ✅ 成功生成: ${targetZipName} (${(fs.statSync(destZip).size / 1024 / 1024).toFixed(2)} MB)`)
      }
    }
  } catch (err) {
    console.error(`   ❌ 处理 ExifTool Unix 包失败:`, err.message)
  }
}

async function main() {
  console.log('🚀 开始构建 omni presetResources 预设压缩包...')
  ensureDir(OMNI_PRESET_DIR)
  ensureDir(TEMP_DIR)

  await buildFfmpegAndFfprobe()
  await buildExifTool()

  console.log('\n🎉 omni 所有平台的 presetResources 压缩包已准备完毕！')
  console.log(`📁 目录: ${OMNI_PRESET_DIR}`)
}

if (require.main === module) {
  main().catch(err => {
    console.error('❌ 构建失败:', err)
    process.exit(1)
  })
}

module.exports = { main }
