//! 固定资产联动接线（`FIXED_ASSET`，LNK-09）的**真库**用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 SKIP（与本仓其它 ERP 用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//!   cargo test --lib business::erp::asset_linkage_tests -- --test-threads=1
//! ```
//!
//! # 覆盖（卡面「处置触发 / 凭证不重复」逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | `lnk09_disposal_triggers_disposed_notice` | 处置（审批通过 → `DISPOSED`）真的触发通知 |
//! | 2 | `lnk09_repeat_disposal_does_not_trigger_twice` | 同一张卡第二次处置被 409 挡住，通知**不增**（重复过账不重复触发） |
//! | 3 | `lnk09_cancel_disposal_triggers_restore_notice` | 作废处置（`DISPOSED → IN_USE`）触发通知，处置凭证已红冲 |
//! | 4 | `lnk09_rolled_back_tx_never_sends_notice` | **事务回滚 → 通知不发**（deferred 随事务丢），并有「提交后才发」的正向对照 |
//! | 5 | `lnk09_excluded_voucher_actions_never_run` | 资产上配 `GENERATE_VOUCHER`：通用引擎**不执行**它（同状态 `SEND_NOTICE` 照跑），处置凭证只有 asset_bridge 那一套 |
//!
//! # 夹具口径
//!
//! * 不走「按前缀扫全库删」：卡片 / 审批实例 / 凭证的主键都由用例自己持有，清理按
//!   **id 集合**删；联动规则与通知按 `LNK09-` / `LNK:FIXED_ASSET:%` 前缀清
//!   （它们没有单据主键可依）；
//! * 用户用 `period_test_support::seed_user`（`p2.` 号段，随 `cleanup()` 一起清）；
//! * 卡片用真实的 `asset_model::asset_insert`（含派生金额），不手工拼 SQL ——
//!   手工拼出来的卡片可能违反 `ck_biz_fa_value` 之类约束，反而测不到接线；
//! * 处置日期取 **2099-03-15**（库里没有这个期间行 ⇒ `post_voucher` 只在「已结账」
//!   时才拒，不会因为缺期间行把处置打成 500；也绝不碰 2026 生产期间）；
//! * 审批实例**直插一行已通过实例**（`status = APPROVED` + `ended_at`）：本卡测的是
//!   「审批回调 → 处置 → 联动」这一段，审批引擎本身由 `flow` 的用例覆盖。
//!
//! **串行**（`--test-threads=1`）：联动规则是全表唯一索引，并行会互删。

use std::str::FromStr;

use axum::Json;
use axum::extract::{Extension, Path};
use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::asset_bridge;
use super::asset_dto::{self, CreateAssetReq};
use super::asset_handler::{CancelDisposalReq, asset_dispose_cancel};
use super::asset_linkage;
use super::asset_model::{self, AssetInsert};
use crate::business::erp::period_test_support as support;
use crate::business::flow::linkage;
use crate::business::flow::linkage::spec;
use crate::business::shared::state_machine;

/// 处置日期（库里没有 2099-03 的期间行，见文件头）。
const DISPOSE_DATE: &str = "2099-03-15";
/// 处置审批实例的业务类型（与 `asset_bridge::BIZ_TYPE` 同源）。
const BIZ_TYPE: &str = asset_bridge::BIZ_TYPE;

/// 处置类型 SALE（出售 → 3 张凭证：1606 清理 / 1002 收款 / 2221.02 销项）。
const KIND_SALE: &str = "SALE";

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("金额字面量非法")
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

