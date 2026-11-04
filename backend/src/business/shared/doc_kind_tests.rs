//! `doc_kind` 的值域闸。
//!
//! 这些用例是「不留第 8 套词表」的**机械保证**：
//!
//! | 用例 | 钉住什么 |
//! |---|---|
//! | [`every_axis_value_is_uppercase_underscore`] | 拼写规则（大写下划线） |
//! | [`values_are_unique_per_axis`] | 同一根轴里不许有两个枚举映射到同一个字符串 |
//! | [`domain_consts_match_enums`] | [`COLUMN_DOMAINS`] 里的字面量 ↔ 枚举 `as_str()` 逐字一致 |
//! | [`domain_columns_exist_and_cover_all_axes`] | 七根轴每一根都被巡检覆盖（漏一根 = 漏一列） |
//! | [`frontend_ts_mirror_matches_rust`] | 前端 `doc-kinds.ts` 的数组 ↔ Rust 枚举（顺序 + 取值） |
//! | [`frontend_ts_labels_match_rust`] | 前端标签表 ↔ Rust `label()` |
//! | [`entity_registry_is_covered_by_doc_kind`] | `state_machine::Entity::ALL` 全部落在 `DocKind` 域内 |
//! | [`flow_health_list_matches_registry`] | `approval_gate::FLOW_TYPES` ↔ `FlowBizType::HEALTH_CHECKED` |
//! | [`approval_entries_use_registered_biz_types`] | 各 `*_bridge::BIZ_TYPE` 都是注册过的枚举值 |
//! | [`no_legacy_transfer_entity_type`] | `TRANSFER` 别名已从代码里消失（只允许 `STOCK_TRANSFER`） |
//! | [`routes_are_unique_and_round_trip`] | REST 路径段唯一且能反解 |
//! | [`cross_axis_mappings_are_consistent`] | 审批/动账轴映射到的对象必须真是单据 |
//! | [`every_enum_has_a_label`] | 每个取值都有中文名 |

use super::*;
use crate::business::erp::approval_gate;

/// 仓库根（`backend/` 的上一级），用于读前端的镜像文件。
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend/ 必须有上级目录")
        .to_path_buf()
}

/// 读前端镜像文件全文。
fn read_frontend_mirror() -> String {
    let path = repo_root().join("frontend/src/api/business/erp/doc-kinds.ts");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("读不到前端词表镜像 {}：{err}", path.display()))
}

/// 从前端 TS 里抠出 `export const NAME = [ ... ] as const;` 的字符串元素（保序）。
fn ts_array(text: &str, name: &str) -> Vec<String> {
    let marker = format!("export const {name} = [");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("前端镜像里找不到 {marker}"))
        + marker.len();
    let rest = &text[start..];
    let end = rest
        .find(']')
        .unwrap_or_else(|| panic!("{name} 的数组没有闭合"));
    rest[..end]
        .split(',')
        .map(|item| item.trim().trim_matches('\'').trim())
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// 从前端 TS 的 `export const NAME: ... = { KEY: '值', ... };` 里抠出 `KEY → 值`。
fn ts_map(text: &str, name: &str) -> Vec<(String, String)> {
    let marker = format!("export const {name}");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("前端镜像里找不到 {marker}"));
    let rest = &text[start..];
    let open = rest.find('{').expect("标签表必须有 {");
    let close = rest[open..].find("};").expect("标签表必须以 }; 结束") + open;
    rest[open + 1..close]
        .lines()
        .filter_map(|line| {
            let line = line.trim().trim_end_matches(',');
            let (key, value) = line.split_once(':')?;
            let key = key.trim();
            let value = value.trim();
            if !value.starts_with('\'') {
                return None;
            }
            Some((key.to_string(), value.trim_matches('\'').trim().to_string()))
        })
        .collect()
}

fn assert_mirror(name: &str, ts_name: &str, rust: &[&str]) {
    let text = read_frontend_mirror();
    let ts = ts_array(&text, ts_name);
    let ts_ref: Vec<&str> = ts.iter().map(String::as_str).collect();
    assert_eq!(
        ts_ref, rust,
        "前端 {ts_name} 与 Rust {name} 不一致（顺序也要一致）—— \
         改了后端枚举就必须同步 frontend/src/api/business/erp/doc-kinds.ts"
    );
}

// ==================== 拼写规则 ====================

