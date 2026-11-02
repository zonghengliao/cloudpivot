//! 合同数据层（A1 合同表基座）：**乙方引用解析**与状态机接线点。
//!
//! 依据：`docs/architecture/ARCH-2-合同表数据模型与校验规则.md`
//!       §3.2 的 V-11 / V-15 与 §3.2.3 的批次 2 替换点 R2；表结构见 `V0195__erp_contract.up.sql`。
//!
//! # 本文件为什么这么小（A1 的边界）
//!
//! A1 只交付**基座**：建表、状态机注册、错误码、权限码、乙方引用解析。
//! 合同的 CRUD / 列表 / 详情 / 状态流转接口是 A2（`contract_handler` / `contract_dto`），
//! 采购单挂合同是 A3，入库侧是 A4，前端是 A5。所以这里**故意没有**：
//! `ContractRow` 结构体、分页查询、取号（`next_contract_no`）、建改删。它们进 A2 时
//! 直接在本文件往下加（模块已声明、`erp/mod.rs` 已挂载，不需要再动装配）。
//!
//! # 乙方没有外键（DEC-02）——这是设计意图，不是漏了
//!
//! `biz_contract.party_b_id` 是**弱引用**（`biz_supplier`，批次 2 起还有 `biz_customer`）。
//! POC 实测：往合同里写一个指向不存在供应商的 id，**DB 写入成功**；随后孤儿巡检 SQL
//! 精准捞出这一行。这就是 DEC-02 的代价与配套：
//!
//! ```text
//! DB 不拦  →  应用层写入校验（本文件的 party_b_ref，A2 在 ensure_party_b 里调）
//!          →  每日巡检兜底（ARCH-2 §3.3 Q1，落地在 B1 的巡检卡）
//! ```
//!
//! **不要"顺手"给 `party_b_id` 补外键**：`.down.sql` 会因此不可回滚、软删主档会连带
//! 失败，而且违反 ARCH-2 §2.3 末段的实测结论。
//!
//! # 批次 2 的替换点（R2，写在这里免得后人翻文档）
//!
//! 客户主数据上线后，本文件只改一处：给 [`party_b_ref`] 增加 `CUSTOMER` 分支
//! （查 `biz_customer(id, deleted_at IS NULL, status = 1)` 返回 `customer_name`）。
//! 若客户表的列名不同（如 `customer_status`），映射**只能写在本函数内**，不外溢。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::contract_dto::ContractQuery;
use crate::business::shared::scope;
use crate::business::shared::state_machine;

/// 合同表名（A2 复用，避免各处硬编码）。
pub const CONTRACT_TABLE: &str = "biz_contract";

/// 乙方类型：供应商（`party_b_id → biz_supplier.id`）。
pub const PARTY_B_SUPPLIER: &str = "SUPPLIER";

/// 乙方类型：客户（表属批次 2，本期写入硬拒 66020，见 ARCH-2 §3.2.3）。
pub const PARTY_B_CUSTOMER: &str = "CUSTOMER";

/// 合同的 5 个状态（与 DDL 的 `ck_biz_contract_status` 白名单逐字一致）。
///
/// 状态推进**只能**走 [`crate::business::shared::state_machine::transition`]
/// （`Entity::Contract`，ARCH-2 DEC-10）；本模块不提供 `UPDATE ... SET status`，
/// 也不该有人提供 —— `rg "SET status" business/erp/contract_*` 必须 0 命中。
pub const STATUSES: [&str; 5] = ["DRAFT", "SIGNED", "ACTIVE", "COMPLETED", "TERMINATED"];

/// 草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 已签订（日期三件套必填；此后关键字段冻结）。
pub const STATUS_SIGNED: &str = "SIGNED";
/// 已生效。
pub const STATUS_ACTIVE: &str = "ACTIVE";
/// 履约完成（终态，不再接受新单）。
pub const STATUS_COMPLETED: &str = "COMPLETED";
/// 已终止（终态；原因写 `remark` 与 `biz_status_log.reason`）。
pub const STATUS_TERMINATED: &str = "TERMINATED";

/// 采购单可以引用的合同状态（ARCH-2 §5.1 V-22 步 2）。
///
/// **只在建单与换合同时用**：提交 / 审批 / 收货不再复检合同状态，
/// 否则「PO 建好三天后合同到期」会让一张已成立的承诺再也提交不了（§5.1 末段）。
pub const USABLE_FOR_ORDER: [&str; 2] = [STATUS_SIGNED, STATUS_ACTIVE];

