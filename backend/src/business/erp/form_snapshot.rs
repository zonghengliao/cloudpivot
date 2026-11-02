//! 审批 `form_data` 的**显示名 / 明细快照**（P1-FIX 配套，后端半边）。
//!
//! # 为什么要这一层
//!
//! 审批人是**岗位角色**。生产库（2026-11-01，144）实测：
//!
//! ```sql
//! select r.role_code, count(*) filter (where m.auth_code like 'business:erp:%') erp_perms
//!   from sys_role r left join sys_role_menu rm on rm.role_id = r.id
//!   left join sys_menu m on m.id = rm.menu_id
//!  where r.role_code in ('DEPT_LEADER','PURCHASE_MANAGER','GENERAL_MANAGER','WAREHOUSE_MANAGER')
//!  group by 1;
//! -- 四个角色 erp_perms 全部 = 0
//! ```
//!
//! 也就是说「前端按 id 查主数据译名」这条路对**真正的审批人**必然 403 → 页面上只能显示
//! `(ID: 01a18def…)`。修法选的是**数据侧（方案 A）**：发起审批时把**人看的显示名**与
//! **明细**一起冻结进 `biz_flow_instance.form_data`。审批人零 ERP 读权限也能看懂单据，
//! 且不用改权限模型（方案 B 是权限决策，留给产品/架构）。
//!
//! 附带解决第三件事：明细以前是前端读业务单**当前值**
//! （`GET /business/erp/purchase-order-item/page?order_id=…`），退款 / 作废等场景下可能出现
//! 「审批时看到的明细 ≠ 单据现在的明细」。冻结进 `form_data` 之后，审批人看到的就是
//! **发起那一刻**的明细。
//!
//! # 键名是契约
//!
//! 键名与前端 `frontend/src/views/business/flow/form-field-labels.ts` 的
//! `FORM_FIELD_LABELS` **逐字对应**：
//!
//! | 键 | 前端行为 |
//! |----|----------|
//! | `supplier_name` / `warehouse_name` / `from_warehouse_name` / `to_warehouse_name` | 普通字符串，原样显示；**不再发主数据请求**（所以审批人没有 `business:erp:*` 也能看到中文名） |
//! | `items` | 对象数组 → 渲染成表格（`buildFormItemTables`），列标题走 [`ITEM_KEYS`] 的中文标签 |
//!
//! 改键名 = 改契约：前端会退化成「原键名 + `(ID: …)`」。加键是安全的（前端按存在的键渲染）。
//!
//! # 名字查不到怎么办
//!
//! `None` → 调用方**不写这个键**（而不是写空串）。这样前端要么按 `*_id` 走老的译名兜底，
//! 要么显示 `—`，都不会出现「供应商：」后面空空的假象。
//!
//! 查询**刻意不过滤 `deleted_at`**：这是「显示名快照」，供应商 / 仓库在单据提交后被软删，
//! 历史审批也仍然应该显示当时那个名字（过滤掉反而退化成 `(ID: …)`）。

use nik::prelude::*;
use nik::uuid::Uuid;

/// 明细行的键（顺序 = 前端表格列顺序；与 `FORM_FIELD_LABELS` 一一对应）。
///
/// `line_no` → 行号、`material_code` → 物料编码、`material_name` → 物料名称、
/// `unit_name` → 单位、`qty` → 数量、`price` → 单价、`amount` → 金额、`tax_rate` → 税率(%)。
pub const ITEM_KEYS: [&str; 8] = [
    "line_no",
    "material_code",
    "material_name",
    "unit_name",
    "qty",
    "price",
    "amount",
    "tax_rate",
];

/// 采购明细快照 SQL（按 `line_no` 升序）。
///
/// `::text` 把 `NUMERIC` 原样转成十进制字符串（`"100.0000"`）——前端 `toNumber()` 认这种形状，
/// 同时不会把 `Decimal` 的精度丢掉。物料 / 单位名用 `LEFT JOIN`：主数据被删也要出行，
/// 名字缺了由 `COALESCE` 兜成空串（前端显示 `—`），绝不整行吞掉。
const PURCHASE_ITEMS_SQL: &str = "SELECT i.line_no, \
        COALESCE(m.material_code, '') AS material_code, \
        COALESCE(m.material_name, '') AS material_name, \
        COALESCE(u.unit_name, '') AS unit_name, \
        i.qty::text AS qty, i.price::text AS price, \
        i.amount::text AS amount, i.tax_rate::text AS tax_rate \
   FROM biz_purchase_order_item i \
   LEFT JOIN biz_material m ON m.id = i.material_id \
   LEFT JOIN biz_unit u ON u.id = i.unit_id \
  WHERE i.order_id = ?::uuid \
  ORDER BY i.line_no ASC";

