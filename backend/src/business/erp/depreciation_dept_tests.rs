//! E2E05-FIX-1 真库用例：**补提历史折旧按「期间归属部门」记费用**（卡 `t_abb4bb0b`）。
//!
//! 不设 `ERP_DB_URL` 时全部 SKIP（与本仓其它 ERP 用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//!   cargo test --lib business::erp::depreciation_dept_tests -- --test-threads=1
//! ```
//!
//! # 覆盖（卡面验收 ①~⑤ 逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | `depreciation_dept_follows_period_not_current_card` | A 台 2048-01 在设备部、2048-02-01 转采购部 → **补跑 2048-01 记设备部**（不是采购部）；再跑 2048-02 记采购部；无转移的 B 台行为不变；DEPT 辅助账按期间各归各位 |
//! | 2 | `depreciation_dept_in_period_transfer_counts_new_dept` | 期间**内**转移（3/15）→ 整月算新部门（`change_date <= period_end` 口径） |
//!
//! # 夹具口径
//!
//! * 期间：本文件独占 **2048**（`period_test_support` 拥有 2033 / 2040，`period.rs` 2046，
//!   LNK-10 2047，其它模块 2031/2034~2039/2050，互不重叠）；
//! * 号段：资产 `FA-DEPT-*`、部门 `FA-DEPT-OLD` / `FA-DEPT-NEW`、用户 `fa.dept_*`；
//! * 转移走**真实 handler** `asset_handler::asset_transfer`（留痕由生产代码写，不是夹具手插）；
//! * 串行（`--test-threads=1` / 共享 [`support::DB_LOCK`]）：清场按期间前缀 + 号段删。

use std::collections::BTreeMap;
use std::str::FromStr;

use axum::Json;
use axum::extract::{Extension, Path};
use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::asset_dto::{self, CreateAssetReq};
use super::asset_handler;
use super::asset_model::{self, AssetInsert};
use super::depreciation_model;
use super::ledger;
use crate::business::erp::period_test_support as support;

/// 本文件独占的期间年度（见文件头）。
const YEAR: i16 = 2048;
/// 资产编号号段。
const ASSET_PREFIX: &str = "FA-DEPT-";
/// 部门编码号段（清场用）。
const DEPT_OLD_CODE: &str = "FA-DEPT-OLD";
/// 采购部编码。
const DEPT_NEW_CODE: &str = "FA-DEPT-NEW";

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("金额字面量非法")
}

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("夹具日期")
}

/// 跑一条写 SQL（失败直接 panic，不静默吞错）。
async fn exec(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 取一列 `id`（清场用：只读）。
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

/// 清场：通知 → 凭证 → 明细 → 批次 → 状态日志 → 变动留痕 → 卡片 → 期间 → 部门 → 用户。
///
/// 顺序按外键：`biz_depreciation` 指向批次与卡片；`biz_fixed_asset_change` 指向卡片
/// （RESTRICT）；卡片指向 `sys_dept`（RESTRICT）。状态日志与辅助表没有外键，先按 id 删。
async fn purge() {
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
            // 联动通知的幂等键里带批次 id（LNK:DEPRECIATION:…:<批次>:<凭证>）
            for batch in &batches {
                exec(&format!(
                    "DELETE FROM sys_notice WHERE idempotency_key LIKE '%{batch}%'"
                ))
                .await;
            }
            exec(&format!(
                "DELETE FROM biz_voucher WHERE id IN ({})",
                in_uuids(&vouchers)
            ))
            .await;
        }
        exec(&format!("DELETE FROM biz_depreciation WHERE batch_id IN ({list})")).await;
        exec(&format!("DELETE FROM biz_depreciation_batch WHERE id IN ({list})")).await;
        exec(&format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'DEPRECIATION' AND entity_id IN ({list})"
        ))
        .await;
    }

    let assets = uuids_of(&format!(
        "SELECT id FROM biz_fixed_asset WHERE asset_no LIKE '{ASSET_PREFIX}%'"
    ))
    .await;
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
        exec(&format!("DELETE FROM biz_accounting_period WHERE id IN ({list})")).await;
    }

    exec(&format!(
        "DELETE FROM sys_dept WHERE dept_code IN ('{DEPT_OLD_CODE}', '{DEPT_NEW_CODE}')"
    ))
    .await;
    exec("DELETE FROM sys_user WHERE username LIKE 'fa.dept%'").await;
}

