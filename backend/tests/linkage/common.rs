//! 跨模块联动矩阵测试 · 夹具（`backend/tests/linkage/common.rs`）
//!
//! # 这个目录为什么存在
//!
//! 仓里已有 1300+ 个后端测试，但它们几乎都在**单模块内**：`task` 测任务状态机、
//! `flow` 测审批引擎、`erp` 测各单据的状态机。于是「任务取消后审批待办没撤」
//! （TSK-202610-000149）这种**跨模块**缺陷没有任何用例覆盖。
//!
//! 本目录按「状态变更 × 关联物」矩阵补这一类用例：一条状态变更发生时，
//! **所有引用它的异物**（审批实例 / 待办 / 节点 / 库存 / 库位账 / 流水 / 凭证 /
//! 应付 / 序列号）必须同步到该有的样子，或者明确地「不动」。
//!
//! # 数据库（红线 3：只碰 `cloudpivot_test`）
//!
//! | 环境变量 | 作用 |
//! |---|---|
//! | `LINKAGE_DB_URL` | 真库连接串，**必须**指向 `cloudpivot_test`（本文件里有硬校验，指错直接 panic） |
//! | `LINKAGE_REQUIRE_DB=1` | 断言「真的连了库」：没配 URL 时**失败**而不是 SKIP（交付验收用） |
//!
//! 缺 URL 时按仓内既有约定打印 `SKIP` 并让用例返回（`cargo test` 在无库 CI 上仍绿）。
//!
//! # 夹具策略：每个用例一套自己的主数据
//!
//! 调用 [`Ctx::new`] 会在**本次用例专属的 UUID / 编码前缀**下建好：角色、三个用户
//! （创建人 / 负责人 / 无关用户）、单位、供应商、两个仓库、两种物料（普通 + 一物一码）、
//! 一个库位。用例结束 [`Ctx::cleanup`] 按这些 id 精确删净。
//!
//! 由此得到三条保证：
//!
//! 1. **不依赖别的用例留下的数据**（主数据也是自己造的，不是共享种子）；
//! 2. **不污染别的用例**（断言一律按本用例的 id / 前缀过滤，不做全局 `COUNT(*)`）；
//! 3. 与并发跑的其他卡（P9/P10/P11 都用同一个 `cloudpivot_test`）互不干扰。
//!
//! 唯一的共享种子是 V0127 的 `TASK_ACCEPT` 流程定义与 V0139 的在途仓 —— 它们是
//! **被系统读取的配置**，不属于「夹具数据」，本文件只读不写（T17 例外，见该用例）。

use std::str::FromStr;

use axum::Extension;
use nik::prelude::*;
use nik::uuid::Uuid;
use rust_decimal::Decimal;

/// 本次运行的用例前缀根（所有夹具编码都以它开头）。
pub const ROOT: &str = "LNK";

// ==================== 连库 ====================

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `LINKAGE_DB_URL`。
///
/// `db()` 是进程级全局池，而每个 `#[tokio::test]` 都是**独立 runtime**；空闲连接
/// 绑在创建它的 runtime 上，runtime 一销毁就死。故把 `max_lifetime` / `idle_timeout`
/// 置零，逼池子每次借出都新建连接（W1b/P2-B1 已实测的做法，不改）。
pub async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("LINKAGE_DB_URL") else {
                if std::env::var("LINKAGE_REQUIRE_DB").as_deref() == Ok("1") {
                    panic!(
                        "LINKAGE_REQUIRE_DB=1（要求必须连真库），但未设置 LINKAGE_DB_URL。\n\
                         正确用法：LINKAGE_DB_URL='postgres://lnkrole:...@127.0.0.1:5432/cloudpivot_test' \\\n\
                         LINKAGE_REQUIRE_DB=1 cargo test --test linkage -- --test-threads=1"
                    );
                }
                println!("SKIP: 未设置 LINKAGE_DB_URL（联动矩阵测试需要真库，不连则不跑）");
                return false;
            };
            // 红线 3（绝不碰 prod）：库名必须是 cloudpivot_test，指错直接炸，不做「尽力而为」。
            if !url.contains("/cloudpivot_test") {
                panic!(
                    "LINKAGE_DB_URL 必须指向 cloudpivot_test 库（当前：{url}）。\
                     联动测试会造库存/凭证/应付，指到别的库会毁掉真实账目。"
                );
            }
            if try_db().is_some() {
                return true;
            }
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.max_lifetime = Some(std::time::Duration::ZERO);
            config.pool.idle_timeout = Some(std::time::Duration::ZERO);
            match Db::connect_with(&url, config).await {
                Ok(connection) => match set_db(connection) {
                    Ok(()) => {
                        println!("联动矩阵测试已连接 cloudpivot_test");
                        true
                    }
                    Err(err) => panic!("set_db 失败：{err}"),
                },
                Err(err) => panic!("LINKAGE_DB_URL 连接失败：{err}"),
            }
        })
        .await
}

