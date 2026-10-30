//! 变更日志的输入 / 输出 DTO（批次 C · C5）。
//!
//! # 为什么要独立的 DTO 层
//!
//! [`crate::business::changelog::record`] 的调用点是**业务 handler 的事务闭包内部**，
//! 调用方手里只有「claims + headers + before/after 两个 JSON」。把这些原始件直接塞给
//! `record()` 会让「操作人 / IP / request_id 从哪来」在每个调用点各写一遍（口径必然漂移）。
//! 因此本文件提供三个**只做装填**的类型：
//!
//! | 类型 | 回答什么问题 |
//! |------|--------------|
//! | [`ChangeCtx`] | 谁、什么时候、从哪个 IP、哪个请求、耗时多少 |
//! | [`ChangeTarget`] | 哪张表、哪条数据、什么动作、中文怎么叫 |
//! | [`DiffInput`] | 变更前 / 变更后的两份 JSON |
//!
//! 它们都是**纯数据**（无 IO、无 DB），构造与断言都不需要数据库；真正落库的只有
//! [`crate::business::changelog::record`]。
//!
//! # 与库表枚举的对应关系
//!
//! [`TraceSource`] 的 4 个取值与 [`Action`] 的 11 个取值**逐字对应** V0084 的
//! `ck_sys_change_log_source` / `ck_sys_change_log_action` 约束。枚举在这里封闭，
//! 调用方无法拼出 `"HACK"` 这类脏值——数据库 CHECK 是最后一道，编译器是第一道。

use nik::prelude::*;
use nik::uuid::Uuid;

/// 变更来源（`sys_change_log.trace_source`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TraceSource {
    /// 手工 HTTP 操作（默认）。
    Api,
    /// 定时任务 / 后台作业。
    Job,
    /// 数据导入。
    Import,
    /// 系统自维护（迁移回填、补偿任务）。
    System,
}

impl TraceSource {
    /// 四个取值（顺序即库表 CHECK 的书写顺序）。
    pub const ALL: &'static [TraceSource] = &[
        TraceSource::Api,
        TraceSource::Job,
        TraceSource::Import,
        TraceSource::System,
    ];

    /// 写入 `sys_change_log.trace_source` 的字符串。
    pub fn code(self) -> &'static str {
        match self {
            TraceSource::Api => "API",
            TraceSource::Job => "JOB",
            TraceSource::Import => "IMPORT",
            TraceSource::System => "SYSTEM",
        }
    }
}

/// 变更动作（`sys_change_log.action`，11 个封闭取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// 新增。
    Create,
    /// 修改。
    Update,
    /// 删除（含软删）。
    Delete,
    /// 改状态（启用/停用、上下架）。
    Status,
    /// 分配（授角色、调部门）。
    Assign,
    /// 发布。
    Publish,
    /// 下线。
    Offline,
    /// 上传。
    Upload,
    /// 下载。
    Download,
    /// 清理（过期数据、分区）。
    Clean,
    /// 登录类事件。
    Login,
}

/// 操作人类型（`sys_change_log.operator_type`，批次 C+ · T5）。
///
/// # 为什么变更日志要单独记一份「操作人是不是 AI」
///
/// `operator_id` 指向 `sys_user`，而 `user_type` 是**可变**的（今天是 `ROBOT`，
/// 明天管理员把它改回 `HUMAN` 也合法）。审计要回答的是「**当时**这条变更是人或
/// AI 做的」，所以必须把类型**冗余落在日志行上**——与 `operator_name` 冗余
/// 同一理由（改名后历史仍可读）。前端据此给 AI 的操作打「AI」标签。
///
/// # 取值
///
/// 与 `sys_user.user_type` 逐字一致（`HUMAN` / `ROBOT`），库侧有 CHECK 兜底；
/// 判定入口是 [`crate::shared::ai_actor::is_robot`]（严格等于 `ROBOT`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OperatorType {
    /// 人类账号（默认）。
    #[default]
    Human,
    /// AI 账号（`user_type = 'ROBOT'`）。
    Robot,
}

