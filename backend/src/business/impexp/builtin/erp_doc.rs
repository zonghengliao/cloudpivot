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
//! 空则回落到 `keyword` / `status` 过滤（「选中全部筛选结果」）。**两个老定义不受影响**：
//! 它们不看 `ids`，行为与本卡之前完全一致。
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

/// 拼「关键词 / 状态 / 主键」三类过滤条件（count 与 fetch 共用，口径不许分叉）。
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
    if !filter.ids.is_empty() {
        sql.push_str(&format!(" AND {} = ANY(?::text::uuid[])", spec.id_expr));
        params.push(uuid_array(&filter.ids));
    }
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
                 total_qty, total_amount, total_tax, total_with_tax, remark) \
                 VALUES ('{doc}', 'IN-D7EXP-000001', 'PURCHASE', '{supplier}', '{warehouse}', '2026-03-01', \
                 'DRAFT', 2.0000, 20.0000, 2.6000, 22.6000, 'D7 导出测试') ON CONFLICT DO NOTHING"
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
}
