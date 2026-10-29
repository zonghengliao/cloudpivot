//! C4「帮助文档」真库集成测试：H-01~H-12（架构 §5.5.2）+ §3.2 的 TOC 锚点闭环。
//!
//! # 为什么要真库
//!
//! | 用例 | 只有真库能证的东西 |
//! |------|--------------------|
//! | H-01 分类深度 | 递归 CTE 的深度计算 + 移动后的「父深度 + 子树高度」判定 |
//! | H-02 分类非空 | 计数查询与软删顺序 |
//! | H-03 slug 唯一 | **部分唯一索引**（`WHERE deleted_at IS NULL`）真的生效，且软删后可复用 |
//! | H-04 发布必填时间 | `ck_sys_help_doc_publish` 真的会拒绝 |
//! | H-05 / H-06 | 事务内 `UPDATE` + `INSERT … SELECT` 快照的版本号连续性 |
//! | H-07 版本清理 | `DELETE … USING (SELECT max(version) …)` 删的确实是**最旧**的 |
//! | H-08 / H-09 | `ILIKE … ESCAPE '\'` 在 PG 里的真实语义 |
//!
//! # 用法（144 实测口径）
//!
//! ```bash
//! export C4_DB_URL="postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_test"
//! cargo test --test batchC_c4_help -- --nocapture --test-threads=1
//! ```
//!
//! 缺 `C4_DB_URL` 时全部用例打印 `SKIP` 并返回（`cargo test` 全绿，CI 不需要数据库）。
//!
//! # 夹具与清理
//!
//! 全部夹具的 `slug` 以 `c4-` 开头、分类 `code` 以 `c4-` 开头；用例开头与结尾各清一次，
//! 只删自己造的行（不会碰库里其它数据），可重复运行。

use alun::prelude::*;
use alun::uuid::Uuid;
use cloudpivot::business::help::model::{
    self, CategoryInput, DocFilter, DocInput, HelpError, SearchFilter,
};
use cloudpivot::business::shared::content;

/// 夹具前缀（slug / category code）。
const TAG: &str = "c4-";

/// 用例间串行锁。
///
/// # 为什么需要
///
/// `cargo test` 默认**并行**跑同一个二进制里的用例，而本文件的 11 个用例共用
/// 一个全局 `db()` 与同一批 `c4-` 前缀夹具：并行跑会互相删掉对方的分类
/// （实测：不串行时 7 个用例失败，全部是「分类/文档不存在」）。
/// 用一把静态异步锁把每个用例的**整个生命周期**串起来，比给每张表加随机后缀更稳
/// （随机后缀仍会争抢 schema 上的递归 CTE 与全局计数）。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 取得串行锁（用例第一行调用）。
async fn serial_guard() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL.lock().await
}

/// 初始化全局数据库（进程内只初始化一次）；返回 `false` 表示跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("C4_DB_URL") else {
                println!("SKIP: 未设置 C4_DB_URL（不连真实数据库）");
                return false;
            };
            match Db::connect(&url).await {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    println!("C4 已连接验证库");
                    true
                }
                Err(error) => {
                    println!("SKIP: 连接数据库失败：{error}");
                    false
                }
            }
        })
        .await
}

/// 清掉本文件的全部夹具（只删 `c4-` 前缀）。
///
/// 调用方必须已持有 [`SERIAL`] 锁（本函数**不自取锁**，否则同一任务内会死锁）。
async fn cleanup() {
    if !ensure_db().await {
        return;
    }
    let doc_rows = db()
        .execute_with_params(
            "DELETE FROM sys_help_doc WHERE slug LIKE ?::text",
            vec![Value::from(format!("{TAG}%"))],
        )
        .await
        .unwrap_or(0);
    let cat_rows = db()
        .execute_with_params(
            "DELETE FROM sys_help_category WHERE code LIKE ?::text",
            vec![Value::from(format!("{TAG}%"))],
        )
        .await
        .unwrap_or(0);
    println!("清理夹具：doc={doc_rows} category={cat_rows}");
}

