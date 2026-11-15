//! 汇兑损益科目配置接口层（M1-06 · 迁移 `V0411__fx_gain_loss_account`）：两个接口。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/fx-settings` | `business:erp:fx-rate:list` | 当前汇兑损益科目 + 可选科目列表 |
//! | PUT | `/business/erp/fx-settings` | `business:erp:fx-settings:update` | 改汇兑损益科目（四条校验过了才落库） |
//!
//! # 为什么读用 `fx-rate:list`、写另开一个码
//!
//! 读是**汇兑口径的一部分**：能看「汇率管理」页的人（销售员也有 `fx-rate:list`）看见
//! 「汇兑差额记在哪个科目」是只读、无副作用的，不该再要一个码。
//! 写是**改账务口径**（一年改一次，改错当期损益就错位），必须单独授权 ——
//! 与「录牌价」分开（与 V0410 给期末调汇单独开 `period:fx-reval` 同一条理由）。
//!
//! # 四条入口校验（`fx_settings::check_target`，纯函数、可单测）
//!
//! 1. 科目存在且未软删；
//! 2. 启用（`status = 1`）；
//! 3. 末级（`is_leaf = 1`）；
//! 4. 损益类（`account_category = 'PROFIT_LOSS'`）；
//! 5. 未标「外币核算」（`is_foreign_currency = 0`）—— 标了就会自己调自己，差额递归放大。
//!
//! 任一条不过 ⇒ `400 + 66083`，报文点名科目与参数键、给出「去哪改」的两条出路。
//!
//! # GET 为什么不因为「配置坏了」报错
//!
//! 参数里存的科目可能被停用 / 改非末级 / 删掉（主数据与参数是两个人维护的）。
//! 这时 GET 仍返回 200，但 `account_id = null` + `problem` 说明原因 ——
//! 页面能画出「当前配置 5603（不可用：已停用）」并让用户就地改；
//! 若 GET 直接 400，用户连改配置的入口都打不开（只能改库）。
//! 真正跑调汇时由 `fx_settings::require_gain_loss_account_api` 硬拒（66083）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: sys_config 是全局参数、biz_account 是全局科目主数据（无 dept_id / 归属人列）；
//! 可见性由 business:erp:fx-rate:list（读）与 business:erp:fx-settings:update（写）控制。
//! ```
//!
//! # 不碰的东西
//!
//! 参数不是单据：不生成凭证、不进流程引擎、不注册 `DocKind` / `LEGAL_EDGES`。
//! 本文件也不改任何科目主数据（不改 `is_foreign_currency`、不改启停）——
//! 校验只**读**主数据，要把科目修好得走「会计科目」页。

use axum::Json;
use axum::extract::Extension;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::fx_settings::{self, AccountCandidate, CONFIG_FX_GAIN_LOSS_ACCOUNT};
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

/// 读接口权限码（复用汇率列表：能看汇率管理页就能看见这个口径）。
pub const PERM_FX_SETTINGS_READ: &str = "business:erp:fx-rate:list";

/// 写接口权限码（与 `V0411` 的 `sys_menu.auth_code` 逐字一致）。
pub const PERM_FX_SETTINGS_UPDATE: &str = "business:erp:fx-settings:update";

/// 本模块**新增**的权限码清单（供 `perm_menu_gate.py` 对账；读码复用 fx-rate:list，不重复登记）。
pub const PERMISSIONS: [&str; 1] = [PERM_FX_SETTINGS_UPDATE];

/// 参数值来源：来自 `sys_config`。
pub const SOURCE_CONFIG: &str = "CONFIG";
/// 参数值来源：参数缺失，走内置缺省。
pub const SOURCE_DEFAULT: &str = "DEFAULT";

// ==================== 出参 / 入参 ====================