/// 解析乙方引用：`SUPPLIER` 分支返回 `biz_supplier.supplier_name`（未软删才算存在）。
///
/// 照抄 [`crate::business::erp::purchase_model::supplier_ref`] 的写法（ARCH-2 附录 B #5
/// 指名了这个范本），签名多一个 `party_b_type`：批次 2 要在这里分叉到 `biz_customer`，
/// 现在就把闸口留好，届时只加分支、不改调用方（§3.2.3 R2）。
///
/// # 参数
///
/// * `party_b_type` —— `SUPPLIER` / `CUSTOMER`（其它值返回 `Ok(None)`，由调用方报 66020/66021）
/// * `id` —— 乙方主键
///
/// # 返回值
///
/// * `Ok(Some(name))` —— 主档存在且未软删；`name` 直接作为 `party_b_name` **快照**落库
///   （V-15：不接受前端自填，否则审计上的"当时乙方是谁"就不可信）
/// * `Ok(None)` —— 不存在 / 已软删 / 本期不支持的乙方类型
/// * `Err(DbError)` —— 数据库异常（连不上、类型不符），由调用方 `?` 上抛
///
/// # 幂等与副作用
///
/// 纯读，无副作用；不加锁（读主档名称不需要与主档写入串行）。
///
/// # 本期为什么 `CUSTOMER` 一定返回 `None`
///
/// 客户主数据（`biz_customer`）属批次 2。**不要**用「先存 id 后补主档」绕过 ——
/// ARCH-2 §3.2.3 明确禁止：那会制造一批永远无法校验的孤儿数据，等客户表上线时
/// 无人知道该把哪条 id 认领成客户。故此处直接返回 `None`，由 A2 的
/// `ensure_party_b` 第一行转成 `66020 客户类合同暂未开放`。
///
/// # 示例
///
/// ```ignore
/// // A2 的 ensure_party_b（ARCH-2 §3.2 V-11 / V-15）
/// match contract_model::party_b_ref(&req.party_b_type, req.party_b_id).await? {
///     Some(name) => party_b_name = name,                       // 快照用服务端读到的名字
///     None => return Err(ApiError::new(400, error_code::ERP_CONTRACT_PARTY_INVALID,
///                                      "乙方供应商不存在或已停用")),
/// }
/// ```
pub async fn party_b_ref(party_b_type: &str, id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: biz_supplier 是全局主数据（无 dept_id / 归属人列），
    // 这里是一次按主键的存在性解析，不是列表查询，不涉及数据范围。
    match party_b_type {
        PARTY_B_SUPPLIER => {
            let rows = db()
                .find_with_params(
                    "SELECT supplier_name FROM biz_supplier WHERE id = ?::uuid AND deleted_at IS NULL",
                    vec![Value::from(&id)],
                )
                .await?;
            match rows.first() {
                None => Ok(None),
                Some(row) => Ok(Some(row.get_typed("supplier_name")?)),
            }
        }
        // 批次 2 在这里加 `biz_customer` 分支（§3.2.3 R2）；本期一律 None → 66020。
        _ => Ok(None),
    }
}

/// 供应商是否可用于**新签**合同（ARCH-2 V-11：新建时额外要求 `status = 1`）。
///
/// 为什么与 [`party_b_ref`] 分开：V-11 明写「停用只在**新建**校验；修改其它字段不重验」——
/// 否则供应商一停用，连合同的备注都改不了。因此两个判定不能合并成一个函数，
/// 调用点必须显式表达「这是新建还是改其它字段」。
///
/// # 返回值
///
/// * `Ok(Some(name))` —— 存在、未软删、且 `status = 1`
/// * `Ok(None)` —— 不存在 / 已软删 / 已停用（三者对外话术一致：66021）
pub async fn party_b_ref_for_new(party_b_type: &str, id: Uuid) -> DbResult<Option<String>> {
    if party_b_type != PARTY_B_SUPPLIER {
        // 客户分支本期不开（66020），不查任何表
        return Ok(None);
    }
    let rows = db()
        .find_with_params(
            "SELECT supplier_name FROM biz_supplier \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = 1",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(row.get_typed("supplier_name")?)),
    }
}

// ==================== A2：行结构 / 查询 / 写入 ====================
//
// 分工：本段只有 SQL 与行映射；校验、错误码、事务边界在 `contract_handler`，
// 入参 / 出参形状在 `contract_dto`。**这里不写 `UPDATE … SET status`**：
// 状态推进的唯一入口是 `state_machine::transition`（见文首 [`STATUSES`] 的说明）。

/// 合同的合法边（**指向 `state_machine::CONTRACT_EDGES`，不再单独维护一份字面量**）。
///
/// `transition()` 会拿调用方传的边表与注册表**核对**，不一致直接 500（防「偷偷多放一条边」）。
/// 【D1 状态机边配置化】之前这里刻意抄了一份字面量「让漂移编译期可见」；现在
/// `state_machine::BASE_EDGES` 成了唯一真相（并新增 `base_edges_match_registry` 单测
/// 钉死 BASE 与注册表逐条同序），再抄一份只会制造**第三个**需要同步的地方，
/// 所以改成别名：想多放一条边必须改 `shared/state_machine.rs`，别的路径做不到。
pub const LEGAL_EDGES: [(&str, &str); 5] = state_machine::CONTRACT_EDGES;

