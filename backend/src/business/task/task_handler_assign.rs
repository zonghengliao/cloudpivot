// ============================================================================
// task_handler_assign.rs —— 分配：分派/转派矩阵、协作人增删查、成员配额与 ROBOT 守卫
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use axum::Json;
use axum::extract::Path;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_handler_common::*;
use super::task_handler_notice::*;
use crate::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput};
use crate::business::shared::principal::{self};
use crate::business::task::dto::{
    self, TaskAssignReq, TaskAssignRes, TaskMemberAddReq, TaskMemberAddRes, TaskMemberRemoveRes,
    TaskMemberRes, TaskMembersRes,
};
use crate::business::task::model::{self};
use crate::shared::ai_actor;
use crate::shared::error_code;
use crate::utils;

/// 当前账号与任务的关系（分派 / 协作人的权限矩阵只看它 + 权限码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActorRole {
    /// 创建人。
    Creator,
    /// 当前负责人。
    Owner,
    /// 分派人。
    Assigner,
    /// 与任务没有行级关系（只能靠权限码 / 超管）。
    Outsider,
}

/// 身份判定（纯函数，单测覆盖全矩阵）。
///
/// 优先级：创建人 > 负责人 > 分派人。一个人同时是「创建人 + 负责人」时按创建人算
/// —— 与 `state::TaskPeople::role_mask` 的位掩码口径一致（那边是全选，这边取最左侧）。
pub(super) fn actor_role(
    me: Uuid,
    creator_id: Uuid,
    owner_id: Uuid,
    assigner_id: Option<Uuid>,
) -> ActorRole {
    if me == creator_id {
        ActorRole::Creator
    } else if me == owner_id {
        ActorRole::Owner
    } else if assigner_id == Some(me) {
        ActorRole::Assigner
    } else {
        ActorRole::Outsider
    }
}

/// 分派 / 转派是否放行：**行级关系（创建人 / 负责人 / 分派人）∪ 权限码**。
///
/// # 为什么不硬拒 `user_type = ROBOT`（修正 11）
///
/// 分派限制**只查权限码**：`ROBOT` 账号没有 `business:task:assign` 时自然被挡
/// （它的行级关系通常也不成立）；一旦管理员把它加进带该权限码的角色（如
/// 「AI 团队主管」），它就**能**分派 —— 「默认不能、可按角色授予」因此自然成立。
/// 在这里按 `user_type` 硬拒，等于把这条路彻底焊死，与修正 11 直接冲突。
pub(super) fn may_assign(role: ActorRole, has_permission: bool) -> bool {
    has_permission || role != ActorRole::Outsider
}

/// 协作人管理是否放行（矩阵与分派相同，权限码换成 `business:task:member`）。
pub(super) fn may_manage_members(role: ActorRole, has_permission: bool) -> bool {
    has_permission || role != ActorRole::Outsider
}

/// 转派后落库的 `assigner_id`（单测覆盖）。
///
/// 口径（卡面 §二 规则 1）：**操作人非创建人时记操作人**；创建人自己派活时沿用原值
/// —— 创建人本来就是这条任务的源头，把 `assigner_id` 指向他反而让「谁派的」变成
/// 冗余信息（`creator_id` 已经说明了一切）。
pub(super) fn next_assigner(
    operator: Uuid,
    creator_id: Uuid,
    current: Option<Uuid>,
) -> Option<Uuid> {
    if operator == creator_id {
        current
    } else {
        Some(operator)
    }
}

/// 创建人 / 负责人是**受保护的协作人**：不许从协作人列表里移除。
///
/// 他们是可见性谓词的前两个分支，移除协作人行并不能让他们看不见任务，
/// 只会让列表少一个人 —— 要么无意义，要么是误操作（卡面 §三 规则 3）。
pub(super) fn protected_member(user_id: Uuid, creator_id: Uuid, owner_id: Uuid) -> bool {
    user_id == creator_id || user_id == owner_id
}

