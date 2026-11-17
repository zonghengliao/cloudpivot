// ============================================================================
// purchase_plan_state_tests.rs —— 采购计划「状态机 × 报错出口 × 审批表单快照 × 执行率」单测（M4-05）
//
// 卡：t_2bfb7729（M4-05【M4·采购】采购计划：单测 / 含执行率）
// 上游：M4-01 表（V0446）/ M4-02 CRUD + 审批（V0447）/ M4-03 转申请（V0448）/ M4-04 执行率（V0449）
//
// # 这一份与另外几份用例的分工（不重测已经在别处钉过的口径）
//
// ```text
// purchase_plan_dto::tests             ← 入参校验（名称 / 类型 / 年度 / 月份 / 数量 / 单价 / 物料×仓库唯一 / 分页）
// purchase_plan_model::tests           ← 取号与周期键、状态闭集、uuid 文本化
// purchase_plan_convert_dto::tests     ← 转申请纯函数（剩余量 / 部分转 / 一仓一单 / 数值闸）
// purchase_plan_execution_dto::tests   ← 执行率纯函数（率 / 百分比 / 四态 / 来源判定 / 汇总）
// purchase_plan_execution_handler::tests ← 执行率详情 DTO（单头 / 明细 / 超计划标记）
// *_db_tests.rs（四份）                ← 真库用例：表结构、CRUD + 审批、转单、执行率视图
// 本文件（M4-05）                      ← **handler 这一层**：三道状态守卫的报文与错误码、
//                                        四个错误出口、审批表单快照（条件边刀口）、
//                                        行 → DTO 映射、**执行率与状态的联动矩阵**
// ```
//
// 也就是本文件钉的是「只有走 `ensure_draft` / `status_drift` / `form_data` / `to_row_res` /
// `to_item_res` 才会执行」的那一层 —— 这些函数在 M4-02 / M4-03 / M4-04 都是**零直接覆盖**
// （CRUD 的 DB 用例走接口，红了只看到「409」看不到「报文有没有下一步」）。
//
// # 为什么要单独钉「表单快照的金额是数字不是字符串」
//
// `pp_approve` 的分级在流程定义的**条件边**里（V0447）：`form.total_planned_amount < 500000`
// 走「不加签」，兜底边（≥ 500000）走财务经理复核。引擎比较走 `as_f64()` —— 快照里
// 写成 `"500000.0000"`（字符串）会 TypeMismatch，引擎按「条件不成立」静默走兜底边，
// 也就是**小额计划悄悄多一道财务复核**；反过来若把键名改成 `amount`，条件边永远读不到值，
// 变成**大额计划悄悄跳过财务经理**。两处都在本文件用断言钉住：
//
//   * `form_amount_threshold_is_lt_500000_matching_pp_approve_edge`（键名与阈值与 V0447 逐字对拍）
//   * `form_totals_are_json_numbers_so_condition_edges_can_compare`（是数字，`as_f64` 可用）
//
// # 执行率为什么要「矩阵 + 舍入边界」两组
//
// 率的**判据**（状态）用未舍入的原始量、率的**展示**舍到 6 位 —— 两者在刀口附近会分叉
// （`999999.9999996 / 1000000` 显示 1.000000 却是 PARTIAL）。如果拿百分比反推状态（前端最容易
// 犯的错），这一行会被显示成「已下满」。`display_rounding_does_not_flip_the_state` 就是钉这个。
//
// # 跑法（纯逻辑，不需要库）
//
// ```bash
// cargo test --lib business::erp::purchase::purchase_plan_state_tests -- --test-threads=1
// ```
//
// 也可直接跑 `bash scripts/verify/m405_purchase_plan_unit_drill.sh`（再连跑本模块四份真库用例）。
// ============================================================================

use super::{
    LABEL, PERM_CREATE, PERM_DELETE, PERM_LIST, PERM_SUBMIT, PERM_TO_REQUISITION, PERM_UPDATE, bad_request, conflict,
    ensure_draft, form_data, json_number, not_found, reference_missing, status_drift, submit_conflict, to_item_res,
    to_row_res, write_error,
};
use crate::business::erp::purchase::purchase_plan_convert_dto as convert_dto;
use crate::business::erp::purchase::purchase_plan_dto as dto;
use crate::business::erp::purchase::purchase_plan_execution_dto as exec_dto;
use crate::business::erp::purchase::purchase_plan_model as model;
use cloudpivot_kernel::shared::error_code;
use nik::prelude::chrono::{DateTime, TimeZone, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

// ==================== 夹具（纯数据，不连库） ====================

/// 定点字面量（`2.5` 这类在 f64 下不精确，一律走字符串）。
fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

/// 固定 uuid（按下标造，用例里能肉眼对上）。
fn uid(seq: u8) -> Uuid {
    Uuid::parse_str(&format!("d4050000-0000-4000-8000-0000000000{seq:02x}")).expect("uuid 字面量")
}

/// 固定时间（用例不依赖「今天」，报文 / DTO 断言才稳定）。
fn at(second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 18, 1, 2, second)
        .single()
        .expect("合法时间")
}

