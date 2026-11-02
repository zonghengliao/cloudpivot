//! 合同接口层（**A1 只留装配位，接口由 A2 落地**）。
//!
//! # 本文件现在有什么、没什么
//!
//! 有：5 个权限码的**唯一常量表**（与 `V0197` 迁移种进 `sys_menu.auth_code` 的值逐字对应），
//! 以及「A2 该往哪填」的路由清单文档。没有：任何 handler 函数。
//!
//! 为什么"空模块"也要先建：路由由 `#[nik::get]` / `#[nik::post]` 宏声明，
//! 由 `lib.rs` 的 `App::scan()` 在启动时统一收集 —— 也就是说 **A2 只要往本文件里加带宏的
//! 函数，路由自动生效**，不需要再动 `lib.rs` / `erp/mod.rs` 的装配。A1 把模块声明、
//! 权限码、菜单种子、错误码全部就位，A2 直接开工。
//!
//! # A2 要填的路由（与权限码一一对应，照 ARCH-2 附录 B #7）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/contract/page` | `erp:contract:list` | 分页列表（软删行不返回） |
//! | GET | `/business/erp/contract/{id}` | `erp:contract:list` | 详情（含可用性判定结果，供采购单选择器） |
//! | POST | `/business/erp/contract` | `erp:contract:create` | 建单（`DRAFT` + `HT-yyyyMMdd-000001`） |
//! | PUT | `/business/erp/contract/{id}` | `erp:contract:update` | 改单（按 §5.3 冻结矩阵逐列比对） |
//! | DELETE | `/business/erp/contract/{id}` | `erp:contract:delete` | 软删（V-21 引用守卫） |
//! | PATCH | `/business/erp/contract/{id}/status` | `erp:contract:status` | 状态流转（5 条边，其余 409 66015） |
//!
//! `path` 里**不写 `/api` 前缀**（由 `[router].prefix` 注入，见 AGENTS.md 约定 8）。
//!
//! # 占位路由（A2 直接改函数体，不用再动装配）
//!
//! 6 个端点已按上表挂上宏（路径 + 权限码 + 401/403 链路全部生效），函数体统一返回
//! `501 + A1 占位说明`。A2 到位后把 [`pending`] 那行换成真实实现即可 ——
//! 路径、权限注解、`App::scan()` 收集都不用碰。**占位返回 501 而不是空 `Ok`**，
//! 是为了让「还没实现」在联调时立刻可见，而不是静默给前端一个空列表。
//!
//! # 权限码前缀为什么是 `erp:contract:*` 而不是 `business:erp:contract:*`
//!
//! 与 V0141 那 32 个 `business:erp:*` 码**不一致**，这是照 ARCH-2 附录 B #7 与实现卡的
//! 逐字契约执行：A2（本文件的注解）、A3（采购单挂合同）、A5（前端按钮判权）三张卡都会按
//! 同一个字符串写代码。A1 已在交付说明里把口径分歧报给总管；若要统一前缀，
//! 必须三卡同时改（改 Rust 注解 + 改 `V0197` 的 `auth_code`），**不允许单方面改**。
//!
//! `SUPER_ADMIN` 短路：`TokenClaims::has_permission` 的实现是
//! `is_super_admin() || permissions.contains(code)`，超管的通配码走 `*:*:*` 分支，
//! 与业务侧角色判定（`shared/principal.rs`）不构成双真值源。
//!
//! # 占位实现为什么是 `501` 而不是 `todo!()`
//!
//! `todo!()` 会让整个进程 panic（一个未实现的接口把服务拖挂）；`501 Not Implemented`
//! 是**语义正确**的 HTTP 状态：路由存在、鉴权生效、只是这一层还没写。
//! 错误码暂时复用 [`error_code::INTERNAL_ERROR`]（A1 不新增业务码，8 个合同码都有明确归属），
//! A2 落地时按 ARCH-2 §3.2 的表逐条换成 `66014`–`66021`。

use axum::extract::Path;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;

/// 合同列表 / 详情（`sys_menu` 里是 menu_type=2 的菜单节点）。
pub const PERM_LIST: &str = "erp:contract:list";
/// 新建合同。
pub const PERM_CREATE: &str = "erp:contract:create";
/// 编辑合同（A2 按 §5.3 冻结矩阵校验）。
pub const PERM_UPDATE: &str = "erp:contract:update";
/// 删除合同（软删，V-21 引用守卫）。
pub const PERM_DELETE: &str = "erp:contract:delete";
/// 状态流转（`state_machine::transition(Entity::Contract, …)` 的唯一入口）。
pub const PERM_STATUS: &str = "erp:contract:status";