/// 当前账号的 `sys_user.user_type`（`HUMAN` / `ROBOT`；读不到就是 `None`）。
///
/// 判定统一交给 [`crate::shared::ai_actor`]：本模块只负责「把类型读出来」，
/// 不自己写 `== "ROBOT"`（四处入口各写一遍，文案与状态码必然漂移）。
pub(super) async fn user_type_of(user_id: Uuid) -> std::result::Result<Option<String>, ApiError> {
    model::user_type_of(user_id)
        .await
        .map_err(|error| utils::db_error(error, "查询账号类型"))
}

/// 当前账号是不是 AI 用户（`sys_user.user_type = 'ROBOT'`）。
///
/// **只用于文案与「硬拒」**（删任务），不参与分派 / 协作人的准入判定（修正 11）。
pub(super) async fn is_robot(user_id: Uuid) -> std::result::Result<bool, ApiError> {
    Ok(ai_actor::is_robot(user_type_of(user_id).await?.as_deref()))
}

/// 分派被拒 → 403（AI 账号给「怎么才能有权限」的文案，人给的文案列出四条通路）。
pub(super) fn forbidden_assign(is_robot: bool) -> ApiError {
    if is_robot {
        ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "AI 用户默认不能分派任务，如需请授予角色权限（business:task:assign）",
        )
    } else {
        ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "只有任务的创建人、当前负责人、分派人，或持有 business:task:assign 权限的人可以分派 / 转派",
        )
    }
}

/// 协作人管理被拒 → 403（文案同上，权限码换成 `business:task:member`）。
pub(super) fn forbidden_member(is_robot: bool) -> ApiError {
    if is_robot {
        ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "AI 用户默认不能管理任务协作人，如需请授予角色权限（business:task:member）",
        )
    } else {
        ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "只有任务的创建人、当前负责人、分派人，或持有 business:task:member 权限的人可以管理协作人",
        )
    }
}

/// 加协作人的上限判定（纯函数，单测覆盖边界：50 允许、51 拒绝）。
///
/// # 为什么按「现有 + 真正新增」算
///
/// 幂等重复提交不该占额度：一个 50 人的任务再提交「把这 50 人加一遍」，实际
/// 新增 0 人，不该报上限错误（前端整表提交时会经常发生）。
pub(super) fn ensure_member_quota(
    existing: usize,
    adding: usize,
) -> std::result::Result<(), ApiError> {
    let total = existing as i64 + adding as i64;
    if total > dto::MAX_MEMBERS_PER_TASK {
        return Err(ApiError::new(
            400,
            error_code::TASK_MEMBER_LIMIT,
            format!(
                "单个任务最多 {} 个协作人（现有 {existing} 人，本次新增 {adding} 人 = {total} 人）",
                dto::MAX_MEMBERS_PER_TASK
            ),
        ));
    }
    Ok(())
}

/// 协作人行 → 出参。
pub(super) fn member_res(row: &model::TaskMemberRow) -> TaskMemberRes {
    TaskMemberRes {
        user_id: row.user_id.to_string(),
        user_name: row.user_name.clone(),
        role: row.role.clone(),
        created_at: row.created_at,
    }
}

/// 解析并去重协作人 id 列表（空列表 → 400）。
pub(super) fn parse_user_ids(raw: &[String]) -> std::result::Result<Vec<Uuid>, ApiError> {
    let mut ids: Vec<Uuid> = Vec::with_capacity(raw.len());
    for item in raw {
        let text = item.trim();
        if text.is_empty() {
            continue;
        }
        let id = parse_uuid_field(text, "协作人")?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err(param_invalid("user_ids 不能为空".to_string()));
    }
    Ok(ids)
}

