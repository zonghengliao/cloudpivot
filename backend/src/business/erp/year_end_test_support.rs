//! 年结（YEAR-END）真库用例的**公共夹具**（`#[cfg(test)]`）。
//!
//! # 用什么年度、为什么
//!
//! | 年度 | 谁在用 | 本模块 |
//! |------|--------|--------|
//! | 2026 | 生产在用的会计年度 / 别的卡的夹具 | **绝不碰** |
//! | 2021–2023 | `year_end.rs` 自带的 15 条用例 | 不碰 |
//! | 2031–2033 | `period.rs` / `period_items` / `period_remind` | 不碰 |
//! | 2034–2035 | `opening.rs` 期初用例 | 不碰 |
//! | **2037 / 2038** | **本次（本模块）** | 本年 2037、下一年度 2038 |
//!
//! 年结要「12 个月全部结账 + 下一年度 12 个月已开账」，所以夹具一次开 24 个期间；
//! 单号前缀 `YE-`，夹具用户 `ye.`，夹具角色 `YE-`，清场按它们删。
//!
//! # 铁律（两条都不能破）
//!
//! 1. **绝不碰 2026-xx**：那是生产年度，别的模块的夹具往那里记凭证；
//! 2. **串行**（[`DB_LOCK`]）：清理按前缀删，并行会互删夹具。
//!    （统一跑法：`cargo test --lib ... -- --test-threads=1`）
//!
//! # 与 `period_test_support` 的关系
//!
//! 造单据的三把工具（采购 / 收货 / 调拨）与角色 / 用户夹具直接复用那边的实现
//! （`seed_purchase_order` / `ensure_role_for` …），本文件只负责**年度区间**与**清场**：
//! 少一份「怎么造一张草稿采购单」的副本，就少一处会漂移的口径。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::period;
use crate::business::erp::period_test_support as base;
use crate::business::shared::state_machine;

/// 本年（被年结的年度）。
pub const YEAR: i16 = 2037;
/// 下一年度（年结要往它写期初，必须先开账）。
pub const NEXT_YEAR: i16 = 2038;

/// 真库用例串行跑（**与 `period_test_support::DB_LOCK` 是同一把锁**；统一用 `--test-threads=1` 跑）。
///
/// # 为什么后来改成共享一把锁（E2E04-FIX-2）
///
/// `period::reopen_period` 现在会判「**更晚的年度**是否已年结」（跨年度挡路），而本模块的
/// 夹具正是把 `<YEAR> 年` 标成 `ACCOUNTING_YEAR=CLOSED`。两把独立的锁意味着
/// `cargo test --lib` 并行跑时，本模块刚写下的 2037 年结状态会被 2031 的反结账用例看见
/// → 那不是缺陷，是夹具串扰造成的假红。共享一把锁把两者互斥掉（顺带也挡住
/// 期间 / 凭证 / 单据夹具的互删）。
pub use crate::business::erp::period_test_support::DB_LOCK;

/// 夹具操作人 id（`closed_by` / `sender_id` 是弱引用，不要求真实用户）。
pub const TEST_UID: &str = "00000000-0000-7000-8000-0000000000c7";

/// 夹具操作人 id。
pub fn uid() -> Uuid {
    Uuid::parse_str(TEST_UID).expect("测试 uuid 非法")
}

/// 夹具操作人（`sender_name` 会进通知，取证时一眼能看出是夹具发的）。
pub fn operator() -> state_machine::Operator {
    state_machine::Operator::named(Some(uid()), "ye-tester")
}

