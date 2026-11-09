//! 结账检查清单的**单据明细**（PERIOD-CLOSE · 把「还有 3 张草稿凭证」变成「这 3 张是谁的、该谁处理、点了去哪」）。
//!
//! # 为什么单独一个模块
//!
//! `period.rs` 的四项前置校验原来只数张数（`COUNT(*)` + 最多 3 个单号），
//! 老板的原话是「不光要显示出来有什么问题，还要把要处理的单据列出来，
//! 方便跳转处理或者发消息给相关人员，列出的列表还要写清楚下一步需要谁处理」。
//! 于是本模块补三样东西：
//!
//! ```text
//! ① 明细：每张挡路单据一行（单号 / 类型 / 金额 / 制单人 / 状态）
//! ② 归属：下一步该谁处理（owner_role 角色码 + owner_name 具体人）
//! ③ 直达：action_label（去过账 / 去审批 / 去收货）+ route（带 id 的前端路由）
//! ```
//!
//! 判定口径（哪张单挡路）**仍然只有 `period.rs` 的四处 SQL** —— 本模块是它们的
//! 「同一条件、多取几列」的版本：条数、样例单号、明细行三者必须同源，否则会出现
//! 「文案说 3 张、表格列了 5 行」这种自相矛盾的弹窗。
//!
//! # owner_role 怎么定（设计重点，不是猜）
//!
//! 规则只有一条：**看这张单的「下一个动作」需要什么权限码，再反查库里哪个启用角色
//! 持有该权限**（`sys_role ⇄ sys_role_menu ⇄ sys_menu.auth_code`）。对照表：
//!
//! | 单据 | 当前状态 | 下一个动作 | 需要的权限码 |
//! |------|----------|-----------|--------------|
//! | 凭证 `VOUCHER` | `DRAFT` | 去过账 | `business:erp:voucher:create`（过账走凭证录入口） |
//! | 采购单 `PURCHASE_ORDER` | `DRAFT` | 去提交 | `business:erp:purchase-order:submit-approval` |
//! | 采购单 | `SUBMITTED` | 去审批 | `business:erp:purchase-order:status` |
//! | 采购单 | `APPROVED` / `PARTIAL_RECEIVED` | 去收货 | `business:erp:stock-in:create` |
//! | 收货单 `STOCK_IN` | `DRAFT` / `PENDING_POST` | 去过账 | `business:erp:stock-in:status` |
//! | 收货单 | `PENDING_CANCEL` | 去处理（作废审批） | `business:erp:stock-in:status` |
//! | 调拨单 `STOCK_TRANSFER` | `DRAFT` | 去提交 | `business:erp:transfer:status` |
//! | 调拨单 | `PENDING` | 去审批 | `business:erp:transfer:status` |
//! | 调拨单 | `APPROVED` | 去发运 | `business:erp:transfer:status` |
//! | 调拨单 | `IN_TRANSIT` | 去收货 | `business:erp:transfer:status` |
//! | 不变量 `INVARIANT` | 分录不平 / 头表不一致 | 去看凭证 | ——（不猜，见下） |
//!
//! **定位不到就留 `null`**（前端显示「待分配」）：
//! * 没有任何启用角色持有该权限码 → `owner_role = None`；
//! * 不变量类单据是「账被改坏了」，怎么改要财务主管判断，系统不指定处理人 → `None`。
//!
//! **制单人只作参考，不作 owner**：造单的人不一定有权过账（本仓的
//! `business:erp:stock-in:create` 与 `business:erp:stock-in:status` 就是两个码）。
//! 所以 `created_by_name` 只是展示信息，owner 一律从**权限**反查。
//!
//! **owner_name 只在「角色下恰好一个启用用户」时填**：一个角色挂 5 个人时
//! 「具体处理人」是 5 选 1，猜谁都是错的；这时只给角色，前端显示角色名，
//! 催办发给该角色全体。这条规则写进单测（`owner_name_only_when_role_has_single_user`）。
//!
//! 反查的**实现**在 [`crate::business::erp::period_owner`]（本模块 `pub use` 重导出，
//! 调用方路径不变）：将来接流程引擎的动态审批人只改那边，明细取数一行不用动。
//!
//! # 条数上限
//!
//! 每类最多返回 [`ITEM_LIMIT`]（50）条明细：一次返回 5000 行会把弹窗撑爆。
//! 超过时限 50 条，并把 `message` 改成「共 N 张（仅显示前 50 张）」，
//! 前端据此提示「请先去列表页批量处理」。
//!
//! # 年结为什么直接复用本模块（YEAR-END）
//!
//! 年结的前置校验与结账**是同一批单据、同一套判据**，只是范围从「一个月」变成「一整年」。
//! 于是本模块给了两个「日期区间」入口，SQL 一行没复制：
//!
//! | 需要 | 期间结账用 | 年结用 |
//! |------|-----------|--------|
//! | 未完成单据 | [`unfinished_doc_items`]（期间起止日） | [`unfinished_doc_items_between`]（1/1 ~ 12/31） |
//! | 不变量 A1/A2 | [`invariant_details`]（`v.period = ?`） | [`invariant_details_between`]（`voucher_date` 区间） |
//!
//! 两条纪律跟着一起复用：
//! * **明细合并后统一截断到 50 条**（年结不是「12 个月 × 50 = 600 行」）；
//! * **处理人一律从权限码反查**（不变量类留空，系统不指定人）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_voucher / biz_purchase_order / biz_stock_in / biz_stock_transfer /
//! biz_inventory 都是按「会计期间」取的全局台账（无 dept_id 与归属人列），
//! 可见性由 business:erp:period:{close,remind} 控制；这里不注入数据范围。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;
use std::collections::HashMap;
use std::result::Result;
use std::str::FromStr;

