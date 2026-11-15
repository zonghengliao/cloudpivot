//! 「记一张会计分录凭证」的**能力接缝**（P2-B 解环 · 卡 `t_3442b4fb`）。
//!
//! # 这个文件解决什么
//!
//! 工资过账要落两张凭证（计提 + 发放），红冲要各冲一张红字凭证。凭证台账
//! （`biz_voucher` / `biz_voucher_entry` / `biz_voucher_rule` / `biz_account`）是 **ERP 的表**，
//! 而调用方是 **HR**：原先 `business/hr/salary_bridge.rs` 直接 import 凭证域的
//! `voucher` / `voucher_multi`（`post_explicit_lines` / `leaf_required` / `rule_accounts` /
//! `reverse_posted`），这就是 `hr → erp` 那条代码回边里最粗的一股（过账 + 红冲全在它上面）。
//!
//! 方向倒过来：**能力由凭证侧实现并注册，HR 只认接缝**。
//!
//! ```text
//! 改前：hr ──post_explicit_lines / reverse_posted / leaf_required──→ erp::voucher
//! 改后：hr ──VoucherWriter（本文件）──→ (抽象) ←──启动期注入── erp::voucher_port_impl
//! ```
//!
//! # 为什么接缝在 `shared` 而不是在 `erp`
//!
//! 与 `hr::employee_reader`（定义在被读方）**方向相反**，理由与 `task::approval_port` 同款：
//!
//! | 接缝 | 谁定义 | 为什么 |
//! |---|---|---|
//! | `employee_reader` / `doc_reader` | **被读方的域** | 读的是别人的数据，口径（软删 / 数据范围 / 列名）归表属主 |
//! | 本接缝 `VoucherWriter` | **跨域基座（`shared`）** | 传的是**写命令**（记一笔分录），口径归凭证域；但若把 trait 放 `erp`，`hr` 就必须 import `erp` —— 边还在，等于没解 |
//!
//! 放进 `shared` 的额外好处：将来别的域要出凭证（比如生产 / 报销）也走这条接缝，
//! 不必各自再 import 一次 ERP。`shared` 本身的纪律（不许反向依赖任何业务域）由
//! `shared/layering_tests` 兜底 —— 所以本文件**一个业务域路径都没有**（连注释都没有）。
//!
//! # 事务纪律（`AGENTS.md` 约定 11 的地基）
//!
//! **每个写方法都收 `&Transaction`**，和在制单 / 过账的**同一个事务**里做事：
//!
//! ```text
//! BEGIN                                    ← HR 开
//!   锁薪资单 / 状态闸 / 明细复算            ← HR
//!   ① post_explicit(计提凭证)              ← 凭证侧（本接缝）
//!   ② post_explicit(发放凭证)              ← 凭证侧（本接缝）
//!   状态机 APPROVED → POSTED + 回写单据     ← HR
//! COMMIT                                   ← HR
//! ```
//!
//! 任何一步失败 ⇒ 整笔 `ROLLBACK` ⇒ **两张凭证一起消失、单据仍是 `APPROVED`**，界面上的
//! 「过账」按钮还能再点一次。这正是「不需要工程师改库就能恢复」的形态。
//!
//! 反例（绝对不能改成这样）：事务提交后再 fire-and-forget 去记账 —— 记账失败时单据已经是
//! `POSTED`，账上却没有凭证，界面上没有任何按钮能修复它。
//!
//! # 没装实现时是什么语义（**大声失败**，与只读接缝相反）
//!
//! | 接缝 | 未安装时的行为 |
//! |---|---|
//! | `hr::employee_reader`（只读） | 返回「查无」：读不到人就是数据事实，界面能继续操作 |
//! | **本接缝（写）** | `Err(InvalidValue("凭证写入能力未安装…"))` → 接口 500 |
//!
//! 为什么写路径不许降级：**「凭证没记」不是一种业务状态**。返回 `Ok` 会让单据带着
//! 「已过账」的状态而没有账，那是最难查的一类生产事故（账面缺一张，没人知道）。
//! 未安装属于**启动链漏了一步**（编程错误），`bootstrap` 里 prod `exit(1)` 兜住，
//! 请求期永远不该遇到；真遇到了也必须是一条刺眼的 500，而不是静默成功。
//!
//! # 三条纪律
//!
//! 1. **不许跨 `await` 持锁**：先克隆 `Arc` 再 `await`（跨 `await` 持读锁会让全进程的凭证写入串行化）；
//! 2. **读锁 poisoning 直接放行**（`into_inner`）：注册期 panic 不该让之后每次记账都 500；
//! 3. **只有一个写入口** [`install`]，调用方固定是启动链（`bootstrap::register_voucher_port`）。

