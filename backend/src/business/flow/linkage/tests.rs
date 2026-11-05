//! 跨单据联动白名单的单测（纯逻辑 + 真库）。
//!
//! # 真库用例的开关
//!
//! 需要 `ERP_DB_URL`（与 `shared::state_machine` / `erp` 的用例同一口径）。
//! **未设置时打印 SKIP 并返回 = 假绿**，交付验收必须用
//! `ERP_DB_URL=… cargo test --lib business::flow::linkage -- --test-threads=1` 真跑一遍。
//!
//! # 为什么真库用例要串行
//!
//! 多条用例会 `ALTER TABLE … RENAME`（模拟「迁移还没上」的降级）——那是
//! ACCESS EXCLUSIVE 锁，并发跑会互相挂住。用一把 `DB_LOCK` 串起来，
//! 与 D1 的 `state_machine` 用例同一做法。
//!
//! # 夹具口径
//!
//! - 规则名一律 `D2L-` 前缀（清理按它删）；
//! - 通知按 `biz_id IN (固定测试 uuid)` 删（`sys_notice_receiver` 有 ON DELETE CASCADE）；
//! - 采购单/明细用固定 uuid，`cleanup()` 里按 order_id 删。
//!
//! SCOPE-EXEMPT: 本文件是**单测夹具与断言**（查内置 admin 的 id、按夹具 biz_id 数通知行），
//! 不是任何面向用户的查询入口；注入数据范围谓词只会让夹具里的行被裁掉而假失败。
//! 生产侧的同类查询（收件人存活校验）在 `actions.rs` 里另有 SCOPE-EXEMPT 说明。

use std::collections::HashMap;
use std::str::FromStr;

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::{stockin_model, voucher};
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{NoticeDraft, NoticeTarget};
use crate::business::shared::state_machine;

use super::actions::{self, ActionError, NoticeMode, NoticeSetting, RecipientStrategy};
use super::engine::{self, LinkageContext, NoticeRoute, Outcome};
use super::model::{self, RulePatch};
use super::spec::{self, ParamKind, Phase, Severity};

// ==================== 纯逻辑用例（无数据库） ====================

/// 白名单至少 8 个动作，且码形/说明/复用指针齐全。
#[test]
fn whitelist_has_eight_or_more_actions() {
    assert!(
        spec::ACTIONS.len() >= 8,
        "卡面要求白名单动作 ≥8 个，实际 {}",
        spec::ACTIONS.len()
    );
    let mut seen: Vec<&str> = Vec::new();
    for action in spec::ACTIONS {
        let code = action.code;
        assert!(
            code.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_'),
            "{code} 必须是大写 + 下划线（与迁移 CHECK 的书写风格一致）"
        );
        assert!((3..=64).contains(&code.chars().count()), "{code} 长度越界");
        assert!(!seen.contains(&code), "{code} 重复");
        seen.push(code);
        assert!(!action.label.trim().is_empty(), "{code} 缺人读名称");
        assert!(
            !action.reuse.trim().is_empty(),
            "{code} 必须写清第 3 步要复用的既有实现（铁律：只复用不重写）"
        );
        // 参数键不许重复，必填项必须有说明
        let mut keys: Vec<&str> = Vec::new();
        for param in action.params {
            assert!(!keys.contains(&param.key), "{code} 参数 {} 重复", param.key);
            keys.push(param.key);
            assert!(
                !param.note.trim().is_empty(),
                "{code}.{} 缺参数说明",
                param.key
            );
            assert!(
                !spec::is_forbidden_key(param.key),
                "{code}.{} 命中了 SQL 禁用键清单",
                param.key
            );
        }
    }
    assert_eq!(
        spec::implemented_codes(),
        vec![
            spec::ACTION_GENERATE_VOUCHER,
            spec::ACTION_UPDATE_RECEIVED_QTY,
            spec::ACTION_CREATE_TASK,
            spec::ACTION_SEND_NOTICE
        ],
        "已接管的动作：D2 的 UPDATE_RECEIVED_QTY / SEND_NOTICE + D3 的 GENERATE_VOUCHER \
         + P1-FIX-5C 的 CREATE_TASK（其余留占位）"
    );
}

/// **禁配 SQL**：任何 SQL 类键都必须被拒，且文案点名禁令。
#[test]
fn forbidden_sql_keys_are_rejected() {
    let action = spec::spec_of(spec::ACTION_SEND_NOTICE).expect("SEND_NOTICE 必须在白名单里");
    for key in spec::FORBIDDEN_KEYS {
        let params = serde_json::json!({ *key: "DELETE FROM biz_voucher" });
        let error = spec::validate_params(action, &params).expect_err("SQL 类键必须被拒");
        assert!(
            error.contains("禁止在联动配置里写 SQL"),
            "{key} 的拒绝文案应点名禁令，实际：{error}"
        );
    }
    // 大小写变体也要拦（有人会写 `SQL` / `Sql`）
    for key in ["SQL", "Sql", "Query"] {
        let params = serde_json::json!({ key: "select 1" });
        assert!(
            spec::validate_params(action, &params).is_err(),
            "{key} 未被拦下"
        );
    }
}

/// 不许出现动作没声明的键（参数白名单）。
#[test]
fn unknown_param_key_is_rejected() {
    let action = spec::spec_of(spec::ACTION_SEND_NOTICE).expect("SEND_NOTICE 必须在白名单里");
    let error = spec::validate_params(action, &serde_json::json!({ "bogus": 1 }))
        .expect_err("未声明的键必须被拒");
    assert!(error.contains("不接受参数 `bogus`"), "实际：{error}");
    assert!(error.contains("允许："), "错误文案要列出允许的键：{error}");
}

/// 参数必须是 JSON 对象。
#[test]
fn params_must_be_object() {
    let action = spec::spec_of(spec::ACTION_SEND_NOTICE).expect("SEND_NOTICE 必须在白名单里");
    for params in [
        serde_json::json!("title=hi"),
        serde_json::json!([1, 2]),
        serde_json::json!(null),
    ] {
        let error = spec::validate_params(action, &params).expect_err("非对象必须被拒");
        assert!(error.contains("必须是 JSON 对象"), "实际：{error}");
    }
    // 空对象是合法的（全部走触发点默认值）
    assert!(spec::validate_params(action, &serde_json::json!({})).is_ok());
}

/// 必填参数缺失要拒。
#[test]
fn required_params_are_enforced() {
    let action = spec::spec_of(spec::ACTION_UPDATE_RECEIVED_QTY).expect("动作必须在白名单里");
    let error = spec::validate_params(action, &serde_json::json!({}))
        .expect_err("缺 source_item_id 必须被拒");
    assert!(
        error.contains("缺少必需参数 `source_item_id`"),
        "实际：{error}"
    );
    let error = spec::validate_params(
        action,
        &serde_json::json!({ "source_item_id": "0d2d0000-0000-4000-8000-000000000002" }),
    )
    .expect_err("缺 qty 必须被拒");
    assert!(error.contains("缺少必需参数 `qty`"), "实际：{error}");
    // null 等同缺失
    let error = spec::validate_params(
        action,
        &serde_json::json!({ "source_item_id": null, "qty": 1 }),
    )
    .expect_err("null 等同缺失");
    assert!(
        error.contains("缺少必需参数 `source_item_id`"),
        "实际：{error}"
    );
}

/// 类型 / 枚举 / 区间 / 长度都要查。
#[test]
fn param_types_are_enforced() {
    let rq = spec::spec_of(spec::ACTION_UPDATE_RECEIVED_QTY).expect("动作必须在白名单里");
    let bad_uuid = spec::validate_params(
        rq,
        &serde_json::json!({ "source_item_id": "not-a-uuid", "qty": 1 }),
    )
    .expect_err("非法 UUID 必须被拒");
    assert!(bad_uuid.contains("不是合法的 UUID"), "实际：{bad_uuid}");

    let bad_decimal = spec::validate_params(
        rq,
        &serde_json::json!({ "source_item_id": "0d2d0000-0000-4000-8000-000000000002", "qty": "abc" }),
    )
    .expect_err("非数字必须被拒");
    assert!(
        bad_decimal.contains("不是合法的十进制数"),
        "实际：{bad_decimal}"
    );

    // 数字字符串是合法的（前端常按字符串传金额/数量）
    assert!(
        spec::validate_params(
            rq,
            &serde_json::json!({ "source_item_id": "0d2d0000-0000-4000-8000-000000000002", "qty": "1.5000" })
        )
        .is_ok()
    );

    let notice = spec::spec_of(spec::ACTION_SEND_NOTICE).expect("动作必须在白名单里");
    let bad_enum = spec::validate_params(notice, &serde_json::json!({ "mode": "TAKEOVER_NOW" }))
        .expect_err("枚举越界必须被拒");
    assert!(bad_enum.contains("不在允许值里"), "实际：{bad_enum}");
    let bad_bool = spec::validate_params(notice, &serde_json::json!({ "force": "yes" }))
        .expect_err("布尔必须被拒");
    assert!(bad_bool.contains("布尔"), "实际：{bad_bool}");
    let too_long = spec::validate_params(notice, &serde_json::json!({ "title": "x".repeat(201) }))
        .expect_err("超长必须被拒");
    assert!(too_long.contains("最长 200 个字符"), "实际：{too_long}");
    let bad_int = spec::validate_params(
        spec::spec_of("CREATE_TASK").expect("动作必须在白名单里"),
        &serde_json::json!({ "title": "t", "due_days": 999 }),
    )
    .expect_err("整数越界必须被拒");
    assert!(bad_int.contains("超出 0..=365"), "实际：{bad_int}");
}

