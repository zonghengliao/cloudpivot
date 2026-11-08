//! 审批单 **PDF 导出**（PDF-05：审批轨迹时间轴 + 每级意见 + 各级签字位）。
//!
//! # 接口（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/flow/instance/{id}/pdf` | `business:flow:instance:read` | 审批单 PDF（含轨迹） |
//!
//! # 卡面要求的字段落点
//!
//! | 要求 | 数据来源 |
//! |------|----------|
//! | 单据信息：类型 / 单号 / 金额 / 申请人 / 申请时间 | `biz_flow_instance.{biz_type, instance_no, form_data, initiator_name, started_at}` |
//! | 金额 | `form_data.amount` → `form_data.total_amount`（与 [`self_approval::amount_of`] **同一口径**），取不到印 `—` |
//! | 审批轨迹（时间轴） | `biz_flow_task` 全量行（[`flow::model::timeline`]）：级次 / 节点 / 审批人 / 结果 / 时间 / 意见 |
//! | 节点名 | `biz_flow_instance_node.{node_key → node_name}`（快照；`node_key` 查不到才退回 key） |
//! | 各级签字位（空白） | 按「级」聚合审批人，签名 / 日期两栏留空（模板不预印任何值） |
//!
//! # ARCH-8 降级标记（卡面「特别要求」）
//!
//! 三种痕迹**全部来自 `biz_flow_task` 的留痕列**，不靠猜：
//!
//! | 场景 | 留痕列 | PDF 上打的字 |
//! |------|--------|--------------|
//! | 自审跳过 | `status='SKIPPED' AND is_self_approval` | `已跳过（自审）` |
//! | 内控排除（制单人 / 发起人本人） | `status='SKIPPED' AND NOT is_self_approval` | `已跳过（内控）` |
//! | 升级（P3-02） | `escalate_reason`（+ `escalated_from_role` / 目标角色名） | `已升级至 XX（原角色无人）` |
//! | 兜底（P3-03） | `assigned_by_rule='FALLBACK'` | `兜底审批人` |
//! | 委托（P3-04） | `assigned_by_rule='DELEGATED'` | `委托代批` |
//!
//! 标记是**独立的一行**（模板 `tr.tag-row`），不拼进「意见」列 —— 意见是审批人写的
//! 原文，系统字样混进去会让审计分不清「谁说的」。
//!
//! # 可见性（与 `GET /business/flow/instance/{id}` 逐条同口径）
//!
//! 发起人 / 审批人（含已审、抄送）/ `business:flow:admin`。**PDF 不比详情页松**：
//! 打印件更容易外流，随便放宽等于把审批单变成公开文档。
//!
//! # 审计
//!
//! 导出是 GET，中间件不记；渲染成功后显式补 `EXPORT_FLOW_INSTANCE_PDF` 一行
//! （写日志失败只记 error，不改响应）。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::Value as Json;

use crate::business::erp::doc_pdf_model;
use crate::business::erp::doc_pdf_render::{self, RenderedDoc};
use crate::business::flow::model;
use crate::business::pdf::handler as pdf_handler;
use crate::business::pdf::template::{
    self, ApprovalContext, ApprovalContextArgs, ApprovalSignSlot, ApprovalTrace, OrderMetaField,
    format_money, yuan_to_fen,
};
use crate::shared::error_code;
use crate::utils;

/// 审计里的操作码。
pub const EXPORT_OPERATION: &str = "EXPORT_FLOW_INSTANCE_PDF";

/// 路由权限码（与审批单详情同一码：PDF 是同一份数据的另一种呈现）。
pub const PERMISSION: &str = "business:flow:instance:read";

/// `operation_logs.target_type`（审批实例，与自审留痕同表名）。
pub const TARGET_TYPE: &str = "biz_flow_instance";

// ==================== 纯口径：状态 / 标记 ====================

/// 审批任务状态 → PDF 上的「结果」文案。
///
/// # 与前端的关系
///
/// 前端 `status-map.ts` 的 `flow-task` 域是「已同意 / 已驳回 / 已跳过」；
/// 打印件用更短的「通过 / 驳回 / 跳过」（表格列宽 40pt，多一个字就折行）。
/// 语义一一对应，改码值时两处都要看。
pub fn result_label(status: &str) -> &'static str {
    match status {
        "APPROVED" => "通过",
        "REJECTED" => "驳回",
        "SKIPPED" => "跳过",
        "TRANSFERRED" => "已转办",
        "CANCELLED" => "已作废",
        _ => "待审批",
    }
}

/// 结果配色类（模板 CSS）。
pub fn result_cls(status: &str) -> &'static str {
    match status {
        "APPROVED" => "is-approve",
        "REJECTED" => "is-reject",
        "SKIPPED" => "is-skip",
        _ => "is-pending",
    }
}

/// 审批单状态 → 结论文案。
pub fn instance_status_label(status: &str) -> &'static str {
    match status {
        "APPROVED" => "已通过",
        "REJECTED" => "已驳回",
        "CANCELLED" => "已取消",
        "WITHDRAWN" => "已撤回",
        _ => "审批中",
    }
}

/// 结论配色类。
pub fn instance_status_kind(status: &str) -> &'static str {
    match status {
        "APPROVED" => "is-approve",
        "REJECTED" => "is-reject",
        "PENDING" => "is-pending",
        _ => "is-void",
    }
}

/// 升级原因码 → 人读文案（码值由 `assignee::ESCALATE_REASON_*` 给定，**改码即破坏契约**）。
///
/// 前端 `flow-progress-tasks.ts::escalateReasonLabel` 是同一份语义
/// （「因自审跳过」/「因角色无成员」）；打印件用「原角色…」句式与卡面文案对齐。
pub fn escalate_reason_label(reason: &str) -> &'static str {
    match reason {
        "SKIP_SELF_APPROVAL" => "原角色自审剔光",
        "NO_MEMBER" => "原角色无人",
        _ => "原角色无可用审批人",
    }
}

/// 一条待办上的 **ARCH-8 降级标记**：`(文案, 配色类)`；`None` = 这条是正常派单。
///
/// # 判定顺序（有语义，别调）
///
/// 1. `FALLBACK` / `DELEGATED` 是**派单规则**（P3-03 / P3-04），它们与「升级」互斥：
///    升级成功才不会再走兜底；委托是逐人的改派。先判它们，避免把「兜底人」误标成「升级」。
/// 2. `escalate_reason` 非空 = 这条待办是沿升级链派下来的（P3-02 留痕）。
/// 3. `SKIPPED` = 被剔掉的人（自审 / 内控），**不是**派上去的人 —— 它的 `escalate_reason`
///    按设计恒为 `None`（`engine.rs` 注释钉死），所以放最后也不会互相遮住。
pub fn downgrade_tag(task: &model::TaskRow) -> Option<(String, &'static str)> {
    match task.assigned_by_rule.as_deref() {
        Some("FALLBACK") => return Some(("兜底审批人".to_string(), "is-fallback")),
        Some("DELEGATED") => return Some(("委托代批".to_string(), "is-delegate")),
        _ => {}
    }
    if let Some(reason) = task.escalate_reason.as_deref() {
        let target = task
            .escalated_to_role
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "上级角色".to_string());
        let label = escalate_reason_label(reason);
        let text = match task.escalated_from_role.as_deref() {
            Some(origin) if !origin.trim().is_empty() => {
                format!("已升级至 {target}（{label}：{origin}）")
            }
            _ => format!("已升级至 {target}（{label}）"),
        };
        return Some((text, "is-escalate"));
    }
    if task.status == "SKIPPED" {
        return Some(match task.is_self_approval {
            true => ("已跳过（自审）".to_string(), "is-skip"),
            false => ("已跳过（内控）".to_string(), "is-skip"),
        });
    }
    None
}

