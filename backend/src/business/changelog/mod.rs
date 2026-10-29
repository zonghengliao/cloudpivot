//! 变更日志（批次 C · C5 / C6）。
//!
//! 依据 `docs/批次C-技术架构.md` §4（变更日志）：现状缺陷与「假日志」证据（§4.1）、
//! 三方案对比（§4.2）、月分区表设计（§4.3）、diff 结构（§4.4）、大字段摘要化（§4.5）、
//! 事务内记录的三条约束（§4.6）、与数据脱敏联动（§4.7）、索引设计（§4.8）、
//! 保留策略（§4.9）、API 契约（§4.10）、回滚不留假日志的三层测试（§4.12）。
//!
//! 模块名取 `changelog`（不是 `change_log`）：架构 §5.4.1 B-4 与 §4.2/§4.12 的文件路径
//! （`business/changelog/registry.rs`、`business/changelog/tests`）都是这个拼法，
//! `business/mod.rs` 的 `pub mod changelog;` 也早已按此声明。
//!
//! # 本模块解决什么问题（一句话）
//!
//! 「**谁**在**什么时候**把**哪条数据的哪个字段**从**什么**改成了**什么**」——
//! 而且这条记录**要么和业务一起提交，要么和业务一起消失**。
//!
//! # 三条硬决策（改任何一条都等于推翻本模块）
//!
//! | 决策 | 内容 | 物理保障 |
//! |------|------|----------|
//! | **D11** | 成功路径的日志与业务**同事务**写；失败尝试不进本表 | [`record`] 与 [`model`] 的每个函数都只吃 `&impl SqlExecutor`，模块内**没有** `db()`（门禁 G2） |
//! | **D12** | diff 只记变化字段、`{b,a}` 成对；单字段 > 2 KiB 或登记为 text/bytea → 摘要化 | [`diff::build`] 是纯函数，四条铁律逐条有单测 |
//! | **D13** | 库内永不存明文：写入前**无条件**脱敏，不看操作人角色 | [`mask::Policy`] 的签名里**没有操作人**参数 |
//!
//! # 文件职责
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`dto`] | [`dto::ChangeCtx`] / [`dto::ChangeTarget`] / [`dto::DiffInput`] / [`dto::Action`] 等纯数据 |
//! | [`diff`] | 白名单登记（[`diff::REGISTRY`]）+ 变化检测 + 大字段摘要化（纯函数） |
//! | [`mask`] | 写入侧脱敏 L1：`sys_field_mask` + 12 个内置兜底字段（纯函数 + 一条同事务查询） |
//! | [`model`] | `sys_change_log` 的 INSERT 与按对象查询（**只吃 executor**） |
//! | 本文件 | [`record`]：把上面四件拼成一次「读规则 → 造 diff → 落库」 |
//!
//! # 标准调用形态
//!
//! ```ignore
//! db().transaction(|tx| async move {
//!     let before = user_model::find(&username).await?;   // 旧值（事务内）
//!     user_model::update(&tx, &id, &req).await?;         // 业务写
//!     changelog::record(                                 // ← 传的是 &tx，不是 db()
//!         &tx,
//!         &ChangeCtx::api(Some(uid), Some(&name)).with_request_id(Some(&rid)),
//!         ChangeTarget::of("sys_user", id.to_string(), Action::Update, "修改用户"),
//!         DiffInput::update(&json!(before), &json!(after)),
//!     )
//!     .await?;                                            // 失败 → 整个事务回滚（这是要的）
//!     Ok(())
//! })
//! .await?;
//! ```
//!
//! # 不做什么（C5 边界）
//!
//! - 不提供查询接口、不做分区运维任务（C6）；
//! - 不改 `operation_logs` / `audit_logs`：它们的职责是「请求流水」「安全事件」，
//!   与「字段级变更凭证」不同（§4.2.1），定位说明与改造在 C6；
//! - 不做「失败尝试」的记录：失败若写进同事务会被自己回滚，写进独立事务就是 F1 假日志的翻版
//!   （§4.3.3）。失败的痕迹在 `operation_logs.status_code` 里。

pub mod diff;
pub mod dto;
pub mod mask;
pub mod model;

use alun::prelude::*;
use alun::uuid::Uuid;

use crate::shared::error_code;

pub use dto::{Action, ChangeCtx, ChangeTarget, DiffInput, RecordOutcome, TraceSource};
pub use model::NewChangeLog;

/// 允许的时钟偏斜：`occurred_at` 最多可以领先「现在」这么多，再多就是未来时间。
///
/// 5 分钟不是随手取的：分布式节点与数据库之间的 NTP 偏差通常在秒级，5 分钟足够容忍
/// 正常漂移，又能挡住「故意写一条 2029 年的日志」这种把分区规划打乱的输入。
const MAX_CLOCK_SKEW_MINUTES: i64 = 5;

