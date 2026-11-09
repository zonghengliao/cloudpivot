//! 自审（`self_approval = ALLOW`）的配套约束：**金额上限 + 审计留痕 + 月度报表**。
//!
//! 依据：ARCH-8 §8 待确认问题 2（P3-05 卡）——「受控的例外必须可审计」。
//! `ALLOW` 让发起人本人能签自己发起的单（一人公司刚需），但**没有配套约束的例外
//! 等于「谁发起谁自己批」的合法后门**。本文件给三层配套：
//!
//! | 层 | 落点 | 回答什么问题 |
//! |----|------|--------------|
//! | ① 金额上限 | [`crate::business::erp::separation::self_approval_max_amount`] + [`demote_over_limit`] | 大额单据还能不能自审（默认不限额） |
//! | ② 审计留痕 | `operation_logs`（[`record`] / 三个 `OP_*` 操作码） | 谁、何时、哪张单、哪个节点、多少钱、为什么允许 |
//! | ③ 月度报表 | [`monthly_report`]（`GET /business/flow/stats/self-approval`） | 本月自审占比多少、谁在自审、多少被阈值拦下 |
//!
//! # 三个决策（本文件是唯一出口，改口径改这里）
//!
//! 1. **金额只从 `biz_flow_instance.form_data` 取**：先 `amount`，取不到再 `total_amount`，
//!    都没有 = 0（不拦）并记 `debug`。**不查业务单据表** —— 那会把流程引擎和 N 张
//!    业务表耦死（每接一个 `biz_type` 都得在这里加一个 JOIN）。
//! 2. **超限 = 降级成 SKIP（先走 P3-02 升级链 / P3-03 兜底人），仍无人可用才 422 + 65044**。
//!    不选「一超限就 422」：一人公司里发起人就是唯一审批人，直接 422 等于把单子彻底堵死，
//!    而升级链 / 兜底人本来就是为了这种场景准备的。也**不选「静默自动通过」**：那等于
//!    大额单据没人签就生效，比自审更坏。
//! 3. **拦截计数不建表**：拦下的每一次都落在 `operation_logs`
//!    （`OP_BLOCKED` / `target_id = 实例 id`），报表直接**数日志**。
//!    理由：多一张计数表就多一份「与真实留痕不一致」的可能；日志是同一事务里写的，
//!    天然与业务动作同真同假。⇒ **本卡无迁移里的一次性计数表**（见 `docs/evidence`）。
//!
//! # 为什么写进 `operation_logs` 而不是走日志总线
//!
//! 总线（`support::log::bus`）是「尽力而为的旁路数据」，进程重启会丢；而「他被允许自审」/
//! 「他真的自审了」是**合规留痕**，必须与这一票同真同假。因此这里用**引擎事务内的
//! 同步 INSERT**（失败即整单回滚），而不是 `enqueue_operation`。
//!
//! 代价：审计写失败会让审批动作失败。这是刻意的取舍 —— 审计不可用时的自审动作
//! 本来就不该被允许（与「日志失败绝不波及业务」的旁路日志是两种数据、两种口径）。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/support/V0011__create_operation_logs.up.sql`（`\d operation_logs` 实测）：
//! `request_id` 是 `NOT NULL`、`created_at` 无默认值（追加写表），因此这里显式生成
//! `request_id`（UUID v7）并显式写 `now()`。

// SCOPE-EXEMPT: 机制登记（**不是**放弃数据权限）—— 本文件的所有实例 / 待办查询都先把
// SCOPE-EXEMPT: `super::flow_model_inbox::instance_visibility(principal)` 生成的片段拼进 `vis` CTE
// SCOPE-EXEMPT: （内部用 `scope::filter_for(INSTANCE_TARGET)` + 「我发起的 ∪ 我待办/已办」，与实例列表、
// SCOPE-EXEMPT: 其余三个统计接口同一口径，架构 D16）。报表的四个查询都以 `vis` 为主体；`operation_logs`
// SCOPE-EXEMPT: 的计数还额外限定 `target_id IN (SELECT id::text FROM vis)`，所以非超管看不到别人的拦截次数。
// SCOPE-EXEMPT: `sys_user` 只出现在 `lookup_username`（按主键取一个账号名，补审计行的 `username` 冗余列，
// SCOPE-EXEMPT: 不是列表查询、也没有跨人可见性问题）。闸② 是按**文件**搜 `scope::` 字面量，片段生成逻辑
// SCOPE-EXEMPT: 住在 `flow_model_inbox.rs`，故按口径显式登记。真正违规的动作是删掉 `instance_visibility()` 调用。

use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::Value as Json;

use crate::shared::error_code;

// ==================== 操作码（报表与日志页都按它检索，不许改字面量） ====================

/// 「他被允许自审」：引擎落 PENDING 且 `is_self_approval = true` 时写一条。
pub const OP_ALLOWED: &str = "FLOW_SELF_APPROVAL_ALLOWED";
/// 「他真的自审了」：`vote()` 提交的这张票 `is_self_approval = true` 时写一条。
pub const OP_VOTED: &str = "FLOW_SELF_APPROVAL_VOTED";
/// 「超出自审金额上限被拦下」：阈值把发起人从可签名单降级成 SKIPPED 时写一条。
///
/// 报表的「超阈值被拦下次数」就数这个操作码（不建计数表）。
pub const OP_BLOCKED: &str = "FLOW_SELF_APPROVAL_BLOCKED";

/// `operation_logs.target_type`：自审留痕挂在审批实例上（`target_id` = 实例 id）。
pub const TARGET_TYPE: &str = "biz_flow_instance";

// ==================== 金额取值 ====================

/// 从实例表单快照里取「单据金额」。
///
/// 口径（卡面钉死）：`form_data.amount` → `form_data.total_amount` → `None`（视为 0，不拦）。
/// 只认**数字**：JSON number，或数值形态的字符串（动态表单常把金额存成 `"5000.00"`）。
/// 其它形态（数组 / 对象 / `"1,000"` / `null`）一律当作「取不到」，继续看下一个键。
///
/// # 参数
///
/// - `form`：`biz_flow_instance.form_data` 快照（发起时冻结）。
///
/// # 返回值
///
/// `Some(金额)`；两个键都取不到时 `None`（调用方记 `debug` 并**不拦**）。
pub fn amount_of(form: &Json) -> Option<f64> {
    for key in ["amount", "total_amount"] {
        if let Some(value) = form.get(key).and_then(numeric) {
            return Some(value);
        }
    }
    None
}

/// 把一个 JSON 值当金额解析（number 直接取，字符串按十进制解析）。
fn numeric(value: &Json) -> Option<f64> {
    match value {
        Json::Number(number) => number.as_f64(),
        Json::String(text) => {
            let trimmed = text.trim();
            // 与 `separation::parse_amount` **同一份口径**：只认十进制字面量，
            // 不猜千分位 / 货币符号 / 科学计数法（SQL 侧的正则也是同一条规则）
            if !crate::business::erp::separation::is_decimal_literal(trimmed) {
                return None;
            }
            trimmed.parse::<f64>().ok().filter(|v| v.is_finite())
        }
        _ => None,
    }
}

/// 金额的可读文本（两位小数；用于留痕文案与报文，不做会计运算）。
pub fn format_amount(value: f64) -> String {
    format!("{value:.2}")
}

// ==================== 超限判定与降级 ====================

