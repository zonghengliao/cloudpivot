//! 折旧批次联动接线（`DEPRECIATION`，LNK-10）的**真库**用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 SKIP（与本仓其它 ERP 用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//!   cargo test --lib business::erp::depreciation_linkage_tests -- --test-threads=1
//! ```
//!
//! # 覆盖（卡面「过账触发 / 大批量通知只发一次 / 重复过账不重复触发 / 排除动作不跑」逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | `lnk10_posting_triggers_one_aggregate_notice` | 过账（`CALCULATED → POSTED`）触发通知，**多台资产只发一条**（聚合口径） |
//! | 2 | `lnk10_repeat_posting_does_not_trigger_twice` | 同期第二次计提走幂等闸（`Exists`），通知**不增**、凭证不增 |
//! | 3 | `lnk10_reversal_triggers_notice_once` | 红冲（`POSTED → REVERSED`）触发通知一次；重复红冲被 409 挡住，通知不增 |
//! | 4 | `lnk10_rolled_back_tx_never_sends_notice` | **事务回滚 → 通知一条都不发**（deferred 随事务丢），并有「提交后才发」的正向对照 |
//! | 5 | `lnk10_excluded_voucher_actions_never_run` | 批次上配 `GENERATE_VOUCHER`：通用引擎**不执行**它（同状态 `SEND_NOTICE` 照跑），凭证只有 `post_multi_debit` 那一张 |
//!
//! # 夹具口径
//!
//! * 期间：本卡独占年度 **2047**（`period_test_support` 拥有 2033 / 2040，其它模块用 2036 / 2046，
//!   互不重叠），每条用例一个月份（`2047-01` / `-02` / `-03`）；
//! * 卡片走真实 `asset_model::asset_insert`（派生金额 / 约束按生产口径），号段 `FA-LNK10-`；
//! * 批次 / 明细 / 凭证 / 通知**全部按本卡 id 集合或号段删**，不按期间前缀整段删别人的数据；
//! * 串行（`--test-threads=1`）：联动规则 `biz_flow_linkage_rule` 是全表唯一索引，
//!   并行会互删；`DB_LOCK` 同时也把 `period_test_support::cleanup()` 的号段清理串起来。

use std::str::FromStr;

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::asset_dto::{self, CreateAssetReq};
use super::asset_model::{self, AssetInsert};
use super::depreciation_linkage;
use super::depreciation_model;
use crate::business::erp::period_test_support as support;
use crate::business::flow::linkage;
use crate::business::flow::linkage::spec;
use crate::business::shared::state_machine;
use crate::shared::error_code;

/// 本卡独占的夹具年度（见文件头）。
const YEAR: i16 = 2047;
/// 购入日期：起提日 = 2047-01-01 ⇒ `2047-01` 及之后各期都在扫描范围内。
const PURCHASE_DATE: (i32, u32, u32) = (2046, 12, 10);

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("金额字面量非法")
}

fn purchase_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(PURCHASE_DATE.0, PURCHASE_DATE.1, PURCHASE_DATE.2).expect("夹具日期")
}

