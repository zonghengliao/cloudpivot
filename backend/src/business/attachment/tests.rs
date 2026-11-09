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
use super::rule::{self as att_rule, AttachRuleError, RequiredAt};
use crate::shared::error_code;

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

/// 销售发票夹具（**数电票**：`invoice_code IS NULL` → 命中电子票规则 pdf/ofd/xml）。
const DOC_INVOICE: &str = "d2120106-0000-4000-8000-000000000001";
/// 销售发票夹具（**纸质票**：带税控代码 → 命中纸质票规则 pdf/jpg/png）。
const DOC_INVOICE_PAPER: &str = "d2120107-0000-4000-8000-000000000001";

/// 两个 `sys_file` 夹具（不同内容哈希，避免撞 `uk_sys_file_hash`）。
const FILE_A: &str = "d2120f01-0000-4000-8000-000000000001";
const FILE_B: &str = "d2120f02-0000-4000-8000-000000000002";
/// 第三个文件（ATT-03 排序用例需要「同一张单据 3 个附件」才能验出顺序）。
const FILE_C: &str = "d2120f03-0000-4000-8000-000000000003";
/// ATT-06：`jpg` 夹具（验「格式不对」必须用白名单外的真实扩展名）。
const FILE_D: &str = "d2120f04-0000-4000-8000-000000000004";

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

/// 十六类单据 → 十六张表，一一对应且不重复（FIN-3 加报销单；ATT-06 加发票 / 收款 / 盘点 / 采购退货 / 销售退货；
/// ATT-11 加调拨单 / 供应商 / 客户 / 期初余额）。
#[test]
fn doc_kinds_cover_five_distinct_tables() {
    assert_eq!(DocKind::ALL.len(), 16);
    let mut tables: Vec<&str> = DocKind::ALL.iter().map(|kind| kind.table()).collect();
    tables.sort_unstable();
    tables.dedup();
    assert_eq!(tables.len(), 16, "十六类单据必须落在十六张不同的表上：{tables:?}");
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
    assert_eq!(DocKind::parse("BILL"), None);
    assert_eq!(DocKind::parse(""), None);
    // ATT-06 新增的 5 类必须可回解析（附件要能挂到它们上）
    for raw in ["INVOICE", "RECEIPT", "STOCK_TAKE", "PURCHASE_RETURN", "SALES_RETURN"] {
        let kind = DocKind::parse(raw).unwrap_or_else(|| panic!("{raw} 必须可解析"));
        assert_eq!(kind.as_str(), raw);
    }
    // ATT-11 新增的 4 类（建议传，不校验）同样必须可回解析
    for raw in ["STOCK_TRANSFER", "SUPPLIER", "CUSTOMER", "OPENING_BALANCE"] {
        let kind = DocKind::parse(raw).unwrap_or_else(|| panic!("{raw} 必须可解析"));
        assert_eq!(kind.as_str(), raw);
    }
    // 期初余额的锚点是**期初凭证**：存在性 SQL 必须带 voucher_type 限定，
    // 否则任意凭证 id 都能当「期初单据」挂附件。
    assert!(
        DocKind::OpeningBalance
            .exists_sql()
            .contains("voucher_type = 'OPENING'"),
        "期初余额的存在性校验必须限定 voucher_type = 'OPENING'"
    );
}

/// 文件角色：15 个可落库值 + 空值回落 DEFAULT + 老值归一 + 未知值拒绝（ATT-04 / ATT-11）。
#[test]
fn file_roles_parse_and_default() {
    // 可落库范围 = 分栏 14 个 + 兼容值 SIGN_RECEIPT
    assert_eq!(FileRole::ALL.len(), 15);
    assert_eq!(FileRole::GROUPS.len(), 14);
    for role in FileRole::ALL {
        assert_eq!(FileRole::parse(Some(role.as_str())), Some(role));
        assert!(!role.label().is_empty());
        // 每个可落库值都必须在约束允许的字面值里（两套词表不能有孤儿）
        assert!(
            FileRole::GROUPS.contains(&role) || role == FileRole::SignReceipt,
            "{} 既不在 GROUPS 里，也不是兼容值",
            role.as_str()
        );
    }
    // 词表顺序逐字一致（ATT-04 的 6 个 + ATT-11 的 8 个，DEFAULT 殿后）
    let groups: Vec<&str> = FileRole::GROUPS.iter().map(|role| role.as_str()).collect();
    assert_eq!(
        groups,
        vec![
            "CONTRACT_SCAN",
            "SUPPLEMENT",
            "ACCEPTANCE",
            "INVOICE_SCAN",
            "BANK_RECEIPT",
            "QUOTATION",
            "CUSTOMER_PO",
            "BIZ_LICENSE",
            "BANK_ACCOUNT_PERMIT",
            "QUALIFICATION",
            "CREDIT_FILE",
            "TRANSFER_APPROVAL",
            "OPENING_EVIDENCE",
            "DEFAULT"
        ]
    );
    // 卡面的中文映射逐字一致（前端分组标题直接用后端 label）
    let labels: Vec<&str> = FileRole::GROUPS.iter().map(|role| role.label()).collect();
    assert_eq!(
        labels,
        vec![
            "合同扫描件",
            "补充协议",
            "验收单",
            "发票影像",
            "银行回单",
            "报价单",
            "客户PO",
            "营业执照",
            "开户许可",
            "资质证书",
            "信用资料",
            "调拨审批单",
            "期初依据",
            "其他附件"
        ]
    );

    // 空值 → DEFAULT（验收④：不传 role 的文件进 DEFAULT 组）
    assert_eq!(FileRole::parse(None), Some(FileRole::Default));
    assert_eq!(FileRole::parse(Some("   ")), Some(FileRole::Default));

    // 老值归一（ATT-04）：INVOICE → INVOICE_SCAN、OTHER → DEFAULT，写库只写规范值
    assert_eq!(FileRole::parse(Some("invoice")), Some(FileRole::InvoiceScan));
    assert_eq!(FileRole::parse(Some("INVOICE")), Some(FileRole::InvoiceScan));
    assert_eq!(FileRole::parse(Some("other")), Some(FileRole::Default));
    assert_eq!(FileRole::InvoiceScan.as_str(), "INVOICE_SCAN");
    assert_eq!(FileRole::Default.as_str(), "DEFAULT");

    // 小写 / 去空白都能用（前端大小写不一致不挡人）
    assert_eq!(FileRole::parse(Some(" supplement ")), Some(FileRole::Supplement));
    assert_eq!(FileRole::parse(Some("Bank_Receipt")), Some(FileRole::BankReceipt));

    // 典型的「写错」：CONTRACT_SCANNED / SIGN 都不能被猜成 SCAN / RECEIPT
    assert_eq!(FileRole::parse(Some("CONTRACT_SCANNED")), None);
    assert_eq!(FileRole::parse(Some("SIGN")), None);
    // 新角色也不能被「近似猜中」（缺少 RECEIPT / 多了 S 都不行）
    assert_eq!(FileRole::parse(Some("BANK_RECEIPTS")), None);
    assert_eq!(FileRole::parse(Some("ACCEPT")), None);
}

/// 分栏清单（ATT-04）：固定 6 个必出现（含空分组），兼容角色只在有数据时出现。
#[test]
fn role_catalog_keeps_empty_groups_and_counts_every_item() {
    let claims = claims_of(U_OWNER, &[]);

    // ① 空列表也要给全**该单据类型**的分组（验收②：空分组显示上传引导）。
    // 合同 = 合同扫描件 / 补充协议 / 其他附件（ATT-11 按业务裁剪，不再画 14 个空分组）。
    let empty = dto::role_catalog(DocKind::Contract, &[]);
    assert_eq!(empty.len(), 3);
    assert!(empty.iter().all(|group| group.count == 0));
    assert_eq!(empty[0].code, "CONTRACT_SCAN");
    assert_eq!(empty[0].label, "合同扫描件");
    assert_eq!(empty[1].code, "SUPPLEMENT");
    assert_eq!(empty[2].code, "DEFAULT");
    assert_eq!(empty[2].label, "其他附件");

    // ② 合同单据：合同扫描件 + 补充协议 → 两个分组（验收①）
    let mut scan = fixture_item("CONTRACT_SCAN");
    let supplement = fixture_item("SUPPLEMENT");
    // 同一 role 多个文件（验收③）
    let scan2 = {
        let mut item = fixture_item("CONTRACT_SCAN");
        item.file_name = "合同扫描件-第2页.pdf".to_string();
        item
    };
    scan.remark = Some("第一联".to_string());

    let items = vec![scan, scan2, supplement];
    let catalog = dto::role_catalog(DocKind::Contract, &items);
    assert_eq!(catalog.len(), 3, "没有兼容角色时就是该类型的 3 个分组");
    let pick = |code: &str| {
        catalog
            .iter()
            .find(|group| group.code == code)
            .unwrap_or_else(|| panic!("分组 {code} 必须存在"))
            .clone()
    };
    assert_eq!(pick("CONTRACT_SCAN").count, 2);
    assert_eq!(pick("CONTRACT_SCAN").label, "合同扫描件");
    assert_eq!(pick("SUPPLEMENT").count, 1);
    assert_eq!(pick("SUPPLEMENT").label, "补充协议");
    // 空分组仍在（验收②）
    assert_eq!(pick("DEFAULT").count, 0);

    // ③ 不变式：每条附件恰好落进一个分组
    let sum: u64 = catalog.iter().map(|group| group.count).sum();
    assert_eq!(sum, items.len() as u64);

    // ④ 老值 / 未知值：INVOICE 归到 INVOICE_SCAN，OTHER 与未知值归到 DEFAULT（验收④）
    //    合同的分组清单里没有 INVOICE_SCAN → 它作为「数据里存在的其它角色」被追加，
    //    附件不会因为「不在建议清单里」而消失。
    let legacy = vec![
        fixture_item("INVOICE"),
        fixture_item("OTHER"),
        fixture_item("NOT_A_ROLE"),
        fixture_item("DEFAULT"),
    ];
    let catalog = dto::role_catalog(DocKind::Contract, &legacy);
    assert_eq!(catalog.len(), 4, "归一后不该多出「INVOICE」分组");
    let pick = |code: &str| {
        catalog
            .iter()
            .find(|group| group.code == code)
            .expect("分组必须存在")
            .count
    };
    assert_eq!(pick("INVOICE_SCAN"), 1);
    assert_eq!(pick("DEFAULT"), 3, "OTHER / 未知值 / DEFAULT 都进其他附件");
    assert_eq!(catalog.iter().map(|group| group.count).sum::<u64>(), 4);

    // ⑤ 兼容角色 SIGN_RECEIPT：有数据才追加（位置在该类型的固定分组之后）
    let with_legacy = vec![fixture_item("SIGN_RECEIPT"), fixture_item("CONTRACT_SCAN")];
    let catalog = dto::role_catalog(DocKind::Contract, &with_legacy);
    assert_eq!(catalog.len(), 4);
    assert_eq!(catalog[3].code, "SIGN_RECEIPT");
    assert_eq!(catalog[3].label, "签收单");
    assert_eq!(catalog[3].count, 1);
    assert_eq!(catalog.iter().map(|group| group.count).sum::<u64>(), 2);

    // ⑥ ATT-11 业务口径：每类单据画自己那几个角色（顺序 = 卡面顺序）
    let codes = |kind: DocKind| -> Vec<String> {
        dto::role_catalog(kind, &[])
            .iter()
            .map(|group| group.code.clone())
            .collect()
    };
    assert_eq!(
        codes(DocKind::Supplier),
        vec!["BIZ_LICENSE", "BANK_ACCOUNT_PERMIT", "QUALIFICATION", "DEFAULT"]
    );
    assert_eq!(codes(DocKind::Customer), vec!["BIZ_LICENSE", "CREDIT_FILE", "DEFAULT"]);
    assert_eq!(
        codes(DocKind::PurchaseOrder),
        vec!["QUOTATION", "CUSTOMER_PO", "DEFAULT"]
    );
    assert_eq!(codes(DocKind::StockTransfer), vec!["TRANSFER_APPROVAL", "DEFAULT"]);
    assert_eq!(codes(DocKind::OpeningBalance), vec!["OPENING_EVIDENCE", "DEFAULT"]);
    assert_eq!(codes(DocKind::Receipt), vec!["BANK_RECEIPT", "DEFAULT"]);

    // ⑦ TO-DTO：file_role_label 也要用归一后的中文名（tag 与分组标题同一份词表）
    let row = fixture_row();
    let item = dto::to_item(&row, &claims, true);
    assert_eq!(item.file_role, "INVOICE_SCAN");
    assert_eq!(item.file_role_label, "发票影像");
}

/// 构造一条列表出参（`role_catalog` 的入参，只需要 role）。
fn fixture_item(file_role: &str) -> dto::DocAttachmentItemRes {
    let mut item = dto::to_item(&fixture_row(), &claims_of(U_OWNER, &[]), true);
    item.file_role = file_role.to_string();
    item.file_role_label = FileRole::parse(Some(file_role))
        .unwrap_or(FileRole::Default)
        .label()
        .to_string();
    item
}

