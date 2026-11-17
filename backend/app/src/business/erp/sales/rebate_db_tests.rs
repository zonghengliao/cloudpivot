// ============================================================================
// rebate_db_tests.rs —— 销售返利政策（M3-15）的真库用例
//
// # 覆盖什么（每条都对应卡面 / 迁移里的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `create_by_qty_reads_back_with_rules_and_customer_name` | 按量建单：取号 `BR-yyyyMMdd-NNNNNN`、规则恰好 1 档、客户名 JOIN 回来 |
// | `create_by_amount_uses_percent` | 按额建单：`rate_unit = PERCENT`、比率原样存取 |
// | `tiered_rules_are_stored_sorted_and_pick_tiers` | 阶梯：乱序入参 → 库内 `seq` 1..N 升序；`pick_tier` 从**库读回**的行落档正确 |
// | `overlap_guard_blocks_same_customer_same_period` | 重叠闸：同客户月度的重叠有效期 → 409 + 66090；季度政策并存；不重叠可建；停用的不参与 |
// | `update_replaces_rules_wholesale` | 改政策 = 政策头覆盖 + 规则整表替换（3 档 → 2 档，不残留旧第 3 档） |
// | `update_allows_editing_itself` | 重叠检查排除自己（同一政策改名字/改档位不报 66090） |
// | `update_conflicts_with_another_policy` | 改完之后与**另一份**政策重叠 → 409 + 66090，且原值不被改动 |
// | `frozen_customer_and_missing_rows` | 冻结客户 → 409；客户不存在 → 404；政策不存在 → 404（详情/删除/编辑） |
// | `bad_rule_shape_is_66089_and_writes_nothing` | 规则形状错 → 400 + 66089，库里一行都不留（事务回滚） |
// | `soft_delete_hides_policy_but_keeps_rules` | 软删：列表/详情查不到；规则物理行留着（审计） |
// | `active_policy_of_customer_respects_day_and_status` | M3-16 要用的取政策口径：按日 + 启用 + 有效期 |
// | `concurrent_creates_only_one_wins` | 并发：两个请求同时建「同客户同粒度重叠」政策，**只有一个成功**（advisory lock） |
// | `zz_teardown_purges_fixtures` | 清场：`M315-` 客户的政策 0 条 |
//
// # 为什么夹具直调 `handler::*_of`
//
// 写入路径就是本卡的三条接口（建 / 改 / 删），没有任何「过账」类业务动作可绕 —— 用例直接调
// `handler` 的编排函数（不是测试自己拼的替身），只有想证明「绕过 handler 时库层也拦得住」
// 的少数断言才直插 SQL。主数据一律走 `crate::test_seed::ensure_*`（门禁：测试不许手写主数据 INSERT）。
//
// # 与其它 ERP 用例共用串行锁
//
// `db()` 是进程级全局池；本文件复用 `aging_db_tests::DB_LOCK`（各锁各的等于没锁）。
//
// ⚠ 真库用例必须串行，跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m315_rebate" cargo test --lib \
//   business::erp::sales::rebate -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::sales::rebate_dto::{
    self, RULE_BY_AMOUNT, RULE_BY_QTY, RULE_TIERED, RebatePolicyFilter, RebatePolicyWriteReq, RebateRuleReq,
    STATUS_DISABLED, STATUS_ENABLED, UNIT_PER_UNIT, UNIT_PERCENT, pick_tier,
};
use crate::business::erp::sales::rebate_handler as handler;
use crate::business::erp::sales::rebate_model as model;
use crate::test_seed;
use cloudpivot_kernel::shared::error_code;

/// 夹具客户编码前缀（清场判据）。
const CUS_PREFIX: &str = "M315-";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

fn day(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
}

async fn customer(code: &str) -> Uuid {
    test_seed::ensure_customer(code, &format!("返利夹具客户 {code}")).await
}

