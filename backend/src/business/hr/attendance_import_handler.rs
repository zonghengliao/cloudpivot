//! 考勤导入入口（设计 §5.2 #23 · `POST /business/hr/attendance/import` · 看板卡 t_df5d6d1c）。
//!
//! # 这条路由为什么存在（契约面，不是功能面）
//!
//! 功能上「导入考勤」早就通了：HR-21 交了 impexp 的导入定义
//! （[`crate::business::impexp::builtin::hr::AttendanceImport`]，`biz_type='hr.attendance'`），
//! 前端与脚本一直用通用三段（`/business/impexp/import/upload` → `/{id}/confirm`）。
//! 缺的是**设计文档点的那个接口**：`docs/architecture/ARCH-9-人力资源模块设计.md` §5.2 #23
//! 写的是 `POST /business/hr/attendance/import`，而它至今 404 —— 前端只能绕过 HR 前缀去打
//! impexp，并且用 `business:impexp:import:*` 这一套与人事无关的权限码。
//!
//! # 薄封装：一行都不能复制
//!
//! 本文件**只做三件事**：
//!
//! 1. 把 `biz_type` 固定成 [`ATTENDANCE_BIZ_TYPE`]（客户端无法通过 query 覆盖）；
//! 2. 把 multipart 与 `mode` / `duplicate` 原样交给
//!    [`crate::business::impexp::handler::import_upload_for`]；
//! 3. 把它的回执原样返回。
//!
//! 解析 / 预检 / 落库 SQL **一律不在这里**。卡面红线说得很直白：复制一份的那天，
//! `AttendanceImport::validate_batch`（「无薪假 ≤ 已批请假天数」）与 `persist` 里的期间头
//! 重算就会有一边漏掉 —— 那正是 t_de6e8d41 花了一整张卡才排掉的缝。
//! 单测 [`tests::handler_owns_no_import_logic`] 静态扫本文件，钉住这条红线。
//!
//! # 为什么不拆第二个「确认入库」接口
//!
//! 确认入库（`POST /business/impexp/import/{id}/confirm`）**本来就是业务无关的**：它按
//! `task_id` 读回任务行 → 解析文件 → 重新校验 → 分批调用定义的 `persist`。`persist` 是谁，
//! 完全由任务行的 `biz_type` 决定（这里必然是 `hr.attendance`），所以「HR 专属 confirm」
//! 只会是一层不做任何判断的转发，多一个接口就多一处「两个入口行为要同步」的维护面。
//!
//! 反过来说，重复实现一个有意义的 confirm 才是危险的：它必须复刻
//! `sys_import_task` 状态机（`VALIDATED` 才可入库）、SHA-256 文件校验、`STRICT/PARTIAL`
//! 的分批与逐行降级、取消标记、错误明细回写 —— 任何一处漏掉都会出现「考勤能进库、别处进不去」
//! 的双口径。**导入 = upload（本路由）+ confirm（impexp 通用）**，这与设计 §5.2 的下一条
//! （#24 导出）同样的取舍一致：只有真正业务专属的动作才另开路由。
//!
//! # 权限码
//!
//! `business:hr:attendance:import` 是 `V0387__hr_menu_perms` §2 早就种下的按钮权限点
//! （`sys_menu` uuid `…e023`，「导入考勤」），HR_MANAGER 角色由同迁移 §3 授权 36 条覆盖到它。
//! 所以本卡**不需要新迁移**：再插一行只会撞 `sys_menu` 的主键 / 让「36 条菜单」的自检
//! （`V0398__hr_selfcheck`）与 `V0387` 的口径分叉。单测
//! [`tests::permission_is_seeded_by_v0387`] 把「码真的在种子里」变成编译期约束。
//!
//! 鉴权行为与其它业务路由一致：未登录 → 401（认证中间件），带令牌但缺码 → 403
//! （`#[nik::permission]` 的 `RequirePermissionLayer`），`SUPER_ADMIN` 短路。

use axum::extract::{Extension, Multipart, Query};
use nik::prelude::*;
use serde::Deserialize;

use crate::business::impexp::builtin::hr::ATTENDANCE_BIZ_TYPE;
use crate::business::impexp::dto::UploadRes;
use crate::business::impexp::handler::import_upload_for;

