//! ERP 内控开关：**制单 ≠ 审批 / 过账 / 付款**（可配，默认不改变现有行为）。
//!
//! # 为什么做成开关而不是硬分离
//!
//! 内控上「制单人不能自己审批 / 过账 / 付款」是正解，但**小团队常常只有一个人能干活**：
//! 硬分离会让单据谁也点不动（历史上还踩过会签死锁，见 [`crate::business::flow::internal_control`]）。
//! 因此三项职责拆成三个独立开关，由管理员在「支撑功能 / 参数配置 / ERP 内控」里按需打开；
//! **默认值与改动前逐字一致**（见下表），升级不影响任何存量用户。
//!
//! | 开关键 | 默认 | 打开后拦什么 | 没打开时的行为 |
//! |---|---|---|---|
//! | `erp.separation_of_duties.enabled` | `false` | **一键开启**总闸：把未单独配置的项默认成「开」 | 不参与判定（只作缺省值来源） |
//! | `erp.separation_of_duties.post` | `false` | 制单人不能**过账**（收货入库单直接过账 / 调拨发货收货） | 与改动前一致：允许同人过账 |
//! | `erp.separation_of_duties.approve` | `true` | 制单人 / 发起人不能**审批**（采购 / 收货过账 / 作废 / 凭证红冲 / 应付付款与作废） | 与改动前一致：内控闸照旧拦 |
//! | `erp.separation_of_duties.pay` | `true` | 制单人不能**审批自己提交的付款**（P17 闸） | 与改动前一致：P17 闸照旧拦 |
//! | `erp.separation_of_duties.approve_execute` | `false` | **审批人**不能自己**执行**（付款执行 / 盘点过账） | 与改动前一致：审批通过后由本人（或系统）执行 |
//! | `erp.separation_of_duties.stocktake` | `false` | **制单人**不能**确认自己建的盘点单**（DRAFT → 待过账的提交确认） | 与改动前一致：制单人可自行确认提交 |
//! | `erp.separation_of_duties.purchase_accept` | `false` | **采购员**（采购单 `create_by`）不能**验收**自己采的货（建收货单确认收货） | 与改动前一致：采购员可自己收货 |
//!
//! 前四项是 P1/ERP-B2 就有的老开关（默认值 = 改动前的真实行为）；
//! 后三项是 P3-07 新增的 SoD 扩展，**默认全部 `false`**（纯增量、升级零行为变化）。
//!
//! # 取值优先级（参数配置 > 代码默认）
//!
//! ```text
//! ① 本项自己的键（post / approve / pay / approve_execute / stocktake / purchase_accept）
//!    存在且能解析成 bool                                     → 用它
//! ② 总闸 enabled 存在且为 true                              → true（一键开启，只升不降）
//! ③ 否则                                                     → 代码默认（见上表）
//! ```
//!
//! **总闸只升不降**是刻意的：种子把 `enabled` 写成 `false`，如果它还能「降级」，
//! 那这一行就会把默认开启的审批 / 付款内控闸一起关掉（等于静默放宽内控）。
//! 要单独关某一项，就显式把该项设成 `false`。
//!
//! # 读性能与生效时延
//!
//! 取值走 [`crate::support::config::model`] 的**进程内运行时快照**（命中是纯内存读，
//! 不会每次查库），TTL 60 秒；管理端改参数会广播重载，**立刻生效**（多实例另有版本号轮询）。
//! 非法值（不是 `true/false/1/0`）回退代码默认并打 `warn`，绝不静默当成 `false`
//! ——把它当 `false` 等于「配置写错 = 内控被关」，那是更危险的方向。
//!
//! # 边界（刻意不做的事）
//!
//! - **手工凭证**（`voucher_write::create_manual_voucher`）是「制单即记账」的单步动作，
//!   `create_by` 永远是操作人本人，同一动作内无法分离；要分离必须拆「制单草稿 + 他人记账」
//!   两步（新状态 + 新接口），不在本开关范围。自动凭证（收货 / 付款产生）随单据过账一起生成，
//!   已由调用方（过账 / 付款）的内控闸覆盖。
//! - **审批流里生成待办时的排除**（SKIPPED 留痕）读的是 `approve` 这一项，
//!   关掉之后制单人能重新看到自己的待办并批得动 —— 与「403 闸关闭」是同一件事的两面。
//! - **`approve_execute` 只管「人工执行」点**（盘点过账、付款执行）。审批通过后由引擎
//!   **自动**动账的动作（收货入库单过账、应付付款回调）不算「执行人」—— 那是系统替已批准的
//!   决定记账，没有第二个人的自由裁量可分离；在这些路径上本项**不拦**，只留 `info` 日志。
//!   付款路径的「人工执行」入口是 `POST /business/erp/payable/payment/{id}/execute`
//!   （审批通过后由**另一个人**执行才能记账）。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;
use crate::support::config::model::{self as config_model, SysConfigModel};

