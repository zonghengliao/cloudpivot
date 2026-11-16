//! ERP **设置类**数据导入 —— 安全库存水位设置（`business.erp.stock-policy` · M2-17）。
//!
//! 卡：t_58a23185（M2-17【M2·仓库】安全库存：前端设置页 —— 批量设置 + 导入）。
//! 表结构唯一依据：`backend/app/migrations/business/V0428__erp_stock_policy_water_level.up.sql`。
//!
//! # 为什么单独一个模块（不塞进 `master_data`）
//!
//! `master_data` 的六个定义口径是「**编码即幂等键**」（`ON CONFLICT (xxx_code)`），
//! 而水位设置的幂等键是「**物料 × 仓库**」这个组合 —— 两列都不唯一、合起来才唯一。
//! 硬塞进去会把「编码唯一」的假设带进设置类数据（进而出现「同一个物料第二次导入被当成
//! 新行」或「两个仓库被当成同一格」）。同形先例：`hr` 模块的考勤导入也是独立模块
//! （幂等键是「期间 × 人」）。
//!
//! # 列契约（模板列顺序 = 下表顺序；表头由本定义生成，前端自动同步）
//!
//! | # | 列键 | 表头 | 必填 | 类型 | 落库列 |
//! |:--:|---|---|:--:|---|---|
//! | 1 | `material_code` | 物料编码 | ✅ | 文本 ≤40（须存在于 `biz_material`） | 查 → `material_id` |
//! | 2 | `warehouse_code` | 仓库编码 | ✅ | 文本 ≤40（须存在于 `biz_warehouse`） | 查 → `warehouse_id` |
//! | 3 | `min_qty` | 最低水位 | ✅ | 小数 4 位 ≥0 | `min_qty` |
//! | 4 | `max_qty` | 最高水位 | — | 小数 4 位 ≥0（留空 = 未设） | `max_qty`（空 → NULL） |
//! | 5 | `status` | 状态 | — | 启用 / 停用（留空 = 启用） | `status` |
//! | 6 | `remark` | 备注 | — | 文本 ≤200 | `remark`（空 → NULL） |
//!
//! # 幂等：同一份文件重导不产生第二行
//!
//! 唯一键是 `uk_biz_msp_mat_wh (material_id, warehouse_id) WHERE deleted_at IS NULL`
//! （**部分**唯一索引，只约束存活行），本定义按它 `ON CONFLICT … DO UPDATE`：
//!
//! ```text
//! duplicate=UPSERT（默认）→ 已存在的格被覆盖成文件里的值
//! duplicate=SKIP           → 已存在的格原样不动（只补新增）
//! ```
//!
//! 软删过的格**不算已存在**（部分索引只看存活行）⇒ 导入会为它新建一行，
//! 与页面上「再设一次」完全同口径。
//!
//! # 三条只在这里判的规则
//!
//! 1. **`max_qty > min_qty`**（跨列）：库层 `ck_biz_msp_max` 也拦，但库层报的是
//!    「约束被违反」，用户看不出是哪一行、更看不出是他把上限填得比下限低。逐行判一次，
//!    错误明细里带 Excel 行号 + 列名，才改得动。
//! 2. **文件内同格重复**（`(物料编码, 仓库编码)` 组合唯一）：两列都标 `unique_key: true`，
//!    框架按**组合**判重（`validate.rs` 的 `unique_key` 拼装）。不判的话同一格会出现两行，
//!    而 `ON CONFLICT DO UPDATE` 在同一条语句里二次命中同一行会直接报错 —— 那时用户
//!    看到的是 SQL 错误，不是「第 7 行与第 12 行是同一格」。
//! 3. **物料 / 仓库必须已存在**（`FkSpec`）：水位是**设置**，不负责建主数据；
//!    导入顺手建物料会让「编码打错一个字母」变成一条新的幽灵物料。
//!
//! # 与页面「批量设置」的关系
//!
//! 同一个写入口径（都写 `biz_material_stock_policy`、都按格 upsert），区别只在**入口**：
//! 页面上圈一批物料 × 仓库用批量接口（≤2 000 格，一个事务），手里有 Excel 表用本定义
//! （≤20 000 行，一个事务）。两条路写进去的列和约束完全一致，所以不存在「导入的数据
//! 判水位时不生效」这种漂移。
//!
//! # 每行都走框架的逐格校验
//!
//! 本模块**不解析 xlsx**：表头、必填、类型、长度、枚举、外键存在性、文件内组合唯一
//! 全由 `business::impexp` 按 [`COLUMNS`] 驱动。这里只做三件框架不管的事：
//! 跨列规则（[`StockPolicyImport::validate_row`]）、落库（[`StockPolicyImport::persist`]）、
//! 以及「本批有行没落库」的显式报错。

