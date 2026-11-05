// ============================================================================
// task_handler_tests.rs —— 接口层纯逻辑用例（标题归一 / 角色矩阵 / 依赖判定 / 分页归一）
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::task::dto::{self, TaskPageQuery};
use crate::shared::error_code;

use super::*;

use chrono::NaiveDate;

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("合法日期")
}

#[test]
fn title_is_trimmed_and_rejected_when_blank() {
    assert_eq!(
        normalize_title("  订单   审批 ").expect("合法标题"),
        "订单 审批"
    );
    let error = normalize_title("   ").expect_err("空标题必须报错");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::TASK_TITLE_INVALID);

    let long = "任".repeat(dto::MAX_TITLE_CHARS + 1);
    assert!(normalize_title(&long).is_err());
}

#[test]
fn hours_are_bounded_and_rounded() {
    assert_eq!(validate_hours(None).expect("空值合法"), None);
    assert_eq!(validate_hours(Some(1.26)).expect("合法"), Some(1.3));
    assert!(validate_hours(Some(-1.0)).is_err());
    assert!(validate_hours(Some(f64::NAN)).is_err());
    assert!(validate_hours(Some(100_000.0)).is_err());
}

#[test]
fn filter_rejects_unknown_status_and_priority() {
    let bad_status = TaskPageQuery {
        status: Some("ARCHIVED".to_string()),
        ..TaskPageQuery::default()
    };
    let error = build_filter(&bad_status).expect_err("非法状态必须 400");
    assert_eq!(error.status, 400);

    let bad_priority = TaskPageQuery {
        priority: Some("P9".to_string()),
        ..TaskPageQuery::default()
    };
    assert!(build_filter(&bad_priority).is_err());

    let bad_range = TaskPageQuery {
        due_from: Some(date(2026, 11, 1)),
        due_to: Some(date(2026, 10, 1)),
        ..TaskPageQuery::default()
    };
    assert!(build_filter(&bad_range).is_err());

    let ok = TaskPageQuery {
        status: Some("doing".to_string()),
        priority: Some("p1".to_string()),
        ..TaskPageQuery::default()
    };
    let filter = build_filter(&ok).expect("合法筛选");
    assert_eq!(filter.status.as_deref(), Some("DOING"));
    assert_eq!(filter.priority.as_deref(), Some("P1"));
}

#[test]
fn filter_normalizes_statuses_list() {
    let query = TaskPageQuery {
        statuses: Some(" TODO , doing ,, DONE ".to_string()),
        ..TaskPageQuery::default()
    };
    let filter = build_filter(&query).expect("合法多选");
    assert_eq!(
        filter.statuses,
        Some(vec![
            "TODO".to_string(),
            "DOING".to_string(),
            "DONE".to_string()
        ]),
        "逗号分隔 + 去空白 + 大写归一 + 丢空段"
    );
}

#[test]
fn filter_rejects_unknown_status_in_statuses() {
    let query = TaskPageQuery {
        statuses: Some("TODO,FOO".to_string()),
        ..TaskPageQuery::default()
    };
    let error = build_filter(&query).expect_err("非法状态必须 400");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::PARAM_INVALID);
    assert!(error.msg.contains("FOO"), "报错要指出非法值：{}", error.msg);
}

#[test]
fn filter_blank_statuses_means_no_filter() {
    // 空串 / 只有逗号 → None（等价不传），不是「筛空集合」
    for raw in ["", "   ", ",", " , , "] {
        let query = TaskPageQuery {
            statuses: Some(raw.to_string()),
            ..TaskPageQuery::default()
        };
        let filter = build_filter(&query).expect("空串合法");
        assert_eq!(filter.statuses, None, "{raw:?} 应归一成 None");
    }
}

#[test]
fn filter_keeps_legacy_status_alongside_statuses() {
    // 两个都传：两个字段各自保留（优先关系由 build_filters 兑现）
    let query = TaskPageQuery {
        status: Some("todo".to_string()),
        statuses: Some("DONE,DOING".to_string()),
        ..TaskPageQuery::default()
    };
    let filter = build_filter(&query).expect("合法");
    assert_eq!(filter.status.as_deref(), Some("TODO"), "旧字段仍归一化");
    assert_eq!(
        filter.statuses,
        Some(vec!["DONE".to_string(), "DOING".to_string()]),
        "多值字段保留顺序"
    );
}

