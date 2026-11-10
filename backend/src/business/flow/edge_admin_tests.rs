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
use crate::shared::error_code;

/// 用例串行锁（改全局配置的用例必须排队）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ==================== 纯逻辑用例（无数据库） ====================

/// ① 审批结论的映射口径：只有 APPROVED 才动配置。
#[test]
fn change_verdict_maps_terminal_states() {
    assert_eq!(edge_admin::change_verdict("APPROVED"), Some(ChangeVerdict::Apply));
    assert_eq!(edge_admin::change_verdict(" approved "), Some(ChangeVerdict::Apply));
    for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
        assert_eq!(
            edge_admin::change_verdict(status),
            Some(ChangeVerdict::Drop),
            "{status} 必须映射成 Drop（配置一分不动）"
        );
    }
    for status in ["PENDING", "", "UNKNOWN"] {
        assert_eq!(edge_admin::change_verdict(status), None, "{status} 不该动配置");
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
        // 【E2E-FIX-1】这条边是主链路必需边：确认位必须一起往返，否则审批通过回调重放时
        // 会被内核再挡一次 400（「审批过了配置没生效」）。
        confirm_critical: true,
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
            assert_eq!(patch.auth_code.as_deref(), Some("business:erp:stock-out:status"));
            assert!(patch.confirm, "回调里一律 confirm=true（审批通过本身就是二次确认）");
            assert!(
                patch.confirm_critical,
                "【E2E-FIX-1】confirm_critical 丢了 → 回调重放禁用主链路边会被内核 400"
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
    assert!(EdgeChange::from_form_data(&json!({"action": "UPSERT", "entity": "STOCK_OUT"})).is_none());
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
    assert_eq!(a.biz_id(), b.biz_id(), "同一 (entity, from, to) 必须永远同一个 id");
    assert_ne!(a.biz_id(), c.biz_id(), "不同的边必须是不同的 id");
    let id = a.biz_id();
    let bytes = id.as_bytes();
    assert_eq!(bytes[6] >> 4, 8, "版本位应为 8（自定义 uuid，不是 v4 随机）");
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
        confirm_critical: false,
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
        assert!(sql.contains(text), "V0206 里找不到 `{text}`：迁移与代码已经漂移");
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
    // 「复核岗绑 admin」是本模块的夹具（`prepare()` 里装），这里一并还原：
    // 用例跑完库里的种子角色要和迁移 V0206 时一样（0 成员，走 FALLBACK_ADMIN）。
    unbind_config_approver().await;
}

// ==================== 夹具：隔离（单人复核）+ 守卫（图基线） ====================
//
// # 为什么夹具要「把复核岗绑死到一个人」（卡 t_977c5189）
//
// 种子 `n_cfg` 是 `assignee_type='ROLE'`（`FLOW_CONFIG_APPROVER`）+ `empty_assignee='FALLBACK_ADMIN'`，
// 而 `join_mode='AND'` 是**会签**：角色 0 人时兜底到「**全部**在职超管」⇒ 一张单要投几票，
// 取决于「库里现在有几个超管」，而不是取决于本例要验的业务语义。
//
// 2026-11-06 全量真库实测（同一隔离库、`--test-threads=1`）：`payable_bridge`（P17）的夹具
// 用户 `p17_ap1 / p17_ap2` 在模块跑完后仍是**在职超管**（它的清场只挂在「下一个用例的
// setup」上，模块跑完不会跑），于是本模块的审批单从 1 票变 3 票，三条「批量同意后必须落边」
// 的用例集体假红（落边 0 行、实例停在 `PENDING`、报文「还有 2 条 PENDING 待办没批」）。
//
// 修法：夹具自己把复核岗绑到**确定的一个人**（内置 admin）⇒ `ROLE` 非空、不再走
// `FALLBACK_ADMIN` 展开 ⇒ 待办恒为 1，与环境里的超管人数彻底解耦。
//
// # 为什么还要一道「图基线」守卫
//
// 上面的隔离只能挡住「环境里有几个超管」。如果**已发布流程本身**被别人改写
// （换版本 / 改节点 / 把 `n_cfg` 从 ROLE 改成 ADMIN / 加会签节点），那破坏的是**本例的前提**，
// 不是被测代码 —— 应该一眼看到「夹具污染」，而不是让 `落边行数 left:0 right:1` 去背锅。
// `guard_published_flow_graph` 就是这道守卫：只认 V0206 的种子图指纹。

/// 种子流程 `flow_edge_config` 的定义 id（V0206 里写死的 uuid；被换掉 = 别人重新发布了流程）。
const SEED_DEF_ID: &str = "20000000-0000-4000-8000-0000000e0206";

// 种子图的基线指纹 —— **P3-06 起不再用字面 hash**（以下为历史说明；原「字面 hash 基线」
// 守卫已改成结构断言，本块记录为什么不能再用字面值）。
//
// # 为什么（P3-06 的语义变更）
//
// `approval_mode` 从「NULL = 全员签」改成「NULL = 或签」之后，存量节点的模式必须**按环境数据
// 冻结**（候选人数 ≥2 → `ALL`，否则 `ANY`，见 `V0339` + `flow::sign_freeze`）。也就是说
// 同一份种子图在不同环境的快照 hash **本来就不同**（该角色有几个成员决定 `ALL`/`ANY`），
// 字面 hash 基线会变成「换个库就红」。
//
// 所以守卫改成**结构断言**（定义 id / 节点 key / 节点数 / 边数 / `n_cfg` 的审批人来源与
// 空审批人策略 / 审批人行）—— 那才是「图被改过」的真正信号；「行 ↔ 快照 ↔ hash 三者一致」
// 由 `seed_flow_hash_matches_rows` 守（那才是 D4d 日检的口径）。
// 历史值（V0206 原始 hash，冻结前）留档备查：`c0ad61937b734114f6865f9347b57ab9824f560605e522a204d22650baa30e08`。

/// 复核岗角色码（V0206 种子角色；`n_cfg` 的审批人来源）。
const APPROVER_ROLE_CODE: &str = "FLOW_CONFIG_APPROVER";

/// 复核岗的角色 id（种子缺失直接炸：那是「V0206 没应用」，不是业务红）。
async fn approver_role_id() -> Uuid {
    let id: Option<Uuid> = db()
        .scalar_with_params::<Option<Uuid>>(
            "SELECT id FROM sys_role WHERE role_code = ? AND deleted_at IS NULL LIMIT 1",
            vec![Value::Text(APPROVER_ROLE_CODE.to_string())],
        )
        .await
        .expect("查复核岗角色");
    id.unwrap_or_else(|| panic!("测试库缺种子角色 {APPROVER_ROLE_CODE}（V0206 没跑？）"))
}

/// 守卫：已发布的 `flow_edge_config` 必须仍是 V0206 的种子图（定义 id / 指纹 / 节点结构逐项钉死）。
///
/// 断言口径**取产品解析出来的那一条**（[`edge_admin::published_flow`]），不是「库里第一条」——
/// 守卫要证明的是「产品这次会拿哪张图」，那才是本例的前提。
async fn guard_published_flow_graph() {
    let flow = edge_admin::published_flow()
        .await
        .expect("查已发布流程（守卫）")
        .unwrap_or_else(|| {
            panic!(
                "夹具污染：库里没有已发布的 `flow_edge_config` 流程（V0206 种子被删/被下线）。\n\
                 本模块的前提是「有一条已发布流程，提交就会送审」；缺了它，用例会走\n\
                 「显式跳过审批」分支 —— 那不是本例要验的路径。\n\
                 修法：确认 V0206 已应用、`biz_flow_def.flow_code='flow_edge_config'` 的 \
                 `status='PUBLISHED'` 且 `deleted_at IS NULL`。"
            )
        });
    assert_eq!(
        flow.def_id.to_string(),
        SEED_DEF_ID,
        "夹具污染：产品解析到的已发布流程是 `{}`（{}），不是 V0206 种子 `{SEED_DEF_ID}`。\n\
         说明有人在共用库上又发布了一条 `biz_type='FLOW_EDGE_CFG'` 的流程盖住了种子。\n\
         修法：删掉多余的那条定义（或把它的 `is_default` / 版本号降下去），再重跑。",
        flow.flow_code,
        flow.def_id
    );

    let row = db()
        .find_with_params(
            "SELECT graph_hash, node_count, edge_count FROM biz_flow_def_version WHERE id = ?::uuid",
            vec![Value::from(&flow.version_id)],
        )
        .await
        .expect("查已发布版本（守卫）");
    let Some(row) = row.first() else {
        panic!("夹具污染：已发布流程指向的版本 {} 不存在", flow.version_id);
    };
    let hash: String = row.get_typed("graph_hash").expect("graph_hash");
    let nodes: i32 = row.get_typed("node_count").expect("node_count");
    let edges: i32 = row.get_typed("edge_count").expect("edge_count");
    let node_rows = db()
        .find_with_params(
            "SELECT node_key::text AS node_key, node_type::text AS node_type, \
                    assignee_type::text AS assignee_type, empty_assignee::text AS empty_assignee, \
                    join_mode::text AS join_mode \
               FROM biz_flow_node WHERE version_id = ?::uuid ORDER BY node_key",
            vec![Value::from(&flow.version_id)],
        )
        .await
        .expect("查已发布版本的节点（守卫）");
    let keys: Vec<String> = node_rows
        .iter()
        .map(|row| row.get_typed::<String>("node_key").unwrap_or_default())
        .collect();
    let assignees = db()
        .find_with_params(
            "SELECT a.assignee_type::text AS assignee_type, a.ref_id::text AS ref_id, \
                    n.node_key::text AS node_key \
               FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id = a.node_id \
              WHERE n.version_id = ?::uuid ORDER BY n.node_key, a.sort_order",
            vec![Value::from(&flow.version_id)],
        )
        .await
        .expect("查已发布版本的审批人（守卫）");
    let assignee_text: Vec<String> = assignees
        .iter()
        .map(|row| {
            format!(
                "{}:{}:{}",
                row.get_typed::<String>("node_key").unwrap_or_default(),
                row.get_typed::<String>("assignee_type").unwrap_or_default(),
                row.get_typed::<String>("ref_id").unwrap_or_default()
            )
        })
        .collect();
    let role = approver_role_id().await;
    let n_cfg = node_rows
        .iter()
        .find(|row| row.get_typed::<String>("node_key").ok().as_deref() == Some("n_cfg"));

    let pollution = |detail: String| -> ! {
        panic!(
            "夹具污染（不是业务断言失败）：已发布 `flow_edge_config` 图与 V0206 基线不一致。\n\
             {detail}\n\
             本模块全部真库用例的前提是「s → n_cfg(ROLE={APPROVER_ROLE_CODE}, 单人复核) → e」。\n\
             两种可能：① 有用例/脚本在**共用库**上改了这张图（改完没恢复）；\n\
             ② 种子被正当替换了 —— 那要同步本文件的 `SEED_DEF_ID` 与结构断言，\n\
                并在提交里写明是哪条迁移改的图。\n\
             修法：换一个干净隔离库跑（`scripts/verify/erp_clean_db.sh`），或把种子图恢复成 V0206 的版本。"
        )
    };

    if nodes != 3 || edges != 2 {
        pollution(format!(
            "基线：node_count=3 edge_count=2\n\
             实际：hash={hash} node_count={nodes} edge_count={edges}（版本 {}）",
            flow.version_id
        ));
    }
    if keys != vec!["e".to_string(), "n_cfg".to_string(), "s".to_string()] {
        pollution(format!("节点 key 基线 [e, n_cfg, s]，实际 {keys:?}"));
    }
    match n_cfg {
        Some(row)
            if row.get_typed::<String>("node_type").ok().as_deref() == Some("APPROVAL")
                && row.get_typed::<String>("assignee_type").ok().as_deref() == Some("ROLE")
                && row.get_typed::<String>("empty_assignee").ok().as_deref() == Some("FALLBACK_ADMIN")
                && row.get_typed::<String>("join_mode").ok().as_deref() == Some("AND") => {}
        _ => pollution("`n_cfg` 的节点配置变了（基线 APPROVAL / ROLE / FALLBACK_ADMIN / AND）".to_string()),
    }
    if assignee_text != vec![format!("n_cfg:ROLE:{role}")] {
        pollution(format!(
            "`n_cfg` 的审批人基线是 [n_cfg:ROLE:{role}]（复核岗），实际 {assignee_text:?}"
        ));
    }
    println!("[t_977c5189] 已发布流程基线校验通过：hash={hash} 节点 {keys:?}");
}

/// 把内置 admin 绑到复核岗 → `n_cfg` **恒为单人复核**（与库里超管人数无关）。
///
/// 绑定后立刻断言「该角色在职成员恰好只有 admin」：别的用例往这个角色里塞人时，
/// 本模块的「一张单 1 票」前提同样不成立 —— 那也要报夹具污染，而不是让业务断言去猜。
async fn bind_config_approver() {
    let role = approver_role_id().await;
    let admin = admin_id().await;
    exec(&format!(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES ('{admin}'::uuid, '{role}'::uuid) \
         ON CONFLICT DO NOTHING"
    ))
    .await;
    // 与产品 `flow/assignee.rs::load_role_members` **逐字同口径**：
    // `role_id` 命中 + 绑定未过期 + 用户未软删（不含 status，产品在这一步不看 status）。
    let members: Vec<String> = db()
        .find_with_params(
            "SELECT u.username FROM sys_user u JOIN sys_user_role ur ON ur.user_id = u.id \
              WHERE ur.role_id = ?::uuid \
                AND (ur.expire_time IS NULL OR ur.expire_time > now()) \
                AND u.deleted_at IS NULL \
              ORDER BY u.username",
            vec![Value::from(&role)],
        )
        .await
        .expect("查复核岗成员（守卫）")
        .iter()
        .map(|row| row.get_typed::<String>("username").unwrap_or_default())
        .collect();
    assert_eq!(
        members,
        vec!["admin".to_string()],
        "夹具污染（不是业务断言失败）：复核岗 `{APPROVER_ROLE_CODE}` 的在职成员应当只有内置 \
         admin，实际 {members:?}。\n\
         本模块断言「一张审批单 = 1 条待办」（`join_mode=AND` 会签），多人绑这个角色就变会签、\
         单点「同意」批不掉整单。\n\
         修法：清掉该角色上的夹具成员（`DELETE FROM sys_user_role WHERE role_id = \
         '<{APPROVER_ROLE_CODE} 的 id>'`），并把它改成「用例结束即清」（不要只挂在 setup 上）。"
    );
}

/// 拆掉「复核岗绑 admin」这条夹具（挂在 `purge()` 上：用例开头自清、结尾还原）。
async fn unbind_config_approver() {
    let role = approver_role_id().await;
    let admin = admin_id().await;
    exec(&format!(
        "DELETE FROM sys_user_role WHERE user_id = '{admin}'::uuid AND role_id = '{role}'::uuid"
    ))
    .await;
}

/// 每个真库用例的开头：守卫（前提）→ 清上一轮 → 装「单人复核」夹具。
///
/// 顺序有意义：守卫先跑，被污染时**第一句**就报「夹具污染」，不会被清理语句的报错盖住。
async fn prepare() {
    guard_published_flow_graph().await;
    purge().await;
    bind_config_approver().await;
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
        // 【E2E-FIX-1】STOCK_OUT DRAFT→POSTED 是主链路必需边：不带这个确认位，
        // handler 会先被内核 400 挡下（那是另一条用例专门验的行为），
        // 这里验的是「送审 → 审批 → 落边」，所以按管理员显式确认带上 true。
        confirm_critical: true,
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
    // P3-06 起不再比字面基线（`approval_mode` 是按环境数据冻结的，见守卫处的说明）：
    // 这里只要求「是个合法 sha256」，真正的守卫是下面的三条一致（行 ↔ hash ↔ 快照）。
    assert_eq!(
        stored_hash.len(),
        64,
        "graph_hash 必须是 64 位小写十六进制（当前 {stored_hash}）"
    );
    assert!(
        stored_hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "graph_hash 必须是 64 位小写十六进制（当前 {stored_hash}）"
    );
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
    prepare().await;
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
    assert_eq!(view.effective_edges.len(), base, "没有配置时有效边 == BASE 边");
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

/// 【E2E-FIX-1】禁用主链路必需边（`STOCK_OUT DRAFT→POSTED`）：不传 `confirm_critical`
/// → 400 点名业务后果，且**不留审批单、配置一分没动**；带上确认才送审。
///
/// 这条用例守的是 2026-11-09 E2E-02 演练的真实坑：那条边被悄悄关掉 → 发货过账 409 →
/// 整条销售链断在发货，而关它的人根本没意识到自己在关主链路。
#[tokio::test]
async fn critical_edge_disable_needs_confirmation() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    prepare().await;
    let claims = super_claims().await;

    let mut req = disable_stock_out_post();
    req.confirm_critical = false; // 模拟「没勾那个红色警告弹窗」
    let err = edge_admin::upsert_state_edge(Extension(AuthClaims(claims.clone())), Json(req))
        .await
        .expect_err("禁用主链路边不确认 → 必须 400");
    assert_eq!(err.status, 400, "报文：{}", err.msg);
    for needle in ["主链路必需边", "销售出库无法直接过账", "confirm_critical"] {
        assert!(
            err.msg.contains(needle),
            "报文必须包含「{needle}」（要点名业务后果 + 告诉调用方怎么继续）：{}",
            err.msg
        );
    }
    // 被挡下的请求不许留审批单，配置也不许动
    let instances: i64 = db()
        .scalar_sql(
            "SELECT count(*) AS v FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG'".to_string(),
            vec![],
        )
        .await
        .expect("数审批单");
    assert_eq!(instances, 0, "被挡下的请求不许留下审批单：{instances}");
    assert!(
        stock_out_allows_direct_post().await,
        "被挡下 = 配置一分没动（DRAFT→POSTED 仍然可用）"
    );

    // 带了确认 → 照常送审（确认位只放开「禁用主链路边」这一件事，不放宽其他校验）
    let res = edge_admin::upsert_state_edge(Extension(AuthClaims(claims)), Json(disable_stock_out_post()))
        .await
        .unwrap_or_else(|err| panic!("确认后应能送审：{} {}", err.code, err.msg));
    assert!(
        !res.data.expect("响应必须带 data").applied,
        "有已发布流程 → 送审，不直接生效"
    );
    purge().await;
}

/// ⑨ **核心路径**：提交 → 审批通过 → 才生效（提交后配置一分没动）。
#[tokio::test]
async fn change_takes_effect_only_after_approval() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    prepare().await;
    assert!(stock_out_allows_direct_post().await, "基线：允许直接过账");

    let claims = super_claims().await;
    let res = edge_admin::upsert_state_edge(Extension(AuthClaims(claims.clone())), Json(disable_stock_out_post()))
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
        change.instance_no.as_deref().unwrap_or_default().starts_with("FLW-"),
        "审批单号要能直接去审批中心搜：{:?}",
        change.instance_no
    );
    // 提交后：配置**还没**生效
    assert!(stock_out_allows_direct_post().await, "审批通过之前配置不能生效");
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
    let dup = edge_admin::upsert_state_edge(Extension(AuthClaims(claims.clone())), Json(disable_stock_out_post()))
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
    prepare().await;
    assert!(stock_out_allows_direct_post().await);

    let claims = super_claims().await;
    let res = edge_admin::upsert_state_edge(Extension(AuthClaims(claims)), Json(disable_stock_out_post()))
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
    prepare().await;

    // ① 没有二次确认 → 400
    let mut req = disable_stock_out_post();
    req.confirm = false;
    let error = edge_admin::upsert_state_edge(Extension(AuthClaims(super_claims().await)), Json(req))
        .await
        .expect_err("confirm 缺失必须 400");
    assert_eq!(error.status, 400, "报文：{}", error.msg);

    // ② 禁用 mandatory 边 → 400（内核判，本层只透传）
    let mut req = disable_stock_out_post();
    req.to_status = "CANCELLED".to_string();
    let error = edge_admin::upsert_state_edge(Extension(AuthClaims(super_claims().await)), Json(req))
        .await
        .expect_err("禁用 mandatory 边必须 400");
    assert_eq!(error.status, 400, "报文：{}", error.msg);
    assert!(
        error.msg.contains("硬约束") || error.msg.contains("不能禁用"),
        "报文要点名「硬约束边不能禁用」：{}",
        error.msg
    );

    // ③ 非超管 → 403
    let error = edge_admin::upsert_state_edge(Extension(AuthClaims(plain_claims())), Json(disable_stock_out_post()))
        .await
        .expect_err("非超管必须 403");
    assert_eq!(error.status, 403, "报文：{}", error.msg);

    // 一条审批单都不该留（试写是回滚的）
    let instances: i64 = db()
        .scalar_sql(
            "SELECT count(*) AS v FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG'".to_string(),
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
    prepare().await;
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
            // SUBMITTED→CLOSED 不是主链路必需边，不需要第三次确认
            confirm_critical: false,
        },
    )
    .await
    .unwrap_or_else(|err| panic!("铺底配置失败：{} {}", err.code, err.msg));
    let allows_close =
        |edges: Vec<state_machine::Edge>| edges.iter().any(|edge| edge.from == "SUBMITTED" && edge.to == "CLOSED");
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
    prepare().await;
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
            // 启用不需要确认（只有禁用主链路边才要）
            confirm_critical: false,
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
    assert_eq!(view.configs[0].label.as_deref(), Some("作废红冲（必须写原因）"));
    let edge = view
        .effective_edges
        .iter()
        .find(|item| item.from == "POSTED" && item.to == "CANCELLED")
        .expect("有效边里必须有这条");
    assert!(edge.configured, "配过的边要标 configured=true");
    assert!(edge.requires_reason, "配置追加的「必须填原因」要出现在有效边里");
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
    prepare().await;
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
    let without_skip =
        edge_admin::upsert_state_edge(Extension(AuthClaims(claims.clone())), Json(disable_stock_out_post()))
            .await
            .expect_err("流程下线 + 没请求跳过 → 默认拒绝");
    let short_reason = edge_admin::upsert_state_edge(Extension(AuthClaims(claims.clone())), Json(skip_req.clone()))
        .await
        .expect_err("请求跳过但理由不足 → 400");
    let mut good_skip = skip_req.clone();
    if let Some(reason) = good_skip.skip.skip_reason.as_mut() {
        *reason = "月底关账，这条捷径本次停用（D3 单测）".to_string();
    }
    let applied = edge_admin::upsert_state_edge(Extension(AuthClaims(claims)), Json(good_skip)).await;
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
    let applied = applied.unwrap_or_else(|err| panic!("持码 + 理由齐全应放行：{} {}", err.code, err.msg));
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

