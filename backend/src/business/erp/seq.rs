//! 主数据编码取号器（卡 **CODE-01**）：`biz_seq` 里的**纯流水**号段。
//!
//! # 与单据号的区别（别混用）
//!
//! | | 单据号（PO / IN / SO / CT …） | 主数据编码（本模块） |
//! |---|---|---|
//! | 日期段 | 有（`date_pattern='yyyyMMdd'`） | **无**（`date_pattern='none'`） |
//! | 期间重置 | 按天/月滚动（`period_key='20261108'`） | **永不重置**（`period_key='GLOBAL'`） |
//! | 位数 | 6 位 | SUP/CUS/MAT 4 位、WH/AC/ET 2 位（`biz_seq.pad_width`） |
//! | 语义 | 单据标识 | 档案编码，**永久唯一** |
//!
//! # 六类主数据（用户已拍板，不许改）
//!
//! | 主数据 | `seq_type` | 前缀 | 位数 | 示例 | 落库表.列 |
//! |---|---|---|---|---|---|
//! | 供应商 | `SUP` | `SUP` | 4 | `SUP-0001` | `biz_supplier.supplier_code` |
//! | 客户 | `CUS` | `CUS` | 4 | `CUS-0001` | `biz_customer.code` |
//! | 物料 | `MAT` | `MAT` | 4 | `MAT-0001` | `biz_material.material_code` |
//! | 仓库 | `WH` | `WH` | 2 | `WH-01` | `biz_warehouse.warehouse_code` |
//! | 资产分类 | `AC` | `AC` | 2 | `AC-01` | `biz_asset_category.category_code` |
//! | 费用类型 | `ET` | `ET` | 2 | `ET-01` | `biz_expense_type.type_code` |
//!
//! **计量单位不自动**（用户明确）：`PCS/KG/M` 是行业标准码，自动生成反而看不懂。
//!
//! # 三个入口
//!
//! 1. [`next_master_code`] —— 纯取号（**不查重**）。调用方自己保证编码没被占用时用。
//! 2. [`next_master_code_free`] —— 取号并**跳过已占用的号**（主数据推荐用这个）。
//!    例：老库已有 `SUP-0001`（V0137 种子）⇒ 第一次自动编码给 `SUP-0002`。
//! 3. [`next_master_code_free_with`] —— 同上，但占位检查 SQL 由调用方给（非标准表/特殊软删口径）。
//!
//! 每个入口都有 `_in` 变体区别只在**执行器**：接受 `&impl SqlExecutor` 的版本可以在
//! **调用方的事务里**取号（建单/建档案与取号同一事务，回滚时号一起回滚，不留空号）；
//! `next_master_code_global` / `next_master_code_free_global` 走全局 `db()`（自动提交）。
//!
//! # 并发安全
//!
//! 发号是单语句 `UPDATE biz_seq … RETURNING current_no`：PG 对该行加行锁，两个并发事务
//! 先后拿到 `n` 与 `n+1`，**天然不重号**（与 [`crate::business::erp::contract_model`] 同款）。
//! 注意持锁到事务提交：同一事务里取完号尽快落库，别在长事务里连续取多个号。
//!
//! # 手工覆盖
//!
//! 本模块只负责**生成**：`code` 有值时一律保留用户输入（企业已有编码体系要能对接），
//! 只有 `code` 为空/纯空白才取号。唯一约束冲突由调用方翻译成友好错误。
//!
//! # 迁移依赖
//!
//! 六条规则由 `V0337__master_data_seq.up.sql` 种入（幂等）；`pad_width` 列也在那条迁移里。

use nik::prelude::*;

/// 默认补零位数（`biz_seq.pad_width` 的列默认值，也是找不到规则时的兜底）。
pub const DEFAULT_PAD_WIDTH: i32 = 4;

/// `pad_width` 的合法区间（与 `ck_biz_seq_pad_width` 完全一致）。
pub const MIN_PAD_WIDTH: i32 = 1;
/// `pad_width` 的合法区间上界（与 `ck_biz_seq_pad_width` 完全一致）。
pub const MAX_PAD_WIDTH: i32 = 12;

