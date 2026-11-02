// ============================================================================
// transfer_write.rs —— 写入：建单/改单/软删/状态流转（含审批落定）/审批流定义
//
// 由 `transfer_model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::shared::state_machine;

use super::transfer_base::*;
use super::transfer_settle::uuid_text;

/// 建单（`status = DRAFT`）：**取号 → 单头 → 明细 → 序列号**，调用方负责事务边界。
///
/// # 异常情况
///
/// 数据库异常（唯一键 / CHECK / 外键）→ [`DbError`]（handler 翻译成 409 / 400 / 500）。
pub async fn create(
    ex: &impl SqlExecutor,
    new: &NewTransfer<'_>,
    items: &[NewItem],
    total_qty: Decimal,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let transfer_no = next_transfer_no(ex, now).await?;
    let id = Uuid::now_v7();
    // 日期用「两条 SQL 片段」而不是 `COALESCE(?::date, CURRENT_DATE)`：
    // `Value::Null` 绑到 `?::date` 会因为类型推断不出来而在驱动侧编码失败
    // （技能 §3 记的 uuid 版同一个坑）。缺席就写成静态的 `CURRENT_DATE`。
    let (date_sql, date_param) = match new.transfer_date {
        Some(date) => ("?", vec![Value::from(date)]),
        None => ("CURRENT_DATE", Vec::new()),
    };
    let operator_text = Value::Text(operator.map(|id| id.to_string()).unwrap_or_default());
    let mut params = vec![
        Value::from(&id),
        Value::Text(transfer_no.clone()),
        Value::from(&new.from_warehouse_id),
        Value::from(&new.to_warehouse_id),
    ];
    params.extend(date_param);
    params.extend(vec![
        Value::Text(new.reason.unwrap_or_default().to_string()),
        Value::Text(
            new.applicant_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
        ),
        Value::Text(new.applicant_name.unwrap_or_default().to_string()),
        Value::Text(total_qty.to_string()),
        Value::Text(new.remark.unwrap_or_default().to_string()),
        Value::from(now),
        Value::from(now),
        operator_text.clone(),
        operator_text,
    ]);
    ex.exec_sql(
        format!(
            "INSERT INTO {TRANSFER_TABLE} \
                (id, transfer_no, from_warehouse_id, to_warehouse_id, status, \
                 transfer_date, reason, applicant_id, applicant_name, total_qty, total_amount, \
                 remark, created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?::uuid, ?::uuid, 'DRAFT', \
                 {date_sql}, NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
                 NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,''), ?, ?, \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        params,
    )
    .await?;
    insert_items(ex, id, items, operator).await?;
    // P10 铁律 ③：建单留痕（`from_status = NULL → DRAFT`），与建单同一事务。
    // 五张单据里调拨单是最后补上的一个 —— 少这一行，详情页的「状态历史」里
    // 调拨单永远缺第一格（提交那条日志的 from 是 DRAFT，但没人告诉他们 DRAFT 从哪来）。
    state_machine::log_created(
        ex,
        state_machine::ENTITY_TRANSFER,
        id,
        &transfer_no,
        STATUS_DRAFT,
        operator,
        None,
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok(id)
}

/// 整批插入明细 + 序列号（建单 / 改单共用）。
pub(super) async fn insert_items(
    ex: &impl SqlExecutor,
    transfer_id: Uuid,
    items: &[NewItem],
    operator: Option<Uuid>,
) -> DbResult<()> {
    for (index, item) in items.iter().enumerate() {
        let line_no = i32::try_from(index + 1).unwrap_or(i32::MAX);
        let item_id: Uuid = ex
            .query_sql(
                format!(
                    "INSERT INTO {ITEM_TABLE} \
                        (id, transfer_id, line_no, material_id, from_location_id, to_location_id, \
                         qty, shipped_qty, received_qty, unit_cost, amount, create_by, update_by) \
                     VALUES (?, ?::uuid, ?, ?::uuid, NULLIF(?::text,'')::uuid, \
                         NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::numeric, 0, 0, 0, 0, \
                         NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
                     RETURNING id"
                ),
                vec![
                    Value::from(Uuid::now_v7()),
                    Value::from(&transfer_id),
                    Value::from(line_no),
                    Value::from(&item.material_id),
                    Value::Text(
                        item.from_location_id
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                    ),
                    Value::Text(
                        item.to_location_id
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                    ),
                    Value::Text(item.qty.to_string()),
                    Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
                    Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
                ],
            )
            .await?
            .first()
            .map(|row| row.get_typed::<Uuid>("id"))
            .transpose()?
            .ok_or_else(|| DbError::NotFound)?;

        if item.serial_ids.is_empty() {
            continue;
        }
        // 一行一个序列号载荷：`itemId|serialId`，SQL 侧拆（不走数组参数，见 count_valid_serials）
        let payload = item
            .serial_ids
            .iter()
            .map(|serial_id| format!("{item_id}|{serial_id}"))
            .collect::<Vec<_>>()
            .join("\n");
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_SERIAL_TABLE} (id, item_id, serial_id, create_by) \
                 SELECT gen_random_uuid(), split_part(t.line, '|', 1)::uuid, \
                        split_part(t.line, '|', 2)::uuid, NULLIF(?::text,'')::uuid \
                   FROM unnest(string_to_array(?, E'\\n')) AS t(line)"
            ),
            vec![
                Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
                Value::Text(payload),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 改单：**只有 DRAFT**（调用方先判状态），明细整体重建。
///
/// 顺序必须是「序列号子表 → 明细行 → 重插」：`_item_serial` 对 `_item` 是硬外键，
/// 先删父行会撞外键；而明细表没有 `deleted_at`，改单只能物理删。
pub async fn update(
    ex: &impl SqlExecutor,
    id: Uuid,
    new: &NewTransfer<'_>,
    items: &[NewItem],
    total_qty: Decimal,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let (date_sql, date_param) = match new.transfer_date {
        Some(date) => ("?", vec![Value::from(date)]),
        None => ("transfer_date", Vec::new()),
    };
    let operator_text = Value::Text(operator.map(|uuid| uuid.to_string()).unwrap_or_default());
    let mut params = vec![
        Value::from(&new.from_warehouse_id),
        Value::from(&new.to_warehouse_id),
    ];
    params.extend(date_param);
    params.extend(vec![
        Value::Text(new.reason.unwrap_or_default().to_string()),
        Value::Text(new.remark.unwrap_or_default().to_string()),
        Value::Text(total_qty.to_string()),
        Value::from(now),
        operator_text,
        Value::from(&id),
    ]);
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET from_warehouse_id = ?::uuid, to_warehouse_id = ?::uuid, \
                        transfer_date = {date_sql}, \
                        reason = NULLIF(?::text,''), \
                        remark = NULLIF(?::text,''), \
                        total_qty = NULLIF(?::text,'')::numeric, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
            ),
            params,
        )
        .await?;
    if affected == 0 {
        return Ok(0);
    }
    replace_items(ex, id, items, operator).await?;
    Ok(affected)
}

