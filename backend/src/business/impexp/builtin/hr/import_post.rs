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

/// 按期间码取期间主键（汇总重算前解析，一条往返）。
const PERIOD_IDS_SQL: &str = "\
SELECT p.id::text AS id \
  FROM jsonb_array_elements_text(?::jsonb) AS c(period_code) \
  JOIN biz_attendance_period p \
    ON p.period_code = c.period_code AND p.org_id = ?::uuid AND p.deleted_at IS NULL";

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
/// 实际重算的期间数（`0` = 一个期间都没匹配上，正常不会发生 —— 入库 SQL 已经 JOIN 过
/// 期间表；真发生说明并发软删了期间，由入库的行数对账兜住）。
///
/// # 异常情况
///
/// 数据库异常原样透传 → 整批（记录 + 汇总）一起回滚。
pub async fn recalc_periods(ex: &impl SqlExecutor, period_codes: &[String], operator: Option<Uuid>) -> DbResult<usize> {
    let codes = period_codes
        .iter()
        .map(|code| code.trim().to_string())
        .filter(|code| !code.is_empty())
        .collect::<BTreeSet<_>>();
    if codes.is_empty() {
        return Ok(0);
    }
    let payload =
        serde_json::to_string(&codes).map_err(|error| DbError::InvalidValue(format!("期间码序列化失败：{error}")))?;
    let rows = ex
        .query_sql(
            PERIOD_IDS_SQL.to_string(),
            vec![Value::Json(payload), Value::from(&SENTINEL_ORG_ID)],
        )
        .await?;
    let mut recalced = 0usize;
    for row in &rows {
        let id_text: String = row.get_typed("id")?;
        let id = Uuid::parse_str(id_text.trim())
            .map_err(|error| DbError::InvalidValue(format!("期间主键 `{id_text}` 不是 UUID：{error}")))?;
        // 唯一出口：期间汇总只走 attendance_period_model::recalc_totals
        // （它的 SQL 带 `status <> 'LOCKED'` 守卫，本函数不复制一份口径）
        attendance_period_model::recalc_totals(ex, id, operator).await?;
        recalced += 1;
    }
    Ok(recalced)
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
    fn period_ids_sql_scopes_by_org_and_dedupes_nothing() {
        assert!(PERIOD_IDS_SQL.contains("jsonb_array_elements_text"));
        assert!(PERIOD_IDS_SQL.contains("deleted_at IS NULL"));
        assert!(PERIOD_IDS_SQL.contains("org_id = ?::uuid"));
        assert_eq!(PERIOD_IDS_SQL.matches('?').count(), 2);
    }
}
