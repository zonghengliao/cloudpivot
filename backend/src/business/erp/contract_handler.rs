//! 合同接口层（**A2**）：6 个接口，路径与权限注解由 A1 挂好，本卡只填函数体。
//!
//! | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/contract/page` | `business:erp:contract:list` | 分页（软删行不返回） |
//! | GET | `/business/erp/contract/{id}` | `business:erp:contract:list` | 详情（含 `can_edit` / `usable_for_order` / `next_statuses`） |
//! | POST | `/business/erp/contract` | `business:erp:contract:create` | 建单（`DRAFT` + `HT-yyyyMMdd-000001`） |
//! | PUT | `/business/erp/contract/{id}` | `business:erp:contract:update` | 改单（**仅 `DRAFT`**） |
//! | DELETE | `/business/erp/contract/{id}` | `business:erp:contract:delete` | 软删（引用守卫） |
//! | PATCH | `/business/erp/contract/{id}/status` | `business:erp:contract:status` | 状态流转（状态机唯一入口） |
//!
//! # 权限码前缀（P2-4 已统一，历史沿革别再改回去）
//!
//! 合同域最初按 `ARCH-2 附录 B #7` 逐字用了 `erp:contract:*`，与 ERP 其余 313 个
//! `business:erp:*` 码前缀不一致（前端判权要记两套规则）。P2-4 统一为
//! `business:erp:contract:*`：
//!
//! - `V0241` 把 `sys_menu.auth_code` **就地改名**（菜单 id 不动 ⇒ 已配的 `sys_role_menu`
//!   一行不丢）；
//! - 旧码 `erp:contract:*` 在兼容期内由 `shared::permission_alias` 装载时互扩，
//!   老令牌 / 前端老包继续可用（详见该模块文档）。
//!
//! # 错误码（`shared/error_code`，卡面的 66013 被 ERP-B2 占用后整体顺延）
//!
//! | 场景 | 码 | HTTP |
//! |---|---|---|
//! | 合同不存在 / 已软删 | `66014 ERP_CONTRACT_NOT_USABLE` | 404 |
//! | 非法流转边 | `66015 ERP_CONTRACT_STATUS_ILLEGAL` | 409 |
//! | 非草稿改单（字段冻结） | `66016 ERP_CONTRACT_FIELD_LOCKED` | 409 |
//! | 合同号重复 | `66017 ERP_CONTRACT_NO_DUPLICATED` | 409 |
//! | 被采购单引用不可删 | `66018 ERP_CONTRACT_IN_USE` | 409 |
//! | 客户类合同未开放 | `66020 ERP_CONTRACT_CUSTOMER_UNSUPPORTED` | 400 |（**SO-26 起废弃**：
//!   `SALES` 已放行，乙方走 `biz_customer`；码值保留不删，避免历史日志 / 前端映射报未知码）
//! | 乙方不存在 / 已停用 | `66021 ERP_CONTRACT_PARTY_INVALID` | 400 |
//!
//! 入参形态问题（必填缺失 / 超长 / 非法枚举 / 金额非正 / 日期反了）一律
//! `400 + 10001 PARAM_INVALID`：**业务码只表达业务语义**，别把「name 没填」也塞进 66014。
//!
//! # 三条硬约束（写在这里免得后人"顺手优化"）
//!
//! ① **状态只能走状态机**：`PATCH /status` 的边判定取 `Entity::Contract`（注册表是唯一真相），
//!    写状态的是 `state_machine::transition`（它自己写 `biz_status_log`）。
//!    handler 里**没有** `match (from, to) { … }` 这种硬判——有的话加一条边就要改两处。
//! ② **单号只用于显示 / 模糊查**（R1-f）：任何关联都靠 `party_b_id`，不靠 `contract_no` 文本。
//! ③ **乙方名称是快照**：新建 / 换乙方时由服务端按 `party_b_type` 从 `biz_supplier`（供应商）
//!    或 `biz_customer`（客户）读，客户端传什么都不算数。
//!
//! # 与卡面的出入（都以 DDL 为准，逐条已写进 A2 交付说明）
//!
//! * 卡面「非 DRAFT → 400 + 66014」→ 实际 **409 + 66016**（`66014` 是「不存在/已删」，
//!   改单被拒是「字段冻结」，用 `FIELD_LOCKED` 才对；`ARCH-2 §5.3` 的字段级冻结矩阵
//!   本期未实现，按卡面「只有 DRAFT 可改」执行，已报告）；
//! * 卡面「非法边 → 400 + 66014」→ 实际 **409 + 66015**；
//! * 卡面「状态流转更新 `status_changed_at`」→ DDL 无此列，用 `updated_at`，
//!   状态变更时刻以 `biz_status_log.created_at` 为准；
//! * 卡面「删除被引用 → 400 + 66018」→ 实际 **409**（冲突语义），并在文案里带出采购单号。

use axum::Json;
use axum::extract::{ConnectInfo, Path, Query};
use axum::http::HeaderMap;
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::changelog::{self, Action as ChangeAction, ChangeTarget, DiffInput};
use crate::business::erp::approval_gate;
use crate::business::erp::contract_bridge;
use crate::business::erp::contract_dto::{
    ContractDetailRes, ContractFilter, ContractRes, ContractStatusReq, CreateContractReq, UpdateContractReq,
    ensure_party_b_type, normalize_page, parse_optional_uuid, parse_uuid_text,
};
use crate::business::erp::contract_linkage;
use crate::business::erp::contract_model::{self, ContractMut, STATUS_DRAFT};
use crate::business::flow::{engine, model as flow_model};
use crate::business::shared::state_machine;
use crate::business::shared::user_name;
use crate::shared::error_code;
use crate::utils;

/// 合同列表 / 详情（`sys_menu` 里是 menu_type=2 的菜单节点）。
pub const PERM_LIST: &str = "business:erp:contract:list";
/// 新建合同。
pub const PERM_CREATE: &str = "business:erp:contract:create";
/// 编辑合同（A2 按 §5.3 冻结矩阵校验）。
pub const PERM_UPDATE: &str = "business:erp:contract:update";
/// 删除合同（软删，V-21 引用守卫）。
pub const PERM_DELETE: &str = "business:erp:contract:delete";
/// 状态流转（`state_machine::transition(Entity::Contract, …)` 的唯一入口）。
pub const PERM_STATUS: &str = "business:erp:contract:status";

/// 合同域全部权限码（顺序与 `V0197` 的 `auth_code` 一致，也是「权限码唯一真相」）。
///
/// 与迁移**逐字对应**：`V0241__erp_contract_permission_prefix_unify.up.sql` 改名后的 5 条
/// `sys_menu` 行就是这个数组。任一侧改动而另一侧没跟，`contract_permissions_match_menu`
/// 用例会红。
pub const PERMISSIONS: [&str; 5] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_DELETE, PERM_STATUS];

/// 合同域历史权限码（P2-4 前缀统一前的 `erp:contract:*`）。
///
/// **只在兼容期存在**：库里已经没有这些码（`V0241` 已改名），但老令牌 / 前端老包 /
/// 第三方脚本可能还在送，因此 `shared::permission_alias` 把它们与 [`PERMISSIONS`]
/// 双向互扩。删兼容层时连同 `LEGACY_ALIASES` 一起删。
pub const LEGACY_PERMISSIONS: [&str; 5] = [
    "erp:contract:list",
    "erp:contract:create",
    "erp:contract:update",
    "erp:contract:delete",
    "erp:contract:status",
];

/// 权限码前缀：`{模块}:{资源}`，动作段由各常量尾段给出。
///
/// P2-4 统一到 `business:erp:contract`（四段式，与 ERP 其余码同族）；统一前是
/// `erp:contract`（三段式），历史原因见文件头。
pub const PERMISSION_PREFIX: &str = "business:erp:contract";

// ==================== 错误构造 ====================

/// 合同不存在 / 已软删（404 + `66014`）。
fn not_usable(detail: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::ERP_CONTRACT_NOT_USABLE,
        format!("合同不存在或已删除：{detail}"),
    )
}

/// 非法流转边（409 + `66015`），文案由状态机生成（含「当前态允许去哪」）。
fn illegal_transition(message: String) -> ApiError {
    ApiError::new(409, error_code::ERP_CONTRACT_STATUS_ILLEGAL, message)
}

/// 通用冲突（409 + `RESOURCE_CONFLICT`）：重复提交审批 / 并发改动。
fn conflict(message: String) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message)
}

/// 非草稿改单（409 + `66016`）。
fn field_locked(status: &str, label: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_CONTRACT_FIELD_LOCKED,
        format!(
            "合同当前是「{status}」（{label}）：只有草稿（DRAFT）可以修改。\
             已签合同的关键字段要改，只能作废（软删）后重签，状态流转走 PATCH /status"
        ),
    )
}

/// 合同号重复（409 + `66017`）。
fn duplicated_no(contract_no: Option<&str>) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_CONTRACT_NO_DUPLICATED,
        match contract_no {
            Some(no) => format!("合同号 {no} 已存在（未删除的合同号唯一）"),
            None => "合同号重复：未删除的合同号唯一（uk_biz_contract_no）".to_string(),
        },
    )
}

/// 被采购单引用不可删（409 + `66018`），**把引用它的单号带出来**方便用户处理。
fn in_use(contract_no: &str, order_nos: &[String]) -> ApiError {
    let listed = order_nos
        .iter()
        .map(|no| format!("「{no}」"))
        .collect::<Vec<_>>()
        .join(" / ");
    ApiError::new(
        409,
        error_code::ERP_CONTRACT_IN_USE,
        format!(
            "合同 {contract_no} 已被采购单 {listed} 引用，不能删除：\
             请先作废或改挂这些采购单，或改用「终止」流转（TERMINATED）而不是删除"
        ),
    )
}

/// 引用守卫的**纯判定**（无 DB，单测直接钉）：有引用 → `66018`，无引用 → `Ok(())`。
///
/// `None` = `biz_purchase_order` 还没有 `contract_id` 列（A3 未落地）→ 跳过检查。
fn reference_guard(contract_no: &str, referencing: Option<&[String]>) -> std::result::Result<(), ApiError> {
    match referencing {
        Some([]) | None => Ok(()),
        Some(order_nos) => Err(in_use(contract_no, order_nos)),
    }
}

/// 乙方不存在 / 已删除（400 + `66021`，改单换乙方用：不查业务状态，V-11）。
fn party_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_CONTRACT_PARTY_INVALID, message.into())
}

/// 乙方主档在 UI / 日志里的称谓（按类型分叉，别让销售合同的报错说「供应商」）。
fn party_label(party_b_type: &str) -> &'static str {
    if party_b_type == contract_model::PARTY_B_CUSTOMER {
        "乙方客户"
    } else {
        "乙方供应商"
    }
}

/// 新签时乙方不可用（400 + `66021`）：文案按类型说清「停用」还是「冻结」。
fn party_missing_for_new(party_b_type: &str, party_b_id: Uuid) -> ApiError {
    let label = party_label(party_b_type);
    let state = if party_b_type == contract_model::PARTY_B_CUSTOMER {
        "已冻结（V-11：冻结客户不得新签合同）"
    } else {
        "已停用（V-11：停用供应商不得新签合同）"
    };
    party_invalid(format!("{label} {party_b_id} 不存在、已删除或{state}"))
}

