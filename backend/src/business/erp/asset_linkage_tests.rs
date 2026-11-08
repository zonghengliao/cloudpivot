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
//! # FIN-ASSET-ST02（停用 / 启用）追加 6 条
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | `st02_idle_triggers_once` | 停用（`IN_USE → IDLE`）真的触发 `FIXED_ASSET/IDLE` 规则：状态 + 状态日志 1 行 + 变动留痕 1 行（dept/user 全 NULL）+ 通知 1 条 |
//! | 2 | `st02_enable_triggers_once` | 启用（`IDLE → IN_USE`）触发 `FIXED_ASSET/IN_USE` 规则：change_type=ENABLE + 通知 1 条 |
//! | 3 | `st02_repeat_idle_conflicts` | 重复停用 → **409 / 66046**，通知 / 变动 / 状态日志**全部不变** |
//! | 4 | `st02_excluded_actions_not_run` | `IDLE` 上配 `GENERATE_VOUCHER`：停用后**凭证 0 张**，同状态 `SEND_NOTICE` 照发（证明排除清单有牙，不是规则没命中） |
//! | 5 | `st02_reversible_edges_notify_twice` | 停用 → 启用 → **再停用** = 通知 **3** 条（事件维度幂等键生效） |
//! | 6 | `st02_dispose_restore_dispose_notifies_twice` | 处置 → 作废处置 → **再处置** = `DISPOSED` 通知 **2** 条（LNK-09 遗留 bug 回归） |
//!
//! 停用 / 启用的夹具号段是 `FA-ST02-` + 规则名 `ST02-`，与 LNK-09 的 `FA-LNK09-` / `LNK09-`
//! 分开清（同一个 [`purge`] 两段都删，两条线互不干扰）。
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
use super::asset_dto::{self, AssetDetailRes, CreateAssetReq, StatusChangeReq};
use super::asset_handler::{CancelDisposalReq, asset_dispose_cancel, asset_status_change};
use super::asset_linkage;
use super::asset_model::{self, AssetInsert};
use crate::business::erp::period_test_support as support;
use crate::business::flow::linkage;
use crate::business::flow::linkage::spec;
use crate::business::shared::state_machine;
use crate::shared::error_code;

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
    // FIN-ASSET-ST02 的规则号段（与本文件 LNK09 段分开清，两条线互不干扰）
    exec("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'ST02-%'").await;
    // 用例会把 V0323 的预置规则改成自己的参数再删掉：这里补回迁移口径，
    // 让「跑过测试的库」与「刚跑完迁移的库」一致（见 [`restore_presets`]）。
    restore_presets().await;
    exec("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:FIXED_ASSET:%'").await;
    // 按本文件的两个号段找残留（`FA-LNK09-` / `FA-ST02-` 前缀是本文件独占的卡片号）
    let assets = uuids_of(
        "SELECT id FROM biz_fixed_asset \
          WHERE asset_no LIKE 'FA-LNK09-%' OR asset_no LIKE 'FA-ST02-%'",
    )
    .await;
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
                 OR biz_no LIKE 'FA-ST02-%' \
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
    make_asset_named(creator, "FA-LNK09", "LNK09 夹具笔记本").await
}

/// 同上，但可指定**卡片号段与名称**（FIN-ASSET-ST02 用 `FA-ST02`）。
///
/// 号段分开是为了「两张卡的用例」在各自 `purge` 段里清得干净：LNK-09 的处置用例与
/// ST02 的停用 / 启用用例会并行跑，按同一个前缀清场会互删。
async fn make_asset_named(creator: Uuid, prefix: &str, name: &str) -> (Uuid, String) {
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
    let asset_no = format!("{prefix}-{}", &id.to_string()[..8]);
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

/// 按幂等键前缀数通知条数（资产联动的通知键 = `LNK:FIXED_ASSET:<状态>:<卡片 id>:<变动行 id>`）。
///
/// 注意是**前缀**匹配：键的事件段（变动行 id）每次不同，写等值会永远 0 条（假绿）。
async fn notices_of(status: &str, asset_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key LIKE ?",
        vec![Value::Text(format!("LNK:FIXED_ASSET:{status}:{asset_id}%"))],
    )
    .await
    .expect("查联动通知")
}