/// 事务语义的不变量：**提交后动作一律非关键**（否则「失败只记日志」与
/// 「关键动作回滚」会自相矛盾——提交后根本回滚不了业务）。
#[test]
fn after_commit_actions_are_never_critical() {
    for action in spec::ACTIONS {
        if action.phase == Phase::AfterCommit {
            assert_eq!(
                action.severity,
                Severity::NonCritical,
                "{} 在提交后执行，不可能回滚业务，因此不能是关键动作",
                action.code
            );
        }
    }
    // 两个已实现动作的事务归属是卡面点名的
    let send = spec::spec_of(spec::ACTION_SEND_NOTICE).expect("动作必须在白名单里");
    assert_eq!(send.phase, Phase::AfterCommit, "发通知必须在事务提交之后");
    assert_eq!(
        send.severity,
        Severity::NonCritical,
        "发通知失败不许回滚业务"
    );
    let qty = spec::spec_of(spec::ACTION_UPDATE_RECEIVED_QTY).expect("动作必须在白名单里");
    assert_eq!(qty.phase, Phase::InTx, "回写累计量必须在事务内");
    assert_eq!(qty.severity, Severity::Critical, "回写累计量失败必须回滚");
}

/// 迁移里的 CHECK 必须覆盖**全部**动作码（加动作不改迁移 → 这条立刻红）。
#[test]
fn migration_check_covers_every_action_code() {
    let up = include_str!("../../../../migrations/business/V0205__flow_linkage_rule.up.sql");
    for code in spec::codes() {
        assert!(
            up.contains(&format!("'{code}'")),
            "迁移 V0205 的 ck_flow_linkage_action_code 缺动作码 {code}"
        );
    }
    for key in spec::FORBIDDEN_KEYS {
        assert!(
            up.contains(&format!("'{key}'")),
            "迁移 V0205 的 ck_flow_linkage_no_sql_key 缺禁用键 {key}"
        );
    }
    assert!(up.contains(spec::AUDIT_ENTITY_TYPE) || up.contains("FLOW_LINKAGE_CFG"));
}

/// 触发点白名单必须覆盖状态机注册表的全部实体（例外必须显式登记在 `TRIGGER_EXEMPT`）。
#[test]
fn triggers_cover_state_machine_entities() {
    for entity in state_machine::Entity::ALL {
        assert!(
            spec::is_known_trigger(entity.as_str())
                || spec::TRIGGER_EXEMPT.contains(&entity.as_str()),
            "状态机实体 {} 既不在联动触发点白名单里、也没登记在 TRIGGER_EXEMPT（漏登记 = 挂不上联动）",
            entity.as_str()
        );
    }
    // 例外清单必须自我一致：不许和真白名单重复、每一项都得是真实实体
    for exempt in spec::TRIGGER_EXEMPT {
        assert!(
            !spec::is_known_trigger(exempt),
            "{exempt} 同时在白名单与例外清单里"
        );
        assert!(
            state_machine::Entity::ALL
                .iter()
                .any(|entity| entity.as_str() == *exempt),
            "例外清单里的 {exempt} 不是状态机注册的实体（写错名字等于白登记）"
        );
    }
    assert!(spec::is_known_trigger(spec::TRIGGER_TASK));
    assert!(!spec::is_known_trigger("UNKNOWN_ENTITY"));
}

/// 通知整形的默认行为 = **逐字沿用触发点给的草稿**（第 2 步「对比一致」的基础）。
#[test]
fn shape_notice_defaults_are_identical() {
    let base = sample_draft();
    let ctx = sample_ctx();
    let setting = NoticeSetting {
        mode: NoticeMode::Takeover,
        title: None,
        content: None,
        force: None,
        recipient: RecipientStrategy::Legacy,
    };
    let shaped = actions::shape_notice(&base, &setting, &ctx).expect("默认整形不该失败");
    assert!(
        actions::compare_notices(&shaped, &base).is_empty(),
        "不写覆盖键时整形结果必须与原草稿逐字段相同"
    );

    // 覆盖键生效
    let setting = NoticeSetting {
        mode: NoticeMode::Takeover,
        title: Some("自定义标题".to_string()),
        content: None,
        force: Some(true),
        recipient: RecipientStrategy::Owner,
    };
    let shaped = actions::shape_notice(&base, &setting, &ctx).expect("整形失败");
    assert_eq!(shaped.title, "自定义标题");
    assert!(shaped.force);
    let diffs = actions::compare_notices(&shaped, &base);
    assert!(diffs.contains(&"title") && diffs.contains(&"force") && diffs.contains(&"recipients"));

    // 收件人策略指向一个「没有这个人」的场景 → 跳过（不静默发空通知）
    let ctx_empty = LinkageContext::new(spec::TRIGGER_TASK, "DOING", Uuid::now_v7(), "T-0", None)
        .with_notice(sample_draft());
    let error = actions::shape_notice(&base, &setting, &ctx_empty).expect_err("无收件人必须跳过");
    assert!(matches!(error, ActionError::Skipped(_)));
}

/// 参数解析：缺省 → SHADOW / LEGACY；非法值 → Err。
#[test]
fn notice_setting_parsing() {
    let default = NoticeSetting::from_params(&serde_json::json!({})).expect("空参数应合法");
    assert_eq!(default.mode, NoticeMode::Shadow);
    assert_eq!(default.recipient, RecipientStrategy::Legacy);
    assert!(default.title.is_none() && default.content.is_none() && default.force.is_none());

    let takeover = NoticeSetting::from_params(&serde_json::json!({
        "mode": "TAKEOVER",
        "recipient": "OWNER",
        "force": false
    }))
    .expect("合法参数");
    assert_eq!(takeover.mode, NoticeMode::Takeover);
    assert_eq!(takeover.recipient, RecipientStrategy::Owner);
    assert_eq!(takeover.force, Some(false));

    assert!(NoticeSetting::from_params(&serde_json::json!({ "mode": "NOPE" })).is_err());
    assert!(NoticeSetting::from_params(&serde_json::json!({ "force": 1 })).is_err());
}

/// `ParamKind::UuidList` 的行为（给第 3 步批量动作用的类型，先钉住语义）。
#[test]
fn uuid_list_kind_validates_items() {
    // 拿一个含 UuidList 的动作做校验；当前第一批没有用到它，直接构造声明验证语义
    let action = spec::ActionSpec {
        code: "TEST_UUID_LIST",
        label: "测试",
        phase: Phase::InTx,
        severity: Severity::NonCritical,
        implemented: false,
        params: &[spec::ParamSpec {
            key: "ids",
            kind: ParamKind::UuidList { max_items: 2 },
            required: false,
            note: "id 列表",
        }],
        reuse: "n/a",
    };
    assert!(
        spec::validate_params(
            &action,
            &serde_json::json!({ "ids": ["0d2d0000-0000-4000-8000-000000000002"] })
        )
        .is_ok()
    );
    assert!(
        spec::validate_params(
            &action,
            &serde_json::json!({ "ids": ["0d2d0000-0000-4000-8000-000000000002", "0d2d0000-0000-4000-8000-000000000003", "0d2d0000-0000-4000-8000-000000000004"] })
        )
        .is_err()
    );
    assert!(spec::validate_params(&action, &serde_json::json!({ "ids": ["x"] })).is_err());
}

// ==================== 真库用例 ====================

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 夹具前缀（规则名 / 单号）。
const PREFIX: &str = "D2L-";

fn uuid_of(suffix: &str) -> Uuid {
    Uuid::from_str(&format!("0d2d0000-0000-4000-8000-0000000{suffix}")).expect("固定 uuid")
}

