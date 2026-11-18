//! 夹具卫生（`#[cfg(test)]`）：`biz_status_log` 的**孤儿行**统一判据与清理。
//!
//! # 为什么需要它
//!
//! `biz_status_log` **没有外键**（它是审计留痕表，刻意不跟单据级联），所以
//! 「删了单据」不会带走它的状态日志。各模块的夹具清场如果只删业务行、不先删日志，
//! 就会在测试库里留下一批 `entity_id` 指向已不存在单据的孤儿行 ——
//! 每跑一遍全量就多攒一批，永远不会被任何模块的「按前缀清场」收走。
//!
//! # 判据为什么是「引用已失效」而不是号段前缀
//!
//! 夹具单号（`AP-P7-*` / `P3B-*`）与本夹具自己造的号容易混，而**生产号段**
//! （`AP-YYYYMMDD-*` / `PZ-YYYYMMDD-*`）恰恰是真实业务入口造的（收货入库 / 付款 / 红冲），
//! 按前缀删会误伤真数据（技能 §6.40：`PM-%` 其实是生产号段，一次误删把真实付款凭证清空）。
//! 孤儿行按定义就是垃圾，所以判据只能是：
//!
//! ```sql
//! entity_type = '<TYPE>' AND entity_id NOT IN (SELECT id FROM <单据表>)
//! ```
//!
//! # 用法
//!
//! ① **根因修复**：各模块的清场函数在删业务行**之前**按 `entity_id` 精确删日志
//! （日志没外键，业务行一删就再也查不到 id 了）；
//! ② **兜底**：模块级 `zz_` teardown 调 [`purge_orphans`] + [`orphans`]，
//! 把本模块或历史上别的模块留下的孤儿一次性收干，并断言为 0。
//!
//! 只清 `biz_status_log`，**不动**任何业务行 —— 宁可少删，不可误删。

#![cfg(test)]

use nik::prelude::*;
use nik::uuid::Uuid;

/// `entity_type` → 单据表（`biz_status_log` 没有外键，只能按「引用是否还活着」判断）。
///
/// 只登记**本卡口径内**的六类：`ACCOUNTING_YEAR` 的 `entity_id` 指向期间行而不是独立表，
/// 与会计期间用例的正常产物混在一起，不在此处清理（见 `year_end` 自己的清场）。
pub(crate) const ENTITY_TABLES: &[(&str, &str)] = &[
    ("STOCK_IN", "biz_stock_in"),
    ("PURCHASE_ORDER", "biz_purchase_order"),
    // P2-5 词表收敛：调拨单的 entity_type 旧值 `TRANSFER` 已统一为 `STOCK_TRANSFER`
    // （与 doc_type / 审批 biz_type 同名），V0219 已改存量。
    ("STOCK_TRANSFER", "biz_stock_transfer"),
    ("PAYABLE", "biz_payable"),
    ("VOUCHER", "biz_voucher"),
    ("ACCOUNTING_PERIOD", "biz_accounting_period"),
    // A2：合同状态流转也写 `biz_status_log`（`entity_type = 'CONTRACT'`），
    // 夹具清场不登记它就会在测试库里攒孤儿行（每跑一次多几条，永远收不走）。
    ("CONTRACT", "biz_contract"),
    // 销售链（M3-05 补登记，M3-08 更正词表）：报价单 `ENTITY_QUOTATION`、销售订单
    // `ENTITY_SALES_ORDER` 都写这张日志表，此前两家都没登记 ⇒ 它们的夹具清场只能
    // 「先删日志再删单据」手工兜着，一旦某个用例漏删就永远收不走。
    //
    // ⚠ 销售订单的落库值是 **`SALE_ORDER`**（`DocKind::SaleOrder.as_str()`），不是 `SALES_ORDER`：
    // M3-05 当初写成 `SALES_ORDER`，于是这条登记从来没匹配到任何一行 —— `orphans()` 恒 0
    // （假绿），真孤儿日志一直攒着。`biz_status_log` 没有外键，写错一个字母就永远收不走。
    ("QUOTATION", "biz_quotation"),
    ("SALE_ORDER", "biz_sales_order"),
];

