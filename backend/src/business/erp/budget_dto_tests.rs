//! `budget_dto` 的纯函数单测：编制 / 调整的入参规则（卡面「编制预算：正常 / 金额为负 /
//! 期间重复」三条负向用例都在这里，不连库）。

use super::*;
use std::str::FromStr;

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("金额字面量非法")
}

fn months(amounts: &[&str]) -> Vec<PeriodReq> {
    amounts
        .iter()
        .enumerate()
        .map(|(index, amount)| PeriodReq {
            period_code: None,
            month: Some(i16::try_from(index + 1).expect("月份")),
            amount: Some(dec(amount)),
            remark: None,
        })
        .collect()
}

/// ① 编制正常：12 行齐全且 Σ = 年度额度 → 通过，且月份/期间码正确。
#[test]
fn create_budget_with_full_periods_passes() {
    let periods = months(&[
        "1000", "1000", "1000", "1000", "1000", "1000", "1000", "1000", "1000", "1000", "1000",
        "1000",
    ]);
    let rows = validate_periods(2026, dec("12000"), Some(&periods)).expect("应通过");
    assert_eq!(rows.len(), 12);
    assert_eq!(rows[0].period_code, "2026-01");
    assert_eq!(rows[11].period_code, "2026-12");
    assert_eq!(rows[11].period_month, 12);
}

/// ② 金额为负：年度额度 / 月度额度任一为负都拒（400 + 10001）。
#[test]
fn negative_amount_is_rejected() {
    let err = validate_periods(2026, dec("-1"), None).expect_err("负额度必须被拒");
    assert_eq!(err.code, error_code::PARAM_INVALID);
    assert!(err.msg.contains("不能为负"), "{}", err.msg);

    let mut periods = months(&["1000"; 12]);
    periods[3].amount = Some(dec("-100"));
    let err = validate_periods(2026, dec("11000"), Some(&periods)).expect_err("负月度额度必须被拒");
    assert!(err.msg.contains("不能为负"), "{}", err.msg);
}

/// ③ 期间重复：同一个月出现两次 → 拒（不是「后来的覆盖前面的」）。
#[test]
fn duplicated_period_is_rejected() {
    let mut periods = months(&["1000"; 12]);
    periods[5].month = Some(3); // 3 月出现两次
    let err = validate_periods(2026, dec("12000"), Some(&periods)).expect_err("期间重复必须被拒");
    assert!(err.msg.contains("期间重复"), "{}", err.msg);
}

/// ④ 缺行 / 多余行：分解必须正好 12 行。
#[test]
fn period_count_must_be_twelve() {
    let err = validate_periods(2026, dec("11000"), Some(&months(&["1000"; 11])))
        .expect_err("11 行必须被拒");
    assert!(err.msg.contains("12 行"), "{}", err.msg);
}

/// ⑤ 合计对不上：Σ ≠ 年度额度 → 拒，报文里给差额。
#[test]
fn period_sum_must_equal_total() {
    let err = validate_periods(2026, dec("11000"), Some(&months(&["1000"; 11])))
        .expect_err("11 行先被行数拦住（缺行同样是错误）");
    assert!(err.msg.contains("12 行"), "{}", err.msg);

    let periods = months(&["1000"; 12]);
    let err = validate_periods(2026, dec("13200"), Some(&periods)).expect_err("合计不等必须被拒");
    assert!(err.msg.contains("≠ 年度额度"), "{}", err.msg);
    assert!(err.msg.contains("1200"), "差额要写在报文里：{}", err.msg);
}

/// ⑥ 期期间码与年度/月份不符：`2026-03` 配 month=4 → 拒。
#[test]
fn period_code_must_match_year_and_month() {
    let mut periods = months(&["1000"; 12]);
    periods[2].period_code = Some("2025-03".to_string());
    let err = validate_periods(2026, dec("12000"), Some(&periods)).expect_err("年度不符必须被拒");
    assert!(err.msg.contains("不符"), "{}", err.msg);
}

/// ⑦ 不给分解 → 平均分摊，尾差记 12 月，合计恒等于年度额度。
#[test]
fn even_split_puts_remainder_on_december() {
    let rows = validate_periods(2026, dec("10000"), None).expect("平均分摊");
    let sum: Decimal = rows.iter().map(|row| row.amount).sum();
    assert_eq!(sum, dec("10000"));
    assert_eq!(rows[0].amount, dec("833.33"));
    assert_eq!(rows[11].amount, dec("833.37"), "尾差 0.04 落到 12 月");
}

/// ⑧ 调整必须写原因，且分解仍要过一遍。
#[test]
fn adjust_requires_reason() {
    let err = validate_adjust(2026, dec("12000"), None, None).expect_err("无原因必须被拒");
    assert!(err.msg.contains("调整原因"), "{}", err.msg);

    let err = validate_adjust(2026, dec("12000"), None, Some("   ")).expect_err("空白原因必须被拒");
    assert!(err.msg.contains("调整原因"), "{}", err.msg);

    let (rows, reason) =
        validate_adjust(2026, dec("12000"), None, Some("业务收缩，砍掉 2 万")).expect("应通过");
    assert_eq!(rows.len(), 12);
    assert_eq!(reason, "业务收缩，砍掉 2 万");
}

/// ⑨ 控制策略：空 = 继承；合法值归一化大写；非法值拒。
#[test]
fn control_mode_validation() {
    assert_eq!(validate_control_mode(None).expect("空"), None);
    assert_eq!(validate_control_mode(Some("  ")).expect("空白"), None);
    assert_eq!(
        validate_control_mode(Some("block")).expect("小写"),
        Some("BLOCK".to_string())
    );
    assert!(validate_control_mode(Some("STRICT")).is_err());
}