/// 测试用采购单（明细挂在它下面）。
fn fixture_po() -> Uuid {
    uuid_of("00001")
}
/// 采购明细 1（订购 10，已收 0）。
fn fixture_item1() -> Uuid {
    uuid_of("00002")
}
/// 采购明细 2（订购 10，已收 0）。
fn fixture_item2() -> Uuid {
    uuid_of("00003")
}
/// 物料 / 单位 / 仓库（弱引用，不需要真实主数据）。
fn fixture_material() -> Uuid {
    uuid_of("00004")
}
fn fixture_unit() -> Uuid {
    uuid_of("00005")
}
fn fixture_warehouse() -> Uuid {
    uuid_of("00006")
}
/// 通知测试用的「单据」主键（清理按它删 sys_notice）。
fn fixture_doc_a() -> Uuid {
    uuid_of("000a1")
}
fn fixture_doc_b() -> Uuid {
    uuid_of("000b2")
}
fn fixture_doc_c() -> Uuid {
    uuid_of("000c3")
}

/// 连库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
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
                Ok(connection) => set_db(connection).is_ok(),
                Err(err) => {
                    println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

/// 取锁 + 连库 + 清夹具。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    ensure_po().await;
    Some(guard)
}

/// 清掉本卡夹具（顺序：明细 → 头 → 规则 → 通知 → 配置审计）。
async fn cleanup() {
    let statements = [
        format!(
            "DELETE FROM biz_purchase_order_item WHERE order_id = '{}'",
            fixture_po()
        ),
        format!(
            "DELETE FROM biz_purchase_order WHERE id = '{}'",
            fixture_po()
        ),
        format!("DELETE FROM biz_flow_linkage_rule WHERE name LIKE '{PREFIX}%'"),
        format!(
            "DELETE FROM sys_notice WHERE biz_id IN ('{}', '{}', '{}')",
            fixture_doc_a(),
            fixture_doc_b(),
            fixture_doc_c()
        ),
        "DELETE FROM biz_status_log WHERE entity_type = 'FLOW_LINKAGE_CFG'".to_string(),
        // D3：收货单夹具 + 它可能留下的凭证 / 状态日志 / 专用期间
        format!(
            "DELETE FROM biz_stock_in WHERE id IN ('{}', '{}', '{}')",
            fixture_si_ok(),
            fixture_si_ok2(),
            fixture_si_zero()
        ),
        format!(
            "DELETE FROM biz_voucher WHERE source_id IN ('{}', '{}', '{}')",
            fixture_si_ok(),
            fixture_si_ok2(),
            fixture_si_zero()
        ),
        "DELETE FROM biz_accounting_period WHERE period_code = '2031-03'".to_string(),
    ];
    for sql in statements {
        db().execute_with_params(&sql, vec![])
            .await
            .unwrap_or_else(|error| panic!("清理夹具失败（{sql}）：{error}"));
    }
}

/// 造一张采购单 + 两条明细（订购 10 / 已收 0）。
async fn ensure_po() {
    let now = Utc::now();
    db().execute_with_params(
        "INSERT INTO biz_purchase_order \
            (id, order_no, supplier_id, order_date, warehouse_id, status, currency_code, \
             total_qty, total_amount, total_tax, total_with_tax, credit_days, created_at, updated_at) \
         VALUES (?::uuid, ?, ?::uuid, ?, ?::uuid, 'APPROVED', 'CNY', 20, 0, 0, 0, 0, ?, ?) \
         ON CONFLICT (id) DO NOTHING",
        vec![
            Value::from(&fixture_po()),
            Value::Text(format!("{PREFIX}PO")),
            Value::from(&uuid_of("00007")),
            Value::from(now.date_naive()),
            Value::from(&fixture_warehouse()),
            Value::from(now),
            Value::from(now),
        ],
    )
    .await
    .unwrap_or_else(|error| panic!("造采购单失败：{error}"));

    for (line_no, item) in [(1_i32, fixture_item1()), (2_i32, fixture_item2())] {
        db().execute_with_params(
            "INSERT INTO biz_purchase_order_item \
                (id, order_id, line_no, material_id, unit_id, qty, price, price_incl, \
                 tax_rate, amount, tax_amount, with_tax_amount, created_at, updated_at) \
             VALUES (?::uuid, ?::uuid, ?, ?::uuid, ?::uuid, 10, 0, 0, 0, 0, 0, 0, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET qty = 10",
            vec![
                Value::from(&item),
                Value::from(&fixture_po()),
                Value::I32(line_no),
                Value::from(&fixture_material()),
                Value::from(&fixture_unit()),
                Value::from(now),
                Value::from(now),
            ],
        )
        .await
        .unwrap_or_else(|error| panic!("造采购明细失败：{error}"));
    }
}

/// 超管声明。
fn super_claims() -> TokenClaims {
    claims(&["*:*:*"], &["SUPER_ADMIN"])
}

/// 普通用户声明。
fn claims(permissions: &[&str], roles: &[&str]) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: "00000000-0000-4000-8000-0000000000b2".to_string(),
        username: Some("d2_tester".to_string()),
        roles: roles.iter().map(|value| value.to_string()).collect(),
        permissions: permissions.iter().map(|value| value.to_string()).collect(),
        token_type: None,
        exp: 0,
        iat: 0,
        extra: HashMap::new(),
    }
}

/// 库里真实存在的 admin 主键（通知收件人要用活人）。
async fn admin_id() -> Uuid {
    let id: Option<Uuid> = db()
        .query_first_sql(
            "SELECT id FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL".to_string(),
            vec![],
        )
        .await
        .expect("查 admin")
        .map(|row| row.get_typed("id").expect("admin id"));
    id.expect("测试库必须有内置管理员 admin")
}

/// 一条规则的 patch（默认 SEND_NOTICE + TAKEOVER）。
fn patch(name: &str, action_code: &str, params: serde_json::Value) -> RulePatch {
    RulePatch {
        name: format!("{PREFIX}{name}"),
        trigger_entity: spec::TRIGGER_TASK.to_string(),
        trigger_status: "DOING".to_string(),
        action_code: action_code.to_string(),
        action_params: params,
        enabled: Some(true),
        seq: Some(10),
        confirm: true,
        // 默认不允许「未接线触发点」上的静默保存（P1-FIX-5C）：本文件用的 TASK 已接线，
        // 所以恒为 false 就是最严格的形态；需要预置配置的用例显式改成 true。
        allow_unwired: false,
    }
}

/// 样例草稿（与 task 状态通知同形：Todo / Info / 双收件人 / 链到任务）。
fn sample_draft() -> NoticeDraft {
    let mut draft = NoticeDraft::text(
        NoticeType::Todo,
        NoticeLevel::Info,
        "任务状态变更",
        "任务「D2 样例」（D2L-T-1）由 d2_tester 从「待开始」改为「进行中」",
        NoticeTarget::User(vec![uuid_of("0aa01"), uuid_of("0aa02")]),
    );
    draft.link_url = Some(format!("/task/list?id={}", fixture_doc_a()));
    draft.biz_type = Some("task".to_string());
    draft.biz_id = Some(fixture_doc_a().to_string());
    draft.sender_name = Some("d2_tester".to_string());
    draft.force = false;
    draft
}

/// 样例上下文（收件人用夹具 uuid，不查库；只给纯逻辑用例用）。
fn sample_ctx() -> LinkageContext<'static> {
    LinkageContext::new(
        spec::TRIGGER_TASK,
        "DOING",
        fixture_doc_a(),
        "D2L-T-1",
        None,
    )
    .with_people(Some(uuid_of("0aa01")), Some(uuid_of("0aa02")), None)
    .with_notice(sample_draft())
}

/// 真库用例的草稿：收件人用**库里真实存在的** admin（第 3 层会校验收件人存活，
/// 用假 uuid 会被判「都查不到人」而跳过，测出来的是别的路径）。
fn draft_for(doc_id: Uuid, admin: Uuid) -> NoticeDraft {
    let mut draft = NoticeDraft::text(
        NoticeType::Todo,
        NoticeLevel::Info,
        "任务状态变更",
        "任务「D2 样例」（D2L-T）由 d2_tester 从「待开始」改为「进行中」",
        NoticeTarget::User(vec![admin]),
    );
    draft.link_url = Some(format!("/task/list?id={doc_id}"));
    draft.biz_type = Some("task".to_string());
    draft.biz_id = Some(doc_id.to_string());
    draft.sender_name = Some("d2_tester".to_string());
    draft.force = false;
    draft
}

// ---------- 配置写入口（第 1 / 2 层校验） ----------

/// 非法动作码 → 400（第 1 层应用侧）。
#[tokio::test]
async fn upsert_rejects_unknown_action_code() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    let error = model::upsert_rule(
        &tx,
        &super_claims(),
        &patch("bad-code", "DROP_TABLE", serde_json::json!({})),
    )
    .await
    .expect_err("未知动作码必须被拒");
    assert_eq!(error.status, 400, "实际：{error:?}");
    assert!(error.msg.contains("不在白名单里"), "实际：{}", error.msg);
    assert!(error.msg.contains("禁配 SQL") || error.msg.contains("不接受任何自定义语句"));
    tx.rollback().await.expect("回滚");
}

