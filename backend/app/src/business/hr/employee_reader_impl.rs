//! [`EmployeeReader`] 的 HR 侧实现：`biz_employee` 的跨域只读 SQL 出口（P2-35）。
//!
//! # 为什么 SQL 长在这里
//!
//! `biz_employee` 是 HR 的表（`V0383__hr_tables.up.sql`），**软删口径与列名口径都归本域**。
//! 消费方（`erp/cost/cost_labor_*`）原先各自写了一份 SQL，最典型的一次事故是：
//! 工号列被猜成 `employee_no`（真名是 `emp_no`），而当时表还没上线、语句永远走不到，
//! 「猜错也看不出来」；表一上线，任何填了工号的工时导入直接 500
//! （`cost_labor_model::employee_ids_by_no` 的注释里有完整记录）。
//! 把 SQL 收回到表的属主这里，是让这类错误**只有一处**可能发生。
//!
//! # 三条口径（与消费方原来的行为逐字对齐，P2-36 切换时才敢说「等价」）
//!
//! | 口径 | 值 | 原来写在哪 |
//! |---|---|---|
//! | 软删 | `deleted_at IS NULL` | `cost_labor_model::employee_ids_by_no` |
//! | 工号多行取法 | `DISTINCT ON (emp_no)` + `ORDER BY emp_no, id`（取最小 id，**确定性**） | 同上（注释写明「不加 DISTINCT 会随机映射到一行」） |
//! | 组织维度 | **不按 `org_id` 过滤** | 同上（工时导入这一层拿不到组织上下文；多组织落地前所有行都是 `hr::model::SENTINEL_ORG_ID`（全零），过滤与否结果相同 —— 但要按组织读就得先有组织，见下） |
//!
//! 组织维度的取舍要**写清楚**：多组织（ARCH-9-1）落地后，这个文件是唯一要加
//! `org_id = ?` 的地方；把过滤条件加在消费方等于把「越权读」的坑留在每个调用点上。
//!
//! # 批量纪律
//!
//! `IN (...)` 按 [`IN_CHUNK`] 切片（PG 单条语句参数上限 65535，500 足够且便于排查），
//! 入参**先去重**（导入 5000 行里同一个工号可能出现几十次），跨批结果再**排序一次**
//! 保证返回顺序与分片方式无关（调用方要求可复现，见 trait 注释）。

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::employee_reader::{self, EmployeeBrief, EmployeeReader};

/// 读的列（与 `V0383` 的 `biz_employee` 逐列对应；**不含敏感列**）。
pub const EMPLOYEE_READ_COLUMNS: &str = "id, emp_no, name, dept_id, status";

/// 主表名（探表 / 报错文案共用一处）。
pub const EMPLOYEE_TABLE: &str = "biz_employee";

/// `IN (...)` 单批最大元素数（PG 单条语句参数上限 65535，500 足够且便于排查）。
pub const IN_CHUNK: usize = 500;

/// 生成 `?,?,?`（与 `Value` 参数按序绑定）。
fn in_clause(size: usize) -> String {
    let mut out = String::with_capacity(size * 2);
    for index in 0..size {
        if index > 0 {
            out.push(',');
        }
        out.push('?');
    }
    out
}

/// 按 id 批量取的单批 SQL（纯函数，便于离线断言形状）。
fn ids_sql(size: usize) -> String {
    format!(
        "SELECT {EMPLOYEE_READ_COLUMNS} FROM {EMPLOYEE_TABLE} \
          WHERE deleted_at IS NULL AND id IN ({}) \
          ORDER BY emp_no, id",
        in_clause(size)
    )
}

/// 按工号批量取的单批 SQL（纯函数，便于离线断言形状）。
///
/// `DISTINCT ON (emp_no)` + `ORDER BY emp_no, id`：多组织下同一工号可能多行，
/// 取 id 最小者 —— **确定性**是导入校验的生命线（随机映射 = 每次导入算到不同的人）。
fn emp_nos_sql(size: usize) -> String {
    format!(
        "SELECT DISTINCT ON (emp_no) {EMPLOYEE_READ_COLUMNS} FROM {EMPLOYEE_TABLE} \
          WHERE deleted_at IS NULL AND emp_no IN ({}) \
          ORDER BY emp_no, id",
        in_clause(size)
    )
}

