//! ERP 单据导出定义（D7）：收货入库单 / 应付单 / 凭证。
//!
//! # 为什么要在这里补三个导出定义
//!
//! 「批量导出」要复用 impexp 的**异步任务**（worker 取数 → 写 xlsx → 落盘 → 导入导出页下载），
//! 而 worker 的取数入口是 `ExportDefinition::count` / `fetch`。这三个 ERP 单据此前**没有**
//! 导出定义（注册表里只有 `system.dict_item` / `system.user`），所以这里补上，
//! 并用一个 [`DocSpec`] 把「三张表结构相同的地方」收成一份实现：
//!
//! | 定义 | 表 | 单号列 | 备注 |
//! |------|----|--------|------|
//! | `erp.stock_in` | `biz_stock_in` | `in_no` | 联供应商 / 仓库取名字（导出给人看，不是给机器看） |
//! | `erp.payable` | `biz_payable` | `payable_no` | 联供应商；金额三件套（应付 / 已付 / 未付） |
//! | `erp.voucher` | `biz_voucher` | `voucher_no` | 期间 + 借贷合计 |
//!
//! # `ids` 语义（D7 批量导出）
//!
//! [`ExportFilter::ids`] 非空时取数收窄为 `id = ANY(?::text::uuid[])`：勾 5 张就只出 5 行。
//! 空则回落到 `keyword` / `status_code` 过滤（「选中全部筛选结果」）。**两个老定义不受影响**：
//! 它们不看 `ids`，行为与本卡之前完全一致。
//!
//! # 状态过滤的契约（卡 `t_93cba3f4`）
//!
//! ERP 单据的状态是**文本码**（`POSTED` / `SETTLED` / `CANCELLED` …，`varchar` + `CHECK`），
//! 表达不了「1 启用 / 0 停用」那套 `i16` 语义。因此：
//!
//! | 传参 | ERP 单据 | 字典项 / 用户 |
//! |------|----------|----------------|
//! | `status_code=POSTED` | ✅ 拼 `AND {status_expr} = ANY(?)` | ❌ 422（没有状态码白名单） |
//! | `status=1` | ❌ 422（提示改用 `status_code`） | ✅ 原行为不变 |
//! | 都不传 | 全量（与修复前一致） | 全量 |
//!
//! 合法码由 [`ExportDefinition::status_codes`] 返回 —— 本文件**现取** `columns` 里
//! `key == "status"` 的 `ColumnKind::Enum`，与导出回显的中文同一份 `EnumItem`，
//! 不另抄白名单。校验入口是 `definition::check_export_filter`（三个入口 + worker 都过一遍）。
//!
//! # 排序口径
//!
//! keyset 分页要求「有序 + 可比较」，这里统一按主键 `ASC`（UUID 稳定、无重复）。
//! 用户拿到的行序与列表页不同，但导出是「批量拿数据」，不是「再排一次版」——
//! 真要按日期排，Excel 里点一列排序即可。

use std::collections::HashMap;

use async_trait::async_trait;
use nik::prelude::*;

use crate::business::impexp::definition::{
    ColumnKind, EnumItem, ExportColumn, ExportDefinition, ExportFilter,
};

/// 收货单状态 → 中文（与 `stockin_model::status_label` 同口径）。
const STOCK_IN_STATUS: &[EnumItem] = &[
    EnumItem {
        label: "草稿",
        code: "DRAFT",
    },
    EnumItem {
        label: "已过账",
        code: "POSTED",
    },
    EnumItem {
        label: "已作废",
        code: "CANCELLED",
    },
    EnumItem {
        label: "过账审批中",
        code: "PENDING_POST",
    },
    EnumItem {
        label: "作废审批中",
        code: "PENDING_CANCEL",
    },
];

/// 应付单状态 → 中文。
///
/// ⚠ 这份列表同时是**导出状态过滤的白名单**（`status_codes()` 从这里取码），
/// 必须与 `ck_biz_payable_status` 逐字一致 —— V0191 起加了 `PENDING_CANCEL`
/// （作废审批中），漏了就会出现「导出里能看到这个状态、却筛不出来」。
/// 与 `payable_query::STATUSES` 同口径。
const PAYABLE_STATUS: &[EnumItem] = &[
    EnumItem {
        label: "未付",
        code: "OPEN",
    },
    EnumItem {
        label: "部分付款",
        code: "PARTIAL_PAID",
    },
    EnumItem {
        label: "已结清",
        code: "SETTLED",
    },
    EnumItem {
        label: "已作废",
        code: "CANCELLED",
    },
    EnumItem {
        label: "作废审批中",
        code: "PENDING_CANCEL",
    },
];

