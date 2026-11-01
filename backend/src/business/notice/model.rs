//! `sys_notice` / `sys_notice_receiver` / `sys_notice_template` / `sys_notice_delivery`
//! 的强类型模型与数据访问。
//!
//! # 为什么不用 `#[derive(Model)]`
//!
//! 这四张表里有 4 个 `JSONB` 列（`target_ids` / `template_vars` / `variables`），
//! 而 derive 的读写路径不做 `::text` 显式转换，遇到 `Value::Null` / 类型推断差异时
//! 报错信息很难定位。这里统一走**显式 SQL**（与 `business/impexp/model.rs` 同款）：
//!
//! - `SELECT` 时把 JSONB 列写成 `xxx::text AS xxx`，Rust 侧拿到 `String` 再按需解析；
//! - 绑定可空参数一律用 `NULLIF(?::text, '')::<目标类型>` 的写法，
//!   **不把 `Value::Null` 绑到强类型占位符**（PG 驱动按 Unknown 类型编码会报
//!   `error serializing parameter` —— impexp 已踩过，这里沿用同一套规避写法）。
//!
//! # 冗余计数的一致性
//!
//! `sys_notice.receiver_count` 在扇出后由 [`refresh_receiver_count`] 用一条
//! `UPDATE ... = (SELECT count(*) ...)` 回写；`read_count` 在已读路径上按
//! `LEAST(receiver_count, read_count + n)` 递增，保证 `ck_sys_notice_counts`
//! （`read_count <= receiver_count`）永不被破。

use nik::prelude::*;

use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;
use crate::shared::constants::EnableStatus;
use crate::utils;

/// 通知主体可空 UUID 列（发送人）。
pub type MaybeId = Option<uuid::Uuid>;

/// 写操作执行器：同一段 SQL 既能在连接池上跑，也能在事务里跑。
///
/// # 为什么需要它
///
/// `dispatch` 必须把「落 notice → 扇出 → 回写计数 → 记投递」放进**同一个事务**：
/// 否则「通知建了但没人收到」或「只收到一半就失败」都会留下脏数据。而框架的
/// `Db` / `Transaction` 是两套**同名固有方法**（没有公共 trait），用这个枚举做一层
/// 极薄的转发，SQL 仍然只写一份，不会出现「事务版 / 非事务版两套 SQL 漂移」。
pub enum Executor<'a> {
    /// 连接池（自动提交）。
    Pool,
    /// 外部事务。
    Tx(&'a Transaction),
}

impl Executor<'_> {
    /// 执行写语句，返回受影响行数。
    pub async fn exec(&self, sql: &str, params: Vec<Value>) -> DbResult<u64> {
        match self {
            Executor::Pool => db().execute_with_params(sql, params).await,
            Executor::Tx(tx) => tx.execute_with_params(sql, params).await,
        }
    }

    /// 执行查询，返回行集合。
    pub async fn rows(&self, sql: &str, params: Vec<Value>) -> DbResult<Vec<Row>> {
        match self {
            Executor::Pool => db().find_with_params(sql, params).await,
            Executor::Tx(tx) => tx.find_with_params(sql, params).await,
        }
    }
}