#[test]
fn filter_rejects_bad_owner_uuid() {
    let query = TaskPageQuery {
        owner_id: Some("not-a-uuid".to_string()),
        ..TaskPageQuery::default()
    };
    assert!(build_filter(&query).is_err());
}

#[test]
fn current_user_rejects_bad_subject() {
    let claims = TokenClaims {
        jti: None,
        sub: "not-a-uuid".to_string(),
        username: None,
        roles: Vec::new(),
        permissions: Vec::new(),
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    };
    let error = current_user(&claims).expect_err("非法 sub 必须报错");
    assert_eq!(error.status, 401);
}

// ==================== T3：分派 / 转派 / 协作人 ====================

/// 分派 / 协作人的权限矩阵：**行级关系 ∪ 权限码**，其他人一律拒绝。
///
/// 逐格断言（4 身份 × 2 权限码 = 8 格 × 2 个动作），防止「改了一边忘了另一边」：
/// 这两条矩阵是「谁能把活派出去 / 谁能拉人」的唯一口径，错一格就是越权。
#[test]
fn assign_and_member_permission_matrix() {
    let roles = [
        ActorRole::Creator,
        ActorRole::Owner,
        ActorRole::Assigner,
        ActorRole::Outsider,
    ];
    for role in roles {
        let related = role != ActorRole::Outsider;
        assert_eq!(
            may_assign(role, false),
            related,
            "{role:?} 没有权限码时：行级关系才放行"
        );
        assert_eq!(
            may_manage_members(role, false),
            related,
            "{role:?} 协作人矩阵应与分派一致"
        );
        assert!(may_assign(role, true), "{role:?} 有权限码时一律放行");
        assert!(
            may_manage_members(role, true),
            "{role:?} 有 member 码时一律放行"
        );
    }
    // 关键的一格：**无关用户 + 无权限码 = 403**（验收第 3 条）
    assert!(!may_assign(ActorRole::Outsider, false));
    // 关键的另一格：AI 用户被授予权限码后必须放行（修正 11，验收第 6 条）
    assert!(may_assign(ActorRole::Outsider, true));
}

/// 身份判定优先级：创建人 > 负责人 > 分派人。
///
/// 「创建人自己就是负责人」是最常见的情形（自建任务），必须稳定判成创建人
/// —— 它与 `next_assigner` 的「操作人 = 创建人则沿用原 assigner」联动。
#[test]
fn actor_role_prefers_creator_then_owner_then_assigner() {
    let creator = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let assigner = Uuid::now_v7();
    let other = Uuid::now_v7();

    assert_eq!(
        actor_role(creator, creator, owner, Some(assigner)),
        ActorRole::Creator
    );
    assert_eq!(
        actor_role(owner, creator, owner, Some(assigner)),
        ActorRole::Owner
    );
    assert_eq!(
        actor_role(assigner, creator, owner, Some(assigner)),
        ActorRole::Assigner
    );
    assert_eq!(actor_role(other, creator, owner, None), ActorRole::Outsider);
    // 分派人缺席（自建任务）时，除了创建人 / 负责人谁都是 outsider
    assert_eq!(
        actor_role(assigner, creator, owner, None),
        ActorRole::Outsider
    );
}

/// `assigner_id` 落库口径：操作人非创建人 → 记操作人；创建人自己派 → 沿用原值。
#[test]
fn assigner_id_follows_card_rule() {
    let creator = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let third = Uuid::now_v7();

    // 创建人分派：assigner 保持原样（自建任务本来就没有 assigner）
    assert_eq!(next_assigner(creator, creator, None), None);
    // 负责人转派：assigner = 操作人（接手的人要知道是谁派的）
    assert_eq!(next_assigner(owner, creator, None), Some(owner));
    // 已有分派人再被转派：assigner 改成新操作人（最近一次经手人）
    assert_eq!(next_assigner(third, creator, Some(creator)), Some(third));
}

/// 创建人 / 负责人是受保护协作人（不许移除）。
#[test]
fn creator_and_owner_are_protected_members() {
    let creator = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let member = Uuid::now_v7();

    assert!(protected_member(creator, creator, owner));
    assert!(protected_member(owner, creator, owner));
    assert!(!protected_member(member, creator, owner));
}