/// 「日期三件套必填」的状态集合（与 DDL 的 `ck_biz_contract_signed_dates` 逐字一致）。
///
/// 新建允许不填日期（草稿可以先建后补），但**流转到这三个状态之前**必须补齐，
/// 否则 DB 的 CHECK 会以 500 的形式炸在状态流转接口上。
pub const FULL_DATES_STATUSES: [&str; 3] = [STATUS_SIGNED, STATUS_ACTIVE, STATUS_COMPLETED];

/// 状态中文名（前端列表直接显示；未知值给「未知状态」而不是 panic）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SIGNED => "已签订",
        STATUS_ACTIVE => "已生效",
        STATUS_COMPLETED => "已完成",
        STATUS_TERMINATED => "已终止",
        _ => "未知状态",
    }
}

/// 该状态是否要求签订 / 生效 / 到期三个日期齐全（DDL 口径）。
pub fn requires_full_dates(status: &str) -> bool {
    FULL_DATES_STATUSES.contains(&status)
}

/// 单号前缀（`V0195` 种的 `biz_seq(seq_type='CT')` 是 `prefix='HT'`）。
pub const NO_PREFIX: &str = "HT";

/// `biz_seq.seq_type`（迁移里种的 CT 行）。
pub const SEQ_TYPE: &str = "CT";

/// 流水号补零宽度（超过 6 位不截断，宁可长一位也不撞号）。
pub const NO_SEQ_WIDTH: usize = 6;

/// 乙方现值（主档名）在行里的别名：`LEFT JOIN biz_supplier` 带出，不做数据源。
pub const PARTY_B_CURRENT_ALIAS: &str = "party_b_current_name";

/// `biz_contract` 的列清单（**带别名 `c`**，供需要 `FOR UPDATE` 的单表查询用）。
pub const CONTRACT_BASE_COLUMNS: &str = "c.id, c.contract_no, c.contract_name, c.contract_type, \
     c.party_a_id, c.party_a_name, c.party_b_type, c.party_b_id, c.party_b_name, c.amount, \
     c.currency_code, c.sign_date, c.start_date, c.end_date, c.status, c.owner_id, c.dept_id, \
     c.remark, c.created_at, c.updated_at, c.create_by, c.update_by, c.deleted_at";

/// 列表 / 详情用的列清单 = 基础列 + 关联带出的乙方现值。
pub const CONTRACT_LIST_COLUMNS: &str = "c.id, c.contract_no, c.contract_name, c.contract_type, \
     c.party_a_id, c.party_a_name, c.party_b_type, c.party_b_id, c.party_b_name, c.amount, \
     c.currency_code, c.sign_date, c.start_date, c.end_date, c.status, c.owner_id, c.dept_id, \
     c.remark, c.created_at, c.updated_at, c.create_by, c.update_by, c.deleted_at, \
     s.supplier_name AS party_b_current_name";

/// 乙方现值关联（**LEFT JOIN**：主档软删 / 不存在也要能查出合同 —— 历史合同不能因为
/// 供应商被删就查不到，那是审计事故；现值给 `null`，快照列照旧可读）。
pub const CONTRACT_JOIN: &str = "FROM biz_contract c \
     LEFT JOIN biz_supplier s ON s.id = c.party_b_id";

/// `biz_contract` 的分页锚点。
///
/// 与 erp 容器其它 13 张表同一口径：显式 `unscoped`（`biz_contract` 有 `dept_id` 列但目前
/// **没有**登记进 `SCOPED_TABLES` —— 数据权限属专业版；此处按容器既有约定声明，改动需单独裁决）。
pub const CONTRACT_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(CONTRACT_TABLE);

/// 默认排序：`created_at DESC`（卡面），`id DESC` 做稳定次序（同毫秒建的单不会抖动）。
pub const CONTRACT_ORDER: &str = "ORDER BY c.created_at DESC, c.id DESC";

