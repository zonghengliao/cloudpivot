//! 单品序列号数据层（批次 D · W2b · 云枢 TSK-202610-000679）。
//!
//! 依据：`backend/migrations/business/V0138__material_serial.up.sql`（表结构**以迁移为准**）。
//! 本文件负责 `biz_material_serial` 的**取号 / 批量生成 / 扫码 / 分页 / 状态流转**，
//! 接口层见 [`crate::business::erp::serial_handler`]，契约见 [`crate::business::erp::serial_dto`]。
//!
//! # 取号规则（卡面 §一）
//!
//! ```text
//! SN-yyyyMMdd-XXXXXX      6 位流水，按天（CST）重置
//! ```
//!
//! 号码来自 `biz_seq`（V0137 建表，V0138 已幂等种入 `seq_type='SN'` 一行）。
//! **一次 `UPDATE … RETURNING` 拿整段号**，绝不 `SELECT max()+1`：
//!
//! ```sql
//! UPDATE biz_seq
//!    SET current_no = CASE WHEN period_key = $1 THEN current_no + $2 ELSE $2 END,
//!        period_key = $1
//!  WHERE seq_type = 'SN' AND deleted_at IS NULL
//! RETURNING current_no - $2 AS start;   -- 号段 (start, start+n]
//! ```
//!
//! 三个性质（都是卡面并发红线的依据）：
//!
//! 1. **原子**：`UPDATE` 天然对该行加行锁，两个事务不可能拿到同一段号；
//! 2. **一次拿整段**：1000 条只发 1 条 `UPDATE`（不是 1000 次），把锁持有时间压到毫秒级；
//! 3. **跨天重置**：进入**更新的一天**时 `current_no` 重置为 `n`，因此隔天第一条必然是
//!    `…-000001`（不需要额外的定时任务清号）。周期键**回退**（时钟回拨 / 手工重放 / 假日期）
//!    时**绝不重置**，只继续累加 —— 否则 A→B→A 会与 A 当天已发出的号整段撞车
//!    （逐条理由见 [`bump_seq`]）。
//!
//! `RETURNING` 读到的是**更新后**的 `current_no`，减 `$2` 得到「本段之前的值」，
//! 所以本段号是 `start+1 ..= start+n`。
//!
//! # 与卡面的差异（逐条实测）
//!
//! 1. 卡面写 `biz_seq` 的键是 `scope = 'material_serial:20261030'`、列叫 `current_value`；
//!    **真实列名是 `seq_type` / `period_key` / `current_no`**（V0137 §5.9 建表），
//!    且 `seq_type='SN'` 上有一条部分唯一索引 `uk_biz_seq_type`。
//!    本文件按真实列名落地：`seq_type='SN'` + `period_key='yyyyMMdd'`。
//!    卡面的 `scope` 语义由 `(seq_type, period_key)` 两列共同表达，等效。
//! 2. 卡面「一次 UPDATE 拿 1000 个号段」→ 落地为 `current_no + n` 一次推进（`step` 列不参与：
//!    它表示「单据号的固定步长」，序列号逐件取号时恒为 1）。
//! 3. `biz_material_serial.location_id` 的硬外键指向 **`biz_inventory_location`**（库位库存明细行），
//!    不是 `biz_location`（库位字典）。所以扫码要拿 `location_name` 必须多一跳：
//!    `serial.location_id → biz_inventory_location.id → biz_inventory_location.location_id → biz_location.location_name`。
//!    这条链写在 [`SCAN_SQL`] 里，别按「location_id = biz_location.id」想当然 JOIN。
//! 4. 卡面 `PATCH …/status` 的 `reason` 字段**没有落库位置**：V0138 定稿的 16 列里没有
//!    `reason` / `remark`，而红线⑤禁止改表结构。故 `reason` 只做长度校验与日志留痕
//!    （见 [`crate::business::erp::serial_handler::apply_status`]），不写库。
//!
//! # 数据权限（门禁闸①/闸②）
//!
//! ```text
//! SCOPE-EXEMPT: 单品序列号是仓储作业数据，V0138 表里没有 dept_id / owner 列，数据范围
//! （SELF / DEPT_*）表达不出来；可见性由 business:erp:serial:* 权限码控制。
//! 分页仍走 scope::paginate_unscoped（闸①口径），不回落到框架 paginate_with_params。
//! ```
//!
//! # 库存联动（W2c-1 · 不变式 ①②）
//!
//! ```text
//! ① 对 serial_required = TRUE 的物料：biz_inventory.qty == count(serial where status='IN_STOCK')
//! ② 序列号状态与库存流水（biz_inventory_txn）在同一事务里落库
//! ```
//!
//! **决策：方案 A —— 数量由序列号派生**（不是「双写 + 巡检兜底」）。
//! 一物一码的核心价值就是单一真相：件数只能有一个来源，双写即使同事务也允许「两边各自算错」，
//! 巡检只能事后发现、不能阻止脏账。所以每次序列号状态变化后调用 [`recompute_quantity`]
//! （`UPDATE … SET qty = (SELECT count(*) …)`），流水只做审计、不回写数量。
//!
//! **事务边界在接口层**：本文件的写函数一律吃 `&impl SqlExecutor`
//! （[`nik::prelude::SqlExecutor`]，`Db` 与 `Transaction` 都实现），
//! 由 [`crate::business::erp::serial_handler::generate_serials`] /
//! [`crate::business::erp::serial_handler::apply_status`] 开事务并提交；
//! 模型层**不再自己 `db().begin()`**，也绝不出现「事务里再开事务」。
//!
//! 与卡面的三处口径差异（都有 DB 约束兜底，照卡面写会直接失败）：
//!
//! 1. 卡面写 `biz_inventory.quantity` —— 真实列名是 **`qty`**（V0137 §1.4）。
//! 2. 卡面写出库流水 `qty=-1` —— `ck_biz_inv_txn_qty CHECK (qty > 0)` **禁止负数**，
//!    方向由 `txn_type IN ('IN','OUT')` 表达。故出库/报废流水是 `txn_type='OUT', qty=+1`。
//! 3. 卡面写「报废流水 type=SCRAP」—— `ck_biz_inv_txn_type` 只认 `IN` / `OUT`，
//!    报废落成 `txn_type='OUT'` + `biz_type='SERIAL_SCRAP'`（业务类型区分报废与销售出库）。
//!
//! 数量重算同时把 `amount` 收成 `qty * avg_cost`（成本账的自洽式），
//! 否则「数量变了、金额没变」会让移动加权反算对不上。
//!
//! # 为什么扫码一定走索引
//!
//! [`serial_scan`] 的谓词是 `s.serial_no = ?`，命中 V0138 的部分唯一索引
//! `uk_biz_material_serial_no (serial_no) WHERE deleted_at IS NULL`。
//! **`deleted_at IS NULL` 必须写在查询里**：部分索引的可命中性依赖它，
//! 少了这一句 planner 就不能用该索引（会退化成 Seq Scan + Filter）。

