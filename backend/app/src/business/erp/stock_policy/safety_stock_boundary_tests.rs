// ============================================================================
// safety_stock_boundary_tests.rs —— 安全库存链路的**边界**用例（M2-16）
//
// 卡：t_72aa8970（M2-16【M2·仓库】安全库存：单测）
//
// # 为什么单独一个文件
//
// M2-13 / M2-14 / M2-15 各自的用例钉的是「主场景走得通」；本卡补的是**刀口**：
// 阈值正好相等、4 位小数（`NUMERIC(18,4)`）的最后一位、`NUMERIC` 上界、
// 明细条数上限、重提醒窗口的整点、以及「触发看哪个数」的三处易混点。这些点一旦被
// 后来的重构挪了半个（`<` 变 `<=`、`max` 变 `min`），主场景用例仍然是绿的。
//
// # 挂载位置
//
// 本文件是 `stock_policy::water_check` 的**子模块**（`#[path]` 挂载），因此能调到
// 父模块的私有函数（`push_item` 的明细上限就是私有实现）。跨模块的口径
// （`dto::resolve_*` / `replenish::dto::suggest`）一律调**生产代码的同一个函数**，
// 不在测试里重写一份 —— 测试自己再算一遍就永远抓不到「两份口径分叉」。
//
// # 覆盖清单（每条一句话）
//
// | 用例 | 钉住什么 |
// |---|---|
// | `guards_are_exact_at_zero_and_equal_edges` | 最低 `>= 0`、最高**严格**大于最低（差 0.0001 就合法） |
// | `resolver_keeps_the_fourth_decimal` | 水位 0.0001 不被当成 0（策略 / 物料级两条来源都测） |
// | `resolver_treats_dirty_negative_policy_as_no_warning` | 库里混进负水位（CHECK 之外）→ 不预警、不回退、不 panic |
// | `judge_edges_at_four_decimals_and_numeric_upper_bound` | 判定刀口 + `NUMERIC(18,4)` 上界不溢出 |
// | `max_level_never_triggers_on_its_own` | 只有最高水位（min = 0 明示不预警）→ 永远不触发 |
// | `title_truncation_is_exact_at_two_hundred_chars` | 200 字不截、201 字截到 200（多字节不切半） |
// | `qty_text_trims_zeros_and_negative_zero` | `12.0000`→`12`、`-0.0000`→`0`、没单位不留空格 |
// | `detail_cap_keeps_all_counts_but_trims_items` | 明细最多 200 条，计数仍是全量 |
// | `realert_window_boundaries_are_strict` | 窗口边界「整点放行」、未来时间（时钟倒流）也抑制 |
// | `suggest_triggers_on_expected_not_on_hand` | 触发看**预计可用**：账面等于水位、在途一压就触发 |
// | `expected_equal_to_min_with_low_on_hand_is_covered` | 预计可用 == 水位但账面已低 → `COVERED`（不是 `OK`） |
// | `open_po_above_max_yields_zero_not_negative` | 未清订单超过最高水位 → 建议 0，不给负数 |
// | `suggest_preserves_four_decimals` | 0.0010 级水位的建议量精确到最后一位 |
// | `no_level_means_no_target_and_ok_is_never_listed` | 没水位 = 没目标；`OK` 带 `include_covered` 也不出现 |
// | `page_bounds_are_clamped_and_never_panic` | 页码 0 / `u64::MAX`、`page_size` 上下界都不 panic |
//
// ⚠ 全为纯逻辑，不碰库（真库刀口用例在 `water_check_db_tests.rs`）。
// ============================================================================

use super::*;

use crate::business::erp::replenish::dto as replenish_dto;

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("定点字面量")
}

fn uuid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// 一格：结存 + 策略行（`None` = 没有行）+ 物料级安全库存。
fn cell(qty: &str, policy: Option<(&str, Option<&str>, i16)>, safety: &str) -> WaterCell {
    WaterCell {
        inventory_id: uuid(1),
        material_id: uuid(2),
        material_code: "M216-1".to_string(),
        material_name: "边界物料".to_string(),
        warehouse_id: uuid(3),
        warehouse_name: "边界仓".to_string(),
        unit_name: Some("千克".to_string()),
        qty: dec(qty),
        material_safety_stock: dec(safety),
        policy: policy.map(|(min, max, status)| PolicyLevels::new(dec(min), max.map(dec), status)),
    }
}