/// 合同行（含关联带出的乙方现值）。
#[derive(Debug, Clone)]
pub struct ContractRow {
    /// 主键。
    pub id: Uuid,
    /// 合同号。
    pub contract_no: String,
    /// 合同名称（DDL 列名）。
    pub contract_name: String,
    /// 合同类型。
    pub contract_type: String,
    /// 甲方 id。
    pub party_a_id: Option<Uuid>,
    /// 甲方名称快照。
    pub party_a_name: Option<String>,
    /// 乙方类型。
    pub party_b_type: String,
    /// 乙方 id（弱引用）。
    pub party_b_id: Uuid,
    /// 乙方名称快照（签约时）。
    pub party_b_name: String,
    /// 乙方名称现值（关联 `biz_supplier`；主档已删 / 不存在为 `None`）。
    pub party_b_current_name: Option<String>,
    /// 金额。
    pub amount: Decimal,
    /// 币种。
    pub currency_code: String,
    /// 签订日期。
    pub sign_date: Option<NaiveDate>,
    /// 生效日。
    pub start_date: Option<NaiveDate>,
    /// 到期日。
    pub end_date: Option<NaiveDate>,
    /// 状态。
    pub status: String,
    /// 负责人。
    pub owner_id: Option<Uuid>,
    /// 归属部门。
    pub dept_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 最后修改人。
    pub update_by: Option<Uuid>,
    /// 软删时间。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl ContractRow {
    /// 由查询行构造（列缺失 / 类型不符 → [`DbError`]）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            contract_no: row.get_typed("contract_no")?,
            contract_name: row.get_typed("contract_name")?,
            contract_type: row.get_typed("contract_type")?,
            party_a_id: row.get_typed("party_a_id")?,
            party_a_name: row.get_typed("party_a_name")?,
            party_b_type: row.get_typed("party_b_type")?,
            party_b_id: row.get_typed("party_b_id")?,
            party_b_name: row.get_typed("party_b_name")?,
            party_b_current_name: row.get_typed(PARTY_B_CURRENT_ALIAS)?,
            amount: row.get_typed("amount")?,
            currency_code: row.get_typed("currency_code")?,
            sign_date: row.get_typed("sign_date")?,
            start_date: row.get_typed("start_date")?,
            end_date: row.get_typed("end_date")?,
            status: row.get_typed("status")?,
            owner_id: row.get_typed("owner_id")?,
            dept_id: row.get_typed("dept_id")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// 新建合同的写入结构（handler 校验 / 解析完之后的形态）。
#[derive(Debug, Clone)]
pub struct NewContract {
    /// 合同名称。
    pub contract_name: String,
    /// 合同类型。
    pub contract_type: String,
    /// 甲方 id。
    pub party_a_id: Option<Uuid>,
    /// 甲方名称快照。
    pub party_a_name: Option<String>,
    /// 乙方类型。
    pub party_b_type: String,
    /// 乙方 id。
    pub party_b_id: Uuid,
    /// 乙方名称快照（服务端从主档读，**不接受客户端自填**）。
    pub party_b_name: String,
    /// 金额（> 0）。
    pub amount: Decimal,
    /// 币种。
    pub currency_code: String,
    /// 签订日期。
    pub sign_date: Option<NaiveDate>,
    /// 生效日。
    pub start_date: Option<NaiveDate>,
    /// 到期日。
    pub end_date: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<String>,
    /// 负责人。
    pub owner_id: Option<Uuid>,
    /// 归属部门。
    pub dept_id: Option<Uuid>,
    /// 操作人（写 `create_by` / `update_by`）。
    pub operator: Option<Uuid>,
}

/// 修改合同的写入结构（**合并后的最终值**，整列覆盖写）。
///
/// 为什么给最终值而不是 patch 集合：日期顺序、签订早于生效、金额 > 0 这些规则
/// 必须对**合并后的结果**判定（只改 end_date 也可能把区间改反），
/// 一次算出最终值再整写，规则只判一遍。
#[derive(Debug, Clone)]
pub struct ContractMut {
    /// 合同名称。
    pub contract_name: String,
    /// 乙方 id。
    pub party_b_id: Uuid,
    /// 乙方名称快照。
    pub party_b_name: String,
    /// 金额。
    pub amount: Decimal,
    /// 签订日期。
    pub sign_date: Option<NaiveDate>,
    /// 生效日。
    pub start_date: Option<NaiveDate>,
    /// 到期日。
    pub end_date: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<String>,
}

/// 只含主键 / 单号 / 状态的最小投影（状态流转入口用）。
#[derive(Debug, Clone)]
pub struct ContractStateRow {
    /// 主键。
    pub id: Uuid,
    /// 合同号（状态日志的 `entity_no`）。
    pub contract_no: String,
    /// 当前状态。
    pub status: String,
    /// 签订日期。
    pub sign_date: Option<NaiveDate>,
    /// 生效日。
    pub start_date: Option<NaiveDate>,
    /// 到期日。
    pub end_date: Option<NaiveDate>,
}

/// 分页查询（软删行不返回）。
///
/// # 索引
///
/// `contract_no` / `name` / `keyword` 走 `LIKE`（合同量级远小于单据，不做 `text_pattern_ops`
/// 的额外索引，`idx_biz_contract_status` / `idx_biz_contract_party_b` / `idx_biz_contract_end`
/// 覆盖等值筛选与到期日区间）。
///
/// # 单号「前缀匹配优先」
///
/// 输入只由 `[A-Za-z0-9_-]` 组成时才把 `CASE WHEN contract_no LIKE '<前缀>%' THEN 0 ELSE 1 END`
/// 拼进 ORDER BY（合同号列本身的 `ck_biz_contract_no_charset` 保证了库里值的字符集，
/// 因此不需要转义，也不可能被注入）；含其它字符（`%` / 引号）时**不加**前缀加权，
/// 退回纯 `created_at DESC` —— 宁可排序朴素，也不把用户输入拼进 SQL。
pub async fn contract_page(q: &ContractQuery) -> DbResult<(Vec<ContractRow>, i64)> {
    let mut sql =
        format!("SELECT {CONTRACT_LIST_COLUMNS} {CONTRACT_JOIN} WHERE c.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();

    if let Some(contract_no) = q.contract_no.as_deref() {
        sql.push_str(" AND c.contract_no LIKE ?");
        params.push(Value::from(format!("%{}%", contract_no.trim())));
    }
    if let Some(name) = q.name.as_deref() {
        sql.push_str(" AND c.contract_name LIKE ?");
        params.push(Value::from(format!("%{}%", name.trim())));
    }
    if let Some(contract_type) = q.contract_type.as_deref() {
        sql.push_str(" AND c.contract_type = ?");
        params.push(Value::from(contract_type.trim()));
    }
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND c.status = ?");
        params.push(Value::from(status.trim()));
    }
    if let Some(party_b_id) = q.party_b_id {
        sql.push_str(" AND c.party_b_id = ?");
        params.push(Value::from(party_b_id));
    }
    if let Some(from) = q.sign_from {
        sql.push_str(" AND c.sign_date >= ?::date");
        params.push(Value::from(from));
    }
    if let Some(to) = q.sign_to {
        sql.push_str(" AND c.sign_date <= ?::date");
        params.push(Value::from(to));
    }
    if let Some(from) = q.expire_from {
        sql.push_str(" AND c.end_date >= ?::date");
        params.push(Value::from(from));
    }
    if let Some(to) = q.expire_to {
        sql.push_str(" AND c.end_date <= ?::date");
        params.push(Value::from(to));
    }
    if let Some(keyword) = q.keyword.as_deref() {
        let pattern = format!("%{}%", keyword.trim());
        sql.push_str(" AND (c.contract_no LIKE ? OR c.contract_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }

    let order_by = q
        .contract_no
        .as_deref()
        .and_then(prefix_boost_order)
        .unwrap_or_else(|| CONTRACT_ORDER.to_string());

    let result = scope::paginate_unscoped(
        CONTRACT_TARGET,
        &sql,
        params,
        Some(&order_by),
        q.page,
        q.page_size,
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(ContractRow::from_row)
        .collect::<DbResult<Vec<ContractRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 前缀加权排序子句：输入只含 `[A-Za-z0-9_-]` 才生效（见 [`contract_page`] 文档）。
fn prefix_boost_order(raw: &str) -> Option<String> {
    let prefix = raw.trim();
    if prefix.is_empty() {
        return None;
    }
    if !prefix
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    // 注：进入本分支的字符串不含引号 / 通配符 / 分号（上面已逐个字符白名单），
    // 且长度上限由调用方（query string）间接约束，拼接是安全的。
    Some(format!(
        "ORDER BY CASE WHEN c.contract_no LIKE '{prefix}%' THEN 0 ELSE 1 END, \
         c.created_at DESC, c.id DESC"
    ))
}

/// 按主键取合同（软删行视为不存在 → `None`）。
pub async fn contract_find(id: Uuid) -> DbResult<Option<ContractRow>> {
    let sql = format!(
        "SELECT {CONTRACT_LIST_COLUMNS} {CONTRACT_JOIN} WHERE c.id = ?::uuid AND c.deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(ContractRow::from_row).transpose()
}

/// 锁读合同的**状态相关列**（`SELECT … FOR UPDATE`）。
///
/// 为什么不用 [`contract_find`]：带 `LEFT JOIN` 的查询**不能** `FOR UPDATE`
/// （PG 报 `FOR UPDATE cannot be applied to the nullable side of an outer join`），
/// 所以锁读走单表 + 最小投影；事务里需要的只有状态与日期。
///
/// 调用方必须在事务里调（否则锁随语句结束就释放，等于没锁）。
pub async fn contract_lock(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ContractStateRow>> {
    let row = ex
        .query_first_sql(
            "SELECT id, contract_no, status, sign_date, start_date, end_date \
               FROM biz_contract WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref()
        .map(|row| {
            Ok(ContractStateRow {
                id: row.get_typed("id")?,
                contract_no: row.get_typed("contract_no")?,
                status: row.get_typed("status")?,
                sign_date: row.get_typed("sign_date")?,
                start_date: row.get_typed("start_date")?,
                end_date: row.get_typed("end_date")?,
            })
        })
        .transpose()
}

/// 锁读整行（`PUT` 合并字段用：需要旧值才能算出「没传的字段保持原值」）。
///
/// 与 [`contract_lock`] 的差别只是投影宽窄；同样是单表 + `FOR UPDATE`。
pub async fn contract_lock_full(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ContractRow>> {
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {CONTRACT_BASE_COLUMNS}, NULL::text AS {PARTY_B_CURRENT_ALIAS} \
                   FROM biz_contract c WHERE c.id = ?::uuid AND c.deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(ContractRow::from_row).transpose()
}

/// `biz_seq(seq_type='CT')` 缺失时幂等补一行（正常库有 `V0195` 的种子）。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'CT', '合同号', 'HT', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '合同号：HT-yyyyMMdd-000001，按天（CST）滚动' \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'CT' AND deleted_at IS NULL)"
            .to_string(),
        vec![Value::from(Uuid::now_v7())],
    )
    .await
    .or_else(|err| match err {
        DbError::UniqueViolation { .. } => Ok(0),
        other => Err(other),
    })?;
    Ok(())
}

/// 推进 `biz_seq(seq_type='CT')` 一个号（三分支：同日累加 / 新日重置 / 时钟回拨不重置）。
async fn bump_seq(ex: &impl SqlExecutor, period_key: &str) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: 同上，biz_seq 是全局取号规则字典。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = 'CT' AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
            ],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("current_no"))
        .transpose()
}

/// 组装合同号（`HT-{周期键}-{6 位流水}`）。
pub fn format_contract_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// 取一个合同号（周期键按北京时间，与 PO / IN / TR 同款）。
pub async fn next_contract_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = crate::business::erp::serial_model::period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_contract_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='CT') 行不存在（period_key={period_key}）"
        ))),
    }
}

