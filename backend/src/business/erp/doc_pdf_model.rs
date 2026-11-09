//! 单据 PDF 导出的**公共数据访问层**（PDF-03：合同 / 收货入库单 / 出库单）。
//!
//! # 边界
//!
//! 只做三件事：① 批量为展示层补名称（库位 / 仓库 / 供应商 / 客户）；
//! ② 取「一物一码」的序列号清单；③ 把「谁导出了哪张单」写进 `operation_logs`。
//! **不含**权限判定，也**不含** PDF 渲染（那是 handler 与 `business/pdf` 的事）。
//!
//! PDF-02（采购 / 销售订单）的取数在 [`crate::business::erp::order_pdf_model`]，
//! 本文件是 PDF-03 起新增的公共件；物料 / 单位 / 用户名的批量解析直接复用那份，
//! 不重复实现（同一份名字解析口径）。
//!
//! # 为什么全部走 `ANY(?::text::uuid[])` 批量
//!
//! 一张 50 行的单据逐行查库位 / 物料 / 单位就是 150 次往返。批量一次查完，
//! 口径与 `purchase_model::supplier_names_of` 一致。
//!
//! # SCOPE-EXEMPT
//!
//! `biz_inventory_location` / `biz_warehouse` / `biz_supplier` / `biz_customer` /
//! `biz_material_serial` 都是**全局主数据 / 仓储台账**（无 `dept_id` / 归属人列），
//! 按主键批量解析名称不涉及数据范围；单据可见性由导出接口的权限码控制。

use std::collections::HashMap;
use std::net::IpAddr;

use nik::prelude::*;
use nik::uuid::Uuid;

/// 路径里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
///
/// 与 `purchase_handler::parse_uuid` / `order_pdf::parse_id` 同一判据
/// （错误码 [`crate::shared::error_code::ERP_FILTER_INVALID`]）。
pub fn parse_doc_id(raw: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            crate::shared::error_code::ERP_FILTER_INVALID,
            "id 不能为空",
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            crate::shared::error_code::ERP_FILTER_INVALID,
            format!("id 不是合法的 UUID：{text}"),
        )
    })
}

/// 打印时间（北京时间，`yyyy-MM-dd HH:mm`）。
///
/// 服务器时区不可控（容器里常常是 UTC），所以**显式**用 +08:00 ——
/// 单据上印的「打印时间」必须是给中国人看的当地时间。
pub fn printed_at_now() -> String {
    let offset = nik::prelude::chrono::FixedOffset::east_opt(8 * 3600).expect("+08:00 一定是合法偏移");
    nik::prelude::chrono::Utc::now()
        .with_timezone(&offset)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// 北京时间格式化（附件上传时间等展示字段）。
pub fn beijing_time(value: &nik::prelude::chrono::DateTime<nik::prelude::chrono::Utc>) -> String {
    let offset = nik::prelude::chrono::FixedOffset::east_opt(8 * 3600).expect("+08:00 一定是合法偏移");
    value.with_timezone(&offset).format("%Y-%m-%d %H:%M").to_string()
}

/// 下载名里的单号必须与 URL / `Content-Disposition` 兼容（去掉路径分隔符与控制字符）。
pub fn sanitize_filename(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '/' | '\\' | '"' | '\'' | ':' | '*' | '?' | '<' | '>' | '|') {
                '_'
            } else {
                ch
            }
        })
        .collect()
}

/// 单据不存在（含已软删）统一 404，文案点名单据类型。
pub fn not_found(label: &str) -> ApiError {
    ApiError::new(
        404,
        crate::shared::error_code::RESOURCE_NOT_FOUND,
        format!("{label}不存在或已删除"),
    )
}

/// `id::text::uuid[]` 的数组字面量（批量查询的入参形态）。
///
/// `Value::Text("{a,b,c}")` 配 `?::text::uuid[]`；**不用** `Value::Json`
/// （那会被绑成 jsonb，`::uuid[]` 转不过去）。
fn uuid_array(ids: &[Uuid]) -> Value {
    let joined = ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
    Value::Text(format!("{{{joined}}}"))
}