/// 凭证状态 → 中文。
const VOUCHER_STATUS: &[EnumItem] = &[
    EnumItem {
        label: "已记账",
        code: "POSTED",
    },
    EnumItem {
        label: "草稿",
        code: "DRAFT",
    },
    EnumItem {
        label: "已审核",
        code: "AUDITED",
    },
    EnumItem {
        label: "已作废",
        code: "CANCELLED",
    },
];

/// 一个单据导出的静态规格。
pub struct DocSpec {
    /// 业务编码（`erp.stock_in` …）。
    pub biz_type: &'static str,
    /// 业务名称（导入导出页下拉里显示的那一行）。
    pub biz_name: &'static str,
    /// `FROM … WHERE …`（基础过滤写在这里，后面只拼 `AND …`）。
    pub from: &'static str,
    /// SELECT 列表（**不含 `FROM`**；必须含 `id` 与每个列 key 的别名）。
    pub select: &'static str,
    /// 主键表达式（keyset 比较 / 排序用，如 `s.id`）。
    pub id_expr: &'static str,
    /// 关键词会 LIKE 的列表达式（单号 / 备注）。
    pub keyword_exprs: &'static [&'static str],
    /// 状态列表达式（`status_code` 过滤拼 `AND {status_expr} = ANY(?::text[])`，如 `v.status`）。
    ///
    /// 合法码不在这里另抄一份：由 `columns` 里 `key == "status"` 的
    /// `ColumnKind::Enum` 现取（见 `ErpDocExport::status_codes`），
    /// 保证「导出回显的中文」与「过滤接受的码」永远是同一份 `EnumItem`。
    pub status_expr: &'static str,
    /// 导出列定义（顺序即 Excel 列顺序）。
    pub columns: &'static [ExportColumn],
}

/// 收货入库单导出列。
static STOCK_IN_COLUMNS: &[ExportColumn] = &[
    ExportColumn {
        key: "in_no",
        header: "收货单号",
        kind: ColumnKind::Text { max_len: None },
        width: Some(22),
        table: "biz_stock_in",
        sensitive: false,
    },
    ExportColumn {
        key: "in_date",
        header: "入库日期",
        kind: ColumnKind::Date,
        width: Some(12),
        table: "biz_stock_in",
        sensitive: false,
    },
    ExportColumn {
        key: "supplier_name",
        header: "供应商",
        kind: ColumnKind::Text { max_len: None },
        width: Some(24),
        table: "biz_stock_in",
        sensitive: false,
    },
    ExportColumn {
        key: "warehouse_name",
        header: "仓库",
        kind: ColumnKind::Text { max_len: None },
        width: Some(16),
        table: "biz_stock_in",
        sensitive: false,
    },
    ExportColumn {
        key: "total_qty",
        header: "数量合计",
        kind: ColumnKind::Decimal {
            scale: 4,
            min: None,
            max: None,
        },
        width: Some(14),
        table: "biz_stock_in",
        sensitive: false,
    },
    ExportColumn {
        key: "total_amount",
        header: "不含税金额",
        kind: ColumnKind::Decimal {
            scale: 4,
            min: None,
            max: None,
        },
        width: Some(16),
        table: "biz_stock_in",
        sensitive: false,
    },
    ExportColumn {
        key: "total_tax",
        header: "税额",
        kind: ColumnKind::Decimal {
            scale: 4,
            min: None,
            max: None,
        },
        width: Some(14),
        table: "biz_stock_in",
        sensitive: false,
    },
    ExportColumn {
        key: "total_with_tax",
        header: "价税合计",
        kind: ColumnKind::Decimal {
            scale: 4,
            min: None,
            max: None,
        },
        width: Some(16),
        table: "biz_stock_in",
        sensitive: false,
    },
    ExportColumn {
        key: "status",
        header: "状态",
        kind: ColumnKind::Enum(STOCK_IN_STATUS),
        width: Some(12),
        table: "biz_stock_in",
        sensitive: false,
    },
    ExportColumn {
        key: "remark",
        header: "备注",
        kind: ColumnKind::Text { max_len: None },
        width: Some(30),
        table: "biz_stock_in",
        sensitive: false,
    },
];

