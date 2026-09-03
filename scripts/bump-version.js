'use strict'

/**
 * Omni 自动版本递增工具
 * 职责：
 * 1. 读取 Cargo.toml 中的 [workspace.package] version
 * 2. 如果提供了 targetVersion 则使用，否则默认递增 patch 位 (X.Y.Z -> X.Y.(Z+1))
 * 3. 回写 Cargo.toml
 * 4. 打印新版本号供下游 CI 使用
 */

const fs = require('fs')
const path = require('path')

const cargoPath = path.resolve(__dirname, '../Cargo.toml')
if (!fs.existsSync(cargoPath)) {
  console.error(`❌ Cargo.toml 文件不存在: ${cargoPath}`)
  process.exit(1)
}

const rawContent = fs.readFileSync(cargoPath, 'utf8')
const match = rawContent.match(/(\[workspace\.package\][\s\S]*?version\s*=\s*")([^"]+)(")/)

if (!match) {
  console.error('❌ 未能在 Cargo.toml 中匹配到 [workspace.package] version')
  process.exit(1)
}

const currentVersion = match[2]
const arg = process.argv[2] ? process.argv[2].trim() : ''
const isDryRun = process.argv.includes('--dry-run')
let targetVersion = arg && !arg.startsWith('--') ? arg.replace(/^v/, '') : ''

if (targetVersion) {
  if (!/^\d+\.\d+\.\d+(-[a-zA-Z0-9.]+)?$/.test(targetVersion)) {
    console.error(`❌ 指定的目标版本格式不符合语义化规范: ${targetVersion}`)
    process.exit(1)
  }
} else {
  const parts = currentVersion.split('.').map(n => parseInt(n, 10))
  if (parts.length !== 3 || parts.some(isNaN)) {
    console.error(`❌ 当前版本号格式非规范语义化版本: ${currentVersion}`)
    process.exit(1)
  }
  parts[2] += 1
  targetVersion = parts.join('.')
}

if (!isDryRun) {
  const updatedContent = rawContent.replace(
    /(\[workspace\.package\][\s\S]*?version\s*=\s*")([^"]+)(")/,
    `$1${targetVersion}$3`
  )
  fs.writeFileSync(cargoPath, updatedContent, 'utf8')
}
console.log(targetVersion)