/// ⑭ FIX-9 ①：审批意见必须回写实例 `result_comment`（以前只落在待办行）。
///
/// 引擎把「同意 / 驳回」的意见写在 `biz_flow_task.comment`，实例行一直是空的 ——
/// 用户在「审批中心 → 实例详情」看不到审批人说了什么（P0-02 演练里就是这么发现的）。
/// 修法是实例落终态时取最后一票的**非空意见**回写（会签末票不写话时回退到前面投过的人）。
#[tokio::test]
async fn approval_comment_written_back_to_instance_result_comment() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    prepare().await;

    const COMMENT: &str = "FIX-9：这条边先禁用，月底盘库后再恢复";
    let instance_id = submit_change("STOCK_OUT", "DRAFT", "POSTED").await;
    let outs = crate::business::flow::test_support::approve_all_pending(instance_id, COMMENT).await;
    assert!(!outs.is_empty(), "审批单必须有 PENDING 待办可批");
    for out in outs {
        // 与 `flow::handler::approve_task` 同序：事务已提交，再跑业务回调
        edge_admin::on_settled(out.instance_id, &out.instance_status).await;
    }
    crate::business::flow::test_support::assert_instance_status(instance_id, "APPROVED").await;

    let result_comment: Option<String> = db()
        .scalar_with_params::<Option<String>>(
            "SELECT result_comment FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("读实例 result_comment");
    let last_task_comment: Option<String> = db()
        .scalar_with_params::<Option<String>>(
            "SELECT comment FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'APPROVED' \
              ORDER BY completed_at DESC LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("读末票意见");
    purge().await;

    assert_eq!(
        last_task_comment.as_deref(),
        Some(COMMENT),
        "前置：意见本来就写在待办行"
    );
    assert_eq!(
        result_comment.as_deref(),
        Some(COMMENT),
        "审批完成后实例 result_comment 必须有最后一条意见（以前这里是 NULL）"
    );
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
    assert!(!outs.is_empty(), "审批单必须有 PENDING 待办可批（实例 {instance_id}）");
    for out in outs {
        // 与 `flow::handler::approve_task` 同序：事务已提交，再跑业务回调
        edge_admin::on_settled(out.instance_id, &out.instance_status).await;
    }
    crate::business::flow::test_support::assert_instance_status(instance_id, "APPROVED").await;
}

// ==================== 批量审批端点与落边（P1-FIX-4） ====================
//
// 旧批量端点 `POST /business/flow/task/batch`（`flow::handler::batch_task`）曾经自己写了一套
// 循环：开事务 → `engine::approve` → `drain(effects)` → 记结果。**它不跑任何业务回调**，
// 于是「批量同意」一张 `FLOW_EDGE_CFG` 单：审批单落 `APPROVED`，`biz_flow_state_edge`
// 一行不写（配置一分没生效），另外 13 个业务回调和 9 道内控闸也一起被绕过。
//
// 修法：批量层改成**逐条委托** `flow::batch::batch_approve`（它调 `approve_task` /
// `reject_task` 本体）。下面这组用例把「批量 = 单条」钉死。

/// 按边生成一条「禁用这条可选边」的请求（与 [`disable_stock_out_post`] 同口径）。
fn disable_edge(entity: &str, from: &str, to: &str) -> EdgeUpsertReq {
    EdgeUpsertReq {
        entity: entity.to_string(),
        from_status: from.to_string(),
        to_status: to.to_string(),
        enabled: false,
        requires_reason: false,
        label: None,
        auth_code: None,
        confirm: true,
        // 【E2E-FIX-1】调用方包含 `STOCK_OUT DRAFT→POSTED`（主链路必需边），
        // 这里代表「管理员已勾选确认」；否则 handler 直接 400，「批量 = 单条」根本跑不到审批。
        confirm_critical: true,
        reason: Some("P1-FIX-4 单测：批量审批也必须落边".to_string()),
        skip: crate::business::erp::approval_gate::SkipApprovalReq::default(),
    }
}

/// 走真 handler 提交一条配置变更 → 审批单 id（断言「必须送审」）。
async fn submit_change(entity: &str, from: &str, to: &str) -> Uuid {
    let res = edge_admin::upsert_state_edge(
        Extension(AuthClaims(super_claims().await)),
        Json(disable_edge(entity, from, to)),
    )
    .await
    .unwrap_or_else(|err| panic!("提交失败：{} {}", err.code, err.msg));
    let change = res.data.expect("响应必须带 data");
    assert!(!change.applied, "有已发布流程时必须送审，不能直接生效");
    change
        .instance_id
        .expect("必须回审批单 id")
        .parse()
        .expect("instance id")
}

/// 调旧批量端点（`handler::batch_task`）。
async fn batch_tasks(
    claims: TokenClaims,
    ids: Vec<String>,
    action: &str,
    comment: Option<&str>,
) -> std::result::Result<crate::business::flow::dto::BatchRes, ApiError> {
    batch_tasks_with_return(claims, ids, action, comment, None).await
}

/// 同上，带驳回退回目标（`NONE` = 驳回并终止 → 实例直接落 REJECTED）。
async fn batch_tasks_with_return(
    claims: TokenClaims,
    ids: Vec<String>,
    action: &str,
    comment: Option<&str>,
    return_to: Option<&str>,
) -> std::result::Result<crate::business::flow::dto::BatchRes, ApiError> {
    crate::business::flow::handler::batch_task(
        Extension(AuthClaims(claims)),
        Json(crate::business::flow::dto::BatchReq {
            task_ids: ids,
            action: action.to_string(),
            comment: comment.map(str::to_string),
            return_to: return_to.map(str::to_string),
        }),
    )
    .await
    .map(|res| res.data.expect("批量响应必须带 data"))
}

/// 存活配置行数。
async fn live_config_rows(entity: &str, from: &str, to: &str) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) AS v FROM biz_flow_state_edge \
              WHERE entity = ? AND from_status = ? AND to_status = ? AND deleted_at IS NULL",
        vec![
            Value::Text(entity.to_string()),
            Value::Text(from.to_string()),
            Value::Text(to.to_string()),
        ],
    )
    .await
    .expect("数配置行")
}

