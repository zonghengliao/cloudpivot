//! 三个校验点共用的互斥守卫（批次 B · B7）。
//!
//! # 为什么守卫集中在本文件
//!
//! 三个校验点的**判定语义必须完全相同**（同一个 `sys_role_mutex` 表、同一份持有口径、
//! 同一组错误码）。任何一个校验点自己写一份 SQL，都是「三个入口三套语义」的开始，
//! 之后必然出现「接口 A 拦得住、接口 B 拦不住」。所以：
//!
//! - 校验 SQL 只有一份：[`crate::business::mutex::model::violations_for_user`]；
//! - 持有口径只有一份：[`crate::business::mutex::model::HELD_ROLE_SQL`]；
//! - 文案只有一份：本文件的 `*_message` 纯函数（可单测，不连库）。
//!
//! # 三个校验点
//!
//! | # | 入口 | 函数 | 失败码 |
//! |---|------|------|--------|
//! | 1 | `system/user` 授予角色 | [`ensure_no_mutex`] | `409 + 61009` |
//! | 2 | `system/role` 启用角色 | [`ensure_role_enable_no_mutex`] | `409 + 61009` |
//! | 3 | 本模块新建 / 启用规则 | [`ensure_pair_has_no_holders`] | `409 + 61010` |
//!
//! # 超管不短路
//!
//! [`ensure_no_mutex`] 里**没有** `is_super_admin → Ok(())`。互斥是数据完整性约束，
//! 不是操作者资格约束；超管双持同样让 SoD 失效。这是与批次 A `delegation cap` 的刻意差异
//! （cap 对超管短路，互斥不对）。别在后续改动里"顺手"补一行超管短路——那会让整批白做。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::mutex::checker;
use crate::business::mutex::model::{self, PairHit};
use crate::shared::error_code;
use crate::utils;

/// 规范化互斥对：按 UUID 字节序返回 `(小, 大)`。
///
/// # 为什么是字节序
///
/// 与 PostgreSQL 的 `LEAST/GREATEST`（uuid 的 btree 比较）以及 `uk_sys_role_mutex_pair`
/// 的表达式索引口径**逐字一致**。应用层若用自己的顺序（比如按字符串比较），会出现
/// 「应用认为不同、库认为相同」的错位。
pub fn normalize_pair(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
    if a <= b { (a, b) } else { (b, a) }
}

/// 反自反：角色不能与自己互斥。
///
/// # 异常情况
///
/// - `400 + 61012 ROLE_MUTEX_SELF_PAIR`：无法建立角色与自身的互斥关系。
pub fn ensure_not_self_pair(a: Uuid, b: Uuid) -> std::result::Result<(), ApiError> {
    if a == b {
        return Err(ApiError::new(
            400,
            error_code::ROLE_MUTEX_SELF_PAIR,
            "不能建立角色与自身的互斥关系。",
        ));
    }
    Ok(())
}

/// 校验点 1 的文案（单组命中）。
///
/// # 为什么需要 `held`
///
/// `assign_roles` 是**全量替换**：前端提交的是「最终角色集合」，所以「新增的付款审批」
/// 与「本来就有的采购员」都会出现在 `requested` 里。只拿 `requested` 判断方向会把
/// 「已持有采购员、本次加付款审批」说成「本次同时授予两个互斥角色」——技术上没错，
/// 但答非所问（管理员会以为自己一次选了俩）。所以再比对 `held`：
/// **在 `requested` 里但不在 `held` 里 = 本次新增**。
fn single_message(hit: &PairHit, requested: &[Uuid], held: &[Uuid]) -> String {
    let a_new = requested.contains(&hit.a.id) && !held.contains(&hit.a.id);
    let b_new = requested.contains(&hit.b.id) && !held.contains(&hit.b.id);
    match (a_new, b_new) {
        (true, false) => format!(
            "角色「{}」与用户已持有的「{}」互斥：{}。请先解除其中一个角色。",
            hit.a.name, hit.b.name, hit.reason
        ),
        (false, true) => format!(
            "角色「{}」与用户已持有的「{}」互斥：{}。请先解除其中一个角色。",
            hit.b.name, hit.a.name, hit.reason
        ),
        // 两侧都是本次新增（架构 D3 的「同次授予两个互斥角色」）
        (true, true) => format!(
            "本次授权同时包含互斥角色「{}」与「{}」：{}。请先移除其中一个角色。",
            hit.a.name, hit.b.name, hit.reason
        ),
        // 两侧本来就都在持有里（历史违规），本次只是原样重提：如实说明是「已双持」
        _ => format!(
            "用户已同时持有互斥角色「{}」与「{}」：{}。请先解除其中一个角色。",
            hit.a.name, hit.b.name, hit.reason
        ),
    }
}

