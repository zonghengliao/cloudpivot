//! 结账相关真库用例的**公共夹具**（`#[cfg(test)]`）—— PERIOD-CLOSE 新增。
//!
//! # 为什么单独一个文件
//!
//! `period.rs` 的单测模块（`mod tests`）里已经有一套「连库 + 串行 + 按前缀清场」的
//! 夹具（`P1-` / `2031-xx`）。PERIOD-CLOSE 的明细 / 催办用例放在另外两个文件里，
//! 它们需要同一套基础设施，但**不能**再用 `2031-xx`：`period.rs` 的用例会自己把
//! 2031 整年删一遍，两组用例撞在一起就会互删夹具有。
//!
//! 所以本文件提供**第二套前缀**（期间 `2033-xx` / `2040-xx`、单号 `P2-`），两套井水不犯河水；
//! 连库 / 串行 / 清场的写法与 `period.rs::tests` 完全一致（同一个 `ERP_DB_URL`、
//! 同一个全局锁），见那边的头注释。
//!
//! # 期次命名空间（谁拥有哪一段，见「改造规范 R1」）
//!
//! | 期次 | 所有者 | 用途 |
//! |---|---|---|
//! | `2033-02..08` | `period_remind_tests` / `stocktake_tests` | 催办 / 盘点（与 `period::tests` 共用一把锁） |
//! | **`2040-01..12`** | **`period_items_tests`（独占）** | 结账明细用例，**一条用例一个期次**，见 [`open_case_period`] |
//!
//! `period_items` 从 2033 迁到 2040 的原因（卡 `t_f404258c`）：2033 的 03–08 同时被
//! `period_remind_tests` 占用，两条用例撞同一期次时「按期间读条数」的断言会互相污染
//! （实测：`draft_voucher_items('2033-03')` 别人多种一张就变 4）。2040 目前无人使用
//! （2034–2035 = opening、2036–2038 = year_end、2039 = reminder，见各模块头注释）。
//!
//! # 铁律（三条都不能破）
//!
//! 1. **绝不碰 2026-xx**：生产期间在 2026，其它模块的夹具往 2026-10 记凭证；
//!    本模块把 2026-10 封了，它们的用例会立刻收到「期间已结账」的 400。
//! 2. **串行**（[`DB_LOCK`]）：清理按前缀删，并行会互删夹具。
//! 3. **写操作只按 id 集合**：[`cleanup`] 先在本夹具**拥有的**期次/号段里查出 id，
//!    再 `WHERE id IN (…)` 删 —— 不再出现「按 period 整期改数」的 UPDATE/DELETE
//!    （改造规范 R3/R4）。
//!
//! # 用法
//!
//! ```ignore
//! let Some(_guard) = support::setup().await else { return; };  // 没有 ERP_DB_URL 时 SKIP
//! let row = support::open_case_period(3).await;                // 2040-03（本用例独占）
//! let period = row.period_code.clone();
//! support::seed_voucher(&period, "P2-V-001", "DRAFT", ("10","10"), ("10","10")).await;
//! ```

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::PeriodRow;
use crate::business::erp::period;
use crate::business::shared::state_machine;

/// 真库用例串行跑的**共享锁**：本文件、`period::tests`、`stocktake_tests`
/// 三处夹具都在 `2033-xx` 上工作（期间 / 凭证 / 单据），必须拿**同一个对象**。
///
/// 为什么要合并（实测）：过去三处各有一把 `DB_LOCK`，于是
/// `period::tests::cleanup()`（删 `period LIKE '2033-%'` + `biz_accounting_period
/// LIKE '2033-%'`）会在 `period_items::tests` 用例体跑到一半时把它刚种的
/// 2033-10 草稿凭证删掉 ⇒ `!blocked.can_close` 失败；`stocktake_tests` 的
/// 2033-03 单据与期间同理会凭空消失。锁合并后「谁在 2033 上动夹具」互斥。
pub static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 夹具操作人（`closed_by` / `sender_id` 是弱引用，不要求真实用户）。
pub const TEST_UID: &str = "00000000-0000-7000-8000-0000000000c1";

/// 夹具操作人 id。
pub fn uid() -> Uuid {
    Uuid::parse_str(TEST_UID).expect("测试 uuid 非法")
}

