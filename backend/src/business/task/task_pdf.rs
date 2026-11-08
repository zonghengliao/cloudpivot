//! 工作任务 **派工单 PDF 导出**（PDF-06）。
//!
//! # 接口（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/task/{id}/pdf` | `business:task:read` | 派工单 PDF（任务信息 + 描述全文 + 检查项 + 签字栏） |
//!
//! # 卡面要求的字段落点
//!
//! | 卡面 | 数据来源 |
//! |------|----------|
//! | 任务号 / 任务标题 / 优先级 / 截止日期 | `biz_task.task_no / title / priority / due_date` |
//! | 负责人 / 协作人 / 所属项目 | `owner_name` / `biz_task_member`（`COLLABORATOR`）/ `project_code + project_name` |
//! | 任务描述（完整正文） | `biz_task.description` **原文照印**（见下「描述」） |
//! | 检查项 | 直接子任务（`biz_task.parent_id = 本任务`），每条一个**空白**勾选框 |
//! | 签字栏：派工人 / 接收人 / 完成确认 / 日期 | `assigner`（自建任务 = 创建人）/ `owner` / 验收口径 / 空 |
//!
//! ## 描述：原文照印，不做 Markdown 渲染
//!
//! `biz_task.description` 存的是「纯文本 / Markdown 源文」。派工单是发到现场的
//! 纸质单据，**必须与库里的原文逐字一致**：把 `##` 渲染成标题、`**` 渲染成加粗，
//! 就等于打印件与系统内容出现差异，出了纠纷无法对账（ auditors 比对的是文本层）。
//! 因此模板只做 HTML 转义 + `white-space: pre-wrap`（保留换行与缩进），
//! 不做任何语法解释。这条决定由单测 `long_description_is_verbatim` 钉住。
//!
//! ## 检查项 = 直接子任务，且**不印状态**
//!
//! 任务模块没有独立的 checklist 表（V0100 的 7 张表里没有），产品语义上
//! 「拆解出来的检查项」就是子任务（任务模板的 `subtasks` jsonb 一键建任务时
//! 也是落成子任务，见 `business/task/template.rs`）。所以：
//!
//! - 取 `model::list_children_for_document`（**这张单的全部直接子任务**，不做数据范围
//!   过滤 —— 子任务是本单的内容，按可见性过滤会让派工单静默漏项，见该函数文档）；
//! - 每条印「序号 / 标题 / 负责人 / 截止日」+ 一个**空白**方框；
//! - **跳过 `CANCELLED`**：已取消的子任务不再是接收人要做的活，印上去只会让人
//!   勾一个不存在的事项；
//! - **不印子任务的 `status`**（卡面「空白供手填」）—— 见 [`checklist_of`]。
//!
//! ## 可见性：与 `GET /business/task/{id}` 逐条同口径
//!
//! 走同一条 [`model::find_detail`]（`scope ∪ 参与关系`），看不见就 `404 + 63001`，
//! **不是 403**：403 会告诉调用者「这个任务 ID 存在」。PDF 不比详情页松 ——
//! 打印件更容易外流。
//!
//! ## 审计
//!
//! 导出是 GET，`middleware::operation_log` 只记写方法；渲染成功后显式补一行
//! `EXPORT_TASK_PDF`（`target_type = biz_task`，`detail.source = PDF-06`）。
//! 写日志失败只记 error，不改已经生成的响应。

use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::doc_pdf_model;
use crate::business::erp::doc_pdf_render::{self, RenderedDoc};
use crate::business::pdf::handler as pdf_handler;
use crate::business::pdf::template::{
    self, OrderMetaField, TaskChecklistItem, TaskContext, TaskContextArgs, TaskSignCell,
};
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::task::handler::{not_found, parse_uuid_field};
use crate::business::task::model::{self, TaskCardRow, TaskDetailRow, TaskMemberRow};
use crate::shared::error_code;
use crate::utils;

/// 审计里的操作码。
pub const EXPORT_OPERATION: &str = "EXPORT_TASK_PDF";

/// 路由权限码（与任务详情同一码：PDF 是同一份数据的另一种呈现）。
pub const PERMISSION: &str = "business:task:read";

/// `operation_logs.target_type`（任务主体表名）。
pub const TARGET_TYPE: &str = "biz_task";

// ==================== 纯口径：枚举文案 ====================

/// 优先级 → 打印文案。
///
/// # 为什么要带中文标签
///
/// 派工单是给班组看的纸：只印 `P1` 得先记住四级映射。文案与
/// `V0100__create_task_tables.up.sql` 的列注释逐字一致（`P0 最高 / P1 高 /
/// P2 中（默认）/ P3 低`），改码值时两边一起看。
pub fn priority_label(priority: &str) -> &'static str {
    match priority {
        "P0" => "P0（最高）",
        "P1" => "P1（高）",
        "P2" => "P2（中）",
        "P3" => "P3（低）",
        _ => "—",
    }
}