/// 写路径的数据库错误收口（与 `handler::write_error` 同款判据：按变体，不按文案）。
///
/// * `UniqueViolation` → 409 + `66017`（表上只有 `uk_biz_contract_no` 一个唯一索引）
/// * `CheckViolation` → 400 + `10001`（DDL 的 CHECK 兜住了应用层没拦到的组合）
/// * 其余 → 500（细节只进日志）
fn write_error(error: DbError, ctx: &str) -> ApiError {
    match error {
        DbError::UniqueViolation { .. } => duplicated_no(None),
        DbError::CheckViolation { .. } => {
            tracing::warn!(context = ctx, error = %error, "合同写库被 DDL CHECK 拦住");
            ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("{ctx} 被数据库约束拒绝：字段组合不合法（amount>0 / 日期顺序 / 状态白名单）"),
            )
        }
        other => utils::db_error(other, ctx),
    }
}

/// 当前操作人（`biz_contract.create_by` / `update_by` / 状态日志的 operator）。
fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

/// 回读详情（写接口的返回体统一走它，保证「返回的就是现在库里的」）。
async fn detail_of(id: Uuid) -> ResResult<ContractDetailRes> {
    let row = contract_model::contract_find(id)
        .await
        .map_err(|error| utils::db_error(error, "回读合同"))?
        .ok_or_else(|| not_usable(&format!("id={id}")))?;
    let mut detail = ContractDetailRes::from(&row);
    fill_approval_no(std::slice::from_mut(&mut detail.base)).await;
    // CTR-02 F6 / F3：履约口径**现算**（已执行金额 / 剩余额度 / 未关闭下游单据）。
    // 与下单额度守卫同一函数，前端「履约进度」与「还能不能下单」不会两个口径。
    // 查库失败直接 500：宁可报错，也不给一个「已执行 0.00」的假数字。
    let commitment = contract_model::commitment_of(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "现算合同履约口径"))?;
    detail.apply_commitment(&commitment);
    // AUD-02：负责人 / 制单人 / 修改人补姓名（批量入口，恒 1 条 SQL；失败只降级为空）
    user_name::fill_person_names(std::slice::from_mut(&mut detail))
        .await
        .unwrap_or_default();
    Ok(Res::ok(detail))
}

/// 回填审批实例单号（`approval_instance_no`）：一次查完整页的实例号，逐行按 id 对上。
///
/// 为什么要它：列表 / 详情要显示「审批中（FLW-xxx）」—— 用户拿着单号能直接去审批中心
/// 找到它；而 `ContractRes::from(&ContractRow)` 只拿得到实例 **id**。
/// 批量查一次而不是逐行查（一页 20 行逐行查就是 20 次往返）。
// SCOPE-EXEMPT: 只在**已授权的行集合**上按 id 批量补展示字段（SELECT id, instance_no FROM biz_flow_instance WHERE id = ANY(...)），不产生行集、也不决定谁能看；行集权限在 business/erp/contract_model.rs:496（contract_page → scope::paginate_unscoped）+ 中间件 business:erp:contract:list（contract_handler.rs:303）。
async fn fill_approval_no(rows: &mut [ContractRes]) {
    let ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|row| row.approval_instance_id.as_deref())
        .filter_map(|raw| Uuid::parse_str(raw).ok())
        .collect();
    if ids.is_empty() {
        return;
    }
    let joined = ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
    let found = db()
        .find_with_params(
            "SELECT id, instance_no FROM biz_flow_instance \
              WHERE id = ANY(?::text::uuid[])",
            vec![Value::Text(format!("{{{joined}}}"))],
        )
        .await
        .unwrap_or_default();
    let map: std::collections::HashMap<String, String> = found
        .iter()
        .filter_map(|row| {
            let id: Uuid = row.get_typed("id").ok()?;
            let no: String = row.get_typed("instance_no").ok()?;
            Some((id.to_string(), no))
        })
        .collect();
    for row in rows.iter_mut() {
        if let Some(id) = row.approval_instance_id.as_deref()
            && let Some(no) = map.get(id)
        {
            row.approval_instance_no = Some(no.clone());
        }
    }
}

// ==================== ① 分页 ====================

/// 合同分页（软删行不返回，默认 `created_at DESC`）。
///
/// # 查询参数
///
/// `page` / `page_size`（默认 1 / 20，**上限 100**）、`contract_no`（模糊，前缀命中优先排序）、
/// `name`（= `contract_name`，模糊）、`contract_type`、`status`、`party_b_id`、
/// `sign_date_from` / `sign_date_to`、`expire_date_from` / `expire_date_to`（落 `end_date`）、
/// `keyword`（合同号**或**名称任一命中）。
///
/// # 异常情况
///
/// - 400 + `10001`：日期格式非法 / 区间反了 / 枚举不在白名单 / `party_b_id` 不是 UUID；
/// - 403：无 `business:erp:contract:list`；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/erp/contract/page",
    method = "GET",
    permission = "business:erp:contract:list"
)]
#[nik::get("/business/erp/contract/page")]
pub async fn contract_page(Query(filter): Query<ContractFilter>) -> ResResult<PageData<Vec<ContractRes>>> {
    let query = filter.parse()?;
    let (page, page_size) = normalize_page(query.page, query.page_size);
    let (rows, total) = contract_model::contract_page(&query)
        .await
        .map_err(|error| utils::db_error(error, "分页查询合同"))?;
    let mut list: Vec<ContractRes> = rows.iter().map(ContractRes::from).collect();
    fill_approval_no(&mut list).await;
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

// ==================== ② 详情 ====================

/// 合同详情（列表字段全集 + 甲方 / 负责人 / 部门 / 备注 / 审计四件）。
///
/// 额外给三个**派生字段**（省得前端 / A3 自己判状态）：
/// `can_edit`（= `DRAFT`）、`usable_for_order`（= `SIGNED` / `ACTIVE`）、
/// `next_statuses`（状态机给的合法去向）。
///
/// # 异常情况
///
/// - 400 + `10001`：`id` 不是合法 UUID；
/// - 404 + `66014`：不存在或已软删；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/erp/contract/{id}",
    method = "GET",
    permission = "business:erp:contract:list"
)]
#[nik::get("/business/erp/contract/{id}")]
pub async fn contract_detail(Path(id): Path<String>) -> ResResult<ContractDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    detail_of(id).await
}

// ==================== ③ 新建 ====================

/// 新建合同（状态恒 `DRAFT`，单号 `HT-yyyyMMdd-000001`，乙方名称由服务端快照）。
///
/// # 处理链
///
/// 1. 纯入参校验（必填 / 长度 / 枚举 / **类型闸**（`PURCHASE`/`SALES`/`SERVICE`/`LEASE`
///    四类全开，见 `contract_dto::SUPPORTED_TYPES`）/ 金额 > 0 / 日期顺序 / 签订 ≤ 生效）；
/// 2. 乙方类型：`SALES` 推导为 `CUSTOMER`（`party_b_id → biz_customer.id`），其余
///    （含 `SERVICE` / `LEASE`）为 `SUPPLIER`；显式值与推导冲突 → 400
///    （`contract_dto::ensure_party_b_type`）；
/// 3. 乙方解析：`party_b_ref_for_new`（供应商 `status = 1`、客户 `status = 'ACTIVE'`）
///    → 无 → `66021`；
/// 4. 同一事务里取号 + 插入（取号与插入必须同事务，否则回滚会吃掉一个号，
///    虽然号码不要求连续，但少一个号会让对账人多问一句）。
///
/// # 异常情况
///
/// - 400 + `10001`：入参不合法 / 类型不在放行集 / 乙方类型与合同类型冲突；
///   400 + `66021`：乙方不存在 / 已删 / 停用或冻结；
/// - 409 + `66017`：合同号重复（唯一索引兜底）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/erp/contract",
    method = "POST",
    permission = "business:erp:contract:create"
)]
#[nik::post("/business/erp/contract")]
pub async fn contract_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateContractReq>,
) -> ResResult<ContractDetailRes> {
    // 含类型闸：四类全开（CTR-02-F1），闸保留给未来新枚举
    req.validate()?;

    let party_b_type = req.resolve_party_b_type()?;
    let party_b_id = parse_uuid_text(req.party_b_id.as_deref().unwrap_or_default(), "party_b_id")?;
    // V-11：新建要求乙方主档存在、未软删、且可开新单（停用/冻结只拦新签，不拦改备注）
    let party_b_name = contract_model::party_b_ref_for_new(party_b_type, party_b_id)
        .await
        .map_err(|error| utils::db_error(error, "校验合同乙方"))?
        .ok_or_else(|| party_missing_for_new(party_b_type, party_b_id))?;

    let operator = operator_of(&claims);
    let mut new = req.into_new(party_b_id, party_b_name, party_b_type, operator);
    // 甲方 / 负责人 / 部门三个可选弱引用（解析失败一律 400，不静默丢）
    new.party_a_id = parse_optional_uuid(req.party_a_id.as_deref(), "party_a_id")?;
    new.owner_id = parse_optional_uuid(req.owner_id.as_deref(), "owner_id")?;
    new.dept_id = parse_optional_uuid(req.dept_id.as_deref(), "dept_id")?;

    let id = Uuid::now_v7();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启新建合同事务"))?;
    let contract_no = match contract_model::next_contract_no(&tx, Utc::now()).await {
        Ok(no) => no,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "合同取号"));
        }
    };
    if let Err(error) = contract_model::contract_insert(&tx, id, &contract_no, &new).await {
        let _ = tx.rollback().await;
        return Err(write_error(error, "新建合同"));
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交新建合同事务"))?;

    detail_of(id).await
}

// ==================== ④ 修改 ====================