/// `sys_notice` 行（列表 / 详情用）。
#[derive(Debug, Clone)]
pub struct SysNoticeModel {
    /// 主键。
    pub id: uuid::Uuid,
    /// 通知编号。
    pub notice_no: String,
    /// 通知类型。
    pub notice_type: String,
    /// 级别。
    pub level: String,
    /// 标题。
    pub title: String,
    /// 正文。
    pub content: String,
    /// 正文格式。
    pub content_format: String,
    /// 跳转链接。
    pub link_url: Option<String>,
    /// 业务类型。
    pub biz_type: Option<String>,
    /// 业务主键。
    pub biz_id: Option<String>,
    /// 模板编码。
    pub template_code: Option<String>,
    /// 发送人 ID。
    pub sender_id: MaybeId,
    /// 发送人名称。
    pub sender_name: Option<String>,
    /// 目标类型。
    pub target_type: String,
    /// 目标 ID 列表（JSONB 解析后的结果；`ALL` 为空）。
    pub target_ids: Vec<String>,
    /// 通道掩码。
    pub channel_mask: i16,
    /// 接收人数。
    pub receiver_count: i32,
    /// 已读人数。
    pub read_count: i32,
    /// 状态（1 正常 / 0 停用）。
    pub status: i16,
    /// 过期时间。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 计划发送时间（`None` = 立即发送；V0051）。
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 投递状态（`PENDING` 待发 / `SENT` 已投递 / `FAILED` 重试耗尽 / `CANCELED`；V0051）。
    pub dispatch_status: String,
    /// 定时投递尝试次数（V0051）。
    pub dispatch_attempts: i16,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// 软删时间。
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `sys_notice_template` 行。
#[derive(Debug, Clone)]
pub struct SysNoticeTemplateModel {
    /// 主键。
    pub id: uuid::Uuid,
    /// 模板编码。
    pub template_code: String,
    /// 模板名称。
    pub template_name: String,
    /// 通知类型。
    pub notice_type: String,
    /// 默认级别。
    pub default_level: String,
    /// 标题模板。
    pub title_template: String,
    /// 正文模板。
    pub content_template: String,
    /// 正文格式。
    pub content_format: String,
    /// 链接模板。
    pub link_template: Option<String>,
    /// 默认通道（CSV）。
    pub channels: String,
    /// 变量声明原文（`variables::text`，由 DTO 层解析）。
    pub variables_json: Option<String>,
    /// 状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// 软删时间。
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 「我的通知」列表行（接收行 + 通知主体联表）。
#[derive(Debug, Clone)]
pub struct MyNoticeRow {
    /// 接收行主键。
    pub receiver_id: uuid::Uuid,
    /// 通知主键。
    pub notice_id: uuid::Uuid,
    /// 通知编号。
    pub notice_no: String,
    /// 通知类型。
    pub notice_type: String,
    /// 级别。
    pub level: String,
    /// 标题。
    pub title: String,
    /// 正文。
    pub content: String,
    /// 正文格式。
    pub content_format: String,
    /// 跳转链接。
    pub link_url: Option<String>,
    /// 是否已读。
    pub read_flag: bool,
    /// 已读时间。
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 送达时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 某条通知的接收人统计（实时口径，与冗余计数交叉校验）。
#[derive(Debug, Clone, Copy, Default)]
pub struct ReceiverStats {
    /// 接收总行数。
    pub total: i64,
    /// 已读行数。
    pub read_count: i64,
    /// 用户侧已删除行数。
    pub deleted_count: i64,
}

/// 投递记录汇总行（`GROUP BY channel, status`）。
#[derive(Debug, Clone)]
pub struct DeliverySummaryRow {
    /// 通道。
    pub channel: String,
    /// 状态。
    pub status: String,
    /// 行数。
    pub count: i64,
}

/// 新建通知的入参（字段多，用结构体避免 `clippy::too_many_arguments`）。
#[derive(Debug)]
pub struct NewNotice<'a> {
    /// 主键。
    pub id: uuid::Uuid,
    /// 通知编号。
    pub notice_no: &'a str,
    /// 通知类型。
    pub notice_type: &'a str,
    /// 级别。
    pub level: &'a str,
    /// 标题。
    pub title: &'a str,
    /// 正文。
    pub content: &'a str,
    /// 正文格式。
    pub content_format: &'a str,
    /// 跳转链接。
    pub link_url: Option<&'a str>,
    /// 业务类型。
    pub biz_type: Option<&'a str>,
    /// 业务主键。
    pub biz_id: Option<&'a str>,
    /// 模板编码。
    pub template_code: Option<&'a str>,
    /// 模板变量（JSON 文本）。
    pub template_vars_json: Option<String>,
    /// 发送人 ID。
    pub sender_id: MaybeId,
    /// 发送人名称。
    pub sender_name: Option<&'a str>,
    /// 目标类型。
    pub target_type: &'a str,
    /// 目标 ID（JSON 数组文本）。
    pub target_ids_json: String,
    /// 通道掩码。
    pub channel_mask: i16,
    /// 幂等键。
    pub idempotency_key: Option<&'a str>,
    /// 过期时间。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 计划发送时间（`None` = 立即发送；V0051）。
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 投递状态（立即发送为 `SENT`，定时为 `PENDING`；V0051）。
    pub dispatch_status: &'a str,
    /// 操作人。
    pub operator: MaybeId,
}

/// 新建 / 编辑模板的入参。
#[derive(Debug)]
pub struct TemplateFields<'a> {
    /// 模板编码。
    pub template_code: &'a str,
    /// 模板名称。
    pub template_name: &'a str,
    /// 通知类型。
    pub notice_type: &'a str,
    /// 默认级别。
    pub default_level: &'a str,
    /// 标题模板。
    pub title_template: &'a str,
    /// 正文模板。
    pub content_template: &'a str,
    /// 正文格式。
    pub content_format: &'a str,
    /// 链接模板。
    pub link_template: Option<&'a str>,
    /// 默认通道（CSV）。
    pub channels: &'a str,
    /// 变量声明（JSON 文本）。
    pub variables_json: Option<String>,
    /// 状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<&'a str>,
    /// 操作人。
    pub operator: MaybeId,
}

/// 通知主体的 SELECT 列（JSONB 列转文本，见文件头说明）。
const NOTICE_COLUMNS: &str = "id, notice_no, notice_type, level, title, content, content_format, \
     link_url, biz_type, biz_id, template_code, sender_id, sender_name, target_type, \
     target_ids::text AS target_ids, channel_mask, receiver_count, read_count, status, \
     expire_at, scheduled_at, dispatch_status, dispatch_attempts, \
     created_at, updated_at, deleted_at";

/// 模板的 SELECT 列。
const TEMPLATE_COLUMNS: &str = "id, template_code, template_name, notice_type, default_level, \
     title_template, content_template, content_format, link_template, channels, \
     variables::text AS variables_json, status, remark, created_at, updated_at, deleted_at";

// ==================== 通知主体 ====================

/// 写入一条通知（幂等键冲突时**不插入**并返回 `false`）。
///
/// # 参数
///
/// - `new`：通知字段。
///
/// # 返回值
///
/// 真正插入返回 `true`；命中 `uk_sys_notice_idem` 返回 `false`（调用方据此判定重放）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn insert_notice(ex: &Executor<'_>, new: &NewNotice<'_>) -> DbResult<bool> {
    let affected = ex
        .exec(
            "INSERT INTO sys_notice (id, notice_no, notice_type, level, title, content, \
             content_format, link_url, biz_type, biz_id, template_code, template_vars, sender_id, \
             sender_name, target_type, target_ids, channel_mask, receiver_count, read_count, \
             idempotency_key, status, expire_at, scheduled_at, dispatch_status, \
             create_by, update_by) \
             VALUES (?::uuid, ?, ?, ?, ?, ?, ?, NULLIF(?::text, ''), NULLIF(?::text, ''), \
             NULLIF(?::text, ''), NULLIF(?::text, ''), ?::jsonb, \
             NULLIF(?::text, '')::uuid, NULLIF(?::text, ''), ?, ?::jsonb, ?, 0, 0, \
             NULLIF(?::text, ''), 1, NULLIF(?::text, '')::timestamptz, \
             NULLIF(?::text, '')::timestamptz, ?, \
             NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid) \
             ON CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING",
            vec![
                Value::from(new.id),
                Value::from(new.notice_no),
                Value::from(new.notice_type),
                Value::from(new.level),
                Value::from(new.title),
                Value::from(new.content),
                Value::from(new.content_format),
                Value::from(new.link_url.unwrap_or("")),
                Value::from(new.biz_type.unwrap_or("")),
                Value::from(new.biz_id.unwrap_or("")),
                Value::from(new.template_code.unwrap_or("")),
                // JSONB 列必须用 `Value::Json`（驱动按 jsonb 类型编码）；
                // 空值用 `{}` 而不是 SQL NULL —— 语义等价且绕开「Null 绑强类型占位符」的编码失败
                Value::Json(
                    new.template_vars_json
                        .clone()
                        .unwrap_or_else(|| "{}".to_string()),
                ),
                Value::from(opt_uuid_text(new.sender_id)),
                Value::from(new.sender_name.unwrap_or("")),
                Value::from(new.target_type),
                Value::Json(new.target_ids_json.clone()),
                TypeMapper::to_value(&new.channel_mask),
                Value::from(new.idempotency_key.unwrap_or("")),
                Value::from(opt_time_text(new.expire_at)),
                Value::from(opt_time_text(new.scheduled_at)),
                Value::from(new.dispatch_status),
                Value::from(opt_uuid_text(new.operator)),
                Value::from(opt_uuid_text(new.operator)),
            ],
        )
        .await?;
    Ok(affected > 0)
}

/// 按主键查通知（未软删）。
///
/// # 返回值
///
/// 命中返回模型，未命中 / 已软删返回 `None`。
pub async fn find_notice(id: &uuid::Uuid) -> DbResult<Option<SysNoticeModel>> {
    let sql = format!(
        "SELECT {NOTICE_COLUMNS} FROM sys_notice WHERE id = ?::uuid AND deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(*id)]).await?;
    rows.first().map(map_notice).transpose()
}

/// 按幂等键查通知（重放时回吐首次结果用；`ex` 可传事务以读到未提交行）。
pub async fn find_notice_by_idempotency(
    ex: &Executor<'_>,
    key: &str,
) -> DbResult<Option<SysNoticeModel>> {
    let sql = format!(
        "SELECT {NOTICE_COLUMNS} FROM sys_notice \
         WHERE idempotency_key = ? AND deleted_at IS NULL LIMIT 1"
    );
    let rows = ex.rows(&sql, vec![Value::from(key)]).await?;
    rows.first().map(map_notice).transpose()
}

// ==================== 定时发送（V0051） ====================

/// 取一批「到点待发」的通知 ID（巡逻任务用，**不加锁**，真正的抢占在
/// [`lock_due_notice`] 里做）。
///
/// # 参数
///
/// - `limit`：单批条数。
///
/// # 返回值
///
/// 按 `scheduled_at` 升序的 ID（最早的先发）。
///
/// # 说明
///
/// 只读判定条件与 [`lock_due_notice`] 保持一致（`PENDING` + 已到点 + 未软删），
/// 命中部分索引 `idx_sys_notice_due`。
pub async fn find_due_notice_ids(limit: i64) -> DbResult<Vec<uuid::Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT id FROM sys_notice \
             WHERE dispatch_status = 'PENDING' AND scheduled_at IS NOT NULL \
               AND scheduled_at <= CURRENT_TIMESTAMP AND deleted_at IS NULL \
             ORDER BY scheduled_at ASC LIMIT ?",
            vec![Value::from(limit)],
        )
        .await?;
    rows.iter().map(|row| row.get_typed("id")).collect()
}

