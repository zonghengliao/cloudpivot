//! 销售订单数据层（销售链 · SO-04）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0264__sales_order.up.sql`
//! 的 `biz_sales_order` / `biz_sales_order_item`。
//!
//! | 列 | 口径 |
//! |----|------|
//! | `so_no` | `SO-yyyyMMdd-000001`；**部分唯一索引** `uk_biz_sales_order_no (so_no) WHERE deleted_at IS NULL` ⇒ 软删后单号可复用（PRD S2-09） |
//! | `status` | `VARCHAR(24)` 域值 7 个（PRD §4.13.3 逐字）：`DRAFT`/`SUBMITTED`/`APPROVED`/`PARTIAL_OUT`/`OUTBOUND`/`CLOSED`/`CANCELLED` |
//! | `total_with_tax` | 恒等 `= total_amount + total_tax`（`ck_sales_order_amount`，算错 = 23514 而非 500） |
//! | `delivered_qty` | 明细「已发货数量」，`DEFAULT 0`，`CHECK 0 <= delivered_qty <= qty`（部分发货累计 + 超发防线） |
//! | `customer_id` | **弱引用** `biz_customer.id`（无外键，与 `biz_purchase_order.supplier_id` 同风格；客户软删后历史单据仍可读） |
//!
//! # 本卡的范围（别顺手扩）
//!
//! SO-04 只交付**表 + 迁移 + 取号**。下面这些**故意不在这里**：
//!
//! - **状态迁移入口 / `LEGAL_EDGES`**：单据状态机的唯一真相在
//!   `business/shared/state_machine.rs`，登记一个新实体要先在
//!   `business/shared/doc_kind.rs` 的 `DocKind` 里加枚举值（该枚举被词表闸④逐字比对）。
//!   本卡不动词表，所以**不建** `SALES_ORDER_EDGES`——SO-05 落 API 时再登记
//!   `DocKind::SaleOrder` + 边表，登记后本文件的 [`STATUSES`] 必须与边表的域值逐字一致。
//! - **建单 / 改单 / 分页 / 详情**：SO-05 的地盘（本文件只到取号为止，不写半边 CRUD）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_sales_order / biz_sales_order_item 是 ERP 单据表（V0264 无 dept_id /
//! 归属人列），数据范围（SELF / DEPT_*）表达不出来；可见性由
//! business:erp:sale-order:* 权限码控制（SO-05 落）。
//! SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
//! ```
//!
//! # 取号为什么照抄采购单
//!
//! `PO-yyyyMMdd-000001` 这套（[`crate::business::erp::purchase_model::next_order_no`]）已经
//! 在库上跑了一年：`biz_seq` 单语句 `UPDATE … RETURNING` 原子取号，行锁天然防重号；
//! 周期键按**北京时间**（`serial_model::period_key_of`）而不是 UTC —— 用 UTC 会让
//! 「北京时间 00:00–08:00」这段算成前一天，跨天那一刻号段会重叠。
//! 销售订单只需换前缀与 `seq_type`，**不另造一套取号逻辑**。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::serial_model::period_key_of;

// ==================== 表名 / 常量 ====================

/// 单据头表。
pub const ORDER_TABLE: &str = "biz_sales_order";

/// 明细表（无 `deleted_at`：明细随单头走，外键 `ON DELETE CASCADE`）。
pub const ITEM_TABLE: &str = "biz_sales_order_item";

/// 单号前缀。
pub const NO_PREFIX: &str = "SO";

/// 流水号补零宽度（超过 6 位不截断，宁可长一位也不撞号）。
pub const NO_SEQ_WIDTH: usize = 6;

/// `biz_seq.seq_type`（V0264 种下的 `SO` 行）。
pub const SEQ_TYPE: &str = "SO";