/// 上传元信息：doc_id 必须是 UUID，缺字段直接 400（不是 500）。
#[test]
fn upload_meta_rejects_missing_or_bad_ids() {
    let error = dto::parse_upload_meta(Some("CONTRACT"), None, None, None).expect_err("缺 doc_id 必须被拒");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("doc_id"), "{}", error.msg);

    let error =
        dto::parse_upload_meta(Some("CONTRACT"), Some("not-a-uuid"), None, None).expect_err("非法 UUID 必须被拒");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("不是合法 UUID"), "{}", error.msg);

    let error =
        dto::parse_upload_meta(Some("CONTRACTS"), Some(DOC_CONTRACT), None, None).expect_err("未知 doc_type 必须被拒");
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
    let meta = dto::parse_upload_meta(Some("CONTRACT"), Some(DOC_CONTRACT), None, Some("  ")).expect("空备注必须放行");
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
///
/// ATT-04 起多了一个入参 `can_view_doc`（持 delete 码时还要求看得见该单据），
/// 两个分支的完整真值表见 [`can_delete_requires_doc_read_for_the_delete_code_branch`]。
#[test]
fn can_delete_is_owner_or_privileged() {
    let owner = claims_of(U_OWNER, &[]);
    let stranger = claims_of(U_OTHER, &[]);
    let auditor = claims_of(U_OTHER, &["business:attachment:delete"]);
    let wildcard = claims_of(U_OTHER, &["*"]);

    assert!(dto::can_delete(&owner, Some(uid(U_OWNER)), true));
    assert!(!dto::can_delete(&stranger, Some(uid(U_OWNER)), true));
    assert!(dto::can_delete(&auditor, Some(uid(U_OWNER)), true));
    // 通配权限（SUPER_ADMIN）走框架的 has_permission 短路
    assert!(dto::can_delete(&wildcard, Some(uid(U_OWNER)), true));
    // 无属主的历史行：只有升权码能删
    assert!(!dto::can_delete(&owner, None, true));
    assert!(dto::can_delete(&auditor, None, true));
}

/// 显示名：原始名优先；原始名为空时只取对象键的最后一段（不把日期目录漏给前端）。
#[test]
fn display_name_never_leaks_object_key() {
    assert_eq!(
        model::display_name(Some("合同扫描件.pdf"), "2026/11/03/abc.pdf"),
        "合同扫描件.pdf"
    );
    assert_eq!(model::display_name(Some("   "), "2026/11/03/abc.pdf"), "abc.pdf");
    assert_eq!(model::display_name(None, "2026/11/03/abc.pdf"), "abc.pdf");
    assert_eq!(model::display_name(None, ""), "");
}

// ---------- E2E07-FIX-3：文件名快照 ----------

/// **漂移守卫**：读路径必须**先**取绑定行的文件名快照，再回落 `sys_file.original_name`。
///
/// 这条断言看的是 SQL 常量本身 —— 秒传复用（同内容共用一条 `sys_file`）时，
/// 只要 `COALESCE` 的参数顺序被换成 `f.original_name` 优先，`db_*` 用例在**没有秒传**
/// 的夹具上依然全绿，bug 会静默复活。所以顺序在这里钉死。
#[test]
fn file_name_snapshot_is_read_before_sys_file_name() {
    let columns = super::model::COLUMNS;
    let snapshot = columns
        .find("NULLIF(a.file_name, '')")
        .unwrap_or_else(|| panic!("读路径丢了文件名快照列：{columns}"));
    let sys_file = columns
        .find("f.original_name")
        .unwrap_or_else(|| panic!("读路径丢了 sys_file 回落：{columns}"));
    assert!(
        snapshot < sys_file,
        "快照必须在 sys_file 之前（否则秒传时又显示别人的文件名）：{columns}"
    );
    // 同一个 COALESCE 里：快照必须是**第一个**参数
    // （`COALESCE(NULLIF(a.file_name,''), f.original_name, ...)`）
    let before = columns[..snapshot].trim_end();
    assert!(
        before.ends_with("COALESCE("),
        "快照必须是这个 COALESCE 的第一个参数（否则回落会盖掉它）：{columns}"
    );
}

/// 迁移 V0362 必须做三件事：加列（幂等）、回填存量、自检空值。
///
/// 与 `permission_codes_match_migration` 同一个思路：**迁移与代码是两份东西**，
/// 代码指望的列 / 回填口径在迁移里少一句，只有真库跑到那一步才会炸。
#[test]
fn migration_adds_and_backfills_file_name() {
    let up = include_str!("../../../migrations/business/V0362__doc_attachment_file_name.up.sql");
    assert!(
        up.contains("ADD COLUMN IF NOT EXISTS file_name varchar(255)"),
        "V0362 必须幂等地加 file_name varchar(255)"
    );
    assert!(
        up.contains("NULLIF(a.file_name, '') IS NULL"),
        "V0362 必须回填存量（且只碰空值，重复执行不覆盖已有快照）"
    );
    assert!(
        up.contains("is_nullable = 'YES'"),
        "V0362 自检要钉住「可空」（历史行要等回填）"
    );
    let down = include_str!("../../../migrations/business/V0362__doc_attachment_file_name.down.sql");
    assert!(
        down.contains("DROP COLUMN IF EXISTS file_name"),
        "V0362 回滚必须幂等地删列"
    );
}