/// 执行一条写 SQL（失败直接 panic，不静默吞错）。
pub async fn sql(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 清掉本套夹具（**只删 2037 / 2038 与 `YE-` / `ye.` 前缀**）。
///
/// 顺序按外键依赖：通知 → 状态日志 → 期初 → 凭证（分录随凭证 CASCADE）→ 单据 → 期间 → 角色 / 用户。
pub async fn cleanup() {
    // 年结催办 / 期间催办留下的通知（`biz_id` 是单据 id 的文本）
    sql("DELETE FROM sys_notice \
          WHERE biz_type IN ('year_end_remind', 'period_remind') \
            AND biz_id IN (SELECT id::text FROM biz_purchase_order WHERE order_no LIKE 'YE-%' \
                           UNION SELECT id::text FROM biz_stock_in WHERE in_no LIKE 'YE-%' \
                           UNION SELECT id::text FROM biz_stock_transfer WHERE transfer_no LIKE 'YE-%' \
                           UNION SELECT id::text FROM biz_voucher WHERE period LIKE '2037-%' \
                                 OR period LIKE '2038-%')")
    .await;
    sql(
        "DELETE FROM biz_status_log WHERE entity_type IN ('ACCOUNTING_YEAR', 'ACCOUNTING_PERIOD') \
           AND (entity_no IN ('2037', '2038') \
                OR entity_no LIKE '2037-%' OR entity_no LIKE '2038-%')",
    )
    .await;
    sql("DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' \
           AND entity_id IN (SELECT id FROM biz_voucher \
                              WHERE period LIKE '2037-%' OR period LIKE '2038-%')")
    .await;
    sql("DELETE FROM biz_opening_balance WHERE period_code LIKE '2037-%' OR period_code LIKE '2038-%'").await;
    sql("DELETE FROM biz_voucher WHERE period LIKE '2037-%' OR period LIKE '2038-%'").await;
    sql("DELETE FROM biz_purchase_order WHERE order_no LIKE 'YE-%'").await;
    sql("DELETE FROM biz_stock_in WHERE in_no LIKE 'YE-%'").await;
    sql("DELETE FROM biz_stock_transfer WHERE transfer_no LIKE 'YE-%'").await;
    sql("DELETE FROM biz_accounting_period WHERE period_code LIKE '2037-%' OR period_code LIKE '2038-%'").await;
    // 夹具角色 / 用户（`ensure_role_for` 造的是 `P2-` 角色 + `p2.` 用户，本模块额外用 `YE-` / `ye.`）
    sql(
        "DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE 'ye.%') \
           OR role_id IN (SELECT id FROM sys_role WHERE role_code LIKE 'YE-%')",
    )
    .await;
    sql("DELETE FROM sys_role_menu WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE 'YE-%')").await;
    sql("DELETE FROM sys_role WHERE role_code LIKE 'YE-%'").await;
    sql("DELETE FROM sys_user WHERE username LIKE 'ye.%'").await;
    // 期间夹具模块（`P2-` / `p2.`）的残留也清一遍：`ensure_role_for` 来自那边。
    base::cleanup().await;
}

/// 取锁 + 连库 + 清场。`None` = 没有测试库，用例直接 return（SKIP，不假绿）。
pub async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !base::ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 清场（**每个用例结尾都要调**）。
///
/// # 为什么「只在 setup 清」不够
///
/// 本模块把 2037 全年封了来测年结。历史成因：`period::tests` 的反结账判据曾经是
/// **全库无界**的（`period_code > ? AND status='CLOSED'`），2037 的封账残留会让
/// 下一轮跑 `cargo test --lib` 时 2031 的反结账用例 409 —— 实测红过两次
/// （`p1_reopen_clears_closed_marks` / `p1_reopen_restores_posting`）。
///
/// E2E04-FIX-2 起判据收敛到同一会计年度（`period_year = ?`），2037 的封账**不再**挡
/// 2031；但本模块的残留仍会污染跨年度口径（跨年试算平衡 / 报表累计 / `period_year+1`
/// 的年结判断），所以每个用例**结束时还是清一遍**：留下的是「干净库」，而不是
/// 「看运气的库」。
pub async fn teardown() {
    cleanup().await;
}

/// 开一整段期间（`2037-01` … `2038-12`，本年 + 下一年度）。
pub async fn open_years() {
    period::batch_open(
        &format!("{YEAR}-01"),
        &format!("{NEXT_YEAR}-12"),
        Some("YE 夹具"),
        operator(),
    )
    .await
    .unwrap_or_else(|error| panic!("批量开账失败：{}", error.msg));
}

/// 把本年 12 个月直接置为「已结账」（本卡测的是年结侧的闸，不走 P1 的结账预检）。
pub async fn close_all_months() {
    sql(&format!(
        "UPDATE biz_accounting_period SET status = 'CLOSED', closed_at = now() \
          WHERE period_code LIKE '{YEAR:04}-%'"
    ))
    .await;
}

/// 开 24 个期间 + 封本年 12 个月（大多数用例的前置）。
pub async fn open_and_close() {
    open_years().await;
    close_all_months().await;
}

/// 月份 → 期间编码（`2037-07`）。
pub fn period_code(month: i16) -> String {
    period::period_code(YEAR, month)
}

/// 造一张挡路的采购单（`YE-PO-…`，月份决定落在本年的哪个月）。
pub async fn seed_po(order_no: &str, month: i16, status: &str) -> Uuid {
    base::seed_purchase_order(&period_code(month), order_no, status).await
}

/// 造一张挡路的收货单（`YE-IN-…`）。
pub async fn seed_stock_in(in_no: &str, month: i16, status: &str) -> Uuid {
    base::seed_stock_in(&period_code(month), in_no, status).await
}

/// 造一张「分录借贷不平」的已过账凭证（A1 不变量），用于验证 INVARIANT 明细。
pub async fn seed_broken_voucher(voucher_no: &str, month: i16) -> Uuid {
    base::seed_voucher(&period_code(month), voucher_no, "POSTED", ("100", "100"), ("60", "40")).await
}

// ==================== 授权快照（测「反查不到就留 null」时用） ====================

/// 某权限节点当前被哪些角色持有（`(id, role_id, menu_id)`）——删之前先存一份，测完还原。
pub async fn grants_of(menu_id: Uuid) -> Vec<(Uuid, Uuid, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT id, role_id, menu_id FROM sys_role_menu WHERE menu_id = ?::uuid",
            vec![Value::from(&menu_id)],
        )
        .await
        .expect("读授权行");
    rows.iter()
        .map(|row| {
            (
                row.get_typed::<Uuid>("id").expect("授权 id"),
                row.get_typed::<Uuid>("role_id").expect("角色 id"),
                row.get_typed::<Uuid>("menu_id").expect("节点 id"),
            )
        })
        .collect()
}

/// 还原授权行（幂等）。
pub async fn restore_grants(rows: &[(Uuid, Uuid, Uuid)]) {
    for (id, role_id, menu_id) in rows {
        sql(&format!(
            "INSERT INTO sys_role_menu (id, role_id, menu_id) VALUES ('{id}', '{role_id}', '{menu_id}') \
             ON CONFLICT (role_id, menu_id) DO NOTHING"
        ))
        .await;
    }
}

/// 权限码对应的 `sys_menu` 节点 id（不存在 → `None`）。
pub async fn menu_id_of(code: &str) -> Option<Uuid> {
    base::menu_id_of(code).await
}