/// 一次「因金额超限被降级」的留痕（节点级列表，逐人一条）。
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// 被降级的人（一定是发起人本人）。
    pub user_id: Uuid,
    /// 姓名快照（写进 SKIPPED 行的 comment）。
    pub name: String,
    /// 单据金额。
    pub amount: f64,
    /// 生效的自审金额上限（> 0）。
    pub threshold: f64,
}

impl Block {
    /// SKIPPED 待办行的 comment / 报文里那句人话（「超出自审金额上限 X」是卡面要求的字样）。
    pub fn comment(&self) -> String {
        format!(
            "单据金额 {} 超出自审金额上限 {}，self_approval=ALLOW 降级为 SKIP（不许自审）",
            format_amount(self.amount),
            format_amount(self.threshold)
        )
    }
}

/// 是否触发了金额上限：上限 ≤ 0 = 不限额（永远不拦）。
///
/// 边界口径：**等于上限不拦**（「不超过 X 可以自审」），只有 `金额 > 上限` 才降级。
/// 写死这条是为了让「阈值 1000 / 单据 1000」这种边界在单测里可判、不含糊。
pub fn over_limit(amount: f64, threshold: f64) -> bool {
    threshold > 0.0 && amount > threshold
}

// ==================== 审计留痕（operation_logs） ====================

/// 一条自审审计留痕的入参（字段即 `detail` 的内容 + 检索列）。
#[derive(Debug, Clone)]
pub struct AuditEntry<'a> {
    /// 操作码（[`OP_ALLOWED`] / [`OP_VOTED`] / [`OP_BLOCKED`]）。
    pub operation: &'a str,
    /// 操作人（自审的那个人）。
    pub user_id: Uuid,
    /// 操作人账号（`None` = 由 [`record`] 查 `sys_user.username` 补）。
    pub username: Option<&'a str>,
    /// 操作人姓名快照（写进 `detail.assignee_name`，日志页不必再联表）。
    pub display_name: Option<&'a str>,
    /// 审批实例。
    pub instance_id: Uuid,
    /// 单据号（`biz_flow_instance.instance_no`）。
    pub instance_no: &'a str,
    /// 业务类型。
    pub biz_type: &'a str,
    /// 业务单号（可能为空）。
    pub biz_no: Option<&'a str>,
    /// 节点 key。
    pub node_key: &'a str,
    /// 节点中文名。
    pub node_name: &'a str,
    /// 单据金额（`None` = 表单里没有金额，按 0 计）。
    pub amount: Option<f64>,
    /// 生效的自审金额上限（0 = 不限额）。
    pub threshold: f64,
    /// 自审原因 / 处置说明（人话，直接给人看）。
    pub reason: &'a str,
    /// 待办 id（`OP_VOTED` 时必有）。
    pub task_id: Option<Uuid>,
    /// 票的结论（`OP_VOTED` 时是 `APPROVED` / `REJECTED`，其余为空）。
    pub vote: Option<&'a str>,
}

/// 写一条自审审计留痕（**调用方必须在引擎事务内**，失败即整单回滚）。
///
/// # 为什么 `request_id` 是现生成的
///
/// 引擎拿不到 HTTP 请求上下文（`vote()` 只有事务 + 参数），而 `operation_logs.request_id`
/// 是 `NOT NULL`。这里生成一个 UUID v7 作为**这次审计事件的追踪 id**，并把实例 / 待办 id
/// 写进 `detail` —— 与同一请求里中间件写的那行日志靠 `detail.instance_id` 关联。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（由调用方 `?` 上抛，整单回滚）。
pub async fn record(ex: &Transaction, entry: &AuditEntry<'_>) -> DbResult<()> {
    // 账号列留空时补一次（自审是低频事件，一条小查询换「日志页可直接按账号检索」）
    let username = match entry.username {
        Some(name) => Some(name.to_string()),
        None => lookup_username(ex, entry.user_id).await,
    };
    let detail = serde_json::json!({
        "instance_id": entry.instance_id.to_string(),
        "instance_no": entry.instance_no,
        "biz_type": entry.biz_type,
        "biz_no": entry.biz_no,
        "node_key": entry.node_key,
        "node_name": entry.node_name,
        "amount": entry.amount.unwrap_or(0.0),
        "threshold": entry.threshold,
        "reason": entry.reason,
        "assignee_name": entry.display_name,
        "task_id": entry.task_id.map(|id| id.to_string()),
        "vote": entry.vote,
        // 留痕自证：这条记录只可能来自「节点配置了自审放开」
        "self_approval": "ALLOW",
    })
    .to_string();

    ex.execute_with_params(
        "INSERT INTO operation_logs \
             (id, request_id, user_id, username, operation, target_type, target_id, detail, \
              request_path, request_method, status_code, created_at) \
         VALUES (?::uuid, ?, ?::uuid, ?, ?, ?, ?, ?::jsonb, ?, ?, ?, now())",
        vec![
            Value::from(&Uuid::now_v7()),
            Value::Text(Uuid::now_v7().to_string()),
            Value::from(&entry.user_id),
            opt_text(username.as_deref()),
            Value::Text(entry.operation.to_string()),
            Value::Text(TARGET_TYPE.to_string()),
            Value::Text(entry.instance_id.to_string()),
            // ⚠️ jsonb 位必须 `Value::Json`（`Value::Text` 绑 `?::jsonb` 会序列化失败）
            Value::Json(detail),
            Value::Null,
            Value::Null,
            Value::Null,
        ],
    )
    .await?;
    Ok(())
}

/// 查账号（查不到返回 `None`，绝不让「用户被删了」把审计写挂）。
async fn lookup_username(ex: &Transaction, user_id: Uuid) -> Option<String> {
    let rows = ex
        .find_with_params(
            "SELECT username FROM sys_user WHERE id = ?::uuid",
            vec![Value::from(&user_id)],
        )
        .await
        .ok()?;
    rows.first().and_then(|row| row.get_typed::<String>("username").ok())
}

fn opt_text(value: Option<&str>) -> Value {
    match value {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Null,
    }
}

// ==================== 月度报表 ====================

/// 报表的月度时间窗（**北京时间自然月，左闭右开**）。
///
/// # 为什么按 +08:00 而不是 UTC
///
/// 「月度自审占比」是给财务 / 内控看的自然月，与会计期间同一口径。用 UTC 切会让
/// 北京时间每月最后一天 08:00–24:00 的自审落到**下个月**，报表与账期对不上。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// 规范化后的月份（`YYYY-MM`）。
    pub month: String,
    /// 起始时刻（含）：当月 1 日 00:00:00 +08:00。
    pub start: chrono::DateTime<chrono::Utc>,
    /// 结束时刻（不含）：次月 1 日 00:00:00 +08:00。
    pub end: chrono::DateTime<chrono::Utc>,
}

