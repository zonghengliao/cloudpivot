//! 状态机边配置「管理面 + 审批化」的单测（【D3】t_217a95ad）。
//!
//! # 真库用例的开关
//!
//! 需要 `ERP_DB_URL`（与 `shared::state_machine` / `erp` / `flow::linkage` 同一口径）。
//! **未设置时打印 SKIP 并返回 = 假绿**，交付验收必须真跑：
//!
//! ```bash
//! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//!   cargo test --lib business::flow::edge_admin -- --test-threads=1
//! ```
//!
//! # 为什么真库用例要串行
//!
//! 用例会**改全局配置**（`biz_flow_state_edge` 的 STOCK_OUT / PURCHASE_ORDER 边），
//! 并发跑会互相看见对方的配置行，用一把 `DB_LOCK` 串起来（与 D1/D2 同一做法）。
//!
//! # 夹具口径
//!
//! - 配置行：物理删（`entity + (from,to)` 精确定位），不软删 —— 软删行会污染「存活行」断言；
//! - 审批单：按 `biz_type = 'FLOW_EDGE_CFG'` + 本模块用到的两条边（确定性 `biz_id`）删，
//!   顺序是「通知 → 待办 → 实例」，与 `pflow3b_tests` 同款；
//! - 审计行：`entity_type = 'FLOW_EDGE_CFG'` 全删（只删本类，不动别的单据日志）。

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Extension, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::edge_admin::{
    self, ChangeVerdict, EdgeChange, EdgeChangeRes, EdgeDeleteQuery, EdgeUpsertReq, StateEdgeQuery,
};
use crate::business::shared::state_machine::{self, Entity};

/// 用例串行锁（改全局配置的用例必须排队）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ==================== 纯逻辑用例（无数据库） ====================

/// ① 审批结论的映射口径：只有 APPROVED 才动配置。
#[test]
fn change_verdict_maps_terminal_states() {
    assert_eq!(
        edge_admin::change_verdict("APPROVED"),
        Some(ChangeVerdict::Apply)
    );
    assert_eq!(
        edge_admin::change_verdict(" approved "),
        Some(ChangeVerdict::Apply)
    );
    for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
        assert_eq!(
            edge_admin::change_verdict(status),
            Some(ChangeVerdict::Drop),
            "{status} 必须映射成 Drop（配置一分不动）"
        );
    }
    for status in ["PENDING", "", "UNKNOWN"] {
        assert_eq!(
            edge_admin::change_verdict(status),
            None,
            "{status} 不该动配置"
        );
    }
}

/// ② `form_data` 往返：审批单里存的**就是**待变更内容，丢了字段等于丢审批语义。
#[test]
fn form_data_round_trips_both_actions() {
    let upsert = EdgeChange::Upsert(state_machine::EdgeConfigPatch {
        entity: "STOCK_OUT".to_string(),
        from_status: "DRAFT".to_string(),
        to_status: "POSTED".to_string(),
        enabled: false,
        requires_reason: true,
        label: Some("直接过账（停用）".to_string()),
        auth_code: Some("business:erp:stock-out:status".to_string()),
        confirm: true,
    });
    let data = upsert.to_form_data(Some("客户要求必须走审批"));
    assert_eq!(data["action"], "UPSERT");
    assert_eq!(data["reason"], "客户要求必须走审批");
    let back = EdgeChange::from_form_data(&data).expect("必须能还原");
    assert_eq!(back.entity(), "STOCK_OUT");
    assert_eq!(back.from(), "DRAFT");
    assert_eq!(back.to(), "POSTED");
    assert_eq!(back.action(), edge_admin::ACTION_UPSERT);
    match &back {
        EdgeChange::Upsert(patch) => {
            assert!(!patch.enabled, "enabled=false 不能丢");
            assert!(patch.requires_reason);
            assert_eq!(patch.label.as_deref(), Some("直接过账（停用）"));
            assert_eq!(
                patch.auth_code.as_deref(),
                Some("business:erp:stock-out:status")
            );
            assert!(
                patch.confirm,
                "回调里一律 confirm=true（审批通过本身就是二次确认）"
            );
        }
        other => panic!("动作还原错了：{other:?}"),
    }

    let delete = EdgeChange::Delete {
        entity: "PURCHASE_ORDER".to_string(),
        from: "SUBMITTED".to_string(),
        to: "CLOSED".to_string(),
    };
    let data = delete.to_form_data(None);
    assert_eq!(data["action"], "DELETE");
    assert!(data["reason"].is_null(), "没填理由就写 null，不写空串");
    let back = EdgeChange::from_form_data(&data).expect("必须能还原");
    assert_eq!(back.action(), edge_admin::ACTION_DELETE);
    assert_eq!(
        back.summary(),
        "PURCHASE_ORDER SUBMITTED→CLOSED 删除配置（回到 BASE_EDGES）"
    );
}

