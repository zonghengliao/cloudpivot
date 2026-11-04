//! 任务挂业务单据（P2-9 / ARCH-3 §5 DC-19）：白名单、存在性校验、单号解析。
//!
//! # 它解决什么
//!
//! `biz_task` 原本没有任何「这条任务说的是哪张单」的列 ⇒ 采购员想给一张采购单
//! 挂一条「催货」任务，只能把单号抄进标题（靠文字关联：查不到、点不回、
//! 单据侧也列不出自己身上的任务）。本模块给「任务 → 单据」这条边定义校验口径。
//!
//! # 词表从哪来：**不新开一套**
//!
//! 单据类型一律取 [`DocKind`]（`business/shared/doc_kind.rs`，DC-21 收敛后的唯一
//! 真相，17 个业务对象 + 前端 TS 镜像 + 巡检域校验都在那里）。本文件只补两件
//! `DocKind` 回答不了的事：
//!
//! 1. **哪些类型允许被任务挂**（[`DOC_TARGETS`]：卡面点名的 5 种；`PAYMENT` /
//!    `BUDGET` 等暂时不在内 —— 加一种就是这里加一行，不需要迁移）；
//! 2. **该类型取哪个列当「单号」**（`DocKind::table()` 给了表，但没给单号列）。
//!
//! 所以诊断口径是：**类型词表归 DC-21（`DocKind`），挂接白名单归本模块**。
//! 若在这里另写一串 `"PURCHASE_ORDER"` 字面量，就是给 DC-21 又添第 8 套词表。
//!
//! # 为什么 DDL 里没有 doc_type 的值域 CHECK
//!
//! 同上：值域的唯一真相在 `DocKind`（+ 前端镜像 + 巡检），写死 CHECK 等于把词表
//! 抄第二遍，且 DC-21 后续往字典表收敛时还要再改一次。库侧只兜**结构性**约束：
//! `doc_type` 与 `doc_id` 必须成对（`ck_biz_task_doc_pair`）。
//!
//! # 与 `biz_flow_task` 的边界
//!
//! `biz_flow_task` 是**审批待办**（流程引擎产生、批准即消失），本模块挂的是
//! **工作任务**（`biz_task`，有人负责、有截止日、有进度）。两套表各司其职，
//! 本卡**不合并**它们（DC-19 的处置也只要求「加一条边」）。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::shared::doc_kind::DocKind;
use crate::shared::error_code;
use crate::utils;

/// 一种「任务可以挂上去」的单据类型：`DocKind` + 该表里的单号列。
///
/// 字段刻意全为 `&'static`：表名来自 `DocKind::table()`（编译期常量），单号列来自
/// 本文件的常量表 —— **没有任何一处把用户输入拼进 SQL 标识符**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocTarget {
    /// 业务对象类型（词表唯一真相在 `DocKind`）。
    pub kind: DocKind,
    /// 该表里「单号」的列名（`DocKind` 不提供，因为不同表命名各异）。
    pub no_column: &'static str,
}

/// 允许被任务挂的单据类型白名单：**唯一一份**。
///
/// 卡面点名 5 种（采购单 / 收货单 / 合同 / 应付单 / 凭证）。加一种 = 这里加一行
/// （外加前端一个跳转路由），**不需要迁移**（DDL 不设值域 CHECK，见模块文档）。
pub const DOC_TARGETS: &[DocTarget] = &[
    DocTarget {
        kind: DocKind::PurchaseOrder,
        no_column: "order_no",
    },
    DocTarget {
        kind: DocKind::StockIn,
        no_column: "in_no",
    },
    DocTarget {
        kind: DocKind::Contract,
        no_column: "contract_no",
    },
    DocTarget {
        kind: DocKind::Payable,
        no_column: "payable_no",
    },
    DocTarget {
        kind: DocKind::Voucher,
        no_column: "voucher_no",
    },
];

