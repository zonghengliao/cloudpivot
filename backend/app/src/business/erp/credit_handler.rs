//! 客户信用「实时占用 / 可用额度」只读接口（M3-10）· 1 个接口。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/credit/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/credit/{customer_id}` | `business:erp:customer:list` | 下单前的实时信用快照（`amount` 试算本单、`exclude_order_id` 排除本单） |
//!
//! # 为什么不新造权限码
//!
//! 这张卡只做「算」：口径、判定、账都在 [`crate::business::erp::credit`] 里，
//! 本文件只把结果摆成报文。读的是**客户档案里已经对外可见的字段**
//! （`credit_limit` / `credit_used` / `credit_available` 本来就在 `GET /customer/{id}` 的响应里），
//! 所以复用客户的读码 `business:erp:customer:list`：
//!
//! * 造一个新码（`business:erp:credit:read`）就要在 `sys_menu` 建承载行、给 5 个销售角色补授权，
//!   否则**能建单的人点开订单页反而 403**（AGENTS.md 约定 7 / 14）；
//! * 现网 5 个销售角色（SALES_SUPERVISOR / SALES_CLERK / WAREHOUSE_MANAGER /
//!   FINANCE_MANAGER / GENERAL_MANAGER）**全部已持** `customer:list`（`V0267` 矩阵），
//!   等于零授权变更、零新增码 —— 也不给权限树添一颗「点了必然 403」的哑弹。
//!
//! # 两个查询参数（未知参数一律 400）
//!
//! | 参数 | 语义 |
//! |------|------|
//! | `amount` | 本次订单价税合计（试算）。不传 = 只看当前口径（打开表单时先拉一次） |
//! | `exclude_order_id` | 把本单从「在审占用」里排除（编辑已提交单时用；草稿本就不在在审里） |
//!
//! 拼错的 `amout` / `excludeOrderId` 一律 **400**：静默忽略会让人以为「已经按本单金额试算过了」，
//! 而屏幕上其实是「不含本单」的数 —— 这比报错危险得多。
//!
//! # 口径（唯一真相在 `credit` 模块，本文件只搬运）
//!
//! ```text
//! 已占 used    = 有效订单含税 − 已核销应收 − 已过账退货（下限 0）  ← 现算，不读缓存列
//! 在审 pending = SUBMITTED 订单价税合计（不含本单）
//! 占用 occupied = used + pending
//! 合计 total    = occupied + amount（本次订单）
//! 可用 available = 额度 − occupied        （下限 0；额度 0 = 不控额度 → null）
//! 超限 over      = max(0, total − 额度)
//! 未清应收 open_receivable = OPEN/PARTIAL 应收的未结金额（**对照口径，不进占用合计**）
//! ```
//!
//! 判定复用 [`crate::business::erp::credit::decide`]（提交守卫用的同一个函数），
//! 所以「这里显示能过」与「提交真的能过」不会因为两份比较而分叉。
//!
//! ```text
//! SCOPE-EXEMPT: 客户 / 订单 / 应收都是 ERP 主数据与单据表，无 dept_id / 归属人列；
//! 单条入口可见性由 business:erp:customer:list 控制（与客户档案详情同码）。
//! ```

use axum::extract::{Path, Query};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;
use std::collections::HashMap;

use crate::business::erp::credit::{self, CreditSnapshot, CreditVerdict};
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（复用客户读码，理由见文件头） ====================

/// 读客户信用快照（复用 `business:erp:customer:list`）。
pub const PERM_READ: &str = "business:erp:customer:list";

/// 合法查询参数（顺序 = 文档顺序；未知参数 400）。
pub const QUERY_KEYS: [&str; 2] = ["amount", "exclude_order_id"];

/// 客户状态 `FROZEN`：冻结客户**根本不许下单**（比额度更硬的一道闸）。
pub const STATUS_FROZEN: &str = "FROZEN";

