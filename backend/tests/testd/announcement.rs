//! 测试D · 公告与消息（G01–G10）。
//!
//! 走真实 handler（`announcement::handler::create/publish/update/delete/read_stats`）与
//! `notice::dispatch`，全部落 `cloudpivot_test`。夹具前缀 `TD-G*`，用例自清理。

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, Path, Query};
use axum::http::HeaderMap;
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot::business::announcement::dto::{
    AnnouncementOfflineReq, AnnouncementSaveReq, ReaderState, ReadersQuery,
};
use cloudpivot::business::announcement::{handler as ann_handler, model as ann_model};
use cloudpivot::business::notice::dto::{ContentFormat, NoticeLevel, NoticeType};
use cloudpivot::business::notice::{
    DispatchResult, NoticeDraft, NoticeTarget, dispatch, model as notice_model,
    render as notice_render,
};

use crate::testd::common::{self as c, Ctx};

// ==================== 辅助 ====================

fn headers() -> HeaderMap {
    HeaderMap::new()
}

fn connect_info() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 51888)))
}

/// 下线请求体（原因必填）。
fn offline_req() -> AnnouncementOfflineReq {
    AnnouncementOfflineReq {
        reason: "测试D：下线以验证状态机".to_string(),
    }
}

/// 通知分页过滤条件。
fn notice_filter(
    _ctx: &Ctx,
    keyword: Option<String>,
) -> cloudpivot::business::notice::dto::NoticeFilter {
    cloudpivot::business::notice::dto::NoticeFilter {
        keyword,
        notice_type: Some("SYSTEM".to_string()),
        level: None,
        biz_type: None,
    }
}

/// 建一份草稿公告（USER 范围，指向 `audience`）。
async fn draft(_publisher: Uuid, ctx: &Ctx, audience: &[Uuid]) -> AnnouncementSaveReq {
    AnnouncementSaveReq {
        title: format!("{} 系统维护通知", ctx.key("ann")),
        summary: None,
        content_md: "今晚 22:00 停机维护，请提前保存工作。".to_string(),
        level: Some("NORMAL".to_string()),
        scope_type: Some("USER".to_string()),
        scope_ids: Some(audience.iter().map(|id| id.to_string()).collect()),
        is_top: Some(false),
        top_expire_at: None,
        expire_at: None,
        remark: None,
        attachments: None,
    }
}

/// 建 + 发布，返回公告 id / 通知 id / 接收人数。
async fn create_and_publish(
    publisher: Uuid,
    ctx: &Ctx,
    audience: &[Uuid],
) -> (Uuid, Option<Uuid>, i64) {
    let req = draft(publisher, ctx, audience).await;
    let created = ann_handler::create(
        headers(),
        connect_info(),
        c::claims(publisher, "publisher", &[]),
        Json(req),
    )
    .await
    .expect("发公告（草稿）");
    let created = created.data.expect("公告 data");
    let id = Uuid::parse_str(&created.id).expect("公告 id");
    let published = ann_handler::publish(
        headers(),
        connect_info(),
        c::claims(publisher, "publisher", &[]),
        Path(id.to_string()),
    )
    .await
    .expect("发布公告");
    let published = published.data.expect("发布 data");
    let notice_id = published
        .notice_id
        .as_deref()
        .and_then(|raw| Uuid::parse_str(raw).ok());
    (id, notice_id, published.receiver_count)
}

/// 一个「能发公告」的发布人（角色 data_scope=1，便于看到自己造的数据）。
async fn publisher(ctx: &Ctx) -> Uuid {
    let role = c::make_role(ctx, "ANN_ADMIN", 1).await;
    let user = c::make_user(ctx, "publisher", "HUMAN").await;
    c::bind_role(user, role).await;
    user
}

// ============================================================================
// G01 发公告 → 目标用户能看到
// ============================================================================

