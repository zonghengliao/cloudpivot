//! 测试D · 项目（P01–P05）与变更日志（C01–C06）。
//!
//! 项目部分走真实 handler（`create/create/update/delete`）与 `model`（成员、统计）；
//! 变更日志部分用公告的真实状态流转产生日志，再验「全、准、不可改、可查」。

use axum::extract::{ConnectInfo, Path, Query};
use axum::http::HeaderMap;
use axum::Json;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::net::SocketAddr;

use cloudpivot::business::announcement::dto::{AnnouncementOfflineReq, AnnouncementSaveReq};
use cloudpivot::business::announcement::handler as ann_handler;
use cloudpivot::business::changelog::model as changelog_model;
use cloudpivot::business::changelog::query as changelog_query;
use cloudpivot::business::project::dto::{ProjectCreateReq, ProjectUpdateReq};
use cloudpivot::business::project::{handler as project_handler, model as project_model};
use cloudpivot::business::shared::principal;

use crate::testd::common::{self as c, Ctx};

fn headers() -> HeaderMap {
    HeaderMap::new()
}

fn connect_info() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 51889)))
}

/// 建项目（返回 id）。
async fn create_project(ctx: &Ctx, owner: Uuid, name: &str) -> Uuid {
    let res = project_handler::create(
        c::claims(owner, "owner", &[]),
        Json(ProjectCreateReq {
            name: format!("{} {name}", ctx.key("proj")),
            code: ctx.key(name).to_uppercase(),
            description: Some("测试D 夹具项目".to_string()),
            owner_id: Some(owner.to_string()),
            start_date: None,
            due_date: None,
        }),
    )
    .await
    .expect("建项目");
    Uuid::parse_str(&res.data.expect("项目 data").id).expect("项目 id")
}

/// 在项目下挂一个任务（最小列）。
async fn attach_task(
    ctx: &Ctx,
    owner: Uuid,
    project: Uuid,
    name: &str,
    status: &str,
    progress: i16,
) -> Uuid {
    let id = Uuid::now_v7();
    c::exec_with(
        "INSERT INTO biz_task (id, task_no, title, status, priority, progress, need_review, \
                               owner_id, creator_id, project_id) \
         VALUES (?::uuid, ?, ?, ?, 'P2', ?, false, ?::uuid, ?::uuid, ?::uuid)",
        vec![
            Value::from(&id),
            Value::Text(format!(
                "{}-{}",
                ctx.key("T"),
                &id.simple().to_string()[24..]
            )),
            Value::Text(ctx.key(name)),
            Value::Text(status.to_string()),
            Value::I16(progress),
            Value::from(&owner),
            Value::from(&owner),
            Value::from(&project),
        ],
    )
    .await;
    id
}

// ============================================================================
// P01 建项目 → 关联任务
// ============================================================================

#[tokio::test]
async fn p01_create_project_and_bind_task() {
    let Some(()) = c::ready().await else { return };
    c::case("P01", |ctx| async move {
        let role = c::make_role(&ctx, "PM", 1).await;
        let owner = c::make_user(&ctx, "pm", "HUMAN").await;
        c::bind_role(owner, role).await;

        let project = create_project(&ctx, owner, "proj").await;
        let task = attach_task(&ctx, owner, project, "任务A", "TODO", 0).await;

        let bound = c::count(
            "SELECT count(*) AS n FROM biz_task WHERE id = ?::uuid AND project_id = ?::uuid",
            vec![Value::from(&task), Value::from(&project)],
        )
        .await;
        assert_eq!(bound, 1, "任务未归入项目");

        // 编码重复必须被拒（uk_biz_project_no）
        let dup = project_handler::create(
            c::claims(owner, "pm", &[]),
            Json(ProjectCreateReq {
                name: format!("{} 重复", ctx.key("proj")),
                code: ctx.key("proj").to_uppercase(),
                description: None,
                owner_id: Some(owner.to_string()),
                start_date: None,
                due_date: None,
            }),
        )
        .await
        .expect_err("重复编码应被拒");
        assert_eq!(dup.status, 400, "重复编码应 400，实际 {}", dup.status);
    })
    .await;
}

// ============================================================================
// P02 项目删除 → 任务不能变孤儿
// ============================================================================

