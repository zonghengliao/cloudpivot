//! 成本核算模块开关探针（ARCH-9 §7.4 灰度 · COST-B4-4）。
//!
//! # 为什么需要一个接口
//!
//! 开关 `erp.cost.enabled` 决定前端三件事：① 菜单要不要渲染（后端已在
//! `/system/auth/menus` 过滤掉了，前端不用管）；② 出库单表单要不要渲染「成本对象」字段；
//! ③ 成本模块页面要不要提示「模块未启用」而不是白屏报错。②③ 都需要前端**自己知道**
//! 当前值，而参数配置接口要 `support:config:list` 权限 —— 普通财务 / 仓管没有。
//!
//! 因此这里开一个**极轻量的只读探针**：任意已登录用户可读，只回一个布尔 + 参数键。
//!
//! # 它自己不会被开关拦住
//!
//! 路径落在 `/business/erp/cost-` 前缀里，但
//! [`crate::business::shared::cost_switch::SWITCH_PROBE_PATH`] 在中间件里被显式豁免
//! （否则关掉开关就再也问不到「开关是关的」，前端只能猜）。
//!
//! # 接口一览
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/cost-switch` | 无（登录即可） | 回 `{enabled, config_key, hint}` |

use nik::prelude::*;
use serde::Serialize;

use crate::business::shared::cost_switch;

/// 开关状态响应。
///
/// 只下发「当前值 + 参数键 + 一句人话」，**不下发**参数页的完整行（描述、分类、可修改标记
/// 属管理端信息，参数配置接口已经有了）。
#[derive(Debug, Clone, Serialize)]
pub struct CostSwitchRes {
    /// 当前是否启用（`false` = 成本核算模块整体关闭）。
    pub enabled: bool,
    /// 参数键，前端拼提示语用（避免前端硬编码字符串）。
    pub config_key: String,
    /// 一句给界面用的说明（模块关闭时展示在成本页 / 出库单字段位置的占位文案）。
    pub hint: String,
}

/// 读取成本核算模块开关。
///
/// # 返回值
///
/// [`CostSwitchRes`]：`enabled` 取 `sys_config` 的当前值（缺省 / 停用 / 写错一律 `false`）。
///
/// # 异常情况
///
/// 无：参数读不到按「关闭」处理，不因配置缺失返回 500（前端只需知道开没开）。
#[nik::get("/business/erp/cost-switch")]
pub async fn state() -> ResResult<CostSwitchRes> {
    let enabled = cost_switch::is_enabled().await;
    let hint = if enabled {
        "成本核算模块已启用".to_string()
    } else {
        format!(
            "成本核算模块未启用（{} = false）。请系统管理员在「支撑功能 / 参数配置 / ERP 成本」里打开，切换即时生效",
            cost_switch::CONFIG_KEY
        )
    };
    Ok(Res::ok(CostSwitchRes {
        enabled,
        config_key: cost_switch::CONFIG_KEY.to_string(),
        hint,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 探针路径必须是「被开关豁免的那一个」——路径与常量写岔了，关掉开关后前端就问不到了。
    #[test]
    fn probe_path_is_exempt_from_gate() {
        assert_eq!(cost_switch::SWITCH_PROBE_PATH, "/business/erp/cost-switch");
        assert!(!cost_switch::is_cost_api_path(cost_switch::SWITCH_PROBE_PATH));
        // 同前缀的普通接口仍然受控（确认豁免是「精确路径」而不是「前缀」）
        assert!(cost_switch::is_cost_api_path("/business/erp/cost-switch/other"));
        assert!(cost_switch::is_cost_api_path("/business/erp/cost-object"));
    }

    /// 响应体形状：三个字段都在，且 `enabled` 是 JSON 布尔（前端 `if (res.enabled)` 直接用）。
    #[test]
    fn response_shape_is_stable() {
        let res = CostSwitchRes {
            enabled: false,
            config_key: cost_switch::CONFIG_KEY.to_string(),
            hint: "成本核算模块未启用".to_string(),
        };
        let json = serde_json::to_value(&res).expect("序列化");
        assert_eq!(json["enabled"], serde_json::json!(false));
        assert_eq!(json["config_key"], serde_json::json!("erp.cost.enabled"));
        assert!(json["hint"].as_str().is_some_and(|s| !s.is_empty()));
    }

    /// 关闭态提示语必须点名参数键与开启路径（用户在界面上照着就能修，不需要问工程师）。
    #[test]
    fn disabled_hint_points_to_config_page() {
        let key = cost_switch::CONFIG_KEY;
        let hint =
            format!("成本核算模块未启用（{key} = false）。请系统管理员在「支撑功能 / 参数配置 / ERP 成本」里打开");
        assert!(hint.contains("erp.cost.enabled"));
        assert!(hint.contains("参数配置"));
    }
}
