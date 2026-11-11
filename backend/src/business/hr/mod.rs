//! 人力资源模块（ARCH-9-2 · 批次 2~5 逐卡交付）。
//!
//! 设计定稿：`docs/architecture/ARCH-9-人力资源模块设计.md`（**不要重新设计**）。
//! 本卡（**HR-09**）只落**骨架**：模块挂进 `business/` 容器、10 张表的名单与建表体检、
//! 46 条接口的**契约表**，以及一条真正能工作的探针接口。业务路由从 HR-10 起逐条落地。
//!
//! # 交付批次（设计 §8.1 的 50 张卡）
//!
//! | 批 | 范围 | 卡 |
//! |:--:|---|---|
//! | 1 | 底座：10 张表 / 9 科目 + 2 规则 / 4 号段 + 4 字典 / 36 菜单 + 授权 / 脱敏配置 / 词表 / 前端镜像 / 状态机 | HR-01~HR-08 |
//! | **2** | **员工档案**：模块骨架 / 员工 CRUD / 敏感字段 / 取号 / 合同 / 薪资标准 / seed-from-users / 前端页 | **HR-09**~HR-18 |
//! | 3 | 考勤 + 请假：期间 / 台账 / impexp / 锁定 / 请假 CRUD + 流程 | HR-19~HR-30 |
//! | 4 | 薪资：薪资项 / 公式解析器 / 计算引擎 / 薪资单 / 过账 / 红冲 / 导出 / 自助 | HR-31~HR-46 |
//! | 5 | 收口：权限矩阵回归 / 全量回归 / 上线演练 / 文档回写 | HR-47~HR-50 |
//!
//! 批 1 已由 HR-01~HR-08 落地（迁移见 `backend/migrations/business/V0383~V0389`）：
//! 10 张表、9 个损益/负债科目、2 条工资凭证规则、4 条号段（`EMP/LV/ATT/SAL`）、4 个字典、
//! 36 行菜单 + 35 个权限码、2 条脱敏规则（`id_card` / `bank_account`）、
//! `doc_kind.rs` 的 5 `DocKind` / 4 `FlowBizType` / 2 `VoucherSourceType`。
//!
//! # 本卡做了什么 / 不做什么
//!
//! **做**：
//!
//! - `business/mod.rs` 挂上 `pub mod hr;`（模块进编译单元、进 `App::scan()` 的扫描范围）；
//! - [`model`]：10 张表的名单 [`model::HR_TABLES`] + 建表体检 [`model::table_presence`]
//!   + 组织哨兵 [`model::SENTINEL_ORG_ID`]；
//! - [`handler::ROUTE_CONTRACT`]：设计 §5.2 的 46 条接口（方法 / 路径 / 权限码 / 落地卡）；
//! - [`handler::health`]：`GET /business/hr/health`，已注册到主 router 并真连库体检。
//!
//! **不做**（留给对应的卡，本卡只登记不实现）：
//!
//! - 不建迁移（批 1 已建，`V0383~V0389` 全部已应用；已应用迁移**不可改**，要补列只能新开迁移）；
//! - 不注册任何「空壳业务路由」：路由 = 已实现，未实现的接口应当 404 而不是 500
//!   （理由见 [`handler`] 模块文档）；
//! - 不动 `erp` / `shared` / `system` / `support` 的既有代码（发现的问题写在卡里上报）。
//!
//! # 接口（46 条，契约见 [`handler::ROUTE_CONTRACT`]）
//!
//! | 组 | 条数 | 路径前缀 |
//! |---|---:|---|
//! | 员工档案 | 12 | `/business/hr/employee/...` |
//! | 劳动合同 | 3 | `/business/hr/employee/{id}/contract`、`/business/hr/contract/...` |
//! | 员工薪资标准 | 2 | `/business/hr/employee/{id}/salary` |
//! | 考勤 | 8 | `/business/hr/attendance/...` |
//! | 请假 | 6 | `/business/hr/leave/...` |
//! | 薪资项 | 4 | `/business/hr/salary-item/...` |
//! | 薪资单 | 11 | `/business/hr/salary/...` |
//!
//! 路由宏路径**不含 `/api`**（由 `[router] prefix` 注入），前端 api 层必须写全 `/api/business/hr/...`。
//!
//! # 权限码（35 个 = 5 页面 + 30 按钮，与 `V0387__hr_menu_perms` 一一对应）
//!
//! `business:hr:{employee|contract|attendance|leave|salary|salary-item}:{动作}`。
//! 页面码 5 个（`*:list` 或 `salary-item:list`）+ 按钮码 30 个；
//! `SUPER_ADMIN` 短路全部校验，`HR_MANAGER` 角色（`data_scope=3`）由 HR-04 种下。
//! 新增权限码必须**同时**加 `sys_menu` 行（新迁移）与 [`handler::ROUTE_CONTRACT`]，
//! `contract_permissions_are_seeded_by_v0387` 单测会把两边对拍。
//!
//! # 四条不变式（后续每张卡都必须守）
//!
//! 1. **组织维度 `org_id`**：10 张表都是 `org_id UUID NOT NULL` 且**无 DEFAULT**，
//!    每条 `INSERT` 必须显式带 [`model::SENTINEL_ORG_ID`]（多组织落地后换成令牌里的组织 id）。
//!    唯一键都是 `(org_id, …)` 形态，漏带 org_id 会让「工号重复检测」这类约束失效。
//! 2. **姓名：快照 vs join 按语义分**（`AGENTS.md` 约定 13）：审计 / 历史类（审批人、
//!    上传人、操作人）存姓名快照；业务展示类与主数据关联类（创建人、部门、岗位）**join 实时取名**，
//!    批量 join，不许 N+1；DTO 里凡是 `*_id` 必须同时给 `*_name`。
//! 3. **单据 vs 主数据**：`biz_leave_request` / `biz_salary_sheet` 是**单据**
//!    （`DocKind::LeaveRequest` / `DocKind::SalarySheet`，`is_document = true`），
//!    状态流转走 `shared::state_machine` 的边表；`biz_employee` / `biz_employee_contract`
//!    是**主数据**，`biz_attendance_period` 是**锁定型作业批次**。
//! 4. **提交后失败必须补偿**（`AGENTS.md` 约定 11）：过账（HR-39）与红冲（HR-40）在
//!    `tx.commit()` 之后的动作（生成凭证、调外部服务、发通知）失败时，必须把前置状态显式
//!    打回（草稿 / 失败标记），用户能从界面继续操作；免审直过分支的每个 `Err` 都要打回。
//!
//! # 与其它模块的边界
//!
//! - **词表**：`DocKind` / `FlowBizType` / `VoucherSourceType` 已在 HR-06 扩好，
//!   本模块**只引用不新增**；要加新值走 `business/shared/doc_kind.rs`（那是全仓唯一的词表）。
//! - **状态机**：`business/shared/state_machine.rs`（HR-08 补 5 个 `Entity` 与边表）。
//! - **审批流**：4 套流程 `hr_leave` / `hr_salary` / `hr_salary_adjust` / `hr_employee_resign`
//!   由 HR-26 / HR-38 的迁移种进 `biz_flow_def`；回调按 `flow::linkage` 的桥接写法
//!   （参考 `erp/contract_bridge.rs`：通过 → 推进状态 / 驳回 → 回草稿 / 反向兜底孤儿实例）。
//! - **凭证**：工资计提 / 发放两张凭证走 `erp::voucher::post_explicit_lines`（HR-39），
//!   科目由 HR-02 的 9 个科目 + 2 条 `biz_voucher_rule` 提供，本模块**不写死科目号**。
//! - **通知**：`business::notice::dispatch`；通知失败不回滚业务（同 `task` 模块口径）。
//! - **附件**：`business::attachment`（`ATTACHMENT_DOC_KINDS` 已含员工 / 合同 / 请假单 / 薪资单）。
//! - **导入导出**：`business::impexp`（`hr.attendance` 定义随 HR-21 落地）。
//! - **脱敏**：`business::masking`（`sys_field_mask` 已种 `biz_employee.id_card` / `bank_account`，
//!   见 V0388）；明文接口要 `business:hr:employee:sensitive` 且写 `audit_logs`（HR-13）。
//!
//! # 错误出口（只有三个名字）
//!
//! handler `ResResult<T>` / model `DbResult<T>` / 启动与任务 `nik::Result<T>`；
//! handler 内 `DbError` / `io::Error` 直接 `?`，不写多余的 `.map_err(ApiError::internal(..))`。
//!
//! # 已知待办（本卡只登记，不修）
//!
//! - 设计 §5.2 的「员工导入 / 导出」（#11 / #12）在 §8.1 的批 2 里**没有对应卡**
//!   （`ROUTE_CONTRACT` 里 `card = "TBD"`）；
//! - 设计 §5.2 表头写「42 条」、正文编号到 46（以 46 为准）；
//!   脚注写「4 处 `/my`」而表里只有 2 处（`/leave/my` 随 HR-41 落）。

