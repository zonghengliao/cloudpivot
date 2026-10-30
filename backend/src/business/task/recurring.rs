//! 周期任务（批次 E · **E11** 二、周期任务）：cron 解析 + 到点扫描 + 生成任务。
//!
//! 依据：卡面 `t_834dcc63`「二、周期任务」——
//! 表加 `cron_expr` / `enabled` / `next_run_at` / `last_run_at`（迁移 `V0135`），
//! 调度复用 `Scheduler::register_builtin`（每 5 分钟扫一次 `next_run_at <= now()`），
//! 生成的任务 `source='RECURRING'`，**标准 cron 即可**（不搞自然语言）。
//!
//! # 三条硬约束
//!
//! 1. **不引入 cron 第三方库**：卡面写「标准 cron 即可」，而已有的框架调度器
//!    只吃「cron 字符串 + 闭包」，不会把「下次触发时间」算给业务侧。为了算
//!    `next_run_at` 去加一个 crate 属于「新增第三方依赖须评审」（`cargo` 红线 1），
//!    本模块自带一个小而全的 5 段 cron 解析器（支持 `*` / `a` / `a-b` / `*/n` /
//!    `a-b/n` / 逗号列表），并带 12 条纯逻辑单测。
//! 2. **同一次排期只生成一次（多实例安全）**：先用 CAS 抢占（`WHERE next_run_at = 旧值`），
//!    抢到才建任务；建任务失败再把 `next_run_at` 还原。两个实例同时扫到同一行时，
//!    只有一个的 `UPDATE … WHERE next_run_at = ?` 会命中 1 行。
//! 3. **生成逻辑与「一键建任务」共用同一份实现**：
//!    [`super::template::create_from_template`] —— 两边只有 `source` 不同
//!    （手动 `SELF`，周期 `RECURRING`），避免「模板建出来的任务」两套字段口径。
//!
//! # 时区
//!
//! `cron_expr` 一律按 **CST（UTC+8）** 解释（老大在北京：「每天 9 点」是北京时间 9 点）。
//! 库里 `next_run_at` / `last_run_at` 存 `TIMESTAMPTZ`（UTC 瞬时），换算只在本模块内做。
//!
//! # 停用语义
//!
//! `enabled = false` 时扫描 SQL 直接过滤掉（`WHERE enabled`），**且编辑接口会把
//! `next_run_at` 清空**（见 [`super::template::update`]）—— 双重保险：
//! 即使有人手工把 `enabled` 改回 true，没有 `next_run_at` 也不会立刻补跑一堆历史排期。

// SCOPE-EXEMPT: 周期调度是**后台扫描**（无登录态）：它必须看到所有启用中的模板，
// 否则「A 建的周期模板」在 B 的数据范围内就跑不起来。写操作只有调度器自己的 CAS 排期推进，
// 不对外暴露列表接口；模板 CRUD 的可见性口径见 `business/task/template.rs` 的豁免说明。

use nik::prelude::chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::task::template::{self, TaskFromTemplateReq, TemplateRow};

// ==================== 常量 ====================

/// 调度任务名（同时用于日志与 [`crate::task::Scheduler::trigger_registered`]）。
pub const TASK_NAME: &str = "business_task_recurring";

/// 调度 cron（6 段：**秒** 分 时 日 月 周）—— 每 5 分钟扫一次。
pub const CRON: &str = "0 */5 * * * *";

/// 单次扫描最多处理的模板数（防止一次积压把调度线程拖长）。
pub const SCAN_LIMIT: i64 = 50;

/// 向前搜索下一次触发的天数上限（约 5 年；`2 月 29 日` 这类表达式最多等 4 年）。
const MAX_SEARCH_DAYS: i64 = 1866;

/// CST 相对 UTC 的偏移（小时）。
const CST_OFFSET_HOURS: i64 = 8;

// ==================== cron 解析 ====================