/// 校验点 1 的文案（多组命中：列出组数与角色对）。
///
/// # 为什么多组要换一种说法
///
/// 单组时用户看清「哪两个角色打架」就够了；多组时逐条复述会变成一大段话，
/// 反而看不出「本次授权到底命中了几组」。所以多组只给组数 + 角色对清单。
pub fn conflict_message(hits: &[PairHit], requested: &[Uuid], held: &[Uuid]) -> String {
    match hits.len() {
        0 => String::new(),
        1 => single_message(&hits[0], requested, held),
        n => {
            let pairs = hits
                .iter()
                .map(|hit| format!("{}×{}", hit.a.name, hit.b.name))
                .collect::<Vec<_>>()
                .join("、");
            format!("本次授权命中 {n} 组互斥关系（{pairs}），请先解除冲突角色。")
        }
    }
}

/// 校验点 3 的文案：建规则时已有用户同时持有。
pub fn holders_message(
    total: i64,
    role_a_name: &str,
    role_b_name: &str,
    sample_usernames: &[String],
) -> String {
    let sample = if sample_usernames.is_empty() {
        String::new()
    } else {
        format!("（如 {}）", sample_usernames.join("、"))
    };
    format!(
        "无法建立互斥关系：{total} 个用户同时持有「{role_a_name}」与「{role_b_name}」，\
         请先解除他们的其中一个角色{sample}。"
    )
}

/// 校验点 2 的文案：启用角色会让互斥关系成立。
pub fn role_enable_message(
    role_name: &str,
    total: i64,
    pairs: &str,
    sample_usernames: &[String],
) -> String {
    let sample = if sample_usernames.is_empty() {
        String::new()
    } else {
        format!("（如 {}）", sample_usernames.join("、"))
    };
    format!(
        "启用角色「{role_name}」后将使 {total} 个用户同时持有互斥角色（{pairs}），\
         请先解除他们的其中一个角色{sample}。"
    )
}

/// 校验点 1：授予角色时校验 `已持有 ∪ 本次授予` 是否命中互斥。
///
/// # 参数
///
/// - `user_id`：目标用户（新建用户时可传即将使用的 ID，此时 `sys_user_role` 里没有它的
///   绑定，并集退化为「本次授予」——这正是「建号即双持」需要被拦住的场景）；
/// - `requested`：本次要授予的**全部**角色 ID（`assign_roles` 是全量替换语义，传最终集合）。
///
/// # 异常情况
///
/// - `409 + 61009 ROLE_MUTEX_CONFLICT`：命中互斥，消息里点名角色与理由；
/// - `500`：数据库异常。
///
/// # 对超管同样生效（刻意）
///
/// 见模块文档「超管不短路」。
pub async fn ensure_no_mutex(
    user_id: &Uuid,
    requested: &[Uuid],
) -> std::result::Result<(), ApiError> {
    if requested.is_empty() {
        // 清空全部角色：不可能新增互斥。历史上已违规的用户由体检接口暴露，不在这里报。
        return Ok(());
    }
    let hits = model::violations_for_user(user_id, requested)
        .await
        .map_err(|error| utils::db_error(error, "互斥校验"))?;
    if hits.is_empty() {
        return Ok(());
    }
    // 已持有的角色集合：只用来判断文案方向（哪一侧是「本次新增」），不参与判定
    let held = model::held_role_ids(user_id)
        .await
        .map_err(|error| utils::db_error(error, "查询用户已持有的角色"))?;
    tracing::warn!(
        user_id = %user_id,
        groups = hits.len(),
        requested = requested.len(),
        "角色互斥校验未通过：本次授权与已持有角色冲突"
    );
    Err(ApiError::new(
        409,
        error_code::ROLE_MUTEX_CONFLICT,
        conflict_message(&hits, requested, &held),
    ))
}

/// 校验点 3：建立（或恢复启用）互斥规则前，确认没有用户已经双持。
///
/// # 参数
///
/// - `role_a` / `role_b`：角色 ID（顺序无意义）；
/// - `role_a_name` / `role_b_name`：角色名称（只用于文案）。
///
/// # 异常情况
///
/// - `409 + 61010 ROLE_MUTEX_HAS_HOLDERS`：已有用户双持，消息里给用户数 + 示例用户名；
/// - `500`：数据库异常。
pub async fn ensure_pair_has_no_holders(
    role_a: &Uuid,
    role_b: &Uuid,
    role_a_name: &str,
    role_b_name: &str,
) -> std::result::Result<(), ApiError> {
    let (total, sample) = checker::pair_holders(role_a, role_b)
        .await
        .map_err(|error| utils::db_error(error, "查询互斥对的双持用户"))?;
    if total == 0 {
        return Ok(());
    }
    let usernames = sample
        .iter()
        .take(3)
        .map(|row| row.username.clone())
        .collect::<Vec<_>>();
    tracing::warn!(
        role_a = %role_a,
        role_b = %role_b,
        holders = total,
        "互斥配置被存量双持用户阻止"
    );
    Err(ApiError::new(
        409,
        error_code::ROLE_MUTEX_HAS_HOLDERS,
        holders_message(total, role_a_name, role_b_name, &usernames),
    ))
}

