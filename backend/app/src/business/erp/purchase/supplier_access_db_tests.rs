// ============================================================================
// supplier_access_db_tests.rs —— 供应商准入 / 黑名单流程（M4-14）的真库用例
//
// 卡：t_583852b1（M4-14【M4·采购】供应商准入/黑名单流程）
//
// # 覆盖什么（每条都对应 V0455 里的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `create_draft_shapes` | 建单：单号形状 `SAC-yyyyMMdd-000001`、状态 DRAFT、供应商编码/名称/申请人姓名快照、`editable`/`submittable` 标记 |
// | `duplicate_open_application_is_rejected` | 同一供应商只能有一张未结单（报文点名已有单号，409 + 66100） |
// | `start_guard_rejects_wrong_type` | 起点判据：对未拉黑的供应商发「解除」、对已拉黑的发「准入」都是 409 |
// | `blacklist_two_votes_then_disables_supplier` | **走 flow**：拉黑单两个节点（采购经理 → 财务经理，条件边）→ 申请单 APPROVED + `biz_supplier.status = 0` |
// | `admit_and_release_enable_supplier` | 准入 / 解除走兜底边（一个节点）→ 通过后 status = 1 |
// | `reject_returns_to_draft_and_keeps_supplier` | 驳回 → 申请单打回 DRAFT（意见进备注），**供应商状态一个字节不动** |
// | `disabled_supplier_is_not_usable` | 采购订单建单闸：停用（含被拉黑）供应商 → 409 + 66099 |
// | `self_approval_is_forbidden` | 内控：申请人不能批自己那一票（受 `separation` 开关控制） |
// | `seeded_flow_hash_matches_rows` | V0455 的 graph_snapshot / graph_hash 与节点·边行**逐字节一致**（种子被改过就红） |
// | `zz_teardown_purges_fixtures` | 清场：`M414-` 夹具 0 残留（含流程实例与待办） |
//
// # ⚠ 真库用例必须串行
//
// 共用全局 `db()` 连接池与 `aging_db_tests::DB_LOCK`，跑法：
//
// ```bash
// ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m414_drill" \
//   cargo test --lib business::erp::purchase::supplier_access_db_tests -- --test-threads=1
// ```
//
// 流程种子（`supplier_access`）由 V0455 种下：**没有它时提交会被 409 拦下**（默认拒绝 +
// 显式跳过），这本身是正确行为 —— 本文件要求「有流程」的用例会红，说明演练库没跑全迁移。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::ApprovalGate;
use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::purchase_handler;
use crate::business::erp::purchase::supplier_access_dto::{CreateAccessReq, SubmitAccessReq, UpdateAccessReq};
use crate::business::erp::purchase::supplier_access_flow as flow;
use crate::business::erp::purchase::supplier_access_handler as handler;
use crate::business::erp::purchase::supplier_access_model as model;
use crate::business::flow::engine;
use crate::test_seed;
use cloudpivot_kernel::shared::error_code;

/// 夹具编码前缀（清场按它反查，绝不按生产号段删）。
const PREFIX: &str = "M414-";

// ==================== 底座 ====================

/// 操作人（真实存在的 `sys_user` 行）：`biz_flow_instance.initiator_id` 有外键。
async fn operator() -> Uuid {
    static CACHED: std::sync::OnceLock<Uuid> = std::sync::OnceLock::new();
    if let Some(id) = CACHED.get() {
        return *id;
    }
    let rows = db()
        .find_with_params(
            "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at, username LIMIT 1",
            vec![],
        )
        .await
        .expect("查用户");
    let id = rows
        .first()
        .map(|row| row.get_typed::<Uuid>("id").expect("user id"))
        .expect("模板库里至少要有一个用户（发起审批要外键到 sys_user）");
    let _ = CACHED.set(id);
    id
}

/// 取串行锁 + 探库 + 清场 + 就绪「非申请人」的审批岗位成员。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    // 空库里往往只有一个 admin，而 `supplier_access` 带职责分离内控（申请人会被排除）——
    // 审批岗位必须有**非申请人**成员，用例才走得完真审批（与 M4-02 同一把底座夹具）。
    test_seed::ensure_approver_roles().await;
    Some(guard)
}

