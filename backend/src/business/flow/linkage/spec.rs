//! 跨单据联动：**白名单动作注册表**（【D2】流程配置化 · 阶段 4）。
//!
//! # 为什么要有白名单
//!
//! 「流程全可配」最容易走歪的一步是让用户在配置里写 SQL：那等于把数据库的
//! 任意读写权限发给每一条配置，数据安全直接归零。所以联动的**唯一**可变部分是
//! 「在哪个触发点、按什么顺序、用哪几个**预定义动作**、带什么参数」；
//! 动作本身是不可配置的代码。
//!
//! 每个动作 = 一个有明确语义的原子操作：固定的入参键集合（[`ActionSpec::params`]）、
//! 固定的事务归属（[`ActionSpec::phase`]）、固定的失败等级（[`ActionSpec::severity`]）。
//!
//! # 三层校验（本文件是第 1 层与第 2 层的判据源）
//!
//! | 层 | 在哪 | 判据 | 不过的后果 |
//! |----|------|------|-----------|
//! | 1 | 数据库 CHECK `ck_flow_linkage_action_code` | 动作码必须是白名单 | 写库被拒 |
//! | 1 | 数据库 CHECK `ck_flow_linkage_no_sql_key` | 参数键里不许有 sql/statement/query… | 写库被拒 |
//! | 2 | [`validate_params`]（写配置时调用） | 键集合 / 必填 / 类型 / 枚举值 | `400` 拒绝保存 |
//! | 3 | `engine` 运行时再校验一次 | target 存在 + 参数合法 | ERROR 日志 + 跳过该动作 |
//!
//! 第 1 层的「动作码清单」写死在迁移里，与 [`ACTIONS`] 的一致性由单测
//! `migration_check_covers_every_action_code`（`include_str!` 读迁移原文）钉住——
//! 加动作只改代码不改迁移，测试立刻红。
//!
//! # 事务边界（卡面要求「在卡里说清」）
//!
//! 动作按 [`Phase`] 分两栏，**这是执行引擎实现事务语义的唯一依据**：
//!
//! - [`Phase::InTx`]：在**调用方自己的事务里**执行（`engine::run` 收 `&impl SqlExecutor`，
//!   不新开事务）。[`Severity::Critical`] 的动作失败 → `engine::run` 返回 `Err`，调用方整笔回滚；
//!   [`Severity::NonCritical`] 的动作失败 → 只记日志，事务继续。
//! - [`Phase::AfterCommit`]：**提交之后**才执行（`engine::run_deferred`），失败只记日志，
//!   永远不回滚业务（发通知失败不能把「已经过账」回滚掉）。
//!
//! 为什么不在 `InTx` 里再包一层事务：调用方（如 `stockin_handler::post_in_tx`）本身就在事务里，
//! PG 不支持嵌套事务，`SAVEPOINT` 只能局部回滚且会把「关键动作失败 → 整笔回滚」的语义
//! 变成「只有这一段回滚」，与卡面要求相反。所以「谁拥有事务」必须由**调用方**决定。
//!
//! # 实现的现状（第 1 步 + 第 2 步 + 【D3】第 3 步）
//!
//! [`ActionSpec::implemented`] 区分「已接管」与「占位」：
//!
//! - **已实现（4 个）**：`SEND_NOTICE`（第 2 步接管任务状态通知）、
//!   `UPDATE_RECEIVED_QTY`（P2-3 起语义=校验：累计收货量是派生值 `POSTED_RECEIVED_SQL`，
//!   本动作判「其他单据已过账收货 + 本次 ≤ 订购量」，作为关键动作事务语义的活样本）、
//!   `GENERATE_VOUCHER`（**D3 第 3 步**：复用 `voucher::create_by_rule`，
//!   与收货过账的旧路径是同一个函数、同一份取数与取号）、
//!   `CREATE_TASK`（**P1-FIX-5C**：复用 `task::model::create_task`，提交后建自由任务；
//!   出库过账 → 建复核任务是它的第一个真实用例）；
//! - **占位（7 个）**：入参契约已定、[`ActionSpec::reuse`] 写清了后续要复用的既有函数；
//!   在接管之前**不允许配置**（写入口直接 `400`），万一有人绕过 API 直接写库，
//!   运行时会打 ERROR + 跳过该动作，绝不半执行。
//!
//! # 铁律：动作实现只能复用，不能重写
//!
//! 每个 [`ActionSpec::reuse`] 都指向既有实现（模型函数 / handler 里的那一段）。
//! 重新写一份算法 = 双真相：同一个业务动作两套实现，改了一处漏一处，
//! 账会以「哪种路径先跑」的方式随机错。第 3 步接管财务动作时必须先抽公共函数再复用。

use std::result::Result;

use nik::uuid::Uuid;

use crate::business::shared::state_machine;

/// 参数类型（第 2 层校验用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// 文本（`max` = 最大字符数；入库前 trim）。
    Text { max: usize },
    /// UUID 字符串。
    Uuid,
    /// 枚举（值必须逐字命中白名单，全大写）。
    Enum(&'static [&'static str]),
    /// 布尔。
    Bool,
    /// 整数（闭区间）。
    Int { min: i64, max: i64 },
    /// 十进制数（JSON number 或数字字符串，按 `Decimal` 解析）。
    Decimal,
    /// UUID 列表。
    UuidList { max_items: usize },
}

impl ParamKind {
    /// 类型的中文说明（错误文案里用）。
    pub fn label(&self) -> String {
        match self {
            ParamKind::Text { max } => format!("文本（≤{max} 字符）"),
            ParamKind::Uuid => "UUID 字符串".to_string(),
            ParamKind::Enum(values) => format!("枚举（{}）", values.join(" / ")),
            ParamKind::Bool => "布尔".to_string(),
            ParamKind::Int { min, max } => format!("整数（{min}..={max}）"),
            ParamKind::Decimal => "十进制数".to_string(),
            ParamKind::UuidList { max_items } => format!("UUID 列表（≤{max_items} 项）"),
        }
    }
}

/// 一个动作参数的声明。
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    /// 键名（`action_params` 的键）。
    pub key: &'static str,
    /// 类型。
    pub kind: ParamKind,
    /// 是否必填。
    pub required: bool,
    /// 人读说明（配置页/文档用）。
    pub note: &'static str,
}

/// 执行阶段 = **事务边界**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 在调用方的事务里执行（失败可整笔回滚）。
    InTx,
    /// 事务提交之后执行（失败只记日志）。
    AfterCommit,
}

impl Phase {
    /// 落日志/文档用的短名。
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::InTx => "IN_TX",
            Phase::AfterCommit => "AFTER_COMMIT",
        }
    }
}

/// 失败等级：决定「动作失败」是回滚还是跳过。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// 关键动作：失败 → 整个状态变更回滚（写库存 / 生成凭证 / 改累计量）。
    Critical,
    /// 非关键动作：失败 → 只记日志，流程继续（发通知 / 建任务）。
    NonCritical,
}

impl Severity {
    /// 落日志/文档用的短名。
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Critical => "CRITICAL",
            Severity::NonCritical => "NON_CRITICAL",
        }
    }
}

/// 一个白名单动作的完整声明。
#[derive(Debug)]
pub struct ActionSpec {
    /// 动作码（唯一定址，落 `biz_flow_linkage_rule.action_code`）。
    pub code: &'static str,
    /// 人读名称。
    pub label: &'static str,
    /// 事务归属。
    pub phase: Phase,
    /// 失败等级。
    pub severity: Severity,
    /// 是否已接管（false = 占位：入参契约已定、执行时跳过、配置写入被拒）。
    pub implemented: bool,
    /// 参数声明（键集合即白名单）。
    pub params: &'static [ParamSpec],
    /// **第 3 步接管时要复用的既有实现**（文件::函数 + 位置），不许另写一套。
    pub reuse: &'static str,
}

