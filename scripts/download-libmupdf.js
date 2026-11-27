/**
 * 下载预编译 MuPDF 静态库脚本 (download-libmupdf.js)
 * 目的：从 firefly-omni 仓库的 ci-resources Release 下载预编译好的
 * libmupdf 静态库（libmupdf-out/：*.a / *.lib + include/），解压到
 * omni 的构建期缓存 apps/omni/build/presetResources/libmupdf/<suffix>/，
 * 供 Rust build script 以 MUPDF_LIB 环境变量引用（见 omni-pro-release.yml / omni-ce-release.yml）。
 *
 * 落点契约：docs/developer/architecture/multi-product-resource-placement-plan.md §4.4
 * —— presetResources 只有 libmupdf/<platform-arch> 与 libclang 两个合法子目录，
 * 旧的摊平布局（libmupdf 根部直接放 include 与 *.lib）与 build/libmupdf/<suffix>
 * 已废止，本脚本不再向那两处写入。
 *
 * 用法:
 *   node scripts/download-libmupdf.js --asset <asset> [--force]
 *
 * 参数:
 *   --asset   上游资产名（必填），如 libmupdf-windows-x86_64.zip
 *   --repo    仓库坐标（默认 Leonard-Li777/firefly-omni）
 *   --tag     Release tag（默认 ci-resources）
 *   --force   忽略已有目标目录强制重新下载部署
 *
 * 环境变量:
 *   GITHUB_TOKEN   私有/受限下载时的认证令牌（可选）
 *
 * 资产名 → 部署后缀映射：
 *   libmupdf-windows-x86_64.zip    → windows-x86_64
 *   libmupdf-macos-universal.tar.gz → macos-x86_64 + macos-aarch64（通用库双份）
 *   libmupdf-linux-x86_64.tar.gz   → linux-x86_64
 *   libmupdf-linux-aarch64.tar.gz  → linux-aarch64
 *
 * 下载失败时兜底：若单源缓存目录下已有任一平台后缀可用（例如
 * libmupdf/windows-x86_64），则复用它部署本次目标后缀，保证纯离线环境
 * 仍可完成同平台构建，不再依赖已废止的摊平 vendored 目录。
 */

const fs = require('fs')
const path = require('path')
const os = require('os')
const crypto = require('crypto')
const { spawnSync } = require('child_process')
const { Readable } = require('stream')

const OMNI_ROOT = path.resolve(__dirname, '..')
const DEFAULT_REPO = 'Leonard-Li777/firefly-omni'
const DEFAULT_TAG = 'ci-resources'
/** 构建期缓存根：presetResources/libmupdf（其下按平台后缀分目录） */
const MUPDF_CACHE_ROOT = path.join(OMNI_ROOT, 'build', 'presetResources', 'libmupdf')

// 资产名 → 目标部署后缀映射（宿主 omni-build matrix 的 artifact_suffix）
const SUFFIX_MAP = {
  'libmupdf-windows-x86_64.zip': ['windows-x86_64'],
  'libmupdf-macos-universal.tar.gz': ['macos-x86_64', 'macos-aarch64'],
  'libmupdf-linux-x86_64.tar.gz': ['linux-x86_64'],
  'libmupdf-linux-aarch64.tar.gz': ['linux-aarch64']
}

function log(msg) {
  // eslint-disable-next-line no-console
  console.log(`[libmupdf] ${msg}`)
}

function ensureDir(dir) {
  fs.mkdirSync(dir, { recursive: true })
}

/**
 * 判断目录内是否包含 MuPDF 静态库（*.a / *.lib）或其头文件目录
 * @param {string} dir 目录
 * @returns {boolean}
 */
function hasStaticLib(dir) {
  if (!fs.existsSync(dir)) return false
  try {
    const entries = fs.readdirSync(dir)
    return entries.some(
      f => /\.(a|lib)$/.test(f) || f === 'include'
    )
  } catch (e) {
    return false
  }
}

/**
 * 缓存目录是否**可用于构建**：静态库与规范嵌套头文件缺一不可。
 * 只判 `include/` 目录存在会让「头文件被摊平到根部」的残骸通过，随后
 * wrapper.c 的 `#include <mupdf/fitz.h>` 照样失败。
 * @param {string} dir 目录
 * @returns {boolean}
 */
function hasReadyMupdf(dir) {
  if (!hasStaticLib(dir)) return false
  return (
    fs.existsSync(path.join(dir, 'include', 'mupdf', 'fitz.h')) ||
    fs.existsSync(path.join(dir, 'mupdf', 'fitz.h'))
  )
}

