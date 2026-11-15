//! 员工敏感字段**明文**出口（HR-13）：权限码 + 每次访问写 `audit_logs` + `sys_field_mask` 接线验证。
//!
//! 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md` §3.2 ⑥（`sys_field_mask` 两条全局规则）、
//! §5.2 #7（`GET /business/hr/employee/{id}/sensitive`）、D-14（存库明文 + 出参脱敏）、
//! §8.1 HR-13、§8.3 R1（`sys_field_mask` 对 `biz_employee` 是否真生效**未实测**）。
//!
//! # 一句话说清
//!
//! 员工档案的身份证号 / 工资卡号**只以打码形态出现在列表与详情里**（`employee_dto` 的
//! `*_masked`，明文一个字节都不进结构体）。要看明文只有这一条路：**带
//! `business:hr:employee:sensitive` 码**调本接口，**每调一次落一条 `audit_logs`**。
//!
//! # 审计口径（本卡验收的两条）
//!
//! | 维度 | 落在哪 | 值 |
//! |---|---|---|
//! | **谁** | `audit_logs.user_id` + `audit_logs.username` | 令牌里的用户（姓名快照语义见 `AGENTS.md` 约定 13） |
//! | **什么时候** | `audit_logs.created_at` | **数据库时钟**（不是进程时钟：多机部署时进程时钟不可信，且本值会回给前端） |
//! | **看了谁** | `audit_logs.target_type='Employee'` + `audit_logs.target_id` | 员工档案 id |
//! | **看了什么字段** | `audit_logs.operation` + `audit_logs.detail.field` | `HR_EMPLOYEE_SENSITIVE_READ` + `id_card` / `bank_account` |
//!
//! `detail` 里另存 `emp_no` / `employee_name` / `dept_id` / `request_id` / `found`：
//! 审计页按 `target_id` 反查时**不需要**再 join 业务表就能回答「看的是谁」——
//! 员工被软删以后，`target_id` 指向的行已经 `deleted_at`，只靠 join 的审计等于没留痕。
//!
//! # 为什么不复用 `masking::registry::audit_plain_read`
//!
//! 那条留痕的语义是「**这个人**持有 `system:field-mask:plain`，所以在**某处**看到了明文」
//! （`operation='FIELD_MASK_PLAIN_READ'`、`target_id` 是**他自己**的 user_id、没有字段维度）。
//! 本接口要回答的是「**谁** 在 **何时** 看了 **哪个员工的哪个字段**」——
//! 三个维度里有两个（target / field）它都没有。硬塞进去的结果是审计页上全是
//! 「admin 看了明文」，查不出「谁看了张三的身份证」。所以**另立一条** `operation`
//! （`HR_EMPLOYEE_SENSITIVE_READ`），两张口径互不覆盖：有人既持明版权限又调本接口时，
//! 两条都会落，不冲突。
//!
//! # 为什么同步写、且**写不进去就不返回明文**（fail-closed）
//!
//! `support::log::record_audit` 是**非阻塞**的（投递到日志总线即返回）：它适合
//! 「建了单要留痕」（单据本身是证据）。明文不一样 —— 留痕是**唯一的**证据，
//! 丢了就等于「有人看过身份证但系统说没人看过」（`masking` 的 BUG-D-13 就是这么丢的：
//! `request_id` 超 `VARCHAR(36)` 导致每次写入都失败，而失败只进 `tracing`）。
//! 所以这里：
//!
//! 1. **同步写**并在写入后才返回明文（`INSERT ... RETURNING id, created_at`）；
//! 2. 写入失败 → **500 + 不返回明文**（宁可让用户重试，也不产生无痕访问）；
//! 3. `request_id` **按列宽截断**（`V0014` 的 `VARCHAR(36)`，见
//!    [`audit_request_id`]）：`x-request-id` 是客户端可控的，原样塞进去就是 BUG-D-13 的翻版。
//!
//! # `sys_field_mask` 接线验证（设计 §8.3 R1）
//!
//! R1 的疑问是「脱敏模块按 `table_name + column_name` 注入，新表会不会漏」。两层证据：
//!
//! - **纯单测**（本文件）：`SensitiveField::column()` 与 `employee_dto::COL_*`、
//!   `V0388` 种下的两条规则逐字对齐 —— 列名写错会立刻红；
//! - **真库用例**（[`crate::business::hr::employee_sensitive_db_tests`]）：把库里
//!   `biz_employee.id_card` 的 `mask_param` 改成 `3,2` 后，**同一条 handler 路径**出的
//!   打码值立刻变成「前 3 后 2」；改回来又变回「前 6 后 4」。这就是「规则来自库、
//!   不是写死在代码里」的直接证据（DTO 里的 `DEFAULT_*_PARAM` 只是**规则缺失时**的兜底）。
//!
//! # 与其它卡的边界
//!
//! - 前端 `api/business/hr/employee.ts` 的 `fetchEmployeeSensitive` 已在 HR-18 按契约写好
//!   （`plainValueOf` 兼容 `{field, value}` 形态），本卡只落后端，**零前端改动**；
//! - 合同 / 薪资标准的敏感值不在本卡（§5.2 #7 只覆盖员工主档的两列）；
//! - 列表 / 详情接口**继续只出打码值**：本卡不放宽任何既有出参。

