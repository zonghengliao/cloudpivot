//! 数据准备脚本化（P2-62）：**统一的 `ensure_*` 夹具**。
//!
//! # 一句话
//!
//! 真库用例要的「部门 / 用户 / 角色成员 / 单位 / 物料 / 仓库 / 科目」不再各写一段
//! `INSERT INTO …`，而是调本模块的 `ensure_*`：**幂等、确定性 id、缺才补**。
//!
//! # 为什么（改前的真实形态）
//!
//! P2-62 盘点（`backend/app/src` 测试代码）：**手写 `INSERT` 736 处 / 245 个文件**，
//! 其中主数据类最多：
//!
//! | 表 | 处数 | 列清单变体 |
//! |---|--:|---|
//! | `sys_user` | 28 | 6 |
//! | `biz_material` | 24 | 8 |
//! | `biz_warehouse` | 22 | 7 |
//! | `biz_unit` | 9 | 2 |
//! | `sys_dept` | 8 | 2 |
//! | `biz_account` | 6 | 3 |
//!
//! 外加 **41 个文件各自定义了一遍 `fn uniq()`**（4 种实现：`String` 取模 1e9 两位/三位、
//! `u64` 混合计数）、8 个文件各自 `fn next_year()`（基数 2100/2200/2500 各写一遍）、
//! 8 个文件各自 `fn tail_code()` / `fn fx_suffix_code()`。后果不是「不好看」，是三条实际损失：
//!
//! 1. **同一件事四份实现**：`uniq()` 的 String 版只差 `{seq:02}` / `{seq:03}`，改一处忘了另三处；
//! 2. **主数据列清单漂移**：`biz_material` 8 种列组合，新写的用例照抄哪一份全靠运气；
//! 3. **每建一份夹具就扫一遍库**：`sweep_leftovers()` 每个 Fixture 跑 13 条 `DELETE`
//!    （单文件最多 25 次 build ⇒ 325 条 DELETE），而这些清理只需要**每进程一次**。
//!
//! # 契约（四条，别破）
//!
//! 1. **幂等**：同一 `code` 调两次，落同一行（`ON CONFLICT DO NOTHING` + 回读）；
//! 2. **确定性 id**：[`fixture_uuid`] 由 key 派生（SHA-256 前 16 字节，version 8），
//!    不是 `Uuid::now_v7()` —— 于是「同一夹具跨进程/跨机器是同一个 id」，断言可复现；
//! 3. **只碰自己的行**：清场按**自己的 code 精确匹配 / 自己的 id**，绝不按前缀扫全表；
//! 4. **缺才补**：库里已有可用行（单位 / 部门 / 审批岗位成员）时**先用现成的**，
//!    一行都不加 —— 种子库上零新增，列表类断言基线不漂移。
//!
//! # API
//!
//! | 函数 | 造什么 | 语义 |
//! |---|---|---|
//! | [`uniq_suffix`] / [`uniq_u64`] | 唯一后缀 | 纳秒取模 1e9 + 进程内序号（跨进程不撞） |
//! | [`tail_code`] / [`last_chars`] | 编码尾巴 | 编码列有长度上限时截尾（只留数字 / 保留 n 字符） |
//! | [`year_seq`] / [`period_code`] | 会计年度 / 期间码 | 每个夹具独占一个年度（远离真实数据） |
//! | [`ensure_unit`] | 计量单位 | 库里启用中的单位直接用，没有才造 `TST-U` |
//! | [`ensure_material`] / [`ensure_material_kind`] | 物料 | 默认 `FINISHED`；id 由 `material:<code>` 派生 |
//! | [`ensure_warehouse`] | 叶子仓 | `is_leaf=1`、`level=1`、`path=/<code>` |
//! | [`ensure_account`] | 末级科目 | `is_leaf=1`、`level=2`、方向 `DEBIT`；类别由调用方给 |
//! | [`ensure_supplier`] / [`ensure_customer`] | 供应商 / 客户 | 编码唯一、幂等 |
//! | [`sweep_once`] | 清场 | 同一 key **每进程只扫一次**（上轮崩溃残留），不是每个夹具一次 |
//! | [`ensure_any_dept`] … [`ensure_approver_roles`] | 底座（`sys_*`） | 见 [`cloudpivot_kernel::shared::test_helpers`]，此处 re-export |
//!
//! # 新增夹具怎么写
//!
//! ```ignore
//! // 改前：10 行手写 INSERT + 自己造 uniq
//! let suffix = uniq();
//! let unit: Uuid = db().scalar_with_params("SELECT id FROM biz_unit …", vec![]).await?;
//! let product = Uuid::now_v7();
//! db().exec_sql("INSERT INTO biz_material (id, material_code, material_name, unit_id, \
//!                material_type, status, created_at, updated_at) VALUES (?::uuid, ?, ?, ?::uuid, \
//!                'FINISHED', 1, now(), now())".to_string(), vec![…]).await?;
//!
//! // 改后：2 行
//! let suffix = test_seed::uniq_suffix();
//! let product = test_seed::ensure_material(&format!("COSTB18-MAT-{suffix}"), "成本对象夹具产出品").await;
//! ```
//!
//! # 边界（本模块**不**做什么）
//!
//! 单据 / 明细（`biz_stock_out` / `biz_cost_collect` / `biz_voucher` …）**不进**这里：
//! 它们的列清单随用例语义变化（状态、金额、来源单据），用例外显式写 SQL 才看得见断言前提。
//! 统一的是**主数据**——同一种东西在所有用例里长得一样，才值得收进 `ensure_*`。

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use nik::prelude::*;
use nik::uuid::Uuid;