/// 修改合同（**只有 `DRAFT` 可改**；状态流转不走这里）。
///
/// 可改列：名称 / 乙方 / 金额 / 三个日期 / 备注。`contract_no` / `status` / `create_by` /
/// `created_at` 不在 `UpdateContractReq` 里，客户端传了也被 serde 丢掉。
///
/// # 变更留痕（CTR-02 F2）
///
/// 「草稿改单零留痕」是 CTR-01 演练发现的实际审计缺口（改前值永久丢失）。现在每次
/// `PUT` 在**同一个事务里**写一条 `sys_change_log`（`resource = biz_contract`，
/// `action = UPDATE`）：
///
/// * diff 输入 = 锁行读到的**旧值**（`contract_model::diff_snapshot_before`）与
///   合并后的**新值**（`diff_snapshot_after`），键与列名逐字一致；
/// * 值已规范化（金额固定 4 位小数、日期 `YYYY-MM-DD`），所以「金额 5 万原样回传」
///   不会写出一条假变更；值确实没变时 `record_in` 返回 `Skipped`，**不写空日志**；
/// * 日志与业务**同生共死**：写日志失败 → 整笔回滚（不允许「合同改了但审计没记」）。
///
/// # 并发
///
/// 事务内 `SELECT … FOR UPDATE` 锁行 → 合并 → 校验 → 带 `WHERE status='DRAFT'` 整写，
/// 第二道闸防「读到 DRAFT 之后被别人流转掉」。
///
/// # 异常情况
///
/// - 400 + `10001`：入参不合法 / 合并后的日期区间反了；400 + `66021`：换乙方但乙方无效；
/// - 404 + `66014`：不存在或已删；409 + `66016`：非 `DRAFT`；
/// - 500：数据库异常 / 变更日志写不进去（含 `62012` 未登记白名单，属接入缺陷）。
#[nik::permission(
    path = "/business/erp/contract/{id}",
    method = "PUT",
    permission = "business:erp:contract:update"
)]
#[nik::put("/business/erp/contract/{id}")]
pub async fn contract_update(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<std::net::SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateContractReq>,
) -> ResResult<ContractDetailRes> {
    req.validate()?;
    let id = parse_uuid_text(&id, "id")?;
    let operator = operator_of(&claims);

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启修改合同事务"))?;
    let current = match contract_model::contract_lock_full(&tx, id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_usable(&format!("id={id}")));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定合同"));
        }
    };
    if current.status != STATUS_DRAFT {
        let label = contract_model::status_label(&current.status);
        let _ = tx.rollback().await;
        return Err(field_locked(&current.status, label));
    }

    // CTR-02-F1：乙方类型必须与（已签冻结的）合同类型配对 —— 服务 / 租赁固定供应商。
    // 改单不解冻 `contract_type`（V-19），所以用**库里的** `contract_type` 判定；
    // 客户端显式传了 `party_b_type` 但值不对 → 400（不静默忽略）。
    if let Some(raw) = req
        .party_b_type
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        && let Err(error) = ensure_party_b_type(&current.contract_type, raw)
    {
        let _ = tx.rollback().await;
        return Err(error);
    }

    // ---- 合并成「最终值」再校验：只改 end_date 也可能把区间改反 ----
    let contract_name = match req.display_name() {
        Some(name) => name.trim().to_string(),
        None => current.contract_name.clone(),
    };
    let amount = req.amount.unwrap_or(current.amount);
    let sign_date = merge_option(req.sign_date, current.sign_date);
    let start_date = merge_option(req.start_date, current.start_date);
    let end_date = merge_option(req.end_date, current.end_date);
    let remark = merge_option(req.remark, current.remark.clone());

    // 乙方：换了就重新解析（快照跟着更新）；没换就保留原快照，
    // 也**不重新校验**其存在性（V-11：停用只拦新签，不拦改其它字段）
    let (party_b_id, party_b_name) = match req.party_b_id.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        None => (current.party_b_id, current.party_b_name.clone()),
        Some(raw) => {
            let new_id = parse_uuid_text(raw, "party_b_id")?;
            if new_id == current.party_b_id {
                (current.party_b_id, current.party_b_name.clone())
            } else {
                let name = contract_model::party_b_ref(&current.party_b_type, new_id)
                    .await
                    .map_err(|error| utils::db_error(error, "校验合同乙方"))?
                    .ok_or_else(|| {
                        party_invalid(format!(
                            "{} {new_id} 不存在或已删除",
                            party_label(&current.party_b_type)
                        ))
                    })?;
                (new_id, name)
            }
        }
    };

    // 合并后整体复校验（与新建同一套规则，避免「改单把合同改成非法状态」）
    let merged = ContractMut {
        contract_name,
        party_b_id,
        party_b_name,
        amount,
        sign_date,
        start_date,
        end_date,
        remark,
    };
    if let Err(error) = validate_mutation(&merged) {
        let _ = tx.rollback().await;
        return Err(error);
    }

    // CTR-02 F2：改前 / 改后快照（键 = 列名，金额/日期已规范化 —— 见
    // `contract_model::ContractDiffFields`；原样回传未变的金额不会写出假变更）
    let before_snapshot = contract_model::diff_snapshot_before(&current);
    let after_snapshot = contract_model::diff_snapshot_after(&merged);
    let change_ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));

    let affected = match contract_model::contract_apply(&tx, id, &merged, operator).await {
        Ok(rows) => rows,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "修改合同"));
        }
    };
    if affected == 0 {
        // 读了 DRAFT、写时已不是（并发流转 / 并发删除）
        let _ = tx.rollback().await;
        return Err(ApiError::new(
            409,
            error_code::ERP_CONTRACT_STATUS_ILLEGAL,
            "合同在本次修改期间状态已变化（不再是草稿或被删除），请刷新后重试",
        ));
    }
    // CTR-02 F2：变更留痕与业务写**同事务**（D11）。失败 → 整笔回滚：
    // 宁可这次改单失败，也不留「合同改了、审计没记」的半截事实。
    // 值没变时 `record_in` 返回 `Skipped`，不写空日志。
    let record = changelog::record_in(
        &tx,
        &change_ctx.ctx(),
        ChangeTarget::of(
            contract_model::CONTRACT_TABLE,
            id.to_string(),
            ChangeAction::Update,
            "修改合同",
        ),
        DiffInput::update(&before_snapshot, &after_snapshot),
    )
    .await;
    if let Err(error) = record {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "写合同变更日志"));
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交修改合同事务"))?;

    detail_of(id).await
}

/// `Option<Option<T>>` 合并：`None` 保持原值，`Some(None)` 清空，`Some(Some(v))` 覆盖。
fn merge_option<T>(patch: Option<Option<T>>, current: Option<T>) -> Option<T> {
    match patch {
        None => current,
        Some(value) => value,
    }
}

/// 合并后的整单复校验（改单专用：规则与新建一致，但错误文案指向「合并结果」）。
fn validate_mutation(merged: &ContractMut) -> std::result::Result<(), ApiError> {
    use nik::prelude::rust_decimal::Decimal;

    if merged.contract_name.trim().is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "name 不能为空".to_string(),
        ));
    }
    if merged.contract_name.chars().count() > 200 {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "name 最长 200 个字符（contract_name VARCHAR(200)）".to_string(),
        ));
    }
    if merged.amount <= Decimal::ZERO {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("amount 必须大于 0（当前 {}）：ck_biz_contract_amount", merged.amount),
        ));
    }
    if let (Some(start), Some(end)) = (merged.start_date, merged.end_date)
        && end < start
    {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("end_date（{end}）不能早于 start_date（{start}）：区间反了"),
        ));
    }
    if let (Some(sign), Some(start)) = (merged.sign_date, merged.start_date)
        && sign > start
    {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("sign_date（{sign}）不能晚于 start_date（{start}）：先签约再生效"),
        ));
    }
    Ok(())
}

// ==================== ⑤ 状态流转 ====================

/// 合同状态流转（`DRAFT → PENDING_APPROVAL → SIGNED → ACTIVE → COMPLETED`，
/// 另有 `PENDING_APPROVAL → DRAFT` 回位与 `SIGNED/ACTIVE → TERMINATED`）。
///
/// # 边从哪来
///
/// **唯一来源是状态机**：`Entity::Contract.next_statuses()` / `can_transition()`
/// 判定，`state_machine::transition()` 复核并写 `biz_status_log`。handler 里没有 if-else 边表。
///
/// # 顺序
///
/// 1. 目标态白名单（6 态；卡面的 `CANCELLED` 不存在 → 400 并点名可用态）；
/// 2. 事务 + `FOR UPDATE` 锁行，取当前态；
/// 3. 边判定（非法 → 409 + `66015`，文案带「当前态允许去哪」）；
///    **`DRAFT → SIGNED/ACTIVE` 在这里被拒**，文案直接点名「先提交审批」
///    （R-P1-1 的核心：没人审过的合同不能变成可用于付款的已签状态）；
/// 4. 目标态 = `PENDING_APPROVAL` → 走**提交审批**分支（起 `contract_approve` 实例 +
///    回写 `approval_instance_id`，与本事务同生共死，见 [`submit_approval_tx`]）；
/// 5. 目标态 = `DRAFT` 且当前 `PENDING_APPROVAL` → 视为**撤回**：先在跑的审批终止
///    （否则留下孤儿实例，下次提交撞唯一索引），再走普通流转并清空实例；
/// 6. DDL 前置：目标态需要日期三件套（`ck_biz_contract_signed_dates`）→ 缺 → 400；
/// 7. `transition()`（需要 reason 的边缺 reason → 它自己报 400）；
/// 8. 带 CAS 的 `UPDATE status`，0 行 → 409。
///
/// # 异常情况
///
/// - 400 + `10001`：目标态非法 / 缺 reason / 缺日期；
/// - 404 + `66014`：合同不存在；409 + `66015`：非法边或并发冲突；
/// - 409 + `65020`（`APPROVAL_FLOW_NOT_CONFIGURED`）：没有已发布的 `contract_approve`
///   流程（fail-closed，显式跳过需 `business:erp:skip-approval` + ≥10 字理由）；
/// - 403 / 422：发起审批时的内控 / 审批人解析问题（引擎原码透传）；500：数据库异常。
#[nik::permission(
    path = "/business/erp/contract/{id}/status",
    method = "PATCH",
    permission = "business:erp:contract:status"
)]
#[nik::patch("/business/erp/contract/{id}/status")]
pub async fn contract_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ContractStatusReq>,
) -> ResResult<ContractDetailRes> {
    let target = req.target()?.to_string();
    let reason = req.reason_text().map(|text| text.to_string());
    let id = parse_uuid_text(&id, "id")?;
    let operator = operator_of(&claims);

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启合同状态流转事务"))?;
    let row = match contract_model::contract_lock(&tx, id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_usable(&format!("id={id}")));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定合同"));
        }
    };

    // ③ 边判定：注册表是唯一真相（不是 handler 里的 match）
    let entity = state_machine::Entity::Contract;
    if !entity.can_transition(&row.status, &target) {
        let message = reveal_approval_requirement(&row, &target)
            .unwrap_or_else(|| state_machine::rejection_message(state_machine::ENTITY_CONTRACT, &row.status, &target));
        let _ = tx.rollback().await;
        return Err(illegal_transition(message));
    }

    // ④ 提交审批：起流程 + 回写实例（同事务）
    if target == contract_model::STATUS_PENDING_APPROVAL {
        let mut out = match submit_approval_tx(&tx, id, &row, &req, &claims, operator).await {
            Ok(out) => out,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        };
        let instance = out.instance_id.unwrap_or_default();
        let auto_finished = out.instance_status.eq_ignore_ascii_case("APPROVED");
        // ⑥ 联动（LNK-07）：`DRAFT → PENDING_APPROVAL` 也是能走到的触发状态，
        //    不接这一条，`CONTRACT/PENDING_APPROVAL` 上配的规则就会「能保存、能启用、
        //    永远不触发」（P1-FIX-5C 原病）。事务内跑关键动作，通知类待 commit 后发。
        let linkage_change = match contract_linkage::run_in_tx(
            &tx,
            id,
            &row.contract_no,
            contract_model::STATUS_PENDING_APPROVAL,
            operator,
        )
        .await
        {
            Ok(change) => change,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        };
        if let Err(error) = tx.commit().await {
            return Err(utils::db_error(error, "提交合同审批事务"));
        }
        // 通知 / 建任务这类 AfterCommit 动作：**提交之后**才跑，失败不回滚业务
        // （状态 + 实例已经落库，一封站内信失败不该把审批撤回去）
        contract_linkage::run_deferred_after_commit(linkage_change).await;
        // 锁外副作用（审批通知 / 客户自配的通知节点）必须在提交之后发，失败只 WARN
        let effects = out.effects.take();
        if !effects.is_empty() {
            engine::flush_effects(effects).await;
        }
        let drafts = out.effects.take_drafts();
        if !drafts.is_empty() {
            engine::flush_drafts(drafts).await;
        }
        tracing::info!(
            contract_id = %id,
            contract_no = %row.contract_no,
            instance_id = %instance,
            flow_code = %contract_bridge::FLOW_CODE,
            "合同已提交审批（状态 PENDING_APPROVAL）"
        );
        // 流程里没有任何审批节点（全被跳过 / 免审配置）→ 实例在同一个事务里就走完了，
        // 提交后立刻执行「审批通过」的回调，否则合同会停在 PENDING_APPROVAL 且没有待办能推进它。
        if auto_finished {
            contract_bridge::on_contract_settled(Some(instance), "APPROVED").await;
        }
        return detail_of(id).await;
    }

    // ⑤ 审批中 → 草稿（手动撤回）：先终止在跑的审批，别留孤儿实例
    if row.status == contract_model::STATUS_PENDING_APPROVAL && target == contract_model::STATUS_DRAFT {
        match contract_bridge::cancel_approval_of(&tx, id, &row.contract_no, "合同撤回审批（操作人主动把单据改回草稿）")
            .await
        {
            Ok(_) => {}
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(utils::db_error(error, "终止合同审批实例"));
            }
        }
    }

    // ④ DDL 前置：SIGNED / ACTIVE / COMPLETED 要求 sign/start/end 三件套齐全
    if contract_model::requires_full_dates(&target) {
        let mut missing = Vec::new();
        if row.sign_date.is_none() {
            missing.push("sign_date");
        }
        if row.start_date.is_none() {
            missing.push("start_date");
        }
        if row.end_date.is_none() {
            missing.push("end_date");
        }
        if !missing.is_empty() {
            let _ = tx.rollback().await;
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!(
                    "流转到「{target}」之前必须先补全 {}：\
                     ck_biz_contract_signed_dates 要求已签合同三个日期齐全（用 PUT 补，草稿才可改）",
                    missing.join(" / ")
                ),
            ));
        }
    }

    // ⑤ 状态机唯一入口（边校验 + reason 校验 + 写 biz_status_log，与本次 UPDATE 同事务）
    if let Err(error) = state_machine::transition(
        &tx,
        state_machine::ENTITY_CONTRACT,
        id,
        &row.contract_no,
        &contract_model::LEGAL_EDGES,
        &row.status,
        &target,
        operator_of(&claims),
        reason.as_deref(),
        None,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }

    // ⑦ 带 CAS 的写状态
    //    * 从 `PENDING_APPROVAL` 回位（撤回 / 人工驳回）→ 连实例一起清空，
    //      否则合同挂在「草稿 + 旧实例」上，下次提交会撞 `uk_biz_flow_instance_biz_active`；
    //    * 其余边（`SIGNED→ACTIVE` 等）只写状态，`approval_instance_id` 保留作审计。
    let write = if row.status == contract_model::STATUS_PENDING_APPROVAL {
        contract_model::contract_apply_approval_transition(&tx, id, &row.status, &target, operator, None).await
    } else {
        contract_model::contract_write_status(&tx, id, &row.status, &target, operator).await
    };
    let affected = match write {
        Ok(rows) => rows,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "合同状态流转"));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(illegal_transition(format!(
            "合同 {} 的状态在本次流转期间已被改动（期望 {}），请刷新后重试",
            row.contract_no, row.status
        )));
    }
    // ⑦-b 终止原因必须同时落 `remark`（ARCH-2 V-18）：状态机只写 `biz_status_log.reason`。
    //     与状态变更**同事务** —— 写失败整笔回滚，不允许出现「状态已终止、备注没留原因」的半截单。
    //     只在 reason 边（SIGNED→TERMINATED / ACTIVE→TERMINATED）上触发，其它边不动 remark。
    if state_machine::CONTRACT_REASON_EDGES.contains(&(row.status.as_str(), target.as_str())) {
        let reason_text = reason.as_deref().unwrap_or_default();
        let existing = match contract_model::contract_remark_of(&tx, id).await {
            Ok(value) => value,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(utils::db_error(error, "读取合同备注"));
            }
        };
        let merged = contract_model::termination_remark(existing.as_deref(), reason_text);
        if let Err(error) = contract_model::contract_write_termination_remark(&tx, id, &merged).await {
            let _ = tx.rollback().await;
            return Err(write_error(error, "回写合同终止原因"));
        }
    }
    // ⑥ 联动（LNK-07）：生效 / 完成 / 终止 / 撤回（`PENDING_APPROVAL → DRAFT`）四条边
    //    都在这里收口 —— 只有**状态机过闸 + CAS 写成功**之后才触发，重复点第二次时
    //    上面的边判定已经把请求挡掉，联动不会跑第二遍。
    let linkage_change = match contract_linkage::run_in_tx(&tx, id, &row.contract_no, &target, operator).await {
        Ok(change) => change,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交合同状态流转事务"))?;
    // 通知类动作在提交之后跑（失败不回滚状态变更）
    contract_linkage::run_deferred_after_commit(linkage_change).await;

    detail_of(id).await
}

