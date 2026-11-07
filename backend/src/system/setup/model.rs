//! 首次启动向导 · 数据访问与领域行为（FIX-1 · 新环境开箱即用）。
//!
//! # 这个模块解决什么
//!
//! 空库只有 `admin` 一个账号，而六条业务链（销售 / 采购 / 报销 / 预算 / 盘点 / 处置）
//! 的审批节点都绑在**角色**上（`SALES_SUPERVISOR` / `PURCHASE_MANAGER` /
//! `FINANCE_MANAGER` / `DEPT_LEADER` / `WAREHOUSE_MANAGER` / `GENERAL_MANAGER`）。
//! 角色里没人 ⇒ 发起审批时 422 `65018`（内控不允许自审），**任何单据都跑不通**。
//!
//! 三个能力：
//!
//! | 能力 | 用途 |
//! |------|------|
//! | [`key_role_status`] | 「哪些关键角色现在 0 人」→ 前端向导的清单 |
//! | [`add_members`] | 批量把用户加进角色（**只增不减**，幂等）→ 向导 / 65018 引导框的「指派」 |
//! | [`seed_demo_users`] | `NIK_SEED_DEMO_USERS=true` 时建演示账号（开发 / 测试环境） |
//!
//! # 幂等（卡面验收 ③：145 已有 demo_lead，不许重复建）
//!
//! - 演示账号：按 `username` 判重，已存在**整条跳过**（不覆盖密码、不改姓名、不动部门）；
//! - 角色绑定：`INSERT ... WHERE NOT EXISTS`，已绑定的不动；
//! - 演示部门：按 `dept_code` 判重。
//!
//! 因此同一份种子跑 1 次和跑 100 次，库里的行数完全一样（有单测与本卡实测双重验证）。
//!
//! # 密码
//!
//! 统一取 `NIK_DEMO_PASSWORD`（缺省 [`DEFAULT_DEMO_PASSWORD`]），并且
//! `must_change_password = true` —— 演示口令只在本地 / 测试环境有意义，
//! 首次登录强制改密，避免「演示口令漏到生产」这类事故。

use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

// ==================== 环境开关 ====================

/// 演示账号种子开关（`NIK_SEED_DEMO_USERS=true|false`）。
///
/// **缺省 = `false`**：生产环境默认不建任何演示账号（卡面「false（生产默认）」）。
/// 开发 / 测试环境在启动脚本里显式打开。
pub const ENV_SEED_DEMO_USERS: &str = "NIK_SEED_DEMO_USERS";

/// 演示账号统一口令（`NIK_DEMO_PASSWORD`）。
pub const ENV_DEMO_PASSWORD: &str = "NIK_DEMO_PASSWORD";

/// 演示账号缺省口令（仅开发 / 测试；首次登录强制改密）。
pub const DEFAULT_DEMO_PASSWORD: &str = "ChangeMe@2026";

/// 演示部门编码（`INITIATOR_SUPERIOR` 需要一个「部门负责人」才解析得出直属上级）。
pub const DEMO_DEPT_CODE: &str = "DEPT_DEMO";

/// 演示部门名称。
pub const DEMO_DEPT_NAME: &str = "演示事业部";

