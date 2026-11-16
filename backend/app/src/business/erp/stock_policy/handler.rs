//! 水位设置接口层（M2-13）。前缀 `/api` 由 `[router].prefix` 注入，宏内写
//! `/business/erp/stock-policy/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/stock-policy/page` | `business:erp:stock-policy:list` | 分页（物料 / 仓库 / 状态 / 关键字） |
//! | PUT | `/business/erp/stock-policy` | `business:erp:stock-policy:save` | 设置一格水位（按 物料 × 仓库 **upsert**） |
//! | DELETE | `/business/erp/stock-policy/{id}` | `business:erp:stock-policy:delete` | 软删（= 该格回退物料级 `safety_stock`） |
//!
//! # 为什么设置是 upsert 而不是 create + update
//!
//! 「一格一行」的设置，新建与编辑是**同一个动作**（同一份 JSON、同一个唯一键）：拆成两个动词
//! 会逼前端先查「这格有没有行」再决定发 POST 还是 PUT，而两次请求之间没有任何闸能保证
//! 行没被别人建出来 —— 于是必然出现「POST 撞唯一键 409 → 前端再查 → 再 PUT」的补丁流程。
//! 同类先例：现金流底稿 `PUT /business/erp/report/cash-flow/worksheet`（M1-14）也是 upsert 幂等。
//! 因此三个权限码而不是四个（`list` / `save` / `delete`，见 V0428）。
//!
//! # 键不可换
//!
//! `material_id` / `warehouse_id` 决定「改的是哪一格」，是 `WHERE` 条件而不是 `SET` 列
//! （[`model::policy_update`] 里可见）。「把 A 格的设置挪到 B 格」不是编辑，是删 A + 设 B
//! —— 混成一个动作会让 A 格悄悄回退物料级而没人注意。
//!
//! # 本接口**不算结存、不判水位**
//!
//! 它只是设置面：写什么就存什么（校验过域和上下界）；「结存 < 最低水位 → 生成待办」是
//! M2-14、补货建议是 M2-15。所以本文件不读 `biz_inventory`。下游要判水位请调
//! [`model::effective_of`]（三级回退的唯一实现），别在这里加一个「顺手算现存量」的字段。
//!
//! # 错误口径（与 ERP 其余维护面一致）
//!
//! | 场景 | 返回 |
//! |------|------|
//! | 参数非法（非法 UUID / 负数水位 / 最高 ≤ 最低 / 状态域外） | 400 + 10001，点名是哪一项 |
//! | 物料 / 仓库不存在或已删 | 404 + 10002（名单点明是哪一边） |
//! | 该格已被别的设置行占用（并发 upsert 撞唯一键） | 409 + 10003 |
//! | 按 id 改 / 删时行不存在或已删 | 404 + 10002 |

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::stock_policy::dto::{
    self, PolicyFilter, PolicyRes, SavePolicyReq, WaterCheckReq, WaterCheckRes,
};
use crate::business::erp::stock_policy::model::{self, PolicyRow};
use crate::business::erp::stock_policy::water_check;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0428 的 sys_menu.auth_code 逐字一致） ====================

/// 水位设置列表。
pub const PERM_LIST: &str = "business:erp:stock-policy:list";
/// 设置一格水位（upsert；新建与编辑是同一个动作）。
pub const PERM_SAVE: &str = "business:erp:stock-policy:save";
/// 删除一格水位设置（软删 = 回退物料级）。
pub const PERM_DELETE: &str = "business:erp:stock-policy:delete";
/// 执行一次水位检查（M2-14：低于最低水位 → 生成待办 / 回升 → 自动收口）。
///
/// 它是**写**码而不是读码：一次检查会建待办（`biz_task`）也会关待办。
/// 用 `list` 这类读码会让「只能看水位设置」的人顺带改任务表（`authz_tests::o13`
/// 就是拦这件事的）。
pub const PERM_CHECK: &str = "business:erp:stock-policy:check";

/// 本模块全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const STOCK_POLICY_PERMISSIONS: [&str; 4] = [PERM_LIST, PERM_SAVE, PERM_DELETE, PERM_CHECK];

