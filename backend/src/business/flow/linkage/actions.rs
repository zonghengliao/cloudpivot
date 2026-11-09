//! 跨单据联动：**动作实现**（白名单里真正会执行的代码）。
//!
//! # 铁律：只复用，不重写
//!
//! 每个动作的函数体都是「取参数 → 调既有实现 → 转成统一结果」，**不含任何业务算法**：
//!
//! | 动作 | 复用的既有实现 |
//! |------|----------------|
//! | [`SEND_NOTICE`](spec::ACTION_SEND_NOTICE) | `business::notice::dispatch`（渲染/扇出/偏好/幂等/投递记录） |
//! | [`UPDATE_RECEIVED_QTY`](spec::ACTION_UPDATE_RECEIVED_QTY) | `erp::purchase_model::POSTED_RECEIVED_SQL`（P2-3：累计收货量是派生值，本动作只校验不落库） |
//! | [`GENERATE_VOUCHER`](spec::ACTION_GENERATE_VOUCHER) | `erp::voucher::create_by_rule`（D3 抽出的唯一实现：取数 + 取号 + 借贷平衡 + 落库） |
//!
//! 其余 8 个动作仍是**占位**：参数契约已定、`spec.reuse` 指向要复用的既有函数，
//! 执行时返回 [`ActionError::Skipped`]（打 ERROR 日志 + 跳过），并且在配置写入阶段就被拒。
//!
//! 为什么不在这里「先把算法写一份」：同一个业务动作两套实现 = 双真相。
//! 收货过账的移动加权、凭证的借贷平衡、红冲的 reversed_by 回写都只有一份实现，
//! 复制到本模块就等于埋下「改一处漏一处、账按路径随机错」的雷。
//!
//! # 第 3 层校验也在这里
//!
//! 执行前统一校验 target 存在、上下文齐全（[`ActionError::Skipped`]）：
//!
//! - `SEND_NOTICE`：收件人非空 + 至少一个收件人在 `sys_user` 里存活；
//! - `UPDATE_RECEIVED_QTY`：采购明细行存在。
//!
//! 「校验不过」与「执行失败」是两件事：前者跳过（不中断流程），后者按
//! [`spec::Severity`] 决定回滚还是继续。

use std::result::Result;

use nik::prelude::chrono::{Duration, NaiveDate};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::stockin_model;
use crate::business::erp::voucher::{self, VoucherBizCtx};
use crate::business::notice::{self, NoticeDraft, NoticeTarget};
use crate::business::shared::state_machine;

use super::engine::LinkageContext;
use super::model::LinkageRuleRow;
use super::spec;

/// 动作执行结果的两类失败。
#[derive(Debug, Clone)]
pub enum ActionError {
    /// 第 3 层校验不过 / 上下文缺失 / 动作尚未接管 → **跳过**该动作（不中断流程）。
    Skipped(String),
    /// 执行失败 → 按 `spec.severity` 决定「整笔回滚」还是「记日志继续」。
    Failed(String),
}

impl ActionError {
    /// 人读原因。
    pub fn reason(&self) -> &str {
        match self {
            ActionError::Skipped(reason) | ActionError::Failed(reason) => reason,
        }
    }
}

// ==================== SEND_NOTICE ====================

/// 通知的发送模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeMode {
    /// 影子模式（默认）：只做**并行对比**，通知仍由触发点的旧代码发。
    Shadow,
    /// 接管模式：由配置发通知，旧代码不发。
    Takeover,
}

/// 收件人策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipientStrategy {
    /// 沿用触发点算出来的收件人（默认，零重复真相）。
    Legacy,
    /// 负责人。
    Owner,
    /// 创建人。
    Creator,
    /// 分派人。
    Assigner,
    /// 操作人。
    Operator,
}

/// `SEND_NOTICE` 的参数解析结果（第 2 层已在写配置时校验过，这里只做取值）。
#[derive(Debug, Clone)]
pub struct NoticeSetting {
    /// 发送模式。
    pub mode: NoticeMode,
    /// 覆盖标题（空 = 沿用触发点文案）。
    pub title: Option<String>,
    /// 覆盖正文（空 = 沿用触发点文案）。
    pub content: Option<String>,
    /// 覆盖强制送达（空 = 沿用触发点分级）。
    pub force: Option<bool>,
    /// 收件人策略。
    pub recipient: RecipientStrategy,
}