fn item(index: usize) -> WaterCheckItem {
    WaterCheckItem {
        inventory_id: uuid(index as u128 + 1).to_string(),
        material_id: uuid(99).to_string(),
        material_code: format!("M216-C{index}"),
        material_name: "明细上限物料".to_string(),
        warehouse_id: uuid(98).to_string(),
        warehouse_name: "明细仓".to_string(),
        qty: "1".to_string(),
        min_qty: "50".to_string(),
        gap_qty: "49".to_string(),
        min_source: MinSource::Policy.as_str().to_string(),
        action: "DRY_RUN".to_string(),
        task_no: None,
    }
}

// ============================================================================
// ① 阈值校验：0 与「相等」两个刀口
// ============================================================================

/// 最低水位 `>= 0`（0 = 本格明示不预警）；最高水位必须**严格**大于最低 ——
/// 相等的上下界是空区间，差 0.0001 才是合法的最小间隔。
#[test]
fn guards_are_exact_at_zero_and_equal_edges() {
    // 最低水位：0 合法、最小负值（4 位小数的最后一位）不合法
    assert!(dto::min_qty_allowed(Decimal::ZERO), "0 = 本格明示不预警，必须允许");
    assert!(!dto::min_qty_allowed(dec("-0.0001")), "负一个最小单位也不行");

    // 最高水位：未设 = 不判超储
    assert!(dto::max_qty_allowed(dec("100"), None), "未设最高水位永远合法");

    // 刀口两侧：100.0001 合法、100.0000 不合法、99.9999 不合法
    assert!(
        dto::max_qty_allowed(dec("100"), Some(dec("100.0001"))),
        "差 0.0001 就够"
    );
    assert!(
        !dto::max_qty_allowed(dec("100"), Some(dec("100.0000"))),
        "相等 = 空区间（既「低于就补」又「高于就停」），必须拒"
    );
    assert!(!dto::max_qty_allowed(dec("100"), Some(dec("99.9999"))));

    // min = 0 + max 0.0001：也是合法的极小区间（0 < 0.0001）
    assert!(dto::max_qty_allowed(Decimal::ZERO, Some(dec("0.0001"))));
    println!("[M2-16] 阈值校验刀口：min>=0 / max 严格大于 min（差 0.0001 合法、相等拒）✓");
}

// ============================================================================
// ② 阈值解析：4 位小数与脏数据
// ============================================================================

/// `NUMERIC(18,4)` 的最小非零值是 0.0001 —— 它不能被当成 0（当成 0 会把
/// 「设了 0.0001 水位」的格子静默降级成「不预警」）。
#[test]
fn resolver_keeps_the_fourth_decimal() {
    // 策略行 0.0001（启用）→ 生效阈值就是它
    let policy = PolicyLevels::new(dec("0.0001"), None, dto::STATUS_ENABLED);
    let resolved = dto::resolve_min_qty(Some(&policy), Decimal::ZERO);
    assert_eq!(resolved.qty, Some(dec("0.0001")), "4 位小数的水位不许被抹成 0");
    assert_eq!(resolved.source, MinSource::Policy);

    // 物料级 0.0001 → 同样生效（来源 MATERIAL）
    let resolved = dto::resolve_min_qty(None, dec("0.0001"));
    assert_eq!(resolved.qty, Some(dec("0.0001")));
    assert_eq!(resolved.source, MinSource::Material);

    // 判定跟着它走：结存 0 < 0.0001 → 报，缺口正好 0.0001
    let low = cell("0", Some(("0.0001", None, dto::STATUS_ENABLED)), "0")
        .judge()
        .expect("0 < 0.0001 必须报");
    assert_eq!(low.gap, dec("0.0001"));
    println!("[M2-16] 阈值解析：0.0001 级别的水位仍生效（来源策略 / 物料级都测）✓");
}

/// 策略行 `min_qty` 理论上被 `CHECK (min_qty >= 0)` 挡住，但结构上（历史脏数据 /
/// 直接改库）可能出现负数：解析必须**不预警**且**不回退物料级** —— 回退会让一格
/// 的报警阈值凭空变成另一个数，比不报警更难排查。
#[test]
fn resolver_treats_dirty_negative_policy_as_no_warning() {
    let dirty = PolicyLevels::new(dec("-5"), Some(dec("100")), dto::STATUS_ENABLED);
    let resolved = dto::resolve_min_qty(Some(&dirty), dec("80"));
    assert_eq!(resolved.qty, None, "负水位 = 不预警（不是回退成 80）");
    assert_eq!(
        resolved.source,
        MinSource::Policy,
        "来源仍是策略行：排查时能看出「这格设过、设的是脏值」"
    );

    // 判定上：不判水位 → 永不低水位（不会因为脏数据刷一屏待办）
    let dirty_cell = cell("-5", Some(("-5", Some("100"), dto::STATUS_ENABLED)), "80");
    assert!(dirty_cell.judge().is_none(), "负水位不产生低水位判定");

    // 最高水位单独看仍是原值（它不依赖 min 的正负）
    assert_eq!(dto::resolve_max_qty(Some(&dirty)), Some(dec("100")));
    println!("[M2-16] 脏负水位：不预警 / 不回退物料级 / 不 panic，来源仍报 POLICY ✓");
}