/// ③ 脏 `form_data`（缺 entity / 未知 action）宁可不还原，也不猜。
#[test]
fn form_data_rejects_dirty_payload() {
    assert!(EdgeChange::from_form_data(&json!({})).is_none());
    assert!(
        EdgeChange::from_form_data(&json!({"action": "UPSERT", "entity": "STOCK_OUT"})).is_none()
    );
    assert!(
        EdgeChange::from_form_data(&json!({
            "action": "DROP_TABLE", "entity": "STOCK_OUT", "from_status": "DRAFT", "to_status": "POSTED"
        }))
        .is_none()
    );
}

/// ④ 「同一条边」的确定性 id：同键同 id、异键异 id，且带 v8 版本位（不是随机 uuid）。
#[test]
fn biz_id_is_stable_per_edge() {
    let a = EdgeChange::Delete {
        entity: "STOCK_OUT".to_string(),
        from: "DRAFT".to_string(),
        to: "POSTED".to_string(),
    };
    let b = a.clone();
    let c = EdgeChange::Delete {
        entity: "STOCK_OUT".to_string(),
        from: "DRAFT".to_string(),
        to: "CANCELLED".to_string(),
    };
    assert_eq!(
        a.biz_id(),
        b.biz_id(),
        "同一 (entity, from, to) 必须永远同一个 id"
    );
    assert_ne!(a.biz_id(), c.biz_id(), "不同的边必须是不同的 id");
    let id = a.biz_id();
    let bytes = id.as_bytes();
    assert_eq!(
        bytes[6] >> 4,
        8,
        "版本位应为 8（自定义 uuid，不是 v4 随机）"
    );
    assert_eq!(bytes[8] >> 6, 2, "variant 位应为 RFC 4122");
}

/// ⑤ 变更说明要能被人读懂（审批单标题 / 响应文案都用它）。
#[test]
fn summary_spells_out_the_change() {
    let change = EdgeChange::Upsert(state_machine::EdgeConfigPatch {
        entity: " STOCK_OUT ".to_string(),
        from_status: " DRAFT ".to_string(),
        to_status: " POSTED ".to_string(),
        enabled: false,
        requires_reason: true,
        label: Some(" 停用 ".to_string()),
        auth_code: None,
        confirm: true,
    });
    let summary = change.summary();
    assert!(summary.contains("STOCK_OUT"), "要点名单据类型：{summary}");
    assert!(summary.contains("DRAFT→POSTED"), "要点名这条边：{summary}");
    assert!(summary.contains("禁用"), "要说清改成了什么：{summary}");
    assert!(summary.contains("要求填原因"), "{summary}");
    assert!(summary.contains("文案「停用」"), "{summary}");
    assert!(!summary.contains("  "), "两侧空白要被 trim：{summary}");
}

/// ⑥ 权限码 / 流程编码 / 业务类型必须与迁移逐字一致（改代码没改迁移 → 这里红）。
#[test]
fn seed_migration_matches_code_constants() {
    let sql = include_str!("../../../migrations/business/V0206__flow_edge_config_admin.up.sql");
    for text in [
        edge_admin::PERM_LIST,
        edge_admin::PERM_MANAGE,
        edge_admin::FLOW_CODE,
        edge_admin::BIZ_TYPE,
        "FLOW_CONFIG_APPROVER",
        "20000000-0000-4000-8000-0000000e0206",
    ] {
        assert!(
            sql.contains(text),
            "V0206 里找不到 `{text}`：迁移与代码已经漂移"
        );
    }
    // 两个权限码必须是**按钮级**（menu_type 3）+ 隐藏（is_visible FALSE）
    assert!(sql.contains("'business:flow:state-edge:list', 3,"));
    assert!(sql.contains("'business:flow:state-edge:manage', 3,"));
    assert!(!sql.contains("{edge_admin}"), "迁移里的占位符没被替换");
}

