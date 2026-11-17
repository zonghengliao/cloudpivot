// ============================================================================
// quotation_convert_tests.rs —— 销售报价单 → 销售订单「转单」专测（M3-08）
//
// 卡：t_8d301195（M3-08【M3·销售】报价单转订单：单测 / 含部分转单）
// 上游：M3-01 表（V0432）/ M3-02 CRUD+状态机 / M3-03 转单主链 / M3-04 审批闸 / M3-05 报价单单测
//
// # 这一份钉的是什么
//
// 只钉**转单这一条链路**（`POST /business/erp/quotation/{id}/convert`）：
//
// ```text
// 入参闸   parse_convert_body   ← 空 body / 非法 JSON / 未知参数 / **部分转单**(line_nos…)
// 窗口闸   order_date × delivery_date × salesperson_id
// 状态闸   ensure_convertible   ← 只有 ACCEPTED
// 行转换   plan_order_items     ← 逐行整列搬 + 四道金额自检（含重复物料 / 赠品行 / 按行舍入）
// 备注     compose_order_remark ← 来源永不被顶掉
// 事务     建订单 + 回填 + 轨迹 同一个事务；中途失败整笔回滚（含取号回退）
// 幂等     已转过 → 回原单（`already_converted = true`），不建第二张
// 库层兜底 `uk_biz_quotation_converted_so` —— 一张订单只被一张报价单认领
// ```
//
// # 与 M3-03 / M3-05 两份用例的分工（不重测）
//
// ```text
// quotation_handler::tests          ← M3-03 自带：转单主链 1 条 + 幂等 + 守卫回滚（骨干）
// quotation_state_tests             ← M3-05：状态机 / 审批闸 / 已转单不许作废 / 并发一张单
// 本文件（M3-08）                    ← 入参闸（含**部分转单**拒绝）、窗口闸、行级复制口径
//                                     （重复物料 / 赠品行 / 按行舍入）、事务原子性（注入中途失败）、
//                                     库层反向唯一、主数据名字缺失、过期已接受仍可转
// ```
//
// # 「含部分转单」（卡面点名）的落点
//
// 本接口**只做整单转单**：V0432 的回链是单值 `converted_so_id`，
// `uk_biz_quotation_converted_so` 又钉死「一张订单只被一张报价单认领」
// ⇒ 一张报价单只能换出一张订单。客户只接受部分明细时的正解是**新开一张只保留接受行的
// 报价单**再转，或整单转换后在 `DRAFT` 订单里删行。
//
// 所以本文件的「部分转单」用例钉的是**拒绝口径**：
//   ① 请求体带 `line_nos` / `lines` / `items` / … → **400 + 专用文案**（6 个键逐个验），
//      绝不能被 serde 静默丢掉后「按全量转出一张含全部明细的订单」；
//   ② 被拒之后库里**零订单、报价单未回填**，且同一张单用空 body 仍能整单转（不留死结）；
//   ③ 行级数据「部分脏」时（只有一行金额对不上）**整笔拒绝**，不会出现「转了一半」的订单。
//
// ⚠ 真库用例必须串行（共用全局 `db()`、同一个 `M308-` 夹具、同一份 `biz_seq` 号段），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m308_ut" cargo test --lib \
//   business::erp::sales::quotation_handler::quotation_convert_tests -- --test-threads=1
// ```
//
// 也可以直接跑 `bash scripts/verify/m308_quotation_convert_drill.sh`
// （克隆演练库 → 补 V0405…V0439 → 跑本组 + 报价单整组回归 → 断言夹具与触发器零残留）。
// ============================================================================

use super::{
    CONVERT_ACTION, ConvertQuotationReq, compose_order_remark, convert_quotation, ensure_convertible,
    parse_convert_body, plan_order_items,
};
use crate::business::erp::fixture_hygiene;
use crate::business::erp::sales::quotation_model as model;
use crate::business::erp::sales::sales_order_model as so_model;
use crate::test_seed;
use cloudpivot_kernel::business::shared::state_machine;
use cloudpivot_kernel::shared::error_code;
use nik::prelude::chrono::{Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::str::FromStr;

/// 定点字面量（`17.65` 这类在 f64 下不精确，一律走字符串）。
fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("定点可解析")
}

/// 固定日期。
fn day(year: i32, month: u32, dom: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, dom).expect("合法日期")
}

// ============================================================================
// 一、入参闸（纯逻辑，不连库）
// ============================================================================