/// 清场：本卡的联动规则 / 通知 + 按 id 集合删卡片侧的一切。
///
/// 顺序：通知 → 凭证（分录随 CASCADE）→ 变动留痕 → 状态日志 → 审批实例 → 卡片。
/// 审批实例按**本卡号段**取（`FLW-LNK09-` / `biz_no LIKE 'FA-LNK09-%'` / 夹具发起人），
/// 这样即使上一个 run 在「插实例」中途 panic（卡片还没建）也能清干净。
async fn purge() {
    exec("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK09-%'").await;
    exec("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:FIXED_ASSET:%'").await;
    // 按本卡号段找残留（`FA-LNK09-` 前缀是本文件独占的卡片号）
    let assets = uuids_of("SELECT id FROM biz_fixed_asset WHERE asset_no LIKE 'FA-LNK09-%'").await;
    if !assets.is_empty() {
        let list = in_uuids(&assets);
        exec(&format!(
            "DELETE FROM biz_voucher WHERE source_type = '{}' AND source_id IN ({list})",
            asset_bridge::SOURCE_DISPOSAL
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_fixed_asset_change WHERE asset_id IN ({list})"
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'FIXED_ASSET' AND entity_id IN ({list})"
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_fixed_asset WHERE id IN ({})",
            in_uuids(&assets)
        ))
        .await;
    }
    // 审批实例：卡片已经删了，只能按号段 / 夹具发起人收（没有外键指向卡片）
    let instances = uuids_of(&format!(
        "SELECT id FROM biz_flow_instance \
          WHERE biz_type = '{BIZ_TYPE}' \
            AND (instance_no LIKE 'FLW-LNK09-%' \
                 OR biz_no LIKE 'FA-LNK09-%' \
                 OR initiator_id IN (SELECT id FROM sys_user WHERE username LIKE 'p2.lnk09%'))"
    ))
    .await;
    if !instances.is_empty() {
        exec(&format!(
            "DELETE FROM biz_flow_instance WHERE id IN ({})",
            in_uuids(&instances)
        ))
        .await;
    }
    // 夹具用户（实例删完才轮得到它：`initiator_id` 是硬外键）
    exec("DELETE FROM sys_user WHERE username LIKE 'p2.lnk09%'").await;
}

/// 连库（`None` = 没有测试库 → 用例 SKIP）+ 拿串行锁 + 清场 + 造操作人。
///
/// 注意顺序：**先**跑本文件的 [`purge`]（它会删掉本卡的审批实例），**再**走
/// `period_test_support::cleanup()` 删夹具用户 —— 反过来会在
/// `biz_flow_instance_initiator_id_fkey` 上炸（上一轮中途 panic 留下的实例还挂在那个人身上）。
async fn setup() -> Option<(tokio::sync::MutexGuard<'static, ()>, Uuid)> {
    let guard = support::DB_LOCK.lock().await;
    if !support::ensure_db().await {
        return None;
    }
    purge().await;
    support::cleanup().await;
    let user = support::seed_user("p2.lnk09_maker", "LNK09 资产管理员").await;
    Some((guard, user))
}

/// 取一个已存在的部门 id（夹具用：不新建部门）。
async fn any_dept() -> Uuid {
    let rows = db()
        .query_sql(
            "SELECT id FROM sys_dept WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1"
                .to_string(),
            vec![],
        )
        .await
        .expect("查部门");
    rows.first()
        .and_then(|row| row.get_typed("id").ok())
        .unwrap_or_else(|| panic!("库里没有部门（先跑基础种子迁移）"))
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

/// 造一张在役资产卡（走真实 `asset_insert`：派生金额 / 约束都按生产口径）。
async fn make_asset(creator: Uuid) -> (Uuid, String) {
    let dept = any_dept().await;
    let category = any_category().await;
    let req = CreateAssetReq {
        asset_name: Some("LNK09 夹具笔记本".to_string()),
        category_id: Some(category.id.to_string()),
        spec: None,
        use_dept_id: Some(dept.to_string()),
        custodian_id: None,
        location: None,
        original_value: Some(dec("100000")),
        tax_amount: Some(Decimal::ZERO),
        purchase_date: NaiveDate::from_ymd_opt(2099, 1, 15),
        source_type: None,
        source_id: None,
        source_no: None,
        buy_mode: None,
        pay_mode: None,
        supplier_id: None,
        remark: Some("LNK-09 真库用例".to_string()),
    };
    let spec = asset_dto::validate_create(&req).expect("夹具建卡请求必须通过校验");
    let id = Uuid::now_v7();
    let asset_no = format!("FA-LNK09-{}", &id.to_string()[..8]);
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
            start_depreciation_date: super::asset_compute::start_depreciation_date(
                NaiveDate::from_ymd_opt(2099, 1, 15).expect("日期"),
            ),
            operator: Some(creator),
        },
    )
    .await
    .expect("建卡");
    (id, asset_no)
}