/// 清场：待办 → 实例 → 申请单 → 夹具供应商。
async fn cleanup() {
    exec(
        &format!(
            "DELETE FROM biz_flow_task WHERE instance_id IN ( \
                 SELECT i.id FROM biz_flow_instance i \
                  JOIN biz_supplier_access a ON a.id = i.biz_id \
                 WHERE i.biz_type = 'SUPPLIER_ACCESS' AND a.supplier_code LIKE '{PREFIX}%')"
        ),
        vec![],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_flow_instance i USING biz_supplier_access a \
              WHERE i.biz_type = 'SUPPLIER_ACCESS' AND i.biz_id = a.id \
                AND a.supplier_code LIKE '{PREFIX}%'"
        ),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_supplier_access WHERE supplier_code LIKE '{PREFIX}%'"),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_supplier WHERE supplier_code LIKE '{PREFIX}%'"),
        vec![],
    )
    .await;
}

/// 造一家夹具供应商（默认启用），返回 id。
///
/// 建档走 [`test_seed::ensure_supplier`]（P2-62：测试里手写主数据 INSERT 有预算门禁，
/// 只许降不许涨）；需要「停用」时再补一条 UPDATE —— 那是本用例的**状态前提**，不是建档。
async fn new_supplier(tag: &str, status: i16) -> Uuid {
    let code = format!("{PREFIX}{tag}");
    let id = test_seed::ensure_supplier(&code, &format!("夹具供应商 {tag}")).await;
    if status != model::SUPPLIER_ENABLED {
        exec(
            "UPDATE biz_supplier SET status = ? WHERE id = ?::uuid",
            vec![Value::I16(status), Value::from(&id)],
        )
        .await;
    }
    id
}

/// 建一张草稿申请单。
async fn draft(supplier_id: Uuid, access_type: &str, reason: &str) -> Uuid {
    let req = CreateAccessReq {
        supplier_id: supplier_id.to_string(),
        access_type: access_type.to_string(),
        reason: Some(reason.to_string()),
        grade_snapshot: None,
        remark: None,
    };
    handler::create_application(&req, Some(operator().await))
        .await
        .unwrap_or_else(|error| panic!("建单应成功：{} {}", error.code, error.msg))
}

/// 提交审批（走真流程）。
async fn submit(id: Uuid) {
    let req = SubmitAccessReq::default();
    handler::submit_application(id, &req, Some(operator().await), &ApprovalGate::none())
        .await
        .unwrap_or_else(|error| panic!("提交审批应成功：{} {}", error.code, error.msg));
}

/// 实例上的待办（`task_id / node_key / assignee_id`），按创建顺序。
async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, String, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT id AS task_id, node_key, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' \
              ORDER BY created_at",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查待办");
    rows.iter()
        .map(|row| {
            (
                row.get_typed("task_id").expect("task id"),
                row.get_typed("node_key").expect("node key"),
                row.get_typed("assignee_id").expect("assignee"),
            )
        })
        .collect()
}

/// 审批同意一票（与 `flow::handler::approve_task` 同一条引擎路径 + 同款「提交后再回调」）。
async fn approve_task(task_id: Uuid, approver: Uuid) -> String {
    let out = db()
        .transaction(|tx| async move { engine::approve(&tx, task_id, approver, Some("M414 用例同意")).await })
        .await
        .unwrap_or_else(|error| panic!("审批同意失败：{error}"));
    flow::on_sa_settled(out.instance_id, &out.instance_status).await;
    out.instance_status
}

/// 审批驳回一票。
async fn reject_task(task_id: Uuid, approver: Uuid, comment: &str) -> String {
    let out = db()
        .transaction(|tx| async move { engine::reject(&tx, task_id, approver, comment, engine::ReturnTo::Final).await })
        .await
        .unwrap_or_else(|error| panic!("审批驳回失败：{error}"));
    flow::on_sa_settled(out.instance_id, &out.instance_status).await;
    out.instance_status
}

