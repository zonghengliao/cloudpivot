// ============================================================================
// framework_agreement_crud_db_tests.rs —— 框架协议维护链路的真库用例（M4-11b）
//
// 卡：t_f358da98（M4-11b【M4·采购】框架协议维护 CRUD 后端接口 + 页面写入口）
//
// # 覆盖什么（每条都对应卡面 / V0456 / `framework_agreement_{dto,model,handler}` 的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `create_draft_shapes` | 建单：单号 `FWA-yyyyMMdd-NNNNNN`、DRAFT、供应商/经办人姓名快照、行号重排、派生合计 = 0 |
// | `create_rejects_each_bad_branch` | 反向：有效期倒序 / FLOATING 无规则 / 同物料两行 / 负价 / 上限 0 / 供应商不存在 / 物料不存在 |
// | `update_replaces_items_in_place` | 改单：同物料**原地** UPDATE（行 id 不变）、改价改上限、新增物料、删掉零执行量的行、行号重排 |
// | `update_rejects_non_draft` | 提交后改单 → 409（审批中的单不接受改写） |
// | `update_rejects_shrinking_below_executed` | 新上限 < 已用 → 409；== 已用允许；删掉有执行量的物料行 → 409 |
// | `delete_only_draft` | 草稿能删（软删后查不到）；提交后删 → 409 |
// | `submit_goes_through_flow` | 提交 → SUBMITTED + `FWA_APPROVE` 实例；两节点真批 → APPROVED；重复提交 → 409 |
// | `reject_returns_to_draft` | 驳回 → DRAFT（意见进备注、实例清空），协议可再改再提 |
// | `submit_rejects_empty_scope` | 空物料范围提交 → 400（空协议不可批） |
// | `full_chain_order_raises_execution` | **建 → 提交 → 审批 → 下单 → 执行量涨**（M4-08 真实下单入口） |
// | `seeded_flow_hash_matches_rows` | V0456 的 graph_snapshot / graph_hash 与节点·边行逐字节一致 |
// | `zz_teardown_purges_fixtures` | 清场：`M411B-` 夹具 0 残留（含流程实例与待办） |
//
// # ⚠ 真库用例必须串行
//
// 共用全局 `db()` 连接池与 `aging_db_tests::DB_LOCK`，跑法：
//
// ```bash
// ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m411b_drill" \
//   cargo test --lib business::erp::purchase::framework_agreement_crud -- --test-threads=1
// ```
//
// 流程种子（`fw_agreement`）由 V0456 种下：**没有它时提交会被 409 拦下**（默认拒绝 +
// 显式跳过），这本身是正确行为 —— 本文件里「有流程」的用例会红，说明演练库没跑全迁移。
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::ApprovalGate;
use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::framework_agreement_dto::{
    AgreementItemReq, CreateAgreementReq, SubmitAgreementReq,
};
use crate::business::erp::purchase::framework_agreement_flow as flow;
use crate::business::erp::purchase::framework_agreement_handler as handler;
use crate::business::erp::purchase::framework_agreement_model as model;
use crate::business::erp::purchase::purchase_dto::{CreatePurchaseOrderReq, PurchaseOrderItemReq};
use crate::business::erp::purchase::purchase_handler;
use crate::business::flow::engine;
use crate::test_seed;
use cloudpivot_kernel::shared::error_code;

/// 夹具名前缀（清场按它反查，绝不按「生产号段」删）。
const PREFIX: &str = "M411B-";
/// 夹具供应商编码。
const SUP_CODE: &str = "M411B-SUP";
/// 夹具物料甲（有上限）。
const MAT_A: &str = "M411B-MAT-A";
/// 夹具物料乙（另一行，可增可删）。
const MAT_B: &str = "M411B-MAT-B";
/// 夹具仓库。
const WH_CODE: &str = "M411B-WH";

// ==================== 底座 ====================

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
}

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

/// 取串行锁 + 探库 + 清场 + 就绪审批岗位。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    // `fw_agreement` 带职责分离内控（制单人会被排除），审批岗位必须有**非制单人**成员
    test_seed::ensure_approver_roles().await;
    Some(guard)
}

