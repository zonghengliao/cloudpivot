//! 批量操作通用件（D7）：勾选 → **逐条独立事务** → 逐条结果汇总。
//!
//! # 为什么逐条事务，而不是一个大事务
//!
//! ```text
//! 10 条里第 5 条失败：
//!   大事务       → 前 4 条一起回滚，用户看到「一条都没过」
//!   逐条事务     → 前 4 条已提交，第 5 条失败点名原因，用户知道下一步做什么
//! ```
//!
//! 所以批量接口**不自己开事务**：它逐条调用已有的「单条业务函数」，那些函数各自
//! `begin()` / `commit()` / `rollback()`（收货过账、采购审批、凭证红冲都是这样写的）。
//! 批量层只做三件事：解析 `ids`、查「主键 → 单号」用于点名、把每条结果收进
//! [`BatchResult`]。
//!
//! # 三类用法
//!
//! 1. [`parse_ids`]：请求体 `ids` 去空白 / 去重 / 上限 [`MAX_BATCH_IDS`]，空数组直接 400；
//! 2. [`labels_of`]：**一次** `id = ANY(?::text::uuid[])` 查回「主键 → 单号」，
//!    这样失败行也能说「IN-20261101-000001：状态已是已过账」；
//! 3. [`run`]：逐条 `await` 单条函数，按结果记 [`BatchResult::ok`] / [`BatchResult::fail`]。
//!
//! # 幂等（卡面 L9）
//!
//! 批量层不吞重复：已过账的单据再过账，单条函数返回 409（带单号与过账时间），
//! 该条进 `failed` 且 `error` 原文可读 —— **重复点批量按钮不会把库存加两次**，
//! 第二次只是「0 成功 / N 失败」而不是「N 成功」。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 本模块只按主键批量点名（biz_* 单据表，无 dept_id / 归属人列），
//! 可见性由各业务接口自己的权限码控制（`#[nik::permission]` 在批量入口上）。
//! ```

use std::collections::{HashMap, HashSet};
use std::future::Future;

use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

/// 单次批量最多处理多少条（超过请分批：一次 200 条已经够点一次干半小时的活）。
pub const MAX_BATCH_IDS: usize = 200;

/// 请求体的公共部分：`{ "ids": ["..."], "reason": null }`。
///
/// 具体接口在自己的 DTO 里再追加 `status` / `comment` 等字段；
/// 想省事就直接用本结构（仅 `ids` / `reason`）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct BatchIdsReq {
    /// 勾选的主键（UUID 字符串）。
    pub ids: Vec<String>,
    /// 原因（高危操作必填，其余可选）。
    pub reason: Option<String>,
}

impl BatchIdsReq {
    /// 去空白后的原因（空串 → `None`）。
    pub fn reason_text(&self) -> Option<&str> {
        self.reason.as_deref().map(str::trim).filter(|text| !text.is_empty())
    }
}

/// 校验权限码（批量入口的**第二道闸**）。
///
/// 路由宏 `#[nik::permission]` 已经在 HTTP 层拦过一道；批量业务函数还会被
/// 内部调用（别的卡片、脚本、未来的任务编排），所以再判一次：
/// **没有码就 403，报文点名缺哪个码**，而不是「默默 0 条」。
///
/// # 异常情况
///
/// - `403 + 30001`：缺少权限码（`SUPER_ADMIN` 的 `*` 通配会自动放行）。
pub fn ensure_permission(claims: &TokenClaims, code: &str) -> std::result::Result<(), ApiError> {
    if claims.has_permission(code) {
        return Ok(());
    }
    Err(ApiError::new(
        403,
        crate::shared::error_code::PERMISSION_DENIED,
        format!("缺少权限：{code}（批量操作比单条更危险，权限码与单条接口一致）"),
    ))
}

/// 解析并校验 `ids`：去空白、去重（保持首次出现的顺序）、上限 [`MAX_BATCH_IDS`]。
///
/// # 异常情况
///
/// - `400`：空数组 / 超过上限 / 空串 / 不是合法 UUID（**报出第几个**，方便前端定位）。
pub fn parse_ids(raw: &[String]) -> std::result::Result<Vec<Uuid>, ApiError> {
    if raw.is_empty() {
        return Err(ApiError::bad_request(
            "ids 不能为空：批量操作至少要勾选一条（前端「全选」按当前页勾选）",
        ));
    }
    if raw.len() > MAX_BATCH_IDS {
        return Err(ApiError::bad_request(format!(
            "单次批量最多 {MAX_BATCH_IDS} 条（本次 {} 条）：请分批执行，避免一次请求跑太久",
            raw.len()
        )));
    }
    let mut seen: HashSet<Uuid> = HashSet::with_capacity(raw.len());
    let mut out: Vec<Uuid> = Vec::with_capacity(raw.len());
    for (index, text) in raw.iter().enumerate() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(ApiError::bad_request(format!("ids[{index}] 是空串")));
        }
        let id = Uuid::parse_str(trimmed)
            .map_err(|_| ApiError::bad_request(format!("ids[{index}] 不是合法 UUID：{trimmed}")))?;
        // 重复勾选（例如「本页全选」后又点了一条）按一条算：重复执行会把状态机踩成 409
        if seen.insert(id) {
            out.push(id);
        }
    }
    if out.is_empty() {
        return Err(ApiError::bad_request("ids 不能为空：批量操作至少要勾选一条"));
    }
    Ok(out)
}