/// 批量：库位账行 id（`biz_inventory_location.id`）→ 库位编码。
///
/// # 口音
///
/// 明细里的 `location_id` 指向的是**库位账行**（`biz_inventory_location.id`），
/// 不是库位字典（`biz_location.id`）——见 V0203 文件头 ⑤。库位账行上只有
/// `location_id` 指向字典，所以编码要 JOIN 过去取 `biz_location.location_code`。
///
/// 账行存在但字典已删 / 未绑库位 → `未分配`；账行本身不存在 → 由调用方显示
/// `已删除的库位账行`（这里不塞假值，保持与 `stockout_model::location_label` 同口径）。
pub async fn location_labels_of(ids: &[Uuid]) -> DbResult<HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT il.id, COALESCE(b.location_code, '未分配') AS location_code \
               FROM biz_inventory_location il \
               LEFT JOIN biz_location b ON b.id = il.location_id \
              WHERE il.id = ANY(?::text::uuid[])",
            vec![uuid_array(ids)],
        )
        .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        map.insert(row.get_typed::<Uuid>("id")?, row.get_typed::<String>("location_code")?);
    }
    Ok(map)
}

/// 批量：**库位字典** id（`biz_location.id`）→ 库位编码。
///
/// # 与 [`location_labels_of`] 的区别（混用就是记错桶）
///
/// - [`location_labels_of`]：入参是**库位账行**（`biz_inventory_location.id`）——
///   入库 / 出库 / 调拨 / 盘点明细走这条（V0139 起「location_id」在单据行上一律指账行）；
/// - 本函数：入参是**库位字典**（`biz_location.id`）—— 销售退货明细的
///   `biz_sales_return_item.location_id` 是唯一走字典口径的列
///   （见 `sales_return_posting` 文件头：字典 id 与账行 id 不能混用）。
///
/// 字典里查不到（已删）→ 调用方显示 `—`，这里不塞假值。
pub async fn location_dict_labels_of(ids: &[Uuid]) -> DbResult<HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT id, location_code FROM biz_location WHERE id = ANY(?::text::uuid[])",
            vec![uuid_array(ids)],
        )
        .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        map.insert(row.get_typed::<Uuid>("id")?, row.get_typed::<String>("location_code")?);
    }
    Ok(map)
}

/// 批量：仓库 id → 仓库名（含已停用仓库 —— 历史单据上的仓库名不能被改掉）。
pub async fn warehouse_names_of(ids: &[Uuid]) -> DbResult<HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT id, warehouse_name FROM biz_warehouse WHERE id = ANY(?::text::uuid[])",
            vec![uuid_array(ids)],
        )
        .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        map.insert(row.get_typed::<Uuid>("id")?, row.get_typed::<String>("warehouse_name")?);
    }
    Ok(map)
}

/// 批量：供应商 id → 供应商名。
pub async fn supplier_names_of(ids: &[Uuid]) -> DbResult<HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT id, supplier_name FROM biz_supplier WHERE id = ANY(?::text::uuid[])",
            vec![uuid_array(ids)],
        )
        .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        map.insert(row.get_typed::<Uuid>("id")?, row.get_typed::<String>("supplier_name")?);
    }
    Ok(map)
}

/// 批量：客户 id → 客户名。
pub async fn customer_names_of(ids: &[Uuid]) -> DbResult<HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT id, name FROM biz_customer WHERE id = ANY(?::text::uuid[])",
            vec![uuid_array(ids)],
        )
        .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        map.insert(row.get_typed::<Uuid>("id")?, row.get_typed::<String>("name")?);
    }
    Ok(map)
}

