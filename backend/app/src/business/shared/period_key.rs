//! 「北京时间周期键」的**唯一实现**（P2-B 解环：从 `business/erp/serial_model.rs` 下移到 `shared`）。
//!
//! # 这个文件解决什么
//!
//! 「把某一刻换算成北京时间的 `yyyyMMdd`」是**取号器**与**会计期间**共用的纯函数：
//! 单据号按天重置、库存流水按月切期、工资凭证取凭证日期，全都要求「用 CST 的天，
//! 不是 UTC 的天」。原先它长在 ERP 的 `serial_model` 里，于是任何别的域想按 CST 取
//! 一个日期，就得 `use` 一条指向 ERP 的路径（`erp` 域名 + `::serial_model`）—— HR 的请假单号就是
//! 这样长出一条 `hr → erp` 的代码回边（P2-B 卡 `t_3442b4fb` 摘掉的第一处）。
//!
//! 它是**纯函数**（不碰任何表、不读配置），归属「跨域基座」而不是任何一个业务域：
//!
//! ```text
//! 改前：hr ──period_key_of──→ erp::serial_model   （HR 为了一个 format 依赖整个 ERP）
//! 改后：hr ──period_key_of──→ shared::period_key  （ERP 侧同一份实现，见下「谁还在用」）
//! ```
//!
//! # 为什么必须 CST 而不是 UTC
//!
//! 用 UTC 会让「北京时间 00:00–08:00」这段算成前一天：跨天那一刻的号段会跟白天重叠，
//! 会计期间也会把月末 23:30 的流水记到上个月。三条口径逐字如下（迁自
//! `serial_model` 的同名函数，**行为一字未改**）：
//!
//! | 函数 | 口径 | 例 |
//! |---|---|---|
//! | [`period_key_of`] | `yyyyMMdd`（CST 的「今天」） | `2026-10-30T16:00Z` → `20261031` |
//! | [`today_period_key`] | `period_key_of(Utc::now())` | `20261115` |
//! | [`period_code_of`] | `yyyy-MM`（CST 的「本月」，会计期间码） | `2026-10-31T16:30Z` → `2026-11` |
//! | [`today_cst`] | `NaiveDate`（CST 的日期，凭证日期用） | `2026-10-30T16:00Z` → `2026-10-31` |
//!
//! # 谁还在用（改后无回边）
//!
//! - ERP：`business/erp/serial_model.rs` 的 `period_key_of` / `today_period_key` /
//!   `period_code_of` **保留同名函数**、函数体转调本模块 —— ERP 侧 10 余处调用点
//!   一行都不用改（`purchase_model` / `sales_model` / `sales_return_model` /
//!   `voucher` 的取号与记账日期都是这条路径）。
//! - HR：`business/hr/leave_model.rs` 的请假单号、`business/hr/salary_bridge.rs` 的
//!   凭证日期直接 `use crate::business::shared::period_key::…`。
//!
//! 也就是说「口径只有一份实现」，而**依赖方向**从「业务域 → 业务域」改成
//! 「业务域 → 基座」。
//!
//! # 纪律
//!
//! - 本文件**只放纯函数**：不读库、不读配置、不打日志。加 IO 会让它没法被离线用例覆盖，
//!   也会把「基座」变成需要启动链的东西。
//! - 不许写死「今天」用于业务判断以外的场景：`today_period_key` 只做「当前时刻」的换算，
//!   需要可注入时刻的场景（用例 / 补录）一律显式传 `now`。

use nik::prelude::chrono::{DateTime, Duration, NaiveDate, Utc};

/// 北京时间相对 UTC 的偏移（CST = UTC+8，无夏令时）。
///
/// 中国自 1991 年起不实行夏令时，所以固定偏移是正确的 —— 不要为了「通用」改成时区数据库，
/// 那会把一个纯函数变成一次可能失败的系统调用。
pub const CST_OFFSET_HOURS: i64 = 8;

/// 把「某一刻」换算成北京时间的周期键 `yyyyMMdd`。
///
/// 号按天重置必须用 **CST 的天**：用 UTC 会让「北京时间 00:00–08:00」这段算成前一天，
/// 跨天那一刻的号段会跟白天重叠。
///
/// ```ignore
/// assert_eq!(period_key_of(utc), "20261030");
/// // 2026-10-30T16:00Z = 北京时间 2026-10-31 00:00
/// assert_eq!(period_key_of(utc), "20261031");
/// ```
pub fn period_key_of(now: DateTime<Utc>) -> String {
    (now + Duration::hours(CST_OFFSET_HOURS)).format("%Y%m%d").to_string()
}

