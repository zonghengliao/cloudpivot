//! 附件必传规则（**ATT-06**）：哪些单据在哪个动作必须传附件、要什么格式、至少要几个。
//!
//! # 为什么是配置表（而不是每个模块写死）
//!
//! 「发票提交必须传发票影像」「付款出款前必须有银行回单」这类要求是会变的：
//! 财务换票种（数电票只认 PDF/OFD/XML）、审计要求多加一个验收单、某类单据临时放宽 ——
//! 写死在各模块里意味着每次都要改代码 + 发版。本模块把规则落成
//! `biz_attachment_rule`（迁移 `V0347`），运行时**逐次读取**：
//!
//! ```text
//!   单据 submit / confirm / activate
//!        └─► rule::assert_attachments(DocKind, doc_id, RequiredAt)
//!                 ├─ 查 biz_attachment_rule（doc_type + required_at + enabled）
//!                 ├─ 查该单据未删附件（file_role + 扩展名）
//!                 └─ 逐条判：数量 / 扩展名白名单 / condition_sql / max_count
//! ```
//!
//! # 三条硬约定
//!
//! 1. **不缓存**：`enabled = false` 改完立即生效（卡面验收⑤：不重启服务）。
//!    规则表只有个位数行，一次索引命中的开销远小于「配了不生效」的排查成本。
//! 2. **查库失败 ≠ 通过**：规则表读不出来时返回 [`AttachRuleError::Db`]（500），
//!    绝不静默放行 —— 否则一次数据库抖动就会让「必传」变成「可不传」。
//! 3. **条件只拼进 WHERE**：`condition_sql` 是对单据表当前行求值的片段
//!    （如 `invoice_code IS NULL`），迁移里的 CHECK 与 [`condition_sql_is_safe`]
//!    双重拒绝语句分隔符与注释符；表名只来自 [`DocKind::table`] 的白名单。
//!
//! # 触发动作与业务动作的对应（本仓真实状态机）
//!
//! | 单据 | `required_at` | 实际卡在哪个动作 |
//! |------|---------------|------------------|
//! | 发票 `INVOICE` | `SUBMIT` | `invoice::change_status` → `ISSUED`（开票） |
//! | 收款单 `RECEIPT` | `SUBMIT` | `receipt::change_status` → `CONFIRMED`（收款单没有独立提交态，确认即提交） |
//! | 付款单 `PAYMENT` | `ACTIVATE` | `payable_payment::execute_approved_payment`（审批通过、真正出款前；付款记录是提交动作内部创建的，提交时还没有单据可挂附件） |
//! | 费用报销 `EXPENSE_REPORT` | `SUBMIT` | `expense_handler::report_submit` |
//! | 固定资产 `FIXED_ASSET` | `ACTIVATE` | `asset_handler::asset_status_change` → `IN_USE`（建卡即 `IN_USE`，故新建不受限） |
//! | 盘点单 `STOCK_TAKE` | `CONFIRM` | `stocktake_handler::change_status` → `PENDING_POST`（提交审批） |
//! | 采购退货 `PURCHASE_RETURN` | `SUBMIT` | `purchase_return_handler::submit_return` |
//! | 销售退货 `SALES_RETURN` | `SUBMIT` | `sales_return_handler::change_return_status` → `SUBMITTED` |
//!
//! # 错误文案的形状（卡面验收②③要求「缺什么、要什么格式、还差几个」）
//!
//! ```text
//!   发票必须上传发票影像（pdf/ofd/xml）：至少要 1 个，当前 0 个（该单据共 1 个附件），还差 1 个；
//!   现有附件是 jpg 格式，不在白名单内，请上传 pdf/ofd/xml 格式
//! ```
//!
//! 第一段（`{单据}必须上传{remark}（{允许格式}）`）由 `remark` 驱动，
//! 所以改 `remark` 就是改文案，不用改代码。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;

use super::model::DocKind;

/// 规则表名（**唯一出处**：本模块只查这一张表）。
pub const RULE_TABLE: &str = "biz_attachment_rule";

/// 触发动作（与 `biz_attachment_rule.required_at` 的 CHECK 逐字对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredAt {
    /// 提交（发票开票 / 报销提交 / 退货提交 / 收款确认）
    Submit,
    /// 确认（盘点单提交审批）
    Confirm,
    /// 生效（固定资产启用入账 / 付款出款）
    Activate,
}

impl RequiredAt {
    /// 全部动作。
    pub const ALL: [RequiredAt; 3] = [RequiredAt::Submit, RequiredAt::Confirm, RequiredAt::Activate];

    /// 数据库字面值（`required_at` 列）。
    pub fn as_str(self) -> &'static str {
        match self {
            RequiredAt::Submit => "SUBMIT",
            RequiredAt::Confirm => "CONFIRM",
            RequiredAt::Activate => "ACTIVATE",
        }
    }

    /// 中文标签（错误文案 / 运维读日志用）。
    pub fn label(self) -> &'static str {
        match self {
            RequiredAt::Submit => "提交",
            RequiredAt::Confirm => "确认",
            RequiredAt::Activate => "生效",
        }
    }

    /// 解析（大小写不敏感）：未知值返回 `None`。
    pub fn parse(raw: &str) -> Option<RequiredAt> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "SUBMIT" => Some(RequiredAt::Submit),
            "CONFIRM" => Some(RequiredAt::Confirm),
            "ACTIVATE" => Some(RequiredAt::Activate),
            _ => None,
        }
    }
}

