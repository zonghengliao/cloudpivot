//! 汇兑损益科目配置（M1-06 · 迁移 `V0411__fx_gain_loss_account`）。
//!
//! # 这个参数管什么
//!
//! 「外币业务产生的汇兑差额记哪个科目」—— 两条链路都读它，缺省都是 `5603`：
//!
//! | 链路 | 谁生成凭证 | 差额落在哪 |
//! |------|-----------|-----------|
//! | M1-05 期末调汇（科目余额口径） | [`crate::business::erp::period::period_fx_reval`] | 逐科目调整行的**净额**一笔 |
//! | SO-41 期末应收重估（逐笔口径） | [`crate::business::erp::fx_revaluation`] | 每期一张重估凭证的对方行 |
//!
//! 参数键 [`CONFIG_FX_GAIN_LOSS_ACCOUNT`]（`sys_config`），值只存**科目编码**。
//! 之所以不存 `biz_account.id`：id 在克隆库 / 重建环境里会变，编码是业务可读、跨库稳定的键
//! （与 `sys_config.inventory_opening_equity_account` 存 `3103` 同一口径）。
//!
//! # 三条铁律
//!
//! 1. **不静默回落**：参数有值但科目不可用（不存在 / 停用 / 非末级 / 非损益类 / 已标外币核算）
//!    ⇒ `400 + 66083` 点名，**不回退到 5603**。静默回落会让「改了配置却没生效」永远查不出来，
//!    而汇兑损益记错科目是**两张报表同时错**（该科目虚增 + 利润表少一块），不能拿它赌。
//!    只有「参数**缺失**」（行不存在 / 值为空 / 被停用）才用缺省 5603 —— 那时确实没有任何配置意图，
//!    且本卡上线前的行为就是硬编码 5603，缺省即等价。
//! 2. **配置值必须过校验才能落库**（[`check_target`]）：四条规则都是纯函数、可单测，
//!    与运行环境无关 —— 见 [`crate::business::erp::fx_settings_handler`] 的 PUT。
//! 3. **写后必须 reload 运行时快照**：`SysConfigModel::read_text` 优先读进程内快照，
//!    不 reload 的话「刚保存、试算还是老科目」，用户会以为没生效（见 [`save_gain_loss_account`]）。
//!
//! # 为什么「已标外币核算」的科目不能当汇兑损益科目
//!
//! M1-05 按 `biz_account.is_foreign_currency = 1` 扫调汇范围。若汇兑损益科目自己被标了，
//! 它既出调整行（按外币重估）又承接净额（本位币分录）—— 下一期它带着上期净额再被重估，
//! 差额递归放大且永不收敛。所以「承接汇兑损益的科目」与「参与外币核算的科目」两个集合必须互斥。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: sys_config 是全局参数（不参与数据权限），biz_account 是全局科目主数据
//! （V0137 无 dept_id / 归属人列）。可见性由 business:erp:fx-rate:list（读）与
//! business:erp:fx-settings:update（写）两个权限码控制。
//! ```

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::voucher::{self, AccountRef};
use crate::support::config::model::{SysConfigModel, reload_runtime_snapshot};
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 常量 ====================

/// 汇兑损益科目的系统参数键（`sys_config.config_key`，`config_value` 存科目编码）。
pub const CONFIG_FX_GAIN_LOSS_ACCOUNT: &str = "erp.fx.gain_loss_account";

/// 汇兑损益科目的**缺省**编码：`5603` 财务费用。
///
/// 与 [`voucher::ACCOUNT_FX_GAIN_LOSS`] 是同一个常量（同一个真相），这里给出语义化的别名：
/// 读本文件的人应该看到「缺省值」，而不是「某个业务常量的名字」。
pub const DEFAULT_FX_GAIN_LOSS_ACCOUNT: &str = voucher::ACCOUNT_FX_GAIN_LOSS;

/// 科目编码最大长度（`biz_account.account_code VARCHAR(30)`）。
pub const ACCOUNT_CODE_MAX_LEN: usize = 30;

/// 损益类科目的 `account_category` 取值（汇兑损益必须落在这里）。
pub const CATEGORY_PROFIT_LOSS: &str = "PROFIT_LOSS";

// ==================== 科目行（校验用快照） ====================