/// `entity_type` 对应的单据表；未登记的实体直接 panic（别静默跳过 —— 那等于「断言永远绿」）。
fn table_of(entity_type: &str) -> &'static str {
    ENTITY_TABLES
        .iter()
        .find(|(name, _)| *name == entity_type)
        .map(|(_, table)| *table)
        .unwrap_or_else(|| panic!("fixture_hygiene 未登记的单据类型：{entity_type}"))
}

/// 孤儿行判据（`entity_type` 已校验，直接拼进 SQL 是安全的）。
fn orphan_where(entity_type: &str) -> String {
    format!(
        "entity_type = '{entity_type}' AND entity_id NOT IN (SELECT id FROM {})",
        table_of(entity_type)
    )
}

/// 统计这些类型的孤儿行数（`entity_id` 指向已不存在的单据）。
pub(crate) async fn orphans(entity_types: &[&str]) -> i64 {
    let mut total = 0_i64;
    for entity_type in entity_types {
        let sql = format!(
            "SELECT count(*) FROM biz_status_log WHERE {}",
            orphan_where(entity_type)
        );
        let n: i64 = db()
            .scalar_with_params::<Option<i64>>(&sql, vec![])
            .await
            .unwrap_or_else(|err| panic!("统计 {entity_type} 孤儿状态日志失败：{err}"))
            .unwrap_or(0);
        total += n;
    }
    total
}

/// 物理删除这些类型的孤儿行，返回删掉的行数（失败直接 panic —— 不静默吞错）。
pub(crate) async fn purge_orphans(entity_types: &[&str]) -> i64 {
    let mut total = 0_i64;
    for entity_type in entity_types {
        let sql = format!("DELETE FROM biz_status_log WHERE {}", orphan_where(entity_type));
        let deleted = db()
            .execute_with_params(&sql, vec![])
            .await
            .unwrap_or_else(|err| panic!("清理 {entity_type} 孤儿状态日志失败：{err}"));
        total += i64::try_from(deleted).unwrap_or(0);
    }
    total
}

/// 本卡口径：凭证 / 应付两类（`t_7f47221c` 的验收对象）。
pub(crate) const FINANCE: &[&str] = &["PAYABLE", "VOUCHER"];

/// 销售链两类（M3-05 登记，M3-08 更正为落库值）：报价单 + 销售订单。
///
/// 转单会**同时**写两条：`QUOTATION`（ACCEPTED→ACCEPTED + action=CONVERT）与
/// 销售订单自己的建单轨迹（`SALE_ORDER`），所以清孤儿必须两类一起看，
/// 否则转单用例漏删一半就查不出来。
pub(crate) const SALES: &[&str] = &["QUOTATION", "SALE_ORDER"];