/// 「跳过已占用号」的最大尝试次数：20 个连号都被占 ⇒ 报错让人看数据，
/// 不做无限重试（无限重试会把 `current_no` 推到没有意义的远处）。
pub const FREE_CODE_MAX_ATTEMPTS: u32 = 20;

/// 六类主数据号段（顺序与用户拍板的表格一致）。
pub const MASTER_CODE_TYPES: [&str; 6] = ["SUP", "CUS", "MAT", "WH", "AC", "ET"];

/// 是否是主数据号段。
pub fn is_master_code_type(seq_type: &str) -> bool {
    MASTER_CODE_TYPES.contains(&seq_type)
}

/// 主数据编码 → 「该编码是否已被占用」的常量 SQL（只有一个 `?` 占位，绑定编码本身）。
///
/// 返回 `None` 表示 `seq_type` 不属于六类主数据。
///
/// 只收录**应用内已存在**的主数据表；表名/列名在这里是常量，不接受外部拼接
/// （新增主数据表时在这里加一行，并补 [`is_master_code_type`]）。
pub fn master_code_exists_sql(seq_type: &str) -> Option<&'static str> {
    match seq_type {
        // SCOPE-EXEMPT: 主数据主档表没有归属列（全局档案）。
        "SUP" => Some("SELECT 1 FROM biz_supplier WHERE supplier_code = ? AND deleted_at IS NULL"),
        // SCOPE-EXEMPT: 同上。
        "CUS" => Some("SELECT 1 FROM biz_customer WHERE code = ? AND deleted_at IS NULL"),
        // SCOPE-EXEMPT: 同上。
        "MAT" => Some("SELECT 1 FROM biz_material WHERE material_code = ? AND deleted_at IS NULL"),
        // SCOPE-EXEMPT: 同上。
        "WH" => Some("SELECT 1 FROM biz_warehouse WHERE warehouse_code = ? AND deleted_at IS NULL"),
        // SCOPE-EXEMPT: 同上。
        "AC" => {
            Some("SELECT 1 FROM biz_asset_category WHERE category_code = ? AND deleted_at IS NULL")
        }
        // SCOPE-EXEMPT: 同上。
        "ET" => Some("SELECT 1 FROM biz_expense_type WHERE type_code = ? AND deleted_at IS NULL"),
        _ => None,
    }
}

/// 纯函数：按「前缀 + 分隔符 + 流水」拼编码（位数不足左补 0，超过位数按实际输出）。
///
/// ```ignore
/// assert_eq!(format_master_code("SUP", "-", 4, 1), "SUP-0001");
/// assert_eq!(format_master_code("WH", "-", 2, 1), "WH-01");
/// assert_eq!(format_master_code("WH", "-", 2, 123), "WH-123"); // 不截断
/// ```
pub fn format_master_code(prefix: &str, separator: &str, pad_width: i32, no: i64) -> String {
    let width = pad_width.clamp(MIN_PAD_WIDTH, MAX_PAD_WIDTH) as usize;
    let body = format!("{:0width$}", no.max(0), width = width);
    if separator.is_empty() {
        format!("{prefix}{body}")
    } else {
        format!("{prefix}{separator}{body}")
    }
}

/// 一条取号规则（`biz_seq` 里那一行里取号需要的三列）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SeqRule {
    prefix: String,
    separator: String,
    pad_width: i32,
}

/// 读规则（**必须**是启用中的存活行；停用 = 不参与取号）。
async fn load_rule(ex: &impl SqlExecutor, seq_type: &str) -> DbResult<SeqRule> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT prefix, separator, pad_width::int AS pad_width \
               FROM biz_seq \
              WHERE seq_type = ? AND deleted_at IS NULL AND status = 1"
                .to_string(),
            vec![Value::from(seq_type)],
        )
        .await?;
    let Some(row) = row else {
        return Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq 里没有可用（未删除且启用中）的取号规则（seq_type='{seq_type}'）。\
             主数据号段由 V0337__master_data_seq 种入；单据号段见各自的 V01xx 迁移。"
        )));
    };
    Ok(SeqRule {
        prefix: row.get_typed::<String>("prefix")?,
        separator: row.get_typed::<String>("separator")?,
        pad_width: row.get_typed::<i32>("pad_width")?,
    })
}

