//! 审批实例联动（`FLOW_INSTANCE`，LNK-12）的**真库**用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 SKIP（与本仓其它真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//!   cargo test --lib business::flow::instance_linkage_db_tests -- --test-threads=1
//! ```
//!
//! # 覆盖（卡面三条验收逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | `lnk12_approve_fires_once_and_replay_is_deduped` | 正常触发一次（通知 1 条）→ **重放** hook → 通知**仍是 1 条**（幂等键 `LNK:FLOW_INSTANCE:APPROVED:<id>`） |
//! | 2 | `lnk12_pending_instance_never_fires` | 中间票（实例仍 `PENDING`）→ 0 条规则、0 条通知（状态闸） |
//! | 3 | `lnk12_excluded_ledger_action_never_runs` | 只配 `GENERATE_VOUCHER`（落账类）→ 引擎**跳过**（hook 返回 0、一张凭证都不生成），证明排除清单有牙 |
//! | 4 | `lnk12_reject_verdict_gets_its_own_key` | `REJECTED` 用另一个幂等键：同一实例的「通过」「驳回」两条结论互不覆盖 |
//!
//! # 夹具口径
//!
//! 实例**直插一行终态实例**（`status + ended_at` 同写，满足 `ck_biz_flow_instance_end`）：
//! 本文件测的是「实例已落终态 → hook 做什么」，审批引擎本身由 `flow` 的用例覆盖。
//! 定义 / 版本用固定前缀 `lnk12_` 的**独立**流程（不碰任何种子流程），清理按主键 id 删。
//! 通知按 `LNK:FLOW_INSTANCE:%` 前缀删（没有单据主键可依的部分只有它）。

use nik::prelude::*;
use nik::uuid::Uuid;

use super::instance_linkage;
use crate::business::flow::linkage::spec;

/// 真库用例串行（共用全局 `db()` 与同一张规则表的全表唯一索引）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
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
            // 每个 `#[tokio::test]` 是独立 runtime，`db()` 是进程级全局池：
            // `max_lifetime = 0` 逼池子每次新建连接（与 stockin fixtures 同口径）。
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

/// 一个用例独占的夹具（主键全部自己持有，清理按 id 删）。
struct Fixture {
    def_id: Uuid,
    version_id: Uuid,
    instance_id: Uuid,
    rule_id: Option<Uuid>,
    initiator: Uuid,
    key: String,
}

impl Fixture {
    /// 建「定义 + 版本 + 终态实例」；`rule` = `(trigger_status, action_code, params)`。
    async fn new(status: &str, rule: Option<(&str, &str, serde_json::Value)>) -> Self {
        let initiator = db()
            .scalar_with_params::<Uuid>(
                "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1",
                Vec::new(),
            )
            .await
            .expect("库里至少要有一个未删用户当发起人");
        let def_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let instance_id = Uuid::now_v7();
        let suffix = instance_id.to_string().to_ascii_uppercase();
        let suffix = &suffix[suffix.len() - 8..];
        let instance_no = format!("LNK12-{suffix}");

        db().execute_with_params(
            "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, status) \
             VALUES (?::uuid, ?, 'LNK12 测试流程', 'LNK12_TEST', 'DRAFT')",
            vec![
                Value::from(&def_id),
                Value::Text(format!("lnk12_{}", suffix.to_ascii_lowercase())),
            ],
        )
        .await
        .expect("插流程定义");

        db().execute_with_params(
            "INSERT INTO biz_flow_def_version (id, def_id, version_no, version_status) \
             VALUES (?::uuid, ?::uuid, 1, 'DRAFT')",
            vec![Value::from(&version_id), Value::from(&def_id)],
        )
        .await
        .expect("插流程版本");

        // 终态实例：status 非 PENDING ⇒ ended_at 必须非空（ck_biz_flow_instance_end）
        db().execute_with_params(
            "INSERT INTO biz_flow_instance \
                 (id, instance_no, def_id, version_id, version_no, flow_code, flow_name, \
                  biz_type, title, status, initiator_id, ended_at) \
             VALUES (?::uuid, ?, ?::uuid, ?::uuid, 1, 'lnk12_test_flow', 'LNK12 测试流程', \
                     'LNK12_TEST', 'LNK12 联动用例单据', ?, ?::uuid, \
                     CASE WHEN ? = 'PENDING' THEN NULL ELSE now() END)",
            vec![
                Value::from(&instance_id),
                Value::Text(instance_no),
                Value::from(&def_id),
                Value::from(&version_id),
                Value::Text(status.to_string()),
                Value::from(&initiator),
                Value::Text(status.to_string()),
            ],
        )
        .await
        .expect("插审批实例");

        let rule_id = match rule {
            None => None,
            Some((trigger_status, action_code, params)) => {
                let rule_id = Uuid::now_v7();
                db().execute_with_params(
                    "INSERT INTO biz_flow_linkage_rule \
                         (id, name, trigger_entity, trigger_status, action_code, action_params) \
                     VALUES (?::uuid, ?, ?, ?, ?, ?::jsonb) \
                     ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE \
                        SET id = EXCLUDED.id, name = EXCLUDED.name, \
                            action_params = EXCLUDED.action_params, \
                            enabled = TRUE, deleted_at = NULL",
                    vec![
                        Value::from(&rule_id),
                        Value::Text(format!("LNK12-{trigger_status}-{action_code}")),
                        Value::Text(spec::TRIGGER_FLOW_INSTANCE.to_string()),
                        Value::Text(trigger_status.to_string()),
                        Value::Text(action_code.to_string()),
                        Value::Json(params.to_string()),
                    ],
                )
                .await
                .expect("插联动规则");
                Some(rule_id)
            }
        };

        Self {
            def_id,
            version_id,
            instance_id,
            rule_id,
            initiator,
            key: instance_linkage::notice_key(instance_id, status),
        }
    }

