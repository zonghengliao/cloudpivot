//! 工号取号（HR-14）：`EMP-0001` 形态的人事档案码 + 前端预览入口。
//!
//! 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md` §5.2 #3（`emp_no` 留空自动取号）、
//! §5.2 #9（`GET /business/hr/employee/next-no`，**不耗号**）、§8.1 HR-14。
//! 号段口径见 D-15：工号是**主数据码**（`date_pattern='none'`、`period_key='GLOBAL'`），
//! 流水**永久不重置**（按日/按月重置会让工号每天从 `0001` 重发，必然撞
//! `uk_biz_employee_no`）。号段行由 `V0386__hr_seq_dict` 种入，本卡**零迁移**。
//!
//! # 本文件只做三件事
//!
//! | # | 事 | 谁是唯一真源 |
//! |:--:|---|---|
//! | 1 | 把 `EMP` 登记成主数据号段 | `erp::seq::MASTER_CODE_TYPES` / [`crate::business::erp::seq::master_code_exists_sql`] |
//! | 2 | 取号（新建员工时，**与 INSERT 同事务**） | [`generate_emp_no`] → [`crate::business::erp::seq::next_master_code_free`] |
//! | 3 | 预览（前端「生成工号」按钮，**不耗号**） | [`preview_emp_no`] → [`crate::business::erp::seq::peek_master_code`] |
//!
//! 三处都复用 `erp::seq`，**不重写发号逻辑**（卡面要求）：发号是单语句
//! `UPDATE biz_seq … RETURNING current_no`，PG 对 `biz_seq(seq_type='EMP')` 那行加行锁，
//! 并发事务先后拿到 `n` / `n+1`，天然不重号；「撞号顺延」靠
//! [`crate::business::erp::seq::master_code_exists_sql`] 里的 `EMP` 查重 SQL（`biz_employee.emp_no`
//! 未删除内存在即跳过）。
//!
//! # 为什么取号必须在调用方的事务里（本卡验收「不跳号」）
//!
//! [`generate_emp_no`] 接的是 `&impl SqlExecutor`，`create_employee` 传的是**同一个事务**：
//!
//! ```text
//! BEGIN
//!   UPDATE biz_seq SET current_no = current_no + step WHERE seq_type='EMP' RETURNING current_no
//!   SELECT 1 FROM biz_employee WHERE emp_no = 'EMP-0007' AND deleted_at IS NULL   -- 占位则顺延
//!   INSERT INTO biz_employee (... emp_no='EMP-0007' ...)
//! COMMIT      -- 号与员工一起落地
//! ```
//!
//! 任何一步失败 → `ROLLBACK` ⇒ `current_no` 跟着回滚 ⇒ **号不被白吃**（没有空号）。
//! 反面做法（先 `next_master_code_free_global` 取号、再另开事务插员工）在「插员工失败」
//! 时会留下一个永久空洞，且工号是人工肉眼可读的档案码 —— 空号会被当成「有人被删档」。
//!
//! # 为什么预览与取号是两条路（`peek` 不推进 `current_no`）
//!
//! 前端交互是「点一下按钮先看号」（设计 §5.2 #9）：看了号又取消，**不能**把号吃掉。
//! 所以 `next-no` 走 `peek_master_code`（纯读 + 查重）；真正占号发生在
//! `POST /business/hr/employee` 落地那一刻（`emp_no` 留空 ⇒ 取号 + 插入同事务）。
//! 预览值天生可能被别人抢走（两人同时点按钮看到同一个号），这正是「预览不是占号」的语义 ——
//! 保存时若撞号，唯一键会以 `409 + 68001` 报出来（HR-10 的冲突翻译），用户再点一次即可。
//!
//! # `emp_no` 何时取号、何时保留手工值
//!
//! 与主数据口径一致（`erp::seq` 模块文档「手工覆盖」）：`emp_no` 有值（非空白）⇒
//! **原样保留**（企业已有工号体系要能对接，老员工的 `E2003` / `HR12-xxx` 也能建档）；
//! 留空 ⇒ 取号。格式校验只挡「能不能用」（长度 ≤ 32 与 `VARCHAR(32)` 对齐），
//! **不强制 `EMP-0001` 形态**。
//!
//! # 权限码
//!
//! `next-no` 用的是**新建**的码 `business:hr:employee:create`（设计 §5.2 #9）：
//! 能新建员工的人才能看下一个工号；预览不产生任何数据，不额外开码。
//!
//! # 与其它卡的边界
//!
//! - 本卡**不改** `employee_handler.rs` 的 7 条路由口径（那文件有「7 条」计数断言，
//!   进去加一条会把 HR-12 的证据口径改掉）—— 路由放本文件；
//! - 号段行属 HR-03 的 `V0386`（**已应用，不可改**）：本卡只读它，不新增/不修改迁移；
//! - 前端员工新建表单还没有对应卡（HR-18 只落了列表页 + 详情抽屉，表单未落），
//!   所以本卡零前端改动；契约（路径 / 出参 `next_no`）按设计 §5.2 #9 就位，
//!   表单卡直接调即可。