/// 配置里出现 SQL 键 → 400，且文案点名禁令。
#[tokio::test]
async fn upsert_rejects_params_with_sql_key() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    let error = model::upsert_rule(
        &tx,
        &super_claims(),
        &patch(
            "sql-key",
            spec::ACTION_SEND_NOTICE,
            serde_json::json!({ "sql": "UPDATE biz_inventory SET qty = 999999" }),
        ),
    )
    .await
    .expect_err("SQL 键必须被拒");
    assert_eq!(error.status, 400, "实际：{error:?}");
    assert!(
        error.msg.contains("禁止在联动配置里写 SQL"),
        "实际：{}",
        error.msg
    );
    tx.rollback().await.expect("回滚");

    // 数据库层的第二道闸：绕过 API 直接写库也必须失败
    let direct = db()
        .execute_with_params(
            "INSERT INTO biz_flow_linkage_rule \
                (id, name, trigger_entity, trigger_status, action_code, action_params) \
             VALUES (gen_random_uuid(), 'D2L-direct-sql', 'TASK', 'DOING', 'SEND_NOTICE', \
                     '{\"sql\": \"select 1\"}'::jsonb)",
            vec![],
        )
        .await;
    assert!(
        matches!(direct, Err(DbError::CheckViolation { .. })),
        "数据库层 ck_flow_linkage_no_sql_key 必须拦住直接写库，实际：{direct:?}"
    );
}

/// 尚未接管的动作 → 400（配得上但什么都不做是最坏的一种「看起来配好了」）。
#[tokio::test]
async fn upsert_rejects_not_implemented_action() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    for code in ["REVERSE_VOUCHER", "WRITE_INVENTORY", "LOCK_DOCUMENT"] {
        let action = spec::spec_of(code).expect("动作必须在白名单里");
        let mut params = serde_json::Map::new();
        for param in action.params.iter().filter(|p| p.required) {
            let value = match param.kind {
                ParamKind::Uuid => serde_json::json!("0d2d0000-0000-4000-8000-000000000002"),
                ParamKind::Decimal => serde_json::json!("1"),
                ParamKind::Int { .. } => serde_json::json!(1),
                ParamKind::Bool => serde_json::json!(true),
                ParamKind::Text { .. } => serde_json::json!("x"),
                ParamKind::Enum(values) => serde_json::json!(values[0]),
                ParamKind::UuidList { .. } => serde_json::json!([]),
            };
            params.insert(param.key.to_string(), value);
        }
        let error = model::upsert_rule(
            &tx,
            &super_claims(),
            &patch("not-impl", code, serde_json::Value::Object(params)),
        )
        .await
        .expect_err("占位动作不能配置");
        assert_eq!(error.status, 400, "{code} 实际：{error:?}");
        assert!(error.msg.contains("尚未接管"), "{code} 实际：{}", error.msg);
    }
    tx.rollback().await.expect("回滚");
}

/// 未知触发点 / 未知状态 → 400（写错状态的规则永远不触发）。
#[tokio::test]
async fn upsert_rejects_unknown_trigger() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    let mut bad_entity = patch(
        "bad-entity",
        spec::ACTION_SEND_NOTICE,
        serde_json::json!({}),
    );
    bad_entity.trigger_entity = "NOT_A_DOC".to_string();
    let error = model::upsert_rule(&tx, &super_claims(), &bad_entity)
        .await
        .expect_err("未知触发点必须被拒");
    assert_eq!(error.status, 400);
    assert!(
        error.msg.contains("未知的触发单据类型"),
        "实际：{}",
        error.msg
    );

    let mut bad_status = patch(
        "bad-status",
        spec::ACTION_SEND_NOTICE,
        serde_json::json!({}),
    );
    bad_status.trigger_status = "DOING_NOT_EXIST".to_string();
    let error = model::upsert_rule(&tx, &super_claims(), &bad_status)
        .await
        .expect_err("未知状态必须被拒");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("没有状态"), "实际：{}", error.msg);
    tx.rollback().await.expect("回滚");
}

/// 非超管 → 403；未二次确认 → 400。
#[tokio::test]
async fn upsert_requires_super_admin_and_confirm() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    let error = model::upsert_rule(
        &tx,
        &claims(&["business:flow:read"], &["OPERATOR"]),
        &patch("no-admin", spec::ACTION_SEND_NOTICE, serde_json::json!({})),
    )
    .await
    .expect_err("非超管必须被拒");
    assert_eq!(error.status, 403, "实际：{error:?}");
    assert!(error.msg.contains("SUPER_ADMIN"), "实际：{}", error.msg);

    let mut no_confirm = patch(
        "no-confirm",
        spec::ACTION_SEND_NOTICE,
        serde_json::json!({}),
    );
    no_confirm.confirm = false;
    let error = model::upsert_rule(&tx, &super_claims(), &no_confirm)
        .await
        .expect_err("未二次确认必须被拒");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("二次确认"), "实际：{}", error.msg);
    tx.rollback().await.expect("回滚");
}

/// 「配置能存能查」：upsert 之后按触发点能读回来，字段与顺序都对，且写了审计。
#[tokio::test]
async fn upsert_then_load_roundtrip() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    let params = serde_json::json!({ "mode": "SHADOW", "force": false });
    let saved = model::upsert_rule(
        &tx,
        &super_claims(),
        &patch("roundtrip", spec::ACTION_SEND_NOTICE, params.clone()),
    )
    .await
    .expect("合法配置应能保存");
    assert_eq!(saved.trigger_entity, spec::TRIGGER_TASK);
    assert_eq!(saved.trigger_status, "DOING");
    assert_eq!(saved.action_params, params);
    assert_eq!(saved.seq, 10);
    assert!(saved.enabled);

    let rules = model::rules_for(&tx, spec::TRIGGER_TASK, "DOING").await;
    assert_eq!(rules.len(), 1, "同一触发点应读到 1 条规则");
    assert_eq!(rules[0].id, saved.id);

    // 改配置走同一行（唯一键 upsert），不是插出新行
    let mut updated = patch("roundtrip", spec::ACTION_SEND_NOTICE, serde_json::json!({}));
    updated.seq = Some(30);
    let again = model::upsert_rule(&tx, &super_claims(), &updated)
        .await
        .expect("更新应成功");
    assert_eq!(
        again.id, saved.id,
        "唯一键 (entity,status,action) 必须复用同一行"
    );
    assert_eq!(again.seq, 30);

    // 审计（biz_status_log / FLOW_LINKAGE_CFG）写了改前与改后
    // ⚠️ 必须用 `&tx` 读：审计行写在同一个事务里，用 `db()`（另一条连接）读不到
    let logs = tx
        .query_sql(
            "SELECT reason FROM biz_status_log \
              WHERE entity_type = 'FLOW_LINKAGE_CFG' AND entity_id = ?::uuid"
                .to_string(),
            vec![Value::from(&saved.id)],
        )
        .await
        .expect("查配置审计");
    assert!(
        logs.len() >= 2,
        "保存 + 更新应各留一行审计，实际 {} 行",
        logs.len()
    );
    let reason: String = logs[0].get_typed("reason").expect("reason");
    assert!(
        reason.contains("改前") && reason.contains("改后"),
        "实际：{reason}"
    );

    tx.rollback().await.expect("回滚"); // 审计行也一起回滚（真库验收时另有 e2e 证据）
}

// ---------- 引擎（事务内动作 / 失败策略 / 顺序 / 启停） ----------

/// `enabled = false` 不执行（配置保留但休眠）。
#[tokio::test]
async fn engine_skips_disabled_rules() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    let mut disabled = patch(
        "disabled",
        spec::ACTION_UPDATE_RECEIVED_QTY,
        serde_json::json!({ "source_item_id": fixture_item1().to_string(), "qty": "5" }),
    );
    disabled.enabled = Some(false);
    let rule = model::upsert_rule(&tx, &super_claims(), &disabled)
        .await
        .expect("保存规则");
    assert!(!rule.enabled);

    let ctx = LinkageContext::new(
        spec::TRIGGER_TASK,
        "DOING",
        fixture_doc_a(),
        "D2L-T-1",
        None,
    );
    let run = engine::run(&tx, &ctx).await.expect("引擎不该报错");
    assert!(
        run.runs.iter().all(|item| item.rule_id != rule.id),
        "禁用规则不该产生执行记录"
    );
    assert!(
        run.deferred.iter().all(|item| item.id != rule.id),
        "禁用规则不该进入提交后队列"
    );

    let received: DbResult<String> = tx
        .scalar_sql(
            format!(
                "SELECT ({}::numeric(18,4))::text FROM biz_purchase_order_item i WHERE i.id = ?::uuid",
                crate::business::erp::purchase_model::POSTED_RECEIVED_SQL
            ),
            vec![Value::from(&fixture_item1())],
        )
        .await;
    assert_eq!(
        received.expect("查已收数量"),
        "0.0000",
        "禁用规则不许改数据"
    );
    tx.rollback().await.expect("回滚");
}

