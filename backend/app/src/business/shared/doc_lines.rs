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
//! 明细最多 200 行（当前 301 行）。请拆成多张单据提交，每张不超过 200 行。
//! ```
//!
//! # 为什么后半句从「用导入向导」改成「拆单」
//!
//! P1-04 初版写的是「大批量请用导入向导（通道支持 2 万行）」，但导入通道
//! `register_import` 全量只有 9 个定义（字典 + 6 个主数据 + HR×2），**没有一个单据**
//! —— 用户点进去找不到自己要建的那张单，那句是空头支票。
//! 产品决策（`t_35a19386`，见 `docs/单据明细导入口径-决策.md` §2/§3.4）改为让用户做
//! **他此刻真能做的动作**：拆成多张单据提交（7 个单据上拆单路径均成立）。
//! 单据明细 Excel 导入另进需求池 `RP-2026-009`（触发制），不是本函数的活 ——
//! 也别在此处留任何指向导入向导的名字，否则下一手会以为通道已经存在。
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
///
/// 7 个单据共用这一句；`200` 是 [`DEFAULT_MAX_LINES`] 的写法值（有单测钉住两者一致）。
/// 盘点单（2000 行）的动作不是「拆单」而是「分多次抽盘」，它走
/// [`too_many_lines_labeled`] 传自己的尾句，不用这里。
pub const SPLIT_DOC_HINT: &str = "请拆成多张单据提交，每张不超过 200 行。";

/// 明细超限的统一错误文案：先说清「超了什么」，再给「下一步做什么」。
///
/// # 参数
///
/// - `limit`：该单据自己的上限（`*_dto::MAX_LINES`）。
/// - `actual`：本次提交收到的行数。
pub fn too_many_lines(limit: usize, actual: usize) -> String {
    too_many_lines_with(limit, actual, SPLIT_DOC_HINT)
}

/// 同一套格式 + 自定义尾句（盘点单 2000 行用：它的动作是「分多次抽盘」，不是「拆单」）。
///
/// 前句的 `limit` / `actual` 始终是插值 —— 尾句只负责「怎么办」，不带数字。
pub fn too_many_lines_with(limit: usize, actual: usize, hint: &str) -> String {
    too_many_lines_labeled("", limit, actual, hint)
}

/// 同口径 + 单据名前缀（盘点单两个入口要分清「抽盘」与「实盘」：
/// 建单时是抽盘范围超限、填实盘数时是实盘行超限，两句话不能混）。
///
/// `label` 直接拼在「明细最多」前面，所以传 `"抽盘"` 得到「抽盘明细最多 …」。
pub fn too_many_lines_labeled(label: &str, limit: usize, actual: usize, hint: &str) -> String {
    format!("{label}明细最多 {limit} 行（当前 {actual} 行）。{hint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文案三段齐全：上限、当前行数、下一步动作 —— 少任何一段用户都得猜。
    ///
    /// 用例名沿用 P1-04 历史（`channel` = 当时的「该走哪条通道」，现在通道就是「拆单」；
    /// 改名会与 `docs/单据明细导入口径-决策.md` §4 的引用脱钩）。
    #[test]
    fn message_names_limit_actual_and_channel() {
        let message = too_many_lines(200, 301);
        assert!(message.contains("最多 200 行"), "要点名上限：{message}");
        assert!(message.contains("当前 301 行"), "要点名实际行数：{message}");
        assert!(
            message.contains("拆成多张单据提交"),
            "必须给出用户能做的动作（拆单）：{message}"
        );
        assert!(
            message.contains("每张不超过 200 行"),
            "要说清每张的上限，否则用户不知道拆到多少：{message}"
        );
        // ↓ 两条反向断言：谁把「导入向导 / 2 万行」这张空头支票改回来，测试就红。
        //   导入通道 `register_import` 里没有这 7 个单据（决策文档 §1 F1），
        //   把它写回去 = 又让用户点进一个找不到单据的页面。
        assert!(
            !message.contains("导入向导"),
            "导入向导里没有这些单据，不许再拿它当出口：{message}"
        );
        assert!(
            !message.contains("2 万行"),
            "也不许暗示导入通道能接 2 万行明细：{message}"
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

    /// 自定义尾句生效（盘点单口径），且 `too_many_lines` 仍等于默认尾句。
    #[test]
    fn custom_hint_replaces_only_the_tail() {
        let take = too_many_lines_with(2000, 2001, "请按仓库或物料分类分多次抽盘。");
        assert_eq!(take, "明细最多 2000 行（当前 2001 行）。请按仓库或物料分类分多次抽盘。");
        assert_eq!(
            too_many_lines(2000, 2001),
            too_many_lines_with(2000, 2001, SPLIT_DOC_HINT)
        );
        assert!(
            too_many_lines(200, 201).ends_with(SPLIT_DOC_HINT),
            "默认尾句必须还是拆单：{}",
            too_many_lines(200, 201)
        );
    }

    /// 带前缀的变体：盘点单「抽盘 / 实盘」两个入口的逐字文案（决策文档 §3.5）。
    #[test]
    fn labeled_variant_keeps_the_document_prefix() {
        let hint = "请按仓库或物料分类分多次抽盘。";
        assert_eq!(
            too_many_lines_labeled("抽盘", 2000, 2001, hint),
            "抽盘明细最多 2000 行（当前 2001 行）。请按仓库或物料分类分多次抽盘。"
        );
        assert_eq!(
            too_many_lines_labeled("实盘", 2000, 2001, hint),
            "实盘明细最多 2000 行（当前 2001 行）。请按仓库或物料分类分多次抽盘。"
        );
        // 无前缀时与 `too_many_lines_with` 完全一致（别为了加前缀把默认口径改坏）。
        assert_eq!(
            too_many_lines_labeled("", 2000, 2001, hint),
            too_many_lines_with(2000, 2001, hint)
        );
    }

    /// 尾句里的 `200` 必须跟着 `DEFAULT_MAX_LINES` 走（改了默认上限，这里先红再让文案对齐）。
    #[test]
    fn split_hint_names_the_default_limit() {
        assert!(
            SPLIT_DOC_HINT.contains(&format!("每张不超过 {DEFAULT_MAX_LINES} 行。")),
            "尾句的每张上限要与 DEFAULT_MAX_LINES 一致：{SPLIT_DOC_HINT}"
        );
    }
}
