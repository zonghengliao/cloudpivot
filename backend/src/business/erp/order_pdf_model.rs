//! 单据 PDF 导出的**数据访问层**（PDF-02：采购订单 / 销售订单）。
//!
//! # 边界
//!
//! 本文件只做三件事：① 按 id 批量解析**展示用**的名字（物料 / 单位 / 用户）；
//! ② 把「谁导出了哪张单」写进 `operation_logs`；③ 提供纯数据行结构给
//! [`crate::business::erp::order_pdf`] 组装模板上下文。**不含**权限判定，
//! 也**不含** PDF 渲染（那是 handler 与 `business/pdf` 的事）。
//!
//! # 为什么名字要批量查
//!
//! 一张 50 行明细的单据逐行查物料 / 单位就是 100 次数据库往返（打印一张单
//! 要几百毫秒的数据库时间），所以全部走 `ANY(?::text::uuid[])` 一次查完，
//! 与 `purchase_model::supplier_names_of` 同一套写法。
//!
//! # SCOPE-EXEMPT
//!
//! `biz_material` / `biz_unit` / `sys_user` 都是**全局主数据**（无 `dept_id` /
//! 归属人列），按主键批量解析名称不涉及数据范围；单据本身的可见性由导出接口的
//! 权限码控制（`business:erp:purchase-order:list` / `business:erp:sale-order:list`）。

use std::collections::HashMap;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::pdf::template::OrderDocKind;

/// 物料主档的展示字段（编码 / 名称 / 规格 / 默认计量单位）。
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialInfo {
    /// 物料编码（`biz_material.material_code`）
    pub code: String,
    /// 物料名称
    pub name: String,
    /// 规格型号（可为空串）
    pub spec: String,
    /// 默认计量单位（`biz_material.unit_id`）
    pub unit_id: Uuid,
}

/// `id::text::uuid[]` 的数组字面量（批量查询的入参形态）。
///
/// 与 `purchase_model::*_names_of` 同一写法：`Value::Text("{a,b,c}")` 配
/// `?::text::uuid[]`。**不用** `Value::Json`——那会被绑成 jsonb，`::uuid[]` 转不过去。
fn uuid_array(ids: &[Uuid]) -> Value {
    let joined = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    Value::Text(format!("{{{joined}}}"))
}

/// 批量取物料主档（`HashMap<物料 id, 展示字段>`）。
///
/// # 参数
///
/// - `ids`：物料 id 列表（空列表直接返回空映射，不发 SQL）。
///
/// # 返回值
///
/// 只包含**存在且未软删**的物料；查不到的行由调用方显示为「（物料已删除）」。
pub async fn materials_of(ids: &[Uuid]) -> DbResult<HashMap<Uuid, MaterialInfo>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT id, material_code, material_name, spec, unit_id FROM biz_material \
              WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL",
            vec![uuid_array(ids)],
        )
        .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        let id: Uuid = row.get_typed("id")?;
        let spec: Option<String> = row.get_typed("spec")?;
        map.insert(
            id,
            MaterialInfo {
                code: row.get_typed("material_code")?,
                name: row.get_typed("material_name")?,
                spec: spec.unwrap_or_default(),
                unit_id: row.get_typed("unit_id")?,
            },
        );
    }
    Ok(map)
}

/// 批量取计量单位名称（`HashMap<单位 id, unit_name>`）。
///
/// # 参数
///
/// - `ids`：单位 id 列表（空列表直接返回空映射）。
pub async fn unit_names_of(ids: &[Uuid]) -> DbResult<HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT id, unit_name FROM biz_unit WHERE id = ANY(?::text::uuid[])",
            vec![uuid_array(ids)],
        )
        .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        let id: Uuid = row.get_typed("id")?;
        let name: String = row.get_typed("unit_name")?;
        map.insert(id, name);
    }
    Ok(map)
}

/// 批量取用户显示名（`HashMap<user id, 姓名>`）。
///
/// # 口径
///
/// `real_name` 优先，空则回落登录账号；**不看** `deleted_at` / `status` ——
/// 历史单据上的经办人即使已离职 / 停用，打印出来也必须是当时的那个名字
/// （与 `sales_order_model::salesperson_labels` 同口径）。
pub async fn user_names_of(ids: &[Uuid]) -> DbResult<HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT id, COALESCE(NULLIF(real_name, ''), username) AS name FROM sys_user \
              WHERE id = ANY(?::text::uuid[])",
            vec![uuid_array(ids)],
        )
        .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        let id: Uuid = row.get_typed("id")?;
        let name: String = row.get_typed("name")?;
        map.insert(id, name);
    }
    Ok(map)
}