/// 某资产名下**所有状态**的通知条数（可逆边用例：停用 / 启用 / 再停用 = 3 条）。
async fn notices_total(asset_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key LIKE ?",
        vec![Value::Text(format!("LNK:FIXED_ASSET:%:{asset_id}%"))],
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

/// 插 / 改一条 `FIXED_ASSET/<status>` 上的规则（绕过 API 写库：与「管理员在配置页配好」等价）。
async fn add_rule(status: &str, action: &str, params: &str) {
    add_rule_named("LNK09", status, action, params).await;
}

/// 同上，规则名前缀可变（`ST02-` = 停用 / 启用用例的号段，清场时按前缀删）。
///
/// 用 `ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE` 而不是裸 INSERT：
/// V0323 起迁移**预置**了 FIXED_ASSET 三个状态上的 `SEND_NOTICE` 规则，同一触发点同一动作码
/// 再插一行会撞 `uk_flow_linkage_rule`（用例直接红）。冲突时按用例给的参数覆盖，
/// 用例收尾由 [`purge`] 删掉、再 [`restore_presets`] 把预置行补回去。
async fn add_rule_named(prefix: &str, status: &str, action: &str, params: &str) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    exec(&format!(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), '{prefix}-{status}-{action}-{nanos}', 'FIXED_ASSET', \
                 '{status}', '{action}', '{params}'::jsonb, 10) \
         ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE \
            SET name = EXCLUDED.name, action_params = EXCLUDED.action_params, \
                enabled = true, updated_at = now(), deleted_at = NULL"
    ))
    .await;
}

/// 把 V0323 预置的 3 条 FIXED_ASSET 规则刷回迁移口径（[`purge`] 的收尾）。
///
/// 为什么需要：本文件的用例要「这个触发点上有一条我配的规则」，而 V0323 之后这三个
/// 触发点上**已经有预置规则**（唯一键）—— 用例先用 `ON CONFLICT DO UPDATE` 把它改成
/// 自己的参数，收尾再按 `LNK09-` / `ST02-` 前缀删掉。删完不补回来的话，跑过测试的库
/// 就与「刚跑完迁移的库」不一致（再断言「FIXED_ASSET 有 3 条规则」就会红）。
async fn restore_presets() {
    exec(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, enabled, seq) \
         VALUES \
            (gen_random_uuid(), '资产停用 → 通知制单人', 'FIXED_ASSET', 'IDLE', 'SEND_NOTICE', \
             '{\"mode\": \"TAKEOVER\"}'::jsonb, true, 10), \
            (gen_random_uuid(), '资产启用 / 作废处置 → 通知制单人', 'FIXED_ASSET', 'IN_USE', 'SEND_NOTICE', \
             '{\"mode\": \"TAKEOVER\"}'::jsonb, true, 10), \
            (gen_random_uuid(), '资产处置 → 通知制单人', 'FIXED_ASSET', 'DISPOSED', 'SEND_NOTICE', \
             '{\"mode\": \"TAKEOVER\"}'::jsonb, true, 10) \
         ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE \
            SET name = EXCLUDED.name, action_params = EXCLUDED.action_params, \
                enabled = true, updated_at = now(), deleted_at = NULL",
    )
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
        Uuid::now_v7(),
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
        Uuid::now_v7(),
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

// ==================== FIN-ASSET-ST02：停用 / 启用（IN_USE ⇄ IDLE） ====================

/// 停用 / 启用用例的卡片号段与规则号段（`purge` 按它们清场）。
const ST02_ASSET_PREFIX: &str = "FA-ST02";
const ST02_RULE_PREFIX: &str = "ST02";

/// 走**真实 handler** 停用 / 启用（锁卡 + 同态 409 + 状态机 + CAS + 留痕 + 联动）。
///
/// 刻意不直接调 `asset_linkage`：本卡要证明的是「配在 `FIXED_ASSET/IDLE` 上的规则由这个
/// HTTP 入口触发」，绕开 handler 就等于绕开被测对象。
async fn status_change(
    user: Uuid,
    asset_id: Uuid,
    target_status: &str,
    reason: &str,
) -> std::result::Result<AssetDetailRes, ApiError> {
    // ATT-06：启用（→ IN_USE）前必须已有发票 / 验收单，先挂一份 pdf 夹具再走真实 handler
    if target_status == asset_dto::STATUS_IN_USE {
        crate::business::attachment::test_support::attach_pdf(
            crate::business::attachment::model::DocKind::FixedAsset,
            asset_id,
        )
        .await;
    }
    let res = asset_status_change(
        Extension(AuthClaims(claims_of(user))),
        Path(asset_id.to_string()),
        Json(StatusChangeReq {
            target_status: Some(target_status.to_string()),
            change_date: None,
            reason: Some(reason.to_string()),
        }),
    )
    .await?;
    Ok(res.data.expect("停用 / 启用必须返回资产详情"))
}

