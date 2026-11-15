//! 主数据编码取号**引擎**（P2-B 解环：从 `business/erp/seq.rs` 下移到 `shared`）。
//!
//! # 这个文件解决什么
//!
//! `biz_seq` 里的**纯流水号段**（`date_pattern='none'`、`period_key='GLOBAL'`，永不重置）
//! 是一张**全局主数据字典**，不属于任何一个业务域：ERP 的供应商 / 客户 / 物料 / 仓库 /
//! 资产分类 / 费用类型用它，HR 的工号（`EMP`）也用它。原先引擎长在 ERP 里，于是 HR 为了
//! 取一个工号就得 `use` 一条指向 ERP 的路径（`erp` 域名 + `::seq`）—— 这就是 P2-B 卡 `t_3442b4fb`
//! 摘掉的第二条 `hr → erp` 代码回边。
//!
//! ```text
//! 改前：hr ──next_master_code_free_with──→ erp::seq   （HR 为了一个 UPDATE…RETURNING 依赖 ERP）
//! 改后：hr ──next_master_code_free_with──→ shared::seq（ERP 侧走 erp::seq 的同名再导出）
//! ```
//!
//! # 这里有什么 / 没什么（刻意的分界）
//!
//! | | 归属 | 理由 |
//! |---|---|---|
//! | 取号引擎（读规则 / 推进流水 / 撞号顺延 / 预览） | **本文件** | 纯 `biz_seq` 读写，与任何业务表无关 |
//! | 主数据号段清单 [`MASTER_CODE_TYPES`] | **本文件** | 「哪些号段是主数据编码」是全公司口径，不随表属主变 |
//! | 手工编码解析（trim / 字符集 / 长度） | **本文件** | 纯函数，六个主数据入口共用一份口径 |
//! | **各主数据表**的「编码是否已被占用」SQL | **不在这里** | 表名 / 列名 / 软删口径归**表属主**：ERP 的六张主档在 `business/erp/seq.rs`，员工主档在 HR 的 `employee_no`（经 `*_with` 入口自带） |
//!
//! 所以本模块**只发号、不认表**：要查重就走两个 `*_with` 入口（`exists_sql` 由调用方给常量 SQL，
//! 不接受拼接）。这也是为什么 `EMP` 虽然登记在 [`MASTER_CODE_TYPES`] 里，但这里没有任何
//! 员工表的名字 —— 号段归属与表归属是两个事实。
//!
//! # 三个入口 + 两个执行器变体
//!
//! 1. [`next_master_code`] —— 纯取号（**不查重**）。调用方自己保证编码没被占用时用。
//! 2. [`next_master_code_free_with`] —— 取号并**跳过已占用的号**（主数据推荐用这个），
//!    占位检查 SQL 由调用方给。例：老库已有 `SUP-0001`（V0137 种子）⇒ 第一次自动编码给 `SUP-0002`。
//! 3. [`peek_master_code_with`] —— **只看不动**：预览下一个可用编码，不推进 `current_no`
//!    （前端「生成编码」按钮用；不是占号）。
//!
//! 每个入口都接受 `&impl SqlExecutor`，所以既能在**调用方的事务里**取号（建档案与取号同一事务，
//! 回滚时号一起回滚，不留空号），也能传 `db()` 走自动提交（离线工具）。
//!
//! # 并发安全
//!
//! 发号是单语句 `UPDATE biz_seq … RETURNING current_no`：PG 对该行加行锁，两个并发事务
//! 先后拿到 `n` 与 `n+1`，**天然不重号**。注意持锁到事务提交：同一事务里取完号尽快落库，
//! 别在长事务里连续取多个号。
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
//! 历史上 `AC` / `ET` 多走了一步 `to_ascii_uppercase`，日志里出现「同一张编码规则表两种行为」，
//! 对接方按一类经验写另一类会踩坑 —— 已抹平。理由：有些企业自有编码区分大小写，
//! 替用户改大小写等于改他的编码；自动取号产出的 `AC-01` / `ET-01` 本来是大写，不受影响。
//!
//! # 迁移依赖
//!
//! 六条 ERP 主数据规则由 `V0337__master_data_seq.up.sql` 种入（幂等）；`EMP` 由
//! `V0386__hr_seq_dict` 种入。`pad_width` 列也在那两条迁移里。

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

