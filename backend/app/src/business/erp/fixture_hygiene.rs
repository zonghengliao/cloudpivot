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
    // P2-5 词表收敛：调拨单的 entity_type 旧值 `TRANSFER` 已统一为 `STOCK_TRANSFER`
    // （与 doc_type / 审批 biz_type 同名），V0219 已改存量。
    ("STOCK_TRANSFER", "biz_stock_transfer"),
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
        let sql = format!("DELETE FROM biz_status_log WHERE {}", orphan_where(entity_type));
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

/// 「付款 / 应付链路**种子行**」的总互斥锁（`#[cfg(test)]`）。
///
/// # 为什么这两个模块必须共用**同一把**锁
///
/// `payable_payment::tests`（P7/D5）与 `payable_bridge::tests`（P17）写的是同一批
/// **全局行**，其中最关键的是**种子流程定义** `biz_flow_def`：
///
/// * P17 的「显式跳过审批的直付」用例会把 `flow_code = 'payable_pay'` 临时改成
///   `status = 'DISABLED'`，跑完再改回 `PUBLISHED`；
/// * P7 的 `setup()` 反过来会把 `payable_pay` 置回 `PUBLISHED`（自愈上一轮的残留）。
///
/// 两把互不相识的锁时，P7 的 setup 会插在 P17 的「DISABLE → 提交付款」中间把流程**重新发布**，
/// 于是 P17 的 `submit_payment` 拿到 10001「有流程时必须走审批」—— 干净库全量第一轮实测就红
/// （卡 `t_0d7c51f8`：`payable_bridge::tests::zz_teardown_purges_payment_status_logs`，
/// 2323 passed / 1 failed）。夹具没法靠「换个唯一前缀」隔离：流程定义按 `flow_code` 查、
/// 是种子行，全库只有一行。按「同一批全局行 = 同一把锁」（测试卫生铁律）合并锁是唯一正确解。
///
/// 除此之外两者还共用 `biz_payable` / `biz_payment` / `biz_voucher` 与 `biz_seq` 号段，
/// 本来就不该并行。
pub(crate) static FINANCE_SEED_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 「共享种子角色」组锁（`#[cfg(test)]`）：`sys_user_role` 上 `ROLE_PM` / `ROLE_GM`
/// 这两条**共享种子角色**的绑定关系。
///
/// # 为什么写这两个角色绑定的模块必须共用同一把锁
///
/// `PURCHASE_MANAGER` / `GENERAL_MANAGER` 是 V0182 种子里的固定角色，审批流程定义
/// （`po_approve` / `contract_approve`）的审批节点按**角色**展开待办。于是：
///
/// * `purchase_handler::tests::setup_approval()` 会摘掉「不属于自己的」绑定；
/// * `contract_approval_tests::setup()` 过去也做对称的摘取（卡 `t_f404258c` 判定为
///   「抢别人绑定」的越权写，已删除）。
///
/// 两把互不相识的锁时，「谁 setup 谁占位」不成立：一边正在跑审批、另一边 setup 把它的
/// 夹具账号从角色上摘掉 ⇒ 待办展开不到人 / 或者多出别人的账号 ⇒
/// `assert_eq!(tasks.len(), 1)` 随机假红（实测：同一库上两个进程并行，`purchase_handler`
/// 一轮 5 条红、`contract_approval` 反向也会被摘）。改夹具前缀解决不了：角色是种子行。
///
/// 按「同一批全局行 = 同一把锁」把**写这两个角色绑定的模块**并入本锁即可（与
/// [`FINANCE_SEED_LOCK`] 同思路，互不嵌套，不会死锁）。
pub(crate) static SEED_ROLE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 「整库不许留孤儿」的**收敛式**判定：删 → 复查，最多等 `max_wait`，返回最终剩余条数。
///
/// # 为什么不能「purge 一次就断言 0」
///
/// 真库用例是**并行**跑的（libtest 默认 4 个测试线程，`cargo test -j N` 的 `-j` 只是编译
/// 并行度）。别的模块随时可能正处在「单据行已删、它自己的状态日志还没删」的毫秒级中间态里
/// —— 那一瞬间本模块的「整库无孤儿」断言会把**别人的中间态**判成自己的假红。实测（卡
/// `t_0d7c51f8`）：干净隔离库上第一轮 `cargo test`，`payable_bridge::tests::
/// zz_teardown_purges_payment_status_logs` 就红在这一句上，单跑/单模块跑全绿。
///
/// # 口径没有放宽
///
/// 真孤儿（没有任何模块会再回来删）等多久都还在 ⇒ 仍然红。收敛只吸收「别人正在删」的
/// 瞬时窗口，所以断言强度 = 原来的「整库最终无孤儿」，而不是「这一瞬间恰好没有」。
pub(crate) async fn settle_orphans(entity_types: &[&str], max_wait: std::time::Duration) -> i64 {
    let step = std::time::Duration::from_millis(50);
    let mut waited = std::time::Duration::ZERO;
    loop {
        purge_orphans(entity_types).await;
        let left = orphans(entity_types).await;
        if left == 0 || waited >= max_wait {
            return left;
        }
        // 用阻塞 sleep 而不是 `tokio::time`：每个 `#[tokio::test]` 是独立的
        // current-thread runtime，本函数内部没有别的任务需要被调度，阻塞等待最直接。
        std::thread::sleep(step);
        waited += step;
    }
}

/// 已登记类型的全量清单（模块级 teardown 的「整库不许留孤儿」断言用）。
pub(crate) const ALL_TYPES: &[&str] = &[
    "STOCK_IN",
    "PURCHASE_ORDER",
    "STOCK_TRANSFER",
    "PAYABLE",
    "VOUCHER",
    "ACCOUNTING_PERIOD",
];