/// 一份按量政策的入参（唯一一档平铺）。
fn by_qty_req(customer_id: Uuid, from: &str, to: Option<&str>, rate: &str) -> RebatePolicyWriteReq {
    RebatePolicyWriteReq {
        policy_name: "M315 按量返利".to_string(),
        customer_id: customer_id.to_string(),
        rule_type: RULE_BY_QTY.to_string(),
        rate_unit: None,
        settle_period: "MONTH".to_string(),
        effective_from: from.to_string(),
        effective_to: to.map(str::to_string),
        status: None,
        remark: Some("夹具".to_string()),
        rules: vec![RebateRuleReq {
            tier_from: Some(Decimal::ZERO),
            tier_to: None,
            rate: Some(dec(rate)),
            remark: None,
        }],
    }
}

/// 清场：删本模块造的政策（规则随政策物理删级联走），客户主数据保留给 `test_seed` 复用。
async fn cleanup() {
    exec(
        &format!(
            "DELETE FROM biz_sales_rebate_policy WHERE customer_id IN \
              (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
        ),
        vec![],
    )
    .await;
    exec(
        &format!("UPDATE biz_customer SET status = 'ACTIVE' WHERE code LIKE '{CUS_PREFIX}%'"),
        vec![],
    )
    .await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 该客户在库里的政策条数（含软删：清场/回滚断言要看物理行）。
async fn policy_count(customer_id: Uuid) -> i64 {
    let rows = db()
        .query_sql(
            "SELECT count(*) AS n FROM biz_sales_rebate_policy WHERE customer_id = ?::uuid".to_string(),
            vec![Value::from(&customer_id)],
        )
        .await
        .expect("计数");
    rows.first()
        .map(|row| row.get_typed::<i64>("n"))
        .transpose()
        .expect("读计数")
        .unwrap_or(0)
}

/// 该政策在库里的规则行数（物理行）。
async fn rule_count(policy_id: Uuid) -> i64 {
    let rows = db()
        .query_sql(
            "SELECT count(*) AS n FROM biz_sales_rebate_rule WHERE policy_id = ?::uuid".to_string(),
            vec![Value::from(&policy_id)],
        )
        .await
        .expect("计数");
    rows.first()
        .map(|row| row.get_typed::<i64>("n"))
        .transpose()
        .expect("读计数")
        .unwrap_or(0)
}

// ==================== 用例 ====================

/// 按量建单：取号 + 规则 1 档 + 客户名 JOIN + 详情与列表一致。
#[tokio::test]
async fn create_by_qty_reads_back_with_rules_and_customer_name() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C1").await;

    let created = handler::create_policy_of(None, by_qty_req(customer_id, "2026-01-01", Some("2026-12-31"), "0.5"))
        .await
        .expect("按量建单");

    // 取号：BR-yyyyMMdd-NNNNNN，且能解析
    let (period, no) = model::parse_policy_no(&created.policy_no).expect("单号形状");
    assert_eq!(period.len(), 8, "周期键应是 yyyyMMdd：{}", created.policy_no);
    assert!(no >= 1, "流水从 1 起：{no}");
    assert_eq!(created.rule_type, RULE_BY_QTY);
    assert_eq!(created.rate_unit, UNIT_PER_UNIT, "按量默认元/件");
    assert_eq!(created.settle_period, "MONTH");
    assert_eq!(created.status, STATUS_ENABLED);
    assert_eq!(created.rules.len(), 1, "按量恰好一档");
    assert_eq!(created.rules[0].rate, dec("0.5"));
    assert_eq!(created.rules[0].tier_from, Decimal::ZERO);
    assert_eq!(created.rules[0].tier_to, None);
    assert_eq!(
        created.customer_code.as_deref(),
        Some("M315-C1"),
        "客户编码要 JOIN 回来"
    );
    assert!(created.customer_name.is_some(), "客户名称要 JOIN 回来");

    // 详情 = 列表行（同一条装配路径）
    let detail = handler::detail_policy_of(&created.id).await.expect("详情");
    assert_eq!(detail.policy_no, created.policy_no);
    assert_eq!(detail.rules.len(), 1);

    let page = handler::page_policies_of(RebatePolicyFilter {
        customer_id: Some(customer_id.to_string()),
        ..Default::default()
    })
    .await
    .expect("分页");
    assert_eq!(page.total, 1);
    assert_eq!(page.list[0].id, created.id);
    assert_eq!(page.list[0].rules.len(), 1, "列表也要带规则（一次批量查，不 N+1）");

    println!("[M3-15] 按量：{} / 1 档 / 客户名 JOIN / 详情=列表 ✓", created.policy_no);
}