use async_trait::async_trait;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use crate::business::impexp::definition::{
    ColumnKind, DuplicateStrategy, EnumItem, ErrorKind, FieldIssue, FkSpec, ImportColumn, ImportCtx, ImportDefinition,
    RowIssue, RowValues,
};

/// 水位设置导入的业务编码（前端「导入」入口的 `biz-type`，逐字一致）。
pub const STOCK_POLICY_BIZ_TYPE: &str = "business.erp.stock-policy";

/// 单次导入行数上限。
///
/// 与主数据同口径（2 万行）：一个客户「几百个物料 × 十来个仓库」也到不了这个量级，
/// 留足余量的同时把「误传一个 50 万行的全库导出」挡在解析之前。
pub const MAX_ROWS: usize = 20_000;

/// 分批大小 = 行数上限 ⇒ **一份文件一个事务**。
///
/// 框架的 `STRICT` 语义是「任一行错就整体不入库」，而入库是按批开事务的。若一批只装 2 000 行，
/// 第 2 批失败时第 1 批已经提交，界面会显示「前 2 000 行已入库」—— 水位这种**设置类**数据
/// 出现「一半格是新水位、一半是旧水位」是最难排查的状态（没人知道哪一半是新的）。
/// 一份文件落在一个事务里，`STRICT` 才真的是全有或全无。
pub const BATCH_SIZE: usize = MAX_ROWS;

/// 数量的上界（`NUMERIC(18,4)` 的整数位是 14 位，即最大 99 999 999 999 999.9999）。
///
/// 取 `1e14` 作为「明显越界」的拦截线：库层能存的最大值仍小于它，所以**不会误杀**合法值，
/// 但「把数量单位填成个（1 亿个螺丝写 100000000）」这种明显录错会被逐行校验挡下来，
/// 而不是等数据库抛 numeric overflow 后看到一句英文。
const QTY_MAX: f64 = 1e14;

/// 状态枚举：Excel 写中文，落库 `1` / `0`（与 `ck_biz_msp_status` 一致）。
const STATUS_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "启用",
        code: "1",
    },
    EnumItem {
        label: "停用",
        code: "0",
    },
];

/// 导入列定义（顺序即模板列顺序）。
static COLUMNS: &[ImportColumn] = &[
    ImportColumn {
        key: "material_code",
        header: "物料编码",
        required: true,
        kind: ColumnKind::Text { max_len: Some(40) },
        // 与 warehouse_code 一起构成「文件内组合唯一」：同一格不许出现两行
        unique_key: true,
        fk: Some(FkSpec {
            table: "biz_material",
            column: "material_code",
            label: "物料",
            soft_delete: true,
        }),
        example: "M-DEMO01",
        width: Some(18),
        remark: "必须已在「物料」里存在（水位是设置，不建主数据）",
    },
    ImportColumn {
        key: "warehouse_code",
        header: "仓库编码",
        required: true,
        kind: ColumnKind::Text { max_len: Some(40) },
        unique_key: true,
        fk: Some(FkSpec {
            table: "biz_warehouse",
            column: "warehouse_code",
            label: "仓库",
            soft_delete: true,
        }),
        example: "WH-DEMO01",
        width: Some(16),
        remark: "必须已在「仓库」里存在；同一物料在同一仓库只能有一行",
    },
    ImportColumn {
        key: "min_qty",
        header: "最低水位",
        required: true,
        kind: ColumnKind::Decimal {
            scale: 4,
            min: Some(0.0),
            max: Some(QTY_MAX),
        },
        unique_key: false,
        fk: None,
        example: "100",
        width: Some(14),
        remark: "账面结存 < 它 → 判低于水位（生成待办）；填 0 = 这一格明示不预警",
    },
    ImportColumn {
        key: "max_qty",
        header: "最高水位",
        required: false,
        kind: ColumnKind::Decimal {
            scale: 4,
            min: Some(0.0),
            max: Some(QTY_MAX),
        },
        unique_key: false,
        fk: None,
        example: "500",
        width: Some(14),
        remark: "留空 = 不判超储；填了必须**严格大于**最低水位",
    },
    ImportColumn {
        key: "status",
        header: "状态",
        required: false,
        kind: ColumnKind::Enum(STATUS_ITEMS),
        unique_key: false,
        fk: None,
        example: "启用",
        width: Some(10),
        remark: "启用 / 停用，留空按「启用」；停用 = 保留数值但退回按物料级 safety_stock 判",
    },
    ImportColumn {
        key: "remark",
        header: "备注",
        required: false,
        kind: ColumnKind::Text { max_len: Some(200) },
        unique_key: false,
        fk: None,
        example: "备件仓保底",
        width: Some(24),
        remark: "为什么这么设，给下一个人看（≤200 字符）",
    },
];

