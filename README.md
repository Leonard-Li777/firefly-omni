# firefly-omni 🚀

> **全能多模态 Rust 文件感知、提取与智能去重引擎**  
> *Universal Multimodal File Intelligence, Extraction & Deduplication Engine in Rust*

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)

---

## 🌟 核心特性 (Key Features)

1. **📄 极速文档转 Markdown**：基于 `anydoc` 纯 Rust 原生解析，5ms 内提取 DOCX、PPTX、PDF、HTML、EPUB 纯文本与结构化 Markdown。
2. **🏷️ 原生 EXIF & Tag 元数据**：集成 `exiftool-rs` (纯 Rust ExifTool)、`lofty` (音频 ID3/Vorbis/MP4 Tag) 与 `kamadak-exif` (图片 EXIF)。
3. **🤖 AI 类型识别 & 视觉分类**：基于 Google `magika` ONNX 识别 200+ 种真实 MIME 格式，结合 `MobileNetV3` 图像分类。
4. **🖼️ 嵌图 OCR 语义原位替换**：文档内嵌图片经 PP-OCRv6 识别后，在 Markdown 原始占位符位置**原位替换**，100% 保留语义顺序。
5. **⚡ 智能去重与感知哈希**：集成 `czkawka_core`，提供图像 pHash 相似度计算、Blake3/XXHash 极速物理去重与损坏文件判定。
6. **🔌 多模态对外接口**：
   - **Axum HTTP REST API** + 托管 React 拖拽/配置 UI
   - **CLI 命令行** (`firefly-omni-cli`)
   - **Agent MCP 服务端** (Model Context Protocol / `SKILL.md`)
   - **Node.js 原生模块** (`@firefly/omni` napi-rs 绑定)

---

## 🏗️ 架构声明 (Architecture)

`firefly-omni` 采用开源 Open-Core 架构模式：
- 开源核心在 `crates/` 下，协议为 **Apache-2.0**。
- 企业定制模块在 `ee/` 目录下隔离（通过 `--features enterprise` 条件编译）。

---

## 📜 许可证 (License)

本项目采用 [Apache-2.0 License](./LICENSE) 协议开源。
---

## 🌍 离线反向地理编码 (omni-geo)

`crates/omni-geo` 提供完全离线的"坐标 → 国家 / 省 / 州 / 城市"解析能力，基于 GeoNames cities500 裁剪数据集（约 20 万居民点），支持分层降级输出：

| 距离 | 输出层级 |
| --- | --- |
| d ≤ 50km（可按请求覆盖） | country + province + city |
| 50km < d ≤ 500km | country + province |
| d > 500km | found: false |

### HTTP 接口

- `POST /api/geo/reverse` — 批量反向地理编码（结果与请求点按下标对齐，脏坐标单点容错）
- `GET /health` — 附 `geoAvailable` 字段供宿主探测

请求示例：

```json
{
  "points": [{ "latitude": 22.55, "longitude": 114.07 }],
  "language": "zh-CN",
  "maxCityKm": 50,
  "maxAnyKm": 500
}
```

数据集缺失时接口恒返回 HTTP 200 + `available:false`（软降级），不影响服务其余功能。

### 数据集装配

开发环境执行资源初始化脚本即可自动装配（本地 presetResources 优先，缺失时从 firefly-omni Release 回退下载）：

```powershell
node scripts/setup-extra-resources.js
```

维护者重建/发布数据集：

```powershell
node scripts/build-geo-dataset.mjs     # 从 GeoNames 官方转储构建（支持 --use-cache）
node scripts/upload-ci-static-resources.js   # 上传至 firefly-omni Release (tag: geo-data)
```

### 📋 数据归属声明 (Attribution — CC-BY 4.0)

本模块使用的地理数据来自 [GeoNames.org](https://www.geonames.org/)，依据 **Creative Commons Attribution 4.0 (CC-BY 4.0)** 许可使用。
使用本软件即视为已获得该数据的许可声明：**"GeoNames 数据由 geonames.org 提供"**。