/// 在事务内抢占一条到点通知（`FOR UPDATE SKIP LOCKED`，并发巡逻不会重复投递）。
///
/// # 参数
///
/// - `ex`：执行器（**必须是事务**，否则 `FOR UPDATE` 报错）。
/// - `id`：通知 ID。
///
/// # 返回值
///
/// 命中（仍是 `PENDING` 且已到点）返回模型；已被别的实例抢走 / 已被取消 / 未到点
/// 返回 `None`（调用方直接跳过）。
pub async fn lock_due_notice(
    ex: &Executor<'_>,
    id: &uuid::Uuid,
) -> DbResult<Option<SysNoticeModel>> {
    let sql = format!(
        "SELECT {NOTICE_COLUMNS} FROM sys_notice \
         WHERE id = ?::uuid AND dispatch_status = 'PENDING' \
           AND scheduled_at IS NOT NULL AND scheduled_at <= CURRENT_TIMESTAMP \
           AND deleted_at IS NULL \
         LIMIT 1 FOR UPDATE SKIP LOCKED"
    );
    let rows = ex.rows(&sql, vec![Value::from(*id)]).await?;
    rows.first().map(map_notice).transpose()
}

/// 投递完成：`dispatch_status` 置 `SENT`。
///
/// # 参数
///
/// - `ex`：执行器（与扇出 / 投递同事务）。
/// - `id`：通知 ID。
///
/// # 返回值
///
/// 受影响行数（0 表示状态已不是 `PENDING`，说明被并发改过）。
pub async fn mark_notice_dispatched(ex: &Executor<'_>, id: &uuid::Uuid) -> DbResult<u64> {
    ex.exec(
        "UPDATE sys_notice SET dispatch_status = 'SENT', updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND dispatch_status = 'PENDING'",
        vec![Value::from(*id)],
    )
    .await
}

/// 记录一次定时投递失败：尝试次数 +1，达到上限置 `FAILED` 不再重试。
///
/// # 参数
///
/// - `id`：通知 ID。
/// - `max_attempts`：失败上限（含本次）。
///
/// # 返回值
///
/// 受影响行数（0 = 行已不是 `PENDING`，无需计入）。
///
/// # 说明
///
/// 在**失败事务之外**单独执行（失败会回滚事务，计数器必须落得住，否则同一行会被
/// 每个调度周期重试到天荒地老）。
pub async fn record_dispatch_failure(id: &uuid::Uuid, max_attempts: i16) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE sys_notice \
         SET dispatch_attempts = dispatch_attempts + 1, \
             dispatch_status = CASE WHEN dispatch_attempts + 1 >= ? THEN 'FAILED' \
                                    ELSE dispatch_status END, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND dispatch_status = 'PENDING'",
        vec![TypeMapper::to_value(&max_attempts), Value::from(*id)],
    )
    .await
}

/// 把「到点前被软删」的定时通知标成 `CANCELED`（巡逻任务顺带收尾，幂等）。
///
/// # 参数
///
/// - `limit`：单批条数。
///
/// # 返回值
///
/// 本次置位的行数。
pub async fn cancel_soft_deleted_scheduled(limit: i64) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE sys_notice SET dispatch_status = 'CANCELED', updated_at = CURRENT_TIMESTAMP \
         WHERE id IN (SELECT id FROM sys_notice \
                      WHERE dispatch_status = 'PENDING' AND deleted_at IS NOT NULL \
                      LIMIT ?)",
        vec![Value::from(limit)],
    )
    .await
}

/// 通知主体分页（管理视角，按创建时间倒序）。
///
/// # 参数
///
/// - `filter`：关键词 / 类型 / 级别 / 业务类型。
/// - `page` / `page_size`：分页。
pub async fn page_notices(
    filter: &crate::business::notice::dto::NoticeFilter,
    page: u64,
    page_size: u64,
    principal: &ScopedPrincipal,
) -> DbResult<Page<SysNoticeModel>> {
    // 批次 B · B5（附录 C #14）：`sys_notice` 管理视角列表按**发送人**收口。
    // 此前 `page_notices` 的过滤条件只有关键词/类型/级别/业务类型，没有任何范围约束 ——
    // 任何持 `business:notice:list` 的人都能看到全站公告（含未发给自己的）。
    // `sys_notice` 无部门列，部门类范围在 `filter_for` 里 fail-closed 降级为「仅本人」
    // （`AND n.sender_id = ?`）并置 `is_degraded()`；片段只锚定主体表 `n`。
    let mut sql = format!("SELECT {NOTICE_COLUMNS} FROM sys_notice n WHERE n.deleted_at IS NULL");
    sql.push_str(scope::FILTER_MARKER);
    let mut params: Vec<Value> = Vec::new();

    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(" AND (n.title LIKE ? OR n.notice_no LIKE ? OR n.content LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(notice_type) = &filter.notice_type {
        sql.push_str(" AND n.notice_type = ?");
        params.push(Value::from(notice_type.clone()));
    }
    if let Some(level) = &filter.level {
        sql.push_str(" AND n.level = ?");
        params.push(Value::from(level.clone()));
    }
    if let Some(biz_type) = &filter.biz_type {
        sql.push_str(" AND n.biz_type = ?");
        params.push(Value::from(biz_type.clone()));
    }

    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = scope::paginate(
        scope::ScopeTarget::SYS_NOTICE,
        &sql,
        params,
        Some("ORDER BY n.created_at DESC, n.id DESC"),
        page,
        page_size,
        principal,
    )
    .await?;
    let list = list
        .iter()
        .map(map_notice)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Page {
        list,
        total,
        page,
        size,
        pages,
    })
}

/// 软删通知 —— **同一事务**里级联把该通知的所有接收记录标 `is_deleted = TRUE`。
///
/// # 为什么必须级联
///
/// `sys_notice_receiver` 的外键是 `ON DELETE CASCADE`，但它**只对硬删生效**；
/// 本函数是软删（只置 `deleted_at`），CASCADE 不会触发。
/// 若不级联，接收行的 `is_deleted` 仍是 FALSE，而 `count_unread` 的
/// 部分索引谓词正是 `user_id = ? AND read_flag = FALSE AND is_deleted = FALSE`
/// —— 已删通知会被算进未读数，导致「未读数 N 条但列表为空」的不一致。
///
/// # 返回值
///
/// 受影响行数（0 表示不存在或已删除）。
pub async fn soft_delete_notice(id: &uuid::Uuid) -> DbResult<u64> {
    let notice_id = *id;
    db().transaction(move |tx| async move {
        let affected = tx
            .execute_with_params(
                "UPDATE sys_notice SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
                 WHERE id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(notice_id)],
            )
            .await?;

        if affected > 0 {
            // 接收侧：已读与否都标删，口径与「未删通知」严格对齐
            tx.execute_with_params(
                "UPDATE sys_notice_receiver SET is_deleted = TRUE, deleted_at = CURRENT_TIMESTAMP \
                 WHERE notice_id = ?::uuid AND is_deleted = FALSE",
                vec![Value::from(notice_id)],
            )
            .await?;
        }

        Ok(affected)
    })
    .await
}

