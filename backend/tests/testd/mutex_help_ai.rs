//! 测试D · 互斥 / 帮助中心 / AI Agent / 监控（X01–X05、H01–H03、AI-01/02、MO-01）。
//!
//! 全部走真库（`cloudpivot_test`），夹具前缀 `TD-M*`，用例自清理。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::testd::common::{self as c};

// ============================================================================
// P2 · 角色互斥（mutex）
// ============================================================================

/// X01 配置互斥角色（制单员 ⚔ 审核员）：落库后**反向也命中**（对称性）。
#[tokio::test]
async fn x01_mutex_pair_is_symmetric() {
    let Some(()) = c::ready().await else { return };
    c::case("M01", |ctx| async move {
        let a = c::make_role(&ctx, "MAKER", 1).await;
        let b = c::make_role(&ctx, "CHECKER", 1).await;

        let id = mutex_model::insert_mutex(&a, &b, &ctx.key("reason"), None)
            .await
            .expect("写入互斥规则");
        // 正向 / 反向都能查到同一条
        let forward = mutex_model::find_active_pair(&a, &b).await.unwrap();
        let backward = mutex_model::find_active_pair(&b, &a).await.unwrap();
        assert_eq!(forward.map(|row| row.id), Some(id), "正向未命中");
        assert_eq!(
            backward.map(|row| row.id),
            Some(id),
            "反向未命中（对称性被破坏）"
        );

        // 库里只有一行（LEAST/GREATEST 唯一索引保证）
        let rows = c::count(
            "SELECT count(*) AS n FROM sys_role_mutex WHERE (role_a_id = ?::uuid AND role_b_id = ?::uuid) \
             OR (role_a_id = ?::uuid AND role_b_id = ?::uuid)",
            vec![
                Value::from(&a), Value::from(&b), Value::from(&b), Value::from(&a),
            ],
        )
        .await;
        assert_eq!(rows, 1, "同一条互斥规则落了多行");

        // 反向再插一次必须被唯一索引拒绝（A×B 与 B×A 是同一条规则）
        let dup = c::try_exec(
            "INSERT INTO sys_role_mutex (id, role_a_id, role_b_id, reason) VALUES (gen_random_uuid(), ?::uuid, ?::uuid, 'dup')",
            vec![Value::from(&b), Value::from(&a)],
        )
        .await;
        assert!(
            dup.is_err(),
            "反向重复插入未被唯一索引拒绝（对称性只靠应用层）"
        );

    })
    .await;
}

/// X02 同时持两个互斥角色 → 拒绝（P09 踩过：qa.mutex 同时持有两个）。
#[tokio::test]
async fn x02_assign_second_mutex_role_rejected() {
    let Some(()) = c::ready().await else { return };
    c::case("M02", |ctx| async move {
        let a = c::make_role(&ctx, "MAKER", 1).await;
        let b = c::make_role(&ctx, "CHECKER", 1).await;
        mutex_model::insert_mutex(&a, &b, &ctx.key("reason"), None)
            .await
            .expect("写入互斥规则");

        // 用户已持有 A，本次再授予 A+B（全量替换语义 → 最终集合 {A,B}）
        let user = c::make_user(&ctx, "holder", "HUMAN").await;
        c::bind_role(user, a).await;

        let err = mutex_guard::ensure_no_mutex(&user, &[a, b])
            .await
            .expect_err("双持必须被拒绝");
        assert_eq!(
            err.status, 409,
            "状态码应为 409，实际 {}：{}",
            err.status, err.msg
        );
        assert_eq!(
            err.code, 61009,
            "错误码应为 ROLE_MUTEX_CONFLICT(61009)，实际 {}",
            err.code
        );

        // 只授予其中一个 → 放行
        assert!(
            mutex_guard::ensure_no_mutex(&user, &[a]).await.is_ok(),
            "单角色被误拒"
        );
        // 「一次调用里把 A 换成 B」同样被拒：assign_roles 是全量替换，请求里同时出现 A、B。
        // 这是模块文档「并集」一节**有意保留**的行为（解除动作要显式留痕，管理员分两步走）。
        let swap = mutex_guard::ensure_no_mutex(&user, &[b]).await;
        assert!(
            swap.is_err(),
            "已持有 A 时同一请求内换成 B 应被拒（两步走），实际通过"
        );
        // 两步走第二步：先解绑 A（库里删绑定）再授予 B → 通过
        c::exec_with(
            "DELETE FROM sys_user_role WHERE user_id = ?::uuid",
            vec![Value::from(&user)],
        )
        .await;
        assert!(
            mutex_guard::ensure_no_mutex(&user, &[b]).await.is_ok(),
            "先解绑再授予 B 应通过"
        );
    })
    .await;
}

