//! 审批委托 · 领域规则与数据访问（P3-04 / ARCH-8 第 ⑤ 层）。
//!
//! # 一句话
//!
//! **出差 / 休假的审批人把「这段时间的审批权」借给同事**，引擎在分配待办时把票
//! 派给受托人，并在待办行上留痕「代 A 审批（委托至 YYYY-MM-DD）」。
//!
//! 真正的**替换**发生在 [`crate::business::flow::assignee::apply_delegation`]
//! （解析期、filter 之后、升级链之前）；本文件负责：
//!
//! | 职责 | 函数 |
//! |------|------|
//! | 保存期校验（区间 / 自委托 / 重叠） | [`validate_window`] / [`overlap_of`] / [`check_overlap`] |
//! | 输入归一化（时间 / 范围 / 事由） | [`parse_time`] / [`normalize_scope`] / [`normalize_reason`] |
//! | CRUD（分页 / 详情 / 建 / 改 / 软删） | [`page`] / [`find`] / [`create`] / [`update`] / [`soft_delete`] |
//! | 并发收口 | [`lock_delegator`]（事务级 advisory lock） |
//!
//! # 为什么「同委托人重叠区间」必须拒（错误码 65043）
//!
//! A 同时委托给 B 和 C 时，「谁代 A」没有确定答案：引擎只能挑一档，另一档的受托人
//! 会一直等一个永远不会来的待办 ⇒ 单据堵到两个人都回来。这是**数据问题**，
//! 必须在保存期吵（422 + `65043`），而不是在运行期随机。
//!
//! 三道闸：
//!
//! 1. 应用层 [`overlap_of`]：同委托人、未软删（**停用的也算** —— 重新启用就会撞）、
//!    区间相交、排除自己这一行；
//! 2. 事务级 advisory lock [`lock_delegator`]：把「同一委托人的并发保存」串行化，
//!    关掉「两个请求同时读到无重叠再各自插入」的窗口；
//! 3. 库层 `EXCLUDE USING gist`（迁移 V0336，需要 `btree_gist`）：最后一道，
//!    绕过接口直接写库时也能拦住。
//!
//! # 委托不需要审批（ARCH-8 §8 待确认问题 4 的建议口径）
//!
//! 它是「本人对自己审批权的临时让渡」；再加一道审批只会把「A 出差」变成
//! 「A 和审批人一起出差」。留痕分两层：逐条待办的 `comment`（[`crate::business::flow::assignee::Delegation::comment`]）
//! 与配置变更审计（P3-05 统一接 `operation_logs`）。
//!
//! SCOPE-EXEMPT: 委托是**本人对自己审批权的配置**，不是「谁的业务数据」——
//! 列表按 `delegator_id` 收窄（缺省只看自己，管理员可看全部），
//! 与部门数据范围无关；`sys_user` 只用来取姓名快照。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;

/// 「管全部委托」的权限码（`:save` / `:delete` 的行级判定用它短路）。
pub const PERM_ADMIN: &str = "business:flow:admin";
/// `scope` 列宽（与迁移 V0336 的 `varchar(40)` 一致）。
pub const SCOPE_MAX_LEN: usize = 40;
/// `reason` 列宽（与迁移 V0336 的 `varchar(200)` 一致）。
pub const REASON_MAX_LEN: usize = 200;
/// 单条委托的最长跨度（天）。超过基本是「起始/结束填反了」或填错年份，
/// 与其让它悄悄生效三年，不如让用户确认一次。
pub const MAX_SPAN_DAYS: i64 = 366;

/// 一行委托（`biz_flow_delegate` + 两侧姓名快照）。
#[derive(Debug, Clone)]
pub struct DelegateRow {
    /// 主键。
    pub id: Uuid,
    /// 委托人。
    pub delegator_id: Uuid,
    /// 委托人姓名（用户已删 → 空串）。
    pub delegator_name: String,
    /// 受托人。
    pub delegate_id: Uuid,
    /// 受托人姓名（用户已删 → 空串）。
    pub delegate_name: String,
    /// 生效起点。
    pub start_at: chrono::DateTime<chrono::Utc>,
    /// 生效终点。
    pub end_at: chrono::DateTime<chrono::Utc>,
    /// 生效业务范围（`None` = 全部）。
    pub scope: Option<String>,
    /// 事由。
    pub reason: Option<String>,
    /// 启用开关。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl DelegateRow {
    /// 此刻是否真的在生效（前端标「生效中」；与引擎判据同一口径）。
    pub fn is_active_now(&self) -> bool {
        let now = chrono::Utc::now();
        self.enabled && self.start_at <= now && now <= self.end_at
    }
}

/// 分页过滤。
#[derive(Debug, Clone, Default)]
pub struct DelegateFilter {
    /// 只看某个委托人（`None` = 不过滤；管理员视角）。
    pub delegator_id: Option<Uuid>,
    /// 只列**此刻生效**的（`false` = 含已过期 / 未开始）。
    pub only_active: bool,
    /// 只看启用中的（`false` = 含已停用的，前端要能重新启用）。
    pub only_enabled: bool,
}