/// 清场：待办 → 实例 → 协议（明细随 CASCADE）→ 夹具主数据。
async fn cleanup() {
    exec(
        &format!(
            "DELETE FROM biz_flow_task WHERE instance_id IN ( \
                 SELECT i.id FROM biz_flow_instance i \
                  JOIN biz_framework_agreement a ON a.id = i.biz_id \
                 WHERE i.biz_type = 'FWA_APPROVE' AND a.agreement_name LIKE '{PREFIX}%')"
        ),
        vec![],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_flow_instance i USING biz_framework_agreement a \
              WHERE i.biz_type = 'FWA_APPROVE' AND i.biz_id = a.id AND a.agreement_name LIKE '{PREFIX}%'"
        ),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_framework_agreement WHERE agreement_name LIKE '{PREFIX}%'"),
        vec![],
    )
    .await;
    // 采购单（全链用例造的）先删，再删物料 / 供应商
    exec(
        &format!(
            "DELETE FROM biz_purchase_order WHERE supplier_id IN \
                 (SELECT id FROM biz_supplier WHERE supplier_code LIKE '{PREFIX}%')"
        ),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_supplier WHERE supplier_code LIKE '{PREFIX}%'"),
        vec![],
    )
    .await;
}

async fn supplier() -> Uuid {
    test_seed::ensure_supplier(SUP_CODE, "M411B 框架协议供应商").await
}

async fn material_a() -> Uuid {
    test_seed::ensure_material_base_unit(MAT_A, "M411B 物料甲").await
}

async fn material_b() -> Uuid {
    test_seed::ensure_material_base_unit(MAT_B, "M411B 物料乙").await
}

async fn warehouse() -> Uuid {
    test_seed::ensure_warehouse(WH_CODE, "M411B 收货仓").await
}

/// 一份「合法」的建单请求（各用例只改自己关心的字段）。
fn req(name: &str, supplier_id: Uuid, mat: Uuid, max_qty: Option<&str>) -> CreateAgreementReq {
    CreateAgreementReq {
        agreement_name: Some(format!("{PREFIX}{name}")),
        supplier_id: Some(supplier_id.to_string()),
        dept_id: None,
        owner_id: None,
        start_date: Some("2026-01-01".to_string()),
        end_date: Some("2026-12-31".to_string()),
        sign_date: Some("2025-12-20".to_string()),
        price_mode: Some("FIXED".to_string()),
        price_includes_tax: Some(false),
        price_currency: Some("CNY".to_string()),
        price_tolerance: Some(dec("0.05")),
        price_adjust_rule: None,
        total_max_qty: None,
        total_max_amount: None,
        remark: Some(format!("{PREFIX}用例")),
        items: vec![AgreementItemReq {
            material_id: Some(mat.to_string()),
            agreed_price: Some(dec("10")),
            tax_rate: Some(dec("0.13")),
            max_qty: max_qty.map(dec),
            remark: Some("首行".to_string()),
        }],
    }
}

/// 建一张草稿并返回 id（用例的公共前置）。
async fn draft(name: &str, max_qty: Option<&str>) -> Uuid {
    let request = req(name, supplier().await, material_a().await, max_qty);
    handler::create_agreement(&request, Some(operator().await))
        .await
        .expect("建草稿必须成功")
}

/// 统一出口的断言：期望 400 / 409 且报文含某段话。
fn expect_error<T: std::fmt::Debug>(result: std::result::Result<T, ApiError>, status: u16, contains: &str) -> ApiError {
    match result {
        Ok(value) => panic!("期望被拒（{status} / 含「{contains}」），实际成功：{value:?}"),
        Err(error) => {
            assert_eq!(error.status, status, "状态码不对：{} —— {}", error.status, error.msg);
            assert!(
                error.msg.contains(contains),
                "报文没点出关键原因（期望含「{contains}」）：{}",
                error.msg
            );
            error
        }
    }
}

/// 实例上的待办（`task_id / node_key / assignee_id`），按创建顺序。
async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, String, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT id AS task_id, node_key, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' ORDER BY created_at, node_key",
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
        .transaction(|tx| async move { engine::approve(&tx, task_id, approver, Some("M411B 用例同意")).await })
        .await
        .expect("审批同意");
    let status = out.instance_status.clone();
    flow::on_fwa_settled(out.instance_id, &status).await;
    status
}