use crate::business::erp::model::PeriodRow;
use crate::utils;

// ==================== 口径常量 ====================

/// 每类 blocker 最多返回的明细条数（防弹窗被 5000 行撑爆）。
pub const ITEM_LIMIT: i64 = 50;

/// 单据类型：记账凭证。
pub const DOC_VOUCHER: &str = "VOUCHER";
/// 单据类型：收货入库单。
pub const DOC_STOCK_IN: &str = "STOCK_IN";
/// 单据类型：采购订单。
pub const DOC_PURCHASE_ORDER: &str = "PURCHASE_ORDER";
/// 单据类型：调拨单。
pub const DOC_STOCK_TRANSFER: &str = "STOCK_TRANSFER";
/// 单据类型：库存台账行（只出现在「全库巡检提示」里）。
pub const DOC_INVENTORY: &str = "INVENTORY";

/// 「下一个动作」的权限码与处理人反查 → 见 [`crate::business::erp::period_owner`]
/// （反查是「该谁处理」的唯一实现，本模块只消费它）。
pub use crate::business::erp::period_owner::{
    Owner, OwnerResolver, PERM_PO_STATUS, PERM_PO_SUBMIT, PERM_STOCK_IN_CREATE, PERM_STOCK_IN_STATUS,
    PERM_TRANSFER_STATUS, PERM_VOUCHER_POST,
};

/// 前端路由：凭证列表（`?voucher_no=` 深链会预填筛选）。
pub const ROUTE_VOUCHER: &str = "/business/erp/voucher";
/// 前端路由：采购订单列表（`?po_id=` 深链直接开详情抽屉）。
pub const ROUTE_PURCHASE_ORDER: &str = "/business/erp/purchase-order";
/// 前端路由：收货入库列表（`?in_id=` 深链直接开详情抽屉）。
pub const ROUTE_STOCK_IN: &str = "/business/erp/stock-in";
/// 前端路由：调拨单列表。
pub const ROUTE_STOCK_TRANSFER: &str = "/business/erp/transfer";
/// 前端路由：库存巡检。
pub const ROUTE_INVENTORY: &str = "/business/erp/inventory";

// ==================== 出参结构 ====================

/// `Uuid` 序列化为字符串（与其它 ERP DTO 的主键口径一致）。
fn uuid_text<S: serde::Serializer>(id: &Uuid, serializer: S) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&id.to_string())
}

/// 一条挡路单据的明细（给用户看的，不是给机器看的）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BlockingItem {
    /// 单据类型：[`DOC_VOUCHER`] / [`DOC_STOCK_IN`] / [`DOC_PURCHASE_ORDER`] /
    /// [`DOC_STOCK_TRANSFER`] / [`DOC_INVENTORY`]。
    pub doc_type: String,
    /// 单据主键（凭证 / 采购单 / 收货单 / 调拨单的 `id`；**序列化为字符串**：
    /// 本仓的 `uuid` crate 没开 `serde` feature，API 契约里主键一律是字符串）。
    #[serde(serialize_with = "uuid_text")]
    pub doc_id: Uuid,
    /// 单据号（凭证号 `PZ-…` / 采购单号 `PO-…` / 收货单号 `IN-…` / 调拨单号 `TR-…`）。
    pub doc_no: String,
    /// 单据当前状态（**中文标签**，前端直接渲染）。
    pub status_label: String,
    /// 金额（可空：不是所有单据都有；`Decimal` 序列化为字符串，避免浮点丢精度）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<Decimal>,
    /// 制单人（谁造成的这张单；**只作参考，不作 owner**）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by_name: Option<String>,
    /// 下一步该谁处理（**角色码**，前端翻译成中文；`None` = 待分配）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_role: Option<String>,
    /// 具体处理人名字（仅在「该角色恰好一个启用用户」时有值）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_name: Option<String>,
    /// 前端动作文案：「去过账」/「去审批」/「去收货」…
    pub action_label: String,
    /// 前端路由（带 id 直达）。
    pub route: String,
    /// 这张单卡了多久（创建时间；用于催办紧迫度与「已等 N 天」文案）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_since: Option<DateTime<Utc>>,
    /// 处理人角色的主键（**不下发前端**，催办时按角色扇出用）。
    #[serde(skip)]
    pub owner_role_id: Option<Uuid>,
    /// 具体处理人的主键（**不下发前端**，催办时定点发送用）。
    #[serde(skip)]
    pub owner_user_id: Option<Uuid>,
}

impl BlockingItem {
    /// 已等了多少天（不足 1 天算 0，负数按 0）。
    pub fn waiting_days(&self, now: DateTime<Utc>) -> i64 {
        self.waiting_since
            .map(|since| (now - since).num_days().max(0))
            .unwrap_or(0)
    }

    /// 有没有可催办的对象（只有角色没有具体人也算有）。
    pub fn has_owner(&self) -> bool {
        self.owner_user_id.is_some() || self.owner_role_id.is_some()
    }
}

// ==================== 下一步动作口径 ====================

/// 一张单据的「下一个动作」：状态中文 / 动作文案 / 需要的权限码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NextAction {
    /// 状态中文标签（如「草稿待过账」）。
    pub status_label: &'static str,
    /// 前端动作文案（如「去过账」）。
    pub action_label: &'static str,
    /// 该动作需要的权限码（空串 = 系统不指定处理人）。
    pub permission: &'static str,
}

