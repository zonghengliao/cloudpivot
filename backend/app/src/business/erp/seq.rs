//! ERP 侧的**主数据编码取号入口**（卡 **CODE-01**）：`biz_seq` 里的**纯流水**号段。
//!
//! # 本文件现在的分工（P2-B 解环 · 卡 `t_3442b4fb`）
//!
//! 取号**引擎**（读规则 / `UPDATE … RETURNING` 推进 / 撞号顺延 / 预览 / 手工编码解析）
//! 已下移到跨域基座 `business/shared/seq.rs` —— 它是纯 `biz_seq` 读写，与任何业务表无关，
//! 而 HR 的工号（`EMP`）也要用它。原先引擎长在 ERP 里，HR 为了取一个工号就得
//! `use crate::business::erp::seq::…`，这就是 `hr → erp` 那条代码回边。
//!
//! ```text
//! 改前：hr ──取号──→ erp::seq   ┐
//! 改后：hr ──取号──→ shared::seq ┘  ERP 侧 `erp::seq` 变成「引擎再导出 + 本域查重表」
//! ```
//!
//! 本文件**只保留 ERP 自己的东西**（其余符号都是 `pub use` 再导出，40 余处调用点不用改）：
//!
//! | 留在本文件 | 为什么 |
//! |---|---|
//! | [`master_code_exists_sql`] | 六张 ERP 主档的**表名 / 列名 / 软删口径**归本域 |
//! | [`master_code_taken`] / [`next_master_code_free`] / [`peek_master_code`] 及其 `_global` 变体 | 用上面那张表的便捷入口（自动选表） |
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
//! # 七个主数据号段（前六类由用户拍板不许改；`EMP` 由 HR-14 按 ARCH-9-2 D-15 加入）
//!
//! | 主数据 | `seq_type` | 前缀 | 位数 | 示例 | 落库表.列 | 查重表在哪 |
//! |---|---|---|---|---|---|---|
//! | 供应商 | `SUP` | `SUP` | 4 | `SUP-0001` | `biz_supplier.supplier_code` | [`master_code_exists_sql`] |
//! | 客户 | `CUS` | `CUS` | 4 | `CUS-0001` | `biz_customer.code` | 同上 |
//! | 物料 | `MAT` | `MAT` | 4 | `MAT-0001` | `biz_material.material_code` | 同上 |
//! | 仓库 | `WH` | `WH` | 2 | `WH-01` | `biz_warehouse.warehouse_code` | 同上 |
//! | 资产分类 | `AC` | `AC` | 2 | `AC-01` | `biz_asset_category.category_code` | 同上 |
//! | 费用类型 | `ET` | `ET` | 2 | `ET-01` | `biz_expense_type.type_code` | 同上 |
//! | 员工工号 | `EMP` | `EMP` | 4 | `EMP-0001` | HR 员工主档（**表名不归本域**） | **HR 自带**（`business/hr/employee_no.rs` 的 `EMP_EXISTS_SQL`） |
//!
//! `EMP` 是**人事档案码**（ARCH-9-2 D-15）：`date_pattern='none'`、`period_key='GLOBAL'`
//! ⇒ 流水**永久不重置**（按日/按月重置会让工号每天从 0001 重发，必然撞员工主档的
//! 工号唯一键）。号段行由 `V0386__hr_seq_dict` 种入；取号入口在 HR 侧
//! （P2-36 起员工主档的查重口径也由 HR 自带，本模块只发号、**不认 HR 的表**）。
//!
//! **计量单位不自动**（用户明确）：`PCS/KG/M` 是行业标准码，自动生成反而看不懂。
//!
//! # 四个入口（引擎在 `business/shared/seq.rs`，这里只做「自动选表」那一层）
//!
//! 1. [`next_master_code`] —— 纯取号（**不查重**）。调用方自己保证编码没被占用时用。
//! 2. [`next_master_code_free`] —— 取号并**跳过已占用的号**（主数据推荐用这个）。
//!    例：老库已有 `SUP-0001`（V0137 种子）⇒ 第一次自动编码给 `SUP-0002`。
//! 3. `next_master_code_free_with` —— 同上，但占位检查 SQL 由调用方给（非标准表 / 特殊软删口径；
//!    HR 的 `EMP` 走这条）。实现在基座，本文件再导出。
//! 4. [`peek_master_code`] —— **只看不动**：预览下一个可用编码，不推进 `current_no`
//!    （前端「生成编码」按钮用；不是占号）。
//!
//! 每个入口都接受 `&impl SqlExecutor`，所以可以在**调用方的事务里**取号（建单 / 建档案与
//! 取号同一事务，回滚时号一起回滚，不留空号）；`*_global` 变体走全局 `db()`（自动提交）。
//!
//! # 并发安全
//!
//! 发号是单语句 `UPDATE biz_seq … RETURNING current_no`：PG 对该行加行锁，两个并发事务
//! 先后拿到 `n` 与 `n+1`，**天然不重号**（实现见基座 `business/shared/seq.rs`）。
//! 注意持锁到事务提交：同一事务里取完号尽快落库，别在长事务里连续取多个号。
//!
//! # 手工覆盖
//!
//! 本模块只负责**生成**：`code` 有值时一律保留用户输入（企业已有编码体系要能对接），
//! 只有 `code` 为空/纯空白才取号。唯一约束冲突由调用方翻译成友好错误。
//!
//! # 大小写：主数据统一「原样保留」（E2E08-FIX-2 ①）
//!
//! [`resolve_manual_code`] 只做 `trim`，**不转大写**。主数据号段（`SUP` / `CUS` / `MAT` /
//! `WH` / `AC` / `ET` / `EMP`）因此是同一套行为：用户填 `e2e08-sup-lc` 就存 `e2e08-sup-lc`。
//!
//! 历史上 `AC` / `ET` 多走了一步 `to_ascii_uppercase`，日志里出现「同一张编码规则表两种行为」，
//! 对接方按一类经验写另一类会踩坑 —— 已抹平。理由：有些企业自有编码区分大小写，
//! 替用户改大小写等于改他的编码；自动取号产出的 `AC-01` / `ET-01` 本来是大写，不受影响。
//!
//! # 迁移依赖
//!
//! 六条规则由 `V0337__master_data_seq.up.sql` 种入（幂等）；`pad_width` 列也在那条迁移里。

