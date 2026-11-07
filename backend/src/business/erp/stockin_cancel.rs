// ============================================================================
// stockin_cancel.rs —— 作废链路：作废前置测算、成本与库位反冲、序列号作废、作废流水
//
// 由 `stockin_model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::serial_model;

use super::inventory_writer::{self, TxnRecord};
use super::stockin_base::*;
use super::stockin_write::{AppliedLocation, lock_inventory};

/// 流水方向：出（作废冲销 / `ck_biz_inv_txn_type` 只认 `IN` / `OUT`）。
pub const TXN_TYPE_OUT: &str = "OUT";

/// 流水业务类型：采购入库**作废冲销**（P9 新增，与 `PURCHASE_IN` 配对）。
pub const BIZ_TYPE_PURCHASE_IN_CANCEL: &str = "PURCHASE_IN_CANCEL";

/// 库存不足的哨兵前缀（`DbError` 只有字符串，handler 据此回 `409` 而不是 `500`）。
///
/// 形态：`INV_SHORTFALL|<明细文案>`，例如
/// `INV_SHORTFALL|物料 P2B1-M1 库存 3.0000 < 本单 9.0000`。
///
/// P2-5A：定义搬去唯一写入口 [`crate::business::erp::inventory_writer::SHORTFALL_PREFIX`]
/// （库位账冲减的 SQL 与文案都在那边），这里 `pub use` 重导出，调用方路径不变。
pub use super::inventory_writer::SHORTFALL_PREFIX;

/// 过账流水缺失的哨兵前缀（历史数据缺口：单据 POSTED 但 `biz_inventory_txn` 里没有对应行）。
///
/// 遇到它必须**拒绝作废**（没法知道该退多少金额），并提示走数据修复脚本 —— 绝不按现价估算。
pub const MISSING_TXN_PREFIX: &str = "INV_NO_TXN|";

/// 序列号已流出仓库的哨兵前缀（P10：本单生成的一物一码号码里已有出库 / 报废的件）。
///
/// 形态：`SERIAL_SHIPPED|<明细文案>`，例如
/// `SERIAL_SHIPPED|收货单 IN-… 生成的一物一码序列号 SN-… 已出库（OUTBOUND）…`。
///
/// 为什么不与 [`SHORTFALL_PREFIX`] 合并：**判据不同**。一物一码物料的「货已出库」最硬的证据是
/// 序列号状态，而不是库存数量 —— 同一物料后来又进过别的货时，数量上「够退」，
/// 但退回来的不是本单那几件（按数量放开就会把别批的货冲掉）。
pub const SERIAL_SHIPPED_PREFIX: &str = "SERIAL_SHIPPED|";

/// 采购单状态被并发改动时的哨兵前缀（P10 铁律 ③：日志与状态必须同生同死）。
///
/// `sync_purchase_status` 的顺序是「先写 `biz_status_log`，再 CAS 改状态」。
/// CAS 落空（`affected = 0`）说明在读状态与写状态之间别人把采购单推走了：
/// 此时**必须让整笔收货回滚**，否则会留下一条「日志说推进了、单据其实没动」的
/// 假轨迹 —— 审计证据链一旦能被伪造，就等于没有。
pub const PO_RACE_PREFIX: &str = "PO_STATUS_RACE|";

/// 作废时**一行**要退回的数量与金额。
///
/// 数量来自 `biz_stock_in_item`（**不**信单据头的 `total_qty`），金额来自
/// `biz_inventory_txn` 里这行过账时**实际写入**的 `amount`（**不**用当前 `avg_cost × qty`：
/// 期间又进过货，均价已变，用现价冲减会污染成本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelLine {
    /// 明细行号（流水 `line_no`）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 收货仓库（单据头的 `warehouse_id`，冲销流水要用）。
    pub warehouse_id: Uuid,
    /// 库位账行 id（`biz_inventory_location.id`，可空 = 未分配库位桶）。
    pub location_id: Option<Uuid>,
    /// 本单这一行的入库数量（`biz_stock_in_item.qty`）。
    pub qty: Decimal,
    /// 本行过账时写入的不含税金额（`biz_inventory_txn.amount`）。
    pub amount: Decimal,
    /// 本行过账时的入库单价（`amount / qty`，6 位；写冲销流水用）。
    pub unit_cost: Decimal,
}