    /// 本实例那条通知的落库行数（幂等键唯一 ⇒ 正常最多 1）。
    async fn notice_count(&self) -> i64 {
        db().scalar_with_params::<i64>(
            "SELECT count(*) FROM sys_notice WHERE idempotency_key = ? AND deleted_at IS NULL",
            vec![Value::Text(self.key.clone())],
        )
        .await
        .expect("查通知条数")
    }

    /// 挂在这个实例上的凭证张数（排除清单有牙的判据）。
    async fn voucher_count(&self) -> i64 {
        db().scalar_with_params::<i64>(
            "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
            vec![Value::from(&self.instance_id)],
        )
        .await
        .expect("查凭证张数")
    }

    async fn cleanup(&self) {
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_notice_receiver WHERE notice_id IN \
                     (SELECT id FROM sys_notice WHERE idempotency_key LIKE ?)",
                vec![Value::Text("LNK:FLOW_INSTANCE:%".to_string())],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_notice WHERE idempotency_key LIKE ?",
                vec![Value::Text("LNK:FLOW_INSTANCE:%".to_string())],
            )
            .await;
        if let Some(rule_id) = self.rule_id {
            let _ = db()
                .execute_with_params(
                    "DELETE FROM biz_flow_linkage_rule WHERE id = ?::uuid",
                    vec![Value::from(&rule_id)],
                )
                .await;
        }
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&self.instance_id)],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_flow_def_version WHERE id = ?::uuid",
                vec![Value::from(&self.version_id)],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_flow_def WHERE id = ?::uuid",
                vec![Value::from(&self.def_id)],
            )
            .await;
        let _ = self.initiator;
    }
}

/// 一条 `SEND_NOTICE` 规则参数（TAKEOVER：由配置发，旧逻辑不发）。
fn notice_params() -> serde_json::Value {
    serde_json::json!({
        "mode": "TAKEOVER",
        "title": "审批已通过（LNK12 用例）",
        "content": "LNK12：这张审批单已通过"
    })
}