/// 快照口径的一行说明（前端 tooltip / 排障时直接展示，不让人去猜）。
pub const BASIS: &str = "已占 = 有效订单价税合计 − 已核销应收 − 已过账退货（下限 0）；\
在审 = 已提交待审批订单价税合计；占用 = 已占 + 在审；合计 = 占用 + 本次订单；\
可用 = 额度 − 占用（下限 0，额度 0 = 不控额度）；未清应收为对照口径，不重复计入";

// ==================== 校验原语 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404`。
fn not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "客户不存在或已删除".to_string())
}

// ==================== 入参 ====================

/// 信用快照的查询条件（解析后的强类型形态）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreditSnapshotQuery {
    /// 本次订单价税合计（`None` = 不试算）。
    pub amount: Option<Decimal>,
    /// 从「在审」里排除的订单（`None` = 不排除）。
    pub exclude_order_id: Option<Uuid>,
}

/// 解析查询串：**未知参数一律 400**（见文件头「两个查询参数」）。
///
/// 空白 / 空串按「没传」处理（`t-input` 清空后前端常发 `amount=`）。
pub fn parse_snapshot_query(params: &HashMap<String, String>) -> std::result::Result<CreditSnapshotQuery, ApiError> {
    if let Some(unknown) = params.keys().find(|key| !QUERY_KEYS.contains(&key.as_str())) {
        return Err(bad_request(format!(
            "未知查询参数 `{unknown}`（只接受 {}）",
            QUERY_KEYS.join(" / ")
        )));
    }
    let mut query = CreditSnapshotQuery::default();
    if let Some(raw) = params
        .get("amount")
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
    {
        let amount = raw
            .parse::<Decimal>()
            .map_err(|_| bad_request(format!("amount 必须是数字（当前 `{raw}`）")))?;
        if amount < Decimal::ZERO {
            return Err(bad_request(format!("amount 不能为负数（当前 `{raw}`）")));
        }
        query.amount = Some(amount.round_dp(2));
    }
    if let Some(raw) = params
        .get("exclude_order_id")
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
    {
        let id =
            Uuid::parse_str(raw).map_err(|_| bad_request(format!("exclude_order_id 不是合法 UUID（当前 `{raw}`）")))?;
        query.exclude_order_id = Some(id);
    }
    Ok(query)
}

// ==================== 出参 ====================

/// 客户信用快照（下单前那张「可用额度」胶囊的全部数据）。
#[derive(Debug, Clone, Serialize)]
pub struct CreditSnapshotRes {
    /// 客户主键。
    pub customer_id: String,
    /// 客户编码。
    pub customer_code: String,
    /// 客户名称。
    pub customer_name: String,
    /// 客户状态（`ACTIVE` / `FROZEN`）。
    pub customer_status: String,
    /// 信用额度（`0` = 不控额度）。
    pub credit_limit: Decimal,
    /// 超限策略（`BLOCK` / `WARN` / `PASS`，库里原值）。
    pub credit_policy: String,
    /// 已占额度（**现算**：有效订单含税 − 已核销应收 − 已过账退货，下限 0）。
    pub used: Decimal,
    /// 缓存列 `biz_customer.credit_used`（对拍用）。
    pub cached_used: Decimal,
    /// 缓存列与现算值是否漂移（`true` = 每日重算该修而没修，是运维信号不是接口错误）。
    pub cache_drift: bool,
    /// 在审占用（`SUBMITTED` 订单价税合计，不含本单）。
    pub pending: Decimal,
    /// 未清应收（`OPEN` / `PARTIAL` 的未结金额，**对照口径，不重复计入占用**）。
    pub open_receivable: Decimal,
    /// 占用 = 已占 + 在审（不含本单）。
    pub occupied: Decimal,
    /// 本次订单价税合计（没传 = `null`）。
    pub order_amount: Option<Decimal>,
    /// 合计 = 占用 + 本次订单。
    pub total: Decimal,
    /// 可用额度 = 额度 − 占用（下限 0；不控额度 → `null`）。
    pub available: Option<Decimal>,
    /// 本单占完之后还剩多少（下限 0；不控额度 → `null`）。
    pub available_after: Option<Decimal>,
    /// 本单导致的超出额（未超 / 不控额度 = 0）。
    pub over: Decimal,
    /// 合计是否已超额度。
    pub is_over: bool,
    /// 判定：`UNCONTROLLED` / `WITHIN` / `WARN` / `PASS` / `BLOCK`。
    pub verdict: &'static str,
    /// 能否提交（`BLOCK` 或客户冻结 → `false`）。
    pub allowed: bool,
    /// 口径说明（一句话）。
    pub basis: String,
}

