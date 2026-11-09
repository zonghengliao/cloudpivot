// ============================================================================
// transfer_handler_db_tests_status.rs —— 真库：六态状态机 — 审核（待审批→通过/驳回）/ 发出 / 在途 / 收货 / 作废 + 审批发起与回调
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::db_tests_fixtures::*;
use super::*;
use crate::business::erp::transfer_model::{self, STATUS_PENDING};
use crate::business::erp::transfer_model::{
    STATUS_APPROVED, STATUS_CANCELLED, STATUS_DONE, STATUS_DRAFT, STATUS_IN_TRANSIT,
};
use crate::business::flow::graph::{canonical_json, parse_snapshot};
use crate::business::flow::{engine, model as flow_model};
use crate::shared::error_code;
use nik::prelude::*;
use nik::uuid::Uuid;

// ---------- ⑦ 状态机：DRAFT → PENDING ----------
#[tokio::test]
async fn draft_to_pending_writes_submitted_at() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let (_, row) = transfer_in_status(STATUS_PENDING).await;
    assert_eq!(row.status, STATUS_PENDING);
    assert!(row.submitted_at.is_some(), "PENDING 必须有 submitted_at");
    assert!(row.approved_at.is_none());
    // 「配了流程就回写 approval_instance_id」由 submit_starts_… 钉住；
    // 这里**不**断言「没有实例」——同一个测试库里可能还留着别的卡种下的
    // STOCK_TRANSFER 流程（e2e 脚本会发一条），那种环境下提交本来就该发起审批。
    assert!(
        row.approval_instance_id.is_some()
            || transfer_model::published_transfer_flow()
                .await
                .expect("查询已发布流程")
                .is_none(),
        "没配流程时不该有审批单"
    );
}

// ---------- ⑧ 状态机：DRAFT → DONE 跳级 → 409 ----------
#[tokio::test]
async fn draft_to_done_is_rejected() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_transfer(&body(vec![item(M_PLAIN, "1")]), Some(uuid(U_APPLICANT)), None)
        .await
        .expect("建单");
    let err = change_transfer_status(id, &status_req(STATUS_DONE, None), uuid(U_APPLICANT))
        .await
        .expect_err("跳级必须 409");
    assert_eq!(err.status, 409);
    assert!(err.msg.contains("非法状态流转"), "{}", err.msg);
    assert_eq!(refresh(id).await.status, STATUS_DRAFT, "状态不得被改动");
}

// ---------- ⑨ 状态机：PENDING → APPROVED（走审批主线） ----------
#[tokio::test]
async fn pending_to_approved_writes_approved_at() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let (_, row) = transfer_in_status(STATUS_APPROVED).await;
    assert_eq!(row.status, STATUS_APPROVED);
    assert!(row.submitted_at.is_some());
    assert!(row.approved_at.is_some());
    // P5 起这条边只由**审批回调**走：approver_id 记的是点「同意」的那个审批人
    // （不是申请人自己），人工直推已被门闸 65030 拦住
    assert!(
        row.approver_id.is_some(),
        "审批通过要在 approver_id 记实际审批人（approval_callback 从待办取）"
    );
    assert_ne!(
        row.approver_id,
        Some(uuid(U_APPLICANT)),
        "审批人不是申请人本人（自审策略 ALLOW 也要求走审批待办）"
    );
}

// ---------- ⑩ 状态机：PENDING → CANCELLED（撤回） ----------
#[tokio::test]
async fn pending_to_cancelled_requires_reason() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let (id, _) = transfer_in_status(STATUS_PENDING).await;
    // 不给原因 → 400（库约束要求 cancel_reason 非空白）
    let err = change_transfer_status(id, &status_req(STATUS_CANCELLED, None), uuid(U_APPLICANT))
        .await
        .expect_err("撤回必须给原因");
    assert_eq!(err.status, 400);
    // 给原因 → CANCELLED
    change_transfer_status(id, &status_req(STATUS_CANCELLED, Some("需求取消")), uuid(U_APPLICANT))
        .await
        .expect("撤回必须成功");
    let row = refresh(id).await;
    assert_eq!(row.status, STATUS_CANCELLED);
    assert_eq!(row.cancel_reason.as_deref(), Some("需求取消"));
    assert!(row.cancelled_at.is_some());
    assert!(row.submitted_at.is_some(), "撤回不清理提交时间（CANCELLED 分支不约束）");
}

