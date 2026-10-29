//! AI 用户接口层（云枢 × Hermes · L1）。
//!
//! 依据：本卡（AI 用户创建：profile 自动创建 + SOUL.md 编辑 + 事务回滚）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/system/user/ai` | `system:user:create` | 创建 AI 用户（建 `sys_user` + Hermes profile + SOUL.md + workspace） |
//! | PUT | `/system/user/ai/{user_id}` | `system:user:update` | 编辑职责 / SOUL.md / 模型 |
//! | GET | `/system/user/ai/{user_id}` | `system:user:list` | 读配置（含 SOUL.md 全文与真实路径） |
//! | GET | `/system/user/ai/{user_id}/status` | `system:user:list` | 探活（目录 / SOUL.md / workspace / CLI / gateway / 凭据） |
//! | POST | `/system/user/ai/{user_id}/repair` | `system:user:update` | 修复 profile 级凭据（补 `.env` 缺失的 provider key） |
//! | DELETE | `/system/user/ai/{user_id}` | `system:user:delete` | 删号（备份 SOUL.md → 删 profile → 删 workspace → 软删 user） |
//!
//! # 为什么复用 `system:user:*` 权限码
//!
//! 卡面没有分配新权限码，而「建 AI 用户」与「建人类用户」是同一权限等级（都是建账号，
//! 都要分配数据范围）。新开 `system:user:ai:*` 需要同时改 `sys_menu` 种子、`V0103` 式授权
//! 与前端声明，属于另一张卡的体量；此处按「同一动作同一码」处理，并在模块文档里写明。
//!
//! # 事务边界（**本卡最容易被评审判错的地方**）
//!
//! 「建 Hermes profile」是**外部副作用**，不能进数据库事务（CLI 不会跟着 PG 回滚）。
//! 而「`user_type='ROBOT'` ⇔ `sys_ai_agent` 有该 user_id」是必须成立的**不变式**，
//! 所以创建流程分成「一次原子写库 + 三步外部副作用 + 补偿」：
//!
//! ```text
//! ① 事务（提交）：INSERT sys_user → UPDATE user_type=ROBOT → INSERT sys_ai_agent
//!                  任一失败 → 整个事务回滚（库内零残留，且中间态不可见）
//! ② hermes profile create            ← 失败 → 补偿：物理删除两行（同事务）
//! ③ 写 SOUL.md（原子）               ← 失败 → 补偿：删 profile + 物理删除两行
//! ④ 建 workspace                     ← 失败 → 补偿：删 profile + 删 workspace + 物理删除两行
//! ⑤ 补齐 + 自检 profile 级凭据        ← 失败 → 同上（缺 provider key 的账号「能建号不能干活」，
//!                                      worker 一启动就 AuthError，比建号失败更糟）
//! ⑥ 探活回写 gateway_status（失败忽略：不影响正确性）
//! ```
//!
//! 「补偿」以**磁盘事实**为准（探测目录是否存在），不看标志位：CLI 可能已经建出目录
//! 而后续步骤才失败，标志位那时还是 false，只看标志位会留孤儿 profile。
//! 库内两行走**物理删除**（账号从未对用户可见，软删只会留一条指向不存在账号的垃圾行）；
//! 删号（`DELETE /system/user/ai/{id}`）仍走软删，两者语义不同、不共用。
//!
//! 补偿失败**不会被吞掉**：错误消息里逐条列出「回滚未完成项」，业务码换成
//! [`error_code::AI_AGENT_ROLLBACK_FAILED`]，让运维知道需要人工收尾（这比伪装成功、
//! 留下一份孤立 profile 安全得多）。

use alun::prelude::*;
use alun::uuid::Uuid;
use axum::Json;
use axum::extract::Path;
use axum::http::HeaderMap;

use crate::business::aiagent::dto::{
    AiUserCreateReq, AiUserCredentialRes, AiUserDeleteRes, AiUserRes, AiUserStatusRes,
    AiUserUpdateReq, normalize_description,
};
use crate::business::aiagent::hermes::Hermes;
use crate::business::aiagent::model::{self, AiAgentRow, AiUserBrief};
use crate::shared::ai_actor;
use crate::shared::constants::EnableStatus;
use crate::shared::error_code;
use crate::system::user::model::{self as user_model, SysUserModel, SysUserRoleModel};
use crate::utils;

// ==================== 创建 ====================

