//! 夹具卫生（`#[cfg(test)]`）：`biz_status_log` 的**孤儿行**统一判据与清理。
//!
//! # 为什么需要它
//!
//! `biz_status_log` **没有外键**（它是审计留痕表，刻意不跟单据级联），所以
//! 「删了单据」不会带走它的状态日志。各模块的夹具清场如果只删业务行、不先删日志，
//! 就会在测试库里留下一批 `entity_id` 指向已不存在单据的孤儿行 ——
//! 每跑一遍全量就多攒一批，永远不会被任何模块的「按前缀清场」收走。
//!
//! # 判据为什么是「引用已失效」而不是号段前缀
//!
//! 夹具单号（`AP-P7-*` / `P3B-*`）与本夹具自己造的号容易混，而**生产号段**
//! （`AP-YYYYMMDD-*` / `PZ-YYYYMMDD-*`）恰恰是真实业务入口造的（收货入库 / 付款 / 红冲），
//! 按前缀删会误伤真数据（技能 §6.40：`PM-%` 其实是生产号段，一次误删把真实付款凭证清空）。
//! 孤儿行按定义就是垃圾，所以判据只能是：
//!
//! ```sql
//! entity_type = '<TYPE>' AND entity_id NOT IN (SELECT id FROM <单据表>)
//! ```
//!
//! # 用法
//!
//! ① **根因修复**：各模块的清场函数在删业务行**之前**按 `entity_id` 精确删日志
//! （日志没外键，业务行一删就再也查不到 id 了）；
//! ② **兜底**：模块级 `zz_` teardown 调 [`purge_orphans`] + [`orphans`]，
//! 把本模块或历史上别的模块留下的孤儿一次性收干，并断言为 0。
//!
//! 只清 `biz_status_log`，**不动**任何业务行 —— 宁可少删，不可误删。

#![cfg(test)]

use nik::prelude::*;

/// `entity_type` → 单据表（`biz_status_log` 没有外键，只能按「引用是否还活着」判断）。
///
/// 只登记**本卡口径内**的六类：`ACCOUNTING_YEAR` 的 `entity_id` 指向期间行而不是独立表，
/// 与会计期间用例的正常产物混在一起，不在此处清理（见 `year_end` 自己的清场）。
pub(crate) const ENTITY_TABLES: &[(&str, &str)] = &[
    ("STOCK_IN", "biz_stock_in"),
    ("PURCHASE_ORDER", "biz_purchase_order"),
    ("TRANSFER", "biz_stock_transfer"),
    ("PAYABLE", "biz_payable"),
    ("VOUCHER", "biz_voucher"),
    ("ACCOUNTING_PERIOD", "biz_accounting_period"),
    // A2：合同状态流转也写 `biz_status_log`（`entity_type = 'CONTRACT'`），
    // 夹具清场不登记它就会在测试库里攒孤儿行（每跑一次多几条，永远收不走）。
    ("CONTRACT", "biz_contract"),
];

/// `entity_type` 对应的单据表；未登记的实体直接 panic（别静默跳过 —— 那等于「断言永远绿」）。
fn table_of(entity_type: &str) -> &'static str {
    ENTITY_TABLES
        .iter()
        .find(|(name, _)| *name == entity_type)
        .map(|(_, table)| *table)
        .unwrap_or_else(|| panic!("fixture_hygiene 未登记的单据类型：{entity_type}"))
}

/// 孤儿行判据（`entity_type` 已校验，直接拼进 SQL 是安全的）。
fn orphan_where(entity_type: &str) -> String {
    format!(
        "entity_type = '{entity_type}' AND entity_id NOT IN (SELECT id FROM {})",
        table_of(entity_type)
    )
}

/// 统计这些类型的孤儿行数（`entity_id` 指向已不存在的单据）。
pub(crate) async fn orphans(entity_types: &[&str]) -> i64 {
    let mut total = 0_i64;
    for entity_type in entity_types {
        let sql = format!(
            "SELECT count(*) FROM biz_status_log WHERE {}",
            orphan_where(entity_type)
        );
        let n: i64 = db()
            .scalar_with_params::<Option<i64>>(&sql, vec![])
            .await
            .unwrap_or_else(|err| panic!("统计 {entity_type} 孤儿状态日志失败：{err}"))
            .unwrap_or(0);
        total += n;
    }
    total
}

/// 物理删除这些类型的孤儿行，返回删掉的行数（失败直接 panic —— 不静默吞错）。
pub(crate) async fn purge_orphans(entity_types: &[&str]) -> i64 {
    let mut total = 0_i64;
    for entity_type in entity_types {
        let sql = format!(
            "DELETE FROM biz_status_log WHERE {}",
            orphan_where(entity_type)
        );
        let deleted = db()
            .execute_with_params(&sql, vec![])
            .await
            .unwrap_or_else(|err| panic!("清理 {entity_type} 孤儿状态日志失败：{err}"));
        total += i64::try_from(deleted).unwrap_or(0);
    }
    total
}

/// 本卡口径：凭证 / 应付两类（`t_7f47221c` 的验收对象）。
pub(crate) const FINANCE: &[&str] = &["PAYABLE", "VOUCHER"];

/// 已登记类型的全量清单（模块级 teardown 的「整库不许留孤儿」断言用）。
pub(crate) const ALL_TYPES: &[&str] = &[
    "STOCK_IN",
    "PURCHASE_ORDER",
    "TRANSFER",
    "PAYABLE",
    "VOUCHER",
    "ACCOUNTING_PERIOD",
];
