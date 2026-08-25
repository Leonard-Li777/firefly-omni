/**
 * omni 依赖资源初始化与解压脚本 (setup-extra-resources.js)
 * 检查 extraResources/bin 目录中是否存在已解压文件，
 * 如果不存在，从 presetResources/ 找到匹配当前平台 (exiftool-bin-[platform]-[arch].[ext]) 的压缩包并解压。
 * 同时负责 omni-geo 地理数据集的两级装配：本地 presetResources 优先，缺失时从 GitHub Release 回退下载。
 */

const fs = require('fs')
const path = require('path')
const { spawnSync, execSync } = require('child_process')

function ensureDir(dirPath) {
  if (!fs.existsSync(dirPath)) {
    fs.mkdirSync(dirPath, { recursive: true })
    console.log(`✅ [omni-setup] 创建目录: ${dirPath}`)
  }
}

function getCurrentPlatform() {
  const platform = process.platform
  const arch = process.arch === 'x64' ? 'x64' : 'arm64'
  return { platform, arch }
}

function matchPlatformArchive(filename, toolName, platform, arch) {
  const lower = filename.toLowerCase()
  if (!lower.startsWith(`${toolName}-bin-`) && !lower.startsWith(`${toolName}-`)) return false
  const nameWithoutExt = lower.replace(/\.(zip|tar\.gz)$/, '')
  const parts = nameWithoutExt.split('-')
  if (parts.length < 4) return false

  const filePlatform = parts[2]
  const fileArch = parts[3]

  // 严格平台前缀匹配，防止 'darwin' 误匹配 'win'
  let platformMatched = false
  if (platform === 'win32') {
    platformMatched = filePlatform === 'win' || filePlatform === 'win32' || filePlatform === 'windows'
  } else if (platform === 'darwin') {
    platformMatched = filePlatform === 'darwin' || filePlatform === 'mac' || filePlatform === 'macos' || filePlatform === 'osx'
  } else if (platform === 'linux') {
    platformMatched = filePlatform === 'linux' || filePlatform === 'ubuntu'
  }

  if (!platformMatched) return false

  // 严格架构匹配
  const isTargetArm64 = arch === 'arm64' || arch === 'aarch64'
  const isFileArm64 = fileArch.includes('arm64') || fileArch.includes('aarch64')
  const isTargetX64 = arch === 'x64' || arch === 'x86_64' || arch === 'amd64'
  const isFileX64 = fileArch.includes('x64') || fileArch.includes('x86_64') || fileArch.includes('amd64')

  if (isTargetArm64 && isFileArm64) return true
  if (isTargetX64 && isFileX64) return true
  return false
}

async function extractZip(zipPath, destDir) {
  ensureDir(destDir)
  if (process.platform === 'win32') {
    try {
      execSync(`tar -xf "${zipPath}" -C "${destDir}"`, { windowsHide: true })
      console.log(`   - [系统 tar] 成功解压: ${path.basename(zipPath)}`)
      return
    } catch (tarErr) {
      try {
        execSync(
          `powershell -NoProfile -Command "Expand-Archive -Path '${zipPath}' -DestinationPath '${destDir}' -Force"`,
          { windowsHide: true }
        )
        console.log(`   - [PowerShell] 成功解压: ${path.basename(zipPath)}`)
        return
      } catch (psErr) {
        console.warn(`   ⚠️ 解压失败:`, psErr.message)
        throw psErr
      }
    }
  } else {
    try {
      const res = spawnSync('unzip', ['-q', '-o', zipPath, '-d', destDir])
      if (res.status === 0) {
        console.log(`   - [系统 unzip] 成功解压: ${path.basename(zipPath)}`)
        return
      }
    } catch (e) {}
  }
}

async function extractTarGz(tarPath, destDir) {
  ensureDir(destDir)
  const res = spawnSync('tar', ['-xzf', tarPath, '-C', destDir])
  if (res.status !== 0) {
    throw new Error(`tar 解压失败 ${tarPath}`)
  }
  console.log(`   - [系统 tar] 成功解压: ${path.basename(tarPath)}`)
}

/**
 * omni-geo 数据集目标仓库：环境变量显式指定 > git origin 解析 > 内置默认
 * 返回形如 "Leonard-Li777/firefly-omni" 的仓库标识
 */
function resolveGeoRepo() {
  if (process.env.OMNI_GEO_REPO) {
    return process.env.OMNI_GEO_REPO
  }
  try {
    const originUrl = execSync('git remote get-url origin', { cwd: path.resolve(__dirname, '..'), windowsHide: true })
      .toString()
      .trim()
    // 支持 https://github.com/<owner>/<repo>.git 与 git@github.com:<owner>/<repo>.git 两种形态
    const m = originUrl.match(/github\.com[/:]([\w.-]+\/[\w.-]+?)(?:\.git)?$/i)
    if (m) {
      return m[1]
    }
  } catch (e) {}
  return 'Leonard-Li777/firefly-omni'
}

/**
 * omni-geo 地理数据集装配（两级）：
 * 阶段一：build/presetResources/geo/geonames-resources.tar.gz 本地解压；
 * 阶段二：本地缺失时从 firefly-omni GitHub Release (tag: geo-data) 回退下载。
 * 全部落空仅告警不中断——地理子系统运行时以 available:false 软降级。
 */
