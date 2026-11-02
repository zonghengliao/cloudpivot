//! 合同接口层（**A2**）：6 个接口，路径与权限注解由 A1 挂好，本卡只填函数体。
//!
//! | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/contract/page` | `erp:contract:list` | 分页（软删行不返回） |
//! | GET | `/business/erp/contract/{id}` | `erp:contract:list` | 详情（含 `can_edit` / `usable_for_order` / `next_statuses`） |
//! | POST | `/business/erp/contract` | `erp:contract:create` | 建单（`DRAFT` + `HT-yyyyMMdd-000001`） |
//! | PUT | `/business/erp/contract/{id}` | `erp:contract:update` | 改单（**仅 `DRAFT`**） |
//! | DELETE | `/business/erp/contract/{id}` | `erp:contract:delete` | 软删（引用守卫） |
//! | PATCH | `/business/erp/contract/{id}/status` | `erp:contract:status` | 状态流转（状态机唯一入口） |
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
//! | 客户类合同未开放 | `66020 ERP_CONTRACT_CUSTOMER_UNSUPPORTED` | 400 |
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
//! ③ **乙方名称是快照**：新建 / 换乙方时由服务端从 `biz_supplier` 读，客户端传什么都不算数。
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
use axum::extract::{Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::contract_dto::{
    ContractDetailRes, ContractFilter, ContractRes, ContractStatusReq, CreateContractReq,
    SUPPORTED_TYPES, UpdateContractReq, normalize_page, parse_optional_uuid, parse_uuid_text,
};
use crate::business::erp::contract_model::{self, ContractMut, STATUS_DRAFT};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

/// 合同列表 / 详情（`sys_menu` 里是 menu_type=2 的菜单节点）。
pub const PERM_LIST: &str = "erp:contract:list";
/// 新建合同。
pub const PERM_CREATE: &str = "erp:contract:create";
/// 编辑合同（A2 按 §5.3 冻结矩阵校验）。
pub const PERM_UPDATE: &str = "erp:contract:update";
/// 删除合同（软删，V-21 引用守卫）。
pub const PERM_DELETE: &str = "erp:contract:delete";
/// 状态流转（`state_machine::transition(Entity::Contract, …)` 的唯一入口）。
pub const PERM_STATUS: &str = "erp:contract:status";

/// 合同域全部权限码（顺序与 `V0197` 的 `auth_code` 一致，也是「权限码唯一真相」）。
///
/// 与迁移**逐字对应**：`V0197__erp_contract_permissions.up.sql` 的 5 条 `sys_menu` 行
/// 就是这个数组。任一侧改动而另一侧没跟，`contract_permissions_match_menu` 用例会红。
pub const PERMISSIONS: [&str; 5] = [
    PERM_LIST,
    PERM_CREATE,
    PERM_UPDATE,
    PERM_DELETE,
    PERM_STATUS,
];

/// 权限码的模块前缀（`{模块}:{资源}:{动作}` 的第一段）。
pub const PERMISSION_MODULE: &str = "erp";
/// 权限码的资源段（第二段）。
pub const PERMISSION_RESOURCE: &str = "contract";

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
fn reference_guard(
    contract_no: &str,
    referencing: Option<&[String]>,
) -> std::result::Result<(), ApiError> {
    match referencing {
        Some([]) | None => Ok(()),
        Some(order_nos) => Err(in_use(contract_no, order_nos)),
    }
}

/// 乙方不存在 / 已停用（400 + `66021`）。
fn party_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_CONTRACT_PARTY_INVALID, message.into())
}

