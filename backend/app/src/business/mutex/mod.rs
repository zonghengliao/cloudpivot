//! 角色互斥（SoD, Separation of Duties）—— 批次 B · B7。
//!
//! # 一句话
//!
//! 「角色 A 与角色 B 不可同时授予同一用户」。互斥是**对称关系**（`A×B` 与 `B×A` 是同一条
//! 规则），由 B1 已建的 `uk_sys_role_mutex_pair`（`LEAST/GREATEST` 表达式唯一索引）在
//! 库层保证「同一规则只有一行」；应用层的写入也按 UUID 字节序规范化，读一律双向匹配。
//!
//! # 模块结构
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`dto`] | 请求 / 响应 DTO（ID 一律 `String`，角色编码 / 名称随响应下发） |
//! | [`model`] | `sys_role_mutex` 读写 + 「有效持有」口径的唯一权威定义 |
//! | [`guard`] | 三个校验点共用的守卫（校验 SQL 只有一份，见 [`guard::ensure_no_mutex`]） |
//! | [`checker`] | 存量违规检查（单用户体检 / 单对持有者 / 全量扫描） |
//! | [`handler`] | 6 个 HTTP 接口（规则 CRUD + 用户体检 + 全量扫描） |
//!
//! # 三个校验点（架构 §4.2，缺一不可）
//!
//! | # | 场景 | 落点 | 失败 |
//! |---|------|------|------|
//! | 1 | 授予角色（`PUT /system/user/{id}/roles`、`POST /system/user` 带 `role_ids`） | [`guard::ensure_no_mutex`] | `409 + 61009 ROLE_MUTEX_CONFLICT` |
//! | 2 | 修改角色（`PUT /system/role/{id}`，角色由停用变启用） | [`guard::ensure_role_enable_no_mutex`] | `409 + 61009 ROLE_MUTEX_CONFLICT` |
//! | 3 | 修改互斥规则（本模块 `POST` / `PUT ?enabled=true`） | [`guard::ensure_pair_has_no_holders`] | `409 + 61010 ROLE_MUTEX_HAS_HOLDERS` |
//!
//! ## 校验点 1 的校验集合：`已持有 ∪ 本次授予`（架构 D9）
//!
//! POC-6b/6c 实测证明：只比「本次 vs 已持有」会让「同一次请求里同时授予 A+B」**零命中**
//! （可绕过）。所以校验集合取并集，两两配对。并集的代价：`assign_roles` 是**全量替换**，
//! 于是「一次调用内把 A 换成互斥的 B」也会被拒（请求里同时出现 A、B）。这是**有意保留**
//! 的行为——SoD 要求解除动作显式留痕，管理员分两步（先移除、再添加）即可，第二步不会
//! 再命中。
//!
//! ## 校验点 2 为什么落在「角色启用」而不是「改角色名 / 编码」
//!
//! 互斥规则按**角色 ID** 定义，改 `role_code` / `role_name` 不改变互斥拓扑（库里一行都不动）。
//! 角色侧唯一能让「新的互斥关系成立」的变更是**由停用变启用**：停用角色的绑定不计入有效
//! 持有（见 [`model::HELD_ROLE_SQL`]），一旦启用，原本休眠的绑定立刻生效，可能让用户真正
//! 同时持有两个互斥角色。因此校验点 2 就落在这个状态跃迁上，并对该角色的全部持有者复核。
//!
//! # 与批次 A「权限上限（delegation cap）」的协同
//!
//! 两者正交，都要过，顺序固定：
//!
//! ```text
//! ① 框架 #[nik::permission("system:user:role")]   → 403 无权限码
//! ② guard::ensure_role_grant_allowed(...)         → 403 越权（批次 A）
//! ③ mutex::guard::ensure_no_mutex(...)            → 409 角色互斥（本模块）
//! ④ 事务：replace_user_roles + 令牌版本自增 + 强制下线
//! ```
//!
//! ②在③之前的理由：②拒的是「非超管授予超管角色」这类**更严重**的越权，先报它才能让安全
//! 审计先看到提权意图；反序会让「角色互斥」的提示掩盖真正的越权信号。
//!
//! # 超管**不短路**（与 cap 的刻意差异）
//!
//! 批次 A 的 cap 对超管短路（超管能授任何角色）；互斥**不对超管短路**：互斥是
//! 「数据完整性」约束，不是「操作者资格」约束，超管双持同样让 SoD 失效。所以
//! [`guard::ensure_no_mutex`] 里**没有** `is_super_admin → return Ok(())` 的捷径。
//! 超管的出路是删除互斥规则（`system:role-mutex:delete`）或先解绑，而不是绕过校验。
//!
//! # 权限码（与 `V0065` 的 `sys_menu.auth_code` 逐字一致）
//!
//! `system:role-mutex:{list,create,delete,conflict}` —— 本卡不新增第 13 个码：
//! `PUT`（改理由 / 启停）复用 `:create`（同为写权限），前端 B8 用 `:create` 控制编辑入口。
//!
//! # 存量扫描与「先解绑再配置」
//!
//! 架构 §4.3 的产品决策是**阻止配置**（不是「允许但告警」）：`POST` 一旦发现存量双持用户
//! 就 `409` 且不落库，消息里给出用户数与示例用户名；管理员可先用
//! [`checker::scan`]（`POST /business/mutex/scan`）看全量清单，解绑后重试。`?force=true`
//! 是给「已确认要接受历史违规」的运维留的显式口子（落库 + `WARN` 日志 + 审计）。

pub mod checker;
pub mod dto;
pub mod guard;
pub mod handler;
pub mod model;

pub use guard::{ensure_no_mutex, ensure_pair_has_no_holders, ensure_role_enable_no_mutex};
