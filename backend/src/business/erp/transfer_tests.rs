// ============================================================================
// transfer_tests.rs —— 测试部件：`erp/transfer_model` 的全部用例（单号/6 态状态机/成本口径/真库流转）
//
// 由 `transfer_model.rs` 的 `#[cfg(test)] mod tests` 原样搬出（TD-1 拆分）。**本文件不含任何行为变更**：
// 用例体、夹具、SQL、断言与拆分前一致 —— 用例数 10 → 10、用例名逐一相同。
// 挂载点写在 facade `transfer_model.rs` 末尾；本文件自带完整 import 头，见方法论 §3 坑 1。
// ============================================================================

// 拆分前本测试模块挂在 `transfer_model.rs` 内部，`use super::*` 同时传递父模块的
// 公开项与私有 `use`；独立成文件后私有 `use` 不再传递，故原样补回父模块 import 头。
use nik::prelude::chrono::{Duration, Utc};
use nik::uuid::Uuid;

use super::*;

/// ① 单号格式：`TK-yyyyMMdd-000001`（卡面 §五 ①）。
#[test]
fn transfer_no_matches_card_format() {
    assert_eq!(format_transfer_no("20261030", 1), "TK-20261030-000001");
    assert_eq!(format_transfer_no("20261030", 42), "TK-20261030-000042");
    // 超过 6 位不截断：宁可长一位也不撞号
    assert_eq!(format_transfer_no("20261030", 1_000_000), "TK-20261030-1000000");
}

/// ② 状态机：6 条合法边全部放行。
#[test]
fn status_machine_allows_six_card_edges() {
    for (from, to) in LEGAL_EDGES {
        assert!(can_transition(from, to), "{from} → {to} 必须放行");
    }
    assert_eq!(LEGAL_EDGES.len(), 6);
}

/// P5：审批门闸（`65030`）的三条判据 —— 纯逻辑，每条都正反两面钉住。
#[test]
fn flow_gate_blocks_only_manual_approve_of_running_instance() {
    let instance = Uuid::now_v7();
    // ① 审批中 + 人工直推 APPROVED → 拦
    assert!(
        flow_gate_blocked(STATUS_PENDING, STATUS_APPROVED, Some(instance), Some("PENDING")),
        "审批中的人工批准必须被拦（否则审批单永远挂着脏待办）"
    );
    // 前后空白 / 小写也算审批中（库里是 VARCHAR，不保证大小写）
    assert!(flow_gate_blocked(
        STATUS_PENDING,
        STATUS_APPROVED,
        Some(instance),
        Some(" pending ")
    ));

    // ② 没挂审批单（降级路径）→ 放行
    assert!(!flow_gate_blocked(STATUS_PENDING, STATUS_APPROVED, None, None));
    // ③ 审批单已落终态 → 放行（这时放行人工推是修数据，拦住会把单据锁死）
    for status in ["APPROVED", "REJECTED", "CANCELLED", "WITHDRAWN"] {
        assert!(
            !flow_gate_blocked(STATUS_PENDING, STATUS_APPROVED, Some(instance), Some(status)),
            "实例已是 {status} 时不该拦"
        );
    }
    // ④ 撤回（PENDING → CANCELLED）永远放行：这是「不想审了」的正规出口
    assert!(!flow_gate_blocked(
        STATUS_PENDING,
        STATUS_CANCELLED,
        Some(instance),
        Some("PENDING")
    ));
    // ⑤ 其它边与本闸无关
    for (from, to) in LEGAL_EDGES {
        if from == STATUS_PENDING && to == STATUS_APPROVED {
            continue;
        }
        assert!(
            !flow_gate_blocked(from, to, Some(instance), Some("PENDING")),
            "{from} → {to} 不该被审批门闸拦"
        );
    }
}

/// ③ 状态机：跳级 / 回退 / 原地 / 终态再流转全部拒绝（卡面 §五 ⑧⑬）。
#[test]
fn status_machine_rejects_jumps_and_terminal() {
    assert!(!can_transition(STATUS_DRAFT, STATUS_DONE), "DRAFT 不能直接 DONE");
    assert!(!can_transition(STATUS_DRAFT, STATUS_APPROVED), "DRAFT 不能跳审批");
    assert!(!can_transition(STATUS_PENDING, STATUS_IN_TRANSIT));
    assert!(!can_transition(STATUS_APPROVED, STATUS_DONE), "必须经过在途");
    assert!(!can_transition(STATUS_DONE, STATUS_CANCELLED), "DONE 是终态");
    assert!(!can_transition(STATUS_CANCELLED, STATUS_DRAFT), "CANCELLED 不可复活");
    assert!(!can_transition(STATUS_PENDING, STATUS_PENDING), "原地流转也算非法");
    // 非法边的文案要点名「当前态的可选去向」
    let message = transition_rejected_message(STATUS_DRAFT, STATUS_DONE);
    assert!(message.contains("PENDING"), "{message}");
    assert!(message.contains("CANCELLED"), "{message}");
    let terminal = transition_rejected_message(STATUS_DONE, STATUS_DRAFT);
    assert!(terminal.contains("终态"), "{terminal}");
}

