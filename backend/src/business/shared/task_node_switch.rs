//! 任务节点「走向」字面量（`config.task.on_complete` / `on_reject` / `on_cancel`）的**唯一真源**。
//!
//! # 为什么这三个枚举放在 `shared`（P2-37 · P2-B 解环）
//!
//! 它们原来长在 `flow::node_config`（流程节点配置的校验与序列化都在那儿），但**两侧都要用**：
//!
//! | 谁 | 怎么用 | 依据 |
//! |---|---|---|
//! | `flow`（节点配置的属主） | 校验发布的 `config.task`、把任务节点走向写进快照 | `flow::node_config` |
//! | `task`（节点快照的消费方） | `GET /business/task/{id}/flow` 要回**规范化字面量**（`DIRECT` / `VERIFY` …），前端按它决定按钮 | `task::task_model_flow::switch_field` |
//!
//! 字面量只有一份实现（`as_str` / `parse` 大小写不敏感），所以两个域必须共享**同一份**——
//! 各写一份 = 「流程发布的取值」与「任务页认得的取值」会在第一次加值时静默分叉。
//!
//! 而只要真源留在 `flow`，`task` 侧就得 import 审批域的 `node_config`（哪怕只为一个枚举），
//! `task → flow` 这条边永远摘不掉（P2-37 的卡面目标就是把它归零）。
//! 于是**本体搬到本模块**，`flow::node_config` 原样再导出（`pub use`），
//! `flow` 侧 100+ 处调用点一行不用改，`task` 侧只依赖 `shared`。
//!
//! # 边界（别把别的东西也搬进来）
//!
//! 本模块**只有**这三个纯枚举（零依赖、无 I/O、无 SQL）。节点配置的**校验**、`TaskNodeConfig`
//! 的默认值、`due` 的键集、`approve_biz_type` 的约束都留在 `flow::node_config` ——
//! 那些是「流程定义长什么样」的知识，任务侧不该也不需要知道。

/// 任务完成后流程怎么走（`config.task.on_complete`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnComplete {
    /// 干完即算通过（负责人 `DOING→DONE`）。
    Direct,
    /// 任务进 `REVIEW`，由 creator（= 流程发起人）验收后通过。
    Verify,
    /// 完成后自动发起一个审批流（**P-FLOW-1B 落地**：任务 `DOING→DONE` 时节点保持 `ACTIVE`，
    /// 同事务发起 `approve_biz_type` 审批单，审批通过才推进下游）。
    Approve,
}

impl OnComplete {
    /// 入库 / 出参字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            OnComplete::Direct => "DIRECT",
            OnComplete::Verify => "VERIFY",
            OnComplete::Approve => "APPROVE",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "DIRECT" => Some(OnComplete::Direct),
            "VERIFY" => Some(OnComplete::Verify),
            "APPROVE" => Some(OnComplete::Approve),
            _ => None,
        }
    }

    /// 全部合法取值（出参文档 / 前端下拉 / 用例断言用，顺序即展示顺序）。
    pub const ALL: [OnComplete; 3] = [OnComplete::Direct, OnComplete::Verify, OnComplete::Approve];
}

/// 任务被「打回」（`REVIEW→DOING`）时流程怎么走（`config.task.on_reject`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnReject {
    /// 退回上一节点（复用引擎 `apply_return`，`round+1`）。
    BackToPrev,
    /// 终止实例（`REJECTED`）。
    Terminate,
    /// 不动，等任务再完成（默认；「返工」本来就该等）。
    Stay,
}

impl OnReject {
    /// 入库 / 出参字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            OnReject::BackToPrev => "BACK_TO_PREV",
            OnReject::Terminate => "TERMINATE",
            OnReject::Stay => "STAY",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "BACK_TO_PREV" => Some(OnReject::BackToPrev),
            "TERMINATE" => Some(OnReject::Terminate),
            "STAY" => Some(OnReject::Stay),
            _ => None,
        }
    }

    /// 全部合法取值（顺序即展示顺序）。
    pub const ALL: [OnReject; 3] = [OnReject::BackToPrev, OnReject::Terminate, OnReject::Stay];
}

/// 任务被取消（`CANCELLED`）时流程怎么走（`config.task.on_cancel`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnCancel {
    /// 终止实例（默认：活被取消了，流程不该继续往下走）。
    Terminate,
    /// 退回上一节点。
    BackToPrev,
    /// 不动。
    Stay,
}

impl OnCancel {
    /// 入库 / 出参字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            OnCancel::Terminate => "TERMINATE",
            OnCancel::BackToPrev => "BACK_TO_PREV",
            OnCancel::Stay => "STAY",
        }
    }

    /// 解析（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "TERMINATE" => Some(OnCancel::Terminate),
            "BACK_TO_PREV" => Some(OnCancel::BackToPrev),
            "STAY" => Some(OnCancel::Stay),
            _ => None,
        }
    }

    /// 全部合法取值（顺序即展示顺序）。
    pub const ALL: [OnCancel; 3] = [OnCancel::Terminate, OnCancel::BackToPrev, OnCancel::Stay];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三个枚举的 `as_str` → `parse` 必须**双向闭环**（落库值与出参值同一个字面量）。
    #[test]
    fn as_str_and_parse_are_symmetric() {
        for value in OnComplete::ALL {
            assert_eq!(OnComplete::parse(value.as_str()), Some(value), "OnComplete 闭环断裂");
        }
        for value in OnReject::ALL {
            assert_eq!(OnReject::parse(value.as_str()), Some(value), "OnReject 闭环断裂");
        }
        for value in OnCancel::ALL {
            assert_eq!(OnCancel::parse(value.as_str()), Some(value), "OnCancel 闭环断裂");
        }
    }

    /// 解析大小写不敏感、两侧空白容忍（编辑器原样输出，大小写不做承诺）。
    #[test]
    fn parse_is_case_and_space_insensitive() {
        assert_eq!(OnComplete::parse(" verify "), Some(OnComplete::Verify));
        assert_eq!(OnReject::parse("back_to_prev"), Some(OnReject::BackToPrev));
        assert_eq!(OnCancel::parse("Terminate"), Some(OnCancel::Terminate));
    }

    /// 未知取值一律 `None`（不猜、不兜底成第一个）—— 调用方据此走「前端按 need_review 推断」。
    #[test]
    fn unknown_literal_is_none() {
        assert_eq!(OnComplete::parse("MAYBE"), None);
        assert_eq!(OnReject::parse(""), None);
        assert_eq!(OnCancel::parse("REJECT"), None);
    }
}