impl ActionSpec {
    /// 取某个参数的声明。
    pub fn param(&self, key: &str) -> Option<&'static ParamSpec> {
        self.params.iter().find(|param| param.key == key)
    }

    /// 允许的键（错误文案 / 文档用）。
    pub fn allowed_keys(&self) -> String {
        self.params
            .iter()
            .map(|param| {
                if param.required {
                    param.key.to_string()
                } else {
                    format!("{}?", param.key)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

// ==================== 触发点白名单 ====================

/// 任务（不在状态机实体注册表里，但有自己的状态流转）。
pub const TRIGGER_TASK: &str = "TASK";

/// 审批实例（`biz_flow_instance`；第 3 步接管「审批通过 → 改单据状态」时用）。
pub const TRIGGER_FLOW_INSTANCE: &str = "FLOW_INSTANCE";

/// 允许作为 `trigger_entity` 的取值。
///
/// 前 10 个与 [`state_machine`] 的实体注册表逐字一致（由单测
/// `triggers_cover_state_machine_entities` 钉住），后 2 个是本模块补充的：
/// `TASK`（任务）与 `FLOW_INSTANCE`（审批实例）。
pub const TRIGGERS: &[&str] = &[
    state_machine::ENTITY_STOCK_IN,
    state_machine::ENTITY_STOCK_OUT,
    state_machine::ENTITY_TRANSFER,
    state_machine::ENTITY_PURCHASE_ORDER,
    state_machine::ENTITY_PAYABLE,
    state_machine::ENTITY_VOUCHER,
    state_machine::ENTITY_PERIOD,
    state_machine::ENTITY_YEAR_END,
    state_machine::ENTITY_CONTRACT,
    // ARCH-3 DC-14：盘点单过账 / 作废也是「状态变更之后要做什么」的合法触发点
    state_machine::ENTITY_STOCK_TAKE,
    // ARCH-6 第五章：固定资产的停用 / 启用 / 处置 / 作废处置都要能挂联动
    state_machine::ENTITY_FIXED_ASSET,
    // ARCH-6 §5.5.5：折旧批次的过账 / 红冲同样是要「状态变更之后做什么」的触发点
    state_machine::ENTITY_DEPRECIATION,
    // ARCH-6 第四章：报销单的提交 / 通过 / 付清 / 作废都要能挂联动
    // （典型用法：「报销通过后给财务发待付款提醒」「作废后通知报销人」）
    state_machine::ENTITY_EXPENSE,
    TRIGGER_TASK,
    TRIGGER_FLOW_INSTANCE,
];

/// 触发点是否在白名单里。
pub fn is_known_trigger(entity: &str) -> bool {
    TRIGGERS.contains(&entity)
}

// ==================== 触发点接线状态（P1-FIX-5C） ====================

/// **一个触发点到底有没有人调引擎**（白名单 ≠ 接线）。
///
/// # 为什么白名单之外还要一张接线表
///
/// 白名单 [`TRIGGERS`] 回答的是「这个单据类型**允许**作为触发点」（数据库 CHECK、
/// `model::upsert_rule` 的校验都按它判）。它**不**回答「代码里真的有人在这个状态变更点
/// 调 [`crate::business::flow::linkage::engine`]」——P1-FIX-5B 实测发现这两件事严重不等：
/// 15 个白名单触发点里只有 2 个真的接了线，其余 13 个上配规则**能保存、能启用、永远不触发**。
/// 这是一种最坏的「看起来配好了」：管理员以为生效了，业务上什么都没发生，而且没有任何报错。
///
/// 所以每个触发点必须额外声明 [`TriggerSpec::wired`]：
///
/// - `true` = 该触发点的状态变更点里有真实调用（[`TriggerSpec::proofs`] 逐条给出
///   「文件 + 文件里必须出现的调用片段」）；
/// - `false` = 没人调引擎 ⇒ 写规则的接口要求显式 `allow_unwired`、配置页标注「未接线、
///   暂不会触发」、预览里给警告。**不许静默保存**。
///
/// # 漂移守卫（本表的 `wired` 不是手抄的自证）
///
/// 单测 `tests_wiring::wired_triggers_match_call_sites` 会在测试时扫描
/// `src/business` 下**全部生产代码**，找出所有调用联动引擎入口的文件：
///
/// 1. 每个 `wired = true` 的触发点，其 [`TriggerSpec::proofs`] 里点名的文件必须真的存在
///    且真的含该片段（删了调用 → 红）；
/// 2. 反过来，扫出来的「调用引擎的文件集合」必须**恰好等于** wired 触发点点名的文件集合
///    （偷偷多接一个触发点却忘了改本表 → 红）。
///
/// 这里刻意不把触发点写进 `TRIGGER_SPECS` 以外的地方：前端下发的 `wired` / `wiring`
/// 字段、写入口的拦截、单测的判据全部读这张表，**一份真相**。
#[derive(Debug)]
pub struct TriggerSpec {
    /// 触发实体（与 [`TRIGGERS`] 逐字一致，顺序也一致，由单测钉住）。
    pub entity: &'static str,
    /// 触发状态的中文名（`编码 → 中文`，P1-FIX-12A）。
    ///
    /// # 为什么中文名要放在后端
    ///
    /// 配置页的「触发状态」下拉原本直接拿 [`TriggerSpec`] 的状态**编码**当 label，
    /// 于是管理员看到的是 `POSTED / DOING / DRAFT`——「已过账 / 进行中 / 草稿」得靠背。
    /// 而这些中文名在各自的单据模块里**早就有一份了**（`stockin_base::status_label`
    /// 等 14 个函数 + 任务状态枚举），前端的 `status-map.ts` 又是第三份：
    /// 同一份文案三处维护，改一处漏两处。所以字典由后端下发，前端只做「编码 → 中文」
    /// 的一次映射（编码仍随 `value` 一起下发，排障时对着日志还能认）。
    ///
    /// # 口径（不许另造句）
    ///
    /// 每个中文名逐字取自该单据自己的状态机定义（单测 `status_labels_match_doc_modules`
    /// 直接调那些函数对拍，不是手抄自证）；顺序按**生命周期**排，不按字母。
    /// 覆盖性由单测 `status_labels_cover_known_statuses` 双向钉住：
    /// 少一项（下拉缺中文）或多项（写了个状态机里不存在的状态）都会红。
    pub status_labels: &'static [(&'static str, &'static str)],
    /// 是否已接线（`false` = 配了规则永远不会触发）。
    pub wired: bool,
    /// `wired = true` → 接线位置（`文件::函数`，说明「凭什么算接线」）；
    /// `wired = false` → 未接线的原因 + 接的时候要动哪里。配置页直接显示这段文字。
    pub wiring: &'static str,
    /// 漂移守卫的证据对：`(相对 backend/ 的文件路径, 文件里必须出现的片段)`；未接线 = 空。
    pub proofs: &'static [(&'static str, &'static str)],
}

/// 触发点接线表（顺序与 [`TRIGGERS`] 一致）。
///
/// 当前 **7/15 已接线**（`STOCK_IN` · `STOCK_OUT` · `STOCK_TRANSFER` · `PURCHASE_ORDER` ·
/// `PAYABLE` · `STOCK_TAKE` · `TASK`），8 个未接线；逐条原因见
/// `docs/architecture/P1-FIX-5C-联动触发点接线清单.md`。
pub const TRIGGER_SPECS: &[TriggerSpec] = &[
    TriggerSpec {
        entity: state_machine::ENTITY_STOCK_IN,
        status_labels: &[
            ("DRAFT", "草稿"),
            ("PENDING_POST", "过账审批中"),
            ("POSTED", "已过账"),
            ("PENDING_CANCEL", "作废审批中"),
            ("CANCELLED", "已作废"),
        ],
        wired: true,
        wiring: "已接线：erp/stockin_handler_post.rs::post_in_tx 第 ⑦ 步走凭证专用路由 \
                 (linkage::engine::stockin_voucher) + 第 ⑧ 步跑通用引擎 \
                 run_excluding(&[GENERATE_VOUCHER])，commit 后 post_stock_in / \
                 post_after_approval 调 run_stockin_deferred_linkage → run_deferred",
        proofs: &[
            (
                "src/business/erp/stockin_handler_post.rs",
                "linkage::engine::run_excluding",
            ),
            (
                "src/business/erp/stockin_handler_post.rs",
                "linkage::engine::stockin_voucher",
            ),
            (
                "src/business/erp/stockin_handler_post.rs",
                "linkage::engine::run_deferred",
            ),
        ],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_STOCK_OUT,
        status_labels: &[
            ("DRAFT", "草稿"),
            ("PENDING_POST", "待过账"),
            ("POSTED", "已过账"),
            ("CANCELLED", "已作废"),
        ],
        wired: true,
        wiring: "已接线：erp/stockout_handler.rs::post_out_tx 第 ⑥ 步跑通用引擎 \
                 run_excluding(spec::STOCKOUT_EXCLUDED_ACTIONS)（库存 / 成本 / 凭证由 ③⑤ \
                 专用路径落账，必须排除，否则重复扣库存 + 一张单两张凭证），commit 后 \
                 post_stock_out 调 stockout_linkage::run_deferred_linkage → run_deferred",
        proofs: &[
            (
                "src/business/erp/stockout_handler.rs",
                "linkage::engine::run_excluding",
            ),
            (
                "src/business/erp/stockout_linkage.rs",
                "linkage::engine::run_deferred",
            ),
        ],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_TRANSFER,
        status_labels: &[
            ("DRAFT", "草稿"),
            ("PENDING", "待审批"),
            ("APPROVED", "已审批"),
            ("IN_TRANSIT", "在途"),
            ("DONE", "已完成"),
            ("CANCELLED", "已取消"),
        ],
        wired: true,
        wiring: "已接线（LNK-01）：erp/transfer_handler_status.rs::apply_transition 第 ⑥ 步 \
                 （提交 / 审核 / 发运 / 收货 / 作废 六态流转的**唯一**状态写入点，CAS 成功才跑）\
                 调 erp::transfer_linkage::run_in_tx → linkage::engine::run_excluding(\
                 spec::TRANSFER_EXCLUDED_ACTIONS)（库存 / 流水由 settle_transfer 落账，必须排除，\
                 否则同一行明细扣两次、一张单四条流水）；commit 后 \
                 change_transfer_status 调 run_deferred_after_commit → run_deferred。\
                 审批通过 / 驳回（流程回调）同样接：flow::task_bridge::settle_transfer 事务内 \
                 run_in_tx + commit 后 run_deferred（不接这条，配了已发布流程的生产环境里 \
                 STOCK_TRANSFER/APPROVED 的规则永远不触发）",
        proofs: &[
            (
                "src/business/erp/transfer_linkage.rs",
                "linkage::engine::run_excluding",
            ),
            (
                "src/business/erp/transfer_linkage.rs",
                "linkage::engine::run_deferred",
            ),
        ],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_PURCHASE_ORDER,
        status_labels: &[
            ("DRAFT", "草稿"),
            ("SUBMITTED", "已提交"),
            ("APPROVED", "已审批"),
            ("PARTIAL_RECEIVED", "部分收货"),
            ("RECEIVED", "已收货"),
            ("CLOSED", "已关闭"),
            ("CANCELLED", "已取消"),
        ],
        wired: true,
        wiring: "已接线（LNK-02）：两处状态写入点共用 erp/purchase_linkage.rs 一份 hook —— \
                 ① purchase_model::apply_status[_with_instance]（提交 / 审批通过 / 驳回 / 关闭 / \
                 作废，CAS 成功 affected>0 才跑）调 purchase_linkage::run_in_tx → \
                 linkage::engine::run_excluding(spec::PURCHASE_EXCLUDED_ACTIONS)；\
                 ② erp/stockin_purchase.rs::sync_purchase_status（部分收货 / 全部收货 / \
                 收货作废回退，走的是 transition_if_changed，**完全不经过 apply_status**）\
                 在 CAS 成功后调同一个 run_in_tx。两处都必须接，否则「部分收货 → 全部收货」\
                 这条边永远不触发。通知 / 建任务这类 AfterCommit 动作由调用方在 commit 之后 \
                 调 LinkageOutbox::flush → linkage::engine::run_deferred",
        proofs: &[
            (
                "src/business/erp/purchase_linkage.rs",
                "linkage::engine::run_excluding",
            ),
            (
                "src/business/erp/purchase_linkage.rs",
                "linkage::engine::run_deferred",
            ),
        ],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_PAYABLE,
        status_labels: &[
            ("OPEN", "未付"),
            ("PARTIAL_PAID", "部分已付"),
            ("SETTLED", "已结清"),
            ("PENDING_CANCEL", "作废审批中"),
            ("CANCELLED", "已作废"),
        ],
        wired: true,
        wiring: "已接线（LNK-03）：应付单的三个状态推进（付款 / 部分付款 / 结清）全部在 \
                 erp/payable_payment.rs::settle_payment_in_tx 里用 transition_if_changed 推进，\
                 该函数在第 ⑤ 步调 erp/payable_linkage.rs::run_in_tx → \
                 linkage::engine::run_excluding(spec::PAYABLE_EXCLUDED_ACTIONS)，\
                 排除 UPDATE_PAYABLE_BALANCE（第 ④ 步已回写 paid/unpaid/status）与 \
                 GENERATE_VOUCHER（第 ② 步已生成付款凭证）。两条入口（直付 pay_in_tx / \
                 审批通过 settle_approved_in_tx）共用这一处；通知 / 建任务由调用方在 \
                 commit 之后调 payable_linkage::run_deferred_after_commit → \
                 linkage::engine::run_deferred。状态没变（PARTIAL_PAID → PARTIAL_PAID）\
                 不触发",
        proofs: &[
            (
                "src/business/erp/payable_linkage.rs",
                "linkage::engine::run_excluding",
            ),
            (
                "src/business/erp/payable_linkage.rs",
                "linkage::engine::run_deferred",
            ),
        ],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_VOUCHER,
        status_labels: &[
            ("DRAFT", "草稿"),
            ("AUDITED", "已审核"),
            ("POSTED", "已记账"),
        ],
        wired: false,
        wiring: "未接线：凭证过账 / 反过账在 erp/voucher_write.rs（append / reverse_posted 已抽成\
                 唯一实现），但没有联动调用点。注意：在凭证状态上配 GENERATE_VOUCHER 会造「凭证\
                 生凭证」的环，接线时必须把该动作排除或直接禁止",
        proofs: &[],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_PERIOD,
        status_labels: &[
            ("NOT_OPENED", "未开启"),
            ("OPEN", "已开启"),
            ("CLOSED", "已结账"),
        ],
        wired: false,
        wiring: "未接线：会计期间结账 / 反结账在 erp/period.rs（apply_status + \
                 state_machine::transition），没有联动调用点。接线要动：月结事务内 + commit 后",
        proofs: &[],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_YEAR_END,
        status_labels: &[("NOT_CLOSED", "未结转"), ("CLOSED", "已结转")],
        wired: false,
        wiring: "未接线：年结批次在 erp/year_end.rs（大批量结转，事务很长）。接线要动：结转事务内\
                 只放关键动作、通知一律 commit 后；年结的失败回滚代价最大，必须单开卡做真机验证",
        proofs: &[],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_CONTRACT,
        status_labels: &[
            ("DRAFT", "草稿"),
            ("PENDING_APPROVAL", "审批中"),
            ("SIGNED", "已签订"),
            ("ACTIVE", "已生效"),
            ("COMPLETED", "已完成"),
            ("TERMINATED", "已终止"),
        ],
        wired: false,
        wiring: "未接线：合同状态在 erp/contract_handler.rs（构造 / 审批通过 / 终止），没有联动\
                 调用点。接线要动：审批通过那一步（与 flow 审批回调相邻，注意别重复触发）",
        proofs: &[],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_STOCK_TAKE,
        status_labels: &[
            ("DRAFT", "草稿"),
            ("PENDING_POST", "待过账"),
            ("POSTED", "已过账"),
            ("CANCELLED", "已作废"),
        ],
        wired: true,
        wiring: "已接线：erp/stocktake_handler.rs::post_tx 第 ⑥ 步（过账 PENDING_POST → \
                 POSTED）与 cancel_tx 第 ③ 步（作废 → CANCELLED）调 \
                 stocktake_linkage::run_in_tx → 通用引擎 run_excluding(\
                 spec::STOCKTAKE_EXCLUDED_ACTIONS)（成本账 / 库位账 / 流水与差异凭证由 \
                 ③⑤ / 回滚红冲专用路径落账，必须排除以免重复落账）；commit 后 \
                 post_stock_take / cancel_stock_take 调 run_deferred_after_commit → run_deferred",
        proofs: &[
            (
                "src/business/erp/stocktake_linkage.rs",
                "linkage::engine::run_excluding",
            ),
            (
                "src/business/erp/stocktake_linkage.rs",
                "linkage::engine::run_deferred",
            ),
        ],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_FIXED_ASSET,
        status_labels: &[("IN_USE", "在役"), ("IDLE", "闲置"), ("DISPOSED", "已处置")],
        wired: false,
        wiring: "未接线：资产停用 / 启用 / 处置 / 作废在 erp/asset_handler.rs（+ asset_bridge.rs \
                 抛凭证），没有联动调用点",
        proofs: &[],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_DEPRECIATION,
        status_labels: &[
            ("CALCULATED", "已计算"),
            ("POSTED", "已过账"),
            ("REVERSED", "已红冲"),
            ("SKIPPED", "已跳过"),
        ],
        wired: false,
        wiring: "未接线：折旧批次过账 / 红冲在 erp/depreciation_handler.rs（+ \
                 depreciation_model.rs），没有联动调用点。批次是大批量操作，通知必须走 commit 后",
        proofs: &[],
    },
    TriggerSpec {
        entity: state_machine::ENTITY_EXPENSE,
        status_labels: &[
            ("DRAFT", "草稿"),
            ("SUBMITTED", "审批中"),
            ("APPROVED", "已审批（待付款）"),
            ("PAID", "已付清"),
            ("CANCELLED", "已作废"),
        ],
        wired: false,
        wiring: "未接线：报销单提交 / 通过 / 付清 / 作废在 erp/expense_handler.rs（+ \
                 expense_bridge.rs 建应付 / 凭证），没有联动调用点",
        proofs: &[],
    },
    TriggerSpec {
        entity: TRIGGER_TASK,
        status_labels: &[
            ("TODO", "待开始"),
            ("DOING", "进行中"),
            ("REVIEW", "待验收"),
            ("DONE", "已完成"),
            ("CANCELLED", "已取消"),
        ],
        wired: true,
        wiring: "已接线：task/task_handler_notice.rs::notify_status_change —— 每次任务状态变更\
                 都会用 task_notice_draft 算好草稿，再调 linkage::engine::status_notice \
                 （SHADOW 影子对比 / TAKEOVER 由配置发、旧逻辑不发；校验不过一律退回旧逻辑）",
        proofs: &[(
            "src/business/task/task_handler_notice.rs",
            "linkage::engine::status_notice",
        )],
    },
    TriggerSpec {
        entity: TRIGGER_FLOW_INSTANCE,
        status_labels: &[
            ("PENDING", "审批中"),
            ("APPROVED", "已通过"),
            ("REJECTED", "已驳回"),
            ("SKIPPED", "已跳过"),
            ("CANCELLED", "已取消"),
        ],
        wired: false,
        wiring: "未接线：审批实例状态在 flow 审批回调里推进（approve / reject / cancel），\
                 那里的「审批通过」还会顺带改业务单据状态，两个触发点的先后顺序必须先定清楚\
                 （否则同一次审批会跑两遍联动），属于必须单开卡设计的一条",
        proofs: &[],
    },
];

/// 取触发点的接线声明（不在白名单 → `None`）。
pub fn trigger_spec(entity: &str) -> Option<&'static TriggerSpec> {
    TRIGGER_SPECS
        .iter()
        .find(|spec| spec.entity == entity.trim())
}

/// 某个触发状态的**中文名**（`None` = 这个状态不在该触发点的状态集里）。
///
/// 配置页 / 预览 / 报文都走这里取值：编码 → 中文只有一份映射（[`TriggerSpec::status_labels`]），
/// 前端不再自己维护状态文案表。
pub fn status_label_of(entity: &str, status: &str) -> Option<&'static str> {
    let code = status.trim();
    trigger_spec(entity)?
        .status_labels
        .iter()
        .find(|(value, _)| *value == code)
        .map(|(_, label)| *label)
}

/// 触发点是否**真的接线**（白名单里有但没人调引擎 = `false`）。
///
/// 未知触发点一律 `false`（fail-closed：写入口据此要求显式确认，不能因为「查不到」就放行）。
pub fn is_wired_trigger(entity: &str) -> bool {
    trigger_spec(entity).map(|spec| spec.wired).unwrap_or(false)
}

/// 未接线的触发点（顺序与 [`TRIGGERS`] 一致；配置页/文档/单测共用）。
pub fn unwired_triggers() -> Vec<&'static str> {
    TRIGGER_SPECS
        .iter()
        .filter(|spec| !spec.wired)
        .map(|spec| spec.entity)
        .collect()
}

/// 已接线的触发点。
pub fn wired_triggers() -> Vec<&'static str> {
    TRIGGER_SPECS
        .iter()
        .filter(|spec| spec.wired)
        .map(|spec| spec.entity)
        .collect()
}