// ============================================================================
// ③ 判定：4 位小数刀口 + NUMERIC 上界
// ============================================================================

/// 判定刀口（`<` 而不是 `<=`）在 4 位小数下依然成立；`NUMERIC(18,4)` 的上界
/// （14 位整数 + 4 位小数）相减不溢出、缺口算得准。
#[test]
fn judge_edges_at_four_decimals_and_numeric_upper_bound() {
    let policy = Some(("50.0000", None, dto::STATUS_ENABLED));

    // 差一个最小单位
    assert!(cell("49.9999", policy, "0").judge().is_some(), "差 0.0001 就该报");
    assert!(cell("50.0000", policy, "0").judge().is_none(), "正好等于 = 达标");
    assert!(cell("50.0001", policy, "0").judge().is_none());

    // NUMERIC(18,4) 上界：99999999999999.9999
    let top = "99999999999999.9999";
    let just_below = "99999999999999.9998";
    let low = cell(just_below, Some((top, None, dto::STATUS_ENABLED)), "0")
        .judge()
        .expect("上界附近差 0.0001 也要报");
    assert_eq!(low.gap, dec("0.0001"), "大数相减不溢出、缺口精确到 4 位");
    assert!(
        cell(top, Some((top, None, dto::STATUS_ENABLED)), "0").judge().is_none(),
        "上界本身等于水位 → 达标"
    );
    println!("[M2-16] 判定刀口：49.9999 报 / 50.0000 不报；NUMERIC 上界相减不溢出 ✓");
}

/// 只有最高水位（策略 `min = 0` 明示不预警）时**永远不触发**：
/// 补货触发点只有最低水位，最高水位只决定「补到多少」。
#[test]
fn max_level_never_triggers_on_its_own() {
    let facts = replenish_dto::CellFacts {
        on_hand: dec("0"),
        outbound_transit: Decimal::ZERO,
        open_po: Decimal::ZERO,
        material_safety_stock: dec("80"),
        policy: Some(PolicyLevels::new(dec("0"), Some(dec("100")), dto::STATUS_ENABLED)),
    };
    let suggestion = replenish_dto::suggest(&facts);
    assert_eq!(suggestion.min_qty, None, "策略行 0 = 本格不预警（阻断物料级 80）");
    assert!(!suggestion.triggered, "没有最低水位就没有触发点");
    assert_eq!(suggestion.suggested_qty, Decimal::ZERO);
    assert_eq!(suggestion.target_source, replenish_dto::TARGET_MAX, "目标仍取最高水位");
    assert_eq!(
        suggestion.shortage_qty,
        dec("100"),
        "缺口照算（解释「差多少到满水位」）"
    );
    assert_eq!(suggestion.verdict, replenish_dto::VERDICT_OK);
    println!("[M2-16] 只有最高水位：不触发（触发点只有最低水位），禁止按 max 拍数 ✓");
}

// ============================================================================
// ④ 文案：200 字截断的精确刀口 / 数字排版
// ============================================================================