impl NoticeSetting {
    /// 从 `action_params` 取值（缺省即「全部沿用触发点」）。
    pub fn from_params(params: &serde_json::Value) -> Result<Self, String> {
        let text = |key: &str| -> Result<Option<String>, String> {
            match params.get(key) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(serde_json::Value::String(value)) => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(trimmed.to_string()))
                    }
                }
                Some(_) => Err(format!("参数 `{key}` 必须是字符串")),
            }
        };
        let mode = match text("mode")?.as_deref() {
            None | Some("SHADOW") => NoticeMode::Shadow,
            Some("TAKEOVER") => NoticeMode::Takeover,
            Some(other) => {
                return Err(format!("参数 `mode` 只允许 SHADOW / TAKEOVER（当前 {other}）"));
            }
        };
        let recipient = match text("recipient")?.as_deref() {
            None | Some("LEGACY") => RecipientStrategy::Legacy,
            Some("OWNER") => RecipientStrategy::Owner,
            Some("CREATOR") => RecipientStrategy::Creator,
            Some("ASSIGNER") => RecipientStrategy::Assigner,
            Some("OPERATOR") => RecipientStrategy::Operator,
            Some(other) => {
                return Err(format!(
                    "参数 `recipient` 只允许 LEGACY / OWNER / CREATOR / ASSIGNER / OPERATOR（当前 {other}）"
                ));
            }
        };
        let force = match params.get("force") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Bool(value)) => Some(*value),
            Some(_) => return Err("参数 `force` 必须是布尔".to_string()),
        };
        Ok(Self {
            mode,
            title: text("title")?,
            content: text("content")?,
            force,
            recipient,
        })
    }
}

/// 一条通知的发送结果（测试与日志用）。
#[derive(Debug, Clone)]
pub struct NoticeOutcome {
    /// 通知主键。
    pub notice_id: Uuid,
    /// 实际接收人数。
    pub receiver_count: i64,
    /// 是否命中幂等键（本次没有新发）。
    pub duplicated: bool,
}

/// 按配置把「触发点给的草稿」整形成「真正要发的草稿」。
///
/// 缺省行为 = **原样返回**（配置不写任何覆盖键时逐字等于旧逻辑，
/// 这是第 2 步「并行跑一段、对比一致」能成立的前提）。
pub fn shape_notice(
    base: &NoticeDraft,
    setting: &NoticeSetting,
    ctx: &LinkageContext<'_>,
) -> Result<NoticeDraft, ActionError> {
    let mut draft = base.clone();
    if let Some(title) = &setting.title {
        draft.title = title.clone();
    }
    if let Some(content) = &setting.content {
        draft.content = content.clone();
    }
    if let Some(force) = setting.force {
        draft.force = force;
    }
    let recipients: Vec<Uuid> = match setting.recipient {
        RecipientStrategy::Legacy => match &base.target {
            NoticeTarget::User(users) => users.clone(),
            _ => {
                return Err(ActionError::Skipped(
                    "SEND_NOTICE 只支持 NoticeTarget::User（收件人必须是具体的人，\
                     角色/部门展开会让「同一配置每次发给不同人」）"
                        .to_string(),
                ));
            }
        },
        RecipientStrategy::Owner => ctx.owner.into_iter().collect(),
        RecipientStrategy::Creator => ctx.creator.into_iter().collect(),
        RecipientStrategy::Assigner => ctx.assigner.into_iter().collect(),
        RecipientStrategy::Operator => ctx.operator.into_iter().collect(),
    };
    if recipients.is_empty() {
        return Err(ActionError::Skipped(
            "SEND_NOTICE：收件人为空（触发点没有可用的人），跳过本次通知".to_string(),
        ));
    }
    draft.target = NoticeTarget::User(recipients);
    if draft.title.trim().is_empty() || draft.content.trim().is_empty() {
        return Err(ActionError::Skipped(
            "SEND_NOTICE：标题或正文为空（未配置覆盖文案时无法凭空生成），跳过本次通知".to_string(),
        ));
    }
    Ok(draft)
}

