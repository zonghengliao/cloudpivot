//! 合同 **PDF 导出**（PDF-03）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/...`）
//!
//! | 方法 | 路径 | 权限码（复用单据查看权限） | 说明 |
//! |------|------|------------------------------|------|
//! | GET | `/business/erp/contract/{id}/pdf` | `business:erp:contract:list` | 合同 PDF（条款 + 签章位 + 附件清单） |
//!
//! # 为什么复用 `:list` 而不是新种 `:export`
//!
//! 导出是「看」的延伸：能看这张合同的人就能打印它。新种码要同步迁移 + 角色矩阵 +
//! 前端按钮三处，还会造出「详情看得见、点导出 403」的哑弹（先例：
//! `purchase-order/{id}/pending-items` 也挂 `:list`，PDF-02 同口径）。
//!
//! # 合同 PDF 的四个特殊要求（卡面）
//!
//! | 要求 | 落地方式 |
//! |------|----------|
//! | 正文（富文本 / 条款）完整渲染 | `biz_contract.remark` 是系统里唯一的条款正文字段，**全文**进 `pre-wrap` 容器（不截断、不去换行）；本卡不引入富文本列 —— 那要同步 CRUD、前端编辑器与迁移，属另一张卡 |
//! | 甲乙双方 / 金额 / 期限 / 付款方式 | 甲方取 `party_a_name`（空则公司名本体）、乙方取**快照** `party_b_name`（主档改名不回溯历史合同）；金额「小写 + 大写」同源；期限由 `start_date`/`end_date` 拼；付款方式从条款里识别「付款方式：…」（识别不到就如实说明，**不编值**，见 `template::PAYMENT_TERMS_EMPTY`） |
//! | 签字盖章位 | 甲方盖章 / 乙方盖章 / 日期（各一个空白手签位，整块不跨页） |
//! | 附件清单 | `biz_doc_attachment`（`doc_type='CONTRACT'`）JOIN `sys_file` 取原始文件名 / 大小 / 上传人 / 时间，按手动排序 |
//! | 骑缝章提示 | 签署页固定印一条「多页请加盖骑缝章」的条件指令 + 页脚每页再提示一次（页数只有 Chromium 打完才知道，见 `template::CONTRACT_SEAL_NOTE`） |
//!
//! # 审计
//!
//! 导出是 GET，中间件的操作日志不记（`AUDITED_METHODS` 只有写方法），
//! 渲染成功后由 [`doc_pdf_model::write_export_log`] 显式补一行
//! `operation='EXPORT_CONTRACT_PDF'`。写日志失败只打 error，不改响应。

use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::attachment::model::{self as attachment_model, DocKind, ListOrder};
use crate::business::erp::contract_model;
use crate::business::erp::doc_pdf_model::{self, ExportLogOwned};
use crate::business::erp::order_pdf_model;
use crate::business::pdf::template::{
    self, ContractAttachment, ContractContext, ContractContextArgs, OrderMetaField, format_size, payment_terms_of,
    term_range_of,
};
use crate::business::pdf::{engine, handler as pdf_handler};
use crate::utils;

/// 审计里的操作码。
pub const EXPORT_OPERATION: &str = "EXPORT_CONTRACT_PDF";

/// 审计里的目标表名。
pub const TARGET_TABLE: &str = "biz_contract";

/// 路由权限码（复用合同查看权限）。
pub const PERMISSION: &str = "business:erp:contract:list";

/// 合同类型中文名（`ck_biz_contract_type` 的四值域）。
pub fn contract_type_label(contract_type: &str) -> &'static str {
    match contract_type {
        "PURCHASE" => "采购合同",
        "SALES" => "销售合同",
        "SERVICE" => "服务合同",
        "LEASE" => "租赁合同",
        _ => "未知类型",
    }
}

/// 乙方类型中文名（`ck_biz_contract_party_b_type`）。
pub fn party_b_type_label(party_b_type: &str) -> &'static str {
    match party_b_type {
        "SUPPLIER" => "供应商",
        "CUSTOMER" => "客户",
        _ => "未知类型",
    }
}