/// 状态机里有、但**明确不做**联动触发点的实体（白名单的例外清单）。
///
/// 加进这里不是「把红测试改绿」——是给「这个实体为什么没有触发点」留一条可查的理由：
/// 没有理由就不许进这张表（单测会核对里面每一项都是真实注册的实体）。
pub const TRIGGER_EXEMPT: &[&str] = &[
    // ARCH-6 §3：预算审批的结果是「额度状态翻 ACTIVE」，没有任何业务单据跟着变状态；
    // 预算的到期/超额提醒走 `biz_business_reminder`（V0210 那套），不挂在单据联动上。
    state_machine::ENTITY_BUDGET,
    // 销售链 SO-05：销售订单的状态流转入口（`sales_order_handler`）**不调联动引擎** ——
    // 本卡只交付单据 + 状态机，出库联动 / 通知 / 生成凭证都在后续卡（S3-05 出库过账卡）。
    // 登记在白名单里却没人调引擎 = 「配了规则能保存、永远不触发」（P1-FIX-5C 那个病），
    // 所以这里显式豁免；等出库过账卡真的接上引擎时，把它从这里移到 `TRIGGER_SPECS`
    // 并给出 `wired = true` + `proofs`（漂移守卫会同时要求两者）。
    state_machine::ENTITY_SALES_ORDER,
    // 销售链 SO-09/SO-10：发货单的状态流转入口（`shipment_handler`）**不调联动引擎** ——
    // 过账的跨单据联动走的是**它生成的那张销售出库单**（`STOCK_OUT/POSTED` 触发点，
    // `stockout_handler::post_out_tx` ⑥ 已接线，`run_excluding` 排除重复落账的四类动作）。
    // 发货单自己没有独立的触发点：同一件事（货出库了）由出库单触发一次，比在两张单上
    // 各触发一次安全（否则通知 / 生成凭证会重复）。真要按发货单触发（例如「通知客户已发货」），
    // 接引擎后把它从这里移到 `TRIGGER_SPECS` 并给 `wired = true` + `proofs`
    // （漂移守卫 `wired_triggers_match_call_sites` 会同时要求两者）。
    state_machine::ENTITY_SHIPMENT,
    // 销售链 SO-13：应收单的状态流转入口（`receivable::write_off` / 收款核销）
    // **不调联动引擎** —— 本卡只交付台账 + 状态机 + 账龄；「核销后通知 / 生成坏账凭证」
    // 属后续凭证卡（`receivable_bridge` 是 SO-14 的出库挂账，不是状态联动）。
    // 按 P1-FIX-5C 的口径显式豁免（登记进白名单却没人调 = 配了规则永远不触发）；
    // 真要按应收触发时，把它从 `TRIGGER_SPECS` 之外移进白名单并给 `wired = true` + `proofs`。
    state_machine::ENTITY_RECEIVABLE,
    // 销售链 SO-17：收款单的状态流转入口（`receipt::post` / 核销）**不调联动引擎** ——
    // 收款只动「应收的可核销余额 + 核销关系」，跨单据联动（应收转为已收 / 通知）走的是
    // 应收自己的状态（`RECEIVABLE` 同为豁免项），不需要在收款单上再触发一次
    // （否则同一笔钱在两个实体上各触发一次，通知 / 凭证会重复）。
    // 按 P1-FIX-5C 的口径显式豁免；真要按收款触发时，把它移进 `TRIGGER_SPECS`
    // 并给 `wired = true` + `proofs`（漂移守卫会同时要求两者）。
    state_machine::ENTITY_RECEIPT,
    // 销售链 SO-20：发票的状态流转入口（`invoice::change_status`）**不调联动引擎** ——
    // 开票 / 作废的会计后果由收入确认凭证 + 红冲凭证承担（`voucher::create_for_sales_revenue`
    // / `reverse_posted`），没有「另一张单据跟着变状态」的联动语义；
    // 真要按发票触发（例如通知客户已开票）时，移进 `TRIGGER_SPECS` 并给 `wired = true` + `proofs`。
    state_machine::ENTITY_INVOICE,
    // 销售链 SO-29：退货单的状态流转入口（`sales_return_handler::change_return_status`）
    // **不调联动引擎** —— 过账后的会计后果（回库 / 冲应收 / 红字凭证）由 SO-30 的过账
    // 事务承担，退货单本身没有「另一张单据跟着变状态」的联动语义；真要按退货触发
    // （例如通知仓库收货）时，移进 `TRIGGER_SPECS` 并给 `wired = true` + `proofs`
    // （漂移守卫 `wired_triggers_match_call_sites` 会同时要求两者）。
    state_machine::ENTITY_SALES_RETURN,
    // 销售链 SO-14 遗留 · D10（卡 t_b9840c8d）：收款退款单（资金单据）同理 ——
    // 状态流转入口 `receipt_refund::change_status` 不调联动引擎；退款凭证在过账那一步
    // 直接生成，应收侧的冲减由 `allocate` 落账（不是状态联动）。
    // 审批通过后的过账是**流程回调**（`receipt_refund_bridge::on_refund_settled`），
    // 不是单据联动规则，所以也不进 `TRIGGER_SPECS`。
    state_machine::ENTITY_RECEIPT_REFUND,
];