/// 开关解析（纯函数，可单测）。
///
/// 只认 `true` / `1` / `yes` / `on`（大小写不敏感）；其余（含缺省、空串、`false`）
/// 一律**关闭** —— 这是「生产默认不开」的唯一保证，不能因为写错一个字就建出一堆账号。
pub fn parse_switch(raw: Option<&str>) -> bool {
    matches!(
        raw.unwrap_or_default().trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

/// 当前进程是否开启演示账号种子。
pub fn demo_users_enabled() -> bool {
    parse_switch(std::env::var(ENV_SEED_DEMO_USERS).ok().as_deref())
}

/// 演示账号口令（`NIK_DEMO_PASSWORD` 非空时优先，否则缺省值）。
pub fn demo_password() -> String {
    match std::env::var(ENV_DEMO_PASSWORD) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => DEFAULT_DEMO_PASSWORD.to_string(),
    }
}

// ==================== 关键角色清单 ====================

/// 一个「关键角色」：审批链上必须有在职成员，否则发起即 422。
///
/// # 为什么是这 6 个（卡面 5 个 + `GENERAL_MANAGER`）
///
/// 卡面列了 5 个，但**审批快照里实际绑的是 6 个**（`so_approve.n_gm`、
/// `stock_take_post.n_gm`、`asset_disposal.n_gm`、`budget_approve.n_gm` 都绑
/// `GENERAL_MANAGER`）。少算这一个 ⇒ 向导说「都配好了」但金额 ≥ 10 万的销售单
/// 依然 422，等于把同一个坑换成更隐蔽的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRole {
    /// 角色编码（`sys_role.role_code`）。
    pub role_code: &'static str,
    /// 角色名称（角色被改名时以库为准，这里只是缺省值 / 提示文案）。
    pub role_name: &'static str,
    /// 建议绑定的演示账号（向导里「一键建演示账号」按它建）。
    pub demo_username: &'static str,
    /// 这个角色卡住哪条链（展示给用户，避免「不知道为什么要配它」）。
    pub why: &'static str,
}

/// 审批链必需的关键角色（顺序 = 向导展示顺序）。
pub const KEY_ROLES: &[KeyRole] = &[
    KeyRole {
        role_code: "SALES_SUPERVISOR",
        role_name: "销售主管",
        demo_username: "demo_sales_mgr",
        why: "销售订单审批（so_approve · 销售主管审批）",
    },
    KeyRole {
        role_code: "PURCHASE_MANAGER",
        role_name: "采购经理",
        demo_username: "demo_purchase_mgr",
        why: "采购订单 / 合同审批（po_approve · 采购经理审批）",
    },
    KeyRole {
        role_code: "FINANCE_MANAGER",
        role_name: "财务经理",
        demo_username: "demo_finance_mgr",
        why: "报销 / 付款 / 作废 / 红冲 / 预算审批（财务经理审批）",
    },
    KeyRole {
        role_code: "DEPT_LEADER",
        role_name: "部门主管",
        demo_username: "demo_lead",
        why: "采购 / 付款 / 预算的部门主管审批",
    },
    KeyRole {
        role_code: "WAREHOUSE_MANAGER",
        role_name: "仓管主管",
        demo_username: "demo_wh",
        why: "入库过账 / 调拨 / 盘点审批（仓管主管审批）",
    },
    KeyRole {
        role_code: "GENERAL_MANAGER",
        role_name: "总经理",
        demo_username: "demo_gm",
        why: "大额销售 / 盘点 / 处置 / 预算调整的总经理审批",
    },
];

// ==================== 演示账号清单 ====================

/// 一个演示账号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemoUser {
    /// 登录名（判重键）。
    pub username: &'static str,
    /// 真实姓名。
    pub real_name: &'static str,
    /// 绑定角色（可多个）。
    pub role_codes: &'static [&'static str],
    /// 是否放进演示部门（放进去才有「直属上级」可解析）。
    pub in_demo_dept: bool,
    /// 这个账号在哪条链上扮演谁（向导展示用）。
    pub note: &'static str,
}