/// 建一个分类（`code` 自动加前缀与后缀）。
async fn make_category(suffix: &str, parent_id: Option<Uuid>) -> model::CategoryRow {
    let input = CategoryInput {
        parent_id,
        name: format!("C4 分类 {suffix}"),
        code: format!("{TAG}{suffix}"),
        icon: None,
        description: Some("集成测试夹具".into()),
        sort_order: 0,
        status: 1,
    };
    model::create_category(&input)
        .await
        .expect("建分类应当成功")
}

/// 建一篇文档（草稿）。
async fn make_doc(category_id: Uuid, suffix: &str, content_md: &str) -> model::DocDetailRow {
    let input = DocInput {
        category_id,
        title: format!("C4 文档 {suffix}"),
        slug: format!("{TAG}doc-{suffix}"),
        summary: None,
        content_md: content_md.to_string(),
        keywords: Some("c4,测试".into()),
        sort_order: 0,
    };
    model::create_doc(&input, None)
        .await
        .expect("建文档应当成功")
}

/// H-01 分类深度：第 4 层 → [`HelpError::DepthExceeded`]；移动导致超深 → 同错误。
#[tokio::test]
async fn h01_category_depth_limit() {
    let _guard = serial_guard().await;
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    let l1 = make_category("h01-l1", None).await;
    let l2 = make_category("h01-l2", Some(l1.id)).await;
    let l3 = make_category("h01-l3", Some(l2.id)).await;

    assert_eq!(model::category_depth(&l1.id).await.unwrap(), Some(1));
    assert_eq!(model::category_depth(&l2.id).await.unwrap(), Some(2));
    assert_eq!(model::category_depth(&l3.id).await.unwrap(), Some(3));

    // 第 4 层：拒绝
    let too_deep = model::create_category(&CategoryInput {
        parent_id: Some(l3.id),
        name: "第 4 层".into(),
        code: format!("{TAG}h01-l4"),
        icon: None,
        description: None,
        sort_order: 0,
        status: 1,
    })
    .await;
    assert!(
        matches!(too_deep, Err(HelpError::DepthExceeded)),
        "第 4 层必须 422 + 62010，实际：{too_deep:?}"
    );

    // 移动导致超深：另建一条 2 层子树（x1→x2），把它挪到第 3 层的 l3 下面
    // → 深度 = 3 + 2 > 3，必须拒绝（注意不能用 l1 自己，那会先命中成环检查）
    let x1 = make_category("h01-x1", None).await;
    let _x2 = make_category("h01-x2", Some(x1.id)).await;
    let moved = model::update_category(
        &x1.id,
        &CategoryInput {
            parent_id: Some(l3.id),
            name: "C4 分类 h01-x1".into(),
            code: format!("{TAG}h01-x1"),
            icon: None,
            description: None,
            sort_order: 0,
            status: 1,
        },
    )
    .await;
    assert!(
        matches!(moved, Err(HelpError::DepthExceeded)),
        "移动导致超深必须同样拒绝，实际：{moved:?}"
    );

    // 读到自己的子树下 → 环，拒绝（l2 在 l1 的子树里）
    let cycle = model::update_category(
        &l1.id,
        &CategoryInput {
            parent_id: Some(l2.id),
            name: "C4 分类 h01-l1".into(),
            code: format!("{TAG}h01-l1"),
            icon: None,
            description: None,
            sort_order: 0,
            status: 1,
        },
    )
    .await;
    assert!(
        matches!(cycle, Err(HelpError::CategoryMoveCycle)),
        "移动到自己的子树必须拒绝，实际：{cycle:?}"
    );

    println!("[PASS] H-01 分类深度：第 4 层 62010 / 移动超深 62010 / 成环拒绝");
    cleanup().await;
}

