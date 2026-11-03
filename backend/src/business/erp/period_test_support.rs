//! 结账相关真库用例的**公共夹具**（`#[cfg(test)]`）—— PERIOD-CLOSE 新增。
//!
//! # 为什么单独一个文件
//!
//! `period.rs` 的单测模块（`mod tests`）里已经有一套「连库 + 串行 + 按前缀清场」的
//! 夹具（`P1-` / `2031-xx`）。PERIOD-CLOSE 的明细 / 催办用例放在另外两个文件里，
//! 它们需要同一套基础设施，但**不能**再用 `2031-xx`：`period.rs` 的用例会自己把
//! 2031 整年删一遍，两组用例撞在一起就会互删夹具有。
//!
//! 所以本文件提供**第二套前缀**（期间 `2033-xx`、单号 `P2-`），两套井水不犯河水；
//! 连库 / 串行 / 清场的写法与 `period.rs::tests` 完全一致（同一个 `ERP_DB_URL`、
//! 同一个全局锁），见那边的头注释。
//!
//! # 铁律（两条都不能破）
//!
//! 1. **绝不碰 2026-xx**：生产期间在 2026，其它模块的夹具往 2026-10 记凭证；
//!    本模块把 2026-10 封了，它们的用例会立刻收到「期间已结账」的 400。
//! 2. **串行**（[`DB_LOCK`]）：清理按前缀删，并行会互删夹具。
//!
//! # 用法
//!
//! ```ignore
//! let Some(_guard) = support::setup().await else { return; };  // 没有 ERP_DB_URL 时 SKIP
//! let period = support::open_test_period(3).await;             // 2033-03，返回 PeriodRow
//! support::seed_draft_voucher("2033-03", "P2-V-001").await;
//! ```

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::PeriodRow;
use crate::business::erp::period;
use crate::business::shared::state_machine;

/// 真库用例串行跑（与 `period.rs::tests::DB_LOCK` 同款：文件级互斥，避免并行清场互删）。
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

/// 清掉本套夹具（**只删 2033-xx 与 P2- 前缀**，不碰生产期间 2026-xx）。
pub async fn cleanup() {
    sql("DELETE FROM sys_notice WHERE biz_type = 'period_remind' \
         AND biz_id IN (SELECT id::text FROM biz_voucher WHERE period LIKE '2033-%')")
    .await;
    sql(
        "DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' \
         AND entity_no LIKE '2033-%'",
    )
    .await;
    // 分录随凭证 CASCADE
    sql("DELETE FROM biz_voucher WHERE period LIKE '2033-%'").await;
    sql("DELETE FROM biz_purchase_order WHERE order_no LIKE 'P2-PO-%'").await;
    sql("DELETE FROM biz_stock_in WHERE in_no LIKE 'P2-IN-%'").await;
    sql("DELETE FROM biz_stock_transfer WHERE transfer_no LIKE 'P2-TR-%'").await;
    sql("DELETE FROM biz_accounting_period WHERE period_code LIKE '2033-%'").await;
    // 夹具角色 / 用户 / 权限节点（外键顺序：先删关系，再删主体）
    sql("DELETE FROM sys_user_role WHERE user_id IN \
         (SELECT id FROM sys_user WHERE username LIKE 'p2.%') \
         OR role_id IN (SELECT id FROM sys_role WHERE role_code LIKE 'P2-%')")
    .await;
    sql("DELETE FROM sys_role_menu WHERE role_id IN \
         (SELECT id FROM sys_role WHERE role_code LIKE 'P2-%')")
    .await;
    sql("DELETE FROM sys_role WHERE role_code LIKE 'P2-%'").await;
    sql("DELETE FROM sys_user WHERE username LIKE 'p2.%'").await;
    sql("DELETE FROM sys_menu WHERE auth_code LIKE 'business:erp:p2-%'").await;
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

/// 开一个测试期间（`2033-MM`）并读回行。
pub async fn open_test_period(month: i16) -> PeriodRow {
    let res = period::open_period(2033, month, Some("P2 夹具"), operator())
        .await
        .unwrap_or_else(|error| panic!("开账失败（2033-{month:02}）：{}", error.msg));
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
            if transit {
                Value::from(&from)
            } else {
                Value::Null
            },
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