use nik::prelude::chrono::{DateTime, Duration, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::rust_decimal::RoundingStrategy;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::normalize_page;
use crate::business::erp::serial_dto::SerialFilter;
use crate::business::erp::stockin_model::{self, CostBasis};
use crate::business::erp::voucher::{self, SerialMovement};
use crate::business::shared::scope;

use super::inventory_writer::{self, TxnRecord};

/// 金额 / 单价舍入策略：**四舍五入**（与 PG `ROUND(numeric, n)` 同口径）。
///
/// `rust_decimal` 默认的 `round_dp` 是「四舍六入五成双」，`.005` 边界会比库里少 1 分钱。
/// 本文件的舍入**只用于展示量**（流水的前后快照 `avg`），落库数字一律由 SQL 侧 `ROUND` 产出。
const ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

/// 零 UUID（`COALESCE(location_id, 零 uuid)` 的唯一键 / 锁顺序口径）。
const ZERO_UUID: &str = "00000000-0000-0000-0000-000000000000";

// ==================== 口径常量 ====================

/// 表名（分页锚点 / 报错文案共用）。
pub const SERIAL_TABLE: &str = "biz_material_serial";

/// `biz_material_serial` 列清单（所有 SELECT / RETURNING 共用，顺序与 DDL 一致）。
pub const SERIAL_COLUMNS: &str = "id, serial_no, material_id, warehouse_id, location_id, batch_no, \
     status, inbound_at, outbound_at, source_type, source_id, created_at, updated_at, \
     create_by, update_by, deleted_at";

/// 在库。
pub const STATUS_IN_STOCK: &str = "IN_STOCK";
/// 已出库。
pub const STATUS_OUTBOUND: &str = "OUTBOUND";
/// 已报废。
pub const STATUS_SCRAPPED: &str = "SCRAPPED";

/// 三态全集（与 V0138 的 `ck_biz_material_serial_status` 逐字一致）。
pub const ALL_STATUSES: [&str; 3] = [STATUS_IN_STOCK, STATUS_OUTBOUND, STATUS_SCRAPPED];

/// 取号规则键（V0138 §3 种入的那一行）。
pub const SEQ_TYPE: &str = "SN";

/// 序列号前缀。
pub const SN_PREFIX: &str = "SN";

/// 流水段宽度（补零到 6 位）。
pub const SN_SEQ_WIDTH: usize = 6;

/// 单次批量生成上限（卡面「批量生成 1000 条」；超过即拒，避免一次锁太久）。
pub const MAX_BATCH_COUNT: i64 = 1000;

/// 北京时间相对 UTC 的偏移（号按天重置用的是 **CST** 的「天」，不是 UTC 的天）。
pub const CST_OFFSET_HOURS: i64 = 8;

/// 来源单据类型白名单（V0138 列注释：PURCHASE / TRANSFER / PRODUCE / RETURN）。
pub const SOURCE_TYPES: [&str; 4] = ["PURCHASE", "TRANSFER", "PRODUCE", "RETURN"];

/// 分页排序：号大（新）的在前；`id` 兜底保证翻页稳定。
pub const SERIAL_ORDER: &str = "ORDER BY serial_no DESC, id ASC";

/// 分页锚点（序列号表不参与数据权限，理由见文件头 `SCOPE-EXEMPT`）。
pub const SERIAL_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(SERIAL_TABLE);

/// 扫码查询（9.5 万行实测走 `uk_biz_material_serial_no` 的 Index Scan，见
/// `scripts/verify/w2b_serial_api.sh` 的 EXPLAIN 段）。
///
/// `m.deleted_at` **不过滤**：物料被软删后历史序列号仍要能扫出来溯源
/// （物料行物理还在，`INNER JOIN` 不会丢行）。
pub const SCAN_SQL: &str = "SELECT s.id, s.serial_no, s.material_id, m.material_code, m.material_name, \
     s.warehouse_id, w.warehouse_name, s.location_id, l.location_name, \
     s.batch_no, s.status, s.inbound_at, s.outbound_at \
     FROM biz_material_serial s \
     JOIN biz_material m ON m.id = s.material_id \
     LEFT JOIN biz_warehouse w ON w.id = s.warehouse_id \
     LEFT JOIN biz_inventory_location il ON il.id = s.location_id \
     LEFT JOIN biz_location l ON l.id = il.location_id \
     WHERE s.serial_no = ? AND s.deleted_at IS NULL";

// ==================== 行结构 ====================

/// `biz_material_serial` 行（16 列，与 V0138 逐列对齐）。
#[derive(Debug, Clone)]
pub struct SerialRow {
    /// 主键。
    pub id: Uuid,
    /// 系统内码 `SN-yyyyMMdd-XXXXXX`（未删除内全局唯一）。
    pub serial_no: String,
    /// 物料。
    pub material_id: Uuid,
    /// 当前 / 最后所在仓库。
    pub warehouse_id: Option<Uuid>,
    /// 当前所在库位（指向 `biz_inventory_location.id`）。
    pub location_id: Option<Uuid>,
    /// 批次号。
    pub batch_no: Option<String>,
    /// 状态：`IN_STOCK` / `OUTBOUND` / `SCRAPPED`。
    pub status: String,
    /// 首次入库时间。
    pub inbound_at: Option<DateTime<Utc>>,
    /// 出库 / 报废时间。
    pub outbound_at: Option<DateTime<Utc>>,
    /// 来源单据类型。
    pub source_type: Option<String>,
    /// 来源单据 id（弱引用）。
    pub source_id: Option<Uuid>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 未删除）。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl SerialRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            serial_no: row.get_typed("serial_no")?,
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            location_id: row.get_typed("location_id")?,
            batch_no: row.get_typed("batch_no")?,
            status: row.get_typed("status")?,
            inbound_at: row.get_typed("inbound_at")?,
            outbound_at: row.get_typed("outbound_at")?,
            source_type: row.get_typed("source_type")?,
            source_id: row.get_typed("source_id")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// 扫码结果（序列号 + 物料 + 仓库 + 库位的**扁平**出参，卡面 §二 ③ 的字段逐条对应）。
#[derive(Debug, Clone)]
pub struct SerialScanRow {
    /// 主键。
    pub id: Uuid,
    /// 序列号。
    pub serial_no: String,
    /// 物料 id。
    pub material_id: Uuid,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库 id。
    pub warehouse_id: Option<Uuid>,
    /// 仓库名称。
    pub warehouse_name: Option<String>,
    /// 库位库存行 id。
    pub location_id: Option<Uuid>,
    /// 库位名称（经 `biz_inventory_location` 一跳取 `biz_location.location_name`）。
    pub location_name: Option<String>,
    /// 批次号。
    pub batch_no: Option<String>,
    /// 状态。
    pub status: String,
    /// 入库时间。
    pub inbound_at: Option<DateTime<Utc>>,
    /// 出库 / 报废时间。
    pub outbound_at: Option<DateTime<Utc>>,
}

impl SerialScanRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            serial_no: row.get_typed("serial_no")?,
            material_id: row.get_typed("material_id")?,
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            warehouse_name: row.get_typed("warehouse_name")?,
            location_id: row.get_typed("location_id")?,
            location_name: row.get_typed("location_name")?,
            batch_no: row.get_typed("batch_no")?,
            status: row.get_typed("status")?,
            inbound_at: row.get_typed("inbound_at")?,
            outbound_at: row.get_typed("outbound_at")?,
        })
    }
}

/// 批量生成入参（数据层口径；`Uuid` 已解析好，非法 UUID 在 dto 层就拒）。
#[derive(Debug, Clone)]
pub struct GenerateParams<'a> {
    /// 物料（调用方已校验 `serial_required = TRUE`）。
    pub material_id: Uuid,
    /// 仓库（可空）。
    pub warehouse_id: Option<Uuid>,
    /// 库位库存行（可空）。
    pub location_id: Option<Uuid>,
    /// 批次号（可空）。
    pub batch_no: Option<&'a str>,
    /// 来源单据类型（可空，白名单见 [`SOURCE_TYPES`]）。
    pub source_type: Option<&'a str>,
    /// 来源单据 id（可空）。
    pub source_id: Option<Uuid>,
    /// 生成条数（1..=[`MAX_BATCH_COUNT`]）。
    pub count: i64,
    /// 成本单价（不含税，6 位；ADR-20261031：批量生成 = 无来源入库，必须录成本）。
    ///
    /// 由 `serial_dto::BatchGenerateReq::parse_unit_cost` 校验并舍入后传入 ——
    /// 模型层不再做「缺省 0」的兜底：零成本入库会让巡检 C3（`qty>0 且 avg_cost=0`）必红。
    pub unit_cost: Decimal,
}

/// 批量生成的结果：回读的号行 + 本次入账口径（供接口层回显，不需要再查一次库）。
#[derive(Debug, Clone)]
pub struct BatchOutcome {
    /// 回读的号行（按号升序）。
    pub rows: Vec<SerialRow>,
    /// 本次成本单价（6 位）。
    pub unit_cost: Decimal,
    /// 本次入账金额 = 该成本桶 `amount` 的真实增量（4 位，取自 `apply_cost` 的 RETURNING）。
    pub amount: Decimal,
    /// 记账后该成本桶的快照（`qty / amount / avg_cost`）。
    pub inventory: InventorySnapshot,
}

/// 状态流转结果（`NotFound` / `Illegal` 都由接口层翻译成对应 HTTP 码，不做 `Err` 短路）。
#[derive(Debug, Clone)]
pub enum TransitionOutcome {
    /// 已更新（带旧状态，便于审计日志）。
    Updated {
        /// 流转前的状态。
        from: String,
        /// 更新后的整行（装箱：`SerialRow` 300+ 字节，直接内联会让枚举整体偏大）。
        row: Box<SerialRow>,
    },
    /// 行不存在 / 已软删。
    NotFound,
    /// 非法流转（不可逆状态机拒绝）。
    Illegal {
        /// 当前状态。
        from: String,
        /// 请求的目标状态。
        to: String,
    },
}

// ==================== 纯逻辑（无数据库，单测直接覆盖） ====================

/// 把「某一刻」换算成北京时间的周期键 `yyyyMMdd`。
///
/// 号按天重置必须用 **CST 的天**：用 UTC 会让「北京时间 00:00–08:00」这段算成前一天，
/// 跨天那一刻的号段会跟白天重叠。
pub fn period_key_of(now: DateTime<Utc>) -> String {
    (now + Duration::hours(CST_OFFSET_HOURS))
        .format("%Y%m%d")
        .to_string()
}

/// 当前周期键（北京时间今天）。
pub fn today_period_key() -> String {
    period_key_of(Utc::now())
}

/// 拼序列号：`SN-yyyyMMdd-XXXXXX`。
///
/// 6 位只补零不截断：流水超过 999999 时自然变成 7 位（一天 100 万件单品已超出本方案
/// 的设计容量，宁可长出 1 位也不要撞号或截断）。
pub fn format_serial_no(period_key: &str, no: i64) -> String {
    format!("{SN_PREFIX}-{period_key}-{no:0SN_SEQ_WIDTH$}")
}