/// 记录一条**已成功**的变更（D11：与业务同事务）。
///
/// # 参数
///
/// - `ex`：**业务事务的执行器**。⚠️ 红线：绝不能传 `db()`——那会开第二个事务，
///   业务回滚时日志留下，正是本模块要消灭的 F1「假日志」；
/// - `ctx`：操作人 / 来源 / IP / 请求 ID / 发生时刻；
/// - `target`：表名 + 业务主键 + 动作；
/// - `input`：变更前 / 变更后的两份 JSON。
///
/// # 返回值
///
/// - [`RecordOutcome::Recorded`]：已写入，含行 ID 与变化字段名；
/// - [`RecordOutcome::Skipped`]：所有白名单字段前后相等 → **不写空日志**。
///
/// # 异常情况
///
/// - 表未登记白名单 → `500 + 62012`（接入缺陷，必须报警，不静默降级成全字段）；
/// - `occurred_at` 在未来（超出 [`MAX_CLOCK_SKEW_MINUTES`]）→ `400 + 62013`；
/// - 数据库异常（含分区越界）→ 原样上抛 [`ApiError`]，**让业务事务一起回滚**。
///
/// # 与脱敏的关系（D13）
///
/// 脱敏在**构造 diff 时**完成，即「写库前」；`sys_field_mask` 的规则用**同一个 executor**
/// 读取（同事务快照）。本函数没有「操作人角色」这一路判断，`SUPER_ADMIN` 写的日志同样是脱敏值。
pub async fn record(
    ex: &impl SqlExecutor,
    ctx: &ChangeCtx<'_>,
    target: ChangeTarget<'_>,
    input: DiffInput<'_>,
) -> std::result::Result<RecordOutcome, ApiError> {
    let (spec, created_at) = preflight(ctx, &target)?;
    // L1 脱敏规则：同一条事务、同一个快照里读（不调 db()）
    let policy = mask::Policy::load(ex, spec.table).await?;
    let doc = diff::build(
        spec,
        &policy,
        target.action,
        &target.resource_id,
        input.before,
        input.after,
    );
    if doc.is_empty() {
        return Ok(RecordOutcome::Skipped);
    }
    let changed_fields = doc.changed_fields();
    let diff = doc.to_json_string().map_err(|error| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("变更日志 diff 序列化失败：{error}"),
        )
    })?;
    let id = Uuid::now_v7();
    let row = NewChangeLog {
        id,
        request_id: ctx.request_id,
        trace_source: ctx.trace_source.code(),
        operator_id: ctx.operator_id,
        operator_name: ctx.operator_name,
        operator_dept_id: ctx.operator_dept_id,
        client_ip: ctx.client_ip,
        module: spec.module,
        resource: spec.table,
        resource_id: &target.resource_id,
        action: target.action.code(),
        action_label: target.action_label,
        diff,
        diff_mode: "WHITELIST",
        changed_fields: &changed_fields,
        error_code: None,
        duration_ms: ctx.duration_ms,
        created_at,
    };
    model::insert(ex, &row).await?;
    Ok(RecordOutcome::Recorded { id, changed_fields })
}