/**
 * 递归复制目录内容（保持相对结构）
 * @param {string} src 源目录
 * @param {string} dest 目标目录
 */
function copyDirSync(src, dest) {
  ensureDir(dest)
  for (const entry of fs.readdirSync(src)) {
    const s = path.join(src, entry)
    const d = path.join(dest, entry)
    if (fs.statSync(s).isDirectory()) {
      copyDirSync(s, d)
    } else {
      fs.copyFileSync(s, d)
    }
  }
}

/**
 * 解压压缩包
 * @param {string} archive 压缩包路径
 * @param {string} destDir 解压目标
 */
function extractArchive(archive, destDir) {
  ensureDir(destDir)
  const isTarGz = archive.endsWith('.tar.gz')
  const isZip = archive.endsWith('.zip')
  let res = null
  if (isTarGz) {
    res = spawnSync('tar', ['-xzf', archive, '-C', destDir], { stdio: 'pipe' })
    if (res.status === 0) {
      log(`解压成功 (tar.gz): ${path.basename(archive)}`)
      return
    }
    // 备用 7z 解压 tar.gz
    const res7z = spawnSync('7z', ['x', archive, `-so`], { stdio: ['pipe', 'pipe', 'pipe'] })
    if (res7z.status === 0 && res7z.stdout) {
      const resTar = spawnSync('tar', ['-xf', '-', '-C', destDir], { input: res7z.stdout, stdio: ['pipe', 'pipe', 'pipe'] })
      if (resTar.status === 0) {
        log(`解压成功 (tar.gz, 7z+tar): ${path.basename(archive)}`)
        return
      }
    }
    throw new Error(`tar 解压失败: ${archive}（${(res.stderr || res.stdout || '').toString().trim()}）`)
  }
  if (isZip) {
    if (process.platform === 'win32') {
      // 1. 尝试 7z（查找常见安装路径）
      const possible7z = [
        '7z',
        'C:\\Program Files\\7-Zip\\7z.exe',
        'C:\\Program Files (x86)\\7-Zip\\7z.exe',
        'C:\\ProgramData\\chocolatey\\bin\\7z.exe'
      ]
      for (const cmd7z of possible7z) {
        try {
          res = spawnSync(cmd7z, ['x', archive, `-o${destDir}`, '-y'], { stdio: 'pipe' })
          if (res.status === 0) {
            log(`解压成功 (zip, ${cmd7z}): ${path.basename(archive)}`)
            return
          }
        } catch (e) {
          // 继续尝试下一个
        }
      }

      // 2. 尝试系统 tar
      try {
        res = spawnSync('tar', ['-xf', archive, '-C', destDir], { stdio: 'pipe' })
        if (res.status === 0) {
          log(`解压成功 (zip, 系统 tar): ${path.basename(archive)}`)
          return
        }
      } catch (e) {
        // 继续尝试下一个
      }

      // 3. 尝试 PowerShell 原生 .NET ZipFile 解压（最稳定、无第三方依赖）
      const safeArchive = archive.replace(/'/g, "''").replace(/\\/g, '/')
      const safeDest = destDir.replace(/'/g, "''").replace(/\\/g, '/')
      const psScript = `
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $dest = '${safeDest}'
        if (!(Test-Path $dest)) { New-Item -ItemType Directory -Path $dest -Force | Out-Null }
        $zip = [System.IO.Compression.ZipFile]::OpenRead('${safeArchive}')
        foreach ($entry in $zip.Entries) {
          $targetPath = [System.IO.Path]::Combine($dest, $entry.FullName)
          if ($entry.FullName.EndsWith('/') -or $entry.FullName.EndsWith('\\')) {
            New-Item -ItemType Directory -Path $targetPath -Force | Out-Null
          } else {
            $parentDir = [System.IO.Path]::GetDirectoryName($targetPath)
            if (!(Test-Path $parentDir)) { New-Item -ItemType Directory -Path $parentDir -Force | Out-Null }
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $targetPath, $true)
          }
        }
        $zip.Dispose()
      `
      res = spawnSync('powershell', ['-NoProfile', '-Command', psScript], { stdio: 'pipe' })
      if (res.status === 0) {
        log(`解压成功 (zip, PowerShell .NET ZipFile): ${path.basename(archive)}`)
        return
      }

      const errMsg = (res.stderr || res.stdout || (res.error ? res.error.message : '') || '').toString().trim()
      throw new Error(`zip 解压失败: ${archive}（${errMsg}）`)
    }

    // 非 Windows 优先尝试 unzip，其次 7z / tar
    res = spawnSync('unzip', ['-q', '-o', archive, '-d', destDir], { stdio: 'pipe' })
    if (res.status === 0) {
      log(`解压成功 (zip, unzip): ${path.basename(archive)}`)
      return
    }
    res = spawnSync('7z', ['x', archive, `-o${destDir}`, '-y'], { stdio: 'pipe' })
    if (res.status === 0) {
      log(`解压成功 (zip, 7z): ${path.basename(archive)}`)
      return
    }
    res = spawnSync('tar', ['-xf', archive, '-C', destDir], { stdio: 'pipe' })
    if (res.status === 0) {
      log(`解压成功 (zip, tar): ${path.basename(archive)}`)
      return
    }
    const errMsg = (res.stderr || res.stdout || (res.error ? res.error.message : '') || '').toString().trim()
    throw new Error(`zip 解压失败: ${archive}（${errMsg}）`)
  }
  throw new Error(`不支持的压缩包格式: ${archive}`)
}

/**
 * 摊平解压结果：资产包内含 libmupdf-out/ 单顶层目录，晋升其内容
 * 使 *.a / *.lib 与 include/ 直接位于部署目标下。
 * @param {string} extractRoot 解压根目录
 * @returns {string} 实际静态库所在目录
 */
function flattenLibRoot(extractRoot) {
  let src = extractRoot
  const entries = fs.readdirSync(src)
  const only = entries.length === 1 ? path.join(src, entries[0]) : null
  if (only && fs.statSync(only).isDirectory() && hasStaticLib(only)) {
    // 资产包裹了单一顶层目录（如 libmupdf-out/）
    src = only
  }
  if (!hasStaticLib(src)) {
    throw new Error(
      `解压结果中未找到静态库（*.a/*.lib）或 include/ 目录：${extractRoot}（内容：${fs.readdirSync(extractRoot).join(', ')}）`
    )
  }
  return src
}

/**
 * 部署解析后的静态库目录到指定 suffix
 * @param {string} libRoot 静态库实际目录
 * @param {string} suffix 目标后缀（如 linux-x86_64）
 * @returns {string[]} 部署的库文件列表
 */
function deployTo(libRoot, suffix) {
  const target = path.join(MUPDF_CACHE_ROOT, suffix)
  copyDirSync(libRoot, target)
  const libs = fs.readdirSync(target).filter(f => /\.(a|lib)$/.test(f))
  log(`✅ 已部署 ${suffix}/（${libs.join(', ') || '仅头文件'}）`)
  return libs
}

/**
 * 本地兜底：若单源缓存下已有任一平台后缀可用，则复用它部署本次目标后缀。
 * 这是「离线可用」而非「兼容旧布局」——只读单源目录，不再接受摊平残留。
 * @param {string[]} suffixes 目标后缀列表
 * @returns {boolean} 是否兜底成功
 */
function deployLocalFallback(suffixes) {
  if (!fs.existsSync(MUPDF_CACHE_ROOT)) {
    return false
  }
  const readyDirs = fs
    .readdirSync(MUPDF_CACHE_ROOT, { withFileTypes: true })
    .filter(e => e.isDirectory() && hasReadyMupdf(path.join(MUPDF_CACHE_ROOT, e.name)))
    .map(e => e.name)
  if (readyDirs.length === 0) {
    return false
  }
  const source = path.join(MUPDF_CACHE_ROOT, readyDirs[0])
  log(`⚠️ 下载不可用，复用单源缓存 ${readyDirs[0]}/ 作为兜底部署源`)
  for (const suffix of suffixes) {
    if (readyDirs.includes(suffix)) continue
    deployTo(source, suffix)
  }
  return true
}

/**
 * 下载到文件（优先使用 gh CLI，回退使用原生 fetch stream）
 * @param {string} url 下载地址
 * @param {string} destPath 落盘路径
 * @param {string} repo 仓库
 * @param {string} tag 标签
 * @param {string} asset 资产名
 * @returns {Promise<void>}
 */
async function downloadToFile(url, destPath, repo, tag, asset) {
  const destDir = path.dirname(destPath)
  // 1. 优先尝试 gh CLI（CI 中原生认证与重定向处理）
  try {
    const gh = spawnSync('gh', ['release', 'download', tag, '--pattern', asset, '--repo', repo, '--dir', destDir, '--clobber'], {
      stdio: 'inherit',
      env: process.env
    })
    if (gh.status === 0 && fs.existsSync(destPath)) {
      return
    }
  } catch (e) {
    // gh 缺失时继续尝试 fetch
  }

  // 2. 原生 fetch 兜底
  const headers = {}
  const token = process.env.GITHUB_TOKEN || process.env.GH_TOKEN
  if (token) {
    headers.Authorization = `token ${token}`
  }
  const res = await fetch(url, { headers, redirect: 'follow' })
  if (!res.ok) {
    throw new Error(`HTTP ${res.status}: ${res.statusText}`)
  }
  const writer = fs.createWriteStream(destPath)
  await new Promise((resolve, reject) => {
    Readable.fromWeb(res.body).pipe(writer)
    writer.on('finish', resolve)
    writer.on('error', reject)
  })
}

function parseArgs(argv) {
  const out = {}
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    const m = a.match(/^--([\w-]+)(?:=(.*))?$/)
    if (!m) continue
    const key = m[1].replace(/-/g, '')
    const next = argv[i + 1]
    out[key] = m[2] || (typeof next === 'string' && !next.startsWith('--') ? argv[++i] : true)
  }
  return out
}

async function main() {
  const args = parseArgs(process.argv.slice(2))
  const resolveAsset = args.resolvedir
  if (resolveAsset) {
    if (!SUFFIX_MAP[resolveAsset]) {
      console.error(`❌ 未知资产名: ${resolveAsset}`)
      process.exit(1)
    }
    const candidates = SUFFIX_MAP[resolveAsset]
    const requestedSuffix = args.archsuffix
    const chosen =
      requestedSuffix && candidates.includes(requestedSuffix) ? requestedSuffix : candidates[0]
    console.log(path.join(MUPDF_CACHE_ROOT, chosen))
    return
  }
  const asset = args.asset
  if (!asset) {
    log('用法: node scripts/download-libmupdf.js --asset <asset> [--force]')
    process.exit(1)
  }
  if (!SUFFIX_MAP[asset]) {
    log(`❌ 未知资产名: ${asset}`)
    log(`   支持: ${Object.keys(SUFFIX_MAP).join(', ')}`)
    process.exit(1)
  }
  const suffixes = SUFFIX_MAP[asset]
  const repo = args.repo || process.env.OMNI_REPO || DEFAULT_REPO
  const tag = args.tag || DEFAULT_TAG
  const force = !!args.force

  // 幂等：目标 suffix 均已就位且未强制 → 跳过
  if (!force) {
    const ready = suffixes.filter(s => hasReadyMupdf(path.join(MUPDF_CACHE_ROOT, s)))
    if (ready.length === suffixes.length) {
      log(`ℹ️ 目标已就位，跳过下载（${ready.join(', ')}）；使用 --force 可重新部署`)
      return
    }
  }

  const url = `https://github.com/${repo}/releases/download/${tag}/${asset}`
  const tmpRoot = path.join(os.tmpdir(), `libmupdf-${crypto.createHash('md5').update(asset).digest('hex').slice(0, 12)}`)
  const archivePath = path.join(tmpRoot, asset)
  const extractRoot = path.join(tmpRoot, 'extracted')

  try {
    ensureDir(tmpRoot)
    log(`📥 下载 ${url}`)
    await downloadToFile(url, archivePath, repo, tag, asset)
    log(`   完成，大小 ${(fs.statSync(archivePath).size / 1048576).toFixed(1)} MiB`)
    extractArchive(archivePath, extractRoot)
    const libRoot = flattenLibRoot(extractRoot)
    for (const suffix of suffixes) {
      deployTo(libRoot, suffix)
      // 落位后立即校验：只判「有 .lib」会放过缺头文件的半成品，
      // 那种目录在下一轮 cargo check 才炸，且会被误判为「已就位」跳过重下。
      if (!hasReadyMupdf(path.join(MUPDF_CACHE_ROOT, suffix))) {
        throw new Error(
          `部署后校验失败：${suffix}/ 缺少静态库或 include/mupdf/fitz.h（目标 ${path.join(MUPDF_CACHE_ROOT, suffix)}）`
        )
      }
    }
  } catch (err) {
    log(`❌ 下载/部署失败: ${err.message}`)
    if (deployLocalFallback(suffixes)) {
      log('⚠️ 已复用单源缓存完成兜底部署（离线构建路径）')
      return
    }
    process.exit(1)
  } finally {
    try {
      fs.rmSync(tmpRoot, { recursive: true, force: true })
    } catch (e) {
      /* 临时目录清理失败不影响主流程 */
    }
  }
}

main().catch(err => {
  log(`执行失败: ${err.stack || err.message}`)
  process.exit(1)
})