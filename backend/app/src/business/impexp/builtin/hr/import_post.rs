//! 考勤导入的**导入后处理**（`hr.attendance` · HR-22）。
//!
//! 设计定稿：`docs/architecture/ARCH-9-人力资源模块设计.md` §8.1（HR-22 行）+
//! §3.2 ⑩（`sys_config.hr.attendance.require_leave_match`）+ §4.2.2（`paid = 0` 的假
//! 才是薪资扣款口径）+ §3.1 ④（期间的四个汇总列「导入 / 修改后重算」）。
//! 本卡交三件事：
//!
//! | # | 做什么 | 落在哪 |
//! |:--:|---|---|
//! | 1 | 「无薪假 ≤ 已批准请假天数」校验 | [`leave_mismatches`]（`validate_batch` 调，命中即整批拒绝） |
//! | 2 | 汇总重算（`employee_count` / 三个合计列） | [`recalc_periods`]（`persist` 内**同一事务**调） |
//! | 3 | 错误行报告（行号 + 原因） | 复用框架的 `sys_import_task_error` 通路（[`mismatch_message`] 出人话） |
//!
//! # 为什么校验放在「批次校验」而不是「入库预检」
//!
//! 这条规则要**整批 + 库里已有数据**才有答案（一人一月的无薪假合计 vs 该员工在当期
//! 已批准的无薪请假单）。框架的逐行 `validate_row` 是纯计算（不发 SQL），入库的
//! `precheck` 只在**已经决定要入库**之后跑（那时再报错，用户拿到的是一条 422 报文，
//! 错误明细表里什么都没有）。所以在 `ImportDefinition::validate_batch` 里做：
//! 上传 / 确认两个阶段都会跑，**错误明细页与「错误行 xlsx」直接可见**（含 Excel 行号），
//! 且 `STRICT` 模式下整批被拒（`error > 0` → 一行都不入库）。
//!
//! # 为什么汇总重算必须在 `persist` 的事务里
//!
//! 考勤的 `batch_size = max_rows`：一份文件一个事务，`STRICT` 才是真全有或全无。
//! 汇总重算若放在事务提交之后，重算失败就留下「记录进了库、期间汇总还是旧值」的中间态，
//! 而汇总列是工资试算的依据（`AGENTS.md` 约定 11 的红线：提交后失败必须有补偿）。
//! 放进 `persist` 的事务里 ⇒ 记录与汇总**一起提交、一起回滚**，不存在中间态。
//! 重算本身是全量覆盖（`attendance_period_model::recalc_totals`：`UPDATE … FROM (SELECT
//! count/SUM …)`），重导多少次结果都只跟当前记录有关，天然幂等。
//!
//! # 「导入后期间头还是 0」是怎么定位的（t_de6e8d41，2026-11-12 复核）
//!
//! HR-29 在真机上报过「明细 2 行、期间合计 0 人」，怀疑本后处理只挂在一张**尚未注册**的
//! HR 专属路由上。复核结论：**没有这回事** —— 重算就长在 [`crate::business::impexp::builtin::hr::AttendanceImport::persist`]
//! 里，而通用三段（`upload` / `template` / `{id}/confirm`）的确认入库正是逐批调 `persist`，
//! 所以任何入口（页面、脚本、其他前端、后续批量导入卡）走通用 impexp 都会重算。
//!
//! 复核证据（145 演练库 + 真后端 + 真实 HTTP，不看页面）：空期间 → 上传 2 行 →
//! `POST /business/impexp/import/{id}/confirm` → **不调任何 recalc 接口**直接
//! `GET /business/hr/attendance/period/page` 即得 `employee_count=2 / should=44.00 /
//! actual=42.00`；锁定期间再导被入库预检拦下且期间头不动。HR-29 看到的 0 最可能是当时
//! 跑着的后端二进制早于 e9c8f43e（本后处理落地的提交），不是代码现状。
//!
//! 复核顺带补上的护栏见 [`recalc_one`]：旧实现把 `recalc_totals` 的返回值丢掉，
//! 「可重算的期间却被更新 0 行」（并发锁定 / 软删）会被当成成功 —— 那是唯一能让
//! 「记录进库、期间头不动」悄悄发生的路径，现在直接整批回滚并点名考勤月。
//!
//! # 「已批请假天数」的口径（写清楚，别让人猜）
//!
//! - 只算 `status = 'APPROVED'`（`DRAFT` / `SUBMITTED` / `CANCELLED` 都不算：
//!   未批准的假不能当扣款依据，否则「先导考勤、慢慢补假」就能绕过校验）；
//! - 只算 `paid = 0`（带薪假不进扣款口径，见设计 §4.2.2 末条与 §3.1 ⑥ 的列注释）；
//! - 期间重叠按**北京时间的自然日**判：`请假起日 <= 期间止 AND 请假止日 >= 期间起`。
//!   `start_time` / `end_time` 是 `timestamptz`，直接 `AT TIME ZONE 'Asia/Shanghai'`
//!   再取日期 —— 不取 date 会让「10-31 23:00 UTC 的假」落到 11 月（跨时区多算一天）。
//! - 跨月的假**整段天数都算进它碰到的每个期间**（不从中间劈开）：`duration_days` 是请假
//!   单上的一个整数 / 半整数，劈开就只能按日历比例猜，猜错的方向是「把合法的无薪假判成
//!   超批」（错杀比放过更糟）。所以这条校验是**下限护栏**：它只拦「无薪假 > 已批天数」这种
//!   一定说不通的情况，不保证「填了无薪假就一定有假单」的全部情形。
//!
//! # 开关
//!
//! `sys_config.hr.attendance.require_leave_match`（`BOOLEAN`，V0388 种下，默认 `TRUE`）。
//! 关掉后校验直接放过（历史客户「没有请假单、只导考勤」的过渡期用），但**汇总重算照做**
//! —— 开关只管一条业务规则，不该影响数据一致性。

