//! 在途调拨超期告警（LNK-F12 · 在途仓「压货不可见」）。
//!
//! # 它解决什么
//!
//! 调拨单 6 态状态机里，`IN_TRANSIT`（已发货未收货）是唯一**两边都不记账**的中间态：
//! 调出仓在结算时才扣、调入仓在结算时才加。于是「发货方点了发货、收货方一直没点收货」
//! 这种单子会永久停在 `IN_TRANSIT` —— 库存既不在调出仓也不在调入仓，账实差异只能靠
//! 人肉发现。本模块是**唯一的「超期」判定与告警入口**：
//!
//! ```text
//! 每小时第 5 分钟（transfer_overdue_scan）
//!   ├─ 扫出「IN_TRANSIT 且 shipped_at < now - 阈值」的单
//!   ├─ 发站内告警（复用 business::notice::dispatch，不另造通道）
//!   └─ 记台账（overdue_alerted_at / overdue_alert_count，CAS）
//! ```
//!
//! # 判定口径（唯一真源在 `transfer_model`）
//!
//! | 项 | 口径 |
//! |---|---|
//! | 状态 | `status = 'IN_TRANSIT'`（其它 5 态一律不算） |
//! | 时间基准 | `shipped_at`（发货时间），不是 `created_at` / `approved_at` |
//! | 阈值 | `sys_config.erp.transfer.in_transit_overdue_days`，缺省 **7 天** |
//! | 边界 | `shipped_at < now - 阈值` 才算（恰好等于阈值不算，与 SQL 同口径） |
//! | 重提醒间隔 | `sys_config.erp.transfer.in_transit_realert_hours`，缺省 **24 小时** |
//!
//! 纯函数在 [`transfer_model::is_in_transit_overdue`]，库查询在
//! [`transfer_model::scan_overdue_candidates`]，两者共用
//! [`transfer_model::cutoff_of`] 算出的同一个分界点 —— 不允许出现「函数说超期、
//! SQL 说不超期」两套口径。
//!
//! # 通知的五个纪律
//!
//! 1. **走既有设施**：`business::notice::dispatch`（模板渲染 / 扇出 / 偏好过滤 /
//!    幂等键 / 投递记录全免费继承）。本模块的产物只是一份 [`NoticeDraft`]。
//! 2. **收件人是「人」，不是角色**：申请人（发货方）+ 审批人（批这张单的人）。
//!    两边都没有（历史脏数据）就跳过并打 WARN —— 不往「全员」推，压货告警推给全员
//!    等于没人负责。
//! 3. **幂等双层**：通知侧 `idempotency_key` 带「第几次」（`…:<count+1>`），
//!    台账侧 `UPDATE … WHERE overdue_alert_count = <读到的旧值>` 的 CAS。
//!    任一层命中都不会产生重复通知。
//! 4. **先发通知，再记台账**：反过来的话，通知发送失败会永久吞掉这张单的告警
//!    （台账已 +1，下轮不再捞到）。代价是「极小概率重发一条同内容的通知」，
//!    那由幂等键兜住。
//! 5. **单条失败不影响同批**：一张单的 dispatch 失败只计 `failed`，继续下一张；
//!    数据库异常逐条吞掉并记日志（与 `flow::timeout` 同款）。
//!
//! # 注册位置
//!
//! 必须由 [`crate::task::Scheduler::register_builtin`] 调用
//! （框架调度器对注册表做一次性快照，写在 `bootstrap.rs` 里会被随后的 `reload()` 清空）。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::transfer_model::{self, OverdueCandidate};
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeDraft, NoticeTarget};
use crate::task::Scheduler;

/// 调度任务名（同时用作调度名与日志标识）。
pub const TASK_NAME: &str = "transfer_overdue_scan";

/// cron 表达式：每小时第 5 分钟扫一次（6 段写法，第 1 段是秒）。
///
/// 取「每小时」而不是「每分钟」：压货是按天计的，业务感知是「今天有没有人提醒我」；
/// 每小时一次的扫描量（走 V0171 的部分索引）在几千张在途单的量级下不到 10ms。
pub const TASK_CRON: &str = "0 5 * * * *";

/// 通知的业务类型（写进 `sys_notice.biz_type`，通知中心按它分组）。
pub const BIZ_TYPE: &str = "transfer_overdue";

/// 跳转链接：调拨列表页（前端暂无详情页路由，列表页可按单号筛）。
pub const LINK_URL: &str = "/business/erp/transfer";

/// 发送人展示名（系统任务，没有真实操作人）。
pub const SENDER_NAME: &str = "仓储作业";

