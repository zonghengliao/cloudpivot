//! 业务单据附件（ARCH-3 §6 R-P1-3 / 断链 **DC-17**）。
//!
//! 合同要扫描件、收货要签收单、付款要发票影像 —— 这些文件以前没有地方存。
//! 本模块把「单据 ↔ 文件」的绑定关系落成一张**桥接表**（范本：`biz_task_attachment`），
//! 文件本体、上传策略、秒传、下载出流、物理回收**全部复用 [`crate::business::storage`]**：
//!
//! ```text
//!   biz_doc_attachment(doc_type, doc_id, file_id, file_role)
//!                                    └────────────► sys_file（storage 模块的唯一真相）
//! ```
//!
//! | 文件 | 内容 |
//! |------|------|
//! | [`model`] | 行结构、单据类型 → 表名映射 / 存在性校验、绑定 / 列表 / 软删 / 引用计数 |
//! | [`dto`] | 入参出参契约（上传 / 列表 / 删除 / 下载）+ 纯逻辑校验（类型 / 角色 / 行级删除判据） |
//! | [`handler`] | 4 个接口：`upload` / `list` / `delete` / `download` |
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏内写 `/business/attachment/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/attachment/upload` | `business:attachment:upload` | 上传并绑定（multipart：`doc_type` / `doc_id` / `file_role` / `remark` / `file`） |
//! | GET | `/attachment/list` | `business:attachment:list` + 单据读码 | 按 `doc_type + doc_id` 查 |
//! | DELETE | `/attachment/{id}` | `business:attachment:upload` + 行级 `:delete` | 软删绑定（上传人本人 ∪ `business:attachment:delete`；写接口不能挂读码，见 `authz_tests::o13`） |
//! | GET | `/attachment/download/{id}` | `business:attachment:list` + 单据读码 | 下载（复用 storage 出流 + 落 `operation_logs`） |
//!
//! 「单据读码」是 ATT-02 补的行级判据（[`model::DocKind::view_permissions`]）：
//! 路由码只回答「能不能用附件功能」，不回答「能不能看这张单」。
//!
//! # 表结构与权限码的唯一依据
//!
//! `backend/migrations/business/V0212__erp_doc_attachment.up.sql`
//! （卡面写 `V0209`，实际 `V0209–V0211` 已被合同审批 / 业务提醒 / 盘点单占用 → 按「最大号 + 1」用 `V0212`）。
//!
//! # 为什么下载不另设权限码
//!
//! 下载是「读」的一种：能看列表却不能下载是没有业务含义的组合。`list` 码同时管两件事，
//! 与任务模块「`business:task:read` 管列表 + 下载」完全同构。
//!
//! # 为什么 `business:attachment:delete` 是行级码
//!
//! 上传人永远能删自己传错的附件（否则要去找管理员，实际就是把附件变成不可撤销）；
//! 「删**别人**传的」才需要 `business:attachment:delete`。这与
//! `business:task:attachment:delete` 的口径逐字一致，前端 `can_delete` 也是同一条判据。

pub mod dto;
pub mod handler;
pub mod model;

#[cfg(test)]
mod tests;