/// 作废单的基本信息（冲销流水要写的 `doc_id` / `doc_no`）。
#[derive(Debug, Clone, Copy)]
pub struct CancelDoc<'a> {
    /// 收货单 id（`biz_inventory_txn.doc_id`）。
    pub id: Uuid,
    /// 收货单号（`biz_inventory_txn.doc_no`）。
    pub in_no: &'a str,
}

/// 组装作废计划：逐行读「明细数量 + 过账流水的金额」。
///
/// * 排序：`material_id, location, line_no` —— **锁顺序**与过账一致（PRD §3.2.6），
///   两张单并发作废时按同一全序拿锁，不会交叉等待；
/// * 流水缺失 / 数量对不上 → 直接报错（**绝不静默按现价重算**）：历史缺口只能由
///   数据修复脚本补流水，代码层面必须让调用方看见问题。
pub async fn cancel_plan_of(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
    in_no: &str,
    warehouse_id: Uuid,
) -> DbResult<Vec<CancelLine>> {
    let rows = ex
        .query_sql(
            "SELECT i.line_no, i.material_id, i.location_id, i.qty, \
                    coalesce(t.qty, 0) AS txn_qty, coalesce(t.amount, 0) AS txn_amount, \
                    coalesce(t.cnt, 0) AS txn_cnt \
               FROM biz_stock_in_item i \
               LEFT JOIN (SELECT line_no, sum(qty) AS qty, sum(amount) AS amount, count(*) AS cnt \
                            FROM biz_inventory_txn \
                           WHERE doc_type = ? AND doc_id = ?::uuid \
                             AND txn_type = ? AND biz_type = ? \
                           GROUP BY line_no) t ON t.line_no = i.line_no \
              WHERE i.stock_in_id = ?::uuid \
              ORDER BY i.material_id, i.location_id NULLS FIRST, i.line_no"
                .to_string(),
            vec![
                Value::from(DOC_TYPE_STOCK_IN),
                Value::from(&stock_in_id),
                Value::from(TXN_TYPE_IN),
                Value::from(BIZ_TYPE_PURCHASE_IN),
                Value::from(&stock_in_id),
            ],
        )
        .await?;

    let mut plan = Vec::with_capacity(rows.len());
    for row in rows {
        let line_no: i32 = row.get_typed("line_no")?;
        let material_id: Uuid = row.get_typed("material_id")?;
        let location_id: Option<Uuid> = row.get_typed("location_id")?;
        let qty: Decimal = row.get_typed("qty")?;
        let txn_qty: Decimal = row.get_typed("txn_qty")?;
        let txn_amount: Decimal = row.get_typed("txn_amount")?;
        let txn_cnt: i64 = row.get_typed("txn_cnt")?;
        if txn_cnt == 0 {
            return Err(DbError::InvalidValue(format!(
                "{MISSING_TXN_PREFIX}收货单 {in_no} 第 {line_no} 行在 biz_inventory_txn 里找不到\
                 过账流水（P9 之前的历史数据缺口）：无法确定要退回多少金额，拒绝按现价估算。\
                 请先用数据修复脚本补流水，或走人工红字调整单"
            )));
        }
        if (txn_qty - qty).abs() > Decimal::new(1, 4) {
            return Err(DbError::InvalidValue(format!(
                "收货单 {in_no} 第 {line_no} 行：明细数量 {qty} 与过账流水数量 {txn_qty} 不一致，\
                 单据已被改过（POSTED 不应可改），拒绝作废"
            )));
        }
        let unit_cost = if txn_qty.is_zero() {
            Decimal::ZERO
        } else {
            (txn_amount / txn_qty).round_dp_with_strategy(6, ROUNDING)
        };
        plan.push(CancelLine {
            line_no,
            material_id,
            warehouse_id,
            location_id,
            qty,
            amount: txn_amount,
            unit_cost,
        });
    }
    Ok(plan)
}