/// 按额建单：`PERCENT`，比率原样存取。
#[tokio::test]
async fn create_by_amount_uses_percent() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C2").await;

    let req = RebatePolicyWriteReq {
        policy_name: "M315 按额返利".to_string(),
        customer_id: customer_id.to_string(),
        rule_type: RULE_BY_AMOUNT.to_string(),
        rate_unit: None,
        settle_period: "QUARTER".to_string(),
        effective_from: "2026-01-01".to_string(),
        effective_to: None,
        status: None,
        remark: None,
        rules: vec![RebateRuleReq {
            tier_from: Some(Decimal::ZERO),
            tier_to: None,
            rate: Some(dec("2.5")),
            remark: None,
        }],
    };
    let created = handler::create_policy_of(None, req).await.expect("按额建单");
    assert_eq!(created.rule_type, RULE_BY_AMOUNT);
    assert_eq!(created.rate_unit, UNIT_PERCENT);
    assert_eq!(created.settle_period, "QUARTER");
    assert_eq!(created.effective_to, None, "空失效日 = 长期有效");
    assert_eq!(created.rules[0].rate, dec("2.5"));
    println!("[M3-15] 按额：{} / PERCENT 2.5 / 长期有效 ✓", created.policy_no);
}

/// 阶梯：乱序入参 → 库内按档位升序编号；从库读回的行落档正确。
#[tokio::test]
async fn tiered_rules_are_stored_sorted_and_pick_tiers() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C3").await;

    let req = RebatePolicyWriteReq {
        policy_name: "M315 阶梯返利".to_string(),
        customer_id: customer_id.to_string(),
        rule_type: RULE_TIERED.to_string(),
        rate_unit: Some(UNIT_PER_UNIT.to_string()),
        settle_period: "MONTH".to_string(),
        effective_from: "2026-01-01".to_string(),
        effective_to: Some("2026-12-31".to_string()),
        status: None,
        remark: None,
        // 故意乱序：500+ 在前、0~100 在最后
        rules: vec![
            RebateRuleReq {
                tier_from: Some(dec("500")),
                tier_to: None,
                rate: Some(dec("2")),
                remark: Some("大客户档".to_string()),
            },
            RebateRuleReq {
                tier_from: Some(dec("100")),
                tier_to: Some(dec("500")),
                rate: Some(dec("1")),
                remark: None,
            },
            RebateRuleReq {
                tier_from: Some(dec("0")),
                tier_to: Some(dec("100")),
                rate: Some(dec("0.5")),
                remark: None,
            },
        ],
    };
    let created = handler::create_policy_of(None, req).await.expect("阶梯建单");
    assert_eq!(created.rules.len(), 3);
    assert_eq!(
        created.rules.iter().map(|rule| rule.seq).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(created.rules[0].tier_from, Decimal::ZERO);
    assert_eq!(created.rules[2].tier_to, None, "末档开口");
    assert_eq!(created.rules[2].remark.as_deref(), Some("大客户档"), "备注跟着档位走");

    // 从**库读回**的行（不是入参）做落档：钉住「库里存的就是能算的那个形状」
    let rows = model::rules_of(Uuid::parse_str(&created.id).expect("uuid"))
        .await
        .expect("读规则");
    let writes: Vec<rebate_dto::RuleWrite> = rows.iter().map(|row| row.to_write()).collect();
    assert_eq!(pick_tier(&writes, dec("0")).expect("0").seq, 1);
    assert_eq!(pick_tier(&writes, dec("100")).expect("100").seq, 2, "边界归下一档");
    assert_eq!(pick_tier(&writes, dec("500")).expect("500").seq, 3);
    assert_eq!(pick_tier(&writes, dec("999999")).expect("超大").seq, 3);
    println!("[M3-15] 阶梯：乱序入参 → seq 1..3；落档 [0,100)/[100,500)/[500,∞) ✓");
}