/// 底座（`sys_*`）夹具：实现留在内核 crate（`cloudpivot-kernel`），此处只做转发，
/// 让用例**一个 `use` 拿到全部 ensure_**（不必记哪个在 kernel、哪个在 app）。
pub use cloudpivot_kernel::shared::test_helpers::{
    FIXTURE_DEPT_CODE, FIXTURE_DEPT_NAME, FIXTURE_DEPT2_CODE, FIXTURE_DEPT2_NAME, bind_role, ensure_any_dept,
    ensure_any_user, ensure_approver_roles, ensure_dept, ensure_dept_user, ensure_fixture_dept, ensure_fixture_user,
    ensure_role_member, ensure_two_users, ensure_user, fixture_uuid, purge_dept, purge_user,
};

// ============================================================================
// 一、编号与编码（改前：41 个文件各自定义一遍 `uniq()`）
// ============================================================================

/// 唯一后缀：**纳秒取模 1e9 + 进程内序号（三位补零）**，共 12 位数字。
///
/// 为什么不直接 `Uuid::now_v7()`：编码列（`material_code` / `warehouse_code` / `account_code`）
/// 有长度上限且要「人眼可读 + 只含数字」（科目段校验要求纯数字），UUID 塞不进去。
///
/// 跨进程不撞的判据：同进程靠 `SEQ`，跨进程靠纳秒；崩溃后重跑最坏情况是**同纳秒同序号**，
/// 此时 `ensure_*` 的 `ON CONFLICT DO NOTHING` + 回读仍是幂等的（不会红，只会复用）。
pub fn uniq_suffix() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("{}{seq:03}", now % 1_000_000_000)
}

/// 唯一整数（HR 侧老写法：`now.wrapping_add(seq * 7919)`）。语义与 [`uniq_suffix`] 同源。
pub fn uniq_u64() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    now.wrapping_add(seq.wrapping_mul(7_919))
}