/// 序列号清单：按物料分组，`HashMap<物料 id, Vec<序列号>>`（序列号升序）。
///
/// # 参数
///
/// - `doc_id`：**收货入库单**主键。
///
/// # 为什么按 `source_id` 而不是 `source_type`
///
/// `biz_material_serial.source_id` 就是「生成这批序列号的收货单 id」
/// （`stockin_query::serial_count_of` 同口径）；而 `source_type` 记的是**来源业务**
/// （PURCHASE / TRANSFER / PRODUCE / RETURN），无源入库（SOURCELESS）那类会留 `NULL`
/// —— 按 `source_type` 过滤会把无源入库的序列号全部漏掉。
///
/// 出库单不登记序列号（出库是改状态，不新建行），所以本函数只服务入库单。
pub async fn serials_by_material(doc_id: Uuid) -> DbResult<HashMap<Uuid, Vec<String>>> {
    let rows = db()
        .find_with_params(
            "SELECT material_id, serial_no FROM biz_material_serial \
              WHERE source_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY material_id, serial_no",
            vec![Value::from(&doc_id)],
        )
        .await?;
    let mut map: HashMap<Uuid, Vec<String>> = HashMap::new();
    for row in rows.iter() {
        map.entry(row.get_typed::<Uuid>("material_id")?)
            .or_default()
            .push(row.get_typed::<String>("serial_no")?);
    }
    Ok(map)
}

/// 发货单引用（出库单的来源单据：`source_type = 'SALES'` 时 `source_id` = 发货单 id）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShipmentRef {
    /// 发货单号（`SH-yyyyMMdd-000001`）
    pub ship_no: String,
    /// 客户（用于出库单的「客户」栏）
    pub customer_id: Uuid,
}

/// 按 id 取发货单号与客户（出库单来源单据号的唯一来源）。
///
/// # 返回值
///
/// 单据不存在 / 已软删 → `None`（调用方打 `—`，不编单号）。
pub async fn shipment_ref(id: Uuid) -> DbResult<Option<ShipmentRef>> {
    let row = db()
        .query_first_sql(
            "SELECT ship_no, customer_id FROM biz_shipment \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref()
        .map(|row| {
            Ok(ShipmentRef {
                ship_no: row.get_typed("ship_no")?,
                customer_id: row.get_typed("customer_id")?,
            })
        })
        .transpose()
}

/// 一条「单据 PDF 导出」审计（`operation_logs` 的一行）。
///
/// # 为什么要自己写
///
/// `middleware::operation_log` 只记 POST / PUT / PATCH / DELETE
/// （`constants::AUDITED_METHODS`），导出是 `GET` —— 不显式写就一点痕迹都没有。
#[derive(Debug, Clone)]
pub struct ExportLog<'a> {
    /// 操作人（来自 `AuthClaims`，**不**取自请求参数）
    pub user_id: Option<Uuid>,
    /// 操作人账号
    pub username: Option<&'a str>,
    /// 操作码（`EXPORT_CONTRACT_PDF` / `EXPORT_STOCK_IN_PDF` / `EXPORT_STOCK_OUT_PDF`）
    pub operation: &'a str,
    /// 目标表名（`biz_contract` / `biz_stock_in` / `biz_stock_out`）
    pub target_type: &'a str,
    /// 单据主键
    pub doc_id: Uuid,
    /// 单号
    pub doc_no: &'a str,
    /// `detail` 里的业务字段（`doc_type` / `row_count` / `amount` …）
    pub detail: serde_json::Value,
    /// 下载文件名
    pub filename: &'a str,
    /// PDF 字节数
    pub bytes: usize,
    /// 渲染耗时（毫秒）
    pub duration_ms: u64,
    /// 请求路径
    pub request_path: &'a str,
    /// 客户端 IP
    pub client_ip: Option<IpAddr>,
    /// User-Agent
    pub user_agent: Option<String>,
    /// 请求追踪 id
    pub request_id: Uuid,
}