use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::erp::seq;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量（与设计 / 迁移逐字一致） ====================

/// 工号号段（`biz_seq.seq_type`，`V0386__hr_seq_dict` 种入的那一行）。
pub const SEQ_TYPE: &str = "EMP";

/// 路由路径（不含 `/api`，由 `[router] prefix` 注入；`ROUTE_CONTRACT` 逐字同此值）。
pub const ROUTE_NEXT_NO: &str = "/business/hr/employee/next-no";

/// 预览下一个工号的权限码（设计 §5.2 #9：与新建共用 `business:hr:employee:create`）。
pub const PERM_NEXT_NO: &str = "business:hr:employee:create";

/// 工号展示形态（`prefix` + `separator` + `pad_width` 来自 `biz_seq`，这里只用于断言与文档）。
pub const EMP_NO_EXAMPLE: &str = "EMP-0001";

/// `biz_employee.emp_no` 的列宽（`VARCHAR(32)`）；取号结果必须能落进去。
pub const MAX_EMP_NO: usize = 32;

// ==================== 出参契约 ====================

/// 预览下一个工号的出参（设计 §5.2 #9：`next_no`）。
///
/// 只有 `next_no` 一个字段，**不返回** `current_no` / `pad_width` 这类内部口径 ——
/// 前端要的只是「填进输入框的那个串」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmployeeNextNoRes {
    /// 下一个可用工号（`EMP-0002` 形态）。
    ///
    /// 「可用」= 当下没被占用；**不占号** —— 两人同时预览可能看到同一个值，
    /// 真正占号发生在 `POST /business/hr/employee`（见模块文档）。
    pub next_no: String,
}

// ==================== 取号 / 预览 ====================

/// 取一个未被占用的工号（**推进** `biz_seq.current_no`）。
///
/// # 参数
///
/// - `ex`：**必须是调用方的事务**（`&tx`）—— 取号与员工 INSERT 同事务，失败一起回滚，
///   号不会被白吃。传 `db()` 也能用（自动提交），但那会留下空号，只适合离线工具。
///
/// # 返回值
///
/// `EMP-0001` 形态的工号；失败 → 500（号段规则缺失/停用，或连号 20 个都被占用）。
pub async fn generate_emp_no(ex: &impl SqlExecutor) -> std::result::Result<String, ApiError> {
    seq::next_master_code_free(ex, SEQ_TYPE)
        .await
        .map_err(|error| seq_error(error, "生成工号"))
}

/// 预览下一个工号（**不推进** `biz_seq.current_no`，设计 §5.2 #9「不耗号」）。
///
/// 实现是 `seq::peek_master_code(db(), "EMP")`：读规则 + 逐个试号 + 查重，
/// 不改任何行。前端点「生成工号」按钮 / 打开新建抽屉时调它。
pub async fn preview_emp_no() -> std::result::Result<String, ApiError> {
    seq::peek_master_code(db(), SEQ_TYPE)
        .await
        .map_err(|error| seq_error(error, "预览工号"))
}

