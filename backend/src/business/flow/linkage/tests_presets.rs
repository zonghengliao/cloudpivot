//! 跨单据联动**预置规则**的单测（纯逻辑，不需要数据库）。
//!
//! # 这些用例在保护什么
//!
//! `V0254` 是「3 条真实业务规则」的落地点，但迁移一旦应用就不能改（指纹校验）。
//! 于是最大的风险不是「写错了」——是**代码与迁移漂移**：
//!
//! - 有人把某个动作从「已接管」改回占位 → 预置规则变成配得上但执行时跳过的空壳；
//! - 有人改了动作的参数契约（键 / 枚举）→ 迁移里的参数可能就不合法了；
//! - 有人把触发状态改名 → 规则永远不触发；
//! - 有人把 `mode` 从 `TAKEOVER` 改成 `SHADOW` → 规则看似在跑，实际一张凭证都不生成。
//!
//! 所以这里用 `include_str!` 读**迁移原文**，逐条与代码侧的判据对拍：
//! 迁移里的每一行都必须能过 [`spec::validate_params`]（第 2 层校验），
//! 触发点/状态/动作码必须在白名单里，`mode` 必须是 `TAKEOVER`。
//! 任何一处漂移 → 用例红，而且文案直接说是哪条规则。
//!
//! # 为什么不查库
//!
//! 「迁移有没有真的应用」由 e2e（`scripts/verify/p1fix5b_linkage_e2e.py`）在真库上验证；
//! 这里只钉「迁移文本 ↔ 代码契约」的一致性，跑得快、任何环境都能跑。

use super::engine;
use super::spec;

/// 迁移原文（up / down）。
const UP: &str = include_str!("../../../../migrations/business/V0254__flow_linkage_presets.up.sql");
const DOWN: &str =
    include_str!("../../../../migrations/business/V0254__flow_linkage_presets.down.sql");

/// 一条预置规则的「代码侧期望值」（与迁移逐字对拍）。
struct Preset {
    /// 规则名（迁移 `name` 列）。
    name: &'static str,
    /// 触发单据类型。
    entity: &'static str,
    /// 触发状态。
    status: &'static str,
    /// 白名单动作码。
    action: &'static str,
    /// 参数 JSONB 字面量（迁移里 `'…'::jsonb` 之间的原文，含空格）。
    params: &'static str,
    /// 执行顺序。
    seq: i32,
}

