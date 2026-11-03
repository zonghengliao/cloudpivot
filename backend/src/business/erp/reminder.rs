//! 业务提醒任务包（P1-2 · 应付到期 / 采购超期 / 合同到期 / 低库存）。
//!
//! # 它解决什么
//!
//! ARCH-3 审计里 4 个「该提醒」的场景一个都没有入口（断链 DC-06 + DC-12 的
//! `safety_stock` 部分）：
//!
//! | # | 场景 | 数据事实 | 现状（本卡之前） |
//! |---|------|----------|------------------|
//! | ① | 应付到期 | `biz_payable.due_date` | 到期前没人知道，逾期没付也没有任何信号 |
//! | ② | 采购超期 | `biz_purchase_order.expect_date` | 已审批未到货只能靠人翻列表 |
//! | ③ | 合同到期 | `biz_contract.end_date` | 到期前没人知道（DC-06） |
//! | ④ | 安全库存 | `biz_material.safety_stock` | 填了但从不参与判定的摆设字段（DC-12） |
//!
//! 本模块是这 4 个提醒的**唯一入口**，范本是 [`crate::business::erp::transfer_overdue`]
//! （在途调拨超期告警）—— 那套「扫描 → 组装草稿 → `notice::dispatch` → 记台账」的结构
//! 已经在本仓验证过，这里只替换「扫什么」与「发给谁」。
//!
//! # 结构
//!
//! | 文件 | 职责 |
//! |------|------|
//! | 本文件 | 四类的元数据 / 业务日 / 幂等键 / 台账读写 / 收件人解析 / 统一发送与计数 / 注册 / HTTP 入口 |
//! | [`crate::business::erp::reminder_payable`] | ① 应付到期（D-7 / D-3 / D-1） |
//! | [`crate::business::erp::reminder_purchase`] | ② 采购超期（超过 `expect_date` 未收完） |
//! | [`crate::business::erp::reminder_contract`] | ③ 合同到期（D-30 / D-15 / D-7 / D-1） |
//! | [`crate::business::erp::reminder_stock`] | ④ 低库存（当前库存 < 安全库存） |
//!
//! # 幂等双层（这是本卡的设计重点）
//!
//! ```text
//! 第一层（本地台账，唯一定论）：biz_reminder_log
//!     唯一键 (doc_type, doc_id, remind_kind, remind_date)
//!     「同一份单据 × 同一种提醒 × 同一个业务日」只允许一行。
//!     扫描前一次性读出今天的行 → 命中即跳过（skipped_reminded）；
//!     重提醒间隔（采购超期 / 低库存）也用它算「最近提醒过没有」。
//!     写回「已提醒」= 这张台账行的 sent_at（append-only，通知过期被清理也不会丢）。
//!
//! 第二层（通知侧）：sys_notice.idempotency_key
//!     ERP:{KIND}:{doc_id}:{stage}:{yyyy-MM-dd}
//!     并发/多实例抢跑时由 uk_sys_notice_idem 兜住（第二次 dispatch 返回 duplicated）。
//! ```
//!
//! **为什么不能只靠通知侧幂等键**：`notice_expire_cleanup` 会按 `expire_at` 硬删过期通知
//! （级联删接收行），幂等键随之消失 —— 只靠它的话，历史通知被清掉后同一张单会被重新提醒。
//! 台账是 append-only 的事实表，不与通知生命周期耦合。
//!
//! **为什么不能只靠台账**：两个实例同时跑，都读到「今天没有行」，都会发。通知幂等键是
//! 数据库层的唯一索引，能把第二条吃掉。两层是互补的，不是重复。
//!
//! # 业务日为什么是 Asia/Shanghai
//!
//! 「同一天只提醒一次」是给人看的语义，不是 UTC 语义。北京时间 08:00 与 UTC 00:00 差一天，
//! 用 UTC 日期会让「早上 7 点的提醒」和「同一天下午的手动触发」算成两个业务日 → 一天两条。
//! 中国无夏令时，用固定 `+08:00` 偏移（[`tz`]），不引入时区数据库依赖。
//!
//! # 收件人怎么定（不猜、不群发）
//!
//! | 场景 | 收件人规则 |
//! |------|------------|
//! | 应付到期 | 供应商对应的采购员（应付 → 收货单 → 采购单的制单人，逐级回退）+ 财务（财务经理 / 财务总监） |
//! | 采购超期 | 采购单制单人 + 采购经理 |
//! | 合同到期 | 合同制单人 / 负责人 + 采购经理 |
//! | 低库存 | 仓管主管（`WAREHOUSE_MANAGER`） |
//!
//! 角色按 `sys_role.role_code` 解析成**启用用户 id 列表**（`NoticeTarget::User`），
//! 与 `transfer_overdue` 的口径一致：**收件人是「人」，不是角色**。
//! 解析不到任何一个人 → 不发、记 `skipped_no_recipient` 并打 WARN（**不静默丢弃**，
//! 也不往全员推：提醒推给全员等于没人负责）。
//!
//! # 通知内容的三件必需品（卡面验收口径）
//!
//! 每条通知必须有：**单据号** / **关键日期** / **一句话该做什么**。
//! 文案同时准备两份：模板（`sys_notice_template.ERP_REMINDER_*`，运维可改字）与
//! 代码内的纯文本兜底（模板被停用/删除时仍能发出去，不因配置丢提醒）。
//!
//! # 注册位置
//!
//! 4 个 cron 由 [`register_builtin`] 注册，调用点必须是
//! [`crate::task::Scheduler::register_builtin`]（框架调度器对注册表做一次性快照，
//! 写在 `bootstrap.rs` 里会被随后的 `reload()` 清空）。
//! `sys_job` 里那 4 行种子（V0210）是**停用**的配置视图，只用于「定时任务」页查看与手动执行，
//! 详见迁移注释。