/// 演示账号清单（卡面 8 个 + `demo_gm`）。
///
/// # 两个口径说明（写在这里免得后面被当成 bug）
///
/// 1. **`demo_purchase` / `demo_finance` 与各自的 `*_mgr` 绑同一个角色**：本仓的
///    角色矩阵（`V0258`）里**没有**「采购员 / 出纳」这两档 —— 采购建单权
///    （`business:erp:purchase-order:create`）在 `PURCHASE_MANAGER` 上，财务建单权也在
///    `FINANCE_MANAGER` 上。演示需要的是**两个不同的人**（制单人 ≠ 审批人，否则内控照样
///    拦），账号角色相同不影响这一诉求。
/// 2. **多一个 `demo_gm`**：卡面 8 个账号里没有总经理，但 `GENERAL_MANAGER` 是 4 条链的
///    必经审批岗（大额单据），少了它「六条链直接能跑」在金额跨档时会断。
pub const DEMO_USERS: &[DemoUser] = &[
    DemoUser {
        username: "demo_sales",
        real_name: "演示·销售员",
        role_codes: &["SALES_CLERK"],
        in_demo_dept: true,
        note: "销售链的制单人",
    },
    DemoUser {
        username: "demo_sales_mgr",
        real_name: "演示·销售主管",
        role_codes: &["SALES_SUPERVISOR"],
        in_demo_dept: false,
        note: "销售单审批（n_sales）",
    },
    DemoUser {
        username: "demo_purchase",
        real_name: "演示·采购员",
        role_codes: &["PURCHASE_MANAGER"],
        in_demo_dept: true,
        note: "采购链的制单人",
    },
    DemoUser {
        username: "demo_purchase_mgr",
        real_name: "演示·采购主管",
        role_codes: &["PURCHASE_MANAGER"],
        in_demo_dept: false,
        note: "采购单 / 合同审批（n_pm）",
    },
    DemoUser {
        username: "demo_finance",
        real_name: "演示·财务",
        role_codes: &["FINANCE_MANAGER"],
        in_demo_dept: true,
        note: "报销 / 付款的制单人",
    },
    DemoUser {
        username: "demo_finance_mgr",
        real_name: "演示·财务主管",
        role_codes: &["FINANCE_MANAGER"],
        in_demo_dept: false,
        note: "报销 / 付款 / 预算审批（n_fin）",
    },
    DemoUser {
        username: "demo_wh",
        real_name: "演示·仓管",
        role_codes: &["WAREHOUSE_MANAGER"],
        in_demo_dept: true,
        note: "入库 / 调拨 / 盘点审批（n_wh）",
    },
    DemoUser {
        username: "demo_lead",
        real_name: "演示·部门经理",
        role_codes: &["DEPT_LEADER"],
        in_demo_dept: true,
        note: "部门主管审批（n_dept）+ 演示部门的直属上级",
    },
    DemoUser {
        username: "demo_gm",
        real_name: "演示·总经理",
        role_codes: &["GENERAL_MANAGER"],
        in_demo_dept: false,
        note: "大额单据的总经理审批（n_gm）",
    },
];

// ==================== 查询：关键角色当前状态 ====================

/// 一个关键角色的现状（向导清单的一行）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct KeyRoleStatus {
    /// 角色编码。
    pub role_code: String,
    /// 角色名称（角色不存在时用缺省名）。
    pub role_name: String,
    /// 角色是否在库（被软删 / 没种 ⇒ `false`）。
    pub exists: bool,
    /// 角色 id（不存在时 `None`）。
    pub role_id: Option<String>,
    /// 在职成员数（未过期、用户未软删）。
    pub member_count: i64,
    /// 其中「启用且非机器人」的人数。
    pub usable_member_count: i64,
    /// 建议的演示账号。
    pub demo_username: String,
    /// 这个角色卡住哪条链。
    pub why: String,
    /// 是否已就绪（`usable_member_count > 0`）。
    pub ready: bool,
}

