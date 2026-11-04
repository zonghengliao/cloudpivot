// ============================================================================
// task_model_tests.rs —— 纯逻辑回归：转派 SQL 口径等
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{Duration, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::shared::scope;

use super::*;

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("合法日期")
}

/// T3：负责人变更只许一条 SQL，且必须带**乐观并发**条件（否则并发转派会串味）。
#[test]
fn assign_owner_sql_is_guarded() {
    assert!(
        ASSIGN_OWNER_SQL.starts_with("UPDATE biz_task"),
        "负责人变更只能是 biz_task 的单条 UPDATE"
    );
    assert_eq!(
        ASSIGN_OWNER_SQL.matches("?::uuid").count(),
        6,
        "owner_id / dept_id / assigner_id / update_by / id / expected_owner 六个 uuid 参数"
    );
    assert!(
        ASSIGN_OWNER_SQL.contains("AND owner_id = ?::uuid"),
        "必须按读到的旧负责人做乐观并发（并发转派 → 0 行 → 409）"
    );
    assert!(
        ASSIGN_OWNER_SQL.contains("deleted_at IS NULL"),
        "已软删的任务不许被分派"
    );
    assert!(
        ASSIGN_OWNER_SQL.contains("dept_id = ?::uuid"),
        "部门快照要跟着换"
    );
    assert!(
        ASSIGN_OWNER_SQL.contains("source = ?"),
        "来源要能改成 TRANSFERRED"
    );
}

/// T3：加协作人必须是**一条**集合式 INSERT（不许 N+1），且幂等 + 回吐新增行。
#[test]
fn members_insert_sql_is_set_based() {
    assert_eq!(
        MEMBERS_INSERT_SQL.matches("INSERT INTO").count(),
        1,
        "50 个人也必须只有一条 INSERT"
    );
    assert!(MEMBERS_INSERT_SQL.contains("unnest(?::text::uuid[])"));
    assert!(MEMBERS_INSERT_SQL.contains("ON CONFLICT (task_id, user_id) DO NOTHING"));
    assert!(
        MEMBERS_INSERT_SQL.contains("RETURNING user_id"),
        "要靠 RETURNING 区分「新增」与「本来就存在」"
    );
}

/// T3：协作人 id 数组参数沿用 `{uuid,uuid}` 文本字面量（驱动不支持 uuid[] 直绑）。
#[test]
fn ids_array_param_is_pg_literal() {
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    match ids_array_param(&[a, b]) {
        Value::Text(text) => assert_eq!(text, format!("{{{a},{b}}}")),
        other => panic!("必须是文本字面量，实际 {other:?}"),
    }
    match ids_array_param(&[]) {
        Value::Text(text) => assert_eq!(text, "{}"),
        other => panic!("空集合必须是 {{}}，实际 {other:?}"),
    }
}

/// F2：A 口径的四个判别用例 + 7 天边界两侧各 1 秒。
#[test]
fn is_new_task_matches_a_criteria() {
    let me = Uuid::parse_str("018f0000-0000-7000-8000-00000000000a").expect("合法 uuid");
    let other = Uuid::parse_str("018f0000-0000-7000-8000-00000000000b").expect("合法 uuid");
    let now = Utc::now();
    let day = Duration::days(1);
    assert!(
        is_new_task_at("TODO", me, now - day, me, now),
        "TODO + 我 + 1 天前 → 新"
    );
    assert!(
        !is_new_task_at("TODO", me, now - Duration::days(8), me, now),
        "TODO + 我 + 8 天前 → 不是新（出窗口）"
    );
    assert!(
        !is_new_task_at("DOING", me, now - day, me, now),
        "DOING + 我 → 不是新（口径只认 TODO）"
    );
    assert!(
        !is_new_task_at("TODO", other, now - day, me, now),
        "TODO + 别人 → 对我不新"
    );
    let window = Duration::days(i64::from(NEW_TASK_WINDOW_DAYS));
    assert!(
        is_new_task_at("TODO", me, now - window + Duration::seconds(1), me, now),
        "窗口内 1 秒 → 仍是新"
    );
    assert!(
        !is_new_task_at("TODO", me, now - window - Duration::seconds(1), me, now),
        "窗口外 1 秒 → 不再新"
    );
}

/// F2：列表/看板/甘特用的窗口必须与菜单徽标 `task_todo` 的窗口同值，
/// 否则 A 口径退化成「徽标 2、列表 0 NEW」。
#[test]
fn new_window_matches_badge() {
    assert_eq!(
        NEW_TASK_WINDOW_DAYS,
        crate::system::badge::model::TASK_TODO_WINDOW_DAYS,
        "is_new 与 /system/badge/counts 的 task_todo 必须同窗口"
    );
}

