//! 标准成本卡 **发布审批**（`COST_STANDARD_PUBLISH` / `cost_standard_publish_approve`）—— M5-10。
//!
//! 「审核后生效」：卡头的 `publish_state` 走 `DRAFT → PENDING → PUBLISHED`，
//! 通过才允许参与差异分析取数（取数口径 = `status = 1 AND publish_state = 'PUBLISHED'`）。
//!
//! # 四个出口（与 `cost_period_bridge` / `purchase_bridge` 同款分工）
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_publish_decided`] | `erp::doc_reader_impl::ErpDoc::CostStandardPublish::settle`（`flow::handler` 四个终态位置扇出，**审批事务已提交之后**） | `APPROVED` → `PENDING → PUBLISHED` + `status = 1`；`REJECTED` / `WITHDRAWN` / `CANCELLED` → `PENDING → DRAFT` + 清空实例指针 + 意见进 `remark` |
//! | [`guard_self_approval`] | 同一注册表的闸（`flow::handler::approve_task` **事务之前**） | 制单人 / 发起人不能批自己这一票 → `403 + 65019` |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把制单人从审批人列表里排除（落 `SKIPPED` 留痕），否则财务经理本人建卡 = 看得见点不动 = 死单 |
//! | [`published_flow`] | [`submit_publish`] | 取已发布的 `cost_standard_publish_approve`；没有 ⇒ 409 + `66033`（**默认拒绝，不静默直过**） |
//!
//! # 状态机（只在两条边上走，别的地方一律 409 + `66111`）
//!
//! ```text
//!  DRAFT ──提交发布──► PENDING ──审批通过──► PUBLISHED
//!    ▲                    │
//!    └────驳回 / 撤回─────┘
//! ```
//!
//! - `PUBLISHED` **不是终态**：`status` 仍可置 0（失效，退出取数），也可以再置 1（复用同一版）；
//!   但**口径**（标准 BOM / 工时 / 费率 / 生效日）在「已生效过」之后不许改（`66109`，M5-08 的规则）。
//! - `PENDING` 期间**不许改口径、不许删、不许手工置生效**：否则审批人批的是一版、库里生效的是
//!   另一版（审批失真比死单更坏）。要改请先「撤回」（发起人）或让审批人「驳回」。
//!
//! # 为什么回调失败不会留死单（约定 11）
//!
//! 回调跑在审批事务**提交之后**：审批结论已落库，回调失败**不许**回滚审批单，只记 `warn`。
//! 最坏情形是「批过了但卡还停在 `PENDING`」—— 这时：
//! 1. 审批中心仍能撤回（发起人）/ 终止（流程管理员），两条路都会再扇出一次终态回调；
//! 2. 卡上还有「撤回」按钮（[`withdraw_publish`] 带**自愈分支**）：检测到实例已不在 `PENDING`
//!    就按实例真实结论回写（`APPROVED` → 发布，其余 → 回草稿），点一次即可恢复；
//! 3. `mark_published` / `mark_publish_back_to_draft` 都是 CAS（`WHERE publish_state = 'PENDING'`），
//!    重复回调幂等无害。
//!
//! 所以「回调失败」在界面上永远有下一步可点，不需要工程师改库。
//!
//! # 权限码：复用 `cost-method:update`
//!
//! 提交 / 撤回是对**这一版标准成本卡**的修改动作，与「改一版」同一档（`V0460` 已按这个口径
//! 授权完毕：FINANCE_MANAGER 改、FINANCE_DIRECTOR 只看）。新开 `cost-standard:publish`
//! 在 M5-18 页落地前没有菜单行承载 ⇒ 就是死码（`permission_audit_tests::n14` 当场判红），
//! 与 M5-08 / M5-09 同一处理（见那两张卡的 evidence README）。

use axum::extract::Path;
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_labor_dto;
use crate::business::erp::cost::cost_standard_dto::{
    self as dto, PUBLISH_DRAFT, PUBLISH_PENDING, PUBLISH_PUBLISHED, StandardDetailRes,
};
use crate::business::erp::cost::cost_standard_handler;
use crate::business::erp::cost::cost_standard_model as model;
use crate::business::flow::engine;
use crate::business::flow::model as flow_model;
use cloudpivot_kernel::business::shared::separation;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