/// 主数据号段（顺序与用户拍板的表格一致，`EMP` 由 HR 追加在末尾）。
///
/// **登记 = 这个号段属于「主数据编码」这一类**（永不重置、`date_pattern='none'`），
/// 与「表归谁管」无关：`EMP` 的表在 HR，另外六类的表在 ERP。
pub const MASTER_CODE_TYPES: [&str; 7] = ["SUP", "CUS", "MAT", "WH", "AC", "ET", "EMP"];

/// 是否是主数据号段。
pub fn is_master_code_type(seq_type: &str) -> bool {
    MASTER_CODE_TYPES.contains(&seq_type)
}

/// 手工指定编码的最大长度（与 `biz_supplier.supplier_code` / `biz_customer.code`
/// 等主数据编码列的 `VARCHAR(40)` 逐字同口径）。
pub const MAX_MANUAL_CODE_LEN: usize = 40;

/// 手工编码允许的字符集：ASCII 字母、数字，以及 `-` `_` `.` 三个分隔符。
///
/// **不强制 `SUP-0001` 这种形态**（用户已拍板）：企业自有编码体系（`GYS-001` / `S.001.A`）
/// 必须能共存，这里只挡掉真正没法用的输入 —— 空白、控制字符、中文标点、`/`、`#` 等。
fn is_manual_code_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')
}

/// 归一化用户传来的编码：`None` / `""` / 纯空白 ⇒ `None`（= 交给取号器自动生成）。
///
/// 前端三种「没填」的形态（`null` 反序列化成 `None`、空串、空格）在这里统一收口，
/// 调用方不必各自判断。
pub fn manual_code(input: Option<&str>) -> Option<String> {
    input.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// 手工编码格式校验（**只校验可用性，不校验编码规则**）：
///
/// - 长度：去空白后 `1..=max_len`（`chars().count()`，与 `VARCHAR(n)` 同口径）；
/// - 字符集：见 [`is_manual_code_char`]。
///
/// 返回 `Err(人话原因)`，由调用方翻译成自己的错误码（供应商 / 客户各自 `400 + 10001`）。
pub fn validate_manual_code(code: &str, max_len: usize) -> std::result::Result<(), String> {
    let text = code.trim();
    if text.is_empty() {
        return Err("编码不能为空（留空则由系统自动生成）".to_string());
    }
    let len = text.chars().count();
    if len > max_len {
        return Err(format!("编码最长 {max_len} 个字符（当前 {len} 个）"));
    }
    if let Some(bad) = text.chars().find(|ch| !is_manual_code_char(*ch)) {
        return Err(format!(
            "编码里不能出现 `{}`：只允许字母、数字与 `-` `_` `.`（企业自有编码可直接沿用）",
            bad.escape_debug()
        ));
    }
    Ok(())
}

/// 一次搞定「有没有手工指定编码」：`Ok(None)` = 留空（交给取号器），
/// `Ok(Some(code))` = 合法的手工编码（已去首尾空白），`Err(原因)` = 格式不合法。
///
/// 六个主数据建档路径（供应商 / 客户 / 物料 / 仓库 / 资产分类 / 费用类型）与 HR 的工号
/// 都走这一个入口，保证各边口径一致。
pub fn resolve_manual_code(input: Option<&str>, max_len: usize) -> std::result::Result<Option<String>, String> {
    let Some(code) = manual_code(input) else {
        return Ok(None);
    };
    validate_manual_code(&code, max_len)?;
    Ok(Some(code))
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

/// 一条取号规则（`biz_seq` 里那一行里取号需要的列）。
///
/// `current_no` / `step` 只有**预览**（[`peek_master_code_with`]）会用到：真实取号走
/// `UPDATE … RETURNING`，不让「读出来的旧值 + 加法」参与发号，避免并发下算重。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SeqRule {
    prefix: String,
    separator: String,
    pad_width: i32,
    current_no: i64,
    step: i64,
}

/// 读规则（**必须**是启用中的存活行；停用 = 不参与取号）。
async fn load_rule(ex: &impl SqlExecutor, seq_type: &str) -> DbResult<SeqRule> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    let row = ex
        .query_first_sql(
            "SELECT prefix, separator, pad_width::int AS pad_width, \
                    current_no, step::bigint AS step \
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
        current_no: row.get_typed::<i64>("current_no")?,
        step: row.get_typed::<i64>("step")?,
    })
}