/// 连库（`None` = 没有测试库 → SKIP）+ 串行锁 + 清场 + 造操作人。
async fn setup() -> Option<(tokio::sync::MutexGuard<'static, ()>, Uuid)> {
    let guard = support::DB_LOCK.lock().await;
    if !support::ensure_db().await {
        return None;
    }
    purge().await;
    support::cleanup().await;
    let user = support::seed_user("fa.dept_maker", "FA 折旧部门会计").await;
    Some((guard, user))
}

/// 建一个部门（固定编码，清场按编码删）。
async fn make_dept(code: &str, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    exec(&format!(
        "INSERT INTO sys_dept (id, dept_code, dept_name, status) VALUES ('{id}'::uuid, '{code}', '{name}', 1)"
    ))
    .await;
    id
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

/// 卡片号后缀：取 UUID 末 8 位（前 8 位是 v7 时间戳，同毫秒连造会撞唯一键）。
fn no_tail(id: &Uuid) -> String {
    let digits: String = id.to_string().chars().filter(|c| *c != '-').collect();
    digits
        .get(digits.len().saturating_sub(8)..)
        .unwrap_or("00000000")
        .to_ascii_uppercase()
}

/// 造一张在役资产卡（走真实 `asset_insert`，派生金额按生产口径）。
async fn make_asset(creator: Uuid, dept: Uuid, name: &str, purchase: NaiveDate) -> Uuid {
    let category = any_category().await;
    let req = CreateAssetReq {
        asset_name: Some(name.to_string()),
        category_id: Some(category.id.to_string()),
        spec: None,
        use_dept_id: Some(dept.to_string()),
        custodian_id: None,
        location: None,
        original_value: Some(dec("60000")),
        tax_amount: Some(Decimal::ZERO),
        purchase_date: Some(purchase),
        source_type: None,
        source_id: None,
        source_no: None,
        buy_mode: None,
        pay_mode: None,
        supplier_id: None,
        remark: Some("E2E05-FIX-1 真库用例".to_string()),
    };
    let spec = asset_dto::validate_create(&req).expect("夹具建卡请求必须通过校验");
    let id = Uuid::now_v7();
    let asset_no = format!("{ASSET_PREFIX}{}", no_tail(&id));
    let (residual, base, monthly) =
        asset_dto::derive_amounts(spec.original_value, category.salvage_rate, category.useful_life_months);
    asset_model::asset_insert(
        db(),
        id,
        &AssetInsert {
            asset_no,
            spec: asset_dto::CreateAssetSpec {
                use_dept_id: dept,
                ..spec
            },
            category_code: category.category_code.clone(),
            use_dept_name: dept_name_of(dept).await,
            custodian_name: None,
            supplier_name: None,
            residual_value: residual,
            depreciable_base: base,
            monthly_depreciation: monthly,
            useful_life_months: category.useful_life_months,
            salvage_rate: category.salvage_rate,
            start_depreciation_date: super::asset_compute::start_depreciation_date(purchase),
            operator: Some(creator),
        },
    )
    .await
    .expect("建卡");
    id
}

/// 部门名（夹具已知编码，直接读库）。
async fn dept_name_of(dept: Uuid) -> String {
    db().scalar_with_params::<String>(
        "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
        vec![Value::from(&dept)],
    )
    .await
    .expect("查部门名")
}

/// 走**真实 handler** 做部门转移（留痕由生产代码写：`asset_set_dept` + `insert_change` 同事务）。
async fn transfer(asset_id: Uuid, to_dept: Uuid, when: NaiveDate, user: Uuid) {
    let claims = ClaimsBuilder::new(user.to_string())
        .username("fa.dept_maker".to_string())
        .build(TokenType::Access, 3600);
    let req = asset_dto::TransferReq {
        to_dept_id: Some(to_dept.to_string()),
        change_date: Some(when),
        reason: Some("夹具：跨部门转移".to_string()),
    };
    let result =
        asset_handler::asset_transfer(Extension(AuthClaims(claims)), Path(asset_id.to_string()), Json(req)).await;
    if let Err(error) = result {
        panic!("部门转移失败：{} {}（status {}）", error.code, error.msg, error.status);
    }
}

/// 开一期（`2048-MM`，OPEN）并返回期间编码。
async fn open_month(month: i16) -> String {
    let row = super::period::open_period(YEAR, month, Some("E2E05-FIX-1 夹具"), support::operator())
        .await
        .unwrap_or_else(|error| panic!("开账失败（{YEAR}-{month:02}）：{}", error.msg));
    assert_eq!(row.period_code, format!("{YEAR}-{month:02}"), "期间编码");
    row.period_code
}

/// 批次 id（DTO 里是 String）。
fn batch_id_of(outcome: &asset_dto::DepRunOutcome) -> Uuid {
    let batch = outcome.batch.as_ref().expect("计提必须返回批次");
    Uuid::parse_str(&batch.id).expect("批次 id 非法")
}

/// 计提（失败带出 code/msg，避免只看到 `unwrap` 的 panic 文案）。
async fn run_period(period: &str, user: Uuid) -> asset_dto::DepRunOutcome {
    depreciation_model::run_batch(Some(period), Some(user), Some("E2E05-FIX-1 用例"))
        .await
        .unwrap_or_else(|error| panic!("计提 {period} 失败：{} {}", error.code, error.msg))
}

/// 本批次计提凭证里**借方**分录按 `aux_dept_id` 汇总的金额。
///
/// 这正是部门损益 / DEPT 辅助账的取数来源（`ledger::aux_balance` 读同一列）。
async fn debit_by_dept(batch_id: Uuid) -> BTreeMap<Uuid, Decimal> {
    let rows = db()
        .find_with_params(
            "SELECT e.aux_dept_id, e.debit_amount::text AS debit_amount \
               FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
              WHERE v.source_type = 'DEPRECIATION' AND v.source_id = ?::uuid \
                AND e.debit_amount > 0",
            vec![Value::from(&batch_id)],
        )
        .await
        .expect("查折旧凭证借方分录");
    let mut map: BTreeMap<Uuid, Decimal> = BTreeMap::new();
    for row in rows.iter() {
        let dept = row
            .get_typed::<Option<Uuid>>("aux_dept_id")
            .expect("读 aux_dept_id")
            .expect("折旧借方分录必须挂部门辅助核算（否则部门损益无从归集）");
        let amount = DecParser::parse(&row.get_typed::<String>("debit_amount").expect("读金额"));
        *map.entry(dept).or_default() += amount;
    }
    map
}

/// 明细行里某台资产在**本期**记的部门（`biz_depreciation.dept_id`）。
async fn detail_dept_of(batch_id: Uuid, asset_id: Uuid) -> (Uuid, String, Decimal) {
    let rows = db()
        .find_with_params(
            "SELECT dept_id, dept_name, amount::text AS amount FROM biz_depreciation \
              WHERE batch_id = ?::uuid AND asset_id = ?::uuid",
            vec![Value::from(&batch_id), Value::from(&asset_id)],
        )
        .await
        .expect("查折旧明细");
    let row = rows.first().expect("该资产本期必须有明细");
    (
        row.get_typed("dept_id").expect("明细 dept_id"),
        row.get_typed("dept_name").expect("明细 dept_name"),
        DecParser::parse(&row.get_typed::<String>("amount").expect("明细金额")),
    )
}

/// 卡片上的月折旧额（断言用，避免硬编码派生金额）。
async fn monthly_of(asset_id: Uuid) -> Decimal {
    let text = db()
        .scalar_with_params::<String>(
            "SELECT monthly_depreciation::text FROM biz_fixed_asset WHERE id = ?::uuid",
            vec![Value::from(&asset_id)],
        )
        .await
        .expect("查月折旧额");
    DecParser::parse(&text)
}

/// 借方分录里挂 `dept` 的费用科目金额（= 该部门那一行的金额）。
async fn expense_debit_of_dept(batch_id: Uuid, dept: Uuid) -> Decimal {
    let rows = db()
        .find_with_params(
            "SELECT e.debit_amount::text AS debit_amount \
               FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
              WHERE v.source_type = 'DEPRECIATION' AND v.source_id = ?::uuid \
                AND e.aux_dept_id = ?::uuid AND e.debit_amount > 0",
            vec![Value::from(&batch_id), Value::from(&dept)],
        )
        .await
        .expect("查部门借方分录");
    rows.iter()
        .map(|row| DecParser::parse(&row.get_typed::<String>("debit_amount").expect("金额")))
        .fold(Decimal::ZERO, |sum, amount| sum + amount)
}

/// 金额文本 → Decimal（`::text` 取出再解析，与生产代码同口径）。
struct DecParser;

impl DecParser {
    fn parse(text: &str) -> Decimal {
        Decimal::from_str(text.trim()).unwrap_or_else(|err| panic!("金额「{text}」解析失败：{err}"))
    }
}

// ==================== ① 补提历史期间按「期间归属部门」====================

/// 卡面验收 ①②③④⑤ 的主场景：
///
/// ```text
/// A 台：2048-01 在设备部，2048-02-01 转采购部（转移发生在"补跑 01 月"之前 —— 这就是 bug 现场）
/// B 台：从未转移，一直在采购部
///
/// 补跑 2048-01 → A 的折旧必须记「设备部」（不是卡片当前的采购部）；B 照旧记采购部
/// 再跑 2048-02 → A 记采购部（已转），B 照旧
/// ```
#[tokio::test]
async fn depreciation_dept_follows_period_not_current_card() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let dept_old = make_dept(DEPT_OLD_CODE, "设备部").await;
    let dept_new = make_dept(DEPT_NEW_CODE, "采购部").await;
    let purchase = date(2047, 12, 10); // 起提日 2048-01-01 ⇒ 2048-01 是第一个计提月
    let asset_a = make_asset(user, dept_old, "A 台", purchase).await;
    let asset_b = make_asset(user, dept_new, "B 台（无转移）", purchase).await;
    let monthly_a = monthly_of(asset_a).await;
    let monthly_b = monthly_of(asset_b).await;

    // 先开账，再做 2048-02-01 的转移（现实中就是这样：2 月初转的账，事后才补跑 1 月折旧）
    open_month(1).await;
    open_month(2).await;
    transfer(asset_a, dept_new, date(2048, 2, 1), user).await;
    // 转移真的生效了：卡片当前部门已经是采购部（= bug 触发条件）
    let rows = db()
        .find_with_params(
            "SELECT use_dept_id FROM biz_fixed_asset WHERE id = ?::uuid",
            vec![Value::from(&asset_a)],
        )
        .await
        .expect("读卡片当前部门");
    let current_dept: Uuid = rows
        .first()
        .expect("卡片必须存在")
        .get_typed("use_dept_id")
        .expect("读 use_dept_id");
    assert_eq!(current_dept, dept_new, "夹具前置：转移必须已改掉卡片当前部门");

    // ---- 补跑 2048-01 ----
    let jan = run_period("2048-01", user).await;
    assert_eq!(jan.outcome, asset_dto::DepRunKind::Posted, "{}", jan.message);
    let jan_batch = batch_id_of(&jan);
    let jan_debits = debit_by_dept(jan_batch).await;
    assert_eq!(
        jan_debits.get(&dept_old).copied().unwrap_or_default(),
        monthly_a,
        "A 台 2048-01 的折旧必须记「设备部」（转移前那个月的归属部门）"
    );
    assert_eq!(
        jan_debits.get(&dept_new).copied().unwrap_or_default(),
        monthly_b,
        "采购部那一行只能有 B 台的折旧：A 台漏进采购部就是本卡的 bug"
    );
    // 明细行同样按期间部门记账（批次明细是部门损益的第二数据源）
    let (detail_dept, detail_dept_name, detail_amount) = detail_dept_of(jan_batch, asset_a).await;
    assert_eq!(detail_dept, dept_old, "A 台 2048-01 明细的 dept_id 必须是设备部");
    assert_eq!(detail_dept_name, "设备部");
    assert_eq!(detail_amount, monthly_a);

    // ---- DEPT 辅助账（部门损益的取数口径）按期间各归各位 ----
    let old_aux = ledger::aux_balance(&ledger::AuxBalanceQuery {
        aux_type: Some(ledger::AUX_DEPT.to_string()),
        aux_id: Some(dept_old.to_string()),
        period_from: Some("2048-01".to_string()),
        period_to: Some("2048-01".to_string()),
    })
    .await
    .expect("查设备部辅助账");
    assert_eq!(
        old_aux.total_period_debit, monthly_a,
        "设备部 2048-01 的辅助账借方只能是 A 台那一笔（{}）",
        old_aux.total_period_debit
    );
    let new_aux = ledger::aux_balance(&ledger::AuxBalanceQuery {
        aux_type: Some(ledger::AUX_DEPT.to_string()),
        aux_id: Some(dept_new.to_string()),
        period_from: Some("2048-01".to_string()),
        period_to: Some("2048-01".to_string()),
    })
    .await
    .expect("查采购部辅助账");
    assert_eq!(
        new_aux.total_period_debit, monthly_b,
        "采购部 2048-01 的辅助账借方只能是 B 台那一笔（{}）",
        new_aux.total_period_debit
    );

    // ---- 再跑 2048-02：A 台已转过去，整月算采购部 ----
    let feb = run_period("2048-02", user).await;
    assert_eq!(feb.outcome, asset_dto::DepRunKind::Posted, "{}", feb.message);
    let feb_batch = batch_id_of(&feb);
    let feb_debits = debit_by_dept(feb_batch).await;
    assert_eq!(
        feb_debits.get(&dept_new).copied().unwrap_or_default(),
        monthly_a + monthly_b,
        "2048-02：A（已转）+ B 都记采购部"
    );
    assert!(
        !feb_debits.contains_key(&dept_old),
        "2048-02 不该再有设备部的折旧借方（A 已转走、B 本来就在采购部）：{feb_debits:?}"
    );
    let (feb_detail_dept, _, feb_amount) = detail_dept_of(feb_batch, asset_a).await;
    assert_eq!(feb_detail_dept, dept_new, "2048-02 明细的部门必须跟着转移走");
    assert_eq!(feb_amount, monthly_a);
    assert_eq!(
        expense_debit_of_dept(feb_batch, dept_old).await,
        Decimal::ZERO,
        "设备部在 2048-02 没有折旧费用"
    );

    // ---- ④ 无转移的资产行为不变：两个月的金额都是它自己的月折旧 ----
    let b_jan = detail_dept_of(jan_batch, asset_b).await;
    assert_eq!((b_jan.0, b_jan.2), (dept_new, monthly_b), "无转移资产 2048-01 不变");
    let b_feb = detail_dept_of(feb_batch, asset_b).await;
    assert_eq!((b_feb.0, b_feb.2), (dept_new, monthly_b), "无转移资产 2048-02 不变");

    purge().await;
}

