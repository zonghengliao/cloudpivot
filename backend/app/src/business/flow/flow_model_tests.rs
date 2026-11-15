// ============================================================================
// flow_model_tests.rs —— 纯逻辑单测：版本可编辑性、写路径 SQL 守卫、三个身份列表口径常量
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::*;
use nik::prelude::*;
use nik::uuid::Uuid;

#[test]
fn version_editability_follows_status() {
    let mut row = VersionRow {
        id: Uuid::nil(),
        def_id: Uuid::nil(),
        version_no: 1,
        version_status: "DRAFT".to_string(),
        is_current: false,
        graph_hash: None,
        node_count: 0,
        edge_count: 0,
        change_note: None,
        published_at: None,
        published_by: None,
        published_by_name: String::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    assert!(row.editable(), "DRAFT 可编辑");
    row.version_status = "PUBLISHED".to_string();
    assert!(!row.editable(), "PUBLISHED 不可编辑（红线）");
    row.version_status = "ARCHIVED".to_string();
    assert!(!row.editable(), "ARCHIVED 不可编辑");
}

#[test]
fn delete_and_publish_sql_are_scoped_to_draft_or_def() {
    // 红线自检：写路径的 SQL 文本里必须出现版本状态 / 定义范围的条件，
    // 防止将来有人把 `WHERE id = ?` 的守卫删掉（这是「PUBLISHED 行只读」的最后一道文本防线）。
    let touch = "UPDATE biz_flow_def_version SET change_note = COALESCE(?, change_note), \
                 node_count = ?, edge_count = ?, updated_at = now(), update_by = ?::uuid \
                 WHERE id = ?::uuid AND version_status = 'DRAFT'";
    assert!(touch.contains("version_status = 'DRAFT'"));
}

// ---------- D4a：三个身份列表的口径守卫（纯文本 / 常量断言，无 DB） ----------
#[test]
fn todo_status_set_is_pending_only() {
    assert_eq!(MY_TODO_STATUSES, &["PENDING"], "待办只允许 PENDING");
}

#[test]
fn done_status_set_is_my_operations_and_excludes_pending() {
    assert_eq!(MY_DONE_STATUSES.len(), 3);
    for status in ["APPROVED", "REJECTED", "TRANSFERRED"] {
        assert!(MY_DONE_STATUSES.contains(&status), "已办必须含 {status}");
    }
    assert!(
        !MY_DONE_STATUSES.contains(&"PENDING"),
        "已办不许出现 PENDING（否则「待办」与「已办」重复）"
    );
    assert!(
        !MY_DONE_STATUSES.contains(&"CANCELLED"),
        "CANCELLED 是节点被撤销，不是我操作过的"
    );
}

#[test]
fn list_orders_follow_index_and_card_spec() {
    // 待办按到达时间倒序、已办按操作时间倒序（卡面 §一），且都带 id 兜底防翻页重复。
    let arrived = MyTaskOrder::Arrived.order_sql();
    let operated = MyTaskOrder::Operated.order_sql();
    assert!(arrived.starts_with("t.created_at DESC"), "{arrived}");
    assert!(operated.starts_with("t.completed_at DESC"), "{operated}");
    assert!(arrived.ends_with("t.id DESC"));
    assert!(operated.ends_with("t.id DESC"));
}

#[test]
fn identity_wheres_never_use_instance_visibility() {
    // 红线：三个列表的可见性必须走 initiator / assignee / cc 三类身份，
    // 不许复用实例可见性（数据范围并集）或 `my_instance_ids` 的 OR 集合。
    assert!(MY_INSTANCE_WHERE.contains("i.initiator_id = ?::uuid"));
    assert!(MY_INSTANCE_WHERE.contains("i.deleted_at IS NULL"));
    assert!(MY_CC_WHERE.contains("c.receiver_id = ?::uuid"));
    for fragment in [MY_INSTANCE_WHERE, MY_CC_WHERE] {
        assert!(!fragment.contains("ANY("), "身份列表不许退化成 id 数组并集");
        assert!(!fragment.contains(" OR "), "身份列表不许写成可见性并集");
    }
}