/// 配置变更审计写 `biz_status_log` 时用的 `entity_type`。
///
/// 与 `FLOW_EDGE_CFG`（D1 状态机边配置）同一套口径：配置表本身不是审计记录，
/// 谁在什么时候把哪条联动改成什么，只在 `biz_status_log` 里。
pub const AUDIT_ENTITY_TYPE: &str = "FLOW_LINKAGE_CFG";

/// 待办通知的动作码（第 2 步接管的就是它，路由/测试都按这个常量取值）。
pub const ACTION_SEND_NOTICE: &str = "SEND_NOTICE";

/// 累计收货量的动作码（已实现的关键动作，用来证明「关键动作失败 → 回滚」）。
pub const ACTION_UPDATE_RECEIVED_QTY: &str = "UPDATE_RECEIVED_QTY";

/// 生成凭证的动作码（【D3】第 3 步接管：复用 `voucher::create_by_rule`）。
pub const ACTION_GENERATE_VOUCHER: &str = "GENERATE_VOUCHER";

/// 建工作任务的动作码（P1-FIX-5C 接管：复用 `task::model::create_task`）。
pub const ACTION_CREATE_TASK: &str = "CREATE_TASK";

/// **出库过账**的专用路由排除清单（P1-FIX-5C）。
///
/// 出库过账在 `erp/stockout_handler::post_out_tx` 的 ③⑤ 步里自己写完了
/// 库存（成本账 + 库位账 + 流水）、成本快照与成本结转凭证，所以通用引擎在这条
/// 触发点上必须把这四类动作交给专用路径：
///
/// - `WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` → 同一行明细扣两次库存；
/// - `UPDATE_COST` → 成本快照被二次覆盖；
/// - `GENERATE_VOUCHER` → 一张单两张成本结转凭证。
///
/// 排除清单放在**调用方**（这里导出、`post_out_tx` 传入）而不是写死在引擎里：
/// 换个触发点（如 `PAYABLE/SETTLED`）通用路线仍然要能跑这些动作。
/// 漂移守卫：`tests_wiring::stockout_post_wires_generic_engine_and_excludes_dedicated_actions`。
pub const STOCKOUT_EXCLUDED_ACTIONS: &[&str] = &[
    "WRITE_INVENTORY",
    "WRITE_INVENTORY_TXN",
    "UPDATE_COST",
    ACTION_GENERATE_VOUCHER,
];

