//! 主数据编码策略：**允许 / 禁止手工指定编码**（CODE-08）。
//!
//! # 一条参数，六个主数据
//!
//! | 主数据 | 号段 | 自动产物 |
//! |---|---|---|
//! | 供应商 | `SUP` 纯流水 4 位 | `SUP-0001` |
//! | 客户 | `CUS` 纯流水 4 位 | `CUS-0001` |
//! | 物料 | `MAT` 纯流水 4 位 | `MAT-0001` |
//! | 仓库 | `WH` 纯流水 2 位 | `WH-01` |
//! | 资产分类 | `AC` 纯流水 2 位 | `AC-01` |
//! | 费用类型 | `ET` 纯流水 2 位 | `ET-01` |
//!
//! 参数：`erp.master_data.allow_manual_code`（`BOOLEAN`，默认 `true`，由迁移
//! `V0342__erp_master_data_code_policy` 种入 `sys_config`）。
//!
//! ```text
//! true  (默认) → 建单可留空（自动取号）也可手填企业自有编码：GYS-001 / CK-001 / YB-001-02
//! false        → 编码完全由系统生成；建单请求里出现非空编码 ⇒ 400 + 66054「系统已禁止手工指定编码」
//! ```
//!
//! # 为什么默认 `true`
//!
//! 改动前就是「可手填」，默认 `false` 会让升级后的环境突然拒收所有带编码的建单请求
//! （历史前端、脚本、对接方全都带编码），那是升级事故。要强制统一的企业自己把开关关掉。
//!
//! # 为什么「读不到就放开」而不是「读不到就禁止」
//!
//! 参数缺失 / 已停用 / 值非法（不是 `true/false/1/0`）一律回退 [`DEFAULT_ALLOW_MANUAL_CODE`]
//! 并打 `warn`。方向很重要：这个开关**收紧**时才会影响业务，读失败如果按「禁止」处理，
//! 一次配置表抖动就会让 6 个主数据全部建不了单 —— 比「允许手填」的代价大得多。
//!
//! # 只拦「建单」，不拦「改单」
//!
//! 6 个主数据的改单接口本来就不接受改编码（编码是档案身份），前端编辑态编码框也是只读的；
//! 因此这里只需要在建单入口拦手工编码，改单路径无需额外判定（写了也没入口）。
//!
//! # 读取时机
//!
//! 每个建单入口在 `req.validate()` 之后、`db().begin()` 之前调 [`guard_manual`]：
//! 拿不到编码策略时直接 400，**不开事务、不取号**（避免「先占号再报错」留下空号）。

use nik::prelude::*;

use crate::business::erp::seq;
use crate::shared::error_code;
use crate::support::config::model::{self as config_model, SysConfigModel};

/// 参数键（`sys_config.config_key`）。
pub const ALLOW_MANUAL_CODE_KEY: &str = "erp.master_data.allow_manual_code";

/// 参数缺失 / 停用 / 值非法时的回退值：**允许**手工指定（= 改动前行为）。
pub const DEFAULT_ALLOW_MANUAL_CODE: bool = true;

/// 纯函数：把参数文本解析成「是否允许手工编码」。
///
/// # 参数
///
/// - `raw`：参数原文（`None` = 参数不存在 / 已停用 / 值为空）。
///
/// # 返回值
///
/// 可解析时取解析值；缺失或非法时取 [`DEFAULT_ALLOW_MANUAL_CODE`]（并打 `warn`）。
pub fn resolve_allow_manual(raw: Option<&str>) -> bool {
    let Some(text) = raw else {
        return DEFAULT_ALLOW_MANUAL_CODE;
    };
    match config_model::parse_bool(text) {
        Some(allow) => allow,
        None => {
            tracing::warn!(
                config_key = ALLOW_MANUAL_CODE_KEY,
                value = text,
                fallback = DEFAULT_ALLOW_MANUAL_CODE,
                "主数据编码开关取值非法（只认 true/false/1/0），已回退默认值"
            );
            DEFAULT_ALLOW_MANUAL_CODE
        }
    }
}