/// 应付单导出列。
static PAYABLE_COLUMNS: &[ExportColumn] = &[
    ExportColumn {
        key: "payable_no",
        header: "应付单号",
        kind: ColumnKind::Text { max_len: None },
        width: Some(22),
        table: "biz_payable",
        sensitive: false,
    },
    ExportColumn {
        key: "source_no",
        header: "来源单号",
        kind: ColumnKind::Text { max_len: None },
        width: Some(22),
        table: "biz_payable",
        sensitive: false,
    },
    ExportColumn {
        key: "supplier_name",
        header: "供应商",
        kind: ColumnKind::Text { max_len: None },
        width: Some(24),
        table: "biz_payable",
        sensitive: false,
    },
    ExportColumn {
        key: "invoice_no",
        header: "发票号",
        kind: ColumnKind::Text { max_len: None },
        width: Some(20),
        table: "biz_payable",
        sensitive: false,
    },
    ExportColumn {
        key: "amount",
        header: "应付金额",
        kind: ColumnKind::Decimal {
            scale: 4,
            min: None,
            max: None,
        },
        width: Some(16),
        table: "biz_payable",
        sensitive: false,
    },
    ExportColumn {
        key: "paid_amount",
        header: "已付金额",
        kind: ColumnKind::Decimal {
            scale: 4,
            min: None,
            max: None,
        },
        width: Some(16),
        table: "biz_payable",
        sensitive: false,
    },
    ExportColumn {
        key: "unpaid_amount",
        header: "未付金额",
        kind: ColumnKind::Decimal {
            scale: 4,
            min: None,
            max: None,
        },
        width: Some(16),
        table: "biz_payable",
        sensitive: false,
    },
    ExportColumn {
        key: "due_date",
        header: "到期日",
        kind: ColumnKind::Date,
        width: Some(12),
        table: "biz_payable",
        sensitive: false,
    },
    ExportColumn {
        key: "period_code",
        header: "会计期间",
        kind: ColumnKind::Text { max_len: None },
        width: Some(10),
        table: "biz_payable",
        sensitive: false,
    },
    ExportColumn {
        key: "status",
        header: "状态",
        kind: ColumnKind::Enum(PAYABLE_STATUS),
        width: Some(12),
        table: "biz_payable",
        sensitive: false,
    },
];

/// 凭证导出列。
static VOUCHER_COLUMNS: &[ExportColumn] = &[
    ExportColumn {
        key: "voucher_no",
        header: "凭证号",
        kind: ColumnKind::Text { max_len: None },
        width: Some(18),
        table: "biz_voucher",
        sensitive: false,
    },
    ExportColumn {
        key: "voucher_date",
        header: "凭证日期",
        kind: ColumnKind::Date,
        width: Some(12),
        table: "biz_voucher",
        sensitive: false,
    },
    ExportColumn {
        key: "period",
        header: "会计期间",
        kind: ColumnKind::Text { max_len: None },
        width: Some(10),
        table: "biz_voucher",
        sensitive: false,
    },
    ExportColumn {
        key: "summary",
        header: "摘要",
        kind: ColumnKind::Text { max_len: None },
        width: Some(34),
        table: "biz_voucher",
        sensitive: false,
    },
    ExportColumn {
        key: "total_debit",
        header: "借方合计",
        kind: ColumnKind::Decimal {
            scale: 4,
            min: None,
            max: None,
        },
        width: Some(16),
        table: "biz_voucher",
        sensitive: false,
    },
    ExportColumn {
        key: "total_credit",
        header: "贷方合计",
        kind: ColumnKind::Decimal {
            scale: 4,
            min: None,
            max: None,
        },
        width: Some(16),
        table: "biz_voucher",
        sensitive: false,
    },
    ExportColumn {
        key: "source_no",
        header: "来源单号",
        kind: ColumnKind::Text { max_len: None },
        width: Some(22),
        table: "biz_voucher",
        sensitive: false,
    },
    ExportColumn {
        key: "status",
        header: "状态",
        kind: ColumnKind::Enum(VOUCHER_STATUS),
        width: Some(12),
        table: "biz_voucher",
        sensitive: false,
    },
];

/// 收货入库单：`erp.stock_in`。
static STOCK_IN: DocSpec = DocSpec {
    biz_type: "erp.stock_in",
    biz_name: "收货入库单",
    from: "FROM biz_stock_in s \
           LEFT JOIN biz_supplier sp ON sp.id = s.supplier_id AND sp.deleted_at IS NULL \
           LEFT JOIN biz_warehouse w ON w.id = s.warehouse_id AND w.deleted_at IS NULL \
           WHERE s.deleted_at IS NULL",
    select: "SELECT s.id::text AS id, s.in_no::text AS in_no, s.in_date::text AS in_date, \
             COALESCE(sp.supplier_name::text, '') AS supplier_name, \
             COALESCE(w.warehouse_name::text, '') AS warehouse_name, \
             s.total_qty::text AS total_qty, s.total_amount::text AS total_amount, \
             s.total_tax::text AS total_tax, s.total_with_tax::text AS total_with_tax, \
             s.status::text AS status, COALESCE(s.remark::text, '') AS remark",
    id_expr: "s.id",
    keyword_exprs: &["s.in_no", "s.remark"],
    status_expr: "s.status",
    columns: STOCK_IN_COLUMNS,
};