/// 附件角色中文名（ATT-04 起 8 个值域，见 V0345）。
pub fn file_role_label(role: &str) -> &'static str {
    match role {
        "CONTRACT_SCAN" => "合同扫描件",
        "SUPPLEMENT" => "补充协议",
        "ACCEPTANCE" => "验收单",
        "INVOICE_SCAN" | "INVOICE" => "发票影像",
        "BANK_RECEIPT" => "银行回单",
        "SIGN_RECEIPT" => "签收单",
        "DEFAULT" | "OTHER" => "其他",
        _ => "其他",
    }
}

/// 合同的渲染上下文：真库取数 + 附件清单。
///
/// # 异常情况
///
/// - 404 [`crate::shared::error_code::RESOURCE_NOT_FOUND`]：合同不存在 / 已软删；
/// - 500：数据库异常。
pub async fn contract_context(id: Uuid) -> std::result::Result<ContractContext, ApiError> {
    let row = contract_model::contract_find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询合同"))?
        .ok_or_else(|| doc_pdf_model::not_found("合同"))?;

    // 附件清单（`doc_type='CONTRACT'`，手动排序优先）
    let files = attachment_model::list_by_doc(DocKind::Contract, &id, ListOrder::Manual)
        .await
        .map_err(|error| utils::db_error(error, "查询合同附件"))?;
    let attachments: Vec<ContractAttachment> = files
        .iter()
        .enumerate()
        .map(|(index, file)| ContractAttachment {
            seq: index as u32 + 1,
            file_name: file.file_name.clone(),
            role_label: file_role_label(&file.file_role).to_string(),
            size: format_size(file.file_size),
            uploader: if file.uploader_name.trim().is_empty() {
                "—".to_string()
            } else {
                file.uploader_name.clone()
            },
            uploaded_at: doc_pdf_model::beijing_time(&file.created_at),
        })
        .collect();

    // 负责人 / 制单人（一次批量补名）
    let people: Vec<Uuid> = row.owner_id.into_iter().chain(row.create_by).collect();
    let users = order_pdf_model::user_names_of(&people)
        .await
        .map_err(|error| utils::db_error(error, "查询合同经办人"))?;
    let owner = row
        .owner_id
        .and_then(|id| users.get(&id).cloned())
        .unwrap_or_else(|| "—".to_string());
    let creator = row
        .create_by
        .and_then(|id| users.get(&id).cloned())
        .unwrap_or_else(|| "—".to_string());

    // 甲方：合同上的快照名；为空表示「本组织本体」
    let party_a_name = row
        .party_a_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(template::COMPANY_NAME)
        .to_string();
    let party_a_rows = vec![OrderMetaField::new("名称", party_a_name.clone())];

    // 乙方：快照优先（历史合同不能因为主档改名就变样），主档现状仅在「与快照不同」时标注
    let party_b_name = row.party_b_name.clone();
    let mut party_b_rows = vec![
        OrderMetaField::new("名称", party_b_name.clone()),
        OrderMetaField::new("类型", party_b_type_label(&row.party_b_type)),
    ];
    match row.party_b_current_name.as_deref() {
        Some(current) if current.trim() != party_b_name.trim() => {
            party_b_rows.push(OrderMetaField::new(
                "主档现状",
                format!("{}（签约后已改名）", current.trim()),
            ));
        }
        None => party_b_rows.push(OrderMetaField::new("主档现状", "（主档已删除）")),
        _ => {}
    }

    let remark = row.remark.clone().unwrap_or_default();
    let term_range = term_range_of(
        row.start_date
            .map(|date| date.format("%Y-%m-%d").to_string())
            .as_deref(),
        row.end_date.map(|date| date.format("%Y-%m-%d").to_string()).as_deref(),
    );

    Ok(ContractContext::new(ContractContextArgs {
        doc_no: row.contract_no.clone(),
        contract_name: row.contract_name.clone(),
        meta: vec![
            OrderMetaField::new("合同号", row.contract_no.clone()),
            OrderMetaField::new("合同类型", contract_type_label(&row.contract_type)),
            OrderMetaField::new("状态", contract_model::status_label(&row.status)),
            OrderMetaField::new(
                "签订日期",
                row.sign_date
                    .map(|date| date.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "—".to_string()),
            ),
            OrderMetaField::new("负责人", owner),
            OrderMetaField::new("制单人", creator),
        ],
        party_a: party_a_rows,
        party_b: party_b_rows,
        party_a_name,
        party_b_name,
        currency_code: row.currency_code.clone(),
        amount: row.amount,
        term_range,
        payment_terms: payment_terms_of(Some(&remark)),
        body: remark,
        attachments,
        printed_at: doc_pdf_model::printed_at_now(),
    }))
}