#[test]
fn visible_tail_uses_member_id_array_only() {
    // 拼法的前提：片段标记必须在 OR 组的左半边，且只出现一次
    assert_eq!(
        VISIBLE_OR_TAIL.matches(scope::FILTER_MARKER).count(),
        1,
        "VISIBLE_OR_TAIL 必须且只能有一个 {{FILTER}} 标记"
    );
    assert!(
        VISIBLE_OR_TAIL.contains("(1 = 1 {FILTER})"),
        "scope 片段必须包在 (1 = 1 …) 里，否则 AND 片段会退化成裸条件"
    );
    // D6：参与关系只允许用一个 id 数组表达
    assert!(
        VISIBLE_OR_TAIL.contains("t.id = ANY(?::text::uuid[])"),
        "参与关系必须用 id 数组（D6），不能改回 EXISTS / 多 OR"
    );
    assert!(
        !VISIBLE_OR_TAIL.contains("EXISTS"),
        "EXISTS 子查询会让优化器放弃 BitmapOr、退化成 Seq Scan（实测 539 ms）"
    );
    for banned in ["t.owner_id", "t.creator_id", "t.assigner_id"] {
        assert!(
            !VISIBLE_OR_TAIL.contains(banned),
            "{banned} 的 OR 分支必须并入 my_member_task_ids 的 id 数组，不得留在谓词里"
        );
    }
    // 占位符数量（不含 {FILTER} 内的）必须等于 visible_params 的长度
    assert_eq!(VISIBLE_OR_TAIL.matches('?').count(), 1);
}

#[test]
fn visible_params_are_one_uuid_array() {
    let ids = [
        Uuid::parse_str("018f0000-0000-7000-8000-000000000001").expect("合法 uuid"),
        Uuid::parse_str("018f0000-0000-7000-8000-000000000002").expect("合法 uuid"),
    ];
    let scope_ids = MyScopeIds {
        ids: ids.to_vec(),
        truncated: false,
    };
    let params = visible_params(&scope_ids);
    assert_eq!(
        params.len(),
        VISIBLE_OR_TAIL.matches('?').count(),
        "参数个数必须与 SQL 里 `?` 的文本顺序一一对应"
    );
    assert_eq!(
        params[0],
        Value::Text(
            "{018f0000-0000-7000-8000-000000000001,018f0000-0000-7000-8000-000000000002}"
                .to_string()
        ),
        "uuid[] 必须编码成 PG 文本数组字面量 {{uuid,uuid}}"
    );
}

#[test]
fn truncated_scope_degrades_to_empty_array() {
    // D7 降级：超过 2000 个 id → 传空数组（ANY('{}') 恒假 = 只按范围），
    // 且参数个数不变（降级不会让绑定错位）。
    let oversized = MyScopeIds {
        ids: vec![Uuid::nil()],
        truncated: true,
    };
    assert_eq!(
        visible_params(&oversized),
        vec![Value::Text("{}".to_string())]
    );
    // 空集合本身的编码就是 `{}`（没有任何参与关系时同样安全）
    assert_eq!(
        visible_params(&MyScopeIds::default()),
        vec![Value::Text("{}".to_string())]
    );
    assert_eq!(MEMBER_SCOPE_LIMIT, 2000);
}

#[test]
fn member_limit_truncation_rule() {
    // 纯逻辑镜像 my_member_task_ids 的截断判定（2000 允许、2001 降级）
    let check = |n: usize| n as i64 > MEMBER_SCOPE_LIMIT;
    assert!(!check(0));
    assert!(!check(2000));
    assert!(check(2001));
}

#[test]
fn fragment_shape_guard_accepts_known_shapes_and_rejects_or() {
    assert!(check_fragment_shape(" AND t.dept_id = ?").is_ok());
    assert!(check_fragment_shape("AND (t.dept_id = ANY(?::text::uuid[]))").is_ok());
    assert!(check_fragment_shape(" 1 = 1 ").is_ok());
    assert!(check_fragment_shape("1 = 0").is_ok());
    // 未来的 OR 片段必须被拦下（否则 (1 = 1 OR …) 恒真 → 全表可见）
    assert!(check_fragment_shape(" OR t.dept_id = ?").is_err());
    assert!(check_fragment_shape("t.dept_id = ?").is_err());
}

#[test]
fn default_due_date_is_this_friday() {
    // 2026-10-29 是周四 → 本周五 = 10-30
    assert_eq!(default_due_date(date(2026, 10, 29)), date(2026, 10, 30));
    // 周五当天 → 今天
    assert_eq!(default_due_date(date(2026, 10, 30)), date(2026, 10, 30));
    // 周六 / 周日 → 下一个周五（周末建任务不该一创建就逾期）
    assert_eq!(default_due_date(date(2026, 10, 31)), date(2026, 11, 6));
    assert_eq!(default_due_date(date(2026, 11, 1)), date(2026, 11, 6));
    // 周一 → 本周五
    assert_eq!(default_due_date(date(2026, 11, 2)), date(2026, 11, 6));
}