/// 分派 / 转派（`PATCH /business/task/{id}/assign`）。
///
/// # 谁能分派（矩阵见 [`may_assign`]）
///
/// | 身份 | 能否 |
/// |------|------|
/// | 创建人 | ✅ |
/// | 当前负责人（转派给别人） | ✅ |
/// | 分派人 | ✅ |
/// | 持有 `business:task:assign`（含超管） | ✅ |
/// | 其他人 | ❌ 403 |
///
/// # 一次分派改五样东西（同一事务）
///
/// ① `owner_id` + `dept_id` 快照（不换 dept 会让「原部门看板」继续显示这条任务）；
/// ② `assigner_id`（操作人非创建人时 = 操作人）；③ `source = TRANSFERRED`；
/// ④ **原负责人自动成为协作人**（否则他一转派就再也看不见这条任务了）；
/// ⑤ 一条 `ASSIGN` 评论 + 一条变更日志（`owner_id / assigner_id / source` 三列）。
///
/// # 通知是**强制**的（修正 8）
///
/// 新负责人「你被分派了任务」、原负责人「任务已被转派」两条都 `force = true`：
/// 收件人把 `notify.inbox` 关掉也照发（漏掉 = 没人干活，是直接损失）。
///
/// # 异常情况
///
/// - 400 + `10001`：路径 / 请求里的 UUID 非法；400 + `63017`：新负责人=当前负责人；
/// - 400 + `63009`：新负责人不存在或已停用；
/// - 403 + `63002`：无权分派（AI 账号会拿到「请授予角色权限」的文案）；
/// - 404 + `63001`：任务不存在 / 已软删；
/// - 409 + `63018`：并发转派（负责人已被别人改掉）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/assign",
    method = "PATCH",
    permission = "business:task:read"
)]
#[nik::patch("/business/task/{id}/assign")]
pub async fn assign(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskAssignReq>,
) -> ResResult<TaskAssignRes> {
    let me = current_user(&claims)?;
    let task_id = parse_uuid_field(&id, "任务")?;
    // 分派属写操作：沿用删除接口的 403 口径（不用可见性的 404），
    // 否则「看不见任务的人来分派」会得到 404，验收要求的是 403。
    let current = require_task(task_id).await?;

    // ① 权限矩阵：行级关系 ∪ 权限码
    let role = actor_role(
        me,
        current.creator_id,
        current.card.owner_id,
        current.assigner_id,
    );
    if !may_assign(role, claims.has_permission(dto::PERM_ASSIGN)) {
        return Err(forbidden_assign(is_robot(me).await?));
    }

    // ② 新负责人：合法 uuid + 有效用户（ROBOT **可以**被分派：AI 是执行者）
    let new_owner = parse_uuid_field(req.owner_id.trim(), "新负责人")?;
    if new_owner == current.card.owner_id {
        return Err(ApiError::new(
            400,
            error_code::TASK_ASSIGN_SAME_OWNER,
            "新负责人与当前负责人相同，无需分派 / 转派",
        ));
    }
    if !model::user_exists(new_owner)
        .await
        .map_err(|error| utils::db_error(error, "校验新负责人"))?
    {
        return Err(ApiError::new(
            400,
            error_code::TASK_OWNER_INVALID,
            "新负责人不存在或已停用（已删除 / status≠1 的账号不能被分派）",
        ));
    }
    let dept_id = model::user_dept(new_owner)
        .await
        .map_err(|error| utils::db_error(error, "查询新负责人所属部门"))?;
    let new_owner_name = model::user_display_name(new_owner)
        .await
        .map_err(|error| utils::db_error(error, "查询新负责人姓名"))?;
    let reason = normalize_reason(req.reason.as_deref())?;

    let old_owner_id = current.card.owner_id;
    let old_owner_name = current.card.owner_name.clone();
    let assigner_id = next_assigner(me, current.creator_id, current.assigner_id);
    let operator_name = display_name(&claims);

    // ③ 评论正文（详情页时间线：「谁把活从谁手里拿给了谁」）
    let mut content = format!("分派：负责人由 {old_owner_name} 改为 {new_owner_name}");
    if let Some(reason) = reason.as_deref() {
        content.push_str(&format!("；原因：{reason}"));
    }

    // ④ 一个事务：改负责人 → 原负责人入库协作人 → ASSIGN 评论 → 变更日志
    let change = model::OwnerChange {
        task_id,
        owner_id: new_owner,
        dept_id,
        assigner_id,
        source: dto::SOURCE_TRANSFERRED.to_string(),
        operator: me,
        expected_owner: old_owner_id,
    };
    let before = serde_json::json!({
        "owner_id": old_owner_id.to_string(),
        "assigner_id": current.assigner_id.map(|id| id.to_string()),
        "source": current.card.source,
    });
    let after = serde_json::json!({
        "owner_id": new_owner.to_string(),
        "assigner_id": assigner_id.map(|id| id.to_string()),
        "source": dto::SOURCE_TRANSFERRED,
    });
    let operator_name_tx = operator_name.clone();
    let (comment_id, affected, auto_member) = db()
        .transaction(|tx| async move {
            let affected = model::assign_owner(&tx, &change).await?;
            if affected == 0 {
                return Ok((Uuid::nil(), 0_u64, false));
            }
            let auto_member = old_owner_id != new_owner;
            if auto_member {
                model::add_member(
                    &tx,
                    task_id,
                    old_owner_id,
                    dto::MEMBER_ROLE_COLLABORATOR,
                    me,
                )
                .await?;
            }
            let comment_id =
                model::insert_comment(&tx, task_id, me, &content, model::COMMENT_TYPE_ASSIGN, me)
                    .await?;
            changelog::record(
                &tx,
                &ChangeCtx::api(Some(me), Some(&operator_name_tx)),
                ChangeTarget::of(
                    "biz_task",
                    task_id.to_string(),
                    Action::Assign,
                    "分派 / 转派",
                ),
                DiffInput::update(&before, &after),
            )
            .await
            .map_err(|error| {
                DbError::InvalidValue(format!("变更日志写入失败 [{}] {}", error.code, error.msg))
            })?;
            Ok((comment_id, affected, auto_member))
        })
        .await
        .map_err(|error| utils::db_error(error, "分派任务"))?;
    if affected == 0 {
        return Err(ApiError::new(
            409,
            error_code::TASK_ASSIGN_CONFLICT,
            "任务负责人已被他人修改（或任务已删除），请刷新后重试",
        ));
    }

    // ⑤ 通知（**强制**：force = true，偏好在服务端被绕过）。事务外发：通知失败不回滚业务。
    let updated = require_task(task_id).await?;
    let mut notified: Vec<Uuid> = Vec::new();
    notified.extend(
        dispatch_task_notice(
            &updated,
            TaskNotice {
                recipients: vec![new_owner],
                force: true,
                exclude_operator: true,
                title: "你被分派了任务",
                content: &format!(
                    "任务「{}」（{}）已由 {operator_name} 分派给你，请打开任务详情确认",
                    updated.card.title, updated.card.task_no
                ),
            },
            me,
            &operator_name,
        )
        .await,
    );
    if old_owner_id != new_owner {
        notified.extend(
            dispatch_task_notice(
                &updated,
                TaskNotice {
                    recipients: vec![old_owner_id],
                    force: true,
                    // 原负责人常常就是操作人：这条回执**必须**发给他（卡面 §二 规则 4）
                    exclude_operator: false,
                    title: "任务已被转派",
                    content: &format!(
                        "任务「{}」（{}）已由 {operator_name} 转派给 {new_owner_name}；你已自动成为协作人，仍可查看进度",
                        updated.card.title, updated.card.task_no
                    ),
                },
                me,
                &operator_name,
            )
            .await,
        );
    }
    notified.sort();
    notified.dedup();

    tracing::info!(
        task_id = %task_id,
        from = %old_owner_id,
        to = %new_owner,
        operator = %me,
        auto_member,
        "任务已分派 / 转派"
    );
    Ok(Res::ok(TaskAssignRes {
        id: task_id.to_string(),
        task_no: updated.card.task_no.clone(),
        owner_id: new_owner.to_string(),
        previous_owner_id: old_owner_id.to_string(),
        assigner_id: assigner_id.map(|id| id.to_string()),
        source: dto::SOURCE_TRANSFERRED.to_string(),
        auto_member,
        comment_id: comment_id.to_string(),
        notified: notified.iter().map(|id| id.to_string()).collect(),
    }))
}