/// 同一触发点多条规则按 `seq` 升序执行。
///
/// 注意：唯一键是 `(trigger_entity, trigger_status, action_code)`（卡面要求），
/// 所以「同一触发点的两条规则」必然是**两个不同动作**。这里用
/// 「占位动作（会被跳过）+ 已实现动作」组合：既证明 `seq` 排序不是插入顺序，
/// 又证明「前一条被跳过不会挡住后一条」。
#[tokio::test]
async fn engine_runs_in_seq_order() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    // 故意先写 seq=20 的那条，验证排序不是「插入顺序」
    let mut later = patch(
        "seq20",
        spec::ACTION_UPDATE_RECEIVED_QTY,
        serde_json::json!({ "source_item_id": fixture_item1().to_string(), "qty": "2" }),
    );
    later.seq = Some(20);
    model::upsert_rule(&tx, &super_claims(), &later)
        .await
        .expect("保存 seq20");
    // 另一条动作不同（WRITE_INVENTORY 是占位），seq 更小 → 应该先被处理
    tx.execute_with_params(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, enabled, seq) \
         VALUES (?::uuid, 'D2L-seq5', 'TASK', 'DOING', 'WRITE_INVENTORY', \
                 '{\"material_id\": \"0d2d0000-0000-4000-8000-000000000004\", \
                   \"warehouse_id\": \"0d2d0000-0000-4000-8000-000000000006\", \
                   \"qty\": \"5\", \"direction\": \"IN\"}'::jsonb, true, 5)",
        vec![Value::from(&Uuid::now_v7())],
    )
    .await
    .expect("直接插占位规则");

    let ctx = LinkageContext::new(
        spec::TRIGGER_TASK,
        "DOING",
        fixture_doc_a(),
        "D2L-T-1",
        None,
    );
    let run = engine::run(&tx, &ctx).await.expect("引擎不该报错");
    assert_eq!(run.runs.len(), 2, "两条规则都要有执行记录");
    assert_eq!(run.runs[0].seq, 5, "seq 小的先跑");
    assert_eq!(run.runs[1].seq, 20);
    assert!(
        matches!(&run.runs[0].outcome, Outcome::Skipped(reason) if reason.contains("尚未接管")),
        "第一条是占位动作，必须被跳过：{:?}",
        run.runs[0].outcome
    );
    assert!(
        run.runs[1].outcome.is_success(),
        "前一条被跳过不能挡住后一条：{:?}",
        run.runs[1].outcome
    );

    // P2-3：累计收货量是派生值（只算已过账收货单），本动作不再写任何列 ——
    // 断言 ①动作成功 ②派生值仍是 0（没有已过账收货单，本就该是 0）。
    let qty1: String = tx
        .scalar_sql(
            format!(
                "SELECT ({}::numeric(18,4))::text FROM biz_purchase_order_item i WHERE i.id = ?::uuid",
                crate::business::erp::purchase_model::POSTED_RECEIVED_SQL
            ),
            vec![Value::from(&fixture_item1())],
        )
        .await
        .expect("查已收数量");
    assert_eq!(
        qty1, "0.0000",
        "派生口径：动作不落库，值只由已过账收货单决定"
    );
    tx.rollback().await.expect("回滚");
}

/// **关键动作失败 → 整笔回滚**（卡面核心用例）。
///
/// 两条规则：第一条通过校验（其他单据已收 0 + 本次 2 ≤ 订购量 10），
/// 第二条超收（qty 999 > 10）→ 引擎返回 Err；
/// 调用方 rollback 后两条规则的效果都不在库里。
///
/// P2-3 起本动作不写列（累计收货量是派生值），"回滚"验的是
/// 「关键动作失败 → 整个事务不提交」，所以断言看的是派生值保持 0 且引擎报错。
#[tokio::test]
async fn engine_critical_failure_rolls_back() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    let mut first = patch(
        "critical-ok",
        spec::ACTION_UPDATE_RECEIVED_QTY,
        serde_json::json!({ "source_item_id": fixture_item1().to_string(), "qty": "2" }),
    );
    first.seq = Some(10);
    model::upsert_rule(&tx, &super_claims(), &first)
        .await
        .expect("保存第一条");
    let mut second = patch(
        "critical-over",
        spec::ACTION_UPDATE_RECEIVED_QTY,
        serde_json::json!({ "source_item_id": fixture_item2().to_string(), "qty": "999" }),
    );
    second.seq = Some(20);
    model::upsert_rule(&tx, &super_claims(), &second)
        .await
        .expect("保存第二条");

    let ctx = LinkageContext::new(
        spec::TRIGGER_TASK,
        "DOING",
        fixture_doc_a(),
        "D2L-T-1",
        None,
    );
    let error = engine::run(&tx, &ctx)
        .await
        .expect_err("超收是关键动作失败，必须返回 Err");
    assert!(
        error.msg.contains("整笔回滚"),
        "错误文案要说清回滚语义，实际：{}",
        error.msg
    );
    tx.rollback().await.expect("回滚");

    // 回滚后：两条明细的已收数量（派生）都没变
    for item in [fixture_item1(), fixture_item2()] {
        let qty: String = db()
            .scalar_sql(
                format!(
                    "SELECT ({}::numeric(18,4))::text FROM biz_purchase_order_item i WHERE i.id = ?::uuid",
                    crate::business::erp::purchase_model::POSTED_RECEIVED_SQL
                ),
                vec![Value::from(&item)],
            )
            .await
            .expect("查已收数量");
        assert_eq!(qty, "0.0000", "关键动作失败后不许留下半条账");
    }
}

/// 运行时 target 不存在 → ERROR 日志 + 跳过（不中断流程、不报错）。
#[tokio::test]
async fn engine_skips_missing_target() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    let missing = Uuid::now_v7();
    model::upsert_rule(
        &tx,
        &super_claims(),
        &patch(
            "missing-target",
            spec::ACTION_UPDATE_RECEIVED_QTY,
            serde_json::json!({ "source_item_id": missing.to_string(), "qty": "1" }),
        ),
    )
    .await
    .expect("保存规则");

    let ctx = LinkageContext::new(
        spec::TRIGGER_TASK,
        "DOING",
        fixture_doc_a(),
        "D2L-T-1",
        None,
    );
    let run = engine::run(&tx, &ctx).await.expect("target 不存在不该报错");
    assert_eq!(run.runs.len(), 1);
    assert!(
        matches!(&run.runs[0].outcome, Outcome::Skipped(reason) if reason.contains("不存在")),
        "实际：{:?}",
        run.runs[0].outcome
    );
    tx.rollback().await.expect("回滚");
}

/// 占位动作（绕过 API 直接写库）→ 跳过 + 不写数据；配置写入本来就被拒。
#[tokio::test]
async fn engine_skips_placeholder_action() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    // 绕过 API 直接插一条 WRITE_INVENTORY（模拟有人手工写库）
    tx.execute_with_params(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, enabled, seq) \
         VALUES (?::uuid, 'D2L-direct-write-inv', 'TASK', 'DOING', 'WRITE_INVENTORY', \
                 '{\"material_id\": \"0d2d0000-0000-4000-8000-000000000004\", \
                   \"warehouse_id\": \"0d2d0000-0000-4000-8000-000000000006\", \
                   \"qty\": \"5\", \"direction\": \"IN\"}'::jsonb, true, 5)",
        vec![Value::from(&Uuid::now_v7())],
    )
    .await
    .expect("直接插库应通过 CHECK（动作码在白名单里）");

    let ctx = LinkageContext::new(
        spec::TRIGGER_TASK,
        "DOING",
        fixture_doc_a(),
        "D2L-T-1",
        None,
    );
    let run = engine::run(&tx, &ctx).await.expect("占位动作不该报错");
    assert_eq!(run.runs.len(), 1);
    assert!(
        matches!(&run.runs[0].outcome, Outcome::Skipped(reason) if reason.contains("尚未接管")),
        "实际：{:?}",
        run.runs[0].outcome
    );
    // 一行库存都没写
    let rows: i64 = tx
        .scalar_sql(
            "SELECT count(*) FROM biz_inventory WHERE material_id = ?::uuid".to_string(),
            vec![Value::from(&fixture_material())],
        )
        .await
        .expect("查库存");
    assert_eq!(rows, 0, "占位动作绝不能半执行");
    tx.rollback().await.expect("回滚");
}

