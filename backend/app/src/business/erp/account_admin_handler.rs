//! 会计科目**唯一写入口**（M1-03 · 迁移 `V0408__account_foreign_currency`）：
//! 科目「是否外币核算」标记的维护面。**一个接口**。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | PUT | `/business/erp/account/{id}/foreign-currency` | `business:erp:account:update` | 设置 / 取消「外币核算」（只有末级科目可开启） |
//!
//! # 为什么科目页是只读的，却要开这一个写口
//!
//! [`crate::business::erp::handler`] 里的 8 张台账是「只读：科目由系统管理员维护」（`account_page`
//! / `account_detail` 只有 GET，科目本身由迁移 + 通用导入种）。那条纪律针对的是**科目结构**
//! （编码 / 名称 / 层级 / 启停）—— 改错一次会让历史凭证的分录快照指向不同科目。
//!
//! 「是否外币核算」不是结构，是**核算属性**：
//!   * 改它不动任何分录、不动余额、不影响已生成凭证（凭证的币种与汇率在 M1-04 建单时就锁进单据行）；
//!   * 它决定的是**未来行为**：M1-05 期末调汇要不要给这个科目出调汇凭证、M1-07 外币余额表要不要列它。
//!
//! 所以本卡把「一个布尔位的维护」单独放成窄接口，而**不是**顺势把科目 CRUD 一起开出来
//! （那会越过只读纪律，也不在本卡范围）。
//!
//! # 三条入口校验（DDL 的 CHECK 只是兜底）
//!
//! 1. 科目必须存在且未软删 → 否则 `404 + 10002`（含已软删）。
//! 2. **只有末级科目可以开启**（`is_leaf = 1`）→ 否则 `400 + 66080`
//!    （DB 侧同规则：`ck_biz_account_fc_leaf`）。理由见 [`crate::shared::error_code::ERP_ACCOUNT_FC_NOT_LEAF`]：
//!    非末级科目自己那一行没有余额，标上会让 M1-05 按「标了外币的科目」扫出金额恒 0 的调汇凭证，
//!    而下级若也标了就变成「同一笔外币余额调两次」。**关闭永远允许**（把标记撤回来不需要理由）。
//! 3. 值相同 → **幂等直接返回当前行**（不写库、不刷 `updated_at`），连点两次开关不会互踢。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_account 是全局主数据（V0137 无 dept_id / 归属人列），数据范围表达不出来；
//! 可见性由 business:erp:account:{list,update} 权限码控制。
//! ```
//!
//! # 不碰的东西
//!
//! 科目不是单据：不生成凭证、不写库存三本账、不进流程引擎、不注册 `DocKind` / `LEGAL_EDGES`。
//! 本文件也不做「切换币种」——科目的 `currency_code`（M1-01 / V0406）由导入 / 运维维护，
//! 本卡只动 `is_foreign_currency` 一位。

use axum::Json;
use axum::extract::{Extension, Path};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::dto::{AccountForeignCurrencyReq, AccountRes};
use crate::business::erp::model;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

/// 科目「是否外币核算」开关权限码（与 `V0408` 的 `sys_menu.auth_code` 逐字一致）。
pub const PERM_ACCOUNT_UPDATE: &str = "business:erp:account:update";

/// 本模块的权限码清单（供 `handler::all_permissions` 汇总 / 自检；只读列表码在 `handler`）。
pub const PERMISSIONS: [&str; 1] = [PERM_ACCOUNT_UPDATE];

// ==================== 共用辅助（与 `currency_admin_handler` 同形，模块自足） ====================

/// 入参不合法统一 `400 + 10001`（参数形状层面）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 资源不存在统一 `404 + 10002`。
fn not_found(what: &str) -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, format!("{what}不存在或已删除"))
}

/// 路径参数 / 主键非法（含空串）统一 `400 + 66006`（与 `handler::parse_uuid` 同口径）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不能为空"),
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        )
    })
}

// ==================== 接口 ====================