// ---------- ⑪ 状态机：APPROVED → IN_TRANSIT（回填在途仓） ----------
#[tokio::test]
async fn approved_to_in_transit_fills_transit_warehouse() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let (_, row) = transfer_in_status(STATUS_IN_TRANSIT).await;
    assert_eq!(row.status, STATUS_IN_TRANSIT);
    assert!(row.shipped_at.is_some());
    let transit = transfer_model::transit_warehouse_id()
        .await
        .expect("查询在途仓")
        .expect("V0139 必须种好 WH-TRANSIT");
    assert_eq!(row.transit_warehouse_id, Some(transit));
}

// ---------- ⑫ 状态机：IN_TRANSIT → DONE ----------
#[tokio::test]
async fn in_transit_to_done_writes_received_at() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let (_, row) = transfer_in_status(STATUS_DONE).await;
    assert_eq!(row.status, STATUS_DONE);
    assert!(row.received_at.is_some());
    // W3c 起：DONE 会结算库存，明细的 shipped_qty / received_qty 都要回填成 qty
    // （库存侧的断言见 w3c_settle_* 系列用例）
    let items = transfer_model::items_of(row.id).await.expect("明细");
    assert_eq!(items[0].shipped_qty, items[0].qty);
    assert_eq!(items[0].received_qty, items[0].qty);
}

// ---------- ⑬ 状态机：DONE 是终态（任何目标 409） ----------
#[tokio::test]
async fn done_rejects_every_transition() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let (id, _) = transfer_in_status(STATUS_DONE).await;
    for target in [STATUS_DRAFT, STATUS_CANCELLED, STATUS_PENDING] {
        let err = change_transfer_status(id, &status_req(target, Some("终态试探")), uuid(U_APPLICANT))
            .await
            .expect_err("DONE 之后任何流转都必须 409");
        assert_eq!(err.status, 409, "目标 {target}");
    }
    assert_eq!(refresh(id).await.status, STATUS_DONE);
}

// ---------- ⑭ / ⑮ 删除：DRAFT 能删、IN_TRANSIT 不能删 ----------
#[tokio::test]
async fn delete_allows_draft_and_rejects_in_transit() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let applicant = uuid(U_APPLICANT);
    let draft = create_transfer(&body(vec![item(M_PLAIN, "1")]), Some(applicant), None)
        .await
        .expect("建单");
    delete_transfer(draft, Some(applicant)).await.expect("DRAFT 必须能删");
    assert!(
        transfer_model::find_by_id(draft).await.expect("查询").is_none(),
        "软删后按主键必须查不到"
    );

    let (transit, _) = transfer_in_status(STATUS_IN_TRANSIT).await;
    let err = delete_transfer(transit, Some(applicant))
        .await
        .expect_err("IN_TRANSIT 不能删");
    assert_eq!(err.status, 409);
    assert!(err.msg.contains("在途"), "{}", err.msg);
    assert_eq!(refresh(transit).await.status, STATUS_IN_TRANSIT);
}

// ---------- ⑯ 审批：有流程时提交即生成 approval_instance_id ----------
#[tokio::test]
async fn submit_starts_approval_instance_when_flow_published() {
    let Some(_guard) = prepare().await else {
        return;
    };
    ensure_seed_flow().await;
    let (_, row) = transfer_in_status(STATUS_PENDING).await;
    let instance_id = row
        .approval_instance_id
        .expect("配了已发布的 STOCK_TRANSFER 流程就必须回写 approval_instance_id");
    let instance = flow_model::find_instance(instance_id)
        .await
        .expect("查询审批实例")
        .expect("审批实例必须存在");
    assert_eq!(instance.biz_type, "STOCK_TRANSFER");
    assert_eq!(instance.biz_id, Some(row.id));
    assert_eq!(instance.biz_no.as_deref(), Some(row.transfer_no.as_str()));
    assert_eq!(instance.status, "PENDING", "审批人未处理前必须是 PENDING");
}

