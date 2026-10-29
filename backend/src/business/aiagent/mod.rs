//! AI 用户（云枢 × Hermes · L1）：profile 自动创建 + SOUL.md 编辑 + 事务性回滚。
//!
//! 依据：老大 2026-10-29「建用户的时候可以建 AI 用户，云枢自动建好 AI 用户的 profile，
//! 并且能编辑 AI 用户的职责和 soul.md。**不需要跟 AI 用户对话。**」
//!
//! # 范围（卡面）
//!
//! - ✅ profile 生命周期（建 / 删 / 探活）+ 职责与 SOUL.md 编辑；
//! - ❌ 云枢内与 AI 对话（L2，明确不做）；
//! - ❌ AI 执行任务（T5b，另开卡）。
//!
//! # 文件分工
//!
//! | 文件 | 内容 |
//! |------|------|
//! | [`hermes`] | Hermes CLI 适配层（**唯一**启动 `hermes` 进程 / 读写 profile 目录的代码） |
//! | [`model`] | `sys_ai_agent` 行与 SQL（手写，因为 `model_override` 是 jsonb） |
//! | [`dto`] | 六个接口的请求 / 响应与字段规整 |
//! | [`handler`] | 六个接口 + 创建 saga（含补偿回滚） |
//!
//! # 六个接口（`/api` 前缀由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/system/user/ai` | `system:user:create` | 创建 AI 用户（`sys_user` + profile + SOUL.md + workspace + 凭据） |
//! | PUT | `/system/user/ai/{user_id}` | `system:user:update` | 编辑职责 / SOUL.md / 模型 |
//! | GET | `/system/user/ai/{user_id}` | `system:user:list` | 读配置（含 SOUL.md 全文） |
//! | GET | `/system/user/ai/{user_id}/status` | `system:user:list` | 探活（目录 / SOUL.md / workspace / CLI / gateway / 凭据） |
//! | POST | `/system/user/ai/{user_id}/repair` | `system:user:update` | 修复 profile 级凭据（补 `.env` 缺失的 provider key） |
//! | DELETE | `/system/user/ai/{user_id}` | `system:user:delete` | 删号（备份 SOUL.md → 删 profile → 删 workspace → 软删 user） |
//!
//! # 不变式（laoliu 2026-10-29 追加硬要求）
//!
//! ```text
//! sys_user.user_type = 'ROBOT'  ⇔  sys_ai_agent 存在该 user_id 的（未软删）记录
//! ```
//!
//! 这条是**双向**的，四条落地位置各自负责一侧：
//!
//! | 位置 | 保证 |
//! |------|------|
//! | [`handler::create`] | `INSERT sys_user`（打标 ROBOT）与 `INSERT sys_ai_agent` **同一事务**，任一失败两行一起回滚；外部副作用（profile / workspace / 凭据）失败则补偿删除磁盘资产 + **同事务物理删除**两行（账号从未可见，不留垃圾行） |
//! | [`handler::delete`] | `sys_ai_agent` 与 `sys_user` **同一事务**软删（中间态不可见） |
//! | `system/user/handler.rs` 的 update / delete | 通用用户接口对 `ROBOT` 账号返回 `409 + 64013`：不许绕过本模块改/删 AI 账号 |
//! | [`crate::shared::startup_guard::check_ai_agent_invariant`] | 每次启动各查一次（两条 SQL），有违规 → `ERROR` 日志，**prod 拒启动** |
//!
//! 为什么必须这么严：`user_type` 是「机器人」的唯一标识，`sys_ai_agent` 是它的人格与
//! 磁盘资产（profile / workspace / SOUL.md）的唯一索引。任一侧单独变化都会产生
//! **静默错**——要么有个机器人在系统里存在却没人能编辑它的人格，要么磁盘上躺着一个
//! 永远不会被清理的 profile。
//!
//! # 三条红线（卡面）
//!
//! 1. **profile 名严格正则校验**（[`hermes::PROFILE_NAME_PATTERN`]）：这是命令注入的唯一入口。
//!    实测 `hermes profile create Bad_Name` 会静默小写成 `bad_name` 并建盘，所以「不校验」
//!    并不等于「失败」，而是等于「建了一个入参之外的 profile」；
//! 2. **用户输入不进 shell**：Local 形态走参数数组；Ssh 形态只经 [`hermes::shell_quote`]，
//!    且 SOUL.md **正文永不进参数**（一律 stdin）；
//! 3. **失败必须回滚**：不允许留下半成品 profile 或孤立 `sys_user`。补偿未完成时业务码换成
//!    [`crate::shared::error_code::AI_AGENT_ROLLBACK_FAILED`] 并在消息里列出残留项。
//!
//! # 部署形态（**必须知道**）
//!
//! 云枢后端在 `172.18.10.144`，Hermes 在 `172.18.10.99`（`/root/.hermes/`）。
//! 因此 [`hermes::Transport`] 有两种形态：
//!
//! - `local`：同机部署（云枢与 Hermes 装在一起）——`Command::new(hermes)`；
//! - `ssh`：跨机部署（当前形态）——`ssh root@172.18.10.99 …`，需要 root 免密。
//!
//! 由环境变量选择（见 [`hermes::HermesConfig`] 的表格）。两种形态的**业务语义完全一致**，
//! 所以单测跑 `local`（临时目录 + 假 CLI），E2E 跑 `ssh`（真机 99 上的真 profile）。
//!
//! # 与其它模块的边界
//!
//! - **不改 Hermes 侧 profile 管理逻辑**：只调官方 CLI（`profile create/describe/show/delete`）
//!   与写 `SOUL.md`；不碰 `state.db`、不改 `config.yaml`；
//! - **不动 `sys_user` 的既有流程**：人类用户的建号/编辑/删除一行未改；AI 用户只是在
//!   `sys_user` 上多一行 `user_type='ROBOT'`（列由 `V0102` 引入）；
//! - **不写变更日志**：`sys_ai_agent` 未在 `changelog` 的 `REGISTRY` 登记，写入会报 `62012`；
//!   本卡不改 changelog（批次 C 的模块），需要审计时以 `tracing` 的结构化日志为准。

pub mod dto;
pub mod handler;
pub mod hermes;
pub mod model;