/// 总闸：一键把未单独配置的项默认成「开」（`true` 只升不降）。
pub const MASTER_KEY: &str = "erp.separation_of_duties.enabled";
/// 制单 ↔ 过账分离。
pub const POST_KEY: &str = "erp.separation_of_duties.post";
/// 制单 ↔ 审批分离。
pub const APPROVE_KEY: &str = "erp.separation_of_duties.approve";
/// 制单 ↔ 付款分离。
pub const PAY_KEY: &str = "erp.separation_of_duties.pay";
/// 审批 ↔ 执行分离（P3-07）：审批人不能自己执行（付款执行 / 盘点过账）。
pub const APPROVE_EXECUTE_KEY: &str = "erp.separation_of_duties.approve_execute";
/// 制单 ↔ 盘点确认分离（P3-07）：制单人不能确认自己建的盘点单。
pub const STOCKTAKE_KEY: &str = "erp.separation_of_duties.stocktake";
/// 采购 ↔ 验收分离（P3-07）：采购员不能验收自己采的货。
pub const PURCHASE_ACCEPT_KEY: &str = "erp.separation_of_duties.purchase_accept";

/// 受管的职责（每一项对应一个开关 + 一段拦截文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Duty {
    /// 过账（收货入库单过账 / 调拨发货收货）。
    Post,
    /// 审批（各单据的审批待办）。
    Approve,
    /// 付款（应付付款审批）。
    Pay,
    /// 执行（审批通过之后真正动账的那一步：付款执行 / 盘点过账）。
    ApproveExecute,
    /// 盘点确认（草稿盘点单 → 待过账的提交确认）。
    Stocktake,
    /// 验收入库（把采购订单的货建成收货单）。
    PurchaseAccept,
}

impl Duty {
    /// 全部职责（枚举 / 诊断用）。
    pub const ALL: [Duty; 6] = [
        Duty::Post,
        Duty::Approve,
        Duty::Pay,
        Duty::ApproveExecute,
        Duty::Stocktake,
        Duty::PurchaseAccept,
    ];

    /// 本项的开关键（`sys_config.config_key`）。
    pub const fn key(self) -> &'static str {
        match self {
            Duty::Post => POST_KEY,
            Duty::Approve => APPROVE_KEY,
            Duty::Pay => PAY_KEY,
            Duty::ApproveExecute => APPROVE_EXECUTE_KEY,
            Duty::Stocktake => STOCKTAKE_KEY,
            Duty::PurchaseAccept => PURCHASE_ACCEPT_KEY,
        }
    }

    /// 本项的**代码默认值**（参数缺失 / 非法时用它）。
    ///
    /// 三项默认值 = 改动前的真实行为：过账本来不拦、审批与付款本来就有内控闸。
    /// 改这三个值等于静默改内控强度，**必须配迁移与说明**，不许随手改。
    ///
    /// P3-07 的三个新项一律 `false`：它们是**新能力**，任何一项默认 `true` 都会让
    /// 升级后的存量环境凭空多出拦截，那是升级事故而不是内控加固。
    pub const fn default_enabled(self) -> bool {
        match self {
            Duty::Post | Duty::ApproveExecute | Duty::Stocktake | Duty::PurchaseAccept => false,
            Duty::Approve | Duty::Pay => true,
        }
    }

    /// 开关的中文名（写进拦截文案与参数说明）。
    ///
    /// **名字里必须写清这一对职责**：前端直接展示这句话，管理员才知道自己开的是什么。
    pub const fn label(self) -> &'static str {
        match self {
            Duty::Post => "制单与过账分离",
            Duty::Approve => "制单与审批分离",
            Duty::Pay => "制单与付款分离",
            Duty::ApproveExecute => "审批与执行分离",
            Duty::Stocktake => "制单与盘点确认分离",
            Duty::PurchaseAccept => "采购与验收分离",
        }
    }

    /// 被拦下的那个动作（`不能自己{动作}`）。
    pub const fn action(self) -> &'static str {
        match self {
            Duty::Post => "过账",
            Duty::Approve => "审批",
            Duty::Pay => "付款审批",
            Duty::ApproveExecute => "执行",
            Duty::Stocktake => "确认",
            Duty::PurchaseAccept => "验收",
        }
    }

    /// 拦截文案里「对方是谁」的定语（`{单据}{定语}`）。
    ///
    /// 老三项是「制单人」口径 → `由你本人制作`（**逐字保留**，十余条用例与前端文案引用它）；
    /// 新三项的职责对不同，定语必须跟着变，否则文案会说出事实错误的话
    /// （「付款单 PM-1 由你本人制作，不能自己执行」——其实是「由你本人审批」）。
    pub const fn counterpart_clause(self) -> &'static str {
        match self {
            Duty::Post | Duty::Approve | Duty::Pay | Duty::Stocktake => "由你本人制作",
            Duty::ApproveExecute => "由你本人审批",
            Duty::PurchaseAccept => "由你本人采购",
        }
    }
}

/// 生效值从哪来（诊断 / 单测用；日志里带上它，排查「为什么没生效」不用猜）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 来自本项自己的键。
    Own,
    /// 来自总闸 `enabled = true`。
    Master,
    /// 来自代码默认值。
    Default,
}

/// 一次开关解析的完整结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// 最终是否开启。
    pub enabled: bool,
    /// 生效值的来源。
    pub source: Source,
    /// 读到了但解析不出来的原文（`Some` = 调用方应当 `warn`）。
    pub invalid: Option<String>,
}