/// 第 3 层：收件人必须至少有一个存活账号（否则发出去也没人收到）。
async fn ensure_recipients_alive(recipients: &[Uuid]) -> Result<(), ActionError> {
    // SCOPE-EXEMPT: 只数「这批 uuid 里还有几个存活账号」，用于发送前的存在性校验；
    // 注入数据范围谓词会让「收件人是别人部门的人」这种正常情况被静默判成不存在。
    let array = recipients.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
    let alive: i64 = db()
        .scalar_sql(
            "SELECT count(*) FROM sys_user \
              WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(format!("{{{array}}}"))],
        )
        .await
        .map_err(|error| ActionError::Failed(format!("校验通知收件人失败：{error}")))?;
    if alive == 0 {
        return Err(ActionError::Skipped(format!(
            "SEND_NOTICE：{} 个收件人在 sys_user 里都不存在（或已删除），跳过本次通知",
            recipients.len()
        )));
    }
    Ok(())
}

/// 发一条通知（`Phase::AfterCommit`，非关键动作）：整形 + 发送。
pub async fn send_notice(rule: &LinkageRuleRow, ctx: &LinkageContext<'_>) -> Result<NoticeOutcome, ActionError> {
    let (draft, _setting) = prepare_notice(rule, ctx).await?;
    dispatch_notice(draft).await
}

/// 只做「整形 + 第 3 层校验」，不发（影子对比与接管两条路共用同一份整形逻辑）。
pub async fn prepare_notice(
    rule: &LinkageRuleRow,
    ctx: &LinkageContext<'_>,
) -> Result<(NoticeDraft, NoticeSetting), ActionError> {
    let setting = NoticeSetting::from_params(&rule.action_params)
        .map_err(|reason| ActionError::Skipped(format!("SEND_NOTICE 参数非法：{reason}")))?;
    let Some(base) = ctx.notice.as_ref() else {
        return Err(ActionError::Skipped(
            "SEND_NOTICE：触发点没有提供通知上下文（标题/正文/收件人都无从谈起），跳过".to_string(),
        ));
    };
    let draft = shape_notice(base, &setting, ctx)?;
    let recipients = match &draft.target {
        NoticeTarget::User(users) => users.clone(),
        _ => Vec::new(),
    };
    ensure_recipients_alive(&recipients).await?;
    Ok((draft, setting))
}

/// 真正投递一份已整形的草稿。
pub async fn dispatch_notice(draft: NoticeDraft) -> Result<NoticeOutcome, ActionError> {
    match notice::dispatch(draft).await {
        Ok(result) => Ok(NoticeOutcome {
            notice_id: result.notice_id,
            receiver_count: result.receiver_count,
            duplicated: result.duplicated,
        }),
        Err(error) => Err(ActionError::Failed(format!("发送通知失败：{error}"))),
    }
}

/// 影子对比：配置发出来的通知与触发点旧逻辑算出来的通知是否一致。
///
/// 返回**不一致的字段名**（空 = 完全一致）。第 2 步的验收就是靠它：
/// 「并行跑一段、对比结果一致」在日志里是可 grep 的事实，不是口头保证。
pub fn compare_notices(configured: &NoticeDraft, base: &NoticeDraft) -> Vec<&'static str> {
    let mut diffs: Vec<&'static str> = Vec::new();
    if configured.title != base.title {
        diffs.push("title");
    }
    if configured.content != base.content {
        diffs.push("content");
    }
    if configured.force != base.force {
        diffs.push("force");
    }
    if configured.notice_type != base.notice_type {
        diffs.push("notice_type");
    }
    if configured.level != base.level {
        diffs.push("level");
    }
    if configured.link_url != base.link_url {
        diffs.push("link_url");
    }
    if configured.biz_type != base.biz_type {
        diffs.push("biz_type");
    }
    if configured.biz_id != base.biz_id {
        diffs.push("biz_id");
    }
    if configured.sender_id != base.sender_id {
        diffs.push("sender_id");
    }
    if configured.sender_name != base.sender_name {
        diffs.push("sender_name");
    }
    if configured.template_code != base.template_code {
        diffs.push("template_code");
    }
    if recipient_ids(configured) != recipient_ids(base) {
        diffs.push("recipients");
    }
    diffs
}