use axum::extract::{ConnectInfo, Path, Query};
use axum::http::HeaderMap;
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::business::hr::employee_dto;
use crate::business::hr::employee_handler;
use crate::business::hr::employee_model::{self, EmployeeRow};
use crate::business::masking::MaskSet;
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::shared::error_code;
use crate::utils;

// ==================== 常量（与迁移 / 契约逐字一致） ====================

/// 明文的权限码（`V0387` 的 `sys_menu.auth_code`，`business:hr:employee:sensitive`）。
pub const PERM_SENSITIVE: &str = "business:hr:employee:sensitive";

/// 路由路径（不含 `/api`，由 `[router] prefix` 注入）。
pub const ROUTE_PATH: &str = "/business/hr/employee/{id}/sensitive";

/// 审计操作码（`audit_logs.operation`，`VARCHAR(50)`）。
pub const AUDIT_OPERATION: &str = "HR_EMPLOYEE_SENSITIVE_READ";

/// 审计对象类型（`audit_logs.target_type`，与其它模块的「类名」口径一致）。
pub const AUDIT_TARGET_TYPE: &str = "Employee";

/// `audit_logs.request_id` 的列宽（`V0014`：`VARCHAR(36)`）。
pub const MAX_REQUEST_ID: usize = 36;

/// `audit_logs.username` 的列宽（`V0014`：`VARCHAR(50)`）。
pub const MAX_USERNAME: usize = 50;

/// 审计 `detail` 里的事件名（与 `operation` 分开：`operation` 给「按类型筛」，
/// `detail.event` 给「按 JSON 过滤」，两者同值但用途不同）。
pub const AUDIT_EVENT: &str = "HR_EMPLOYEE_SENSITIVE_READ";

// ==================== 字段枚举 ====================

/// 可查看明文的字段（设计 §5.2 #7 的 `field` 入参只有这两个取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensitiveField {
    /// 身份证号（`biz_employee.id_card`）。
    IdCard,
    /// 工资卡号（`biz_employee.bank_account`）。
    BankAccount,
}

impl SensitiveField {
    /// 全部合法取值（错误报文与前端下拉共用一份，防止两边漂移）。
    pub const ALL: &'static [SensitiveField] = &[SensitiveField::IdCard, SensitiveField::BankAccount];

    /// 解析入参（大小写不敏感 + 去空白）。
    ///
    /// # 参数
    ///
    /// - `raw`：查询串里的 `field`。
    ///
    /// # 返回值
    ///
    /// 不在 [`SensitiveField::ALL`] 里 → `None`（**不做模糊匹配**：`"id"` / `"card"` 都不接受，
    /// 少一个字符就少一层可审计的语义）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "id_card" => Some(SensitiveField::IdCard),
            "bank_account" => Some(SensitiveField::BankAccount),
            _ => None,
        }
    }

    /// 出参 / 审计里的稳定编码（`id_card` / `bank_account`）。
    pub fn code(self) -> &'static str {
        match self {
            SensitiveField::IdCard => employee_dto::COL_ID_CARD,
            SensitiveField::BankAccount => employee_dto::COL_BANK_ACCOUNT,
        }
    }

    /// 对应的 `sys_field_mask.column_name`（与 [`SensitiveField::code`] 同值，
    /// 分开是为了让「列名」这件事在代码里有一个**语义明确**的取点）。
    pub fn column(self) -> &'static str {
        self.code()
    }

    /// 中文标签（错误报文 / 前端提示）。
    pub fn label(self) -> &'static str {
        match self {
            SensitiveField::IdCard => "身份证号",
            SensitiveField::BankAccount => "工资卡号",
        }
    }

    /// 从员工行取**明文**（空白视同未登记 → `None`，不返回 `""`）。
    pub fn value_of(self, row: &EmployeeRow) -> Option<&str> {
        let raw = match self {
            SensitiveField::IdCard => row.id_card.as_deref(),
            SensitiveField::BankAccount => row.bank_account.as_deref(),
        }?;
        let raw = raw.trim();
        if raw.is_empty() { None } else { Some(raw) }
    }

    /// 同一份规则集合下的打码值（与详情页 `sensitive_masked` **同一个函数**，
    /// 所以「明文旁边的打码值」和「详情里的打码值」永不打架）。
    pub fn masked(self, set: &MaskSet, raw: Option<&str>) -> Option<String> {
        match self {
            SensitiveField::IdCard => employee_dto::mask_id_card(set, raw),
            SensitiveField::BankAccount => employee_dto::mask_bank_account(set, raw),
        }
    }
}

impl std::fmt::Display for SensitiveField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// `field` 入参 → 枚举，非法值给出**可点的**报文（把合法取值列出来）。
///
/// # 参数
///
/// - `raw`：查询串里的 `field`。
pub fn field_of(raw: &str) -> std::result::Result<SensitiveField, ApiError> {
    SensitiveField::parse(raw).ok_or_else(|| {
        let allowed: Vec<&str> = SensitiveField::ALL.iter().map(|item| item.code()).collect();
        bad_request(format!("field 只能是 {}，收到 `{}`", allowed.join(" / "), raw.trim()))
    })
}

// ==================== 入参 / 出参 ====================