/// 上传文件名归一（E2E07-FIX-3）：只取 basename、去控制字符、超长 400。
#[test]
fn normalize_file_name_strips_paths_and_controls() {
    // 正常名：原样保留（含中文 / 空格）
    assert_eq!(
        dto::normalize_file_name("e2e07-contract-scan.pdf").expect("正常名"),
        "e2e07-contract-scan.pdf"
    );
    assert_eq!(
        dto::normalize_file_name("  合同 扫描件（第 1 页）.pdf  ").expect("去首尾空白"),
        "合同 扫描件（第 1 页）.pdf"
    );

    // 目录段：只取最后一段（伪造路径不能进库、不能进列表）
    assert_eq!(
        dto::normalize_file_name("../../etc/passwd").expect("相对路径"),
        "passwd"
    );
    assert_eq!(
        dto::normalize_file_name("C:\\Users\\x\\drill-invoice.pdf").expect("Windows 路径"),
        "drill-invoice.pdf"
    );
    assert_eq!(dto::normalize_file_name("/tmp/a/b/c.pdf").expect("绝对路径"), "c.pdf");

    // 控制字符（头注入 / 伪造多行）：删掉而不是拒绝 —— 用户看到的仍是自己的文件名
    assert_eq!(
        dto::normalize_file_name("发票\r\nX-Injected: 1.pdf").expect("控制字符"),
        "发票X-Injected: 1.pdf"
    );

    // 空 / 只有目录 / 只有控制字符 → 400（与 read_multipart 的「缺少文件名」同码同文案）
    for bad in ["", "   ", "a/b/", "\r\n"] {
        let error = dto::normalize_file_name(bad).expect_err(&format!("{bad:?} 应被拒"));
        assert_eq!(error.status, 400, "{bad:?}");
        assert!(error.msg.contains("文件名"), "{bad:?} → {}", error.msg);
    }

    // 长度按**字符**算：255 个汉字必须过（varchar(255) 是 255 字符，不是 255 字节）
    let max_cn = "汉".repeat(dto::FILE_NAME_MAX_CHARS);
    assert_eq!(
        dto::normalize_file_name(&max_cn)
            .expect("255 个汉字应通过")
            .chars()
            .count(),
        dto::FILE_NAME_MAX_CHARS
    );
    // 256 个 → 400，报文明说上限（不静默截断）
    let too_long = format!("{max_cn}x");
    let error = dto::normalize_file_name(&too_long).expect_err("256 字符应被拒");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("255"), "{}", error.msg);
    assert!(error.msg.contains("256"), "报文要带上实际长度：{}", error.msg);
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
    let expected: [(DocKind, &[&str]); 16] = [
        (DocKind::Contract, &["business:erp:contract:list"]),
        (DocKind::PurchaseOrder, &["business:erp:purchase-order:list"]),
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
        // ATT-06 新增 5 类：发票 / 收款 / 盘点 / 采购退货 / 销售退货
        (
            DocKind::Invoice,
            &["business:erp:invoice:list", "business:erp:invoice:detail"],
        ),
        (
            DocKind::Receipt,
            &["business:erp:receipt:list", "business:erp:receipt:detail"],
        ),
        (
            DocKind::StockTake,
            &["business:erp:stock-take:list", "business:erp:stock-take:detail"],
        ),
        // 采购退货 / 销售退货没有独立的 GET 详情路由（`GET {id}` 也挂 `:list`）
        (DocKind::PurchaseReturn, &["business:erp:purchase-return:list"]),
        (DocKind::SalesReturn, &["business:erp:sales-return:list"]),
        // ATT-11：调拨单（列表 / 详情）、供应商 / 客户（列表）、期初（读码）
        (
            DocKind::StockTransfer,
            &["business:erp:transfer:list", "business:erp:transfer:detail"],
        ),
        (DocKind::Supplier, &["business:erp:supplier:list"]),
        (DocKind::Customer, &["business:erp:customer:list"]),
        (DocKind::OpeningBalance, &["business:erp:opening:read"]),
    ];
    assert_eq!(expected.len(), DocKind::ALL.len(), "十六类单据必须全覆盖");
    for (kind, codes) in expected {
        assert_eq!(kind.view_permissions(), codes, "{} 的模块读码漂移了", kind.label());
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
        assert!(!dto::can_view_doc(&attachment_only, kind), "{}", kind.label());
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

// ---------- ATT-04：上传 / 删除补单据读码 ----------

/// ATT-04 ①：上传前置判定链的顺序 —— **字段非法 400 先于 单据读码 403**，
/// 且**无读码的人拿任何 `doc_id` 都只得到同一份 403**（探测面截断）。
///
/// # 为什么这条测试能证明「403 先于 404」
///
/// [`dto::preflight_upload`] 是 handler 在**查库之前**调用的最后一步：
/// 它不接触数据库，所以「这个 id 在库里存不存在」在这里根本不可见。于是
/// **存在的 id / 不存在的 id / 临时生成的随机 UUID 返回逐字相同的 403 消息**，
/// 就说明用户拿不到「某 payable/contract id 是否存在」的任何一位信息
/// （404 只可能出现在读码通过之后，见 `handler::upload` 的第 5 步）。
#[test]
fn upload_preflight_forbids_blind_upload_before_existence_check() {
    // 典型口子（ATT-02 遗留）：持附件码 + 合同读码，**没有**应付单读码
    let blind = claims_of(
        U_OTHER,
        &[
            "business:attachment:upload",
            "business:attachment:list",
            "business:erp:contract:list",
        ],
    );
    // 什么都不持（连路由码都没有 —— 这里只看本函数的口径）
    let nobody = claims_of(U_OTHER, &[]);
    // 有应付单读码的人：本函数放行，存在性交给 handler 的 404 分支
    let payable = claims_of(U_OTHER, &["business:erp:payable:list"]);

    // ① 400 先于 403：连权都没有的人把 doc_type 写错 → 400（不是 403）
    let error = dto::preflight_upload(&nobody, Some("NOT_A_DOC"), Some(DOC_PAYABLE), None, None)
        .expect_err("非法 doc_type 必须 400");
    assert_eq!(error.status, 400, "{}", error.msg);
    assert_eq!(error.code, error_code::PARAM_INVALID);
    assert!(error.msg.contains("doc_type"), "{}", error.msg);

    // 400 也先于 403：有附件码但 id 不是 UUID → 400
    let error = dto::preflight_upload(&blind, Some("PAYABLE"), Some("not-a-uuid"), None, None)
        .expect_err("非法 doc_id 必须 400");
    assert_eq!(error.status, 400, "{}", error.msg);

    // ② 403：无读码 → 无论 id 是「存在 / 不存在 / 随机」都是同一份 403
    let random = Uuid::now_v7().to_string();
    let ghost = "00000000-0000-4000-8000-0000000000ff";
    let mut messages: Vec<String> = Vec::new();
    for doc_id in [DOC_PAYABLE, ghost, random.as_str()] {
        let error = dto::preflight_upload(&blind, Some("PAYABLE"), Some(doc_id), None, None)
            .expect_err("无应付单读码 → 不许盲传");
        assert_eq!(error.status, 403, "doc_id={doc_id}：{}", error.msg);
        assert_eq!(error.code, error_code::PERMISSION_DENIED);
        assert!(
            error.msg.contains("上传附件"),
            "上传被拒的消息要说「上传」而不是「查看」：{}",
            error.msg
        );
        assert!(
            error.msg.contains("business:erp:payable:list"),
            "必须点名缺哪个码：{}",
            error.msg
        );
        // 消息里不得出现 id / 文件名 / 「不存在」这类会泄露存在性的事实
        assert!(!error.msg.contains(doc_id), "403 消息不得回显 doc_id：{}", error.msg);
        assert!(!error.msg.contains("不存在"), "{}", error.msg);
        messages.push(error.msg);
    }
    assert_eq!(
        messages[0], messages[2],
        "存在的 id 与随机 id 的 403 消息必须逐字相同（否则就是存在性探测面）"
    );

    // ③ 非一刀切：同一账号对「有读码的模块」（合同）放行到查库阶段
    let ok = dto::preflight_upload(
        &blind,
        Some("CONTRACT"),
        Some(DOC_CONTRACT),
        Some("CONTRACT_SCAN"),
        Some("合同扫描件"),
    )
    .expect("有合同读码 → 上传前置通过");
    assert_eq!(ok.doc_type, DocKind::Contract);
    assert_eq!(ok.file_role, FileRole::ContractScan);
    assert_eq!(ok.remark.as_deref(), Some("合同扫描件"));

    // ④ 有应付单读码 → 也放行（存在性判定由 handler 负责）
    assert!(dto::preflight_upload(&payable, Some("PAYABLE"), Some(DOC_PAYABLE), None, None).is_ok());

    // ⑤ 白名单外的 doc_type 之上再叠非法 doc_id：仍然是 400（先判字段）
    let error = dto::preflight_upload(&blind, Some("NOT_A_DOC"), Some("bad"), None, None).expect_err("字段非法优先");
    assert_eq!(error.status, 400);

    println!("[ATT-04] 上传前置：400 先于 403；无读码时任意 id 均同一份 403（无可探测位）✓");
}

/// ATT-04 ②：上传 403 的消息口径（与 ATT-02 的 403 同一出口，只换动作短语）。
#[test]
fn doc_upload_forbidden_names_the_missing_permission() {
    let error = dto::doc_upload_forbidden(DocKind::Payable);
    assert_eq!(error.status, 403);
    assert_eq!(error.code, error_code::PERMISSION_DENIED);
    assert!(error.msg.contains("应付单"), "{}", error.msg);
    assert!(error.msg.contains("上传"), "{}", error.msg);

    // 16 类单据都能生成 403（不留「某类单据没配码 → 空消息」的坑）
    for kind in DocKind::ALL {
        let error = dto::doc_upload_forbidden(kind);
        assert_eq!(error.status, 403, "{}", kind.label());
        assert!(error.msg.contains(kind.label()), "{}", error.msg);
        assert!(
            error.msg.contains(kind.view_permissions()[0]),
            "{} 必须点名读码：{}",
            kind.label(),
            error.msg
        );
        assert!(!error.msg.contains("/ ）"), "{}", error.msg);
        assert!(!error.msg.contains("/ )"), "{}", error.msg);
        // 与读侧 403 共用同一条构造：只差动作短语
        let view = dto::doc_forbidden(kind);
        assert_ne!(view.msg, error.msg, "读 / 写两条 403 的措辞要能区分动作");
    }
    println!("[ATT-04] 上传 403：30001 + 点名读码，16 类单据全覆盖 ✓");
}

/// ATT-04 ③：`can_delete` 真值表 —— **上传人删自己的不看读码；删别人的要 delete 码且看得见**。
#[test]
fn can_delete_requires_doc_read_for_the_delete_code_branch() {
    let owner = claims_of(U_OWNER, &["business:attachment:upload"]);
    let deleter = claims_of(U_OTHER, &["business:attachment:delete"]);
    let wildcard = claims_of(U_OTHER, &["*"]);
    let viewer = claims_of(U_OTHER, &["business:erp:payable:list"]);

    // 上传人本人：看不看得见都能删（换岗后仍能撤自己当年挂的附件 —— 卡面明确接受）
    assert!(dto::can_delete(&owner, Some(uid(U_OWNER)), true));
    assert!(
        dto::can_delete(&owner, Some(uid(U_OWNER)), false),
        "上传人本人在看不见单据时仍能删自己的（否则那条附件只能改库删）"
    );

    // 持 delete 码删别人的：必须看得见该单据（ATT-04 补的）
    assert!(dto::can_delete(&deleter, Some(uid(U_OWNER)), true));
    assert!(
        !dto::can_delete(&deleter, Some(uid(U_OWNER)), false),
        "持 :delete 但看不见该单据 → 不许删（升权处置一张看不见的单没有业务含义）"
    );

    // 无属主历史行：只有 delete 码能删，且同样要看读码
    assert!(!dto::can_delete(&deleter, None, false));
    assert!(dto::can_delete(&deleter, None, true));
    assert!(!dto::can_delete(&owner, None, true), "上传人不是属主时不能删无主行");

    // 超管：两个短路都要生效（has_permission("*") + has_any_permission("*")）
    assert!(dto::can_delete(
        &wildcard,
        Some(uid(U_OWNER)),
        dto::can_view_doc(&wildcard, DocKind::Payable)
    ));

    // 关键不变式：**能看 ≠ 能删**（读码不能替代删除权）
    assert!(
        !dto::can_delete(&viewer, Some(uid(U_OWNER)), true),
        "只有读码、既非上传人又无 :delete → 不许删别人的"
    );

    println!("[ATT-04] can_delete：上传人本人不看读码；删别人的要 :delete + 读码；能看 ≠ 能删 ✓");
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

    let entry = super::handler::download_log(&headers, &connect, &claims, &row, DocKind::Payable, 7);

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
    assert_eq!(entry.username.as_deref(), Some(&*format!("att_{}", &U_OWNER[..8])));
    // 请求上下文与其它日志同源
    assert_eq!(entry.request_id, "req-att02-1");
    assert_eq!(entry.user_agent.as_deref(), Some("att02-test/1.0"));
    assert_eq!(entry.ip, Some("10.9.8.7".parse().expect("IP")));
    // 下载了什么（detail 五个事实）
    let detail = entry.detail.as_ref().expect("detail 必须有");
    assert_eq!(detail["doc_type"], serde_json::json!("PAYABLE"));
    assert_eq!(detail["doc_id"], serde_json::json!(row.doc_id.to_string()));
    assert_eq!(detail["file_id"], serde_json::json!(row.file_id.to_string()));
    assert_eq!(detail["file_name"], serde_json::json!("发票影像.pdf"));
    assert_eq!(detail["file_size"], serde_json::json!(2048));
    assert_eq!(detail["file_role"], serde_json::json!("INVOICE_SCAN"));
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
        file_role: "INVOICE_SCAN".to_string(),
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

// ---------- ATT-08：校验接口（`GET /attachment/check`）的纯逻辑 ----------

/// `at` 解析：缺省 / 空串 → `SUBMIT`，大小写不敏感，未知值 400（不静默回落）。
#[test]
fn check_parse_required_at() {
    assert_eq!(dto::parse_required_at(None).expect("缺省"), RequiredAt::Submit);
    assert_eq!(dto::parse_required_at(Some("  ")).expect("空串"), RequiredAt::Submit);
    assert_eq!(
        dto::parse_required_at(Some("confirm")).expect("小写"),
        RequiredAt::Confirm
    );
    assert_eq!(
        dto::parse_required_at(Some("Activate")).expect("混合大小写"),
        RequiredAt::Activate
    );
    for bad in ["CONFIRMED", "SUBMITTED", "1", "提交"] {
        let error = dto::parse_required_at(Some(bad)).expect_err("未知动作必须 400");
        assert_eq!(error.status, 400, "{bad}");
        assert_eq!(error.code, error_code::PARAM_INVALID, "{bad}");
        assert!(
            error.msg.contains("SUBMIT / CONFIRM / ACTIVATE"),
            "文案要列出可选值：{}",
            error.msg
        );
    }
}

/// 卡面契约形状：`{ can_proceed, missing: [{ role, need, have, min, ... }] }`。
#[test]
fn check_res_shape_matches_card_contract() {
    let rule = att_rule::AttachmentRule {
        id: uid("d3470001-0000-4000-8000-000000000001"),
        doc_type: DocKind::Invoice,
        file_role: None,
        required_at: RequiredAt::Submit,
        min_count: 1,
        max_count: None,
        allow_exts: att_rule::parse_exts(Some("pdf,ofd,xml")),
        condition_sql: None,
        remark: Some("发票影像".to_string()),
    };
    let violation = att_rule::evaluate(&rule, &[]).expect("没有附件必须违例");
    let res = dto::to_check_res(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit, &[violation]);
    assert!(!res.can_proceed);
    assert_eq!(res.doc_type, "INVOICE");
    assert_eq!(res.doc_id, DOC_INVOICE);
    assert_eq!(res.at, "SUBMIT");
    assert_eq!(res.at_label, "提交");
    assert_eq!(res.missing.len(), 1);
    let missing = &res.missing[0];
    // 卡面点名的四个字段
    assert_eq!(missing.role, None, "种子规则 file_role 为 NULL = 任意角色");
    assert_eq!(missing.need.as_deref(), Some("pdf/ofd/xml"));
    assert_eq!(missing.have, 0);
    assert_eq!(missing.min, 1);
    // 补齐的事实字段（前端不自己再算）
    assert_eq!(missing.total, 0);
    assert_eq!(missing.shortfall, 1);
    assert!(missing.actual_exts.is_empty());
    assert_eq!(missing.role_label, "发票影像");
    // 红字与 400 报文（同源）
    assert_eq!(missing.hint, "缺少发票影像（需 pdf/ofd/xml 格式）");
    assert_eq!(res.hint.as_deref(), Some("缺少发票影像（需 pdf/ofd/xml 格式）"));
    assert!(
        res.message
            .as_deref()
            .is_some_and(|text| text.contains("发票必须上传发票影像（pdf/ofd/xml）")),
        "tooltip 必须与后端 400 同源"
    );

    // 无缺口：can_proceed = true，hint / message 都是 None（前端据此不画红字）
    let ok = dto::to_check_res(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit, &[]);
    assert!(ok.can_proceed);
    assert!(ok.missing.is_empty());
    assert!(ok.hint.is_none());
    assert!(ok.message.is_none());
}

/// `can_proceed` 与 `missing.is_empty()` 是同一条判据（前端只读一个字段也不会判错）。
#[test]
fn check_can_proceed_iff_no_missing() {
    let mut rule = att_rule::AttachmentRule {
        id: uid("d3470001-0000-4000-8000-000000000001"),
        doc_type: DocKind::Invoice,
        file_role: None,
        required_at: RequiredAt::Submit,
        min_count: 2,
        max_count: None,
        allow_exts: att_rule::parse_exts(Some("pdf")),
        condition_sql: None,
        remark: Some("发票影像".to_string()),
    };
    let facts = [att_rule::AttachmentFact {
        file_role: "DEFAULT".to_string(),
        ext: "pdf".to_string(),
    }];
    let violation = att_rule::evaluate(&rule, &facts).expect("只传 1 个、要 2 个 → 违例");
    let res = dto::to_check_res(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit, &[violation]);
    assert_eq!(res.can_proceed, res.missing.is_empty());
    assert!(!res.can_proceed);

    // 多条缺口 → 红字用「；」串、报文用换行串
    rule.min_count = 1;
    rule.remark = Some("发票影像".to_string());
    let mut second = rule.clone();
    second.id = uid("d3470001-0000-4000-8000-000000000003");
    second.remark = Some("银行回单".to_string());
    let first = att_rule::evaluate(&rule, &[]).expect("缺一");
    let second = att_rule::evaluate(&second, &[]).expect("缺二");
    let res = dto::to_check_res(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit, &[first, second]);
    let hint = res.hint.expect("有缺口就有红字");
    assert!(hint.contains('；'), "多条红字用中文分号连接：{hint}");
    assert!(hint.contains("发票影像") && hint.contains("银行回单"), "{hint}");
    let message = res.message.expect("有缺口就有报文");
    assert!(message.contains('\n'), "报文逐行（与 400 msg 一致）：{message}");
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
            'd2120105-0000-4000-8000-000000000001'::uuid, \
            'd2120106-0000-4000-8000-000000000001'::uuid, \
            'd2120107-0000-4000-8000-000000000001'::uuid)"
            .to_string(),
        "DELETE FROM sys_file WHERE id IN \
            ('d2120f01-0000-4000-8000-000000000001'::uuid, \
             'd2120f02-0000-4000-8000-000000000002'::uuid, \
             'd2120f03-0000-4000-8000-000000000003'::uuid, \
             'd2120f04-0000-4000-8000-000000000004'::uuid)"
            .to_string(),
        // ATT-06：发票夹具 + 规则开关复原（用例里改过 enabled 的，下一轮从启用态开始）
        "DELETE FROM biz_invoice WHERE id IN \
            ('d2120106-0000-4000-8000-000000000001'::uuid, \
             'd2120107-0000-4000-8000-000000000001'::uuid)"
            .to_string(),
        "UPDATE biz_attachment_rule SET enabled = TRUE WHERE doc_type = 'INVOICE'".to_string(),
        "DELETE FROM biz_payment WHERE id = 'd2120105-0000-4000-8000-000000000001'::uuid".to_string(),
        "DELETE FROM biz_payable WHERE id = 'd2120104-0000-4000-8000-000000000001'::uuid".to_string(),
        "DELETE FROM biz_stock_in WHERE id = 'd2120103-0000-4000-8000-000000000001'::uuid".to_string(),
        "DELETE FROM biz_purchase_order WHERE id = 'd2120102-0000-4000-8000-000000000001'::uuid".to_string(),
        "DELETE FROM biz_contract WHERE id = 'd2120101-0000-4000-8000-000000000001'::uuid".to_string(),
        "DELETE FROM biz_warehouse WHERE id = 'd2120200-0000-4000-8000-000000000002'::uuid".to_string(),
        "DELETE FROM biz_supplier WHERE id = 'd2120200-0000-4000-8000-000000000001'::uuid".to_string(),
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
            "INSERT INTO biz_invoice (id, invoice_no, invoice_type, invoice_code, is_electronic, customer_id, \
                 invoice_date, amount, tax_amount, total_with_tax, tax_rate, status) \
             VALUES ('{DOC_INVOICE}'::uuid, 'ATT-IV-0001', 'OUT', NULL, TRUE, '{SUPPLIER}'::uuid, \
                 '2033-03-01', 100, 13, 113, 0.13, 'DRAFT'), \
                    ('{DOC_INVOICE_PAPER}'::uuid, 'ATT-IV-0002', 'OUT', '044031900111', FALSE, \
                 '{SUPPLIER}'::uuid, '2033-03-01', 100, 13, 113, 0.13, 'DRAFT') \
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
                 'xlsx', 'attachment', '{U_OWNER}'::uuid, 'SERVER', FALSE), \
                 ('{FILE_D}'::uuid, '2033/03/01/att_d.jpg', '发票影像.jpg', '2033/03/01/att_d.jpg', \
                 8192, 'att-hash-d', 'image/jpeg', 'jpg', 'attachment', '{U_OWNER}'::uuid, 'SERVER', FALSE) \
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
        let exists = model::doc_exists(kind, &uid(doc_id)).await.expect("查存在性");
        assert!(exists, "{} 的夹具单据应存在（表 {}）", kind.label(), kind.table());
    }
    // 不存在的主键
    let missing = uuid::Uuid::now_v7();
    assert!(!model::doc_exists(DocKind::Contract, &missing).await.expect("查存在性"));
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

    let list = model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT), model::ListOrder::TimeDesc)
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
    let row = model::find_active(&id).await.expect("查单条").expect("应存在");
    assert_eq!(row.file_id, uid(FILE_A));
    assert_eq!(row.doc_type, "CONTRACT");

    // 软删后：列表空、单条查不到、影响行数 1
    let affected = model::soft_delete(db(), &id).await.expect("软删");
    assert_eq!(affected, 1);
    assert!(
        model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT), model::ListOrder::TimeDesc,)
            .await
            .expect("查列表")
            .is_empty()
    );
    assert!(model::find_active(&id).await.expect("查单条").is_none());
    // 幂等：再删一次影响 0 行（不报错）
    assert_eq!(model::soft_delete(db(), &id).await.expect("再删"), 0);
}