impl Window {
    /// 解析 `month=YYYY-MM`（缺省 / 空串 = 当月）。
    ///
    /// # 异常情况
    ///
    /// 格式非法（不是 `YYYY-MM`、月份不在 1–12）→ `Err(中文原因)`，handler 翻成 `400 + 65005`。
    pub fn parse(month: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<Self, String> {
        const OFFSET_SECONDS: i32 = 8 * 3600; // 北京时间
        let Some(offset) = chrono::FixedOffset::east_opt(OFFSET_SECONDS) else {
            return Err("时区偏移构造失败".to_string());
        };
        let local_now = now.with_timezone(&offset);
        let trimmed = month.map(str::trim).filter(|value| !value.is_empty());
        let (year, month_no) = match trimmed {
            None => {
                use chrono::Datelike;
                (local_now.year(), local_now.month())
            }
            Some(text) => {
                let (year_raw, month_raw) = text
                    .split_once('-')
                    .ok_or_else(|| format!("month 格式非法（应为 YYYY-MM）：{text}"))?;
                let year: i32 = year_raw
                    .parse()
                    .map_err(|_| format!("month 格式非法（应为 YYYY-MM）：{text}"))?;
                let month_no: u32 = month_raw
                    .parse()
                    .map_err(|_| format!("month 格式非法（应为 YYYY-MM）：{text}"))?;
                if !(1..=12).contains(&month_no) {
                    return Err(format!("month 的月份必须在 01–12 之间：{text}"));
                }
                (year, month_no)
            }
        };

        let start_local = chrono::NaiveDate::from_ymd_opt(year, month_no, 1)
            .ok_or_else(|| format!("month 非法：{year}-{month_no:02}"))?
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| "月份起点构造失败".to_string())?;
        let (next_year, next_month) = if month_no == 12 {
            (year + 1, 1)
        } else {
            (year, month_no + 1)
        };
        let end_local = chrono::NaiveDate::from_ymd_opt(next_year, next_month, 1)
            .ok_or_else(|| "月份终点构造失败".to_string())?
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| "月份终点构造失败".to_string())?;

        Ok(Self {
            month: format!("{year}-{month_no:02}"),
            start: start_local
                .and_local_timezone(offset)
                .single()
                .ok_or_else(|| "月份起点时区换算失败".to_string())?
                .with_timezone(&chrono::Utc),
            end: end_local
                .and_local_timezone(offset)
                .single()
                .ok_or_else(|| "月份终点时区换算失败".to_string())?
                .with_timezone(&chrono::Utc),
        })
    }
}

/// 按节点聚合的一行。
#[derive(Debug, Clone, Default)]
pub struct NodeRow {
    /// 节点 key。
    pub node_key: String,
    /// 节点中文名（快照里没名字时回退 key）。
    pub node_name: String,
    /// 自审票数。
    pub tickets: i64,
    /// 涉及实例数。
    pub instances: i64,
    /// 涉及实例金额合计（`::text`，见 [`Report::self_approval_amount`] 的说明）。
    pub amount: String,
}

/// 按人聚合的一行。
#[derive(Debug, Clone, Default)]
pub struct PersonRow {
    /// 审批人 id。
    pub user_id: Uuid,
    /// 审批人姓名快照。
    pub assignee_name: String,
    /// 自审票数。
    pub tickets: i64,
    /// 涉及实例数。
    pub instances: i64,
    /// 涉及实例金额合计（`::text`）。
    pub amount: String,
}

/// 明细行（最近若干条自审票）。
#[derive(Debug, Clone)]
pub struct TicketRow {
    /// 待办 id。
    pub task_id: Uuid,
    /// 实例 id。
    pub instance_id: Uuid,
    /// 单据号。
    pub instance_no: String,
    /// 标题。
    pub title: String,
    /// 节点 key。
    pub node_key: String,
    /// 节点中文名。
    pub node_name: String,
    /// 审批人 id。
    pub user_id: Uuid,
    /// 审批人姓名。
    pub assignee_name: String,
    /// 投票时刻。
    pub voted_at: chrono::DateTime<chrono::Utc>,
    /// 该实例的金额（`::text`）。
    pub amount: String,
}

/// 月度自审报表。
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// 月份（`YYYY-MM`）。
    pub month: String,
    /// 时间窗起点（含）。
    pub start: chrono::DateTime<chrono::Utc>,
    /// 时间窗终点（不含）。
    pub end: chrono::DateTime<chrono::Utc>,
    /// 当前生效的自审金额上限（0 = 不限额）。
    pub threshold: f64,
    /// **分母**：同期已结算（`status=APPROVED` 且 `ended_at` 落在本窗）的审批单数。
    pub settled_instances: i64,
    /// **分子**：本窗内有自审票被通过（`status=APPROVED`）的审批单数（去重）。
    pub self_approval_instances: i64,
    /// 本窗内自审票数（一个会签节点可能有多张）。
    pub self_approval_tickets: i64,
    /// 本窗内自审单的金额合计（**字符串**：金额一律 `::text` 取出，别用浮点加减）。
    pub self_approval_amount: String,
    /// 超阈值被拦下次数（数 `operation_logs` 里的 [`OP_BLOCKED`]）。
    pub blocked_count: i64,
    /// 按节点 top N。
    pub by_node: Vec<NodeRow>,
    /// 按人 top N。
    pub by_person: Vec<PersonRow>,
    /// 明细（最近 [`DETAIL_LIMIT`] 条）。
    pub list: Vec<TicketRow>,
}

impl Report {
    /// 自审占比 = 分子 / 分母（0–1）。
    ///
    /// 分母为 0（本月没有已结算单）→ `0.0`，**绝不下发 NaN / null**。
    /// 允许 > 1：自审票在本月、单据下月才结算时，分子可能暂时超过本月分母；
    /// 调用方把口径写在响应里（`ratio_note`），别让前端自己猜。
    pub fn ratio(&self) -> f64 {
        if self.settled_instances <= 0 {
            return 0.0;
        }
        let value = self.self_approval_instances as f64 / self.settled_instances as f64;
        (value * 10_000.0).round() / 10_000.0
    }
}

/// 按节点 / 按人 top N 的条数。
pub const TOP_N: i64 = 10;
/// 明细条数上限（报表页一次展示足够，明细翻查请用审批中心列表）。
pub const DETAIL_LIMIT: i64 = 50;

/// 表单金额的 SQL 表达式（与 [`amount_of`] **逐字同口径**）。
///
/// 正则先判「像不像数字」再 `::numeric`：直接 `(form_data->>'amount')::numeric`
/// 会在遇到 `"1,000"` / `"待定"` 时**整条查询报错**（一张脏表单打挂整份报表）。
const AMOUNT_SQL: &str = "CASE \
     WHEN (i.form_data->>'amount') ~ '^-?[0-9]+(\\.[0-9]+)?$' \
       THEN (i.form_data->>'amount')::numeric \
     WHEN (i.form_data->>'total_amount') ~ '^-?[0-9]+(\\.[0-9]+)?$' \
       THEN (i.form_data->>'total_amount')::numeric \
     ELSE 0::numeric END";

/// 月度自审报表（**可见性口径与实例列表 / 其余统计三接口同一份**）。
///
/// # 参数
///
/// - `principal`：当前登录者（数据范围裁剪用）。
/// - `window`：月度时间窗（[`Window::parse`]）。
/// - `biz_type`：可选业务类型过滤（空 = 全部业务）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn monthly_report(
    principal: &crate::business::shared::principal::ScopedPrincipal,
    window: &Window,
    biz_type: Option<&str>,
    threshold: f64,
) -> DbResult<Report> {
    let vis = super::flow_model_inbox::instance_visibility(principal).await?;
    monthly_report_with(&vis, window, biz_type, threshold).await
}