/// 逐节点真派真批，返回（**走了几个节点**，最终状态）。
///
/// ANY 会签模式下同一节点可能有多个候选人**各持一条待办**（模板库里 采购经理 角色就有多名
/// 成员），所以「票数」不等于「节点数」：这里每轮只批第一条（一票即过节点，其余候选待办
/// 由引擎收口），用**走过的节点 key 个数**断言审批链的形状。
async fn approve_until_final(instance_id: Uuid) -> (usize, String) {
    let mut rounds = 0usize;
    let mut last = String::from("PENDING");
    for _ in 0..5 {
        let tasks = pending_tasks(instance_id).await;
        if tasks.is_empty() {
            break;
        }
        let node_key = tasks[0].1.clone();
        assert!(
            tasks.iter().all(|task| task.1 == node_key),
            "同一轮里混了多个节点的待办：{:?}（引擎推进状态可疑）",
            tasks.iter().map(|task| task.1.clone()).collect::<Vec<_>>()
        );
        tracing::info!(%node_key, "M414 用例：审批一票");
        last = approve_task(tasks[0].0, tasks[0].2).await;
        rounds += 1;
        if last == "APPROVED" || last == "REJECTED" {
            break;
        }
    }
    (rounds, last)
}

/// 取申请单的实例 id。
async fn instance_of(access_id: Uuid) -> Uuid {
    let row = model::find_access(db(), access_id)
        .await
        .expect("查申请单")
        .expect("申请单存在");
    row.approval_instance_id.expect("提交后必须有实例 id")
}

/// 供应商当前 status。
async fn supplier_status(supplier_id: Uuid) -> i16 {
    let rows = db()
        .find_with_params(
            "SELECT status FROM biz_supplier WHERE id = ?::uuid",
            vec![Value::from(&supplier_id)],
        )
        .await
        .expect("查供应商");
    rows.first().expect("供应商存在").get_typed("status").expect("status")
}

// ==================== 用例 ====================

/// ① 建单：单号形状 / 状态 / 快照 / 可编辑标记。
#[tokio::test]
async fn create_draft_shapes() {
    let Some(_guard) = setup().await else {
        return;
    };
    let supplier_id = new_supplier("SHAPE", model::SUPPLIER_ENABLED).await;
    let id = draft(supplier_id, model::TYPE_BLACKLIST, "来料连续三批不合格，拟列入黑名单").await;

    let row = model::find_access(db(), id).await.expect("查单").expect("存在");
    // 单号 `SAC-yyyyMMdd-000001`：4 + 8 + 1 + 6 = 19 个字符
    assert_eq!(
        row.access_no.len(),
        19,
        "单号长度必须是 SAC-yyyyMMdd-NNNNNN，实际 {}",
        row.access_no
    );
    assert!(
        row.access_no.starts_with("SAC-"),
        "单号必须以 SAC- 开头，实际 {}",
        row.access_no
    );
    assert!(
        row.access_no[4..12].chars().all(|c| c.is_ascii_digit())
            && row.access_no[13..].chars().all(|c| c.is_ascii_digit()),
        "单号日期段与流水段必须是数字，实际 {}",
        row.access_no
    );
    assert_eq!(row.status, model::STATUS_DRAFT);
    assert_eq!(row.access_type, model::TYPE_BLACKLIST);
    assert_eq!(row.supplier_code, format!("{PREFIX}SHAPE"), "供应商编码必须快照落库");
    assert_eq!(row.supplier_name, "夹具供应商 SHAPE", "供应商名必须快照落库");
    assert!(row.apply_by.is_some(), "申请人必落");
    assert!(
        row.apply_by_name.is_some(),
        "申请人姓名必须快照（约定 13：审计类存姓名）"
    );
    assert_eq!(row.supplier_status, model::SUPPLIER_ENABLED, "建单不改供应商状态");
    assert!(row.approval_instance_id.is_none(), "草稿没有实例");

    let res = crate::business::erp::purchase::supplier_access_dto::to_row_res(&row);
    assert!(res.editable, "草稿必须可编辑");
    assert!(res.submittable, "理由够字 → 可提交");
    assert_eq!(
        res.target_supplier_status,
        model::SUPPLIER_DISABLED,
        "黑名单生效后是停用"
    );

    // 理由不足 5 字 → 不可提交（但草稿仍可存）
    // 换一家供应商：同一供应商同时只能有一张未结单（uk_biz_sa_supplier_open）
    let other = new_supplier("SHAPE-SHORT", model::SUPPLIER_ENABLED).await;
    let short = draft(other, model::TYPE_ADMIT, "改").await;
    let res_short = handler::detail_of(short).await.expect("详情");
    assert!(!res_short.submittable, "理由不足 5 字不许提交");
    assert!(res_short.editable, "但草稿仍可编辑");

    cleanup().await;
}