/// 资产的状态日志（按写入顺序的 `(from, to)`）。
///
/// 夹具建卡走 `asset_model::asset_insert` 直插、不写状态日志，所以这里只会看到
/// 停用 / 启用 / 处置自身的流转行。
async fn status_logs_of(asset_id: Uuid) -> Vec<(String, String)> {
    let rows = db()
        .query_sql(
            "SELECT from_status, to_status FROM biz_status_log \
              WHERE entity_type = 'FIXED_ASSET' AND entity_id = ?::uuid \
              ORDER BY created_at, id"
                .to_string(),
            vec![Value::from(&asset_id)],
        )
        .await
        .expect("查状态日志");
    rows.iter()
        .map(|row| {
            (
                row.get_typed::<Option<String>>("from_status")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
                row.get_typed::<Option<String>>("to_status")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// 变动留痕的一行（只取本卡断言用得到的字段）。
struct ChangeRow {
    /// 变动类型（`IDLE` / `ENABLE` / `DISPOSAL` …）。
    change_type: String,
    /// 原状态。
    from_status: Option<String>,
    /// 新状态。
    to_status: Option<String>,
    /// 原因。
    reason: String,
    /// 部门与使用人四组是否**全为 NULL**（停用 / 启用必须全 NULL：变动的是状态）。
    people_null: bool,
}

/// 某资产的变动时间线（按写入顺序）。
async fn changes_of(asset_id: Uuid) -> Vec<ChangeRow> {
    let rows = db()
        .query_sql(
            "SELECT change_type, from_status, to_status, reason, \
                    (from_dept_id IS NULL AND to_dept_id IS NULL \
                     AND from_user_id IS NULL AND to_user_id IS NULL) AS people_null \
               FROM biz_fixed_asset_change WHERE asset_id = ?::uuid \
              ORDER BY created_at, id"
                .to_string(),
            vec![Value::from(&asset_id)],
        )
        .await
        .expect("查变动留痕");
    rows.iter()
        .map(|row| ChangeRow {
            change_type: row.get_typed("change_type").expect("change_type"),
            from_status: row.get_typed("from_status").expect("from_status"),
            to_status: row.get_typed("to_status").expect("to_status"),
            reason: row.get_typed("reason").expect("reason"),
            people_null: row.get_typed("people_null").expect("people_null"),
        })
        .collect()
}

/// 某资产名下**任何来源**的凭证条数（停用 / 启用必须 0 张）。
async fn vouchers_of(asset_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&asset_id)],
    )
    .await
    .expect("查凭证")
}

/// ① 停用：`IN_USE → IDLE` 必须触发 `FIXED_ASSET/IDLE` 上的规则，且落 1 行状态日志 +
/// 1 行变动留痕（部门 / 使用人全 NULL、原因落库），**不落凭证**。
#[tokio::test]
async fn st02_idle_triggers_once() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let (asset_id, asset_no) = make_asset_named(user, ST02_ASSET_PREFIX, "ST02 停用夹具").await;
    add_rule_named(
        ST02_RULE_PREFIX,
        asset_dto::STATUS_IDLE,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;
    let reason = "生产线搬迁，设备暂不外借";

    let detail = status_change(user, asset_id, asset_dto::STATUS_IDLE, reason)
        .await
        .unwrap_or_else(|err| panic!("停用失败：{} {}", err.code, err.msg));
    assert_eq!(
        detail.asset.status,
        asset_dto::STATUS_IDLE,
        "停用后接口返回的详情里状态必须是 IDLE（前端拿它就刷新列表与时间线）"
    );
    assert_eq!(
        status_of(asset_id).await.as_deref(),
        Some(asset_dto::STATUS_IDLE),
        "停用后卡片状态必须是 IDLE"
    );

    let logs = status_logs_of(asset_id).await;
    assert_eq!(
        logs,
        vec![(
            asset_dto::STATUS_IN_USE.to_string(),
            asset_dto::STATUS_IDLE.to_string()
        )],
        "状态日志必须恰好一行 IN_USE → IDLE：{logs:?}"
    );

    let changes = changes_of(asset_id).await;
    assert_eq!(changes.len(), 1, "停用必须恰好写一行变动留痕：{asset_no}");
    let row = &changes[0];
    assert_eq!(
        row.change_type,
        asset_dto::CHANGE_IDLE,
        "停用的变动类型是 IDLE"
    );
    assert_eq!(row.from_status.as_deref(), Some(asset_dto::STATUS_IN_USE));
    assert_eq!(row.to_status.as_deref(), Some(asset_dto::STATUS_IDLE));
    assert_eq!(row.reason, reason, "原因必须原样落库（内控留痕）");
    assert!(
        row.people_null,
        "停用变动的是状态，不是部门 / 使用人：四组必须全 NULL"
    );

    assert_eq!(
        notices_of(asset_dto::STATUS_IDLE, asset_id).await,
        1,
        "FIXED_ASSET/IDLE 上配的 SEND_NOTICE 必须真的发出一条通知"
    );
    assert_eq!(
        vouchers_of(asset_id).await,
        0,
        "停用不落凭证（折旧口径也不变，见设计文档 §4）"
    );
    purge().await;
}

/// ② 启用：`IDLE → IN_USE` 触发 `FIXED_ASSET/IN_USE` 上的规则，留痕 `change_type=ENABLE`。
#[tokio::test]
async fn st02_enable_triggers_once() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let (asset_id, asset_no) = make_asset_named(user, ST02_ASSET_PREFIX, "ST02 启用夹具").await;
    add_rule_named(
        ST02_RULE_PREFIX,
        asset_dto::STATUS_IN_USE,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;
    status_change(
        user,
        asset_id,
        asset_dto::STATUS_IDLE,
        "先停用，便于验证启用",
    )
    .await
    .unwrap_or_else(|err| panic!("前置停用失败：{} {}", err.code, err.msg));
    assert_eq!(
        notices_of(asset_dto::STATUS_IN_USE, asset_id).await,
        0,
        "停用不许触发 IN_USE 的规则（触发点按**变更后**的状态匹配）"
    );

    status_change(user, asset_id, asset_dto::STATUS_IN_USE, "恢复使用")
        .await
        .unwrap_or_else(|err| panic!("启用失败：{} {}", err.code, err.msg));
    assert_eq!(
        status_of(asset_id).await.as_deref(),
        Some(asset_dto::STATUS_IN_USE),
        "启用后卡片必须回到在役"
    );
    let changes = changes_of(asset_id).await;
    assert_eq!(changes.len(), 2, "停用 + 启用 = 两行留痕：{asset_no}");
    assert_eq!(changes[0].change_type, asset_dto::CHANGE_IDLE);
    let enable = &changes[1];
    assert_eq!(
        enable.change_type,
        asset_dto::CHANGE_ENABLE,
        "启用的变动类型是 ENABLE"
    );
    assert_eq!(enable.from_status.as_deref(), Some(asset_dto::STATUS_IDLE));
    assert_eq!(enable.to_status.as_deref(), Some(asset_dto::STATUS_IN_USE));
    assert!(enable.people_null, "启用同样不动部门 / 使用人");
    assert_eq!(
        notices_of(asset_dto::STATUS_IN_USE, asset_id).await,
        1,
        "FIXED_ASSET/IN_USE 上配的 SEND_NOTICE 必须由「启用」触发一次"
    );
    assert_eq!(vouchers_of(asset_id).await, 0, "启用也不落凭证");
    purge().await;
}

/// ③ 重复停用 → **409 / 66046**，且通知 / 变动留痕 / 状态日志**一条都不增**。
///
/// 这条是「同态判定」的守卫：删掉判定，第二次停用会走完状态机（`from == to` 被
/// `transition` 的刹车拒掉是另一层，但那时报的是状态机的错，不是 66046）与联动。
#[tokio::test]
async fn st02_repeat_idle_conflicts() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let (asset_id, _asset_no) =
        make_asset_named(user, ST02_ASSET_PREFIX, "ST02 重复停用夹具").await;
    add_rule_named(
        ST02_RULE_PREFIX,
        asset_dto::STATUS_IDLE,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;
    status_change(user, asset_id, asset_dto::STATUS_IDLE, "首次停用")
        .await
        .unwrap_or_else(|err| panic!("首次停用失败：{} {}", err.code, err.msg));

    let notices_before = notices_total(asset_id).await;
    let changes_before = changes_of(asset_id).await.len();
    let logs_before = status_logs_of(asset_id).await.len();
    assert_eq!(notices_before, 1, "前置：首次停用必须发 1 条");

    let err = status_change(user, asset_id, asset_dto::STATUS_IDLE, "重复点击")
        .await
        .expect_err("重复停用必须被拒（同态不是状态变更）");
    assert_eq!(err.status, 409, "重复停用是状态冲突（409），不是参数错误");
    assert_eq!(
        err.code,
        error_code::ERP_ASSET_STATUS_ILLEGAL,
        "必须用 66046（66040 是「处置审批在跑」，会把排障指错方向）：{}",
        err.msg
    );
    assert!(
        err.msg.contains("闲置"),
        "报文要点名当前状态的中文名：{}",
        err.msg
    );

    assert_eq!(
        status_of(asset_id).await.as_deref(),
        Some(asset_dto::STATUS_IDLE),
        "被拒的重复停用不许改坏状态"
    );
    assert_eq!(
        notices_total(asset_id).await,
        notices_before,
        "重复停用不许再发通知"
    );
    assert_eq!(
        changes_of(asset_id).await.len(),
        changes_before,
        "重复停用不许再写变动留痕"
    );
    assert_eq!(
        status_logs_of(asset_id).await.len(),
        logs_before,
        "重复停用不许再写状态日志"
    );
    purge().await;
}

/// ④ 排除清单有牙：`IDLE` 上配 `GENERATE_VOUCHER` → 停用后**凭证 0 张**；
/// 同状态的 `SEND_NOTICE` 照发（证明规则真的被引擎读到了，不是「没命中」的虚绿）。
#[tokio::test]
async fn st02_excluded_actions_not_run() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let (asset_id, _asset_no) =
        make_asset_named(user, ST02_ASSET_PREFIX, "ST02 排除动作夹具").await;
    add_rule_named(
        ST02_RULE_PREFIX,
        asset_dto::STATUS_IDLE,
        spec::ACTION_GENERATE_VOUCHER,
        r#"{"biz_type":"STOCK_IN","mode":"TAKEOVER"}"#,
    )
    .await;
    add_rule_named(
        ST02_RULE_PREFIX,
        asset_dto::STATUS_IDLE,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;

    // 引擎级对照：同一份真实规则行 + 同一上下文，带/不带排除清单
    let ctx = linkage::LinkageContext::new(
        state_machine::ENTITY_FIXED_ASSET,
        asset_dto::STATUS_IDLE,
        asset_id,
        "FA-ST02-CTX",
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
        "对照：不带排除清单时这条 GENERATE_VOUCHER 规则必须被读到并执行"
    );
    assert!(
        with_exclusion
            .runs
            .iter()
            .all(|item| item.action_code != spec::ACTION_GENERATE_VOUCHER),
        "带排除清单时 GENERATE_VOUCHER 必须整条不跑：{:?}",
        with_exclusion
            .runs
            .iter()
            .map(|item| item.action_code.as_str())
            .collect::<Vec<_>>()
    );

    // 端到端：走真实 handler 停用
    status_change(user, asset_id, asset_dto::STATUS_IDLE, "停用并验证不落凭证")
        .await
        .unwrap_or_else(|err| panic!("停用失败：{} {}", err.code, err.msg));
    assert_eq!(
        vouchers_of(asset_id).await,
        0,
        "停用 / 启用一律不落凭证：通用引擎的 GENERATE_VOUCHER 必须被排除清单拦住"
    );
    assert_eq!(
        notices_of(asset_dto::STATUS_IDLE, asset_id).await,
        1,
        "排除清单只排凭证：同状态的 SEND_NOTICE 必须照发（否则就是「接线了永远不响」）"
    );
    purge().await;
}

/// ⑤ 可逆边：停用 → 启用 → **再停用** = 通知 **3** 条。
///
/// 事件维度幂等键的回归守卫：旧键只有「状态 + 卡片」，第二次停用会撞第一次的键被
/// `ON CONFLICT DO NOTHING` 吞掉（left 2 / right 3）。
#[tokio::test]
async fn st02_reversible_edges_notify_twice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let (asset_id, _asset_no) = make_asset_named(user, ST02_ASSET_PREFIX, "ST02 可逆边夹具").await;
    for status in [asset_dto::STATUS_IDLE, asset_dto::STATUS_IN_USE] {
        add_rule_named(
            ST02_RULE_PREFIX,
            status,
            spec::ACTION_SEND_NOTICE,
            NOTICE_CREATOR,
        )
        .await;
    }

    for (target, reason) in [
        (asset_dto::STATUS_IDLE, "第一次停用"),
        (asset_dto::STATUS_IN_USE, "再启用"),
        (asset_dto::STATUS_IDLE, "第二次停用"),
    ] {
        status_change(user, asset_id, target, reason)
            .await
            .unwrap_or_else(|err| panic!("{} 失败：{} {}", target, err.code, err.msg));
    }

    assert_eq!(
        status_of(asset_id).await.as_deref(),
        Some(asset_dto::STATUS_IDLE),
        "三步走完必须是 IDLE"
    );
    assert_eq!(
        notices_total(asset_id).await,
        3,
        "停用 → 启用 → 再停用必须各发一条（共 3 条）：事件维度幂等键"
    );
    assert_eq!(
        notices_of(asset_dto::STATUS_IDLE, asset_id).await,
        2,
        "两次停用是两个不同事件（不同变动行），必须两条通知"
    );
    assert_eq!(
        notices_of(asset_dto::STATUS_IN_USE, asset_id).await,
        1,
        "启用一条"
    );
    assert_eq!(
        changes_of(asset_id).await.len(),
        3,
        "三行变动留痕（IDLE / ENABLE / IDLE）"
    );
    purge().await;
}