#[tokio::test]
async fn g01_published_announcement_visible_to_target() {
    let Some(()) = c::ready().await else { return };
    c::case("G01", |ctx| async move {
        let admin = publisher(&ctx).await;
        let target = c::make_user(&ctx, "target", "HUMAN").await;
        let outsider = c::make_user(&ctx, "outsider", "HUMAN").await;

        let (id, notice_id, receiver_count) = create_and_publish(admin, &ctx, &[target]).await;
        assert_eq!(receiver_count, 1, "接收人数应为 1，实际 {receiver_count}");
        assert!(notice_id.is_some(), "发布未扇出通知");

        // 目标用户能看到
        let audience = ann_model::load_audience(&target)
            .await
            .expect("目标用户可见集");
        let unread = ann_model::count_unread(&audience).await.expect("未读数");
        assert!(unread >= 1, "目标用户未读数应 >= 1，实际 {unread}");
        let visible = ann_model::page_visible(
            &audience,
            &ann_model::VisibleFilter {
                keyword: Some(format!("%{}%", ctx.key("ann"))),
                only_unread: false,
            },
            1,
            50,
        )
        .await
        .expect("可见分页");
        assert!(
            visible.list.iter().any(|row| row.id == id),
            "目标用户在可见列表里看不到该公告"
        );

        // 非目标用户看不到
        let other = ann_model::load_audience(&outsider)
            .await
            .expect("无关用户可见集");
        let other_page = ann_model::page_visible(
            &other,
            &ann_model::VisibleFilter {
                keyword: Some(format!("%{}%", ctx.key("ann"))),
                only_unread: false,
            },
            1,
            50,
        )
        .await
        .expect("可见分页");
        assert!(
            !other_page.list.iter().any(|row| row.id == id),
            "非目标用户看到了定向公告（可见性泄漏）"
        );
    })
    .await;
}

// ============================================================================
// G02 已读 / 未读统计准确
// ============================================================================

#[tokio::test]
async fn g02_read_stats_accurate() {
    let Some(()) = c::ready().await else { return };
    c::case("G02", |ctx| async move {
        let admin = publisher(&ctx).await;
        let u1 = c::make_user(&ctx, "u1", "HUMAN").await;
        let u2 = c::make_user(&ctx, "u2", "HUMAN").await;

        let (id, notice_id, receiver_count) = create_and_publish(admin, &ctx, &[u1, u2]).await;
        assert_eq!(receiver_count, 2, "接收人数应为 2");

        // u1 标记已读（返回 (首次?, 最新 read_count)）
        let (first, read_count) = ann_model::mark_read(&id, &u1).await.expect("标记已读");
        assert!(first, "首次标记应为新增已读");
        assert_eq!(read_count, 1, "已读数应为 1，实际 {read_count}");
        // 幂等：再标一次不再计数
        let (again, still) = ann_model::mark_read(&id, &u1).await.expect("重复标记");
        assert!(!again, "重复标记不该计为新已读");
        assert_eq!(still, 1, "重复标记后已读数仍应为 1");

        // 管理端阅读名单：总数 2、已读 1、未读 1
        let stats = ann_handler::read_stats(
            Path(id.to_string()),
            Query(ReadersQuery {
                page: Some(1),
                page_size: Some(50),
                state: Some("ALL".to_string()),
                keyword: None,
            }),
        )
        .await
        .expect("阅读统计");
        let stats = stats.data.expect("统计 data");
        assert_eq!(stats.receiver_count, 2, "统计里的接收人数不符");
        assert_eq!(stats.read_count, 1, "统计里的已读数不符");

        let unread_only = ann_model::page_readers(
            &id,
            notice_id.as_ref(),
            ann_model::ReaderStateFilter::Unread,
            None,
            1,
            50,
        )
        .await
        .expect("未读名单");
        assert_eq!(unread_only.total, 1, "未读名单应只有 1 人");
        assert_eq!(
            unread_only.list.first().map(|row| row.user_id),
            Some(u2),
            "未读名单里的用户不对"
        );
    })
    .await;
}

// ============================================================================
// G03 公告删除 → 已读回执 + 关联通知（FIX-04 之后：公告软删，通知同事务下线）
// ============================================================================

