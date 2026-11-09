//! 真库用例的**前置体检**（`#[cfg(test)]`）—— 卡 TST-02。
//!
//! # 为什么需要它
//!
//! 「依赖种子数据」的真库用例（`asset_linkage_tests` / `expense_linkage_tests` /
//! `voucher_linkage_tests` …）在**只跑过迁移、没跑 bootstrap 演示组织**的库上会红，
//! 而且报错点离真因很远：
//!
//! ```text
//! thread '…::st02_idle_triggers_once' panicked at asset_linkage_tests.rs:195:
//! 库里没有部门（先跑基础种子迁移）
//! ```
//!
//! 这句话既没说「本套件需要什么」，也没说「怎么补」，还容易被当成代码 bug 去查接线。
//! 更糟的是 `voucher_linkage_tests::two_users()` 在只有 1 个账号的库上 `return None`
//! ⇒ 用例**静默 pass**（假绿），而它其实一行断言都没跑。
//!
//! 所以：[`require_seed_data`] 在用例**建夹具之前**按清单体检，缺什么就把「缺什么 /
//! 为什么需要 / 怎么补」一次性打在 panic 里；静默 pass 也一并变成响亮的失败。
//!
//! # 用法
//!
//! ```ignore
//! use crate::shared::test_helpers as guard;
//!
//! async fn setup() -> Option<()> {
//!     if !ensure_db().await { return None; }        // 没有 ERP_DB_URL → SKIP（原口径不变）
//!     guard::require_seed_data("asset_linkage_tests",
//!         &[guard::DEPT, guard::ASSET_CATEGORY]).await;   // 空库在这里就说清楚
//!     …
//! }
//! ```
//!
//! # 清单（[`SeedRequirement`]）与判据
//!
//! 每条 =「一句人话 + 一条 `SELECT count(*)` + 最少行数 + 缺了会怎样」。
//! 判据一律是 `count >= min`；查询本身报错（表/列不存在）也算缺 —— 那说明库的迁移
//! 不是当前版本，同样要人去重建，而不是让用例跑到一半炸在莫名其妙的断言上。
//!
//! # 修复路径（不要用 `--ignored` / `--skip` 绕过）
//!
//! ```bash
//! bash scripts/verify/make_full_suite_db.sh     # 建库 → 全部迁移 → 演示组织/账号/角色种子
//! # 脚本最后打印四条 URL（必须指同一个库）
//! export ERP_DB_URL='postgres://…/cloudpivot_test_full'
//! cd backend && cargo test --lib -- --test-threads=1
//! ```

use nik::prelude::*;
use nik::uuid::Uuid;

/// 一条种子前置条件。
#[derive(Debug, Clone, Copy)]
pub struct SeedRequirement {
    /// 人话名（打在 panic 里，带表名）。
    pub label: &'static str,
    /// `SELECT count(*) …`：返回「当前库里有几行可用的」。
    pub count_sql: &'static str,
    /// 最少要几行（`>=` 才算满足）。
    pub min: i64,
    /// 缺了会怎样（为什么这个套件离不开它）。
    pub hint: &'static str,
}

/// 启用中的部门（资产卡 / 报销单 / 预算都要挂部门）。
pub const DEPT: SeedRequirement = SeedRequirement {
    label: "部门（sys_dept）",
    count_sql: "SELECT count(*) FROM sys_dept WHERE deleted_at IS NULL",
    min: 1,
    hint: "资产卡、报销单、预算占用都带 dept_id / dept_name；空库没有部门就建不了单",
};

/// 在职且**挂部门**的用户（报销人 / 制单人必须是真实用户，部门是预算维度）。
pub const DEPT_USER: SeedRequirement = SeedRequirement {
    label: "在职且挂部门的用户（sys_user.dept_id IS NOT NULL）",
    count_sql: "SELECT count(*) FROM sys_user \
                 WHERE deleted_at IS NULL AND dept_id IS NOT NULL",
    min: 1,
    hint: "报销单的部门取自报销人；只有 admin 且 admin 未挂部门时，建单入口直接没有维度",
};

/// 至少两个在职用户（内控：制单人 ≠ 审核人）。
pub const TWO_USERS: SeedRequirement = SeedRequirement {
    label: "在职用户（sys_user，至少 2 名）",
    count_sql: "SELECT count(*) FROM sys_user WHERE deleted_at IS NULL",
    min: 2,
    hint: "凭证用例要「制单人 ≠ 审核人」（制单不能审自己的凭证）；只有 1 个账号时\
           旧代码会静默 return，用例变成假绿",
};