/// H-02 分类非空：分类下有文档时删除 → 62011，且分类仍在。
#[tokio::test]
async fn h02_category_not_empty() {
    let _guard = serial_guard().await;
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    let cat = make_category("h02", None).await;
    let doc = make_doc(cat.id, "h02", "## 正文\n\n内容").await;

    let deleted = model::delete_category(&cat.id).await;
    assert!(
        matches!(deleted, Err(HelpError::CategoryNotEmpty)),
        "有文档时必须 409 + 62011，实际：{deleted:?}"
    );
    let still_there = model::find_category(&cat.id).await.unwrap();
    assert!(still_there.is_some(), "删除失败后分类必须仍然存在（H-02）");

    // 文档删掉（软删）之后分类可删
    model::delete_doc(&doc.id).await.expect("软删文档");
    model::delete_category(&cat.id)
        .await
        .expect("文档删除后分类应可删");
    println!("[PASS] H-02 分类非空：62011 且分类仍在；文档软删后可删");
    cleanup().await;
}

/// H-03 slug 唯一：重复 → 409 冲突；格式非法 → 内容非法（DB CHECK 兜底）。
#[tokio::test]
async fn h03_slug_unique_and_format() {
    let _guard = serial_guard().await;
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    let cat = make_category("h03", None).await;
    let _doc = make_doc(cat.id, "h03", "## 正文\n\n内容").await;

    let duplicate = model::create_doc(
        &DocInput {
            category_id: cat.id,
            title: "重复 slug".into(),
            slug: format!("{TAG}doc-h03"),
            summary: None,
            content_md: "重复".into(),
            keywords: None,
            sort_order: 0,
        },
        None,
    )
    .await;
    assert!(
        matches!(duplicate, Err(HelpError::DocSlugConflict)),
        "重复 slug 必须 409，实际：{duplicate:?}"
    );

    for bad_slug in ["C4-Upper", "c4 with space", "-c4-leading", "c4_underscore"] {
        let invalid = model::create_doc(
            &DocInput {
                category_id: cat.id,
                title: "非法 slug".into(),
                slug: bad_slug.to_string(),
                summary: None,
                content_md: "x".into(),
                keywords: None,
                sort_order: 0,
            },
            None,
        )
        .await;
        assert!(
            matches!(invalid, Err(HelpError::DocContentInvalid)),
            "非法 slug（{bad_slug}）必须 400 内容非法，实际：{invalid:?}"
        );
    }

    // 软删后同一 slug 可复用（部分唯一索引 WHERE deleted_at IS NULL）
    let first = model::find_doc_by_slug(&format!("{TAG}doc-h03")).await.unwrap();
    let first = first.expect("应能按 slug 找到");
    model::delete_doc(&first.id).await.expect("软删");
    let reused = make_doc(cat.id, "h03", "## 复用\n\n同 slug 重建").await;
    assert_eq!(reused.slug, format!("{TAG}doc-h03"));

    println!("[PASS] H-03 slug：重复 409 / 非法 400 / 软删后可复用");
    cleanup().await;
}

/// H-04 发布必填时间：`status='PUBLISHED'` 而 `published_at` 为 NULL → DB 拒绝。
#[tokio::test]
async fn h04_publish_requires_published_at() {
    let _guard = serial_guard().await;
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    let cat = make_category("h04", None).await;
    let doc = make_doc(cat.id, "h04", "## 正文\n\n内容").await;

    let raw = db()
        .execute_with_params(
            "UPDATE sys_help_doc SET status = 'PUBLISHED' WHERE id = ?::uuid",
            vec![Value::from(&doc.id)],
        )
        .await;
    assert!(
        raw.is_err(),
        "ck_sys_help_doc_publish 必须拒绝「已发布但无发布时间」，实际：{raw:?}"
    );

    // 正常发布路径（应用层同时写 published_at）必须成功
    let published = model::publish_doc(&doc.id, None, Some("首次发布"))
        .await
        .expect("正常发布应成功");
    assert!(published.published_at.is_some(), "发布必须写 published_at");

    println!("[PASS] H-04 发布必填时间：DB 拒绝裸改状态；正常发布成功");
    cleanup().await;
}