/// 状态机：**单向不可逆**（卡面 §二 ④）。
///
/// | 从 \ 到 | `OUTBOUND` | `SCRAPPED` |
/// |---|---|---|
/// | `IN_STOCK` | ✅ | ✅ |
/// | `OUTBOUND` | — | ✅ |
/// | `SCRAPPED` | ❌ | ❌ |
///
/// 同一状态重复提交也算非法（`IN_STOCK → IN_STOCK`）：幂等重放在这里没有业务意义，
/// 放行只会掩盖前端重复点击。
pub fn can_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("IN_STOCK", "OUTBOUND") | ("IN_STOCK", "SCRAPPED") | ("OUTBOUND", "SCRAPPED")
    )
}

/// 状态流转被拒时的中文文案（接口层 400 的 `msg`）。
pub fn transition_rejected_message(from: &str, to: &str) -> String {
    match (from, to) {
        ("OUTBOUND", "IN_STOCK") => {
            "已出库的序列号不能回退到在库：要重新入库请生成新的序列号".to_string()
        }
        ("SCRAPPED", _) => {
            format!("已报废的序列号不能再流转（当前 {from} → 目标 {to}），状态机不可逆")
        }
        _ => format!(
            "非法状态流转 {from} → {to}：只允许 IN_STOCK→OUTBOUND、IN_STOCK→SCRAPPED、OUTBOUND→SCRAPPED"
        ),
    }
}

// ==================== 取号 ====================

/// 批量落库载荷：每行 `id|serial_no`，行间用换行分隔。
///
/// # 为什么不用数组参数
///
/// `Value::Text("{a,b}")` 绑 `?::text[]` 会在驱动侧报 `error serializing parameter N`
/// （PG 把占位符推断成 `text[]`，驱动只有 `text` 的编码器 —— 实测 1000 条批量生成直接 500）。
/// 改成「**一个 text 参数 + SQL 侧 `string_to_array` / `split_part`**」：
/// 参数类型单一（`text`），1000 条一行不落，SQL 侧拆行拆列，实测稳。
///
/// 分隔符选 `|` 与 `\n`：序列号字符集是 `[A-Z0-9-]`，UUID 是十六进制 + 连字符，两者都不会出现这两个字符。
pub fn pair_payload(pairs: &[(Uuid, String)]) -> String {
    pairs
        .iter()
        .map(|(id, serial_no)| format!("{id}|{serial_no}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 可空 UUID 参数（`Value::Null` 绑 `::uuid` 驱动编不出来 → `NULLIF(?::text,'')::uuid`）。
fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// `biz_seq(seq_type='SN')` 那一行不存在时幂等补一行（V0138 已种，这里只是兜底：
/// 手工清过序列表 / 老库跳过 V0138 时不会 500）。并发下唯一索引会拦重复 → 视为成功。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列，不需要数据范围。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'SN', '单品序列号', 'SN', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '一物一码单品序列号：SN-yyyyMMdd-000001，按天（CST）滚动' \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'SN' AND deleted_at IS NULL)"
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

/// 一次 `UPDATE … RETURNING` 推进 `count` 个号，返回「本段之前的值」；`None` = 序列表没这行。
///
/// # 三条分支（安全性的核心）
///
/// ```text
/// period_key =  本次  → 接着上一段（current_no + n）
/// period_key <  本次  → 进入更新的一天：重置为 n（本段号 1..=n）
/// period_key >  本次  → 周期键回退（时钟回拨 / 手工重放 / 测试用假周期）
///                        → **绝不重置**，继续累加
/// ```
///
/// 第三条分支是**防重号的关键**：如果「与本次不同就重置」，那么周期键一旦
/// A → B → A 地来回跳（NTP 回拨、测试给假日期、运维手工改 `period_key`），
/// 回到 A 时会从 000001 重新发号，**与 A 当天已发出去的号整段撞车**
/// （本卡实测：单测用假周期键把 `period_key` 推来推去，把当天真实的 1000 个号撞了）。
/// 周期键是 `yyyyMMdd` 定宽，字典序 = 时间序，所以用 `<` / `>` 比较是安全的。
async fn bump_seq(ex: &impl SqlExecutor, count: i64, period_key: &str) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: 同上，biz_seq 是全局取号规则字典。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + ? \
                                      WHEN period_key < ? THEN ? \
                                      ELSE current_no + ? END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = 'SN' AND deleted_at IS NULL \
             RETURNING current_no - ? AS start"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(count),
                Value::from(period_key),
                Value::from(count),
                Value::from(count),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(count),
            ],
        )
        .await?;
    rows.first().map(|row| row.get_typed("start")).transpose()
}

/// 原子取号：占用 `count` 个连号，返回号段起点（本段号 = `start+1 ..= start+count`）。
///
/// 并发安全由 `UPDATE` 的行锁保证；跨天由 `CASE WHEN period_key = ?` 重置保证。
/// W2c-1 起改为吃 `&impl SqlExecutor`：批量生成整条链路（取号 → 落库 → 流水 → 重算）
/// 必须落在**同一个事务**里，取号失败/落库失败才能一起回滚（不会白吃一段号）。
pub async fn take_numbers(ex: &impl SqlExecutor, count: i64, period_key: &str) -> DbResult<i64> {
    if count <= 0 {
        return Err(DbError::InvalidValue(format!(
            "取号条数必须 > 0（收到 {count}）"
        )));
    }
    if count > MAX_BATCH_COUNT {
        return Err(DbError::InvalidValue(format!(
            "单次最多取 {MAX_BATCH_COUNT} 个号（收到 {count}）"
        )));
    }
    if let Some(start) = bump_seq(ex, count, period_key).await? {
        return Ok(start);
    }
    ensure_seq_row(ex).await?;
    bump_seq(ex, count, period_key).await?.ok_or_else(|| {
        DbError::InvalidValue(
            "biz_seq 里没有 seq_type='SN' 的取号规则（V0138 应已种入），无法生成序列号".to_string(),
        )
    })
}

// ==================== 仓储联动：数量由序列号派生（不变式①②） ====================

/// 流水方向：入（`ck_biz_inv_txn_type` 只认这两个值）。
pub const TXN_TYPE_IN: &str = "IN";
/// 流水方向：出。
pub const TXN_TYPE_OUT: &str = "OUT";

/// 业务类型：批量生成（一物一码入库）。
pub const BIZ_TYPE_SERIAL_IN: &str = "SERIAL_IN";
/// 业务类型：序列号出库。
pub const BIZ_TYPE_SERIAL_OUT: &str = "SERIAL_OUT";
/// 业务类型：在库序列号报废（`txn_type` 仍是 `OUT`，方向由它定）。
pub const BIZ_TYPE_SERIAL_SCRAP: &str = "SERIAL_SCRAP";

/// 单据类型：单件（一张流水对一条序列号）。
pub const DOC_TYPE_SERIAL: &str = "MATERIAL_SERIAL";
/// 单据类型：批量生成（一张流水对 N 条序列号）。
pub const DOC_TYPE_SERIAL_BATCH: &str = "SERIAL_BATCH";

/// `biz_inventory_txn.remark` 列宽（V0137：`VARCHAR(255)`）。
///
/// 模型层**不做截断**：超长由 DB 报错、事务整体回滚（宁可失败也不悄悄丢审计信息）；
/// 接口层把 `reason` 限在 200 字（[`crate::business::erp::serial_dto::MAX_REASON_LEN`]）。
pub const TXN_REMARK_MAX: usize = 255;

/// 「某一刻」的会计期间码 `yyyy-MM`（同样用 **CST**：月末 23:30 的流水不该记到上个月）。
pub fn period_code_of(now: DateTime<Utc>) -> String {
    (now + Duration::hours(CST_OFFSET_HOURS))
        .format("%Y-%m")
        .to_string()
}

/// 库存快照（流水的前后六列里，变动前的三项）。
#[derive(Debug, Clone, Copy)]
pub struct InventorySnapshot {
    /// 变动前账面数量。
    pub qty: Decimal,
    /// 变动前账面金额。
    pub amount: Decimal,
    /// 变动前移动加权单价。
    pub avg_cost: Decimal,
}

impl InventorySnapshot {
    /// 全 0（`biz_inventory` 还没有该物料行时的口径）。
    pub fn zero() -> Self {
        Self {
            qty: Decimal::ZERO,
            amount: Decimal::ZERO,
            avg_cost: Decimal::ZERO,
        }
    }
}

/// 仓级快照的移动加权单价（**只用于流水的 `before_cost` / `after_cost` 展示**，
/// 绝不写回 `biz_inventory.avg_cost` —— 那是 SQL 侧 `ROUND(…,6)` 的唯一产物）。
fn avg_of(qty: Decimal, amount: Decimal) -> Decimal {
    if qty.is_zero() {
        Decimal::ZERO
    } else {
        (amount / qty).round_dp_with_strategy(6, ROUNDING)
    }
}