/// 纯函数：按「本项 → 总闸 → 代码默认」解析开关值（**不碰库**，便于单测钉死优先级）。
///
/// # 参数
///
/// - `duty`：哪一项职责。
/// - `own`：本项键的原始文本（`None` = 参数不存在 / 已停用 / 值为空）。
/// - `master`：总闸键的原始文本（同上）。
///
/// # 返回值
///
/// [`Resolution`]；非法文本不参与判定，只记在 `invalid` 里由调用方打日志。
pub fn resolve(duty: Duty, own: Option<&str>, master: Option<&str>) -> Resolution {
    let mut invalid: Option<String> = None;
    if let Some(text) = own {
        match config_model::parse_bool(text) {
            Some(enabled) => {
                return Resolution {
                    enabled,
                    source: Source::Own,
                    invalid: None,
                };
            }
            None => invalid = Some(text.to_string()),
        }
    }
    if let Some(text) = master {
        match config_model::parse_bool(text) {
            // 总闸只升不降：`false` 不覆盖本项默认值（否则种子里的 false 会关掉内控）
            Some(true) => {
                return Resolution {
                    enabled: true,
                    source: Source::Master,
                    invalid,
                };
            }
            Some(false) => {}
            None => {
                if invalid.is_none() {
                    invalid = Some(text.to_string());
                }
            }
        }
    }
    Resolution {
        enabled: duty.default_enabled(),
        source: Source::Default,
        invalid,
    }
}

/// 读取本项开关的**生效值**（走运行时快照，60 秒 TTL + 写操作后广播）。
///
/// # 参数
///
/// - `duty`：哪一项职责。
///
/// # 返回值
///
/// 生效的布尔值；参数缺失 / 停用 / 解析失败时回退 [`Duty::default_enabled`]。
pub async fn enabled(duty: Duty) -> bool {
    let own = SysConfigModel::read_text(duty.key()).await;
    let master = SysConfigModel::read_text(MASTER_KEY).await;
    let resolution = resolve(duty, own.as_deref(), master.as_deref());
    if let Some(raw) = resolution.invalid.as_deref() {
        tracing::warn!(
            switch = duty.key(),
            value = raw,
            fallback = duty.default_enabled(),
            "ERP 内控开关取值非法（只认 true/false/1/0），已回退代码默认值"
        );
    }
    tracing::debug!(
        switch = duty.key(),
        enabled = resolution.enabled,
        source = ?resolution.source,
        "ERP 内控开关解析"
    );
    resolution.enabled
}

/// 拦截文案（**唯一出口**）：说明「哪个开关开着」「哪张单」「哪一对职责冲突」「为什么不行」「怎么放行」。
///
/// # 参数
///
/// - `duty`：哪一项职责（决定开关名、职责对、动作词）。
/// - `subject`：单据描述（如「收货入库单 IN-20261102-000001」，为空时退化为「这张单」）。
///
/// # 返回值
///
/// 面向业务人员的完整中文提示（前端直接弹这句话）。
pub fn rejection_text(duty: Duty, subject: &str) -> String {
    let subject = subject.trim();
    let subject = if subject.is_empty() { "这张单" } else { subject };
    format!(
        "系统已开启「{}」，{subject} {}，不能自己{}。\
         请交给其他有权限的账号处理，或在系统参数中关闭该开关（{}）。",
        duty.label(),
        duty.counterpart_clause(),
        duty.action(),
        duty.key()
    )
}

/// 审批 / 付款审批入口的**补充提示**（追加在原有 403 文案后面）。
///
/// 老文案（`不能审批自己提交的采购订单…请交给其他有审批权限的账号处理`）已经在前端文案
/// 与十余条用例里被引用，**不改写只追加**：把「哪个开关开着」「怎么放行」补在后面。
///
/// # 参数
///
/// - `duty`：哪一项职责（审批传 [`Duty::Approve`]，付款审批传 [`Duty::Pay`]）。
///
/// # 返回值
///
/// 一段带括号的中文尾巴，可直接 `format!("{原文案}{}")`。
pub fn approval_hint(duty: Duty) -> String {
    format!(
        "（内控开关 {} 已开启；小团队如确需本人处理，可在系统参数中关闭该开关）",
        duty.key()
    )
}

/// 纯函数闸门：开关开着且操作人就是制单人 → 409（**不碰库**，便于单测）。
///
/// `create_by` 为空的历史单据一律放行：没有制单人可比，谈不上「自己批自己」。
///
/// # 参数
///
/// - `duty`：哪一项职责。
/// - `switch_on`：开关的生效值（调用方已解析好）。
/// - `creator`：单据制单人（`None` = 历史数据 / 系统生成）。
/// - `operator`：本次操作人。
/// - `subject`：单据描述（拼进文案）。
///
/// # 返回值
///
/// 放行 `Ok(())`；命中内控 `Err(409 + 66013)`。
pub fn check(
    duty: Duty,
    switch_on: bool,
    creator: Option<Uuid>,
    operator: Option<Uuid>,
    subject: &str,
) -> std::result::Result<(), ApiError> {
    let principals: &[Uuid] = creator.as_slice();
    check_among(duty, switch_on, principals, operator, subject)
}