/// 创建 AI 用户。
///
/// # 参数
///
/// - `claims`：当前操作者（写入 `create_by`）；
/// - `req`：用户名（= profile 名）/ 显示名 / 职责 / SOUL.md / 模型覆盖 / 克隆模板。
///
/// # 创建流程（见模块文档的「一次原子写库 + 补偿」）
///
/// 1. 校验 profile 名（正则，红线）与职责长度；
/// 2. 唯一性：`sys_user.username` + `sys_ai_agent.hermes_profile`（均只算未软删行）；
/// 3. **一个事务**：`INSERT sys_user` → `UPDATE user_type=ROBOT` → `INSERT sys_ai_agent`
///    （不变式的写侧保证：不允许「只有账号」或「只有配置」的中间态）；
/// 4. `hermes profile create <name> --clone-from <模板> --description <职责> --no-alias`；
/// 5. 写 `SOUL.md`（内容已随 3 落库；留空则用模板渲染）；
/// 6. 建 `<workspace_root>/<name>/`；
/// 7. **补齐并自检 profile 级凭据**（`<profiles_root>/<name>/.env` 里当前 provider 的 key
///    非空且权限 600）——不满足就是创建失败，因为 worker 的凭据池只读 profile 级 `.env`；
/// 8. 探活回写 `gateway_status`；
/// 9. 4–7 任一步失败 → 补偿删除 profile / workspace + 同事务软删两行。
///
/// # 返回值
///
/// [`AiUserRes`]（含 SOUL.md 全文、profile 路径、workspace 路径）。
///
/// # 异常情况
///
/// - 400 + `64002`：profile 名不合法（含命令注入企图，一次进程都不会启动）；
/// - 400 + `64010`：职责超长；
/// - 400 + `64012`：模型覆盖不是 JSON 对象；
/// - 404 + `64004`：克隆模板 profile 不存在（Hermes 侧报错）；
/// - 409 + `64003`：用户名 / profile 名已被占用；
/// - 500 + `64005`：Hermes CLI 失败（消息里带 stderr 前 500 字符）；
/// - 500 + `64014`：profile 级凭据不可用（全局 `.env` 与进程环境都没有 provider key，
///   或 `.env` 权限不是 600）——**已回滚，磁盘与库零残留**；
/// - 500 + `64009`：回滚未完全成功（消息里逐条列出残留项）。
#[alun::permission(
    path = "/system/user/ai",
    method = "POST",
    permission = "system:user:create"
)]
#[alun::post("/system/user/ai")]
pub async fn create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<AiUserCreateReq>,
) -> ResResult<AiUserRes> {
    // 红线（T5）：只有人类管理员能建 AI 用户（AI 建 AI = 自我复制，没人是它的上级）
    forbid_robot(&claims).await?;
    let hermes = Hermes::from_env();
    let username = req.username.trim().to_string();
    // 红线：任何命令之前先校验名字（`hermes` 会把 Bad_Name 静默小写成 bad_name 并建盘）
    Hermes::validate_profile_name(&username)?;

    let description = normalize_description(req.agent_description.as_deref())?;
    let real_name = req
        .real_name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let template = match req
        .clone_from
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(raw) => {
            // 模板名同样进命令参数，必须同样校验（第二条注入面）；
            // 允许 `default`（Hermes 内置模板）—— 见 Hermes::validate_template_name。
            Hermes::validate_template_name(raw)?;
            Some(raw.to_string())
        }
        None => None,
    };
    let model_override = normalize_model(req.model_override)?;

    // 唯一性：账号与 profile 名是两套唯一约束，都要查
    let taken = user_model::count_users_by_username(&username, None)
        .await
        .map_err(|e| utils::db_error(e, "校验用户名唯一性"))?;
    if taken > 0 {
        return Err(ApiError::new(
            409,
            error_code::AI_AGENT_PROFILE_EXISTS,
            format!("用户名 {username} 已被占用"),
        ));
    }
    if model::profile_exists(&username)
        .await
        .map_err(|e| utils::db_error(e, "校验 profile 名唯一性"))?
    {
        return Err(ApiError::new(
            409,
            error_code::AI_AGENT_PROFILE_EXISTS,
            format!("Hermes profile {username} 已被占用"),
        ));
    }

    // ---- ① 两行**同事务**（laoliu 追加不变式：`user_type='ROBOT'` ⇔ `sys_ai_agent` 有该 user_id）
    //
    // 顺序刻意是「先算好 SOUL.md 与 workspace 路径，再开事务」：
    // 这样事务里能一次写完 `sys_user`（打标 ROBOT）与 `sys_ai_agent`，
    // 不会出现「只有账号没有配置」或「只有配置没有账号」的中间态（两条都不合法）。
    // 事务之外的 profile / workspace 属于**外部副作用**（CLI 不跟着 PG 回滚），
    // 失败由 ②–④ 的补偿删除兜底（见 `compensate`）。
    let user_id = Uuid::now_v7();
    let now = chrono::Utc::now();
    let operator = utils::current_user_uuid(&claims);
    let soul_md = req
        .soul_md
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            // 注意：这里不能用 `hermes::render_default_soul` —— 上面的 `hermes` 是本地的
            // `Hermes` 实例，会遮蔽同名模块别名
            crate::business::aiagent::hermes::render_default_soul(
                &username,
                real_name.as_deref().unwrap_or(""),
                &description,
            )
        });
    let workspace_path = hermes.workspace_path(&username);
    let mut row = AiAgentRow {
        user_id,
        hermes_profile: username.clone(),
        workspace_path: workspace_path.to_string_lossy().to_string(),
        soul_md,
        agent_description: (!description.is_empty()).then(|| description.clone()),
        model_override,
        gateway_status: "unknown".to_string(),
        created_at: now,
        updated_at: now,
    };
    let new_user = SysUserModel {
        id: user_id,
        username: username.clone(),
        // 机器人不登录（登录闸门属 T5）：口令随机生成且永不外泄，满足 NOT NULL
        password: robot_password_hash()?,
        real_name: real_name.clone(),
        email: None,
        mobile: None,
        dept_id: None,
        status: EnableStatus::enabled,
        login_fail_count: 0,
        is_locked: false,
        lock_time: None,
        last_login_at: None,
        remark: Some("云枢 × Hermes AI 用户（profile 自动创建）".to_string()),
        must_change_password: false,
        token_version: 0,
        created_at: now,
        updated_at: now,
        create_by: operator,
        update_by: operator,
        deleted_at: None,
    };
    let tx_user = new_user.clone();
    let tx_row = row.clone();
    db().transaction(|tx| async move {
        let mut u = tx_user;
        u.insert(&tx).await?;
        // 同事务打标：不允许「HUMAN 的 AI 用户」落盘
        model::mark_user_robot(&tx, &u.id).await?;
        // 同事务写配置行：任一 INSERT 失败 → 两行一起回滚（不变式的写侧保证）
        model::insert(&tx, &tx_row).await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "创建 AI 用户（sys_user + sys_ai_agent 同事务）"))?;

    // ---- ② profile（外部副作用，失败 → 补偿：软删两行，不留孤立账号/配置）
    let mut profile_created = false;
    let mut workspace_created = false;
    if let Err(e) = hermes
        .create_profile(&username, template.as_deref(), &description)
        .await
    {
        return Err(compensate(
            &hermes,
            &user_id,
            &username,
            profile_created,
            workspace_created,
            e,
        )
        .await);
    }
    profile_created = true;

    // ---- ③ SOUL.md 落盘（内容已在 ① 写进库；这里写磁盘，失败同样整体补偿）
    if let Err(e) = hermes.write_soul(&username, &row.soul_md).await {
        return Err(compensate(
            &hermes,
            &user_id,
            &username,
            profile_created,
            workspace_created,
            e,
        )
        .await);
    }

    // ---- ④ workspace（`hermes profile create` 不建 workspace，卡面要求建号即建目录）
    match hermes.ensure_workspace(&username).await {
        Ok(path) => {
            workspace_created = true;
            row.workspace_path = path.to_string_lossy().to_string();
        }
        Err(e) => {
            return Err(compensate(
                &hermes,
                &user_id,
                &username,
                profile_created,
                workspace_created,
                e,
            )
            .await);
        }
    }

    // ---- ⑤ 凭据自检（**创建成功判定的一部分**：能不能干活）
    //
    // `create_profile` 内部已经硬性补齐并自检过一次；这里是**接口层的判定**，
    // 也是卡面要求的「成功条件」：profile 级 `.env` 里当前 provider 的 key 非空
    // 且权限 600，否则整个创建算失败并回滚。
    // 为什么值得判两次：`create_profile` 是 Hermes 适配层的自检（它自己写完立刻读回），
    // 而这里读的是「调用方视角的最终状态」——两者之间还可能有人改了盘（运维手工改 .env、
    // 别的卡 clone 覆盖）。代价是一次读文件，收益是「建号 API 返回 200」这件事
    // 从此等于「worker 认证能过」。
    match hermes.verify_profile_credentials(&username).await {
        Ok(check) if check.ok => tracing::info!(
            user_id = %user_id,
            profile = %username,
            provider = ?check.provider,
            required_key = ?check.required_key,
            keys = ?check.present_keys,
            mode = ?check.env_file_mode,
            "AI 员工凭据自检通过（worker 可认证）"
        ),
        Ok(check) => {
            return Err(compensate(
                &hermes,
                &user_id,
                &username,
                profile_created,
                workspace_created,
                ApiError::new(
                    500,
                    error_code::AI_AGENT_CREDENTIAL_MISSING,
                    check.failure_message(),
                ),
            )
            .await);
        }
        Err(e) => {
            return Err(compensate(
                &hermes,
                &user_id,
                &username,
                profile_created,
                workspace_created,
                e,
            )
            .await);
        }
    }

    // ---- ⑥ 立刻探活（body 里的 gateway_status 就不是「未知」）
    if let Ok(status) = hermes.status(&username).await {
        row.gateway_status = status.gateway;
        let _ = model::update_gateway_status(db(), &user_id, &row.gateway_status).await;
    }

    let brief = AiUserBrief {
        username: username.clone(),
        real_name,
        user_type: "ROBOT".to_string(),
    };
    tracing::info!(
        user_id = %user_id,
        profile = %username,
        gateway = %row.gateway_status,
        workspace_ready = workspace_created,
        "AI 用户已创建（Hermes profile + SOUL.md + workspace）"
    );
    Ok(Res::ok(to_res(&hermes, &row, Some(&brief))))
}

