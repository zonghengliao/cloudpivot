//! 凭证写入接缝的**实现**（P2-B 解环 · 卡 `t_3442b4fb`）。
//!
//! # 这个文件解决什么
//!
//! `shared/voucher_port.rs` 定义了「记一张会计分录凭证」的能力接口，本文件是它的
//! **唯一实现**：每个方法都是一次**直接委派**给凭证域已有的入口，没有新增 SQL、
//! 没有改事务边界、没有改错误口径。
//!
//! ```text
//! 改前：hr ──直接用──→ voucher::{post_explicit_lines, leaf_required, rule_accounts,
//!                     find_voucher, find_voucher_locked, reverse_posted}
//! 改后：hr ──shared::voucher_port（抽象）←──本文件（实现）──委托──→ 上面这些入口
//! ```
//!
//! 这样 `hr → erp` 那条边就断了（HR 只认 `shared::voucher_port`），而**记账口径仍然只有一份**
//! —— 取号 / 借贷平衡 / 期间已结账闸 / 分录落库 / 红冲回写全在原来的函数里。
//!
//! # 等价性（逐方法）
//!
//! | 接缝方法 | 委派给 | 改造前 HR 的调用点 | 差异 |
//! |---|---|---|---|
//! | [`VoucherWriter::dept_aware_account_codes`] | 本文件 [`DEPT_AWARE_ACCOUNTS_SQL`] | `salary_bridge::dept_aware_accounts` | **SQL 原样搬来**（原先写在 HR，读的是凭证域的表 —— 这才是它该待的地方） |
//! | [`VoucherWriter::leaf_account`] | `voucher_multi::leaf_required` | 同 | 无 |
//! | [`VoucherWriter::rule_credit_account`] | `voucher_multi::rule_accounts` → `.credit` | 同 | 只取贷方（HR 的用途就这一个） |
//! | [`VoucherWriter::post_explicit`] | `voucher_multi::post_explicit_lines` | 同 | 无（`AccountPick` → `AccountRef` 字段一一对应） |
//! | [`VoucherWriter::voucher_no`] | `voucher::find_voucher` | 同 | 只返回号（HR 只要这个） |
//! | [`VoucherWriter::lock_voucher`] | `voucher::find_voucher_locked` | 同 | 只返回四个字段（HR 只核对来源） |
//! | [`VoucherWriter::reverse_posted`] | `voucher::find_voucher_locked` + `voucher::reverse_posted` | 同（HR 自己先锁、再冲） | 锁与冲合并成一次调用，**同一个事务、同一把行锁**（`FOR UPDATE` 二次取锁在本事务内是幂等的） |
//!
//! # 失败语义没变（`AGENTS.md` 约定 11）
//!
//! 每个方法都只用调用方给的 `&Transaction`，失败一律 `Err`：
//! 调用方（HR 的过账 / 红冲）把整笔事务 `ROLLBACK`，单据留在 `APPROVED` / `POSTED`，
//! 界面上的按钮还能再点一次 —— **不需要工程师改库**。
//!
//! 唯一新增的失败形态是「接缝没安装」，那是启动链漏步（编程错误），
//! `bootstrap::register_voucher_port` 里 prod `exit(1)` 兜住。

use std::sync::Arc;

use async_trait::async_trait;
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::voucher::voucher_multi::{self, ExplicitLine, ExplicitVoucherInput};
use crate::business::erp::voucher::{self, AccountRef, ReversalInput};
use cloudpivot_kernel::business::shared::voucher_port::{
    self, AccountPick, ExplicitVoucherReq, ReversalSpec, VoucherBrief, VoucherLine, VoucherWriter,
};

/// 声明了 `DEPT` 辅助核算的科目编码（决定凭证行要不要带部门）。
///
/// # 为什么这条 SQL 从 HR 搬到了这里（不是「顺手」）
///
/// `biz_account` 是**凭证域的表**：`aux_dimensions` 的取值口径（`DEPT` 怎么写、空值怎么算、
/// 停用科目算不算）都归这里。原先它写在 `hr/salary_bridge.rs` 里，等于让 HR 认 ERP 的表名
/// —— 而那正是 `hr → erp` 回边的形状之一（表级直连，依赖报告里只表现为 import，看不见 SQL）。
/// 搬到实现侧后，「谁能读科目表」重新变成一句话能回答的事。
pub const DEPT_AWARE_ACCOUNTS_SQL: &str = "SELECT account_code FROM biz_account \
      WHERE deleted_at IS NULL AND status = 1 AND upper(coalesce(aux_dimensions, '')) LIKE '%DEPT%'";

/// 把凭证域的 [`AccountRef`] 翻成跨域快照 [`AccountPick`]。
fn pick(account: AccountRef) -> AccountPick {
    AccountPick {
        id: account.id,
        code: account.code,
        name: account.name,
    }
}

/// 反向：跨域快照 → 凭证域的分录科目。
fn account_ref(pick: AccountPick) -> AccountRef {
    AccountRef {
        id: pick.id,
        code: pick.code,
        name: pick.name,
    }
}

/// 凭证写入接缝的实现（每个方法都是「一次委派」，见模块注释的等价性表）。
pub struct ErpVoucherWriter;

impl ErpVoucherWriter {
    /// 自报名字：启动日志里打这一行，排障时能一眼看出装的是谁。
    pub const NAME: &'static str = "erp::voucher（凭证台账）";
}