/// 「付款 / 应付链路**种子行**」的总互斥锁（`#[cfg(test)]`）。
///
/// # 为什么这两个模块必须共用**同一把**锁
///
/// `payable_payment::tests`（P7/D5）与 `payable_bridge::tests`（P17）写的是同一批
/// **全局行**，其中最关键的是**种子流程定义** `biz_flow_def`：
///
/// * P17 的「显式跳过审批的直付」用例会把 `flow_code = 'payable_pay'` 临时改成
///   `status = 'DISABLED'`，跑完再改回 `PUBLISHED`；
/// * P7 的 `setup()` 反过来会把 `payable_pay` 置回 `PUBLISHED`（自愈上一轮的残留）。
///
/// 两把互不相识的锁时，P7 的 setup 会插在 P17 的「DISABLE → 提交付款」中间把流程**重新发布**，
/// 于是 P17 的 `submit_payment` 拿到 10001「有流程时必须走审批」—— 干净库全量第一轮实测就红
/// （卡 `t_0d7c51f8`：`payable_bridge::tests::zz_teardown_purges_payment_status_logs`，
/// 2323 passed / 1 failed）。夹具没法靠「换个唯一前缀」隔离：流程定义按 `flow_code` 查、
/// 是种子行，全库只有一行。按「同一批全局行 = 同一把锁」（测试卫生铁律）合并锁是唯一正确解。
///
/// 除此之外两者还共用 `biz_payable` / `biz_payment` / `biz_voucher` 与 `biz_seq` 号段，
/// 本来就不该并行。
pub(crate) static FINANCE_SEED_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 「共享种子角色」组锁（`#[cfg(test)]`）：`sys_user_role` 上 `ROLE_PM` / `ROLE_GM`
/// 这两条**共享种子角色**的绑定关系。
///
/// # 为什么写这两个角色绑定的模块必须共用同一把锁
///
/// `PURCHASE_MANAGER` / `GENERAL_MANAGER` 是 V0182 种子里的固定角色，审批流程定义
/// （`po_approve` / `contract_approve`）的审批节点按**角色**展开待办。于是：
///
/// * `purchase_handler::tests::setup_approval()` 会摘掉「不属于自己的」绑定；
/// * `contract_approval_tests::setup()` 过去也做对称的摘取（卡 `t_f404258c` 判定为
///   「抢别人绑定」的越权写，已删除）。
///
/// 两把互不相识的锁时，「谁 setup 谁占位」不成立：一边正在跑审批、另一边 setup 把它的
/// 夹具账号从角色上摘掉 ⇒ 待办展开不到人 / 或者多出别人的账号 ⇒
/// `assert_eq!(tasks.len(), 1)` 随机假红（实测：同一库上两个进程并行，`purchase_handler`
/// 一轮 5 条红、`contract_approval` 反向也会被摘）。改夹具前缀解决不了：角色是种子行。
///
/// 按「同一批全局行 = 同一把锁」把**写这两个角色绑定的模块**并入本锁即可（与
/// [`FINANCE_SEED_LOCK`] 同思路，互不嵌套，不会死锁）。
pub(crate) static SEED_ROLE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 「整库不许留孤儿」的**收敛式**判定：删 → 复查，最多等 `max_wait`，返回最终剩余条数。
///
/// # 为什么不能「purge 一次就断言 0」
///
/// 真库用例是**并行**跑的（libtest 默认 4 个测试线程，`cargo test -j N` 的 `-j` 只是编译
/// 并行度）。别的模块随时可能正处在「单据行已删、它自己的状态日志还没删」的毫秒级中间态里
/// —— 那一瞬间本模块的「整库无孤儿」断言会把**别人的中间态**判成自己的假红。实测（卡
/// `t_0d7c51f8`）：干净隔离库上第一轮 `cargo test`，`payable_bridge::tests::
/// zz_teardown_purges_payment_status_logs` 就红在这一句上，单跑/单模块跑全绿。
///
/// # 口径没有放宽
///
/// 真孤儿（没有任何模块会再回来删）等多久都还在 ⇒ 仍然红。收敛只吸收「别人正在删」的
/// 瞬时窗口，所以断言强度 = 原来的「整库最终无孤儿」，而不是「这一瞬间恰好没有」。
pub(crate) async fn settle_orphans(entity_types: &[&str], max_wait: std::time::Duration) -> i64 {
    let step = std::time::Duration::from_millis(50);
    let mut waited = std::time::Duration::ZERO;
    loop {
        purge_orphans(entity_types).await;
        let left = orphans(entity_types).await;
        if left == 0 || waited >= max_wait {
            return left;
        }
        // 用阻塞 sleep 而不是 `tokio::time`：每个 `#[tokio::test]` 是独立的
        // current-thread runtime，本函数内部没有别的任务需要被调度，阻塞等待最直接。
        std::thread::sleep(step);
        waited += step;
    }
}

/// 已登记类型的全量清单（模块级 teardown 的「整库不许留孤儿」断言用）。
pub(crate) const ALL_TYPES: &[&str] = &[
    "STOCK_IN",
    "PURCHASE_ORDER",
    "STOCK_TRANSFER",
    "PAYABLE",
    "VOUCHER",
    "ACCOUNTING_PERIOD",
    "CONTRACT",
    "QUOTATION",
    "SALE_ORDER",
];