/// 成功的一条。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatchOkItem {
    /// 主键。
    pub id: String,
    /// 单号（查不到时 `None`，例如「删了才被勾上」）。
    pub no: Option<String>,
}

/// 失败的一条（**必须**带原因：卡面 O5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatchFailItem {
    /// 主键。
    pub id: String,
    /// 单号（查不到时 `None`）。
    pub no: Option<String>,
    /// 业务码（与单条接口同一个码：409 冲突 / 404 不存在 / 400 参数）。
    pub code: i32,
    /// 原因原文（单条接口的 `msg`，不加工，用户能照着改）。
    pub error: String,
}

/// 批量结果（卡面 §一 ② 的响应体）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct BatchResult {
    /// 成功的条目（按入参顺序）。
    pub succeeded: Vec<BatchOkItem>,
    /// 失败的条目（按入参顺序，每条带原因）。
    pub failed: Vec<BatchFailItem>,
    /// 入参条数（去重后）。
    pub total: usize,
    /// 成功条数。
    pub success_count: usize,
    /// 失败条数。
    pub fail_count: usize,
}

impl BatchResult {
    /// 空结果；`total` = 去重后的入参条数。
    pub fn new(total: usize) -> Self {
        Self {
            total,
            ..Default::default()
        }
    }

    /// 记一条成功。
    pub fn ok(&mut self, id: Uuid, no: Option<String>) {
        self.succeeded.push(BatchOkItem { id: id.to_string(), no });
        self.success_count = self.succeeded.len();
    }

    /// 记一条失败（原因取单条接口的 `ApiError`）。
    pub fn fail(&mut self, id: Uuid, no: Option<String>, error: &ApiError) {
        self.failed.push(BatchFailItem {
            id: id.to_string(),
            no,
            code: error.code,
            error: error.msg.clone(),
        });
        self.fail_count = self.failed.len();
    }

    /// 记一条失败（自带码与文案；用于「主键不存在」这类批量层自己发现的情况）。
    pub fn fail_with(&mut self, id: Uuid, no: Option<String>, code: i32, message: impl Into<String>) {
        self.failed.push(BatchFailItem {
            id: id.to_string(),
            no,
            code,
            error: message.into(),
        });
        self.fail_count = self.failed.len();
    }

    /// 是否全部成功。
    pub fn all_ok(&self) -> bool {
        self.failed.is_empty()
    }

    /// 一句话汇总：`共 3 条：成功 2 条，失败 1 条`。
    pub fn summary(&self) -> String {
        format!(
            "共 {} 条：成功 {} 条，失败 {} 条",
            self.total, self.success_count, self.fail_count
        )
    }
}

/// 逐条执行 `action`，每条一个独立事务（事务在 `action` 内部的单条函数里）。
///
/// # 参数
///
/// - `ids`：已去重的主键（[`parse_ids`] 的返回值）；
/// - `labels`：主键 → 单号（[`labels_of`] 的返回值，查不到的键就是 `None`）；
/// - `action`：单条业务函数；`Ok(())` = 该条已提交，`Err` = 该条已回滚。
///
/// # 返回值
///
/// 汇总结果。**不会**因为某条失败提前退出：一条坏数据不该挡住后面 199 条。
pub async fn run<F, Fut>(ids: &[Uuid], labels: &HashMap<Uuid, String>, mut action: F) -> BatchResult
where
    F: FnMut(Uuid) -> Fut,
    Fut: Future<Output = std::result::Result<(), ApiError>>,
{
    let mut out = BatchResult::new(ids.len());
    for id in ids {
        let no = labels.get(id).cloned();
        match action(*id).await {
            Ok(()) => out.ok(*id, no),
            Err(error) => out.fail(*id, no, &error),
        }
    }
    out
}