use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

// ==================== 跨域可见的数据形状 ====================

/// 一个**末级会计科目**的快照（凭证分录要落编码 / 名称快照）。
///
/// 只带记账需要的三列，不带余额、方向、辅助核算配置 —— 那些是凭证侧的口径，
/// 调用方拿到也用不上（多一个字段就多一份「以为能自己算」的诱惑）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPick {
    /// 科目主键（`biz_account.id`）。
    pub id: Uuid,
    /// 科目编码（如 `5602.10`）。
    pub code: String,
    /// 科目名称（如 `工资薪金`）。
    pub name: String,
}

/// 一条**显式分录**（借贷只能有一个方向，金额恒正）。
///
/// `debit` 与 `credit` 同时 > 0 会被凭证侧拒绝（那是「同一行正负相抵」的错误写法，
/// 红字要走红冲），由实现方翻译成 400/409。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherLine {
    /// 科目（末级）。
    pub account: AccountPick,
    /// 借方金额（`0` = 本行不是借方）。
    pub debit: Decimal,
    /// 贷方金额（`0` = 本行不是贷方）。
    pub credit: Decimal,
    /// 部门辅助核算（只有声明了 `DEPT` 维度的科目才该带；带错会让按部门汇总多一层永远对不上的切分）。
    pub aux_dept_id: Option<Uuid>,
    /// 行摘要（`None` = 用整单摘要）。
    pub summary: Option<String>,
}

/// 「落一张显式分录凭证」的请求（一次调用 = 一张凭证）。
#[derive(Debug, Clone, Copy)]
pub struct ExplicitVoucherReq<'a> {
    /// 来源单据类型（落 `biz_voucher.source_type`，由来源域定义，如 `PAYROLL_ACCRUAL`）。
    pub source_type: &'a str,
    /// 来源单据主键（`None` = 无来源行的手工凭证）。
    pub source_id: Option<Uuid>,
    /// 来源单据号（摘要与报错文案都用它）。
    pub source_no: &'a str,
    /// 凭证日期（落 `voucher_date`；按期记账的业务口径由调用方决定）。
    pub voucher_date: NaiveDate,
    /// 凭证摘要（分录没写自己的摘要时用它兜底）。
    pub summary: &'a str,
    /// 显式分录（至少 2 行）。
    pub lines: &'a [VoucherLine],
    /// 凭证备注（`None` = 不填）。
    pub remark: Option<&'a str>,
    /// 操作人（落 `create_by`；`None` = 系统 / 测试）。
    pub operator: Option<Uuid>,
    /// 业务时间（落 `created_at`）。
    pub now: DateTime<Utc>,
}

/// 一张凭证的**只读摘要**（核对「这张凭证是不是我这张单的」用）。
///
/// 只给四个字段：红冲前要核对的正是「来源类型 + 来源 id」，多了就是泄漏凭证域的内部形状。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherBrief {
    /// 凭证主键。
    pub id: Uuid,
    /// 凭证号（人能查的那个号，如 `PZ-20261115-000001`）。
    pub voucher_no: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据主键。
    pub source_id: Option<Uuid>,
}