/// ② 同一供应商只能有一张未结单（报文点名已有单号）。
#[tokio::test]
async fn duplicate_open_application_is_rejected() {
    let Some(_guard) = setup().await else {
        return;
    };
    let supplier_id = new_supplier("DUP", model::SUPPLIER_ENABLED).await;
    let first = draft(supplier_id, model::TYPE_BLACKLIST, "重复建单用例的第一张").await;
    let first_no = model::find_access(db(), first)
        .await
        .expect("查单")
        .expect("存在")
        .access_no;

    let req = CreateAccessReq {
        supplier_id: supplier_id.to_string(),
        access_type: model::TYPE_ADMIT.to_string(),
        reason: Some("重复建单用例的第二张".to_string()),
        grade_snapshot: None,
        remark: None,
    };
    let error = handler::create_application(&req, Some(operator().await))
        .await
        .expect_err("第二张必须被拒");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::ERP_SUPPLIER_ACCESS_STATUS_GUARD);
    assert!(error.msg.contains(&first_no), "报文必须点名已有单号：{}", error.msg);

    cleanup().await;
}

/// ③ 起点判据：一种状态只有一个合法动作。
#[tokio::test]
async fn start_guard_rejects_wrong_type() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 未拉黑 → 不能发「解除」
    let enabled = new_supplier("GUARD-ON", model::SUPPLIER_ENABLED).await;
    let req = CreateAccessReq {
        supplier_id: enabled.to_string(),
        access_type: model::TYPE_RELEASE.to_string(),
        reason: Some("未拉黑却要解除，应被拒".to_string()),
        grade_snapshot: None,
        remark: None,
    };
    let error = handler::create_application(&req, Some(operator().await))
        .await
        .expect_err("未拉黑不能解除");
    assert_eq!(error.status, 409);
    assert!(error.msg.contains("不需要解除"), "报文要说清为什么拒：{}", error.msg);

    // 已拉黑（用一张已通过的黑名单单造出真实状态）→ 不能发「准入」
    let blacklisted = new_supplier("GUARD-BLK", model::SUPPLIER_ENABLED).await;
    let blk = draft(blacklisted, model::TYPE_BLACKLIST, "先把它拉黑再试准入").await;
    submit(blk).await;
    let instance_id = instance_of(blk).await;
    let (votes, final_status) = approve_until_final(instance_id).await;
    assert_eq!(final_status, "APPROVED");
    assert_eq!(votes, 2, "拉黑必须走两个节点（采购经理 → 财务经理）");
    assert_eq!(supplier_status(blacklisted).await, model::SUPPLIER_DISABLED);

    let req = CreateAccessReq {
        supplier_id: blacklisted.to_string(),
        access_type: model::TYPE_ADMIT.to_string(),
        reason: Some("黑名单中直接准入，应被拒".to_string()),
        grade_snapshot: None,
        remark: None,
    };
    let error = handler::create_application(&req, Some(operator().await))
        .await
        .expect_err("黑名单中不能直接准入");
    assert_eq!(error.status, 409);
    assert!(error.msg.contains("解除黑名单"), "报文要指出正确路径：{}", error.msg);

    cleanup().await;
}