/// 候选 / 目标科目的校验快照。
///
/// 只带校验要用的列，避免把 `AccountRes` 的展示字段（余额方向、备注…）拉进校验逻辑 ——
/// [`check_target`] 才能是纯函数、可单测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetAccount {
    /// 科目编码。
    pub code: String,
    /// 科目名称。
    pub name: String,
    /// 科目类别（`ASSET` / `LIABILITY` / `EQUITY` / `COST` / `PROFIT_LOSS`）。
    pub category: String,
    /// 是否末级（`is_leaf`，1 = 末级）。
    pub is_leaf: bool,
    /// 是否启用（`status`，`1` = 启用）。
    pub enabled: bool,
    /// 是否已标「外币核算」（`is_foreign_currency`，1 = 已标）。
    pub is_foreign_currency: bool,
}

/// 下拉候选（只带编码与名称，展示用）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AccountCandidate {
    /// 科目编码。
    pub code: String,
    /// 科目名称。
    pub name: String,
}

// ==================== 纯函数：规范化与校验 ====================

/// 规范化科目编码入参：去空白；空 / 超长 / 含控制字符一律拒。
///
/// 不做大小写转换：科目编码是数字 + 点的业务编码（`5603` / `5602.01`），
/// 但主数据里也可能有 `M105.01` 这类大写字母编码 —— 原样比对，避免把用户输入
/// 「悄悄修好」后与库里那一行对不上（`literal preservation`：不替用户猜）。
pub fn normalize_code(raw: &str) -> std::result::Result<String, String> {
    let code = raw.trim();
    if code.is_empty() {
        return Err("科目编码不能为空：请选择或填写承接汇兑损益的会计科目".to_string());
    }
    if code.chars().count() > ACCOUNT_CODE_MAX_LEN {
        return Err(format!(
            "科目编码过长（{} 字）：biz_account.account_code 最长 {ACCOUNT_CODE_MAX_LEN} 字，请检查是否贴错",
            code.chars().count()
        ));
    }
    if code.chars().any(|ch| ch.is_control()) {
        return Err("科目编码含控制字符：请重新选择科目".to_string());
    }
    Ok(code.to_string())
}

/// 校验「这个科目能不能承接汇兑损益」——**纯函数**，四条规则，返回人话错误。
///
/// | 规则 | 不满足的后果 |
/// |------|-------------|
/// | 存在（未软删） | 凭证写不出来 |
/// | 启用（`status = 1`） | 凭证闸会拒（`account_by_code` 只认启用科目） |
/// | 末级（`is_leaf = 1`） | 汇总科目自身没有余额，「差额记到汇总科目」= 记到不存在的账 |
/// | 损益类（`account_category = 'PROFIT_LOSS'`） | 汇兑损益是当期损益，记进资产负债类会让两处同时错 |
/// | 未标外币核算（`is_foreign_currency = 0`） | 它自己会被 M1-05 扫进调汇范围 ⇒ 自相加成（见模块头注释） |
///
/// `row = None` 表示「库里查不到这个编码」（含已软删 —— 查询本身带 `deleted_at IS NULL`）。
pub fn check_target(code: &str, row: Option<&TargetAccount>) -> std::result::Result<(), String> {
    let Some(account) = row else {
        return Err(format!(
            "科目 {code} 不存在或已删除：汇兑损益必须记在一个**启用中的末级损益类科目**上。\
             请到「会计科目」页确认该编码，或改选一个现有科目（系统参数 \
             {CONFIG_FX_GAIN_LOSS_ACCOUNT} 的缺省是 {DEFAULT_FX_GAIN_LOSS_ACCOUNT} 财务费用）"
        ));
    };
    if !account.enabled {
        return Err(format!(
            "科目 {} {} 已停用：停用科目不能记账（凭证闸会拒），请先在「会计科目」页启用它，或改选其他科目",
            account.code, account.name
        ));
    }
    if !account.is_leaf {
        return Err(format!(
            "科目 {} {} 不是末级科目：汇总科目自身没有余额，汇兑差额挂上去等于记到不存在的账。\
             请选择末级明细科目（或在「会计科目」页给它建下级科目后选那一级）",
            account.code, account.name
        ));
    }
    if account.category != CATEGORY_PROFIT_LOSS {
        return Err(format!(
            "科目 {} {} 是 {} 类科目：汇兑损益属于**当期损益**，只能落在损益类（PROFIT_LOSS）科目上。\
             记到资产 / 负债 / 权益 / 成本类会让该科目与利润表同时错（多算一笔资产、少算一块损益）",
            account.code, account.name, account.category
        ));
    }
    if account.is_foreign_currency {
        return Err(format!(
            "科目 {} {} 已标记「外币核算」：它会自己被期末调汇（M1-05 按外币核算标记扫科目）扫进调整范围，\
             于是同一笔差额既作为「调整行」又作为「净额承接行」—— 上期净额下期再被重估，差额递归放大。\
             请先到「会计科目」页取消它的外币核算标记，或改选一个未标外币核算的损益类科目",
            account.code, account.name
        ));
    }
    Ok(())
}