/// 按落库值查白名单（**严格全等**，与 [`DocKind::parse`] 同一口径：不做大小写归一）。
///
/// 为什么不做归一：`doc_kind.rs` 的约定是「落库值一律大写下划线，严格全等匹配」，
/// 归一就是「新旧写法都认」—— 前端已经从 `DOC_KINDS` 常量取值，后端再放宽只会
/// 掩盖调用方的错。错误文案会把允许的取值全列出来，调试成本并不高。
pub fn find(raw: &str) -> Option<&'static DocTarget> {
    let kind = DocKind::parse(raw)?;
    DOC_TARGETS.iter().find(|target| target.kind == kind)
}

/// 校验 `doc_type` / `doc_id` 这一对：合法 → `Some((目标, id))`；都没传 → `None`。
///
/// # 语义
///
/// - 两者都空（或缺席）→ `None`（自由任务，不挂单）；
/// - 只传一个 → `400 + 63004`（`ck_biz_task_doc_pair` 的库侧约束在这里提前拦，
///   文案能说清「补哪个字段」，而不是让 PG 抛约束名）；
/// - `doc_type` 不在白名单（含「是合法业务对象但本版本不许挂」）→ `400 + 63022`；
/// - `doc_id` 不是合法 UUID → `400 + 63004`（与其它 ID 字段同口径）。
///
/// # 异常情况
///
/// 无数据库访问，只做纯校验。
pub fn normalize_pair(
    doc_type: Option<&str>,
    doc_id: Option<&str>,
) -> std::result::Result<Option<(&'static DocTarget, Uuid)>, ApiError> {
    let raw_type = doc_type.map(str::trim).filter(|text| !text.is_empty());
    let raw_id = doc_id.map(str::trim).filter(|text| !text.is_empty());
    match (raw_type, raw_id) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(field_invalid(
            "doc_type 与 doc_id 必须同时给出：只给类型无法定位单据（补 doc_id，或两者都清空）"
                .to_string(),
        )),
        (None, Some(_)) => Err(field_invalid(
            "doc_type 与 doc_id 必须同时给出：只给 id 不知道该查哪张表（补 doc_type，或两者都清空）"
                .to_string(),
        )),
        (Some(raw_type), Some(raw_id)) => {
            let target = find(raw_type).ok_or_else(|| {
                ApiError::new(
                    400,
                    error_code::TASK_DOC_TYPE_INVALID,
                    format!(
                        "doc_type 取值非法：{raw_type}（本版本允许挂单的类型：{}）",
                        DOC_TARGETS
                            .iter()
                            .map(|target| target.kind.as_str())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    ),
                )
            })?;
            let id = utils::parse_uuid(raw_id)
                .ok_or_else(|| field_invalid(format!("doc_id 不是合法 UUID：{raw_id}")))?;
            Ok(Some((target, id)))
        }
    }
}

/// 校验并解析「任务要挂的单据」：白名单 → UUID → **存在且未软删**。
///
/// # 为什么存在性必须在写库前查一次
///
/// `doc_id` 是弱引用（跨表，无外键）。不查的话，一条挂了不存在单据的任务会
/// **永久存在**：详情页显示不出单号（只能显示 uuid 前缀）、单据侧的「关联任务」区
/// 也永远不会列出它 —— 脏数据没有任何自愈路径。
///
/// 软删一并判掉（`deleted_at IS NULL`）：单据软删后还能被新任务挂上去，等于承认
/// 「这张单还有业务含义」，与 ERP 侧所有引用校验口径相反。
///
/// # 异常情况
///
/// - `400 + 63022`：`doc_type` 不在白名单；
/// - `400 + 63023`：单据不存在 / 已软删；
/// - `400 + 63004`：两者不成对 / `doc_id` 非法 UUID；
/// - `500`：数据库异常。
pub async fn resolve(
    doc_type: Option<&str>,
    doc_id: Option<&str>,
) -> std::result::Result<Option<(&'static DocTarget, Uuid)>, ApiError> {
    let Some((target, id)) = normalize_pair(doc_type, doc_id)? else {
        return Ok(None);
    };
    if !exists(target, id).await? {
        return Err(ApiError::new(
            400,
            error_code::TASK_DOC_NOT_FOUND,
            format!(
                "{}不存在或已删除（{id}）：无法把任务挂到这张单上",
                target.kind.label()
            ),
        ));
    }
    Ok(Some((target, id)))
}

