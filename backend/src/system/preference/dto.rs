//! 用户偏好体系的请求 / 响应 DTO。
//!
//! # 两代请求形态（都要支持）
//!
//! | 形态 | 示例 | 谁在用 |
//! |------|------|--------|
//! | 旧（扁平三字段） | `{"palette":"deep-sea","theme_mode":"dark","side_compact":true}` | 第一版前端（`composables/usePalette.ts`） |
//! | 新（KV） | `{"items":{"ui.palette":"deep-sea","ui.page_size":50}}` | 通用偏好体系（本卡起） |
//!
//! 两者可以同时出现：**`items` 里的同键优先**，旧字段只在 `items` 没带该键时补位。
//! 这样前端可以「先按新形态发、旧字段留着兜底」，迁移期不会因为两代字段顺序不同而互相覆盖。
//!
//! # 未知字段（含 `user_id`）一律忽略
//!
//! `UpdatePreferenceReq` 没有 `deny_unknown_fields`，因此请求体里塞 `user_id` / `items` 之外的
//! 任意字段都不会进入写入流程 —— **越权的第一道防线是「body 里的 user_id 根本没有字段可落」**，
//! 第二道是 handler 只从 `AuthClaims.sub` 取用户 ID（见 `handler::update_preference`）。
//!
//! # 响应形状
//!
//! ```json
//! {
//!   "palette": "mist-blue",     // 兼容字段 = items["ui.palette"]
//!   "theme_mode": "light",      // 兼容字段 = items["ui.theme_mode"]
//!   "side_compact": false,      // 兼容字段 = items["ui.side_compact"]
//!   "items": { "ui.palette": "mist-blue", "...": "..." }
//! }
//! ```
//!
//! 兼容字段不是「另一份真相」：它们由 [`crate::system::preference::model::assemble`] 从 `items`
//! 派生，改一处必然同改两处。

use nik::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::shared::error_code;

/// 偏好值：任意 JSON 形态（`string` / `number` / `boolean` / 对象 / 数组）。
pub type PrefValue = serde_json::Value;

/// 更新偏好请求（**同时兼容两代形态**，见模块文档）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdatePreferenceReq {
    /// 旧形态：配色方案 id（等价于 `items["ui.palette"]`）。
    ///
    /// `null` 与「不传」等价（serde 的 `Option` 语义）：偏好不接受 null 值，
    /// 「取消设置」应当走「改回默认值」，而不是写 null。
    pub palette: Option<PrefValue>,
    /// 旧形态：明暗模式（等价于 `items["ui.theme_mode"]`）。
    pub theme_mode: Option<PrefValue>,
    /// 旧形态：侧边栏折叠（等价于 `items["ui.side_compact"]`）。
    pub side_compact: Option<PrefValue>,
    /// 新形态：KV 批量更新，键为 `pref_key`。
    pub items: Option<BTreeMap<String, PrefValue>>,
}

impl UpdatePreferenceReq {
    /// 归一化成 `(pref_key, value)` 列表（只做形态转换，不校验值域）。
    ///
    /// # 返回值
    ///
    /// 按 `pref_key` 升序的键值对（`BTreeMap` 决定顺序，与请求体字段顺序无关，
    /// 便于单测与日志稳定对比）。
    ///
    /// # 异常情况
    ///
    /// 请求体里一个偏好项都没有（`{}` / `{"items":{}}`）→ `400 + 62023`。
    /// 空请求不当作「无操作」成功返回：前端 bug 会因此静默丢设置，报错更容易被发现。
    pub fn into_patch(self) -> std::result::Result<Vec<(String, PrefValue)>, ApiError> {
        let mut patch: BTreeMap<String, PrefValue> = self.items.unwrap_or_default();
        // 旧字段只在 items 没带同键时补位（items 优先，见模块文档）
        if let Some(value) = self.palette {
            patch.entry("ui.palette".to_string()).or_insert(value);
        }
        if let Some(value) = self.theme_mode {
            patch.entry("ui.theme_mode".to_string()).or_insert(value);
        }
        if let Some(value) = self.side_compact {
            patch.entry("ui.side_compact".to_string()).or_insert(value);
        }
        if patch.is_empty() {
            return Err(ApiError::new(
                400,
                error_code::PREFERENCE_PAYLOAD_EMPTY,
                "请求体里没有任何偏好项（需要 palette/theme_mode/side_compact 或 items）",
            ));
        }
        Ok(patch.into_iter().collect())
    }
}