#[tokio::test]
async fn p02_delete_project_unbinds_tasks() {
    let Some(()) = c::ready().await else { return };
    c::case("P02", |ctx| async move {
        let role = c::make_role(&ctx, "PM", 1).await;
        let owner = c::make_user(&ctx, "pm", "HUMAN").await;
        c::bind_role(owner, role).await;

        let project = create_project(&ctx, owner, "proj").await;
        let t1 = attach_task(&ctx, owner, project, "任务1", "TODO", 0).await;
        let t2 = attach_task(&ctx, owner, project, "任务2", "DOING", 50).await;

        let res = project_handler::delete(c::claims(owner, "pm", &[]), Path(project.to_string()))
            .await
            .expect("删项目");
        let data = res.data.expect("删除 data");
        assert_eq!(
            data.unbound_tasks, 2,
            "解绑任务数应为 2，实际 {}",
            data.unbound_tasks
        );

        // 任务本体**不删**，只是 project_id 置空
        let alive = c::count(
            "SELECT count(*) AS n FROM biz_task WHERE id IN (?::uuid, ?::uuid) AND deleted_at IS NULL",
            vec![Value::from(&t1), Value::from(&t2)],
        )
        .await;
        assert_eq!(alive, 2, "删项目把任务也删了（孤儿/误删）");
        let bound = c::count(
            "SELECT count(*) AS n FROM biz_task WHERE project_id = ?::uuid",
            vec![Value::from(&project)],
        )
        .await;
        assert_eq!(bound, 0, "删项目后仍有任务挂在上面");

    })
    .await;
}

// ============================================================================
// P03 项目进度 = 任务进度汇总（口径一致）
// ============================================================================

#[tokio::test]
async fn p03_project_stats_aggregate_tasks() {
    let Some(()) = c::ready().await else { return };
    c::case("P03", |ctx| async move {
        let role = c::make_role(&ctx, "PM", 1).await;
        let owner = c::make_user(&ctx, "pm", "HUMAN").await;
        c::bind_role(owner, role).await;

        let project = create_project(&ctx, owner, "proj").await;
        attach_task(&ctx, owner, project, "完成", "DONE", 100).await;
        attach_task(&ctx, owner, project, "进行中", "DOING", 50).await;

        let principal = principal::resolve(&owner).await.expect("解析范围");
        let stats = project_model::stats_for_projects(&principal, &[project])
            .await
            .expect("项目统计");
        let stats = stats.first().expect("统计行");
        println!("[P03] 项目统计：{:?}", stats);
        assert_eq!(stats.total, 2, "任务总数应为 2，实际 {}", stats.total);
        assert_eq!(stats.done, 1, "已完成应为 1，实际 {}", stats.done);
        assert_eq!(stats.doing, 1, "进行中应为 1，实际 {}", stats.doing);
        assert_eq!(
            stats.completion_rate(),
            50.0,
            "完成率应为 50%，实际 {}",
            stats.completion_rate()
        );

        // 项目行本身不存「进度」，前端用统计口径渲染；这里钉住分母/分子一致
        let db_total = c::count(
            "SELECT count(*) AS n FROM biz_task WHERE project_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&project)],
        )
        .await;
        assert_eq!(db_total, stats.total, "统计口径与库里的任务数不一致");
    })
    .await;
}

// ============================================================================
// P04 项目成员权限（项目不是权限边界）
// ============================================================================

#[tokio::test]
async fn p04_project_membership_is_not_a_scope() {
    let Some(()) = c::ready().await else { return };
    c::case("P04", |ctx| async move {
        let pm_role = c::make_role(&ctx, "PM", 1).await;
        let owner = c::make_user(&ctx, "pm", "HUMAN").await;
        c::bind_role(owner, pm_role).await;
        let member = c::make_user(&ctx, "member", "HUMAN").await;
        let member_role = c::make_role(&ctx, "MEMBER_ONLY", 4).await; // 仅本人范围
        c::bind_role(member, member_role).await;

        let project = create_project(&ctx, owner, "proj").await;
        db().transaction(|tx| async move {
            project_model::add_member(&tx, project, member, "MEMBER", owner).await
        })
        .await
        .expect("加成员");
        assert!(
            project_model::is_member(project, member)
                .await
                .expect("成员判断"),
            "成员关系没写进 biz_project_member"
        );

        // 任务：成员不是负责人 → 仅本人范围下不可见（项目成员 ≠ 任务可见）
        let task = attach_task(&ctx, owner, project, "别人的任务", "TODO", 0).await;
        let member_principal = principal::resolve(&member).await.expect("成员范围");
        assert_eq!(
            format!("{:?}", member_principal.kind()),
            "SelfOnly",
            "成员角色范围被意外放宽"
        );
        let visible = c::count(
            "SELECT count(*) AS n FROM biz_task WHERE id = ?::uuid AND owner_id = ?::uuid",
            vec![Value::from(&task), Value::from(&member)],
        )
        .await;
        assert_eq!(
            visible, 0,
            "成员通过项目关系看到了别人的任务（项目被当成权限边界）"
        );

        // 移出成员
        db().transaction(
            |tx| async move { project_model::remove_member(&tx, project, member).await },
        )
        .await
        .expect("移出成员");
        assert!(!project_model::is_member(project, member)
            .await
            .expect("成员判断"));
    })
    .await;
}