/// 一轮扫描的处理结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// 本轮扫到的「该告警」单数。
    pub scanned: usize,
    /// 真的发出告警的单数。
    pub alerted: u64,
    /// 跳过（无收件人 / 通知侧幂等命中 / 台账 CAS 未命中）。
    pub skipped: u64,
    /// 发送或记账失败的条数（下轮自动重试）。
    pub failed: u64,
}

impl ScanReport {
    /// 一行可读摘要（写日志用，运维 grep 依赖它）。
    pub fn summary(&self) -> String {
        format!(
            "{TASK_NAME}: scanned={} alerted={} skipped={} failed={}",
            self.scanned, self.alerted, self.skipped, self.failed
        )
    }
}

/// 通知幂等键：同一张单的第 N 次告警必须命中同一个键。
///
/// # 为什么必须带「第几次」
///
/// `uk_sys_notice_idem` 会把同键的第二次 dispatch 当成重发直接吞掉。不带次数时，
/// 「第 2 次重提醒」（24 小时后再推一条）永远发不出去 —— 与 `flow::timeout::urge_idempotency_key`
/// 同一个坑。
pub fn idempotency_key(transfer_id: &Uuid, alert_seq: i16) -> String {
    format!("TRANSFER:IN_TRANSIT_OVERDUE:{transfer_id}:{alert_seq}")
}