/// H-05 版本快照：连发 3 版 → 3 行，版本 1/2/3，内容各自正确。
#[tokio::test]
async fn h05_version_snapshots() {
    let _guard = serial_guard().await;
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    let cat = make_category("h05", None).await;
    let doc = make_doc(cat.id, "h05", "## 第零版\n\n初始内容").await;

    for round in 1..=3 {
        model::publish_doc(&doc.id, None, Some(&format!("第 {round} 次发布")))
            .await
            .expect("发布应当成功");
    }
    let versions = model::list_versions(&doc.id).await.unwrap();
    assert_eq!(versions.len(), 3, "连发 3 版必须正好 3 行快照");
    let numbers: Vec<i32> = versions.iter().map(|v| v.version).collect();
    assert_eq!(numbers, vec![3, 2, 1], "版本号必须 1/2/3（列表倒序）");
    for version in &versions {
        let detail = model::find_version(&doc.id, version.version)
            .await
            .unwrap()
            .expect("版本应存在");
        assert!(
            !detail.content_md.is_empty(),
            "版本 {} 的正文不能为空",
            version.version
        );
    }
    // 每条快照的 change_note 与内容是配套的
    let v1 = model::find_version(&doc.id, 1).await.unwrap().unwrap();
    assert_eq!(v1.content_md, "## 第零版\n\n初始内容");

    println!("[PASS] H-05 版本快照：3 行 / 版本 1,2,3 / 内容各自正确");
    cleanup().await;
}

/// H-06 回滚：改动 3 次 → 回滚到 v1 → 内容 = v1 且 `version = 4`，历史仍在。
#[tokio::test]
async fn h06_rollback_creates_new_version() {
    let _guard = serial_guard().await;
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    let cat = make_category("h06", None).await;
    let doc = make_doc(cat.id, "h06", "## 原始版本\n\n第一版内容 V1").await;
    model::publish_doc(&doc.id, None, Some("发布 v1"))
        .await
        .expect("发布 v1");

    // 改 3 次（每次保存都会 +1 版并留快照）
    for round in 2..=4 {
        model::save_doc(
            &doc.id,
            &DocInput {
                category_id: cat.id,
                title: format!("C4 文档 h06"),
                slug: format!("{TAG}doc-h06"),
                summary: None,
                content_md: format!("## 第 {round} 版\n\n内容 V{round}"),
                keywords: None,
                sort_order: 0,
            },
            None,
            Some(&format!("保存 v{round}")),
        )
        .await
        .expect("保存应当成功");
        model::publish_doc(&doc.id, None, Some(&format!("发布 v{round}")))
            .await
            .expect("发布应当成功");
    }

    let before = model::find_doc(&doc.id).await.unwrap().expect("文档存在");
    let history_before = model::list_versions(&doc.id).await.unwrap();
    println!(
        "回滚前：version={} 历史版本={:?}",
        before.version,
        history_before.iter().map(|v| v.version).collect::<Vec<_>>()
    );

    let rolled = model::rollback_doc(&doc.id, 1, None, None)
        .await
        .expect("回滚应当成功");
    assert_eq!(
        rolled.content_md, "## 原始版本\n\n第一版内容 V1",
        "回滚后内容必须等于 v1"
    );
    assert_eq!(
        rolled.version,
        before.version + 1,
        "回滚必须生成新版本（H-06：version + 1）"
    );
    let rolled_html = rolled.content_html;
    assert!(
        rolled_html.contains("第一版内容 V1"),
        "回滚后的 content_html 也必须来自 v1：{rolled_html}"
    );

    let history_after = model::list_versions(&doc.id).await.unwrap();
    let numbers_after: Vec<i32> = history_after.iter().map(|v| v.version).collect();
    assert_eq!(
        history_after.len(),
        history_before.len() + 1,
        "回滚只允许新增一行（历史一行不删）"
    );
    for version in &history_before {
        assert!(
            numbers_after.contains(&version.version),
            "历史版本 {} 必须仍然存在",
            version.version
        );
    }

    println!(
        "[PASS] H-06 回滚：内容回到 v1，version {} → {}，历史 {:?} → {:?}",
        before.version,
        rolled.version,
        history_before.iter().map(|v| v.version).collect::<Vec<_>>(),
        numbers_after
    );
    cleanup().await;
}

