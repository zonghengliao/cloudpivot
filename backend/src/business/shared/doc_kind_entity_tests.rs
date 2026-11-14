//! `state_machine` 实体注册表 ↔ 词表（`DocKind`）的契约（P2-29）。
//!
//! 这两条必须 import **应用代码**（`business::shared::state_machine`），所以它们留在应用 crate，
//! 不进零依赖的 `cloudpivot-doc` crate —— 本模块**不**引用 ERP 业务层，
//! `shared → erp` 的回边不会再长出来（P2-30 起 `shared/` 全目录零业务域路径引用）。
//!
//! | 用例 | 钉住什么 |
//! |---|---|
//! | [`entity_registry_is_covered_by_doc_kind`] | `state_machine::Entity::ALL` 全部落在 `DocKind` 域内 |
//! | [`no_legacy_transfer_entity_type`] | 旧别名 `TRANSFER` 已从代码里消失（只允许 `STOCK_TRANSFER`） |
//!
//! 同批拆出去的另外两处（P2-29）：
//!
//! - 纯词表不变量 → `cloudpivot_doc::vocab_tests`（P2-27 已搬）；
//! - 词表 ↔ 前端镜像 / `demo_seed.sh` / 巡检 SQL 契约 → `cloudpivot_doc` crate 的
//!   `contract_tests.rs`（只读文件、不 import 应用代码）；
//! - 审批类型（`FlowBizType`）契约 → `backend/src/business/erp/flow_biz_type_contract_tests.rs`。

use crate::business::shared::state_machine::{ENTITY_TRANSFER, Entity};
use cloudpivot_doc::DocKind;

/// `state_machine::Entity::ALL` 的每个落库值都必须落在 `DocKind` 域内。
///
/// 反向不要求（`DocKind` 里有 `PAYMENT` / `INVENTORY` / `FLOW_LINKAGE_CFG` 这些
/// 不走状态机的对象），但正向一定会：状态机写进 `biz_status_log.entity_type`
/// 的每个值都在巡检域里，否则巡检会对正常数据报红。
#[test]
fn entity_registry_is_covered_by_doc_kind() {
    for entity in Entity::ALL {
        assert!(
            DocKind::parse(entity.as_str()).is_some(),
            "state_machine 实体 {} 不在 DocKind 域内 —— 巡检会把正常数据判红",
            entity.as_str()
        );
    }
}

/// `TRANSFER` 这个实体别名必须从代码里彻底消失（V0219 已把存量改成 `STOCK_TRANSFER`）。
///
/// 只允许出现在注释里说明历史，所以检查的是「生产代码里的字符串字面量」：
/// `Entity::Transfer.as_str()` 必须是 `STOCK_TRANSFER`。
#[test]
fn no_legacy_transfer_entity_type() {
    assert_eq!(
        ENTITY_TRANSFER, "STOCK_TRANSFER",
        "调拨单的 entity_type 必须与 doc_type / 审批 biz_type 同名"
    );
    assert_eq!(Entity::Transfer.as_str(), "STOCK_TRANSFER");
    assert!(
        DocKind::parse("TRANSFER").is_none(),
        "旧别名 TRANSFER 不能出现在域里（留着它等于兼容层永远在）"
    );
}