/// **库存调拨单**的专用路由排除清单（LNK-01）。
///
/// 调拨的库存落账**不在通用引擎里**：`IN_TRANSIT → DONE` 由
/// `erp/transfer_settle.rs::settle_transfer` 在同一个事务里把调出仓 −、调入仓 +、写两条
/// `TRANSFER_OUT` / `TRANSFER_IN` 流水、并把成本原价从调出仓搬到调入仓。所以这条
/// 触发点上通用引擎必须把这 3 类动作交给专用路径：
///
/// - `WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` → 同一行明细扣/加两次，流水翻倍；
/// - `GENERATE_VOUCHER` → 调拨不抛凭证（只搬成本），且该动作目前只接管 `STOCK_IN`
///   的取数口径，挂在调拨上只会产生一条「跳过」噪音。
///
/// `UPDATE_COST` **刻意不排除**：调拨的成本转移只在 `settle_transfer` 里做，通用引擎
/// 在别的目标态（如 `APPROVED`）上仍可能需要它；而 `settle_transfer` 那条路上
/// `UPDATE_COST` 没有对应的取数实现（占位动作，执行时 Skipped），不会重复落账。
///
/// 排除清单放在**调用方**（这里导出、`erp::transfer_linkage` 传入）而不是写死在引擎里：
/// 换个触发点（如 `PAYABLE/SETTLED`）通用路线仍然要能跑这些动作。
/// 漂移守卫：`tests_wiring::transfer_status_wires_generic_engine_and_excludes_inventory`。
pub const TRANSFER_EXCLUDED_ACTIONS: &[&str] = &[
    "WRITE_INVENTORY",
    "WRITE_INVENTORY_TXN",
    ACTION_GENERATE_VOUCHER,
];

/// **采购订单**的专用路由排除清单（LNK-02）。
///
/// 采购单的状态变更**不落任何账**：库存 / 库位账 / 流水 / 进项凭证 / 应付全部由
/// **收货单过账**（`erp/stockin_handler_post.rs::post_in_tx`）在同一事务里写完，
/// 采购单只是被 `sync_purchase_status` 被动推状态。所以通用引擎在这个触发点上
/// 必须把这 4 类动作交给专用路径：
///
/// - `WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` → 同一物料加两次库存、一张单两条流水；
/// - `GENERATE_VOUCHER` → 一张收货单两张进项凭证；
/// - `UPDATE_RECEIVED_QTY` → 已收数量是**派生值**（`purchase_model::POSTED_RECEIVED_SQL`，
///   由已过账收货单现算），累计量动作再推一次就是「第二真相」，报表与过账闸立刻分叉。
///
/// 排除清单放在**调用方**（这里导出、`erp::purchase_linkage` 传入）而不是写死在引擎里：
/// 换个触发点（如 `PAYABLE/SETTLED`）通用路线仍然要能跑这些动作。
/// 漂移守卫：`tests_wiring::purchase_order_wires_generic_engine_and_excludes_stock_actions`。
pub const PURCHASE_EXCLUDED_ACTIONS: &[&str] = &[
    "WRITE_INVENTORY",
    "WRITE_INVENTORY_TXN",
    ACTION_GENERATE_VOUCHER,
    ACTION_UPDATE_RECEIVED_QTY,
];

/// **应付单**的专用路由排除清单（LNK-03）。
///
/// 付款事务（`erp/payable_payment.rs::settle_payment_in_tx`）自己已经写完了两类账，
/// 通用引擎在这个触发点上必须把它们交给专用路径，否则就是**重复落账**：
///
/// - `UPDATE_PAYABLE_BALANCE` → 同一笔钱扣两次：`paid_amount` 翻倍、`unpaid_amount`
///   变负数（库侧 CHECK 会直接把整笔付款炸成 500），状态也会原地跳；
///   余额的**唯一**维护点是付款事务第 ④ 步的条件回写（CAS 在 `paid_amount` 上）。
/// - `GENERATE_VOUCHER` → 一笔付款两张付款凭证（借 2202 / 贷 1002 各两条），
///   总账 / 现金流量表按凭证汇总就翻倍；付款凭证的唯一生成点是付款事务第 ② 步。
///
/// 排除清单放在**调用方**（这里导出、`erp::payable_linkage` 传入）而不是写死在引擎里：
/// 换个触发点（如 `PURCHASE_ORDER/SETTLED`）通用路线仍然要能跑这些动作。
/// 漂移守卫：`tests_wiring::payable_status_wires_generic_engine_and_excludes_balance_and_voucher`。
pub const PAYABLE_EXCLUDED_ACTIONS: &[&str] = &["UPDATE_PAYABLE_BALANCE", ACTION_GENERATE_VOUCHER];

/// **盘点单**的专用路由排除清单（LNK-04）。
///
/// 盘点单的状态变更**自己落账**：`erp/stocktake_handler.rs::post_tx` 第 ③ 步按差异
/// 正负分别走 `apply_gain` / `apply_loss`（成本账 + 库位账 + 流水），第 ⑤ 步出
/// 盘点差异凭证（借 1405 / 贷 1901 成对），作废时 `cancel_tx` 第 ④ 步原路回滚 + 红冲。
/// 所以通用引擎在这个触发点上必须把这 3 类动作交给专用路径，否则就是**重复落账**：
///
/// - `WRITE_INVENTORY` → 同一物料在成本账上加减两次，账面数量 ≠ 实盘数；
///   「三账一致（盘点后库存 = 实盘）」直接失效；
/// - `WRITE_INVENTORY_TXN` → 一张盘点单四条进销存流水（盘盈 IN 两条 / 盘亏 OUT 两条）；
/// - `GENERATE_VOUCHER` → 一张盘点单两张差异凭证，总账按凭证汇总就翻倍。
///
/// `SEND_NOTICE` / `CREATE_TASK` **不能排除**：过账 / 作废后的通知与待办就靠通用引擎
/// 的 `Phase::AfterCommit` 动作，排掉等于接线了但永远不响。
///
/// 排除清单放在**调用方**（这里导出、`erp::stocktake_linkage` 传入）而不是写死在引擎里：
/// 换个触发点（如 `PAYABLE/SETTLED`）通用路线仍然要能跑这些动作。
/// 漂移守卫：`tests_wiring::stocktake_wires_generic_engine_and_excludes_inventory`。
pub const STOCKTAKE_EXCLUDED_ACTIONS: &[&str] = &[
    "WRITE_INVENTORY",
    "WRITE_INVENTORY_TXN",
    ACTION_GENERATE_VOUCHER,
];