/// 5 段标准 cron 的解析结果（字段已展开成取值集合，`dow` 里 `7` 已折成 `0`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSpec {
    /// 分钟集合（0–59）。
    pub minutes: Vec<u8>,
    /// 小时集合（0–23）。
    pub hours: Vec<u8>,
    /// 日集合（1–31）。
    pub days: Vec<u8>,
    /// 月集合（1–12）。
    pub months: Vec<u8>,
    /// 星期集合（0–6，0 = 周日）。
    pub weekdays: Vec<u8>,
    /// 日字段是否为 `*`（cron 的「日 / 星期」二选一口径需要它）。
    pub day_star: bool,
    /// 星期字段是否为 `*`。
    pub weekday_star: bool,
}

/// 解析标准 cron（5 段；6 段时首段是「秒」，**被忽略**——本卡只要分钟级精度）。
///
/// # 支持语法
///
/// `*`、`a`、`a-b`、`*/n`、`a-b/n`、逗号列表（如 `1,15,30`）；
/// 月份支持 `JAN`–`DEC`，星期支持 `SUN`–`SAT`（大小写不敏感）。
///
/// # 异常情况
///
/// 段数不是 5 / 6、取值越界、区间倒置、步长为 0、出现无法识别的字符 → `Err(中文说明)`。
pub fn parse_cron(expr: &str) -> std::result::Result<CronSpec, String> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    let cron_fields = match fields.len() {
        5 => fields,
        6 => fields[1..].to_vec(),
        other => {
            return Err(format!(
                "应为 5 段（分 时 日 月 周）或 6 段（秒 分 时 日 月 周），实际 {other} 段"
            ));
        }
    };
    let minutes = parse_field(cron_fields[0], 0, 59, &[])?;
    let hours = parse_field(cron_fields[1], 0, 23, &[])?;
    let days = parse_field(cron_fields[2], 1, 31, &[])?;
    let months = parse_field(
        cron_fields[3],
        1,
        12,
        &[
            ("JAN", 1),
            ("FEB", 2),
            ("MAR", 3),
            ("APR", 4),
            ("MAY", 5),
            ("JUN", 6),
            ("JUL", 7),
            ("AUG", 8),
            ("SEP", 9),
            ("OCT", 10),
            ("NOV", 11),
            ("DEC", 12),
        ],
    )?;
    let raw_weekdays = parse_field(
        cron_fields[4],
        0,
        7,
        &[
            ("SUN", 0),
            ("MON", 1),
            ("TUE", 2),
            ("WED", 3),
            ("THU", 4),
            ("FRI", 5),
            ("SAT", 6),
        ],
    )?;
    let mut weekdays: Vec<u8> = raw_weekdays
        .into_iter()
        .map(|day| if day == 7 { 0 } else { day })
        .collect();
    weekdays.sort_unstable();
    weekdays.dedup();
    Ok(CronSpec {
        minutes,
        hours,
        days,
        months,
        weekdays,
        day_star: cron_fields[2].trim() == "*",
        weekday_star: cron_fields[4].trim() == "*",
    })
}

/// 解析单个 cron 字段 → 升序去重后的取值集合。
fn parse_field(
    raw: &str,
    min: u8,
    max: u8,
    aliases: &[(&str, u8)],
) -> std::result::Result<Vec<u8>, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("字段为空".to_string());
    }
    let mut out: Vec<u8> = Vec::new();
    for part in text.split(',') {
        let piece = part.trim();
        if piece.is_empty() {
            return Err(format!("字段 `{text}` 里有空项"));
        }
        out.extend(parse_range(piece, min, max, aliases)?);
    }
    out.sort_unstable();
    out.dedup();
    if out.is_empty() {
        return Err(format!("字段 `{text}` 展开后没有任何取值"));
    }
    Ok(out)
}