/// 分页结果。
#[derive(Debug, Clone)]
pub struct DelegatePage {
    /// 当前页。
    pub list: Vec<DelegateRow>,
    /// 总数。
    pub total: i64,
    /// 页码。
    pub page: i64,
    /// 页大小。
    pub size: i64,
}

/// 新建入参（已归一化）。
#[derive(Debug, Clone)]
pub struct NewDelegate {
    /// 主键。
    pub id: Uuid,
    /// 委托人。
    pub delegator_id: Uuid,
    /// 受托人。
    pub delegate_id: Uuid,
    /// 起点。
    pub start_at: chrono::DateTime<chrono::Utc>,
    /// 终点。
    pub end_at: chrono::DateTime<chrono::Utc>,
    /// 范围。
    pub scope: Option<String>,
    /// 事由。
    pub reason: Option<String>,
    /// 启用。
    pub enabled: bool,
}

/// 修改入参（全量覆盖式：前端表单总是提交整份）。
#[derive(Debug, Clone)]
pub struct DelegatePatch {
    /// 受托人。
    pub delegate_id: Uuid,
    /// 起点。
    pub start_at: chrono::DateTime<chrono::Utc>,
    /// 终点。
    pub end_at: chrono::DateTime<chrono::Utc>,
    /// 范围。
    pub scope: Option<String>,
    /// 事由。
    pub reason: Option<String>,
    /// 启用 / 停用。
    pub enabled: bool,
}

// ==================== 输入归一化 ====================

/// 解析时间字符串（前端三种形态都吃）。
///
/// | 输入 | 解释 |
/// |------|------|
/// | `2026-11-08T10:00:00+08:00`（RFC3339） | 按自带时区 |
/// | `2026-11-08 10:00:00` / `2026-11-08T10:00:00`（无时区） | 按 **Asia/Shanghai**（业务时区） |
/// | `2026-11-08`（只有日期） | 当天 00:00（Asia/Shanghai） |
///
/// # 异常情况
///
/// - `400 + 65005`：三种形态都解析不出来（文案点名字段，不静默当「现在」）。
pub fn parse_time(
    raw: &str,
    label: &str,
) -> std::result::Result<chrono::DateTime<chrono::Utc>, ApiError> {
    use chrono::{FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::FLOW_FIELD_INVALID,
            format!("{label}不能为空"),
        ));
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(text) {
        return Ok(parsed.with_timezone(&chrono::Utc));
    }
    let offset = FixedOffset::east_opt(8 * 3600).expect("+08:00 一定是合法偏移");
    for pattern in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, pattern)
            && let Some(at) = offset.from_local_datetime(&naive).single()
        {
            return Ok(at.with_timezone(&chrono::Utc));
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        let naive = date.and_hms_opt(0, 0, 0).expect("00:00:00 一定合法");
        if let Some(at) = offset.from_local_datetime(&naive).single() {
            return Ok(at.with_timezone(&chrono::Utc));
        }
    }
    Err(ApiError::new(
        400,
        error_code::FLOW_FIELD_INVALID,
        format!("{label}格式非法（支持 2026-11-08、2026-11-08 10:00、2026-11-08T10:00:00+08:00）"),
    ))
}

/// 归一化 `scope`：空 / 全空白 → `None`（全部业务）；否则大写 + 只允许 `A-Z 0-9 _ -`。
///
/// # 异常情况
///
/// - `400 + 65005`：长度超过 40 / 含非法字符。
pub fn normalize_scope(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > SCOPE_MAX_LEN {
        return Err(ApiError::new(
            400,
            error_code::FLOW_FIELD_INVALID,
            format!("生效范围过长（最多 {SCOPE_MAX_LEN} 个字符）"),
        ));
    }
    let upper = trimmed.to_ascii_uppercase();
    if !upper
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(ApiError::new(
            400,
            error_code::FLOW_FIELD_INVALID,
            "生效范围只能是业务类型编码（字母 / 数字 / 下划线 / 连字符，如 PURCHASE_ORDER）",
        ));
    }
    Ok(Some(upper))
}

/// 归一化事由：空白 → `None`；超长 → `400 + 65005`。
pub fn normalize_reason(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > REASON_MAX_LEN {
        return Err(ApiError::new(
            400,
            error_code::FLOW_FIELD_INVALID,
            format!("事由过长（最多 {REASON_MAX_LEN} 个字符）"),
        ));
    }
    Ok(Some(trimmed.to_string()))
}

// ==================== 保存期校验（65043） ====================

