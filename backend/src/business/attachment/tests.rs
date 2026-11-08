// ============================================================================
// tests.rs —— 业务单据附件用例（ARCH-3 DC-17）
//
// 两类用例：
//   · **纯逻辑**（不连库）：单据类型 / 文件角色白名单、备注上限、行级删除判据、
//     显示名口径、权限码与迁移逐字一致；
//   · **真库**（`ERP_DB_URL` 开关，与其它 ERP 用例同口径）：五类单据的存在性映射、
//     绑定幂等、列表顺序、删绑定 / 共享文件保留、软删单据拒绝挂附件。
//
// ⚠ `ERP_DB_URL` 未设置时真库用例全部 SKIP（假绿）。交付前必须真跑：
//
// ```bash
// ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//   cargo test --lib business::attachment -- --test-threads=1
// ```
//
// # 夹具口径
//
// | 对象 | 前缀 / UUID |
// |------|-------------|
// | 五类单据 | `att_` 单号前缀 + `d2120x` UUID |
// | `sys_file` | `d2120fxx` UUID（**直接插行**，不写物理对象：本文件验的是绑定关系与 SQL） |
// | 用户 | `att_` 前缀 + `d212090x` UUID |
//
// **串行**（`--test-threads=1`）：清理按前缀删，并行会互删夹具。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use super::dto;
use super::model::{self, DocKind, FileRole};

// ---------- 纯逻辑 ----------

/// 夹具用户（附件上传人）。
const U_OWNER: &str = "d2120901-0000-4000-8000-000000000001";
/// 夹具用户（另一个人：用来验「删别人附件」被拒）。
const U_OTHER: &str = "d2120902-0000-4000-8000-000000000002";

/// 合同夹具。
const DOC_CONTRACT: &str = "d2120101-0000-4000-8000-000000000001";
/// 采购单夹具。
const DOC_PO: &str = "d2120102-0000-4000-8000-000000000001";
/// 收货单夹具。
const DOC_IN: &str = "d2120103-0000-4000-8000-000000000001";
/// 应付单夹具。
const DOC_PAYABLE: &str = "d2120104-0000-4000-8000-000000000001";
/// 付款单夹具。
const DOC_PAYMENT: &str = "d2120105-0000-4000-8000-000000000001";
/// 供应商夹具。
const SUPPLIER: &str = "d2120200-0000-4000-8000-000000000001";
/// 仓库夹具。
const WAREHOUSE: &str = "d2120200-0000-4000-8000-000000000002";

/// 两个 `sys_file` 夹具（不同内容哈希，避免撞 `uk_sys_file_hash`）。
const FILE_A: &str = "d2120f01-0000-4000-8000-000000000001";
const FILE_B: &str = "d2120f02-0000-4000-8000-000000000002";
/// 第三个文件（ATT-03 排序用例需要「同一张单据 3 个附件」才能验出顺序）。
const FILE_C: &str = "d2120f03-0000-4000-8000-000000000003";

fn uid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

fn claims_of(user: &str, permissions: &[&str]) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some(format!("att_{}", &user[..8])),
        roles: vec!["user".to_string()],
        permissions: permissions.iter().map(|code| (*code).to_string()).collect(),
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
}

/// 七类单据 → 七张表，一一对应且不重复（FIN-3 加了报销单）。
#[test]
fn doc_kinds_cover_five_distinct_tables() {
    assert_eq!(DocKind::ALL.len(), 7);
    let mut tables: Vec<&str> = DocKind::ALL.iter().map(|kind| kind.table()).collect();
    tables.sort_unstable();
    tables.dedup();
    assert_eq!(
        tables.len(),
        7,
        "七类单据必须落在七张不同的表上：{tables:?}"
    );
    for kind in DocKind::ALL {
        // 字面值与 DDL 的 ck_biz_doc_attachment_type 逐字一致
        assert_eq!(
            DocKind::parse(kind.as_str()),
            Some(kind),
            "{} 的字面值无法回解析",
            kind.as_str()
        );
        assert!(kind.exists_sql().contains(kind.table()));
        assert!(!kind.label().is_empty());
    }
    // 大小写不敏感 + 去空白（前端传小写也能用）
    assert_eq!(DocKind::parse(" contract "), Some(DocKind::Contract));
    assert_eq!(DocKind::parse("stock_in"), Some(DocKind::StockIn));
    // 未知类型一律 None（绝不做「默认当合同」这种猜测）
    assert_eq!(DocKind::parse("INVOICE"), None);
    assert_eq!(DocKind::parse(""), None);
}

/// 文件角色：四个白名单值 + 空值回落 OTHER + 未知值拒绝。
#[test]
fn file_roles_parse_and_default() {
    assert_eq!(FileRole::ALL.len(), 4);
    for role in FileRole::ALL {
        assert_eq!(FileRole::parse(Some(role.as_str())), Some(role));
        assert!(!role.label().is_empty());
    }
    assert_eq!(FileRole::parse(None), Some(FileRole::Other));
    assert_eq!(FileRole::parse(Some("   ")), Some(FileRole::Other));
    assert_eq!(FileRole::parse(Some("invoice")), Some(FileRole::Invoice));
    // 典型的「写错」：CONTRACT_SCANNED / SIGN 都不能被猜成 SCAN / RECEIPT
    assert_eq!(FileRole::parse(Some("CONTRACT_SCANNED")), None);
    assert_eq!(FileRole::parse(Some("SIGN")), None);
}

/// 上传元信息：doc_id 必须是 UUID，缺字段直接 400（不是 500）。
#[test]
fn upload_meta_rejects_missing_or_bad_ids() {
    let error =
        dto::parse_upload_meta(Some("CONTRACT"), None, None, None).expect_err("缺 doc_id 必须被拒");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("doc_id"), "{}", error.msg);

    let error = dto::parse_upload_meta(Some("CONTRACT"), Some("not-a-uuid"), None, None)
        .expect_err("非法 UUID 必须被拒");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("不是合法 UUID"), "{}", error.msg);

    let error = dto::parse_upload_meta(Some("CONTRACTS"), Some(DOC_CONTRACT), None, None)
        .expect_err("未知 doc_type 必须被拒");
    assert_eq!(error.status, 400);
    // 消息里必须列出可选值，前端写错时能直接看到正确值
    assert!(error.msg.contains("PURCHASE_ORDER"), "{}", error.msg);

    let meta = dto::parse_upload_meta(
        Some("purchase_order"),
        Some(DOC_PO),
        Some("sign_receipt"),
        Some("  签收单  "),
    )
    .expect("合法入参必须通过");
    assert_eq!(meta.doc_type, DocKind::PurchaseOrder);
    assert_eq!(meta.file_role, FileRole::SignReceipt);
    assert_eq!(meta.remark.as_deref(), Some("签收单"));
    // 空备注归一为 None（列可空，不用空串占位）
    let meta = dto::parse_upload_meta(Some("CONTRACT"), Some(DOC_CONTRACT), None, Some("  "))
        .expect("空备注必须放行");
    assert!(meta.remark.is_none());
}