/// 解析单段（`*` / `a` / `a-b` / `*/n` / `a-b/n`）。
fn parse_range(
    piece: &str,
    min: u8,
    max: u8,
    aliases: &[(&str, u8)],
) -> std::result::Result<Vec<u8>, String> {
    let (base, step) = match piece.split_once('/') {
        Some((base, raw_step)) => {
            let step: u8 = raw_step
                .trim()
                .parse()
                .map_err(|_| format!("步长不是数字：`{raw_step}`"))?;
            if step == 0 {
                return Err("步长不能为 0".to_string());
            }
            (base.trim(), step)
        }
        None => (piece, 1),
    };
    let (from, to) = if base == "*" || base.is_empty() {
        (min, max)
    } else if let Some((left, right)) = base.split_once('-') {
        let from = parse_value(left, aliases)?;
        let to = parse_value(right, aliases)?;
        if from > to {
            return Err(format!("区间倒置：{from}-{to}"));
        }
        (from, to)
    } else {
        let value = parse_value(base, aliases)?;
        // `a/n` 的语义（vixie cron）= 从 a 到上限按步长取
        if step > 1 {
            (value, max)
        } else {
            (value, value)
        }
    };
    if from < min || to > max {
        return Err(format!("取值越界：{from}-{to}（允许 {min}-{max}）"));
    }
    let mut out = Vec::new();
    let mut value = from;
    while value <= to {
        out.push(value);
        match value.checked_add(step) {
            Some(next) => value = next,
            None => break,
        }
    }
    Ok(out)
}

/// 解析单个取值（数字或月份 / 星期别名）。
fn parse_value(raw: &str, aliases: &[(&str, u8)]) -> std::result::Result<u8, String> {
    let text = raw.trim();
    if let Some((_, value)) = aliases
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(text))
    {
        return Ok(*value);
    }
    text.parse::<u8>()
        .map_err(|_| format!("无法识别的取值：`{text}`"))
}

impl CronSpec {
    /// 判断某个 CST 日期是否命中「日 / 月 / 星期」三个字段。
    ///
    /// cron 的经典口径：`日` 与 `星期` **都**被限定时是「或」，只要其中一个为 `*` 就是「与」。
    pub fn matches_date(&self, date: NaiveDate) -> bool {
        if !self.months.contains(&(date.month() as u8)) {
            return false;
        }
        // chrono: 周一 = 1 … 周日 = 7 → cron: 周日 = 0 … 周六 = 6
        let weekday = (date.weekday().num_days_from_sunday() as u8).min(6);
        let day_ok = self.days.contains(&(date.day() as u8));
        let week_ok = self.weekdays.contains(&weekday);
        match (self.day_star, self.weekday_star) {
            (true, true) => true,
            (true, false) => week_ok,
            (false, true) => day_ok,
            (false, false) => day_ok || week_ok,
        }
    }