// ==================== 编辑 ====================

/// 编辑 AI 用户（职责 / SOUL.md / 模型）。
///
/// # 顺序（先外部副作用，后落库）
///
/// 1. 读库里的旧行（不存在 → 404）；
/// 2. 有 `soul_md` → **原子写** `<profiles_root>/<name>/SOUL.md`；
/// 3. 有 `agent_description` → `hermes profile describe <name> --text …`；
/// 4. 合并后 `UPDATE sys_ai_agent`。
///
/// 先写文件再写库：文件写失败时库里还是旧值，前端重试即可；若反过来（先落库后写盘），
/// 库说「已生效」而盘上是旧的，漂移不会被任何人发现。
///
/// # 异常情况
///
/// - 404 + `64001`：该用户不是 AI 用户 / 已删除；
/// - 404 + `64007`：profile 目录丢失（要求重建后再保存 SOUL.md）；
/// - 400 + `64010`：职责超长；400 + `64012`：模型覆盖非法；
/// - 500 + `64005`：Hermes CLI 失败（含 stderr 摘录）。
#[alun::permission(
    path = "/system/user/ai/{user_id}",
    method = "PUT",
    permission = "system:user:update"
)]
#[alun::put("/system/user/ai/{user_id}")]
pub async fn update(
    Path(user_id): Path<String>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<AiUserUpdateReq>,
) -> ResResult<AiUserRes> {
    // 红线（T5）：AI 用户不许改自己 / 他人的 profile（含 SOUL.md）
    forbid_robot(&claims).await?;
    let id = utils::require_uuid(&user_id, "user_id")?;
    let hermes = Hermes::from_env();
    let mut row = model::find_by_user(&id)
        .await
        .map_err(|e| utils::db_error(e, "查询 AI 用户"))?
        .ok_or_else(|| ai_not_found(&id))?;
    let brief = model::find_user_brief(&id)
        .await
        .map_err(|e| utils::db_error(e, "查询 AI 用户账号信息"))?;

    if let Some(soul_md) = req.soul_md.as_deref() {
        hermes.write_soul(&row.hermes_profile, soul_md).await?;
        row.soul_md = soul_md.to_string();
    }
    if let Some(raw) = req.agent_description.as_deref() {
        let description = normalize_description(Some(raw))?;
        hermes.describe(&row.hermes_profile, &description).await?;
        row.agent_description = (!description.is_empty()).then_some(description);
    }
    if let Some(model) = req.model_override.clone() {
        row.model_override = normalize_model(Some(model))?;
    }

    model::update_content(db(), &row)
        .await
        .map_err(|e| utils::db_error(e, "更新 AI 用户配置"))?;

    let operator = claims.sub.clone();
    tracing::info!(
        user_id = %id,
        profile = %row.hermes_profile,
        operator = %operator,
        "AI 用户配置已更新（SOUL.md / 职责 / 模型）"
    );
    Ok(Res::ok(to_res(&hermes, &row, brief.as_ref())))
}