/// 原子推进一个号并返回新流水（单语句 `UPDATE … RETURNING`，行锁保证并发不重号）。
///
/// 纯流水号段**不看 `period_key`**：`GLOBAL` 只是「永不清零」的标记，
/// 推进逻辑与期间无关（对照单据号段的 [`crate::business::erp::serial_model`] 三分支）。
async fn bump_master_no(ex: &impl SqlExecutor, seq_type: &str) -> DbResult<i64> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    let row = ex
        .query_first_sql(
            "UPDATE biz_seq \
                SET current_no = current_no + step, \
                    updated_at = now() \
              WHERE seq_type = ? AND deleted_at IS NULL AND status = 1 \
             RETURNING current_no"
                .to_string(),
            vec![Value::from(seq_type)],
        )
        .await?;
    match row {
        Some(row) => Ok(row.get_typed::<i64>("current_no")?),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='{seq_type}') 行不存在或已停用（status <> 1）"
        ))),
    }
}

/// 取一个主数据编码（**不查重**）。在调用方的事务里执行时，事务回滚则号一起回滚。
///
/// `SUP-0001` / `WH-01`：前缀、分隔符、位数都来自 `biz_seq` 那一行。
pub async fn next_master_code(ex: &impl SqlExecutor, seq_type: &str) -> DbResult<String> {
    let rule = load_rule(ex, seq_type).await?;
    let no = bump_master_no(ex, seq_type).await?;
    Ok(format_master_code(
        &rule.prefix,
        &rule.separator,
        rule.pad_width,
        no,
    ))
}

/// 同 [`next_master_code`]，但走全局 `db()`（自动提交；无法参与调用方事务）。
pub async fn next_master_code_global(seq_type: &str) -> DbResult<String> {
    next_master_code(db(), seq_type).await
}

/// 编码是否已被占用（`exists_sql` 是 [`master_code_exists_sql`] 这类常量 SQL，只绑一个参数）。
async fn code_taken(ex: &impl SqlExecutor, exists_sql: &str, code: &str) -> DbResult<bool> {
    Ok(ex
        .query_first_sql(exists_sql.to_string(), vec![Value::from(code)])
        .await?
        .is_some())
}

/// 取一个**未被占用**的主数据编码：撞了就继续取下一个号，最多 `max_attempts` 次。
///
/// 这是卡 CODE-01 ③「序列号不重复的保护」的落点：
/// 主数据表的编码列是「未删除内唯一」，而老库里的编码可能早于取号器存在
/// （例：V0137 种了 `SUP-0001`）。撞号时**不报错**，直接顺延到下一个号。
///
/// `exists_sql` 必须是一条**常量** SQL（一个 `?` 占位，绑定候选编码），
/// 不允许把用户输入拼进去。
pub async fn next_master_code_free_with(
    ex: &impl SqlExecutor,
    seq_type: &str,
    exists_sql: &str,
    max_attempts: u32,
) -> DbResult<String> {
    let attempts = max_attempts.max(1);
    let rule = load_rule(ex, seq_type).await?;
    let mut last: Option<String> = None;
    for _ in 0..attempts {
        let no = bump_master_no(ex, seq_type).await?;
        let code = format_master_code(&rule.prefix, &rule.separator, rule.pad_width, no);
        if !code_taken(ex, exists_sql, &code).await? {
            return Ok(code);
        }
        last = Some(code);
    }
    Err(DbError::InvalidValue(format!(
        "取号失败：seq_type='{seq_type}' 连续 {attempts} 个号都已被占用（最后一次尝试 = {}）。\
         请核对主数据表里的历史编码，或把 biz_seq.current_no 抬到已有最大流水之上。",
        last.as_deref().unwrap_or("<无>")
    )))
}

/// 同 [`next_master_code_free_with`]，用内置的六类主数据占位表（推荐入口）。
///
/// `seq_type` 不是 SUP/CUS/MAT/WH/AC/ET 时返回 `InvalidValue`（不猜表名）。
pub async fn next_master_code_free(ex: &impl SqlExecutor, seq_type: &str) -> DbResult<String> {
    let Some(exists_sql) = master_code_exists_sql(seq_type) else {
        return Err(DbError::InvalidValue(format!(
            "取号失败：seq_type='{seq_type}' 不是主数据号段（支持 SUP/CUS/MAT/WH/AC/ET）"
        )));
    };
    next_master_code_free_with(ex, seq_type, exists_sql, FREE_CODE_MAX_ATTEMPTS).await
}