/// 重叠闸：同客户同粒度重叠 409 + 66090；季度并存；不重叠可建；停用不参与。
#[tokio::test]
async fn overlap_guard_blocks_same_customer_same_period() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C4").await;

    let first = handler::create_policy_of(None, by_qty_req(customer_id, "2026-01-01", Some("2026-06-30"), "0.5"))
        .await
        .expect("首份政策");

    // ① 同月度、区间相交 → 66090
    let error = handler::create_policy_of(None, by_qty_req(customer_id, "2026-06-01", Some("2026-12-31"), "0.8"))
        .await
        .expect_err("重叠必须被拒");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::ERP_REBATE_POLICY_OVERLAP);
    assert!(
        error.msg.contains(&first.policy_no),
        "文案要点名撞了哪一份：{}",
        error.msg
    );

    // ② 同月度、区间不相交 → 可以建
    let second = handler::create_policy_of(None, by_qty_req(customer_id, "2026-07-01", Some("2026-12-31"), "0.8"))
        .await
        .expect("区间错开可以建");

    // ③ 换成季度粒度 → 与月度政策并存（结算粒度不同）
    let mut quarter = by_qty_req(customer_id, "2026-01-01", Some("2026-12-31"), "1");
    quarter.settle_period = "QUARTER".to_string();
    quarter.policy_name = "M315 季度返利".to_string();
    let quarter_policy = handler::create_policy_of(None, quarter).await.expect("季度政策可并存");

    // ④ 把第二份停用后，它原来的区间可以让给新政策
    let mut disable = by_qty_req(customer_id, "2026-07-01", Some("2026-12-31"), "0.8");
    disable.status = Some(STATUS_DISABLED);
    disable.policy_name = "M315 停用版".to_string();
    let disabled = handler::update_policy_of(None, &second.id, disable.clone())
        .await
        .expect("停用第二份");
    assert_eq!(disabled.status, STATUS_DISABLED);
    disable.status = None;
    let third = handler::create_policy_of(None, disable)
        .await
        .expect("停用的不参与重叠检查");
    assert_ne!(third.id, second.id);
    assert_ne!(third.id, quarter_policy.id);
    println!(
        "[M3-15] 重叠闸：相交 409+66090（点名 {}）/ 错开可建 / 季度并存 / 停用不参与 ✓",
        first.policy_no
    );
}

/// 改政策 = 政策头覆盖 + 规则整表替换（3 档 → 2 档，不残留旧档）。
#[tokio::test]
async fn update_replaces_rules_wholesale() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C5").await;

    let mut req = RebatePolicyWriteReq {
        policy_name: "M315 三档".to_string(),
        customer_id: customer_id.to_string(),
        rule_type: RULE_TIERED.to_string(),
        rate_unit: Some(UNIT_PER_UNIT.to_string()),
        settle_period: "MONTH".to_string(),
        effective_from: "2026-01-01".to_string(),
        effective_to: None,
        status: None,
        remark: None,
        rules: vec![
            RebateRuleReq {
                tier_from: Some(dec("0")),
                tier_to: Some(dec("100")),
                rate: Some(dec("1")),
                remark: None,
            },
            RebateRuleReq {
                tier_from: Some(dec("100")),
                tier_to: Some(dec("500")),
                rate: Some(dec("2")),
                remark: None,
            },
            RebateRuleReq {
                tier_from: Some(dec("500")),
                tier_to: None,
                rate: Some(dec("3")),
                remark: None,
            },
        ],
    };
    let created = handler::create_policy_of(None, req.clone()).await.expect("三档");
    let id = Uuid::parse_str(&created.id).expect("uuid");
    assert_eq!(rule_count(id).await, 3);

    // 改成两档 + 改名
    req.policy_name = "M315 两档（改版）".to_string();
    req.rules = vec![
        RebateRuleReq {
            tier_from: Some(dec("0")),
            tier_to: Some(dec("300")),
            rate: Some(dec("1.5")),
            remark: None,
        },
        RebateRuleReq {
            tier_from: Some(dec("300")),
            tier_to: None,
            rate: Some(dec("4")),
            remark: None,
        },
    ];
    let updated = handler::update_policy_of(None, &created.id, req).await.expect("改两档");
    assert_eq!(updated.policy_name, "M315 两档（改版）");
    assert_eq!(updated.policy_no, created.policy_no, "改政策不换号");
    assert_eq!(updated.rules.len(), 2);
    assert_eq!(
        updated.rules.iter().map(|rule| rule.seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        rule_count(id).await,
        2,
        "旧第 3 档必须被物理删掉（否则两个档位上界相同 → 落档看执行计划）"
    );
    println!("[M3-15] 改政策：3 档 → 2 档，旧档不残留，单号不变 ✓");
}