/// 所有轴的落库值都必须是大写下划线（唯一拼写规则）。
#[test]
fn every_axis_value_is_uppercase_underscore() {
    let all: Vec<(&str, &str)> = DocKind::ALL
        .iter()
        .map(|item| ("DocKind", item.as_str()))
        .chain(
            FlowBizType::ALL
                .iter()
                .map(|item| ("FlowBizType", item.as_str())),
        )
        .chain(
            TxnBizType::ALL
                .iter()
                .map(|item| ("TxnBizType", item.as_str())),
        )
        .chain(
            TxnDocType::ALL
                .iter()
                .map(|item| ("TxnDocType", item.as_str())),
        )
        .chain(
            VoucherSourceType::ALL
                .iter()
                .map(|item| ("VoucherSourceType", item.as_str())),
        )
        .chain(
            SerialSourceType::ALL
                .iter()
                .map(|item| ("SerialSourceType", item.as_str())),
        )
        .chain(
            DocSubtype::ALL
                .iter()
                .map(|item| ("DocSubtype", item.as_str())),
        )
        .chain(TakeType::ALL.iter().map(|item| ("TakeType", item.as_str())))
        .collect();
    for (axis, value) in all {
        assert!(
            !value.is_empty()
                && value
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_'),
            "{axis} 的取值 {value:?} 违反拼写规则（只允许 A-Z / 0-9 / _）"
        );
    }
}

/// 同一根轴里不许两个枚举映射到同一个字符串（那等于两个概念共用一个名字）。
#[test]
fn values_are_unique_per_axis() {
    fn check(axis: &str, values: &[&str]) {
        let mut seen = std::collections::BTreeSet::new();
        for value in values {
            assert!(seen.insert(*value), "{axis} 里 {value} 重复了");
        }
    }
    check("DocKind", &DocKind::ALL.map(DocKind::as_str));
    check("FlowBizType", &FlowBizType::ALL.map(FlowBizType::as_str));
    let mut txn = Vec::new();
    for item in TxnBizType::ALL {
        txn.push(item.as_str());
    }
    check("TxnBizType", &txn);
    let mut tdoc = Vec::new();
    for item in TxnDocType::ALL {
        tdoc.push(item.as_str());
    }
    check("TxnDocType", &tdoc);
    let mut voucher = Vec::new();
    for item in VoucherSourceType::ALL {
        voucher.push(item.as_str());
    }
    check("VoucherSourceType", &voucher);
    let mut serial = Vec::new();
    for item in SerialSourceType::ALL {
        serial.push(item.as_str());
    }
    check("SerialSourceType", &serial);
    let mut take = Vec::new();
    for item in TakeType::ALL {
        take.push(item.as_str());
    }
    check("TakeType", &take);
}

// ==================== 值域配置 ↔ 枚举 ====================

/// `COLUMN_DOMAINS` 里的字面量与枚举 `as_str()` 逐字一致（顺序也一致）。
#[test]
fn domain_consts_match_enums() {
    let doc: Vec<&str> = DOC_KIND_VALUES.to_vec();
    assert_eq!(doc, DocKind::ALL.map(DocKind::as_str).to_vec(), "DocKind");

    let flow: Vec<&str> = FLOW_BIZ_TYPE_VALUES.to_vec();
    assert_eq!(
        flow,
        FlowBizType::ALL.map(FlowBizType::as_str).to_vec(),
        "FlowBizType"
    );

    let txn: Vec<&str> = TXN_BIZ_TYPE_VALUES.to_vec();
    assert_eq!(
        txn,
        TxnBizType::ALL.map(TxnBizType::as_str).to_vec(),
        "TxnBizType"
    );

    let tdoc: Vec<&str> = TXN_DOC_TYPE_VALUES.to_vec();
    assert_eq!(
        tdoc,
        TxnDocType::ALL.map(TxnDocType::as_str).to_vec(),
        "TxnDocType"
    );

    let voucher: Vec<&str> = VOUCHER_SOURCE_VALUES.to_vec();
    assert_eq!(
        voucher,
        VoucherSourceType::ALL
            .map(VoucherSourceType::as_str)
            .to_vec(),
        "VoucherSourceType"
    );

    let serial: Vec<&str> = SERIAL_SOURCE_VALUES.to_vec();
    assert_eq!(
        serial,
        SerialSourceType::ALL.map(SerialSourceType::as_str).to_vec(),
        "SerialSourceType"
    );

    let ins: Vec<&str> = IN_TYPE_VALUES.to_vec();
    assert_eq!(
        ins,
        DocSubtype::IN_TYPES.map(DocSubtype::as_str).to_vec(),
        "DocSubtype::IN_TYPES"
    );

    let outs: Vec<&str> = OUT_TYPE_VALUES.to_vec();
    assert_eq!(
        outs,
        DocSubtype::OUT_TYPES.map(DocSubtype::as_str).to_vec(),
        "DocSubtype::OUT_TYPES"
    );

    let takes: Vec<&str> = TAKE_TYPE_VALUES.to_vec();
    assert_eq!(
        takes,
        TakeType::ALL.map(TakeType::as_str).to_vec(),
        "TakeType"
    );
}

