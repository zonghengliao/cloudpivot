//! B2「数据权限基座」真库集成测试（**需要真实 PostgreSQL**，且已迁移到 V0067）。
//!
//! # 为什么在 `tests/` 而不是单测
//!
//! S1–S14 的纯逻辑断言都在 `business/shared/*` 的模块内单测里（零外部依赖）。
//! 本文件只验证**单测验证不了**的三件事：
//!
//! 1. `principal::resolve` 的解析 SQL 在真库上跑得通（3–4 表 join + 递归 CTE + `UNION ALL`），
//!    且五种范围 + 并集 + 三条降级（无部门 / 无角色 / 过期绑定 / 空 CUSTOM）的**可见行数**
//!    与预期**精确相等**；
//! 2. `scope::paginate` 的 `= ANY(?::text::uuid[])` 绑定形态在真驱动上真的能绑（`Value::Array`
//!    在本栈是 `UnsupportedFeature`，所以这条必须实测），并且 count 语句可执行、与数据页一致；
//! 3. **缓存不可用 → 回退查库**：测试进程内**没有**初始化全局缓存（`set_cache` 的参数类型
//!    `SharedCache` 未被 `alun` 门面 re-export），所以这里走的正是生产里「Redis 挂了」的
//!    那条分支：`cache_stats().cache_unavailable` 递增、`db_loads` 递增、结果仍然正确。
//!
//! # 用法
//!
//! ```bash
//! # 先准备一个专用库（不要用生产库），跑一次迁移（会自动建表 + 种子）：
//! set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
//! export ALUN_DATABASE_NAME=cloudpivot_b2v ALUN_SERVER_LISTEN=127.0.0.1:8099
//! timeout 30 ./target/debug/cloudpivot   # 看到「数据库迁移完成: 应用 N 个迁移」即可（自动退出）
//!
//! # 再跑本测试（夹具见 tests/sql/batchb/b2_scope_fixture.sql，测试会自己灌入）
//! export B2_DB_URL="postgres://cloudpivot:${ALUN_DATABASE_PASSWORD}@127.0.0.1:5432/cloudpivot_b2v"
//! cargo test --test batchB_b2_scope -- --nocapture --test-threads=1
//! ```
//!
//! 缺 `B2_DB_URL` 时打印 `SKIP` 并返回（`cargo test` 全绿，CI 不需要数据库）。
//!
//! # 一个用例，串行执行
//!
//! 埋点是进程内全局计数、而且断言依赖「用户总数 = 12」这个精确数字，所以本文件只有
//! **一个** `#[tokio::test]`（配合 `--test-threads=1` 使用，与其他集成测试文件互不干扰）。

use std::collections::BTreeSet;

use alun::prelude::*;
use alun::uuid::Uuid;
use cloudpivot::business::shared::principal::{self, ScopedPrincipal};
use cloudpivot::business::shared::scope::{self, ScopeTarget};
use cloudpivot::business::shared::{guard, scope::ScopeKind};

/// 夹具里固定 UUID（与 `tests/sql/batchb/b2_scope_fixture.sql` 一一对应）。
mod fixture {
    pub const DEPT_D1: &str = "b2000000-0000-0000-0000-000000000001";
    pub const DEPT_D1_1: &str = "b2000000-0000-0000-0000-000000000002";
    pub const DEPT_D1_1_1: &str = "b2000000-0000-0000-0000-000000000003";
    pub const DEPT_D1_1_2: &str = "b2000000-0000-0000-0000-000000000004";
    pub const DEPT_D2: &str = "b2000000-0000-0000-0000-000000000005";
    pub const DEPT_D2_1: &str = "b2000000-0000-0000-0000-000000000006";

    pub const U_ALL: &str = "b2200000-0000-0000-0000-000000000001";
    pub const U_DEPT: &str = "b2200000-0000-0000-0000-000000000002";
    pub const U_CHILD: &str = "b2200000-0000-0000-0000-000000000003";
    pub const U_SELF: &str = "b2200000-0000-0000-0000-000000000004";
    pub const U_CUSTOM: &str = "b2200000-0000-0000-0000-000000000005";
    pub const U_MULTI: &str = "b2200000-0000-0000-0000-000000000006";
    pub const U_NODEPT: &str = "b2200000-0000-0000-0000-000000000007";
    pub const U_NOROLE: &str = "b2200000-0000-0000-0000-000000000008";
    pub const U_BRANCH: &str = "b2200000-0000-0000-0000-000000000011";

