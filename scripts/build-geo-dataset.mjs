/**
 * omni-geo 数据集构建脚本 (build-geo-dataset.mjs)
 *
 * 从 GeoNames 官方转储下载源数据，裁剪生成 omni-geo 运行时使用的紧凑数据集：
 *   源：cities500.zip / admin1CodesASCII.txt / admin2Codes.txt / countryInfo.txt / alternateNames.zip
 *        (https://download.geonames.org/export/dump/)
 *   产物：
 *     build/extraResources/geo/geonames-compact.json     运行时数据集（解压即用明文 JSON）
 *     build/extraResources/geo/VERSION                    版本号（YYYYMMDD 整数）
 *     build/presetResources/geo/geonames-resources.tar.gz 分发包（前两者打包）
 *
 * 语言范围：en zh ja ko fr de es ru pt ar（与桌面端多语言一致）。
 * 命名优先级：isPreferredName 标记 > 非简称 > 首个候选；英文缺省时回退 asciiName/name 列并烘焙进数据集，
 * 保证运行时任何语言查询都能取到城市名（运行时另有"请求语言→英文"回退链兜底）。
 *
 * 用法：
 *   node scripts/build-geo-dataset.mjs              # 全量下载 + 构建 + 打包
 *   node scripts/build-geo-dataset.mjs --use-cache  # 复用 build/geo-cache 已下载文件
 */

import fs from 'node:fs'
import path from 'node:path'
import { Readable } from 'node:stream'
import { pipeline } from 'node:stream/promises'
import readline from 'node:readline'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const OMNI_ROOT = path.resolve(__dirname, '..')
const CACHE_DIR = path.join(OMNI_ROOT, 'build', 'geo-cache')
const GEO_OUT_DIR = path.join(OMNI_ROOT, 'build', 'extraResources', 'geo')
const PRESET_GEO_DIR = path.join(OMNI_ROOT, 'build', 'presetResources', 'geo')

const GEONAMES_DUMP_BASE = 'https://download.geonames.org/export/dump/'
const USE_CACHE = process.argv.includes('--use-cache')

/** 支持的语言集合（GeoNames alternateNames 的 iso 语言码，精确或主子码匹配） */
const LANGS = new Set(['en', 'zh', 'ja', 'ko', 'fr', 'de', 'es', 'ru', 'pt', 'ar'])
/** 英文兜底语言码 */
const EN = 'en'
/** 数据集版本号：构建日期（本地时区 YYYYMMDD） */
const VERSION = Number(
  new Date()
    .toLocaleDateString('sv-SE', { timeZone: 'Asia/Shanghai' })
    .replaceAll('-', '')
)

function log(msg) {
  console.log(`[${new Date().toLocaleTimeString()}] ${msg}`)
}

function ensureDir(dir) {
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true })
  }
}

/**
 * 将 GeoNames 语言码归一到基础子码；不在支持集合内时返回 null
 * 例如 "zh-Hans" -> "zh"，"pt-BR" -> "pt"，"lv" -> null
 */
function normalizeLang(iso) {
  const lower = String(iso || '').trim().toLowerCase()
  if (!lower) return null
  const base = lower.split(/[-_]/)[0]
  return LANGS.has(lower) ? lower : LANGS.has(base) ? base : null
}

async function downloadFile(url, dest) {
  log(`⬇️  下载 ${url}`)
  // 优先使用系统 curl：自带重试与断点续传，对代理链路中断更鲁棒
  const curl = spawnSync(
    'curl',
    ['-L', '--fail', '--retry', '5', '--retry-delay', '2', '--retry-all-errors', '-C', '-', '-o', dest, url],
    { stdio: ['ignore', 'ignore', 'pipe'] }
  )
  if (curl.status !== 0) {
    // 回退到 Node 内置 fetch（无重试能力）
    try {
      const res = await fetch(url)
      if (!res.ok) {
        throw new Error(`下载失败 HTTP ${res.status}: ${url}`)
      }
      await pipeline(Readable.fromWeb(res.body), fs.createWriteStream(dest))
    } catch (err) {
      throw new Error(`下载失败 (${url}): ${err.message}（curl 输出: ${curl.stderr?.toString().trim()}）`)
    }
  }
  const sizeMB = (fs.statSync(dest).size / 1024 / 1024).toFixed(1)
  log(`✅ 完成 ${path.basename(dest)} (${sizeMB} MB)`)
}