/// 校验点 2：角色由停用变启用前，复核它参与的全部互斥对。
///
/// # 为什么需要它
///
/// 停用角色的绑定不计入有效持有（[`model::HELD_ROLE_SQL`]）。因此
/// 「用户持有 A(生效) + B(停用)」在 B 启用前不算违规；一旦 B 启用，用户就真正双持了。
/// 不在这里拦，互斥就会被「先停用、授完、再启用」这条路径绕过。
///
/// # 参数
///
/// - `role_id`：**即将生效**的角色 ID；
/// - `role_code` / `role_name`：只用于日志与文案（角色名可能是本次刚改的新名字）。
///
/// # 异常情况
///
/// - `409 + 61009 ROLE_MUTEX_CONFLICT`：启用后会有用户双持；
/// - `500`：数据库异常。
pub async fn ensure_role_enable_no_mutex(
    role_id: &Uuid,
    role_code: &str,
    role_name: &str,
) -> std::result::Result<(), ApiError> {
    let hits = model::role_enable_hits(role_id)
        .await
        .map_err(|error| utils::db_error(error, "查询启用角色会造成的双持"))?;
    if hits.is_empty() {
        return Ok(());
    }
    let mut users: Vec<Uuid> = Vec::new();
    let mut pair_names: Vec<String> = Vec::new();
    for hit in &hits {
        if !users.contains(&hit.user_id) {
            users.push(hit.user_id);
        }
        let pair = format!("{role_name}×{}", hit.partner_name);
        if !pair_names.contains(&pair) {
            pair_names.push(pair);
        }
    }
    let usernames = hits
        .iter()
        .map(|hit| hit.username.clone())
        .collect::<Vec<_>>();
    let usernames = dedup_take(&usernames, 3);
    let total = i64::try_from(users.len()).unwrap_or(i64::MAX);
    tracing::warn!(
        role_id = %role_id,
        role_code = %role_code,
        holders = total,
        "启用角色会使互斥关系成立，已阻止"
    );
    Err(ApiError::new(
        409,
        error_code::ROLE_MUTEX_CONFLICT,
        role_enable_message(role_name, total, &pair_names.join("、"), &usernames),
    ))
}