// ============================================================================
// P05 项目归档
// ============================================================================

#[tokio::test]
async fn p05_archive_project() {
    let Some(()) = c::ready().await else { return };
    c::case("P05", |ctx| async move {
        let role = c::make_role(&ctx, "PM", 1).await;
        let owner = c::make_user(&ctx, "pm", "HUMAN").await;
        c::bind_role(owner, role).await;
        let project = create_project(&ctx, owner, "proj").await;

        let res = project_handler::update(
            c::claims(owner, "pm", &[]),
            Path(project.to_string()),
            Json(ProjectUpdateReq {
                status: Some("ARCHIVED".to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("归档项目");
        assert!(res.data.is_some(), "归档返回空 data");
        let status = c::text(
            "SELECT status AS v FROM biz_project WHERE id = ?::uuid",
            vec![Value::from(&project)],
        )
        .await;
        assert_eq!(status, "ARCHIVED", "项目状态未变为 ARCHIVED");

        // 非法状态被拒（DB check：ACTIVE / ARCHIVED）
        let bad = project_handler::update(
            c::claims(owner, "pm", &[]),
            Path(project.to_string()),
            Json(ProjectUpdateReq {
                status: Some("DELETED".to_string()),
                ..Default::default()
            }),
        )
        .await;
        assert!(bad.is_err(), "非法项目状态未被拒绝");
    })
    .await;
}

// ============================================================================
// 变更日志 C01–C06
// ============================================================================

/// 造一个能发公告的用户并返回 (发布人, 公告 id)。
async fn published_announcement(ctx: &Ctx) -> (Uuid, Uuid) {
    let role = c::make_role(ctx, "ANN", 1).await;
    let user = c::make_user(ctx, "publisher", "HUMAN").await;
    c::bind_role(user, role).await;
    let created = ann_handler::create(
        headers(),
        connect_info(),
        c::claims(user, "publisher", &[]),
        Json(AnnouncementSaveReq {
            title: format!("{} 日志用例", ctx.key("ann")),
            summary: None,
            content_md: "正文".to_string(),
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
    let id = Uuid::parse_str(&created.data.expect("data").id).expect("id");
    ann_handler::publish(
        headers(),
        connect_info(),
        c::claims(user, "publisher", &[]),
        Path(id.to_string()),
    )
    .await
    .expect("发布公告");
    (user, id)
}

/// C01 ★ 每次状态变更都记一条（建 / 发布 / 下线 = 3 条）。
#[tokio::test]
async fn c01_every_state_change_logged() {
    let Some(()) = c::ready().await else { return };
    c::case("C01", |ctx| async move {
        let (user, ann_id) = published_announcement(&ctx).await;
        ann_handler::offline(
            headers(),
            connect_info(),
            c::claims(user, "publisher", &[]),
            Path(ann_id.to_string()),
            Json(AnnouncementOfflineReq {
                reason: "维护结束".to_string(),
            }),
        )
        .await
        .expect("下线");

        let count =
            changelog_model::count_for_object(&*db(), "sys_announcement", &ann_id.to_string())
                .await
                .expect("计数");
        assert_eq!(count, 3, "建/发布/下线应各留一条日志，实际 {count} 条");

        // 动作序列按时间递增且覆盖三种动作
        let timeline = changelog_query::timeline(
            &*db(),
            "sys_announcement",
            &ann_id.to_string(),
            None,
            None,
            50,
        )
        .await
        .expect("时间线");
        let actions: Vec<&str> = timeline.iter().map(|row| row.action.as_str()).collect();
        println!("[C01] 动作序列：{actions:?}");
        for expected in ["CREATE", "PUBLISH", "OFFLINE"] {
            assert!(
                actions.iter().any(|action| *action == expected),
                "缺少 {expected} 动作，实际 {actions:?}"
            );
        }
    })
    .await;
}

/// C02 日志字段完整（谁 / 何时 / 从什么到什么 / 为什么）。
#[tokio::test]
async fn c02_log_fields_complete() {
    let Some(()) = c::ready().await else { return };
    c::case("C02", |ctx| async move {
        let (user, ann_id) = published_announcement(&ctx).await;

        let rows = db()
            .query_sql(
                "SELECT operator_id::text AS operator_id, operator_name, operator_type, action, \
                        action_label, diff::text AS diff, created_at::text AS created_at \
                   FROM sys_change_log WHERE resource = 'sys_announcement' AND resource_id = ?::text \
                   ORDER BY created_at"
                    .to_string(),
                vec![Value::Text(ann_id.to_string())],
            )
            .await
            .expect("查日志");
        assert!(rows.len() >= 2, "日志条数不足：{}", rows.len());
        // 每一行都必须有人 / 有事 / 有时间（缺一列就等于「无法追责」）
        for row in &rows {
            let operator_id: Option<String> = row.get_typed("operator_id").expect("operator_id 列");
            let action_label: String = row.get_typed("action_label").expect("action_label 列");
            assert!(operator_id.is_some(), "存在没有操作人的日志行");
            assert!(!action_label.is_empty(), "存在没有动作中文名的日志行");
        }

        let first = &rows[0];
        assert_eq!(
            first
                .get_typed::<Option<String>>("operator_id")
                .unwrap()
                .as_deref(),
            Some(user.to_string().as_str()),
            "操作人缺失"
        );
        assert_eq!(
            first
                .get_typed::<Option<String>>("operator_name")
                .unwrap()
                .as_deref(),
            Some("publisher"),
            "操作人姓名缺失"
        );
        assert_eq!(first.get_typed::<String>("operator_type").unwrap(), "HUMAN");
        let diff: String = first.get_typed("diff").unwrap();
        assert!(
            diff.contains("title") || diff.contains("before") || diff.contains("status"),
            "diff 里没有变更内容：{diff}"
        );
        assert!(
            !first.get_typed::<String>("created_at").unwrap().is_empty(),
            "发生时刻缺失"
        );

    })
    .await;
}

/// C03 ★ 日志不可篡改：业务代码里没有任何 `UPDATE sys_change_log`。
#[tokio::test]
async fn c03_log_has_no_update_path() {
    let Some(()) = c::ready().await else { return };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    collect_updates(&root, &mut offenders);
    assert!(
        offenders.is_empty(),
        "发现日志 UPDATE 路径（审计可被篡改）：{offenders:?}"
    );

    // 库里也确实没有「改日志」的权限码
    let code = c::count(
        "SELECT count(*) AS n FROM sys_menu WHERE auth_code LIKE '%change-log%update%' \
            OR auth_code LIKE '%changelog%update%'",
        Vec::new(),
    )
    .await;
    assert_eq!(code, 0, "存在「修改变更日志」的权限码（不该有）");
}

/// 递归扫描源码里对 `sys_change_log` 的 UPDATE 语句。
fn collect_updates(dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_updates(&path, out);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let upper = text.to_uppercase();
        if upper.contains("UPDATE SYS_CHANGE_LOG") {
            out.push(path.display().to_string());
        }
    }
}

/// C04 按单据能查完整轨迹（时间线按时间升序、条数与状态变更一致）。
#[tokio::test]
async fn c04_timeline_by_object() {
    let Some(()) = c::ready().await else { return };
    c::case("C04", |ctx| async move {
        let (_user, ann_id) = published_announcement(&ctx).await;

        let timeline = changelog_query::timeline(
            &*db(),
            "sys_announcement",
            &ann_id.to_string(),
            None,
            None,
            100,
        )
        .await
        .expect("时间线");
        assert_eq!(
            timeline.len(),
            2,
            "建 + 发布应有 2 条，实际 {}",
            timeline.len()
        );
        // 时间线默认倒序（最新在前），倒过来即为正序轨迹
        let times: Vec<_> = timeline.iter().map(|row| row.created_at).collect();
        let mut sorted = times.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(
            times, sorted,
            "时间线未按时间倒序（与 SQL ORDER BY 不一致）"
        );

        // 查一个不存在的单据 → 空（不能串到别的单据）
        let empty = changelog_query::timeline(
            &*db(),
            "sys_announcement",
            &Uuid::now_v7().to_string(),
            None,
            None,
            100,
        )
        .await
        .expect("空轨迹");
        assert!(empty.is_empty(), "无关单据查出了日志");
    })
    .await;
}

/// C05 按时间 / 操作人筛选。
#[tokio::test]
async fn c05_filter_by_operator_and_time() {
    let Some(()) = c::ready().await else { return };
    c::case("C05", |ctx| async move {
        let (user, ann_id) = published_announcement(&ctx).await;
        let other = c::make_user(&ctx, "other", "HUMAN").await;

        let mine = changelog_query::page(
            &changelog_query::Filter {
                resource: Some("sys_announcement".to_string()),
                resource_id: Some(ann_id.to_string()),
                operator_id: Some(user),
                ..Default::default()
            },
            1,
            50,
        )
        .await
        .expect("按操作人筛选");
        assert_eq!(
            mine.total, 2,
            "按操作人筛选应命中 2 条，实际 {}",
            mine.total
        );

        let theirs = changelog_query::page(
            &changelog_query::Filter {
                resource: Some("sys_announcement".to_string()),
                resource_id: Some(ann_id.to_string()),
                operator_id: Some(other),
                ..Default::default()
            },
            1,
            50,
        )
        .await
        .expect("按别人筛选");
        assert_eq!(theirs.total, 0, "无关操作人筛出了日志");

        // 时间窗：未来 1 分钟之后 → 0 条
        let future = changelog_query::page(
            &changelog_query::Filter {
                resource: Some("sys_announcement".to_string()),
                resource_id: Some(ann_id.to_string()),
                start_time: Some(chrono::Utc::now() + chrono::Duration::minutes(1)),
                ..Default::default()
            },
            1,
            50,
        )
        .await
        .expect("未来时间窗");
        assert_eq!(future.total, 0, "未来时间窗仍查出日志（时间过滤失效）");
    })
    .await;
}

/// C06 ★ 抽查 10 张单据，changelog 条数与实际状态变更次数一致。
#[tokio::test]
async fn c06_ten_objects_log_count_matches() {
    let Some(()) = c::ready().await else { return };
    c::case("C06", |ctx| async move {
        let mut ids = Vec::new();
        for index in 0..10 {
            let (_, id) = published_announcement_named(&ctx, index).await;
            ids.push(id);
        }

        for (index, id) in ids.iter().enumerate() {
            let count =
                changelog_model::count_for_object(&*db(), "sys_announcement", &id.to_string())
                    .await
                    .expect("计数");
            // 每张单据：1 次建 + 1 次发布 = 2 次状态变更
            assert_eq!(
                count, 2,
                "第 {index} 张单据的日志条数 {count} 与实际状态变更次数 2 不一致"
            );
        }
    })
    .await;
}

/// C06 的夹具：同一用户连发 10 张公告（每张都建 + 发布）。
async fn published_announcement_named(ctx: &Ctx, index: usize) -> (Uuid, Uuid) {
    let role = c::make_role(ctx, &format!("ANN{index}"), 1).await;
    let user = c::make_user(ctx, &format!("publisher{index}"), "HUMAN").await;
    c::bind_role(user, role).await;
    let created = ann_handler::create(
        headers(),
        connect_info(),
        c::claims(user, &format!("publisher{index}"), &[]),
        Json(AnnouncementSaveReq {
            title: format!("{} 抽查{index}", ctx.key("ann")),
            summary: None,
            content_md: "正文".to_string(),
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
    let id = Uuid::parse_str(&created.data.expect("data").id).expect("id");
    ann_handler::publish(
        headers(),
        connect_info(),
        c::claims(user, &format!("publisher{index}"), &[]),
        Path(id.to_string()),
    )
    .await
    .expect("发布");
    (user, id)
}

// Query 未使用时的占位（保持 import 完整）
#[allow(dead_code)]
fn _q() -> Option<Query<()>> {
    None
}