/// 查询入参（设计 §5.2 #7：`field` 必填）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SensitiveQuery {
    /// 要看明文的字段（`id_card` / `bank_account`）。
    #[serde(default)]
    pub field: String,
}

/// 明文出参。
///
/// # 形状为什么是 `{field, value}`（而不是 `{id_card: ...}`）
///
/// ① 一次请求只给**一个**字段的明文：多给一个字节都是多余的暴露面；
/// ② 前端 `plainValueOf`（HR-18）已兼容本形态（形态 ②），本卡不需要改前端；
/// ③ 带 `field` 的回执让「响应体被贴进工单」时也能一眼看出这是哪个字段的明文。
///
/// `masked_value` / `audit_id` / `audited_at` 是**可追溯性**字段：
/// 前者让前端能在同一行同时显示「打码 → 明文」，后两者是本次访问的审计凭据
/// （用户界面上写「本次查看已记入审计日志」时，可以带上 `audit_id` 方便追问）。
#[derive(Debug, Clone, Serialize)]
pub struct EmployeeSensitivePlainRes {
    /// 员工 id。
    pub employee_id: String,
    /// 工号（审计对账时人对得上）。
    pub emp_no: String,
    /// 姓名（同上；主数据实时值）。
    pub name: String,
    /// 字段编码（`id_card` / `bank_account`）。
    pub field: String,
    /// **明文**（本次查看的唯一目的）。该字段未登记 → `null`（前端已有「未登记」兜底文案）。
    pub value: Option<String>,
    /// 同一规则集合下的打码值（`sys_field_mask` 生效结果，方便前端并排显示）。
    pub masked_value: Option<String>,
    /// 本次留痕的主键（`audit_logs.id`）——响应里能查到，审计页里也能查到。
    pub audit_id: String,
    /// 留痕时间（**数据库时钟**，与 `audit_logs.created_at` 同值）。
    pub audited_at: String,
}

/// 审计写入的入参（与 [`ReaderContext`] 分开：前者是「这次看了什么」，后者是「谁在看」）。
#[derive(Debug, Clone)]
pub struct AuditSubject<'a> {
    /// 被看的员工行（取 `id` / `emp_no` / `name` / `dept_id`）。
    pub row: &'a EmployeeRow,
    /// 字段。
    pub field: SensitiveField,
    /// 该字段是否有值（`false` = 未登记；仍要留痕：**查过**这件事本身就是审计事实）。
    pub found: bool,
}

/// 发起本次查看的人 + 请求上下文（谁 / 从哪来 / 哪个请求）。
///
/// # 为什么把「读的人」与「token」解耦
///
/// 真库用例不需要造一份 `TokenClaims`（它由中间件产出，手工构造等于绕过认证去测认证）；
/// handler 负责把 `headers + ConnectInfo + claims` 折成这个结构，业务函数只认它。
#[derive(Debug, Clone, Default)]
pub struct ReaderContext {
    /// 谁：用户 id（令牌 `sub`）。
    pub user_id: Option<Uuid>,
    /// 谁：用户名（令牌 `username`；已按列宽截断）。
    pub username: Option<String>,
    /// 哪个请求：`x-request-id`（缺失则生成 UUID v7；已按列宽截断）。
    pub request_id: String,
    /// 从哪来：客户端 IP（`X-Forwarded-For` / 连接对端，见 `utils::client_ip`）。
    pub ip: Option<std::net::IpAddr>,
    /// 从哪来：User-Agent。
    pub user_agent: Option<String>,
}

impl ReaderContext {
    /// 由请求上下文构造（handler 唯一的调用点）。
    ///
    /// # 参数
    ///
    /// - `headers`：请求头（`x-request-id` / UA / 转发 IP）；
    /// - `connect_info`：TCP 对端地址（没有可信转发头时的 IP 来源）；
    /// - `claims`：令牌声明（`user_id` / `username`）。
    pub fn from_request(headers: &HeaderMap, connect_info: &std::net::SocketAddr, claims: &TokenClaims) -> Self {
        Self {
            user_id: utils::current_user_uuid(claims),
            username: claims.username.as_deref().map(|name| clamp_chars(name, MAX_USERNAME)),
            request_id: audit_request_id(headers),
            ip: utils::client_ip(headers, Some(connect_info)),
            user_agent: utils::user_agent(headers).map(|agent| clamp_chars(&agent, 512)),
        }
    }
}

// ==================== ① 明文接口 ====================

/// 查看敏感字段明文（设计 §5.2 #7）。
///
/// 权限码 `business:hr:employee:sensitive`；**每次调用**写一条 `audit_logs`（见模块文档）。
/// 数据范围之外 / 档案不存在 → 404（`id` 存在性不外泄，同 HR-12 的单条入口口径）。
///
/// # 参数
///
/// - `headers` / `connect_info` / `claims`：审计留痕的三个来源（谁 / 从哪来 / 哪个请求）；
/// - `id`：员工主键；
/// - `query`：`field`（`id_card` / `bank_account`）。
///
/// # 返回值
///
/// [`EmployeeSensitivePlainRes`]：明文 + 打码值 + 审计凭据。
#[nik::permission(
    path = "/business/hr/employee/{id}/sensitive",
    method = "GET",
    permission = "business:hr:employee:sensitive"
)]
#[nik::get("/business/hr/employee/{id}/sensitive")]
pub async fn employee_sensitive(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<std::net::SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Query(query): Query<SensitiveQuery>,
) -> ResResult<EmployeeSensitivePlainRes> {
    let id = parse_uuid(&id, "id")?;
    let field = field_of(&query.field)?;
    let reader = ReaderContext::from_request(&headers, &connect_info, &claims);

    let principal = principal::resolve_for_claims(&claims).await?;
    let mask = crate::business::masking::resolve(Some(&principal), Some(&claims))
        .await
        .map_err(|error| utils::db_error(error, "加载字段脱敏规则"))?;

    Ok(Res::ok(read_sensitive(&principal, &reader, id, field, &mask).await?))
}