/// 迁移还没上（表不存在）→ 读规则降级为空 + 不污染事务（D1 的 SAVEPOINT 教训）。
#[tokio::test]
async fn engine_is_fail_safe_when_table_missing() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    tx.execute_with_params(
        "ALTER TABLE biz_flow_linkage_rule RENAME TO biz_flow_linkage_rule_gone",
        vec![],
    )
    .await
    .expect("改名（模拟迁移未上）");

    // 读规则：降级为空，不报错
    let rules = model::rules_for(&tx, spec::TRIGGER_TASK, "DOING").await;
    assert!(rules.is_empty(), "表不存在时必须降级为空规则集");

    // 事务仍可用：紧接着的业务写不能因为「读过一次不存在的表」而 aborted
    let ctx = LinkageContext::new(
        spec::TRIGGER_TASK,
        "DOING",
        fixture_doc_a(),
        "D2L-T-1",
        None,
    );
    let run = engine::run(&tx, &ctx).await.expect("降级路径不该报错");
    assert!(run.is_empty());
    tx.execute_with_params(
        "UPDATE biz_purchase_order_item SET remark = 'after-probe' WHERE id = ?::uuid",
        vec![Value::from(&fixture_item1())],
    )
    .await
    .expect("读配置失败后事务必须还能写（SAVEPOINT 生效）");

    tx.rollback().await.expect("回滚（改名一起撤销）");
    let restored: i64 = db()
        .scalar_sql(
            "SELECT count(*) FROM pg_tables WHERE tablename = 'biz_flow_linkage_rule'".to_string(),
            vec![],
        )
        .await
        .expect("查表");
    assert_eq!(restored, 1, "回滚后表名应恢复");
}

// ---------- 第 2 步：SEND_NOTICE 接管（影子对比 / 接管 / 兜底） ----------

/// 没有 SEND_NOTICE 规则 → 路由是 Legacy，且不产生任何通知行。
#[tokio::test]
async fn status_notice_without_rule_is_legacy() {
    let Some(_guard) = setup().await else { return };
    let admin = admin_id().await;
    let ctx = LinkageContext::new(
        spec::TRIGGER_TASK,
        "DOING",
        fixture_doc_b(),
        "D2L-T-2",
        None,
    )
    .with_notice(draft_for(fixture_doc_b(), admin));
    let route = engine::status_notice(&ctx).await;
    assert_eq!(route, NoticeRoute::Legacy { compare: None });
    assert_eq!(
        notice_rows(fixture_doc_b()).await,
        0,
        "没配置时不该多发通知"
    );
}

/// 影子模式：旧逻辑照发，配置只做对比（`matched = true`）。
#[tokio::test]
async fn status_notice_shadow_keeps_legacy() {
    let Some(_guard) = setup().await else { return };
    let admin = admin_id().await;
    model::upsert_rule(
        db(),
        &super_claims(),
        &patch(
            "shadow",
            spec::ACTION_SEND_NOTICE,
            serde_json::json!({ "mode": "SHADOW" }),
        ),
    )
    .await
    .expect("保存影子规则");

    let ctx = LinkageContext::new(
        spec::TRIGGER_TASK,
        "DOING",
        fixture_doc_b(),
        "D2L-T-2",
        None,
    )
    .with_notice(draft_for(fixture_doc_b(), admin));
    match engine::status_notice(&ctx).await {
        NoticeRoute::Legacy {
            compare: Some(compare),
        } => {
            assert!(
                compare.matched,
                "默认参数下对比必须一致，差异：{:?}",
                compare.diffs
            );
        }
        other => panic!("影子模式应返回 Legacy + compare，实际：{other:?}"),
    }
    assert_eq!(
        notice_rows(fixture_doc_b()).await,
        0,
        "影子模式不许自己发通知（否则用户会收到两条）"
    );

    // 清掉规则，避免污染后续用例（setup 也会清，这里显式一次更好读）
    db().execute_with_params(
        "DELETE FROM biz_flow_linkage_rule WHERE name = 'D2L-shadow'",
        vec![],
    )
    .await
    .expect("清理影子规则");
}

/// 接管模式：配置发通知（旧逻辑不发），通知内容与旧逻辑逐字段一致。
#[tokio::test]
async fn status_notice_takeover_sends_configured_notice() {
    let Some(_guard) = setup().await else { return };
    let admin = admin_id().await;
    // 用一个真实存在的收件人（admin），否则第 3 层会把「都查不到人」当跳过
    let base = draft_for(fixture_doc_c(), admin);

    model::upsert_rule(
        db(),
        &super_claims(),
        &patch(
            "takeover",
            spec::ACTION_SEND_NOTICE,
            serde_json::json!({ "mode": "TAKEOVER" }),
        ),
    )
    .await
    .expect("保存接管规则");

    let ctx = LinkageContext::new(
        spec::TRIGGER_TASK,
        "DOING",
        fixture_doc_c(),
        "D2L-T-3",
        Some(admin),
    )
    .with_notice(base.clone());
    match engine::status_notice(&ctx).await {
        NoticeRoute::Configured {
            dispatched,
            compare,
            ..
        } => {
            assert!(dispatched, "接管模式必须真的发出去");
            assert!(
                compare.matched,
                "配置未覆盖任何字段，必须与旧逻辑一致：{:?}",
                compare.diffs
            );
        }
        other => panic!("接管模式应返回 Configured，实际：{other:?}"),
    }

    let rows = db()
        .query_sql(
            "SELECT n.title, n.content, n.biz_type, n.receiver_count::text AS receiver_count, \
                    (SELECT count(*)::text FROM sys_notice_receiver r WHERE r.notice_id = n.id) \
                        AS receivers \
               FROM sys_notice n WHERE n.biz_id = ? AND n.deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(fixture_doc_c().to_string())],
        )
        .await
        .expect("查通知");
    assert_eq!(rows.len(), 1, "接管后必须恰好 1 条通知（不能双发）");
    let title: String = rows[0].get_typed("title").expect("标题");
    let content: String = rows[0].get_typed("content").expect("正文");
    let biz_type: Option<String> = rows[0].get_typed("biz_type").expect("业务类型");
    let receivers: String = rows[0].get_typed("receivers").expect("接收人行数");
    assert_eq!(title, base.title, "标题必须与旧逻辑逐字一致");
    assert_eq!(content, base.content, "正文必须与旧逻辑逐字一致");
    assert_eq!(biz_type.as_deref(), Some("task"));
    assert_eq!(receivers, "1", "收件人 = admin 一个人");
}

/// 参数被绕过 API 写脏（`{"sql": ...}`）→ 运行时不执行、退回旧逻辑（不崩、不丢通知）。
#[tokio::test]
async fn status_notice_bad_params_fall_back_to_legacy() {
    let Some(_guard) = setup().await else { return };
    // 直接写库：CHECK 会拦住 sql 键，所以用一个「类型不对」的合法键（runtime 第 3 层要拦）
    //
    // 触发点用 TASK/REVIEW（不是 TASK/DOING）：V0254 的预置规则占了
    // `TASK/DOING/SEND_NOTICE`（唯一键），在带预置数据的库上直插同一键会撞唯一约束。
    // 本用例只关心「脏参数能不能退回旧逻辑」，换一个没被预置占用的合法状态即可。
    db().execute_with_params(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, enabled, seq) \
         VALUES (gen_random_uuid(), 'D2L-bad-params', 'TASK', 'REVIEW', 'SEND_NOTICE', \
                 '{\"mode\": \"TAKEOVER_XXX\"}'::jsonb, true, 1)",
        vec![],
    )
    .await
    .expect("脏参数能写进库（绕过 API 的形态）");

    let ctx = LinkageContext::new(
        spec::TRIGGER_TASK,
        "REVIEW",
        fixture_doc_b(),
        "D2L-T-2",
        None,
    )
    .with_notice(sample_draft());
    let route = engine::status_notice(&ctx).await;
    assert_eq!(
        route,
        NoticeRoute::Legacy { compare: None },
        "参数非法时必须退回旧逻辑（通知不能丢）"
    );
    assert_eq!(
        notice_rows(fixture_doc_b()).await,
        0,
        "退回旧逻辑时引擎自己不发"
    );
}

/// 通知行数（按 biz_id 数；`sys_notice.biz_id` 是 **varchar(64)**，不能绑 uuid）。
async fn notice_rows(biz_id: Uuid) -> i64 {
    db().scalar_sql(
        "SELECT count(*) FROM sys_notice WHERE biz_id = ? AND deleted_at IS NULL".to_string(),
        vec![Value::Text(biz_id.to_string())],
    )
    .await
    .unwrap_or_else(|error| panic!("查通知行数失败：{error}"))
}

// ==================== 【D3】GENERATE_VOUCHER 接管 ====================

/// 收货单夹具 1（正常金额：不含税 100 + 税 13 = 价税合计 113）。
fn fixture_si_ok() -> Uuid {
    uuid_of("00101")
}
/// 收货单夹具 2（第二张正常单，用于「接管后不再走旧逻辑」的对照）。
fn fixture_si_ok2() -> Uuid {
    uuid_of("00102")
}
/// 收货单夹具 3（金额全 0：单据不完整，凭证不该生成）。
fn fixture_si_zero() -> Uuid {
    uuid_of("00103")
}