// ==================== 详情 ====================

/// 读 AI 用户配置（含 SOUL.md 全文）。
///
/// # 返回值
///
/// [`AiUserRes`]：`soul_md` 是**库内副本**（写入时与磁盘同源），`soul_path` 是磁盘真实路径，
/// 前端「保存前提示」直接展示它。磁盘与库是否一致由 `/status` 的 `soul_file_exists` 兜底。
///
/// # 异常情况
///
/// - 404 + `64001`：该用户不是 AI 用户 / 已删除。
#[alun::permission(
    path = "/system/user/ai/{user_id}",
    method = "GET",
    permission = "system:user:list"
)]
#[alun::get("/system/user/ai/{user_id}")]
pub async fn detail(Path(user_id): Path<String>) -> ResResult<AiUserRes> {
    let id = utils::require_uuid(&user_id, "user_id")?;
    let hermes = Hermes::from_env();
    let row = model::find_by_user(&id)
        .await
        .map_err(|e| utils::db_error(e, "查询 AI 用户"))?
        .ok_or_else(|| ai_not_found(&id))?;
    let brief = model::find_user_brief(&id)
        .await
        .map_err(|e| utils::db_error(e, "查询 AI 用户账号信息"))?;
    Ok(Res::ok(to_res(&hermes, &row, brief.as_ref())))
}

// ==================== 探活 ====================