/// upsert 时覆盖哪些列（物料 / 仓库是**键**，不在 `SET` 里；`id` / `created_at` 也不动）。
const UPSERT_SET: &str = "min_qty = EXCLUDED.min_qty, max_qty = EXCLUDED.max_qty, \
     status = EXCLUDED.status, remark = EXCLUDED.remark, \
     updated_at = CURRENT_TIMESTAMP, update_by = EXCLUDED.update_by";

/// 安全库存水位设置导入定义。
pub struct StockPolicyImport;

#[async_trait]
impl ImportDefinition for StockPolicyImport {
    fn biz_type(&self) -> &'static str {
        STOCK_POLICY_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "安全库存水位"
    }

    fn max_rows(&self) -> usize {
        MAX_ROWS
    }

    fn batch_size(&self) -> usize {
        BATCH_SIZE
    }

    /// 允许部分导入：一份 2 000 行的水位表里有一行编码打错，不该逼用户整份重填 ——
    /// 但**默认仍是 STRICT**（`mode` 不传 = 全有或全无），要部分导入得在向导里显式选。
    fn allow_partial(&self) -> bool {
        true
    }

    fn columns(&self) -> &'static [ImportColumn] {
        COLUMNS
    }

    /// 跨列规则：`max_qty` 填了就必须**严格大于** `min_qty`。
    ///
    /// 用 [`Decimal`] 比较而不是 `f64`：`0.1 + 0.2` 那套浮点误差会让「本来相等」的两个数
    /// 比出大小，而水位恰恰是要拿「相等 = 空区间」当错误拦下来的。
    async fn validate_row(&self, row: &RowValues, _ctx: &ImportCtx) -> std::result::Result<(), Vec<RowIssue>> {
        let min_text = row.get("min_qty").trim();
        let max_text = row.get("max_qty").trim();
        if max_text.is_empty() {
            return Ok(());
        }
        let (Ok(min_qty), Ok(max_qty)) = (min_text.parse::<Decimal>(), max_text.parse::<Decimal>()) else {
            // 解析失败已经由逐格校验报过（这里是「两列都合法才比」的兜底）
            return Ok(());
        };
        if max_qty <= min_qty {
            let column = COLUMNS
                .iter()
                .find(|column| column.key == "max_qty")
                .expect("max_qty 列一定存在");
            return Err(vec![RowIssue::column(
                row.row_no,
                column,
                max_text,
                FieldIssue {
                    kind: ErrorKind::Rule,
                    message: format!(
                        "最高水位（{max_qty}）必须严格大于最低水位（{min_qty}）：相等的上下界是空区间\
                         （既「低于就补」又「高于就停」）；不判超储请把这一格留空"
                    ),
                },
            )]);
        }
        Ok(())
    }

    async fn persist(
        &self,
        tx: &Transaction,
        rows: &[RowValues],
        ctx: &ImportCtx,
    ) -> std::result::Result<usize, DbError> {
        if rows.is_empty() {
            return Ok(0);
        }
        let strategy = duplicate_strategy(ctx).await?;
        let payload = serde_json::to_string(
            &rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        // 主键在 Rust 侧生成：新建行不必让 SQL 再 RETURNING 一次
                        "id": uuid::Uuid::now_v7().to_string(),
                        "material_code": row.get("material_code"),
                        "warehouse_code": row.get("warehouse_code"),
                        "min_qty": row.get("min_qty"),
                        // 空串 → SQL 里 NULLIF → NULL（「未设最高水位」不能落成 0）
                        "max_qty": row.get("max_qty"),
                        "status": status_of(row),
                        "remark": row.get("remark"),
                    })
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| DbError::InvalidValue(format!("水位设置序列化失败：{error}")))?;

        let sql = format!(
            "INSERT INTO biz_material_stock_policy \
                 (id, material_id, warehouse_id, min_qty, max_qty, status, remark, \
                  created_at, updated_at, create_by, update_by) \
             SELECT v.id::uuid, m.id, w.id, v.min_qty::numeric, NULLIF(v.max_qty,'')::numeric, \
                    v.status, NULLIF(v.remark,''), \
                    CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, \
                    NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid \
               FROM jsonb_to_recordset(?::jsonb) AS v( \
                        id text, material_code text, warehouse_code text, \
                        min_qty text, max_qty text, status smallint, remark text) \
               JOIN biz_material m ON m.material_code = v.material_code AND m.deleted_at IS NULL \
               JOIN biz_warehouse w ON w.warehouse_code = v.warehouse_code AND w.deleted_at IS NULL \
             ON CONFLICT (material_id, warehouse_id) WHERE deleted_at IS NULL {}",
            conflict_clause(strategy)
        );
        let operator = ctx.operator.map(|id| id.to_string()).unwrap_or_default();
        let affected = tx
            .execute_with_params(
                &sql,
                vec![
                    Value::from(operator.clone()),
                    Value::from(operator),
                    Value::Json(payload),
                ],
            )
            .await?;
        if strategy == DuplicateStrategy::Upsert && affected as usize != rows.len() {
            // JOIN 把行滤掉只可能是「预检之后物料 / 仓库被删了」—— 静默返回成功会让用户
            // 以为整份文件都导进去了。整批回滚 + 说清下一步。
            return Err(DbError::InvalidValue(format!(
                "本批 {} 行中只有 {} 行落库：其余行的物料 / 仓库在预检后不存在了\
                 （可能刚被删除或停用），已整批回滚；请刷新物料与仓库后重新上传",
                rows.len(),
                affected
            )));
        }
        Ok(affected as usize)
    }
}