/// 多主体版网关（P3-07）：一个人对多个「职责对端」的场景（审批人可能不止一个）。
///
/// 判据与 [`check`] 完全相同，只是把「制单人」换成一**组**人：操作人 **命中其中任何一个**
/// 就拦。`approve_execute` 要把「这张单所有投过同意票的人」都当作对端 —— 只比最后一个
/// 审批人会给多级审批留后门（A 批、B 批、A 执行 → 只比 B 就漏了 A）。
///
/// 空集合 = 没有对端可比（没走审批 / 历史数据）→ 放行。
///
/// # 参数
///
/// - `duty`：哪一项职责。
/// - `switch_on`：开关的生效值（调用方已解析好）。
/// - `principals`：与操作人互斥的那些人（制单人 / 审批人 / 采购员）。
/// - `operator`：本次操作人。
/// - `subject`：单据描述（拼进文案）。
///
/// # 返回值
///
/// 放行 `Ok(())`；命中内控 `Err(409 + 66013)`。
pub fn check_among(
    duty: Duty,
    switch_on: bool,
    principals: &[Uuid],
    operator: Option<Uuid>,
    subject: &str,
) -> std::result::Result<(), ApiError> {
    if !switch_on {
        return Ok(());
    }
    // 操作人未知（系统任务 / 无令牌）→ 没有「自己」可言，放行
    let Some(operator) = operator else {
        return Ok(());
    };
    if !principals.contains(&operator) {
        return Ok(());
    }
    tracing::info!(
        switch = duty.key(),
        %operator,
        subject = subject,
        principals = principals.len(),
        "ERP 内控拦截：{}（不能自己{}）",
        duty.label(),
        duty.action()
    );
    Err(ApiError::new(
        409,
        error_code::ERP_SEPARATION_OF_DUTIES,
        rejection_text(duty, subject),
    ))
}

/// 闸门（读开关 + 判定）：业务入口直接 `?` 即可。
///
/// # 参数
///
/// - `duty`：哪一项职责。
/// - `creator`：单据制单人。
/// - `operator`：本次操作人。
/// - `subject`：单据描述（拼进文案）。
///
/// # 返回值
///
/// 放行 `Ok(())`；命中内控 `Err(409 + 66013)`。
pub async fn guard(
    duty: Duty,
    creator: Option<Uuid>,
    operator: Option<Uuid>,
    subject: &str,
) -> std::result::Result<(), ApiError> {
    let switch_on = enabled(duty).await;
    check(duty, switch_on, creator, operator, subject)
}

/// 多主体版闸门（P3-07）：读开关 + 与一组对端比对。
///
/// # 参数
///
/// - `duty`：哪一项职责。
/// - `principals`：与操作人互斥的那些人（见 [`check_among`]）。
/// - `operator`：本次操作人。
/// - `subject`：单据描述（拼进文案）。
///
/// # 返回值
///
/// 放行 `Ok(())`；命中内控 `Err(409 + 66013)`。
pub async fn guard_among(
    duty: Duty,
    principals: &[Uuid],
    operator: Option<Uuid>,
    subject: &str,
) -> std::result::Result<(), ApiError> {
    let switch_on = enabled(duty).await;
    check_among(duty, switch_on, principals, operator, subject)
}

/// 取某个审批实例里**投过同意票的人**（`biz_flow_task.status = 'APPROVED'` 的 `assignee_id`）。
///
/// `approve_execute` 用它拿「审批人」这一端的 id。只认 `APPROVED`（驳回 / 转办 / 跳过的人
/// 没批过这张单，不该被当成审批人）。
///
/// # 参数
///
/// - `instance_id`：审批实例（`None` = 这张单没走审批流程）。
///
/// # 返回值
///
/// 审批人 id 列表（去重、按完成时间升序）；`None` 或查库失败 → 空列表。
///
/// # 为什么查库失败是「放行」而不是「拦下」
///
/// 这是内控**附加**闸门，不是记账主流程：一次瞬时 DB 抖动就把付款 / 过账全卡死，
/// 比漏拦一次更糟（且失败会 `warn` 出声，不是静默）。真正的钱账安全由各自的
/// 状态机 + 唯一记账实现保证。
pub async fn approvers_of(instance_id: Option<Uuid>) -> Vec<Uuid> {
    let Some(instance_id) = instance_id else {
        return Vec::new();
    };
    // SCOPE-EXEMPT: 审批待办是流程运行时表，按实例 id 精确取，行数 = 该实例的投票人数。
    let rows = match db()
        .find_with_params(
            "SELECT assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'APPROVED' \
              ORDER BY COALESCE(completed_at, created_at) ASC, created_at ASC",
            vec![Value::from(&instance_id)],
        )
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                %error,
                "查询审批人失败：本次「审批 ≠ 执行」闸放行（不因查库抖动卡住记账）"
            );
            return Vec::new();
        }
    };
    let mut approvers: Vec<Uuid> = Vec::with_capacity(rows.len());
    for row in &rows {
        // `assignee_id` 是 `NOT NULL` 列 → 这里拿到的是 `Result<Uuid, DbError>`（非 Option）
        if let Ok(id) = row.get_typed::<Uuid>("assignee_id")
            && !approvers.contains(&id)
        {
            approvers.push(id);
        }
    }
    approvers
}

// ==================== 自审金额上限（P3-05） ====================

/// 自审金额上限的配置键（P3-05）。
///
/// 语义：节点 `self_approval=ALLOW` 时，**单据金额超过该值就不许自审**
/// （降级为 SKIP 走升级链 / 兜底人；仍无人可用则 422 + `65044`）。
/// `0` / 参数缺失 = **不限额**（默认口径，ALLOW 本身已经是显式配置）。
pub const SELF_APPROVAL_MAX_AMOUNT_KEY: &str = "erp.separation_of_duties.self_approval_max_amount";

/// 自审金额上限的**代码默认值**：`0.0` = 不限额。
///
/// 为什么默认 0 而不是 1 万：ARCH-8 §8 待确认问题 2 的两条建议里选了「不限额」——
/// 阈值属于「客户自己的内控松紧」，代码替客户拍一个数字会在升级后**静默改变**
/// 已有 ALLOW 节点的行为；默认 0 时本卡的能力是纯增量（不配就与改动前逐字一致）。
pub const SELF_APPROVAL_MAX_AMOUNT_DEFAULT: f64 = 0.0;