/// ⑫ 批量同意 = 单条同意：**审批通过必须真正落边**（P1-FIX-4 的核心回归）。
#[tokio::test]
async fn batch_task_writes_edge_after_approval() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    prepare().await;
    assert!(stock_out_allows_direct_post().await, "基线：允许直接过账");

    let instance_id = submit_change("STOCK_OUT", "DRAFT", "POSTED").await;
    let task_id = pending_task_of(instance_id).await;
    assert_eq!(live_config_rows("STOCK_OUT", "DRAFT", "POSTED").await, 0);

    let out = batch_tasks(super_claims().await, vec![task_id.to_string()], "APPROVE", None)
        .await
        .unwrap_or_else(|err| panic!("批量同意失败：{} {}", err.code, err.msg));
    assert_eq!(out.total, 1);
    assert_eq!(out.succeeded, 1, "逐条结果：{:?}", out.items);
    assert!(out.items[0].ok, "这一条必须成功：{:?}", out.items[0]);

    crate::business::flow::test_support::assert_instance_status(instance_id, "APPROVED").await;
    assert_eq!(
        live_config_rows("STOCK_OUT", "DRAFT", "POSTED").await,
        1,
        "批量同意后必须落一行配置（修复前这里是 0：审批单 APPROVED 但配置不生效）"
    );
    assert!(
        !stock_out_allows_direct_post().await,
        "落边之后 DRAFT → POSTED 必须从有效边里消失"
    );
    purge().await;
}