/// 单据是否存在且未软删。
///
/// # 异常情况
///
/// 数据库异常 → `500`（按「校验关联单据」的上下文包装）。
pub async fn exists(target: &DocTarget, id: Uuid) -> std::result::Result<bool, ApiError> {
    let Some(table) = target.kind.table() else {
        // 白名单里的 5 种都有实体表；真走到这里说明词表被改坏了 → 显式报错，
        // 不许静默「当成存在」（那会写进一条永远查不到单号的脏关联）。
        return Err(ApiError::new(
            500,
            error_code::TASK_DOC_TYPE_INVALID,
            format!("{} 没有实体表，不能作为可挂单据", target.kind.as_str()),
        ));
    };
    // 表名来自编译期常量（`DocKind::table()`），不含任何请求参数 ⇒ 无注入面。
    let sql =
        format!("SELECT 1 AS ok FROM {table} WHERE id = ?::uuid AND deleted_at IS NULL LIMIT 1");
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&id)])
        .await
        .map_err(|error| utils::db_error(error, "校验关联单据"))?;
    Ok(!rows.is_empty())
}

/// 取单据单号（任务详情「关联单据」区的显示文本）。
///
/// 查不到 / 已软删 → `None`（前端显示「单据已删除」），**不回落 uuid**：
/// 一串 36 位十六进制对用户没有任何信息量（与 P2-8 的 `project_name` 同口径）。
///
/// 错误一律吞成 `None`：单号只是展示字段，查不到不该让整个任务详情 500
/// （与 `purchase_handler::fill_project_names` 同一理由）。
pub async fn doc_no(target: &DocTarget, id: Uuid) -> Option<String> {
    let table = target.kind.table()?;
    let sql = format!(
        "SELECT {} AS doc_no FROM {table} WHERE id = ?::uuid AND deleted_at IS NULL",
        target.no_column
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&id)])
        .await
        .ok()?;
    rows.first()
        .and_then(|row| row.get_typed::<String>("doc_no").ok())
        .filter(|text| !text.trim().is_empty())
}

/// 按落库值 + 主键取单号（详情回填用；词表里没有该类型 → `None`）。
pub async fn doc_no_of(doc_type: Option<&str>, doc_id: Option<&str>) -> Option<String> {
    let raw_type = doc_type?;
    let raw_id = doc_id?;
    let target = find(raw_type)?;
    let id = utils::parse_uuid(raw_id)?;
    doc_no(target, id).await
}

/// 400 + 63004（字段取值非法）。
fn field_invalid(message: String) -> ApiError {
    ApiError::new(400, error_code::TASK_FIELD_INVALID, message)
}