/// 科目表（凭证分录 / 期间结转都要科目）。
pub const ACCOUNT: SeedRequirement = SeedRequirement {
    label: "科目（biz_account）",
    count_sql: "SELECT count(*) FROM biz_account WHERE deleted_at IS NULL",
    min: 1,
    hint: "任何凭证都要科目；用例还会把 1405 / 2202 置为启用",
};

/// 启用中的费用类型（V0242 种子；报销明细的分类与科目来源）。
pub const EXPENSE_TYPE: SeedRequirement = SeedRequirement {
    label: "费用类型（biz_expense_type）",
    count_sql: "SELECT count(*) FROM biz_expense_type WHERE deleted_at IS NULL",
    min: 1,
    hint: "报销明细要带 expense_type_id + 对应科目（V0242 种子），空表连一笔都建不出来",
};

/// 启用中的资产分类（V0229 种子；类别决定残值率 / 折旧月数）。
pub const ASSET_CATEGORY: SeedRequirement = SeedRequirement {
    label: "资产分类（biz_asset_category，启用中）",
    count_sql: "SELECT count(*) FROM biz_asset_category \
                 WHERE deleted_at IS NULL AND status = 1",
    min: 1,
    hint: "资产卡必须挂分类（残值率 / 折旧月数来自分类，V0229 种子）",
};

/// 已发布的审批流定义（发起任何审批都要先有流程）。
pub const PUBLISHED_FLOW: SeedRequirement = SeedRequirement {
    label: "已发布审批流（biz_flow_def.status = 'PUBLISHED'）",
    count_sql: "SELECT count(*) FROM biz_flow_def \
                 WHERE deleted_at IS NULL AND status = 'PUBLISHED'",
    min: 1,
    hint: "单据提交后要派审批人；没有已发布流程时提交直接失败",
};

/// 六个关键审批岗位都有在职成员（演示组织种子 `NIK_SEED_DEMO_USERS=true`）。
///
/// 只要求「每个岗位至少 1 人」：角色没绑人 ⇒ 发起审批 `422 / 65018`
/// 「流程节点没有有效审批人」，而那个报错看起来像代码 bug。
pub const APPROVER_ROLES: SeedRequirement = SeedRequirement {
    label: "关键审批岗位的在职成员（SALES_SUPERVISOR / PURCHASE_MANAGER / \
            FINANCE_MANAGER / DEPT_LEADER / WAREHOUSE_MANAGER / GENERAL_MANAGER 六个岗位）",
    count_sql: "SELECT count(DISTINCT r.role_code) FROM sys_role r \
                  JOIN sys_user_role ur ON ur.role_id = r.id \
                  JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL \
                 WHERE r.deleted_at IS NULL \
                   AND r.role_code IN ('SALES_SUPERVISOR', 'PURCHASE_MANAGER', \
                                       'FINANCE_MANAGER', 'DEPT_LEADER', \
                                       'WAREHOUSE_MANAGER', 'GENERAL_MANAGER')",
    min: 6,
    hint: "审批节点按角色派单；某个岗位没人 ⇒ 提交 422 / 65018「没有有效审批人」\
           （演示组织种子 `NIK_SEED_DEMO_USERS=true` 会种这 7 个演示账号）",
};

/// 最小集合：部门 + 挂部门的用户。
pub const CORE: &[SeedRequirement] = &[DEPT, DEPT_USER];