/// 一张计划单头（默认：年度、公司级、带部门与编制人、2 行明细、金额 450）。
fn plan_row(status: &str) -> model::PlanRow {
    model::PlanRow {
        id: uid(1),
        plan_no: "PPL-20261118-000001".to_string(),
        plan_name: "2026 年度采购计划".to_string(),
        plan_type: dto::PLAN_TYPE_ANNUAL.to_string(),
        plan_year: 2026,
        plan_month: None,
        parent_plan_id: None,
        parent_plan_no: None,
        dept_id: Some(uid(2)),
        dept_name: Some("采购部".to_string()),
        planner_id: Some(uid(3)),
        planner_name: Some("张三".to_string()),
        status: status.to_string(),
        approval_instance_id: None,
        total_planned_qty: dec("14"),
        total_executed_qty: Decimal::ZERO,
        total_planned_amount: dec("450"),
        remark: Some("年度滚动".to_string()),
        created_at: at(0),
        create_by: Some(uid(3)),
        item_count: 2,
    }
}

/// 一行计划明细（金额按 `qty × price` 4 位舍入，与 `dto::line_amount` 同口径）。
fn item_row(line_no: i32, qty: &str, price: &str) -> model::PlanItemRow {
    let qty = dec(qty);
    let price = dec(price);
    model::PlanItemRow {
        id: uid(8 + u8::try_from(line_no).unwrap_or(0)),
        plan_id: uid(1),
        line_no,
        material_id: uid(10 + u8::try_from(line_no).unwrap_or(0)),
        material_code: format!("M-{line_no:03}"),
        material_name: format!("物料 {line_no}"),
        warehouse_id: uid(20 + u8::try_from(line_no).unwrap_or(0)),
        warehouse_name: format!("仓库 {line_no}"),
        unit_id: uid(30),
        unit_name: "个".to_string(),
        planned_qty: qty,
        executed_qty: Decimal::ZERO,
        planned_price: price,
        planned_amount: dto::line_amount(qty, price),
        remark: Some(format!("第 {line_no} 行")),
    }
}

/// 执行率汇总的输入事实（只放四个量，与 `exec_dto::ExecFacts` 一致）。
fn facts(planned: &str, booked: &str, req: &str, ordered: &str) -> exec_dto::ExecFacts {
    exec_dto::ExecFacts {
        planned_qty: dec(planned),
        booked_qty: dec(booked),
        req_qty: dec(req),
        ordered_qty: dec(ordered),
    }
}