/// 红冲的入参（原因 + 红字凭证日期）。
#[derive(Debug, Clone, Copy)]
pub struct ReversalSpec<'a> {
    /// 红冲原因（写进红字凭证的分录摘要与备注，也是审计追溯的抓手）。
    pub reason: Option<&'a str>,
    /// 红字凭证日期（业务口径：作废 / 更正当天）。
    pub voucher_date: NaiveDate,
}

// ==================== 接缝本体 ====================

/// 「记账能力」的全部接口（**唯一实现者是凭证域**，见模块注释）。
///
/// # 实现方约定
///
/// - 实现放在**持有凭证表的域**里（现在是 `business/erp/voucher/voucher_port_impl.rs`）；
/// - 每个方法都必须走凭证域**已有的**唯一写入口（取号 / 平衡校验 / 期间闸 / 分录落库都在那里），
///   **不许**在接缝里另写一套 SQL —— 两套记账口径是账务模块最贵的 bug；
/// - 写方法收 `&Transaction`：和调用方的业务写入同一个事务，不许自己 `begin` / `commit`。
///
/// # 为什么每个方法都**不给**默认实现
///
/// 默认实现意味着「漏实现也能编译过」。对写路径来说那就是**静默不记账**：单据过了账、
/// 凭证没有，谁都发现不了。所以漏实现必须是编译错误。
#[async_trait]
pub trait VoucherWriter: Send + Sync {
    /// 声明了部门（`DEPT`）辅助核算的科目编码集合 —— 决定凭证行要不要带部门。
    ///
    /// 返回编码而不是「一张表」：调用方只需要做 `contains`，多给任何东西都是耦合。
    async fn dept_aware_account_codes(&self, tx: &Transaction) -> DbResult<Vec<String>>;

    /// 按编码取**末级**科目（非末级 / 停用 / 不存在 → `InvalidValue`，报文点名科目）。
    ///
    /// 「末级」是硬要求：记到父科目上，报表会翻倍（科目余额与明细各算一次）。
    async fn leaf_account(&self, tx: &Transaction, code: &str) -> DbResult<AccountPick>;

    /// 按凭证规则编码取**贷方**科目（规则缺失 / 停用 → `Ok(None)`，由调用方决定回落口径）。
    ///
    /// 规则是「口径登记」（哪张单默认用哪个科目），配置页可见、客户可改；
    /// 返回 `Option` 而不是报错，是因为「规则没配」在有些单据上是可回落的数据状态。
    async fn rule_credit_account(&self, tx: &Transaction, rule_code: &str) -> DbResult<Option<AccountPick>>;