/// 一次合同导出的结果（HTTP 层据此组响应头与审计行）。
#[derive(Debug, Clone)]
pub struct RenderedContract {
    /// PDF 字节
    pub bytes: Vec<u8>,
    /// 下载文件名
    pub filename: String,
    /// 合同号
    pub doc_no: String,
    /// 附件份数
    pub attachment_count: usize,
    /// 渲染耗时（毫秒）
    pub duration_ms: u64,
}

/// 渲染合同 PDF（**不含**审计与 HTTP 头，便于单测复用）。
///
/// # 异常情况
///
/// - 422 [`crate::shared::error_code::PDF_TEMPLATE_INVALID`]：模板渲染失败；
/// - 503 [`crate::shared::error_code::PDF_RENDER_UNAVAILABLE`]：Chromium 不可用；
/// - 504 [`crate::shared::error_code::PDF_RENDER_TIMEOUT`]：渲染超时。
pub async fn render_contract_pdf(context: &ContractContext) -> std::result::Result<RenderedContract, ApiError> {
    let html = template::render_contract(context).map_err(|error| error.into_api())?;
    let filename = format!("合同-{}.pdf", doc_pdf_model::sanitize_filename(&context.doc_no));
    let opts = template::contract_pdf_opts(&context.doc_no, Some(filename.clone()));
    let started = Instant::now();
    let bytes = engine::render_pdf(&html, &opts)
        .await
        .map_err(|error| error.into_api())?;
    let duration_ms = started.elapsed().as_millis() as u64;
    Ok(RenderedContract {
        bytes,
        filename,
        doc_no: context.doc_no.clone(),
        attachment_count: context.attachments.len(),
        duration_ms,
    })
}

/// 导出合同 PDF（取数 + 渲染，不含 HTTP 头 / 审计）。
///
/// # 异常情况
///
/// - 400 [`crate::shared::error_code::ERP_FILTER_INVALID`]：id 非法；
/// - 404：合同不存在；
/// - 422 / 503 / 504：同 [`render_contract_pdf`]。
pub async fn export_contract(id_raw: &str) -> std::result::Result<RenderedContract, ApiError> {
    let id = doc_pdf_model::parse_doc_id(id_raw)?;
    let context = contract_context(id).await?;
    render_contract_pdf(&context).await
}

/// 合同 PDF（`GET /api/business/erp/contract/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`crate::shared::error_code::PERMISSION_DENIED`]：缺 `business:erp:contract:list`；
/// - 400 / 404 / 422 / 503 / 504：见 [`export_contract`]。
#[nik::permission(
    path = "/business/erp/contract/{id}/pdf",
    method = "GET",
    permission = "business:erp:contract:list"
)]
#[nik::get("/business/erp/contract/{id}/pdf")]
pub async fn contract_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    let started = Instant::now();
    match export_contract(&id).await {
        Ok(rendered) => {
            let response = match pdf_handler::pdf_response(&rendered.bytes, Some(&rendered.filename)) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(
                        doc_no = %rendered.doc_no,
                        bytes = rendered.bytes.len(),
                        "合同 PDF 响应组装失败"
                    );
                    return error.into_response();
                }
            };
            audit(&headers, &connect_info, &claims, &id, &rendered, started);
            response
        }
        Err(error) => error.into_response(),
    }
}