/// 收件人集合（排序去重后比较：顺序不是语义）。
fn recipient_ids(draft: &NoticeDraft) -> Vec<Uuid> {
    let mut ids = match &draft.target {
        NoticeTarget::User(users) => users.clone(),
        _ => Vec::new(),
    };
    ids.sort();
    ids.dedup();
    ids
}

// ==================== UPDATE_RECEIVED_QTY ====================

/// 校验采购明细的累计收货量（`Phase::InTx`，**关键**动作）。
///
/// # P2-3：从「写一列」改成「校验一处」
///
/// 动作码保留（配置与 `ck_flow_linkage_action_code` 不用迁移），语义变成
/// **校验**：`已过账收货量 + 本次 qty ≤ 订购量`，超了就是关键动作失败
/// （调用方整笔回滚）。为什么不再回写：`biz_purchase_order_item.received_qty`
/// 列已被迁移 V0217 删除，累计收货量是派生值
/// （[`crate::business::erp::purchase_model::POSTED_RECEIVED_SQL`]：只算已过账收货单）。
///
/// 口径复用同一个表达式，且**减掉触发单据自己的贡献**：过账流程里本动作跑在
/// 单据已经是 `POSTED` 之后，`POSTED_RECEIVED_SQL` 已经把本次收货算进去了，
/// 直接 `+ qty` 会把自己算两遍（假超收）。所以：
///
/// ```text
/// 其他单据已过账收货 = POSTED_RECEIVED_SQL − 本单(source_id = ctx.doc_id)贡献
/// 判据：其他单据已过账收货 + 本次 qty ≤ 订购量
/// ```
///
/// 这与过账闸 `guard_purchase_before_post` 的判据**同源**（都读 `POSTED_RECEIVED_SQL`），
/// 差别只是过账闸在状态变更**之前**跑（本单还没进 POSTED 集合，天然不用减）。
pub async fn update_received_qty(
    ex: &impl SqlExecutor,
    rule: &LinkageRuleRow,
    ctx: &LinkageContext<'_>,
) -> Result<String, ActionError> {
    let Some(source_item_id) =
        text_param(&rule.action_params, "source_item_id").and_then(|text| Uuid::parse_str(&text).ok())
    else {
        return Err(ActionError::Skipped(
            "UPDATE_RECEIVED_QTY：参数 source_item_id 缺失或不是合法 UUID，跳过".to_string(),
        ));
    };
    let Some(qty) = decimal_param(&rule.action_params, "qty") else {
        return Err(ActionError::Skipped(
            "UPDATE_RECEIVED_QTY：参数 qty 缺失或不是十进制数，跳过".to_string(),
        ));
    };
    // 第 3 层：明细行必须存在（不存在 = target 不在，跳过而不是「判成超收」）
    let sql = format!(
        "SELECT i.qty AS order_qty, \
                {posted} AS posted_total, \
                COALESCE((SELECT SUM(si.qty) FROM biz_stock_in s \
                           JOIN biz_stock_in_item si ON si.stock_in_id = s.id \
                          WHERE s.source_order_id = i.order_id \
                            AND si.source_item_id = i.id \
                            AND s.status = 'POSTED' \
                            AND s.deleted_at IS NULL \
                            AND s.id = ?::uuid), 0) AS posted_self \
           FROM biz_purchase_order_item i \
          WHERE i.id = ?::uuid",
        posted = crate::business::erp::purchase_model::POSTED_RECEIVED_SQL
    );
    let rows = ex
        .query_sql(sql, vec![Value::from(&ctx.doc_id), Value::from(&source_item_id)])
        .await
        .map_err(|error| ActionError::Failed(format!("校验采购明细行失败：{error}")))?;
    let Some(row) = rows.first() else {
        return Err(ActionError::Skipped(format!(
            "UPDATE_RECEIVED_QTY：采购明细行 {source_item_id} 不存在，跳过（target 不在）"
        )));
    };
    let order_qty: Decimal = row
        .get_typed("order_qty")
        .map_err(|error| ActionError::Failed(format!("读订购量失败：{error}")))?;
    let posted_total: Decimal = row
        .get_typed("posted_total")
        .map_err(|error| ActionError::Failed(format!("读已过账收货量失败：{error}")))?;
    let posted_self: Decimal = row
        .get_typed("posted_self")
        .map_err(|error| ActionError::Failed(format!("读本单已过账收货量失败：{error}")))?;
    let posted_other = posted_total - posted_self;
    if posted_other + qty > order_qty {
        return Err(ActionError::Failed(format!(
            "UPDATE_RECEIVED_QTY：采购明细行 {source_item_id} 其他单据已收 {posted_other} + \
             本次 {qty} 超过订购量 {order_qty}（超收），关键动作失败 → 本次状态变更整笔回滚"
        )));
    }
    Ok(format!(
        "采购明细 {source_item_id} 累计收货校验通过：其他单据已收 {posted_other} + 本次 {qty} \
         ≤ 订购量 {order_qty}（P2-3 起累计收货量为派生值，不落库）"
    ))
}