/// 发布审批的业务类型（与 `V0465` 种子 / `cloudpivot_doc::FlowBizType::CostStandardPublish` 同口径）。
pub const BIZ_TYPE: &str = "COST_STANDARD_PUBLISH";
/// 发布审批的流程编码（`V0465` 种子的 `flow_code`）。
pub const FLOW_CODE: &str = "cost_standard_publish_approve";
/// 状态日志 / 备注里「审批驳回」的前缀（审计能搜到的关键词）。
pub const REJECT_NOTE_PREFIX: &str = "发布驳回：";
/// 状态日志 / 备注里「提交人撤回」的前缀。
pub const WITHDRAW_NOTE_PREFIX: &str = "发布撤回：";
/// 提交 / 撤回用的权限码（理由见模块头「权限码」）。
pub const PERM_UPDATE: &str = "business:erp:cost-method:update";

/// 已发布的发布审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的标准成本发布审批流程」。`None` = 没配（调用方 409 并指名去发布流程）。
pub async fn published_flow() -> DbResult<Option<PublishFlow>> {
    // SCOPE-EXEMPT: 流程定义是全局配置，按 biz_type / flow_code 精确取 1 行。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'COST_STANDARD_PUBLISH' OR lower(d.flow_code) = 'cost_standard_publish_approve') \
              ORDER BY (d.biz_type = 'COST_STANDARD_PUBLISH') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PublishFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

/// 审批结论 → 业务语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 通过：`PENDING → PUBLISHED`（并置 `status = 1`）。
    Approve,
    /// 驳回 / 撤回 / 终止：`PENDING → DRAFT`（清空审批痕迹，可改后重提）。
    BackToDraft,
}

impl Verdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Verdict::Approve => "APPROVE",
            Verdict::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动卡）。
///
/// ⚠️ **必须是白名单**，不能写成 `!is_approved()`：多级流程批完第一站时实例还是 `PENDING`，
/// 用「非 APPROVED 就打回」会把刚批完第一站在途的卡打回草稿（`cost_period_bridge` 同款注释）。
pub fn verdict_of(instance_status: &str) -> Option<Verdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(Verdict::Approve),
        "REJECTED" | "REJECTED_TERMINATED" | "WITHDRAWN" | "CANCELLED" => Some(Verdict::BackToDraft),
        _ => None,
    }
}

/// 发布状态闸（纯函数）：动作在当前发布状态下允许吗。
///
/// | 动作 | 允许的发布状态 | 为什么 |
/// |---|---|---|
/// | 提交发布 | `DRAFT` | `PENDING` 已在审批中（重复提交 = 两张审批单）；`PUBLISHED` 已发布（要改口径请新建一版） |
/// | 撤回 | `PENDING` | 没有在跑的审批就无从撤起 |
/// | 改口径 / 删除 | 非 `PENDING` | 审批中改口径 = 审批人批的不是库里那版 |
/// | 手工置生效（`status = 1`） | `PUBLISHED` | 「审核后生效」不被绕过的唯一保证 |
pub fn publish_guard(action: &str, current: &str, allowed: &[&str]) -> std::result::Result<(), ApiError> {
    if allowed.contains(&current.trim()) {
        return Ok(());
    }
    Err(ApiError::new(
        409,
        dto::CODE_PUBLISH_GUARD,
        format!(
            "这一版标准成本卡当前是「{}」（publish_state = {}），不能{action}：\n\
             · 提交发布只在「草稿」状态可做（草稿 = 还没提交 / 被驳回 / 撤回后的状态）；\n\
             · 审批中（PENDING）要先「撤回」或让审批人「驳回」，才能改口径 / 删除 / 重新提交；\n\
             · 已发布（PUBLISHED）的卡要换口径请**新增一版**（历史版本是差异分析的依据，不许回头改）",
            dto::publish_state_label(current),
            current.trim()
        ),
    ))
}