/// AUD-01：上传人姓名是**快照**（写入即落库、改名不回改），
/// 没有快照的历史行回落实时 join 现值 —— 两条路都不许出现 UUID。
#[tokio::test]
async fn db_uploader_name_is_snapshot_and_survives_rename() {
    let Some(_guard) = setup().await else {
        return;
    };
    let owner = uid(U_OWNER);

    // ① 生产路径：`insert_with_uploader`（handler 走的就是它）带姓名快照
    let with_snapshot = model::insert_with_uploader(model::NewAttachmentBinding {
        kind: DocKind::Contract,
        doc_id: &uid(DOC_CONTRACT),
        file_id: &uid(FILE_A),
        role: FileRole::ContractScan,
        remark: None,
        uploaded_by: Some(owner),
        uploader_name: Some("张三"),
        file_name: None,
    })
    .await
    .expect("绑定附件（带姓名快照）")
    .id;

    // ② 历史行：V0352 之前的行 / 夹具没有快照（只有 uploaded_by）
    let legacy = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_B),
        FileRole::Default,
        None,
        Some(owner),
    )
    .await
    .expect("绑定附件（无快照）");

    // ③ 用户改名：审计列的分水岭
    exec(&format!(
        "UPDATE sys_user SET real_name = '张三丰' WHERE id = '{U_OWNER}'::uuid"
    ))
    .await;

    let list = model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT), model::ListOrder::TimeDesc)
        .await
        .expect("查列表");
    let snap = list
        .iter()
        .find(|row| row.id == with_snapshot)
        .expect("带快照的行在列表里");
    assert_eq!(
        snap.uploader_name, "张三",
        "快照生效：改名后旧附件必须显示**上传当时**的名字"
    );
    let live = list.iter().find(|row| row.id == legacy).expect("历史行在列表里");
    assert_eq!(
        live.uploader_name, "张三丰",
        "没有快照的历史行回落实时 join 现值（不是 UUID、不是空白）"
    );

    // ④ 单条读（下载 / 删除路径）同样优先快照
    let row = model::find_active(&with_snapshot)
        .await
        .expect("查单条")
        .expect("应存在");
    assert_eq!(row.uploader_name, "张三");

    // 复原夹具姓名（后续用例依赖 '附件上传人'）
    exec(&format!(
        "UPDATE sys_user SET real_name = '附件上传人' WHERE id = '{U_OWNER}'::uuid"
    ))
    .await;

    println!("[AUD-01] 单据附件上传人姓名快照（写入 / 改名 / 历史行回落）✓");
}

/// **E2E07-FIX-3**：文件名是**本次上传**的快照 ——
/// 秒传复用同一个 `sys_file` 时，两条绑定各自显示自己的名字。
///
/// 复现原 bug 的最小场景（卡面）：
///
/// ```text
///   A 上传 drill-invoice.pdf（内容 X）      → sys_file.original_name = drill-invoice.pdf
///   B 上传 e2e07-contract-scan.pdf（同内容X）→ 复用同一条 sys_file
///   B 的附件列表 → 旧实现显示 drill-invoice.pdf（别人的名字）
/// ```
///
/// 断言口径：**同一个 `file_id`、两条绑定、两个不同的显示名**。
#[tokio::test]
async fn db_file_name_snapshot_survives_seconds_upload() {
    let Some(_guard) = setup().await else {
        return;
    };
    let owner = uid(U_OWNER);
    let other = uid(U_OTHER);

    // ① A 上传（写入 sys_file 的名字是夹具的「合同扫描件.png」）
    let first = model::insert_with_uploader(model::NewAttachmentBinding {
        kind: DocKind::Contract,
        doc_id: &uid(DOC_CONTRACT),
        file_id: &uid(FILE_A),
        role: FileRole::ContractScan,
        remark: None,
        uploaded_by: Some(owner),
        uploader_name: Some("附件上传人"),
        file_name: Some("drill-invoice.pdf"),
    })
    .await
    .expect("A 的绑定");
    // ② B 上传同一份内容（秒传：同一个 file_id，不同单据），名字不同
    let second = model::insert_with_uploader(model::NewAttachmentBinding {
        kind: DocKind::PurchaseOrder,
        doc_id: &uid(DOC_PO),
        file_id: &uid(FILE_A),
        role: FileRole::Default,
        remark: None,
        uploaded_by: Some(other),
        uploader_name: Some("附件旁观者"),
        file_name: Some("e2e07-contract-scan.pdf"),
    })
    .await
    .expect("B 的绑定（秒传复用）");

    // ③ 两条绑定行、同一个 sys_file（这就是「秒传」）
    assert_ne!(first.id, second.id, "秒传不是共用绑定行，是共用 sys_file");
    assert_eq!(first.file_name.as_deref(), Some("drill-invoice.pdf"));
    assert_eq!(second.file_name.as_deref(), Some("e2e07-contract-scan.pdf"));
    let file_ids = db()
        .find_with_params(
            "SELECT file_id FROM biz_doc_attachment WHERE id IN (?::uuid, ?::uuid) \
             ORDER BY file_id",
            vec![Value::from(&first.id), Value::from(&second.id)],
        )
        .await
        .expect("查 file_id");
    assert_eq!(
        file_ids.len(),
        2,
        "两条绑定都得在（否则下面的「同一 sys_file」断言没意义）"
    );
    let mut ids: Vec<String> = file_ids
        .iter()
        .map(|row| row.get_typed::<Uuid>("file_id").expect("file_id").to_string())
        .collect();
    ids.dedup();
    assert_eq!(ids, vec![FILE_A.to_string()], "秒传：两条绑定共用同一个 sys_file");

    // ④ 列表：各自显示**自己的**名字（不是 sys_file.original_name 的「合同扫描件.png」）
    let contract = model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT), model::ListOrder::TimeDesc)
        .await
        .expect("合同附件列表");
    assert_eq!(contract.len(), 1);
    assert_eq!(
        contract[0].file_name, "drill-invoice.pdf",
        "合同这侧显示 A 传的名字（绝不能是 sys_file 的名字）"
    );
    let po = model::list_by_doc(DocKind::PurchaseOrder, &uid(DOC_PO), model::ListOrder::TimeDesc)
        .await
        .expect("采购单附件列表");
    assert_eq!(po.len(), 1);
    assert_eq!(
        po[0].file_name, "e2e07-contract-scan.pdf",
        "秒传这侧必须显示 B 自己的名字（原 bug：显示 A 的 drill-invoice.pdf）"
    );

    // ⑤ 出参：DTO 直接带 file_name（前端 `DocAttachmentPanel` 读的就是它）
    let claims = claims_of(U_OWNER, &["business:erp:purchase-order:list"]);
    let item = dto::to_item(&po[0], &claims, true);
    assert_eq!(item.file_name, "e2e07-contract-scan.pdf");
    assert_eq!(item.file_id, FILE_A, "file_id 仍是复用后的那一条");

    // ⑥ 单条读（下载 / 删除路径）同样读快照
    let row = model::find_active(&second.id).await.expect("查单条").expect("应存在");
    assert_eq!(row.file_name, "e2e07-contract-scan.pdf");

    // ⑦ 幂等分支：同一张单据重复上传同一份字节 → 沿用既有行**与既有名字**，
    //    响应（handler 用返回值）与列表因此同源，不会「上传返回 X、列表显示 Y」。
    let again = model::insert_with_uploader(model::NewAttachmentBinding {
        kind: DocKind::Contract,
        doc_id: &uid(DOC_CONTRACT),
        file_id: &uid(FILE_A),
        role: FileRole::ContractScan,
        remark: None,
        uploaded_by: Some(owner),
        uploader_name: Some("附件上传人"),
        file_name: Some("第二次上传-改个名.pdf"),
    })
    .await
    .expect("重复上传（幂等）");
    assert_eq!(again.id, first.id, "同一张单据 + 同一份文件 = 同一行");
    assert_eq!(
        again.file_name.as_deref(),
        Some("drill-invoice.pdf"),
        "幂等分支不回写文件名（等于第一次那次上传），响应必须跟库里一致"
    );

    // ⑧ 历史行（`file_name` 为 NULL，V0362 之前 / 夹具）回落到 sys_file.original_name
    let legacy = model::insert(
        DocKind::Payable,
        &uid(DOC_PAYABLE),
        &uid(FILE_B),
        FileRole::Default,
        None,
        Some(owner),
    )
    .await
    .expect("历史行（无文件名快照）");
    let payable = model::list_by_doc(DocKind::Payable, &uid(DOC_PAYABLE), model::ListOrder::TimeDesc)
        .await
        .expect("应付单附件列表");
    let legacy_row = payable.iter().find(|row| row.id == legacy).expect("历史行在列表里");
    assert_eq!(
        legacy_row.file_name, "签收单.pdf",
        "没有快照的历史行回落 sys_file.original_name（界面不许空、不许 UUID）"
    );

    println!("[E2E07-FIX-3] 秒传复用下文件名各归其主（列表 / 单条 / 幂等 / 历史回落）✓");
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
        FileRole::Default,
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
    let list = model::list_by_doc(DocKind::PurchaseOrder, &uid(DOC_PO), model::ListOrder::TimeDesc)
        .await
        .expect("查列表");
    assert_eq!(list.len(), 1, "同一文件不能在同一单据上出现两行");
    // 幂等不覆盖角色：第二次的角色被忽略（行仍是首次的 DEFAULT），这是刻意的
    assert_eq!(list[0].file_role, "DEFAULT");
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

    assert_eq!(model::count_by_file(&uid(FILE_A)).await.expect("引用计数"), 2);
    model::soft_delete(db(), &a).await.expect("删一条");
    assert_eq!(
        model::count_by_file(&uid(FILE_A)).await.expect("引用计数"),
        1,
        "还有别的单据在用这份文件，计数必须仍是 1"
    );
    model::soft_delete(db(), &b).await.expect("删另一条");
    assert_eq!(model::count_by_file(&uid(FILE_A)).await.expect("引用计数"), 0);
}

