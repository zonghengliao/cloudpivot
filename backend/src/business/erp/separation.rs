//! ERP 内控开关：**制单 ≠ 审批 / 过账 / 付款**（可配，默认不改变现有行为）。
//!
//! # 为什么做成开关而不是硬分离
//!
//! 内控上「制单人不能自己审批 / 过账 / 付款」是正解，但**小团队常常只有一个人能干活**：
//! 硬分离会让单据谁也点不动（历史上还踩过会签死锁，见 [`crate::business::flow::internal_control`]）。
//! 因此三项职责拆成三个独立开关，由管理员在「支撑功能 / 参数配置 / ERP 内控」里按需打开；
//! **默认值与改动前逐字一致**（见下表），升级不影响任何存量用户。
//!
//! | 开关键 | 默认 | 打开后拦什么 | 没打开时的行为 |
//! |---|---|---|---|
//! | `erp.separation_of_duties.enabled` | `false` | **一键开启**总闸：把未单独配置的项默认成「开」 | 不参与判定（只作缺省值来源） |
//! | `erp.separation_of_duties.post` | `false` | 制单人不能**过账**（收货入库单直接过账 / 调拨发货收货） | 与改动前一致：允许同人过账 |
//! | `erp.separation_of_duties.approve` | `true` | 制单人 / 发起人不能**审批**（采购 / 收货过账 / 作废 / 凭证红冲 / 应付付款与作废） | 与改动前一致：内控闸照旧拦 |
//! | `erp.separation_of_duties.pay` | `true` | 制单人不能**审批自己提交的付款**（P17 闸） | 与改动前一致：P17 闸照旧拦 |
//!
//! # 取值优先级（参数配置 > 代码默认）
//!
//! ```text
//! ① 本项自己的键（post / approve / pay）存在且能解析成 bool  → 用它
//! ② 总闸 enabled 存在且为 true                              → true（一键开启，只升不降）
//! ③ 否则                                                     → 代码默认（见上表）
//! ```
//!
//! **总闸只升不降**是刻意的：种子把 `enabled` 写成 `false`，如果它还能「降级」，
//! 那这一行就会把默认开启的审批 / 付款内控闸一起关掉（等于静默放宽内控）。
//! 要单独关某一项，就显式把该项设成 `false`。
//!
//! # 读性能与生效时延
//!
//! 取值走 [`crate::support::config::model`] 的**进程内运行时快照**（命中是纯内存读，
//! 不会每次查库），TTL 60 秒；管理端改参数会广播重载，**立刻生效**（多实例另有版本号轮询）。
//! 非法值（不是 `true/false/1/0`）回退代码默认并打 `warn`，绝不静默当成 `false`
//! ——把它当 `false` 等于「配置写错 = 内控被关」，那是更危险的方向。
//!
//! # 边界（刻意不做的事）
//!
//! - **手工凭证**（`voucher_write::create_manual_voucher`）是「制单即记账」的单步动作，
//!   `create_by` 永远是操作人本人，同一动作内无法分离；要分离必须拆「制单草稿 + 他人记账」
//!   两步（新状态 + 新接口），不在本开关范围。自动凭证（收货 / 付款产生）随单据过账一起生成，
//!   已由调用方（过账 / 付款）的内控闸覆盖。
//! - **审批流里生成待办时的排除**（SKIPPED 留痕）读的是 `approve` 这一项，
//!   关掉之后制单人能重新看到自己的待办并批得动 —— 与「403 闸关闭」是同一件事的两面。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;
use crate::support::config::model::{self as config_model, SysConfigModel};

/// 总闸：一键把未单独配置的项默认成「开」（`true` 只升不降）。
pub const MASTER_KEY: &str = "erp.separation_of_duties.enabled";
/// 制单 ↔ 过账分离。
pub const POST_KEY: &str = "erp.separation_of_duties.post";
/// 制单 ↔ 审批分离。
pub const APPROVE_KEY: &str = "erp.separation_of_duties.approve";
/// 制单 ↔ 付款分离。
pub const PAY_KEY: &str = "erp.separation_of_duties.pay";