/// 当前汇兑损益科目配置。
#[derive(Debug, Clone, Serialize)]
pub struct FxSettingsRes {
    /// 参数键（页面要显示「改的是哪条参数」）。
    pub config_key: String,
    /// 当前生效的科目编码（缺省 `5603`）。
    pub account_code: String,
    /// 科目名称（科目不可用时为 `null`）。
    pub account_name: Option<String>,
    /// 科目 id（科目不可用时为 `null`；页面据此判断「配置是否可用」）。
    /// 用字符串而不是 `Uuid`：本仓 JSON 出参统一把主键序列化成字符串（`Uuid` 未开 serde）。
    pub account_id: Option<String>,
    /// 值来源：`CONFIG`（参数里配的）/ `DEFAULT`（参数缺失，走缺省 5603）。
    pub source: String,
    /// 内置缺省编码（页面提示用）。
    pub default_code: String,
    /// 配置不可用的原因（`null` = 可用）。
    pub problem: Option<String>,
    /// 可选科目（启用 + 末级 + 损益类 + 未标外币核算，按编码升序）。
    pub candidates: Vec<AccountCandidate>,
}

/// 改配置入参。
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFxSettingsReq {
    /// 目标科目编码（`biz_account.account_code`，如 `5603`）。
    pub account_code: String,
}

// ==================== 错误出口 ====================

/// `400 + 10001`（参数形状层面：空 / 超长 / 含控制字符）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `400 + 66083`（业务规则层面：科目不可用）。
fn account_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_FX_ACCOUNT_INVALID, message.into())
}

// ==================== 业务实现 ====================

/// 组装当前配置（GET 与 PUT 的返回体同一份，页面切换后只刷新一次）。
async fn load_settings() -> std::result::Result<FxSettingsRes, ApiError> {
    let configured = fx_settings::configured_gain_loss_account().await;
    let account_code = configured
        .clone()
        .unwrap_or_else(|| fx_settings::DEFAULT_FX_GAIN_LOSS_ACCOUNT.to_string());
    let target = fx_settings::load_target(db(), &account_code)
        .await
        .map_err(|error| utils::db_error(error, "查询汇兑损益科目"))?;
    let problem = fx_settings::check_target(&account_code, target.as_ref()).err();
    let account = crate::business::erp::voucher::account_by_code(db(), &account_code)
        .await
        .map_err(|error| utils::db_error(error, "按编码回读汇兑损益科目"))?;
    let candidates = fx_settings::candidates(db())
        .await
        .map_err(|error| utils::db_error(error, "查询可选汇兑损益科目"))?;
    Ok(FxSettingsRes {
        config_key: CONFIG_FX_GAIN_LOSS_ACCOUNT.to_string(),
        account_code,
        account_name: account.as_ref().map(|row| row.name.clone()),
        account_id: account.as_ref().map(|row| row.id.to_string()),
        source: if configured.is_some() {
            SOURCE_CONFIG.to_string()
        } else {
            SOURCE_DEFAULT.to_string()
        },
        default_code: fx_settings::DEFAULT_FX_GAIN_LOSS_ACCOUNT.to_string(),
        problem,
        candidates,
    })
}

/// 改配置：规范化 → 校验 → 落库（值相同则幂等空操作）。
async fn save(account_code: &str, operator: Option<Uuid>) -> std::result::Result<FxSettingsRes, ApiError> {
    let code = fx_settings::normalize_code(account_code).map_err(bad_request)?;
    let target = fx_settings::load_target(db(), &code)
        .await
        .map_err(|error| utils::db_error(error, "查询待配置的汇兑损益科目"))?;
    fx_settings::check_target(&code, target.as_ref()).map_err(account_invalid)?;

    // 幂等：值已经是它就不写库（不刷 updated_at，连点两次保存不互相踢）。
    if fx_settings::configured_gain_loss_account().await.as_deref() != Some(code.as_str()) {
        fx_settings::save_gain_loss_account(&code, operator)
            .await
            .map_err(|error| utils::db_error(error, "保存汇兑损益科目参数"))?;
    }
    load_settings().await
}

// ==================== 接口 ====================

/// 当前汇兑损益科目配置（只读）。
#[nik::permission(
    path = "/business/erp/fx-settings",
    method = "GET",
    permission = "business:erp:fx-rate:list"
)]
#[nik::get("/business/erp/fx-settings")]
pub async fn fx_settings_get_api() -> ResResult<FxSettingsRes> {
    Ok(Res::ok(load_settings().await?))
}

/// 改汇兑损益科目。
#[nik::permission(
    path = "/business/erp/fx-settings",
    method = "PUT",
    permission = "business:erp:fx-settings:update"
)]
#[nik::put("/business/erp/fx-settings")]
pub async fn fx_settings_put_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<UpdateFxSettingsReq>,
) -> ResResult<FxSettingsRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(save(&req.account_code, operator).await?))
}

