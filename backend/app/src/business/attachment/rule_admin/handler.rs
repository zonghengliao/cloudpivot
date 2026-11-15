//! 附件必传规则管理接口（**ATT-09**）：列表 / 下拉 / 新建 / 编辑 / 启停 / 软删。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/support/attachment-rule/list` | `support:attachment-rule:list` | 全部规则（含已禁用，不含已软删） |
//! | GET | `/support/attachment-rule/options` | `support:attachment-rule:list` | 下拉数据源：`doc_type` / `required_at` / `file_role` |
//! | POST | `/support/attachment-rule` | `support:attachment-rule:create` | 新建 |
//! | PUT | `/support/attachment-rule/{id}` | `support:attachment-rule:update` | 全量编辑 |
//! | PUT | `/support/attachment-rule/{id}/enabled` | `support:attachment-rule:update` | 启用 / 禁用（**即时生效**） |
//! | DELETE | `/support/attachment-rule/{id}` | `support:attachment-rule:delete` | 软删 |
//!
//! # 两层鉴权：路由码 + 超管判定
//!
//! 每个接口都先过 `#[nik::permission]`（路由码），再调 [`dto::ensure_super_admin`]。
//! 顺序不能反：路由码由中间件在最外层拦（无 claims 直接 401/403），超管判定在 handler
//! 里做（能拿到 `username` 写审计、能给出「需要 SUPER_ADMIN」的具体文案）。
//! 卡面验收⑤「非超管账号打开 → 403」走的是第一层（`support:attachment-rule:list`
//! 只授予 SUPER_ADMIN）；两层都过不了才是彻底打不进。
//!
//! # 错误出口
//!
//! 本文件是 handler 层，统一 `ResResult<T>`；校验失败由 [`dto::validate`] 给 400，
//! 唯一键冲突由 [`model::write_error`] 给 409，数据库其它错误直接 `?`。
//! **不写** `.map_err(ApiError::internal(..))`（框架已有 `DbError → ApiError`）。
//!
//! # 审计
//!
//! 四个写接口都落 `audit_logs`（`ATTACHMENT_RULE_CREATE` / `_UPDATE` / `_TOGGLE` /
//! `_DELETE`），`before_data` / `after_data` 记录**规则全文**（不含文件内容，无敏感信息）——
//! 「谁把哪条必传规则关掉了」必须可回溯，这正是本页面的内控价值。

use axum::extract::{ConnectInfo, Json, Path};
use axum::http::HeaderMap;
use nik::prelude::*;

use std::net::SocketAddr;

use crate::shared::error_code;
use crate::support::log;
use crate::utils;

use super::dto::{self, RuleDeleteRes, RuleItemRes, RuleListRes, RuleOptionsRes, RuleSaveReq, RuleToggleReq};
use super::model;

// ==================== 接口 ====================

/// 规则列表（**含已禁用**，不含已软删）。
///
/// # 参数
///
/// - `claims`：当前用户声明（超管判定）。
///
/// # 返回值
///
/// [`RuleListRes`]：`list` 按「单据 → 动作 → 角色」稳定排序，`total` 为条数。
///
/// # 异常情况
///
/// - 403：非超管；
/// - 500：数据库异常（不返回空列表 —— 那会让人以为规则丢了）。
#[nik::permission(
    path = "/support/attachment-rule/list",
    method = "GET",
    permission = "support:attachment-rule:list"
)]
#[nik::get("/support/attachment-rule/list")]
pub async fn list(Extension(AuthClaims(claims)): Extension<AuthClaims>) -> ResResult<RuleListRes> {
    dto::ensure_super_admin(&claims, "查看")?;
    let rows = model::list_all().await?;
    let list = rows.iter().map(dto::to_item).collect::<Vec<_>>();
    Ok(Res::ok(RuleListRes {
        total: list.len() as u64,
        list,
    }))
}