/// 备注上限：200 字符（中文按字符数，不是字节数）。
#[test]
fn upload_meta_rejects_long_remark() {
    let ok = "附".repeat(dto::REMARK_MAX_CHARS);
    assert!(dto::parse_upload_meta(Some("CONTRACT"), Some(DOC_CONTRACT), None, Some(&ok)).is_ok());
    let too_long = "附".repeat(dto::REMARK_MAX_CHARS + 1);
    let error = dto::parse_upload_meta(Some("CONTRACT"), Some(DOC_CONTRACT), None, Some(&too_long))
        .expect_err("超长备注必须被拒");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("200"), "{}", error.msg);
}

/// 行级删除判据：上传人本人可删；别人必须持 `business:attachment:delete`。
#[test]
fn can_delete_is_owner_or_privileged() {
    let owner = claims_of(U_OWNER, &[]);
    let stranger = claims_of(U_OTHER, &[]);
    let auditor = claims_of(U_OTHER, &["business:attachment:delete"]);
    let wildcard = claims_of(U_OTHER, &["*"]);

    assert!(dto::can_delete(&owner, Some(uid(U_OWNER))));
    assert!(!dto::can_delete(&stranger, Some(uid(U_OWNER))));
    assert!(dto::can_delete(&auditor, Some(uid(U_OWNER))));
    // 通配权限（SUPER_ADMIN）走框架的 has_permission 短路
    assert!(dto::can_delete(&wildcard, Some(uid(U_OWNER))));
    // 无属主的历史行：只有升权码能删
    assert!(!dto::can_delete(&owner, None));
    assert!(dto::can_delete(&auditor, None));
}

/// 显示名：原始名优先；原始名为空时只取对象键的最后一段（不把日期目录漏给前端）。
#[test]
fn display_name_never_leaks_object_key() {
    assert_eq!(
        model::display_name(Some("合同扫描件.pdf"), "2026/11/03/abc.pdf"),
        "合同扫描件.pdf"
    );
    assert_eq!(
        model::display_name(Some("   "), "2026/11/03/abc.pdf"),
        "abc.pdf"
    );
    assert_eq!(model::display_name(None, "2026/11/03/abc.pdf"), "abc.pdf");
    assert_eq!(model::display_name(None, ""), "");
}

/// 权限码清单必须与 `V0212` 迁移逐字一致（改一处忘另一处 → 这里先红）。
#[test]
fn permission_codes_match_migration() {
    let codes = super::handler::PERMISSIONS;
    assert_eq!(
        codes,
        [
            "business:attachment:upload",
            "business:attachment:list",
            "business:attachment:delete"
        ]
    );
    // 生成 SQL 里出现的三个码也必须是这三个
    for code in codes {
        assert!(code.starts_with("business:attachment:"), "{code}");
    }
}

/// 单据不存在时的错误口径：404 + 中文里点明是哪类单据。
#[test]
fn doc_not_found_message_names_the_kind() {
    let error = dto::doc_not_found(DocKind::StockIn);
    assert_eq!(error.status, 404);
    assert!(error.msg.contains("收货单"), "{}", error.msg);
    let error = dto::attachment_not_found();
    assert_eq!(error.status, 404);
    assert!(error.msg.contains("附件"), "{}", error.msg);
}

// ---------- ATT-02：附件下载权限（纯逻辑） ----------

/// ATT-02 ①：单据类型 → 模块读码的映射必须与各 ERP 模块的真实读码逐字一致。
///
/// 这张表是 `dto::can_view_doc` 的唯一依据，改一个字母就会让「有权限的人 403」或
/// 「没权限的人放行」，所以在这里钉死。库侧一致性由真库用例
/// [`view_permissions_are_real_menu_codes`] 再兜一层（防哑弹）。
#[test]
fn view_permissions_map_to_erp_read_codes() {
    let expected: [(DocKind, &[&str]); 7] = [
        (DocKind::Contract, &["business:erp:contract:list"]),
        (
            DocKind::PurchaseOrder,
            &["business:erp:purchase-order:list"],
        ),
        (
            DocKind::StockIn,
            &["business:erp:stock-in:list", "business:erp:stock-in:detail"],
        ),
        (
            DocKind::Payable,
            &["business:erp:payable:list", "business:erp:payable:detail"],
        ),
        // 付款单没有独立菜单：可见性同应付单（付款记录挂在应付详情里）
        (
            DocKind::Payment,
            &["business:erp:payable:list", "business:erp:payable:detail"],
        ),
        (
            DocKind::FixedAsset,
            &["business:erp:asset:list", "business:erp:asset:detail"],
        ),
        (
            DocKind::ExpenseReport,
            &["business:erp:expense:list", "business:erp:expense:detail"],
        ),
    ];
    assert_eq!(expected.len(), DocKind::ALL.len(), "七类单据必须全覆盖");
    for (kind, codes) in expected {
        assert_eq!(
            kind.view_permissions(),
            codes,
            "{} 的模块读码漂移了",
            kind.label()
        );
        for code in codes {
            // 形态 `business:erp:{资源}:{动作}`（模块前缀本身带冒号），且全小写
            assert!(
                code.starts_with("business:erp:") && code.split(':').count() >= 4,
                "权限码形态不对：{code}（{}）",
                kind.label()
            );
            assert_eq!(*code, code.to_ascii_lowercase(), "权限码必须小写：{code}");
        }
    }
    println!("[ATT-02] 七类单据 → 模块读码一一对应 ✓");
}

/// ATT-02 ②：`can_view_doc` 的真值表 —— 只认「该模块的读码」。
///
/// | 持有的码 | 期望 |
/// |---|---|
/// | 该模块 `:list` | 放行 |
/// | 该模块 `:detail` | 放行（有的模块详情另有码） |
/// | 别的模块的 `:list` | **拒绝**（仓库主管读应付单附件 = 越权） |
/// | 只有 `business:attachment:*` | **拒绝**（附件码 ≠ 单据码） |
/// | 空权限 | 拒绝 |
/// | `*`（SUPER_ADMIN） | 全放行（框架短路） |
#[test]
fn can_view_doc_truth_table() {
    let payable_list = claims_of(U_OTHER, &["business:erp:payable:list"]);
    let payable_detail = claims_of(U_OTHER, &["business:erp:payable:detail"]);
    let stock_in = claims_of(U_OTHER, &["business:erp:stock-in:list"]);
    let attachment_only = claims_of(
        U_OTHER,
        &[
            "business:attachment:list",
            "business:attachment:upload",
            "business:attachment:delete",
        ],
    );
    let nobody = claims_of(U_OTHER, &[]);
    let wildcard = claims_of(U_OTHER, &["*"]);

    assert!(dto::can_view_doc(&payable_list, DocKind::Payable));
    assert!(dto::can_view_doc(&payable_detail, DocKind::Payable));
    // 付款单同应付单口径
    assert!(dto::can_view_doc(&payable_list, DocKind::Payment));

    // 有别的模块的读码 ≠ 有应付单的读码（ATT-02 的核心）
    assert!(
        !dto::can_view_doc(&stock_in, DocKind::Payable),
        "收货单读码不能放行应付单附件"
    );
    assert!(dto::can_view_doc(&stock_in, DocKind::StockIn));
    assert!(
        !dto::can_view_doc(&attachment_only, DocKind::Payable),
        "只有附件码（能用附件功能）不等于能看这张单"
    );
    for kind in DocKind::ALL {
        assert!(
            !dto::can_view_doc(&attachment_only, kind),
            "{}",
            kind.label()
        );
        assert!(!dto::can_view_doc(&nobody, kind), "{}", kind.label());
        // 超管短路（框架的 has_any_permission）
        assert!(dto::can_view_doc(&wildcard, kind), "{}", kind.label());
    }
    println!("[ATT-02] can_view_doc 真值表：本模块读码→放行；他模块/仅附件码→拒；超管→放行 ✓");
}

