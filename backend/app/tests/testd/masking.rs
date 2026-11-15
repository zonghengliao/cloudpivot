//! 测试D · 列级脱敏在「详情 / 导出 / 异步导出」里的生效性（X02–X05）。
//!
//! 与测试C 的列表接口脱敏互补：这里验的是**另外三条出口**，任何一条漏掉都等于脱敏失效。
//! 脱敏规则落 `sys_field_mask`（按角色命中），用户的角色决定他看到明文还是掩码。

use axum::body::to_bytes;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;
use std::collections::HashMap;

use cloudpivot::business::impexp::dto::ExportSyncQuery;
use cloudpivot::business::impexp::handler as impexp_handler;
use cloudpivot::business::masking::model as mask_model;
use cloudpivot::system::user::handler as user_handler;

use crate::testd::common::{self as c, Ctx};

/// 给角色配三条规则（姓名 4 / 邮箱 3 / 手机 1）。
async fn seed_rules(ctx: &Ctx, role: Uuid) {
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
    // 规则有进程级缓存：写完立刻失效，避免读到空集
    let _ = mask_model::invalidate_all().await;
}

/// 造一个用户并给敏感字段填值。
async fn make_target(ctx: &Ctx, name: &str) -> Uuid {
    let id = c::make_user(ctx, name, "HUMAN").await;
    c::exec_with(
        "UPDATE sys_user SET real_name = ?, email = ?, mobile = ? WHERE id = ?::uuid",
        vec![
            Value::Text("张三丰".to_string()),
            Value::Text("zhangsan@example.com".to_string()),
            Value::Text("13812345678".to_string()),
            Value::from(&id),
        ],
    )
    .await;
    id
}

// ============================================================================
// X02 详情接口脱敏
// ============================================================================

/// X02 ★ 详情接口脱敏：无豁免的用户看详情，三个敏感字段都必须是掩码。
#[tokio::test]
async fn x02_detail_masked_for_plain_user() {
    let Some(()) = c::ready().await else { return };
    c::case("X02", |ctx| async move {
        let role = c::make_role(&ctx, "VIEWER", 1).await;
        let viewer = c::make_user(&ctx, "viewer", "HUMAN").await;
        c::bind_role(viewer, role).await;
        seed_rules(&ctx, role).await;
        let target = make_target(&ctx, "target").await;

        let detail = user_handler::detail(
            Path(target.to_string()),
            c::claims(viewer, "viewer", &["system:user:list"]),
        )
        .await
        .expect("查详情");
        let detail = detail.data.expect("详情 data");

        assert_eq!(detail.mobile, "138****5678", "手机号未脱敏：{}", detail.mobile);
        assert_eq!(detail.real_name, "张**", "姓名未脱敏：{}", detail.real_name);
        assert!(detail.email.starts_with("z***@"), "邮箱未脱敏：{}", detail.email);
        assert!(
            !detail.mobile.contains("1234") && !detail.real_name.contains("三丰"),
            "详情里仍能看到明文片段"
        );
    })
    .await;
}

// ============================================================================
// X03 导出文件脱敏（同步导出）
// ============================================================================

/// X03 ★ 导出文件脱敏：CSV/XLSX 产物里必须是掩码值，不能是明文。
#[tokio::test]
async fn x03_sync_export_masked() {
    let Some(()) = c::ready().await else { return };
    c::case("X03", |ctx| async move {
        let role = c::make_role(&ctx, "EXPORTER", 1).await;
        let exporter = c::make_user(&ctx, "exporter", "HUMAN").await;
        c::bind_role(exporter, role).await;
        seed_rules(&ctx, role).await;
        let target = make_target(&ctx, "target").await;
        let target_name = c::text(
            "SELECT username AS v FROM sys_user WHERE id = ?::uuid",
            vec![Value::from(&target)],
        )
        .await;

        let response = impexp_handler::export_sync(
            c::claims(exporter, "exporter", &[]),
            Query(ExportSyncQuery {
                biz_type: "system.user".to_string(),
                keyword: Some(format!("%{}%", ctx.key("target"))),
                status: None,
                status_code: None,
            }),
        )
        .await;
        assert_eq!(response.status(), 200, "导出应成功（HTTP {}）", response.status());
        let body = to_bytes(response.into_body(), usize::MAX).await.expect("读产物");

        let cells = crate::testd::xlsx::read_xlsx(&body);
        let flat: String = cells.iter().flatten().cloned().collect::<Vec<_>>().join("|");
        assert!(flat.contains(&target_name), "导出文件里没有夹具用户：{flat}");
        assert!(
            !flat.contains("13812345678"),
            "导出文件里出现了明文手机号（脱敏失效）：{flat}"
        );
        assert!(
            !flat.contains("zhangsan@example.com"),
            "导出文件里出现了明文邮箱（脱敏失效）：{flat}"
        );
        assert!(!flat.contains("张三丰"), "导出文件里出现了明文姓名（脱敏失效）");
        assert!(flat.contains("138****5678"), "导出文件里没有掩码手机号：{flat}");

        // 导出任务行必须留痕 mask_applied（审计）
        println!("[X03] 同步导出掩码值命中：138****5678");
    })
    .await;
}