/// 判定 → 报文枚举名（与 `credit::CreditVerdict` 的变体一一对应，单测钉住）。
pub fn verdict_label(verdict: &CreditVerdict) -> &'static str {
    match verdict {
        CreditVerdict::Uncontrolled => "UNCONTROLLED",
        CreditVerdict::Within => "WITHIN",
        CreditVerdict::Warn(_) => "WARN",
        CreditVerdict::Pass(_) => "PASS",
        CreditVerdict::Block(_) => "BLOCK",
    }
}

impl CreditSnapshotRes {
    /// 由快照构造报文（`customer_id` 由调用方给，快照里没有主键）。
    pub fn of(customer_id: Uuid, snapshot: &CreditSnapshot) -> Self {
        let verdict = snapshot.verdict();
        let frozen = snapshot.status == STATUS_FROZEN;
        Self {
            customer_id: customer_id.to_string(),
            customer_code: snapshot.code.clone(),
            customer_name: snapshot.name.clone(),
            customer_status: snapshot.status.clone(),
            credit_limit: snapshot.limit,
            credit_policy: snapshot.policy.clone(),
            used: snapshot.used,
            cached_used: snapshot.stored_used,
            cache_drift: snapshot.cache_drift(),
            pending: snapshot.pending,
            open_receivable: snapshot.open_receivable,
            occupied: snapshot.occupied(),
            order_amount: if snapshot.amount.is_zero() {
                None
            } else {
                Some(snapshot.amount)
            },
            total: snapshot.total(),
            available: snapshot.available(),
            available_after: snapshot.available_after(),
            over: snapshot.over(),
            is_over: snapshot.over() > Decimal::ZERO,
            verdict: verdict_label(&verdict),
            // 冻结客户的主数据闸比额度闸更硬：提交守卫第一道就拒（`require_active_customer`），
            // 所以这里必须如实说「不能交」，否则前端会显示「额度够，可以提交」。
            allowed: verdict.is_allowed() && !frozen,
            basis: BASIS.to_string(),
        }
    }
}

// ==================== 业务主体 ====================

/// 读客户信用快照（单测直接调本函数，不走认证）。
pub async fn credit_snapshot_of(
    customer_id: &str,
    query: CreditSnapshotQuery,
) -> std::result::Result<CreditSnapshotRes, ApiError> {
    let id = Uuid::parse_str(customer_id.trim())
        .map_err(|_| bad_request(format!("客户 id 不是合法 UUID（当前 `{customer_id}`）")))?;
    let snapshot = credit::snapshot_of(db(), id, query.amount, query.exclude_order_id)
        .await
        .map_err(|error| utils::db_error(error, "读取客户信用快照"))?
        .ok_or_else(not_found)?;
    Ok(CreditSnapshotRes::of(id, &snapshot))
}

// ==================== HTTP 入口 ====================