/// 读 `(物料, 仓库)` 的**仓级聚合**快照；`lock = true` 时按固定顺序锁住全部桶行。
///
/// # 为什么是「聚合」而不是「一行」（旧实现的坑）
///
/// V0139 之后 `biz_inventory` 的行粒度是「物料 × 仓库 × 库位」，同一 (物料, 仓库)
/// 可以有多行（NULL 汇总桶 + 若干库位桶）。而流水的 `before_*` / `after_*` 六列是**仓级**的
/// （`erp_invariants.py` 的 B4 也只按仓级求和）。旧实现用 `query_first_sql` 读一行，
/// 多桶时只拿到其中一行 ⇒ 前后快照与账面自相矛盾。
///
/// # 锁顺序（全局唯一，防死锁）
///
/// `ORDER BY COALESCE(location_id, 零 uuid)` —— 与 `stockin_model::apply_cost` 的
/// `ON CONFLICT` 表达式同形。`FOR UPDATE` 不能直接配聚合函数，所以锁在子查询里拿。
async fn read_warehouse_snapshot(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    lock: bool,
) -> DbResult<InventorySnapshot> {
    // SCOPE-EXEMPT: 库存是仓储数据，无归属列（口径同 erp/model.rs）。
    let sql = if lock {
        format!(
            "SELECT COALESCE(SUM(t.qty), 0) AS qty, COALESCE(SUM(t.amount), 0) AS amount \
               FROM (SELECT qty, amount FROM biz_inventory \
                      WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                      ORDER BY COALESCE(location_id, '{ZERO_UUID}'::uuid) FOR UPDATE) t"
        )
    } else {
        "SELECT COALESCE(SUM(qty), 0) AS qty, COALESCE(SUM(amount), 0) AS amount \
           FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid"
            .to_string()
    };
    let row = ex
        .query_first_sql(
            sql,
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await?
        .ok_or_else(|| {
            DbError::InvalidValue(
                "库存快照查询未返回行（聚合查询不应为空，请检查连接）".to_string(),
            )
        })?;
    let qty: Decimal = row.get_typed("qty")?;
    let amount: Decimal = row.get_typed("amount")?;
    Ok(InventorySnapshot {
        qty,
        amount,
        avg_cost: avg_of(qty, amount),
    })
}

/// 锁读仓级快照（成本账行 + 库位账行的加锁入口）。
async fn lock_warehouse_snapshot(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<InventorySnapshot> {
    read_warehouse_snapshot(ex, material_id, warehouse_id, true).await
}

/// 读仓级快照（不额外加锁 —— 调用方已经在同一事务里锁过了）。
async fn warehouse_snapshot(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<InventorySnapshot> {
    read_warehouse_snapshot(ex, material_id, warehouse_id, false).await
}

/// 某成本桶的移动加权单价（出库 / 报废流水的 `unit_cost`）；桶行不存在 = 0。
///
/// 桶键是 `biz_inventory.location_id`（行 id，NULL = 仓级汇总桶），
/// 与序列号的 `location_id` **同一个 id 空间**，可以直连（规格 v3 §4.1）。
async fn bucket_avg_cost(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
) -> DbResult<Decimal> {
    // SCOPE-EXEMPT: 库存是仓储数据，无归属列。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT avg_cost FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                    AND COALESCE(location_id, '{ZERO_UUID}'::uuid) \
                      = COALESCE(NULLIF(?::text,'')::uuid, '{ZERO_UUID}'::uuid)"
            ),
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                uuid_text(location_id),
            ],
        )
        .await?;
    match row {
        Some(row) => row.get_typed("avg_cost"),
        None => Ok(Decimal::ZERO),
    }
}

/// **不变式①（按桶版）**：把 `(物料, 仓库)` 的两本账对齐成「每个桶的在库件数」。
///
/// 出库 / 报废的**唯一收尾**（`transition_status_with` 的 ⑤ 步）。语义：
///
/// * **成本账**（`biz_inventory`）：以**自身行**驱动 —— `qty := 该桶的在库号件数`、
///   `amount := qty × 该桶 avg_cost`（`avg_cost` 不动：出库 / 报废只减量、不改移动加权单价）；
/// * **库位账**（`biz_inventory_location`）：同样以**自身行**驱动 —— `qty := 该桶的在库号件数`。
///   注意 `il.location_id` 是**库位字典 id**，而序列号的 `location_id` 是**库位账行 id**，
///   所以匹配条件是 `s.location_id = il.id`（NULL 桶单独匹配 `location_id IS NULL`）。
///   拿序列号的 `location_id` 去 upsert 库位账会多插一条「把行 id 当字典 id」的行，
///   实测库位账 13 vs 成本账 10（规格 v3 §1.4(e)）。
///
/// # 为什么用「自身行驱动的 UPDATE」而不是规格 §5.2 的 upsert 写法
///
/// §5.2 给的是 `INSERT … SELECT count(*) GROUP BY 桶 … ON CONFLICT DO UPDATE`。那种写法
/// **漏了「桶里最后一件事也走了」的情形**：某个桶的号全部出库后，该桶不再出现在 `GROUP BY`
/// 的结果里，成本账那一行会**保留旧 qty**（例如 1 件），而库位账已经是 0 ⇒ B1 直接红。
/// 本实现改成「先在成本账侧按桶对齐（含归零），再核对有没有『有件数但无成本账行』的桶」：
/// 语义等价、且天然覆盖「归零」与「首次建桶」两种边界。
///
/// # 记账断链 = 报错回滚，不静默造行
///
/// 桶里有在库件数、成本账却没有对应行 ⇒ 说明有人绕过记账写了序列号（或历史脏数据）。
/// 这时「补一行 avg_cost=0」会让巡检 C3（`qty>0 且 avg_cost=0`）**永远红**，
/// 所以这里直接 `InvalidValue` 整单回滚，把问题留在入口而不是留在账上。
/// （**注意**：`qty>0 且 avg_cost=0` 的**已存在**行不报错 —— 那是改判前生成的历史零成本
/// 存量，按「存量不补账」口径必须能正常出库，见规格 §8.2 / 单测 T16。）
///
/// # 返回值
///
/// 重算后的**仓级** qty（= `Σ` 桶 qty = 在库件数）。
pub async fn recompute_quantity(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<Decimal> {
    // ① 锁顺序（全局唯一）：成本账全部桶行 → 库位账全部行，都按 COALESCE(location_id, 零 uuid) 升序。
    lock_warehouse_snapshot(ex, material_id, warehouse_id).await?;
    ex.query_sql(
        format!(
            "SELECT qty FROM biz_inventory_location \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
              ORDER BY COALESCE(location_id, '{ZERO_UUID}'::uuid) FOR UPDATE"
        ),
        vec![Value::from(&material_id), Value::from(&warehouse_id)],
    )
    .await?;

    // ② 有件数但成本账没有桶行 ⇒ 记账断链，整单回滚（理由见函数文档）。
    let orphans: i64 = ex
        .scalar_sql(
            format!(
                "SELECT count(*) FROM (SELECT DISTINCT location_id FROM biz_material_serial \
                    WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                      AND status = 'IN_STOCK' AND deleted_at IS NULL) s \
                  WHERE NOT EXISTS (SELECT 1 FROM biz_inventory i \
                                     WHERE i.material_id = ?::uuid AND i.warehouse_id = ?::uuid \
                                       AND COALESCE(i.location_id, '{ZERO_UUID}'::uuid) \
                                         = COALESCE(s.location_id, '{ZERO_UUID}'::uuid))"
            ),
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                Value::from(&material_id),
                Value::from(&warehouse_id),
            ],
        )
        .await?;
    if orphans > 0 {
        return Err(DbError::InvalidValue(format!(
            "成本账桶缺失：物料 {material_id} / 仓库 {warehouse_id} 有 {orphans} 个库位桶存在在库件数\
             但没有成本账行（记账断链），请先按库存巡检修复再操作"
        )));
    }

    // ③ 成本账：逐桶对齐成在库件数（有在库件的桶对齐、没有在库件的桶归零；avg_cost 不动）。
    // ④ 库位账：以自身行驱动对齐成在库件数。
    //
    // P2-5A：这三段（① + ①b 必须成对、漏一条就留下「号走了账还在」）收口到唯一写入口
    // [`inventory_writer::sync_cost_qty_from_serials`] /
    // [`inventory_writer::sync_location_qty_from_serials`]。
    inventory_writer::sync_cost_qty_from_serials(ex, material_id, warehouse_id).await?;
    inventory_writer::sync_location_qty_from_serials(ex, material_id, warehouse_id).await?;

    // ⑤ 返回仓级 qty。
    let total: Decimal = ex
        .scalar_sql(
            "SELECT COALESCE(SUM(qty), 0) FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid"
                .to_string(),
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await?;
    Ok(total)
}

/// 一条库存流水的全部业务入参（写库时展开成 22 列）。
#[derive(Debug, Clone)]
pub struct TxnNote<'a> {
    /// 方向：`IN` / `OUT`。
    pub txn_type: &'a str,
    /// 业务类型：`SERIAL_IN` / `SERIAL_OUT` / `SERIAL_SCRAP`。
    pub biz_type: &'a str,
    /// 单据类型：`SERIAL_BATCH` / `MATERIAL_SERIAL`。
    pub doc_type: &'a str,
    /// 单据主键（批量生成 = 首件 id；单件流转 = 该序列号 id）。
    pub doc_id: Uuid,
    /// 单据号（批量生成 = 首号；单件流转 = 序列号本身）。
    pub doc_no: String,
    /// 备注（`reason` 或状态文案；超长由 DB 拒、事务回滚）。
    pub remark: Option<String>,
    /// 操作人。
    pub operator: Option<Uuid>,
    /// 记账时刻（Rust 侧算好再绑，保证流水时间与期间码同源）。
    pub now: DateTime<Utc>,
    /// 本次单价（6 位）。生成 = 录入的成本单价；出库 / 报废 = 该桶的 `avg_cost`。
    pub unit_cost: Decimal,
    /// 本次金额（4 位，**账上真实差额**，不是 `qty × avg_cost` 重算）。
    pub amount: Decimal,
    /// 变动前**仓级**快照。
    pub before: InventorySnapshot,
    /// 变动后**仓级**快照。
    pub after: InventorySnapshot,
}