/// 当前周期键（北京时间今天）。
pub fn today_period_key() -> String {
    period_key_of(Utc::now())
}

/// 「某一刻」的会计期间码 `yyyy-MM`（同样用 **CST**：月末 23:30 的流水不该记到上个月）。
pub fn period_code_of(now: DateTime<Utc>) -> String {
    (now + Duration::hours(CST_OFFSET_HOURS)).format("%Y-%m").to_string()
}

/// 「某一刻」在北京时间的日期（凭证日期 / 红冲日期用）。
///
/// 实现走 [`period_key_of`] 再解析，而不是 `(now + 8h).date_naive()`：两条路在
/// `chrono` 里等价，但走同一份周期键保证「凭证日期」与「凭证号里的日期段」永远是同一天
/// （凭证号 `PZ-yyyyMMdd-…` 用的就是周期键）。
///
/// 解析理论上不会失败（`format` 产出的就是 8 位数字）；真失败时回落 `now.date_naive()`
/// 而不是 panic —— 一个格式化异常不该让过账整笔 500。
pub fn today_cst(now: DateTime<Utc>) -> NaiveDate {
    let key = period_key_of(now);
    NaiveDate::parse_from_str(&key, "%Y%m%d").unwrap_or_else(|_| now.date_naive())
}

#[cfg(test)]
mod tests {
    //! 纯单测：跨天边界 / 格式 / 与「手算 CST」一致。
    //!
    //! 这些断言**逐字迁自** `business/erp/serial_model.rs` 与 `business/erp/voucher/mod.rs`
    //! 的同款用例（搬家不改口径），所以它们同时也是「等价性」的证据。

    use super::*;

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("合法时间")
            .with_timezone(&Utc)
    }

    /// ① 同一天内：UTC 2026-10-30 03:00 = 北京 11:00 ⇒ `20261030`。
    #[test]
    fn period_key_uses_cst_day() {
        assert_eq!(period_key_of(utc("2026-10-30T03:00:00Z")), "20261030");
    }

    /// ② 跨天边界：UTC 16:00 是北京次日 00:00 ⇒ 必须已经换天。
    #[test]
    fn period_key_rolls_over_at_cst_midnight() {
        assert_eq!(
            period_key_of(utc("2026-10-30T15:59:59Z")),
            "20261030",
            "北京 23:59:59 还算当天"
        );
        assert_eq!(
            period_key_of(utc("2026-10-30T16:00:00Z")),
            "20261031",
            "北京 00:00:00 必须换天（UTC 直接 format 会算成 20261030）"
        );
    }

    /// ③ `today_period_key` 永远是 8 位数字（可以直接进 `period_key VARCHAR(8)`）。
    #[test]
    fn today_period_key_is_eight_digits() {
        let key = today_period_key();
        assert_eq!(key.len(), 8, "period_key 列宽 8：{key}");
        assert!(key.chars().all(|ch| ch.is_ascii_digit()), "{key}");
    }

    /// ④ 会计期间码：UTC 2026-10-31T16:30 = 北京 11-01 00:30 ⇒ 记到 11 月。
    #[test]
    fn period_code_rolls_over_at_cst_midnight() {
        assert_eq!(period_code_of(utc("2026-10-31T15:00:00Z")), "2026-10");
        assert_eq!(period_code_of(utc("2026-10-31T16:30:00Z")), "2026-11");
    }

    /// ⑤ 凭证日期与周期键是**同一天**（凭证号里的日期段 = 凭证日期）。
    #[test]
    fn today_cst_agrees_with_period_key() {
        for text in [
            "2026-10-30T15:59:59Z",
            "2026-10-30T16:00:00Z",
            "2026-12-31T23:59:59Z",
            "2027-01-01T00:00:00Z",
        ] {
            let now = utc(text);
            let key = period_key_of(now);
            assert_eq!(
                today_cst(now).format("%Y%m%d").to_string(),
                key,
                "凭证日期必须与周期键同一天（{text}）"
            );
        }
    }

    /// ⑥ 手算对拍：任意时刻的 CST 日期 = UTC 时间 + 8 小时后的 `date_naive`。
    #[test]
    fn cst_offset_matches_hand_computation() {
        for text in [
            "2026-01-01T00:00:00Z",
            "2026-06-30T20:00:00Z",
            "2026-11-15T13:45:00Z",
            "2027-02-28T16:00:00Z",
        ] {
            let now = utc(text);
            let manual = (now + Duration::hours(8)).date_naive();
            assert_eq!(today_cst(now), manual, "{text}");
        }
    }
}