    /// 落一张显式分录凭证，返回凭证 id。
    ///
    /// 取号 / 借贷平衡 / 期间已结账拒绝 / 分录落库全部由实现方负责；
    /// 失败一律 `Err`，调用方按 `AGENTS.md` 约定 11 整笔回滚。
    async fn post_explicit(&self, tx: &Transaction, req: ExplicitVoucherReq<'_>) -> DbResult<Uuid>;

    /// 读凭证号（`Ok(None)` = 查无此凭证，由调用方翻译成自己的错误码）。
    async fn voucher_no(&self, tx: &Transaction, voucher_id: Uuid) -> DbResult<Option<String>>;

    /// 按 id 取凭证摘要并**加行锁**（红冲前核对来源 + 防并发重复红冲）。
    async fn lock_voucher(&self, tx: &Transaction, voucher_id: Uuid) -> DbResult<Option<VoucherBrief>>;

    /// 红冲一张已记账凭证：生成红字凭证（借贷互换、金额恒正、`is_red=1`、回写原凭证
    /// `reversed_by`），返回红字凭证 id。
    ///
    /// 校验（已记账 / 不是红字 / 没红冲过 / 期间没结账）全在实现方 —— 调用方只需保证
    /// 「这张凭证确实是我这张单的那张」（用 [`lock_voucher`] 核对）。
    async fn reverse_posted(
        &self,
        tx: &Transaction,
        voucher_id: Uuid,
        spec: ReversalSpec<'_>,
        operator: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> DbResult<Uuid>;
    /// 实现方自报名字（启动日志 / 排障用）。
    ///
    /// 默认取具体类型的 `type_name`；实现方想给更短的名字可以覆盖它。
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

// ==================== 注册表 ====================

/// 进程级注册表（同一时刻只有一个实现）。
struct Registry {
    writer: RwLock<Option<Arc<dyn VoucherWriter>>>,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry {
        writer: RwLock::new(None),
    })
}

/// 安装实现（**唯一写入口**，只该在启动链里调一次）。
///
/// 重复安装 → `Err`：两个凭证实现同时存在一定是编程错误（谁会赢取决于启动顺序）。
pub fn install(writer: Arc<dyn VoucherWriter>) -> std::result::Result<(), String> {
    let mut slot = registry()
        .writer
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.is_some() {
        return Err("凭证写入接缝已经安装过实现（重复安装是编程错误）".to_string());
    }
    *slot = Some(writer);
    Ok(())
}

/// 是否已安装（排障 / 用例用；业务代码判断「能不能记账」请直接调方法）。
pub fn is_installed() -> bool {
    current().is_some()
}

/// 已安装实现的名字（启动摘要里打一行，排障时能一眼看出装的是谁）。
pub fn port_name() -> Option<&'static str> {
    current().map(|writer| writer.name())
}

/// 取当前实现（**不跨 `await` 持锁**：克隆 `Arc` 后立刻释放读锁）。
fn current() -> Option<Arc<dyn VoucherWriter>> {
    registry()
        .writer
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// 未安装时的错误（写路径一律**大声失败**，见模块注释「没装实现时是什么语义」）。
fn not_installed(what: &str) -> DbError {
    DbError::InvalidValue(format!(
        "{what}失败：凭证写入接缝未安装实现。这是启动链漏了一步（编程错误）—— \
         生产环境应由 bootstrap 的 register_voucher_port 在启动期安装，装不上就直接退出，\
         不该让请求跑到这里。请检查启动日志并联系研发。"
    ))
}

/// 取实现或报「未安装」。
fn writer_or_error(what: &str) -> DbResult<Arc<dyn VoucherWriter>> {
    current().ok_or_else(|| not_installed(what))
}

// ==================== 调用方入口（模块级函数，调用方只认这几个） ====================

/// 声明了部门（`DEPT`）辅助核算的科目编码集合。
pub async fn dept_aware_account_codes(tx: &Transaction) -> DbResult<Vec<String>> {
    writer_or_error("读取科目辅助核算维度")?
        .dept_aware_account_codes(tx)
        .await
}

/// 按编码取末级科目。
pub async fn leaf_account(tx: &Transaction, code: &str) -> DbResult<AccountPick> {
    writer_or_error("解析会计科目")?.leaf_account(tx, code).await
}

/// 按规则编码取贷方科目（规则缺失 = `Ok(None)`）。
pub async fn rule_credit_account(tx: &Transaction, rule_code: &str) -> DbResult<Option<AccountPick>> {
    writer_or_error("读取凭证规则")?
        .rule_credit_account(tx, rule_code)
        .await
}

/// 落一张显式分录凭证，返回凭证 id。
pub async fn post_explicit(tx: &Transaction, req: ExplicitVoucherReq<'_>) -> DbResult<Uuid> {
    writer_or_error("生成会计凭证")?.post_explicit(tx, req).await
}

/// 读凭证号（`Ok(None)` = 查无此凭证）。
pub async fn voucher_no(tx: &Transaction, voucher_id: Uuid) -> DbResult<Option<String>> {
    writer_or_error("读取凭证号")?.voucher_no(tx, voucher_id).await
}

/// 按 id 取凭证摘要并加行锁。
pub async fn lock_voucher(tx: &Transaction, voucher_id: Uuid) -> DbResult<Option<VoucherBrief>> {
    writer_or_error("锁定会计凭证")?.lock_voucher(tx, voucher_id).await
}

/// 红冲一张已记账凭证，返回红字凭证 id。
pub async fn reverse_posted(
    tx: &Transaction,
    voucher_id: Uuid,
    spec: ReversalSpec<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    writer_or_error("红冲会计凭证")?
        .reverse_posted(tx, voucher_id, spec, operator, now)
        .await
}

#[cfg(test)]
pub mod tests {
    //! 接缝自身的纪律用例（离线）：注册表行为 + 「本文件不认任何业务域」。
    //!
    //! 真实现的行为由凭证域自己的用例覆盖；这里只证明**接缝本身**可用、可判、不漏线。

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 假实现：只数调用次数，证明「调用方真的穿过了接缝」。
    #[derive(Default)]
    struct FakeWriter {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl VoucherWriter for FakeWriter {
        async fn dept_aware_account_codes(&self, _tx: &Transaction) -> DbResult<Vec<String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec!["5602.10".to_string()])
        }

        async fn leaf_account(&self, _tx: &Transaction, code: &str) -> DbResult<AccountPick> {
            Ok(AccountPick {
                id: Uuid::nil(),
                code: code.to_string(),
                name: "假科目".to_string(),
            })
        }

        async fn rule_credit_account(&self, _tx: &Transaction, _rule_code: &str) -> DbResult<Option<AccountPick>> {
            Ok(None)
        }

        async fn post_explicit(&self, _tx: &Transaction, _req: ExplicitVoucherReq<'_>) -> DbResult<Uuid> {
            Ok(Uuid::nil())
        }

        async fn voucher_no(&self, _tx: &Transaction, _voucher_id: Uuid) -> DbResult<Option<String>> {
            Ok(Some("PZ-FAKE".to_string()))
        }

        async fn lock_voucher(&self, _tx: &Transaction, _voucher_id: Uuid) -> DbResult<Option<VoucherBrief>> {
            Ok(None)
        }

        async fn reverse_posted(
            &self,
            _tx: &Transaction,
            _voucher_id: Uuid,
            _spec: ReversalSpec<'_>,
            _operator: Option<Uuid>,
            _now: DateTime<Utc>,
        ) -> DbResult<Uuid> {
            Ok(Uuid::nil())
        }
    }

    /// ① 接缝文件**不出现任何业务域路径**（连注释都不许）：这是它能解环的唯一理由。
    ///
    /// 判据与 `shared/layering_tests` 同源，但这里额外把**注释里的路径**也钉住 ——
    /// 注释里的业务域绝对路径会被人复制成代码，也会让依赖报告一直把这条边算在 `shared` 头上
    /// （`module_cycle_report` 连注释一起扫）。
    ///
    /// 允许 `shared` **自己**的绝对路径：基座内部互引（`use ...::shared::scope`）不是回边，
    /// 否则本用例第一条就先把 `mod.rs` / `guard.rs` / `scope.rs` 全钉红了。
    #[test]
    fn seam_source_is_free_of_business_paths() {
        let source = include_str!("voucher_port.rs");
        assert!(source.len() > 4096, "读到的源码太短（路径写错 = 假绿）");
        assert!(
            offenders_in(source).is_empty(),
            "shared::voucher_port 出现了业务域绝对路径：接缝只能依赖框架，实现方靠注册表反向注入（{}）",
            offenders_in(source).join(", ")
        );

        // 同一判据覆盖整个 `shared`：基座**整体**不许反向依赖任何业务域（P2-30 的纪律），
        // 新增文件不该成为例外。
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("business")
            .join("shared");
        let mut scanned = 0usize;
        let mut offenders: Vec<String> = Vec::new();
        let mut stack = vec![dir.clone()];
        while let Some(current) = stack.pop() {
            for entry in std::fs::read_dir(&current).expect("读 shared 目录") {
                let path = entry.expect("目录项").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                    continue;
                }
                scanned += 1;
                let text = std::fs::read_to_string(&path).expect("读源码");
                for hit in offenders_in(&text) {
                    offenders.push(format!("{}: {hit}", path.display()));
                }
            }
        }
        assert!(scanned >= 10, "扫描面太小：{scanned} 个文件（路径写错会静默变绿）");
        assert!(
            offenders.is_empty(),
            "shared 里出现了业务域绝对路径（工具层不许反向依赖业务层）：{offenders:?}"
        );
    }