/// 落库之前的纯校验：登记表 + 时间合法性。
///
/// 抽成纯函数（不碰数据库）有两个好处：① 62012 / 62013 两条错误路径能在**无库**环境单测；
/// ② [`record`] 只剩「读规则 → 造 diff → 落库」三段，评审时一眼能看出它没开第二个事务。
///
/// # 参数
///
/// - `ctx`：操作主体上下文（取 `occurred_at`）；
/// - `target`：变更对象（取 `resource`）。
///
/// # 返回值
///
/// 登记条目与最终使用的 `created_at`。
///
/// # 异常情况
///
/// 未登记 → `500 + 62012`；未来时间 → `400 + 62013`。
fn preflight<'a>(
    ctx: &ChangeCtx<'_>,
    target: &ChangeTarget<'a>,
) -> std::result::Result<(&'static diff::ResourceSpec, chrono::DateTime<chrono::Utc>), ApiError> {
    let spec = diff::spec(target.resource).ok_or_else(|| {
        ApiError::new(
            500,
            error_code::CHANGE_LOG_RESOURCE_UNREGISTERED,
            format!(
                "变更日志：资源「{}」未登记白名单，拒绝记录（请在 business/changelog/diff.rs 的 REGISTRY 里登记）",
                target.resource
            ),
        )
    })?;

    let now = chrono::Utc::now();
    let created_at = ctx.occurred_at.unwrap_or(now);
    if created_at > now + chrono::Duration::minutes(MAX_CLOCK_SKEW_MINUTES) {
        return Err(ApiError::new(
            400,
            error_code::CHANGE_LOG_TIME_INVALID,
            format!("变更时间不合法：{created_at} 晚于当前时间超过 {MAX_CLOCK_SKEW_MINUTES} 分钟"),
        ));
    }
    Ok((spec, created_at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> ChangeCtx<'static> {
        ChangeCtx::api(None, None)
    }

    /// 未登记的表在**进入数据库之前**就被拒（62012 / HTTP 500）。
    ///
    /// 这条是无库版本的断言；真库版本（`record()` 端到端）在
    /// `tests/batchC_c5_changelog.rs::record_rejects_unregistered_table`。
    #[test]
    fn preflight_rejects_unregistered_table() {
        let target = ChangeTarget::of("sys_not_registered", "x-1", Action::Update, "改点啥");
        let error = preflight(&ctx(), &target).expect_err("未登记的资源必须被拒");
        assert_eq!(error.code, error_code::CHANGE_LOG_RESOURCE_UNREGISTERED);
        assert_eq!(error.code, 62012);
        assert_eq!(error.status, 500, "接入缺陷走 500，必须能被监控抓到");
        assert!(error.msg.contains("sys_not_registered"), "{}", error.msg);
    }

    /// 已登记的表放行，并带出登记表里的模块归属（调用方无权覆盖）。
    #[test]
    fn preflight_accepts_registered_table() {
        let target = ChangeTarget::of("sys_user", "u-1", Action::Update, "修改用户");
        let (spec, created_at) = preflight(&ctx(), &target).expect("sys_user 已登记");
        assert_eq!(spec.table, "sys_user");
        assert_eq!(spec.module, "system");
        assert!(created_at <= chrono::Utc::now());
    }

    /// 未来时间 → 400 + 62013（防止「写一条 2029 年的日志」打乱分区规划）。
    #[test]
    fn preflight_rejects_future_time() {
        let target = ChangeTarget::of("sys_user", "u-1", Action::Update, "修改用户");
        let future = chrono::Utc::now() + chrono::Duration::days(2);
        let ctx = ctx().with_occurred_at(Some(future));
        let error = preflight(&ctx, &target).expect_err("未来时间必须被拒");
        assert_eq!(error.code, error_code::CHANGE_LOG_TIME_INVALID);
        assert_eq!(error.status, 400);
    }

    /// 小偏斜（1 分钟）放行：NTP 抖动不该让业务失败。
    #[test]
    fn preflight_tolerates_small_skew() {
        let target = ChangeTarget::of("sys_user", "u-1", Action::Update, "修改用户");
        let soon = chrono::Utc::now() + chrono::Duration::minutes(1);
        let (_, created_at) =
            preflight(&ctx().with_occurred_at(Some(soon)), &target).expect("1 分钟偏斜应放行");
        assert_eq!(created_at, soon, "显式传入的发生时刻必须原样使用");
    }

    /// 动作与来源的编码必须与 V0084 的 CHECK 约束逐字一致（写错就是 500 约束冲突）。
    #[test]
    fn action_and_source_codes_match_ddl() {
        let ddl_actions = [
            "CREATE", "UPDATE", "DELETE", "STATUS", "ASSIGN", "PUBLISH", "OFFLINE", "UPLOAD",
            "DOWNLOAD", "CLEAN", "LOGIN",
        ];
        assert_eq!(Action::ALL.len(), ddl_actions.len());
        for action in Action::ALL {
            assert!(
                ddl_actions.contains(&action.code()),
                "动作 {} 不在 DDL 的 CHECK 里",
                action.code()
            );
        }
        let ddl_sources = ["API", "JOB", "IMPORT", "SYSTEM"];
        assert_eq!(TraceSource::ALL.len(), ddl_sources.len());
        for source in TraceSource::ALL {
            assert!(ddl_sources.contains(&source.code()));
        }
    }

    /// diff 的 `meta.action` 必须来自同一个 `Action::code()`（不是另一套字面量）。
    #[test]
    fn diff_meta_action_matches_code() {
        let policy = mask::Policy::builtin_only("sys_user");
        let before = json!({"username": "alice"});
        let after = json!({"username": "alice2"});
        let doc = diff::build(
            &diff::SYS_USER,
            &policy,
            Action::Status,
            "u-1",
            Some(&before),
            Some(&after),
        );
        assert_eq!(doc.meta.action, "STATUS");
        assert_eq!(doc.meta.table, "sys_user");
        assert_eq!(doc.meta.pk, "u-1");
    }
}