use nik::prelude::*;

// ==================== 引擎（真源在 `business/shared/seq.rs`） ====================
//
// P2-B 解环（卡 `t_3442b4fb`）：取号引擎是**纯 `biz_seq` 读写**，与业务表无关，
// 已下移到跨域基座。这里再导出，ERP 侧 40 余处 `seq::xxx` 调用点一行都不用改。
// 本文件**只保留 ERP 自己的东西**：六张主档的查重 SQL + 用它们的四个便捷入口。
pub use crate::business::shared::seq::{
    DEFAULT_PAD_WIDTH, FREE_CODE_MAX_ATTEMPTS, MASTER_CODE_TYPES, MAX_MANUAL_CODE_LEN, MAX_PAD_WIDTH, MIN_PAD_WIDTH,
    code_taken, format_master_code, is_master_code_type, manual_code, next_master_code, next_master_code_free_with,
    next_master_code_global, peek_master_code_with, resolve_manual_code, validate_manual_code,
};

/// 主数据编码 → 「该编码是否已被占用」的常量 SQL（只有一个 `?` 占位，绑定编码本身）。
///
/// 返回 `None` 表示本模块**不自带**该号段的查重表：不是主数据号段，或表不归本域管。
///
/// 只收录**本域持有的**主数据表；表名/列名在这里是常量，不接受外部拼接
/// （新增主数据表时在这里加一行，并补 [`is_master_code_type`]）。
///
/// # `EMP` 为什么不在表里（P2-36 · `erp → hr` 解环）
///
/// 工号**仍是主数据号段**（[`MASTER_CODE_TYPES`]，发号规则与其它号段完全一样），
/// 但员工主档是 **HR 的表**。这里是 ERP 的取号器，写死「读 HR 的表」就等于让 `erp`
/// 认 HR 的表名 / 列名 / 软删口径 —— 而列名恰恰在这里被猜错过一次
/// （工号列叫 `emp_no`，不是 `employee_no`；表还没上线时猜错也没有任何反馈）。
///
/// 所以查重口径交给表的属主：HR 调 [`next_master_code_free_with`] /
/// [`peek_master_code_with`] 并自带常量 SQL（见 `hr::employee_no` 的 `EMP_EXISTS_SQL`）。
/// 本模块只发号，不认表。新增「表在别的域」的号段时照此办理，别往这张表里塞。
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
        "AC" => Some("SELECT 1 FROM biz_asset_category WHERE category_code = ? AND deleted_at IS NULL"),
        // SCOPE-EXEMPT: 同上。
        "ET" => Some("SELECT 1 FROM biz_expense_type WHERE type_code = ? AND deleted_at IS NULL"),
        _ => None,
    }
}

