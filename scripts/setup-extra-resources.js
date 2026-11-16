/**
 * omni 依赖资源初始化与解压脚本 (setup-extra-resources.js)
 * 检查 extraResources/bin 目录中是否存在已解压文件，
 * 如果不存在，从 presetResources/ 找到匹配当前平台 (exiftool-bin-[platform]-[arch].[ext]) 的压缩包并解压。
 * 同时负责 omni-geo 地理数据集的两级装配：本地 presetResources 优先，缺失时经
 * consume-preset-resources.js 从 firefly-resources 统一资源管线拉取（两步认证下载 + sha256 校验）。
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
 * 本地候选分发包（阶段一）：omni 自身 preset 优先，其次 monorepo 内 desktop 侧 preset。
 * 背景：资源统一装配流水线（consume-preset-resources.js）按 firefly-resources 标准把
 * geo-<版本>.tar.gz 下载到 apps/desktop/build/presetResources/geo/，apps/omni 自身
 * presetResources/ 历史上从未被写入 geo 目录 —— 因此阶段二改为直接调用该消费流水线
 * 从 firefly-resources 拉取，落盘后即命中本函数第二候选，避免再依赖已 404 的旧 Release。
 */
function resolveLocalGeoArchives(omniRoot) {
  const presetDirs = [
    path.join(omniRoot, 'build', 'presetResources', 'geo'),
    path.resolve(omniRoot, '..', '..', 'apps', 'desktop', 'build', 'presetResources', 'geo')
  ]
  const archives = []
  for (const dir of presetDirs) {
    if (!fs.existsSync(dir)) continue
    for (const f of fs.readdirSync(dir)) {
      // 兼容 geonames-resources.tar.gz（历史命名）与 geo-<版本>.tar.gz（firefly-resources 标准命名）
      if (f.endsWith('.tar.gz') || f.endsWith('.zip')) {
        archives.push(path.join(dir, f))
      }
    }
  }
  // 优先历史命名（语义更明确），其余按版本号新→旧
  archives.sort((a, b) => {
    const aHist = path.basename(a).startsWith('geonames-resources')
    const bHist = path.basename(b).startsWith('geonames-resources')
    if (aHist !== bHist) return aHist ? -1 : 1
    return path.basename(b).localeCompare(path.basename(a))
  })
  return archives
}

/**
 * 从 firefly-resources 统一资源管线消费 geo 数据包。
 * 复用 consume-preset-resources.js 的 lock 驱动 + 两步认证下载 + sha256 校验，
 * 落盘到 desktop/build/presetResources/geo/（即 resolveLocalGeoArchives 第二候选）。
 * 只消费 geo 一项（--resource geo）。BOM 重定向到 omni 专属路径：consumer 的 engines 段
 * 始终按当前平台全量重建，若写 desktop 的共享 BOM 会在跨平台构建时把 win32 资产换成
 * 当前平台资产，污染 desktop 全量清单——geo 是 data 资源，与引擎 BOM 无关，故隔离。
 * 生产/CI 强制 sha 校验，开发态沿用 consumer 的缓存命中与截断门禁逻辑。
 * @returns {boolean} 消费是否成功（退出码 0）
 */
function consumeGeoFromFireflyResources() {
  const consumerScript = path.resolve(
    __dirname,
    '../..',
    'desktop',
    'pro',
    'scripts',
    'consume-preset-resources.js'
  )
  if (!fs.existsSync(consumerScript)) {
    console.warn(`⚠️ [omni-setup] 未找到资源消费脚本，无法从 firefly-resources 拉取 geo: ${consumerScript}`)
    return false
  }
  // 独立 BOM 避免写 desktop 共享 BOM；落盘 preset 目录仍用默认（desktop 缓存，供阶段一命中）
  const omniGeoBom = path.join(__dirname, '..', 'build', 'extraResources', 'configs', 'geo-resources.bom.json')
  ensureDir(path.dirname(omniGeoBom))
  console.log('⬇️ [omni-setup] 本地无地理数据包，从 firefly-resources 消费 geo ...')
  const res = spawnSync(process.execPath, [consumerScript, '--resource', 'geo', '--bom', omniGeoBom], {
    cwd: path.resolve(__dirname, '../..'),
    windowsHide: true,
    encoding: 'utf8'
  })
  if (res.error) {
    console.warn(`⚠️ [omni-setup] 调用资源消费脚本失败: ${res.error.message}`)
    return false
  }
  if (res.status !== 0) {
    const tail = `${res.stderr || res.stdout || ''}`.trim().split('\n').slice(-8).join('\n')
    console.warn(
      `⚠️ [omni-setup] firefly-resources geo 消费失败（退出码 ${res.status}，服务将以不可用状态软降级）:\n${tail}`
    )
    return false
  }
  return true
}

/**
 * omni-geo 地理数据集装配（两级）：
 * 阶段一：本地 presetResources/geo/*.tar.gz 存在则直接解压；
 * 阶段二：本地缺失时经 consume-preset-resources.js 从 firefly-resources 拉取后解压。
 * 全部落空仅告警不中断——地理子系统运行时以 available:false 软降级；
 * 但消费/解压失败必须显式告警原因，严禁静默吞掉（AGENTS.md §2 禁止静默降级）。
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

  // 阶段一：本地分发包存在则直接使用（omni preset 或 desktop preset）
  let localArchives = resolveLocalGeoArchives(omniRoot)
  let tarPath = localArchives[0]
  if (tarPath) {
    console.log(`📦 [omni-setup] 使用本地地理数据包: ${tarPath}`)
  } else {
    // 阶段二：经统一资源管线从 firefly-resources 拉取，落盘后重新扫描本地候选
    const consumed = consumeGeoFromFireflyResources()
    if (!consumed) return
    localArchives = resolveLocalGeoArchives(omniRoot)
    tarPath = localArchives[0]
    if (!tarPath) {
      console.warn(
        '⚠️ [omni-setup] geo 消费成功但未找到落盘的地理数据包（服务将以不可用状态软降级）'
      )
      return
    }
    console.log(`📦 [omni-setup] 使用 firefly-resources 拉取的地理数据包: ${tarPath}`)
  }

  try {
    await extractTarGz(tarPath, geoTargetDir)
    console.log(`✅ [omni-setup] omni-geo 地理数据集装配完成！`)
  } catch (err) {
    console.warn(`⚠️ [omni-setup] 地理数据集解压失败（服务将以不可用状态软降级）: ${err.message}`)
  }
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
  // --geo-only 仅供 omni:deploy 以同步子进程方式单独装配 geo（避免 async 竞态）；
  // 只跑 geo 装配，不解压 ffmpeg/exiftool（deploy 有独立的 bin 复制段）。
  if (process.argv.includes('--geo-only')) {
    setupOmniGeoDataset().catch(err => {
      console.error('❌ [omni-setup] 地理数据集装配失败:', err)
      process.exit(1)
    })
  } else {
    setupOmniExtraResources()
      .then(() => setupOmniGeoDataset())
      .catch(err => {
        console.error('❌ [omni-setup] 资源设置失败:', err)
        process.exit(1)
      })
  }
}

module.exports = { setupOmniExtraResources, setupOmniGeoDataset }