/// 同 [`next_master_code_free`]，但走全局 `db()`。
pub async fn next_master_code_free_global(seq_type: &str) -> DbResult<String> {
    next_master_code_free(db(), seq_type).await
}

#[cfg(test)]
mod tests {
    //! 真库用例：`ERP_DB_URL` 指向已跑完迁移的库（V0337 已应用）。
    //!
    //! 每个用例用**专用测试号段**（`T4CON` / `T2DGT` / `T7SKP` / `TCCON`），
    //! 首尾按 `seq_type` 精确删除 —— 不碰生产号段（PO/SUP/WH…），所以用例之间
    //! 以及和别的模块并行跑都不会互删夹具。

    use super::*;

    /// 连库（只从 `ERP_DB_URL` 取；缺省打印 SKIP 并返回 false）。
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
                // 每个 `#[tokio::test]` 是独立 runtime，`db()` 是进程级池：
                // max_lifetime = 0 逼池子每次新建连接，避免借到上一个 runtime 的死连接。
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

    /// 造一条专用测试号段（先删同名的，保证幂等）。
    async fn reset_test_seq(seq_type: &str, prefix: &str, pad_width: i32, start_no: i64) {
        assert!(
            !is_master_code_type(seq_type),
            "测试号段不得占用六类主数据号段（{seq_type}）"
        );
        db().execute_with_params(
            "DELETE FROM biz_seq WHERE seq_type = ?",
            vec![Value::from(seq_type)],
        )
        .await
        .expect("清测试号段");
        db().execute_with_params(
            "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                                  current_no, step, separator, status, pad_width) \
             VALUES (gen_random_uuid(), ?, '测试号段（CODE-01）', ?, 'none', 'GLOBAL', ?, 1, '-', 1, ?)",
            vec![
                Value::from(seq_type),
                Value::from(prefix),
                Value::from(start_no),
                Value::I16(pad_width as i16),
            ],
        )
        .await
        .expect("造测试号段");
    }

    /// 删掉专用测试号段。
    async fn drop_test_seq(seq_type: &str) {
        db().execute_with_params(
            "DELETE FROM biz_seq WHERE seq_type = ?",
            vec![Value::from(seq_type)],
        )
        .await
        .expect("清测试号段");
    }

    async fn current_no_of(seq_type: &str) -> i64 {
        db().query_first_sql(
            "SELECT current_no FROM biz_seq WHERE seq_type = ? AND deleted_at IS NULL".to_string(),
            vec![Value::from(seq_type)],
        )
        .await
        .expect("查 current_no")
        .expect("号段行存在")
        .get_typed::<i64>("current_no")
        .expect("current_no")
    }

    // ---------- ① 纯函数：位数 / 分隔符 / 不截断 ----------

    #[test]
    fn t01_format_master_code_pads_by_width() {
        assert_eq!(format_master_code("SUP", "-", 4, 1), "SUP-0001");
        assert_eq!(format_master_code("MAT", "-", 4, 42), "MAT-0042");
        assert_eq!(format_master_code("WH", "-", 2, 1), "WH-01");
        assert_eq!(format_master_code("ET", "-", 2, 9), "ET-09");
        // 超过位数不截断（否则 100 号会变成 HH-00 撞回 1 号）
        assert_eq!(format_master_code("WH", "-", 2, 100), "WH-100");
        // 分隔符为空 ⇒ 直接相连（企业自定义规则时可能有这种号段）
        assert_eq!(format_master_code("PCS", "", 4, 7), "PCS0007");
        // 非法位数按合法区间收敛，不 panic
        assert_eq!(format_master_code("X", "-", 0, 1), "X-1");
        assert_eq!(format_master_code("X", "-", 99, 1), "X-000000000001");
    }

    #[test]
    fn t02_exists_sql_covers_exactly_six_types() {
        for seq_type in MASTER_CODE_TYPES {
            assert!(
                master_code_exists_sql(seq_type).is_some(),
                "{seq_type} 应有占位检查 SQL"
            );
            assert!(is_master_code_type(seq_type));
        }
        assert!(
            master_code_exists_sql("PO").is_none(),
            "单据号段不是主数据号段"
        );
        assert!(master_code_exists_sql("SUP2").is_none());
        assert!(!is_master_code_type("SN"));
    }