/// 某个主数据编码是否已被占用（按 `seq_type` 自动选表，表名/列名来自常量表）。
///
/// 建档案时用它做**手工编码的预检**：撞了就返回「编码 `XXX` 已存在」的人话错误，
/// 不必等 DB 的部分唯一索引抛 `23505` 再翻译。
pub async fn master_code_taken(ex: &impl SqlExecutor, seq_type: &str, code: &str) -> DbResult<bool> {
    let Some(exists_sql) = master_code_exists_sql(seq_type) else {
        return Err(DbError::InvalidValue(format!(
            "唯一性校验失败：seq_type='{seq_type}' 不是主数据号段（内置查重表：SUP/CUS/MAT/WH/AC/ET；EMP 由 HR 侧用 `*_with` 自带查重 SQL）"
        )));
    };
    code_taken(ex, exists_sql, code).await
}

/// 同 [`next_master_code_free_with`]，用内置的主数据占位表（推荐入口）。
///
/// `seq_type` 不是本模块自带查重表的主数据号段（SUP/CUS/MAT/WH/AC/ET）时返回
/// `InvalidValue`（**不猜表名**）。`EMP` 请用 [`next_master_code_free_with`] 自带查重 SQL。
pub async fn next_master_code_free(ex: &impl SqlExecutor, seq_type: &str) -> DbResult<String> {
    let Some(exists_sql) = master_code_exists_sql(seq_type) else {
        return Err(DbError::InvalidValue(format!(
            "取号失败：seq_type='{seq_type}' 不是主数据号段（内置查重表：SUP/CUS/MAT/WH/AC/ET；EMP 由 HR 侧用 `*_with` 自带查重 SQL）"
        )));
    };
    next_master_code_free_with(ex, seq_type, exists_sql, FREE_CODE_MAX_ATTEMPTS).await
}

/// 同 [`next_master_code_free`]，但走全局 `db()`。
pub async fn next_master_code_free_global(seq_type: &str) -> DbResult<String> {
    next_master_code_free(db(), seq_type).await
}

/// 预览下一个可用主数据编码（**不消耗号**）。
///
/// # 用途
///
/// 交互方式 B（用户已拍板）：表单里点「生成编码」按钮先看号，保存时把看到的号当
/// **手工编码**提交。前端因此需要一个「只看不动」的接口 —— 就是本函数。
///
/// # 与 [`next_master_code_free`] 的唯一差别
///
/// **不推进 `biz_seq.current_no`**（纯读 + 查重），所以：
///
/// * 用户点了按钮又取消 ⇒ 号不会被白白吃掉（没有空号）；
/// * 同一时刻别人抢走这个号 ⇒ 保存时按手工编码撞唯一索引处理（409），再点一次按钮即可。
///
/// 也就是说预览是「**当下最优猜测**」，不是占号。要占号请直接建档案（取号与 INSERT 同事务）。
pub async fn peek_master_code(ex: &impl SqlExecutor, seq_type: &str) -> DbResult<String> {
    let Some(exists_sql) = master_code_exists_sql(seq_type) else {
        return Err(DbError::InvalidValue(format!(
            "预览编码失败：seq_type='{seq_type}' 不是主数据号段（内置查重表：SUP/CUS/MAT/WH/AC/ET；EMP 由 HR 侧用 `*_with` 自带查重 SQL）"
        )));
    };
    peek_master_code_with(ex, seq_type, exists_sql, FREE_CODE_MAX_ATTEMPTS).await
}

