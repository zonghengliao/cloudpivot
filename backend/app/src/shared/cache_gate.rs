//! 缓存失效「欠账」闸门：失效失败时让进程记住「这份缓存不可信」，直到补失效成功。
//!
//! # 为什么需要（BUG-SYS-03 的第三段）
//!
//! 菜单树 / 角色下拉 / 部门树这类「整表快照」缓存，写库成功后靠 `Cache::del` 打掉旧值。
//! 缓存层不可用（无 Redis / 抖动 / 重启窗口）时 `del` 会失败：业务必须照常成功（否则就是
//! 「500 但数据已写入」），但**旧值还留在缓存里**，TTL 长达 [`crate::shared::constants::
//! CACHE_HOT_QUERY_TTL_SECS`]（3600 s）—— 缓存恢复后的整整一小时内，接口返回的都是
//! 已经被删掉/改掉的菜单树，比 500 更难发现（用户看到「删了还在」）。
//!
//! # 做法
//!
//! 只改**调用点**，不动缓存层接口签名：
//!
//! 1. 失效失败 → [`InvalidationGate::record`]`(false)` 置「脏」并打 `warn`（带
//!    `cache_invalidation_pending=true` 字段，便于告警规则抓）；
//! 2. 之后的读缓存前先问 [`InvalidationGate::cache_readable`]：脏 → **跳过缓存直接查库**
//!    （并顺手补一次失效，成功即恢复可信）；
//! 3. 失效成功 / 成功写入新快照 → 置「净」。
//!
//! 因此缓存恢复后的第一次读不会拿到旧值，最坏只是多查几次库；脏标记是进程内的
//! （`AtomicBool`），多实例部署下每个实例各自记账——各自跳过各自的旧命中，语义仍然正确。
//!
//! # 纯逻辑、可单测
//!
//! 闸门不碰 IO，判定结果由调用方喂进来，因此「失效失败 → 不读缓存 → 补失效成功后恢复」
//! 这条状态机可以用普通单测钉死（见文件末尾的用例）。

use std::sync::atomic::{AtomicBool, Ordering};

/// 一份缓存的「可信 / 欠账」状态。
///
/// 用 `const fn new()` 支持 `static` 初始化（`static GATE: InvalidationGate =
/// InvalidationGate::new();`）。
#[derive(Debug)]
pub(crate) struct InvalidationGate {
    /// `true` = 上次失效失败，缓存里可能还留着旧值。
    dirty: AtomicBool,
}

impl InvalidationGate {
    /// 新建闸门（初始「净」：进程刚起来时缓存里没有本进程写过的值）。
    pub(crate) const fn new() -> Self {
        Self {
            dirty: AtomicBool::new(false),
        }
    }

    /// 当前缓存是否可以信任（可以读）。
    ///
    /// # 返回值
    ///
    /// - `true`：上次失效成功（或从未失败），读缓存安全；
    /// - `false`：上次失效失败，缓存里可能是旧值，调用方必须跳过缓存。
    pub(crate) fn cache_readable(&self) -> bool {
        !self.dirty.load(Ordering::Acquire)
    }

    /// 记录一次失效 / 快照写入的结果。
    ///
    /// # 参数
    ///
    /// - `ok`：`true` = 缓存侧动作成功（缓存可信）；`false` = 失败（缓存欠账）。
    pub(crate) fn record(&self, ok: bool) {
        self.dirty.store(!ok, Ordering::Release);
    }

    /// 仅供单元测试：把闸门直接置成「欠账」，模拟「上一次失效失败了」。
    #[cfg(test)]
    pub(crate) fn mark_dirty_for_test(&self) {
        self.dirty.store(true, Ordering::Release);
    }
}

impl Default for InvalidationGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 初始状态必须「净」：否则进程刚起来就白白放弃缓存。
    #[test]
    fn new_gate_is_clean() {
        let gate = InvalidationGate::new();
        assert!(gate.cache_readable(), "新建闸门应可读缓存");
    }

    /// 失效失败 → 不可读缓存（BUG-SYS-03：恢复后不许返回旧值）。
    #[test]
    fn failed_invalidation_blocks_cache_reads() {
        let gate = InvalidationGate::new();
        gate.record(false);
        assert!(
            !gate.cache_readable(),
            "失效失败后必须跳过缓存（否则 TTL 3600 s 内一直返回旧值）"
        );
    }

    /// 失败后补失效成功 → 恢复可读（脏标记不能永久粘住）。
    #[test]
    fn successful_retry_restores_reads() {
        let gate = InvalidationGate::new();
        gate.record(false);
        assert!(!gate.cache_readable());
        gate.record(true);
        assert!(gate.cache_readable(), "补失效成功后应恢复读缓存");
    }

    /// 连续失败仍然「脏」，且成功后立刻「净」（无中间态泄漏）。
    #[test]
    fn repeated_failures_keep_gate_dirty_then_clear() {
        let gate = InvalidationGate::new();
        gate.record(false);
        gate.record(false);
        assert!(!gate.cache_readable());
        gate.record(true);
        assert!(gate.cache_readable());
        gate.record(true);
        assert!(gate.cache_readable());
    }

    /// 测试专用置脏入口与 `record(false)` 等价（调用点用例靠它造状态）。
    #[test]
    fn mark_dirty_helper_matches_record_false() {
        let gate = InvalidationGate::new();
        gate.mark_dirty_for_test();
        assert!(!gate.cache_readable());
    }
}