impl OperatorType {
    /// 两个取值（顺序即库表 CHECK 的书写顺序）。
    pub const ALL: &'static [OperatorType] = &[OperatorType::Human, OperatorType::Robot];

    /// 写入 `sys_change_log.operator_type` 的字符串。
    pub fn code(self) -> &'static str {
        match self {
            OperatorType::Human => "HUMAN",
            OperatorType::Robot => "ROBOT",
        }
    }

    /// 由 `sys_user.user_type` 的行值判定。
    ///
    /// `None` / 空串 / 任何非 `ROBOT` 的脏值都折成 [`OperatorType::Human`]：
    /// 这一列是**展示与检索**用的，遇到脏数据宁可显示「人」，也不要在写日志时
    /// 因为一个无法解释的枚举把整条业务事务打回滚（日志写不进去 → 业务失败）。
    /// 数据一致性由迁移里的 CHECK 与启动自检负责，不靠这里容错。
    pub fn from_user_type(user_type: Option<&str>) -> Self {
        if crate::shared::ai_actor::is_robot(user_type) {
            OperatorType::Robot
        } else {
            OperatorType::Human
        }
    }

    /// 是否 AI 账号（前端「AI」标签的判据）。
    pub fn is_robot(self) -> bool {
        matches!(self, OperatorType::Robot)
    }
}

impl Action {
    /// 十一个取值（顺序即库表 CHECK 的书写顺序）。
    pub const ALL: &'static [Action] = &[
        Action::Create,
        Action::Update,
        Action::Delete,
        Action::Status,
        Action::Assign,
        Action::Publish,
        Action::Offline,
        Action::Upload,
        Action::Download,
        Action::Clean,
        Action::Login,
    ];

    /// 写入 `sys_change_log.action` 的字符串。
    pub fn code(self) -> &'static str {
        match self {
            Action::Create => "CREATE",
            Action::Update => "UPDATE",
            Action::Delete => "DELETE",
            Action::Status => "STATUS",
            Action::Assign => "ASSIGN",
            Action::Publish => "PUBLISH",
            Action::Offline => "OFFLINE",
            Action::Upload => "UPLOAD",
            Action::Download => "DOWNLOAD",
            Action::Clean => "CLEAN",
            Action::Login => "LOGIN",
        }
    }

    /// 是否为「新增」形态。
    ///
    /// 只影响 diff 的读法：`CREATE` 的 `b` 侧必定是 null（没有「旧值」），
    /// 展示层据此渲染成「从空改成 X」而不是「旧值丢失」。
    pub fn is_create(self) -> bool {
        matches!(self, Action::Create)
    }

    /// 是否为「删除」形态（`a` 侧为 null）。
    pub fn is_delete(self) -> bool {
        matches!(self, Action::Delete)
    }
}

/// 操作主体上下文：谁、什么时候、从哪来。
///
/// 全部字段都可缺省（系统任务没有 IP、后台作业没有 request_id），缺失即写 NULL——
/// **不填假值**：审计表里「未知」比「猜一个」诚实得多。
#[derive(Debug, Clone)]
pub struct ChangeCtx<'a> {
    /// 与 `operation_logs.request_id` 可 join 的请求 ID（手工 HTTP 操作必有）。
    pub request_id: Option<&'a str>,
    /// 变更来源。
    pub trace_source: TraceSource,
    /// 操作人 ID（系统任务为 `None`）。
    pub operator_id: Option<Uuid>,
    /// 操作人名字（冗余存储：用户改名/删除后历史仍可读）。
    pub operator_name: Option<&'a str>,
    /// 操作人当时所属部门（冗余存储：调部门后历史不乱）。
    pub operator_dept_id: Option<Uuid>,
    /// 操作人类型（`HUMAN` / `ROBOT`）。
    ///
    /// `None` 表示「**由 [`crate::business::changelog::record`] 按 `operator_id`
    /// 现查 `sys_user.user_type`**」——默认就走这条路，调用方不用管。
    /// 只有在「操作人账号已被物理删掉 / 不是库里的用户」这类查不到的场景，
    /// 才需要 `with_operator_type` 显式指定。
    ///
    /// 为什么默认查库而不是默认 `Human`：`ROBOT` 的日志必须正确落 `ROBOT`
    /// （卡面验收 8），而调用点有十几个，靠每个调用点自觉填一定漏。
    /// 查库走的是**同一个业务事务的执行器**，成本是一次主键查找（亚毫秒）。
    pub operator_type: Option<OperatorType>,
    /// 客户端 IP（按 [`crate::utils::client_ip`] 的口径解析，与中间件一致）。
    pub client_ip: Option<&'a str>,
    /// 业务发生时刻；`None` 表示「就是现在」。
    ///
    /// 允许显式传入是为了两件事：① 导入/作业批量写日志时对齐业务时间；
    /// ② 让「未来时间 → 62013」这条校验可被单测覆盖（否则只能等系统时钟错乱）。
    pub occurred_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 处理耗时（毫秒），可选。
    pub duration_ms: Option<i32>,
}