use axum::Extension;
use axum::extract::Path;
use nik::prelude::chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use crate::business::erp::period;
use crate::business::erp::{
    reminder_contract, reminder_payable, reminder_purchase, reminder_stock,
};
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, DispatchError, NoticeDraft, NoticeTarget};
use crate::shared::error_code;
use crate::support::config::model::SysConfigModel;
use crate::task::Scheduler;
use crate::utils;

/// 手动触发提醒的权限码（定时任务本身不需要：后台任务没有请求者）。
pub const PERM_RUN: &str = "business:erp:reminder:run";

/// 发送人展示名（系统任务，没有真实操作人）。
pub const SENDER_NAME: &str = "ERP 提醒";

/// 业务日时区偏移（秒）：Asia/Shanghai = 固定 +08:00（中国无夏令时）。
pub const TZ_OFFSET_SECS: i32 = 8 * 3600;

/// 一类提醒一轮最多处理多少条（防止「一次补跑把通知系统打爆」）。
pub const SCAN_BATCH: i64 = 200;

/// 一条通知最多带多少个收件人（角色展开后的上限；超出部分丢弃并 WARN）。
pub const MAX_RECIPIENTS: usize = 200;

/// 采购超期重提醒间隔参数键（`sys_config`，单位：天）。
pub const CONFIG_PO_REALERT_DAYS: &str = "erp.reminder.po_overdue.realert_days";

/// 低库存重提醒间隔参数键（`sys_config`，单位：天）。
pub const CONFIG_LOW_STOCK_REALERT_DAYS: &str = "erp.reminder.low_stock.realert_days";

/// 重提醒间隔缺省值（天）：参数缺失 / 非正数时回退。
pub const DEFAULT_REALERT_DAYS: i64 = 3;

// ==================== 四类提醒的元数据 ====================

/// 提醒类型（4 个场景，一一对应一个 cron 任务 + 一个模板 + 一个台账口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemindKind {
    /// ① 应付到期（到期前 7 / 3 / 1 天）。
    PayableDue,
    /// ② 采购超期（超过 `expect_date` 未收完）。
    PoOverdue,
    /// ③ 合同到期（到期前 30 / 15 / 7 / 1 天）。
    ContractExpire,
    /// ④ 低库存（当前库存 < 安全库存）。
    LowStock,
}

impl RemindKind {
    /// 全部 4 类（注册与批量触发按它遍历，顺序 = 卡面顺序）。
    pub const ALL: [RemindKind; 4] = [
        RemindKind::PayableDue,
        RemindKind::PoOverdue,
        RemindKind::ContractExpire,
        RemindKind::LowStock,
    ];

    /// 台账 `remind_kind` 列值（同时也是幂等键里的类型段）。
    pub fn code(self) -> &'static str {
        match self {
            RemindKind::PayableDue => "PAYABLE_DUE",
            RemindKind::PoOverdue => "PO_OVERDUE",
            RemindKind::ContractExpire => "CONTRACT_EXPIRE",
            RemindKind::LowStock => "LOW_STOCK",
        }
    }

    /// 台账 `doc_type` 列值（一张单只有一种类型，故与 kind 一一对应）。
    pub fn doc_type(self) -> &'static str {
        match self {
            RemindKind::PayableDue => "PAYABLE",
            RemindKind::PoOverdue => "PURCHASE_ORDER",
            RemindKind::ContractExpire => "CONTRACT",
            RemindKind::LowStock => "INVENTORY",
        }
    }

    /// `sys_notice.biz_type`（消息中心分组 + 排查判据）。
    pub fn biz_type(self) -> &'static str {
        match self {
            RemindKind::PayableDue => "payable_due",
            RemindKind::PoOverdue => "po_overdue",
            RemindKind::ContractExpire => "contract_expire",
            RemindKind::LowStock => "low_stock",
        }
    }

    /// 调度名（同时是 `sys_job.invoke_target` 与日志标识）。
    pub fn task_name(self) -> &'static str {
        match self {
            RemindKind::PayableDue => "erp_payable_due_scan",
            RemindKind::PoOverdue => "erp_po_overdue_scan",
            RemindKind::ContractExpire => "erp_contract_expire_scan",
            RemindKind::LowStock => "erp_low_stock_scan",
        }
    }

    /// cron：每天 07:10 / 07:20 / 07:30 / 07:40（6 段写法，第 1 段是秒）。
    ///
    /// 排「上班前、整点后」而不是整点：整点是全系统定时任务的拥挤点
    /// （日志清理 03:00 / 03:10…、通知清理 04:50、导出回收 04:40），错开 10 分钟既避开尖峰，
    /// 又保证提醒在业务员上班时已经在站内信里。
    pub fn cron(self) -> &'static str {
        match self {
            RemindKind::PayableDue => "0 10 7 * * *",
            RemindKind::PoOverdue => "0 20 7 * * *",
            RemindKind::ContractExpire => "0 30 7 * * *",
            RemindKind::LowStock => "0 40 7 * * *",
        }
    }

    /// 任务描述（调度器列表 + 日志）。
    pub fn description(self) -> &'static str {
        match self {
            RemindKind::PayableDue => "系统任务：应付到期提醒（到期前 7/3/1 天）",
            RemindKind::PoOverdue => "系统任务：采购超期提醒（超过期望到货日未收完）",
            RemindKind::ContractExpire => "系统任务：合同到期提醒（到期前 30/15/7/1 天）",
            RemindKind::LowStock => "系统任务：低库存提醒（库存 < 安全库存）",
        }
    }

    /// 通知模板编码（V0210 种子；停用/删除时退回代码内的纯文本兜底）。
    pub fn template_code(self) -> &'static str {
        match self {
            RemindKind::PayableDue => "ERP_REMINDER_PAYABLE_DUE",
            RemindKind::PoOverdue => "ERP_REMINDER_PO_OVERDUE",
            RemindKind::ContractExpire => "ERP_REMINDER_CONTRACT_EXPIRE",
            RemindKind::LowStock => "ERP_REMINDER_LOW_STOCK",
        }
    }

    /// 通知里的跳转链接（列表页，前端可按单号筛）。
    pub fn link_url(self) -> &'static str {
        match self {
            RemindKind::PayableDue => "/business/erp/payable",
            RemindKind::PoOverdue => "/business/erp/purchase-order",
            RemindKind::ContractExpire => "/business/erp/contract",
            RemindKind::LowStock => "/business/erp/inventory",
        }
    }

    /// 重提醒间隔（天）：`1` = 每天最多一次（档位型提醒，档位本身已保证不会天天重复）。
    ///
    /// 采购超期 / 低库存是「状态型」提醒 —— 只要单据没被处理就永远成立，
    /// 天天提醒等于噪音，所以读 `sys_config` 的间隔（缺省 3 天）。
    pub async fn realert_days(self) -> i64 {
        match self {
            RemindKind::PayableDue | RemindKind::ContractExpire => 1,
            RemindKind::PoOverdue => config_positive_i64(CONFIG_PO_REALERT_DAYS)
                .await
                .unwrap_or(DEFAULT_REALERT_DAYS),
            RemindKind::LowStock => config_positive_i64(CONFIG_LOW_STOCK_REALERT_DAYS)
                .await
                .unwrap_or(DEFAULT_REALERT_DAYS),
        }
    }

    /// HTTP / 台账口径的短码（`payable_due` …），用于手动触发接口的路径参数。
    pub fn short_code(self) -> &'static str {
        self.biz_type()
    }

    /// 短码 → 类型（未知返回 `None`，由调用方给 400）。
    pub fn from_short_code(raw: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.short_code().eq_ignore_ascii_case(raw.trim()))
    }

    /// 调度名 → 类型（`task::run_handler` 用）。
    pub fn from_task_name(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.task_name() == raw)
    }
}