/// 造一张收货单 + 一行明细（**只写凭证取数用得到的列**）。
///
/// 物料 / 单位 / 仓库 / 供应商用固定假 uuid：`biz_stock_in` 与 `biz_stock_in_item`
/// 上没有主数据外键（实测 `pg_constraint` 只有 item → header 一条），
/// 而科目解析会按 `biz_voucher_rule(PURCHASE_IN)` 拿到 1405 / 2202，不需要物料档案。
async fn seed_stock_in(id: Uuid, seq: u32, date: &str, amount: &str, tax: &str) {
    let with_tax = format!(
        "{}",
        amount.parse::<f64>().unwrap_or(0.0) + tax.parse::<f64>().unwrap_or(0.0)
    );
    db().execute_with_params(
        "INSERT INTO biz_stock_in \
            (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status, \
             total_qty, total_amount, total_tax, total_with_tax, is_sourceless) \
         VALUES (?::uuid, ?, 'PURCHASE', NULL, ?::uuid, ?::uuid, ?::text::date, 'POSTED', \
                 1, ?::text::numeric, ?::text::numeric, ?::text::numeric, true) \
         ON CONFLICT (id) DO UPDATE SET in_date = EXCLUDED.in_date, \
             total_amount = EXCLUDED.total_amount, total_tax = EXCLUDED.total_tax, \
             total_with_tax = EXCLUDED.total_with_tax",
        vec![
            Value::from(&id),
            Value::Text(format!("{PREFIX}SI-{seq}")),
            Value::from(&uuid_of("00007")),
            Value::from(&fixture_warehouse()),
            Value::Text(date.to_string()),
            Value::Text(amount.to_string()),
            Value::Text(tax.to_string()),
            Value::Text(with_tax.clone()),
        ],
    )
    .await
    .unwrap_or_else(|error| panic!("造收货单失败：{error}"));
    db().execute_with_params(
        "INSERT INTO biz_stock_in_item \
            (id, stock_in_id, line_no, material_id, unit_id, qty, price, amount, tax_amount, with_tax_amount) \
         VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, 1, ?::text::numeric, ?::text::numeric, \
                 ?::text::numeric, ?::text::numeric) \
         ON CONFLICT (id) DO NOTHING",
        vec![
            Value::from(&uuid_of(&format!("0010{}", 4 + seq))),
            Value::from(&id),
            Value::from(&fixture_material()),
            Value::from(&fixture_unit()),
            Value::Text(amount.to_string()),
            Value::Text(amount.to_string()),
            Value::Text(tax.to_string()),
            Value::Text(with_tax),
        ],
    )
    .await
    .unwrap_or_else(|error| panic!("造收货明细失败：{error}"));
}

/// 内存里造一条 `GENERATE_VOUCHER` 规则（直接调 `prepare_voucher` 用，不落库）。
fn voucher_rule(params: serde_json::Value) -> model::LinkageRuleRow {
    model::LinkageRuleRow {
        id: Uuid::now_v7(),
        name: format!("{PREFIX}d3-ins"),
        trigger_entity: state_machine::ENTITY_STOCK_IN.to_string(),
        trigger_status: "POSTED".to_string(),
        action_code: spec::ACTION_GENERATE_VOUCHER.to_string(),
        action_params: params,
        enabled: true,
        seq: 10,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// 直接插一条 `GENERATE_VOUCHER` 规则（第 2 层已过；这里是测试夹具）。
async fn seed_voucher_rule(name: &str, params: serde_json::Value) {
    db().execute_with_params(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, enabled, seq) \
         VALUES (?::uuid, ?, 'STOCK_IN', 'POSTED', 'GENERATE_VOUCHER', ?::text::jsonb, true, 10) \
         ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE \
            SET action_params = EXCLUDED.action_params, name = EXCLUDED.name, enabled = true",
        vec![
            Value::from(&Uuid::now_v7()),
            Value::Text(format!("{PREFIX}{name}")),
            Value::Text(params.to_string()),
        ],
    )
    .await
    .unwrap_or_else(|error| panic!("插凭证规则失败：{error}"));
}

/// 收货过账的联动上下文（与 `post_in_tx` 里构造的逐字一致）。
fn stock_in_ctx(id: Uuid, in_no: &str) -> LinkageContext<'static> {
    let in_no: &'static str = Box::leak(in_no.to_string().into_boxed_str());
    LinkageContext::new(state_machine::ENTITY_STOCK_IN, "POSTED", id, in_no, None)
}

/// 某来源单据的凭证张数。
async fn vouchers_of(doc_id: Uuid) -> i64 {
    db().scalar_sql(
        "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_IN' \
          AND source_id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(&doc_id)],
    )
    .await
    .unwrap_or_else(|error| panic!("查凭证张数失败：{error}"))
}

/// ① 参数非法 → 400（配置写入）：缺 `biz_type` / `mode` 不在枚举 / 未声明的键。
#[tokio::test]
async fn voucher_action_params_are_validated_on_write() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    let cases: Vec<(&str, serde_json::Value, &str)> = vec![
        (
            "缺必填 biz_type",
            serde_json::json!({ "mode": "TAKEOVER" }),
            "biz_type",
        ),
        (
            "mode 不在枚举",
            serde_json::json!({ "biz_type": "PURCHASE_IN", "mode": "TAKEOVER_XXX" }),
            "mode",
        ),
        (
            "未声明的键",
            serde_json::json!({ "biz_type": "PURCHASE_IN", "bogus": 1 }),
            "bogus",
        ),
        (
            "SQL 键",
            serde_json::json!({ "biz_type": "PURCHASE_IN", "sql": "select 1" }),
            "SQL",
        ),
    ];
    for (label, params, expect) in cases {
        let error = model::upsert_rule(
            &tx,
            &super_claims(),
            &patch("d3-voucher-bad", spec::ACTION_GENERATE_VOUCHER, params),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("{label}：非法配置必须被拒"));
        assert_eq!(error.status, 400, "{label} 实际：{error:?}");
        assert!(
            error.msg.contains(expect),
            "{label} 的文案应点名 `{expect}`，实际：{}",
            error.msg
        );
    }
    // 合法配置能写进去（已接管，不再是「尚未接管」）
    model::upsert_rule(
        &tx,
        &super_claims(),
        &patch(
            "d3-voucher-ok",
            spec::ACTION_GENERATE_VOUCHER,
            serde_json::json!({ "biz_type": "PURCHASE_IN", "mode": "SHADOW" }),
        ),
    )
    .await
    .expect("已接管的动作必须能写配置");
    tx.rollback().await.expect("回滚");
}

/// ② 运行时 target 不存在 → 跳过（ERROR 日志 + 不写任何凭证）。
#[tokio::test]
async fn voucher_action_skips_missing_target() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    let rule = voucher_rule(serde_json::json!({
        "biz_type": "PURCHASE_IN",
        "mode": "TAKEOVER",
    }));
    // 单据不存在
    let ctx = stock_in_ctx(Uuid::now_v7(), "D2L-SI-MISSING");
    let error = actions::prepare_voucher(&tx, &rule, &ctx)
        .await
        .expect_err("单据不存在必须跳过");
    assert!(
        matches!(&error, ActionError::Skipped(reason) if reason.contains("不存在")),
        "实际：{error:?}"
    );
    tx.rollback().await.expect("回滚");
}

/// ③ 凭证模板停用 / 模板码不存在 → 跳过（不是失败，凭证由旧逻辑照发）。
#[tokio::test]
async fn voucher_action_skips_inactive_template() {
    let Some(_guard) = setup().await else { return };
    let tx = db().begin().await.expect("开事务");
    let rule = voucher_rule(serde_json::json!({
        "biz_type": "PURCHASE_IN",
        "template_code": "NO_SUCH_TEMPLATE",
        "mode": "TAKEOVER",
    }));
    let ctx = stock_in_ctx(fixture_si_ok(), "D2L-SI-1");
    let error = actions::prepare_voucher(&tx, &rule, &ctx)
        .await
        .expect_err("模板停用必须跳过");
    assert!(
        matches!(&error, ActionError::Skipped(reason) if reason.contains("未启用")),
        "实际：{error:?}"
    );
    tx.rollback().await.expect("回滚");
}

/// ④ 金额全 0 的单据（单据不完整）→ 跳过，不去凑一张 2 行的假凭证。
#[tokio::test]
async fn voucher_action_skips_zero_amount_doc() {
    let Some(_guard) = setup().await else { return };
    seed_stock_in(fixture_si_zero(), 3, "2026-11-10", "0", "0").await;
    let tx = db().begin().await.expect("开事务");
    let rule = voucher_rule(serde_json::json!({
        "biz_type": "PURCHASE_IN",
        "mode": "TAKEOVER",
    }));
    let ctx = stock_in_ctx(fixture_si_zero(), "D2L-SI-3");
    let error = actions::prepare_voucher(&tx, &rule, &ctx)
        .await
        .expect_err("金额全 0 必须跳过");
    assert!(
        matches!(&error, ActionError::Skipped(reason) if reason.contains("金额全为 0")),
        "实际：{error:?}"
    );
    tx.rollback().await.expect("回滚");
}