// ---------- ⑰ 审批回调：APPROVED / REJECTED ----------
#[tokio::test]
async fn approval_callback_moves_transfer() {
    let Some(_guard) = prepare().await else {
        return;
    };
    ensure_seed_flow().await;
    let applicant = uuid(U_APPLICANT);

    // APPROVED → 调拨单 APPROVED
    let (approved_id, approved_row) = transfer_in_status(STATUS_PENDING).await;
    let instance_id = approved_row.approval_instance_id.expect("审批实例 id");
    crate::business::flow::task_bridge::on_transfer_settled(Some(instance_id), "APPROVED").await;
    let row = refresh(approved_id).await;
    assert_eq!(row.status, STATUS_APPROVED);
    assert!(row.approved_at.is_some());

    // REJECTED → 打回 DRAFT（审批意见写 reason，时间戳清空）
    let rejected_id = create_transfer(&body(vec![item(M_PLAIN, "1")]), Some(applicant), None)
        .await
        .expect("建单");
    change_transfer_status(rejected_id, &status_req(STATUS_PENDING, None), applicant)
        .await
        .expect("提交审批");
    let rejected_row = refresh(rejected_id).await;
    let instance_id = rejected_row.approval_instance_id.expect("审批实例 id");
    // 引擎把驳回意见写在待办行；这里直接断言「意见进了 reason」这条链路
    db().execute_with_params(
        "UPDATE biz_flow_instance SET result_comment = '不同意，数量再核' WHERE id = ?::uuid",
        vec![Value::from(&instance_id)],
    )
    .await
    .expect("写审批意见");
    crate::business::flow::task_bridge::on_transfer_settled(Some(instance_id), "REJECTED").await;
    let row = refresh(rejected_id).await;
    assert_eq!(row.status, STATUS_DRAFT, "驳回必须打回 DRAFT");
    assert!(row.submitted_at.is_none(), "DRAFT 分支要求时间戳全空");
    assert!(row.approved_at.is_none());
    assert_eq!(row.reason.as_deref(), Some("不同意，数量再核"));
    assert!(row.approval_instance_id.is_none(), "打回后要能重新提交");
}

// ---------- ⑰-b（P5 ①）迁移种的流程：PUBLISHED + 默认 + 图 hash 自洽 ----------
//
// 证明的是「种子迁移与引擎的自检口径一致」：`maintain.rs` 每天用
// `hash(发布快照)` 与 `hash(节点/边行)` 对账 `graph_hash`，种子若有一处对不上，
// 巡检会天天报 drift。这里把快照重新 canonical 化后重算 sha256，与落库的 `graph_hash` 比对。
#[tokio::test]
async fn seed_flow_is_published_with_self_consistent_hash() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let (def_id, version_id) = ensure_seed_flow().await;

    // ① 定义行：存在 / 编码 / 业务类型 / 默认
    let defs = db()
        .find_with_params(
            "SELECT flow_code, biz_type, status, is_default FROM biz_flow_def \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&def_id)],
        )
        .await
        .expect("查流程定义");
    let def = defs.first().expect("① 迁移种后 biz_flow_def 必须有 stock_transfer");
    assert_eq!(def.get_typed::<String>("flow_code").expect("code"), "stock_transfer");
    assert_eq!(def.get_typed::<String>("biz_type").expect("biz_type"), "STOCK_TRANSFER");

    // ① 版本行：PUBLISHED + 快照/hash/发布时间 + 计数
    let versions = db()
        .find_with_params(
            "SELECT version_status, graph_hash, node_count, edge_count, \
                    graph_snapshot::text AS snapshot_text \
               FROM biz_flow_def_version WHERE id = ?::uuid",
            vec![Value::from(&version_id)],
        )
        .await
        .expect("查流程版本");
    let version = versions.first().expect("① 迁移种后必须有版本行");
    assert_eq!(
        version.get_typed::<String>("version_status").expect("status"),
        "PUBLISHED"
    );
    assert_eq!(version.get_typed::<i32>("node_count").expect("nodes"), 3);
    assert_eq!(version.get_typed::<i32>("edge_count").expect("edges"), 2);

    // ① 图内容：快照能被引擎解析，且重算 hash == 落库 hash
    let text: String = version.get_typed("snapshot_text").expect("snapshot");
    let snapshot = parse_snapshot(&text).expect("① 快照必须能被 graph::parse_snapshot 解析");
    let recomputed = hex_sha256(&canonical_json(&serde_json::to_value(&snapshot).expect("快照序列化")));
    assert_eq!(
        recomputed,
        version.get_typed::<String>("graph_hash").expect("hash"),
        "① 快照重算的 sha256 必须等于落库 graph_hash（否则 D4d 每日自检会报 drift）"
    );
    let keys: Vec<&str> = snapshot.nodes.iter().map(|n| n.key.as_str()).collect();
    assert_eq!(keys, vec!["e", "n_approve", "s"], "① 节点按 key 升序（canonical 前提）");
    let approver = snapshot
        .nodes
        .iter()
        .find(|node| node.key == "n_approve")
        .expect("审批节点");
    assert_eq!(approver.node_type, "APPROVAL");
    // V0251（P0-FIX-2B-B）起种子口径从「ADMIN = 全部超管」改成业务角色
    // `WAREHOUSE_MANAGER`；「任何环境都有人收得到待办」由 empty_assignee 兜底，
    // 所以这里同时钉住「来源 = ROLE」与「空角色 → FALLBACK_ADMIN」两条，
    // 改种子时漏掉兜底也会红。
    assert_eq!(
        approver.assignee_type.as_deref(),
        Some("ROLE"),
        "① 审批人来源 = 业务角色（V0251 去 ADMIN 化；不再是 ADMIN 展开全部超管）"
    );
    assert_eq!(
        approver.empty_assignee, "FALLBACK_ADMIN",
        "① 角色无人时必须兜底到超管，保证任何环境都有人收得到待办"
    );

    // ① 行侧与快照侧一致（node/edge 行是编辑器读的来源，不一致会让「打开流程图」看到旧图）
    let node_rows: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_flow_node WHERE version_id = ?::uuid",
            vec![Value::from(&version_id)],
        )
        .await
        .expect("查节点行数");
    assert_eq!(node_rows, 3, "① 节点行 3 条（与快照一致）");
}