// ==================== GENERATE_VOUCHER ====================

/// 凭证动作的上线模式（与 `SEND_NOTICE` 的 `mode` 同义，但独立成类型：
/// 通知的「接管」与凭证的「接管」不该被同一个枚举绑死，将来一个加值另一个不必跟着改）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoucherMode {
    /// 影子模式（默认）：配置路径**试算一遍并回滚**，与旧路径的凭证逐字段对比，凭证仍由旧逻辑落。
    Shadow,
    /// 接管模式：由配置生成凭证，旧路径本次不生成。
    Takeover,
}

/// `GENERATE_VOUCHER` 的参数解析结果（第 2 层已在写配置时校验过，这里只做取值）。
#[derive(Debug, Clone)]
pub struct VoucherSetting {
    /// 上线模式。
    pub mode: VoucherMode,
    /// 映射模板（`biz_voucher_rule.rule_code`；空 = 按 `biz_type` 取默认模板）。
    pub template_code: Option<String>,
    /// 业务类型（必填）。
    pub biz_type: String,
    /// 记账日期覆盖（空 = 取单据业务日期）。
    pub account_date: Option<String>,
}

impl VoucherSetting {
    /// 从 `action_params` 取值。
    pub fn from_params(params: &serde_json::Value) -> Result<Self, String> {
        let text = |key: &str| -> Result<Option<String>, String> {
            match params.get(key) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(serde_json::Value::String(value)) => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(trimmed.to_string()))
                    }
                }
                Some(_) => Err(format!("参数 `{key}` 必须是字符串")),
            }
        };
        let mode = match text("mode")?.as_deref() {
            None | Some("SHADOW") => VoucherMode::Shadow,
            Some("TAKEOVER") => VoucherMode::Takeover,
            Some(other) => {
                return Err(format!("参数 `mode` 只允许 SHADOW / TAKEOVER（当前 {other}）"));
            }
        };
        let Some(biz_type) = text("biz_type")? else {
            return Err("参数 `biz_type` 必填（空 = 不知道该用哪套科目映射）".to_string());
        };
        Ok(Self {
            mode,
            template_code: text("template_code")?,
            biz_type,
            account_date: text("account_date")?,
        })
    }

    /// 记账日期（`None` = 沿用单据日期）；格式非法返回错误文案。
    pub fn voucher_date(&self) -> Result<Option<NaiveDate>, String> {
        let Some(text) = self.account_date.as_deref() else {
            return Ok(None);
        };
        NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| format!("参数 `account_date` 必须是 yyyy-MM-dd（当前 {text}）"))
    }
}

/// 一次凭证动作的执行产物（引擎的日志与 SHADOW 对比都用它）。
#[derive(Debug, Clone)]
pub struct VoucherOutcome {
    /// 落下的凭证 id。
    pub voucher_id: Uuid,
    /// 生效的配置（模式 / 模板 / 记账日期覆盖）。
    pub setting: VoucherSetting,
    /// 取数结果（取号前的上下文：金额 / 摘要 / 关联单据）。
    pub biz_ctx: VoucherBizCtx,
}