/// 导入路由（**不含** `/api`，由 `[router].prefix` 注入；设计 §5.2 #23 逐字）。
pub const IMPORT_ROUTE: &str = "/business/hr/attendance/import";

/// 导入权限码（与 `V0387` 的 `sys_menu.auth_code` 逐字一致）。
pub const IMPORT_PERMISSION: &str = "business:hr:attendance:import";

/// 上传入参：只有 `mode` / `duplicate` 两个可选开关。
///
/// **没有 `biz_type`**：它是本路由的存在意义（固定 `hr.attendance`）。从客户端读
/// `biz_type` 等于把「HR 导入」变成「任意业务导入」的别名，顺带绕开权限面。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AttendanceImportQuery {
    /// 导入模式：`STRICT`（默认）/ `PARTIAL`。
    pub mode: Option<String>,
    /// 重复数据策略：`UPSERT`（默认）/ `SKIP`。
    pub duplicate: Option<String>,
}

/// 上传 + 预检（**路由与真库用例共用的唯一入口**）。
///
/// # 参数
///
/// - `mode` / `duplicate`：客户端开关（`None` = 默认值由 impexp 决定）；
/// - `multipart`：multipart 表单，字段名固定 `file`（与通用上传一致）；
/// - `claims`：当前用户（任务归属 `create_by` + 审计）。
///
/// # 返回值
///
/// impexp 的 [`UploadRes`]（`task_id` / `task_no` / `status` / `report`），与通用上传同构。
///
/// # 异常情况
///
/// 与通用上传完全一致：400/422（文件与结构）、413（超限）、404 + `60011`（定义未注册）、
/// 500（落盘 / 建任务失败）。锁定期间的拦截发生在预检（`report.error` 带 Excel 行号）
/// 与确认入库（422 + `60014`）两处，口径见
/// [`crate::business::impexp::builtin::hr::AttendanceImport`]。
pub(crate) async fn import_for_attendance(
    mode: Option<&str>,
    duplicate: Option<&str>,
    multipart: Multipart,
    claims: &TokenClaims,
) -> std::result::Result<UploadRes, ApiError> {
    import_upload_for(ATTENDANCE_BIZ_TYPE, mode, duplicate, multipart, claims).await
}