/// 取库 + 用例前置：连不上时返回 `None`，用例直接 `return`（与仓内既有约定一致）。
pub async fn db_or_skip() -> Option<()> {
    if ensure_db().await { Some(()) } else { None }
}

/// 进程级串行闸 —— 每个用例开头 `let _gate = common::serial().await;`。
///
/// # 为什么需要它
///
/// 用例本身是互相独立的（各自一套夹具、各自前缀），但 **T17 会临时把 `TASK_ACCEPT`
/// 的种子流程置为 `DRAFT`**（模拟「没配流程」），这是**全局状态**。如果这时另一个 T 用例
/// 正好提交验收，它会读到「没配流程」→ 走两级验收 → 用例假红。
///
/// 卡面写的验收命令是 `cargo test --test linkage`（libtest 默认多线程），所以不能靠
/// 「请用 `--test-threads=1`」这种口头约定兜底 —— 直接让所有用例先过这道闸：
/// 串行跑时零开销（无竞争），并发跑时等价于 `--test-threads=1`，命令怎么写都不会漂。
pub async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    GATE.lock().await
}

// ==================== 身份 ====================

/// 造一份 `AuthClaims`（HTTP 层同款入参）。
///
/// 角色列表只影响 `has_permission` 类判断；数据范围走 `principal::resolve_for_claims`
/// 查库（夹具用户都挂了 `data_scope = 4`「仅本人」的角色，所以看得见自己的任务）。
pub fn claims(user: Uuid, name: &str) -> Extension<AuthClaims> {
    Extension(AuthClaims(TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some(name.to_string()),
        roles: vec!["LNK_FIXTURE".to_string()],
        permissions: Vec::new(),
        token_type: Some(TokenType::Access),
        exp: 4_000_000_000,
        iat: 1_700_000_000,
        extra: std::collections::HashMap::new(),
    }))
}

// ==================== 十进制 ====================

/// 金额 / 数量字面量 → `Decimal`（测试里写 `dec("12.34")` 比 `Decimal::new` 可读）。
pub fn dec(text: &str) -> Decimal {
    Decimal::from_str(text)
        .unwrap_or_else(|err| panic!("测试字面量 {text} 不是合法 Decimal：{err}"))
}

// ==================== 夹具 ====================

/// 一次用例的全部夹具（主数据 + 三个用户）。
#[derive(Debug, Clone)]
pub struct Ctx {
    /// 用例短名（`T01` / `S05` …），只用于编码与日志。
    pub tag: String,
    /// 本用例专属前缀（`LNK-T01-ab12cd34`）：所有夹具编码都以它开头。
    pub prefix: String,
    /// 任务创建人 / 审批人。
    pub creator: Uuid,
    /// 任务负责人 / 提交验收的人。
    pub worker: Uuid,
    /// 与任务无关的用户（多用户视角用例）。
    pub outsider: Uuid,
    /// 夹具角色（`data_scope = 4` 仅本人）。
    pub role: Uuid,
    /// 单位。
    pub unit: Uuid,
    /// 供应商。
    pub supplier: Uuid,
    /// 调出仓（收货也收这里）。
    pub wh_from: Uuid,
    /// 调入仓。
    pub wh_to: Uuid,
    /// 普通物料（非一物一码）。
    pub mat: Uuid,
    /// 一物一码物料。
    pub mat_sn: Uuid,
    /// 库位字典（挂在 `wh_from` 下）。
    pub loc: Uuid,
}

impl Ctx {
    /// 建一套用例专属夹具。`tag` 只影响可读性（编码里带上它，日志一眼能对上是哪个用例）。
    pub async fn new(tag: &str) -> Self {
        // 取 **UUIDv7 的随机段**（末 8 位）而不是前 8 位：前 8 位是时间戳高段，
        // 同一毫秒内建两个夹具前缀会完全相同 —— 实测导致
        // `uk_sys_role_code` 唯一冲突（上一次 panic 留下的角色按 role_code 撞上）。
        let raw = Uuid::now_v7().simple().to_string();
        let suffix = raw[raw.len() - 8..].to_string();
        let prefix = format!("{ROOT}-{tag}-{suffix}");
        let ctx = Self {
            tag: tag.to_string(),
            prefix,
            creator: Uuid::now_v7(),
            worker: Uuid::now_v7(),
            outsider: Uuid::now_v7(),
            role: Uuid::now_v7(),
            unit: Uuid::now_v7(),
            supplier: Uuid::now_v7(),
            wh_from: Uuid::now_v7(),
            wh_to: Uuid::now_v7(),
            mat: Uuid::now_v7(),
            mat_sn: Uuid::now_v7(),
            loc: Uuid::now_v7(),
        };
        ctx.seed().await;
        ctx
    }