/// 提交发布：`DRAFT → PENDING` + 发起审批单（**一个事务**）。
///
/// # 异常情况
///
/// - 卡不存在 / 已删 ⇒ 404
/// - 不是草稿（审批中 / 已发布）⇒ 409 + `66111`
/// - 已有在跑的审批实例 ⇒ 409（引擎侧 `uk_biz_flow_instance_biz_active` 兜底）
/// - 没有已发布的发布审批流程 ⇒ 409 + `66033`（**默认拒绝**，不静默直过）
/// - 审批人是制单人且开了职责分离 ⇒ 引擎拒绝（`StartFailure::Rejected`），整笔回滚 ⇒ 卡仍是草稿
pub async fn submit_publish(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<StandardDetailRes, ApiError> {
    let id = cost_labor_dto::parse_uuid(id_raw, "id")?;
    let Some(operator) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交标准成本发布需要登录用户（审批单要记发起人）",
        ));
    };
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待发布的标准成本卡"))?
        .ok_or_else(|| dto::not_found(id))?;
    // ① 只有草稿能提交
    publish_guard("提交发布", &row.publish_state, &[PUBLISH_DRAFT])?;
    // ② 已有在跑的审批 → 409（引擎侧唯一索引兜底）
    if flow_model::biz_instance_running(BIZ_TYPE, id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑的发布审批"))?
    {
        return Err(dto::publish_guard_error(format!(
            "标准成本卡 {} v{} 已有在跑的发布审批：请等审批结束或先撤回",
            row.material_label(),
            row.version_no
        )));
    }
    // ③ 没有已发布流程 = 默认拒绝
    let flow = published_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询标准成本发布审批流程"))?
        .ok_or_else(|| {
            ApiError::new(
                409,
                error_code::APPROVAL_FLOW_NOT_CONFIGURED,
                format!(
                    "标准成本卡 {} v{} 的发布必须走审批，但系统里没有已发布的 \
                     cost_standard_publish_approve 流程，所以不能提交。请管理员在「审批流程」里\
                     发布标准成本发布审批流程（财务经理一级或签）。",
                    row.material_label(),
                    row.version_no
                ),
            )
        })?;
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(id),
        biz_no: Some(doc_no_of(&row)),
        title: format!(
            "{} v{} 标准成本发布（{}）",
            row.material_label(),
            row.version_no,
            dto::window_text(row.effective_from, row.effective_to)
        )
        .chars()
        .take(200)
        .collect::<String>(),
        initiator_id: operator,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: submit_form_data(&row),
    };
    let out = {
        let tx = db()
            .begin()
            .await
            .map_err(|error| utils::db_error(error, "开启提交发布事务"))?;
        let outcome = async {
            // ④ CAS 回写 DRAFT → PENDING + 挂实例（库层 ck_biz_cost_std_publish_inst 要求同事务给实例）
            let affected = model::mark_publish_submitted(&tx, id, instance_id, Some(operator))
                .await
                .map_err(|error| utils::db_error(error, "标记标准成本卡为审批中"))?;
            if affected == 0 {
                return Err(dto::publish_guard_error(format!(
                    "标准成本卡 {} v{} 已不是草稿（可能刚被别人提交 / 发布），请刷新后重试",
                    row.material_label(),
                    row.version_no
                )));
            }
            // ⑤ 发起审批单：引擎的业务拒绝当硬错误 ⇒ 整笔回滚（卡仍是草稿、无残留实例）
            match engine::start_in_tx(&tx, &start).await {
                Ok(out) => Ok::<engine::ActionOut, ApiError>(out),
                Err(engine::StartFailure::Rejected(rejected)) => Err(rejected.appended(
                    "（标准成本卡保持草稿、没有生成审批单：请让能审批的人提交，\
                     或把一位非制单人加入财务经理角色后再提交）",
                )),
                Err(engine::StartFailure::Db(error)) => Err(utils::db_error(error, "发起标准成本发布审批单")),
            }
        }
        .await;
        match outcome {
            Ok(out) => {
                tx.commit()
                    .await
                    .map_err(|error| utils::db_error(error, "提交提交发布事务"))?;
                out
            }
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        }
    };
    // 事务已提交 → 才发通知 / 建任务（提交后的副作用不回滚业务）
    let mut out = out;
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    // 单站 / 免审兜底：真的当场走完了也要立刻回写 PUBLISHED（不能停在「审批过了但没落地」）
    if out.instance_status.trim().eq_ignore_ascii_case("APPROVED") {
        on_publish_decided(Some(instance_id), &out.instance_status).await;
    }
    tracing::info!(
        %id,
        version_no = row.version_no,
        %instance_id,
        instance_status = %out.instance_status,
        "标准成本卡已提交发布审批"
    );
    cost_standard_handler::detail_standard(&id.to_string()).await
}

