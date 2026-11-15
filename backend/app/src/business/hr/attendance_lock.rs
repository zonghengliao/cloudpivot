//! 考勤「**锁定后写保护**」的唯一守卫面（卡 HR-23 / t_8aa9f8ed）。
//!
//! 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md` §4.2.3
//!
//! ```text
//! DRAFT ──提交──> SUBMITTED ──锁定──> LOCKED ──解锁(给原因, 需 attendance:lock)──> DRAFT
//! ```
//!
//! > `LOCKED` 之后**记录不许增删改**（handler 闸 + 巡检 A8）。
//! > 锁定不校验审批（HR 自己锁），但要 `attendance:lock` 权限码 + 写 `biz_status_log`。
//!
//! # 这一层守什么（与另外两层的分工）
//!
//! | 层 | 位置 | 拦得住什么 | 拦不住什么 |
//! |---|---|---|---|
//! | **接口层** | `#[nik::permission(... business:hr:attendance:lock)]`（HR-19 的 `#20` 状态路由） | 没有锁定权限的人调接口 | 有权限的人 + 别的写路径 |
//! | **handler 层** | [`lock_for_write`] / [`assert_writable`]（本文件，供所有写路径调用） | 忘了判状态的写路径（先锁期间行 → 判 `LOCKED` → 499 早退） | 绕过 handler 的裸 SQL |
//! | **库层** | `V0392` 的 `trg_biz_att_record_lock_guard`（BEFORE INSERT OR UPDATE） | 任何路径（导入 upsert / bridge / 手工脚本）对 `LOCKED` 期间的新增与修改 | 硬 `DELETE`（刻意不拦，理由见 `V0392` 文件头 ③） |
//!
//! 三层是**同一条规则的三处落点**，不是三套规则：
//! 判定只看 `biz_attendance_period.status`，`LOCKED` 是唯一不可写状态
//! （[`crate::business::shared::state_machine::ATTENDANCE_PERIOD_LOCKED`] 是唯一真相，
//! 单测交叉验证本文件的 [`WRITABLE_STATUSES`] 与它互补）。
//!
//! # 为什么 `SUBMITTED` 可写
//!
//! 提交只是「这一期数齐了，请对方看」，客户第一轮导入几乎总有错行要补；
//! 真正的冻结点是**锁定**（薪资试算的数据基线）。把 `SUBMITTED` 也冻上，
//! 用户就只能「提交 → 锁定 → 发现错 → 解锁（写原因）→ 改 → 再提交」空转一圈。
//!
//! # 怎么用（给后续卡的模板）
//!
//! ```ignore
//! let tx = db().begin().await?;
//! // ① 锁期间行（FOR UPDATE，与 HR-19 的锁定/解锁、HR-20 的修正抢同一把锁）
//! let period = attendance_lock::lock_for_write(&tx, period_id).await?;
//! // ↑ 期间已 LOCKED → 直接 Err(409 + 68010)，调用方 **rollback** 后返回
//! // ② 写记录（库层还有 V0392 的触发器兜底）
//! // ③ 万一还是撞上 CP001（例如期间在 ①② 之间被锁），用下面这个翻译：
//! let _ = attendance_lock::map_write_error(err, "写入考勤记录");
//! ```
//!
//! # 错误码约定
//!
//! 拦截一律 `409 + 68010`（`HR_ATTENDANCE_PERIOD_RULE_BLOCKED`）——
//! 与 HR-19「锁定后改/删/重算」、HR-20「锁定后单行修正」同码：前端只需要认
//! 「这一期动不了，去解锁或找 HR」一件事。库层触发器抛的是自定义 SQLSTATE
//! `CP001` + 固定前缀报文，[`is_lock_violation`] 认这个指纹。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::attendance_period_dto::{STATUS_DRAFT, STATUS_SUBMITTED};
use crate::business::hr::attendance_period_model::{self as period_model, AttendancePeriodRow};
use crate::shared::error_code;
use crate::utils;

/// 逃生门 GUC 名（**事务级**，只许用 `SET LOCAL`）。
///
/// 演练库重建 / 客户数据修复 / `V0383.down.sql` 这类必须动历史数据的场合才用；
/// 会话级 `SET` 会在连接池里留到下一个请求，等于把守卫永久关掉且无人知晓。
pub const GUARD_GUC: &str = "cloudpivot.attendance_lock_guard";