// ---------- ⑰-c（P5 ③）提交审批后审批人待办里出现该调拨单 ----------
//
// V0251（P0-FIX-2B-B 去 ADMIN 化）把 `n_approve` 的审批人来源从 `ADMIN`（展开**全部超管**）
// 改成业务角色 `WAREHOUSE_MANAGER`，角色无人时按 `empty_assignee = FALLBACK_ADMIN` 兜底。
// 所以「派几条、派给谁」不再写死超管人数，而是**按库里节点的真实绑定算期望**：
// 改种子（换角色 / 换兜底策略）时用例自动跟着走，只有「引擎派单与绑定不一致」才红。
#[tokio::test]
async fn submit_creates_pending_todo_for_seed_assignee() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let (_, version_id) = ensure_seed_flow().await;
    let (id, row) = transfer_in_status(STATUS_PENDING).await;
    let instance_id = row.approval_instance_id.expect("② 提交审批必须生成实例");

    let todos = db()
        .find_with_params(
            "SELECT t.status, t.assignee_id, t.assignee_name, t.assignee_source, t.node_key \
               FROM biz_flow_task t WHERE t.instance_id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查审批待办");

    // ① 节点真实绑定（当前版本）+ 空角色策略
    let binds = db()
        .find_with_params(
            "SELECT a.assignee_type, coalesce(r.role_code, '') AS role_code, n.empty_assignee \
               FROM biz_flow_node n \
               JOIN biz_flow_node_assignee a ON a.node_id = n.id \
               LEFT JOIN sys_role r ON r.id = a.ref_id \
              WHERE n.version_id = ?::uuid AND n.node_key = 'n_approve'",
            vec![Value::from(&version_id)],
        )
        .await
        .expect("查审批人绑定");
    let bind = binds.first().expect("① 种子审批节点必须有审批人绑定");
    let bind_type = bind.get_typed::<String>("assignee_type").expect("assignee_type");
    let role_code: String = bind.get_typed("role_code").expect("role_code");
    let empty_policy = bind.get_typed::<String>("empty_assignee").expect("empty_assignee");

    // ② 由绑定推导「期望收件人与来源」（角色无人 → 兜底超管）
    let admins = crate::business::flow::test_support::super_admin_count().await;
    let (expect_source, expect_count) = match bind_type.as_str() {
        "ROLE" => {
            let holders: i64 = db()
                .scalar_with_params(
                    "SELECT count(*) FROM sys_user u \
                       JOIN sys_user_role ur ON ur.user_id = u.id \
                       JOIN sys_role r ON r.id = ur.role_id \
                      WHERE r.role_code = ? AND u.deleted_at IS NULL",
                    vec![Value::Text(role_code.clone())],
                )
                .await
                .expect("查角色成员");
            if holders == 0 {
                assert_eq!(
                    empty_policy, "FALLBACK_ADMIN",
                    "① 角色 `{role_code}` 无人时种子必须兜底到超管，否则提交审批没人收待办"
                );
                ("ADMIN".to_string(), admins)
            } else {
                ("ROLE".to_string(), holders)
            }
        }
        "ADMIN" => ("ADMIN".to_string(), admins),
        other => panic!(
            "① 本用例只覆盖种子节点来源为 ROLE / ADMIN 的口径，实际是 {other}：\
             换了来源类型请同步补断言（不允许静默跳过）"
        ),
    };
    assert!(
        expect_count > 0,
        "③ 期望收件人必须 > 0（来源 {expect_source}，否则审批中心里没人看得到）"
    );
    assert_eq!(
        todos.len() as i64,
        expect_count,
        "③ 单节点审批：期望 {expect_count} 条待办（来源 {expect_source}；同一人不得重复派单），实际 {}",
        todos.len()
    );
    let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for todo in &todos {
        assert_eq!(todo.get_typed::<String>("status").expect("status"), "PENDING");
        assert_eq!(todo.get_typed::<String>("node_key").expect("node"), "n_approve");
        assert_eq!(
            todo.get_typed::<String>("assignee_source").expect("source"),
            expect_source,
            "③ 待办的 assignee_source 必须与节点绑定一致（{expect_source}）"
        );
        let assignee: Uuid = todo.get_typed("assignee_id").expect("assignee_id");
        assert!(seen.insert(assignee), "③ 同一人不得被重复派单");
        // 待办必须落在「真实存在、且确实是该来源成员」的账号上（否则审批中心里没人看得到）
        let member: i64 = if expect_source == "ROLE" {
            db().scalar_with_params(
                "SELECT count(*) FROM sys_user u \
                   JOIN sys_user_role ur ON ur.user_id = u.id \
                   JOIN sys_role r ON r.id = ur.role_id \
                  WHERE u.id = ?::uuid AND r.role_code = ? AND u.deleted_at IS NULL",
                vec![Value::from(&assignee), Value::Text(role_code.clone())],
            )
            .await
            .expect("查角色成员")
        } else {
            db().scalar_with_params(
                "SELECT count(*) FROM sys_user u \
                   JOIN sys_user_role ur ON ur.user_id = u.id \
                   JOIN sys_role r ON r.id = ur.role_id \
                  WHERE u.id = ?::uuid AND r.role_code = 'SUPER_ADMIN' AND u.deleted_at IS NULL",
                vec![Value::from(&assignee)],
            )
            .await
            .expect("查超管角色")
        };
        assert_eq!(member, 1, "③ 待办审批人必须是 {expect_source} 的合法成员（{assignee}）");
    }

    // ③ 按调拨单反查也能命中（审批中心「待办」按 biz_type + biz_id 过滤的口径）
    let hit: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_flow_instance i \
               JOIN biz_flow_task t ON t.instance_id = i.id \
              WHERE i.biz_type = 'STOCK_TRANSFER' AND i.biz_id = ?::uuid \
                AND t.status = 'PENDING'",
            vec![Value::from(&id)],
        )
        .await
        .expect("按单据反查待办");
    assert_eq!(
        hit, expect_count,
        "③ 审批中心里能按调拨单找到全部 {expect_count} 条待办（实际 {hit}）"
    );
}