// ==================== 业务对象下线（公告删除的联动） ====================

/// 公告在 `sys_notice.biz_type` 上的取值。
///
/// # 为什么抽成常量
///
/// 写入侧在 [`crate::business::announcement::maintenance::dispatch_announcement`]，
/// 下线侧在 [`withdraw_by_biz`]。两处若是各自写字符串字面量，改一个漏一个就会出现
/// 「公告删了、通知还在」的静默不一致 —— 这个缺陷（BUG-D-05）正是这么来的。
/// 现在两侧都引用本常量，编译器保证不会漂移。
pub const BIZ_TYPE_ANNOUNCEMENT: &str = "announcement";

/// 「按业务对象下线通知」的结果（返回给调用方记日志 / 拼审计标签）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BizWithdrawReport {
    /// 本次被下线的通知主体行数（`sys_notice.deleted_at` 由空变有值的行）。
    pub notices: u64,
    /// 本次被标为用户侧删除的接收行数（`sys_notice_receiver.is_deleted`）。
    pub receivers: u64,
}

impl BizWithdrawReport {
    /// 是否有任何行被改动（无改动说明此前已下线 —— 幂等重放）。
    pub fn is_empty(&self) -> bool {
        self.notices == 0 && self.receivers == 0
    }
}

/// 按业务对象（`biz_type` + `biz_id`）下线其关联的全部通知。
///
/// # 为什么必须「同事务」
///
/// 调用方是公告软删（`biz_announcement`/`sys_announcement` 置 `deleted_at`）。若通知
/// 下线放在另一个事务，两次提交之间会存在「公告看不见、通知还在」的窗口 ——
/// 用户恰好在这几百毫秒里点开消息中心，就会拿到一条指向 404 的通知。
/// 因此本函数**不自己开事务**，只接受调用方传来的 [`Executor::Tx`]，由调用方的
/// 事务边界统一提交 / 回滚。
///
/// # 接收行的处理（审计口径）
///
/// `sys_notice_receiver` 的行**一条都不删**（`read_flag` / `read_at` / `created_at`
/// 一字不改），只把 `is_deleted` 置 TRUE —— 这与本模块既有的
/// [`soft_delete_notice`] 口径完全一致，原因见下：
///
/// - **已读回执必须留**：`read_flag` / `read_at` 是「谁在什么时候看过」的审计证据，
///   公告删除后依然要能回答「这条通知当时被谁读了」；
/// - **但未读数必须减**：`count_unread` 的部分索引谓词是
///   `user_id = ? AND read_flag = FALSE AND is_deleted = FALSE`，它**不 JOIN 主体表**。
///   只改 `sys_notice.deleted_at` 的话徽标数会一直挂着（「0 条列表 + 1 个未读徽标」），
///   正是 BUG-D-05 验收 ④ 要堵的不一致。
///
/// # 边界情况
///
/// - 从未扇出过的公告（`notice_id IS NULL`，见 `announcement_dispatch_retry`）：没有通知行，
///   返回 `{ notices: 0, receivers: 0 }`，不报错；
/// - 重复调用幂等：`deleted_at IS NULL` 与 `is_deleted = FALSE` 两个谓词保证第二次是 0 行；
/// - 公告卡片只软删过、之后又「擦除」过（理论上不存在）：同样幂等。
///
/// # 参数
///
/// - `ex`：执行器，**必须是事务**（见「为什么必须同事务」）；
/// - `biz_type`：业务类型（公告传 [`BIZ_TYPE_ANNOUNCEMENT`]）；
/// - `biz_id`：业务主键的字符串形态（公告传 `announcement_id.to_string()`）。
///
/// # 返回值
///
/// [`BizWithdrawReport`]：本次下线的通知数 / 接收行数。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（由调用方事务回滚，公告的软删一并撤销）。
pub async fn withdraw_by_biz(
    ex: &Executor<'_>,
    biz_type: &str,
    biz_id: &str,
) -> DbResult<BizWithdrawReport> {
    let params = vec![
        Value::from(biz_type.to_string()),
        Value::from(biz_id.to_string()),
    ];

    // ① 主体：置 deleted_at。谓词 `deleted_at IS NULL` 保证幂等。
    let notices = ex
        .exec(
            "UPDATE sys_notice SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
             WHERE biz_type = ? AND biz_id = ? AND deleted_at IS NULL",
            params.clone(),
        )
        .await?;

    // ② 顺带把「已建未发」（PENDING 定时通知）取消掉，与 cleanup 任务
    //    `cancel_soft_deleted_scheduled` 的口径一致 —— 少了这一步，已删公告的定时通知
    //    仍会在到点后被 patrol 捞出来扇出（用户会收到一条指向不存在公告的通知）。
    ex.exec(
        "UPDATE sys_notice SET dispatch_status = 'CANCELED', updated_at = CURRENT_TIMESTAMP \
         WHERE biz_type = ? AND biz_id = ? AND dispatch_status = 'PENDING'",
        params.clone(),
    )
    .await?;

    // ③ 接收行：只打用户侧删除标记，已读回执（read_flag / read_at）原样保留。
    //    条件里的 `n.deleted_at IS NOT NULL` 是为了覆盖**历史遗留**行（本次修复之前
    //    已被删掉的公告，通知一直没下线）—— 调用方一旦再次删除同 id（不可能，但幂等）
    //    或做一次性回填时，这批也能被收尾。
    let receivers = ex
        .exec(
            "UPDATE sys_notice_receiver r \
                SET is_deleted = TRUE, deleted_at = CURRENT_TIMESTAMP \
               FROM sys_notice n \
              WHERE n.id = r.notice_id \
                AND n.biz_type = ? AND n.biz_id = ? \
                AND n.deleted_at IS NOT NULL \
                AND r.is_deleted = FALSE",
            params,
        )
        .await?;

    Ok(BizWithdrawReport { notices, receivers })
}

/// 按接收人统计某条通知的实时口径（总行数 / 已读 / 用户删除）。
pub async fn receiver_stats(notice_id: &uuid::Uuid) -> DbResult<ReceiverStats> {
    let sql = "SELECT COUNT(*) AS total, \
               COUNT(*) FILTER (WHERE read_flag) AS read_count, \
               COUNT(*) FILTER (WHERE is_deleted) AS deleted_count \
               FROM sys_notice_receiver WHERE notice_id = ?::uuid";
    let rows = db()
        .find_with_params(sql, vec![Value::from(*notice_id)])
        .await?;
    match rows.first() {
        Some(row) => Ok(ReceiverStats {
            total: row.get_typed("total")?,
            read_count: row.get_typed("read_count")?,
            deleted_count: row.get_typed("deleted_count")?,
        }),
        None => Ok(ReceiverStats::default()),
    }
}

