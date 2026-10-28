//! 业务模块容器（二开业务）。
//!
//! `system/` 与 `support/` 属底座，二开业务一律落在本容器下，每个子模块统一遵循
//! `mod.rs + handler.rs + dto.rs + model.rs` 四件套，不设 service 层。
//!
//! | 子模块 | 职责 |
//! |--------|------|
//! | [`monitor`] | 服务器运行状态（CPU / 内存 / 磁盘 / 数据库 / Redis），仅超管可读 |
//! | [`storage`] | 对象存储底座：多后端路由、SigV4 预签名、AK/SK 加密、配置与策略管理 |
//! | [`impexp`] | 导入导出引擎：定义注册、流式解析、逐行校验、模板生成、同步导出 |
//! | [`notice`] | 消息通知底座：四种目标扇出、模板渲染、未读数、`dispatch` 公共 API |
//! | [`permission`] | 数据权限（批次 B · B4）：范围配置、部门树裁剪、自检接口 |
//! | [`shared`] | 数据权限基座（批次 B · B2）：`ScopedPrincipal` / `scope::paginate` / epoch 缓存 |

pub mod impexp;
pub mod monitor;
pub mod notice;
pub mod permission;
pub mod shared;
pub mod storage;

/// 编译期契约示例：**外部模块可以直接调用 `notice::dispatch`**（架构文档 §4.7 接口预留）。
///
/// C 批次（公告发布）与 D 批次（工作流待办 / 抄送 / 超时提醒）只需要构造
/// [`crate::business::notice::NoticeDraft`] 并 `await` 这个函数，不需要了解扇出、
/// 模板渲染、投递记录的任何细节。本函数**不会被调用**，它的唯一用途是让
/// 「公共 API 对其它模块可见」这件事在 `cargo clippy --all-targets` 阶段被编译器验证：
/// 若 `dispatch` / `NoticeDraft` / `NoticeTarget` 被改成私有或签名变更，这里立刻编译失败。
#[allow(dead_code)]
fn _example_notice_dispatch_from_external_module() {
    use crate::business::notice::dto::{ContentFormat, NoticeLevel, NoticeType};
    use crate::business::notice::{NoticeDraft, NoticeTarget, dispatch};
    use alun::uuid::Uuid;

    let instance_id = Uuid::nil();
    let assignee = Uuid::nil();
    let draft = NoticeDraft {
        notice_type: NoticeType::Todo,
        level: NoticeLevel::Info,
        title: "你有新的审批待办".to_string(),
        content: "订单审批待处理".to_string(),
        content_format: ContentFormat::Text,
        link_url: Some(format!("/workflow/task/{instance_id}")),
        biz_type: Some("workflow".to_string()),
        biz_id: Some(instance_id.to_string()),
        template_code: None,
        template_vars: serde_json::json!({}),
        target: NoticeTarget::User(vec![assignee]),
        channel_mask: 1,
        idempotency_key: Some(format!("wf:{instance_id}:todo:{assignee}")),
        expire_at: None,
        // 定时发送（V0051）：外部模块同样可以给未来的计划时间
        scheduled_at: None,
        sender_id: None,
        sender_name: Some("工作流引擎".to_string()),
    };

    // 只构造 Future 不执行：证明签名可用，且不在编译期触发任何 IO
    let _pending = dispatch(draft);
}