/// 体检：`needs` 里每条都满足才放行，否则**一次 panic 报全部缺口**。
///
/// `suite` 只用于报错文案（写用例模块名，例：`"asset_linkage_tests"`）。
pub async fn require_seed_data(suite: &str, needs: &[SeedRequirement]) {
    let db_name = match db().scalar::<String>("SELECT current_database()").await {
        Ok(name) => name,
        Err(err) => format!("<查 current_database() 失败：{err}>"),
    };

    let mut missing: Vec<String> = Vec::new();
    for need in needs {
        match db().scalar_with_params::<i64>(need.count_sql, vec![]).await {
            Ok(n) if n >= need.min => {}
            Ok(n) => missing.push(format!(
                "  · {}：库里只有 {n} 行，至少需要 {} 行\n      └ {}",
                need.label, need.min, need.hint
            )),
            Err(err) => missing.push(format!(
                "  · {}：查询失败（{err}）\n      └ 表/列不存在 ⇒ 这个库的迁移不是当前版本，\
                 别在这个库上跑；{}",
                need.label, need.hint
            )),
        }
    }
    if missing.is_empty() {
        return;
    }

    panic!(
        "\n\
         ══════════════════════════════════════════════════════════════════════\n\
         【需要种子数据】{suite} 在库「{db_name}」上缺少 {} 项前置数据：\n\
         {}\n\
         \n\
         这不是代码 bug：本套件测的是业务接线，库里先得有「组织 / 账号 / 科目 / 分类」这些种子。\n\
         只跑过 migrations 的空库天然没有（bootstrap 的演示组织要 `NIK_SEED_DEMO_USERS=true` 才会种）。\n\
         \n\
         修复（一条命令造一个可复现的种子库）：\n\
             bash scripts/verify/make_full_suite_db.sh\n\
         它 = 建空库 → 跑全部迁移 → 开 `NIK_SEED_DEMO_USERS=true` 种下演示组织/9 个演示账号/\n\
         关键角色绑定 → 自检，最后打印四条 URL（ERP_DB_URL / SYS_DB_URL / TASK_DB_URL /\n\
         P0FIX_DB_URL，**必须指同一个库**）：\n\
             export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test_full'\n\
             cd backend && cargo test --lib -- --test-threads=1\n\
         \n\
         已有的种子库也可以直接用（内网 `cloudpivot_test` 一类）。\n\
         不要用 `--skip` / `--ignored` 绕过：那样这 {} 条用例一行断言都不会跑，等于假绿。\n\
         ══════════════════════════════════════════════════════════════════════",
        missing.len(),
        missing.join("\n"),
        needs.len()
    );
}

// ============================================================================
// A 方案（TST-02b）：用例**自带夹具** —— 空库也能全绿，不再依赖种子
// ============================================================================
//
// B 方案（[`require_seed_data`]）解决的是「红得看不懂」：空库上先点名缺什么。
// 但它仍然要求库里有种子。本段是 A 方案：**用例自己把缺的那两行造出来**。
//
// 为什么可以自己造（而不是复制别人的演示组织）：
//   · `sys_dept` / `sys_user` / `sys_user_role` 是**弱引用**为主的底座表
//     （`sys_user.dept_id` 不建外键），造两行不影响任何生产口径；
//   · 科目 / 费用类型 / 资产分类 / 审批流定义**都是迁移种下的**（V0226 / V0242 /
//     V0229 / 流程种子），空库上本来就有 ⇒ 真的只缺「部门 + 用户 + 岗位成员」；
//   · 真库套件要的部门/用户只是「一个可挂靠的维度」，具体是谁无关紧要。
//
// 三条铁律（和卡面约定一致）：
//   1. **固定 UUID**：[`fixture_uuid`] 由键（`dept:TST-DEPT`）SHA-256 派生，可复现、
//      不随机；重复跑落到同一行，`ON CONFLICT DO NOTHING` 保证幂等。
//   2. **只碰自己的行**：删除一律按**自己的 code / username 精确匹配**，
//      绝不按前缀扫全表删别人的夹具。
//   3. **缺才补**：[`ensure_role_member`] 只在某岗位**一个在职成员都没有**时才补人 ——
//      种子库上演示账号已经占着岗位，此时一行都不加，行为与 B 方案下的种子库完全一致。

/// 夹具部门编码（资产 / 预算 / 报销 / 凭证共用）。
pub const FIXTURE_DEPT_CODE: &str = "TST-DEPT";
/// 夹具部门名（会落进 `dept_name` 快照列，取个一眼能认出来的名字）。
pub const FIXTURE_DEPT_NAME: &str = "TST 夹具部门";
/// 第二个夹具部门（跨部门用例：预算部门覆盖、报销跨部门校验）。
pub const FIXTURE_DEPT2_CODE: &str = "TST-DEPT-2";
/// 第二个夹具部门名。
pub const FIXTURE_DEPT2_NAME: &str = "TST 夹具部门二";

/// 由夹具键**确定性**派生 UUID（SHA-256 前 16 字节 + 版本/variant 位），
/// 不是 `Uuid::now_v7()`。
///
/// 为什么不引 `uuid` 的 v5 feature：那要改 `nik` 的 feature 点单（框架铁律），
/// 而 `sha2` 本来就是本 crate 的直接依赖（文件秒传用它）⇒ 零新增依赖。
pub fn fixture_uuid(key: &str) -> Uuid {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80; // version 8 = 自定义（确定性派生）
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    Uuid::from_bytes(bytes)
}