/// ⑬ 批量驳回 = 单条驳回：配置一分不动，只留一条审计。
#[tokio::test]
async fn batch_task_reject_keeps_edge_config_untouched() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    prepare().await;

    let instance_id = submit_change("STOCK_OUT", "DRAFT", "POSTED").await;
    let task_id = pending_task_of(instance_id).await;
    let out = batch_tasks_with_return(
        super_claims().await,
        vec![task_id.to_string()],
        "REJECT",
        Some("不同意：这条捷径要保留"),
        Some("NONE"),
    )
    .await
    .expect("批量驳回");
    assert_eq!(out.succeeded, 1, "逐条结果：{:?}", out.items);
    crate::business::flow::test_support::assert_instance_status(instance_id, "REJECTED").await;
    assert_eq!(
        live_config_rows("STOCK_OUT", "DRAFT", "POSTED").await,
        0,
        "驳回不该留下任何配置行"
    );
    assert!(stock_out_allows_direct_post().await, "驳回后这条边必须还在");
    let audits: i64 = db()
        .scalar_sql(
            "SELECT count(*) AS v FROM biz_status_log \
              WHERE entity_type = 'FLOW_EDGE_CFG' AND to_status = 'REJECTED'"
                .to_string(),
            vec![],
        )
        .await
        .expect("数审计行");
    assert!(audits >= 1, "批量驳回也要留痕");
    purge().await;
}