#[test]
fn buckets_cover_all_cases() {
    let today = date(2026, 10, 29); // 周四；本周日 = 11-01
    assert_eq!(bucket_of(None, today), MyBucket::NoDue);
    assert_eq!(
        bucket_of(Some(date(2026, 10, 28)), today),
        MyBucket::Overdue
    );
    assert_eq!(bucket_of(Some(today), today), MyBucket::Today);
    assert_eq!(
        bucket_of(Some(date(2026, 10, 30)), today),
        MyBucket::ThisWeek
    );
    assert_eq!(
        bucket_of(Some(date(2026, 11, 1)), today),
        MyBucket::ThisWeek,
        "周日仍算本周"
    );
    assert_eq!(
        bucket_of(Some(date(2026, 11, 2)), today),
        MyBucket::Later,
        "下周一算以后"
    );
}

#[test]
fn task_no_is_zero_padded_and_month_scoped() {
    assert_eq!(format_task_no("202610", 1), "TSK-202610-000001");
    assert_eq!(format_task_no("202610", 123456), "TSK-202610-123456");
    assert_eq!(month_key(date(2026, 10, 29)), "202610");
    // substring(task_no from 12) 的口径必须与编号格式一致：前缀 TSK-YYYYMM- 共 11 字符
    let sample = format_task_no("202610", 42);
    assert_eq!(&sample[10..], "-000042");
    assert_eq!(&sample[11..], "000042");
    assert_eq!(sample.chars().take(11).collect::<String>(), "TSK-202610-");
}

#[test]
fn cst_date_shifts_by_eight_hours() {
    let utc = chrono::DateTime::parse_from_rfc3339("2026-10-29T16:30:00Z")
        .expect("合法 RFC3339")
        .with_timezone(&Utc);
    // UTC 16:30 = CST 次日 00:30
    assert_eq!(cst_date_of(utc), date(2026, 10, 30));
    let utc_early = chrono::DateTime::parse_from_rfc3339("2026-10-29T01:00:00Z")
        .expect("合法 RFC3339")
        .with_timezone(&Utc);
    assert_eq!(cst_date_of(utc_early), date(2026, 10, 29));
}

#[test]
fn like_wildcards_are_escaped() {
    assert_eq!(escape_like("100%"), "100\\%");
    assert_eq!(escape_like("a_b"), "a\\_b");
    assert_eq!(escape_like("plain"), "plain");
}

#[test]
fn patch_empty_detection() {
    assert!(TaskPatch::default().is_empty());
    let patch = TaskPatch {
        priority: Some("P0".to_string()),
        ..TaskPatch::default()
    };
    assert!(!patch.is_empty());
}

/// E6a：任务卡列清单必须同时下发「子任务总数」与「已完成数」。
///
/// 少任一列 → [`TaskCardRow::from_row`] 运行期 `get_typed` 失败（列表 / 详情直接 500），
/// 而现有一批用例都不连库，只有这条能在 `cargo test` 里提前拦住。
#[test]
fn card_columns_expose_child_stats() {
    assert!(CARD_COLUMNS.contains("AS child_count"));
    assert!(CARD_COLUMNS.contains("AS child_done_count"));
    // E6b：树形表格靠任务卡上的 parent_id 判定层级，缺列 → 列表 / 详情 500
    assert!(CARD_COLUMNS.contains("t.id, t.parent_id, t.task_no"));
    // 分子口径：未软删 + 终态 DONE（软删子任务不能算进完成数）
    assert!(CARD_COLUMNS.contains("c.deleted_at IS NULL"));
    assert!(CARD_COLUMNS.contains("c.status = 'DONE'"));
    // 子查询必须按 parent_id 等值锚定（走 idx_biz_task_parent），不许退化成全表扫描
    assert!(CARD_COLUMNS.contains("c.parent_id = t.id"));
    // 反向断言：CANCELLED 不是「完成」，别把取消也算进去
    assert!(!CARD_COLUMNS.contains("CANCELLED"));
}

/// P2-9 / DC-19：任务卡的列清单必须带上「关联单据」两列。
///
/// 与上一条同性质：`TaskCardRow::from_row` 对 `doc_type` / `doc_id` 是**硬取**
/// （`get_typed` 不吞列缺失），少一列整条列表 / 详情接口就是 500。
/// 纯逻辑用例，不连库 —— `CARD_COLUMNS` 被 5 条查询共用，改它时这条会先红。
#[test]
fn card_columns_expose_doc_link() {
    assert!(
        CARD_COLUMNS.contains("t.doc_type, t.doc_id"),
        "任务卡必须下发 doc_type / doc_id（P2-9）：{CARD_COLUMNS}"
    );
}