/// 下拉数据源：`doc_type` / `required_at` / `file_role`。
///
/// # 为什么下拉也要后端给
///
/// 三份词表在 Rust 侧各有一个唯一真相（[`DocKind::ALL`] / [`RequiredAt::ALL`] /
/// [`FileRole::GROUPS`]）。前端自己抄一份就会出现「界面能选、后端 400」的漂移
/// （ATT-06 就因为 `ATTACHMENT_DOC_KINDS` 少 5 个值踩过一次）。
///
/// # 异常情况
///
/// 403：非超管（页面都打不开，下拉数据也不该单独漏出去）。
#[nik::permission(
    path = "/support/attachment-rule/options",
    method = "GET",
    permission = "support:attachment-rule:list"
)]
#[nik::get("/support/attachment-rule/options")]
pub async fn options(Extension(AuthClaims(claims)): Extension<AuthClaims>) -> ResResult<RuleOptionsRes> {
    dto::ensure_super_admin(&claims, "查看")?;
    Ok(Res::ok(dto::options()))
}

/// 新建规则。
///
/// # 参数
///
/// - `headers` / `connect_info`：审计日志的 IP / UA / request_id 来源；
/// - `claims`：当前用户声明（超管判定 + 审计留痕）；
/// - `req`：规则全文（[`RuleSaveReq`]）。
///
/// # 返回值
///
/// 新建后的规则（[`RuleItemRes`]，带 `requirement` 文案）。
///
/// # 异常情况
///
/// - 400：任何一项不符合 DDL 约束或会让规则永不成立（`dto::validate` 点名哪一项）；
/// - 403：非超管；
/// - 409：同 `(doc_type, required_at, file_role, condition_sql)` 已有存活规则；
/// - 500：数据库异常。
#[nik::permission(
    path = "/support/attachment-rule",
    method = "POST",
    permission = "support:attachment-rule:create"
)]
#[nik::post("/support/attachment-rule")]
pub async fn create(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<RuleSaveReq>,
) -> ResResult<RuleItemRes> {
    dto::ensure_super_admin(&claims, "新建")?;
    let draft = dto::validate(&req)?;

    // 先查唯一键：并发窗口内仍可能撞索引，insert 侧还有一层 UniqueViolation → 409
    if let Some(existing) = model::find_conflict(&draft, None).await? {
        return Err(dto::rule_key_conflict(&draft, &existing));
    }

    let id = model::insert(&claims, &draft)
        .await
        .map_err(|error| model::write_error(error, "新建", || dto::rule_key_conflict(&draft, &id_unknown())))?;
    let Some(row) = model::find(&id).await? else {
        // 刚插进去的行读不回来只可能是并发软删：如实报 500，不返回半成品
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            "新建附件规则后回读失败（可能被并发删除），请刷新列表确认",
        ));
    };
    let item = dto::to_item(&row);
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "ATTACHMENT_RULE_CREATE",
            "AttachmentRule",
            id.to_string(),
            None,
            Some(serde_json::to_value(&item).unwrap_or(serde_json::Value::Null)),
        ),
    );
    Ok(Res::ok(item))
}

/// 全量编辑规则（**可空字段传 `null` = 置空**）。
///
/// # 参数
///
/// - `id`：规则主键；
/// - `headers` / `connect_info` / `claims`：审计与权限；
/// - `req`：规则全文（PUT 语义，见 [`RuleSaveReq`] 文件注释）。
///
/// # 返回值
///
/// 编辑后的规则（[`RuleItemRes`]）。
///
/// # 异常情况
///
/// - 400：`id` 非法 / 校验不过；
/// - 403：非超管；
/// - 404：规则不存在或已删除；
/// - 409：改成了与另一条存活规则相同的键；
/// - 500：数据库异常。
#[nik::permission(
    path = "/support/attachment-rule/{id}",
    method = "PUT",
    permission = "support:attachment-rule:update"
)]
#[nik::put("/support/attachment-rule/{id}")]
pub async fn update(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<RuleSaveReq>,
) -> ResResult<RuleItemRes> {
    dto::ensure_super_admin(&claims, "编辑")?;
    let id = utils::require_uuid(&id, "id")?;
    let before = model::find(&id).await?.ok_or_else(|| dto::rule_not_found(&id))?;
    let draft = dto::validate(&req)?;

    if let Some(existing) = model::find_conflict(&draft, Some(&id)).await? {
        return Err(dto::rule_key_conflict(&draft, &existing));
    }

    let affected = model::update(&claims, &id, &draft)
        .await
        .map_err(|error| model::write_error(error, "编辑", || dto::rule_key_conflict(&draft, &id)))?;
    if affected == 0 {
        // 校验与 UPDATE 之间被并发软删
        return Err(dto::rule_not_found(&id));
    }
    let Some(row) = model::find(&id).await? else {
        return Err(dto::rule_not_found(&id));
    };
    let item = dto::to_item(&row);
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "ATTACHMENT_RULE_UPDATE",
            "AttachmentRule",
            id.to_string(),
            Some(serde_json::to_value(dto::to_item(&before)).unwrap_or(serde_json::Value::Null)),
            Some(serde_json::to_value(&item).unwrap_or(serde_json::Value::Null)),
        ),
    );
    Ok(Res::ok(item))
}