/// ⑩ 映射校验：目标类型分两档 —— 有 id 的（MATERIAL / WAREHOUSE）要 ref_id，
///    没有 id 的（MATERIAL_TYPE，物料类型是字符串枚举）要 ref_code；DEFAULT 两个都不要。
///
/// 本用例是 t_ce73ae26 的回归闸：修复前 `MATERIAL_TYPE` 只给 ref_code 会被 400
/// 「必须给 ref_id」，而解析侧只看 ref_code ⇒ 该类型根本没有合法请求体。
#[test]
fn mapping_validation() {
    let base = MappingReq {
        map_type: "DEFAULT".to_string(),
        ref_id: None,
        ref_code: None,
        account_id: Uuid::now_v7().to_string(),
        dept_id: None,
        priority: Some(0),
        status: Some(1),
        remark: None,
    };
    validate_mapping(&base).expect("DEFAULT 合法");

    let mut bad = base.clone();
    bad.ref_id = Some(Uuid::now_v7().to_string());
    assert!(validate_mapping(&bad).is_err(), "DEFAULT 不许给 ref_id");

    let mut bad_code = base.clone();
    bad_code.ref_code = Some("RAW".to_string());
    assert!(
        validate_mapping(&bad_code).is_err(),
        "DEFAULT 不许给 ref_code（兜底不挂目标编码）"
    );

    // ---- MATERIAL_TYPE：ref_code 是唯一目标，ref_id 必须为空 ----
    let mut mt = base.clone();
    mt.map_type = "MATERIAL_TYPE".to_string();
    assert!(
        validate_mapping(&mt).is_err(),
        "MATERIAL_TYPE 缺 ref_code 必须拒绝"
    );

    mt.ref_code = Some("RAW".to_string());
    validate_mapping(&mt).expect("MATERIAL_TYPE 只给 ref_code 必须通过（本卡修的正是这条）");

    mt.ref_code = Some("  raw ".to_string());
    validate_mapping(&mt).expect("物料类型编码容忍空白与大小写");

    mt.ref_code = Some("BOGUS".to_string());
    let err = validate_mapping(&mt).expect_err("值域外的物料类型必须拒绝");
    assert!(
        err.msg.contains("ref_code"),
        "文案要点名 ref_code：{}",
        err.msg
    );

    mt.ref_code = Some("RAW".to_string());
    mt.ref_id = Some(Uuid::now_v7().to_string());
    let err = validate_mapping(&mt).expect_err("MATERIAL_TYPE 不许给 ref_id");
    assert!(err.msg.contains("ref_id"), "文案要点名 ref_id：{}", err.msg);

    // ---- MATERIAL / WAREHOUSE：必须有真实目标 id ----
    let mut material = base.clone();
    material.map_type = "MATERIAL".to_string();
    assert!(
        validate_mapping(&material).is_err(),
        "MATERIAL 必须给 ref_id"
    );

    material.ref_id = Some(Uuid::now_v7().to_string());
    validate_mapping(&material).expect("MATERIAL 合法");

    let mut warehouse = base.clone();
    warehouse.map_type = "WAREHOUSE".to_string();
    assert!(
        validate_mapping(&warehouse).is_err(),
        "WAREHOUSE 必须给 ref_id"
    );
    warehouse.ref_id = Some(Uuid::now_v7().to_string());
    validate_mapping(&warehouse).expect("WAREHOUSE 合法");

    let mut bad_status = base.clone();
    bad_status.status = Some(9);
    assert!(validate_mapping(&bad_status).is_err());

    let mut bad_type = base;
    bad_type.map_type = "SUPPLIER".to_string();
    assert!(validate_mapping(&bad_type).is_err());
}

/// ⑩-b 入库归一化：`MATERIAL_TYPE` 的编码转大写、其余类型只去空白；
///      `MATERIAL_TYPE` / `DEFAULT` 的 `ref_id` 一律丢成 `None`（写路径只走这一处）。
#[test]
fn mapping_target_normalization() {
    assert_eq!(
        normalize_ref_code("MATERIAL_TYPE", Some(" raw ")),
        Some("RAW".to_string())
    );
    assert_eq!(
        normalize_ref_code("MATERIAL", Some(" m-001 ")),
        Some("m-001".to_string()),
        "物料编码大小写由主数据决定，映射不改写"
    );
    assert_eq!(normalize_ref_code("MATERIAL_TYPE", Some("   ")), None);
    assert_eq!(normalize_ref_code("MATERIAL_TYPE", None), None);

    let rid = Uuid::now_v7();
    let (kept, code) = normalize_target("MATERIAL", Some(rid), Some("m-001"));
    assert_eq!(kept, Some(rid), "MATERIAL 必须保留目标 id");
    assert_eq!(code.as_deref(), Some("m-001"));

    let (dropped, code) = normalize_target("MATERIAL_TYPE", Some(rid), Some("fg"));
    assert_eq!(dropped, None, "物料类型没有目标 id，硬塞的必须丢掉");
    assert_eq!(code.as_deref(), Some("FG"), "物料类型编码入库时转大写");

    let (dropped, code) = normalize_target("DEFAULT", Some(rid), Some("x"));
    assert_eq!(dropped, None);
    assert_eq!(code, Some("x".to_string()));
}

/// ⑪ 分页归一化：页码 < 1 归 1，页大小 0 用默认、超限截断。
#[test]
fn page_normalization() {
    assert_eq!(normalize_page(0, 0), (1, DEFAULT_PAGE_SIZE));
    assert_eq!(normalize_page(3, 5000), (3, MAX_PAGE_SIZE));
    assert_eq!(normalize_page(2, 50), (2, 50));
}