/// 执行一条写 SQL；失败直接 panic（夹具写不进去就是夹具坏了，不许静默）。
async fn fixture_exec(sql: &str, params: Vec<Value>) {
    if let Err(err) = db().execute_with_params(sql, params).await {
        panic!("夹具 SQL 失败：{err}\nSQL: {sql}");
    }
}

/// 查一个单值 uuid（查不到就 panic，报清楚是哪个夹具键）。
async fn fixture_uuid_of(sql: &str, params: Vec<Value>, what: &str) -> Uuid {
    db().scalar_with_params::<Uuid>(sql, params)
        .await
        .unwrap_or_else(|err| panic!("夹具回读失败（{what}）：{err}\nSQL: {sql}"))
}

/// 造（或取回）一个夹具部门：`id` 由 [`fixture_uuid`] 定死，`code` 唯一，幂等。
///
/// 软删过的同 code 行会先被复活：`uk_sys_dept_code` 是**部分索引**（只覆盖
/// `deleted_at IS NULL`），历史软删行既不挡插入也会让固定 id 撞主键，两边都不对。
pub async fn ensure_dept(code: &str, name: &str) -> Uuid {
    fixture_exec(
        "UPDATE sys_dept SET deleted_at = NULL, status = 1 WHERE dept_code = ?",
        vec![Value::Text(code.to_string())],
    )
    .await;
    fixture_exec(
        "INSERT INTO sys_dept (id, dept_code, dept_name, status, sort_order) \
         VALUES (?, ?, ?, 1, 900) ON CONFLICT DO NOTHING",
        vec![
            Value::from(&fixture_uuid(&format!("dept:{code}"))),
            Value::Text(code.to_string()),
            Value::Text(name.to_string()),
        ],
    )
    .await;
    fixture_uuid_of(
        "SELECT id FROM sys_dept WHERE dept_code = ? AND deleted_at IS NULL",
        vec![Value::Text(code.to_string())],
        &format!("部门 {code}"),
    )
    .await
}

/// [`FIXTURE_DEPT_CODE`] 对应的部门 id（最常用的那个）。
pub async fn ensure_fixture_dept() -> Uuid {
    ensure_dept(FIXTURE_DEPT_CODE, FIXTURE_DEPT_NAME).await
}

/// 「随便一个可用部门」：**库里有就用现成的**（种子库零新增，断言基线不漂移），
/// 一个都没有才造 [`FIXTURE_DEPT_CODE`]。
///
/// 这是 A 方案对「取库里第一个部门」这类辅助函数的直接替代：语义不变，只是把
/// `Option` + 调用方 `else { return; }`（静默 pass）换成「没有就自己造」。
pub async fn ensure_any_dept() -> Uuid {
    let rows = db()
        .query_sql(
            "SELECT id FROM sys_dept WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1".to_string(),
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("查部门失败：{err}"));
    if let Some(id) = rows.first().and_then(|row| row.get_typed::<Uuid>("id").ok()) {
        return id;
    }
    ensure_fixture_dept().await
}

/// 「随便一个在职用户」（不要求挂部门）：库里有就用，没有才造 `tst02b.user`。
///
/// 需要「挂部门的报销人」时用 [`ensure_dept_user`]（报销单的部门取自报销人）。
pub async fn ensure_any_user() -> Uuid {
    let rows = db()
        .query_sql(
            "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1".to_string(),
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("查在职用户失败：{err}"));
    if let Some(id) = rows.first().and_then(|row| row.get_typed::<Uuid>("id").ok()) {
        return id;
    }
    ensure_user("tst02b.user", "TST 夹具账号", None).await
}