impl<'a> ChangeCtx<'a> {
    /// 手工 API 操作的上下文骨架（只填操作人）。
    ///
    /// # 参数
    ///
    /// - `operator_id`：操作人 ID；
    /// - `operator_name`：操作人名字。
    pub fn api(operator_id: Option<Uuid>, operator_name: Option<&'a str>) -> Self {
        Self {
            request_id: None,
            trace_source: TraceSource::Api,
            operator_id,
            operator_name,
            operator_dept_id: None,
            operator_type: None,
            client_ip: None,
            occurred_at: None,
            duration_ms: None,
        }
    }

    /// 系统 / 定时任务的上下文（无操作人、无 IP）。
    pub fn system(source: TraceSource) -> Self {
        Self {
            request_id: None,
            trace_source: source,
            operator_id: None,
            operator_name: None,
            operator_dept_id: None,
            operator_type: None,
            client_ip: None,
            occurred_at: None,
            duration_ms: None,
        }
    }

    /// 补 `request_id`。
    #[must_use]
    pub fn with_request_id(mut self, request_id: Option<&'a str>) -> Self {
        self.request_id = request_id;
        self
    }

    /// 补操作人部门。
    #[must_use]
    pub fn with_operator_dept(mut self, dept_id: Option<Uuid>) -> Self {
        self.operator_dept_id = dept_id;
        self
    }

    /// 显式指定操作人类型（**跳过** `record` 的查库推断）。
    ///
    /// 只在「`operator_id` 查不到 `sys_user` 行」时需要（例如记录一条
    /// 已被物理删除的账号、或导入任务的系统操作人）。正常 API 路径**不要调用它**：
    /// 让 `record` 自己按 `operator_id` 查，才不会出现「明明是 AI 却记成 HUMAN」。
    #[must_use]
    pub fn with_operator_type(mut self, operator_type: OperatorType) -> Self {
        self.operator_type = Some(operator_type);
        self
    }

    /// 补客户端 IP。
    #[must_use]
    pub fn with_client_ip(mut self, ip: Option<&'a str>) -> Self {
        self.client_ip = ip;
        self
    }

    /// 指定业务发生时刻。
    #[must_use]
    pub fn with_occurred_at(mut self, at: Option<chrono::DateTime<chrono::Utc>>) -> Self {
        self.occurred_at = at;
        self
    }

    /// 指定耗时（毫秒）。
    #[must_use]
    pub fn with_duration_ms(mut self, ms: Option<i32>) -> Self {
        self.duration_ms = ms;
        self
    }
}

/// 变更对象：哪张表、哪条数据、什么动作。
///
/// # 为什么没有 `module` 字段
///
/// `module`（system / business / support）由**登记表**[`crate::business::changelog::diff::spec`]
/// 决定，而不是调用方随手写：同一张表的模块归属必须唯一，否则
/// 「按模块检索」的结果会随调用点漂移。调用方想改模块 = 去改登记表，这正是我们要的摩擦。
#[derive(Debug, Clone)]
pub struct ChangeTarget<'a> {
    /// 表名（同时是白名单登记键，如 `sys_user`）。
    pub resource: &'a str,
    /// 业务主键（UUID 或复合键字符串）。
    pub resource_id: String,
    /// 动作。
    pub action: Action,
    /// 中文可读动作名（「修改用户」），直接展示给审计员。
    pub action_label: &'a str,
}