/// 区间与自委托的**纯**校验（不查库，可单测）。
///
/// # 异常情况
///
/// - `422 + 65043`：`end_at <= start_at` / 受托人 = 委托人 / 跨度超过 [`MAX_SPAN_DAYS`]。
pub fn validate_window(
    delegator_id: Uuid,
    delegate_id: Uuid,
    start_at: chrono::DateTime<chrono::Utc>,
    end_at: chrono::DateTime<chrono::Utc>,
) -> std::result::Result<(), ApiError> {
    if delegator_id == delegate_id {
        return Err(ApiError::new(
            422,
            error_code::FLOW_DELEGATE_INVALID,
            "受托人不能是委托人自己（自己代自己审批没有意义）",
        ));
    }
    if end_at <= start_at {
        return Err(ApiError::new(
            422,
            error_code::FLOW_DELEGATE_INVALID,
            "委托结束时间必须晚于开始时间",
        ));
    }
    let span = end_at - start_at;
    if span.num_days() > MAX_SPAN_DAYS {
        return Err(ApiError::new(
            422,
            error_code::FLOW_DELEGATE_INVALID,
            format!(
                "委托跨度 {} 天，超过上限 {MAX_SPAN_DAYS} 天（多半是起止日期填反 / 年份填错）",
                span.num_days()
            ),
        ));
    }
    Ok(())
}

/// 重叠校验的**纯**部分：把 [`overlap_of`] 的结果翻成 422 / 65043。
///
/// # 异常情况
///
/// - `422 + 65043`：与已有委托区间重叠（文案点名冲突区间，用户知道去改哪条）。
pub fn check_overlap(
    conflict: Option<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>,
) -> std::result::Result<(), ApiError> {
    let Some((start, end)) = conflict else {
        return Ok(());
    };
    Err(ApiError::new(
        422,
        error_code::FLOW_DELEGATE_INVALID,
        format!(
            "与你已有的委托区间重叠（{} → {}）。同一时间只能委托给一个人，\
             否则「谁代你审批」不确定 —— 请先停用 / 删除那条，或改成本段时间委托给他",
            format_biz_time(start),
            format_biz_time(end)
        ),
    ))
}

/// 把 UTC 时刻写成业务时区（Asia/Shanghai）的 `YYYY-MM-DD HH:MM`，只用于文案。
fn format_biz_time(at: chrono::DateTime<chrono::Utc>) -> String {
    use chrono::FixedOffset;
    let offset = FixedOffset::east_opt(8 * 3600).expect("+08:00 一定是合法偏移");
    at.with_timezone(&offset)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

// ==================== 并发收口 ====================

/// 串行化**同一委托人**的并发保存（事务级 advisory lock，事务结束自动释放）。
///
/// 为什么需要：`overlap_of` 是「先查后插」，两个请求同时通过检查时会各自插入一条重叠行。
/// 库层的 `EXCLUDE` 排除约束能兜住，但它依赖 `btree_gist` 扩展；
/// 扩展不可用的库（见迁移 V0336 的降级分支）就只剩这条锁。
pub async fn lock_delegator(ex: &impl SqlExecutor, delegator_id: Uuid) -> DbResult<()> {
    // 为什么要在外面套一层 `count(*)`：`pg_advisory_xact_lock` 返回 `void`，
    // 驱动读这一列会 `error deserializing column 0`（实测 500）。
    // 套一层只取 bigint 计数，函数副作用（拿锁）照常发生。
    ex.query_sql(
        "SELECT count(*)::bigint AS locked \
           FROM (SELECT pg_advisory_xact_lock(hashtext(?))) AS lock_row"
            .to_string(),
        vec![Value::Text(format!("biz_flow_delegate:{delegator_id}"))],
    )
    .await?;
    Ok(())
}

/// 查同委托人**已存在**的重叠区间（`None` = 不冲突）。
///
/// 判据：
/// - `deleted_at IS NULL`（软删的不算 —— 删掉就是为了腾出这段时间）；
/// - **不判 `enabled`**：停用的行重新启用就会撞，保存期一并拦（用户改时间而不是先启用）；
/// - `exclude`：改自己时排除自己那一行；
/// - 区间按 `[start, end)` 相交判（与迁移里的 `tstzrange(..., '[)')` 一致），
///   所以「上一档结束 = 下一档开始」是合法的无缝衔接。
pub async fn overlap_of(
    ex: &impl SqlExecutor,
    delegator_id: Uuid,
    start_at: chrono::DateTime<chrono::Utc>,
    end_at: chrono::DateTime<chrono::Utc>,
    exclude: Option<Uuid>,
) -> DbResult<Option<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>> {
    let mut sql = String::from(
        "SELECT start_at, end_at FROM biz_flow_delegate \
          WHERE deleted_at IS NULL \
            AND delegator_id = ?::uuid \
            AND start_at < ?::timestamptz AND end_at > ?::timestamptz",
    );
    let mut params: Vec<Value> = vec![
        Value::from(delegator_id),
        Value::from(end_at),
        Value::from(start_at),
    ];
    if let Some(exclude) = exclude {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(exclude));
    }
    sql.push_str(" ORDER BY start_at LIMIT 1");
    let rows = ex.query_sql(sql, params).await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some((row.get_typed("start_at")?, row.get_typed("end_at")?))),
    }
}