/// 补一条导出审计（失败只记日志，不改变已生成的响应）。
fn audit(
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    id_raw: &str,
    rendered: &RenderedContract,
    started: Instant,
) {
    let Ok(doc_id) = doc_pdf_model::parse_doc_id(id_raw) else {
        return;
    };
    ExportLogOwned {
        user_id: utils::current_user_uuid(claims),
        username: claims.username.clone(),
        operation: EXPORT_OPERATION.to_string(),
        target_type: TARGET_TABLE.to_string(),
        doc_id,
        doc_no: rendered.doc_no.clone(),
        detail: serde_json::json!({
            "doc_type": "CONTRACT",
            "attachment_count": rendered.attachment_count,
            "render_ms": rendered.duration_ms,
        }),
        filename: rendered.filename.clone(),
        bytes: rendered.bytes.len(),
        duration_ms: started.elapsed().as_millis() as u64,
        request_path: format!("/api/business/erp/contract/{doc_id}/pdf"),
        client_ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        request_id: Uuid::now_v7(),
    }
    .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::pdf::template::sample_contract;

    /// 合同类型 / 乙方类型 / 附件角色 的中文名必须与迁移里的值域逐字对应。
    #[test]
    fn labels_match_migration_domains() {
        assert_eq!(contract_type_label("PURCHASE"), "采购合同");
        assert_eq!(contract_type_label("SALES"), "销售合同");
        assert_eq!(contract_type_label("SERVICE"), "服务合同");
        assert_eq!(contract_type_label("LEASE"), "租赁合同");
        assert_eq!(contract_type_label("WEIRD"), "未知类型");

        assert_eq!(party_b_type_label("SUPPLIER"), "供应商");
        assert_eq!(party_b_type_label("CUSTOMER"), "客户");

        // V0345 的 8 个值域一个不落
        for (raw, label) in [
            ("CONTRACT_SCAN", "合同扫描件"),
            ("SUPPLEMENT", "补充协议"),
            ("ACCEPTANCE", "验收单"),
            ("INVOICE_SCAN", "发票影像"),
            ("BANK_RECEIPT", "银行回单"),
            ("SIGN_RECEIPT", "签收单"),
            ("DEFAULT", "其他"),
            ("OTHER", "其他"),
        ] {
            assert_eq!(file_role_label(raw), label, "角色 {raw} 的中文名不对");
        }
    }

    /// 文件名只保留合同号（不带时间戳 / 行号），且危险字符被替换。
    #[test]
    fn filename_is_built_from_doc_no() {
        let ctx = sample_contract(0);
        let html = template::render_contract(&ctx).expect("渲染必须成功");
        assert!(html.contains(&ctx.doc_no));
        assert_eq!(doc_pdf_model::sanitize_filename("HT/2026:11*01"), "HT_2026_11_01");
    }

    /// 无权限路由必须挂 `:list` 码（不是新种的 `:export`）。
    #[test]
    fn permission_code_is_contract_list() {
        assert_eq!(PERMISSION, "business:erp:contract:list");
        assert_eq!(EXPORT_OPERATION, "EXPORT_CONTRACT_PDF");
        assert_eq!(TARGET_TABLE, "biz_contract");
    }
}

// ==================== 真库用例（ERP_DB_URL；未设置则 SKIP） ====================

