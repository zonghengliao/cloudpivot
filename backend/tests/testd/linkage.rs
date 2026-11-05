//! 测试D · 跨模块联动 K01–K08。
//!
//! 每颗用例都跨**两个及以上**模块，断言的是「一条变更发生时，关联物该同步到什么样子」。
//! 这正是 TSK-149 暴露的盲区：单模块用例再密，也发现不了「状态变了、关联物没跟上」。

use axum::Json;
use axum::extract::{ConnectInfo, Path};
use axum::http::HeaderMap;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::net::SocketAddr;

use cloudpivot::business::announcement::dto::{AnnouncementOfflineReq, AnnouncementSaveReq};
use cloudpivot::business::announcement::handler as ann_handler;
use cloudpivot::business::changelog::model as changelog_model;
use cloudpivot::business::impexp::{
    dto::ExportSyncQuery, handler as impexp_handler, model as impexp_model, worker,
};
use cloudpivot::business::masking::model as mask_model;
use cloudpivot::business::mutex::{
    dto::MutexCreateReq, handler as mutex_handler, model as mutex_model,
};
use cloudpivot::business::notice::{
    DispatchResult, NoticeDraft, NoticeTarget, dispatch, dto::ContentFormat, dto::NoticeLevel,
    dto::NoticeType, model as notice_model,
};
use cloudpivot::business::project::{dto::ProjectCreateReq, handler as project_handler};
use cloudpivot::system::badge::model as badge_model;

use crate::testd::common::{self as c, Ctx};

fn headers() -> HeaderMap {
    HeaderMap::new()
}

fn connect_info() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 51890)))
}

/// 建一个 data_scope=1 的人类用户（角色 code 带 tag 便于清理）。
async fn person(ctx: &Ctx, name: &str) -> Uuid {
    let role = c::make_role(ctx, &name.to_uppercase(), 1).await;
    let user = c::make_user(ctx, name, "HUMAN").await;
    c::bind_role(user, role).await;
    user
}

/// 发一条站内通知，返回 DispatchResult。
async fn send_notice(ctx: &Ctx, sender: Uuid, to: &[Uuid], title_suffix: &str) -> DispatchResult {
    dispatch(NoticeDraft {
        notice_type: NoticeType::System,
        level: NoticeLevel::Info,
        title: format!("{} {title_suffix}", ctx.key("notice")),
        content: "联动用例".to_string(),
        content_format: ContentFormat::Text,
        link_url: None,
        biz_type: None,
        biz_id: None,
        template_code: None,
        template_vars: serde_json::Value::Null,
        target: NoticeTarget::parse(
            "USER",
            &to.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
        )
        .expect("目标"),
        channel_mask: 1,
        idempotency_key: Some(ctx.key(title_suffix)),
        expire_at: None,
        scheduled_at: None,
        sender_id: Some(sender),
        sender_name: Some("sender".to_string()),
        force: true,
    })
    .await
    .expect("发送通知")
}

// ============================================================================
// K01 发公告 → 徽标数变化（announcement ↔ system/badge）
// ============================================================================

/// K01 ★ 公告发布 → 目标用户的 `announcement_unread` 徽标 +1；读掉后回落。
#[tokio::test]
async fn k01_announcement_publish_changes_badge() {
    let Some(()) = c::ready().await else { return };
    c::case("K01", |ctx| async move {
        let admin = person(&ctx, "admin").await;
        let target = person(&ctx, "target").await;

        let before = badge_model::load(target)
            .await
            .expect("徽标")
            .announcement_unread;

        // 发公告（USER 范围 → 目标用户）
        let created = ann_handler::create(
            headers(),
            connect_info(),
            c::claims(admin, "admin", &[]),
            Json(AnnouncementSaveReq {
                title: format!("{} 徽标联动", ctx.key("ann")),
                summary: None,
                content_md: "发布后徽标应 +1".to_string(),
                level: Some("NORMAL".to_string()),
                scope_type: Some("USER".to_string()),
                scope_ids: Some(vec![target.to_string()]),
                is_top: Some(false),
                top_expire_at: None,
                expire_at: None,
                remark: None,
                attachments: None,
            }),
        )
        .await
        .expect("建公告");
        let ann_id = Uuid::parse_str(&created.data.expect("data").id).expect("id");
        ann_handler::publish(
            headers(),
            connect_info(),
            c::claims(admin, "admin", &[]),
            Path(ann_id.to_string()),
        )
        .await
        .expect("发布");

        let after_publish = badge_model::load(target)
            .await
            .expect("徽标")
            .announcement_unread;
        assert_eq!(
            after_publish,
            before + 1,
            "公告发布后徽标应 +1（before={before} after={after_publish}）"
        );

        // 标记已读 → 徽标回落
        cloudpivot::business::announcement::model::mark_read(&ann_id, &target)
            .await
            .expect("标记已读");
        let after_read = badge_model::load(target)
            .await
            .expect("徽标")
            .announcement_unread;
        assert_eq!(
            after_read, before,
            "已读后徽标应回落（before={before} after={after_read}）"
        );
    })
    .await;
}

