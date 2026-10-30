//! 存量违规检查（批次 B · B7）：单用户体检 / 单角色对持有者 / 全量扫描。
//!
//! # 三个入口，一套口径
//!
//! | 用途 | 函数 | 对应接口 |
//! |------|------|----------|
//! | 某用户当前是否已违规 | [`user_conflicts`] | `GET /business/mutex/conflict?user_id=` |
//! | 某角色对的双持用户 | [`pair_holders`] | `POST` / `PUT` 的校验点 3、校验点 2 |
//! | 全量扫描 | [`scan`] | `POST /business/mutex/scan` |
//!
//! 三者的「持有」判定都走 [`crate::business::mutex::model`] 里那份唯一的 SQL 口径
//! （绑定未过期 ∧ 用户未软删 ∧ 角色未软删 ∧ 角色未停用），本文件**不重复写条件**。
//!
//! # 为什么扫描按规则逐对查、而不是一条大 SQL
//!
//! 规则数是「几十条」量级（管理员手工维护的合规约束），逐对查让每条规则的 `total` 与
//! `sample` 都能精确收口（一条大 SQL 拿到的是一张扁平表，还要在应用层重新分组，反而更
//! 容易把 `total` 算成 `sample.len()`）。真正的风险是「规则多到上万条」，那时该做的是
//! 归档停用规则，而不是把 `total` 近似掉。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::mutex::model::{self, HolderRow, MutexRow, PairHit};

/// 每条规则的违规用户样本上限（`409` 文案与扫描结果都用它）。
pub const SAMPLE_LIMIT: i64 = 20;

/// 一条规则下的违规用户（规则 + 双持用户总数 + 样本）。
#[derive(Debug, Clone)]
pub struct ViolationGroup {
    /// 命中的规则。
    pub rule: MutexRow,
    /// 双持用户总数。
    pub total: i64,
    /// 双持用户样本（最多 [`SAMPLE_LIMIT`] 条）。
    pub sample: Vec<HolderRow>,
}

/// 全量扫描结果。
#[derive(Debug, Clone)]
pub struct ScanResult {
    /// 扫描的生效规则数。
    pub scanned_pairs: usize,
    /// 只保留有违规的规则（按角色编码排序，沿用 [`model::list_active_pairs`] 的顺序）。
    pub items: Vec<ViolationGroup>,
}

/// 取某角色对的双持用户：`(总数, 样本)`。
///
/// # 返回值
///
/// `total = 0` 时 `sample` 必为空（总数与样本用同一组条件，只差 `LIMIT`）。
pub async fn pair_holders(role_a: &Uuid, role_b: &Uuid) -> DbResult<(i64, Vec<HolderRow>)> {
    let total = model::count_pair_holders(role_a, role_b).await?;
    if total == 0 {
        return Ok((0, Vec::new()));
    }
    let sample = model::list_pair_holders(role_a, role_b, SAMPLE_LIMIT).await?;
    Ok((total, sample))
}

/// 某用户当前已违反的互斥对（只看**已持有**，不看将要授予的角色）。
pub async fn user_conflicts(user_id: &Uuid) -> DbResult<Vec<PairHit>> {
    model::violations_for_user(user_id, &[]).await
}

/// 全量存量扫描：逐条生效规则取双持用户，只返回有违规的规则。
pub async fn scan() -> DbResult<ScanResult> {
    let rules = model::list_active_pairs().await?;
    let scanned_pairs = rules.len();
    let mut items = Vec::new();
    for rule in rules {
        let (total, sample) = pair_holders(&rule.role_a_id, &rule.role_b_id).await?;
        if total > 0 {
            items.push(ViolationGroup {
                rule,
                total,
                sample,
            });
        }
    }
    Ok(ScanResult {
        scanned_pairs,
        items,
    })
}

/// 汇总「违规规则数 / 违规人次」（纯函数，可无库单测）。
///
/// # 为什么是「人次」而不是「人数」
///
/// 同一用户可能同时命中两条规则（例如持有 A、B、C 而 A×B、B×C 都互斥），
/// 去重人数需要再查一次库；而管理员真正要处理的是**绑定**，人次更贴近工作量。
pub fn summarize(items: &[ViolationGroup]) -> (u64, u64) {
    let violated_pairs = u64::try_from(items.len()).unwrap_or(u64::MAX);
    let holders = items
        .iter()
        .map(|item| u64::try_from(item.total).unwrap_or(0))
        .sum();
    (violated_pairs, holders)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(total: i64) -> ViolationGroup {
        ViolationGroup {
            rule: MutexRow {
                id: Uuid::from_u128(1),
                role_a_id: Uuid::from_u128(2),
                role_a_code: "A".to_string(),
                role_a_name: "角色 A".to_string(),
                role_b_id: Uuid::from_u128(3),
                role_b_code: "B".to_string(),
                role_b_name: "角色 B".to_string(),
                reason: "合规".to_string(),
                enabled: true,
                created_at: chrono::Utc::now(),
                create_by: None,
            },
            total,
            sample: Vec::new(),
        }
    }

    /// 空清单：0 / 0（不是「1 条空规则」）。
    #[test]
    fn summarize_empty_scan() {
        assert_eq!(summarize(&[]), (0, 0));
    }

    /// 两条规则各自 1 人和 3 人 → 规则数 2、人次 4（人次按 total 累加，不按样本条数）。
    #[test]
    fn summarize_counts_holders_by_total_not_by_sample() {
        let items = vec![group(1), group(3)];
        assert_eq!(summarize(&items), (2, 4));
    }

    /// 负数 total（理论上不会出现）：按 0 计入，不让汇总变成天文数字。
    #[test]
    fn summarize_ignores_negative_total() {
        let items = vec![group(-5)];
        assert_eq!(summarize(&items), (1, 0));
    }
}