/// 去重后取前 `limit` 个（文案里的「如 a、b、c」，顺序沿用库的 `ORDER BY username`）。
fn dedup_take(items: &[String], limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if out.len() >= limit {
            break;
        }
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(id: u128, name: &str) -> model::RoleSide {
        model::RoleSide {
            id: Uuid::from_u128(id),
            code: name.to_ascii_uppercase(),
            name: name.to_string(),
        }
    }

    fn hit(a: u128, a_name: &str, b: u128, b_name: &str) -> PairHit {
        PairHit {
            mutex_id: Uuid::from_u128(99),
            reason: "不得同时持有采购与付款权限".to_string(),
            a: side(a, a_name),
            b: side(b, b_name),
        }
    }

    /// 规范化：两个方向进来，出去的是同一个 `(小, 大)`。
    #[test]
    fn normalize_pair_is_symmetric() {
        let a = Uuid::from_u128(0xBB);
        let b = Uuid::from_u128(0xAA);
        assert_eq!(normalize_pair(a, b), normalize_pair(b, a));
        assert_eq!(normalize_pair(a, b), (b, a));
    }

    /// 反自反：同一个 UUID → 400 + 61012，且文案不含「互斥自己」这类模糊说法。
    #[test]
    fn self_pair_is_rejected() {
        let a = Uuid::from_u128(7);
        let error = ensure_not_self_pair(a, a).expect_err("自反必须被拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ROLE_MUTEX_SELF_PAIR);
        assert_eq!(error.msg, "不能建立角色与自身的互斥关系。");
    }

    /// 非自反：不同 UUID 通过。
    #[test]
    fn distinct_pair_is_accepted() {
        assert!(ensure_not_self_pair(Uuid::from_u128(1), Uuid::from_u128(2)).is_ok());
    }

    /// 单组命中、B 是本次新增（已持有 A）：文案点名「新的 B」与「已持有的 A」。
    #[test]
    fn single_message_names_granted_side_and_held_side() {
        let one = hit(1, "采购员", 2, "付款审批");
        let text = conflict_message(
            std::slice::from_ref(&one),
            &[Uuid::from_u128(1), Uuid::from_u128(2)],
            &[Uuid::from_u128(1)],
        );
        assert_eq!(
            text,
            "角色「付款审批」与用户已持有的「采购员」互斥：不得同时持有采购与付款权限。\
             请先解除其中一个角色。"
        );
    }

    /// 对称的另一半：已持有 B、本次新增 A（`assign_roles` 传全量集合，两角色都在请求里）。
    #[test]
    fn single_message_flips_when_granted_side_is_a() {
        let one = hit(1, "采购员", 2, "付款审批");
        let text = conflict_message(
            std::slice::from_ref(&one),
            &[Uuid::from_u128(1), Uuid::from_u128(2)],
            &[Uuid::from_u128(2)],
        );
        assert_eq!(
            text,
            "角色「采购员」与用户已持有的「付款审批」互斥：不得同时持有采购与付款权限。\
             请先解除其中一个角色。"
        );
    }

    /// 同次授予两个互斥角色（架构 D3）：文案要说「同时包含」，不能冒充「已持有」。
    #[test]
    fn single_message_handles_same_batch_grant() {
        let one = hit(1, "采购员", 2, "付款审批");
        let text = conflict_message(
            std::slice::from_ref(&one),
            &[Uuid::from_u128(1), Uuid::from_u128(2)],
            &[],
        );
        assert_eq!(
            text,
            "本次授权同时包含互斥角色「采购员」与「付款审批」：\
             不得同时持有采购与付款权限。请先移除其中一个角色。"
        );
    }

    /// 历史违规原样重提（两侧都已在持有里）：文案如实说「已同时持有」。
    #[test]
    fn single_message_reports_existing_violation() {
        let one = hit(1, "采购员", 2, "付款审批");
        let text = conflict_message(
            std::slice::from_ref(&one),
            &[Uuid::from_u128(1), Uuid::from_u128(2)],
            &[Uuid::from_u128(1), Uuid::from_u128(2)],
        );
        assert_eq!(
            text,
            "用户已同时持有互斥角色「采购员」与「付款审批」：\
             不得同时持有采购与付款权限。请先解除其中一个角色。"
        );
    }

    /// 多组命中：给组数 + 角色对，不逐条复述理由。
    #[test]
    fn multi_message_lists_group_count_and_pairs() {
        let hits = vec![hit(1, "采购员", 2, "付款审批"), hit(3, "库管", 4, "稽核")];
        let text = conflict_message(&hits, &[Uuid::from_u128(1)], &[]);
        assert_eq!(
            text,
            "本次授权命中 2 组互斥关系（采购员×付款审批、库管×稽核），请先解除冲突角色。"
        );
    }

    /// 零命中：空串（调用方在空串时不会构造错误）。
    #[test]
    fn no_hits_yields_empty_message() {
        assert!(conflict_message(&[], &[], &[]).is_empty());
    }

    /// 校验点 3 文案：必须同时出现用户数、两个角色名、示例用户名。
    #[test]
    fn holders_message_contains_total_roles_and_sample() {
        let text = holders_message(
            2,
            "采购员",
            "付款审批",
            &["zhangsan".to_string(), "lisi".to_string()],
        );
        assert!(text.starts_with("无法建立互斥关系：2 个用户同时持有「采购员」与「付款审批」"));
        assert!(text.contains("如 zhangsan、lisi"));
        assert!(text.ends_with("请先解除他们的其中一个角色（如 zhangsan、lisi）。"));
    }

    /// 校验点 3 文案：没有样本时不出现空括号。
    #[test]
    fn holders_message_omits_empty_sample() {
        let text = holders_message(1, "采购员", "付款审批", &[]);
        assert_eq!(
            text,
            "无法建立互斥关系：1 个用户同时持有「采购员」与「付款审批」，\
             请先解除他们的其中一个角色。"
        );
    }

    /// 校验点 2 文案：点名被启用的角色 + 互斥对 + 人数。
    #[test]
    fn role_enable_message_names_role_and_pairs() {
        let text = role_enable_message("付款审批", 3, "采购员×付款审批", &["wangwu".to_string()]);
        assert!(text.starts_with("启用角色「付款审批」后将使 3 个用户同时持有互斥角色"));
        assert!(text.contains("（采购员×付款审批）"));
        assert!(text.contains("如 wangwu"));
    }

    /// 去重截断：重复项不占位，最多留 3 个（文案不因同一用户命中两条规则而变长）。
    #[test]
    fn dedup_take_limits_and_dedups() {
        let items: Vec<String> = ["a", "a", "b", "c", "d"]
            .iter()
            .map(|v| v.to_string())
            .collect();
        assert_eq!(
            dedup_take(&items, 3),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }
}
