//! 附件必传规则管理接口（**ATT-09**）：让管理员在界面上改规则，不写代码、不发版。
//!
//! # 为什么单独一个子模块
//!
//! [`super::rule`] 是**读侧 + 闸门**（`load_rules` / `assert_attachments`），被七类单据的
//! submit / confirm / activate 分支直接调用；写侧（新建 / 编辑 / 软删 / 启停）只服务一个
//! 管理页面。把写侧塞进 `handler.rs` 会让「单据附件接口」与「规则配置接口」共用一份
//! 权限码文档，读的人分不清哪些码属于业务、哪些属于运维。
//!
//! 于是本子模块自带四件套（`mod` / `handler` / `dto` / `model`），路由前缀独立成
//! `/support/attachment-rule`，与前端页面 `/support/attachment-rule`、权限码
//! `support:attachment-rule:*` 逐字对齐。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/support/attachment-rule/list` | `support:attachment-rule:list` | 全部规则（含已禁用，不含已软删） |
//! | GET | `/support/attachment-rule/options` | `support:attachment-rule:list` | 下拉数据源：`doc_type` / `required_at` / `file_role` |
//! | POST | `/support/attachment-rule` | `support:attachment-rule:create` | 新建 |
//! | PUT | `/support/attachment-rule/{id}` | `support:attachment-rule:update` | 全量编辑（含条件片段） |
//! | PUT | `/support/attachment-rule/{id}/enabled` | `support:attachment-rule:update` | 启用 / 禁用（**即时生效**） |
//! | DELETE | `/support/attachment-rule/{id}` | `support:attachment-rule:delete` | 软删 |
//!
//! # 权限：为什么「路由码 + 超管判定」两层都要
//!
//! 卡面要求「只有超管能改」且「非超管账号打开 → 403」。只有路由码不够：权限码在
//! 「角色授权」页可以被任何人勾给任何角色（那是超管自己点的，但一次误点就把「谁能
//! 关掉必传校验」扩散出去了）。规则表决定「哪些单据提交不了」，属于**内控配置**，
//! 与 `flow::linkage` 的联动规则同级别 —— 那里用的就是「路由码 + `ensure_config_admin`」，
//! 本模块照抄同一口径（[`dto::ensure_super_admin`]）。
//!
//! 两层都在时，非超管拿到的最坏结果是 403 + 点名的权限码 / 角色，而不是「页面能开、
//! 保存被拒」的半吊子状态：`list` 也要求超管，页面打不开就不会有人去点保存。
//!
//! # 即时生效是怎么保证的
//!
//! 闸门 [`super::rule::load_rules`] **每次调用都查表**（无缓存、无进程内快照），
//! 本模块的写接口改完提交即对下一次 submit / confirm / activate 生效。
//! 谁想在这里加缓存，先看 `rule.rs` 文件头第 1 条硬约定（卡面验收⑤就是它）。
//!
//! # 与迁移的关系
//!
//! 表结构 `biz_attachment_rule` 由 `V0347` 建（ATT-06），**本卡不改表结构**，
//! 只新增菜单 / 权限点（`V0349`）与代码。已应用的迁移一个字都不能改（全仓铁律）。

pub mod dto;
pub mod handler;
pub mod model;

#[cfg(test)]
mod tests;
