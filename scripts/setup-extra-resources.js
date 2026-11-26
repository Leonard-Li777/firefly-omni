/**
 * omni 依赖资源初始化脚本 (setup-extra-resources.js)
 *
 * 职责已收敛为「地理数据集装配」一件事：omni-geo 本地 presetResources 优先，
 * 缺失时经 consume-preset-resources.js 从 firefly-resources 统一资源管线拉取
 * （两步认证下载 + sha256 校验）。
 *
 * ffmpeg / ffprobe / exiftool 三个共享 tool 资产自 2026-11-25 起不再由本脚本维护，
 * 唯一装配入口是 desktop 的 `pro/scripts/setup-extra-resources.js`（见 setupOmniExtraResources 注释）。
 */

const fs = require('fs')
const path = require('path')
const { spawnSync } = require('child_process')

function ensureDir(dirPath) {
  if (!fs.existsSync(dirPath)) {
    fs.mkdirSync(dirPath, { recursive: true })
    console.log(`✅ [omni-setup] 创建目录: ${dirPath}`)
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
 * 本地候选分发包（阶段一）：仅探测 omni 自身 `build/presetResources/geo` 目录（不再跨仓穿透 desktop）。
 */
function resolveLocalGeoArchives(omniRoot) {
  const presetDirs = [path.join(omniRoot, 'build', 'presetResources', 'geo')]
  const archives = []
  for (const dir of presetDirs) {
    if (!fs.existsSync(dir)) continue
    for (const f of fs.readdirSync(dir)) {
      if (f.endsWith('.tar.gz') || f.endsWith('.zip')) {
        archives.push(path.join(dir, f))
      }
    }
  }
  archives.sort((a, b) => {
    const aHist = path.basename(a).startsWith('geonames-resources')
    const bHist = path.basename(b).startsWith('geonames-resources')
    if (aHist !== bHist) return aHist ? -1 : 1
    return path.basename(b).localeCompare(path.basename(a))
  })
  return archives
}

/**
 * 通过统一资源仓库 `apps/firefly-resources` 下载最新 geo 数据包至 `omni/build/presetResources/geo/`。
 * 彻底消除跨仓反向穿透 `../../desktop/pro/scripts` 的耦合。
 * @returns {Promise<boolean>}
 */
async function consumeGeoFromFireflyResources() {
  const resourcesRoot = path.resolve(__dirname, '../..', 'firefly-resources')
  const indexLibPath = path.join(resourcesRoot, 'lib', 'index.js')
  if (!fs.existsSync(indexLibPath)) {
    console.warn(`⚠️ [omni-setup] 未找到同级 firefly-resources 模块，跳过在线拉取 geo: ${resourcesRoot}`)
    return false
  }
  const indexLib = require(indexLibPath)
  const { authHeaders } = require(path.join(resourcesRoot, 'lib', 'auth.js'))
  const { downloadFile } = require(path.join(resourcesRoot, 'lib', 'fetch.js'))

  const indexPath = path.join(resourcesRoot, 'index.json')
  const index = indexLib.readIndexFile(indexPath)
  const ver = indexLib.resolveLatestVersion(index, 'geo')
  const asset = ver && index.resources?.geo?.versions?.[ver]?.default
  if (!asset) {
    console.warn('⚠️ [omni-setup] firefly-resources/index.json 中未声明 geo 默认资产')
    return false
  }

  const destDir = path.join(__dirname, '..', 'build', 'presetResources', 'geo')
  ensureDir(destDir)
  const destPath = path.join(destDir, asset.name)
  if (fs.existsSync(destPath)) return true

  console.log(`⬇️ [omni-setup] 从 firefly-resources 拉取 geo@${ver} (${asset.name}) ...`)
  try {
    const repo = 'Leonard-Li777/firefly-resources'
    const apiRes = await fetch(`https://api.github.com/repos/${repo}/releases/tags/resources`, {
      headers: authHeaders({ 'user-agent': 'omni-geo-setup', accept: 'application/vnd.github+json' })
    })
    if (!apiRes.ok) {
      throw new Error(`release 查询失败: ${apiRes.status}`)
    }
    const rel = await apiRes.json()
    const hit = (rel.assets || []).find(a => a.name === asset.name)
    if (!hit) {
      throw new Error(`release 中不存在资产 ${asset.name}`)
    }
    await downloadFile(
      hit.url,
      destPath,
      authHeaders({ 'user-agent': 'omni-geo-setup', accept: 'application/octet-stream' }),
      asset.sha256
    )
    return true
  } catch (err) {
    console.warn(`⚠️ [omni-setup] 拉取 geo 失败（服务将以不可用状态软降级）: ${err.message}`)
    return false
  }
}

/**
 * omni-geo 地理数据集装配（两级，纯开发辅助）：
 * 阶段一：本地 `omni/build/presetResources/geo/*.tar.gz` 存在则直接解压；
 * 阶段二：本地缺失时经同级 `firefly-resources` 拉取后解压。
 * @param {{ dryRun?: boolean }} [opts]
 */
async function setupOmniGeoDataset() {
  const omniRoot = path.resolve(__dirname, '..')
  const geoTargetDir = path.join(omniRoot, 'build', 'extraResources', 'geo')
  const datasetFile = path.join(geoTargetDir, 'geonames-compact.json')

  if (fs.existsSync(datasetFile)) {
    console.log('ℹ️ [omni-setup] [跳过已装配]: omni-geo 地理数据集已存在')
    return
  }

  ensureDir(geoTargetDir)

  let localArchives = resolveLocalGeoArchives(omniRoot)
  let tarPath = localArchives[0]
  if (tarPath) {
    console.log(`📦 [omni-setup] 使用本地地理数据包: ${tarPath}`)
  } else {
    const consumed = await consumeGeoFromFireflyResources()
    if (!consumed) return
    localArchives = resolveLocalGeoArchives(omniRoot)
    tarPath = localArchives[0]
    if (!tarPath) {
      console.warn('⚠️ [omni-setup] geo 下载完成但未在 preset 目录发现归档（软降级）')
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

async function setupOmniExtraResources() {
  const omniRoot = path.resolve(__dirname, '..')
  const presetDir = path.join(omniRoot, 'build', 'presetResources')
  const extraResourcesBin = path.join(omniRoot, 'build', 'extraResources', 'bin')

  ensureDir(extraResourcesBin)

  // 已退役（2026-11-25）：omni 不再自行解压/维护 ffmpeg / ffprobe / exiftool。
  //
  // 这三个是 firefly-resources 的共享 tool 资产，唯一装配入口是 desktop 的
  // `pro/scripts/setup-extra-resources.js`（落到 `build/extraResources/bin/<tool>/<exe>`）。
  // omni 侧的旧实现解压的是已退役 npm 包产物（2018 年 ffmpeg/ffprobe 构建），
  // 且 `scripts/omni-build-to-desktop.js` 会把它覆盖回 desktop，形成 ADR-0029 禁止的越界冗余。
  // 运行态定位已由 `czkawka_bridge::ensure_media_tools_in_path` 改为按资源相对路径探测
  // （`resources/bin/<tool>/<exe>` / `build/extraResources/bin/<tool>/<exe>`），无需本脚本落盘。
  console.log(
    'ℹ️ [omni-setup] ffmpeg / ffprobe / exiftool 由 desktop 统一装配，omni 侧不再解压维护'
  )
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