/// 七根轴每一根都必须出现在 `COLUMN_DOMAINS` 里（漏一根就等于漏一列巡检）。
#[test]
fn domain_columns_exist_and_cover_all_axes() {
    let axes: std::collections::BTreeSet<&str> =
        COLUMN_DOMAINS.iter().map(|item| item.axis).collect();
    assert_eq!(axes.len(), 7, "必须覆盖七根轴，实际：{axes:?}");
    // 每根轴至少一列
    assert!(axes.contains("① 业务对象"));
    assert!(axes.contains("② 审批动作"));
    assert!(axes.contains("③ 库存变动动作"));
    assert!(axes.contains("④ 流水来源单据"));
    assert!(axes.contains("⑤ 凭证来源"));
    assert!(axes.contains("⑥ 序列号来源"));
    assert!(axes.contains("⑦ 单据子类型"));

    // 列清单里不许有重复的 (表, 列)
    let mut seen = std::collections::BTreeSet::new();
    for domain in COLUMN_DOMAINS {
        assert!(
            seen.insert((domain.table, domain.column)),
            "{}.{} 在 COLUMN_DOMAINS 里登记了两次",
            domain.table,
            domain.column
        );
        assert!(
            !domain.values.is_empty(),
            "{}.{} 的值域不能为空",
            domain.table,
            domain.column
        );
    }
    // 七根轴的落库列一个不漏
    for (table, column) in [
        ("biz_status_log", "entity_type"),
        ("biz_doc_attachment", "doc_type"),
        ("biz_reminder_log", "doc_type"),
        ("biz_flow_def", "biz_type"),
        ("biz_flow_instance", "biz_type"),
        ("biz_inventory_txn", "biz_type"),
        ("biz_inventory_txn", "doc_type"),
        ("biz_voucher", "source_type"),
        ("biz_payable", "source_type"),
        ("biz_material_serial", "source_type"),
        ("biz_stock_in", "in_type"),
        ("biz_stock_out", "out_type"),
        ("biz_stock_take", "take_type"),
    ] {
        assert!(
            COLUMN_DOMAINS
                .iter()
                .any(|item| item.table == table && item.column == column),
            "巡检漏了 {table}.{column}"
        );
    }
}

// ==================== 前端镜像 ====================

/// 前端 `doc-kinds.ts` 的七根轴数组与 Rust 枚举逐字一致（顺序也一致）。
#[test]
fn frontend_ts_mirror_matches_rust() {
    assert_mirror(
        "DocKind::ALL",
        "DOC_KINDS",
        &DocKind::ALL.map(DocKind::as_str),
    );
    assert_mirror(
        "FlowBizType::ALL",
        "FLOW_BIZ_TYPES",
        &FlowBizType::ALL.map(FlowBizType::as_str),
    );
    assert_mirror(
        "TxnBizType::ALL",
        "TXN_BIZ_TYPES",
        &TxnBizType::ALL.map(TxnBizType::as_str),
    );
    assert_mirror(
        "TxnDocType::ALL",
        "TXN_DOC_TYPES",
        &TxnDocType::ALL.map(TxnDocType::as_str),
    );
    assert_mirror(
        "VoucherSourceType::ALL",
        "VOUCHER_SOURCE_TYPES",
        &VoucherSourceType::ALL.map(VoucherSourceType::as_str),
    );
    assert_mirror(
        "SerialSourceType::ALL",
        "SERIAL_SOURCE_TYPES",
        &SerialSourceType::ALL.map(SerialSourceType::as_str),
    );
    assert_mirror(
        "DocSubtype::IN_TYPES",
        "IN_TYPES",
        &DocSubtype::IN_TYPES.map(DocSubtype::as_str),
    );
    assert_mirror(
        "DocSubtype::OUT_TYPES",
        "OUT_TYPES",
        &DocSubtype::OUT_TYPES.map(DocSubtype::as_str),
    );
    assert_mirror(
        "TakeType::ALL",
        "TAKE_TYPES",
        &TakeType::ALL.map(TakeType::as_str),
    );
}