/// ⑥ LNK-09 遗留 bug 回归：处置 → 作废处置 → **再处置** = `DISPOSED` 通知 **2** 条。
///
/// 两次处置的目标态相同、卡片相同，旧幂等键（状态 + 卡片）下第二次必被吞。
#[tokio::test]
async fn st02_dispose_restore_dispose_notifies_twice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let (asset_id, asset_no) = make_asset_named(user, ST02_ASSET_PREFIX, "ST02 二次处置夹具").await;
    add_rule_named(
        ST02_RULE_PREFIX,
        asset_dto::STATUS_DISPOSED,
        spec::ACTION_SEND_NOTICE,
        NOTICE_CREATOR,
    )
    .await;

    let first = seed_approved_instance(asset_id, &asset_no, user).await;
    assert_eq!(
        asset_bridge::on_disposal_settled(Some(first), "APPROVED").await,
        Some(asset_id),
        "第一次处置必须成功"
    );
    assert_eq!(
        notices_of(asset_dto::STATUS_DISPOSED, asset_id).await,
        1,
        "第一次处置发一条"
    );

    asset_dispose_cancel(
        Extension(AuthClaims(claims_of(user))),
        Path(asset_id.to_string()),
        Json(CancelDisposalReq {
            reason: Some("ST02 用例：处置依据不成立，作废".to_string()),
        }),
    )
    .await
    .unwrap_or_else(|err| panic!("作废处置失败：{} {}", err.code, err.msg));
    assert_eq!(
        status_of(asset_id).await.as_deref(),
        Some(asset_dto::STATUS_IN_USE),
        "作废处置后恢复在役"
    );

    let second = seed_approved_instance(asset_id, &asset_no, user).await;
    assert_eq!(
        asset_bridge::on_disposal_settled(Some(second), "APPROVED").await,
        Some(asset_id),
        "再处置必须成功（作废后卡片已回到在役）"
    );
    assert_eq!(
        notices_of(asset_dto::STATUS_DISPOSED, asset_id).await,
        2,
        "处置 → 作废 → 再处置必须发两条 DISPOSED 通知（旧键会只剩 1 条：LNK-09 遗留 bug）"
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