/// 状态：草稿（**默认值**，也是唯一可删的状态，PRD S2-09）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：已提交（等待审批）。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";
/// 状态：审批通过（可拉出库单）。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 状态：部分出库（由出库过账回写，不是人工点出来的）。
pub const STATUS_PARTIAL_OUT: &str = "PARTIAL_OUT";
/// 状态：已全部出库。
pub const STATUS_OUTBOUND: &str = "OUTBOUND";
/// 状态：已关闭（剩余未发数量作废，终态）。
pub const STATUS_CLOSED: &str = "CLOSED";
/// 状态：已取消（终态）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 状态域白名单（与 `ck_sales_order_status` **逐字同序**，单测钉住）。
///
/// ⚠ 不是 `docs/ERP-批次2-技术架构.md` §2.2.1 的 `PARTIAL_SHIPPED` / `SHIPPED`：
/// 那份稿与 PRD（§2.2 S2-07/S2-08、§4.13.3 状态流转图、§4.13.4 算例）自相矛盾，
/// 以 PRD 为准（全文一致）。迁移 V0264 的自检会把 `SHIPPED` 当越界值断言被拒。
pub const STATUSES: [&str; 7] = [
    STATUS_DRAFT,
    STATUS_SUBMITTED,
    STATUS_APPROVED,
    STATUS_PARTIAL_OUT,
    STATUS_OUTBOUND,
    STATUS_CLOSED,
    STATUS_CANCELLED,
];

/// 状态是否在域内。
pub fn is_valid_status(value: &str) -> bool {
    STATUSES.contains(&value)
}

/// 状态中文标签（错误文案与前端徽标共用；未知值原样回显，不 panic）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "已提交",
        STATUS_APPROVED => "已审批",
        STATUS_PARTIAL_OUT => "部分出库",
        STATUS_OUTBOUND => "已出库",
        STATUS_CLOSED => "已关闭",
        STATUS_CANCELLED => "已取消",
        _ => "未知",
    }
}

// ==================== 单号（纯逻辑，无数据库） ====================

/// 组装单号（`SO-{周期键}-{6 位流水}`）。
pub fn format_so_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// 拆单号：`SO-yyyyMMdd-NNNNNN` → `(周期键, 流水)`。
///
/// 严格校验（不是 `split('-')` 一把梭）：
/// - 前缀必须是 [`NO_PREFIX`]、分隔符必须是 `-`；
/// - 周期键 8 位数字，且**年/月/日都合法**（`20261306` 这种要拒，否则「单号能生成」
///   就变成「什么字符串都能当单号」）；
/// - 流水 ≥ 1 的全数字（7 位以上也接受 —— 超过 999999 时号自然变长，不截断）。
///
/// 任何一条不符 → `None`（调用方按参数非法处理）。
pub fn parse_so_no(value: &str) -> Option<(String, i64)> {
    let rest = value.strip_prefix(NO_PREFIX)?.strip_prefix('-')?;
    let (period, seq) = rest.split_once('-')?;
    if period.len() != 8 || !period.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i32 = period.get(0..4)?.parse().ok()?;
    let month: u32 = period.get(4..6)?.parse().ok()?;
    let day: u32 = period.get(6..8)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1970 {
        return None;
    }
    if seq.is_empty() || !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let no: i64 = seq.parse().ok()?;
    if no < 1 {
        return None;
    }
    Some((period.to_string(), no))
}

/// 单号是否合法（[`parse_so_no`] 的布尔薄壳）。
pub fn is_valid_so_no(value: &str) -> bool {
    parse_so_no(value).is_some()
}

// ==================== 金额 / 数量守卫（纯逻辑） ====================

/// 单头金额三项是否**非负**（DDL 的 `ck_sales_order_amount_nonneg` 是兜底，入口先拒）。
pub fn amounts_non_negative(total_amount: Decimal, total_tax: Decimal, total_with_tax: Decimal) -> bool {
    total_amount >= Decimal::ZERO && total_tax >= Decimal::ZERO && total_with_tax >= Decimal::ZERO
}

/// 价税合计恒等：`total_with_tax == total_amount + total_tax`（**精确相等**，不容差）。
///
/// 与 `ck_sales_order_amount` 同口径：库里是精确 CHECK，入口也必须精确比对，
/// 否则「入口放过、库拒绝」就是 23514（对用户是 500 手感）。金额一律 4 位小数，
/// 舍入在**逐行**发生时做（与采购单/入库单同款：先舍入再相加，禁止先加总再舍入）。
pub fn totals_consistent(total_amount: Decimal, total_tax: Decimal, total_with_tax: Decimal) -> bool {
    total_with_tax == total_amount + total_tax
}

/// 已发货数量是否在 `[0, qty]` 内（`ck_sales_order_item_delivered` 的应用层同款）。
///
/// 出库过账累加 `delivered_qty` 前必须过这一关：超发在库层是 23514，
/// 在这里是 400 + 指名字段，用户能看懂。
pub fn delivered_qty_allowed(delivered_qty: Decimal, qty: Decimal) -> bool {
    delivered_qty >= Decimal::ZERO && delivered_qty <= qty
}