/// 读一个「正整数」系统参数：缺失 / 停用 / 非数字 / 非正 → 返回 `None` 让调用方回退缺省值。
///
/// 与 `transfer_scan::overdue_threshold_days` 同一取舍：配置读不到不能让提醒整体失效。
async fn config_positive_i64(key: &str) -> Option<i64> {
    SysConfigModel::read_i64(key)
        .await
        .filter(|value| *value > 0)
}

// ==================== 业务日 / 幂等键 ====================

/// 业务日时区（固定 +08:00）。
///
/// # Panics
///
/// 偏移量是编译期常量且远在合法区间内，`east_opt` 不可能失败。
pub fn tz() -> FixedOffset {
    FixedOffset::east_opt(TZ_OFFSET_SECS).expect("+08:00 一定是合法偏移")
}

/// 时间点对应的**业务日**（Asia/Shanghai）。
pub fn business_day(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&tz()).date_naive()
}

/// 通知幂等键：`ERP:{KIND}:{doc_id}:{stage}:{yyyy-MM-dd}:{seq}`。
///
/// # 为什么带 stage
///
/// 应付的 7/3/1 天与合同的 30/15/7/1 天是**不同档位**：同一张单在 D-7 与 D-3 各要发一条，
/// 不带档位就会被唯一索引当成重放。带上档位后，「同一天同一档位只发一条」自然成立。
///
/// # 为什么带 seq
///
/// 采购超期 / 低库存是状态型提醒，过了重提醒间隔要**再发一次**；此时 stage 与 date 可能
/// 与上一次相同（如「逾期 5 天」隔天再发仍是同一档），不带次数就会被 `uk_sys_notice_idem`
/// 吞掉。`seq` = 台账里这张单的提醒历史行数 + 1（见 [`next_seq_of`]）。
pub fn idempotency_key(
    kind: RemindKind,
    doc_id: Uuid,
    stage: &str,
    day: NaiveDate,
    seq: i64,
) -> String {
    format!(
        "ERP:{}:{}:{}:{}:{}",
        kind.code(),
        doc_id,
        stage,
        day.format("%Y-%m-%d"),
        seq
    )
}

// ==================== 一条待发提醒 ====================

/// 扫描产物：一条待发的提醒（收件人已解析成具体用户）。
#[derive(Debug, Clone)]
pub struct ReminderMessage {
    /// 台账 `doc_id`（应付/采购/合同主键；低库存是库存行主键）。
    pub doc_id: Uuid,
    /// 单号（低库存是物料编码）—— 进台账快照与日志。
    pub doc_no: String,
    /// 档位（`D-7` / `D-3` / `D-1` / `D-30` / `OVERDUE-12` / `LOW`）。
    pub stage: String,
    /// 兜底标题（模板可用时以模板为准）。
    pub title: String,
    /// 兜底正文（模板可用时以模板为准）。
    pub content: String,
    /// 模板变量（与 `ERP_REMINDER_*` 模板里的占位符逐字对应）。
    pub vars: serde_json::Value,
    /// 收件人（已去重、已截断；空 = 定位不到人）。
    pub recipients: Vec<Uuid>,
    /// 收件人规则的**人话描述**（收件人为空时进 WARN 日志，说明「试过哪些规则」）。
    pub recipient_rule: String,
}

/// 一轮扫描的处理结果（每类一份）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// 本轮扫到的「该提醒」条数。
    pub scanned: usize,
    /// 真的发出通知的条数。
    pub sent: u64,
    /// 今天这一档已经提醒过（台账命中）而跳过的条数。
    pub skipped_reminded: u64,
    /// 还在重提醒间隔内（采购超期 / 低库存）而跳过的条数。
    pub skipped_recent: u64,
    /// 定位不到收件人而跳过的条数（已记 WARN）。
    pub skipped_no_recipient: u64,
    /// 发送 / 记账失败（下一轮自动重试）。
    pub failed: u64,
}