/// 扇出后回写 `receiver_count`（一条集合式 UPDATE，不在 Rust 侧计数）。
///
/// # 返回值
///
/// 回写后的 `receiver_count`（供 `DispatchResult` 直接使用，省一次查询）。
pub async fn refresh_receiver_count(ex: &Executor<'_>, notice_id: &uuid::Uuid) -> DbResult<i64> {
    ex.exec(
        "UPDATE sys_notice n SET receiver_count = \
             (SELECT COUNT(*) FROM sys_notice_receiver r WHERE r.notice_id = n.id), \
             updated_at = CURRENT_TIMESTAMP \
         WHERE n.id = ?::uuid AND n.deleted_at IS NULL",
        vec![Value::from(*notice_id)],
    )
    .await?;
    let rows = ex
        .rows(
            "SELECT receiver_count FROM sys_notice WHERE id = ?::uuid",
            vec![Value::from(*notice_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("receiver_count"),
        None => Ok(0),
    }
}

// ==================== 接收人（我的通知 / 未读数 / 已读） ====================

/// 我的通知分页（接收行 + 通知主体联表）。
///
/// # 参数
///
/// - `user_id`：当前用户。
/// - `read_flag`：`Some(true)` 只看已读 / `Some(false)` 只看未读 / `None` 全部。
/// - `page` / `page_size`：分页。
///
/// # 说明
///
/// 排序走 `idx_sys_notice_receiver_list`（`(user_id, created_at DESC) WHERE is_deleted = FALSE`）。
pub async fn page_my_notices(
    user_id: &uuid::Uuid,
    read_flag: Option<bool>,
    page: u64,
    page_size: u64,
) -> DbResult<Page<MyNoticeRow>> {
    // SCOPE-EXEMPT: 「我的通知」已按 user_id 硬绑定（`WHERE r.user_id = ?::uuid`，值取自令牌的
    // sub，不是请求参数），数据权限注入在这里既无意义也有害 —— 部门/归属条件会把
    // 「别人发给我的通知」二次过滤掉（静默少数据）。主体表 `sys_notice_receiver` 也不在
    // §2.5 登记清单里。闸① 仍走唯一入口 `scope::paginate_unscoped`（显式 unscoped 锚点）。
    let mut sql = String::from(
        "SELECT r.id AS receiver_id, r.notice_id, r.read_flag, r.read_at, r.created_at, \
         n.notice_no, n.notice_type, n.level, n.title, n.content, n.content_format, n.link_url \
         FROM sys_notice_receiver r \
         JOIN sys_notice n ON n.id = r.notice_id AND n.deleted_at IS NULL AND n.status = 1 \
         WHERE r.user_id = ?::uuid AND r.is_deleted = FALSE",
    );
    let mut params = vec![Value::from(*user_id)];
    if let Some(flag) = read_flag {
        sql.push_str(" AND r.read_flag = ?");
        params.push(Value::from(flag));
    }

    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("sys_notice_receiver"),
        &sql,
        params,
        Some("ORDER BY r.created_at DESC, r.id DESC"),
        page,
        page_size,
    )
    .await?;
    let list = list
        .iter()
        .map(|row| {
            Ok(MyNoticeRow {
                receiver_id: row.get_typed("receiver_id")?,
                notice_id: row.get_typed("notice_id")?,
                notice_no: row.get_typed("notice_no")?,
                notice_type: row.get_typed("notice_type")?,
                level: row.get_typed("level")?,
                title: row.get_typed("title")?,
                content: row.get_typed("content")?,
                content_format: row.get_typed("content_format")?,
                link_url: row.get_typed("link_url")?,
                read_flag: row.get_typed("read_flag")?,
                read_at: row.get_typed("read_at")?,
                created_at: row.get_typed("created_at")?,
            })
        })
        .collect::<std::result::Result<Vec<_>, DbError>>()?;
    Ok(Page {
        list,
        total,
        page,
        size,
        pages,
    })
}

/// 未读数（高频查询，**必须命中 `idx_sys_notice_receiver_unread`**）。
///
/// 查询条件与部分索引谓词逐字一致：`user_id = ? AND read_flag = FALSE AND is_deleted = FALSE`。
/// 验证见 `docs/批次A-A5-验证证据.md` 的 `EXPLAIN (ANALYZE, BUFFERS)` 输出。
pub async fn count_unread(user_id: &uuid::Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT COUNT(*) FROM sys_notice_receiver \
         WHERE user_id = ?::uuid AND read_flag = FALSE AND is_deleted = FALSE",
        vec![Value::from(*user_id)],
    )
    .await
}

/// 单条已读（事务内同时回写 `sys_notice.read_count`）。
///
/// # 返回值
///
/// 真正发生状态变化返回 `true`；不存在 / 不属于本人 / 已读 / 已删返回 `false`。
pub async fn mark_read(receiver_id: &uuid::Uuid, user_id: &uuid::Uuid) -> DbResult<bool> {
    let receiver = *receiver_id;
    let user = *user_id;
    let changed = db()
        .transaction(|tx| async move {
            let affected = tx
                .execute_with_params(
                    "UPDATE sys_notice_receiver SET read_flag = TRUE, read_at = CURRENT_TIMESTAMP \
                     WHERE id = ?::uuid AND user_id = ?::uuid AND read_flag = FALSE \
                       AND is_deleted = FALSE",
                    vec![Value::from(receiver), Value::from(user)],
                )
                .await?;
            if affected == 0 {
                return Ok(false);
            }
            tx.execute_with_params(
                "UPDATE sys_notice n SET read_count = LEAST(n.receiver_count, n.read_count + 1), \
                     updated_at = CURRENT_TIMESTAMP \
                 WHERE n.id = (SELECT notice_id FROM sys_notice_receiver WHERE id = ?::uuid)",
                vec![Value::from(receiver)],
            )
            .await?;
            Ok(true)
        })
        .await?;
    Ok(changed)
}

/// 全部已读（一条集合式 UPDATE + 一条增量回写，均不在 Rust 侧循环）。
///
/// # 返回值
///
/// 本次置为已读的行数。
pub async fn mark_all_read(user_id: &uuid::Uuid) -> DbResult<i64> {
    let user = *user_id;
    db().transaction(|tx| async move {
        // 数据修改型 CTE：先更新接收行并汇总每个通知的增量，再用增量回写 read_count。
        // read_flag 只会 FALSE → TRUE，故「增量」等于真实已读数；LEAST 兜住计数漂移。
        let rows = tx
            .find_with_params(
                "WITH updated AS ( \
                     UPDATE sys_notice_receiver SET read_flag = TRUE, read_at = CURRENT_TIMESTAMP \
                     WHERE user_id = ?::uuid AND read_flag = FALSE AND is_deleted = FALSE \
                     RETURNING notice_id \
                 ), agg AS ( \
                     SELECT notice_id, COUNT(*) AS cnt FROM updated GROUP BY notice_id \
                 ), bump AS ( \
                     UPDATE sys_notice n SET read_count = LEAST(n.receiver_count, n.read_count + agg.cnt), \
                            updated_at = CURRENT_TIMESTAMP \
                     FROM agg WHERE n.id = agg.notice_id RETURNING n.id \
                 ) \
                 SELECT COALESCE(SUM(cnt), 0)::bigint AS total FROM agg",
                vec![Value::from(user)],
            )
            .await?;
        let total: i64 = match rows.first() {
            Some(row) => row.get_typed("total")?,
            None => 0,
        };
        Ok(total)
    })
    .await
}

/// 删除「我的这条」（用户侧软删，不影响其他接收人）。
///
/// # 返回值
///
/// 受影响行数（0 表示不存在 / 不属于本人 / 已删除）。
pub async fn delete_my_receiver(receiver_id: &uuid::Uuid, user_id: &uuid::Uuid) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE sys_notice_receiver SET is_deleted = TRUE, deleted_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND user_id = ?::uuid AND is_deleted = FALSE",
        vec![Value::from(*receiver_id), Value::from(*user_id)],
    )
    .await
}