// ==================== 真库用例 ====================

/// 连库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
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
                Ok(connection) => set_db(connection).is_ok(),
                Err(err) => {
                    println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

async fn exec(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 内生 admin 的主键（审批发起人必须是在职超管）。
async fn admin_id() -> Uuid {
    let id: Option<Uuid> = db()
        .scalar_with_params::<Option<Uuid>>(
            "SELECT id FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL LIMIT 1",
            vec![],
        )
        .await
        .expect("查 admin");
    id.expect("测试库必须有内置管理员 admin")
}

/// 超管声明（sub 是真实 admin，回调重建 claims 时才查得到角色）。
async fn super_claims() -> TokenClaims {
    let id = admin_id().await;
    TokenClaims {
        jti: None,
        sub: id.to_string(),
        username: Some("admin".to_string()),
        roles: vec!["SUPER_ADMIN".to_string()],
        permissions: vec!["*:*:*".to_string()],
        token_type: None,
        exp: 0,
        iat: 0,
        extra: HashMap::new(),
    }
}

/// 普通用户声明（只用于证明「非超管在提交阶段就被拒」）。
fn plain_claims() -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: "00000000-0000-4000-8000-0000000d3001".to_string(),
        username: Some("d3_plain".to_string()),
        roles: vec!["STAFF".to_string()],
        permissions: vec!["business:flow:state-edge:manage".to_string()],
        token_type: None,
        exp: 0,
        iat: 0,
        extra: HashMap::new(),
    }
}

/// 清本模块夹具（通知 → 待办 → 实例 → 配置行 → 审计行）。
async fn purge() {
    for sql in [
        "DELETE FROM sys_notice_receiver WHERE notice_id IN \
           (SELECT id FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN \
              (SELECT id::text FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG'))",
        "DELETE FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN \
           (SELECT id::text FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG')",
        "UPDATE biz_flow_instance SET prev_instance_id = NULL \
          WHERE biz_type = 'FLOW_EDGE_CFG'",
        "DELETE FROM biz_flow_task WHERE instance_id IN \
           (SELECT id FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG')",
        "DELETE FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG'",
        "DELETE FROM biz_flow_state_edge WHERE entity IN ('STOCK_OUT', 'PURCHASE_ORDER') \
           AND ((entity = 'STOCK_OUT' AND from_status = 'DRAFT' AND to_status = 'POSTED') \
             OR (entity = 'STOCK_OUT' AND from_status = 'POSTED' AND to_status = 'CANCELLED') \
             OR (entity = 'PURCHASE_ORDER' AND from_status = 'SUBMITTED' AND to_status = 'CLOSED'))",
        "DELETE FROM biz_status_log WHERE entity_type = 'FLOW_EDGE_CFG'",
    ] {
        exec(sql).await;
    }
}

/// 一条「禁用 STOCK_OUT DRAFT→POSTED」的请求（走真正的 handler）。
fn disable_stock_out_post() -> EdgeUpsertReq {
    EdgeUpsertReq {
        entity: "STOCK_OUT".to_string(),
        from_status: "DRAFT".to_string(),
        to_status: "POSTED".to_string(),
        enabled: false,
        requires_reason: false,
        label: None,
        auth_code: None,
        confirm: true,
        reason: Some("D3 单测：必须走审批再过账".to_string()),
        skip: crate::business::erp::approval_gate::SkipApprovalReq::default(),
    }
}

/// STOCK_OUT 的有效边里还有没有 `DRAFT → POSTED`。
async fn stock_out_allows_direct_post() -> bool {
    state_machine::effective_edges(db(), Entity::StockOut)
        .await
        .iter()
        .any(|edge| edge.from == "DRAFT" && edge.to == "POSTED")
}