// ==================== 禁用键（SQL 禁令） ====================

/// **禁配 SQL** 的键黑名单（第 1 层 CHECK 与第 2 层校验共用同一份清单）。
///
/// 出现这些键 = 有人想把「可执行语句」塞进配置。应用层 `400`，
/// 数据库层 `ck_flow_linkage_no_sql_key` 兜底（防绕过 API 直接写库）。
pub const FORBIDDEN_KEYS: &[&str] = &[
    "sql",
    "sql_text",
    "statement",
    "stmt",
    "query",
    "queries",
    "script",
    "expression",
    "exec",
    "execute",
    "shell",
    "cmd",
];

/// 键是不是被禁的「SQL 类」键（大小写不敏感：`SQL` / `Sql` 一并拦）。
pub fn is_forbidden_key(key: &str) -> bool {
    let lowered = key.trim().to_ascii_lowercase();
    FORBIDDEN_KEYS.contains(&lowered.as_str())
}

// ==================== 白名单动作 ====================

/// 第一批白名单动作（11 个）：够覆盖现有跨单据联动，且每个都能指回一段既有实现。
///
/// 参数表的读法：`key` 后带 `?` 的是选填（[`ActionSpec::allowed_keys`]）。
pub const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        code: "WRITE_INVENTORY",
        label: "写库存（成本账 + 库位账）",
        phase: Phase::InTx,
        severity: Severity::Critical,
        implemented: false,
        params: &[
            ParamSpec {
                key: "material_id",
                kind: ParamKind::Uuid,
                required: true,
                note: "物料",
            },
            ParamSpec {
                key: "warehouse_id",
                kind: ParamKind::Uuid,
                required: true,
                note: "仓库",
            },
            ParamSpec {
                key: "location_id",
                kind: ParamKind::Uuid,
                required: false,
                note: "库位（空 = 整仓汇总桶）",
            },
            ParamSpec {
                key: "qty",
                kind: ParamKind::Decimal,
                required: true,
                note: "数量（永远写正数，方向由 direction 表达）",
            },
            ParamSpec {
                key: "direction",
                kind: ParamKind::Enum(&["IN", "OUT"]),
                required: true,
                note: "方向",
            },
            ParamSpec {
                key: "unit_cost",
                kind: ParamKind::Decimal,
                required: false,
                note: "入库单价（空 = 取单据明细快照）",
            },
        ],
        reuse: "erp/stockin_model.rs::apply_cost + apply_location_qty（stockin_handler::post_in_tx ③/③b 的唯一实现，含移动加权与库位账同步）",
    },
    ActionSpec {
        code: "WRITE_INVENTORY_TXN",
        label: "写库存流水",
        phase: Phase::InTx,
        severity: Severity::Critical,
        implemented: false,
        params: &[
            ParamSpec {
                key: "material_id",
                kind: ParamKind::Uuid,
                required: true,
                note: "物料",
            },
            ParamSpec {
                key: "warehouse_id",
                kind: ParamKind::Uuid,
                required: true,
                note: "仓库",
            },
            ParamSpec {
                key: "qty",
                kind: ParamKind::Decimal,
                required: true,
                note: "数量（正数；txn_type 由 direction 决定）",
            },
            ParamSpec {
                key: "direction",
                kind: ParamKind::Enum(&["IN", "OUT"]),
                required: true,
                note: "方向",
            },
            ParamSpec {
                key: "biz_type",
                kind: ParamKind::Text { max: 32 },
                required: true,
                note: "业务类型（落 biz_inventory_txn.biz_type）",
            },
        ],
        reuse: "erp/stockin_model.rs::write_txn（post_in_tx ③ 的流水写法：qty 恒正 + txn_type 表达方向）",
    },
    ActionSpec {
        code: "UPDATE_COST",
        label: "更新成本账（移动加权）",
        phase: Phase::InTx,
        severity: Severity::Critical,
        implemented: false,
        params: &[
            ParamSpec {
                key: "material_id",
                kind: ParamKind::Uuid,
                required: true,
                note: "物料",
            },
            ParamSpec {
                key: "warehouse_id",
                kind: ParamKind::Uuid,
                required: true,
                note: "仓库",
            },
            ParamSpec {
                key: "qty",
                kind: ParamKind::Decimal,
                required: true,
                note: "本次数量",
            },
            ParamSpec {
                key: "unit_cost",
                kind: ParamKind::Decimal,
                required: true,
                note: "本次单价（不含税）",
            },
            ParamSpec {
                key: "direction",
                kind: ParamKind::Enum(&["IN", "OUT"]),
                required: true,
                note: "方向",
            },
        ],
        reuse: "erp/stockin_model.rs::apply_cost（移动加权 + 结存≤0 重置单价，PRD §3.2.6 边界表）",
    },
    ActionSpec {
        code: "GENERATE_VOUCHER",
        label: "生成凭证",
        phase: Phase::InTx,
        severity: Severity::Critical,
        // ★ 已实现（D3 第 3 步）：复用 voucher::create_by_rule（取数 + 取号 + 借贷平衡唯一实现）
        implemented: true,
        params: &[
            ParamSpec {
                key: "template_code",
                kind: ParamKind::Text { max: 32 },
                required: false,
                note: "凭证模板（空 = 按 biz_type 取默认模板）",
            },
            ParamSpec {
                key: "biz_type",
                kind: ParamKind::Text { max: 32 },
                required: true,
                note: "业务类型（STOCK_IN / PAYMENT / …）",
            },
            ParamSpec {
                key: "account_date",
                kind: ParamKind::Text { max: 10 },
                required: false,
                note: "记账日期 yyyy-MM-dd（空 = 取单据日期）",
            },
            ParamSpec {
                key: "mode",
                kind: ParamKind::Enum(&["SHADOW", "TAKEOVER"]),
                required: false,
                note: "上线模式（空 = SHADOW：只做逐字段对比，凭证仍由旧逻辑发）",
            },
        ],
        reuse: "erp/voucher.rs::create_by_rule（D3 抽出的唯一实现：取数 VoucherBizCtx::of_stock_in → \
                科目解析 → 取号 PZ- → ≥2 行 + 借贷平衡 → voucher::post_voucher；\
                与 create_for_stock_in 同一函数，不另写任何算法）",
    },
    ActionSpec {
        code: "REVERSE_VOUCHER",
        label: "红冲凭证",
        phase: Phase::InTx,
        severity: Severity::Critical,
        implemented: false,
        params: &[
            ParamSpec {
                key: "reason",
                kind: ParamKind::Text { max: 200 },
                required: false,
                note: "红冲原因（写进凭证摘要）",
            },
            ParamSpec {
                key: "account_date",
                kind: ParamKind::Text { max: 10 },
                required: false,
                note: "红冲日期 yyyy-MM-dd（空 = 今天）",
            },
        ],
        reuse: "erp/voucher_write.rs::reverse_posted（校验 + 红字 INSERT + reversed_by 回写；P7 已抽成唯一实现）",
    },
    ActionSpec {
        code: "UPDATE_DOC_STATUS",
        label: "改单据状态（走状态机校验）",
        phase: Phase::InTx,
        severity: Severity::Critical,
        implemented: false,
        params: &[
            ParamSpec {
                key: "target_status",
                kind: ParamKind::Text { max: 32 },
                required: true,
                note: "目标状态（必须是有效边能走到的状态）",
            },
            ParamSpec {
                key: "reason",
                kind: ParamKind::Text { max: 200 },
                required: false,
                note: "变更原因（边要求原因时必填，由状态机自己判）",
            },
        ],
        reuse: "shared/state_machine.rs::transition + <entity>_model::apply_status（边表校验 + CAS + biz_status_log 同事务）",
    },
    ActionSpec {
        code: "UPDATE_RECEIVED_QTY",
        label: "校验累计收货量",
        phase: Phase::InTx,
        severity: Severity::Critical,
        // ★ 已接管（P2-3 起语义=校验）：累计收货量是派生值（`POSTED_RECEIVED_SQL`），
        //   本动作只判「其他单据已过账收货 + 本次 ≤ 订购量」，故意不再写任何列。
        implemented: true,
        params: &[
            ParamSpec {
                key: "source_item_id",
                kind: ParamKind::Uuid,
                required: true,
                note: "采购明细行 id",
            },
            ParamSpec {
                key: "qty",
                kind: ParamKind::Decimal,
                required: true,
                note: "本次收货数量（正数）",
            },
        ],
        reuse: "erp/purchase_model.rs::POSTED_RECEIVED_SQL（派生口径：只算已过账收货单；P2-3 起本动作只校验不落库）",
    },
    ActionSpec {
        code: "UPDATE_PAYABLE_BALANCE",
        label: "更新应付余额",
        phase: Phase::InTx,
        severity: Severity::Critical,
        implemented: false,
        params: &[
            ParamSpec {
                key: "payable_id",
                kind: ParamKind::Uuid,
                required: false,
                note: "应付单（空 = 取触发单据关联的应付单）",
            },
            ParamSpec {
                key: "direction",
                kind: ParamKind::Enum(&["INCREASE", "DECREASE"]),
                required: true,
                note: "增加 / 减少",
            },
            ParamSpec {
                key: "amount",
                kind: ParamKind::Decimal,
                required: true,
                note: "金额（正数）",
            },
        ],
        reuse: "erp/payable_payment.rs 的余额维护（paid_amount / balance 与状态推进同一事务）",
    },
    ActionSpec {
        code: "CREATE_TASK",
        label: "生成工作任务",
        phase: Phase::AfterCommit,
        severity: Severity::NonCritical,
        // ★ 已实现（P1-FIX-5C）：出库过账 → 建复核任务 / 收货过账 → 建跟单任务
        implemented: true,
        params: &[
            ParamSpec {
                key: "title",
                kind: ParamKind::Text { max: 200 },
                required: true,
                note: "任务标题",
            },
            ParamSpec {
                key: "content",
                kind: ParamKind::Text { max: 2000 },
                required: false,
                note: "任务描述",
            },
            ParamSpec {
                key: "assignee_strategy",
                kind: ParamKind::Enum(&["OPERATOR", "DOC_CREATOR"]),
                required: false,
                note: "负责人（空 = OPERATOR）",
            },
            ParamSpec {
                key: "due_days",
                kind: ParamKind::Int { min: 0, max: 365 },
                required: false,
                note: "几天后到期（空 = 不设）",
            },
        ],
        reuse: "business/task/model.rs::create_task（取号 + 落 biz_task；注意它吃 &Transaction，接管时要么改造签名，要么在独立事务里调用）",
    },
    ActionSpec {
        code: "SEND_NOTICE",
        label: "发通知",
        phase: Phase::AfterCommit,
        severity: Severity::NonCritical,
        // ★ 已实现：第 2 步接管任务状态通知
        implemented: true,
        params: &[
            ParamSpec {
                key: "mode",
                kind: ParamKind::Enum(&["SHADOW", "TAKEOVER"]),
                required: false,
                note: "SHADOW（默认）只做并行对比、仍由旧代码发；TAKEOVER 由配置发、旧代码不发",
            },
            ParamSpec {
                key: "title",
                kind: ParamKind::Text { max: 200 },
                required: false,
                note: "覆盖标题（空 = 沿用触发点既有文案，零重复真相）",
            },
            ParamSpec {
                key: "content",
                kind: ParamKind::Text { max: 2000 },
                required: false,
                note: "覆盖正文（空 = 沿用触发点既有文案）",
            },
            ParamSpec {
                key: "force",
                kind: ParamKind::Bool,
                required: false,
                note: "覆盖强制送达（空 = 沿用触发点的分级）",
            },
            ParamSpec {
                key: "recipient",
                kind: ParamKind::Enum(&["LEGACY", "OWNER", "CREATOR", "ASSIGNER", "OPERATOR"]),
                required: false,
                note: "收件人策略（空 = LEGACY，沿用触发点算出来的收件人）",
            },
        ],
        reuse: "business/notice::dispatch（模板渲染 / 扇出 / 偏好过滤 / 幂等键 / 投递记录全免费继承；flow/notify.rs 与 task/handler.rs::dispatch_task_notice 同一条口径）",
    },
    ActionSpec {
        code: "LOCK_DOCUMENT",
        label: "锁定单据（禁止编辑）",
        phase: Phase::InTx,
        severity: Severity::Critical,
        implemented: false,
        params: &[ParamSpec {
            key: "reason",
            kind: ParamKind::Text { max: 255 },
            required: false,
            note: "锁定原因（写进单据备注/日志）",
        }],
        reuse: "**前置项缺失**：单据表目前没有 lock_* 列，编辑入口也没有锁判据。接管前要先加列 + 让 6 个 update 入口统一过锁闸，属第 3 步的前置工作，不在本卡范围",
    },
];