/// 编码尾巴：**只留数字**，取末尾 `take` 位（`account_code` / `warehouse_code` 有长度上限）。
pub fn tail_code(suffix: &str, take: usize) -> String {
    suffix
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .take(take)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

/// 编码尾巴：保留末尾 `n` 个字符（不过滤字符；等价于老的 `fx_suffix_code`）。
pub fn last_chars(suffix: &str, n: usize) -> String {
    let chars: Vec<char> = suffix.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

/// 会计年度序号：`base + 进程内计数器 % span`，**每个夹具独占一个年度**。
///
/// 为什么不用当前年：真库里当前年是真实数据在用的，夹具撞进去会让「期间没关」这类断言
/// 依赖运行时日期。`base` 由调用方给（各模块历史上分别是 2100 / 2200 / 2400 / 2500，
/// 保持原值以免撞上别的模块留在库里的夹具年度）。
pub fn year_seq(base: i32, span: u64) -> i32 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    base + i32::try_from(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % span).unwrap_or(0)
}

/// 期间码 `YYYY-MM`。
pub fn period_code(year: i32, month: u32) -> String {
    format!("{year}-{month:02}")
}

// ============================================================================
// 二、写库小工具（失败一律 panic：夹具写不进去就是夹具坏了，不许静默）
// ============================================================================

/// 执行一条写 SQL，返回受影响行数；失败一律 panic（夹具写不进去就是夹具坏了，不许静默）。
///
/// 返回行数的用处：`ON CONFLICT DO NOTHING` 插入成功 ⇒ 行就是我派生的 id，
/// **不必再 `SELECT` 回读**（每个夹具省一次往返；99→145 一次往返约 0.3 ms，
/// 全量 5000+ 用例累起来是秒级）。
async fn exec(sql: &str, params: Vec<Value>) -> u64 {
    match db().execute_with_params(sql, params).await {
        Ok(n) => n,
        Err(err) => panic!("夹具 SQL 失败：{err}\nSQL: {sql}"),
    }
}

async fn scalar_uuid(sql: &str, params: Vec<Value>, what: &str) -> Uuid {
    db().scalar_with_params::<Uuid>(sql, params)
        .await
        .unwrap_or_else(|err| panic!("夹具回读失败（{what}）：{err}\nSQL: {sql}"))
}

/// 回读：按唯一编码取回 id（只在「插入被 `ON CONFLICT` 吃掉」时才需要）。
async fn id_by_code(table: &str, column: &str, code: &str, what: &str) -> Uuid {
    scalar_uuid(
        &format!("SELECT id FROM {table} WHERE {column} = ? LIMIT 1"),
        vec![Value::Text(code.to_string())],
        what,
    )
    .await
}

/// 幂等写一行：插入成功（`n > 0`）直接返回**派生 id**；被 `ON CONFLICT` 吃掉才回读。
///
/// 这是「一个夹具两条往返」变「一条」的关键：`ON CONFLICT DO NOTHING` 的空码路径
/// 只在「上一轮同一 code 的行还在库里」时发生（真库演练常见），日常全新后缀都走快路径。
async fn upsert_id(sql: &str, params: Vec<Value>, id: Uuid, table: &str, column: &str, code: &str, what: &str) -> Uuid {
    if exec(sql, params).await > 0 {
        return id;
    }
    id_by_code(table, column, code, what).await
}

// ============================================================================
// 三、业务主数据（`biz_*`）—— 每个都是「同一种东西在所有用例里长得一样」
// ============================================================================

/// 计量单位：**库里启用中的单位直接用**（种子库零新增），一个都没有才造 `TST-U`。
///
/// 排序带 `unit_code` 兜底：`V…` 种子灌的单位 `created_at` 可能**同一时刻**（实测 3 行同值），
/// 只按 `created_at` 排会退化成堆序 —— 同一库里不同进程可能挑到不同单位，
/// 「入库明细的 `unit_id` = 夹具单位」这类断言就会随机红。
pub async fn ensure_unit() -> Uuid {
    static CACHED: OnceLock<Uuid> = OnceLock::new();
    if let Some(id) = CACHED.get() {
        return *id;
    }
    let rows = db()
        .query_sql(
            "SELECT id FROM biz_unit WHERE deleted_at IS NULL AND status = 1 \
             ORDER BY created_at, unit_code LIMIT 1"
                .to_string(),
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("查计量单位失败：{err}"));
    if let Some(id) = rows.first().and_then(|row| row.get_typed::<Uuid>("id").ok()) {
        let _ = CACHED.set(id);
        return id;
    }
    let id = fixture_uuid("unit:TST-U");
    exec(
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status) \
         VALUES (?, 'TST-U', 'TST 夹具单位', 1, 1, 1) ON CONFLICT DO NOTHING",
        vec![Value::from(&id)],
    )
    .await;
    let _ = CACHED.set(id);
    id
}