/// ⑤ **新旧两路生成的凭证逐字段一致**（抽公共函数后的核心回归）。
///
/// 两条路都在各自的事务里跑完、取快照、回滚 —— 谁都不留数据，
/// 所以连凭证号都必须一样（`biz_seq` 的 bump 也被回滚了）。
#[tokio::test]
async fn voucher_new_path_matches_legacy_field_by_field() {
    let Some(_guard) = setup().await else { return };
    seed_stock_in(fixture_si_ok(), 1, "2026-11-10", "100", "13").await;
    let header = stockin_model::header_on(db(), fixture_si_ok())
        .await
        .expect("取单头")
        .expect("单头必须存在");
    let material_lines = stockin_model::material_lines_of(db(), fixture_si_ok())
        .await
        .expect("取按物料聚合的明细行");

    // ---- 路 1：旧路径 ----
    let tx = db().begin().await.expect("开事务（旧路）");
    let legacy_id = voucher::create_for_stock_in(&tx, &header, material_lines, None, Utc::now())
        .await
        .expect("旧路生成凭证");
    let legacy = voucher::snapshot_of(&tx, legacy_id)
        .await
        .expect("旧路凭证快照");
    tx.rollback().await.expect("回滚旧路");

    // ---- 路 2：配置路径（TAKEOVER）----
    seed_voucher_rule(
        "d3-voucher-takeover",
        serde_json::json!({ "biz_type": "PURCHASE_IN", "mode": "TAKEOVER" }),
    )
    .await;
    let ctx = stock_in_ctx(fixture_si_ok(), "D2L-SI-1");
    let tx2 = db().begin().await.expect("开事务（配置路）");
    let route = engine::stockin_voucher(&tx2, &ctx)
        .await
        .expect("配置路不该失败");
    let configured_id = match route {
        engine::VoucherRoute::Configured { voucher_id, .. } => voucher_id,
        other => panic!("TAKEOVER 必须由配置落账，实际：{other:?}"),
    };
    let configured = voucher::snapshot_of(&tx2, configured_id)
        .await
        .expect("配置路凭证快照");
    tx2.rollback().await.expect("回滚配置路");

    let diffs = voucher::compare_snapshots(&configured, &legacy);
    assert!(
        diffs.is_empty(),
        "新旧两路凭证必须逐字段一致，差异：{diffs:?}"
    );
    // 8 项以上硬断言（卡面要求，别只靠 compare 的空数组）
    assert_eq!(configured.voucher_count, 1, "凭证张数");
    assert_eq!(configured.voucher_word, legacy.voucher_word, "凭证字");
    assert_eq!(configured.voucher_no, legacy.voucher_no, "凭证号");
    assert_eq!(configured.total_debit, legacy.total_debit, "借方合计");
    assert_eq!(configured.total_credit, legacy.total_credit, "贷方合计");
    assert_eq!(configured.entry_count, legacy.entry_count, "分录行数");
    assert_eq!(configured.period, legacy.period, "会计期间");
    assert_eq!(configured.source_type, legacy.source_type, "关联单据类型");
    assert_eq!(configured.source_id, legacy.source_id, "关联单据 id");
    assert_eq!(configured.summary, legacy.summary, "摘要");
    assert_eq!(configured.lines.len(), legacy.lines.len(), "分录逐行");
    assert_eq!(
        configured
            .lines
            .iter()
            .map(|l| l.account_code.clone())
            .collect::<Vec<_>>(),
        legacy
            .lines
            .iter()
            .map(|l| l.account_code.clone())
            .collect::<Vec<_>>(),
        "逐行科目编码"
    );
    assert!(
        configured.total_debit > Decimal::ZERO,
        "夹具金额必须非 0（否则这条用例证明不了什么）"
    );
}

/// ⑥ SHADOW：配置路径真跑一遍但**回滚**，库里不多出凭证；对比一致才允许接管。
#[tokio::test]
async fn voucher_shadow_rolls_back_and_compares() {
    let Some(_guard) = setup().await else { return };
    seed_stock_in(fixture_si_ok2(), 2, "2026-11-11", "200", "26").await;
    seed_voucher_rule(
        "d3-voucher-shadow",
        serde_json::json!({ "biz_type": "PURCHASE_IN", "mode": "SHADOW" }),
    )
    .await;
    let header = stockin_model::header_on(db(), fixture_si_ok2())
        .await
        .expect("取单头")
        .expect("单头必须存在");
    let ctx = stock_in_ctx(fixture_si_ok2(), "D2L-SI-2");

    let tx = db().begin().await.expect("开事务");
    let route = engine::stockin_voucher(&tx, &ctx)
        .await
        .expect("影子不该失败");
    let shadow = match route {
        engine::VoucherRoute::Legacy {
            shadow: Some(shadow),
            ..
        } => shadow,
        other => panic!("SHADOW 必须退回旧逻辑并带影子快照，实际：{other:?}"),
    };
    assert!(
        vouchers_of(fixture_si_ok2()).await == 0,
        "影子阶段库里不能有凭证（试算已回滚）"
    );
    assert!(
        shadow.snapshot.lines.len() >= 2,
        "影子凭证至少 2 行分录，实际 {}",
        shadow.snapshot.lines.len()
    );
    assert_eq!(
        shadow.snapshot.source_id,
        Some(fixture_si_ok2()),
        "影子的关联单据必须是这张收货单"
    );

    // 旧路径落账 → 事后逐字段对比
    let material_lines = stockin_model::material_lines_of(db(), fixture_si_ok2())
        .await
        .expect("取按物料聚合的明细行");
    let legacy_id = voucher::create_for_stock_in(&tx, &header, material_lines, None, Utc::now())
        .await
        .expect("旧路生成凭证");
    let compare = engine::finish_stockin_voucher_shadow(&tx, &ctx, &shadow, legacy_id).await;
    assert!(
        compare.matched,
        "影子与旧路径必须一致，差异：{:?}",
        compare.diffs
    );
    assert!(compare.diffs.is_empty());
    tx.rollback().await.expect("回滚");
}

/// ⑦ 关键动作失败（期间已结账）→ 整笔回滚，不留半张凭证。
#[tokio::test]
async fn voucher_action_fails_and_rolls_back_when_period_closed() {
    let Some(_guard) = setup().await else { return };
    // 专用期间（2031-03）钉成 CLOSED，避免影响别的用例
    db().execute_with_params(
        "INSERT INTO biz_accounting_period \
            (period_year, period_month, period_code, start_date, end_date, status) \
         VALUES (2031, 3, '2031-03', '2031-03-01', '2031-03-31', 'CLOSED') \
         ON CONFLICT (period_code) DO UPDATE SET status = 'CLOSED'",
        vec![],
    )
    .await
    .expect("造已结账期间");
    seed_stock_in(fixture_si_ok(), 1, "2031-03-15", "100", "13").await;
    seed_voucher_rule(
        "d3-voucher-closed",
        serde_json::json!({ "biz_type": "PURCHASE_IN", "mode": "TAKEOVER" }),
    )
    .await;
    let ctx = stock_in_ctx(fixture_si_ok(), "D2L-SI-1");

    let tx = db().begin().await.expect("开事务");
    // 先写一条「过账前的状态」当哨兵：回滚必须把它一起带走
    tx.execute_with_params(
        "INSERT INTO biz_status_log (id, entity_type, entity_id, entity_no, from_status, to_status, created_at) \
         VALUES (?::uuid, 'STOCK_IN', ?::uuid, 'D2L-SI-1', 'DRAFT', 'POSTED', NOW())",
        vec![Value::from(&Uuid::now_v7()), Value::from(&fixture_si_ok())],
    )
    .await
    .expect("写过账状态日志");
    let error = engine::stockin_voucher(&tx, &ctx)
        .await
        .expect_err("期间已结账必须是关键动作失败");
    assert!(
        error.msg.contains("已结账"),
        "错误文案要点名期间已结账，实际：{}",
        error.msg
    );
    tx.rollback().await.expect("整笔回滚");

    assert_eq!(
        vouchers_of(fixture_si_ok()).await,
        0,
        "关键动作失败后不许留下半张凭证"
    );
    let logs: i64 = db()
        .scalar_sql(
            "SELECT count(*) FROM biz_status_log WHERE entity_id = ?::uuid".to_string(),
            vec![Value::from(&fixture_si_ok())],
        )
        .await
        .expect("查状态日志");
    assert_eq!(logs, 0, "整笔回滚必须把过账状态日志也带走（不留半条）");
    // 清掉专用期间
    db().execute_with_params(
        "DELETE FROM biz_accounting_period WHERE period_code = '2031-03'",
        vec![],
    )
    .await
    .expect("清期间");
}
