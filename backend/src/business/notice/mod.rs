//! 消息通知底座（批次 A · A5）。
//!
//! # 模块结构
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`model`] | `sys_notice` / `sys_notice_receiver` / `sys_notice_template` / `sys_notice_delivery` 读写 |
//! | [`dto`] | 领域枚举（类型 / 级别 / 格式 / 通道掩码）与请求响应 DTO |
//! | [`render`] | 自研 `{{var}}` 模板渲染器（含 5 条规则的单测） |
//! | [`fanout`] | 四种目标的集合式扇出（用户 / 角色 / 部门含下级 / 全员） |
//! | [`handler`] | 发送 / 列表 / 详情 / 已读 / 未读数 / 删除 / 模板 CRUD / 通道配置 / **SSE 订阅（A6）** |
//! | [`delivery`] | 通道投递计划与投递记录（站内信必投；邮件 / Webhook / 短信未配置记 `SKIPPED`，A6） |
//! | [`sse_hub`] | **per-user** SSE 频道中心（订阅 / 取消 / 推送 / 计数 / 心跳 / 空频道回收，A6） |
//! | [`cleanup`] | 过期通知与用户删除行的清理任务（每日 04:50） |
//!
//! # 对外唯一发送口径：[`dispatch`]
//!
//! C 批次（公告）、D 批次（工作流待办 / 抄送 / 超时提醒）都只调这一个函数，
//! 不自己拼 SQL、不自己扇出：
//!
//! ```ignore
//! use cloudpivot::business::notice::{dispatch, NoticeDraft, NoticeTarget};
//!
//! let result = dispatch(NoticeDraft {
//!     notice_type: NoticeType::Todo,
//!     level: NoticeLevel::Info,
//!     title: "你有新的审批待办".into(),
//!     content: "订单审批待处理".into(),
//!     content_format: ContentFormat::Text,
//!     link_url: Some(format!("/workflow/task/{task_id}")),
//!     biz_type: Some("workflow".into()),
//!     biz_id: Some(instance_id.to_string()),
//!     template_code: None,
//!     template_vars: serde_json::Value::Null,
//!     target: NoticeTarget::User(vec![assignee]),
//!     channel_mask: CHANNEL_INBOX,
//!     idempotency_key: Some(format!("wf:{instance_id}:todo:{task_id}")),
//!     expire_at: None,
//!     sender_id: None,
//!     sender_name: Some("工作流引擎".into()),
//! })
//! .await?;
//! ```
//!
//! **重复调用天然幂等**：`uk_sys_notice_idem` 保证同一 `idempotency_key` 只落一条通知，
//! 重放时返回 `duplicated = true` 与首次的 `notice_id`（不重复扇出、不重复推送）。
//!
//! # 本批次**不做**的事
//!
//! - 邮件 / Webhook / 短信的**真实发送**：只落 `sys_notice_delivery` 状态机，
//!   未配置通道一律记 `SKIPPED`（不报错，不影响站内信送达）。投递计划见 [`delivery`]；
//! - 跨副本推送：SSE 频道是进程内的（见 [`sse_hub`] 头注释），B 批次用 Redis Pub/Sub 替换。

pub mod cleanup;
pub mod delivery;
pub mod dto;
pub mod fanout;
pub mod handler;
pub mod model;
pub mod render;
pub mod sse_hub;

pub use fanout::NoticeTarget;

use alun::prelude::*;
use serde::Serialize;

use crate::business::notice::dto::{
    CHANNEL_INBOX, ContentFormat, NoticeLevel, NoticeType, channel_mask_of_csv,
};
use crate::business::notice::model::Executor;
use crate::shared::error_code;
use crate::utils;