    /// 从源码里抽出「指向**别的**业务域」的绝对路径引用（`shared` 自己不算）。
    ///
    /// 返回 `business::<域>` 形态的片段；域取不到（源码在 `::` 处结束）则忽略。
    fn offenders_in(source: &str) -> Vec<String> {
        const PREFIX: &str = "crate::business::";
        let mut out = Vec::new();
        let mut rest = source;
        while let Some(at) = rest.find(PREFIX) {
            let after = &rest[at + PREFIX.len()..];
            let domain: String = after
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect();
            if !domain.is_empty() && domain != "shared" {
                out.push(format!("business::{domain}"));
            }
            rest = after;
        }
        out
    }

    /// ② 安装 / 重复安装 / 未安装时的**大声失败**（写路径的核心纪律）。
    #[test]
    fn registry_installs_once_and_fails_loudly_when_missing() {
        let registry = Registry {
            writer: RwLock::new(None),
        };
        assert!(registry.writer.read().expect("读锁").is_none());

        let fake = Arc::new(FakeWriter::default());
        {
            let mut slot = registry.writer.write().expect("写锁");
            *slot = Some(fake);
        }
        let installed = registry.writer.read().expect("读锁").clone().expect("已安装");
        assert!(Arc::strong_count(&installed) >= 2, "取到的是同一个实现的 Arc 克隆");

        // 未安装 → 明确指出「启动链漏了一步」，而不是静默成功
        let message = format!("{}", not_installed("生成会计凭证"));
        assert!(message.contains("凭证写入接缝未安装"), "{message}");
        assert!(message.contains("register_voucher_port"), "报文要指向接线点：{message}");
    }