impl<'a> ChangeTarget<'a> {
    /// 构造一个变更对象。
    ///
    /// # 参数
    ///
    /// - `resource`：表名（必须是登记过的表，否则 [`crate::business::changelog::record`] 报 62012）；
    /// - `resource_id`：业务主键字符串；
    /// - `action`：动作；
    /// - `action_label`：中文可读动作名。
    pub fn of(
        resource: &'a str,
        resource_id: impl Into<String>,
        action: Action,
        action_label: &'a str,
    ) -> Self {
        Self {
            resource,
            resource_id: resource_id.into(),
            action,
            action_label,
        }
    }
}

/// 变更前 / 变更后的两份 JSON（列名 → 值）。
///
/// **只放登记白名单里的列就够了**：多余列会被 diff 引擎忽略（白名单是唯一口径），
/// 所以调用方可以直接把 `model` 序列化出来的整行塞进来，不必自己裁剪。
#[derive(Debug, Clone, Copy)]
pub struct DiffInput<'a> {
    /// 变更前（`None` = 新增，此时 `b` 侧全部为 null）。
    pub before: Option<&'a serde_json::Value>,
    /// 变更后（`None` = 删除，此时 `a` 侧全部为 null）。
    pub after: Option<&'a serde_json::Value>,
}

impl<'a> DiffInput<'a> {
    /// 修改：两侧都有。
    pub fn update(before: &'a serde_json::Value, after: &'a serde_json::Value) -> Self {
        Self {
            before: Some(before),
            after: Some(after),
        }
    }

    /// 新增：只有新值。
    pub fn create(after: &'a serde_json::Value) -> Self {
        Self {
            before: None,
            after: Some(after),
        }
    }

    /// 删除：只有旧值。
    pub fn delete(before: &'a serde_json::Value) -> Self {
        Self {
            before: Some(before),
            after: None,
        }
    }
}

/// [`crate::business::changelog::record`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// 已写入一行，返回行 ID 与实际发生变化的字段名（升序）。
    Recorded {
        /// 新写入行的 `id`（UUIDv7）。
        id: Uuid,
        /// 变化字段名列表（`changed_fields` 列的来源）。
        changed_fields: Vec<String>,
    },
    /// **没有写入**（所有白名单字段前后相等）。
    ///
    /// 这是有意为之：值没变就没有「变更」可记，写一行空 diff 只会污染审计检索
    /// （架构 §4.12.2 `record_skips_unchanged`）。
    Skipped,
}

// ==================== 查询接口 DTO（批次 C · C6）====================
//
// 追加在 C5 的 DTO 之后，**不改上面任何一个类型**（C5 的写入侧契约保持原样）。
// 这里只有两类东西：
//
// 1. `*Query`：HTTP 查询参数（字符串进、解析在 handler 里做，DTO 不碰时间格式）；
// 2. `*Res`：出参。`from_row` 负责把 `query::ChangeLogRow` 转成前端契约，
//    **不做任何脱敏**——库里的 `diff` 已经是 L1 脱敏值，读侧不二次变形；
//    唯一会「再脱敏一次」的出口是导出（见 `handler::render_diff` 的 L3）。

/// 列表 / 导出共用的查询参数（`GET /business/changelog/page`、`.../export`）。
///
/// 全部可缺省：缺省即「不过滤」。时间窗的默认值补在 handler 里（近 90 天），
/// 不在 DTO 里补 —— DTO 保持「用户传了什么就是什么」的纯粹语义。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ChangeLogPageQuery {
    /// 起始时间（RFC3339 / `YYYY-MM-DD HH:MM:SS` / `YYYY-MM-DD`）。
    pub start_time: Option<String>,
    /// 结束时间（同上）。
    pub end_time: Option<String>,
    /// 操作人 ID。
    pub operator_id: Option<String>,
    /// 操作人部门（架构 §4.7.4 的显式部门视角）。
    pub operator_dept_id: Option<String>,
    /// 操作人名字（模糊）。
    pub operator_name: Option<String>,
    /// 模块 `system` / `business` / `support`。
    pub module: Option<String>,
    /// 资源表名（如 `sys_user`）。
    pub resource: Option<String>,
    /// 对象主键。
    pub resource_id: Option<String>,
    /// 动作码（如 `UPDATE`）。
    pub action: Option<String>,
    /// 结果（v1 只有 `SUCCESS`）。
    pub result: Option<String>,
    /// 关键词（动作名 / 操作人 / 对象 ID 三处模糊）。
    pub keyword: Option<String>,
    /// 页码（缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省 20）。
    pub page_size: Option<u64>,
    /// 导出条数（仅 `/export` 用，缺省与上限都是 5000）。
    pub limit: Option<u64>,
}