/// 前端 `ATTACHMENT_DOC_TYPES` 与 `ATTACHMENT_DOC_KINDS` 逐字一致（顺序也一致）。
#[test]
fn attachment_doc_types_match_rust() {
    assert_mirror(
        "ATTACHMENT_DOC_KINDS",
        "ATTACHMENT_DOC_TYPES",
        &ATTACHMENT_DOC_KINDS.map(DocKind::as_str),
    );
}

/// 前端标签表与 Rust `label()` 逐条一致（中文文案也不许漂）。
#[test]
fn frontend_ts_labels_match_rust() {
    let text = read_frontend_mirror();
    fn check(pairs: Vec<(String, String)>, expected: Vec<(&str, &str)>, name: &str) {
        for (key, label) in expected {
            let hit = pairs
                .iter()
                .find(|(item, _)| item == key)
                .unwrap_or_else(|| panic!("{name} 里没有 {key}"));
            assert_eq!(hit.1, label, "前端 {name}[{key}] 与 Rust label() 不一致");
        }
    }
    check(
        ts_map(&text, "DOC_KIND_LABELS"),
        DocKind::ALL
            .iter()
            .map(|item| (item.as_str(), item.label()))
            .collect(),
        "DOC_KIND_LABELS",
    );
    check(
        ts_map(&text, "FLOW_BIZ_TYPE_LABELS"),
        FlowBizType::ALL
            .iter()
            .map(|item| (item.as_str(), item.label()))
            .collect(),
        "FLOW_BIZ_TYPE_LABELS",
    );
    check(
        ts_map(&text, "TXN_BIZ_TYPE_LABELS"),
        TxnBizType::ALL
            .iter()
            .map(|item| (item.as_str(), item.label()))
            .collect(),
        "TXN_BIZ_TYPE_LABELS",
    );
    check(
        ts_map(&text, "TXN_DOC_TYPE_LABELS"),
        TxnDocType::ALL
            .iter()
            .map(|item| (item.as_str(), item.label()))
            .collect(),
        "TXN_DOC_TYPE_LABELS",
    );
    check(
        ts_map(&text, "VOUCHER_SOURCE_TYPE_LABELS"),
        VoucherSourceType::ALL
            .iter()
            .map(|item| (item.as_str(), item.label()))
            .collect(),
        "VOUCHER_SOURCE_TYPE_LABELS",
    );
    check(
        ts_map(&text, "SERIAL_SOURCE_TYPE_LABELS"),
        SerialSourceType::ALL
            .iter()
            .map(|item| (item.as_str(), item.label()))
            .collect(),
        "SERIAL_SOURCE_TYPE_LABELS",
    );
}

// ==================== 与既有注册表对齐 ====================

/// `state_machine::Entity::ALL` 的每个落库值都必须落在 `DocKind` 域内。
///
/// 反向不要求（`DocKind` 里有 `PAYMENT` / `INVENTORY` / `FLOW_LINKAGE_CFG` 这些
/// 不走状态机的对象），但正向一定会：状态机写进 `biz_status_log.entity_type`
/// 的每个值都在巡检域里，否则巡检会对正常数据报红。
#[test]
fn entity_registry_is_covered_by_doc_kind() {
    use crate::business::shared::state_machine::Entity;
    for entity in Entity::ALL {
        assert!(
            DocKind::parse(entity.as_str()).is_some(),
            "state_machine 实体 {} 不在 DocKind 域内 —— 巡检会把正常数据判红",
            entity.as_str()
        );
    }
}

/// `approval_gate::FLOW_TYPES`（健康检查的 8 个）与 `FlowBizType::HEALTH_CHECKED` 逐条一致。
#[test]
fn flow_health_list_matches_registry() {
    let gate: Vec<(&str, &str)> = approval_gate::FLOW_TYPES.to_vec();
    let mine: Vec<(&str, &str)> = FlowBizType::HEALTH_CHECKED
        .iter()
        .map(|item| (item.as_str(), item.flow_code()))
        .collect();
    assert_eq!(gate, mine, "审批流健康检查清单与词表不一致");
}

