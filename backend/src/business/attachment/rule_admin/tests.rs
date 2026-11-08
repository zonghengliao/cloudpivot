// ============================================================================
// tests.rs —— 附件必传规则**管理面**用例（ATT-09）
//
// 两类用例：
//   · **纯逻辑**（不连库）：入参归一 / 校验（doc_type / required_at / file_role /
//     数量区间 / allow_exts / condition_sql / remark）、超管判定、行 → 出参、
//     下拉词表与枚举同源；
//   · **真库**（`ERP_DB_URL` 开关，与其它 ERP 用例同口径）：新建 → 列表可见 →
//     同键 409 → 启停即时生效（闸门 `rule::load_rules` 立刻看不到）→ 编辑改
//     allow_exts → 软删后同键可重建。
//
// ⚠ `ERP_DB_URL` 未设置时真库用例 SKIP（假绿）。交付前必须真跑：
//
// ```bash
// ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//   cargo test --lib business::attachment::rule_admin -- --test-threads=1
// ```
//
// # 夹具口径
//
// | 对象 | 值 |
// |------|-----|
// | 规则键 | `SALES_RETURN` + `ACTIVATE` + `DEFAULT` + 无条件（种子 9 条里没有这一组） |
// | 清理 | 按上面四元组物理删，并复原 test 期间被改过的 `enabled` |
//
// **串行**（`--test-threads=1`）：真库用例共用全局 `db()` 与同一批夹具。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use super::dto::{self, RuleSaveReq};
use super::model::{self, RuleRow};
use crate::business::attachment::model::{DocKind, FileRole};
use crate::business::attachment::rule::{self as att_rule, RequiredAt};
use crate::shared::constants::SUPER_ADMIN_ROLE_CODE;
use crate::shared::error_code;

// ---------- 纯逻辑 ----------

/// 一个合法请求的最小骨架（各用例只改自己关心的字段）。
fn valid_req() -> RuleSaveReq {
    RuleSaveReq {
        doc_type: "INVOICE".to_string(),
        file_role: None,
        required_at: "SUBMIT".to_string(),
        min_count: None,
        max_count: None,
        allow_exts: Some("pdf,ofd,xml".to_string()),
        condition_sql: None,
        remark: Some("发票影像".to_string()),
        enabled: None,
    }
}

fn claims_of(roles: &[&str], permissions: &[&str]) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: "d2120901-0000-4000-8000-000000000001".to_string(),
        username: Some("att09_admin".to_string()),
        roles: roles.iter().map(|role| (*role).to_string()).collect(),
        permissions: permissions.iter().map(|code| (*code).to_string()).collect(),
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
}

/// 合法入参：全部字段归一（大小写 / 空白 / 点号前缀 / 缺省值）。
#[test]
fn validate_normalizes_every_field() {
    let req = RuleSaveReq {
        doc_type: " invoice ".to_string(),
        file_role: Some(" bank_receipt ".to_string()),
        required_at: " activate ".to_string(),
        min_count: None,
        max_count: Some(3),
        allow_exts: Some(" PDF, .Ofd ,pdf，， JPG ".to_string()),
        condition_sql: Some("  invoice_code IS NULL  ".to_string()),
        remark: Some("  发票影像  ".to_string()),
        enabled: None,
    };
    let draft = dto::validate(&req).expect("合法入参必须通过");
    assert_eq!(draft.doc_type, DocKind::Invoice);
    assert_eq!(draft.file_role.as_deref(), Some("BANK_RECEIPT"));
    assert_eq!(draft.required_at, RequiredAt::Activate);
    assert_eq!(draft.min_count, 1, "min_count 缺省 = 1");
    assert_eq!(draft.max_count, Some(3));
    assert_eq!(
        draft.allow_exts.as_deref(),
        Some("pdf,ofd,jpg"),
        "小写、去重、保序、去点号前缀、中文逗号也认"
    );
    assert_eq!(draft.condition_sql.as_deref(), Some("invoice_code IS NULL"));
    assert_eq!(draft.remark.as_deref(), Some("发票影像"));
    assert!(draft.enabled, "enabled 缺省 = true");
}