    /// 一天里的候选时刻（`时 * 60 + 分`，升序）。
    pub fn slots(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.hours.len() * self.minutes.len());
        for hour in &self.hours {
            for minute in &self.minutes {
                out.push(*hour as u32 * 60 + *minute as u32);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// 计算「严格晚于 `after`」的下一次触发时间（UTC）。
///
/// # 参数
///
/// - `expr`：标准 cron（5 段 / 6 段）；
/// - `after`：基准时刻（UTC）。
///
/// # 返回值
///
/// 下次触发时刻（UTC）；表达式非法或 [`MAX_SEARCH_DAYS`] 内找不到 → `None`。
///
/// # 口径
///
/// - 按 **CST** 解释 cron 字段；
/// - **严格大于** `after`（同一分钟不重复触发）；
/// - 秒级精度恒为 0（卡面只要分钟级）。
pub fn next_run_after(expr: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let spec = parse_cron(expr).ok()?;
    let local = after + Duration::hours(CST_OFFSET_HOURS);
    let start_date = local.date_naive();
    let start_slot = local.hour() * 60 + local.minute();
    let slots = spec.slots();
    if slots.is_empty() {
        return None;
    }
    let mut date = start_date;
    for _ in 0..MAX_SEARCH_DAYS {
        if spec.matches_date(date) {
            for slot in &slots {
                if date == start_date && *slot <= start_slot {
                    continue;
                }
                let naive = date.and_hms_opt(slot / 60, slot % 60, 0)?;
                let utc = DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc);
                return Some(utc - Duration::hours(CST_OFFSET_HOURS));
            }
        }
        date = date.succ_opt()?;
    }
    None
}

// ==================== 调度 ====================

/// 注册到框架调度器（由 [`crate::task::Scheduler::register_builtin`] 调用）。
///
/// # 参数
///
/// - `scheduler`：启动链里的调度器句柄。
///
/// # 说明
///
/// 注册点必须在 `register_builtin` 里（放 `bootstrap.rs` 会被随后的 `reload`
/// 清掉 —— 这是批次 A 踩过的坑，见 `business/notice/cleanup.rs` 的注释）。
pub fn register_builtin(scheduler: &crate::task::Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        CRON,
        "业务任务：周期任务生成（每 5 分钟扫一次到期模板）",
        || {
            tokio::spawn(async move {
                match run_once().await {
                    Ok(report) => {
                        if report.generated.is_empty() && report.failed.is_empty() {
                            tracing::debug!(
                                scanned = report.scanned,
                                "周期任务扫描完成（无需生成）"
                            );
                        } else {
                            tracing::info!(
                                scanned = report.scanned,
                                generated = report.generated.len(),
                                failed = report.failed.len(),
                                "周期任务扫描完成"
                            );
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "周期任务扫描失败（下一次 5 分钟后再试）")
                    }
                }
            })
        },
    );
    tracing::info!(
        invoke_target = TASK_NAME,
        cron = CRON,
        "周期任务生成任务已注册（E11）"
    );
}

/// 单次扫描的产物（供日志与验收脚本使用）。
#[derive(Debug, Clone, Default)]
pub struct RecurringReport {
    /// 本轮扫到的到期模板数。
    pub scanned: usize,
    /// 成功生成的任务。
    pub generated: Vec<GeneratedTask>,
    /// 生成失败的模板（`模板 id: 原因`）。
    pub failed: Vec<String>,
}

/// 一次成功生成。
#[derive(Debug, Clone)]
pub struct GeneratedTask {
    /// 模板 id。
    pub template_id: Uuid,
    /// 模板名。
    pub template_name: String,
    /// 生成的任务 id。
    pub task_id: Uuid,
    /// 生成的任务编号。
    pub task_no: String,
    /// 推进后的下次生成时间。
    pub next_run_at: Option<DateTime<Utc>>,
}

/// 扫一轮到期模板并生成任务（**幂等：同一排期只生成一次**）。
///
/// # 返回值
///
/// [`RecurringReport`]；单个模板失败只进 `failed`，不中断其它模板。
///
/// # 异常情况
///
/// 查询到期模板本身失败 → [`DbError`]（调度器记 error 日志，下一轮重试）。
pub async fn run_once() -> DbResult<RecurringReport> {
    let due = due_templates().await?;
    let mut report = RecurringReport {
        scanned: due.len(),
        ..Default::default()
    };
    for tpl in &due {
        let Some(cron_expr) = tpl.cron_expr.clone() else {
            continue;
        };
        let Some(claimed_from) = tpl.next_run_at else {
            continue;
        };
        let base = Utc::now();
        let next = next_run_after(&cron_expr, base);
        // ① CAS 抢占：只有把 next_run_at 从旧值推到新值的那一个实例才建任务
        let claimed = claim_slot(tpl.id, claimed_from, next).await?;
        if !claimed {
            tracing::debug!(template_id = %tpl.id, "周期模板已被其它实例领取，跳过");
            continue;
        }
        // ② 生成任务（失败要把 next_run_at 还原，否则这一轮就永久丢了）
        let actor = match tpl.create_by.or(tpl.owner_id) {
            Some(user) => user,
            None => {
                let message = format!("{}: 模板没有创建人 / 负责人，无法确定操作人", tpl.id);
                report.failed.push(message.clone());
                restore_slot(tpl.id, claimed_from, next).await?;
                tracing::error!(template_id = %tpl.id, %message, "周期任务生成失败");
                continue;
            }
        };
        match template::create_from_template(
            tpl,
            actor,
            &TaskFromTemplateReq::default(),
            template::SOURCE_RECURRING,
        )
        .await
        {
            Ok(created) => {
                let task_id = Uuid::parse_str(&created.id).unwrap_or_else(|_| Uuid::nil());
                tracing::info!(
                    template_id = %tpl.id,
                    template_name = %tpl.name,
                    task_id = %created.id,
                    task_no = %created.task_no,
                    subtasks = created.subtask_ids.len(),
                    next_run_at = ?next,
                    "周期任务已生成"
                );
                report.generated.push(GeneratedTask {
                    template_id: tpl.id,
                    template_name: tpl.name.clone(),
                    task_id,
                    task_no: created.task_no,
                    next_run_at: next,
                });
            }
            Err(error) => {
                let message = format!("{}: {error}", tpl.id);
                report.failed.push(message.clone());
                restore_slot(tpl.id, claimed_from, next).await?;
                tracing::error!(template_id = %tpl.id, %error, "周期任务生成失败，排期已还原");
            }
        }
    }
    Ok(report)
}

/// 取所有到期的周期模板（`enabled` + 已排期 + `next_run_at <= now()`）。
async fn due_templates() -> DbResult<Vec<TemplateRow>> {
    let sql = format!(
        "SELECT {} FROM biz_task_template \
          WHERE deleted_at IS NULL AND enabled \
            AND cron_expr IS NOT NULL AND cron_expr <> '' \
            AND next_run_at IS NOT NULL AND next_run_at <= now() \
          ORDER BY next_run_at ASC LIMIT ?",
        template::TEMPLATE_COLUMNS
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::I64(SCAN_LIMIT)])
        .await?;
    rows.iter().map(TemplateRow::from_row).collect()
}