impl ScanReport {
    /// 一行可读摘要（写日志与任务执行结果用，运维 grep 依赖字段名）。
    pub fn summary(&self, kind: RemindKind) -> String {
        format!(
            "{}: scanned={} sent={} skipped_reminded={} skipped_recent={} \
             skipped_no_recipient={} failed={}",
            kind.task_name(),
            self.scanned,
            self.sent,
            self.skipped_reminded,
            self.skipped_recent,
            self.skipped_no_recipient,
            self.failed
        )
    }
}

// ==================== 台账（唯一真源） ====================

/// 台账快照：某一类提醒在「业务日窗口内」已经提醒过谁。
#[derive(Debug, Clone, Default)]
pub struct LedgerSnapshot {
    /// 按单据聚合的台账摘要（只含「重提醒窗口内」的行）。
    pub entries: std::collections::HashMap<Uuid, LedgerEntry>,
}

/// 一张单据在窗口内的台账摘要。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LedgerEntry {
    /// 最近一次提醒的业务日（窗口内）。
    pub last_date: Option<NaiveDate>,
}

impl LedgerSnapshot {
    /// 今天（业务日）已经提醒过这张单。
    pub fn reminded_today(&self, doc_id: &Uuid, day: NaiveDate) -> bool {
        self.entries
            .get(doc_id)
            .map(|entry| entry.last_date == Some(day))
            .unwrap_or(false)
    }

    /// 重提醒窗口内提醒过这张单（含今天）。
    pub fn recently_reminded(&self, doc_id: &Uuid) -> bool {
        self.entries.contains_key(doc_id)
    }
}

/// 这张单的「本次是第几次提醒」= 台账里同类提醒的历史行数 + 1。
///
/// # 为什么幂等键必须带它
///
/// `uk_sys_notice_idem` 会把同键的第二次 dispatch 当重发直接吞掉。采购超期/低库存这类
/// 「状态型」提醒，过了重提醒间隔要**再发一次**，而那时 stage（如「逾期 5 天」）与日期
/// 可能和上一次不同 —— 但若键里不含次数，某些情形（台账行落在重提醒窗口之外、序号从 1
/// 重新开始）会撞上一次已存在的键，于是「第二次提醒」永远发不出去
/// （与 `flow::timeout::urge_idempotency_key` / `transfer_overdue` 同一个坑）。
///
/// 计数取**全历史**（不按窗口截断）：窗口只影响「要不要提醒」，不影响「这是第几次」。
pub async fn next_seq_of(kind: RemindKind, doc_id: Uuid) -> DbResult<i64> {
    let prior: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*)::bigint FROM biz_reminder_log WHERE remind_kind = ? AND doc_id = ?",
            vec![Value::Text(kind.code().to_string()), Value::from(doc_id)],
        )
        .await?;
    Ok(prior + 1)
}

/// 读台账快照（一次查询覆盖整类，避免逐条往返）。
///
/// # 参数
///
/// - `kind`：提醒类型；
/// - `day`：业务日；
/// - `window_days`：重提醒间隔天数（`1` = 只看今天）。
pub async fn ledger_snapshot(
    kind: RemindKind,
    day: NaiveDate,
    window_days: i64,
) -> DbResult<LedgerSnapshot> {
    let window_days = window_days.max(1);
    let since = day - nik::prelude::chrono::Duration::days(window_days - 1);
    let rows = db()
        .query_sql(
            "SELECT doc_id, MAX(remind_date)::text AS last_date \
               FROM biz_reminder_log \
              WHERE remind_kind = ? AND remind_date >= ?::text::date \
              GROUP BY doc_id"
                .to_string(),
            vec![
                Value::Text(kind.code().to_string()),
                Value::Text(since.format("%Y-%m-%d").to_string()),
            ],
        )
        .await?;
    let mut snapshot = LedgerSnapshot::default();
    for row in rows.iter() {
        let doc_id: Uuid = row.get_typed("doc_id")?;
        let raw: String = row.get_typed("last_date")?;
        let last_date = NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").ok();
        snapshot.entries.insert(doc_id, LedgerEntry { last_date });
    }
    Ok(snapshot)
}

/// 记一条台账（`ON CONFLICT DO NOTHING`，返回是否新插入）。
///
/// **先发通知、再记台账**：反过来的话，通知发送失败会永久吞掉这张单当天的提醒
/// （台账已有行，下一轮直接跳过）。代价是极小概率「通知发出但记账失败」→ 下一轮再发一次，
/// 那由幂等键（同一档位同一天同一个键）兜住。
pub async fn ledger_record(
    kind: RemindKind,
    day: NaiveDate,
    message: &ReminderMessage,
    notice_id: Uuid,
    receiver_count: i64,
) -> DbResult<u64> {
    db().exec_sql(
        "INSERT INTO biz_reminder_log \
             (doc_type, doc_id, doc_no, remind_kind, stage, remind_date, notice_id, receiver_count) \
         VALUES (?, ?, ?, ?, ?, ?::text::date, ?, ?) \
         ON CONFLICT (doc_type, doc_id, remind_kind, remind_date) DO NOTHING"
            .to_string(),
        vec![
            Value::Text(kind.doc_type().to_string()),
            Value::from(message.doc_id),
            Value::Text(message.doc_no.clone()),
            Value::Text(kind.code().to_string()),
            Value::Text(message.stage.clone()),
            Value::Text(day.format("%Y-%m-%d").to_string()),
            Value::from(notice_id),
            Value::I64(receiver_count),
        ],
    )
    .await
}

// ==================== 收件人解析 ====================