#[async_trait]
impl VoucherWriter for ErpVoucherWriter {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn dept_aware_account_codes(&self, tx: &Transaction) -> DbResult<Vec<String>> {
        // SCOPE-EXEMPT: 科目表是全局主数据，这里列编码 + 维度，不按数据范围裁。
        let rows = tx.query_sql(DEPT_AWARE_ACCOUNTS_SQL.to_string(), vec![]).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row.get_typed::<String>("account_code")?);
        }
        Ok(out)
    }

    async fn leaf_account(&self, tx: &Transaction, code: &str) -> DbResult<AccountPick> {
        voucher_multi::leaf_required(tx, code).await.map(pick)
    }

    async fn rule_credit_account(&self, tx: &Transaction, rule_code: &str) -> DbResult<Option<AccountPick>> {
        Ok(voucher_multi::rule_accounts(tx, rule_code)
            .await?
            .map(|accounts| pick(accounts.credit)))
    }

    async fn post_explicit(&self, tx: &Transaction, req: ExplicitVoucherReq<'_>) -> DbResult<Uuid> {
        let lines: Vec<ExplicitLine> = req
            .lines
            .iter()
            .map(|line: &VoucherLine| ExplicitLine {
                account: account_ref(line.account.clone()),
                debit: line.debit,
                credit: line.credit,
                aux_dept_id: line.aux_dept_id,
                summary: line.summary.clone(),
            })
            .collect();
        voucher_multi::post_explicit_lines(
            tx,
            ExplicitVoucherInput {
                source_type: req.source_type,
                source_id: req.source_id,
                source_no: req.source_no,
                voucher_date: req.voucher_date,
                summary: req.summary,
                lines: &lines,
                remark: req.remark,
                operator: req.operator,
                now: req.now,
            },
        )
        .await
    }

    async fn voucher_no(&self, tx: &Transaction, voucher_id: Uuid) -> DbResult<Option<String>> {
        Ok(voucher::find_voucher(tx, voucher_id).await?.map(|row| row.voucher_no))
    }

    async fn lock_voucher(&self, tx: &Transaction, voucher_id: Uuid) -> DbResult<Option<VoucherBrief>> {
        Ok(voucher::find_voucher_locked(tx, voucher_id)
            .await?
            .map(|row| VoucherBrief {
                id: row.id,
                voucher_no: row.voucher_no,
                source_type: row.source_type,
                source_id: row.source_id,
            }))
    }

    async fn reverse_posted(
        &self,
        tx: &Transaction,
        voucher_id: Uuid,
        spec: ReversalSpec<'_>,
        operator: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> DbResult<Uuid> {
        // 锁 + 冲在同一事务里：`SELECT … FOR UPDATE` 在本事务已持锁时是幂等的，
        // 所以调用方（HR）先前用 `lock_voucher` 核对来源的那把锁不会被重复获取。
        let Some(original) = voucher::find_voucher_locked(tx, voucher_id).await? else {
            // `NotFound` 由调用方翻成自己的文案（HR 要说清「单据记着这张凭证但它不存在」）。
            return Err(DbError::NotFound);
        };
        voucher::reverse_posted(
            tx,
            &original,
            ReversalInput {
                reason: spec.reason,
                voucher_date: spec.voucher_date,
            },
            operator,
            now,
        )
        .await
    }
}

/// 把实现装进 `shared::voucher_port` 的注册表（启动期调用一次）。
///
/// 失败 = 重复安装（编程错误），由 `bootstrap::register_voucher_port` 按
/// prod `exit(1)` / 其它 profile `panic` 处理。
pub fn install() -> std::result::Result<(), String> {
    voucher_port::install(Arc::new(ErpVoucherWriter))
}

#[cfg(test)]
mod tests {
    //! 离线用例：SQL 形状 + 上下文转换 + 名字。

    use super::*;

    /// ① 部门维度 SQL 的三个口径：软删 / 启用 / `DEPT` 大写匹配。
    #[test]
    fn dept_aware_sql_keeps_soft_delete_and_status() {
        assert!(DEPT_AWARE_ACCOUNTS_SQL.contains("FROM biz_account"));
        assert!(DEPT_AWARE_ACCOUNTS_SQL.contains("deleted_at IS NULL"));
        assert!(DEPT_AWARE_ACCOUNTS_SQL.contains("status = 1"));
        assert!(
            DEPT_AWARE_ACCOUNTS_SQL.contains("upper(coalesce(aux_dimensions, '')) LIKE '%DEPT%'"),
            "维度要大小写无关、空值不算：{DEPT_AWARE_ACCOUNTS_SQL}"
        );
        assert_eq!(DEPT_AWARE_ACCOUNTS_SQL.matches('?').count(), 0, "这条 SQL 不带参数");
    }

    /// ② 科目快照 ↔ 凭证域科目**双向无信息丢失**（三个字段一一对应）。
    #[test]
    fn account_pick_round_trips() {
        let snapshot = AccountPick {
            id: Uuid::nil(),
            code: "5602.10".to_string(),
            name: "工资薪金".to_string(),
        };
        assert_eq!(
            account_ref(snapshot.clone()),
            AccountRef {
                id: snapshot.id,
                code: snapshot.code.clone(),
                name: snapshot.name.clone(),
            }
        );
        assert_eq!(pick(account_ref(snapshot.clone())), snapshot);
    }

    /// ③ 名字自报（启动日志里认得出装的是谁）。
    #[test]
    fn writer_reports_its_name() {
        assert!(ErpVoucherWriter::NAME.contains("voucher"));
        assert_eq!(ErpVoucherWriter.name(), ErpVoucherWriter::NAME);
    }
}