use std::collections::BTreeSet;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::rust_decimal::prelude::FromStr;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::attendance_period_model;
use crate::business::hr::model::SENTINEL_ORG_ID;

/// 校验开关的配置键（V0388 种下，`config_level = 1`）。
pub const CONFIG_REQUIRE_LEAVE_MATCH: &str = "hr.attendance.require_leave_match";

/// 读开关的 SQL（`sys_config` 的键是**未软删部分唯一**，所以一句就够）。
const CONFIG_SQL: &str = "\
SELECT config_value FROM sys_config \
 WHERE config_key = ? AND deleted_at IS NULL \
 ORDER BY created_at DESC LIMIT 1";

/// 「无薪假 ≤ 已批请假天数」的批量校验 SQL（一条往返覆盖整批，无 N+1）。
///
/// 绑定参数（6 个，顺序固定）：`rows` 的 JSON / 组织 ×5（员工、期间、请假、外层员工、
/// 外层期间各一次 —— 与 `builtin::hr` 的预检同一写法，全部显式带 `org_id`，
/// 多组织落地后只改 `SENTINEL_ORG_ID` 一处）。
///
/// 返回值：**只有说不通的行**（`无薪假 > 已批天数`），已按 Excel 行号排序。
/// 两列天数都转成 `text` 返回（`numeric` 在本框架的行读取里按文本更稳，与
/// `leave_model::approved_unpaid_days` 同口径）。
pub const LEAVE_MATCH_SQL: &str = "\
WITH v AS ( \
    SELECT * FROM jsonb_to_recordset(?::jsonb) \
        AS x(row_no int, period_code text, emp_no text, leave_unpaid_days numeric) \
), agg AS ( \
    SELECT e.id AS employee_id, p.id AS period_id, SUM(l.duration_days) AS approved_days \
      FROM (SELECT DISTINCT period_code, emp_no FROM v) ve \
      JOIN biz_employee e \
        ON e.emp_no = ve.emp_no AND e.org_id = ?::uuid AND e.deleted_at IS NULL \
      JOIN biz_attendance_period p \
        ON p.period_code = ve.period_code AND p.org_id = ?::uuid AND p.deleted_at IS NULL \
      JOIN biz_leave_request l \
        ON l.employee_id = e.id AND l.org_id = ?::uuid AND l.deleted_at IS NULL \
       AND l.status = 'APPROVED' AND l.paid = 0 \
       AND (l.start_time AT TIME ZONE 'Asia/Shanghai')::date <= p.period_end \
       AND (l.end_time   AT TIME ZONE 'Asia/Shanghai')::date >= p.period_start \
     GROUP BY 1, 2 \
) \
SELECT v.row_no::text AS row_no, v.emp_no, v.period_code, \
       COALESCE(v.leave_unpaid_days, 0)::text AS leave_unpaid_days, \
       COALESCE(agg.approved_days, 0)::text AS approved_days \
  FROM v \
  JOIN biz_employee e \
    ON e.emp_no = v.emp_no AND e.org_id = ?::uuid AND e.deleted_at IS NULL \
  JOIN biz_attendance_period p \
    ON p.period_code = v.period_code AND p.org_id = ?::uuid AND p.deleted_at IS NULL \
  LEFT JOIN agg ON agg.employee_id = e.id AND agg.period_id = p.id \
 WHERE COALESCE(v.leave_unpaid_days, 0) > COALESCE(agg.approved_days, 0) \
 ORDER BY v.row_no";