/// 同 [`peek_master_code`]，但走全局 `db()`（只读，不参与调用方事务）。
pub async fn peek_master_code_global(seq_type: &str) -> DbResult<String> {
    peek_master_code(db(), seq_type).await
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
        db().execute_with_params("DELETE FROM biz_seq WHERE seq_type = ?", vec![Value::from(seq_type)])
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
        db().execute_with_params("DELETE FROM biz_seq WHERE seq_type = ?", vec![Value::from(seq_type)])
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
    fn t02_exists_sql_covers_every_erp_owned_master_type() {
        // 内置查重表 = **本域持有**的 6 类主档（员工主档不在此列，见下）
        for seq_type in ["SUP", "CUS", "MAT", "WH", "AC", "ET"] {
            assert!(
                master_code_exists_sql(seq_type).is_some(),
                "{seq_type} 应有占位检查 SQL"
            );
            assert!(is_master_code_type(seq_type));
        }
        assert!(master_code_exists_sql("PO").is_none(), "单据号段不是主数据号段");
        assert!(master_code_exists_sql("SUP2").is_none());
        assert!(!is_master_code_type("SN"));

        // HR-14 + P2-36：`EMP` 仍是主数据号段（发号规则一致），但**查重口径不归本域** ——
        // 员工主档是 HR 的表，`erp` 不许在这里认它的表名/列名/软删口径
        // （列名被猜错过一次：`emp_no` 不是 `employee_no`）。
        assert!(is_master_code_type("EMP"), "EMP 必须登记为主数据号段");
        assert!(
            master_code_exists_sql("EMP").is_none(),
            "EMP 的查重 SQL 必须由表属主（HR）用 `*_with` 自带 —— 写在 erp 里就又是一条 erp→hr 直连"
        );
        assert_eq!(
            MASTER_CODE_TYPES.len(),
            7,
            "主数据号段仍应 7 类（去掉的是查重表，不是号段）"
        );
        assert!(
            master_code_exists_sql("EMP").is_none() && is_master_code_type("EMP"),
            "两者语义分开：**号段登记** ≠ **查重表归属**"
        );
    }

    // ---------- ①b 手工编码：空 → 自动；有值 → 保留；非法 → 报错 ----------

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

        let expected: [(&str, i32); 6] = [("SUP", 4), ("CUS", 4), ("MAT", 4), ("WH", 2), ("AC", 2), ("ET", 2)];
        for (seq_type, pad) in expected {
            let row = rows
                .iter()
                .find(|r| r.get_typed::<String>("seq_type").expect("seq_type") == seq_type)
                .unwrap_or_else(|| panic!("缺 seq_type={seq_type} 的取号规则（V0337 未应用？）"));
            assert_eq!(row.get_typed::<String>("prefix").expect("prefix"), seq_type);
            // 真实列是 NOT NULL + 白名单，'none' 即「不带日期」（卡面写的 NULL 落不了库）
            assert_eq!(row.get_typed::<String>("date_pattern").expect("date_pattern"), "none");
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
            assert_eq!(row.get_typed::<String>("separator").expect("separator"), "-");
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
        assert_eq!(next_master_code(db(), "T2DGT").await.expect("取号"), "T2D-01");
        assert_eq!(next_master_code(db(), "T2DGT").await.expect("取号"), "T2D-02");
        // 手工把流水推到 99（模拟老库），下一次必须是 100（3 位），不回到 00
        db().execute_with_params(
            "UPDATE biz_seq SET current_no = 99 WHERE seq_type = ?",
            vec![Value::from("T2DGT")],
        )
        .await
        .expect("推流水");
        assert_eq!(next_master_code(db(), "T2DGT").await.expect("取号"), "T2D-100");
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
        assert_eq!(sorted.len(), 10, "10 次取号必须得到 10 个不同号，实际 {codes:?}");
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
        // 预检入口（建档案时给「编码已存在」用）与占位 SQL 同口径
        assert!(
            master_code_taken(db(), "SUP", code_taken_by_seed)
                .await
                .expect("查占用"),
            "0001 已被占用"
        );
        assert!(
            !master_code_taken(db(), "SUP", "T7S-0002").await.expect("查占用"),
            "0002 还没人占"
        );
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
        assert!(msg.contains("SUP/CUS/MAT/WH/AC/ET"), "错误里必须列出支持范围：{msg}");

        // 规则行不存在：报错指向 V0337
        let err = next_master_code(db(), "NO_RULE_TYPE").await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("NO_RULE_TYPE"), "错误里必须带上 seq_type：{msg}");
        assert!(msg.contains("V0337"), "错误里必须指出应由哪条迁移种入：{msg}");

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

    // ---------- ⑧ 预览（CODE-04）：「只看不动」，不消耗号 ----------

    #[tokio::test]
    async fn t09_peek_never_consumes() {
        if !ensure_db().await {
            return;
        }
        const SEQ: &str = "TPEEK";
        reset_test_seq(SEQ, "TPK", 4, 0).await;
        // 测试号段不在六类主数据里 ⇒ 没有内置占位表，用 SUP 的占位 SQL 代打
        // （只查 `biz_supplier.supplier_code`，与 TPK-xxxx 不会真撞上，除非下面故意造一行）
        let exists_sql = master_code_exists_sql("SUP").expect("SUP 占位 SQL");

        // 非主数据号段走内置入口：明确报错，不猜表
        let err = peek_master_code(db(), SEQ).await.expect_err("非六类必须报错");
        assert!(
            format!("{err}").contains("不是主数据号段"),
            "错误要说清楚支持范围：{err}"
        );

        // 连看三次都是同一个号（真实取号才会推进）
        for _ in 0..3 {
            assert_eq!(
                peek_master_code_with(db(), SEQ, exists_sql, 5).await.expect("预览"),
                "TPK-0001",
                "预览不能推进 current_no"
            );
        }
        assert_eq!(current_no_of(SEQ).await, 0, "预览后 current_no 必须还是 0");

        // 真实取号拿到的正是刚才预览的那个号
        assert_eq!(
            next_master_code(db(), SEQ).await.expect("取号"),
            "TPK-0001",
            "预览值与随后真实取号必须一致"
        );
        assert_eq!(current_no_of(SEQ).await, 1);

        // 预览同样会跳过已被占用的号（用 SUP 的占位 SQL + 临时供应商夹具）
        let taken = "TPK-0002";
        db().execute_with_params(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, credit_days, status) \
             VALUES (gen_random_uuid(), ?, 'CODE-04 预览撞号夹具', 30, 1)",
            vec![Value::from(taken)],
        )
        .await
        .expect("造已占用编码");
        db().execute_with_params(
            "UPDATE biz_seq SET current_no = 1 WHERE seq_type = ?",
            vec![Value::from(SEQ)],
        )
        .await
        .expect("把预览起点拨到 1");
        let peeked = peek_master_code_with(db(), SEQ, exists_sql, 5)
            .await
            .expect("按自定义占位 SQL 预览");
        assert_eq!(peeked, "TPK-0003", "0002 已被占用，预览必须顺延到 0003");
        assert_eq!(current_no_of(SEQ).await, 1, "预览依旧不动 current_no");

        db().execute_with_params(
            "DELETE FROM biz_supplier WHERE supplier_code = ?",
            vec![Value::from(taken)],
        )
        .await
        .expect("清供应商夹具");
        drop_test_seq(SEQ).await;
    }
}