/// 合同域全部权限码（顺序与 `V0197` 的 `auth_code` 一致，也是「权限码唯一真相」）。
///
/// 与迁移**逐字对应**：`V0197__erp_contract_permissions.up.sql` 的 5 条 `sys_menu` 行
/// 就是这个数组。任一侧改动而另一侧没跟，`contract_permissions_match_menu` 用例会红。
pub const PERMISSIONS: [&str; 5] = [
    PERM_LIST,
    PERM_CREATE,
    PERM_UPDATE,
    PERM_DELETE,
    PERM_STATUS,
];

/// 权限码的模块前缀（`{模块}:{资源}:{动作}` 的第一段）。
pub const PERMISSION_MODULE: &str = "erp";
/// 权限码的资源段（第二段）。
pub const PERMISSION_RESOURCE: &str = "contract";

/// 占位错误：路由已挂、鉴权已生效，业务实现归 A2。
///
/// 消息里点名端点与归属卡，便于联调时一眼看出「不是 404 找不到路，是这层还没写」。
fn pending(endpoint: &str) -> ApiError {
    ApiError::new(
        501,
        error_code::INTERNAL_ERROR,
        format!(
            "{endpoint} 尚未实现：A1 只交付 biz_contract 表 / Entity::Contract 状态机 / \
             66014–66021 错误码 / erp:contract:* 权限码这套基座，接口实现归 A2。\
             路由与权限注解已就位，A2 只需替换本函数体"
        ),
    )
}

/// 合同分页列表（`A2` 落地）：软删行不返回，默认 `created_at DESC`。
#[nik::permission(
    path = "/business/erp/contract/page",
    method = "GET",
    permission = "erp:contract:list"
)]
#[nik::get("/business/erp/contract/page")]
pub async fn contract_page() -> ResResult<serde_json::Value> {
    Err(pending("GET /business/erp/contract/page"))
}

/// 合同详情（`A2` 落地）：软删 / 不存在一律 404 + `66014`。
#[nik::permission(
    path = "/business/erp/contract/{id}",
    method = "GET",
    permission = "erp:contract:list"
)]
#[nik::get("/business/erp/contract/{id}")]
pub async fn contract_detail(Path(id): Path<String>) -> ResResult<serde_json::Value> {
    let _ = Uuid::parse_str(id.trim());
    Err(pending("GET /business/erp/contract/{id}"))
}

/// 新建合同（`A2` 落地）：`DRAFT` + `HT-yyyyMMdd-000001`，乙方名称由服务端快照。
#[nik::permission(
    path = "/business/erp/contract",
    method = "POST",
    permission = "erp:contract:create"
)]
#[nik::post("/business/erp/contract")]
pub async fn contract_create() -> ResResult<serde_json::Value> {
    Err(pending("POST /business/erp/contract"))
}

/// 修改合同（`A2` 落地）：按 ARCH-2 §5.3 冻结矩阵逐列比对，`66016` 拒绝越权改动。
#[nik::permission(
    path = "/business/erp/contract/{id}",
    method = "PUT",
    permission = "erp:contract:update"
)]
#[nik::put("/business/erp/contract/{id}")]
pub async fn contract_update(Path(id): Path<String>) -> ResResult<serde_json::Value> {
    let _ = Uuid::parse_str(id.trim());
    Err(pending("PUT /business/erp/contract/{id}"))
}

/// 软删合同（`A2` 落地）：V-21 引用守卫，被未关闭采购单引用时 `66018`。
#[nik::permission(
    path = "/business/erp/contract/{id}",
    method = "DELETE",
    permission = "erp:contract:delete"
)]
#[nik::delete("/business/erp/contract/{id}")]
pub async fn contract_delete(Path(id): Path<String>) -> ResResult<serde_json::Value> {
    let _ = Uuid::parse_str(id.trim());
    Err(pending("DELETE /business/erp/contract/{id}"))
}