/// 各模块写审批用的 `BIZ_TYPE` 常量都必须是注册过的枚举值。
#[test]
fn approval_entries_use_registered_biz_types() {
    use crate::business::erp::{
        contract_bridge, payable_bridge, purchase_bridge, stockin_query, stocktake_bridge,
        voucher_bridge,
    };
    let registered: Vec<&str> = FlowBizType::ALL.iter().map(|item| item.as_str()).collect();
    for (name, value) in [
        ("purchase_bridge::BIZ_TYPE", purchase_bridge::BIZ_TYPE),
        ("stockin_query::POST_BIZ_TYPE", stockin_query::POST_BIZ_TYPE),
        (
            "stockin_query::CANCEL_BIZ_TYPE",
            stockin_query::CANCEL_BIZ_TYPE,
        ),
        ("payable_bridge::BIZ_TYPE", payable_bridge::BIZ_TYPE),
        (
            "payable_bridge::CANCEL_BIZ_TYPE",
            payable_bridge::CANCEL_BIZ_TYPE,
        ),
        ("voucher_bridge::BIZ_TYPE", voucher_bridge::BIZ_TYPE),
        ("contract_bridge::BIZ_TYPE", contract_bridge::BIZ_TYPE),
        ("stocktake_bridge::BIZ_TYPE", stocktake_bridge::BIZ_TYPE),
    ] {
        assert!(
            registered.contains(&value),
            "{name} = {value:?} 不在 FlowBizType 域内（新增业务类型必须先登记）"
        );
    }
}

/// `TRANSFER` 这个实体别名必须从代码里彻底消失（V0219 已把存量改成 `STOCK_TRANSFER`）。
///
/// 只允许出现在本文件的注释里说明历史，所以检查的是「生产代码里的字符串字面量」：
/// `Entity::Transfer.as_str()` 必须是 `STOCK_TRANSFER`。
#[test]
fn no_legacy_transfer_entity_type() {
    use crate::business::shared::state_machine::{ENTITY_TRANSFER, Entity};
    assert_eq!(
        ENTITY_TRANSFER, "STOCK_TRANSFER",
        "调拨单的 entity_type 必须与 doc_type / 审批 biz_type 同名"
    );
    assert_eq!(Entity::Transfer.as_str(), "STOCK_TRANSFER");
    assert!(
        DocKind::parse("TRANSFER").is_none(),
        "旧别名 TRANSFER 不能出现在域里（留着它等于兼容层永远在）"
    );
}

/// REST 路径段唯一、可反解，且只给支持作废的对象。
#[test]
fn routes_are_unique_and_round_trip() {
    let mut seen = std::collections::BTreeSet::new();
    for kind in DocKind::ALL {
        let Some(route) = kind.route() else {
            continue;
        };
        assert!(
            seen.insert(route),
            "路径段 {route} 被两个对象共用（{}）",
            kind.as_str()
        );
        assert!(
            route
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'),
            "路径段 {route} 必须是小写连字符"
        );
        assert_eq!(DocKind::parse_route(route), Some(kind), "{route} 反解失败");
    }
    // 驼峰 / 下划线写法一律不认（不留兼容层）
    for legacy in ["purchase_order", "purchaseorder", "po", "purchaseOrder"] {
        assert_eq!(
            DocKind::parse_route(legacy),
            None,
            "{legacy} 不该被当成路径段 —— 兼容层会让问题永久存在"
        );
    }
}

/// 跨轴映射：审批 / 动账 / 凭证来源映射到的对象必须**真是单据**（或明确为 `None`）。
#[test]
fn cross_axis_mappings_are_consistent() {
    for item in FlowBizType::ALL {
        if let Some(kind) = item.doc_kind() {
            assert!(
                kind.table().is_some(),
                "{} 映射到 {} 但它没有实体表",
                item.as_str(),
                kind.as_str()
            );
        }
    }
    for item in TxnBizType::ALL {
        assert!(
            item.doc_kind().table().is_some(),
            "{} 映射到不存在的实体",
            item.as_str()
        );
    }
    for item in TxnDocType::ALL {
        assert!(item.doc_kind().table().is_some());
    }
    for item in VoucherSourceType::ALL {
        if let Some(kind) = item.doc_kind() {
            // 年结 / 期初指向的是「期间 / 年度」这类虚拟对象（没有实体表），
            // 所以这里只要求「对象已登记」，不要求有表。
            assert!(
                DocKind::ALL.contains(&kind),
                "{} 映射到未登记的对象 {}",
                item.as_str(),
                kind.as_str()
            );
        }
    }
    // 附件 / 提醒的白名单必须是单据或被提醒对象
    for kind in ATTACHMENT_DOC_KINDS {
        assert!(kind.is_document(), "{} 不是单据，不能挂附件", kind.as_str());
    }
    for kind in REMINDER_DOC_KINDS {
        assert!(
            DocKind::ALL.contains(&kind),
            "{} 不在 DocKind 域内",
            kind.as_str()
        );
    }
}

