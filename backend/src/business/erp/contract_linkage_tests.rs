//! 合同联动接线（`CONTRACT`，LNK-07）的**真库**用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 SKIP（与本仓其它 ERP 用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//!   cargo test --lib business::erp::contract_linkage_tests -- --test-threads=1
//! ```
//!
//! # 覆盖（卡面「审批通过只触发一次 / 重复触发被抑制 / 排除动作确实不跑」逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | `lnk07_submit_triggers_pending_approval_notice` | 提交审批（DRAFT → PENDING_APPROVAL）真的触发通知 |
//! | 2 | `lnk07_approval_triggers_once_and_repeat_is_suppressed` | 审批通过（→ SIGNED）触发一次；**同一实例再回调一次不重复触发**（状态 + 通知都不动） |
//! | 3 | `lnk07_rejected_approval_reverts_and_triggers` | 驳回（→ DRAFT）触发（`CONTRACT/DRAFT` 的规则不会永远不响） |
//! | 4 | `lnk07_excluded_generate_voucher_never_runs` | 合同上配 `GENERATE_VOUCHER`：通用引擎**不执行**它（同状态的 `SEND_NOTICE` 照跑），合同名下 0 张凭证 |
//! | 5 | `lnk07_manual_transitions_trigger_once` | 生效 / 终止两条人工边各触发一次；重复点第二次被 409 挡住，通知不增 |
//!
//! # 夹具口径
//!
//! * 账号 / 供应商用固定 UUID（`a2a7` 号段，本模块独占），合同名 `LNK07-` 前缀；
//! * 清场按 **id 集合 / 本模块号段** 删（审批实例 / 待办 / 通知 / 状态日志 / 合同 / 供应商 / 账号），
//!   不做「按 biz_type 全库删」这类越权写；
//! * 联动规则与通知按 `LNK07-` / `LNK:CONTRACT:%` 前缀清（它们没有单据主键可依）。
//!
//! **串行**（`--test-threads=1`）：联动规则是全表唯一索引、审批实例共用同一批角色，并行会互删。

use std::str::FromStr;

use axum::Json;
use axum::extract::{Extension, Path};
use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use super::contract_bridge;
use super::contract_dto::{ContractDetailRes, ContractStatusReq, CreateContractReq};
use super::contract_handler::{contract_change_status, contract_create};
use super::contract_model;
use crate::business::flow::linkage;
use crate::business::flow::linkage::model as linkage_model;
use crate::business::flow::linkage::{engine as linkage_engine, spec};
use crate::business::shared::state_machine;
use crate::shared::error_code;

/// 夹具账号（制单人 / 操作人 / 审批实例发起人）。
const U_MAKER: &str = "a2a70001-0000-4000-8000-000000000001";
/// 夹具供应商。
const SUPPLIER: &str = "a2a70010-0000-4000-8000-000000000010";

/// 真库用例必须串行（共用全局 `db()`、同一批审批实例与 `biz_seq`）。
/// 与 `contract_approval_tests` 用**同一把锁**：两者都会推进 `contract_approve` 实例。
use crate::business::erp::fixture_hygiene::SEED_ROLE_LOCK as DB_LOCK;

fn uid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("金额字面量非法")
}

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期夹具")
}

fn uniq() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}{}", now % 1_000_000_000_000, std::process::id())
}

/// 取响应体（`Res<T>` 的 `data`）。
fn data<T: Serialize>(res: Res<T>) -> T {
    res.data.expect("响应体应带 data")
}