/// 状态 → 打印文案（与前端 `status-map.ts` 的 `task` 域同义）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        "TODO" => "待开始",
        "DOING" => "进行中",
        "REVIEW" => "待验收",
        "DONE" => "已完成",
        "CANCELLED" => "已取消",
        _ => "未知",
    }
}

/// 状态 → 模板配色类。
pub fn status_kind(status: &str) -> &'static str {
    match status {
        "TODO" => "is-todo",
        "DOING" => "is-doing",
        "REVIEW" => "is-review",
        "DONE" => "is-done",
        _ => "is-void",
    }
}

/// 日期 → `yyyy-MM-dd`；无则 `—`（**不编日期**：没有截止日的任务本来就可能没有）。
pub fn date_text(value: Option<chrono::NaiveDate>) -> String {
    match value {
        Some(date) => date.format("%Y-%m-%d").to_string(),
        None => "—".to_string(),
    }
}

/// 协作人一栏：只取 `COLLABORATOR`，多个用「、」连，空则 `—`。
///
/// # 为什么不带关注者（`FOLLOWER`）
///
/// 派工单上印「协作人」是给现场看的「还有谁一起干」；关注者只是订阅通知，
/// 把他印进协作人会让接收人去找一个根本不分担工作的人。
pub fn collaborators_text(members: &[TaskMemberRow]) -> String {
    let names: Vec<&str> = members
        .iter()
        .filter(|member| member.role == "COLLABORATOR")
        .map(|member| member.user_name.as_str())
        .filter(|name| !name.trim().is_empty())
        .collect();
    if names.is_empty() {
        "—".to_string()
    } else {
        names.join("、")
    }
}

/// 派工人显示名：有分派人就印分派人，自建任务（`assigner_id IS NULL`）印创建人。
///
/// # 为什么自建任务不是「—」
///
/// 自建任务的「派工人」就是创建人自己（他给自己派活）。印「—」会让签字栏
/// 看起来缺一个人，现场还得回来问「这张单谁派的」。
pub fn assigner_display(row: &TaskDetailRow) -> String {
    let name = row.assigner_name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    let creator = row.creator_name.trim();
    if creator.is_empty() {
        "—".to_string()
    } else {
        creator.to_string()
    }
}

/// 所属项目：`编码 名称` / 只有名称 / `—`（项目已软删或未归属）。
pub fn project_text(row: &TaskDetailRow) -> String {
    let code = row.card.project_code.trim();
    let name = row.card.project_name.trim();
    match (code.is_empty(), name.is_empty()) {
        (false, false) => format!("{code} {name}"),
        (true, false) => name.to_string(),
        (false, true) => code.to_string(),
        (true, true) => "—".to_string(),
    }
}

/// 预估工时：`16.0 小时` / `—`。
pub fn estimate_text(hours: Option<f64>) -> String {
    match hours {
        Some(value) => format!("{value:.1} 小时"),
        None => "—".to_string(),
    }
}

/// 子任务 → 检查项（**跳过 `CANCELLED`**，序号连续）。
///
/// # 两个刻意为之
///
/// 1. **不读 `status`**：卡面验收②要求勾选框「空白供手填」，纸质件是接收人的
///    确认凭证；预印 ☑ 会与「纸上已勾」混淆（结构体 `TaskChecklistItem` 干脆
///    没有 status 字段，模板想印也印不出来）；
/// 2. **序号连续**：跳过一个已取消的子任务后，序号不能留空洞（`1,3,4` 会被
///    现场当成「漏印了一项」）。
///
/// # 参数
///
/// - `children`：`model::list_children` 的结果（已按 `created_at` 升序）。
pub fn checklist_of(children: &[TaskCardRow]) -> Vec<TaskChecklistItem> {
    children
        .iter()
        .filter(|child| child.status != "CANCELLED")
        .enumerate()
        .map(|(index, child)| TaskChecklistItem {
            seq: (index + 1).to_string(),
            title: child.title.clone(),
            owner: if child.owner_name.trim().is_empty() {
                "—".to_string()
            } else {
                child.owner_name.clone()
            },
            due: date_text(child.due_date),
        })
        .collect()
}

/// 签字栏（固定 4 格）。
///
/// `hint` 是**预印的灰色小字**（谁该签 / 验收口径），签名与日期区一律留空。
/// `完成确认` 的口径取 `need_review`：
///
/// - `true` → 由创建人验收（状态机里 `REVIEW → DONE` 的验收人就是创建人 /
///   分派人，见 `task/state.rs` 的迁移白名单）；
/// - `false` → 任务可直接 `DONE`，无需验收，纸上写明「无需验收」，
///   免得现场等一个不会来的验收人。
pub fn sign_cells_of(row: &TaskDetailRow) -> Vec<TaskSignCell> {
    let confirm_hint = if row.card.need_review {
        format!("验收人：{}", row.creator_name)
    } else {
        "无需验收（完成后直接归档）".to_string()
    };
    vec![
        TaskSignCell {
            label: "派工人".to_string(),
            hint: assigner_display(row),
        },
        TaskSignCell {
            label: "接收人".to_string(),
            hint: if row.card.owner_name.trim().is_empty() {
                "—".to_string()
            } else {
                row.card.owner_name.clone()
            },
        },
        TaskSignCell {
            label: "完成确认".to_string(),
            hint: confirm_hint,
        },
        TaskSignCell {
            label: "日期".to_string(),
            hint: String::new(),
        },
    ]
}

