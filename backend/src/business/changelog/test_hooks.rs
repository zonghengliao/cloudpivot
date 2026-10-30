//! 强制回滚测试钩子（**仅** `test-hooks` feature 下编译）。
//!
//! 依据 `docs/批次C-技术架构.md` §4.12（回滚不留假日志的三层测试）与 §7 C7 行。
//!
//! # 这个文件解决什么问题
//!
//! 「业务事务回滚 → 变更日志一起消失」这件事，C5 已经用真库实测证明过一次
//! （`tests/batchC_c5_changelog.rs::record_in_same_transaction_rolls_back`）。
//! 但**一次性的人工实测**不能防止将来有人把 `&tx` 改成 `db()`——那需要一条
//! 随时能重跑、失败就红的门禁。本模块就是这条门禁的执行体：
//!
//! ```text
//! ① 查「回滚前」该对象的日志行数      → 期望 0
//! ② begin 一个真实事务 → record() 写日志（传的是 &tx，不是 db()）
//! ③ 在同一事务里再查一次行数          → 期望 1（证明真的写进去了）
//! ④ tx.rollback() 强制回滚
//! ⑤ 用连接池再查一次行数              → 期望 0（证明随业务一起消失）
//! ```
//!
//! 输出被固定成 `回滚前|事务内|回滚后`（正常情况下就是 `0|1|0`），
//! 供 `scripts/verify/changelog_rollback.sh` 直接断言与打印。
//!
//! # 为什么必须是 feature 门控（红线）
//!
//! 「强制回滚」是一个**故意让事务失败**的能力。它一旦进了生产二进制，就等于
//! 给线上留了一个「可以把业务改动吞掉」的入口（哪怕需要额外参数触发，也是
//! 审计上无法接受的攻击面）。因此：
//!
//! - 整个模块由 `#[cfg(feature = "test-hooks")]` 控制（见 `super::test_hooks` 的声明）；
//! - `Cargo.toml` 里 `test-hooks` **不在 default 里**，`cargo build` / `cargo build --release`
//!   的产物里**不存在** `record_then_rollback` 这个符号（C7 验收会 grep 二进制确认）。
//!
//! # 为什么本文件里没有 `db()`
//!
//! 门禁 G2（`grep -rn "db()" business/changelog/` 非注释命中 = 0）对整个模块目录生效，
//! 本文件也在其中。所以这里**不接受全局连接**，而是要求调用方把连接（`&Db`）显式传进来——
//! 测试与探针本来就自己 `Db::connect()`，多传一个参数换来「模块内永不自开第二个事务」这条
//! 可 grep 的不变量，值得。

use nik::prelude::*;

use crate::shared::error_code;

use super::{ChangeCtx, ChangeTarget, DiffInput, RecordOutcome, model, record_in};

/// 回滚探针的一次采样结果。
///
/// 三个计数的含义见模块文档的 ①③⑤：它们共同构成「回滚零残留」的可执行断言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackProbe {
    /// 被探测对象的业务主键（每次调用都不同，避免与库里其它数据互相干扰）。
    pub resource_id: String,
    /// 事务开始**之前**，`sys_change_log` 里该对象的行数（期望 0）。
    pub before: i64,
    /// 事务**内部**（`record` 之后、回滚之前）该对象的行数（期望 1）。
    pub inside: i64,
    /// 回滚**之后**，该对象的行数（期望 0）。
    pub after: i64,
    /// `record` 的返回值是否真的写了行（`Skipped` 说明输入前后相等，探针无效）。
    pub recorded: bool,
}

impl RollbackProbe {
    /// 固定格式 `回滚前|事务内|回滚后`（门禁脚本的契约，不要改格式）。
    pub fn flat(&self) -> String {
        format!("{}|{}|{}", self.before, self.inside, self.after)
    }

    /// 是否满足「回滚零残留」：`0|1|0` 且确实写进去过一行。
    ///
    /// `inside` 必须恰好为 1 是关键的一半：如果 `record` 根本没写（输入无变化、
    /// 或实现里把日志改成异步写在别处），那么 `0|0|0` 也会满足「回滚后 0 行」，
    /// 但那证明的是「什么都没发生」，不是「写了又一起回滚」。**这一条把假绿排除掉。**
    pub fn is_zero_residue(&self) -> bool {
        self.recorded && self.before == 0 && self.inside == 1 && self.after == 0
    }
}