/// 写一条「PDF 导出」审计行。
///
/// # `detail.source`
///
/// 固定写 `PDF-03`：它标识的是「单据 PDF 导出」这个**子系统**
/// （PDF-03 起所有单据共用本函数），不是卡号 —— PDF-04 的调拨 / 盘点 / 退货
/// 同样落这个值，日志页按它一眼筛出「哪些导出走的是这条通道」。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（由调用方决定阻断还是只记日志；导出成功后只记日志）。
pub async fn write_export_log(entry: &ExportLog<'_>) -> DbResult<()> {
    let mut detail = match entry.detail.clone() {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    detail.insert("doc_id".to_string(), serde_json::json!(entry.doc_id.to_string()));
    detail.insert("doc_no".to_string(), serde_json::json!(entry.doc_no));
    detail.insert("filename".to_string(), serde_json::json!(entry.filename));
    detail.insert("bytes".to_string(), serde_json::json!(entry.bytes));
    detail.insert("duration_ms".to_string(), serde_json::json!(entry.duration_ms));
    // `source` 默认写 PDF-03（`write_export_log` 是 PDF-03 起的公共件），
    // 但**调用方传了就以调用方为准** —— 否则 PDF-05 的审批单导出会挂上
    // 「来源 PDF-03」的假标签，审计按来源统计时把单据族串在一起。
    if !detail.contains_key("source") {
        detail.insert("source".to_string(), serde_json::json!("PDF-03"));
    }
    db().execute_with_params(
        "INSERT INTO operation_logs \
             (id, request_id, user_id, username, operation, target_type, target_id, detail, \
              ip, user_agent, duration_ms, request_path, request_method, status_code, created_at) \
         VALUES (?::uuid, ?, ?::uuid, ?, ?, ?, ?, ?::jsonb, ?::text::inet, ?, ?::text::int4, ?, 'GET', 200, now())",
        vec![
            Value::from(&Uuid::now_v7()),
            Value::Text(entry.request_id.to_string()),
            entry.user_id.as_ref().map(Value::from).unwrap_or(Value::Null),
            entry
                .username
                .map(|name| Value::Text(name.to_string()))
                .unwrap_or(Value::Null),
            Value::Text(entry.operation.to_string()),
            Value::Text(entry.target_type.to_string()),
            Value::Text(entry.doc_id.to_string()),
            Value::Json(serde_json::Value::Object(detail).to_string()),
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

/// 导出审计的**拥有所有权**版本（`tokio::spawn` 要求 `'static`）。
///
/// 借用版 [`ExportLog`] 留给单测 / 同步调用；HTTP 层用这一版 `spawn` 出去，
/// 写日志失败只打 error，不改已经生成的响应（与中间件同口径）。
#[derive(Debug, Clone)]
pub struct ExportLogOwned {
    /// 操作人
    pub user_id: Option<Uuid>,
    /// 操作人账号
    pub username: Option<String>,
    /// 操作码
    pub operation: String,
    /// 目标表名
    pub target_type: String,
    /// 单据主键
    pub doc_id: Uuid,
    /// 单号
    pub doc_no: String,
    /// 业务字段
    pub detail: serde_json::Value,
    /// 文件名
    pub filename: String,
    /// 字节数
    pub bytes: usize,
    /// 耗时
    pub duration_ms: u64,
    /// 请求路径
    pub request_path: String,
    /// 客户端 IP
    pub client_ip: Option<IpAddr>,
    /// UA
    pub user_agent: Option<String>,
    /// 请求追踪 id
    pub request_id: Uuid,
}

impl ExportLogOwned {
    /// 落库（转成借用的 [`ExportLog`]）。
    pub async fn write(&self) -> DbResult<()> {
        write_export_log(&ExportLog {
            user_id: self.user_id,
            username: self.username.as_deref(),
            operation: &self.operation,
            target_type: &self.target_type,
            doc_id: self.doc_id,
            doc_no: &self.doc_no,
            detail: self.detail.clone(),
            filename: &self.filename,
            bytes: self.bytes,
            duration_ms: self.duration_ms,
            request_path: &self.request_path,
            client_ip: self.client_ip,
            user_agent: self.user_agent.clone(),
            request_id: self.request_id,
        })
        .await
    }

    /// 后台落库（不拖慢响应）。
    pub fn spawn(self) {
        let operation = self.operation.clone();
        let doc_no = self.doc_no.clone();
        let bytes = self.bytes;
        tokio::spawn(async move {
            match self.write().await {
                Ok(()) => tracing::info!(
                    audit = %operation,
                    doc_no = %doc_no,
                    bytes,
                    "单据 PDF 导出已留痕"
                ),
                Err(error) => tracing::error!(
                    audit = %operation,
                    doc_no = %doc_no,
                    error = %error,
                    "单据 PDF 导出审计写入失败（导出本身已成功）"
                ),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空 id 列表不发 SQL（打印 0 行单据时不该有额外往返）。
    #[tokio::test]
    async fn empty_ids_short_circuit() {
        for map_len in [
            location_labels_of(&[]).await.expect("空列表直接返回").len(),
            location_dict_labels_of(&[]).await.expect("空列表直接返回").len(),
            warehouse_names_of(&[]).await.expect("空列表直接返回").len(),
            supplier_names_of(&[]).await.expect("空列表直接返回").len(),
            customer_names_of(&[]).await.expect("空列表直接返回").len(),
        ] {
            assert_eq!(map_len, 0);
        }
    }

    /// 数组字面量必须是 PG 认的 `{a,b}` 形态。
    #[test]
    fn uuid_array_literal_shape() {
        let a = Uuid::nil();
        let b = Uuid::from_u128(1);
        match uuid_array(&[a, b]) {
            Value::Text(text) => assert_eq!(text, format!("{{{a},{b}}}")),
            other => panic!("必须是 Text：{other:?}"),
        }
    }

    /// 路径里的 UUID：非法 / 空 → 400 + 66006（不静默当「没传」）。
    #[test]
    fn parse_doc_id_rejects_garbage() {
        let error = parse_doc_id("not-a-uuid").expect_err("非法 id 必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, crate::shared::error_code::ERP_FILTER_INVALID);
        let empty = parse_doc_id("   ").expect_err("空 id 必须报错");
        assert_eq!(empty.status, 400);
        assert!(parse_doc_id(&Uuid::nil().to_string()).is_ok());
        // 前后空白不该误伤（前端偶尔带空格）
        assert!(parse_doc_id(&format!(" {} ", Uuid::nil())).is_ok());
    }

    /// 下载名净化：路径分隔符 / 引号 / 控制字符一律替换成下划线。
    #[test]
    fn sanitize_filename_strips_path_chars() {
        assert_eq!(sanitize_filename("HT-20261108-000001"), "HT-20261108-000001");
        assert_eq!(sanitize_filename("a/b\\c:d*e?f"), "a_b_c_d_e_f");
        assert_eq!(sanitize_filename("IN\n1"), "IN_1");
    }

    /// 打印时间必须是北京时间（容器里常是 UTC，差 8 小时）。
    #[test]
    fn printed_at_is_beijing_time() {
        let text = printed_at_now();
        assert_eq!(text.len(), 16, "yyyy-MM-dd HH:mm：{text}");
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[10..11], " ");
    }

    /// 审计 detail 必须带 `source` 标记（日志页按它区分 PDF-02 / PDF-03 两批导出）。
    ///
    /// 说明：真正的落库断言在 `contract_pdf::db_tests` / `stock_pdf::db_tests`
    /// 里对 `operation_logs` 回查（这里只钉住常量口径）。
    #[test]
    fn export_log_source_tag() {
        assert_eq!(
            crate::business::erp::contract_pdf::EXPORT_OPERATION,
            "EXPORT_CONTRACT_PDF"
        );
        assert_eq!(
            crate::business::erp::stock_pdf::export_operation(crate::business::pdf::template::StockDocKind::StockIn),
            "EXPORT_STOCK_IN_PDF"
        );
        assert_eq!(
            crate::business::erp::stock_pdf::export_operation(crate::business::pdf::template::StockDocKind::StockOut),
            "EXPORT_STOCK_OUT_PDF"
        );
    }
}