// ==================== 单测（纯逻辑 + 真库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::config::model::reload_runtime_snapshot;

    // ---------- 纯逻辑 ----------

    /// ① 权限码：写码逐字钉住、只有一个新码、读码复用汇率列表。
    #[test]
    fn permissions_are_well_formed() {
        assert_eq!(
            PERM_FX_SETTINGS_UPDATE, "business:erp:fx-settings:update",
            "码值必须与 V0411 的 sys_menu.auth_code 逐字一致（改一处就授不出去）"
        );
        assert!(
            PERM_FX_SETTINGS_UPDATE.starts_with("business:erp:fx-settings:"),
            "码必须落在 business:erp:fx-settings: 前缀下"
        );
        assert_eq!(
            PERM_FX_SETTINGS_READ, "business:erp:fx-rate:list",
            "读接口复用汇率列表码（V0316 已种），不新增码"
        );
        assert_eq!(PERMISSIONS, [PERM_FX_SETTINGS_UPDATE], "只新增一个码");
    }

    /// ② 入参契约：`account_code` 必填（缺字段报错，**不**静默当空串 → 否则会把配置写成空值，
    /// 而空值在读取侧等价于「未配置」，看起来像「保存成功但没生效」）。
    #[test]
    fn request_requires_account_code() {
        let ok: UpdateFxSettingsReq = serde_json::from_str(r#"{"account_code":"5601"}"#).expect("带字段应可反序列化");
        assert_eq!(ok.account_code, "5601");
        assert!(
            serde_json::from_str::<UpdateFxSettingsReq>(r#"{}"#).is_err(),
            "缺 account_code 必须反序列化失败（400）"
        );
    }

    /// ③ 出参字段名钉住：前端按这些名字取值，改名等于页面白屏。
    #[test]
    fn response_field_names_are_pinned() {
        let res = FxSettingsRes {
            config_key: CONFIG_FX_GAIN_LOSS_ACCOUNT.to_string(),
            account_code: "5603".to_string(),
            account_name: Some("财务费用".to_string()),
            account_id: None,
            source: SOURCE_DEFAULT.to_string(),
            default_code: "5603".to_string(),
            problem: None,
            candidates: vec![AccountCandidate {
                code: "5603".to_string(),
                name: "财务费用".to_string(),
            }],
        };
        let json = serde_json::to_value(&res).expect("可序列化");
        for key in [
            "config_key",
            "account_code",
            "account_name",
            "account_id",
            "source",
            "default_code",
            "problem",
            "candidates",
        ] {
            assert!(json.get(key).is_some(), "出参缺字段 {key}");
        }
        assert_eq!(json["candidates"][0]["code"], "5603");
    }

    // ---------- 真库 ----------

    /// 真库用例串行锁（与期末调汇 / 凭证用例共用同一把：设置会改全局参数，互相必须串行）。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = crate::business::erp::voucher::voucher_query::tests::DB_LOCK
            .lock()
            .await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 清场：把参数恢复成迁移种子那一行（`5603`），并刷快照（否则快照里还是上一个用例的值）。
    async fn cleanup() {
        if let Err(error) = db()
            .execute_with_params(
                "INSERT INTO sys_config (config_key, config_value, config_type, description, category, \
                                         sort_order, config_level, is_modifiable, is_encrypted, is_restart, status) \
                 VALUES (?, '5603', 'STRING', 'M1-06 夹具', 'erp_fx', 10, 2, TRUE, FALSE, FALSE, 1) \
                 ON CONFLICT (config_key) WHERE deleted_at IS NULL \
                 DO UPDATE SET config_value = '5603', status = 1",
                vec![Value::Text(CONFIG_FX_GAIN_LOSS_ACCOUNT.to_string())],
            )
            .await
        {
            panic!("夹具恢复汇兑损益科目参数失败：{error}");
        }
        if let Err(error) = reload_runtime_snapshot().await {
            panic!("夹具刷新参数快照失败：{error}");
        }
    }

    /// 直改参数值（绕过接口校验，用来造「配置坏了」的场景），写完刷快照。
    async fn force_config_value(value: &str) {
        db().execute_with_params(
            "UPDATE sys_config SET config_value = ?, status = 1 \
              WHERE config_key = ? AND deleted_at IS NULL",
            vec![
                Value::Text(value.to_string()),
                Value::Text(CONFIG_FX_GAIN_LOSS_ACCOUNT.to_string()),
            ],
        )
        .await
        .expect("直改参数值");
        reload_runtime_snapshot().await.expect("刷新参数快照");
    }

    /// 真库：迁移种子就是 5603，`load_settings` 读得到科目名与候选列表。
    #[tokio::test]
    async fn get_returns_seeded_5603_with_candidates() {
        let Some(_guard) = setup().await else {
            return;
        };
        let res = load_settings().await.expect("读配置");
        assert_eq!(res.config_key, CONFIG_FX_GAIN_LOSS_ACCOUNT);
        assert_eq!(res.account_code, "5603");
        assert_eq!(res.source, SOURCE_CONFIG, "迁移已种参数 ⇒ 来源是 CONFIG");
        assert_eq!(res.default_code, "5603");
        assert!(res.account_name.is_some(), "5603 在模板库里存在");
        assert!(res.account_id.is_some());
        assert!(res.problem.is_none(), "迁移种子必须是一条可用配置：{:?}", res.problem);
        assert!(
            res.candidates.iter().any(|row| row.code == "5603"),
            "候选里必须有 5603 自身（否则页面一打开就看不到当前值）"
        );
        assert!(
            res.candidates.iter().all(|row| !row.code.starts_with("1601")),
            "固定资产属于资产类，不该出现在损益类候选里"
        );
    }

    /// 真库：参数缺失 ⇒ 回落缺省 5603，`source = DEFAULT`（业务链路等价于本卡之前的行为）。
    #[tokio::test]
    async fn missing_config_falls_back_to_default_5603() {
        let Some(_guard) = setup().await else {
            return;
        };
        db().execute_with_params(
            "DELETE FROM sys_config WHERE config_key = ?",
            vec![Value::Text(CONFIG_FX_GAIN_LOSS_ACCOUNT.to_string())],
        )
        .await
        .expect("删参数行");
        reload_runtime_snapshot().await.expect("刷新快照");

        let res = load_settings().await.expect("读配置");
        assert_eq!(res.source, SOURCE_DEFAULT, "无参数 ⇒ 缺省");
        assert_eq!(res.account_code, "5603");
        assert_eq!(res.account_name.as_deref(), Some("财务费用"));
        assert_eq!(fx_settings::gain_loss_account_code().await, "5603");
        assert_eq!(fx_settings::configured_gain_loss_account().await, None);
        cleanup().await;
    }

    /// 真库：改成 5601（销售费用，末级损益类、未标外币核算）⇒ 落库 + 读回 + 业务链路取到新科目。
    #[tokio::test]
    async fn save_writes_config_and_business_path_reads_it() {
        let Some(_guard) = setup().await else {
            return;
        };
        let res = save("5601", None).await.expect("保存 5601");
        assert_eq!(res.account_code, "5601");
        assert_eq!(res.source, SOURCE_CONFIG);
        assert!(res.problem.is_none(), "5601 可用：{:?}", res.problem);
        assert_eq!(res.account_name.as_deref(), Some("销售费用"));

        // 参数行真的改了（不是只在返回体里）
        let stored = db()
            .query_first_sql(
                "SELECT config_value FROM sys_config WHERE config_key = ? AND deleted_at IS NULL".to_string(),
                vec![Value::Text(CONFIG_FX_GAIN_LOSS_ACCOUNT.to_string())],
            )
            .await
            .expect("读参数")
            .expect("参数行必须存在");
        assert_eq!(stored.get_typed::<String>("config_value").unwrap(), "5601");

        // 业务链路（DbResult 出口 + ApiError 出口）都拿到新科目
        assert_eq!(fx_settings::gain_loss_account_code().await, "5601");
        let api_account = fx_settings::require_gain_loss_account_api(db())
            .await
            .expect("5601 可用于调汇");
        assert_eq!(api_account.code, "5601");
        let db_account = fx_settings::require_gain_loss_account(db())
            .await
            .expect("5601 可用于重估");
        assert_eq!(db_account.code, "5601");
        assert_eq!(db_account.name, "销售费用");
        // 收尾：把参数恢复成迁移种子的 5603（后续用例 / 其它模块按缺省断言）
        cleanup().await;
    }

    /// 真库：同一个值保存两次 = 幂等（第二次不写库），连点保存不互踢。
    #[tokio::test]
    async fn saving_the_same_value_twice_is_idempotent() {
        let Some(_guard) = setup().await else {
            return;
        };
        save("5601", None).await.expect("第一次保存");
        let before = db()
            .query_first_sql(
                "SELECT updated_at FROM sys_config WHERE config_key = ? AND deleted_at IS NULL".to_string(),
                vec![Value::Text(CONFIG_FX_GAIN_LOSS_ACCOUNT.to_string())],
            )
            .await
            .expect("读 updated_at")
            .expect("参数行");
        let before_at: chrono::DateTime<chrono::Utc> = before.get_typed("updated_at").expect("时间");

        let again = save("5601", None).await.expect("第二次保存");
        assert_eq!(again.account_code, "5601");
        let after = db()
            .query_first_sql(
                "SELECT updated_at FROM sys_config WHERE config_key = ? AND deleted_at IS NULL".to_string(),
                vec![Value::Text(CONFIG_FX_GAIN_LOSS_ACCOUNT.to_string())],
            )
            .await
            .expect("读 updated_at")
            .expect("参数行");
        let after_at: chrono::DateTime<chrono::Utc> = after.get_typed("updated_at").expect("时间");
        assert_eq!(before_at, after_at, "值相同不该再写库（updated_at 不变）");
        cleanup().await;
    }

    /// 真库：四条校验在**接口层**也真生效（不只纯函数单测），且拒绝时参数值一个字节都不动。
    #[tokio::test]
    async fn invalid_targets_are_rejected_and_config_stays_untouched() {
        let Some(_guard) = setup().await else {
            return;
        };
        // 资产类（1002 库存现金）、汇总科目（5602 管理费用）、不存在（M106.99）
        for (code, expect) in [("1002", "损益"), ("5602", "末级"), ("M106.99", "不存在")] {
            let error = save(code, None).await.expect_err("必须拒绝");
            assert_eq!(error.status, 400, "{code} → {}", error.msg);
            assert_eq!(error.code, error_code::ERP_FX_ACCOUNT_INVALID, "{code} 码必须是 66083");
            assert!(
                error.msg.contains(expect),
                "{code} 的报文应包含「{expect}」，实际：{}",
                error.msg
            );
            assert_eq!(
                fx_settings::gain_loss_account_code().await,
                "5603",
                "被拒的保存不许改参数（{code}）"
            );
        }
        // 空编码 / 超长编码走参数校验（10001），不落到科目校验
        let empty = save("   ", None).await.expect_err("空编码必须拒");
        assert_eq!(empty.code, error_code::PARAM_INVALID);
    }

    /// 真库：配置指向一个不可用科目（被停用 / 被删）⇒ GET 仍 200 但带 problem，
    /// 而调汇链路硬拒 66083（**不静默回落 5603**）。
    #[tokio::test]
    async fn broken_config_is_reported_by_get_and_rejected_by_reval_path() {
        let Some(_guard) = setup().await else {
            return;
        };
        force_config_value("M106.99").await;

        let res = load_settings().await.expect("GET 不因配置坏而报错");
        assert_eq!(res.account_code, "M106.99");
        assert_eq!(res.source, SOURCE_CONFIG);
        assert!(res.account_id.is_none(), "科目不存在 ⇒ 没有 id");
        let problem = res.problem.expect("必须带 problem 说明");
        assert!(problem.contains("M106.99"), "problem 要点名编码：{problem}");
        assert!(
            problem.contains(CONFIG_FX_GAIN_LOSS_ACCOUNT),
            "problem 要点名参数键：{problem}"
        );

        let error = fx_settings::require_gain_loss_account_api(db())
            .await
            .expect_err("调汇必须硬拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_FX_ACCOUNT_INVALID);

        let db_error = fx_settings::require_gain_loss_account(db())
            .await
            .expect_err("重估必须硬拒");
        assert!(
            matches!(db_error, DbError::InvalidValue(_)),
            "DbResult 出口用 InvalidValue 表达「配置不可用」，实际：{db_error:?}"
        );
        cleanup().await;
    }
}