/// 卡面点名的「部分转单」：6 个「转哪几行」的键**逐个**都要 400，文案要能指导下一步。
///
/// 这条用例是防「静默降级」的：`serde` 默认丢未知字段，一旦这里松掉，
/// 调用方传 `{"line_nos":[1]}` 会拿到一张**含全部明细**的订单 —— 那张单要占信用、
/// 要出库、要开票，而且界面上看不出比客户认的多转了几行。
#[test]
fn partial_convert_keys_are_rejected_with_a_way_out() {
    for key in ["line_nos", "line_no", "lines", "items", "line_ids", "convert_lines"] {
        let body = format!(r#"{{"{key}":[1,3]}}"#);
        let err = parse_convert_body(body.as_bytes()).expect_err(&format!("`{key}` 必须被拒（部分转单不支持）"));
        assert_eq!(err.status, 400, "部分转单是入参问题：{key}");
        assert_eq!(err.code, error_code::PARAM_INVALID, "错误码要能区分入参问题：{key}");
        assert!(err.msg.contains("部分转单"), "文案要点名「部分转单」：{}", err.msg);
        assert!(err.msg.contains(key), "文案要指出是哪个参数被拒（{key}）：{}", err.msg);
        // 给人下一步：新建只留接受行的报价单 / 整单转换后删行
        assert!(
            err.msg.contains("新建") && err.msg.contains("删"),
            "被拒之后用户要知道怎么办（{key}）：{}",
            err.msg
        );
    }

    // 反过来：真正整单转单的参数不能被这条闸误伤
    let ok = parse_convert_body(r#"{"order_date":"2026-11-17","remark":"款到发货"}"#.as_bytes())
        .expect("整单转单参数必须放行");
    assert_eq!(ok.order_date.as_deref(), Some("2026-11-17"));
    assert_eq!(ok.remark.as_deref(), Some("款到发货"));
}

/// 未知键（拼错的 `order_dtae`、旧版前端多传）一律 400 —— 参数被丢掉 = 用户看到的行为和参数不符。
///
/// 与「部分转单」分开验：那个给专用文案，这个给 serde 的「expected one of …」原文
/// （出现新参数时排障第一眼就能看到合法键名）。
#[test]
fn unknown_and_misspelled_params_are_400_not_silently_dropped() {
    for body in [
        r#"{"order_dtae":"2026-11-17"}"#,
        r#"{"delivery_date":"2026-11-20","note":"加急"}"#,
        r#"{"quotation_id":"x"}"#,
    ] {
        let err = parse_convert_body(body.as_bytes()).expect_err("未知参数必须 400");
        assert_eq!(err.status, 400, "请求体：{body}");
        assert!(
            err.msg.contains("参数不认识"),
            "文案要说清是参数问题（{body}）：{}",
            err.msg
        );
    }
    // 报错里要能看出合法键名（排障靠它，不靠读源码）
    let err = parse_convert_body(br#"{"order_dtae":"2026-11-17"}"#).expect_err("拼错必须 400");
    for field in ["order_date", "delivery_date", "salesperson_id", "remark"] {
        assert!(err.msg.contains(field), "报错要列出合法参数 {field}：{}", err.msg);
    }
    assert!(err.msg.contains("order_dtae"), "报错要回显拼错的键：{}", err.msg);
}

/// 空 body / `{}` / 四个键全 `null`（前端 form 原样提交）都走默认值；
/// 非对象、非法 JSON、非 UTF-8 一律 400。
#[test]
fn empty_object_and_null_bodies_default_but_never_partially_parse() {
    for empty in [&b""[..], b"   ", b"\n\t ", b"{}"] {
        let req = parse_convert_body(empty).expect("空 body / 空对象 = 全默认");
        assert!(req.order_date.is_none() && req.delivery_date.is_none());
        assert!(req.salesperson_id.is_none() && req.remark.is_none());
    }

    // 前端把可空字段原样序列化成 null（`ConvertQuotationReq` 的 `?: string | null`）
    let req = parse_convert_body(br#"{"order_date":null,"delivery_date":null,"salesperson_id":null,"remark":null}"#)
        .expect("null 等同「没填」");
    assert!(req.order_date.is_none() && req.delivery_date.is_none());
    assert!(req.salesperson_id.is_none() && req.remark.is_none());

    // 四个键齐全时全部解析出来
    let req = parse_convert_body(
        format!(
            r#"{{"order_date":"2026-11-17","delivery_date":"2026-11-20","salesperson_id":"{}","remark":"款到发货"}}"#,
            Uuid::now_v7()
        )
        .as_bytes(),
    )
    .expect("四个参数都应被接受");
    assert_eq!(req.order_date.as_deref(), Some("2026-11-17"));
    assert_eq!(req.delivery_date.as_deref(), Some("2026-11-20"));
    assert!(req.salesperson_id.is_some());
    assert_eq!(req.remark.as_deref(), Some("款到发货"));

    // 不是 JSON 对象：数组 / 字符串 / 数字 / null 都要 400（说清「必须是对象」）
    for body in [&b"[1,2,3]"[..], b"\"hello\"", b"123", b"null"] {
        let err = parse_convert_body(body).expect_err("非对象必须 400");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("JSON 对象"), "文案要说清要对象：{}", err.msg);
    }
    // 非法 JSON / 非 UTF-8
    for body in [&b"{"[..], b"order_date=2026-11-17", b"{\"order_date\":}"] {
        let err = parse_convert_body(body).expect_err("非法 JSON 必须 400");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("JSON"), "文案要说清是 JSON 问题：{}", err.msg);
    }
    let err = parse_convert_body(&[0xff, 0xfe]).expect_err("非 UTF-8 必须 400");
    assert_eq!(err.status, 400);
    assert!(err.msg.contains("UTF-8"), "{}", err.msg);
}

/// 窗口闸：日期格式严格、交期不能早于点单日 —— 两条都**在读库之前**给出 400
/// （这样用户拿到的永远是「你填错了」，而不是连不上库的 500）。
#[tokio::test]
async fn date_window_is_validated_before_any_database_work() {
    let id = Uuid::now_v7().to_string();
    // 非法日期格式：报文点名字段
    for (field, value) in [
        ("order_date", "2026/11/17"),
        ("order_date", "20261117"),
        ("delivery_date", "2026-13-01"),
    ] {
        let req = ConvertQuotationReq {
            order_date: (field == "order_date").then(|| value.to_string()),
            delivery_date: (field == "delivery_date").then(|| value.to_string()),
            ..ConvertQuotationReq::default()
        };
        let err = convert_quotation(None, &id, req).await.expect_err("非法日期必须 400");
        assert_eq!(err.status, 400, "字段 {field} = {value}");
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID);
        assert!(err.msg.contains(field), "报文要指名字段（{field}）：{}", err.msg);
    }

    // 交期早于点单日 → 400，两个日期都要出现在报文里（用户能对照着改）
    let err = convert_quotation(
        None,
        &id,
        ConvertQuotationReq {
            order_date: Some("2026-11-20".to_string()),
            delivery_date: Some("2026-11-19".to_string()),
            ..ConvertQuotationReq::default()
        },
    )
    .await
    .expect_err("交期早于点单日必须拒");
    assert_eq!(err.status, 400);
    assert!(
        err.msg.contains("2026-11-19") && err.msg.contains("2026-11-20"),
        "报文要给出两个日期：{}",
        err.msg
    );

    // 销售员 id 不是 UUID → 400（同样在读库之前）
    let err = convert_quotation(
        None,
        &id,
        ConvertQuotationReq {
            salesperson_id: Some("not-a-uuid".to_string()),
            ..ConvertQuotationReq::default()
        },
    )
    .await
    .expect_err("非法销售员 id 必须 400");
    assert_eq!(err.status, 400);
    assert!(err.msg.contains("salesperson_id"), "报文要指名字段：{}", err.msg);
}

// ============================================================================
// 二、状态闸 + 行转换 + 备注（纯逻辑）
// ============================================================================

/// 一张报价单（只有转单用得上的字段有真实值）。
fn quote_row(status: &str, amount: &str, tax: &str) -> model::QuotationRow {
    let today = day(2026, 11, 17);
    model::QuotationRow {
        id: Uuid::now_v7(),
        quote_no: "QT-20261117-000001".to_string(),
        customer_id: Uuid::now_v7(),
        quote_date: today,
        valid_until: today,
        status: status.to_string(),
        total_amount: dec(amount),
        total_tax: dec(tax),
        total_with_tax: dec(amount) + dec(tax),
        converted_so_id: None,
        converted_at: None,
        approval_instance_id: None,
        remark: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        create_by: None,
        update_by: None,
        deleted_at: None,
    }
}

/// 一行报价明细（金额按 model 层口径现算，保证「干净数据」）。
fn quote_line(line_no: i32, material: Uuid, qty: &str, price: &str, rate: &str) -> model::QuotationLineRow {
    let (qty, price, tax_rate) = (dec(qty), dec(price), dec(rate));
    let (amount, tax_amount) = model::line_amounts(qty, price, tax_rate);
    model::QuotationLineRow {
        id: Uuid::now_v7(),
        quotation_id: Uuid::now_v7(),
        line_no,
        material_id: material,
        qty,
        price,
        amount,
        tax_rate,
        tax_amount,
        remark: None,
    }
}

/// 单头合计按行的实际金额对齐（避免用例「先被合计不一致挡下」，测不到想测的东西）。
fn row_of_lines(status: &str, lines: &[model::QuotationLineRow]) -> model::QuotationRow {
    let amount: Decimal = lines.iter().map(|line| line.amount).sum();
    let tax: Decimal = lines.iter().map(|line| line.tax_amount).sum();
    quote_row(status, &amount.to_string(), &tax.to_string())
}

/// 状态闸：**只有 ACCEPTED**；其余四态一律 409，报文带单号 + 中文标签 + 状态码。
///
/// 409 而不是 400/403：单据存在、用户也有权限，只是当前状态不允许 —— 前端据此渲染
/// 「先去标客户已接受」而不是弹出「无权限」。
#[test]
fn convert_guard_names_status_label_and_quote_no_for_every_refusal() {
    let accepted = quote_row(model::STATUS_ACCEPTED, "0", "0");
    ensure_convertible(&accepted).expect("已接受必须能转");

    for (status, label) in [
        (model::STATUS_DRAFT, "草稿"),
        (model::STATUS_SENT, "已发出"),
        (model::STATUS_REJECTED, "已拒绝"),
        (model::STATUS_CANCELLED, "已作废"),
    ] {
        let err = ensure_convertible(&quote_row(status, "0", "0")).expect_err("非 ACCEPTED 一律不能转");
        assert_eq!(err.status, 409, "{status} 必须是状态冲突");
        assert_eq!(err.code, error_code::RESOURCE_CONFLICT, "{status}");
        assert!(err.msg.contains(status), "报文要带状态码（{status}）：{}", err.msg);
        assert!(err.msg.contains(label), "报文要带中文标签（{label}）：{}", err.msg);
        assert!(
            err.msg.contains("QT-20261117-000001"),
            "报文要点名单号（不然用户在列表里不知道点的是哪张）：{}",
            err.msg
        );
        assert!(
            err.msg.contains(model::STATUS_ACCEPTED),
            "报文要给出目标状态（ACCEPTED）：{}",
            err.msg
        );
    }

    // SENT 的文案要额外给「先标客户已接受」这一步（它是唯一一个「差一步就能转」的状态）
    let sent = ensure_convertible(&quote_row(model::STATUS_SENT, "0", "0")).expect_err("SENT 不能直接转");
    assert!(sent.msg.contains("客户已接受"), "SENT 要给可执行提示：{}", sent.msg);
    assert!(sent.msg.contains("状态接口"), "要指出用哪个入口改：{}", sent.msg);
}

/// 逐行整列搬，**不合并、不丢行、不改金额**：
/// 同一物料出现两次（不同单价）要两行都在；单价 0 的赠品行合法；行数必须与报价行数相等。
///
/// 这条防的是「按物料聚合」这类优化 —— 一旦合并，出库数量对、金额对，但客户认的
/// 两种价格只剩一种，退货 / 对账时完全对不上。
#[test]
fn plan_copies_every_line_even_with_repeated_material_and_zero_price() {
    let shared = Uuid::now_v7();
    let gift = Uuid::now_v7();
    let lines = vec![
        quote_line(1, shared, "10", "9.5", "13"),
        quote_line(2, gift, "5", "0", "0"),
        quote_line(3, shared, "2", "8.25", "13"),
    ];
    let row = row_of_lines(model::STATUS_ACCEPTED, &lines);

    let (items, totals) = plan_order_items(&row, &lines).expect("干净数据必须通过");
    assert_eq!(items.len(), lines.len(), "行数必须一一对应（不许合并、不许丢行）");
    for (index, (item, line)) in items.iter().zip(lines.iter()).enumerate() {
        assert_eq!(item.material_id, line.material_id, "第 {} 行物料", index + 1);
        assert_eq!(item.qty, line.qty, "第 {} 行数量", index + 1);
        assert_eq!(
            item.price,
            line.price,
            "第 {} 行单价（同一物料两种价都要在）",
            index + 1
        );
        assert_eq!(item.tax_rate, line.tax_rate, "第 {} 行税率", index + 1);
        assert_eq!(item.amount, line.amount, "第 {} 行金额整列搬，不重算", index + 1);
        assert_eq!(item.tax_amount, line.tax_amount, "第 {} 行税额整列搬", index + 1);
    }
    assert_eq!(items[0].material_id, items[2].material_id, "前提：同一物料出现两次");
    assert_ne!(items[0].price, items[2].price, "前提：两行价格不同");
    assert_eq!(items[1].price, Decimal::ZERO, "赠品行单价 0 合法（>= 0 即可）");
    assert_eq!(items[1].tax_amount, Decimal::ZERO);
    assert_eq!(totals.amount, row.total_amount);
    assert_eq!(totals.tax, row.total_tax);
    assert_eq!(totals.with_tax, row.total_with_tax);
}

/// 金额口径：**先按行舍入再相加**（不是先加再舍入），否则订单与报价单会差几分钱。
///
/// 例：3 行「1 × 0.00005」→ 每行 round4 = 0.0001，合计 0.0003；
/// 若先加再舍入 = round4(0.00015) = 0.0002 —— 差 1 个最小单位，足够让对账报警。
#[test]
fn plan_keeps_per_line_rounding_and_sums_the_rounded_lines() {
    let lines: Vec<model::QuotationLineRow> = (1..=3)
        .map(|line_no| quote_line(line_no, Uuid::now_v7(), "1", "0.00005", "13"))
        .collect();
    assert_eq!(lines[0].amount, dec("0.0001"), "前提：单行 round4 命中半值进位");
    let row = row_of_lines(model::STATUS_ACCEPTED, &lines);

    let (items, totals) = plan_order_items(&row, &lines).expect("半值行也必须能转");
    assert_eq!(items.len(), 3);
    assert_eq!(
        totals.amount,
        dec("0.0003"),
        "先按行舍入再相加（不是先加再舍入的 0.0002）"
    );
    assert_eq!(totals.tax, dec("0.0000"), "0.0001 × 13% 舍入到 4 位是 0");
    assert_eq!(totals.with_tax, dec("0.0003"));
}

/// 「部分脏」= 只有一行 / 只有单头出问题 → **整笔拒绝**（不是少搬一行、也不是凑合转）。
///
/// 四类脏数据里，M3-03 已覆盖「数量 0 / 单价负 / 税率越界 / 行金额对不上」；
/// 这里补的是最容易漏的两类：**只有税额被改脏**、**单头税额与明细差一分**。
#[test]
fn plan_rejects_partially_dirty_data_as_a_whole() {
    let lines = vec![
        quote_line(1, Uuid::now_v7(), "2", "10", "13"),
        quote_line(2, Uuid::now_v7(), "1", "30", "13"),
    ];
    let row = row_of_lines(model::STATUS_ACCEPTED, &lines);

    // ① 只有第 2 行的 tax_amount 被绕过写入口改脏 → 拒，报文要指出第几行
    let mut dirty = lines.clone();
    dirty[1].tax_amount = dec("999.9999");
    let err = plan_order_items(&row, &dirty).expect_err("脏税额必须拒");
    assert_eq!(err.status, 409);
    assert!(err.msg.contains("第 2 行"), "报文要点名哪一行：{}", err.msg);
    assert!(err.msg.contains("tax_amount"), "报文要点名字段：{}", err.msg);

    // ② 单头税额比明细多一分 → 拒（订单与报价单必须是同一分钱）
    let mut bad_header = row.clone();
    bad_header.total_tax += dec("0.0001");
    bad_header.total_with_tax += dec("0.0001");
    let err = plan_order_items(&bad_header, &lines).expect_err("单头合计不一致必须拒");
    assert_eq!(err.status, 409);
    assert!(err.msg.contains("不一致"), "{}", err.msg);
    assert!(err.msg.contains("合计"), "报文要说清是合计问题：{}", err.msg);

    // ③ 空明细仍然先拒（空订单提交上去，审批人批的是空单）
    let err = plan_order_items(&row, &[]).expect_err("空明细必须拒");
    assert_eq!(err.status, 409);
    assert!(err.msg.contains("没有明细"), "{}", err.msg);

    // ④ 只删掉一行（= 部分转单的数据形态）+ 单头不动 → 同样被合计自检挡下
    let err = plan_order_items(&row, &lines[..1]).expect_err("少一行必然与单头合计对不上");
    assert_eq!(err.status, 409);
    assert!(err.msg.contains("不一致"), "{}", err.msg);
}

/// 税率边界：0 与 100 都合法（0 = 免税、100 = 极端价内税），越界才拒。
#[test]
fn plan_accepts_tax_rate_bounds_zero_and_hundred() {
    let zero = quote_line(1, Uuid::now_v7(), "1", "100", "0");
    let hundred = quote_line(2, Uuid::now_v7(), "1", "100", "100");
    let lines = vec![zero, hundred];
    let row = row_of_lines(model::STATUS_ACCEPTED, &lines);
    let (items, totals) = plan_order_items(&row, &lines).expect("0% 与 100% 都是合法税率");
    assert_eq!(items[0].tax_amount, Decimal::ZERO, "0% 税额恒 0");
    assert_eq!(items[1].tax_amount, dec("100.0000"), "100% 税额 = 行金额");
    assert_eq!(totals.with_tax, dec("300.0000"));
}

/// 订单备注：用户填的文字只是**前缀**，来源（`由报价单 QT-… 转换`）永不被顶掉 ——
/// 哪怕用户自己写了句像来源的假话。
#[test]
fn remark_keeps_the_real_source_even_when_user_text_mimics_it() {
    let quote_no = "QT-20261117-000042";
    assert_eq!(
        compose_order_remark(quote_no, None),
        format!("由报价单 {quote_no} 转换")
    );
    assert_eq!(
        compose_order_remark(quote_no, Some("款到发货")),
        format!("款到发货｜由报价单 {quote_no} 转换")
    );
    // 前后空白要收掉；纯空白等同没填
    assert_eq!(
        compose_order_remark(quote_no, Some("  款到发货  ")),
        format!("款到发货｜由报价单 {quote_no} 转换")
    );
    assert_eq!(
        compose_order_remark(quote_no, Some("   ")),
        format!("由报价单 {quote_no} 转换")
    );

    // 用户自己写「由报价单 QT-FAKE 转换」→ 真来源仍然在最后，假的不被当成来源
    let composed = compose_order_remark(quote_no, Some("由报价单 QT-FAKE 转换"));
    assert!(
        composed.ends_with(&format!("由报价单 {quote_no} 转换")),
        "真来源必须压轴：{composed}"
    );
    assert!(
        composed.starts_with("由报价单 QT-FAKE 转换｜"),
        "用户文字原样保留：{composed}"
    );
}

// ============================================================================
// 三、真库用例（M308- 夹具；每例开始时清场）
// ============================================================================

/// 真库用例串行锁（与 M3-03 / M3-05 各自那把同口径：本组共用全局 `db()` 与号段）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 夹具客户编码（清场按它定位 —— 单号是取号器生成的，不能按前缀删单据）。
const CUSTOMER_CODE: &str = "M308-CUST";
/// 夹具销售员用户名（ACTIVE / 停用各一个）。
///
/// 账号走 `test_seed::ensure_user`（P2-62 门禁：测试代码里手写 `sys_user` 的 INSERT
/// 有预算上限、只许降不许升），停用那一个用 `UPDATE … status = 0` 表达 —— UPDATE 不算夹具新增。
const SALES_USER_ACTIVE: &str = "m308_sales_active";
const SALES_USER_INACTIVE: &str = "m308_sales_inactive";
/// 原子性用例注入故障用的 CHECK 约束名（清场必须拆掉，否则会污染后面的用例）。
///
/// 为什么用「临时 CHECK 约束」而不是 PG 触发器：触发器要写 plpgsql 函数体（`$$ … $$`），
/// 在 prepared statement 里传整段 DDL 容易与占位符解析打架，而且在别的语句里漏拆会长期隐身。
/// `CHECK (action IS DISTINCT FROM 'CONVERT') NOT VALID` 只挡**新写入**、不扫存量行，
/// 一拆就干净 —— 语义正好是「这一步必失败」。
const BLOCK_CONSTRAINT: &str = "ck_m308_block_convert";

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
            match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                Ok(connection) => match set_db(connection) {
                    Ok(()) => true,
                    Err(err) => {
                        println!("SKIP: set_db 失败（{err}）");
                        false
                    }
                },
                Err(err) => {
                    println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 清场：注入故障的约束 → 状态日志 → 销售订单（明细 CASCADE）→ 报价单（明细 CASCADE）→ 客户 → 夹具账号。
///
/// 状态日志**先于**单据删（它没有外键，单据一删就再也定位不到 id —— 孤儿行收不走）。
async fn cleanup() {
    let by_customer = "SELECT id FROM biz_customer WHERE code = ?";
    let quotation_of_customer = format!("SELECT id FROM biz_quotation WHERE customer_id IN ({by_customer})");
    // 注入故障的约束先拆：中途 panic 的用例如果不拆，后面的用例会被它误伤
    let sql = format!("ALTER TABLE biz_status_log DROP CONSTRAINT IF EXISTS {BLOCK_CONSTRAINT}");
    if let Err(err) = db().execute_with_params(&sql, vec![]).await {
        panic!("清理注入约束失败：{err}\nSQL: {sql}");
    }
    let statements: Vec<(String, Vec<Value>)> = vec![
        (
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'QUOTATION' AND entity_id IN ({quotation_of_customer})"
            ),
            vec![Value::from(CUSTOMER_CODE)],
        ),
        (
            format!(
                "DELETE FROM biz_status_log \
                  WHERE entity_type = 'SALE_ORDER' AND entity_id IN ( \
                        SELECT id FROM biz_sales_order WHERE customer_id IN ({by_customer}))"
            ),
            vec![Value::from(CUSTOMER_CODE)],
        ),
        (
            format!("DELETE FROM biz_sales_order WHERE customer_id IN ({by_customer})"),
            vec![Value::from(CUSTOMER_CODE)],
        ),
        (
            format!("DELETE FROM biz_quotation WHERE customer_id IN ({by_customer})"),
            vec![Value::from(CUSTOMER_CODE)],
        ),
        (
            "DELETE FROM biz_customer WHERE code = ?".to_string(),
            vec![Value::from(CUSTOMER_CODE)],
        ),
    ];
    for (sql, params) in statements {
        if let Err(err) = db().execute_with_params(&sql, params).await {
            panic!("清理夹具失败：{err}\nSQL: {sql}");
        }
    }
    // 夹具账号走统一夹具清场（`purge_user` 连 `sys_user_role` 一起收，且只按自己的用户名删）
    test_seed::purge_user(SALES_USER_ACTIVE).await;
    test_seed::purge_user(SALES_USER_INACTIVE).await;
}

/// 夹具客户（ACTIVE）—— 主数据走 `test_seed`（P2-62：测试里的主数据 INSERT 只许降不许升）。
async fn fixture_customer() -> Uuid {
    test_seed::ensure_customer(CUSTOMER_CODE, "M3-08 转单夹具客户").await
}

/// 夹具销售员：一个启用、一个停用（`active_salesperson` 的 `status = 1` 口径）。
async fn fixture_salespersons() -> (Uuid, Uuid) {
    // 统一夹具（P2-62）：`ensure_user` 幂等建/复活账号，且固定 `status = 1`
    let active = test_seed::ensure_user(SALES_USER_ACTIVE, "M3-08 销售员 启用", None).await;
    let inactive = test_seed::ensure_user(SALES_USER_INACTIVE, "M3-08 销售员 停用", None).await;
    // 停用那个按 `active_salesperson` 的口径（`status = 1` 才算可接单）改成 0
    db().execute_with_params(
        "UPDATE sys_user SET status = 0 WHERE id = ?",
        vec![Value::from(&inactive)],
    )
    .await
    .expect("停用夹具销售员应成功");
    (active, inactive)
}

fn new_line(material: Uuid, qty: &str, price: &str, rate: &str) -> model::NewLine {
    let (qty, price, tax_rate) = (dec(qty), dec(price), dec(rate));
    let (amount, tax_amount) = model::line_amounts(qty, price, tax_rate);
    model::NewLine {
        material_id: material,
        qty,
        price,
        tax_rate,
        amount,
        tax_amount,
        remark: None,
    }
}

/// 建一张报价单并推到 `status`（走 `model::create_quotation` + 合法边 CAS，不绕过状态机）。
async fn quotation_in_status(customer: Uuid, status: &str, lines: &[model::NewLine]) -> (Uuid, String) {
    let today = model::today_cst(Utc::now());
    let totals = model::Totals::of(lines);
    let tx = db().begin().await.expect("开事务");
    let (id, quote_no) = model::create_quotation(
        &tx,
        &model::NewQuotation {
            customer_id: customer,
            quote_date: today,
            valid_until: today + Duration::days(14),
            remark: Some("M3-08 转单夹具"),
        },
        lines,
        &totals,
        state_machine::Operator::of(None),
        Utc::now(),
    )
    .await
    .expect("建报价单应成功");
    let path: &[&str] = match status {
        model::STATUS_DRAFT => &[],
        model::STATUS_SENT => &[model::STATUS_SENT],
        model::STATUS_ACCEPTED => &[model::STATUS_SENT, model::STATUS_ACCEPTED],
        other => panic!("夹具不支持的中间状态：{other}"),
    };
    let mut from = model::STATUS_DRAFT;
    for target in path {
        let affected = model::apply_status(&tx, id, from, target, None, Utc::now())
            .await
            .expect("状态流转应成功");
        assert_eq!(affected, 1, "CAS 应命中（{from} → {target}）");
        from = target;
    }
    tx.commit().await.expect("提交");
    (id, quote_no)
}

async fn order_count(customer: Uuid) -> i64 {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_sales_order WHERE customer_id = ? AND deleted_at IS NULL",
        vec![Value::from(&customer)],
    )
    .await
    .expect("统计订单数应成功")
}