/// 非法 `doc_type` / `required_at`：400 且**点名**合法取值（不能让人猜）。
#[test]
fn validate_rejects_unknown_enums_with_readable_message() {
    let mut req = valid_req();
    req.doc_type = "BILL".to_string();
    let err = dto::validate(&req).expect_err("未知单据类型必须被拒");
    assert_eq!(err.status, 400);
    assert_eq!(err.code, error_code::PARAM_INVALID);
    assert!(err.msg.contains("doc_type"), "{}", err.msg);
    assert!(
        err.msg.contains("INVOICE"),
        "消息里要列出可选值：{}",
        err.msg
    );

    let mut req = valid_req();
    req.required_at = "APPROVE".to_string();
    let err = dto::validate(&req).expect_err("未知动作必须被拒");
    assert_eq!(err.status, 400);
    assert!(
        err.msg.contains("SUBMIT"),
        "消息里要列出可选值：{}",
        err.msg
    );
}

/// 数量区间：`min_count = 0`（永不校验）与 `max_count < min_count`（永不成立）都必须拒。
#[test]
fn validate_rejects_impossible_counts() {
    let mut req = valid_req();
    req.min_count = Some(0);
    let err = dto::validate(&req).expect_err("min_count = 0 必须被拒");
    assert_eq!(err.status, 400);
    assert!(err.msg.contains("min_count"), "{}", err.msg);

    let mut req = valid_req();
    req.min_count = Some(3);
    req.max_count = Some(2);
    let err = dto::validate(&req).expect_err("max < min 必须被拒");
    assert!(err.msg.contains("max_count"), "{}", err.msg);

    let mut req = valid_req();
    req.max_count = Some(0);
    let err = dto::validate(&req).expect_err("max_count = 0 必须被拒");
    assert!(err.msg.contains("max_count"), "{}", err.msg);

    // 相等是合法的（「恰好 N 个」）
    let mut req = valid_req();
    req.min_count = Some(2);
    req.max_count = Some(2);
    dto::validate(&req).expect("min == max 是合法区间");
}