/** 获取源文件：--use-cache 时复用缓存，否则下载并写入缓存目录 */
async function obtainSource(fileName) {
  ensureDir(CACHE_DIR)
  const cached = path.join(CACHE_DIR, fileName)
  if (USE_CACHE && fs.existsSync(cached)) {
    log(`♻️  使用缓存: ${fileName}`)
    return cached
  }
  await downloadFile(GEONAMES_DUMP_BASE + fileName, cached)
  return cached
}

/** 解压 zip 到指定目录（Windows/macOS 走系统 bsdtar，Linux 回退 unzip） */
function extractZip(zipPath, destDir) {
  ensureDir(destDir)
  let res = spawnSync('tar', ['-xf', zipPath, '-C', destDir])
  if (res.status !== 0) {
    res = spawnSync('unzip', ['-q', '-o', zipPath, '-d', destDir])
  }
  if (res.status !== 0) {
    throw new Error(`解压失败: ${zipPath}`)
  }
}

/** 逐行读取（自动识别 .txt 与 .zip 内单文件） */
async function* readLines(filePath) {
  let stream
  if (filePath.endsWith('.zip')) {
    // zip 内文本直接经 Node 内置 zlib 无法读取，先解压到临时目录再读
    const tmpDir = path.join(CACHE_DIR, '_extracted_' + path.basename(filePath, '.zip'))
    extractZip(filePath, tmpDir)
    const inner = fs.readdirSync(tmpDir).find(f => f.endsWith('.txt'))
    if (!inner) throw new Error(`压缩包内未找到 txt: ${filePath}`)
    stream = fs.createReadStream(path.join(tmpDir, inner))
  } else {
    stream = fs.createReadStream(filePath)
  }
  const rl = readline.createInterface({ input: stream, crlfDelay: Infinity })
  for await (const line of rl) {
    yield line
  }
  rl.close()
  stream.close()
}

/**
 * 地名候选评分：isPreferredName 权重最高，非简称次之
 * 同分保持先到先得（alternateNames 行序稳定）
 */
function nameScore(preferred, short) {
  return (preferred === '1' ? 4 : 0) + (short !== '1' ? 2 : 0)
}

/** 多语言名容器：收集各语言最优名，最终烘焙英文兜底 */
class NameBag {
  constructor() {
    this.map = new Map() // lang -> { score, name }
  }

  offer(lang, name, score) {
    if (!lang || !name) return
    const prev = this.map.get(lang)
    if (!prev || score > prev.score) {
      this.map.set(lang, { name, score })
    }
  }

  /** 烘焙英文兜底后输出紧凑对象（无名的语言不占空间） */
  bake(enFallback) {
    const out = {}
    for (const [lang, { name }] of this.map) {
      out[lang] = name
    }
    if (!out[EN] && enFallback) {
      out[EN] = enFallback
    }
    return Object.keys(out).length ? out : undefined
  }
}

/** 将 "键 -> NameBag" 字典序列化为 "键 -> 多语言名对象"，空包剔除 */
function serializeBags(bagMap) {
  const out = {}
  for (const [key, bag] of bagMap) {
    const baked = bag.bake()
    if (baked) {
      out[key] = baked
    }
  }
  return out
}