/// 同一政策改自己不算重叠（排除自己）。
#[tokio::test]
async fn update_allows_editing_itself() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C6").await;
    let created = handler::create_policy_of(None, by_qty_req(customer_id, "2026-01-01", Some("2026-12-31"), "0.5"))
        .await
        .expect("建单");

    // 只改名 + 改比率，有效期原样（会和自己重叠）
    let mut req = by_qty_req(customer_id, "2026-01-01", Some("2026-12-31"), "0.6");
    req.policy_name = "M315 改名后".to_string();
    let updated = handler::update_policy_of(None, &created.id, req)
        .await
        .expect("改自己不能被重叠闸拦");
    assert_eq!(updated.rules[0].rate, dec("0.6"));
    println!("[M3-15] 改自己：有效期不变也不报 66090（exclude_id 生效）✓");
}

/// 改完之后与另一份政策重叠 → 409 + 66090，且原值不被改动。
#[tokio::test]
async fn update_conflicts_with_another_policy() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C7").await;
    let left = handler::create_policy_of(None, by_qty_req(customer_id, "2026-01-01", Some("2026-06-30"), "0.5"))
        .await
        .expect("左政策");
    let right = handler::create_policy_of(None, by_qty_req(customer_id, "2026-07-01", Some("2026-12-31"), "0.8"))
        .await
        .expect("右政策");

    // 把右政策往前伸到 6 月 → 与左政策重叠
    let mut req = by_qty_req(customer_id, "2026-06-01", Some("2026-12-31"), "0.9");
    req.policy_name = "M315 硬碰".to_string();
    let error = handler::update_policy_of(None, &right.id, req)
        .await
        .expect_err("重叠必须拒");
    assert_eq!(error.code, error_code::ERP_REBATE_POLICY_OVERLAP);
    assert!(error.msg.contains(&left.policy_no), "{}", error.msg);

    // 原值不动（事务回滚）
    let current = handler::detail_policy_of(&right.id).await.expect("详情");
    assert_eq!(current.effective_from, day("2026-07-01"));
    assert_eq!(current.rules[0].rate, dec("0.8"));
    println!(
        "[M3-15] 改后重叠：409 + 66090 点名 {}，被拒后原值不变 ✓",
        left.policy_no
    );
}

/// 冻结客户 → 409；客户 / 政策不存在 → 404。
#[tokio::test]
async fn frozen_customer_and_missing_rows() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C8").await;
    let created = handler::create_policy_of(None, by_qty_req(customer_id, "2026-01-01", Some("2026-12-31"), "0.5"))
        .await
        .expect("建单");

    // 客户冻结后不能再建 / 改
    exec(
        "UPDATE biz_customer SET status = 'FROZEN' WHERE id = ?::uuid",
        vec![Value::from(&customer_id)],
    )
    .await;
    let error = handler::create_policy_of(None, by_qty_req(customer_id, "2027-01-01", Some("2027-12-31"), "0.5"))
        .await
        .expect_err("冻结客户必须拒");
    assert_eq!(error.status, 409);
    assert!(error.msg.contains("冻结"), "{}", error.msg);
    let error = handler::update_policy_of(
        None,
        &created.id,
        by_qty_req(customer_id, "2026-01-01", Some("2026-12-31"), "0.7"),
    )
    .await
    .expect_err("冻结客户不能改政策");
    assert_eq!(error.status, 409);
    exec(
        "UPDATE biz_customer SET status = 'ACTIVE' WHERE id = ?::uuid",
        vec![Value::from(&customer_id)],
    )
    .await;

    // 客户不存在 / 政策不存在
    let ghost = Uuid::now_v7();
    let error = handler::create_policy_of(None, by_qty_req(ghost, "2026-01-01", None, "0.5"))
        .await
        .expect_err("客户不存在");
    assert_eq!(error.status, 404);
    let error = handler::detail_policy_of(&ghost.to_string())
        .await
        .expect_err("政策不存在");
    assert_eq!(error.status, 404);
    let error = handler::delete_policy_of(None, &ghost.to_string())
        .await
        .expect_err("删不存在的政策");
    assert_eq!(error.status, 404);
    let error = handler::update_policy_of(
        None,
        &ghost.to_string(),
        by_qty_req(customer_id, "2026-01-01", None, "0.5"),
    )
    .await
    .expect_err("改不存在的政策");
    assert_eq!(error.status, 404);
    println!("[M3-15] 冻结客户 409；客户/政策不存在 404（详情/删除/编辑各一条）✓");
}