/// ④ 拉黑：两票（条件边 form.access_type = BLACKLIST → 财务经理）→ 申请单 APPROVED + 供应商停用。
#[tokio::test]
async fn blacklist_two_votes_then_disables_supplier() {
    let Some(_guard) = setup().await else {
        return;
    };
    let supplier_id = new_supplier("BLK", model::SUPPLIER_ENABLED).await;
    let id = draft(supplier_id, model::TYPE_BLACKLIST, "连续三批来料不合格，建议列入黑名单").await;
    submit(id).await;

    let instance_id = instance_of(id).await;
    // 实例真的挂在 SUPPLIER_ACCESS 上，并且第一站是采购经理
    let rows = db()
        .find_with_params(
            "SELECT biz_type, biz_id, status FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查实例");
    let instance = rows.first().expect("实例存在");
    assert_eq!(
        instance.get_typed::<String>("biz_type").expect("biz_type"),
        "SUPPLIER_ACCESS"
    );
    assert_eq!(
        instance.get_typed::<Uuid>("biz_id").expect("biz_id"),
        id,
        "biz_id 必须指向申请单"
    );
    let tasks = pending_tasks(instance_id).await;
    assert!(!tasks.is_empty(), "提交后必须有待办");
    assert!(
        tasks.iter().all(|task| task.1 == "n_pm"),
        "第一站必须是采购经理审批，实际 {:?}",
        tasks.iter().map(|task| task.1.clone()).collect::<Vec<_>>()
    );

    // 供应商在审批期间**不被改动**（收紧也要等结论）
    assert_eq!(
        supplier_status(supplier_id).await,
        model::SUPPLIER_ENABLED,
        "审批中供应商仍启用"
    );

    let (votes, final_status) = approve_until_final(instance_id).await;
    assert_eq!(final_status, "APPROVED");
    assert_eq!(votes, 2, "拉黑必须走两个节点（采购经理 → 财务经理）");

    let row = model::find_access(db(), id).await.expect("查单").expect("存在");
    assert_eq!(row.status, model::STATUS_APPROVED);
    assert!(row.decided_at.is_some(), "通过必须有结论时间");
    assert!(row.decided_by.is_some(), "通过必须有结论人");
    assert!(row.decided_by_name.is_some(), "结论人姓名快照必落");
    assert_eq!(
        supplier_status(supplier_id).await,
        model::SUPPLIER_DISABLED,
        "拉黑通过 → 供应商停用"
    );

    cleanup().await;
}

/// ⑤ 准入 / 解除走兜底边（一票）→ 通过后供应商启用。
#[tokio::test]
async fn admit_and_release_enable_supplier() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 准入：一家被手工停用、从未准入过的供应商
    let supplier_id = new_supplier("ADMIT", model::SUPPLIER_DISABLED).await;
    let admit = draft(supplier_id, model::TYPE_ADMIT, "资质审核通过，申请进入合格供应商名录").await;
    submit(admit).await;
    let (votes, final_status) = approve_until_final(instance_of(admit).await).await;
    assert_eq!(final_status, "APPROVED");
    assert_eq!(votes, 1, "准入只走采购经理一个节点（兜底边）");
    assert_eq!(
        supplier_status(supplier_id).await,
        model::SUPPLIER_ENABLED,
        "准入通过 → 启用"
    );

    // 拉黑 → 解除：解除只走兜底边（一票）
    let blk = draft(supplier_id, model::TYPE_BLACKLIST, "临时拉黑以验证解除路径").await;
    submit(blk).await;
    assert_eq!(approve_until_final(instance_of(blk).await).await.1, "APPROVED");
    assert_eq!(supplier_status(supplier_id).await, model::SUPPLIER_DISABLED);

    let release = draft(supplier_id, model::TYPE_RELEASE, "整改完成，申请解除黑名单恢复供货").await;
    submit(release).await;
    let (votes, final_status) = approve_until_final(instance_of(release).await).await;
    assert_eq!(final_status, "APPROVED");
    assert_eq!(votes, 1, "解除只走采购经理一个节点（兜底边）");
    assert_eq!(
        supplier_status(supplier_id).await,
        model::SUPPLIER_ENABLED,
        "解除通过 → 启用"
    );

    // 准入过之后不能再来一张准入（起点判据）
    let again = CreateAccessReq {
        supplier_id: supplier_id.to_string(),
        access_type: model::TYPE_ADMIT.to_string(),
        reason: Some("已准入过还要再准入".to_string()),
        grade_snapshot: None,
        remark: None,
    };
    let error = handler::create_application(&again, Some(operator().await))
        .await
        .expect_err("已准入过不能再准入");
    assert_eq!(error.status, 409);

    cleanup().await;
}