/// 撤回（`PENDING → DRAFT`）：终止在跑的实例 + 清空审批痕迹，**同一个事务**。
///
/// # 自愈分支（实例已落终态）
///
/// 审批中心的驳回 / 撤回是**提交后回调**，回调失败时卡会停在 `PENDING`。这里检测到实例已不在
/// `PENDING`，就按实例的**真实结论**回写（`APPROVED` → 发布，其余 → 回草稿），让用户点一次
/// 「撤回」即可恢复 —— 不需要工程师改库（与 `cost_period_handler::withdraw_period` 同款）。
pub async fn withdraw_publish(
    operator: Option<Uuid>,
    id_raw: &str,
) -> std::result::Result<StandardDetailRes, ApiError> {
    let id = cost_labor_dto::parse_uuid(id_raw, "id")?;
    let Some(operator) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "撤回标准成本发布需要登录用户",
        ));
    };
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待撤回的标准成本卡"))?
        .ok_or_else(|| dto::not_found(id))?;
    publish_guard("撤回", &row.publish_state, &[PUBLISH_PENDING])?;
    let Some(instance_id) = row.publish_instance_id else {
        return Err(dto::publish_guard_error(format!(
            "标准成本卡 {} v{} 处于审批中但没有审批实例，请让管理员修数据",
            row.material_label(),
            row.version_no
        )));
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启撤回事务"))?;
    let outcome = async {
        let Some(instance) = flow_model::lock_instance(&tx, instance_id)
            .await
            .map_err(|error| utils::db_error(error, "锁读发布审批单"))?
        else {
            return Err(dto::publish_guard_error(format!(
                "标准成本卡 {} v{} 挂的审批实例已不存在（{instance_id}），请让管理员修数据",
                row.material_label(),
                row.version_no
            )));
        };
        if instance.status.trim().eq_ignore_ascii_case("PENDING") {
            let out = engine::withdraw(&tx, instance_id, operator)
                .await
                .map_err(|error| utils::db_error(error, "撤回标准成本发布审批"))?;
            if let Some(rejected) = out.rejected {
                // 原样透传（重建 `ApiError` 会丢结构化载荷）
                return Err(rejected);
            }
        } else {
            tracing::warn!(
                %id,
                version_no = row.version_no,
                %instance_id,
                instance_status = %instance.status,
                "发布审批实例已落终态但卡仍在审批中（回调曾失败）：按实例结论自愈"
            );
        }
        // 终止 / 终态判定之后，按实例结论把卡推到终态（同一事务，与上面同生共死）
        apply_decided_in_tx(&tx, instance_id).await.map_err(|error| {
            ApiError::new(
                500,
                error.code,
                format!("撤回成功但标准成本卡回写失败（已回滚，卡仍是原状态）：{}", error.msg),
            )
        })?;
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => tx
            .commit()
            .await
            .map_err(|error| utils::db_error(error, "提交撤回事务"))?,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    }
    tracing::info!(%id, version_no = row.version_no, "标准成本卡的发布审批已撤回（按实例结论回写）");
    cost_standard_handler::detail_standard(&id.to_string()).await
}

/// 审批单落终态 → 推进标准成本卡的发布状态（`DocReader::on_settled` 身份）。
pub async fn on_publish_decided(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = verdict_of(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(standard_id)) => {
            tracing::info!(
                %instance_id,
                %standard_id,
                verdict = verdict.as_str(),
                instance_status,
                "标准成本发布审批结论已回调卡头"
            );
            Some(standard_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                code = error.code,
                msg = %error.msg,
                "标准成本发布审批结论回调失败（不影响审批单；卡仍在审批中，可再撤回 / 驳回）"
            );
            None
        }
    }
}