/// 守卫函数抛出的自定义 SQLSTATE（`V0392` 的 `RAISE … USING ERRCODE`）。
pub const GUARD_ERRCODE: &str = "CP001";

/// 守卫报文前缀：库层错误里出现它，就说明是「期间已锁定」而不是别的数据库故障。
pub const GUARD_MARKER: &str = "cloudpivot.attendance_lock_guard";

/// 逃生门的**唯一**写法（事务级），留给脚本与测试引用，避免有人手打会话级 `SET`。
pub const BYPASS_SQL: &str = "SET LOCAL cloudpivot.attendance_lock_guard = 'off'";

/// 可写状态（= 状态域里除 `LOCKED` 之外的全体）。
///
/// 单测 [`tests::writable_statuses_are_complement_of_locked`] 拿
/// `state_machine::ATTENDANCE_PERIOD_LOCKED` 对拍，两边改一个忘另一个就会红。
pub const WRITABLE_STATUSES: [&str; 2] = [STATUS_DRAFT, STATUS_SUBMITTED];

/// 锁定/解锁用的权限码（设计 §5.2 #20；与 `V0387` 的 `sys_menu.auth_code` 逐字一致）。
pub const LOCK_PERMISSION: &str = "business:hr:attendance:lock";

// ==================== 判定 ====================

/// 该状态允不允许写记录。
///
/// 未知状态**一律不可写**（fail closed）：状态域由 `ck_biz_att_period_status` 兜着，
/// 真出现第三个值是库被绕过改了，此时宁可拒绝写入也不要放进去。
pub fn is_writable(status: &str) -> bool {
    WRITABLE_STATUSES.contains(&status)
}

/// `409 + 68010`：期间已锁定，写不了。
///
/// 报文三段式（与 HR-19/HR-20 的拦截报文同款）：**哪一期** + **为什么不行** + **下一步**。
pub fn locked_error(period_code: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED,
        format!(
            "考勤期间 `{period_code}` 已锁定（LOCKED），记录不许新增、修改或删除；\
             请让有 `{LOCK_PERMISSION}` 权限的人先解锁（解锁必须填写原因，会写入状态日志），再改考勤"
        ),
    )
}

/// 判状态，不可写就 `409 + 68010`。
pub fn assert_writable(status: &str, period_code: &str) -> std::result::Result<(), ApiError> {
    if is_writable(status) {
        Ok(())
    } else {
        Err(locked_error(period_code))
    }
}

// ==================== 锁期间行 + 判状态 ====================

/// 写考勤记录前的**唯一**前置动作：锁期间行（`FOR UPDATE`）→ 判 `LOCKED` → 放行/报错。
///
/// # 为什么返回整行
///
/// 调用方通常还要 `period_code`（拼报文）、`period_start/end`（校验记录日期在期内）。
/// 再查一次不但多一次往返，还会读到别人的新状态（读到「已解锁」但写的时候又被锁上）。
///
/// # 调用方义务
///
/// 本函数**不持有事务**：返回 `Err` 时锁还握在你传进来的 `tx` 上，
/// 调用方必须 `tx.rollback()`（或让 `tx` 直接 drop）后再把错误往上抛，
/// 否则同一期间的其它写请求会一直等到事务超时。
///
/// # 加锁顺序
///
/// 固定「**期间 → 记录**」：HR-19 的锁定/解锁、HR-19 的重算、HR-20 的单行修正、
/// HR-21/22 的导入都先锁期间。所有人同一顺序才不会互相等成死锁
/// （PG 检测到死锁会杀掉其中一个，用户看到 500）。
pub async fn lock_for_write(
    ex: &impl SqlExecutor,
    period_id: Uuid,
) -> std::result::Result<AttendancePeriodRow, ApiError> {
    let row = match period_model::lock_in(ex, period_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(period_model::not_found_error()),
        Err(error) => return Err(utils::db_error(error, "锁定考勤记录的所属期间")),
    };
    assert_writable(&row.status, &row.period_code)?;
    Ok(row)
}

// ==================== 库层守卫的错误翻译 ====================