/// 十进制字面量判定（**配置值 / 表单金额共用的唯一口径**）。
///
/// 只认 `-?digits(.digits)?`：整数或小数，前后空白去掉后判。
/// **不接受** `1e6`（科学计数法）/ `+1` / `.5` / `1.` / `10,000` / `¥100` ——
/// 这些写法要么是配置错误、要么是业务侧的显示格式，猜一个数比报错更危险。
///
/// # 参数
///
/// - `text`：已 trim 的原始文本。
///
/// # 返回值
///
/// 形如十进制字面量 → `true`。
pub fn is_decimal_literal(text: &str) -> bool {
    let body = text.strip_prefix('-').unwrap_or(text);
    if body.is_empty() {
        return false;
    }
    let mut parts = body.split('.');
    let Some(int_part) = parts.next() else {
        return false;
    };
    let frac_part = parts.next();
    if parts.next().is_some() {
        return false; // 多个小数点
    }
    if int_part.is_empty() || !int_part.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    match frac_part {
        Some(frac) => !frac.is_empty() && frac.bytes().all(|byte| byte.is_ascii_digit()),
        None => true,
    }
}

/// 纯函数：解析金额上限的原始文本。
///
/// 只认十进制数字（允许前后空白、允许负号），**不认千分位逗号 / 货币符号 / 科学计数法**
/// —— 参数里写成 `10,000` 或 `¥1万` 是配置错误，应当 `warn` 后回退默认值，
/// 而不是猜一个数（猜错的后果是「大额单据被放行自审」）。
///
/// # 参数
///
/// - `raw`：`sys_config.config_value` 原文。
///
/// # 返回值
///
/// 可解析且有限、非负时返回 `Some(数值)`；空串 / 非法 / 负数 → `None`。
pub fn parse_amount(raw: &str) -> Option<f64> {
    let text = raw.trim();
    if !is_decimal_literal(text) {
        return None;
    }
    // 负上限没有意义（等于全拦），按非法处理，由调用方回退默认值并告警
    let value: f64 = text.parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some(value)
}

