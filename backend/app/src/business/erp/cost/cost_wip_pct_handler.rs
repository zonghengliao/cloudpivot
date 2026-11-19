//! 在制品「完工率录入（按工序）」· 接口层（成本核算 · M5-21）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-object/{id}/operations`
//! （AGENTS 约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-object/{id}/operations` | `business:erp:cost-object:list` | 该批的工序完工率 + 加权汇总 + 状态闸 |
//! | 2 | PUT | `/business/erp/cost-object/{id}/operations` | `business:erp:cost-object:update` | 整表保存工序（按 seq 对齐，缺的软删），同事务回写对象级完工率 |
//!
//! # 为什么只有两条路由
//!
//! * 没有 POST / DELETE / PATCH 单行接口：录入面是**整张工序表**，payload 里有几行就是几行
//!   （少一行 = 少一道工序）。逐行接口会让「删一道工序」与「漏传一行」在服务端无法区分。
//! * 不返回「试算」：加权平均是一行公式，回单里已经带 `rollup`（保存前想看，GET 一次就有）。
//! * 没有「按期间」参数：对象级完工率是**当前快照**，详见 `cost_wip_pct_dto` 模块头。
//!
//! # 权限码：复用 `cost-object:list` / `cost-object:update`（零新增码、零新增菜单）
//!
//! 与 `cost-object`（B2-14 的 `PUT …/output`）同一套码：产量 / 在制数量是「干了多少件」，
//! 完工率是「干到哪一步」，同一张 `biz_cost_object`、同一批人维护。新开 `cost-wip:*` 会带来
//! 两个问题：① 没有菜单行承载 ⇒ 死码（`permission_audit_tests::n14` 判红）；② 先插一条指向
//! 不存在组件的菜单行 ⇒ `n08` 判红。页面码 / 接口码解耦（PERM-01）只在 HR 试点，成本域另开卡。
//!
//! # 事务与补偿（AGENTS 约定 11）
//!
//! 保存是一个事务：工序行增 / 改 / 软删 + 对象级完工率回写 + 变更日志，任一步失败整笔回滚
//! （工序表与对象级要么一起变、要么一起不变）。**提交后只做只读回查**，不会留下
//! 「工序说 50%、对象级说 80%」的中间态，也没有「提交后失败」的形态要补偿。
//!
//! # 状态闸（409，不是 403）
//!
//! * 对象非 `OPEN`（已关批 / 已作废）⇒ 409；
//! * 该批参与过的成本批次已结算（`SETTLED` / `CLOSED`）⇒ 409 + `66070`
//!   （与 `PUT …/output` 同码：完工率已经进了计算单，再改就是改写已入账成本）。
//!
//! 这两件事都**不是权限问题**：账号有 `update` 码，只是这张批现在不许改。
//!
//! # 数据权限
//!
//! 两条路由都先做**对象探针**（[`model::find_visible_object`]，按 `workshop_dept_id` 裁剪）：
//! 看不见的批 ⇒ 404（不是 403 —— 单条入口也不当「探测口」）。写入路径在事务里再用
//! [`model::lock_object`] 锁行重读，防「保存期间被别人关批」。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_wip_equiv_handler` / `cost_object_handler` 同款：axum handler 要 `AuthClaims`
//! 才能跑，单测调不到；把「探针 + 归一化 + 汇总 + 落库 + 回查」收进 [`operations_of`] /
//! [`save_operations`] 之后，真库用例走**同一条代码路径**（只跳过认证）。

use axum::Json;
use axum::extract::Path;
use axum::http::HeaderMap;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::changelog::{self, Action as ChangeAction, ChangeTarget, DiffInput};
use crate::business::erp::cost::cost_object_dto;
use crate::business::erp::cost::cost_object_model;
use crate::business::erp::cost::cost_wip_pct_dto::{self as dto, OperationRes, OperationSaveReq};
use crate::business::erp::cost::cost_wip_pct_model::{self as model, ObjectHead, StoredRow};
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码 ====================

/// 读（**复用** `cost-object:list`，理由见模块头）。
pub const PERM_READ: &str = dto::PERM_READ;

/// 写（**复用** `cost-object:update`，理由见模块头）。
pub const PERM_WRITE: &str = dto::PERM_WRITE;

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形）。
pub const WIP_PCT_PERMISSIONS: [&str; 2] = dto::PERMISSIONS;

// ==================== 错误出口 ====================

fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("成本对象不存在、已删除，或不在你的数据范围内（{id}）"),
    )
}