/// 对外发送草稿（C / D 批次的入参结构，字段与架构文档 §4.4 一致）。
#[derive(Debug, Clone)]
pub struct NoticeDraft {
    /// 通知类型。
    pub notice_type: NoticeType,
    /// 级别。
    pub level: NoticeLevel,
    /// 标题（用模板时可留空）。
    pub title: String,
    /// 正文（用模板时可留空）。
    pub content: String,
    /// 正文格式（用模板时以模板为准）。
    pub content_format: ContentFormat,
    /// 跳转链接。
    pub link_url: Option<String>,
    /// 业务类型（如 `workflow` / `announcement`）。
    pub biz_type: Option<String>,
    /// 业务主键。
    pub biz_id: Option<String>,
    /// 模板编码（给了就走模板渲染）。
    pub template_code: Option<String>,
    /// 模板变量（`Null` 视为空对象）。
    pub template_vars: serde_json::Value,
    /// 发送目标。
    pub target: NoticeTarget,
    /// 通道位掩码（1 站内 / 2 邮件 / 4 Webhook / 8 短信）。
    pub channel_mask: u16,
    /// 幂等键（**强烈建议传**：业务事件唯一键）。
    pub idempotency_key: Option<String>,
    /// 过期时间（过期后由清理任务硬删）。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发送人 ID（系统触发可空）。
    pub sender_id: Option<uuid::Uuid>,
    /// 发送人名称（展示用，如「工作流引擎」）。
    pub sender_name: Option<String>,
}

impl NoticeDraft {
    /// 构造一条「纯文本站内信」草稿（最常用形态，减少样板字段）。
    ///
    /// # 参数
    ///
    /// - `notice_type` / `level`：类型与级别。
    /// - `title` / `content`：标题与正文。
    /// - `target`：发送目标。
    pub fn text(
        notice_type: NoticeType,
        level: NoticeLevel,
        title: impl Into<String>,
        content: impl Into<String>,
        target: NoticeTarget,
    ) -> Self {
        NoticeDraft {
            notice_type,
            level,
            title: title.into(),
            content: content.into(),
            content_format: ContentFormat::Text,
            link_url: None,
            biz_type: None,
            biz_id: None,
            template_code: None,
            template_vars: serde_json::Value::Null,
            target,
            channel_mask: CHANNEL_INBOX,
            idempotency_key: None,
            expire_at: None,
            sender_id: None,
            sender_name: None,
        }
    }
}

/// 单个通道的投递结果摘要。
#[derive(Debug, Clone, Serialize)]
pub struct ChannelState {
    /// 通道名（`INBOX` / `EMAIL` / `WEBHOOK` / `SMS`）。
    pub channel: String,
    /// 状态（站内信恒 `DELIVERED`；其余为 `PENDING` / `SKIPPED`）。
    pub status: String,
    /// 落库行数（站内信为接收人数）。
    pub rows: u64,
}

/// 发送结果。
#[derive(Debug, Clone, Serialize)]
pub struct DispatchResult {
    /// 通知主键（重放时是首次的通知 ID）。
    #[serde(serialize_with = "serialize_uuid")]
    pub notice_id: uuid::Uuid,
    /// 通知编号。
    pub notice_no: String,
    /// 接收人数（扇出后回写的 `receiver_count`）。
    pub receiver_count: i64,
    /// 是否命中幂等键（`true` 表示本次没有新发通知）。
    pub duplicated: bool,
    /// 各通道结果。
    pub channels: Vec<ChannelState>,
}

/// 发送失败原因（渲染 / 校验类走 [`DispatchError::Api`]，数据库类走 [`DispatchError::Db`]）。
#[derive(Debug)]
pub enum DispatchError {
    /// 业务校验 / 渲染失败（可直接回给调用方）。
    Api(ApiError),
    /// 数据库异常。
    Db(DbError),
}

impl From<DbError> for DispatchError {
    fn from(error: DbError) -> Self {
        DispatchError::Db(error)
    }
}

impl From<ApiError> for DispatchError {
    fn from(error: ApiError) -> Self {
        DispatchError::Api(error)
    }
}