// ==================== 模板 ====================

/// 模板分页。
pub async fn page_templates(
    filter: &crate::business::notice::dto::TemplateFilter,
    page: u64,
    page_size: u64,
) -> DbResult<Page<SysNoticeTemplateModel>> {
    // 闸① 唯一入口：`sys_notice_template` 是全局资产（§2.5 / 附录 C #16），
    // 可见性由权限码 `business:notice:template:*` 控制，不注入数据范围 → 显式 unscoped。
    let mut sql =
        format!("SELECT {TEMPLATE_COLUMNS} FROM sys_notice_template WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(" AND (template_code LIKE ? OR template_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(notice_type) = &filter.notice_type {
        sql.push_str(" AND notice_type = ?");
        params.push(Value::from(notice_type.clone()));
    }
    if let Some(status) = filter.status {
        sql.push_str(" AND status = ?");
        params.push(TypeMapper::to_value(&status));
    }

    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("sys_notice_template"),
        &sql,
        params,
        Some("ORDER BY created_at DESC"),
        page,
        page_size,
    )
    .await?;
    let list = list
        .iter()
        .map(map_template)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Page {
        list,
        total,
        page,
        size,
        pages,
    })
}

/// 按主键查模板（未软删）。
pub async fn find_template(id: &uuid::Uuid) -> DbResult<Option<SysNoticeTemplateModel>> {
    let sql = format!(
        "SELECT {TEMPLATE_COLUMNS} FROM sys_notice_template \
         WHERE id = ?::uuid AND deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(*id)]).await?;
    rows.first().map(map_template).transpose()
}

/// 按编码查**启用中**的模板（发送时用；停用模板等同不存在）。
pub async fn find_enabled_template_by_code(code: &str) -> DbResult<Option<SysNoticeTemplateModel>> {
    let sql = format!(
        "SELECT {TEMPLATE_COLUMNS} FROM sys_notice_template \
         WHERE template_code = ? AND deleted_at IS NULL AND status = 1 LIMIT 1"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(code)]).await?;
    rows.first().map(map_template).transpose()
}

/// 统计同编码的模板数（唯一性校验；软删表的部分唯一索引兜底）。
pub async fn count_template_code(code: &str, exclude_id: Option<&uuid::Uuid>) -> DbResult<i64> {
    match exclude_id {
        Some(id) => {
            db().scalar_with_params(
                "SELECT COUNT(*) FROM sys_notice_template \
                 WHERE template_code = ? AND deleted_at IS NULL AND id <> ?::uuid",
                vec![Value::from(code), Value::from(*id)],
            )
            .await
        }
        None => {
            db().scalar_with_params(
                "SELECT COUNT(*) FROM sys_notice_template \
                 WHERE template_code = ? AND deleted_at IS NULL",
                vec![Value::from(code)],
            )
            .await
        }
    }
}

/// 新建模板，返回新主键。
pub async fn insert_template(id: &uuid::Uuid, fields: &TemplateFields<'_>) -> DbResult<()> {
    db().execute_with_params(
        "INSERT INTO sys_notice_template (id, template_code, template_name, notice_type, \
         default_level, title_template, content_template, content_format, link_template, channels, \
         variables, status, remark, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?, ?, ?, ?, ?, NULLIF(?::text, ''), ?, ?::jsonb, \
         ?, NULLIF(?::text, ''), NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid)",
        vec![
            Value::from(*id),
            Value::from(fields.template_code),
            Value::from(fields.template_name),
            Value::from(fields.notice_type),
            Value::from(fields.default_level),
            Value::from(fields.title_template),
            Value::from(fields.content_template),
            Value::from(fields.content_format),
            Value::from(fields.link_template.unwrap_or("")),
            Value::from(fields.channels),
            Value::Json(
                fields
                    .variables_json
                    .clone()
                    .unwrap_or_else(|| "[]".to_string()),
            ),
            TypeMapper::to_value(&fields.status),
            Value::from(fields.remark.unwrap_or("")),
            Value::from(opt_uuid_text(fields.operator)),
            Value::from(opt_uuid_text(fields.operator)),
        ],
    )
    .await?;
    Ok(())
}

/// 更新模板，返回受影响行数。
pub async fn update_template(id: &uuid::Uuid, fields: &TemplateFields<'_>) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE sys_notice_template SET template_code = ?, template_name = ?, notice_type = ?, \
         default_level = ?, title_template = ?, content_template = ?, content_format = ?, \
         link_template = NULLIF(?::text, ''), channels = ?, variables = ?::jsonb, \
         status = ?, remark = NULLIF(?::text, ''), update_by = NULLIF(?::text, '')::uuid, \
         updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(fields.template_code),
            Value::from(fields.template_name),
            Value::from(fields.notice_type),
            Value::from(fields.default_level),
            Value::from(fields.title_template),
            Value::from(fields.content_template),
            Value::from(fields.content_format),
            Value::from(fields.link_template.unwrap_or("")),
            Value::from(fields.channels),
            Value::Json(
                fields
                    .variables_json
                    .clone()
                    .unwrap_or_else(|| "[]".to_string()),
            ),
            TypeMapper::to_value(&fields.status),
            Value::from(fields.remark.unwrap_or("")),
            Value::from(opt_uuid_text(fields.operator)),
            Value::from(*id),
        ],
    )
    .await
}

/// 软删模板，返回受影响行数。
pub async fn soft_delete_template(id: &uuid::Uuid) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE sys_notice_template SET deleted_at = CURRENT_TIMESTAMP, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(*id)],
    )
    .await
}

/// 统计引用了某模板编码的有效通知数（删除模板前拦截，409）。
pub async fn count_notices_using_template(code: &str) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT COUNT(*) FROM sys_notice WHERE template_code = ? AND deleted_at IS NULL",
        vec![Value::from(code)],
    )
    .await
}

// ==================== 投递记录 ====================

/// 写入某通道的投递记录（**集合式 INSERT ... SELECT**，逐通道一条，不在 Rust 侧循环接收人）。
///
/// # 参数
///
/// - `notice_id`：通知主键。
/// - `channel`：`EMAIL` / `SMS`（按接收人的邮箱 / 手机号投递）。
/// - `configured`：通道是否已配置；未配置时状态直接落 `SKIPPED`（不报错）。
///
/// # 返回值
///
/// 写入（或命中唯一键跳过）后新增的行数。
pub async fn insert_receiver_deliveries(
    ex: &Executor<'_>,
    notice_id: &uuid::Uuid,
    channel: &str,
    configured: bool,
) -> DbResult<u64> {
    // 目标列按通道取：EMAIL → sys_user.email，SMS → sys_user.mobile。
    // 取不到目标地址的接收人不生成投递行（通道层没有可投递对象）。
    let target_column = match channel {
        "EMAIL" => "u.email",
        "SMS" => "u.mobile",
        _ => return Ok(0),
    };
    let sql = format!(
        "INSERT INTO sys_notice_delivery (notice_id, receiver_id, channel, target, status) \
         SELECT r.notice_id, r.id, ?, {target_column}, ? \
         FROM sys_notice_receiver r \
         JOIN sys_user u ON u.id = r.user_id \
         WHERE r.notice_id = ?::uuid AND r.is_deleted = FALSE \
           AND COALESCE({target_column}, '') <> '' \
         ON CONFLICT (notice_id, COALESCE(receiver_id, notice_id), channel) DO NOTHING"
    );
    ex.exec(
        &sql,
        vec![
            Value::from(channel.to_string()),
            Value::from(if configured { "PENDING" } else { "SKIPPED" }),
            Value::from(*notice_id),
        ],
    )
    .await
}