/// G03 ★ 公告删除（软删）时的联动口径。
///
/// # 口径变更：旧断言守护的缺陷已修（BUG-D-05 关闭）
///
/// 旧断言写的是「公告下线后关联 `sys_notice` **不**被软删」，它是 **BUG-D-05（P3 数据
/// 一致性）** 的守卫。旧断言失效点：**FIX-04** 把「公告软删」与「关联通知下线」绑进
/// 同一个事务（`announcement::model::soft_delete_with_notice` →
/// `notice::model::withdraw_by_biz`），缺陷已修 ⇒ 守卫必然变红（不是回归）。
///
/// 现在改成正向断言（缺陷已修 → 断言新行为正确）：
///
/// 1. 公告 `deleted_at` 有值（软删，不是物理删），用户可见列表里查不到；
/// 2. 公告已读回执（`sys_announcement_read`）行数不变 —— 审计要留痕；
/// 3. 关联通知 `deleted_at` 有值；收件行**不删**，只打 `is_deleted` 用户侧删除标记；
/// 4. 已读回执字段（`read_flag` / `read_at`）一字未改；
/// 5. 「我的消息」查不到它、未读徽标归零（收件人不再被一条「点进去 404」的通知骚扰）。
#[tokio::test]
async fn g03_delete_announcement_keeps_read_receipts() {
    let Some(()) = c::ready().await else { return };
    c::case("G03", |ctx| async move {
        let admin = publisher(&ctx).await;
        let u1 = c::make_user(&ctx, "u1", "HUMAN").await;
        let u2 = c::make_user(&ctx, "u2", "HUMAN").await;

        let (id, notice_id, receivers) = create_and_publish(admin, &ctx, &[u1, u2]).await;
        assert_eq!(receivers, 2, "定向公告应扇给 2 个目标用户");
        let notice_id = notice_id.expect("发布必须扇出站内通知（FIX-04 的通知下线对象）");

        // ---- 删前基线（正向断言要「变前/变后」成对，否则「本来就是 0」也能骗过用例）----
        // 公告：u1 已读 1 条；通知：u1 已读、u2 未读（徽标 = 1）
        ann_model::mark_read(&id, &u1).await.expect("标记公告已读");
        assert_eq!(
            c::count(
                "SELECT count(*) AS n FROM sys_announcement_read WHERE announcement_id = ?::uuid",
                vec![Value::from(&id)]
            )
            .await,
            1,
            "删前应有 1 条公告已读回执"
        );
        let receiver_of_u1 = c::opt_uuid(
            "SELECT id AS v FROM sys_notice_receiver \
             WHERE notice_id = ?::uuid AND user_id = ?::uuid",
            vec![Value::from(&notice_id), Value::from(&u1)],
        )
        .await
        .expect("u1 的通知收件行（扇出即建）");
        notice_model::mark_read(&receiver_of_u1, &u1)
            .await
            .expect("标记通知已读");
        assert_eq!(
            notice_model::count_unread(&u1).await.expect("u1 未读"),
            0,
            "u1 已读，删前未读应为 0"
        );
        assert_eq!(
            notice_model::count_unread(&u2).await.expect("u2 未读"),
            1,
            "u2 未读，删前徽标应为 1"
        );

        // 已发布公告删除：先下线再删（ensure_deletable 的口径）
        ann_handler::offline(
            headers(),
            connect_info(),
            c::claims(admin, "publisher", &[]),
            Path(id.to_string()),
            Json(offline_req()),
        )
        .await
        .expect("下线");
        ann_handler::delete(
            headers(),
            connect_info(),
            c::claims(admin, "publisher", &[]),
            Path(id.to_string()),
        )
        .await
        .expect("删除公告");

        // ---- ① 公告软删（deleted_at 有值）+ 已读回执保留 ----
        let deleted_at = c::opt_text(
            "SELECT deleted_at::text AS v FROM sys_announcement WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await;
        assert!(deleted_at.is_some(), "公告未被软删（应是软删，不是物理删）");
        let receipts = c::count(
            "SELECT count(*) AS n FROM sys_announcement_read WHERE announcement_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await;
        assert_eq!(receipts, 1, "公告已读回执在删除后被清掉了（审计需要保留）");

        // ---- ② 用户侧不再可见（软删过滤生效）----
        let audience = ann_model::load_audience(&u1).await.expect("可见集");
        let page = ann_model::page_visible(
            &audience,
            &ann_model::VisibleFilter {
                keyword: Some(ctx.key("ann")),
                only_unread: false,
            },
            1,
            50,
        )
        .await
        .expect("可见分页");
        assert!(
            !page.list.iter().any(|row| row.id == id),
            "软删后的公告仍出现在用户可见列表里"
        );

        // ---- ③ 关联通知：FIX-04 起随公告**同事务**软删 ----
        let notice_deleted = c::opt_text(
            "SELECT deleted_at::text AS v FROM sys_notice WHERE id = ?::uuid",
            vec![Value::from(&notice_id)],
        )
        .await;
        assert!(
            notice_deleted.is_some(),
            "公告已删但关联通知仍在线 —— BUG-D-05 回归（FIX-04 的通知下线没生效）"
        );
        // 收件行不删（审计要能回答「发给谁了」），只打用户侧删除标记
        let total_rows = c::count(
            "SELECT count(*) AS n FROM sys_notice_receiver WHERE notice_id = ?::uuid",
            vec![Value::from(&notice_id)],
        )
        .await;
        assert_eq!(
            total_rows, 2,
            "通知收件行被物理删了（应只打 is_deleted 标记、行留着做审计）"
        );
        let still_visible = c::count(
            "SELECT count(*) AS n FROM sys_notice_receiver \
             WHERE notice_id = ?::uuid AND is_deleted = FALSE",
            vec![Value::from(&notice_id)],
        )
        .await;
        assert_eq!(still_visible, 0, "通知下线后仍有收件行没打用户侧删除标记");
        // ---- ④ 已读回执字段（read_flag / read_at）原样保留 ----
        let read_kept = c::count(
            "SELECT count(*) AS n FROM sys_notice_receiver \
             WHERE notice_id = ?::uuid AND user_id = ?::uuid \
               AND read_flag = TRUE AND read_at IS NOT NULL",
            vec![Value::from(&notice_id), Value::from(&u1)],
        )
        .await;
        assert_eq!(
            read_kept, 1,
            "已读回执（read_flag/read_at）被下线动作清掉了（审计口径要求保留）"
        );

        // ---- ⑤ 消息中心 / 未读徽标：两人都查不到、未读归零 ----
        for (user, who) in [(u1, "u1"), (u2, "u2")] {
            assert_eq!(
                notice_model::count_unread(&user).await.expect("未读"),
                0,
                "{who} 的未读徽标没归零（公告已删，通知不该再计未读）"
            );
            let mine = notice_model::page_my_notices(&user, None, 1, 50)
                .await
                .expect("我的消息");
            assert!(
                !mine.list.iter().any(|row| row.notice_id == notice_id),
                "{who} 的消息中心仍能看到已删公告的通知（点进去 404）"
            );
        }
    })
    .await;
}

// ============================================================================
// G04 已发布还能编辑吗（实测：不能，必须先下线）
// ============================================================================

#[tokio::test]
async fn g04_published_announcement_not_editable() {
    let Some(()) = c::ready().await else { return };
    c::case("G04", |ctx| async move {
        let admin = publisher(&ctx).await;
        let u1 = c::make_user(&ctx, "u1", "HUMAN").await;

        let (id, _, _) = create_and_publish(admin, &ctx, &[u1]).await;
        let mut req = draft(admin, &ctx, &[u1]).await;
        req.title = format!("{} 试图改标题", ctx.key("ann"));

        let err = ann_handler::update(
            headers(),
            connect_info(),
            c::claims(admin, "publisher", &[]),
            Path(id.to_string()),
            Json(req),
        )
        .await
        .expect_err("已发布公告应禁止编辑");
        assert!(
            err.status == 409 || err.status == 400,
            "已发布编辑应被拒（实测 {}+{}）",
            err.status,
            err.code
        );

        // 下线后可以编辑（状态机放行）
        ann_handler::offline(
            headers(),
            connect_info(),
            c::claims(admin, "publisher", &[]),
            Path(id.to_string()),
            Json(offline_req()),
        )
        .await
        .expect("下线");
        let mut req2 = draft(admin, &ctx, &[u1]).await;
        req2.title = format!("{} 下线后改标题", ctx.key("ann"));
        let updated = ann_handler::update(
            headers(),
            connect_info(),
            c::claims(admin, "publisher", &[]),
            Path(id.to_string()),
            Json(req2),
        )
        .await
        .expect("下线后可编辑");
        assert!(updated.data.is_some());
    })
    .await;
}

// ============================================================================
// G05 消息扇出到多目标
// ============================================================================

#[tokio::test]
async fn g05_notice_fanout_multiple_targets() {
    let Some(()) = c::ready().await else { return };
    c::case("G05", |ctx| async move {
        let sender = publisher(&ctx).await;
        let u1 = c::make_user(&ctx, "u1", "HUMAN").await;
        let u2 = c::make_user(&ctx, "u2", "HUMAN").await;

        let draft = NoticeDraft {
            notice_type: NoticeType::System,
            level: NoticeLevel::Info,
            title: format!("{} 站内通知", ctx.key("notice")),
            content: "多目标扇出用例".to_string(),
            content_format: ContentFormat::Text,
            link_url: None,
            biz_type: Some("testd".to_string()),
            biz_id: Some(ctx.key("biz")),
            template_code: None,
            template_vars: serde_json::Value::Null,
            target: NoticeTarget::parse("USER", &[u1.to_string(), u2.to_string()])
                .expect("目标解析"),
            channel_mask: 1, // 仅站内信
            idempotency_key: Some(ctx.key("idem")),
            expire_at: None,
            scheduled_at: None,
            sender_id: Some(sender),
            sender_name: Some("publisher".to_string()),
            force: true,
        };
        let result: DispatchResult = dispatch(draft).await.expect("发送通知");
        assert_eq!(
            result.receiver_count, 2,
            "扇出接收人数应为 2，实际 {}",
            result.receiver_count
        );

        // 站内信通道：两人各一条收件行
        let receivers = c::count(
            "SELECT count(*) AS n FROM sys_notice_receiver WHERE notice_id = ?::uuid",
            vec![Value::from(&result.notice_id)],
        )
        .await;
        assert_eq!(receivers, 2, "收件行应为 2 条，实际 {receivers}");

        // 每人未读 +1
        assert_eq!(notice_model::count_unread(&u1).await.expect("u1 未读"), 1);
        assert_eq!(notice_model::count_unread(&u2).await.expect("u2 未读"), 1);
    })
    .await;
}

// ============================================================================
// G06 消息已读 → 未读数 -1
// ============================================================================

#[tokio::test]
async fn g06_mark_read_decrements_unread() {
    let Some(()) = c::ready().await else { return };
    c::case("G06", |ctx| async move {
        let sender = publisher(&ctx).await;
        let u1 = c::make_user(&ctx, "u1", "HUMAN").await;

        let result = dispatch(NoticeDraft {
            notice_type: NoticeType::System,
            level: NoticeLevel::Info,
            title: format!("{} 已读联动", ctx.key("notice")),
            content: "点开就少一个角标".to_string(),
            content_format: ContentFormat::Text,
            link_url: None,
            biz_type: None,
            biz_id: None,
            template_code: None,
            template_vars: serde_json::Value::Null,
            target: NoticeTarget::parse("USER", &[u1.to_string()]).expect("目标"),
            channel_mask: 1,
            idempotency_key: Some(ctx.key("idem")),
            expire_at: None,
            scheduled_at: None,
            sender_id: Some(sender),
            sender_name: Some("publisher".to_string()),
            force: true,
        })
        .await
        .expect("发送");

        assert_eq!(notice_model::count_unread(&u1).await.unwrap(), 1);
        let receiver_id = c::opt_uuid(
            "SELECT id AS v FROM sys_notice_receiver WHERE notice_id = ?::uuid AND user_id = ?::uuid",
            vec![Value::from(&result.notice_id), Value::from(&u1)],
        )
        .await
        .expect("收件行");
        assert!(
            notice_model::mark_read(&receiver_id, &u1)
                .await
                .expect("标记已读")
        );
        assert_eq!(
            notice_model::count_unread(&u1).await.unwrap(),
            0,
            "已读后未读数应为 0"
        );
        // 重复标记不报错也不重复计数
        assert!(
            !notice_model::mark_read(&receiver_id, &u1)
                .await
                .expect("重复标记")
        );

    })
    .await;
}

// ============================================================================
// G07 消息模板变量替换
// ============================================================================

#[tokio::test]
async fn g07_template_variable_substitution() {
    let Some(()) = c::ready().await else { return };
    c::case("G07", |ctx| async move {
        let sender = publisher(&ctx).await;
        let u1 = c::make_user(&ctx, "u1", "HUMAN").await;

        // 模板：标题与正文都含 {{name}}
        // 约束 ck_sys_notice_template_code 只允许 `^[A-Za-z][A-Za-z0-9_]{1,63}$`（无连字符）
        let template_code = ctx.key("tpl").replace('-', "_");
        c::exec_with(
            "INSERT INTO sys_notice_template \
                 (id, template_code, template_name, notice_type, default_level, title_template, \
                  content_template, content_format, channels, variables, status) \
             VALUES (gen_random_uuid(), ?, ?, 'SYSTEM', 'INFO', ?, ?, 'TEXT', 1, \
                     '{\"name\":{\"label\":\"姓名\",\"required\":true}}'::jsonb, 1)",
            vec![
                Value::Text(template_code.clone()),
                Value::Text(ctx.key("模板")),
                Value::Text("你好 {{name}}".to_string()),
                Value::Text("{{name}} 的账号已开通".to_string()),
            ],
        )
        .await;

        let result = dispatch(NoticeDraft {
            notice_type: NoticeType::System,
            level: NoticeLevel::Info,
            title: String::new(),
            content: String::new(),
            content_format: ContentFormat::Text,
            link_url: None,
            biz_type: None,
            biz_id: None,
            template_code: Some(template_code.clone()),
            template_vars: serde_json::json!({ "name": ctx.key("张三") }),
            target: NoticeTarget::parse("USER", &[u1.to_string()]).expect("目标"),
            channel_mask: 1,
            idempotency_key: Some(ctx.key("idem")),
            expire_at: None,
            scheduled_at: None,
            sender_id: Some(sender),
            sender_name: Some("publisher".to_string()),
            force: true,
        })
        .await
        .expect("模板发送");

        let title = c::text(
            "SELECT title AS v FROM sys_notice WHERE id = ?::uuid",
            vec![Value::from(&result.notice_id)],
        )
        .await;
        let content = c::text(
            "SELECT content AS v FROM sys_notice WHERE id = ?::uuid",
            vec![Value::from(&result.notice_id)],
        )
        .await;
        assert_eq!(
            title,
            format!("你好 {}", ctx.key("张三")),
            "标题变量未替换：{title}"
        );
        assert_eq!(
            content,
            format!("{} 的账号已开通", ctx.key("张三")),
            "正文变量未替换：{content}"
        );

        // 缺变量 → 渲染失败（不能把 {{name}} 原样发给用户）
        let missing = notice_render::render_content(
            "你好 {{name}}",
            &serde_json::json!({}),
            ContentFormat::Text,
        );
        assert!(missing.is_err(), "缺变量时渲染应失败");
    })
    .await;
}

// ============================================================================
// G08 发送失败 → 失败状态可见 / 可重试
// ============================================================================

#[tokio::test]
async fn g08_channel_failure_visible() {
    let Some(()) = c::ready().await else { return };
    c::case("G08", |ctx| async move {
        let sender = publisher(&ctx).await;
        let u1 = c::make_user(&ctx, "u1", "HUMAN").await;
        // 给收件人一个邮箱：否则「邮件通道」不生成投递行（只按有地址的收件人建行）
        c::exec_with(
            "UPDATE sys_user SET email = ? WHERE id = ?::uuid",
            vec![
                Value::Text(format!("{}@example.test", ctx.prefix.to_lowercase())),
                Value::from(&u1),
            ],
        )
        .await;
        let result = dispatch(NoticeDraft {
            notice_type: NoticeType::System,
            level: NoticeLevel::Info,
            title: format!("{} 邮件通道", ctx.key("notice")),
            content: "邮件网关未配置".to_string(),
            content_format: ContentFormat::Text,
            link_url: None,
            biz_type: None,
            biz_id: None,
            template_code: None,
            template_vars: serde_json::Value::Null,
            target: NoticeTarget::parse("USER", &[u1.to_string()]).expect("目标"),
            channel_mask: 2, // 邮件通道
            idempotency_key: Some(ctx.key("idem")),
            expire_at: None,
            scheduled_at: None,
            sender_id: Some(sender),
            sender_name: Some("publisher".to_string()),
            force: true,
        })
        .await
        .expect("发送（邮件通道）");

        // 收件人行（用于「我的消息」列表与已读回执）不区分通道，仍然要建；
        // 「通道掩码」约束的是**投递**（sys_notice_delivery），这是设计口径。
        let receivers = c::count(
            "SELECT count(*) AS n FROM sys_notice_receiver WHERE notice_id = ?::uuid",
            vec![Value::from(&result.notice_id)],
        )
        .await;
        assert_eq!(receivers, 1, "收件人行应为 1（收件人 ≠ 通道投递）");
        let inbox_delivery = c::count(
            "SELECT count(*) AS n FROM sys_notice_delivery \
             WHERE notice_id = ?::uuid AND lower(channel) = 'inbox'",
            vec![Value::from(&result.notice_id)],
        )
        .await;
        assert_eq!(
            inbox_delivery, 0,
            "只勾邮件时不该有站内信投递记录（通道掩码失效）"
        );

        // 投递记录必须存在且状态可读（未配置通道 = SKIPPED，不是静默丢失）
        let email_status = c::text(
            "SELECT status AS v FROM sys_notice_delivery \
             WHERE notice_id = ?::uuid AND lower(channel) = 'email' LIMIT 1",
            vec![Value::from(&result.notice_id)],
        )
        .await;
        assert_eq!(
            email_status, "SKIPPED",
            "本地没有邮件网关，邮件投递应为 SKIPPED（实际 {email_status}）"
        );
        // DispatchResult 里也逐通道给出状态（调用方不必查库）
        let email_state = result
            .channels
            .iter()
            .find(|state| state.channel.eq_ignore_ascii_case("EMAIL"))
            .expect("DispatchResult 缺 EMAIL 通道状态");
        assert_eq!(email_state.status, "SKIPPED");
        assert_eq!(email_state.rows, 1, "有邮箱的收件人应生成 1 条邮件投递行");
        let inbox_state = result
            .channels
            .iter()
            .find(|state| state.channel.eq_ignore_ascii_case("INBOX"))
            .expect("站内信通道恒在（plan_channels 强制 OR）");
        assert_eq!(inbox_state.status, "DELIVERED");
        println!(
            "[G08] 通道状态：{email_status} / INBOX={}",
            inbox_state.status
        );
    })
    .await;
}

// ============================================================================
// G09 公告权限：谁能发
// ============================================================================

#[tokio::test]
async fn g09_announcement_publish_permission() {
    let Some(()) = c::ready().await else { return };
    c::case("G09", |ctx| async move {
        for code in [
            "business:announcement:create",
            "business:announcement:publish",
            "business:announcement:delete",
            "business:notice:send",
        ] {
            let n = c::count(
                "SELECT count(*) AS n FROM sys_menu WHERE auth_code = ?",
                vec![Value::Text(code.to_string())],
            )
            .await;
            assert!(n >= 1, "权限码 {code} 未登记（人人可发的风险）");
        }

        // 普通用户（只有读取码）不得被判为可发布
        let role = c::make_role(&ctx, "READER", 4).await;
        let user = c::make_user(&ctx, "reader", "HUMAN").await;
        c::bind_role(user, role).await;
        let claims = c::token_claims(user, "reader", &["business:announcement:read"]);
        assert!(!claims.has_permission("business:announcement:publish"));
        assert!(!claims.has_permission("business:notice:send"));
        assert!(claims.has_permission("business:announcement:read"));
    })
    .await;
}

// ============================================================================
// G10 消息搜索 / 筛选 / 分页
// ============================================================================

#[tokio::test]
async fn g10_notice_search_filter_paging() {
    let Some(()) = c::ready().await else { return };
    c::case("G10", |ctx| async move {
        let sender = publisher(&ctx).await;
        let u1 = c::make_user(&ctx, "u1", "HUMAN").await;

        // 3 条同一关键词的通知
        for index in 0..3 {
            dispatch(NoticeDraft {
                notice_type: NoticeType::System,
                level: NoticeLevel::Info,
                title: format!("{} 第{index}条", ctx.key("notice")),
                content: "检索用".to_string(),
                content_format: ContentFormat::Text,
                link_url: None,
                biz_type: Some(ctx.key("biz")),
                biz_id: None,
                template_code: None,
                template_vars: serde_json::Value::Null,
                target: NoticeTarget::parse("USER", &[u1.to_string()]).expect("目标"),
                channel_mask: 1,
                idempotency_key: Some(ctx.key(&format!("idem{index}"))),
                expire_at: None,
                scheduled_at: None,
                sender_id: Some(sender),
                sender_name: Some("publisher".to_string()),
                force: true,
            })
            .await
            .expect("发送");
        }

        let sender_principal = cloudpivot::business::shared::principal::resolve(&sender)
            .await
            .expect("解析发送人范围");
        let filter = notice_filter(&ctx, Some(ctx.key("notice")));
        let first = notice_model::page_notices(&filter, 1, 2, &sender_principal)
            .await
            .expect("第 1 页");
        assert_eq!(first.total, 3, "关键词命中总数应为 3，实际 {}", first.total);
        assert_eq!(first.list.len(), 2, "每页 2 条，第 1 页应有 2 条");
        let second = notice_model::page_notices(&filter, 2, 2, &sender_principal)
            .await
            .expect("第 2 页");
        assert_eq!(second.list.len(), 1, "第 2 页应剩 1 条");

        // 无关关键词 → 0 条
        let miss = notice_model::page_notices(
            &notice_filter(&ctx, Some(format!("{}-不存在", ctx.prefix))),
            1,
            10,
            &sender_principal,
        )
        .await
        .expect("无关关键词");
        assert_eq!(miss.total, 0, "无关关键词不该命中");

        // 用户侧「我的通知」分页同口径
        let mine = notice_model::page_my_notices(&u1, None, 1, 10)
            .await
            .expect("我的通知");
        assert!(mine.total >= 3, "我的通知应 >= 3 条，实际 {}", mine.total);

        // 管理端分页查询（按发送人范围收口：用发送人自己的 principal）
        let sender_principal = cloudpivot::business::shared::principal::resolve(&sender)
            .await
            .expect("解析发送人范围");
        let by_admin = notice_model::page_notices(
            &notice_filter(&ctx, Some(ctx.key("notice"))),
            1,
            10,
            &sender_principal,
        )
        .await
        .expect("管理端分页");
        assert!(
            by_admin.total >= 3,
            "管理端分页应 >= 3 条，实际 {}",
            by_admin.total
        );
    })
    .await;
}

// 供上方的类型引用（ReaderState 仅用于类型完整性检查）
#[allow(dead_code)]
fn _reader_state() -> ReaderState {
    ReaderState::All
}