impl DispatchError {
    /// 转成 HTTP 层可用的 [`ApiError`]（保留业务码与中文文案）。
    pub fn into_api(self) -> ApiError {
        match self {
            DispatchError::Api(error) => error,
            DispatchError::Db(error) => utils::db_error(error, "发送通知"),
        }
    }
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchError::Api(error) => write!(formatter, "[{}] {}", error.code, error.msg),
            DispatchError::Db(error) => write!(formatter, "数据库异常：{error}"),
        }
    }
}

/// **唯一的对外发送口径**：校验 → 渲染 → 落库 → 扇出 → 回写计数 → 投递记录。
///
/// # 参数
///
/// - `draft`：发送草稿（见 [`NoticeDraft`]）。
///
/// # 返回值
///
/// 发送结果（含 `notice_id` / 接收人数 / 各通道状态 / 是否幂等重放）。
///
/// # 异常情况
///
/// - `400`：目标为空 / 目标类型非法 / 通道掩码越界 / 缺标题正文；
/// - `404`：模板不存在或已停用；
/// - `422`：模板渲染失败（缺变量 / 语法 / 长度）、接收人超 5 万；
/// - `500`：数据库异常。
///
/// # 事务边界
///
/// 「落 notice → `INSERT ... SELECT` 扇出 → 回写 `receiver_count` → 写投递记录」在
/// **一个事务**内完成：任何一步失败都整体回滚，不会留下「通知存在但没人收到」的半成品。
pub async fn dispatch(draft: NoticeDraft) -> std::result::Result<DispatchResult, DispatchError> {
    draft.target.validate()?;

    // 调用方通道先校验（0 表示未指定，按站内信处理）；模板声明的通道稍后并入
    let requested_mask = if draft.channel_mask == 0 {
        CHANNEL_INBOX
    } else {
        draft.channel_mask
    };
    dto::ensure_channel_mask(requested_mask)?;
    let mut template_mask: Option<u16> = None;

    // ---------- 1. 模板解析（有模板则以模板的类型 / 级别 / 格式 / 通道为准） ----------
    //
    // 级别 / 类型的生效规则（写进文档，避免调用方误解）：
    // 模板存在时，若调用方给的是**默认档**（类型 `SYSTEM`、级别 `INFO`），则采用模板的
    // `notice_type` / `default_level`；显式指定了非默认值则以调用方为准。
    let (title_source, content_source, format, link_url, template_code, notice_type, level) =
        match draft.template_code.as_deref().map(str::trim) {
            Some(code) if !code.is_empty() => {
                let template = model::find_enabled_template_by_code(code)
                    .await
                    .map_err(DispatchError::Db)?
                    .ok_or_else(|| {
                        ApiError::new(
                            404,
                            error_code::NOTICE_TEMPLATE_NOT_FOUND,
                            format!("通知模板不存在或已停用：{code}"),
                        )
                    })?;
                let format = ContentFormat::parse(&template.content_format)?;
                // 模板声明的通道：与调用方通道取**并集**（见 delivery::plan_channels）
                template_mask = Some(channel_mask_of_csv(&template.channels));
                let link = draft
                    .link_url
                    .clone()
                    .or_else(|| template.link_template.clone());
                let notice_type = if draft.notice_type == NoticeType::System {
                    NoticeType::parse(&template.notice_type).unwrap_or(NoticeType::System)
                } else {
                    draft.notice_type
                };
                let level = if draft.level == NoticeLevel::Info {
                    NoticeLevel::parse(&template.default_level).unwrap_or(NoticeLevel::Info)
                } else {
                    draft.level
                };
                (
                    template.title_template.clone(),
                    template.content_template.clone(),
                    format,
                    link,
                    Some(template.template_code.clone()),
                    notice_type,
                    level,
                )
            }
            _ => {
                if draft.title.trim().is_empty() {
                    return Err(DispatchError::Api(ApiError::new(
                        400,
                        error_code::PARAM_INVALID,
                        "未使用模板时标题不能为空",
                    )));
                }
                if draft.content.trim().is_empty() {
                    return Err(DispatchError::Api(ApiError::new(
                        400,
                        error_code::PARAM_INVALID,
                        "未使用模板时正文不能为空",
                    )));
                }
                (
                    draft.title.clone(),
                    draft.content.clone(),
                    draft.content_format,
                    draft.link_url.clone(),
                    None,
                    draft.notice_type,
                    draft.level,
                )
            }
        };

    // ---------- 2. 生效通道掩码：模板通道 ∪ 调用方通道，站内信必投 ----------
    let mask = delivery::plan_channels(template_mask, draft.channel_mask);
    dto::ensure_channel_mask(mask)?;

    // ---------- 3. 渲染（长度 / 缺变量 / HTML 转义都在 render 内统一处理） ----------
    let vars = match &draft.template_vars {
        serde_json::Value::Null => serde_json::json!({}),
        other => other.clone(),
    };
    let (title, content) = render::render_notice(&title_source, &content_source, &vars, format)
        .map_err(DispatchError::Api)?;

    // ---------- 4. 扇出规模预演（超 5 万先拒，不先插再回滚） ----------
    let planned = fanout::count_receivers(&draft.target)
        .await
        .map_err(DispatchError::Db)?;
    fanout::ensure_within_limit(planned)?;

    // ---------- 5. 通道配置（未配置 → SKIPPED，不报错） ----------
    let settings = model::channel_settings_best_effort().await;

    // ---------- 6. 一个事务内完成落库 / 扇出 / 计数 / 投递 ----------
    let notice_id = uuid::Uuid::now_v7();
    let notice_no = build_notice_no(&notice_id, &notice_type);
    let notice_no_in = notice_no.clone();
    // 实时推送种子（事务闭包会 move 掉 title / link_url / draft，这里先留一份）
    let push_seed = sse_hub::NoticeEvent {
        notice_id: notice_id.to_string(),
        notice_no: notice_no.clone(),
        notice_type: notice_type.as_str().to_string(),
        level: level.as_str().to_string(),
        title: title.clone(),
        link_url: link_url.clone(),
        biz_type: draft
            .biz_type
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        biz_id: draft
            .biz_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        receiver_id: String::new(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let target_ids_json = draft.target.ids_json();
    let target_type = draft.target.target_type();
    let template_vars_json = match &draft.template_vars {
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    };
    let idempotency_key = draft
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string);

    let outcome = db()
        .transaction(|tx| async move {
            let ex = Executor::Tx(&tx);
            let new = model::NewNotice {
                id: notice_id,
                notice_no: &notice_no_in,
                notice_type: notice_type.as_str(),
                level: level.as_str(),
                title: &title,
                content: &content,
                content_format: format.as_str(),
                link_url: link_url.as_deref(),
                biz_type: draft
                    .biz_type
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty()),
                biz_id: draft
                    .biz_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty()),
                template_code: template_code.as_deref(),
                template_vars_json,
                sender_id: draft.sender_id,
                sender_name: draft.sender_name.as_deref(),
                target_type,
                target_ids_json,
                channel_mask: mask as i16,
                idempotency_key: idempotency_key.as_deref(),
                expire_at: draft.expire_at,
                operator: draft.sender_id,
            };

            if !model::insert_notice(&ex, &new).await? {
                // 幂等命中：回吐首次结果，不重复扇出 / 不重复推送
                let key = idempotency_key.clone().unwrap_or_default();
                let existing = model::find_notice_by_idempotency(&ex, &key).await?;
                return Ok(Outcome::Duplicate(existing.map(|notice| {
                    (
                        notice.id,
                        notice.notice_no,
                        i64::from(notice.receiver_count),
                    )
                })));
            }

            let rows = fanout::fanout(&tx, &notice_id, &draft.target).await?;
            let receiver_count = model::refresh_receiver_count(&ex, &notice_id).await?;

            // 通道投递（A6 抽到 delivery 模块）：站内信必投（DELIVERED），
            // 邮件 / Webhook / 短信未配置记 SKIPPED —— 落库即「非失败」，不影响整条通知。
            let channels = delivery::deliver(&ex, &notice_id, mask, &settings, rows).await?;

            Ok(Outcome::Created {
                receiver_count,
                channels,
            })
        })
        .await
        .map_err(DispatchError::Db)?;

    match outcome {
        Outcome::Created {
            receiver_count,
            channels,
        } => {
            // ---------- 7. 实时推送（A6）：只推给「接收人 ∩ 在线用户」 ----------
            push_realtime(&notice_id, &push_seed).await;
            Ok(DispatchResult {
                notice_id,
                notice_no,
                receiver_count,
                duplicated: false,
                channels,
            })
        }
        Outcome::Duplicate(found) => match found {
            Some((id, no, count)) => Ok(DispatchResult {
                notice_id: id,
                notice_no: no,
                receiver_count: count,
                duplicated: true,
                channels: Vec::new(),
            }),
            None => Err(DispatchError::Api(ApiError::new(
                409,
                error_code::NOTICE_DUPLICATE_KEY,
                "同一幂等键的通知已存在，请检查 idempotency_key",
            ))),
        },
    }
}