/// 插入一张合同（状态恒 `DRAFT`，单号由调用方先取好）。
///
/// # 参数
///
/// * `id` —— 先由 handler 生成（`Uuid::now_v7()`），便于插入后无需 `RETURNING`
/// * `contract_no` —— [`next_contract_no`] 的返回值（同一事务里取）
pub async fn contract_insert(
    ex: &impl SqlExecutor,
    id: Uuid,
    contract_no: &str,
    new: &NewContract,
) -> DbResult<u64> {
    ex.exec_sql(
        "INSERT INTO biz_contract \
            (id, contract_no, contract_name, contract_type, party_a_id, party_a_name, \
             party_b_type, party_b_id, party_b_name, amount, currency_code, \
             sign_date, start_date, end_date, status, owner_id, dept_id, remark, \
             created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?, NULLIF(?::text,'')::uuid, ?, ?, ?::uuid, ?, NULLIF(?::text,'')::numeric, ?, \
                 NULLIF(?::text,'')::date, NULLIF(?::text,'')::date, NULLIF(?::text,'')::date, \
                 ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, ?, now(), now(), \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(contract_no.to_string()),
            Value::from(new.contract_name.clone()),
            Value::from(new.contract_type.clone()),
            uuid_text(new.party_a_id),
            Value::from(new.party_a_name.clone()),
            Value::from(new.party_b_type.clone()),
            Value::from(&new.party_b_id),
            Value::from(new.party_b_name.clone()),
            Value::from(new.amount.to_string()),
            Value::from(new.currency_code.clone()),
            date_text(new.sign_date),
            date_text(new.start_date),
            date_text(new.end_date),
            Value::from(STATUS_DRAFT),
            uuid_text(new.owner_id),
            uuid_text(new.dept_id),
            Value::from(new.remark.clone()),
            uuid_text(new.operator),
            uuid_text(new.operator),
        ],
    )
    .await
}

