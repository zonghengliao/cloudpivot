//! 单据附件数据层（`biz_doc_attachment`）。
//!
//! 只做三件事：**单据类型 → 表名映射 + 存在性校验**、**绑定关系读写**、**文件引用计数**。
//! 上传策略 / 秒传 / 对象写入 / 下载出流一律在 [`crate::business::storage`]，本文件不碰。
//!
//! # 五类单据的五张表
//!
//! | `doc_type` | 单据 | 表 | 前端详情页 |
//! |------------|------|----|-----------|
//! | `CONTRACT` | 合同 | `biz_contract` | `views/business/erp/contract` |
//! | `PURCHASE_ORDER` | 采购单 | `biz_purchase_order` | `views/business/erp/purchase` |
//! | `STOCK_IN` | 收货单 | `biz_stock_in` | `views/business/erp/stockin` |
//! | `PAYABLE` | 应付单 | `biz_payable` | `views/business/erp/payable` |
//! | `PAYMENT` | 付款单 | `biz_payment` | （应付详情里的付款记录） |
//!
//! 表名只来自本文件的常量白名单（[`DocKind::table`]），**不接受外部字符串拼 SQL** —— 这是
//! 「一张表存五类单据」这个设计的安全前提。
//!
//! # 为什么不建物理外键
//!
//! `doc_id` 要指向五张不同的表，单一列做不到。存在性由 [`doc_exists`] 在**写库之前**校验
//! （找不到 → 404，不会写出悬空附件）；`file_id` 反而**有**外键（同一张 `sys_file`），
//! 且是 `ON DELETE RESTRICT`：文件记录都不在了，绑定行就没有意义。
//!
//! # SCOPE-EXEMPT: 附件的可见性 = 单据的可见性
//!
//! 本文件的查询都发生在「调用方已经按单据做了业务判定」之后（handler 先校验单据存在、
//! 再由路由权限码 `business:attachment:list` 控制入口）。给附件再注入一次数据范围谓词
//! 会需要 join 五张单据表并各写一套范围规则，收益为零、静默少数据的风险反而更大。
//! 与 `biz_task_attachment`（`task/collab.rs` 文件头同款豁免）同一口径。

use nik::prelude::*;
use nik::uuid::Uuid;

/// 单据类型：合同。
pub const DOC_CONTRACT: &str = "CONTRACT";
/// 单据类型：采购订单。
pub const DOC_PURCHASE_ORDER: &str = "PURCHASE_ORDER";
/// 单据类型：收货入库单。
pub const DOC_STOCK_IN: &str = "STOCK_IN";
/// 单据类型：应付单。
pub const DOC_PAYABLE: &str = "PAYABLE";
/// 单据类型：付款登记单。
pub const DOC_PAYMENT: &str = "PAYMENT";

/// 文件角色：合同扫描件。
pub const ROLE_CONTRACT_SCAN: &str = "CONTRACT_SCAN";
/// 文件角色：签收单。
pub const ROLE_SIGN_RECEIPT: &str = "SIGN_RECEIPT";
/// 文件角色：发票影像。
pub const ROLE_INVOICE: &str = "INVOICE";
/// 文件角色：其他。
pub const ROLE_OTHER: &str = "OTHER";

/// 单张单据的附件上限（**读流之前**判定，避免大文件先读进内存再被拒）。
///
/// 取 50 与任务附件同口径：扫描件按「一份单据 3–10 张」估算，50 足够；上限的意义是
/// 防止误拖整个文件夹把单据变成图床。
pub const MAX_ATTACHMENTS_PER_DOC: i64 = 50;

/// 单据类型枚举（与迁移里的 `ck_biz_doc_attachment_type` 逐字对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// 合同 → `biz_contract`
    Contract,
    /// 采购订单 → `biz_purchase_order`
    PurchaseOrder,
    /// 收货入库单 → `biz_stock_in`
    StockIn,
    /// 应付单 → `biz_payable`
    Payable,
    /// 付款登记 → `biz_payment`
    Payment,
}

impl DocKind {
    /// 全部类型（顺序与前端下拉一致）。
    pub const ALL: [DocKind; 5] = [
        DocKind::Contract,
        DocKind::PurchaseOrder,
        DocKind::StockIn,
        DocKind::Payable,
        DocKind::Payment,
    ];