/// 在真实事务里 `record` 之后**强制回滚**，并返回三个采样点。
///
/// # 参数
///
/// - `connection`：连接池（`Db`）。探针用它开事务、也用它做回滚**前后**的两次查询——
///   回滚后的查询走的是**另一条连接**，因此「看不到那行」不是同一个事务的快照错觉；
/// - `ctx` / `target` / `input`：同 [`record_in`]，透传给真正写日志的那条路径。
///
/// # 返回值
///
/// [`RollbackProbe`]：`0|1|0` 即通过（用 [`RollbackProbe::is_zero_residue`] 判定）。
///
/// # 异常情况
///
/// - `record` 本身失败（未登记资源 62012、未来时间 62013、分区越界、数据库故障）→
///   先把事务回滚掉再返回 `500 + 内部错误码`，错误消息里带上被折叠掉的原始错误，
///   免得测试只看到「500」而不知道根因；
/// - 计数查询失败 → 同上（事务一律先回滚，不留悬挂连接）。
pub async fn record_then_rollback(
    connection: &Db,
    ctx: &ChangeCtx<'_>,
    target: ChangeTarget<'_>,
    input: DiffInput<'_>,
) -> std::result::Result<RollbackProbe, ApiError> {
    // 目标坐标要在 move 进 record_in 之前留下副本：回滚前后两次计数都要用它。
    let resource = target.resource;
    let resource_id = target.resource_id.clone();

    let before = model::count_for_object(connection, resource, &resource_id)
        .await
        .map_err(|error| internal("回滚前计数失败", error))?;

    // 显式事务对象（`Db::begin`），而不是 `db().transaction(闭包)`：
    // 闭包式 API 只能在「返回 Err」时回滚，那样探针就没法把「事务内计数」带出来。
    let tx = connection
        .begin()
        .await
        .map_err(|error| internal("开启探针事务失败", error))?;

    // ① 走的是生产同款路径：record_in(&tx, ...)（传 &tx —— 与 handler 里的写法逐字一致）
    let outcome = match record_in(&tx, ctx, target, input).await {
        Ok(outcome) => outcome,
        Err(error) => {
            // 失败也要把事务终结掉，否则连接会挂着直到超时
            let _ = tx.rollback().await;
            return Err(internal("探针事务内 record 失败", error));
        }
    };

    // ② 事务内计数：证明「此刻确实写在事务里」，而不是被跳过或写到别处去了
    let inside = match model::count_for_object(&tx, resource, &resource_id).await {
        Ok(count) => count,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(internal("事务内计数失败", error));
        }
    };

    // ③ 强制回滚（D11 的另一半：日志与业务同生共死）
    tx.rollback()
        .await
        .map_err(|error| internal("强制回滚失败", error))?;

    // ④ 回滚后计数：走连接池，看到的是「已提交的数据」这一真实视角
    let after = model::count_for_object(connection, resource, &resource_id)
        .await
        .map_err(|error| internal("回滚后计数失败", error))?;

    Ok(RollbackProbe {
        resource_id,
        before,
        inside,
        after,
        recorded: matches!(outcome, RecordOutcome::Recorded { .. }),
    })
}

/// 把任何错误折成「500 + 内部错误码」的 [`ApiError`]，消息里保留原始成因。
fn internal(context: &str, error: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!("{context}：{error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::RollbackProbe;

    fn probe(before: i64, inside: i64, after: i64, recorded: bool) -> RollbackProbe {
        RollbackProbe {
            resource_id: "c7-unit".to_string(),
            before,
            inside,
            after,
            recorded,
        }
    }

    /// 输出格式是全套门禁的契约（脚本按 `|` 切三段做断言），改动即破坏 CI。
    #[test]
    fn flat_format_is_fixed() {
        assert_eq!(probe(0, 1, 0, true).flat(), "0|1|0");
        assert_eq!(probe(2, 3, 2, true).flat(), "2|3|2");
    }

    /// 只有「写了(inside=1) + 回滚后没了(after=0)」才算通过。
    #[test]
    fn zero_residue_requires_the_inside_row() {
        assert!(probe(0, 1, 0, true).is_zero_residue());
        // 0|0|0 是「什么都没写」，不是「写了又回滚」，必须判红
        assert!(!probe(0, 0, 0, false).is_zero_residue());
        // 写了但没回滚（after=1）是假日志，必须判红
        assert!(!probe(0, 1, 1, true).is_zero_residue());
        // 回滚前就有残留（before>0）说明夹具不干净，同上
        assert!(!probe(1, 2, 1, true).is_zero_residue());
    }
}