/// ATT-02 ③：403 的口径 —— 状态码 / 错误码 / 点名缺失权限 / 不泄露附件事实。
#[test]
fn doc_forbidden_names_the_missing_permission() {
    let error = dto::doc_forbidden(DocKind::Payable);
    assert_eq!(error.status, 403);
    assert_eq!(error.code, crate::shared::error_code::PERMISSION_DENIED);
    assert!(error.msg.contains("应付单"), "{}", error.msg);
    assert!(
        error.msg.contains("business:erp:payable:list"),
        "必须点名缺哪个码：{}",
        error.msg
    );
    // 消息由「单据类型」推导，不含任何附件行的事实（文件名 / 大小 / 备注）
    for leaked in ["签收单.pdf", "合同扫描件.png", "1024", "附件不存在"] {
        assert!(
            !error.msg.contains(leaked),
            "403 消息不得含附件行的事实（{leaked}）：{}",
            error.msg
        );
    }
    // 每类单据都能生成 403（不留「某类单据没配码 → 空消息」的坑）
    for kind in DocKind::ALL {
        let error = dto::doc_forbidden(kind);
        assert_eq!(error.status, 403, "{}", kind.label());
        assert!(error.msg.contains(kind.label()), "{}", error.msg);
        // 至少点名一个可申请的权限码，且列表不出现空段（`… / ）`）
        assert!(
            error.msg.contains(kind.view_permissions()[0]),
            "{} 必须点名读码：{}",
            kind.label(),
            error.msg
        );
        assert!(!error.msg.contains("/ ）"), "{}", error.msg);
        assert!(!error.msg.contains("/ )"), "{}", error.msg);
    }
    println!("[ATT-02] 403 + 30001 + 点名权限码，且不泄露附件事实 ✓");
}

/// ATT-02 ④：下载留痕的字段（纯逻辑，不连库）。
///
/// 「谁下载了什么」必须一行读得懂：操作码 / 操作人 / 目标 = 附件绑定行 /
/// `detail` 里带单据类型 + 单据 id + 文件 id + 文件名 + 字节数。
#[test]
fn download_log_fields() {
    use axum::http::HeaderMap;

    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", "req-att02-1".parse().expect("头值"));
    headers.insert("user-agent", "att02-test/1.0".parse().expect("头值"));
    headers.insert("x-real-ip", "10.9.8.7".parse().expect("头值"));
    let connect: std::net::SocketAddr = "127.0.0.1:54321".parse().expect("地址");
    let claims = claims_of(U_OWNER, &["business:erp:payable:list"]);
    let row = fixture_row();

    let entry =
        super::handler::download_log(&headers, &connect, &claims, &row, DocKind::Payable, 7);

    assert_eq!(entry.operation, super::handler::OP_DOWNLOAD);
    assert_eq!(entry.target_type.as_deref(), Some("DocAttachment"));
    assert_eq!(entry.target_id.as_deref(), Some(&*row.id.to_string()));
    assert_eq!(entry.request_method.as_deref(), Some("GET"));
    assert_eq!(
        entry.request_path.as_deref(),
        Some(&*format!("/api/business/attachment/download/{}", row.id))
    );
    assert_eq!(entry.status_code, Some(200));
    assert_eq!(entry.duration_ms, Some(7));
    // 操作人（谁）
    assert_eq!(entry.user_id, Some(uid(U_OWNER)));
    assert_eq!(
        entry.username.as_deref(),
        Some(&*format!("att_{}", &U_OWNER[..8]))
    );
    // 请求上下文与其它日志同源
    assert_eq!(entry.request_id, "req-att02-1");
    assert_eq!(entry.user_agent.as_deref(), Some("att02-test/1.0"));
    assert_eq!(entry.ip, Some("10.9.8.7".parse().expect("IP")));
    // 下载了什么（detail 五个事实）
    let detail = entry.detail.as_ref().expect("detail 必须有");
    assert_eq!(detail["doc_type"], serde_json::json!("PAYABLE"));
    assert_eq!(detail["doc_id"], serde_json::json!(row.doc_id.to_string()));
    assert_eq!(
        detail["file_id"],
        serde_json::json!(row.file_id.to_string())
    );
    assert_eq!(detail["file_name"], serde_json::json!("发票影像.pdf"));
    assert_eq!(detail["file_size"], serde_json::json!(2048));
    assert_eq!(detail["file_role"], serde_json::json!("INVOICE"));
    // 读操作不留请求体 / 响应体（文件字节不进日志）
    assert!(entry.request_params.is_none());
    assert!(entry.response_data.is_none());
    println!("[ATT-02] 下载留痕：operation/target/detail/IP/UA/request_id 全部就位 ✓");
}

/// 构造一条下载留痕用的附件行（字段与 `find_active` 的投影一致）。
fn fixture_row() -> model::DocAttachmentRow {
    model::DocAttachmentRow {
        id: uid("d2120300-0000-4000-8000-000000000001"),
        doc_type: "PAYABLE".to_string(),
        doc_id: uid(DOC_PAYABLE),
        file_id: uid(FILE_B),
        file_role: "INVOICE".to_string(),
        remark: Some("发票第一联".to_string()),
        uploaded_by: Some(uid(U_OWNER)),
        uploader_name: "附件上传人".to_string(),
        file_name: "发票影像.pdf".to_string(),
        file_size: 2048,
        mime_type: Some("application/pdf".to_string()),
        sort_order: 0,
        created_at: chrono::Utc::now(),
    }
}

// ---------- 真库 ----------

/// 真库用例必须串行（共用全局 `db()` 与同一批夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 连库（`ERP_DB_URL` 未设置 → `None`，用例 SKIP）。
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