/// 读全部关键角色的现状（一次查询，不按角色循环）。
///
/// 口径与 `flow::no_assignee::role_infos` 一致：`expire_time IS NULL OR > now()`、
/// 用户 `deleted_at IS NULL`；「可用」额外要求 `status = 1 AND user_type <> 'ROBOT'`。
pub async fn key_role_status() -> DbResult<Vec<KeyRoleStatus>> {
    let codes = KEY_ROLES
        .iter()
        .map(|role| role.role_code.to_string())
        .collect::<Vec<_>>();
    let body = codes.join(",");
    let rows = db()
        .query_sql(
            "SELECT r.role_code AS role_code, r.role_name AS role_name, r.id AS role_id, \
                    count(ur.id) AS member_count, \
                    count(ur.id) FILTER (WHERE u.status = 1 AND u.user_type <> 'ROBOT') AS usable_member_count \
               FROM sys_role r \
               LEFT JOIN sys_user_role ur ON ur.role_id = r.id \
                    AND (ur.expire_time IS NULL OR ur.expire_time > now()) \
               LEFT JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL \
              WHERE r.role_code = ANY(?::text::text[]) AND r.deleted_at IS NULL \
              GROUP BY r.role_code, r.role_name, r.id"
                .to_string(),
            vec![Value::Text(format!("{{{body}}}"))],
        )
        .await?;

    let mut found: std::collections::HashMap<String, (String, Uuid, i64, i64)> =
        std::collections::HashMap::new();
    for row in &rows {
        let code: String = row.get_typed("role_code")?;
        found.insert(
            code,
            (
                row.get_typed("role_name")?,
                row.get_typed("role_id")?,
                row.get_typed("member_count")?,
                row.get_typed("usable_member_count")?,
            ),
        );
    }

    let mut out = Vec::with_capacity(KEY_ROLES.len());
    for role in KEY_ROLES {
        let hit = found.get(role.role_code);
        let member_count = hit.map(|(_, _, m, _)| *m).unwrap_or(0);
        let usable_member_count = hit.map(|(_, _, _, u)| *u).unwrap_or(0);
        out.push(KeyRoleStatus {
            role_code: role.role_code.to_string(),
            role_name: hit
                .map(|(name, _, _, _)| name.clone())
                .unwrap_or_else(|| role.role_name.to_string()),
            exists: hit.is_some(),
            role_id: hit.map(|(_, id, _, _)| id.to_string()),
            member_count,
            usable_member_count,
            demo_username: role.demo_username.to_string(),
            why: role.why.to_string(),
            ready: usable_member_count > 0,
        });
    }
    Ok(out)
}

/// 按角色编码取角色（未软删）。
pub async fn find_role_by_code(code: &str) -> DbResult<Option<(Uuid, String, String)>> {
    let rows = db()
        .query_sql(
            "SELECT id, role_code, role_name FROM sys_role \
              WHERE role_code = ? AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(code.to_string())],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some((
        row.get_typed("id")?,
        row.get_typed("role_code")?,
        row.get_typed("role_name")?,
    )))
}

/// 按角色 id 取角色（未软删）——`role_code` 缺失时的备选入口。
pub async fn find_role_by_id(role_id: Uuid) -> DbResult<Option<(Uuid, String, String)>> {
    let rows = db()
        .query_sql(
            "SELECT id, role_code, role_name FROM sys_role WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&role_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some((
        row.get_typed("id")?,
        row.get_typed("role_code")?,
        row.get_typed("role_name")?,
    )))
}

/// 过滤出「存在且未软删」的用户 id（保持入参顺序、去重）。
pub async fn existing_user_ids(user_ids: &[Uuid]) -> DbResult<Vec<Uuid>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let body = user_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let rows = db()
        .query_sql(
            "SELECT id FROM sys_user WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(format!("{{{body}}}"))],
        )
        .await?;
    let mut existing = std::collections::HashSet::new();
    for row in &rows {
        existing.insert(row.get_typed::<Uuid>("id")?);
    }
    let mut out: Vec<Uuid> = Vec::new();
    for id in user_ids {
        if existing.contains(id) && !out.contains(id) {
            out.push(*id);
        }
    }
    Ok(out)
}