/// ⑭ 非法主键只让**那一条**失败：其余照常执行，且结果按入参顺序回填。
#[tokio::test]
async fn batch_task_invalid_uuid_only_fails_that_item() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    prepare().await;

    let instance_id = submit_change("PURCHASE_ORDER", "SUBMITTED", "CLOSED").await;
    let task_id = pending_task_of(instance_id).await;
    let out = batch_tasks(
        super_claims().await,
        vec![task_id.to_string(), "not-a-uuid".to_string(), String::new()],
        "APPROVE",
        None,
    )
    .await
    .expect("批量同意");
    assert_eq!(out.total, 3);
    assert_eq!(out.succeeded, 1, "只有第一条是合法待办：{:?}", out.items);
    assert_eq!(out.failed, 2);
    assert_eq!(out.items[0].task_id, task_id.to_string());
    assert!(out.items[0].ok, "合法的那条必须成功");
    assert!(!out.items[1].ok);
    assert_eq!(out.items[1].code, error_code::FLOW_FIELD_INVALID);
    assert!(out.items[1].message.contains("格式非法"), "{:?}", out.items[1]);
    assert!(!out.items[2].ok, "空串也算格式非法");
    assert_eq!(out.items[2].code, error_code::FLOW_FIELD_INVALID);
    assert_eq!(
        live_config_rows("PURCHASE_ORDER", "SUBMITTED", "CLOSED").await,
        1,
        "合法的那条已经批完 → 必须落边（一条坏 id 不该拖垮整批）"
    );
    purge().await;
}