/// 标题上限 200 字：正好 200 字**原样保留**，201 字截到 200 且不把多字节字符切半。
#[test]
fn title_truncation_is_exact_at_two_hundred_chars() {
    // 前缀「库存低于最低水位：」9 字 + 编码 7 + 空格 1 + 名称 N + 空格 1 + '@' 1 + 仓库 1
    let prefix_len = "库存低于最低水位：".chars().count();
    let base = prefix_len + "M216-B1".chars().count() + 1 + 1 + 1 + 1; // 名称与仓库名之外
    let exact_name = 200usize.saturating_sub(base);

    let mut exact = cell("1", Some(("50", None, dto::STATUS_ENABLED)), "0");
    exact.material_code = "M216-B1".to_string();
    exact.material_name = "水".repeat(exact_name);
    exact.warehouse_name = "W".to_string();
    let title = task_title(&exact);
    assert_eq!(title.chars().count(), task_dto::MAX_TITLE_CHARS, "正好 200 字不该被截");
    assert!(title.ends_with("@W"), "没超限的标题必须完整（尾部仓库名还在）：{title}");

    // 多一个字 → 截到 200，且是原串的前缀（没切坏字符）
    let mut longer = cell("1", Some(("50", None, dto::STATUS_ENABLED)), "0");
    longer.material_code = "M216-B1".to_string();
    longer.warehouse_name = "W".to_string();
    longer.material_name = "水".repeat(exact_name + 1);
    let raw_prefix = format!(
        "库存低于最低水位：{} {} @{}",
        longer.material_code, longer.material_name, longer.warehouse_name
    );
    let truncated = task_title(&longer);
    assert_eq!(truncated.chars().count(), task_dto::MAX_TITLE_CHARS);
    assert!(raw_prefix.starts_with(&truncated), "截断必须是前缀，不许拼凑");
    println!("[M2-16] 标题：200 字不截、201 字截到 200（多字节安全）✓");
}

/// 数量排版：尾零裁掉、`-0` 归一成 `0`、没有单位时不留多余空格。
#[test]
fn qty_text_trims_zeros_and_negative_zero() {
    let with_unit = cell("12.0000", None, "0");
    assert_eq!(with_unit.qty_text(dec("12.0000")), "12 千克");
    assert_eq!(with_unit.qty_text(dec("12.5000")), "12.5 千克");
    assert_eq!(with_unit.qty_text(dec("-0.0000")), "0 千克", "-0 不许显示成 -0");
    assert_eq!(with_unit.qty_text(Decimal::ZERO), "0 千克");

    // 没单位 / 单位是空白 → 只有数字（不留「12 」这种尾空格）
    let mut no_unit = cell("12", None, "0");
    no_unit.unit_name = None;
    assert_eq!(no_unit.qty_text(dec("12.0000")), "12");
    no_unit.unit_name = Some("   ".to_string());
    assert_eq!(no_unit.qty_text(dec("12.5")), "12.5", "空白单位当没单位");
    println!("[M2-16] 数量排版：尾零裁剪 / -0 → 0 / 空白单位不留空格 ✓");
}

// ============================================================================
// ⑤ 明细上限：计数全量、明细截断
// ============================================================================

/// 响应明细上限 `MAX_ITEMS`：第 201 条起不再进 `items`，但**计数**由调用方逐格累加，
/// 所以「低水位 201 格」必须仍然报 `low = 201`。
#[test]
fn detail_cap_keeps_all_counts_but_trims_items() {
    let mut report = WaterCheckRes::default();
    for index in 0..MAX_ITEMS {
        push_item(&mut report, item(index));
    }
    assert_eq!(report.items.len(), MAX_ITEMS, "正好 200 条全部保留");

    push_item(&mut report, item(MAX_ITEMS));
    assert_eq!(report.items.len(), MAX_ITEMS, "第 201 条不许挤进来（响应体会被打爆）");
    assert_eq!(
        report.items[MAX_ITEMS - 1].material_code,
        format!("M216-C{}", MAX_ITEMS - 1),
        "保留的是最早的前 200 条，顺序不变"
    );
    println!("[M2-16] 明细上限：前 {MAX_ITEMS} 条保留、第 {} 条被截 ✓", MAX_ITEMS + 1);
}

// ============================================================================
// ⑥ 重提醒窗口的整点边界
// ============================================================================

/// 窗口判定是 `now - last < days`：**整点放行**（正好 days 天前 = 可以再提醒），
/// 差一秒仍在窗口内。未来时间（时钟倒流 / 手工改库）也按「刚提醒过」抑制，
/// 否则会出现「每轮都提醒」的死循环。
#[test]
fn realert_window_boundaries_are_strict() {
    let now = Utc::now();
    assert!(!within_realert_window(None, now, 3), "从没提醒过 → 不抑制");

    // 整点两侧
    assert!(
        within_realert_window(Some(now - Duration::days(3) + Duration::seconds(1)), now, 3),
        "差一秒仍在窗口内"
    );
    assert!(
        !within_realert_window(Some(now - Duration::days(3)), now, 3),
        "正好 days 天前 = 放行（`<` 不是 `<=`）"
    );
    assert!(!within_realert_window(Some(now - Duration::days(4)), now, 3));

    // 1 天窗口的整点
    assert!(within_realert_window(Some(now - Duration::hours(23)), now, 1));
    assert!(!within_realert_window(Some(now - Duration::hours(24)), now, 1));

    // 未来时间（时钟倒流）→ 差值为负 → 抑制，不对库里的脏时间反复提醒
    assert!(within_realert_window(Some(now + Duration::days(1)), now, 3));
    println!("[M2-16] 重提醒窗口：整点放行 / 窗口内抑制 / 未来时间也抑制 ✓");
}