/// ⑦ 种子的 `graph_hash` 必须与「从行重建出来的快照」逐字节一致
///（否则 D4d 每日自检会把它报成 `RowsEdited` 漂移）。
#[tokio::test]
async fn seed_flow_hash_matches_rows() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    let row = db()
        .find_with_params(
            "SELECT d.flow_code, d.biz_type, d.status, v.id AS version_id, v.graph_hash, \
                    v.graph_snapshot::text AS snapshot_text, v.node_count, v.edge_count \
               FROM biz_flow_def d JOIN biz_flow_def_version v ON v.id = d.current_version_id \
              WHERE d.flow_code = ? AND d.deleted_at IS NULL",
            vec![Value::Text(edge_admin::FLOW_CODE.to_string())],
        )
        .await
        .expect("查种子流程");
    let Some(row) = row.first() else {
        panic!("测试库缺 flow_edge_config 种子（V0206 没跑？）");
    };
    let version_id: Uuid = row.get_typed("version_id").expect("version_id");
    let stored_hash: String = row.get_typed("graph_hash").expect("graph_hash");
    let biz_type: String = row.get_typed("biz_type").expect("biz_type");
    let status: String = row.get_typed("status").expect("status");
    assert_eq!(biz_type, edge_admin::BIZ_TYPE);
    assert_eq!(status, "PUBLISHED");
    assert_eq!(row.get_typed::<i32>("node_count").expect("node_count"), 3);
    assert_eq!(row.get_typed::<i32>("edge_count").expect("edge_count"), 2);

    // 从 L1 行重建图 → 重算 hash（与 flow::maintain 的日检同一套函数）
    let graph = crate::business::flow::model::load_graph(version_id)
        .await
        .expect("从行重建图");
    assert_eq!(
        graph.graph_hash(),
        stored_hash,
        "种子流程的行与 graph_hash 不一致（手改了 node/edge 或抄错了 hash）"
    );
    // 快照也必须是同一份（jsonb 取回来 canonical 化后逐字节相等）
    let snapshot_text: Option<String> = row.get_typed("snapshot_text").ok();
    let snapshot: serde_json::Value =
        serde_json::from_str(snapshot_text.as_deref().unwrap_or("null")).expect("快照 JSON");
    assert_eq!(
        crate::business::flow::graph::canonical_json(&snapshot),
        graph.snapshot_json(),
        "graph_snapshot 与行重建出来的快照不一致"
    );
    println!("[D3] 种子流程 hash 自洽：{stored_hash}");
}

/// ⑧ 管理面能一次拿到「BASE 边 + 配置 + 有效边」，且三者的口径一致。
#[tokio::test]
async fn list_view_reports_base_config_and_effective() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    purge().await;
    let base = state_machine::Entity::StockOut.base_edges().len();
    let res = edge_admin::list_state_edges(Query(StateEdgeQuery {
        entity: Some("STOCK_OUT".to_string()),
    }))
    .await
    .expect("查配置");
    assert_eq!(res.code, 0);
    let data = res.data.expect("响应必须带 data");
    assert_eq!(data.total, 1, "传了 entity 就只返回一个实体");
    let view = &data.list[0];
    assert_eq!(view.entity, "STOCK_OUT");
    assert_eq!(view.base_edges.len(), base, "BASE 边一条不能少");
    assert!(view.configs.is_empty(), "干净库里没有配置行");
    assert_eq!(
        view.effective_edges.len(),
        base,
        "没有配置时有效边 == BASE 边"
    );
    assert!(
        view.effective_edges
            .iter()
            .any(|edge| edge.from == "DRAFT" && edge.to == "POSTED"),
        "默认允许直接过账（optional 边默认启用）"
    );

    // 未知实体 → 400（不静默返回空列表）
    let error = edge_admin::list_state_edges(Query(StateEdgeQuery {
        entity: Some("NOT_AN_ENTITY".to_string()),
    }))
    .await
    .expect_err("未知实体必须 400");
    assert_eq!(error.status, 400);
}

