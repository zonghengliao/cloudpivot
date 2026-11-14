//! 联动**预置规则覆盖率**的漂移守卫（E2E-07 · FIX-2：CONTRACT 零规则问题）。
//!
//! # 这些用例在保护什么
//!
//! `V0254` / `V0323` / `V0351` / `V0360` 是「预置规则」的落地点。迁移一旦应用就不能改
//! （指纹校验），所以最大的风险不是「写错了」，是**代码与预置数据漂移**：
//!
//! | 漂移 | 后果 | 钉住它的用例 |
//! |------|------|--------------|
//! | 新接一个触发点（`wired = true`）却忘了种规则 | 「接线了但零规则」= 该能力不存在（E2E-07 的 CONTRACT 就是这个病） | [`every_wired_trigger_has_a_preset`] |
//! | 触发状态改名 / 写错 | 规则配得上、永远不触发 | [`every_preset_status_is_a_real_status`] |
//! | 动作改回占位 | 规则在跑但什么都不做 | [`every_preset_action_is_implemented`] |
//! | `mode` 从 `TAKEOVER` 改成 `SHADOW` | 规则静默失效（只做影子对比，不投递） | [`every_preset_is_takeover`] |
//! | 参数契约变了（键 / 枚举） | 迁移里的参数过不了第 2 层校验 | [`every_preset_params_pass_layer2_validation`] |
//! | 不变量脚本的状态表 / 动作表 / 禁令表过时 | 脚本静默放行（假绿） | [`invariants_script_status_table_matches_spec`] 等 |
//!
//! # 为什么不查库
//!
//! 「迁移有没有真的应用」由 `scripts/verify/linkage_rule_invariants.sh` 在真库上验
//! （wired 但零 enabled 规则 → rc=1）。这里只钉「迁移文本 ↔ 代码契约」的一致性，
//! 跑得快、任何环境都能跑（不需要 `ERP_DB_URL`）。

use super::spec;

/// 全部**种规则的**迁移原文（顺序 = 迁移号顺序）。
const PRESET_MIGRATIONS: &[&str] = &[
    include_str!("../../../../migrations/business/V0254__flow_linkage_presets.up.sql"),
    include_str!("../../../../migrations/business/V0323__asset_linkage_rules.up.sql"),
    include_str!("../../../../migrations/business/V0351__purchase_return_linkage_rule.up.sql"),
    include_str!("../../../../migrations/business/V0360__linkage_rules_coverage.up.sql"),
];

/// 本卡（E2E-07 · FIX-2）的迁移原文（up / down）。
const UP: &str = include_str!("../../../../migrations/business/V0360__linkage_rules_coverage.up.sql");
const DOWN: &str = include_str!("../../../../migrations/business/V0360__linkage_rules_coverage.down.sql");

/// 库侧不变量脚本原文（写死在里面的是触发点 / 状态 / 动作清单，必须与代码同口径）。
const INVARIANTS: &str = include_str!("../../../../../scripts/verify/linkage_rule_invariants.sh");

/// 一条预置规则的「代码侧期望值」（与迁移逐字对拍）。
struct Preset {
    name: &'static str,
    entity: &'static str,
    status: &'static str,
    action: &'static str,
    /// 迁移里 `'…'::jsonb` 之间的原文（含空格）。
    params: &'static str,
    seq: i32,
}