/// 客户类合同未开放（400 + `66020`）。
fn customer_unsupported() -> ApiError {
    ApiError::new(
        400,
        error_code::ERP_CONTRACT_CUSTOMER_UNSUPPORTED,
        "客户类合同（SALES / 乙方类型 CUSTOMER）本批次未开放：客户主数据属批次 2，\
         上线后 party_b_ref 会自动支持（ARCH-2 §3.2.3 R2）",
    )
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
                format!(
                    "{ctx} 被数据库约束拒绝：字段组合不合法（amount>0 / 日期顺序 / 状态白名单）"
                ),
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
    Ok(Res::ok(ContractDetailRes::from(&row)))
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
/// - 403：无 `erp:contract:list`；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/erp/contract/page",
    method = "GET",
    permission = "erp:contract:list"
)]
#[nik::get("/business/erp/contract/page")]
pub async fn contract_page(
    Query(filter): Query<ContractFilter>,
) -> ResResult<PageData<Vec<ContractRes>>> {
    let query = filter.parse()?;
    let (page, page_size) = normalize_page(query.page, query.page_size);
    let (rows, total) = contract_model::contract_page(&query)
        .await
        .map_err(|error| utils::db_error(error, "分页查询合同"))?;
    let list = rows.iter().map(ContractRes::from).collect();
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
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
    permission = "erp:contract:list"
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
/// 1. 纯入参校验（必填 / 长度 / 枚举 / 金额 > 0 / 日期顺序 / 签订 ≤ 生效）；
/// 2. 类型闸：本期只放 `PURCHASE`，`SALES` → `66020`，其余 → 400；
/// 3. 乙方解析：`party_b_ref_for_new`（要求 `status = 1`）→ 无 → `66021`；
/// 4. 同一事务里取号 + 插入（取号与插入必须同事务，否则回滚会吃掉一个号，
///    虽然号码不要求连续，但少一个号会让对账人多问一句）。
///
/// # 异常情况
///
/// - 400 + `10001`：入参不合法；400 + `66020`：客户类合同；400 + `66021`：乙方无效；
/// - 409 + `66017`：合同号重复（唯一索引兜底）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/erp/contract",
    method = "POST",
    permission = "erp:contract:create"
)]
#[nik::post("/business/erp/contract")]
pub async fn contract_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateContractReq>,
) -> ResResult<ContractDetailRes> {
    req.validate()?;

    // 类型闸：本期只开放 PURCHASE（卡面）；SALES 的乙方是客户 → 66020
    let contract_type = req.contract_type_value();
    if contract_type == "SALES" {
        return Err(customer_unsupported());
    }
    if !SUPPORTED_TYPES.contains(&contract_type) {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "contract_type `{contract_type}` 本批次未开放：只支持 {}",
                SUPPORTED_TYPES.join(" / ")
            ),
        ));
    }

    let party_b_type = req.resolve_party_b_type()?;
    if party_b_type == contract_model::PARTY_B_CUSTOMER {
        return Err(customer_unsupported());
    }
    let party_b_id = parse_uuid_text(req.party_b_id.as_deref().unwrap_or_default(), "party_b_id")?;
    // V-11：新建要求供应商存在、未软删、且启用中（停用只拦新签，不拦改备注）
    let party_b_name = contract_model::party_b_ref_for_new(party_b_type, party_b_id)
        .await
        .map_err(|error| utils::db_error(error, "校验合同乙方"))?
        .ok_or_else(|| {
            party_invalid(format!(
                "乙方供应商 {party_b_id} 不存在、已删除或已停用（V-11：停用供应商不得新签合同）"
            ))
        })?;

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
/// # 并发
///
/// 事务内 `SELECT … FOR UPDATE` 锁行 → 合并 → 校验 → 带 `WHERE status='DRAFT'` 整写，
/// 第二道闸防「读到 DRAFT 之后被别人流转掉」。
///
/// # 异常情况
///
/// - 400 + `10001`：入参不合法 / 合并后的日期区间反了；400 + `66021`：换乙方但乙方无效；
/// - 404 + `66014`：不存在或已删；409 + `66016`：非 `DRAFT`；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/erp/contract/{id}",
    method = "PUT",
    permission = "erp:contract:update"
)]
#[nik::put("/business/erp/contract/{id}")]
pub async fn contract_update(
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
    let (party_b_id, party_b_name) = match req
        .party_b_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => (current.party_b_id, current.party_b_name.clone()),
        Some(raw) => {
            let new_id = parse_uuid_text(raw, "party_b_id")?;
            if new_id == current.party_b_id {
                (current.party_b_id, current.party_b_name.clone())
            } else {
                let name = contract_model::party_b_ref(&current.party_b_type, new_id)
                    .await
                    .map_err(|error| utils::db_error(error, "校验合同乙方"))?
                    .ok_or_else(|| party_invalid(format!("乙方供应商 {new_id} 不存在或已删除")))?;
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
            format!(
                "amount 必须大于 0（当前 {}）：ck_biz_contract_amount",
                merged.amount
            ),
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

/// 合同状态流转（`DRAFT→SIGNED→ACTIVE→COMPLETED`，另有 `SIGNED/ACTIVE→TERMINATED`）。
///
/// # 边从哪来
///
/// **唯一来源是状态机**：`Entity::Contract.next_statuses()` / `can_transition()`
/// 判定，`state_machine::transition()` 复核并写 `biz_status_log`。handler 里没有 if-else 边表。
///
/// # 顺序
///
/// 1. 目标态白名单（5 态；卡面的 `CANCELLED` 不存在 → 400 并点名可用态）；
/// 2. 事务 + `FOR UPDATE` 锁行，取当前态；
/// 3. 边判定（非法 → 409 + `66015`，文案带「当前态允许去哪」）；
/// 4. DDL 前置：目标态需要日期三件套（`ck_biz_contract_signed_dates`）→ 缺 → 400；
/// 5. `transition()`（需要 reason 的边缺 reason → 它自己报 400）；
/// 6. 带 CAS 的 `UPDATE status`，0 行 → 409。
///
/// # 异常情况
///
/// - 400 + `10001`：目标态非法 / 缺 reason / 缺日期；404 + `66014`：合同不存在；
/// - 409 + `66015`：非法边或并发冲突；500：数据库异常。
#[nik::permission(
    path = "/business/erp/contract/{id}/status",
    method = "PATCH",
    permission = "erp:contract:status"
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
        let message =
            state_machine::rejection_message(state_machine::ENTITY_CONTRACT, &row.status, &target);
        let _ = tx.rollback().await;
        return Err(illegal_transition(message));
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

    // ⑥ 带 CAS 的写状态
    let affected = match contract_model::contract_write_status(
        &tx,
        id,
        &row.status,
        &target,
        operator,
    )
    .await
    {
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
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交合同状态流转事务"))?;

    detail_of(id).await
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
    permission = "erp:contract:delete"
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
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交删除合同事务"))?;

    Ok(Res::ok(serde_json::json!({
        "id": id.to_string(),
        "contract_no": row.contract_no,
        "deleted": true,
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

    // ---------- 纯逻辑用例（无需数据库） ----------

    /// ① 权限码：5 个、不重复、`erp:contract:*` 三段式。
    #[test]
    fn contract_permission_codes_are_well_formed() {
        assert_eq!(PERMISSIONS.len(), 5, "ARCH-2 附录 B #7 定了 5 个码");
        let unique: std::collections::BTreeSet<&str> = PERMISSIONS.iter().copied().collect();
        assert_eq!(unique.len(), PERMISSIONS.len(), "权限码不得重复");

        for code in PERMISSIONS {
            assert_eq!(
                code.matches(':').count(),
                2,
                "{code} 必须是 {{模块}}:{{资源}}:{{动作}} 三段式（N15 只认 ≥2 个冒号）"
            );
            let mut parts = code.split(':');
            assert_eq!(parts.next(), Some(PERMISSION_MODULE), "{code} 的模块段不对");
            assert_eq!(
                parts.next(),
                Some(PERMISSION_RESOURCE),
                "{code} 的资源段不对"
            );
            let action = parts.next().unwrap_or_default();
            assert!(!action.is_empty(), "{code} 缺动作段");
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

    /// ② 常量名与码值一一对应。
    #[test]
    fn contract_permission_constants_match_list() {
        assert_eq!(PERM_LIST, "erp:contract:list");
        assert_eq!(PERM_CREATE, "erp:contract:create");
        assert_eq!(PERM_UPDATE, "erp:contract:update");
        assert_eq!(PERM_DELETE, "erp:contract:delete");
        assert_eq!(PERM_STATUS, "erp:contract:status");
    }

    /// ③ 合同边表与状态机注册表逐条一致（A1 改了注册表、A2 没跟上 → 立刻红）。
    #[test]
    fn legal_edges_match_state_machine() {
        assert!(
            contract_model::legal_edges_match_registry(),
            "contract_model::LEGAL_EDGES 与 state_machine::CONTRACT_EDGES 不一致（transition 会 500）"
        );
        assert_eq!(contract_model::LEGAL_EDGES.len(), 5);
        let entity = state_machine::Entity::Contract;
        for (from, to) in contract_model::LEGAL_EDGES {
            assert!(
                entity.can_transition(from, to),
                "{from}→{to} 在注册表里不合法"
            );
        }
        // 卡面写的 DRAFT→ACTIVE（跳过签订）必须是非法边
        assert!(!entity.can_transition("DRAFT", "ACTIVE"));
        assert!(
            !entity.can_transition("DRAFT", "CANCELLED"),
            "没有 CANCELLED 态"
        );
    }

    /// ④ 新建缺必填 → 400（卡面验收第 2 条；validate 在连库之前就拦下）。
    #[tokio::test]
    async fn create_without_required_fields_returns_400() {
        let err = contract_create(
            Extension(AuthClaims(claims())),
            Json(CreateContractReq::default()),
        )
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
    /// 多种一个码 → 管理面能勾一个永不生效的权限。A1 定的 5 个码，A2 一个都没动。
    #[tokio::test]
    async fn contract_permissions_match_menu() {
        let Some(_guard) = setup().await else { return };
        let rows = db()
            .find_with_params(
                "SELECT auth_code FROM sys_menu \
                  WHERE auth_code LIKE 'erp:contract:%' AND deleted_at IS NULL AND status = 1",
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
            "sys_menu.auth_code 必须与 crate 常量逐字一致（V0197 迁移 + 本文件要同步改）"
        );
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
        db().execute_with_params(
            "DELETE FROM biz_contract WHERE contract_name LIKE 'A2C-%'",
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("清理合同失败：{err}"));
        db().execute_with_params(
            "DELETE FROM biz_supplier WHERE supplier_code LIKE 'A2CM-%'",
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("清理供应商失败：{err}"));
    }

    /// 造一个供应商（`status = 1` 可新签）。
    async fn make_supplier(tag: &str) -> (Uuid, String) {
        let id = Uuid::now_v7();
        let name = format!("A2 合同乙方 {tag}");
        let code = format!("{PREFIX}{tag}-{}", &id.simple().to_string()[..8]);
        db().execute_with_params(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) \
             VALUES (?::uuid, ?, ?, 1)",
            vec![
                Value::from(&id),
                Value::Text(code),
                Value::Text(name.clone()),
            ],
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
            contract_create(
                Extension(AuthClaims(claims())),
                Json(create_req(tag, supplier_id)),
            )
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

    /// 验收第 1 条：新建成功 → 拿到 id + HT 号，状态 DRAFT，乙方名称是服务端快照。
    #[tokio::test]
    async fn create_returns_draft_with_ht_number() {
        let Some(_guard) = setup().await else { return };
        let (supplier_id, supplier_name) = make_supplier("OK").await;
        let detail = data(
            contract_create(
                Extension(AuthClaims(claims())),
                Json(create_req("NEW", supplier_id)),
            )
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
        assert_eq!(detail.next_statuses, vec!["SIGNED".to_string()]);
    }

    /// 验收第 3 条：party_b_id 不存在 → 400 + 66021。
    #[tokio::test]
    async fn create_with_unknown_party_b_returns_66021() {
        let Some(_guard) = setup().await else { return };
        let ghost = Uuid::now_v7();
        let err = contract_create(
            Extension(AuthClaims(claims())),
            Json(create_req("GHOST", ghost)),
        )
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
        assert_eq!(
            detail.base.party_b_name, other_name,
            "换乙方后快照必须是新主档的名字"
        );
        assert_eq!(detail.remark.as_deref(), Some("改后备注"));
        assert_eq!(detail.base.status, "DRAFT", "改单不动状态");
    }

    /// 验收第 7 条：修改非 DRAFT → 409 + 66016（卡面写 400 + 66014，已按 A1 的码表更正）。
    #[tokio::test]
    async fn update_non_draft_returns_66016() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = create_ok("LOCK").await;
        // 先流转到 SIGNED（合法的第一条边）
        data(
            contract_change_status(
                Extension(AuthClaims(claims())),
                Path(id.to_string()),
                Json(ContractStatusReq {
                    target_status: Some("SIGNED".to_string()),
                    reason: None,
                    status: None,
                }),
            )
            .await
            .expect("DRAFT→SIGNED"),
        );

        let err = contract_update(
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

    /// 验收第 8 条：状态流转合法边成功（DRAFT → SIGNED；卡面的 DRAFT→ACTIVE 不是合法边）。
    #[tokio::test]
    async fn status_legal_edge_moves_forward() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = create_ok("MOVE").await;

        let signed = data(
            contract_change_status(
                Extension(AuthClaims(claims())),
                Path(id.to_string()),
                Json(ContractStatusReq {
                    target_status: Some("SIGNED".to_string()),
                    ..Default::default()
                }),
            )
            .await
            .expect("DRAFT→SIGNED"),
        );
        assert_eq!(signed.base.status, "SIGNED");
        assert_eq!(signed.base.status_label, "已签订");
        assert!(signed.usable_for_order, "SIGNED 可被采购单引用");
        assert!(!signed.can_edit, "非草稿不可改");

        // 继续走 SIGNED → ACTIVE
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

        // biz_status_log 必须留下两条轨迹（状态机的副作用）
        let logged: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_status_log WHERE entity_type = 'CONTRACT' AND entity_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("查状态日志");
        assert_eq!(logged, 2, "两次流转要有两条日志（谁在什么时候改的）");
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
        assert!(
            err.msg.contains("SIGNED"),
            "文案要点名合法去向：{}",
            err.msg
        );

        // 数据库里状态没动
        let row = contract_model::contract_find(id)
            .await
            .expect("查")
            .expect("行在");
        assert_eq!(row.status, "DRAFT", "非法边不得改状态");
    }

    /// 验收第 10 条：需要 reason 的边没带 reason → 400。
    #[tokio::test]
    async fn status_reason_edge_without_reason_returns_400() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = create_ok("REASON").await;
        data(
            contract_change_status(
                Extension(AuthClaims(claims())),
                Path(id.to_string()),
                Json(ContractStatusReq {
                    target_status: Some("SIGNED".to_string()),
                    ..Default::default()
                }),
            )
            .await
            .expect("DRAFT→SIGNED"),
        );

        let err = contract_change_status(
            Extension(AuthClaims(claims())),
            Path(id.to_string()),
            Json(ContractStatusReq {
                target_status: Some("TERMINATED".to_string()),
                reason: None,
                status: None,
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
        assert_eq!(
            deleted.get("deleted").and_then(|value| value.as_bool()),
            Some(true)
        );

        let err = contract_detail(Path(id.to_string()))
            .await
            .expect_err("软删后必须 404");
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
        assert_eq!(
            (page.page, page.page_size),
            (1, 100),
            "page/page_size 都要归一化"
        );
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