// ==================== ⑤-b 提交审批（内部） ====================

/// 边表判定失败时的「额外提示」：把 `DRAFT → SIGNED/ACTIVE` 这种**被审批要求挡住**的边
/// 翻译成用户能照着做的文案（而不是干巴巴的「非法流转」）。
///
/// 为什么单独一个函数：文案要带合同号与当前状态，还要点名「提交到哪个目标态」，
/// 塞进 `match` 里会把 handler 撑成第二张边表（P10 铁律③的坑）。
fn reveal_approval_requirement(row: &contract_model::ContractStateRow, target: &str) -> Option<String> {
    if row.status != contract_model::STATUS_DRAFT {
        return None;
    }
    if target != contract_model::STATUS_SIGNED && target != contract_model::STATUS_ACTIVE {
        return None;
    }
    Some(format!(
        "合同 {} 当前是「草稿」：{} 必须先提交审批（target_status=PENDING_APPROVAL），\
         审批通过后系统自动落「已签订（SIGNED）」，再点「生效」到 ACTIVE。\
         这是内控要求（ARCH-3 R-P1-1）：没人审过的合同不能变成可用于采购付款的已签状态。",
        row.contract_no,
        if target == contract_model::STATUS_SIGNED {
            "「登记签订」"
        } else {
            "「生效」"
        }
    ))
}

/// 提交审批的事务体：`DRAFT → PENDING_APPROVAL` + 起 `contract_approve` 实例 + 回写实例 id。
///
/// 返回引擎动作结果（调用方**提交事务之后**负责发锁外副作用：审批通知 / 通知节点草稿）。
///
/// # 闸门顺序（越早拒绝越好，全部在同一个事务里）
///
/// 1. 三个日期必须齐全 —— 审批通过会直接落 `SIGNED`，而 `ck_biz_contract_signed_dates`
///    要求 `SIGNED` 三件套齐全；提交时不校验，审批通过那一刻就会 500（最难排查的形态）；
/// 2. 同一单已有在跑的审批 → 409 且带审批单号（幂等，与采购单同款文案）；
/// 3. 流程：**有流程就必须走流程**（请求里带 `skip_approval` 也 400）；
///    没流程 → `approval_gate::authorize_missing_flow`（默认 409 + 点名 `contract_approve`，
///    只有 `business:erp:skip-approval` + ≥10 字理由才放行）；
/// 4. 没流程且被显式跳过 → 直接落 `SIGNED`（记一条 warn + 理由进 `biz_status_log`），
///    与「审批通过」同一个落点 —— 跳过审批不能产生一个谁都没批过的「审批中」。
async fn submit_approval_tx(
    tx: &Transaction,
    id: Uuid,
    row: &contract_model::ContractStateRow,
    req: &ContractStatusReq,
    claims: &TokenClaims,
    operator: Option<Uuid>,
) -> std::result::Result<engine::ActionOut, ApiError> {
    // ① 只有草稿能提交审批（边表已保证，这里再核一次状态值便于文案）
    if row.status != contract_model::STATUS_DRAFT {
        return Err(conflict(format!(
            "合同 {} 当前是「{}」（{}），只有草稿（DRAFT）可以提交审批",
            row.contract_no,
            contract_model::status_label(&row.status),
            row.status
        )));
    }
    // ② 三个日期必须齐全（审批通过直接落 SIGNED）
    let missing: Vec<&str> = [
        ("sign_date", row.sign_date.is_none()),
        ("start_date", row.start_date.is_none()),
        ("end_date", row.end_date.is_none()),
    ]
    .into_iter()
    .filter_map(|(name, absent)| absent.then_some(name))
    .collect();
    if !missing.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "合同 {} 提交审批前必须先补全 {}：审批通过会直接把合同落成「已签订」，\
                 而 ck_biz_contract_signed_dates 要求三个日期齐全（草稿可用 PUT 补）",
                row.contract_no,
                missing.join(" / ")
            ),
        ));
    }
    // ③ 幂等：同一单已有在跑的审批 → 409
    if flow_model::biz_instance_running(contract_bridge::BIZ_TYPE, id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        return Err(conflict(format!(
            "合同 {} 已在审批中：请等审批结束，或从审批中心撤回后再提交",
            row.contract_no
        )));
    }
    let flow = contract_bridge::published_contract_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询合同审批流程"))?;
    let gate = approval_gate::ApprovalGate::new(&req.gate, claims);
    let Some(flow) = flow else {
        // ARCH-3 DC-01 的 fail-closed 在这里**更严一档**：合同**不提供**「跳过审批」旁路。
        //
        // 采购单 / 收货单没流程时还能靠 `business:erp:skip-approval` + 理由放行（它们
        // 的边表里有 `DRAFT → SUBMITTED` / `DRAFT → POSTED` 这类「不走审批也能推进」的边）。
        // 合同的边表里**没有** `DRAFT → SIGNED`（R-P1-1 就是要封掉它），所以「跳过审批」
        // 在合同上唯一可行的实现就是绕过状态机 —— 那等于把本卡刚堵上的洞重新打开。
        //
        // 于是这里一律 409：让管理员去发布 `contract_approve`（种子 `V0209` 已经种好，
        // 正常库不会走到这里；走到这里说明有人删了/停用了流程）。请求里带
        // `skip_approval=true` 也是 409，报文同样点名流程编码。
        let _ = gate;
        return Err(ApiError::new(
            409,
            error_code::APPROVAL_FLOW_NOT_CONFIGURED,
            format!(
                "没有配置已发布的审批流程 `{}`（业务类型 {}）：合同必须先审批才能签订/生效，\
                 本次提交已被拒绝。请让管理员在「审批配置」里发布该流程\
                 （种子迁移 V0209 已预置，正常库不应缺失）。",
                contract_bridge::FLOW_CODE,
                contract_bridge::BIZ_TYPE
            ),
        ));
    };
    // 有流程就必须走流程：请求里带了 skip_approval 也要 400（不许绕过已配置的内控）
    approval_gate::ensure_not_skipped(&flow.flow_code, &gate)?;
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 合同审批", row.contract_no)
        .chars()
        .take(200)
        .collect::<String>();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(id),
        biz_no: Some(row.contract_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: contract_form_data(id, row).await,
    };
    let mut out = engine::start(tx, &start)
        .await
        .map_err(|error| submit_conflict(error, "发起合同审批单", &row.contract_no))?;
    if let Some(rejected) = out.rejected.take() {
        let hint = if rejected.code == error_code::FLOW_SELF_APPROVAL_FORBIDDEN {
            "（内控要求：审批人不能是发起人本人。请由他人提交，或让另一个账号具备审批权限后再审）"
        } else {
            ""
        };
        // FIX-1：用 `appended` 而不是 `ApiError::new(..)` 重建 —— 后者会把 65018 的
        // 结构化载荷（前端「指派审批人」引导框）连同 `internal_detail` 一起丢掉。
        return Err(rejected.appended(hint));
    }
    let started = out
        .instance_id
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
    // 状态机唯一入口（边校验 + 写 biz_status_log）
    state_machine::transition(
        tx,
        state_machine::ENTITY_CONTRACT,
        id,
        &row.contract_no,
        &contract_model::LEGAL_EDGES,
        contract_model::STATUS_DRAFT,
        contract_model::STATUS_PENDING_APPROVAL,
        operator,
        Some("提交审批"),
        None,
    )
    .await?;
    // CAS 回写：状态 + 实例 id（同一条 UPDATE，杜绝「状态变了但没挂实例」）
    let affected = contract_model::contract_apply_approval_transition(
        tx,
        id,
        contract_model::STATUS_DRAFT,
        contract_model::STATUS_PENDING_APPROVAL,
        operator,
        Some(started),
    )
    .await
    .map_err(|error| write_error(error, "合同进入审批中"))?;
    if affected == 0 {
        return Err(illegal_transition(format!(
            "合同 {} 的状态在本次提交期间已被改动（期望 DRAFT），请刷新后重试",
            row.contract_no
        )));
    }
    Ok(out)
}

