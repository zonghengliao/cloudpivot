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
//! - **已实现（3 个）**：`SEND_NOTICE`（第 2 步接管任务状态通知）、
//!   `UPDATE_RECEIVED_QTY`（复用既有 `add_received_qty`，作为关键动作事务语义的活样本）、
//!   `GENERATE_VOUCHER`（**D3 第 3 步**：复用 `voucher::create_by_rule`，
//!   与收货过账的旧路径是同一个函数、同一份取数与取号）；
//! - **占位（8 个）**：入参契约已定、[`ActionSpec::reuse`] 写清了后续要复用的既有函数；
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
    TRIGGER_TASK,
    TRIGGER_FLOW_INSTANCE,
];

/// 触发点是否在白名单里。
pub fn is_known_trigger(entity: &str) -> bool {
    TRIGGERS.contains(&entity)
}

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
        label: "更新累计收货量",
        phase: Phase::InTx,
        severity: Severity::Critical,
        // ★ 已实现：直接复用 post_in_tx ⑤ 用的那个条件更新（0 行 = 超收）
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
        reuse: "erp/stockin_purchase.rs::add_received_qty（条件更新 received_qty + qty <= qty，0 行 = 超收）",
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
        implemented: false,
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