/// 一条明细的**成本账反向冲减**：`qty -= 本单数量`、`amount -= 过账时的金额`。
///
/// # 三条硬口径（卡面 §一 的 ①②③）
///
/// 1. `SELECT … FOR UPDATE` 锁库存行（调用方已按 `material_id` 升序排好，锁顺序全局一致）；
/// 2. **可退性校验**：`qty - 本单数量 >= 0`。已出库导致退不回来 → `DbError::InvalidValue`
///    带 [`SHORTFALL_PREFIX`]，handler 转 `409`（不是 500，也不是静默少退）；
/// 3. `qty` 归零 → `amount` 同时归零（避免留下「0 件还有钱」的幽灵金额）。
///
/// # 与卡面的一处差异：`avg_cost` 按剩余金额重算（不是原样不动）
///
/// 卡面写「移动加权：减量不改单价」。但作废退的是**这一层入库的原额**（`amount` 取自流水），
/// 而当前均价已被期间内其它入库稀释 ⇒ 「金额减 a、数量减 q、单价不动」三者必然互相矛盾
/// （`amount ≠ qty × avg_cost`，直接触发不变式 C4）。
/// 按 `(amount - a) / (qty - q)` 重算单价在数学上恰好等于「把这一层从加权平均里摘掉」：
/// 冲销**最近一层**入库时它会精确回到冲销前的历史均价（可被单测证明），冲销**中间层**时
/// 得到剩余各层的加权平均 —— 两者都是移动加权平均法的正解。
pub async fn reverse_cost(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    line: &CancelLine,
    now: DateTime<Utc>,
) -> DbResult<AppliedInventory> {
    let locked = lock_inventory(ex, material_id, warehouse_id, location_id).await?;
    let Some(before) = locked else {
        return Err(DbError::InvalidValue(format!(
            "{}物料 {material_id} 在仓库 {warehouse_id} 没有库存行，无法冲减（过账流水存在但库存被删了）",
            SHORTFALL_PREFIX
        )));
    };
    let after_qty = before.qty - line.qty;
    if after_qty < Decimal::ZERO {
        let code: Option<String> = ex
            .scalar_sql(
                "SELECT material_code FROM biz_material WHERE id = ?::uuid".to_string(),
                vec![Value::from(&material_id)],
            )
            .await?;
        return Err(DbError::InvalidValue(format!(
            "{SHORTFALL_PREFIX}物料 {} 库存 {} < 本单 {}",
            code.unwrap_or_else(|| material_id.to_string()),
            before.qty.round_dp(4),
            line.qty.round_dp(4)
        )));
    }

    // P2-5A：冲减 SQL（按桶键减 + 均价重算 + 归零夹 0）收口到唯一写入口。
    let after = inventory_writer::subtract_cost(
        ex,
        material_id,
        warehouse_id,
        location_id,
        line.qty,
        line.amount,
        now,
    )
    .await?;
    Ok(AppliedInventory { before, after })
}