/// 按角色码取**启用用户** id（去重；角色不存在 / 没有成员 → 空列表）。
///
/// 收件人是人不是角色：`NoticeTarget::Role` 会把「此刻角色里有谁」交给扇出侧决定，
/// 而提醒要在台账里写清「发给了几个人」，两者口径必须一致，所以这里直接展开。
// SCOPE-EXEMPT: 后台扇出的收件人解析，没有请求操作人 ⇒ 不存在「操作人的数据范围」；收件人由角色定义决定（谁在角色里谁收提醒），注入范围会让同角色里的其他人静默收不到提醒；调用方全是调度器注册的后台扫描（reminder.rs:795 register_builtin → register_cron → tokio::spawn；业务侧 reminder_purchase.rs:106 / reminder_payable.rs:91 / reminder_contract.rs:116 / reminder_stock.rs:128）。
pub async fn users_of_roles(codes: &[&str]) -> DbResult<Vec<Uuid>> {
    if codes.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; codes.len()].join(", ");
    let sql = format!(
        "SELECT DISTINCT u.id AS id \
           FROM sys_user u \
           JOIN sys_user_role ur ON ur.user_id = u.id \
           JOIN sys_role r ON r.id = ur.role_id \
          WHERE u.deleted_at IS NULL AND u.status = 1 \
            AND r.deleted_at IS NULL AND r.status = 1 \
            AND r.role_code IN ({placeholders})"
    );
    let params = codes
        .iter()
        .map(|code| Value::Text(code.to_string()))
        .collect::<Vec<_>>();
    let rows = db().query_sql(sql, params).await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let id: Uuid = row.get_typed("id")?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// 合并收件人：剔除空值、去重（保序）、截断到 [`MAX_RECIPIENTS`]。
///
/// 截断时打 WARN（「收件人超上限」是配置问题 —— 例如某个「角色」里被塞进了整个公司）。
pub fn merge_recipients(ids: impl IntoIterator<Item = Option<Uuid>>) -> Vec<Uuid> {
    let mut out: Vec<Uuid> = Vec::new();
    for id in ids.into_iter().flatten() {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    if out.len() > MAX_RECIPIENTS {
        tracing::warn!(
            total = out.len(),
            limit = MAX_RECIPIENTS,
            "业务提醒收件人超过上限，超出部分被丢弃（请检查角色成员是否被批量误加）"
        );
        out.truncate(MAX_RECIPIENTS);
    }
    out
}

// ==================== 文案小工具 ====================

/// 把 PG 的 `NUMERIC::text`（如 `1234.5000`）去成人类可读：`1234.5`、`50`、`0`。
///
/// 为什么在 Rust 侧处理而不是 SQL `trim_scale()`：本仓 PostgreSQL 最低版本不保证有
/// `trim_scale`（PG 13+ 才有），且金额/数量的统一口径集中在一个纯函数里更好测。
pub fn decimal_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "0".to_string();
    }
    let (sign, body) = match trimmed.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", trimmed),
    };
    let body = if body.contains('.') {
        body.trim_end_matches('0').trim_end_matches('.')
    } else {
        body
    };
    let body = if body.is_empty() { "0" } else { body };
    if body == "0" {
        return "0".to_string();
    }
    format!("{sign}{body}")
}

// ==================== 发送 ====================

/// 单条发送结果（内部用，决定计数走哪一格）。
enum SendOutcome {
    /// 发出去了（`duplicated = false`）。
    Sent { notice_id: Uuid, receivers: i64 },
    /// 通知侧幂等命中（并发/多实例已发过同一档）。
    Duplicated { notice_id: Uuid, receivers: i64 },
}

/// 组装通知草稿（模板可选）。
fn build_draft(
    kind: RemindKind,
    day: NaiveDate,
    seq: i64,
    message: &ReminderMessage,
) -> NoticeDraft {
    let mut draft = NoticeDraft::text(
        NoticeType::Alert,
        NoticeLevel::Warn,
        message.title.clone(),
        message.content.clone(),
        NoticeTarget::User(message.recipients.clone()),
    );
    draft.biz_type = Some(kind.biz_type().to_string());
    draft.biz_id = Some(message.doc_id.to_string());
    draft.link_url = Some(kind.link_url().to_string());
    draft.idempotency_key = Some(idempotency_key(
        kind,
        message.doc_id,
        &message.stage,
        day,
        seq,
    ));
    draft.sender_name = Some(SENDER_NAME.to_string());
    // 到期/超期/断料提醒属于「漏了要赔钱」的一类，强制送达（跳过 notify.inbox 偏好），
    // 与 `transfer_overdue` / `flow::notify::FlowEvent::Urge` 同分级。
    draft.force = true;
    draft.template_code = Some(kind.template_code().to_string());
    draft.template_vars = message.vars.clone();
    draft
}

/// 发一条提醒：优先走模板，模板缺失/停用时退回代码内的纯文本。
///
/// 为什么要有兜底：模板是**运维可改**的配置行（停用 / 改名 / 被清理都可能发生），
/// 而提醒是业务闭环的一部分 —— 不能因为「模板被人停用了」就整批提醒消失。
async fn send_one(
    kind: RemindKind,
    day: NaiveDate,
    seq: i64,
    message: &ReminderMessage,
) -> std::result::Result<SendOutcome, String> {
    let draft = build_draft(kind, day, seq, message);
    let first = notice::dispatch(draft.clone()).await;
    let result = match first {
        Ok(result) => result,
        Err(DispatchError::Api(error)) if error.code == error_code::NOTICE_TEMPLATE_NOT_FOUND => {
            tracing::warn!(
                kind = kind.code(),
                template = kind.template_code(),
                "业务提醒模板缺失或已停用，退回代码内纯文本（提醒不丢）"
            );
            let mut fallback = draft;
            fallback.template_code = None;
            fallback.template_vars = serde_json::Value::Null;
            notice::dispatch(fallback)
                .await
                .map_err(|error| error.to_string())?
        }
        Err(error) => return Err(error.to_string()),
    };
    Ok(if result.duplicated {
        SendOutcome::Duplicated {
            notice_id: result.notice_id,
            receivers: result.receiver_count,
        }
    } else {
        SendOutcome::Sent {
            notice_id: result.notice_id,
            receivers: result.receiver_count,
        }
    })
}