/// ⑨ **核心路径**：提交 → 审批通过 → 才生效（提交后配置一分没动）。
#[tokio::test]
async fn change_takes_effect_only_after_approval() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    purge().await;
    assert!(stock_out_allows_direct_post().await, "基线：允许直接过账");

    let claims = super_claims().await;
    let res = edge_admin::upsert_state_edge(
        Extension(AuthClaims(claims.clone())),
        Json(disable_stock_out_post()),
    )
    .await
    .unwrap_or_else(|err| panic!("提交失败：{} {}", err.code, err.msg));
    let change: EdgeChangeRes = res.data.expect("响应必须带 data");
    assert!(!change.applied, "有已发布流程时必须送审，不能直接生效");
    let instance_id: Uuid = change
        .instance_id
        .as_deref()
        .expect("必须回审批单 id")
        .parse()
        .expect("instance id");
    assert_eq!(change.instance_status.as_deref(), Some("PENDING"));
    assert!(
        change
            .instance_no
            .as_deref()
            .unwrap_or_default()
            .starts_with("FLW-"),
        "审批单号要能直接去审批中心搜：{:?}",
        change.instance_no
    );
    // 提交后：配置**还没**生效
    assert!(
        stock_out_allows_direct_post().await,
        "审批通过之前配置不能生效"
    );
    let pending = edge_admin::list_state_edges(Query(StateEdgeQuery {
        entity: Some("STOCK_OUT".to_string()),
    }))
    .await
    .expect("查配置");
    assert_eq!(
        pending.data.expect("data").list[0].pending_changes.len(),
        1,
        "管理面要能看到「这条边正在审批中」"
    );

    // 同一条边再提交一次 → 409（不许两张并行的审批单）
    let dup = edge_admin::upsert_state_edge(
        Extension(AuthClaims(claims.clone())),
        Json(disable_stock_out_post()),
    )
    .await
    .expect_err("同一条边不允许两张在跑的审批单");
    assert_eq!(dup.status, 409, "报文：{}", dup.msg);

    // 审批通过 → 回调落库
    approve_all(instance_id).await;
    assert!(
        !stock_out_allows_direct_post().await,
        "审批通过后 `DRAFT → POSTED` 必须从有效边里消失"
    );
    let row = db()
        .find_with_params(
            "SELECT enabled, requires_reason FROM biz_flow_state_edge \
              WHERE entity = 'STOCK_OUT' AND from_status = 'DRAFT' AND to_status = 'POSTED' \
                AND deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("查配置行");
    assert_eq!(row.len(), 1, "必须落一行配置");
    assert!(!row[0].get_typed::<bool>("enabled").expect("enabled"));

    // 详情 DTO 的 next_statuses 跟着有效边走（前端按钮联动的后端一半）
    let edges = state_machine::effective_edges(db(), Entity::StockOut).await;
    let draft_targets: Vec<&str> = edges
        .iter()
        .filter(|edge| edge.from == "DRAFT")
        .map(|edge| edge.to)
        .collect();
    assert!(
        !draft_targets.contains(&"POSTED"),
        "草稿态的可去状态里不该再有 POSTED：{draft_targets:?}"
    );
    assert!(
        draft_targets.contains(&"PENDING_POST"),
        "更严的那条路（提交待过账）必须还在：{draft_targets:?}"
    );

    purge().await;
}

/// ⑩ 驳回 → 配置一分不动（只留一条审计行）。
#[tokio::test]
async fn rejected_change_does_not_take_effect() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    purge().await;
    assert!(stock_out_allows_direct_post().await);

    let claims = super_claims().await;
    let res = edge_admin::upsert_state_edge(
        Extension(AuthClaims(claims)),
        Json(disable_stock_out_post()),
    )
    .await
    .unwrap_or_else(|err| panic!("提交失败：{} {}", err.code, err.msg));
    let submitted = res.data.expect("响应必须带 data");
    let instance_id: Uuid = submitted
        .instance_id
        .as_deref()
        .expect("审批单 id")
        .parse()
        .expect("instance id");

    let task_id = pending_task_of(instance_id).await;
    let operator = admin_id().await;
    let out = db()
        .transaction(|tx| async move {
            crate::business::flow::engine::reject(
                &tx,
                task_id,
                operator,
                "不同意：这条捷径要保留",
                crate::business::flow::engine::ReturnTo::Final,
            )
            .await
        })
        .await
        .unwrap_or_else(|err| panic!("驳回失败：{err}"));
    edge_admin::on_settled(out.instance_id, &out.instance_status).await;

    assert!(
        stock_out_allows_direct_post().await,
        "驳回之后 `DRAFT → POSTED` 必须还在（配置一分不动）"
    );
    let configs: i64 = db()
        .scalar_sql(
            "SELECT count(*) AS v FROM biz_flow_state_edge \
              WHERE entity = 'STOCK_OUT' AND from_status = 'DRAFT' AND to_status = 'POSTED' \
                AND deleted_at IS NULL"
                .to_string(),
            vec![],
        )
        .await
        .expect("数配置行");
    assert_eq!(configs, 0, "驳回不该留下任何配置行");
    let audits: i64 = db()
        .scalar_sql(
            "SELECT count(*) AS v FROM biz_status_log \
              WHERE entity_type = 'FLOW_EDGE_CFG' AND to_status = 'REJECTED'"
                .to_string(),
            vec![],
        )
        .await
        .expect("数审计行");
    assert!(audits >= 1, "驳回也要留痕（审计要能回答「谁试过改什么」）");
    purge().await;
}

