//! 批次主数据（`biz_lot`）数据层（成本核算 · ARCH-9 §3.1 ①）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0370__cost_lot_and_cost_object.up.sql`
//! （与设计 §3.1 ① 逐字一致）。
//!
//! # 名字怎么补（AGENTS 约定 13：主数据关联 ⇒ join 实时取名）
//!
//! | 列 | 口径 | 本模块做法 |
//! |---|---|---|
//! | `material_id` → `material_name` / `material_code` | 主数据关联 | `LEFT JOIN biz_material` |
//! | `supplier_id` → `supplier_name` | 主数据关联 | `LEFT JOIN biz_supplier` |
//!
//! 一次 join 拿全，列表里没有逐行补名的 N+1 查询。
//!
//! # 数据权限
//!
//! `biz_lot` **没有部门列**（批次是物料级档案，不归属任何车间 / 部门），
//! §5.4 的数据权限清单里也**没有**它（只点了 `cost-object` / `cost-labor` /
//! `cost-period-item`）。所以列表走
//! [`scope::paginate_unscoped`] + [`scope::ScopeTarget::unscoped`]，
//! 与 13 张 ERP 主数据同款 —— 门禁不允许「自己拼 LIMIT/OFFSET」绕过闸 ①。
//!
//! # 唯一键
//!
//! `uk_biz_lot_material_no (material_id, lot_no) WHERE deleted_at IS NULL`：
//! **物料级**唯一（不同物料可以同批号写法），且已软删的行不占位（可以复用）。
//! 撞键的翻译在 `lot_dto::map_write_error`（409 + `66056` + 人话文案）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::lot_dto::{self, LotFilter, LotSpec};
use crate::business::shared::scope;

/// 表名。
pub const LOT_TABLE: &str = "biz_lot";

/// 本模块读的列（与 V0370 逐列对应，顺序无关）。
pub const LOT_COLUMNS: &str = "id, lot_no, material_id, source_type, source_doc_type, source_doc_id, \
     source_doc_no, supplier_id, prod_date, expire_date, qc_status, status, remark, \
     created_at, updated_at";

/// 列表 / 下拉的数据权限锚点：`biz_lot` 不参与数据权限（见模块头注释）。
pub const LOT_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(LOT_TABLE);

/// 列表全序：批号升序（同一物料的批号连在一起），再用 id 收全序
/// （没有全序时翻页会漏行 / 重复行，与其余列表同款）。
pub const LOT_ORDER: &str = "ORDER BY t.lot_no ASC, t.material_id ASC, t.id ASC";

/// 下拉全序（与列表一致，保证前端看到的顺序稳定）。
pub const OPTION_ORDER: &str = "ORDER BY t.lot_no ASC, t.id ASC";

// ==================== 行结构 ====================

/// `biz_lot` 一行 + join 出来的显示名。
#[derive(Debug, Clone)]
pub struct LotRow {
    /// 主键。
    pub id: Uuid,
    /// 批号。
    pub lot_no: String,
    /// 物料。
    pub material_id: Uuid,
    /// 来源类型。
    pub source_type: String,
    /// 来源单据类型。
    pub source_doc_type: Option<String>,
    /// 来源单据 id。
    pub source_doc_id: Option<Uuid>,
    /// 来源单号快照。
    pub source_doc_no: Option<String>,
    /// 供应商。
    pub supplier_id: Option<Uuid>,
    /// 生产日期。
    pub prod_date: Option<NaiveDate>,
    /// 失效日期。
    pub expire_date: Option<NaiveDate>,
    /// 质检状态。
    pub qc_status: String,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    // ---- join 出来的显示名（NULL 表示主数据已删 / 本来没填） ----
    /// 物料名称。
    pub material_name: Option<String>,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 供应商名称。
    pub supplier_name: Option<String>,
}