/// 造一条**已通过**的资产处置审批实例（本卡只测回调 → 处置 → 联动这一段）。
///
/// 单号取 UUIDv7 的**末 8 位**（`flow_model::new_instance_no` 同口径）：前 8 位是时间戳，
/// 同一毫秒连造两条会撞 `uk_biz_flow_instance_no`（实测踩过）。
fn instance_tail(id: &Uuid) -> String {
    let digits: String = id.to_string().chars().filter(|c| *c != '-').collect();
    digits
        .get(digits.len().saturating_sub(8)..)
        .unwrap_or("00000000")
        .to_ascii_uppercase()
}

async fn seed_approved_instance(asset_id: Uuid, asset_no: &str, initiator: Uuid) -> Uuid {
    let flow = asset_bridge::published_disposal_flow()
        .await
        .expect("查处置流程")
        .expect("库里必须有已发布的 asset_disposal 流程（V0231 种子）");
    let id = Uuid::now_v7();
    let form = serde_json::json!({
        "asset_no": asset_no,
        "disposal_type": KIND_SALE,
        "disposal_date": DISPOSE_DATE,
        "reason": "LNK-09 用例：闲置出售",
        "sale_price": "50000.0000",
        "output_tax": "6500.0000",
    });
    db().execute_with_params(
        "INSERT INTO biz_flow_instance \
            (id, instance_no, def_id, version_id, version_no, flow_code, flow_name, \
             biz_type, biz_id, biz_no, title, status, initiator_id, initiator_name, \
             initiator_dept_id, form_data, auto_passed_nodes, round_total, revision, \
             started_at, ended_at, created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?, ?, ?, ?, ?::uuid, ?, ?, 'APPROVED', \
                 ?::uuid, ?, NULL, ?::jsonb, 0, 1, 1, now(), now(), now(), now(), \
                 ?::uuid, ?::uuid)",
        vec![
            Value::from(&id),
            Value::Text(format!("FLW-LNK09-{}", instance_tail(&id))),
            Value::from(&flow.def_id),
            Value::from(&flow.version_id),
            Value::from(flow.version_no),
            Value::Text(flow.flow_code.clone()),
            Value::Text(flow.flow_name.clone()),
            Value::Text(BIZ_TYPE.to_string()),
            Value::from(&asset_id),
            Value::Text(asset_no.to_string()),
            Value::Text(format!("{asset_no} LNK-09 处置审批")),
            Value::from(&initiator),
            Value::Text("lnk09-tester".to_string()),
            Value::Json(form.to_string()),
            Value::from(&initiator),
            Value::from(&initiator),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入审批实例失败：{err}"));
    id
}

/// 卡片当前状态。
async fn status_of(asset_id: Uuid) -> Option<String> {
    db().scalar_with_params::<Option<String>>(
        "SELECT status FROM biz_fixed_asset WHERE id = ?::uuid",
        vec![Value::from(&asset_id)],
    )
    .await
    .expect("读卡片状态")
}

/// 按幂等键数通知条数（资产联动的通知唯一键 = `LNK:FIXED_ASSET:<状态>:<卡片 id>`）。
async fn notices_of(status: &str, asset_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key = ?",
        vec![Value::Text(format!("LNK:FIXED_ASSET:{status}:{asset_id}"))],
    )
    .await
    .expect("查联动通知")
}

/// 某资产名下**未删除**的处置凭证条数（按来源类型 + 来源 id）。
async fn disposal_vouchers_of(asset_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM biz_voucher \
          WHERE source_type = 'FIXED_ASSET_DISPOSAL' AND source_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&asset_id)],
    )
    .await
    .expect("查处置凭证")
}