/// 明细整体重建（改单用）。
pub(super) async fn replace_items(
    ex: &impl SqlExecutor,
    transfer_id: Uuid,
    items: &[NewItem],
    operator: Option<Uuid>,
) -> DbResult<()> {
    ex.exec_sql(
        format!(
            "DELETE FROM {ITEM_SERIAL_TABLE} \
              WHERE item_id IN (SELECT id FROM {ITEM_TABLE} WHERE transfer_id = ?::uuid)"
        ),
        vec![Value::from(&transfer_id)],
    )
    .await?;
    ex.exec_sql(
        format!("DELETE FROM {ITEM_TABLE} WHERE transfer_id = ?::uuid"),
        vec![Value::from(&transfer_id)],
    )
    .await?;
    insert_items(ex, transfer_id, items, operator).await
}

/// 软删（调用方先判 `DRAFT` / `CANCELLED`）；返回受影响行数。
pub async fn soft_delete(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TRANSFER_TABLE} \
                SET deleted_at = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status IN ('DRAFT','CANCELLED')"
        ),
        vec![
            Value::from(now),
            Value::from(now),
            Value::Text(operator.map(|uuid| uuid.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 写：状态流转 ====================

/// 状态流转落库（**CAS：`WHERE status = 期望的当前态`**）。
///
/// 五个目标各有自己的 `SET` 子句（`ck_biz_transfer_state` 绑死了时间戳），
/// 并且都带 `AND status = <from>` —— 并发下两个人同时推同一张单，只有一个 `affected = 1`，
/// 另一个拿到 409 而不是「两笔都写进去」。
///
/// # 参数
///
/// - `effect`：[`transition_effect`] 的产物（含目标状态与要写的列）；
/// - `from`：调用方读到的当前状态（CAS 期望值）；
/// - `now`：Rust 侧算好的时间点（与变更日志 / 响应里的时间同源）；
/// - `cancel_reason`：仅 `→ CANCELLED` 用（`ck_biz_transfer_state` 要求非空白）；
/// - `transit_warehouse_id`：仅 `→ IN_TRANSIT` 用（该状态起必须非空）；
/// - `approver`：仅 `→ APPROVED` 用（写 `approver_id`）；
/// - `approval_instance_id`：仅 `DRAFT → PENDING` 用（发起审批后回写）。
///
/// # 异常情况
///
/// 数据库异常 / CHECK 冲突 → [`DbError`]；`affected = 0` 表示状态已被别人改走。
#[allow(clippy::too_many_arguments)]
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    effect: &TransitionEffect,
    cancel_reason: Option<&str>,
    transit_warehouse_id: Option<Uuid>,
    approver: Option<Uuid>,
    approval_instance_id: Option<Uuid>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let uuid_text =
        |value: Option<Uuid>| Value::Text(value.map(|id| id.to_string()).unwrap_or_default());
    let (sql, params) = match effect.status {
        STATUS_PENDING => (
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET status = 'PENDING', submitted_at = ?, updated_at = ?, \
                        approval_instance_id = NULLIF(?::text,'')::uuid, \
                        applicant_id = COALESCE(applicant_id, NULLIF(?::text,'')::uuid), \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                Value::from(now),
                uuid_text(approval_instance_id),
                uuid_text(operator),
                uuid_text(operator),
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_APPROVED => (
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET status = 'APPROVED', approved_at = ?, approver_id = NULLIF(?::text,'')::uuid, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                uuid_text(approver.or(operator)),
                Value::from(now),
                uuid_text(operator),
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_CANCELLED => (
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET status = 'CANCELLED', cancelled_at = ?, cancel_reason = ?, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                Value::Text(cancel_reason.unwrap_or_default().to_string()),
                Value::from(now),
                uuid_text(operator),
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_IN_TRANSIT => (
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET status = 'IN_TRANSIT', shipped_at = ?, \
                        transit_warehouse_id = NULLIF(?::text,'')::uuid, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                uuid_text(transit_warehouse_id),
                Value::from(now),
                uuid_text(operator),
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_DONE => (
            format!(
                "UPDATE {TRANSFER_TABLE} \
                    SET status = 'DONE', received_at = ?, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                Value::from(now),
                uuid_text(operator),
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        other => {
            return Err(DbError::InvalidValue(format!(
                "状态流转的目标 `{other}` 不在 5 个可写目标里（DRAFT 只能由建单产生）"
            )));
        }
    };
    ex.exec_sql(sql, params).await
}

/// 审批结论 → 调拨单（**审批回调专用**，`PENDING` 才动）。
///
/// | 审批结论 | 调拨单 | 依据 |
/// |---|---|---|
/// | `APPROVED` | `PENDING → APPROVED` | 审批通过 = 可发货；`approver_id` 记**实际审批人**（回调方从审批待办里取，`None` 时保留原值） |
/// | `REJECTED` | `PENDING → DRAFT`（打回） | `submitted_at` / `approved_at` **必须清回 NULL**（`ck_biz_transfer_state`），审批意见写 `reason` |
///
/// 返回受影响行数（0 = 当前不在 `PENDING`，重复回调 / 已被人工撤回 → 静默跳过）。
pub async fn settle_approval(
    ex: &impl SqlExecutor,
    id: Uuid,
    approved: bool,
    opinion: Option<&str>,
    approver: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    if approved {
        return ex
            .exec_sql(
                format!(
                    "UPDATE {TRANSFER_TABLE} \
                        SET status = 'APPROVED', approved_at = ?, updated_at = ?, \
                            approver_id = COALESCE(NULLIF(?::text,'')::uuid, approver_id) \
                      WHERE id = ?::uuid AND status = 'PENDING' AND deleted_at IS NULL"
                ),
                vec![
                    Value::from(now),
                    Value::from(now),
                    uuid_text(approver),
                    Value::from(&id),
                ],
            )
            .await;
    }
    // 打回：DRAFT 分支要求全部时间戳为 NULL，所以四个时间列 + 取消列一起清空，
    // 审批意见覆盖 reason（列宽 255，超长截断），审批实例 id 清空以便重新提交。
    let reason = opinion.map(|text| text.trim().chars().take(255).collect::<String>());
    ex.exec_sql(
        format!(
            "UPDATE {TRANSFER_TABLE} \
                SET status = 'DRAFT', submitted_at = NULL, approved_at = NULL, \
                    shipped_at = NULL, received_at = NULL, cancelled_at = NULL, \
                    cancel_reason = NULL, approval_instance_id = NULL, \
                    reason = COALESCE(NULLIF(?::text,''), reason), updated_at = ? \
              WHERE id = ?::uuid AND status = 'PENDING' AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(reason.unwrap_or_default()),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 审批接入：找已发布的调拨流程 ====================

/// 已发布的调拨审批流程（引擎发起参数的最小集合）。
#[derive(Debug, Clone)]
pub struct PublishedFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`STOCK_TRANSFER`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的调拨流程」。
///
/// 卡面口径：`flow_code = 'stock_transfer'` **或** `biz_type = 'STOCK_TRANSFER'`。
/// 排序让 `biz_type` 精确命中的排在前面（同一个库里两种都配时取业务类型那条），
/// 同组内按 `is_default` / 版本号 / 更新时间取最新。
///
/// `None` = **没配流程**（不是错误）：提交审批照样允许，`PENDING → APPROVED` 由人工推。
pub async fn published_transfer_flow() -> DbResult<Option<PublishedFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'STOCK_TRANSFER' OR lower(d.flow_code) = 'stock_transfer') \
              ORDER BY (d.biz_type = 'STOCK_TRANSFER') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PublishedFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 结算：IN_TRANSIT → DONE 的库存双写（W3c） ====================
//
// 卡面（云枢 TSK-202610-000683）要求的 5 步，全部在**调用方开的同一个事务**里：
//
// ```text
// ① 调出仓：qty -= 数量；流水 OUT / TRANSFER_OUT（qty 恒正，ck_biz_inv_txn_qty 禁负数）
// ② 调入仓：qty += 数量；库存行不存在 → INSERT 建档；流水 IN / TRANSFER_IN
// ③ 一物一码物料：biz_material_serial 的 warehouse_id / location_id 跟着走
// ④ 成本原价转移：unit_cost = 调出仓的 avg_cost 快照（不是当前重新算的均价）
// ⑤ 不变式断言：调出仓 qty >= 0、一物一码 qty == 在库序列号件数、成本守恒
// ```
//
// # 三个实现口径（与卡面字面的差异，逐条实测后定的）
//
// 1. **目标库存行的选择**：卡面写「按 material + warehouse + location」，实测库里
//    `biz_inventory` 的 `location_id IS NULL` 是**整仓汇总桶**（W2c-1 的
//    `recompute_quantity` 与 W2c-2 的巡检都只认这一桶）。所以：
//    有汇总桶 → 只动汇总桶（否则会与库位行并存，`SUM(qty)` 双计、不变式③必假）；
//    没有汇总桶（纯库位记账的仓）→ 按 item 的 `from_location_id` / `to_location_id` 精确到库位。
// 2. **并发防超卖**用条件 UPDATE（`AND qty >= $1` + `RETURNING`），不用显式 `FOR UPDATE`：
//    返回 0 行 = 库存不足，调用方整笔回滚。PG 在拿到行锁后会按新版本重判谓词，
//    所以两笔同时扣同一行只有一个能成功（实测：库存 8、两笔各扣 5 → 一个 200 一个 409）。
// 3. **金额**一律由 SQL 侧的 `ROUND(数量 * avg_cost, 4)` 算出来并回读，再原样转给调入仓 ——
//    Rust 侧 `Decimal` 的舍入是「四舍六入五成双」、PG 的 numeric 是「四舍五入」，
//    各算一遍会在 .00005 的边界上差 1 分钱，成本守恒断言就会偶发多/少 0.0001。