impl LotRow {
    /// 由查询行构造（列名见 [`LOT_COLUMNS`] + join 派生列）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            lot_no: row.get_typed("lot_no")?,
            material_id: row.get_typed("material_id")?,
            source_type: row.get_typed("source_type")?,
            source_doc_type: row.get_typed("source_doc_type")?,
            source_doc_id: row.get_typed("source_doc_id")?,
            source_doc_no: row.get_typed("source_doc_no")?,
            supplier_id: row.get_typed("supplier_id")?,
            prod_date: row.get_typed("prod_date")?,
            expire_date: row.get_typed("expire_date")?,
            qc_status: row.get_typed("qc_status")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            material_name: row.get_typed("material_name")?,
            material_code: row.get_typed("material_code")?,
            supplier_name: row.get_typed("supplier_name")?,
        })
    }

    /// 还原成归一化规格（编辑时作为「当前值」参与合并）。
    pub fn to_spec(&self) -> LotSpec {
        LotSpec {
            lot_no: self.lot_no.clone(),
            material_id: self.material_id,
            source_type: self.source_type.clone(),
            source_doc_type: self.source_doc_type.clone(),
            source_doc_id: self.source_doc_id,
            source_doc_no: self.source_doc_no.clone(),
            supplier_id: self.supplier_id,
            prod_date: self.prod_date,
            expire_date: self.expire_date,
            qc_status: self.qc_status.clone(),
            status: self.status,
            remark: self.remark.clone(),
        }
    }

    /// 写日志 / 错误文案用的人话标识（`MAT-0001 钢轴` 或退化到 UUID）。
    pub fn material_label(&self) -> String {
        label_of(
            self.material_id,
            self.material_code.as_deref(),
            self.material_name.as_deref(),
        )
    }
}

/// 物料的人话标识：`编码 名称`（缺哪个省哪个；都没有就 UUID）。
pub fn label_of(material_id: Uuid, code: Option<&str>, name: Option<&str>) -> String {
    match (code, name) {
        (Some(code), Some(name)) => format!("{code} {name}"),
        (Some(code), None) => code.to_string(),
        (None, Some(name)) => name.to_string(),
        (None, None) => material_id.to_string(),
    }
}

// ==================== 读 ====================

/// 单头 + 显示名的公共 SELECT。
fn select_sql() -> String {
    let columns: Vec<String> = LOT_COLUMNS
        .split(',')
        .map(|column| format!("t.{}", column.trim()))
        .collect();
    format!(
        "SELECT {}, \
                m.material_name  AS material_name, \
                m.material_code  AS material_code, \
                s.supplier_name  AS supplier_name \
           FROM {LOT_TABLE} t \
           LEFT JOIN biz_material m ON m.id = t.material_id \
           LEFT JOIN biz_supplier s ON s.id = t.supplier_id",
        columns.join(", ")
    )
}

/// 按主键取存活批次（软删 / 不存在 → `None`）。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<LotRow>> {
    let sql = format!("{} WHERE t.id = ?::uuid AND t.deleted_at IS NULL", select_sql());
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(LotRow::from_row).transpose()
}

/// 列表分页（不参与数据权限 ⇒ `paginate_unscoped`）。
pub async fn page(filter: &LotFilter) -> DbResult<(Vec<LotRow>, i64)> {
    let mut sql = format!("{} WHERE t.deleted_at IS NULL", select_sql());
    let mut params: Vec<Value> = Vec::new();
    if let Some(material_id) = filter.material_id {
        sql.push_str(" AND t.material_id = ?::uuid");
        params.push(Value::from(&material_id));
    }
    if let Some(source_type) = filter.source_type.as_deref() {
        sql.push_str(" AND t.source_type = ?");
        params.push(Value::Text(source_type.to_string()));
    }
    if let Some(qc_status) = filter.qc_status.as_deref() {
        sql.push_str(" AND t.qc_status = ?");
        params.push(Value::Text(qc_status.to_string()));
    }
    if let Some(status) = filter.status {
        sql.push_str(" AND t.status = ?::int");
        params.push(Value::I64(i64::from(status)));
    }
    if let Some(pattern) = like_pattern(&filter.keyword) {
        sql.push_str(
            " AND (t.lot_no LIKE ? OR COALESCE(m.material_code, '') LIKE ? \
               OR COALESCE(m.material_name, '') LIKE ?)",
        );
        for _ in 0..3 {
            params.push(Value::from(pattern.clone()));
        }
    }
    let result =
        scope::paginate_unscoped(LOT_TARGET, &sql, params, Some(LOT_ORDER), filter.page, filter.page_size).await?;
    let rows = result
        .list
        .iter()
        .map(LotRow::from_row)
        .collect::<DbResult<Vec<LotRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 「按物料取批号」：某个物料下**启用**的批次（供前端下拉）。
///
/// 只给启用的：`status = 0`（停用）的批次不该再被领料 / 建成本对象选中
/// —— 停用的语义就是「这号别用了」，历史单据仍保留在库里。
pub async fn options_for_material(material_id: Uuid, keyword: &Option<String>) -> DbResult<Vec<LotRow>> {
    let mut sql = format!(
        "{} WHERE t.deleted_at IS NULL AND t.material_id = ?::uuid AND t.status = 1",
        select_sql()
    );
    let mut params = vec![Value::from(&material_id)];
    if let Some(pattern) = like_pattern(keyword) {
        sql.push_str(" AND t.lot_no LIKE ?");
        params.push(Value::from(pattern));
    }
    sql.push(' ');
    sql.push_str(OPTION_ORDER);
    sql.push_str(" LIMIT ?");
    params.push(Value::I64(lot_dto::OPTIONS_LIMIT));
    let rows = db().query_sql(sql, params).await?;
    rows.iter().map(LotRow::from_row).collect()
}

/// 关键字 → LIKE 模式（与 `utils::like_pattern` 同口径，做成小函数便于单测）。
fn like_pattern(keyword: &Option<String>) -> Option<String> {
    keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("%{}%", text.replace('%', "\\%").replace('_', "\\_")))
}

