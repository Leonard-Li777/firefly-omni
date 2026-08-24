/**
 * omni 依赖资源初始化与解压脚本 (setup-extra-resources.js)
 * 检查 extraResources/bin 目录中是否存在已解压文件，
 * 如果不存在，从 presetResources/ 找到匹配当前平台 (exiftool-bin-[platform]-[arch].[ext]) 的压缩包并解压。
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

function getPlatformKeywords(platform) {
  if (platform === 'win32') return ['win', 'win32']
  if (platform === 'darwin') return ['darwin', 'macos', 'mac']
  if (platform === 'linux') return ['linux', 'ubuntu']
  return [platform]
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
  const keywords = getPlatformKeywords(platform)

  console.log(`📦 [omni-setup] 检查并解压预设二进制工具包 (平台: ${platform}-${arch})...`)

  // 1. 处理 exiftool
  const exiftoolPresetDir = path.join(presetDir, 'exiftool')
  if (fs.existsSync(exiftoolPresetDir)) {
    const files = fs.readdirSync(exiftoolPresetDir)
    const matchedFile = files.find(f => {
      const lower = f.toLowerCase()
      if (!lower.startsWith('exiftool-bin-') && !lower.startsWith('exiftool-')) return false
      return keywords.some(k => lower.includes(k))
    })

    if (matchedFile) {
      const srcPath = path.join(exiftoolPresetDir, matchedFile)
      const ext = matchedFile.endsWith('.tar.gz') ? '.tar.gz' : path.extname(matchedFile)
      const zipName = path.basename(matchedFile, ext)

      const destDir = path.join(extraResourcesBin, 'exiftool')
      const readyFlag = path.join(destDir, `.ready-${zipName}`)
      const exeName = platform === 'win32' ? 'exiftool.exe' : 'exiftool'
      const targetExe = path.join(destDir, exeName)

      if (!fs.existsSync(targetExe) || !fs.existsSync(readyFlag)) {
        console.log(`📦 [omni-setup] 解压 [${matchedFile}] -> [${destDir}]...`)
        ensureDir(destDir)
        if (matchedFile.endsWith('.zip')) {
          await extractZip(srcPath, destDir)
        } else if (matchedFile.endsWith('.tar.gz')) {
          await extractTarGz(srcPath, destDir)
        }
        fs.writeFileSync(readyFlag, 'completed')
        console.log(`✅ [omni-setup] ExifTool 解压部署完成！`)
      } else {
        console.log(`ℹ️ [omni-setup] [跳过已解压]: ${matchedFile}`)
      }
    } else {
      console.warn(`⚠️ [omni-setup] 未发现匹配平台 (${platform}-${arch}) 的 ExifTool 预设包`)
    }
  }
}

if (require.main === module) {
  setupOmniExtraResources().catch(err => {
    console.error('❌ [omni-setup] 资源设置失败:', err)
    process.exit(1)
  })
}

module.exports = { setupOmniExtraResources }
