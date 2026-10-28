//! 列级脱敏（批次 B · B6）：七种类型 + DTO 出参前脱敏 + 导出同步脱敏。
//!
//! 依据 `docs/批次B-技术架构.md` §3（列级脱敏）+ §7 B6 行 + PRD 脱敏章节。
//!
//! # 一句话说清
//!
//! 「同一行数据，谁看到明文、谁看到 `138****5678`」由 `sys_field_mask` 的规则决定；
//! 规则按 **角色 ∪ 全局** 命中，**范围（数据权限）不参与判定**（§3.5）。
//! 脱敏发生在**出参之前**（DTO 层），不发生在 SQL 层、也不发生在 serde 层（§3.3）。
//!
//! # 模块结构
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`masker`] | 七种类型的**纯函数**实现（永不 panic / 幂等 / 空值原样）+ 单测 |
//! | [`model`] | `sys_field_mask` 读写 + 模板校验 + 缓存清理 |
//! | [`dto`] | 请求 / 响应 DTO |
//! | [`registry`] | [`registry::MaskSet`] + [`registry::Maskable`] + 合并口径 + 已登记 DTO 的实现 |
//! | [`handler`] | 规则 CRUD / 试运行 / 生效规则自检 |
//! | [`user_export`] | `sys_user` 导出定义（导出脱敏的实机验证载体） |
//!
//! # 五个必须记住的结论
//!
//! 1. **D6：出参前脱敏**。落点是 `impl Maskable for XxxRes` + handler 里一次
//!    `item.mask(&set)`；**不用** serde 序列化器（拿不到「字段属于哪张表」），
//!    **不用** SQL 层脱敏（`SELECT *` 无从替换，且过滤/排序会被破坏）。
//! 2. **D7：绝不调用 `alun_utils::Mask::mask_by_type`**。它在 `"张三"` 上 panic
//!    （`byte index 2 is not a char boundary`）。只调具名方法，且对仍按字节切片的
//!    `email` / `bank_card` 加 ASCII 前置判定。
//! 3. **fail-closed**：脱敏失败 → 明确报错（导出 422 / 页面全掩码），
//!    **绝不静默返回明文**。
//! 4. **导出必须脱敏**：`impexp::engine::format_export_rows` 是唯一改点，
//!    敏感列没有生效规则 → `422 + 61008`，并把 `mask_applied` 写进 `sys_export_task`。
//! 5. **豁免要留痕**：`SUPER_ADMIN` 或持 `system:field-mask:plain` 看明文；
//!    后者每次明文访问落一条 `audit_logs`（`FIELD_MASK_PLAIN_READ`）。

pub mod dto;
pub mod handler;
pub mod masker;
pub mod model;
pub mod registry;
pub mod user_export;

pub use masker::{MaskError, MaskType, apply};
pub use registry::{MaskRule, MaskSet, MaskSource, Maskable, resolve};
