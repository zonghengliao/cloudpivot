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

use alun::prelude::*;
use alun::uuid::Uuid;

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
