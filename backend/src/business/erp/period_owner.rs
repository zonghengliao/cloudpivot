//! 结账挡路单据的**处理人反查**（PERIOD-CLOSE）——「下一步该谁处理」。
//!
//! # 规则只有一条：看权限，不看人
//!
//! 一张单挡路了，下一步该谁做，取决于**那个动作需要什么权限码**，而不是「谁造的单」：
//!
//! ```text
//! 动作「去过账」 → business:erp:voucher:create / business:erp:stock-in:status
//! 动作「去审批」 → business:erp:purchase-order:status
//! 动作「去收货」 → business:erp:stock-in:create
//!        ↓ 反查
//! sys_menu.auth_code → sys_role_menu → sys_role（启用、未删，按 sort_order + role_code 取第一个）
//!        ↓ 再看这个角色下有几个人
//! 恰好 1 个启用用户 → owner_name 填上（能点名催）
//! 0 个 / 多个       → 只给角色（催办发给该角色全体）
//! 没有角色持有该权限 → 两个都留空（前端显示「待分配」，**不猜**）
//! ```
//!
//! **制单人只作参考**：`business:erp:stock-in:create`（建单）与
//! `business:erp:stock-in:status`（过账）是两个码，造单的人不一定有权过账。
//!
//! # 为什么单独一个文件
//!
//! 「挡路的是哪几张单」（[`crate::business::erp::period_items`]）与「该谁处理」（本文件）
//! 是两件可以独立演进的事：将来接上「流程引擎的动态审批人」时，只改本文件的反查实现，
//! 明细取数一行不用动。两边都保持在 1000 行以内（TD 系列的文件体量纪律）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: sys_role / sys_role_menu / sys_menu / sys_user_role / sys_user 是 RBAC 配置表，
//! 与业务数据范围无关；这里只读「谁拥有某权限」，不读任何业务单据。
//! ```

use nik::prelude::*;
use nik::uuid::Uuid;
use std::collections::HashMap;
use std::result::Result;

use crate::utils;

// ==================== 「下一个动作」的权限码 ====================

/// 凭证过账（本仓没有「过账」独立权限码，过账走凭证录入口）。
pub const PERM_VOUCHER_POST: &str = "business:erp:voucher:create";
/// 采购单提交审批。
pub const PERM_PO_SUBMIT: &str = "business:erp:purchase-order:submit-approval";
/// 采购单状态流转（含 `SUBMITTED` 的审批入口）。
pub const PERM_PO_STATUS: &str = "business:erp:purchase-order:status";
/// 由采购单生成收货单。
pub const PERM_STOCK_IN_CREATE: &str = "business:erp:stock-in:create";
/// 收货单状态流转（`DRAFT → POSTED` 过账）。
pub const PERM_STOCK_IN_STATUS: &str = "business:erp:stock-in:status";
/// 调拨单状态流转（提交 / 审批 / 发运 / 收货）。
pub const PERM_TRANSFER_STATUS: &str = "business:erp:transfer:status";

// ==================== 处理人 ====================

/// 一个「下一个动作」的责任角色（含可能的唯一具体处理人）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    /// 角色主键。
    pub role_id: Uuid,
    /// 角色码（如 `ACCOUNTANT`）。
    pub role_code: String,
    /// 该角色下**唯一**启用用户时给出（多人时不猜）。
    pub user_id: Option<Uuid>,
    /// 该用户的显示名。
    pub user_name: Option<String>,
}

/// 权限码 → 责任角色的反查器（**按请求缓存**，一次预检里同一个权限码只查一次库）。
///
/// 多个角色命中时取 `sort_order` 最小、`role_code` 字典序最小那个 —— 排序是**确定**的，
/// 同一个权限码每次给同一个答案（否则「该谁处理」会在两次刷新之间跳来跳去）。
#[derive(Debug, Clone, Default)]
pub struct OwnerResolver {
    cache: HashMap<String, Option<Owner>>,
}

impl OwnerResolver {
    /// 空缓存。
    pub fn new() -> Self {
        Self::default()
    }

    /// 反查一个权限码的责任角色（`None` = 没有启用角色持有该码，不猜）。
    pub async fn resolve(
        &mut self,
        ex: &impl SqlExecutor,
        permission: &str,
    ) -> Result<Option<Owner>, ApiError> {
        let key = permission.trim().to_string();
        if let Some(cached) = self.cache.get(&key) {
            return Ok(cached.clone());
        }
        let found = if key.is_empty() {
            None
        } else {
            load_owner(ex, &key).await?
        };
        self.cache.insert(key, found.clone());
        Ok(found)
    }
}

/// 真正查库：权限码 → 角色 →（唯一用户）。
async fn load_owner(ex: &impl SqlExecutor, permission: &str) -> Result<Option<Owner>, ApiError> {
    let rows = ex
        .query_sql(
            "SELECT r.id AS role_id, r.role_code AS role_code \
               FROM sys_role r \
               JOIN sys_role_menu rm ON rm.role_id = r.id \
               JOIN sys_menu m ON m.id = rm.menu_id \
              WHERE m.auth_code = ? AND m.deleted_at IS NULL \
                AND r.deleted_at IS NULL AND r.status = 1 \
              ORDER BY r.sort_order, r.role_code \
              LIMIT 1"
                .to_string(),
            vec![Value::Text(permission.to_string())],
        )
        .await
        .map_err(|error| db_error(error, "反查处理角色"))?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let role_id: Uuid = row
        .get_typed("role_id")
        .map_err(|error| db_error(error, "解析角色 id"))?;
    let role_code: String = row
        .get_typed("role_code")
        .map_err(|error| db_error(error, "解析角色码"))?;

    // 角色下的启用用户：**只看前 2 个** —— 多一个就说明「具体处理人」无法确定，不必再查。
    let users = ex
        .query_sql(
            "SELECT u.id AS user_id, COALESCE(NULLIF(u.real_name, ''), u.username) AS user_name \
               FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id \
              WHERE ur.role_id = ?::uuid AND u.deleted_at IS NULL AND u.status = 1 \
                AND (ur.expire_time IS NULL OR ur.expire_time > now()) \
              ORDER BY u.created_at, u.username \
              LIMIT 2"
                .to_string(),
            vec![Value::from(&role_id)],
        )
        .await
        .map_err(|error| db_error(error, "反查处理人"))?;
    let (user_id, user_name) = if users.len() == 1 {
        let row = &users[0];
        (
            Some(
                row.get_typed::<Uuid>("user_id")
                    .map_err(|error| db_error(error, "解析处理人 id"))?,
            ),
            row.get_typed::<String>("user_name").ok(),
        )
    } else {
        (None, None)
    };

    Ok(Some(Owner {
        role_id,
        role_code,
        user_id,
        user_name,
    }))
}

/// 数据库错误 → 500（带上下文）。
fn db_error(error: DbError, ctx: &str) -> ApiError {
    utils::db_error(error, ctx)
}