/// 写入通知级（非按人）的投递记录，如 Webhook（一条通知一条记录）。
///
/// # 参数
///
/// - `ex`：执行器（事务内调用时传 [`Executor::Tx`]，与通知落库同事务）。
/// - `notice_id`：通知主键。
/// - `channel`：通道（`WEBHOOK`）。
/// - `target`：目标地址（Webhook URL；未配置时可为空串）。
/// - `configured`：是否已配置。
pub async fn insert_notice_delivery(
    ex: &Executor<'_>,
    notice_id: &uuid::Uuid,
    channel: &str,
    target: &str,
    configured: bool,
) -> DbResult<u64> {
    ex.exec(
        "INSERT INTO sys_notice_delivery (notice_id, receiver_id, channel, target, status) \
         VALUES (?::uuid, NULL, ?, ?, ?) \
         ON CONFLICT (notice_id, COALESCE(receiver_id, notice_id), channel) DO NOTHING",
        vec![
            Value::from(*notice_id),
            Value::from(channel.to_string()),
            Value::from(target.to_string()),
            Value::from(if configured { "PENDING" } else { "SKIPPED" }),
        ],
    )
    .await
}

/// 按通道 + 状态汇总某条通知的投递记录（详情页展示）。
pub async fn delivery_summary(notice_id: &uuid::Uuid) -> DbResult<Vec<DeliverySummaryRow>> {
    let rows = db()
        .find_with_params(
            "SELECT channel, status, COUNT(*) AS count FROM sys_notice_delivery \
             WHERE notice_id = ?::uuid GROUP BY channel, status ORDER BY channel, status",
            vec![Value::from(*notice_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(DeliverySummaryRow {
                channel: row.get_typed("channel")?,
                status: row.get_typed("status")?,
                count: row.get_typed("count")?,
            })
        })
        .collect()
}

/// 查询某条通知在**给定用户集合**内的接收行（A6 SSE 推送用）。
///
/// # 参数
///
/// - `notice_id`：通知主键。
/// - `user_ids`：候选接收人（通常是「当前在线用户」；为空直接返回空 `Vec`，不查库）。
///
/// # 返回值
///
/// `(user_id, receiver_id)` 列表。`receiver_id` 是**该用户自己的**接收行主键，
/// 前端据此调 `PUT /notice/read/{receiver_id}`，无需再查一次。
///
/// # 为什么按用户集合过滤
///
/// 全员通知的接收行可能有 5 万条，而在线用户通常只有几十个。用 `user_id IN (...)`
/// 让数据库只回这几行（走 `uk_sys_notice_receiver(notice_id, user_id)` 索引），
/// 避免「为了推几十条消息把 5 万行拉进内存」。
///
/// # 参数传递
///
/// 与扇出（[`crate::business::notice::fanout`]）同一口径：UUID 数组用
/// `jsonb_array_elements_text((?::text)::jsonb)` 作为**一个文本参数**传入，不拼 SQL 字符串。
///
/// 参数类型必须显式钉成 `text` 再转 `jsonb`：驱动按语句描述里推断出的参数类型校验
/// `Value` 变体，直接写 `?::jsonb` 时推断出的是 jsonb 而这里给的是文本，
/// 会在驱动侧报 `error serializing parameter`（A6 实测踩过：先绑 `Value::Json` 再绑
/// `Value::from(String)` 都失败，`(?::text)::jsonb` + `Value::Text` 才稳定）。
pub async fn online_receivers(
    notice_id: &uuid::Uuid,
    user_ids: &[uuid::Uuid],
) -> DbResult<Vec<(uuid::Uuid, uuid::Uuid)>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::Value::from(
        user_ids
            .iter()
            .map(uuid::Uuid::to_string)
            .collect::<Vec<String>>(),
    );
    let rows = db()
        .find_with_params(
            "SELECT r.user_id, r.id FROM sys_notice_receiver r \
             WHERE r.notice_id = ?::uuid AND r.is_deleted = FALSE \
               AND r.user_id IN \
                   (SELECT (x)::uuid FROM jsonb_array_elements_text((?::text)::jsonb) AS x)",
            vec![Value::from(*notice_id), Value::Text(ids.to_string())],
        )
        .await?;
    rows.iter()
        .map(|row| Ok((row.get_typed("user_id")?, row.get_typed("id")?)))
        .collect()
}

/// 通道配置（内部视图：掩码化的对外响应 + 真实 Webhook 地址）。
///
/// `webhook_url` 只用于写 `sys_notice_delivery.target`（投递记录必须记下投往哪里），
/// **绝不进任何 API 响应**——对外只给 [`crate::business::notice::dto::ChannelConfigRes::webhook_masked`]。
pub struct ChannelSettings {
    /// 对外（掩码后）的通道配置。
    pub config: crate::business::notice::dto::ChannelConfigRes,
    /// 真实 Webhook 地址（未配置时为空串）。
    pub webhook_url: String,
}

/// 读取通道配置（`sys_config` 的 `notice.*` 键；**只读、不落库、不含密钥**）。
///
/// # 返回值
///
/// 各通道是否已配置 + 真实 Webhook 地址（内部用）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（调用方按「全部未配置」降级也能跑，见 [`channel_settings_best_effort`]）。
pub async fn channel_settings() -> DbResult<ChannelSettings> {
    use crate::business::notice::dto::ChannelConfigRes;

    let rows = db()
        .find_with_params(
            "SELECT config_key, COALESCE(config_value, '') AS config_value FROM sys_config \
             WHERE config_key IN ('notice.email.enabled', 'notice.webhook.url', \
                                  'notice.sms.enabled') \
               AND deleted_at IS NULL AND status = 1",
            Vec::new(),
        )
        .await?;

    let mut email = false;
    let mut sms = false;
    let mut webhook_url = String::new();
    for row in rows {
        let key: String = row.get_typed("config_key")?;
        let value: String = row.get_typed("config_value")?;
        let value = value.trim().to_string();
        match key.as_str() {
            "notice.email.enabled" => email = is_truthy(&value),
            "notice.sms.enabled" => sms = is_truthy(&value),
            "notice.webhook.url" => webhook_url = value,
            _ => {}
        }
    }
    let webhook = webhook_url.starts_with("http://") || webhook_url.starts_with("https://");
    Ok(ChannelSettings {
        config: ChannelConfigRes {
            inbox: true,
            email,
            webhook,
            sms,
            webhook_masked: mask_url(&webhook_url),
        },
        webhook_url,
    })
}

/// 读通道配置，失败时降级为「全部未配置」并记一条告警。
///
/// 用途：发送通知不能因为「读配置失败」而失败——通道未配置本来就记 `SKIPPED`。
pub async fn channel_settings_best_effort() -> ChannelSettings {
    match channel_settings().await {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(error = %error, "读取通知通道配置失败，按全部未配置处理");
            ChannelSettings {
                config: crate::business::notice::dto::ChannelConfigRes {
                    inbox: true,
                    email: false,
                    webhook: false,
                    sms: false,
                    webhook_masked: String::new(),
                },
                webhook_url: String::new(),
            }
        }
    }
}