/// 一条明细的**库位账反向冲减**（与 [`apply_location_qty`] 的两个分支一一对应）。
///
/// P6 让过账同时加库位账，这里必须能退；退不回来（库位账 < 本单数量）同样报
/// [`SHORTFALL_PREFIX`]，让 handler 回 `409` 而不是把库位账冲成负数。
pub async fn reverse_location_qty(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<AppliedLocation> {
    // P2-5A：两条分支（按库位账行 id / 未分配桶）+ 退不回来的 409 哨兵文案
    // 全部收口到 [`inventory_writer::subtract_location_qty`]。
    let (before_qty, after_qty) = inventory_writer::subtract_location_qty(
        ex,
        material_id,
        warehouse_id,
        location_id,
        qty,
        now,
    )
    .await?;
    Ok(AppliedLocation {
        before_qty,
        after_qty,
    })
}

/// 写一条**反向流水**：`txn_type='OUT'` + `biz_type='PURCHASE_IN_CANCEL'`。
///
/// `before_*` / `after_*` 六个字段全部真填（审计要能从流水复原每一次冲减）。
#[allow(clippy::too_many_arguments)]
pub async fn write_cancel_txn(
    ex: &impl SqlExecutor,
    doc: &CancelDoc<'_>,
    line: &CancelLine,
    applied: &AppliedInventory,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    // P2-5A：流水 SQL 收口到 [`inventory_writer::insert_txn`]。
    inventory_writer::insert_txn(
        ex,
        &TxnRecord {
            material_id: line.material_id,
            warehouse_id: line.warehouse_id,
            txn_type: TXN_TYPE_OUT,
            biz_type: BIZ_TYPE_PURCHASE_IN_CANCEL,
            doc_type: DOC_TYPE_STOCK_IN,
            doc_id: doc.id,
            doc_no: doc.in_no,
            line_no: line.line_no,
            qty: line.qty.round_dp(4),
            unit_cost: line.unit_cost,
            amount: line.amount.round_dp(4),
            before: applied.before,
            after: applied.after,
            operator,
            period_code: None,
            remark: None,
            now,
        },
    )
    .await
    .map(|_| ())
}

// ==================== 一物一码：序列号 ====================

/// 落一行序列号（客户给号 / 自动取号共用）。
///
/// `batch.source_type` 由调用方按本单 `in_type` 决定（采购入库 / 退货入库，见
/// [`super::stockin_base::serial_source_of_in_type`]）；`None` = 该 `in_type`
/// 不在序列号来源 4 类域内（其他入库 / 无源入库）⇒ **只写 `source_id`（恒写本单 id）、
/// `source_type` 留空**。理由：`source_id` 是「号挂在哪张单」的唯一线索，详情计数与
/// 作废退号都靠它；来源**类型**只是分类标签，缺了不影响任何链路（见 [`insert_serials`]）。
///
/// 参数直接收 `&SerialBatch`（而不是 8 个散参数）：clippy 的 `too_many_arguments`
/// 只是症状，真问题是「谁的号 / 落在哪 / 来自哪张单」散着传时调用点读不出来。
pub(super) async fn insert_serials(
    ex: &impl SqlExecutor,
    pairs: &[(Uuid, String)],
    batch: &SerialBatch<'_>,
) -> DbResult<()> {
    let SerialBatch {
        stock_in_id,
        source_type,
        material_id,
        warehouse_id,
        location_id,
        operator,
        ..
    } = *batch;
    if pairs.is_empty() {
        return Ok(());
    }
    let payload = serial_model::pair_payload(pairs);
    let operator_text = Value::Text(operator.map(|v| v.to_string()).unwrap_or_default());
    // 来源类型：只有 `PURCHASE` / `RETURN` 在序列号来源的 4 类域内才写
    // （其他入库 / 无源入库 `in_type = SOURCELESS` 时不写类型）。
    let source_type_text = Value::Text(source_type.unwrap_or_default().to_string());
    // 来源单据：**恒写本单 id**。
    //
    // 旧写法是「没有来源类型 ⇒ 连 source_id 一起留空」（追求「有类型必有 id、有 id 必有类型」）。
    // 代价太大：`biz_material_serial.source_id` 是**唯一能把序列号挂回收货单**的线索，
    // 留空之后
    //   · 详情 `serial_count_of(单)` 恒 0（FIX-7 之后无源入库的序列号全成孤儿）；
    //   · 作废收货单时 `serial_by_source(单)` 查不到号 ⇒ **号不退、库存与序列号台账漂移**。
    // 所以改成「source_id 恒写，source_type 可空」：一对多的关联是事实，类型只是分类标签。
    let source_id_text = Value::Text(stock_in_id.to_string());
    ex.exec_sql(
        "INSERT INTO biz_material_serial \
             (id, serial_no, material_id, warehouse_id, location_id, batch_no, status, inbound_at, \
              source_type, source_id, create_by, update_by) \
         SELECT split_part(t.line, '|', 1)::uuid, split_part(t.line, '|', 2), ?::uuid, ?::uuid, \
                NULLIF(?::text,'')::uuid, NULL, 'IN_STOCK', now(), \
                NULLIF(?::text,''), NULLIF(?::text,'')::uuid, \
                NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid \
           FROM unnest(string_to_array(?, chr(10))) AS t(line)"
            .to_string(),
        vec![
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::Text(location_id.map(|v| v.to_string()).unwrap_or_default()),
            source_type_text,
            source_id_text,
            operator_text.clone(),
            operator_text,
            Value::Text(payload),
        ],
    )
    .await?;
    Ok(())
}

/// 一批序列号的落库入参（收进结构体：既避开 `clippy::too_many_arguments`，
/// 也让「谁的序列号、落在哪个仓/库位、来自哪张单」在调用点一眼看清）。
#[derive(Debug, Clone)]
pub struct SerialBatch<'a> {
    /// 来源收货单（`biz_material_serial.source_id`）。
    pub stock_in_id: Uuid,
    /// 序列号来源类型（按本单 `in_type` → `PURCHASE` / `RETURN`；`None` = 不记来源）。
    pub source_type: Option<&'static str>,
    /// 物料。
    pub material_id: Uuid,
    /// 收货仓库（序列号的 `warehouse_id`）。
    pub warehouse_id: Uuid,
    /// 库位（可空）。
    pub location_id: Option<Uuid>,
    /// 本次入库件数（必须与 `serial_ids` 等长，或按其取号）。
    pub qty: Decimal,
    /// 客户端给的号码（空 = 自动取号）。
    pub serial_ids: &'a [String],
    /// 操作人。
    pub operator: Option<Uuid>,
    /// 记账时刻（Rust 侧算好，保证号码周期键与流水时间同源）。
    pub now: DateTime<Utc>,
}

