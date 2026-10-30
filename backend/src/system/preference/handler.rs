//! 用户偏好体系的 HTTP 接口。
//!
//! # 接口一览（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/system/auth/...`）
//!
//! | 方法 | 路径 | 免鉴权 | 说明 |
//! |------|------|--------|------|
//! | GET | `/system/auth/preference` | 否 | 当前用户**全量**偏好（字典默认值 + 用户值合并，无记录也返默认、不返 404） |
//! | PUT | `/system/auth/preference` | 否 | 局部更新 + upsert，逐 key 按字典校验，返回更新后的全量偏好 |
//!
//! 路径与前端 `src/api/system/preference-api.ts` 逐字一致（该文件第一版就固化了这两个
//! 路径，本卡只是把「后端尚未上线」的占位注释变成真实接口）。
//!
//! # 这两个接口为什么没有权限码
//!
//! 「读写自己的偏好」是**登录用户的固有权利**，与 `system:preference:view` 这类管理权限
//! 无关：若挂权限码，普通用户改一次配色就要 403。行的归属由 `AuthClaims.sub` 强制，
//! 不存在「读别人偏好」的入口（没有 `{user_id}` 路径参数，body 里的 `user_id` 也不落库）。
//!
//! # 越权防线（三层）
//!
//! 1. `dto::UpdatePreferenceReq` 里**没有** `user_id` 字段（body 里带了也进不来）；
//! 2. 用户 ID 只从 `Extension(AuthClaims(claims))` 的 `sub` 取，且 `require_uuid` 校验形态；
//! 3. SQL 一律 `WHERE user_id = ?` / `INSERT ... VALUES (?, ...)`，参数就是上一步那个 ID。
//!
//! # 审计
//!
//! 偏好变更**不写** `audit_logs`：它是「界面观感」类数据，量级高（每切一次配色一条）、
//! 无安全语义，写审计会把真正的敏感操作淹掉。运维排查靠 `sys_user_preference.updated_at`
//! 与 `tracing` 日志（本文件在成功路径上记一条 `info`）。

use axum::Json;
use nik::prelude::*;

use crate::system::preference::dto::{PreferenceRes, UpdatePreferenceReq};
use crate::system::preference::model;
use crate::utils;

/// 读取当前登录用户的全量偏好。
///
/// # 返回值
///
/// 合并默认值后的全量偏好（`items` 覆盖字典里每个启用键）。**永不返回 404**：
/// 一次都没设置过的用户拿到的是「全默认」，前端因此不需要区分「没设置」与「设置成默认」。
///
/// # 异常情况
///
/// - 401：令牌里的 `sub` 不是合法 UUID（认证层已保证，防御性保留）；
/// - 500：字典默认值配置损坏（`62024`）或数据库异常。
///
/// # 日志
///
/// 正常读取不写业务日志（每次进设置页都会打一次，写了就是噪声）。
#[nik::get("/system/auth/preference")]
pub async fn get_preference(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<PreferenceRes> {
    let user_id = utils::require_uuid(&claims.sub, "sub")?;
    let defs = model::load_defs(db())
        .await
        .map_err(|e| utils::db_error(e, "查询偏好字典"))?;
    let stored = model::load_values(db(), &user_id)
        .await
        .map_err(|e| utils::db_error(e, "查询用户偏好"))?;
    Ok(Res::ok(model::assemble(&defs, &stored)?))
}

/// 局部更新当前登录用户的偏好（upsert）。
///
/// # 语义
///
/// - **局部**：只写请求里出现的 key，没出现的 key 原样保留（不会因为前端只发 palette
///   就把明暗模式重置成默认值）；
/// - **幂等**：同一个值重复 PUT 不会产生第二行（`ON CONFLICT ... DO UPDATE`）；
/// - **事务**：多键更新在同一个事务里，任一键失败则整批不落库。
///
/// # 参数
///
/// - `claims`：认证中间件注入的令牌声明（`sub` = 用户 ID，唯一来源）；
/// - `req`：两代形态的偏好补丁（见 [`UpdatePreferenceReq`]）。
///
/// # 返回值
///
/// 更新后的**全量**偏好（与 GET 同形）。前端可以据此直接校正本地缓存，不用再发一次 GET。
///
/// # 异常情况
///
/// - 400 `62023`：请求体里一个偏好项都没有；
/// - 400 `62020`：键不在字典里（或已被停用）；
/// - 400 `62021`：值类型不符（如布尔字段塞字符串）；
/// - 400 `62022`：值域不符（oneof / range）；
/// - 500 `62024`：字典自身配置损坏；
/// - 500 `10004`：数据库异常。
#[nik::put("/system/auth/preference")]
pub async fn update_preference(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<UpdatePreferenceReq>,
) -> ResResult<PreferenceRes> {
    let user_id = utils::require_uuid(&claims.sub, "sub")?;
    let patch = req.into_patch()?;

    let defs = model::load_defs(db())
        .await
        .map_err(|e| utils::db_error(e, "查询偏好字典"))?;
    // 先校验后写库：校验失败时一行都不写（不留半套偏好）
    model::validate_patch(&defs, &patch)?;

    let keys: Vec<String> = patch.iter().map(|(key, _)| key.clone()).collect();
    // 事务闭包必须 `move` 且只捕获拥有所有权的数据：`patch` 在闭包外还要用，
    // 所以另外 clone 一份给事务（键值对只有几个，拷贝成本可忽略）。
    let patch_in_tx = patch.clone();
    db().transaction(|tx| async move {
        model::upsert_values(&tx, &user_id, &patch_in_tx).await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "保存用户偏好"))?;

    let stored = model::load_values(db(), &user_id)
        .await
        .map_err(|e| utils::db_error(e, "回读用户偏好"))?;
    tracing::info!(user_id = %user_id, keys = ?keys, "用户偏好已更新");
    Ok(Res::ok(model::assemble(&defs, &stored)?))
}