/// H-07 版本清理：造 25 版 → 清理 → 只剩 20 版，删的是最旧的。
#[tokio::test]
async fn h07_version_cleanup_keeps_latest_20() {
    let _guard = serial_guard().await;
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    let cat = make_category("h07", None).await;
    let doc = make_doc(cat.id, "h07", "## 初版\n\n内容 0").await;
    for round in 1..=25 {
        model::publish_doc(&doc.id, None, Some(&format!("发布 {round}")))
            .await
            .expect("发布应当成功");
    }
    let before = model::list_versions(&doc.id).await.unwrap();
    assert_eq!(before.len(), 25, "造数后应有 25 个版本");

    let deleted = model::cleanup_versions(&doc.id, model::KEEP_VERSIONS)
        .await
        .expect("清理应当成功");
    let after = model::list_versions(&doc.id).await.unwrap();
    assert_eq!(deleted, 5, "25 - 20 = 删 5 条");
    assert_eq!(after.len(), 20, "清理后只剩 20 版");
    let numbers: Vec<i32> = after.iter().map(|v| v.version).collect();
    assert_eq!(numbers.first(), Some(&25), "最新版必须保留");
    assert_eq!(numbers.last(), Some(&6), "最旧的 5 条（1..5）必须删掉");

    println!("[PASS] H-07 版本清理：25 → 20，删除 1..5，保留 6..25");
    cleanup().await;
}

/// H-08 检索转义：`%` / `_` / `\` 不全表命中、不报错。
#[tokio::test]
async fn h08_search_escapes_wildcards() {
    let _guard = serial_guard().await;
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    let cat = make_category("h08", None).await;
    // 三篇：一篇含字面量 "100%"，一篇含 "a_b"，一篇完全无关
    let percent = make_doc(cat.id, "h08-percent", "## 折扣\n\n满 100% 返现说明").await;
    model::publish_doc(&percent.id, None, None).await.unwrap();
    let underscore = make_doc(cat.id, "h08-underscore", "## 命名\n\n变量 a_b 的含义").await;
    model::publish_doc(&underscore.id, None, None).await.unwrap();
    let plain = make_doc(cat.id, "h08-plain", "## 无关\n\n这里没有任何通配符").await;
    model::publish_doc(&plain.id, None, None).await.unwrap();

    let total = model::search(
        &SearchFilter {
            terms: vec![],
            published_only: true,
            ..Default::default()
        },
        1,
        50,
    )
    .await
    .unwrap();
    assert_eq!(total.total, 0, "空关键词必须返回空页（不查库、不全表命中）");

    // 搜 "%"：只能命中含字面 % 的那一篇
    let percent_hits = model::search(
        &SearchFilter {
            terms: model::split_terms("%"),
            published_only: true,
            ..Default::default()
        },
        1,
        50,
    )
    .await
    .expect("搜 % 不能报错");
    let hit_slugs: Vec<String> = percent_hits.list.iter().map(|r| r.slug.clone()).collect();
    assert!(
        hit_slugs.contains(&format!("{TAG}doc-h08-percent")),
        "含字面 % 的文档必须命中：{hit_slugs:?}"
    );
    assert!(
        !hit_slugs.contains(&format!("{TAG}doc-h08-plain")),
        "不含 % 的文档不能命中（否则就是全表命中）：{hit_slugs:?}"
    );

    // 搜 "_"：只命中 a_b
    let underscore_hits = model::search(
        &SearchFilter {
            terms: model::split_terms("a_b"),
            published_only: true,
            ..Default::default()
        },
        1,
        50,
    )
    .await
    .expect("搜 a_b 不能报错");
    let underscore_slugs: Vec<String> =
        underscore_hits.list.iter().map(|r| r.slug.clone()).collect();
    assert_eq!(
        underscore_slugs,
        vec![format!("{TAG}doc-h08-underscore")],
        "a_b 只能命中真含 a_b 的那篇"
    );

    // 反斜杠：不 panic、不报错
    let backslash = model::search(
        &SearchFilter {
            terms: model::split_terms("\\"),
            published_only: true,
            ..Default::default()
        },
        1,
        50,
    )
    .await;
    assert!(backslash.is_ok(), "搜反斜杠不能报错：{backslash:?}");

    println!(
        "[PASS] H-08 检索转义：% 命中 {percent_hits_total} 条、a_b 精确 1 条、反斜杠不报错",
        percent_hits_total = percent_hits.total
    );
    cleanup().await;
}