/// 生成（或落客户端给的）序列号，返回本次落的号码（按顺序）。
///
/// * `serial_ids` 非空：**必须**正好 `qty` 条（数量不符在 DTO 层就 400），号码原样落库
///   （供应商已经贴好的码，不允许改写）；
/// * 否则按 `SN-yyyyMMdd-NNNNNN` 取号生成 `qty` 条。
///
/// 见模块文档「与卡面 §三 e) 的实现差异」：这里**不调用** `serial_model::batch_generate`
/// （它会重算库存数量并再写一条 `SERIAL_IN` 流水，与本模块的移动加权加法冲突）。
pub async fn generate_serials(
    ex: &impl SqlExecutor,
    batch: &SerialBatch<'_>,
) -> DbResult<Vec<String>> {
    let count: i64 = batch.qty.try_into().map_err(|_| {
        DbError::InvalidValue(format!(
            "一物一码物料的数量必须是整数（当前 {}）",
            batch.qty
        ))
    })?;
    if count <= 0 {
        return Err(DbError::InvalidValue(
            "一物一码物料的入库数量必须 > 0".into(),
        ));
    }
    let pairs: Vec<(Uuid, String)> = if batch.serial_ids.is_empty() {
        let period_key = serial_model::period_key_of(batch.now);
        let start = serial_model::take_numbers(ex, count, &period_key).await?;
        (1..=count)
            .map(|offset| {
                (
                    Uuid::now_v7(),
                    serial_model::format_serial_no(&period_key, start + offset),
                )
            })
            .collect()
    } else {
        batch
            .serial_ids
            .iter()
            .map(|serial_no| (Uuid::now_v7(), serial_no.trim().to_string()))
            .collect()
    };
    insert_serials(ex, &pairs, batch).await?;
    Ok(pairs.into_iter().map(|(_, serial_no)| serial_no).collect())
}

/// 本单生成的一条序列号（作废前的校验视图：号码 + 状态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialBrief {
    /// 系统内码（`SN-yyyyMMdd-NNNNNN`）。
    pub serial_no: String,
    /// 当前状态（`IN_STOCK` / `OUTBOUND` / `SCRAPPED`）。
    pub status: String,
}

/// 序列号状态的中文文案（`409` 报文用）。
pub(super) fn serial_status_label(status: &str) -> &'static str {
    match status {
        serial_model::STATUS_OUTBOUND => "出库",
        serial_model::STATUS_SCRAPPED => "报废",
        _ => "变更",
    }
}