/// 报表主体（可见性片段外部传入 ⇒ 单测不必造 `ScopedPrincipal`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub(crate) async fn monthly_report_with(
    vis: &super::flow_model_inbox::VisibilityClause,
    window: &Window,
    biz_type: Option<&str>,
    threshold: f64,
) -> DbResult<Report> {
    let start = window.start.to_rfc3339();
    let end = window.end.to_rfc3339();
    let biz = biz_type.unwrap_or("").to_string();

    // ---------- ① 汇总（分子 / 分母 / 金额 / 拦下次数） ----------
    let metrics_sql = format!(
        "WITH vis AS ( \
             SELECT i.id, i.status, i.ended_at, i.biz_type, i.form_data \
               FROM biz_flow_instance i \
              WHERE i.deleted_at IS NULL AND {vis_sql} \
         ), self_t AS ( \
             SELECT t.id, t.instance_id \
               FROM biz_flow_task t \
               JOIN vis ON vis.id = t.instance_id \
              WHERE t.is_self_approval = TRUE AND t.status = 'APPROVED' \
                AND t.completed_at >= ?::text::timestamptz \
                AND t.completed_at <  ?::text::timestamptz \
                AND (? = '' OR vis.biz_type = ?) \
         ) \
         SELECT \
           (SELECT count(DISTINCT instance_id)::bigint FROM self_t) AS self_instances, \
           (SELECT count(*)::bigint FROM self_t) AS self_tickets, \
           (SELECT COALESCE(sum({amount_sql}), 0)::text \
              FROM vis i WHERE i.id IN (SELECT instance_id FROM self_t)) AS self_amount, \
           (SELECT count(*)::bigint FROM vis \
             WHERE status = 'APPROVED' \
               AND ended_at >= ?::text::timestamptz \
               AND ended_at <  ?::text::timestamptz \
               AND (? = '' OR biz_type = ?)) AS settled, \
           (SELECT count(*)::bigint FROM operation_logs o \
             WHERE o.operation = ? \
               AND o.created_at >= ?::text::timestamptz \
               AND o.created_at <  ?::text::timestamptz \
               AND o.target_id IN (SELECT id::text FROM vis) \
               AND (? = '' OR o.detail->>'biz_type' = ?)) AS blocked",
        vis_sql = vis.sql,
        amount_sql = AMOUNT_SQL
    );
    let mut params: Vec<Value> = vis.params.clone();
    params.push(Value::Text(start.clone()));
    params.push(Value::Text(end.clone()));
    params.push(Value::Text(biz.clone()));
    params.push(Value::Text(biz.clone()));
    params.push(Value::Text(start.clone()));
    params.push(Value::Text(end.clone()));
    params.push(Value::Text(biz.clone()));
    params.push(Value::Text(biz.clone()));
    params.push(Value::Text(OP_BLOCKED.to_string()));
    params.push(Value::Text(start.clone()));
    params.push(Value::Text(end.clone()));
    params.push(Value::Text(biz.clone()));
    params.push(Value::Text(biz.clone()));
    let rows = db().find_with_params(&metrics_sql, params).await?;
    // 聚合查询（无 GROUP BY）恒定返回一行；返回空只可能是驱动层异常 → NotFound
    let row = rows.first().ok_or(DbError::NotFound)?;
    let mut report = Report {
        month: window.month.clone(),
        start: window.start,
        end: window.end,
        threshold,
        settled_instances: row.get_typed("settled")?,
        self_approval_instances: row.get_typed("self_instances")?,
        self_approval_tickets: row.get_typed("self_tickets")?,
        self_approval_amount: row.get_typed("self_amount")?,
        blocked_count: row.get_typed("blocked")?,
        ..Report::default()
    };

    // ---------- ② 按节点 top N ----------
    let node_sql = format!(
        "WITH vis AS ( \
             SELECT i.id, i.status, i.ended_at, i.biz_type, i.form_data \
               FROM biz_flow_instance i \
              WHERE i.deleted_at IS NULL AND {vis_sql} \
         ) \
         SELECT t.node_key AS node_key, \
                COALESCE(max(n.node_name), max(t.node_key)) AS node_name, \
                count(*)::bigint AS tickets, \
                count(DISTINCT t.instance_id)::bigint AS instances, \
                COALESCE(sum({amount_sql}), 0)::text AS amount \
           FROM biz_flow_task t \
           JOIN vis i ON i.id = t.instance_id \
           LEFT JOIN biz_flow_instance_node n ON n.id = t.instance_node_id \
          WHERE t.is_self_approval = TRUE AND t.status = 'APPROVED' \
            AND t.completed_at >= ?::text::timestamptz \
            AND t.completed_at <  ?::text::timestamptz \
            AND (? = '' OR i.biz_type = ?) \
          GROUP BY t.node_key \
          ORDER BY tickets DESC, node_key ASC \
          LIMIT ?",
        vis_sql = vis.sql,
        amount_sql = AMOUNT_SQL
    );
    let mut params: Vec<Value> = vis.params.clone();
    params.push(Value::Text(start.clone()));
    params.push(Value::Text(end.clone()));
    params.push(Value::Text(biz.clone()));
    params.push(Value::Text(biz.clone()));
    params.push(Value::from(TOP_N));
    for row in &db().find_with_params(&node_sql, params).await? {
        report.by_node.push(NodeRow {
            node_key: row.get_typed("node_key")?,
            node_name: row.get_typed("node_name")?,
            tickets: row.get_typed("tickets")?,
            instances: row.get_typed("instances")?,
            amount: row.get_typed("amount")?,
        });
    }

    // ---------- ③ 按人 top N ----------
    let person_sql = format!(
        "WITH vis AS ( \
             SELECT i.id, i.status, i.ended_at, i.biz_type, i.form_data \
               FROM biz_flow_instance i \
              WHERE i.deleted_at IS NULL AND {vis_sql} \
         ) \
         SELECT t.assignee_id AS user_id, \
                COALESCE(max(t.assignee_name), '') AS assignee_name, \
                count(*)::bigint AS tickets, \
                count(DISTINCT t.instance_id)::bigint AS instances, \
                COALESCE(sum({amount_sql}), 0)::text AS amount \
           FROM biz_flow_task t \
           JOIN vis i ON i.id = t.instance_id \
          WHERE t.is_self_approval = TRUE AND t.status = 'APPROVED' \
            AND t.completed_at >= ?::text::timestamptz \
            AND t.completed_at <  ?::text::timestamptz \
            AND (? = '' OR i.biz_type = ?) \
          GROUP BY t.assignee_id \
          ORDER BY tickets DESC, user_id ASC \
          LIMIT ?",
        vis_sql = vis.sql,
        amount_sql = AMOUNT_SQL
    );
    let mut params: Vec<Value> = vis.params.clone();
    params.push(Value::Text(start.clone()));
    params.push(Value::Text(end.clone()));
    params.push(Value::Text(biz.clone()));
    params.push(Value::Text(biz.clone()));
    params.push(Value::from(TOP_N));
    for row in &db().find_with_params(&person_sql, params).await? {
        report.by_person.push(PersonRow {
            user_id: row.get_typed("user_id")?,
            assignee_name: row.get_typed("assignee_name")?,
            tickets: row.get_typed("tickets")?,
            instances: row.get_typed("instances")?,
            amount: row.get_typed("amount")?,
        });
    }

    // ---------- ④ 明细 ----------
    let detail_sql = format!(
        "WITH vis AS ( \
             SELECT i.id, i.instance_no, i.title, i.biz_type, i.form_data \
               FROM biz_flow_instance i \
              WHERE i.deleted_at IS NULL AND {vis_sql} \
         ) \
         SELECT t.id AS task_id, t.instance_id AS instance_id, \
                i.instance_no AS instance_no, i.title AS title, \
                t.node_key AS node_key, \
                COALESCE(n.node_name, t.node_key) AS node_name, \
                t.assignee_id AS user_id, \
                COALESCE(t.assignee_name, '') AS assignee_name, \
                t.completed_at AS voted_at, \
                ({amount_sql})::text AS amount \
           FROM biz_flow_task t \
           JOIN vis i ON i.id = t.instance_id \
           LEFT JOIN biz_flow_instance_node n ON n.id = t.instance_node_id \
          WHERE t.is_self_approval = TRUE AND t.status = 'APPROVED' \
            AND t.completed_at >= ?::text::timestamptz \
            AND t.completed_at <  ?::text::timestamptz \
            AND (? = '' OR i.biz_type = ?) \
          ORDER BY t.completed_at DESC, t.id DESC \
          LIMIT ?",
        vis_sql = vis.sql,
        amount_sql = AMOUNT_SQL
    );
    let mut params: Vec<Value> = vis.params.clone();
    params.push(Value::Text(start));
    params.push(Value::Text(end));
    params.push(Value::Text(biz.clone()));
    params.push(Value::Text(biz));
    params.push(Value::from(DETAIL_LIMIT));
    for row in &db().find_with_params(&detail_sql, params).await? {
        report.list.push(TicketRow {
            task_id: row.get_typed("task_id")?,
            instance_id: row.get_typed("instance_id")?,
            instance_no: row.get_typed("instance_no")?,
            title: row.get_typed("title")?,
            node_key: row.get_typed("node_key")?,
            node_name: row.get_typed("node_name")?,
            user_id: row.get_typed("user_id")?,
            assignee_name: row.get_typed("assignee_name")?,
            voted_at: row.get_typed("voted_at")?,
            amount: row.get_typed("amount")?,
        });
    }

    Ok(report)
}