/// 一段 SQL 的「单空格归一化」文本（迁移里的对齐空格不是契约，判据是词）。
fn squeeze(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

// `pp_approve` 的流程定义（条件边在这里：`form.total_planned_amount < 500000`）。
const PP_FLOW_SQL: &str = include_str!("../../../../migrations/business/V0447__purchase_plan_crud.up.sql");
// 执行率的两个视图（状态判据与率的口径在这里）。
const PP_EXEC_SQL: &str = include_str!("../../../../migrations/business/V0449__purchase_plan_execution.up.sql");

// ==================== A. 状态守卫（改 / 删 / 提交三处共用） ====================

/// ① 草稿：三道守卫全部放行（改 / 删 / 提交 / 转申请）。
#[test]
fn draft_passes_every_state_guard() {
    let row = plan_row(model::STATUS_DRAFT);
    for action in ["修改", "删除", "提交审批", "转采购申请"] {
        assert!(
            ensure_draft(&row, action).is_ok(),
            "草稿必须能{action}（守卫只在非草稿时拦）"
        );
    }
}

/// ② 非草稿 × 四个动作：一律 409 + 66049，且报文要「点名单号 / 状态 / 动作 / 下一步」。
///
/// 判据就是用户能不能自己解套：只说「状态不允许」等于要工程师改库。
#[test]
fn every_non_draft_status_is_rejected_with_actionable_text() {
    // 三个库内状态 + 两个库外值（库外值证明「未知状态也走同一套报文」而不是 panic / 500）
    for status in [model::STATUS_SUBMITTED, model::STATUS_APPROVED, "CLOSED", "REJECTED"] {
        let row = plan_row(status);
        for action in ["修改", "删除", "提交审批", "转采购申请"] {
            let error = ensure_draft(&row, action).expect_err(&format!("{status} 不允许{action}"));
            assert_eq!(error.status, 409, "{status}/{action}：状态守卫是 409（不是 500）");
            assert_eq!(
                error.code,
                error_code::ERP_REQUISITION_STATUS_GUARD,
                "{status}/{action}：错误码必须是 66049（前端/脚本按码判）"
            );
            assert!(
                error.msg.contains(&row.plan_no),
                "{status}/{action}：报文必须点名单号，实际「{}」",
                error.msg
            );
            assert!(
                error.msg.contains(status),
                "{status}/{action}：报文必须带原始状态值，实际「{}」",
                error.msg
            );
            assert!(
                error.msg.contains(dto::status_label(status)),
                "{status}/{action}：报文必须带中文状态名，实际「{}」",
                error.msg
            );
            assert!(
                error.msg.contains(action),
                "{status}/{action}：报文必须说明是哪个动作被拒，实际「{}」",
                error.msg
            );
            assert!(
                error.msg.contains("新建一张"),
                "{status}/{action}：必须给下一步（已批准的计划要调整请新建一张），实际「{}」",
                error.msg
            );
            assert!(
                !error.msg.contains("500") && !error.msg.contains("系统错误"),
                "{status}/{action}：守卫不是系统错误"
            );
        }
    }
}

/// ③ 并发漂移（CAS 撞车）与状态入口守卫**同一个错误码**，但文案要求刷新而不是新建。
#[test]
fn status_drift_is_409_asks_refresh_and_shares_the_guard_code() {
    let row = plan_row(model::STATUS_SUBMITTED);
    for action in ["修改", "删除", "提交审批"] {
        let error = status_drift(&row, action);
        assert_eq!(error.status, 409);
        assert_eq!(
            error.code,
            error_code::ERP_REQUISITION_STATUS_GUARD,
            "并发漂移与状态入口守卫共用一个码（客户端一套处理）"
        );
        assert!(error.msg.contains(&row.plan_no), "报文带单号");
        assert!(error.msg.contains(action), "报文说明是哪个动作期间被改动");
        assert!(error.msg.contains("已被改动"), "{}", error.msg);
        assert!(error.msg.contains("刷新后重试"), "必须说清怎么恢复：{}", error.msg);
    }
    // 与 ensure_draft 的恢复动作不同：漂移是「刷新重试」，不是「新建一张」
    assert!(!status_drift(&row, "修改").msg.contains("新建一张"));
}

/// ④ 三个库内状态都有中文名，且守卫报文里出现的就是这个名字（不是未翻译的码）。
#[test]
fn guard_message_labels_every_known_status() {
    let expected = [
        (model::STATUS_DRAFT, "草稿"),
        (model::STATUS_SUBMITTED, "审批中"),
        (model::STATUS_APPROVED, "已批准"),
    ];
    for (status, label) in expected {
        assert_eq!(dto::status_label(status), label);
        if status == model::STATUS_DRAFT {
            continue;
        }
        let error = ensure_draft(&plan_row(status), "修改").expect_err("非草稿必拒");
        assert!(error.msg.contains(label), "{status} 的中文名必须出现在报文里");
    }
    // 未知名原样返回（不 panic、不空白）—— 库外状态也要能看见「到底是什么」
    assert_eq!(dto::status_label("WEIRD"), "WEIRD");
}

// ==================== B. 错误出口（三个名字 + 两个专用 409） ====================

/// ⑤ 四个错误出口的状态码与业务码各就各位，报文带可定位信息。
#[test]
fn error_exits_use_the_documented_codes() {
    let error = bad_request("plan_year 不能为空");
    assert_eq!((error.status, error.code), (400, error_code::PARAM_INVALID));
    assert_eq!(error.msg, "plan_year 不能为空");

    let error = not_found(uid(1));
    assert_eq!((error.status, error.code), (404, error_code::RESOURCE_NOT_FOUND));
    assert!(error.msg.contains("采购计划不存在或已删除"), "{}", error.msg);
    assert!(error.msg.contains(&uid(1).to_string()), "404 报文要带 id");

    let error = reference_missing("dept_id 部门不存在或已删除");
    assert_eq!((error.status, error.code), (400, error_code::ERP_REFERENCE_MISSING));
    assert_eq!(error.msg, "dept_id 部门不存在或已删除");

    let error = conflict(error_code::RESOURCE_CONFLICT, "同一编制范围已存在年度计划");
    assert_eq!((error.status, error.code), (409, error_code::RESOURCE_CONFLICT));
    assert_eq!(LABEL, "采购计划", "报文里的单据名只此一份");
}

/// ⑥ 写库失败：500 + 10004，且保留上下文与底层原因（排障要能看见是哪一步）。
#[test]
fn write_error_is_500_internal_with_context_and_cause() {
    let error = write_error(DbError::InvalidValue("模拟写库失败".to_string()), "修改采购计划");
    assert_eq!((error.status, error.code), (500, error_code::INTERNAL_ERROR));
    assert!(
        error.msg.contains("修改采购计划失败"),
        "报文要点出失败的那一步：{}",
        error.msg
    );
    assert!(error.msg.contains("模拟写库失败"), "底层原因不许吞掉：{}", error.msg);
}

/// ⑦ 并发发起审批撞 `uk_biz_flow_instance_biz_active`：翻 409 + 单号；其它写错误仍是 500。
///
/// 这一条是「500 与 409 别混」的刀口：撞唯一索引说明用户重复点了提交，
/// 给他 500 会让他以为是系统坏了，实际上刷新看进度即可。
#[test]
fn submit_conflict_maps_active_index_to_409_and_other_errors_to_500() {
    let plan_no = "PPL-20261118-000001";
    let duplicated = DbError::UniqueViolation {
        column: None,
        constraint: Some("uk_biz_flow_instance_biz_active".to_string()),
        source: None,
    };
    let error = submit_conflict(duplicated, "发起采购计划审批", plan_no);
    assert_eq!(error.status, 409, "并发提交是 409");
    assert_eq!(error.code, error_code::RESOURCE_CONFLICT);
    assert!(error.msg.contains(plan_no), "409 报文必须带单号：{}", error.msg);
    assert!(error.msg.contains("已有审批中实例"), "{}", error.msg);
    assert!(error.msg.contains("撤回"), "必须给下一步：{}", error.msg);

    let other = DbError::CheckViolation {
        constraint: Some("ck_biz_pp_status".to_string()),
        source: None,
    };
    let error = submit_conflict(other, "发起采购计划审批", plan_no);
    assert_eq!(
        (error.status, error.code),
        (500, error_code::INTERNAL_ERROR),
        "非并发唯一索引冲突不许伪装成 409"
    );
    assert!(error.msg.contains("发起采购计划审批失败"), "{}", error.msg);
}

// ==================== C. 审批表单快照（条件边的作用域） ====================

/// ⑧ 单头两个金额 / 数量键必须是 JSON **数字**（引擎 `as_f64()` 只认数字）；
/// 明细金额是字符串快照（只做展示，条件边不读）。
#[test]
fn form_totals_are_json_numbers_so_condition_edges_can_compare() {
    let mut row = plan_row(model::STATUS_DRAFT);
    row.total_planned_qty = dec("14.0000");
    row.total_planned_amount = dec("500000.0000");
    let items = vec![item_row(1, "10", "2.5"), item_row(2, "4", "100")];
    let form = form_data(&row, &items);

    let amount = form.get("total_planned_amount").expect("金额键必须存在");
    assert!(amount.is_number(), "金额必须是数字，字符串会让条件边 TypeMismatch");
    assert_eq!(amount.as_f64(), Some(500000.0));

    let qty = form.get("total_planned_qty").expect("数量键必须存在");
    assert!(qty.is_number(), "数量同样按数字给（条件边可扩展按量分级）");
    assert_eq!(qty.as_f64(), Some(14.0));

    // 明细：金额 / 单价 / 数量给十进制字符串（展示用，参与审批意见阅读，不参与比较）
    let line = &form["items"][0];
    assert_eq!(line["line_no"], 1);
    assert!(line["planned_amount"].is_string(), "明细金额是展示快照");
    let shown: Decimal = line["planned_amount"]
        .as_str()
        .expect("字符串")
        .parse()
        .expect("可解析");
    assert_eq!(shown, dec("25"), "10 × 2.5 = 25（与 dto::line_amount 同口径）");
    assert_eq!(line["warehouse_name"], "仓库 1");
    assert_eq!(line["unit_name"], "个");
}

/// ⑨ 条件边刀口：V0447 是 `form.total_planned_amount < 500000`（`cmp=lt`），
/// 快照的键名与阈值必须和流程定义逐字一致 —— 否则分级静默失效。
#[test]
fn form_amount_threshold_is_lt_500000_matching_pp_approve_edge() {
    // 流程定义里条件边读的字段 = 快照里给的键（数字）
    assert!(
        PP_FLOW_SQL.contains("\"field\":\"form.total_planned_amount\""),
        "V0447 的条件边字段改了？这里必须同步改快照键名"
    );
    assert!(PP_FLOW_SQL.contains("\"cmp\":\"lt\""), "V0447 的比较符必须是 lt");
    assert!(PP_FLOW_SQL.contains("\"value\":500000"), "V0447 的阈值必须是 500000");

    let mut row = plan_row(model::STATUS_DRAFT);
    // 刀口三连：差 1 分 / 恰好等于 / 超 1 分（等于走兜底边 → 财务复核）
    for (amount, goes_to_finance) in [
        ("499999.99", false),
        ("499999.9999", false),
        ("500000", true),
        ("500000.0000", true),
        ("500000.0001", true),
    ] {
        row.total_planned_amount = dec(amount);
        let value = form_data(&row, &[])
            .get("total_planned_amount")
            .and_then(serde_json::Value::as_f64)
            .expect("数字");
        assert_eq!(
            value < 500_000.0,
            !goes_to_finance,
            "金额 {amount}：`< 500000` 判为 {}",
            !goes_to_finance
        );
    }
}

/// ⑩ 快照先按 4 位小数舍入再参与分级：`499999.99999` 会变成 `500000` → **走财务复核**。
///
/// 这是刻意口径（金额落库本就 4 位），但也说明「差 0.00001 也加签」是确定的，
/// 不是浮点抖动；改舍入位数必须同时改这条用例。
#[test]
fn form_totals_round_to_four_dp_before_threshold() {
    let mut row = plan_row(model::STATUS_DRAFT);
    row.total_planned_amount = dec("499999.99999");
    let value = form_data(&row, &[])["total_planned_amount"].as_f64().expect("数字");
    assert_eq!(value, 500_000.0, "4 位舍入后正好等于阈值");
    assert!(value >= 500_000.0, "等于阈值 → 财务经理复核（兜底边）");

    row.total_planned_amount = dec("499999.99994");
    let value = form_data(&row, &[])["total_planned_amount"].as_f64().expect("数字");
    assert_eq!(value, 499_999.999_9, "4 位舍入后仍是 499999.9999");
    assert!(value < 500_000.0, "低于阈值 → 不加签");

    // 数量同样 4 位；0.00004 舍成 0 不是「0.00004 的计划量」
    row.total_planned_qty = dec("0.00004");
    assert_eq!(form_data(&row, &[])["total_planned_qty"].as_f64(), Some(0.0));
}

/// ⑪ 部门 / 编制人缺失时给空串（不是 null）：审批页模板直接拼字符串，null 会打出「undefined」。
#[test]
fn form_defaults_empty_strings_for_missing_names() {
    let mut row = plan_row(model::STATUS_DRAFT);
    row.dept_name = None;
    row.planner_name = None;
    let form = form_data(&row, &[]);
    assert_eq!(form["dept_name"], "", "缺省是空串");
    assert_eq!(form["planner_name"], "", "缺省是空串");
    assert!(form["dept_name"].is_string() && form["planner_name"].is_string());

    // 月度计划：plan_month 是数字，年度计划是 null（前端按 null 判断「年度」）
    row.plan_month = Some(11);
    row.plan_type = dto::PLAN_TYPE_MONTHLY.to_string();
    let form = form_data(&row, &[]);
    assert_eq!(form["plan_month"], 11);
    assert_eq!(form["plan_type"], "MONTHLY", "快照给原始码，条件边按码比较");
    assert_eq!(form["plan_year"], 2026);
}

/// ⑫ 明细逐行进快照且**保持行序**；`item_count` 取自单头（派生列以服务端重算为准）。
#[test]
fn form_lists_all_items_in_line_order() {
    let row = plan_row(model::STATUS_DRAFT);
    let items = vec![item_row(1, "10", "2.5"), item_row(2, "4", "100")];
    let form = form_data(&row, &items);
    let lines = form["items"].as_array().expect("数组");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["line_no"], 1);
    assert_eq!(lines[1]["line_no"], 2);
    assert_eq!(lines[0]["material_code"], "M-001");
    assert_eq!(lines[1]["material_name"], "物料 2");

    // 单头 item_count 与明细行数被刻意分开取：改单一半失败时两者会不一致，
    // 快照必须忠实反映「读到的单头」（否则审批人看到的行数是幻觉）。
    let mut drift = plan_row(model::STATUS_DRAFT);
    drift.item_count = 9;
    assert_eq!(form_data(&drift, &items)["item_count"], 9);
}