// ============================================================================
// 共享种子角色成员的**借出 / 归还**（`sys_user_role`）—— 卡 `t_18b222b9`
// ============================================================================
//
// # 病根（同一库连跑两轮全量实测）
//
// 审批节点按**角色成员**展开待办，所以「待办派给谁 / 有几条」只有在「角色上只有本模块
// 夹具」时才可判定。历史写法用「按 `role_id` 删掉所有不是我的绑定」制造这个前提，但
// **删完不还**：
//
// ```sql
// DELETE FROM sys_user_role WHERE role_id = '<WAREHOUSE_MANAGER>' AND user_id <> '<夹具>';
// DELETE FROM sys_user_role WHERE role_id IN ('<FINANCE_MANAGER>', '<FINANCE_DIRECTOR>');  -- 整表删
// ```
//
// 演示种子的绑定（`system::setup::model::DEMO_USERS`：`demo_finance` / `demo_finance_mgr` /
// `demo_wh` …）就这样被永久抹掉，后果不是「少一行」而是**滚雪球**：
//
// * 下一个模块的 `test_helpers::ensure_role_member` 见岗位**一个在职成员都没有**就补出
//   `tst02b.<role>` ⇒ `cost_period_approval_db_tests` 的「岗位上只能有夹具 + 演示种子成员」
//   断言在**第二轮**红（第一轮绿 —— 没有上一轮的残留，这正是它只红第二轮的原因）；
// * 角色里塞进别人的夹具账号 ⇒ 会签 / 或签的待办条数与「谁批完节点才推进」一起漂移。
//
// # 判据（只加不减）
//
// 「让角色上只有本模块夹具」本身是**合法的夹具需求**（业务事实需要），越界的只是
// 「永久摘掉别人的绑定」。所以不改需求，改成 **借出 → 归还**：
//
// 1. [`borrow_role_bindings`]：把角色上不属于自己的绑定**逐行登记**进进程内台账，然后只删
//    这些行（自己的夹具照旧绑上，不动）；
// 2. [`restore_role_bindings`]：台账原样挂回（`ON CONFLICT DO NOTHING` 幂等；账号已被
//    对方模块删掉的跳过）。每个借用模块的 `setup()` 起手 + `zz_` 收尾各调一次 ——
//    起手那次是**自愈**（上一条用例 panic 没还的，这里补还）；
// 3. [`restore_and_assert_role_bindings`]：归还后再按 `DEMO_USERS` 逐条核对「演示账号在它
//    该在的岗位上」，漏还一条就当场点名 —— 护栏由断言钉住，不靠人肉 psql 复查。
//
// ⚠️ 写同一批角色绑定的模块共用 [`SEED_ROLE_LOCK`]（`contract` / `purchase` / `cost` 已是）。
// 台账是**进程级**的，`--test-threads=1` 下按模块串行收口；并行跑时「谁是最后一个」
// 不可依赖（与本仓其它 `zz_` 收尾同口径）。

/// 借出台账：`(user_id, role_id)` —— 摘掉了哪些别人的绑定，收尾要原样挂回。
static BORROWED_ROLE_BINDINGS: std::sync::Mutex<Vec<(Uuid, Uuid)>> = std::sync::Mutex::new(Vec::new());

/// 台账句柄（被 panic 毒化时照样拿得到 —— 归还路径必须能跑）。
fn borrowed_slot() -> std::sync::MutexGuard<'static, Vec<(Uuid, Uuid)>> {
    BORROWED_ROLE_BINDINGS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `['a'::uuid, 'b'::uuid]` —— 空集合给 `NULL`。