/// 第 3 层校验 + 取数（**不写库**）。
///
/// 顺序很重要：先看参数（便宜），再看模板（一次查询），最后取单据（一次查询 + 一次查询）。
/// 「target 不存在」的三类都在这里被拦成 [`ActionError::Skipped`]：
///
/// 1. `GENERATE_VOUCHER` 目前只接管收货入库（`STOCK_IN`）的凭证 —— 别的实体没有
///    「取数」定义（付款 / 出库各有各的金额口径），硬套会生成错的账；
/// 2. 模板停用 / 模板码不存在（`biz_voucher_rule` 里没有存活规则）；
/// 3. 单据不存在 / 金额全 0（会计上不构成凭证）。
///
/// 取数走 `stockin_model::voucher_biz_ctx` → `VoucherBizCtx::of_stock_in`，
/// 与收货过账**同一个函数**，不在这里另算金额或摘要。
pub async fn prepare_voucher(
    ex: &impl SqlExecutor,
    rule: &LinkageRuleRow,
    ctx: &LinkageContext<'_>,
) -> Result<(VoucherSetting, VoucherBizCtx), ActionError> {
    let setting = VoucherSetting::from_params(&rule.action_params)
        .map_err(|reason| ActionError::Skipped(format!("GENERATE_VOUCHER 参数非法：{reason}")))?;
    if ctx.entity != state_machine::ENTITY_STOCK_IN {
        return Err(ActionError::Skipped(format!(
            "GENERATE_VOUCHER 目前只接管收货入库（STOCK_IN）的凭证，\
             触发实体 {} 还没有对应的取数口径（付款 / 出库的金额来源不同），跳过",
            ctx.entity
        )));
    }
    let account_date = setting
        .voucher_date()
        .map_err(|reason| ActionError::Skipped(format!("GENERATE_VOUCHER {reason}")))?;
    if let Some(code) = setting.template_code.as_deref() {
        let active = voucher::template_active(ex, code)
            .await
            .map_err(|error| ActionError::Failed(format!("校验凭证模板 {code} 失败：{error}")))?;
        if !active {
            return Err(ActionError::Skipped(format!(
                "GENERATE_VOUCHER：凭证模板 {code} 未启用（biz_voucher_rule 里没有这条存活规则），\
                 跳过（target 不在）"
            )));
        }
    }
    let Some(biz_ctx) = stockin_model::voucher_biz_ctx(ex, ctx.doc_id)
        .await
        .map_err(|error| ActionError::Failed(format!("取收货单 {} 的凭证上下文失败：{error}", ctx.doc_id)))?
    else {
        return Err(ActionError::Skipped(format!(
            "GENERATE_VOUCHER：单据 {}（{}）不存在或已删除，跳过（target 不在）",
            ctx.doc_id, ctx.doc_no
        )));
    };
    // 单据不完整：金额全 0 → 分录凑不出 2 行 → 会计上不构成凭证。这不是「失败」，
    // 是「这张单本来就不该有凭证」，所以跳过（旧逻辑同样会因为 len<2 报错，但那是
    // 过账路径的护栏；配置路径上把它当 target 不在，避免因一张 0 元单回滚整笔过账）。
    if biz_ctx.amounts.total() <= Decimal::ZERO && biz_ctx.amounts.amount <= Decimal::ZERO {
        return Err(ActionError::Skipped(format!(
            "GENERATE_VOUCHER：单据 {} 金额全为 0（不含税 {} / 税 {} / 价税合计 {}），\
             会计上不构成一张凭证，跳过",
            biz_ctx.source_no, biz_ctx.amounts.amount, biz_ctx.amounts.tax, biz_ctx.amounts.with_tax
        )));
    }
    let biz_ctx = match account_date {
        Some(date) => biz_ctx.with_voucher_date(date),
        None => biz_ctx,
    };
    Ok((setting, biz_ctx))
}