/// 跑一条写 SQL（失败直接 panic，不静默吞错）。
async fn exec(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 取一列 `id`（清场用：只读，不写）。
async fn uuids_of(query: &str) -> Vec<Uuid> {
    let rows = db()
        .query_sql(query.to_string(), vec![])
        .await
        .unwrap_or_else(|err| panic!("查夹具残留失败：{err}\nSQL: {query}"));
    rows.iter()
        .map(|row| row.get_typed::<Uuid>("id").expect("夹具 id"))
        .collect()
}

fn in_uuids(ids: &[Uuid]) -> String {
    ids.iter()
        .map(|id| format!("'{id}'::uuid"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 清场：联动规则 / 通知 / 批次 + 明细 + 凭证 + 状态日志 + 卡片 + 期间 + 夹具用户。
///
/// 顺序（外键 + 无外键的日志）：通知 → 凭证 → 明细（FK 指向批次与卡片）→ 批次 →
/// 状态日志（无 FK，靠 id 定位，主体删掉后就再也找不到）→ 卡片变动 / 卡片 → 期间 → 用户。
async fn purge() {
    exec("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK10-%'").await;
    exec("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:DEPRECIATION:%'").await;

    let batches = uuids_of(&format!(
        "SELECT id FROM biz_depreciation_batch WHERE period_code LIKE '{YEAR}-%'"
    ))
    .await;
    if !batches.is_empty() {
        let list = in_uuids(&batches);
        let vouchers = uuids_of(&format!(
            "SELECT id FROM biz_voucher WHERE source_type IN ('DEPRECIATION','DEPRECIATION_REVERSAL') \
               AND source_id IN ({list})"
        ))
        .await;
        if !vouchers.is_empty() {
            exec(&format!(
                "DELETE FROM biz_voucher WHERE id IN ({})",
                in_uuids(&vouchers)
            ))
            .await;
        }
        exec(&format!(
            "DELETE FROM biz_depreciation WHERE batch_id IN ({list})"
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_depreciation_batch WHERE id IN ({list})"
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'DEPRECIATION' AND entity_id IN ({list})"
        ))
        .await;
    }

    let assets = uuids_of("SELECT id FROM biz_fixed_asset WHERE asset_no LIKE 'FA-LNK10-%'").await;
    if !assets.is_empty() {
        let list = in_uuids(&assets);
        exec(&format!(
            "DELETE FROM biz_fixed_asset_change WHERE asset_id IN ({list})"
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'FIXED_ASSET' AND entity_id IN ({list})"
        ))
        .await;
        exec(&format!("DELETE FROM biz_fixed_asset WHERE id IN ({list})")).await;
    }

    let periods = uuids_of(&format!(
        "SELECT id FROM biz_accounting_period WHERE period_code LIKE '{YEAR}-%'"
    ))
    .await;
    if !periods.is_empty() {
        let list = in_uuids(&periods);
        exec(&format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' AND entity_id IN ({list})"
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_accounting_period WHERE id IN ({list})"
        ))
        .await;
    }

    // 卡片删干净了才轮得到部门（`biz_fixed_asset.use_dept_id` 是硬外键）
    exec("DELETE FROM sys_dept WHERE dept_code = 'LNK10-DEPT'").await;
    exec("DELETE FROM sys_user WHERE username LIKE 'p2.lnk10%'").await;
}

/// 连库（`None` = 没有测试库 → 用例 SKIP）+ 串行锁 + 清场 + 造操作人。
async fn setup() -> Option<(tokio::sync::MutexGuard<'static, ()>, Uuid)> {
    let guard = support::DB_LOCK.lock().await;
    if !support::ensure_db().await {
        return None;
    }
    purge().await;
    support::cleanup().await;
    let user = support::seed_user("p2.lnk10_maker", "LNK10 折旧会计").await;
    Some((guard, user))
}

/// 开一期（`2047-MM`，OPEN）并返回期间编码。
async fn open_month(month: i16) -> String {
    let row = super::period::open_period(YEAR, month, Some("LNK-10 夹具"), support::operator())
        .await
        .unwrap_or_else(|error| panic!("开账失败（{YEAR}-{month:02}）：{}", error.msg));
    assert_eq!(row.period_code, format!("{YEAR}-{month:02}"), "期间编码");
    row.period_code
}

/// 取一个已存在的部门 id；空库（只有迁移、没有种子部门）时造一个本卡专用的部门。
///
/// 真库夹具库常常是「只跑了迁移」的空库（LNK-09 那台 `cloudpivot_lnk09n` 就没有 sys_dept），
/// 而资产卡片 `use_dept_id` 是硬外键 —— 这里自己造 `LNK10-DEPT` 并在 [`purge`] 里删掉。
async fn any_dept() -> Uuid {
    let rows = db()
        .query_sql(
            "SELECT id FROM sys_dept WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1"
                .to_string(),
            vec![],
        )
        .await
        .expect("查部门");
    if let Some(id) = rows.first().and_then(|row| row.get_typed("id").ok()) {
        return id;
    }
    let rows = db()
        .query_sql(
            "INSERT INTO sys_dept (dept_code, dept_name, status, sort_order) \
             VALUES ('LNK10-DEPT', 'LNK10 夹具部门', 1, 900) RETURNING id"
                .to_string(),
            vec![],
        )
        .await
        .expect("造夹具部门");
    rows.first()
        .and_then(|row| row.get_typed("id").ok())
        .expect("夹具部门 id")
}

/// 取一个启用中的资产分类（V0229 种子）。
async fn any_category() -> asset_model::CategoryRow {
    asset_model::category_active()
        .await
        .expect("查资产分类")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("库里没有启用中的资产分类（先跑 V0229）"))
}

/// 卡片号后缀：取 UUID 的**末 8 位**（前 8 位是 UUIDv7 的时间戳，同一毫秒连造三张会撞
/// `uk_biz_fa_no` —— 实测踩过）。
fn no_tail(id: &Uuid) -> String {
    let digits: String = id.to_string().chars().filter(|c| *c != '-').collect();
    digits
        .get(digits.len().saturating_sub(8)..)
        .unwrap_or("00000000")
        .to_ascii_uppercase()
}

/// 造一张在役资产卡（走真实 `asset_insert`：派生金额 / 约束都按生产口径）。
async fn make_asset(creator: Uuid, name: &str) -> (Uuid, String) {
    let dept = any_dept().await;
    let category = any_category().await;
    let req = CreateAssetReq {
        asset_name: Some(name.to_string()),
        category_id: Some(category.id.to_string()),
        spec: None,
        use_dept_id: Some(dept.to_string()),
        custodian_id: None,
        location: None,
        original_value: Some(dec("100000")),
        tax_amount: Some(Decimal::ZERO),
        purchase_date: Some(purchase_date()),
        source_type: None,
        source_id: None,
        source_no: None,
        buy_mode: None,
        pay_mode: None,
        supplier_id: None,
        remark: Some("LNK-10 真库用例".to_string()),
    };
    let spec = asset_dto::validate_create(&req).expect("夹具建卡请求必须通过校验");
    let id = Uuid::now_v7();
    let asset_no = format!("FA-LNK10-{}", no_tail(&id));
    let (residual, base, monthly) = asset_dto::derive_amounts(
        spec.original_value,
        category.salvage_rate,
        category.useful_life_months,
    );
    asset_model::asset_insert(
        db(),
        id,
        &AssetInsert {
            asset_no: asset_no.clone(),
            spec: asset_dto::CreateAssetSpec {
                use_dept_id: dept,
                ..spec
            },
            category_code: category.category_code.clone(),
            use_dept_name: "夹具部门".to_string(),
            custodian_name: None,
            supplier_name: None,
            residual_value: residual,
            depreciable_base: base,
            monthly_depreciation: monthly,
            useful_life_months: category.useful_life_months,
            salvage_rate: category.salvage_rate,
            start_depreciation_date: super::asset_compute::start_depreciation_date(purchase_date()),
            operator: Some(creator),
        },
    )
    .await
    .expect("建卡");
    (id, asset_no)
}

/// 插一条 `DEPRECIATION/<status>` 上的规则（绕过 API 写库：与「管理员在配置页配好」等价）。
async fn add_rule(status: &str, action: &str, params: &str) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    exec(&format!(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), 'LNK10-{status}-{action}-{nanos}', 'DEPRECIATION', \
                 '{status}', '{action}', '{params}'::jsonb, 10)"
    ))
    .await;
}