/// ⑬ 十进制 → JSON 数字：正常值可往返；空 / 垃圾 / NaN / inf 一律 null（不许 panic、不许串型）。
#[test]
fn json_number_parses_and_degrades_to_null() {
    assert_eq!(json_number("500000.0000").as_f64(), Some(500_000.0));
    assert_eq!(json_number("-12.5").as_f64(), Some(-12.5));
    assert_eq!(json_number("0").as_f64(), Some(0.0));
    assert_eq!(json_number("0.000001").as_f64(), Some(0.000001));
    for broken in ["", "  ", "abc", "NaN", "inf", "-inf", "1e999", "--1"] {
        assert!(
            json_number(broken).is_null(),
            "「{broken}」必须降级成 null（条件边见到 null 走兜底，不会 panic）"
        );
    }
}

// ==================== D. 行 → DTO 映射（列表 / 详情共用一份赋值路径） ====================

/// ⑭ 只有草稿可编辑；状态 / 类型都带中文名；合计原样透传（服务端在写入口已重算）。
#[test]
fn row_res_marks_only_draft_editable() {
    for status in model::STATUSES {
        let row = plan_row(status);
        let res = to_row_res(&row);
        assert_eq!(
            res.editable,
            *status == model::STATUS_DRAFT,
            "{status}：只有草稿可编辑（前端按钮据此显隐）"
        );
        assert_eq!(res.status, *status);
        assert_eq!(res.status_label, dto::status_label(status));
        assert_eq!(res.plan_type_label, "年度计划");
        assert_eq!(res.id, row.id.to_string());
        assert_eq!(res.created_at, row.created_at.to_rfc3339());
        assert_eq!(res.total_planned_qty, row.total_planned_qty);
        assert_eq!(res.total_planned_amount, row.total_planned_amount);
        assert_eq!(res.total_executed_qty, row.total_executed_qty);
        assert_eq!(res.item_count, row.item_count);
        assert_eq!(res.remark.as_deref(), Some("年度滚动"));
    }
}