// ============================================================================
// X04 敏感列缺规则 → fail-closed
// ============================================================================

/// X04 ★ fail-closed：敏感列没有生效规则时**拒绝导出**（绝不回退明文）。
#[tokio::test]
async fn x04_missing_rule_fails_closed() {
    let Some(()) = c::ready().await else { return };
    c::case("X04", |ctx| async move {
        // 角色**不配**任何脱敏规则
        let role = c::make_role(&ctx, "NO_RULE", 1).await;
        let exporter = c::make_user(&ctx, "exporter", "HUMAN").await;
        c::bind_role(exporter, role).await;
        let _target = make_target(&ctx, "target").await;

        // ① 规则解析：未豁免、规则集为空
        let principal = cloudpivot::business::shared::principal::resolve(&exporter)
            .await
            .expect("解析范围");
        assert!(!principal.is_super(), "夹具用户被解析成超管");
        let claims = c::token_claims(exporter, "exporter", &[]);
        let mask = cloudpivot::business::masking::resolve(Some(&principal), Some(&claims))
            .await
            .expect("解析脱敏规则");
        assert!(!mask.is_exempt(), "无明版权限却被判成豁免");
        // 本角色没有配规则；若不为空，只能来自库里的全局规则（role_id IS NULL）
        println!("[X04] 解析到的规则数：{}", mask.len());

        // ② 引擎层：敏感列无规则 → Err(MissingRule)（就是 422 + 61008 的来源）
        //    用**合成列**钉住 fail-closed 本身，不受库里既有全局规则的影响
        static SYNTHETIC: &[cloudpivot::business::impexp::definition::ExportColumn] =
            &[cloudpivot::business::impexp::definition::ExportColumn {
                key: "secret",
                header: "机密列",
                kind: cloudpivot::business::impexp::definition::ColumnKind::Text { max_len: None },
                width: None,
                table: "sys_user",
                sensitive: true,
            }];
        let mut synthetic_row: HashMap<&'static str, String> = HashMap::new();
        synthetic_row.insert("secret", "绝密内容".to_string());
        // 一个「不命中任何规则」的空集合（未豁免）
        let none = cloudpivot::business::masking::MaskSet::empty();
        let error = cloudpivot::business::impexp::engine::format_export_rows(
            SYNTHETIC,
            &[synthetic_row],
            &HashMap::new(),
            &none,
        )
        .expect_err("敏感列无规则时格式化必须失败");
        println!("[X04] 引擎层拒绝：{error:?}");
        assert_eq!(error.http_status(), 422, "fail-closed 应映射 422");

        // 库里是否配了全局规则（role_id IS NULL）会影响 ③ 的实际走向，打印出来便于报告引用
        let globals = c::count(
            "SELECT count(*) AS n FROM sys_field_mask WHERE role_id IS NULL AND deleted_at IS NULL",
            Vec::new(),
        )
        .await;
        println!("[X04] 库里全局脱敏规则数：{globals}，本角色规则数：{}", mask.len());
        let definition = cloudpivot::business::impexp::global_registry()
            .export_definition("system.user")
            .expect("用户导出定义");
        assert!(!definition.columns().is_empty(), "用户导出定义没有列");

        // ③ 接口层：要么直接 422，要么（若返回文件）文件里**绝不能有明文**
        let response = impexp_handler::export_sync(
            c::claims(exporter, "exporter", &[]),
            Query(ExportSyncQuery {
                biz_type: "system.user".to_string(),
                keyword: Some(format!("%{}%", ctx.key("target"))),
                status: None,
                status_code: None,
            }),
        )
        .await;
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.expect("读响应");
        if status == 200 {
            let text = String::from_utf8_lossy(&body).to_string();
            let cells = crate::testd::xlsx::read_xlsx(&body);
            let flat = cells.iter().flatten().cloned().collect::<Vec<_>>().join("|");
            println!("[X04] 接口层返回 200（body {} 字节），单元格：{flat}", text.len());
            assert!(
                !flat.contains("13812345678") && !flat.contains("zhangsan@example.com"),
                "无脱敏规则却导出了明文（fail-closed 被绕过）：{flat}"
            );
        } else {
            assert_eq!(status, 422, "接口层应为 422，实际 {status}");
        }
    })
    .await;
}