/// 探活：profile 目录 / SOUL.md / workspace / CLI / gateway。
///
/// # 说明
///
/// 本接口会产生 2–4 次 Hermes CLI / 文件系统调用（Local 形态毫秒级，Ssh 形态约 1–3 s），
/// 因此定位是**运维按钮**而不是列表轮询源；列表页只在打开详情时调一次。
///
/// 探测到的 `gateway` 会回写 `sys_ai_agent.gateway_status`（失败忽略：探活不应写坏库）。
///
/// # 异常情况
///
/// - 404 + `64001`：该用户不是 AI 用户 / 已删除。
#[alun::permission(
    path = "/system/user/ai/{user_id}/status",
    method = "GET",
    permission = "system:user:list"
)]
#[alun::get("/system/user/ai/{user_id}/status")]
pub async fn status(Path(user_id): Path<String>) -> ResResult<AiUserStatusRes> {
    let id = utils::require_uuid(&user_id, "user_id")?;
    let hermes = Hermes::from_env();
    let row = model::find_by_user(&id)
        .await
        .map_err(|e| utils::db_error(e, "查询 AI 用户"))?
        .ok_or_else(|| ai_not_found(&id))?;
    let probe = hermes.status(&row.hermes_profile).await?;
    if probe.gateway != row.gateway_status && probe.gateway != "unknown" {
        let _ = model::update_gateway_status(db(), &id, &probe.gateway).await;
    }
    Ok(Res::ok(AiUserStatusRes::new(
        &id,
        &row.hermes_profile,
        &row.gateway_status,
        &probe,
    )))
}

// ==================== 凭据修复（L1 修复） ====================

/// 修复 AI 用户的 profile 级凭据（`<profiles_root>/<name>/.env`）。
///
/// # 为什么需要这个接口（卡面第 4 条「已有 AI 用户的补救」）
///
/// 修复之前的存量账号可能是「能建号、不能干活」的形态（`.env` 里没有 provider key）。
/// 自动批量修是危险动作（可能覆盖运维给某个 AI 员工配的专用 key），所以这里做成
/// **单账号、显式触发**的运维按钮，语义与建号时的那一步完全一致：
///
/// - 缺的键从全局 `<hermes_home>/.env` / 云枢进程环境补；
/// - **已有非空值不覆盖**；
/// - 落盘 600；
/// - 修完立刻自检，把「补了哪些键 / 现在能不能认证」如实回给前端。
///
/// # 返回值
///
/// [`AiUserCredentialRes`]：`repaired_keys` 空数组 = 本来就齐（幂等，可反复点）。
///
/// # 异常情况
///
/// - 403 + `63204`：AI 账号不能给自己修凭据（AI 不管理 AI）；
/// - 404 + `64001`：该用户不是 AI 用户 / 已删除；
/// - 404 + `64004`：库里有行但 profile 目录已丢（先重建 profile）；
/// - 500 + `64014`：全局凭据与进程环境都找不到当前 provider 的 key（**不会**伪造成功）；
/// - 500 + `64008`：`.env` 读写失败。
#[alun::permission(
    path = "/system/user/ai/{user_id}/repair",
    method = "POST",
    permission = "system:user:update"
)]
#[alun::post("/system/user/ai/{user_id}/repair")]
pub async fn repair_credentials(
    Path(user_id): Path<String>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<AiUserCredentialRes> {
    forbid_robot(&claims).await?;
    let id = utils::require_uuid(&user_id, "user_id")?;
    let hermes = Hermes::from_env();
    let row = model::find_by_user(&id)
        .await
        .map_err(|e| utils::db_error(e, "查询 AI 用户"))?
        .ok_or_else(|| ai_not_found(&id))?;

    let sync = hermes.ensure_profile_env(&row.hermes_profile).await?;
    let check = hermes
        .verify_profile_credentials(&row.hermes_profile)
        .await?;
    tracing::info!(
        user_id = %id,
        profile = %row.hermes_profile,
        repaired_keys = ?sync.written_keys,
        provider = ?check.provider,
        credentials_ok = check.ok,
        mode = ?check.env_file_mode,
        "AI 员工凭据修复完成"
    );
    Ok(Res::ok(AiUserCredentialRes {
        user_id: id.to_string(),
        hermes_profile: row.hermes_profile.clone(),
        env_path: check.env_path.clone(),
        env_file_mode: check.env_file_mode,
        provider: check.provider.clone(),
        required_key: check.required_key.clone(),
        present_keys: check.present_keys.clone(),
        credentials_ok: check.ok,
        repaired_keys: sync.written_keys,
        detail: check.detail.clone(),
    }))
}

// ==================== 删除 ====================

/// 删除 AI 用户。
///
/// # 顺序（卡面验收 4）
///
/// 1. 读行（不存在 → 404）；
/// 2. **先备份 SOUL.md** 到 `<backup_dir>/<profile>-<时间戳>.md`（磁盘上没有 SOUL.md 时跳过，
///    备份路径回 `null`）；
/// 3. `hermes profile delete <name> -y`（失败即中止，此时库里与磁盘上都还是原样）；
/// 4. 删 workspace（幂等）；
/// 5. 事务：软删 `sys_ai_agent` + 软删 `sys_user` + 清角色绑定。
///
/// # 返回值
///
/// [`AiUserDeleteRes`]：备份路径 + 两项「已确认消失」标志（都是**查过**而不是猜的）。
///
/// # 异常情况
///
/// - 404 + `64001`：该用户不是 AI 用户 / 已删除；
/// - 500 + `64005`：profile 删除失败（库未动，可重试）。
#[alun::permission(
    path = "/system/user/ai/{user_id}",
    method = "DELETE",
    permission = "system:user:delete"
)]
#[alun::delete("/system/user/ai/{user_id}")]
pub async fn delete(
    Path(user_id): Path<String>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    _headers: HeaderMap,
) -> ResResult<AiUserDeleteRes> {
    // 红线（T5）：AI 用户不许删自己 / 他人的 profile
    forbid_robot(&claims).await?;
    let id = utils::require_uuid(&user_id, "user_id")?;
    let hermes = Hermes::from_env();
    let row = model::find_by_user(&id)
        .await
        .map_err(|e| utils::db_error(e, "查询 AI 用户"))?
        .ok_or_else(|| ai_not_found(&id))?;

    // ② 备份（SOUL.md 可能已被外部删掉 —— 那也没关系，备份路径回 null 并在日志留痕）
    let backup_path = match hermes.read_soul(&row.hermes_profile).await {
        Ok(content) => Some(
            hermes
                .backup_soul(&row.hermes_profile, &content)
                .await?
                .to_string_lossy()
                .to_string(),
        ),
        Err(e) if e.status == 404 => {
            tracing::warn!(
                profile = %row.hermes_profile,
                "删除 AI 用户时 SOUL.md 已不存在，跳过备份"
            );
            None
        }
        Err(e) => return Err(e),
    };

    // ③ 删 profile（失败即中止：库里仍是「有效 AI 用户」，运维可重试）
    hermes.delete_profile(&row.hermes_profile).await?;
    let profile_removed = !hermes
        .profile_dir_exists(&row.hermes_profile)
        .await
        .unwrap_or(true);

    // ④ 删 workspace（幂等；失败只记日志，不阻断软删 —— 目录残留不影响账号语义）
    let workspace_removed = match hermes.remove_workspace(&row.hermes_profile).await {
        Ok(()) => !hermes
            .workspace_path_exists(&row.hermes_profile)
            .await
            .unwrap_or(false),
        Err(e) => {
            tracing::warn!(
                profile = %row.hermes_profile,
                error = %e.msg,
                "删除 workspace 失败（继续软删账号）"
            );
            false
        }
    };

    // ⑤ 软删（**同事务**：配置行 + 账号 + 角色绑定 —— 不变式的删侧保证）
    soft_delete_records(&id)
        .await
        .map_err(|e| utils::db_error(e, "软删 AI 用户（sys_ai_agent + sys_user 同事务）"))?;

    tracing::info!(
        user_id = %id,
        profile = %row.hermes_profile,
        operator = %claims.sub,
        backup = ?backup_path,
        "AI 用户已删除（profile 与 workspace 已清理，账号已软删）"
    );
    Ok(Res::ok(AiUserDeleteRes {
        user_id: id.to_string(),
        hermes_profile: row.hermes_profile,
        soul_backup_path: backup_path,
        profile_removed,
        workspace_removed,
    }))
}