// ==================== 取号（`SO-yyyyMMdd-000001`） ====================

/// `biz_seq(seq_type='SO')` 缺失时幂等补一行（正常库有 V0264 的种子）。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'SO', '销售订单号', 'SO', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '销售订单单号：SO-yyyyMMdd-000001，按天（CST）滚动' \
         ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING"
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

/// 推进 `biz_seq(seq_type='SO')` 一个号（三分支：同日累加 / 新日重置 / 时钟回拨不重置）。
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
              WHERE seq_type = 'SO' AND deleted_at IS NULL \
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
    rows.first().map(|row| row.get_typed("current_no")).transpose()
}

/// 取一个销售订单号（周期键按北京时间）。
///
/// 调用方（SO-05 的建单）必须在**同一个事务**里调用本函数 + 写单头 + 写明细：
/// 取号把 `current_no` 推进了却没落单，那个号就永久占用（跳号可接受，重号不可接受）。
pub async fn next_so_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_so_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='SO') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;
    use std::str::FromStr;

    /// 定点字面量（`17.65` 这类在 f64 下不精确，一律走字符串）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    /// 状态域与 `ck_sales_order_status` 同域：7 个值，**不含**技术架构旧稿的 SHIPPED 系。
    #[test]
    fn status_domain_matches_check_constraint() {
        assert_eq!(
            STATUSES,
            [
                "DRAFT",
                "SUBMITTED",
                "APPROVED",
                "PARTIAL_OUT",
                "OUTBOUND",
                "CLOSED",
                "CANCELLED"
            ]
        );
        for good in STATUSES {
            assert!(is_valid_status(good), "`{good}` 应在域内");
            assert_ne!(status_label(good), "未知", "`{good}` 缺中文标签");
        }
        for bad in [
            "SHIPPED",
            "PARTIAL_SHIPPED",
            "partial_out",
            "Outbound",
            "1",
            "",
            "DRAFT ",
        ] {
            assert!(!is_valid_status(bad), "`{bad}` 不应通过状态域校验");
        }
        assert_eq!(status_label("WHATEVER"), "未知");
    }

    /// 单号格式：`SO-yyyyMMdd-6 位`；超 6 位不截断；拆解可逆。
    #[test]
    fn format_and_parse_so_no_roundtrip() {
        assert_eq!(format_so_no("20261106", 7), "SO-20261106-000007");
        assert_eq!(format_so_no("20261106", 123456), "SO-20261106-123456");
        // 流水超过 6 位：长一位，不截断（截断就撞号了）
        assert_eq!(format_so_no("20261106", 1234567), "SO-20261106-1234567");

        assert_eq!(parse_so_no("SO-20261106-000007"), Some(("20261106".to_string(), 7)));
        assert_eq!(
            parse_so_no("SO-20261106-1234567"),
            Some(("20261106".to_string(), 1234567))
        );
        // format → parse 恒等（流水从 1 到 1 万抽样）
        for no in [1_i64, 2, 999, 1000, 65_535, 999_999, 1_000_000] {
            let text = format_so_no("20261106", no);
            assert_eq!(parse_so_no(&text), Some(("20261106".to_string(), no)));
        }
    }

    /// 单号严格校验：前缀 / 分隔符 / 年月日合法性 / 流水，一条不符就拒。
    #[test]
    fn parse_so_no_rejects_malformed_values() {
        for bad in [
            "PO-20261106-000001",  // 前缀不对（采购单）
            "so-20261106-000001",  // 小写前缀
            "SO20261106-000001",   // 缺第一个分隔符
            "SO-2026116-000001",   // 周期键 7 位
            "SO-20261106_000001",  // 分隔符不对
            "SO-20261106-",        // 流水空
            "SO-20261106-abc",     // 流水非数字
            "SO-20261106-000001x", // 流水尾部有杂质
            "SO-20261106-000000",  // 流水 0 不是合法单号
            "SO-20261306-000001",  // 13 月
            "SO-20261132-000001",  // 32 日
            "SO-19691106-000001",  // 早于 1970（明显是脏数据）
            "",
            "SO",
        ] {
            assert_eq!(parse_so_no(bad), None, "`{bad}` 不应被当成合法单号");
            assert!(!is_valid_so_no(bad));
        }
        assert!(is_valid_so_no("SO-20261106-000001"));
        // 合法日期边界（闰年 2 月 29 日、12 月 31 日）
        assert!(is_valid_so_no("SO-20240229-000001"));
        assert!(is_valid_so_no("SO-20261231-999999"));
    }

    /// 金额恒等：精确相等（不是「差不多」）；非负另有一条守卫。
    #[test]
    fn totals_consistency_is_exact() {
        assert!(totals_consistent(Decimal::ZERO, Decimal::ZERO, Decimal::ZERO));
        assert!(totals_consistent(dec("100"), dec("13"), dec("113")));
        assert!(totals_consistent(dec("100.0000"), dec("13.0000"), dec("113.0000")));
        // 差 1 分也必须判否（库里 CHECK 就是精确相等）
        assert!(!totals_consistent(dec("100"), dec("13"), dec("113.0001")));
        assert!(!totals_consistent(dec("100"), dec("13"), dec("112.9999")));
        // 负数能「恒等」但不合法 → 由非负守卫拦
        assert!(totals_consistent(dec("-1"), dec("0"), dec("-1")));
        assert!(!amounts_non_negative(dec("-1"), Decimal::ZERO, dec("-1")));
        assert!(amounts_non_negative(Decimal::ZERO, Decimal::ZERO, Decimal::ZERO));
    }

    /// 已发货数量边界：`[0, qty]` 闭区间。
    #[test]
    fn delivered_qty_boundary() {
        assert!(delivered_qty_allowed(Decimal::ZERO, dec("10")));
        assert!(delivered_qty_allowed(dec("10"), dec("10"))); // 全部发完
        assert!(delivered_qty_allowed(dec("0.0001"), dec("10")));
        assert!(!delivered_qty_allowed(dec("10.0001"), dec("10"))); // 超发
        assert!(!delivered_qty_allowed(dec("-0.0001"), dec("10"))); // 负数
    }

    // ---------- 真库用例（`ERP_DB_URL` 开关） ----------
    //
    // ⚠ 不设 `ERP_DB_URL` 时这些用例打印 SKIP 并 return —— libtest 记 passed，
    //   属于「假绿」。交付/发布前必须显式带库跑一遍（见卡内汇报的实测命令）。

    /// 真库用例必须串行（共用全局 `db()` 与同一批 `SO4T-` 前缀夹具）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 本模块夹具的单号前缀 —— 清场只按它删，**不扫全表**。
    const NO_PREFIX_TEST: &str = "SO4T-";

    /// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
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
                // 每个 `#[tokio::test]` 是独立 runtime，`db()` 是进程级全局池：
                // max_lifetime/idle_timeout 归零，逼池子每次新建连接（避免借到死连接）。
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 拿锁 + 连库 + 清掉上一轮本模块的夹具（明细随单头 CASCADE 走）。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 清场：只删 `SO4T-*` 的单头（明细由外键级联删除）。
    async fn cleanup() {
        let sql = "DELETE FROM biz_sales_order WHERE so_no LIKE ?";
        if let Err(err) = db()
            .execute_with_params(sql, vec![Value::from(format!("{NO_PREFIX_TEST}%"))])
            .await
        {
            panic!("清理夹具失败：{err}\nSQL: {sql}");
        }
    }

    /// 插一条单头（金额三项显式给，便于测恒等约束）。
    ///
    /// ⚠ 金额参数必须写 `?::text::numeric`：把 `Value::Text` 直接绑到 numeric 列会被驱动拒
    /// （`cannot bind parameter to column type numeric`），与 `purchase_model::insert_items`
    /// 的 `NULLIF(?::text,'')::numeric` 同口径。
    async fn insert_order(so_no: &str, amount: &str, tax: &str, with_tax: &str) -> DbResult<u64> {
        db().execute_with_params(
            "INSERT INTO biz_sales_order (so_no, customer_id, order_date, \
                     total_amount, total_tax, total_with_tax) \
                 VALUES (?, gen_random_uuid(), CURRENT_DATE, \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric)",
            vec![
                Value::from(so_no.to_string()),
                Value::from(amount.to_string()),
                Value::from(tax.to_string()),
                Value::from(with_tax.to_string()),
            ],
        )
        .await
    }

    /// 取号：两次连续调用 → 同一天、流水 +1、格式可拆解（真库 `biz_seq` 原子 UPDATE）。
    #[tokio::test]
    async fn next_so_no_generates_consecutive_numbers_on_real_db() {
        let Some(_guard) = setup().await else { return };
        let now = Utc::now();
        let first = next_so_no(db(), now).await.expect("第一次取号应成功");
        let second = next_so_no(db(), now).await.expect("第二次取号应成功");

        let (period_a, no_a) = parse_so_no(&first).expect("取出的号必须能拆解");
        let (period_b, no_b) = parse_so_no(&second).expect("取出的号必须能拆解");
        assert_eq!(period_a, period_key_of(now), "周期键必须是北京时间的今天");
        assert_eq!(period_b, period_a, "同一天两次取号周期键应相同");
        assert_eq!(no_b, no_a + 1, "同一天流水必须 +1（{first} → {second}）");
        assert!(first.starts_with("SO-"), "前缀必须是 SO-（实际 {first}）");
        assert_eq!(first.len(), "SO-yyyyMMdd-000001".len(), "6 位补零：{first}");
        // 证据：把真实取出来的号打进日志（`--nocapture` 可见，卡内汇报引用它）
        println!("[SO-04] 取号实况：{first} → {second}（period_key={period_a}）");

        // 号在库里是「已用最大值」：biz_seq.current_no 必须 ≥ 刚取到的流水
        let rows = db()
            .query_sql(
                "SELECT current_no, period_key FROM biz_seq WHERE seq_type = 'SO' AND deleted_at IS NULL".to_string(),
                vec![],
            )
            .await
            .expect("读 biz_seq 应成功");
        let row = rows.first().expect("biz_seq(seq_type=SO) 必须存在");
        let current: i64 = row.get_typed("current_no").expect("current_no 可读");
        assert!(current >= no_b, "biz_seq.current_no({current}) 应 ≥ {no_b}");
        let key: String = row.get_typed("period_key").expect("period_key 可读");
        assert_eq!(key, period_a, "period_key 必须滚到当天");
    }

    /// 金额约束生效：`total_with_tax <> total_amount + total_tax` 必须被库拒绝，等号才放行。
    #[tokio::test]
    async fn amount_identity_constraint_enforced_by_db() {
        let Some(_guard) = setup().await else { return };
        let bad_no = format!("{NO_PREFIX_TEST}CK-0001");
        for (amount, tax, with_tax) in [
            ("100", "13", "114"),      // 差 1：价税合计算错
            ("100", "13", "113.0001"), // 差 0.0001：4 位小数下也不容差
            ("0", "0", "0.0001"),
        ] {
            let err = insert_order(&bad_no, amount, tax, with_tax)
                .await
                .expect_err("金额不自洽必须被 ck_sales_order_amount 拒绝");
            let text = format!("{err}");
            assert!(
                text.contains("ck_sales_order_amount") || text.contains("violat"),
                "错误应指向 CHECK 约束，实际：{text}"
            );
        }
        // 恒等的三条都能插
        insert_order(&bad_no, "100", "13", "113")
            .await
            .expect("自洽金额应可插入");
        insert_order(&format!("{NO_PREFIX_TEST}CK-0002"), "0", "0", "0")
            .await
            .expect("三项全 0 应可插入（草稿态）");
        // 负数金额被非负约束拒
        insert_order(&format!("{NO_PREFIX_TEST}CK-0003"), "-1", "0", "-1")
            .await
            .expect_err("负金额必须被 ck_sales_order_amount_nonneg 拒绝");
        cleanup().await;
    }

    /// 默认值 + 明细守卫 + 级联：status=DRAFT、delivered_qty=0、超发/零数量被拒、删单头带走明细。
    #[tokio::test]
    async fn defaults_delivered_guard_and_cascade_on_real_db() {
        let Some(_guard) = setup().await else { return };
        let so_no = format!("{NO_PREFIX_TEST}DF-0001");
        db().execute_with_params(
            "INSERT INTO biz_sales_order (so_no, customer_id, order_date) \
                 VALUES (?, gen_random_uuid(), CURRENT_DATE)",
            vec![Value::from(so_no.clone())],
        )
        .await
        .expect("最简插入应成功（status/金额三项都有 DEFAULT）");

        let rows = db()
            .query_sql(
                "SELECT id, status, total_amount::text AS a, total_with_tax::text AS w \
                   FROM biz_sales_order WHERE so_no = ?"
                    .to_string(),
                vec![Value::from(so_no.clone())],
            )
            .await
            .expect("回读单头应成功");
        let row = rows.first().expect("单头必须存在");
        let status: String = row.get_typed("status").expect("status 可读");
        assert_eq!(status, STATUS_DRAFT, "未传 status 应落默认 DRAFT");
        let amount: String = row.get_typed("a").expect("total_amount 可读");
        assert_eq!(amount, "0.0000", "金额默认 0（NUMERIC(18,4) 标度）");
        let order_id: Uuid = row.get_typed("id").expect("id 可读");

        // 明细：只给必填 → delivered_qty 默认 0
        db().execute_with_params(
            "INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty) \
                 VALUES (?, 1, gen_random_uuid(), 10)",
            vec![Value::from(order_id)],
        )
        .await
        .expect("最简明细应可插入");
        let items = db()
            .query_sql(
                "SELECT delivered_qty::text AS d, line_no FROM biz_sales_order_item WHERE so_id = ?".to_string(),
                vec![Value::from(order_id)],
            )
            .await
            .expect("回读明细应成功");
        let item = items.first().expect("明细必须存在");
        let delivered: String = item.get_typed("d").expect("delivered_qty 可读");
        assert_eq!(delivered, "0.0000", "delivered_qty 默认 0");
        let line_no: i32 = item.get_typed("line_no").expect("line_no 可读");
        assert_eq!(line_no, 1);

        // 超发：delivered_qty > qty 被拒
        db().execute_with_params(
            "INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty, delivered_qty) \
                 VALUES (?, 2, gen_random_uuid(), 10, 11)",
            vec![Value::from(order_id)],
        )
        .await
        .expect_err("超发必须被 ck_sales_order_item_delivered 拒绝");
        // 零数量被拒
        db().execute_with_params(
            "INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty) \
                 VALUES (?, 3, gen_random_uuid(), 0)",
            vec![Value::from(order_id)],
        )
        .await
        .expect_err("qty = 0 必须被 ck_sales_order_item_qty 拒绝");
        // 行号重复被拒（uk_biz_sales_order_item_line）
        db().execute_with_params(
            "INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty) \
                 VALUES (?, 1, gen_random_uuid(), 5)",
            vec![Value::from(order_id)],
        )
        .await
        .expect_err("同一单内行号必须唯一");

        // 级联：物理删单头 → 明细一起消失
        db().execute_with_params("DELETE FROM biz_sales_order WHERE id = ?", vec![Value::from(order_id)])
            .await
            .expect("删单头应成功");
        let left = db()
            .query_sql(
                "SELECT count(*) AS n FROM biz_sales_order_item WHERE so_id = ?".to_string(),
                vec![Value::from(order_id)],
            )
            .await
            .expect("数残留明细应成功");
        let n: i64 = left
            .first()
            .expect("count 必有一行")
            .get_typed("n")
            .expect("count 可读");
        assert_eq!(n, 0, "ON DELETE CASCADE 未生效：明细还剩 {n} 行");
        cleanup().await;
    }

    /// 单号唯一：存活行不许重号；**软删之后同一单号可复用**（部分唯一索引语义）。
    #[tokio::test]
    async fn so_no_unique_among_live_rows_only() {
        let Some(_guard) = setup().await else { return };
        let so_no = format!("{NO_PREFIX_TEST}UK-0001");
        insert_order(&so_no, "0", "0", "0").await.expect("首插应成功");
        insert_order(&so_no, "0", "0", "0")
            .await
            .expect_err("存活行重号必须被 uk_biz_sales_order_no 拒绝");

        db().execute_with_params(
            "UPDATE biz_sales_order SET deleted_at = now() WHERE so_no = ?",
            vec![Value::from(so_no.clone())],
        )
        .await
        .expect("软删应成功");
        insert_order(&so_no, "0", "0", "0")
            .await
            .expect("软删后单号应可复用（唯一索引带 WHERE deleted_at IS NULL）");
        cleanup().await;
    }

    /// 取号周期键用北京时间：UTC 17:00 = 北京次日 01:00（日号不能落后一天）。
    #[test]
    fn period_key_uses_beijing_day() {
        use chrono::TimeZone;
        let utc_before_midnight = Utc.with_ymd_and_hms(2026, 11, 6, 15, 59, 0).single().expect("合法时刻"); // 北京 23:59
        let utc_after = Utc.with_ymd_and_hms(2026, 11, 6, 16, 1, 0).single().expect("合法时刻"); // 北京次日 00:01
        assert_eq!(period_key_of(utc_before_midnight), "20261106");
        assert_eq!(period_key_of(utc_after), "20261107");
        let _ = NaiveDate::from_ymd_opt(2026, 11, 6);
    }
}