/// 合同审批的**表单变量**。金额必须写成 JSON **数字**（不是字符串）：
/// 条件边比较走 `as_f64()`，`"500000.0000"` 会 TypeMismatch → 引擎按「条件不成立」
/// 静默走兜底边（这会静默把 ≥10 万的合同降级成单级审批 —— 正是审批最不该出错的地方）。
///
/// 同时把**人看的中文名**冻进 `form_data`：审批岗位角色没有 `business:erp:*` 读权限
/// （`form_snapshot` 文件头有实测），审批人零 ERP 权限也要能看懂这是哪家供应商的合同。
async fn contract_form_data(id: Uuid, row: &contract_model::ContractStateRow) -> serde_json::Value {
    let full = contract_model::contract_find(id).await.ok().flatten();
    let amount_text = full
        .as_ref()
        .map(|item| item.amount.round_dp(4).to_string())
        .unwrap_or_default();
    let number = amount_text
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null);
    let mut form = serde_json::json!({
        "contract_id": id.to_string(),
        "contract_no": row.contract_no,
        "amount": number,
        "amount_text": amount_text,
        "sign_date": row.sign_date.map(|d| d.to_string()),
        "start_date": row.start_date.map(|d| d.to_string()),
        "end_date": row.end_date.map(|d| d.to_string()),
    });
    if let Some(full) = full.as_ref() {
        for (key, value) in [
            ("contract_name", Some(full.contract_name.clone())),
            ("contract_type", Some(full.contract_type.clone())),
            ("currency_code", Some(full.currency_code.trim().to_string())),
        ] {
            if let Some(value) = value {
                form[key] = serde_json::Value::String(value);
            }
        }
        form["supplier_id"] = serde_json::Value::String(full.party_b_id.to_string());
        form["supplier_name"] = serde_json::Value::String(full.party_b_name.clone());
        form["create_by"] = match full.create_by {
            Some(id) => serde_json::Value::String(id.to_string()),
            None => serde_json::Value::Null,
        };
    }
    form
}

/// 发起审批时的写冲突翻译（唯一索引撞车 / 其它写错误）。
fn submit_conflict(error: DbError, ctx: &str, contract_no: &str) -> ApiError {
    write_error(error, &format!("{ctx}（{contract_no}）"))
}

// ==================== ⑥ 软删 ====================