/// ⑥ 驳回 → 申请单打回 DRAFT（意见进备注），供应商状态不动。
#[tokio::test]
async fn reject_returns_to_draft_and_keeps_supplier() {
    let Some(_guard) = setup().await else {
        return;
    };
    let supplier_id = new_supplier("REJ", model::SUPPLIER_ENABLED).await;
    let id = draft(supplier_id, model::TYPE_BLACKLIST, "证据不足，先提一单试试驳回").await;
    submit(id).await;
    let instance_id = instance_of(id).await;
    let tasks = pending_tasks(instance_id).await;
    assert!(!tasks.is_empty(), "提交后必须有待办");
    let final_status = reject_task(tasks[0].0, tasks[0].2, "证据不充分，补充质检报告再来").await;
    assert_eq!(final_status, "REJECTED");

    let row = model::find_access(db(), id).await.expect("查单").expect("存在");
    assert_eq!(
        row.status,
        model::STATUS_DRAFT,
        "驳回必须打回草稿（不是造「已驳回」状态）"
    );
    assert!(
        row.approval_instance_id.is_none(),
        "打回后实例 id 清空（下一轮重新提交会用新实例）"
    );
    assert!(
        row.remark.as_deref().unwrap_or_default().contains("证据不充分"),
        "驳回意见必须写进备注：{:?}",
        row.remark
    );
    assert_eq!(
        supplier_status(supplier_id).await,
        model::SUPPLIER_ENABLED,
        "驳回不许动供应商状态"
    );

    // 打回后可以改理由再提交（同一张单）
    let update = UpdateAccessReq {
        reason: Some("补充质检报告后重新申请拉黑".to_string()),
        grade_snapshot: None,
        remark: None,
    };
    handler::update_application(id, &update, Some(operator().await))
        .await
        .expect("打回后应可改草稿");
    submit(id).await;
    let row = model::find_access(db(), id).await.expect("查单").expect("存在");
    assert_eq!(row.status, model::STATUS_SUBMITTED, "重新提交应回到审批中");

    cleanup().await;
}

/// ⑦ 采购订单建单闸：停用（含被拉黑）供应商 → 409 + 66099。
#[tokio::test]
async fn disabled_supplier_is_not_usable() {
    let Some(_guard) = setup().await else {
        return;
    };
    let usable = new_supplier("USE-OK", model::SUPPLIER_ENABLED).await;
    purchase_handler::ensure_supplier_usable(usable, Some("夹具供应商 USE-OK"))
        .await
        .expect("启用供应商必须放行");

    let disabled = new_supplier("USE-OFF", model::SUPPLIER_DISABLED).await;
    let error = purchase_handler::ensure_supplier_usable(disabled, Some("夹具供应商 USE-OFF"))
        .await
        .expect_err("停用供应商必须拒");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::ERP_SUPPLIER_NOT_USABLE);
    assert!(
        error.msg.contains("停用") || error.msg.contains("黑名单"),
        "报文要点明当前状态：{}",
        error.msg
    );

    // 被拉黑之后同样不可下单（状态是审批回调落的那个 0）
    let blacklisted = new_supplier("USE-BLK", model::SUPPLIER_ENABLED).await;
    let id = draft(blacklisted, model::TYPE_BLACKLIST, "拉黑后不许下单，验证建单闸").await;
    submit(id).await;
    assert_eq!(approve_until_final(instance_of(id).await).await.1, "APPROVED");
    let error = purchase_handler::ensure_supplier_usable(blacklisted, Some("夹具供应商 USE-BLK"))
        .await
        .expect_err("黑名单供应商必须拒");
    assert_eq!(error.code, error_code::ERP_SUPPLIER_NOT_USABLE);
    assert!(error.msg.contains("黑名单"), "报文要点明黑名单：{}", error.msg);

    cleanup().await;
}