async fn exec(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 清本文件造的夹具（外键顺序：绑定行 → sys_file → 单据 → 主数据 → 用户）。
async fn purge_fixtures() {
    let cleanups = [
        "DELETE FROM biz_doc_attachment WHERE doc_id IN \
           ('d2120101-0000-4000-8000-000000000001'::uuid, \
            'd2120102-0000-4000-8000-000000000001'::uuid, \
            'd2120103-0000-4000-8000-000000000001'::uuid, \
            'd2120104-0000-4000-8000-000000000001'::uuid, \
            'd2120105-0000-4000-8000-000000000001'::uuid)"
            .to_string(),
        "DELETE FROM sys_file WHERE id IN \
            ('d2120f01-0000-4000-8000-000000000001'::uuid, \
             'd2120f02-0000-4000-8000-000000000002'::uuid, \
             'd2120f03-0000-4000-8000-000000000003'::uuid)"
            .to_string(),
        "DELETE FROM biz_payment WHERE id = 'd2120105-0000-4000-8000-000000000001'::uuid"
            .to_string(),
        "DELETE FROM biz_payable WHERE id = 'd2120104-0000-4000-8000-000000000001'::uuid"
            .to_string(),
        "DELETE FROM biz_stock_in WHERE id = 'd2120103-0000-4000-8000-000000000001'::uuid"
            .to_string(),
        "DELETE FROM biz_purchase_order WHERE id = 'd2120102-0000-4000-8000-000000000001'::uuid"
            .to_string(),
        "DELETE FROM biz_contract WHERE id = 'd2120101-0000-4000-8000-000000000001'::uuid"
            .to_string(),
        "DELETE FROM biz_warehouse WHERE id = 'd2120200-0000-4000-8000-000000000002'::uuid"
            .to_string(),
        "DELETE FROM biz_supplier WHERE id = 'd2120200-0000-4000-8000-000000000001'::uuid"
            .to_string(),
        "DELETE FROM sys_user_role WHERE user_id IN \
           ('d2120901-0000-4000-8000-000000000001'::uuid, \
            'd2120902-0000-4000-8000-000000000002'::uuid)"
            .to_string(),
        "DELETE FROM sys_user WHERE id IN \
           ('d2120901-0000-4000-8000-000000000001'::uuid, \
            'd2120902-0000-4000-8000-000000000002'::uuid)"
            .to_string(),
    ];
    for sql in cleanups {
        exec(&sql).await;
    }
}

/// 拿锁 + 连库 + 清上一轮 + 造五类单据与两个 `sys_file`。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    purge_fixtures().await;

    let statements = [
        format!(
            "INSERT INTO sys_user (id, username, password, real_name, status, deleted_at) \
             VALUES ('{U_OWNER}'::uuid, 'att_owner', 'x', '附件上传人', 1, NULL), \
                    ('{U_OTHER}'::uuid, 'att_other', 'x', '附件旁观者', 1, NULL) \
             ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, credit_days, credit_limit, status) \
             VALUES ('{SUPPLIER}'::uuid, 'ATT-S01', '附件供应商', 0, 0, 1) ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative) \
             VALUES ('{WAREHOUSE}'::uuid, 'ATT-W01', '附件仓', '/{WAREHOUSE}/', 1, 1, 1, 0) ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, \
                 party_b_type, party_b_id, party_b_name, amount, status) \
             VALUES ('{DOC_CONTRACT}'::uuid, 'ATT-CT-0001', '附件测试合同', 'PURCHASE', \
                 'SUPPLIER', '{SUPPLIER}'::uuid, '附件供应商', 1000, 'DRAFT') ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status) \
             VALUES ('{DOC_PO}'::uuid, 'ATT-PO-0001', '{SUPPLIER}'::uuid, '2033-03-01', \
                 '{WAREHOUSE}'::uuid, 'DRAFT') ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_stock_in (id, in_no, in_type, warehouse_id, in_date, status, is_sourceless) \
             VALUES ('{DOC_IN}'::uuid, 'ATT-IN-0001', 'OTHER', '{WAREHOUSE}'::uuid, '2033-03-01', 'DRAFT', TRUE) \
             ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_payable (id, payable_no, supplier_id, source_type, source_id, source_no, \
                 amount, paid_amount, unpaid_amount, due_date, period_code, status) \
             VALUES ('{DOC_PAYABLE}'::uuid, 'ATT-AP-0001', '{SUPPLIER}'::uuid, 'STOCK_IN', \
                 '{DOC_IN}'::uuid, 'ATT-IN-0001', 0, 0, 0, '2033-03-31', '2033-03', 'OPEN') \
             ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_payment (id, payment_no, payable_id, amount, pay_date) \
             VALUES ('{DOC_PAYMENT}'::uuid, 'ATT-PAY-0001', '{DOC_PAYABLE}'::uuid, 100, '2033-03-10') \
             ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO sys_file (id, file_name, original_name, file_key, file_size, file_hash, \
                 mime_type, extension, category, uploaded_by, upload_mode, is_shared) \
             VALUES ('{FILE_A}'::uuid, '2033/03/01/att_a.png', '合同扫描件.png', '2033/03/01/att_a.png', \
                 1024, 'att-hash-a', 'image/png', 'png', 'attachment', '{U_OWNER}'::uuid, 'SERVER', FALSE), \
                    ('{FILE_B}'::uuid, '2033/03/01/att_b.pdf', '签收单.pdf', '2033/03/01/att_b.pdf', \
                 2048, 'att-hash-b', 'application/pdf', 'pdf', 'attachment', '{U_OWNER}'::uuid, 'SERVER', FALSE), \
                    ('{FILE_C}'::uuid, '2033/03/01/att_c.xlsx', '对账表.xlsx', '2033/03/01/att_c.xlsx', \
                 1536000, 'att-hash-c', 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet', \
                 'xlsx', 'attachment', '{U_OWNER}'::uuid, 'SERVER', FALSE) \
             ON CONFLICT DO NOTHING"
        ),
    ];
    for sql in statements {
        exec(&sql).await;
    }
    Some(guard)
}

/// 五类单据的存在性映射都指向正确的表（表名写错只会在真库用例里暴露）。
#[tokio::test]
async fn db_doc_exists_covers_five_tables() {
    let Some(_guard) = setup().await else {
        return;
    };
    for (kind, doc_id) in [
        (DocKind::Contract, DOC_CONTRACT),
        (DocKind::PurchaseOrder, DOC_PO),
        (DocKind::StockIn, DOC_IN),
        (DocKind::Payable, DOC_PAYABLE),
        (DocKind::Payment, DOC_PAYMENT),
    ] {
        let exists = model::doc_exists(kind, &uid(doc_id))
            .await
            .expect("查存在性");
        assert!(
            exists,
            "{} 的夹具单据应存在（表 {}）",
            kind.label(),
            kind.table()
        );
    }
    // 不存在的主键
    let missing = uuid::Uuid::now_v7();
    assert!(
        !model::doc_exists(DocKind::Contract, &missing)
            .await
            .expect("查存在性")
    );
}

/// 软删的单据不能再挂附件（`deleted_at IS NULL` 是判据的一部分）。
#[tokio::test]
async fn db_doc_exists_rejects_soft_deleted_doc() {
    let Some(_guard) = setup().await else {
        return;
    };
    exec(&format!(
        "UPDATE biz_contract SET deleted_at = now() WHERE id = '{DOC_CONTRACT}'::uuid"
    ))
    .await;
    let exists = model::doc_exists(DocKind::Contract, &uid(DOC_CONTRACT))
        .await
        .expect("查存在性");
    assert!(!exists, "已软删的合同不能再挂附件");
    // 复原（后续用例还要用这张单据）
    exec(&format!(
        "UPDATE biz_contract SET deleted_at = NULL WHERE id = '{DOC_CONTRACT}'::uuid"
    ))
    .await;
}

