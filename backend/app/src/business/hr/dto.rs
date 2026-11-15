//! HR 模块出参契约（骨架 · HR-09）。
//!
//! 骨架阶段只有一个响应体：[`HrHealthRes`]（`GET /business/hr/health` 的模块体检）。
//! 员工 / 合同 / 考勤 / 请假 / 薪资的出参 DTO 随各卡落到
//! `employee_dto.rs` / `attendance_dto.rs` / `leave_dto.rs` / `salary_dto.rs`。
//!
//! # 出参硬约定（设计 §5.1 / `AGENTS.md` 约定 13）
//!
//! - 凡是 `*_id` 必须同时给 `*_name`（批量 join 取，禁止 N+1）；
//! - 审计 / 历史类字段存**姓名快照**（`operator_name` / `uploader_name` …），
//!   业务展示类与主数据关联类一律 **join 实时取名**；
//! - 金额 / 天数 / 小时 `Decimal` → 字符串；日期 `YYYY-MM-DD`；时间 ISO8601 `+08:00`；
//! - 敏感列（`id_card` / `bank_account`）出参只给 `*_masked`，明文走
//!   `GET /business/hr/employee/{id}/sensitive`（要 `business:hr:employee:sensitive` 且写审计）。

use serde::Serialize;

/// 单张 HR 表在本库的存在性。
///
/// `present = false` 只说明「建表迁移没生效」，不是错误 —— 探针的作用就是把这种状态
/// 变成一句能直接照着修的人话，而不是让用户在看到 500 之后去猜。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HrTablePresence {
    /// 表名（`biz_employee` …），顺序与 [`crate::business::hr::model::HR_TABLES`] 一致。
    pub table: String,
    /// 是否已建（`to_regclass('public.<表>') IS NOT NULL`）。
    pub present: bool,
}

/// `GET /business/hr/health` 的响应体：HR 模块骨架的**机器可读自述**。
///
/// 这份响应同时回答三个问题，一次 HTTP 调用全覆盖：
///
/// 1. **模块装没装上** —— `module` / `prefix`：路由前缀拼错了（漏 `/api`、写成
///    `/api/business/hr`）在浏览器里表现为「响应体格式不符合」，这里给出后端认的前缀；
/// 2. **迁移跑没跑** —— `tables` / `ready` / `hint`：10 张表逐张给存在性，
///    缺哪张、该跑什么，`hint` 里说清楚；
/// 3. **接口落地进度** —— `api_planned`：设计 §5.2 定稿的接口条数（46）。
///    骨架阶段**全部未注册**，所以不给「已注册条数」这种会随时间漂移的字段 ——
///    真实进度看 [`crate::business::hr::handler::ROUTE_CONTRACT`] 里各行的 `card` 列。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HrHealthRes {
    /// 模块名，固定 `hr`。
    pub module: String,
    /// 路由前缀（**不含** `/api`，由 `[router] prefix` 注入；前端调用要写全 `/api` + 它）。
    pub prefix: String,
    /// 设计 §5.2 定稿的接口条数（= `ROUTE_CONTRACT` 行数，骨架阶段全部待落地）。
    pub api_planned: usize,
    /// 10 张表逐张的存在性（顺序固定）。
    pub tables: Vec<HrTablePresence>,
    /// 是否就绪 = 10 张表全部存在。
    pub ready: bool,
    /// 一句人话：就绪时说明表数，未就绪时点名缺哪几张 + 怎么修。
    pub hint: String,
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ready: bool) -> HrHealthRes {
        HrHealthRes {
            module: "hr".to_string(),
            prefix: "/business/hr".to_string(),
            api_planned: 46,
            tables: Vec::new(),
            ready,
            hint: if ready {
                "HR 建表迁移已生效（10 / 10 张表在位）".to_string()
            } else {
                "HR 建表迁移未全部生效".to_string()
            },
        }
    }

    /// 字段名是前后端的硬契约：改一个名字，前端所有 `res.ready` 立刻变 `undefined`
    /// （`if (undefined)` 恒假 ⇒ 界面永远显示「未就绪」，还不会报错）。
    #[test]
    fn response_field_names_are_stable() {
        let json = serde_json::to_value(sample(true)).expect("序列化");
        for field in ["module", "prefix", "api_planned", "tables", "ready", "hint"] {
            assert!(json.get(field).is_some(), "响应体缺字段 {field}：{json}");
        }
        assert_eq!(json["module"], serde_json::json!("hr"));
        assert_eq!(json["prefix"], serde_json::json!("/business/hr"));
        assert_eq!(json["api_planned"], serde_json::json!(46));
        // ready 必须是 JSON 布尔（前端 `if (res.ready)` 直接用，不接受 "false" 字符串）
        assert_eq!(json["ready"], serde_json::json!(true));
    }

    /// 单表体检项：两个字段都在，`present` 是布尔。
    #[test]
    fn table_presence_shape_is_stable() {
        let json = serde_json::to_value(HrTablePresence {
            table: "biz_employee".to_string(),
            present: false,
        })
        .expect("序列化");
        assert_eq!(json["table"], serde_json::json!("biz_employee"));
        assert_eq!(json["present"], serde_json::json!(false));
    }
}