/// 受管的三项职责（每一项对应一个开关 + 一段拦截文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Duty {
    /// 过账（收货入库单过账 / 调拨发货收货）。
    Post,
    /// 审批（各单据的审批待办）。
    Approve,
    /// 付款（应付付款审批）。
    Pay,
}

impl Duty {
    /// 全部职责（枚举 / 诊断用）。
    pub const ALL: [Duty; 3] = [Duty::Post, Duty::Approve, Duty::Pay];

    /// 本项的开关键（`sys_config.config_key`）。
    pub const fn key(self) -> &'static str {
        match self {
            Duty::Post => POST_KEY,
            Duty::Approve => APPROVE_KEY,
            Duty::Pay => PAY_KEY,
        }
    }

    /// 本项的**代码默认值**（参数缺失 / 非法时用它）。
    ///
    /// 三项默认值 = 改动前的真实行为：过账本来不拦、审批与付款本来就有内控闸。
    /// 改这三个值等于静默改内控强度，**必须配迁移与说明**，不许随手改。
    pub const fn default_enabled(self) -> bool {
        match self {
            Duty::Post => false,
            Duty::Approve | Duty::Pay => true,
        }
    }

    /// 开关的中文名（写进拦截文案与参数说明）。
    pub const fn label(self) -> &'static str {
        match self {
            Duty::Post => "制单与过账分离",
            Duty::Approve => "制单与审批分离",
            Duty::Pay => "制单与付款分离",
        }
    }

    /// 被拦下的那个动作（`不能自己{动作}`）。
    pub const fn action(self) -> &'static str {
        match self {
            Duty::Post => "过账",
            Duty::Approve => "审批",
            Duty::Pay => "付款审批",
        }
    }
}

/// 生效值从哪来（诊断 / 单测用；日志里带上它，排查「为什么没生效」不用猜）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 来自本项自己的键。
    Own,
    /// 来自总闸 `enabled = true`。
    Master,
    /// 来自代码默认值。
    Default,
}

/// 一次开关解析的完整结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// 最终是否开启。
    pub enabled: bool,
    /// 生效值的来源。
    pub source: Source,
    /// 读到了但解析不出来的原文（`Some` = 调用方应当 `warn`）。
    pub invalid: Option<String>,
}

/// 纯函数：按「本项 → 总闸 → 代码默认」解析开关值（**不碰库**，便于单测钉死优先级）。
///
/// # 参数
///
/// - `duty`：哪一项职责。
/// - `own`：本项键的原始文本（`None` = 参数不存在 / 已停用 / 值为空）。
/// - `master`：总闸键的原始文本（同上）。
///
/// # 返回值
///
/// [`Resolution`]；非法文本不参与判定，只记在 `invalid` 里由调用方打日志。
pub fn resolve(duty: Duty, own: Option<&str>, master: Option<&str>) -> Resolution {
    let mut invalid: Option<String> = None;
    if let Some(text) = own {
        match config_model::parse_bool(text) {
            Some(enabled) => {
                return Resolution {
                    enabled,
                    source: Source::Own,
                    invalid: None,
                };
            }
            None => invalid = Some(text.to_string()),
        }
    }
    if let Some(text) = master {
        match config_model::parse_bool(text) {
            // 总闸只升不降：`false` 不覆盖本项默认值（否则种子里的 false 会关掉内控）
            Some(true) => {
                return Resolution {
                    enabled: true,
                    source: Source::Master,
                    invalid,
                };
            }
            Some(false) => {}
            None => {
                if invalid.is_none() {
                    invalid = Some(text.to_string());
                }
            }
        }
    }
    Resolution {
        enabled: duty.default_enabled(),
        source: Source::Default,
        invalid,
    }
}