/// 按期间码取「重算目标」（主键 + 期间码 + 状态），一条往返。
///
/// 三个列都要：`period_code` 用于**报错点名**（用户认的是「2026-11」，不是 UUID），
/// `status` 用于区分「锁定期间（按设计冻结，重算 0 行是正确的）」与「更新 0 行的异常」。
const PERIOD_IDS_SQL: &str = "\
SELECT p.id::text AS id, p.period_code, p.status \
  FROM jsonb_array_elements_text(?::jsonb) AS c(period_code) \
  JOIN biz_attendance_period p \
    ON p.period_code = c.period_code AND p.org_id = ?::uuid AND p.deleted_at IS NULL";

/// 期间状态 `LOCKED`（与 `attendance_period_dto::STATUS_LOCKED` 同值）。
///
/// 这里写字面量而不是 import DTO 常量：本模块只判断「能不能重算」，不参与状态机；
/// 单测 `locked_status_matches_the_page_contract` 与 DTO 常量对拍，防止哪天改了码值。
const STATUS_LOCKED: &str = "LOCKED";

/// 一个「可能需要重算」的期间（[`PERIOD_IDS_SQL`] 的一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecalcTarget {
    /// 期间主键。
    pub id: Uuid,
    /// 考勤月（报错点名用）。
    pub period_code: String,
    /// 期间状态（`DRAFT` / `SUBMITTED` / `LOCKED`）。
    pub status: String,
}

/// 汇总重算的回执。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecalcReport {
    /// 真正重算的期间数。
    pub recalced: usize,
    /// 因为**已锁定**而跳过的期间数（锁定 = 定稿，汇总列不许再变，跳过是设计意图）。
    pub skipped_locked: usize,
    /// 文件里有、库里却查不到的期间码（入库 SQL 已 JOIN 过期间表，正常为空；
    /// 非空只可能是并发软删了期间，或 `SKIP` 策略把该期间的行全跳过）。
    pub missing: Vec<String>,
}

/// 一行「待校验」的考勤数据（只有校验需要的四列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveMatchRow {
    /// Excel 真实行号（错误明细要靠它定位）。
    pub row_no: u32,
    /// 考勤月 `yyyy-MM`。
    pub period_code: String,
    /// 工号。
    pub emp_no: String,
    /// 文件里填的无薪假天数（空串 → `None`，按 0 算）。
    pub leave_unpaid_days: Option<String>,
}