/// 造（或取回）一个夹具用户：`username` 唯一，`dept` 可空。
///
/// `password` 写一个**不是哈希的固定串**：夹具账号不登录，写什么都能挡住误登录；
/// 不给它 `SUPER_ADMIN`（否则权限短路会让越权用例假绿）。
pub async fn ensure_user(username: &str, real_name: &str, dept: Option<Uuid>) -> Uuid {
    fixture_exec(
        "UPDATE sys_user SET deleted_at = NULL, status = 1 WHERE username = ?",
        vec![Value::Text(username.to_string())],
    )
    .await;
    let dept_values = match dept {
        Some(id) => format!("'{id}'::uuid"),
        None => "NULL".to_string(),
    };
    // 参数一律绑原生类型（不把 uuid 转成 text 再 `::uuid` 强转：driver 会把
    // `$n::uuid` 的参数类型推成 uuid，绑 Text 会报 `error serializing parameter`）。
    fixture_exec(
        &format!(
            "INSERT INTO sys_user (id, username, password, real_name, dept_id, status) \
             VALUES (?, ?, 'tst02b-fixture-not-a-real-hash', ?, {dept_values}, 1) \
             ON CONFLICT DO NOTHING"
        ),
        vec![
            Value::from(&fixture_uuid(&format!("user:{username}"))),
            Value::Text(username.to_string()),
            Value::Text(real_name.to_string()),
        ],
    )
    .await;
    // dept 是幂等参数：库里已有这行时也要把部门对齐（上一轮可能没挂部门）
    if let Some(id) = dept {
        fixture_exec(
            "UPDATE sys_user SET dept_id = ? WHERE username = ?",
            vec![Value::from(&id), Value::Text(username.to_string())],
        )
        .await;
    }
    fixture_uuid_of(
        "SELECT id FROM sys_user WHERE username = ? AND deleted_at IS NULL",
        vec![Value::Text(username.to_string())],
        &format!("用户 {username}"),
    )
    .await
}

/// 「挂夹具部门的夹具用户」——报销 / 凭证用例的最小夹具。
pub async fn ensure_fixture_user(username: &str, real_name: &str) -> (Uuid, Uuid) {
    let dept = ensure_fixture_dept().await;
    let user = ensure_user(username, real_name, Some(dept)).await;
    (user, dept)
}

/// 「一个挂部门、在职的报销人」——**库里有就用现成的，一行都不加**；没有才造夹具。
///
/// 为什么先查现成的：种子库上演示账号占着部门，新增夹具用户会让
/// 「列表里恰好 N 条」「收件人是某个人」一类断言基线漂移；空库里没有，才补一行。
///
/// 返回 `(用户 id, 部门 id, 显示名)`。
pub async fn ensure_dept_user() -> (Uuid, Uuid, String) {
    let rows = db()
        .query_sql(
            "SELECT u.id, u.dept_id, coalesce(nullif(u.real_name, ''), u.username) AS name \
               FROM sys_user u \
               JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL \
              WHERE u.deleted_at IS NULL AND u.dept_id IS NOT NULL \
              ORDER BY u.created_at LIMIT 1"
                .to_string(),
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("查「挂部门的在职用户」失败：{err}"));
    if let Some(row) = rows.first()
        && let (Ok(user), Ok(dept), Ok(name)) = (
            row.get_typed::<Uuid>("id"),
            row.get_typed::<Uuid>("dept_id"),
            row.get_typed::<String>("name"),
        )
    {
        return (user, dept, name);
    }
    let (user, dept) = ensure_fixture_user("tst02b.reporter", "TST 夹具报销人").await;
    (user, dept, "TST 夹具报销人".to_string())
}

/// 「两个在职账号」（内控：制单人 ≠ 审核人）——**不够才补**。
///
/// 库里有 1 个就只造 1 个夹具补位，一个都没有就造 2 个；已有 2 个以上原样返回
/// （按 `created_at` 取最早的两个，结果稳定）。
pub async fn ensure_two_users() -> (Uuid, Uuid) {
    let rows = db()
        .query_sql(
            "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at LIMIT 2".to_string(),
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("查在职用户失败：{err}"));
    let mut ids: Vec<Uuid> = rows.iter().filter_map(|row| row.get_typed::<Uuid>("id").ok()).collect();
    let mut seq = 0usize;
    while ids.len() < 2 {
        seq += 1;
        let username = format!("tst02b.maker{seq}");
        let (user, _dept) = ensure_fixture_user(&username, &format!("TST 夹具账号{seq}")).await;
        if !ids.contains(&user) {
            ids.push(user);
        }
    }
    (ids[0], ids[1])
}

/// 给夹具用户绑一个**已由迁移种下**的角色（按 `role_code`；角色不存在就 panic）。
pub async fn bind_role(user: Uuid, role_code: &str) {
    let role = fixture_uuid_of(
        "SELECT id FROM sys_role WHERE role_code = ? AND deleted_at IS NULL",
        vec![Value::Text(role_code.to_string())],
        &format!("角色 {role_code}"),
    )
    .await;
    fixture_exec(
        "INSERT INTO sys_user_role (id, user_id, role_id) VALUES (?, ?, ?) \
         ON CONFLICT (user_id, role_id) DO NOTHING",
        vec![
            Value::from(&fixture_uuid(&format!("user_role:{}:{role_code}", user.as_u128()))),
            Value::from(&user),
            Value::from(&role),
        ],
    )
    .await;
}