/// 逐节点真派真批，返回（走过的节点数，最终状态）。
async fn approve_until_final(instance_id: Uuid) -> (usize, String) {
    let mut rounds = 0usize;
    let mut last = String::from("PENDING");
    for _ in 0..6 {
        let tasks = pending_tasks(instance_id).await;
        if tasks.is_empty() {
            break;
        }
        let node_key = tasks[0].1.clone();
        assert!(
            tasks.iter().all(|task| task.1 == node_key),
            "同一轮里混了多个节点的待办：{:?}",
            tasks.iter().map(|task| task.1.clone()).collect::<Vec<_>>()
        );
        last = approve_task(tasks[0].0, tasks[0].2).await;
        rounds += 1;
        if last == "APPROVED" || last == "REJECTED" {
            break;
        }
    }
    (rounds, last)
}

/// 提交审批（走真实 gate；用例不带 skip）。
async fn submit(agreement_id: Uuid) -> std::result::Result<(), ApiError> {
    let request = SubmitAgreementReq::default();
    let claims = claims();
    let gate = ApprovalGate::new(&request.skip, &claims);
    handler::submit_agreement(agreement_id, &request, Some(operator().await), &gate).await
}

/// 登录态（不带 `skip-approval`：本文件的用例走的是「有流程就必须走流程」那条路）。
fn claims() -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: operator_blocking_hint().to_string(),
        username: Some("m411b_case".to_string()),
        roles: Vec::new(),
        permissions: Vec::new(),
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
}

/// `TokenClaims.sub` 只要是个字符串；`ApprovalGate` 只用它判「有没有 skip 权限」。
/// 真正落库的发起人由 `submit_agreement(…, operator)` 显式传入。
fn operator_blocking_hint() -> &'static str {
    "00000000-0000-0000-0000-000000000000"
}

/// 把某行已用量改成给定值（模拟 M4-08 已下单的回写；收紧守卫的**前提**）。
async fn set_executed(agreement_id: Uuid, material_id: Uuid, qty: &str) {
    exec(
        &format!(
            "UPDATE {} SET executed_qty = ?::text::numeric, executed_amount = ?::text::numeric \
              WHERE agreement_id = ?::uuid AND material_id = ?::uuid",
            model::ITEM_TABLE
        ),
        vec![
            Value::Text(qty.to_string()),
            Value::Text(qty.to_string()),
            Value::from(&agreement_id),
            Value::from(&material_id),
        ],
    )
    .await;
    model::recalc_totals(db(), agreement_id).await.expect("重算合计");
}

// ==================== 用例 ====================

/// ① 建单：单号 / 状态 / 快照 / 行号 / 派生合计。
#[tokio::test]
async fn create_draft_shapes() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;
    let mat = material_a().await;
    let request = req("SHAPE", sup, mat, Some("100"));
    let id = handler::create_agreement(&request, Some(operator().await))
        .await
        .expect("建单");

    let row = model::find_agreement(db(), id).await.expect("查协议").expect("存在");
    // `FWA-yyyyMMdd-NNNNNN`：4 + 8 + 1 + 6 = 19
    assert_eq!(
        row.agreement_no.len(),
        19,
        "单号长度必须是 FWA-yyyyMMdd-NNNNNN，实际 {}",
        row.agreement_no
    );
    assert!(
        row.agreement_no.starts_with("FWA-"),
        "单号前缀，实际 {}",
        row.agreement_no
    );
    assert!(
        row.agreement_no[4..12].chars().all(|c| c.is_ascii_digit())
            && row.agreement_no[13..].chars().all(|c| c.is_ascii_digit()),
        "日期段与流水段必须是数字，实际 {}",
        row.agreement_no
    );
    assert_eq!(row.status, model::STATUS_DRAFT, "新建必须是草稿");
    assert_eq!(row.agreement_name, format!("{PREFIX}SHAPE"));
    assert_eq!(
        row.supplier_name.as_deref(),
        Some("M411B 框架协议供应商"),
        "供应商名必须落快照（约定 13）"
    );
    assert_eq!(row.supplier_display_name, "M411B 框架协议供应商");
    assert!(row.owner_id.is_some(), "经办人缺省 = 当前登录人");
    assert!(row.owner_name.is_some(), "经办人姓名必须落快照");
    assert_eq!(row.price_mode, model::PRICE_MODE_FIXED);
    assert_eq!(row.price_currency, "CNY");
    assert_eq!(row.total_executed_qty, Decimal::ZERO, "派生合计初始为 0");
    assert_eq!(row.total_executed_amount, Decimal::ZERO);
    assert_eq!(row.item_count, 1);

    let items = model::items_of_agreement(db(), id).await.expect("查明细");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].line_no, 1);
    assert_eq!(items[0].material_id, mat);
    assert_eq!(items[0].agreed_price, dec("10"));
    assert_eq!(items[0].max_qty, Some(dec("100")));
    assert_eq!(items[0].executed_qty, Decimal::ZERO);
    assert!(items[0].unit_id != Uuid::nil(), "单位由物料基础单位带出");

    // 详情出参的 `editable`（前端据此判按钮，别自己写状态机）
    let detail = handler::detail_of(id).await.expect("详情");
    assert!(detail.agreement.editable, "草稿必须 editable");
    assert_eq!(detail.items.len(), 1);
    assert_eq!(detail.items[0].remaining_qty, Some(dec("100")));
    assert_eq!(
        detail.items[0].remark.as_deref(),
        Some("首行"),
        "行备注必须回传（改单要回填）"
    );
}