/// 绑定 → 列表 → 单条查询 → 软删：完整读写链路（含显示名与角色标签）。
#[tokio::test]
async fn db_bind_list_and_soft_delete() {
    let Some(_guard) = setup().await else {
        return;
    };
    let owner = uid(U_OWNER);
    let id = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_A),
        FileRole::ContractScan,
        Some("第一页有章"),
        Some(owner),
    )
    .await
    .expect("绑定附件");

    let list = model::list_by_doc(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        model::ListOrder::TimeDesc,
    )
    .await
    .expect("查列表");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].file_role, "CONTRACT_SCAN");
    assert_eq!(list[0].file_name, "合同扫描件.png");
    assert_eq!(list[0].file_size, 1024);
    assert_eq!(list[0].uploader_name, "附件上传人");
    assert_eq!(list[0].remark.as_deref(), Some("第一页有章"));
    assert_eq!(list[0].uploaded_by, Some(owner));

    // 单条查询（下载 / 删除前用）
    let row = model::find_active(&id)
        .await
        .expect("查单条")
        .expect("应存在");
    assert_eq!(row.file_id, uid(FILE_A));
    assert_eq!(row.doc_type, "CONTRACT");

    // 软删后：列表空、单条查不到、影响行数 1
    let affected = model::soft_delete(db(), &id).await.expect("软删");
    assert_eq!(affected, 1);
    assert!(
        model::list_by_doc(
            DocKind::Contract,
            &uid(DOC_CONTRACT),
            model::ListOrder::TimeDesc,
        )
        .await
        .expect("查列表")
        .is_empty()
    );
    assert!(model::find_active(&id).await.expect("查单条").is_none());
    // 幂等：再删一次影响 0 行（不报错）
    assert_eq!(model::soft_delete(db(), &id).await.expect("再删"), 0);
}

/// 同一张单据重复绑定同一份文件 → 幂等（返回同一行，不撞部分唯一索引）。
#[tokio::test]
async fn db_bind_is_idempotent_per_doc_and_file() {
    let Some(_guard) = setup().await else {
        return;
    };
    let first = model::insert(
        DocKind::PurchaseOrder,
        &uid(DOC_PO),
        &uid(FILE_B),
        FileRole::Other,
        None,
        Some(uid(U_OWNER)),
    )
    .await
    .expect("首次绑定");
    let second = model::insert(
        DocKind::PurchaseOrder,
        &uid(DOC_PO),
        &uid(FILE_B),
        FileRole::SignReceipt,
        None,
        Some(uid(U_OWNER)),
    )
    .await
    .expect("重复绑定必须幂等");
    assert_eq!(first, second);
    let list = model::list_by_doc(
        DocKind::PurchaseOrder,
        &uid(DOC_PO),
        model::ListOrder::TimeDesc,
    )
    .await
    .expect("查列表");
    assert_eq!(list.len(), 1, "同一文件不能在同一单据上出现两行");
    // 幂等不覆盖角色：第二次的角色被忽略（行仍是首次的 OTHER），这是刻意的
    assert_eq!(list[0].file_role, "OTHER");
}

/// 秒传共享的文件：还在被别的单据引用时，引用计数必须 > 0（删除时据此保留 `sys_file`）。
#[tokio::test]
async fn db_shared_file_reference_count() {
    let Some(_guard) = setup().await else {
        return;
    };
    let owner = Some(uid(U_OWNER));
    let a = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_A),
        FileRole::ContractScan,
        None,
        owner,
    )
    .await
    .expect("合同绑定");
    let b = model::insert(
        DocKind::StockIn,
        &uid(DOC_IN),
        &uid(FILE_A),
        FileRole::SignReceipt,
        None,
        owner,
    )
    .await
    .expect("收货单绑定同一份文件");

    assert_eq!(
        model::count_by_file(&uid(FILE_A)).await.expect("引用计数"),
        2
    );
    model::soft_delete(db(), &a).await.expect("删一条");
    assert_eq!(
        model::count_by_file(&uid(FILE_A)).await.expect("引用计数"),
        1,
        "还有别的单据在用这份文件，计数必须仍是 1"
    );
    model::soft_delete(db(), &b).await.expect("删另一条");
    assert_eq!(
        model::count_by_file(&uid(FILE_A)).await.expect("引用计数"),
        0
    );
}

/// 配额口径：`count_by_doc` 只数未删的行，且按单据隔离。
#[tokio::test]
async fn db_count_by_doc_isolation_and_quota() {
    let Some(_guard) = setup().await else {
        return;
    };
    let owner = Some(uid(U_OWNER));
    assert_eq!(
        model::count_by_doc(DocKind::StockIn, &uid(DOC_IN))
            .await
            .expect("计数"),
        0
    );
    let id = model::insert(
        DocKind::StockIn,
        &uid(DOC_IN),
        &uid(FILE_A),
        FileRole::SignReceipt,
        None,
        owner,
    )
    .await
    .expect("绑定");
    assert_eq!(
        model::count_by_doc(DocKind::StockIn, &uid(DOC_IN))
            .await
            .expect("计数"),
        1
    );
    // 别的单据 / 别的类型都不受影响
    assert_eq!(
        model::count_by_doc(DocKind::StockIn, &uid(DOC_PO))
            .await
            .expect("计数"),
        0
    );
    assert_eq!(
        model::count_by_doc(DocKind::Contract, &uid(DOC_IN))
            .await
            .expect("计数"),
        0
    );
    model::soft_delete(db(), &id).await.expect("软删");
    assert_eq!(
        model::count_by_doc(DocKind::StockIn, &uid(DOC_IN))
            .await
            .expect("计数"),
        0,
        "软删后配额立即释放"
    );
    // 上限常量必须与前端文案一致（50）
    assert_eq!(model::MAX_ATTACHMENTS_PER_DOC, 50);
}

// ---------- ATT-02：真库 ----------

/// ATT-02 ⑤（防哑弹）：`view_permissions()` 里的每个码都必须在 `sys_menu` 里真实存在。
///
/// 权限码与菜单行是一一对应的（AGENTS.md 约定 7）：写一个库里没有的码，等于给
/// 「有权限的角色」也发 403，而这种错误**只会在真机上被用户体验到**，单测看不出来。
/// 同一批码还必须至少被一个角色持有，否则它仍是个哑弹 —— 这里只查菜单存在性，
/// 「谁持有」由各模块的角色矩阵迁移负责。
#[tokio::test]
async fn view_permissions_are_real_menu_codes() {
    let Some(_guard) = setup().await else {
        return;
    };
    for kind in DocKind::ALL {
        for code in kind.view_permissions() {
            let found: i64 = db()
                .scalar_with_params(
                    "SELECT count(*) FROM sys_menu WHERE auth_code = ? AND deleted_at IS NULL",
                    vec![Value::from(*code)],
                )
                .await
                .expect("查 sys_menu");
            assert!(
                found > 0,
                "{} 的读码 {code} 不在 sys_menu 里（哑弹：有权限的人也会 403）",
                kind.label()
            );
        }
    }
    // 反例守卫：BUG-SYS-05 删掉的采购单详情码不能被重新写回清单
    for kind in DocKind::ALL {
        assert!(
            !kind
                .view_permissions()
                .contains(&"business:erp:purchase-order:detail"),
            "{} 引用了已删除的采购单详情码",
            kind.label()
        );
    }
    println!("[ATT-02] 七类单据的读码全部命中 sys_menu ✓");
}