/// ⑮ 同一条待办批两次：引擎按幂等成功返回，**配置不会写成两行**。
#[tokio::test]
async fn batch_task_repeated_approval_keeps_single_row() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    prepare().await;

    let instance_id = submit_change("STOCK_OUT", "DRAFT", "POSTED").await;
    let task_id = pending_task_of(instance_id).await;
    let claims = super_claims().await;
    let first = batch_tasks(claims.clone(), vec![task_id.to_string()], "APPROVE", None)
        .await
        .expect("第一次批量同意");
    assert_eq!(first.succeeded, 1);
    let second = batch_tasks(claims, vec![task_id.to_string()], "APPROVE", None)
        .await
        .expect("第二次调用本身成功（引擎幂等）");
    assert_eq!(second.total, 1);
    assert_eq!(
        live_config_rows("STOCK_OUT", "DRAFT", "POSTED").await,
        1,
        "重复审批不能把配置写成两行（第二次必须幂等，不是又落一次）"
    );
    crate::business::flow::test_support::assert_instance_status(instance_id, "APPROVED").await;
    purge().await;
}

/// ⑯ 批量与单条**同口径**：同一个不存在的待办，两条路给出同一个业务码。
#[tokio::test]
async fn batch_task_error_code_matches_single_approve() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    prepare().await;

    let claims = super_claims().await;
    let ghost = Uuid::now_v7();
    let single = crate::business::flow::handler::approve_task(
        Extension(AuthClaims(claims.clone())),
        axum::extract::Path(ghost.to_string()),
        Some(Json(crate::business::flow::dto::ApproveReq { comment: None })),
    )
    .await
    .expect_err("不存在的待办必须报错");
    let batched = batch_tasks(claims, vec![ghost.to_string()], "APPROVE", None)
        .await
        .expect("批量本身成功（逐条失败）");
    assert_eq!(batched.failed, 1);
    assert!(!batched.items[0].ok);
    assert_eq!(
        batched.items[0].code, single.code,
        "批量必须与单条同一个业务码（批量层只是薄壳）：single={} batch={}",
        single.code, batched.items[0].code
    );
    assert_eq!(
        batched.items[0].message, single.msg,
        "失败文案也要与单条逐字一致（直接取自单条接口的 msg）"
    );
    purge().await;
}