/// V0360 的 15 条预置规则（E2E-07 · FIX-2）。
const PRESETS: &[Preset] = &[
    Preset {
        name: "合同签订 → 通知制单人",
        entity: "CONTRACT",
        status: "SIGNED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "合同终止 → 通知制单人",
        entity: "CONTRACT",
        status: "TERMINATED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 20,
    },
    Preset {
        name: "出库过账 → 通知制单人",
        entity: "STOCK_OUT",
        status: "POSTED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "调拨完成 → 通知制单人",
        entity: "STOCK_TRANSFER",
        status: "DONE",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "采购订单全部收货 → 通知制单人",
        entity: "PURCHASE_ORDER",
        status: "RECEIVED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "应付结清 → 通知制单人",
        entity: "PAYABLE",
        status: "SETTLED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "凭证记账 → 通知制单人",
        entity: "VOUCHER",
        status: "POSTED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "会计期间结账 → 通知操作人",
        entity: "ACCOUNTING_PERIOD",
        status: "CLOSED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "会计年度结转 → 通知操作人",
        entity: "ACCOUNTING_YEAR",
        status: "CLOSED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "盘点过账 → 通知制单人",
        entity: "STOCK_TAKE",
        status: "POSTED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "折旧过账 → 通知制单人",
        entity: "DEPRECIATION",
        status: "POSTED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "报销付清 → 通知制单人",
        entity: "EXPENSE_REPORT",
        status: "PAID",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "任务开工 → 通知负责人与创建人",
        entity: "TASK",
        status: "DOING",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "审批通过 → 通知发起人",
        entity: "FLOW_INSTANCE",
        status: "APPROVED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
    Preset {
        name: "审批驳回 → 通知发起人",
        entity: "FLOW_INSTANCE",
        status: "REJECTED",
        action: spec::ACTION_SEND_NOTICE,
        params: r#"{"mode": "TAKEOVER"}"#,
        seq: 10,
    },
];

/// 把连续空白压成一个空格（只用于「文本里有没有这段」的比对，不改写 SQL / shell）。
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

/// 取不变量脚本里 `# BEGIN <name>` / `# END <name>` 之间的块（原样返回，只 trim 每行）。
///
/// 这三张表是「脚本与代码同口径」的唯一落点：脚本在库上跑，判据在代码里，
/// 中间靠本函数把两边钉在一起 —— 表过期了脚本会静默放行（假绿），所以必须红。
fn script_block(name: &str) -> String {
    let begin = format!("# BEGIN {name}");
    let end = format!("# END {name}");
    let from = INVARIANTS
        .find(&begin)
        .unwrap_or_else(|| panic!("不变量脚本里找不到标记 `{begin}`"));
    let rest = &INVARIANTS[from + begin.len()..];
    let to = rest
        .find(&end)
        .unwrap_or_else(|| panic!("不变量脚本里找不到标记 `{end}`"));
    rest[..to].to_string()
}

/// 把 `ENTITY|STATUS` / `ENTITY|ACTION` 这样的行解析成有序对（保持脚本里的顺序）。
///
/// 忽略块里的 shell 包装行（`STATUS_PAIRS="` / 结尾的 `"`）：只认「恰好两段、且都是大写标识符」的行。
fn pairs_of(name: &str) -> Vec<(String, String)> {
    script_block(name)
        .lines()
        .map(str::trim)
        .filter_map(|line| {
            let mut parts = line.split('|');
            let left = parts.next()?.trim();
            let right = parts.next()?.trim();
            if parts.next().is_some() {
                return None;
            }
            if !is_identifier(left) || !is_identifier(right) {
                return None;
            }
            Some((left.to_string(), right.to_string()))
        })
        .collect()
}

/// 把块里的**标识符**逐个取出来（跳过 `VAR="` / `"` 这类 shell 包装）。
fn words_of(name: &str) -> Vec<String> {
    script_block(name)
        .split_whitespace()
        .filter(|token| is_identifier(token))
        .map(str::to_string)
        .collect()
}

/// `STOCK_IN` / `SEND_NOTICE` 这种全大写标识符（shell 包装、引号、注释一律不算）。
fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

/// 去掉全部空白（用于「SQL 文本里有没有这个键」的比对：迁移里为了对齐会塞很多空格）。
fn tight(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

// ==================== ① 覆盖率：wired ⇒ 有预置 ====================

/// **本卡的核心漂移守卫**：`wired = true` 的触发点，必须在**预置迁移**里至少出现一次
/// 合法状态（否则就是「接线了但零 enabled 规则」= 该能力不存在）。
///
/// 这条用例同时是「以后新接一个触发点，忘了种规则就红」的闸门 —— 不需要人去记
/// 「新增 wired 触发点要配迁移」（E2E-07 的 CONTRACT 就是这么漏的）。
#[test]
fn every_wired_trigger_has_a_preset() {
    let all = squeeze_ws(&PRESET_MIGRATIONS.join("\n"));
    let mut gaps: Vec<String> = Vec::new();
    for entity in spec::wired_triggers() {
        let spec_row = spec::trigger_spec(entity).expect("wired 触发点必须在 TRIGGER_SPECS 里");
        let covered = spec_row.status_labels.iter().any(|(status, _)| {
            all.contains(&format!("'{entity}', '{status}'")) || all.contains(&format!("'{entity}','{status}'"))
        });
        if !covered {
            gaps.push(entity.to_string());
        }
    }
    assert!(
        gaps.is_empty(),
        "以下 wired 触发点在全部预置迁移（V0254/V0323/V0351/V0360）里一条规则都没有：\
         {gaps:?} —— 接线了但零 enabled 规则 = 该触发点的联动能力不存在（E2E-07 的 CONTRACT 病）。\
         请在新的迁移里给每个触发点至少种一条落在它真实状态上的规则"
    );
}

/// 预置迁移不许给**未接线**的触发点种规则（配得上、永远不触发）。
#[test]
fn presets_only_for_wired_triggers() {
    let all = squeeze_ws(&PRESET_MIGRATIONS.join("\n"));
    let mut unwired: Vec<&str> = spec::unwired_triggers();
    unwired.extend(spec::TRIGGER_EXEMPT.iter().copied());
    assert!(!unwired.is_empty(), "未接线实体清单为空：这条守卫就变成空跑了");
    for entity in unwired {
        assert!(
            !all.contains(&format!("'{entity}', '")),
            "{entity} 没接线（wired = false / 在 TRIGGER_EXEMPT 里），预置迁移里不许给它种规则（配了永远不触发）"
        );
    }
}

// ==================== ② 每条预置规则本身必须合法 ====================

/// 触发点必须在白名单里、且**已接线**（未接线的触发点配规则等于配了个开关没接线）。
#[test]
fn every_preset_trigger_is_wired() {
    for preset in PRESETS {
        assert!(
            spec::is_known_trigger(preset.entity),
            "规则「{}」的触发点 {} 不在白名单里",
            preset.name,
            preset.entity
        );
        assert!(
            spec::is_wired_trigger(preset.entity),
            "规则「{}」的触发点 {} 未接线：配了永远不触发",
            preset.name,
            preset.entity
        );
    }
}

/// 触发状态必须该触发点的**真实状态**（写错状态的规则永远等不到触发）。
#[test]
fn every_preset_status_is_a_real_status() {
    for preset in PRESETS {
        let spec_row = spec::trigger_spec(preset.entity).expect("触发点必须在接线表里");
        assert!(
            spec_row
                .status_labels
                .iter()
                .any(|(status, _)| *status == preset.status),
            "规则「{}」的触发状态 {} 不在 {} 的状态集里（可用：{:?}）",
            preset.name,
            preset.status,
            preset.entity,
            spec_row.status_labels.iter().map(|(code, _)| *code).collect::<Vec<_>>()
        );
        assert!(
            super::model::known_statuses(preset.entity).contains(&preset.status),
            "规则「{}」的状态 {} 与状态机（model::known_statuses）对不上",
            preset.name,
            preset.status
        );
    }
}

/// 动作必须已接管（占位动作配得上但执行时 Skipped）。
#[test]
fn every_preset_action_is_implemented() {
    for preset in PRESETS {
        let action = spec::spec_of(preset.action)
            .unwrap_or_else(|| panic!("规则「{}」的动作码 {} 不在白名单里", preset.name, preset.action));
        assert!(
            action.implemented,
            "规则「{}」的动作 {} 仍是占位（implemented = false）：配得上但执行时被跳过",
            preset.name, preset.action
        );
        assert_eq!(
            action.phase.as_str(),
            "AFTER_COMMIT",
            "本卡的预置规则都是提交后动作（发通知）"
        );
    }
}

/// 动作不许是该实体结构性被禁的（合同出凭证 / 凭证生凭证 / 审批实例落账）。
#[test]
fn every_preset_action_is_not_forbidden_on_its_trigger() {
    for preset in PRESETS {
        assert!(
            spec::forbidden_action_reason(preset.entity, preset.action).is_none(),
            "规则「{}」的动作 {} 在 {} 上是结构性被禁的（配置期就该 400）",
            preset.name,
            preset.action,
            preset.entity
        );
    }
}

/// 参数必须过第 2 层校验，且 `mode = TAKEOVER`（`SHADOW` 只做影子对比、不投递）。
#[test]
fn every_preset_params_pass_layer2_validation() {
    for preset in PRESETS {
        let action = spec::spec_of(preset.action).expect("动作码必须在白名单里");
        let params: serde_json::Value = serde_json::from_str(preset.params).expect("参数必须是合法 JSON");
        if let Err(reason) = spec::validate_params(action, &params) {
            panic!("规则「{}」的参数过不了第 2 层校验：{reason}", preset.name);
        }
        assert_eq!(
            params.get("mode").and_then(serde_json::Value::as_str),
            Some("TAKEOVER"),
            "规则「{}」不是 TAKEOVER：预置规则必须真生效，不能是只做对比的影子",
            preset.name
        );
        for key in params.as_object().expect("参数必须是对象").keys() {
            assert!(
                !spec::is_forbidden_key(key),
                "规则「{}」的参数里出现禁用键 `{key}`（禁配 SQL）",
                preset.name
            );
        }
    }
}

/// `(trigger_entity, trigger_status, action_code)` 必须唯一（表上有唯一约束）。
#[test]
fn preset_keys_are_unique() {
    let mut seen: Vec<(&str, &str, &str)> = Vec::new();
    // 已经由更早的迁移占掉的键（V0254 / V0323 / V0351）也要算进来，
    // 否则「新迁移重复种同一键」会在生产上被 ON CONFLICT 静默吞掉（假生效）。
    let earlier = [
        ("STOCK_IN", "POSTED", "GENERATE_VOUCHER"),
        ("STOCK_IN", "POSTED", "SEND_NOTICE"),
        // `TASK/DOING` 不在预置「历史键」里：本迁移**故意重种**它（145 演练清场时 V0254 那行被硬删），
        // 属于幂等补齐，不是重复配置。
        ("FIXED_ASSET", "IDLE", "SEND_NOTICE"),
        ("FIXED_ASSET", "IN_USE", "SEND_NOTICE"),
        ("FIXED_ASSET", "DISPOSED", "SEND_NOTICE"),
        ("PURCHASE_RETURN", "CONFIRMED", "SEND_NOTICE"),
    ];
    seen.extend(earlier);
    for preset in PRESETS {
        let key = (preset.entity, preset.status, preset.action);
        assert!(!seen.contains(&key), "预置规则 {key:?} 重复：唯一约束会拒");
        seen.push(key);
    }
}

// ==================== ③ 迁移原文与代码对拍 ====================

/// 迁移里逐条包含规则名 / 触发键 / 参数 / seq。
#[test]
fn migration_seeds_every_preset_row() {
    // 迁移里为了对齐塞了很多空格（`'CONTRACT',          'SIGNED'`），比对前先压掉全部空白
    let packed = tight(UP);
    for preset in PRESETS {
        assert!(UP.contains(preset.name), "迁移里找不到规则名「{}」", preset.name);
        let key = tight(&format!(
            "'{}', '{}', '{}'",
            preset.entity, preset.status, preset.action
        ));
        assert!(
            packed.contains(&key),
            "迁移里找不到触发键/动作码 {key}（规则「{}」）",
            preset.name
        );
        let params = tight(&format!("'{}'::jsonb", preset.params));
        assert!(
            packed.contains(&params),
            "迁移里找不到参数原文 {params}（规则「{}」）",
            preset.name
        );
        let seq = tight(&format!("true, {}", preset.seq));
        assert!(
            packed.contains(&seq),
            "迁移里找不到 `enabled, seq` 片段 `{seq}`（规则「{}」）",
            preset.name
        );
    }
    // 15 行 INSERT（每行一个 `gen_random_uuid()`）
    assert_eq!(UP.matches("gen_random_uuid()").count(), 15, "V0360 应该有 15 行 INSERT");
}

/// down 删的是同一批键；**`TASK/DOING` 例外**（那是 V0254 的键，回滚本卡不该拆掉它）。
#[test]
fn migration_down_removes_the_same_keys_except_task() {
    let packed = tight(DOWN);
    assert!(
        DOWN.contains("DELETE FROM biz_flow_linkage_rule"),
        "down 必须真的删规则行"
    );
    for preset in PRESETS {
        if preset.entity == spec::TRIGGER_TASK {
            continue;
        }
        let key = tight(&format!(
            "('{}', '{}', '{}')",
            preset.entity, preset.status, preset.action
        ));
        assert!(
            packed.contains(&key),
            "down 里找不到 {key}（规则「{}」回滚不掉）",
            preset.name
        );
    }
    assert!(
        !packed.contains(&tight("('TASK', 'DOING', 'SEND_NOTICE')")),
        "down 不许删 TASK/DOING：它是 V0254 的基线能力（回滚本卡会静默关掉任务通知）"
    );
}

// ==================== ④ 不变量脚本与代码同口径 ====================

/// 脚本里的 `WIRED_TRIGGERS` 必须**恰好等于** `spec::wired_triggers()`（多一个 / 少一个都红）。
#[test]
fn invariants_script_lists_every_wired_trigger() {
    let scripted = words_of("WIRED_TRIGGERS");
    let expected: Vec<String> = spec::wired_triggers().iter().map(|entity| entity.to_string()).collect();
    assert_eq!(
        scripted, expected,
        "不变量脚本的 WIRED_TRIGGERS 与 spec::wired_triggers() 不一致：脚本过期会静默放行（假绿）"
    );
}

/// 脚本里的状态表必须**恰好等于** `TRIGGER_SPECS[].status_labels`（双向）。
#[test]
fn invariants_script_status_table_matches_spec() {
    let scripted = pairs_of("STATUS_TABLE");
    let mut expected: Vec<(String, String)> = Vec::new();
    for spec_row in spec::TRIGGER_SPECS {
        for (status, _) in spec_row.status_labels {
            expected.push((spec_row.entity.to_string(), (*status).to_string()));
        }
    }
    for (entity, status) in &expected {
        assert!(
            scripted.contains(&(entity.clone(), status.clone())),
            "不变量脚本的状态表缺 {entity}/{status}：这条状态上的坏规则会被静默放行"
        );
    }
    for (entity, status) in &scripted {
        assert!(
            expected.contains(&(entity.clone(), status.clone())),
            "不变量脚本的状态表多了 {entity}/{status}：它在代码里不存在（脚本会放行一条永远不触发的规则）"
        );
    }
}

/// 脚本里的「已接管动作」表必须恰好等于 `spec::implemented_codes()`。
#[test]
fn invariants_script_implemented_actions_match_spec() {
    let mut scripted = words_of("IMPLEMENTED_ACTIONS");
    scripted.sort();
    let mut expected: Vec<String> = spec::implemented_codes().iter().map(|code| code.to_string()).collect();
    expected.sort();
    assert_eq!(
        scripted, expected,
        "不变量脚本的 IMPLEMENTED_ACTIONS 与 spec::implemented_codes() 不一致：脚本会把占位动作当成可用"
    );
}

/// 脚本里的禁令表必须**双向**等于 `spec::forbidden_actions(entity)`（对每个白名单触发点）。
#[test]
fn invariants_script_forbidden_pairs_match_spec() {
    let scripted = pairs_of("FORBIDDEN_PAIRS");
    if scripted.is_empty() {
        panic!("不变量脚本的禁令表不能为空：至少要有 CONTRACT/VOUCHER 的 GENERATE_VOUCHER");
    }
    for entity in spec::TRIGGERS {
        for action in spec::forbidden_actions(entity) {
            assert!(
                scripted.contains(&(entity.to_string(), (*action).to_string())),
                "不变量脚本的禁令表缺 {entity}/{action}（这条脏规则会被放行）"
            );
            assert!(
                spec::spec_of(action).is_some(),
                "{entity} 的禁令里写了不在白名单的动作 `{action}`"
            );
        }
    }
    for (entity, action) in &scripted {
        assert!(
            spec::forbidden_actions(entity).contains(&action.as_str()),
            "不变量脚本的禁令表多了 {entity}/{action}：代码里这个动作在该实体上并没有被禁"
        );
    }
}

/// 脚本本身要能跑（bash 语法 + 有 4 条断言）。
#[test]
fn invariants_script_is_well_formed() {
    assert!(
        INVARIANTS.contains("① 每个 wired 触发点都有 ≥1 条 enabled 规则"),
        "脚本缺断言 ① 的输出"
    );
    assert!(
        INVARIANTS.contains("② enabled 规则全部挂在已接线的触发点上"),
        "脚本缺断言 ② 的输出"
    );
    assert!(
        INVARIANTS.contains("③ enabled 规则的状态全部是该触发点的真实状态"),
        "脚本缺断言 ③ 的输出"
    );
    assert!(
        INVARIANTS.contains("④ enabled 规则的动作全部已接管"),
        "脚本缺断言 ④ 的输出"
    );
    assert!(
        INVARIANTS.contains("exit 1"),
        "脚本必须有非零退出码（否则 CI 看不出红）"
    );
    assert!(
        INVARIANTS.contains("exit 2"),
        "表不存在 / 连不上库必须 rc=2，不能说全绿"
    );
}