///
/// ⚠️ 只在**已判空**的地方用：`NOT IN (NULL)` 恒为 NULL（一行都选不到），
/// 所以 `keep` 为空时 [`borrow_role_bindings`] 会整段省掉 `NOT IN (...)`，
/// 不是把 `NULL` 拼进去。
fn uuid_sql_list(ids: &[Uuid]) -> String {
    if ids.is_empty() {
        return "NULL".to_string();
    }
    ids.iter()
        .map(|id| format!("'{id}'::uuid"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// **借出** `roles` 上的共享绑定：`keep` = 本模块夹具账号（留在角色上不动），其余逐行登记后摘掉。
///
/// 返回摘掉的行数（`0` = 角色上本来就只有自己人，没动别人的东西）。
pub(crate) async fn borrow_role_bindings(roles: &[Uuid], keep: &[Uuid]) -> usize {
    if roles.is_empty() {
        return 0;
    }
    let select = if keep.is_empty() {
        // `keep` 为空 = 把角色成员**全部**借出。这里必须整段省掉 `NOT IN (...)`：
        // `user_id NOT IN (NULL)` 在 SQL 三值逻辑里恒为 NULL（一行都选不到），
        // 写成 `NULL` 会让「借出全部」静默变成「什么都没借」（实测：water_check 的
        // 「岗位没人」用例因此拿到 `owner_source = ROLE`）。
        format!(
            "SELECT user_id, role_id FROM sys_user_role \
              WHERE role_id IN ({}) \
              ORDER BY role_id, user_id",
            uuid_sql_list(roles)
        )
    } else {
        format!(
            "SELECT user_id, role_id FROM sys_user_role \
              WHERE role_id IN ({}) AND user_id NOT IN ({}) \
              ORDER BY role_id, user_id",
            uuid_sql_list(roles),
            uuid_sql_list(keep)
        )
    };
    let rows = db()
        .find_with_params(&select, vec![])
        .await
        .unwrap_or_else(|err| panic!("借出共享角色成员失败（读）：{err}\nSQL: {select}"));
    let mut pairs: Vec<(Uuid, Uuid)> = Vec::new();
    for row in &rows {
        let user: Uuid = row
            .get_typed("user_id")
            .unwrap_or_else(|err| panic!("成员 user_id：{err}"));
        let role: Uuid = row
            .get_typed("role_id")
            .unwrap_or_else(|err| panic!("成员 role_id：{err}"));
        if !pairs.contains(&(user, role)) {
            pairs.push((user, role));
        }
    }
    if pairs.is_empty() {
        return 0;
    }
    for (user, role) in &pairs {
        db().execute_with_params(
            "DELETE FROM sys_user_role WHERE user_id = ? AND role_id = ?",
            vec![Value::from(user), Value::from(role)],
        )
        .await
        .unwrap_or_else(|err| panic!("借出共享角色成员失败（摘掉 {user}/{role}）：{err}"));
    }
    let mut slot = borrowed_slot();
    for pair in &pairs {
        if !slot.contains(pair) {
            slot.push(*pair);
        }
    }
    pairs.len()
}

/// **归还**台账里的全部绑定（幂等）。返回真正写回的行数。
///
/// 账号已经被对方模块删掉时跳过（`sys_user` 外键拦着，也本来就没有意义）；已存在则
/// `ON CONFLICT DO NOTHING` 不动。
pub(crate) async fn restore_role_bindings() -> usize {
    let pairs = {
        let mut slot = borrowed_slot();
        std::mem::take(&mut *slot)
    };
    if pairs.is_empty() {
        return 0;
    }
    let mut restored = 0usize;
    let mut skipped = 0usize;
    for (user, role) in pairs {
        let sql = "INSERT INTO sys_user_role (user_id, role_id) \
                   SELECT ?::uuid, ?::uuid \
                    WHERE EXISTS (SELECT 1 FROM sys_user WHERE id = ?::uuid) \
                   ON CONFLICT DO NOTHING";
        match db()
            .execute_with_params(sql, vec![Value::from(&user), Value::from(&role), Value::from(&user)])
            .await
        {
            Ok(n) => {
                restored += usize::try_from(n).unwrap_or(0);
                if n == 0 {
                    skipped += 1;
                }
            }
            Err(err) => panic!("归还共享角色绑定失败（{user}/{role}）：{err}"),
        }
    }
    if skipped > 0 {
        println!("[fixture_hygiene] 归还共享角色绑定：写回 {restored} 行，{skipped} 行跳过（账号已被删 / 绑定已在）");
    }
    restored
}

/// 当前还挂着的借出台账（`zz_` 收尾断言「一条都没漏还」用）。
pub(crate) fn borrowed_role_bindings() -> Vec<(Uuid, Uuid)> {
    borrowed_slot().clone()
}

/// 收尾用：把**本模块自己的夹具账号**从这些角色上摘掉，并从借出台账里划掉（不归还自己）。
///
/// # 为什么自己的夹具也不能留
///
/// 借出/归还是为了「别人的绑定一条不少」。但本模块的临时账号（`p3a_*` / `p16_reviewer` /
/// `p2c1_reviewer` / `m214.*` …）活儿干完之后还占着岗位，会让**下一轮的基线与种子库不同**
/// （岗位成员数从 1 变 5）—— 那正是这类卡反复复发的形态：第一轮绿、第二轮红。
/// 所以收尾顺序固定为「先摘自己的，再归还别人的」。
///
/// 返回摘掉的行数。
pub(crate) async fn release_fixture_role_bindings(roles: &[Uuid], fixture_users: &[Uuid]) -> usize {
    if roles.is_empty() || fixture_users.is_empty() {
        return 0;
    }
    let sql = format!(
        "DELETE FROM sys_user_role WHERE role_id IN ({}) AND user_id IN ({})",
        uuid_sql_list(roles),
        uuid_sql_list(fixture_users)
    );
    let removed = db()
        .execute_with_params(&sql, vec![])
        .await
        .unwrap_or_else(|err| panic!("摘掉本模块夹具角色绑定失败：{err}\nSQL: {sql}"));
    let removed = usize::try_from(removed).unwrap_or(0);
    // 台账里也划掉：这些行是**自己的**，归还等于把它们又绑回去。
    let pairs: Vec<(Uuid, Uuid)> = fixture_users
        .iter()
        .flat_map(|user| roles.iter().map(move |role| (*user, *role)))
        .collect();
    {
        let mut slot = borrowed_slot();
        slot.retain(|pair| !pairs.contains(pair));
    }
    removed
}

/// 演示种子账号在**它该在的岗位上**的绑定，缺了哪些（`"用户名→角色码"`，人读）。
///
/// 判据来自产品种子常量 `DEMO_USERS`（不手抄名单、不按前缀猜）：演示账号**在库且未删**
/// 时，它声明的每个角色都必须有绑定。空库 / 生产快照库没有演示账号 ⇒ 自然跳过（不是本判据
/// 的对象，不假红）。
pub(crate) async fn missing_demo_role_bindings() -> Vec<String> {
    use crate::system::setup::model::DEMO_USERS;

    let mut missing = Vec::new();
    for demo in DEMO_USERS {
        let named = vec![Value::Text(demo.username.to_string())];
        let present: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM sys_user WHERE username = ? AND deleted_at IS NULL",
                named.clone(),
            )
            .await
            .unwrap_or_else(|err| panic!("查演示账号 {} 失败：{err}", demo.username))
            .unwrap_or(0);
        if present == 0 {
            continue;
        }
        for role_code in demo.role_codes {
            let bound: i64 = db()
                .scalar_with_params::<Option<i64>>(
                    "SELECT count(*) FROM sys_user_role ur \
                       JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL \
                       JOIN sys_role r ON r.id = ur.role_id AND r.deleted_at IS NULL \
                      WHERE u.username = ? AND r.role_code = ?",
                    vec![
                        Value::Text(demo.username.to_string()),
                        Value::Text((*role_code).to_string()),
                    ],
                )
                .await
                .unwrap_or_else(|err| panic!("查演示账号 {} 的角色 {role_code} 失败：{err}", demo.username))
                .unwrap_or(0);
            if bound == 0 {
                missing.push(format!("{}→{role_code}", demo.username));
            }
        }
    }
    missing
}

/// 归还 + 断言：`zz_` 收尾的**统一出口**（卡 `t_18b222b9` 的护栏，别绕过它）。
///
/// 三件事一起做，缺一件就等于没有护栏：
/// ① 台账挂回（别人的绑定一条不少地回来）；
/// ② 台账必须清零（漏还 = 本模块的 bug，当场点名）；
/// ③ 演示种子账号在它声明的岗位上必须有绑定（`DEMO_USERS` 逐条对账 —— 这正是第二轮
///    `cost_period_approval_db_tests` / `period::tests` 那 15 条红的直接病根）。
pub(crate) async fn restore_and_assert_role_bindings(stage: &str) -> usize {
    let restored = restore_role_bindings().await;
    let leftover = borrowed_role_bindings();
    assert!(
        leftover.is_empty(),
        "[{stage}] 共享角色借出台账没还清：{leftover:?} —— 归还路径必须覆盖所有借出点"
    );
    let missing = missing_demo_role_bindings().await;
    assert!(
        missing.is_empty(),
        "[{stage}] 演示种子账号的岗位绑定在跑完之后少了 {missing:?}（`system::setup::model::DEMO_USERS`）。\n\
         病根：某个夹具按 `role_id` 整表删了共享种子角色成员却没归还 —— 改成\n\
         `fixture_hygiene::borrow_role_bindings` / `restore_role_bindings` 借出归还，别永久摘掉。\n\
         这条断言就是为了让下一次「第二轮才红」当场在第一轮点名。"
    );
    println!("[fixture_hygiene] {stage}：归还共享角色绑定 {restored} 行，演示种子岗位绑定完好 ✓");
    restored
}