// ==================== 取数 → 上下文 ====================

/// 组装派工单上下文（纯函数，真库用例与 HTTP 链路共用同一份口径）。
///
/// # 参数
///
/// - `row`：任务详情行（含负责人 / 创建人 / 分派人姓名与项目）；
/// - `members`：协作人 / 关注者（`COLLABORATOR` 才进「协作人」栏）；
/// - `children`：直接子任务（进「检查项」）；
/// - `printed_at`：打印时间（调用方给，便于固定用例）。
pub fn build_context(
    row: &TaskDetailRow,
    members: &[TaskMemberRow],
    children: &[TaskCardRow],
    printed_at: String,
) -> TaskContext {
    let card = &row.card;
    let meta = vec![
        OrderMetaField::new("任务号", card.task_no.clone()),
        OrderMetaField::new("优先级", priority_label(&card.priority)),
        OrderMetaField::new("截止日期", date_text(card.due_date)),
        OrderMetaField::new("负责人", card.owner_name.clone()),
        OrderMetaField::new("协作人", collaborators_text(members)),
        OrderMetaField::new("所属项目", project_text(row)),
        OrderMetaField::new("状态", status_label(&card.status)),
        OrderMetaField::new("创建人", row.creator_name.clone()),
        OrderMetaField::new("分派人", assigner_display(row)),
        OrderMetaField::new("计划开始", date_text(card.start_date)),
        OrderMetaField::new("预估工时", estimate_text(row.estimate_hours)),
        OrderMetaField::new(
            "部门",
            if row.dept_name.trim().is_empty() {
                "—".to_string()
            } else {
                row.dept_name.clone()
            },
        ),
    ];

    TaskContext::new(TaskContextArgs {
        doc_no: card.task_no.clone(),
        subject: card.title.clone(),
        status: status_label(&card.status).to_string(),
        status_kind: status_kind(&card.status).to_string(),
        meta,
        description: row.description.clone().unwrap_or_default(),
        checklist: checklist_of(children),
        sign_cells: sign_cells_of(row),
        printed_at,
    })
}