/// 一条「无薪假超批」命中。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveMismatch {
    /// Excel 真实行号。
    pub row_no: u32,
    /// 考勤月。
    pub period_code: String,
    /// 工号。
    pub emp_no: String,
    /// 文件里填的无薪假天数。
    pub unpaid_days: Decimal,
    /// 该员工在当期**已批准的无薪**请假单天数合计。
    pub approved_days: Decimal,
}

/// 开关值是否表示「关闭」（大小写不敏感；只认明确的假值，其余一律视为开启）。
///
/// # 参数
///
/// - `raw`：`sys_config.config_value` 原文。
///
/// # 为什么只认明确的假值
///
/// 这条开关保护的是「无薪假扣款有据可依」。读到一个看不懂的值（`''` / `'maybe'` /
/// 老客户手填的中文）时**按开启处理**，即宁可多拦一次让用户去补请假单，也不静默放过
/// 一笔没有依据的扣款 —— 与公告扇出开关「读不到就照常发」同一取舍方向。
pub fn switch_off(raw: &str) -> bool {
    matches!(raw.trim().to_ascii_lowercase().as_str(), "false" | "0" | "off" | "no")
}

/// 读「无薪假 ≤ 已批请假天数」开关（缺省 / 查不到 / 读库失败一律视为开启）。
///
/// SCOPE-EXEMPT: `sys_config` 是全局配置（不参与数据权限）。
pub async fn require_leave_match() -> bool {
    let rows = db()
        .find_with_params(CONFIG_SQL, vec![Value::Text(CONFIG_REQUIRE_LEAVE_MATCH.to_string())])
        .await;
    match rows {
        Ok(rows) => match rows.first() {
            Some(row) => {
                let raw: String = row.get_typed("config_value").unwrap_or_default();
                !switch_off(&raw)
            }
            None => true,
        },
        Err(error) => {
            tracing::warn!(%error, "读取 hr.attendance.require_leave_match 失败，按「开启」处理");
            true
        }
    }
}

/// 把待校验行拼成 `jsonb_to_recordset` 的载荷。
///
/// # 异常情况
///
/// JSON 序列化失败（理论上不会）→ [`DbError::InvalidValue`]（整批失败，不猜）。
pub fn payload(rows: &[LeaveMatchRow]) -> DbResult<String> {
    let items = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "row_no": row.row_no,
                "period_code": row.period_code,
                "emp_no": row.emp_no,
                "leave_unpaid_days": row.leave_unpaid_days,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&items)
        .map_err(|error| DbError::InvalidValue(format!("考勤无薪假校验载荷序列化失败：{error}")))
}

/// 批量查「无薪假 > 已批请假天数」的行（一条 SQL，返回 Excel 行号）。
///
/// # 参数
///
/// - `ex`：`db()` 或事务（导出 / 校验路径都只是读）；
/// - `rows`：已过逐行校验的行（只用到期间码 / 工号 / 无薪假）。
///
/// # 返回值
///
/// 命中的行，按 Excel 行号升序；空 = 全部合规。
///
/// # 异常情况
///
/// 数据库异常原样透传（`numeric` 文本读不回来也算异常：宁可整批失败，也不猜一个天数）。
pub async fn leave_mismatches(ex: &impl SqlExecutor, rows: &[LeaveMatchRow]) -> DbResult<Vec<LeaveMismatch>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let payload = payload(rows)?;
    let rows = ex
        .query_sql(
            LEAVE_MATCH_SQL.to_string(),
            vec![
                Value::Json(payload),
                Value::from(&SENTINEL_ORG_ID),
                Value::from(&SENTINEL_ORG_ID),
                Value::from(&SENTINEL_ORG_ID),
                Value::from(&SENTINEL_ORG_ID),
                Value::from(&SENTINEL_ORG_ID),
            ],
        )
        .await?;
    let mut result = Vec::with_capacity(rows.len());
    for row in &rows {
        let row_no: String = row.get_typed("row_no")?;
        let unpaid: String = row.get_typed("leave_unpaid_days")?;
        let approved: String = row.get_typed("approved_days")?;
        result.push(LeaveMismatch {
            row_no: row_no
                .trim()
                .parse::<u32>()
                .map_err(|error| DbError::InvalidValue(format!("无薪假校验返回的行号 `{row_no}` 不是整数：{error}")))?,
            period_code: row.get_typed("period_code")?,
            emp_no: row.get_typed("emp_no")?,
            unpaid_days: parse_days(&unpaid)?,
            approved_days: parse_days(&approved)?,
        });
    }
    Ok(result)
}