/// 一条规则（`biz_attachment_rule` 的读侧投影）。
#[derive(Debug, Clone)]
pub struct AttachmentRule {
    /// 规则主键。
    pub id: Uuid,
    /// 单据类型。
    pub doc_type: DocKind,
    /// 指定文件角色（**原样大写**；`None` = 任意角色都算）。
    ///
    /// 刻意不在读侧把未知角色解析成 `FileRole::Default`：一个配错角色的规则
    /// 应该「永远不匹配 → 报缺附件」，而不是悄悄把「任意附件」当成满足条件。
    pub file_role: Option<String>,
    /// 触发动作。
    pub required_at: RequiredAt,
    /// 至少几个命中白名单的附件。
    pub min_count: i32,
    /// 最多几个（`None` = 不限）。
    pub max_count: Option<i32>,
    /// 扩展名白名单（小写、已拆分去重；`None` = 不限格式）。
    pub allow_exts: Option<Vec<String>>,
    /// 附加条件（对单据表当前行求值的 WHERE 片段）。
    pub condition_sql: Option<String>,
    /// 人话名（直接进错误文案）。
    pub remark: Option<String>,
}

impl AttachmentRule {
    /// 命中该规则需要的文件角色（`None` = 任意）。
    pub fn file_role_filter(&self) -> Option<&str> {
        self.file_role.as_deref()
    }

    /// 要求的人话名：`remark` → 角色中文名 → 「附件」。
    pub fn requirement_name(&self) -> String {
        if let Some(remark) = self.remark.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            return remark.to_string();
        }
        if let Some(role) = self.file_role.as_deref() {
            if let Some(parsed) = super::model::FileRole::parse(Some(role)) {
                return parsed.label().to_string();
            }
            return role.to_string();
        }
        "附件".to_string()
    }

    /// 第一段文案：`{单据}必须上传{人话名}（{允许格式}）`。
    ///
    /// 卡面验收②的逐字文案就是这条拼出来的：
    /// `发票` + `必须上传` + `发票影像` + `（pdf/ofd/xml）`。
    pub fn requirement(&self) -> String {
        let mut text = format!("{}必须上传{}", self.doc_type.label(), self.requirement_name());
        if let Some(exts) = self.allow_exts.as_deref()
            && !exts.is_empty()
        {
            text.push_str(&format!("（{}）", exts.join("/")));
        }
        text
    }

    /// 允许格式的展示串（`pdf/ofd/xml`）；不限格式时返回 `None`。
    pub fn allowed_exts_text(&self) -> Option<String> {
        self.allow_exts
            .as_deref()
            .filter(|exts| !exts.is_empty())
            .map(|exts| exts.join("/"))
    }
}

/// 单条规则的违例（结构化：缺什么、要什么格式、还差几个）。
#[derive(Debug, Clone)]
pub struct RuleViolation {
    /// 规则主键（运维按它去改表）。
    pub rule_id: Uuid,
    /// 第一段文案（`{单据}必须上传{人话名}（{格式}）`）。
    pub requirement: String,
    /// 要求的文件角色（`None` = 任意）。
    pub file_role: Option<String>,
    /// 允许格式（空 = 不限）。
    pub allowed_exts: Vec<String>,
    /// 至少几个。
    pub min_count: i32,
    /// 最多几个（`None` = 不限）。
    pub max_count: Option<i32>,
    /// 命中白名单的附件数。
    pub matched: i32,
    /// 该规则范围内的附件总数（不分格式）。
    pub total: i32,
    /// 该规则范围内真实存在的扩展名（去重升序；不含白名单内已满足的部分）。
    pub actual_exts: Vec<String>,
}

impl RuleViolation {
    /// 还差几个（`matched >= min_count` 时为 0）。
    pub fn shortfall(&self) -> i32 {
        (self.min_count - self.matched).max(0)
    }

    /// **前端按钮旁的一行红字**（ATT-08）：`hint` 与 400 报文同源，只换个说法。
    ///
    /// # 为什么由后端拼（而不是前端自己写「缺少 X」）
    ///
    /// 三条线必须说同一件事：① 按钮旁红字；② 点 disabled 按钮的 tooltip；
    /// ③ 绕过前端直接提交时后端返回的 400 报文。前两处在前端画，第三处在后端拦 ——
    /// 一旦各自拼文案，用户会看到「缺少发票影像」和「发票必须上传发票影像（pdf/ofd/xml）」
    /// 两种说法，还以为缺的是两样东西。这里统一出短句，前端原样显示。
    ///
    /// # 四种形状
    ///
    /// - 一个附件都没有：`缺少发票影像（需 pdf/ofd/xml 格式）`（卡面验收①的文案）；
    /// - 有附件但**格式全不对**：`发票影像格式不符，需要 pdf/ofd/xml（当前 jpg）`（验收③）；
    /// - 格式对但**数量不够**：`发票影像还差 1 个（需 pdf/ofd/xml 格式，已有 1/2）`；
    /// - 超过上限：`发票影像最多 2 个（当前 3 个，请先删掉多余的）`。
    ///
    /// 不限格式（`allow_exts` 为空）时不写「需 xx 格式」，避免出现「需  格式」这种空括号。
    pub fn hint(&self) -> String {
        if let Some(max) = self.max_count
            && self.total > max
        {
            return format!(
                "{}最多 {max} 个（当前 {} 个，请先删掉多余的）",
                self.requirement_name(),
                self.total
            );
        }
        let need_text = match self.allowed_exts.is_empty() {
            true => String::new(),
            false => format!("需 {} 格式", self.allowed_exts.join("/")),
        };
        // ① 一个都没有（`total` 是规则范围内的附件总数，不含别的角色的）
        if self.total == 0 {
            return if need_text.is_empty() {
                format!("缺少{}（至少 {} 个）", self.requirement_name(), self.min_count)
            } else {
                format!("缺少{}（{need_text}）", self.requirement_name())
            };
        }
        // ② 有附件但格式全不符（`matched == 0`）：把「当前是什么」也说出来
        if self.matched == 0 && !self.allowed_exts.is_empty() {
            let actual = if self.actual_exts.is_empty() {
                "无扩展名".to_string()
            } else {
                self.actual_exts.join("/")
            };
            return format!(
                "{}格式不符，需要 {}（当前 {actual}）",
                self.requirement_name(),
                self.allowed_exts.join("/")
            );
        }
        // ③ 格式对但数量不够
        if self.allowed_exts.is_empty() {
            format!(
                "{}还差 {} 个（至少 {} 个，当前 {} 个）",
                self.requirement_name(),
                self.shortfall(),
                self.min_count,
                self.matched
            )
        } else {
            format!(
                "{}还差 {} 个（需 {} 格式，已有 {}/{}）",
                self.requirement_name(),
                self.shortfall(),
                self.allowed_exts.join("/"),
                self.matched,
                self.min_count
            )
        }
    }

