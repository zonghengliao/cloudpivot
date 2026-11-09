//! 「我的指标」接口：工作台顶部四张个人统计卡。
//!
//! # 权限
//!
//! **登录即可**（不挂 `#[nik::permission]`），但返回的每个数字都只按
//! `claims` 里的用户 id 过滤 —— 没有 `?user_id=` 这类入参，别人无从看别人的数。
//!
//! 为什么不像审批中心那样挂 `business:flow:instance:read`：这四张卡是**自己的工作台**，
//! 数字取的是「属于我」的行（我的待办 / 我操作过的 / 我负责的任务），
//! 不是「系统里的单据」；给它们再套一个模块读权限，会让「有新员工但还没配角色」的
//! 账号一进首页就被 403 糊一脸，卡片却本来就能安全返回 0。
//!
//! # 异常情况
//!
//! | 码 | 场景 |
//! |----|------|
//! | 401 | 令牌缺少用户标识 |
//! | 500 | 数据库查询异常 |

use nik::prelude::*;

use crate::business::dashboard::dto::MetricCardRes;
use crate::business::dashboard::model;
use crate::shared::error_code;
use crate::utils;

/// 我的指标（四张卡：待办审批 / 本周已办 / 今日任务 / 逾期）。
///
/// # 返回值
///
/// 成功返回固定 4 条的 [`MetricCardRes`] 列表（空数据也是 4 条，值为 0）。
///
/// # 异常情况
///
/// - 401：令牌缺少用户标识；
/// - 500：数据库查询失败。
#[nik::get("/business/dashboard/my-metrics")]
pub async fn my_metrics(Extension(AuthClaims(claims)): Extension<AuthClaims>) -> ResResult<Vec<MetricCardRes>> {
    let me = utils::current_user_uuid(&claims)
        .ok_or_else(|| ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识"))?;
    let counts = model::load_counts(me)
        .await
        .map_err(|error| utils::db_error(error, "查询我的工作台指标"))?;
    Ok(Res::ok(model::build_cards(counts)))
}