/// 覆盖写可改列（`WHERE status = 'DRAFT'` 是第二道闸，防「读了 DRAFT、写时已流转」）。
///
/// # 返回值
///
/// 受影响行数：`0` = 期间状态已不是 `DRAFT`（或行被删），调用方应报冲突。
pub async fn contract_apply(
    ex: &impl SqlExecutor,
    id: Uuid,
    mut_row: &ContractMut,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_contract \
            SET contract_name = ?, party_b_id = ?::uuid, party_b_name = ?, \
                amount = NULLIF(?::text,'')::numeric, \
                sign_date = NULLIF(?::text,'')::date, start_date = NULLIF(?::text,'')::date, \
                end_date = NULLIF(?::text,'')::date, remark = ?, updated_at = now(), \
                update_by = NULLIF(?::text,'')::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'DRAFT'"
            .to_string(),
        vec![
            Value::from(mut_row.contract_name.clone()),
            Value::from(&mut_row.party_b_id),
            Value::from(mut_row.party_b_name.clone()),
            Value::from(mut_row.amount.to_string()),
            date_text(mut_row.sign_date),
            date_text(mut_row.start_date),
            date_text(mut_row.end_date),
            Value::from(mut_row.remark.clone()),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 写状态（**只由 `contract_handler::change_status` 在状态机过闸之后调用**，
/// 且带 `status = <旧状态>` 的 CAS 条件，防并发双写覆盖）。
///
/// ⚠️ 卡面写「更新 `status` + `status_changed_at`」，但 `V0195` 的 23 列里
/// **没有 `status_changed_at`**（§2.1 明确不加「状态时间」冗余列）。因此这里只写
/// `status` + `updated_at`；真正的「什么时候变成这个状态」以 `biz_status_log.created_at`
/// 为准（`transition()` 在同一事务里写的，不会缺）。已写进 A2 交付说明的顺带报告。
pub async fn contract_write_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_contract SET status = ?, updated_at = now(), \
                update_by = NULLIF(?::text,'')::uuid \
          WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::from(to.to_string()),
            uuid_text(operator),
            Value::from(&id),
            Value::from(from.to_string()),
        ],
    )
    .await
}

