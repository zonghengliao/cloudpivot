//! 成本费率取数的业务编排（成本核算 · ARCH-9 §4.5.4 / B2-3）。
//!
//! # 没有 HTTP 入口（这是有意的）
//!
//! 设计 §5.2 的接口清单里没有「费率」这一组，§5.3 也没有 `cost-rate:*` 权限码：
//! 一期费率的维护方式是财务直接维护 `biz_cost_rate`（配置数据，不进业务菜单）。
//! 所以本模块**不注册路由**，只是给工时台账（单条录入 #11 / 导入 #15）用的
//! 一层薄编排：
//!
//! ```text
//! resolve_labor_rate(成本对象, 车间, 工作日期, 对象号, 车间名)
//!   → cost_rate_model::rates_of_type("LABOR")   // 一次查库（导入时全批复用）
//!   → cost_rate_dto::resolve                    // 纯函数：对象 > 车间 > 公司
//! ```
//!
//! 缺费率时抛 `422` + `66059`（[`cost_rate_dto::RATE_MISSING_CODE`]），报文里点名
//! 成本对象号 / 车间 / 日期 / 已查级别 / 怎么修 —— **绝不静默按 0**（设计 D-7）。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_rate_dto::{
    self as dto, RATE_TYPE_LABOR, RateRequest, RateRow, RateSubject, ResolvedRate,
};
use crate::business::erp::cost_rate_model as model;
use crate::utils;

/// 取某一类型的全部候选行（供批量场景复用：导入时只查一次库）。
///
/// # 异常情况
///
/// 查库失败 ⇒ 500（`utils::db_error`，真实原因只进日志）。
pub async fn candidates(rate_type: &str) -> std::result::Result<Vec<RateRow>, ApiError> {
    model::rates_of_type(rate_type)
        .await
        .map_err(|error| utils::db_error(error, "查询成本费率"))
}

/// 工资率取数：`成本对象 > 车间 > 公司`，缺费率 ⇒ `422` + `66059`。
///
/// # 参数
///
/// - `cost_object_id` / `dept_id` / `work_date`：取值主体（对象级 / 车间级 / 版本）。
/// - `cost_object_no` / `dept_name`：只用于报错文案（让用户看得懂是哪一批哪个车间）。
pub async fn resolve_labor_rate(
    cost_object_id: Uuid,
    dept_id: Uuid,
    work_date: NaiveDate,
    cost_object_no: &str,
    dept_name: &str,
) -> std::result::Result<ResolvedRate, ApiError> {
    let rows = candidates(RATE_TYPE_LABOR).await?;
    dto::resolve(
        &rows,
        &RateRequest::labor(cost_object_id, dept_id, work_date),
        &RateSubject {
            cost_object_no,
            dept_name,
        },
    )
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 编排层只做「查库 → 纯函数」的搬运：确认它引用的口径没有走样。
    #[test]
    fn module_uses_the_documented_constants() {
        assert_eq!(RATE_TYPE_LABOR, "LABOR");
        assert_eq!(dto::RATE_MISSING_CODE, 66059);
        assert_eq!(dto::lookup_chain(), "成本对象 → 车间 → 公司");
    }
}
