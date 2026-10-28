//! 定义注册表：`biz_type` → 业务定义。
//!
//! 注册表是**进程级单例**（[`global_registry`]），首次访问时装载内置业务定义
//! （见 [`crate::business::impexp::builtin`]）。新增业务只需在自己的模块里实现
//! `ImportDefinition` / `ExportDefinition` 并在 `builtin::register` 里挂一行，
//! 不需要改路由、模板或校验代码。
//!
//! # 为什么不做运行期动态注册
//!
//! 定义里的表名 / 列名 / 外键都写进 SQL（编译期常量），运行期注册等于给「随便传表名」
//! 开口子。注册表是 `OnceLock` + 只读 `HashMap`，没有任何写路径。

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use crate::business::impexp::definition::{ExportDefinition, ImportDefinition, ImportMode};

/// 已注册定义的摘要（前端下拉 / 帮助页用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DefinitionBrief {
    /// 业务编码。
    pub biz_type: &'static str,
    /// 业务名称。
    pub biz_name: &'static str,
    /// 是否支持导入。
    pub supports_import: bool,
    /// 是否支持导出。
    pub supports_export: bool,
    /// 模板版本（仅导入有）。
    pub template_version: u32,
    /// 单次导入行数上限。
    pub max_rows: usize,
    /// 是否允许部分导入（P0 固定关闭，见 PRD D6）。
    pub allow_partial: bool,
}

/// 导入 / 导出定义注册表。
#[derive(Default)]
pub struct DefinitionRegistry {
    imports: BTreeMap<&'static str, Arc<dyn ImportDefinition>>,
    exports: BTreeMap<&'static str, Arc<dyn ExportDefinition>>,
}

impl DefinitionRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册导入定义。
    ///
    /// # 参数
    ///
    /// - `definition`：业务定义。
    ///
    /// # 异常情况
    ///
    /// 同一 `biz_type` 重复注册返回错误（编码必须全局唯一，避免静默覆盖）。
    pub fn register_import(
        &mut self,
        definition: Arc<dyn ImportDefinition>,
    ) -> std::result::Result<(), String> {
        let biz_type = definition.biz_type();
        if self.imports.contains_key(biz_type) {
            return Err(format!("导入定义 {biz_type} 重复注册"));
        }
        self.imports.insert(biz_type, definition);
        Ok(())
    }

    /// 注册导出定义。
    ///
    /// # 参数
    ///
    /// - `definition`：业务定义。
    ///
    /// # 异常情况
    ///
    /// 同一 `biz_type` 重复注册返回错误。
    pub fn register_export(
        &mut self,
        definition: Arc<dyn ExportDefinition>,
    ) -> std::result::Result<(), String> {
        let biz_type = definition.biz_type();
        if self.exports.contains_key(biz_type) {
            return Err(format!("导出定义 {biz_type} 重复注册"));
        }
        self.exports.insert(biz_type, definition);
        Ok(())
    }

    /// 按业务编码取导入定义。
    ///
    /// # 参数
    ///
    /// - `biz_type`：业务编码。
    ///
    /// # 返回值
    ///
    /// 命中返回定义，未注册返回 `None`（调用方转 60011）。
    pub fn import_definition(&self, biz_type: &str) -> Option<Arc<dyn ImportDefinition>> {
        self.imports.get(biz_type).cloned()
    }

    /// 按业务编码取导出定义。
    ///
    /// # 参数
    ///
    /// - `biz_type`：业务编码。
    ///
    /// # 返回值
    ///
    /// 命中返回定义，未注册返回 `None`。
    pub fn export_definition(&self, biz_type: &str) -> Option<Arc<dyn ExportDefinition>> {
        self.exports.get(biz_type).cloned()
    }

    /// 全部定义的摘要（按 `biz_type` 升序）。
    ///
    /// # 返回值
    ///
    /// 导入与导出合并后的列表；同一 `biz_type` 两侧都有时合并成一行。
    pub fn briefs(&self) -> Vec<DefinitionBrief> {
        let mut merged: BTreeMap<&'static str, DefinitionBrief> = BTreeMap::new();
        for (biz_type, definition) in &self.imports {
            merged.insert(
                *biz_type,
                DefinitionBrief {
                    biz_type,
                    biz_name: definition.biz_name(),
                    supports_import: true,
                    supports_export: false,
                    template_version: definition.template_version(),
                    max_rows: definition.max_rows(),
                    allow_partial: definition.allow_partial(),
                },
            );
        }
        for (biz_type, definition) in &self.exports {
            merged
                .entry(*biz_type)
                .and_modify(|brief| {
                    brief.supports_export = true;
                })
                .or_insert(DefinitionBrief {
                    biz_type,
                    biz_name: definition.biz_name(),
                    supports_import: false,
                    supports_export: true,
                    template_version: 0,
                    max_rows: 0,
                    allow_partial: false,
                });
        }
        merged.into_values().collect()
    }
}