/// 配额口径：`count_by_doc` 只数未删的行，且按单据隔离。
#[tokio::test]
async fn db_count_by_doc_isolation_and_quota() {
    let Some(_guard) = setup().await else {
        return;
    };
    let owner = Some(uid(U_OWNER));
    assert_eq!(
        model::count_by_doc(DocKind::StockIn, &uid(DOC_IN)).await.expect("计数"),
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
        model::count_by_doc(DocKind::StockIn, &uid(DOC_IN)).await.expect("计数"),
        1
    );
    // 别的单据 / 别的类型都不受影响
    assert_eq!(
        model::count_by_doc(DocKind::StockIn, &uid(DOC_PO)).await.expect("计数"),
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
        model::count_by_doc(DocKind::StockIn, &uid(DOC_IN)).await.expect("计数"),
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
            !kind.view_permissions().contains(&"business:erp:purchase-order:detail"),
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
    exec(&format!("DELETE FROM operation_logs WHERE id = '{log_id}'::uuid")).await;

    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", "req-att02-db".parse().expect("头值"));
    headers.insert("user-agent", "att02-db-test/1.0".parse().expect("头值"));
    headers.insert("x-real-ip", "10.9.8.7".parse().expect("头值"));
    let connect: std::net::SocketAddr = "127.0.0.1:54321".parse().expect("地址");
    let claims = claims_of(U_OWNER, &["business:erp:payable:list"]);
    let row = fixture_row();

    let mut entry = super::handler::download_log(&headers, &connect, &claims, &row, DocKind::Payable, 7);
    entry.id = log_id;
    entry.insert(db()).await.expect("下载留痕必须能写进 operation_logs");

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
    assert_eq!(sql_row.get_typed::<i32>("status_code").expect("状态码"), 200);
    assert_eq!(sql_row.get_typed::<String>("request_method").expect("方法"), "GET");
    assert_eq!(sql_row.get_typed::<String>("doc_type").expect("doc_type"), "PAYABLE");
    assert_eq!(
        sql_row.get_typed::<String>("file_name").expect("文件名"),
        "发票影像.pdf"
    );
    assert_eq!(sql_row.get_typed::<i64>("file_size").expect("大小"), 2048);
    assert_eq!(sql_row.get_typed::<String>("ip_text").expect("IP"), "10.9.8.7");
    assert_eq!(
        sql_row.get_typed::<String>("username").expect("账号"),
        format!("att_{}", &U_OWNER[..8])
    );

    exec(&format!("DELETE FROM operation_logs WHERE id = '{log_id}'::uuid")).await;
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
    assert_eq!(ListOrder::parse(Some("sort_order")), Some(ListOrder::Manual));
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
    assert!(!time_sql.contains("sort_order"), "时间口径不应读 sort_order");
    assert!(manual_sql.starts_with("ORDER BY a.sort_order ASC"), "{manual_sql}");
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
    assert!(error.msg.contains("time"), "消息要列出可选值：{}", error.msg);
    assert!(error.msg.contains("manual"), "消息要列出可选值：{}", error.msg);
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
    assert!(bad.msg.contains("not-a-uuid"), "消息要带上原文：{}", bad.msg);

    // 重复
    let dup = dto::parse_sort_request(&build(vec![FILE_A, FILE_A])).expect_err("重复 id");
    assert_eq!(dup.status, 400);
    assert!(dup.msg.contains(FILE_A), "{}", dup.msg);

    // 单据主键非法 → 400（不是 500）
    let mut bad_doc = build(vec![FILE_A]);
    bad_doc.doc_id = "123".to_string();
    let err = dto::parse_sort_request(&bad_doc).expect_err("非法 doc_id");
    assert_eq!(err.status, 400);

    // 单据类型非法 → 400（消息列出全部可选值）
    let mut bad_kind = build(vec![FILE_A]);
    bad_kind.doc_type = "BILL".to_string();
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
    let extra =
        super::handler::ensure_same_members(&active, &[a, b, c, uid(DOC_CONTRACT)]).expect_err("多余 id 必须拒绝");
    assert_eq!(extra.status, 400);
    assert!(extra.msg.contains("不属于该单据"), "{}", extra.msg);

    // 少一个（漏提）
    let missing = super::handler::ensure_same_members(&active, &[a, b]).expect_err("缺失 id 必须拒绝");
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
    let item = dto::to_item(&row, &claims, true);
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
        FileRole::InvoiceScan,
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

    let time_desc = model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT), model::ListOrder::TimeDesc)
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
    let manual_before = model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT), model::ListOrder::Manual)
        .await
        .expect("查列表");
    assert_eq!(
        manual_before.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![a, b, c],
        "手动口径按 sort_order 升序（新上传追加在末尾）"
    );
    assert_eq!(
        manual_before.iter().map(|row| row.sort_order).collect::<Vec<_>>(),
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
        FileRole::Default,
        None,
        owner,
    )
    .await
    .expect("绑 B");

    let manual = model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT), model::ListOrder::Manual)
        .await
        .expect("查列表");
    assert_eq!(
        manual.iter().map(|row| (row.id, row.sort_order)).collect::<Vec<_>>(),
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
    let manual = model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT), model::ListOrder::Manual)
        .await
        .expect("查列表");
    assert_eq!(
        manual.iter().map(|row| (row.id, row.sort_order)).collect::<Vec<_>>(),
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
        FileRole::InvoiceScan,
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
        FileRole::Default,
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
    let manual = model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT), model::ListOrder::Manual)
        .await
        .expect("查列表");
    assert_eq!(manual.iter().map(|row| row.id).collect::<Vec<_>>(), vec![c, b, a]);
    assert_eq!(
        manual.iter().map(|row| row.sort_order).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );

    // 跨单据的 id：WHERE 里带 doc_type + doc_id，命中 0 行（不会改到采购单那条）
    let crossed = model::set_sort_orders(db(), DocKind::Contract, &uid(DOC_CONTRACT), &[foreign, a, b, c])
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

/// 删除留痕（纯逻辑）：操作码 / 目标 / detail / 方法 / 路径全都要对。
///
/// 为什么这条必须有：ATT-03 在 145 实测发现删除原来走审计总线，**总线那行没落库**，
/// `operation_logs` 里只剩中间件的 `DELETE /api/business/attachment/{id}`（不带文件名）。
/// 改成直写后，字段正确性由这条用例钉住。
#[test]
fn delete_log_fields() {
    use axum::http::HeaderMap;

    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", "req-att03-del".parse().expect("头值"));
    headers.insert("user-agent", "att03-test/1.0".parse().expect("头值"));
    headers.insert("x-real-ip", "10.9.8.9".parse().expect("头值"));
    let connect: std::net::SocketAddr = "127.0.0.1:54322".parse().expect("地址");
    let claims = claims_of(U_OWNER, &["business:attachment:upload"]);
    let row = fixture_row();

    let entry = super::handler::delete_log(&headers, &connect, &claims, &row, true, 12);
    let id_text = row.id.to_string();
    let username_text = format!("att_{}", &U_OWNER[..8]);
    let path_text = format!("/api/business/attachment/{}", row.id);
    assert_eq!(entry.operation, super::handler::OP_DELETE);
    assert_eq!(entry.operation, "DOC_ATTACHMENT_DELETE");
    assert_eq!(entry.target_type.as_deref(), Some("DocAttachment"));
    assert_eq!(entry.target_id.as_deref(), Some(id_text.as_str()));
    assert_eq!(entry.request_method.as_deref(), Some("DELETE"));
    assert_eq!(entry.request_path.as_deref(), Some(path_text.as_str()));
    assert_eq!(entry.status_code, Some(200));
    assert_eq!(entry.duration_ms, Some(12));
    assert_eq!(entry.user_id, Some(uid(U_OWNER)));
    assert_eq!(entry.username.as_deref(), Some(username_text.as_str()));

    let detail = entry.detail.clone().expect("detail 必须存在");
    assert_eq!(detail["doc_type"], serde_json::json!("PAYABLE"));
    assert_eq!(detail["doc_id"], serde_json::json!(DOC_PAYABLE));
    assert_eq!(detail["file_name"], serde_json::json!("发票影像.pdf"));
    assert_eq!(detail["file_size"], serde_json::json!(2048));
    assert_eq!(detail["file_role"], serde_json::json!("INVOICE_SCAN"));
    assert_eq!(
        detail["file_deleted"],
        serde_json::json!(true),
        "detail 要记文件是否一并软删"
    );
    assert_eq!(detail["soft_delete"], serde_json::json!(true), "必须能看出这是软删");
    assert_eq!(
        entry.response_data.as_ref().map(|v| v["deleted"].clone()),
        Some(serde_json::json!(true))
    );
    assert_eq!(entry.ip, Some("10.9.8.9".parse::<std::net::IpAddr>().expect("IP")));
    println!("[ATT-03] 删除留痕字段（operation/target/detail/软删标记）✓");
}

/// 真库：删除留痕能真正落进 `operation_logs`（`ip` 是 INET、`detail` 是 JSONB）。
///
/// 单测只验模型字段；这条验写库路径 —— 绑定类型写错只在运行期报错，而留痕失败会被
/// `warn` 吞掉（不能因此让删除失败），所以必须有真库用例兜住。
#[tokio::test]
async fn db_delete_log_lands_in_operation_logs() {
    let Some(_guard) = setup().await else {
        return;
    };
    use axum::http::HeaderMap;

    let log_id = uid("d2120e02-0000-4000-8000-000000000002");
    exec(&format!("DELETE FROM operation_logs WHERE id = '{log_id}'::uuid")).await;

    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", "req-att03-db-del".parse().expect("头值"));
    headers.insert("user-agent", "att03-db-test/1.0".parse().expect("头值"));
    headers.insert("x-real-ip", "10.9.8.9".parse().expect("头值"));
    let connect: std::net::SocketAddr = "127.0.0.1:54322".parse().expect("地址");
    let claims = claims_of(U_OWNER, &["business:attachment:upload"]);
    let row = fixture_row();

    let mut entry = super::handler::delete_log(&headers, &connect, &claims, &row, false, 9);
    entry.id = log_id;
    entry.insert(db()).await.expect("删除留痕必须能写进 operation_logs");

    let rows = db()
        .find_with_params(
            "SELECT operation, target_type, target_id, status_code, request_method, \
                    detail->>'doc_type' AS doc_type, detail->>'file_name' AS file_name, \
                    (detail->>'file_size')::bigint AS file_size, \
                    (detail->>'soft_delete')::boolean AS soft_delete, \
                    host(ip) AS ip_text \
               FROM operation_logs WHERE id = ?::uuid",
            vec![Value::from(&log_id)],
        )
        .await
        .expect("读回留痕");
    let row_db = rows.first().expect("留痕必须落库");

    assert_eq!(
        row_db.get_typed::<String>("operation").expect("操作码"),
        "DOC_ATTACHMENT_DELETE"
    );
    assert_eq!(
        row_db.get_typed::<String>("target_type").expect("目标类型"),
        "DocAttachment"
    );
    assert_eq!(
        row_db.get_typed::<String>("target_id").expect("目标 id"),
        row.id.to_string()
    );
    assert_eq!(row_db.get_typed::<String>("request_method").expect("方法"), "DELETE");
    assert_eq!(row_db.get_typed::<i32>("status_code").expect("状态码"), 200);
    assert_eq!(row_db.get_typed::<String>("doc_type").expect("doc_type"), "PAYABLE");
    assert_eq!(row_db.get_typed::<String>("file_name").expect("文件名"), "发票影像.pdf");
    assert_eq!(row_db.get_typed::<i64>("file_size").expect("大小"), 2048);
    assert!(row_db.get_typed::<bool>("soft_delete").expect("软删标记"));
    assert_eq!(row_db.get_typed::<String>("ip_text").expect("IP"), "10.9.8.9");

    exec(&format!("DELETE FROM operation_logs WHERE id = '{log_id}'::uuid")).await;
    let left: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM operation_logs WHERE id = ?::uuid",
            vec![Value::from(&log_id)],
        )
        .await
        .expect("查残留");
    assert_eq!(left, 0, "夹具留痕必须清干净");
    println!("[ATT-03] 删除留痕真库落库（operation/target/detail/ip）✓");
}

/// 上传留痕的夹具事实（纯逻辑用例用，`&'static str` 便于构造）。
fn fixture_upload_facts() -> super::handler::UploadLogFacts<'static> {
    super::handler::UploadLogFacts {
        binding_id: uid("d2120e03-0000-4000-8000-000000000003"),
        doc_type: DocKind::Payable,
        doc_id: uid(DOC_PAYABLE),
        file_id: uid(FILE_B),
        file_role: FileRole::InvoiceScan,
        file_name: "发票影像.pdf",
        submitted_name: "发票影像.pdf",
        file_size: 2048,
        storage_code: Some("LOCAL"),
        duplicated: false,
    }
}