/// ⑮ 可空 uuid 列：有值给字符串，无值给 null（前端不用再判空串）。
#[test]
fn row_res_uuid_fields_become_strings_or_null() {
    let mut row = plan_row(model::STATUS_DRAFT);
    let res = to_row_res(&row);
    assert!(res.parent_plan_id.is_none());
    assert!(res.approval_instance_id.is_none());
    assert_eq!(res.dept_id.as_deref(), Some(uid(2).to_string().as_str()));
    assert_eq!(res.planner_id.as_deref(), Some(uid(3).to_string().as_str()));
    assert_eq!(res.create_by.as_deref(), Some(uid(3).to_string().as_str()));

    row.parent_plan_id = Some(uid(4));
    row.parent_plan_no = Some("PPL-20260101-000009".to_string());
    row.approval_instance_id = Some(uid(5));
    let res = to_row_res(&row);
    assert_eq!(res.parent_plan_id.as_deref(), Some(uid(4).to_string().as_str()));
    assert_eq!(res.parent_plan_no.as_deref(), Some("PPL-20260101-000009"));
    assert_eq!(res.approval_instance_id.as_deref(), Some(uid(5).to_string().as_str()));
}

/// ⑯ 明细映射：join 出来的名称 / 单位原样给；**金额不重算**（读路径只搬数据）。
#[test]
fn item_res_passes_amounts_and_joined_names() {
    let mut item = item_row(3, "2", "7.5");
    // 故意造「金额 ≠ 数量 × 单价」：读路径若偷偷重算，这条会红
    item.planned_amount = dec("999");
    let res = to_item_res(&item);
    assert_eq!(res.id, item.id.to_string());
    assert_eq!(res.line_no, 3);
    assert_eq!(res.material_id, item.material_id.to_string());
    assert_eq!(res.material_code, "M-003");
    assert_eq!(res.material_name, "物料 3");
    assert_eq!(res.warehouse_id, item.warehouse_id.to_string());
    assert_eq!(res.warehouse_name, "仓库 3");
    assert_eq!(res.unit_id, item.unit_id.to_string());
    assert_eq!(res.unit_name, "个");
    assert_eq!(res.planned_qty, dec("2"));
    assert_eq!(res.planned_price, dec("7.5"));
    assert_eq!(res.planned_amount, dec("999"), "读路径不重算金额");
    assert_eq!(res.executed_qty, Decimal::ZERO);
    assert_eq!(res.remark.as_deref(), Some("第 3 行"));
}