/// 凭证的下一个动作（本仓没有「过账」独立权限码，过账即走凭证录入口）。
pub fn voucher_next(status: &str) -> NextAction {
    match status {
        "DRAFT" => NextAction {
            status_label: "草稿（未过账）",
            action_label: "去过账",
            permission: PERM_VOUCHER_POST,
        },
        "AUDITED" => NextAction {
            status_label: "已审核",
            action_label: "去看凭证",
            permission: "",
        },
        "POSTED" => NextAction {
            status_label: "已过账",
            action_label: "去看凭证",
            permission: "",
        },
        _ => NextAction {
            status_label: "已作废",
            action_label: "去看凭证",
            permission: "",
        },
    }
}

/// 采购单的下一个动作（状态机见 `ck_biz_po_status`）。
pub fn purchase_order_next(status: &str) -> NextAction {
    match status {
        "DRAFT" => NextAction {
            status_label: "草稿（未提交）",
            action_label: "去提交",
            permission: PERM_PO_SUBMIT,
        },
        "SUBMITTED" => NextAction {
            status_label: "已提交待审批",
            action_label: "去审批",
            permission: PERM_PO_STATUS,
        },
        "APPROVED" => NextAction {
            status_label: "已批准待收货",
            action_label: "去收货",
            permission: PERM_STOCK_IN_CREATE,
        },
        "PARTIAL_RECEIVED" => NextAction {
            status_label: "部分收货",
            action_label: "去收货",
            permission: PERM_STOCK_IN_CREATE,
        },
        _ => NextAction {
            status_label: "状态未知",
            action_label: "去处理",
            permission: "",
        },
    }
}

/// 收货单的下一个动作（`DRAFT / PENDING_POST / PENDING_CANCEL` 都会挡结账）。
pub fn stock_in_next(status: &str) -> NextAction {
    match status {
        "DRAFT" => NextAction {
            status_label: "草稿（未过账）",
            action_label: "去过账",
            permission: PERM_STOCK_IN_STATUS,
        },
        "PENDING_POST" => NextAction {
            status_label: "待过账审批",
            action_label: "去过账",
            permission: PERM_STOCK_IN_STATUS,
        },
        "PENDING_CANCEL" => NextAction {
            status_label: "待作废审批",
            action_label: "去处理",
            permission: PERM_STOCK_IN_STATUS,
        },
        _ => NextAction {
            status_label: "状态未知",
            action_label: "去处理",
            permission: "",
        },
    }
}

/// 调拨单的下一个动作（6 态状态机）。
pub fn stock_transfer_next(status: &str) -> NextAction {
    match status {
        "DRAFT" => NextAction {
            status_label: "草稿（未提交）",
            action_label: "去提交",
            permission: PERM_TRANSFER_STATUS,
        },
        "PENDING" => NextAction {
            status_label: "待审批",
            action_label: "去审批",
            permission: PERM_TRANSFER_STATUS,
        },
        "APPROVED" => NextAction {
            status_label: "已批准待发运",
            action_label: "去发运",
            permission: PERM_TRANSFER_STATUS,
        },
        "IN_TRANSIT" => NextAction {
            status_label: "已发运待收货",
            action_label: "去收货",
            permission: PERM_TRANSFER_STATUS,
        },
        _ => NextAction {
            status_label: "状态未知",
            action_label: "去处理",
            permission: "",
        },
    }
}

/// 按单据类型分发「下一个动作」。
pub fn next_action(doc_type: &str, status: &str) -> NextAction {
    match doc_type {
        DOC_VOUCHER => voucher_next(status),
        DOC_PURCHASE_ORDER => purchase_order_next(status),
        DOC_STOCK_IN => stock_in_next(status),
        DOC_STOCK_TRANSFER => stock_transfer_next(status),
        _ => NextAction {
            status_label: "状态未知",
            action_label: "去处理",
            permission: "",
        },
    }
}

/// 单据类型的中文名（催办正文 / 日志用）。
pub fn doc_type_label(doc_type: &str) -> &'static str {
    match doc_type {
        DOC_VOUCHER => "凭证",
        DOC_STOCK_IN => "收货单",
        DOC_PURCHASE_ORDER => "采购单",
        DOC_STOCK_TRANSFER => "调拨单",
        DOC_INVENTORY => "库存行",
        _ => "单据",
    }
}

// ==================== 明细行读取 ====================

/// 从库里读出来的一行原始单据（四类单据共用的投影）。
#[derive(Debug, Clone)]
struct RawDoc {
    id: Uuid,
    doc_no: String,
    status: String,
    amount: Option<Decimal>,
    created_at: DateTime<Utc>,
    creator: Option<String>,
}

/// 四类单据的行 SQL 投影必须**逐列同名**（`doc_id / doc_no / status / amount / created_at / creator`），
/// 这样解析只有一份实现（少一处就能少一处漂移）。
fn parse_docs(rows: &[Row], ctx: &str) -> Result<Vec<RawDoc>, ApiError> {
    let mut docs = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.get_typed("doc_id").map_err(|error| db_error(error, ctx))?;
        let doc_no: String = row.get_typed("doc_no").map_err(|error| db_error(error, ctx))?;
        let status: String = row.get_typed("status").map_err(|error| db_error(error, ctx))?;
        let amount_text: Option<String> = row.get_typed("amount").ok();
        let amount = amount_text.as_deref().and_then(|text| Decimal::from_str(text).ok());
        let created_at: DateTime<Utc> = row.get_typed("created_at").map_err(|error| db_error(error, ctx))?;
        let creator: Option<String> = row.get_typed("creator").ok();
        docs.push(RawDoc {
            id,
            doc_no,
            status,
            amount,
            created_at,
            creator: creator.filter(|name| !name.trim().is_empty()),
        });
    }
    Ok(docs)
}