/// 真库用例：**取数 → 上下文（含附件清单）→ 审计行**三段都在生产同一条路径上。
///
/// # 跑法
///
/// ```bash
/// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<测试库>'
/// cargo test --lib business::erp::contract_pdf -- --test-threads=1
/// ```
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::business::pdf::template::PAYMENT_TERMS_EMPTY;

    // ---------- 夹具 id（本卡独占命名空间 0f0300xx） ----------
    const CONTRACT: Uuid = Uuid::from_u128(0x0f03_0000_0000_4000_8000_0000_0000_0001);
    const OWNER: Uuid = Uuid::from_u128(0x0f03_0000_0000_4000_8000_0000_0000_0003);
    const FILE_A: Uuid = Uuid::from_u128(0x0f03_0000_0000_4000_8000_0000_0000_0004);
    const FILE_B: Uuid = Uuid::from_u128(0x0f03_0000_0000_4000_8000_0000_0000_0005);
    const ATTACH_A: Uuid = Uuid::from_u128(0x0f03_0000_0000_4000_8000_0000_0006);
    const ATTACH_B: Uuid = Uuid::from_u128(0x0f03_0000_0000_4000_8000_0000_0007);

    const CONTRACT_NO: &str = "PDF03-HT-000001";

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

    async fn exec(sql: &str, params: Vec<Value>, label: &str) {
        db().execute_with_params(sql, params)
            .await
            .unwrap_or_else(|error| panic!("{label}：{error}\nSQL: {sql}"));
    }

    /// 夹具清场（顺序：附件 → 文件 → 合同 → 主数据 → 审计行）。
    async fn purge() {
        for sql in [
            "DELETE FROM biz_doc_attachment WHERE doc_id = '0f030000-0000-4000-8000-000000000001'",
            "DELETE FROM sys_file WHERE id IN ('0f030000-0000-4000-8000-000000000004', \
                                              '0f030000-0000-4000-8000-000000000005')",
            "DELETE FROM operation_logs WHERE target_id = '0f030000-0000-4000-8000-000000000001'",
            "DELETE FROM biz_contract WHERE id = '0f030000-0000-4000-8000-000000000001'",
            "DELETE FROM biz_supplier WHERE id = '0f030000-0000-4000-8000-000000000002'",
            "DELETE FROM sys_user WHERE id = '0f030000-0000-4000-8000-000000000003'",
        ] {
            exec(sql, vec![], "清理 PDF-03 合同夹具").await;
        }
    }

    /// 造一张已签订的采购合同 + 两个附件（扫描件 / 补充协议）。
    async fn seed() {
        exec(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
             VALUES ('0f030000-0000-4000-8000-000000000002', 'PDF03-SUP', 'PDF03 测试供应商', 1)",
            vec![],
            "写入供应商夹具",
        )
        .await;
        exec(
            "INSERT INTO sys_user (id, username, password, real_name, status)
             VALUES ('0f030000-0000-4000-8000-000000000003', 'pdf03.owner', 'x', '合同负责人', 1)",
            vec![],
            "写入用户夹具",
        )
        .await;
        exec(
            "INSERT INTO biz_contract
                (id, contract_no, contract_name, contract_type, party_a_name, party_b_type,
                 party_b_id, party_b_name, amount, currency_code, sign_date, start_date, end_date,
                 status, owner_id, remark, create_by)
             VALUES ('0f030000-0000-4000-8000-000000000001', ?,
                     'PDF03 不锈钢管件年度采购合同', 'PURCHASE', '云枢（上海）科技有限公司',
                     'SUPPLIER', '0f030000-0000-4000-8000-000000000002', 'PDF03 测试供应商',
                     123456.7800, 'CNY', '2026-11-01', '2026-11-01', '2027-10-31', 'SIGNED',
                     '0f030000-0000-4000-8000-000000000003',
                     '第一条 标的物：不锈钢管件一批。\n第二条 付款方式：货到验收后 30 日内电汇付清。\n第三条 违约责任：逾期按 0.5‰ 计罚。',
                     '0f030000-0000-4000-8000-000000000003')",
            vec![Value::Text(CONTRACT_NO.to_string())],
            "写入合同夹具",
        )
        .await;
        for (file_id, name, size) in [
            (FILE_A, "PDF03-合同扫描件-第1页.pdf", 268_800i64),
            (FILE_B, "PDF03-补充协议.pdf", 92_160),
        ] {
            exec(
                "INSERT INTO sys_file (id, file_name, original_name, file_key, file_size, mime_type)
                 VALUES (?::uuid, ?, ?, ?, ?::text::int8, 'application/pdf')",
                vec![
                    Value::from(&file_id),
                    Value::Text(name.to_string()),
                    Value::Text(name.to_string()),
                    Value::Text(format!("pdf03/{file_id}")),
                    Value::Text(size.to_string()),
                ],
                "写入文件夹具",
            )
            .await;
        }
        for (id, file_id, role) in [
            (ATTACH_A, FILE_A, "CONTRACT_SCAN"),
            // 第二个附件用 SIGN_RECEIPT（签收单）：它在 V0212 的 4 值域与 V0345 的 8 值域里都存在，
            // 夹具因此在「老库 / 新库」两种约束下都能插入（不依赖迁移进度）
            (ATTACH_B, FILE_B, "SIGN_RECEIPT"),
        ] {
            exec(
                "INSERT INTO biz_doc_attachment (id, doc_type, doc_id, file_id, file_role, uploaded_by)
                 VALUES (?::uuid, 'CONTRACT', '0f030000-0000-4000-8000-000000000001'::uuid,
                         ?::uuid, ?, '0f030000-0000-4000-8000-000000000003'::uuid)",
                vec![Value::from(&id), Value::from(&file_id), Value::Text(role.to_string())],
                "写入附件夹具",
            )
            .await;
        }
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        purge().await;
        seed().await;
        Some(guard)
    }

    /// 验收①：合同上下文含签字盖章位所需信息 + 附件清单（文件名逐个对上）+ 正文全文。
    #[tokio::test]
    async fn contract_context_has_seal_block_and_attachments() {
        let Some(_guard) = setup().await else {
            return;
        };
        let ctx = contract_context(CONTRACT).await.expect("上下文必须组装成功");

        assert_eq!(ctx.doc_no, CONTRACT_NO);
        assert_eq!(ctx.contract_name, "PDF03 不锈钢管件年度采购合同");
        assert_eq!(ctx.amount, "123,456.78");
        assert_eq!(ctx.amount_cn, "壹拾贰万叁仟肆佰伍拾陆元柒角捌分");
        assert_eq!(ctx.term_range, "2026-11-01 至 2027-10-31");
        // 付款方式从条款里识别出来（不是兜底文案）
        assert_eq!(ctx.payment_terms, "货到验收后 30 日内电汇付清。");
        // 正文全文（三行一条不缺）
        for clause in ["第一条 标的物", "第二条 付款方式", "第三条 违约责任"] {
            assert!(ctx.body.contains(clause), "正文缺：{clause}");
        }
        assert!(ctx.has_body);

        // 附件清单（2 份，文件名 / 角色 / 大小 / 上传人齐）。
        // 顺序按 `sort_order ASC, created_at DESC, id DESC`（手动排序模式）——
        // 两个夹具 sort_order 相同且同一秒创建，所以顺序由 id 决定，断言不写死次序。
        assert_eq!(ctx.attachments.len(), 2);
        assert_eq!(ctx.attachment_count, "2");
        let by_name: std::collections::HashMap<&str, &crate::business::pdf::template::ContractAttachment> = ctx
            .attachments
            .iter()
            .map(|file| (file.file_name.as_str(), file))
            .collect();
        let scan = by_name
            .get("PDF03-合同扫描件-第1页.pdf")
            .expect("合同扫描件必须在附件清单里");
        assert_eq!(scan.role_label, "合同扫描件");
        assert_eq!(scan.size, "262.5 KB");
        assert_eq!(scan.uploader, "合同负责人");
        let receipt = by_name.get("PDF03-补充协议.pdf").expect("第二个附件必须在清单里");
        assert_eq!(receipt.role_label, "签收单");
        // 序号 1..n 连续
        let mut seqs: Vec<u32> = ctx.attachments.iter().map(|file| file.seq).collect();
        seqs.sort_unstable();
        assert_eq!(seqs, vec![1, 2]);

        // 甲乙双方
        let party_b: Vec<&str> = ctx.party_b_rows.iter().map(|field| field.value.as_str()).collect();
        assert!(party_b.contains(&"PDF03 测试供应商"), "乙方快照名必须在");

        // 渲染（不碰 chromium）：盖章位 / 附件清单 / 骑缝章 / 正文
        let html = template::render_contract(&ctx).expect("模板必须渲染成功");
        assert!(html.contains("甲方（盖章）"));
        assert!(html.contains("乙方（盖章）"));
        assert!(html.contains("PDF03-合同扫描件-第1页.pdf"));
        assert!(html.contains("骑缝章"));
        assert!(html.contains("第二条 付款方式"));
        assert!(html.contains("壹拾贰万叁仟肆佰伍拾陆元柒角捌分"));
        purge().await;
    }

    /// 备注里没有「付款方式」时打兜底文案（不编值）；软删合同 404。
    #[tokio::test]
    async fn context_without_payment_terms_and_soft_deleted() {
        let Some(_guard) = setup().await else {
            return;
        };
        exec(
            "UPDATE biz_contract SET remark = '本合同自双方签字盖章之日起生效'
              WHERE id = '0f030000-0000-4000-8000-000000000001'",
            vec![],
            "改写合同备注",
        )
        .await;
        let ctx = contract_context(CONTRACT).await.expect("上下文必须成功");
        assert_eq!(ctx.payment_terms, PAYMENT_TERMS_EMPTY);

        exec(
            "UPDATE biz_contract SET deleted_at = now()
              WHERE id = '0f030000-0000-4000-8000-000000000001'",
            vec![],
            "软删合同",
        )
        .await;
        let error = contract_context(CONTRACT).await.expect_err("软删合同必须 404");
        assert_eq!(error.status, 404);
        assert!(error.msg.contains("合同"));
        purge().await;
    }

    /// 审计：`operation_logs` 真的落下「谁导出了哪张合同」。
    #[tokio::test]
    async fn export_writes_operation_log_row() {
        let Some(_guard) = setup().await else {
            return;
        };
        let request_id = Uuid::now_v7();
        doc_pdf_model::write_export_log(&doc_pdf_model::ExportLog {
            user_id: Some(OWNER),
            username: Some("pdf03.owner"),
            operation: EXPORT_OPERATION,
            target_type: TARGET_TABLE,
            doc_id: CONTRACT,
            doc_no: CONTRACT_NO,
            detail: serde_json::json!({"doc_type": "CONTRACT", "attachment_count": 2}),
            filename: "合同-PDF03-HT-000001.pdf",
            bytes: 88_000,
            duration_ms: 240,
            request_path: "/api/business/erp/contract/0f030000-0000-4000-8000-000000000001/pdf",
            client_ip: Some(std::net::IpAddr::from([127, 0, 0, 1])),
            user_agent: Some("curl/8.5.0".to_string()),
            request_id,
        })
        .await
        .expect("审计写入必须成功");

        let rows = db()
            .find_with_params(
                "SELECT operation, target_type, target_id, username, detail, ip::text AS ip, \
                        request_method, status_code
                   FROM operation_logs WHERE target_id = ? AND request_id = ?",
                vec![Value::Text(CONTRACT.to_string()), Value::Text(request_id.to_string())],
            )
            .await
            .expect("回查审计行");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.get_typed::<String>("operation").unwrap(), "EXPORT_CONTRACT_PDF");
        assert_eq!(row.get_typed::<String>("target_type").unwrap(), "biz_contract");
        assert_eq!(row.get_typed::<String>("username").unwrap(), "pdf03.owner");
        assert_eq!(row.get_typed::<String>("request_method").unwrap(), "GET");
        assert_eq!(row.get_typed::<i32>("status_code").unwrap(), 200);
        let detail: serde_json::Value = row.get_typed("detail").expect("detail 必须是 jsonb");
        assert_eq!(detail["doc_type"], "CONTRACT");
        assert_eq!(detail["attachment_count"], 2);
        assert_eq!(detail["source"], "PDF-03");
        assert_eq!(detail["doc_no"], CONTRACT_NO);
        purge().await;
    }

    /// id 不是合法 UUID 时导出必须 400（在碰数据库之前就挡掉）。
    #[tokio::test]
    async fn export_rejects_bad_uuid_before_query() {
        if !ensure_db().await {
            return;
        }
        let error = export_contract("not-a-uuid").await.expect_err("必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, crate::shared::error_code::ERP_FILTER_INVALID);
    }
}
