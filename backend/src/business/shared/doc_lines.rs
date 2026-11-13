//! 单据明细行数上限的统一口径（P1-04：**明确拒绝 + 可操作提示**）。
//!
//! # 为什么要收成一份
//!
//! 7 个单据各自声明一份 `MAX_LINES = 200`（`stocktake` 是 2000，不在此列），
//! 超限时的文案此前是 7 个文件各写一遍 `format!`，且都是纯技术口吻：
//!
//! ```text
//! items 最多 200 行（收到 301）
//! ```
//!
//! 用户读到这句不知道下一步该干什么 —— 卡面 P1-04 的决策是「不做万行明细，
//! 做明确拒绝 + 可操作提示」，于是把「说什么」收到这里：
//!
//! ```text
//! 明细最多 200 行（当前 301 行）。大批量请用「导入向导」：业务 → 通用底座 → 导入向导（导入通道支持 2 万行）
//! ```
//!
//! 各单据只保留自己的 `MAX_LINES`（上限值仍可不同），**文案不再各写一遍**：
//! 改口径改这一个文件。前端提示同口径，见 `frontend/src/views/business/erp/shared/detail-line-limit.ts`。
//!
//! # 只给文案，不给错误构造
//!
//! 各单据的 `bad_request` / `ApiError::new(400, …)` 保持原样（错误码 `PARAM_INVALID` 不变），
//! 这里只产字符串 —— 少一处耦合，改文案不会意外改错误码。

/// 单据明细行数上限的默认值（与 7 个 `*_dto.rs` 的 `MAX_LINES` 同值；改上限仍改各自的常量）。
pub const DEFAULT_MAX_LINES: usize = 200;

/// 超限后「该怎么办」的引导文案（前后端同一份口径）。
pub const IMPORT_WIZARD_HINT: &str = "大批量请用「导入向导」：业务 → 通用底座 → 导入向导（导入通道支持 2 万行）";

/// 明细超限的统一错误文案：先说清「超了什么」，再给「该走哪条通道」。
///
/// # 参数
///
/// - `limit`：该单据自己的上限（`*_dto::MAX_LINES`）。
/// - `actual`：本次提交收到的行数。
pub fn too_many_lines(limit: usize, actual: usize) -> String {
    format!("明细最多 {limit} 行（当前 {actual} 行）。{IMPORT_WIZARD_HINT}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文案三段齐全：上限、实际行数、通道引导 —— 少任何一段用户都得猜。
    #[test]
    fn message_names_limit_actual_and_channel() {
        let message = too_many_lines(200, 301);
        assert!(message.contains("最多 200 行"), "要点名上限：{message}");
        assert!(message.contains("当前 301 行"), "要点名实际行数：{message}");
        assert!(message.contains("导入向导"), "必须给出可操作通道：{message}");
        assert!(
            message.contains("2 万行"),
            "要说清通道容量（否则用户不知道为什么值得换通道）：{message}"
        );
        println!("[P1-04] 超限文案：{message}");
    }

    /// 上限跟着单据走（`stocktake` 若接入也是同一条文案，只是数字不同）。
    #[test]
    fn message_follows_the_per_document_limit() {
        assert!(too_many_lines(2000, 2001).contains("最多 2000 行"));
    }

    /// 空行数不会溢出成负数（调用方只在 `actual > limit` 时调，但函数本身要稳）。
    #[test]
    fn message_is_stable_for_zero_rows() {
        assert!(too_many_lines(200, 0).contains("当前 0 行"));
    }
}