/// ⑧ 内控：申请人不能批自己那一票。
#[tokio::test]
async fn self_approval_is_forbidden() {
    let Some(_guard) = setup().await else {
        return;
    };
    let supplier_id = new_supplier("SELF", model::SUPPLIER_ENABLED).await;
    let id = draft(supplier_id, model::TYPE_BLACKLIST, "自审用例：申请人批自己那一票").await;
    submit(id).await;
    let instance_id = instance_of(id).await;
    let tasks = pending_tasks(instance_id).await;
    assert!(!tasks.is_empty(), "提交后必须有待办");

    let enabled = cloudpivot_kernel::business::shared::separation::enabled(
        cloudpivot_kernel::business::shared::separation::Duty::Approve,
    )
    .await;
    let outcome = flow::guard_self_approval(tasks[0].0, operator().await).await;
    if enabled {
        let error = outcome.expect_err("开职责分离时申请人不能自审");
        assert_eq!(error.status, 403);
        assert!(
            error.msg.contains("不能审批自己提交的供应商准入"),
            "报文要说清是自审拦截：{}",
            error.msg
        );
    } else {
        outcome.expect("关职责分离时放行（配置口径，不是缺陷）");
        println!("NOTE: separation::Approve 已关闭，自审闸按配置放行 —— 本用例只验证代码路径不 panic");
    }

    cleanup().await;
}

/// ⑨ 种子自检：V0455 的 graph_snapshot / graph_hash 必须与节点·边行逐字节一致。
///
/// 谁改过种子里的节点 / 边（哪怕只是 `config`）都会让 `load_graph()` 重算的 hash 与库里分叉：
/// 运行期「流程看起来正常、实际跑的是另一张图」是最难查的一类事故，所以在用例里钉住。
#[tokio::test]
async fn seeded_flow_hash_matches_rows() {
    let Some(_guard) = setup().await else {
        return;
    };
    let rows = db()
        .find_with_params(
            "SELECT d.current_version_id AS version_id, v.graph_hash, v.node_count, v.edge_count \
               FROM biz_flow_def d JOIN biz_flow_def_version v ON v.id = d.current_version_id \
              WHERE d.flow_code = 'supplier_access' AND d.deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("查流程定义");
    let row = rows.first().expect("V0455 必须种出 supplier_access 流程");
    let version_id: Uuid = row.get_typed("version_id").expect("version id");
    let stored: String = row.get_typed("graph_hash").expect("graph hash");
    let nodes: i32 = row.get_typed("node_count").expect("node count");
    let edges: i32 = row.get_typed("edge_count").expect("edge count");
    assert_eq!(nodes, 5, "节点数不对（开始 / 采购经理 / 条件 / 财务经理 / 结束）");
    assert_eq!(edges, 5, "边数不对");

    let graph = crate::business::flow::model::load_graph(version_id)
        .await
        .expect("从行重建流程图失败");
    assert_eq!(
        graph.graph_hash(),
        stored,
        "supplier_access 的 graph_hash 与行数据重算不一致（快照写错了）"
    );

    // 条件边必须真的落在「拉黑」上（分级在流程定义里，不在代码里）
    let rows = db()
        .find_with_params(
            "SELECT condition_expr->>'field' AS field, condition_expr->>'value' AS value, is_default \
               FROM biz_flow_edge WHERE version_id = ?::uuid AND edge_key = 'e_type_blk'",
            vec![Value::from(&version_id)],
        )
        .await
        .expect("查条件边");
    let edge = rows.first().expect("拉黑条件边必须存在");
    assert_eq!(edge.get_typed::<String>("field").expect("field"), "form.access_type");
    assert_eq!(edge.get_typed::<String>("value").expect("value"), "BLACKLIST");
    assert!(!edge.get_typed::<bool>("is_default").expect("is_default"));

    cleanup().await;
}

/// ⑩ 清场：`M414-` 夹具 0 残留（含流程实例与待办）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else {
        return;
    };
    cleanup().await;
    let rows = db()
        .find_with_params(
            "SELECT (SELECT count(*) FROM biz_supplier_access WHERE supplier_code LIKE 'M414-%') AS a, \
                    (SELECT count(*) FROM biz_supplier WHERE supplier_code LIKE 'M414-%') AS s, \
                    (SELECT count(*) FROM biz_flow_instance i \
                      JOIN biz_supplier_access x ON x.id = i.biz_id \
                     WHERE i.biz_type = 'SUPPLIER_ACCESS' AND x.supplier_code LIKE 'M414-%') AS i",
            vec![],
        )
        .await
        .expect("查残留");
    let row = rows.first().expect("count 行");
    for (column, label) in [("a", "申请单"), ("s", "供应商"), ("i", "审批实例")] {
        let left: i64 = row.get_typed(column).expect("count");
        assert_eq!(left, 0, "M414- 夹具残留：{label} {left} 行");
    }
}