/// 报价单明细快照（`(行号, 物料, 数量, 单价, 金额, 税率, 税额)`）—— 用来证明转单**只读**报价明细。
async fn quotation_line_snapshot(id: Uuid) -> Vec<(i32, Uuid, Decimal, Decimal, Decimal, Decimal, Decimal)> {
    model::lines_of(id)
        .await
        .expect("查报价明细")
        .into_iter()
        .map(|line| {
            (
                line.line_no,
                line.material_id,
                line.qty,
                line.price,
                line.amount,
                line.tax_rate,
                line.tax_amount,
            )
        })
        .collect()
}

async fn so_seq() -> i64 {
    db().scalar_sql(
        "SELECT current_no FROM biz_seq WHERE seq_type = 'SO' AND deleted_at IS NULL".to_string(),
        vec![],
    )
    .await
    .expect("读 SO 号段")
}

/// 卡面点名的「部分转单」：**端到端**证明它被拒、且拒得干净。
///
/// 顺序与路由一致（`quotation_convert` 先 `parse_convert_body` 再 `convert_quotation`）：
/// 请求体解析失败时**不会**走到建单那一步，所以库里的订单数、回填、报价明细都必须原样。
/// 末尾再补一次空 body 整单转：证明「被拒」没有把这张单卡死（约定 11：不留死结）。
#[tokio::test]
async fn partial_conversion_is_refused_and_quote_stays_fully_convertible() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let lines = vec![
        new_line(Uuid::now_v7(), "10", "9.5", "13"),
        new_line(Uuid::now_v7(), "5", "0", "0"),
        new_line(Uuid::now_v7(), "2", "8.25", "13"),
    ];
    let (id, _no) = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;
    let before = quotation_line_snapshot(id).await;
    let seq_before = so_seq().await;

    // 前端/第三方以为能只转前两行
    let err = parse_convert_body(br#"{"line_nos":[1,2]}"#).expect_err("部分转单必须被拒");
    assert_eq!(err.status, 400);
    assert!(err.msg.contains("部分转单"), "{}", err.msg);

    // 被拒之后：零订单、未回填、号段没动、报价明细原样（一次写都没发生）
    assert_eq!(order_count(customer).await, 0, "被拒的转单不许建订单");
    assert_eq!(so_seq().await, seq_before, "被拒的转单不许吃号段");
    let row = model::find_quotation(id)
        .await
        .expect("查报价单")
        .expect("报价单必须存在");
    assert!(row.converted_so_id.is_none(), "被拒的转单不许回填");
    assert_eq!(quotation_line_snapshot(id).await, before, "被拒的转单不许改报价明细");

    // 同一张单用空 body 走整单转 → 三行全部到位（拒绝没有留下「转不了」的状态）
    let res = convert_quotation(None, &id.to_string(), ConvertQuotationReq::default())
        .await
        .expect("整单转单应成功");
    assert_eq!(res.items.len(), 3, "整单转单必须把三行都搬过去");
    assert_eq!(order_count(customer).await, 1);
    for (item, line) in res.items.iter().zip(lines.iter()) {
        assert_eq!(item.material_id, line.material_id.to_string());
        assert_eq!(item.qty, line.qty);
        assert_eq!(item.price, line.price);
    }
}