/// ① 正常触发一次 + 重放不翻倍（**本卡验收的核心**：同一次审批只触发一次）。
#[tokio::test]
async fn lnk12_approve_fires_once_and_replay_is_deduped() {
    let _guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return;
    }
    let fx = Fixture::new("APPROVED", Some(("APPROVED", "SEND_NOTICE", notice_params()))).await;

    let first = instance_linkage::fire_after_commit(Some(fx.instance_id), "APPROVED", Some(fx.initiator)).await;
    assert_eq!(first, 1, "第一次审批通过必须真的执行 1 条规则（通知）");
    assert_eq!(fx.notice_count().await, 1, "通知必须落 1 条");

    // 重放：同一实例同一结论再触发一次（模拟回调重试 / 幂等重放）
    let replay = instance_linkage::fire_after_commit(Some(fx.instance_id), "APPROVED", Some(fx.initiator)).await;
    assert!(replay >= 1, "重放仍会命中规则（引擎照跑），但通知必须被幂等键挡下");
    assert_eq!(
        fx.notice_count().await,
        1,
        "重放不许翻倍：LNK:FLOW_INSTANCE:APPROVED:<id> 只允许一条通知"
    );

    // 中间票 / 未知状态：状态闸直接挡在引擎之前
    assert_eq!(
        instance_linkage::fire_after_commit(Some(fx.instance_id), "PENDING", None).await,
        0,
        "PENDING 不是结论，不该触发"
    );
    assert_eq!(fx.notice_count().await, 1, "PENDING 更不该多出通知");

    fx.cleanup().await;
}

/// ② 实例仍是 PENDING（会签第一票）→ 一条规则都不跑、一条通知都不发。
#[tokio::test]
async fn lnk12_pending_instance_never_fires() {
    let _guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return;
    }
    // 配了 APPROVED 的规则，但实例还在 PENDING
    let fx = Fixture::new("PENDING", Some(("APPROVED", "SEND_NOTICE", notice_params()))).await;

    assert_eq!(
        instance_linkage::fire_after_commit(Some(fx.instance_id), "APPROVED", None).await,
        0,
        "实例状态与触发状态不一致时必须不触发（防止用旧出参重放）"
    );
    assert_eq!(fx.notice_count().await, 0, "一条通知都不该发");

    fx.cleanup().await;
}

/// ③ 落账类动作（`GENERATE_VOUCHER`）在 FLOW_INSTANCE 上**不执行**：排除清单有牙。
///
/// 同一条规则在其它触发点上是能跑的（`implemented = true`），这里被排除 ⇒ 返回 0、
/// 一张凭证都不生成。这证明「排除了」而不是「规则没命中」：规则确实存在且 enabled。
#[tokio::test]
async fn lnk12_excluded_ledger_action_never_runs() {
    let _guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return;
    }
    let voucher_params = serde_json::json!({
        "biz_type": "STOCK_IN",
        "voucher_date": "2026-11-07",
        "summary": "LNK12 不该出现的一张凭证"
    });
    let fx = Fixture::new("APPROVED", Some(("APPROVED", "GENERATE_VOUCHER", voucher_params))).await;

    let executed = instance_linkage::fire_after_commit(Some(fx.instance_id), "APPROVED", None).await;
    assert_eq!(
        executed, 0,
        "GENERATE_VOUCHER 被 spec::FLOW_INSTANCE_EXCLUDED_ACTIONS 排除 ⇒ 一条都不执行"
    );
    assert_eq!(
        fx.voucher_count().await,
        0,
        "审批实例上绝不允许落凭证（同一笔账由业务桥的专用实现落）"
    );
    // 配置期也拒：不能「配得上、跑起来被静默跳过」
    assert!(
        spec::forbidden_action_reason(spec::TRIGGER_FLOW_INSTANCE, "GENERATE_VOUCHER").is_some(),
        "配置期必须直接 400"
    );

    fx.cleanup().await;
}

/// ④ 驳回用**另一个**幂等键：同一实例的「通过」与「驳回」两条结论互不覆盖。
#[tokio::test]
async fn lnk12_reject_verdict_gets_its_own_key() {
    let _guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return;
    }
    let fx = Fixture::new("REJECTED", Some(("REJECTED", "SEND_NOTICE", notice_params()))).await;

    let executed = instance_linkage::fire_after_commit(Some(fx.instance_id), "REJECTED", None).await;
    assert_eq!(executed, 1, "驳回同样要触发（通知发起人「单子被打回了」）");
    assert_eq!(fx.notice_count().await, 1);
    assert_eq!(
        fx.key,
        format!("LNK:FLOW_INSTANCE:REJECTED:{}", fx.instance_id),
        "驳回的幂等键必须带 REJECTED（不能与 APPROVED 共键）"
    );

    fx.cleanup().await;
}