/// 读取本项开关的**生效值**（走运行时快照，60 秒 TTL + 写操作后广播）。
///
/// # 参数
///
/// - `duty`：哪一项职责。
///
/// # 返回值
///
/// 生效的布尔值；参数缺失 / 停用 / 解析失败时回退 [`Duty::default_enabled`]。
pub async fn enabled(duty: Duty) -> bool {
    let own = SysConfigModel::read_text(duty.key()).await;
    let master = SysConfigModel::read_text(MASTER_KEY).await;
    let resolution = resolve(duty, own.as_deref(), master.as_deref());
    if let Some(raw) = resolution.invalid.as_deref() {
        tracing::warn!(
            switch = duty.key(),
            value = raw,
            fallback = duty.default_enabled(),
            "ERP 内控开关取值非法（只认 true/false/1/0），已回退代码默认值"
        );
    }
    tracing::debug!(
        switch = duty.key(),
        enabled = resolution.enabled,
        source = ?resolution.source,
        "ERP 内控开关解析"
    );
    resolution.enabled
}

/// 拦截文案（**唯一出口**）：说明「哪个开关开着」「哪张单」「为什么不行」「怎么放行」。
///
/// # 参数
///
/// - `duty`：哪一项职责。
/// - `subject`：单据描述（如「收货入库单 IN-20261102-000001」，为空时退化为「这张单」）。
///
/// # 返回值
///
/// 面向业务人员的完整中文提示（前端直接弹这句话）。
pub fn rejection_text(duty: Duty, subject: &str) -> String {
    let subject = subject.trim();
    let subject = if subject.is_empty() {
        "这张单"
    } else {
        subject
    };
    format!(
        "系统已开启「{}」，{subject} 由你本人制作，不能自己{}。\
         请交给其他有权限的账号处理，或在系统参数中关闭该开关（{}）。",
        duty.label(),
        duty.action(),
        duty.key()
    )
}

/// 审批 / 付款审批入口的**补充提示**（追加在原有 403 文案后面）。
///
/// 老文案（`不能审批自己提交的采购订单…请交给其他有审批权限的账号处理`）已经在前端文案
/// 与十余条用例里被引用，**不改写只追加**：把「哪个开关开着」「怎么放行」补在后面。
///
/// # 参数
///
/// - `duty`：哪一项职责（审批传 [`Duty::Approve`]，付款审批传 [`Duty::Pay`]）。
///
/// # 返回值
///
/// 一段带括号的中文尾巴，可直接 `format!("{原文案}{}")`。
pub fn approval_hint(duty: Duty) -> String {
    format!(
        "（内控开关 {} 已开启；小团队如确需本人处理，可在系统参数中关闭该开关）",
        duty.key()
    )
}

/// 纯函数闸门：开关开着且操作人就是制单人 → 409（**不碰库**，便于单测）。
///
/// `create_by` 为空的历史单据一律放行：没有制单人可比，谈不上「自己批自己」。
///
/// # 参数
///
/// - `duty`：哪一项职责。
/// - `switch_on`：开关的生效值（调用方已解析好）。
/// - `creator`：单据制单人（`None` = 历史数据 / 系统生成）。
/// - `operator`：本次操作人。
/// - `subject`：单据描述（拼进文案）。
///
/// # 返回值
///
/// 放行 `Ok(())`；命中内控 `Err(409 + 66013)`。
pub fn check(
    duty: Duty,
    switch_on: bool,
    creator: Option<Uuid>,
    operator: Option<Uuid>,
    subject: &str,
) -> std::result::Result<(), ApiError> {
    if !switch_on {
        return Ok(());
    }
    // 操作人未知（系统任务 / 无令牌）→ 没有「自己」可言，放行
    let Some(operator) = operator else {
        return Ok(());
    };
    if creator != Some(operator) {
        return Ok(());
    }
    tracing::info!(
        switch = duty.key(),
        %operator,
        subject = subject,
        "ERP 内控拦截：制单人不能自己{}",
        duty.action()
    );
    Err(ApiError::new(
        409,
        error_code::ERP_SEPARATION_OF_DUTIES,
        rejection_text(duty, subject),
    ))
}