pub mod dto;
/// 员工档案出参契约（`biz_employee`）：`*_id`/`*_name` 成对 + 敏感字段只出 `*_masked`（HR-11）。
pub mod employee_dto;
/// HR-11 的真库用例（`HR_DB_URL` / `ERP_DB_URL` 开关）：`V0388` 的两条脱敏规则 = DTO 兜底常量，
/// 以及「真库规则 → DTO 打码值」整条链。只在 `cargo test` 时编译。
#[cfg(test)]
mod employee_dto_db_tests;
/// 员工档案接口层（`biz_employee`）：设计 §5.2 #1~#6 + #8 绑账号（HR-12）。
pub mod employee_handler;
/// HR-12 的真库用例（`HR_DB_URL` / `ERP_DB_URL` 开关）：6 个接口的完整链路 +
/// **部门 / 本人两种数据范围**的可见性边界。只在 `cargo test` 时编译。
#[cfg(test)]
mod employee_handler_db_tests;
/// 员工主档数据层（`biz_employee`）：CRUD + 三个唯一键的冲突翻译（HR-10）。
pub mod employee_model;
/// HR-10 的真库用例（`HR_DB_URL` / `ERP_DB_URL` 开关）：CRUD 全通 + 三条冲突的真实报文。只在 `cargo test` 时编译。
#[cfg(test)]
mod employee_model_db_tests;
pub mod handler;
pub mod model;