/// 读取自审金额上限的**生效值**（走运行时快照，60 秒 TTL + 写操作后广播）。
///
/// # 返回值
///
/// `0.0` = 不限额（参数缺失 / 停用 / 非法都落到这里）；否则为上限金额。
///
/// # 异常情况
///
/// 参数值非法（非数字 / 负数）时打 `warn` 并回退 [`SELF_APPROVAL_MAX_AMOUNT_DEFAULT`]
/// —— **回退到 0（不限额）不是放宽内控**：ALLOW 节点本身就是显式配置过的例外，
/// 与布尔开关「解析失败当 false = 关掉内控」的方向相反，这里回退到默认值不会
/// 让任何本来被拦的动作变成放行（阈值只在 ALLOW 上起作用）。
pub async fn self_approval_max_amount() -> f64 {
    let Some(raw) = SysConfigModel::read_text(SELF_APPROVAL_MAX_AMOUNT_KEY).await else {
        return SELF_APPROVAL_MAX_AMOUNT_DEFAULT;
    };
    match parse_amount(&raw) {
        Some(value) => {
            tracing::debug!(
                config_key = SELF_APPROVAL_MAX_AMOUNT_KEY,
                max_amount = value,
                "ERP 自审金额上限解析"
            );
            value
        }
        None => {
            tracing::warn!(
                config_key = SELF_APPROVAL_MAX_AMOUNT_KEY,
                value = raw,
                fallback = SELF_APPROVAL_MAX_AMOUNT_DEFAULT,
                "ERP 自审金额上限取值非法（只认非负十进制数字），已回退默认值（0 = 不限额）"
            );
            SELF_APPROVAL_MAX_AMOUNT_DEFAULT
        }
    }
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    const ME: Uuid = Uuid::from_u128(0x5e9a_0000_0000_0000_0000_0000_0000_0001);
    const OTHER: Uuid = Uuid::from_u128(0x5e9a_0000_0000_0000_0000_0000_0000_0002);

    /// ① 开关关闭（默认，参数不存在）→ 制单人能自己过账。
    #[test]
    fn default_off_lets_creator_post() {
        let resolution = resolve(Duty::Post, None, None);
        assert!(!resolution.enabled, "post 默认必须是关闭的（不影响存量用户）");
        assert_eq!(resolution.source, Source::Default);
        assert!(
            check(Duty::Post, resolution.enabled, Some(ME), Some(ME), "收货入库单 IN-T").is_ok(),
            "默认关闭时制单人必须能过账"
        );
    }

    /// ② 开关开启 → 制单人过账 409，且文案点名开关 + 给出放行办法。
    #[test]
    fn switch_on_blocks_creator_with_readable_409() {
        let resolution = resolve(Duty::Post, Some("true"), None);
        assert!(resolution.enabled);
        assert_eq!(resolution.source, Source::Own);

        let error = check(
            Duty::Post,
            resolution.enabled,
            Some(ME),
            Some(ME),
            "收货入库单 IN-20261102-000001",
        )
        .expect_err("制单人过账必须被拦");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_SEPARATION_OF_DUTIES);
        assert!(
            error.msg.contains("系统已开启「制单与过账分离」"),
            "文案要点名开关：{}",
            error.msg
        );
        assert!(
            error.msg.contains("IN-20261102-000001"),
            "文案要点名单据：{}",
            error.msg
        );
        assert!(error.msg.contains("不能自己过账"), "文案要说清动作：{}", error.msg);
        assert!(
            error.msg.contains("关闭该开关"),
            "文案必须给出放行办法（O5）：{}",
            error.msg
        );
        assert!(
            error.msg.contains(POST_KEY),
            "文案要带开关键名，管理员才知道去哪改：{}",
            error.msg
        );
    }

    /// ③ 开关开启 → 别人过账放行。
    #[test]
    fn switch_on_lets_others_post() {
        assert!(
            check(Duty::Post, true, Some(ME), Some(OTHER), "收货入库单 IN-T").is_ok(),
            "别人过账必须放行"
        );
        // 没有制单人的历史单据同样放行（无可比对象）
        assert!(
            check(Duty::Post, true, None, Some(ME), "收货入库单 IN-T").is_ok(),
            "无制单人的历史单据不许被拦"
        );
    }

    /// ④ 采购单审批：同一套闸，动作文案是「审批」（默认就是开的）。
    #[test]
    fn approve_duty_blocks_creator_by_default() {
        let resolution = resolve(Duty::Approve, None, None);
        assert!(resolution.enabled, "approve 默认开（= 改动前的内控闸）");
        let error = check(Duty::Approve, true, Some(ME), Some(ME), "采购单 PO-T").expect_err("制单人自审必须被拦");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("制单与审批分离"), "{}", error.msg);
        assert!(error.msg.contains("不能自己审批"), "{}", error.msg);
        assert!(
            check(Duty::Approve, true, Some(ME), Some(OTHER), "采购单 PO-T").is_ok(),
            "其他审批人放行"
        );
    }

    /// ⑤ 付款：默认开（P17 闸），拦自己；别人放行。
    #[test]
    fn pay_duty_blocks_creator_by_default() {
        let resolution = resolve(Duty::Pay, None, None);
        assert!(resolution.enabled);
        let error = check(Duty::Pay, true, Some(ME), Some(ME), "付款单 PAY-T").expect_err("制单人自付必须被拦");
        assert!(error.msg.contains("制单与付款分离"), "{}", error.msg);
        assert!(error.msg.contains("付款审批"), "{}", error.msg);
        assert!(
            check(Duty::Pay, true, Some(ME), Some(OTHER), "付款单 PAY-T").is_ok(),
            "别人付款放行"
        );
        // 小团队显式关掉 → 自己也能付
        let off = resolve(Duty::Pay, Some("false"), None);
        assert!(!off.enabled);
        assert_eq!(off.source, Source::Own);
    }

    /// ⑥ 参数不存在 → 三项都用代码默认值（false / true / true）。
    #[test]
    fn missing_parameters_fall_back_to_code_defaults() {
        for duty in Duty::ALL {
            let resolution = resolve(duty, None, None);
            assert_eq!(resolution.source, Source::Default, "{}", duty.key());
            assert_eq!(resolution.enabled, duty.default_enabled(), "{}", duty.key());
            assert!(resolution.invalid.is_none(), "缺参数不是「非法值」");
        }
        assert!(!Duty::Post.default_enabled());
        assert!(Duty::Approve.default_enabled());
        assert!(Duty::Pay.default_enabled());
    }

    /// ⑦ 参数值非法（非 bool）→ 回退默认值，并上报「非法」让调用方 warn。
    #[test]
    fn invalid_value_falls_back_to_default_and_reports() {
        let resolution = resolve(Duty::Post, Some("yes"), None);
        assert!(!resolution.enabled, "非法值必须回退 post 的默认 false");
        assert_eq!(resolution.source, Source::Default);
        assert_eq!(resolution.invalid.as_deref(), Some("yes"), "要能打出原文");

        // approve 的默认是 true：写错值不能把它误关成 false
        let resolution = resolve(Duty::Approve, Some("ture"), None);
        assert!(resolution.enabled, "非法值不许把内控关掉");
        assert!(resolution.invalid.is_some());

        // 本项合法时不再抱怨总闸（本项优先，总闸根本没参与判定）
        let resolution = resolve(Duty::Post, Some("1"), Some("oops"));
        assert!(resolution.enabled);
        assert_eq!(resolution.source, Source::Own);
        assert!(resolution.invalid.is_none());
    }

    /// ⑧ 改完立即生效：解析不缓存、每次重新按当前值算；总闸只升不降。
    #[test]
    fn changes_take_effect_immediately() {
        // 同一个键，两次解析拿的是两次读到的值（调用点不做任何记忆）
        assert!(!resolve(Duty::Post, Some("false"), None).enabled);
        assert!(resolve(Duty::Post, Some("true"), None).enabled);
        assert!(!resolve(Duty::Post, Some("false"), None).enabled);

        // 总闸开 → 未单独配置的项默认开
        let via_master = resolve(Duty::Post, None, Some("true"));
        assert!(via_master.enabled);
        assert_eq!(via_master.source, Source::Master);

        // 总闸关 → 不降级（否则种子里的 false 会把审批 / 付款内控关掉）
        let master_off = resolve(Duty::Post, None, Some("false"));
        assert_eq!(master_off.source, Source::Default);
        assert!(!master_off.enabled);
        assert!(
            resolve(Duty::Approve, None, Some("false")).enabled,
            "总闸不许关掉审批内控"
        );

        // 显式关掉某一项比总闸优先
        let explicit_off = resolve(Duty::Post, Some("false"), Some("true"));
        assert!(!explicit_off.enabled, "显式配置优先于总闸");
        assert_eq!(explicit_off.source, Source::Own);
    }

    /// 文案恒等约束：三项都能拼出一句完整、可执行的中文（含空单据描述时不出现空档）。
    #[test]
    fn rejection_text_is_complete_for_every_duty() {
        for duty in Duty::ALL {
            let text = rejection_text(duty, "");
            assert!(text.contains(duty.label()), "{}", text);
            assert!(text.contains(duty.action()), "{}", text);
            assert!(text.contains("关闭该开关"), "{}", text);
            assert!(text.contains(duty.key()), "{}", text);
            assert!(!text.contains(" 这次"), "空白单据描述不该留空档：{text}");
        }
    }

    /// P3-05：自审金额上限的文本解析口径（纯逻辑，**不碰库**）。
    ///
    /// 关键三条：`0` 是合法值（= 不限额）、非法文本一律 `None`（调用方回退默认值）、
    /// 负数不吃（负上限等于全拦，属于配置错误）。
    #[test]
    fn parse_amount_only_accepts_non_negative_decimal() {
        assert_eq!(parse_amount("0"), Some(0.0));
        assert_eq!(parse_amount(" 1000 "), Some(1000.0));
        assert_eq!(parse_amount("1000.50"), Some(1000.5));
        assert_eq!(parse_amount("0.01"), Some(0.01));

        // 非法：空 / 千分位 / 货币符号 / 汉字单位 / 科学计数法 / 负数
        for bad in ["", "   ", "10,000", "¥1000", "1万", "1e6", "-1", "abc"] {
            assert_eq!(parse_amount(bad), None, "不该把 {bad:?} 当成金额");
        }

        // 「不限额」的唯一合法表示是 0 / 参数缺失；负零按 0 处理
        assert_eq!(parse_amount("-0"), Some(0.0));
        assert_eq!(SELF_APPROVAL_MAX_AMOUNT_DEFAULT, 0.0);
    }

    // ==================== P3-07：三个新开关 ====================

    /// ①a 审批 ↔ 执行：关闭（默认）→ 审批人自己执行也放行；打开 → 409。
    #[test]
    fn approve_execute_switch_blocks_approver_when_on() {
        // 默认关闭（参数不存在 = 代码默认 false）
        let off = resolve(Duty::ApproveExecute, None, None);
        assert!(!off.enabled, "approve_execute 默认必须关闭（不改存量行为）");
        assert_eq!(off.source, Source::Default);
        assert!(
            check(Duty::ApproveExecute, off.enabled, Some(ME), Some(ME), "付款单 PM-T").is_ok(),
            "开关关闭时审批人必须能自己执行"
        );

        // 打开 → 拦
        let on = resolve(Duty::ApproveExecute, Some("true"), None);
        assert!(on.enabled);
        assert_eq!(on.source, Source::Own);
        let error = check(Duty::ApproveExecute, on.enabled, Some(ME), Some(ME), "付款单 PM-T")
            .expect_err("审批人自己执行必须被拦");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_SEPARATION_OF_DUTIES);
        // 换人放行
        assert!(
            check(Duty::ApproveExecute, on.enabled, Some(ME), Some(OTHER), "付款单 PM-T").is_ok(),
            "换一个执行人必须放行"
        );
    }

    /// ①b 制单 ↔ 盘点确认：关闭 → 制单人可自己确认；打开 → 409；换人放行。
    #[test]
    fn stocktake_switch_blocks_creator_when_on() {
        let off = resolve(Duty::Stocktake, None, None);
        assert!(!off.enabled, "stocktake 默认必须关闭");
        assert!(
            check(Duty::Stocktake, off.enabled, Some(ME), Some(ME), "盘点单 PD-T").is_ok(),
            "开关关闭时制单人必须能确认自己的盘点单"
        );

        let on = resolve(Duty::Stocktake, Some("1"), None);
        assert!(on.enabled);
        let error = check(Duty::Stocktake, on.enabled, Some(ME), Some(ME), "盘点单 PD-T")
            .expect_err("制单人确认自己的盘点单必须被拦");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_SEPARATION_OF_DUTIES);
        assert!(
            check(Duty::Stocktake, on.enabled, Some(ME), Some(OTHER), "盘点单 PD-T").is_ok(),
            "换一个确认人必须放行"
        );
    }

    /// ①c 采购 ↔ 验收：关闭 → 采购员可自己验收；打开 → 409；换人放行。
    #[test]
    fn purchase_accept_switch_blocks_buyer_when_on() {
        let off = resolve(Duty::PurchaseAccept, None, None);
        assert!(!off.enabled, "purchase_accept 默认必须关闭");
        assert!(
            check(Duty::PurchaseAccept, off.enabled, Some(ME), Some(ME), "采购订单 PO-T").is_ok(),
            "开关关闭时采购员必须能自己收货"
        );

        let on = resolve(Duty::PurchaseAccept, Some("true"), None);
        assert!(on.enabled);
        let error = check(Duty::PurchaseAccept, on.enabled, Some(ME), Some(ME), "采购订单 PO-T")
            .expect_err("采购员验收自己采的货必须被拦");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_SEPARATION_OF_DUTIES);
        assert!(
            check(Duty::PurchaseAccept, on.enabled, Some(ME), Some(OTHER), "采购订单 PO-T").is_ok(),
            "换一个验收人必须放行"
        );
    }

    /// ② 总闸 `enabled = true` 且三个子键都没配 → 三项默认变成「拦」。
    ///
    /// 这是「一键开启」的全部意义：管理员只打开总闸，新扩展的三对职责立刻生效，
    /// 不必逐个去配（也避免「以为开了其实没开」）。
    #[test]
    fn master_switch_turns_on_new_duties_by_default() {
        for duty in [Duty::ApproveExecute, Duty::Stocktake, Duty::PurchaseAccept] {
            let resolution = resolve(duty, None, Some("true"));
            assert!(resolution.enabled, "总闸打开时 {} 必须默认开", duty.key());
            assert_eq!(resolution.source, Source::Master, "{}", duty.key());
            let error =
                check(duty, resolution.enabled, Some(ME), Some(ME), "单据 T").expect_err("总闸打开后必须默认拦");
            assert_eq!(error.status, 409);
            assert_eq!(error.code, error_code::ERP_SEPARATION_OF_DUTIES);
            // 显式关掉某一项仍然优先于总闸（小团队只放宽这一项）
            let explicit_off = resolve(duty, Some("false"), Some("true"));
            assert!(!explicit_off.enabled, "{} 显式关闭要压过总闸", duty.key());
            assert_eq!(explicit_off.source, Source::Own);
        }
    }

    /// ③ 报错文案必须点名「哪一对职责」——前端原样展示这句话，管理员据此知道怎么放行。
    #[test]
    fn new_duties_error_text_names_the_duty_pair() {
        let cases = [
            (Duty::ApproveExecute, "审批与执行分离", "由你本人审批", "不能自己执行"),
            (Duty::Stocktake, "制单与盘点确认分离", "由你本人制作", "不能自己确认"),
            (Duty::PurchaseAccept, "采购与验收分离", "由你本人采购", "不能自己验收"),
        ];
        for (duty, pair, clause, action) in cases {
            let error = check(duty, true, Some(ME), Some(ME), "单据 X-1").expect_err("打开后必须拦");
            assert!(error.msg.contains(pair), "文案要点名职责对「{pair}」：{}", error.msg);
            assert!(error.msg.contains(clause), "文案要说清对端角色：{}", error.msg);
            assert!(error.msg.contains(action), "文案要说清动作：{}", error.msg);
            assert!(
                error.msg.contains(duty.key()),
                "文案要带开关键名，管理员才知道去哪改：{}",
                error.msg
            );
            assert!(error.msg.contains("关闭该开关"), "{}", error.msg);
            assert!(error.msg.contains("X-1"), "{}", error.msg);
        }
    }

    /// ④ 多审批人：命中**任意一个**审批人就拦（只比最后一个是漏洞）。
    #[test]
    fn approver_list_is_checked_as_a_whole() {
        let approvers = [ME, OTHER];
        // 第一个审批人（历史上投过票）去执行 → 拦
        assert!(
            check_among(Duty::ApproveExecute, true, &approvers, Some(ME), "盘点单 PD-T").is_err(),
            "早期审批人执行也要拦"
        );
        assert!(
            check_among(Duty::ApproveExecute, true, &approvers, Some(OTHER), "盘点单 PD-T").is_err(),
            "最后一个审批人执行也要拦"
        );
        // 第三个人执行 → 放行
        let third = Uuid::from_u128(0x5e9a_0000_0000_0000_0000_0000_0000_0003);
        assert!(
            check_among(Duty::ApproveExecute, true, &approvers, Some(third), "盘点单 PD-T").is_ok(),
            "没批过这张单的人执行必须放行"
        );
        // 空审批人列表（没走审批）→ 放行
        assert!(
            check_among(Duty::ApproveExecute, true, &[], Some(ME), "盘点单 PD-T").is_ok(),
            "没走审批的单据不许被这个闸拦"
        );
        // 开关关闭 → 一律放行
        assert!(check_among(Duty::ApproveExecute, false, &approvers, Some(ME), "盘点单 PD-T").is_ok());
        // 单主体版与多主体版判据一致（check 是 check_among 的特例）
        assert_eq!(
            check(Duty::ApproveExecute, true, Some(ME), Some(ME), "s").is_err(),
            check_among(Duty::ApproveExecute, true, &[ME], Some(ME), "s").is_err()
        );
    }

    /// ⑤ 六个职责的开关矩阵完整性：键唯一、默认值口径正确、中文名互不相同。
    #[test]
    fn duty_matrix_is_coherent() {
        let mut keys: Vec<&str> = Duty::ALL.iter().map(|duty| duty.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), Duty::ALL.len(), "每个职责必须有独立开关键");

        let mut labels: Vec<&str> = Duty::ALL.iter().map(|duty| duty.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), Duty::ALL.len(), "中文名不能重复（前端分组展示）");

        for duty in Duty::ALL {
            assert!(
                duty.key().starts_with("erp.separation_of_duties."),
                "{} 必须落在同一个参数命名空间",
                duty.key()
            );
            assert_eq!(
                duty.default_enabled(),
                matches!(duty, Duty::Approve | Duty::Pay),
                "只有 approve / pay 默认开（= 改动前的内控闸）"
            );
        }
        // 三个新扩展项默认全关 —— 升级零行为变化
        for duty in [Duty::ApproveExecute, Duty::Stocktake, Duty::PurchaseAccept] {
            assert!(!duty.default_enabled(), "{} 默认必须 false", duty.key());
        }
    }
}