/// 生成一张凭证（`Phase::InTx`，**关键**动作）。
///
/// # 铁律：不新写任何取号 / 平衡算法
///
/// 整个函数体只有「校验取数 + 一句 [`voucher::create_by_rule`]」—— 取数、取号、≥2 行、
/// 借贷平衡、期间已结账、写主表 + 分录全在那一份实现里（收货过账的旧路径也是它）。
///
/// # 失败语义（第 3 步的验收点）
///
/// - 校验 / target 不在 → [`ActionError::Skipped`]（引擎打 ERROR + 调用方退回旧逻辑）；
/// - 取号失败 / 借贷不平 / 期间已结账 → [`ActionError::Failed`] → 关键动作 →
///   调用方**整笔回滚**（不会留下半张凭证，也不会留下「库存加了但没凭证」）。
pub async fn generate_voucher(
    ex: &impl SqlExecutor,
    rule: &LinkageRuleRow,
    ctx: &LinkageContext<'_>,
) -> Result<VoucherOutcome, ActionError> {
    let (setting, biz_ctx) = prepare_voucher(ex, rule, ctx).await?;
    let voucher_id = commit_voucher(ex, &setting, &biz_ctx, ctx).await?;
    Ok(VoucherOutcome {
        voucher_id,
        setting,
        biz_ctx,
    })
}

/// 真正落账的那一步（取数已在 [`prepare_voucher`] 完成）。
///
/// 引擎的 SHADOW 分支会在 `SAVEPOINT` 里单独调它：跑一遍 → 取快照 → 回滚，
/// 这样「配置路径会生成什么」是**真跑出来的**，而不是照旧逻辑推算的。
pub async fn commit_voucher(
    ex: &impl SqlExecutor,
    setting: &VoucherSetting,
    biz_ctx: &VoucherBizCtx,
    ctx: &LinkageContext<'_>,
) -> Result<Uuid, ActionError> {
    voucher::create_by_rule(ex, setting.template_code.as_deref(), biz_ctx, ctx.operator, ctx.now)
        .await
        .map_err(|error| ActionError::Failed(format!("GENERATE_VOUCHER 生成凭证失败（关键动作，整笔回滚）：{error}")))
}

// ==================== 提交后动作：建工作任务 ====================

/// 建一条工作任务（`Phase::AfterCommit`，**非关键**动作）。
///
/// # 为什么它自己开事务
///
/// `Phase::AfterCommit` 动作在业务事务**提交之后**执行（[`super::engine::run_deferred`]），
/// 那时调用方的事务已经结束；而 `task::model::create_task` 吃的是 `&Transaction`
/// （取号要靠 `pg_advisory_xact_lock` 的事务级锁）。所以这里 `db().begin()` 开一个**自己的**
/// 短事务：落库成功即提交，失败只回 [`ActionError::Failed`]，**永不回滚业务**
/// （发不出待办任务不能把「已经过账」撤销）。
///
/// # 只建「自由任务」不挂单据
///
/// `biz_task.doc_type` 的值域是 `DocKind ∩ task/doc_ref.rs::DOC_TARGETS`
/// （`PURCHASE_ORDER` / `STOCK_IN` / `CONTRACT` / `PAYABLE` / `VOUCHER`），
/// 联动触发的单据类型里多数不在这个白名单内（如 `STOCK_OUT` / `EXPENSE`）。
/// 为了让「任何触发点都能配」，这里只落自由任务，源单号写进标题与描述
/// （可读、可 grep）；要挂单据关联得先扩 `DOC_TARGETS`，那是另一张卡的事，不在这里偷跑。
pub async fn create_task(rule: &LinkageRuleRow, ctx: &LinkageContext<'_>) -> Result<String, ActionError> {
    let Some(title) = text_param(&rule.action_params, "title") else {
        return Err(ActionError::Skipped(
            "CREATE_TASK：title 必填（任务标题不能凭空生成）".to_string(),
        ));
    };
    let strategy = text_param(&rule.action_params, "assignee_strategy").unwrap_or_else(|| "OPERATOR".to_string());
    let owner = match strategy.as_str() {
        "DOC_CREATOR" => ctx.creator,
        _ => ctx.operator,
    };
    let Some(owner) = owner else {
        return Err(ActionError::Skipped(format!(
            "CREATE_TASK：assignee_strategy = {strategy} 在触发点没有对应的人，跳过建任务"
        )));
    };
    let creator = ctx.creator.or(ctx.operator).unwrap_or(owner);
    // 当前 CST 日期 → 月份键（编号按月递增，与任务模块同一口径，不另写一份）
    let today = voucher::today_cst(ctx.now);
    let month = crate::business::task::model::month_key(today);
    let due_days = rule.action_params.get("due_days").and_then(|value| value.as_i64());
    let due_date = due_days.map(|days| today + Duration::days(days));
    let assignment = if owner == creator { "SELF" } else { "ASSIGNED" };
    let new = crate::business::task::model::NewTask {
        id: Uuid::now_v7(),
        title,
        description: text_param(&rule.action_params, "content"),
        priority: "P2".to_string(),
        need_review: false,
        owner_id: owner,
        creator_id: creator,
        assigner_id: if owner == creator { None } else { ctx.operator },
        dept_id: None,
        parent_id: None,
        project_id: None,
        // 自由任务（见函数文档：doc_type 的值域不含多数联动触发点）
        doc_type: None,
        doc_id: None,
        start_date: None,
        due_date,
        estimate_hours: None,
        source: assignment.to_string(),
        add_creator_as_collaborator: false,
    };
    let operator = ctx.operator.unwrap_or(creator);
    let tx = db()
        .begin()
        .await
        .map_err(|error| ActionError::Failed(format!("CREATE_TASK：开启事务失败：{error}")))?;
    let task_no = match crate::business::task::model::create_task(&tx, &new, operator, &month).await {
        Ok(task_no) => task_no,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(ActionError::Failed(format!(
                "CREATE_TASK：任务落库失败（非关键动作，业务不回滚）：{error}"
            )));
        }
    };
    tx.commit()
        .await
        .map_err(|error| ActionError::Failed(format!("CREATE_TASK：提交任务事务失败：{error}")))?;
    Ok(format!(
        "任务 {task_no} 已创建（负责人 {owner}，来源单据 {} {})",
        ctx.entity, ctx.doc_no
    ))
}