/// ⑪ 二次确认缺失 / 非超管：**在提交阶段**就被内核拒掉，一张审批单都不该留下。
#[tokio::test]
async fn bad_patch_is_rejected_before_any_approval() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    purge().await;

    // ① 没有二次确认 → 400
    let mut req = disable_stock_out_post();
    req.confirm = false;
    let error =
        edge_admin::upsert_state_edge(Extension(AuthClaims(super_claims().await)), Json(req))
            .await
            .expect_err("confirm 缺失必须 400");
    assert_eq!(error.status, 400, "报文：{}", error.msg);

    // ② 禁用 mandatory 边 → 400（内核判，本层只透传）
    let mut req = disable_stock_out_post();
    req.to_status = "CANCELLED".to_string();
    let error =
        edge_admin::upsert_state_edge(Extension(AuthClaims(super_claims().await)), Json(req))
            .await
            .expect_err("禁用 mandatory 边必须 400");
    assert_eq!(error.status, 400, "报文：{}", error.msg);
    assert!(
        error.msg.contains("硬约束") || error.msg.contains("不能禁用"),
        "报文要点名「硬约束边不能禁用」：{}",
        error.msg
    );

    // ③ 非超管 → 403
    let error = edge_admin::upsert_state_edge(
        Extension(AuthClaims(plain_claims())),
        Json(disable_stock_out_post()),
    )
    .await
    .expect_err("非超管必须 403");
    assert_eq!(error.status, 403, "报文：{}", error.msg);

    // 一条审批单都不该留（试写是回滚的）
    let instances: i64 = db()
        .scalar_sql(
            "SELECT count(*) AS v FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG'"
                .to_string(),
            vec![],
        )
        .await
        .expect("数审批单");
    assert_eq!(instances, 0, "非法补丁不该留下审批单：{instances}");
    purge().await;
}

/// ⑫ 删除配置也走审批：提交 → 通过 → 配置消失（回到 BASE_EDGES）。
#[tokio::test]
async fn delete_config_also_goes_through_approval() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    purge().await;
    // 先直接落一行配置（超管 + 内核唯一入口）
    let claims = super_claims().await;
    state_machine::upsert_edge_config(
        db(),
        &claims,
        &state_machine::EdgeConfigPatch {
            entity: "PURCHASE_ORDER".to_string(),
            from_status: "SUBMITTED".to_string(),
            to_status: "CLOSED".to_string(),
            enabled: false,
            requires_reason: false,
            label: None,
            auth_code: None,
            confirm: true,
        },
    )
    .await
    .unwrap_or_else(|err| panic!("铺底配置失败：{} {}", err.code, err.msg));
    let allows_close = |edges: Vec<state_machine::Edge>| {
        edges
            .iter()
            .any(|edge| edge.from == "SUBMITTED" && edge.to == "CLOSED")
    };
    assert!(
        !allows_close(state_machine::effective_edges(db(), Entity::PurchaseOrder).await),
        "基线：直接关闭已被禁用"
    );

    // 没有配置可删 → 404（不产生无意义的审批单）
    let missing = edge_admin::delete_state_edge(
        Extension(AuthClaims(claims.clone())),
        Query(EdgeDeleteQuery {
            entity: "STOCK_OUT".to_string(),
            from_status: "DRAFT".to_string(),
            to_status: "POSTED".to_string(),
            confirm: true,
            reason: None,
            skip_approval: false,
            skip_reason: None,
        }),
    )
    .await
    .expect_err("没有配置行应 404");
    assert_eq!(missing.status, 404);

    let res = edge_admin::delete_state_edge(
        Extension(AuthClaims(claims)),
        Query(EdgeDeleteQuery {
            entity: "PURCHASE_ORDER".to_string(),
            from_status: "SUBMITTED".to_string(),
            to_status: "CLOSED".to_string(),
            confirm: true,
            reason: Some("D3 单测：恢复默认".to_string()),
            skip_approval: false,
            skip_reason: None,
        }),
    )
    .await
    .unwrap_or_else(|err| panic!("提交删除失败：{} {}", err.code, err.msg));
    let started = res.data.expect("响应必须带 data");
    assert!(!started.applied, "删除也要送审");
    let instance_id: Uuid = started
        .instance_id
        .as_deref()
        .expect("审批单 id")
        .parse()
        .expect("instance id");
    assert!(
        !allows_close(state_machine::effective_edges(db(), Entity::PurchaseOrder).await),
        "审批通过之前配置还在"
    );

    approve_all(instance_id).await;
    assert!(
        allows_close(state_machine::effective_edges(db(), Entity::PurchaseOrder).await),
        "删除审批通过后应回到 BASE_EDGES（可以直接关闭）"
    );
    let alive: i64 = db()
        .scalar_sql(
            "SELECT count(*) AS v FROM biz_flow_state_edge \
              WHERE entity = 'PURCHASE_ORDER' AND from_status = 'SUBMITTED' \
                AND to_status = 'CLOSED' AND deleted_at IS NULL"
                .to_string(),
            vec![],
        )
        .await
        .expect("数配置行");
    assert_eq!(alive, 0, "删除 = 软删（存活行 0）");
    purge().await;
}