    /// 夹具用户总数（11 个夹具用户 + 种子里的 admin）。
    pub const TOTAL_USERS: u64 = 12;
}

/// `sys_user` 列表的基础 SQL（主体表别名 `u`，与 `ScopeTarget::SYS_USER` 一致）。
const BASE_USER_SQL: &str = "SELECT u.*, d.dept_name AS dept_name FROM sys_user u \
     LEFT JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL \
     WHERE u.deleted_at IS NULL";

/// `sys_dept` 列表的基础 SQL（主体表别名 `d`）。
const BASE_DEPT_SQL: &str = "SELECT d.* FROM sys_dept d WHERE d.deleted_at IS NULL";

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

/// 初始化全局数据库（进程内只初始化一次）；返回 `false` 表示跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("B2_DB_URL") else {
                println!("SKIP: 未设置 B2_DB_URL（不连真实数据库）");
                return false;
            };
            if try_db().is_some() {
                return true;
            }
            match Db::connect(&url).await {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    println!("B2 已连接验证库");
                    true
                }
                Err(error) => {
                    println!("SKIP: 连接数据库失败：{error}");
                    false
                }
            }
        })
        .await
}

/// 灌入夹具（幂等）。
async fn seed() -> bool {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/sql/batchb/b2_scope_fixture.sql"
    );
    let Ok(sql) = std::fs::read_to_string(path) else {
        println!("SKIP: 读不到夹具 {path}");
        return false;
    };
    match db().execute_script(&sql).await {
        Ok(()) => {
            println!("B2 夹具已灌入（{path}）");
            true
        }
        Err(error) => {
            println!("SKIP: 夹具灌入失败：{error}");
            false
        }
    }
}

/// 解析并打印用户范围，返回 principal。
async fn resolve_and_log(label: &str, user_id: &str) -> ScopedPrincipal {
    let principal = principal::resolve(&uuid(user_id))
        .await
        .unwrap_or_else(|error| panic!("{label} 解析失败：{error}"));
    println!(
        "  {label:<10} kind={:<14} dept_set={:<2} self_only={:<5} own_dept={}",
        principal.kind().code(),
        principal.dept_ids().len(),
        principal.self_only(),
        principal
            .own_dept_id()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "NULL".to_string())
    );
    principal
}

/// 真库可见行数（`scope::paginate` 的 `total`）。
async fn count_users(principal: &ScopedPrincipal) -> u64 {
    let page = scope::paginate(
        ScopeTarget::SYS_USER,
        BASE_USER_SQL,
        vec![],
        Some("ORDER BY u.created_at ASC, u.id ASC"),
        1,
        50,
        principal,
    )
    .await
    .expect("sys_user 分页失败");
    page.total
}