/// 取动作声明（不在白名单 → `None`）。
pub fn spec_of(code: &str) -> Option<&'static ActionSpec> {
    let trimmed = code.trim();
    ACTIONS.iter().find(|spec| spec.code == trimmed)
}

/// 动作码是否在白名单里。
pub fn is_whitelisted(code: &str) -> bool {
    spec_of(code).is_some()
}

/// 全部动作码（错误文案 / 测试 / 迁移对拍用）。
pub fn codes() -> Vec<&'static str> {
    ACTIONS.iter().map(|spec| spec.code).collect()
}

/// 已接管的动作码。
pub fn implemented_codes() -> Vec<&'static str> {
    ACTIONS
        .iter()
        .filter(|spec| spec.implemented)
        .map(|spec| spec.code)
        .collect()
}

// ==================== 第 2 层：参数校验 ====================

/// 校验 `action_params`（第 2 层，**写配置时**调用）。
///
/// 判据（任何一条不过都是 `400`）：
///
/// 1. 必须是 JSON 对象；
/// 2. 键不许命中 [`FORBIDDEN_KEYS`]（禁配 SQL，文案点名禁令）；
/// 3. 键必须在该动作的参数表里（不许多塞）；
/// 4. 必填参数必须在（`null` 等同缺失）；
/// 5. 类型 / 枚举 / 长度 / 区间必须合法。
///
/// 返回 `Err(中文原因)`：调用方包成 `400 + PARAM_INVALID`。
pub fn validate_params(spec: &ActionSpec, params: &serde_json::Value) -> Result<(), String> {
    let Some(object) = params.as_object() else {
        return Err(format!(
            "动作 {} 的参数 `action_params` 必须是 JSON 对象（当前是 {}）",
            spec.code,
            json_kind(params)
        ));
    };

    // ② 禁配 SQL：先判这一条，错误文案要让人知道「这不是笔误」
    for key in object.keys() {
        if is_forbidden_key(key) {
            return Err(format!(
                "禁止在联动配置里写 SQL（参数键 `{key}`）：跨单据联动只接受预定义动作 + 固定入参，\
                 任何可执行语句都不允许。动作 {} 允许的参数：{}",
                spec.code,
                spec.allowed_keys()
            ));
        }
    }

    // ③ 不许出现动作没声明的键
    for key in object.keys() {
        if spec.param(key).is_none() {
            return Err(format!(
                "动作 {} 不接受参数 `{key}`（允许：{}）",
                spec.code,
                spec.allowed_keys()
            ));
        }
    }

    for param in spec.params {
        let value = object.get(param.key).filter(|value| !value.is_null());
        match value {
            None => {
                if param.required {
                    return Err(format!(
                        "动作 {} 缺少必需参数 `{}`（{}）：{}",
                        spec.code,
                        param.key,
                        param.kind.label(),
                        param.note
                    ));
                }
            }
            Some(value) => check_value(spec, param, value)?,
        }
    }
    Ok(())
}