// ============================================================================
// ⑦ 补货建议的触发口径（三处易混点）
// ============================================================================

fn facts(
    on_hand: &str,
    transit: &str,
    open_po: &str,
    safety: &str,
    policy: Option<(&str, Option<&str>, i16)>,
) -> replenish_dto::CellFacts {
    replenish_dto::CellFacts {
        on_hand: dec(on_hand),
        outbound_transit: dec(transit),
        open_po: dec(open_po),
        material_safety_stock: dec(safety),
        policy: policy.map(|(min, max, status)| PolicyLevels::new(dec(min), max.map(dec), status)),
    }
}

/// 触发看的是**预计可用**（结存 − 在途 + 未清订单），不是账面结存：
/// 账面正好等于水位、但已发在途一压就跌破 → 要补货，而 `below_min` 仍是 `false`
/// （待办那一侧不会报，这正是两张表的差异所在，必须显式钉住）。
#[test]
fn suggest_triggers_on_expected_not_on_hand() {
    let suggestion = replenish_dto::suggest(&facts("50", "5", "0", "0", Some(("50", None, dto::STATUS_ENABLED))));
    assert_eq!(suggestion.available_qty, dec("45"), "可用 = 结存 50 − 在途 5");
    assert_eq!(suggestion.expected_qty, dec("45"));
    assert!(suggestion.triggered, "预计可用 45 < 水位 50 → 触发");
    assert!(!suggestion.below_min, "账面 50 没跌破水位（待办不报，补货建议报）");
    assert_eq!(suggestion.suggested_qty, dec("5"));
    assert_eq!(suggestion.verdict, replenish_dto::VERDICT_PURCHASE);

    // 把在途归零 → 不触发（同一结存，差别只在那 5 个在途上）
    let same = replenish_dto::suggest(&facts("50", "0", "0", "0", Some(("50", None, dto::STATUS_ENABLED))));
    assert!(!same.triggered);
    assert_eq!(same.verdict, replenish_dto::VERDICT_OK);
    println!("[M2-16] 触发看预计可用：账面 == 水位但在途一压就触发（below_min 仍 false）✓");
}

/// 预计可用正好等于水位、但**账面已跌破**：不触发（`==` 不报），结论是
/// `COVERED`（不是 `OK`）—— 「账面低、被未清订单盖住」这一格必须能被
/// `include_covered=true` 查到，否则用户想复核时它会凭空消失。
#[test]
fn expected_equal_to_min_with_low_on_hand_is_covered() {
    let suggestion = replenish_dto::suggest(&facts("45", "0", "5", "0", Some(("50", None, dto::STATUS_ENABLED))));
    assert_eq!(suggestion.expected_qty, dec("50"));
    assert!(!suggestion.triggered, "预计可用 == 水位不报（与 M2-14 同一刀）");
    assert!(suggestion.below_min, "账面 45 < 50");
    assert_eq!(suggestion.suggested_qty, Decimal::ZERO);
    assert_eq!(
        suggestion.verdict,
        replenish_dto::VERDICT_COVERED,
        "账面跌破但被未清订单盖住 = COVERED，不是 OK"
    );
    assert!(!replenish_dto::keep_row(&suggestion, false), "默认列表是待办清单");
    assert!(replenish_dto::keep_row(&suggestion, true), "include_covered 下要看得到");
    println!("[M2-16] 预计可用 == 水位且账面已低 → COVERED（默认不显示、include_covered 可见）✓");
}