/// 期间已结算（与 `cost_object_handler::set_output` 同码同文案口径）。
fn period_locked(object_no: &str, settled: &[(String, String)]) -> ApiError {
    let list = settled
        .iter()
        .map(|(period, status)| format!("{period}({status})"))
        .collect::<Vec<_>>()
        .join("、");
    ApiError::new(
        409,
        cost_object_dto::PERIOD_LOCKED_CODE,
        format!(
            "成本对象 {object_no} 的完工率已锁定：以下成本批次已结算（{list}），\
             完工率已经进了计算单与约当产量，再改就是改写已入账成本。\
             如需更正，请先红冲该期间的成本批次，再回来改"
        ),
    )
}

// ==================== 读：工序完工率 ====================

/// 读该批的工序完工率（探针 → 状态闸 → 工序行 → 汇总 → 出参）。
pub async fn operations_of(principal: &ScopedPrincipal, id_raw: &str) -> std::result::Result<OperationRes, ApiError> {
    let id = cost_object_dto::parse_uuid(id_raw, "id")?;
    let head = visible_head(principal, id).await?;
    let settled = cost_object_model::settled_periods(id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本对象的已结算期间"))?;
    let gate = dto::gate_of(&head.status, &settled);
    let rows = model::rows_of(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询工序完工率"))?;
    Ok(build_res(&head, &gate, &rows, false, Counts::default()))
}

/// 对象探针（数据范围 → 404）。
async fn visible_head(principal: &ScopedPrincipal, id: Uuid) -> std::result::Result<ObjectHead, ApiError> {
    model::find_visible_object(principal, id)
        .await
        .map_err(|error| utils::db_error(error, "查询待录完工率的成本对象"))?
        .ok_or_else(|| not_found(id))
}

// ==================== 写：整表保存 ====================

/// 本次保存的写入计数（出参用）。
#[derive(Debug, Clone, Copy, Default)]
struct Counts {
    inserted: usize,
    updated: usize,
    deleted: usize,
}

impl Counts {
    fn changed(&self) -> bool {
        self.inserted + self.updated + self.deleted > 0
    }
}

/// 整表保存工序（**事务**：工序行增 / 改 / 软删 + 对象级完工率回写 + 变更日志）。
///
/// # 幂等
///
/// 同一份 payload 再保存一次：行内容逐字段相同 ⇒ 不写库、不写审计、`changed = false`
/// （`updated_at` 不会因为「点了两次保存」而变，审计里也不会出现空 diff）。
///
/// # 空 payload
///
/// `rows: []` = **清空工序**（全部软删）。对象级完工率**不重算**（保持最后的值）：
/// 清空是「回到对象级手填模式」，此时若把它归零，在制 > 0 的批会被 `66067` 直接拦在计算门口。
/// 报文 `note` 会把这件事写清楚。
pub async fn save_operations(
    principal: &ScopedPrincipal,
    ctx: &changelog::ChangeCtx<'_>,
    id_raw: &str,
    req: OperationSaveReq,
) -> std::result::Result<OperationRes, ApiError> {
    let id = cost_object_dto::parse_uuid(id_raw, "id")?;
    let specs = dto::specs_of(&req)?;
    let rollup = dto::rollup_of(&specs);

    // ① 探针（数据范围 → 404）+ 状态闸（409）
    let head = visible_head(principal, id).await?;
    let settled = cost_object_model::settled_periods(id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本对象的已结算期间"))?;
    let gate = dto::gate_of(&head.status, &settled);
    if let dto::EditGate::Locked(reason) = &gate {
        return Err(match head.status != cost_object_dto::STATUS_OPEN {
            true => dto::conflict(reason.clone()),
            false => period_locked(&head.object_no, &settled),
        });
    }

    // ② 「在制 > 0 但完工率 0」在录入这一步就拦下（与 calc 66067 / 产量录入同一判据）
    if let Some(rollup) = rollup.as_ref()
        && head.wip_qty > Decimal::ZERO
        && rollup.pct == Decimal::ZERO
    {
        return Err(dto::bad_request(format!(
            "期末在制 {} 但工序加权完工率是 0%：跑期间计算会被 66067 拦下\
             （「期末在制 > 0 但完工进度为 0」）。请把已开工工序的进度填上再保存",
            head.wip_qty
        )));
    }

    // ③ 一个事务：锁行重读 → 逐 seq 增 / 改 → 缺的软删 → 回写对象级 → 变更日志
    let operator = ctx.operator_id;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启工序完工率保存事务"))?;
    let outcome = write_in_tx(&tx, ctx, id, &head.object_no, &specs, rollup.as_ref(), operator).await;
    let counts = match outcome {
        Ok(counts) => counts,
        Err(error) => {
            if let Err(rollback) = tx.rollback().await {
                tracing::error!(error = %rollback, "工序完工率保存回滚失败");
            }
            return Err(error);
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交工序完工率保存事务"))?;

    // ④ 回查（不信内存）：工序行与对象级都从库里读回来
    let head = model::find_visible_object(principal, id)
        .await
        .map_err(|error| utils::db_error(error, "回查成本对象"))?
        .ok_or_else(|| not_found(id))?;
    let settled = cost_object_model::settled_periods(id)
        .await
        .map_err(|error| utils::db_error(error, "回查已结算期间"))?;
    let gate = dto::gate_of(&head.status, &settled);
    let rows = model::rows_of(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "回查工序完工率"))?;
    Ok(build_res(&head, &gate, &rows, counts.changed(), counts))
}

/// 事务体（单独一段，方便失败时统一回滚）。
async fn write_in_tx(
    tx: &Transaction,
    ctx: &changelog::ChangeCtx<'_>,
    object_id: Uuid,
    object_no: &str,
    specs: &[dto::OperationSpec],
    rollup: Option<&dto::Rollup>,
    operator: Option<Uuid>,
) -> std::result::Result<Counts, ApiError> {
    // ① 锁对象行重读（SELECT … FOR UPDATE）：串行化同一个批的并发保存
    let locked = model::lock_object(tx, object_id)
        .await
        .map_err(|error| utils::db_error(error, "锁定成本对象"))?
        .ok_or_else(|| not_found(object_id))?;
    if locked.status != cost_object_dto::STATUS_OPEN {
        return Err(dto::conflict(format!(
            "成本对象 {} 的状态在本次保存期间被改动（可能刚被关批 / 作废），请刷新后重试",
            locked.object_no
        )));
    }

    // ② 现有工序行按 seq 建索引（整表覆盖的对齐基准）
    let existing = model::rows_of(tx, object_id)
        .await
        .map_err(|error| utils::db_error(error, "读取现有工序行"))?;
    let mut by_seq: std::collections::BTreeMap<i32, StoredRow> =
        existing.into_iter().map(|row| (row.spec.seq, row)).collect();
    let mut counts = Counts::default();

    // ③ payload 里有几行就是几行：同 seq 更新 / 新 seq 插入
    for spec in specs {
        match by_seq.remove(&spec.seq) {
            Some(row) if row.spec == *spec => {}
            Some(row) => {
                let affected = model::update_row(tx, object_id, row.id, spec, operator)
                    .await
                    .map_err(|error| utils::db_error(error, "更新工序行"))?;
                if affected == 0 {
                    return Err(dto::conflict(
                        "工序行在本次保存期间被删除，请刷新后重试（同一批的并发保存已由行锁串行化，\
                         出现这条说明有绕过接口的写入）",
                    ));
                }
                record_row(
                    tx,
                    ctx,
                    row.id,
                    ChangeAction::Update,
                    Some(&spec_json(&row.spec)),
                    Some(&spec_json(spec)),
                )
                .await?;
                counts.updated += 1;
            }
            None => {
                let id = model::insert_row(tx, object_id, spec, operator)
                    .await
                    .map_err(|error| utils::db_error(error, "新增工序行"))?;
                record_row(tx, ctx, id, ChangeAction::Create, None, Some(&spec_json(spec))).await?;
                counts.inserted += 1;
            }
        }
    }

    // ④ 库里剩下（payload 没给的 seq）= 用户删掉的工序 → 软删
    for (_, row) in by_seq {
        let affected = model::soft_delete_row(tx, object_id, row.id, operator)
            .await
            .map_err(|error| utils::db_error(error, "删除工序行"))?;
        if affected == 0 {
            continue; // 已被并发软删：结果与本次意图相同，不报错
        }
        record_row(tx, ctx, row.id, ChangeAction::Delete, Some(&spec_json(&row.spec)), None).await?;
        counts.deleted += 1;
    }

    // ⑤ 对象级完工率 = 加权汇总（空 payload 不重算，见函数文档）
    if let Some(rollup) = rollup
        && rollup.pct != locked.completion_pct
    {
        let affected = model::apply_object_pct(tx, object_id, rollup.pct, operator)
            .await
            .map_err(|error| utils::db_error(error, "回写对象级完工率"))?;
        if affected == 0 {
            return Err(dto::conflict(format!(
                "成本对象 {object_no} 的状态在本次保存期间被改动（可能刚被关批），请刷新后重试"
            )));
        }
        let before = serde_json::json!({ "completion_pct": locked.completion_pct.to_string() });
        let after = serde_json::json!({ "completion_pct": rollup.pct.to_string() });
        changelog::record_in(
            tx,
            ctx,
            ChangeTarget::of(
                cost_object_model::COST_OBJECT_TABLE,
                object_id.to_string(),
                ChangeAction::Update,
                dto::ACTION_LABEL,
            ),
            DiffInput::update(&before, &after),
        )
        .await
        .map_err(|error| utils::db_error(error, "写对象级完工率变更日志"))?;
    }

    Ok(counts)
}

/// 写一条工序行变更日志（表名已登记 `changelog::diff::REGISTRY`，未登记会 500 —— 接入缺陷）。
async fn record_row(
    tx: &Transaction,
    ctx: &changelog::ChangeCtx<'_>,
    row_id: Uuid,
    action: ChangeAction,
    before: Option<&serde_json::Value>,
    after: Option<&serde_json::Value>,
) -> std::result::Result<(), ApiError> {
    let input = match (before, after) {
        (Some(before), Some(after)) => DiffInput::update(before, after),
        (None, Some(after)) => DiffInput::create(after),
        (Some(before), None) => DiffInput::delete(before),
        (None, None) => return Ok(()),
    };
    changelog::record_in(
        tx,
        ctx,
        ChangeTarget::of(model::OPERATION_TABLE, row_id.to_string(), action, dto::ACTION_LABEL),
        input,
    )
    .await
    .map_err(|error| utils::db_error(error, "写工序完工率变更日志"))?;
    Ok(())
}

/// 工序行 → 审计 JSON（只放登记白名单里的 5 列，多余列会被 diff 引擎忽略）。
fn spec_json(spec: &dto::OperationSpec) -> serde_json::Value {
    serde_json::json!({
        "seq": spec.seq,
        "operation_name": spec.operation_name,
        "weight": spec.weight.normalize().to_string(),
        "completion_pct": spec.completion_pct.normalize().to_string(),
        "remark": spec.remark,
    })
}

// ==================== 出参装配 ====================

/// 抬头 + 状态闸 + 工序行 → 完整出参（纯装配，不查库）。
fn build_res(
    head: &ObjectHead,
    gate: &dto::EditGate,
    rows: &[StoredRow],
    changed: bool,
    counts: Counts,
) -> OperationRes {
    let specs: Vec<dto::OperationSpec> = rows.iter().map(|row| row.spec.clone()).collect();
    let rollup = dto::rollup_of(&specs);
    let warnings = dto::warnings_of(head.wip_qty, head.completion_pct, &specs, rollup.as_ref());
    let rollup_view = rollup.as_ref().map(|rollup| dto::RollupView {
        row_count: specs.len(),
        weight_sum: dto::text_of(rollup.weight_sum),
        weighted_sum: dto::text_of(rollup.weighted_sum),
        pct_unrounded: dto::fixed_of(rollup.pct_unrounded, 6),
        pct: dto::text_of(rollup.pct),
        object_pct: dto::text_of(head.completion_pct),
        matched: head.completion_pct == rollup.pct,
        diff: dto::fixed_of(head.completion_pct - rollup.pct, 4),
    });
    let note = match rollup.as_ref() {
        Some(rollup) => OperationRes::filled_note(&head.object_no, specs.len(), &dto::text_of(rollup.pct)),
        None => OperationRes::empty_note(&head.object_no),
    };
    OperationRes {
        object: head.view(gate),
        rows: rows.iter().map(|row| row.view(rollup.as_ref())).collect(),
        rollup: rollup_view,
        changed,
        inserted: counts.inserted,
        updated: counts.updated,
        deleted: counts.deleted,
        state: gate.state().to_string(),
        formula: dto::FORMULA.to_string(),
        weight_hint: dto::FORMULA_WEIGHT.to_string(),
        warnings,
        note,
    }
}

// ==================== HTTP 入口 ====================

/// 读该批的工序完工率（含加权汇总与状态闸）。
#[nik::permission(
    path = "/business/erp/cost-object/{id}/operations",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-object/{id}/operations")]
pub async fn cost_object_operations_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<OperationRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(operations_of(&principal, &id).await?))
}

/// 整表保存工序完工率（同事务回写对象级完工率）。
#[nik::permission(
    path = "/business/erp/cost-object/{id}/operations",
    method = "PUT",
    permission = "business:erp:cost-object:update"
)]
#[nik::put("/business/erp/cost-object/{id}/operations")]
pub async fn cost_object_operations_save_api(
    headers: HeaderMap,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<OperationSaveReq>,
) -> ResResult<OperationRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    let audit = changelog::RequestCtx::from_request(&claims, &headers, None);
    let ctx = audit.ctx();
    Ok(Res::ok(save_operations(&principal, &ctx, &id, req).await?))
}