/// 报表「分母」口径的人话说明（随响应下发，避免前端自己解释占比）。
pub const RATIO_NOTE: &str = "占比 = 本窗内有自审票被通过的审批单数 ÷ 同窗已结算（APPROVED 且 ended_at 在本窗）\
的审批单数；窗口按北京时间自然月左闭右开。自审票在本窗而单据下月才结算时，占比可能暂时大于 100%。";

/// 65044 的报文（**唯一出口**）：说清金额、上限、系统已经替他想过的降级链、以及两条出路。
///
/// # 参数
///
/// - `node_name`：节点中文名。
/// - `amount`：单据金额。
/// - `threshold`：生效的自审金额上限。
/// - `dispatch`：升级链 / 兜底人的处置说明（[`super::assignee::dispatch_hint`] 的产物）。
pub fn over_limit_message(node_name: &str, amount: f64, threshold: f64, dispatch: &str) -> String {
    format!(
        "节点「{node_name}」配置了允许自审（self_approval=ALLOW），但单据金额 {} 超出自审金额上限 {}，\
         系统已按内控把自审降级为跳过（先走升级链 / 兜底人），仍没有其他可用审批人。{dispatch}。\
         请把该节点的审批岗位指派给发起人以外的人，或在系统参数「{}」里调整上限（0 = 不限额）。",
        format_amount(amount),
        format_amount(threshold),
        crate::business::erp::separation::SELF_APPROVAL_MAX_AMOUNT_KEY
    )
}

/// 便捷构造 65044 错误（报文见 [`over_limit_message`]）。
pub fn over_limit_error(message: impl Into<String>) -> ApiError {
    ApiError::new(422, error_code::FLOW_SELF_APPROVAL_AMOUNT_LIMIT, message)
}

// ============================================================================================
// 单测：纯逻辑（不连库）
// ============================================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn utc(text: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(text)
            .expect("时间字面量")
            .with_timezone(&chrono::Utc)
    }

    /// ① 金额取值口径：`amount` 优先 → `total_amount` → 都没有 = None（不拦）。
    #[test]
    fn amount_prefers_amount_then_total_amount() {
        assert_eq!(
            amount_of(&serde_json::json!({"amount": 5000, "total_amount": 9000})),
            Some(5000.0),
            "amount 优先"
        );
        assert_eq!(
            amount_of(&serde_json::json!({"total_amount": 9000})),
            Some(9000.0),
            "没有 amount 时退到 total_amount"
        );
        assert_eq!(
            amount_of(&serde_json::json!({"amount": "5000.00"})),
            Some(5000.0),
            "动态表单把金额存成字符串也要认"
        );
        assert_eq!(
            amount_of(&serde_json::json!({"amount": "1,000"})),
            None,
            "千分位不是数字，不许猜"
        );
        assert_eq!(amount_of(&serde_json::json!({"amount": null})), None);
        assert_eq!(amount_of(&serde_json::json!({})), None);
        assert_eq!(amount_of(&serde_json::json!({"amount": [5000]})), None);
    }

    /// ② 阈值 0 = 不限额；等于上限不拦；超过才拦。
    #[test]
    fn over_limit_boundaries() {
        assert!(!over_limit(5_000.0, 0.0), "上限 0 = 不限额");
        assert!(!over_limit(5_000.0, 1_000.0 * 5.0), "等于上限不拦");
        assert!(over_limit(5_000.01, 5_000.0));
        assert!(!over_limit(0.0, 1_000.0));
        assert!(over_limit(1_000.01, 1_000.0), "阈值 1000 / 单据 1000.01 必须拦");
    }

    /// ③ 留痕文案必须点名金额与上限（卡面：「超出自审金额上限 X」）。
    #[test]
    fn block_comment_mentions_amount_and_threshold() {
        let block = Block {
            user_id: Uuid::nil(),
            name: "张三".to_string(),
            amount: 5_000.0,
            threshold: 1_000.0,
        };
        let text = block.comment();
        assert!(text.contains("5000.00"), "{text}");
        assert!(text.contains("1000.00"), "{text}");
        assert!(text.contains("超出自审金额上限"), "{text}");
        assert!(text.contains("SKIP"), "{text}");
    }

    /// ④ 月度边界（北京时间）：1 号 00:00 含、月末 23:59:59 含、次月 1 号 00:00 不含。
    ///
    /// 用 2026-11 做样本：起点必须是 UTC `2026-10-31T16:00:00Z`（= 北京 11-01 00:00）。
    #[test]
    fn window_parses_beijing_month_boundaries() {
        let window = Window::parse(Some("2026-11"), utc("2026-11-08T02:00:00Z")).expect("合法月份");
        assert_eq!(window.month, "2026-11");
        assert_eq!(window.start.to_rfc3339(), "2026-10-31T16:00:00+00:00");
        assert_eq!(window.end.to_rfc3339(), "2026-11-30T16:00:00+00:00");

        // 1 号 00:00:00（北京时间）落在窗内
        let first_midnight = utc("2026-10-31T16:00:00Z");
        assert!(first_midnight >= window.start && first_midnight < window.end);
        // 月末 23:59:59（北京时间 = UTC 15:59:59）仍落在窗内
        let last_second = utc("2026-11-30T15:59:59Z");
        assert!(last_second >= window.start && last_second < window.end);
        // 次月 1 号 00:00（北京时间）不算本月
        let next_month = utc("2026-11-30T16:00:00Z");
        assert!(next_month >= window.end, "右开区间：次月 1 号 00:00 不属于本月");

        // 12 月跨年
        let december = Window::parse(Some("2026-12"), utc("2026-12-15T00:00:00Z")).expect("合法月份");
        assert_eq!(december.end.to_rfc3339(), "2026-12-31T16:00:00+00:00");
        assert_eq!(december.month, "2026-12");

        // 缺省 / 空串 = 当月（按北京时间算：UTC 10-31T16:30 已经是北京 11-01）
        let implicit = Window::parse(None, utc("2026-10-31T16:30:00Z")).expect("缺省当月");
        assert_eq!(implicit.month, "2026-11", "缺省月份按北京时间判");
        let blank = Window::parse(Some("  "), utc("2026-11-08T02:00:00Z")).expect("空串当月");
        assert_eq!(blank.month, "2026-11");

        // 非法格式一律报错（handler 翻 400 + 65005）
        for bad in ["2026-13", "202611", "2026-1x", "abc", "2026-00"] {
            assert!(
                Window::parse(Some(bad), utc("2026-11-08T02:00:00Z")).is_err(),
                "{bad} 应当非法"
            );
        }
    }

    /// ⑤ 占比：分母 0 → 0.0（不许 NaN）；分子可暂时超过分母，但不许出现 NaN。
    #[test]
    fn ratio_never_returns_nan() {
        let mut report = Report::default();
        assert_eq!(report.ratio(), 0.0);
        report.settled_instances = 4;
        report.self_approval_instances = 1;
        assert_eq!(report.ratio(), 0.25);
        report.settled_instances = 1;
        report.self_approval_instances = 1;
        assert_eq!(report.ratio(), 1.0);
        report.settled_instances = 2;
        report.self_approval_instances = 3;
        assert_eq!(report.ratio(), 1.5, "分子可暂时超分母（跨月结算）");
    }

    /// ⑥ 65044 报文必须点名上限与配置键（否则运维不知道去哪儿改）。
    #[test]
    fn over_limit_message_is_actionable() {
        let text = over_limit_message("部门经理审批", 5_000.0, 1_000.0, "兜底人未配置");
        assert!(text.contains("部门经理审批"), "{text}");
        assert!(text.contains("5000.00"), "{text}");
        assert!(text.contains("1000.00"), "{text}");
        assert!(text.contains("超出自审金额上限"), "{text}");
        assert!(text.contains("兜底人未配置"), "{text}");
        assert!(
            text.contains(crate::business::erp::separation::SELF_APPROVAL_MAX_AMOUNT_KEY),
            "{text}"
        );
        assert_eq!(over_limit_error("x").code, error_code::FLOW_SELF_APPROVAL_AMOUNT_LIMIT);
    }
}