/// 上传留痕（纯逻辑）：操作码 / 目标 / detail 九个事实 / 方法 / 路径全都要对。
///
/// 为什么这条必须有：AUD-02 在 145 实测发现上传走审计总线，**总线那行没落库**，
/// `operation_logs` 里只剩中间件的 `POST /api/business/attachment/upload`（不带单据
/// 类型 / 文件名）。改成直写后，字段正确性由这条用例钉住。
///
/// 另钉一条坑：`detail` 里 **`file_name` 与 `original_name` 两个键都要有**。
/// 旧实现只有 `original_name`，ATT-04 的清理脚本按 `detail->>'file_name'` 检索时
/// 一条也查不到。
#[test]
fn upload_log_fields() {
    use axum::http::HeaderMap;

    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", "req-aud02-up".parse().expect("头值"));
    headers.insert("user-agent", "aud02-test/1.0".parse().expect("头值"));
    headers.insert("x-real-ip", "10.9.8.11".parse().expect("头值"));
    let connect: std::net::SocketAddr = "127.0.0.1:54323".parse().expect("地址");
    let claims = claims_of(U_OWNER, &["business:attachment:upload"]);
    let facts = fixture_upload_facts();

    let entry = super::handler::upload_log(&headers, &connect, &claims, &facts, 23);

    // 操作码 / 目标
    assert_eq!(entry.operation, super::handler::OP_UPLOAD);
    assert_eq!(entry.operation, "DOC_ATTACHMENT_UPLOAD");
    assert_eq!(entry.target_type.as_deref(), Some("DocAttachment"));
    assert_eq!(
        entry.target_id.as_deref(),
        Some(&*facts.binding_id.to_string()),
        "target_id 必须是绑定行 id（不是 file_id / doc_id）"
    );
    // 请求上下文
    assert_eq!(entry.request_method.as_deref(), Some("POST"));
    assert_eq!(entry.request_path.as_deref(), Some("/api/business/attachment/upload"));
    assert_eq!(entry.status_code, Some(200));
    assert_eq!(entry.duration_ms, Some(23));
    assert_eq!(entry.user_id, Some(uid(U_OWNER)));
    assert_eq!(entry.username.as_deref(), Some(&*format!("att_{}", &U_OWNER[..8])));
    assert_eq!(entry.request_id, "req-aud02-up");
    assert_eq!(entry.user_agent.as_deref(), Some("aud02-test/1.0"));
    assert_eq!(entry.ip, Some("10.9.8.11".parse::<std::net::IpAddr>().expect("IP")));
    // multipart 正文（含文件字节）不进日志
    assert!(entry.request_params.is_none());
    assert_eq!(
        entry.response_data.as_ref().map(|v| v["id"].clone()),
        Some(serde_json::json!(facts.binding_id.to_string()))
    );

    // detail：九个事实一个都不能少
    let detail = entry.detail.as_ref().expect("detail 必须存在");
    assert_eq!(detail["doc_type"], serde_json::json!("PAYABLE"));
    assert_eq!(detail["doc_id"], serde_json::json!(DOC_PAYABLE));
    assert_eq!(detail["file_id"], serde_json::json!(FILE_B));
    assert_eq!(detail["file_role"], serde_json::json!("INVOICE_SCAN"));
    assert_eq!(detail["file_name"], serde_json::json!("发票影像.pdf"));
    assert_eq!(detail["original_name"], serde_json::json!("发票影像.pdf"));
    assert_eq!(detail["file_size"], serde_json::json!(2048));
    assert_eq!(detail["storage_code"], serde_json::json!("LOCAL"));
    assert_eq!(detail["duplicated"], serde_json::json!(false));

    // 秒传：显示名是首个上传者的，original_name 是本次提交的 —— 两个键可分辨
    let duplicated_facts = super::handler::UploadLogFacts {
        submitted_name: "同名副本.pdf",
        duplicated: true,
        ..facts.clone()
    };
    let dup_entry = super::handler::upload_log(&headers, &connect, &claims, &duplicated_facts, 5);
    let dup_detail = dup_entry.detail.as_ref().expect("detail 必须存在");
    assert_eq!(dup_detail["file_name"], serde_json::json!("发票影像.pdf"));
    assert_eq!(dup_detail["original_name"], serde_json::json!("同名副本.pdf"));
    assert_eq!(dup_detail["duplicated"], serde_json::json!(true));
    println!("[AUD-02] 上传留痕字段（operation/target/九个 detail/秒传双键）✓");
}

/// 真库：上传留痕能真正落进 `operation_logs`（`ip` 是 INET、`detail` 是 JSONB）。
///
/// 单测只验模型字段；这条验写库路径 —— 绑定类型写错只在运行期报错，而留痕失败会被
/// `warn` 吞掉（不能因此让上传失败），所以必须有真库用例兜住。
#[tokio::test]
async fn db_upload_log_lands_in_operation_logs() {
    let Some(_guard) = setup().await else {
        return;
    };
    use axum::http::HeaderMap;

    let log_id = uid("d2120e03-0000-4000-8000-000000000004");
    exec(&format!("DELETE FROM operation_logs WHERE id = '{log_id}'::uuid")).await;

    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", "req-aud02-db-up".parse().expect("头值"));
    headers.insert("user-agent", "aud02-db-test/1.0".parse().expect("头值"));
    headers.insert("x-real-ip", "10.9.8.11".parse().expect("头值"));
    let connect: std::net::SocketAddr = "127.0.0.1:54323".parse().expect("地址");
    let claims = claims_of(U_OWNER, &["business:attachment:upload"]);
    let facts = fixture_upload_facts();

    let mut entry = super::handler::upload_log(&headers, &connect, &claims, &facts, 23);
    entry.id = log_id;
    entry.insert(db()).await.expect("上传留痕必须能写进 operation_logs");

    let rows = db()
        .find_with_params(
            "SELECT operation, target_type, target_id, status_code, request_method, \
                    request_path, username, detail->>'doc_type' AS doc_type, \
                    detail->>'file_name' AS file_name, \
                    detail->>'original_name' AS original_name, \
                    detail->>'storage_code' AS storage_code, \
                    (detail->>'file_size')::bigint AS file_size, \
                    (detail->>'duplicated')::boolean AS duplicated, \
                    host(ip) AS ip_text \
               FROM operation_logs WHERE id = ?::uuid",
            vec![Value::from(&log_id)],
        )
        .await
        .expect("读回留痕");
    let sql_row = rows.first().expect("留痕必须落库");

    assert_eq!(
        sql_row.get_typed::<String>("operation").expect("操作码"),
        "DOC_ATTACHMENT_UPLOAD"
    );
    assert_eq!(
        sql_row.get_typed::<String>("target_type").expect("目标类型"),
        "DocAttachment"
    );
    assert_eq!(
        sql_row.get_typed::<String>("target_id").expect("目标 id"),
        facts.binding_id.to_string()
    );
    assert_eq!(sql_row.get_typed::<String>("request_method").expect("方法"), "POST");
    assert_eq!(
        sql_row.get_typed::<String>("request_path").expect("路径"),
        "/api/business/attachment/upload"
    );
    assert_eq!(sql_row.get_typed::<i32>("status_code").expect("状态码"), 200);
    assert_eq!(
        sql_row.get_typed::<String>("username").expect("账号"),
        format!("att_{}", &U_OWNER[..8])
    );
    assert_eq!(sql_row.get_typed::<String>("doc_type").expect("doc_type"), "PAYABLE");
    // 两个键都在库里（ATT-04 清理脚本按 file_name 检索的坑）
    assert_eq!(
        sql_row.get_typed::<String>("file_name").expect("文件名"),
        "发票影像.pdf"
    );
    assert_eq!(
        sql_row.get_typed::<String>("original_name").expect("原始文件名"),
        "发票影像.pdf"
    );
    assert_eq!(sql_row.get_typed::<String>("storage_code").expect("存储后端"), "LOCAL");
    assert_eq!(sql_row.get_typed::<i64>("file_size").expect("大小"), 2048);
    assert!(!sql_row.get_typed::<bool>("duplicated").expect("秒传标记"));
    assert_eq!(sql_row.get_typed::<String>("ip_text").expect("IP"), "10.9.8.11");

    exec(&format!("DELETE FROM operation_logs WHERE id = '{log_id}'::uuid")).await;
    let left: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM operation_logs WHERE id = ?::uuid",
            vec![Value::from(&log_id)],
        )
        .await
        .expect("查残留");
    assert_eq!(left, 0, "夹具留痕必须清干净");
    println!("[AUD-02] operation_logs 真库上传留痕：operation/target/detail/ip 全部正确落库 ✓");
}

/// 仓库根（`backend/` 的上一级），用于读前端的角色词表镜像（与 `doc_kind_tests` 同款做法）。
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend/ 必须有上级目录")
        .to_path_buf()
}

/// 从前端 `attachment.ts` 里抠出 `FILE_ROLE_OPTIONS` 的 (value, label) 列表（保序）。
fn frontend_file_role_options() -> Vec<(String, String)> {
    let path = repo_root().join("frontend/src/api/business/erp/attachment.ts");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到前端角色词表 {}：{err}", path.display()));
    let start = text
        .find("export const FILE_ROLE_OPTIONS")
        .expect("前端 attachment.ts 必须有 FILE_ROLE_OPTIONS");
    let body = &text[start..];
    let end = body.find("];").expect("FILE_ROLE_OPTIONS 数组必须闭合");
    let mut rest = &body[..end];
    let mut options = Vec::new();
    while let Some(value_at) = rest.find("value: '") {
        let after = &rest[value_at + "value: '".len()..];
        let value_end = after.find('\'').expect("value 字符串必须闭合");
        let value = after[..value_end].to_string();
        let label_at = after.find("label: '").expect("每个选项必须有 label");
        let label_after = &after[label_at + "label: '".len()..];
        let label_end = label_after.find('\'').expect("label 字符串必须闭合");
        let label = label_after[..label_end].to_string();
        options.push((value, label));
        rest = &label_after[label_end..];
    }
    options
}

/// 前端上传下拉 ↔ 后端分栏词表逐条一致（ATT-04：分组标题用后端的，下拉用这份）。
///
/// 漂移后果：前端给一个后端不认的 role → 上传 400；后端加一个 role 而前端没有 →
/// 那个分组永远「暂无」。所以两边必须是同一份 6 项、同序、同文案。
#[test]
fn file_roles_match_frontend_vocabulary() {
    let expected: Vec<(String, String)> = FileRole::GROUPS
        .iter()
        .map(|role| (role.as_str().to_string(), role.label().to_string()))
        .collect();
    assert_eq!(
        frontend_file_role_options(),
        expected,
        "前端 FILE_ROLE_OPTIONS 必须与 FileRole::GROUPS（code + 中文名 + 顺序）逐字一致"
    );
}

// ---------- ATT-05：单文件 20 MB 上限 + 下载引导 ----------

/// ATT-05：上限数字与提示文案（验收③④的后端口径）。
///
/// 数字写死在单测里（不是引用常量）—— 常量改了这里必须跟着改，否则「产品说 20MB、
/// 代码写成 50MB」这种漂移没人发现。
#[test]
fn file_size_limit_is_20mb_with_exact_message() {
    assert_eq!(dto::MAX_FILE_BYTES, 20 * 1024 * 1024, "单文件上限必须是 20 MiB");
    assert_eq!(dto::MAX_FILE_BYTES, 20_971_520, "20 MiB 的十进制值");
    assert_eq!(dto::MAX_FILE_MB, 20);
    assert_eq!(dto::FILE_TOO_LARGE_MSG, "文件超过 20MB，请压缩或分卷上传");

    let error = dto::file_too_large();
    assert_eq!(error.status, 400, "卡面验收④：后端必须是 400（不是 413 / 422）");
    assert_eq!(error.code, error_code::PARAM_INVALID, "入参不合法 → 10001");
    assert_eq!(error.msg, dto::FILE_TOO_LARGE_MSG);
}

/// ATT-05：分片累加的上限判定（真库用例之前的纯逻辑闸门）。
#[test]
fn extend_within_file_limit_truth_table() {
    // 空 buffer + 空分片：不算超限
    let mut buffer: Vec<u8> = Vec::new();
    dto::extend_within_file_limit(&mut buffer, &[]).expect("空分片必须通过");
    assert!(buffer.is_empty());

    // 刚好到上限：通过（20 MB 整的文件要能传 —— 边界不能提前一天拒绝）
    let mut buffer: Vec<u8> = Vec::new();
    let full = vec![7u8; dto::MAX_FILE_BYTES as usize];
    dto::extend_within_file_limit(&mut buffer, &full).expect("正好 20 MB 必须通过");
    assert_eq!(buffer.len() as u64, dto::MAX_FILE_BYTES);

    // 再多 1 字节：拒绝，且 buffer 保持原样（不写入半份超限数据）
    let before = buffer.len();
    let error = dto::extend_within_file_limit(&mut buffer, &[0u8]).expect_err("超 1 字节必须拒");
    assert_eq!(error.status, 400);
    assert_eq!(error.msg, dto::FILE_TOO_LARGE_MSG);
    assert_eq!(buffer.len(), before, "超限分片绝不能落进内存缓冲区");

    // 多片累加：每片都不超限，但总量超限 → 在最后一片处拒绝
    let mut buffer: Vec<u8> = Vec::new();
    let chunk = vec![1u8; 8 * 1024 * 1024];
    for _ in 0..2 {
        dto::extend_within_file_limit(&mut buffer, &chunk).expect("8 MB + 8 MB 必须通过");
    }
    assert_eq!(buffer.len(), 16 * 1024 * 1024);
    // 再来 4 MB → 20 MB 整，通过；再来 1 字节 → 拒绝
    dto::extend_within_file_limit(&mut buffer, &vec![2u8; 4 * 1024 * 1024]).expect("凑满 20 MB");
    assert_eq!(buffer.len() as u64, dto::MAX_FILE_BYTES);
    assert!(dto::extend_within_file_limit(&mut buffer, &[3u8]).is_err());

    // 25 MB 单片（卡面验收④的真实场景：curl 直传）→ 直接拒
    let mut buffer: Vec<u8> = Vec::new();
    let big = vec![0u8; 25 * 1024 * 1024];
    let error = dto::extend_within_file_limit(&mut buffer, &big).expect_err("25 MB 必须拒");
    assert_eq!(error.status, 400);
    assert_eq!(error.msg, dto::FILE_TOO_LARGE_MSG);
    assert!(buffer.is_empty(), "超限文件一个字节都不留");
}