/// 明文的业务实现（接口与真库用例共用同一条路径 —— 审计方式不会出现两套）。
///
/// # 参数
///
/// - `principal`：数据范围（不参与脱敏判定，只判「这个档案对他可见吗」）；
/// - `reader`：谁在看（写进 `audit_logs`）；
/// - `id`：员工主键；
/// - `field`：字段；
/// - `mask`：本次请求的 `sys_field_mask` 规则集合（只用于算 `masked_value`）。
///
/// # 返回值
///
/// [`EmployeeSensitivePlainRes`]。
///
/// # 错误
///
/// - 404：档案不存在 / 已软删 / 数据范围之外；
/// - 500：**审计写入失败**（fail-closed：不返回明文，见模块文档）。
pub async fn read_sensitive(
    principal: &ScopedPrincipal,
    reader: &ReaderContext,
    id: Uuid,
    field: SensitiveField,
    mask: &MaskSet,
) -> std::result::Result<EmployeeSensitivePlainRes, ApiError> {
    // ① 数据范围闸：与列表 / 详情**同一份**探针（HR-12 的 `ensure_visible`）。
    employee_handler::ensure_visible(db(), principal, id).await?;
    let row = employee_model::find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询员工档案"))?
        .ok_or_else(employee_model::not_found_error)?;

    // ② 取明文 + 同一规则集合下的打码值（打码值走 DTO 的同一批函数，形态与详情页一致）。
    let value = field.value_of(&row).map(str::to_string);
    let masked_value = field.masked(mask, value.as_deref());

    // ③ **先留痕再出明文**：写不进去就不返回（fail-closed）。
    let subject = AuditSubject {
        row: &row,
        field,
        found: value.is_some(),
    };
    let audit = insert_read_audit(reader, &subject)
        .await
        .map_err(|error| {
            tracing::error!(
                error = %error,
                employee_id = %id,
                field = field.code(),
                user_id = ?reader.user_id,
                "明文访问审计写入失败，已拒绝返回明文（fail-closed）"
            );
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!(
                    "本次「查看{}明文」的审计日志写入失败，已拒绝返回明文（不留无痕访问）。请重试；若持续失败请联系管理员。",
                    field.label()
                ),
            )
        })?;

    Ok(EmployeeSensitivePlainRes {
        employee_id: row.id.to_string(),
        emp_no: row.emp_no.clone(),
        name: row.name.clone(),
        field: field.code().to_string(),
        value,
        masked_value,
        audit_id: audit.0,
        audited_at: audit.1,
    })
}

// ==================== ② 审计留痕 ====================

/// 一条留痕的读数（`audit_logs.id` + 数据库时钟的 `created_at`）。
type SensitiveAudit = (String, String);

/// 把一次明文查看写进 `audit_logs`（同步、追加写，返回 `audit_id` + `created_at`）。
///
/// # 参数
///
/// - `reader`：谁在查（`user_id` / `username` / `request_id` / `ip` / `user_agent`）；
/// - `subject`：查了谁的什么字段。
///
/// # 返回值
///
/// `(audit_logs.id, created_at 的 ISO8601+08:00 文本)`。
///
/// # SQL 口径
///
/// - 可空列一律 `NULLIF(?::text,'')::xxx`：驱动的 `Value::Null` 绑 `::uuid` / `::inet`
///   编不出参数（与 `changelog/model.rs` 同一个坑），空串还原成 NULL 更稳；
/// - `user_id` / `username` 允许为空（理论上进不来：接口要认证；但**审计不许因此失败**，
///   匿名访问也要留痕 —— 一条 `user_id IS NULL` 的痕仍然能证明「某个请求看过明文」）；
/// - `created_at` 用 `CURRENT_TIMESTAMP`（数据库时钟），并在 `RETURNING` 里读回，
///   保证「回给前端的时间」与「审计行里的时间」是同一个值。
pub async fn insert_read_audit(reader: &ReaderContext, subject: &AuditSubject<'_>) -> DbResult<SensitiveAudit> {
    let detail = audit_detail(reader, subject);
    let row = db()
        .query_first_sql(
            "INSERT INTO audit_logs (id, request_id, user_id, username, operation, target_type, \
             target_id, detail, ip, user_agent, created_at) \
             VALUES (gen_random_uuid(), ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,''), ?, ?, ?, \
             ?::jsonb, NULLIF(?::text,'')::inet, NULLIF(?::text,''), CURRENT_TIMESTAMP) \
             RETURNING id::text AS id, created_at"
                .to_string(),
            vec![
                Value::Text(clamp_chars(&reader.request_id, MAX_REQUEST_ID)),
                Value::Text(reader.user_id.map(|id| id.to_string()).unwrap_or_default()),
                Value::Text(reader.username.clone().unwrap_or_default()),
                Value::Text(AUDIT_OPERATION.to_string()),
                Value::Text(AUDIT_TARGET_TYPE.to_string()),
                Value::Text(subject.row.id.to_string()),
                Value::Json(detail),
                Value::Text(reader.ip.map(|ip| ip.to_string()).unwrap_or_default()),
                Value::Text(reader.user_agent.clone().unwrap_or_default()),
            ],
        )
        .await?
        .ok_or_else(|| DbError::InvalidValue("audit_logs 的 INSERT ... RETURNING 没有回执（不应发生）".to_string()))?;
    let id: String = row.get_typed("id")?;
    let created_at: DateTime<Utc> = row.get_typed("created_at")?;
    Ok((id, employee_dto::timestamp_cst(created_at)))
}

