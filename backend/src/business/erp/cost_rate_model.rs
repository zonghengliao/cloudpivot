//! 成本费率的读库层（成本核算 · ARCH-9 §3.1 ⑥ / B2-3）。
//!
//! 只有**一个**查询：把某一类型的费率整批取回来（一次查库，不做 N+1），
//! 判定（优先级 / 生效期 / 状态 / 范围）全部交给纯函数
//! [`crate::business::erp::cost_rate_dto::resolve`] —— 那样每条规则都能单测，
//! 而且「导入 3000 行」与「单条录入」走的是同一套判定。
//!
//! # 为什么连停用 / 过期的行一起取
//!
//! 取数失败时（`66059`）要能回答「为什么没命中」：是根本没配、还是配了但停用了、
//! 还是生效期不覆盖这一天。只取「命中」的行就永远说不清，用户只能瞎试。
//! `biz_cost_rate` 是配置表（量级：车间数 × 版本数，通常几十行），整批读的代价可忽略；
//! 而「按 ? 逐行查」在导入 5000 行时就是 5000 次往返。
//!
//! # 本模块不做什么
//!
//! 不写 `biz_cost_rate`（设计 §5.2 / §5.3 没有费率的增删改接口与权限码），
//! 因此这里只有 `SELECT`。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_rate_dto::RateRow;

/// 表名。
pub const RATE_TABLE: &str = "biz_cost_rate";

/// 取数需要的列（与 [`RateRow`] 的字段一一对应）。
const RATE_COLUMNS: &str = "id, rate_type, scope, scope_id, rate, effective_from, effective_to, status";

impl RateRow {
    /// 由查询行构造（列名见 [`RATE_COLUMNS`]）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            rate_type: row.get_typed("rate_type")?,
            scope: row.get_typed("scope")?,
            scope_id: row.get_typed("scope_id")?,
            rate: row.get_typed("rate")?,
            effective_from: row.get_typed("effective_from")?,
            effective_to: row.get_typed("effective_to")?,
            status: row.get_typed("status")?,
        })
    }
}

/// 取某一费率类型的全部**未删除**行（启用的、停用的、生效期在各处的都在里面）。
///
/// 排序只是为了让日志 / 单测输出稳定（`effective_from DESC, id DESC`）；
/// 选哪一条由纯函数按优先级规则决定，**不依赖 SQL 顺序**。
///
/// # 异常情况
///
/// 查库失败 ⇒ `DbError`（调用方用 `utils::db_error` 翻成人话 500）。
pub async fn rates_of_type(rate_type: &str) -> DbResult<Vec<RateRow>> {
    let sql = format!(
        "SELECT {RATE_COLUMNS} FROM {RATE_TABLE} \
          WHERE rate_type = ? AND deleted_at IS NULL \
          ORDER BY effective_from DESC, id DESC"
    );
    let rows = db().query_sql(sql, vec![Value::Text(rate_type.to_string())]).await?;
    rows.iter().map(RateRow::from_row).collect()
}

/// 按主键取一行（未删除）—— 给出参回填 / 排查「这条费率是哪来的」用。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<RateRow>> {
    let sql = format!("SELECT {RATE_COLUMNS} FROM {RATE_TABLE} WHERE id = ?::uuid AND deleted_at IS NULL");
    let rows = db().query_sql(sql, vec![Value::from(&id)]).await?;
    rows.first().map(RateRow::from_row).transpose()
}