/// **缺才补**：某岗位一个在职成员都没有时，造一个夹具成员绑上去；已经有人的岗位一行都不加。
///
/// 为什么要判「有没有」：种子库里六个岗位由演示账号占着，这时候再加人会让
/// 「每跳恰好 1 条待办」一类断言的基线漂移；空库里岗位没人，审批提交会
/// `422 / 65018 本节点没有有效审批人`（看起来像代码 bug）。判据只有一条：
/// **有就不动，没有才补**。
pub async fn ensure_role_member(role_code: &str) -> Option<Uuid> {
    let has = db()
        .scalar_with_params::<i64>(
            "SELECT count(*) FROM sys_user_role ur \
               JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL \
               JOIN sys_role r ON r.id = ur.role_id AND r.deleted_at IS NULL \
              WHERE r.role_code = ?",
            vec![Value::Text(role_code.to_string())],
        )
        .await
        .unwrap_or_else(|err| panic!("查角色 {role_code} 的在职成员失败：{err}"));
    if has > 0 {
        return None;
    }
    let username = format!("tst02b.{}", role_code.to_ascii_lowercase());
    let (user, _dept) = ensure_fixture_user(&username, &format!("TST 夹具 {role_code}")).await;
    bind_role(user, role_code).await;
    Some(user)
}

/// 六个审批岗位都补上（空库上把「没有有效审批人」这一类红一次性干掉）。
///
/// 返回**本次新建**的成员数（打印用；种子库上恒为 0）。
pub async fn ensure_approver_roles() -> usize {
    const ROLES: [&str; 6] = [
        "SALES_SUPERVISOR",
        "PURCHASE_MANAGER",
        "FINANCE_MANAGER",
        "DEPT_LEADER",
        "WAREHOUSE_MANAGER",
        "GENERAL_MANAGER",
    ];
    let mut created = 0;
    for role in ROLES {
        if ensure_role_member(role).await.is_some() {
            created += 1;
        }
    }
    created
}

/// 删掉**自己这个**夹具用户（按 username 精确匹配）。
///
/// 只用于 setup 前置清场：`biz_flow_instance.initiator_id` 之类是硬外键，
/// 上一轮中途 panic 留下的实例还挂在人身上时删除会失败 —— 那是调用方的
/// 清场顺序问题（先删实例再删人），这里的失败**不吞也不 panic**：
/// 夹具是固定 id 的，留着不影响下一轮。
pub async fn purge_user(username: &str) {
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_user_role WHERE user_id IN \
               (SELECT id FROM sys_user WHERE username = ?)",
            vec![Value::Text(username.to_string())],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_user WHERE username = ?",
            vec![Value::Text(username.to_string())],
        )
        .await;
}

/// 删掉**自己这个**夹具部门（按 dept_code 精确匹配）。
///
/// 同上：被资产卡 `use_dept_id` 等硬外键引用时删除会失败，这里不 panic ——
/// 夹具固定 id，留着下一轮照样复用；但**绝不做前缀扫描**，别人的部门一行不碰。
pub async fn purge_dept(code: &str) {
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_dept WHERE dept_code = ?",
            vec![Value::Text(code.to_string())],
        )
        .await;
}

/// 按「套件自己造的号段」清场：既清夹具用户，也清夹具部门。
pub async fn purge_fixture(username: &str, dept_code: &str) {
    purge_user(username).await;
    purge_dept(dept_code).await;
}

#[cfg(test)]
mod fixture_tests {
    use super::*;

    /// 固定 UUID 必须**确定性**：同键两次调用相等、不同键不相等、版本/variant 位对。
    #[test]
    fn fixture_uuid_is_deterministic() {
        let a = fixture_uuid("dept:TST-DEPT");
        let b = fixture_uuid("dept:TST-DEPT");
        let c = fixture_uuid("dept:TST-DEPT-2");
        assert_eq!(a, b, "同一个夹具键必须派生出同一个 UUID（可复现）");
        assert_ne!(a, c, "不同夹具键必须落在不同的行上");
        assert_eq!(a.get_version_num(), 8, "派生 UUID 的版本位固定为 8");
        assert_eq!(
            a.to_string().as_bytes()[14],
            b'8',
            "UUID 字符串里第 15 位是版本号（人眼可校验）"
        );
        assert_eq!(a.get_variant(), uuid::Variant::RFC4122, "variant 位必须是 RFC 4122");
    }
}