/// 协作人上限边界：50 允许、51 拒绝，错误是 **400 + 63014**（不是 500、不是 409）。
#[test]
fn member_quota_boundary() {
    assert!(ensure_member_quota(0, 0).is_ok());
    assert!(ensure_member_quota(0, 50).is_ok(), "正好 50 人必须允许");
    assert!(ensure_member_quota(47, 3).is_ok(), "47 + 3 = 50 必须允许");
    // 幂等重复提交（新增 0 人）不该占额度
    assert!(
        ensure_member_quota(50, 0).is_ok(),
        "满 50 人时重复加同一批人不报上限"
    );

    let error = ensure_member_quota(48, 3).expect_err("51 人必须被拒绝");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::TASK_MEMBER_LIMIT);
    assert!(error.msg.contains("50"), "文案要带上限数字：{}", error.msg);

    let error = ensure_member_quota(0, 51).expect_err("一次加 51 人必须被拒绝");
    assert_eq!(error.code, error_code::TASK_MEMBER_LIMIT);
}

/// 人类入口的正文上限：5000 允许、5001 → **400 + 63004**（不许静默截断）。
#[test]
fn user_comment_content_limit_is_strict() {
    let at_limit = "字".repeat(MAX_USER_COMMENT_CHARS);
    let accepted =
        normalize_comment_content(&at_limit, &CommentLimit::Strict(MAX_USER_COMMENT_CHARS))
            .expect("正好 5000 字必须接受");
    assert_eq!(accepted.chars().count(), MAX_USER_COMMENT_CHARS);
    // 首尾空白不计入长度
    let padded = format!("  {}  ", "字".repeat(MAX_USER_COMMENT_CHARS));
    assert!(
        normalize_comment_content(&padded, &CommentLimit::Strict(MAX_USER_COMMENT_CHARS)).is_ok()
    );

    let too_long = "字".repeat(MAX_USER_COMMENT_CHARS + 1);
    let error = normalize_comment_content(&too_long, &CommentLimit::Strict(MAX_USER_COMMENT_CHARS))
        .expect_err("5001 字必须 400");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::TASK_FIELD_INVALID);
    assert!(error.msg.contains("5000"), "文案要带上限：{}", error.msg);

    // 空 / 全空白 → 400
    let error = normalize_comment_content("  \n\t ", &CommentLimit::Strict(5_000))
        .expect_err("空正文必须 400");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::TASK_FIELD_INVALID);
}

/// AI 入口保持「超长截断」（回传体积不可控，截断也比整条丢掉强）。
#[test]
fn ai_comment_content_is_truncated() {
    let long = "字".repeat(MAX_COMMENT_CHARS + 10);
    let kept = normalize_comment_content(&long, &CommentLimit::Truncate(MAX_COMMENT_CHARS))
        .expect("AI 正文超长要截断而不是拒绝");
    assert_eq!(kept.chars().count(), MAX_COMMENT_CHARS);
}

/// `comment_type` 只允许 PLAIN：伪造系统类型一律 400（红线）。
#[test]
fn comment_type_forgery_is_rejected() {
    assert!(comment_type_verdict(None).is_ok());
    assert!(comment_type_verdict(Some("   ")).is_ok());
    assert!(comment_type_verdict(Some("PLAIN")).is_ok());
    assert!(comment_type_verdict(Some(" plain ")).is_ok());
    for forged in ["AI_REPORT", "STATUS_CHANGE", "AI_PROGRESS", "ASSIGN"] {
        let error = comment_type_verdict(Some(forged)).expect_err("伪造类型必须被拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::TASK_FIELD_INVALID);
        assert!(
            error.msg.contains(forged),
            "文案要点名被拒的类型：{}",
            error.msg
        );
    }
}

/// D9：副本标题 = `原标题（副本）`；原标题顶到 200 字上限时**砍原标题**，后缀完整保留。
#[test]
fn copy_title_appends_suffix_and_keeps_limit() {
    assert_eq!(copy_title("月度对账"), "月度对账（副本）");
    // 尾部空白先折掉，不产出「标题 （副本）」这种带空格的怪东西
    assert_eq!(copy_title("  月度对账  "), "月度对账（副本）");

    let full = "任".repeat(dto::MAX_TITLE_CHARS);
    let copied = copy_title(&full);
    assert_eq!(
        copied.chars().count(),
        dto::MAX_TITLE_CHARS,
        "副本标题不能超 200 字（会被 normalize_title 拒）"
    );
    assert!(
        copied.ends_with(COPY_TITLE_SUFFIX),
        "后缀必须完整：{copied}"
    );
    assert!(normalize_title(&copied).is_ok(), "拼出来的标题必须仍然合法");
}