// ==================== 统一入口 ====================

/// 执行一个**事务内**动作（`Phase::InTx`）。
///
/// `ex` 必须是调用方自己的事务执行器：关键动作失败时由调用方回滚整笔
/// （引擎不在这里开事务，理由见 `spec` 模块文档）。
pub async fn run_in_tx(
    ex: &impl SqlExecutor,
    rule: &LinkageRuleRow,
    ctx: &LinkageContext<'_>,
) -> Result<String, ActionError> {
    match rule.action_code.as_str() {
        spec::ACTION_UPDATE_RECEIVED_QTY => update_received_qty(ex, rule, ctx).await,
        spec::ACTION_GENERATE_VOUCHER => generate_voucher(ex, rule, ctx)
            .await
            .map(|outcome| format!("凭证 {} 已生成（{}）", outcome.voucher_id, outcome.biz_ctx.summary)),
        other => Err(ActionError::Skipped(not_taken_over(other))),
    }
}

/// 执行一个**提交后**动作（`Phase::AfterCommit`）。
pub async fn run_after_commit(rule: &LinkageRuleRow, ctx: &LinkageContext<'_>) -> Result<String, ActionError> {
    match rule.action_code.as_str() {
        spec::ACTION_SEND_NOTICE => send_notice(rule, ctx).await.map(|outcome| {
            format!(
                "通知 {} 已发送（接收人 {}，duplicated={}）",
                outcome.notice_id, outcome.receiver_count, outcome.duplicated
            )
        }),
        spec::ACTION_CREATE_TASK => create_task(rule, ctx).await,
        other => Err(ActionError::Skipped(not_taken_over(other))),
    }
}

/// 「尚未接管」的统一文案（占位动作与未实现分支共用）。
fn not_taken_over(code: &str) -> String {
    format!(
        "动作 {code} 尚未接管（第 1 步只建表 + 引擎）：占位动作不执行、不写任何数据。\
         接管要等第 3 步复用既有 handler（见 spec.rs 的 reuse 指针）"
    )
}

/// 取字符串参数（已由第 2 层校验过类型，这里只做容错）。
fn text_param(params: &serde_json::Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(|value| value.as_str())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 取十进制参数。
fn decimal_param(params: &serde_json::Value, key: &str) -> Option<nik::prelude::rust_decimal::Decimal> {
    use std::str::FromStr;
    let text = match params.get(key)? {
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(text) => text.trim().to_string(),
        _ => return None,
    };
    nik::prelude::rust_decimal::Decimal::from_str(&text).ok()
}