/// 审批结论的实现体：自己开事务（回调路径用）。
async fn settle(instance_id: Uuid, verdict: Verdict) -> std::result::Result<Option<Uuid>, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| internal(error, "开启审批回调事务"))?;
    match settle_in(&tx, instance_id, Some(verdict)).await {
        Ok(value) => {
            tx.commit().await.map_err(|error| internal(error, "提交审批回调事务"))?;
            Ok(value)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 在**调用方事务内**按实例当前结论回写卡头（撤回路径用：终止实例 + 回草稿同事务）。
pub async fn apply_decided_in_tx(tx: &Transaction, instance_id: Uuid) -> std::result::Result<Option<Uuid>, ApiError> {
    settle_in(tx, instance_id, None).await
}

/// [`settle`] / [`apply_decided_in_tx`] 的共同实现（事务由调用方给）。
async fn settle_in(
    tx: &Transaction,
    instance_id: Uuid,
    forced: Option<Verdict>,
) -> std::result::Result<Option<Uuid>, ApiError> {
    // ① 审批单必须是「标准成本发布」类的（其它业务的待办零影响）
    let Some(instance) = flow_model::lock_instance(tx, instance_id)
        .await
        .map_err(|error| internal(error, "锁读审批单"))?
    else {
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(standard_id) = instance.biz_id else {
        return Ok(None);
    };
    let Some(row) = model::find_by_id_in(tx, standard_id)
        .await
        .map_err(|error| internal(error, "查询标准成本卡"))?
    else {
        tracing::warn!(%standard_id, "标准成本卡不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    let verdict = match forced {
        Some(verdict) => verdict,
        None => match verdict_of(&instance.status) {
            Some(verdict) => verdict,
            // 实例还在跑：卡一个字节都不动
            None => return Ok(None),
        },
    };
    let to_state = match verdict {
        Verdict::Approve => PUBLISH_PUBLISHED,
        Verdict::BackToDraft => PUBLISH_DRAFT,
    };
    // ② 卡还在不在「等这个审批」的状态上（幂等重放 / 已被别的路径推走 → 不动）
    //
    // 为什么不用 `flow::settle_guard`：那张表的判定要问
    // `state_machine::effective_edges`，而标准成本卡**没有**登记进
    // `state_machine::Entity`（它的「状态历史」就是版本行本身 + `publish_state`，
    // 不写 `biz_status_log`）。硬塞一个别的实体去问边表，必然拿到 `Moved`
    // ⇒ 回调静默不落地（正是 settle_guard 要治的那种事故）。这里用同一套三条出口的
    // 语义本地实现（已到目标 = 幂等跳过；不在等审批 = WARN 留痕；在等 = 落状态）。
    let current = row.publish_state.trim();
    if current.eq_ignore_ascii_case(to_state) {
        tracing::debug!(%standard_id, publish_state = current, "发布回调重复到达，已幂等跳过");
        return Ok(None);
    }
    if !current.eq_ignore_ascii_case(PUBLISH_PENDING) {
        tracing::warn!(
            %instance_id,
            %standard_id,
            publish_state = current,
            to_state,
            instance_status = %instance.status,
            "标准成本卡不在「等这个审批」的状态上，审批结论不落卡（单据已被别的路径推走）"
        );
        return Ok(None);
    }
    let approver = latest_completed_assignee(tx, instance_id)
        .await
        .map_err(|error| internal(error, "查询审批人"))?;
    let comment = latest_reject_comment(tx, instance_id)
        .await
        .map_err(|error| internal(error, "查询审批意见"))?;
    let note = match verdict {
        Verdict::Approve => None,
        Verdict::BackToDraft => {
            let (prefix, text) = match instance.status.as_str() {
                "WITHDRAWN" => (WITHDRAW_NOTE_PREFIX, "提交人撤回，审批终止".to_string()),
                _ => (
                    REJECT_NOTE_PREFIX,
                    comment
                        .as_deref()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .unwrap_or("未填写意见")
                        .to_string(),
                ),
            };
            Some(format!("{prefix}{text}"))
        }
    };
    // ③ CAS 回写（`WHERE publish_state = 'PENDING'`，重复回调幂等）
    let affected = match verdict {
        Verdict::Approve => model::mark_published(tx, row.id, approver).await,
        Verdict::BackToDraft => model::mark_publish_back_to_draft(tx, row.id, note.as_deref(), approver).await,
    }
    .map_err(|error| internal(error, "回写标准成本卡发布状态"))?;
    if affected == 0 {
        return Err(dto::publish_guard_error(format!(
            "标准成本卡 {} v{} 的发布状态在审批回调期间被改动，请刷新后重试",
            row.material_label(),
            row.version_no
        )));
    }
    Ok(Some(row.id))
}

/// 卡头的「单据号」（审批实例的 `biz_no`，也是报文里的人读标识）：`物料编码 v版本号`。
///
/// ⚠️ `biz_flow_instance.biz_no` 是 **varchar(32)**：物料编码 + 版本号拼起来可能超长
/// （实测 `M510-APPROVE-6199041004 v1` = 24 字符没问题，但自定义长编码会撞 22001）。
/// 所以这里**按字符数截断物料编码**，版本号永远保留（它是这张卡的身份里最要紧的部分）；
/// 没有物料编码时回落 id 前 8 位。
pub fn doc_no_of(row: &model::StandardRow) -> String {
    const MAX_CHARS: usize = 32;
    let tail = format!(" v{}", row.version_no);
    let head = row
        .material_code
        .clone()
        .map(|code| code.trim().to_string())
        .filter(|code| !code.is_empty())
        .unwrap_or_else(|| row.id.to_string().chars().take(8).collect());
    let room = MAX_CHARS.saturating_sub(tail.chars().count());
    let head: String = head.chars().take(room).collect();
    format!("{head}{tail}")
}

/// 提交时冻结的表单数据（审批详情 / 条件表达式的作用域）。
fn submit_form_data(row: &model::StandardRow) -> serde_json::Value {
    serde_json::json!({
        "standard_id": row.id.to_string(),
        "material_id": row.material_id.to_string(),
        "material_code": row.material_code.clone(),
        "material_name": row.material_name.clone(),
        "version_no": row.version_no,
        "effective_from": dto::date_text(row.effective_from),
        "effective_to": row.effective_to.map(dto::date_text),
        "std_material_cost": dto::amount_text(row.std_material_cost),
        "std_labor_cost": dto::amount_text(row.std_labor_cost),
        "std_overhead_cost": dto::amount_text(row.std_overhead_cost),
        "std_total_cost": dto::amount_text(row.std_total_cost),
        "std_labor_hours": dto::hours_text(row.std_labor_hours),
        "std_labor_rate": dto::amount_text(row.std_labor_rate),
        "items_count": row.items_count,
        "submit_at": Utc::now().to_rfc3339(),
    })
}

/// 最近一个已完成的审批人（写进 `published_by`）。
async fn latest_completed_assignee(ex: &impl SqlExecutor, instance_id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = ex
        .query_sql(
            "SELECT assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status IN ('APPROVED','REJECTED') \
                AND assignee_id IS NOT NULL \
              ORDER BY completed_at DESC NULLS LAST LIMIT 1"
                .to_string(),
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("assignee_id").ok()),
    }
}

/// 最近一条驳回意见（拼进备注，审计据此知道为什么被打回）。
async fn latest_reject_comment(ex: &impl SqlExecutor, instance_id: Uuid) -> DbResult<Option<String>> {
    let rows = ex
        .query_sql(
            "SELECT comment FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'REJECTED' AND comment IS NOT NULL \
              ORDER BY completed_at DESC NULLS LAST LIMIT 1"
                .to_string(),
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("comment").ok()),
    }
}

// ==================== 内控：制单人不能批自己建的卡 ====================

/// 内控用：这一版标准成本卡的**制单人**（`biz_cost_standard.create_by`）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(standard_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_cost_standard WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&standard_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("create_by").ok()),
    }
}