/// 审计 `detail` 的 JSON 文本（**纯函数**，单测直接断言，不连库）。
///
/// # 参数
///
/// - `reader`：谁在查（只用 `request_id`）；
/// - `subject`：查了谁的什么字段。
///
/// # 返回值
///
/// JSON 文本。**绝不含明文值**（审计表不是第二个明文出口 —— 否则「查审计」就成了
/// 绕开 `sensitive` 权限码的后门），只留「看了哪个字段」这个事实。
pub fn audit_detail(reader: &ReaderContext, subject: &AuditSubject<'_>) -> String {
    json!({
        "event": AUDIT_EVENT,
        "field": subject.field.code(),
        "field_label": subject.field.label(),
        "employee_id": subject.row.id.to_string(),
        "emp_no": subject.row.emp_no,
        "employee_name": subject.row.name,
        "dept_id": subject.row.dept_id.to_string(),
        "found": subject.found,
        "request_id": clamp_chars(&reader.request_id, MAX_REQUEST_ID),
        "ip": reader.ip.map(|ip| ip.to_string()),
        "note": "按字段查看员工敏感明文；本次查看已记入审计日志（operation=HR_EMPLOYEE_SENSITIVE_READ）",
    })
    .to_string()
}

/// `x-request-id` → `audit_logs.request_id`（`VARCHAR(36)`）的安全值。
///
/// # 为什么必须截断
///
/// `x-request-id` 是**客户端可控**的头。原样塞进 36 字符的列会 `value too long`，
/// 而且这条 INSERT 在 fail-closed 路径上 → 明文接口 500、用户什么都看不到。
/// 这正是 `masking` 的 BUG-D-13（`field-mask:` + UUID = 47 字符）的翻版。
///
/// 截断按**字符**（不是字节）做，多字节头（有人传中文）不会切在字符中间。
///
/// # 参数
///
/// - `headers`：请求头（优先 `x-request-id`，缺失则生成 UUID v7）。
pub fn audit_request_id(headers: &HeaderMap) -> String {
    let raw = utils::request_id(headers);
    let clamped = clamp_chars(&raw, MAX_REQUEST_ID);
    if clamped.is_empty() {
        Uuid::now_v7().to_string()
    } else {
        clamped
    }
}

/// 按**字符数**截断（不是字节数）：多字节内容不会被切成半个字符。
///
/// # 参数
///
/// - `raw`：原值；
/// - `max`：最大字符数。
pub fn clamp_chars(raw: &str, max: usize) -> String {
    raw.trim().chars().take(max).collect()
}

// ==================== 小工具（与 HR-12 同口径） ====================