/// ⑬ 管理面要能分辨「这条边配了」与「没配」：`configured` / `configs` 不能糊成一片。
#[tokio::test]
async fn list_view_marks_configured_edges() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    purge().await;
    let claims = super_claims().await;
    // 直接落一行「启用 + 要求原因 + 改文案」的配置（内核唯一入口）
    state_machine::upsert_edge_config(
        db(),
        &claims,
        &state_machine::EdgeConfigPatch {
            entity: "STOCK_OUT".to_string(),
            from_status: "POSTED".to_string(),
            to_status: "CANCELLED".to_string(),
            enabled: true,
            requires_reason: true,
            label: Some("作废红冲（必须写原因）".to_string()),
            auth_code: None,
            confirm: true,
        },
    )
    .await
    .unwrap_or_else(|err| panic!("铺底配置失败：{} {}", err.code, err.msg));

    let res = edge_admin::list_state_edges(Query(StateEdgeQuery {
        entity: Some("STOCK_OUT".to_string()),
    }))
    .await
    .expect("查配置");
    let view = res.data.expect("data").list.remove(0);
    assert_eq!(view.configs.len(), 1, "存活配置行应当被列出来");
    assert_eq!(
        view.configs[0].label.as_deref(),
        Some("作废红冲（必须写原因）")
    );
    let edge = view
        .effective_edges
        .iter()
        .find(|item| item.from == "POSTED" && item.to == "CANCELLED")
        .expect("有效边里必须有这条");
    assert!(edge.configured, "配过的边要标 configured=true");
    assert!(
        edge.requires_reason,
        "配置追加的「必须填原因」要出现在有效边里"
    );
    assert_eq!(edge.label, "作废红冲（必须写原因）", "文案以配置为准");
    // 没配过的边不能标 configured
    let untouched = view
        .effective_edges
        .iter()
        .find(|item| item.from == "DRAFT" && item.to == "POSTED")
        .expect("默认的 optional 边");
    assert!(!untouched.configured, "没配过的边不能标 configured");
    assert_eq!(untouched.label, "直接过账", "没配过就用 BASE 默认文案");
    purge().await;
}