/// 插一条 `FIXED_ASSET/<status>` 上的规则（绕过 API 写库：与「管理员在配置页配好」等价）。
async fn add_rule(status: &str, action: &str, params: &str) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    exec(&format!(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), 'LNK09-{status}-{action}-{nanos}', 'FIXED_ASSET', \
                 '{status}', '{action}', '{params}'::jsonb, 10)"
    ))
    .await;
}

/// 资产规则参数：TAKEOVER + 收件人取制单人（卡片有 `create_by`）。
const NOTICE_CREATOR: &str = r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#;

// ==================== ① 处置触发 ====================

/// 处置（审批通过 → `DISPOSED`）必须真的触发联动：不接这一条，
/// `FIXED_ASSET/DISPOSED` 上配的「资产处置通知制单人」能保存、能启用、永远不触发。
#[tokio::test]
async fn lnk09_disposal_triggers_disposed_notice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let (asset_id, asset_no) = make_asset(user).await;
    let instance = seed_approved_instance(asset_id, &asset_no, user).await;
    add_rule(
        asset_dto::STATUS_DISPOSED,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;

    let executed = asset_bridge::on_disposal_settled(Some(instance), "APPROVED").await;
    assert_eq!(
        executed,
        Some(asset_id),
        "审批通过必须真的执行处置（否则下面的断言都是虚绿）"
    );
    assert_eq!(
        status_of(asset_id).await.as_deref(),
        Some(asset_dto::STATUS_DISPOSED),
        "处置后卡片状态必须是 DISPOSED"
    );
    assert!(
        disposal_vouchers_of(asset_id).await >= 1,
        "处置必须真的出凭证（出售 3 张）"
    );
    assert_eq!(
        notices_of(asset_dto::STATUS_DISPOSED, asset_id).await,
        1,
        "FIXED_ASSET/DISPOSED 上配的 SEND_NOTICE 必须真的发出一条通知"
    );
    purge().await;
}

// ==================== ② 重复处置不重复触发 ====================

/// 处置是「状态机 + CAS」的单向操作：同一张卡第二次处置被 409 挡住，
/// 联动**不许**跑第二遍（同一状态的通知只发一条）。
#[tokio::test]
async fn lnk09_repeat_disposal_does_not_trigger_twice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let (asset_id, asset_no) = make_asset(user).await;
    let first = seed_approved_instance(asset_id, &asset_no, user).await;
    add_rule(
        asset_dto::STATUS_DISPOSED,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;

    assert_eq!(
        asset_bridge::on_disposal_settled(Some(first), "APPROVED").await,
        Some(asset_id),
        "第一次处置必须成功"
    );
    assert_eq!(
        notices_of(asset_dto::STATUS_DISPOSED, asset_id).await,
        1,
        "第一次处置必须发一条通知"
    );

    // 第二张审批单（同一张卡）：`disposable()` 已经是 false ⇒ 处置被拒，联动不跑
    let second = seed_approved_instance(asset_id, &asset_no, user).await;
    let again = asset_bridge::on_disposal_settled(Some(second), "APPROVED").await;
    assert_eq!(
        again, None,
        "已处置的卡片第二次处置必须失败（on_disposal_settled 只 warn 不回滚审批单）"
    );
    assert_eq!(
        status_of(asset_id).await.as_deref(),
        Some(asset_dto::STATUS_DISPOSED),
        "重复处置不许把状态改坏"
    );
    assert_eq!(
        notices_of(asset_dto::STATUS_DISPOSED, asset_id).await,
        1,
        "被挡住的重复处置**不许**再发一条通知"
    );
    purge().await;
}