/// 事务内结果（避免函数体内出现 `None` 语义不清的 `Option` 元组）。
enum Outcome {
    /// 新建成功。
    Created {
        /// 扇出后回写的接收人数。
        receiver_count: i64,
        /// 各通道结果。
        channels: Vec<ChannelState>,
    },
    /// 幂等命中（携带首次通知的 `(id, notice_no, receiver_count)`；理论上不会为 `None`）。
    Duplicate(Option<(uuid::Uuid, String, i64)>),
}

/// 把一条通知实时推给「接收人 ∩ 在线用户」（A6）。
///
/// # 参数
///
/// - `notice_id`：通知主键。
/// - `seed`：事件模板（`receiver_id` 逐接收人填充）。
///
/// # 为什么只查在线用户
///
/// 全员通知可能有 5 万接收人，而在线用户通常只有几十个。SQL 里用
/// `user_id IN (在线集合)` 让数据库只回这几十行，避免为一次推送拉 5 万行进来。
///
/// # 失败语义
///
/// **推送失败绝不影响发送结果**：通知已经落库（站内信已送达），SSE 只是「立即刷新」
/// 的加速器，查询或推送出错只记日志，前端轮询未读数兜底。
async fn push_realtime(notice_id: &uuid::Uuid, seed: &sse_hub::NoticeEvent) {
    let online = sse_hub::online_user_ids();
    if online.is_empty() {
        return;
    }
    match model::online_receivers(notice_id, &online).await {
        Ok(rows) => {
            let mut pushed = 0usize;
            for (user_id, receiver_id) in rows {
                let mut event = seed.clone();
                event.receiver_id = receiver_id.to_string();
                pushed += sse_hub::publish(user_id, &event);
            }
            tracing::info!(
                notice_id = %notice_id,
                online = online.len(),
                pushed,
                "通知已实时推送（SSE per-user 频道）"
            );
        }
        Err(error) => tracing::warn!(
            notice_id = %notice_id,
            error = %error,
            "查询在线接收人失败，跳过实时推送（站内信不受影响）"
        ),
    }
}