// ==================== E. 执行率（含） ====================

/// ⑰ 率 × 状态 的完整矩阵：状态判据、百分比换算、率乘回去的舍入误差三者同时成立，
/// 且网格能跑出全部四个状态（证明没有死分支）。
#[test]
fn exec_rate_and_state_are_consistent_over_a_grid() {
    let planned_values = ["1", "3", "7", "100", "1000", "0.0001"];
    let ordered_values = [
        "0", "0.0001", "0.3333", "0.5", "0.9999", "1", "1.0001", "2", "3", "1000",
    ];
    let mut seen = std::collections::BTreeSet::new();
    for planned_text in planned_values {
        for ordered_text in ordered_values {
            let planned = dec(planned_text);
            let ordered = dec(ordered_text);
            let state = exec_dto::exec_state_of(ordered, planned);
            seen.insert(state);

            // 判据（与 V0449 的 CASE 逐条对应；见 ⑳ 的迁移对拍）
            let expected = if ordered > planned {
                exec_dto::STATE_OVER
            } else if ordered >= planned {
                exec_dto::STATE_DONE
            } else if ordered > Decimal::ZERO {
                exec_dto::STATE_PARTIAL
            } else {
                exec_dto::STATE_NOT_STARTED
            };
            assert_eq!(state, expected, "plan={planned} / ordered={ordered}");

            // 率：正计划量必有值，且 = 已下单 / 计划（6 位）
            let rate = exec_dto::exec_rate_of(ordered, planned).expect("正计划量必有分母");
            assert_eq!(rate, (ordered / planned).round_dp(exec_dto::RATE_DP));
            assert!(rate >= Decimal::ZERO, "率不会为负（计划量 > 0、已下单 ≥ 0）");

            // 百分比 = 率 × 100 两位（与预算执行报表同口径）
            assert_eq!(
                exec_dto::pct_of(Some(rate)),
                Some((rate * Decimal::from(100)).round_dp(exec_dto::PCT_DP))
            );

            // 率乘回去只差一个 6 位舍入（不是 ±1%，更不是丢了量）
            let drift = (rate * planned - ordered).abs();
            assert!(
                drift <= planned * dec("0.0000005"),
                "plan={planned} ordered={ordered}：率反算误差 {drift} 超过 6 位舍入"
            );

            // 率的零值 ⟺ 未开工（率的 0 与「无分母」是两回事）
            assert_eq!(state == exec_dto::STATE_NOT_STARTED, ordered.is_zero());
        }
    }
    assert_eq!(
        seen.len(),
        exec_dto::EXEC_STATES.len(),
        "网格必须覆盖全部四态，缺态说明判据写死了"
    );
    for state in exec_dto::EXEC_STATES {
        assert!(seen.contains(state), "{state} 没在网格里出现");
    }
}

/// ⑱ 展示舍入不许翻转状态：`999999.9999996 / 1000000` 显示 1.000000（100.00%），但它是 PARTIAL。
///
/// 前端若拿百分比反推状态（`pct >= 100 → 已下满`），这一行会被画成「已下满」而账上还差 0.0000004。
/// 这条用例钉死「状态只来自后端状态字段」这个约定。
#[test]
fn display_rounding_does_not_flip_the_state() {
    let planned = dec("1000000");
    let ordered = dec("999999.9999996");
    assert_eq!(
        exec_dto::exec_rate_of(ordered, planned),
        Some(dec("1.000000")),
        "率的展示值确实被舍成 1"
    );
    assert_eq!(
        exec_dto::pct_of(exec_dto::exec_rate_of(ordered, planned)),
        Some(dec("100.00")),
        "百分比也是 100.00"
    );
    assert_eq!(
        exec_dto::exec_state_of(ordered, planned),
        exec_dto::STATE_PARTIAL,
        "但原始量没满，状态必须是 PARTIAL"
    );

    // 反向：超一点点，率舍成 1 但状态是 OVER
    let ordered = dec("3.0000004");
    let planned = dec("3");
    assert_eq!(exec_dto::exec_rate_of(ordered, planned), Some(dec("1.000000")));
    assert_eq!(exec_dto::exec_state_of(ordered, planned), exec_dto::STATE_OVER);
}

/// ⑲ 汇总：合计 = 逐行之和；率按**合计**算（不是各行率的平均）；状态按合计判；
/// 四个行状态计数之和 = 行数。
#[test]
fn summary_totals_equal_per_line_sums_and_state_follows_totals() {
    let rows = vec![
        facts("100", "112", "112", "40"), // PARTIAL
        facts("10", "10", "10", "0"),     // NOT_STARTED
        facts("5", "5", "5", "5"),        // DONE
        facts("2", "3", "3", "3"),        // OVER
    ];
    let summary = exec_dto::summary_of(&rows);
    assert_eq!(summary.line_count, 4);
    assert_eq!(summary.planned_qty, dec("117"));
    assert_eq!(summary.booked_qty, dec("130"));
    assert_eq!(summary.req_qty, dec("130"));
    assert_eq!(summary.ordered_qty, dec("48"));
    assert_eq!(summary.remaining_qty, dec("69"), "剩余 = 合计计划量 − 合计已下单");
    assert_eq!((summary.not_started_lines, summary.partial_lines), (1, 1));
    assert_eq!((summary.done_lines, summary.over_lines), (1, 1));
    assert_eq!(
        summary.not_started_lines + summary.partial_lines + summary.done_lines + summary.over_lines,
        summary.line_count,
        "行状态计数必须覆盖每一行（漏一个状态就会少于行数）"
    );

    // 率按合计：48 / 117，舍到 6 位
    assert_eq!(summary.exec_rate, Some(dec("0.410256")));
    // **不是**各行率的算术平均（(0.4 + 0 + 1 + 1.5) / 4 = 0.725）
    let mean = (dec("0.4") + Decimal::ZERO + Decimal::ONE + dec("1.5")) / Decimal::from(4);
    assert_ne!(summary.exec_rate, Some(mean), "汇总是加权（按量），不是平均");
    // 已申请率同样按合计：130 / 117 > 1（申请超计划是允许的）
    assert_eq!(summary.req_rate, Some(dec("1.111111")));
    assert_eq!(summary.exec_state(), exec_dto::STATE_PARTIAL);
    assert_eq!(summary.exec_rate_pct(), Some(dec("41.03")));
    assert_eq!(summary.req_rate_pct(), Some(dec("111.11")));
}