/// 进程级注册表。
static REGISTRY: OnceLock<DefinitionRegistry> = OnceLock::new();

/// 取进程级注册表（首次调用时装载内置定义）。
///
/// # 返回值
///
/// 全局注册表。
///
/// # 异常情况
///
/// 内置定义注册冲突时 panic——这是**编译期就能发现**的编码重复，属于编程错误，
/// 且发生在首次请求的懒装载阶段，不会影响进程启动。
pub fn global_registry() -> &'static DefinitionRegistry {
    REGISTRY.get_or_init(|| {
        let mut registry = DefinitionRegistry::new();
        if let Err(error) = crate::business::impexp::builtin::register(&mut registry) {
            panic!("内置导入导出定义注册失败：{error}");
        }
        // 批次 B · B6：`sys_user` 导出定义（三个敏感列）。注册在 `masking/` 而不是
        // `builtin/`，因为它是「导出脱敏」的验证载体，属于 B6 的交付面；
        // 脱敏本身在 `engine::format_export_rows` 里统一处理，与定义放哪无关。
        if let Err(error) = registry.register_export(std::sync::Arc::new(
            crate::business::masking::user_export::UserExport,
        )) {
            panic!("用户导出定义注册失败：{error}");
        }
        registry
    })
}

/// 校验导入模式与定义能力是否匹配。
///
/// # 参数
///
/// - `allow_partial`：定义是否允许部分导入。
/// - `mode`：请求的模式。
///
/// # 返回值
///
/// 允许返回 `Ok(mode)`。
///
/// # 异常情况
///
/// 定义未开「允许部分导入」而请求 `PARTIAL` 时返回 400。
pub fn ensure_mode_allowed(
    allow_partial: bool,
    mode: ImportMode,
) -> std::result::Result<ImportMode, alun::prelude::ApiError> {
    if mode == ImportMode::Partial && !allow_partial {
        return Err(alun::prelude::ApiError::new(
            400,
            crate::shared::error_code::PARAM_INVALID,
            "该导入未开启「允许部分导入」，请使用 STRICT 模式",
        ));
    }
    Ok(mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::impexp::builtin;

    #[test]
    fn builtin_definitions_are_registered() {
        let registry = global_registry();
        let dict_item = registry
            .import_definition(builtin::DICT_ITEM_BIZ_TYPE)
            .expect("sys_dict_item 导入定义应已注册");
        assert_eq!(dict_item.biz_type(), builtin::DICT_ITEM_BIZ_TYPE);
        assert!(!dict_item.columns().is_empty());
        let export = registry
            .export_definition(builtin::DICT_ITEM_BIZ_TYPE)
            .expect("sys_dict_item 导出定义应已注册");
        assert_eq!(export.columns().len(), dict_item.columns().len());
    }

    #[test]
    fn briefs_merge_import_and_export() {
        let briefs = global_registry().briefs();
        let row = briefs
            .iter()
            .find(|brief| brief.biz_type == builtin::DICT_ITEM_BIZ_TYPE)
            .expect("应出现在列表里");
        assert!(row.supports_import && row.supports_export);
        assert_eq!(row.template_version, 1);
        assert!(row.max_rows > 0);
    }

    #[test]
    fn unknown_biz_type_returns_none() {
        assert!(global_registry().import_definition("nope.nope").is_none());
        assert!(global_registry().export_definition("nope.nope").is_none());
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut registry = DefinitionRegistry::new();
        builtin::register(&mut registry).expect("首次注册应成功");
        let error = builtin::register(&mut registry).expect_err("重复注册应失败");
        assert!(error.contains("重复注册"));
    }

    #[test]
    fn partial_mode_requires_definition_switch() {
        assert!(ensure_mode_allowed(false, ImportMode::Strict).is_ok());
        let error = ensure_mode_allowed(false, ImportMode::Partial).expect_err("未开部分导入");
        assert_eq!(error.status, 400);
        assert!(ensure_mode_allowed(true, ImportMode::Partial).is_ok());
    }
}