// ---------- ⑰-d（P5 ⑦）审批中不许人工直推 APPROVED（409 + 65030） ----------
#[tokio::test]
async fn manual_approve_blocked_while_approval_pending() {
    let Some(_guard) = prepare().await else {
        return;
    };
    ensure_seed_flow().await;
    let applicant = uuid(U_APPLICANT);
    let (id, row) = transfer_in_status(STATUS_PENDING).await;
    let instance_id = row.approval_instance_id.expect("实例 id");

    let err = change_transfer_status(id, &status_req(STATUS_APPROVED, None), applicant)
        .await
        .expect_err("⑦ 审批还在跑时，人工直推 APPROVED 必须被拒");
    assert_eq!(err.status, 409, "⑦ HTTP 409（不是 500/400）");
    assert_eq!(err.code, error_code::TRANSFER_FLOW_GATE_BLOCKED, "⑦ 业务码 65030");
    assert_eq!(err.code, 65030);
    assert!(err.msg.contains("审批中"), "⑦ 文案要说清「在审批中」：{}", err.msg);

    // 单据与审批单都原地不动
    assert_eq!(refresh(id).await.status, STATUS_PENDING);
    let instance = flow_model::find_instance(instance_id)
        .await
        .expect("查实例")
        .expect("实例存在");
    assert_eq!(instance.status, "PENDING", "⑦ 被拦后审批单仍是 PENDING");

    // 撤回仍放行（正规出口），撤回会终止审批单
    change_transfer_status(id, &status_req(STATUS_CANCELLED, Some("不调了")), applicant)
        .await
        .expect("⑦ 门闸只拦 APPROVED，撤回必须放行");
    assert_eq!(refresh(id).await.status, STATUS_CANCELLED);
}