// ============================================================================================
// 单测：真库（`ERP_DB_URL` 开关，不设即 SKIP 并打印 —— CI 无库时不假绿）
// ============================================================================================
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::business::flow::flow_model_inbox::VisibilityClause;

    /// 本模块夹具的业务类型（**报表按它过滤** ⇒ 与库里其它夹具数据互不干扰）。
    const BIZ: &str = "P305UT";

    async fn ensure_db() -> bool {
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
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn exec(sql: &str, params: Vec<Value>) {
        if let Err(err) = db().execute_with_params(sql, params).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    /// 一轮夹具：流程定义 + 版本 + 发起人 + 三个审批人（名字可读，报表断言按名字比对）。
    struct Fixture {
        tag: String,
        def_id: Uuid,
        version_id: Uuid,
        initiator: Uuid,
        person_a: Uuid,
        person_b: Uuid,
    }

    async fn setup(tag: &str) -> Fixture {
        let def_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let initiator = Uuid::now_v7();
        let person_a = Uuid::now_v7();
        let person_b = Uuid::now_v7();
        cleanup(tag).await;
        exec(
            "INSERT INTO sys_user (id, username, password, real_name, status) VALUES \
             (?::uuid, ?, 'x', '自审发起人', 1), \
             (?::uuid, ?, 'x', '自审甲', 1), \
             (?::uuid, ?, 'x', '自审乙', 1)",
            vec![
                Value::from(&initiator),
                Value::Text(format!("p305_{tag}_init")),
                Value::from(&person_a),
                Value::Text(format!("p305_{tag}_a")),
                Value::from(&person_b),
                Value::Text(format!("p305_{tag}_b")),
            ],
        )
        .await;
        exec(
            "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, status, \
                 current_version_id, current_version_no) \
             VALUES (?::uuid, ?, 'P3-05 自审监控夹具', ?, 'PUBLISHED', ?::uuid, 1)",
            vec![
                Value::from(&def_id),
                Value::Text(format!("p305_{tag}")),
                Value::Text(BIZ.to_string()),
                Value::from(&version_id),
            ],
        )
        .await;
        exec(
            "INSERT INTO biz_flow_def_version \
                 (id, def_id, version_no, version_status, is_current, graph_hash, \
                  graph_snapshot, node_count, edge_count, published_at) \
             VALUES (?::uuid, ?::uuid, 1, 'PUBLISHED', true, ?, '{}'::jsonb, 0, 0, now())",
            vec![
                Value::from(&version_id),
                Value::from(&def_id),
                Value::Text("0".repeat(64)),
            ],
        )
        .await;
        Fixture {
            tag: tag.to_string(),
            def_id,
            version_id,
            initiator,
            person_a,
            person_b,
        }
    }

    async fn cleanup(tag: &str) {
        // ① 日志（按实例 id 反查，先取出 id 列表）
        let ids = instance_ids(tag).await;
        for id in &ids {
            exec(
                "DELETE FROM operation_logs WHERE target_id = ?",
                vec![Value::Text(id.to_string())],
            )
            .await;
        }
        // ② 待办 → 节点 → 实例 → 版本 → 定义 → 用户
        exec(
            "DELETE FROM biz_flow_task WHERE instance_id IN \
             (SELECT id FROM biz_flow_instance WHERE flow_code = ?)",
            vec![Value::Text(format!("p305_{tag}"))],
        )
        .await;
        exec(
            "DELETE FROM biz_flow_instance_node WHERE instance_id IN \
             (SELECT id FROM biz_flow_instance WHERE flow_code = ?)",
            vec![Value::Text(format!("p305_{tag}"))],
        )
        .await;
        exec(
            "DELETE FROM biz_flow_instance WHERE flow_code = ?",
            vec![Value::Text(format!("p305_{tag}"))],
        )
        .await;
        exec(
            "DELETE FROM biz_flow_def_version WHERE def_id IN \
             (SELECT id FROM biz_flow_def WHERE flow_code = ?)",
            vec![Value::Text(format!("p305_{tag}"))],
        )
        .await;
        exec(
            "DELETE FROM biz_flow_def WHERE flow_code = ?",
            vec![Value::Text(format!("p305_{tag}"))],
        )
        .await;
        exec(
            "DELETE FROM sys_user WHERE username LIKE ?",
            vec![Value::Text(format!("p305_{tag}_%"))],
        )
        .await;
    }

    async fn instance_ids(tag: &str) -> Vec<Uuid> {
        let rows = db()
            .find_with_params(
                "SELECT id FROM biz_flow_instance WHERE flow_code = ?",
                vec![Value::Text(format!("p305_{tag}"))],
            )
            .await
            .expect("查夹具实例");
        rows.iter()
            .map(|row| row.get_typed::<Uuid>("id").expect("实例 id"))
            .collect()
    }

    /// 造一条实例（`ended` = `ended_at` 的 SQL 表达式，`None` 表示未结束）。
    #[allow(clippy::too_many_arguments)]
    async fn add_instance(fx: &Fixture, no: &str, status: &str, ended: Option<&str>, form: serde_json::Value) -> Uuid {
        let id = Uuid::now_v7();
        let ended_sql = ended.map(|sql| sql.to_string()).unwrap_or("NULL".to_string());
        exec(
            &format!(
                "INSERT INTO biz_flow_instance \
                     (id, instance_no, def_id, version_id, version_no, flow_code, flow_name, \
                      biz_type, biz_no, title, status, initiator_id, ended_at, form_data) \
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, 1, ?, 'P3-05 自审夹具', ?, ?, ?, ?, ?::uuid, {ended_sql}, ?::jsonb)"
            ),
            vec![
                Value::from(&id),
                Value::Text(no.to_string()),
                Value::from(&fx.def_id),
                Value::from(&fx.version_id),
                Value::Text(format!("p305_{}", fx.tag)),
                Value::Text(BIZ.to_string()),
                Value::Text(no.to_string()),
                Value::Text(format!("P3-05 {no}")),
                Value::Text(status.to_string()),
                Value::from(&fx.initiator),
                Value::Json(form.to_string()),
            ],
        )
        .await;
        id
    }

    /// 造一条实例节点（节点名可读，报表的 `by_node` 断言按它比对）。
    async fn add_node(instance_id: Uuid, node_key: &str, node_name: &str) -> Uuid {
        let id = Uuid::now_v7();
        exec(
            "INSERT INTO biz_flow_instance_node \
                 (id, instance_id, node_key, node_name, node_type, status, ended_at) \
             VALUES (?::uuid, ?::uuid, ?, ?, 'APPROVAL', 'APPROVED', now())",
            vec![
                Value::from(&id),
                Value::from(&instance_id),
                Value::Text(node_key.to_string()),
                Value::Text(node_name.to_string()),
            ],
        )
        .await;
        id
    }

    /// 造一张票（`completed` = `completed_at` 的 SQL 表达式）。
    ///
    /// `#[allow(clippy::too_many_arguments)]`：这是测试夹具的「一行全列」写入器，
    /// 参数与 `biz_flow_task` 的必填列一一对应；拆 struct 只会让 20+ 处调用点更难读。
    #[allow(clippy::too_many_arguments)]
    async fn add_task(
        instance_id: Uuid,
        node_id: Uuid,
        node_key: &str,
        assignee: Uuid,
        assignee_name: &str,
        is_self: bool,
        status: &str,
        completed: &str,
    ) -> Uuid {
        let id = Uuid::now_v7();
        exec(
            &format!(
                "INSERT INTO biz_flow_task \
                     (id, instance_id, instance_node_id, node_key, assignee_id, assignee_name, \
                      assignee_source, status, is_self_approval, completed_at) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?, ?::uuid, ?, 'ROLE', ?, ?, {completed})"
            ),
            vec![
                Value::from(&id),
                Value::from(&instance_id),
                Value::from(&node_id),
                Value::Text(node_key.to_string()),
                Value::from(&assignee),
                Value::Text(assignee_name.to_string()),
                Value::Text(status.to_string()),
                Value::from(is_self),
            ],
        )
        .await;
        id
    }

    /// 直接插一条「超阈值被拦」日志（模拟引擎写过的那条）。
    async fn add_blocked_log(instance_id: Uuid, biz: &str, created_at_sql: &str) {
        let detail = serde_json::json!({ "biz_type": biz, "threshold": 1000.0, "amount": 5000.0 });
        exec(
            &format!(
                "INSERT INTO operation_logs \
                     (id, request_id, user_id, operation, target_type, target_id, detail, created_at) \
                 VALUES (?::uuid, ?, NULL, ?, ?, ?, ?::jsonb, {created_at_sql})"
            ),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::Text(Uuid::now_v7().to_string()),
                Value::Text(super::OP_BLOCKED.to_string()),
                Value::Text(super::TARGET_TYPE.to_string()),
                Value::Text(instance_id.to_string()),
                Value::Json(detail.to_string()),
            ],
        )
        .await;
    }

    fn all_visible() -> VisibilityClause {
        VisibilityClause {
            sql: "(1 = 1)".to_string(),
            params: Vec::new(),
        }
    }

    /// ⑤ 审计行字段断言：`operation_logs` 一行必须能读懂「谁 / 何时 / 哪张单 / 哪个节点 / 多少钱 / 为什么」。
    ///
    /// 整个用例在**事务里跑完回滚**，不在夹具库留任何行。
    #[tokio::test]
    async fn record_writes_readable_operation_log_row() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("log").await;
        let instance_id = add_instance(
            &fx,
            "P305-LOG-1",
            "PENDING",
            None,
            serde_json::json!({ "amount": 4321.5 }),
        )
        .await;

        let tx = db().begin().await.expect("开事务");
        super::record(
            &tx,
            &super::AuditEntry {
                operation: super::OP_VOTED,
                user_id: fx.initiator,
                username: None,
                display_name: Some("自审发起人"),
                instance_id,
                instance_no: "P305-LOG-1",
                biz_type: BIZ,
                biz_no: Some("P305-LOG-1"),
                node_key: "n_mgr",
                node_name: "部门经理审批",
                amount: Some(4321.5),
                threshold: 10_000.0,
                reason: "发起人以自审身份同意了自己的单据",
                task_id: Some(Uuid::now_v7()),
                vote: Some("APPROVED"),
            },
        )
        .await
        .expect("写审计行");

        let rows = tx
            .find_with_params(
                "SELECT request_id, user_id, username, operation, target_type, target_id, \
                        detail::text AS detail_text, created_at, request_path, status_code \
                   FROM operation_logs WHERE target_id = ?",
                vec![Value::Text(instance_id.to_string())],
            )
            .await
            .expect("读回审计行");
        assert_eq!(rows.len(), 1, "一次调用只写一行");
        let row = &rows[0];
        let request_id: String = row.get_typed("request_id").expect("request_id");
        assert!(!request_id.is_empty(), "request_id 是 NOT NULL 列，必须显式赋值");
        assert_eq!(row.get_typed::<Uuid>("user_id").expect("user_id"), fx.initiator);
        assert_eq!(
            row.get_typed::<Option<String>>("username").expect("username"),
            Some("p305_log_init".to_string()),
            "账号列必须由 record 补齐（日志页可直接按账号检索）"
        );
        assert_eq!(
            row.get_typed::<String>("operation").expect("operation"),
            super::OP_VOTED
        );
        assert_eq!(
            row.get_typed::<String>("target_type").expect("target_type"),
            super::TARGET_TYPE
        );
        let detail_text: String = row.get_typed("detail_text").expect("detail 文本");
        let detail: serde_json::Value = serde_json::from_str(&detail_text).expect("detail 必须是合法 JSON");
        assert_eq!(detail["node_name"], "部门经理审批", "{detail}");
        assert_eq!(detail["node_key"], "n_mgr", "{detail}");
        assert_eq!(detail["amount"], 4321.5, "{detail}");
        assert_eq!(detail["threshold"], 10_000.0, "{detail}");
        assert_eq!(detail["biz_type"], BIZ, "{detail}");
        assert_eq!(detail["vote"], "APPROVED", "{detail}");
        assert_eq!(detail["self_approval"], "ALLOW", "{detail}");
        assert!(
            detail["reason"].as_str().unwrap_or_default().contains("自审身份"),
            "{detail}"
        );
        // 追加写表关闭了时间戳 Hook：created_at 必须真被写上
        let created_at: chrono::DateTime<chrono::Utc> = row.get_typed("created_at").expect("created_at");
        let age = chrono::Utc::now() - created_at;
        assert!(age.num_seconds().abs() < 300, "created_at 必须是写入时刻");

        tx.rollback().await.expect("回滚");
        cleanup("log").await;
    }

    /// ④ + 验收：月度报表的分子 / 分母 / 金额 / top N / 明细 / 拦截计数 + **月度边界**。
    ///
    /// 边界口径（北京时间左闭右开）：
    /// - 当月 1 日 00:00:00（北京）= 窗内；月末 23:59:59（北京）= 窗内；
    /// - 次月 1 日 00:00（北京）= **窗外**。
    #[tokio::test]
    async fn monthly_report_counts_molecules_and_boundaries() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("rpt").await;

        // A：1 日 00:00 边界（窗内），金额 5000，自审票在边界时刻
        let a = add_instance(
            &fx,
            "P305-RPT-A",
            "APPROVED",
            Some("'2026-11-01T00:00:00+08:00'::timestamptz"),
            serde_json::json!({ "amount": 5000 }),
        )
        .await;
        let a_node = add_node(a, "n_mgr", "部门经理审批").await;
        add_task(
            a,
            a_node,
            "n_mgr",
            fx.initiator,
            "自审发起人",
            true,
            "APPROVED",
            "'2026-11-01T00:00:00+08:00'::timestamptz",
        )
        .await;

        // B：月末 23:59:59（窗内），金额走 total_amount = 800.50
        let b = add_instance(
            &fx,
            "P305-RPT-B",
            "APPROVED",
            Some("'2026-11-30T23:59:59+08:00'::timestamptz"),
            serde_json::json!({ "total_amount": "800.50" }),
        )
        .await;
        let b_node = add_node(b, "n_fin", "财务复核").await;
        add_task(
            b,
            b_node,
            "n_fin",
            fx.initiator,
            "自审发起人",
            true,
            "APPROVED",
            "'2026-11-30T23:59:59+08:00'::timestamptz",
        )
        .await;

        // C：次月 1 日 00:00（窗外）—— 分子分母都不许算进来
        let c = add_instance(
            &fx,
            "P305-RPT-C",
            "APPROVED",
            Some("'2026-12-01T00:00:00+08:00'::timestamptz"),
            serde_json::json!({ "amount": 9999 }),
        )
        .await;
        let c_node = add_node(c, "n_mgr", "部门经理审批").await;
        add_task(
            c,
            c_node,
            "n_mgr",
            fx.initiator,
            "自审发起人",
            true,
            "APPROVED",
            "'2026-12-01T00:00:00+08:00'::timestamptz",
        )
        .await;

        // D：窗内已结算但没有自审票（只进分母）
        let d = add_instance(
            &fx,
            "P305-RPT-D",
            "APPROVED",
            Some("'2026-11-15T12:00:00+08:00'::timestamptz"),
            serde_json::json!({ "amount": 100 }),
        )
        .await;
        let d_node = add_node(d, "n_mgr", "部门经理审批").await;
        add_task(
            d,
            d_node,
            "n_mgr",
            fx.person_a,
            "自审甲",
            false,
            "APPROVED",
            "'2026-11-15T12:00:00+08:00'::timestamptz",
        )
        .await;
        // 同一节点再加一张**非自审**票（按人榜里绝不能出现乙：只有自审票才进榜）
        add_task(
            d,
            d_node,
            "n_mgr",
            fx.person_b,
            "自审乙",
            false,
            "APPROVED",
            "'2026-11-15T13:00:00+08:00'::timestamptz",
        )
        .await;

        // E：窗内被拦下两次（一条在窗外，不该计）
        add_blocked_log(a, BIZ, "'2026-11-02T10:00:00+08:00'::timestamptz").await;
        add_blocked_log(b, BIZ, "'2026-11-20T10:00:00+08:00'::timestamptz").await;
        add_blocked_log(c, BIZ, "'2026-12-02T10:00:00+08:00'::timestamptz").await;

        let window = super::Window::parse(Some("2026-11"), chrono::Utc::now()).expect("2026-11");
        let report = super::monthly_report_with(&all_visible(), &window, Some(BIZ), 1_000.0)
            .await
            .expect("报表查询");

        assert_eq!(report.month, "2026-11");
        assert_eq!(report.settled_instances, 3, "分母 = A/B/D（C 在次月）");
        assert_eq!(report.self_approval_instances, 2, "分子 = A/B（C 在次月）");
        assert_eq!(report.self_approval_tickets, 2);
        assert_eq!(
            report.self_approval_amount, "5800.50",
            "金额合计 = 5000 + 800.50（total_amount 字符串也要认）"
        );
        assert_eq!(report.blocked_count, 2, "拦截计数只数窗内的两条");
        assert!(
            (report.ratio() - 0.6667).abs() < 1e-4,
            "占比 = 2/3，实际 {}",
            report.ratio()
        );

        // 按节点：n_mgr 1 票 / n_fin 1 票（C 的 n_mgr 不在窗内）
        assert_eq!(report.by_node.len(), 2, "两个节点：{:?}", report.by_node.len());
        let mgr = report
            .by_node
            .iter()
            .find(|row| row.node_key == "n_mgr")
            .expect("n_mgr 行");
        assert_eq!(mgr.node_name, "部门经理审批");
        assert_eq!(mgr.tickets, 1);
        assert_eq!(mgr.instances, 1);
        assert_eq!(mgr.amount, "5000");

        // 按人：自审发起人 2 票 2 单
        let person = report
            .by_person
            .iter()
            .find(|row| row.user_id == fx.initiator)
            .expect("发起人行");
        assert_eq!(person.assignee_name, "自审发起人");
        assert_eq!(person.tickets, 2);
        assert_eq!(person.instances, 2);
        assert_eq!(person.amount, "5800.50");
        assert_eq!(report.by_person.len(), 1, "按人榜只收自审票：非自审的甲 / 乙都不该出现");

        // 明细：按投票时刻倒序，C 不在里面
        assert_eq!(report.list.len(), 2);
        assert_eq!(report.list[0].instance_no, "P305-RPT-B", "最近的排最前");
        assert_eq!(report.list[0].node_name, "财务复核");
        assert_eq!(report.list[0].amount, "800.50");
        assert_eq!(report.list[1].instance_no, "P305-RPT-A");
        assert_eq!(report.list[1].amount, "5000");

        // biz_type 过滤真的生效（换一个业务类型 → 全 0，不串数据）
        let other = super::monthly_report_with(&all_visible(), &window, Some("OTHER_BIZ"), 1_000.0)
            .await
            .expect("报表查询");
        assert_eq!(other.self_approval_instances, 0);
        assert_eq!(other.settled_instances, 0);
        assert_eq!(other.blocked_count, 0);

        cleanup("rpt").await;
    }
}