/// 取号失败 → 500 + 可执行的人话（不是「服务器内部错误」）。
///
/// 号段不可用是**配置问题**（`biz_seq` 里 `EMP` 那行被删/停用、或 `current_no`
/// 落后到连续 20 个号都已被占）：用户无论怎么点都不会成功，报文必须点名
/// 「找管理员核对 `biz_seq(seq_type='EMP')`」，否则只会变成一张「新建员工报 500」的工单。
/// 原始 `DbError` 一律进 `internal_detail`（不出响应体），与 `utils::db_error` 同口径。
fn seq_error(error: DbError, ctx: &str) -> ApiError {
    match &error {
        DbError::InvalidValue(detail) => ApiError {
            status: 500,
            code: error_code::INTERNAL_ERROR,
            msg: format!(
                "{ctx}失败：工号号段 EMP 当前不可用（取号规则被删除/停用，或连号长期被占用）。\
                 请让管理员核对 biz_seq(seq_type='EMP') —— 该行由迁移 V0386 种入，\
                 工号是永久不重置的档案码。"
            ),
            internal_detail: Some(format!("{ctx}: {detail}")),
            data: None,
        },
        _ => utils::db_error(error, ctx),
    }
}

// ==================== 接口 ====================

/// 预览下一个工号（设计 §5.2 #9）。
///
/// `GET /business/hr/employee/next-no` → `{ "next_no": "EMP-0002" }`。
///
/// 静态段 `/next-no` 与详情路由 `/employee/{id}` 的 `{id}` 在 axum 的路由树里
/// **静态优先**匹配，所以这条不会被 `employee_detail` 抢走（那边会拿 `"next-no"` 去
/// `parse_uuid` 报 400）。
#[nik::permission(
    path = "/business/hr/employee/next-no",
    method = "GET",
    permission = "business:hr:employee:create"
)]
#[nik::get("/business/hr/employee/next-no")]
pub async fn employee_next_no() -> ResResult<EmployeeNextNoRes> {
    Ok(Res::ok(EmployeeNextNoRes {
        next_no: preview_emp_no().await?,
    }))
}

#[cfg(test)]
mod tests {
    //! 纯单测：号段登记 / 出参形状 / 路由与契约对齐 / 失败报文。
    //!
    //! 真库用例（取号不重不跳、并发、回滚不留空号、撞号顺延、create 全链路）在
    //! [`crate::business::hr::employee_no_db_tests`]。

    use super::*;
    use crate::business::hr::employee_handler;
    use crate::business::hr::handler::ROUTE_CONTRACT;

    /// `EMP` 必须登记成主数据号段 —— 否则 `next_master_code_free(&tx, "EMP")` 直接
    /// `InvalidValue("不是主数据号段")`，自动取号这条路根本走不通。
    #[test]
    fn emp_is_registered_as_master_code() {
        assert_eq!(SEQ_TYPE, "EMP");
        assert!(
            seq::is_master_code_type(SEQ_TYPE),
            "EMP 不在 seq::MASTER_CODE_TYPES 里 —— 取号会直接报「不是主数据号段」"
        );
        assert!(
            seq::master_code_exists_sql(SEQ_TYPE).is_some(),
            "EMP 没有占位检查 SQL —— 撞号不会顺延"
        );
    }

    /// 取号形态与列宽：`EMP-0001`（4 位），且流水涨到列上限也塞得进 `VARCHAR(32)`。
    #[test]
    fn emp_no_shape_fits_column() {
        assert_eq!(seq::format_master_code("EMP", "-", 4, 1), EMP_NO_EXAMPLE);
        assert_eq!(seq::format_master_code("EMP", "-", 4, 42), "EMP-0042");
        // 位数是 `pad_width`（不足左补 0，超过不截断）：12 位满宽 = 16 字符
        let full = seq::format_master_code("EMP", "-", 12, 999_999_999_999);
        assert_eq!(full, "EMP-999999999999");
        assert_eq!(full.len(), 16, "EMP- + 12 位 = 16 字符：{full}");
        // 工号永不重置，流水迟早涨到 i64 上限（19 位）—— 那时也必须是 23 字符，仍塞得下
        let widest = seq::format_master_code("EMP", "-", 12, i64::MAX);
        assert_eq!(widest.len(), 23, "EMP- + 19 位 = 23 字符：{widest}");
        assert!(
            widest.len() <= MAX_EMP_NO,
            "最长形态 {widest} 都要塞进 VARCHAR({MAX_EMP_NO})"
        );
    }