// ============================================================================
// X05 明版权限豁免 + 留痕
// ============================================================================

/// X05 明版权限（`system:field-mask:plain`）：导出是明文，且每次访问落一条审计。
#[tokio::test]
async fn x05_plain_permission_exempt_and_audited() {
    let Some(()) = c::ready().await else { return };
    c::case("X05", |ctx| async move {
        let role = c::make_role(&ctx, "PLAIN_READER", 1).await;
        let user = c::make_user(&ctx, "plain", "HUMAN").await;
        c::bind_role(user, role).await;
        seed_rules(&ctx, role).await;
        let _target = make_target(&ctx, "target").await;

        let before = c::count(
            "SELECT count(*) AS n FROM audit_logs WHERE operation = 'FIELD_MASK_PLAIN_READ'",
            Vec::new(),
        )
        .await;
        // 留痕「彻底失败」的进程级计数基线（本用例结束时必须没有增长）
        let failures_before = mask_model::plain_read_audit_failures();

        let response = impexp_handler::export_sync(
            c::claims(user, "plain", &["system:field-mask:plain"]),
            Query(ExportSyncQuery {
                biz_type: "system.user".to_string(),
                keyword: Some(format!("%{}%", ctx.key("target"))),
                status: None,
                status_code: None,
            }),
        )
        .await;
        assert_eq!(response.status(), 200, "明版权限导出应成功");
        let body = to_bytes(response.into_body(), usize::MAX).await.expect("读产物");
        let flat: String = crate::testd::xlsx::read_xlsx(&body)
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join("|");
        assert!(
            flat.contains("13812345678"),
            "持明版权限却仍未看到明文（豁免失效）：{flat}"
        );

        // 明文访问的留痕是 `tokio::spawn` 异步写的：轮询等它落库（最多 5 s）。
        // 修前这里永远等不到 —— 写入函数把 `field-mask:<uuid>`（47 字符）塞进
        // `audit_logs.request_id VARCHAR(36)`，每次都 `value too long`，而调用处
        // 只打一行日志 ⇒ 明文访问静默无痕（BUG-D-13）。
        let mut after = before;
        let mut waited = 0u64;
        while waited < 5_000 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            waited += 100;
            after = c::count(
                "SELECT count(*) AS n FROM audit_logs WHERE operation = 'FIELD_MASK_PLAIN_READ'",
                Vec::new(),
            )
            .await;
            if after > before {
                break;
            }
        }
        assert!(
            after > before,
            "明文导出没有留痕：audit_logs.operation='FIELD_MASK_PLAIN_READ' 计数 {before} → {after}（等了 {waited} ms）"
        );

        // 留痕内容要能定位「谁、从哪条路径看的」：detail 是合法 JSON，target_id 是用户 id
        let detail = c::opt_text(
            "SELECT detail::text AS v FROM audit_logs WHERE operation = 'FIELD_MASK_PLAIN_READ' \
             AND user_id = ?::uuid ORDER BY created_at DESC LIMIT 1",
            vec![Value::from(&user)],
        )
        .await
        .expect("明文留痕行缺 user_id");
        let parsed: serde_json::Value = serde_json::from_str(&detail).expect("detail 不是合法 JSON");
        assert_eq!(
            parsed.get("event").and_then(|v| v.as_str()),
            Some("FIELD_MASK_PLAIN_READ"),
            "detail 里缺 event 标识：{detail}"
        );

        // 写入函数本身也必须能直接调通（区分「没触发」与「写不进去」）
        let direct = mask_model::insert_plain_read_audit(
            &user,
            &serde_json::json!({"event": "FIELD_MASK_PLAIN_READ", "route": "testd"}).to_string(),
        )
        .await;
        assert!(direct.is_ok(), "留痕写入函数在真库上失败（BUG-D-13 回归）：{direct:?}");
        let now_count = c::count(
            "SELECT count(*) AS n FROM audit_logs WHERE operation = 'FIELD_MASK_PLAIN_READ'",
            Vec::new(),
        )
        .await;
        assert!(
            now_count > after,
            "直接写入返回成功却没有落行（before={before} / after={after} / now={now_count}）"
        );
        // 没有任何一次留痕写入「彻底失败」（失败会计数并打 error 日志）
        assert_eq!(
            mask_model::plain_read_audit_failures(),
            failures_before,
            "有留痕写入彻底失败（进程级计数从 {failures_before} 增长）"
        );

        println!(
            "[X05] 明文导出留痕：FIELD_MASK_PLAIN_READ {before} → {now_count}（含直接调用 1 条，等待 {waited} ms）"
        );
    })
    .await;
}