    // ---------- ② 迁移落地：六条规则就是拍板的那六条 ----------

    #[tokio::test]
    async fn t03_seeded_rules_match_spec() {
        if !ensure_db().await {
            return;
        }
        let rows = db()
            .query_sql(
                "SELECT seq_type, prefix, date_pattern, period_key, pad_width::int AS pad_width, \
                        separator, status \
                   FROM biz_seq \
                  WHERE seq_type IN ('SUP','CUS','MAT','WH','AC','ET') AND deleted_at IS NULL"
                    .to_string(),
                vec![],
            )
            .await
            .expect("查六条主数据规则");
        assert_eq!(rows.len(), 6, "六类主数据各一条，实际 {} 条", rows.len());

        let expected: [(&str, i32); 6] = [
            ("SUP", 4),
            ("CUS", 4),
            ("MAT", 4),
            ("WH", 2),
            ("AC", 2),
            ("ET", 2),
        ];
        for (seq_type, pad) in expected {
            let row = rows
                .iter()
                .find(|r| r.get_typed::<String>("seq_type").expect("seq_type") == seq_type)
                .unwrap_or_else(|| panic!("缺 seq_type={seq_type} 的取号规则（V0337 未应用？）"));
            assert_eq!(row.get_typed::<String>("prefix").expect("prefix"), seq_type);
            // 真实列是 NOT NULL + 白名单，'none' 即「不带日期」（卡面写的 NULL 落不了库）
            assert_eq!(
                row.get_typed::<String>("date_pattern")
                    .expect("date_pattern"),
                "none"
            );
            assert_eq!(
                row.get_typed::<String>("period_key").expect("period_key"),
                "GLOBAL",
                "{seq_type} 必须不按期间重置"
            );
            assert_eq!(
                row.get_typed::<i32>("pad_width").expect("pad_width"),
                pad,
                "{seq_type} 位数"
            );
            assert_eq!(
                row.get_typed::<String>("separator").expect("separator"),
                "-"
            );
            assert_eq!(row.get_typed::<i16>("status").expect("status"), 1);
        }
    }

    // ---------- ③ 连续取号 ----------

    #[tokio::test]
    async fn t04_consecutive_takes_increment() {
        if !ensure_db().await {
            return;
        }
        reset_test_seq("T4CON", "T4C", 4, 0).await;
        let c1 = next_master_code(db(), "T4CON").await.expect("第一次取号");
        let c2 = next_master_code(db(), "T4CON").await.expect("第二次取号");
        let c3 = next_master_code(db(), "T4CON").await.expect("第三次取号");
        assert_eq!(
            (c1.as_str(), c2.as_str(), c3.as_str()),
            ("T4C-0001", "T4C-0002", "T4C-0003")
        );
        assert_eq!(current_no_of("T4CON").await, 3, "current_no 应正好前进 3");
        drop_test_seq("T4CON").await;
    }

    // ---------- ④ 2 位号段（WH/AC/ET 的形态）+ 进位不截断 ----------

    #[tokio::test]
    async fn t05_two_digit_pad_and_overflow() {
        if !ensure_db().await {
            return;
        }
        reset_test_seq("T2DGT", "T2D", 2, 0).await;
        assert_eq!(
            next_master_code(db(), "T2DGT").await.expect("取号"),
            "T2D-01"
        );
        assert_eq!(
            next_master_code(db(), "T2DGT").await.expect("取号"),
            "T2D-02"
        );
        // 手工把流水推到 99（模拟老库），下一次必须是 100（3 位），不回到 00
        db().execute_with_params(
            "UPDATE biz_seq SET current_no = 99 WHERE seq_type = ?",
            vec![Value::from("T2DGT")],
        )
        .await
        .expect("推流水");
        assert_eq!(
            next_master_code(db(), "T2DGT").await.expect("取号"),
            "T2D-100"
        );
        drop_test_seq("T2DGT").await;
    }