/// 规则参数：TAKEOVER + 收件人取制单人（`depreciation_linkage` 把操作人同时当 owner / creator）。
const NOTICE_CREATOR: &str = r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#;

/// 批次的联动通知条数（幂等键 = `LNK:DEPRECIATION:<状态>:<批次 id>:<事件凭证>`）。
async fn notices_of(status: &str, batch_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key LIKE ?",
        vec![Value::Text(format!(
            "LNK:DEPRECIATION:{status}:{batch_id}:%"
        ))],
    )
    .await
    .expect("查联动通知")
}

/// 批次通知正文（派障 / 聚合口径的用户可见证据）。
async fn notice_content(status: &str, batch_id: Uuid) -> String {
    db().scalar_with_params::<String>(
        "SELECT content FROM sys_notice WHERE idempotency_key LIKE ? ORDER BY created_at DESC LIMIT 1",
        vec![Value::Text(format!(
            "LNK:DEPRECIATION:{status}:{batch_id}:%"
        ))],
    )
    .await
    .expect("查联动通知正文")
}

/// 批次状态（直接读库：`batch_find` 的 `DepBatchRes.status` 同源）。
async fn batch_status(batch_id: Uuid) -> String {
    db().scalar_with_params::<String>(
        "SELECT status FROM biz_depreciation_batch WHERE id = ?::uuid",
        vec![Value::from(&batch_id)],
    )
    .await
    .expect("读批次状态")
}

/// 批次名下的蓝字计提凭证条数（`post_multi_debit` 一笔批次只出一张多借方凭证）。
async fn posted_vouchers_of(batch_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM biz_voucher \
          WHERE source_type = 'DEPRECIATION' AND source_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&batch_id)],
    )
    .await
    .expect("查计提凭证")
}

