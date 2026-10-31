//! anchor_cache.rs — 最近分析文件的**全局语义锚点**进程内缓存（spec §2.2.6 / §3.3）
//!
//! ## 存在理由
//!
//! 批 B（LLM 产物返送打标）需要用「与首次 perceive **同源**的全局锚点」来计算余弦，
//! 否则评测会退化为「同文本自比自」的自我强化。故 `perceive` 结束时把 `e_global`
//! 存入本缓存，返送请求凭 `file_fingerprint` 取用。
//!
//! ## 为何住在 omni-core 而非 omni-text
//!
//! 本类型是**纯容器**（`Vec<f32>` + 同源元数据 + 淘汰策略），不含任何 Pro 专有推理逻辑。
//! 但它被 `omni-server` 的 `AppState` 持有，而 `omni-server` 需同时支持 Open-Core 与 Pro
//! 两种构建（非 Pro 下 `omni_pro` 为存根）。若落在 omni-text，存根侧必须复制一份平行实现
//! ——那正是 §3.5 明令禁止的「第二套口径」。故统一放在两构建共享的 omni-core。
//!
//! ## 三重淘汰（spec §3.3 已裁定）
//!
//! 1. **命中即删（首选）**：锚点使命是「单次返送比对」，`take` 成功后立即移除。
//!    正常路径下绝大多数条目由此清出，缓存实际等价于「待消费锚点的中转区」；
//! 2. **TTL 兜底**：清理「文件被分析但 LLM 产物从未返送」的悬挂条目；
//! 3. **LRU 容量上限**：进程内存硬上限（384 维 f32 ≈ 1.5 KB/锚点，300 条 ≈ 0.45 MB）。
//!
//! ## 编码器同源铁律（spec §2.2.6）
//!
//! 融合锚点走 `BekkoEmbedder`（384 维），而 omni-server 文本分析路径在 `gemma_unified`
//! 档位下走 `GemmaTextEmbedder`（MRL 256/512/768 维）。**跨编码器余弦物理无效**，
//! 故取用时必须校验 `encoder_id` 与 `dim`，不匹配即**确定性降级**（不静默）。
//!
//! `encoder_id` 直接复用 [`crate::encoder_id`] 的单字节身份契约（`1=Bekko, 4=Gemma` …），
//! **不新造字符串编码器名**——否则又是第二份事实源。
//!
//! ## 并发
//!
//! omni-server 为 tokio 多请求模型，缓存必须 `RwLock` 保护读写，杜绝脏读/覆盖写。

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// 默认容量上限（文件数）
pub const DEFAULT_ANCHOR_CACHE_CAPACITY: usize = 300;
/// 默认 TTL（兜底淘汰）
pub const DEFAULT_ANCHOR_CACHE_TTL: Duration = Duration::from_secs(30 * 60);

/// 缓存条目：锚点向量 + 同源校验元数据
#[derive(Debug, Clone)]
pub struct AnchorCacheEntry {
    pub anchor_vector: Vec<f32>,
    /// 产出该锚点的编码器身份（`crate::encoder_id` 单字节契约）
    pub encoder_id: u8,
    /// 向量维度（用于跨维度同源校验）
    pub dim: usize,
    /// 写入时刻（TTL 判定基准）
    pub updated_at: Instant,
}

impl AnchorCacheEntry {
    pub fn new(anchor_vector: Vec<f32>, encoder_id: u8, now: Instant) -> Self {
        let dim = anchor_vector.len();
        Self {
            anchor_vector,
            encoder_id,
            dim,
            updated_at: now,
        }
    }
}

/// 取用结果。**每个失败分支都必须能区分原因**，供上层 `warn` 上报（禁静默降级）。
#[derive(Debug, Clone, PartialEq)]
pub enum AnchorLookup {
    /// 命中：返回锚点向量（条目已从缓存移除）
    Hit(Vec<f32>),
    /// 无该指纹的条目
    Missing,
    /// 条目已过 TTL（已顺带移除）
    Expired,
    /// 编码器身份不匹配（已顺带移除，禁止用异构编码器算余弦）
    EncoderMismatch { cached: u8, current: u8 },
    /// 向量维度不匹配（已顺带移除）
    DimMismatch { cached: usize, current: usize },
}

impl AnchorLookup {
    /// 降级原因的可读标签（日志用；`Hit` 返回 `None`）
    pub fn degrade_reason(&self) -> Option<String> {
        match self {
            AnchorLookup::Hit(_) => None,
            AnchorLookup::Missing => Some("anchor_cache_missing".to_string()),
            AnchorLookup::Expired => Some("anchor_cache_expired".to_string()),
            AnchorLookup::EncoderMismatch { cached, current } => Some(format!(
                "anchor_encoder_mismatch(cached={}, current={})",
                encoder_label(*cached),
                encoder_label(*current)
            )),
            AnchorLookup::DimMismatch { cached, current } => {
                Some(format!("anchor_dim_mismatch(cached={cached}, current={current})"))
            }
        }
    }
}