// ==================== 数据访问 ====================

/// 按编码取科目的校验快照（含停用 / 未软删过滤：已软删视为不存在）。
pub async fn load_target(ex: &impl SqlExecutor, code: &str) -> DbResult<Option<TargetAccount>> {
    // SCOPE-EXEMPT: biz_account 是会计科目主数据（全局），没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT account_code, account_name, account_category, is_leaf, status, is_foreign_currency \
               FROM biz_account WHERE account_code = ? AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(code.to_string())],
        )
        .await?;
    row.as_ref().map(row_to_target).transpose()
}

/// 行 → 校验快照。
fn row_to_target(row: &Row) -> DbResult<TargetAccount> {
    Ok(TargetAccount {
        code: row.get_typed("account_code")?,
        name: row.get_typed("account_name")?,
        category: row.get_typed("account_category")?,
        is_leaf: row.get_typed::<i16>("is_leaf")? == 1,
        enabled: row.get_typed::<i16>("status")? == 1,
        is_foreign_currency: row.get_typed::<i16>("is_foreign_currency")? == 1,
    })
}

/// 可选科目列表（配置抽屉的下拉）：启用 + 末级 + 损益类 + 未标外币核算。
///
/// 排序按编码升序 —— 报表 / 凭证里都是按编码看科目，下拉也照同一个顺序，
/// 免得「页面上找不到 5603」这种低级困惑。
pub async fn candidates(ex: &impl SqlExecutor) -> DbResult<Vec<AccountCandidate>> {
    // SCOPE-EXEMPT: 同上（科目主数据全局可见）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT account_code, account_name FROM biz_account \
                  WHERE deleted_at IS NULL AND status = 1 AND is_leaf = 1 \
                    AND account_category = '{CATEGORY_PROFIT_LOSS}' AND is_foreign_currency = 0 \
                  ORDER BY account_code"
            ),
            vec![],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(AccountCandidate {
                code: row.get_typed("account_code")?,
                name: row.get_typed("account_name")?,
            })
        })
        .collect()
}