/// 读取生效值（走运行时快照：命中是纯内存读，管理端改参数后立即重载）。
///
/// # 返回值
///
/// `true` = 允许手工指定编码（默认），`false` = 编码完全由系统生成。
pub async fn allow_manual_code() -> bool {
    resolve_allow_manual(SysConfigModel::read_text(ALLOW_MANUAL_CODE_KEY).await.as_deref())
}

/// 拦截文案（**唯一出口**）：说清「为什么不行」「现在该怎么办」「要放开去哪里」。
///
/// 前端把 400 原文直接贴到错误面板 / 编码框下方，所以这句话必须自己站得住。
pub fn rejection_text() -> String {
    format!(
        "系统已禁止手工指定编码：本环境要求主数据编码完全由系统生成\
         （系统参数 {ALLOW_MANUAL_CODE_KEY} = false）。请清空编码栏后直接保存，\
         系统会按号段规则自动取号；确需沿用企业自有编码请联系管理员打开该参数。"
    )
}

/// 手工编码闸：开关关闭时，请求里**非空**的编码一律 `400 + 66054`。
///
/// # 参数
///
/// - `allow`：[`allow_manual_code`] 的生效值。
/// - `submitted`：请求体里的编码原值（`None` / `""` / 纯空白 = 没填，交给取号器）。
///
/// # 返回值
///
/// 放行返回 `Ok(())`；开关关闭且确实填了编码返回 400（[`error_code::ERP_MANUAL_CODE_FORBIDDEN`]）。
///
/// # 异常情况
///
/// 只在「填了编码 + 开关关闭」时返回错误；留空永远放行（自动取号不受开关影响）。
pub fn guard_manual(allow: bool, submitted: Option<&str>) -> std::result::Result<(), ApiError> {
    if allow {
        return Ok(());
    }
    if seq::manual_code(submitted).is_none() {
        return Ok(());
    }
    Err(ApiError::new(
        400,
        error_code::ERP_MANUAL_CODE_FORBIDDEN,
        rejection_text(),
    ))
}

/// 编码策略的对外载荷（`GET /business/erp/master-data/code-policy`）。
///
/// 前端 6 个主数据表单用它决定「编码栏是否只读 / 生成编码按钮是否隐藏」。
///
/// # 返回值
///
/// `{ allow_manual_code, config_key, default_allow_manual_code }`：`allow_manual_code`
/// 是生效值，另两项是给前端兜底与排查用的元信息（前端拿不到接口时按 `default` 处理）。
pub async fn policy_payload() -> serde_json::Value {
    json!({
        "allow_manual_code": allow_manual_code().await,
        "config_key": ALLOW_MANUAL_CODE_KEY,
        "default_allow_manual_code": DEFAULT_ALLOW_MANUAL_CODE,
    })
}

