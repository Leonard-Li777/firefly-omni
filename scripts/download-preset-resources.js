/**
 * omni 预设资源下载 —— 已退役（2026-11-25）
 *
 * 历史职责：本地缺少 exiftool / ffmpeg / ffprobe 预设包时，从 GitHub
 * `firefly-omni` 仓库的 `ci-resources` Release 拉取 `*-resources.zip` 并解压到
 * `apps/omni/build/presetResources/`，供 `setup-extra-resources.js` 解压到 omni 的 bin。
 *
 * 退役原因（与 ADR-0029「统一资源分发」对齐）：
 * 1. 这三个媒体工具是 firefly-resources 的共享 tool 资产，唯一权威源是
 *    `apps/firefly-resources/sources/{ffmpeg,ffprobe,exiftool}.json`，
 *    由发布流水线产出 `*-master-<date>-<platform>-<arch>.zip` 并回填 index/BOM；
 * 2. 本脚本拉取的是**已退役 npm 包产物**（2018 年的 ffmpeg/ffprobe 构建），
 *    与 desktop 装配的 BtbN 新构建并存，且会被 `scripts/omni-build-to-desktop.js`
 *    覆盖回 desktop，是「资产已升级但装包仍是老二进制」事故的直接成因；
 * 3. omni 侧装配已取消（`setup-extra-resources.js` 只剩 geo），运行时改为按资源相对路径
 *    探测 `resources/bin/<tool>/<exe>`（见 `czkawka_bridge::ensure_media_tools_in_path`），
 *    不再需要本地副本。
 *
 * 保留本文件与同名导出仅为兼容既有命令注册表（`scripts/commands-registry.js`）调用，
 * 行为是显式跳过并提示正确来源，绝不静默成功。
 */

function downloadPresetResources() {
  console.log(
    '⏭️ [omni] download-preset-resources 已退役：ffmpeg / ffprobe / exiftool 由 ' +
      'firefly-resources 统一发布、由 desktop 装配（bin/<tool>/），omni 侧不再下载维护。\n' +
      '   如桌面端缺资源，请在 apps/desktop 执行 `pnpm setup-resources`（lock/BOM 驱动 + sha256 校验）。'
  )
  return Promise.resolve({ retired: true })
}

if (require.main === module) {
  downloadPresetResources()
}

module.exports = { downloadPresetResources }