/// 表单快照里的「单据金额」：`form_data.amount` → `form_data.total_amount` → `None`。
///
/// 口径与 [`self_approval::amount_of`] 一致（同一份表单、同一个键序）；
/// 那边返回 `f64` 做「是否超上限」的比较，这里要**精确到分**做打印与审计，
/// 所以自己走 `Decimal`，避免 `f64` 在 `.xx5` 上四舍五入到别的分。
///
/// # 为什么 JSON number 先 `to_string()` 再解析
///
/// `serde_json::Number` 底层是 f64，`1.005` 存进去就是 `1.00499999999999989…`；
/// 直接 `from_f64` 会让「1.005」打成 **1.00**（少一分）。`Number::to_string()` 走的是
/// 最短可回环表示，`1.005` 出来的就是 `"1.005"`，再按十进制字面量解析得到精确值。
pub fn form_amount_opt(form: &Json) -> Option<Decimal> {
    for key in ["amount", "total_amount"] {
        if let Some(value) = form.get(key).and_then(decimal_of) {
            return Some(value);
        }
    }
    None
}

/// 一个 JSON 值当金额解析（number 取其十进制字面量；字符串只认十进制字面量）。
fn decimal_of(value: &Json) -> Option<Decimal> {
    match value {
        Json::Number(number) => number.to_string().parse::<Decimal>().ok(),
        Json::String(text) => {
            let trimmed = text.trim();
            if crate::business::erp::separation::is_decimal_literal(trimmed) {
                trimmed.parse::<Decimal>().ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 金额取不到时按 0 处理（审计口径：不能因为表单没填金额就让导出失败）。
pub fn form_amount(form: &Json) -> Decimal {
    form_amount_opt(form).unwrap_or(Decimal::ZERO)
}

/// 金额展示串（千分位两位小数）。
///
/// **取不到就印 `—`**，不印 `0.00`：一张没填金额的单据说「金额 0.00」是假信息，
/// 而 `—` 一眼就知道「这单没有金额字段 / 填的不是数字」。
pub fn amount_text(form: &Json) -> String {
    match form_amount_opt(form) {
        Some(value) => format_money(yuan_to_fen(value)),
        None => "—".to_string(),
    }
}

// ==================== 取数 → 上下文 ====================

/// 实例上「与 PDF 有关」的那一小撮字段（纯函数的入参，不拖 `InstanceRow` 的 30 个字段进单测）。
#[derive(Debug, Clone)]
pub struct InstanceMeta {
    /// 单号
    pub instance_no: String,
    /// 业务类型（`PURCHASE_ORDER` …）
    pub biz_type: String,
    /// 业务单号（可空）
    pub biz_no: Option<String>,
    /// 流程名称
    pub flow_name: String,
    /// 绑定版本号
    pub version_no: i32,
    /// 标题
    pub title: String,
    /// 状态
    pub status: String,
    /// 发起人姓名
    pub initiator_name: String,
    /// 表单快照
    pub form_data: Json,
    /// 结束说明
    pub result_comment: Option<String>,
    /// 最大退回轮次
    pub round_total: i32,
    /// 发起时间
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// 结束时间
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl InstanceMeta {
    /// 由实例行取展示字段（姓名快照优先，空串才用「—」）。
    pub fn of(row: &model::InstanceRow) -> Self {
        Self {
            instance_no: row.instance_no.clone(),
            biz_type: row.biz_type.clone(),
            biz_no: row.biz_no.clone().filter(|no| !no.trim().is_empty()),
            flow_name: row.flow_name.clone(),
            version_no: row.version_no,
            title: row.title.clone(),
            status: row.status.clone(),
            initiator_name: row
                .initiator_name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "—".to_string()),
            form_data: row.form_data.clone(),
            result_comment: row
                .result_comment
                .clone()
                .filter(|text| !text.trim().is_empty()),
            round_total: row.round_total,
            started_at: row.started_at,
            ended_at: row.ended_at,
        }
    }
}

/// 级次键：`(node_key, round)`。
///
/// # 为什么不用「节点定义顺序」
///
/// 退回过一次的单据，同一节点有 `round=1` / `round=2` 两批待办 —— 它们是**两个级次**
/// （打印件上要能看出「这一级走过两遍」）。用「首次出现顺序」编号，
/// 既覆盖退回轮次，又不会被并行网关的节点乱序影响。
pub type LevelKey = (String, i32);

/// 给每个 `(node_key, round)` 编级次（从 1 开始，按首次出现顺序）。
pub fn level_index_of(tasks: &[model::TaskRow]) -> HashMap<LevelKey, usize> {
    let mut index = HashMap::new();
    for task in tasks {
        let key = (task.node_key.clone(), task.round);
        let next = index.len() + 1;
        index.entry(key).or_insert(next);
    }
    index
}

/// 时间戳 → 北京时间展示串（`yyyy-MM-dd HH:mm`）。
fn at_text(value: Option<chrono::DateTime<chrono::Utc>>) -> String {
    match value {
        Some(ts) => doc_pdf_model::beijing_time(&ts),
        None => "—".to_string(),
    }
}

/// 一条待办是否「需要这个人签字」。
///
/// `SKIPPED`（自审 / 内控剔掉的人）、`TRANSFERRED`（已转办出去的人）、`CANCELLED`
/// （或签被别人抢先后作废的票）**都没有审过这张单**，在签字位里给他们留栏
/// 会让打印件看起来「他也签过」。签字位只收真要签字的人。
pub fn signable(status: &str) -> bool {
    matches!(status, "APPROVED" | "REJECTED" | "PENDING")
}

/// 由「实例 + 节点 + 待办」组装渲染上下文（**纯函数**，不碰数据库）。
///
/// # 参数
///
/// - `meta`：实例展示字段；
/// - `nodes`：实例节点行（只用来把 `node_key` 翻成中文节点名）；
/// - `tasks`：`model::timeline` 的结果（已按 `round, created_at, id` 排序）。
///
/// # 口径
///
/// - **一条待办 = 一行轨迹**：会签 3 人就是 3 行（打印件要能逐人签字）；
/// - 级次取 `(node_key, round)` 的首次出现序号，`round > 1` 另打「第 N 轮」；
/// - 签字位按「级」聚合，且**只收真的要签字的人**（见 [`signable`]）；某一级一个人都不剩时
///   仍保留该行、姓名打「—（本节点无人签字）」——「这一级走过了但没人签」正是审计要看的信息；
/// - 没有待办时 `traces` / `sign_slots` 为空，模板打「暂无审批记录」。
pub fn build_context(
    meta: &InstanceMeta,
    nodes: &[model::NodeRow],
    tasks: &[model::TaskRow],
) -> ApprovalContext {
    let node_names: HashMap<&str, &str> = nodes
        .iter()
        .map(|node| (node.node_key.as_str(), node.node_name.as_str()))
        .collect();
    let levels = level_index_of(tasks);

    let mut traces = Vec::with_capacity(tasks.len());
    // 签字位：按级次顺序聚合审批人（保持首次出现顺序 → 用 index 数组而不是 BTreeMap）
    let mut slots: Vec<(usize, String, String, Vec<String>)> = Vec::new();

    for task in tasks {
        let key = (task.node_key.clone(), task.round);
        let index = levels.get(&key).copied().unwrap_or(1);
        let level = format!("第 {index} 级");
        let node_name = node_names
            .get(task.node_key.as_str())
            .map(|name| (*name).to_string())
            .unwrap_or_else(|| task.node_key.clone());
        let approver = task
            .assignee_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "（未记录）".to_string());
        let (tag, tag_kind) = downgrade_tag(task)
            .map(|(text, kind)| (text, kind.to_string()))
            .unwrap_or_default();

        traces.push(ApprovalTrace {
            level: level.clone(),
            round_note: match task.round {
                0 | 1 => String::new(),
                round => format!("第 {round} 轮"),
            },
            node_name: node_name.clone(),
            approver: approver.clone(),
            result: result_label(&task.status).to_string(),
            cls: result_cls(&task.status).to_string(),
            at: at_text(task.completed_at.or(Some(task.created_at))),
            opinion: task.comment.clone().unwrap_or_default(),
            // 系统代点（超时自动通过 / 空审批人跳过）要能看出来，否则打印件上
            // 「谁批的」会被误读成真人点的
            auto_note: if task.is_auto && task.status != "SKIPPED" {
                "系统代点".to_string()
            } else {
                String::new()
            },
            tag,
            tag_kind,
        });

        // 签字位：同级的行都要登记（哪怕这个人不签），否则「整级都被剔光」时
        // 连行都不会出现；姓名在下面统一按 `signable` 过滤。
        match slots
            .iter_mut()
            .find(|(slot_index, ..)| *slot_index == index)
        {
            Some(slot) => {
                if !slot.3.contains(&approver) {
                    slot.3.push(approver);
                }
            }
            None => slots.push((index, level, node_name, vec![approver])),
        }
    }

    // 每一级「真要签字的人」：按 status 过滤（自审跳过 / 转办 / 作废的票不算签字人）
    let mut signable_names: HashMap<usize, Vec<String>> = HashMap::new();
    for task in tasks {
        if !signable(&task.status) {
            continue;
        }
        let index = levels
            .get(&(task.node_key.clone(), task.round))
            .copied()
            .unwrap_or(1);
        let name = task
            .assignee_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "（未记录）".to_string());
        let bucket = signable_names.entry(index).or_default();
        if !bucket.contains(&name) {
            bucket.push(name);
        }
    }

    let sign_slots: Vec<ApprovalSignSlot> = slots
        .into_iter()
        .map(|(index, level, node_name, _all)| {
            let names = signable_names.remove(&index).unwrap_or_default();
            ApprovalSignSlot {
                level,
                node_name,
                approver: if names.is_empty() {
                    "—（本节点无人签字）".to_string()
                } else {
                    names.join("、")
                },
            }
        })
        .collect();

    let mut fields = vec![
        OrderMetaField::new("单据类型", meta.biz_type.clone()),
        OrderMetaField::new("单号", meta.instance_no.clone()),
        OrderMetaField::new(
            "业务单号",
            meta.biz_no.clone().unwrap_or_else(|| "—".to_string()),
        ),
        OrderMetaField::new("金额", amount_text(&meta.form_data)),
        OrderMetaField::new("申请人", meta.initiator_name.clone()),
        OrderMetaField::new("申请时间", doc_pdf_model::beijing_time(&meta.started_at)),
        OrderMetaField::new(
            "流程",
            format!("{}（v{}）", meta.flow_name, meta.version_no),
        ),
        OrderMetaField::new("状态", instance_status_label(&meta.status)),
        OrderMetaField::new("完成时间", at_text(meta.ended_at)),
        OrderMetaField::new("标题", meta.title.clone()),
    ];
    if meta.round_total > 1 {
        fields.push(OrderMetaField::new(
            "退回轮次",
            format!("第 {} 轮", meta.round_total),
        ));
    }

    ApprovalContext::new(ApprovalContextArgs {
        doc_no: meta.instance_no.clone(),
        status: instance_status_label(&meta.status).to_string(),
        status_kind: instance_status_kind(&meta.status).to_string(),
        meta: fields,
        traces,
        sign_slots,
        result_comment: meta.result_comment.clone().unwrap_or_default(),
        printed_at: doc_pdf_model::printed_at_now(),
    })
}

/// 取数并组装上下文（真库）。
///
/// # 异常情况
///
/// - 404：审批单不存在 / 已软删；
/// - 500：数据库异常。
pub async fn instance_context(id: Uuid) -> std::result::Result<ApprovalContext, ApiError> {
    let row = model::find_instance(id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批单"))?
        .ok_or_else(|| doc_pdf_model::not_found("审批单"))?;
    let nodes = model::nodes_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批节点"))?;
    let tasks = model::timeline(id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批轨迹"))?;
    Ok(build_context(&InstanceMeta::of(&row), &nodes, &tasks))
}

/// 可见性判定（与 `handler::instance_detail` **逐条同口径**）：发起人 / 审批人（含已审、抄送）/
/// `business:flow:admin`。
///
/// # 为什么单独抽出来
///
/// 导出链路后面紧接着就是「渲染 PDF」（需要 Chromium），真库用例不可能为了测一个 403
/// 去起浏览器。判定单独成函数后，403 / 404 / 400 三条守卫都能在真库上单测，
/// 而渲染只剩「把 HTML 交给引擎」这一件事。
///
/// # 异常情况
///
/// - 403 [`error_code::FLOW_FORBIDDEN`]：与这张单无关；
/// - 500：数据库异常。
pub async fn ensure_visible(
    row: &model::InstanceRow,
    viewer: Uuid,
    admin: bool,
) -> std::result::Result<(), ApiError> {
    if admin || row.initiator_id == viewer {
        return Ok(());
    }
    let mine = model::my_instance_ids(viewer)
        .await
        .map_err(|error| utils::db_error(error, "查询我的相关审批单"))?;
    if mine.contains(&row.id) {
        return Ok(());
    }
    Err(ApiError::new(
        403,
        error_code::FLOW_FORBIDDEN,
        "你与这张审批单无关，不能导出",
    ))
}

/// 审批单 PDF（取数 + 渲染，不含 HTTP 头 / 审计）。
///
/// # 参数
///
/// - `id_raw`：路径里的 id（非法 → 400）；
/// - `viewer`：当前用户；
/// - `admin`：是否持有 `business:flow:admin`（跳过「与我相关」判定）。
///
/// # 异常情况
///
/// - 400 [`error_code::ERP_FILTER_INVALID`]：id 不是 UUID；
/// - 403 [`error_code::FLOW_FORBIDDEN`]：与这张单无关；
/// - 404：审批单不存在；
/// - 422 / 503 / 504：模板 / 渲染引擎异常。
pub async fn export_instance(
    id_raw: &str,
    viewer: Uuid,
    admin: bool,
) -> std::result::Result<RenderedDoc, ApiError> {
    let id = doc_pdf_model::parse_doc_id(id_raw)?;
    let row = model::find_instance(id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批单"))?
        .ok_or_else(|| doc_pdf_model::not_found("审批单"))?;

    // 可见性：PDF 不比详情页松（打印件更容易外流）
    ensure_visible(&row, viewer, admin).await?;

    let nodes = model::nodes_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批节点"))?;
    let tasks = model::timeline(id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批轨迹"))?;
    let context = build_context(&InstanceMeta::of(&row), &nodes, &tasks);

    let html = template::render_approval(&context).map_err(|error| error.into_api())?;
    let filename = doc_pdf_render::pdf_filename(&context.doc_label, &context.doc_no);
    let opts = template::approval_pdf_opts(&context.doc_no, Some(filename));
    doc_pdf_render::render_document(
        &html,
        &context.doc_no,
        &opts,
        context.traces.len(),
        form_amount(&row.form_data),
    )
    .await
    .map_err(|error| error.into_api())
}

/// 审批单 PDF（`GET /api/business/flow/instance/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`error_code::PERMISSION_DENIED`]：缺 `business:flow:instance:read`；
/// - 403 [`error_code::FLOW_FORBIDDEN`]：与本单无关；
/// - 400 / 404 / 422 / 503 / 504：见 [`export_instance`]。
#[nik::permission(
    path = "/business/flow/instance/{id}/pdf",
    method = "GET",
    permission = "business:flow:instance:read"
)]
#[nik::get("/business/flow/instance/{id}/pdf")]
pub async fn instance_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    let started = Instant::now();
    let viewer = utils::current_user_uuid(&claims);
    let Some(viewer) = viewer else {
        return ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识")
            .into_response();
    };
    let admin = claims.has_permission("business:flow:admin");
    match export_instance(&id, viewer, admin).await {
        Ok(rendered) => {
            let response =
                match pdf_handler::pdf_response(&rendered.bytes, Some(&rendered.filename)) {
                    Ok(response) => response,
                    Err(error) => {
                        tracing::error!(
                            doc_no = %rendered.doc_no,
                            bytes = rendered.bytes.len(),
                            "审批单 PDF 响应组装失败"
                        );
                        return error.into_response();
                    }
                };
            audit(&id, &headers, &connect_info, &claims, &rendered, started);
            response
        }
        Err(error) => error.into_response(),
    }
}

/// 补一条导出审计（失败只记日志，不改变已生成的响应）。
fn audit(
    id_raw: &str,
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    rendered: &RenderedDoc,
    started: Instant,
) {
    let Ok(instance_id) = doc_pdf_model::parse_doc_id(id_raw) else {
        return;
    };
    doc_pdf_model::ExportLogOwned {
        user_id: utils::current_user_uuid(claims),
        username: claims.username.clone(),
        operation: EXPORT_OPERATION.to_string(),
        target_type: TARGET_TYPE.to_string(),
        doc_id: instance_id,
        doc_no: rendered.doc_no.clone(),
        detail: serde_json::json!({
            "doc_type": "FLOW_INSTANCE",
            "source": "PDF-05",
            "trace_count": rendered.row_count,
            "amount": rendered.total_amount.round_dp(2).to_string(),
            "render_ms": rendered.duration_ms,
        }),
        filename: rendered.filename.clone(),
        bytes: rendered.bytes.len(),
        duration_ms: started.elapsed().as_millis() as u64,
        request_path: format!("/api/business/flow/instance/{instance_id}/pdf"),
        client_ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        request_id: Uuid::now_v7(),
    }
    .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(key: &str, name: &str) -> model::NodeRow {
        model::NodeRow {
            id: Uuid::now_v7(),
            instance_id: Uuid::now_v7(),
            node_id: None,
            node_key: key.to_string(),
            node_name: name.to_string(),
            node_type: "APPROVAL".to_string(),
            round: 1,
            status: "APPROVED".to_string(),
            approval_mode: None,
            approve_ratio: None,
            reject_veto: None,
            approver_count: 1,
            approved_count: 1,
            rejected_count: 0,
            taken_edge_key: None,
            is_auto_passed: false,
            activated_at: None,
            ended_at: None,
            duration_ms: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// 一条待办（默认：正常派单、已通过、无降级留痕）。
    fn task(node_key: &str, who: &str, status: &str) -> model::TaskRow {
        model::TaskRow {
            id: Uuid::now_v7(),
            instance_id: Uuid::now_v7(),
            instance_node_id: Uuid::now_v7(),
            node_key: node_key.to_string(),
            round: 1,
            task_kind: "APPROVAL".to_string(),
            assignee_id: Uuid::now_v7(),
            assignee_name: Some(who.to_string()),
            assignee_source: "ROLE".to_string(),
            assignee_source_ref: None,
            status: status.to_string(),
            is_added_sign: false,
            is_self_approval: false,
            is_auto: false,
            comment: None,
            attachments: Json::Array(Vec::new()),
            transferred_from: None,
            transferred_to: None,
            due_at: None,
            remind_count: 0,
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            escalated_from_role: None,
            escalate_reason: None,
            assigned_by_rule: None,
            escalated_to_role: None,
        }
    }

    fn meta(status: &str) -> InstanceMeta {
        InstanceMeta {
            instance_no: "FLW-20261108-00012345".to_string(),
            biz_type: "PURCHASE_ORDER".to_string(),
            biz_no: Some("PO-20261108-000004".to_string()),
            flow_name: "采购订单审批".to_string(),
            version_no: 1,
            title: "球阀采购 200 台".to_string(),
            status: status.to_string(),
            initiator_name: "张三".to_string(),
            form_data: serde_json::json!({ "amount": 12345.67, "reason": "备货" }),
            result_comment: None,
            round_total: 1,
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
        }
    }

    /// 验收①：普通 2 级审批 → 两行轨迹 + 各自意见 + 两个签字位。
    #[test]
    fn normal_two_levels_render_two_rows_with_opinions() {
        let nodes = vec![node("manager", "部门经理审批"), node("gm", "总经理审批")];
        let mut first = task("manager", "王五", "APPROVED");
        first.comment = Some("金额与预算一致，同意".to_string());
        let mut second = task("gm", "孙七", "APPROVED");
        second.comment = Some("同意，按流程执行".to_string());
        let tasks = vec![first, second];

        let ctx = build_context(&meta("APPROVED"), &nodes, &tasks);
        assert_eq!(ctx.traces.len(), 2);
        assert_eq!(ctx.traces[0].level, "第 1 级");
        assert_eq!(ctx.traces[0].node_name, "部门经理审批");
        assert_eq!(ctx.traces[0].approver, "王五");
        assert_eq!(ctx.traces[0].result, "通过");
        assert_eq!(ctx.traces[0].opinion, "金额与预算一致，同意");
        assert_eq!(ctx.traces[1].level, "第 2 级");
        assert_eq!(ctx.traces[1].node_name, "总经理审批");
        assert_eq!(ctx.traces[1].opinion, "同意，按流程执行");
        assert!(ctx.traces[0].tag.is_empty(), "正常派单不该有降级标记");
        assert!(ctx.traces[0].tag_kind.is_empty());
        // 签字位 = 2 级，顺序与轨迹一致
        assert_eq!(ctx.sign_slots.len(), 2);
        assert_eq!(ctx.sign_slots[0].approver, "王五");
        assert_eq!(ctx.sign_slots[1].node_name, "总经理审批");
        assert_eq!(ctx.degree_count, "2");
        assert_eq!(ctx.trace_count, "2");
        assert_eq!(ctx.status, "已通过");
        assert_eq!(ctx.status_kind, "is-approve");

        // 真的渲染成 HTML：两行轨迹 + 两条意见都得在 PDF 里
        let html = template::render_approval(&ctx).expect("模板必须渲染成功");
        assert_eq!(html.matches("class=\"trace-row\"").count(), 2);
        assert!(html.contains("金额与预算一致，同意"));
        assert!(html.contains("同意，按流程执行"));
        assert_eq!(html.matches("<td class=\"blank\"></td>").count(), 4);
    }

    /// 验收②：自审跳过 → 轨迹上有「已跳过（自审）」标记。
    #[test]
    fn self_approval_skip_is_tagged() {
        let nodes = vec![node("manager", "部门经理审批")];
        let mut skipped = task("manager", "张三", "SKIPPED");
        skipped.is_self_approval = true;
        skipped.comment = Some("发起人本人不可审批自己的单据（自审策略 SKIP）".to_string());
        let mut approved = task("manager", "王五", "APPROVED");
        approved.comment = Some("同意".to_string());

        let ctx = build_context(&meta("APPROVED"), &nodes, &[skipped, approved]);
        assert_eq!(ctx.traces[0].result, "跳过");
        assert_eq!(ctx.traces[0].tag, "已跳过（自审）");
        assert_eq!(ctx.traces[0].tag_kind, "is-skip");
        assert!(ctx.traces[1].tag.is_empty());
        // 同一级两个人 → 一个签字位，但**只列真要签字的人**（被跳过的人没签过）
        assert_eq!(ctx.sign_slots.len(), 1);
        assert_eq!(
            ctx.sign_slots[0].approver, "王五",
            "被自审跳过的人不该出现在签字位里"
        );

        let html = template::render_approval(&ctx).expect("模板必须渲染成功");
        assert!(html.contains("已跳过（自审）"));
        assert!(html.contains("tag-row is-skip"));
    }

    /// 内控排除（制单人，不是发起人）也落 SKIPPED，但要与「自审」区分。
    #[test]
    fn internal_control_skip_has_its_own_label() {
        let nodes = vec![node("manager", "部门经理审批")];
        let skipped = task("manager", "制单员", "SKIPPED");
        let ctx = build_context(&meta("APPROVED"), &nodes, &[skipped]);
        assert_eq!(ctx.traces[0].tag, "已跳过（内控）");
    }

    /// 验收③（ARCH-8）：升级 / 兜底 / 委托三种标记，以及它们的优先级。
    #[test]
    fn escalation_fallback_and_delegate_tags() {
        let nodes = vec![node("gm", "总经理审批"), node("fin", "财务复核")];

        // 升级：原角色 0 人 → 派到「总经理」
        let mut escalated = task("gm", "孙七", "APPROVED");
        escalated.escalate_reason = Some("NO_MEMBER".to_string());
        escalated.escalated_from_role = Some("SALES_CLERK".to_string());
        escalated.escalated_to_role = Some("总经理".to_string());
        escalated.assigned_by_rule = Some("ESCALATED".to_string());
        // 兜底：升级链走完无人 → 兜底人接管
        let mut fallback = task("fin", "周八", "APPROVED");
        fallback.assigned_by_rule = Some("FALLBACK".to_string());
        // 委托（P3-04）：逐人改派
        let mut delegated = task("fin", "钱九", "APPROVED");
        delegated.assigned_by_rule = Some("DELEGATED".to_string());

        let tasks = vec![escalated, fallback, delegated];
        let ctx = build_context(&meta("APPROVED"), &nodes, &tasks);
        assert_eq!(
            ctx.traces[0].tag, "已升级至 总经理（原角色无人：SALES_CLERK）",
            "升级标记要写清目标角色 + 原因 + 原角色"
        );
        assert_eq!(ctx.traces[0].tag_kind, "is-escalate");
        assert_eq!(ctx.traces[1].tag, "兜底审批人");
        assert_eq!(ctx.traces[1].tag_kind, "is-fallback");
        assert_eq!(ctx.traces[2].tag, "委托代批");
        assert_eq!(ctx.traces[2].tag_kind, "is-delegate");

        let html = template::render_approval(&ctx).expect("模板必须渲染成功");
        for needle in [
            "已升级至 总经理（原角色无人：SALES_CLERK）",
            "兜底审批人",
            "委托代批",
        ] {
            assert!(html.contains(needle), "PDF 缺标记：{needle}");
        }
    }

    /// 升级原因两种码值都要有自己的一句人话（自审剔光 / 角色 0 人）。
    #[test]
    fn escalation_reason_labels_cover_both_codes() {
        assert_eq!(
            escalate_reason_label("SKIP_SELF_APPROVAL"),
            "原角色自审剔光"
        );
        assert_eq!(escalate_reason_label("NO_MEMBER"), "原角色无人");
        assert_eq!(escalate_reason_label("SOMETHING_NEW"), "原角色无可用审批人");

        // 没有目标角色名时不许印出「已升级至 （…）」
        let mut task_row = task("gm", "孙七", "APPROVED");
        task_row.escalate_reason = Some("SKIP_SELF_APPROVAL".to_string());
        let (text, kind) = downgrade_tag(&task_row).expect("升级行必须出标记");
        assert_eq!(text, "已升级至 上级角色（原角色自审剔光）");
        assert_eq!(kind, "is-escalate");
    }

    /// 兜底优先于升级（同一条待办两个痕迹都有时，不许标成「升级」）。
    #[test]
    fn fallback_wins_over_escalation_marker() {
        let mut both = task("fin", "周八", "APPROVED");
        both.assigned_by_rule = Some("FALLBACK".to_string());
        both.escalate_reason = Some("NO_MEMBER".to_string());
        let (text, kind) = downgrade_tag(&both).expect("必须出标记");
        assert_eq!(text, "兜底审批人");
        assert_eq!(kind, "is-fallback");
    }

    /// 验收④：驳回流程 → 结论「已驳回」+ 结果「驳回」+ 驳回意见原文。
    #[test]
    fn rejected_flow_keeps_the_opinion() {
        let nodes = vec![node("gm", "总经理审批")];
        let mut rejected = task("gm", "孙七", "REJECTED");
        rejected.comment = Some("单价高于年度框架协议 8%，退回重谈。".to_string());
        let mut meta_row = meta("REJECTED");
        meta_row.result_comment = Some("总经理驳回".to_string());

        let ctx = build_context(&meta_row, &nodes, &[rejected]);
        assert_eq!(ctx.status, "已驳回");
        assert_eq!(ctx.status_kind, "is-reject");
        assert_eq!(ctx.traces[0].result, "驳回");
        assert_eq!(ctx.traces[0].cls, "is-reject");
        assert_eq!(ctx.traces[0].opinion, "单价高于年度框架协议 8%，退回重谈。");
        assert_eq!(ctx.result_comment, "总经理驳回");

        let html = template::render_approval(&ctx).expect("模板必须渲染成功");
        assert!(html.contains("已驳回"));
        assert!(html.contains("单价高于年度框架协议 8%，退回重谈。"));
        assert!(html.contains("总经理驳回"));
        assert!(html.contains("verdict is-reject"));
    }

    /// 退回一轮：同一节点 round=2 是**另一个级次**，并标「第 2 轮」。
    #[test]
    fn second_round_is_a_new_level() {
        let nodes = vec![node("manager", "部门经理审批"), node("gm", "总经理审批")];
        let mut first_round_manager = task("manager", "王五", "REJECTED");
        first_round_manager.comment = Some("先补预算".to_string());
        first_round_manager.completed_at = Some(chrono::Utc::now());
        let mut manager_round2 = task("manager", "王五", "APPROVED");
        manager_round2.round = 2;
        let mut gm_round2 = task("gm", "孙七", "APPROVED");
        gm_round2.round = 2;

        let tasks = vec![first_round_manager, manager_round2, gm_round2];
        let ctx = build_context(&meta("APPROVED"), &nodes, &tasks);
        assert_eq!(ctx.traces.len(), 3);
        assert_eq!(ctx.traces[0].level, "第 1 级");
        assert!(ctx.traces[0].round_note.is_empty(), "首轮不印轮次");
        assert_eq!(ctx.traces[1].level, "第 2 级");
        assert_eq!(ctx.traces[1].round_note, "第 2 轮");
        assert_eq!(ctx.traces[2].level, "第 3 级");
        assert_eq!(ctx.sign_slots.len(), 3);
    }

    /// 会签：同一级多人 → 多条轨迹但只有一个签字位，姓名用「、」连。
    #[test]
    fn countersign_members_share_one_sign_slot() {
        let nodes = vec![node("cs", "会签节点")];
        let tasks = vec![
            task("cs", "王五", "APPROVED"),
            task("cs", "赵六", "APPROVED"),
            task("cs", "孙七", "APPROVED"),
        ];
        let ctx = build_context(&meta("APPROVED"), &nodes, &tasks);
        assert_eq!(ctx.traces.len(), 3);
        assert_eq!(ctx.sign_slots.len(), 1);
        assert_eq!(ctx.sign_slots[0].approver, "王五、赵六、孙七");
        // 同一人重复出现（加签前的那张票）不重复列名
        let tasks = vec![
            task("cs", "王五", "APPROVED"),
            task("cs", "王五", "APPROVED"),
        ];
        let ctx = build_context(&meta("APPROVED"), &nodes, &tasks);
        assert_eq!(ctx.sign_slots[0].approver, "王五");
    }

    /// 系统代点（超时自动通过）必须留痕说明，否则打印件会误读成真人批的。
    #[test]
    fn auto_vote_is_marked() {
        let nodes = vec![node("gm", "总经理审批")];
        let mut auto_task = task("gm", "孙七", "APPROVED");
        auto_task.is_auto = true;
        let ctx = build_context(&meta("APPROVED"), &nodes, &[auto_task]);
        assert_eq!(ctx.traces[0].auto_note, "系统代点");

        // SKIPPED 行不重复标注（结果列已经写「跳过」）
        let mut skipped = task("gm", "孙七", "SKIPPED");
        skipped.is_auto = true;
        let ctx = build_context(&meta("APPROVED"), &nodes, &[skipped]);
        assert!(ctx.traces[0].auto_note.is_empty());
    }

    /// 金额口径：`amount` 优先 → `total_amount` 兜底 → 取不到印 `—`。
    #[test]
    fn form_amount_prefers_amount_then_total() {
        assert_eq!(
            amount_text(&serde_json::json!({ "amount": 12345.67 })),
            "12,345.67"
        );
        assert_eq!(
            amount_text(&serde_json::json!({ "amount": "5000.00" })),
            "5,000.00",
            "动态表单常把金额存成字符串"
        );
        assert_eq!(
            amount_text(&serde_json::json!({ "total_amount": 88 })),
            "88.00"
        );
        assert_eq!(
            amount_text(&serde_json::json!({ "amount": "1,000" })),
            "—",
            "千分位串不是十进制字面量，不许猜"
        );
        assert_eq!(amount_text(&serde_json::json!({})), "—");
        // 「有一个键但值不是数字」= 金额未知，印横线（不许印 0.00 假冒「零元」）
        assert_eq!(amount_text(&serde_json::json!({ "amount": null })), "—");
        assert_eq!(form_amount(&serde_json::json!({})), Decimal::ZERO);
        assert_eq!(
            form_amount(&serde_json::json!({ "amount": 0.1 })).to_string(),
            "0.1"
        );
    }

    /// 单价 / 金额精度：`0.005` 在分位上是「进位」，两种取法（f64 / Decimal）都不许差 1 分。
    #[test]
    fn form_amount_rounds_half_up_to_fen() {
        assert_eq!(amount_text(&serde_json::json!({ "amount": 1.005 })), "1.01");
        assert_eq!(
            amount_text(&serde_json::json!({ "amount": "2.675" })),
            "2.68"
        );
    }

    /// 没有待办（刚发起 / 只到 CC 节点）时不能炸：空轨迹 + 空签字位。
    #[test]
    fn empty_timeline_is_renderable() {
        let ctx = build_context(&meta("PENDING"), &[], &[]);
        assert!(ctx.traces.is_empty());
        assert!(ctx.sign_slots.is_empty());
        assert_eq!(ctx.trace_count, "0");
        let html = template::render_approval(&ctx).expect("空轨迹也要能渲染");
        assert!(html.contains("暂无审批记录"));
        assert!(html.contains("审批中"));
    }

    /// 节点名查不到时退回 `node_key`（不许打空单元格）。
    #[test]
    fn unknown_node_key_falls_back_to_key() {
        let ctx = build_context(
            &meta("APPROVED"),
            &[],
            &[task("orphan", "王五", "APPROVED")],
        );
        assert_eq!(ctx.traces[0].node_name, "orphan");
    }

    /// 审批人姓名快照为空时打「（未记录）」，而不是空白。
    #[test]
    fn missing_assignee_name_is_marked() {
        let mut row = task("gm", "孙七", "APPROVED");
        row.assignee_name = None;
        let ctx = build_context(&meta("APPROVED"), &[node("gm", "总经理")], &[row]);
        assert_eq!(ctx.traces[0].approver, "（未记录）");
    }

    /// 签字位只收真要签字的人；整级都被剔光时留一行「无人签字」。
    #[test]
    fn sign_slots_only_list_real_signers() {
        // 状态判据
        assert!(signable("APPROVED"));
        assert!(signable("REJECTED"));
        assert!(signable("PENDING"));
        assert!(!signable("SKIPPED"), "自审 / 内控剔掉的人不算签字人");
        assert!(!signable("TRANSFERRED"), "转办出去的人没审过");
        assert!(!signable("CANCELLED"), "或签抢先后作废的票不算");

        // 整级被剔光（节点自动通过）：行还在，但写明没人签字
        let nodes = vec![node("cs", "自动通过节点")];
        let mut skipped = task("cs", "张三", "SKIPPED");
        skipped.is_self_approval = true;
        let ctx = build_context(&meta("APPROVED"), &nodes, &[skipped]);
        assert_eq!(ctx.traces.len(), 1, "轨迹仍要留痕");
        assert_eq!(ctx.sign_slots.len(), 1, "「这一级走过了」本身要印");
        assert_eq!(ctx.sign_slots[0].approver, "—（本节点无人签字）");
        let html = template::render_approval(&ctx).expect("模板必须渲染成功");
        assert!(html.contains("—（本节点无人签字）"));
        // 空白签字栏照样留（现场仍可能补签）
        assert_eq!(html.matches("<td class=\"blank\"></td>").count(), 2);
    }

    /// 状态映射的完整性（改码值时这条会先红）。
    #[test]
    fn status_labels_and_kinds() {
        assert_eq!(result_label("APPROVED"), "通过");
        assert_eq!(result_label("REJECTED"), "驳回");
        assert_eq!(result_label("SKIPPED"), "跳过");
        assert_eq!(result_label("TRANSFERRED"), "已转办");
        assert_eq!(result_label("CANCELLED"), "已作废");
        assert_eq!(result_label("PENDING"), "待审批");
        assert_eq!(result_cls("APPROVED"), "is-approve");
        assert_eq!(result_cls("REJECTED"), "is-reject");
        assert_eq!(result_cls("SKIPPED"), "is-skip");
        assert_eq!(result_cls("PENDING"), "is-pending");

        assert_eq!(instance_status_label("PENDING"), "审批中");
        assert_eq!(instance_status_label("APPROVED"), "已通过");
        assert_eq!(instance_status_label("REJECTED"), "已驳回");
        assert_eq!(instance_status_label("CANCELLED"), "已取消");
        assert_eq!(instance_status_label("WITHDRAWN"), "已撤回");
        assert_eq!(instance_status_kind("PENDING"), "is-pending");
        assert_eq!(instance_status_kind("APPROVED"), "is-approve");
        assert_eq!(instance_status_kind("REJECTED"), "is-reject");
        assert_eq!(instance_status_kind("WITHDRAWN"), "is-void");
    }

    /// 权限码 / 操作码 / 目标表名三者不能漂移（点错名字就是点错权限）。
    #[test]
    fn operation_and_permission_constants() {
        assert_eq!(EXPORT_OPERATION, "EXPORT_FLOW_INSTANCE_PDF");
        assert_eq!(PERMISSION, "business:flow:instance:read");
        assert_eq!(TARGET_TYPE, "biz_flow_instance");
    }

    /// 退回多轮时单据头要印「退回轮次」。
    #[test]
    fn round_total_is_printed() {
        let mut meta_row = meta("PENDING");
        meta_row.round_total = 2;
        let ctx = build_context(&meta_row, &[], &[]);
        let flat: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        assert!(flat.contains(&("退回轮次".to_string(), "第 2 轮".to_string())));
    }
}

// ==================== 真库用例（ERP_DB_URL；未设置则 SKIP） ====================

/// 真库用例：审批单 PDF 的取数与可见性。
///
/// # 跑法
///
/// ```bash
/// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<测试库>'
/// cargo test --lib business::flow::instance_pdf -- --test-threads=1
/// ```
///
/// # 夹具口径
///
/// 独占命名空间 `0f61…`（PDF-05），进 / 出都按这一组 id 清；不扫单号前缀、不删别人的行。
/// 审批行按**引擎真实写法**造（`status/completed_at` 成对、`is_self_approval`、`escalate_reason`、
/// `assigned_by_rule` 都与 `engine.rs` 落库的形态一致），因为本卡测的是「把它们读出来印对」，
/// 引擎侧的语义由 `flow::engine` / `p3-02~05` 的用例与证据负责。
#[cfg(test)]
mod db_tests {
    use super::*;

    // 夹具 id 命名空间 0f61…
    //   0001 角色「PDF05 总经理」/ 0002 发起人 张三 / 0003 审批人 王五 / 0004 审批人 孙七 /
    //   0005 无关人 李四 / 0010 流程定义 / 0011 版本 / 0020 实例 /
    //   0030 节点 manager / 0031 节点 gm / 0040-0042 待办
    //   （只把代码里要用的 id 抽成常量；纯 SQL 字面量的那几个写在注释里）
    const INITIATOR: Uuid = Uuid::from_u128(0x0f61_0000_0000_4000_8000_0000_0000_0002);
    const APPROVER: Uuid = Uuid::from_u128(0x0f61_0000_0000_4000_8000_0000_0000_0003);
    const GM: Uuid = Uuid::from_u128(0x0f61_0000_0000_4000_8000_0000_0000_0004);
    const STRANGER: Uuid = Uuid::from_u128(0x0f61_0000_0000_4000_8000_0000_0000_0005);
    const INSTANCE: Uuid = Uuid::from_u128(0x0f61_0000_0000_4000_8000_0000_0000_0020);

    const INSTANCE_NO: &str = "PDF05-FLW-000001";

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn exec(sql: &str, params: Vec<Value>, label: &str) {
        db().execute_with_params(sql, params)
            .await
            .unwrap_or_else(|error| panic!("{label}：{error}\nSQL: {sql}"));
    }

    async fn purge() {
        for sql in [
            // 待办 → 节点 → 抄送 → 实例 → 版本 → 定义（外键顺序）
            "DELETE FROM biz_flow_task WHERE instance_id = '0f610000-0000-4000-8000-000000000020'::uuid",
            "DELETE FROM biz_flow_cc WHERE instance_id = '0f610000-0000-4000-8000-000000000020'::uuid",
            "DELETE FROM biz_flow_instance_node WHERE instance_id = '0f610000-0000-4000-8000-000000000020'::uuid",
            "DELETE FROM biz_flow_instance WHERE instance_no LIKE 'PDF05-%'",
            "DELETE FROM biz_flow_instance WHERE id = '0f610000-0000-4000-8000-000000000020'::uuid",
            "DELETE FROM biz_flow_def_version WHERE def_id = '0f610000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM biz_flow_def WHERE flow_code LIKE 'pdf05_%'",
            "DELETE FROM biz_flow_def WHERE id = '0f610000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM operation_logs WHERE target_id = '0f610000-0000-4000-8000-000000000020'",
            "DELETE FROM sys_user_role WHERE user_id IN (\
                '0f610000-0000-4000-8000-000000000002'::uuid, '0f610000-0000-4000-8000-000000000003'::uuid, \
                '0f610000-0000-4000-8000-000000000004'::uuid, '0f610000-0000-4000-8000-000000000005'::uuid)",
            "DELETE FROM sys_user WHERE username LIKE 'pdf05.%'",
            "DELETE FROM sys_role WHERE role_code LIKE 'PDF05_%'",
        ] {
            exec(sql, vec![], "清理 PDF-05 审批夹具").await;
        }
    }

    /// 造一张「2 级审批 + 自审跳过 + 升级」都已留痕的实例（终态 APPROVED）。
    async fn seed() {
        for sql in [
            "INSERT INTO sys_role (id, role_code, role_name, status, sort_order)
             VALUES ('0f610000-0000-4000-8000-000000000001', 'PDF05_GM', 'PDF05 总经理', 1, 97)",
            "INSERT INTO sys_user (id, username, password, real_name, status)
             VALUES ('0f610000-0000-4000-8000-000000000002', 'pdf05.zhangsan', 'x', '张三', 1),
                    ('0f610000-0000-4000-8000-000000000003', 'pdf05.wangwu', 'x', '王五', 1),
                    ('0f610000-0000-4000-8000-000000000004', 'pdf05.sunqi', 'x', '孙七', 1),
                    ('0f610000-0000-4000-8000-000000000005', 'pdf05.lisi', 'x', '李四', 1)",
            // 定义用 DRAFT 直插：`ck_biz_flow_def_published_has_version` 要求 PUBLISHED 必须已有
            // `current_version_id`，而本用例只关心「实例读什么」，不测发布链路（那有专门的卡）
            "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, status)
             VALUES ('0f610000-0000-4000-8000-000000000010', 'pdf05_po_flow', 'PDF05 采购订单审批', 'PURCHASE_ORDER', 'DRAFT')",
            // 版本同样用 DRAFT：`ck_biz_flow_def_version_published` 要求 PUBLISHED 必须带
            // 快照 + hash + 发布时间；本用例的实例只读 `biz_flow_task`，不需要真图
            "INSERT INTO biz_flow_def_version (id, def_id, version_no, version_status)
             VALUES ('0f610000-0000-4000-8000-000000000011',
                     '0f610000-0000-4000-8000-000000000010'::uuid, 1, 'DRAFT')",
            // 终态实例：status 非 PENDING ⇒ ended_at 必须非空（ck_biz_flow_instance_end）
            "INSERT INTO biz_flow_instance
                 (id, instance_no, def_id, version_id, version_no, flow_code, flow_name,
                  biz_type, biz_no, title, status, initiator_id, initiator_name, form_data,
                  started_at, ended_at)
             VALUES ('0f610000-0000-4000-8000-000000000020', 'PDF05-FLW-000001',
                     '0f610000-0000-4000-8000-000000000010'::uuid,
                     '0f610000-0000-4000-8000-000000000011'::uuid, 1, 'pdf05_po_flow',
                     'PDF05 采购订单审批', 'PURCHASE_ORDER', 'PDF05-PO-000001', '球阀采购 200 台',
                     'APPROVED', '0f610000-0000-4000-8000-000000000002'::uuid, '张三',
                     '{\"amount\": 12345.67, \"reason\": \"备货\"}'::jsonb,
                     '2026-11-08 01:00:00+00', '2026-11-08 02:30:00+00')",
            "INSERT INTO biz_flow_instance_node
                 (id, instance_id, node_key, node_name, node_type, round, status,
                  approver_count, approved_count, activated_at, ended_at)
             VALUES ('0f610000-0000-4000-8000-000000000030',
                     '0f610000-0000-4000-8000-000000000020'::uuid, 'manager', '部门经理审批',
                     'APPROVAL', 1, 'APPROVED', 2, 1,
                     '2026-11-08 01:00:00+00', '2026-11-08 01:40:00+00'),
                    ('0f610000-0000-4000-8000-000000000031',
                     '0f610000-0000-4000-8000-000000000020'::uuid, 'gm', '总经理审批',
                     'APPROVAL', 1, 'APPROVED', 1, 1,
                     '2026-11-08 01:40:00+00', '2026-11-08 02:30:00+00')",
            // 待办：SKIPPED（自审）/ APPROVED（正常）/ APPROVED（升级，原角色 PDF05_CLERK → 总经理）
            "INSERT INTO biz_flow_task
                 (id, instance_id, instance_node_id, node_key, round, task_kind, assignee_id,
                  assignee_name, assignee_source, assignee_source_ref, status, is_self_approval,
                  is_auto, comment, created_at, completed_at,
                  escalated_from_role, escalate_reason, assigned_by_rule)
             VALUES ('0f610000-0000-4000-8000-000000000040',
                     '0f610000-0000-4000-8000-000000000020'::uuid,
                     '0f610000-0000-4000-8000-000000000030'::uuid, 'manager', 1, 'APPROVAL',
                     '0f610000-0000-4000-8000-000000000002'::uuid, '张三', 'ROLE', NULL, 'SKIPPED',
                     TRUE, FALSE, '发起人本人不能审批自己的单据（自审策略 SKIP）',
                     '2026-11-08 01:00:00+00', '2026-11-08 01:00:00+00', NULL, NULL, NULL),
                    ('0f610000-0000-4000-8000-000000000041',
                     '0f610000-0000-4000-8000-000000000020'::uuid,
                     '0f610000-0000-4000-8000-000000000030'::uuid, 'manager', 1, 'APPROVAL',
                     '0f610000-0000-4000-8000-000000000003'::uuid, '王五', 'ROLE', NULL, 'APPROVED',
                     FALSE, FALSE, '预算内，同意采购', '2026-11-08 01:00:00+00',
                     '2026-11-08 01:40:00+00', NULL, NULL, 'ROLE'),
                    ('0f610000-0000-4000-8000-000000000042',
                     '0f610000-0000-4000-8000-000000000020'::uuid,
                     '0f610000-0000-4000-8000-000000000031'::uuid, 'gm', 1, 'APPROVAL',
                     '0f610000-0000-4000-8000-000000000004'::uuid, '孙七', 'ROLE',
                     '0f610000-0000-4000-8000-000000000001'::uuid, 'APPROVED', FALSE, FALSE,
                     '同意，按流程执行', '2026-11-08 01:40:00+00', '2026-11-08 02:30:00+00',
                     'PDF05_CLERK', 'NO_MEMBER', 'ESCALATED')",
        ] {
            exec(sql, vec![], "写入 PDF-05 审批夹具").await;
        }
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        purge().await;
        seed().await;
        Some(guard)
    }

    /// 真库取数：级次 / 节点名 / 审批人 / 结果 / 意见 / 三种 ARCH-8 标记全部到位。
    #[tokio::test]
    async fn instance_context_reads_full_timeline() {
        let Some(_guard) = setup().await else {
            return;
        };
        let ctx = instance_context(INSTANCE)
            .await
            .expect("上下文必须组装成功");

        assert_eq!(ctx.doc_no, INSTANCE_NO);
        assert_eq!(ctx.doc_title, "审 批 单");
        assert_eq!(ctx.status, "已通过");
        assert_eq!(ctx.status_kind, "is-approve");

        // 单据信息（卡面「单据信息」段逐项）
        let meta: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        for (label, value) in [
            ("单据类型", "PURCHASE_ORDER"),
            ("单号", INSTANCE_NO),
            ("业务单号", "PDF05-PO-000001"),
            ("金额", "12,345.67"),
            ("申请人", "张三"),
            ("流程", "PDF05 采购订单审批（v1）"),
            ("状态", "已通过"),
        ] {
            assert!(
                meta.contains(&(label.to_string(), value.to_string())),
                "单据头缺/错：{label} = {value}（实际 {meta:?}）"
            );
        }
        // 申请时间按北京时间印（+08:00）
        let started = meta
            .iter()
            .find(|(label, _)| label == "申请时间")
            .map(|(_, value)| value.clone())
            .expect("必须有申请时间");
        assert_eq!(started, "2026-11-08 09:00");

        // 轨迹：3 条（SKIPPED 自审 + 王五通过 + 升级到总经理的孙七）
        assert_eq!(ctx.traces.len(), 3);
        assert_eq!(ctx.traces[0].level, "第 1 级");
        assert_eq!(ctx.traces[0].node_name, "部门经理审批");
        assert_eq!(ctx.traces[0].approver, "张三");
        assert_eq!(ctx.traces[0].result, "跳过");
        assert_eq!(ctx.traces[0].tag, "已跳过（自审）");
        assert_eq!(ctx.traces[1].result, "通过");
        assert_eq!(ctx.traces[1].opinion, "预算内，同意采购");
        assert_eq!(ctx.traces[1].tag, "", "正常派单不许有标记");
        assert_eq!(ctx.traces[2].level, "第 2 级");
        assert_eq!(ctx.traces[2].node_name, "总经理审批");
        assert_eq!(ctx.traces[2].approver, "孙七");
        assert_eq!(
            ctx.traces[2].tag, "已升级至 PDF05 总经理（原角色无人：PDF05_CLERK）",
            "升级标记要打出目标角色名 + 原因 + 原角色编码"
        );

        // 签字位：2 级；第 1 级只剩真正签过的王五（张三 是自审跳过，不占签字位）
        assert_eq!(ctx.sign_slots.len(), 2);
        assert_eq!(
            ctx.sign_slots[0].approver, "王五",
            "被自审跳过的张三不该出现在签字位"
        );
        assert_eq!(ctx.sign_slots[1].approver, "孙七");

        // 落到 HTML 上也要有（验收②③的字样）
        let html = template::render_approval(&ctx).expect("模板必须渲染成功");
        assert_eq!(html.matches("class=\"trace-row\"").count(), 3);
        assert!(html.contains("已跳过（自审）"));
        assert!(html.contains("已升级至 PDF05 总经理（原角色无人：PDF05_CLERK）"));
        assert!(html.contains("预算内，同意采购"));
    }

    /// 可见性：发起人 / 审批人 / 超管可导出，无关人 403。
    #[tokio::test]
    async fn visibility_follows_instance_detail_rules() {
        let Some(_guard) = setup().await else {
            return;
        };
        let row = model::find_instance(INSTANCE)
            .await
            .expect("查实例")
            .expect("实例必须在");

        ensure_visible(&row, INITIATOR, false)
            .await
            .expect("发起人必须能导出");
        ensure_visible(&row, APPROVER, false)
            .await
            .expect("审批人必须能导出");
        ensure_visible(&row, GM, false)
            .await
            .expect("升级后的审批人也必须能导出");

        let error = ensure_visible(&row, STRANGER, false)
            .await
            .expect_err("无关人必须 403");
        assert_eq!(error.status, 403);
        assert_eq!(error.code, error_code::FLOW_FORBIDDEN);
        assert!(error.msg.contains("无关"));

        // 流程管理员（business:flow:admin）不受「与我相关」限制
        ensure_visible(&row, STRANGER, true)
            .await
            .expect("管理员必须能导出任何单");
    }

    /// 不存在 / 已软删的实例都 404（不是「打一张空轨迹的单」）。
    #[tokio::test]
    async fn missing_or_deleted_instance_is_404() {
        let Some(_guard) = setup().await else {
            return;
        };
        let missing = Uuid::from_u128(0x0f61_0000_0000_4000_8000_0000_0000_00ff);
        let error = instance_context(missing)
            .await
            .expect_err("不存在的实例必须 404");
        assert_eq!(error.status, 404);
        assert!(error.msg.contains("审批单"));

        exec(
            "UPDATE biz_flow_instance SET deleted_at = now() \
              WHERE id = '0f610000-0000-4000-8000-000000000020'::uuid",
            vec![],
            "软删审批实例",
        )
        .await;
        let error = instance_context(INSTANCE)
            .await
            .expect_err("软删实例必须 404");
        assert_eq!(error.status, 404);
    }

    /// id 不是合法 UUID 时导出必须 400（在查库之前就拦住）。
    #[tokio::test]
    async fn export_rejects_bad_uuid_before_query() {
        if !ensure_db().await {
            return;
        }
        let error = export_instance("not-a-uuid", INITIATOR, false)
            .await
            .expect_err("必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
    }

    /// 导出审计行：与 handler 同一条写入路径，字段口径一致（trace_count 而非 row_count）；
    /// 同时钉住 `write_export_log` 的 `source` 语义（调用方给了就不覆盖成默认的 PDF-03）。
    #[tokio::test]
    async fn export_writes_operation_log_row() {
        let Some(_guard) = setup().await else {
            return;
        };
        let ctx = instance_context(INSTANCE).await.expect("上下文必须成功");
        let request_id = Uuid::now_v7();
        doc_pdf_model::write_export_log(&doc_pdf_model::ExportLog {
            user_id: Some(INITIATOR),
            username: Some("pdf05.zhangsan"),
            operation: EXPORT_OPERATION,
            target_type: TARGET_TYPE,
            doc_id: INSTANCE,
            doc_no: INSTANCE_NO,
            detail: serde_json::json!({
                "doc_type": "FLOW_INSTANCE",
                "source": "PDF-05",
                "trace_count": ctx.traces.len(),
                "amount": form_amount(&serde_json::json!({"amount": 12345.67})).to_string(),
            }),
            filename: "审批单-PDF05-FLW-000001.pdf",
            bytes: 68_000,
            duration_ms: 180,
            request_path: "/api/business/flow/instance/0f610000-0000-4000-8000-000000000020/pdf",
            client_ip: Some(std::net::IpAddr::from([127, 0, 0, 1])),
            user_agent: Some("curl/8.5.0".to_string()),
            request_id,
        })
        .await
        .expect("审计写入必须成功");

        let rows = db()
            .find_with_params(
                "SELECT operation, target_type, detail FROM operation_logs \
                  WHERE target_id = ? AND request_id = ?",
                vec![
                    Value::Text(INSTANCE.to_string()),
                    Value::Text(request_id.to_string()),
                ],
            )
            .await
            .expect("回查审计行");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get_typed::<String>("operation").unwrap(),
            EXPORT_OPERATION
        );
        assert_eq!(
            rows[0].get_typed::<String>("target_type").unwrap(),
            TARGET_TYPE
        );
        let detail: serde_json::Value = rows[0].get_typed("detail").expect("detail 必须是 jsonb");
        assert_eq!(detail["doc_type"], "FLOW_INSTANCE");
        assert_eq!(detail["trace_count"], 3);
        assert_eq!(detail["amount"], "12345.67");
        assert_eq!(
            detail["source"], "PDF-05",
            "调用方给了 source 就不许被默认值 PDF-03 覆盖"
        );
        assert_eq!(detail["doc_no"], INSTANCE_NO, "单号必须落进 detail");
    }
}