/// ⑳ 整单状态按**合计**判：一行超计划 + 一行部分，合计恰好下满 → 整单 DONE。
///
/// 与「行状态多数」口径的区别在这里显形：行上 OVER 与 PARTIAL 各一，
/// 整单既不是 OVER 也不是 PARTIAL。
#[test]
fn summary_mixed_over_and_under_can_land_done() {
    let rows = vec![
        facts("100", "150", "150", "150"), // OVER
        facts("100", "50", "50", "50"),    // PARTIAL
    ];
    let summary = exec_dto::summary_of(&rows);
    assert_eq!((summary.over_lines, summary.partial_lines), (1, 1));
    assert_eq!(summary.planned_qty, dec("200"));
    assert_eq!(summary.ordered_qty, dec("200"));
    assert_eq!(summary.remaining_qty, Decimal::ZERO);
    assert_eq!(
        summary.exec_state(),
        exec_dto::STATE_DONE,
        "整单按合计判（不是行状态多数，也不是「有一行超就整单超」）"
    );
    assert_eq!(summary.exec_rate, Some(Decimal::ONE));

    // 空计划：没有分母 → 率 None（不是 0），行数 0
    let empty = exec_dto::summary_of(&[]);
    assert_eq!(empty.line_count, 0);
    assert_eq!(empty.exec_rate, None);
    assert_eq!(empty.exec_rate_pct(), None);
    assert_eq!(empty.req_rate, None);
}

/// ㉑ 「已转申请」与「已下单」是两个量：只转了申请（还没转采购单）时
/// 已申请率 100%、执行率仍是 0、状态仍是未开工 —— 这正是 M4-04 要显形的东西。
#[test]
fn request_rate_and_exec_rate_diverges_when_not_ordered_yet() {
    let row = facts("100", "100", "100", "0");
    let summary = exec_dto::summary_of(&[row]);
    assert_eq!(summary.req_rate, Some(Decimal::ONE), "申请 100/100");
    assert_eq!(summary.exec_rate, Some(Decimal::ZERO), "下单 0/100");
    assert_eq!(summary.req_rate_pct(), Some(dec("100.00")));
    assert_eq!(summary.exec_rate_pct(), Some(dec("0.00")));
    assert_eq!(summary.exec_state(), exec_dto::STATE_NOT_STARTED);
    assert_eq!(summary.remaining_qty, dec("100"), "还没下单 → 剩余仍是全量");
    // 而「还能再转申请」是 0（M4-03 的回写列已满）
    assert_eq!(exec_dto::convertible_of(dec("100"), dec("100")), Decimal::ZERO);

    // 采购单作废 → 来源判定不计入：该行已下单量回落到 0（链断即不算）
    let (counted, reason) = exec_dto::source_verdict(Some(uid(40)), Some("CANCELLED"), false);
    assert!(!counted);
    assert_eq!(reason, "采购单已作废");
    let after_void = exec_dto::summary_of(&[facts("100", "100", "100", "0")]);
    assert_eq!(after_void.ordered_qty, Decimal::ZERO);
    assert_eq!(after_void.exec_rate, Some(Decimal::ZERO));
}

/// ㉒ 剩余量两套口径：报表 `remaining_of` 保留负数（超计划要显形），
/// 转申请 `convertible_of` 夹到 0（不许显示「还能转 -5」）；与转单模块的 `ConvertibleLine::remaining` 对拍。
#[test]
fn convertible_clips_at_zero_while_remaining_keeps_negative() {
    assert_eq!(exec_dto::remaining_of(dec("100"), dec("130")), dec("-30"));
    assert_eq!(exec_dto::convertible_of(dec("100"), dec("130")), Decimal::ZERO);
    assert_eq!(exec_dto::remaining_of(dec("100"), dec("40")), dec("60"));
    assert_eq!(exec_dto::convertible_of(dec("100"), dec("40")), dec("60"));

    // 跨模块对拍（4 位标度内必须完全一致；两处一起改才不漂）
    let line = |planned: &str, booked: &str| convert_dto::ConvertibleLine {
        plan_item_id: uid(41),
        line_no: 1,
        material_id: uid(42),
        material_code: "M-001".to_string(),
        material_name: "物料 1".to_string(),
        warehouse_id: uid(43),
        warehouse_name: "仓库 1".to_string(),
        unit_id: uid(44),
        planned_qty: dec(planned),
        executed_qty: dec(booked),
        planned_price: dec("1"),
        remark: None,
    };
    for (planned, booked) in [
        ("100", "40"),
        ("100", "100"),
        ("100", "130"),
        ("0.0001", "0.0001"),
        ("7", "6.9999"),
    ] {
        assert_eq!(
            line(planned, booked).remaining(),
            exec_dto::convertible_of(dec(planned), dec(booked)),
            "plan={planned} booked={booked}：转申请剩余量与执行率的可转量必须同口径"
        );
    }

    // 刻意保留的差异：转申请按 4 位标度落库（QTY_SCALE），执行率视图保留原值。
    // 差异只在第 5 位小数，且各自都在自己的契约里（不许「顺手抹平」把某一侧改坏）。
    assert_eq!(line("7", "0.00004").remaining(), dec("7"));
    assert_eq!(exec_dto::convertible_of(dec("7"), dec("0.00004")), dec("6.99996"));
}