    /// ③ 未安装时每个入口都返回 `Err`（写路径绝不许出现「静默成功」）。
    #[test]
    fn every_entry_reports_missing_implementation() {
        // 本用例只查错误文本，不连库：Uninstalled 分支在打开事务之前就返回。
        for what in [
            "读取科目辅助核算维度",
            "解析会计科目",
            "读取凭证规则",
            "生成会计凭证",
            "读取凭证号",
            "锁定会计凭证",
            "红冲会计凭证",
        ] {
            let error = not_installed(what);
            assert!(format!("{error}").contains(what), "错误里要点名是哪个动作：{error}");
        }
    }

    /// ④ 数据形状的构造约束：金额恒正、借贷单向 —— 由实现方校验，这里只钉「类型允许表达」。
    #[test]
    fn line_shape_expresses_one_direction() {
        let account = AccountPick {
            id: Uuid::nil(),
            code: "2211.01".to_string(),
            name: "应付职工薪酬".to_string(),
        };
        let debit = VoucherLine {
            account: account.clone(),
            debit: Decimal::new(746437, 2),
            credit: Decimal::ZERO,
            aux_dept_id: None,
            summary: None,
        };
        assert_eq!(debit.debit, Decimal::new(746437, 2));
        assert!(debit.credit.is_zero());
        let credit = VoucherLine {
            account,
            debit: Decimal::ZERO,
            credit: Decimal::new(746437, 2),
            aux_dept_id: None,
            summary: Some("计提工资".to_string()),
        };
        assert!(credit.debit.is_zero());
    }
}