/// 客户信用实时快照（下单前的可用额度）。
#[nik::permission(
    path = "/business/erp/credit/{customer_id}",
    method = "GET",
    permission = "business:erp:customer:list"
)]
#[nik::get("/business/erp/credit/{customer_id}")]
pub async fn credit_snapshot_api(
    Path(customer_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ResResult<CreditSnapshotRes> {
    let query = parse_snapshot_query(&params)?;
    Ok(Res::ok(credit_snapshot_of(&customer_id, query).await?))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        text.parse::<Decimal>().expect("定点数可解析")
    }

    fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    // ---------- 入参解析（不碰库） ----------

    #[test]
    fn parse_query_accepts_both_params_and_blank_as_absent() {
        let parsed = parse_snapshot_query(&params(&[
            ("amount", " 1130.005 "),
            ("exclude_order_id", "01a10000-0000-4000-8000-000000000001"),
        ]))
        .expect("合法参数");
        // 4 位标度收敛到分（round_dp 默认 midpoint→偶数，1130.005 → 1130.00）
        assert_eq!(parsed.amount, Some(dec("1130.00")));
        assert_eq!(
            parsed.exclude_order_id,
            Some(Uuid::from_str("01a10000-0000-4000-8000-000000000001").unwrap())
        );

        // 空串 / 纯空白 = 没传（前端清空输入框后的常态）
        let blank = parse_snapshot_query(&params(&[("amount", "  "), ("exclude_order_id", "")])).expect("空白不算错");
        assert_eq!(blank, CreditSnapshotQuery::default());

        // 一个参数都不给
        assert_eq!(
            parse_snapshot_query(&HashMap::new()).expect("空查询"),
            CreditSnapshotQuery::default()
        );
    }

    #[test]
    fn parse_query_rejects_unknown_misspelled_and_negative() {
        // 拼错的参数名：静默忽略 = 让人以为按本单金额算过了
        let unknown =
            parse_snapshot_query(&params(&[("amout", "100"), ("amount", "100")])).expect_err("未知参数必须 400");
        assert_eq!(unknown.code, error_code::PARAM_INVALID);
        assert!(unknown.msg.contains("amout"), "报文要点名参数：{}", unknown.msg);

        // 驼峰也不放过（前端 camelCase 混用是常见笔误）
        assert!(parse_snapshot_query(&params(&[("excludeOrderId", "x")])).is_err());

        let bad_amount = parse_snapshot_query(&params(&[("amount", "12a")])).expect_err("非数字 400");
        assert_eq!(bad_amount.code, error_code::PARAM_INVALID);

        let negative = parse_snapshot_query(&params(&[("amount", "-1")])).expect_err("负数 400");
        assert_eq!(negative.code, error_code::PARAM_INVALID);
        assert!(negative.msg.contains("不能为负数"));

        let bad_id = parse_snapshot_query(&params(&[("exclude_order_id", "not-a-uuid")])).expect_err("UUID 400");
        assert_eq!(bad_id.code, error_code::PARAM_INVALID);
    }

    /// 读码复用客户档案的 `PERM_LIST`（两处必须逐字一致，否则 `sys_menu` 只有一个承载行、
    /// 另一个码授不出去 —— 见 `scripts/verify/perm_menu_gate.py`）。
    #[test]
    fn perm_read_matches_customer_list_code() {
        assert_eq!(PERM_READ, crate::business::erp::customer::customer_handler::PERM_LIST);
    }

    /// 报文枚举名与 `CreditVerdict` 变体一一对应（前端按这些字面量分支）。
    #[test]
    fn verdict_labels_match_enum_variants() {
        let over = credit::OverCredit {
            limit: dec("10"),
            used: dec("9"),
            pending: dec("0"),
            amount: dec("2"),
        };
        assert_eq!(verdict_label(&CreditVerdict::Uncontrolled), "UNCONTROLLED");
        assert_eq!(verdict_label(&CreditVerdict::Within), "WITHIN");
        assert_eq!(verdict_label(&CreditVerdict::Warn(over)), "WARN");
        assert_eq!(verdict_label(&CreditVerdict::Pass(over)), "PASS");
        assert_eq!(verdict_label(&CreditVerdict::Block(over)), "BLOCK");
    }

    /// 报文组装：可用额度 / 合计 / 超限直接取快照口径，冻结客户 `allowed = false`。
    #[test]
    fn response_maps_snapshot_fields() {
        let customer = Uuid::from_str("01a10000-0000-4000-8000-00000000c001").unwrap();
        let within = CreditSnapshot {
            code: "CUS-0001".to_string(),
            name: "示例客户".to_string(),
            status: "ACTIVE".to_string(),
            limit: dec("1000"),
            policy: "BLOCK".to_string(),
            used: dec("400"),
            stored_used: dec("400"),
            pending: dec("100"),
            open_receivable: dec("250"),
            amount: dec("500"),
        };
        let res = CreditSnapshotRes::of(customer, &within);
        assert_eq!(res.customer_code, "CUS-0001");
        assert_eq!(res.occupied, dec("500"), "占用 = 已占 + 在审");
        assert_eq!(res.total, dec("1000"), "合计含本单");
        assert_eq!(res.available, Some(dec("500")), "可用 = 额度 − 占用（不含本单）");
        assert_eq!(res.available_after, Some(Decimal::ZERO), "本单占完刚好用光");
        assert_eq!(res.over, Decimal::ZERO);
        assert!(!res.is_over);
        assert_eq!(res.verdict, "WITHIN");
        assert!(res.allowed);
        assert!(!res.cache_drift);
        assert_eq!(res.order_amount, Some(dec("500")));

        // 超一分就变成 BLOCK / 不可提交
        let over = CreditSnapshot {
            amount: dec("500.01"),
            ..within.clone()
        };
        let res = CreditSnapshotRes::of(customer, &over);
        assert_eq!(res.over, dec("0.01"));
        assert!(res.is_over);
        assert_eq!(res.verdict, "BLOCK");
        assert!(!res.allowed);

        // 缓存列漂移：照旧返回真账，但明示 drift
        let drifted = CreditSnapshot {
            stored_used: dec("999"),
            ..within.clone()
        };
        let res = CreditSnapshotRes::of(customer, &drifted);
        assert!(res.cache_drift, "现算 400 vs 缓存 999 必须报漂移");
        assert_eq!(res.used, dec("400"), "返回的仍是现算值");

        // 冻结客户：额度够也不许提交
        let frozen = CreditSnapshot {
            status: "FROZEN".to_string(),
            ..within.clone()
        };
        let res = CreditSnapshotRes::of(customer, &frozen);
        assert_eq!(res.customer_status, "FROZEN");
        assert!(!res.allowed, "冻结客户第一道闸就拒，不能显示成「额度够可以提交」");

        // 不控额度（额度 0）：可用额度无意义 → null，且任何金额都放行
        let free = CreditSnapshot {
            limit: Decimal::ZERO,
            used: dec("99999"),
            stored_used: dec("99999"),
            amount: dec("88888"),
            ..within.clone()
        };
        let res = CreditSnapshotRes::of(customer, &free);
        assert_eq!(res.available, None);
        assert_eq!(res.available_after, None);
        assert_eq!(res.verdict, "UNCONTROLLED");
        assert!(res.allowed);
        assert_eq!(res.over, Decimal::ZERO);
    }

    /// 不传金额时 `order_amount` 是 `null`（而不是 `0`）—— 前端靠它区分「没试算」与「试算 0 元」。
    #[test]
    fn zero_amount_serializes_as_null_order_amount() {
        let customer = Uuid::from_str("01a10000-0000-4000-8000-00000000c001").unwrap();
        let snapshot = CreditSnapshot {
            code: "CUS-0002".to_string(),
            name: "示例客户二".to_string(),
            status: "ACTIVE".to_string(),
            limit: dec("100"),
            policy: "WARN".to_string(),
            used: dec("0"),
            stored_used: dec("0"),
            pending: dec("0"),
            open_receivable: dec("0"),
            amount: Decimal::ZERO,
        };
        let res = CreditSnapshotRes::of(customer, &snapshot);
        assert_eq!(res.order_amount, None);
        assert_eq!(res.total, Decimal::ZERO);
        assert_eq!(res.available, Some(dec("100")));
        assert_eq!(res.available_after, Some(dec("100")));
    }

    // ---------- 真库用例（缺 ERP_DB_URL 时 SKIP，绝不假绿） ----------

    /// 真库用例串行（共用全局 `db()`）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn exec(sql: impl AsRef<str>, params: Vec<Value>) -> DbResult<u64> {
        db().exec_sql(sql.as_ref().to_string(), params).await
    }

    async fn ensure_db() -> bool {
        static ONCE: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
        *ONCE
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(error) => {
                            println!("SKIP: set_db 失败（{error}）");
                            false
                        }
                    },
                    Err(error) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{error}）");
                        false
                    }
                }
            })
            .await
    }

    /// 夹具客户（`code` 前缀 `CR10-` 便于清场）。
    async fn fixture_customer(code: &str, limit: &str, status: &str) -> Uuid {
        let stale = db()
            .query_sql(
                "SELECT id::text AS id FROM biz_customer WHERE code = ?".to_string(),
                vec![Value::from(code)],
            )
            .await
            .unwrap_or_default();
        for row in &stale {
            if let Ok(text) = row.get_typed::<String>("id")
                && let Ok(id) = Uuid::parse_str(&text)
            {
                teardown(id).await;
            }
        }
        let id = Uuid::now_v7();
        exec(
            "INSERT INTO biz_customer (id, code, name, credit_limit, status) \
                 VALUES (?::uuid, ?, ?, ?::text::numeric, ?)",
            vec![
                Value::from(&id),
                Value::from(code),
                Value::from(format!("M310 夹具客户 {code}")),
                Value::Text(limit.to_string()),
                Value::Text(status.to_string()),
            ],
        )
        .await
        .expect("插夹具客户");
        id
    }

    /// 夹具订单（`so_no` 前缀 `SO10-`）。
    async fn fixture_order(customer: Uuid, status: &str, taxed: &str) -> Uuid {
        let id = Uuid::now_v7();
        exec(
            "INSERT INTO biz_sales_order \
                    (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax) \
                 VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, ?, ?::text::numeric, 0, ?::text::numeric)",
            vec![
                Value::from(&id),
                Value::from(format!("SO10-{}", &id.to_string()[24..])),
                Value::from(&customer),
                Value::Text(status.to_string()),
                Value::Text(taxed.to_string()),
                Value::Text(taxed.to_string()),
            ],
        )
        .await
        .expect("插夹具订单");
        id
    }

    /// 夹具应收（`ar_no` 前缀 `AR-SO10-`；`received` = 已核销）。
    async fn fixture_receivable(customer: Uuid, amount: &str, received: &str, status: &str) -> Uuid {
        let id = Uuid::now_v7();
        let unsettled = dec(amount) - dec(received);
        exec(
            "INSERT INTO biz_receivable \
                    (id, ar_no, customer_id, source_type, source_id, source_no, amount, \
                     received_amount, unsettled_amount, credit_days, period_code, due_date, status) \
                 VALUES (?::uuid, ?, ?::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, 30, '2026-11', \
                         CURRENT_DATE + 30, ?)",
            vec![
                Value::from(&id),
                Value::from(format!("AR-SO10-{}", &id.to_string()[24..])),
                Value::from(&customer),
                Value::Text(amount.to_string()),
                Value::Text(received.to_string()),
                Value::Text(unsettled.to_string()),
                Value::Text(status.to_string()),
            ],
        )
        .await
        .expect("插夹具应收");
        id
    }

    /// 清场（按客户 id + 单号前缀；正常 / panic 都清）。
    async fn teardown(customer: Uuid) {
        let _ = exec(
            "DELETE FROM biz_credit_log WHERE customer_id = ?::uuid",
            vec![Value::from(&customer)],
        )
        .await;
        let _ = exec(
            "DELETE FROM biz_sales_order WHERE customer_id = ?::uuid AND so_no LIKE 'SO10-%'",
            vec![Value::from(&customer)],
        )
        .await;
        let _ = exec(
            "DELETE FROM biz_receivable WHERE customer_id = ?::uuid AND ar_no LIKE 'AR-SO10-%'",
            vec![Value::from(&customer)],
        )
        .await;
        let _ = exec(
            "DELETE FROM biz_customer WHERE id = ?::uuid",
            vec![Value::from(&customer)],
        )
        .await;
    }

    /// 场景一：**下单时实时占用** —— 三个分量各自取对，合计含本单，可用额度与超限按口径算。
    #[tokio::test]
    async fn scenario_snapshot_computes_available_credit() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let customer = fixture_customer("CR10-SNAP", "1000", "ACTIVE").await;
        // 已生效订单 600（进「已占」）
        fixture_order(customer, "APPROVED", "600").await;
        // 在审订单 200（进「在审」）
        fixture_order(customer, "SUBMITTED", "200").await;
        // 未清应收 300（其中已核销 100 → 未结 200，只做对照口径）
        fixture_receivable(customer, "300", "100", "PARTIAL").await;
        // 收清的应收不再占额度（SETTLED 不在未清集合里）
        fixture_receivable(customer, "150", "150", "SETTLED").await;

        let snapshot = credit::snapshot_of(db(), customer, None, None)
            .await
            .expect("读快照")
            .expect("客户存在");
        assert_eq!(snapshot.code, "CR10-SNAP");
        assert_eq!(snapshot.limit, dec("1000"));
        assert_eq!(snapshot.policy, "BLOCK");
        assert_eq!(
            snapshot.used,
            dec("350"),
            "已占 = 有效订单含税 600 − 已核销应收 250（两张应收各核销 100 + 150；\
             收清的那张虽然不进「未清应收」，但它的已核销额照样抵减占用）"
        );
        assert_eq!(snapshot.pending, dec("200"), "在审只算 SUBMITTED");
        assert_eq!(
            snapshot.open_receivable,
            dec("200"),
            "未清应收只算 OPEN/PARTIAL 的未结额（不含收清的那张）"
        );
        assert_eq!(snapshot.amount, Decimal::ZERO, "没传本单金额");
        assert_eq!(snapshot.occupied(), dec("550"));
        assert_eq!(snapshot.total(), dec("550"));
        assert_eq!(snapshot.available(), Some(dec("450")));
        assert_eq!(snapshot.over(), Decimal::ZERO);
        assert!(matches!(snapshot.verdict(), CreditVerdict::Within));
        // 缓存列还是 0（没有走 sync），于是漂移要显式报出来 —— 真账优先
        assert_eq!(snapshot.stored_used, Decimal::ZERO);
        assert!(snapshot.cache_drift(), "现算 350 vs 缓存 0 必须报漂移");

        // 本单 500：合计 1050 超 50 → BLOCK（默认策略）
        let with_amount = credit::snapshot_of(db(), customer, Some(dec("500")), None)
            .await
            .expect("读快照")
            .expect("客户存在");
        assert_eq!(with_amount.total(), dec("1050"));
        assert_eq!(with_amount.available_after(), Some(Decimal::ZERO), "可用额度不出负数");
        assert_eq!(with_amount.over(), dec("50"));
        assert!(matches!(with_amount.verdict(), CreditVerdict::Block(_)));

        // 边界：本单 450 → 合计正好 = 额度 → 通过（与守卫的 `after > limit` 一致）
        let edge = credit::snapshot_of(db(), customer, Some(dec("450")), None)
            .await
            .expect("读快照")
            .expect("客户存在");
        assert_eq!(edge.total(), dec("1000"));
        assert_eq!(edge.over(), Decimal::ZERO);
        assert!(matches!(edge.verdict(), CreditVerdict::Within));

        // exclude：把那张在审单排除掉，在审归零
        let submitted = db()
            .query_sql(
                "SELECT id::text AS id FROM biz_sales_order \
                  WHERE customer_id = ?::uuid AND status = 'SUBMITTED'"
                    .to_string(),
                vec![Value::from(&customer)],
            )
            .await
            .expect("查在审单");
        let submitted_id = Uuid::parse_str(&submitted[0].get_typed::<String>("id").expect("读 id")).expect("uuid");
        let excluded = credit::snapshot_of(db(), customer, None, Some(submitted_id))
            .await
            .expect("读快照")
            .expect("客户存在");
        assert_eq!(excluded.pending, Decimal::ZERO, "exclude_order_id 把本单从在审里排除");

        teardown(customer).await;
    }

    /// 场景二：**WARN / PASS 策略在快照里如实反映**（放行但要说清楚超了多少）。
    #[tokio::test]
    async fn scenario_snapshot_follows_policy() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;
        let customer = fixture_customer("CR10-POLICY", "100", "ACTIVE").await;
        fixture_order(customer, "APPROVED", "80").await;

        for (policy, expect_label, expect_allowed) in [
            ("BLOCK", "BLOCK", false),
            ("WARN", "WARN", true),
            ("PASS", "PASS", true),
        ] {
            exec(
                "UPDATE biz_customer SET credit_policy = ? WHERE id = ?::uuid",
                vec![Value::Text(policy.to_string()), Value::from(&customer)],
            )
            .await
            .expect("改策略");
            let res = credit_snapshot_of(
                &customer.to_string(),
                CreditSnapshotQuery {
                    amount: Some(dec("50")),
                    exclude_order_id: None,
                },
            )
            .await
            .expect("读快照");
            assert_eq!(res.used, dec("80"));
            assert_eq!(res.total, dec("130"));
            assert_eq!(res.over, dec("30"));
            assert_eq!(res.verdict, expect_label, "策略 `{policy}` 的判定名");
            assert_eq!(res.allowed, expect_allowed, "只有 BLOCK 不放行");
            assert_eq!(res.credit_policy, policy);
        }

        teardown(customer).await;
    }

    /// 场景三：**边界与错误** —— 不控额度 / 冻结客户 / 客户不存在 / id 不合法。
    #[tokio::test]
    async fn scenario_snapshot_edges_and_errors() {
        if !ensure_db().await {
            return;
        }
        let _guard = DB_LOCK.lock().await;

        // 额度 0 = 不控额度：可用额度 null，任何金额都 allowed
        let free = fixture_customer("CR10-FREE", "0", "ACTIVE").await;
        let res = credit_snapshot_of(
            &free.to_string(),
            CreditSnapshotQuery {
                amount: Some(dec("999999")),
                exclude_order_id: None,
            },
        )
        .await
        .expect("读快照");
        assert_eq!(res.available, None);
        assert_eq!(res.available_after, None);
        assert_eq!(res.verdict, "UNCONTROLLED");
        assert!(res.allowed);
        assert_eq!(res.over, Decimal::ZERO);
        teardown(free).await;

        // 冻结客户：额度够也不能交
        let frozen = fixture_customer("CR10-FROZEN", "100000", "FROZEN").await;
        let res = credit_snapshot_of(&frozen.to_string(), CreditSnapshotQuery::default())
            .await
            .expect("读快照");
        assert_eq!(res.customer_status, "FROZEN");
        assert_eq!(res.verdict, "WITHIN");
        assert!(!res.allowed, "冻结客户不是额度问题，但同样不许提交");
        teardown(frozen).await;

        // 客户不存在 → 404
        let ghost = Uuid::now_v7();
        let missing = credit_snapshot_of(&ghost.to_string(), CreditSnapshotQuery::default())
            .await
            .expect_err("不存在的客户必须 404");
        assert_eq!(missing.code, error_code::RESOURCE_NOT_FOUND);

        // id 不合法 → 400（不是 500、也不是 404）
        let bad = credit_snapshot_of("not-a-uuid", CreditSnapshotQuery::default())
            .await
            .expect_err("非法 id 必须 400");
        assert_eq!(bad.code, error_code::PARAM_INVALID);
    }
}