/// 设置科目「是否外币核算」。
///
/// 幂等：值相同直接返回当前行（不写库）。唯一拒绝开启的情况是「科目不是末级」→ `400 + 66080`。
#[nik::permission(
    path = "/business/erp/account/{id}/foreign-currency",
    method = "PUT",
    permission = "business:erp:account:update"
)]
#[nik::put("/business/erp/account/{id}/foreign-currency")]
pub async fn account_set_foreign_currency_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<AccountForeignCurrencyReq>,
) -> ResResult<AccountRes> {
    let id = parse_uuid(&id, "id")?;
    let current = model::account_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待标记外币核算的科目"))?
        .ok_or_else(|| not_found("会计科目"))?;

    let current_flag = current.is_foreign_currency == 1;
    if current_flag == req.is_foreign_currency {
        // 幂等：不写库、不刷 updated_at，直接回当前行（连点两次开关不互踢）。
        return Ok(Res::ok(AccountRes::from(&current)));
    }

    if req.is_foreign_currency && current.is_leaf != 1 {
        return Err(ApiError::new(
            400,
            error_code::ERP_ACCOUNT_FC_NOT_LEAF,
            format!(
                "科目 {} {} 不是末级科目，不能标记外币核算：外币核算按末级科目挂分录、期末调汇，\
                 汇总科目由下级汇总（标注末级明细科目，或先给当前科目建下级科目）",
                current.account_code, current.account_name
            ),
        ));
    }

    let operator = utils::current_user_uuid(&claims);
    let affected = model::account_set_foreign_currency(id, req.is_foreign_currency, operator)
        .await
        .map_err(|error| utils::db_error(error, "设置科目外币核算标记"))?;

    // 预读之后被人抢改（改成非末级 / 软删 / 已改成同一个值）都会让 UPDATE 落空：
    // 回读一次给「真实状态」，而不是把一次空写当成功。
    let saved = model::account_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读科目外币核算标记"))?
        .ok_or_else(|| not_found("会计科目"))?;
    let saved_flag = saved.is_foreign_currency == 1;
    if saved_flag != req.is_foreign_currency {
        return Err(bad_request(format!(
            "科目 {} 的外币核算标记未能写入（可能刚被他人改动，请刷新后重试）",
            saved.account_code
        )));
    }
    if affected == 0 {
        // 到这儿说明值已经是目标值但 UPDATE 报告 0 行 —— 只可能是并发下别人先写成了同一个值。
        // 结果与请求一致，按幂等成功返回（`saved_flag == req` 上面已核对过）。
        return Ok(Res::ok(AccountRes::from(&saved)));
    }
    Ok(Res::ok(AccountRes::from(&saved)))
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::dto::AccountForeignCurrencyReq;

    /// ① 权限码：前缀正确、只有一个码、且与迁移里的 `auth_code` 逐字一致。
    #[test]
    fn permissions_are_well_formed() {
        assert_eq!(PERMISSIONS.len(), 1, "M1-03 只开一个权限码");
        assert_eq!(
            PERM_ACCOUNT_UPDATE, "business:erp:account:update",
            "码值必须与 V0408 的 sys_menu.auth_code 逐字一致（改一处就授不出去）"
        );
        assert!(
            PERM_ACCOUNT_UPDATE.starts_with("business:erp:account:"),
            "码必须落在 business:erp:account: 前缀下"
        );
    }

    /// ② 主键解析：空串 / 非 UUID 都是 `400 + 66006`，不静默落库。
    #[test]
    fn parse_uuid_rejects_garbage() {
        let empty = parse_uuid("  ", "id").expect_err("空串必须被拒");
        assert_eq!(empty.status, 400);
        assert_eq!(empty.code, error_code::ERP_FILTER_INVALID);

        let garbage = parse_uuid("not-a-uuid", "id").expect_err("非 UUID 必须被拒");
        assert_eq!(garbage.status, 400);
        assert_eq!(garbage.code, error_code::ERP_FILTER_INVALID);

        let ok = parse_uuid("018f4b2c-1111-7222-8333-444455556666", "id").expect("合法 UUID");
        assert_eq!(ok.to_string(), "018f4b2c-1111-7222-8333-444455556666");
    }

    /// ③ 入参契约：`is_foreign_currency` **必填**（缺字段报错，不静默当 false —— 静默 false
    /// 会把已标记的外币科目悄悄关掉，M1-05 少一张调汇凭证且没有任何报错）。
    #[test]
    fn request_body_requires_the_flag() {
        let ok: AccountForeignCurrencyReq =
            serde_json::from_str(r#"{"is_foreign_currency": true}"#).expect("带字段应可反序列化");
        assert!(ok.is_foreign_currency);

        let off: AccountForeignCurrencyReq =
            serde_json::from_str(r#"{"is_foreign_currency": false}"#).expect("false 也应可反序列化");
        assert!(!off.is_foreign_currency);

        let missing = serde_json::from_str::<AccountForeignCurrencyReq>("{}");
        assert!(missing.is_err(), "缺 is_foreign_currency 必须报错，不允许默认 false");
    }

    /// ④ 错误码 66080 的语义口径：400（业务规则拒绝，不是 500 也不是 404）。
    #[test]
    fn not_leaf_error_is_400_66080() {
        assert_eq!(error_code::ERP_ACCOUNT_FC_NOT_LEAF, 66080, "M1-03 在 66xxx 段续号");
        let err = ApiError::new(400, error_code::ERP_ACCOUNT_FC_NOT_LEAF, "x");
        assert_eq!(err.status, 400);
    }
}