/// `Uuid` 序列化为字符串（`uuid` crate 在本项目未开 `serde` feature，
/// 而 API 契约里主键一律是字符串，与其它 DTO 保持一致）。
fn serialize_uuid<S: serde::Serializer>(
    id: &uuid::Uuid,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&id.to_string())
}

/// 生成通知编号 `NT + 类型前缀 + yyyyMMddHHmmss + UUID 末 8 位`（≤ 32 字符，唯一索引兜底）。
///
/// 取 UUID 的**末** 8 位而不是前 6 位：UUIDv7 的高位是时间戳，同一秒内生成的多条通知
/// 高位完全相同，用前 6 位会直接撞 `uk_sys_notice_no`（实测同一秒连发 2 条即 500）。
fn build_notice_no(id: &uuid::Uuid, notice_type: &NoticeType) -> String {
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let simple = id.simple().to_string();
    let tail = &simple[simple.len() - 8..];
    format!("NT{}{stamp}{tail}", notice_type_prefix(notice_type))
}

/// 通知编号前缀（按类型区分，便于运维 grep）。
fn notice_type_prefix(notice_type: &NoticeType) -> &'static str {
    match notice_type {
        NoticeType::System => "S",
        NoticeType::Todo => "T",
        NoticeType::Alert => "A",
        NoticeType::Workflow => "W",
        NoticeType::Circular => "C",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 通知编号：前缀 + 14 位时间戳 + 8 位尾巴，长度稳定且 ≤ 32（DB 列宽）。
    ///
    /// 关键回归点：同一实例在同一秒生成的两条通知编号必须不同（早期用 UUID 前 6 位，
    /// 因为 UUIDv7 高位是时间戳而撞 `uk_sys_notice_no`，实测同秒连发即 500）。
    #[test]
    fn notice_no_shape_is_stable() {
        let first = uuid::Uuid::now_v7();
        let second = uuid::Uuid::now_v7();
        let no = build_notice_no(&first, &NoticeType::Todo);
        assert!(no.starts_with("NTT"), "待办通知前缀应为 NTT：{no}");
        assert_eq!(no.chars().count(), 3 + 14 + 8);
        assert!(no.chars().count() <= 32);
        assert_ne!(
            build_notice_no(&first, &NoticeType::Todo),
            build_notice_no(&second, &NoticeType::Todo),
            "同一秒内不同通知的编号必须不同"
        );

        let no = build_notice_no(&first, &NoticeType::Circular);
        assert!(no.starts_with("NTC"));
        assert_eq!(
            notice_type_prefix(&NoticeType::System),
            "S",
            "前缀映射不可随意改动（运维按它 grep）"
        );
    }

    /// 通道状态只可能是 PENDING / SKIPPED（未配置不报错，绝不是 FAILED）。
    #[test]
    fn channel_status_mapping() {
        assert_eq!(delivery::status_for(true), "PENDING");
        assert_eq!(delivery::status_for(false), "SKIPPED");
        assert_ne!(delivery::status_for(false), "FAILED");
    }

    /// `dispatch` 的公共 API 形状必须稳定：C/D 批次按此签名调用（编译期契约）。
    ///
    /// 这里只**构造** Future 而不执行（判空断言随即可），因此不需要数据库，
    /// 可以在 `cargo test` 里跑；若 `dispatch` 的入参或返回类型被改动，这里编译失败。
    #[test]
    fn dispatch_api_shape_is_public() {
        fn assert_future<F>(_: F)
        where
            F: std::future::Future<Output = std::result::Result<DispatchResult, DispatchError>>,
        {
        }

        let draft = NoticeDraft::text(
            NoticeType::Todo,
            NoticeLevel::Info,
            "t",
            "c",
            NoticeTarget::All,
        );
        assert_eq!(draft.channel_mask, CHANNEL_INBOX);
        assert!(draft.idempotency_key.is_none());
        assert_eq!(draft.notice_type, NoticeType::Todo);
        assert_future(dispatch(draft));
        assert_eq!(delivery::status_for(true), "PENDING");
    }

    /// A6：通道计划把「模板通道 + 调用方通道」取并集，站内信必投。
    #[test]
    fn dispatch_channel_plan_is_union() {
        assert_eq!(
            delivery::plan_channels(Some(crate::business::notice::dto::CHANNEL_INBOX), 0),
            CHANNEL_INBOX
        );
        assert_eq!(
            delivery::plan_channels(Some(crate::business::notice::dto::CHANNEL_INBOX), 2),
            CHANNEL_INBOX | 2
        );
    }
}
