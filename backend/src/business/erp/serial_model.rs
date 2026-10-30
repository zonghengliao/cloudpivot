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
//! # 为什么扫码一定走索引
//!
//! [`serial_scan`] 的谓词是 `s.serial_no = ?`，命中 V0138 的部分唯一索引
//! `uk_biz_material_serial_no (serial_no) WHERE deleted_at IS NULL`。
//! **`deleted_at IS NULL` 必须写在查询里**：部分索引的可命中性依赖它，
//! 少了这一句 planner 就不能用该索引（会退化成 Seq Scan + Filter）。

use nik::prelude::chrono::{DateTime, Duration, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::normalize_page;
use crate::business::erp::serial_dto::SerialFilter;
use crate::business::shared::scope;

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
async fn ensure_seq_row() -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列，不需要数据范围。
    db().execute_with_params(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'SN', '单品序列号', 'SN', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '一物一码单品序列号：SN-yyyyMMdd-000001，按天（CST）滚动' \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'SN' AND deleted_at IS NULL)",
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
async fn bump_seq(count: i64, period_key: &str) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: 同上，biz_seq 是全局取号规则字典。
    let rows = db()
        .find_with_params(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + ? \
                                      WHEN period_key < ? THEN ? \
                                      ELSE current_no + ? END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = 'SN' AND deleted_at IS NULL \
             RETURNING current_no - ? AS start",
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
pub async fn take_numbers(count: i64, period_key: &str) -> DbResult<i64> {
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
    if let Some(start) = bump_seq(count, period_key).await? {
        return Ok(start);
    }
    ensure_seq_row().await?;
    bump_seq(count, period_key).await?.ok_or_else(|| {
        DbError::InvalidValue(
            "biz_seq 里没有 seq_type='SN' 的取号规则（V0138 应已种入），无法生成序列号".to_string(),
        )
    })
}

// ==================== 批量生成 ====================

/// 批量生成序列号：取一段号 + 一条 `INSERT … SELECT` 落库 + 回读整行。
///
/// 为什么用「一条 INSERT + text 载荷」而不是 1000 条 `INSERT`：1000 个往返实测 1.5–2 s，
/// 单条语句 < 100 ms。载荷格式见 [`pair_payload`]（数组参数绑不上，见那里的说明）。
/// 号码先取后插，**插入失败会浪费掉这一小段号**（序列号允许跳号，
/// 业务上不可接受的是重号）——这条取舍写在证据文档里。
pub async fn batch_generate(
    params: &GenerateParams<'_>,
    period_key: &str,
    operator: Option<Uuid>,
) -> DbResult<Vec<SerialRow>> {
    let start = take_numbers(params.count, period_key).await?;
    let ids: Vec<Uuid> = (0..params.count).map(|_| Uuid::now_v7()).collect();
    let serial_nos: Vec<String> = (1..=params.count)
        .map(|offset| format_serial_no(period_key, start + offset))
        .collect();
    let pairs: Vec<(Uuid, String)> = ids.iter().copied().zip(serial_nos).collect();
    let payload = pair_payload(&pairs);

    db().execute_with_params(
        "INSERT INTO biz_material_serial \
             (id, serial_no, material_id, warehouse_id, location_id, batch_no, status, inbound_at, \
              source_type, source_id, create_by, update_by) \
         SELECT split_part(t.line, '|', 1)::uuid, split_part(t.line, '|', 2), ?::uuid, \
                NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid, NULLIF(?::text, ''), \
                'IN_STOCK', now(), NULLIF(?::text, ''), NULLIF(?::text, '')::uuid, \
                NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid \
           FROM unnest(string_to_array(?, E'\\n')) AS t(line)",
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

    let rows = db()
        .find_with_params(
            &format!(
                "SELECT {SERIAL_COLUMNS} FROM biz_material_serial \
                  WHERE id IN (SELECT split_part(t.line, '|', 1)::uuid \
                                 FROM unnest(string_to_array(?, E'\\n')) AS t(line)) \
                    AND deleted_at IS NULL ORDER BY serial_no ASC"
            ),
            vec![Value::Text(pair_payload(&pairs))],
        )
        .await?;
    rows.iter().map(SerialRow::from_row).collect()
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
/// - `updated_by` 用操作人，`deleted_at` 不参与（软删行一律当不存在）。
pub async fn transition_status(
    id: Uuid,
    target: &str,
    operator: Option<Uuid>,
) -> DbResult<TransitionOutcome> {
    if !ALL_STATUSES.contains(&target) {
        return Err(DbError::InvalidValue(format!(
            "目标状态只能是 IN_STOCK / OUTBOUND / SCRAPPED（收到 {target}）"
        )));
    }
    let tx = db().begin().await?;
    let locked = tx
        .find_with_params(
            &format!(
                "SELECT {SERIAL_COLUMNS} FROM biz_material_serial \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    let current = locked.first().map(SerialRow::from_row).transpose()?;
    let Some(current) = current else {
        tx.rollback().await?;
        return Ok(TransitionOutcome::NotFound);
    };
    if !can_transition(&current.status, target) {
        tx.rollback().await?;
        return Ok(TransitionOutcome::Illegal {
            from: current.status,
            to: target.to_string(),
        });
    }
    let updated = tx
        .find_with_params(
            &format!(
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
    tx.commit().await?;
    Ok(TransitionOutcome::Updated {
        from: current.status,
        row: Box::new(row),
    })
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
