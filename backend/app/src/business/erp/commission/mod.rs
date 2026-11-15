//! 佣金域（SO-36 / SO-36b）：佣金结算单 + 佣金规则。
//!
//! 拆目录见 P2-17：原 `erp/commission_*.rs` 六个文件整体搬进本目录，模块路径由
//! `erp::commission_*` 变为 `erp::commission::commission_*`，文件内容与逻辑一字未改
//! （只改 `use` 路径）。
//!
//! * [`commission_dto`] —— 佣金结算契约：计提入参 / 结算单行 / 明细行 / 状态迁移
//!   （`DRAFT → APPROVED → PAID`）；
//! * [`commission_model`] —— 佣金数据层（V0309/V0311）：候选应收 → 已认领基数 /
//!   规则快照 / 结算单取号（`CM-yyyyMM`）；
//! * [`commission_handler`] —— 佣金 4 个接口：计提（`settle`）/ 分页 / 详情 /
//!   审批发放 + 纯函数 `pick_rule` / `commission_of`；
//! * [`commission_rule_dto`] —— 佣金规则契约（V0309 §②）：范围 / 计算方式 /
//!   比率金额校验 + 部分更新三态合并；
//! * [`commission_rule_model`] —— 佣金规则数据层（`biz_commission_rule` 17 列）：
//!   分页（含 `scope_name` 派生列）/ 建改软删 / 引用计数；
//! * [`commission_rule_handler`] —— 佣金规则 4 个接口：分页 / 新建 / 编辑（停用）/
//!   软删。

pub mod commission_dto;
pub mod commission_handler;
pub mod commission_model;
pub mod commission_rule_dto;
pub mod commission_rule_handler;
pub mod commission_rule_model;