/// 单个参数的值校验。
fn check_value(
    spec: &ActionSpec,
    param: &'static ParamSpec,
    value: &serde_json::Value,
) -> Result<(), String> {
    let reject = |why: String| -> Result<(), String> {
        Err(format!(
            "动作 {} 的参数 `{}` 不合法：{why}（期望 {}）",
            spec.code,
            param.key,
            param.kind.label()
        ))
    };
    match param.kind {
        ParamKind::Text { max } => {
            let Some(text) = value.as_str() else {
                return reject(format!("当前是 {}", json_kind(value)));
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                if param.required {
                    return reject("不能是空白字符串".to_string());
                }
                return Ok(());
            }
            if trimmed.chars().count() > max {
                return reject(format!(
                    "最长 {max} 个字符，当前 {}",
                    trimmed.chars().count()
                ));
            }
            Ok(())
        }
        ParamKind::Uuid => {
            let Some(text) = value.as_str() else {
                return reject(format!("当前是 {}", json_kind(value)));
            };
            match Uuid::parse_str(text.trim()) {
                Ok(_) => Ok(()),
                Err(_) => reject(format!("不是合法的 UUID：`{text}`")),
            }
        }
        ParamKind::Enum(values) => {
            let Some(text) = value.as_str() else {
                return reject(format!("当前是 {}", json_kind(value)));
            };
            if values.contains(&text.trim()) {
                Ok(())
            } else {
                reject(format!("`{}` 不在允许值里", text.trim()))
            }
        }
        ParamKind::Bool => {
            if value.is_boolean() {
                Ok(())
            } else {
                reject(format!("当前是 {}", json_kind(value)))
            }
        }
        ParamKind::Int { min, max } => match value.as_i64() {
            Some(number) if (min..=max).contains(&number) => Ok(()),
            Some(number) => reject(format!("{number} 超出 {min}..={max}")),
            None => reject(format!("当前是 {}", json_kind(value))),
        },
        ParamKind::Decimal => {
            let text = match value {
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::String(text) => text.trim().to_string(),
                other => return reject(format!("当前是 {}", json_kind(other))),
            };
            match parse_decimal(&text) {
                Some(_) => Ok(()),
                None => reject(format!("`{text}` 不是合法的十进制数")),
            }
        }
        ParamKind::UuidList { max_items } => {
            let Some(items) = value.as_array() else {
                return reject(format!("当前是 {}", json_kind(value)));
            };
            if items.len() > max_items {
                return reject(format!("最多 {max_items} 项，当前 {}", items.len()));
            }
            for item in items {
                let Some(text) = item.as_str() else {
                    return reject(format!("列表里有非字符串项：{}", json_kind(item)));
                };
                if Uuid::parse_str(text.trim()).is_err() {
                    return reject(format!("列表里有非法 UUID：`{text}`"));
                }
            }
            Ok(())
        }
    }
}

/// 十进制解析（与 erp 侧同一套：JSON number 或数字字符串）。
fn parse_decimal(text: &str) -> Option<nik::prelude::rust_decimal::Decimal> {
    use std::str::FromStr;
    nik::prelude::rust_decimal::Decimal::from_str(text).ok()
}

/// JSON 值类型的中文名（错误文案用）。
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "布尔",
        serde_json::Value::Number(_) => "数字",
        serde_json::Value::String(_) => "字符串",
        serde_json::Value::Array(_) => "数组",
        serde_json::Value::Object(_) => "对象",
    }
}

// ==================== 状态中文名（P1-FIX-12A） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::flow::linkage::model;
    use crate::business::task::state::TaskStatus;

    /// ① 覆盖性（**双向**）：`statuses`（`model::known_statuses`）的每一项都要有中文名，
    /// 表里也不许出现状态机里根本不存在的状态。
    ///
    /// 单向只测「不遗漏」的话，删掉一个状态机边、表里留着旧状态名照样全绿 ——
    /// 那会给管理员一个「能选、但永远等不到」的状态。所以两个方向都判。
    #[test]
    fn status_labels_cover_known_statuses() {
        assert_eq!(
            TRIGGER_SPECS.len(),
            TRIGGERS.len(),
            "触发点表与白名单必须等长"
        );
        for spec in TRIGGER_SPECS {
            let known = model::known_statuses(spec.entity);
            assert!(!known.is_empty(), "{} 的状态集不能为空", spec.entity);
            for status in &known {
                assert!(
                    status_label_of(spec.entity, status).is_some(),
                    "{} 的状态 `{status}` 缺中文名（配置页会退回显示编码）",
                    spec.entity
                );
            }
            for &(code, _) in spec.status_labels {
                assert!(
                    known.contains(&code),
                    "{} 的状态表里写了 `{code}`，但它不在状态集 {known:?} 里（写错状态 = 规则永远不触发）",
                    spec.entity
                );
            }
        }
    }

    /// ② 表本身要干净：编码非空且全大写、中文非空、同一触发点内编码不重复。
    #[test]
    fn status_labels_are_well_formed() {
        for spec in TRIGGER_SPECS {
            let mut seen: Vec<&str> = Vec::new();
            for &(code, label) in spec.status_labels {
                assert_eq!(
                    code,
                    code.trim(),
                    "{}：编码不许带空白 `{code}`",
                    spec.entity
                );
                assert!(!code.is_empty(), "{}：编码不能为空", spec.entity);
                assert!(
                    code.chars()
                        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_'),
                    "{}：编码 `{code}` 必须是大写下划线形式（与库 CHECK 逐字一致）",
                    spec.entity
                );
                assert!(
                    !label.trim().is_empty(),
                    "{}：`{code}` 的中文名不能为空",
                    spec.entity
                );
                assert_eq!(
                    label,
                    label.trim(),
                    "{}：`{code}` 的中文名不许带空白",
                    spec.entity
                );
                assert!(
                    !seen.contains(&code),
                    "{}：状态 `{code}` 重复登记（下拉会出现两个一样的选项）",
                    spec.entity
                );
                seen.push(code);
            }
        }
    }

    /// ③ 漂移守卫：逐字对拍**各单据自己的** `status_label()`（不是手抄自证）。
    ///
    /// 只对拍能从 crate 外部调用的 7 个模块 + 任务状态枚举；`stockin_base` /
    /// `stockout_base` / `transfer_base` 三个底座模块在 `erp/mod.rs` 里是私有 `mod`，
    /// 跨模块拿不到（它们的文案由各自模块的单测守着）。这里点名统计对拍条数，
    /// 映射断开（例如把实体名写错）会立刻反映成条数不足。
    #[test]
    fn status_labels_match_doc_modules() {
        use crate::business::erp::{
            asset_dto, contract_model, expense_dto, payable_query, purchase_model, stocktake_model,
            voucher_query,
        };

        fn doc_module_label(entity: &str, status: &str) -> Option<String> {
            let label = match entity {
                e if e == state_machine::ENTITY_PURCHASE_ORDER => {
                    purchase_model::status_label(status).to_string()
                }
                e if e == state_machine::ENTITY_PAYABLE => {
                    payable_query::status_label(status).to_string()
                }
                e if e == state_machine::ENTITY_VOUCHER => {
                    voucher_query::status_label(status).to_string()
                }
                e if e == state_machine::ENTITY_CONTRACT => {
                    contract_model::status_label(status).to_string()
                }
                e if e == state_machine::ENTITY_STOCK_TAKE => {
                    stocktake_model::status_label(status).to_string()
                }
                e if e == state_machine::ENTITY_FIXED_ASSET => {
                    asset_dto::status_label(status).to_string()
                }
                e if e == state_machine::ENTITY_EXPENSE => {
                    expense_dto::status_label(status).to_string()
                }
                TRIGGER_TASK => TaskStatus::parse(status)?.label().to_string(),
                _ => return None,
            };
            Some(label)
        }

        let mut checked = 0usize;
        for spec in TRIGGER_SPECS {
            for &(code, label) in spec.status_labels {
                let Some(expected) = doc_module_label(spec.entity, code) else {
                    continue;
                };
                assert_eq!(
                    label, expected,
                    "{} / `{code}`：字典里的中文名与单据模块的 status_label 不一致",
                    spec.entity
                );
                checked += 1;
            }
        }
        // 7 个可调用模块 + 任务枚举覆盖的状态总数（采购 7 + 应付 5 + 凭证 3 + 合同 6 +
        // 盘点 4 + 资产 3 + 报销 5 + 任务 5）。少一条就说明上面的映射断了。
        assert_eq!(checked, 38, "对拍状态数不符：映射表被改坏或实体名写错");
    }

    /// ④ 查表函数的边界：未知实体 / 未知状态一律 `None`（fail-closed，不回显编码当中文）。
    #[test]
    fn status_label_lookup_rejects_unknown() {
        assert_eq!(
            status_label_of(state_machine::ENTITY_STOCK_IN, "POSTED"),
            Some("已过账")
        );
        assert_eq!(
            status_label_of(state_machine::ENTITY_STOCK_IN, " POSTED "),
            Some("已过账")
        );
        assert_eq!(
            status_label_of(state_machine::ENTITY_STOCK_IN, "NOT_A_STATUS"),
            None
        );
        assert_eq!(status_label_of("NOT_AN_ENTITY", "POSTED"), None);
    }
}