/// 写一条库存流水（**只增不改**）。
///
/// `qty` 恒为正数（`ck_biz_inv_txn_qty` 禁负数），方向看 `txn_type`。
///
/// # 与旧实现的三个差别（P2-B1 踩过的坑，规格 v3 §3.7）
///
/// 1. **`INSERT … VALUES` 而非 `INSERT … SELECT … FROM biz_inventory`**：
///    旧写法没有桶条件（`WHERE i.material_id = … AND i.warehouse_id = …`），同一物料同仓有两个桶时
///    会写**两条**流水、每条 `qty = N` ⇒ B4（BLOCKER）双计。现在一条语句恰好一行。
/// 2. **`unit_cost` / `amount` 写实值**：旧实现恒取 `i.avg_cost` 并把金额算成
///    `qty × avg_cost` —— 生成时 `avg_cost` 还没含本次单价，台账金额是错的（P10 残留判据
///    ① 靠 `SERIAL_IN + SERIAL_BATCH` 识别这类流水，所以形态保留、数值必须写实）。
/// 3. **前后快照是仓级聚合**（`before` / `after` 由调用方在同一事务里读），多桶时只有仓级求和
///    才有意义（B4 也只按仓级求和）。
///
/// 影响行数必须恰为 1，否则 `InvalidValue` 整单回滚 —— 双写/漏写都在这里拦住。
async fn post_serial_txn(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    note: &TxnNote<'_>,
    qty: i64,
) -> DbResult<()> {
    // P2-5A：流水 SQL 收口到 [`inventory_writer::insert_txn`]；本函数只保留
    // 「影响行数必须恰为 1」的自检（双写/漏写都在这里拦住整单回滚）。
    let snapshot = |s: InventorySnapshot| super::stockin_base::InventorySnapshot {
        qty: s.qty,
        amount: s.amount,
        avg_cost: s.avg_cost,
    };
    let affected = inventory_writer::insert_txn(
        ex,
        &TxnRecord {
            material_id,
            warehouse_id,
            txn_type: note.txn_type,
            biz_type: note.biz_type,
            doc_type: note.doc_type,
            doc_id: note.doc_id,
            doc_no: note.doc_no.as_str(),
            line_no: 1,
            qty: Decimal::from(qty).round_dp(4),
            unit_cost: note.unit_cost.round_dp_with_strategy(6, ROUNDING),
            amount: note.amount.round_dp(4),
            before: snapshot(note.before),
            after: snapshot(note.after),
            operator: note.operator,
            period_code: None,
            remark: note.remark.as_deref(),
            now: note.now,
        },
    )
    .await?;
    if affected != 1 {
        return Err(DbError::InvalidValue(format!(
            "库存流水写入行数异常：期望 1，实际 {affected}（物料 {material_id} / 仓 {warehouse_id}）"
        )));
    }
    Ok(())
}
// ==================== 批量生成 ====================

/// 批量生成序列号 = **无来源入库（期初 / 其他入库）**：取号 + 落号 + 两本账 + 凭证 + 一条流水。
///
/// 为什么用「一条 INSERT + text 载荷」而不是 1000 条 `INSERT`：1000 个往返实测 1.5–2 s，
/// 单条语句 < 100 ms。载荷格式见 [`pair_payload`]（数组参数绑不上，见那里的说明）。
///
/// 号码先取后插，**插入失败会浪费掉这一小段号**（序列号允许跳号，
/// 业务上不可接受的是重号）——这条取舍写在证据文档里。
///
/// # ADR-20261031 的口径（一次调用 = 一笔入库业务）
///
/// ```text
/// ① take_numbers                  取号（号段浪费可接受，绝不重号）
/// ② INSERT … SELECT 落 N 个号      status='IN_STOCK'（形态一字未改）
///    断言影响行数 == N            ≠ N ⇒ 整单回滚（记账用的是「请求条数」）
/// ③ stockin_model::apply_cost     成本账：加法 + SQL 里 ROUND(…,6) 算移动加权
/// ④ stockin_model::apply_location_qty  库位账：加法（有库位行按主键、无则 NULL 汇总桶）
/// ⑤ voucher::create_for_serial_movement 凭证：借 1405 / 贷 1901（金额 = ③ 的真实增量）
/// ⑥ post_serial_txn               一条 SERIAL_IN / SERIAL_BATCH 流水，单价与金额写实值
/// ```
///
/// **禁止**在这里调 [`recompute_quantity`]：那是绝对值重算（`qty := 在库号件数`），
/// 与 ③ 的加法叠加就是**双计**（实测 9 vs 5）；而且请求带 `location_id` 时重算只写 NULL 桶，
/// 会把库位桶的加法抹掉。不变式①靠「落号 N == 记账 N」的增量等式保证（规格 v3 §3.5）。
///
/// **唯一写数量的地方是 ③/④**，顺序也固定：成本账先锁行、库位账后锁行 ——
/// 与收货过账（`stockin_handler` 的 `apply_cost` → `apply_location_qty` → `write_txn`）同一口径，
/// 两个入口并发收同一物料时只会在成本账行上排队。
pub async fn batch_generate(
    ex: &impl SqlExecutor,
    params: &GenerateParams<'_>,
    period_key: &str,
    operator: Option<Uuid>,
) -> DbResult<BatchOutcome> {
    if params.count <= 0 {
        return Err(DbError::InvalidValue("生成条数必须 > 0".to_string()));
    }
    if params.unit_cost <= Decimal::ZERO {
        return Err(DbError::InvalidValue(
            "批量生成必须录成本单价（> 0）：零成本入库会在成本账留下 qty>0/avg_cost=0 的行"
                .to_string(),
        ));
    }
    let Some(warehouse_id) = params.warehouse_id else {
        return Err(DbError::InvalidValue(
            "一物一码物料批量生成必须指定仓库：账面数量按 物料×仓库 由序列号派生（不变式①）"
                .to_string(),
        ));
    };
    let now = Utc::now();
    let start = take_numbers(ex, params.count, period_key).await?;
    let ids: Vec<Uuid> = (0..params.count).map(|_| Uuid::now_v7()).collect();
    let serial_nos: Vec<String> = (1..=params.count)
        .map(|offset| format_serial_no(period_key, start + offset))
        .collect();
    let doc_id = ids[0];
    let doc_no = serial_nos[0].clone();
    let first_no = format_serial_no(period_key, start + 1);
    let pairs: Vec<(Uuid, String)> = ids.iter().copied().zip(serial_nos).collect();
    let payload = pair_payload(&pairs);
    let qty = Decimal::from(params.count);

    // 记账前的仓级快照（同时锁住全部桶行）：流水的 before_* 用它，不与记账顺序耦合。
    let before = lock_warehouse_snapshot(ex, params.material_id, warehouse_id).await?;

    let inserted = ex
        .exec_sql(
            "INSERT INTO biz_material_serial \
                 (id, serial_no, material_id, warehouse_id, location_id, batch_no, status, inbound_at, \
                  source_type, source_id, create_by, update_by) \
             SELECT split_part(t.line, '|', 1)::uuid, split_part(t.line, '|', 2), ?::uuid, \
                    NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid, NULLIF(?::text, ''), \
                    'IN_STOCK', now(), NULLIF(?::text, ''), NULLIF(?::text, '')::uuid, \
                    NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid \
               FROM unnest(string_to_array(?, chr(10))) AS t(line)"
                .to_string(),
            vec![
                Value::from(&params.material_id),
                uuid_text(params.warehouse_id),
                uuid_text(params.location_id),
                Value::Text(params.batch_no.unwrap_or_default().to_string()),
                Value::Text(params.source_type.unwrap_or_default().to_string()),
                uuid_text(params.source_id),
                uuid_text(operator),
                uuid_text(operator),
                Value::Text(payload),
            ],
        )
        .await?;
    // 记账用的是「请求条数 N」：插入 ≠ N 就会凭空多记/少记，必须整单回滚。
    if inserted != u64::try_from(params.count).unwrap_or(u64::MAX) {
        return Err(DbError::InvalidValue(format!(
            "批量生成落号条数不符：期望 {}，实际 {inserted}",
            params.count
        )));
    }

    // ③ 成本账：加法 + 移动加权（`avg_cost` 一律由 SQL 的 ROUND(…,6) 产出）。
    let cost = CostBasis {
        qty,
        amount: qty * params.unit_cost,
        unit_cost: params.unit_cost,
    };
    let applied = stockin_model::apply_cost(
        ex,
        params.material_id,
        warehouse_id,
        params.location_id,
        cost,
        now,
    )
    .await?;
    // ④ 库位账：加法（有库位行按主键、无则 NULL 汇总桶）。
    stockin_model::apply_location_qty(
        ex,
        params.material_id,
        warehouse_id,
        params.location_id,
        qty,
        now,
    )
    .await?;

    // 本次入账金额 = 该成本桶 `amount` 的真实增量（4 位）。Rust 侧不重算 `N × unit_cost`。
    let amount = (applied.after.amount - applied.before.amount).round_dp(4);
    let after = warehouse_snapshot(ex, params.material_id, warehouse_id).await?;

    // ⑤ 凭证（成本账一加金额，账上就必须有对应分录 —— 否则巡检 B2 必红）。
    voucher::create_for_serial_movement(
        ex,
        SerialMovement::Inbound {
            material_id: params.material_id,
            warehouse_id,
            count: params.count,
        },
        doc_id,
        &doc_no,
        amount,
        operator,
        now,
    )
    .await?;

    // ⑥ 一条流水（形态不变：IN + SERIAL_IN + SERIAL_BATCH；数值写实）。
    let note = TxnNote {
        txn_type: TXN_TYPE_IN,
        biz_type: BIZ_TYPE_SERIAL_IN,
        doc_type: DOC_TYPE_SERIAL_BATCH,
        doc_id,
        doc_no: doc_no.clone(),
        remark: Some(format!(
            "批量生成 {} 件（{first_no} 起）｜单价 {}｜金额 {}",
            params.count,
            // `params.unit_cost` 已在 `BatchGenerateReq::parse_unit_cost` 里定标到 6 位；
            // 这里**不要**再 `round_dp(6)` —— rust_decimal 的 `round_dp` 会把尾随 0 抹掉
            // （实测 12.340000 变成 12.34），而规格 §3.7 的 remark 形态要求逐字 `单价 12.340000`。
            params.unit_cost,
            amount
        )),
        operator,
        now,
        unit_cost: params.unit_cost,
        amount,
        before,
        after,
    };
    post_serial_txn(ex, params.material_id, warehouse_id, &note, params.count).await?;

    let rows = ex
        .query_sql(
            format!(
                "SELECT {SERIAL_COLUMNS} FROM biz_material_serial \
                  WHERE id IN (SELECT split_part(t.line, '|', 1)::uuid \
                                 FROM unnest(string_to_array(?, chr(10))) AS t(line)) \
                    AND deleted_at IS NULL ORDER BY serial_no ASC"
            ),
            vec![Value::Text(pair_payload(&pairs))],
        )
        .await?;
    let rows: Vec<SerialRow> = rows
        .iter()
        .map(SerialRow::from_row)
        .collect::<DbResult<_>>()?;
    Ok(BatchOutcome {
        rows,
        unit_cost: params.unit_cost,
        amount,
        // `apply_cost` 返回的是 `stockin_model::InventorySnapshot`（同名不同型），
        // 这里显式搬字段而不是 impl From —— 避免将来两边字段漂移时静默取错。
        inventory: InventorySnapshot {
            qty: applied.after.qty,
            amount: applied.after.amount,
            avg_cost: applied.after.avg_cost,
        },
    })
}