/// 把 `run_batch` 的返回值里的批次 id 解析出来（DTO 里是 String）。
fn batch_id_of(outcome: &asset_dto::DepRunOutcome) -> Uuid {
    let batch = outcome.batch.as_ref().expect("计提必须返回批次");
    Uuid::parse_str(&batch.id).expect("批次 id 非法")
}

// ==================== ① 过账触发 + 大批量聚合只发一条 ====================

/// 过账（`CALCULATED → POSTED`）必须真的触发联动，且**多台资产只发一条通知**。
///
/// 卡面重点「大批量时通知不刷屏」的证据链：
///
/// * 触发单据是批次（`ctx.doc_id = 批次 id`），不是每台资产 ⇒ 3 台（乃至 300 台）也跑一遍规则；
/// * 通知正文里带台数，用户点进去看明细，而不是收 N 条；
/// * 凭证仍然只有一张（`voucher_multi::post_multi_debit` 的多借方凭证），说明排除清单没把
///   业务自己的落账路径带偏。
#[tokio::test]
async fn lnk10_posting_triggers_one_aggregate_notice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let period = open_month(1).await;
    for index in 0..3 {
        make_asset(user, &format!("LNK10 夹具资产 {index}")).await;
    }
    add_rule(
        asset_dto::DEP_POSTED,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;

    let outcome = depreciation_model::run_batch(Some(&period), Some(user), Some("LNK-10 用例"))
        .await
        .unwrap_or_else(|error| panic!("计提失败：{} {}", error.code, error.msg));
    assert_eq!(
        outcome.outcome,
        asset_dto::DepRunKind::Posted,
        "首次计提必须真的过账（否则下面的通知断言都是虚绿）：{}",
        outcome.message
    );
    let batch_id = batch_id_of(&outcome);
    let batch = outcome.batch.as_ref().expect("批次");
    assert!(
        batch.asset_count >= 3,
        "本次批次至少要带 3 台夹具资产（实际 {}）：夹具没造出来",
        batch.asset_count
    );
    assert_eq!(
        batch.status,
        asset_dto::DEP_POSTED,
        "计提成功后批次必须是 POSTED"
    );

    assert_eq!(
        notices_of(asset_dto::DEP_POSTED, batch_id).await,
        1,
        "{} 台资产也必须只发**一条**通知（触发单据是批次，不是每台资产）",
        batch.asset_count
    );
    let content = notice_content(asset_dto::DEP_POSTED, batch_id).await;
    assert!(
        content.contains(&format!("{} 台资产", batch.asset_count)),
        "聚合口径必须把台数写在正文里（用户点进去看明细）：{content}"
    );
    assert_eq!(
        posted_vouchers_of(batch_id).await,
        1,
        "计提凭证只有 post_multi_debit 那一张（多借方汇总）"
    );
    purge().await;
}

// ==================== ② 重复过账不重复触发 ====================

/// 同期第二次计提走幂等闸（`Exists`）：批次不重开、通知不增、凭证不增。
#[tokio::test]
async fn lnk10_repeat_posting_does_not_trigger_twice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let period = open_month(2).await;
    make_asset(user, "LNK10 重复计提夹具").await;
    add_rule(
        asset_dto::DEP_POSTED,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;

    let first = depreciation_model::run_batch(Some(&period), Some(user), Some("LNK-10 用例"))
        .await
        .expect("首次计提");
    assert_eq!(first.outcome, asset_dto::DepRunKind::Posted);
    let batch_id = batch_id_of(&first);
    assert_eq!(
        notices_of(asset_dto::DEP_POSTED, batch_id).await,
        1,
        "首次过账必须发一条通知"
    );

    let second = depreciation_model::run_batch(Some(&period), Some(user), Some("LNK-10 用例"))
        .await
        .expect("第二次计提");
    assert_eq!(
        second.outcome,
        asset_dto::DepRunKind::Exists,
        "同一期间第二次计提必须被幂等闸挡住：{}",
        second.message
    );
    assert_eq!(
        notices_of(asset_dto::DEP_POSTED, batch_id).await,
        1,
        "被幂等闸挡住的重复计提**不许**再发通知"
    );
    assert_eq!(
        posted_vouchers_of(batch_id).await,
        1,
        "重复计提不许再落一张凭证"
    );
    assert_eq!(batch_status(batch_id).await, asset_dto::DEP_POSTED);
    purge().await;
}

// ==================== ③ 红冲触发（重复红冲不重复触发） ====================