/// 读回本任务生效的重复数据策略（`sys_import_task.options.duplicate_strategy`）。
///
/// 与 `master_data::duplicate_strategy` 同口径但**各写一份**：那个函数是
/// `pub(super)` 的主数据私有工具（主数据口径是「编码即幂等键」），本模块的幂等键是
/// 「物料 × 仓库」组合。共享代码只能共享**口径相同**的部分，把不同口径硬拽进同一个函数
/// 才是隐患。
///
/// # 参数
///
/// - `ctx`：导入上下文（只用到 `task_id`）。
///
/// # 返回值
///
/// 解析出的策略；未设置 / 无法解析时按默认 `UPSERT`。
///
/// # 异常情况
///
/// 读库失败返回 [`DbError`]（宁可整批失败，也不猜一个策略去覆盖线上设置）。
async fn duplicate_strategy(ctx: &ImportCtx) -> std::result::Result<DuplicateStrategy, DbError> {
    let raw = crate::business::impexp::model::import_task_options(&ctx.task_id).await?;
    let parsed = raw
        .as_deref()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| {
            value
                .get("duplicate_strategy")
                .and_then(|item| item.as_str())
                .map(str::to_string)
        })
        .and_then(|text| DuplicateStrategy::parse(&text));
    Ok(parsed.unwrap_or_default())
}

/// 冲突处理子句：`UPSERT` 覆盖四列 / `SKIP` 整行不动。
///
/// # 参数
///
/// - `strategy`：本任务的重复数据策略。
fn conflict_clause(strategy: DuplicateStrategy) -> String {
    match strategy {
        DuplicateStrategy::Upsert => format!("DO UPDATE SET {UPSERT_SET}"),
        DuplicateStrategy::Skip => "DO NOTHING".to_string(),
    }
}