/// ⑰ 批量驳回的意见门槛与单条一致（< 5 字 → 400 + 65005，且不动配置）。
#[tokio::test]
async fn batch_task_reject_requires_five_char_comment() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    prepare().await;

    let instance_id = submit_change("STOCK_OUT", "DRAFT", "POSTED").await;
    let task_id = pending_task_of(instance_id).await;
    let error = batch_tasks(super_claims().await, vec![task_id.to_string()], "REJECT", Some("不行"))
        .await
        .expect_err("驳回意见不足 5 字必须被拒");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::FLOW_FIELD_INVALID);
    assert_eq!(live_config_rows("STOCK_OUT", "DRAFT", "POSTED").await, 0);
    crate::business::flow::test_support::assert_instance_status(instance_id, "PENDING").await;
    purge().await;
}

/// ⑱ 没有 `business:flow:approve` 的账号：整批 403（不是「默默 0 条」）。
#[tokio::test]
async fn batch_task_requires_approve_permission() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    let error = batch_tasks(plain_claims(), vec![Uuid::now_v7().to_string()], "APPROVE", None)
        .await
        .expect_err("缺权限码必须 403");
    assert_eq!(error.status, 403, "报文：{}", error.msg);
}

/// ⑲ 老契约的上限与动作白名单仍在（50 条 / APPROVE|REJECT）。
#[tokio::test]
async fn batch_task_rejects_oversized_batch_and_illegal_action() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    let ids: Vec<String> = (0..51).map(|_| Uuid::now_v7().to_string()).collect();
    let too_many = batch_tasks(super_claims().await, ids, "APPROVE", None)
        .await
        .expect_err("超过 50 条必须 400");
    assert_eq!(too_many.status, 400);
    assert_eq!(too_many.code, error_code::FLOW_FIELD_INVALID);

    let illegal = batch_tasks(super_claims().await, vec![Uuid::now_v7().to_string()], "DELETE", None)
        .await
        .expect_err("动作非法必须 400");
    assert_eq!(illegal.status, 400);
    assert_eq!(illegal.code, error_code::FLOW_FIELD_INVALID);
}

/// ⑳ 空 `task_ids` 仍然 400（老契约不变）。
#[tokio::test]
async fn batch_task_empty_ids_is_rejected() {
    if !ensure_db().await {
        return;
    }
    let _guard = DB_LOCK.lock().await;
    let error = batch_tasks(super_claims().await, vec![], "APPROVE", None)
        .await
        .expect_err("空列表必须 400");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::FLOW_FIELD_INVALID);
}
