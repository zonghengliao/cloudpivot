//! 公告管理（批次 C · C2 / C3）。
//!
//! 依据 `docs/批次C-技术架构.md` §2（公告）：内容 vs 投递的边界（§2.1）、表设计（§2.3）、
//! 附件复用 `business/storage`（§2.5）、有效期与自动下线（§2.6）、已读未读（§2.7）、
//! API 契约（§2.8）。
//!
//! # 交付拆分
//!
//! | 卡 | 交付物 | 状态 |
//! |----|--------|------|
//! | C2 | `V0080` / `V0081` / `V0087` 迁移 + [`model`]（可见性查询 / 已读 / 计数）+ [`dto`] + [`handler`]（**用户侧 7 个接口**） | 已交付 |
//! | C3 | 管理侧 10 个接口（含 `preview`）+ 发布/下线状态机 + 扇出与补偿 + 附件绑定 | 待办 |
//!
//! # 两条不变量（C2/C3 都必须遵守）
//!
//! 1. **渲染与净化统一走** [`crate::business::shared::content`]（`Recipe::Announcement`），
//!    不允许在这里另写一套 Markdown 渲染 —— 一个渲染器一套 bug；**禁止**用 Markdown
//!    原文拼 HTML（架构 §2.4.2）；
//! 2. 公告的**三张新表有意不登记** `shared/scope.rs` 的 `SCOPED_TABLES`（全局内容 / 审计
//!    完整性，架构 §2.3.3 + S-1），列表接口不要给公告注入数据权限 —— 用户侧的可见性
//!    由 `scope_type` + `sys_announcement_scope` 判定（[`model::VISIBLE_PREDICATE`]）。
//!
//! # 用户侧接口一览（详见 [`handler`] 的模块文档）
//!
//! | 方法 | 路径（宏内，`/api` 由 `[router].prefix` 注入） | 说明 |
//! |------|------|------|
//! | GET | `/business/announcement/visible` | 我能看的公告（分页） |
//! | GET | `/business/announcement/visible/page` | 兼容分页变体（同一实现） |
//! | GET | `/business/announcement/{id}` | 详情（`content_html` 出参前再净化） |
//! | POST | `/business/announcement/{id}/read` | 标已读（幂等） |
//! | GET | `/business/announcement/unread-count` | 未读数 |
//! | PUT | `/business/announcement/read-all` | 全部已读 |
//! | GET | `/business/announcement/{id}/attachment/{att_id}` | 附件代理下载 |
//!
//! 另有架构 §2.8.1 的 `/mine/*` 与 `/attachments/{file_id}/download` **别名**（同一实现）。

pub mod dto;
pub mod handler;
pub mod model;