    /// 解析单据类型（大小写不敏感、去空白）。
    ///
    /// # 参数
    ///
    /// - `raw`：请求里的 `doc_type`。
    ///
    /// # 返回值
    ///
    /// 合法类型；未知类型返回 `None`（调用方报 400 + `PARAM_INVALID` 并列出可选值）。
    pub fn parse(raw: &str) -> Option<DocKind> {
        match raw.trim().to_ascii_uppercase().as_str() {
            DOC_CONTRACT => Some(DocKind::Contract),
            DOC_PURCHASE_ORDER => Some(DocKind::PurchaseOrder),
            DOC_STOCK_IN => Some(DocKind::StockIn),
            DOC_PAYABLE => Some(DocKind::Payable),
            DOC_PAYMENT => Some(DocKind::Payment),
            _ => None,
        }
    }

    /// 数据库 / 前端使用的字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            DocKind::Contract => DOC_CONTRACT,
            DocKind::PurchaseOrder => DOC_PURCHASE_ORDER,
            DocKind::StockIn => DOC_STOCK_IN,
            DocKind::Payable => DOC_PAYABLE,
            DocKind::Payment => DOC_PAYMENT,
        }
    }

    /// 中文标签（错误消息与审计用）。
    pub fn label(self) -> &'static str {
        match self {
            DocKind::Contract => "合同",
            DocKind::PurchaseOrder => "采购单",
            DocKind::StockIn => "收货单",
            DocKind::Payable => "应付单",
            DocKind::Payment => "付款单",
        }
    }

    /// 单据所在表（**唯一白名单**，禁止用外部字符串拼 SQL）。
    pub fn table(self) -> &'static str {
        match self {
            DocKind::Contract => "biz_contract",
            DocKind::PurchaseOrder => "biz_purchase_order",
            DocKind::StockIn => "biz_stock_in",
            DocKind::Payable => "biz_payable",
            DocKind::Payment => "biz_payment",
        }
    }

    /// 单据存在性 SQL（`deleted_at IS NULL` 都成立：五张表都是软删模型）。
    pub fn exists_sql(self) -> &'static str {
        match self {
            DocKind::Contract => {
                "SELECT 1 FROM biz_contract WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::PurchaseOrder => {
                "SELECT 1 FROM biz_purchase_order WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::StockIn => {
                "SELECT 1 FROM biz_stock_in WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::Payable => {
                "SELECT 1 FROM biz_payable WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::Payment => {
                "SELECT 1 FROM biz_payment WHERE id = ?::uuid AND deleted_at IS NULL"
            }
        }
    }
}

/// 文件角色枚举（与迁移里的 `ck_biz_doc_attachment_role` 逐字对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRole {
    /// 合同扫描件
    ContractScan,
    /// 签收单
    SignReceipt,
    /// 发票影像
    Invoice,
    /// 其他（缺省）
    Other,
}

impl FileRole {
    /// 全部角色（顺序与前端下拉一致）。
    pub const ALL: [FileRole; 4] = [
        FileRole::ContractScan,
        FileRole::SignReceipt,
        FileRole::Invoice,
        FileRole::Other,
    ];

    /// 解析角色：**空值回落 `OTHER`**，未知值返回 `None`。
    ///
    /// 空值回落是刻意的：`file_role` 是分类而非契约，前端老版本不带该字段时
    /// 不应该把上传挡在 400。
    pub fn parse(raw: Option<&str>) -> Option<FileRole> {
        let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
            return Some(FileRole::Other);
        };
        match text.to_ascii_uppercase().as_str() {
            ROLE_CONTRACT_SCAN => Some(FileRole::ContractScan),
            ROLE_SIGN_RECEIPT => Some(FileRole::SignReceipt),
            ROLE_INVOICE => Some(FileRole::Invoice),
            ROLE_OTHER => Some(FileRole::Other),
            _ => None,
        }
    }

    /// 数据库 / 前端使用的字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            FileRole::ContractScan => ROLE_CONTRACT_SCAN,
            FileRole::SignReceipt => ROLE_SIGN_RECEIPT,
            FileRole::Invoice => ROLE_INVOICE,
            FileRole::Other => ROLE_OTHER,
        }
    }

    /// 中文标签（列表里的 tag）。
    pub fn label(self) -> &'static str {
        match self {
            FileRole::ContractScan => "合同扫描件",
            FileRole::SignReceipt => "签收单",
            FileRole::Invoice => "发票影像",
            FileRole::Other => "其他",
        }
    }
}