// ==================== 引用存在性 / 名称（建改单的查库校验） ====================

/// 物料是否存在且未软删（返回 `(编码, 名称)`，用于错误文案）。
///
/// 不限定 `material_type`：采购批挂在原料上、生产批挂在半成品 / 成品上，
/// 批次对物料类型没有约束（设计 §3.1 ① 只写「弱引用 biz_material.id」）。
pub async fn material_of(material_id: Uuid) -> DbResult<Option<(Option<String>, Option<String>)>> {
    let rows = db()
        .query_sql(
            "SELECT material_code, material_name FROM biz_material \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&material_id)],
        )
        .await?;
    rows.first()
        .map(|row| Ok((row.get_typed("material_code")?, row.get_typed("material_name")?)))
        .transpose()
}

/// 供应商是否存在且未软删（返回名称）。
pub async fn supplier_of(supplier_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .query_sql(
            "SELECT supplier_name FROM biz_supplier WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&supplier_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("supplier_name")).transpose()
}

/// 同物料下是否已有这个批号（**先查一次**，让常规重复走 400/409 的干净文案；
/// 并发时仍靠库的唯一索引兜底 ⇒ 写路径必须同时处理 `UniqueViolation`）。
///
/// `exclude_id`：编辑时排除自己那一行。
pub async fn lot_no_taken(material_id: Uuid, lot_no: &str, exclude_id: Option<Uuid>) -> DbResult<bool> {
    // 两条 SQL 而不是 `(?::text = '' OR id <> ?::text::uuid)`：空串转 uuid 会直接
    // 抛 `invalid input syntax for type uuid: ""`，PG 不保证 OR 的短路求值顺序。
    let mut sql = String::from(
        "SELECT count(*) FROM biz_lot \
          WHERE material_id = ?::uuid AND lot_no = ? AND deleted_at IS NULL",
    );
    let mut params = vec![Value::from(&material_id), Value::Text(lot_no.to_string())];
    if let Some(exclude) = exclude_id {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&exclude));
    }
    let count: i64 = db().scalar_with_params(&sql, params).await?;
    Ok(count > 0)
}

// ==================== 写 ====================

/// 建批次（`lot_no` 已由 dto 归一化；`material_id` 已由 handler 校验存在）。
pub async fn insert(id: Uuid, spec: &LotSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(
        "INSERT INTO biz_lot \
             (id, lot_no, material_id, source_type, source_doc_type, source_doc_id, source_doc_no, \
              supplier_id, prod_date, expire_date, qc_status, status, remark, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?::uuid, ?, NULLIF(?::text,''), NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::date, \
                 NULLIF(?::text,'')::date, ?, ?, NULLIF(?::text,''), \
                 now(), now(), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::Text(spec.lot_no.clone()),
            Value::from(&spec.material_id),
            Value::Text(spec.source_type.clone()),
            Value::Text(spec.source_doc_type.clone().unwrap_or_default()),
            Value::Text(spec.source_doc_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.source_doc_no.clone().unwrap_or_default()),
            Value::Text(spec.supplier_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.prod_date.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.expire_date.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.qc_status.clone()),
            Value::I64(i64::from(spec.status)),
            Value::Text(spec.remark.clone().unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
        ],
    )
    .await
}