/// 一条导出审计（`operation_logs` 的一行）。
///
/// `Middleware` 的操作日志**只记 POST / PUT / PATCH / DELETE**
/// （`constants::AUDITED_METHODS`），导出是 `GET` —— 不显式写就**没有痕迹**。
/// 这张卡要求「记录谁导出了哪张单」，所以在这里补一行。
#[derive(Debug, Clone)]
pub struct ExportLog<'a> {
    /// 操作人（来自 `AuthClaims`，**不**取自请求参数）
    pub user_id: Option<Uuid>,
    /// 操作人账号（冗余列，日志页直接展示）
    pub username: Option<&'a str>,
    /// 单据种类
    pub kind: OrderDocKind,
    /// 单据主键
    pub doc_id: Uuid,
    /// 单号（检索 + 展示）
    pub doc_no: &'a str,
    /// 明细行数
    pub row_count: usize,
    /// 价税合计（元）
    pub total_with_tax: Decimal,
    /// 下载文件名
    pub filename: &'a str,
    /// PDF 字节数
    pub bytes: usize,
    /// 渲染耗时（毫秒）
    pub duration_ms: u64,
    /// 请求路径（`/api/business/erp/purchase-order/{id}/pdf` 的实际值）
    pub request_path: &'a str,
    /// 客户端 IP（可为空）
    pub client_ip: Option<std::net::IpAddr>,
    /// User-Agent（可为空）
    pub user_agent: Option<String>,
    /// 关联的请求追踪 id（与同一请求里其它日志可 join）
    pub request_id: Uuid,
}

/// 写一条「PDF 导出」审计行。
///
/// # 参数
///
/// - `entry`：见 [`ExportLog`]。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（由调用方决定是阻断还是只记日志）。
///
/// # 为什么 `detail` 是 jsonb 而不是文本
///
/// `operation_logs.detail` 是 `JSONB`，日志页与导出脚本都按 JSON 取值；
/// 绑参必须用 `Value::Json`（`Value::Text` 绑 `?::jsonb` 会序列化失败）。
pub async fn write_export_log(entry: &ExportLog<'_>) -> DbResult<()> {
    let detail = serde_json::json!({
        "doc_type": entry.kind.doc_kind_str(),
        "doc_id": entry.doc_id.to_string(),
        "doc_no": entry.doc_no,
        "row_count": entry.row_count,
        "total_with_tax": entry.total_with_tax.round_dp(2).to_string(),
        "filename": entry.filename,
        "bytes": entry.bytes,
        "duration_ms": entry.duration_ms,
        "source": "PDF-02",
    })
    .to_string();
    db().execute_with_params(
        "INSERT INTO operation_logs \
             (id, request_id, user_id, username, operation, target_type, target_id, detail, \
              ip, user_agent, duration_ms, request_path, request_method, status_code, created_at) \
         VALUES (?::uuid, ?, ?::uuid, ?, ?, ?, ?, ?::jsonb, ?::text::inet, ?, ?::text::int4, ?, 'GET', 200, now())",
        vec![
            Value::from(&Uuid::now_v7()),
            Value::Text(entry.request_id.to_string()),
            entry
                .user_id
                .as_ref()
                .map(Value::from)
                .unwrap_or(Value::Null),
            entry
                .username
                .map(|name| Value::Text(name.to_string()))
                .unwrap_or(Value::Null),
            Value::Text(operation_code(entry.kind).to_string()),
            Value::Text(entry.kind.table().to_string()),
            Value::Text(entry.doc_id.to_string()),
            Value::Json(detail),
            entry
                .client_ip
                .map(|ip| Value::Text(ip.to_string()))
                .unwrap_or(Value::Null),
            entry
                .user_agent
                .as_ref()
                .map(|text| Value::Text(text.clone()))
                .unwrap_or(Value::Null),
            Value::Text(entry.duration_ms.to_string()),
            Value::Text(entry.request_path.to_string()),
        ],
    )
    .await?;
    Ok(())
}

/// 审计里的操作码（`operation_logs.operation`，日志页按它筛「谁导出了 PDF」）。
pub fn operation_code(kind: OrderDocKind) -> &'static str {
    match kind {
        OrderDocKind::Purchase => "EXPORT_PURCHASE_ORDER_PDF",
        OrderDocKind::Sales => "EXPORT_SALES_ORDER_PDF",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 操作码必须与单据种类一一对应（日志页按码检索，拼错就等于查不到）。
    #[test]
    fn operation_codes_are_distinct() {
        assert_eq!(
            operation_code(OrderDocKind::Purchase),
            "EXPORT_PURCHASE_ORDER_PDF"
        );
        assert_eq!(
            operation_code(OrderDocKind::Sales),
            "EXPORT_SALES_ORDER_PDF"
        );
        assert_ne!(
            operation_code(OrderDocKind::Purchase),
            operation_code(OrderDocKind::Sales)
        );
    }

    /// 空 id 列表不发 SQL（打印 0 行单据时不该有一次多余往返）。
    #[tokio::test]
    async fn empty_ids_short_circuit() {
        assert!(
            materials_of(&[])
                .await
                .expect("空列表必须直接返回")
                .is_empty()
        );
        assert!(
            unit_names_of(&[])
                .await
                .expect("空列表必须直接返回")
                .is_empty()
        );
        assert!(
            user_names_of(&[])
                .await
                .expect("空列表必须直接返回")
                .is_empty()
        );
    }

    /// 数组字面量必须是 PG 认的 `{a,b}` 形态（写错就是 `invalid input syntax`）。
    #[test]
    fn uuid_array_literal_shape() {
        let id = Uuid::nil();
        match uuid_array(&[id]) {
            Value::Text(text) => {
                assert!(text.starts_with('{') && text.ends_with('}'));
                assert_eq!(text, format!("{{{id}}}"));
            }
            other => panic!("必须是 Text：{other:?}"),
        }
    }
}