/// 附件行（列表 / 详情共用）。
#[derive(Debug, Clone)]
pub struct DocAttachmentRow {
    /// 绑定行主键（删除 / 下载用的 `{id}`）。
    pub id: Uuid,
    /// 单据类型。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: Uuid,
    /// `sys_file.id`。
    pub file_id: Uuid,
    /// 文件角色。
    pub file_role: String,
    /// 备注。
    pub remark: Option<String>,
    /// 上传人。
    pub uploaded_by: Option<Uuid>,
    /// 上传人姓名（`COALESCE(real_name, username, '')`）。
    pub uploader_name: String,
    /// 显示用文件名（`original_name` 优先，回落对象键）。
    pub file_name: String,
    /// 字节数。
    pub file_size: i64,
    /// MIME。
    pub mime_type: Option<String>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 列表 / 详情要读的列（唯一来源，改列名只改这里）。
///
/// **`original_name` 与 `file_name` 分开取原始值**：显示名由 [`display_name`] 算。
/// 若直接 `COALESCE(original_name, file_name)`，存储路径（`2026/11/03/….png`）
/// 就会漏进前端当下载保存名（任务附件 E12 踩过同一个坑）。
const COLUMNS: &str = "a.id, a.doc_type, a.doc_id, a.file_id, a.file_role, a.remark, \
                       a.uploaded_by, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS uploader_name, \
                       COALESCE(f.original_name, '') AS original_name, \
                       COALESCE(f.file_name, '') AS stored_name, \
                       COALESCE(f.file_size, 0) AS file_size, f.mime_type, a.created_at";

/// 显示用文件名：原始名优先，为空时退回对象键的 basename（不含日期目录）。
///
/// # 参数
///
/// - `original`：`sys_file.original_name`；
/// - `stored`：`sys_file.file_name`（对象键，形如 `2026/11/03/<uuid>.png`）。
pub fn display_name(original: Option<&str>, stored: &str) -> String {
    if let Some(name) = original.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    stored.rsplit('/').next().unwrap_or(stored).to_string()
}

impl DocAttachmentRow {
    /// 从查询结果行构造。
    ///
    /// # 异常情况
    ///
    /// 列类型不匹配时返回 [`DbError`]（说明 SQL 与行结构漂移了）。
    fn from_row(row: &Row) -> DbResult<DocAttachmentRow> {
        let original: Option<String> = row.get_typed("original_name")?;
        let stored: String = row.get_typed("stored_name")?;
        Ok(DocAttachmentRow {
            id: row.get_typed("id")?,
            doc_type: row.get_typed("doc_type")?,
            doc_id: row.get_typed("doc_id")?,
            file_id: row.get_typed("file_id")?,
            file_role: row.get_typed("file_role")?,
            remark: row.get_typed("remark")?,
            uploaded_by: row.get_typed("uploaded_by")?,
            uploader_name: row.get_typed("uploader_name")?,
            file_name: display_name(original.as_deref(), &stored),
            file_size: row.get_typed("file_size")?,
            mime_type: row.get_typed("mime_type")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 单据是否存在（含软删判定）。
///
/// # 参数
///
/// - `kind`：单据类型（决定查哪张表）；
/// - `doc_id`：单据主键。
///
/// # 返回值
///
/// 存在且未软删返回 `true`。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn doc_exists(kind: DocKind, doc_id: &Uuid) -> DbResult<bool> {
    let rows = db()
        .find_with_params(kind.exists_sql(), vec![Value::from(doc_id)])
        .await?;
    Ok(!rows.is_empty())
}

/// 当前单据的未删附件数（配额判定；与 [`list_by_doc`] 同口径）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn count_by_doc(kind: DocKind, doc_id: &Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_doc_attachment \
          WHERE doc_type = ? AND doc_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::Text(kind.as_str().to_string()), Value::from(doc_id)],
    )
    .await
}

/// 同一份文件还被多少个**未删**绑定引用（软删 `sys_file` 之前的判据）。
///
/// 秒传（内容 hash 命中）会让多张单据共用一条 `sys_file`：删一张单据的附件时
/// 不能顺手把文件记录软删，否则别的单据的附件会变成「记录在、文件已删」。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn count_by_file(file_id: &Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_doc_attachment \
          WHERE file_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(file_id)],
    )
    .await
}

