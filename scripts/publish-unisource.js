'use strict'

/**
 * 统一资源中心（firefly-resources）多平台发布脚本
 * 供 omni-pro-release.yml 聚合 job 与本地手动发布使用：
 *   扫描构建产物目录 → 按文件名映射平台键 → 组装 manifest → publish-resources.js 上传+更新 index → 提交推送 index.json。
 *
 * 用法：
 *   node scripts/publish-unisource.js <distDir> <version> [--fr <firefly-resources目录>] [--dry-run]
 *
 * 环境变量：
 *   GH_TOKEN   用于 gh CLI（上传资产）与 git push（更新 index.json，需对 firefly-resources 有推送权限）
 *
 * 产物命名规范（与 omni-pro-release.yml 打包一致）：
 *   firefly-omni-windows-x86_64.zip / firefly-omni-macos-x86_64.tar.gz / firefly-omni-linux-aarch64.tar.gz 等
 * 将映射为统一资源中心平台键：win32-x64 / win32-arm64 / darwin-x64 / darwin-arm64 / linux-x64 / linux-arm64。
 * 未收集到的平台（如实验性构建失败的 windows-arm64）自动跳过，不影响其它平台发布。
 */

const fs = require('fs')
const path = require('path')
const { spawnSync } = require('child_process')

// 资产后缀 → 统一资源中心平台键 / 归档扩展名
const SUFFIX_MAP = {
  'windows-x86_64': { key: 'win32-x64', ext: 'zip' },
  'windows-aarch64': { key: 'win32-arm64', ext: 'zip' },
  'macos-x86_64': { key: 'darwin-x64', ext: 'tar.gz' },
  'macos-aarch64': { key: 'darwin-arm64', ext: 'tar.gz' },
  'linux-x86_64': { key: 'linux-x64', ext: 'tar.gz' },
  'linux-aarch64': { key: 'linux-arm64', ext: 'tar.gz' }
}

const FILE_RE = /^firefly-omni-(windows|macos|linux)-(x86_64|aarch64)\.(zip|tar\.gz)$/

function parseArgs(argv) {
  const out = { dryRun: false, force: false }
  const rest = []
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a === '--dry-run') {
      out.dryRun = true
    } else if (a === '--force' || a === '--overwrite') {
      out.force = true
    } else if (a === '--fr') {
      out.frDir = path.resolve(argv[++i])
    } else {
      rest.push(a)
    }
  }
  out.distDir = rest[0] && path.resolve(rest[0])
  out.version = rest[1]
  return out
}

function main() {
  const opts = parseArgs(process.argv.slice(2))
  if (!opts.distDir || !opts.version) {
    console.error('用法：node scripts/publish-unisource.js <distDir> <version> [--fr <目录>] [--dry-run]')
    process.exit(1)
  }
  if (!fs.existsSync(opts.distDir)) {
    console.error(`✗ 产物目录不存在：${opts.distDir}`)
    process.exit(1)
  }

  const frDir = opts.frDir || process.env.UNISOURCE_REPO_DIR
  if (!frDir) {
    console.error('✗ 未指定 firefly-resources 仓库目录（--fr 或环境变量 UNISOURCE_REPO_DIR）')
    process.exit(1)
  }
  const publishCli = path.join(frDir, 'publish-resources.js')
  if (!fs.existsSync(publishCli)) {
    console.error(`✗ 未找到统一资源中心发布脚本：${publishCli}`)
    process.exit(1)
  }

  // 1. 扫描产物 → assets 映射
  const assets = {}
  for (const f of fs.readdirSync(opts.distDir)) {
    const m = FILE_RE.exec(f)
    if (!m) continue
    const meta = SUFFIX_MAP[`${m[1]}-${m[2]}`]
    if (!meta) continue
    const localPath = path.join(opts.distDir, f)
    if (!fs.existsSync(localPath)) continue
    assets[meta.key] = meta.ext === 'zip' ? localPath : { path: localPath, ext: meta.ext }
  }
  if (Object.keys(assets).length === 0) {
    console.error(`✗ 产物目录 ${opts.distDir} 中未匹配到 firefly-omni-<os>-<arch> 归档`)
    process.exit(1)
  }

  // 2. 组装 manifest（写入 firefly-resources/manifests 以便审计）
  const manifest = {
    resources: {
      'firefly-omni': {
        version: opts.version,
        ext: 'zip',
        assets
      }
    }
  }
  const manifestsDir = path.join(frDir, 'manifests')
  if (!fs.existsSync(manifestsDir)) fs.mkdirSync(manifestsDir, { recursive: true })
  const manifestPath = path.join(manifestsDir, `omni-${opts.version}.json`)
  fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2))

  console.log(`📦 平台资产：${Object.keys(assets).join(', ')}`)
  console.log(`📄 清单：${manifestPath}${opts.dryRun ? '（dry-run，不上传）' : ''}`)

  // 3. 调 firefly-resources 发布管线（上传 + 更新本地 index.json）
  const pubArgs = [publishCli, '--manifest', manifestPath]
  if (opts.dryRun) pubArgs.push('--dry-run')
  if (opts.force) pubArgs.push('--force')
  const pub = spawnSync(process.execPath, pubArgs, { stdio: 'inherit', env: process.env })
  if (pub.status !== 0) {
    console.error('✗ 发布到统一资源中心失败。')
    console.error('💡 排查提示：')
    console.error('   1. 若报错 HTTP 401: Bad credentials，说明缺少具有写权限的 PAT Token。')
    console.error('   2. 请确保在 firefly-omni 仓库 Settings -> Secrets and variables -> Actions 中已配置 UNISOURCE_RELEASE_TOKEN 或 PRO_SUBMODULE_TOKEN。')
    console.error('   3. 该 Token 需具备 repo 或 write:packages 作用域以操作 Leonard-Li777/firefly-resources。')
    process.exit(pub.status || 1)
  }

  // 4. 提交并推送 index.json（dry-run 跳过）
  if (opts.dryRun) {
    console.log('🔍 [dry-run] 完成，未提交 index.json。')
    process.exit(0)
  }
  const userEmail = spawnSync('git', ['-C', frDir, 'config', 'user.email'], { encoding: 'utf8' }).stdout.trim()
  if (!userEmail) {
    spawnSync('git', ['-C', frDir, 'config', 'user.email', 'github-actions[bot]@users.noreply.github.com'])
    spawnSync('git', ['-C', frDir, 'config', 'user.name', 'github-actions[bot]'])
  }
  const add = spawnSync('git', ['-C', frDir, 'add', 'index.json'], { stdio: 'inherit' })
  if (add.status !== 0) process.exit(add.status || 1)
  const status = spawnSync('git', ['-C', frDir, 'status', '--porcelain', '--', 'index.json'], { encoding: 'utf8' }).stdout.trim()
  if (!status) {
    console.log('ℹ️  index.json 无变化，跳过提交。')
    process.exit(0)
  }
  const commit = spawnSync(
    'git',
    ['-C', frDir, 'commit', '-m', `ci: 发布 firefly-omni ${opts.version} 各平台资产`],
    { stdio: 'inherit' }
  )
  if (commit.status !== 0) process.exit(commit.status || 1)
  const push = spawnSync('git', ['-C', frDir, 'push', 'origin', 'HEAD'], { stdio: 'inherit' })
  if (push.status !== 0) process.exit(push.status || 1)
  console.log(`🚀 已推送统一资源中心 index（firefly-omni ${opts.version}）。`)
}

main()