/// 状态列：空 → `1`（启用）。
///
/// 框架已把 `启用 / 停用` 翻成 `1` / `0`；这里兜的是「业务自己构造的行」（单测 / 修复脚本）。
fn status_of(row: &RowValues) -> i16 {
    row.get("status").parse::<i16>().unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 列契约稳定：6 列、前两列必填且参与组合唯一、后四列可选。
    #[test]
    fn column_contract_is_stable() {
        assert_eq!(COLUMNS.len(), 6);
        assert_eq!(COLUMNS[0].key, "material_code");
        assert_eq!(COLUMNS[1].key, "warehouse_code");
        assert!(COLUMNS[0].required && COLUMNS[1].required);
        assert!(
            COLUMNS[0].unique_key && COLUMNS[1].unique_key,
            "两列组合唯一 = 一格一行"
        );
        assert!(COLUMNS[2].required, "最低水位必填");
        for (index, column) in COLUMNS.iter().enumerate().skip(3) {
            assert!(!column.required, "第 {} 列（最高水位 / 状态 / 备注）可留空", index + 1);
        }
        for (index, column) in COLUMNS.iter().enumerate().skip(2) {
            assert!(!column.unique_key, "第 {} 列不该参与唯一键", index + 1);
        }
        // 物料 / 仓库都必须是「已存在才导」，不顺手建主数据
        assert_eq!(COLUMNS[0].fk.expect("物料外键").table, "biz_material");
        assert_eq!(COLUMNS[1].fk.expect("仓库外键").table, "biz_warehouse");
        println!("[M2-17] 导入列契约：物料编码 + 仓库编码 组合唯一，其余列可留空 ✓");
    }

    /// 业务编码与部署文档 / 前端 `biz-type` 逐字一致（改它 = 前端导入入口 404）。
    #[test]
    fn biz_type_and_limits_are_pinned() {
        let definition = StockPolicyImport;
        assert_eq!(definition.biz_type(), "business.erp.stock-policy");
        assert_eq!(STOCK_POLICY_BIZ_TYPE, definition.biz_type());
        assert_eq!(definition.biz_name(), "安全库存水位");
        assert_eq!(definition.max_rows(), MAX_ROWS);
        assert_eq!(
            definition.batch_size(),
            MAX_ROWS,
            "一份文件一个事务（STRICT 才是全有或全无）"
        );
        assert!(definition.allow_partial(), "向导里可选部分导入；默认仍是 STRICT");
        println!("[M2-17] biz_type = business.erp.stock-policy；batch_size == max_rows ✓");
    }

    /// 跨列规则：`max_qty` 填了就必须严格大于 `min_qty`；相等 / 小于 / 未填 三种刀口。
    #[tokio::test]
    async fn max_must_be_strictly_greater_than_min() {
        let definition = StockPolicyImport;
        let ctx = ImportCtx {
            task_id: uuid::Uuid::now_v7(),
            operator: None,
            mode: crate::business::impexp::definition::ImportMode::Strict,
        };

        let row = |min: &str, max: &str| {
            let mut row = RowValues {
                row_no: 2,
                ..Default::default()
            };
            row.fields.insert("material_code", "M1".to_string());
            row.fields.insert("warehouse_code", "W1".to_string());
            row.fields.insert("min_qty", min.to_string());
            row.fields.insert("max_qty", max.to_string());
            row
        };

        // 未填最高水位 = 不判超储 → 通过
        assert!(definition.validate_row(&row("100", ""), &ctx).await.is_ok());
        // 严格大于 → 通过（4 位小数的刀口：100.0001 > 100）
        assert!(definition.validate_row(&row("100", "100.0001"), &ctx).await.is_ok());
        // 相等 → 空区间，拒（并点名 max_qty 列 + Excel 行号）
        let issues = definition
            .validate_row(&row("100", "100"), &ctx)
            .await
            .expect_err("相等必须拒");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].row_no, 2);
        assert_eq!(issues[0].column_key, Some("max_qty"));
        assert_eq!(issues[0].kind, ErrorKind::Rule);
        assert!(issues[0].message.contains("严格大于"), "{}", issues[0].message);
        // 小于 → 拒
        assert!(definition.validate_row(&row("100", "99.9999"), &ctx).await.is_err());
        println!("[M2-17] 跨列规则：max 留空放行、max>min 放行、相等/小于 拒绝并点名 max_qty 列 ✓");
    }

    /// 冲突子句：UPSERT 覆盖四列、SKIP 整行不动；键列（物料 / 仓库）永远不在 `SET` 里。
    #[test]
    fn upsert_never_touches_key_columns() {
        let upsert = conflict_clause(DuplicateStrategy::Upsert);
        assert!(upsert.starts_with("DO UPDATE SET"));
        assert!(upsert.contains("min_qty = EXCLUDED.min_qty"));
        assert!(upsert.contains("max_qty = EXCLUDED.max_qty"));
        assert!(upsert.contains("status = EXCLUDED.status"));
        assert!(upsert.contains("remark = EXCLUDED.remark"));
        for key in ["material_id", "warehouse_id", "created_at", "id ="] {
            assert!(!UPSERT_SET.contains(key), "键列 / 创建时间不许被覆盖：{key}");
        }
        assert_eq!(conflict_clause(DuplicateStrategy::Skip), "DO NOTHING");
        println!("[M2-17] 冲突子句：UPSERT 只覆盖四列、SKIP 不动；键列不被改 ✓");
    }

    /// 状态列缺省 = 启用（`1`），与参数校验的 `status.unwrap_or(STATUS_ENABLED)` 同口径。
    #[test]
    fn status_defaults_to_enabled() {
        let mut row = RowValues::default();
        assert_eq!(status_of(&row), 1);
        row.fields.insert("status", "0".to_string());
        assert_eq!(status_of(&row), 0);
        println!("[M2-17] 状态缺省 = 启用（1）✓");
    }
}