// ==================== 错误出口 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404 + 10002`：物料 / 仓库 / 设置行不存在或已删。
fn not_found(message: impl Into<String>) -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, message.into())
}

/// `409 + 10003`：水位设置唯一键冲突（同物料同仓库只能有一行）。
fn duplicate(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 写路径 DB 错误收口：唯一索引 / CHECK 兜底也翻成人话，其余仍是 500。
fn write_error(error: DbError, ctx: &str) -> ApiError {
    match error {
        DbError::UniqueViolation { constraint, .. } => duplicate(format!(
            "{ctx}：该物料在该仓库已有水位设置{}（同一格只能一行；要改请直接保存，要清除请删除那一行）",
            constraint
                .as_deref()
                .map(|name| format!("（{name}）"))
                .unwrap_or_default()
        )),
        DbError::CheckViolation { constraint, .. } => bad_request(format!(
            "{ctx}：数据不满足约束{}（最低水位 >= 0 / 最高水位 > 最低水位 / 状态 1 或 0）",
            constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
        )),
        other => utils::db_error(other, ctx),
    }
}

/// UUID 入参：非法 → 400（点名是哪个参数）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    Uuid::parse_str(text).map_err(|error| bad_request(format!("{field} 不是合法 UUID：{error}")))
}

/// 可选 UUID（空白 = 不筛）。
fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Ok(Some(parse_uuid(text, field)?)),
    }
}