/// 详情查询参数（`GET /business/changelog/{id}`）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ChangeLogDetailQuery {
    /// 业务发生时刻。传了就能只扫一个分区（主键含分区键）。
    pub created_at: Option<String>,
}

/// 对象历史查询参数（`GET /business/changelog/timeline`）。
///
/// `table` 是 SQL 保留字，字段名加 `#[serde(rename)]` 保持对外的参数名不变
/// （任务书就是 `?table=&target_id=`）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ChangeLogTimelineQuery {
    /// 资源表名（如 `sys_user`）。
    #[serde(rename = "table")]
    pub table: Option<String>,
    /// 对象主键。
    pub target_id: Option<String>,
    /// 可选起始时间（不传 = 全部历史）。
    pub start_time: Option<String>,
    /// 可选结束时间。
    pub end_time: Option<String>,
    /// 条数（缺省 50，上限 200）。
    pub limit: Option<u32>,
}

/// 统计查询参数（`GET /business/changelog/stats`）——与列表同口径，去掉分页字段。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ChangeLogStatsQuery {
    /// 起始时间。
    pub start_time: Option<String>,
    /// 结束时间。
    pub end_time: Option<String>,
    /// 操作人 ID。
    pub operator_id: Option<String>,
    /// 操作人部门。
    pub operator_dept_id: Option<String>,
    /// 操作人名字（模糊）。
    pub operator_name: Option<String>,
    /// 模块。
    pub module: Option<String>,
    /// 资源表名。
    pub resource: Option<String>,
    /// 对象主键。
    pub resource_id: Option<String>,
    /// 动作码。
    pub action: Option<String>,
    /// 结果。
    pub result: Option<String>,
    /// 关键词。
    pub keyword: Option<String>,
}

/// 列表项（列表页只给需要的字段，`diff` 只给变化字段名）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChangeLogListItem {
    /// 行 ID。
    pub id: String,
    /// 业务发生时刻（RFC3339）。
    pub created_at: String,
    /// 模块。
    pub module: String,
    /// 资源表名。
    pub resource: String,
    /// 对象主键。
    pub resource_id: String,
    /// 动作码。
    pub action: String,
    /// 动作中文名。
    pub action_label: String,
    /// 操作人 ID。
    pub operator_id: Option<String>,
    /// 操作人名字。
    pub operator_name: Option<String>,
    /// 操作人类型（`HUMAN` / `ROBOT`；前端据此显示「AI」标签，批次 C+ · T5）。
    pub operator_type: String,
    /// 来源。
    pub trace_source: String,
    /// 变化字段名（升序）。
    pub changed_fields: Vec<String>,
    /// 是否含脱敏字段（列表页角标用）。
    pub masked: bool,
    /// 结果。
    pub result: String,
}

impl ChangeLogListItem {
    /// 从查询行构造。
    pub fn from_row(row: &crate::business::changelog::query::ChangeLogRow) -> Self {
        Self {
            id: row.id.to_string(),
            created_at: row.created_at.to_rfc3339(),
            module: row.module.clone(),
            resource: row.resource.clone(),
            resource_id: row.resource_id.clone(),
            action: row.action.clone(),
            action_label: row.action_label.clone(),
            operator_id: row.operator_id.map(|id| id.to_string()),
            operator_name: row.operator_name.clone(),
            operator_type: row.operator_type.clone(),
            trace_source: row.trace_source.clone(),
            changed_fields: row.changed_fields.clone(),
            masked: row.masked(),
            result: row.result.clone(),
        }
    }
}