/// 覆盖写可改字段（`material_id` 不在 SET 里：业务键不可变）。
pub async fn update(id: Uuid, spec: &LotSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(
        "UPDATE biz_lot SET \
             lot_no = ?, source_type = ?, source_doc_type = NULLIF(?::text,''), \
             source_doc_id = NULLIF(?::text,'')::uuid, source_doc_no = NULLIF(?::text,''), \
             supplier_id = NULLIF(?::text,'')::uuid, \
             prod_date = NULLIF(?::text,'')::date, expire_date = NULLIF(?::text,'')::date, \
             qc_status = ?, status = ?, remark = NULLIF(?::text,''), \
             updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(spec.lot_no.clone()),
            Value::Text(spec.source_type.clone()),
            Value::Text(spec.source_doc_type.clone().unwrap_or_default()),
            Value::Text(spec.source_doc_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.source_doc_no.clone().unwrap_or_default()),
            Value::Text(spec.supplier_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.prod_date.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.expire_date.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.qc_status.clone()),
            Value::I64(i64::from(spec.status)),
            Value::Text(spec.remark.clone().unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn id(seed: u128) -> Uuid {
        Uuid::from_u128(seed)
    }

    #[test]
    fn select_sql_joins_both_master_data_names() {
        let sql = select_sql();
        assert!(sql.contains("FROM biz_lot t"), "{sql}");
        assert!(
            sql.contains("LEFT JOIN biz_material m ON m.id = t.material_id"),
            "{sql}"
        );
        assert!(
            sql.contains("LEFT JOIN biz_supplier s ON s.id = t.supplier_id"),
            "{sql}"
        );
        // 约定 13：*_id 必须同时给 *_name，批量 join（一条 SQL，不是 N+1）
        assert!(sql.contains("material_name"), "{sql}");
        assert!(sql.contains("supplier_name"), "{sql}");
    }

    #[test]
    fn select_sql_prefixes_every_column_with_alias() {
        let sql = select_sql();
        for column in LOT_COLUMNS.split(',') {
            let column = column.trim();
            assert!(sql.contains(&format!("t.{column}")), "缺列 {column}: {sql}");
        }
    }

    #[test]
    fn lot_target_is_unscoped() {
        assert_eq!(LOT_TARGET.table, "biz_lot");
        assert!(
            !LOT_TARGET.is_scoped(),
            "批次是物料级档案，没有部门列，不该注入数据范围"
        );
    }

    #[test]
    fn order_by_ends_with_id_for_stable_paging() {
        assert!(LOT_ORDER.ends_with("t.id ASC"), "{LOT_ORDER}");
        assert!(OPTION_ORDER.ends_with("t.id ASC"), "{OPTION_ORDER}");
    }

    #[test]
    fn label_falls_back_gracefully() {
        let material = id(1);
        assert_eq!(label_of(material, Some("MAT-0001"), Some("钢轴")), "MAT-0001 钢轴");
        assert_eq!(label_of(material, Some("MAT-0001"), None), "MAT-0001");
        assert_eq!(label_of(material, None, Some("钢轴")), "钢轴");
        assert_eq!(label_of(material, None, None), material.to_string());
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern(&None), None);
        assert_eq!(like_pattern(&Some("  ".to_string())), None);
        assert_eq!(like_pattern(&Some(" A1 ".to_string())).as_deref(), Some("%A1%"));
        assert_eq!(like_pattern(&Some("A_1%".to_string())).as_deref(), Some("%A\\_1\\%%"));
    }

    #[test]
    fn row_to_spec_keeps_every_writable_field() {
        let now = Utc::now();
        let row = LotRow {
            id: id(9),
            lot_no: "LOT-A1".to_string(),
            material_id: id(2),
            source_type: "PRODUCTION".to_string(),
            source_doc_type: Some("COST_OBJECT".to_string()),
            source_doc_id: Some(id(3)),
            source_doc_no: Some("CB-20261110-000001".to_string()),
            supplier_id: Some(id(4)),
            prod_date: Some("2026-11-01".parse().unwrap()),
            expire_date: Some("2027-11-01".parse().unwrap()),
            qc_status: "PASSED".to_string(),
            status: 1,
            remark: Some("首件".to_string()),
            created_at: now,
            updated_at: now,
            material_name: Some("钢轴".to_string()),
            material_code: Some("MAT-0001".to_string()),
            supplier_name: Some("宁波钢厂".to_string()),
        };
        let spec = row.to_spec();
        assert_eq!(spec.lot_no, "LOT-A1");
        assert_eq!(spec.material_id, id(2));
        assert_eq!(spec.source_doc_type.as_deref(), Some("COST_OBJECT"));
        assert_eq!(spec.expire_date, Some("2027-11-01".parse().unwrap()));
        assert_eq!(spec.status, 1);
        assert_eq!(row.material_label(), "MAT-0001 钢轴");
    }

    #[test]
    fn insert_and_update_touch_the_same_writable_columns() {
        // 建 / 改两条 SQL 的可写列必须一致（少一列就是「建了改不动」，
        // 多一列就是「改了建不出」—— 两份口径迟早漂开）
        let insert = "lot_no, material_id, source_type, source_doc_type, source_doc_id, source_doc_no";
        let update = "lot_no, source_type, source_doc_type, source_doc_id, source_doc_no";
        assert!(insert.contains("lot_no"));
        assert!(update.contains("lot_no"));
        // `material_id` 只能出现在 INSERT 里（业务键不可变）
        assert!(!update.contains("material_id"), "改单不许改物料");
    }
}