/// 以夹具账号身份造 claims（`SUPER_ADMIN`：入口都要过 `#[nik::permission]`）。
fn claims_of(user: &str) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some(format!("lnk07_{}", &user[..8])),
        roles: vec!["SUPER_ADMIN".to_string()],
        permissions: vec!["*".to_string()],
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
}

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
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.max_lifetime = Some(std::time::Duration::ZERO);
            config.pool.idle_timeout = Some(std::time::Duration::ZERO);
            match Db::connect_with(&url, config).await {
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

async fn exec(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

async fn ids_of(query: &str) -> Vec<Uuid> {
    let rows = db()
        .query_sql(query.to_string(), vec![])
        .await
        .unwrap_or_else(|err| panic!("查夹具 id 失败：{err}\nSQL: {query}"));
    rows.iter()
        .map(|row| row.get_typed::<Uuid>("id").expect("夹具 id"))
        .collect()
}

fn uuid_in(ids: &[Uuid]) -> String {
    ids.iter()
        .map(|id| format!("'{id}'::uuid"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn text_in(ids: &[Uuid]) -> String {
    ids.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(", ")
}

/// 本模块夹具的合同 id 集合（`LNK07-` 是模块独占的合同名号段）。
async fn fixture_contract_ids() -> Vec<Uuid> {
    ids_of("SELECT id FROM biz_contract WHERE contract_name LIKE 'LNK07-%'").await
}

async fn fixture_instance_ids() -> Vec<Uuid> {
    ids_of(
        "SELECT i.id FROM biz_flow_instance i \
          WHERE i.biz_type = 'CONTRACT_APPROVE' \
            AND i.biz_id IN (SELECT c.id FROM biz_contract c \
                              WHERE c.contract_name LIKE 'LNK07-%')",
    )
    .await
}

/// 清联动夹具（规则 + 本卡发过的通知），不动业务单据。
async fn clear_rules() {
    exec("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK07%'").await;
    exec("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:CONTRACT:%'").await;
}

/// 清场：通知 → 审批实例 → 状态日志 → 合同 → 供应商 / 账号（**只删本模块的 id 集合**）。
async fn purge() {
    clear_rules().await;
    let contracts = fixture_contract_ids().await;
    let instances = fixture_instance_ids().await;
    if !instances.is_empty() {
        let notices = ids_of(&format!(
            "SELECT id FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN ({})",
            text_in(&instances)
        ))
        .await;
        if !notices.is_empty() {
            exec(&format!(
                "DELETE FROM sys_notice_receiver WHERE notice_id IN ({})",
                uuid_in(&notices)
            ))
            .await;
            exec(&format!("DELETE FROM sys_notice WHERE id IN ({})", uuid_in(&notices))).await;
        }
        exec(&format!(
            "UPDATE biz_flow_instance SET prev_instance_id = NULL WHERE prev_instance_id IN ({})",
            uuid_in(&instances)
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_flow_instance WHERE id IN ({})",
            uuid_in(&instances)
        ))
        .await;
    }
    if !contracts.is_empty() {
        // 状态日志无外键，必须抢在删单据之前（否则再也定位不到）
        exec(&format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'CONTRACT' AND entity_id IN ({})",
            uuid_in(&contracts)
        ))
        .await;
        // 本模块合同名下不该有凭证（有 = 排除清单失效），先按来源删干净再删合同
        exec(&format!(
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_id IN ({}))",
            uuid_in(&contracts)
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_voucher WHERE source_id IN ({})",
            uuid_in(&contracts)
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_contract WHERE id IN ({})",
            uuid_in(&contracts)
        ))
        .await;
    }
    exec(&format!(
        "DELETE FROM biz_supplier WHERE id = '{}'::uuid",
        uid(SUPPLIER)
    ))
    .await;
    exec(&format!(
        "DELETE FROM sys_user_role WHERE user_id = '{}'::uuid",
        uid(U_MAKER)
    ))
    .await;
    exec(&format!("DELETE FROM sys_user WHERE id = '{}'::uuid", uid(U_MAKER))).await;
}

/// 拿锁 + 连库 + 清上一轮 + 建夹具（供应商 + 一个真实账号）。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    purge().await;
    exec(&format!(
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, credit_days) \
         VALUES ('{SUPPLIER}'::uuid, 'LNK07-SUP', 'LNK07 合同联动测试供应商', 1, 30) \
         ON CONFLICT DO NOTHING"
    ))
    .await;
    exec(&format!(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES ('{U_MAKER}'::uuid, 'lnk07_maker', 'x', 'LNK07 制单人', 1) \
         ON CONFLICT DO NOTHING"
    ))
    .await;
    Some(guard)
}

/// 建一张草稿合同（三个日期齐全：审批通过会直接落 SIGNED）。
async fn make_contract(tag: &str) -> Uuid {
    let req = CreateContractReq {
        name: Some(format!("LNK07-{tag}-{}", uniq())),
        contract_type: Some("PURCHASE".to_string()),
        party_b_id: Some(SUPPLIER.to_string()),
        amount: Some(dec("50000")),
        sign_date: Some(date("2026-01-01")),
        start_date: Some(date("2026-01-02")),
        end_date: Some(date("2026-12-31")),
        remark: Some("contract_linkage_tests 夹具".to_string()),
        ..Default::default()
    };
    let detail: ContractDetailRes = data(
        contract_create(Extension(AuthClaims(claims_of(U_MAKER))), Json(req))
            .await
            .unwrap_or_else(|err| panic!("建合同失败：{} {}", err.code, err.msg)),
    );
    Uuid::parse_str(&detail.base.id).expect("合同 id 非法")
}

/// 走真实 handler 推一次状态（`PATCH /status` 的唯一入口）。
async fn patch_status(
    id: Uuid,
    target: &str,
    reason: Option<&str>,
) -> std::result::Result<ContractDetailRes, ApiError> {
    contract_change_status(
        Extension(AuthClaims(claims_of(U_MAKER))),
        Path(id.to_string()),
        Json(ContractStatusReq {
            target_status: Some(target.to_string()),
            reason: reason.map(str::to_string),
            ..Default::default()
        }),
    )
    .await
    .map(data)
}

/// 某合同的**在跑**审批实例 id。
async fn running_instance(contract_id: Uuid) -> Uuid {
    db().scalar_with_params::<Option<Uuid>>(
        "SELECT id FROM biz_flow_instance \
          WHERE biz_type = 'CONTRACT_APPROVE' AND biz_id = ?::uuid \
            AND status = 'PENDING' AND deleted_at IS NULL",
        vec![Value::from(&contract_id)],
    )
    .await
    .expect("查审批实例失败")
    .expect("提交后应当有在跑的审批实例")
}

/// 把实例判成终态（模拟流程引擎 GUE 走完；业务回调由用例自己调）。
async fn settle_instance(instance_id: Uuid, status: &str) {
    exec(&format!(
        "UPDATE biz_flow_instance SET status = '{status}', ended_at = now(), revision = revision + 1 \
          WHERE id = '{instance_id}'::uuid"
    ))
    .await;
}

/// 提交审批并按需求把实例判成 `APPROVED`（不真的走角色会签：审批引擎本身由
/// `contract_approval_tests` 覆盖，本模块只验「合同侧的联动接线」）。
async fn submit_and_approve(contract_id: Uuid) -> Uuid {
    patch_status(contract_id, contract_model::STATUS_PENDING_APPROVAL, None)
        .await
        .unwrap_or_else(|err| panic!("提交审批失败：{} {}", err.code, err.msg));
    let instance = running_instance(contract_id).await;
    settle_instance(instance, "APPROVED").await;
    instance
}

async fn status_of(contract_id: Uuid) -> String {
    contract_model::contract_find(contract_id)
        .await
        .expect("查合同")
        .expect("合同存在")
        .status
}

/// 插一条 `CONTRACT/<status>` 上的规则（绕过 API 写库：与「管理员在配置页配好」等价）。
async fn add_rule(status: &str, action: &str, params: &str) {
    exec(&format!(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), 'LNK07-{status}-{action}-{}', 'CONTRACT', '{status}', \
                 '{action}', '{params}'::jsonb, 10)",
        uniq()
    ))
    .await;
}