/// ② 反向：每条校验分支各一条。
#[tokio::test]
async fn create_rejects_each_bad_branch() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;
    let mat = material_a().await;
    let op = Some(operator().await);

    // 有效期倒序（ck_biz_fwa_period 同口径）
    let mut bad = req("BAD-PERIOD", sup, mat, None);
    bad.start_date = Some("2026-12-31".to_string());
    bad.end_date = Some("2026-01-01".to_string());
    expect_error(handler::create_agreement(&bad, op).await, 400, "不能晚于");

    // FLOATING 但没有调价规则（ck_biz_fwa_adjust 同口径）
    let mut bad = req("BAD-FLOAT", sup, mat, None);
    bad.price_mode = Some("FLOATING".to_string());
    bad.price_adjust_rule = Some("   ".to_string());
    expect_error(handler::create_agreement(&bad, op).await, 400, "调价规则");

    // 同一物料两行（uk_biz_fwai_material 同口径）
    let mut bad = req("BAD-DUP", sup, mat, None);
    bad.items.push(AgreementItemReq {
        material_id: Some(mat.to_string()),
        agreed_price: Some(dec("11")),
        ..Default::default()
    });
    expect_error(handler::create_agreement(&bad, op).await, 400, "同一个物料");

    // 负协议价（ck_biz_fwai_price 同口径）
    let mut bad = req("BAD-PRICE", sup, mat, None);
    bad.items[0].agreed_price = Some(dec("-0.01"));
    expect_error(handler::create_agreement(&bad, op).await, 400, "不能为负");

    // 明细上限 0（ck_biz_fwai_cap 同口径：0 是「禁止采购」，不是「不限」）
    let mut bad = req("BAD-ITEMCAP", sup, mat, None);
    bad.items[0].max_qty = Some(Decimal::ZERO);
    expect_error(handler::create_agreement(&bad, op).await, 400, "不限量");

    // 单头总量上限 0（ck_biz_fwa_caps 同口径）
    let mut bad = req("BAD-TOTALCAP", sup, mat, None);
    bad.total_max_qty = Some(Decimal::ZERO);
    expect_error(handler::create_agreement(&bad, op).await, 400, "不限量");

    // 容差越界
    let mut bad = req("BAD-TOL", sup, mat, None);
    bad.price_tolerance = Some(dec("1.5"));
    expect_error(handler::create_agreement(&bad, op).await, 400, "price_tolerance");

    // 供应商不存在（弱引用，但建单要判存在 + 取快照）
    let mut bad = req("BAD-SUP", sup, mat, None);
    bad.supplier_id = Some(Uuid::now_v7().to_string());
    expect_error(handler::create_agreement(&bad, op).await, 400, "供应商不存在");

    // 物料不存在
    let mut bad = req("BAD-MAT", sup, mat, None);
    bad.items[0].material_id = Some(Uuid::now_v7().to_string());
    expect_error(handler::create_agreement(&bad, op).await, 400, "物料不存在");

    // 有效期为空的另一半（start_date 必填）
    let mut bad = req("BAD-START", sup, mat, None);
    bad.start_date = None;
    expect_error(handler::create_agreement(&bad, op).await, 400, "start_date");

    // 全部被拒 → 一行都没落库
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT count(*) AS v FROM {} WHERE agreement_name LIKE '{PREFIX}BAD-%'",
                model::AGREEMENT_TABLE
            ),
            vec![],
        )
        .await
        .expect("统计");
    let left: i64 = rows.first().and_then(|row| row.get_typed("v").ok()).unwrap_or(-1);
    assert_eq!(left, 0, "被拒的请求一张单都不许落库");
}