/// 应付单：`erp.payable`。
static PAYABLE: DocSpec = DocSpec {
    biz_type: "erp.payable",
    biz_name: "应付单",
    from: "FROM biz_payable p \
           LEFT JOIN biz_supplier sp ON sp.id = p.supplier_id AND sp.deleted_at IS NULL \
           WHERE p.deleted_at IS NULL",
    select: "SELECT p.id::text AS id, p.payable_no::text AS payable_no, \
             p.source_no::text AS source_no, \
             COALESCE(sp.supplier_name::text, '') AS supplier_name, \
             COALESCE(p.invoice_no::text, '') AS invoice_no, \
             p.amount::text AS amount, p.paid_amount::text AS paid_amount, \
             p.unpaid_amount::text AS unpaid_amount, p.due_date::text AS due_date, \
             p.period_code::text AS period_code, p.status::text AS status",
    id_expr: "p.id",
    keyword_exprs: &["p.payable_no", "p.source_no", "p.invoice_no"],
    status_expr: "p.status",
    columns: PAYABLE_COLUMNS,
};

/// 凭证：`erp.voucher`。
static VOUCHER: DocSpec = DocSpec {
    biz_type: "erp.voucher",
    biz_name: "凭证",
    from: "FROM biz_voucher v WHERE v.deleted_at IS NULL",
    select: "SELECT v.id::text AS id, \
             (v.voucher_word || '-' || v.voucher_no)::text AS voucher_no, \
             v.voucher_date::text AS voucher_date, v.period::text AS period, \
             COALESCE(v.summary::text, '') AS summary, \
             v.total_debit::text AS total_debit, v.total_credit::text AS total_credit, \
             COALESCE(v.source_no::text, '') AS source_no, v.status::text AS status",
    id_expr: "v.id",
    keyword_exprs: &["v.voucher_no", "v.summary", "v.source_no"],
    status_expr: "v.status",
    columns: VOUCHER_COLUMNS,
};

/// 导出定义（一个规格一个实例）。
pub struct ErpDocExport {
    /// 静态规格。
    pub spec: &'static DocSpec,
}

#[async_trait]
impl ExportDefinition for ErpDocExport {
    fn biz_type(&self) -> &'static str {
        self.spec.biz_type
    }

    fn biz_name(&self) -> &'static str {
        self.spec.biz_name
    }

    fn columns(&self) -> &'static [ExportColumn] {
        self.spec.columns
    }

    /// 单据状态码白名单：**现取**导出列定义里 `status` 列的 `EnumItem`。
    ///
    /// 不另抄一份常量数组的原因：ERP 单据的状态是 `varchar` + `CHECK`，
    /// 中文回显与合法码必须同源（列定义里加一个状态而忘了白名单，就会出现
    /// 「导出里能看到 `PENDING_CANCEL`，却筛不出来」的静默缺陷）。
    fn status_codes(&self) -> Vec<&'static str> {
        self.spec
            .columns
            .iter()
            .find(|column| column.key == "status")
            .and_then(|column| match column.kind {
                ColumnKind::Enum(items) => Some(items.iter().map(|item| item.code).collect()),
                _ => None,
            })
            .unwrap_or_default()
    }

    async fn count(&self, filter: &ExportFilter) -> std::result::Result<i64, DbError> {
        let mut sql = format!("SELECT COUNT(*) {}", self.spec.from);
        let mut params: Vec<Value> = Vec::new();
        push_filters(&mut sql, &mut params, self.spec, filter);
        db().scalar_with_params(&sql, params).await
    }

    async fn fetch(
        &self,
        filter: &ExportFilter,
        after_id: Option<&str>,
        limit: u32,
    ) -> std::result::Result<Vec<(String, HashMap<&'static str, String>)>, DbError> {
        // `spec.select` 自带 `SELECT …` 前缀（与 `count` 的 `SELECT COUNT(*)` 对称），
        // 这里只拼 `FROM …`，别再补一个 SELECT（踩过：`SELECT SELECT …` 会以
        // 「分块取数失败：query failed」的形式把导出任务打回重试）
        let mut sql = format!("{} {}", self.spec.select, self.spec.from);
        let mut params: Vec<Value> = Vec::new();
        push_filters(&mut sql, &mut params, self.spec, filter);
        if let Some(after) = after_id {
            // `?::uuid` 必须绑 uuid：传字符串会在驱动侧 "error serializing parameter"（A4 实测）。
            let after = uuid::Uuid::parse_str(after).map_err(|error| {
                DbError::InvalidValue(format!("after_id 不是合法 UUID（{after}）：{error}"))
            })?;
            sql.push_str(&format!(" AND {} > ?::uuid", self.spec.id_expr));
            params.push(Value::from(after));
        }
        sql.push_str(&format!(" ORDER BY {} ASC LIMIT ?", self.spec.id_expr));
        params.push(Value::from(i64::from(limit)));

        let rows = db().find_with_params(&sql, params).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get_typed("id")?;
            let mut values: HashMap<&'static str, String> = HashMap::new();
            for column in self.spec.columns {
                let value: String = row.get_typed(column.key).unwrap_or_default();
                values.insert(column.key, value);
            }
            out.push((id, values));
        }
        Ok(out)
    }
}