/// CAS 抢占排期：`next_run_at` 必须仍等于扫描时读到的旧值。
///
/// # 返回值
///
/// `true` = 本轮由我领取；`false` = 已被别的实例推进。
async fn claim_slot(id: Uuid, from: DateTime<Utc>, to: Option<DateTime<Utc>>) -> DbResult<bool> {
    let affected = db()
        .execute_with_params(
            "UPDATE biz_task_template \
                SET last_run_at = now(), next_run_at = ?::timestamptz, updated_at = now() \
              WHERE id = ?::uuid AND deleted_at IS NULL AND enabled \
                AND next_run_at = ?::timestamptz",
            vec![Value::from(to), Value::from(&id), Value::from(from)],
        )
        .await?;
    Ok(affected == 1)
}

/// 生成失败时把排期还原（保持 `last_run_at` 不变，避免误报「跑过了」）。
async fn restore_slot(
    id: Uuid,
    from: DateTime<Utc>,
    attempted: Option<DateTime<Utc>>,
) -> DbResult<()> {
    db().execute_with_params(
        "UPDATE biz_task_template \
            SET next_run_at = ?::timestamptz, updated_at = now() \
          WHERE id = ?::uuid AND next_run_at IS NOT DISTINCT FROM ?::timestamptz",
        vec![Value::from(from), Value::from(&id), Value::from(attempted)],
    )
    .await?;
    Ok(())
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 北京时间 `2026-10-30 08:00:00` 对应的 UTC（跨时区换算的基准）。
    fn utc_of_cst(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        let naive = NaiveDate::from_ymd_opt(year, month, day)
            .and_then(|date| date.and_hms_opt(hour, minute, 0))
            .expect("测试用日期必须合法");
        DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc) - Duration::hours(CST_OFFSET_HOURS)
    }

    /// 5 段 / 6 段都能解析；6 段的首段（秒）被忽略。
    #[test]
    fn parse_cron_accepts_five_and_six_fields() {
        let five = parse_cron("0 9 * * *").expect("5 段应解析成功");
        assert_eq!(five.hours, vec![9]);
        assert_eq!(five.minutes, vec![0]);
        assert!(five.day_star && five.weekday_star);

        let six = parse_cron("30 0 9 * * *").expect("6 段应解析成功");
        assert_eq!(six.hours, vec![9], "6 段的第一个字段是秒，必须忽略");
        assert_eq!(six.minutes, vec![0]);
    }

    /// 复杂语法：区间 / 步长 / 列表 / 别名 / 星期 7 折成 0。
    #[test]
    fn parse_cron_expands_ranges_steps_lists() {
        let spec = parse_cron("*/15 8-10 1,15 * MON-FRI").expect("复杂表达式应解析成功");
        assert_eq!(spec.minutes, vec![0, 15, 30, 45]);
        assert_eq!(spec.hours, vec![8, 9, 10]);
        assert_eq!(spec.days, vec![1, 15]);
        assert_eq!(spec.weekdays, vec![1, 2, 3, 4, 5]);
        assert!(!spec.day_star && !spec.weekday_star);

        let sunday = parse_cron("0 0 * * 7").expect("星期 7 = 周日");
        assert_eq!(sunday.weekdays, vec![0]);
    }

    /// 非法表达式一律拒绝（段数 / 越界 / 倒置 / 步长 0 / 非数字）。
    #[test]
    fn parse_cron_rejects_bad_expressions() {
        for bad in [
            "每天九点",
            "* * * *",
            "0 25 * * *",
            "0 9 32 * *",
            "0 9 * 13 *",
            "0 9 * * 8",
            "10-5 * * * *",
            "*/0 * * * *",
            "a * * * *",
            "0 9 * * * * *",
        ] {
            assert!(parse_cron(bad).is_err(), "非法表达式必须被拒绝：{bad}");
        }
    }

    /// 每天 9 点：同一分钟的基准不重复触发，严格取下一秒的 9 点。
    #[test]
    fn next_run_daily_nine() {
        let after = utc_of_cst(2026, 10, 30, 8, 0);
        let next = next_run_after("0 9 * * *", after).expect("应算出下次触发");
        assert_eq!(next, utc_of_cst(2026, 10, 30, 9, 0));

        // 已经过了 9:00 → 顺延到次日 9:00
        let after_past = utc_of_cst(2026, 10, 30, 9, 0);
        let next_day = next_run_after("0 9 * * *", after_past).expect("应算出次日触发");
        assert_eq!(next_day, utc_of_cst(2026, 10, 31, 9, 0));
    }

    /// 每周一 9 点 / 每月 1 号 9 点（卡面前端三个示例口径）。
    #[test]
    fn next_run_weekly_and_monthly() {
        // 2026-10-30 是周五 → 下一个周一 = 2026-11-02
        let after = utc_of_cst(2026, 10, 30, 12, 0);
        let weekly = next_run_after("0 9 * * 1", after).expect("每周一应能算出");
        assert_eq!(weekly, utc_of_cst(2026, 11, 2, 9, 0));

        let monthly = next_run_after("0 9 1 * *", after).expect("每月 1 号应能算出");
        assert_eq!(monthly, utc_of_cst(2026, 11, 1, 9, 0));
    }

    /// 「日 / 星期」交叉口径：两个都限定时是「或」。
    #[test]
    fn day_and_weekday_follow_cron_semantics() {
        let spec = parse_cron("0 9 1 * 1").expect("合法表达式");
        let monday = NaiveDate::from_ymd_opt(2026, 11, 2).expect("周一");
        let first = NaiveDate::from_ymd_opt(2026, 11, 1).expect("1 号（周日）");
        let tuesday = NaiveDate::from_ymd_opt(2026, 11, 3).expect("周二");
        assert!(spec.matches_date(monday), "周一命中（星期字段）");
        assert!(spec.matches_date(first), "1 号命中（日字段）");
        assert!(!spec.matches_date(tuesday), "既非 1 号也非周一");
    }

    /// 无法命中的组合（2 月 30 日）→ `None`，不无限循环。
    #[test]
    fn next_run_impossible_returns_none() {
        let after = utc_of_cst(2026, 1, 1, 0, 0);
        assert!(next_run_after("0 9 30 2 *", after).is_none());
    }
}