/// 行号有空洞（1 / 3 / 7）的报价单：订单行仍按 1..n 连续编号，且**每一行的每个字段**都搬对。
///
/// 报价行的 `line_no` 是「订单行 ↔ 报价行」的对应键（V0432 注释），但订单自己要求
/// 1 起连续（`insert_items` 用 `index + 1`）；空洞只能被重新编号，不能被复制过去或跳号。
#[tokio::test]
async fn conversion_renumbers_gapped_line_nos_and_copies_every_field() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let lines = vec![
        new_line(Uuid::now_v7(), "3", "17.655", "13"),
        new_line(Uuid::now_v7(), "2.5", "100", "0"),
        new_line(Uuid::now_v7(), "1", "0", "0"),
    ];
    let (id, _no) = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;

    // 把行号改成 1 / 3 / 7（降序改，避开 `uk_biz_quotation_line_no` 的临时撞号）
    for (from, to) in [(3, 7), (2, 3)] {
        db().execute_with_params(
            "UPDATE biz_quotation_line SET line_no = ? WHERE quotation_id = ?::uuid AND line_no = ?",
            vec![Value::from(to), Value::from(&id), Value::from(from)],
        )
        .await
        .expect("改行号应成功");
    }
    let gapped: Vec<i32> = model::lines_of(id)
        .await
        .expect("查报价明细")
        .iter()
        .map(|line| line.line_no)
        .collect();
    assert_eq!(gapped, vec![1, 3, 7], "前提：报价行号有空洞");

    let res = convert_quotation(None, &id.to_string(), ConvertQuotationReq::default())
        .await
        .expect("转单应成功");
    let so_id = Uuid::parse_str(&res.header.so_id).expect("订单 id 是 uuid");
    let stored = so_model::items_of(so_id).await.expect("查订单明细");
    assert_eq!(stored.len(), 3);
    let stored_nos: Vec<i32> = stored.iter().map(|item| item.line_no).collect();
    assert_eq!(
        stored_nos,
        vec![1, 2, 3],
        "订单行号必须 1..n 连续（不继承报价行的空洞）"
    );
    for (item, line) in stored.iter().zip(lines.iter()) {
        assert_eq!(item.material_id, line.material_id);
        assert_eq!(item.qty, line.qty);
        assert_eq!(item.price, line.price);
        assert_eq!(item.amount, line.amount);
        assert_eq!(item.tax_rate, line.tax_rate);
        assert_eq!(item.tax_amount, line.tax_amount);
        assert_eq!(item.delivered_qty, Decimal::ZERO, "新订单的已发货量必须是 0");
    }
    // 单头三项与报价单同一分钱
    let order = so_model::find_order(so_id)
        .await
        .expect("查订单")
        .expect("订单必须存在");
    let quote = model::find_quotation(id).await.expect("查报价单").expect("报价单在");
    assert_eq!(order.total_amount, quote.total_amount);
    assert_eq!(order.total_tax, quote.total_tax);
    assert_eq!(order.total_with_tax, quote.total_with_tax);
}