// ==================== 自检 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 白名单就是卡面点名的 5 种，一个不多一个不少（新增类型必须同时改这里）。
    #[test]
    fn whitelist_is_exactly_the_five_doc_types() {
        let codes: Vec<&str> = DOC_TARGETS.iter().map(|t| t.kind.as_str()).collect();
        assert_eq!(
            codes,
            vec![
                "PURCHASE_ORDER",
                "STOCK_IN",
                "CONTRACT",
                "PAYABLE",
                "VOUCHER"
            ],
            "白名单与采购单前端 / 文档口径必须逐字一致"
        );
        // 每种都必须真有实体表 + 单号列名以 _no 结尾（词表抄错行的机械检测）
        let mut tables = std::collections::BTreeSet::<&str>::new();
        for target in DOC_TARGETS {
            let table = target.kind.table().expect("白名单里的类型必须有实体表");
            assert!(
                table.starts_with("biz_"),
                "{} 的表名必须落在 biz_ 前缀（{table} 不在）",
                target.kind.as_str()
            );
            assert!(
                target.no_column.ends_with("_no"),
                "{} 的单号列名看起来不像单号（{}）",
                target.kind.as_str(),
                target.no_column
            );
            assert!(tables.insert(table), "同一张表出现在两条白名单里：{table}");
        }
    }

    /// 类型词表来自 `DocKind`（不新开一套）：白名单里的每个 code 都能被 `DocKind::parse` 回解。
    #[test]
    fn whitelist_codes_come_from_the_single_vocabulary() {
        for target in DOC_TARGETS {
            assert_eq!(
                DocKind::parse(target.kind.as_str()),
                Some(target.kind),
                "{} 必须是 DocKind 的规范拼写（不做第二种写法）",
                target.kind.as_str()
            );
        }
        // 反向：`DocKind` 里合法但**本版本不许挂**的类型（避免白名单被无意放宽）
        for forbidden in [DocKind::Payment, DocKind::StockOut, DocKind::Budget] {
            assert!(
                find(forbidden.as_str()).is_none(),
                "{} 不在本版本的可挂白名单里",
                forbidden.as_str()
            );
        }
    }

    /// 严格全等：大小写 / 空串都不被接受（错误文案会把允许值列出来）。
    #[test]
    fn find_is_strict_and_rejects_unknown() {
        assert_eq!(
            find("PURCHASE_ORDER").map(|t| t.kind.as_str()),
            Some("PURCHASE_ORDER")
        );
        assert!(find("purchase_order").is_none(), "不做大小写归一");
        assert!(find("SALES_ORDER").is_none(), "白名单外的类型必须查不到");
        assert!(find("").is_none());
    }

    /// 两者都不传 = 自由任务；只传一个 = 63004（不是 63022：错在「成对」，不在值域）。
    #[test]
    fn normalize_requires_pair() {
        assert!(normalize_pair(None, None).unwrap().is_none());
        assert!(normalize_pair(Some("  "), Some("")).unwrap().is_none());

        let only_type = normalize_pair(Some("PURCHASE_ORDER"), None).unwrap_err();
        assert_eq!(only_type.status, 400);
        assert_eq!(only_type.code, error_code::TASK_FIELD_INVALID);

        let only_id =
            normalize_pair(None, Some("7a9c0000-0000-4000-8000-000000000001")).unwrap_err();
        assert_eq!(only_id.code, error_code::TASK_FIELD_INVALID);
    }

    /// 白名单外的类型 → 63022；非法 UUID → 63004（两条错误码不能混）。
    #[test]
    fn normalize_rejects_unknown_type_and_bad_uuid() {
        let unknown = normalize_pair(
            Some("SALES_ORDER"),
            Some("7a9c0000-0000-4000-8000-000000000001"),
        )
        .unwrap_err();
        assert_eq!(unknown.status, 400);
        assert_eq!(unknown.code, error_code::TASK_DOC_TYPE_INVALID);
        assert!(
            unknown.msg.contains("PURCHASE_ORDER"),
            "报文必须列出允许值：{}",
            unknown.msg
        );

        let bad_id = normalize_pair(Some("PURCHASE_ORDER"), Some("not-a-uuid")).unwrap_err();
        assert_eq!(bad_id.code, error_code::TASK_FIELD_INVALID);
    }

    /// 合法一对 → 返回归一化后的 (白名单行, uuid)。
    #[test]
    fn normalize_returns_canonical_pair() {
        let (target, id) = normalize_pair(
            Some("STOCK_IN"),
            Some("7a9c0000-0000-4000-8000-0000000000c1"),
        )
        .unwrap()
        .expect("应解析出单据");
        assert_eq!(target.kind, DocKind::StockIn);
        assert_eq!(target.kind.table(), Some("biz_stock_in"));
        assert_eq!(id.to_string(), "7a9c0000-0000-4000-8000-0000000000c1");
    }
}