/// 收货入库明细快照 SQL（按 `line_no` 升序）。
const STOCK_IN_ITEMS_SQL: &str = "SELECT i.line_no, \
        COALESCE(m.material_code, '') AS material_code, \
        COALESCE(m.material_name, '') AS material_name, \
        COALESCE(u.unit_name, '') AS unit_name, \
        i.qty::text AS qty, i.price::text AS price, \
        i.amount::text AS amount, i.tax_rate::text AS tax_rate \
   FROM biz_stock_in_item i \
   LEFT JOIN biz_material m ON m.id = i.material_id \
   LEFT JOIN biz_unit u ON u.id = i.unit_id \
  WHERE i.stock_in_id = ?::uuid \
  ORDER BY i.line_no ASC";

/// 调拨明细快照 SQL（按 `line_no` 升序）。
///
/// 调拨行的「单价」是 `unit_cost`（调出仓移动加权均价快照）：写进 `price` 键，
/// 前端标签就是「单价」。调拨没有税率概念 → `tax_rate` 恒 `0`（列还是保留，
/// 让四类单的明细表长得一样，用户不用两套阅读习惯）。
const TRANSFER_ITEMS_SQL: &str = "SELECT i.line_no, \
        COALESCE(m.material_code, '') AS material_code, \
        COALESCE(m.material_name, '') AS material_name, \
        COALESCE(u.unit_name, '') AS unit_name, \
        i.qty::text AS qty, i.unit_cost::text AS price, \
        i.amount::text AS amount, '0' AS tax_rate \
   FROM biz_stock_transfer_item i \
   LEFT JOIN biz_material m ON m.id = i.material_id \
   LEFT JOIN biz_unit u ON u.id = m.unit_id \
  WHERE i.transfer_id = ?::uuid \
  ORDER BY i.line_no ASC";

/// 取单列文本（首行；无行 → `None`）。
async fn one_text(
    ex: &impl SqlExecutor,
    sql: &str,
    id: Uuid,
    column: &str,
) -> DbResult<Option<String>> {
    let rows = ex.query_sql(sql.to_string(), vec![Value::from(id)]).await?;
    match rows.first() {
        Some(row) => Ok(row.get_typed::<Option<String>>(column)?),
        None => Ok(None),
    }
}