// ==================== CRUD ====================

/// 分页查询（缺省按开始时间倒序；含已停用行，前端要能重新启用）。
pub async fn page(
    ex: &impl SqlExecutor,
    filter: &DelegateFilter,
    page_no: u64,
    page_size: u64,
) -> DbResult<DelegatePage> {
    let mut where_sql = String::from(" WHERE d.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(delegator_id) = filter.delegator_id {
        where_sql.push_str(" AND d.delegator_id = ?::uuid");
        params.push(Value::from(delegator_id));
    }
    if filter.only_active {
        where_sql.push_str(" AND d.enabled AND now() BETWEEN d.start_at AND d.end_at");
    } else if filter.only_enabled {
        where_sql.push_str(" AND d.enabled");
    }

    let total: i64 = ex
        .scalar_sql(
            format!("SELECT count(*)::bigint FROM biz_flow_delegate d{where_sql}"),
            params.clone(),
        )
        .await?;

    let page_no = page_no.max(1) as i64;
    let page_size = page_size.clamp(1, 200) as i64;
    let offset = (page_no - 1) * page_size;
    let mut list_params = params;
    list_params.push(Value::I64(page_size));
    list_params.push(Value::I64(offset));
    let rows = ex
        .query_sql(
            format!(
                "SELECT d.id, d.delegator_id, d.delegate_id, d.start_at, d.end_at, d.scope, \
                        d.reason, d.enabled, d.created_at, d.updated_at, \
                        COALESCE(NULLIF(du.real_name, ''), du.username, '') AS delegator_name, \
                        COALESCE(NULLIF(tu.real_name, ''), tu.username, '') AS delegate_name \
                   FROM biz_flow_delegate d \
                   LEFT JOIN sys_user du ON du.id = d.delegator_id AND du.deleted_at IS NULL \
                   LEFT JOIN sys_user tu ON tu.id = d.delegate_id AND tu.deleted_at IS NULL \
                   {where_sql} \
                  ORDER BY d.start_at DESC, d.created_at DESC \
                  LIMIT ?::bigint OFFSET ?::bigint"
            ),
            list_params,
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(row_from(row)?);
    }
    Ok(DelegatePage {
        list,
        total,
        page: page_no,
        size: page_size,
    })
}