// ==================== 私有辅助 ====================

/// 断言调用者不是 AI 用户（批次 C+ · T5：`63204`）。
///
/// # 为什么 AI 用户不能碰这些接口
///
/// 建号 / 改 SOUL.md / 删号决定的是「AI 的行为准则与生命周期」。AI 若能改自己的
/// SOUL.md，就能改自己的行为约束；若能改别人的 profile，就能冒充别的执行者。
/// 因此这三个写接口**只允许人类管理员**调用 —— 即使 AI 账号被授予了
/// `system:user:create/update/delete` 权限码，也照样拒绝。
///
/// 读接口（详情 / 探活）不拦：AI 需要知道自己在系统里的配置。
///
/// # 异常情况
///
/// - 403 + `63204`：调用者是 AI 用户；
/// - 401 + `20004`：令牌里没有可用的用户 ID（`sub` 不是 UUID）；
/// - 500：查账号类型时数据库异常。
async fn forbid_robot(claims: &TokenClaims) -> std::result::Result<(), ApiError> {
    let operator_id = utils::current_user_uuid(claims).ok_or_else(|| {
        ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "令牌缺少有效的用户标识",
        )
    })?;
    let user_type = user_model::find_user_type(&operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询操作者账号类型"))?;
    match ai_actor::aiagent_admin_forbidden(user_type.as_deref()) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// 补偿（回滚）未成功时的错误包装：保留原始错误语义，把残留项逐条塞进消息。
async fn compensate(
    hermes: &Hermes,
    user_id: &Uuid,
    profile: &str,
    profile_created: bool,
    workspace_created: bool,
    cause: ApiError,
) -> ApiError {
    let failures = rollback(hermes, user_id, profile, profile_created, workspace_created).await;
    if failures.is_empty() {
        tracing::warn!(
            user_id = %user_id,
            profile = %profile,
            cause = %cause.msg,
            "AI 用户创建失败，已完整回滚（无残留）"
        );
        return cause;
    }
    tracing::error!(
        user_id = %user_id,
        profile = %profile,
        cause = %cause.msg,
        leftovers = ?failures,
        "AI 用户创建失败且回滚未完全成功"
    );
    ApiError::new(
        500,
        error_code::AI_AGENT_ROLLBACK_FAILED,
        format!(
            "创建 AI 用户失败：{}；回滚未完成（需人工清理）：{}",
            cause.msg,
            failures.join("；")
        ),
    )
}

/// 逆序回滚：删 profile → 删 workspace → **同事务物理删除** `sys_ai_agent` + `sys_user` + 角色绑定。
///
/// 返回**失败项**列表（空 = 干净）。
///
/// # 为什么以「磁盘事实」为准，而不是只看调用方的标志位
///
/// `create_profile` 是「CLI 建目录 + 写 config.yaml + 补凭据」的复合动作：**CLI 已经建出目录、
/// 后面的步骤失败**时它返回 `Err`，而调用方那一侧的 `profile_created` 还是 `false`
/// —— 只看标志位就会漏删，磁盘上留下一个不完整的 profile（2026-10-29 实测踩到，
/// 正是卡面验收 6「磁盘零残留」挡下的那条）。所以这里先探一次磁盘，标志位与磁盘任一为真就删。
///
/// # 为什么回滚用物理删除（而删号用软删）
///
/// 删号是**业务动作**，软删留痕才有价值（谁在什么时候删了哪个 AI 员工）。
/// 回滚是**事务补偿**：这个账号从未对任何用户可见过，软删只会给 `sys_user` 留一条
/// 「指向不存在的账号」的垃圾行；卡面要求「库内零残留」，物理删除才是那个语义。
/// 角色绑定（`sys_user_role`）一并清掉，`sys_ai_agent` 有 FK CASCADE 也显式删一次，
/// 不依赖级联的行为差异。
async fn rollback(
    hermes: &Hermes,
    user_id: &Uuid,
    profile: &str,
    profile_created: bool,
    workspace_created: bool,
) -> Vec<String> {
    let mut failures = Vec::new();
    let profile_on_disk = hermes.profile_dir_exists(profile).await.unwrap_or(false);
    if (profile_created || profile_on_disk)
        && let Err(e) = hermes.delete_profile(profile).await
    {
        failures.push(format!("删除 profile 失败（{}）", e.msg));
    }
    let workspace_on_disk = hermes.workspace_path_exists(profile).await.unwrap_or(false);
    if (workspace_created || workspace_on_disk)
        && let Err(e) = hermes.remove_workspace(profile).await
    {
        failures.push(format!("删除 workspace 失败（{}）", e.msg));
    }
    if let Err(e) = hard_delete_records(user_id).await {
        failures.push(format!("物理删除 sys_user / sys_ai_agent 失败（{e}）"));
    }
    failures
}

/// **回滚专用**：物理删除本次请求刚插入的两行 + 角色绑定。
///
/// 只由 [`rollback`] 调用（删号走 [`soft_delete_records`]，语义不同，别混用）。
// SCOPE-EXEMPT: 按主键物理删除**本请求刚插入**的两行（创建失败的事务补偿），
// 不存在「读哪些行」的问题，注入数据范围谓词在语义上是错的（会让补偿漏删、留下
// 一个删不掉的半成品账号）。框架的 `delete_by_pk` 对软删模型只会写 `deleted_at`，
// 拿不到物理删除，所以这里必须手写 SQL。
async fn hard_delete_records(user_id: &Uuid) -> std::result::Result<(), DbError> {
    let id = *user_id;
    db().transaction(|tx| async move {
        SysUserRoleModel::delete_by(&tx, "user_id", (&id,)).await?;
        tx.exec_sql(
            "DELETE FROM sys_ai_agent WHERE user_id = ?".to_string(),
            vec![Value::from(&id)],
        )
        .await?;
        tx.exec_sql(
            "DELETE FROM sys_user WHERE id = ?".to_string(),
            vec![Value::from(&id)],
        )
        .await?;
        Ok(())
    })
    .await
}

/// 同事务软删 `sys_ai_agent` + `sys_user` + 角色绑定（**删号**专用，回滚走 [`hard_delete_records`]）。
///
/// # 为什么必须同事务
///
/// 不变式是双向的：`ROBOT` 账号必须有配置行、配置行必须对应 `ROBOT` 账号。
/// 分两次写的话，中间那一刻库是不合法的；同事务保证外部观察者只看到
/// 「都在」或「都软删」两种状态。
async fn soft_delete_records(user_id: &Uuid) -> std::result::Result<(), DbError> {
    let id = *user_id;
    db().transaction(|tx| async move {
        model::soft_delete(&tx, &id).await?;
        SysUserModel::of(id).delete(&tx).await?;
        SysUserRoleModel::delete_by(&tx, "user_id", (&id,)).await?;
        Ok(())
    })
    .await
}

/// `sys_ai_agent` / `sys_user` 行 → 对外响应。
fn to_res(hermes: &Hermes, row: &AiAgentRow, brief: Option<&AiUserBrief>) -> AiUserRes {
    AiUserRes {
        user_id: row.user_id.to_string(),
        username: brief
            .map(|b| b.username.clone())
            .unwrap_or_else(|| row.hermes_profile.clone()),
        real_name: brief.and_then(|b| b.real_name.clone()).unwrap_or_default(),
        user_type: brief
            .map(|b| b.user_type.clone())
            .unwrap_or_else(|| "ROBOT".to_string()),
        hermes_profile: row.hermes_profile.clone(),
        workspace_path: row.workspace_path.clone(),
        soul_path: hermes
            .soul_path(&row.hermes_profile)
            .to_string_lossy()
            .to_string(),
        agent_description: row.agent_description.clone().unwrap_or_default(),
        soul_md: row.soul_md.clone(),
        model_override: row.model_override.clone(),
        gateway_status: row.gateway_status.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn ai_not_found(user_id: &Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::AI_AGENT_NOT_FOUND,
        format!("AI 用户不存在或已删除：{user_id}"),
    )
}

/// 机器人账号口令：随机 64 hex，哈希后落库。
///
/// 机器人**不登录**（登录闸门属 T5），但 `sys_user.password` 是 NOT NULL，因此必须给一个
/// 没人知道的随机值 —— 绝不能用固定串（那等于给所有机器人开了同一个后门口令）。
fn robot_password_hash() -> std::result::Result<String, ApiError> {
    let raw = format!("{}{}", Uuid::now_v7().simple(), Uuid::now_v7().simple());
    Crypto::hash_password(&raw)
        .map_err(|_| ApiError::new(500, error_code::INTERNAL_ERROR, "机器人账号口令加密失败"))
}

/// 规整模型覆盖：`null` → `None`；其它必须是 JSON 对象（如 `{"model":"deepseek-v4.1-flash"}`）。
fn normalize_model(
    raw: Option<serde_json::Value>,
) -> std::result::Result<Option<serde_json::Value>, ApiError> {
    match raw {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(None),
        Some(value @ serde_json::Value::Object(_)) => Ok(Some(value)),
        Some(other) => Err(ApiError::new(
            400,
            error_code::AI_AGENT_MODEL_INVALID,
            format!("模型覆盖必须是 JSON 对象，收到：{other}"),
        )),
    }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    // 单测里要引用常量：模块别名在此作用域内不会与「局部的 Hermes 实例」冲突
    use crate::business::aiagent::hermes;

    /// 模型覆盖：null / 缺省 → None；对象 → 原样；其它一律 400。
    #[test]
    fn normalize_model_accepts_only_object_or_null() {
        assert!(normalize_model(None).expect("None").is_none());
        assert!(
            normalize_model(Some(serde_json::Value::Null))
                .expect("null")
                .is_none()
        );
        let obj = serde_json::json!({"model": "deepseek-v4.1-flash"});
        assert_eq!(normalize_model(Some(obj.clone())).expect("对象"), Some(obj));
        for bad in [
            serde_json::json!("deepseek"),
            serde_json::json!(1),
            serde_json::json!([1, 2]),
            serde_json::json!(true),
        ] {
            let err = normalize_model(Some(bad.clone())).expect_err("必须拒绝");
            assert_eq!(err.status, 400);
            assert_eq!(err.code, error_code::AI_AGENT_MODEL_INVALID);
        }
    }

    /// 职责规整：多行压成一行、去空白；超长 400。
    #[test]
    fn description_is_flattened_and_bounded() {
        assert_eq!(
            normalize_description(Some("  第一行\n\n  第二行  ")).expect("规整"),
            "第一行 第二行"
        );
        assert_eq!(normalize_description(None).expect("None"), "");
        let long = "职".repeat(hermes::DESCRIPTION_MAX_CHARS + 1);
        let err = normalize_description(Some(&long)).expect_err("必须拒绝");
        assert_eq!(err.code, error_code::AI_AGENT_DESCRIPTION_INVALID);
        // 刚好等于上限必须通过（边界值不误伤）
        let edge = "职".repeat(hermes::DESCRIPTION_MAX_CHARS);
        assert_eq!(
            normalize_description(Some(&edge))
                .expect("边界值")
                .chars()
                .count(),
            hermes::DESCRIPTION_MAX_CHARS
        );
    }

    /// 机器人口令哈希：两次调用不同、且是 Argon2 前缀（不是明文/固定串）。
    #[test]
    fn robot_password_hash_is_random_and_hashed() {
        let a = robot_password_hash().expect("哈希 1");
        let b = robot_password_hash().expect("哈希 2");
        assert_ne!(a, b, "两次生成的口令哈希必须不同");
        assert!(
            a.starts_with("$argon2") || a.starts_with("$2"),
            "应为 Argon2/BCrypt 哈希：{}",
            &a[..a.len().min(12)]
        );
        assert!(a.len() > 40, "哈希长度异常：{}", a.len());
    }
}
