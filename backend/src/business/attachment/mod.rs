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
//! | [`model`] | 行结构、单据类型 → 表名映射 / 存在性校验、文件角色词表（[`model::FileRole`]）、绑定 / 列表（两种排序口径）/ 软删 / 重排 / 引用计数 |
//! | [`dto`] | 入参出参契约（上传 / 列表 / 重排 / 删除 / 下载）+ 纯逻辑校验（类型 / 角色 / 排序口径 / id 列表 / 行级删除判据）+ **角色分组**（[`dto::role_catalog`]，ATT-04） |
//! | [`handler`] | 6 个接口：`upload` / `list` / `sort` / `delete` / `download` / `check`（ATT-08） |
//! | [`rule`] | 附件必传规则（**ATT-06**）：`biz_attachment_rule` 读侧 + 闸门 [`rule::assert_attachments`]（单据 submit / confirm / activate 前调用） |
//! | [`rule_admin`] | 附件规则**管理面**（**ATT-09**）：列表 / 下拉 / 新建 / 编辑 / 启停 / 软删，6 个接口，仅超管 |
//!
//! # 文件角色分栏（ATT-04）
//!
//! `list` / `sort` 的出参带 `roles: [{ code, label, count }]`：**固定 6 个角色**（空分组
//! `count = 0` 也在，前端据此画「暂无 XX，点击上传」）+ 数据里真实存在的兼容角色
//! （老值 `SIGN_RECEIPT`）。前端不再维护第二份角色词表，分组标题直接用后端 `label`。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏内写 `/business/attachment/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/attachment/upload` | `business:attachment:upload` + 单据读码 | 上传并绑定（multipart：`doc_type` / `doc_id` / `file_role` / `remark` / `file`） |
//! | GET | `/attachment/list` | `business:attachment:list` + 单据读码 | 按 `doc_type + doc_id` 查（ATT-03：`sort=time` 缺省按上传时间倒序，`sort=manual` 按 `sort_order`） |
//! | PUT | `/attachment/sort` | `business:attachment:upload` + 单据读码 | 手动排序（ATT-03：提交该单据附件的完整 id 顺序） |
//! | DELETE | `/attachment/{id}` | `business:attachment:upload` + 行级 | 软删绑定（上传人本人；删别人的要 `business:attachment:delete` **且**看得见该单据；写接口不能挂读码，见 `authz_tests::o13`） |
//! | GET | `/attachment/download/{id}` | `business:attachment:list` + 单据读码 | 下载（复用 storage 出流 + 落 `operation_logs`） |
//! | GET | `/attachment/check` | `business:attachment:list` + 单据读码 | **ATT-08**：本单据在本动作下缺什么附件（前端禁用按钮 / 红字 / tooltip 的数据源） |
//!
//! 「单据读码」是 ATT-02 补的行级判据（[`model::DocKind::view_permissions`]）：
//! 路由码只回答「能不能用附件功能」，不回答「能不能看这张单」。
//! **ATT-04** 把它补到了写路径（`upload`）与「删别人的附件」分支上 ——
//! 读、写、删三处同一把尺子：看不见的单据，既不能看它的附件，也不能往上挂东西。
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
pub mod rule;
pub mod rule_admin;

#[cfg(test)]
mod tests;

/// 其它模块的真库用例用的小工具（**只编译进测试**）：给单据挂一份 pdf 附件。
///
/// 为什么需要它：ATT-06 给「提交 / 确认 / 生效」加了必传闸门，凡是会走这些动作的
/// 既有 ERP 用例（盘点提交、报销提交、退货提交……）都要先满足闸门才能继续验它自己
/// 的业务。与其在十几个用例里各抄一遍 `sys_file` + 绑定 SQL，不如在这里给一个
/// `attach_pdf(kind, doc_id)`：内容哈希按 `doc_id` 派生，不会撞 `uk_sys_file_hash`。
#[cfg(test)]
pub mod test_support {
    use nik::prelude::*;
    use nik::uuid::Uuid;

    use super::model::{self, DocKind, FileRole};

    /// 给单据挂一份 `pdf` 附件（必传规则里所有单据类型都接受 pdf）。
    ///
    /// # 异常情况
    ///
    /// 写库失败直接 panic（测试夹具失败应该是显式红色，而不是静默跳过）。
    pub async fn attach_pdf(doc_type: DocKind, doc_id: Uuid) {
        let file_id = Uuid::now_v7();
        let key = format!("att06-fixture/{file_id}.pdf");
        db().execute_with_params(
            "INSERT INTO sys_file (id, file_name, original_name, file_key, file_size, \
                     file_hash, mime_type, extension, category, upload_mode, is_shared) \
                 VALUES (?::uuid, ?, '必传规则夹具.pdf', ?, 2048, ?, 'application/pdf', 'pdf', \
                     'attachment', 'SERVER', FALSE) ON CONFLICT DO NOTHING",
            vec![
                Value::from(&file_id),
                Value::Text(key.clone()),
                Value::Text(key),
                Value::Text(format!("att06-hash-{file_id}")),
            ],
        )
        .await
        .unwrap_or_else(|error| panic!("插入附件夹具文件失败：{error}"));
        model::insert(
            doc_type,
            &doc_id,
            &file_id,
            FileRole::Default,
            Some("ATT-06 夹具"),
            None,
        )
        .await
        .unwrap_or_else(|error| panic!("绑定附件夹具失败：{error}"));
    }
}