/// ⑭ 流程被下线时的「默认拒绝 + 显式跳过」：不能因为删了流程就静默放行。
///
/// 这条路复用 `approval_gate`（与采购 / 收货等 6 个入口同一套判据），所以三个分支都要验：
/// 没请求跳过 → 409；请求了但没理由 → 400；理由齐全 → 直接生效并留痕。
#[tokio::test]
async fn offline_flow_falls_back_to_explicit_skip() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    purge().await;
    let claims = super_claims().await;
    // 下线流程（跑完**无论成败都恢复**：先登记恢复语句，再断言）
    exec(
        "UPDATE biz_flow_def SET status = 'DISABLED', updated_at = now() \
          WHERE flow_code = 'flow_edge_config'",
    )
    .await;
    let mut skip_req = disable_stock_out_post();
    skip_req.skip = crate::business::erp::approval_gate::SkipApprovalReq {
        skip_approval: true,
        skip_reason: Some("短".to_string()),
    };
    let without_skip = edge_admin::upsert_state_edge(
        Extension(AuthClaims(claims.clone())),
        Json(disable_stock_out_post()),
    )
    .await
    .expect_err("流程下线 + 没请求跳过 → 默认拒绝");
    let short_reason = edge_admin::upsert_state_edge(
        Extension(AuthClaims(claims.clone())),
        Json(skip_req.clone()),
    )
    .await
    .expect_err("请求跳过但理由不足 → 400");
    let mut good_skip = skip_req.clone();
    if let Some(reason) = good_skip.skip.skip_reason.as_mut() {
        *reason = "月底关账，这条捷径本次停用（D3 单测）".to_string();
    }
    let applied =
        edge_admin::upsert_state_edge(Extension(AuthClaims(claims)), Json(good_skip)).await;
    // 先恢复流程，再断言（避免「断言失败把流程永久留成 DISABLED」污染后续用例）
    exec(
        "UPDATE biz_flow_def SET status = 'PUBLISHED', updated_at = now() \
          WHERE flow_code = 'flow_edge_config'",
    )
    .await;

    assert_eq!(without_skip.status, 409, "报文：{}", without_skip.msg);
    assert_eq!(
        without_skip.code,
        crate::shared::error_code::APPROVAL_FLOW_NOT_CONFIGURED,
        "要点名「没有已发布的审批流程」"
    );
    assert_eq!(short_reason.status, 400, "报文：{}", short_reason.msg);
    assert!(
        short_reason.msg.contains("10"),
        "理由不足要说清门槛：{}",
        short_reason.msg
    );
    let applied =
        applied.unwrap_or_else(|err| panic!("持码 + 理由齐全应放行：{} {}", err.code, err.msg));
    let data = applied.data.expect("data");
    assert!(data.applied, "跳过审批时直接生效（不产生审批单）");
    assert!(data.instance_id.is_none(), "跳过路径不该有审批单");
    let audits: i64 = db()
        .scalar_sql(
            "SELECT count(*) AS v FROM biz_status_log \
              WHERE entity_type = 'FLOW_EDGE_CFG' AND from_status = 'SKIPPED'"
                .to_string(),
            vec![],
        )
        .await
        .expect("数审计行");
    assert!(audits >= 1, "跳过审批必须留痕（SKIPPED）");
    purge().await;
}

// ---------- 真库用例的小工具 ----------
/// 实例上的待办 id（PENDING）。
async fn pending_task_of(instance_id: Uuid) -> Uuid {
    let value: Option<Uuid> = db()
        .scalar_with_params::<Option<Uuid>>(
            "SELECT id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' \
              ORDER BY created_at LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查待办");
    value.expect("审批单必须有待办（否则没人能批）")
}

/// 把审批单**批到终态**，并按 `flow::handler` 的口径跑回调。
///
/// ⚠️ `flow_edge_config` 的 `n_cfg` 是 `ROLE` + `empty_assignee = FALLBACK_ADMIN`：
/// 那个角色当前没人时回退给**全部超管**，`join_mode = AND` ⇒ 会签人数 = 超管人数。
/// 只批一条会让实例停在 `PENDING`（实测在这里报「应当一次通过」假红），
/// 所以一律批到终态并断言真的 `APPROVED`。
async fn approve_all(instance_id: Uuid) {
    let outs = crate::business::flow::test_support::approve_all_pending(instance_id, "同意").await;
    assert!(
        !outs.is_empty(),
        "审批单必须有 PENDING 待办可批（实例 {instance_id}）"
    );
    for out in outs {
        // 与 `flow::handler::approve_task` 同序：事务已提交，再跑业务回调
        edge_admin::on_settled(out.instance_id, &out.instance_status).await;
    }
    crate::business::flow::test_support::assert_instance_status(instance_id, "APPROVED").await;
}