/// 3 条预置业务规则（P1-FIX-5B）。
const PRESETS: &[Preset] = &[
    Preset {
        name: "收货过账 → 生成凭证（联动接管）",
        entity: "STOCK_IN",
        status: "POSTED",
        action: "GENERATE_VOUCHER",
        params: r#"{"biz_type": "PURCHASE_IN", "mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "收货过账 → 通知制单人",
        entity: "STOCK_IN",
        status: "POSTED",
        action: "SEND_NOTICE",
        params: r#"{"mode": "TAKEOVER", "recipient": "CREATOR"}"#,
        seq: 20,
    },
    Preset {
        name: "任务开工 → 通知负责人与创建人",
        entity: "TASK",
        status: "DOING",
        action: "SEND_NOTICE",
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
];

/// 1️⃣ 预置规则正好 3 条（卡面验收：3 条规则配好）。
#[test]
fn presets_are_exactly_three() {
    assert_eq!(PRESETS.len(), 3, "卡面要求 3 条预置规则");
    let inserts = UP.matches("gen_random_uuid()").count();
    assert_eq!(
        inserts, 3,
        "迁移 up.sql 里应有 3 行 INSERT（实际 {inserts}）"
    );
}

/// 2️⃣ 每条规则的触发点都在 `spec::TRIGGERS` 白名单里。
#[test]
fn every_preset_trigger_is_whitelisted() {
    for preset in PRESETS {
        assert!(
            spec::is_known_trigger(preset.entity),
            "规则「{}」的触发点 {} 不在白名单里",
            preset.name,
            preset.entity
        );
    }
}

/// 3️⃣ 每条规则的触发状态都是该单据的合法状态（写错状态的规则永远不触发）。
#[test]
fn every_preset_status_is_reachable() {
    for preset in PRESETS {
        let statuses = super::model::known_statuses(preset.entity);
        assert!(
            statuses.contains(&preset.status),
            "规则「{}」的触发状态 {} 不在 {} 的状态集里（可用：{}）",
            preset.name,
            preset.status,
            preset.entity,
            statuses.join(" / ")
        );
    }
}

/// 4️⃣ 每条规则的动作码都在白名单里**且已接管**（占位动作配得上但执行时什么都不做）。
#[test]
fn every_preset_action_is_implemented() {
    for preset in PRESETS {
        let action = spec::spec_of(preset.action).unwrap_or_else(|| {
            panic!(
                "规则「{}」的动作码 {} 不在白名单里",
                preset.name, preset.action
            )
        });
        assert!(
            action.implemented,
            "规则「{}」的动作 {} 仍是占位（implemented=false）：配得上但执行时会被跳过",
            preset.name, preset.action
        );
    }
}

/// 5️⃣ 每条规则的参数都能过第 2 层校验（键集合 / 必填 / 类型 / 枚举）。
#[test]
fn every_preset_params_pass_layer2_validation() {
    for preset in PRESETS {
        let action = spec::spec_of(preset.action).expect("动作码必须在白名单里");
        let params: serde_json::Value =
            serde_json::from_str(preset.params).expect("迁移里的参数必须是合法 JSON");
        if let Err(reason) = spec::validate_params(action, &params) {
            panic!("规则「{}」的参数过不了第 2 层校验：{reason}", preset.name);
        }
    }
}

/// 6️⃣ 参数里不许出现 SQL 类键（禁配 SQL 是本模块的核心禁令）。
#[test]
fn every_preset_params_have_no_sql_key() {
    for preset in PRESETS {
        let params: serde_json::Value = serde_json::from_str(preset.params).expect("合法 JSON");
        for key in params.as_object().expect("参数必须是对象").keys() {
            assert!(
                !spec::is_forbidden_key(key),
                "规则「{}」的参数里出现禁用键 `{key}`（禁配 SQL）",
                preset.name
            );
        }
    }
}

/// 7️⃣ 预置规则全部是 `TAKEOVER`，不是 `SHADOW`。
///
/// `SHADOW` 只做影子对比、真账仍由旧代码落 —— 那是灰度中间态。
/// 预置规则必须是真生效的：改成 SHADOW 而不同步改这条用例，测试立刻红。
#[test]
fn presets_are_takeover_not_shadow() {
    for preset in PRESETS {
        let params: serde_json::Value = serde_json::from_str(preset.params).expect("合法 JSON");
        assert_eq!(
            params.get("mode").and_then(|value| value.as_str()),
            Some("TAKEOVER"),
            "规则「{}」不是 TAKEOVER：预置规则必须真生效，不能是只做对比的影子",
            preset.name
        );
    }
}

/// 8️⃣ `(trigger_entity, trigger_status, action_code)` 不重复（表上有唯一约束）。
#[test]
fn preset_keys_are_unique() {
    let mut seen: Vec<(&str, &str, &str)> = Vec::new();
    for preset in PRESETS {
        let key = (preset.entity, preset.status, preset.action);
        assert!(
            !seen.contains(&key),
            "预置规则 {key:?} 重复：表唯一约束会拒，且两行参数的取哪份不可判"
        );
        seen.push(key);
    }
}

/// 9️⃣ 迁移原文逐条包含每条规则的名字 / 触发键 / 参数 / seq（文本与代码对拍）。
#[test]
fn migration_seeds_every_preset_row() {
    for preset in PRESETS {
        assert!(
            UP.contains(preset.name),
            "迁移里找不到规则名「{}」",
            preset.name
        );
        let key = format!(
            "'{}', '{}', '{}'",
            preset.entity, preset.status, preset.action
        );
        assert!(
            UP.contains(&key),
            "迁移里找不到触发键 / 动作码 {key}（规则「{}」）",
            preset.name
        );
        let params = format!("'{}'::jsonb", preset.params);
        assert!(
            UP.contains(&params),
            "迁移里找不到参数原文 {params}（规则「{}」）",
            preset.name
        );
        let seq = format!("true, {}", preset.seq);
        assert!(
            UP.contains(&seq),
            "迁移里找不到 `enabled, seq` 片段 `{seq}`（规则「{}」）",
            preset.name
        );
    }
}

/// 🔟 down.sql 删的是**同一批键**（回滚后回到「引擎读不到规则 = 不接管」）。
#[test]
fn migration_down_removes_the_same_keys() {
    // down.sql 为了可读性做了列对齐（`('TASK',     'REVIEW', …)`），先压掉多余空白再比
    let squeezed = squeeze_ws(DOWN);
    for preset in PRESETS {
        let key = format!(
            "('{}', '{}', '{}')",
            preset.entity, preset.status, preset.action
        );
        assert!(
            squeezed.contains(&key),
            "down.sql 里找不到 {key}（规则「{}」回滚不掉）",
            preset.name
        );
    }
    assert!(
        DOWN.contains("DELETE FROM biz_flow_linkage_rule"),
        "down.sql 必须真的删规则行"
    );
}

/// 把连续空白压成一个空格（只用于「文本里有没有这段」的比对，不改写 SQL）。
fn squeeze_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out
}