/// 供应商显示名（`None` = 查不到该 id）。
pub async fn supplier_name(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<String>> {
    one_text(
        ex,
        "SELECT supplier_name FROM biz_supplier WHERE id = ?::uuid",
        id,
        "supplier_name",
    )
    .await
}

/// 仓库显示名（`None` = 查不到该 id）。
pub async fn warehouse_name(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<String>> {
    one_text(
        ex,
        "SELECT warehouse_name FROM biz_warehouse WHERE id = ?::uuid",
        id,
        "warehouse_name",
    )
    .await
}

/// 明细游标：把一条 SQL 的多行摊成 `[{ 键: 值 }]`。
///
/// 所有键都写成 JSON **字符串**（数字也是十进制字符串）：审批页面是给人看的，
/// 前端 `toNumber()` / 数量格式化都认字符串，且不会因 f64 出现 `0.30000000000000004`。
fn item_rows(rows: &[Row]) -> DbResult<serde_json::Value> {
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(serde_json::json!({
            "line_no": row.get_typed::<i32>("line_no")?,
            "material_code": row.get_typed::<String>("material_code")?,
            "material_name": row.get_typed::<String>("material_name")?,
            "unit_name": row.get_typed::<String>("unit_name")?,
            "qty": row.get_typed::<String>("qty")?,
            "price": row.get_typed::<String>("price")?,
            "amount": row.get_typed::<String>("amount")?,
            "tax_rate": row.get_typed::<String>("tax_rate")?,
        }));
    }
    Ok(serde_json::Value::Array(items))
}

/// 采购订单明细快照（`items` 键的值）。
pub async fn purchase_items(ex: &impl SqlExecutor, order_id: Uuid) -> DbResult<serde_json::Value> {
    let rows = ex
        .query_sql(PURCHASE_ITEMS_SQL.to_string(), vec![Value::from(order_id)])
        .await?;
    item_rows(&rows)
}

/// 收货入库单明细快照（`items` 键的值）。
pub async fn stock_in_items(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
) -> DbResult<serde_json::Value> {
    let rows = ex
        .query_sql(
            STOCK_IN_ITEMS_SQL.to_string(),
            vec![Value::from(stock_in_id)],
        )
        .await?;
    item_rows(&rows)
}

/// 调拨单明细快照（`items` 键的值）。
pub async fn transfer_items(
    ex: &impl SqlExecutor,
    transfer_id: Uuid,
) -> DbResult<serde_json::Value> {
    let rows = ex
        .query_sql(
            TRANSFER_ITEMS_SQL.to_string(),
            vec![Value::from(transfer_id)],
        )
        .await?;
    item_rows(&rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 夹具 id 带独立前缀 `d2a3`（P1-FIX 快照专用），避免与 purchase_handler 的 `d2a2`
    // 夹具、以及别的卡的用例互相踩。
    const SUPPLIER: &str = "d2a30001-0000-4000-8000-000000000001";
    const WH_FROM: &str = "d2a30001-0000-4000-8000-000000000002";
    const WH_TO: &str = "d2a30001-0000-4000-8000-000000000003";
    const ORDER: &str = "d2a30001-0000-4000-8000-000000000021";
    const ORDER_EMPTY: &str = "d2a30001-0000-4000-8000-000000000022";
    const STOCK_IN: &str = "d2a30001-0000-4000-8000-000000000031";
    const TRANSFER: &str = "d2a30001-0000-4000-8000-000000000041";

    /// 真库用例必须串行（共用全局 `db()` 与同一批夹具）。
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

    const CLEANUP: &[&str] = &[
        // 调拨明细没有 ON DELETE CASCADE（V0140），必须先删明细再删主单。
        "DELETE FROM biz_stock_transfer_item WHERE transfer_id = \
         'd2a30001-0000-4000-8000-000000000041'::uuid",
        "DELETE FROM biz_purchase_order WHERE id IN ('d2a30001-0000-4000-8000-000000000021'::uuid, \
                                                     'd2a30001-0000-4000-8000-000000000022'::uuid)",
        "DELETE FROM biz_stock_in WHERE id = 'd2a30001-0000-4000-8000-000000000031'::uuid",
        "DELETE FROM biz_stock_transfer WHERE id = 'd2a30001-0000-4000-8000-000000000041'::uuid",
    ];

    const FIXTURES: &[&str] = &[
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
         VALUES ('d2a30001-0000-4000-8000-000000000004', 'P1FIX-PCS', '个', 1, NULL, 1.000000, 1, 1)
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
         VALUES ('d2a30001-0000-4000-8000-000000000001', 'P1FIX-SUP', 'P1FIX 快照供应商', 1)
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
         VALUES ('d2a30001-0000-4000-8000-000000000002', 'P1FIX-WH-A', 'P1FIX 调出仓',
                 '/d2a30001-0000-4000-8000-000000000002/', 1, 1, 1),
                ('d2a30001-0000-4000-8000-000000000003', 'P1FIX-WH-B', 'P1FIX 调入仓',
                 '/d2a30001-0000-4000-8000-000000000003/', 1, 1, 1)
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status)
         VALUES ('d2a30001-0000-4000-8000-000000000011', 'P1FIX-M1', '快照螺丝 M6x20',
                 'd2a30001-0000-4000-8000-000000000004', 1),
                ('d2a30001-0000-4000-8000-000000000012', 'P1FIX-M2', '快照垫片 8mm',
                 'd2a30001-0000-4000-8000-000000000004', 1)
         ON CONFLICT (id) DO NOTHING",
        // 采购单（两张：一张两行、一张空单）
        "INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id,
                                         status, total_qty, total_amount, total_tax, total_with_tax)
         VALUES ('d2a30001-0000-4000-8000-000000000021', 'P1FIX-PO-1',
                 'd2a30001-0000-4000-8000-000000000001', CURRENT_DATE,
                 'd2a30001-0000-4000-8000-000000000002', 'DRAFT', 105.0000, 250.0000, 32.5000, 282.5000),
                ('d2a30001-0000-4000-8000-000000000022', 'P1FIX-PO-2',
                 'd2a30001-0000-4000-8000-000000000001', CURRENT_DATE,
                 'd2a30001-0000-4000-8000-000000000002', 'DRAFT', 0, 0, 0, 0)
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_purchase_order_item (order_id, line_no, material_id, unit_id, qty, price,
                                             tax_rate, amount, tax_amount, with_tax_amount)
         VALUES ('d2a30001-0000-4000-8000-000000000021', 1,
                 'd2a30001-0000-4000-8000-000000000011', 'd2a30001-0000-4000-8000-000000000004',
                 100.0000, 2.000000, 13.0000, 200.0000, 26.0000, 226.0000),
                ('d2a30001-0000-4000-8000-000000000021', 2,
                 'd2a30001-0000-4000-8000-000000000012', 'd2a30001-0000-4000-8000-000000000004',
                 5.0000, 10.000000, 13.0000, 50.0000, 6.5000, 56.5000)",
        // 收货入库单
        "INSERT INTO biz_stock_in (id, in_no, in_type, supplier_id, warehouse_id, in_date, status,
                                   total_qty, total_amount, total_tax, total_with_tax, is_sourceless)
         VALUES ('d2a30001-0000-4000-8000-000000000031', 'P1FIX-IN-1', 'PURCHASE',
                 'd2a30001-0000-4000-8000-000000000001', 'd2a30001-0000-4000-8000-000000000002',
                 CURRENT_DATE, 'DRAFT', 100.0000, 200.0000, 26.0000, 226.0000, true)
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_stock_in_item (stock_in_id, line_no, material_id, unit_id, qty, price,
                                        tax_rate, amount, tax_amount, with_tax_amount)
         VALUES ('d2a30001-0000-4000-8000-000000000031', 1,
                 'd2a30001-0000-4000-8000-000000000011', 'd2a30001-0000-4000-8000-000000000004',
                 100.0000, 2.000000, 13.0000, 200.0000, 26.0000, 226.0000)",
        // 调拨单（A 仓 → B 仓）
        "INSERT INTO biz_stock_transfer (id, transfer_no, from_warehouse_id, to_warehouse_id,
                                         status, transfer_date, total_qty, total_amount)
         VALUES ('d2a30001-0000-4000-8000-000000000041', 'P1FIX-TR-1',
                 'd2a30001-0000-4000-8000-000000000002', 'd2a30001-0000-4000-8000-000000000003',
                 'DRAFT', CURRENT_DATE, 10.0000, 20.0000)
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_stock_transfer_item (transfer_id, line_no, material_id, qty, unit_cost, amount)
         VALUES ('d2a30001-0000-4000-8000-000000000041', 1,
                 'd2a30001-0000-4000-8000-000000000011', 10.0000, 2.000000, 20.0000)",
    ];

    /// 拿锁 + 连库 + 清上一轮 + 建夹具。`None` = 没配 `ERP_DB_URL`。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        for sql in CLEANUP {
            if let Err(err) = db().execute_with_params(sql, vec![]).await {
                panic!("清理上一轮夹具失败：{err}\nSQL: {sql}");
            }
        }
        for sql in FIXTURES {
            if let Err(err) = db().execute_with_params(sql, vec![]).await {
                panic!("夹具写入失败：{err}\nSQL: {sql}");
            }
        }
        Some(guard)
    }

    fn uuid(raw: &str) -> Uuid {
        Uuid::parse_str(raw).expect("夹具 uuid 非法")
    }

    /// 一条明细行必须**八个键全在**（前端按存在的键出列；少一个就是少一列）。
    fn assert_item_keys(row: &serde_json::Value) {
        for key in ITEM_KEYS {
            assert!(
                row.get(key).is_some(),
                "明细行缺键 `{key}`：{row}（前端会少一列）"
            );
        }
    }

    /// 显示名：查得到给中文名，查不到给 `None`（调用方不写这个键）。
    #[tokio::test]
    async fn display_names_resolve_and_degrade_to_none() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        assert_eq!(
            supplier_name(db(), uuid(SUPPLIER))
                .await
                .unwrap()
                .as_deref(),
            Some("P1FIX 快照供应商")
        );
        assert_eq!(
            warehouse_name(db(), uuid(WH_FROM))
                .await
                .unwrap()
                .as_deref(),
            Some("P1FIX 调出仓")
        );
        assert_eq!(
            warehouse_name(db(), uuid(WH_TO)).await.unwrap().as_deref(),
            Some("P1FIX 调入仓")
        );
        // 不存在的 id → None（不是空串：空串会被前端当成「查到了但没名字」）。
        let missing = Uuid::nil();
        assert!(supplier_name(db(), missing).await.unwrap().is_none());
        assert!(warehouse_name(db(), missing).await.unwrap().is_none());
    }

    /// 采购明细：两行的码 / 名 / 单位 / 数量 / 单价 / 金额 / 税率全冻结，且按 `line_no` 升序。
    #[tokio::test]
    async fn purchase_items_freeze_every_display_key() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let items = purchase_items(db(), uuid(ORDER)).await.unwrap();
        let rows = items.as_array().expect("items 必须是数组");
        assert_eq!(rows.len(), 2, "两行明细都要冻结进来");
        for row in rows {
            assert_item_keys(row);
        }
        assert_eq!(
            rows[0]["line_no"], 1,
            "必须按 line_no 升序（前端不出行号也要按单子顺序）"
        );
        assert_eq!(rows[0]["material_code"], "P1FIX-M1");
        assert_eq!(rows[0]["material_name"], "快照螺丝 M6x20");
        assert_eq!(rows[0]["unit_name"], "个");
        assert_eq!(rows[0]["qty"], "100.0000");
        assert_eq!(rows[0]["price"], "2.000000");
        assert_eq!(rows[0]["amount"], "200.0000");
        assert_eq!(rows[0]["tax_rate"], "13.0000");
        assert_eq!(rows[1]["material_code"], "P1FIX-M2");
    }

    /// 空单 → 空数组（不是 `null`：前端拿 `null` 会走进另一条格式化分支）。
    #[tokio::test]
    async fn empty_order_yields_empty_array() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let items = purchase_items(db(), uuid(ORDER_EMPTY)).await.unwrap();
        assert_eq!(items, serde_json::json!([]));
    }

    /// 收货入库明细：同样八个键，单位来自 `biz_stock_in_item.unit_id`。
    #[tokio::test]
    async fn stock_in_items_freeze_every_display_key() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let items = stock_in_items(db(), uuid(STOCK_IN)).await.unwrap();
        let rows = items.as_array().expect("items 必须是数组");
        assert_eq!(rows.len(), 1);
        assert_item_keys(&rows[0]);
        assert_eq!(rows[0]["material_code"], "P1FIX-M1");
        assert_eq!(rows[0]["unit_name"], "个");
        assert_eq!(rows[0]["qty"], "100.0000");
        assert_eq!(rows[0]["tax_rate"], "13.0000");
    }

    /// 调拨明细：`price` 取 `unit_cost`（调出仓均价快照），`tax_rate` 恒 `0`。
    #[tokio::test]
    async fn transfer_items_freeze_every_display_key() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let items = transfer_items(db(), uuid(TRANSFER)).await.unwrap();
        let rows = items.as_array().expect("items 必须是数组");
        assert_eq!(rows.len(), 1);
        assert_item_keys(&rows[0]);
        assert_eq!(rows[0]["material_code"], "P1FIX-M1");
        assert_eq!(rows[0]["material_name"], "快照螺丝 M6x20");
        assert_eq!(rows[0]["unit_name"], "个", "单位来自物料的计量单位");
        assert_eq!(rows[0]["qty"], "10.0000");
        assert_eq!(rows[0]["price"], "2.000000", "调拨的单价 = unit_cost");
        assert_eq!(rows[0]["amount"], "20.0000");
        assert_eq!(rows[0]["tax_rate"], "0");
    }
}