/// 前端路由（带 id 直达）。凭证用 `voucher_no`（凭证页的深链口径），
/// 采购单 / 收货单用 `id`（页面的 `po_id` / `in_id` 深链口径）。
fn route_of(doc_type: &str, id: &Uuid, doc_no: &str) -> String {
    match doc_type {
        DOC_VOUCHER => format!("{ROUTE_VOUCHER}?voucher_no={}", url_encode(doc_no)),
        DOC_PURCHASE_ORDER => format!("{ROUTE_PURCHASE_ORDER}?po_id={id}"),
        DOC_STOCK_IN => format!("{ROUTE_STOCK_IN}?in_id={id}"),
        DOC_STOCK_TRANSFER => format!("{ROUTE_STOCK_TRANSFER}?transfer_no={}", url_encode(doc_no)),
        _ => ROUTE_INVENTORY.to_string(),
    }
}

/// 只转义会破坏 query 的字符（单号是 `PZ-2031-08-001` 这种，用全量 percent-encode 会更难读）。
fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(ch),
            other => {
                let mut buffer = [0u8; 4];
                for byte in other.encode_utf8(&mut buffer).as_bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    out
}

/// 原始行 → 下发明细（补状态中文 / 动作文案 / 路由 / 处理人）。
async fn build_items(
    ex: &impl SqlExecutor,
    doc_type: &'static str,
    docs: Vec<RawDoc>,
    owners: &mut OwnerResolver,
) -> Result<Vec<BlockingItem>, ApiError> {
    let mut items = Vec::with_capacity(docs.len());
    for doc in docs {
        let action = next_action(doc_type, &doc.status);
        let owner = owners.resolve(ex, action.permission).await?;
        items.push(BlockingItem {
            doc_type: doc_type.to_string(),
            doc_id: doc.id,
            doc_no: doc.doc_no.clone(),
            status_label: action.status_label.to_string(),
            amount: doc.amount,
            created_by_name: doc.creator,
            owner_role: owner.as_ref().map(|item| item.role_code.clone()),
            owner_name: owner.as_ref().and_then(|item| item.user_name.clone()),
            action_label: action.action_label.to_string(),
            route: route_of(doc_type, &doc.id, &doc.doc_no),
            waiting_since: Some(doc.created_at),
            owner_role_id: owner.as_ref().map(|item| item.role_id),
            owner_user_id: owner.as_ref().and_then(|item| item.user_id),
        });
    }
    Ok(items)
}

/// 一条「按单据类型取数」的取数口径：条数 SQL + 明细 SQL + 参数。
struct DocSource {
    doc_type: &'static str,
    count_sql: String,
    rows_sql: String,
    params: Vec<Value>,
}

impl DocSource {
    /// 取「条数 + 明细（最多 50 条）」。
    async fn load(
        &self,
        ex: &impl SqlExecutor,
        owners: &mut OwnerResolver,
    ) -> Result<(i64, Vec<BlockingItem>), ApiError> {
        let count: i64 = ex
            .scalar_sql(self.count_sql.clone(), self.params.clone())
            .await
            .map_err(|error| db_error(error, "统计挡路单据"))?;
        if count <= 0 {
            return Ok((0, Vec::new()));
        }
        let rows = ex
            .query_sql(self.rows_sql.clone(), self.params.clone())
            .await
            .map_err(|error| db_error(error, "读取挡路单据明细"))?;
        let docs = parse_docs(&rows, "解析挡路单据")?;
        let items = build_items(ex, self.doc_type, docs, owners).await?;
        Ok((count, items))
    }
}

// ==================== ① 草稿凭证 ====================

/// 期间内草稿凭证：`(总数, 明细)`。
pub async fn draft_voucher_items(
    ex: &impl SqlExecutor,
    period_code: &str,
    owners: &mut OwnerResolver,
) -> Result<(i64, Vec<BlockingItem>), ApiError> {
    let params = vec![Value::Text(period_code.to_string())];
    DocSource {
        doc_type: DOC_VOUCHER,
        count_sql: "SELECT COUNT(*) FROM biz_voucher \
                    WHERE deleted_at IS NULL AND period = ? AND status = 'DRAFT'"
            .to_string(),
        rows_sql: format!(
            "SELECT v.id AS doc_id, v.voucher_no AS doc_no, v.status AS status, \
                    v.total_debit::text AS amount, v.created_at AS created_at, \
                    COALESCE(NULLIF(u.real_name, ''), u.username) AS creator \
               FROM biz_voucher v LEFT JOIN sys_user u ON u.id = v.create_by \
              WHERE v.deleted_at IS NULL AND v.period = ? AND v.status = 'DRAFT' \
              ORDER BY v.voucher_no LIMIT {ITEM_LIMIT}"
        ),
        params,
    }
    .load(ex, owners)
    .await
}

// ==================== ② 未完成单据 ====================