/// ATT-02 ⑥（真库）：下载留痕能真正落进 `operation_logs`（含 INET / JSONB 绑定）。
///
/// 单测只验「模型字段对不对」；这条验「写库路径真的能通」—— `ip` 是 `INET`、
/// `detail` 是 `JSONB`，绑定类型写错在编译期看不出来，只会在运行期报错，
/// 而留痕失败是被 `warn` 吞掉的（不能因此让下载失败），所以必须有这条用例兜住。
#[tokio::test]
async fn db_download_log_lands_in_operation_logs() {
    let Some(_guard) = setup().await else {
        return;
    };
    use axum::http::HeaderMap;

    let log_id = uid("d2120e01-0000-4000-8000-000000000001");
    // 先清（重跑安全）
    exec(&format!(
        "DELETE FROM operation_logs WHERE id = '{log_id}'::uuid"
    ))
    .await;

    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", "req-att02-db".parse().expect("头值"));
    headers.insert("user-agent", "att02-db-test/1.0".parse().expect("头值"));
    headers.insert("x-real-ip", "10.9.8.7".parse().expect("头值"));
    let connect: std::net::SocketAddr = "127.0.0.1:54321".parse().expect("地址");
    let claims = claims_of(U_OWNER, &["business:erp:payable:list"]);
    let row = fixture_row();

    let mut entry =
        super::handler::download_log(&headers, &connect, &claims, &row, DocKind::Payable, 7);
    entry.id = log_id;
    entry
        .insert(db())
        .await
        .expect("下载留痕必须能写进 operation_logs");

    let rows = db()
        .find_with_params(
            "SELECT operation, target_type, target_id, status_code, request_method, \
                    request_path, username, detail->>'doc_type' AS doc_type, \
                    detail->>'file_name' AS file_name, \
                    (detail->>'file_size')::bigint AS file_size, \
                    host(ip) AS ip_text \
               FROM operation_logs WHERE id = ?::uuid",
            vec![Value::from(&log_id)],
        )
        .await
        .expect("读回留痕");
    assert_eq!(rows.len(), 1, "留痕必须恰好一行");
    let sql_row = &rows[0];
    assert_eq!(
        sql_row.get_typed::<String>("operation").expect("operation"),
        super::handler::OP_DOWNLOAD
    );
    assert_eq!(
        sql_row.get_typed::<String>("target_type").expect("type"),
        "DocAttachment"
    );
    assert_eq!(
        sql_row.get_typed::<String>("target_id").expect("id"),
        row.id.to_string()
    );
    assert_eq!(
        sql_row.get_typed::<i32>("status_code").expect("状态码"),
        200
    );
    assert_eq!(
        sql_row.get_typed::<String>("request_method").expect("方法"),
        "GET"
    );
    assert_eq!(
        sql_row.get_typed::<String>("doc_type").expect("doc_type"),
        "PAYABLE"
    );
    assert_eq!(
        sql_row.get_typed::<String>("file_name").expect("文件名"),
        "发票影像.pdf"
    );
    assert_eq!(sql_row.get_typed::<i64>("file_size").expect("大小"), 2048);
    assert_eq!(
        sql_row.get_typed::<String>("ip_text").expect("IP"),
        "10.9.8.7"
    );
    assert_eq!(
        sql_row.get_typed::<String>("username").expect("账号"),
        format!("att_{}", &U_OWNER[..8])
    );

    exec(&format!(
        "DELETE FROM operation_logs WHERE id = '{log_id}'::uuid"
    ))
    .await;
    let left: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM operation_logs WHERE id = ?::uuid",
            vec![Value::from(&log_id)],
        )
        .await
        .expect("查残留");
    assert_eq!(left, 0, "夹具留痕必须清干净");
    println!("[ATT-02] operation_logs 真库留痕：operation/target/detail/ip 全部正确落库 ✓");
}

// ============================================================================
// ATT-03：列表排序（默认上传时间倒序 + 手动 sort_order）
// ============================================================================

/// `?sort=` 口径解析：缺省 / 别名 → 时间倒序；未知值必须返回 `None`（由调用方 400）。
///
/// 钉住三件事：
/// 1. 老前端不带 `sort` 时行为不变（时间倒序），不是「报错」也不是「按 id 排」；
/// 2. 前端写 `manual` / `sort` / `sort_order` 都能命中手动口径（别名收敛，避免大小写漂移）；
/// 3. **未知值不静默回落** —— 回落会让「排序参数写错却拿到结果」变成最难查的假成功。
#[test]
fn list_order_parse_and_sql() {
    use super::model::ListOrder;

    assert_eq!(ListOrder::parse(None), Some(ListOrder::TimeDesc));
    assert_eq!(ListOrder::parse(Some("")), Some(ListOrder::TimeDesc));
    assert_eq!(ListOrder::parse(Some("   ")), Some(ListOrder::TimeDesc));
    assert_eq!(ListOrder::parse(Some("time")), Some(ListOrder::TimeDesc));
    assert_eq!(ListOrder::parse(Some(" TIME ")), Some(ListOrder::TimeDesc));
    assert_eq!(ListOrder::parse(Some("manual")), Some(ListOrder::Manual));
    assert_eq!(ListOrder::parse(Some("MANUAL")), Some(ListOrder::Manual));
    assert_eq!(ListOrder::parse(Some("sort")), Some(ListOrder::Manual));
    assert_eq!(
        ListOrder::parse(Some("sort_order")),
        Some(ListOrder::Manual)
    );
    assert_eq!(ListOrder::parse(Some("desc")), None, "未知口径必须拒绝");
    assert_eq!(ListOrder::parse(Some("1")), None);

    // 字面值可回解析（前端拿响应里的 sort 再传回来必须成立）
    for order in [ListOrder::TimeDesc, ListOrder::Manual] {
        assert_eq!(ListOrder::parse(Some(order.as_str())), Some(order));
    }

    // ORDER BY 是常量 SQL：两种口径都必须「时间倒序」兜底，手动口径还必须带 sort_order
    let time_sql = ListOrder::TimeDesc.order_by();
    let manual_sql = ListOrder::Manual.order_by();
    assert!(time_sql.contains("a.created_at DESC"), "{time_sql}");
    assert!(
        !time_sql.contains("sort_order"),
        "时间口径不应读 sort_order"
    );
    assert!(
        manual_sql.starts_with("ORDER BY a.sort_order ASC"),
        "{manual_sql}"
    );
    assert!(manual_sql.contains("a.created_at DESC"), "{manual_sql}");
    // 两种都以 id 收尾（同秒上传不抖）
    assert!(time_sql.ends_with("a.id DESC"), "{time_sql}");
    assert!(manual_sql.ends_with("a.id DESC"), "{manual_sql}");
    println!("[ATT-03] 排序口径解析 + ORDER BY 常量 ✓");
}