// ==================== ③ 作废处置触发 ====================

/// 作废处置（`DISPOSED → IN_USE`）必须触发：`FIXED_ASSET/IN_USE` 上配的
/// 「资产恢复在役 → 通知使用部门」不能永远不响
/// （建卡是建行不接、停用 / 启用没有入口，IN_USE 的唯一触发源就是这一条）。
#[tokio::test]
async fn lnk09_cancel_disposal_triggers_restore_notice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let (asset_id, asset_no) = make_asset(user).await;
    let instance = seed_approved_instance(asset_id, &asset_no, user).await;
    assert_eq!(
        asset_bridge::on_disposal_settled(Some(instance), "APPROVED").await,
        Some(asset_id),
        "前置处置必须成功"
    );
    let blue = disposal_vouchers_of(asset_id).await;
    assert!(blue >= 1, "前置处置必须出凭证");

    add_rule(
        asset_dto::STATUS_IN_USE,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;
    // 处置不该触发 IN_USE 上的规则（触发点按**变更后**的状态匹配）
    assert_eq!(
        notices_of(asset_dto::STATUS_IN_USE, asset_id).await,
        0,
        "处置不许触发 IN_USE 的规则"
    );

    let claims = claims_of(user);
    let res = asset_dispose_cancel(
        Extension(AuthClaims(claims)),
        Path(asset_id.to_string()),
        Json(CancelDisposalReq {
            reason: Some("LNK-09 用例：处置依据不成立，作废".to_string()),
        }),
    )
    .await
    .unwrap_or_else(|err| panic!("作废处置失败：{} {}", err.code, err.msg));
    assert!(res.data.is_some(), "作废处置应返回详情");

    assert_eq!(
        status_of(asset_id).await.as_deref(),
        Some(asset_dto::STATUS_IN_USE),
        "作废处置后卡片必须恢复在役"
    );
    assert_eq!(
        notices_of(asset_dto::STATUS_IN_USE, asset_id).await,
        1,
        "FIXED_ASSET/IN_USE 上配的 SEND_NOTICE 必须由「作废处置」触发一次"
    );
    purge().await;
}

// ==================== ④ 事务回滚：通知不发 ====================

/// **铁律 ④**：通知走 `commit 后 run_deferred`，所以事务回滚时一条都不许发。
///
/// 两层证据：
///
/// 1. 事务内调 hook：`SEND_NOTICE` 只进 `run.deferred`（事务内不执行），此时库里 0 条；
/// 2. 事务回滚后再查：仍然 0 条 —— 没有「先发后回滚」的窗口；
/// 3. 正向对照：同一套动作改成 commit + `run_deferred_after_commit`，通知立刻 1 条
///    （否则上面的 0 可能是「规则没命中」的虚绿）。
#[tokio::test]
async fn lnk09_rolled_back_tx_never_sends_notice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let (asset_id, asset_no) = make_asset(user).await;
    add_rule(
        asset_dto::STATUS_DISPOSED,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;

    // ---- ① 事务内：通知只在 deferred 里 ----
    let tx = db().begin().await.expect("开事务");
    let change = asset_linkage::run_in_tx(
        &tx,
        asset_id,
        &asset_no,
        asset_dto::STATUS_DISPOSED,
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
        "事务内不该有 InTx 动作被执行（资产上只配了通知）"
    );
    assert_eq!(
        notices_of(asset_dto::STATUS_DISPOSED, asset_id).await,
        0,
        "事务还没提交，通知一条都不许发（否则回滚就是幽灵通知）"
    );

    // ---- ② 回滚：通知仍然 0 ----
    tx.rollback().await.expect("回滚事务");
    assert_eq!(
        notices_of(asset_dto::STATUS_DISPOSED, asset_id).await,
        0,
        "事务回滚后通知必须仍然为 0（「通知已发但处置失败」就是卡面点名的事故）"
    );

    // ---- ③ 正向对照：commit 之后 flush 才会发 ----
    let tx = db().begin().await.expect("开事务");
    let change = asset_linkage::run_in_tx(
        &tx,
        asset_id,
        &asset_no,
        asset_dto::STATUS_DISPOSED,
        Some(user),
    )
    .await
    .expect("事务内跑联动");
    tx.commit().await.expect("提交事务");
    asset_linkage::run_deferred_after_commit(change).await;
    assert_eq!(
        notices_of(asset_dto::STATUS_DISPOSED, asset_id).await,
        1,
        "对照：commit 之后 run_deferred 必须真的发出通知（否则上面的 0 是虚绿）"
    );
    purge().await;
}