// ==================== 一轮扫描 ====================

/// 扫一类并发送（可被定时任务 / HTTP / 单测直接调用）。
///
/// # 顺序
///
/// ```text
/// ① 取整类的待提醒消息（各 reminder_* 模块的 scan）
/// ② 读台账快照（今天 + 重提醒窗口）
/// ③ 逐条：今天已提醒 → 跳过；窗口内提醒过 → 跳过；无收件人 → WARN + 跳过
/// ④ 发通知（模板 → 兜底文本）→ 记台账
/// ```
///
/// 单条失败只计 `failed` 并继续下一条（与 `transfer_overdue` 同款）。
pub async fn run_kind(kind: RemindKind, now: DateTime<Utc>) -> DbResult<ScanReport> {
    let day = business_day(now);
    let window = kind.realert_days().await;
    let messages = match kind {
        RemindKind::PayableDue => reminder_payable::scan(now).await?,
        RemindKind::PoOverdue => reminder_purchase::scan(now).await?,
        RemindKind::ContractExpire => reminder_contract::scan(now).await?,
        RemindKind::LowStock => reminder_stock::scan(now).await?,
    };

    let mut report = ScanReport {
        scanned: messages.len(),
        ..ScanReport::default()
    };
    if messages.is_empty() {
        return Ok(report);
    }

    let snapshot = ledger_snapshot(kind, day, window).await?;
    for message in &messages {
        let doc_id = message.doc_id;
        if snapshot.reminded_today(&doc_id, day) {
            report.skipped_reminded += 1;
            continue;
        }
        if window > 1 && snapshot.recently_reminded(&doc_id) {
            report.skipped_recent += 1;
            tracing::debug!(
                kind = kind.code(),
                doc_no = %message.doc_no,
                window,
                "重提醒间隔内，跳过（防骚扰）"
            );
            continue;
        }
        if message.recipients.is_empty() {
            report.skipped_no_recipient += 1;
            tracing::warn!(
                kind = kind.code(),
                doc_type = kind.doc_type(),
                doc_id = %message.doc_id,
                doc_no = %message.doc_no,
                stage = %message.stage,
                rule = %message.recipient_rule,
                "业务提醒定位不到收件人（角色下没有启用用户 / 单据没有制单人），跳过并打点，请修数据或补角色成员"
            );
            continue;
        }

        let seq = next_seq_of(kind, doc_id).await?;
        match send_one(kind, day, seq, message).await {
            Ok(outcome) => {
                let (notice_id, receivers, duplicated) = match outcome {
                    SendOutcome::Sent {
                        notice_id,
                        receivers,
                    } => (notice_id, receivers, false),
                    SendOutcome::Duplicated {
                        notice_id,
                        receivers,
                    } => (notice_id, receivers, true),
                };
                match ledger_record(kind, day, message, notice_id, receivers).await {
                    Ok(1) => {
                        if duplicated {
                            report.skipped_reminded += 1;
                        } else {
                            report.sent += 1;
                        }
                    }
                    Ok(_) => {
                        // 台账行已被别的实例写上（通知侧也已幂等）—— 本轮不算新发
                        report.skipped_reminded += 1;
                    }
                    Err(error) => {
                        report.failed += 1;
                        tracing::warn!(
                            kind = kind.code(),
                            doc_no = %message.doc_no,
                            error = %error,
                            "业务提醒台账写入失败（通知已发出，下一轮可能重发一条同内容通知）"
                        );
                    }
                }
            }
            Err(error) => {
                report.failed += 1;
                tracing::warn!(
                    kind = kind.code(),
                    doc_no = %message.doc_no,
                    error = %error,
                    "业务提醒发送失败（本轮不记台账，下一轮重试）"
                );
            }
        }
    }
    Ok(report)
}

/// 扫全部 4 类（手动触发 `kind=all` 用）。
pub async fn run_all(now: DateTime<Utc>) -> DbResult<Vec<(RemindKind, ScanReport)>> {
    let mut out = Vec::with_capacity(RemindKind::ALL.len());
    for kind in RemindKind::ALL {
        out.push((kind, run_kind(kind, now).await?));
    }
    Ok(out)
}

// ==================== 注册 ====================

/// 在调度器上注册 4 个提醒任务（由 `task::Scheduler::register_builtin` 调用）。
pub fn register_builtin(scheduler: &Scheduler) {
    for kind in RemindKind::ALL {
        scheduler.register_cron(
            kind.task_name(),
            kind.cron(),
            kind.description(),
            move || {
                tokio::spawn(async move {
                    match run_kind(kind, Utc::now()).await {
                        Ok(report) => {
                            tracing::info!(summary = %report.summary(kind), "业务提醒扫描完成")
                        }
                        Err(error) => tracing::error!(
                            summary = %report_error(kind, &error),
                            "业务提醒扫描失败"
                        ),
                    }
                })
            },
        );
        tracing::info!(
            invoke_target = kind.task_name(),
            cron = kind.cron(),
            "业务提醒任务已注册"
        );
    }
}

/// 扫描失败的日志摘要（失败时没有 `ScanReport`，单独拼一行，保持字段名一致）。
fn report_error(kind: RemindKind, error: &DbError) -> String {
    format!(
        "{}: scanned=0 sent=0 failed=1 error={error}",
        kind.task_name()
    )
}

// ==================== 接口层（手动触发，验收入口） ====================

/// 手动触发的结果（每类一份）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReminderRunRes {
    /// 本次触发涉及的类型与计数（`kind=all` 时 4 条）。
    pub runs: Vec<KindRunRes>,
    /// 一句给人看的结论。
    pub message: String,
}