    // ---------- ⑤ 并发 10 次取号：不重号、连续、current_no 正好推进 10 ----------

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn t06_concurrent_ten_takes_never_duplicate() {
        if !ensure_db().await {
            return;
        }
        reset_test_seq("TCCON", "TCC", 4, 0).await;

        // 每个任务自己开事务、取号、提交 —— 与生产建档案路径同形（行锁持有到提交）
        let mut handles = Vec::new();
        for _ in 0..10 {
            handles.push(tokio::spawn(async move {
                let tx = db().begin().await.expect("begin");
                let code = next_master_code(&tx, "TCCON").await.expect("取号");
                tx.commit().await.expect("commit");
                code
            }));
        }
        let mut codes = Vec::new();
        for handle in handles {
            codes.push(handle.await.expect("并发任务未 panic"));
        }

        let mut sorted = codes.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            10,
            "10 次取号必须得到 10 个不同号，实际 {codes:?}"
        );
        let expected: Vec<String> = (1..=10).map(|n| format!("TCC-{n:04}")).collect();
        assert_eq!(sorted, expected, "号码必须连续 0001..0010");
        assert_eq!(current_no_of("TCCON").await, 10, "current_no 应正好推进 10");
        drop_test_seq("TCCON").await;
    }

    // ---------- ⑥ 撞号保护：已存在的编码自动跳过 ----------

    #[tokio::test]
    async fn t07_free_code_skips_taken_codes() {
        if !ensure_db().await {
            return;
        }
        const SEQ: &str = "T7SKP";
        let code_taken_by_seed = "T7S-0001";
        reset_test_seq(SEQ, "T7S", 4, 0).await;
        // 主数据表里先占掉 0001（模拟「老数据早于取号器存在」，就是 V0137 的 SUP-0001 那种形态）
        db().execute_with_params(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, credit_days, status) \
             VALUES (gen_random_uuid(), ?, 'CODE-01 撞号夹具', 30, 1)",
            vec![Value::from(code_taken_by_seed)],
        )
        .await
        .expect("造已占用编码");

        let exists_sql = master_code_exists_sql("SUP").expect("SUP 占位 SQL");
        let got = next_master_code_free_with(db(), SEQ, exists_sql, 5)
            .await
            .expect("取号（应跳过 0001）");
        assert_eq!(got, "T7S-0002", "0001 已被占用，必须顺延到 0002");

        // 正常路径（不撞号）与 free_with 一致
        assert_eq!(
            next_master_code_free_with(db(), SEQ, exists_sql, 5)
                .await
                .expect("取号"),
            "T7S-0003"
        );

        // 清夹具：只删自己那行
        db().execute_with_params(
            "DELETE FROM biz_supplier WHERE supplier_code = ?",
            vec![Value::from(code_taken_by_seed)],
        )
        .await
        .expect("清供应商夹具");
        drop_test_seq(SEQ).await;
    }

    // ---------- ⑦ 资源与参数错误：说清楚，不静默 ----------

    #[tokio::test]
    async fn t08_unknown_and_missing_rule_errors() {
        if !ensure_db().await {
            return;
        }
        // 不是主数据号段：直接报错，不猜表
        let err = next_master_code_free(db(), "NO_SUCH").await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("NO_SUCH"), "错误里必须带上 seq_type：{msg}");
        assert!(
            msg.contains("SUP/CUS/MAT/WH/AC/ET"),
            "错误里必须列出支持范围：{msg}"
        );

        // 规则行不存在：报错指向 V0337
        let err = next_master_code(db(), "NO_RULE_TYPE").await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("NO_RULE_TYPE"),
            "错误里必须带上 seq_type：{msg}"
        );
        assert!(
            msg.contains("V0337"),
            "错误里必须指出应由哪条迁移种入：{msg}"
        );

        // 停用的规则不参与取号（status = 0）
        reset_test_seq("TOFF", "TOF", 4, 0).await;
        db().execute_with_params(
            "UPDATE biz_seq SET status = 0 WHERE seq_type = ?",
            vec![Value::from("TOFF")],
        )
        .await
        .expect("停用号段");
        let err = next_master_code(db(), "TOFF").await.unwrap_err();
        assert!(
            format!("{err}").contains("没有可用"),
            "停用号段的错误应说明「没有可用规则」：{err}"
        );
        drop_test_seq("TOFF").await;
    }
}