/// 按 id 查一行（软删的不返回）。
pub async fn find(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<DelegateRow>> {
    let rows = ex
        .query_sql(
            "SELECT d.id, d.delegator_id, d.delegate_id, d.start_at, d.end_at, d.scope, \
                    d.reason, d.enabled, d.created_at, d.updated_at, \
                    COALESCE(NULLIF(du.real_name, ''), du.username, '') AS delegator_name, \
                    COALESCE(NULLIF(tu.real_name, ''), tu.username, '') AS delegate_name \
               FROM biz_flow_delegate d \
               LEFT JOIN sys_user du ON du.id = d.delegator_id AND du.deleted_at IS NULL \
               LEFT JOIN sys_user tu ON tu.id = d.delegate_id AND tu.deleted_at IS NULL \
              WHERE d.id = ?::uuid AND d.deleted_at IS NULL"
                .to_string(),
            vec![Value::from(id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(row_from(row)?)),
    }
}

fn row_from(row: &Row) -> DbResult<DelegateRow> {
    Ok(DelegateRow {
        id: row.get_typed("id")?,
        delegator_id: row.get_typed("delegator_id")?,
        delegator_name: row.get_typed("delegator_name")?,
        delegate_id: row.get_typed("delegate_id")?,
        delegate_name: row.get_typed("delegate_name")?,
        start_at: row.get_typed("start_at")?,
        end_at: row.get_typed("end_at")?,
        scope: row.get_typed("scope")?,
        reason: row.get_typed("reason")?,
        enabled: row.get_typed("enabled")?,
        created_at: row.get_typed("created_at")?,
        updated_at: row.get_typed("updated_at")?,
    })
}

/// 建一条（调用方负责先跑 [`validate_window`] / [`check_overlap`] / [`lock_delegator`]）。
pub async fn create(ex: &impl SqlExecutor, new: &NewDelegate) -> DbResult<()> {
    ex.exec_sql(
        "INSERT INTO biz_flow_delegate \
             (id, delegator_id, delegate_id, start_at, end_at, scope, reason, enabled, \
              created_at, updated_at) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?::timestamptz, ?::timestamptz, \
                 NULLIF(?::text, ''), NULLIF(?::text, ''), ?, now(), now())"
            .to_string(),
        vec![
            Value::from(new.id),
            Value::from(new.delegator_id),
            Value::from(new.delegate_id),
            Value::from(new.start_at),
            Value::from(new.end_at),
            Value::Text(new.scope.clone().unwrap_or_default()),
            Value::Text(new.reason.clone().unwrap_or_default()),
            Value::Bool(new.enabled),
        ],
    )
    .await?;
    Ok(())
}

/// 改一条（全量覆盖；`delegator_id` 不可改 —— 委托人是这行的归属，改了等于换个人）。
pub async fn update(ex: &impl SqlExecutor, id: Uuid, patch: &DelegatePatch) -> DbResult<u64> {
    let affected = ex
        .exec_sql(
            "UPDATE biz_flow_delegate SET \
                 delegate_id = ?::uuid, \
                 start_at = ?::timestamptz, \
                 end_at = ?::timestamptz, \
                 scope = NULLIF(?::text, ''), \
                 reason = NULLIF(?::text, ''), \
                 enabled = ?, \
                 updated_at = now() \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::from(patch.delegate_id),
                Value::from(patch.start_at),
                Value::from(patch.end_at),
                Value::Text(patch.scope.clone().unwrap_or_default()),
                Value::Text(patch.reason.clone().unwrap_or_default()),
                Value::Bool(patch.enabled),
                Value::from(id),
            ],
        )
        .await?;
    Ok(affected)
}

/// 软删一条（历史留着：审计要能回答「A 曾经把票委托给谁」）。
pub async fn soft_delete(ex: &impl SqlExecutor, id: Uuid) -> DbResult<u64> {
    let affected = ex
        .exec_sql(
            "UPDATE biz_flow_delegate SET deleted_at = now(), enabled = FALSE, updated_at = now() \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(id)],
        )
        .await?;
    Ok(affected)
}

/// 受托人是否可用于代签（用户存在 / 未停用 / 不是机器人）。
///
/// 返回 `Some(原因)` = 不可用。**保存期**就拦：让用户在配的时候改，
/// 而不是等出差第一天单据发不出去才发现。
pub async fn delegate_unusable_reason(
    ex: &impl SqlExecutor,
    delegate_id: Uuid,
) -> DbResult<Option<String>> {
    let rows = ex
        .query_sql(
            "SELECT u.user_type AS user_type, (u.status = 1) AS active \
               FROM sys_user u WHERE u.id = ?::uuid AND u.deleted_at IS NULL"
                .to_string(),
            vec![Value::from(delegate_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(Some("受托人不存在（已删除或 id 非法）".to_string()));
    };
    let user_type: String = row.get_typed("user_type")?;
    let active: bool = row.get_typed("active")?;
    if user_type.eq_ignore_ascii_case("ROBOT") {
        return Ok(Some(
            "受托人不能是机器人（委托要的是能签字的人）".to_string(),
        ));
    }
    if !active {
        return Ok(Some("受托人已停用".to_string()));
    }
    Ok(None)
}

/// 批量取「这些用户里哪些可以代签」（一次性去重后逐个查，条数 ≤ 委托档数）。
pub async fn unusable_reasons(
    ex: &impl SqlExecutor,
    ids: &[Uuid],
) -> DbResult<std::collections::BTreeMap<Uuid, String>> {
    let mut unique: Vec<Uuid> = ids.to_vec();
    unique.sort();
    unique.dedup();
    let mut out = std::collections::BTreeMap::new();
    for id in unique {
        if let Some(reason) = delegate_unusable_reason(ex, id).await? {
            out.insert(id, reason);
        }
    }
    Ok(out)
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn at(y: i32, m: u32, d: u32) -> chrono::DateTime<chrono::Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    /// 三种时间形态都要吃；无时区的按 Asia/Shanghai 理解（UTC = 本地 - 8h）。
    #[test]
    fn parse_time_accepts_three_shapes() {
        let rfc = parse_time("2026-11-08T10:00:00+08:00", "开始时间").expect("RFC3339");
        assert_eq!(rfc, Utc.with_ymd_and_hms(2026, 11, 8, 2, 0, 0).unwrap());

        let naive = parse_time("2026-11-08 10:00:00", "开始时间").expect("naive");
        assert_eq!(naive, rfc, "无时区按 Asia/Shanghai（+08:00）折算");

        let date_only = parse_time("2026-11-08", "开始时间").expect("date only");
        assert_eq!(
            date_only,
            Utc.with_ymd_and_hms(2026, 11, 7, 16, 0, 0).unwrap()
        );

        let bad = parse_time("明天上午", "开始时间").expect_err("非法格式必须 400");
        assert_eq!(bad.status, 400);
        assert_eq!(bad.code, error_code::FLOW_FIELD_INVALID);
        assert!(bad.msg.contains("开始时间"), "报文要点名字段：{}", bad.msg);
    }

    /// scope：空 = 全部业务；大小写归一；非法字符 / 超长 → 400。
    #[test]
    fn normalize_scope_rules() {
        assert_eq!(normalize_scope(None).unwrap(), None);
        assert_eq!(normalize_scope(Some("   ")).unwrap(), None);
        assert_eq!(
            normalize_scope(Some(" purchase_order ")).unwrap(),
            Some("PURCHASE_ORDER".to_string())
        );
        assert_eq!(
            normalize_scope(Some("A".repeat(SCOPE_MAX_LEN + 1).as_str()))
                .expect_err("超长必须 400")
                .code,
            error_code::FLOW_FIELD_INVALID
        );
        assert_eq!(
            normalize_scope(Some("采购单"))
                .expect_err("中文不是业务类型编码")
                .code,
            error_code::FLOW_FIELD_INVALID
        );
    }

    /// 事由：空白 → None；超长 → 400。
    #[test]
    fn normalize_reason_rules() {
        assert_eq!(normalize_reason(None).unwrap(), None);
        assert_eq!(normalize_reason(Some("  ")).unwrap(), None);
        assert_eq!(
            normalize_reason(Some(" 出差北京 ")).unwrap(),
            Some("出差北京".to_string())
        );
        assert_eq!(
            normalize_reason(Some("x".repeat(REASON_MAX_LEN + 1).as_str()))
                .expect_err("超长必须 400")
                .code,
            error_code::FLOW_FIELD_INVALID
        );
    }

    /// 卡面验收 ④（纯逻辑半边）：自委托与区间非法 → 422 + 65043。
    #[test]
    fn validate_window_rejects_self_and_bad_period() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);

        let self_delegate =
            validate_window(a, a, at(2026, 11, 8), at(2026, 11, 11)).expect_err("自委托必须拒");
        assert_eq!(self_delegate.code, error_code::FLOW_DELEGATE_INVALID);
        assert_eq!(self_delegate.code, 65043);
        assert!(self_delegate.msg.contains("不能是委托人自己"));

        let reversed = validate_window(a, b, at(2026, 11, 11), at(2026, 11, 8))
            .expect_err("结束早于开始必须拒");
        assert_eq!(reversed.code, 65043);

        let zero = validate_window(a, b, at(2026, 11, 8), at(2026, 11, 8))
            .expect_err("零长度区间必须拒（end_at > start_at）");
        assert_eq!(zero.code, 65043);

        assert!(validate_window(a, b, at(2026, 11, 8), at(2026, 11, 11)).is_ok());
    }

    /// 跨度上限：366 天以内放行，超过即视为填错。
    #[test]
    fn validate_window_caps_span() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        assert!(validate_window(a, b, at(2026, 1, 1), at(2027, 1, 1)).is_ok());
        let too_long =
            validate_window(a, b, at(2026, 1, 1), at(2028, 1, 1)).expect_err("两年必须拒");
        assert_eq!(too_long.code, 65043);
        assert!(too_long.msg.contains("跨度"));
    }

    /// 重叠文案要点名冲突区间（用户要知道去改哪条）。
    #[test]
    fn check_overlap_names_conflict_window() {
        assert!(check_overlap(None).is_ok());
        let error =
            check_overlap(Some((at(2026, 11, 8), at(2026, 11, 11)))).expect_err("有重叠必须 65043");
        assert_eq!(error.code, 65043);
        assert!(
            error.msg.contains("2026-11-08 08:00"),
            "报文：{}",
            error.msg
        );
        assert!(error.msg.contains("重叠"));
    }

    /// 「生效中」判据：停用 / 未开始 / 已过期都不算。
    #[test]
    fn is_active_now_covers_three_states() {
        let now = chrono::Utc::now();
        let mut row = DelegateRow {
            id: Uuid::from_u128(9),
            delegator_id: Uuid::from_u128(1),
            delegator_name: "A".to_string(),
            delegate_id: Uuid::from_u128(2),
            delegate_name: "B".to_string(),
            start_at: now - chrono::Duration::hours(1),
            end_at: now + chrono::Duration::hours(1),
            scope: None,
            reason: None,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        assert!(row.is_active_now());
        row.enabled = false;
        assert!(!row.is_active_now(), "停用不算生效");
        row.enabled = true;
        row.start_at = now + chrono::Duration::hours(1);
        assert!(!row.is_active_now(), "未开始不算生效");
        row.start_at = now - chrono::Duration::hours(2);
        row.end_at = now - chrono::Duration::hours(1);
        assert!(!row.is_active_now(), "已过期不算生效");
    }
}

// ==================== 真库单测（P3-04 · 卡面验收 ④ 与 CRUD 往返） ====================
//
// 靠 `ERP_DB_URL` 开关：不设就打印 SKIP 并返回。
// 覆盖：
//   ④ 同委托人重叠区间 → 422 / 65043（含「跨端点相接不算重叠」「软删后可复用这段时间」
//      「改自己时排除自己」「停用的行也算重叠」四条负例/正例）；
//   CRUD 往返：建 → 查 → 改 → 软删 → 查不到；
//   受托人可用性：停用 / 不存在 都要给中文原因。
#[cfg(test)]
mod db_tests {
    use super::*;

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

    async fn exec(sql: &str) {
        if let Err(err) = db().execute_with_params(sql, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    struct Fixture {
        delegator: Uuid,
        delegate: Uuid,
        stopped: Uuid,
    }

    async fn setup(tag: &str) -> Fixture {
        for sql in [
            format!(
                "DELETE FROM biz_flow_delegate WHERE delegator_id IN \
                 (SELECT id FROM sys_user WHERE username LIKE 'p304dl\\_%\\_{tag}')"
            ),
            format!("DELETE FROM sys_user WHERE username LIKE 'p304dl\\_%\\_{tag}'"),
        ] {
            exec(&sql).await;
        }
        let delegator = Uuid::now_v7();
        let delegate = Uuid::now_v7();
        let stopped = Uuid::now_v7();
        exec(&format!(
            "INSERT INTO sys_user (id, username, password, real_name, status) VALUES \
             ('{delegator}'::uuid, 'p304dl_a_{tag}', 'x', 'P3-04 委托人', 1), \
             ('{delegate}'::uuid, 'p304dl_b_{tag}', 'x', 'P3-04 受托人', 1), \
             ('{stopped}'::uuid, 'p304dl_s_{tag}', 'x', 'P3-04 停用人', 0)"
        ))
        .await;
        Fixture {
            delegator,
            delegate,
            stopped,
        }
    }

    async fn teardown(fx: &Fixture) {
        exec(&format!(
            "DELETE FROM biz_flow_delegate WHERE delegator_id = '{0}'::uuid",
            fx.delegator
        ))
        .await;
        exec(&format!(
            "DELETE FROM sys_user WHERE id IN ('{0}'::uuid,'{1}'::uuid,'{2}'::uuid)",
            fx.delegator, fx.delegate, fx.stopped
        ))
        .await;
    }

    fn at(y: i32, m: u32, d: u32) -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono::Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    async fn insert_window(
        fx: &Fixture,
        delegate: Uuid,
        start: chrono::DateTime<chrono::Utc>,
        end: chrono::DateTime<chrono::Utc>,
        enabled: bool,
    ) -> Uuid {
        let id = Uuid::now_v7();
        let new = NewDelegate {
            id,
            delegator_id: fx.delegator,
            delegate_id: delegate,
            start_at: start,
            end_at: end,
            scope: None,
            reason: Some("P3-04 夹具".to_string()),
            enabled,
        };
        create(db(), &new).await.expect("插入委托");
        id
    }

    /// 卡面验收 ④：重叠 → 65043；相邻 / 排除自己 / 软删后 都不算重叠。
    #[tokio::test]
    async fn overlap_is_rejected_with_65043() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("a").await;
        let first = insert_window(&fx, fx.delegate, at(2027, 1, 10), at(2027, 1, 20), true).await;

        // ① 完全包含 → 重叠
        let inside = overlap_of(db(), fx.delegator, at(2027, 1, 12), at(2027, 1, 15), None)
            .await
            .expect("查重叠");
        let error = check_overlap(inside).expect_err("重叠必须 65043");
        assert_eq!(error.status, 422);
        assert_eq!(error.code, error_code::FLOW_DELEGATE_INVALID);
        assert_eq!(error.code, 65043);

        // ② 左搭边（end == 已有 start）→ 不算重叠（[start, end) 半开区间）
        assert!(
            overlap_of(db(), fx.delegator, at(2027, 1, 5), at(2027, 1, 10), None)
                .await
                .expect("查重叠")
                .is_none(),
            "跨端点相接必须放行（否则「无缝接力」配不出来）"
        );

        // ③ 右搭边同理
        assert!(
            overlap_of(db(), fx.delegator, at(2027, 1, 20), at(2027, 1, 25), None)
                .await
                .expect("查重叠")
                .is_none()
        );

        // ④ 改自己：排除自己那一行 → 不重叠
        assert!(
            overlap_of(
                db(),
                fx.delegator,
                at(2027, 1, 10),
                at(2027, 1, 20),
                Some(first)
            )
            .await
            .expect("查重叠")
            .is_none(),
            "改自己（区间不变）不能被自己判成重叠"
        );

        // ⑤ 停用的行也算重叠（重新启用就会撞）
        let disabled =
            insert_window(&fx, fx.delegate, at(2027, 2, 1), at(2027, 2, 10), false).await;
        assert!(
            overlap_of(db(), fx.delegator, at(2027, 2, 5), at(2027, 2, 8), None)
                .await
                .expect("查重叠")
                .is_some(),
            "停用的行重新启用就会撞，保存期一并拦"
        );
        exec(&format!(
            "UPDATE biz_flow_delegate SET deleted_at = now() WHERE id = '{disabled}'::uuid"
        ))
        .await;

        // ⑥ 软删后那段时间可以复用
        exec(&format!(
            "UPDATE biz_flow_delegate SET deleted_at = now() WHERE id = '{first}'::uuid"
        ))
        .await;
        assert!(
            overlap_of(db(), fx.delegator, at(2027, 1, 10), at(2027, 1, 20), None)
                .await
                .expect("查重叠")
                .is_none(),
            "软删的就是为了腾出这段时间"
        );
        teardown(&fx).await;
    }

    /// 库层 EXCLUDE 约束必须真的存在（btree_gist 可用的库），否则只剩应用层闸。
    #[tokio::test]
    async fn gist_exclusion_constraint_exists() {
        if !ensure_db().await {
            return;
        }
        let has_constraint: bool = db()
            .scalar_with_params(
                "SELECT EXISTS (SELECT 1 FROM pg_constraint \
                  WHERE conname = 'ex_biz_flow_delegate_no_overlap')",
                vec![],
            )
            .await
            .unwrap_or(false);
        let has_gist: bool = db()
            .scalar_with_params(
                "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'btree_gist')",
                vec![],
            )
            .await
            .unwrap_or(false);
        if has_gist {
            assert!(
                has_constraint,
                "btree_gist 可用时迁移必须建 ex_biz_flow_delegate_no_overlap"
            );
        } else {
            println!("SKIP: 本库没有 btree_gist（迁移走了降级分支，重叠只靠应用层闸）");
        }
    }

    /// CRUD 往返：建 → 查（含姓名快照 / 生效中）→ 改 → 软删 → 查不到。
    #[tokio::test]
    async fn crud_roundtrip() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("b").await;
        let now = chrono::Utc::now();
        let id = Uuid::now_v7();
        create(
            db(),
            &NewDelegate {
                id,
                delegator_id: fx.delegator,
                delegate_id: fx.delegate,
                start_at: now - chrono::Duration::hours(1),
                end_at: now + chrono::Duration::hours(1),
                scope: Some("PURCHASE_ORDER".to_string()),
                reason: Some("出差".to_string()),
                enabled: true,
            },
        )
        .await
        .expect("插入");
        let row = find(db(), id).await.expect("查询").expect("必须查得到");
        assert_eq!(row.delegator_name, "P3-04 委托人");
        assert_eq!(row.delegate_name, "P3-04 受托人");
        assert_eq!(row.scope.as_deref(), Some("PURCHASE_ORDER"));
        assert!(row.is_active_now(), "起止包住现在 → 生效中");

        update(
            db(),
            id,
            &DelegatePatch {
                delegate_id: fx.delegate,
                start_at: now,
                end_at: now + chrono::Duration::days(2),
                scope: None,
                reason: None,
                enabled: false,
            },
        )
        .await
        .expect("更新");
        let row = find(db(), id).await.expect("查询").expect("必须还在");
        assert!(!row.enabled && row.scope.is_none(), "改 / 停用要落地");
        assert!(!row.is_active_now(), "停用后不算生效");

        soft_delete(db(), id).await.expect("软删");
        assert!(
            find(db(), id).await.expect("查询").is_none(),
            "软删后不该再查得到"
        );
        teardown(&fx).await;
    }

    /// 受托人可用性：停用 / 不存在 → 中文原因（保存期据此报 400）。
    #[tokio::test]
    async fn unusable_delegate_reasons() {
        if !ensure_db().await {
            return;
        }
        let fx = setup("c").await;
        assert_eq!(
            delegate_unusable_reason(db(), fx.delegate)
                .await
                .expect("查询"),
            None,
            "在职真人必须可用"
        );
        let stopped = delegate_unusable_reason(db(), fx.stopped)
            .await
            .expect("查询")
            .expect("停用必须不可用");
        assert!(stopped.contains("已停用"), "{stopped}");
        let ghost = delegate_unusable_reason(db(), Uuid::now_v7())
            .await
            .expect("查询")
            .expect("不存在必须不可用");
        assert!(ghost.contains("不存在"), "{ghost}");
        teardown(&fx).await;
    }
}

// ==================== 顾问锁：驱动可读性（实测 500 的根因回归） ====================
//
// `pg_advisory_xact_lock()` 返回 **void**；直接 `SELECT pg_advisory_xact_lock(...)`
// 会让驱动在 `error deserializing column 0` 上炸成 500（真 HTTP 验收时踩到）。
// `lock_delegator` 用 `count(*)` 套一层把 void 吃掉 —— 这条用例钉住「能读回」。
#[cfg(test)]
mod lock_tests {
    use super::*;

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

    /// 事务级顾问锁：拿到锁、读回不报错、事务结束自动释放。
    #[tokio::test]
    async fn advisory_lock_is_readable_by_driver() {
        if !ensure_db().await {
            return;
        }
        let delegator = Uuid::now_v7();
        let tx = db().begin().await.expect("开事务");
        lock_delegator(&tx, delegator)
            .await
            .expect("拿锁 + 读回都必须成功（void 列要吃干净）");
        // 同一事务里重复拿同一把锁是幂等的（事务级锁可重入）
        lock_delegator(&tx, delegator).await.expect("可重入");
        tx.rollback().await.expect("回滚释放锁");
    }
}