/// 软删合同（写 `deleted_at`），被未关闭采购单引用时拒绝。
///
/// # 引用守卫（防御式，A3 之前自动跳过）
///
/// `biz_purchase_order.contract_id` 由 A3 加列；A2 落地时该列**还不存在**，
/// 所以先查 `information_schema`（`contract_reference_column_exists`）：
/// 列不在 → 跳过检查（否则删除接口在 A3 之前整个不可用）；列在 → 按
/// `contract_id = ? AND deleted_at IS NULL` 找引用，有则 409 + `66018` 并带出单号。
/// A3 落地后这层守卫**自动生效**，不需要回来改代码。
///
/// # 异常情况
///
/// - 404 + `66014`：不存在或已删；409 + `66018`：被采购单引用；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/erp/contract/{id}",
    method = "DELETE",
    permission = "business:erp:contract:delete"
)]
#[nik::delete("/business/erp/contract/{id}")]
pub async fn contract_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<serde_json::Value> {
    let id = parse_uuid_text(&id, "id")?;
    let operator = operator_of(&claims);

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启删除合同事务"))?;
    let row = match contract_model::contract_lock_full(&tx, id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_usable(&format!("id={id}")));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定合同"));
        }
    };

    let referencing = match contract_model::contract_referencing_orders(&tx, id).await {
        Ok(refs) => refs,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "检查合同引用"));
        }
    };
    if let Err(error) = reference_guard(&row.contract_no, referencing.as_deref()) {
        let _ = tx.rollback().await;
        return Err(error);
    }

    let affected = match contract_model::contract_soft_delete(&tx, id, operator).await {
        Ok(rows) => rows,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "软删合同"));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(not_usable(&format!("id={id}（并发删除）")));
    }
    // R-P1-1：审批中删单是合法的反向路径（草稿作废走软删除），但**在跑的审批必须先终止**
    // —— 审批实例的 `biz_id` 是弱引用，合同删了而实例还在跑，就是一条永远等不到回调的
    // 孤儿待办（点进去 404），并且会撞 `uk_biz_flow_instance_biz_active`。
    let cancelled = match contract_bridge::cancel_approval_of(
        &tx,
        id,
        &row.contract_no,
        "合同已删除（草稿作废），在跑的审批一并终止",
    )
    .await
    {
        Ok(count) => count,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "终止合同审批实例"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交删除合同事务"))?;

    Ok(Res::ok(serde_json::json!({
        "id": id.to_string(),
        "contract_no": row.contract_no,
        "deleted": true,
        "cancelled_approvals": cancelled.unwrap_or(0),
        "reference_guard": if referencing.is_none() {
            "skipped: biz_purchase_order.contract_id 尚未建列（A3 落地后自动生效）"
        } else {
            "checked: 无引用"
        },
    })))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::contract_dto::CreateContractReq;
    use nik::prelude::chrono::NaiveDate;
    use nik::prelude::rust_decimal::Decimal;
    use serde::Serialize;

    /// 取响应体（`Res<T>` 的 `data`）。
    fn data<T: Serialize>(res: Res<T>) -> T {
        res.data.expect("响应体应带 data")
    }

    /// 假 claims（真库用例只用到 `sub` / `permissions`）。
    fn claims() -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: "018f4b2c-1111-7222-8333-444455556666".to_string(),
            username: Some("a2-contract-test".to_string()),
            roles: vec!["SUPER_ADMIN".to_string()],
            permissions: vec!["*".to_string()],
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期夹具")
    }

    /// `ConnectInfo` 夹具：真库用例直接调 handler，补上对端地址
    /// （CTR-02 起 `contract_update` 要它来落 `sys_change_log.client_ip`）。
    ///
    /// ⚠️ 用 **TEST-NET-3（203.0.113.0/24）** 而不是 127.0.0.1：`utils::client_ip` 对
    /// 私网 / 回环地址一律归零（框架口径「内网保护」），拿回环地址断言 `client_ip` 永远是
    /// `None` —— 会写出一条测不出东西的假绿用例。
    fn peer() -> ConnectInfo<std::net::SocketAddr> {
        ConnectInfo(std::net::SocketAddr::from(([203, 0, 113, 9], 5555)))
    }

    // ---------- 纯逻辑用例（无需数据库） ----------

    /// ① 权限码：5 个、不重复、统一前缀 `business:erp:contract:`（P2-4 前缀统一后四段式）。
    #[test]
    fn contract_permission_codes_are_well_formed() {
        assert_eq!(PERMISSIONS.len(), 5, "ARCH-2 附录 B #7 定了 5 个码");
        let unique: std::collections::BTreeSet<&str> = PERMISSIONS.iter().copied().collect();
        assert_eq!(unique.len(), PERMISSIONS.len(), "权限码不得重复");

        for code in PERMISSIONS {
            assert_eq!(
                code.matches(':').count(),
                3,
                "{code} 必须是 {{模块}}:{{资源}}:{{动作}} 四段式（business:erp:contract:x，与 ERP 其余码同族）"
            );
            let action = code
                .strip_prefix(PERMISSION_PREFIX)
                .and_then(|rest| rest.strip_prefix(':'))
                .unwrap_or_default();
            assert!(!action.is_empty(), "{code} 缺动作段 / 前缀不是 {PERMISSION_PREFIX}");
            assert!(!action.contains(':'), "{code} 的动作段不得再含冒号");
            assert!(
                action.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{code} 的动作段必须小写（前端 error-code / 按钮判权按小写匹配）"
            );
        }

        let actions: Vec<&str> = PERMISSIONS
            .iter()
            .map(|code| code.rsplit(':').next().unwrap_or_default())
            .collect();
        assert_eq!(
            actions,
            vec!["list", "create", "update", "delete", "status"],
            "动作集合必须与 ARCH-2 附录 B #7 一致"
        );
    }

    /// ② 常量名与码值一一对应（P2-4：统一到 `business:erp:contract:*`）。
    #[test]
    fn contract_permission_constants_match_list() {
        assert_eq!(PERM_LIST, "business:erp:contract:list");
        assert_eq!(PERM_CREATE, "business:erp:contract:create");
        assert_eq!(PERM_UPDATE, "business:erp:contract:update");
        assert_eq!(PERM_DELETE, "business:erp:contract:delete");
        assert_eq!(PERM_STATUS, "business:erp:contract:status");
        assert_eq!(PERMISSION_PREFIX, "business:erp:contract");
        for code in PERMISSIONS {
            assert!(
                code.starts_with(PERMISSION_PREFIX),
                "{code} 必须落在统一前缀下（前端 / 运维只按 business:erp:* 一套规则判权）"
            );
        }
    }

    /// ②b P2-4 兼容期双码：每个规范码都能互扩出对应的历史码，反之亦然。
    ///
    /// 这条把「旧码仍能访问」钉在代码层：装载权限时（`auth::model::load_permissions`）
    /// 只有规范码，靠 `shared::permission_alias::expand` 补出旧码 —— 老令牌 / 前端老包
    /// 因此在兼容期内继续命中。
    #[test]
    fn contract_legacy_codes_alias_both_ways() {
        use crate::shared::permission_alias;
        assert_eq!(LEGACY_PERMISSIONS.len(), PERMISSIONS.len());
        for (i, canonical) in PERMISSIONS.iter().enumerate() {
            let legacy = LEGACY_PERMISSIONS[i];
            assert_eq!(
                legacy.rsplit(':').next(),
                canonical.rsplit(':').next(),
                "对照表必须按同一动作位排列（{legacy} vs {canonical}）"
            );
            assert_eq!(
                permission_alias::aliases_of(canonical),
                vec![legacy],
                "{canonical} 应扩出 {legacy}"
            );
            assert_eq!(
                permission_alias::aliases_of(legacy),
                vec![*canonical],
                "{legacy} 应扩出 {canonical}（老令牌带着旧码时路由才放行）"
            );
        }
    }

    /// ③ 合同边表与状态机注册表逐条一致（A1 改了注册表、A2 没跟上 → 立刻红）。
    #[test]
    fn legal_edges_match_state_machine() {
        assert!(
            contract_model::legal_edges_match_registry(),
            "contract_model::LEGAL_EDGES 与 state_machine::CONTRACT_EDGES 不一致（transition 会 500）"
        );
        assert_eq!(contract_model::LEGAL_EDGES.len(), 7);
        let entity = state_machine::Entity::Contract;
        for (from, to) in contract_model::LEGAL_EDGES {
            assert!(entity.can_transition(from, to), "{from}→{to} 在注册表里不合法");
        }
        // 卡面写的 DRAFT→ACTIVE（跳过签订）必须是非法边
        assert!(!entity.can_transition("DRAFT", "ACTIVE"));
        assert!(!entity.can_transition("DRAFT", "CANCELLED"), "没有 CANCELLED 态");
    }

    /// ④ 新建缺必填 → 400（卡面验收第 2 条；validate 在连库之前就拦下）。
    #[tokio::test]
    async fn create_without_required_fields_returns_400() {
        let err = contract_create(Extension(AuthClaims(claims())), Json(CreateContractReq::default()))
            .await
            .expect_err("空请求必须拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
    }

    /// ⑤ 引用守卫纯判定：有引用 → 409 + 66018 且带出单号；列不存在（None）→ 放行。
    #[test]
    fn reference_guard_reports_in_use_with_order_numbers() {
        let refs = vec!["PO-20261102-000001".to_string()];
        let err = reference_guard("HT-20261102-000001", Some(&refs)).expect_err("有引用必须拒");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, error_code::ERP_CONTRACT_IN_USE);
        assert!(
            err.msg.contains("PO-20261102-000001"),
            "文案要带出引用单号：{}",
            err.msg
        );
        assert!(reference_guard("HT-20261102-000001", Some(&[])).is_ok());
        assert!(
            reference_guard("HT-20261102-000001", None).is_ok(),
            "A3 未建列时必须跳过（否则删除接口整个不可用）"
        );
    }

    /// ⑥ 非草稿改单的文案要点明「只有草稿可改」并给出去路。
    #[test]
    fn field_locked_message_points_to_status_endpoint() {
        let err = field_locked("SIGNED", "已签订");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, error_code::ERP_CONTRACT_FIELD_LOCKED);
        assert!(err.msg.contains("DRAFT"), "{}", err.msg);
        assert!(err.msg.contains("PATCH /status"), "{}", err.msg);
    }

    /// ⑦ 合并语义：`None` 保持、`Some(None)` 清空、`Some(Some(v))` 覆盖。
    #[test]
    fn merge_option_keeps_patch_semantics() {
        let original = Some(date("2026-01-01"));
        assert_eq!(merge_option(None, original), original, "没传 → 保持");
        assert_eq!(merge_option(Some(None), original), None, "显式 null → 清空");
        assert_eq!(
            merge_option(Some(Some(date("2026-03-01"))), original),
            Some(date("2026-03-01")),
            "给了值 → 覆盖"
        );
    }

    /// ⑧ 改单合并后的复校验：只改 end_date 把区间改反 → 400。
    #[test]
    fn validate_mutation_catches_reversed_range_after_merge() {
        let merged = ContractMut {
            contract_name: "A2 改单".to_string(),
            party_b_id: Uuid::now_v7(),
            party_b_name: "乙方".to_string(),
            amount: Decimal::new(100, 0),
            sign_date: Some(date("2026-01-01")),
            start_date: Some(date("2026-01-10")),
            end_date: Some(date("2026-01-05")),
            remark: None,
        };
        let err = validate_mutation(&merged).expect_err("区间反了必须拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("end_date"), "{}", err.msg);
    }

    /// ⑨ 与 `sys_menu.auth_code` 一一对应（真库用例；需 `ERP_DB_URL`，否则跳过）。
    ///
    /// 这条是「权限码唯一真相」的守门人：迁移里少种一个码 → 除超管外全员 403；
    /// 多种一个码 → 管理面能勾一个永不生效的权限。
    ///
    /// P2-4 之后口径（`V0241`）：库里有且只有 5 个 `business:erp:contract:*`，
    /// **旧码 `erp:contract:*` 必须 0 行**（否则管理面会同时出现两套同义节点，
    /// 而代码只认一套 —— 那正是本卡要消灭的"双真值源"）。
    #[tokio::test]
    async fn contract_permissions_match_menu() {
        let Some(_guard) = setup().await else { return };
        let rows = db()
            .find_with_params(
                "SELECT auth_code FROM sys_menu \
                  WHERE auth_code LIKE 'business:erp:contract:%' AND deleted_at IS NULL AND status = 1",
                vec![],
            )
            .await
            .expect("查询 sys_menu 失败");

        let mut in_db: Vec<String> = Vec::with_capacity(rows.len());
        for row in rows {
            in_db.push(row.get_typed::<String>("auth_code").expect("auth_code 列"));
        }
        in_db.sort();
        let mut expected: Vec<String> = PERMISSIONS.iter().map(|c| (*c).to_string()).collect();
        expected.sort();
        assert_eq!(
            in_db, expected,
            "sys_menu.auth_code 必须与 crate 常量逐字一致（V0241 迁移 + 本文件要同步改）"
        );

        let legacy_left: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM sys_menu \
                  WHERE auth_code LIKE 'erp:contract:%' AND deleted_at IS NULL",
                vec![],
            )
            .await
            .expect("查询旧码残留失败");
        assert_eq!(
            legacy_left, 0,
            "V0241 之后 sys_menu 里不应再有 erp:contract:*（旧码只活在兼容别名表里）"
        );
    }

    /// ⑩ 真库：授权 → 装载权限 → **双码都在**（P2-4「兼容期旧码仍能访问」的落地点）。
    ///
    /// 造一个只持合同 5 个权限点的非超管用户，走真实的 `auth::model::load_permissions`
    /// （登录 / 刷新令牌 / `/me` 都是这一条读路径），断言：
    ///
    /// 1. 5 个规范码 `business:erp:contract:*` 在（库里就这些）；
    /// 2. 5 个历史码 `erp:contract:*` 也在 —— 全靠 `shared::permission_alias::expand`
    ///    在装载末尾补出来，因此老令牌里的旧码、前端老包按旧码判权都继续命中；
    /// 3. 其它 ERP 码不被误扩（只按对照表逐条映射，不按前缀）。
    #[tokio::test]
    async fn granted_contract_permissions_carry_legacy_alias() {
        let Some(_guard) = setup().await else { return };
        let role_id = Uuid::now_v7();
        let user_id = Uuid::now_v7();
        let tag = &user_id.simple().to_string()[..8];
        let role_code = format!("A2P4_ROLE_{tag}");
        let username = format!("A2P4_USER_{tag}");

        db().execute_with_params(
            "INSERT INTO sys_role (id, role_code, role_name, status) VALUES (?::uuid, ?, 'P2-4 合同授权角色', 1)",
            vec![Value::from(&role_id), Value::Text(role_code.clone())],
        )
        .await
        .unwrap_or_else(|err| panic!("造角色失败：{err}"));
        db().execute_with_params(
            "INSERT INTO sys_user (id, username, password, real_name, status) \
             VALUES (?::uuid, ?, 'x', 'P2-4 用户', 1)",
            vec![Value::from(&user_id), Value::Text(username.clone())],
        )
        .await
        .unwrap_or_else(|err| panic!("造用户失败：{err}"));
        db().execute_with_params(
            "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::uuid, ?::uuid)",
            vec![Value::from(&user_id), Value::from(&role_id)],
        )
        .await
        .unwrap_or_else(|err| panic!("关联用户角色失败：{err}"));
        // 按 auth_code 找菜单 id 授权 —— 与管理员在「角色授权」树里勾选等价（按 menu_id 落库）
        let mut granted = 0u64;
        for code in PERMISSIONS {
            granted += db()
                .execute_with_params(
                    "INSERT INTO sys_role_menu (role_id, menu_id) \
                     SELECT ?::uuid, id FROM sys_menu \
                      WHERE auth_code = ? AND deleted_at IS NULL",
                    vec![Value::from(&role_id), Value::Text(code.to_string())],
                )
                .await
                .unwrap_or_else(|err| panic!("授权 {code} 失败：{err}"));
        }
        assert_eq!(granted, 5, "5 个合同权限点都应授出去（实际 {granted}）");

        let perms = crate::system::auth::model::load_permissions(&user_id, false)
            .await
            .expect("装载权限失败");
        for code in PERMISSIONS {
            assert!(perms.contains(&code.to_string()), "缺规范码 {code}：{perms:?}");
        }
        for code in LEGACY_PERMISSIONS {
            assert!(
                perms.contains(&code.to_string()),
                "缺历史码 {code}（兼容期老令牌将 403）：{perms:?}"
            );
        }
        assert_eq!(perms.len(), 10, "只应涉及合同 10 个码：{perms:?}");
        assert!(
            !perms
                .iter()
                .any(|c| c.starts_with("business:erp:contract:") && !PERMISSIONS.contains(&c.as_str())),
            "不得凭前缀凭空扩码：{perms:?}"
        );
        println!("[P2-4] 授权 5 个新码 → load_permissions 返回 10 个（新 5 + 旧 5）✓");

        // 清场（授权 → 用户角色 → 用户 → 角色；sys_role_menu 先删以免外键拦住）
        for (sql, param) in [
            ("DELETE FROM sys_role_menu WHERE role_id = ?::uuid", &role_id),
            ("DELETE FROM sys_user_role WHERE user_id = ?::uuid", &user_id),
            ("DELETE FROM sys_user WHERE id = ?::uuid", &user_id),
            ("DELETE FROM sys_role WHERE id = ?::uuid", &role_id),
        ] {
            db().execute_with_params(sql, vec![Value::from(param)])
                .await
                .unwrap_or_else(|err| panic!("清场失败（{sql}）：{err}"));
        }
    }

    // ---------- 真库用例（`ERP_DB_URL`，与 A1 同一套夹具约定） ----------

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 本模块夹具前缀（清理用；避开 A1 用过的 `A1CM-` / `A1CT-`）。
    const PREFIX: &str = "A2CM-";
    const CONTRACT_PREFIX: &str = "A2C-";

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

    /// 清场：先删状态日志（`biz_status_log` 无外键，业务行一删就再也找不回 id），
    /// 再删合同与供应商（真库用例外不留痕）。
    async fn cleanup() {
        db().execute_with_params(
            "DELETE FROM biz_status_log WHERE entity_type = 'CONTRACT' \
               AND entity_id IN (SELECT id FROM biz_contract WHERE contract_name LIKE 'A2C-%')",
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("清理合同状态日志失败：{err}"));
        db().execute_with_params("DELETE FROM biz_contract WHERE contract_name LIKE 'A2C-%'", vec![])
            .await
            .unwrap_or_else(|err| panic!("清理合同失败：{err}"));
        db().execute_with_params("DELETE FROM biz_supplier WHERE supplier_code LIKE 'A2CM-%'", vec![])
            .await
            .unwrap_or_else(|err| panic!("清理供应商失败：{err}"));
        // AUD-02：人员姓名用例造的账号（见 `make_user`），用例失败也不留痕。
        db().execute_with_params("DELETE FROM sys_user WHERE username LIKE 'A2CM-U%'", vec![])
            .await
            .unwrap_or_else(|err| panic!("清理测试账号失败：{err}"));
    }

    /// 造一个账号（**AUD-02**：人员字段的姓名来源）。
    ///
    /// 只给 `sys_user` 的非空列（与 `granted_contract_permissions_carry_legacy_alias`
    /// 里的造号语句同口径），不授任何角色。
    async fn make_user(tag: &str, real_name: &str) -> Uuid {
        let id = Uuid::now_v7();
        let suffix = &id.simple().to_string()[..8];
        db().execute_with_params(
            "INSERT INTO sys_user (id, username, password, real_name, status) \
             VALUES (?::uuid, ?, 'x', ?, 1)",
            vec![
                Value::from(&id),
                Value::Text(format!("{PREFIX}U{tag}_{suffix}")),
                Value::Text(real_name.to_string()),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("造账号失败：{err}"));
        id
    }

    /// 造一个供应商（`status = 1` 可新签）。
    async fn make_supplier(tag: &str) -> (Uuid, String) {
        let id = Uuid::now_v7();
        let name = format!("A2 合同乙方 {tag}");
        let code = format!("{PREFIX}{tag}-{}", &id.simple().to_string()[..8]);
        db().execute_with_params(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) \
             VALUES (?::uuid, ?, ?, 1)",
            vec![Value::from(&id), Value::Text(code), Value::Text(name.clone())],
        )
        .await
        .unwrap_or_else(|err| panic!("造供应商失败：{err}"));
        (id, name)
    }

    /// 组装一个合法的建单请求。
    fn create_req(tag: &str, supplier_id: Uuid) -> CreateContractReq {
        CreateContractReq {
            name: Some(format!("{CONTRACT_PREFIX}{tag}")),
            contract_type: Some("PURCHASE".to_string()),
            party_b_id: Some(supplier_id.to_string()),
            amount: Some(Decimal::new(500_000, 2)), // 5000.00
            sign_date: Some(date("2026-01-01")),
            start_date: Some(date("2026-01-02")),
            end_date: Some(date("2026-12-31")),
            remark: Some("A2 合同 CRUD 用例夹具".to_string()),
            ..Default::default()
        }
    }

    /// 建一张合同，返回 (id, 乙方名称快照)。
    async fn create_ok(tag: &str) -> (Uuid, String) {
        let (supplier_id, supplier_name) = make_supplier(tag).await;
        let detail = data(
            contract_create(Extension(AuthClaims(claims())), Json(create_req(tag, supplier_id)))
                .await
                .unwrap_or_else(|err| {
                    panic!(
                        "建合同失败：{} {}（内部细节：{:?}）",
                        err.code, err.msg, err.internal_detail
                    )
                }),
        );
        let id = Uuid::parse_str(&detail.base.id).expect("id 是 uuid");
        (id, supplier_name)
    }

    /// **AUD-02** 验收 ②③④：详情下发三个人员字段的**姓名**（不是 UUID）；
    /// 改名后立刻取新名（实时 join，不是快照）；一次详情只发 **1** 条取名字的 SQL。
    #[tokio::test]
    async fn detail_fills_person_names_and_follows_rename() {
        use crate::business::shared::user_name::stats;

        let Some(_guard) = setup().await else { return };
        // 三个账号分别占三个字段：负责人 / 制单人 / 修改人
        let owner = make_user("OW", "张负责").await;
        let creator = make_user("CR", "王制单").await;
        let updater = make_user("UP", "李修改").await;

        let (contract_id, _) = create_ok("AUD2").await;
        db().execute_with_params(
            "UPDATE biz_contract SET owner_id = ?::uuid, create_by = ?::uuid, update_by = ?::uuid \
              WHERE id = ?::uuid",
            vec![
                Value::from(&owner),
                Value::from(&creator),
                Value::from(&updater),
                Value::from(&contract_id),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("写人员列失败：{err}"));

        stats::reset();
        let detail = data(
            contract_detail(Path(contract_id.to_string()))
                .await
                .unwrap_or_else(|err| panic!("查详情失败：{} {}", err.code, err.msg)),
        );
        // ③ 无 N+1：三个字段去重后仍只 1 条 SQL
        assert_eq!(
            stats::count(),
            1,
            "一次详情只允许 1 条取名字 SQL（实测 {}）",
            stats::count()
        );
        // ② 三个字段都给人名，不是 UUID
        assert_eq!(detail.owner_name.as_deref(), Some("张负责"), "负责人姓名");
        assert_eq!(detail.create_by_name.as_deref(), Some("王制单"), "制单人姓名");
        assert_eq!(detail.update_by_name.as_deref(), Some("李修改"), "修改人姓名");

        // ④ 实时 join：改名后详情立刻是新名
        db().execute_with_params(
            "UPDATE sys_user SET real_name = '张负责（改名后）' WHERE id = ?::uuid",
            vec![Value::from(&owner)],
        )
        .await
        .unwrap_or_else(|err| panic!("改名失败：{err}"));
        let again = data(
            contract_detail(Path(contract_id.to_string()))
                .await
                .unwrap_or_else(|err| panic!("查详情失败：{} {}", err.code, err.msg)),
        );
        assert_eq!(
            again.owner_name.as_deref(),
            Some("张负责（改名后）"),
            "展示类字段必须跟改名走（实时 join）"
        );
        assert_ne!(
            again.owner_name.as_deref(),
            Some("张负责"),
            "改名后不得还是旧名（那是快照口径，AUD-01 的语义）"
        );

        // 清场：合同行由 `cleanup()` 统一删，账号这里删（用户无角色关联）
        for id in [&owner, &creator, &updater] {
            db().execute_with_params("DELETE FROM sys_user WHERE id = ?::uuid", vec![Value::from(id)])
                .await
                .unwrap_or_else(|err| panic!("清理账号失败：{err}"));
        }
    }

    /// 验收第 1 条：新建成功 → 拿到 id + HT 号，状态 DRAFT，乙方名称是服务端快照。
    #[tokio::test]
    async fn create_returns_draft_with_ht_number() {
        let Some(_guard) = setup().await else { return };
        let (supplier_id, supplier_name) = make_supplier("OK").await;
        let detail = data(
            contract_create(Extension(AuthClaims(claims())), Json(create_req("NEW", supplier_id)))
                .await
                .expect("新建合同"),
        );

        assert!(!detail.base.id.is_empty(), "必须给 id");
        assert!(
            detail.base.contract_no.starts_with("HT-"),
            "合同号必须是 HT- 前缀：{}",
            detail.base.contract_no
        );
        assert_eq!(detail.base.status, "DRAFT", "初始状态恒 DRAFT");
        assert_eq!(detail.base.status_label, "草稿");
        assert_eq!(detail.base.name, format!("{CONTRACT_PREFIX}NEW"));
        assert_eq!(
            detail.base.party_b_name, supplier_name,
            "乙方名称必须是服务端从主档读到的快照"
        );
        assert_eq!(
            detail.base.party_b_current_name.as_deref(),
            Some(supplier_name.as_str()),
            "关联带出的现值"
        );
        assert!(detail.can_edit, "草稿可改");
        assert_eq!(
            detail.next_statuses,
            vec!["PENDING_APPROVAL".to_string()],
            "R-P1-1：草稿只有一个去处 —— 提交审批（不再能一步签订）"
        );
    }

    /// 验收第 3 条：party_b_id 不存在 → 400 + 66021。
    #[tokio::test]
    async fn create_with_unknown_party_b_returns_66021() {
        let Some(_guard) = setup().await else { return };
        let ghost = Uuid::now_v7();
        let err = contract_create(Extension(AuthClaims(claims())), Json(create_req("GHOST", ghost)))
            .await
            .expect_err("不存在的乙方必须拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_CONTRACT_PARTY_INVALID);
    }

    /// 验收第 6 条：修改 DRAFT 成功（含换乙方 → 快照跟着更新）。
    #[tokio::test]
    async fn update_draft_persists_changes() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = create_ok("UPD").await;
        let (other_id, other_name) = make_supplier("UPD2").await;

        let detail = data(
            contract_update(
                HeaderMap::new(),
                peer(),
                Extension(AuthClaims(claims())),
                Path(id.to_string()),
                Json(UpdateContractReq {
                    name: Some(format!("{CONTRACT_PREFIX}UPD-改后")),
                    party_b_id: Some(other_id.to_string()),
                    amount: Some(Decimal::new(777_700, 2)),
                    remark: Some(Some("改后备注".to_string())),
                    ..Default::default()
                }),
            )
            .await
            .expect("改草稿"),
        );

        assert_eq!(detail.base.name, format!("{CONTRACT_PREFIX}UPD-改后"));
        assert_eq!(detail.base.amount, Decimal::new(777_700, 2));
        assert_eq!(detail.base.party_b_id, other_id.to_string());
        assert_eq!(detail.base.party_b_name, other_name, "换乙方后快照必须是新主档的名字");
        assert_eq!(detail.remark.as_deref(), Some("改后备注"));
        assert_eq!(detail.base.status, "DRAFT", "改单不动状态");
    }

    /// **CTR-02 F2**：草稿改金额 5000 → 5500，`sys_change_log` 必须落**一行**，
    /// 且 diff 里能读到「谁、什么时候、从什么改成什么」；值没变时**不写空日志**。
    ///
    /// 这条替代了 CTR-01 §3 F2 的实测缺口（当时 `sys_change_log` 0 行）。
    #[tokio::test]
    async fn update_draft_writes_change_log() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = create_ok("LOG").await;

        let updated = data(
            contract_update(
                HeaderMap::new(),
                peer(),
                Extension(AuthClaims(claims())),
                Path(id.to_string()),
                Json(UpdateContractReq {
                    amount: Some(Decimal::new(550_000, 2)), // 5000.00 → 5500.00
                    ..Default::default()
                }),
            )
            .await
            .expect("改草稿"),
        );
        assert_eq!(updated.base.amount, Decimal::new(550_000, 2));
        // F6：没挂订单时已执行 0、剩余 = 合同额
        assert_eq!(updated.committed_amount, Decimal::ZERO, "无订单时已执行 0");
        assert_eq!(
            updated.remaining_amount,
            Decimal::new(550_000, 2),
            "无订单时剩余额度 = 合同额"
        );
        assert_eq!(updated.open_order_count, 0);
        assert!(updated.open_order_nos.is_empty());

        let rows = db()
            .find_with_params(
                "SELECT operator_name, module, action, action_label, \
                        array_to_string(changed_fields, ',') AS changed_fields, \
                        diff::text AS diff, host(client_ip) AS client_ip \
                   FROM sys_change_log \
                  WHERE resource = 'biz_contract' AND resource_id = ?",
                vec![Value::Text(id.to_string())],
            )
            .await
            .expect("查变更日志");
        assert_eq!(rows.len(), 1, "改一次只写一行（F2 的判据就是这行）");
        let row = &rows[0];
        assert_eq!(
            row.get_typed::<String>("module").expect("module"),
            "business",
            "module 由登记表决定"
        );
        assert_eq!(row.get_typed::<String>("action").expect("action"), "UPDATE");
        assert_eq!(
            row.get_typed::<String>("action_label").expect("action_label"),
            "修改合同"
        );
        assert_eq!(
            row.get_typed::<String>("changed_fields").expect("changed"),
            "amount",
            "只有金额变了"
        );
        assert_eq!(
            row.get_typed::<Option<String>>("operator_name")
                .expect("operator_name")
                .as_deref(),
            Some("a2-contract-test"),
            "操作人姓名是快照（改单时是谁）"
        );
        assert_eq!(
            row.get_typed::<Option<String>>("client_ip")
                .expect("client_ip")
                .as_deref(),
            Some("203.0.113.9"),
            "客户端 IP 取自 ConnectInfo（回环/私网会被归零，夹具用 TEST-NET-3）"
        );
        let diff: serde_json::Value =
            serde_json::from_str(&row.get_typed::<String>("diff").expect("diff")).expect("diff json");
        assert_eq!(
            diff["fields"]["amount"]["b"],
            serde_json::json!("5000.0000"),
            "改前值必须留下来（CTR-01 当时丢的就是它）"
        );
        assert_eq!(diff["fields"]["amount"]["a"], serde_json::json!("5500.0000"));

        // 同值再改一次：不许写空日志
        contract_update(
            HeaderMap::new(),
            peer(),
            Extension(AuthClaims(claims())),
            Path(id.to_string()),
            Json(UpdateContractReq {
                amount: Some(Decimal::new(550_000, 2)),
                ..Default::default()
            }),
        )
        .await
        .expect("同值再改应当成功");
        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM sys_change_log \
                  WHERE resource = 'biz_contract' AND resource_id = ?",
                vec![Value::Text(id.to_string())],
            )
            .await
            .expect("count");
        assert_eq!(count, 1, "值没变不写空日志（diff 引擎 Skipped）");
    }

    /// 夹具：往合同上挂一张采购单（只填 `biz_purchase_order` 的必填列）。
    ///
    /// `supplier_id` / `warehouse_id` 都是**弱引用**（V0137 / V0198 无外键），
    /// 所以这里不需要真造供应商与仓库 —— 本用例验的是履约口径的 SQL，不是下单链。
    async fn insert_po(contract_id: Uuid, order_no: &str, total_with_tax: &str, status: &str) {
        // `ck_biz_po_contract_pair`：`purchase_type='CONTRACT'` ⇒ `contract_id` 与
        // `contract_no_snapshot` 都必须非空（V0198 的强化版），所以先取合同号再插。
        let contract_no: String = db()
            .scalar_with_params(
                "SELECT contract_no FROM biz_contract WHERE id = ?::uuid",
                vec![Value::from(&contract_id)],
            )
            .await
            .expect("取合同号");
        db().execute_with_params(
            "INSERT INTO biz_purchase_order \
                     (id, order_no, supplier_id, order_date, warehouse_id, status, purchase_type, \
                      contract_id, contract_no_snapshot, \
                      total_qty, total_amount, total_tax, total_with_tax) \
                 VALUES (?, ?, ?, CURRENT_DATE, ?, ?, 'CONTRACT', ?::uuid, ?, \
                         1, ?::text::numeric, 0, ?::text::numeric)",
            vec![
                Value::from(&Uuid::now_v7()),
                Value::Text(order_no.to_string()),
                Value::from(&Uuid::now_v7()),
                Value::from(&Uuid::now_v7()),
                Value::Text(status.to_string()),
                Value::from(&contract_id),
                Value::Text(contract_no),
                Value::Text(total_with_tax.to_string()),
                Value::Text(total_with_tax.to_string()),
            ],
        )
        .await
        .expect("插采购单夹具");
    }

    /// **CTR-02 F6 / F3（真库）**：详情现算「已执行金额 / 剩余额度 / 未关闭单据」。
    ///
    /// 断言面：
    /// ① 已执行 = 采购单价税合计的 SUM（**排除 CANCELLED**，与下单额度守卫同函数）；
    /// ② 剩余额度**允许为负**（合同 5000，已下单 34000 → -29000 = 超签金额）；
    /// ③ 未关闭单据口径：`DRAFT` 算未完成、`RECEIVED` / `CANCELLED` 不算；
    /// ④ 条款禁止改归档行为 —— 本用例**不断言** 409（ARCH-2 §4.1 E4「不硬拦」）。
    #[tokio::test]
    async fn detail_reports_commitment_and_open_orders() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = create_ok("CMT").await;
        // 单号带合同 id 前 8 位：`uk_biz_po_no` 是全库唯一索引，重复跑同一用例不能撞
        let tag = &id.to_string()[..8];

        insert_po(id, &format!("PO-CTR02-{tag}-1"), "33900.00", "DRAFT").await;
        insert_po(id, &format!("PO-CTR02-{tag}-2"), "100.00", "RECEIVED").await;
        insert_po(id, &format!("PO-CTR02-{tag}-3"), "999.00", "CANCELLED").await;

        let detail = data(contract_detail(Path(id.to_string())).await.expect("合同详情"));
        assert_eq!(
            detail.committed_amount,
            Decimal::new(3_400_000, 2),
            "已执行 = 33900 + 100（CANCELLED 的 999 不计）"
        );
        assert_eq!(
            detail.remaining_amount,
            Decimal::new(500_000, 2) - Decimal::new(3_400_000, 2),
            "剩余额度 = 合同额 5000 - 已执行 34000 = -29000（超签，不夹到 0）"
        );
        assert_eq!(detail.open_order_count, 1, "只有 DRAFT 那张算未完成");
        assert_eq!(
            detail.open_order_nos,
            vec![format!("PO-CTR02-{tag}-1")],
            "未关闭单号要点名（前端在归档前据此提示）"
        );
    }

    /// 前置：把合同直接推到某个「已签」状态（**只用于给别的用例造前置**）。
    ///
    /// 为什么可以绕过状态机：`DRAFT → SIGNED` 在 R-P1-1 之后已经不是合法边了（必须走审批），
    /// 而这几条用例测的是「非草稿能不能改」「终止边要不要 reason」—— 它们只需要一个
    /// **状态值**，不需要审批链路。真正的「提交审批 → 通过 → SIGNED」链路由
    /// `contract_approval_tests` 覆盖（真种子 + 真待办）。
    async fn force_status(id: Uuid, status: &str) {
        db().execute_with_params(
            "UPDATE biz_contract SET status = ? WHERE id = ?::uuid",
            vec![Value::Text(status.to_string()), Value::from(&id)],
        )
        .await
        .unwrap_or_else(|err| panic!("造前置状态失败：{err}"));
    }

    /// 验收第 7 条：修改非 DRAFT → 409 + 66016（卡面写 400 + 66014，已按 A1 的码表更正）。
    #[tokio::test]
    async fn update_non_draft_returns_66016() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = create_ok("LOCK").await;
        // 前置：已签订（真实链路见 contract_approval_tests）
        force_status(id, "SIGNED").await;

        let err = contract_update(
            HeaderMap::new(),
            peer(),
            Extension(AuthClaims(claims())),
            Path(id.to_string()),
            Json(UpdateContractReq {
                amount: Some(Decimal::new(1, 0)),
                ..Default::default()
            }),
        )
        .await
        .expect_err("非草稿必须拒");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, error_code::ERP_CONTRACT_FIELD_LOCKED);
    }

    /// 验收第 8 条：状态流转合法边成功。
    ///
    /// R-P1-1 之后 `DRAFT` 的第一条边变成「提交审批」（`DRAFT → PENDING_APPROVAL`），
    /// 完整链路（提交 → 一级/两级审批 → SIGNED → ACTIVE）由 `contract_approval_tests`
    /// 用真种子跑；这里验「签订之后」那条边：`SIGNED → ACTIVE` 放行 + 写状态日志。
    #[tokio::test]
    async fn status_legal_edge_moves_forward() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = create_ok("MOVE").await;
        force_status(id, "SIGNED").await;

        let active = data(
            contract_change_status(
                Extension(AuthClaims(claims())),
                Path(id.to_string()),
                Json(ContractStatusReq {
                    target_status: Some("ACTIVE".to_string()),
                    ..Default::default()
                }),
            )
            .await
            .expect("SIGNED→ACTIVE"),
        );
        assert_eq!(active.base.status, "ACTIVE");
        assert_eq!(active.base.status_label, "已生效");
        assert!(active.usable_for_order, "已生效可被采购单引用");

        // biz_status_log 必须留下轨迹（状态机的副作用）
        let logged: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_status_log WHERE entity_type = 'CONTRACT' AND entity_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("查状态日志");
        assert_eq!(logged, 1, "一次流转要有一条日志（谁在什么时候改的）");
    }

    /// 验收第 9 条：非法边 → 409 + 66015（卡面写 400 + 66014，已按 A1 的码表更正）。
    #[tokio::test]
    async fn status_illegal_edge_returns_66015() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = create_ok("JUMP").await;

        let err = contract_change_status(
            Extension(AuthClaims(claims())),
            Path(id.to_string()),
            Json(ContractStatusReq {
                target_status: Some("ACTIVE".to_string()), // 跳签：DRAFT→ACTIVE 非法
                ..Default::default()
            }),
        )
        .await
        .expect_err("跳签必须拒");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, error_code::ERP_CONTRACT_STATUS_ILLEGAL);
        assert!(err.msg.contains("SIGNED"), "文案要点名合法去向：{}", err.msg);

        // 数据库里状态没动
        let row = contract_model::contract_find(id).await.expect("查").expect("行在");
        assert_eq!(row.status, "DRAFT", "非法边不得改状态");
    }

    /// 验收第 10 条：需要 reason 的边没带 reason → 400。
    #[tokio::test]
    async fn status_reason_edge_without_reason_returns_400() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = create_ok("REASON").await;
        force_status(id, "SIGNED").await;

        let err = contract_change_status(
            Extension(AuthClaims(claims())),
            Path(id.to_string()),
            Json(ContractStatusReq {
                target_status: Some("TERMINATED".to_string()),
                reason: None,
                status: None,
                ..Default::default()
            }),
        )
        .await
        .expect_err("终止边缺 reason 必须拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("reason"), "文案要点名 reason：{}", err.msg);

        // 补上 reason 就能过
        let terminated = data(
            contract_change_status(
                Extension(AuthClaims(claims())),
                Path(id.to_string()),
                Json(ContractStatusReq {
                    target_status: Some("TERMINATED".to_string()),
                    reason: Some("供应商违约".to_string()),
                    status: None,
                    ..Default::default()
                }),
            )
            .await
            .expect("带 reason 的终止"),
        );
        assert_eq!(terminated.base.status, "TERMINATED");
    }

    /// 验收第 12 条：GET 软删的合同 → 404 + 66014（卡面写 404 + 66013，码值已更正）。
    #[tokio::test]
    async fn detail_of_soft_deleted_returns_66014() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = create_ok("DEL").await;
        let deleted = data(
            contract_delete(Extension(AuthClaims(claims())), Path(id.to_string()))
                .await
                .expect("软删"),
        );
        assert_eq!(deleted.get("deleted").and_then(|value| value.as_bool()), Some(true));

        let err = contract_detail(Path(id.to_string())).await.expect_err("软删后必须 404");
        assert_eq!(err.status, 404);
        assert_eq!(err.code, error_code::ERP_CONTRACT_NOT_USABLE);

        // 再删一次也是 404（幂等语义：不存在就是不存在）
        let again = contract_delete(Extension(AuthClaims(claims())), Path(id.to_string()))
            .await
            .expect_err("重复删必须 404");
        assert_eq!(again.code, error_code::ERP_CONTRACT_NOT_USABLE);
    }

    /// 分页：关键词 / 状态 / 乙方都能筛，软删行不返回，`page_size` 上限 100。
    #[tokio::test]
    async fn page_filters_and_hides_soft_deleted() {
        let Some(_guard) = setup().await else { return };
        let (keep_id, _name) = create_ok("PAGE1").await;
        let (gone_id, _name2) = create_ok("PAGE2").await;
        data(
            contract_delete(Extension(AuthClaims(claims())), Path(gone_id.to_string()))
                .await
                .expect("删掉第二条"),
        );

        let filter = ContractFilter {
            keyword: Some(CONTRACT_PREFIX.to_string()),
            page: 1,
            page_size: 100,
            ..Default::default()
        };
        let page = data(contract_page(Query(filter)).await.expect("分页"));
        assert_eq!(page.total, 1, "软删的那条不该出现");
        assert_eq!(page.list.len(), 1);
        assert_eq!(page.list[0].id, keep_id.to_string());
        assert_eq!(page.page_size, 100);

        // status 过滤
        let draft = ContractFilter {
            status: Some("DRAFT".to_string()),
            keyword: Some(CONTRACT_PREFIX.to_string()),
            page: 1,
            page_size: 20,
            ..Default::default()
        };
        let page = data(contract_page(Query(draft)).await.expect("按状态分页"));
        assert_eq!(page.total, 1);
        assert_eq!(page.list[0].status, "DRAFT");

        // page_size 超上限被 clamp 到 100
        let oversized = ContractFilter {
            page: 0,
            page_size: 500,
            ..Default::default()
        };
        let page = data(contract_page(Query(oversized)).await.expect("超限分页"));
        assert_eq!((page.page, page.page_size), (1, 100), "page/page_size 都要归一化");
    }

    /// 验收第 11 条（防御式）：A3 之前 `biz_purchase_order.contract_id` 不存在 →
    /// 引用检查**跳过**（返回 `None`），删除照常成功。
    ///
    /// 66018 分支本身由纯逻辑用例 `reference_guard_reports_in_use_with_order_numbers` 覆盖；
    /// A3 建好列之后，本用例会自动走「列存在」分支并断言无引用。
    #[tokio::test]
    async fn reference_check_skips_before_a3_column_exists() {
        let Some(_guard) = setup().await else { return };
        let column_exists = contract_model::contract_reference_column_exists(db())
            .await
            .expect("查列");
        let (id, _name) = create_ok("REF").await;
        let refs = contract_model::contract_referencing_orders(db(), id)
            .await
            .expect("查引用");
        if column_exists {
            assert_eq!(refs, Some(Vec::new()), "列已存在（A3 已落地）：无引用");
        } else {
            assert_eq!(refs, None, "A3 未落地：必须跳过而不是 500");
        }
        // 两种情况下删除都应成功（无引用）
        data(
            contract_delete(Extension(AuthClaims(claims())), Path(id.to_string()))
                .await
                .expect("无引用时删除成功"),
        );
    }
}