/// ATT-05：前端必须与后端同一个数字、同一句提示（漂了就在前端放行 25 MB 让后端 400 打脸）。
fn frontend_upload_pure_source() -> String {
    let path = repo_root().join("frontend/src/components/attachment-upload-pure.ts");
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到前端上限词表 {}：{err}", path.display()))
}

#[test]
fn file_size_limit_matches_frontend() {
    let source = frontend_upload_pure_source();
    assert!(
        source.contains(&format!(
            "export const MAX_ATTACHMENT_FILE_BYTES = {} * 1024 * 1024;",
            dto::MAX_FILE_MB
        )),
        "前端 MAX_ATTACHMENT_FILE_BYTES 必须是 {} * 1024 * 1024（与后端 MAX_FILE_BYTES 同一数字）",
        dto::MAX_FILE_MB
    );
    assert!(
        source.contains(&format!(
            "export const FILE_TOO_LARGE_MESSAGE = '{}';",
            dto::FILE_TOO_LARGE_MSG
        )),
        "前端超限文案必须与后端 FILE_TOO_LARGE_MSG 逐字一致（卡面：文件超过 20MB，请压缩或分卷上传）"
    );
    assert!(
        source.contains("export const OFFICE_OPEN_HINT = '已下载到文件夹，请用本地 Office/WPS 打开';"),
        "前端 Office 下载后的提示文案必须逐字为「已下载到文件夹，请用本地 Office/WPS 打开」"
    );
}

/// ATT-05 真库：`attachment` 存储策略必须放行 Office + WPS 九种扩展名。
///
/// # 为什么要在真库上验
///
/// 「Office 只下载」的前提是**能传上来**：`sys_storage_policy.allowed_extensions` 是
/// fail-closed 白名单，V0048 种的那份原本没有 wps / et / dps —— 而 ATT-05 卡面明确要求
/// 这九种扩展名都要能进、能下。V0346 补的就是这三个；本用例钉住「策略行里真的有」，
/// 防止有人回滚了迁移或改窄了白名单（改窄后前端能选 WPS 文件、上传却被拒，用户看到的是
/// 「不支持的文件类型 .wps」，很难自查）。
#[tokio::test]
async fn db_attachment_policy_allows_office_and_wps() {
    let Some(_guard) = setup().await else {
        return;
    };
    let raw: String = db()
        .scalar_with_params(
            "SELECT allowed_extensions FROM sys_storage_policy \
              WHERE category = 'attachment' AND deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("读 attachment 策略");
    let exts: Vec<&str> = raw.split(',').map(str::trim).collect();
    for ext in [
        "doc", "docx", "xls", "xlsx", "ppt", "pptx", "wps", "et", "dps", "pdf", "jpg", "png",
    ] {
        assert!(exts.contains(&ext), "attachment 策略必须放行 .{ext}（当前：{raw}）");
    }
    // 上限仍由 storage 策略给出（业务 20 MB 闸更严，见 `dto::MAX_FILE_BYTES`）
    let max_mb: i32 = db()
        .scalar_with_params(
            "SELECT max_size_mb FROM sys_storage_policy \
              WHERE category = 'attachment' AND deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("读 attachment 策略上限");
    assert!(
        max_mb >= 20,
        "策略上限（{max_mb} MB）不能比业务闸（20 MB）还小，否则 20 MB 边界传不上来"
    );
    println!("[ATT-05] attachment 策略白名单含 Office + WPS · max={max_mb} MB ✓");
}

/// ATT-05：超限后必须「先排水再报错」——直接 `?` 返回会让客户端撞 ECONNRESET。
///
/// # 这条为什么值得钉
///
/// 本卡在 145 实测踩到过：直接 `dto::extend_within_file_limit(...)?` 时，curl 能拿到 400，
/// 但「一次写完再读」的客户端（Python urllib / 部分 SDK）先撞 `ECONNRESET`，拿不到
/// 「文件超过 20MB，请压缩或分卷上传」。修法是超限后继续 `chunk()` 读完丢弃、读完再报错。
/// 这段行为在 handler 的异步读流里，纯逻辑测试覆盖不到，只能用源码契约钉住。
#[test]
fn upload_drains_body_before_reporting_too_large() {
    let src = include_str!("handler.rs");
    assert!(
        src.contains("if too_large {\n                    continue;"),
        "超限后必须继续读分片（排水），否则大文件上传会变成连接重置"
    );
    assert!(
        !src.contains("dto::extend_within_file_limit(&mut bytes, &chunk)?;"),
        "超限处不许直接 `?` 返回 —— 必须先排水再统一报 400"
    );
    assert!(
        src.contains("return Err(dto::file_too_large());"),
        "排水完之后要统一返回 400 + 卡面文案"
    );
}

/// 真库：新角色能落库（CHECK 已放开），且分组按 role 正确切分（验收①③④）。
#[tokio::test]
async fn db_role_groups_split_contract_scan_and_supplement() {
    let Some(_guard) = setup().await else {
        return;
    };
    let owner = Some(uid(U_OWNER));
    // 验收①：合同扫描件 + 补充协议；验收③：同一 role 多个文件
    let scan1 = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_A),
        FileRole::ContractScan,
        None,
        owner,
    )
    .await
    .expect("绑合同扫描件");
    let scan2 = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_B),
        FileRole::ContractScan,
        None,
        owner,
    )
    .await
    .expect("绑第二个合同扫描件");
    let supplement = model::insert(
        DocKind::Contract,
        &uid(DOC_CONTRACT),
        &uid(FILE_C),
        FileRole::Supplement,
        Some("补充条款第 3 条"),
        owner,
    )
    .await
    .expect("绑补充协议（新角色必须能被 CHECK 放行）");

    let rows = model::list_by_doc(DocKind::Contract, &uid(DOC_CONTRACT), model::ListOrder::TimeDesc)
        .await
        .expect("查列表");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows.iter().filter(|row| row.file_role == "CONTRACT_SCAN").count(), 2);
    assert!(rows.iter().any(|row| row.id == supplement));

    let claims = claims_of(U_OWNER, &[]);
    let items: Vec<dto::DocAttachmentItemRes> = rows.iter().map(|row| dto::to_item(row, &claims, true)).collect();
    let catalog = dto::role_catalog(DocKind::Contract, &items);
    assert_eq!(catalog.len(), 3, "合同的分组清单（含空分组）");
    let count = |code: &str| {
        catalog
            .iter()
            .find(|group| group.code == code)
            .unwrap_or_else(|| panic!("分组 {code} 必须存在"))
            .count
    };
    assert_eq!(count("CONTRACT_SCAN"), 2);
    assert_eq!(count("SUPPLEMENT"), 1);
    assert_eq!(count("DEFAULT"), 0, "空分组必须留着（前端画上传引导）");
    assert_eq!(catalog.iter().map(|group| group.count).sum::<u64>(), 3);
    let _ = (scan1, scan2);

    // 验收④：不写 file_role 的行必须落 DEFAULT（列默认值 = 迁移 §2）
    // 用应付单夹具 + FILE_A（合同单据上已绑过 FILE_A，撞部分唯一索引）
    exec(&format!(
        "INSERT INTO biz_doc_attachment (id, doc_type, doc_id, file_id, uploaded_by) \
         VALUES ('d2120400-0000-4000-8000-000000000001'::uuid, 'PAYABLE', \
                 '{DOC_PAYABLE}'::uuid, \
                 'd2120f01-0000-4000-8000-000000000001'::uuid, \
                 '{U_OWNER}'::uuid)"
    ))
    .await;
    let default_role: String = db()
        .scalar_with_params(
            "SELECT file_role FROM biz_doc_attachment \
              WHERE id = 'd2120400-0000-4000-8000-000000000001'::uuid",
            vec![],
        )
        .await
        .expect("读默认角色");
    assert_eq!(default_role, "DEFAULT", "不传 role 必须落 DEFAULT 组");

    // 老值已从约束里剔除（归一在 parse 侧完成，写库只会写规范值）
    let bad = db()
        .execute_with_params(
            "UPDATE biz_doc_attachment SET file_role = 'INVOICE' \
              WHERE id = 'd2120400-0000-4000-8000-000000000001'::uuid",
            vec![],
        )
        .await;
    assert!(bad.is_err(), "V0345 之后不应再能写入老值 INVOICE");

    exec(
        "DELETE FROM biz_doc_attachment \
          WHERE id = 'd2120400-0000-4000-8000-000000000001'::uuid",
    )
    .await;
    println!("[ATT-04] 新角色可落库 / 分组切分正确 / 缺省落 DEFAULT ✓");
}

// ============================================================================
// ATT-06：附件必传规则（真库）—— 卡面验收 ①②③④⑤ 各一条
// ============================================================================

/// 取出规则违例的人话；读库失败直接炸（那不是「规则不满足」）。
fn violation_text(error: AttachRuleError) -> String {
    match error {
        AttachRuleError::Violation(violations) => violations
            .iter()
            .map(|violation| violation.message())
            .collect::<Vec<_>>()
            .join("\n"),
        AttachRuleError::Db(detail) => panic!("期望规则违例，实际是读库失败：{detail}"),
    }
}

/// 给单据绑一份夹具文件（角色 DEFAULT：种子规则的 file_role 都是 NULL = 任意角色）。
async fn bind_file(doc_type: DocKind, doc: &str, file: &str) {
    model::insert(
        doc_type,
        &uid(doc),
        &uid(file),
        FileRole::Default,
        Some("ATT-06 夹具"),
        Some(uid(U_OWNER)),
    )
    .await
    .expect("绑定附件夹具");
}

/// 验收①：迁移后规则表有 8 条以上规则，且覆盖卡面点名的 8 个单据。
#[tokio::test]
async fn db_rule_seeds_cover_card_documents() {
    let Some(_guard) = setup().await else {
        return;
    };
    let total: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_attachment_rule WHERE enabled AND deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("数规则");
    assert!(total >= 8, "启用中的规则必须 ≥ 8 条，实际 {total}");

    // 卡面点名的 8 个单据 + 各自 required_at 都要有启用中的规则
    for (doc_type, at) in [
        ("INVOICE", "SUBMIT"),
        ("RECEIPT", "SUBMIT"),
        ("PAYMENT", "ACTIVATE"),
        ("EXPENSE_REPORT", "SUBMIT"),
        ("FIXED_ASSET", "ACTIVATE"),
        ("STOCK_TAKE", "CONFIRM"),
        ("PURCHASE_RETURN", "SUBMIT"),
        ("SALES_RETURN", "SUBMIT"),
    ] {
        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_attachment_rule \
                  WHERE doc_type = ? AND required_at = ? AND enabled AND deleted_at IS NULL",
                vec![Value::Text(doc_type.to_string()), Value::Text(at.to_string())],
            )
            .await
            .expect("数规则");
        assert!(count >= 1, "{doc_type} 的 {at} 规则缺失");
    }

    // 发票的两条分支：电子票 pdf/ofd/xml、纸质票 pdf/jpg/png
    let rules = att_rule::load_rules(DocKind::Invoice, RequiredAt::Submit)
        .await
        .expect("读发票规则");
    assert_eq!(rules.len(), 2, "发票提交必须有电子 / 纸质两条规则");
    let electronic = rules
        .iter()
        .find(|rule| rule.condition_sql.as_deref() == Some("is_electronic IS TRUE"))
        .expect("缺电子票规则");
    assert_eq!(
        electronic.allow_exts.as_deref(),
        Some(&["pdf".to_string(), "ofd".to_string(), "xml".to_string()][..])
    );
    assert_eq!(electronic.requirement(), "发票必须上传发票影像（pdf/ofd/xml）");
}

/// 验收②：不传附件提交发票 → 结构化错误，文案逐字含「发票必须上传发票影像（pdf/ofd/xml）」。
#[tokio::test]
async fn db_invoice_without_attachment_is_blocked_verbatim() {
    let Some(_guard) = setup().await else {
        return;
    };
    let error = att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect_err("没有附件必须被拦");
    let text = violation_text(error);
    assert!(
        text.contains("发票必须上传发票影像（pdf/ofd/xml）"),
        "文案必须逐字包含要求：{text}"
    );
    assert!(text.contains("还差 1 个"), "文案必须说清还差几个：{text}");

    // 闸门转 HTTP：400 + 10001（前端按 code 分流，不靠文案）
    let api = match att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit).await {
        Ok(()) => panic!("没有附件必须被拦"),
        Err(error) => error.into_api_error(),
    };
    assert_eq!(api.status, 400);
    assert_eq!(api.code, error_code::PARAM_INVALID);
}