/// 红冲（`POSTED → REVERSED`）必须触发 `DEPRECIATION/REVERSED` 上的规则；
/// 第二次红冲被状态机挡住（409），通知不增。
#[tokio::test]
async fn lnk10_reversal_triggers_notice_once() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let period = open_month(3).await;
    make_asset(user, "LNK10 红冲夹具").await;
    // 只配 REVERSED：顺带证明「过账不误触发 REVERSED 的规则」
    add_rule(
        asset_dto::DEP_REVERSED,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;

    let outcome = depreciation_model::run_batch(Some(&period), Some(user), Some("LNK-10 用例"))
        .await
        .expect("计提");
    assert_eq!(outcome.outcome, asset_dto::DepRunKind::Posted);
    let batch_id = batch_id_of(&outcome);
    assert_eq!(
        notices_of(asset_dto::DEP_REVERSED, batch_id).await,
        0,
        "过账不许触发 REVERSED 的规则（触发点按**变更后**的状态匹配）"
    );

    let reversed = depreciation_model::reverse_batch(batch_id, "LNK-10 用例：重算基数", Some(user))
        .await
        .unwrap_or_else(|error| panic!("红冲失败：{} {}", error.code, error.msg));
    assert_eq!(
        reversed.status,
        asset_dto::DEP_REVERSED,
        "红冲后批次必须是 REVERSED"
    );
    assert_eq!(
        notices_of(asset_dto::DEP_REVERSED, batch_id).await,
        1,
        "DEPRECIATION/REVERSED 上配的 SEND_NOTICE 必须由红冲触发一次"
    );

    let again =
        depreciation_model::reverse_batch(batch_id, "LNK-10 用例：重复红冲", Some(user)).await;
    let error = again.expect_err("重复红冲必须被挡住");
    assert_eq!(
        error.code,
        error_code::ERP_DEPRECIATION_STATE,
        "重复红冲必须被状态机挡住（{}）：{}",
        error.code,
        error.msg
    );
    assert!(
        error.msg.contains("只有已过账"),
        "被挡住的原因必须说清「只有 POSTED 能红冲」：{}",
        error.msg
    );
    assert_eq!(
        notices_of(asset_dto::DEP_REVERSED, batch_id).await,
        1,
        "被挡住的重复红冲**不许**再发一条通知"
    );
    purge().await;
}

// ==================== ④ 事务回滚：通知不发 ====================

/// **卡面铁律**：通知走 commit 后 `run_deferred`，所以事务回滚时一条都不许发。
///
/// 三层证据：
///
/// 1. 事务内调 hook：`SEND_NOTICE` 只进 `run.deferred`（事务内不执行），此时库里 0 条；
/// 2. 回滚后再查：仍然 0 条 —— 没有「先发后回滚」的窗口；
/// 3. 正向对照：同一套动作 `commit()` + `run_deferred_after_commit`，通知立刻 1 条
///    （否则上面的 0 可能是「规则没命中」的虚绿）。
#[tokio::test]
async fn lnk10_rolled_back_tx_never_sends_notice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let batch_id = Uuid::now_v7();
    let event = Uuid::now_v7();
    add_rule(
        asset_dto::DEP_POSTED,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;

    // ---- ① 事务内：通知只在 deferred 里 ----
    let tx = db().begin().await.expect("开事务");
    let change = depreciation_linkage::run_in_tx(
        &tx,
        batch_id,
        "DEP-LNK10-0001",
        "2047-01",
        asset_dto::DEP_POSTED,
        300,
        event,
        Some(user),
    )
    .await
    .expect("事务内跑联动");
    assert_eq!(
        change.run.deferred.len(),
        1,
        "SEND_NOTICE 必须是 AfterCommit 动作：事务内只排队，不许直接发"
    );
    assert_eq!(
        change.run.runs.len(),
        0,
        "批次上只配了通知：事务内不该有 InTx 动作被执行"
    );
    assert_eq!(
        notices_of(asset_dto::DEP_POSTED, batch_id).await,
        0,
        "事务还没提交，通知一条都不许发（否则回滚就是幽灵通知）"
    );

    // ---- ② 回滚：通知仍然 0 ----
    tx.rollback().await.expect("回滚事务");
    assert_eq!(
        notices_of(asset_dto::DEP_POSTED, batch_id).await,
        0,
        "事务回滚后通知必须仍然为 0（「通知已发但计提失败」就是卡面点名的事故）"
    );

    // ---- ③ 正向对照：commit 之后 flush 才会发 ----
    let tx = db().begin().await.expect("开事务");
    let change = depreciation_linkage::run_in_tx(
        &tx,
        batch_id,
        "DEP-LNK10-0001",
        "2047-01",
        asset_dto::DEP_POSTED,
        300,
        event,
        Some(user),
    )
    .await
    .expect("事务内跑联动");
    tx.commit().await.expect("提交事务");
    depreciation_linkage::run_deferred_after_commit(change).await;
    assert_eq!(
        notices_of(asset_dto::DEP_POSTED, batch_id).await,
        1,
        "对照：commit 之后 run_deferred 必须真的发出通知（否则上面的 0 是虚绿）"
    );
    purge().await;
}