/// ㉓ 六条权限码与 DTO 清单一致；页面显形码（`…:page`）不许出现在后端（AGENTS 约定 14）。
#[test]
fn permission_constants_are_the_six_api_codes_and_none_is_page_only() {
    let declared = [
        PERM_LIST,
        PERM_CREATE,
        PERM_UPDATE,
        PERM_DELETE,
        PERM_SUBMIT,
        PERM_TO_REQUISITION,
    ];
    assert_eq!(declared.len(), dto::PLAN_PERMISSIONS.len());
    for code in declared {
        assert!(
            dto::PLAN_PERMISSIONS.contains(&code),
            "{code} 不在 dto::PLAN_PERMISSIONS 里（两份清单漂移）"
        );
        assert!(code.starts_with("business:erp:purchase-plan:"), "{code} 前缀不对");
        assert!(
            !code.ends_with(":page"),
            "{code}：页面显形码只给前端，后端零引用（约定 14）"
        );
    }
    for code in dto::PLAN_PERMISSIONS {
        assert!(declared.contains(&code), "dto 里有 {code}，handler 没声明");
    }
    // 执行率的码单独一个模块，同样不许是页面码
    assert_eq!(exec_dto::EXEC_PERMISSION, "business:erp:purchase-plan:execution");
    assert!(!exec_dto::EXEC_PERMISSION.ends_with(":page"));
}

/// ㉔ 纯函数的状态判据与 V0449 的 CASE **逐词对拍**（含率的 6 位舍入与「计划量 0 不给率」）。
///
/// 视图改了而纯函数没改 = 报表（读视图）与详情（读纯函数交叉校验）两套口径；
/// 这条用例把两处绑在同一个提交里。
#[test]
fn exec_state_case_matches_pure_function() {
    let item = squeeze(PP_EXEC_SQL);
    // 明细级：四态 + 率的 6 位舍入
    assert!(
        item.contains(&format!("VIEW {} AS", exec_dto::ITEM_VIEW)),
        "明细视图名与 exec_dto::ITEM_VIEW 不一致"
    );
    assert!(item.contains("WHEN COALESCE(po.ordered_qty, 0) > it.planned_qty THEN 'OVER'"));
    assert!(item.contains("WHEN COALESCE(po.ordered_qty, 0) >= it.planned_qty THEN 'DONE'"));
    assert!(item.contains("WHEN COALESCE(po.ordered_qty, 0) > 0 THEN 'PARTIAL'"));
    assert!(item.contains("ELSE 'NOT_STARTED'"));
    assert!(
        item.contains(&format!(
            "round(COALESCE(po.ordered_qty, 0) / it.planned_qty, {})",
            exec_dto::RATE_DP
        )),
        "视图舍入位数必须等于 exec_dto::RATE_DP（{}）",
        exec_dto::RATE_DP
    );
    // 剩余量刻意保留负数（不是 GREATEST(…, 0)）
    assert!(item.contains("it.planned_qty - COALESCE(po.ordered_qty, 0) AS remaining_qty"));
    assert!(!item.contains("GREATEST"), "剩余量不许夹到 0（超计划要显形）");

    // 单头级：整单按合计判 + 计划量 0 不给率（CASE WHEN … > 0 THEN round(…) END）
    assert!(
        item.contains(&format!("VIEW {} AS", exec_dto::HEAD_VIEW)),
        "单头视图名与 exec_dto::HEAD_VIEW 不一致"
    );
    assert!(item.contains("WHEN COALESCE(agg.ordered_qty, 0) > COALESCE(agg.planned_qty, 0) THEN 'OVER'"));
    assert!(item.contains("WHEN COALESCE(agg.ordered_qty, 0) >= COALESCE(agg.planned_qty, 0) THEN 'DONE'"));
    assert!(item.contains("WHEN COALESCE(agg.ordered_qty, 0) > 0 THEN 'PARTIAL'"));
    assert!(
        item.contains(
            "CASE WHEN COALESCE(agg.planned_qty, 0) > 0 THEN round(COALESCE(agg.ordered_qty, 0) / agg.planned_qty"
        ),
        "单头率的空分母保护（与 exec_rate_of 返回 None 同义）"
    );
    // 四个行状态计数由明细视图的 exec_state 聚合，字段名与 ExecSummary 一致
    for column in ["not_started_lines", "partial_lines", "done_lines", "over_lines"] {
        assert!(item.contains(column), "单头视图缺 {column}（ExecSummary 有）");
    }
}