/// `parse_sort_order`：非法值 400 + 消息里列出可选值（与其它 4xx 同口径，运维不必翻代码）。
#[test]
fn parse_sort_order_rejects_unknown() {
    let ok = dto::parse_sort_order(Some("manual")).expect("manual 合法");
    assert_eq!(ok, super::model::ListOrder::Manual);
    let default = dto::parse_sort_order(None).expect("缺省合法");
    assert_eq!(default, super::model::ListOrder::TimeDesc);

    let error = dto::parse_sort_order(Some("uploaded_at")).expect_err("未知值必须拒绝");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, crate::shared::error_code::PARAM_INVALID);
    assert!(
        error.msg.contains("time"),
        "消息要列出可选值：{}",
        error.msg
    );
    assert!(
        error.msg.contains("manual"),
        "消息要列出可选值：{}",
        error.msg
    );
}

/// 重排入参：空 / 非 UUID / 重复 全部 400；合法时**按入参顺序**保留 id。
#[test]
fn parse_sort_request_validates() {
    use super::dto::DocAttachmentSortReq;

    let build = |ids: Vec<&str>| DocAttachmentSortReq {
        doc_type: "contract".to_string(),
        doc_id: DOC_CONTRACT.to_string(),
        ids: ids.into_iter().map(str::to_string).collect(),
    };

    // 合法：小写 doc_type 也能解析（大小写不敏感的老口径）
    let ok = dto::parse_sort_request(&build(vec![FILE_C, FILE_A, FILE_B])).expect("合法入参");
    assert_eq!(ok.kind, DocKind::Contract);
    assert_eq!(ok.doc_id, uid(DOC_CONTRACT));
    assert_eq!(ok.ids, vec![uid(FILE_C), uid(FILE_A), uid(FILE_B)]);
    assert_eq!(DocKind::Contract.as_str(), ok.kind.as_str());

    // 空列表
    let empty = dto::parse_sort_request(&build(vec![])).expect_err("空 ids 必须拒绝");
    assert_eq!(empty.status, 400);
    assert!(empty.msg.contains("ids"), "{}", empty.msg);

    // 非 UUID
    let bad = dto::parse_sort_request(&build(vec![FILE_A, "not-a-uuid"])).expect_err("非法 id");
    assert_eq!(bad.status, 400);
    assert!(
        bad.msg.contains("not-a-uuid"),
        "消息要带上原文：{}",
        bad.msg
    );

    // 重复
    let dup = dto::parse_sort_request(&build(vec![FILE_A, FILE_A])).expect_err("重复 id");
    assert_eq!(dup.status, 400);
    assert!(dup.msg.contains(FILE_A), "{}", dup.msg);

    // 单据主键非法 → 400（不是 500）
    let mut bad_doc = build(vec![FILE_A]);
    bad_doc.doc_id = "123".to_string();
    let err = dto::parse_sort_request(&bad_doc).expect_err("非法 doc_id");
    assert_eq!(err.status, 400);

    // 单据类型非法 → 400（消息列出七个可选值）
    let mut bad_kind = build(vec![FILE_A]);
    bad_kind.doc_type = "INVOICE".to_string();
    let err = dto::parse_sort_request(&bad_kind).expect_err("非法 doc_type");
    assert_eq!(err.status, 400);
    assert!(err.msg.contains("CONTRACT"), "{}", err.msg);

    println!("[ATT-03] 重排入参校验（空/非UUID/重复/类型/主键）✓");
}

/// 成员校验真值表：**顺序可以变**，集合必须相等（多一个 / 少一个都 400）。
#[test]
fn ensure_same_members_truth_table() {
    let a = uid(FILE_A);
    let b = uid(FILE_B);
    let c = uid(FILE_C);
    let active = vec![a, b, c];

    // 顺序不同 → 通过（重排的本质就是改顺序）
    super::handler::ensure_same_members(&active, &[c, a, b]).expect("顺序不同应通过");
    super::handler::ensure_same_members(&active, &active).expect("原样提交应通过");

    // 多一个不属于该单据的 id
    let extra = super::handler::ensure_same_members(&active, &[a, b, c, uid(DOC_CONTRACT)])
        .expect_err("多余 id 必须拒绝");
    assert_eq!(extra.status, 400);
    assert!(extra.msg.contains("不属于该单据"), "{}", extra.msg);

    // 少一个（漏提）
    let missing =
        super::handler::ensure_same_members(&active, &[a, b]).expect_err("缺失 id 必须拒绝");
    assert_eq!(missing.status, 400);
    assert!(missing.msg.contains("不完整"), "{}", missing.msg);

    // 空提交：报「缺失」而不是「多余」（顺序重要：先判多余再判缺失）
    let empty = super::handler::ensure_same_members(&active, &[]).expect_err("空提交必须拒绝");
    assert!(empty.msg.contains("不完整"), "{}", empty.msg);

    // 都空：0 个附件时提交空列表是合法边界（通过，不写库也没变化）
    super::handler::ensure_same_members(&[], &[]).expect("空对空应通过");

    println!("[ATT-03] 成员校验真值表（顺序可变 / 多一少一拒绝）✓");
}

/// 出参必须带 `sort_order`（前端上移 / 下移靠它和后端对齐）。
#[test]
fn to_item_carries_sort_order() {
    let mut row = fixture_row();
    row.sort_order = 7;
    let claims = claims_of(U_OWNER, &["business:attachment:list"]);
    let item = dto::to_item(&row, &claims);
    assert_eq!(item.sort_order, 7);
    assert_eq!(item.file_name, "发票影像.pdf");
    assert_eq!(item.file_size, 2048);
    assert_eq!(item.uploader_name, "附件上传人");
    assert!(item.can_delete, "上传人本人可删");
    // 序列化给前端的键名（前端 api/business/erp/attachment.ts 的 DocAttachmentItem）
    let json = serde_json::to_value(&item).expect("出参可序列化");
    assert_eq!(json["sort_order"], serde_json::json!(7));
    assert!(json["created_at"].is_string(), "时间必须是字符串");
    println!("[ATT-03] 出参带 sort_order ✓");
}