/// 状态流转（`A2` 落地）：**唯一入口**是 `state_machine::transition(Entity::Contract, …)`，
/// 非法边 409 + `66015`，需要原因的两条终止边缺 reason 400。
#[nik::permission(
    path = "/business/erp/contract/{id}/status",
    method = "PATCH",
    permission = "erp:contract:status"
)]
#[nik::patch("/business/erp/contract/{id}/status")]
pub async fn contract_change_status(Path(id): Path<String>) -> ResResult<serde_json::Value> {
    let _ = Uuid::parse_str(id.trim());
    Err(pending("PATCH /business/erp/contract/{id}/status"))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 五个码齐全、不重复，且都落在 `erp:contract:*` 下（N15 的 `{模块}:{资源}:{动作}` 形态）。
    #[test]
    fn contract_permission_codes_are_well_formed() {
        assert_eq!(PERMISSIONS.len(), 5, "ARCH-2 附录 B #7 定了 5 个码");
        let unique: std::collections::BTreeSet<&str> = PERMISSIONS.iter().copied().collect();
        assert_eq!(unique.len(), PERMISSIONS.len(), "权限码不得重复");

        for code in PERMISSIONS {
            assert_eq!(
                code.matches(':').count(),
                2,
                "{code} 必须是 {{模块}}:{{资源}}:{{动作}} 三段式（N15 只认 ≥2 个冒号）"
            );
            let mut parts = code.split(':');
            assert_eq!(parts.next(), Some(PERMISSION_MODULE), "{code} 的模块段不对");
            assert_eq!(
                parts.next(),
                Some(PERMISSION_RESOURCE),
                "{code} 的资源段不对"
            );
            let action = parts.next().unwrap_or_default();
            assert!(!action.is_empty(), "{code} 缺动作段");
            assert!(
                action.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{code} 的动作段必须小写（前端 error-code / 按钮判权按小写匹配）"
            );
        }

        // 动作集合与文档逐字一致（顺序即文档顺序）
        let actions: Vec<&str> = PERMISSIONS
            .iter()
            .map(|code| code.rsplit(':').next().unwrap_or_default())
            .collect();
        assert_eq!(
            actions,
            vec!["list", "create", "update", "delete", "status"],
            "动作集合必须与 ARCH-2 附录 B #7 一致"
        );
    }

    /// ② 常量名与码值的一一对应（防止有人改了常量却改了别的码）。
    #[test]
    fn contract_permission_constants_match_list() {
        assert_eq!(PERM_LIST, "erp:contract:list");
        assert_eq!(PERM_CREATE, "erp:contract:create");
        assert_eq!(PERM_UPDATE, "erp:contract:update");
        assert_eq!(PERM_DELETE, "erp:contract:delete");
        assert_eq!(PERM_STATUS, "erp:contract:status");
        assert_eq!(
            PERMISSIONS,
            [
                PERM_LIST,
                PERM_CREATE,
                PERM_UPDATE,
                PERM_DELETE,
                PERM_STATUS
            ]
        );
    }

    /// ③ 与 `sys_menu.auth_code` 一一对应（真库用例；需 `ERP_DB_URL`，否则跳过）。
    ///
    /// 这条用例是「权限码唯一真相」的守门人：迁移里少种一个码 → 除超管外全员 403；
    /// 迁移里多种一个码 → 管理面能勾一个永不生效的权限（N14 的反方向缺陷）。
    #[tokio::test]
    async fn contract_permissions_match_menu() {
        use nik::prelude::*;

        let Some(_guard) = DbTestGuard::acquire().await else {
            return;
        };
        let rows = db()
            .find_with_params(
                "SELECT auth_code FROM sys_menu \
                  WHERE auth_code LIKE 'erp:contract:%' AND deleted_at IS NULL AND status = 1",
                vec![],
            )
            .await
            .expect("查询 sys_menu 失败");

        let mut in_db: Vec<String> = Vec::with_capacity(rows.len());
        for row in rows {
            in_db.push(row.get_typed::<String>("auth_code").expect("auth_code 列"));
        }
        in_db.sort();
        let mut expected: Vec<String> = PERMISSIONS.iter().map(|c| (*c).to_string()).collect();
        expected.sort();
        assert_eq!(
            in_db, expected,
            "sys_menu.auth_code 必须与 crate 常量逐字一致（V0197 迁移 + 本文件要同步改）"
        );
    }

    /// 真库用例串行闸（与其它 erp 模块同一套约定：`ERP_DB_URL` 未设置则返回 `None`）。
    struct DbTestGuard;

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    impl DbTestGuard {
        async fn acquire() -> Option<tokio::sync::MutexGuard<'static, ()>> {
            let guard = DB_LOCK.lock().await;
            use nik::prelude::*;
            use tokio::sync::OnceCell;
            static READY: OnceCell<bool> = OnceCell::const_new();
            let ready = *READY
                .get_or_init(|| async {
                    let Ok(url) = std::env::var("ERP_DB_URL") else {
                        println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                        return false;
                    };
                    if try_db().is_some() {
                        return true;
                    }
                    let mut config = DatabaseConfig::from_url(&url);
                    config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                    config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                    match Db::connect_with(&url, config).await {
                        Ok(connection) => set_db(connection).is_ok(),
                        Err(err) => {
                            println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                            false
                        }
                    }
                })
                .await;
            if ready { Some(guard) } else { None }
        }
    }
}