/// 闸门（读开关 + 判定）：业务入口直接 `?` 即可。
///
/// # 参数
///
/// - `duty`：哪一项职责。
/// - `creator`：单据制单人。
/// - `operator`：本次操作人。
/// - `subject`：单据描述（拼进文案）。
///
/// # 返回值
///
/// 放行 `Ok(())`；命中内控 `Err(409 + 66013)`。
pub async fn guard(
    duty: Duty,
    creator: Option<Uuid>,
    operator: Option<Uuid>,
    subject: &str,
) -> std::result::Result<(), ApiError> {
    let switch_on = enabled(duty).await;
    check(duty, switch_on, creator, operator, subject)
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    const ME: Uuid = Uuid::from_u128(0x5e9a_0000_0000_0000_0000_0000_0000_0001);
    const OTHER: Uuid = Uuid::from_u128(0x5e9a_0000_0000_0000_0000_0000_0000_0002);

    /// ① 开关关闭（默认，参数不存在）→ 制单人能自己过账。
    #[test]
    fn default_off_lets_creator_post() {
        let resolution = resolve(Duty::Post, None, None);
        assert!(
            !resolution.enabled,
            "post 默认必须是关闭的（不影响存量用户）"
        );
        assert_eq!(resolution.source, Source::Default);
        assert!(
            check(
                Duty::Post,
                resolution.enabled,
                Some(ME),
                Some(ME),
                "收货入库单 IN-T"
            )
            .is_ok(),
            "默认关闭时制单人必须能过账"
        );
    }

    /// ② 开关开启 → 制单人过账 409，且文案点名开关 + 给出放行办法。
    #[test]
    fn switch_on_blocks_creator_with_readable_409() {
        let resolution = resolve(Duty::Post, Some("true"), None);
        assert!(resolution.enabled);
        assert_eq!(resolution.source, Source::Own);

        let error = check(
            Duty::Post,
            resolution.enabled,
            Some(ME),
            Some(ME),
            "收货入库单 IN-20261102-000001",
        )
        .expect_err("制单人过账必须被拦");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_SEPARATION_OF_DUTIES);
        assert!(
            error.msg.contains("系统已开启「制单与过账分离」"),
            "文案要点名开关：{}",
            error.msg
        );
        assert!(
            error.msg.contains("IN-20261102-000001"),
            "文案要点名单据：{}",
            error.msg
        );
        assert!(
            error.msg.contains("不能自己过账"),
            "文案要说清动作：{}",
            error.msg
        );
        assert!(
            error.msg.contains("关闭该开关"),
            "文案必须给出放行办法（O5）：{}",
            error.msg
        );
        assert!(
            error.msg.contains(POST_KEY),
            "文案要带开关键名，管理员才知道去哪改：{}",
            error.msg
        );
    }

    /// ③ 开关开启 → 别人过账放行。
    #[test]
    fn switch_on_lets_others_post() {
        assert!(
            check(Duty::Post, true, Some(ME), Some(OTHER), "收货入库单 IN-T").is_ok(),
            "别人过账必须放行"
        );
        // 没有制单人的历史单据同样放行（无可比对象）
        assert!(
            check(Duty::Post, true, None, Some(ME), "收货入库单 IN-T").is_ok(),
            "无制单人的历史单据不许被拦"
        );
    }

    /// ④ 采购单审批：同一套闸，动作文案是「审批」（默认就是开的）。
    #[test]
    fn approve_duty_blocks_creator_by_default() {
        let resolution = resolve(Duty::Approve, None, None);
        assert!(resolution.enabled, "approve 默认开（= 改动前的内控闸）");
        let error = check(Duty::Approve, true, Some(ME), Some(ME), "采购单 PO-T")
            .expect_err("制单人自审必须被拦");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("制单与审批分离"), "{}", error.msg);
        assert!(error.msg.contains("不能自己审批"), "{}", error.msg);
        assert!(
            check(Duty::Approve, true, Some(ME), Some(OTHER), "采购单 PO-T").is_ok(),
            "其他审批人放行"
        );
    }

    /// ⑤ 付款：默认开（P17 闸），拦自己；别人放行。
    #[test]
    fn pay_duty_blocks_creator_by_default() {
        let resolution = resolve(Duty::Pay, None, None);
        assert!(resolution.enabled);
        let error = check(Duty::Pay, true, Some(ME), Some(ME), "付款单 PAY-T")
            .expect_err("制单人自付必须被拦");
        assert!(error.msg.contains("制单与付款分离"), "{}", error.msg);
        assert!(error.msg.contains("付款审批"), "{}", error.msg);
        assert!(
            check(Duty::Pay, true, Some(ME), Some(OTHER), "付款单 PAY-T").is_ok(),
            "别人付款放行"
        );
        // 小团队显式关掉 → 自己也能付
        let off = resolve(Duty::Pay, Some("false"), None);
        assert!(!off.enabled);
        assert_eq!(off.source, Source::Own);
    }

    /// ⑥ 参数不存在 → 三项都用代码默认值（false / true / true）。
    #[test]
    fn missing_parameters_fall_back_to_code_defaults() {
        for duty in Duty::ALL {
            let resolution = resolve(duty, None, None);
            assert_eq!(resolution.source, Source::Default, "{}", duty.key());
            assert_eq!(resolution.enabled, duty.default_enabled(), "{}", duty.key());
            assert!(resolution.invalid.is_none(), "缺参数不是「非法值」");
        }
        assert!(!Duty::Post.default_enabled());
        assert!(Duty::Approve.default_enabled());
        assert!(Duty::Pay.default_enabled());
    }

    /// ⑦ 参数值非法（非 bool）→ 回退默认值，并上报「非法」让调用方 warn。
    #[test]
    fn invalid_value_falls_back_to_default_and_reports() {
        let resolution = resolve(Duty::Post, Some("yes"), None);
        assert!(!resolution.enabled, "非法值必须回退 post 的默认 false");
        assert_eq!(resolution.source, Source::Default);
        assert_eq!(resolution.invalid.as_deref(), Some("yes"), "要能打出原文");

        // approve 的默认是 true：写错值不能把它误关成 false
        let resolution = resolve(Duty::Approve, Some("ture"), None);
        assert!(resolution.enabled, "非法值不许把内控关掉");
        assert!(resolution.invalid.is_some());

        // 本项合法时不再抱怨总闸（本项优先，总闸根本没参与判定）
        let resolution = resolve(Duty::Post, Some("1"), Some("oops"));
        assert!(resolution.enabled);
        assert_eq!(resolution.source, Source::Own);
        assert!(resolution.invalid.is_none());
    }

    /// ⑧ 改完立即生效：解析不缓存、每次重新按当前值算；总闸只升不降。
    #[test]
    fn changes_take_effect_immediately() {
        // 同一个键，两次解析拿的是两次读到的值（调用点不做任何记忆）
        assert!(!resolve(Duty::Post, Some("false"), None).enabled);
        assert!(resolve(Duty::Post, Some("true"), None).enabled);
        assert!(!resolve(Duty::Post, Some("false"), None).enabled);

        // 总闸开 → 未单独配置的项默认开
        let via_master = resolve(Duty::Post, None, Some("true"));
        assert!(via_master.enabled);
        assert_eq!(via_master.source, Source::Master);

        // 总闸关 → 不降级（否则种子里的 false 会把审批 / 付款内控关掉）
        let master_off = resolve(Duty::Post, None, Some("false"));
        assert_eq!(master_off.source, Source::Default);
        assert!(!master_off.enabled);
        assert!(
            resolve(Duty::Approve, None, Some("false")).enabled,
            "总闸不许关掉审批内控"
        );

        // 显式关掉某一项比总闸优先
        let explicit_off = resolve(Duty::Post, Some("false"), Some("true"));
        assert!(!explicit_off.enabled, "显式配置优先于总闸");
        assert_eq!(explicit_off.source, Source::Own);
    }

    /// 文案恒等约束：三项都能拼出一句完整、可执行的中文（含空单据描述时不出现空档）。
    #[test]
    fn rejection_text_is_complete_for_every_duty() {
        for duty in Duty::ALL {
            let text = rejection_text(duty, "");
            assert!(text.contains(duty.label()), "{}", text);
            assert!(text.contains(duty.action()), "{}", text);
            assert!(text.contains("关闭该开关"), "{}", text);
            assert!(text.contains(duty.key()), "{}", text);
            assert!(!text.contains(" 这次"), "空白单据描述不该留空档：{text}");
        }
    }
}