/// X03 互斥校验在**分配角色时**生效：新建用户「一次请求里同时授予 A+B」也要拦（架构 D9 并集）。
#[tokio::test]
async fn x03_single_request_dual_grant_rejected() {
    let Some(()) = c::ready().await else { return };
    c::case("M03", |ctx| async move {
        let a = c::make_role(&ctx, "MAKER", 1).await;
        let b = c::make_role(&ctx, "CHECKER", 1).await;
        mutex_model::insert_mutex(&a, &b, &ctx.key("reason"), None)
            .await
            .expect("写入互斥规则");

        // 尚未落库的新用户（sys_user_role 里没有绑定）→ 并集退化为「本次授予」
        let fresh = Uuid::now_v7();
        let err = mutex_guard::ensure_no_mutex(&fresh, &[a, b])
            .await
            .expect_err("同次授予两个互斥角色必须被拒绝（D9）");
        assert_eq!(err.code, 61009);
    })
    .await;
}

/// X04 已有用户同时持互斥角色（绕过 API 直接写库）→ 体检 / 全量扫描必须报出来。
#[tokio::test]
async fn x04_existing_dual_holder_detected() {
    let Some(()) = c::ready().await else { return };
    c::case("M04", |ctx| async move {
        let a = c::make_role(&ctx, "MAKER", 1).await;
        let b = c::make_role(&ctx, "CHECKER", 1).await;
        mutex_model::insert_mutex(&a, &b, &ctx.key("reason"), None)
            .await
            .expect("写入互斥规则");

        let user = c::make_user(&ctx, "legacy", "HUMAN").await;
        c::bind_role(user, a).await;
        c::bind_role(user, b).await; // 模拟历史违规

        let hits = mutex_checker::user_conflicts(&user).await.expect("体检");
        assert_eq!(
            hits.len(),
            1,
            "单用户体检未报出双持（实际命中 {} 组）",
            hits.len()
        );

        // 全量扫描里也要出现该用户（不是只看单用户）
        let scan = mutex_checker::scan().await.expect("全量扫描");
        let scanned = scan
            .items
            .iter()
            .any(|group| group.sample.iter().any(|holder| holder.user_id == user));
        assert!(scanned, "全量扫描未包含该违规用户");

        // 结论：校验点 3 会阻止在该角色对上「新建规则」，但存量违规仍靠扫描暴露
        let blocked = mutex_guard::ensure_pair_has_no_holders(&a, &b, "制单员", "审核员").await;
        assert!(blocked.is_err(), "已有双持用户时，规则对必须被 409 拦住");
    })
    .await;
}

/// X05 ★ 互斥对 ERP 单据是否生效（制单人 ≠ 审核人，内控第一原则）。
///
/// 这里证明的是**互斥基座**对 ERP 单据场景成立：把 ERP 的「制单」「审核」两个角色配成互斥，
/// 同一用户就无法同时持有 → 交接单/凭证的制单与审核不可能落在一个人身上。
#[tokio::test]
async fn x05_mutex_covers_erp_maker_checker() {
    let Some(()) = c::ready().await else { return };
    c::case("M05", |ctx| async move {
        let maker = c::make_role(&ctx, "ERP_MAKER", 1).await;
        let checker = c::make_role(&ctx, "ERP_CHECKER", 1).await;
        mutex_model::insert_mutex(&maker, &checker, &ctx.key("erp-sod"), None)
            .await
            .expect("写入互斥规则");

        let user = c::make_user(&ctx, "erp-user", "HUMAN").await;
        c::bind_role(user, maker).await;
        let err = mutex_guard::ensure_no_mutex(&user, &[maker, checker])
            .await
            .expect_err("制单人+审核人双持必须被拒");
        assert_eq!(err.code, 61009);

        // 现状说明（ERP 侧是否另有独立校验由 K08 覆盖）：本断言只钉住「互斥基座可用」
        let held = mutex_model::held_role_ids(&user)
            .await
            .expect("查询已持有角色");
        assert_eq!(held, vec![maker], "有效持有集合应只含制单角色");
    })
    .await;
}

// ============================================================================
// P2 · 帮助中心（help）
// ============================================================================

