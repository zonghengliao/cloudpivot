//! 内置业务定义容器。
//!
//! 本批次至少落地 **1 个真实业务定义**做端到端验证：`system.dict_item`
//! （`sys_dict_item` 字典项的导入 / 导出）。选它的理由：
//!
//! 1. 它同时用到**全部通用能力**——必填、文本长度、整数范围、布尔、枚举（状态）、
//!    外键（字典编码 → `sys_dict_type.dict_code`）、组合唯一（字典编码 + 字典项值）；
//! 2. 目标表是底座表，**只读底座、只写自己的业务表**，不会为了验证去改 `system/`；
//! 3. 导入的是运维真会用的数据（批量维护字典项），不是玩具定义。
//!
//! 新增业务定义只需在本模块加一个文件 + 在 [`register`] 里挂一行。

pub mod dict_item;

use crate::business::impexp::registry::DefinitionRegistry;

/// `sys_dict_item` 的业务编码。
pub const DICT_ITEM_BIZ_TYPE: &str = "system.dict_item";

/// 注册全部内置定义。
///
/// # 参数
///
/// - `registry`：待填充的注册表。
///
/// # 异常情况
///
/// 同一 `biz_type` 重复注册返回错误（编码必须全局唯一）。
pub fn register(registry: &mut DefinitionRegistry) -> std::result::Result<(), String> {
    registry.register_import(std::sync::Arc::new(dict_item::DictItemImport))?;
    registry.register_export(std::sync::Arc::new(dict_item::DictItemExport))?;
    Ok(())
}