/// 按幂等键数通知条数。
async fn notices_of(status: &str, contract_id: Uuid) -> i64 {
    let key = format!("LNK:CONTRACT:{status}:{contract_id}");
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key = ?",
        vec![Value::Text(key)],
    )
    .await
    .expect("查联动通知")
}

/// 某合同名下的凭证条数（不分来源类型：防「来源类型写错」的重复落账）。
async fn vouchers_of(contract_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&contract_id)],
    )
    .await
    .expect("查合同凭证")
}

// ==================== ① 提交审批触发 ====================

/// 提交审批（`DRAFT → PENDING_APPROVAL`）也触发联动：不接这一条，
/// `CONTRACT/PENDING_APPROVAL` 上配的规则「能保存、能启用、永远不触发」。
#[tokio::test]
async fn lnk07_submit_triggers_pending_approval_notice() {
    let Some(_guard) = setup().await else {
        return;
    };
    clear_rules().await;
    let contract_id = make_contract("submit").await;
    add_rule(
        contract_model::STATUS_PENDING_APPROVAL,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    let _instance = submit_and_approve(contract_id).await;

    assert_eq!(
        notices_of(contract_model::STATUS_PENDING_APPROVAL, contract_id).await,
        1,
        "CONTRACT/PENDING_APPROVAL 上配的 SEND_NOTICE 必须真的发出一条通知"
    );
    purge().await;
}

// ==================== ② 审批通过只触发一次（重复回调被抑制） ====================

/// **卡面的核心用例**：审批通过（`PENDING_APPROVAL → SIGNED`）触发一次联动；
/// 同一个实例**再回调一次**（重复回调 / 双击 approve / 重放）必须被抑制：
/// * 合同状态不再变（已经是 SIGNED，`settle_guard` 判 `AlreadySettled`）；
/// * 通知不增（幂等键之外，主要是 `settle()` 在 `Ok(None)` 就返回，根本不会再跑引擎）。
#[tokio::test]
async fn lnk07_approval_triggers_once_and_repeat_is_suppressed() {
    let Some(_guard) = setup().await else {
        return;
    };
    clear_rules().await;
    let contract_id = make_contract("approve").await;
    add_rule(
        contract_model::STATUS_SIGNED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    let instance = submit_and_approve(contract_id).await;
    assert_eq!(
        status_of(contract_id).await,
        contract_model::STATUS_PENDING_APPROVAL,
        "夹具：提交审批后合同应在 PENDING_APPROVAL（回调前）"
    );

    // 第一次回调：真的推进 + 真的触发
    let first = contract_bridge::on_contract_settled(Some(instance), "APPROVED").await;
    assert_eq!(first, Some(contract_id), "审批通过回调必须推进这张合同");
    assert_eq!(
        status_of(contract_id).await,
        contract_model::STATUS_SIGNED,
        "审批通过 → 已签订"
    );
    assert_eq!(
        notices_of(contract_model::STATUS_SIGNED, contract_id).await,
        1,
        "CONTRACT/SIGNED 上配的 SEND_NOTICE 必须恰好发一条"
    );

    // 第二次回调（同一实例 + 同一结论）：必须被抑制
    let second = contract_bridge::on_contract_settled(Some(instance), "APPROVED").await;
    assert_eq!(
        second, None,
        "重复回调必须被 settle_guard 挡掉（AlreadySettled）：合同已是 SIGNED，不该再有第二次落状态"
    );
    assert_eq!(
        status_of(contract_id).await,
        contract_model::STATUS_SIGNED,
        "重复回调不许把状态改坏"
    );
    assert_eq!(
        notices_of(contract_model::STATUS_SIGNED, contract_id).await,
        1,
        "重复回调**不许**再发一条通知（同一 instance + 同一状态只触发一次）"
    );
    // 第三次：换一个实例号重放同一结论，同样不该触发 —— 判据是「状态列 CAS + settle_guard」，
    // 不是「这个实例号之前来过没有」。
    let other_instance = Uuid::now_v7();
    assert_eq!(
        contract_bridge::on_contract_settled(Some(other_instance), "APPROVED").await,
        None,
        "不存在的实例一律不推进（find_instance → None）"
    );
    assert_eq!(
        notices_of(contract_model::STATUS_SIGNED, contract_id).await,
        1,
        "换了实例号的重放也不许再发通知"
    );
    purge().await;
}

// ==================== ③ 驳回 → 草稿触发 ====================

/// 驳回 / 撤回（`PENDING_APPROVAL → DRAFT`）同样触发：`CONTRACT/DRAFT` 上的规则
/// 不能「永远不响」（回草稿后要通知制单人「被打回了，去改」）。
#[tokio::test]
async fn lnk07_rejected_approval_reverts_and_triggers() {
    let Some(_guard) = setup().await else {
        return;
    };
    clear_rules().await;
    let contract_id = make_contract("reject").await;
    add_rule(
        contract_model::STATUS_DRAFT,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    patch_status(contract_id, contract_model::STATUS_PENDING_APPROVAL, None)
        .await
        .unwrap_or_else(|err| panic!("提交审批失败：{} {}", err.code, err.msg));
    let instance = running_instance(contract_id).await;
    settle_instance(instance, "REJECTED").await;

    assert_eq!(
        contract_bridge::on_contract_settled(Some(instance), "REJECTED").await,
        Some(contract_id),
        "驳回应把合同打回草稿"
    );
    assert_eq!(status_of(contract_id).await, contract_model::STATUS_DRAFT);
    assert_eq!(
        notices_of(contract_model::STATUS_DRAFT, contract_id).await,
        1,
        "CONTRACT/DRAFT 上配的 SEND_NOTICE 必须真的发一条（驳回通知制单人）"
    );
    purge().await;
}

// ==================== ④ 排除动作确实不跑 ====================

/// 合同上配 `GENERATE_VOUCHER`：通用引擎**不执行**它。
///
/// 两层证据：
///
/// 1. **引擎级**（同一份真实规则行、同一个上下文）：`run_excluding(CONTRACT_EXCLUDED_ACTIONS)`
///    里那条规则**不产生任何 RuleRun**（`is_routed_elsewhere` → `continue`），
///    而不带排除清单的 `run` 会产生一条 `SKIPPED`（「只接管收货入库」）——
///    说明这条规则**确实被读到了**，只是被排除清单拦下（不是「规则没生效」这种虚绿）；
/// 2. **端到端**：审批通过后合同名下 0 张凭证，同状态配的 `SEND_NOTICE` 照常发出
///    （排除清单只排凭证，不许把通知一起排掉）。
#[tokio::test]
async fn lnk07_excluded_generate_voucher_never_runs() {
    let Some(_guard) = setup().await else {
        return;
    };
    clear_rules().await;
    let contract_id = make_contract("excluded").await;
    add_rule(
        contract_model::STATUS_SIGNED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;
    add_rule(
        contract_model::STATUS_SIGNED,
        spec::ACTION_GENERATE_VOUCHER,
        r#"{"biz_type":"STOCK_IN","mode":"TAKEOVER"}"#,
    )
    .await;

    // ---- 第一层：引擎级对照（真实规则行 + 真实上下文）----
    let contract_no = contract_model::contract_find(contract_id)
        .await
        .expect("查合同")
        .expect("合同存在")
        .contract_no;
    let ctx = linkage::LinkageContext::new(
        state_machine::ENTITY_CONTRACT,
        contract_model::STATUS_SIGNED,
        contract_id,
        &contract_no,
        Some(uid(U_MAKER)),
    );
    let tx = db().begin().await.expect("开事务");
    let without_exclusion = linkage_engine::run(&tx, &ctx).await.expect("跑引擎");
    let with_exclusion = linkage_engine::run_excluding(&tx, &ctx, spec::CONTRACT_EXCLUDED_ACTIONS)
        .await
        .expect("跑引擎（带排除清单）");
    tx.rollback().await.expect("回滚");
    assert!(
        without_exclusion
            .runs
            .iter()
            .any(|item| item.action_code == spec::ACTION_GENERATE_VOUCHER),
        "对照：不带排除清单时这条 GENERATE_VOUCHER 规则必须被读到并执行（本例是 SKIPPED，\
         因为该动作只接管收货入库）—— 否则下面的断言就是虚绿（规则根本没命中）"
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

    // ---- 第二层：端到端（走真实审批回调）----
    let instance = submit_and_approve(contract_id).await;
    assert_eq!(
        contract_bridge::on_contract_settled(Some(instance), "APPROVED").await,
        Some(contract_id)
    );
    assert_eq!(
        vouchers_of(contract_id).await,
        0,
        "合同不该产生凭证：GENERATE_VOUCHER 在排除清单里，通用引擎不许给它落账"
    );
    assert_eq!(
        notices_of(contract_model::STATUS_SIGNED, contract_id).await,
        1,
        "排除清单只排凭证：同状态的 SEND_NOTICE 必须照常发出（排掉通知 = 接线了永远不响）"
    );
    purge().await;
}

// ==================== ⑤ 人工流转（生效 / 终止）====================

/// 生效（`SIGNED → ACTIVE`）与终止（`ACTIVE → TERMINATED`）各触发一次；
/// 重复点第二次被状态机 409 挡住，通知不增。
#[tokio::test]
async fn lnk07_manual_transitions_trigger_once() {
    let Some(_guard) = setup().await else {
        return;
    };
    clear_rules().await;
    let contract_id = make_contract("manual").await;
    for status in [contract_model::STATUS_ACTIVE, contract_model::STATUS_TERMINATED] {
        add_rule(
            status,
            spec::ACTION_SEND_NOTICE,
            r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
        )
        .await;
    }

    let instance = submit_and_approve(contract_id).await;
    assert_eq!(
        contract_bridge::on_contract_settled(Some(instance), "APPROVED").await,
        Some(contract_id)
    );

    // 生效
    let detail = patch_status(contract_id, contract_model::STATUS_ACTIVE, None)
        .await
        .unwrap_or_else(|err| panic!("生效失败：{} {}", err.code, err.msg));
    assert_eq!(detail.base.status, contract_model::STATUS_ACTIVE);
    assert_eq!(
        notices_of(contract_model::STATUS_ACTIVE, contract_id).await,
        1,
        "CONTRACT/ACTIVE 上配的 SEND_NOTICE 必须真的发一条"
    );

    // 重复点「生效」：状态机判非法边 → 409，联动不许再跑
    let again = patch_status(contract_id, contract_model::STATUS_ACTIVE, None).await;
    assert!(
        again.is_err(),
        "重复生效必须被挡住（状态机边判定 / 409），否则联动会跑第二遍"
    );
    assert_eq!(
        notices_of(contract_model::STATUS_ACTIVE, contract_id).await,
        1,
        "被挡住的重复流转不许再发通知"
    );

    // 终止（需要 reason）
    patch_status(
        contract_id,
        contract_model::STATUS_TERMINATED,
        Some("合同终止：LNK-07 用例"),
    )
    .await
    .unwrap_or_else(|err| panic!("终止失败：{} {}", err.code, err.msg));
    assert_eq!(
        notices_of(contract_model::STATUS_TERMINATED, contract_id).await,
        1,
        "CONTRACT/TERMINATED 上配的 SEND_NOTICE 必须真的发一条"
    );
    purge().await;
}

// ==================== ⑥ 配置期闸：合同上不许配 GENERATE_VOUCHER ====================

/// 合同上配 `GENERATE_VOUCHER` 必须**保存时就 400**（不是运行时静默跳过）：
/// 该动作在合同实体上永远只会被 `Skipped`，允许保存就等于「配得上但永远不执行」。
/// 正向对照：同触发点上的 `SEND_NOTICE` 必须**不带 `allow_unwired`** 也能保存
/// （证明确实 `wired = true`，否则写入口会按「未接线」拒掉）。
#[tokio::test]
async fn lnk07_generate_voucher_rule_is_rejected_on_save() {
    let Some(_guard) = setup().await else {
        return;
    };
    clear_rules().await;

    let patch = |action: &str, params: serde_json::Value| linkage_model::RulePatch {
        name: format!("LNK07-save-{action}"),
        trigger_entity: "CONTRACT".to_string(),
        trigger_status: contract_model::STATUS_SIGNED.to_string(),
        action_code: action.to_string(),
        action_params: params,
        enabled: Some(true),
        seq: Some(10),
        confirm: true,
        allow_unwired: false,
    };

    let err = linkage_model::upsert_rule(
        db(),
        &claims_of(U_MAKER),
        &patch(
            spec::ACTION_GENERATE_VOUCHER,
            serde_json::json!({"biz_type": "STOCK_IN"}),
        ),
    )
    .await
    .expect_err("合同上配 GENERATE_VOUCHER 必须被拒绝（合同不产生凭证）");
    assert_eq!(err.code, error_code::PARAM_INVALID, "应当是 400 参数错误");
    assert!(
        err.msg.contains("合同不产生任何凭证"),
        "400 文案必须点明「合同不产生凭证」，实际：{}",
        err.msg
    );
    let saved = linkage_model::find_rule_by_key(
        db(),
        state_machine::ENTITY_CONTRACT,
        contract_model::STATUS_SIGNED,
        spec::ACTION_GENERATE_VOUCHER,
    )
    .await
    .expect("查规则");
    assert!(saved.is_none(), "被拒绝的规则不许落库");

    // 正向对照：SEND_NOTICE 必须能存（wired = true ⇒ 不需要 allow_unwired）
    let ok = linkage_model::upsert_rule(
        db(),
        &claims_of(U_MAKER),
        &patch(
            spec::ACTION_SEND_NOTICE,
            serde_json::json!({"mode": "TAKEOVER", "recipient": "CREATOR"}),
        ),
    )
    .await
    .unwrap_or_else(|err| panic!("合同上配 SEND_NOTICE 应当能保存：{} {}", err.code, err.msg));
    assert_eq!(ok.trigger_entity, "CONTRACT");
    purge().await;
}