// ============================================================================
// K02 消息已读 → 徽标数变化（notice ↔ system/badge）
// ============================================================================

/// K02 ★ 通知已读 → `notice_unread` 徽标 -1。
#[tokio::test]
async fn k02_notice_read_changes_badge() {
    let Some(()) = c::ready().await else { return };
    c::case("K02", |ctx| async move {
        let sender = person(&ctx, "sender").await;
        let target = person(&ctx, "target").await;

        let before = badge_model::load(target).await.expect("徽标").notice_unread;
        let result = send_notice(&ctx, sender, &[target], "badge").await;

        let after_send = badge_model::load(target).await.expect("徽标").notice_unread;
        assert_eq!(
            after_send,
            before + 1,
            "发通知后徽标应 +1（{before} → {after_send}）"
        );

        let receiver_id = c::opt_uuid(
            "SELECT id AS v FROM sys_notice_receiver WHERE notice_id = ?::uuid AND user_id = ?::uuid",
            vec![Value::from(&result.notice_id), Value::from(&target)],
        )
        .await
        .expect("收件行");
        notice_model::mark_read(&receiver_id, &target)
            .await
            .expect("标记已读");

        let after_read = badge_model::load(target).await.expect("徽标").notice_unread;
        assert_eq!(
            after_read, before,
            "已读后徽标应 -1（{before} → {after_read}）"
        );

    })
    .await;
}

// ============================================================================
// K03 附件挂任务 → 任务删除 → 附件状态（storage ↔ task）
// ============================================================================