/// 启用 / 禁用（**即时生效**，不重启）。
///
/// # 只改 `enabled`
///
/// 启停是高频操作（临时放行某类单据），整行回传会让两次并发编辑互相覆盖
/// （后写的把前者的 `allow_exts` 改回去）。因此本接口只收 `{ enabled }`。
///
/// # 生效时机
///
/// 闸门 [`super::super::rule::load_rules`] 每次校验都查表（无缓存），本接口
/// 提交后的**下一次** submit / confirm / activate 立即按新状态走。
///
/// # 异常情况
///
/// - 400：`id` 非法；403：非超管；404：规则不存在；500：数据库异常。
#[nik::permission(
    path = "/support/attachment-rule/{id}/enabled",
    method = "PUT",
    permission = "support:attachment-rule:update"
)]
#[nik::put("/support/attachment-rule/{id}/enabled")]
pub async fn set_enabled(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<RuleToggleReq>,
) -> ResResult<RuleItemRes> {
    dto::ensure_super_admin(&claims, if req.enabled { "启用" } else { "禁用" })?;
    let id = utils::require_uuid(&id, "id")?;
    let before = model::find(&id).await?.ok_or_else(|| dto::rule_not_found(&id))?;

    let affected = model::set_enabled(&claims, &id, req.enabled).await?;
    if affected == 0 {
        return Err(dto::rule_not_found(&id));
    }
    let Some(row) = model::find(&id).await? else {
        return Err(dto::rule_not_found(&id));
    };
    let item = dto::to_item(&row);
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "ATTACHMENT_RULE_TOGGLE",
            "AttachmentRule",
            id.to_string(),
            Some(serde_json::json!({ "enabled": before.enabled })),
            Some(serde_json::json!({ "enabled": item.enabled })),
        ),
    );
    Ok(Res::ok(item))
}

/// 软删规则（`deleted_at` 置位；不物理删）。
///
/// # 幂等
///
/// 已删 / 不存在的规则返回 404（不是 500）：管理员点第二次删除时看到「已删除」
/// 比看到「服务器错误」有用。真正的幂等语义由 `deleted = false` 表达。
///
/// # 异常情况
///
/// - 400：`id` 非法；403：非超管；404：规则不存在；500：数据库异常。
#[nik::permission(
    path = "/support/attachment-rule/{id}",
    method = "DELETE",
    permission = "support:attachment-rule:delete"
)]
#[nik::delete("/support/attachment-rule/{id}")]
pub async fn delete(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<RuleDeleteRes> {
    dto::ensure_super_admin(&claims, "删除")?;
    let id = utils::require_uuid(&id, "id")?;
    let before = model::find(&id).await?.ok_or_else(|| dto::rule_not_found(&id))?;

    let affected = model::soft_delete(&claims, &id).await?;
    if affected == 0 {
        return Err(dto::rule_not_found(&id));
    }
    let item = dto::to_item(&before);
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "ATTACHMENT_RULE_DELETE",
            "AttachmentRule",
            id.to_string(),
            Some(serde_json::to_value(&item).unwrap_or(serde_json::Value::Null)),
            None,
        ),
    );
    Ok(Res::ok(RuleDeleteRes {
        id: id.to_string(),
        deleted: true,
    }))
}

// ==================== 辅助 ====================

/// 触发器兜底冲突文案用的「未知已有规则」主键。
///
/// 只在 `insert` 撞唯一索引时用到：那一刻我们**不知道**是哪一条占了键
/// （唯一索引报错不带对方 id）。给一个全零 UUID 让 409 文案仍然读得通，
/// 真正的定位靠提示里的四元组（单据 / 动作 / 角色 / 条件）去列表里搜。
fn id_unknown() -> nik::uuid::Uuid {
    nik::uuid::Uuid::nil()
}