/// 物料（默认 `FINISHED` 产出品）。`code` 建议带唯一后缀，`name` 会进列表/导出断言。
pub async fn ensure_material(code: &str, name: &str) -> Uuid {
    ensure_material_kind(code, name, "FINISHED").await
}

/// 物料（指定 `material_type`：`RAW` / `FINISHED` …）。
///
/// `id` 由 `material:<code>` 派生 ⇒ 同一 code 跨进程是同一个 id；`code` 已存在时**复用**
/// （这就是「缺才补」：上一轮跑完没清掉的行，这一轮直接接着用，不会撞 `uk_biz_material_code`）。
pub async fn ensure_material_kind(code: &str, name: &str, material_type: &str) -> Uuid {
    let unit = ensure_unit().await;
    let id = fixture_uuid(&format!("material:{code}"));
    upsert_id(
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, \
                                   status, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 1, now(), now()) ON CONFLICT DO NOTHING",
        vec![
            Value::from(&id),
            Value::Text(code.to_string()),
            Value::Text(name.to_string()),
            Value::from(&unit),
            Value::Text(material_type.to_string()),
        ],
        id,
        "biz_material",
        "material_code",
        code,
        &format!("物料 {code}"),
    )
    .await
}

/// 叶子仓库：`is_leaf=1` / `level=1` / `warehouse_path=/{code}` / 允许负库存按库默认。
pub async fn ensure_warehouse(code: &str, name: &str) -> Uuid {
    let id = fixture_uuid(&format!("warehouse:{code}"));
    upsert_id(
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, \
                                    is_leaf, level, status, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 1, 1, 1, now(), now()) ON CONFLICT DO NOTHING",
        vec![
            Value::from(&id),
            Value::Text(code.to_string()),
            Value::Text(name.to_string()),
            Value::Text(format!("/{code}")),
        ],
        id,
        "biz_warehouse",
        "warehouse_code",
        code,
        &format!("仓库 {code}"),
    )
    .await
}

/// 末级科目：`level=2` / `is_leaf=1` / 方向 `DEBIT`；`category` 由调用方给
/// （`COST` / `EXPENSE` / `ASSET` …，各模块口径不同，不在这里替你猜）。
pub async fn ensure_account(code: &str, name: &str, category: &str) -> Uuid {
    let id = fixture_uuid(&format!("account:{code}"));
    upsert_id(
        "INSERT INTO biz_account (id, account_code, account_name, parent_id, level, \
                                  account_category, balance_direction, is_leaf, status, \
                                  created_at, updated_at) \
         VALUES (?, ?, ?, NULL, 2, ?, 'DEBIT', 1, 1, now(), now()) ON CONFLICT DO NOTHING",
        vec![
            Value::from(&id),
            Value::Text(code.to_string()),
            Value::Text(name.to_string()),
            Value::Text(category.to_string()),
        ],
        id,
        "biz_account",
        "account_code",
        code,
        &format!("科目 {code}"),
    )
    .await
}

/// 供应商（`credit_days` 给默认 30 天：多数用例只关心编码）。
pub async fn ensure_supplier(code: &str, name: &str) -> Uuid {
    let id = fixture_uuid(&format!("supplier:{code}"));
    upsert_id(
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, credit_days) \
         VALUES (?, ?, ?, 1, 30) ON CONFLICT DO NOTHING",
        vec![
            Value::from(&id),
            Value::Text(code.to_string()),
            Value::Text(name.to_string()),
        ],
        id,
        "biz_supplier",
        "supplier_code",
        code,
        &format!("供应商 {code}"),
    )
    .await
}