/// 锁读本单生成的一物一码序列号（`source_id = 本单 id`）。
///
/// P2-6b：判据从「`source_type='PURCHASE' AND source_id=本单`」收紧为**只按 `source_id`**。
/// 理由：`source_id` 就是「哪张收货单生成的号」，与来源类型无关 —— 退货入库
/// （`in_type=RETURN`）的号写的是 `source_type='RETURN'`，若还带类型谓词，作废时会
/// 命中 0 行、静默跳过退号（留下「单已作废、号还在库」的幽灵序列号）。
/// 收货单生成的号在这个库里只有本模块会写，`source_id` 唯一指回本单，不存在误伤。
///
/// `FOR UPDATE` 是必须的：作废与「单品出库 / 报废」改的是同一行的 `status`，
/// 行锁让两边在这行上串行 —— 否则「刚校验完就有人出库」会让后面的软删漏掉已出库的件，
/// 留下一条「单已作废、号还在库」的幽灵序列号（正是 W2c 口径要防的账实不符）。
pub async fn lock_serials_of(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
) -> DbResult<Vec<SerialBrief>> {
    // SCOPE-EXEMPT: 序列号台账是仓储数据，无归属列（口径同 serial_model）。
    let rows = ex
        .query_sql(
            "SELECT serial_no, status FROM biz_material_serial \
              WHERE source_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY serial_no FOR UPDATE"
                .to_string(),
            vec![Value::from(&stock_in_id)],
        )
        .await?;
    let mut serials = Vec::with_capacity(rows.len());
    for row in rows {
        serials.push(SerialBrief {
            serial_no: row.get_typed("serial_no")?,
            status: row.get_typed("status")?,
        });
    }
    Ok(serials)
}

/// **作废时「退序列号」**（P10）：本单生成的号码必须全部仍在库，否则拒绝；全部在库则**软删**。
/// 返回被软删的行数（0 行 = 该单不是一物一码，正常）。
///
/// # 口径（卡面 §一 三选一 → 取 ②+③ 的组合，理由写在这里免得下一张卡重猜）
///
/// 1. **不加 `VOID` 第四态**：`ck_biz_material_serial_status` 只认
///    `IN_STOCK / OUTBOUND / SCRAPPED`（V0138 定稿）。加 `VOID` 要动迁移 + `ALL_STATUSES` +
///    合法边表 + 前端状态映射 + 序列号分页筛选，只为一个「从未出库的单据作废」付这份代价不值；
/// 2. **未出库 → 软删该单生成的号**（`deleted_at` 置位、行保留）而不是物理删：
///    `uk_biz_material_serial_no` 是 `WHERE deleted_at IS NULL` 的部分唯一索引，
///    软删后同号可被重新贴用（W2a 口径），而「谁的号、哪天作废」的审计痕迹不丢；
///    物理删会同时丢掉痕迹与外键（`biz_stock_transfer_item_serial.serial_id` 是硬外键）；
/// 3. **已出库 / 已报废 → 拒绝作废**（[`SERIAL_SHIPPED_PREFIX`]，handler 转 `409`）：
///    与「货已部分出库」同款语义 —— 件已经离开仓库，只能先做退货入库冲销。
///
/// 幂等：重复作废在单据状态 CAS 处就被 `409` 拦住，本函数不会第二次执行；
/// 即便执行，`deleted_at IS NULL` 的条件也让第二遍命中 0 行。
pub async fn void_serials_of(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
    in_no: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let serials = lock_serials_of(ex, stock_in_id).await?;
    if serials.is_empty() {
        return Ok(0);
    }
    if let Some(blocked) = serials
        .iter()
        .find(|serial| serial.status != serial_model::STATUS_IN_STOCK)
    {
        return Err(DbError::InvalidValue(format!(
            "{SERIAL_SHIPPED_PREFIX}收货单 {in_no} 生成的一物一码序列号 {} 已{}（{}），\
             不能随单作废：那几件已经离开仓库，请先做退货入库冲销",
            blocked.serial_no,
            serial_status_label(&blocked.status),
            blocked.status
        )));
    }
    // SCOPE-EXEMPT: 序列号台账是仓储数据，无归属列（口径同 serial_model）。
    let affected = ex
        .exec_sql(
            "UPDATE biz_material_serial \
                SET deleted_at = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE source_id = ?::uuid AND status = ? \
                AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::from(now),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(&stock_in_id),
                Value::from(serial_model::STATUS_IN_STOCK),
            ],
        )
        .await?;
    Ok(affected)
}

// ==================== 采购单回写（已收数量 + 单据状态） ====================