/// 拼「关键词 / 状态码 / 主键」三类过滤条件（count 与 fetch 共用，口径不许分叉）。
///
/// 状态码分支是本卡（`t_93cba3f4`）补的：此前只有 keyword 与 ids，
/// 于是 ERP 单据导出收到 `status`（`i16`，为启用 / 停用设计）时**静默返回全量**。
/// 现在只认文本码 `status_code`（白名单校验在 `definition::check_export_filter`），拼
/// `AND {status_expr} = ANY(?::text[])`，与列表页 `status` 过滤同口径。
fn push_filters(sql: &mut String, params: &mut Vec<Value>, spec: &DocSpec, filter: &ExportFilter) {
    if let Some(pattern) = crate::utils::like_pattern(filter.keyword.as_deref())
        && !spec.keyword_exprs.is_empty()
    {
        sql.push_str(" AND (");
        for (index, expr) in spec.keyword_exprs.iter().enumerate() {
            if index > 0 {
                sql.push_str(" OR ");
            }
            sql.push_str(expr);
            sql.push_str("::text LIKE ?");
            params.push(Value::from(pattern.clone()));
        }
        sql.push(')');
    }
    if let Some(raw) = filter.status_code.as_deref() {
        // 与 `check_export_filter` 同一套规范化（去空白 + 大写）：即使调用方跳过了校验，
        // 也不会因为大小写差异把「POSTED」筛成 0 行（那又是一次静默失效）。
        let code = raw.trim().to_ascii_uppercase();
        if !code.is_empty() {
            // `?::text::text[]`：本栈驱动不支持 `Value::Array`，数组一律走
            // `{a,b}` 文本字面量 + 两级 cast（与 `scope.rs` / `assignee.rs` 的 uuid 数组同款）。
            sql.push_str(&format!(" AND {} = ANY(?::text::text[])", spec.status_expr));
            params.push(text_array(&[code.as_str()]));
        }
    }
    if !filter.ids.is_empty() {
        sql.push_str(&format!(" AND {} = ANY(?::text::uuid[])", spec.id_expr));
        params.push(uuid_array(&filter.ids));
    }
}

/// 文本数组的文本字面量（`{POSTED,CANCELLED}`），配 `?::text[]`。
///
/// 用数组而不是 `= ?`：前端的状态筛选天然是多选，将来放开多码时 SQL 不用改。
fn text_array(values: &[&str]) -> Value {
    let mut literal = String::with_capacity(values.len() * 12 + 2);
    literal.push('{');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            literal.push(',');
        }
        literal.push_str(value);
    }
    literal.push('}');
    Value::Text(literal)
}

/// UUID 数组的文本字面量（`{a,b}`），配 `?::text::uuid[]`。
fn uuid_array(ids: &[uuid::Uuid]) -> Value {
    let mut literal = String::with_capacity(ids.len() * 37 + 2);
    literal.push('{');
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            literal.push(',');
        }
        literal.push_str(&id.to_string());
    }
    literal.push('}');
    Value::Text(literal)
}

