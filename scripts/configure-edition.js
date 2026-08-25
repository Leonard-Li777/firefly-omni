const fs = require('fs')
const path = require('path')

/**
 * Omni 开源版 (Community Edition - CE) 与 专业版 (Pro Edition) 切换配置器
 * @param {'ce' | 'pro'} edition
 */
function configureOmniEdition(edition = 'pro') {
  const omniRoot = path.resolve(__dirname, '..')
  const serverCargoPath = path.join(omniRoot, 'crates', 'omni-server', 'Cargo.toml')
  const rootCargoPath = path.join(omniRoot, 'Cargo.toml')

  const isCE = edition === 'ce' || process.env.OMNI_EDITION === 'ce' || process.argv.includes('--ce')

  if (fs.existsSync(serverCargoPath)) {
    let content = fs.readFileSync(serverCargoPath, 'utf8')
    if (isCE) {
      // 切换至开源存根: omni-pro = { path = "../omni-pro-stub", package = "omni-pro-stub" }
      content = content.replace(
        /omni-pro\s*=\s*\{[^}]*\}/g,
        'omni-pro = { path = "../omni-pro-stub", package = "omni-pro-stub" }'
      )
    } else {
      // 切换至 Pro 版源码: omni-pro = { path = "../../omni-pro", package = "omni-pro" }
      content = content.replace(
        /omni-pro\s*=\s*\{[^}]*\}/g,
        'omni-pro = { path = "../../omni-pro", package = "omni-pro" }'
      )
    }
    fs.writeFileSync(serverCargoPath, content, 'utf8')
  }

  if (fs.existsSync(rootCargoPath)) {
    let rootContent = fs.readFileSync(rootCargoPath, 'utf8')
    if (isCE) {
      // CE 版排除 omni-pro 相关工作区子包
      const ceMembers = `members = [
    "crates/omni-core",
    "crates/omni-extract",
    "crates/omni-vision",
    "crates/omni-server",
    "crates/omni-cli",
    "crates/omni-mcp",
    "crates/omni-node",
    "crates/omni-pro-stub",
]`
      rootContent = rootContent.replace(/members\s*=\s*\[[\s\S]*?\]/m, ceMembers)
    } else {
      // Pro 版包含全量工作区子包
      const proMembers = `members = [
    "crates/omni-core",
    "crates/omni-extract",
    "crates/omni-vision",
    "crates/omni-server",
    "crates/omni-cli",
    "crates/omni-mcp",
    "crates/omni-node",
    "crates/omni-pro-stub",
    "omni-pro",
    "omni-pro/crates/omni-geo",
    "omni-pro/crates/omni-cleanup",
]`
      rootContent = rootContent.replace(/members\s*=\s*\[[\s\S]*?\]/m, proMembers)
    }
    fs.writeFileSync(rootCargoPath, rootContent, 'utf8')
  }

  console.log(`🔧 [firefly-omni] Configured edition: ${isCE ? 'Community Edition (CE 开源模式)' : 'Pro Enterprise Edition (闭源完整模式)'}`)
}

if (require.main === module) {
  const editionArg = process.argv.includes('--ce') ? 'ce' : 'pro'
  configureOmniEdition(editionArg)
}

module.exports = { configureOmniEdition }