/// 期间内未完成单据：`(按类型条数, 明细)`。
///
/// 判据与 `period.rs` 头注释 §② 逐字一致（日期落在期间区间 + 状态不在终态）：
/// * 采购单 `DRAFT / SUBMITTED / APPROVED / PARTIAL_RECEIVED`
/// * 收货单 `DRAFT / PENDING_POST / PENDING_CANCEL`
/// * 调拨单 `DRAFT / PENDING / APPROVED / IN_TRANSIT`
///
/// 明细按「单据日期 + 单号」排序，**三类混排后统一截断**到 [`ITEM_LIMIT`] 条
/// （每条 blocker 的明细上限是 50，不能因为合并了三类就变成 150）；
/// 条数是**全量** `COUNT(*)`（前端据此提示「共 N 张，仅显示前 50 张」）。
///
/// 年结走 [`unfinished_doc_items_between`]（同一份 SQL，只是把「一个月」换成「全年」）。
pub async fn unfinished_doc_items(
    ex: &impl SqlExecutor,
    row: &PeriodRow,
    owners: &mut OwnerResolver,
) -> Result<(Vec<(&'static str, i64)>, Vec<BlockingItem>), ApiError> {
    unfinished_doc_items_between(ex, row.start_date, row.end_date, owners).await
}

/// 日期区间内未完成单据：`(按类型条数, 明细)`。
///
/// 与 [`unfinished_doc_items`] 是**同一份取数口径**（本函数是唯一实现）：
/// 期间结账传「当月 1 日 ~ 月末」，年结传「1 月 1 日 ~ 12 月 31 日」。
/// 年结合并 12 个月不必按月循环 12 次 —— 一条 `BETWEEN` 就等价，
/// 而且天然不会踩「12 个月 × 50 = 600 行」的坑。
pub async fn unfinished_doc_items_between(
    ex: &impl SqlExecutor,
    start: NaiveDate,
    end: NaiveDate,
    owners: &mut OwnerResolver,
) -> Result<(Vec<(&'static str, i64)>, Vec<BlockingItem>), ApiError> {
    let mut counts = Vec::with_capacity(3);
    let mut items = Vec::new();

    for (label, source) in doc_sources_between(start, end) {
        let (count, mut loaded) = source.load(ex, owners).await?;
        counts.push((label, count));
        items.append(&mut loaded);
    }

    // 明细跨三类混排：按「已等最久」优先（等得最久的先处理），同时间按单号。
    items.sort_by(|left, right| {
        left.waiting_since
            .cmp(&right.waiting_since)
            .then_with(|| left.doc_no.cmp(&right.doc_no))
    });
    // 上限是「每条 blocker ≤ 50 条」：三类混排后统一截断
    // （否则 3 类 × 50 = 150 行，与「每类 blocker 最多 50 条」的验收口径不符，也会把弹窗撑爆）。
    items.truncate(usize::try_from(ITEM_LIMIT).unwrap_or(50));
    Ok((counts, items))
}

/// 三类未完成单据的取数口径（标签 + SQL）。抽成函数是为了让测试能逐类断言。
///
/// 参数是**日期区间**（不是 `PeriodRow`）：期间结账与年结合用一份 SQL，
/// 差别只在区间大小 —— 两处口径不允许分裂。
fn doc_sources_between(start: NaiveDate, end: NaiveDate) -> [(&'static str, DocSource); 3] {
    let start = start.to_string();
    let end = end.to_string();
    let date_params = vec![Value::Text(start.clone()), Value::Text(end.clone())];

    let purchase = DocSource {
        doc_type: DOC_PURCHASE_ORDER,
        count_sql: "SELECT COUNT(*) FROM biz_purchase_order \
                    WHERE deleted_at IS NULL \
                      AND order_date BETWEEN ?::text::date AND ?::text::date \
                      AND status IN ('DRAFT','SUBMITTED','APPROVED','PARTIAL_RECEIVED')"
            .to_string(),
        rows_sql: format!(
            "SELECT p.id AS doc_id, p.order_no AS doc_no, p.status AS status, \
                    p.total_with_tax::text AS amount, p.created_at AS created_at, \
                    COALESCE(NULLIF(u.real_name, ''), u.username) AS creator \
               FROM biz_purchase_order p LEFT JOIN sys_user u ON u.id = p.create_by \
              WHERE p.deleted_at IS NULL \
                AND p.order_date BETWEEN ?::text::date AND ?::text::date \
                AND p.status IN ('DRAFT','SUBMITTED','APPROVED','PARTIAL_RECEIVED') \
              ORDER BY p.order_date, p.order_no LIMIT {ITEM_LIMIT}"
        ),
        params: date_params.clone(),
    };

    let stock_in = DocSource {
        doc_type: DOC_STOCK_IN,
        count_sql: "SELECT COUNT(*) FROM biz_stock_in \
                    WHERE deleted_at IS NULL \
                      AND in_date BETWEEN ?::text::date AND ?::text::date \
                      AND status IN ('DRAFT','PENDING_POST','PENDING_CANCEL')"
            .to_string(),
        rows_sql: format!(
            "SELECT s.id AS doc_id, s.in_no AS doc_no, s.status AS status, \
                    s.total_with_tax::text AS amount, s.created_at AS created_at, \
                    COALESCE(NULLIF(u.real_name, ''), u.username) AS creator \
               FROM biz_stock_in s LEFT JOIN sys_user u ON u.id = s.create_by \
              WHERE s.deleted_at IS NULL \
                AND s.in_date BETWEEN ?::text::date AND ?::text::date \
                AND s.status IN ('DRAFT','PENDING_POST','PENDING_CANCEL') \
              ORDER BY s.in_date, s.in_no LIMIT {ITEM_LIMIT}"
        ),
        params: date_params.clone(),
    };

    let transfer = DocSource {
        doc_type: DOC_STOCK_TRANSFER,
        count_sql: "SELECT COUNT(*) FROM biz_stock_transfer \
                    WHERE deleted_at IS NULL \
                      AND transfer_date BETWEEN ?::text::date AND ?::text::date \
                      AND status IN ('DRAFT','PENDING','APPROVED','IN_TRANSIT')"
            .to_string(),
        rows_sql: format!(
            "SELECT t.id AS doc_id, t.transfer_no AS doc_no, t.status AS status, \
                    t.total_amount::text AS amount, t.created_at AS created_at, \
                    COALESCE(NULLIF(u.real_name, ''), u.username, t.applicant_name) AS creator \
               FROM biz_stock_transfer t LEFT JOIN sys_user u ON u.id = t.create_by \
              WHERE t.deleted_at IS NULL \
                AND t.transfer_date BETWEEN ?::text::date AND ?::text::date \
                AND t.status IN ('DRAFT','PENDING','APPROVED','IN_TRANSIT') \
              ORDER BY t.transfer_date, t.transfer_no LIMIT {ITEM_LIMIT}"
        ),
        params: date_params,
    };

    [("采购单", purchase), ("收货单", stock_in), ("调拨单", transfer)]
}

// ==================== ④ 不变量（期间内 A 组） ====================

/// 期间内不变量失败：`(给文案的失败描述, 明细)`。
///
/// 判据（与 `period.rs` 头注释 §④ 一致，只放「属于这个月、这个月该修」的两条）：
/// * **A1** 每张凭证 Σ分录借 ≠ Σ分录贷
/// * **A2** 凭证头金额 ≠ 分录取和
///
/// 明细里的单据是**凭证**，动作是「去看凭证」；处理人**留空**——
/// 账被改坏了要怎么改（红冲？补分录？）是财务主管的判断，系统不指定人。
pub async fn invariant_details(
    ex: &impl SqlExecutor,
    period_code: &str,
    owners: &mut OwnerResolver,
) -> Result<(Vec<String>, Vec<BlockingItem>), ApiError> {
    invariant_details_scoped(ex, VoucherScope::Period(period_code), owners).await
}

/// 日期区间内的不变量失败：`(失败描述, 明细)` —— 年结的「全年 A1 / A2」走这里。
///
/// 与 [`invariant_details`] 同一份实现（差别只有取数范围），
/// 所以年结页与结账页看到的不变量口径一致，不会「结账说平、年结说不平」。
pub async fn invariant_details_between(
    ex: &impl SqlExecutor,
    start: NaiveDate,
    end: NaiveDate,
    owners: &mut OwnerResolver,
) -> Result<(Vec<String>, Vec<BlockingItem>), ApiError> {
    invariant_details_scoped(ex, VoucherScope::Dates(start, end), owners).await
}

/// 凭证取数范围：**期间**（`v.period = ?`，与 P1 结账同口径）或**日期区间**
/// （`voucher_date` 落在区间内，年结的「全年」口径）。
///
/// 唯一的差别是占位符（1 个 vs 2 个），所以用枚举把「同一段 SQL + 同一套参数」收在一处：
/// A1 / A2 的 where 片段由 [`VoucherScope::filter`] 给出，计数与明细共用，
/// 两处口径不允许分裂（历史上「条数说 3 张、表格列 5 行」就是这么来的）。
#[derive(Debug, Clone, Copy)]
enum VoucherScope<'a> {
    /// 单期间（`2036-03`）。
    Period(&'a str),
    /// 日期区间（年结：`2036-01-01` ~ `2036-12-31`）。
    Dates(NaiveDate, NaiveDate),
}

impl VoucherScope<'_> {
    /// WHERE 片段（**不含** `WHERE` 关键字）。
    fn filter(&self) -> &'static str {
        match self {
            Self::Period(_) => "v.deleted_at IS NULL AND v.period = ?",
            Self::Dates(_, _) => {
                "v.deleted_at IS NULL AND v.voucher_date >= ?::text::date \
                 AND v.voucher_date <= ?::text::date"
            }
        }
    }

    /// 与 [`Self::filter`] 一一对应的参数（顺序不能乱）。
    fn params(&self) -> Vec<Value> {
        match self {
            Self::Period(code) => vec![Value::Text((*code).to_string())],
            Self::Dates(start, end) => {
                vec![Value::Text(start.to_string()), Value::Text(end.to_string())]
            }
        }
    }
}

/// 不变量取数的唯一实现（期间 / 日期区间共用）。
async fn invariant_details_scoped(
    ex: &impl SqlExecutor,
    scope: VoucherScope<'_>,
    owners: &mut OwnerResolver,
) -> Result<(Vec<String>, Vec<BlockingItem>), ApiError> {
    let params = scope.params();
    let filter = scope.filter();
    let mut failures = Vec::new();
    let mut items = Vec::new();

    // ---------- A1：分录借贷不平 ----------
    let a1_having = "HAVING ABS(COALESCE(SUM(e.debit_amount), 0) - COALESCE(SUM(e.credit_amount), 0)) > 0.0001";
    let a1_where = format!(
        "FROM biz_voucher v LEFT JOIN biz_voucher_entry e ON e.voucher_id = v.id \
         WHERE {filter} \
         GROUP BY v.id, v.voucher_no \
         {a1_having}"
    );
    let a1_total: i64 = ex
        .scalar_sql(
            format!("SELECT COUNT(*) FROM (SELECT 1 {a1_where}) bad"),
            params.clone(),
        )
        .await
        .map_err(|error| db_error(error, "不变量 A1 计数"))?;
    if a1_total > 0 {
        let a1_detail_where = format!(
            "FROM biz_voucher v LEFT JOIN biz_voucher_entry e ON e.voucher_id = v.id \
                   LEFT JOIN sys_user u ON u.id = v.create_by \
             WHERE {filter} \
             GROUP BY v.id, v.voucher_no, v.status, v.total_debit, v.created_at, \
                      u.real_name, u.username \
             {a1_having}"
        );
        let rows = ex
            .query_sql(
                format!(
                    "SELECT v.id AS doc_id, v.voucher_no AS doc_no, v.status AS status, \
                            v.total_debit::text AS amount, v.created_at AS created_at, \
                            COALESCE(NULLIF(u.real_name, ''), u.username) AS creator \
                     {a1_detail_where} ORDER BY v.voucher_no LIMIT {ITEM_LIMIT}"
                ),
                params.clone(),
            )
            .await
            .map_err(|error| db_error(error, "不变量 A1 明细"))?;
        let names = voucher_names(&rows, 3);
        failures.push(format!("A1 有 {a1_total} 张凭证分录取和不平（{}）", names.join(" / ")));
        let mut loaded = build_items(ex, DOC_VOUCHER, parse_docs(&rows, "解析不变量 A1 明细")?, owners).await?;
        for item in &mut loaded {
            item.status_label = "分录借贷不平".to_string();
            item.action_label = "去看凭证".to_string();
        }
        items.append(&mut loaded);
    }

    // ---------- A2：单头金额 ≠ 分录取和 ----------
    let a2_having = "HAVING ABS(v.total_debit - COALESCE(SUM(e.debit_amount), 0)) > 0.0001 \
                         OR ABS(v.total_credit - COALESCE(SUM(e.credit_amount), 0)) > 0.0001";
    let a2_where = format!(
        "FROM biz_voucher v LEFT JOIN biz_voucher_entry e ON e.voucher_id = v.id \
         WHERE {filter} \
         GROUP BY v.id, v.voucher_no, v.total_debit, v.total_credit \
         {a2_having}"
    );
    let a2_total: i64 = ex
        .scalar_sql(
            format!("SELECT COUNT(*) FROM (SELECT 1 {a2_where}) bad"),
            params.clone(),
        )
        .await
        .map_err(|error| db_error(error, "不变量 A2 计数"))?;
    if a2_total > 0 {
        let rows = ex
            .query_sql(
                format!(
                    "SELECT v.id AS doc_id, v.voucher_no AS doc_no, v.status AS status, \
                            v.total_debit::text AS amount, v.created_at AS created_at, \
                            COALESCE(NULLIF(u.real_name, ''), u.username) AS creator \
                       FROM biz_voucher v LEFT JOIN biz_voucher_entry e ON e.voucher_id = v.id \
                            LEFT JOIN sys_user u ON u.id = v.create_by \
                      WHERE {filter} \
                      GROUP BY v.id, v.voucher_no, v.status, v.total_debit, v.total_credit, \
                               v.created_at, u.real_name, u.username \
                     {a2_having} \
                      ORDER BY v.voucher_no LIMIT {ITEM_LIMIT}"
                ),
                params,
            )
            .await
            .map_err(|error| db_error(error, "不变量 A2 明细"))?;
        let names = voucher_names(&rows, 3);
        failures.push(format!(
            "A2 有 {a2_total} 张凭证头与分录不一致（{}）",
            names.join(" / ")
        ));
        let mut loaded = build_items(ex, DOC_VOUCHER, parse_docs(&rows, "解析不变量 A2 明细")?, owners).await?;
        for item in &mut loaded {
            item.status_label = "凭证头与分录不一致".to_string();
            item.action_label = "去看凭证".to_string();
        }
        items.append(&mut loaded);
    }

    Ok((failures, dedupe_voucher_items(items)))
}

/// 同一张凭证同时踩 A1 和 A2 时只留一行（状态标签合并），
/// 否则弹窗里会出现两行一模一样的凭证号 —— 用户会以为系统数错了。
fn dedupe_voucher_items(items: Vec<BlockingItem>) -> Vec<BlockingItem> {
    let mut index_of: HashMap<Uuid, usize> = HashMap::new();
    let mut merged: Vec<BlockingItem> = Vec::with_capacity(items.len());
    for item in items {
        match index_of.get(&item.doc_id).copied() {
            Some(index) => {
                merged[index].status_label = "分录与头表都不一致".to_string();
            }
            None => {
                index_of.insert(item.doc_id, merged.len());
                merged.push(item);
            }
        }
    }
    merged
}

/// 前 N 个凭证号（文案里点名用）。
fn voucher_names(rows: &[Row], limit: usize) -> Vec<String> {
    rows.iter()
        .take(limit)
        .map(|row| row.get_typed::<String>("doc_no").unwrap_or_default())
        .filter(|no| !no.is_empty())
        .collect()
}

// ==================== 全库巡检提示的明细 ====================

/// 负库存行（C1/C2）：`(总数, 明细)`。**只提示、不拦结账**（口径见 `period.rs`）。
pub async fn negative_stock_items(ex: &impl SqlExecutor) -> Result<(i64, Vec<BlockingItem>), ApiError> {
    let count: i64 = ex
        .scalar_sql(
            "SELECT (SELECT COUNT(*) FROM biz_inventory WHERE qty < 0) \
                  + (SELECT COUNT(*) FROM biz_inventory_location WHERE qty < 0)"
                .to_string(),
            vec![],
        )
        .await
        .map_err(|error| db_error(error, "巡检 C1/C2 计数"))?;
    if count == 0 {
        return Ok((0, Vec::new()));
    }
    let rows = ex
        .query_sql(
            format!(
                "SELECT i.id AS doc_id, \
                        COALESCE(m.material_code || ' ' || m.material_name, i.material_id::text) \
                            || ' @ ' || COALESCE(w.warehouse_name, i.warehouse_id::text) AS doc_no, \
                        'NEGATIVE' AS status, i.amount::text AS amount, \
                        i.created_at AS created_at, NULL::text AS creator \
                   FROM biz_inventory i \
                   LEFT JOIN biz_material m ON m.id = i.material_id \
                   LEFT JOIN biz_warehouse w ON w.id = i.warehouse_id \
                  WHERE i.qty < 0 \
                  ORDER BY i.qty LIMIT {ITEM_LIMIT}"
            ),
            vec![],
        )
        .await
        .map_err(|error| db_error(error, "巡检 C1/C2 明细"))?;
    let docs = parse_docs(&rows, "解析负库存明细")?;
    let items = inventory_items(docs);
    Ok((count, items))
}

/// 账实漂移组（B4）：`(总数, 明细)`。**只提示、不拦结账**。
pub async fn stock_drift_items(ex: &impl SqlExecutor) -> Result<(i64, Vec<BlockingItem>), ApiError> {
    let count: i64 = ex
        .scalar_sql(
            "SELECT COUNT(*) FROM (\
                WITH txn AS (SELECT material_id, warehouse_id, \
                                    SUM(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END) AS q \
                               FROM biz_inventory_txn GROUP BY 1, 2), \
                     inv AS (SELECT material_id, warehouse_id, SUM(qty) AS q \
                               FROM biz_inventory GROUP BY 1, 2) \
                SELECT 1 FROM txn t FULL OUTER JOIN inv i \
                  ON t.material_id = i.material_id AND t.warehouse_id = i.warehouse_id \
                 WHERE ABS(COALESCE(t.q, 0) - COALESCE(i.q, 0)) > 0.0001) bad"
                .to_string(),
            vec![],
        )
        .await
        .map_err(|error| db_error(error, "巡检 B4 计数"))?;
    if count == 0 {
        return Ok((0, Vec::new()));
    }
    let rows = ex
        .query_sql(
            format!(
                "SELECT COALESCE(m.material_id, i.material_id) AS doc_id, \
                        COALESCE(mc.material_code || ' ' || mc.material_name, \
                                 COALESCE(m.material_id, i.material_id)::text) \
                            || ' @ ' || COALESCE(wc.warehouse_name, \
                                                 COALESCE(m.warehouse_id, i.warehouse_id)::text) AS doc_no, \
                        'DRIFT' AS status, \
                        ABS(COALESCE(m.q, 0) - COALESCE(i.q, 0))::text AS amount, \
                        CURRENT_TIMESTAMP AS created_at, NULL::text AS creator \
                   FROM (SELECT material_id, warehouse_id, \
                                SUM(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END) AS q \
                           FROM biz_inventory_txn GROUP BY 1, 2) m \
                   FULL OUTER JOIN (SELECT material_id, warehouse_id, SUM(qty) AS q \
                                      FROM biz_inventory GROUP BY 1, 2) i \
                     ON m.material_id = i.material_id AND m.warehouse_id = i.warehouse_id \
                   LEFT JOIN biz_material mc ON mc.id = COALESCE(m.material_id, i.material_id) \
                   LEFT JOIN biz_warehouse wc ON wc.id = COALESCE(m.warehouse_id, i.warehouse_id) \
                  WHERE ABS(COALESCE(m.q, 0) - COALESCE(i.q, 0)) > 0.0001 \
                  ORDER BY 4 DESC LIMIT {ITEM_LIMIT}"
            ),
            vec![],
        )
        .await
        .map_err(|error| db_error(error, "巡检 B4 明细"))?;
    let docs = parse_docs(&rows, "解析账实漂移明细")?;
    let items = inventory_items(docs);
    Ok((count, items))
}

/// 库存类明细的统一形态（动作「去看库存」，处理人留空）。
fn inventory_items(docs: Vec<RawDoc>) -> Vec<BlockingItem> {
    docs.into_iter()
        .map(|doc| BlockingItem {
            doc_type: DOC_INVENTORY.to_string(),
            doc_id: doc.id,
            doc_no: doc.doc_no,
            status_label: match doc.status.as_str() {
                "NEGATIVE" => "负库存".to_string(),
                "DRIFT" => "流水累计 ≠ 当前库存".to_string(),
                other => other.to_string(),
            },
            amount: doc.amount,
            created_by_name: None,
            owner_role: None,
            owner_name: None,
            action_label: "去看库存".to_string(),
            route: format!("{ROUTE_INVENTORY}?audit=1"),
            waiting_since: None,
            owner_role_id: None,
            owner_user_id: None,
        })
        .collect()
}

// ==================== 错误收口 ====================

/// 数据库错误 → 500（带上下文）。
fn db_error(error: DbError, ctx: &str) -> ApiError {
    utils::db_error(error, ctx)
}

#[cfg(test)]
#[path = "period_items_tests.rs"]
mod tests;