// ==================== ⑤ 排除动作确实不跑 ====================

/// 资产上配 `GENERATE_VOUCHER` / `REVERSE_VOUCHER`：通用引擎**不执行**它们。
///
/// 两层证据：
///
/// 1. **引擎级**（同一份真实规则行、同一个上下文）：`run_excluding(FIXED_ASSET_EXCLUDED_ACTIONS)`
///    里那两条规则**不产生任何 RuleRun**（`is_routed_elsewhere` → `continue`），
///    而不带排除清单的 `run` 会产生 RuleRun —— 说明规则**确实被读到了**，
///    只是被排除清单拦下（不是「规则没生效」这种虚绿）；
/// 2. **端到端**：处置后凭证只有 `asset_bridge` 那一套（出售 3 张，不多不少），
///    同状态配的 `SEND_NOTICE` 照常发出（排除清单只排凭证，不许把通知一起排掉）。
#[tokio::test]
async fn lnk09_excluded_voucher_actions_never_run() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let (asset_id, asset_no) = make_asset(user).await;
    let instance = seed_approved_instance(asset_id, &asset_no, user).await;
    add_rule(
        asset_dto::STATUS_DISPOSED,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;
    add_rule(
        asset_dto::STATUS_DISPOSED,
        spec::ACTION_GENERATE_VOUCHER,
        r#"{"biz_type":"STOCK_IN","mode":"TAKEOVER"}"#,
    )
    .await;

    // ---- 第一层：引擎级对照（真实规则行 + 真实上下文）----
    let ctx = linkage::LinkageContext::new(
        state_machine::ENTITY_FIXED_ASSET,
        asset_dto::STATUS_DISPOSED,
        asset_id,
        &asset_no,
        Some(user),
    );
    let tx = db().begin().await.expect("开事务");
    let without_exclusion = linkage::engine::run(&tx, &ctx).await.expect("跑引擎");
    let with_exclusion =
        linkage::engine::run_excluding(&tx, &ctx, spec::FIXED_ASSET_EXCLUDED_ACTIONS)
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

    // ---- 第二层：端到端（走真实审批回调 → 处置）----
    assert_eq!(
        asset_bridge::on_disposal_settled(Some(instance), "APPROVED").await,
        Some(asset_id),
        "处置必须成功"
    );
    assert_eq!(
        disposal_vouchers_of(asset_id).await,
        3,
        "出售处置只该有 asset_bridge 出的 3 张凭证（1606 / 1002 / 2221.02）：\
         多出来就是 GENERATE_VOUCHER 在重复落账"
    );
    assert_eq!(
        notices_of(asset_dto::STATUS_DISPOSED, asset_id).await,
        1,
        "排除清单只排凭证：同状态的 SEND_NOTICE 必须照常发出（排掉通知 = 接线了永远不响）"
    );
    purge().await;
}

/// 以某个真实用户身份造 claims（`asset_dispose_cancel` 带 `#[nik::permission]`，需 SUPER_ADMIN）。
fn claims_of(user: Uuid) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some(format!("lnk09_{}", &user.to_string()[..8])),
        roles: vec!["SUPER_ADMIN".to_string()],
        permissions: vec!["*".to_string()],
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
}
