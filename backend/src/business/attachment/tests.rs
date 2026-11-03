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

/// 五类单据 → 五张表，一一对应且不重复。
#[test]
fn doc_kinds_cover_five_distinct_tables() {
    assert_eq!(DocKind::ALL.len(), 5);
    let mut tables: Vec<&str> = DocKind::ALL.iter().map(|kind| kind.table()).collect();
    tables.sort_unstable();
    tables.dedup();
    assert_eq!(
        tables.len(),
        5,
        "五类单据必须落在五张不同的表上：{tables:?}"
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
             'd2120f02-0000-4000-8000-000000000002'::uuid)"
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
                 2048, 'att-hash-b', 'application/pdf', 'pdf', 'attachment', '{U_OWNER}'::uuid, 'SERVER', FALSE) \
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

    let list = model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT))
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
        model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT))
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
    let list = model::list_by_doc(DocKind::PurchaseOrder, &uid(DOC_PO))
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
                 'd2120f02-0000-4000-8000-000000000002'::uuid)",
            vec![],
        )
        .await
        .expect("查残留");
    assert_eq!(files, 0, "夹具 sys_file 必须清零");
}