    /// 写夹具（角色 → 用户 → 用户角色 → 单位 → 供应商 → 仓库 → 物料 → 库位）。
    async fn seed(&self) {
        let p = &self.prefix;
        // 先清一次：同 tag 的残留（上次用例 panic 未清理）不该影响本次
        self.cleanup().await;

        let statements: Vec<(String, Vec<Value>)> = vec![
            (
                "INSERT INTO sys_role (id, role_code, role_name, data_scope, status) \
                 VALUES (?::uuid, ?, ?, 4, 1)".to_string(),
                vec![
                    Value::from(&self.role),
                    Value::Text(format!("{p}-ROLE")),
                    Value::Text(format!("{} 夹具角色（仅本人）", self.tag)),
                ],
            ),
            (
                "INSERT INTO sys_user (id, username, password, real_name, status, user_type) \
                 VALUES (?::uuid, ?, 'fixture-not-a-real-hash', ?, 1, 'HUMAN'), \
                        (?::uuid, ?, 'fixture-not-a-real-hash', ?, 1, 'HUMAN'), \
                        (?::uuid, ?, 'fixture-not-a-real-hash', ?, 1, 'HUMAN')".to_string(),
                vec![
                    Value::from(&self.creator),
                    Value::Text(format!("{p}-creator")),
                    Value::Text(format!("{} 创建人", self.tag)),
                    Value::from(&self.worker),
                    Value::Text(format!("{p}-worker")),
                    Value::Text(format!("{} 负责人", self.tag)),
                    Value::from(&self.outsider),
                    Value::Text(format!("{p}-outsider")),
                    Value::Text(format!("{} 无关用户", self.tag)),
                ],
            ),
            (
                "INSERT INTO sys_user_role (id, user_id, role_id) \
                 VALUES (gen_random_uuid(), ?::uuid, ?::uuid), \
                        (gen_random_uuid(), ?::uuid, ?::uuid), \
                        (gen_random_uuid(), ?::uuid, ?::uuid)".to_string(),
                vec![
                    Value::from(&self.creator),
                    Value::from(&self.role),
                    Value::from(&self.worker),
                    Value::from(&self.role),
                    Value::from(&self.outsider),
                    Value::from(&self.role),
                ],
            ),
            (
                "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) \
                 VALUES (?::uuid, ?, '个', 1, NULL, 1.000000, 1, 1)".to_string(),
                vec![Value::from(&self.unit), Value::Text(format!("{p}-PCS"))],
            ),
            (
                "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) \
                 VALUES (?::uuid, ?, ?, 1)".to_string(),
                vec![
                    Value::from(&self.supplier),
                    Value::Text(format!("{p}-SUP")),
                    Value::Text(format!("{} 供应商", self.tag)),
                ],
            ),
            (
                "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status) \
                 VALUES (?::uuid, ?, ?, ?, 1, 1, 1), (?::uuid, ?, ?, ?, 1, 1, 1)".to_string(),
                vec![
                    Value::from(&self.wh_from),
                    Value::Text(format!("{p}-WFA")),
                    Value::Text(format!("{} 调出仓", self.tag)),
                    Value::Text(format!("/{}/", self.wh_from)),
                    Value::from(&self.wh_to),
                    Value::Text(format!("{p}-WTO")),
                    Value::Text(format!("{} 调入仓", self.tag)),
                    Value::Text(format!("/{}/", self.wh_to)),
                ],
            ),
            (
                "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required) \
                 VALUES (?::uuid, ?, ?, ?::uuid, 1, false), (?::uuid, ?, ?, ?::uuid, 1, true)".to_string(),
                vec![
                    Value::from(&self.mat),
                    Value::Text(format!("{p}-M1")),
                    Value::Text(format!("{} 螺丝", self.tag)),
                    Value::from(&self.unit),
                    Value::from(&self.mat_sn),
                    Value::Text(format!("{p}-M2")),
                    Value::Text(format!("{} 电机（一物一码）", self.tag)),
                    Value::from(&self.unit),
                ],
            ),
            (
                "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type, \
                                           is_default, capacity, status, sort_order) \
                 VALUES (?::uuid, ?, ?, ?::uuid, 'STORAGE', 0, NULL, 1, 1)".to_string(),
                vec![
                    Value::from(&self.loc),
                    Value::Text(format!("{p}-L01")),
                    Value::Text(format!("{} A 区", self.tag)),
                    Value::from(&self.wh_from),
                ],
            ),
        ];
        for (sql, params) in statements {
            if let Err(error) = db().execute_with_params(&sql, params).await {
                panic!("夹具写入失败（{}）：{error}\nSQL: {sql}", self.tag);
            }
        }
    }