/// 这张单的告警收件人（去重、剔空）。
///
/// 申请人（发货方）与审批人两边都算：货压在途中，发货方最该去催，审批人也该知道
/// 「自己批的单没落地」。
pub fn recipients_of(candidate: &OverdueCandidate) -> Vec<Uuid> {
    let mut ids = Vec::with_capacity(2);
    for id in [candidate.applicant_id, candidate.approver_id]
        .into_iter()
        .flatten()
    {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// 组装一条超期告警草稿（纯函数，便于单测钉住文案与幂等键）。
///
/// # 参数
///
/// - `candidate`：扫描到的那一行。
/// - `now`：本轮扫描的时间基准（与判定同源）。
/// - `threshold_days`：阈值（文案里写「超过 N 天」）。
/// - `from_name` / `to_name`：两侧仓库名（查不到时回落成 id，不 panic）。
pub fn notice_draft(
    candidate: &OverdueCandidate,
    now: DateTime<Utc>,
    threshold_days: i64,
    from_name: &str,
    to_name: &str,
) -> Option<NoticeDraft> {
    let recipients = recipients_of(candidate);
    if recipients.is_empty() {
        return None;
    }
    let elapsed = transfer_model::overdue_elapsed_days(candidate.shipped_at, now);
    let alert_seq = candidate.overdue_alert_count.saturating_add(1);
    let round = if alert_seq > 1 {
        format!("（第 {alert_seq} 次提醒）")
    } else {
        String::new()
    };
    let title = format!(
        "在途调拨超期 {elapsed} 天未收货：{}{round}",
        candidate.transfer_no
    );
    let content = format!(
        "调拨单 {} 于 {} 发货，已压 {elapsed} 天仍未收货（阈值 {threshold_days} 天）。\n\
         调出仓：{from_name}\n调入仓：{to_name}\n\
         这段时间库存既不在调出仓也不在调入仓（结算时才双写），请尽快让收货方点「收货」，\
         或撤回 / 取消这张单。",
        candidate.transfer_no,
        candidate.shipped_at.format("%Y-%m-%d %H:%M"),
    );
    let mut draft = NoticeDraft::text(
        NoticeType::Alert,
        NoticeLevel::Warn,
        title,
        content,
        NoticeTarget::User(recipients),
    );
    draft.biz_type = Some(BIZ_TYPE.to_string());
    draft.biz_id = Some(candidate.id.to_string());
    draft.link_url = Some(LINK_URL.to_string());
    draft.idempotency_key = Some(idempotency_key(&candidate.id, alert_seq));
    draft.sender_name = Some(SENDER_NAME.to_string());
    // 压货告警是「漏了就造成账实差异」的一类，强制送达（跳过 notify.inbox 偏好）；
    // 与 `flow::notify::FlowEvent::Urge` 同分级。
    draft.force = true;
    Some(draft)
}

/// 执行一轮扫描（可被单测 / 运维脚本直接调用）。
///
/// # 异常情况
///
/// 扫描 SQL 本身失败 → `Err`（调用方只记日志，不影响服务）；
/// 单条通知 / 单条台账失败 → 计入 [`ScanReport::failed`] 并继续下一张。
///
/// SCOPE-EXEMPT: 后台定时任务没有请求者身份，按 `status + shipped_at` 全局取数；
/// 注入数据范围会让别人的压货单永不告警（调拨单本身也无 dept_id / 归属人列）。
pub async fn scan_once(now: DateTime<Utc>) -> std::result::Result<ScanReport, DbError> {
    let threshold_days = transfer_model::overdue_threshold_days().await;
    let realert_hours = transfer_model::realert_hours().await;
    let candidates = transfer_model::scan_overdue_candidates(
        db(),
        now,
        threshold_days,
        realert_hours,
        transfer_model::OVERDUE_SCAN_BATCH,
    )
    .await?;

    let mut report = ScanReport {
        scanned: candidates.len(),
        ..ScanReport::default()
    };
    if candidates.is_empty() {
        return Ok(report);
    }

    // 仓库名一次查完（批量，避免 N+1）
    let mut ids = Vec::with_capacity(candidates.len() * 2);
    for candidate in &candidates {
        ids.push(candidate.from_warehouse_id);
        ids.push(candidate.to_warehouse_id);
    }
    let names = transfer_model::warehouse_names(&ids)
        .await
        .unwrap_or_default();
    let name_of = |id: Uuid| names.get(&id).cloned().unwrap_or_else(|| id.to_string());

    for candidate in &candidates {
        let Some(draft) = notice_draft(
            candidate,
            now,
            threshold_days,
            &name_of(candidate.from_warehouse_id),
            &name_of(candidate.to_warehouse_id),
        ) else {
            report.skipped += 1;
            tracing::warn!(
                transfer_id = %candidate.id,
                transfer_no = %candidate.transfer_no,
                "在途超期单既没有申请人也没有审批人，无法确定告警收件人，跳过（请修数据）"
            );
            continue;
        };

        // ① 先发通知：发送失败就不记台账，下一轮自然重试
        match notice::dispatch(draft).await {
            Ok(result) if result.duplicated => {
                // 同一轮里被别的实例先发过（幂等键命中）
                report.skipped += 1;
                continue;
            }
            Ok(_) => {}
            Err(error) => {
                report.failed += 1;
                tracing::warn!(
                    transfer_id = %candidate.id,
                    error = %error,
                    "在途调拨超期告警发送失败（本轮不记台账，下轮重试）"
                );
                continue;
            }
        }

        // ② 再记台账（CAS：计数仍是我读到的旧值才 +1）
        match transfer_model::mark_overdue_alerted(
            db(),
            candidate.id,
            candidate.overdue_alert_count,
            now,
        )
        .await
        {
            Ok(1) => report.alerted += 1,
            Ok(_) => {
                // 被别的扫描抢先记了台账（通知侧已经幂等，不会重复推）
                report.skipped += 1;
                tracing::debug!(
                    transfer_id = %candidate.id,
                    "在途超期台账 CAS 未命中（已被其它实例处理）"
                );
            }
            Err(error) => {
                report.failed += 1;
                tracing::warn!(
                    transfer_id = %candidate.id,
                    error = %error,
                    "在途超期台账写入失败（通知已发出，下轮会再推一次）"
                );
            }
        }
    }
    Ok(report)
}

/// 在调度器上注册超期扫描任务（由 `task::Scheduler::register_builtin` 调用）。
pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        TASK_CRON,
        "系统任务：在途调拨超期扫描与告警（LNK-F12）",
        || {
            tokio::spawn(async move {
                match scan_once(Utc::now()).await {
                    Ok(report) => {
                        tracing::info!(summary = %report.summary(), "在途调拨超期扫描完成")
                    }
                    Err(error) => tracing::error!(error = %error, "在途调拨超期扫描失败"),
                }
            })
        },
    );
    tracing::info!(
        invoke_target = TASK_NAME,
        cron = TASK_CRON,
        "在途调拨超期扫描任务已注册"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::Duration;

    fn candidate(count: i16) -> OverdueCandidate {
        OverdueCandidate {
            id: Uuid::parse_str("0199aaaa-0000-7000-8000-000000000001").expect("固定 uuid"),
            transfer_no: "TK-20261001-000007".to_string(),
            applicant_id: Some(
                Uuid::parse_str("0199aaaa-0000-7000-8000-000000000011").expect("固定 uuid"),
            ),
            approver_id: Some(
                Uuid::parse_str("0199aaaa-0000-7000-8000-000000000012").expect("固定 uuid"),
            ),
            shipped_at: Utc::now() - Duration::days(30),
            overdue_alert_count: count,
            from_warehouse_id: Uuid::parse_str("0199aaaa-0000-7000-8000-000000000021")
                .expect("固定 uuid"),
            to_warehouse_id: Uuid::parse_str("0199aaaa-0000-7000-8000-000000000022")
                .expect("固定 uuid"),
        }
    }

    /// ① 幂等键：同一张单不同「第几次」必须是不同键（否则第二次重提醒被吞）。
    #[test]
    fn idempotency_key_changes_with_seq() {
        let id = candidate(0).id;
        let first = idempotency_key(&id, 1);
        let second = idempotency_key(&id, 2);
        assert_ne!(first, second);
        assert_eq!(first, idempotency_key(&id, 1), "同键必须稳定");
        assert!(
            first.starts_with("TRANSFER:IN_TRANSIT_OVERDUE:"),
            "键前缀要能一眼看出业务：{first}"
        );
        // `sys_notice.idempotency_key` 是 VARCHAR(128)：超长会被库拒绝
        assert!(second.len() <= 128, "幂等键超长：{}", second.len());
    }

    /// ② 收件人：申请人 + 审批人，去重、剔空；两边都空 → 空列表（调用方跳过并 WARN）。
    #[test]
    fn recipients_dedup_and_drop_empty() {
        let mut one = candidate(0);
        one.approver_id = None;
        assert_eq!(recipients_of(&one).len(), 1, "只有申请人时只推给申请人");

        let mut same = candidate(0);
        same.approver_id = same.applicant_id;
        assert_eq!(
            recipients_of(&same).len(),
            1,
            "申请人与审批人相同时不重复推"
        );

        let mut none = candidate(0);
        none.applicant_id = None;
        none.approver_id = None;
        assert!(recipients_of(&none).is_empty(), "没有收件人时必须是空列表");
        assert!(
            notice_draft(&none, Utc::now(), 7, "A", "B").is_none(),
            "没有收件人时不应产出草稿"
        );
    }

    /// ③ 告警文案：带单号、已压天数、阈值与两侧仓库名；标题不超列宽（255）。
    #[test]
    fn notice_draft_carries_facts() {
        let now = Utc::now();
        let draft =
            notice_draft(&candidate(0), now, 7, "调出仓 A", "调入仓 B").expect("应产出草稿");
        assert_eq!(draft.notice_type, NoticeType::Alert);
        assert_eq!(draft.level, NoticeLevel::Warn);
        assert!(draft.force, "压货告警必须强制送达");
        let title = &draft.title;
        assert!(
            title.contains("TK-20261001-000007"),
            "标题要带单号：{title}"
        );
        assert!(title.contains("超期"), "标题要写清是超期：{title}");
        assert!(
            title.chars().count() <= 255,
            "标题超列宽：{}",
            title.chars().count()
        );
        for fact in ["TK-20261001-000007", "阈值 7 天", "调出仓 A", "调入仓 B"] {
            assert!(
                draft.content.contains(fact),
                "正文缺事实 `{fact}`：{}",
                draft.content
            );
        }
        assert_eq!(draft.biz_type.as_deref(), Some(BIZ_TYPE));
        let expected_id = candidate(0).id.to_string();
        assert_eq!(draft.biz_id.as_deref(), Some(expected_id.as_str()));
        let expected_key = idempotency_key(&candidate(0).id, 1);
        assert_eq!(
            draft.idempotency_key.as_deref(),
            Some(expected_key.as_str()),
            "首次告警的幂等键序号必须是 1"
        );
    }

    /// ④ 第二次（重）提醒：文案标注「第 2 次提醒」，幂等键序号跟着变。
    #[test]
    fn realert_is_marked_as_second_round() {
        let draft = notice_draft(&candidate(1), Utc::now(), 7, "A", "B").expect("应产出草稿");
        assert!(
            draft.title.contains("第 2 次提醒"),
            "重提醒要在标题里标次数：{}",
            draft.title
        );
        let expected = idempotency_key(&candidate(1).id, 2);
        assert_eq!(draft.idempotency_key.as_deref(), Some(expected.as_str()));
    }

    /// ⑤ 摘要行：运维 grep 依赖它的字段名，不能悄悄改名。
    #[test]
    fn report_summary_shape() {
        let report = ScanReport {
            scanned: 3,
            alerted: 2,
            skipped: 1,
            failed: 0,
        };
        assert_eq!(
            report.summary(),
            "transfer_overdue_scan: scanned=3 alerted=2 skipped=1 failed=0"
        );
    }

    /// ⑥ 任务注册元数据：名字与 cron 是运维/日志的契约（改名等于告警静默消失）。
    #[test]
    fn task_metadata_is_stable() {
        assert_eq!(TASK_NAME, "transfer_overdue_scan");
        assert_eq!(TASK_CRON, "0 5 * * * *");
        assert_eq!(
            TASK_CRON.split_whitespace().count(),
            6,
            "必须是 6 段 cron（第 1 段秒）"
        );
        assert_eq!(LINK_URL, "/business/erp/transfer");
    }
}