    /// 需求名（人话名），从 `requirement`（`{单据}必须上传{人话名}（{格式}）`）里剥出来。
    ///
    /// 不复用 `AttachmentRule::requirement_name()` 是因为 [`RuleViolation`] 是「已判定」
    /// 的结果，不该再回头依赖规则对象 —— 但它必须与 `requirement` 的第一段同源，
    /// 所以这里**从 `requirement` 反解**（剥掉 `{单据}必须上传` 前缀与格式括号）。
    /// 单测 `hint_uses_same_name_as_requirement` 钉住两者不会漂。
    pub fn requirement_name(&self) -> String {
        let text = self
            .requirement
            .split_once("必须上传")
            .map(|(_, rest)| rest)
            .unwrap_or(self.requirement.as_str());
        match text.split_once('（') {
            Some((name, _)) => name.to_string(),
            None => text.to_string(),
        }
    }

    /// 单条违例的人话（多段用中文分号连接）。
    pub fn message(&self) -> String {
        // ① 超上限：多出来的先删（比「缺」更容易让用户自己修好）
        if let Some(max) = self.max_count
            && self.total > max
        {
            return format!(
                "{}最多允许 {max} 个附件，当前 {} 个：请先删掉多余附件再重试",
                self.requirement, self.total
            );
        }
        let role_text = self
            .file_role
            .as_deref()
            .and_then(|role| super::model::FileRole::parse(Some(role)))
            .map(|role| format!("「{}」角色", role.label()))
            .unwrap_or_default();
        let mut text = format!(
            "{}：{}至少要 {} 个，当前 {} 个（该单据{}共 {} 个附件），还差 {} 个",
            self.requirement,
            role_text,
            self.min_count,
            self.matched,
            if role_text.is_empty() { "" } else { "该角色" },
            self.total,
            self.shortfall()
        );
        // ② 有附件但格式不对：必须把「现有是什么」「要什么」都写清楚（卡面验收③）
        if self.total > 0 && self.matched == 0 && !self.allowed_exts.is_empty() {
            let actual = if self.actual_exts.is_empty() {
                "无扩展名".to_string()
            } else {
                self.actual_exts.join("/")
            };
            text.push_str(&format!(
                "；现有附件是 {actual} 格式，不在白名单内，请上传 {} 格式",
                self.allowed_exts.join("/")
            ));
        } else if self.matched > 0 && self.shortfall() > 0 && !self.actual_exts.is_empty() {
            text.push_str(&format!(
                "；不符合格式的附件是 {} 格式，请补传 {} 格式",
                self.actual_exts.join("/"),
                self.allowed_exts.join("/")
            ));
        }
        text
    }
}

/// 校验失败（**不是**「随便一个错误」：要么规则不满足，要么读库失败）。
#[derive(Debug, Clone)]
pub enum AttachRuleError {
    /// 规则不满足（至少一条）。
    Violation(Vec<RuleViolation>),
    /// 读规则 / 读附件 / 求值条件失败（**绝不当成通过**）。
    Db(String),
}

impl AttachRuleError {
    /// 转成 HTTP 错误：规则不满足 = 400 + `PARAM_INVALID`；读库失败 = 500。
    ///
    /// 报文（`message`）在多条违例时逐行拼接：前端 toast 换行显示，
    /// 用户一次就能看到「缺哪几样」而不是改一个再撞一个。
    pub fn into_api_error(self) -> ApiError {
        match self {
            AttachRuleError::Violation(violations) => ApiError::new(
                400,
                error_code::PARAM_INVALID,
                violations
                    .iter()
                    .map(RuleViolation::message)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            AttachRuleError::Db(detail) => ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("附件必传规则校验失败：{detail}"),
            ),
        }
    }

    /// `Result<(), AttachRuleError>` → `Result<(), ApiError>`（handler 里一行 `?`）。
    pub fn into_api_result(result: std::result::Result<(), AttachRuleError>) -> std::result::Result<(), ApiError> {
        result.map_err(AttachRuleError::into_api_error)
    }
}