async function setupOmniGeoDataset() {
  const omniRoot = path.resolve(__dirname, '..')
  const geoTargetDir = path.join(omniRoot, 'build', 'extraResources', 'geo')
  const datasetFile = path.join(geoTargetDir, 'geonames-compact.json')

  // 已装配 → 直接跳过（幂等）
  if (fs.existsSync(datasetFile)) {
    console.log('ℹ️ [omni-setup] [跳过已装配]: omni-geo 地理数据集已存在')
    return
  }

  ensureDir(geoTargetDir)
  let tarPath = path.join(omniRoot, 'build', 'presetResources', 'geo', 'geonames-resources.tar.gz')

  // 阶段一：本地分发包存在则直接使用
  if (!fs.existsSync(tarPath)) {
    // 阶段二：网络回退下载（firefly-omni 滚动 Release: geo-data）
    const repo = resolveGeoRepo()
    const tag = process.env.OMNI_GEO_RELEASE_TAG || 'geo-data'
    const url = `https://github.com/${repo}/releases/download/${tag}/geonames-resources.tar.gz`
    console.log(`⬇️ [omni-setup] 本地无地理数据包，开始下载: ${url}`)
    try {
      await downloadToFile(url, tarPath)
    } catch (err) {
      console.warn(`⚠️ [omni-setup] 地理数据集下载失败（服务将以不可用状态软降级）: ${err.message}`)
      return
    }
  }

  try {
    await extractTarGz(tarPath, geoTargetDir)
    console.log(`✅ [omni-setup] omni-geo 地理数据集装配完成！`)
  } catch (err) {
    console.warn(`⚠️ [omni-setup] 地理数据集解压失败（服务将以不可用状态软降级）: ${err.message}`)
  }
}

/** 简单的 HTTPS 下载器：跟随重定向写入目标文件 */
async function downloadToFile(url, dest) {
  const res = await fetch(url, { redirect: 'follow' })
  if (!res.ok) {
    throw new Error(`HTTP ${res.status}: ${url}`)
  }
  const buffer = Buffer.from(await res.arrayBuffer())
  fs.writeFileSync(dest, buffer)
}

async function setupOmniExtraResources() {
  const omniRoot = path.resolve(__dirname, '..')
  const presetDir = path.join(omniRoot, 'build', 'presetResources')
  const extraResourcesBin = path.join(omniRoot, 'build', 'extraResources', 'bin')

  ensureDir(extraResourcesBin)

  if (!fs.existsSync(presetDir)) {
    console.log(`ℹ️ [omni-setup] 预设资源目录不存在，跳过解压: ${presetDir}`)
    return
  }

  const { platform, arch } = getCurrentPlatform()

  console.log(`📦 [omni-setup] 检查并解压预设二进制工具包 (平台: ${platform}-${arch})...`)

  const tools = [
    { name: 'exiftool', exeName: platform === 'win32' ? 'exiftool.exe' : 'exiftool', displayName: 'ExifTool' },
    { name: 'ffmpeg', exeName: platform === 'win32' ? 'ffmpeg.exe' : 'ffmpeg', displayName: 'FFmpeg' },
    { name: 'ffprobe', exeName: platform === 'win32' ? 'ffprobe.exe' : 'ffprobe', displayName: 'FFprobe' },
  ]

  for (const tool of tools) {
    const toolPresetDir = path.join(presetDir, tool.name)
    if (!fs.existsSync(toolPresetDir)) {
      continue
    }

    const files = fs.readdirSync(toolPresetDir)
    const matchedFile = files.find(f => matchPlatformArchive(f, tool.name, platform, arch))

    if (matchedFile) {
      const srcPath = path.join(toolPresetDir, matchedFile)
      const ext = matchedFile.endsWith('.tar.gz') ? '.tar.gz' : path.extname(matchedFile)
      const zipName = path.basename(matchedFile, ext)

      const destDir = path.join(extraResourcesBin, tool.name)
      const readyFlag = path.join(destDir, `.ready-${zipName}`)
      const targetExe = path.join(destDir, tool.exeName)

      if (!fs.existsSync(targetExe) || !fs.existsSync(readyFlag)) {
        console.log(`📦 [omni-setup] 解压 [${matchedFile}] -> [${destDir}]...`)
        ensureDir(destDir)
        if (matchedFile.endsWith('.zip')) {
          await extractZip(srcPath, destDir)
        } else if (matchedFile.endsWith('.tar.gz')) {
          await extractTarGz(srcPath, destDir)
        }
        fs.writeFileSync(readyFlag, 'completed')
        console.log(`✅ [omni-setup] ${tool.displayName} 解压部署完成！`)
      } else {
        console.log(`ℹ️ [omni-setup] [跳过已解压]: ${matchedFile}`)
      }
    } else {
      console.warn(`⚠️ [omni-setup] 未发现匹配平台 (${platform}-${arch}) 的 ${tool.displayName} 预设包`)
    }
  }
}

if (require.main === module) {
  setupOmniExtraResources()
    .then(() => setupOmniGeoDataset())
    .catch(err => {
      console.error('❌ [omni-setup] 资源设置失败:', err)
      process.exit(1)
    })
}

module.exports = { setupOmniExtraResources, setupOmniGeoDataset }