// ==================== 查询 ====================

/// 物料的一物一码开关（`None` = 物料不存在或已软删）。
///
/// 单独查一次而不复用 `erp::model::MaterialRow`：`MaterialRow` 的 18 个字段是 V0137 的
/// 定稿口径（W1b 写的时候 `serial_required` 还没建），为了一个布尔值去扩那张行结构
/// 会牵动 W1c 的 44 个接口；这里只取需要的列。
pub async fn material_serial_required(material_id: Uuid) -> DbResult<Option<bool>> {
    // SCOPE-EXEMPT: biz_material 是全局主数据，无归属列（口径同 erp/model.rs）。
    let rows = db()
        .find_with_params(
            "SELECT serial_required FROM biz_material WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&material_id)],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("serial_required"))
        .transpose()
}

/// 库位库存行的归属（批量生成时校验 `location_id` 与 `warehouse_id` 是否自洽）。
#[derive(Debug, Clone)]
pub struct InventoryLocationRef {
    /// 该库位库存行所属仓库。
    pub warehouse_id: Uuid,
    /// 具体库位（`biz_location.id`，可空 = 未分配库位）。
    pub location_id: Option<Uuid>,
}

/// 按主键取库位库存行的归属信息（`None` = 行不存在）。
///
/// 注意：`biz_material_serial.location_id` 的硬外键指向 **`biz_inventory_location.id`**，
/// 不是 `biz_location.id`（V0138 §2）。校验引用必须查这张表，否则会出现
/// 「传库位字典 id 也通过、但外键插入时报 23503」的糟糕体验。
pub async fn inventory_location_ref(id: Uuid) -> DbResult<Option<InventoryLocationRef>> {
    // SCOPE-EXEMPT: 库位库存是仓储结构数据，无归属列（口径同 erp/model.rs）。
    let rows = db()
        .find_with_params(
            "SELECT warehouse_id, location_id FROM biz_inventory_location WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await?;
    rows.first()
        .map(|row| {
            Ok(InventoryLocationRef {
                warehouse_id: row.get_typed("warehouse_id")?,
                location_id: row.get_typed("location_id")?,
            })
        })
        .transpose()
}

/// 按主键取序列号（软删过滤恒带）。
pub async fn serial_find_by_id(id: Uuid) -> DbResult<Option<SerialRow>> {
    // SCOPE-EXEMPT: 同文件头，biz_material_serial 无归属列。
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT {SERIAL_COLUMNS} FROM biz_material_serial WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(SerialRow::from_row).transpose()
}

/// 扫码定位：按 `serial_no` 精确查一条（未软删），命中 `uk_biz_material_serial_no`。
pub async fn serial_scan(serial_no: &str) -> DbResult<Option<SerialScanRow>> {
    // SCOPE-EXEMPT: 同文件头；本查询是扫码枪的单行点查，不需要也不能注入数据范围。
    let rows = db()
        .find_with_params(SCAN_SQL, vec![Value::from(serial_no.trim())])
        .await?;
    rows.first().map(SerialScanRow::from_row).transpose()
}

/// 序列号分页（物料 / 仓库 / 库位 / 状态 / 号模糊）。
///
/// 非法 UUID → `DbError::InvalidValue`（接口层转 `400 + 66006`）：**绝不静默跳过条件**
/// ——跳过等于「按物料筛」变成「返回全部」。
pub async fn serial_page(q: &SerialFilter) -> DbResult<(Vec<SerialRow>, i64)> {
    let mut sql =
        format!("SELECT {SERIAL_COLUMNS} FROM biz_material_serial WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(material_id) = q.material_id.as_deref() {
        sql.push_str(" AND material_id = ?::uuid");
        params.push(Value::from(parse_id(material_id, "material_id")?));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?::uuid");
        params.push(Value::from(parse_id(warehouse_id, "warehouse_id")?));
    }
    if let Some(location_id) = q.location_id.as_deref() {
        sql.push_str(" AND location_id = ?::uuid");
        params.push(Value::from(parse_id(location_id, "location_id")?));
    }
    if let Some(status) = q.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if !ALL_STATUSES.contains(&status) {
            return Err(DbError::InvalidValue(format!(
                "status 只能是 IN_STOCK / OUTBOUND / SCRAPPED（收到 {status}）"
            )));
        }
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(pattern) = crate::utils::like_pattern(q.serial_no.as_deref()) {
        sql.push_str(" AND serial_no LIKE ?");
        params.push(Value::from(pattern));
    }
    let (page, page_size) = normalize_page(q.page, q.page_size);
    let result = scope::paginate_unscoped(
        SERIAL_TARGET,
        &sql,
        params,
        Some(SERIAL_ORDER),
        page,
        page_size,
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(SerialRow::from_row)
        .collect::<DbResult<Vec<SerialRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// DTO 里的 id 是 `String`，落库前显式解析（与 `erp::model::parse_id` 同口径）。
fn parse_id(value: &str, field: &str) -> DbResult<Uuid> {
    Uuid::parse_str(value.trim())
        .map_err(|err| DbError::InvalidValue(format!("{field} 不是合法 UUID `{value}`：{err}")))
}

// ==================== 状态流转 ====================

/// 状态流转（`IN_STOCK → OUTBOUND|SCRAPPED`、`OUTBOUND → SCRAPPED`；其余非法）。
///
/// 实现要点：
/// - **事务内 `SELECT … FOR UPDATE`**：并发的两次 `PATCH` 不会都读到 `IN_STOCK`
///   然后各自「合法地」写一次，第二次必然看到第一次的结果并被状态机拒绝；
/// - 转 `OUTBOUND` / `SCRAPPED` 都写 `outbound_at`（V0138 注释：出库/报废时间），
///   用 `COALESCE(outbound_at, now())` 保证「先出库后报废」时保留出库时刻；
/// - `updated_by` 用操作人，`deleted_at` 不参与（软删行一律当不存在）；
/// - **W2c-1 不变式②**：同一事务里追加库存流水 + 按序列号重算账面数量（见 [`link_inventory`]）；
/// - 吃 `&impl SqlExecutor`：事务由接口层开（[`crate::business::erp::serial_handler::apply_status`]），
///   本函数不 `begin`、不 `commit`，也不允许在事务里再开事务。
pub async fn transition_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    target: &str,
    operator: Option<Uuid>,
) -> DbResult<TransitionOutcome> {
    transition_status_with(ex, id, target, operator, None).await
}

/// 带业务原因的状态流转：`reason` 写进库存流水的 `remark`（W2c-1 起有地方落了）。
///
/// `reason` **不截断**：`VARCHAR(255)` 超长会让流水 INSERT 失败、整个事务回滚
/// （接口层已把它限在 200 字，见 `serial_dto::MAX_REASON_LEN`）；
/// 这条「宁可整单失败也不悄悄丢审计信息」的取舍，同时是回滚用例的注入点。
pub async fn transition_status_with(
    ex: &impl SqlExecutor,
    id: Uuid,
    target: &str,
    operator: Option<Uuid>,
    reason: Option<&str>,
) -> DbResult<TransitionOutcome> {
    if !ALL_STATUSES.contains(&target) {
        return Err(DbError::InvalidValue(format!(
            "目标状态只能是 IN_STOCK / OUTBOUND / SCRAPPED（收到 {target}）"
        )));
    }
    let locked = ex
        .query_sql(
            format!(
                "SELECT {SERIAL_COLUMNS} FROM biz_material_serial \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    let current = locked.first().map(SerialRow::from_row).transpose()?;
    let Some(current) = current else {
        return Ok(TransitionOutcome::NotFound);
    };
    if !can_transition(&current.status, target) {
        return Ok(TransitionOutcome::Illegal {
            from: current.status,
            to: target.to_string(),
        });
    }
    let updated = ex
        .query_sql(
            format!(
                "UPDATE biz_material_serial \
                    SET status = ?, outbound_at = COALESCE(outbound_at, now()), \
                        updated_at = now(), update_by = NULLIF(?::text, '')::uuid \
                  WHERE id = ?::uuid AND deleted_at IS NULL \
                 RETURNING {SERIAL_COLUMNS}"
            ),
            vec![Value::from(target), uuid_text(operator), Value::from(&id)],
        )
        .await?;
    let row = updated
        .first()
        .map(SerialRow::from_row)
        .transpose()?
        .ok_or_else(|| DbError::InvalidValue("状态更新未返回行（不应发生）".to_string()))?;

    // 不变式②：在库件的出库/报废都要落一条 OUT 流水 + 重算账面数量。
    // `OUTBOUND → SCRAPPED` **不涉及库存**：件数在出库那一刻已经减过，再减一次就是重复扣减。
    let movement = match (current.status.as_str(), target) {
        (STATUS_IN_STOCK, STATUS_OUTBOUND) => Some((BIZ_TYPE_SERIAL_OUT, "出库")),
        (STATUS_IN_STOCK, STATUS_SCRAPPED) => Some((BIZ_TYPE_SERIAL_SCRAP, "报废")),
        _ => None,
    };
    if let Some((biz_type, label)) = movement {
        let remark = reason
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("序列号{label}：{}", row.serial_no));
        post_outbound_movement(ex, &row, biz_type, &remark, operator).await?;
    }

    Ok(TransitionOutcome::Updated {
        from: current.status,
        row: Box::new(row),
    })
}

/// 出库 / 报废的库存联动 + 成本结转（规格 v3 §5.3 的 ④–⑧ 步）。
///
/// ```text
/// ④ 仓级 before 快照（FOR UPDATE 锁住全部桶行）
/// ⑤ recompute_quantity（按桶版）：两本账一起减到「在库件数」
/// ⑥ 仓级 after 快照
/// ⑦ 一条 OUT 流水（unit_cost = 该桶 avg_cost，amount = before.amount − after.amount）
/// ⑧ 凭证：出库 借 6401 / 贷 1405；报废 借 1901 / 贷 1405（金额同上，≤0 跳过）
/// ```
///
/// **不许只减成本账不减库位账**：那样生成时绿的 B1 会在第一次出库就红
/// （实测 4 / 5 / 4，规格 §1.4(b)）。也**不许**用 `qty ± 1` 代替按桶重算 ——
/// 并发出库 + 软删号会让「账面数量」与「在库件数」脱钩。
///
/// `warehouse_id` 为空的序列号（历史行）跳过：`biz_inventory.warehouse_id` 是 NOT NULL，
/// 没有仓库维度的号记不进任何一本仓账，只留 WARN。
async fn post_outbound_movement(
    ex: &impl SqlExecutor,
    row: &SerialRow,
    biz_type: &str,
    remark: &str,
    operator: Option<Uuid>,
) -> DbResult<()> {
    let Some(warehouse_id) = row.warehouse_id else {
        tracing::warn!(
            serial_no = %row.serial_no,
            "序列号没有仓库归属，跳过库存联动与成本结转（biz_inventory.warehouse_id 是 NOT NULL）"
        );
        return Ok(());
    };
    let now = Utc::now();
    // ④ 仓级 before（顺带锁住全部桶行，锁顺序全局唯一）
    let before = lock_warehouse_snapshot(ex, row.material_id, warehouse_id).await?;
    // 该桶的移动加权单价（流水的 unit_cost；桶行不存在 = 0，历史脏数据）
    let unit_cost = bucket_avg_cost(ex, row.material_id, warehouse_id, row.location_id).await?;
    // ⑤ 两本账一起按桶对齐成在库件数
    recompute_quantity(ex, row.material_id, warehouse_id).await?;
    // ⑥ 仓级 after
    let after = warehouse_snapshot(ex, row.material_id, warehouse_id).await?;

    // 本次结转金额 = 账上真实差额（4 位）。**不在 Rust 里重算** `1 × avg_cost`：
    // 多桶时差额与「件数 × 单价」不相等（规格 v3 §8.3 的 N11）。
    let raw_amount = (before.amount - after.amount).round_dp(4);
    let amount = if raw_amount < Decimal::ZERO {
        // 账面自身不自洽（amount 与 qty × avg 脱钩的历史脏数据）时差额会为负；
        // `ck_biz_inv_txn_amt` 禁负数，这里归零并留 WARN，由库存巡检去修账面。
        tracing::warn!(
            serial_no = %row.serial_no,
            raw_amount = %raw_amount,
            "出库/报废的账面金额差额为负（历史脏账），流水金额按 0 记，请跑库存巡检修复"
        );
        Decimal::ZERO
    } else {
        raw_amount
    };

    // ⑦ 一条 OUT 流水（形态不变：OUT + SERIAL_OUT / SERIAL_SCRAP；数量恒正，方向看 txn_type）
    let note = TxnNote {
        txn_type: TXN_TYPE_OUT,
        biz_type,
        doc_type: DOC_TYPE_SERIAL,
        doc_id: row.id,
        doc_no: row.serial_no.clone(),
        remark: Some(remark.to_string()),
        operator,
        now,
        unit_cost,
        amount,
        before,
        after,
    };
    post_serial_txn(ex, row.material_id, warehouse_id, &note, 1).await?;

    // ⑧ 成本结转凭证（差额 ≤ 0 ⇒ 内部跳过并 WARN：历史零成本存量的过渡口径，规格 §8.2）
    let movement = if biz_type == BIZ_TYPE_SERIAL_OUT {
        SerialMovement::Outbound {
            material_id: row.material_id,
            warehouse_id,
            serial_no: row.serial_no.as_str(),
        }
    } else {
        SerialMovement::Scrapped {
            material_id: row.material_id,
            warehouse_id,
            serial_no: row.serial_no.as_str(),
        }
    };
    voucher::create_for_serial_movement(
        ex,
        movement,
        row.id,
        &row.serial_no,
        amount,
        operator,
        now,
    )
    .await?;
    Ok(())
}

// ==================== 移仓（不变式④，供 W3b 调拨单调用） ====================

/// 调拨 / 出库时，把序列号同步改到目标仓库 / 库位。
///
/// `serial_ids` 里**不存在的 id、已软删的 id 一律跳过**（不报错），
/// 返回值是**实际改动的行数** —— 调用方拿它做「预期 N 条 vs 实际 M 条」的对账
/// （少改就是数据不一致，W3b 可以据此决定告警还是回滚整单）。
///
/// # 只改四列
///
/// `warehouse_id` / `location_id` / `updated_at` / `update_by`。
/// **`status` 不动**（红线④）：移仓不是收发，件还是 `IN_STOCK`；
/// 因此本函数**也不写库存流水**（`biz_inventory_txn` 只记收发/报废）。
///
/// # 调用方必须自己重算账面（重要）
///
/// 移仓会让两本仓账同时过期：源仓少 M 件、目标仓多 M 件。
/// 本函数**故意不重算** —— 「移几件」只有调用方知道要落成几张单据
/// （一张调拨单可能拆成多行、多次调用）。正确收尾是在**同一事务**里对
/// 受影响的每个 (物料, 仓库) 调 [`recompute_quantity`]：
///
/// ```ignore
/// let tx = db().begin().await?;
/// let moved = serial_model::move_serials(&tx, &ids, to_wh, to_loc, operator).await?;
/// serial_model::recompute_quantity(&tx, material_id, from_wh).await?;  // 源仓
/// serial_model::recompute_quantity(&tx, material_id, to_wh).await?;    // 目标仓
/// tx.commit().await?;
/// ```
///
/// 忘了重算就是脏账 —— 由巡检
/// （[`crate::business::erp::inventory_audit::audit`]）兜住：
/// `qty != count(IN_STOCK)` 会被报出来，`dry_run=false` 也能修回来。
///
/// # 事务
///
/// 吃 `&impl SqlExecutor`（`Db` 与 `Transaction` 都实现）：由调用方开事务，
/// 本函数不 `begin`、不 `commit`，也绝不「事务里再开事务」。
pub async fn move_serials(
    ex: &impl SqlExecutor,
    serial_ids: &[Uuid],
    to_warehouse_id: Uuid,
    to_location_id: Option<Uuid>,
    operator: Uuid,
) -> DbResult<u64> {
    if serial_ids.is_empty() {
        return Ok(0);
    }
    let payload = id_payload(serial_ids);
    // `location_id` 置空时走「SQL 里直接写 NULL」的常量语句：`Value::Null` 绑 uuid 位
    // 会 `error serializing parameter`（skill §3 实测）。两条语句除该列外完全一致。
    let (sql, params) = match to_location_id {
        Some(location_id) => (
            "UPDATE biz_material_serial \
                SET warehouse_id = ?::uuid, location_id = ?::uuid, \
                    updated_at = now(), update_by = ?::uuid \
              WHERE id IN (SELECT unnest(string_to_array(?::text, E'\\n'))::uuid) \
                AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::from(&to_warehouse_id),
                Value::from(&location_id),
                Value::from(&operator),
                Value::from(payload.as_str()),
            ],
        ),
        None => (
            "UPDATE biz_material_serial \
                SET warehouse_id = ?::uuid, location_id = NULL, \
                    updated_at = now(), update_by = ?::uuid \
              WHERE id IN (SELECT unnest(string_to_array(?::text, E'\\n'))::uuid) \
                AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::from(&to_warehouse_id),
                Value::from(&operator),
                Value::from(payload.as_str()),
            ],
        ),
    };
    // SCOPE-EXEMPT: 序列号是仓储作业数据（V0138 无 dept_id / 归属人列），口径同 erp/model.rs。
    ex.exec_sql(sql, params).await
}

/// 序列号 id 的行分隔载荷（每行一个 uuid，`\\n` 分隔）。
///
/// 为什么不用 `?::uuid[]` / `?::text[]`：驱动把占位符推断成数组类型后只有 text 编码器，
/// 实测 `error serializing parameter N`（与 [`pair_payload`] 同一个坑）。
/// 一个 `text` 参数 + SQL 侧 `string_to_array` 拆是稳的形态；
/// uuid 数组例外（`?::text::uuid[]` 可行），这里统一走一条载荷，少一套口径。
fn id_payload(ids: &[Uuid]) -> String {
    let mut payload = String::with_capacity(ids.len() * 37);
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            payload.push('\n');
        }
        payload.push_str(&id.to_string());
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 号格式：`SN-yyyyMMdd-000001`，流水补零到 6 位。
    #[test]
    fn serial_no_matches_card_format() {
        assert_eq!(format_serial_no("20261030", 1), "SN-20261030-000001");
        assert_eq!(format_serial_no("20261030", 42), "SN-20261030-000042");
        assert_eq!(format_serial_no("20261030", 999_999), "SN-20261030-999999");
    }

    /// ② 超过 6 位不截断（宁可长一位，也不撞号）。
    #[test]
    fn serial_no_grows_beyond_six_digits() {
        assert_eq!(
            format_serial_no("20261030", 1_000_000),
            "SN-20261030-1000000"
        );
    }

    /// ③ 一整个号段连续：`start+1 ..= start+n`，且互不相同。
    #[test]
    fn number_range_is_contiguous_and_unique() {
        let start = 1_234_i64;
        let nos: Vec<String> = (1..=5)
            .map(|i| format_serial_no("20261101", start + i))
            .collect();
        assert_eq!(
            nos,
            vec![
                "SN-20261101-001235",
                "SN-20261101-001236",
                "SN-20261101-001237",
                "SN-20261101-001238",
                "SN-20261101-001239",
            ]
        );
        let unique: std::collections::BTreeSet<&String> = nos.iter().collect();
        assert_eq!(unique.len(), nos.len(), "号段内不得重号");
    }

    /// ④ 周期键用 CST：UTC 16:00 = 北京次日 00:00（跨天那一刻必须翻）。
    #[test]
    fn period_key_uses_china_time() {
        let utc = DateTime::parse_from_rfc3339("2026-10-30T15:59:59Z")
            .expect("合法时间")
            .with_timezone(&Utc);
        assert_eq!(period_key_of(utc), "20261030");
        let utc = DateTime::parse_from_rfc3339("2026-10-30T16:00:00Z")
            .expect("合法时间")
            .with_timezone(&Utc);
        assert_eq!(period_key_of(utc), "20261031", "北京时间已跨天");
    }

    /// ⑤ `today_period_key` 永远是 8 位数字（可以直接进 `period_key VARCHAR(8)`）。
    #[test]
    fn today_period_key_is_eight_digits() {
        let key = today_period_key();
        assert_eq!(key.len(), 8, "period_key 列宽 8：{key}");
        assert!(key.chars().all(|ch| ch.is_ascii_digit()), "{key}");
    }

    /// ⑥ 状态机：三条合法边放行。
    #[test]
    fn status_machine_allows_three_forward_edges() {
        assert!(can_transition("IN_STOCK", "OUTBOUND"));
        assert!(can_transition("IN_STOCK", "SCRAPPED"));
        assert!(can_transition("OUTBOUND", "SCRAPPED"));
    }

    /// ⑦ 状态机：回退 / 原地 / 报废后流转全部拒绝。
    #[test]
    fn status_machine_rejects_backward_and_idempotent() {
        assert!(!can_transition("OUTBOUND", "IN_STOCK"), "出库不能回库");
        assert!(!can_transition("SCRAPPED", "IN_STOCK"));
        assert!(!can_transition("SCRAPPED", "OUTBOUND"));
        assert!(!can_transition("IN_STOCK", "IN_STOCK"), "原地流转也算非法");
    }

    /// ⑧ 拒绝文案：回退与报废两条要给出「重新生成新号」的指引。
    #[test]
    fn transition_rejection_messages_are_actionable() {
        let back = transition_rejected_message("OUTBOUND", "IN_STOCK");
        assert!(back.contains("新的序列号"), "{back}");
        let scrap = transition_rejected_message("SCRAPPED", "OUTBOUND");
        assert!(scrap.contains("已报废"), "{scrap}");
        let other = transition_rejected_message("IN_STOCK", "IN_STOCK");
        assert!(other.contains("非法状态流转"), "{other}");
    }

    /// ⑨ 落库载荷：每行 `id|serial_no`，换行分隔（SQL 侧 `string_to_array` + `split_part` 拆）。
    #[test]
    fn pair_payload_is_line_delimited() {
        let a = Uuid::parse_str("018f4b2c-1111-7222-8333-444455556666").expect("uuid");
        let b = Uuid::parse_str("018f4b2c-1111-7222-8333-444455556667").expect("uuid");
        let payload = pair_payload(&[
            (a, "SN-20261030-000001".to_string()),
            (b, "SN-20261030-000002".to_string()),
        ]);
        let lines: Vec<&str> = payload.split('\n').collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], format!("{a}|SN-20261030-000001"));
        assert_eq!(lines[1], format!("{b}|SN-20261030-000002"));
        // 拆回来必须还原成 (uuid, 号) 且不含空行（空行会让 ::uuid 转换报错）
        assert!(lines.iter().all(|line| !line.is_empty()));
        for line in lines {
            let mut parts = line.split('|');
            assert!(Uuid::parse_str(parts.next().expect("uuid 段")).is_ok());
            assert!(parts.next().expect("号段").starts_with("SN-"));
        }
        assert_eq!(pair_payload(&[]), "");
    }
}