// ==================== ⑤ 排除动作确实不跑 ====================

/// 批次上配 `GENERATE_VOUCHER`：通用引擎**不执行**它。
///
/// 两层证据：
///
/// 1. **引擎级**（同一份真实规则行、同一个上下文）：`run_excluding(DEPRECIATION_EXCLUDED_ACTIONS)`
///    里那条规则**不产生任何 RuleRun**（`is_routed_elsewhere` → `continue`），
///    而不带排除清单的 `run` 会产生 RuleRun —— 说明规则**确实被读到了**，
///    只是被排除清单拦下（不是「规则没生效」这种虚绿）；
/// 2. **端到端**：过账后凭证只有 `post_multi_debit` 那一张（多出来就是重复落账），
///    同状态配的 `SEND_NOTICE` 照常发出（排除清单只排凭证，不许把通知一起排掉）。
#[tokio::test]
async fn lnk10_excluded_voucher_actions_never_run() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let batch_id = Uuid::now_v7();
    add_rule(
        asset_dto::DEP_POSTED,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;
    add_rule(
        asset_dto::DEP_POSTED,
        spec::ACTION_GENERATE_VOUCHER,
        r#"{"biz_type":"STOCK_IN","mode":"TAKEOVER"}"#,
    )
    .await;

    // ---- 第一层：引擎级对照（真实规则行 + 真实上下文）----
    let ctx = linkage::LinkageContext::new(
        state_machine::ENTITY_DEPRECIATION,
        asset_dto::DEP_POSTED,
        batch_id,
        "DEP-LNK10-0002",
        Some(user),
    )
    .with_people(Some(user), Some(user), None);
    let tx = db().begin().await.expect("开事务");
    let without_exclusion = linkage::engine::run(&tx, &ctx).await.expect("跑引擎");
    let with_exclusion =
        linkage::engine::run_excluding(&tx, &ctx, spec::DEPRECIATION_EXCLUDED_ACTIONS)
            .await
            .expect("跑引擎（带排除清单）");
    tx.rollback().await.expect("回滚");
    assert!(
        without_exclusion
            .runs
            .iter()
            .any(|item| item.action_code == spec::ACTION_GENERATE_VOUCHER),
        "对照：不带排除清单时这条 GENERATE_VOUCHER 规则必须被读到并执行\
         （本例是 SKIPPED，因为该动作只接管收货入库）—— 否则下面的断言就是虚绿"
    );
    assert!(
        with_exclusion
            .runs
            .iter()
            .all(|item| item.action_code != spec::ACTION_GENERATE_VOUCHER),
        "带排除清单时 GENERATE_VOUCHER 必须**整条不跑**（专用路由，不产生 RuleRun）：{:?}",
        with_exclusion
            .runs
            .iter()
            .map(|item| item.action_code.as_str())
            .collect::<Vec<_>>()
    );

    // ---- 第二层：端到端（真实 `run_batch`）----
    let period = open_month(4).await;
    make_asset(user, "LNK10 排除动作夹具").await;
    let outcome = depreciation_model::run_batch(Some(&period), Some(user), Some("LNK-10 用例"))
        .await
        .expect("计提");
    assert_eq!(outcome.outcome, asset_dto::DepRunKind::Posted);
    let real_batch = batch_id_of(&outcome);
    assert_eq!(
        posted_vouchers_of(real_batch).await,
        1,
        "过账只该有 post_multi_debit 出的那一张凭证：多出来就是 GENERATE_VOUCHER 在重复落账"
    );
    assert_eq!(
        notices_of(asset_dto::DEP_POSTED, real_batch).await,
        1,
        "排除清单只排凭证：同状态的 SEND_NOTICE 必须照常发出（排掉通知 = 接线了永远不响）"
    );
    purge().await;
}