    /// 按本用例的 id / 前缀删净所有数据（外键反序）。
    ///
    /// 全部按 id 精确命中：不用 `LIKE '%'`、不用全局 `COUNT(*)`，因此并发跑别的卡也不受影响。
    pub async fn cleanup(&self) {
        let so = format!(
            "(SELECT id FROM biz_stock_in WHERE supplier_id = '{}'::uuid)",
            self.supplier
        );
        let tk = format!(
            "(SELECT id FROM biz_stock_transfer WHERE from_warehouse_id IN \
             ('{}'::uuid, '{}'::uuid) OR to_warehouse_id IN ('{}'::uuid, '{}'::uuid))",
            self.wh_from, self.wh_to, self.wh_from, self.wh_to
        );
        let vo = format!(
            "(SELECT v.id FROM biz_voucher v WHERE v.source_id IN {so}) UNION \
             (SELECT v.id FROM biz_voucher v WHERE v.source_id IN {tk})"
        );
        let statements = [
            // ---- 审批（任务 / 调拨单挂的实例）----
            format!(
                "DELETE FROM biz_flow_task WHERE instance_id IN \
                 (SELECT id FROM biz_flow_instance WHERE biz_id IN \
                  (SELECT id FROM biz_task WHERE creator_id IN \
                   ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid)) \
                  OR biz_id IN {tk})",
                c = self.creator,
                w = self.worker,
                o = self.outsider
            ),
            format!(
                "DELETE FROM biz_flow_instance_node WHERE instance_id IN \
                 (SELECT id FROM biz_flow_instance WHERE biz_id IN \
                  (SELECT id FROM biz_task WHERE creator_id IN \
                   ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid)) \
                  OR biz_id IN {tk})",
                c = self.creator,
                w = self.worker,
                o = self.outsider
            ),
            format!(
                "DELETE FROM biz_flow_cc WHERE instance_id IN \
                 (SELECT id FROM biz_flow_instance WHERE biz_id IN \
                  (SELECT id FROM biz_task WHERE creator_id IN \
                   ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid)) \
                  OR biz_id IN {tk})",
                c = self.creator,
                w = self.worker,
                o = self.outsider
            ),
            format!(
                "DELETE FROM biz_flow_instance WHERE biz_id IN \
                 (SELECT id FROM biz_task WHERE creator_id IN \
                  ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid)) \
                 OR biz_id IN {tk} \
                 OR initiator_id IN ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid)",
                c = self.creator,
                w = self.worker,
                o = self.outsider
            ),
            // ---- 任务侧 ----
            format!(
                "DELETE FROM biz_task_comment WHERE task_id IN \
                 (SELECT id FROM biz_task WHERE creator_id IN ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid))",
                c = self.creator,
                w = self.worker,
                o = self.outsider
            ),
            format!(
                "DELETE FROM biz_task_member WHERE task_id IN \
                 (SELECT id FROM biz_task WHERE creator_id IN ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid))",
                c = self.creator,
                w = self.worker,
                o = self.outsider
            ),
            format!(
                "DELETE FROM biz_task_dependency WHERE task_id IN \
                 (SELECT id FROM biz_task WHERE creator_id IN ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid)) \
                 OR depends_on_id IN \
                 (SELECT id FROM biz_task WHERE creator_id IN ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid))",
                c = self.creator,
                w = self.worker,
                o = self.outsider
            ),
            // 状态日志无外键：按任务 id / 单号前缀清
            format!(
                "DELETE FROM biz_status_log WHERE entity_id IN \
                 (SELECT id FROM biz_task WHERE creator_id IN ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid)) \
                 OR entity_no LIKE '{p}-%'",
                c = self.creator,
                w = self.worker,
                o = self.outsider,
                p = self.prefix
            ),
            format!(
                "UPDATE biz_task SET parent_id = NULL WHERE creator_id IN \
                 ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid)",
                c = self.creator,
                w = self.worker,
                o = self.outsider
            ),
            format!(
                "DELETE FROM biz_task WHERE creator_id IN ('{c}'::uuid, '{w}'::uuid, '{o}'::uuid)",
                c = self.creator,
                w = self.worker,
                o = self.outsider
            ),
            // ---- ERP 单据 ----
            format!(
                "DELETE FROM biz_payment WHERE payable_id IN (SELECT id FROM biz_payable WHERE source_id IN {so})"
            ),
            format!("DELETE FROM biz_payable WHERE source_id IN {so}"),
            format!("DELETE FROM biz_voucher_entry WHERE voucher_id IN ({vo})"),
            format!("DELETE FROM biz_voucher WHERE id IN ({vo})"),
            // 序列号子表的主键是 (item_id, serial_id) —— 没有 transfer_id 列，
            // 必须经明细表 item_id 反查（P5 起的表结构）。
            //
            // ★ 顺序坑：它引用 biz_material_serial，必须**先**删它再删序列号。
            //   写反了会让「删序列号」撞外键 → 连带 biz_material / biz_warehouse 也删不掉，
            //   最后只剩 WARN（默认被 cargo 的 stdout 捕获吞掉），表面全绿实际留一套夹具（实测踩过）。
            format!(
                "DELETE FROM biz_stock_transfer_item_serial WHERE item_id IN \
                 (SELECT id FROM biz_stock_transfer_item WHERE transfer_id IN {tk})"
            ),
            // 序列号：本单产生的 + 夹具物料名下的（调拨用例直接在夹具里造 IN_STOCK 序列号，
            // 它们没有 source_id；不一起删，后面删 biz_material 会撞外键）。
            format!(
                "DELETE FROM biz_material_serial WHERE source_id IN {so} OR source_id IN {tk} \
                 OR material_id IN ('{}'::uuid, '{}'::uuid)",
                self.mat, self.mat_sn
            ),
            format!(
                "DELETE FROM biz_inventory_txn WHERE material_id IN ('{}'::uuid, '{}'::uuid)",
                self.mat, self.mat_sn
            ),
            // 库位账的 location_id 指 biz_inventory，必须后删
            format!(
                "DELETE FROM biz_inventory WHERE material_id IN ('{}'::uuid, '{}'::uuid)",
                self.mat, self.mat_sn
            ),
            format!(
                "DELETE FROM biz_inventory_location WHERE material_id IN ('{}'::uuid, '{}'::uuid)",
                self.mat, self.mat_sn
            ),
            format!("DELETE FROM biz_stock_transfer_item WHERE transfer_id IN {tk}"),
            format!("DELETE FROM biz_stock_transfer WHERE id IN {tk}"),
            format!("DELETE FROM biz_stock_in_item WHERE stock_in_id IN {so}"),
            format!(
                "DELETE FROM biz_stock_in WHERE supplier_id = '{}'::uuid",
                self.supplier
            ),
            // ---- 主数据 ----
            format!("DELETE FROM biz_location WHERE id = '{}'::uuid", self.loc),
            format!(
                "DELETE FROM biz_material WHERE id IN ('{}'::uuid, '{}'::uuid)",
                self.mat, self.mat_sn
            ),
            format!(
                "DELETE FROM biz_warehouse WHERE id IN ('{}'::uuid, '{}'::uuid)",
                self.wh_from, self.wh_to
            ),
            format!(
                "DELETE FROM biz_supplier WHERE id = '{}'::uuid",
                self.supplier
            ),
            format!("DELETE FROM biz_unit WHERE id = '{}'::uuid", self.unit),
            // ---- 用户 ----
            format!(
                "DELETE FROM sys_user_role WHERE user_id IN ('{}'::uuid, '{}'::uuid, '{}'::uuid)",
                self.creator, self.worker, self.outsider
            ),
            // 前缀兜底：上一次 panicked 的运行（id 已丢）留下的夹具，按编码前缀清掉。
            // 只认本用例前缀，不会碰到别人的数据。
            format!(
                "DELETE FROM sys_user_role WHERE user_id IN \
                 (SELECT id FROM sys_user WHERE username LIKE '{p}-%')",
                p = self.prefix
            ),
            format!(
                "DELETE FROM sys_user WHERE username LIKE '{p}-%'",
                p = self.prefix
            ),
            format!(
                "DELETE FROM sys_role WHERE role_code LIKE '{p}-%'",
                p = self.prefix
            ),
            format!(
                "DELETE FROM sys_user WHERE id IN ('{}'::uuid, '{}'::uuid, '{}'::uuid)",
                self.creator, self.worker, self.outsider
            ),
            format!("DELETE FROM sys_role WHERE id = '{}'::uuid", self.role),
        ];
        let mut failures: Vec<String> = Vec::new();
        for sql in statements {
            if let Err(error) = db().execute_with_params(&sql, vec![]).await {
                // 单条失败继续往下删（一条脏数据不该让整批停摆），但记下来
                println!("WARN: 清理失败（{}）：{error}\nSQL: {sql}", self.tag);
                failures.push(format!("{error}\nSQL: {sql}"));
            }
        }
        // ★ 收尾自检：不许「静默留夹具」。
        //
        // 为什么必须显式查：`println!` / `eprintln!` 都会被 libtest 捕获，**全绿的用例看不到**
        // —— 实测 F11 就因为「删序列号早于删关联表」撞外键，连带物料/仓库删不掉，
        // 表面 60 passed 而库里真留了一套夹具。这里把它变成硬失败：
        // 清理不干净 = 测试基建缺陷，必须当场炸，而不是留到别人踩。
        let residue = self.residue().await;
        if !failures.is_empty() || !residue.is_empty() {
            panic!(
                "夹具清理失败（{}，prefix {}）：{} 条 SQL 报错，残留 {residue:?}。\n\
                 这是**测试基建**问题（外键顺序 / 遗漏表），不是业务断言失败；\n\
                 收尾工具见 backend/tests/linkage/cleanup_leftovers.sql",
                self.tag,
                self.prefix,
                failures.len()
            );
        }
    }