    /// 出参只有一个 `next_no`（前端按这个形状解析；多字段是契约漂移）。
    #[test]
    fn next_no_response_shape() {
        let res = EmployeeNextNoRes {
            next_no: "EMP-0007".to_string(),
        };
        let json = serde_json::to_value(&res).expect("序列化");
        assert_eq!(json, serde_json::json!({ "next_no": "EMP-0007" }));

        let back: EmployeeNextNoRes =
            serde_json::from_value(serde_json::json!({ "next_no": "EMP-0008" })).expect("反序列化");
        assert_eq!(back.next_no, "EMP-0008");
    }

    /// 预览用的权限码就是新建的码（设计 §5.2 #9），且**不是**新造的第 6 个员工码。
    #[test]
    fn preview_permission_is_the_create_code() {
        assert_eq!(PERM_NEXT_NO, employee_handler::PERM_CREATE);
    }

    /// 本文件声明的路由 = `ROUTE_CONTRACT` 里登记给 **HR-14** 的那一条
    /// （路径 / 方法 / 权限码三者逐字对上）。
    ///
    /// `#[nik::permission]` 与 `#[nik::get]` 写错都不会编译失败，只会在线上 403 / 404 ——
    /// 所以这里按源码扫一遍与契约对拍。
    #[test]
    fn route_and_permission_match_contract() {
        let source = include_str!("employee_no.rs");
        let route = regex::Regex::new(r#"#\[nik::(get|post|put|delete)\(\s*"([^"]+)"\)\]"#).expect("正则合法");
        let mut mine: Vec<(String, String)> = Vec::new();
        for capture in route.captures_iter(source) {
            mine.push((capture[1].to_ascii_uppercase(), capture[2].to_string()));
        }
        assert_eq!(mine.len(), 1, "本卡只落 #9 一条路由，实际 {mine:?}");
        let (method, path) = mine.remove(0);
        assert_eq!(method, "GET");
        assert_eq!(path, ROUTE_NEXT_NO, "路径必须与常量一致（契约表逐字对拍）");
        assert!(
            !path.starts_with("/api"),
            "路由宏路径不含 /api 前缀（由 [router] prefix 注入）"
        );

        let spec = ROUTE_CONTRACT
            .iter()
            .find(|spec| spec.path == path && spec.method == method)
            .unwrap_or_else(|| panic!("{method} {path} 不在 ROUTE_CONTRACT 里 —— 先登记契约再实现"));
        assert_eq!(spec.card, "HR-14", "本路由在契约里登记的卡");
        assert_eq!(spec.permission, Some(PERM_NEXT_NO));

        let permission = regex::Regex::new(r#"permission = "([^"]+)"\s*\)\]"#)
            .expect("正则合法")
            .captures(source)
            .map(|capture| capture[1].to_string())
            .expect("本文件必须有 #[nik::permission]");
        assert_eq!(permission, PERM_NEXT_NO, "权限属性与契约不一致（线上会 403）");
    }

    /// 取号失败：`InvalidValue`（号段缺失/停用）→ **500 且报文点名去哪查**，
    /// 原始原因只进 `internal_detail`；其它 `DbError` 仍走通用 500。
    #[test]
    fn seq_error_is_actionable() {
        let missing = seq_error(
            DbError::InvalidValue("没有可用取号规则（seq_type='EMP'）".to_string()),
            "生成工号",
        );
        assert_eq!(missing.status, 500);
        assert_eq!(missing.code, error_code::INTERNAL_ERROR);
        assert!(missing.msg.contains("EMP"), "报文点名号段：{}", missing.msg);
        assert!(missing.msg.contains("biz_seq"), "报文点名去哪查：{}", missing.msg);
        assert!(missing.msg.contains("管理员"), "报文点名找谁：{}", missing.msg);
        let detail = missing.internal_detail.expect("原始原因进 internal_detail");
        assert!(
            detail.contains("seq_type='EMP'"),
            "internal_detail 保留原始原因：{detail}"
        );

        let generic = seq_error(DbError::NotFound, "生成工号");
        assert_eq!(generic.status, 500);
        assert_eq!(generic.msg, "服务器内部错误", "非配置类错误走通用 500 口径");
    }
}