/// 规则形状错 → 400 + 66089，且一条都不落库。
#[tokio::test]
async fn bad_rule_shape_is_66089_and_writes_nothing() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C9").await;

    // 阶梯只有一档
    let mut one_tier = RebatePolicyWriteReq {
        rule_type: RULE_TIERED.to_string(),
        rate_unit: Some(UNIT_PER_UNIT.to_string()),
        ..by_qty_req(customer_id, "2026-01-01", None, "1")
    };
    one_tier.rules = vec![RebateRuleReq {
        tier_from: Some(Decimal::ZERO),
        tier_to: None,
        rate: Some(dec("1")),
        remark: None,
    }];
    let error = handler::create_policy_of(None, one_tier).await.expect_err("阶梯一档");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID);

    // 中间断档
    let mut gap = RebatePolicyWriteReq {
        rule_type: RULE_TIERED.to_string(),
        rate_unit: Some(UNIT_PERCENT.to_string()),
        ..by_qty_req(customer_id, "2026-01-01", None, "1")
    };
    gap.rules = vec![
        RebateRuleReq {
            tier_from: Some(Decimal::ZERO),
            tier_to: Some(dec("100")),
            rate: Some(dec("1")),
            remark: None,
        },
        RebateRuleReq {
            tier_from: Some(dec("200")),
            tier_to: None,
            rate: Some(dec("2")),
            remark: None,
        },
    ];
    let error = handler::create_policy_of(None, gap).await.expect_err("断档");
    assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID);

    // 按量给多档
    let mut qty_multi = by_qty_req(customer_id, "2026-01-01", None, "0.5");
    qty_multi.rules = vec![
        RebateRuleReq {
            tier_from: Some(Decimal::ZERO),
            tier_to: Some(dec("100")),
            rate: Some(dec("0.5")),
            remark: None,
        },
        RebateRuleReq {
            tier_from: Some(dec("100")),
            tier_to: None,
            rate: Some(dec("1")),
            remark: None,
        },
    ];
    let error = handler::create_policy_of(None, qty_multi).await.expect_err("按量多档");
    assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID);

    assert_eq!(policy_count(customer_id).await, 0, "形状错必须一条都不落库");
    println!("[M3-15] 规则形状错（阶梯一档 / 断档 / 按量多档）→ 400+66089，库里 0 行 ✓");
}

/// 软删：列表 / 详情查不到；规则物理行留着（审计）。
#[tokio::test]
async fn soft_delete_hides_policy_but_keeps_rules() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C10").await;
    let created = handler::create_policy_of(None, by_qty_req(customer_id, "2026-01-01", Some("2026-12-31"), "0.5"))
        .await
        .expect("建单");
    let id = Uuid::parse_str(&created.id).expect("uuid");

    handler::delete_policy_of(None, &created.id).await.expect("软删");
    assert!(model::find_policy(id).await.expect("查询").is_none(), "软删后查不到");
    let error = handler::detail_policy_of(&created.id).await.expect_err("详情 404");
    assert_eq!(error.status, 404);
    let page = handler::page_policies_of(RebatePolicyFilter {
        customer_id: Some(customer_id.to_string()),
        ..Default::default()
    })
    .await
    .expect("分页");
    assert_eq!(page.total, 0, "软删的政策不进列表");
    assert_eq!(rule_count(id).await, 1, "规则物理行留着（审计：当时按什么比率算）");

    // 再删一次 → 404（幂等口径：已删的不再报成功）
    let error = handler::delete_policy_of(None, &created.id).await.expect_err("重复删");
    assert_eq!(error.status, 404);
    println!("[M3-15] 软删：列表/详情 0 条，规则行保留，重复删 404 ✓");
}