async function main() {
  log(`🚀 开始构建 omni-geo 紧凑数据集 (版本: ${VERSION})`)

  // ========== 1. 获取全部源文件 ==========
  const citiesZip = await obtainSource('cities500.zip')
  const admin1Txt = await obtainSource('admin1CodesASCII.txt')
  const admin2Txt = await obtainSource('admin2Codes.txt')
  const countryTxt = await obtainSource('countryInfo.txt')
  const altNamesZip = await obtainSource('alternateNames.zip')

  /** wantedIds: 参与地名匹配的 geonameid 集合（居民点 + 行政区 + 国家） */
  const wantedIds = new Set()

  // ========== 2. 解析国家表（跳过 # 注释行）==========
  /** countries: iso2 -> NameBag */
  const countries = new Map()
  /** countryByGid: 国家 geonameid -> NameBag 列表（用于 alternateNames 反查） */
  const countryByGid = new Map()
  for await (const line of readLines(countryTxt)) {
    if (!line || line.startsWith('#')) continue
    const c = line.split('\t')
    // 列: [0]=ISO2 [4]=Country 名称 [16]=geonameid
    const iso = c[0]
    const name = c[4]
    if (!iso || !name) continue
    const bag = new NameBag()
    bag.offer('en', name, nameScore('1', '0'))
    countries.set(iso, bag)
    const gid = Number(c[16])
    if (gid) {
      // 国家 geonameid 必须同时进入 wantedIds，否则扫描阶段会被整行过滤
      wantedIds.add(gid)
      const list = countryByGid.get(gid) ?? []
      list.push(bag)
      countryByGid.set(gid, list)
    }
  }

  // ========== 3. 解析居民点（cities500：人口 ≥ 500，约 20 万条）==========
  /** points 数组元素: { id, lat, lng, cc, ad1, ad2, pop, bag } */
  const points = []
  /** pointsById: geonameid -> 居民点（alternateNames 反查用） */
  const pointsById = new Map()
  for await (const line of readLines(citiesZip)) {
    const p = line.split('\t')
    if (p.length < 15) continue
    // 列: [0]=geonameid [1]=name [2]=asciiName [4]=lat [5]=lng [8]=cc [10]=admin1 [11]=admin2 [14]=population
    const id = Number(p[0])
    const lat = Number(p[4])
    const lng = Number(p[5])
    const cc = p[8] || ''
    if (!Number.isFinite(lat) || !Number.isFinite(lng) || !cc) continue
    const point = {
      id,
      lat,
      lng,
      cc,
      ad1: p[10] || null,
      ad2: p[11] || null,
      pop: Number(p[14]) || 0,
      bag: new NameBag(),
      asciiName: p[2] || p[1] || '',
    }
    points.push(point)
    pointsById.set(id, point)
    wantedIds.add(id)
  }

  // ========== 4. 解析一级行政区 ==========
  /** admin1: "CC.ADM1" -> NameBag */
  const admin1 = new Map()
  /** admin1ByGid: geonameid -> NameBag 列表 */
  const admin1ByGid = new Map()
  for await (const line of readLines(admin1Txt)) {
    const a = line.split('\t')
    if (a.length < 4) continue
    // 列: [0]=key(CC.ADM1) [1]=name [2]=asciiName [3]=geonameid
    const bag = new NameBag()
    bag.offer('en', a[1], nameScore('1', '0'))
    admin1.set(a[0], bag)
    const gid = Number(a[3])
    if (gid) {
      wantedIds.add(gid)
      const list = admin1ByGid.get(gid) ?? []
      list.push(bag)
      admin1ByGid.set(gid, list)
    }
  }

  // ========== 5. 解析二级行政区 ==========
  const admin2 = new Map()
  /** admin2ByGid: geonameid -> NameBag 列表 */
  const admin2ByGid = new Map()
  for await (const line of readLines(admin2Txt)) {
    const a = line.split('\t')
    if (a.length < 4) continue
    // 列: [0]=key(CC.ADM1.ADM2) [1]=name [2]=asciiName [3]=geonameid
    const bag = new NameBag()
    bag.offer('en', a[1], nameScore('1', '0'))
    admin2.set(a[0], bag)
    const gid = Number(a[3])
    if (gid) {
      wantedIds.add(gid)
      const list = admin2ByGid.get(gid) ?? []
      list.push(bag)
      admin2ByGid.set(gid, list)
    }
  }

  // ========== 6. 扫描 alternateNames（大流式，逐行过滤）==========
  log('🌍 扫描 alternateNames（约千万行，请耐心等待）...')
  let matched = 0
  for await (const line of readLines(altNamesZip)) {
    const a = line.split('\t')
    if (a.length < 5) continue
    // 列: [0]=altId [1]=geonameid [2]=iso语言 [3]=名称 [4]=isPreferred [5]=isShort [6]=isColloquial [7]=isHistoric
    const gid = Number(a[1])
    if (!wantedIds.has(gid)) continue
    const lang = normalizeLang(a[2])
    if (!lang) continue
    // 口语化/历史地名一律排除，避免"旧称"污染输出
    if (a[6] === '1' || a[7] === '1') continue

    const name = a[3]?.trim()
    if (!name) continue
    const score = nameScore(a[4], a[5])
    matched++

    // 居民点 → 一级/二级行政区 → 国家，依次反查挂载
    const point = pointsById.get(gid)
    if (point) {
      point.bag.offer(lang, name, score)
      continue
    }
    for (const bag of admin1ByGid.get(gid) ?? []) {
      bag.offer(lang, name, score)
    }
    for (const bag of admin2ByGid.get(gid) ?? []) {
      bag.offer(lang, name, score)
    }
    for (const bag of countryByGid.get(gid) ?? []) {
      bag.offer(lang, name, score)
    }
  }

  // ========== 7. 序列化紧凑 JSON 并 gzip 输出 ==========
  ensureDir(GEO_OUT_DIR)
  const dataset = {
    version: VERSION,
    points: points.map(p => {
      const n = p.bag.bake(p.asciiName)
      return {
        id: p.id,
        lat: p.lat,
        lng: p.lng,
        cc: p.cc,
        ad1: p.ad1,
        ad2: p.ad2,
        pop: p.pop,
        ...(n ? { n } : {}),
      }
    }),
    admin1: serializeBags(admin1),
    admin2: serializeBags(admin2),
    countries: serializeBags(countries),
  }

  const jsonStr = JSON.stringify(dataset)
  // 运行时产物为解压即用的明文 JSON；压缩仅发生在分发归档层（tar -czf）
  const jsonPath = path.join(GEO_OUT_DIR, 'geonames-compact.json')
  fs.writeFileSync(jsonPath, jsonStr)
  fs.writeFileSync(path.join(GEO_OUT_DIR, 'VERSION'), String(VERSION))

  const jsonMB = (fs.statSync(jsonPath).size / 1024 / 1024).toFixed(1)
  log(`✅ 数据集已生成: ${jsonPath} (${jsonMB} MB, ${dataset.points.length} 个居民点, 地名匹配 ${matched} 条)`)

  // ========== 8. 打包分发包（tar.gz，跨平台系统 tar 创建）==========
  ensureDir(PRESET_GEO_DIR)
  const tarPath = path.join(PRESET_GEO_DIR, 'geonames-resources.tar.gz')
  const tarRes = spawnSync('tar', ['-czf', tarPath, '-C', GEO_OUT_DIR, 'geonames-compact.json', 'VERSION'])
  if (tarRes.status !== 0) {
    throw new Error('tar 打包失败，请确认系统存在 tar 命令')
  }
  const tarMB = (fs.statSync(tarPath).size / 1024 / 1024).toFixed(1)
  log(`📦 分发包已生成: ${tarPath} (${tarMB} MB)`)
  log('🎉 构建完成！可执行 node scripts/upload-ci-static-resources.js 上传至 firefly-omni Release (geo-data)。')
}

main().catch(err => {
  console.error('❌ 构建失败:', err)
  process.exit(1)
})