/// 夹具操作人。
pub fn operator() -> state_machine::Operator {
    state_machine::Operator::named(Some(uid()), "pc-tester")
}

/// 连库（只从 `ERP_DB_URL` 取；缺省 SKIP）。
pub async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("ERP_DB_URL") else {
                println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                return false;
            };
            if try_db().is_some() {
                return true;
            }
            // 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级全局池：
            // `max_lifetime = 0` 逼池子每次新建连接，避免借到上一个 runtime 的死连接。
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.max_lifetime = Some(std::time::Duration::ZERO);
            config.pool.idle_timeout = Some(std::time::Duration::ZERO);
            match Db::connect_with(&url, config).await {
                Ok(connection) => match set_db(connection) {
                    Ok(()) => true,
                    Err(err) => {
                        println!("SKIP: set_db 失败（{err}）");
                        false
                    }
                },
                Err(err) => {
                    println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

/// 执行一条写 SQL（失败直接 panic，不静默吞错）。
pub async fn sql(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 本夹具工具包**拥有**的期次命名空间（`period_items` 独占 2040；2033 与催办/盘点共管，
/// 但共用同一把 [`DB_LOCK`]，所以谁都不会在别人跑到一半时动手）。
const OWNED_PERIOD_PREFIXES: [&str; 2] = ["2033-%", "2040-%"];

/// 查一列 `id`（清场第一半：**只按本夹具拥有的范围取 id**，不做任何写操作）。
async fn uuids_of(query: &str) -> Vec<Uuid> {
    let rows = db()
        .query_sql(query.to_string(), vec![])
        .await
        .unwrap_or_else(|err| panic!("查夹具残留 id 失败：{err}\nSQL: {query}"));
    rows.iter()
        .map(|row| row.get_typed::<Uuid>("id").expect("夹具 id"))
        .collect()
}

/// `IN ('<uuid>', …)` 片段（uuid 是库自己生成的，拼进 SQL 安全）。
fn in_uuids(ids: &[Uuid]) -> String {
    ids.iter()
        .map(|id| format!("'{id}'::uuid"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `IN ('<uuid>', …)` 片段（用于 `biz_id` 这类 **text** 弱引用列）。
fn in_texts(ids: &[Uuid]) -> String {
    ids.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(", ")
}

/// 清掉本套夹具（**只删本夹具拥有的期次/号段里的行，且写操作一律按 id 集合**）。
///
/// # 为什么先 SELECT 再按 id 删（改造规范 R3/R4）
///
/// 过去的写法是「按 period 前缀整期删凭证」：删除范围 = 整个 2033 年，
/// 而 2033-03 是 `stocktake_tests` 的夹具期、2033-02..08 是 `period_remind_tests` 的 ——
/// 一旦哪天哪处没拿同一把锁，这里就会把别人正在用的夹具删掉（卡 `t_f404258c` 的根因族）。
///
/// 现在的两段式：① 在**本夹具拥有**的期次/号段里读出 id（读的范围有明确所有权）；
/// ② `DELETE … WHERE id IN (…)` —— 写操作对象是逐一登记的 id，不再「按期间批量改数」。
pub async fn cleanup() {
    // ---- ① 期次命名空间内的凭证 / 期间（催办通知按凭证 id 派生）----
    let vouchers = uuids_of(&format!(
        "SELECT id FROM biz_voucher WHERE period LIKE '{}' OR period LIKE '{}'",
        OWNED_PERIOD_PREFIXES[0], OWNED_PERIOD_PREFIXES[1]
    ))
    .await;
    if !vouchers.is_empty() {
        sql(&format!(
            "DELETE FROM sys_notice WHERE biz_type = 'period_remind' AND biz_id IN ({})",
            in_texts(&vouchers)
        ))
        .await;
        // 分录随凭证 CASCADE
        sql(&format!(
            "DELETE FROM biz_voucher WHERE id IN ({})",
            in_uuids(&vouchers)
        ))
        .await;
    }
    let periods = uuids_of(&format!(
        "SELECT id FROM biz_accounting_period \
          WHERE period_code LIKE '{}' OR period_code LIKE '{}'",
        OWNED_PERIOD_PREFIXES[0], OWNED_PERIOD_PREFIXES[1]
    ))
    .await;
    if !periods.is_empty() {
        // 状态日志没有外键：必须按期间 id 先删（期间行一删就再也定位不到那些日志）
        sql(&format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' \
             AND entity_id IN ({})",
            in_uuids(&periods)
        ))
        .await;
        sql(&format!(
            "DELETE FROM biz_accounting_period WHERE id IN ({})",
            in_uuids(&periods)
        ))
        .await;
    }

    // ---- ② 单据号段（`P2-*` 是本夹具独占的号段）：取 id → 按 id 删 ----
    for (table, column, prefix) in [
        ("biz_purchase_order", "order_no", "P2-PO-%"),
        ("biz_stock_in", "in_no", "P2-IN-%"),
        ("biz_stock_transfer", "transfer_no", "P2-TR-%"),
    ] {
        let ids = uuids_of(&format!("SELECT id FROM {table} WHERE {column} LIKE '{prefix}'")).await;
        if !ids.is_empty() {
            sql(&format!("DELETE FROM {table} WHERE id IN ({})", in_uuids(&ids))).await;
        }
    }

    // ---- ③ 夹具角色 / 用户 / 权限节点（`P2-` / `p2.` / `P2I-` / `p2i.` 都是本夹具的号段）----
    // 外键顺序：先删关系，再删主体。
    let roles = uuids_of("SELECT id FROM sys_role WHERE role_code LIKE 'P2-%' OR role_code LIKE 'P2I-%'").await;
    let users = uuids_of("SELECT id FROM sys_user WHERE username LIKE 'p2.%' OR username LIKE 'p2i.%'").await;
    if !roles.is_empty() || !users.is_empty() {
        let mut where_parts = Vec::new();
        if !roles.is_empty() {
            where_parts.push(format!("role_id IN ({})", in_uuids(&roles)));
        }
        if !users.is_empty() {
            where_parts.push(format!("user_id IN ({})", in_uuids(&users)));
        }
        let clause = where_parts.join(" OR ");
        sql(&format!("DELETE FROM sys_user_role WHERE {clause}")).await;
        if !roles.is_empty() {
            sql(&format!(
                "DELETE FROM sys_role_menu WHERE role_id IN ({})",
                in_uuids(&roles)
            ))
            .await;
            sql(&format!("DELETE FROM sys_role WHERE id IN ({})", in_uuids(&roles))).await;
        }
        if !users.is_empty() {
            sql(&format!("DELETE FROM sys_user WHERE id IN ({})", in_uuids(&users))).await;
        }
    }
    let menus = uuids_of(
        "SELECT id FROM sys_menu \
          WHERE auth_code LIKE 'business:erp:p2-%' OR auth_code LIKE 'business:erp:p2i-%'",
    )
    .await;
    if !menus.is_empty() {
        sql(&format!(
            "DELETE FROM sys_role_menu WHERE menu_id IN ({})",
            in_uuids(&menus)
        ))
        .await;
        sql(&format!("DELETE FROM sys_menu WHERE id IN ({})", in_uuids(&menus))).await;
    }
}

/// 取锁 + 连库 + 清场。`None` = 没有测试库，用例直接 return（SKIP）。
pub async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 改一张凭证的状态（**按 id**，不按期间整期 UPDATE）。
///
/// 为什么必须按 id：`can_close_is_unaffected_by_items` 过去写的是
/// 「把整个 2033-10 期次的凭证一起改成 POSTED」，在并行跑时会把
/// 别的用例刚种进同一期间的草稿凭证一并过账 —— 那几个用例的断言（「必须还有草稿挡路」）
/// 就随机变红。夹具只许动自己造的那一行。
pub async fn set_voucher_status(id: Uuid, status: &str) {
    db().execute_with_params(
        "UPDATE biz_voucher SET status = ? WHERE id = ?",
        vec![Value::Text(status.to_string()), Value::from(&id)],
    )
    .await
    .unwrap_or_else(|err| panic!("改凭证状态失败（{id} → {status}）：{err}"));
}

/// 开一个测试期间（`2033-MM`）并读回行。
///
/// 供 `period_remind_tests` / 其它 2033 号段的调用方使用（**共享号段**，靠 [`DB_LOCK`] 串行）。
/// `period_items` 自己的用例一律改用 [`open_case_period`]（独占号段 + 用例唯一）。
pub async fn open_test_period(month: i16) -> PeriodRow {
    open_period_in(2033, month, "P2 夹具").await
}

/// `period_items` 独占的测试期年（见文件头「期次命名空间」）。
pub const ITEMS_PERIOD_YEAR: i16 = 2040;

/// 已分配的用例月号（`true` = 该期次已被某条用例占用）。
///
/// 用途是把「两条用例撞同一期次」从**约定**变成**运行期断言**：重复申请直接 panic，
/// 而不是让两条用例在同一个期间里互相把对方的凭证算进条数（卡 `t_f404258c` 的假红形态）。
static CASE_PERIOD_TAKEN: std::sync::Mutex<[bool; 13]> = std::sync::Mutex::new([false; 13]);

/// 开一条用例**独占**的期次并读回行（`2040-<用例序号>`）。
///
/// * `case_no` = 期次月号 = 用例序号，必须是 1..=12 且**全模块唯一**；
/// * 同一进程内重复申请同一序号 → `panic`（这就是「同模块任意两条用例不落同一期次」的门禁）；
/// * 跨进程/上一轮残留由 [`cleanup`] 在本模块独占的 `2040-%` 里按 id 清掉。
pub async fn open_case_period(case_no: i16) -> PeriodRow {
    assert!(
        (1..=12).contains(&case_no),
        "用例序号必须是 1..=12（月号即用例序号）：{case_no}"
    );
    {
        let mut taken = CASE_PERIOD_TAKEN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let slot = usize::try_from(case_no).expect("序号转 usize");
        assert!(
            !taken[slot],
            "期次 {ITEMS_PERIOD_YEAR}-{case_no:02} 已被另一条用例占用：\
             每条用例必须有自己唯一的期次（见 period_test_support 头注释「期次命名空间」）"
        );
        taken[slot] = true;
    }
    let row = open_period_in(ITEMS_PERIOD_YEAR, case_no, "P2I 夹具（用例独占期次）").await;
    let expected = format!("{ITEMS_PERIOD_YEAR}-{case_no:02}");
    assert_eq!(row.period_code, expected, "开出来的期次必须与用例序号一致");
    row
}

/// 开账 + 读回行（[`open_test_period`] / [`open_case_period`] 的唯一实现）。
async fn open_period_in(year: i16, month: i16, remark: &str) -> PeriodRow {
    let res = period::open_period(year, month, Some(remark), operator())
        .await
        .unwrap_or_else(|error| panic!("开账失败（{year}-{month:02}）：{}", error.msg));
    let id = Uuid::parse_str(&res.id).expect("期间 id 非法");
    period::load_period_row(db(), id)
        .await
        .expect("读期间行")
        .expect("期间行应存在")
}

/// 造一张凭证（单头 + 两条分录）；`head` / `entries` 分开给，便于造「分录被改过」的坏账。
pub async fn seed_voucher(
    period: &str,
    voucher_no: &str,
    status: &str,
    head: (&str, &str),
    entries: (&str, &str),
) -> Uuid {
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_voucher \
            (id, voucher_no, period, voucher_date, total_debit, total_credit, entry_count, \
             status, summary, voucher_type, source_type) \
         VALUES (?, ?, ?, ?::text::date, ?::text::numeric, ?::text::numeric, 2, ?, ?, \
                 'MANUAL', 'MANUAL')",
        vec![
            Value::from(&id),
            Value::Text(voucher_no.to_string()),
            Value::Text(period.to_string()),
            Value::Text(format!("{period}-15")),
            Value::Text(head.0.to_string()),
            Value::Text(head.1.to_string()),
            Value::Text(status.to_string()),
            Value::Text(format!("P2 夹具 {voucher_no}")),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入凭证头失败：{err}"));
    for (line_no, code, name, debit, credit) in [
        (1, "1001", "库存现金", entries.0, "0"),
        (2, "3001", "实收资本", "0", entries.1),
    ] {
        db().execute_with_params(
            "INSERT INTO biz_voucher_entry \
                (voucher_id, line_no, summary, account_id, account_code, account_name, \
                 debit_amount, credit_amount) \
             VALUES (?, ?, ?, ?, ?, ?, ?::text::numeric, ?::text::numeric)",
            vec![
                Value::from(&id),
                Value::I64(i64::from(line_no)),
                Value::Text(format!("P2 夹具 {voucher_no}")),
                Value::from(&Uuid::now_v7()),
                Value::Text(code.to_string()),
                Value::Text(name.to_string()),
                Value::Text(debit.to_string()),
                Value::Text(credit.to_string()),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("插入分录失败：{err}"));
    }
    id
}

/// 造一张采购单（`order_date` 取期间内的 15 日；金额 0，满足 `ck_biz_po_tax_sum`）。
pub async fn seed_purchase_order(period: &str, order_no: &str, status: &str) -> Uuid {
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_purchase_order \
            (id, order_no, supplier_id, order_date, warehouse_id, status, create_by) \
         VALUES (?, ?, ?, ?::text::date, ?, ?, ?)",
        vec![
            Value::from(&id),
            Value::Text(order_no.to_string()),
            Value::from(&Uuid::now_v7()),
            Value::Text(format!("{period}-15")),
            Value::from(&Uuid::now_v7()),
            Value::Text(status.to_string()),
            Value::from(&uid()),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入采购单失败：{err}"));
    id
}

/// 造一张收货单（无源入库：`in_type = OTHER` + `is_sourceless = TRUE`，见 V0199 的两条 CHECK）。
///
/// 入库单有三条互相咬合的约束，夹具必须同时满足（否则插不进去）：
/// * `ck_biz_si_supplier_scope`：非 PURCHASE 类型**不许**带 supplier_id；
/// * `ck_biz_si_sourceless_pair`：`is_sourceless = (source_order_id IS NULL)`；
/// * `ck_biz_si_pending_cancel`：`PENDING_POST / PENDING_CANCEL` 必须带审批实例 id ——
///   夹具只造 `DRAFT`（要造待审批态得先拉一条真实审批实例，超出本卡范围）。
pub async fn seed_stock_in(period: &str, in_no: &str, status: &str) -> Uuid {
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_stock_in \
            (id, in_no, in_type, warehouse_id, in_date, status, create_by, is_sourceless) \
         VALUES (?, ?, 'OTHER', ?, ?::text::date, ?, ?, TRUE)",
        vec![
            Value::from(&id),
            Value::Text(in_no.to_string()),
            Value::from(&Uuid::now_v7()),
            Value::Text(format!("{period}-15")),
            Value::Text(status.to_string()),
            Value::from(&uid()),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入收货单失败：{err}"));
    id
}

/// 造一张调拨单（需要两个已存在的仓库：`biz_stock_transfer` 对仓库有硬外键）。
///
/// `ck_biz_transfer_state` 把「状态 ↔ 时间戳」钉死（`IN_TRANSIT` 必须
/// `submitted_at / approved_at / shipped_at` 齐全 + `transit_warehouse_id` 非空），
/// 所以夹具按状态把时间戳一起填上 —— 只写 status 会被库当场拒掉。
///
/// 返回 `None` = 隔离库没有两个仓库，用例应 SKIP（不伪造主数据）。
pub async fn seed_stock_transfer(period: &str, transfer_no: &str, status: &str) -> Option<Uuid> {
    let rows = db()
        .query_sql(
            "SELECT id FROM biz_warehouse WHERE deleted_at IS NULL ORDER BY id LIMIT 2".to_string(),
            vec![],
        )
        .await
        .expect("查仓库");
    if rows.len() < 2 {
        return None;
    }
    let from: Uuid = rows[0].get_typed("id").expect("仓库 id");
    let to: Uuid = rows[1].get_typed("id").expect("仓库 id");
    let stamp = |set: bool| {
        if set {
            Value::Text(chrono::Utc::now().to_rfc3339())
        } else {
            Value::Null
        }
    };
    let (submitted, approved, shipped, transit) = match status {
        "PENDING" => (true, false, false, false),
        "APPROVED" => (true, true, false, false),
        "IN_TRANSIT" | "DONE" => (true, true, true, true),
        _ => (false, false, false, false),
    };
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_stock_transfer \
            (id, transfer_no, from_warehouse_id, to_warehouse_id, status, transfer_date, create_by, \
             submitted_at, approved_at, shipped_at, transit_warehouse_id) \
         VALUES (?, ?, ?, ?, ?, ?::text::date, ?, \
                 ?::text::timestamptz, ?::text::timestamptz, ?::text::timestamptz, ?)",
        vec![
            Value::from(&id),
            Value::Text(transfer_no.to_string()),
            Value::from(&from),
            Value::from(&to),
            Value::Text(status.to_string()),
            Value::Text(format!("{period}-15")),
            Value::from(&uid()),
            stamp(submitted),
            stamp(approved),
            stamp(shipped),
            if transit { Value::from(&from) } else { Value::Null },
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入调拨单失败：{err}"));
    Some(id)
}

// ==================== 权限 / 角色 / 用户夹具 ====================

/// 权限码对应的 `sys_menu` 节点 id（不存在 → `None`）。
pub async fn menu_id_of(code: &str) -> Option<Uuid> {
    db().find_with_params(
        "SELECT id FROM sys_menu WHERE auth_code = ? AND deleted_at IS NULL",
        vec![Value::Text(code.to_string())],
    )
    .await
    .expect("查权限节点")
    .first()
    .map(|row| row.get_typed::<Uuid>("id").expect("节点 id"))
}

/// 造一个夹具角色（`sort_order = -1`：反查时排最前，结果确定）。
pub async fn seed_role(role_code: &str) -> Uuid {
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO sys_role (id, role_code, role_name, status, sort_order) \
         VALUES (?, ?, ?, 1, -1)",
        vec![
            Value::from(&id),
            Value::Text(role_code.to_string()),
            Value::Text(format!("夹具角色 {role_code}")),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入角色失败：{err}"));
    id
}

/// 造一个夹具权限节点（`auth_code` 必须用 `business:erp:p2-` 前缀，清理按它删）。
pub async fn seed_menu(auth_code: &str) -> Uuid {
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO sys_menu (id, name, menu_type, auth_code, sort_order, status, title, \
                               is_visible, module_code) \
         VALUES (?, ?, 3, ?, 0, 1, ?, TRUE, 'business')",
        vec![
            Value::from(&id),
            Value::Text(format!("p2-{auth_code}")),
            Value::Text(auth_code.to_string()),
            Value::Text(format!("夹具权限 {auth_code}")),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入权限节点失败：{err}"));
    id
}

/// 给角色授权一个权限节点（幂等）。
pub async fn grant(role_id: Uuid, menu_id: Uuid) {
    sql(&format!(
        "INSERT INTO sys_role_menu (id, role_id, menu_id) \
         VALUES (gen_random_uuid(), '{role_id}', '{menu_id}') \
         ON CONFLICT (role_id, menu_id) DO NOTHING"
    ))
    .await;
}

/// 造一个夹具用户（`username` 必须用 `p2.` 前缀，清理按它删）。
pub async fn seed_user(username: &str, real_name: &str) -> Uuid {
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES (?, ?, 'p2-fixture-not-a-real-hash', ?, 1)",
        vec![
            Value::from(&id),
            Value::Text(username.to_string()),
            Value::Text(real_name.to_string()),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入用户失败：{err}"));
    id
}

/// 把用户挂到角色上（幂等）。
pub async fn assign(user_id: Uuid, role_id: Uuid) {
    sql(&format!(
        "INSERT INTO sys_user_role (id, user_id, role_id) \
         VALUES (gen_random_uuid(), '{user_id}', '{role_id}') \
         ON CONFLICT (user_id, role_id) DO NOTHING"
    ))
    .await;
}

/// 保证「某权限码有角色持有」：建一个 `sort_order = -1` 的夹具角色 + 授权 +（可选）挂用户。
///
/// 隔离库里往往只有 `SUPER_ADMIN` 有授权行（它会短路全部校验，所以种子迁移没给别的角色勾），
/// 于是「该谁处理」在裸库里是**待分配**。用例需要验证「能反查到人」时，就按
/// 管理员在「系统管理 · 菜单权限」里勾选的方式补一个夹具角色 —— 测的是反查逻辑，
/// 不是这个库的授权现状。
///
/// 夹具角色 `sort_order = -1`：多个角色同时持有该权限时它排最前，反查结果确定。
pub async fn ensure_role_for(code: &str, role_code: &str, users: &[(&str, &str)]) -> Uuid {
    let menu = menu_id_of(code)
        .await
        .unwrap_or_else(|| panic!("权限码 {code} 没有 sys_menu 节点（种子迁移未应用？）"));
    let role = seed_role(role_code).await;
    grant(role, menu).await;
    for (username, real_name) in users {
        let user = seed_user(username, real_name).await;
        assign(user, role).await;
    }
    role
}