/// 批量点名：一次查询把「主键 → 单号」查回来。
///
/// # 参数
///
/// - `table`：`biz_*` 表名（**只传模块内的 `const`**，不要来自请求）；
/// - `column`：单号列名（同上）；
/// - `ids`：主键。
///
/// # 返回值
///
/// 主键 → 单号；查不到的键不在 map 里（调用方按「无单号」处理）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn labels_of(table: &str, column: &str, ids: &[Uuid]) -> std::result::Result<HashMap<Uuid, String>, DbError> {
    let sql = format!(
        "SELECT id::text AS id, {column}::text AS no FROM {table} \
         WHERE id = ANY(?::text::uuid[])"
    );
    labels_from(&sql, ids).await
}

/// 同 [`labels_of`]，但 SQL 自己拼（联表取可读标题时用）。
///
/// 要求：选出 `id`（主键，`text`）与 `no`（展示用单号 / 标题，`text`）两列，
/// 且 SQL 里留着 `?::text::uuid[]` 这个唯一参数位。
pub async fn labels_from(sql: &str, ids: &[Uuid]) -> std::result::Result<HashMap<Uuid, String>, DbError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db().find_with_params(sql, vec![array_literal(ids)]).await?;
    let mut out = HashMap::with_capacity(rows.len());
    for row in rows {
        let raw: String = row.get_typed("id")?;
        let id = Uuid::parse_str(&raw)
            .map_err(|error| DbError::InvalidValue(format!("id 不是合法 UUID（{raw}）：{error}")))?;
        let no: Option<String> = row.get_typed("no").ok();
        if let Some(no) = no.filter(|text| !text.trim().is_empty()) {
            out.insert(id, no);
        }
    }
    Ok(out)
}

/// UUID 数组参数的文本字面量：`{a,b,c}`，配 SQL 里的 `?::text::uuid[]`。
///
/// 直接绑 `Vec<Uuid>` 驱动编码不上（见 `references/pre-submit-traps.md`）。
fn array_literal(ids: &[Uuid]) -> Value {
    let mut literal = String::with_capacity(ids.len() * 37 + 2);
    literal.push('{');
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            literal.push(',');
        }
        literal.push_str(&id.to_string());
    }
    literal.push('}');
    Value::Text(literal)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "018f4b2c-1111-7222-8333-444455550001";
    const B: &str = "018f4b2c-1111-7222-8333-444455550002";

    #[test]
    fn empty_ids_is_bad_request() {
        let error = parse_ids(&[]).expect_err("空 ids 必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("勾选一条"), "报文要能照做：{}", error.msg);
    }

    #[test]
    fn parse_ids_dedupes_and_keeps_order() {
        let ids = parse_ids(&[
            A.to_string(),
            format!("  {B}  "),
            A.to_string(), // 重复勾选只算一条
        ])
        .expect("应合法");
        assert_eq!(ids.len(), 2, "重复的 id 必须去重");
        assert_eq!(ids[0].to_string(), A);
        assert_eq!(ids[1].to_string(), B);
    }

    #[test]
    fn parse_ids_rejects_bad_uuid_with_index() {
        let error = parse_ids(&[A.to_string(), "not-a-uuid".to_string()]).expect_err("必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("ids[1]"), "点名第几条：{}", error.msg);
    }

    #[test]
    fn parse_ids_caps_batch_size() {
        let raw: Vec<String> = (0..=MAX_BATCH_IDS).map(|_| A.to_string()).collect();
        let error = parse_ids(&raw).expect_err("超过上限必须 400");
        assert!(error.msg.contains(&MAX_BATCH_IDS.to_string()));
    }

    #[test]
    fn batch_result_keeps_counts_and_summary() {
        let mut out = BatchResult::new(3);
        out.ok(Uuid::parse_str(A).expect("A"), Some("IN-1".to_string()));
        out.ok(Uuid::parse_str(B).expect("B"), None);
        out.fail_with(
            Uuid::parse_str(A).expect("A"),
            Some("IN-2".to_string()),
            409,
            "状态已是已过账",
        );
        assert_eq!(out.total, 3);
        assert_eq!(out.success_count, 2);
        assert_eq!(out.fail_count, 1);
        assert!(!out.all_ok());
        assert_eq!(out.summary(), "共 3 条：成功 2 条，失败 1 条");
        assert_eq!(out.failed[0].code, 409);
        assert_eq!(out.failed[0].no.as_deref(), Some("IN-2"));
    }

    #[test]
    fn array_literal_is_pg_text_form() {
        let ids = vec![Uuid::parse_str(A).expect("A"), Uuid::parse_str(B).expect("B")];
        match array_literal(&ids) {
            Value::Text(text) => assert_eq!(text, format!("{{{A},{B}}}")),
            other => panic!("必须是文本字面量：{other:?}"),
        }
    }
}