/// 偏好读取 / 保存的统一响应体。
#[derive(Debug, Clone, Serialize)]
pub struct PreferenceRes {
    /// 兼容字段：配色方案 id（= `items["ui.palette"]`）。
    pub palette: String,
    /// 兼容字段：明暗模式（= `items["ui.theme_mode"]`）。
    pub theme_mode: String,
    /// 兼容字段：侧边栏折叠（= `items["ui.side_compact"]`）。
    pub side_compact: bool,
    /// 全量 KV（字典里每个启用键都在，未设置过的取字典默认值）。
    pub items: BTreeMap<String, PrefValue>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> UpdatePreferenceReq {
        serde_json::from_str(body).expect("测试用例的 JSON 必须合法")
    }

    fn keys(patch: &[(String, PrefValue)]) -> Vec<String> {
        patch.iter().map(|(key, _)| key.clone()).collect()
    }

    /// 旧形态：三个扁平字段 → 三个 pref_key。
    #[test]
    fn legacy_shape_maps_to_keys() {
        let patch = parse(r#"{"palette":"deep-sea","theme_mode":"dark","side_compact":true}"#)
            .into_patch()
            .expect("旧形态必须被接受");
        assert_eq!(
            keys(&patch),
            vec!["ui.palette", "ui.side_compact", "ui.theme_mode"]
        );
        assert_eq!(patch[0].1, serde_json::json!("deep-sea"));
        assert_eq!(patch[2].1, serde_json::json!("dark"));
    }

    /// 新形态：items 原样透传（含字典外的键，值域校验交给 model）。
    #[test]
    fn items_shape_passes_through() {
        let patch = parse(r#"{"items":{"ui.page_size":50,"notify.email":true}}"#)
            .into_patch()
            .expect("新形态必须被接受");
        assert_eq!(keys(&patch), vec!["notify.email", "ui.page_size"]);
        assert_eq!(patch[1].1, serde_json::json!(50));
    }

    /// 两代形态同时出现：**items 优先**（旧字段不得把新值覆盖回去）。
    #[test]
    fn items_wins_over_legacy() {
        let patch =
            parse(r#"{"palette":"deep-sea","items":{"ui.palette":"jade","ui.theme_mode":"dark"}}"#)
                .into_patch()
                .expect("混合形态必须被接受");
        assert_eq!(patch.len(), 2, "palette 被 items 吸收，不应产生重复键");
        assert_eq!(patch[0].0, "ui.palette");
        assert_eq!(patch[0].1, serde_json::json!("jade"));
    }

    /// 空请求 → 62023（不静默成功）。
    #[test]
    fn empty_payload_is_rejected() {
        for body in [r#"{}"#, r#"{"items":{}}"#, r#"{"palette":null}"#] {
            let err = parse(body).into_patch().expect_err("空请求必须被拒绝");
            assert_eq!(
                err.code,
                error_code::PREFERENCE_PAYLOAD_EMPTY,
                "body={body}"
            );
            assert_eq!(err.code, 62023);
        }
    }

    /// 越权第一道防线：请求体里的 `user_id` 没有任何字段可落，直接被忽略。
    #[test]
    fn user_id_in_body_is_ignored() {
        let patch = parse(
            r#"{"user_id":"00000000-0000-0000-0000-000000000001",
                "items":{"ui.palette":"jade"},"unknown":"x"}"#,
        )
        .into_patch()
        .expect("多余字段不应导致失败");
        assert_eq!(
            keys(&patch),
            vec!["ui.palette"],
            "body 里的 user_id 不得进入写入集合"
        );
        assert!(!keys(&patch).iter().any(|key| key.contains("user")));
    }
}