/// 1️⃣1️⃣ 通用引擎的「专用路由排除」判据：`GENERATE_VOUCHER` 被排除、`SEND_NOTICE` 不被排除。
///
/// 这是 `GENERATE_VOUCHER` 不会「一张单两张凭证」的代码级保证：
/// 收货过账 ⑦ 走 `stockin_voucher` 专用路由，⑧ 调 `engine::run_excluding(…, &[ACTION_GENERATE_VOUCHER])`。
#[test]
fn engine_skips_only_actions_routed_elsewhere() {
    let skip = [spec::ACTION_GENERATE_VOUCHER];
    assert!(
        engine::is_routed_elsewhere(&skip, spec::ACTION_GENERATE_VOUCHER),
        "GENERATE_VOUCHER 必须被通用引擎排除（它在 ⑦ 有专用路由）"
    );
    assert!(
        !engine::is_routed_elsewhere(&skip, spec::ACTION_SEND_NOTICE),
        "SEND_NOTICE 不能被通用引擎排除（收货过账的通知就靠它）"
    );
    assert!(
        !engine::is_routed_elsewhere(&skip, spec::ACTION_UPDATE_RECEIVED_QTY),
        "UPDATE_RECEIVED_QTY 不能被排除（事务内校验动作走通用引擎）"
    );
    assert!(
        !engine::is_routed_elsewhere(&[], spec::ACTION_GENERATE_VOUCHER),
        "没有排除清单时不得跳过任何动作（run 的默认行为）"
    );
}

/// 1️⃣2️⃣ 预置规则覆盖了「专用路由」和「通用引擎」两条执行入口。
///
/// - `GENERATE_VOUCHER` → `engine::stockin_voucher`（专用路由，要影子试算）
/// - `SEND_NOTICE` → `engine::run_deferred`（通用引擎的提交后动作）
///
/// 只覆盖一条入口 = 另一条永远没人验证（评审说的「引擎从未真正跑过」）。
#[test]
fn presets_cover_both_execution_entries() {
    let codes: Vec<&str> = PRESETS.iter().map(|preset| preset.action).collect();
    assert!(
        codes.contains(&spec::ACTION_GENERATE_VOUCHER),
        "预置规则必须覆盖专用路由 GENERATE_VOUCHER"
    );
    assert!(
        codes.contains(&spec::ACTION_SEND_NOTICE),
        "预置规则必须覆盖通用引擎的 SEND_NOTICE"
    );
    let entities: Vec<&str> = PRESETS.iter().map(|preset| preset.entity).collect();
    assert!(
        entities.contains(&super::spec::TRIGGER_TASK),
        "任务触发点（既有 status_notice 路由）也要有一条预置规则"
    );
}

/// 1️⃣3️⃣ 收货过账的调用点**真的**接了通用引擎，并且真的排除了专用路由动作。
///
/// 这条用例防的是最致命的一种漂移：`run_excluding` 本身好用、预置规则也配好了，
/// 但 `post_in_tx` 里那一行被删掉/改成 `run`（不加排除清单）——
/// 前者让「收货过账→通知」的规则永远不触发（配了白配），后者让一张单出两张凭证。
/// 两种都是「代码改一行、规则静默变形」，靠 e2e 才能发现，所以在这里钉住源码原文。
#[test]
fn stockin_post_wires_generic_engine_and_excludes_voucher() {
    let post = include_str!("../../erp/stockin_handler_post.rs");
    assert!(
        post.contains("linkage::engine::run_excluding("),
        "收货过账必须调用通用引擎（run_excluding）：否则 STOCK_IN/POSTED 的联动规则配了也不触发"
    );
    assert!(
        post.contains("&[linkage::spec::ACTION_GENERATE_VOUCHER]"),
        "收货过账调通用引擎时必须排除 GENERATE_VOUCHER（它在 ⑦ 有专用路由），否则一张单两张凭证"
    );
    assert!(
        post.contains("linkage::engine::run_deferred("),
        "提交后动作必须交给 run_deferred（commit 之后跑）：否则 SEND_NOTICE 这类 AfterCommit 动作永远不执行"
    );
}
