//! 员工档案出参（`employee_dto`）的真库用例（HR-11）。
//!
//! 纯单测（`employee_dto.rs` 的 `mod tests`）已经证明 DTO 的**形状**与打码**算法**，
//! 但证明不了最关键的一环：**库里那两条 `sys_field_mask` 规则（`V0388`）折算出来的参数
//! 就是 DTO 兜底常量**。这一环断了不会报错，只会静默变成「另一种打码形态」或（最坏的）
//! 明文 —— 所以必须真跑一次库。
//!
//! ```bash
//! HR_DB_URL=postgres://alun:***@127.0.0.1:5432/<已迁移的库> \
//!   cargo test --lib business::hr::employee_dto_db -- --test-threads=1
//! ```
//!
//! 环境变量优先 `HR_DB_URL`，回退 `ERP_DB_URL`（与 HR-10 的真库用例同一口径）。
//! 库要求：迁移跑完（`V0388` 已应用）。本文件**只读**，不造任何夹具、不改库。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::hr::employee_dto as dto;
    use crate::business::masking::masker::MaskType;
    use crate::business::masking::registry;
    use tokio::sync::OnceCell;

    /// 连库（进程内只做一次初始化）。`false` = 没配库地址，用例直接跳过。
    async fn ensure_db() -> bool {
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("HR_DB_URL").or_else(|_| std::env::var("ERP_DB_URL")) else {
                    println!("SKIP: 未设置 HR_DB_URL / ERP_DB_URL（不连真实数据库）");
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
                        println!("SKIP: HR_DB_URL / ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 库里的 `biz_employee` 全局脱敏规则：`(列名, mask_type, mask_param)`。
    async fn seeded_rules() -> Vec<(String, i16, Option<String>)> {
        let rows = db()
            .find_with_params(
                "SELECT column_name, mask_type, mask_param FROM sys_field_mask \
                 WHERE table_name = 'biz_employee' AND role_id IS NULL \
                   AND deleted_at IS NULL AND status = 1 \
                 ORDER BY column_name ASC",
                vec![],
            )
            .await
            .expect("查 sys_field_mask（库必须已跑完 V0388）");
        rows.iter()
            .map(|row| {
                Ok((
                    row.get_typed::<String>("column_name")?,
                    row.get_typed::<i16>("mask_type")?,
                    row.get_typed::<Option<String>>("mask_param")?,
                ))
            })
            .collect::<DbResult<Vec<_>>>()
            .expect("解析规则行")
    }

    // ==================== ① 库里的规则 = DTO 的兜底常量 ====================

    /// `V0388` 种的两条全局规则必须与 [`dto::DEFAULT_ID_CARD_PARAM`] /
    /// [`dto::DEFAULT_BANK_ACCOUNT_PARAM`] 逐字一致。
    ///
    /// 不一致的后果：规则被删（或新环境漏跑 V0388）时，DTO 会按自己的兜底打码 ——
    /// 形态与生产不同，前端截图/对账/审计三方对不上，且**没有任何报错**。
    #[tokio::test]
    async fn seeded_rules_match_dto_defaults() {
        if !ensure_db().await {
            return;
        }
        let rules = seeded_rules().await;
        println!("[HR-11] sys_field_mask(biz_employee) = {rules:?}");

        let id_card = rules
            .iter()
            .find(|(column, _, _)| column == dto::COL_ID_CARD)
            .expect("V0388 应种 biz_employee.id_card 的全局规则");
        assert_eq!(
            id_card.1,
            MaskType::IdCard.as_i16(),
            "id_card 的 mask_type 必须是 2（身份证）"
        );
        assert_eq!(
            id_card.2.as_deref(),
            Some(dto::DEFAULT_ID_CARD_PARAM),
            "id_card 的 mask_param 必须与 DTO 兜底常量一致"
        );

        let bank = rules
            .iter()
            .find(|(column, _, _)| column == dto::COL_BANK_ACCOUNT)
            .expect("V0388 应种 biz_employee.bank_account 的全局规则");
        assert_eq!(
            bank.1,
            MaskType::BankCard.as_i16(),
            "bank_account 的 mask_type 必须是 5（银行卡）"
        );
        assert_eq!(
            bank.2.as_deref(),
            Some(dto::DEFAULT_BANK_ACCOUNT_PARAM),
            "bank_account 的 mask_param 必须与 DTO 兜底常量一致"
        );

        // 只允许这两条（多一条 = 有人加了规则，出参形态会变，前端要同步）
        assert_eq!(rules.len(), 2, "biz_employee 的全局脱敏规则应当只有 2 条：{rules:?}");
    }

    // ==================== ② 真库规则 → DTO 打码值 ====================

    /// 用**真库解析出来的规则集合**（`MaskSet`）构造详情 DTO：
    ///
    /// - 打码形态必须是 `110101****1234`（规则 `2 + 6,4` 的实际输出）；
    /// - 出参 JSON 里**一个明文字节都不能有**；
    /// - 银行卡不许出现中段数字。
    ///
    /// 这条用例把「`sys_field_mask` → `MaskSet` → DTO」整条链在真库上跑通一次；
    /// HTTP 层的接线（handler 有没有把 `MaskSet` 传进来）由 HR-13 验收。
    #[tokio::test]
    async fn detail_masks_with_real_db_rules() {
        if !ensure_db().await {
            return;
        }
        // 任意一个真实用户：规则集里的**全局**规则与用户无关（角色规则才是并集）。
        // 只读不建夹具：`SELECT` 一个已有账号（空库首启也有 admin）。
        let rows = db()
            .find_with_params(
                "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at ASC LIMIT 1",
                vec![],
            )
            .await
            .expect("查 sys_user");
        let Some(row) = rows.first() else {
            println!("SKIP: 库里没有任何 sys_user，无法解析规则集合");
            return;
        };
        let user: Uuid = row.get_typed("id").expect("user id");
        let set = registry::load_user_set(&user)
            .await
            .expect("按用户加载脱敏规则（库必须已跑完 V0388）");
        let hits: Vec<String> = set
            .rules()
            .iter()
            .filter(|rule| rule.table == dto::TABLE)
            .map(|rule| format!("{}({},{:?})", rule.column, rule.mask_type, rule.mask_param))
            .collect();
        println!("[HR-11] 生效的 biz_employee 规则 = {hits:?}");

        let detail = dto::EmployeeDetailRes::from_row(&dto::sample_row(), &dto::sample_names(), &set);
        let id_card = detail
            .sensitive_masked
            .id_card_masked
            .clone()
            .expect("身份证应有打码值");
        let bank = detail
            .sensitive_masked
            .bank_account_masked
            .clone()
            .expect("银行卡应有打码值");
        println!("[HR-11] id_card_masked = {id_card} / bank_account_masked = {bank}");

        // 规则 2 + 6,4：保留前 6 后 4，中间固定 4 星（与 V0388 的落地记录一致）
        assert_eq!(id_card, "110101****1234");
        assert!(bank.starts_with("6222"), "保留前 4 位：{bank}");
        assert!(bank.ends_with("7890"), "保留后 4 位：{bank}");

        let text = serde_json::to_string(&detail).expect("序列化");
        assert!(!text.contains("110101199001011234"), "身份证明文泄漏：{text}");
        assert!(!text.contains("6222021234567890"), "银行卡明文泄漏：{text}");
        // 中段数字不许整体出现（`6222 **** 7890` 之外还留着 8 位就是遮得不干净）
        assert!(!text.contains("02123456"), "银行卡中段未遮：{text}");
    }

    // ==================== ③ 规则被删也不退化 ====================

    /// 空规则集合（等价于「客户把两条全局规则删了」）→ 仍然按设计默认参数打码。
    ///
    /// 这条不连库也能跑（`MaskSet::empty()`），放在本文件是为了紧跟上面两条真库证据一起看：
    /// 规则在、规则没了，出参都是打码形态。
    #[test]
    fn empty_rule_set_keeps_masking_shape() {
        let detail =
            dto::EmployeeDetailRes::from_row(&dto::sample_row(), &dto::sample_names(), &registry::MaskSet::empty());
        assert_eq!(
            detail.sensitive_masked.id_card_masked.as_deref(),
            Some("110101****1234")
        );
        assert!(detail.sensitive_masked.bank_account_masked.is_some());
    }
}