/// 绑定一行（幂等：同一张单据重复绑定同一份文件时返回既有行主键）。
///
/// # 参数
///
/// - `kind` / `doc_id`：单据；
/// - `file_id`：`sys_file.id`（调用方已确认该记录有效）；
/// - `role`：文件角色；
/// - `remark`：备注；
/// - `uploaded_by`：上传人。
///
/// # 返回值
///
/// 绑定行主键。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（外键失败说明 `file_id` 不存在，调用方应先校验）。
pub async fn insert(
    kind: DocKind,
    doc_id: &Uuid,
    file_id: &Uuid,
    role: FileRole,
    remark: Option<&str>,
    uploaded_by: Option<Uuid>,
) -> DbResult<Uuid> {
    let rows = db()
        .find_with_params(
            "INSERT INTO biz_doc_attachment \
                 (id, doc_type, doc_id, file_id, file_role, remark, uploaded_by, created_at) \
             VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?, ?, ?::uuid, now()) \
             ON CONFLICT (doc_type, doc_id, file_id) WHERE deleted_at IS NULL DO NOTHING \
             RETURNING id",
            vec![
                Value::from(&Uuid::now_v7()),
                Value::Text(kind.as_str().to_string()),
                Value::from(doc_id),
                Value::from(file_id),
                Value::Text(role.as_str().to_string()),
                remark.map_or(Value::Null, |text| Value::Text(text.to_string())),
                uploaded_by.map_or(Value::Null, |id| Value::from(&id)),
            ],
        )
        .await?;
    if let Some(row) = rows.first() {
        return row.get_typed("id");
    }

    // 已绑定过（并发或重复上传同一份字节）：返回既有行主键，不报错
    let existing = db()
        .find_with_params(
            "SELECT id FROM biz_doc_attachment \
              WHERE doc_type = ? AND doc_id = ?::uuid AND file_id = ?::uuid AND deleted_at IS NULL",
            vec![
                Value::Text(kind.as_str().to_string()),
                Value::from(doc_id),
                Value::from(file_id),
            ],
        )
        .await?;
    existing
        .first()
        .ok_or_else(|| DbError::InvalidValue("附件绑定冲突且查不到既有行".to_string()))?
        .get_typed("id")
}

/// 按单据列附件（未删，按上传时间升序）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn list_by_doc(kind: DocKind, doc_id: &Uuid) -> DbResult<Vec<DocAttachmentRow>> {
    let sql = format!(
        "SELECT {COLUMNS} \
           FROM biz_doc_attachment a \
           LEFT JOIN sys_file f ON f.id = a.file_id \
           LEFT JOIN sys_user u ON u.id = a.uploaded_by \
          WHERE a.doc_type = ? AND a.doc_id = ?::uuid AND a.deleted_at IS NULL \
          ORDER BY a.created_at ASC, a.id ASC"
    );
    let rows = db()
        .find_with_params(
            &sql,
            vec![Value::Text(kind.as_str().to_string()), Value::from(doc_id)],
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(DocAttachmentRow::from_row(row)?);
    }
    Ok(list)
}

/// 取单条未删附件（下载 / 删除前的归属校验）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn find_active(id: &Uuid) -> DbResult<Option<DocAttachmentRow>> {
    let sql = format!(
        "SELECT {COLUMNS} \
           FROM biz_doc_attachment a \
           LEFT JOIN sys_file f ON f.id = a.file_id \
           LEFT JOIN sys_user u ON u.id = a.uploaded_by \
          WHERE a.id = ?::uuid AND a.deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(id)]).await?;
    match rows.first() {
        Some(row) => Ok(Some(DocAttachmentRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 软删绑定行（只写 `deleted_at`：绑定关系是一次性事实，没有 `updated_at` / `update_by`）。
///
/// # 返回值
///
/// 影响行数（0 = 已经被别人删掉了，调用方按幂等处理）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn soft_delete(ex: &impl SqlExecutor, id: &Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_doc_attachment SET deleted_at = now() \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(id)],
    )
    .await
}
