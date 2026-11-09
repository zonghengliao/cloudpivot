//! ERP 单据出参三段式口径（E2E01-FIX-4）：`data.header` / `data.items` / `data.meta`。
//!
//! # 为什么要统一
//!
//! 同一 ERP 域里，单据类接口的出参形状一度各写各的（演练实测）：
//!
//! | 接口 | 单头字段名 | 明细字段名 |
//! |------|------------|------------|
//! | `POST /purchase-requisition/{id}/to-po` | `purchase_order` | `lines` |
//! | `POST /purchase-order/{id}/submit-approval` | `order` | `items` |
//! | `POST /price-inquiry` | `head` | `items` |
//! | `POST /stock-in` | `header` | `items` |
//!
//! 于是前端每接一个接口都要单独写一遍解析，通用表格组件也没法复用。
//!
//! # 口径（本模块是**唯一**定义处）
//!
//! ```text
//! data.header  单据主表（单头）—— 一个对象
//! data.items   单据明细 —— 一个数组（没明细的单据给空数组，不给 null）
//! data.meta    其他附属信息（可选）—— 审批实例号 / 流状态 / 统计等
//! ```
//!
//! # 兼容策略：只做加法
//!
//! 老字段（`purchase_order` / `lines` / `order` / `head`）**一律保留**，
//! 与新字段在同一份响应里并列下发；两者由**同一个构造器**从同一份数据 clone 出来
//! （见各 `XxxRes::new`），所以不存在「两套查询逻辑跑出两个值」的可能，
//! `#[cfg(test)]` 里也有断言把这条钉住（`header == 旧字段`、`items == 旧字段`）。
//!
//! 删除旧字段属于破坏性变更，要单独规划 + 前端全量验证，不在本模块的范围。
//! 待迁移清单见 `docs/编码规范.md` §1.12。

use std::collections::BTreeMap;

use serde::Serialize;

/// 出参的附属信息段（`data.meta`）。
///
/// 三个固定键是「几乎每张单据都能用上」的：审批实例号与流状态；
/// 其余按接口自取（合并张数 / 比价统计 / 是否可转单 …）走 [`DocMeta::with_extra`]，
/// 序列化时**平铺**进 `meta`，不再多套一层。
///
/// 空 `meta`（所有键都没有）序列化为 `{}`——这是刻意的：前端可以无条件读 `data.meta`，
/// 不用判 `undefined`（`meta` 本身永远存在）。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DocMeta {
    /// 审批实例 id（单据进过审批才有；`None` 时不下发该键）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    /// 审批实例单号（`FLW-…`，与 `instance_id` 同源；前端直接展示，不用再查一次）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_no: Option<String>,
    /// 审批实例状态（`PENDING` / `APPROVED` / `REJECTED` / `CANCELLED` / `WITHDRAWN`）。
    ///
    /// 与 [`Self::instance_id`] 的区别：实例号是「单据挂着哪个实例」，这里是「那个实例现在到哪了」。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_status: Option<String>,
    /// 接口自定义的附属信息（统计 / 合并张数 / 比价摘要 …），序列化时平铺进 `meta`。
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl DocMeta {
    /// 空的 meta（`{}`）。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 挂上审批实例三元组。
    ///
    /// 三个参数都允许 `None`（`skip_serializing_if` 会把缺的键去掉），
    /// 这样「有实例号但查不到实例状态」时不会下发出一个假的 `null`。
    #[must_use]
    pub fn with_instance(
        mut self,
        instance_id: Option<String>,
        instance_no: Option<String>,
        flow_status: Option<String>,
    ) -> Self {
        self.instance_id = instance_id;
        self.instance_no = instance_no;
        self.flow_status = flow_status;
        self
    }

    /// 追加一条自定义附属信息（同名键覆盖）。
    #[must_use]
    pub fn with_extra(mut self, key: &str, value: serde_json::Value) -> Self {
        self.extra.insert(key.to_string(), value);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_meta_serializes_to_empty_object() {
        // 前端可以无条件读 data.meta，不用判 undefined
        assert_eq!(serde_json::to_value(DocMeta::empty()).unwrap(), json!({}));
    }

    #[test]
    fn instance_triple_uses_erp_flow_status_vocabulary() {
        let meta = DocMeta::empty().with_instance(
            Some("11111111-1111-1111-1111-111111111111".to_string()),
            Some("FLW-20261109-000001".to_string()),
            Some("PENDING".to_string()),
        );
        let value = serde_json::to_value(&meta).unwrap();
        assert_eq!(value["instance_id"], json!("11111111-1111-1111-1111-111111111111"));
        assert_eq!(value["instance_no"], json!("FLW-20261109-000001"));
        assert_eq!(value["flow_status"], json!("PENDING"));
        // 没给的自定义键不会凭空冒出来
        assert_eq!(value.as_object().unwrap().len(), 3);
    }

    #[test]
    fn missing_instance_fields_are_omitted_not_null() {
        let meta = DocMeta::empty().with_instance(None, None, None);
        assert_eq!(serde_json::to_value(&meta).unwrap(), json!({}));
    }

    #[test]
    fn extra_keys_flatten_into_meta() {
        let meta = DocMeta::empty()
            .with_extra("merged_count", json!(3))
            .with_extra("requisition_nos", json!(["PRQ-1", "PRQ-2", "PRQ-3"]));
        let value = serde_json::to_value(&meta).unwrap();
        assert_eq!(value["merged_count"], json!(3));
        assert_eq!(value["requisition_nos"].as_array().unwrap().len(), 3);
        // 没有多余嵌套：extra 的内容平铺在 meta 顶层
        assert!(value.get("extra").is_none());
    }

    #[test]
    fn extra_overrides_same_key() {
        let meta = DocMeta::empty()
            .with_extra("quote_count", json!(1))
            .with_extra("quote_count", json!(7));
        assert_eq!(serde_json::to_value(&meta).unwrap()["quote_count"], json!(7));
    }
}