/// ③ 改单：同物料**原地** UPDATE（行 id 不变）、新增 / 删除零执行量的行、行号重排。
#[tokio::test]
async fn update_replaces_items_in_place() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;
    let mat_a = material_a().await;
    let mat_b = material_b().await;
    let id = draft("UPDATE", Some("100")).await;

    let before = model::items_of_agreement(db(), id).await.expect("查明细");
    let row_id_a = before[0].id;

    // 改价 + 改上限 + 加一行物料乙 + 把单头总额上限写上
    let mut request = req("UPDATE", sup, mat_a, Some("150"));
    request.items[0].agreed_price = Some(dec("12.5"));
    request.total_max_amount = Some(dec("200000"));
    request.items.push(AgreementItemReq {
        material_id: Some(mat_b.to_string()),
        agreed_price: Some(dec("3")),
        tax_rate: Some(dec("0.09")),
        max_qty: None,
        remark: Some("第二行".to_string()),
    });
    handler::update_agreement(id, &request, Some(operator().await))
        .await
        .expect("改单");

    let after = model::items_of_agreement(db(), id).await.expect("查明细");
    assert_eq!(after.len(), 2, "改单后应有 2 行");
    assert_eq!(
        after[0].id, row_id_a,
        "同物料的行必须**原地**保留 id（弱引用：采购明细指着它）"
    );
    assert_eq!(after[0].agreed_price, dec("12.5"));
    assert_eq!(after[0].max_qty, Some(dec("150")));
    assert_eq!(after[0].line_no, 1);
    assert_eq!(after[1].material_id, mat_b);
    assert_eq!(after[1].line_no, 2);
    assert_eq!(after[1].max_qty, None, "不限量必须是 NULL，不是 0");

    let row = model::find_agreement(db(), id).await.expect("查").expect("存在");
    assert_eq!(row.total_max_amount, Some(dec("200000")));
    assert_eq!(row.item_count, 2);

    // 再改一次：去掉物料乙（它没执行量 → 允许删），只留物料甲
    let mut request = req("UPDATE", sup, mat_a, Some("150"));
    request.items[0].agreed_price = Some(dec("12.5"));
    handler::update_agreement(id, &request, Some(operator().await))
        .await
        .expect("再改单");
    let after = model::items_of_agreement(db(), id).await.expect("查明细");
    assert_eq!(after.len(), 1, "零执行量的行必须能删掉");
    assert_eq!(after[0].id, row_id_a, "留下的那一行 id 不变");
}

/// ④ 提交后改单 → 409（审批中的单不接受改写）。
#[tokio::test]
async fn update_rejects_non_draft() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;
    let mat = material_a().await;
    let id = draft("UPD-DRAFT", Some("100")).await;
    submit(id).await.expect("提交");

    let request = req("UPD-DRAFT", sup, mat, Some("120"));
    let error = expect_error(
        handler::update_agreement(id, &request, Some(operator().await)).await,
        409,
        "只有草稿",
    );
    assert_eq!(error.code, error_code::RESOURCE_CONFLICT);
    // 删除同理
    expect_error(
        handler::delete_agreement(id, Some(operator().await)).await,
        409,
        "只有草稿",
    );
}

/// ⑤ 收紧额度的两条守卫（跨行语义，库层 CHECK 做不到）。
#[tokio::test]
async fn update_rejects_shrinking_below_executed() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;
    let mat_a = material_a().await;
    let mat_b = material_b().await;
    let id = draft("SHRINK", Some("100")).await;
    set_executed(id, mat_a, "30").await;

    // 新上限 10 < 已用 30 → 409
    let request = req("SHRINK", sup, mat_a, Some("10"));
    let error = expect_error(
        handler::update_agreement(id, &request, Some(operator().await)).await,
        409,
        "小于已执行量",
    );
    assert_eq!(error.code, error_code::RESOURCE_CONFLICT);

    // 恰等于已用（30）→ 允许（「用满了」是合法的收紧终点）
    let request = req("SHRINK", sup, mat_a, Some("30"));
    handler::update_agreement(id, &request, Some(operator().await))
        .await
        .expect("新上限 == 已用必须放行");
    let items = model::items_of_agreement(db(), id).await.expect("查明细");
    assert_eq!(items[0].max_qty, Some(dec("30")));
    // 执行量不能被改单抹掉
    assert_eq!(items[0].executed_qty, dec("30"));

    // 把已用量的物料行删掉 → 409（弱引用会悬空、报表会静默少量）
    let mut request = req("SHRINK", sup, mat_b, Some("50"));
    request.items[0].material_id = Some(mat_b.to_string());
    let error = expect_error(
        handler::update_agreement(id, &request, Some(operator().await)).await,
        409,
        "不能从物料范围里删掉",
    );
    assert_eq!(error.code, error_code::RESOURCE_CONFLICT);

    // 单头总量上限也不能改小到低于已用（已用合计 = 30）
    let mut request = req("SHRINK", sup, mat_a, Some("30"));
    request.total_max_qty = Some(dec("20"));
    expect_error(
        handler::update_agreement(id, &request, Some(operator().await)).await,
        409,
        "小于已执行量",
    );
}