/// 编码器身份 → 可读名（仅用于日志，不参与判定）
pub fn encoder_label(id: u8) -> &'static str {
    match id {
        crate::encoder_id::BEKKO => "bekko",
        crate::encoder_id::CHINESE_CLIP => "chinese_clip",
        crate::encoder_id::MOBILE_CLIP => "mobile_clip",
        crate::encoder_id::GEMMA => "gemma",
        _ => "unknown",
    }
}

struct CacheInner {
    map: HashMap<String, (AnchorCacheEntry, u64)>,
    /// 单调递增的 LRU 序号（越大越新）
    seq: u64,
}

/// 最近分析文件锚点缓存（进程内共享，不落盘）
pub struct AnchorCache {
    inner: RwLock<CacheInner>,
    capacity: usize,
    ttl: Duration,
}

impl Default for AnchorCache {
    fn default() -> Self {
        Self::new(DEFAULT_ANCHOR_CACHE_CAPACITY, DEFAULT_ANCHOR_CACHE_TTL)
    }
}

impl AnchorCache {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            inner: RwLock::new(CacheInner {
                map: HashMap::new(),
                seq: 0,
            }),
            capacity: capacity.max(1),
            ttl,
        }
    }

    /// 写入锚点（同指纹覆盖旧值）。超过容量时淘汰**最久未使用**条目。
    pub fn put(&self, fingerprint: &str, entry: AnchorCacheEntry) {
        if fingerprint.is_empty() {
            return;
        }
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.seq += 1;
        let seq = inner.seq;
        inner.map.insert(fingerprint.to_string(), (entry, seq));

        if inner.map.len() > self.capacity {
            // LRU 淘汰：移除 seq 最小者（最久未被 put/take 命中）
            if let Some(victim) = inner
                .map
                .iter()
                .min_by_key(|(_, (_, s))| *s)
                .map(|(k, _)| k.clone())
            {
                inner.map.remove(&victim);
                tracing::info!(
                    "[锚点缓存:LRU淘汰] fingerprint={} 当前容量={}",
                    victim,
                    inner.map.len()
                );
            }
        }
    }

    /// 取用锚点（**命中即删**）。使用 `Instant::now()` 作为 TTL 基准。
    pub fn take(&self, fingerprint: &str, encoder_id: u8, dim: usize) -> AnchorLookup {
        self.take_with_now(fingerprint, encoder_id, dim, Instant::now())
    }

    /// 取用锚点（可注入 `now`，供 TTL 确定性单测）。
    ///
    /// 语义顺序：**先判存在 → 再判 TTL → 再判同源**。任何失败分支都会移除条目
    /// （锚点一次性消费，失败不应滞留污染后续判定）。
    pub fn take_with_now(
        &self,
        fingerprint: &str,
        encoder_id: u8,
        dim: usize,
        now: Instant,
    ) -> AnchorLookup {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());

        let Some((entry, _)) = inner.map.remove(fingerprint) else {
            return AnchorLookup::Missing;
        };

        // TTL 兜底：`now` 早于 `updated_at` 视为时钟异常，按未过期处理（不误杀）
        let expired = now
            .checked_duration_since(entry.updated_at)
            .map(|elapsed| elapsed > self.ttl)
            .unwrap_or(false);
        if expired {
            return AnchorLookup::Expired;
        }

        if entry.encoder_id != encoder_id {
            return AnchorLookup::EncoderMismatch {
                cached: entry.encoder_id,
                current: encoder_id,
            };
        }
        if entry.dim != dim {
            return AnchorLookup::DimMismatch {
                cached: entry.dim,
                current: dim,
            };
        }

        AnchorLookup::Hit(entry.anchor_vector)
    }

    /// 当前条目数（可观测性 / 测试）
    pub fn len(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .map
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder_id::{BEKKO, GEMMA};

    fn entry(seed: f32, encoder: u8, dim: usize, now: Instant) -> AnchorCacheEntry {
        AnchorCacheEntry::new(vec![seed; dim], encoder, now)
    }

    /// T6-1：**命中即删**——同一锚点只能被消费一次，第二次取用必为 `Missing`
    #[test]
    fn take_is_single_consumption() {
        let cache = AnchorCache::new(4, Duration::from_secs(60));
        let now = Instant::now();
        cache.put("fp-1", entry(0.5, BEKKO, 384, now));

        match cache.take_with_now("fp-1", BEKKO, 384, now) {
            AnchorLookup::Hit(v) => assert_eq!(v.len(), 384),
            other => panic!("首次取用应命中，实得 {other:?}"),
        }
        assert_eq!(cache.take_with_now("fp-1", BEKKO, 384, now), AnchorLookup::Missing);
        assert!(cache.is_empty(), "命中即删后缓存必须为空");
    }

    /// T6-2：TTL 兜底——超时条目按 `Expired` 报出（不得静默当作 Missing）
    #[test]
    fn take_reports_expired_after_ttl() {
        let cache = AnchorCache::new(4, Duration::from_secs(30));
        let t0 = Instant::now();
        cache.put("fp-ttl", entry(0.1, BEKKO, 384, t0));

        // 边界内：仍应命中
        cache.put("fp-ttl2", entry(0.1, BEKKO, 384, t0));
        assert!(matches!(
            cache.take_with_now("fp-ttl2", BEKKO, 384, t0 + Duration::from_secs(29)),
            AnchorLookup::Hit(_)
        ));

        assert_eq!(
            cache.take_with_now("fp-ttl", BEKKO, 384, t0 + Duration::from_secs(31)),
            AnchorLookup::Expired
        );
    }

    /// T6-3：LRU 容量上限——超容量淘汰最久未使用者
    #[test]
    fn capacity_evicts_least_recently_used() {
        let cache = AnchorCache::new(2, Duration::from_secs(60));
        let now = Instant::now();
        cache.put("a", entry(0.1, BEKKO, 384, now));
        cache.put("b", entry(0.2, BEKKO, 384, now));
        // 重新 put "a" ⇒ "a" 变为最新，"b" 成为最久未使用
        cache.put("a", entry(0.3, BEKKO, 384, now));
        cache.put("c", entry(0.4, BEKKO, 384, now));

        assert_eq!(cache.len(), 2, "容量上限必须生效");
        assert_eq!(cache.take_with_now("b", BEKKO, 384, now), AnchorLookup::Missing);
        assert!(matches!(cache.take_with_now("a", BEKKO, 384, now), AnchorLookup::Hit(_)));
        assert!(matches!(cache.take_with_now("c", BEKKO, 384, now), AnchorLookup::Hit(_)));
    }

    /// T9：**编码器同源校验** —— `encoder_id` / `dim` 不匹配必须显式区分，不得静默降级
    #[test]
    fn encoder_and_dim_mismatch_are_reported() {
        let cache = AnchorCache::new(4, Duration::from_secs(60));
        let now = Instant::now();

        cache.put("fp-enc", entry(0.5, BEKKO, 384, now));
        assert_eq!(
            cache.take_with_now("fp-enc", GEMMA, 384, now),
            AnchorLookup::EncoderMismatch {
                cached: BEKKO,
                current: GEMMA
            }
        );

        cache.put("fp-dim", entry(0.5, BEKKO, 384, now));
        assert_eq!(
            cache.take_with_now("fp-dim", BEKKO, 512, now),
            AnchorLookup::DimMismatch {
                cached: 384,
                current: 512
            }
        );
    }

    /// 空指纹不得入缓存（`file_fingerprint` 缺省时 `put` 必须是无操作）
    #[test]
    fn empty_fingerprint_is_rejected() {
        let cache = AnchorCache::new(4, Duration::from_secs(60));
        cache.put("", entry(0.5, BEKKO, 384, Instant::now()));
        assert!(cache.is_empty());
    }

    /// 降级原因标签必须可读且覆盖全部分支（供上层 warn 上报，禁静默）
    #[test]
    fn degrade_reason_labels_every_failure_branch() {
        assert_eq!(AnchorLookup::Hit(vec![0.0]).degrade_reason(), None);
        assert_eq!(
            AnchorLookup::Missing.degrade_reason().as_deref(),
            Some("anchor_cache_missing")
        );
        assert_eq!(
            AnchorLookup::Expired.degrade_reason().as_deref(),
            Some("anchor_cache_expired")
        );
        assert_eq!(
            AnchorLookup::EncoderMismatch {
                cached: BEKKO,
                current: GEMMA
            }
            .degrade_reason()
            .as_deref(),
            Some("anchor_encoder_mismatch(cached=bekko, current=gemma)")
        );
        assert_eq!(
            AnchorLookup::DimMismatch {
                cached: 384,
                current: 512
            }
            .degrade_reason()
            .as_deref(),
            Some("anchor_dim_mismatch(cached=384, current=512)")
        );
    }
}