/// 软删（写 `deleted_at`）。
///
/// # 返回值
///
/// 受影响行数：`0` = 不存在 / 已删。
pub async fn contract_soft_delete(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_contract SET deleted_at = now(), updated_at = now(), \
                update_by = NULLIF(?::text,'')::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![uuid_text(operator), Value::from(&id)],
    )
    .await
}

/// `biz_purchase_order` 是否已经有 `contract_id` 列（A3 之前没有）。
///
/// 这是**防御式实现**：A2 落地时 A3（采购单挂合同）还没做，直接查 `contract_id`
/// 会让删除接口在 A3 之前**整个不可用**（列不存在 → 500）。A3 建好列后本函数返回 `true`，
/// 引用守卫自动生效，不需要回来改代码。
pub async fn contract_reference_column_exists(ex: &impl SqlExecutor) -> DbResult<bool> {
    let count: i64 = ex
        .scalar_sql(
            "SELECT count(*) FROM information_schema.columns \
              WHERE table_schema = 'public' AND table_name = 'biz_purchase_order' \
                AND column_name = 'contract_id'"
                .to_string(),
            vec![],
        )
        .await?;
    Ok(count > 0)
}

/// 引用该合同的采购单号（**未软删**的）。
///
/// # 返回值
///
/// * `Ok(None)` —— `biz_purchase_order` 还没有 `contract_id` 列（A3 未落地），守卫跳过
/// * `Ok(Some(vec![]))` —— 列在、无引用
/// * `Ok(Some(order_nos))` —— 有引用（最多 20 个，够用户处理；超出会在文案里提示）
pub async fn contract_referencing_orders(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> DbResult<Option<Vec<String>>> {
    if !contract_reference_column_exists(ex).await? {
        return Ok(None);
    }
    // SCOPE-EXEMPT: 按合同主键精确查引用单据，不是列表查询，不涉及数据范围。
    let rows = ex
        .query_sql(
            "SELECT order_no FROM biz_purchase_order \
              WHERE contract_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY order_no LIMIT 20"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    let mut numbers = Vec::with_capacity(rows.len());
    for row in &rows {
        numbers.push(row.get_typed::<String>("order_no")?);
    }
    Ok(Some(numbers))
}

/// `Option<Uuid>` → 参数（空绑定空串，SQL 侧 `NULLIF(?::text,'')::uuid` 转 NULL）。
fn uuid_text(value: Option<Uuid>) -> Value {
    Value::from(value.map(|id| id.to_string()))
}

/// `Option<NaiveDate>` → 参数（同上，SQL 侧 `NULLIF(?::text,'')::date`）。
fn date_text(value: Option<NaiveDate>) -> Value {
    Value::from(value.map(|date| date.format("%Y-%m-%d").to_string()))
}

/// 合同状态机的边是否与注册表一致（**配置漂移的运行时守卫**，也被单测直接调用）。
///
/// `transition()` 自己也会核对（不一致 500）；这里给出 `bool` 是为了让
/// `contract_handler` 的单测能在**不连库**的情况下把「A1 改了注册表、A2 没跟上」钉死。
pub fn legal_edges_match_registry() -> bool {
    state_machine::Entity::Contract.edges() == &LEGAL_EDGES[..]
}

// ==================== 单测（真库用例需 ERP_DB_URL，未设置则跳过） ====================

#[cfg(test)]
mod tests {
    use super::*;

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 本模块造的主数据编码前缀（清理用）。
    const PREFIX: &str = "A1CM-";

    /// 初始化全局 `db()`（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 物理删掉本模块造的主数据（真库用例外不留痕）。
    async fn cleanup() {
        db().execute_with_params(
            "DELETE FROM biz_supplier WHERE supplier_code LIKE 'A1CM-%'",
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("清理 biz_supplier 夹具失败：{err}"));
    }

    /// 造一个供应商，返回 (id, name)。`status` 用来验「停用」分支。
    async fn make_supplier(tag: &str, status: i16) -> (Uuid, String) {
        let id = Uuid::now_v7();
        let name = format!("A1 合同乙方 {tag}");
        let code = format!("{PREFIX}{tag}-{}", &id.simple().to_string()[..8]);
        db().execute_with_params(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) \
             VALUES (?::uuid, ?, ?, ?)",
            vec![
                Value::from(&id),
                Value::Text(code),
                Value::Text(name.clone()),
                Value::I16(status),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("造供应商失败：{err}"));
        (id, name)
    }

    /// ① 存在且未软删 → 返回主档名称（V-15 的快照来源）。
    #[tokio::test]
    async fn party_b_ref_returns_supplier_name() {
        let Some(_guard) = setup().await else { return };
        let (id, name) = make_supplier("OK", 1).await;

        let got = party_b_ref(PARTY_B_SUPPLIER, id).await.unwrap();
        assert_eq!(got.as_deref(), Some(name.as_str()), "应返回主档当前名称");
    }

    /// ② 不存在的 id → None（应用层据此给 66021）。
    #[tokio::test]
    async fn party_b_ref_unknown_id_is_none() {
        let Some(_guard) = setup().await else { return };
        // 随机 id 保证不会撞上任何真实主档
        let ghost = Uuid::now_v7();
        assert_eq!(
            party_b_ref(PARTY_B_SUPPLIER, ghost).await.unwrap(),
            None,
            "不存在的乙方必须返回 None，不能静默放过"
        );
    }

    /// ③ 软删的主档 → None（巡检 Q1 的病根在这条之后：软删是允许的，合同会变孤儿）。
    #[tokio::test]
    async fn party_b_ref_soft_deleted_is_none() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = make_supplier("DEL", 1).await;
        db().execute_with_params(
            "UPDATE biz_supplier SET deleted_at = now() WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();

        assert_eq!(
            party_b_ref(PARTY_B_SUPPLIER, id).await.unwrap(),
            None,
            "已软删的主档不得作为新合同乙方"
        );
    }

    /// ④ CUSTOMER 分支本期不开：即使 id 存在也返回 None（§3.2.3 的硬拒口径）。
    ///
    /// 这条断言同时是「批次 2 的提醒」：R2 落地后，本用例要改成
    /// 「造一个 biz_customer 行 → 返回 customer_name」。
    #[tokio::test]
    async fn party_b_ref_customer_branch_is_closed() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = make_supplier("CUS", 1).await;
        assert_eq!(
            party_b_ref(PARTY_B_CUSTOMER, id).await.unwrap(),
            None,
            "客户主数据未上线，CUSTOMER 分支必须返回 None（调用方转 66020）"
        );
        assert_eq!(
            party_b_ref("UNKNOWN", id).await.unwrap(),
            None,
            "未知乙方类型也不能静默放过"
        );
    }

    /// ⑤ 新建口径更严：停用的供应商在 `party_b_ref_for_new` 里拿不到，但通用解析器仍能
    /// 取到名字 —— 这个**差异本身就是 V-11 的语义**（停用只拦新签，不拦改备注）。
    #[tokio::test]
    async fn party_b_ref_for_new_rejects_disabled_supplier() {
        let Some(_guard) = setup().await else { return };
        let (disabled_id, disabled_name) = make_supplier("OFF", 0).await;
        let (enabled_id, enabled_name) = make_supplier("ON", 1).await;

        // 通用解析器不看 status：停用供应商的行仍在库里，改其它字段时要能取到名字
        assert_eq!(
            party_b_ref(PARTY_B_SUPPLIER, disabled_id)
                .await
                .unwrap()
                .as_deref(),
            Some(disabled_name.as_str()),
            "通用解析器不按 status 过滤（否则供应商一停用，连合同备注都改不了，V-11 末段）"
        );
        // 新签闸口才看 status
        assert_eq!(
            party_b_ref_for_new(PARTY_B_SUPPLIER, disabled_id)
                .await
                .unwrap(),
            None,
            "停用供应商不得新签（V-11）"
        );
        assert_eq!(
            party_b_ref_for_new(PARTY_B_SUPPLIER, enabled_id)
                .await
                .unwrap()
                .as_deref(),
            Some(enabled_name.as_str()),
            "启用中的供应商可以新签"
        );
        // 客户分支在新签闸口同样关闭（不查表，直接 None）
        assert_eq!(
            party_b_ref_for_new(PARTY_B_CUSTOMER, enabled_id)
                .await
                .unwrap(),
            None,
            "CUSTOMER 本期不开（66020）"
        );
    }
}