/// ④ 每条合法边写入后仍然满足 `ck_biz_transfer_state`（逐边对照，含在途仓）。
#[test]
fn every_legal_edge_keeps_db_state_check_holds() {
    // 流转前的快照就按「当前状态应有的样子」造
    let before_of = |status: &str| StateTimestamps {
        submitted_at: !matches!(status, STATUS_DRAFT),
        approved_at: matches!(status, STATUS_APPROVED | STATUS_IN_TRANSIT | STATUS_DONE),
        shipped_at: matches!(status, STATUS_IN_TRANSIT | STATUS_DONE),
        received_at: matches!(status, STATUS_DONE),
        cancelled_at: matches!(status, STATUS_CANCELLED),
        transit_warehouse_id: matches!(status, STATUS_IN_TRANSIT | STATUS_DONE),
        cancel_reason: matches!(status, STATUS_CANCELLED),
    };
    for (from, to) in LEGAL_EDGES {
        let before = before_of(from);
        assert!(state_check_holds(from, &before), "夹具自身不满足库约束：{from}");
        let effect = transition_effect(from, to).expect("合法边必须有 effect");
        let after = apply_effect(before, &effect);
        assert!(
            state_check_holds(to, &after),
            "{from} → {to} 写入后会违反 ck_biz_transfer_state：{after:?}"
        );
    }
}

/// ⑤ 反向对照：漏写时间戳 / 漏回填在途仓会被 `state_check_holds` 抓住（证明校验不是恒真）。
#[test]
fn state_check_catches_missing_timestamps() {
    // IN_TRANSIT 少了 shipped_at
    let broken = StateTimestamps {
        submitted_at: true,
        approved_at: true,
        shipped_at: false,
        received_at: false,
        cancelled_at: false,
        transit_warehouse_id: true,
        cancel_reason: false,
    };
    assert!(!state_check_holds(STATUS_IN_TRANSIT, &broken));
    // IN_TRANSIT 少了在途仓
    let no_transit = StateTimestamps {
        shipped_at: true,
        transit_warehouse_id: false,
        ..broken
    };
    assert!(!state_check_holds(STATUS_IN_TRANSIT, &no_transit));
    // CANCELLED 没有原因
    let no_reason = StateTimestamps {
        cancelled_at: true,
        cancel_reason: false,
        ..no_transit
    };
    assert!(!state_check_holds(STATUS_CANCELLED, &no_reason));
    // 审批驳回打回 DRAFT：留着 submitted_at 就违反约束
    let still_submitted = StateTimestamps {
        submitted_at: true,
        ..no_reason
    };
    assert!(!state_check_holds(STATUS_DRAFT, &still_submitted));
}

/// ⑥ 状态中文名覆盖 6 态 + 未知值兜底（列表 `status_label` 不能 panic）。
#[test]
fn status_labels_cover_all_states() {
    for status in ALL_STATUSES {
        assert_ne!(status_label(status), "未知状态", "{status} 没有中文名");
    }
    assert_eq!(status_label("WHATEVER"), "未知状态");
}

/// ⑦ LNK-F12：超期在途判定的四条边界（纯函数，与 SQL 同一分界点）。
///
/// 这条用例存在的意义：SQL 侧 `shipped_at < cutoff` 与 Rust 侧
/// [`is_in_transit_overdue`] 必须**同时**认为「恰好 7 天」不超期 ——
/// 两套口径差一微秒就会出现「列表筛不出来、扫描任务却告警」。
#[test]
fn overdue_boundary_is_strict_and_status_bound() {
    let now = Utc::now();
    let threshold = DEFAULT_OVERDUE_DAYS;
    let cutoff = cutoff_of(now, threshold);

    // ① 恰好等于阈值（`shipped_at == cutoff`）不算超期
    assert!(
        !is_in_transit_overdue(STATUS_IN_TRANSIT, Some(cutoff), now, threshold),
        "恰好 {threshold} 天不算超期（必须是严格小于）"
    );
    // ② 早一秒就算超期
    assert!(
        is_in_transit_overdue(STATUS_IN_TRANSIT, Some(cutoff - Duration::seconds(1)), now, threshold),
        "超过阈值 1 秒即算超期"
    );
    // ③ 非 IN_TRANSIT 一律不算（哪怕压了 30 天）
    for status in [
        STATUS_DRAFT,
        STATUS_PENDING,
        STATUS_APPROVED,
        STATUS_DONE,
        STATUS_CANCELLED,
    ] {
        assert!(
            !is_in_transit_overdue(status, Some(now - Duration::days(30)), now, threshold),
            "{status} 不该被判为超期在途"
        );
    }
    // ④ 在途但没有 shipped_at（脏数据）→ 不告警，不误报
    assert!(!is_in_transit_overdue(STATUS_IN_TRANSIT, None, now, threshold));
}

/// ⑧ LNK-F12：已压天数向下取整；未来时间不出现负数。
#[test]
fn overdue_elapsed_days_floor_and_clamp() {
    let now = Utc::now();
    assert_eq!(overdue_elapsed_days(now - Duration::days(30), now), 30);
    assert_eq!(
        overdue_elapsed_days(now - Duration::days(30) - Duration::hours(23), now),
        30,
        "30 天 23 小时向下取整是 30（不是四舍五入成 31）"
    );
    assert_eq!(
        overdue_elapsed_days(now - Duration::hours(5), now),
        0,
        "不足一天算 0 天"
    );
    assert_eq!(
        overdue_elapsed_days(now + Duration::hours(5), now),
        0,
        "虚拟时钟回拨（shipped_at 在未来）不能出现负数"
    );
}

/// ⑨ LNK-F12：查询缺省值不能把阈值落成 0（derive(Default) 的静默坑）。
#[test]
fn query_defaults_keep_overdue_threshold() {
    let query = TransferQuery::default();
    assert_eq!(query.overdue_days, DEFAULT_OVERDUE_DAYS);
    assert_eq!(query.in_transit_overdue, None);
    assert_eq!(query.page_size, crate::business::erp::model::DEFAULT_PAGE_SIZE);
}