/// 事务原子性：**建单之后**任何一步失败 → 整笔回滚（订单、回填、轨迹、号段一样都不许留）。
///
/// 注入点选在最后一步「写 `biz_status_log` 的 CONVERT 轨迹」（用 PG 触发器让它必失败）：
/// 这正是约定 11 说的「提交后失败要有补偿」的镜像 —— 本接口把轨迹也放进同一个事务，
/// 于是根本不存在「提交后」；触发器拆掉后同一张单必须能正常转（不留死结）。
#[tokio::test]
async fn failure_after_order_insert_rolls_back_order_backfill_log_and_seq() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let lines = vec![new_line(Uuid::now_v7(), "2", "30", "13")];
    let (id, quote_no) = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;
    let seq_before = so_seq().await;
    let before = quotation_line_snapshot(id).await;

    // 注入故障：让「写 CONVERT 轨迹」这一步必失败（NOT VALID = 只挡新写入，不扫存量行）
    db().execute_with_params(
        &format!(
            "ALTER TABLE biz_status_log ADD CONSTRAINT {BLOCK_CONSTRAINT} \
             CHECK (action IS DISTINCT FROM '{CONVERT_ACTION}') NOT VALID"
        ),
        vec![],
    )
    .await
    .expect("注入故障约束应成功");

    let err = convert_quotation(None, &id.to_string(), ConvertQuotationReq::default())
        .await
        .expect_err("轨迹写不进去时整笔转单必须失败");
    assert!(
        err.status >= 400,
        "失败要以错误返回（不是 panic 也不是 200）：{} {}",
        err.status,
        err.msg
    );

    // ① 订单一行都不许留 ② 回填不许留 ③ 号段回退 ④ 报价明细原样 ⑤ 轨迹 0 行
    assert_eq!(order_count(customer).await, 0, "整笔回滚：不许留孤订单");
    let row = model::find_quotation(id)
        .await
        .expect("查报价单")
        .expect("报价单必须存在");
    assert!(row.converted_so_id.is_none(), "整笔回滚：不许留半截回链");
    assert!(
        row.converted_at.is_none(),
        "converted_so_id / converted_at 必须成对为空"
    );
    assert_eq!(so_seq().await, seq_before, "整笔回滚：取号必须回退（不留跳号）");
    assert_eq!(quotation_line_snapshot(id).await, before, "整笔回滚：报价明细不许动");
    let logs: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_status_log WHERE entity_type = 'QUOTATION' AND entity_id = ?::uuid AND action = ?",
            vec![Value::from(&id), Value::from(CONVERT_ACTION)],
        )
        .await
        .expect("统计轨迹应成功");
    assert_eq!(logs, 0, "整笔回滚：不许留半条轨迹");

    // 拆掉故障 → 同一张单能正常转（不留「只能改库才能收拾」的中间态）
    drop_block_constraint().await;
    let res = convert_quotation(None, &id.to_string(), ConvertQuotationReq::default())
        .await
        .expect("拆掉故障后应能正常转单");
    assert_eq!(order_count(customer).await, 1);
    assert!(
        res.header.remark.unwrap_or_default().contains(&quote_no),
        "重试成功的订单同样要带来源"
    );
    assert_eq!(so_seq().await, seq_before + 1, "只有真建成的那一笔吃号");
}