/// H-09 检索只含已发布：DRAFT 里的词普通用户 0 命中；带 `status=DRAFT` 命中。
#[tokio::test]
async fn h09_search_respects_publish_state() {
    let _guard = serial_guard().await;
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    let cat = make_category("h09", None).await;
    let draft = make_doc(cat.id, "h09-draft", "## 草稿\n\n内含独特词 coconut-draft").await;
    assert_eq!(draft.status, "DRAFT");

    let published_only = model::search(
        &SearchFilter {
            terms: model::split_terms("coconut-draft"),
            published_only: true,
            ..Default::default()
        },
        1,
        20,
    )
    .await
    .unwrap();
    assert_eq!(published_only.total, 0, "草稿对普通用户必须 0 命中");

    let admin_view = model::search(
        &SearchFilter {
            terms: model::split_terms("coconut-draft"),
            status: Some("DRAFT".into()),
            published_only: false,
            ..Default::default()
        },
        1,
        20,
    )
    .await
    .unwrap();
    assert_eq!(admin_view.total, 1, "管理员带 status=DRAFT 必须命中");
    assert_eq!(admin_view.list[0].id, draft.id);

    println!("[PASS] H-09 检索状态过滤：普通用户 0 命中 / 管理员 status=DRAFT 命中 1");
    cleanup().await;
}

/// H-10 / H-11 / H-12：片段纯文本、TOC 锚点存在且唯一。
#[tokio::test]
async fn h10_h11_h12_snippet_and_toc() {
    let _guard = serial_guard().await;
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    let cat = make_category("h10", None).await;
    let md = "## 概述\n\n第一段提到关键词 baseplate 与 <script>alert(1)</script>。\n\n\
              ### 细节\n\n第二段。\n\n## 概述\n\n同名标题两。\n\n## 概述\n\n同名标题三。\n";
    let doc = make_doc(cat.id, "h10", md).await;
    model::publish_doc(&doc.id, None, None).await.unwrap();

    let filter = DocFilter {
        category_id: Some(cat.id),
        include_children: false,
        keyword: None,
        status: None,
        published_only: true,
    };
    let page = model::page_docs(&filter, 1, 10).await.unwrap();
    assert_eq!(page.total, 1, "列表应能看到这篇已发布文档");

    // H-10：snippet 不含 < / >
    let hits = model::search(
        &SearchFilter {
            terms: model::split_terms("baseplate"),
            published_only: true,
            ..Default::default()
        },
        1,
        10,
    )
    .await
    .unwrap();
    assert_eq!(hits.total, 1);
    let plain = content::to_plain_text(&hits.list[0].content_md);
    let (snippet, matched) = model::make_snippet(&plain, &["baseplate".to_string()]);
    assert!(!snippet.contains('<'), "H-10：片段不能含 <：{snippet}");
    assert!(!snippet.contains('>'), "H-10：片段不能含 >：{snippet}");
    assert!(snippet.contains("baseplate"), "片段必须围绕命中：{snippet}");
    assert_eq!(matched, vec!["baseplate".to_string()]);

    // H-11 / H-12：TOC 两级嵌套 + 同名标题 id 唯一 + id 真的出现在 HTML 里
    let rendered = content::render_help(md);
    let toc = &rendered.toc;
    assert_eq!(toc.len(), 3, "三个 ## 应产生三个顶层 TOC 项：{toc:?}");
    assert_eq!(toc[0].children.len(), 1, "概述下的 ### 应挂成子节点");
    let mut ids: Vec<String> = Vec::new();
    fn collect(nodes: &[content::TocNode], ids: &mut Vec<String>) {
        for node in nodes {
            ids.push(node.id.clone());
            collect(&node.children, ids);
        }
    }
    collect(toc, &mut ids);
    let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "H-12：同名标题的 id 不能重复：{ids:?}");
    for id in &ids {
        assert!(
            rendered.content_html.contains(&format!("id=\"{id}\"")),
            "H-11：TOC 的 id {id} 必须出现在 content_html 里（锚点可用）：{}",
            rendered.content_html
        );
    }
    // 落库的 content_html 与现渲染一致（写路径走的是同一个渲染入口）
    let stored = model::find_doc(&doc.id).await.unwrap().unwrap();
    assert!(
        stored.content_html.contains(&format!("id=\"{}\"", ids[0])),
        "落库快照必须带锚点 id：{}",
        stored.content_html
    );

    println!(
        "[PASS] H-10/11/12：片段纯文本、TOC {} 项（含 {} 个子节点）、id 唯一且全部命中 HTML",
        toc.len(),
        toc[0].children.len()
    );
    cleanup().await;
}