/// 参数里的汇兑损益科目编码（**未配置时为 `None`**，不套缺省）。
///
/// 与 [`gain_loss_account_code`] 的分工：本函数保留「到底有没有配」这个事实，
/// 供配置页区分「人配的」与「走缺省的」；业务链路只关心「用哪个」，走后者。
pub async fn configured_gain_loss_account() -> Option<String> {
    SysConfigModel::read_text(CONFIG_FX_GAIN_LOSS_ACCOUNT)
        .await
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 当前生效的汇兑损益科目编码：`sys_config` → 缺省 [`DEFAULT_FX_GAIN_LOSS_ACCOUNT`]。
///
/// **只有「没有配置」才回缺省**；有配置但科目不可用由调用方按 66083 拒（见模块头注释「三条铁律」）。
/// 读失败（DB 抖动 / 快照未装载）同样按「没有配置」处理并 warn —— 参数表读不到就整条调汇链 500
/// 的代价远大于「按缺省 5603 记账」（与 `inventory_equity_account_code` 同一取舍）。
pub async fn gain_loss_account_code() -> String {
    match configured_gain_loss_account().await {
        Some(code) => code,
        None => {
            tracing::debug!(
                config_key = CONFIG_FX_GAIN_LOSS_ACCOUNT,
                default = DEFAULT_FX_GAIN_LOSS_ACCOUNT,
                "汇兑损益科目：参数未配置（或已停用），按缺省值记账"
            );
            DEFAULT_FX_GAIN_LOSS_ACCOUNT.to_string()
        }
    }
}

/// 取汇兑损益科目（**DbResult 出口**，供返回 `DbResult` 的调用方，如结账事务里的 SO-41 重估）。
///
/// 不可用时返回 [`DbError::InvalidValue`]，文案点名参数键与科目编码。
pub async fn require_gain_loss_account(ex: &impl SqlExecutor) -> DbResult<AccountRef> {
    let code = gain_loss_account_code().await;
    voucher::account_by_code(ex, &code)
        .await?
        .ok_or_else(|| DbError::InvalidValue(unavailable_message(&code)))
}

/// 取汇兑损益科目（**ApiError 出口**，`400 + 66083`，供 handler / 调汇接口用）。
pub async fn require_gain_loss_account_api(ex: &impl SqlExecutor) -> std::result::Result<AccountRef, ApiError> {
    let code = gain_loss_account_code().await;
    let account = voucher::account_by_code(ex, &code)
        .await
        .map_err(|error| utils::db_error(error, "查询汇兑损益科目"))?;
    account.ok_or_else(|| ApiError::new(400, error_code::ERP_FX_ACCOUNT_INVALID, unavailable_message(&code)))
}

/// 「配置了但取不到这个科目」的报文（两条出口共用，保证文案一致）。
fn unavailable_message(code: &str) -> String {
    format!(
        "汇兑损益科目 {code} 不存在 / 已停用 / 已删除：系统参数 {CONFIG_FX_GAIN_LOSS_ACCOUNT} 指定了它，\
         但凭证写不进去。请到「汇率管理 → 汇兑损益科目」改选一个启用中的末级损益类科目，\
         或到「会计科目」页恢复该科目（缺省 {DEFAULT_FX_GAIN_LOSS_ACCOUNT}）"
    )
}

/// 保存汇兑损益科目（写 `sys_config` + 刷运行时快照）。
///
/// **调用方必须先过 [`check_target`]**：本函数只负责落库，不重复校验（校验要读科目行，
/// 放在 handler 里能给出更早、更准的错误）。
///
/// 行已存在 → `UPDATE`（同时把 `config_type` 归一到 `STRING`、`status` 置启用：
/// 写了值就该生效，不能出现「值改了但参数被停用 ⇒ 实际还在用 5603」这种隐形态）；
/// 行不存在 → `INSERT`（含 `description` / `category`，与 V0411 种子同形）。
pub async fn save_gain_loss_account(code: &str, operator: Option<Uuid>) -> DbResult<()> {
    let operator_text = operator.map(|id| id.to_string()).unwrap_or_default();
    let affected = db()
        .execute_with_params(
            "UPDATE sys_config \
                SET config_value = ?, config_type = 'STRING', status = 1, \
                    updated_at = now(), update_by = NULLIF(?::text, '')::uuid \
              WHERE config_key = ? AND deleted_at IS NULL",
            vec![
                Value::Text(code.to_string()),
                Value::Text(operator_text.clone()),
                Value::Text(CONFIG_FX_GAIN_LOSS_ACCOUNT.to_string()),
            ],
        )
        .await?;
    if affected == 0 {
        db().execute_with_params(
            "INSERT INTO sys_config (config_key, config_value, config_type, description, category, \
                                     sort_order, config_level, is_modifiable, is_encrypted, is_restart, status) \
             VALUES (?, ?, 'STRING', ?, 'erp_fx', 10, 2, TRUE, FALSE, FALSE, 1)",
            vec![
                Value::Text(CONFIG_FX_GAIN_LOSS_ACCOUNT.to_string()),
                Value::Text(code.to_string()),
                Value::Text(
                    "汇兑损益承接科目编码（期末调汇 M1-05 / 应收重估 SO-41 的差额都记这个科目；缺省 5603 财务费用）"
                        .to_string(),
                ),
            ],
        )
        .await?;
    }
    // 读完就走快照，写完必须刷：否则「刚保存、试算还是老科目」。
    // reload 失败不回滚本次写入（值已经落库）—— warn 后由 60s TTL 自愈。
    if let Err(error) = reload_runtime_snapshot().await {
        tracing::warn!(
            config_key = CONFIG_FX_GAIN_LOSS_ACCOUNT,
            error = %error,
            "汇兑损益科目已写入，但运行时参数快照刷新失败（60s 后自动回源自愈）"
        );
    }
    Ok(())
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn target(code: &str, category: &str, is_leaf: bool, enabled: bool, is_fc: bool) -> TargetAccount {
        TargetAccount {
            code: code.to_string(),
            name: format!("{code} 名称"),
            category: category.to_string(),
            is_leaf,
            enabled,
            is_foreign_currency: is_fc,
        }
    }

    /// ① 参数键逐字钉住：改键 = 迁移里那一行对不上 = 参数永远读不到（会静默回 5603）。
    #[test]
    fn config_key_is_pinned_to_the_migration_seed() {
        assert_eq!(
            CONFIG_FX_GAIN_LOSS_ACCOUNT, "erp.fx.gain_loss_account",
            "键必须与 V0411 的 sys_config.config_key 逐字一致（改一处就静默回缺省）"
        );
        assert!(
            CONFIG_FX_GAIN_LOSS_ACCOUNT.starts_with("erp."),
            "ERP 参数统一 erp. 前缀"
        );
    }

    /// ② 缺省值就是 M1-05 / SO-41 此前的硬编码值：本卡上线后存量行为必须零变化。
    #[test]
    fn default_is_the_previously_hardcoded_5603() {
        assert_eq!(DEFAULT_FX_GAIN_LOSS_ACCOUNT, "5603");
        assert_eq!(DEFAULT_FX_GAIN_LOSS_ACCOUNT, voucher::ACCOUNT_FX_GAIN_LOSS);
    }

    /// ③ 编码规范化：去空白、保留大小写与点、空 / 超长 / 控制字符拒绝。
    #[test]
    fn normalize_keeps_code_intact_and_rejects_garbage() {
        assert_eq!(normalize_code("  5603 ").unwrap(), "5603");
        assert_eq!(normalize_code("5602.01").unwrap(), "5602.01");
        // 不替用户改大小写（库里可能就是 M105 这种自造编码）
        assert_eq!(normalize_code("m105.01").unwrap(), "m105.01");

        assert!(normalize_code("   ").is_err(), "空编码必须拒");
        assert!(
            normalize_code(&"9".repeat(ACCOUNT_CODE_MAX_LEN + 1)).is_err(),
            "超过 VARCHAR(30) 必须拒（否则写进去会被库截断）"
        );
        assert!(normalize_code("5603\n").is_ok(), "尾部换行属于空白");
        assert!(normalize_code("5603\u{0}").is_err(), "控制字符必须拒");
    }

    /// ④ 四条校验规则：合格科目通过，四种坏情形各自被点名。
    #[test]
    fn check_target_enforces_all_four_rules() {
        let ok = target("5603", CATEGORY_PROFIT_LOSS, true, true, false);
        assert!(check_target("5603", Some(&ok)).is_ok(), "5603 财务费用必须通过");

        // ① 不存在（含软删）
        let missing = check_target("9999", None).expect_err("不存在的科目必须拒");
        assert!(missing.contains("9999"), "报文要点名编码：{missing}");
        assert!(
            missing.contains(CONFIG_FX_GAIN_LOSS_ACCOUNT),
            "报文要点名参数键（用户才知道去哪儿改）：{missing}"
        );

        // ② 停用
        let disabled = target("5603", CATEGORY_PROFIT_LOSS, true, false, false);
        let message = check_target("5603", Some(&disabled)).expect_err("停用科目必须拒");
        assert!(message.contains("已停用"), "{message}");

        // ③ 非末级
        let parent = target("5602", CATEGORY_PROFIT_LOSS, false, true, false);
        let message = check_target("5602", Some(&parent)).expect_err("汇总科目必须拒");
        assert!(message.contains("末级"), "{message}");

        // ④ 非损益类（资产类 / 负债类 / 权益类 / 成本类都拒）
        for category in ["ASSET", "LIABILITY", "EQUITY", "COST"] {
            let wrong = target("1002", category, true, true, false);
            let message = check_target("1002", Some(&wrong)).expect_err("非损益类必须拒");
            assert!(message.contains("损益"), "{} 类必须点名损益要求：{message}", category);
        }

        // ⑤ 已标外币核算（自相加成）
        let fc = target("5603", CATEGORY_PROFIT_LOSS, true, true, true);
        let message = check_target("5603", Some(&fc)).expect_err("已标外币核算必须拒");
        assert!(message.contains("外币核算"), "{message}");
    }

    /// ⑤ 校验顺序：先存在、再停用、再末级、再类别、最后外币核算 ——
    /// 多种问题叠加时报的是**第一个**能修的问题（一次改一处，不让人猜）。
    #[test]
    fn check_target_reports_the_first_blocking_rule() {
        let everything_wrong = target("1002", "ASSET", false, false, true);
        let message = check_target("1002", Some(&everything_wrong)).expect_err("必须拒");
        assert!(message.contains("已停用"), "先报停用：{message}");
    }
}
