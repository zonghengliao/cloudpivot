//! 词表自身的**结构性**不变量（P2-27 从应用 crate 的一致性用例里搬来）。
//!
//! 判据全部只依赖本 crate 内部：枚举、私有 `*_VALUES` 常量、`COLUMN_DOMAINS` 登记表。
//! 所以它们**跟着词表走** —— 词表在哪，「拼写规则 / 轴内唯一 / 值域 ↔ 枚举 / 路径段 /
//! 跨轴映射 / 中文名 / 严格解析 / 巡检列覆盖」这些不变量就在哪被验证，
//! `cargo test -p cloudpivot-doc` 也因此不会是一次 `0 passed` 的假绿。
//!
//! P2-29 把剩下的用例按「依赖谁」拆成两处：
//!
//! - **读文件的契约**（前端 TS 镜像、`scripts/demo_seed.sh`、快照巡检 SQL）→ 本 crate 的
//!   [`contract_tests`]：它们只碰 `std::fs`，不 import 应用代码，跟着词表走；
//! - **应用内断言**（`state_machine` 实体注册表、各 `*_bridge::BIZ_TYPE`）→ 留在应用 crate
//!   （`business/shared/doc_kind_entity_tests.rs`、`business/erp/flow_biz_type_contract_tests.rs`），
//!   因为它们必须 import 应用代码，塞进零依赖 crate 会把 `shared → erp` 这条回边重新拉回来。
use super::*;

// ==================== 拼写规则 ====================

/// 所有轴的落库值都必须是大写下划线（唯一拼写规则）。
#[test]
fn every_axis_value_is_uppercase_underscore() {
    let all: Vec<(&str, &str)> = DocKind::ALL
        .iter()
        .map(|item| ("DocKind", item.as_str()))
        .chain(FlowBizType::ALL.iter().map(|item| ("FlowBizType", item.as_str())))
        .chain(TxnBizType::ALL.iter().map(|item| ("TxnBizType", item.as_str())))
        .chain(TxnDocType::ALL.iter().map(|item| ("TxnDocType", item.as_str())))
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
        .chain(DocSubtype::ALL.iter().map(|item| ("DocSubtype", item.as_str())))
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
    assert_eq!(flow, FlowBizType::ALL.map(FlowBizType::as_str).to_vec(), "FlowBizType");

    let txn: Vec<&str> = TXN_BIZ_TYPE_VALUES.to_vec();
    assert_eq!(txn, TxnBizType::ALL.map(TxnBizType::as_str).to_vec(), "TxnBizType");

    let tdoc: Vec<&str> = TXN_DOC_TYPE_VALUES.to_vec();
    assert_eq!(tdoc, TxnDocType::ALL.map(TxnDocType::as_str).to_vec(), "TxnDocType");

    let voucher: Vec<&str> = VOUCHER_SOURCE_VALUES.to_vec();
    assert_eq!(
        voucher,
        VoucherSourceType::ALL.map(VoucherSourceType::as_str).to_vec(),
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
    assert_eq!(takes, TakeType::ALL.map(TakeType::as_str).to_vec(), "TakeType");
}

/// 十一根轴每一根都必须出现在 `COLUMN_DOMAINS` 里（漏一根就等于漏一列巡检）。
#[test]
fn domain_columns_exist_and_cover_all_axes() {
    let axes: std::collections::BTreeSet<&str> = COLUMN_DOMAINS.iter().map(|item| item.axis).collect();
    assert_eq!(
        axes.len(),
        11,
        "必须覆盖十一根轴（⑦ 之后加了 ⑧ 预算状态 / ⑨ 预算占用来源 / ⑩ 资产状态 / ⑪ 报销状态），实际：{axes:?}"
    );
    // 每根轴至少一列
    assert!(axes.contains("① 业务对象"));
    assert!(axes.contains("② 审批动作"));
    assert!(axes.contains("③ 库存变动动作"));
    assert!(axes.contains("④ 流水来源单据"));
    assert!(axes.contains("⑤ 凭证来源"));
    assert!(axes.contains("⑥ 序列号来源"));
    assert!(axes.contains("⑦ 单据子类型"));
    assert!(axes.contains("⑩ 资产状态"));

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
/// REST 路径段唯一、可反解，且只给支持作废的对象。
#[test]
fn routes_are_unique_and_round_trip() {
    let mut seen = std::collections::BTreeSet::new();
    for kind in DocKind::ALL {
        let Some(route) = kind.route() else {
            continue;
        };
        assert!(seen.insert(route), "路径段 {route} 被两个对象共用（{}）", kind.as_str());
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
    // 附件 / 提醒的白名单必须是「有实体表的对象」。
    //
    // ATT-11 之前这里断言 `is_document()`；供应商 / 客户是**主数据**（没有审批 /
    // 作废流转，`is_document() = false`）、期初余额挂的是期初凭证 —— 三者都有实体表
    // 可做存在性校验，所以判据改成「有目标表」。变成 `is_document()` 就会挡住
    // 「营业执照挂在供应商上」这个真实需求。
    for kind in ATTACHMENT_DOC_KINDS {
        assert!(kind.table().is_some(), "{} 没有实体表，不能挂附件", kind.as_str());
    }
    for kind in REMINDER_DOC_KINDS {
        assert!(DocKind::ALL.contains(&kind), "{} 不在 DocKind 域内", kind.as_str());
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
    assert_eq!(FlowBizType::parse("PO_APPROVE"), Some(FlowBizType::PoApprove));
    assert_eq!(DocKind::parse("STOCK_TRANSFER"), Some(DocKind::StockTransfer));
}