/// 面向用户的一句话（错误明细表 / 错误行 xlsx 的「原因」列）。
///
/// 三个数都要出现（填了多少 / 批了多少 / 多出多少），否则用户不知道是「假单没批」还是
/// 「考勤填错」：前者去补请假单，后者去改台账。
pub fn mismatch_message(mismatch: &LeaveMismatch) -> String {
    let gap = mismatch.unpaid_days - mismatch.approved_days;
    format!(
        "工号 {} 在考勤月 {} 填了无薪假 {} 天，但该员工当期已批准的无薪请假单只有 {} 天\
         （多出 {} 天）：请先补请假单并审批通过，或把无薪假天数改到 {} 天以内",
        mismatch.emp_no,
        mismatch.period_code,
        days_text(mismatch.unpaid_days),
        days_text(mismatch.approved_days),
        days_text(gap),
        days_text(mismatch.approved_days),
    )
}

/// 天数的展示形态（去尾零：`2.00` → `2`，`0.50` → `0.5`，`21.75` 保持）。
pub fn days_text(value: Decimal) -> String {
    value.normalize().to_string()
}

/// 文本 → 天数（`numeric` 走文本，与 `leave_model` 同口径）。
fn parse_days(raw: &str) -> DbResult<Decimal> {
    Decimal::from_str(raw.trim())
        .map_err(|error| DbError::InvalidValue(format!("请假天数 `{raw}` 不是合法小数：{error}")))
}

/// 汇总重算：把本批涉及的每个期间的四个汇总列全量覆盖。
///
/// # 参数
///
/// - `ex`：**必须**是导入事务（与记录写入同生共死，见模块文档）；
/// - `period_codes`：本批出现过的考勤月（去重由本函数自己做）；
/// - `operator`：写进 `update_by` 的操作人。
///
/// # 返回值
///
/// [`RecalcReport`]：真正重算的期间数 / 因锁定跳过数 / 查不到的期间码。
///
/// # 异常情况
///
/// - 期间**可重算却更新 0 行**（期间在「解析」与「更新」之间被并发锁定 / 软删）→
///   [`DbError::InvalidValue`]，报文点名考勤月。这一条是 **t_de6e8d41 的护栏**：旧实现
///   把 `recalc_totals` 的返回值丢掉，0 行更新被当成成功，于是「记录进了库、期间头还是
///   旧值」能一路静默到工资试算。宁可整批回滚让用户重导，也不留一张自相矛盾的期间头；
/// - 数据库异常原样透传 → 整批（记录 + 汇总）一起回滚。
///
/// # 为什么锁定期间跳过而不是报错
///
/// `LOCKED` = 定稿（设计 §4.2.3：工资按它算），期间头就该冻住。此时 `recalc_totals`
/// 的 `p.status <> 'LOCKED'` 守卫让更新影响 0 行，这是**设计意图**，不是异常，所以单独
/// 计数（[`RecalcReport::skipped_locked`]）而不是当成失败。
pub async fn recalc_periods(
    ex: &impl SqlExecutor,
    period_codes: &[String],
    operator: Option<Uuid>,
) -> DbResult<RecalcReport> {
    let codes = period_codes
        .iter()
        .map(|code| code.trim().to_string())
        .filter(|code| !code.is_empty())
        .collect::<BTreeSet<_>>();
    let mut report = RecalcReport::default();
    if codes.is_empty() {
        return Ok(report);
    }
    let payload =
        serde_json::to_string(&codes).map_err(|error| DbError::InvalidValue(format!("期间码序列化失败：{error}")))?;
    let targets = resolve_targets(ex, &payload).await?;
    // 文件里有、库里查不到的期间码（正常为空）
    let resolved = targets.iter().map(|t| t.period_code.as_str()).collect::<BTreeSet<_>>();
    report.missing = codes
        .iter()
        .filter(|code| !resolved.contains(code.as_str()))
        .cloned()
        .collect();
    for target in &targets {
        if target.status == STATUS_LOCKED {
            // 锁定 = 定稿：期间头不许再变（守卫在 recalc_totals 的 SQL 里，见函数文档）
            report.skipped_locked += 1;
            continue;
        }
        recalc_one(ex, target, operator).await?;
        report.recalced += 1;
    }
    Ok(report)
}