/// 「员工主档是否已建」的探针 SQL。
///
/// 用 `to_regclass` 而不是 `SELECT … FROM biz_employee`：表不存在时前者返回 NULL、
/// 后者抛 `42P01` 让整个探针 500 —— 而探针的用途恰恰是告诉运维「迁移没跑」。
fn present_sql() -> String {
    format!("SELECT (to_regclass('public.{EMPLOYEE_TABLE}') IS NOT NULL)::text AS present")
}

/// 一行 → 快照（列名与 [`EMPLOYEE_READ_COLUMNS`] 一一对应）。
fn brief_of(row: &Row) -> DbResult<EmployeeBrief> {
    Ok(EmployeeBrief {
        id: row.get_typed("id")?,
        emp_no: row.get_typed("emp_no")?,
        name: row.get_typed("name")?,
        dept_id: row.get_typed("dept_id")?,
        status: row.get_typed("status")?,
    })
}

/// 去重且**保持首次出现顺序**（`HashSet` 只做判重，输出顺序仍由入参决定）。
fn distinct<T: Clone + std::hash::Hash + Eq>(values: &[T]) -> Vec<T> {
    let mut seen = HashSet::with_capacity(values.len());
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        if seen.insert(value.clone()) {
            out.push(value.clone());
        }
    }
    out
}

/// HR 侧实现：真库读 `biz_employee`。
///
/// 无状态（连接来自进程级 `db()`），可以安全地以 `Arc<dyn EmployeeReader>` 共享。
pub struct HrEmployeeReader;