/// 出参：设置行 → JSON（名字来自 `LEFT JOIN`，软删的主数据为 `null`）。
impl From<&PolicyRow> for PolicyRes {
    fn from(row: &PolicyRow) -> Self {
        Self {
            id: row.id.to_string(),
            material_id: row.material_id.to_string(),
            material_code: row.material_code.clone(),
            material_name: row.material_name.clone(),
            spec: row.spec.clone(),
            unit_name: row.unit_name.clone(),
            warehouse_id: row.warehouse_id.to_string(),
            warehouse_code: row.warehouse_code.clone(),
            warehouse_name: row.warehouse_name.clone(),
            min_qty: row.min_qty,
            max_qty: row.max_qty,
            status: row.status,
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== 编排 ====================

/// 分页列出水位设置。
pub async fn page_policy_of(filter: PolicyFilter) -> std::result::Result<PageData<Vec<PolicyRes>>, ApiError> {
    let material_id = parse_optional_uuid(filter.material_id.as_deref(), "material_id")?;
    let warehouse_id = parse_optional_uuid(filter.warehouse_id.as_deref(), "warehouse_id")?;
    let status = dto::parse_status_filter(filter.status.as_deref()).map_err(bad_request)?;
    let (rows, total) = model::policy_page(
        material_id,
        warehouse_id,
        status,
        filter.keyword.as_deref(),
        filter.page,
        filter.page_size,
    )
    .await
    .map_err(|error| utils::db_error(error, "分页查询水位设置"))?;
    let (page, page_size) = model::normalize_page(filter.page, filter.page_size);
    Ok(PageData {
        list: rows.iter().map(PolicyRes::from).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 设置一格水位（upsert）：不存在则建，存在则覆盖 `min_qty` / `max_qty` / `status` / `remark`。
///
/// 单表单行写入，**不用事务**：一条 `INSERT` 或一条 `UPDATE` 本身就是原子的；
/// 没有「事务提交后还要做别的」的第二步，也就没有约定 11 的补偿问题。
pub async fn save_policy_of(operator: Option<Uuid>, req: SavePolicyReq) -> std::result::Result<PolicyRes, ApiError> {
    let material_id = parse_uuid(&req.material_id, "material_id")?;
    let warehouse_id = parse_uuid(&req.warehouse_id, "warehouse_id")?;
    let write = req.to_write().map_err(bad_request)?;

    if !model::material_exists(material_id)
        .await
        .map_err(|error| utils::db_error(error, "校验物料存在性"))?
    {
        return Err(not_found(format!("物料 {material_id} 不存在或已删除，无法设置水位")));
    }
    if !model::warehouse_exists(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "校验仓库存在性"))?
    {
        return Err(not_found(format!("仓库 {warehouse_id} 不存在或已删除，无法设置水位")));
    }

    let id = match model::policy_find_by_key(material_id, warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询已有水位设置"))?
    {
        Some(existing) => {
            // 改：键不动，只覆盖数值（见模块头注释「键不可换」）
            model::policy_update(existing.id, &write, operator)
                .await
                .map_err(|error| write_error(error, "更新水位设置"))?;
            existing.id
        }
        None => {
            // 建：先查一次唯一键给人话 409（并发下由唯一索引兜底，同一个写法）
            if model::policy_taken(material_id, warehouse_id, None)
                .await
                .map_err(|error| utils::db_error(error, "校验水位设置唯一性"))?
            {
                return Err(duplicate(
                    "该物料在该仓库已有水位设置：同一格只能一行（并发写入时请刷新后重试）",
                ));
            }
            let id = Uuid::now_v7();
            model::policy_insert(id, material_id, warehouse_id, &write, operator)
                .await
                .map_err(|error| write_error(error, "新建水位设置"))?;
            id
        }
    };

    let row = model::policy_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读水位设置"))?
        .ok_or_else(|| not_found("水位设置行不存在或已删除"))?;
    Ok(PolicyRes::from(&row))
}

/// 删除一格水位设置（软删）：该格从此回退 `biz_material.safety_stock`。
pub async fn delete_policy_of(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let affected = model::policy_soft_delete(id, operator)
        .await
        .map_err(|error| utils::db_error(error, "删除水位设置"))?;
    if affected == 0 {
        return Err(not_found("水位设置行不存在或已删除"));
    }
    Ok(())
}

// ==================== 水位检查（M2-14） ====================

/// 执行一次水位检查（手动触发 / 验收 / 补跑）。
///
/// # 语义
///
/// 与定时任务 [`crate::business::erp::stock_policy::water_check::run_background`]
/// **同一个任务体**（[`water_check::run_check`]），只有两点不同：
///
/// 1. 可以限定范围（物料 / 仓库）—— 排障「这一格怎么没生成待办」用；
/// 2. 可以 `dry_run`（只算不写）；
/// 3. **角色下没人时负责人兜底成触发人自己**（source = `OPERATOR`）。理由：手动触发是
///    「我现在就要这件事有人管」，而空库里常常还没配仓管主管角色 —— 那时返回
///    `skipped_no_owner=N` 却一条待办都不建，用户会以为功能坏了。
///
/// 一小时内连点两次不会重复建单（未完成待办唯一索引 + 本轮预读），第二条的计数落在
/// `skipped_open` 里 —— 幂等，不是错误。
///
/// # 异常情况
///
/// - 参数非法（UUID 格式）→ 400 + 10001，点名是哪个字段；
/// - 读库 / 建单失败 → 500（单格失败已计入 `failed`，不会让整轮报错）；
/// - 无 `business:erp:stock-policy:check` → 403（宏层）。
pub async fn check_water_of(
    operator: Option<Uuid>,
    req: WaterCheckReq,
) -> std::result::Result<WaterCheckRes, ApiError> {
    let material_id = parse_optional_uuid(req.material_id.as_deref(), "material_id")?;
    let warehouse_id = parse_optional_uuid(req.warehouse_id.as_deref(), "warehouse_id")?;
    water_check::run_check(material_id, warehouse_id, req.dry_run, operator)
        .await
        .map_err(|error| utils::db_error(error, "执行水位检查"))
}

// ==================== HTTP 入口 ====================

/// 水位设置分页。
#[nik::permission(
    path = "/business/erp/stock-policy/page",
    method = "GET",
    permission = "business:erp:stock-policy:list"
)]
#[nik::get("/business/erp/stock-policy/page")]
pub async fn stock_policy_page_api(Query(filter): Query<PolicyFilter>) -> ResResult<PageData<Vec<PolicyRes>>> {
    Ok(Res::ok(page_policy_of(filter).await?))
}

/// 设置一格水位（按 物料 × 仓库 upsert）。
#[nik::permission(
    path = "/business/erp/stock-policy",
    method = "PUT",
    permission = "business:erp:stock-policy:save"
)]
#[nik::put("/business/erp/stock-policy")]
pub async fn stock_policy_save_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SavePolicyReq>,
) -> ResResult<PolicyRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(save_policy_of(operator, req).await?))
}