/// ATT-03 验收②（后端半边）：**默认口径按上传时间倒序**，`manual` 口径按 `sort_order`。
///
/// 三个附件按「A 最旧 → C 最新」造，于是：
/// - `time` → C, B, A（最近的在最上）；
/// - 写入 `sort_order = A:0, B:1, C:2` 后 `manual` → A, B, C（与时间口径相反，
///   这样两个口径不会因为恰好同序而无法区分）。
#[tokio::test]
async fn db_list_default_order_is_time_desc() {
    let Some(_guard) = setup().await else {
        return;
    };
    let owner = Some(uid(U_OWNER));
    let a = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_A),
        FileRole::ContractScan,
        None,
        owner,
    )
    .await
    .expect("绑 A");
    let b = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_B),
        FileRole::SignReceipt,
        None,
        owner,
    )
    .await
    .expect("绑 B");
    let c = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_C),
        FileRole::Invoice,
        None,
        owner,
    )
    .await
    .expect("绑 C");

    // 显式拉开上传时间（同一毫秒内插入会让倒序断言不稳定，但排序键带 id 兜底——
    // 这里写死时间是为了让断言表达「按时间」，而不是「按 id」）
    exec(&format!(
        "UPDATE biz_doc_attachment SET created_at = now() - interval '3 hours' WHERE id = '{a}'::uuid"
    ))
    .await;
    exec(&format!(
        "UPDATE biz_doc_attachment SET created_at = now() - interval '2 hours' WHERE id = '{b}'::uuid"
    ))
    .await;
    exec(&format!(
        "UPDATE biz_doc_attachment SET created_at = now() - interval '1 hour' WHERE id = '{c}'::uuid"
    ))
    .await;

    let time_desc = model::list_by_doc(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        model::ListOrder::TimeDesc,
    )
    .await
    .expect("查列表");
    assert_eq!(time_desc.len(), 3);
    assert_eq!(
        time_desc.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![c, b, a],
        "默认口径必须是上传时间倒序（最新的在最上）"
    );
    assert_eq!(time_desc[0].file_name, "对账表.xlsx");
    assert_eq!(time_desc[0].file_size, 1_536_000);

    // 手动口径：此时还没排过序，序号按插入顺序 0/1/2 → A, B, C
    let manual_before = model::list_by_doc(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        model::ListOrder::Manual,
    )
    .await
    .expect("查列表");
    assert_eq!(
        manual_before.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![a, b, c],
        "手动口径按 sort_order 升序（新上传追加在末尾）"
    );
    assert_eq!(
        manual_before
            .iter()
            .map(|row| row.sort_order)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    println!("[ATT-03] 默认时间倒序 / manual 按 sort_order ✓");
}

/// 新上传的附件**追加到末尾**（`sort_order = max + 1`），不吃列的 `DEFAULT 0`。
///
/// 反例很具体：如果新行拿默认 0，用户把 A,B,C 排好后新传 D 会插到最前面 ——
/// 这是「手动排序」功能最容易被用户一眼看出的错。
#[tokio::test]
async fn db_insert_appends_sort_order() {
    let Some(_guard) = setup().await else {
        return;
    };
    let owner = Some(uid(U_OWNER));
    let first = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_A),
        FileRole::ContractScan,
        None,
        owner,
    )
    .await
    .expect("绑 A");
    let second = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_B),
        FileRole::Other,
        None,
        owner,
    )
    .await
    .expect("绑 B");

    let manual = model::list_by_doc(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        model::ListOrder::Manual,
    )
    .await
    .expect("查列表");
    assert_eq!(
        manual
            .iter()
            .map(|row| (row.id, row.sort_order))
            .collect::<Vec<_>>(),
        vec![(first, 0), (second, 1)]
    );

    // 删掉第一个再重挂同一份文件：新行必须拿 2（max(1)+1），不能复用 0
    model::soft_delete(db(), &first).await.expect("软删 A");
    let again = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_A),
        FileRole::ContractScan,
        None,
        owner,
    )
    .await
    .expect("重挂 A");
    assert_ne!(again, first, "软删后重挂是一条新绑定行");
    let manual = model::list_by_doc(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        model::ListOrder::Manual,
    )
    .await
    .expect("查列表");
    assert_eq!(
        manual
            .iter()
            .map(|row| (row.id, row.sort_order))
            .collect::<Vec<_>>(),
        vec![(second, 1), (again, 2)],
        "重挂的附件必须追加在末尾（max+1），且序号不重号"
    );
    println!("[ATT-03] 新绑定追加 sort_order=max+1 ✓");
}

/// 手动排序写库：提交顺序 → `sort_order`，`active_ids` 同步；跨单据的 id 命中 0 行。
#[tokio::test]
async fn db_manual_sort_persists_and_rejects_foreign_ids() {
    let Some(_guard) = setup().await else {
        return;
    };
    let owner = Some(uid(U_OWNER));
    let a = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_A),
        FileRole::ContractScan,
        None,
        owner,
    )
    .await
    .expect("绑 A");
    let b = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_B),
        FileRole::SignReceipt,
        None,
        owner,
    )
    .await
    .expect("绑 B");
    let c = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_C),
        FileRole::Invoice,
        None,
        owner,
    )
    .await
    .expect("绑 C");
    // 另一张单据的附件（越权改排序的目标）
    let foreign = model::insert(
        DocKind::PurchaseOrder,
        &uid(DOC_PO),
        &uid(FILE_A),
        FileRole::Other,
        None,
        owner,
    )
    .await
    .expect("绑到采购单");

    // 倒序提交：C, B, A
    let updated = model::set_sort_orders(db(), DocKind::Contract, &uid(DOC_CONTRACT), &[c, b, a])
        .await
        .expect("重排");
    assert_eq!(updated, 3);

    assert_eq!(
        model::active_ids(DocKind::Contract, &uid(DOC_CONTRACT))
            .await
            .expect("查 id"),
        vec![c, b, a],
        "active_ids 必须与写入的顺序一致（重排接口的成员校验也读它）"
    );
    let manual = model::list_by_doc(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        model::ListOrder::Manual,
    )
    .await
    .expect("查列表");
    assert_eq!(
        manual.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![c, b, a]
    );
    assert_eq!(
        manual.iter().map(|row| row.sort_order).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );

    // 跨单据的 id：WHERE 里带 doc_type + doc_id，命中 0 行（不会改到采购单那条）
    let crossed = model::set_sort_orders(
        db(),
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &[foreign, a, b, c],
    )
    .await
    .expect("带外部 id 的重排不报错（只是命中少）");
    assert_eq!(crossed, 3, "只有属于该合同的三行被更新");
    assert_eq!(
        model::active_ids(DocKind::PurchaseOrder, &uid(DOC_PO))
            .await
            .expect("查 id"),
        vec![foreign],
        "采购单的附件序号不能被合同的请求改到"
    );

    // 已被软删的 id 也不命中
    model::soft_delete(db(), &c).await.expect("软删 C");
    assert_eq!(
        model::set_sort_orders(db(), DocKind::Contract, &uid(DOC_CONTRACT), &[c, a, b])
            .await
            .expect("含已删行"),
        2,
        "已软删的附件不能被重排"
    );
    println!("[ATT-03] 手动排序落库 / active_ids 同步 / 跨单据与已删行不命中 ✓");
}

/// 收尾：断言夹具全部清干净（跨 run 不留残留）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else {
        return;
    };
    purge_fixtures().await;
    let left: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_doc_attachment WHERE doc_id IN \
                ('d2120101-0000-4000-8000-000000000001'::uuid, \
                 'd2120102-0000-4000-8000-000000000001'::uuid, \
                 'd2120103-0000-4000-8000-000000000001'::uuid, \
                 'd2120104-0000-4000-8000-000000000001'::uuid, \
                 'd2120105-0000-4000-8000-000000000001'::uuid)",
            vec![],
        )
        .await
        .expect("查残留");
    assert_eq!(left, 0, "夹具附件绑定必须清零");
    let files: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM sys_file WHERE id IN \
                ('d2120f01-0000-4000-8000-000000000001'::uuid, \
                 'd2120f02-0000-4000-8000-000000000002'::uuid, \
                 'd2120f03-0000-4000-8000-000000000003'::uuid)",
            vec![],
        )
        .await
        .expect("查残留");
    assert_eq!(files, 0, "夹具 sys_file 必须清零");
}