/// 一个字段的前后值（详情页的「前后值对比」一行）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FieldDiffRes {
    /// 列名。
    pub field: String,
    /// 变更前（新增时为 `null`）。
    pub before: serde_json::Value,
    /// 变更后（删除时为 `null`）。
    pub after: serde_json::Value,
    /// 该字段的值是否经过脱敏（L1）。
    pub masked: bool,
    /// 是否被整值抹掉（`{"redacted":true}`）。
    pub redacted: bool,
}

/// 大字段摘要（**不含全文**）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BigSummaryRes {
    /// 原值字符数。
    pub len: u64,
    /// 原文 SHA-256（被抹掉时为空串）。
    pub sha256: String,
    /// 前 200 字预览。
    pub preview: String,
    /// 是否被整值抹掉。
    pub redacted: bool,
}

/// 一个大字段的前后摘要。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BigFieldRes {
    /// 列名。
    pub field: String,
    /// 变更前摘要。
    pub before: BigSummaryRes,
    /// 变更后摘要。
    pub after: BigSummaryRes,
}

/// 详情。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChangeLogDetailRes {
    /// 行 ID。
    pub id: String,
    /// 业务发生时刻（RFC3339）。
    pub created_at: String,
    /// 模块。
    pub module: String,
    /// 资源表名。
    pub resource: String,
    /// 对象主键。
    pub resource_id: String,
    /// 动作码。
    pub action: String,
    /// 动作中文名。
    pub action_label: String,
    /// 请求 ID（可与 `operation_logs` 关联）。
    pub request_id: Option<String>,
    /// 来源。
    pub trace_source: String,
    /// 操作人 ID。
    pub operator_id: Option<String>,
    /// 操作人名字。
    pub operator_name: Option<String>,
    /// 操作人类型（`HUMAN` / `ROBOT`，批次 C+ · T5）。
    pub operator_type: String,
    /// 操作人部门。
    pub operator_dept_id: Option<String>,
    /// 客户端 IP。
    pub client_ip: Option<String>,
    /// 变化字段名（升序）。
    pub changed_fields: Vec<String>,
    /// 逐字段前后值（按列名升序）。
    pub fields: Vec<FieldDiffRes>,
    /// 大字段摘要。
    pub big: Vec<BigFieldRes>,
    /// 结果。
    pub result: String,
    /// 记录模式。
    pub diff_mode: String,
    /// 耗时（毫秒）。
    pub duration_ms: Option<i32>,
    /// 脱敏标记（任一字段被脱敏即为 `true`）。
    pub masked: bool,
    /// 原始 diff（前端要自己渲染时用；**已是脱敏值**）。
    pub diff: serde_json::Value,
}