/// 编码策略（登录即可读，不挂权限码）。
///
/// # 为什么不挂 `#[nik::permission]`
///
/// 这是**界面策略**而不是业务数据：6 个主数据表单各自有 create / list 权限，
/// 挂任何单一权限码都会让「只有物料权限的人」在物料页拿不到开关状态，
/// 从而绕过前端的只读态（后端仍会 400，但用户会莫名其妙撞错）。
/// 收紧编码的能力在后端 [`guard_manual`]，本接口只决定输入框长什么样，泄露的是
/// 「这个环境允不允许手填编码」——不是敏感信息。
#[nik::get("/business/erp/master-data/code-policy")]
pub async fn master_data_code_policy(
    Extension(AuthClaims(_claims)): Extension<AuthClaims>,
) -> ResResult<serde_json::Value> {
    Ok(Res::ok(policy_payload().await))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 纯逻辑（不碰库） ----------

    /// 参数缺失（`None`）= 允许手工编码（= 改动前行为）。
    #[test]
    fn missing_config_falls_back_to_allow() {
        assert!(resolve_allow_manual(None));
    }

    /// 四种合法写法都认，大小写不敏感。
    #[test]
    fn parses_all_four_bool_spellings() {
        assert!(resolve_allow_manual(Some("true")));
        assert!(resolve_allow_manual(Some(" TRUE ")));
        assert!(resolve_allow_manual(Some("1")));
        assert!(!resolve_allow_manual(Some("false")));
        assert!(!resolve_allow_manual(Some("FALSE")));
        assert!(!resolve_allow_manual(Some("0")));
    }

    /// 非法值回退「允许」（不能因为配置写错就把 6 个主数据全锁死）。
    #[test]
    fn invalid_text_falls_back_to_allow() {
        assert!(resolve_allow_manual(Some("yes")));
        assert!(resolve_allow_manual(Some("")));
        assert!(resolve_allow_manual(Some("停用")));
    }

    /// 开关打开：手工编码 / 留空都放行（= 现有行为）。
    #[test]
    fn guard_passes_everything_when_allowed() {
        assert!(guard_manual(true, Some("GYS-001")).is_ok());
        assert!(guard_manual(true, Some("")).is_ok());
        assert!(guard_manual(true, None).is_ok());
    }

    /// 开关关闭：留空（`None` / `""` / 纯空白）仍放行 —— 自动取号不受影响。
    #[test]
    fn guard_passes_blank_when_forbidden() {
        assert!(guard_manual(false, None).is_ok());
        assert!(guard_manual(false, Some("")).is_ok());
        assert!(guard_manual(false, Some("   ")).is_ok());
    }

    /// 开关关闭 + 填了编码 ⇒ 400 + 66054，文案点名「系统已禁止手工指定编码」+ 参数键。
    #[test]
    fn guard_rejects_manual_when_forbidden() {
        let err = guard_manual(false, Some(" GYS-001 ")).expect_err("开关关闭时必须拒收手工编码");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_MANUAL_CODE_FORBIDDEN);
        assert!(
            err.msg.starts_with("系统已禁止手工指定编码"),
            "文案必须与卡面逐字一致，实际：{}",
            err.msg
        );
        assert!(
            err.msg.contains(ALLOW_MANUAL_CODE_KEY),
            "文案要指出去哪个参数放开，实际：{}",
            err.msg
        );
    }

    /// 配置文本写错时**不能**把建单拦死：回退值是「允许」，闸直接放行（只打 warn）。
    #[test]
    fn invalid_config_does_not_block_creation() {
        let allow = resolve_allow_manual(Some("maybe"));
        assert!(allow, "非法配置必须回退「允许」");
        assert!(
            guard_manual(allow, Some("GYS-001")).is_ok(),
            "回退「允许」后手工编码必须放行"
        );
    }

    // ---------- 真库用例（`ERP_DB_URL`，与 erp 其它真库用例同一套约定） ----------
    //
    // ⚠️ 本模块的真库用例**刻意不改 `sys_config`**（不 set 不 restore）。
    //    `allow_manual_code()` 读的是**进程级**运行时快照：用例改库 + 重载快照后，
    //    同进程里并发跑的其它用例（建供应商 / 客户 / 物料 …）会突然收到 400 + 66054
    //    —— 实测过一次：模块用例与另一个测试进程并行跑，customer_handler 的 4 条用例
    //    全红，逐条读日志才发现是被这里的 `false` 波及。
    //    所以「允许 / 禁止」两条分支靠 `create_supplier_with_policy(…, allow)` 直接给入参，
    //    只有「读参数」这一件事才碰库，而且是**只读**。

    use nik::uuid::Uuid;
    use tokio::sync::Mutex;

    /// 真库用例串行（共用全局 `db()` 与同前缀编码）。
    static DB_LOCK: Mutex<()> = Mutex::const_new(());

    /// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
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
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 拿锁 + 连库；`None` = 没配库。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        Some(guard)
    }

    /// 迁移 V0342 真的把开关种进了库、且默认 `true`（**只读**，不改任何行）。
    ///
    /// 这条同时钉住两件事：① 参数键名逐字是 `erp.master_data.allow_manual_code`；
    /// ② 升级后行为不变（默认允许手填）。读到 `None` 说明迁移没跑或参数被停用。
    #[tokio::test]
    async fn migration_seeds_switch_as_true() {
        let Some(_guard) = setup().await else { return };
        let raw = SysConfigModel::read_text(ALLOW_MANUAL_CODE_KEY).await;
        assert_eq!(
            raw.as_deref(),
            Some("true"),
            "V0342 必须把 {ALLOW_MANUAL_CODE_KEY} 种成 true（读到 {raw:?}）"
        );
        assert!(allow_manual_code().await, "默认值 true ⇒ 生效值必须是「允许手工编码」");
    }

    /// 策略载荷携带前端要的三样东西，且 `allow_manual_code` 与读取口径一致。
    #[tokio::test]
    async fn policy_payload_carries_metadata() {
        let Some(_guard) = setup().await else { return };
        let payload = policy_payload().await;
        assert_eq!(
            payload["allow_manual_code"],
            serde_json::json!(allow_manual_code().await),
            "载荷里的生效值必须与 allow_manual_code() 同源"
        );
        assert_eq!(payload["config_key"], serde_json::json!(ALLOW_MANUAL_CODE_KEY));
        assert_eq!(
            payload["default_allow_manual_code"],
            serde_json::json!(DEFAULT_ALLOW_MANUAL_CODE)
        );
    }

    /// 建单入口按**传入的策略**走两条分支（零全局副作用）：
    /// `false` ⇒ 手工编码 400 + 66054、留空仍自动取号；`true` ⇒ 手工编码原样保留。
    #[tokio::test]
    async fn create_supplier_respects_policy_argument() {
        use crate::business::erp::dto::CreateSupplierReq;
        use crate::business::erp::handler;
        use rust_decimal::Decimal;

        let Some(_guard) = setup().await else { return };

        let req = |code: Option<&str>, name: &str| CreateSupplierReq {
            supplier_code: code.map(str::to_string),
            supplier_name: name.to_string(),
            short_name: None,
            contact_person: None,
            contact_phone: None,
            email: None,
            tax_no: None,
            address: None,
            credit_days: 30,
            credit_limit: Some(Decimal::ZERO),
            payable_account_id: None,
            invoice_title: None,
            invoice_bank_name: None,
            invoice_bank_account: None,
            status: 1,
            remark: None,
        };

        // 编码带随机后缀：一次性库重跑也不会撞唯一索引
        let tag = &Uuid::now_v7().simple().to_string()[..8];
        let manual_forbidden = format!("GYS-C08-{tag}-OFF");
        let manual_allowed = format!("GYS-C08-{tag}-ON");

        let blocked =
            handler::create_supplier_with_policy(None, req(Some(&manual_forbidden), "CODE-08 手工被拒"), false).await;
        let auto = handler::create_supplier_with_policy(None, req(None, "CODE-08 自动取号"), false).await;
        let manual =
            handler::create_supplier_with_policy(None, req(Some(&manual_allowed), "CODE-08 手工放行"), true).await;

        // 清场（真库用例外不留痕）：只删本用例造的两行
        for created in [&auto, &manual] {
            if let Ok(res) = created
                && let Ok(id) = Uuid::parse_str(&res.id)
            {
                let _ = db()
                    .execute_with_params("DELETE FROM biz_supplier WHERE id = ?::uuid", vec![Value::from(&id)])
                    .await;
            }
        }

        let err = blocked.expect_err("策略为「禁止」时手工编码必须被拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_MANUAL_CODE_FORBIDDEN);
        assert!(
            err.msg.starts_with("系统已禁止手工指定编码"),
            "文案必须与卡面逐字一致，实际：{}",
            err.msg
        );

        let auto = auto.expect("策略为「禁止」时留空仍应自动取号");
        assert!(
            auto.supplier_code.starts_with("SUP-"),
            "自动编码应是 SUP- 前缀的号段产物，实际 {}",
            auto.supplier_code
        );

        let manual = manual.expect("策略为「允许」时手工编码必须可用");
        assert_eq!(manual.supplier_code, manual_allowed);

        // 被拒的那次不得落库（400 是真的没写）
        let hit: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_supplier WHERE supplier_code = ?",
                vec![Value::Text(manual_forbidden.clone())],
            )
            .await
            .expect("统计被拒的编码");
        assert_eq!(hit, 0, "被 400 拒掉的编码不该出现在库里");
    }
}