/// `400 + 10001`（入参格式错）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 解析路径里的 UUID。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("{field} 不是合法的主键：`{raw}`")))
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    /// 菜单 / 权限种子原文（编译期嵌入）：权限码 / 脱敏规则的唯一事实来源。
    const V0387_SEED: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");
    const V0388_SEED: &str = include_str!("../../../migrations/business/V0388__hr_mask_config.up.sql");

    /// 一条员工行（`id_card` / `bank_account` 都是**明文**，与模型层一致）。
    fn row_fixture() -> EmployeeRow {
        EmployeeRow {
            id: Uuid::from_u128(0x1301),
            org_id: Uuid::from_u128(0x1302),
            emp_no: "EMP-0013".to_string(),
            name: "张三".to_string(),
            gender: 1,
            birth_date: NaiveDate::from_ymd_opt(1990, 1, 1),
            id_card: Some("110101199001011234".to_string()),
            id_card_type: "ID_CARD".to_string(),
            mobile: Some("13800000000".to_string()),
            email: Some("zhangsan@example.com".to_string()),
            dept_id: Uuid::from_u128(0x1303),
            dept_name: "生产部".to_string(),
            position_code: Some("WORKER".to_string()),
            job_level: Some("P3".to_string()),
            employment_type: "FULL_TIME".to_string(),
            hire_date: NaiveDate::from_ymd_opt(2024, 3, 1).expect("合法日期"),
            probation_end_date: NaiveDate::from_ymd_opt(2024, 6, 1),
            leave_date: None,
            leave_type: None,
            status: "ACTIVE".to_string(),
            education: Some("BACHELOR".to_string()),
            school: Some("某大学".to_string()),
            major: Some("计算机".to_string()),
            graduate_date: NaiveDate::from_ymd_opt(2012, 6, 30),
            native_place: Some("北京".to_string()),
            address: Some("北京市海淀区".to_string()),
            emergency_contact: Some("李四".to_string()),
            emergency_phone: Some("13900000000".to_string()),
            emergency_relation: Some("配偶".to_string()),
            bank_name: Some("工商银行".to_string()),
            branch: Some("中关村支行".to_string()),
            bank_account: Some("6222021234567890".to_string()),
            bank_account_name: Some("张三".to_string()),
            user_id: Some(Uuid::from_u128(0x1304)),
            username: "zhangsan".to_string(),
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            create_by: None,
            update_by: None,
        }
    }

    /// 一个读者上下文（含一个超长 `x-request-id`：模拟客户端乱传）。
    fn reader_fixture() -> ReaderContext {
        ReaderContext {
            user_id: Some(Uuid::from_u128(0x1305)),
            username: Some("hr.admin".to_string()),
            request_id: "req-0123456789abcdef0123456789abcdef-too-long".to_string(),
            ip: Some(std::net::IpAddr::from([127, 0, 0, 1])),
            user_agent: Some("curl/8.5.0".to_string()),
        }
    }

    // ==================== ① 权限码 / 路由：与迁移和契约对拍 ====================

    /// 权限码必须真在 `V0387` 的菜单种子里（写错的码不会编译失败，只会线上 403）。
    #[test]
    fn permission_code_is_seeded_by_v0387() {
        assert!(
            V0387_SEED.contains(PERM_SENSITIVE),
            "权限码 {PERM_SENSITIVE} 不在 V0387 的菜单种子里 —— 线上会 403"
        );
    }

    /// 本文件声明的路由必须是 `ROUTE_CONTRACT` 里**登记给 HR-13** 的那一条，且权限码一致。
    #[test]
    fn route_is_registered_to_hr13_in_contract() {
        use crate::business::hr::handler::ROUTE_CONTRACT;
        let source = include_str!("employee_sensitive.rs");
        let pattern = regex::Regex::new(r#"#\[nik::(get|post|put|delete)\(\s*"([^"]+)""#).expect("正则合法");
        let declared: Vec<(String, String)> = pattern
            .captures_iter(source)
            .map(|capture| (capture[1].to_ascii_uppercase(), capture[2].to_string()))
            .collect();
        assert_eq!(declared.len(), 1, "本卡只落一条路由，实际 {declared:?}");
        let (method, path) = &declared[0];
        assert_eq!(path, ROUTE_PATH, "路由路径必须与常量一致");

        let spec = ROUTE_CONTRACT
            .iter()
            .find(|spec| spec.path == *path && spec.method == *method)
            .unwrap_or_else(|| panic!("{method} {path} 不在 ROUTE_CONTRACT 里 —— 先登记契约再实现"));
        assert_eq!(spec.card, "HR-13", "{method} {path} 在契约里登记的卡是 {}", spec.card);
        assert_eq!(spec.permission, Some(PERM_SENSITIVE), "契约里的权限码必须与实现一致");
    }

    /// `#[nik::permission]` 写的码与常量逐字一致（宏写错只有非超管才会 403，超管短路看不出来）。
    #[test]
    fn permission_attribute_matches_constant() {
        let source = include_str!("employee_sensitive.rs");
        let pattern = regex::Regex::new(r#"permission = "([^"]+)"\s*\)\]\s*#\[nik::get"#).expect("正则合法");
        let codes: Vec<String> = pattern
            .captures_iter(source)
            .map(|capture| capture[1].to_string())
            .collect();
        assert_eq!(codes, vec![PERM_SENSITIVE.to_string()]);
    }

    // ==================== ② 字段解析 / 列名对拍 ====================

    /// `field` 只接受两个取值；大小写与空白不敏感；不做模糊匹配。
    #[test]
    fn field_parsing_accepts_only_two_columns() {
        assert_eq!(SensitiveField::parse("id_card"), Some(SensitiveField::IdCard));
        assert_eq!(SensitiveField::parse("  ID_CARD  "), Some(SensitiveField::IdCard));
        assert_eq!(SensitiveField::parse("bank_account"), Some(SensitiveField::BankAccount));
        for bad in ["", "id", "card", "idcard", "mobile", "id_card,bank_account", "身份证"] {
            assert_eq!(SensitiveField::parse(bad), None, "`{bad}` 不该被接受");
        }
    }

    /// 非法 `field` 的报文要把合法取值列出来（否则前端只能猜）。
    #[test]
    fn invalid_field_message_lists_allowed_values() {
        let error = field_of("mobile").expect_err("mobile 不是合法字段");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("id_card"), "报文缺合法取值：{}", error.msg);
        assert!(error.msg.contains("bank_account"), "报文缺合法取值：{}", error.msg);
        assert!(error.msg.contains("mobile"), "报文要点名收到的值：{}", error.msg);
    }

    /// 字段 → 列名 必须与 `employee_dto` 的常量、`V0388` 种下的规则**三方对齐**。
    ///
    /// 这条断言守的是设计 §8.3 R1：「脱敏按 `table_name + column_name` 注入」——
    /// 列名与库里种的对不上，`sys_field_mask` 就静默不生效（列表直接出明文）。
    #[test]
    fn field_columns_match_dto_and_v0388_seed() {
        assert_eq!(SensitiveField::IdCard.column(), employee_dto::COL_ID_CARD);
        assert_eq!(SensitiveField::BankAccount.column(), employee_dto::COL_BANK_ACCOUNT);
        assert_eq!(SensitiveField::IdCard.column(), "id_card");
        assert_eq!(SensitiveField::BankAccount.column(), "bank_account");
        for (column, param) in [
            ("id_card", employee_dto::DEFAULT_ID_CARD_PARAM),
            ("bank_account", employee_dto::DEFAULT_BANK_ACCOUNT_PARAM),
        ] {
            assert!(
                V0388_SEED.contains("'biz_employee', '") && V0388_SEED.contains(column),
                "V0388 没有种 biz_employee.{column} 的脱敏规则"
            );
            assert!(
                V0388_SEED.contains(param),
                "V0388 里 {column} 的 mask_param 不是 {param}"
            );
        }
        // 前端下拉 / 错误报文与枚举同源
        let codes: Vec<&str> = SensitiveField::ALL.iter().map(|item| item.code()).collect();
        assert_eq!(codes, vec!["id_card", "bank_account"]);
    }

    /// 明文取值的三个边界：有值 / 空串 / 未登记。
    #[test]
    fn value_of_treats_blank_as_unregistered() {
        let mut row = row_fixture();
        assert_eq!(SensitiveField::IdCard.value_of(&row), Some("110101199001011234"));
        assert_eq!(SensitiveField::BankAccount.value_of(&row), Some("6222021234567890"));

        row.id_card = Some("   ".to_string());
        assert_eq!(SensitiveField::IdCard.value_of(&row), None, "纯空白视同未登记");
        row.bank_account = None;
        assert_eq!(SensitiveField::BankAccount.value_of(&row), None);
    }

    // ==================== ③ 审计：谁 / 何时 / 谁的 / 什么字段 ====================

    /// `detail` 必须同时回答「谁的」「什么字段」「哪个请求」，且**不含明文**。
    #[test]
    fn audit_detail_names_who_whose_what_without_plaintext() {
        let row = row_fixture();
        let reader = reader_fixture();
        let subject = AuditSubject {
            row: &row,
            field: SensitiveField::IdCard,
            found: true,
        };
        let text = audit_detail(&reader, &subject);
        let value: serde_json::Value = serde_json::from_str(&text).expect("合法 JSON");

        assert_eq!(value["event"], AUDIT_EVENT);
        assert_eq!(value["operation_owner"], serde_json::Value::Null, "不该有乱七八糟的键");
        assert_eq!(value["field"], "id_card");
        assert_eq!(value["field_label"], "身份证号");
        assert_eq!(value["employee_id"], row.id.to_string(), "看的是谁");
        assert_eq!(value["emp_no"], row.emp_no, "工号（人对得上）");
        assert_eq!(value["employee_name"], row.name, "姓名快照");
        assert_eq!(value["dept_id"], row.dept_id.to_string());
        assert_eq!(value["found"], true);
        // request_id 已按列宽截断（客户端可控头的防线）
        let request_id = value["request_id"].as_str().expect("request_id 是字符串");
        assert_eq!(request_id.chars().count(), MAX_REQUEST_ID);
        assert_eq!(value["ip"], "127.0.0.1");

        // 审计表不是第二个明文出口
        assert!(
            !text.contains("110101199001011234"),
            "审计 detail 泄漏了身份证明文：{text}"
        );
        assert!(
            !text.contains("6222021234567890"),
            "审计 detail 泄漏了工资卡明文：{text}"
        );
    }

    /// 未登记也要留痕，但 `found=false`（「查过」本身是审计事实）。
    #[test]
    fn audit_detail_marks_missing_value_as_not_found() {
        let mut row = row_fixture();
        row.bank_account = None;
        let subject = AuditSubject {
            row: &row,
            field: SensitiveField::BankAccount,
            found: false,
        };
        let value: serde_json::Value =
            serde_json::from_str(&audit_detail(&reader_fixture(), &subject)).expect("合法 JSON");
        assert_eq!(value["field"], "bank_account");
        assert_eq!(value["found"], false);
    }

    /// 审计四个文本列的长度必须落在 `V0014` 的列宽内（否则 INSERT 直接 `value too long`）。
    #[test]
    fn audit_columns_fit_column_widths() {
        assert!(AUDIT_OPERATION.chars().count() <= 50, "operation 超 VARCHAR(50)");
        assert!(AUDIT_TARGET_TYPE.chars().count() <= 50, "target_type 超 VARCHAR(50)");
        assert!(AUDIT_EVENT.chars().count() <= 50);
        // target_id = 员工 UUID
        assert!(Uuid::now_v7().to_string().chars().count() <= 100);
        // username / request_id 走 clamp，逐条验证
        assert!(clamp_chars(&"u".repeat(200), MAX_USERNAME).chars().count() <= MAX_USERNAME);
        assert!(clamp_chars(&"r".repeat(200), MAX_REQUEST_ID).chars().count() <= MAX_REQUEST_ID);
    }

    /// 客户端乱传超长 `x-request-id` → 截断到 36 字符（BUG-D-13 的防线）。
    #[test]
    fn request_id_is_clamped_to_column_width() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "r".repeat(200).parse().expect("合法头值"));
        let request_id = audit_request_id(&headers);
        assert_eq!(request_id.chars().count(), MAX_REQUEST_ID);
        assert!(request_id.chars().all(|c| c == 'r'));
    }

    /// 多字节内容按**字符**截断，不会切在字符中间。
    ///
    /// 两层防线：`clamp_chars` 用 `chars()` 天然按边界切；HTTP 头里的非 ASCII 值
    /// 在 `HeaderValue::to_str()` 就已经取不到（`utils::request_id` 于是回退 UUID v7）——
    /// 也就是说「脏头」既不会写进 36 字符的列，也不会把多字节切坏。
    #[test]
    fn request_id_clamp_is_char_boundary_safe() {
        // 回归：按字节截断 `&s[..36]` 会 panic（`byte index 36 is not a char boundary`）
        assert_eq!(clamp_chars("张三", 1), "张");
        assert_eq!(
            clamp_chars(&"请".repeat(50), MAX_REQUEST_ID).chars().count(),
            MAX_REQUEST_ID
        );
        assert_eq!(
            clamp_chars(&"请".repeat(50), MAX_REQUEST_ID),
            "请".repeat(MAX_REQUEST_ID)
        );

        // 非 ASCII 的 header 值在 `to_str()` 就失败 → 回退 UUID v7（长度天然合规）
        let mut headers = HeaderMap::new();
        if let Ok(value) = "请".repeat(50).parse() {
            headers.insert("x-request-id", value);
            let request_id = audit_request_id(&headers);
            assert_eq!(request_id.chars().count(), MAX_REQUEST_ID);
            assert!(Uuid::parse_str(&request_id).is_ok() || request_id.chars().all(|c| c == '请'));
        }
    }

    /// 没有 `x-request-id` → 生成 UUID v7（36 字符，恰好占满列宽）。
    #[test]
    fn request_id_falls_back_to_uuid_v7() {
        let request_id = audit_request_id(&HeaderMap::new());
        assert_eq!(request_id.len(), 36, "UUID 文本应当是 36 字符：{request_id}");
        assert!(Uuid::parse_str(&request_id).is_ok(), "兜底值必须是合法 UUID");
    }

    // ==================== ④ 出参形状：前端契约 ====================

    /// 出参必须带 `field` + `value`（HR-18 的 `plainValueOf` 形态 ②）与审计凭据。
    #[test]
    fn response_shape_matches_frontend_contract() {
        let res = EmployeeSensitivePlainRes {
            employee_id: Uuid::now_v7().to_string(),
            emp_no: "EMP-0013".to_string(),
            name: "张三".to_string(),
            field: "id_card".to_string(),
            value: Some("110101199001011234".to_string()),
            masked_value: Some("110101****1234".to_string()),
            audit_id: Uuid::now_v7().to_string(),
            audited_at: "2026-11-12T10:00:00+08:00".to_string(),
        };
        let json = serde_json::to_value(&res).expect("序列化");
        assert_eq!(json["field"], "id_card");
        assert_eq!(json["value"], "110101199001011234");
        assert_eq!(json["masked_value"], "110101****1234");
        assert!(json["audit_id"].as_str().is_some_and(|id| !id.is_empty()));
        assert!(json["audited_at"].as_str().is_some_and(|at| at.ends_with("+08:00")));
        // 未登记时 `value` 是 null（不是空串），前端 `plainValueOf` 会回落到打码值
        let empty = EmployeeSensitivePlainRes {
            value: None,
            masked_value: None,
            ..res
        };
        let json = serde_json::to_value(&empty).expect("序列化");
        assert!(json["value"].is_null());
    }

    /// 打码值走的就是详情页同一批函数：同一规则集合下两处结果必须一致。
    #[test]
    fn masked_value_uses_the_same_dto_functions() {
        let row = row_fixture();
        let set = crate::business::masking::MaskSet::empty();
        let id_card = SensitiveField::IdCard
            .masked(&set, SensitiveField::IdCard.value_of(&row))
            .expect("有打码值");
        let bank = SensitiveField::BankAccount
            .masked(&set, SensitiveField::BankAccount.value_of(&row))
            .expect("有打码值");
        assert_eq!(
            id_card,
            employee_dto::mask_id_card(&set, row.id_card.as_deref()).unwrap()
        );
        assert_eq!(
            bank,
            employee_dto::mask_bank_account(&set, row.bank_account.as_deref()).unwrap()
        );
        assert_eq!(id_card, "110101****1234");
        assert!(bank.starts_with("6222") && bank.contains('*'));
    }

    /// 路径里的 UUID 解析（非法值 → 400，不 500）。
    #[test]
    fn uuid_parsing_guards() {
        let id = Uuid::now_v7();
        assert_eq!(parse_uuid(&id.to_string(), "id").expect("合法"), id);
        let error = parse_uuid("not-a-uuid", "id").expect_err("非法 UUID");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
    }
}