/// 客户。
///
/// ⚠ `biz_customer.status` 是 **VARCHAR + `ck_biz_customer_status CHECK (status IN ('ACTIVE','FROZEN'))`**
/// （V0260），与其它主数据表的 `smallint 1/0` **不是一套**：写 `1` 会直接撞 CHECK 23514。
/// 这个 `ensure_*` 在 M1-22 之前没有调用方，写 `1` 的错误一直没暴露（首个用户是
/// `receivable::aging_partial_tests` 的账龄夹具）。
pub async fn ensure_customer(code: &str, name: &str) -> Uuid {
    let id = fixture_uuid(&format!("customer:{code}"));
    upsert_id(
        "INSERT INTO biz_customer (id, code, name, status) VALUES (?, ?, ?, 'ACTIVE') \
         ON CONFLICT DO NOTHING",
        vec![
            Value::from(&id),
            Value::Text(code.to_string()),
            Value::Text(name.to_string()),
        ],
        id,
        "biz_customer",
        "code",
        code,
        &format!("客户 {code}"),
    )
    .await
}

/// 会计期间（`YYYY-MM` + 月初/月末）。状态固定 `OPEN`（需要别的状态用 UPDATE 改，
/// 不在这里加参数：期间状态的语义属于用例自己的场景，夹具只负责「这一行存在」）。
pub async fn ensure_accounting_period(year: i32, month: u32) -> Uuid {
    let code = period_code(year, month);
    let id = fixture_uuid(&format!("period:{code}"));
    let start = format!("{year}-{month:02}-01");
    let end = format!("{year}-{month:02}-{:02}", days_in_month(year, month));
    // 年月与日期**内联**（都是整数派生，不可能注入）：`date` 列吃文本参数会报
    // `error serializing parameter N`（驱动按列类型编码），内联 + `::date` 是最稳的写法。
    let sql = format!(
        "INSERT INTO biz_accounting_period (id, period_year, period_month, period_code, \
                                            start_date, end_date, status) \
         VALUES (?, {year}, {month}, ?, '{start}'::date, '{end}'::date, 'OPEN') \
         ON CONFLICT DO NOTHING"
    );
    upsert_id(
        &sql,
        vec![Value::from(&id), Value::Text(code.clone())],
        id,
        "biz_accounting_period",
        "period_code",
        &code,
        &format!("期间 {code}"),
    )
    .await
}

/// 当月天数（闰年 2 月 29）：期间夹具要 `end_date`，不想在 SQL 里做日期算术。
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// 仓库库位（叶子仓下挂一个默认库位）。
pub async fn ensure_location(warehouse_id: Uuid, code: &str) -> Uuid {
    let id = fixture_uuid(&format!("location:{code}"));
    upsert_id(
        "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, is_default, status) \
         VALUES (?, ?, ?, ?, 1, 1) ON CONFLICT DO NOTHING",
        vec![
            Value::from(&id),
            Value::Text(code.to_string()),
            Value::Text(code.to_string()),
            Value::from(&warehouse_id),
        ],
        id,
        "biz_location",
        "location_code",
        code,
        &format!("库位 {code}"),
    )
    .await
}

// ============================================================================
// 四、清场：**每进程一次**（改前：每个 Fixture build 都扫一遍）
// ============================================================================