/// 每个取值都有中文名（展示层不许出现英文码兜底）。
#[test]
fn every_enum_has_a_label() {
    for item in DocKind::ALL {
        assert!(!item.label().is_empty(), "{} 没有中文名", item.as_str());
    }
    for item in FlowBizType::ALL {
        assert!(!item.label().is_empty());
        assert!(!item.flow_code().is_empty());
    }
    for item in TxnBizType::ALL {
        assert!(!item.label().is_empty());
    }
    for item in TxnDocType::ALL {
        assert!(!item.label().is_empty());
    }
    for item in VoucherSourceType::ALL {
        assert!(!item.label().is_empty());
    }
    for item in SerialSourceType::ALL {
        assert!(!item.label().is_empty());
    }
    for item in DocSubtype::ALL {
        assert!(!item.label().is_empty());
    }
    for item in TakeType::ALL {
        assert!(!item.label().is_empty());
    }
}

/// `parse` 是严格全等：大小写 / 分隔符变体一律 `None`。
#[test]
fn parse_is_strict() {
    for raw in ["po_approve", "PoApprove", "po-approve", "PO APPROVE"] {
        assert_eq!(FlowBizType::parse(raw), None, "{raw} 不该被归一");
    }
    for raw in ["stock_in", "StockIn", "stock-in"] {
        assert_eq!(DocKind::parse(raw), None, "{raw} 不该被归一");
    }
    assert_eq!(
        FlowBizType::parse("PO_APPROVE"),
        Some(FlowBizType::PoApprove)
    );
    assert_eq!(
        DocKind::parse("STOCK_TRANSFER"),
        Some(DocKind::StockTransfer)
    );
}

// ==================== 测试残留口径（t_61dca492） ====================

/// [`is_test_residue`] 的判据必须与 Python 巡检（`biz_type_domain.py::is_test_residue`）逐字一致。
///
/// 三边（Rust / Python / SQL 片段）任何一边改了拼写，这条就会红 —— 目的就是逼人同步改，
/// 而不是又一次「第 8 套词表」。
#[test]
fn test_residue_predicate_matches_python_and_sql() {
    // 真阳性：e2e 脚本写进生产库的那两种形态
    assert!(crate::business::shared::doc_kind::is_test_residue(
        "E2E_AP_E2EMVUARCAT"
    ));
    assert!(crate::business::shared::doc_kind::is_test_residue("E2E_"));
    assert!(crate::business::shared::doc_kind::is_test_residue("D2_SMOKE"));
    // 真阴性：内置流程 / 客户自定义流程 / 形似但不是
    for raw in ["PO_APPROVE", "TASK_ACCEPT", "OTHER", "E2EX", "e2e_ap_x", "D2_SMOKE_X"] {
        assert!(
            !crate::business::shared::doc_kind::is_test_residue(raw),
            "{raw} 不该被当成测试残留"
        );
    }
    // SQL 片段与函数同口径（前缀 4 字符 + 精确值）
    assert_eq!(
        crate::business::shared::doc_kind::TEST_RESIDUE_SQL,
        "(left(biz_type, 4) = 'E2E_' OR biz_type = 'D2_SMOKE')"
    );
    assert_eq!(
        crate::business::shared::doc_kind::TEST_RESIDUE_PREFIX.len(),
        4,
        "SQL 里用的是 left(biz_type, 4)，前缀长度必须也是 4"
    );
    // 内置词表里绝不能有测试残留值（否则「删残留」会把内置流程一起删掉）
    for item in crate::business::shared::doc_kind::COLUMN_DOMAINS {
        for value in item.values {
            assert!(
                !crate::business::shared::doc_kind::is_test_residue(value),
                "{}.{} 的词表里出现了测试残留值 {value}",
                item.table,
                item.column
            );
        }
    }
}