/// 值是否是 `{"redacted": true}` 形态。
fn is_redacted(value: &serde_json::Value) -> bool {
    value
        .get("redacted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// 解析大字段摘要（缺字段用默认值，不 panic）。
fn big_summary(value: Option<&serde_json::Value>) -> BigSummaryRes {
    let Some(serde_json::Value::Object(map)) = value else {
        return BigSummaryRes {
            len: 0,
            sha256: String::new(),
            preview: String::new(),
            redacted: false,
        };
    };
    BigSummaryRes {
        len: map
            .get("len")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        sha256: map
            .get("sha256")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        preview: map
            .get("preview")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        redacted: map
            .get("redacted")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    }
}

impl ChangeLogDetailRes {
    /// 从查询行构造（`fields` / `big` 按列名升序，前端渲染顺序稳定）。
    pub fn from_row(row: &crate::business::changelog::query::ChangeLogRow) -> Self {
        let mut fields: Vec<FieldDiffRes> = Vec::new();
        if let Some(map) = row.diff.get("fields").and_then(|v| v.as_object()) {
            for (name, pair) in map {
                let before = pair.get("b").cloned().unwrap_or(serde_json::Value::Null);
                let after = pair.get("a").cloned().unwrap_or(serde_json::Value::Null);
                fields.push(FieldDiffRes {
                    field: name.clone(),
                    masked: pair
                        .get("masked")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    redacted: is_redacted(&before) || is_redacted(&after),
                    before,
                    after,
                });
            }
        }
        fields.sort_by(|left, right| left.field.cmp(&right.field));

        let mut big: Vec<BigFieldRes> = Vec::new();
        if let Some(map) = row.diff.get("big").and_then(|v| v.as_object()) {
            for (name, pair) in map {
                big.push(BigFieldRes {
                    field: name.clone(),
                    before: big_summary(pair.get("b")),
                    after: big_summary(pair.get("a")),
                });
            }
        }
        big.sort_by(|left, right| left.field.cmp(&right.field));

        Self {
            id: row.id.to_string(),
            created_at: row.created_at.to_rfc3339(),
            module: row.module.clone(),
            resource: row.resource.clone(),
            resource_id: row.resource_id.clone(),
            action: row.action.clone(),
            action_label: row.action_label.clone(),
            request_id: row.request_id.clone(),
            trace_source: row.trace_source.clone(),
            operator_id: row.operator_id.map(|id| id.to_string()),
            operator_name: row.operator_name.clone(),
            operator_type: row.operator_type.clone(),
            operator_dept_id: row.operator_dept_id.map(|id| id.to_string()),
            client_ip: row.client_ip.clone(),
            changed_fields: row.changed_fields.clone(),
            fields,
            big,
            result: row.result.clone(),
            diff_mode: row.diff_mode.clone(),
            duration_ms: row.duration_ms,
            masked: row.masked(),
            diff: row.diff.clone(),
        }
    }
}

/// 时间线一项（比详情精简：不带 big / meta，保留前端 Tab 需要的字段）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChangeLogTimelineItem {
    /// 行 ID。
    pub id: String,
    /// 业务发生时刻（RFC3339）。
    pub created_at: String,
    /// 动作码。
    pub action: String,
    /// 动作中文名。
    pub action_label: String,
    /// 操作人名字。
    pub operator_name: Option<String>,
    /// 操作人类型（`HUMAN` / `ROBOT`，批次 C+ · T5）。
    pub operator_type: String,
    /// 变化字段名。
    pub changed_fields: Vec<String>,
    /// 原始 diff（脱敏值）。
    pub diff: serde_json::Value,
}

impl ChangeLogTimelineItem {
    /// 从查询行构造。
    pub fn from_row(row: &crate::business::changelog::query::ChangeLogRow) -> Self {
        Self {
            id: row.id.to_string(),
            created_at: row.created_at.to_rfc3339(),
            action: row.action.clone(),
            action_label: row.action_label.clone(),
            operator_name: row.operator_name.clone(),
            operator_type: row.operator_type.clone(),
            changed_fields: row.changed_fields.clone(),
            diff: row.diff.clone(),
        }
    }
}

/// 对象历史响应。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChangeLogTimelineRes {
    /// 该对象的**总**变更次数（不受 `limit` 影响）。
    pub total: i64,
    /// 本次返回的条数上限（回显，避免前端猜）。
    pub limit: u32,
    /// 变更列表（时间倒序）。
    pub items: Vec<ChangeLogTimelineItem>,
}

/// 一个聚合桶。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CountBucketRes {
    /// 分组键。
    pub key: String,
    /// 条数。
    pub count: i64,
}

/// 统计响应。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChangeLogStatsRes {
    /// 过滤条件下的总条数。
    pub total: i64,
    /// 按模块。
    pub by_module: Vec<CountBucketRes>,
    /// 按操作人（Top 20）。
    pub by_operator: Vec<CountBucketRes>,
    /// 按天（升序）。
    pub by_day: Vec<CountBucketRes>,
}

/// 一张登记表。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TableRegistryEntryRes {
    /// 表名。
    pub table: String,
    /// 模块归属。
    pub module: String,
    /// 审计白名单字段。
    pub fields: Vec<String>,
    /// 大字段列（无论多短都摘要化）。
    pub big_fields: Vec<String>,
}

/// 登记表清单响应。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TableRegistryRes {
    /// 已登记的表（按表名升序）。
    pub tables: Vec<TableRegistryEntryRes>,
}
