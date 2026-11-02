// ============================================================================
// model_base.rs —— 底座：分页归一化、id 解析、通用分页/按 id 取数模板（保序与列清单无关的公共件）
//
// 由 `model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::shared::scope;

// ==================== 分页口径 ====================

/// 默认每页条数（与其余模块一致）。
pub const DEFAULT_PAGE_SIZE: u64 = 20;

/// 每页条数硬上限（防止 `page_size=100000` 把主数据表整表吐出去）。
pub const MAX_PAGE_SIZE: u64 = 200;

/// 归一化 `(page, page_size)`：页码从 1 起，`page_size = 0` 回落默认值，上限 [`MAX_PAGE_SIZE`]。
///
/// 放在 model 而不是 dto：这是**取数口径**，handler 不该各自 clamp 一遍。
pub fn normalize_page(page: u64, page_size: u64) -> (u64, u64) {
    let page = page.max(1);
    let size = if page_size == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        page_size.min(MAX_PAGE_SIZE)
    };
    (page, size)
}

/// UUID 数组参数（PG 的 `{a,b}` 文本字面量 + `?::text::uuid[]`）。
///
/// 驱动不接受 `Vec<Uuid>` 直接绑定，必须走文本数组（与 `task/model.rs` 同款）。
pub(super) fn ids_array_param(ids: &[Uuid]) -> Value {
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

/// DTO 里的 id 是 `String`（`Uuid` 没开 serde），落库前必须显式解析成 uuid。
///
/// 非法 UUID → `DbError::InvalidValue`（handler 转 400）：**不静默跳过条件**
/// ——跳过会让「按仓库筛库位」变成「返回全部库位」，是静默错数据。
pub(super) fn parse_id(value: &str) -> DbResult<Uuid> {
    Uuid::parse_str(value.trim())
        .map_err(|err| DbError::InvalidValue(format!("非法 UUID `{value}`：{err}")))
}

/// 分页取数的共用实现（13 个 `*_page` 都走这里）。
///
/// # 参数
///
/// - `target`：**必须**是 [`scope::ScopeTarget::unscoped`] 构造的锚点（本模块 13 张表都不参与数据权限）；
/// - `sql`：不含 `ORDER BY` / `LIMIT` 的基础查询（`WHERE …` 已存在）；
/// - `order_by`：`ORDER BY …` 原文（独立参数，见 `scope::paginate` 的契约）；
/// - `map`：`Row` → 强类型行的转换函数。
///
/// # 异常情况
///
/// 分页失败 / 行转换失败原样上抛 [`DbError`]。
pub(super) async fn page_of<T>(
    target: scope::ScopeTarget,
    sql: String,
    params: Vec<Value>,
    order_by: &str,
    page: u64,
    page_size: u64,
    map: fn(&Row) -> DbResult<T>,
) -> DbResult<(Vec<T>, i64)> {
    let (page, page_size) = normalize_page(page, page_size);
    let result =
        scope::paginate_unscoped(target, &sql, params, Some(order_by), page, page_size).await?;
    let rows = result.list.iter().map(map).collect::<DbResult<Vec<T>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 按主键取一行（13 个 `*_find_by_id` 的共用实现，软删过滤恒带）。
pub(super) async fn find_by_id<T>(
    table: &str,
    columns: &str,
    id: Uuid,
    soft_delete: bool,
    map: fn(&Row) -> DbResult<T>,
) -> DbResult<Option<T>> {
    let mut sql = format!("SELECT {columns} FROM {table} WHERE id = ?");
    if soft_delete {
        sql.push_str(" AND deleted_at IS NULL");
    }
    let rows = db().find_with_params(&sql, vec![Value::from(id)]).await?;
    rows.first().map(map).transpose()
}

// ==================== ① biz_unit 计量单位 ====================