/// 审批入口的内控闸（**事务之前**调）：制单人 / 发起人不能批自己那一票 → 403 + `65019`。
///
/// 与 `cost_period_bridge::guard_self_approval` 同口径：流程节点写 `self_approval = ALLOW`
/// （待办可见、留痕），真正的否决在这里；与本业务无关的待办直接放行（零影响）。
/// 「制单与审批分离」开关关掉时（小团队）一律放行 —— 与 `internal_control` 读同一个键，
/// 不会出现「待办被 SKIP 了但闸说可以批」。
pub async fn guard_self_approval(task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError> {
    let Some(task) = flow_model::find_task(task_id)
        .await
        .map_err(|error| internal(error, "查询待办"))?
    else {
        return Ok(());
    };
    let Some(instance) = flow_model::find_instance(task.instance_id)
        .await
        .map_err(|error| internal(error, "查询审批单"))?
    else {
        return Ok(());
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(());
    }
    if !separation::enabled(separation::Duty::Approve).await {
        return Ok(());
    }
    let Some(standard_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = model::find_by_id(standard_id).await else {
        return Ok(());
    };
    // 制单人取 `create_by`（与内控排除同一个来源，别另查一列）
    let Ok(creator) = internal_control_creator(Some(standard_id)).await else {
        return Ok(());
    };
    if creator == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己制单的标准成本卡：{} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                doc_no_of(&row),
                separation::approval_hint(separation::Duty::Approve)
            ),
        ));
    }
    Ok(())
}