/// 单类触发结果。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct KindRunRes {
    /// 类型短码（`payable_due` …）。
    pub kind: String,
    /// 调度名（与 `sys_job.invoke_target` 一致）。
    pub task: String,
    /// 扫到的条数。
    pub scanned: i64,
    /// 发出的通知条数。
    pub sent: i64,
    /// 今天已提醒过而跳过。
    pub skipped_reminded: i64,
    /// 重提醒间隔内而跳过。
    pub skipped_recent: i64,
    /// 定位不到收件人而跳过。
    pub skipped_no_recipient: i64,
    /// 发送/记账失败。
    pub failed: i64,
    /// 一行摘要（与日志同格式）。
    pub summary: String,
}

impl KindRunRes {
    /// 由 [`ScanReport`] 组装。
    pub fn of(kind: RemindKind, report: &ScanReport) -> Self {
        Self {
            kind: kind.short_code().to_string(),
            task: kind.task_name().to_string(),
            scanned: report.scanned as i64,
            sent: report.sent as i64,
            skipped_reminded: report.skipped_reminded as i64,
            skipped_recent: report.skipped_recent as i64,
            skipped_no_recipient: report.skipped_no_recipient as i64,
            failed: report.failed as i64,
            summary: report.summary(kind),
        }
    }
}

/// 一句给人看的结论。
fn run_message(runs: &[KindRunRes]) -> String {
    let sent: i64 = runs.iter().map(|run| run.sent).sum();
    let scanned: i64 = runs.iter().map(|run| run.scanned).sum();
    let skipped: i64 = runs
        .iter()
        .map(|run| run.skipped_reminded + run.skipped_recent)
        .sum();
    let no_recipient: i64 = runs.iter().map(|run| run.skipped_no_recipient).sum();
    let failed: i64 = runs.iter().map(|run| run.failed).sum();
    if scanned == 0 {
        return "本次扫描没有需要提醒的单据".to_string();
    }
    let mut parts = vec![format!("扫到 {scanned} 条，已发 {sent} 条通知")];
    if skipped > 0 {
        parts.push(format!("{skipped} 条今天已提醒过（不重复打扰）"));
    }
    if no_recipient > 0 {
        parts.push(format!("{no_recipient} 条定位不到收件人（见 WARN 日志）"));
    }
    if failed > 0 {
        parts.push(format!("{failed} 条发送失败（可重试）"));
    }
    parts.join("，")
}

/// 手动触发一类（或全部）提醒扫描。
///
/// # 参数
///
/// - `kind_code`：`payable_due` / `po_overdue` / `contract_expire` / `low_stock` / `all`。
///
/// # 异常情况
///
/// 类型非法 → `400`；数据库异常 → `500`。
pub async fn run_by_code(kind_code: &str) -> std::result::Result<ReminderRunRes, ApiError> {
    let now = Utc::now();
    let mut runs = Vec::new();
    if kind_code.trim().eq_ignore_ascii_case("all") {
        for (kind, report) in run_all(now)
            .await
            .map_err(|e| utils::db_error(e, "执行业务提醒扫描"))?
        {
            runs.push(KindRunRes::of(kind, &report));
        }
    } else {
        let kind = RemindKind::from_short_code(kind_code).ok_or_else(|| {
            ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!(
                    "提醒类型非法：{kind_code}（允许 payable_due / po_overdue / contract_expire / low_stock / all）"
                ),
            )
        })?;
        let report = run_kind(kind, now)
            .await
            .map_err(|e| utils::db_error(e, "执行业务提醒扫描"))?;
        runs.push(KindRunRes::of(kind, &report));
    }
    let message = run_message(&runs);
    Ok(ReminderRunRes { runs, message })
}