/// 数据库错误是不是「期间已锁定」这条守卫拒绝的（而不是别的库故障）。
///
/// # 为什么不能只看 `Display`
///
/// 守卫抛的是**自定义 SQLSTATE** `CP001`，`nik-db-core` 的
/// `DbError::from_raw_driver_error` 认不出这个码，落到
/// `DbError::Query { sql: "", source }`，其 `Display` 是 `query failed: : db error`
/// ——tokio-postgres 的 `Error::fmt` 只写 `"db error"`，服务端报文（含我们的
/// 前缀和 HINT）**只在 `Debug` 与 `source()` 链里**。实测（真库）第一版只认
/// `Display` 时三条用例全红在「拒绝必须能认出来」这一步。
///
/// 所以这里沿 `source()` 链逐层用 `Display` + `Debug` 双查；最后再用整条
/// `Debug` 兜一次（`Query` 的 Debug 会把 `sql` 与 `source` 都展开）。
///
/// 反面的风险也钉着：普通故障（连接重置、语法错）不该被误认成「已锁定」，
/// 否则 500 会被掩盖成「去解锁」——单测
/// [`tests::non_lock_failure_is_not_mistaken_for_lock`] 与
/// `attendance_record_model::tests::lock_guard_violation_becomes_409_68010` 各钉一侧。
pub fn is_lock_violation(error: &DbError) -> bool {
    text_is_guard(&error_chain_text(error))
}

/// 把错误连同 `source()` 链拼成一段文本（每层 `Display` + `Debug`）。
///
/// 用途有两个：① [`is_lock_violation`] 的匹配源；② 记日志 / 给用户看「到底哪个约束、
/// 哪个字段」——驱动把服务端报文藏在 `Debug` 里，只打 `Display` 会丢信息。
pub fn error_chain_text(error: &DbError) -> String {
    let mut text = format!("{error} | {error:?}");
    let mut source = std::error::Error::source(error);
    while let Some(current) = source {
        text.push_str(&format!(" | {current} | {current:?}"));
        source = current.source();
    }
    text
}

/// 报文里有没有守卫指纹（前缀 / SQLSTATE）。
fn text_is_guard(text: &str) -> bool {
    text.contains(GUARD_MARKER) || text.contains(GUARD_ERRCODE)
}