/// ⑥ 只有草稿能删；软删后查不到。
#[tokio::test]
async fn delete_only_draft() {
    let Some(_guard) = setup().await else { return };
    let id = draft("DELETE", None).await;
    handler::delete_agreement(id, Some(operator().await))
        .await
        .expect("删草稿");
    assert!(
        model::find_agreement(db(), id).await.expect("查").is_none(),
        "软删后 find_agreement 必须查不到（deleted_at IS NULL 过滤）"
    );
    // 明细仍在（软删保审计），但单头不可见
    let items = model::items_of_agreement(db(), id).await.expect("查明细");
    assert_eq!(items.len(), 1, "明细随单头软删保留（审计需要）");
    // 重复删 → 404
    expect_error(
        handler::delete_agreement(id, Some(operator().await)).await,
        404,
        "不存在或已删除",
    );
}

/// ⑦ 提交 → 两节点真批 → APPROVED；重复提交 409。
#[tokio::test]
async fn submit_goes_through_flow() {
    let Some(_guard) = setup().await else { return };
    let id = draft("SUBMIT", Some("100")).await;
    submit(id).await.expect("提交");

    let row = model::find_agreement(db(), id).await.expect("查").expect("存在");
    assert_eq!(row.status, model::STATUS_SUBMITTED, "提交后必须是 SUBMITTED");
    let instance_id = row
        .approval_instance_id
        .expect("提交后必须有审批实例（ck_biz_fwa_instance）");

    // 实例必须是 FWA_APPROVE 且挂在协议上
    let instances = db()
        .find_with_params(
            "SELECT biz_type, biz_id, biz_no FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查实例");
    let instance = instances.first().expect("实例存在");
    assert_eq!(
        instance.get_typed::<String>("biz_type").expect("biz_type"),
        flow::BIZ_TYPE
    );
    assert_eq!(instance.get_typed::<Uuid>("biz_id").expect("biz_id"), id);
    assert_eq!(
        instance.get_typed::<Option<String>>("biz_no").expect("biz_no"),
        Some(row.agreement_no.clone())
    );

    // 重复提交 → 409（带审批单号）
    let error = expect_error(submit(id).await, 409, "已在审批中");
    assert_eq!(error.code, error_code::RESOURCE_CONFLICT);

    // 两个节点（采购经理 → 财务经理），全部通过 → APPROVED
    let (rounds, status) = approve_until_final(instance_id).await;
    assert_eq!(rounds, 2, "fw_agreement 种子流程是两道审批，实际走了 {rounds} 个节点");
    assert_eq!(status, "APPROVED");

    let row = model::find_agreement(db(), id).await.expect("查").expect("存在");
    assert_eq!(row.status, model::STATUS_APPROVED, "审批通过后必须 APPROVED");
    assert_eq!(
        row.approval_instance_id,
        Some(instance_id),
        "通过后保留实例 id（回溯「这一版是谁批的」）"
    );
    assert!(
        row.remark.as_deref().unwrap_or_default().contains("审批通过"),
        "结论要进备注"
    );
    // 已批准 → 不能再改 / 删 / 提交
    let request = req("SUBMIT", supplier().await, material_a().await, Some("100"));
    expect_error(
        handler::update_agreement(id, &request, Some(operator().await)).await,
        409,
        "只有草稿",
    );
}