/// 上传考勤 Excel（`POST /business/hr/attendance/import`）。
///
/// # 参数
///
/// - `query`：`mode` / `duplicate`（均可省略）；
/// - `claims`：当前用户（由认证中间件注入）；
/// - `multipart`：字段名固定 `file` 的 xlsx。
///
/// # 返回值
///
/// 200 + impexp 的 `UploadRes`（`report` 里含总行数 / 通过 / 失败 / 错误行明细）。
///
/// # 异常情况
///
/// - 401：未登录；403：缺 `business:hr:attendance:import`；
/// - 其余同 [`import_for_attendance`]（就是它的错误）。
#[nik::permission(
    path = "/business/hr/attendance/import",
    method = "POST",
    permission = "business:hr:attendance:import"
)]
#[nik::post("/business/hr/attendance/import")]
pub async fn import(
    Query(query): Query<AttendanceImportQuery>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    multipart: Multipart,
) -> ResResult<UploadRes> {
    let outcome = import_for_attendance(query.mode.as_deref(), query.duplicate.as_deref(), multipart, &claims).await?;
    Ok(Res::ok(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `V0387` 的菜单 / 权限种子原文（编译期嵌入）。
    const V0387_SEED: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");

    /// 本文件自身的源码（静态红线检查用）。
    const SOURCE: &str = include_str!("attendance_import_handler.rs");

    #[test]
    fn route_and_permission_are_the_design_contract() {
        // 设计 §5.2 #23：POST /business/hr/attendance/import，码 business:hr:attendance:import
        assert_eq!(IMPORT_ROUTE, "/business/hr/attendance/import");
        assert_eq!(IMPORT_PERMISSION, "business:hr:attendance:import");
        assert_eq!(
            ATTENDANCE_BIZ_TYPE, "hr.attendance",
            "biz_type 必须是设计 §5.2 #23 逐字的那一个"
        );
    }

    #[test]
    fn permission_is_seeded_by_v0387() {
        // 权限点必须真实存在（否则非超管账号在线上全 403，且看起来像「权限没配」）
        assert!(
            V0387_SEED.contains(IMPORT_PERMISSION),
            "{IMPORT_PERMISSION} 不在 V0387 的菜单种子里"
        );
        // 且是「按钮」权限点（menu_type = 3），不是页面
        let line = V0387_SEED
            .lines()
            .find(|line| line.contains(IMPORT_PERMISSION))
            .expect("种子行");
        assert!(
            line.contains("import', 3,") || line.contains("import',3,") || line.contains(", 3,"),
            "导入考勤应是 menu_type = 3 的按钮权限点：{line}"
        );
        assert!(line.contains("导入考勤"), "按钮标题应为「导入考勤」：{line}");
    }

    /// 入参只有 `mode` / `duplicate`：客户端拿不到 `biz_type` 这个口子。
    #[test]
    fn query_cannot_override_biz_type() {
        let query: AttendanceImportQuery =
            serde_json::from_str(r##"{"mode":"PARTIAL","duplicate":"SKIP"}"##).expect("两个开关可解析");
        assert_eq!(query.mode.as_deref(), Some("PARTIAL"));
        assert_eq!(query.duplicate.as_deref(), Some("SKIP"));

        // 传 biz_type 也不会被采纳（结构体里没有这个字段）
        let sneaky: AttendanceImportQuery =
            serde_json::from_str(r##"{"biz_type":"system.user","mode":null}"##).expect("多余字段被忽略");
        assert_eq!(sneaky.mode, None);
        assert_eq!(sneaky.duplicate, None);
        // 结构体定义里不许出现 biz_type 字段（只查结构体本身，注释与测试断言不算）
        let struct_body = SOURCE
            .split("pub struct AttendanceImportQuery")
            .nth(1)
            .and_then(|tail| tail.split('}').next())
            .expect("找得到入参结构体");
        assert!(
            !struct_body.contains("biz_type"),
            "入参结构体不许出现 biz_type 字段（它就是本路由固定住的那一个）：{struct_body}"
        );
        assert!(struct_body.contains("mode") && struct_body.contains("duplicate"));
    }

    /// 红线：本文件只做转发，**不得**出现任何解析 / 预检 / 入库逻辑与 SQL。
    ///
    /// 这条断言是「复制一份导入实现」的唯一机械拦阻：复制者必须写下 `INSERT INTO`
    /// 或 `validate::` / `persist(`，而这些都是生产代码里不该有的词。
    /// 只扫 `#[cfg(test)]` 之前的部分（测试自身的禁词表不该被自己命中）。
    #[test]
    fn handler_owns_no_import_logic() {
        let production = SOURCE.split("#[cfg(test)]").next().unwrap_or_default();
        assert!(!production.is_empty());
        for forbidden in [
            "INSERT INTO",
            "ON CONFLICT",
            "UPDATE biz_attendance",
            "validate::",
            ".persist(",
            "jsonb_to_recordset",
            "sha2::",
        ] {
            assert!(
                !production.contains(forbidden),
                "考勤导入路由里出现了 `{forbidden}` —— 导入逻辑必须留在 impexp，本文件只转发"
            );
        }
        // 正向：确实委托给了 impexp 的同一条路径（同一条代码路径，不是复制）
        assert!(
            production.contains("import_upload_for("),
            "必须委托 impexp::handler::import_upload_for（同一条代码路径）"
        );
    }

    /// 定义在注册表里（启动时装载 + 单测钉住），否则路由会 404 + `60011`。
    #[test]
    fn definition_is_registered() {
        let definition = crate::business::impexp::registry::global_registry()
            .import_definition(ATTENDANCE_BIZ_TYPE)
            .expect("hr.attendance 应已注册为导入定义");
        assert_eq!(definition.biz_type(), ATTENDANCE_BIZ_TYPE);
        assert_eq!(definition.biz_name(), "考勤记录");
        // 允许部分导入（页面可选 STRICT / PARTIAL）—— 本路由只是把这层校验转交给 impexp
        assert!(definition.allow_partial());
    }
}
