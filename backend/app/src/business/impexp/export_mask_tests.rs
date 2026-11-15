//! 导出链路的脱敏端到端用例（批次 B · B6 的导出半条链路，P2-39 由 `masking/tests.rs`
//! 的 M04 搬入导出域）。
//!
//! # 为什么这条用例住在 `impexp/`
//!
//! 它验证的是**导出引擎** `format_export_rows` 会不会把明文漏进 xlsx，脱敏域只提供
//! 规则（经由 [`crate::business::masking::masker::Masker`] 接缝）。P2-39 之前它住在
//! `business/masking/tests.rs`，导致
//! 脱敏域反向依赖导出域；搬到导出域后依赖只剩 `impexp → masking` 一个方向。
//!
//! # 断言口径（与原 M04 一致）
//!
//! 1. 库里配了「某角色对 `sys_user.remark` 脱敏」→ 该角色的规则集送进导出引擎 →
//!    单元格里**不含原值**、非敏感列不受影响；
//! 2. 同一个导出引擎 + 空规则集 → 敏感列 **fail-closed**（422 + `61008`），
//!    且错误信息里也不许出现原值。
//!
//! # 隔离
//!
//! 沿用 [`crate::system::tests_support`] 的用例级身份：角色名 / 用户名 / 规则 reason
//! 都带 `tc_x_mask_<case>_<uniq>`，收尾 `fx.sweep()` 只按本用例身份删行。

use std::collections::HashMap;

use nik::prelude::*;

use crate::business::impexp::definition::{ColumnKind, ExportColumn};
use crate::business::impexp::engine::format_export_rows;
use crate::business::masking::dto::FieldMaskCreateReq;
use crate::business::masking::{handler as mask_handler, registry};
use crate::system::tests_support as ts;
use cloudpivot_kernel::shared::error_code;

/// 夹具前缀族（子表头；真正身份由 [`ts::Fixture`] 生成）。
const PREFIX: &str = "tc_x_mask";
/// 目标列：`sys_user.remark`（该列**没有**全局种子规则，见 `masking/tests.rs` 文件头）。
const TARGET_TABLE: &str = "sys_user";
const TARGET_COLUMN: &str = "remark";
/// 脱敏类型：手机号（1）。
const MOBILE: i16 = 1;

/// 导出列定义：一个普通列 + 一个敏感列（敏感列带 `table`，脱敏按 `(表, 列)` 匹配）。
static COLUMNS: &[ExportColumn] = &[
    ExportColumn {
        key: "username",
        header: "用户名",
        kind: ColumnKind::Text { max_len: None },
        width: None,
        table: TARGET_TABLE,
        sensitive: false,
    },
    ExportColumn {
        key: "remark",
        header: "备注",
        kind: ColumnKind::Text { max_len: None },
        width: None,
        table: TARGET_TABLE,
        sensitive: true,
    },
];

/// 造「角色 + 持角色用户」夹具，返回 `(角色ID, 持角色用户ID)`。
async fn fixtures(fx: &ts::Fixture) -> (uuid::Uuid, uuid::Uuid) {
    let role_id = uuid::Uuid::now_v7();
    let holder = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) VALUES (?::text::uuid, ?, ?, 1, 1)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(fx.name("ROLE")),
            Value::from(format!("{}-角色", fx.tag())),
        ],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, remark, status) \
         VALUES (?::text::uuid, ?, 'x', ?, '机密备注-13912345678', 1)",
        vec![
            Value::from(holder.to_string()),
            Value::from(fx.name("holder")),
            Value::from(format!("{}-holder", fx.tag())),
        ],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![Value::from(holder.to_string()), Value::from(role_id.to_string())],
    )
    .await;
    (role_id, holder)
}

/// 用真实 handler 建一条「该角色对 `sys_user.remark` 按手机号脱敏」的规则。
async fn create_rule(role_id: &str, reason: &str) {
    let request = FieldMaskCreateReq {
        table_name: TARGET_TABLE.to_string(),
        column_name: TARGET_COLUMN.to_string(),
        mask_type: MOBILE,
        mask_param: None,
        role_id: Some(role_id.to_string()),
        status: Some(1),
        reason: reason.to_string(),
    };
    mask_handler::create(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(ts::super_claims()),
        axum::Json(request),
    )
    .await
    .unwrap_or_else(|e| panic!("建脱敏规则失败：{}/{} {}", e.status, e.code, e.msg));
}

/// E2E：库里配的规则经 [`crate::business::masking::masker::Masker`] 接缝进导出引擎 → 单元格被打码；
/// 空规则集 → 422 61008。
///
/// 这条用例同时是「导出路径没有第二条脱敏实现」的回归：同步导出与异步 worker 都调
/// `format_export_rows`，只要这个函数 fail-closed，两条路都不可能出明文。
#[tokio::test]
async fn export_applies_mask_from_database_rule_and_fails_closed_without_it() {
    if !ts::ensure_db().await {
        return;
    }
    let fx = ts::Fixture::new(PREFIX, "M04");
    let (role_id, holder) = fixtures(&fx).await;
    create_rule(&role_id.to_string(), &fx.name("导出也要脱敏")).await;

    let set = registry::load_user_set(&holder).await.expect("加载规则失败");
    assert!(!set.is_empty(), "持角色用户必须命中 1 条规则");
    let raw = "13912345678";
    let mut row: HashMap<&'static str, String> = HashMap::new();
    row.insert("username", "alice".to_string());
    row.insert("remark", raw.to_string());

    // ① 有规则 → 敏感列被打码，非敏感列不动
    let formatted = format_export_rows(COLUMNS, &[row.clone()], &HashMap::new(), &set).expect("库里有规则，导出应成功");
    let cell = formatted[0].get("备注").cloned().unwrap_or_default();
    assert!(!cell.contains(raw), "导出的单元格里不能出现原值，实际 {cell:?}");
    assert!(cell.contains('*'), "敏感列应被打码，实际 {cell:?}");
    assert_eq!(
        formatted[0].get("用户名").map(String::as_str),
        Some("alice"),
        "非敏感列不受影响"
    );

    // ② 空规则集（未豁免）→ 敏感列必须 fail-closed，不是明文
    let empty = registry::MaskSet::empty();
    let error = format_export_rows(COLUMNS, &[row], &HashMap::new(), &empty).expect_err("敏感列没有规则必须报错");
    assert_eq!(error.error_code(), error_code::FIELD_MASK_RULE_MISSING);
    assert_eq!(error.http_status(), 422);
    assert!(
        !error.message().contains(raw),
        "错误信息里也不能泄漏原值，实际 {}",
        error.message()
    );
    assert!(
        error.message().contains("sys_user.remark"),
        "错误信息应指出列坐标，实际 {}",
        error.message()
    );

    println!("[X-MASK] 导出单元格 {cell:?} 已打码；无规则时 fail-closed（422 + 61008）✓");
    fx.sweep().await;
}