/// 取数并组装上下文（真库）。
///
/// # 异常情况
///
/// - 400 [`error_code::PARAM_INVALID`]：id 不是 UUID；
/// - 404 [`error_code::TASK_NOT_FOUND`]：任务不存在 / 已软删 / **对本用户不可见**；
/// - 500：数据库异常。
pub async fn load_context(
    principal: &ScopedPrincipal,
    id_raw: &str,
) -> std::result::Result<TaskContext, ApiError> {
    let id = parse_uuid_field(id_raw, "任务")?;
    let row = model::find_detail(principal, id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;
    let members = model::list_members(id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务协作人"))?;
    // 子任务 = 这张单上的「检查项」：**不加数据范围过滤**（父任务可见性已在上面校验过）。
    // 理由见 `model::list_children_for_document` 的文档：按可见性过滤会让派工单
    // 静默漏掉别人负责的检查项，接收人照残单干活。
    let children = model::list_children_for_document(id)
        .await
        .map_err(|error| utils::db_error(error, "查询子任务"))?;
    Ok(build_context(
        &row,
        &members,
        &children,
        doc_pdf_model::printed_at_now(),
    ))
}

/// 派工单 PDF（取数 + 渲染，不含 HTTP 头 / 审计）。
///
/// # 异常情况
///
/// - 401：令牌缺用户标识；
/// - 403 [`error_code::TASK_FORBIDDEN`]：缺 `business:task:read`（路由注解已拦一次，
///   这里再判一次 —— 单测可直接证明「无码 → 403」）；
/// - 400 / 404 / 500：见 [`load_context`]；
/// - 422 / 503 / 504：模板 / 渲染引擎异常。
pub async fn export_task(
    id_raw: &str,
    claims: &TokenClaims,
) -> std::result::Result<RenderedDoc, ApiError> {
    if !claims.has_permission(PERMISSION) {
        return Err(ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            format!("没有任务读取权限（{PERMISSION}），不能导出派工单"),
        ));
    }
    let principal = principal::resolve_for_claims(claims).await?;
    let context = load_context(&principal, id_raw).await?;

    let html = template::render_task(&context).map_err(|error| error.into_api())?;
    let filename = doc_pdf_render::pdf_filename(&context.doc_label, &context.doc_no);
    let opts = template::task_pdf_opts(&context.doc_no, Some(filename));
    doc_pdf_render::render_document(
        &html,
        &context.doc_no,
        &opts,
        context.checklist.len(),
        Decimal::ZERO,
    )
    .await
    .map_err(|error| error.into_api())
}

/// 派工单 PDF（`GET /api/business/task/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`error_code::PERMISSION_DENIED`]：缺 `business:task:read`（中间件）；
/// - 403 [`error_code::TASK_FORBIDDEN`]：claims 里没有该码（handler 兜底）；
/// - 404 [`error_code::TASK_NOT_FOUND`]：任务不存在或与本用户无关；
/// - 400 / 422 / 503 / 504：见 [`export_task`]。
#[nik::permission(
    path = "/business/task/{id}/pdf",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/pdf")]
pub async fn task_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    let started = Instant::now();
    match export_task(&id, &claims).await {
        Ok(rendered) => {
            let response =
                match pdf_handler::pdf_response(&rendered.bytes, Some(&rendered.filename)) {
                    Ok(response) => response,
                    Err(error) => {
                        tracing::error!(
                            doc_no = %rendered.doc_no,
                            bytes = rendered.bytes.len(),
                            "派工单 PDF 响应组装失败"
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
    let Some(task_id) = utils::parse_uuid(id_raw) else {
        return;
    };
    doc_pdf_model::ExportLogOwned {
        user_id: utils::current_user_uuid(claims),
        username: claims.username.clone(),
        operation: EXPORT_OPERATION.to_string(),
        target_type: TARGET_TYPE.to_string(),
        doc_id: task_id,
        doc_no: rendered.doc_no.clone(),
        detail: serde_json::json!({
            "doc_type": "TASK",
            "source": "PDF-06",
            "checklist_count": rendered.row_count,
            "render_ms": rendered.duration_ms,
        }),
        filename: rendered.filename.clone(),
        bytes: rendered.bytes.len(),
        duration_ms: started.elapsed().as_millis() as u64,
        request_path: format!("/api/business/task/{task_id}/pdf"),
        client_ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        request_id: Uuid::now_v7(),
    }
    .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::task::model::TaskCardRow;
    use nik::prelude::chrono::{NaiveDate, TimeZone, Utc};

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, day).expect("夹具日期非法")
    }

    fn card_row() -> TaskCardRow {
        TaskCardRow {
            id: Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0100),
            task_no: "TSK-202611-000123".to_string(),
            title: "泵站阀门到货验收".to_string(),
            status: "DOING".to_string(),
            priority: "P1".to_string(),
            progress: 40,
            need_review: true,
            owner_id: Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0002),
            owner_name: "张工".to_string(),
            dept_id: None,
            source: "ASSIGNED".to_string(),
            start_date: Some(date(9)),
            due_date: Some(date(20)),
            child_count: 2,
            child_done_count: 0,
            parent_id: None,
            project_id: Some(Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0200)),
            project_code: "PRJ-2026-011".to_string(),
            project_name: "泵站改造".to_string(),
            doc_type: None,
            doc_id: None,
            created_at: Utc
                .with_ymd_and_hms(2026, 11, 8, 1, 0, 0)
                .single()
                .expect("夹具时间"),
            updated_at: Utc
                .with_ymd_and_hms(2026, 11, 8, 1, 0, 0)
                .single()
                .expect("夹具时间"),
            flow_instance_id: None,
            flow_node_key: None,
            flow_round: None,
            is_new: false,
        }
    }

    fn detail_row() -> TaskDetailRow {
        TaskDetailRow {
            card: card_row(),
            description: Some("第一行\n第二行".to_string()),
            creator_id: Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0003),
            creator_name: "李经理".to_string(),
            assigner_id: Some(Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0003)),
            assigner_name: "李经理".to_string(),
            dept_name: "设备部".to_string(),
            parent_id: None,
            parent_task_no: None,
            project_id: Some(Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0200)),
            done_at: None,
            estimate_hours: Some(16.0),
            actual_hours: None,
            cancel_reason: None,
            reject_reason: None,
            approval_instance_id: None,
            exec_mode: "HUMAN".to_string(),
            hermes_board: None,
            hermes_task_id: None,
            hermes_status: None,
            ai_summary: None,
            ai_result: None,
        }
    }

    fn member(name: &str, role: &str) -> TaskMemberRow {
        TaskMemberRow {
            user_id: Uuid::now_v7(),
            user_name: name.to_string(),
            role: role.to_string(),
            created_at: Utc
                .with_ymd_and_hms(2026, 11, 8, 2, 0, 0)
                .single()
                .expect("夹具时间"),
        }
    }

    fn child(title: &str, status: &str, due: Option<NaiveDate>) -> TaskCardRow {
        let mut row = card_row();
        row.id = Uuid::now_v7();
        row.title = title.to_string();
        row.status = status.to_string();
        row.due_date = due;
        row.owner_name = "王五".to_string();
        row
    }

    /// 优先级 / 状态：四个码值 + 未知值都有确定文案（不许 panic、不许空串）。
    #[test]
    fn enum_labels_cover_every_code() {
        assert_eq!(priority_label("P0"), "P0（最高）");
        assert_eq!(priority_label("P1"), "P1（高）");
        assert_eq!(priority_label("P2"), "P2（中）");
        assert_eq!(priority_label("P3"), "P3（低）");
        assert_eq!(priority_label("PX"), "—");

        assert_eq!(status_label("TODO"), "待开始");
        assert_eq!(status_label("DOING"), "进行中");
        assert_eq!(status_label("REVIEW"), "待验收");
        assert_eq!(status_label("DONE"), "已完成");
        assert_eq!(status_label("CANCELLED"), "已取消");
        assert_eq!(status_label("WHATEVER"), "未知");

        assert_eq!(status_kind("TODO"), "is-todo");
        assert_eq!(status_kind("DOING"), "is-doing");
        assert_eq!(status_kind("REVIEW"), "is-review");
        assert_eq!(status_kind("DONE"), "is-done");
        assert_eq!(status_kind("CANCELLED"), "is-void");
    }

    /// 日期缺口印 `—`（不编日期）。
    #[test]
    fn date_text_never_invents_value() {
        assert_eq!(date_text(Some(date(20))), "2026-11-20");
        assert_eq!(date_text(None), "—");
    }

    /// 协作人只收 `COLLABORATOR`，关注者不进这一栏。
    #[test]
    fn collaborators_exclude_followers() {
        let members = vec![
            member("王五", "COLLABORATOR"),
            member("赵六", "COLLABORATOR"),
            member("孙七", "FOLLOWER"),
        ];
        assert_eq!(collaborators_text(&members), "王五、赵六");
        assert_eq!(collaborators_text(&[]), "—");
        assert_eq!(collaborators_text(&[member("孙七", "FOLLOWER")]), "—");
    }

    /// 自建任务（无分派人）的派工人 = 创建人，不是「—」。
    #[test]
    fn self_built_task_dispatch_signed_by_creator() {
        let mut row = detail_row();
        row.assigner_id = None;
        row.assigner_name = String::new();
        assert_eq!(assigner_display(&row), "李经理");

        row.creator_name = String::new();
        assert_eq!(assigner_display(&row), "—");
    }

    /// 项目三种形态：编码+名称 / 只有名称 / 都没有。
    #[test]
    fn project_text_handles_partial_metadata() {
        let mut row = detail_row();
        assert_eq!(project_text(&row), "PRJ-2026-011 泵站改造");

        row.card.project_code = String::new();
        assert_eq!(project_text(&row), "泵站改造");

        row.card.project_name = String::new();
        assert_eq!(project_text(&row), "—");
    }

    /// 验收②：每条检查项一个条目，**已取消的子任务跳过且序号不留空洞**。
    #[test]
    fn checklist_skips_cancelled_and_keeps_sequence_dense() {
        let children = vec![
            child("核对数量", "TODO", Some(date(10))),
            child("已取消的事项", "CANCELLED", None),
            child("检查密封面", "DONE", None),
        ];
        let items = checklist_of(&children);
        assert_eq!(items.len(), 2, "CANCELLED 不进检查项");
        assert_eq!(items[0].seq, "1");
        assert_eq!(items[1].seq, "2", "跳过取消项后序号必须连续");
        assert_eq!(items[0].due, "2026-11-10");
        assert_eq!(items[1].due, "—");
        assert_eq!(items[0].title, "核对数量");
        assert_eq!(items[0].owner, "王五");
        assert!(checklist_of(&[]).is_empty());
    }

    /// 签字栏：4 格齐全，`need_review` 决定「完成确认」的口径。
    #[test]
    fn sign_cells_four_slots_and_review_rule() {
        let row = detail_row();
        let cells = sign_cells_of(&row);
        let labels: Vec<&str> = cells.iter().map(|cell| cell.label.as_str()).collect();
        assert_eq!(labels, vec!["派工人", "接收人", "完成确认", "日期"]);
        assert_eq!(cells[0].hint, "李经理");
        assert_eq!(cells[1].hint, "张工");
        assert_eq!(cells[2].hint, "验收人：李经理");
        assert!(cells[3].hint.is_empty(), "日期栏不预印内容");

        let mut no_review = detail_row();
        no_review.card.need_review = false;
        assert_eq!(
            sign_cells_of(&no_review)[2].hint,
            "无需验收（完成后直接归档）"
        );
    }

    /// 上下文：卡面点名的字段全部就位，描述原文不做任何改写。
    #[test]
    fn context_carries_every_card_field() {
        let row = detail_row();
        let members = vec![member("王五", "COLLABORATOR"), member("孙七", "FOLLOWER")];
        let children = vec![child("核对数量", "TODO", Some(date(10)))];
        let ctx = build_context(&row, &members, &children, "2026-11-08 22:30".to_string());

        assert_eq!(ctx.doc_no, "TSK-202611-000123");
        assert_eq!(ctx.subject, "泵站阀门到货验收");
        assert_eq!(ctx.doc_title, "工 作 任 务 派 工 单");
        assert_eq!(ctx.doc_label, "派工单");
        assert_eq!(ctx.status, "进行中");
        assert_eq!(ctx.status_kind, "is-doing");
        // 12 个信息字段 → 6 行 × 2 组（每行 2 组：姓名字段与项目名较长，3 组会把值挤成两行）
        assert_eq!(ctx.meta_rows.len(), 6);
        assert!(ctx.meta_rows.iter().all(|row| row.len() == 2));
        let flat: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        let value_of = |label: &str| {
            flat.iter()
                .find(|(key, _)| key == label)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        assert_eq!(value_of("任务号"), "TSK-202611-000123");
        assert_eq!(value_of("优先级"), "P1（高）");
        assert_eq!(value_of("截止日期"), "2026-11-20");
        assert_eq!(value_of("负责人"), "张工");
        assert_eq!(value_of("协作人"), "王五");
        assert_eq!(value_of("所属项目"), "PRJ-2026-011 泵站改造");
        assert_eq!(value_of("状态"), "进行中");
        assert_eq!(value_of("创建人"), "李经理");
        assert_eq!(value_of("分派人"), "李经理");
        assert_eq!(value_of("计划开始"), "2026-11-09");
        assert_eq!(value_of("预估工时"), "16.0 小时");
        assert_eq!(value_of("部门"), "设备部");

        // 描述原文（换行保留）
        assert_eq!(ctx.description, "第一行\n第二行");
        assert!(ctx.has_description);
        assert!(ctx.has_checklist);
        assert_eq!(ctx.checklist_count, "1");
        assert_eq!(ctx.sign_cells.len(), 4);
        assert_eq!(ctx.printed_at, "2026-11-08 22:30");

        // 渲染一遍，确认模板变量与上下文对得上（模板语法错在这里就会炸）
        let html = template::render_task(&ctx).expect("派工单渲染必须成功");
        assert!(html.contains("工 作 任 务 派 工 单"));
        assert!(html.contains("第一行\n第二行"));
    }

    /// 无描述 / 无子任务时两个「有才出现」的开关都关掉。
    #[test]
    fn context_flags_empty_description_and_checklist() {
        let mut row = detail_row();
        row.description = None;
        let ctx = build_context(&row, &[], &[], "2026-11-08 22:30".to_string());
        assert!(!ctx.has_description);
        assert!(!ctx.has_checklist);
        assert_eq!(ctx.checklist_count, "0");
        assert_eq!(
            ctx.meta_rows.iter().flatten().count(),
            12,
            "没检查项也要印完整的任务信息"
        );
    }

    /// 非法 id → 400 + 10001（**先判格式再查库**，绝不静默当「没传」）。
    #[test]
    fn invalid_id_is_400_before_any_query() {
        // load_context 第一步就是 parse_uuid_field，因此不需要连库
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let principal = ScopedPrincipal::for_test(
            Uuid::now_v7(),
            None,
            true,
            crate::business::shared::scope::ScopeKind::All,
            Vec::new(),
            false,
        );
        for raw in ["", "   ", "not-a-uuid", "12345"] {
            let error = runtime
                .block_on(load_context(&principal, raw))
                .expect_err("非法 id 必须报错");
            assert_eq!(error.status, 400, "raw={raw:?}");
            assert_eq!(error.code, error_code::PARAM_INVALID, "raw={raw:?}");
        }
    }
}