// ==================== ② 期间内转移：整月算新部门 ====================

/// 期间**内**转移（3/15）→ 按 `change_date <= period_end` 判，整月算新部门。
///
/// 与「当月减少当月照提」的**资产**口径分开：那条决定"提不提"，这条决定"记哪个部门"。
#[tokio::test]
async fn depreciation_dept_in_period_transfer_counts_new_dept() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let dept_old = make_dept(DEPT_OLD_CODE, "设备部").await;
    let dept_new = make_dept(DEPT_NEW_CODE, "采购部").await;
    let asset = make_asset(user, dept_old, "C 台", date(2047, 12, 10)).await;
    let monthly = monthly_of(asset).await;

    open_month(3).await;
    transfer(asset, dept_new, date(2048, 3, 15), user).await;

    let mar = run_period("2048-03", user).await;
    assert_eq!(mar.outcome, asset_dto::DepRunKind::Posted, "{}", mar.message);
    let batch = batch_id_of(&mar);
    let debits = debit_by_dept(batch).await;
    assert_eq!(
        debits.get(&dept_new).copied().unwrap_or_default(),
        monthly,
        "期间内转移（3/15）→ 整月算新部门"
    );
    assert!(
        !debits.contains_key(&dept_old),
        "期间内转移后整月不该再有旧部门的折旧借方：{debits:?}"
    );

    purge().await;
}