// ---------- ⑰-e（P5 ⑥）没配流程时：提交成功但无实例，允许人工推 APPROVED ----------
#[tokio::test]
async fn submit_without_flow_degrades_to_manual_approve() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let (def_id, _) = ensure_seed_flow().await;
    // 模拟「客户没配流程」：软删种子定义（降级路径的唯一判据是「查不到已发布流程」）
    exec(
        "UPDATE biz_flow_def SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&def_id)],
        "软删调拨流程（模拟未配置）",
    )
    .await;
    assert!(
        transfer_model::published_transfer_flow()
            .await
            .expect("查流程")
            .is_none(),
        "⑥ 前置：软删后必须查不到已发布流程"
    );

    let applicant = uuid(U_APPLICANT);
    let id = create_transfer(&body(vec![item(M_PLAIN, "2")]), Some(applicant), None)
        .await
        .expect("建单");
    change_transfer_status(id, &status_req(STATUS_PENDING, None), applicant)
        .await
        .expect("⑥ 没配流程时提交审批必须照样成功（降级，不是 500）");
    let row = refresh(id).await;
    assert_eq!(row.status, STATUS_PENDING);
    assert!(
        row.approval_instance_id.is_none(),
        "⑥ 降级路径：没有实例可挂，approval_instance_id 必须为空"
    );

    // 降级路径下人工推 APPROVED 必须放行（否则单据彻底卡死）
    change_transfer_status(id, &status_req(STATUS_APPROVED, None), applicant)
        .await
        .expect("⑥ 没审批单时人工推 APPROVED 必须放行");
    let row = refresh(id).await;
    assert_eq!(row.status, STATUS_APPROVED);
    assert!(row.approved_at.is_some(), "⑥ 人工批准要写 approved_at");

    // 复原（prepare 里也会自愈，这里显式恢复以免影响同批次后续断言）
    exec(
        "UPDATE biz_flow_def SET deleted_at = NULL WHERE id = ?::uuid",
        vec![Value::from(&def_id)],
        "恢复调拨流程",
    )
    .await;
}

// ---------- ⑰-f（P5 ⑨）撤回（PENDING → CANCELLED）终止在跑的审批单 ----------
#[tokio::test]
async fn withdraw_cancels_running_approval_instance() {
    let Some(_guard) = prepare().await else {
        return;
    };
    ensure_seed_flow().await;
    let applicant = uuid(U_APPLICANT);
    let (id, row) = transfer_in_status(STATUS_PENDING).await;
    let instance_id = row.approval_instance_id.expect("实例 id");

    change_transfer_status(id, &status_req(STATUS_CANCELLED, Some("仓库临时不调了")), applicant)
        .await
        .expect("⑨ 撤回必须成功");
    let row = refresh(id).await;
    assert_eq!(row.status, STATUS_CANCELLED);
    assert!(row.cancelled_at.is_some());

    let instance = flow_model::find_instance(instance_id)
        .await
        .expect("查实例")
        .expect("实例存在");
    assert_eq!(
        instance.status, "CANCELLED",
        "⑨ 撤回必须把在跑审批单一起终止（否则 uk_biz_flow_instance_biz_active 会让重新提交永久 409）"
    );
    assert!(
        instance.ended_at.is_some(),
        "⑨ 终止的实例必须有 ended_at（ck_biz_flow_instance_end）"
    );
    // 待办也不能留 PENDING（脏待办会污染审批中心的待办数与统计）
    let pending: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查待办");
    assert_eq!(pending, 0, "⑨ 撤回后不许留 PENDING 待办");
}