/// H01 帮助文档 CRUD：分类 → 建草稿 → 保存（版本 +1）→ 发布 → 软删。
#[tokio::test]
async fn h01_help_doc_crud() {
    let Some(()) = c::ready().await else { return };
    c::case("H01", |ctx| async move {
        let cat = help_model::create_category(&help_model::CategoryInput {
            parent_id: None,
            name: format!("{} 分类", ctx.tag),
            code: ctx.lkey("cat"),
            icon: None,
            description: None,
            sort_order: 1,
            status: 1,
        })
        .await
        .expect("建分类");

        let doc = help_model::create_doc(
            &help_model::DocInput {
                category_id: cat.id,
                title: format!("{} 快速上手", ctx.tag),
                slug: ctx.lkey("doc"),
                summary: None,
                content_md: "# 标题\n\n第一版正文".to_string(),
                keywords: Some("测试,帮助".to_string()),
                sort_order: 1,
            },
            None,
        )
        .await
        .expect("建文档");
        assert_eq!(doc.status, "DRAFT", "新建文档应为草稿");

        // 保存 → 版本 +1（新建时 version = 0，首次保存落 1 并生成一份快照）
        let saved = help_model::save_doc(
            &doc.id,
            &help_model::DocInput {
                category_id: cat.id,
                title: format!("{} 快速上手", ctx.tag),
                slug: ctx.lkey("doc"),
                summary: None,
                content_md: "# 标题\n\n第二版正文".to_string(),
                keywords: None,
                sort_order: 1,
            },
            None,
            Some("改正文"),
        )
        .await
        .expect("保存文档");
        assert_eq!(
            doc.version, 0,
            "新建文档 version 应为 0，实际 {}",
            doc.version
        );
        assert_eq!(
            saved.version, 1,
            "首次保存后 version 应为 1，实际 {}",
            saved.version
        );
        assert_eq!(
            c::count(
                "SELECT count(*) AS n FROM sys_help_doc_version WHERE doc_id = ?::uuid",
                vec![Value::from(&doc.id)]
            )
            .await,
            1,
            "保存应生成一份版本快照"
        );

        // 发布
        let published = help_model::publish_doc(&doc.id, None, Some("首发"))
            .await
            .expect("发布文档");
        assert_eq!(published.status, "PUBLISHED");

        // 软删分类前必须先无文档：先删文档
        exec_delete_doc(&doc.id).await;
        help_model::delete_category(&cat.id).await.expect("删分类");
    })
    .await;
}

async fn exec_delete_doc(id: &Uuid) {
    c::exec_with(
        "UPDATE sys_help_doc SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(id)],
    )
    .await;
}

/// H02 帮助搜索：命中 / 未命中 / 空关键词（空关键词 = 空页，不查库）。
#[tokio::test]
async fn h02_help_search() {
    let Some(()) = c::ready().await else { return };
    c::case("H02", |ctx| async move {
        let cat = help_model::create_category(&help_model::CategoryInput {
            parent_id: None,
            name: format!("{} 分类", ctx.tag),
            code: ctx.lkey("cat"),
            icon: None,
            description: None,
            sort_order: 1,
            status: 1,
        })
        .await
        .expect("建分类");
        let doc = help_model::create_doc(
            &help_model::DocInput {
                category_id: cat.id,
                title: format!("{} 报销流程说明", ctx.tag),
                slug: ctx.lkey("doc"),
                summary: None,
                content_md: "差旅报销需要附发票与行程单".to_string(),
                keywords: Some(ctx.key("kw").as_str().to_string()),
                sort_order: 1,
            },
            None,
        )
        .await
        .expect("建文档");
        help_model::publish_doc(&doc.id, None, None)
            .await
            .expect("发布");

        // 搜索命中：关键词只匹配 title / content_md（**不含 keywords 列**，见 model::search 的 SQL）
        let hit = help_model::search(
            &help_model::SearchFilter {
                terms: help_model::split_terms("发票"),
                status: None,
                published_only: true,
                category_id: None,
            },
            1,
            20,
        )
        .await
        .expect("搜索");
        assert!(
            hit.list.iter().any(|row| row.id == doc.id),
            "正文关键词「发票」未命中本用例文档（total={}）",
            hit.total
        );

        // 现状（已记入报告 BUG-D-12，P3）：`keywords` 列不参与检索 —— 只写进 keywords 的词搜不到
        let kw_only = help_model::search(
            &help_model::SearchFilter {
                terms: help_model::split_terms(&ctx.key("kw")),
                status: None,
                published_only: true,
                category_id: None,
            },
            1,
            20,
        )
        .await
        .expect("搜索");
        assert_eq!(
            kw_only.total, 0,
            "keywords 列若已接入检索，请更新本断言与报告 BUG-D-12"
        );

        let miss = help_model::search(
            &help_model::SearchFilter {
                terms: help_model::split_terms("绝不存在的关键词zzz"),
                status: None,
                published_only: true,
                category_id: None,
            },
            1,
            20,
        )
        .await
        .expect("搜索");
        assert_eq!(miss.total, 0, "无关关键词不该命中");

        // 空关键词 → split_terms 空（handler 层直接返回空页，不查库）
        assert!(
            help_model::split_terms("   ").is_empty(),
            "空白关键词应被切成空词表"
        );
    })
    .await;
}