/// 拆掉注入故障的约束（失败用例里必须能单独收尾）。
async fn drop_block_constraint() {
    let sql = format!("ALTER TABLE biz_status_log DROP CONSTRAINT IF EXISTS {BLOCK_CONSTRAINT}");
    db().execute_with_params(&sql, vec![])
        .await
        .unwrap_or_else(|err| panic!("拆注入约束失败：{err}"));
}

/// 单头参数落到订单上：`order_date` / `delivery_date`（相等也合法）/ 启用销售员（带名字）。
#[tokio::test]
async fn header_params_land_on_the_order_including_salesperson_name() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let (active, _inactive) = fixture_salespersons().await;
    let lines = vec![new_line(Uuid::now_v7(), "1", "50", "13")];
    let (id, _no) = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;

    let res = convert_quotation(
        None,
        &id.to_string(),
        ConvertQuotationReq {
            order_date: Some("2026-11-17".to_string()),
            // 交期 = 点单日：合法（只禁「早于」）
            delivery_date: Some("2026-11-17".to_string()),
            salesperson_id: Some(active.to_string()),
            remark: Some("款到发货".to_string()),
        },
    )
    .await
    .expect("参数合法的转单应成功");
    assert_eq!(res.header.order_date, "2026-11-17");
    assert_eq!(
        res.header.delivery_date.as_deref(),
        Some("2026-11-17"),
        "同一天交期合法"
    );
    assert_eq!(res.header.salesperson_id.as_deref(), Some(active.to_string().as_str()));
    assert_eq!(
        res.header.salesperson_name.as_deref(),
        Some("M3-08 销售员 启用"),
        "出参要带销售员名字（DTO 里凡是 *_id 都要同时给 *_name）"
    );
    let remark = res.header.remark.clone().unwrap_or_default();
    assert!(remark.starts_with("款到发货｜"), "用户备注作前缀：{remark}");
    assert!(remark.ends_with("转换"), "来源压轴：{remark}");

    // 库里的单头也要对（不是只在出参里拼出来的）
    let so_id = Uuid::parse_str(&res.header.so_id).expect("订单 id 是 uuid");
    let order = so_model::find_order(so_id).await.expect("查订单").expect("订单在");
    assert_eq!(order.order_date, day(2026, 11, 17));
    assert_eq!(order.delivery_date, Some(day(2026, 11, 17)));
    assert_eq!(order.salesperson_id, Some(active));
    assert_eq!(order.contract_id, None, "报价单不挂合同，转单也不许凭空挂上");
    assert_eq!(order.status, so_model::STATUS_DRAFT, "转单只建草稿订单");
    assert_eq!(order.exchange_rate, Decimal::from(1), "本位币单汇率恒 1");
    assert_eq!(order.base_total_with_tax, order.total_with_tax);
}