/// 原子推进一个号并返回新流水（单语句 `UPDATE … RETURNING`，行锁保证并发不重号）。
///
/// 纯流水号段**不看 `period_key`**：`GLOBAL` 只是「永不清零」的标记，
/// 推进逻辑与期间无关（对照单据号段的三分支写法，见 `business/erp/serial_model.rs`）。
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
    Ok(format_master_code(&rule.prefix, &rule.separator, rule.pad_width, no))
}

/// 同 [`next_master_code`]，但走全局 `db()`（自动提交；无法参与调用方事务）。
pub async fn next_master_code_global(seq_type: &str) -> DbResult<String> {
    next_master_code(db(), seq_type).await
}

/// 编码是否已被占用（`exists_sql` 必须是常量 SQL，只绑一个参数：候选编码）。
///
/// 公开它是为了让**查重表的属主**（ERP 的六张主档、HR 的员工主档）能在自己的
/// `master_code_taken` 之类入口里复用同一份执行口径，而不必各自写一遍 `query_first_sql`。
pub async fn code_taken(ex: &impl SqlExecutor, exists_sql: &str, code: &str) -> DbResult<bool> {
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
/// 不允许把用户输入拼进去 —— 表名 / 列名 / 软删口径归表属主。
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

/// 预览下一个可用主数据编码（**不消耗号**）。
///
/// # 用途
///
/// 交互方式 B（用户已拍板）：表单里点「生成编码」按钮先看号，保存时把看到的号当
/// **手工编码**提交。前端因此需要一个「只看不动」的接口 —— 就是本函数。
///
/// # 与 [`next_master_code_free_with`] 的唯一差别
///
/// **不推进 `biz_seq.current_no`**（纯读 + 查重），所以：
///
/// * 用户点了按钮又取消 ⇒ 号不会被白白吃掉（没有空号）；
/// * 同一时刻别人抢走这个号 ⇒ 保存时按手工编码撞唯一索引处理（409），再点一次按钮即可。
///
/// 也就是说预览是「**当下最优猜测**」，不是占号。要占号请直接建档案（取号与 INSERT 同事务）。
pub async fn peek_master_code_with(
    ex: &impl SqlExecutor,
    seq_type: &str,
    exists_sql: &str,
    max_attempts: u32,
) -> DbResult<String> {
    let rule = load_rule(ex, seq_type).await?;
    let attempts = max_attempts.max(1);
    // 步长兜底 ≥1：即使有脏数据把 step 写成 0，也不会原地打转出重复号。
    let step = rule.step.max(1);
    let mut no = rule.current_no;
    for _ in 0..attempts {
        no += step;
        let code = format_master_code(&rule.prefix, &rule.separator, rule.pad_width, no);
        if !code_taken(ex, exists_sql, &code).await? {
            return Ok(code);
        }
    }
    Err(DbError::InvalidValue(format!(
        "预览编码失败：seq_type='{seq_type}' 连续 {attempts} 个号都已被占用。\
         请核对主数据表里的历史编码，或把 biz_seq.current_no 抬到已有最大流水之上。"
    )))
}

#[cfg(test)]
mod tests {
    //! 纯单测：位数 / 分隔符 / 不截断 / 手工编码解析（离线，不连库）。
    //!
    //! 用例逐字迁自 `business/erp/seq.rs` 的同名用例（搬家不改口径）。
    //! 连库用例（连续取号 / 并发不重号 / 撞号顺延 / 预览不耗号）留在 `business/erp/seq.rs`：
    //! 它们要造 `biz_supplier` 夹具，属 ERP 的测试面。

    use super::*;

    /// ① 位数不足左补 0，超过位数**不截断**，非法位数按合法区间收敛。
    #[test]
    fn format_pads_by_width_without_truncating() {
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

    /// ② 三种「没填」都归一到 `None`；有值只去空白，**不校验 `SUP-xxx` 形态**。
    #[test]
    fn manual_code_resolution_keeps_user_input() {
        assert_eq!(manual_code(None), None, "null ⇒ 自动生成");
        assert_eq!(manual_code(Some("")), None, "空串 ⇒ 自动生成");
        assert_eq!(manual_code(Some("   ")), None, "纯空白 ⇒ 自动生成");
        assert_eq!(
            manual_code(Some("  GYS-001 ")),
            Some("GYS-001".to_string()),
            "手工编码只 trim，不改大小写与分隔符"
        );
        assert_eq!(
            resolve_manual_code(Some("S.001_A"), MAX_MANUAL_CODE_LEN),
            Ok(Some("S.001_A".to_string())),
            "企业自有编码体系要能共存"
        );
        assert_eq!(
            resolve_manual_code(Some("SUP-9999"), MAX_MANUAL_CODE_LEN),
            Ok(Some("SUP-9999".to_string()))
        );
        assert_eq!(
            resolve_manual_code(Some("  "), MAX_MANUAL_CODE_LEN),
            Ok(None),
            "空白仍是自动生成"
        );
    }

    /// ③ 手工编码格式校验：超长 / 含非法字符 → 人话错误（不静默截断或吞掉）。
    #[test]
    fn manual_code_validation_rejects_unusable_input() {
        let long = "A".repeat(MAX_MANUAL_CODE_LEN + 1);
        let err = resolve_manual_code(Some(&long), MAX_MANUAL_CODE_LEN).expect_err("超长必须被拒");
        assert!(err.contains("最长"), "超长文案：{err}");

        let exact = "A".repeat(MAX_MANUAL_CODE_LEN);
        assert!(
            resolve_manual_code(Some(&exact), MAX_MANUAL_CODE_LEN).is_ok(),
            "恰好 {MAX_MANUAL_CODE_LEN} 个字符是合法的（与 VARCHAR(40) 同口径）"
        );

        for bad in ["GYS 001", "供应商001", "GYS/001", "GYS#001", "GYS\t001"] {
            match resolve_manual_code(Some(bad), MAX_MANUAL_CODE_LEN) {
                Ok(got) => panic!("`{bad}` 应被拒，却通过：{got:?}"),
                Err(msg) => assert!(msg.contains("只允许"), "`{bad}` 的错误文案应说明字符集：{msg}"),
            }
        }
    }

    /// ④ 主数据号段清单：7 类、含 `EMP`，且**清单与表归属无关**（本模块不认任何表）。
    #[test]
    fn master_code_types_are_registered_by_semantics_not_by_owner() {
        assert_eq!(MASTER_CODE_TYPES.len(), 7, "EMP 加入后应 7 类");
        for seq_type in ["SUP", "CUS", "MAT", "WH", "AC", "ET", "EMP"] {
            assert!(is_master_code_type(seq_type), "{seq_type} 必须是主数据号段");
        }
        assert!(!is_master_code_type("PO"), "单据号段不是主数据号段");
        assert!(!is_master_code_type("SN"));
    }
}