/// 重算**一个**期间，并确认它真的被更新了（`affected == 1`）。
///
/// # 参数
///
/// - `ex`：导入事务；
/// - `target`：已解析的期间（主键 + 期间码 + 状态）；
/// - `operator`：写进 `update_by` 的操作人。
///
/// # 异常情况
///
/// `UPDATE` 影响行数 ≠ 1（期间在「解析」与「更新」之间被并发锁定 / 软删）→
/// [`DbError::InvalidValue`]（[`stale_totals_message`]）。这是「记录进库、期间头不动」
/// 的唯一剩余入口，必须炸出来而不是记一句 warn。
///
/// # 为什么单独抽出来
///
/// 这个 0 行判断是**可被数据库单测直接命中**的：用例故意把「库里其实是 LOCKED 的期间」
/// 包成 `status = DRAFT` 的 target 传进来，就能稳定复现「解析时 DRAFT、更新时被守卫挡下」
/// 的时序，不需要开两个连接做真并发（那种用例会 flaky）。
async fn recalc_one(ex: &impl SqlExecutor, target: &RecalcTarget, operator: Option<Uuid>) -> DbResult<()> {
    // 唯一出口：期间汇总只走 attendance_period_model::recalc_totals
    // （它的 SQL 带 `status <> 'LOCKED'` 守卫，本函数不复制一份口径）
    let affected = attendance_period_model::recalc_totals(ex, target.id, operator).await?;
    if affected != 1 {
        return Err(DbError::InvalidValue(stale_totals_message(
            &target.period_code,
            affected,
        )));
    }
    Ok(())
}

/// 单期间重算 + 0 行护栏（只在 `cargo test` 里存在：真库用例借它直接命中「解析时
/// DRAFT、更新时被锁」的时序分支，业务代码请用 [`recalc_periods`]）。
#[cfg(test)]
pub(crate) async fn recalc_one_for_tests(
    ex: &impl SqlExecutor,
    target: &RecalcTarget,
    operator: Option<Uuid>,
) -> DbResult<()> {
    recalc_one(ex, target, operator).await
}

/// 解析期间码 → 重算目标（一条 SQL，无 N+1）。
///
/// # 异常情况
///
/// 主键不是 UUID / 状态读不回来 → [`DbError::InvalidValue`]（宁可整批失败，也不猜）。
async fn resolve_targets(ex: &impl SqlExecutor, payload: &str) -> DbResult<Vec<RecalcTarget>> {
    let rows = ex
        .query_sql(
            PERIOD_IDS_SQL.to_string(),
            vec![Value::Json(payload.to_string()), Value::from(&SENTINEL_ORG_ID)],
        )
        .await?;
    let mut targets = Vec::with_capacity(rows.len());
    for row in &rows {
        let id_text: String = row.get_typed("id")?;
        let id = Uuid::parse_str(id_text.trim())
            .map_err(|error| DbError::InvalidValue(format!("期间主键 `{id_text}` 不是 UUID：{error}")))?;
        targets.push(RecalcTarget {
            id,
            period_code: row.get_typed("period_code")?,
            status: row.get_typed("status")?,
        });
    }
    Ok(targets)
}