/// 同一单据**已经有审批中实例**时再提交：必须是 `409 + 单号 + 「已有审批中实例」`。
///
/// 三条路径都钉住（卡面 §要做的事 2）：
/// 1. **应用层预检**（`prepare_start` → `biz_instance_running`）命中 → 409，文案带单号。
///    构造方式就是卡面残留现场：单据还停在 `DRAFT`，但已经挂着一张在跑的审批单
///    （上一轮 panic 没清干净 / 库被别的入口写脏）——**只有这种状态才会走到预检**，
///    「已 PENDING 再提交」会被状态机 409 先拦（非法边），测不到这一步。
/// 2. **库层兜底**（并发双击绕过预检，撞 `uk_biz_flow_instance_biz_active`）→
///    真去跑一次 `engine::start`，拿**真驱动**返回的错误验 `is_biz_active_duplicate`。
///    这一步是关键：只按变体判、驱动又把错误降级成 `DbError::Query` 时，
///    旧代码会掉进 `utils::db_error` → `500 服务器内部错误`（卡面排查几十分钟的那个坑）。
#[tokio::test]
async fn submit_twice_is_409_with_transfer_no_not_500() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let applicant = uuid(U_APPLICANT);
    let id = create_transfer(&body(vec![item(M_PLAIN, "3")]), Some(applicant), Some("w3b_applicant"))
        .await
        .expect("建单必须成功");
    let row = refresh(id).await;
    let start = prepare_start(&row, applicant)
        .await
        .expect("准备审批参数")
        .expect("种子流程是 PUBLISHED，必须返回发起参数");
    // 手工发起一张实例、**不动单据状态**（= 残留现场：DRAFT 单据 + 在跑实例）
    let tx = db().begin().await.expect("开事务");
    let out = engine::start(&tx, &start).await.expect("发起审批单应成功");
    tx.commit().await.expect("提交事务");
    let instance_id = out.instance_id.expect("必须有实例 id");

    // ① 应用层预检：单据仍是 DRAFT，提交 → 预检命中 → 409 + 单号
    let err = change_transfer_status(id, &status_req(STATUS_PENDING, None), applicant)
        .await
        .expect_err("已有审批中实例，提交必须 409");
    assert_eq!(err.status, 409, "不能是 500：{}", err.msg);
    assert!(err.msg.contains(&row.transfer_no), "409 文案必须带单号：{}", err.msg);
    assert!(
        err.msg.contains("审批中实例"),
        "409 文案必须说清「已有审批中实例」：{}",
        err.msg
    );

    // ② 库层兜底：绕过预检，直接用同一个 (biz_type, biz_id) 再发一张实例
    let inst = db()
        .find_with_params(
            "SELECT def_id, version_id, version_no, flow_code, flow_name, biz_type \
               FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查实例模板");
    let src = inst.first().expect("实例必须存在");
    let duplicate = engine::StartReq {
        instance_id: Uuid::now_v7(),
        def_id: src.get_typed("def_id").expect("def_id"),
        version_id: src.get_typed("version_id").expect("version_id"),
        version_no: src.get_typed("version_no").expect("version_no"),
        flow_code: src.get_typed("flow_code").expect("flow_code"),
        flow_name: src.get_typed("flow_name").expect("flow_name"),
        biz_type: src.get_typed("biz_type").expect("biz_type"),
        biz_id: Some(id),
        biz_no: Some(row.transfer_no.clone()),
        title: format!("{} 库存调拨（并发重复提交）", row.transfer_no),
        initiator_id: applicant,
        initiator_name: Some("w3b_applicant".to_string()),
        initiator_dept_id: None,
        form_data: serde_json::json!({}),
    };
    let tx = db().begin().await.expect("开事务");
    let conflict_err = engine::start(&tx, &duplicate)
        .await
        .expect_err("同 biz_id 的第二张在跑实例必须撞 uk_biz_flow_instance_biz_active");
    let _ = tx.rollback().await;
    assert!(
        is_biz_active_duplicate(&conflict_err),
        "真驱动返回的 uk 冲突必须能被识别（识别不了就会变成 500）：{conflict_err}"
    );
    let api = write_error(conflict_err, "调拨单状态流转");
    assert_eq!(api.status, 409, "库层兜底也必须是 409：{}", api.msg);
    assert!(
        api.msg.contains("审批中实例"),
        "库层兜底的 409 也要说清原因：{}",
        api.msg
    );

    // 实例数没有变成 2（第二条被库层挡住，没落库）
    let instances = count_of(
        "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'STOCK_TRANSFER' AND biz_id = ?::uuid",
        vec![Value::from(&id)],
        "本单实例数",
    )
    .await;
    assert_eq!(instances, 1, "在跑实例只能有 1 张");
}