/// M3-16 要用的取政策口径：按日 + 启用 + 有效期（停用/未生效都取不到）。
#[tokio::test]
async fn active_policy_of_customer_respects_day_and_status() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C11").await;
    let created = handler::create_policy_of(None, by_qty_req(customer_id, "2026-03-01", Some("2026-09-30"), "0.5"))
        .await
        .expect("建单");

    let hit = model::active_policy_of_customer(customer_id, "MONTH", day("2026-05-01"))
        .await
        .expect("查询")
        .expect("期内应命中");
    assert_eq!(hit.policy_no, created.policy_no);
    assert!(hit.is_active_on(day("2026-05-01")));

    assert!(
        model::active_policy_of_customer(customer_id, "MONTH", day("2026-01-01"))
            .await
            .expect("查询")
            .is_none(),
        "未生效期间取不到"
    );
    assert!(
        model::active_policy_of_customer(customer_id, "QUARTER", day("2026-05-01"))
            .await
            .expect("查询")
            .is_none(),
        "不同结算粒度取不到"
    );

    // 停用后取不到
    let mut disable = by_qty_req(customer_id, "2026-03-01", Some("2026-09-30"), "0.5");
    disable.status = Some(STATUS_DISABLED);
    handler::update_policy_of(None, &created.id, disable)
        .await
        .expect("停用");
    assert!(
        model::active_policy_of_customer(customer_id, "MONTH", day("2026-05-01"))
            .await
            .expect("查询")
            .is_none(),
        "停用后取不到"
    );
    println!("[M3-15] 取政策口径：期内命中 / 未生效 / 粒度不符 / 停用 都取不到 ✓");
}

/// 并发：两个请求同时建「同客户同粒度重叠」政策，只有一个成功（advisory lock）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_only_one_wins() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer("M315-C12").await;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));

    let mut tasks = Vec::new();
    for rate in ["0.5", "0.9"] {
        let barrier = barrier.clone();
        let req = by_qty_req(customer_id, "2026-01-01", Some("2026-12-31"), rate);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            handler::create_policy_of(None, req).await
        }));
    }
    let mut ok = 0;
    let mut conflicts = 0;
    for task in tasks {
        match task.await.expect("任务不 panic") {
            Ok(_) => ok += 1,
            Err(error) => {
                assert_eq!(
                    error.code,
                    error_code::ERP_REBATE_POLICY_OVERLAP,
                    "失败的那一方必须是「重叠」（不是 500 / 唯一键）: {}",
                    error.msg
                );
                conflicts += 1;
            }
        }
    }
    assert_eq!((ok, conflicts), (1, 1), "并发建重叠政策：必须恰好一个成功、一个 66090");
    assert_eq!(policy_count(customer_id).await, 1, "库里只能留一份政策");
    println!("[M3-15] 并发建重叠政策：1 成功 + 1 个 66090，库里 1 份 ✓");
}

/// 清场：跑完不留 `M315-` 客户的政策（规则随政策级联删）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;
    let rows = db()
        .query_sql(
            format!(
                "SELECT count(*) AS n FROM biz_sales_rebate_policy p \
                  JOIN biz_customer c ON c.id = p.customer_id WHERE c.code LIKE '{CUS_PREFIX}%'"
            ),
            vec![],
        )
        .await
        .expect("计数");
    let n: i64 = rows
        .first()
        .map(|row| row.get_typed("n"))
        .transpose()
        .expect("读")
        .unwrap_or(0);
    assert_eq!(n, 0, "清场后 M315- 客户的政策必须为 0");
    println!("[M3-15] 清场：M315- 客户的政策 0 条 ✓");
}