/// 加协作人（`POST /business/task/{id}/members`）。
///
/// # 规则
///
/// 1. 谁能加：创建人 / 负责人 / 分派人 ∪ `business:task:member`（矩阵同分派）；
/// 2. 协作人**自动获得可见性**（`biz_task_member` 已在可见性 id 数组里，T1）；
/// 3. **创建人 / 负责人不入协作人行**（他们本来就能看，落行只会让列表虚胖）——
///    请求里带了他们不算错：进回执的 `skipped`；
/// 4. **幂等**：已经在里头的进 `existing`，不报错、不重复插；
/// 5. 上限 **50 人**（现有 + 本次新增，超了 400 + `63014`）；
/// 6. 强制通知（`force = true`）：被加的人一定收到。
///
/// # 异常情况
///
/// - 400 + `10001`：uuid 非法 / `user_ids` 为空；400 + `63019`：含无效用户；
/// - 400 + `63014`：超过 50 人上限；
/// - 403 + `63002`：无权管理协作人；404 + `63001`：任务不存在 / 已软删；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/members",
    method = "POST",
    permission = "business:task:read"
)]
#[nik::post("/business/task/{id}/members")]
pub async fn add_members(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskMemberAddReq>,
) -> ResResult<TaskMemberAddRes> {
    let me = current_user(&claims)?;
    let task_id = parse_uuid_field(&id, "任务")?;
    let current = require_task(task_id).await?;
    let role = actor_role(
        me,
        current.creator_id,
        current.card.owner_id,
        current.assigner_id,
    );
    if !may_manage_members(role, claims.has_permission(dto::PERM_MEMBER)) {
        return Err(forbidden_member(is_robot(me).await?));
    }

    let wanted = parse_user_ids(&req.user_ids)?;

    // ① 一次性校验有效性（一条 SQL 取出有效 id，差集就是不能加的人）
    let valid = model::valid_users(&wanted)
        .await
        .map_err(|error| utils::db_error(error, "校验协作人"))?;
    let invalid: Vec<String> = wanted
        .iter()
        .filter(|id| !valid.contains(id))
        .map(|id| id.to_string())
        .collect();
    if !invalid.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::TASK_MEMBER_INVALID,
            format!(
                "以下用户不存在或已停用，不能加为协作人：{}",
                invalid.join(", ")
            ),
        ));
    }

    // ② 创建人 / 负责人跳过；其余分「已存在」与「待新增」
    let existing_ids = model::member_ids(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务协作人"))?;
    let mut skipped: Vec<Uuid> = Vec::new();
    let mut existing: Vec<Uuid> = Vec::new();
    let mut to_add: Vec<Uuid> = Vec::new();
    for id in &wanted {
        if protected_member(*id, current.creator_id, current.card.owner_id) {
            skipped.push(*id);
        } else if existing_ids.contains(id) {
            existing.push(*id);
        } else {
            to_add.push(*id);
        }
    }

    // ③ 上限：现有 + 本次新增 ≤ 50（按「去重后真正会新增的人数」算，幂等重复不加人）
    ensure_member_quota(existing_ids.len(), to_add.len())?;

    // ④ 集合式入库（一条 INSERT ... SELECT，返回真正新增的 id）
    let added = if to_add.is_empty() {
        Vec::new()
    } else {
        let ids = to_add.clone();
        db().transaction(|tx| async move { model::insert_members(&tx, task_id, &ids, me).await })
            .await
            .map_err(|error| utils::db_error(error, "加任务协作人"))?
    };

    // ⑤ 通知（强制：协作关系是「你被拉进来了」，不能因为偏好静默丢失）
    if !added.is_empty() {
        let content = format!(
            "任务「{}」（{}）已把你加为协作人，你可以查看进度、评论并收到后续通知",
            current.card.title, current.card.task_no
        );
        dispatch_task_notice(
            &current,
            TaskNotice {
                recipients: added.clone(),
                force: true,
                exclude_operator: true,
                title: "你被加为任务协作人",
                content: &content,
            },
            me,
            &display_name(&claims),
        )
        .await;
    }

    let members = model::list_members(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务协作人"))?;
    let total = members.len() as i64;
    tracing::info!(task_id = %task_id, added = added.len(), existing = existing.len(), skipped = skipped.len(), "任务协作人已添加");
    Ok(Res::ok(TaskMemberAddRes {
        task_id: task_id.to_string(),
        added: added.iter().map(|id| id.to_string()).collect(),
        existing: existing.iter().map(|id| id.to_string()).collect(),
        skipped: skipped.iter().map(|id| id.to_string()).collect(),
        list: members.iter().map(member_res).collect(),
        total,
        limit: dto::MAX_MEMBERS_PER_TASK,
    }))
}

/// 移除协作人（`DELETE /business/task/{id}/members/{user_id}`）。
///
/// # 规则
///
/// - 权限同「加协作人」；
/// - **创建人 / 负责人不许移除**（400 + `63015`）：他们本来就能看，「移除」只会
///   造成「列表里没有他、但他照样能改任务」的错觉；
/// - 不是协作人 → 404 + `63020`（不静默成功：否则前端会以为移除生效了）；
/// - 通知**尊重偏好**（`force = false`，修正 8 的噪音档）。
///
/// # 异常情况
///
/// - 400 + `10001`：uuid 非法；400 + `63015`：目标是创建人 / 负责人；
/// - 403 + `63002`：无权管理协作人；404 + `63001` / `63020`：任务 / 协作关系不存在；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/members/{user_id}",
    method = "DELETE",
    permission = "business:task:read"
)]
#[nik::delete("/business/task/{id}/members/{user_id}")]
pub async fn remove_member(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, user_id)): Path<(String, String)>,
) -> ResResult<TaskMemberRemoveRes> {
    let me = current_user(&claims)?;
    let task_id = parse_uuid_field(&id, "任务")?;
    let target = parse_uuid_field(&user_id, "协作人")?;
    let current = require_task(task_id).await?;
    let role = actor_role(
        me,
        current.creator_id,
        current.card.owner_id,
        current.assigner_id,
    );
    if !may_manage_members(role, claims.has_permission(dto::PERM_MEMBER)) {
        return Err(forbidden_member(is_robot(me).await?));
    }
    if protected_member(target, current.creator_id, current.card.owner_id) {
        return Err(ApiError::new(
            400,
            error_code::TASK_MEMBER_PROTECTED,
            "创建人和负责人本来就是任务的可见方，不能从协作人里移除（要换人请用分派 / 转派）",
        ));
    }

    let removed = db()
        .transaction(|tx| async move { model::remove_member(&tx, task_id, target).await })
        .await
        .map_err(|error| utils::db_error(error, "移除任务协作人"))?;
    if removed == 0 {
        return Err(ApiError::new(
            404,
            error_code::TASK_MEMBER_NOT_FOUND,
            "该用户不是本任务的协作人",
        ));
    }

    // 尊重偏好（force = false）：这是噪音档通知，收件人关了站内信就不发。
    let content = format!(
        "任务「{}」（{}）已把你从协作人中移除，你将不再收到该任务的更新",
        current.card.title, current.card.task_no
    );
    dispatch_task_notice(
        &current,
        TaskNotice {
            recipients: vec![target],
            force: false,
            exclude_operator: true,
            title: "你已从任务协作人中移除",
            content: &content,
        },
        me,
        &display_name(&claims),
    )
    .await;

    let members = model::list_members(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务协作人"))?;
    let total = members.len() as i64;
    tracing::info!(task_id = %task_id, user_id = %target, operator = %me, "任务协作人已移除");
    Ok(Res::ok(TaskMemberRemoveRes {
        task_id: task_id.to_string(),
        user_id: target.to_string(),
        removed: true,
        total,
    }))
}

/// 协作人列表（`GET /business/task/{id}/members`）。
///
/// # 可见性
///
/// 走 [`model::find_detail`]（数据范围 ∪ 参与关系）：看不见的任务一律 **404**
/// —— 协作人列表会暴露「这条任务上都有谁」，不能给看不见任务的人看。
///
/// # 异常情况
///
/// - 400 + `10001`：uuid 非法；404 + `63001`：任务不存在 / 不可见；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/members",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/members")]
pub async fn members(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskMembersRes> {
    let mine = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;
    model::find_detail(&mine, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;
    let rows = model::list_members(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务协作人"))?;
    let total = rows.len() as i64;
    Ok(Res::ok(TaskMembersRes {
        task_id: task_id.to_string(),
        list: rows.iter().map(member_res).collect(),
        total,
        limit: dto::MAX_MEMBERS_PER_TASK,
    }))
}

// 写路径的权限判断口径（说明）：
//
// 编辑用 `find_raw` + 「我是不是负责人/创建人」，不碰数据范围 —— 协作人虽然
// *能看见* 任务，但 **不能改**（可见性 ≠ 写权限）。只有删除需要判「超管」，
// 那里才解析 `ScopedPrincipal`。

// ==================== 单元测试（纯逻辑） ====================

// ==================== 流程来源（P-FLOW-1A） ====================