#[tokio::test]
async fn b2_real_database_acceptance() {
    if !ensure_db().await {
        return;
    }
    if !seed().await {
        return;
    }

    println!("\n================ B2 真库验收：五种范围 + 并集 + 降级 ================");
    principal::reset_cache_stats();

    // ---------- ① 超管（SUPER_ADMIN）短路 ----------
    let admin_rows: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM sys_user \
             WHERE deleted_at IS NULL AND username = 'admin' AND status = 1",
            vec![],
        )
        .await
        .expect("查 admin 失败");
    assert!(admin_rows > 0, "种子里的 admin 必须存在");
    let admin_id: Uuid = db()
        .scalar_with_params(
            "SELECT id FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("查 admin id 失败");
    let admin = resolve_and_log("admin", &admin_id.to_string()).await;
    assert!(admin.is_super(), "admin 必须持有 SUPER_ADMIN");
    assert_eq!(admin.kind(), ScopeKind::All);
    assert_eq!(
        count_users(&admin).await,
        fixture::TOTAL_USERS,
        "超管看到全部用户"
    );

    // ---------- ② 五种范围 ----------
    // 2.1 ALL
    let u_all = resolve_and_log("u_all", fixture::U_ALL).await;
    assert_eq!(u_all.kind(), ScopeKind::All);
    assert!(u_all.dept_ids().is_empty(), "ALL 短路后部门集合必须清空");
    assert_eq!(count_users(&u_all).await, fixture::TOTAL_USERS);

    // 2.2 DEPT（本部门，不含下级）
    let u_dept = resolve_and_log("u_dept", fixture::U_DEPT).await;
    assert_eq!(u_dept.kind(), ScopeKind::Dept);
    assert_eq!(u_dept.dept_ids(), &[uuid(fixture::DEPT_D1_1)]);
    assert_eq!(count_users(&u_dept).await, 3, "B2-D1-1 本部门 3 个用户");

    // 2.3 DEPT_AND_CHILD（本部门及以下 = 3 个部门）
    let u_child = resolve_and_log("u_child", fixture::U_CHILD).await;
    assert_eq!(u_child.kind(), ScopeKind::DeptAndChild);
    let subtree: BTreeSet<String> = u_child.dept_ids().iter().map(|id| id.to_string()).collect();
    let expected: BTreeSet<String> = [
        fixture::DEPT_D1_1,
        fixture::DEPT_D1_1_1,
        fixture::DEPT_D1_1_2,
    ]
    .iter()
    .map(|id| id.to_string())
    .collect();
    assert_eq!(subtree, expected, "子树必须恰好是 B2-D1-1 及其 2 个子部门");
    assert_eq!(count_users(&u_child).await, 5, "子树内 5 个用户");

    // 2.4 SELF（仅本人）
    let u_self = resolve_and_log("u_self", fixture::U_SELF).await;
    assert_eq!(u_self.kind(), ScopeKind::SelfOnly);
    assert!(u_self.self_only());
    assert_eq!(count_users(&u_self).await, 1, "仅本人只能看到自己");

    // 2.5 CUSTOM（B2-D2 + B2-D2-1）
    let u_custom = resolve_and_log("u_custom", fixture::U_CUSTOM).await;
    assert_eq!(u_custom.kind(), ScopeKind::Custom);
    let custom: BTreeSet<String> = u_custom
        .dept_ids()
        .iter()
        .map(|id| id.to_string())
        .collect();
    let expected_custom: BTreeSet<String> = [fixture::DEPT_D2, fixture::DEPT_D2_1]
        .iter()
        .map(|id| id.to_string())
        .collect();
    assert_eq!(custom, expected_custom);
    assert_eq!(count_users(&u_custom).await, 3, "B2-D2 系 3 个用户");

    // ---------- ③ 多角色并集（DEPT ∪ SELF） ----------
    let u_multi = resolve_and_log("u_multi", fixture::U_MULTI).await;
    assert_eq!(u_multi.kind(), ScopeKind::Dept, "并集的主导范围仍是部门类");
    assert!(u_multi.self_only(), "并集里出现 SELF → self_only=true");
    assert_eq!(
        count_users(&u_multi).await,
        3,
        "本人已在部门内，并集不放大也不缩小"
    );

    // ---------- ④ 三条降级（安全侧） ----------
    let u_nodept = resolve_and_log("u_nodept", fixture::U_NODEPT).await;
    assert_eq!(
        u_nodept.kind(),
        ScopeKind::SelfOnly,
        "无部门 → 降级为仅本人"
    );
    assert_eq!(count_users(&u_nodept).await, 1);

    let u_norole = resolve_and_log("u_norole", fixture::U_NOROLE).await;
    assert_eq!(
        u_norole.kind(),
        ScopeKind::SelfOnly,
        "无角色 → 降级为仅本人"
    );
    assert_eq!(
        u_norole.own_dept_id(),
        Some(uuid(fixture::DEPT_D1)),
        "部门归属仍应如实解析出来（降级只影响可见范围）"
    );
    assert_eq!(count_users(&u_norole).await, 1);

    let u_branch = resolve_and_log("u_branch", fixture::U_BRANCH).await;
    assert_eq!(
        u_branch.kind(),
        ScopeKind::SelfOnly,
        "过期绑定不算持有 → 降级为仅本人"
    );
    assert_eq!(count_users(&u_branch).await, 1);

    // ---------- ⑤ `sys_dept` 也要按范围裁剪 ----------
    let dept_page = scope::paginate(
        ScopeTarget::SYS_DEPT,
        BASE_DEPT_SQL,
        vec![],
        Some("ORDER BY d.sort_order ASC, d.id ASC"),
        1,
        50,
        &u_child,
    )
    .await
    .expect("sys_dept 分页失败");
    assert_eq!(dept_page.total, 3, "部门树按子树裁剪：3 个部门");
    let self_dept_page = scope::paginate(
        ScopeTarget::SYS_DEPT,
        BASE_DEPT_SQL,
        vec![],
        None,
        1,
        50,
        &u_self,
    )
    .await
    .expect("sys_dept(SELF) 分页失败");
    assert_eq!(
        self_dept_page.total, 1,
        "仅本人 → 只能看到本人所在部门这一行"
    );

    // ---------- ⑥ count 语句可直接执行（S12：无 ORDER BY），且与数据页一致 ----------
    let filter = scope::filter_for(&u_child, ScopeTarget::SYS_USER);
    let count_sql = format!(
        "SELECT COUNT(*) FROM ({}{}) AS _scope_count",
        BASE_USER_SQL.trim_end(),
        filter.sql()
    );
    let manual_total: i64 = db()
        .scalar_with_params(&count_sql, filter.params().to_vec())
        .await
        .expect("count 语句必须可执行");
    assert_eq!(manual_total as u64, count_users(&u_child).await);
    assert!(
        !count_sql.to_uppercase().contains("ORDER BY"),
        "count 不得含 ORDER BY"
    );
    println!("\n[S12] 真库 count 语句（无 ORDER BY，可执行）：\n  {count_sql}");
    println!(
        "[S12] 片段：`{}` binds={}",
        filter.sql(),
        filter.bind_count()
    );

    // ---------- ⑦ 参数顺序契约（S8/S13）：带业务参数的查询必须用 {FILTER} ----------
    // u_child 子树 + 用户名 LIKE '%leaf%' → 只有 b2_u_leaf1/b2_u_leaf2 命中。
    // 若片段参数与业务参数错位，驱动会因「text 参数绑到 uuid[]」直接报错——真库是最硬的断言。
    let base_with_business_filter = "SELECT u.* FROM sys_user u \
         WHERE u.deleted_at IS NULL \
         {FILTER} AND u.username LIKE ?";
    let page = scope::paginate(
        ScopeTarget::SYS_USER,
        base_with_business_filter,
        vec![Value::Text("%leaf%".to_string())],
        Some("ORDER BY u.username ASC"),
        1,
        50,
        &u_child,
    )
    .await
    .expect("带业务参数的分页失败（参数顺序契约被破坏时会在这里报错）");
    assert_eq!(page.total, 2, "子树 ∩ username LIKE %leaf% = 2");
    let names: Vec<String> = page
        .list
        .iter()
        .map(|row| row.get_typed::<String>("username").unwrap_or_default())
        .collect();
    assert_eq!(names, vec!["b2_u_leaf1", "b2_u_leaf2"]);
    println!(
        "\n[S8/S13] FILTER 标记 + 业务参数 `AND u.username LIKE ?` → total=2 rows={names:?} ✓"
    );

    // 反向：带业务参数却没有 `{FILTER}` → 必须报错（否则参数会静默错位）
    let error = scope::paginate(
        ScopeTarget::SYS_USER,
        "SELECT u.* FROM sys_user u WHERE u.deleted_at IS NULL AND u.username LIKE ?",
        vec![Value::Text("%leaf%".to_string())],
        None,
        1,
        50,
        &u_child,
    )
    .await
    .expect_err("缺 {FILTER} 时必须拒绝");
    println!("[S13] 缺 {{FILTER}} → {error} ✓");

    // ---------- ⑧ 缓存不可用 → 回退查库（本进程没有初始化全局缓存） ----------
    let stats = principal::cache_stats();
    assert_eq!(stats.hits, 0, "本进程没有初始化缓存，不可能命中");
    assert!(stats.db_loads >= 9, "9 次 resolve 必须全部查库：{stats:?}");
    assert_eq!(
        stats.cache_unavailable, stats.db_loads,
        "每次都记录「缓存不可用」：{stats:?}"
    );
    println!("\n[回退查库] 埋点 {stats:?}");
    println!("[回退查库] 无缓存时 9 个用户的解析与可见行数全部正确（见上方逐条断言）✓");

    // ---------- ⑨ 启动自检（闸 3）：登记表列存在 + SUPER_ADMIN 存在 ----------
    let report = guard::preflight_permission().await.expect("自检查库失败");
    println!("\n[自检] {}", report.summary());
    assert!(report.is_ok(), "登记表与超管角色必须齐备：{report:?}");
    assert_eq!(report.checked_tables, 10);

    // ---------- ⑩ 范围上限（S14）：真解析出的操作者 + 越界部门 ----------
    let allowed = guard::ensure_scope_grant_allowed(&u_child, &[uuid(fixture::DEPT_D1_1_1)]);
    assert!(allowed.is_ok(), "子树内的部门可以授权");
    let denied = guard::ensure_scope_grant_allowed(&u_child, &[uuid(fixture::DEPT_D2)]);
    assert!(denied.is_err(), "跨树部门必须拒绝");
    println!(
        "[S14] 子树内→Ok；跨树 B2-D2 → {} ✓",
        denied.err().unwrap().msg
    );

    println!("\n================ B2 真库验收全部通过 ================\n");
}