/// 停用 / 不存在的销售员：**400 且一次写都不发生**（停用账号不该再接新单计佣）。
#[tokio::test]
async fn inactive_or_unknown_salesperson_is_rejected_before_any_write() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let (_active, inactive) = fixture_salespersons().await;
    let lines = vec![new_line(Uuid::now_v7(), "1", "50", "13")];
    let (id, _no) = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;
    let seq_before = so_seq().await;

    for who in [inactive, Uuid::now_v7()] {
        let err = convert_quotation(
            None,
            &id.to_string(),
            ConvertQuotationReq {
                salesperson_id: Some(who.to_string()),
                ..ConvertQuotationReq::default()
            },
        )
        .await
        .expect_err("停用 / 不存在的销售员必须拒");
        assert_eq!(err.status, 400, "销售员不可用是入参问题：{who}");
        assert!(err.msg.contains("销售员"), "报文要点名销售员：{}", err.msg);
        assert!(err.msg.contains(&who.to_string()), "报文要带具体 id：{}", err.msg);
        assert_eq!(order_count(customer).await, 0, "被拒时不许建订单（{who}）");
        assert_eq!(so_seq().await, seq_before, "被拒时不许吃号（{who}）");
    }
}

/// 已接受但**今天已过期**仍可转 —— 过期约束的是「接受时」，不是「转单时」；
/// 出参 `meta.quotation_expired` 要如实标 `true`（前端据此提示，但不拦）。
#[tokio::test]
async fn expired_accepted_quote_still_converts_and_flags_expired() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let lines = vec![new_line(Uuid::now_v7(), "4", "25", "13")];
    let (id, _no) = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;

    // 把报价日 / 有效期一起推到过去（`ck_biz_quotation_valid_until` 要求成对推）
    let yesterday = model::today_cst(Utc::now()) - Duration::days(1);
    let quote_day = yesterday - Duration::days(14);
    db().execute_with_params(
        "UPDATE biz_quotation SET quote_date = ?::text::date, valid_until = ?::text::date WHERE id = ?::uuid",
        vec![
            Value::Text(quote_day.to_string()),
            Value::Text(yesterday.to_string()),
            Value::from(&id),
        ],
    )
    .await
    .expect("把有效期推到过去应成功");

    let res = convert_quotation(None, &id.to_string(), ConvertQuotationReq::default())
        .await
        .expect("客户在有效期内接受过的报价，即使今天已过期也必须能转");
    assert_eq!(
        res.meta.extra.get("quotation_expired"),
        Some(&serde_json::json!(true)),
        "过期事实要如实标示（不拦，但要让人看见）"
    );
    assert_eq!(
        res.meta.extra.get("quotation_status"),
        Some(&serde_json::json!(model::STATUS_ACCEPTED))
    );
    assert_eq!(res.meta.extra.get("line_count"), Some(&serde_json::json!(1)));
    assert_eq!(res.meta.extra.get("already_converted"), Some(&serde_json::json!(false)));
    assert_eq!(order_count(customer).await, 1);
}

/// 主数据名字：能 join 到的物料带编码/名称，join 不到的（物料已软删 / 不存在）**留空但整单照转**
/// —— 名字缺失是显示问题，不是拦截理由（AGENTS.md 约定 13：主数据 join 实时取名，缺失按缺失处理）。
#[tokio::test]
async fn material_labels_are_filled_when_present_and_absent_without_failing() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let known = test_seed::ensure_material("M308-MAT-01", "M3-08 夹具物料").await;
    let unknown = Uuid::now_v7();
    let lines = vec![new_line(known, "2", "12.5", "13"), new_line(unknown, "1", "7", "0")];
    let (id, _no) = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;

    let res = convert_quotation(None, &id.to_string(), ConvertQuotationReq::default())
        .await
        .expect("物料不在主数据里也必须能转（弱引用）");
    let first = res.items.first().expect("第一行");
    assert_eq!(first.material_code.as_deref(), Some("M308-MAT-01"));
    assert_eq!(first.material_name.as_deref(), Some("M3-08 夹具物料"));
    let second = res.items.get(1).expect("第二行");
    assert_eq!(second.material_id, unknown.to_string());
    assert!(second.material_code.is_none(), "查不到的物料编码留空，不编造");
    assert!(second.material_name.is_none(), "查不到的物料名称留空，不编造");
    // 两行都真的落进了订单（名字缺失不影响搬运）
    let so_id = Uuid::parse_str(&res.header.so_id).expect("订单 id 是 uuid");
    assert_eq!(so_model::items_of(so_id).await.expect("查订单明细").len(), 2);
}

/// 库层反向唯一：`uk_biz_quotation_converted_so` 保证**一张订单只被一张报价单认领**。
///
/// 这不是「接口守卫」能覆盖的：两张报价单都能过状态闸、都能建单，唯一能挡住
/// 「同一张订单被两张报价认领」的就是这条部分唯一索引（一旦漏建，来源追溯直接串味）。
#[tokio::test]
async fn unique_index_prevents_two_quotes_claiming_the_same_order() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let lines = vec![new_line(Uuid::now_v7(), "1", "10", "13")];
    let (first_id, _no) = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;
    let (second_id, _no2) = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;

    let res = convert_quotation(None, &first_id.to_string(), ConvertQuotationReq::default())
        .await
        .expect("第一张应能转");
    let so_id = Uuid::parse_str(&res.header.so_id).expect("订单 id 是 uuid");

    // 绕过接口直接写库（模拟「另一个入口/人工脚本」想认领同一张订单）
    let err = db()
        .execute_with_params(
            "UPDATE biz_quotation SET converted_so_id = ?::uuid, converted_at = now() WHERE id = ?::uuid",
            vec![Value::from(&so_id), Value::from(&second_id)],
        )
        .await
        .expect_err("同一张订单被第二张报价认领必须被唯一索引拒绝");
    match err {
        DbError::UniqueViolation { constraint, .. } => assert_eq!(
            constraint.as_deref(),
            Some("uk_biz_quotation_converted_so"),
            "冲突必须来自那条转单反向唯一索引"
        ),
        other => panic!("期望唯一约束冲突，实际：{other:?}"),
    }
    // 第二张单仍然可转 → 建自己的订单（不是被这条索引堵死不能转）
    let second = convert_quotation(None, &second_id.to_string(), ConvertQuotationReq::default())
        .await
        .expect("第二张报价应能转成自己的订单");
    assert_ne!(second.header.so_id, res.header.so_id, "两张报价单必须各自有订单");
    assert_eq!(order_count(customer).await, 2);
}