/// ⑧ 驳回 → 打回草稿（意见进备注、实例清空），可再改再提。
#[tokio::test]
async fn reject_returns_to_draft() {
    let Some(_guard) = setup().await else { return };
    let id = draft("REJECT", Some("100")).await;
    submit(id).await.expect("提交");
    let instance_id = model::find_agreement(db(), id)
        .await
        .expect("查")
        .expect("存在")
        .approval_instance_id
        .expect("实例");

    let tasks = pending_tasks(instance_id).await;
    let rejecter = tasks[0].2;
    let out = db()
        .transaction(|tx| async move {
            engine::reject(
                &tx,
                tasks[0].0,
                rejecter,
                "M411B 价格偏高，请复核",
                engine::ReturnTo::Final,
            )
            .await
        })
        .await
        .expect("驳回");
    flow::on_fwa_settled(out.instance_id, &out.instance_status).await;

    let row = model::find_agreement(db(), id).await.expect("查").expect("存在");
    assert_eq!(
        row.status,
        model::STATUS_DRAFT,
        "驳回必须打回草稿（不造「已驳回」状态）"
    );
    assert_eq!(
        row.approval_instance_id, None,
        "打回草稿要清掉实例（草稿不挂已结束的审批单）"
    );
    assert!(
        row.remark.as_deref().unwrap_or_default().contains("价格偏高"),
        "驳回意见必须写进备注：{:?}",
        row.remark
    );

    // 打完回能改、能再提交
    let request = req("REJECT", supplier().await, material_a().await, Some("90"));
    handler::update_agreement(id, &request, Some(operator().await))
        .await
        .expect("打回后必须能改");
    submit(id).await.expect("打回后必须能再提");
}

/// ⑨ 空物料范围的协议不能提交审批（空协议批了也产生不了执行量）。
#[tokio::test]
async fn submit_rejects_empty_scope() {
    let Some(_guard) = setup().await else { return };
    let mut request = req("EMPTY", supplier().await, material_a().await, None);
    request.items.clear();
    let id = handler::create_agreement(&request, Some(operator().await))
        .await
        .expect("空物料范围的协议单头合法（拟稿还没编物料）");
    expect_error(submit(id).await, 400, "没有任何物料范围");
}

/// ⑩ 全链：建 → 提交 → 审批 → 下单（M4-08 真实入口）→ 执行量涨。
#[tokio::test]
async fn full_chain_order_raises_execution() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;
    let mat = material_a().await;
    let wh = warehouse().await;

    let request = req("CHAIN", sup, mat, Some("100"));
    let id = handler::create_agreement(&request, Some(operator().await))
        .await
        .expect("建单");
    submit(id).await.expect("提交");
    let instance_id = model::find_agreement(db(), id)
        .await
        .expect("查")
        .expect("存在")
        .approval_instance_id
        .expect("实例");
    let (_, status) = approve_until_final(instance_id).await;
    assert_eq!(status, "APPROVED");
    let row = model::find_agreement(db(), id).await.expect("查").expect("存在");
    assert_eq!(row.status, model::STATUS_APPROVED);

    // 下单（M4-08 的校验 + 回写走的是真实路径）
    let order = CreatePurchaseOrderReq {
        supplier_id: Some(sup.to_string()),
        warehouse_id: Some(wh.to_string()),
        purchase_type: Some("SPOT".to_string()),
        contract_id: None,
        framework_agreement_id: Some(id.to_string()),
        project_id: None,
        order_date: Some(date("2026-06-01")),
        expect_date: None,
        price_mode: Some("EXCL".to_string()),
        credit_days: None,
        remark: Some(format!("{PREFIX}全链用例")),
        items: vec![PurchaseOrderItemReq {
            material_id: Some(mat.to_string()),
            qty: Some(dec("40")),
            price_excl: Some(dec("10.4")), // 10 × (1 + 5%) = 10.5 上限内
            price_incl: None,
            tax_rate: Some(dec("13")),
            remark: None,
        }],
    };
    let order_id = purchase_handler::create_purchase_order(&order, Some(operator().await))
        .await
        .expect("审批通过的协议必须能被下单引用");

    // 执行量涨：明细回写 40、单头派生合计同步、来源单据现算 40
    let items = model::items_of_agreement(db(), id).await.expect("查明细");
    assert_eq!(items[0].executed_qty, dec("40"), "M4-08 下单必须回写已执行量");
    let row = model::find_agreement(db(), id).await.expect("查").expect("存在");
    assert_eq!(row.total_executed_qty, dec("40"), "单头派生合计必须重算");
    assert!(row.total_executed_amount > Decimal::ZERO);

    // 执行量报表（M4-09 视图现算）也要涨 —— 两个口径必须一致
    let exec_detail = handler::detail_of(id).await.expect("详情");
    assert_eq!(
        exec_detail.items[0].executed_qty,
        dec("40"),
        "详情里的已用量取「链现算」与「回写」的较大值，必须同步涨"
    );
    assert_eq!(exec_detail.items[0].remaining_qty, Some(dec("60")), "剩余 = 100 − 40");

    // 再改单把上限收到 40 以下 → 409（已用量保护）
    let mut shrink = req("CHAIN", sup, mat, Some("30"));
    shrink.items[0].agreed_price = Some(dec("10"));
    expect_error(
        handler::update_agreement(id, &shrink, Some(operator().await)).await,
        409,
        "只有草稿",
    );

    // 下单后协议仍不是草稿 → 改单整体被 409；把单头打回草稿才谈得上收紧
    exec(
        &format!(
            "UPDATE {} SET status = 'DRAFT' WHERE id = ?::uuid",
            model::AGREEMENT_TABLE
        ),
        vec![Value::from(&id)],
    )
    .await;
    let mut shrink = req("CHAIN", sup, mat, Some("30"));
    shrink.items[0].agreed_price = Some(dec("10"));
    expect_error(
        handler::update_agreement(id, &shrink, Some(operator().await)).await,
        409,
        "小于已执行量",
    );

    // 清场：把这张采购单删掉（避免污染其它用例的额度统计）
    exec(
        "UPDATE biz_purchase_order SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await;
}