fn swept() -> &'static Mutex<HashSet<&'static str>> {
    static KEYS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    KEYS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 清掉**上一轮崩溃**留下的夹具行：`key` 相同的调用**全进程只执行一次**。
///
/// 为什么可以「只一次」：这些 `DELETE` 清的是**上一次进程**（跑挂/被杀）留下的行；
/// 本进程内每个夹具的编码都带 [`uniq_suffix`] 后缀、年度由 [`year_seq`] 递增，
/// 天然不撞。改前每个 `Fixture::build()` 都扫一遍（单文件最多 25 次 × 13 条），
/// 那是拿 O(用例数) 条 `DELETE` 换一个只与「上轮是否崩溃」有关的保险。
///
/// `statements` 里**必须只包含自己的前缀**（`LIKE 'CB210-%'` 之类），
/// 绝不能出现按全表扫的删除——那是别人的夹具。
///
/// 返回本次实际执行的语句数（`0` = 本进程已扫过，可直接作为断言/打点依据）。
pub async fn sweep_once(key: &'static str, statements: &[&str]) -> usize {
    {
        let mut keys = swept().lock().expect("sweep 登记表被 poison");
        if !keys.insert(key) {
            return 0;
        }
    }
    let mut done = 0;
    for sql in statements {
        match db().exec_sql((*sql).to_string(), vec![]).await {
            Ok(_) => done += 1,
            // 表不存在 / 列不存在：说明这个模块的迁移没跑，交给用例自己的断言去炸
            Err(err) => println!("sweep 跳过（{key}）：{err}\nSQL: {sql}"),
        }
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 唯一后缀：两次调用必须不同（进程内计数器），且只含数字（能直接进编码列）。
    #[test]
    fn uniq_suffix_is_unique_and_numeric() {
        let a = uniq_suffix();
        let b = uniq_suffix();
        assert_ne!(a, b, "同一进程内两次 uniq_suffix 必须不同");
        assert!(a.chars().all(|ch| ch.is_ascii_digit()), "后缀只含数字：{a}");
        assert!(a.len() <= 12, "后缀长度必须能塞进编码列：{a}");
    }

    /// 编码尾巴：只留数字、取末尾 take 位；`fx_suffix_code` 的老语义（保留 n 字符）也要在。
    #[test]
    fn tail_code_keeps_only_digits_and_respects_limit() {
        assert_eq!(tail_code("12ab34cd5678", 6), "345678");
        assert_eq!(
            tail_code("4101.98765", 8),
            "10198765",
            "带点的科目码先滤掉非数字，再取末尾 8 位"
        );
        assert_eq!(last_chars("12ab34cd5678", 4), "5678");
        assert_eq!(tail_code("", 8), "");
    }

    /// 年度序号：从 base 起递增，绕回 span（每个夹具独占一个年度，不与真实数据撞）。
    ///
    /// ⚠ `SEQ` 是**进程全局**计数器（所有 base 共用，见 `year_seq`），所以这条用例**不许断言
    /// 起始偏移**——它是不是 0 取决于本进程此前调用过几次 `year_seq`。P2-63 实测：单进程全量
    /// 跑（此前已调过奇数次）必红「left: 900001, right: 900000」，而分域 `platform` 跑碰巧绿。
    /// 断言契约本身：相邻两次递增 1、满 span 绕回（与起始偏移无关）。
    #[test]
    fn year_seq_walks_from_base_and_wraps() {
        let base = 900_000; // 远离真实年度，测试内自证
        let span = 2u64;
        let off = |v: i32| (v - base).rem_euclid(span as i32) as u64;
        let a = off(year_seq(base, span));
        let b = off(year_seq(base, span));
        let c = off(year_seq(base, span));
        assert_eq!(b, (a + 1) % span, "每次调用 +1");
        assert_eq!(c, (a + 2) % span, "span 到顶必须绕回（间隔 span 次回到同一位）");
        assert_eq!(c, a, "span=2 时第 3 次必然绕回起始位");
        assert_eq!(period_code(2026, 3), "2026-03");
    }

    /// 派生 id 必须与 `fixture_uuid` 同一口径（同 key 同 id ⇒ 跨进程复用同一行）。
    #[test]
    fn ensure_ids_are_derived_not_random() {
        assert_eq!(
            fixture_uuid("material:CB210-1"),
            fixture_uuid("material:CB210-1"),
            "同一物料编码必须派生同一个 id"
        );
        assert_ne!(
            fixture_uuid("material:CB210-1"),
            fixture_uuid("warehouse:CB210-1"),
            "不同实体的同名编码必须落在不同 id（否则撞主键）"
        );
    }

    /// P2-62 的**耗时证据**（默认 `#[ignore]`，不占日常套件）：
    /// ```bash
    /// ERP_DB_URL=… cargo test --lib test_seed::tests::bench_fixture -- --ignored --nocapture
    /// ```
    ///
    /// 量的是「数据准备」这一段本身的墙钟，**不含任何业务调用**，所以不受用例逻辑干扰：
    ///
    /// | 形态 | 每份夹具的往返 |
    /// |---|---|
    /// | 改前（手写 INSERT） | 单位 `SELECT` → 物料 `INSERT` → 仓库 `INSERT` → 部门 `SELECT` = **4** |
    /// | 改后（`ensure_*`） | 物料 `INSERT` → 仓库 `INSERT` → 部门 `SELECT` = **3**（单位进程内已缓存） |
    ///
    /// 外加改前每个 `Fixture::build()` 还要跑一遍 `sweep_leftovers()`（cost 各模块 12~13 条
    /// `DELETE`），改后是 [`sweep_once`]：**每进程一次**。
    #[ignore = "耗时基准：要真库（ERP_DB_URL），只在 P2-62 取证时手动跑"]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bench_fixture_data_preparation() {
        let Ok(url) = std::env::var("ERP_DB_URL") else {
            println!("SKIP: 未设置 ERP_DB_URL");
            return;
        };
        match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
            Ok(connection) => {
                if set_db(connection).is_err() {
                    println!("SKIP: set_db 失败");
                    return;
                }
            }
            Err(err) => {
                println!("SKIP: 连接失败（{err}）");
                return;
            }
        }

        const N: usize = 200;
        let raw_sql = "INSERT INTO biz_material (id, material_code, material_name, unit_id, \
                       material_type, status, created_at, updated_at) \
                       VALUES (?, ?, ?, ?, 'FINISHED', 1, now(), now())";
        let wh_sql = "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, \
                      warehouse_path, is_leaf, level, status, created_at, updated_at) \
                      VALUES (?, ?, ?, ?, 1, 1, 1, now(), now())";

        // —— 改前形态：4 条往返/份（单位每次现查）——
        let before = std::time::Instant::now();
        for _ in 0..N {
            let suffix = uniq_suffix();
            let unit: Uuid = db()
                .scalar_with_params("SELECT id FROM biz_unit WHERE deleted_at IS NULL LIMIT 1", vec![])
                .await
                .expect("单位");
            let material = Uuid::now_v7();
            db().execute_with_params(
                raw_sql,
                vec![
                    Value::from(&material),
                    Value::Text(format!("BENCH-RAW-{suffix}")),
                    Value::Text("基准物料（改前形态）".to_string()),
                    Value::from(&unit),
                ],
            )
            .await
            .expect("物料");
            let warehouse = Uuid::now_v7();
            let code = format!("BENCHW{}", tail_code(&suffix, 8));
            db().execute_with_params(
                wh_sql,
                vec![
                    Value::from(&warehouse),
                    Value::Text(code.clone()),
                    Value::Text("基准仓（改前形态）".to_string()),
                    Value::Text(format!("/{code}")),
                ],
            )
            .await
            .expect("仓库");
            let _ = ensure_any_dept().await;
        }
        let before = before.elapsed();

        // —— 改后形态：3 条往返/份（单位缓存；插入即知 id，不需回读）——
        let after = std::time::Instant::now();
        for _ in 0..N {
            let suffix = uniq_suffix();
            let _ = ensure_material(&format!("BENCH-NEW-{suffix}"), "基准物料（改后形态）").await;
            let code = format!("BENCHN{}", tail_code(&suffix, 8));
            let _ = ensure_warehouse(&code, "基准仓（改后形态）").await;
            let _ = ensure_any_dept().await;
        }
        let after = after.elapsed();

        println!(
            "\n[P2-62] 数据准备 {N} 份夹具：改前 {:?} → 改后 {:?}（每份 {:.3} ms → {:.3} ms）",
            before,
            after,
            before.as_secs_f64() * 1000.0 / N as f64,
            after.as_secs_f64() * 1000.0 / N as f64
        );
        // 只做量级断言（跨机器波动大）：改后不应比改前慢一倍以上
        assert!(
            after.as_secs_f64() < before.as_secs_f64() * 2.0,
            "改后形态（{after:?}）比改前（{before:?}）慢一倍以上 ⇒ 夹具退化"
        );
    }
}