/// 幂等重放：回同一张订单、`already_converted = true`、**不吃第二号、不建第二张单**。
///
/// 与 M3-03 / M3-05 的幂等用例的分工：这里额外钉住「号段不重复消耗」与
/// 「幂等路径也要带明细 + meta」（前端拿它直接渲染，不该为空）。
#[tokio::test]
async fn idempotent_replay_returns_the_same_order_without_burning_a_second_number() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let lines = vec![
        new_line(Uuid::now_v7(), "3", "17.655", "13"),
        new_line(Uuid::now_v7(), "1", "0", "0"),
    ];
    let (id, _no) = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;
    let seq_before = so_seq().await;

    let first = convert_quotation(None, &id.to_string(), ConvertQuotationReq::default())
        .await
        .expect("第一次转单应成功");
    assert_eq!(so_seq().await, seq_before + 1, "真建单吃一个号");
    let second = convert_quotation(
        None,
        &id.to_string(),
        // 重放时带上不同参数（用户第二次点了「转单」还改了交期/备注）
        ConvertQuotationReq {
            delivery_date: Some(model::today_cst(Utc::now()).to_string()),
            remark: Some("第二次点".to_string()),
            ..ConvertQuotationReq::default()
        },
    )
    .await
    .expect("第二次点必须幂等成功（不是 409）");

    assert_eq!(second.header.so_id, first.header.so_id, "必须回同一张订单");
    assert_eq!(second.header.so_no, first.header.so_no);
    assert_eq!(so_seq().await, seq_before + 1, "幂等路径不许再吃号");
    assert_eq!(order_count(customer).await, 1, "绝不能建出第二张订单");
    assert_eq!(
        second.meta.extra.get("already_converted"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(second.items.len(), 2, "幂等路径也要带明细（前端直接渲染）");
    assert_eq!(
        second.header.delivery_date, first.header.delivery_date,
        "幂等重放**不改**已建订单的交期（参数只在真建单那次生效）"
    );
    assert_eq!(
        second.header.remark, first.header.remark,
        "幂等重放**不改**已建订单的备注（否则来源备注会被第二次的输入顶掉）"
    );
}

/// 转单**只读**报价单：状态仍是 ACCEPTED、三项合计与明细逐行原样，只多出 `converted_so_id/at`
/// 与一条 `CONVERT` 轨迹；订单侧另有自己的建单轨迹。
#[tokio::test]
async fn conversion_only_backfills_the_link_and_never_rewrites_the_quote() {
    let Some(_guard) = setup().await else { return };
    let customer = fixture_customer().await;
    let lines = vec![
        new_line(Uuid::now_v7(), "2", "10", "13"),
        new_line(Uuid::now_v7(), "1", "30", "6"),
    ];
    let (id, quote_no) = quotation_in_status(customer, model::STATUS_ACCEPTED, &lines).await;
    let before = model::find_quotation(id).await.expect("查报价单").expect("报价单在");
    let before_lines = quotation_line_snapshot(id).await;

    let res = convert_quotation(None, &id.to_string(), ConvertQuotationReq::default())
        .await
        .expect("转单应成功");
    let after = model::find_quotation(id).await.expect("查报价单").expect("报价单在");

    assert_eq!(after.status, model::STATUS_ACCEPTED, "转单不改状态");
    assert_eq!(after.total_amount, before.total_amount, "报价金额一分不许动");
    assert_eq!(after.total_tax, before.total_tax);
    assert_eq!(after.total_with_tax, before.total_with_tax);
    assert_eq!(after.remark, before.remark, "报价备注不许被订单备注覆盖");
    assert_eq!(quotation_line_snapshot(id).await, before_lines, "报价明细逐行原样");
    let so_id = Uuid::parse_str(&res.header.so_id).expect("订单 id 是 uuid");
    assert_eq!(after.converted_so_id, Some(so_id));
    assert!(after.converted_at.is_some(), "回链两列成对");

    // 轨迹：报价单一条 CONVERT（状态 ACCEPTED→ACCEPTED，biz_ref 指向订单号）
    let convert_logs: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_status_log \
              WHERE entity_type = 'QUOTATION' AND entity_id = ?::uuid AND action = ?",
            vec![Value::from(&id), Value::from(CONVERT_ACTION)],
        )
        .await
        .expect("统计转单轨迹");
    assert_eq!(convert_logs, 1, "一次转单恰好一条 CONVERT 轨迹");
    let biz_ref: Option<String> = db()
        .scalar_with_params(
            "SELECT biz_ref_no FROM biz_status_log \
              WHERE entity_type = 'QUOTATION' AND entity_id = ?::uuid AND action = ?",
            vec![Value::from(&id), Value::from(CONVERT_ACTION)],
        )
        .await
        .expect("查转单轨迹引用");
    assert_eq!(biz_ref.as_deref(), Some(res.header.so_no.as_str()), "轨迹要指向订单号");
    // 订单侧自己的建单轨迹（审计要求「订单是怎么来的」两头都能查）
    // ⚠ entity_type 是 `SALE_ORDER`（`DocKind::SaleOrder.as_str()`），**不是** `SALES_ORDER` ——
    // 写错这一处，断言就永远绿（0 行也算过），孤儿日志也收不走。
    let so_logs: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_status_log WHERE entity_type = 'SALE_ORDER' AND entity_id = ?::uuid",
            vec![Value::from(&so_id)],
        )
        .await
        .expect("统计订单轨迹");
    assert!(so_logs >= 1, "订单建单轨迹必须留痕（否则订单不知道自己是转来的）");
    assert!(quote_no.starts_with("QT-"), "夹具单号形态：{quote_no}");
}

// ============================================================================
// 四、清场（`zz_` 前缀 = 本组最后一条）
// ============================================================================

/// 跑完不留夹具：客户 / 账号 / 报价单 / 销售订单 / 故障触发器全清，销售链孤儿状态日志收干。
///
/// `cleanup()` 在每条用例**开始时**跑，最后一条用例的夹具会留在库里 —— `zz_` 排序最后，
/// 正好收尾（演练脚本还会在库外再断言一遍，双保险）。
#[tokio::test]
async fn zz_teardown_purges_fixtures_and_status_log_orphans() {
    let Some(_guard) = setup().await else { return };
    for (label, sql) in [
        (
            "夹具客户",
            format!("SELECT count(*) FROM biz_customer WHERE code = '{CUSTOMER_CODE}'"),
        ),
        (
            "夹具账号",
            format!("SELECT count(*) FROM sys_user WHERE username IN ('{SALES_USER_ACTIVE}', '{SALES_USER_INACTIVE}')"),
        ),
        (
            "本组报价单",
            format!(
                "SELECT count(*) FROM biz_quotation q JOIN biz_customer c ON c.id = q.customer_id \
                  WHERE c.code = '{CUSTOMER_CODE}'"
            ),
        ),
        (
            "本组销售订单",
            format!(
                "SELECT count(*) FROM biz_sales_order o JOIN biz_customer c ON c.id = o.customer_id \
                  WHERE c.code = '{CUSTOMER_CODE}'"
            ),
        ),
        (
            "注入故障的约束",
            format!(
                "SELECT count(*) FROM pg_constraint WHERE conname = '{BLOCK_CONSTRAINT}' \
                  AND conrelid = 'biz_status_log'::regclass"
            ),
        ),
    ] {
        let left: i64 = db()
            .scalar_sql(sql, vec![])
            .await
            .unwrap_or_else(|err| panic!("查{label}残留失败：{err}"));
        assert_eq!(left, 0, "清场后仍有{label}残留：{left} 行");
    }

    let purged = fixture_hygiene::purge_orphans(fixture_hygiene::SALES).await;
    let orphans = fixture_hygiene::orphans(fixture_hygiene::SALES).await;
    assert_eq!(orphans, 0, "收尾后仍有孤儿状态日志：{orphans} 行");
    println!("[M3-08] 清场：夹具 0 残留，销售链孤儿状态日志 0 条（本次顺手收干 {purged} 条）");
}