/// H03 ★ 帮助内容 XSS 防护：`<script>` 即便被手工塞进 content_html，出参也必须净化。
#[tokio::test]
async fn h03_help_content_xss_sanitized() {
    let Some(()) = c::ready().await else { return };
    c::case("H03", |ctx| async move {
        let cat = help_model::create_category(&help_model::CategoryInput {
            parent_id: None,
            name: format!("{} 分类", ctx.tag),
            code: ctx.lkey("cat"),
            icon: None,
            description: None,
            sort_order: 1,
            status: 1,
        })
        .await
        .expect("建分类");

        // 走真实写路径：正文里带脚本
        let doc = help_model::create_doc(
            &help_model::DocInput {
                category_id: cat.id,
                title: format!("{} XSS 用例", ctx.tag),
                slug: ctx.lkey("doc"),
                summary: None,
                content_md: "正常文本 <script>alert('xss')</script> 结尾".to_string(),
                keywords: None,
                sort_order: 1,
            },
            None,
        )
        .await
        .expect("建文档");
        let stored = c::text(
            "SELECT content_html AS v FROM sys_help_doc WHERE id = ?::uuid",
            vec![Value::from(&doc.id)],
        )
        .await;
        assert!(
            !stored.to_lowercase().contains("<script"),
            "入库快照里仍有 <script>：{stored}"
        );

        // 防御纵深：手工把脏 HTML 写进库里，出参净化仍然要挡住
        c::exec_with(
            "UPDATE sys_help_doc SET content_html = ? WHERE id = ?::uuid",
            vec![
                Value::Text(
                    "<p>ok</p><script>alert(1)</script><img src=x onerror=alert(2)>".to_string(),
                ),
                Value::from(&doc.id),
            ],
        )
        .await;
        let row = help_model::find_doc(&doc.id)
            .await
            .unwrap()
            .expect("文档存在");
        // 详情出参的唯一出口是 handler::detail_html（见 help/handler.rs 闸 3），这里复算同一净化
        let purified = content_api::sanitize_html(&row.content_html, content_api::Recipe::Help);
        assert!(
            !purified.to_lowercase().contains("<script"),
            "净化后仍有 script：{purified}"
        );
        assert!(
            !purified.to_lowercase().contains("onerror"),
            "净化后仍有事件属性：{purified}"
        );
    })
    .await;
}

// ============================================================================
// P2 · AI Agent + 监控
// ============================================================================

/// AI-01 AI 用户不能绕过权限：AI 账号的权限 = 其绑定角色的权限，不因 `user_type=ROBOT` 放宽。
#[tokio::test]
async fn ai01_robot_user_gets_only_role_permissions() {
    let Some(()) = c::ready().await else { return };
    c::case("A01", |ctx| async move {
        let role = c::make_role(&ctx, "AI_LIMITED", 4).await;
        let robot = c::make_user(&ctx, "robot", "ROBOT").await;
        c::bind_role(robot, role).await;

        let tc = c::token_claims(robot, &ctx.key("robot"), &["business:task:list"]);
        assert!(tc.has_permission("business:task:list"));
        assert!(
            !tc.has_permission("business:erp:voucher:approve"),
            "AI 用户凭角色之外的权限码不应通过"
        );
        assert!(!tc.is_super_admin(), "AI 用户不该是超管");

        // 数据范围按其角色解析（4 = 仅本人），不比人类账号更宽
        let principal = principal::resolve_for_claims(&tc)
            .await
            .expect("解析数据范围");
        assert!(!principal.is_super(), "AI 用户被解析成了超管");
        assert_eq!(
            principal.kind(),
            scope::ScopeKind::SelfOnly,
            "AI 用户的范围应与角色一致（仅本人），实际 {:?}",
            principal.kind()
        );
    })
    .await;
}