    /// 本用例夹具在库里的残留（只数本用例 id / 前缀，不做全局 `COUNT(*)`）。
    ///
    /// 空 = 清理干净。任何一项非 0 都说明 `cleanup()` 的语句顺序或覆盖面漏了。
    async fn residue(&self) -> Vec<(String, i64)> {
        // 谓词与 `cleanup()` 里的删除条件一一对应（否则「删得掉但查不出来」= 假阴性）。
        let so = format!(
            "(SELECT id FROM biz_stock_in WHERE supplier_id = '{}'::uuid)",
            self.supplier
        );
        let tk = format!(
            "(SELECT id FROM biz_stock_transfer WHERE from_warehouse_id IN ('{f}'::uuid,'{t}'::uuid) \
              OR to_warehouse_id IN ('{f}'::uuid,'{t}'::uuid))",
            f = self.wh_from,
            t = self.wh_to
        );
        let sql = format!(
            "SELECT 'user' AS t, count(*) AS n FROM sys_user WHERE username LIKE '{p}-%' OR id IN ('{c}'::uuid,'{w}'::uuid,'{o}'::uuid)
             UNION ALL SELECT 'role', count(*) FROM sys_role WHERE role_code LIKE '{p}-%' OR id = '{r}'::uuid
             UNION ALL SELECT 'task', count(*) FROM biz_task WHERE creator_id IN ('{c}'::uuid,'{w}'::uuid,'{o}'::uuid)
             UNION ALL SELECT 'task_child', count(*) FROM biz_task_comment WHERE task_id IN (SELECT id FROM biz_task WHERE creator_id IN ('{c}'::uuid,'{w}'::uuid,'{o}'::uuid))
             UNION ALL SELECT 'flow_instance', count(*) FROM biz_flow_instance WHERE biz_id IN (SELECT id FROM biz_task WHERE creator_id IN ('{c}'::uuid,'{w}'::uuid,'{o}'::uuid)) OR biz_id IN {tk} OR initiator_id IN ('{c}'::uuid,'{w}'::uuid,'{o}'::uuid)
             UNION ALL SELECT 'flow_task', count(*) FROM biz_flow_task WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_id IN (SELECT id FROM biz_task WHERE creator_id IN ('{c}'::uuid,'{w}'::uuid,'{o}'::uuid)) OR biz_id IN {tk})
             UNION ALL SELECT 'stock_in', count(*) FROM biz_stock_in WHERE supplier_id = '{s}'::uuid
             UNION ALL SELECT 'transfer', count(*) FROM biz_stock_transfer WHERE id IN {tk}
             UNION ALL SELECT 'voucher', count(*) FROM biz_voucher WHERE source_id IN {so} OR source_id IN {tk}
             UNION ALL SELECT 'voucher_entry', count(*) FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_id IN {so} OR source_id IN {tk})
             UNION ALL SELECT 'payable', count(*) FROM biz_payable WHERE source_id IN {so}
             UNION ALL SELECT 'payment', count(*) FROM biz_payment WHERE payable_id IN (SELECT id FROM biz_payable WHERE source_id IN {so})
             UNION ALL SELECT 'inventory', count(*) FROM biz_inventory WHERE material_id IN ('{m}'::uuid,'{mn}'::uuid)
             UNION ALL SELECT 'inventory_location', count(*) FROM biz_inventory_location WHERE material_id IN ('{m}'::uuid,'{mn}'::uuid)
             UNION ALL SELECT 'inventory_txn', count(*) FROM biz_inventory_txn WHERE material_id IN ('{m}'::uuid,'{mn}'::uuid)
             UNION ALL SELECT 'serial', count(*) FROM biz_material_serial WHERE serial_no LIKE '{p}-%' OR material_id IN ('{m}'::uuid,'{mn}'::uuid)
             UNION ALL SELECT 'material', count(*) FROM biz_material WHERE material_code LIKE '{p}-%' OR id IN ('{m}'::uuid,'{mn}'::uuid)
             UNION ALL SELECT 'warehouse', count(*) FROM biz_warehouse WHERE warehouse_code LIKE '{p}-%' OR id IN ('{wf}'::uuid,'{wt}'::uuid)
             UNION ALL SELECT 'supplier', count(*) FROM biz_supplier WHERE supplier_code LIKE '{p}-%' OR id = '{s}'::uuid
             UNION ALL SELECT 'location', count(*) FROM biz_location WHERE location_code LIKE '{p}-%' OR id = '{l}'::uuid
             UNION ALL SELECT 'unit', count(*) FROM biz_unit WHERE unit_code LIKE '{p}-%' OR id = '{u}'::uuid
             UNION ALL SELECT 'status_log', count(*) FROM biz_status_log WHERE entity_no LIKE '{p}-%'",
            p = self.prefix,
            c = self.creator,
            w = self.worker,
            o = self.outsider,
            r = self.role,
            s = self.supplier,
            u = self.unit,
            m = self.mat,
            mn = self.mat_sn,
            wf = self.wh_from,
            wt = self.wh_to,
            l = self.loc
        );
        let rows = db()
            .find_with_params(&sql, vec![])
            .await
            .unwrap_or_else(|error| panic!("{}：残留自检查询失败：{error}", self.tag));
        rows.iter()
            .filter_map(|row| {
                let n = row.get_typed::<i64>("n").unwrap_or(-1);
                let t = row.get_typed::<String>("t").unwrap_or_default();
                (n != 0).then_some((t, n))
            })
            .collect()
    }

    /// 编码：`LNK-T01-ab12cd34-SUP` 这类（便于在库里按前缀定位夹具）。
    pub fn code(&self, tail: &str) -> String {
        format!("{}-{tail}", self.prefix)
    }

    /// 造一行**库位库存行**（`biz_inventory_location`），返回它的 `id`。
    ///
    /// # 为什么需要它（实测踩过）
    ///
    /// 收货明细的 `items[].location_id` 要的是 `biz_inventory_location.id`（库位库存行），
    /// **不是**库位字典 `biz_location.id`。传字典 id 会得到
    /// `66002 items[0].location_id 库位库存行不存在`。
    /// 而库位库存行本身要挂在某个字典库位上（`location_id` = `self.loc`），
    /// 所以夹具按「物料 × 仓 × 字典库位」建行，数量 0，交给过账去加。
    pub async fn loc_row(&self, material: Uuid) -> Uuid {
        // 幂等：`uk_biz_inv_loc (material_id, warehouse_id, location_id)` 唯一，
        // 同一物料同一库位只能有一行（实测：第二次盲插直接 unique violation）。
        if let Some(existing) = scalar_text(
            "SELECT id::text AS v FROM biz_inventory_location \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id = ?::uuid",
            vec![
                Value::from(&material),
                Value::from(&self.wh_from),
                Value::from(&self.loc),
            ],
        )
        .await
        {
            return Uuid::parse_str(&existing)
                .unwrap_or_else(|error| panic!("{}：库位库存行 id 解析失败：{error}", self.tag));
        }
        let id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty) \
             VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, 0)",
            vec![
                Value::from(&id),
                Value::from(&material),
                Value::from(&self.wh_from),
                Value::from(&self.loc),
            ],
        )
        .await
        .unwrap_or_else(|error| panic!("{}：建库位库存行失败：{error}", self.tag));
        id
    }
}