// ==================== 内部映射 ====================

/// 行 → 通知模型。
fn map_notice(row: &Row) -> std::result::Result<SysNoticeModel, DbError> {
    let target_ids_text: Option<String> = row.get_typed("target_ids")?;
    Ok(SysNoticeModel {
        id: row.get_typed("id")?,
        notice_no: row.get_typed("notice_no")?,
        notice_type: row.get_typed("notice_type")?,
        level: row.get_typed("level")?,
        title: row.get_typed("title")?,
        content: row.get_typed("content")?,
        content_format: row.get_typed("content_format")?,
        link_url: row.get_typed("link_url")?,
        biz_type: row.get_typed("biz_type")?,
        biz_id: row.get_typed("biz_id")?,
        template_code: row.get_typed("template_code")?,
        sender_id: row.get_typed("sender_id")?,
        sender_name: row.get_typed("sender_name")?,
        target_type: row.get_typed("target_type")?,
        target_ids: parse_uuid_list(target_ids_text.as_deref()),
        channel_mask: row.get_typed("channel_mask")?,
        receiver_count: row.get_typed("receiver_count")?,
        read_count: row.get_typed("read_count")?,
        status: row.get_typed("status")?,
        expire_at: row.get_typed("expire_at")?,
        scheduled_at: row.get_typed("scheduled_at")?,
        dispatch_status: row.get_typed("dispatch_status")?,
        dispatch_attempts: row.get_typed("dispatch_attempts")?,
        created_at: row.get_typed("created_at")?,
        updated_at: row.get_typed("updated_at")?,
        deleted_at: row.get_typed("deleted_at")?,
    })
}

/// 行 → 模板模型。
fn map_template(row: &Row) -> std::result::Result<SysNoticeTemplateModel, DbError> {
    Ok(SysNoticeTemplateModel {
        id: row.get_typed("id")?,
        template_code: row.get_typed("template_code")?,
        template_name: row.get_typed("template_name")?,
        notice_type: row.get_typed("notice_type")?,
        default_level: row.get_typed("default_level")?,
        title_template: row.get_typed("title_template")?,
        content_template: row.get_typed("content_template")?,
        content_format: row.get_typed("content_format")?,
        link_template: row.get_typed("link_template")?,
        channels: row.get_typed("channels")?,
        variables_json: row.get_typed("variables_json")?,
        status: row.get_typed("status")?,
        remark: row.get_typed("remark")?,
        created_at: row.get_typed("created_at")?,
        updated_at: row.get_typed("updated_at")?,
        deleted_at: row.get_typed("deleted_at")?,
    })
}

/// 解析 `target_ids::text`（JSON 数组）为字符串列表，脏数据降级为空。
fn parse_uuid_list(raw: Option<&str>) -> Vec<String> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Vec::new();
    };
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// 可空 UUID → 绑定用文本（空串在 SQL 里被 `NULLIF` 转回 NULL）。
fn opt_uuid_text(id: MaybeId) -> String {
    id.map(|id| id.to_string()).unwrap_or_default()
}

/// 可空时间 → 绑定用文本（RFC3339；空串 → NULL）。
fn opt_time_text(time: Option<chrono::DateTime<chrono::Utc>>) -> String {
    time.map(|time| time.to_rfc3339()).unwrap_or_default()
}

/// 参数值是否表达「真」。
fn is_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

/// URL 掩码：保留协议与主机首段，其余打码（不泄露完整 Webhook 地址）。
fn mask_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let (scheme, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) => (format!("{scheme}://"), rest),
        None => (String::new(), trimmed),
    };
    let host = rest.split('/').next().unwrap_or(rest);
    if host.is_empty() {
        return format!("{scheme}****");
    }
    format!("{scheme}{host}/****")
}

/// 把「启用状态」转成 `i16`（模板 `status` 列用）。
pub fn enable_status_value(status: EnableStatus) -> i16 {
    status.as_i16()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 公告业务类型常量：下线侧与扇出侧共用的那个字符串。
    ///
    /// # 为什么值得断言
    ///
    /// `sys_notice.biz_type` 里已经躺着历史行（值是 `announcement`）。这个常量一旦被
    /// 改成别的拼法（`ANN` / `announcements`），**新删除的公告认不出老通知**，
    /// BUG-D-05 会以「只有历史数据不生效」的形态悄悄复发 —— 而且照样能编译通过。
    #[test]
    fn announcement_biz_type_is_stable() {
        assert_eq!(BIZ_TYPE_ANNOUNCEMENT, "announcement");
        // 与 `announcement::maintenance` 的扇出、V0043 的列宽（VARCHAR(64)）都相容
        assert!(!BIZ_TYPE_ANNOUNCEMENT.is_empty());
        assert!(BIZ_TYPE_ANNOUNCEMENT.len() <= 64);
    }

    /// 下线报告的「空」判定：两个计数都为 0 才算没做事（幂等重放的判据）。
    #[test]
    fn withdraw_report_is_empty_only_when_both_zero() {
        assert!(BizWithdrawReport::default().is_empty());
        assert!(
            !BizWithdrawReport {
                notices: 1,
                receivers: 0
            }
            .is_empty()
        );
        assert!(
            !BizWithdrawReport {
                notices: 0,
                receivers: 2
            }
            .is_empty()
        );
        assert!(
            !BizWithdrawReport {
                notices: 1,
                receivers: 2
            }
            .is_empty()
        );
    }

    /// `target_ids` 列的脏数据必须降级为空列表（详情接口不能因此 500）。
    #[test]
    fn parse_uuid_list_tolerates_dirty_json() {
        assert!(parse_uuid_list(None).is_empty());
        assert!(parse_uuid_list(Some("")).is_empty());
        assert!(parse_uuid_list(Some("not-json")).is_empty());
        assert!(parse_uuid_list(Some("\"abc\"")).is_empty());

        let values = parse_uuid_list(Some(r#"["a","b",3]"#));
        assert_eq!(values, vec!["a", "b"], "非字符串元素被丢弃");
    }

    /// 空 UUID / 空时间都绑定成空串，SQL 侧再用 NULLIF 转 NULL（避免 Value::Null 编码失败）。
    #[test]
    fn optional_bindings_use_empty_string() {
        assert_eq!(opt_uuid_text(None), "");
        assert_eq!(opt_time_text(None), "");
        let id = uuid::Uuid::now_v7();
        assert_eq!(opt_uuid_text(Some(id)), id.to_string());
    }

    /// Webhook 地址只回掩码，不能把完整地址吐给前端。
    #[test]
    fn webhook_url_is_masked() {
        assert_eq!(mask_url(""), "");
        assert_eq!(
            mask_url("https://hooks.example.com/a/b"),
            "https://hooks.example.com/****"
        );
        // 没有协议头时整段当主机，只保留第一段
        assert_eq!(mask_url("ftp//bad"), "ftp/****");
        assert_eq!(mask_url("hooks.example.com/x"), "hooks.example.com/****");
    }

    /// 真值判断：只认白名单字面量，避免 "false" 被当成真。
    #[test]
    fn truthy_values() {
        for value in ["true", " TRUE ", "1", "yes", "on"] {
            assert!(is_truthy(value), "{value} 应为真");
        }
        for value in ["", "false", "0", "off", "no"] {
            assert!(!is_truthy(value), "{value} 应为假");
        }
    }
}