/// 期间头没能重算时的用户文案（点名考勤月 + 说清后果与下一步）。
///
/// # 参数
///
/// - `period_code`：考勤月（用户认的标识）；
/// - `affected`：`UPDATE` 实际影响的行数（正常情况下必须是 1）。
///
/// # 为什么要说「整批回滚」
///
/// 这句话由 `persist` 包装后返回给用户，用户看到的必须是**结果**（没导进去），
/// 而不是「部分成功」—— 否则他会以为记录进去了。
pub fn stale_totals_message(period_code: &str, affected: u64) -> String {
    format!(
        "考勤月「{period_code}」的期间汇总没能重算（更新影响 {affected} 行，正常应是 1 行）：\
         该期间可能在导入事务进行中被锁定或删除。本次导入整批回滚，考勤记录与期间汇总都未落库；\
         请刷新期间列表后重试（若该月已锁定，先解锁再导入）"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mismatch(unpaid: &str, approved: &str) -> LeaveMismatch {
        LeaveMismatch {
            row_no: 7,
            period_code: "2026-11".to_string(),
            emp_no: "EMP-0007".to_string(),
            unpaid_days: Decimal::from_str(unpaid).expect("无薪假天数"),
            approved_days: Decimal::from_str(approved).expect("已批天数"),
        }
    }

    #[test]
    fn switch_only_recognises_explicit_false_forms() {
        for raw in ["FALSE", "false", " 0 ", "off", "No", " OFF "] {
            assert!(switch_off(raw), "`{raw}` 应判为关闭");
        }
        for raw in ["TRUE", "true", "1", "on", "yes", "", "maybe", "关闭"] {
            assert!(!switch_off(raw), "`{raw}` 应判为开启（看不懂就按开启处理）");
        }
    }

    #[test]
    fn message_names_employee_period_and_the_gap() {
        let text = mismatch_message(&mismatch("3", "0.5"));
        assert!(text.contains("EMP-0007"), "要点名工号：{text}");
        assert!(text.contains("2026-11"), "要点名考勤月：{text}");
        assert!(text.contains("无薪假 3 天"), "要给出文件里的天数：{text}");
        assert!(text.contains("只有 0.5 天"), "要给出已批天数：{text}");
        assert!(text.contains("多出 2.5 天"), "要给出差额：{text}");
        assert!(text.contains("0.5 天以内"), "要给出可操作的上界：{text}");
    }

    #[test]
    fn days_text_drops_trailing_zeros_but_keeps_real_precision() {
        assert_eq!(days_text(Decimal::from_str("2.00").unwrap()), "2");
        assert_eq!(days_text(Decimal::from_str("0.50").unwrap()), "0.5");
        assert_eq!(days_text(Decimal::from_str("21.75").unwrap()), "21.75");
        assert_eq!(days_text(Decimal::from_str("0").unwrap()), "0");
    }

    #[test]
    fn payload_keeps_excel_row_no_and_blank_days() {
        let rows = vec![
            LeaveMatchRow {
                row_no: 2,
                period_code: "2026-11".to_string(),
                emp_no: "EMP-0001".to_string(),
                leave_unpaid_days: Some("2".to_string()),
            },
            LeaveMatchRow {
                row_no: 3,
                period_code: "2026-11".to_string(),
                emp_no: "EMP-0002".to_string(),
                leave_unpaid_days: None,
            },
        ];
        let text = payload(&rows).expect("载荷");
        let value: serde_json::Value = serde_json::from_str(&text).expect("合法 JSON");
        assert_eq!(value[0]["row_no"], 2, "行号必须原样带给 SQL（报错定位靠它）");
        assert_eq!(value[1]["leave_unpaid_days"], serde_json::Value::Null);
        assert_eq!(value[1]["emp_no"], "EMP-0002");
    }

    #[test]
    fn leave_match_sql_is_cst_date_based_and_only_counts_approved_unpaid() {
        // 口径三条：只算 APPROVED / 只算 paid = 0 / 按北京时间自然日判重叠
        assert!(LEAVE_MATCH_SQL.contains("l.status = 'APPROVED'"));
        assert!(LEAVE_MATCH_SQL.contains("l.paid = 0"));
        assert!(
            LEAVE_MATCH_SQL.contains("AT TIME ZONE 'Asia/Shanghai'"),
            "重叠判定必须按北京时间自然日（timestamptz 直接比会跨时区多算一天）"
        );
        assert!(
            LEAVE_MATCH_SQL.contains("l.duration_days"),
            "已批天数 = 请假单的 duration_days 合计（0.5 天粒度）"
        );
        // 软删与组织维度：三张表（员工 / 期间 / 请假）都要带，漏一个就会跨组织 /
        // 读到已删的假单（4 处 `deleted_at IS NULL` 在子查询里 + 1 处在外层员工 join）
        assert_eq!(LEAVE_MATCH_SQL.matches("deleted_at IS NULL").count(), 5);
        assert_eq!(LEAVE_MATCH_SQL.matches("org_id = ?::uuid").count(), 5);
        // 比较直接落在 SQL 里（Rust 侧不重算阈值，避免两套口径）
        assert!(LEAVE_MATCH_SQL.contains("> COALESCE(agg.approved_days, 0)"));
        // 参数个数与「6 个绑定参数」的文档一致
        assert_eq!(LEAVE_MATCH_SQL.matches('?').count(), 6);
    }

    #[test]
    fn period_ids_sql_scopes_by_org_and_carries_status_for_the_locked_branch() {
        assert!(PERIOD_IDS_SQL.contains("jsonb_array_elements_text"));
        assert!(PERIOD_IDS_SQL.contains("deleted_at IS NULL"));
        assert!(PERIOD_IDS_SQL.contains("org_id = ?::uuid"));
        // 三个列缺一不可：id 用于更新、period_code 用于报错点名、status 用于区分「锁定跳过」
        assert!(
            PERIOD_IDS_SQL.contains("p.id::text AS id"),
            "主键要转文本（数值/二进制读不回来）"
        );
        assert!(PERIOD_IDS_SQL.contains("p.period_code"), "报错要点名考勤月，不是 UUID");
        assert!(
            PERIOD_IDS_SQL.contains("p.status"),
            "锁定期间要能识别出来（跳过而不是报错）"
        );
        assert_eq!(PERIOD_IDS_SQL.matches('?').count(), 2);
    }

    #[test]
    fn locked_status_matches_the_page_contract() {
        // 本模块写字面量 `LOCKED` 而不 import DTO 常量（只判断能不能重算，不参与状态机），
        // 这个对拍就是「不许哪天改了码值两边分叉」的护栏。
        assert_eq!(STATUS_LOCKED, crate::business::hr::attendance_period_dto::STATUS_LOCKED);
        // 未锁定的两个状态都不许被当成「跳过」
        for status in [
            crate::business::hr::attendance_period_dto::STATUS_DRAFT,
            crate::business::hr::attendance_period_dto::STATUS_SUBMITTED,
        ] {
            assert_ne!(status, STATUS_LOCKED, "只有 LOCKED 才冻结期间头");
        }
    }

    #[test]
    fn stale_totals_message_names_period_and_consequence() {
        let text = stale_totals_message("2026-11", 0);
        assert!(text.contains("2026-11"), "要点名考勤月：{text}");
        assert!(text.contains("影响 0 行"), "要给出实际影响行数：{text}");
        assert!(text.contains("整批回滚"), "要说清结果是「没导进去」：{text}");
        assert!(text.contains("解锁"), "要给出下一步（锁定就解锁）：{text}");
        assert!(!text.contains("部分成功"), "不许让用户以为记录进去了：{text}");
    }

    #[test]
    fn recalc_report_starts_empty_so_missing_stays_a_vec() {
        let report = RecalcReport::default();
        assert_eq!(report.recalced, 0);
        assert_eq!(report.skipped_locked, 0);
        assert!(report.missing.is_empty());
    }
}