/// 手动触发业务提醒扫描（验收 / 补跑入口）。
///
/// ```text
/// POST /business/erp/reminder/{kind}/run      kind = payable_due | po_overdue | contract_expire | low_stock | all
/// →   { runs: [{ kind, task, scanned, sent, skipped_*, failed, summary }], message }
/// ```
///
/// 幂等由台账保证：同一天连点 3 次，第 2、3 次 `sent = 0`（`skipped_reminded` 全中）。
#[nik::permission(
    path = "/business/erp/reminder/{kind}/run",
    method = "POST",
    permission = "business:erp:reminder:run"
)]
#[nik::post("/business/erp/reminder/{kind}/run")]
pub async fn reminder_run_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(kind): Path<String>,
) -> ResResult<ReminderRunRes> {
    period::ensure_permission(&claims, PERM_RUN, "手动触发业务提醒")?;
    Ok(Res::ok(run_by_code(&kind).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::{Duration, TimeZone};

    fn fixed_now() -> DateTime<Utc> {
        // 2026-11-03T02:00:00Z = 北京时间 2026-11-03 10:00
        Utc.with_ymd_and_hms(2026, 11, 3, 2, 0, 0)
            .single()
            .expect("固定时间")
    }

    /// ① 业务日按 Asia/Shanghai 折算：UTC 前一天 17:00 = 北京次日 01:00。
    #[test]
    fn business_day_uses_beijing_offset() {
        let day = business_day(fixed_now());
        assert_eq!(day.to_string(), "2026-11-03");

        // UTC 2026-11-03 17:30 → 北京 2026-11-04 01:30（业务日必须是 04 号）
        let later = Utc
            .with_ymd_and_hms(2026, 11, 3, 17, 30, 0)
            .single()
            .unwrap();
        assert_eq!(business_day(later).to_string(), "2026-11-04");

        // UTC 2026-11-03 15:59 → 北京 23:59，还是 03 号（差一分钟跨日）
        let edge = Utc
            .with_ymd_and_hms(2026, 11, 3, 15, 59, 0)
            .single()
            .unwrap();
        assert_eq!(business_day(edge).to_string(), "2026-11-03");
    }

    /// ② 幂等键：同单据同档同日同次稳定；换档 / 换日 / 换次必须变（否则重提醒被吞）。
    #[test]
    fn idempotency_key_shape() {
        let id = Uuid::parse_str("0199bbbb-0000-7000-8000-000000000001").expect("固定 uuid");
        let day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        let key = idempotency_key(RemindKind::PayableDue, id, "D-7", day, 1);
        assert_eq!(
            key,
            idempotency_key(RemindKind::PayableDue, id, "D-7", day, 1),
            "同输入必须稳定"
        );
        assert!(
            key.starts_with("ERP:PAYABLE_DUE:"),
            "键前缀要能一眼看出业务：{key}"
        );
        assert_ne!(
            key,
            idempotency_key(RemindKind::PayableDue, id, "D-3", day, 1),
            "同一张单的不同档位必须是不同的键"
        );
        assert_ne!(
            key,
            idempotency_key(
                RemindKind::PayableDue,
                id,
                "D-7",
                day + Duration::days(1),
                1
            ),
            "跨业务日必须是不同的键"
        );
        assert_ne!(
            key,
            idempotency_key(RemindKind::PayableDue, id, "D-7", day, 2),
            "重提醒的「第 N 次」必须是不同的键（否则第二次提醒被唯一索引吞掉）"
        );
        // sys_notice.idempotency_key 是 VARCHAR(128)：最长档位 `OVERDUE-9999` + 4 位序号也不能超
        let longest = idempotency_key(RemindKind::PoOverdue, id, "OVERDUE-9999", day, 9999);
        assert!(longest.len() <= 128, "幂等键超长：{}", longest.len());
    }

    /// ③ 收件人合并：剔空、去重（保序）、超上限截断。
    #[test]
    fn merge_recipients_dedup_and_cap() {
        let a = Uuid::parse_str("0199bbbb-0000-7000-8000-000000000011").unwrap();
        let b = Uuid::parse_str("0199bbbb-0000-7000-8000-000000000012").unwrap();
        assert_eq!(
            merge_recipients([Some(a), None, Some(b), Some(a)]),
            vec![a, b],
            "去重保序、剔空"
        );
        assert!(
            merge_recipients([None, None]).is_empty(),
            "全空必须是空列表"
        );

        let many = (0..MAX_RECIPIENTS + 5)
            .map(|index| Uuid::from_u128(index as u128 + 1))
            .map(Some)
            .collect::<Vec<_>>();
        assert_eq!(merge_recipients(many).len(), MAX_RECIPIENTS, "超上限要截断");
    }

    /// ④ 元数据是不变量：调度名 / cron / 模板码 / biz_type 与 V0210 迁移逐字一致
    ///    （改名等于「提醒静默消失」或「模板 404」，迁移自检也钉了同一串）。
    #[test]
    fn kind_metadata_is_stable() {
        let expected = [
            (
                RemindKind::PayableDue,
                "erp_payable_due_scan",
                "0 10 7 * * *",
                "ERP_REMINDER_PAYABLE_DUE",
                "payable_due",
                "PAYABLE_DUE",
                "PAYABLE",
            ),
            (
                RemindKind::PoOverdue,
                "erp_po_overdue_scan",
                "0 20 7 * * *",
                "ERP_REMINDER_PO_OVERDUE",
                "po_overdue",
                "PO_OVERDUE",
                "PURCHASE_ORDER",
            ),
            (
                RemindKind::ContractExpire,
                "erp_contract_expire_scan",
                "0 30 7 * * *",
                "ERP_REMINDER_CONTRACT_EXPIRE",
                "contract_expire",
                "CONTRACT_EXPIRE",
                "CONTRACT",
            ),
            (
                RemindKind::LowStock,
                "erp_low_stock_scan",
                "0 40 7 * * *",
                "ERP_REMINDER_LOW_STOCK",
                "low_stock",
                "LOW_STOCK",
                "INVENTORY",
            ),
        ];
        for (kind, task, cron, template, short, code, doc_type) in expected {
            assert_eq!(kind.task_name(), task);
            assert_eq!(kind.cron(), cron);
            assert_eq!(kind.template_code(), template);
            assert_eq!(kind.short_code(), short);
            assert_eq!(kind.code(), code);
            assert_eq!(kind.doc_type(), doc_type);
            assert_eq!(RemindKind::from_task_name(task), Some(kind));
            assert_eq!(RemindKind::from_short_code(short), Some(kind));
            assert_eq!(
                kind.cron().split_whitespace().count(),
                6,
                "必须是 6 段 cron（第 1 段秒）：{}",
                kind.cron()
            );
            assert!(
                kind.cron().starts_with("0 ") || kind.cron().starts_with("0 "),
                "cron 分位必须是整分（错开整点即可，别卡 0 秒之外）"
            );
        }
        assert_eq!(RemindKind::from_short_code("nope"), None);
    }

    /// ⑤ 摘要行的字段名是运维 grep 契约，不能悄悄改名。
    #[test]
    fn report_summary_shape() {
        let report = ScanReport {
            scanned: 3,
            sent: 2,
            skipped_reminded: 1,
            skipped_recent: 0,
            skipped_no_recipient: 0,
            failed: 0,
        };
        assert_eq!(
            report.summary(RemindKind::PayableDue),
            "erp_payable_due_scan: scanned=3 sent=2 skipped_reminded=1 skipped_recent=0 \
             skipped_no_recipient=0 failed=0"
        );
    }

    /// ⑥ 结论句：扫不到不要写「已发 0 条通知」，要写人话。
    #[test]
    fn run_message_is_human() {
        let empty = KindRunRes::of(RemindKind::LowStock, &ScanReport::default());
        assert_eq!(run_message(&[empty]), "本次扫描没有需要提醒的单据");

        let report = ScanReport {
            scanned: 2,
            sent: 1,
            skipped_no_recipient: 1,
            ..Default::default()
        };
        let message = run_message(&[KindRunRes::of(RemindKind::LowStock, &report)]);
        assert!(message.contains("扫到 2 条"), "{message}");
        assert!(message.contains("已发 1 条通知"), "{message}");
        assert!(message.contains("定位不到收件人"), "{message}");
    }
}
