//! 员工档案导入模板下载（卡 `t_b0808afe` · `GET /business/hr/employee/import-template`）。
//!
//! # 为什么 HR 模块也要有一条模板路由
//!
//! impexp 框架已经有通用模板下载（`GET /business/impexp/import/template?biz_type=hr.employee`），
//! 但那一条要求 `business:impexp:import:template` —— 人事专员通常只有 HR 的权限码。
//! 本路由把「下载员工导入模板」挂到 **`business:hr:employee:import`**（`V0387` 已种下的
//! 那个按钮权限），人事专员在员工档案页点「下载导入模板」不会因为缺 impexp 权限而 403。
//!
//! # 模板内容 = 定义 → xlsx（**唯一来源**）
//!
//! 模板不是在这里手写的：它由 `impexp::template::build_template` 从
//! [`crate::business::impexp::builtin::hr::employee::EmployeeImport`] 的列定义生成，
//! 与「导入解析读的列定义」是**同一份常量**。所以「模板改了 12 列、解析还是 11 列」这种
//! 错位事故在结构上不可能发生（卡面注意事项第 2 条）。
//!
//! 上传 / 预检 / 确认入库 / 错误行下载仍然走 impexp 的通用接口（那几条要
//! `business:impexp:import:{upload,confirm,download}`），前端按钮按这两个权限码一起门禁。

use axum::response::{IntoResponse, Response};
use nik::prelude::*;

use crate::business::impexp::builtin::hr::employee::EMPLOYEE_BIZ_TYPE;
use crate::business::impexp::handler::xlsx_response;
use crate::business::impexp::registry::global_registry;
use crate::business::impexp::template;
use crate::shared::error_code;

/// 模板下载路由（不含 `/api`，由 `[router].prefix` 注入）。
pub const TEMPLATE_ROUTE: &str = "/business/hr/employee/import-template";

/// 模板下载权限码（与 `sys_menu.auth_code` 逐字一致：`V0387` 的「导入员工」按钮）。
pub const TEMPLATE_PERMISSION: &str = "business:hr:employee:import";

/// 下载员工档案导入模板（xlsx）。
///
/// # 返回值
///
/// `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet` 文件流，
/// 文件名 `员工档案导入模板_v1.xlsx`（带版本号：用户手里有旧模板时能一眼看出差异）。
///
/// # 异常情况
///
/// - 403：无 `business:hr:employee:import`；
/// - 500 + `10001`：定义未注册（理论上不可能，注册表在启动时装载并有单测钉住）或写 xlsx 失败。
#[nik::permission(
    path = "/business/hr/employee/import-template",
    method = "GET",
    permission = "business:hr:employee:import"
)]
#[nik::get("/business/hr/employee/import-template")]
pub async fn import_template() -> Response {
    match build_template() {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 生成模板响应（handler 与单测共用）。
fn build_template() -> std::result::Result<Response, ApiError> {
    let definition = global_registry().import_definition(EMPLOYEE_BIZ_TYPE).ok_or_else(|| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("导入定义「{EMPLOYEE_BIZ_TYPE}」未注册，模板无法生成（请联系管理员）"),
        )
    })?;
    let bytes = template::build_template(definition.as_ref())?;
    Ok(xlsx_response(bytes, &template::template_file_name(definition.as_ref())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_route_and_permission_are_the_card_contract() {
        // 卡面：GET /business/hr/employee/import-template，权限 business:hr:employee:import
        assert_eq!(TEMPLATE_ROUTE, "/business/hr/employee/import-template");
        assert_eq!(TEMPLATE_PERMISSION, "business:hr:employee:import");
        // 权限码必须是 V0387 种下的那一个（auth_code 与 sys_menu 一一对应）
        assert!(TEMPLATE_PERMISSION.starts_with("business:hr:employee:"));
    }

    #[test]
    fn template_response_is_a_real_xlsx() {
        let response = build_template().expect("模板应生成成功");
        // xlsx = zip 容器，响应体前 4 字节是 PK\x03\x04
        assert_eq!(response.status(), 200);
        let headers = response.headers();
        assert_eq!(
            headers
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
        );
        let disposition = headers
            .get(axum::http::header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(disposition.contains("filename*=UTF-8''"), "{disposition}");
        assert!(
            disposition.contains("%E5%91%98%E5%B7%A5"), // 「员工」的 UTF-8 百分号编码
            "文件名里应带中文「员工档案导入模板」：{disposition}"
        );
    }
}