/// 写考勤记录失败的统一翻译：守卫拒绝 → `409 + 68010`；其它 → `500 + 10004`。
///
/// 给「先写后判」的实现留着（导入 upsert 用一条 `INSERT … ON CONFLICT` 批处理时，
/// 逐行先查期间状态反而更慢）。正常路径请先 [`lock_for_write`]，别拿它当主判据。
pub fn map_write_error(error: DbError, ctx: &str) -> ApiError {
    if is_lock_violation(&error) {
        tracing::warn!(context = ctx, error = %error_chain_text(&error), "考勤记录写入被库层锁定守卫拦下");
        return ApiError::new(
            409,
            error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED,
            format!(
                "考勤期间已锁定（LOCKED），记录不许新增、修改或删除；\
                 请让有 `{LOCK_PERMISSION}` 权限的人先解锁（解锁必须填写原因，会写入状态日志），再改考勤"
            ),
        );
    }
    utils::db_error(error, ctx)
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::attendance_period_dto::{STATUS_LOCKED, STATUS_VALUES};
    use crate::business::hr::handler::{ROUTE_CONTRACT, RouteSpec};
    use crate::business::shared::state_machine::{
        ATTENDANCE_PERIOD_EDGES, ATTENDANCE_PERIOD_LOCKED, ATTENDANCE_PERIOD_REASON_EDGES,
    };

    /// 迁移文件（编译期读进来，删文件/改关键字都会编译失败或断言红）。
    const V0392_UP: &str = include_str!("../../../migrations/business/V0392__attendance_lock_guard.up.sql");
    const V0392_DOWN: &str = include_str!("../../../migrations/business/V0392__attendance_lock_guard.down.sql");

    #[test]
    fn writable_statuses_are_complement_of_locked() {
        // 状态域三值：LOCKED 之外都可写。两边（本文件 / state_machine）改一个忘另一个就红。
        for status in STATUS_VALUES {
            let locked = ATTENDANCE_PERIOD_LOCKED.contains(&status);
            assert_eq!(
                is_writable(status),
                !locked,
                "状态 `{status}`：is_writable 与 ATTENDANCE_PERIOD_LOCKED 不一致"
            );
        }
        // 可写集合里不许混进 LOCKED（防有人手滑把 LOCKED 加进白名单）
        assert!(!WRITABLE_STATUSES.contains(&STATUS_LOCKED));
        // 未知状态 fail closed
        assert!(!is_writable("CLOSED"));
        assert!(!is_writable(""));
    }

    #[test]
    fn assert_writable_reports_409_68010_with_period_code() {
        let error = assert_writable(STATUS_LOCKED, "2401-07").expect_err("锁定态必须拒绝");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED);
        assert!(error.msg.contains("2401-07"), "报文要点名期间：{}", error.msg);
        assert!(
            error.msg.contains(LOCK_PERMISSION),
            "报文要指出下一步（权限码）：{}",
            error.msg
        );
        assert!(error.msg.contains("解锁"), "报文要指明解锁：{}", error.msg);
        // 未知状态同样拒绝（fail closed），且不会 panic
        assert!(assert_writable("WHATEVER", "2401-07").is_err());
    }

    #[test]
    fn draft_and_submitted_are_writable() {
        assert!(assert_writable(STATUS_DRAFT, "2401-07").is_ok());
        assert!(assert_writable(STATUS_SUBMITTED, "2401-07").is_ok());
    }

    #[test]
    fn guard_marker_is_the_migration_prefix() {
        // 报文前缀与迁移里的 RAISE 逐字一致（否则 is_lock_violation 永远认不出来）
        assert!(V0392_UP.contains(GUARD_MARKER));
        assert!(V0392_UP.contains(GUARD_ERRCODE));
        assert!(V0392_UP.contains(GUARD_GUC));
        assert!(V0392_UP.contains(BYPASS_SQL), "逃生门写法要能在迁移与脚本里照抄");
    }

    #[test]
    fn migration_guards_exactly_insert_and_update() {
        // 只拦 INSERT / UPDATE：硬 DELETE 留给清场与 ON DELETE CASCADE（理由见文件头 ③）
        assert!(
            V0392_UP.contains("BEFORE INSERT OR UPDATE ON biz_attendance_record"),
            "触发器事件必须是 INSERT OR UPDATE"
        );
        assert!(
            !V0392_UP.contains("BEFORE INSERT OR UPDATE OR DELETE"),
            "不许把 DELETE 也挂上：演练清场 / 回滚会删不掉记录"
        );
        assert!(V0392_UP.contains("biz_attendance_period"));
        assert!(V0392_UP.contains("'LOCKED'"));
        // 幂等：函数可重建 + 触发器先删后建
        assert!(V0392_UP.contains("CREATE OR REPLACE FUNCTION"));
        assert!(V0392_UP.contains("DROP TRIGGER IF EXISTS trg_biz_att_record_lock_guard"));
    }

    #[test]
    fn down_migration_removes_trigger_and_function() {
        assert!(V0392_DOWN.contains("DROP TRIGGER IF EXISTS trg_biz_att_record_lock_guard"));
        assert!(V0392_DOWN.contains("DROP FUNCTION IF EXISTS fn_biz_att_record_lock_guard()"));
        // down 不许动数据
        assert!(!V0392_DOWN.contains("DELETE FROM"));
        assert!(!V0392_DOWN.contains("UPDATE biz_attendance_record"));
    }

    #[test]
    fn lock_and_unlock_route_carries_lock_permission() {
        // 验收项「解锁要权限」的第一道闸：设计 §5.2 #20 的状态路由必须挂 attendance:lock
        let route = ROUTE_CONTRACT
            .iter()
            .find(|spec| spec.path == "/business/hr/attendance/period/{id}/status")
            .expect("契约表里要有考勤期间状态路由（设计 §5.2 #20）");
        let RouteSpec {
            method,
            permission,
            card,
            ..
        } = route;
        assert_eq!(*method, "POST");
        assert_eq!(
            *permission,
            Some(LOCK_PERMISSION),
            "锁定/解锁必须挂 `{LOCK_PERMISSION}`，否则「解锁要权限」形同虚设"
        );
        assert_eq!(*card, "HR-19", "状态路由由 HR-19 落地，本卡只补写保护");
    }

    #[test]
    fn record_write_route_carries_record_permission() {
        // 记录层写入口（#22 单行修正）只许挂 attendance:record：不该顺手把它升成 lock 权限
        let route = ROUTE_CONTRACT
            .iter()
            .find(|spec| spec.path == "/business/hr/attendance/record/{id}")
            .expect("契约表里要有台账单行修正路由（设计 §5.2 #22）");
        assert_eq!(route.method, "PUT");
        assert_eq!(route.permission, Some("business:hr:attendance:record"));
    }

    #[test]
    fn unlock_edge_requires_reason() {
        // 验收项「解锁要审计」的第一道闸：解锁边必须在 reason 边表里（否则不给原因也能解锁）
        assert_eq!(ATTENDANCE_PERIOD_EDGES.len(), 3);
        assert!(
            ATTENDANCE_PERIOD_REASON_EDGES.contains(&(STATUS_LOCKED, STATUS_DRAFT)),
            "LOCKED → DRAFT（解锁）必须在 ATTENDANCE_PERIOD_REASON_EDGES 里"
        );
        // 另外两条边（提交 / 锁定）刻意不要原因
        assert_eq!(ATTENDANCE_PERIOD_REASON_EDGES, [(STATUS_LOCKED, STATUS_DRAFT)]);
    }

    #[test]
    fn bypass_sql_is_transaction_scoped() {
        // 会话级 SET 会在连接池里留到下一个请求，把守卫永久关掉 —— 只许 SET LOCAL
        assert!(BYPASS_SQL.starts_with("SET LOCAL"));
        assert!(!BYPASS_SQL.contains("SET cloudpivot"));
        // 默认值必须是「开」：忘了设的后果是多拦一次，不是漏拦
        assert!(V0392_UP.contains("coalesce(current_setting('cloudpivot.attendance_lock_guard', true), 'on')"));
    }

    /// 模拟 tokio-postgres 的错误：`Display` 只有 `"db error"`，报文（含守卫前缀）
    /// 只在 `Debug` 里 —— 这正是真库实测的样子。
    ///
    /// `Debug` 手写而不用 `derive`：一是要复刻「报文只在 Debug」的形态，二是 `derive`
    /// 的字段不算「被使用」，`-D warnings` 下会以 dead_code 红掉。
    struct FakePgError {
        message: String,
    }

    impl std::fmt::Debug for FakePgError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FakePgError").field("message", &self.message).finish()
        }
    }

    impl std::fmt::Display for FakePgError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("db error")
        }
    }

    impl std::error::Error for FakePgError {}

    fn driver_error(message: &str) -> DbError {
        DbError::Query {
            sql: String::new(),
            source: Box::new(FakePgError {
                message: message.to_string(),
            }),
        }
    }

    /// 守卫拒绝藏在驱动 `Debug` / `source()` 链里也必须能认出来。
    ///
    /// 只认 `Display` 的写法在真库上是**永远认不出来**的（`CP001` 落 `DbError::Query`，
    /// `Display` = `query failed: : db error`），前端就会看到 500「服务器内部错误」。
    #[test]
    fn driver_hidden_message_is_still_recognised() {
        let error = driver_error(&format!("ERROR: {GUARD_MARKER}: 考勤期间 2026-11 已锁定"));
        assert!(
            !error.to_string().contains(GUARD_MARKER),
            "前提：Display 认不出来（tokio-postgres 只写 db error）"
        );
        assert!(is_lock_violation(&error), "必须能从 Debug / source 链认出守卫拒绝");

        // 翻译层随之翻成 409 + 68010（而不是 500）
        let api = map_write_error(error, "插入考勤记录");
        assert_eq!(api.status, 409);
        assert_eq!(api.code, error_code::HR_ATTENDANCE_PERIOD_RULE_BLOCKED);
        assert!(api.msg.contains("解锁"), "报文要给下一步：{}", api.msg);
    }

    /// 反面：别的库故障不许被误认成「已锁定」（否则真故障被掩盖成「去解锁」）。
    #[test]
    fn non_lock_failure_is_not_mistaken_for_lock() {
        let error = driver_error("ERROR: connection reset by peer");
        assert!(!is_lock_violation(&error));
        assert_eq!(map_write_error(error, "插入考勤记录").status, 500);

        // 唯一约束冲突这类有专有变体的错误也不该被认成锁定
        assert!(!is_lock_violation(&DbError::NotFound));
    }
}