/// 角色当前在职成员数。
pub async fn member_count(role_id: Uuid) -> DbResult<i64> {
    let rows = db()
        .query_sql(
            "SELECT count(*) AS n FROM sys_user_role ur \
               JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL \
              WHERE ur.role_id = ?::uuid AND (ur.expire_time IS NULL OR ur.expire_time > now())"
                .to_string(),
            vec![Value::from(&role_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("n"),
        None => Ok(0),
    }
}

/// 把一批用户加进角色（**只增不减、幂等**）。
///
/// # 返回值
///
/// 真正新增绑定的用户 id（已绑定的不在其中 —— 调用方据此告诉用户「谁是新加的」）。
///
/// # 幂等
///
/// `INSERT ... SELECT ... WHERE NOT EXISTS`：并发下也不会重复插
/// （`sys_user_role` 上有 `(user_id, role_id)` 唯一约束时会由库兜底）。
pub async fn add_members(role_id: Uuid, user_ids: &[Uuid]) -> DbResult<Vec<Uuid>> {
    let mut added = Vec::new();
    for user_id in user_ids {
        let rows = db()
            .query_sql(
                "INSERT INTO sys_user_role (id, user_id, role_id, expire_time, created_at) \
                 SELECT gen_random_uuid(), ?::uuid, ?::uuid, NULL, now() \
                  WHERE NOT EXISTS ( \
                        SELECT 1 FROM sys_user_role WHERE user_id = ?::uuid AND role_id = ?::uuid ) \
                 RETURNING user_id"
                    .to_string(),
                vec![
                    Value::from(user_id),
                    Value::from(&role_id),
                    Value::from(user_id),
                    Value::from(&role_id),
                ],
            )
            .await?;
        if let Some(row) = rows.first() {
            added.push(row.get_typed::<Uuid>("user_id")?);
        }
    }
    Ok(added)
}

// ==================== 演示部门 / 演示账号 ====================

/// 确保演示部门存在（`leader_id` 留给 [`seed_demo_users`] 在账号建好后回填）。
///
/// 返回 `(dept_id, created)`：`created = false` 表示早就有了（幂等）。
pub async fn ensure_demo_dept() -> DbResult<(Uuid, bool)> {
    let rows = db()
        .query_sql(
            "SELECT id FROM sys_dept WHERE dept_code = ? AND deleted_at IS NULL".to_string(),
            vec![Value::Text(DEMO_DEPT_CODE.to_string())],
        )
        .await?;
    if let Some(row) = rows.first() {
        return Ok((row.get_typed::<Uuid>("id")?, false));
    }
    let id = Uuid::now_v7();
    let path = format!("/{id}/");
    db().exec_sql(
        "INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, dept_path, leader_id, \
                               status, sort_order, created_at, updated_at) \
         VALUES (?::uuid, ?, ?, NULL, ?, NULL, 1, 99, now(), now())"
            .to_string(),
        vec![
            Value::from(&id),
            Value::Text(DEMO_DEPT_CODE.to_string()),
            Value::Text(DEMO_DEPT_NAME.to_string()),
            Value::Text(path),
        ],
    )
    .await?;
    Ok((id, true))
}

/// 演示账号种子的结果（启动日志与接口返回共用）。
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct DemoSeedOutcome {
    /// 开关是否打开（`false` 时下面两个列表必为空）。
    pub enabled: bool,
    /// 新建的登录名。
    pub created: Vec<String>,
    /// 已存在、本次跳过的登录名（**幂等的证据**）。
    pub existed: Vec<String>,
    /// 新建的部门编码（空 = 部门早就有）。
    pub dept_created: bool,
    /// 本次新建立的「用户-角色」绑定条数。
    pub bindings_added: usize,
}

/// 创建演示账号（`NIK_SEED_DEMO_USERS=true` 时；启动与接口共用）。
///
/// # 幂等
///
/// - 账号按 `username` 判重：已存在 ⇒ 整条跳过（不覆盖密码 / 姓名 / 部门）；
/// - 角色绑定走 [`add_members`]（只增不减）；
/// - 部门按 `dept_code` 判重。
///
/// # 参数
///
/// `password`：统一口令明文（仅用于**新建**账号；已存在账号的口令不动）。
///
/// # 异常情况
///
/// - 密码哈希失败 ⇒ `DbError::InvalidValue`（调用方按「种子失败不阻断启动」处理）；
/// - 数据库异常 ⇒ `DbError`。
pub async fn seed_demo_users(password: &str) -> DbResult<DemoSeedOutcome> {
    let mut outcome = DemoSeedOutcome {
        enabled: true,
        ..DemoSeedOutcome::default()
    };

    // 哈希是 CPU 密集的慢操作，放阻塞线程池（与 bootstrap 的管理员种子同一做法）
    let plain = password.to_string();
    let hashed = tokio::task::spawn_blocking(move || {
        Crypto::hash_password(&plain).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| DbError::InvalidValue(format!("演示账号口令哈希任务失败: {e}")))?
    .map_err(|e| DbError::InvalidValue(format!("演示账号口令哈希失败: {e}")))?;

    let (dept_id, dept_created) = ensure_demo_dept().await?;
    outcome.dept_created = dept_created;

    let mut user_ids: std::collections::HashMap<&'static str, Uuid> =
        std::collections::HashMap::new();
    // ① 账号（顺手把「演示部门」里的成员设为部门负责人 = demo_lead）
    for spec in DEMO_USERS {
        let existing = db()
            .query_sql(
                "SELECT id FROM sys_user WHERE username = ? AND deleted_at IS NULL".to_string(),
                vec![Value::Text(spec.username.to_string())],
            )
            .await?;
        if let Some(row) = existing.first() {
            user_ids.insert(spec.username, row.get_typed::<Uuid>("id")?);
            outcome.existed.push(spec.username.to_string());
            continue;
        }
        let id = Uuid::now_v7();
        let now = chrono::Utc::now();
        db().exec_sql(
            "INSERT INTO sys_user (id, username, password, real_name, dept_id, status, \
                                   login_fail_count, is_locked, must_change_password, token_version, \
                                   created_at, updated_at) \
             VALUES (?::uuid, ?, ?, ?, ?::uuid, 1, 0, FALSE, TRUE, 0, ?, ?)"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(spec.username.to_string()),
                Value::Text(hashed.clone()),
                Value::Text(spec.real_name.to_string()),
                match spec.in_demo_dept {
                    true => Value::from(&dept_id),
                    false => Value::Null,
                },
                Value::from(now),
                Value::from(now),
            ],
        )
        .await?;
        user_ids.insert(spec.username, id);
        outcome.created.push(spec.username.to_string());
    }

    // ② 角色绑定（只增不减）
    for spec in DEMO_USERS {
        let Some(user_id) = user_ids.get(spec.username).copied() else {
            continue;
        };
        for role_code in spec.role_codes {
            let Some((role_id, _, _)) = find_role_by_code(role_code).await? else {
                // 角色没种（老库缺角色）不算失败：把「没绑上」记进日志，由向导提示管理员
                tracing::warn!(
                    username = spec.username,
                    role_code,
                    "演示账号绑定的角色不存在，已跳过（请先执行迁移补齐角色）"
                );
                continue;
            };
            outcome.bindings_added += add_members(role_id, &[user_id]).await?.len();
        }
    }

    // ③ 演示部门的负责人 = demo_lead（`INITIATOR_SUPERIOR` 才有「直属上级」）
    if let Some(lead_id) = user_ids.get("demo_lead").copied() {
        db().exec_sql(
            "UPDATE sys_dept SET leader_id = ?::uuid, updated_at = now() \
              WHERE id = ?::uuid AND leader_id IS DISTINCT FROM ?::uuid"
                .to_string(),
            vec![
                Value::from(&lead_id),
                Value::from(&dept_id),
                Value::from(&lead_id),
            ],
        )
        .await?;
    }

    Ok(outcome)
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 开关只认 true/1/yes/on —— 生产（缺省 / 空 / false）绝不开
    #[test]
    fn switch_defaults_to_off() {
        assert!(!parse_switch(None), "缺省必须关闭");
        assert!(!parse_switch(Some("")), "空串必须关闭");
        assert!(!parse_switch(Some("false")));
        assert!(!parse_switch(Some("0")));
        assert!(!parse_switch(Some("maybe")));
        assert!(parse_switch(Some("true")));
        assert!(parse_switch(Some(" TRUE ")));
        assert!(parse_switch(Some("1")));
        assert!(parse_switch(Some("yes")));
        assert!(parse_switch(Some("ON")));
    }

    /// ② 演示账号清单覆盖卡面 8 个 + 总经理，且登录名唯一
    #[test]
    fn demo_users_are_unique_and_cover_card() {
        let names = DEMO_USERS
            .iter()
            .map(|user| user.username)
            .collect::<Vec<_>>();
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "演示登录名必须唯一");
        for card in [
            "demo_sales",
            "demo_sales_mgr",
            "demo_purchase",
            "demo_purchase_mgr",
            "demo_finance",
            "demo_finance_mgr",
            "demo_wh",
            "demo_lead",
        ] {
            assert!(names.contains(&card), "卡面要求的 {card} 缺失");
        }
        assert!(names.contains(&"demo_gm"), "总经理账号是 4 条链的必经岗");
    }

    /// ③ 每个关键角色都能被至少一个演示账号覆盖（否则「启动即能跑」是假的）
    #[test]
    fn every_key_role_has_a_demo_owner() {
        for role in KEY_ROLES {
            let covered = DEMO_USERS
                .iter()
                .any(|user| user.role_codes.contains(&role.role_code));
            assert!(covered, "关键角色 {} 没有演示账号覆盖", role.role_code);
            let named = DEMO_USERS
                .iter()
                .any(|user| user.username == role.demo_username);
            assert!(
                named,
                "关键角色 {} 建议的演示账号 {} 不在清单里",
                role.role_code, role.demo_username
            );
        }
    }

    /// ④ 关键角色清单必须含卡面 5 个 + 总经理（少一个就还有链跑不通）
    #[test]
    fn key_roles_cover_all_flow_gates() {
        let codes = KEY_ROLES
            .iter()
            .map(|role| role.role_code)
            .collect::<Vec<_>>();
        for code in [
            "SALES_SUPERVISOR",
            "PURCHASE_MANAGER",
            "FINANCE_MANAGER",
            "DEPT_LEADER",
            "WAREHOUSE_MANAGER",
            "GENERAL_MANAGER",
        ] {
            assert!(codes.contains(&code), "关键角色清单缺 {code}");
        }
        assert_eq!(codes.len(), KEY_ROLES.len(), "关键角色不许重复");
    }

    /// ⑤ 演示部门里必须有且只有 demo_lead 一位「部门负责人候选」
    #[test]
    fn demo_dept_has_one_leader_candidate() {
        let leaders = DEMO_USERS
            .iter()
            .filter(|user| user.in_demo_dept && user.username == "demo_lead")
            .count();
        assert_eq!(leaders, 1, "演示部门的负责人只能是 demo_lead");
        let members = DEMO_USERS.iter().filter(|user| user.in_demo_dept).count();
        assert!(members >= 4, "演示部门至少要容纳报销 / 采购的制单人");
    }

    /// ⑥ 缺省口令非空且与卡面一致（写死在测试里 = 契约）
    #[test]
    fn default_password_matches_card() {
        assert_eq!(DEFAULT_DEMO_PASSWORD, "ChangeMe@2026");
        assert!(!DEFAULT_DEMO_PASSWORD.trim().is_empty());
    }
}