// ==================== 读：审批实例 / 待办 ====================

/// 任务名下最近一条审批实例：`(id, status)`。
pub async fn flow_instance_of(task_id: Uuid) -> Option<(Uuid, String)> {
    let rows = db()
        .find_with_params(
            "SELECT id, status FROM biz_flow_instance \
              WHERE biz_type = 'TASK_ACCEPT' AND biz_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY started_at DESC LIMIT 1",
            vec![Value::from(&task_id)],
        )
        .await
        .unwrap_or_else(|err| panic!("查审批实例失败：{err}"));
    rows.first().map(|row| {
        (
            row.get_typed::<Uuid>("id").expect("instance id"),
            row.get_typed::<String>("status").expect("instance status"),
        )
    })
}

/// 单据号（`biz_no`）名下最近一条实例 —— 调拨单走这条（`biz_id` 也指单据 id，但用单号更好读）。
///
/// 卡面要求的「按单号查实例」入口；当前用例走的是 [`flow_instance_of`]（按 `biz_id`），
/// 保留本函数给后续按单号断言的用例，故显式豁免 dead_code 而非删掉。
#[allow(dead_code)]
pub async fn flow_instance_of_no(biz_type: &str, biz_no: &str) -> Option<(Uuid, String)> {
    let rows = db()
        .find_with_params(
            "SELECT id, status FROM biz_flow_instance \
              WHERE biz_type = ? AND biz_no = ? AND deleted_at IS NULL \
              ORDER BY started_at DESC LIMIT 1",
            vec![
                Value::Text(biz_type.to_string()),
                Value::Text(biz_no.to_string()),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("查审批实例失败：{err}"));
    rows.first().map(|row| {
        (
            row.get_typed::<Uuid>("id").expect("instance id"),
            row.get_typed::<String>("status").expect("instance status"),
        )
    })
}

/// 实例下的待办：`(id, status, completed_at_is_null)`。
pub async fn flow_tasks_of(instance_id: Uuid) -> Vec<(Uuid, String, bool)> {
    let rows = db()
        .find_with_params(
            "SELECT id, status, completed_at FROM biz_flow_task \
              WHERE instance_id = ?::uuid ORDER BY created_at",
            vec![Value::from(&instance_id)],
        )
        .await
        .unwrap_or_else(|err| panic!("查待办失败：{err}"));
    rows.iter()
        .map(|row| {
            (
                row.get_typed::<Uuid>("id").expect("task id"),
                row.get_typed::<String>("status").expect("task status"),
                row.get_typed::<Option<chrono::DateTime<chrono::Utc>>>("completed_at")
                    .expect("completed_at")
                    .is_none(),
            )
        })
        .collect()
}

/// 实例下的节点：`(node_key, status)`。
pub async fn flow_nodes_of(instance_id: Uuid) -> Vec<(String, String)> {
    let rows = db()
        .find_with_params(
            "SELECT node_key, status FROM biz_flow_instance_node \
              WHERE instance_id = ?::uuid ORDER BY created_at",
            vec![Value::from(&instance_id)],
        )
        .await
        .unwrap_or_else(|err| panic!("查实例节点失败：{err}"));
    rows.iter()
        .map(|row| {
            (
                row.get_typed::<String>("node_key").expect("node_key"),
                row.get_typed::<String>("status").expect("node status"),
            )
        })
        .collect()
}

/// 某人在「我的待办」里能看到的本用例实例（按实例单号过滤，不做全局计数）。
///
/// 与生产接口 `flow::handler::my_todo` **同参数**：`live_only = true`
/// （P11：待办只认实例还在跑的单子，历史脏数据的 PENDING 待办不进列表）。
pub async fn todo_of(assignee: Uuid, instance_no: &str) -> Vec<(Uuid, String)> {
    let page = cloudpivot::business::flow::model::page_my_tasks(
        assignee,
        cloudpivot::business::flow::model::MY_TODO_STATUSES,
        &["APPROVAL", "RESUBMIT"],
        cloudpivot::business::flow::model::MyTaskOrder::Arrived,
        1,
        200,
        true,
    )
    .await
    .unwrap_or_else(|err| panic!("查我的待办失败：{err}"));
    page.list
        .iter()
        .filter(|row| row.instance_no == instance_no)
        .map(|row| (row.task.id, row.task.status.clone()))
        .collect()
}

// ==================== 读：四账快照 ====================

/// 四账快照：`(库存数量, 库位账数量, 流水条数, 凭证张数)`。
///
/// - 库存数量：`biz_inventory` 该物料在该仓**全部行**求和。
///   ⚠️ 踩过的坑：不能只看 `location_id IS NULL` 的「成本账行」—— 收货明细一旦带库位，
///   过账把数量写进 `location_id = 库位库存行 id` 的那一行（`biz_inventory.location_id`
///   外键指 `biz_inventory_location.id`），此时不存在 NULL 行，只查 NULL 会恒为 0，
///   让「作废回基线」类断言全部变成假阳性。
/// - 库位账数量：`biz_inventory_location` 同物料同仓求和（不变式 B1 的另一侧）；
/// - 流水条数：`biz_inventory_txn` 该物料该仓条数（含反向冲销流水，所以「作废」不是回到 0 条）；
/// - 凭证张数：**该仓下的收货单**产生的凭证（按 `source_id` 关联，不做全局 `COUNT(*)`）。
pub async fn four_ledger_snapshot(mat: Uuid, wh: Uuid) -> (Decimal, Decimal, i64, i64) {
    let inv = scalar_dec(
        "SELECT COALESCE(sum(qty), 0) AS v FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&mat), Value::from(&wh)],
    )
    .await;
    let loc = scalar_dec(
        "SELECT COALESCE(sum(qty), 0) AS v FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&mat), Value::from(&wh)],
    )
    .await;
    let txn = scalar_i64(
        "SELECT count(*) AS v FROM biz_inventory_txn \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&mat), Value::from(&wh)],
    )
    .await;
    let vou = scalar_i64(
        "SELECT count(*) AS v FROM biz_voucher v \
          WHERE v.deleted_at IS NULL AND v.source_id IN \
                (SELECT s.id FROM biz_stock_in s \
                  WHERE s.warehouse_id = ?::uuid AND s.deleted_at IS NULL \
                    AND s.id IN (SELECT i.stock_in_id FROM biz_stock_in_item i \
                                  WHERE i.material_id = ?::uuid))",
        vec![Value::from(&wh), Value::from(&mat)],
    )
    .await;
    (inv, loc, txn, vou)
}

/// 单个数字（`Decimal`）。
pub async fn scalar_dec(sql: &str, params: Vec<Value>) -> Decimal {
    let rows = db()
        .find_with_params(sql, params)
        .await
        .unwrap_or_else(|err| panic!("标量查询失败：{err}\nSQL: {sql}"));
    match rows.first() {
        None => Decimal::ZERO,
        Some(row) => row.get_typed::<Decimal>("v").expect("标量 Decimal"),
    }
}

/// 单个整数。
pub async fn scalar_i64(sql: &str, params: Vec<Value>) -> i64 {
    let rows = db()
        .find_with_params(sql, params)
        .await
        .unwrap_or_else(|err| panic!("标量查询失败：{err}\nSQL: {sql}"));
    match rows.first() {
        None => 0,
        Some(row) => row.get_typed::<i64>("v").expect("标量 i64"),
    }
}

/// 单个字符串（无行 → `None`）。
pub async fn scalar_text(sql: &str, params: Vec<Value>) -> Option<String> {
    let rows = db()
        .find_with_params(sql, params)
        .await
        .unwrap_or_else(|err| panic!("标量查询失败：{err}\nSQL: {sql}"));
    rows.first()
        .and_then(|row| row.get_typed::<Option<String>>("v").ok().flatten())
}

/// 断言两个 `Decimal` 相等并打印业务口径（失败时给出可读上下文）。
#[track_caller]
pub fn assert_dec(actual: Decimal, expect: Decimal, what: &str) {
    assert_eq!(actual, expect, "{what}：期望 {expect}，实际 {actual}");
}