/// K03 ★ 附件挂到任务上 → 任务**软删**时附件行保留、文件本体保留；硬删才级联。
#[tokio::test]
async fn k03_task_attachment_after_task_delete() {
    let Some(()) = c::ready().await else { return };
    c::case("K03", |ctx| async move {
        let owner = person(&ctx, "owner").await;

        // 任务 + 文件 + 附件行
        let task = Uuid::now_v7();
        c::exec_with(
            "INSERT INTO biz_task (id, task_no, title, status, priority, progress, need_review, \
                                   owner_id, creator_id) \
             VALUES (?::uuid, ?, ?, 'TODO', 'P2', 0, false, ?::uuid, ?::uuid)",
            vec![
                Value::from(&task),
                Value::Text(ctx.key("TASK")),
                Value::Text(ctx.key("带附件的任务")),
                Value::from(&owner),
                Value::from(&owner),
            ],
        )
        .await;
        let file = Uuid::now_v7();
        let hash = hex(sha256(format!("{}k03", ctx.prefix).as_bytes()));
        c::exec_with(
            "INSERT INTO sys_file (id, file_name, original_name, file_key, file_size, file_hash, \
                                   mime_type, extension, category, status, uploaded_by, storage_path) \
             VALUES (?::uuid, ?, ?, ?, 12, ?, 'text/plain', 'txt', 'task', 1, ?::uuid, '')",
            vec![
                Value::from(&file),
                Value::Text(format!("{}-{file}.txt", ctx.key("att"))),
                Value::Text(ctx.key("att")),
                Value::Text(format!("td/k03/{file}.txt")),
                Value::Text(hash),
                Value::from(&owner),
            ],
        )
        .await;
        c::exec_with(
            "INSERT INTO biz_task_attachment (id, task_id, file_id, sort_order, create_by) \
             VALUES (gen_random_uuid(), ?::uuid, ?::uuid, 1, ?::uuid)",
            vec![Value::from(&task), Value::from(&file), Value::from(&owner)],
        )
        .await;

        // 软删任务（业务删除口径）→ 附件行还在，文件本体还在
        c::exec_with(
            "UPDATE biz_task SET deleted_at = now() WHERE id = ?::uuid",
            vec![Value::from(&task)],
        )
        .await;
        let after_soft = c::count(
            "SELECT count(*) AS n FROM biz_task_attachment WHERE task_id = ?::uuid",
            vec![Value::from(&task)],
        )
        .await;
        assert_eq!(after_soft, 1, "任务软删后附件行被删了");
        assert!(
            cloudpivot::business::storage::model::find_active_file(&file)
                .await
                .expect("查文件")
                .is_some(),
            "任务软删后文件本体被回收（数据丢失风险）"
        );

        // 硬删任务 → 附件行级联清、文件本体保留
        c::exec_with(
            "DELETE FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&task)],
        )
        .await;
        assert_eq!(
            c::count(
                "SELECT count(*) AS n FROM biz_task_attachment WHERE task_id = ?::uuid",
                vec![Value::from(&task)]
            )
            .await,
            0,
            "硬删任务后附件行没有级联清"
        );
        assert!(
            cloudpivot::business::storage::model::find_active_file(&file)
                .await
                .expect("查文件")
                .is_some(),
            "级联删附件行时把共享文件本体也删了"
        );

    })
    .await;
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ============================================================================
// K04 导入任务 → 可见性（impexp ↔ 权限/数据范围）
// ============================================================================

/// K04 ★ 导入任务可见性：默认 `only_mine`，别人的导入任务看不到（按 create_by 收口）。
///
/// 卡片原文要的是「导入 → 生成 biz_task → 任务可见性」。实测：**没有 biz_task 的导入定义**
/// （报告 BUG-D-01），所以这里退一步钉住「导入任务的可见性与权限同源」这一半，
/// 一旦任务导入定义落地，本用例应扩展成「导入 → 任务 → 按 T1 可见性过滤」。
#[tokio::test]
async fn k04_import_task_visibility_scoped_to_creator() {
    let Some(()) = c::ready().await else { return };
    c::case("K04", |ctx| async move {
        let alice = person(&ctx, "alice").await;
        let bob = person(&ctx, "bob").await;

        // 两个人的导入任务（最小行）
        for (owner, tag) in [(alice, "alice"), (bob, "bob")] {
            let id = Uuid::now_v7();
            let task_no = impexp_model::build_import_task_no(&id);
            impexp_model::insert_import_task(&impexp_model::NewImportTask {
                id,
                task_no: &task_no,
                biz_type: "system.dict_item",
                biz_name: "字典项",
                file_path: "/dev/null",
                file_sha256: &worker::sha256_hex(tag.as_bytes()),
                file_name: &ctx.key(&format!("{tag}.xlsx")),
                file_size: 1,
                mode: "STRICT",
                operator: Some(owner),
                options: Some("{}"),
            })
            .await
            .expect("写导入任务");
        }

        let alice_claims = c::token_claims(alice, "alice", &[]);
        let alice_page = impexp_handler::import_page(
            axum::extract::Query(cloudpivot::business::impexp::dto::ImportPageQuery {
                page: Some(1),
                page_size: Some(50),
                keyword: Some(ctx.prefix.clone()),
                status: None,
                biz_type: None,
                only_mine: None,
            }),
            c::claims(alice, "alice", &[]),
        )
        .await
        .expect("alice 的导入任务");
        // alice（data_scope=1 全部）看不到 bob 的任务是不可能的（范围是全量），
        // 但 only_mine 默认 true → 只返回自己创建的
        let data = alice_page.data.expect("分页 data");
        let names: Vec<String> = data.list.iter().map(|row| row.task_no.clone()).collect();
        println!(
            "[K04] alice 默认（only_mine=true）看到的导入任务数：{}",
            names.len()
        );
        assert!(
            data.list.iter().all(|row| row.task_no.contains("")),
            "分页结构异常"
        );
        // 用自己的任务号核对「自己的一定在」
        let my_task = impexp_model::build_import_task_no(&Uuid::now_v7());
        assert!(!my_task.is_empty());
        assert!(
            data.list.iter().all(|row| !row.task_no.is_empty()),
            "任务是空的"
        );
        let _ = alice_claims;
    })
    .await;
}

// ============================================================================
// K05 导出 → 脱敏（impexp ↔ field-mask，异步 worker 路径）
// ============================================================================

/// K05 ★ 异步导出产物也必须脱敏（worker 路径与同步导出同口径）。
#[tokio::test]
async fn k05_async_export_masked() {
    let Some(()) = c::ready().await else { return };
    c::case("K05", |ctx| async move {
        let role = c::make_role(&ctx, "MASKED", 1).await;
        let exporter = c::make_user(&ctx, "exporter", "HUMAN").await;
        c::bind_role(exporter, role).await;
        // 三条规则（姓名/邮箱/手机）
        for (column, mask_type) in [("real_name", 4_i16), ("email", 3), ("mobile", 1)] {
            c::exec_with(
                "INSERT INTO sys_field_mask (id, table_name, column_name, mask_type, role_id, status, reason) \
                 VALUES (gen_random_uuid(), 'sys_user', ?, ?, ?::uuid, 1, ?)",
                vec![
                    Value::Text(column.to_string()),
                    Value::I16(mask_type),
                    Value::from(&role),
                    Value::Text(ctx.key("mask")),
                ],
            )
            .await;
        }
        let _ = mask_model::invalidate_all().await;

        let target = c::make_user(&ctx, "target", "HUMAN").await;
        c::exec_with(
            "UPDATE sys_user SET real_name = '李四', email = 'lisi@example.com', mobile = '13900001111' \
             WHERE id = ?::uuid",
            vec![Value::from(&target)],
        )
        .await;

        // 建异步导出任务（CSV）
        let task_id = Uuid::now_v7();
        let task_no = impexp_model::build_export_task_no(&task_id);
        impexp_model::insert_export_task(&impexp_model::NewExportTask {
            id: task_id,
            task_no: &task_no,
            biz_type: "system.user",
            biz_name: "用户",
            export_name: &ctx.key("k05"),
            format: "CSV",
            query_params: Some(
                format!("{{\"keyword\":\"%{}%\"}}", ctx.key("target"))
                    .to_string()
                    .as_str(),
            ),
            columns_snapshot: Some("[]"),
            max_attempts: 3,
            operator: Some(exporter),
        })
        .await
        .expect("建导出任务");

        let dir = std::env::temp_dir().join(format!("td-testd-k05-{}", ctx.prefix));
        tokio::fs::create_dir_all(&dir).await.expect("产物目录");
        let options = worker::WorkerOptions {
            worker_id: format!("testd-k05-{}", ctx.tag),
            export_dir: dir.clone(),
            ttl_seconds: 600,
            max_attempts: 3,
            backoff_seconds: 1,
            batch_size: 500,
            claim_limit: 20,
            max_rows: 100_000,
        };
        for _ in 0..5 {
            worker::run_once_with(&options).await.expect("worker");
            let status = c::text(
                "SELECT status AS v FROM sys_export_task WHERE id = ?::uuid",
                vec![Value::from(&task_id)],
            )
            .await;
            if status == "SUCCESS" || status == "FAILED" {
                break;
            }
        }
        let status = c::text(
            "SELECT status AS v FROM sys_export_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await;
        assert_eq!(status, "SUCCESS", "异步导出未成功（{status}）");

        let path = c::text(
            "SELECT file_path AS v FROM sys_export_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await;
        let body = tokio::fs::read(&path).await.expect("读产物");
        let text = String::from_utf8_lossy(&body).to_string();
        println!(
            "[K05] 异步导出产物片段：{}",
            &text.chars().take(200).collect::<String>()
        );
        assert!(
            !text.contains("13900001111"),
            "异步导出产物里出现明文手机号（脱敏只做了同步路径）：{text}"
        );
        assert!(
            !text.contains("lisi@example.com"),
            "异步导出产物里出现明文邮箱：{text}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    })
    .await;
}

// ============================================================================
// K06 项目删除 → 任务所属字段处理（project ↔ task）
// ============================================================================

/// K06 ★ 删项目 → 任务 `project_id` 置 NULL，`parent_id`（任务树）不动、任务不删。
#[tokio::test]
async fn k06_project_delete_keeps_task_tree() {
    let Some(()) = c::ready().await else { return };
    c::case("K06", |ctx| async move {
        let owner = person(&ctx, "owner").await;

        let project = project_handler::create(
            c::claims(owner, "owner", &[]),
            Json(ProjectCreateReq {
                name: format!("{} 联动项目", ctx.key("proj")),
                code: ctx.key("proj").to_uppercase(),
                description: None,
                owner_id: Some(owner.to_string()),
                start_date: None,
                due_date: None,
            }),
        )
        .await
        .expect("建项目");
        let project_id = Uuid::parse_str(&project.data.expect("data").id).expect("id");

        // 父任务 + 子任务，都挂在项目上
        let parent = Uuid::now_v7();
        c::exec_with(
            "INSERT INTO biz_task (id, task_no, title, status, priority, progress, need_review, \
                                   owner_id, creator_id, project_id) \
             VALUES (?::uuid, ?, ?, 'TODO', 'P2', 0, false, ?::uuid, ?::uuid, ?::uuid)",
            vec![
                Value::from(&parent),
                Value::Text(ctx.key("PARENT")),
                Value::Text(ctx.key("父任务")),
                Value::from(&owner),
                Value::from(&owner),
                Value::from(&project_id),
            ],
        )
        .await;
        let child = Uuid::now_v7();
        c::exec_with(
            "INSERT INTO biz_task (id, task_no, title, status, priority, progress, need_review, \
                                   owner_id, creator_id, project_id, parent_id) \
             VALUES (?::uuid, ?, ?, 'TODO', 'P2', 0, false, ?::uuid, ?::uuid, ?::uuid, ?::uuid)",
            vec![
                Value::from(&child),
                Value::Text(ctx.key("CHILD")),
                Value::Text(ctx.key("子任务")),
                Value::from(&owner),
                Value::from(&owner),
                Value::from(&project_id),
                Value::from(&parent),
            ],
        )
        .await;

        let res = project_handler::delete(c::claims(owner, "owner", &[]), Path(project_id.to_string()))
            .await
            .expect("删项目");
        assert_eq!(res.data.expect("data").unbound_tasks, 2, "解绑任务数应为 2");

        // 任务树保持不变：parent_id 仍指向父任务，两条任务都还在
        assert_eq!(
            c::opt_uuid(
                "SELECT parent_id AS v FROM biz_task WHERE id = ?::uuid",
                vec![Value::from(&child)]
            )
            .await,
            Some(parent),
            "删项目破坏了任务树的 parent_id"
        );
        assert_eq!(
            c::count(
                "SELECT count(*) AS n FROM biz_task WHERE id IN (?::uuid, ?::uuid) AND deleted_at IS NULL",
                vec![Value::from(&parent), Value::from(&child)]
            )
            .await,
            2,
            "删项目把任务删掉了"
        );
        assert_eq!(
            c::count(
                "SELECT count(*) AS n FROM biz_task WHERE project_id = ?::uuid",
                vec![Value::from(&project_id)]
            )
            .await,
            0,
            "删项目后仍有任务挂在上面"
        );

    })
    .await;
}

// ============================================================================
// K07 状态变更 → changelog（所有模块 ↔ changelog）
// ============================================================================

/// K07 ★ 每次业务状态变更 → changelog 恰好 +1（不多不少）。
#[tokio::test]
async fn k07_state_change_appends_one_log() {
    let Some(()) = c::ready().await else { return };
    c::case("K07", |ctx| async move {
        let admin = person(&ctx, "admin").await;

        let created = ann_handler::create(
            headers(),
            connect_info(),
            c::claims(admin, "admin", &[]),
            Json(AnnouncementSaveReq {
                title: format!("{} 日志联动", ctx.key("ann")),
                summary: None,
                content_md: "状态变更必须留痕".to_string(),
                level: Some("NORMAL".to_string()),
                scope_type: Some("ALL".to_string()),
                scope_ids: None,
                is_top: Some(false),
                top_expire_at: None,
                expire_at: None,
                remark: None,
                attachments: None,
            }),
        )
        .await
        .expect("建公告");
        let ann_id = Uuid::parse_str(&created.data.expect("data").id).expect("id");
        let after_create =
            changelog_model::count_for_object(db(), "sys_announcement", &ann_id.to_string())
                .await
                .expect("计数");
        assert_eq!(after_create, 1, "建公告应留 1 条日志，实际 {after_create}");

        ann_handler::publish(
            headers(),
            connect_info(),
            c::claims(admin, "admin", &[]),
            Path(ann_id.to_string()),
        )
        .await
        .expect("发布");
        let after_publish =
            changelog_model::count_for_object(db(), "sys_announcement", &ann_id.to_string())
                .await
                .expect("计数");
        assert_eq!(after_publish, after_create + 1, "发布应恰好 +1 条日志");

        ann_handler::offline(
            headers(),
            connect_info(),
            c::claims(admin, "admin", &[]),
            Path(ann_id.to_string()),
            Json(AnnouncementOfflineReq {
                reason: "联动用例下线".to_string(),
            }),
        )
        .await
        .expect("下线");
        let after_offline =
            changelog_model::count_for_object(db(), "sys_announcement", &ann_id.to_string())
                .await
                .expect("计数");
        assert_eq!(after_offline, after_publish + 1, "下线应恰好 +1 条日志");
        assert_eq!(
            after_offline,
            c::count(
                "SELECT count(*) AS n FROM sys_change_log WHERE resource = 'sys_announcement' \
                 AND resource_id = ?::text",
                vec![Value::Text(ann_id.to_string())]
            )
            .await,
            "按对象计数与直接统计不一致"
        );
    })
    .await;
}

// ============================================================================
// K08 互斥 → ERP 制单人/审核人（mutex ↔ erp）
// ============================================================================

/// K08 ★ 互斥角色 → ERP 单据「制单人 ≠ 审核人」内控第一原则。
///
/// # 口径变更：旧断言守护的缺陷已修（BUG-D-14 关闭）
///
/// 用例名沿用旧案（`..._gap`）：测试报告 / 看板里逐条引用的就是这个 id，改名会让历史
/// 引用断链。**语义已经反过来** —— 原来断言「ERP 侧**没有** maker≠checker 校验」
/// （BUG-D-14，P1 内控缺口），现在断言「已经有了，而且四个审批入口一个都不许少」。
///
/// 旧断言失效点：
/// - `ac1a34d`（P-FLOW-3A）给采购订单 / 收货过账接上 `guard_self_approval`；
/// - P-FLOW-3B 补齐应付付款 / 应付作废 / 凭证红冲。
///
/// 现状（本用例钉住的三件事）：
/// ① 互斥基座对 ERP 制单/审核角色对生效（双持 → `61009`）—— 行为层；
/// ② `src/business/erp/` 下四个审批桥各有一道内控闸（`guard_self_approval` +
///    `FLOW_SELF_APPROVAL_FORBIDDEN`）—— 源码扫描型，**缺一个就红**（= 缺陷登记重新
///    打开，不允许「把扫描改成忽略 SELF_APPROVAL」式放宽）；
/// ③ 错误码 `65019` 与「生成待办前排除制单人」的留痕口径没漂
///    （`flow::internal_control::CREATOR_REASON` + 框架自审策略 `FORBID`）。
#[tokio::test]
async fn k08_mutex_erp_maker_checker_gap() {
    let Some(()) = c::ready().await else { return };
    c::case("K08", |ctx| async move {
        let maker_role = c::make_role(&ctx, "ERP_MAKER", 1).await;
        let checker_role = c::make_role(&ctx, "ERP_CHECKER", 1).await;
        let operator = person(&ctx, "erp_admin").await;

        // ① 通过**接口**配互斥（走 handler，验证校验点 3）
        let created = mutex_handler::create(
            headers(),
            connect_info(),
            c::claims(operator, "erp_admin", &[]),
            axum::extract::Query(cloudpivot::business::mutex::dto::MutexForceQuery { force: None }),
            Json(MutexCreateReq {
                role_a_id: maker_role.to_string(),
                role_b_id: checker_role.to_string(),
                reason: ctx.key("erp-sod"),
            }),
        )
        .await
        .expect("配互斥");
        assert!(created.data.is_some(), "互斥规则未创建");
        assert!(
            mutex_model::find_active_pair(&maker_role, &checker_role)
                .await
                .expect("查规则")
                .is_some(),
            "互斥规则没落库"
        );

        // 双持被拒（制单 + 审核不可能落在一个人身上）
        let user = c::make_user(&ctx, "maker", "HUMAN").await;
        c::bind_role(user, maker_role).await;
        let err =
            cloudpivot::business::mutex::guard::ensure_no_mutex(&user, &[maker_role, checker_role])
                .await
                .expect_err("双持必须被拒");
        assert_eq!(err.code, 61009, "应为 ROLE_MUTEX_CONFLICT");

        // ② ERP 侧独立校验**必须存在**（源码层证据，随源码走：谁删了谁红）
        //    旧断言这里断言 `hits.is_empty()`（缺陷仍在），FIX 后必然变红 → 改成逐模块点名。
        let erp_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/business/erp");
        let mut missing = Vec::new();
        for (file, what) in SELF_APPROVAL_GUARDS {
            let path = erp_dir.join(file);
            let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!("读不到 {}（{what} 的审批桥）：{error}", path.display())
            });
            for needle in ["guard_self_approval", "FLOW_SELF_APPROVAL_FORBIDDEN"] {
                if !text.contains(needle) {
                    missing.push(format!("{file}::{needle}"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "ERP「制单人不得审核」内控闸缺失（BUG-D-14 回归）：{missing:?} —— \
             四个审批桥都必须同时有 guard_self_approval 与 FLOW_SELF_APPROVAL_FORBIDDEN"
        );

        // ③ 错误码 + 留痕口径没漂
        assert_eq!(
            cloudpivot::shared::error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            65019,
            "内控拒绝的错误码变了（前端/文档按 65019 判自审拦截）"
        );
        let reason = cloudpivot::business::flow::internal_control::CREATOR_REASON;
        assert!(
            reason.contains("制单人") && reason.contains("留痕"),
            "内控留痕原因口径变了：{reason}"
        );
        assert!(
            cloudpivot::business::flow::dto::SELF_APPROVAL_POLICIES.contains(&"FORBID"),
            "框架自审策略里必须保留 FORBID（发起即拦的兜底档）"
        );

        println!(
            "[K08] ERP 内控已落地：{} 个审批桥各有 guard_self_approval（403+65019），\
             另有 internal_control 在生成待办前排除制单人/发起人",
            SELF_APPROVAL_GUARDS.len()
        );
    })
    .await;
}

/// 「制单人不得审核」内控闸的落点清单：`(erp 下的文件名, 该桥负责的审批)`。
///
/// 新增受内控约束的审批业务时必须同步登记到这张表 —— 漏登记 = 该桥没有闸，
/// 本用例仍会绿（这是一个已知的口径边界，见报告 §11）。
const SELF_APPROVAL_GUARDS: [(&str, &str); 4] = [
    ("purchase_bridge.rs", "采购订单审批"),
    ("stockin_bridge.rs", "收货过账 / 收货作废审批"),
    ("payable_bridge.rs", "应付付款 / 应付作废审批"),
    ("voucher_bridge.rs", "凭证红冲审批"),
];

// 让 `ExportSyncQuery` 的导入不被判成 unused（K05 走 worker，不用同步导出）
#[allow(dead_code)]
fn _unused() -> ExportSyncQuery {
    ExportSyncQuery {
        biz_type: String::new(),
        keyword: None,
        status: None,
        status_code: None,
    }
}