/// 删除一格水位设置（软删）。
#[nik::permission(
    path = "/business/erp/stock-policy/{id}",
    method = "DELETE",
    permission = "business:erp:stock-policy:delete"
)]
#[nik::delete("/business/erp/stock-policy/{id}")]
pub async fn stock_policy_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_policy_of(operator, &id).await?;
    Ok(Res::ok(()))
}

/// 执行一次水位检查（M2-14：低于最低水位 → 生成待办 / 回升 → 自动收口）。
///
/// `POST` 而不是 `GET`：它会写 `biz_task`（建待办 / 关待办），幂等性由未完成待办唯一索引
/// 保证（重复点 = `skipped_open` 计数，不是重复建单），但语义是**动作**不是**查询**。
#[nik::permission(
    path = "/business/erp/stock-policy/check",
    method = "POST",
    permission = "business:erp:stock-policy:check"
)]
#[nik::post("/business/erp/stock-policy/check")]
pub async fn stock_policy_check_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<WaterCheckReq>,
) -> ResResult<WaterCheckRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(check_water_of(operator, req).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 权限码与 V0428 / V0429 逐字一致；四个码互不相同（少一个就等于少一个开关）。
    #[test]
    fn permission_codes_match_migration() {
        assert_eq!(
            STOCK_POLICY_PERMISSIONS,
            [PERM_LIST, PERM_SAVE, PERM_DELETE, PERM_CHECK]
        );
        assert_eq!(PERM_LIST, "business:erp:stock-policy:list");
        assert_eq!(PERM_SAVE, "business:erp:stock-policy:save");
        assert_eq!(PERM_DELETE, "business:erp:stock-policy:delete");
        assert_eq!(PERM_CHECK, "business:erp:stock-policy:check");
        assert_ne!(PERM_LIST, PERM_SAVE);
        assert_ne!(PERM_SAVE, PERM_DELETE);
        assert_ne!(PERM_LIST, PERM_DELETE);
        assert_ne!(PERM_CHECK, PERM_LIST);
        assert_ne!(PERM_CHECK, PERM_SAVE);
        assert_ne!(PERM_CHECK, PERM_DELETE);
        println!("[M2-13/M2-14] 权限码 list / save / delete / check（四码互不相同）✓");
    }

    /// 非法 UUID / 空 id 必须 400 + 10001，且文案点名字段（不静默当作没传）。
    #[test]
    fn invalid_uuid_is_400_and_names_the_field() {
        let error = parse_uuid("not-a-uuid", "material_id").expect_err("非法 UUID 必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("material_id"), "文案要点名字段：{}", error.msg);

        let error = parse_uuid("   ", "warehouse_id").expect_err("空值必须报错");
        assert!(error.msg.contains("warehouse_id"), "{}", error.msg);

        // 可选：空白 = 不筛（不是 400，也不是 `= ''` 把结果清空）
        assert!(
            parse_optional_uuid(Some("  "), "material_id")
                .expect("空白不报错")
                .is_none()
        );
        assert!(parse_optional_uuid(None, "material_id").expect("缺省不报错").is_none());
        println!("[M2-13] 非法/空 UUID → 400 + 10001 并点名字段；空白可选参数 = 不筛 ✓");
    }

    /// 唯一键冲突 / CHECK 兜底都翻成人话（409 / 400），不是 500。
    #[test]
    fn db_error_mapping_is_human_readable() {
        let conflict = write_error(
            DbError::UniqueViolation {
                column: None,
                constraint: Some("uk_biz_msp_mat_wh".to_string()),
                source: None,
            },
            "新建水位设置",
        );
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.code, error_code::RESOURCE_CONFLICT);
        assert!(conflict.msg.contains("uk_biz_msp_mat_wh"), "{}", conflict.msg);

        let check = write_error(
            DbError::CheckViolation {
                constraint: Some("ck_biz_msp_max".to_string()),
                source: None,
            },
            "更新水位设置",
        );
        assert_eq!(check.status, 400);
        assert!(check.msg.contains("ck_biz_msp_max"), "{}", check.msg);
        println!("[M2-13] 唯一键 → 409 / CHECK → 400，均点名约束（不落成 500）✓");
    }
}