/// `allow_exts` 的三种坏写法：只有分隔符 / 通配符 / 超长 token。
#[test]
fn parse_allow_exts_rejects_junk() {
    assert_eq!(dto::parse_allow_exts(None).unwrap(), None);
    assert_eq!(dto::parse_allow_exts(Some("   ")).unwrap(), None);

    let err = dto::parse_allow_exts(Some(" , ， ")).expect_err("只有分隔符必须被拒");
    assert_eq!(err.status, 400);

    let err = dto::parse_allow_exts(Some("pdf,*")).expect_err("通配符必须被拒");
    assert!(err.msg.contains('*'), "{}", err.msg);

    let err = dto::parse_allow_exts(Some("pdf,p df")).expect_err("空格 token 必须被拒");
    assert_eq!(err.status, 400);

    let long = "a".repeat(dto::EXT_TOKEN_MAX_CHARS + 1);
    let err = dto::parse_allow_exts(Some(&format!("pdf,{long}"))).expect_err("超长 token 必须被拒");
    assert_eq!(err.status, 400);

    // 上限 20 个
    let many = (0..=dto::EXT_TOKEN_MAX_COUNT)
        .map(|i| format!("e{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let err = dto::parse_allow_exts(Some(&many)).expect_err("超过 20 个必须被拒");
    assert!(err.msg.contains("20"), "{}", err.msg);
}

/// `file_role`：空 → 任意；老值归一；未知值 400。
#[test]
fn parse_file_role_normalizes_and_rejects() {
    assert_eq!(dto::parse_file_role(None).unwrap(), None);
    assert_eq!(dto::parse_file_role(Some("   ")).unwrap(), None);
    assert_eq!(
        dto::parse_file_role(Some("invoicE")).unwrap().as_deref(),
        Some("INVOICE_SCAN"),
        "老值 INVOICE 归一为 INVOICE_SCAN（ATT-04 口径）"
    );
    let err = dto::parse_file_role(Some("SCAN")).expect_err("未知角色必须被拒");
    assert_eq!(err.status, 400);
    assert!(
        err.msg.contains("DEFAULT"),
        "消息里要列出可选值：{}",
        err.msg
    );
}

/// `condition_sql`：注入片段（`;` / `--` / `/*`）与超长都必须 400。
#[test]
fn parse_condition_blocks_dangerous_fragments() {
    assert_eq!(dto::parse_condition(None).unwrap(), None);
    for bad in [
        "invoice_code IS NULL; DROP TABLE x",
        "1=1 -- 注释",
        "1=1 /* 注释 */",
        "status = ?",
    ] {
        let err = dto::parse_condition(Some(bad)).expect_err("危险片段必须被拒");
        assert_eq!(err.status, 400, "{bad}");
    }
    let long = "x".repeat(dto::CONDITION_MAX_CHARS + 1);
    let err = dto::parse_condition(Some(&long)).expect_err("超长必须被拒");
    assert!(err.msg.contains("condition_sql"), "{}", err.msg);
}

/// `remark` 超列宽 → 400（否则 PG 报 500，用户只看到「服务器错误」）。
#[test]
fn validate_rejects_overlong_remark() {
    let mut req = valid_req();
    req.remark = Some("备".repeat(dto::REMARK_MAX_CHARS + 1));
    let err = dto::validate(&req).expect_err("超长 remark 必须被拒");
    assert_eq!(err.status, 400);
    assert!(err.msg.contains("remark"), "{}", err.msg);
}

/// 超管判定：角色短路 / 通配权限都认；普通用户 403 且说明为什么。
#[test]
fn ensure_super_admin_accepts_role_or_wildcard() {
    dto::ensure_super_admin(&claims_of(&[SUPER_ADMIN_ROLE_CODE], &[]), "编辑")
        .expect("超管角色必须通过");
    dto::ensure_super_admin(&claims_of(&["user"], &["*:*:*"]), "编辑").expect("通配权限必须通过");
    dto::ensure_super_admin(&claims_of(&["user"], &["*"]), "编辑").expect("通配 * 必须通过");

    let err = dto::ensure_super_admin(
        &claims_of(&["AUDITOR"], &[dto::PERM_LIST, dto::PERM_CREATE]),
        "编辑",
    )
    .expect_err("非超管必须 403");
    assert_eq!(err.status, 403);
    assert_eq!(err.code, error_code::PERMISSION_DENIED);
    assert!(err.msg.contains(SUPER_ADMIN_ROLE_CODE), "{}", err.msg);
}

/// 行 → 出参：标签、`allow_exts_list`、`requirement` 文案全部与闸门同源。
#[test]
fn to_item_mirrors_gate_requirement_text() {
    let now = chrono::Utc::now();
    let row = RuleRow {
        id: Uuid::now_v7(),
        doc_type: DocKind::Invoice,
        file_role: Some("INVOICE_SCAN".to_string()),
        required_at: RequiredAt::Submit,
        min_count: 1,
        max_count: None,
        allow_exts: Some("pdf,ofd,xml".to_string()),
        condition_sql: None,
        remark: Some("发票影像".to_string()),
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    let item = dto::to_item(&row);
    assert_eq!(item.doc_type_label, "发票");
    assert_eq!(item.file_role_label.as_deref(), Some("发票影像"));
    assert_eq!(item.required_at_label, "提交");
    assert_eq!(item.allow_exts_list, vec!["pdf", "ofd", "xml"]);
    assert_eq!(
        item.requirement, "发票必须上传发票影像（pdf/ofd/xml）",
        "页面显示的文案必须与闸门 400 报文第一段逐字一致"
    );

    // 序列化契约：前端拿到的就是这些字段（Uuid 已转 String，不靠框架 impl）
    let json = serde_json::to_value(&item).expect("出参必须可序列化");
    assert_eq!(json["doc_type"], "INVOICE");
    assert_eq!(json["allow_exts_list"][0], "pdf");
    assert!(json["id"].is_string(), "id 必须是字符串");
    assert_eq!(json["file_role"], "INVOICE_SCAN");

    // 留空 = 任意角色：标签为空，文案回落「附件」
    let row = RuleRow {
        file_role: None,
        allow_exts: None,
        remark: None,
        ..row
    };
    let item = dto::to_item(&row);
    assert_eq!(item.file_role_label, None);
    assert!(item.allow_exts_list.is_empty());
    assert_eq!(item.requirement, "发票必须上传附件");
}

/// 下拉词表：12 / 3 / 7，与枚举逐字同源（前端不再抄第二份）。
#[test]
fn options_come_from_enums() {
    let options = dto::options();
    assert_eq!(options.doc_types.len(), DocKind::ALL.len());
    assert_eq!(options.required_ats.len(), RequiredAt::ALL.len());
    assert_eq!(options.file_roles.len(), FileRole::ALL.len());

    let values: Vec<&str> = options
        .doc_types
        .iter()
        .map(|option| option.value.as_str())
        .collect();
    assert_eq!(
        values,
        DocKind::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
    );
    let ats: Vec<&str> = options
        .required_ats
        .iter()
        .map(|option| option.value.as_str())
        .collect();
    assert_eq!(ats, vec!["SUBMIT", "CONFIRM", "ACTIVATE"]);
    for option in &options.file_roles {
        assert!(
            FileRole::parse(Some(&option.value)).is_some(),
            "{} 必须在 FileRole 词表内",
            option.value
        );
        assert!(!option.label.is_empty());
    }
}

/// 权限码常量必须与迁移 / `#[nik::permission]` 的写法逐字一致。
#[test]
fn permission_codes_are_stable() {
    assert_eq!(dto::PERM_LIST, "support:attachment-rule:list");
    assert_eq!(dto::PERM_CREATE, "support:attachment-rule:create");
    assert_eq!(dto::PERM_UPDATE, "support:attachment-rule:update");
    assert_eq!(dto::PERM_DELETE, "support:attachment-rule:delete");
    // 路由宏里的字面值必须与常量相同（两条路走的是同一份权限点）
    let source = include_str!("handler.rs");
    for code in [
        dto::PERM_LIST,
        dto::PERM_CREATE,
        dto::PERM_UPDATE,
        dto::PERM_DELETE,
    ] {
        assert!(
            source.contains(&format!("permission = \"{code}\"")),
            "handler.rs 里缺少 permission = \"{code}\""
        );
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

/// 清本文件造的规则（四元组与种子 9 条不重叠）。
async fn purge_fixtures() {
    exec(
        "DELETE FROM biz_attachment_rule \
          WHERE doc_type = 'SALES_RETURN' AND required_at = 'ACTIVATE' \
            AND COALESCE(file_role, '') = 'DEFAULT'",
    )
    .await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    purge_fixtures().await;
    Some(guard)
}

/// 管理面全链路：新建 → 列表 → 同键 409 → 启停即时生效 → 编辑 → 软删 → 可重建。
///
/// 卡面验收②③④ 的后端半边都在这一条里：**改完下一次校验就按新状态走**。
#[tokio::test]
async fn admin_crud_roundtrip_takes_effect_immediately() {
    let Some(_guard) = setup().await else {
        return;
    };
    let claims = claims_of(&[SUPER_ADMIN_ROLE_CODE], &["*:*:*"]);
    let req = RuleSaveReq {
        doc_type: "SALES_RETURN".to_string(),
        file_role: Some("DEFAULT".to_string()),
        required_at: "ACTIVATE".to_string(),
        min_count: Some(1),
        max_count: None,
        allow_exts: Some("PDF,JPG".to_string()),
        condition_sql: None,
        remark: Some("ATT-09 用例规则".to_string()),
        enabled: None,
    };

    // ① 新建：归一后落库
    let draft = dto::validate(&req).expect("合法入参");
    let id = model::insert(&claims, &draft).await.expect("新建必须成功");
    let row = model::find(&id)
        .await
        .expect("查询不能报错")
        .expect("刚插的行必须能读回");
    assert_eq!(row.doc_type, DocKind::SalesReturn);
    assert_eq!(row.required_at, RequiredAt::Activate);
    assert_eq!(row.file_role.as_deref(), Some("DEFAULT"));
    assert_eq!(row.allow_exts.as_deref(), Some("pdf,jpg"));
    assert!(row.enabled);

    // ② 列表可见（管理面含禁用行，但这里是启用态）
    let list = model::list_all().await.expect("列表必须可读");
    assert!(
        list.iter().any(|item| item.id == id),
        "新建的规则必须出现在列表里"
    );

    // ③ 同键再建：闸门前置查询必须把它认出来（handler 转 409 的依据）
    assert_eq!(
        model::find_conflict(&draft, None)
            .await
            .expect("查冲突不能报错"),
        Some(id),
        "同键必须被识别为冲突"
    );
    // 编辑自己不算冲突
    assert_eq!(
        model::find_conflict(&draft, Some(&id))
            .await
            .expect("查冲突不能报错"),
        None
    );

    // ④ 闸门读到它（新建即生效）
    let gated = att_rule::load_rules(DocKind::SalesReturn, RequiredAt::Activate)
        .await
        .expect("闸门读规则不能报错");
    assert!(
        gated.iter().any(|rule| rule.id == id),
        "新建规则必须参与校验"
    );

    // ⑤ 禁用 → 闸门立刻看不到（**无缓存**，卡面验收③）
    assert_eq!(
        model::set_enabled(&claims, &id, false)
            .await
            .expect("禁用必须成功"),
        1
    );
    let gated = att_rule::load_rules(DocKind::SalesReturn, RequiredAt::Activate)
        .await
        .expect("闸门读规则不能报错");
    assert!(
        !gated.iter().any(|rule| rule.id == id),
        "禁用后的规则必须立刻不参与校验"
    );
    // 管理面仍然看得到（否则「配过什么」不可追）
    let list = model::list_all().await.expect("列表必须可读");
    let shown = list
        .iter()
        .find(|item| item.id == id)
        .expect("禁用行必须仍在管理列表里");
    assert!(!shown.enabled);

    // ⑥ 重新启用 → 又参与校验
    assert_eq!(
        model::set_enabled(&claims, &id, true)
            .await
            .expect("启用必须成功"),
        1
    );
    let gated = att_rule::load_rules(DocKind::SalesReturn, RequiredAt::Activate)
        .await
        .expect("闸门读规则不能报错");
    assert!(gated.iter().any(|rule| rule.id == id), "启用后必须恢复校验");

    // ⑦ 编辑改 allow_exts → 闸门读到的白名单立刻变（卡面验收④）
    let edited = RuleSaveReq {
        allow_exts: Some("png".to_string()),
        remark: Some("改成只收 png".to_string()),
        ..req.clone()
    };
    let edited_draft = dto::validate(&edited).expect("合法入参");
    assert_eq!(
        model::update(&claims, &id, &edited_draft)
            .await
            .expect("编辑必须成功"),
        1
    );
    let gated = att_rule::load_rules(DocKind::SalesReturn, RequiredAt::Activate)
        .await
        .expect("闸门读规则不能报错");
    let current = gated
        .iter()
        .find(|rule| rule.id == id)
        .expect("编辑后规则仍在闸门里");
    assert_eq!(
        current.allow_exts.as_deref(),
        Some(["png".to_string()].as_slice()),
        "闸门必须读到新的扩展名白名单"
    );
    assert_eq!(current.requirement_name(), "改成只收 png");

    // ⑧ 软删 → 管理面 / 闸门都看不到，同键可以重建（唯一索引带 WHERE deleted_at IS NULL）
    assert_eq!(
        model::soft_delete(&claims, &id)
            .await
            .expect("软删必须成功"),
        1
    );
    assert!(model::find(&id).await.expect("查询不能报错").is_none());
    assert_eq!(
        model::soft_delete(&claims, &id)
            .await
            .expect("重复软删不能报错"),
        0,
        "已删的行再删影响 0 行（handler 据此回 404）"
    );
    let gated = att_rule::load_rules(DocKind::SalesReturn, RequiredAt::Activate)
        .await
        .expect("闸门读规则不能报错");
    assert!(!gated.iter().any(|rule| rule.id == id));
    assert_eq!(
        model::find_conflict(&draft, None)
            .await
            .expect("查冲突不能报错"),
        None,
        "软删后同键不再算冲突"
    );
    let rebuilt = model::insert(&claims, &draft)
        .await
        .expect("同键重建必须成功");
    assert_ne!(rebuilt, id, "重建是一条新行");

    purge_fixtures().await;
}

/// 并发窗口：同键第二次 `insert` 撞唯一索引 → `UniqueViolation`（handler 转 409）。
#[tokio::test]
async fn duplicate_insert_is_unique_violation() {
    let Some(_guard) = setup().await else {
        return;
    };
    let claims = claims_of(&[SUPER_ADMIN_ROLE_CODE], &["*:*:*"]);
    let req = RuleSaveReq {
        doc_type: "SALES_RETURN".to_string(),
        file_role: Some("DEFAULT".to_string()),
        required_at: "ACTIVATE".to_string(),
        min_count: None,
        max_count: None,
        allow_exts: None,
        condition_sql: None,
        remark: None,
        enabled: Some(false),
    };
    let draft = dto::validate(&req).expect("合法入参");
    let first = model::insert(&claims, &draft).await.expect("首插必须成功");
    let err = model::insert(&claims, &draft)
        .await
        .expect_err("同键第二次插入必须失败");
    assert!(
        matches!(err, DbError::UniqueViolation { .. }),
        "必须是唯一冲突（判据用变体，handler 再转 409）：{err:?}"
    );
    // 关掉再读：禁用行也不能被闸门读到
    let gated = att_rule::load_rules(DocKind::SalesReturn, RequiredAt::Activate)
        .await
        .expect("闸门读规则不能报错");
    assert!(
        !gated.iter().any(|rule| rule.id == first),
        "新建即禁用 → 闸门从一开始就不该读到它"
    );

    // 写路径收口：唯一冲突 → 409，CHECK 违反 → 400
    let conflict = model::write_error(err, "新建", || dto::rule_key_conflict(&draft, &first));
    assert_eq!(conflict.status, 409);
    assert_eq!(conflict.code, error_code::RESOURCE_CONFLICT);
    assert!(conflict.msg.contains("SALES_RETURN"), "{}", conflict.msg);

    purge_fixtures().await;
}

/// 空库首启后必须能看到 9 条预置规则（卡面验收①的数据侧）。
#[tokio::test]
async fn seed_rules_are_visible() {
    let Some(_guard) = setup().await else {
        return;
    };
    let list = model::list_all().await.expect("列表必须可读");
    assert!(
        list.len() >= 9,
        "V0347 预置 9 条规则（发票拆电子/纸质），当前只有 {} 条：{:?}",
        list.len(),
        list.iter()
            .map(|row| (row.doc_type.as_str(), row.required_at.as_str()))
            .collect::<Vec<_>>()
    );
    for kind in [
        DocKind::Invoice,
        DocKind::Receipt,
        DocKind::Payment,
        DocKind::ExpenseReport,
        DocKind::FixedAsset,
        DocKind::StockTake,
        DocKind::PurchaseReturn,
        DocKind::SalesReturn,
    ] {
        assert!(
            list.iter().any(|row| row.doc_type == kind),
            "{} 必须有预置规则",
            kind.as_str()
        );
    }
    assert_eq!(
        list.iter()
            .filter(|row| row.doc_type == DocKind::Invoice)
            .count(),
        2,
        "发票按电子 / 纸质拆成两条"
    );
}