/// `DbError` → 500（回调里的内部故障一律这个出口）。
fn internal(error: DbError, what: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, format!("{what}失败：{error}"))
}

// ==================== HTTP 入口 ====================

/// 提交发布审批（`DRAFT → PENDING`）。
#[nik::permission(
    path = "/business/erp/cost-standard/{id}/publish",
    method = "POST",
    permission = "business:erp:cost-method:update"
)]
#[nik::post("/business/erp/cost-standard/{id}/publish")]
pub async fn cost_standard_publish_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<StandardDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(submit_publish(operator, &id).await?))
}

/// 撤回发布审批（`PENDING → DRAFT`，带「实例已终态」自愈分支）。
#[nik::permission(
    path = "/business/erp/cost-standard/{id}/publish-withdraw",
    method = "POST",
    permission = "business:erp:cost-method:update"
)]
#[nik::post("/business/erp/cost-standard/{id}/publish-withdraw")]
pub async fn cost_standard_publish_withdraw_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<StandardDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(withdraw_publish(operator, &id).await?))
}

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// `V0465` 的种子（**已应用迁移**，只读不改）：flow_code / biz_type 的唯一来源。
    const FLOW_SEED: &str = include_str!("../../../../migrations/business/V0465__cost_standard_publish.up.sql");
    /// 审批中心的三处回调点 + 内控闸都在这里注册（接线漏一处 = 单据永久停在中间态）。
    const FLOW_HANDLER: &str = include_str!("../../flow/handler.rs");
    /// 内控排除表（制单人不能批自己的单）。
    const INTERNAL_CONTROL: &str = include_str!("../../flow/internal_control.rs");

    #[test]
    fn flow_constants_match_v0465_seed() {
        assert_eq!(BIZ_TYPE, "COST_STANDARD_PUBLISH");
        assert_eq!(FLOW_CODE, "cost_standard_publish_approve");
        assert!(
            FLOW_SEED.contains("'cost_standard_publish_approve'"),
            "V0465 里必须有 cost_standard_publish_approve 这条流程定义"
        );
        assert!(
            FLOW_SEED.contains("'COST_STANDARD_PUBLISH'"),
            "V0465 的 biz_type 必须是 COST_STANDARD_PUBLISH（审批回调靠它认领实例）"
        );
        // 审批角色（自审闸 / 内控靠它展开待办）
        assert!(
            FLOW_SEED.contains("20000000-0000-4000-8000-0000000f0005"),
            "V0465 缺审批角色 FINANCE_MANAGER"
        );
    }

    #[test]
    fn verdict_mapping_is_whitelist_only() {
        assert_eq!(verdict_of("APPROVED"), Some(Verdict::Approve));
        assert_eq!(verdict_of(" approved "), Some(Verdict::Approve));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED", "REJECTED_TERMINATED"] {
            assert_eq!(verdict_of(status), Some(Verdict::BackToDraft), "{status} 必须打回草稿");
        }
        // ⚠️ 中间态一律 None：多级流程批完第一站时实例还是 PENDING
        for status in ["PENDING", "RUNNING", "SKIPPED", ""] {
            assert_eq!(verdict_of(status), None, "{status} 不该动卡");
        }
    }

    #[test]
    fn publish_guard_only_accepts_the_declared_state() {
        assert!(publish_guard("提交发布", PUBLISH_DRAFT, &[PUBLISH_DRAFT]).is_ok());
        let err = publish_guard("提交发布", PUBLISH_PENDING, &[PUBLISH_DRAFT]).expect_err("审批中不能提交");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, dto::CODE_PUBLISH_GUARD);
        assert!(err.msg.contains("审批中"), "{}", err.msg);
        assert!(err.msg.contains("撤回"), "报文必须给出路：{}", err.msg);
        // 已发布也不能「提交」—— 要换口径请新增一版
        let err = publish_guard("提交发布", PUBLISH_PUBLISHED, &[PUBLISH_DRAFT]).expect_err("已发布不能提交");
        assert!(err.msg.contains("新增一版"), "{}", err.msg);
        // 撤回只认审批中
        assert!(publish_guard("撤回", PUBLISH_PENDING, &[PUBLISH_PENDING]).is_ok());
        assert!(publish_guard("撤回", PUBLISH_DRAFT, &[PUBLISH_PENDING]).is_err());
    }

    #[test]
    fn verdict_labels_are_stable() {
        assert_eq!(Verdict::Approve.as_str(), "APPROVE");
        assert_eq!(Verdict::BackToDraft.as_str(), "BACK_TO_DRAFT");
    }

    #[test]
    fn note_prefixes_are_auditable() {
        assert!(REJECT_NOTE_PREFIX.contains("驳回"));
        assert!(WITHDRAW_NOTE_PREFIX.contains("撤回"));
    }

    #[test]
    fn wired_into_flow_handler_and_internal_control() {
        // ① 终态出口：四个终态位置（同意 / 驳回 / 撤回 / 终止）各调一次扇出
        let calls = FLOW_HANDLER.matches("doc_reader::dispatch_settled(").count();
        assert!(
            calls >= 4,
            "flow/handler.rs 里终态扇出只出现 {calls} 次（同意 / 驳回 / 撤回 / 终止各一处，至少 4）"
        );
        // ② 本桥的实现确实在注册表里（否则上面那次扇出到不了标准成本发布）
        assert!(
            crate::business::erp::doc_reader_impl::ErpDoc::CostStandardPublish
                .biz_type()
                .as_str()
                .eq(BIZ_TYPE),
            "ErpDoc::CostStandardPublish 必须对应 COST_STANDARD_PUBLISH"
        );
        // ③ 内控闸：审批入口必须调注册表的分派
        assert!(
            FLOW_HANDLER.contains("doc_reader::guard_self_approval_for_task("),
            "flow/handler.rs 必须调 guard_self_approval_for_task"
        );
        assert!(
            INTERNAL_CONTROL.contains("\"COST_STANDARD_PUBLISH\""),
            "COST_STANDARD_PUBLISH 必须在受控码表里：漏了就等于把标准成本发布的内控关掉"
        );
    }

    #[test]
    fn draft_is_the_only_submittable_state() {
        // 三值域本身是「审核后生效」的全部语义：DRAFT 才能提交，PENDING 只能等/撤，
        // PUBLISHED 只能失效或新增一版 —— 任何第四种值都不该出现（库层有 CHECK 兜）
        assert_eq!(dto::PUBLISH_STATES.len(), 3);
        assert!(dto::is_publish_state(PUBLISH_DRAFT));
        assert!(dto::is_publish_state(" pending "), "带空白也要认（前端可能 trim 漏了）");
        assert!(!dto::is_publish_state("SUBMITTED"), "本模块没有 SUBMITTED 这个值");
        for (state, label) in [
            (PUBLISH_DRAFT, "草稿"),
            (PUBLISH_PENDING, "审批中"),
            (PUBLISH_PUBLISHED, "已发布"),
        ] {
            assert_eq!(dto::publish_state_label(state), label);
        }
    }
}