/// 检索性能：2000 篇造数下 `ILIKE` 检索耗时（H 的 §7 C4 行要求 < 100 ms）。
///
/// 该用例造 2000 行后会**保留**数据供调用方观察执行计划，末尾统一清理；
/// 想跳过就设 `C4_SKIP_PERF=1`。
#[tokio::test]
async fn search_performance_2000_docs() {
    let _guard = serial_guard().await;
    if !ensure_db().await {
        return;
    }
    if std::env::var("C4_SKIP_PERF").is_ok() {
        println!("SKIP: C4_SKIP_PERF 已设置");
        return;
    }
    cleanup().await;
    let cat = make_category("perf", None).await;

    // 批量造 2000 篇（直接 SQL，避免 2000 次渲染；正文各 1.5 KB 左右）
    let inserted = db()
        .execute_with_params(
            "INSERT INTO sys_help_doc \
                 (category_id, title, slug, summary, content_md, content_html, keywords, \
                  version, status, sort_order, view_count, published_at) \
             SELECT ?::uuid, 'C4 性能文档 ' || g, 'c4-perf-' || g, '压测夹具', \
                    repeat('第 ' || g || ' 篇的性能测试正文，包含关键字 基座 与 公告 。', 40), '', \
                    'perf', 1, 'PUBLISHED', g, 0, now() - (g || ' minutes')::interval \
               FROM generate_series(1, 2000) AS g",
            vec![Value::from(&cat.id)],
        )
        .await
        .expect("批量造数应当成功");
    println!("造数：{inserted} 行");

    let mut worst = 0u128;
    for round in 0..5 {
        let started = std::time::Instant::now();
        let hits = model::search(
            &SearchFilter {
                terms: model::split_terms("基座"),
                published_only: true,
                ..Default::default()
            },
            1,
            20,
        )
        .await
        .expect("检索应当成功");
        let elapsed = started.elapsed().as_millis();
        worst = worst.max(elapsed);
        println!(
            "第 {} 轮：{} ms（命中 {} 条）",
            round + 1,
            elapsed,
            hits.total
        );
        assert_eq!(hits.list.len(), 20, "分页大小应为 20");
    }
    assert!(worst < 100, "2000 篇检索最坏耗时应 < 100 ms，实测 {worst} ms");
    println!("[PASS] 检索性能：2000 篇最坏 {worst} ms（阈值 100 ms）");
    cleanup().await;
}