/// 三个内置 ERP 单据导出定义（`builtin::register` 里逐条注册）。
pub fn definitions() -> [ErpDocExport; 3] {
    [
        ErpDocExport { spec: &STOCK_IN },
        ErpDocExport { spec: &PAYABLE },
        ErpDocExport { spec: &VOUCHER },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::impexp::definition::check_export_filter;

    #[test]
    fn biz_types_are_unique_and_prefixed() {
        let mut seen = std::collections::HashSet::new();
        for definition in definitions() {
            let biz_type = definition.biz_type();
            assert!(
                biz_type.starts_with("erp."),
                "业务编码要带 erp. 前缀：{biz_type}"
            );
            assert!(seen.insert(biz_type), "业务编码重复：{biz_type}");
            assert!(!definition.biz_name().is_empty());
            assert!(!definition.columns().is_empty());
        }
    }

    #[test]
    fn every_column_has_select_alias() {
        // 列 key 必须出现在 select 里（否则 `get_typed` 取不到值时是空字符串，
        // 导出的表「整列空白」而没有任何报错 —— 这类事故没人会在测试里发现）
        for definition in definitions() {
            for column in definition.columns() {
                let alias = format!(" AS {}", column.key);
                assert!(
                    definition.spec.select.contains(&alias),
                    "{} 的列 {} 没有在 select 里取别名",
                    definition.biz_type(),
                    column.key
                );
            }
        }
    }

    #[test]
    fn filters_bind_keyword_and_ids() {
        let mut sql = String::from("WHERE 1 = 1");
        let mut params: Vec<Value> = Vec::new();
        push_filters(
            &mut sql,
            &mut params,
            &STOCK_IN,
            &ExportFilter {
                keyword: Some("IN-2026".to_string()),
                status: None,
                status_code: None,
                ids: vec![
                    uuid::Uuid::parse_str("018f4b2c-1111-7222-8333-444455550001").expect("uuid"),
                ],
            },
        );
        assert!(sql.contains("s.in_no::text LIKE ?"));
        assert!(sql.contains("s.id = ANY(?::text::uuid[])"));
        assert_eq!(params.len(), 3, "2 个关键词列 + 1 个 ids 数组");
    }

    #[test]
    fn id_filter_matches_keyset_column() {
        // 主键过滤必须与 keyset 排序用同一表达式，否则「第 2 页」会漏行
        assert!(STOCK_IN.id_expr.contains('.'));
        assert_eq!(STOCK_IN.keyword_exprs.len(), 2);
        assert_eq!(PAYABLE.id_expr, "p.id");
        assert_eq!(VOUCHER.id_expr, "v.id");
    }

    // ---------- 状态过滤契约（卡 t_93cba3f4） ----------

    #[test]
    fn push_filters_binds_status_code_as_text_array() {
        let mut sql = String::from("WHERE 1 = 1");
        let mut params: Vec<Value> = Vec::new();
        push_filters(
            &mut sql,
            &mut params,
            &VOUCHER,
            &ExportFilter {
                status_code: Some("posted".to_string()),
                ..Default::default()
            },
        );
        assert!(
            sql.contains("v.status = ANY(?::text::text[])"),
            "状态码必须真的进 SQL（否则就是本卡修的静默失效）：{sql}"
        );
        assert_eq!(params.len(), 1, "一个状态码 = 一个参数");
        let bound = format!("{:?}", params[0]);
        assert!(
            bound.contains("{POSTED}"),
            "绑定值要归一化成大写（POSTED）：{bound}"
        );
    }

    #[test]
    fn push_filters_omits_status_when_absent() {
        // 不传状态 → SQL 里一个状态子句都不许出现（行为与修复前一致：全量）
        let mut sql = String::from("WHERE 1 = 1");
        let mut params: Vec<Value> = Vec::new();
        push_filters(&mut sql, &mut params, &STOCK_IN, &ExportFilter::default());
        assert_eq!(sql, "WHERE 1 = 1");
        assert!(params.is_empty());
    }

    #[test]
    fn push_filters_never_consumes_legacy_status_flag() {
        // `status`（i16）在 ERP 单据上是「用错了字段」，由 check_export_filter 报 422；
        // SQL 侧不许偷偷把它当状态用（那会变成另一个口径）
        let mut sql = String::from("WHERE 1 = 1");
        let mut params: Vec<Value> = Vec::new();
        push_filters(
            &mut sql,
            &mut params,
            &PAYABLE,
            &ExportFilter {
                status: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(sql, "WHERE 1 = 1");
        assert!(params.is_empty());
    }

    #[test]
    fn status_codes_are_the_column_enum() {
        // 白名单不许另抄一份：必须逐字等于导出列里 status 列的 EnumItem 码表
        for definition in definitions() {
            let codes: Vec<&str> = definition
                .spec
                .columns
                .iter()
                .find(|column| column.key == "status")
                .and_then(|column| match column.kind {
                    ColumnKind::Enum(items) => Some(items.iter().map(|item| item.code).collect()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{} 没有 Enum 状态列", definition.biz_type()));
            assert!(
                !codes.is_empty(),
                "{} 的状态列不许为空",
                definition.biz_type()
            );
            assert_eq!(
                definition.status_codes(),
                codes,
                "{} 的状态码白名单必须与导出列同一份",
                definition.biz_type()
            );
            // status_expr 必须指向同一张表的别名（复制粘贴写错别名 = 过滤到别的表）
            let alias = definition
                .spec
                .id_expr
                .split('.')
                .next()
                .unwrap_or_default();
            assert_eq!(definition.spec.status_expr, format!("{alias}.status"));
        }
        // 与 DB CHECK 同步：V0191 起应付单有 PENDING_CANCEL（漏了就会出现「能看到、筛不出」）
        assert!(
            ErpDocExport { spec: &PAYABLE }
                .status_codes()
                .contains(&"PENDING_CANCEL")
        );
        assert_eq!(
            ErpDocExport { spec: &VOUCHER }.status_codes(),
            vec!["POSTED", "DRAFT", "AUDITED", "CANCELLED"]
        );
    }

    #[test]
    fn check_export_filter_normalizes_accepts_and_rejects() {
        let voucher = ErpDocExport { spec: &VOUCHER };

        // ① 合法码：大小写 / 空白都归一化成大写，SQL 绑定同一份值
        let mut filter = ExportFilter {
            status_code: Some("  posted ".to_string()),
            ..Default::default()
        };
        check_export_filter(&voucher, &mut filter).expect("POSTED 是合法码");
        assert_eq!(filter.status_code.as_deref(), Some("POSTED"));

        // ② 空串 = 不过滤（与 keyword 同口径），不是错误
        let mut filter = ExportFilter {
            status_code: Some("   ".to_string()),
            ..Default::default()
        };
        check_export_filter(&voucher, &mut filter).expect("空串按不过滤处理");
        assert!(filter.status_code.is_none());

        // ③ 非法码 → 422，消息点名合法码
        let mut filter = ExportFilter {
            status_code: Some("PAID".to_string()),
            ..Default::default()
        };
        let error = check_export_filter(&voucher, &mut filter).expect_err("PAID 不是凭证状态");
        assert_eq!(error.status, 422);
        assert_eq!(error.code, crate::shared::error_code::PARAM_INVALID);
        for code in voucher.status_codes() {
            assert!(
                error.msg.contains(code),
                "报错要把合法码列出来（缺 {code}）：{}",
                error.msg
            );
        }

        // ④ 本卡核心：ERP 单据收到 i16 的 status → 422，绝不静默返回全量
        let mut filter = ExportFilter {
            status: Some(1),
            ..Default::default()
        };
        let error = check_export_filter(&ErpDocExport { spec: &PAYABLE }, &mut filter)
            .expect_err("status 必须被拒");
        assert_eq!(error.status, 422);
        assert!(
            error.msg.contains("status_code") && error.msg.contains("OPEN"),
            "要提示改用 status_code 并给出合法码：{}",
            error.msg
        );

        // ⑤ 什么都不传 → 通过（全量，与修复前一致）
        let mut filter = ExportFilter::default();
        check_export_filter(&voucher, &mut filter).expect("不传状态不是错误");
    }

    // ---------- 真库用例（ERP_DB_URL）：SELECT 拼错会当场红 ----------

    /// 真库用例必须串行（共用全局 `db()`）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 初始化全局数据库；`false` = 没配 `ERP_DB_URL`。
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
                    Err(error) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{error}）");
                        false
                    }
                }
            })
            .await
    }

    /// 批量导出的取数必须真的能跑：`count` 与 `fetch` 走同一份 `from`/`select`，
    /// 拼错一个 `SELECT` 就会以「分块取数失败」的形式把任务打回重试 —— 真机踩过一次，
    /// 这里用真库把「查得出行、列有值」钉住。
    #[tokio::test]
    async fn stock_in_export_fetches_real_rows() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let unit = "b2b7e001-0000-4000-8000-000000000001";
        let supplier = "b2b7e001-0000-4000-8000-000000000002";
        let warehouse = "b2b7e001-0000-4000-8000-000000000003";
        let doc = "b2b7e001-0000-4000-8000-000000000004";
        let fixtures = [
            format!(
                "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status, sort_order) \
                 VALUES ('{unit}', 'D7EXP-PCS', '个', 1, 1, 1) ON CONFLICT DO NOTHING"
            ),
            format!(
                "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) \
                 VALUES ('{supplier}', 'D7EXP-SUP', 'D7 导出供应商', 1) ON CONFLICT DO NOTHING"
            ),
            format!(
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status) \
                 VALUES ('{warehouse}', 'D7EXP-WH', 'D7 导出仓', '/{warehouse}/', 1, 1, 1) ON CONFLICT DO NOTHING"
            ),
            format!(
                "INSERT INTO biz_stock_in (id, in_no, in_type, supplier_id, warehouse_id, in_date, status, \
                 total_qty, total_amount, total_tax, total_with_tax, remark, is_sourceless) \
                 VALUES ('{doc}', 'IN-D7EXP-000001', 'PURCHASE', '{supplier}', '{warehouse}', '2026-03-01', \
                 'DRAFT', 2.0000, 20.0000, 2.6000, 22.6000, 'D7 导出测试', true) ON CONFLICT DO NOTHING"
            ),
        ];
        for sql in fixtures {
            db().exec_sql(sql, vec![]).await.expect("夹具");
        }

        let definition = ErpDocExport { spec: &STOCK_IN };
        let filter = ExportFilter {
            ids: vec![uuid::Uuid::parse_str(doc).expect("uuid")],
            ..Default::default()
        };
        let total = definition.count(&filter).await.expect("count 应能跑");
        assert_eq!(total, 1, "勾了 1 行就该数到 1 行");
        let page = definition
            .fetch(&filter, None, 100)
            .await
            .expect("fetch 应能跑");
        assert_eq!(
            page.len(),
            1,
            "取数必须真的返回行（SELECT 拼错时这里就是 0/报错）"
        );
        let (id, values) = &page[0];
        assert_eq!(id, doc);
        assert_eq!(
            values.get("in_no").map(String::as_str),
            Some("IN-D7EXP-000001")
        );
        assert_eq!(
            values.get("supplier_name").map(String::as_str),
            Some("D7 导出供应商")
        );
        assert_eq!(
            values.get("warehouse_name").map(String::as_str),
            Some("D7 导出仓")
        );
        assert_eq!(
            values.get("status").map(String::as_str),
            Some("DRAFT"),
            "枚举列取的是码值"
        );
        assert_eq!(
            values.get("total_with_tax").map(String::as_str),
            Some("22.6000")
        );

        // 清理（真库用例不留痕）
        let _ = db()
            .exec_sql(
                format!("DELETE FROM biz_stock_in WHERE id = '{doc}'::uuid"),
                vec![],
            )
            .await;
        for (table, id) in [
            ("biz_warehouse", warehouse),
            ("biz_supplier", supplier),
            ("biz_unit", unit),
        ] {
            let _ = db()
                .exec_sql(
                    format!("DELETE FROM {table} WHERE id = '{id}'::uuid"),
                    vec![],
                )
                .await;
        }
    }

    /// 本卡验收 ①②③ 的真库版：`status_code=POSTED` 只回已记账那张，
    /// 不传状态仍是全量（回归），非法码在进 SQL 之前就被 `check_export_filter` 挡掉。
    ///
    /// 缺 `ERP_DB_URL` 时打印 SKIP 并返回（**不假绿**：输出里能看到 SKIP 行）。
    #[tokio::test]
    async fn voucher_status_code_filters_real_rows() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let posted = "b3c8e002-0000-4000-8000-000000000001";
        let cancelled = "b3c8e002-0000-4000-8000-000000000002";
        let fixtures = [
            format!(
                "INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, \
                 summary, total_debit, total_credit, entry_count, status) \
                 VALUES ('{posted}', '记', 'D7EXPSTATUS0001', '2026-03', '2026-03-01', \
                 'D7 状态过滤-已记账', 100.0000, 100.0000, 2, 'POSTED') ON CONFLICT DO NOTHING"
            ),
            format!(
                "INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, \
                 summary, total_debit, total_credit, entry_count, status) \
                 VALUES ('{cancelled}', '记', 'D7EXPSTATUS0002', '2026-03', '2026-03-02', \
                 'D7 状态过滤-已作废', 50.0000, 50.0000, 2, 'CANCELLED') ON CONFLICT DO NOTHING"
            ),
        ];
        for sql in fixtures {
            db().exec_sql(sql, vec![]).await.expect("凭证夹具");
        }

        let definition = ErpDocExport { spec: &VOUCHER };
        // 关键词把范围收窄到本用例的 2 张（真库里还有别的凭证）
        let base = || ExportFilter {
            keyword: Some("D7EXPSTATUS".to_string()),
            ..Default::default()
        };

        // ③ 不传状态 = 全量：修复前后行为一致
        assert_eq!(
            definition.count(&base()).await.expect("count 全量"),
            2,
            "不传状态时必须还是全量（回归口径）"
        );

        // ① status_code=POSTED 只返回 POSTED 那行
        let mut filter = base();
        filter.status_code = Some("posted".to_string());
        check_export_filter(&definition, &mut filter).expect("小写也要接受");
        assert_eq!(
            definition.count(&filter).await.expect("count POSTED"),
            1,
            "status_code=POSTED 只能命中已记账那张（本卡修的就是这里放过全量）"
        );
        let page = definition
            .fetch(&filter, None, 100)
            .await
            .expect("fetch POSTED");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].0, posted, "命中的必须是 POSTED 那张");
        assert_eq!(page[0].1.get("status").map(String::as_str), Some("POSTED"));

        // CANCELLED 只回另一张（证明是「按传入码过滤」而不是「过滤成固定那张」）
        let mut filter = base();
        filter.status_code = Some("CANCELLED".to_string());
        check_export_filter(&definition, &mut filter).expect("CANCELLED 合法");
        let page = definition
            .fetch(&filter, None, 100)
            .await
            .expect("fetch CANCELLED");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].0, cancelled);

        // ② 非法码：SQL 之前就 422，消息点名合法码
        let mut filter = base();
        filter.status_code = Some("OPEN".to_string());
        let error = check_export_filter(&definition, &mut filter).expect_err("OPEN 不是凭证状态");
        assert_eq!(error.status, 422);
        assert!(error.msg.contains("POSTED") && error.msg.contains("AUDITED"));

        // 清理（真库用例不留痕）
        for id in [posted, cancelled] {
            let _ = db()
                .exec_sql(
                    format!("DELETE FROM biz_voucher WHERE id = '{id}'::uuid"),
                    vec![],
                )
                .await;
        }
    }
}
