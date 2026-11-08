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