/// 条件片段的安全性判据（与迁移里的 CHECK 同口径，双保险）。
///
/// # 规则
///
/// - 空串 / 纯空白 → 视为「无附加条件」（`None`）；
/// - 含 `;` `--` `/*` `*/` → 拒绝（它是 WHERE 片段，不是完整语句）；
/// - 含 `?` → 拒绝（参数占位只属于本模块，混进来必然对不上参数个数）。
///
/// # 返回值
///
/// `Ok(None)` = 无附加条件；`Ok(Some(text))` = 可用的片段；`Err(原因)` = 拒绝。
pub fn condition_sql_is_safe(raw: Option<&str>) -> std::result::Result<Option<String>, String> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    for forbidden in [";", "--", "/*", "*/"] {
        if text.contains(forbidden) {
            return Err(format!(
                "condition_sql 含非法片段「{forbidden}」：它只允许是对单据行求值的条件片段"
            ));
        }
    }
    if text.contains('?') {
        return Err("condition_sql 不允许占位符「?」：参数由附件模块自己传入".to_string());
    }
    Ok(Some(text.to_string()))
}

/// `allow_exts` 拆分：逗号分隔、小写、去空白、去重（保序）。
pub fn parse_exts(raw: Option<&str>) -> Option<Vec<String>> {
    let text = raw?;
    let mut out: Vec<String> = Vec::new();
    for part in text.split(',') {
        let ext = part.trim().trim_start_matches('.').to_ascii_lowercase();
        if ext.is_empty() {
            continue;
        }
        if !out.contains(&ext) {
            out.push(ext);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// 从文件名 / `sys_file.extension` 取扩展名（小写、无点）。
///
/// `extension` 列优先（上传时已归一）；为空时从 `original_name` / `file_name`
/// 的最后一个 `.` 之后取；都没有返回空串（视为「无扩展名」→ 必然不在白名单）。
pub fn ext_of(extension: Option<&str>, display_name: &str) -> String {
    if let Some(ext) = extension.map(str::trim).filter(|ext| !ext.is_empty()) {
        return ext.trim_start_matches('.').to_ascii_lowercase();
    }
    match display_name.rsplit_once('.') {
        Some((_, ext)) if !ext.trim().is_empty() && !ext.contains('/') => ext.trim().to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// 一条已绑定的附件（校验只需要这两个字段）。
#[derive(Debug, Clone)]
pub struct AttachmentFact {
    /// 文件角色（`biz_doc_attachment.file_role` 原样值）。
    pub file_role: String,
    /// 扩展名（小写、无点）。
    pub ext: String,
}

/// 读该单据**未删**附件的 (角色, 扩展名)。
///
/// # 异常情况
///
/// 数据库异常 → [`AttachRuleError::Db`]。
pub async fn attachments_of(kind: DocKind, doc_id: &Uuid) -> std::result::Result<Vec<AttachmentFact>, AttachRuleError> {
    let rows = db()
        .find_with_params(
            "SELECT a.file_role, \
                    COALESCE(NULLIF(f.original_name, ''), f.file_name, '') AS display_name, \
                    f.extension \
               FROM biz_doc_attachment a \
               LEFT JOIN sys_file f ON f.id = a.file_id \
              WHERE a.doc_type = ? AND a.doc_id = ?::uuid AND a.deleted_at IS NULL \
              ORDER BY a.created_at ASC, a.id ASC",
            vec![Value::Text(kind.as_str().to_string()), Value::from(doc_id)],
        )
        .await
        .map_err(|error| AttachRuleError::Db(format!("{error}")))?;
    let mut facts = Vec::with_capacity(rows.len());
    for row in &rows {
        let file_role: String = row
            .get_typed("file_role")
            .map_err(|error| AttachRuleError::Db(format!("{error}")))?;
        let display_name: String = row
            .get_typed("display_name")
            .map_err(|error| AttachRuleError::Db(format!("{error}")))?;
        let extension: Option<String> = row
            .get_typed("extension")
            .map_err(|error| AttachRuleError::Db(format!("{error}")))?;
        facts.push(AttachmentFact {
            file_role,
            ext: ext_of(extension.as_deref(), &display_name),
        });
    }
    Ok(facts)
}

/// 读该单据 + 动作下**启用中**的规则（按 `id` 稳定排序）。
///
/// # 异常情况
///
/// 数据库异常 → [`AttachRuleError::Db`]。
pub async fn load_rules(kind: DocKind, at: RequiredAt) -> std::result::Result<Vec<AttachmentRule>, AttachRuleError> {
    let sql = format!(
        "SELECT id, doc_type, file_role, required_at, min_count, max_count, \
                allow_exts, condition_sql, remark \
           FROM {RULE_TABLE} \
          WHERE doc_type = ? AND required_at = ? AND enabled AND deleted_at IS NULL \
          ORDER BY id"
    );
    let rows = db()
        .find_with_params(
            &sql,
            vec![
                Value::Text(kind.as_str().to_string()),
                Value::Text(at.as_str().to_string()),
            ],
        )
        .await
        .map_err(|error| AttachRuleError::Db(format!("{error}")))?;
    let mut rules = Vec::with_capacity(rows.len());
    for row in &rows {
        let db_err = |error: DbError| AttachRuleError::Db(format!("{error}"));
        let id: Uuid = row.get_typed("id").map_err(db_err)?;
        let doc_type_raw: String = row.get_typed("doc_type").map_err(db_err)?;
        let Some(doc_type) = DocKind::parse(&doc_type_raw) else {
            return Err(AttachRuleError::Db(format!(
                "规则 {id} 的 doc_type「{doc_type_raw}」不在 DocKind 词表内"
            )));
        };
        let file_role: Option<String> = row.get_typed("file_role").map_err(db_err)?;
        let required_at_raw: String = row.get_typed("required_at").map_err(db_err)?;
        let Some(required_at) = RequiredAt::parse(&required_at_raw) else {
            return Err(AttachRuleError::Db(format!(
                "规则 {id} 的 required_at「{required_at_raw}」不在 SUBMIT/CONFIRM/ACTIVATE 内"
            )));
        };
        let min_count: i32 = row.get_typed("min_count").map_err(db_err)?;
        let max_count: Option<i32> = row.get_typed("max_count").map_err(db_err)?;
        let allow_exts_raw: Option<String> = row.get_typed("allow_exts").map_err(db_err)?;
        let condition_raw: Option<String> = row.get_typed("condition_sql").map_err(db_err)?;
        let remark: Option<String> = row.get_typed("remark").map_err(db_err)?;
        rules.push(AttachmentRule {
            id,
            doc_type,
            file_role: file_role
                .map(|role| role.trim().to_ascii_uppercase())
                .filter(|role| !role.is_empty()),
            required_at,
            min_count,
            max_count,
            allow_exts: parse_exts(allow_exts_raw.as_deref()),
            condition_sql: condition_sql_is_safe(condition_raw.as_deref())
                .map_err(|reason| AttachRuleError::Db(format!("规则 {id}：{reason}")))?,
            remark,
        });
    }
    Ok(rules)
}

/// `condition_sql` 是否对该单据成立（`None` = 恒成立）。
///
/// # 异常情况
///
/// 规则片段不合法 / 数据库异常 → [`AttachRuleError::Db`]。
pub async fn condition_applies(
    kind: DocKind,
    doc_id: &Uuid,
    condition: Option<&str>,
) -> std::result::Result<bool, AttachRuleError> {
    let Some(fragment) = condition_sql_is_safe(condition).map_err(AttachRuleError::Db)? else {
        return Ok(true);
    };
    let sql = format!("SELECT 1 FROM {} WHERE id = ?::uuid AND ({fragment})", kind.table());
    let rows = db()
        .find_with_params(&sql, vec![Value::from(doc_id)])
        .await
        .map_err(|error| AttachRuleError::Db(format!("求值条件「{fragment}」失败：{error}")))?;
    Ok(!rows.is_empty())
}

/// 纯判定：给定规则 + 附件事实，算出违例（不碰数据库）。
///
/// 抽成纯函数是为了让「数量 / 白名单 / 上下限」的边界能被单测穷举，
/// 不必每次都连库造数据。
pub fn evaluate(rule: &AttachmentRule, facts: &[AttachmentFact]) -> Option<RuleViolation> {
    let scoped: Vec<&AttachmentFact> = facts
        .iter()
        .filter(|fact| match rule.file_role_filter() {
            Some(role) => fact.file_role.eq_ignore_ascii_case(role),
            None => true,
        })
        .collect();
    let allowed = rule.allow_exts.as_deref().unwrap_or(&[]);
    let mut matched = 0i32;
    let mut actual_exts: Vec<String> = Vec::new();
    for fact in &scoped {
        let hit = allowed.is_empty() || allowed.iter().any(|ext| ext == &fact.ext);
        if hit {
            matched += 1;
        } else {
            let label = if fact.ext.is_empty() {
                "无扩展名".to_string()
            } else {
                fact.ext.clone()
            };
            if !actual_exts.contains(&label) {
                actual_exts.push(label);
            }
        }
    }
    actual_exts.sort();
    let total = i32::try_from(scoped.len()).unwrap_or(i32::MAX);
    let short = matched < rule.min_count;
    let too_many = rule.max_count.is_some_and(|max| total > max);
    if !short && !too_many {
        return None;
    }
    Some(RuleViolation {
        rule_id: rule.id,
        requirement: rule.requirement(),
        file_role: rule.file_role.clone(),
        allowed_exts: allowed.to_vec(),
        min_count: rule.min_count,
        max_count: rule.max_count,
        matched,
        total,
        actual_exts,
    })
}

/// **附件必传闸门**：单据动作前调用，规则不满足返回结构化错误。
///
/// # 参数
///
/// - `doc_type`：单据类型（决定查哪张单据表与哪组规则）；
/// - `doc_id`：单据主键；
/// - `at`：触发动作（`SUBMIT` / `CONFIRM` / `ACTIVATE`）。
///
/// # 返回值
///
/// - `Ok(())`：没有启用中的规则，或所有启用中的规则都满足；
/// - `Err(AttachRuleError::Violation(..))`：至少一条规则不满足（结构化：缺什么、
///   要什么格式、还差几个）；
/// - `Err(AttachRuleError::Db(..))`：读库 / 求值失败（**必须当失败处理**）。
///
/// # 为什么规则为空直接放行
///
/// 规则表是「有要求才配」：某类单据没配规则 = 不强制传附件（例如合同扫描件是
/// 业务约定而不是硬闸）。把「没配」当成「必须传」会让上线的第一秒所有单据都提交不了。
pub async fn assert_attachments(
    doc_type: DocKind,
    doc_id: Uuid,
    at: RequiredAt,
) -> std::result::Result<(), AttachRuleError> {
    let violations = check_attachments(doc_type, doc_id, at).await?;
    if violations.is_empty() {
        Ok(())
    } else {
        Err(AttachRuleError::Violation(violations))
    }
}

/// **查缺口**（ATT-08）：与 [`assert_attachments`] 同一条判定链，但**不报错**，
/// 把违例原样返回给调用方（前端「按钮旁红字 / tooltip」需要结构化数据）。
///
/// # 为什么与闸门共用一条链
///
/// 前端提前拦与后端最终拦必须是**同一个判据**：一旦各写一份，就会出现
/// 「按钮亮了但提交 400」或「按钮灰着但其实能提交」——两种都会被当成 bug 报上来。
/// 所以 `assert_attachments` 就是本函数的薄包装（`Ok(()) / Err(Violation)`），
/// 不存在第二条 SQL 路径。
///
/// # 返回值
///
/// - `Ok(vec![])`：没有启用中的规则，或全部满足（前端据此亮按钮）；
/// - `Ok(violations)`：有缺口（前端据此禁用按钮 + 红字）；
/// - `Err(AttachRuleError::Db(..))`：读库 / 求值失败 —— 调用方**不应**把它当成
///   「可以提交」，但也**不要**因此禁用按钮（见 handler 的降级口径）。
///
/// # 异常情况
///
/// 数据库异常 / 规则表里出现词表外的值时返回 [`AttachRuleError::Db`]。
pub async fn check_attachments(
    doc_type: DocKind,
    doc_id: Uuid,
    at: RequiredAt,
) -> std::result::Result<Vec<RuleViolation>, AttachRuleError> {
    let rules = load_rules(doc_type, at).await?;
    if rules.is_empty() {
        return Ok(Vec::new());
    }
    let facts = attachments_of(doc_type, &doc_id).await?;
    let mut violations = Vec::new();
    for rule in &rules {
        // 条件不成立 = 这条规则对这张单不适用（例：纸质票规则遇到数电票）
        if !condition_applies(doc_type, &doc_id, rule.condition_sql.as_deref()).await? {
            continue;
        }
        if let Some(violation) = evaluate(rule, &facts) {
            violations.push(violation);
        }
    }
    Ok(violations)
}

/// handler 用的一行封装：`assert_attachments(..).await` + 转 `ApiError`。
///
/// 存在的意义是让调用点保持三行以内、且**不会忘记把错误转成 400**：
/// ```ignore
/// rule::ensure_attachments(DocKind::Invoice, id, RequiredAt::Submit).await?;
/// ```
pub async fn ensure_attachments(doc_type: DocKind, doc_id: Uuid, at: RequiredAt) -> std::result::Result<(), ApiError> {
    AttachRuleError::into_api_result(assert_attachments(doc_type, doc_id, at).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(allow_exts: Option<&str>, min_count: i32, max_count: Option<i32>) -> AttachmentRule {
        AttachmentRule {
            id: Uuid::now_v7(),
            doc_type: DocKind::Invoice,
            file_role: None,
            required_at: RequiredAt::Submit,
            min_count,
            max_count,
            allow_exts: parse_exts(allow_exts),
            condition_sql: None,
            remark: Some("发票影像".to_string()),
        }
    }

    fn fact(role: &str, ext: &str) -> AttachmentFact {
        AttachmentFact {
            file_role: role.to_string(),
            ext: ext.to_string(),
        }
    }

    /// 触发动作字面值与迁移里的 CHECK 逐字一致，且可回解析。
    #[test]
    fn required_at_round_trip() {
        assert_eq!(RequiredAt::ALL.len(), 3);
        for at in RequiredAt::ALL {
            assert_eq!(RequiredAt::parse(at.as_str()), Some(at));
            assert!(!at.label().is_empty());
        }
        assert_eq!(RequiredAt::parse(" submit "), Some(RequiredAt::Submit));
        assert_eq!(RequiredAt::parse("activate"), Some(RequiredAt::Activate));
        assert_eq!(RequiredAt::parse("CONFIRMED"), None);
        assert_eq!(RequiredAt::parse(""), None);
        assert_eq!(RequiredAt::Submit.as_str(), "SUBMIT");
        assert_eq!(RequiredAt::Confirm.as_str(), "CONFIRM");
        assert_eq!(RequiredAt::Activate.as_str(), "ACTIVATE");
    }

    /// 白名单解析：小写、去空白、去点、去重保序、空 → None。
    #[test]
    fn parse_exts_normalizes() {
        assert_eq!(
            parse_exts(Some(" PDF, .Ofd ,xml,pdf ")),
            Some(vec!["pdf".to_string(), "ofd".to_string(), "xml".to_string()])
        );
        assert_eq!(parse_exts(Some(" , ,")), None);
        assert_eq!(parse_exts(None), None);
    }

    /// 扩展名提取：`extension` 列优先，其次文件名后缀，都没有 → 空串。
    #[test]
    fn ext_of_prefers_extension_column() {
        assert_eq!(ext_of(Some("PDF"), "x.jpg"), "pdf");
        assert_eq!(ext_of(Some(" .Ofd "), "x"), "ofd");
        assert_eq!(ext_of(None, "2026/11/08/发票扫描件.PNG"), "png");
        assert_eq!(ext_of(Some(""), "无后缀"), "");
        assert_eq!(ext_of(None, "2026/11/08/无后缀"), "");
        // 目录名里的点不能被当扩展名
        assert_eq!(ext_of(None, "a.b/无后缀"), "");
    }

    /// 条件片段安全判据：拒绝语句分隔符 / 注释 / 占位符，空白视为无条件。
    #[test]
    fn condition_sql_guard() {
        assert_eq!(condition_sql_is_safe(None), Ok(None));
        assert_eq!(condition_sql_is_safe(Some("   ")), Ok(None));
        assert_eq!(
            condition_sql_is_safe(Some(" invoice_code IS NULL ")),
            Ok(Some("invoice_code IS NULL".to_string()))
        );
        for bad in ["1=1; DROP TABLE x", "1=1 -- x", "1=1 /* x */", "invoice_code = ?"] {
            assert!(condition_sql_is_safe(Some(bad)).is_err(), "{bad} 必须被拒绝");
        }
    }

    /// 卡面验收②：没有附件时的文案逐字包含要求。
    #[test]
    fn message_without_attachments_is_verbatim() {
        let violation = evaluate(&rule(Some("pdf,ofd,xml"), 1, None), &[]).expect("必须违例");
        assert_eq!(violation.matched, 0);
        assert_eq!(violation.total, 0);
        assert_eq!(violation.shortfall(), 1);
        assert_eq!(
            violation.message(),
            "发票必须上传发票影像（pdf/ofd/xml）：至少要 1 个，当前 0 个（该单据共 0 个附件），还差 1 个"
        );
    }

    /// 卡面验收③：传了 jpg 但要求 pdf → 文案必须同时说「现有是什么」「要什么」。
    #[test]
    fn message_reports_wrong_format() {
        let facts = [fact("DEFAULT", "jpg")];
        let violation = evaluate(&rule(Some("pdf,ofd,xml"), 1, None), &facts).expect("必须违例");
        let message = violation.message();
        assert!(message.starts_with("发票必须上传发票影像（pdf/ofd/xml）"), "{message}");
        assert!(
            message.contains("现有附件是 jpg 格式，不在白名单内，请上传 pdf/ofd/xml 格式"),
            "{message}"
        );
        assert_eq!(violation.actual_exts, vec!["jpg".to_string()]);
    }

    /// 卡面验收④：格式对 + 数量够 → 不违例（纯逻辑层）。
    #[test]
    fn correct_format_passes() {
        let facts = [fact("DEFAULT", "pdf")];
        assert!(evaluate(&rule(Some("pdf,ofd,xml"), 1, None), &facts).is_none());
    }

    /// 数量边界：min_count = 2 只传 1 个 → 还差 1 个；上限超了 → 报「最多允许」。
    #[test]
    fn count_bounds() {
        let facts = [fact("DEFAULT", "pdf")];
        let violation = evaluate(&rule(Some("pdf"), 2, None), &facts).expect("必须违例");
        assert_eq!(violation.shortfall(), 1);
        assert!(violation.message().contains("还差 1 个"), "{}", violation.message());

        let facts = [fact("DEFAULT", "pdf"), fact("DEFAULT", "pdf"), fact("DEFAULT", "pdf")];
        let violation = evaluate(&rule(Some("pdf"), 1, Some(2)), &facts).expect("必须违例");
        assert!(
            violation.message().contains("最多允许 2 个附件，当前 3 个"),
            "{}",
            violation.message()
        );
        // 上限内不违例
        assert!(evaluate(&rule(Some("pdf"), 1, Some(3)), &facts).is_none());
    }

    /// 角色过滤：规则指定 `INVOICE_SCAN` 时，挂在 `DEFAULT` 上的 pdf 不算数。
    #[test]
    fn role_filter_applies() {
        let mut scoped = rule(Some("pdf"), 1, None);
        scoped.file_role = Some("INVOICE_SCAN".to_string());
        let facts = [fact("DEFAULT", "pdf")];
        let violation = evaluate(&scoped, &facts).expect("必须违例");
        assert_eq!(violation.total, 0);
        assert!(
            violation.message().contains("「发票影像」角色"),
            "{}",
            violation.message()
        );

        let facts = [fact("INVOICE_SCAN", "pdf")];
        assert!(evaluate(&scoped, &facts).is_none());
        // 角色比较大小写不敏感（表里存原样值）；扩展名必须已小写（ext_of 归一）
        let facts = [fact("invoice_scan", "pdf")];
        assert!(evaluate(&scoped, &facts).is_none(), "角色大小写不敏感");
        let facts = [fact("INVOICE_SCAN", "PDF")];
        assert!(
            evaluate(&scoped, &facts).is_some(),
            "扩展名不做二次归一：事实层必须已小写"
        );
    }

    /// 不限格式（`allow_exts = NULL`）时任何附件都算命中。
    #[test]
    fn null_whitelist_accepts_any_ext() {
        let facts = [fact("DEFAULT", "zip")];
        assert!(evaluate(&rule(None, 1, None), &facts).is_none());
        let violation = evaluate(&rule(None, 2, None), &facts).expect("必须违例");
        assert_eq!(violation.shortfall(), 1);
        // 不限格式时不该出现「格式不对」的误导性后缀
        assert!(!violation.message().contains("不在白名单内"), "{}", violation.message());
    }

    /// 文案第一段完全由 `remark` 驱动（改表就是改文案）。
    #[test]
    fn requirement_uses_remark() {
        let mut bank = rule(Some("pdf,jpg,png"), 1, None);
        bank.doc_type = DocKind::Payment;
        bank.required_at = RequiredAt::Activate;
        bank.remark = Some("银行回单".to_string());
        assert_eq!(bank.requirement(), "付款单必须上传银行回单（pdf/jpg/png）");

        // 没有 remark 时回落角色中文名
        let mut role = rule(Some("pdf"), 1, None);
        role.file_role = Some("BANK_RECEIPT".to_string());
        role.remark = None;
        assert_eq!(role.requirement(), "发票必须上传银行回单（pdf）");

        // 两者都没有 → 「附件」
        let mut bare = rule(Some("pdf"), 1, None);
        bare.remark = None;
        assert_eq!(bare.requirement(), "发票必须上传附件（pdf）");
    }

    /// 多条违例合并成一个 400，报文逐行（前端一次显示全部缺口）。
    #[test]
    fn multiple_violations_join_with_newline() {
        let mut first = rule(Some("pdf"), 1, None);
        first.remark = Some("发票影像".to_string());
        let mut second = rule(Some("jpg"), 1, None);
        second.file_role = Some("ACCEPTANCE".to_string());
        second.remark = Some("验收单".to_string());
        let error = AttachRuleError::Violation(vec![
            evaluate(&first, &[]).expect("违例一"),
            evaluate(&second, &[]).expect("违例二"),
        ]);
        let api = error.into_api_error();
        assert_eq!(api.status, 400);
        assert!(api.msg.contains('\n'), "{}", api.msg);
        assert!(api.msg.contains("发票影像"));
        assert!(api.msg.contains("验收单"));

        // 读库失败 → 500，绝不当成「通过」
        let api = AttachRuleError::Db("boom".to_string()).into_api_error();
        assert_eq!(api.status, 500);
        assert!(api.msg.contains("附件必传规则校验失败"));
    }

    /// ATT-08 卡面验收①：一个附件都没有 → 按钮旁红字逐字文案。
    #[test]
    fn hint_without_attachments_is_verbatim() {
        let violation = evaluate(&rule(Some("pdf,ofd,xml"), 1, None), &[]).expect("必须违例");
        assert_eq!(violation.hint(), "缺少发票影像（需 pdf/ofd/xml 格式）");
    }

    /// ATT-08 卡面验收③：传了 jpg → 红字必须说「格式不符，需要 pdf/ofd/xml」。
    #[test]
    fn hint_reports_wrong_format() {
        let facts = [fact("DEFAULT", "jpg")];
        let violation = evaluate(&rule(Some("pdf,ofd,xml"), 1, None), &facts).expect("必须违例");
        let hint = violation.hint();
        assert!(
            hint.contains("格式不符，需要 pdf/ofd/xml"),
            "红字必须点名格式不符：{hint}"
        );
        assert!(hint.contains("当前 jpg"), "还要说出现有格式：{hint}");
        assert!(hint.starts_with("发票影像"), "{hint}");
    }

    /// 格式对但数量不够 / 超上限：两个分支都要有专属说法（不能都退化成「缺少」）。
    #[test]
    fn hint_covers_count_bounds() {
        let facts = [fact("DEFAULT", "pdf")];
        let violation = evaluate(&rule(Some("pdf"), 2, None), &facts).expect("必须违例");
        assert_eq!(violation.hint(), "发票影像还差 1 个（需 pdf 格式，已有 1/2）");

        let facts = [fact("DEFAULT", "pdf"), fact("DEFAULT", "pdf"), fact("DEFAULT", "pdf")];
        let violation = evaluate(&rule(Some("pdf"), 1, Some(2)), &facts).expect("必须违例");
        assert_eq!(violation.hint(), "发票影像最多 2 个（当前 3 个，请先删掉多余的）");
    }

    /// 不限格式时不出现「需  格式」的空括号。
    #[test]
    fn hint_without_whitelist_has_no_dangling_parens() {
        let violation = evaluate(&rule(None, 1, None), &[]).expect("必须违例");
        assert_eq!(violation.hint(), "缺少发票影像（至少 1 个）");
        let facts = [fact("DEFAULT", "zip")];
        let violation = evaluate(&rule(None, 2, None), &facts).expect("必须违例");
        assert_eq!(violation.hint(), "发票影像还差 1 个（至少 2 个，当前 1 个）");
        for hint in [violation.hint()] {
            assert!(!hint.contains("  "), "不该出现双空格：{hint}");
        }
    }

    /// `hint` 里的需求名必须与 `requirement` 第一段同源（剥前缀剥括号不能剥错）。
    #[test]
    fn hint_uses_same_name_as_requirement() {
        let mut bank = rule(Some("pdf,jpg,png"), 1, None);
        bank.doc_type = DocKind::Payment;
        bank.required_at = RequiredAt::Activate;
        bank.remark = Some("银行回单".to_string());
        let violation = evaluate(&bank, &[]).expect("必须违例");
        assert_eq!(violation.requirement, "付款单必须上传银行回单（pdf/jpg/png）");
        assert_eq!(violation.hint(), "缺少银行回单（需 pdf/jpg/png 格式）");

        // 没有 remark、靠角色中文名时同理
        let mut role = rule(Some("pdf"), 1, None);
        role.file_role = Some("BANK_RECEIPT".to_string());
        role.remark = None;
        let facts = [fact("DEFAULT", "pdf")];
        let violation = evaluate(&role, &facts).expect("必须违例（角色不符）");
        assert_eq!(violation.hint(), "缺少银行回单（需 pdf 格式）");
    }
}