/// AI-02 ★ AI 操作留痕：`ai_actor` 要记进 changelog（operator_type = ROBOT）。
#[tokio::test]
async fn ai02_robot_operation_traced_in_changelog() {
    let Some(()) = c::ready().await else { return };
    c::case("A02", |ctx| async move {
        let robot = c::make_user(&ctx, "robot", "ROBOT").await;
        // 表里冗余的 operator_type 由 users.user_type 推导（changelog::model::operator_type_of）
        let ty = changelog_model::operator_type_of(db(), Some(robot))
            .await
            .expect("推导操作人类型");
        assert_eq!(
            format!("{ty:?}"),
            "Robot",
            "ROBOT 账号被推导成 {ty:?}，审计里会与人类操作混在一起"
        );

        let id = Uuid::now_v7();
        let resource_id = ctx.key("obj");
        changelog_model::insert(
            db(),
            &changelog_model::NewChangeLog {
                id,
                request_id: Some(&ctx.key("req")),
                trace_source: "API",
                operator_id: Some(robot),
                operator_name: Some(&ctx.key("robot")),
                operator_type: "ROBOT",
                operator_dept_id: None,
                client_ip: Some("127.0.0.1"),
                module: "business",
                resource: "biz_task",
                resource_id: &resource_id,
                action: "UPDATE",
                action_label: "AI 改状态",
                diff: "{}".to_string(),
                diff_mode: "WHITELIST",
                changed_fields: &[],
                error_code: None,
                duration_ms: Some(12),
                created_at: c::now(),
            },
        )
        .await
        .expect("写入变更日志");

        let stored_type = c::text(
            "SELECT operator_type AS v FROM sys_change_log WHERE id = ?::uuid",
            vec![Value::from(id)],
        )
        .await;
        assert_eq!(stored_type, "ROBOT", "changelog 未记录 AI 操作人类型");
        assert_eq!(
            changelog_model::count_for_object(db(), "biz_task", &resource_id)
                .await
                .expect("按对象计数"),
            1
        );

        c::exec_with(
            "DELETE FROM sys_change_log WHERE id = ?::uuid",
            vec![Value::from(id)],
        )
        .await;
    })
    .await;
}

/// MO-01 监控接口返回正确：采集器在**真实 /proc** 上产出合法值，且降级项有明确状态。
///
/// # 为什么不停在 `model::collect()`
///
/// `collect()` 会读框架的全局配置资源（`nik_web::resources::cfg()`），该资源只在真正的
/// 服务进程里由 `App::new()` 初始化；集成测试进程里没有它，直接调用会 panic
/// 「配置未初始化」。所以本用例只验证**与配置无关的采集链**（解析器 + 真实 /proc 输入），
/// 整卡返回形状（数据库 up / Redis down 的降级）由 `scripts/verify/testd_http.sh` 在真服务上验。
#[tokio::test]
async fn mo01_monitor_collectors_on_real_proc() {
    let Some(()) = c::ready().await else { return };

    let stat = std::fs::read_to_string("/proc/stat").expect("读 /proc/stat");
    let first = monitor_model::parse_proc_stat(&stat).expect("解析 cpu 行");
    assert!(first.total > 0, "CPU 计数为 0（解析失效）");

    let meminfo = std::fs::read_to_string("/proc/meminfo").expect("读 /proc/meminfo");
    let mem = monitor_model::parse_meminfo(&meminfo).expect("解析内存");
    assert!(mem.total_bytes > 0, "内存总量为 0");
    assert!(mem.available_bytes <= mem.total_bytes, "可用内存大于总量");

    // 使用率换算：边界必须收敛在 0..100
    let usage = monitor_model::usage(100, 1000);
    assert_eq!(usage.percent, 10.0, "使用率换算错误：{}", usage.percent);
    let full = monitor_model::usage(2000, 1000);
    assert!(
        full.percent <= 100.0,
        "超量使用率未收敛到 100：{}",
        full.percent
    );

    // Redis INFO 解析（离线文本，不依赖 Redis 在跑）
    let info = monitor_model::parse_redis_info(
        "# Server\r\nredis_version:7.2.0\r\nused_memory:1048576\r\n",
    );
    assert_eq!(info.version, "7.2.0");
    assert_eq!(info.used_memory_bytes, 1048576);
}

// ============================================================================
// 引用别名（把模块路径收敛到一行，便于阅读）
// ============================================================================

use cloudpivot::business::changelog::model as changelog_model;
use cloudpivot::business::help::model as help_model;
use cloudpivot::business::monitor::model as monitor_model;
use cloudpivot::business::mutex::checker as mutex_checker;
use cloudpivot::business::mutex::guard as mutex_guard;
use cloudpivot::business::mutex::model as mutex_model;
use cloudpivot::business::shared::content as content_api;
use cloudpivot::business::shared::principal;
use cloudpivot::business::shared::scope;