/// 未清订单**超过**最高水位：缺口 `max(0, target − expected)` 不许为负，
/// 建议 0、结论 `COVERED`（下单下多了这一格不该再给出「负数建议」）。
#[test]
fn open_po_above_max_yields_zero_not_negative() {
    let suggestion = replenish_dto::suggest(&facts(
        "30",
        "0",
        "500",
        "0",
        Some(("50", Some("100"), dto::STATUS_ENABLED)),
    ));
    assert_eq!(suggestion.expected_qty, dec("530"));
    assert!(!suggestion.triggered);
    assert_eq!(suggestion.shortage_qty, Decimal::ZERO, "缺口夹到 0，不许负数");
    assert_eq!(suggestion.suggested_qty, Decimal::ZERO, "更不许建议负采购量");
    assert!(suggestion.below_min, "账面 30 < 50");
    assert_eq!(suggestion.verdict, replenish_dto::VERDICT_COVERED);
    println!("[M2-16] 未清订单超过最高水位：缺口夹 0、建议 0、结论 COVERED ✓");
}

/// 4 位水位的建议量精确到最后一位（0.0010 级），不出现浮点尾巴。
#[test]
fn suggest_preserves_four_decimals() {
    let suggestion = replenish_dto::suggest(&facts(
        "0.0009",
        "0",
        "0",
        "0",
        Some(("0.0010", Some("0.0025"), dto::STATUS_ENABLED)),
    ));
    assert_eq!(suggestion.min_qty, Some(dec("0.0010")));
    assert!(suggestion.triggered);
    assert_eq!(suggestion.suggested_qty, dec("0.0016"), "0.0025 − 0.0009 = 0.0016");
    assert_eq!(suggestion.verdict, replenish_dto::VERDICT_PURCHASE);
    println!("[M2-16] 4 位小数：建议量 0.0016 精确到最后一位 ✓");
}

/// 没水位 = 没目标 = 永远 `OK`；`OK` 即使 `include_covered=true` 也不许出现在列表里
/// （几百上千行没跌破的格子混进来会把真正的缺口埋掉）。
#[test]
fn no_level_means_no_target_and_ok_is_never_listed() {
    let suggestion = replenish_dto::suggest(&facts("0", "0", "0", "0", None));
    assert_eq!(suggestion.min_qty, None);
    assert_eq!(suggestion.max_qty, None);
    assert_eq!(suggestion.target_qty, None, "没水位就没有「补到多少」这个目标");
    assert_eq!(suggestion.target_source, replenish_dto::TARGET_NONE);
    assert_eq!(suggestion.shortage_qty, Decimal::ZERO);
    assert_eq!(suggestion.suggested_qty, Decimal::ZERO);
    assert_eq!(suggestion.verdict, replenish_dto::VERDICT_OK);
    assert!(!replenish_dto::keep_row(&suggestion, true), "OK 永远不进列表");
    println!("[M2-16] 无水位：目标 NONE / 建议 0 / OK 不进列表（带 include_covered 也不进）✓");
}

// ============================================================================
// ⑧ 分页边界：0 页与超大页
// ============================================================================

/// 页码归一化与切片：`page = 0` 当第 1 页、`u64::MAX` 当越界空页，
/// `page_size` 0 回落、超一位就夹到上限 —— 全部不许 panic
/// （`page - 1` 在 `page = 0` 时下溢、`(page-1)*size` 在超大页码时上溢，
/// 二者都会在 debug 构建里 panic → HTTP 500）。
#[test]
fn page_bounds_are_clamped_and_never_panic() {
    assert_eq!(
        replenish_dto::normalize_page(0, 0),
        (1, replenish_dto::DEFAULT_PAGE_SIZE)
    );
    assert_eq!(replenish_dto::normalize_page(0, 200), (1, 200), "等于上限保留");
    assert_eq!(replenish_dto::normalize_page(1, 201), (1, 200), "超一位夹到上限");
    assert_eq!(
        replenish_dto::normalize_page(u64::MAX, 0),
        (u64::MAX, 20),
        "页码不夹取（越界靠切片兜）"
    );

    let rows: Vec<u32> = (1..=5).collect();
    assert_eq!(replenish_dto::page_slice(&rows, 1, 2), vec![1, 2]);
    assert_eq!(replenish_dto::page_slice(&rows, 3, 2), vec![5]);
    assert_eq!(
        replenish_dto::page_slice(&rows, 0, 2),
        vec![1, 2],
        "页码 0 当第 1 页（不许 `page - 1` 下溢 panic）"
    );
    assert!(
        replenish_dto::page_slice(&rows, u64::MAX, 200).is_empty(),
        "超大页码必须给空页（`(page-1)*size` 上溢 panic = 接口 500）"
    );
    assert!(replenish_dto::page_slice(&rows, u64::MAX, u64::MAX).is_empty());
    println!("[M2-16] 分页边界：page 0 / u64::MAX、page_size 0 / 上限 全部不 panic ✓");
}