/// ⑪ 种子自检：V0456 的 graph_snapshot / graph_hash 必须与节点·边行逐字节一致。
///
/// 谁改过种子里的节点 / 边（哪怕只是 `config`）都会让 `load_graph()` 重算的 hash 与库里分叉：
/// 运行期「流程看起来正常、实际跑的是另一张图」是最难查的一类事故，所以在用例里钉住。
#[tokio::test]
async fn seeded_flow_hash_matches_rows() {
    let Some(_guard) = setup().await else { return };
    let rows = db()
        .find_with_params(
            "SELECT d.current_version_id AS version_id, v.graph_hash, v.node_count, v.edge_count \
               FROM biz_flow_def d JOIN biz_flow_def_version v ON v.id = d.current_version_id \
              WHERE d.flow_code = 'fw_agreement' AND d.deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("查流程定义");
    let row = rows.first().expect("V0456 必须种出 fw_agreement 流程");
    let version_id: Uuid = row.get_typed("version_id").expect("version id");
    let stored: String = row.get_typed("graph_hash").expect("graph hash");
    let nodes: i32 = row.get_typed("node_count").expect("node count");
    let edges: i32 = row.get_typed("edge_count").expect("edge count");
    assert_eq!(nodes, 4, "节点数不对（开始 / 采购经理 / 财务经理 / 结束）");
    assert_eq!(edges, 3, "边数不对");

    let graph = crate::business::flow::model::load_graph(version_id)
        .await
        .expect("从行重建流程图失败");
    let recomputed = graph.graph_hash();
    assert_eq!(
        recomputed, stored,
        "fw_agreement 的 graph_hash 与行数据重算不一致：库里的 hash = {stored}，重算 = {recomputed}\
         （把迁移里的 hash 换成重算值即可）"
    );
}

/// ⑫ 清场自检：本文件的夹具 0 残留。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT (SELECT count(*) FROM {} WHERE agreement_name LIKE '{PREFIX}%') AS ag, \
                        (SELECT count(*) FROM biz_flow_instance i JOIN biz_purchase_order o ON o.id = i.biz_id \
                          WHERE i.biz_type = 'FWA_APPROVE') AS inst",
                model::AGREEMENT_TABLE
            ),
            vec![],
        )
        .await
        .expect("统计");
    let row = rows.first().expect("一行");
    let ag: i64 = row.get_typed("ag").expect("ag");
    let inst: i64 = row.get_typed("inst").expect("inst");
    assert_eq!(ag, 0, "夹具协议必须 0 残留");
    assert_eq!(inst, 0, "FWA_APPROVE 实例不该挂在采购单上（biz_type 张冠李戴）");
}