/// 验收③：传了 jpg 但规则要求 pdf/ofd/xml → 400 + 明确提示格式（现有是什么、要什么）。
#[tokio::test]
async fn db_invoice_wrong_format_is_blocked_with_format_hint() {
    let Some(_guard) = setup().await else {
        return;
    };
    bind_file(DocKind::Invoice, DOC_INVOICE, FILE_D).await; // FILE_D = 发票影像.jpg
    let error = att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect_err("格式不对必须被拦");
    let text = violation_text(error);
    assert!(
        text.contains("现有附件是 jpg 格式，不在白名单内，请上传 pdf/ofd/xml 格式"),
        "文案必须点明现有格式与要求格式：{text}"
    );

    // 换一份 pdf（FILE_B = 签收单.pdf）后同样这张单能过 —— 说明拦的是格式而不是「有没有传」
    bind_file(DocKind::Invoice, DOC_INVOICE, FILE_B).await;
    att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect("有 pdf 后必须放行（jpg 那份还在，但只要求至少一个命中白名单）");
}

/// 验收④：传了正确格式 → 通过。
#[tokio::test]
async fn db_invoice_correct_format_passes() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 先确认此刻是被拦的（避免「规则根本没生效」导致的假绿）
    assert!(
        att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
            .await
            .is_err()
    );
    bind_file(DocKind::Invoice, DOC_INVOICE, FILE_B).await; // pdf
    att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect("pdf 必须通过");
}

/// 验收⑤：规则表改 `enabled = false` → 该规则不再校验（**不重启服务**），改回来又重新校验。
#[tokio::test]
async fn db_rule_disabled_takes_effect_without_restart() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 关掉电子票那条规则（发票只有这一条会命中数电票夹具）
    let affected = db()
        .execute_with_params(
            "UPDATE biz_attachment_rule SET enabled = FALSE, updated_at = now() \
              WHERE doc_type = 'INVOICE' AND condition_sql = 'is_electronic IS TRUE'",
            vec![],
        )
        .await
        .expect("关规则");
    assert_eq!(affected, 1, "必须正好关掉一条规则");

    // 同一个进程、没有任何重启：立刻放行
    att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect("规则停用后必须放行");

    // 再打开：立刻又拦（证明是每次读表，不是进程内缓存）
    db().execute_with_params(
        "UPDATE biz_attachment_rule SET enabled = TRUE, updated_at = now() \
          WHERE doc_type = 'INVOICE' AND condition_sql = 'is_electronic IS TRUE'",
        vec![],
    )
    .await
    .expect("开规则");
    assert!(
        att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
            .await
            .is_err(),
        "规则重新启用后必须再次拦截"
    );
}

/// `condition_sql` 分支：纸质票（`is_electronic = FALSE`）只命中 pdf/jpg/png 那条，jpg 直接通过。
#[tokio::test]
async fn db_paper_invoice_uses_scan_rule_branch() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 纸质票没附件：文案是「纸质发票扫描件」，而不是电子票的 pdf/ofd/xml
    let error = att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE_PAPER), RequiredAt::Submit)
        .await
        .expect_err("纸质票没附件也要拦");
    let text = violation_text(error);
    assert!(
        text.contains("发票必须上传纸质发票扫描件（pdf/jpg/png）"),
        "纸质票分支文案不对：{text}"
    );

    // 纸质票传 jpg：通过（电子票分支对它是「不适用」，不是「格式错」）
    bind_file(DocKind::Invoice, DOC_INVOICE_PAPER, FILE_D).await;
    att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE_PAPER), RequiredAt::Submit)
        .await
        .expect("纸质票 jpg 扫描件必须通过");

    // 反过来：电子票挂 jpg 仍然被拦（分支互斥，不是「任一分支满足即可」）
    bind_file(DocKind::Invoice, DOC_INVOICE, FILE_D).await;
    assert!(
        att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
            .await
            .is_err()
    );
}

/// 卡面 ATT-07 验收③④：同一张发票、同一份 jpg 附件，**票面形式一改口径就跟着改** ——
/// 证明分支读的是 `is_electronic` 列本身，而不是附件内容或历史 `invoice_code`。
#[tokio::test]
async fn db_invoice_branch_follows_is_electronic_column() {
    let Some(_guard) = setup().await else {
        return;
    };
    // ③ 电子票（fixture: is_electronic = TRUE）+ jpg → 拦（要 pdf/ofd/xml）
    bind_file(DocKind::Invoice, DOC_INVOICE, FILE_D).await;
    let error = att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect_err("电子票挂 jpg 必须拦");
    let text = violation_text(error);
    assert!(
        text.contains("发票必须上传发票影像（pdf/ofd/xml）"),
        "电子票分支文案不对：{text}"
    );
    assert!(text.contains("现有附件是 jpg 格式"), "必须点明现有格式：{text}");

    // ③' 电子票 + pdf → 通过
    bind_file(DocKind::Invoice, DOC_INVOICE, FILE_B).await;
    att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect("电子票挂 pdf 必须通过");

    // ④ 同一张单改成纸质票（附件不动）→ 同一份 jpg 现在通过
    db().execute_with_params(
        "UPDATE biz_invoice SET is_electronic = FALSE WHERE id = ?::uuid",
        vec![Value::from(&uid(DOC_INVOICE))],
    )
    .await
    .expect("改票面形式为纸质票");
    att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect("纸质票挂 jpg 必须通过");

    // 改回电子票（先摘掉那份 pdf，否则电子票规则被 pdf 满足，测不出分支已切回）
    exec(
        "DELETE FROM biz_doc_attachment WHERE doc_type = 'INVOICE' AND doc_id = \
         'd2120106-0000-4000-8000-000000000001'::uuid AND file_id = \
         'd2120f02-0000-4000-8000-000000000002'::uuid",
    )
    .await;
    db().execute_with_params(
        "UPDATE biz_invoice SET is_electronic = TRUE WHERE id = ?::uuid",
        vec![Value::from(&uid(DOC_INVOICE))],
    )
    .await
    .expect("改回电子票");
    let error = att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect_err("改回电子票后（只剩 jpg）必须再次被拦");
    assert!(violation_text(error).contains("格式，不在白名单内"));
}

/// 没配规则的（动作, 单据）组合直接放行；动作不匹配的规则不参与校验。
#[tokio::test]
async fn db_rule_scope_is_doc_type_and_action() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 合同没有配任何必传规则 → 放行
    att_rule::assert_attachments(DocKind::Contract, uid(DOC_CONTRACT), RequiredAt::Submit)
        .await
        .expect("合同没有规则必须放行");
    assert!(
        att_rule::load_rules(DocKind::Contract, RequiredAt::Submit)
            .await
            .expect("读规则")
            .is_empty()
    );
    // 发票的规则是 SUBMIT：ACTIVATE 动作下一条都不加载
    assert!(
        att_rule::load_rules(DocKind::Invoice, RequiredAt::Activate)
            .await
            .expect("读规则")
            .is_empty()
    );
    att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Activate)
        .await
        .expect("动作不匹配必须放行");
}

/// 卡面 ATT-08 验收①：发票没附件 → `check` 返回 `can_proceed = false` + 结构化缺口；
/// 绑上 pdf 后同一个调用立刻变成 `can_proceed = true`（验收②的接口侧）。
///
/// 关键断言是「check 与闸门同一条判据」：两次对比 `assert_attachments` 的结论，
/// 任一时刻两者必须一致，否则前端会亮着按钮让用户撞 400。
#[tokio::test]
async fn db_check_reports_invoice_gap_then_clears_it() {
    let Some(_guard) = setup().await else {
        return;
    };
    clear_doc_attachments(DocKind::Invoice, DOC_INVOICE).await;

    // ① 没附件：缺口一条，字段与红字都符合卡面契约
    let violations = att_rule::check_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect("查缺口");
    let res = dto::to_check_res(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit, &violations);
    assert!(!res.can_proceed, "没有发票影像时按钮必须禁用");
    assert_eq!(res.missing.len(), 1, "数电票只命中电子票一条规则");
    let missing = &res.missing[0];
    assert_eq!(missing.need.as_deref(), Some("pdf/ofd/xml"));
    assert_eq!((missing.have, missing.min), (0, 1));
    assert_eq!(missing.hint, "缺少发票影像（需 pdf/ofd/xml 格式）");
    assert_eq!(res.hint.as_deref(), Some("缺少发票影像（需 pdf/ofd/xml 格式）"));
    assert!(
        missing.message.contains("还差 1 个"),
        "tooltip 必须说清还差几个：{}",
        missing.message
    );
    // 同一时刻闸门也拦 → 前端提前告知与后端最终拦完全一致
    let gate = att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit).await;
    assert!(gate.is_err(), "check 说不通过时闸门必须也拦");

    // ② 传一份 pdf：can_proceed 立刻为 true、红字消失、闸门放行
    bind_file(DocKind::Invoice, DOC_INVOICE, FILE_B).await;
    let violations = att_rule::check_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect("查缺口");
    let res = dto::to_check_res(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit, &violations);
    assert!(res.can_proceed, "有 pdf 后按钮必须恢复");
    assert!(res.missing.is_empty());
    assert!(res.hint.is_none());
    att_rule::assert_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect("有 pdf 后闸门必须放行");
}

/// 卡面 ATT-08 验收③：上传错误格式（jpg）→ 红字必须说「格式不符，需要 pdf/ofd/xml」。
#[tokio::test]
async fn db_check_reports_wrong_format_hint() {
    let Some(_guard) = setup().await else {
        return;
    };
    clear_doc_attachments(DocKind::Invoice, DOC_INVOICE).await;
    bind_file(DocKind::Invoice, DOC_INVOICE, FILE_D).await; // FILE_D = 发票影像.jpg

    let violations = att_rule::check_attachments(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit)
        .await
        .expect("查缺口");
    let res = dto::to_check_res(DocKind::Invoice, uid(DOC_INVOICE), RequiredAt::Submit, &violations);
    assert!(!res.can_proceed, "jpg 不算数，按钮仍要禁用");
    let missing = &res.missing[0];
    assert_eq!((missing.have, missing.total), (0, 1), "有 1 个附件但 0 个命中白名单");
    assert_eq!(missing.actual_exts, vec!["jpg".to_string()]);
    assert!(
        missing.hint.contains("格式不符，需要 pdf/ofd/xml"),
        "红字必须点名格式：{}",
        missing.hint
    );
    assert!(
        missing.hint.contains("当前 jpg"),
        "红字要说出当前格式：{}",
        missing.hint
    );
}

/// 纸质票分支：同一个 `check` 接口按 `is_electronic` 给出**不同**的格式要求。
#[tokio::test]
async fn db_check_follows_electronic_branch() {
    let Some(_guard) = setup().await else {
        return;
    };
    clear_doc_attachments(DocKind::Invoice, DOC_INVOICE_PAPER).await;
    let violations = att_rule::check_attachments(DocKind::Invoice, uid(DOC_INVOICE_PAPER), RequiredAt::Submit)
        .await
        .expect("查缺口");
    let res = dto::to_check_res(
        DocKind::Invoice,
        uid(DOC_INVOICE_PAPER),
        RequiredAt::Submit,
        &violations,
    );
    assert!(!res.can_proceed);
    let missing = &res.missing[0];
    assert_eq!(missing.need.as_deref(), Some("pdf/jpg/png"));
    assert_eq!(missing.role_label, "纸质发票扫描件");
    assert_eq!(missing.hint, "缺少纸质发票扫描件（需 pdf/jpg/png 格式）");

    // 纸质票传 jpg → 放行（电子票那条规则对它「不适用」，不是「格式错」）
    bind_file(DocKind::Invoice, DOC_INVOICE_PAPER, FILE_D).await;
    let violations = att_rule::check_attachments(DocKind::Invoice, uid(DOC_INVOICE_PAPER), RequiredAt::Submit)
        .await
        .expect("查缺口");
    assert!(violations.is_empty(), "纸质票 jpg 扫描件必须放行");
}

/// 没配规则的组合（合同）→ `check` 直接通过（不制造假缺口）。
#[tokio::test]
async fn db_check_passes_when_no_rule() {
    let Some(_guard) = setup().await else {
        return;
    };
    let violations = att_rule::check_attachments(DocKind::Contract, uid(DOC_CONTRACT), RequiredAt::Submit)
        .await
        .expect("查缺口");
    assert!(violations.is_empty(), "合同没有必传规则，不该报缺");
    let res = dto::to_check_res(DocKind::Contract, uid(DOC_CONTRACT), RequiredAt::Submit, &violations);
    assert!(res.can_proceed);
}

/// 清掉某张单据的全部附件绑定（真库用例串行共用夹具，前置状态必须自己收敛）。
async fn clear_doc_attachments(doc_type: DocKind, doc: &str) {
    exec(&format!(
        "DELETE FROM biz_doc_attachment WHERE doc_type = '{}' AND doc_id = '{doc}'::uuid",
        doc_type.as_str()
    ))
    .await;
}

/// 收尾：清夹具（必须最后跑，`zz_` 前缀保证顺序）。
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
                 'd2120105-0000-4000-8000-000000000001'::uuid, \
                 'd2120106-0000-4000-8000-000000000001'::uuid, \
                 'd2120107-0000-4000-8000-000000000001'::uuid)",
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
                 'd2120f03-0000-4000-8000-000000000003'::uuid, \
                 'd2120f04-0000-4000-8000-000000000004'::uuid)",
            vec![],
        )
        .await
        .expect("查残留");
    assert_eq!(files, 0, "夹具 sys_file 必须清零");
}