// ---------- ㉒ 收尾清场：同库连跑两遍不留残留（名字 zz_ 排最后 = 模块级 teardown） ----------
/// 本卡跑完**不许留**「单据 / 实例 / 待办 / 状态日志」四类残留。
///
/// 为什么必须有这一条：`cleanup()` 只在**每个用例开头**跑，最后一个用例的产物没人收；
/// 同库连跑第二遍时，残留的 `STOCK_TRANSFER|PENDING` 实例会顶住
/// `uk_biz_flow_instance_biz_active`（下一次提交永久 409/500），
/// `biz_status_log` 则每轮多攒几百行永远对不上的孤儿。
/// 名字以 `zz_` 开头 → 排序排在模块最后，等于给这个模块加了 teardown。
#[tokio::test]
async fn zz_teardown_purges_fixture_residue() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let applicant = uuid(U_APPLICANT);
    // 造一张**带在跑审批单**的单：正好是卡面残留的那四类
    let id = create_transfer(&body(vec![item(M_PLAIN, "2")]), Some(applicant), Some("w3b_applicant"))
        .await
        .expect("建单必须成功");
    change_transfer_status(id, &status_req(STATUS_PENDING, None), applicant)
        .await
        .expect("提交审批应成功");
    let row = refresh(id).await;
    let instance_id = row.approval_instance_id.expect("必须挂实例");
    assert!(
        count_of(
            "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
            vec![Value::from(&instance_id)],
            "清场前待办数",
        )
        .await
            > 0,
        "清场前必须有待办，否则这条用例证明不了什么"
    );

    cleanup().await;

    // ① 单据没了
    assert!(
        transfer_model::find_by_id(id).await.expect("查单据").is_none(),
        "清场后调拨单必须不存在"
    );
    // ② 实例没了
    assert_eq!(
        count_of(
            "SELECT count(*) FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
            "残留实例",
        )
        .await,
        0,
        "清场后审批实例必须为 0"
    );
    // ③ 待办没了
    assert_eq!(
        count_of(
            "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid",
            vec![Value::from(&instance_id)],
            "残留待办",
        )
        .await,
        0,
        "清场后待办必须为 0"
    );
    // ④ 状态日志没了（无外键 → 必须在删单据之前清，否则永远对不上）
    assert_eq!(
        count_of(
            "SELECT count(*) FROM biz_status_log WHERE entity_type = 'STOCK_TRANSFER' AND entity_id = ?::uuid",
            vec![Value::from(&id)],
            "残留状态日志",
        )
        .await,
        0,
        "清场后状态日志必须为 0"
    );
    // ⑤ 全库不剩「在跑的调拨实例」—— 这就是顶住 uk_biz_flow_instance_biz_active 的那个东西
    assert_eq!(
        count_of(
            "SELECT count(*) FROM biz_flow_instance \
              WHERE biz_type = 'STOCK_TRANSFER' AND status = 'PENDING' AND deleted_at IS NULL",
            vec![],
            "在跑的调拨实例",
        )
        .await,
        0,
        "同库连跑第二遍的前提：不许剩 PENDING 的调拨实例"
    );
}