// ==================== 真库用例（ERP_DB_URL / TASK_DB_URL；未设置则 SKIP） ====================

/// 真库用例：派工单的取数口径与可见性。
///
/// # 跑法
///
/// ```bash
/// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<已迁移的测试库>'
/// cargo test --lib business::task::task_pdf -- --test-threads=1
/// ```
///
/// # 夹具口径
///
/// 独占命名空间 `0f62…`（PDF-06），进 / 出都按这一组 id 清；不扫单号前缀
/// 之外的任何行。项目用 `biz_project` 真行（`project_code / project_name`
/// 从 JOIN 取，不塞常量），子任务按 `parent_id` 挂在本任务下。
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::business::shared::scope::ScopeKind;

    const OWNER: Uuid = Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0002);
    const CREATOR: Uuid = Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0003);
    const STRANGER: Uuid = Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0004);
    /// 关注者（`FOLLOWER`）：能看见任务，但**不该进「协作人」栏**。
    /// 与 STRANGER 分开是必须的 —— 参与关系（含关注者）本身就能让任务可见，
    /// 若拿 STRANGER 当关注者，`stranger_gets_404_not_403` 永远看不到 404。
    const FOLLOWER: Uuid = Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0005);
    const PROJECT: Uuid = Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0200);
    const TASK: Uuid = Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0100);
    const CHILD_A: Uuid = Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0101);
    const CHILD_B: Uuid = Uuid::from_u128(0x0f62_0000_0000_4000_8000_0000_0000_0102);

    const TASK_NO: &str = "PDF06-TSK-000001";
    const LONG_LINE: &str = "第 001 行：现场检查记录（PDF-06 长描述夹具）";

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL").or_else(|_| std::env::var("TASK_DB_URL"))
                else {
                    println!("SKIP: 未设置 ERP_DB_URL / TASK_DB_URL（不连真实数据库）");
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
            "DELETE FROM biz_task_member WHERE task_id = '0f620000-0000-4000-8000-000000000100'::uuid",
            "UPDATE biz_task SET parent_id = NULL WHERE creator_id = '0f620000-0000-4000-8000-000000000003'::uuid",
            "DELETE FROM biz_task WHERE task_no LIKE 'PDF06-%'",
            "DELETE FROM biz_project WHERE id = '0f620000-0000-4000-8000-000000000200'::uuid",
            "DELETE FROM operation_logs WHERE target_id = '0f620000-0000-4000-8000-000000000100'",
            "DELETE FROM sys_user WHERE username LIKE 'pdf06.%'",
        ] {
            exec(sql, vec![], "清理 PDF-06 任务夹具").await;
        }
    }

    /// 一次性造齐：3 个用户 + 1 个项目 + 1 个任务（长描述）+ 2 个子任务（一个已取消）+ 协作人 / 关注者各一。
    async fn seed() {
        purge().await;
        for (id, username) in [
            (OWNER, "pdf06.owner"),
            (CREATOR, "pdf06.creator"),
            (STRANGER, "pdf06.stranger"),
            (FOLLOWER, "pdf06.follower"),
        ] {
            exec(
                "INSERT INTO sys_user (id, username, real_name, password) \
                 VALUES (?::uuid, ?, ?, 'pdf06-not-a-real-hash')",
                vec![
                    Value::from(&id),
                    Value::Text(username.to_string()),
                    Value::Text(username.replace("pdf06.", "PDF06 ")),
                ],
                "夹具用户",
            )
            .await;
        }
        exec(
            "INSERT INTO biz_project (id, project_no, project_name, status) \
             VALUES (?::uuid, 'PRJP06-T', 'PDF-06 派工单夹具项目', 'ACTIVE')",
            vec![Value::from(&PROJECT)],
            "夹具项目",
        )
        .await;

        let long_description = (1..=80)
            .map(|i| format!("第 {i:03} 行：现场检查记录（PDF-06 长描述夹具）"))
            .collect::<Vec<_>>()
            .join("\n");
        exec(
            "INSERT INTO biz_task (id, task_no, title, description, status, priority, progress, \
                                   need_review, owner_id, creator_id, assigner_id, project_id, \
                                   start_date, due_date, estimate_hours) \
             VALUES (?::uuid, ?, 'PDF06 派工单取数用例', ?, 'DOING', 'P1', 30, true, \
                     ?::uuid, ?::uuid, ?::uuid, ?::uuid, '2026-11-09', '2026-11-20', 16.0)",
            vec![
                Value::from(&TASK),
                Value::Text(TASK_NO.to_string()),
                Value::Text(long_description),
                Value::from(&OWNER),
                Value::from(&CREATOR),
                Value::from(&CREATOR),
                Value::from(&PROJECT),
            ],
            "夹具任务",
        )
        .await;

        // 子任务 A（正常，**负责人是 STRANGER** —— 用来证明「检查项不按数据范围过滤」）
        // + 子任务 B（已取消 → 不该进检查项）
        for (id, no, parent_title, status, owner) in [
            (
                CHILD_A,
                "PDF06-TSK-000101",
                "核对到货数量",
                "TODO",
                STRANGER,
            ),
            (
                CHILD_B,
                "PDF06-TSK-000102",
                "已取消的事项",
                "CANCELLED",
                OWNER,
            ),
        ] {
            exec(
                "INSERT INTO biz_task (id, task_no, title, status, priority, progress, \
                                       need_review, owner_id, creator_id, parent_id, due_date) \
                 VALUES (?::uuid, ?, ?, ?, 'P1', 0, false, ?::uuid, ?::uuid, ?::uuid, '2026-11-12')",
                vec![
                    Value::from(&id),
                    Value::Text(no.to_string()),
                    Value::Text(parent_title.to_string()),
                    Value::Text(status.to_string()),
                    Value::from(&owner),
                    Value::from(&CREATOR),
                    Value::from(&TASK),
                ],
                "夹具子任务",
            )
            .await;
        }

        for (user, role) in [(OWNER, "COLLABORATOR"), (FOLLOWER, "FOLLOWER")] {
            exec(
                "INSERT INTO biz_task_member (task_id, user_id, role) VALUES (?::uuid, ?::uuid, ?)",
                vec![
                    Value::from(&TASK),
                    Value::from(&user),
                    Value::Text(role.to_string()),
                ],
                "夹具协作人",
            )
            .await;
        }
    }

    fn principal_all(user: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(user, None, false, ScopeKind::All, Vec::new(), false)
    }

    fn principal_self(user: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::for_test(user, None, false, ScopeKind::SelfOnly, Vec::new(), true)
    }

    /// 取数口径：任务字段 / 项目 JOIN / 协作人过滤 / 检查项跳过已取消 / 长描述完整。
    #[tokio::test]
    async fn context_from_real_database() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        seed().await;

        let ctx = load_context(&principal_all(OWNER), &TASK.to_string())
            .await
            .expect("负责人必须能导出");

        assert_eq!(ctx.doc_no, TASK_NO);
        assert_eq!(ctx.subject, "PDF06 派工单取数用例");
        assert_eq!(ctx.status, "进行中");
        assert_eq!(ctx.status_kind, "is-doing");
        assert_eq!(ctx.checklist_count, "1", "已取消的子任务不进检查项");
        assert_eq!(ctx.checklist[0].title, "核对到货数量");
        assert_eq!(ctx.checklist[0].due, "2026-11-12");
        // 检查项**不按数据范围过滤**：子任务负责人不在我的范围里也必须印出来
        assert_eq!(
            ctx.checklist[0].owner, "PDF06 stranger",
            "别人的检查项必须印在这张单上（否则接收人照残单干活）"
        );

        let flat: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        let value_of = |label: &str| {
            flat.iter()
                .find(|(key, _)| key == label)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        assert_eq!(value_of("优先级"), "P1（高）");
        assert_eq!(value_of("截止日期"), "2026-11-20");
        assert_eq!(value_of("所属项目"), "PRJP06-T PDF-06 派工单夹具项目");
        assert_eq!(value_of("预估工时"), "16.0 小时");
        // 协作人只有 OWNER（FOLLOWER 是关注者，不进这一栏）
        assert_eq!(value_of("协作人"), "PDF06 owner");

        // 长描述：80 行一行不少（验收①③的数据侧）
        let lines: Vec<&str> = ctx.description.lines().collect();
        assert_eq!(lines.len(), 80);
        assert_eq!(lines[0], LONG_LINE);
        assert!(lines[79].starts_with("第 080 行"));

        // 渲染一遍：模板拿到真库数据也是可渲染的
        let html = template::render_task(&ctx).expect("真库上下文必须能渲染");
        assert!(html.contains("PDF06 派工单取数用例"));
        assert!(html.contains("核对到货数量"));
        assert!(!html.contains("已取消的事项"), "已取消的子任务不许印");
    }

    /// 可见性：无关的人（自建任务也不参与）拿到 **404 + 63001**，不是 403。
    #[tokio::test]
    async fn stranger_gets_404_not_403() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        seed().await;

        let error = load_context(&principal_self(STRANGER), &TASK.to_string())
            .await
            .expect_err("无关用户必须被拦下");
        assert_eq!(error.status, 404, "看不见一律 404（不暴露「存在但无权」）");
        assert_eq!(error.code, error_code::TASK_NOT_FOUND);

        // 协作人（本用例里是关注者也不行）看得见：参与关系进可见性谓词
        let ok = load_context(&principal_self(OWNER), &TASK.to_string()).await;
        assert!(ok.is_ok(), "负责人必须可见");

        // 不存在的任务 → 404（同一码）
        let error = load_context(
            &principal_all(OWNER),
            "0f620000-0000-4000-8000-0000000009ff",
        )
        .await
        .expect_err("不存在的任务必须 404");
        assert_eq!(error.code, error_code::TASK_NOT_FOUND);
    }

    /// 审计写库：`source = PDF-06`、`target_type = biz_task`、单号与条数就位。
    #[tokio::test]
    async fn export_log_row_is_written() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        seed().await;

        doc_pdf_model::ExportLogOwned {
            user_id: Some(OWNER),
            username: Some("pdf06.owner".to_string()),
            operation: EXPORT_OPERATION.to_string(),
            target_type: TARGET_TYPE.to_string(),
            doc_id: TASK,
            doc_no: TASK_NO.to_string(),
            detail: serde_json::json!({
                "doc_type": "TASK",
                "source": "PDF-06",
                "checklist_count": 1usize,
                "render_ms": 12u64,
            }),
            filename: "派工单-PDF06-TSK-000001.pdf".to_string(),
            bytes: 2048,
            duration_ms: 15,
            request_path: format!("/api/business/task/{TASK}/pdf"),
            client_ip: None,
            user_agent: Some("pdf06-test".to_string()),
            request_id: Uuid::now_v7(),
        }
        .write()
        .await
        .expect("审计行必须写成功");

        let rows = db()
            .find_with_params(
                "SELECT operation, target_type, detail->>'source' AS source, \
                        detail->>'doc_no' AS doc_no, detail->>'checklist_count' AS cnt \
                   FROM operation_logs WHERE target_id = ?",
                vec![Value::Text(TASK.to_string())],
            )
            .await
            .expect("审计行必须能查回来");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(
            row.get_typed::<String>("operation").expect("operation"),
            EXPORT_OPERATION
        );
        assert_eq!(
            row.get_typed::<String>("target_type").expect("target_type"),
            "biz_task"
        );
        assert_eq!(row.get_typed::<String>("source").expect("source"), "PDF-06");
        assert_eq!(row.get_typed::<String>("doc_no").expect("doc_no"), TASK_NO);
        assert_eq!(row.get_typed::<String>("cnt").expect("cnt"), "1");
    }
}