#[async_trait]
impl EmployeeReader for HrEmployeeReader {
    async fn employees_by_ids(&self, ids: &[Uuid]) -> DbResult<Vec<EmployeeBrief>> {
        // 去重：导入 5000 行里同一个员工会出现几十次，重复参数白占往返
        let ids: Vec<Uuid> = distinct(ids);
        if ids.is_empty() {
            // 空入参不查库：`IN ()` 是语法错误，而「没有要查的 id」本来就该直接返回空
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(IN_CHUNK) {
            let rows = db()
                .query_sql(ids_sql(chunk.len()), chunk.iter().map(Value::from).collect())
                .await?;
            for row in &rows {
                out.push(brief_of(row)?);
            }
        }
        sort_briefs(&mut out);
        Ok(out)
    }

    async fn employees_by_emp_nos(&self, emp_nos: &[String]) -> DbResult<Vec<EmployeeBrief>> {
        // trim + 去空 + 去重：Excel 里的工号常年带空格，空串不是工号
        let emp_nos: Vec<String> = distinct(
            &emp_nos
                .iter()
                .map(|no| no.trim().to_string())
                .filter(|no| !no.is_empty())
                .collect::<Vec<_>>(),
        );
        if emp_nos.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(emp_nos.len());
        for chunk in emp_nos.chunks(IN_CHUNK) {
            let rows = db()
                .query_sql(
                    emp_nos_sql(chunk.len()),
                    chunk.iter().map(|no| Value::Text(no.clone())).collect(),
                )
                .await?;
            for row in &rows {
                out.push(brief_of(row)?);
            }
        }
        sort_briefs(&mut out);
        Ok(out)
    }

    async fn is_available(&self) -> DbResult<bool> {
        let rows = db().query_sql(present_sql(), vec![]).await?;
        Ok(rows
            .first()
            .map(|row| row.get_typed::<String>("present"))
            .transpose()?
            .is_some_and(|value| value == "true"))
    }
}

/// 全序：`(emp_no, id)`。
///
/// 跨批结果必须重新排序 —— 否则「分片方式」会泄漏成「返回顺序」，
/// 调用方的快照 / 断言会随批量变了又变。
fn sort_briefs(briefs: &mut [EmployeeBrief]) {
    briefs.sort_by(|left, right| left.emp_no.cmp(&right.emp_no).then_with(|| left.id.cmp(&right.id)));
}

/// 启动期安装 HR 的实现（`bootstrap::register_hr_employee_reader` 调用）。
///
/// # 异常情况
///
/// 重复安装返回 `Err`（语义见 [`EmployeeRegistry::install`](super::employee_reader::EmployeeRegistry::install)）。
pub fn install() -> std::result::Result<(), String> {
    employee_reader::install(Arc::new(HrEmployeeReader))
}

// ==================== 单测（离线，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// SQL 形状：软删过滤 + 确定性全序 + 占位符个数与参数个数一致。
    ///
    /// 这四条是「等价性」的可离线证明部分（真库部分见 `employee_reader_db_tests`）：
    /// 少了 `deleted_at IS NULL` 会把离职删档的人捞回来；少了 `ORDER BY emp_no, id`
    /// 返回顺序就不确定；占位符与参数不对称会在第一行就报绑定错误。
    #[test]
    fn sql_shape_keeps_soft_delete_and_deterministic_order() {
        let by_ids = ids_sql(3);
        assert!(by_ids.contains("FROM biz_employee"), "{by_ids}");
        assert!(by_ids.contains("deleted_at IS NULL"), "{by_ids}");
        assert!(by_ids.contains("id IN (?,?,?)"), "{by_ids}");
        assert!(by_ids.ends_with("ORDER BY emp_no, id"), "{by_ids}");
        // 敏感列一律不读（跨域读不提供敏感面）
        assert!(!by_ids.contains("id_card"), "{by_ids}");
        assert!(!by_ids.contains("bank_account"), "{by_ids}");

        let by_nos = emp_nos_sql(2);
        assert!(by_nos.contains("DISTINCT ON (emp_no)"), "{by_nos}");
        assert!(by_nos.contains("emp_no IN (?,?)"), "{by_nos}");
        assert!(by_nos.ends_with("ORDER BY emp_no, id"), "{by_nos}");

        assert!(present_sql().contains("to_regclass('public.biz_employee')"));
        // 读的列与 V0383 对齐：工号列叫 emp_no（不是 employee_no —— ERP 侧踩过的坑）
        assert!(EMPLOYEE_READ_COLUMNS.contains("emp_no"), "{EMPLOYEE_READ_COLUMNS}");
        assert!(
            !EMPLOYEE_READ_COLUMNS.contains("employee_no"),
            "{EMPLOYEE_READ_COLUMNS}"
        );
    }

    /// 去重保持首次出现顺序（跨批排序要可复现，入参顺序是最稳的那一版）。
    #[test]
    fn distinct_keeps_first_occurrence_order() {
        let values = vec![3_u8, 1, 3, 2, 1];
        assert_eq!(distinct(&values), vec![3, 1, 2]);
        let empty: Vec<u8> = Vec::new();
        assert_eq!(distinct(&empty), Vec::<u8>::new());
    }

    /// 跨批结果按 `(emp_no, id)` 重排：返回顺序不许随分片方式变化。
    #[test]
    fn sort_briefs_is_a_total_order() {
        let brief = |emp_no: &str, seed: u128| EmployeeBrief {
            id: Uuid::from_u128(seed),
            emp_no: emp_no.to_string(),
            name: format!("员工{emp_no}"),
            dept_id: Uuid::from_u128(9),
            status: "ACTIVE".to_string(),
        };
        let mut rows = vec![brief("EMP-0002", 2), brief("EMP-0001", 3), brief("EMP-0001", 1)];
        sort_briefs(&mut rows);
        assert_eq!(
            rows.iter().map(|row| (row.emp_no.as_str(), row.id)).collect::<Vec<_>>(),
            vec![
                ("EMP-0001", Uuid::from_u128(1)),
                ("EMP-0001", Uuid::from_u128(3)),
                ("EMP-0002", Uuid::from_u128(2))
            ]
        );
    }
}
