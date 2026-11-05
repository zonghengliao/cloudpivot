//! 测试C（系统管理模块测试）共用的夹具与真库连接。
//!
//! # 为什么在 `src/` 里的 `#[cfg(test)]` 而不是 `tests/`
//!
//! 交付口径是 `cargo test --lib system::` 全绿（卡片「四、交付要求 1」），只有
//! **lib 单测**能被这个过滤串命中；`tests/` 下的集成测试属于另一个 target，过滤串
//! 命中不了它们。此外这些用例要直接调用 `handler` 的真实函数（拿到真实的 400 / 404
//! / 409 语义与真实副作用），`pub(crate)` 的辅助函数在集成测试里也看不见。
//!
//! # 真库约定
//!
//! - 连接串只从环境变量 `SYS_DB_URL` 取；**没设就跳过**（打印 `SKIP` 后返回，
//!   `cargo test` 仍然全绿）。测试代码里不写任何账号口令（gitleaks 红线）。
//! - 只用 `cloudpivot_test` 库（卡片硬红线 3）。
//! - 每个用例自己造夹具、自己按前缀清理，**不依赖全局 COUNT(\*)**：本库里还有别的
//!   卡留下的数据，任何「总数等于 N」的断言都会漂。
//!
//! # 为什么可以直接调 handler
//!
//! handler 的四个提取器都可手工构造：`HeaderMap::new()`、`ConnectInfo(addr)`、
//! `Extension(AuthClaims(claims))`、`ValidatedJson(req)`（`ValidatedJson` 是
//! `pub struct ValidatedJson<T>(pub T)`）。因此可以绕过 HTTP 直接跑真实业务分支，
//! 断言 `ApiError.status` / `ApiError.code` 与库里的真实落库结果。
//!
//! **唯一绕不过的是 `#[nik::permission]` 的 403**：那是路由层的中间件决策，不在
//! handler 内部。越权类用例（O 系列）因此在 `system::authz_tests` 里用「登录时装载
//! 的权限集合 + 框架 `has_permission` 判定」来验证，判定谓词与中间件逐字一致
//! （`vendor/nik-web/src/middleware/permission_check.rs:262`）。

#![cfg(test)]

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use nik::prelude::*;
use std::net::SocketAddr;

/// 供 `Db::connect` 的测试库连接串（只从环境变量读）。
pub(crate) fn db_url() -> Option<String> {
    std::env::var("SYS_DB_URL").ok().filter(|v| !v.is_empty())
}

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `SYS_DB_URL` 或连不上。
///
/// 与 `tests/batchB_b2_scope.rs`（B2 卡）同一套约定：缺库时打印 SKIP 返回，
/// 用例体直接 `return`，不让 `cargo test` 变红。
///
/// # 为什么连接必须建在专用 runtime 里（而不是用例的 runtime）
///
/// `#[tokio::test]` 给**每个用例**都建一个独立 runtime，而全局 `db()` 只能初始化
/// 一次（`OnceCell`）。如果在用例的 runtime 里建连，池里的连接（及其后台任务）就
/// 绑在那个 runtime 上；用例一结束连接被关闭，后续用例取到这种连接，任何一句 SQL
/// 都会失败成 `connection closed` —— 在 handler 里就是 `500 / 10004 服务器内部错误`。
/// 实测：同一份代码连续跑三次，每次都有 1–3 条**随机**用例因此变红（菜单用例），
/// 与业务逻辑无关，纯属测试基建抖动。
///
/// 修法：在一个常驻的专用 thread + runtime 里建连并把它留在那里
/// （[`connect_in_dedicated_runtime`]），连接生命周期与任何用例的 runtime 解耦。
pub(crate) async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Some(url) = db_url() else {
                println!("SKIP: 未设置 SYS_DB_URL（不连真实数据库）");
                return false;
            };
            // 钉住本进程的起点：残留自检据此区分「历史残留」与「本次 run 造的夹具」
            let _ = suite_start();
            if try_db().is_some() {
                // 库是别的模块先建的：run 起点照样要钉（审计断言依赖它界定判据范围）
                capture_run_epoch().await;
                return true;
            }
            match connect_in_dedicated_runtime(&url) {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    // 关键次序：**先**钉 run 起点，再把控制权交回用例 ——
                    // 任何夹具行的 created_at 都只会 >= 这个时刻（见 `run_epoch`）
                    capture_run_epoch().await;
                    println!("测试C 已连接验证库（cloudpivot_test）");
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

/// 在**永不退出**的专用 runtime 里建连，返回连接池。
///
/// 建连完成后该 runtime 用 `pending()` 常驻：池里的连接与它们的后台任务
/// （`tokio_postgres` 的 socket driver 是 `tokio::spawn` 出来的）都跑在这条线程上，
/// 因此任何用例的 runtime 结束都不会把它们带走（原因见 [`ensure_db`]）。
///
/// # 为什么要把 `min_connections` 拉满
///
/// 驱动的池是**懒建连**的：`min_connections` 只在建池时预热，之后「空闲栈空了 + 还有
/// 并发额度」就会在**调用方 runtime** 里新建连接 —— 那条连接又会被下一个用例的
/// runtime 结束带走。默认 `min_connections = 1`，实测仍会随机报
/// `connection closed`。这里把 `min/max` 都设成 [`DEDICATED_POOL_SIZE`]，
/// 让所有连接都在专用 runtime 里一次性建好，用例只复用它们。
///
/// 建连结果通过 `std::sync::mpsc` 同步取回（只阻塞一次建连，测试代码可接受）。
fn connect_in_dedicated_runtime(url: &str) -> std::result::Result<Db, String> {
    /// 专用池容量（用例串行执行，8 条远超实际并发额度）
    const DEDICATED_POOL_SIZE: u32 = 8;

    let (sender, receiver) = std::sync::mpsc::channel::<std::result::Result<Db, String>>();
    let url = url.to_string();
    std::thread::Builder::new()
        .name("test-db-runtime".to_string())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            else {
                let _ = sender.send(Err("无法创建专用数据库 runtime".to_string()));
                return;
            };
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.min_connections = DEDICATED_POOL_SIZE;
            config.pool.max_connections = DEDICATED_POOL_SIZE;
            // 不设空闲回收 / 存活上限：连接一旦被回收，下一次又会在用例 runtime 里重建
            config.pool.idle_timeout = None;
            config.pool.max_lifetime = None;
            match runtime.block_on(Db::connect_with(&url, config)) {
                Ok(database) => {
                    let _ = sender.send(Ok(database));
                    // 常驻：这条 runtime 一退出，池里的连接就又变成「已关闭」
                    runtime.block_on(std::future::pending::<()>());
                }
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                }
            }
        })
        .map_err(|error| format!("无法启动专用数据库 runtime 线程：{error}"))?;
    receiver
        .recv()
        .unwrap_or_else(|_| Err("专用数据库 runtime 未返回建连结果".to_string()))
}

/// 请求头（审计日志的派生字段用；空表即可）。
pub(crate) fn headers() -> HeaderMap {
    HeaderMap::new()
}

/// `ConnectInfo` 提取器（真实客户端 IP 的来源；审计日志会读它）。
pub(crate) fn connect_info() -> ConnectInfo<SocketAddr> {
    ConnectInfo(
        "127.0.0.1:54321"
            .parse::<SocketAddr>()
            .expect("固定地址必须可解析"),
    )
}

/// 构造令牌声明。
///
/// `permissions` 决定 `has_permission` 的结果：传 `["*:*:*"]` 即超管（框架判定），
/// 传空数组即「无任何权限」的普通用户。
pub(crate) fn claims(user_id: &str, username: &str, permissions: &[&str]) -> AuthClaims {
    AuthClaims(TokenClaims {
        jti: Some(format!("testc-{user_id}")),
        sub: user_id.to_string(),
        username: Some(username.to_string()),
        roles: Vec::new(),
        permissions: permissions.iter().map(|p| (*p).to_string()).collect(),
        token_type: Some(TokenType::Access),
        exp: usize::try_from(chrono::Utc::now().timestamp() + 3600).unwrap_or(usize::MAX),
        iat: usize::try_from(chrono::Utc::now().timestamp()).unwrap_or(0),
        extra: std::collections::HashMap::new(),
    })
}

/// 超管声明（`*:*:*`），用于调用「需要超管豁免」的 handler 分支。
pub(crate) fn super_claims() -> AuthClaims {
    claims(
        "00000000-0000-7000-8000-0000000000ff",
        constants_admin(),
        &["*:*:*"],
    )
}

/// 内置管理员用户名（与 `constants::ADMIN_USERNAME` 保持一致）。
fn constants_admin() -> &'static str {
    crate::shared::constants::ADMIN_USERNAME
}

/// 每个用例唯一的后缀（毫秒 + 进程内自增），用于所有夹具标识与清理前缀。
pub(crate) fn uniq() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let ms = chrono::Utc::now().timestamp_millis();
    format!("T{ms}{seq:03}")
}

/// 执行一条写 SQL（返回受影响行数）。
pub(crate) async fn exec(sql: &str, params: Vec<Value>) -> u64 {
    db().execute_with_params(sql, params)
        .await
        .unwrap_or_else(|e| panic!("执行 SQL 失败：{e}\nSQL: {sql}"))
}

/// 读一个 i64 标量。
pub(crate) async fn scalar_i64(sql: &str, params: Vec<Value>) -> i64 {
    db().scalar_with_params(sql, params)
        .await
        .unwrap_or_else(|e| panic!("查询标量失败：{e}\nSQL: {sql}"))
}

/// 读一个 `Option<String>` 标量（列不存在 / NULL → None）。
pub(crate) async fn scalar_text(sql: &str, params: Vec<Value>) -> Option<String> {
    let rows = db()
        .find_with_params(sql, params)
        .await
        .unwrap_or_else(|e| panic!("查询失败：{e}\nSQL: {sql}"));
    let row = rows.first()?;
    row.get_typed::<Option<String>>("v").unwrap_or(None)
}

/// 断言 `ApiError` 的 HTTP 状态与业务码。
pub(crate) fn assert_err(error: &ApiError, status: u16, code: i32, label: &str) {
    assert_eq!(
        (error.status, error.code),
        (status, code),
        "{label}：期望 {status}/{code}，实际 {}/{}（{}）",
        error.status,
        error.code,
        error.msg
    );
}

// ==================== 清理：失败必须可见（不许静默吞错） ====================
//
// # 为什么不再写 `let _ = db().execute_with_params(...)`
//
// 清理 SQL 的错误被吞掉时，表象是「用例全绿、库里一堆夹具」：
//
// - 用例在 cleanup 之前 panic（并发 / 中断 / `connection closed`）；
// - 或者删的次序不满足外键依赖（`sys_role_menu` → `sys_user_role` → 实体表）；
// - 或者**整批 DELETE 里只要有一行被业务表外键挡住，PostgreSQL 就把整条语句回滚**
//   —— 一个脏行连累全部夹具。实测：一次 `system::` 只跑一遍就往 `cloudpivot_test`
//   攒下 33 行软删用户，且永远删不掉。
//
// 残留还会被一致性扫描（死权限码 /「注解有但菜单无」）算进基线，让「基线计数」失真。
//
// 因此这里的规矩是：清理失败**不 panic**（会掩盖真正的断言结果），但必须打印
// `WARN[cleanup]` + 原始 SQL + 数据库错误原文；单行删不掉时**只影响那一行**。

/// 执行一条清理 SQL；失败打 `WARN[cleanup]` 并返回 0（**不 panic**）。
pub(crate) async fn cleanup_sql(label: &str, sql: &str, params: Vec<Value>) -> u64 {
    match db().execute_with_params(sql, params).await {
        Ok(rows) => rows,
        Err(error) => {
            println!("WARN[cleanup] {label} 失败：{error}\n  SQL: {sql}");
            0
        }
    }
}

/// 清理本次用例造的数据：按 `like` 前缀物理删（`sys_dept` / `sys_user` / `sys_role` 等）。
///
/// 用前缀而不是「全表删除」：`cloudpivot_test` 是多卡共用的验证库，
/// 全局 `DELETE` / `COUNT(*)` 都会把别的卡的数据卷进来（卡片硬红线 6）。
///
/// 返回受影响行数；失败打 `WARN[cleanup]`（见文件末尾「清理：失败必须可见」）。
pub(crate) async fn cleanup_table(table: &str, column: &str, prefix: &str) -> u64 {
    let sql = format!("DELETE FROM {table} WHERE {column} LIKE ?");
    cleanup_sql(
        &format!("cleanup_table({table}.{column} LIKE '{prefix}%')"),
        &sql,
        vec![Value::from(format!("{prefix}%"))],
    )
    .await
}

/// 一次批量清理的结果。
#[derive(Debug, Default)]
pub(crate) struct SweepReport {
    /// 实体（表名 + 前缀），用于日志
    pub label: String,
    /// 物理删掉的行数
    pub deleted: usize,
    /// 仍被**业务表**外键引用、按设计只能软删的行（业务数据要留引用，不算清理失败）
    pub kept_by_business: Vec<String>,
    /// 清理失败（不该发生；自检用例会把它判红）
    pub failed: Vec<String>,
}

impl SweepReport {
    pub(crate) fn is_clean(&self) -> bool {
        self.failed.is_empty()
    }

    pub(crate) fn summary(&self) -> String {
        format!(
            "{}：硬删 {} 行，业务外键保留 {} 行，清理失败 {} 行",
            self.label,
            self.deleted,
            self.kept_by_business.len(),
            self.failed.len()
        )
    }
}

/// 从外键报错里认出「被哪张**业务表**钉住」。
///
/// 驱动会给出两种措辞，都要认：
///
/// 1. PostgreSQL 原始文案：`update or delete on table "sys_user" violates foreign key
///    constraint "biz_flow_task_assignee_id_fkey" on table "biz_flow_task"` → 返回 `biz_flow_task`；
/// 2. 框架包装文案：`foreign key violated (constraint \`biz_flow_task_assignee_id_fkey\`)`
///    → 返回约束名（同样足以定位）。
///
/// 业务表（`biz_*`）里的单据是别的卡造的真实业务数据，测试夹具用户被它引用时
/// **不能硬删**（会破坏引用完整性），只能软删并留一条可见记录；如果钉住的是
/// 系统表自己的绑定（`sys_user_role` / `sys_user_preference`），那就是我们清理
/// 没清干净，必须算失败。
fn business_pin(error: &str) -> Option<String> {
    if let Some(start) = error.find("on table \"biz_") {
        let rest = &error[start + "on table \"".len()..];
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }
    // 包装文案：取 `constraint` 之后引号里的名字，且必须是以 `biz_` 开头的约束名
    let constraint_at = error.find("constraint")?;
    let rest = &error[constraint_at..];
    for quote in ['`', '"'] {
        let Some(open) = rest.find(quote) else {
            continue;
        };
        let after = &rest[open + 1..];
        let Some(close) = after.find(quote) else {
            continue;
        };
        let name = &after[..close];
        if name.starts_with("biz_") {
            return Some(name.to_string());
        }
    }
    None
}

/// 清一批**实体行**：先按依赖顺序清引用行，再**逐行**物理删实体行。
///
/// # 为什么逐行
///
/// 整批 `DELETE ... WHERE username LIKE 'tc_u%'` 只要有一行被外键挡住，
/// PostgreSQL 会整条语句回滚 —— 一个脏行连累全部夹具。逐行删把影响面收敛到那一行，
/// 并让每一行的失败都能单独打印出来。
///
/// `dependents` 是 `(标签, SQL, 占位符个数)`：SQL 里的 `?` 全部绑定同一个
/// `like` 参数（`sys_role_mutex` 需要两处，所以有 arity）。
async fn sweep_entity(
    label: &str,
    table: &str,
    column: &str,
    prefix: &str,
    dependents: &[(&str, &str, usize)],
) -> SweepReport {
    let like = format!("{prefix}%");
    let mut report = SweepReport {
        label: format!("{label}({table}.{column} LIKE '{like}')"),
        ..SweepReport::default()
    };

    for (dep_label, sql, arity) in dependents {
        let params = vec![Value::from(like.clone()); *arity];
        let rows = cleanup_sql(&format!("{}→{dep_label}", report.label), sql, params).await;
        if rows > 0 {
            println!(
                "[cleanup] {} 先清引用行 {dep_label}：{rows} 行",
                report.label
            );
        }
    }

    let ids_sql = format!("SELECT id::text AS v FROM {table} WHERE {column} LIKE ?");
    let ids = match db()
        .find_with_params(&ids_sql, vec![Value::from(like)])
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| row.get_typed::<Option<String>>("v").unwrap_or(None))
            .collect::<Vec<_>>(),
        Err(error) => {
            report
                .failed
                .push(format!("查询待清行失败：{error}｜SQL: {ids_sql}"));
            println!("WARN[cleanup] {} 查询待清行失败：{error}", report.label);
            return report;
        }
    };

    for id in ids {
        let sql = format!("DELETE FROM {table} WHERE id = ?::text::uuid");
        match db()
            .execute_with_params(&sql, vec![Value::from(id.clone())])
            .await
        {
            Ok(_) => report.deleted += 1,
            Err(error) => {
                let text = error.to_string();
                if let Some(pinned_by) = business_pin(&text) {
                    // 业务表钉住：软删（保留引用完整性），并留一条可见记录
                    let soft = format!(
                        "UPDATE {table} SET deleted_at = now() WHERE id = ?::text::uuid AND deleted_at IS NULL"
                    );
                    match db()
                        .execute_with_params(&soft, vec![Value::from(id.clone())])
                        .await
                    {
                        Ok(_) => {
                            println!(
                                "WARN[cleanup] {} 行 {id} 被业务表 {pinned_by} 引用，已软删（硬删会破坏业务引用）",
                                report.label
                            );
                            report.kept_by_business.push(format!("{id}←{pinned_by}"));
                        }
                        Err(soft_error) => {
                            report.failed.push(format!("{id} 软删失败：{soft_error}"));
                            println!(
                                "WARN[cleanup] {} 行 {id} 软删也失败：{soft_error}",
                                report.label
                            );
                        }
                    }
                    continue;
                }
                report.failed.push(format!("{id}: {text}"));
                println!(
                    "WARN[cleanup] {} 行 {id} 硬删失败（非业务表外键）：{text}",
                    report.label
                );
            }
        }
    }

    println!("[cleanup] {}", report.summary());
    report
}

/// 清一批夹具用户（`sys_user.username LIKE '<prefix>%'`）。
pub(crate) async fn sweep_users(prefix: &str) -> SweepReport {
    sweep_entity(
        "夹具用户",
        "sys_user",
        "username",
        prefix,
        &[
            (
                "sys_user_preference",
                "DELETE FROM sys_user_preference WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE ?)",
                1,
            ),
            (
                "sys_user_role",
                "DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE ?)",
                1,
            ),
            (
                "sys_field_mask",
                "DELETE FROM sys_field_mask WHERE create_by IN (SELECT id FROM sys_user WHERE username LIKE ?)",
                1,
            ),
        ],
    )
    .await
}

/// 清一批夹具角色（`sys_role.role_code LIKE '<prefix>%'`）。
///
/// 依赖顺序：`sys_role_menu` / `sys_user_role` / `sys_role_dept` / `sys_field_mask`
/// / `sys_role_mutex` → `sys_role`。
pub(crate) async fn sweep_roles(prefix: &str) -> SweepReport {
    sweep_entity(
        "夹具角色",
        "sys_role",
        "role_code",
        prefix,
        &[
            (
                "sys_role_menu",
                "DELETE FROM sys_role_menu WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
                1,
            ),
            (
                "sys_user_role",
                "DELETE FROM sys_user_role WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
                1,
            ),
            (
                "sys_role_dept",
                "DELETE FROM sys_role_dept WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
                1,
            ),
            (
                "sys_field_mask",
                "DELETE FROM sys_field_mask WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
                1,
            ),
            (
                "sys_role_mutex",
                "DELETE FROM sys_role_mutex WHERE role_a_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?) \
                 OR role_b_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
                2,
            ),
        ],
    )
    .await
}

/// 清一批夹具菜单（`sys_menu.name LIKE '<prefix>%'`）：先 `sys_role_menu`，再删菜单。
pub(crate) async fn sweep_menus(prefix: &str) -> SweepReport {
    sweep_entity(
        "夹具菜单",
        "sys_menu",
        "name",
        prefix,
        &[(
            "sys_role_menu",
            "DELETE FROM sys_role_menu WHERE menu_id IN (SELECT id FROM sys_menu WHERE name LIKE ?)",
            1,
        )],
    )
    .await
}

/// 清一批夹具部门（`sys_dept.dept_code LIKE '<prefix>%'`）：先 `sys_role_dept`，再删部门。
pub(crate) async fn sweep_depts(prefix: &str) -> SweepReport {
    sweep_entity(
        "夹具部门",
        "sys_dept",
        "dept_code",
        prefix,
        &[(
            "sys_role_dept",
            "DELETE FROM sys_role_dept WHERE dept_id IN (SELECT id FROM sys_dept WHERE dept_code LIKE ?)",
            1,
        )],
    )
    .await
}

// ==================== 用例级夹具身份（并发安全的清扫口径） ====================
//
// # 为什么要「按本用例身份」而不是按模块前缀清扫
//
// `masking` / `system::user` / `system::role` 三个模块原先各有一个
// `sweep()`：`DELETE FROM sys_user WHERE username LIKE 'tc_u%'` 这种**整批前缀**扫。
// 在 libtest 默认线程数（= CPU 数）下，同一个模块的兄弟用例是**并行**跑的：
// A 用例一开头 sweep 一次，就把 B 用例刚刚建好、正在使用的操作者/角色/规则一起删掉。
// 实测（卡 t_18dcb777，干净库、只改线程数）：
//
// ```text
// business::masking  --test-threads=4 → 8 passed / 2 failed   --test-threads=1 → 10/0
// system::user       --test-threads=4 → 7 passed / 5 failed   --test-threads=1 → 12/0
// system::role       --test-threads=4 → 4 passed / 4 failed   --test-threads=1 → 8/0
// ```
//
// 报错形如 `30001 操作者账号不存在，拒绝操作`、`sys_user_role_user_id_fkey` 违约 ——
// 都是「自己的操作者被兄弟用例 sweep 掉了」。
//
// # 现在的口径
//
// 每个用例开头 `let fx = ts::Fixture::new("tc_u", "u03")` 拿到一个**唯一身份**
// （`tc_u_u03_<毫秒+进程内序号>`），本用例造的所有行都带上它；
// `fx.sweep()` 只做两段式删除：
//
// ```sql
// SELECT id FROM <表> WHERE <身份列> LIKE '<本用例身份>%'   -- ① 只捞自己的
// DELETE FROM <表> WHERE id IN (<上面那批 id>)              -- ② 只按 id 删
// ```
//
// **禁止**再按 `tc_m%` / `tc_u%` / `tc_r%` 整批前缀扫：那是「删别人的行」的唯一来源。

/// 正在运行（尚未结束）的用例身份登记表。
///
/// 残留自检（`x00`）据此排除**并行兄弟用例正在用**的行：那些行不是残留，
/// 只是还没到清理那一步。用例一结束（`Fixture` 被 Drop）身份即注销，
/// 它留下的行立刻重新纳入残留判据 —— 口径没有放宽，只是不再把「在途」当「泄漏」。
fn active_registry() -> &'static std::sync::Mutex<Vec<String>> {
    static REGISTRY: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// 当前仍在运行的用例身份（快照）。
pub(crate) fn active_tags() -> Vec<String> {
    active_registry()
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default()
}

fn register_tag(tag: &str) {
    if let Ok(mut guard) = active_registry().lock() {
        guard.push(tag.to_string());
    }
}

fn unregister_tag(tag: &str) {
    if let Ok(mut guard) = active_registry().lock() {
        guard.retain(|item| item != tag);
    }
}

/// 一个用例的夹具身份。Drop 时自动注销（panic 展开也会走到）。
#[derive(Debug)]
pub(crate) struct Fixture {
    tag: String,
}

impl Fixture {
    /// 造一个用例级身份。`family` 是模块族的表头（`tc_m` / `tc_u` / `tc_r`），
    /// `case` 是用例号（`m05` / `u03` / `r02`），最终形如 `tc_m_m05_T1793800000000001`。
    pub(crate) fn new(family: &str, case: &str) -> Self {
        let tag = format!("{family}_{}_{}", case.to_ascii_lowercase(), uniq());
        register_tag(&tag);
        Self { tag }
    }

    /// 本用例的唯一身份（同时是 `username` / `role_code` / `menu.name` /
    /// `dept_code` / `sys_field_mask.reason` 的公共前缀）。
    pub(crate) fn tag(&self) -> &str {
        &self.tag
    }

    /// 在本用例身份下派生一个夹具标识（`fx.name("holder")` → `tc_m_m05_xxx_holder`）。
    pub(crate) fn name(&self, suffix: &str) -> String {
        format!("{}_{}", self.tag, suffix)
    }

    /// 清掉本用例造的全部夹具（只按 id 集合删，不碰兄弟用例）。
    pub(crate) async fn sweep(&self) {
        let reports = sweep_scope(&self.tag).await;
        let failed: Vec<String> = reports
            .iter()
            .flat_map(|report| report.failed.iter().cloned())
            .collect();
        if !failed.is_empty() {
            println!(
                "WARN[cleanup] 用例身份 {} 清理未完全成功：{failed:?}",
                self.tag
            );
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unregister_tag(&self.tag);
    }
}

/// 把 id 列表渲染成内联 uuid 字面量（id 来自库、且逐条 `Uuid::parse_str` 校验过，
/// 不存在注入面；空列表渲染成 `NULL` —— `IN (NULL)` 恒不命中，语义就是「什么都不删」）。
fn uuid_literals(ids: &[String]) -> String {
    let mut out = String::new();
    let mut first = true;
    for id in ids {
        if uuid::Uuid::parse_str(id).is_err() {
            continue;
        }
        if !first {
            out.push_str(", ");
        }
        out.push('\'');
        out.push_str(id);
        out.push_str("'::uuid");
        first = false;
    }
    if out.is_empty() {
        "NULL".to_string()
    } else {
        out
    }
}

/// 按身份列捞 id（两段式的第一段）。
async fn ids_like(table: &str, column: &str, tag: &str) -> Vec<String> {
    let sql = format!("SELECT id::text AS v FROM {table} WHERE {column} LIKE ?");
    match db()
        .find_with_params(&sql, vec![Value::from(format!("{tag}%"))])
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| row.get_typed::<Option<String>>("v").unwrap_or(None))
            .collect(),
        Err(error) => {
            println!("WARN[cleanup] 按身份查 id 失败（{table}.{column} LIKE '{tag}%'）：{error}");
            Vec::new()
        }
    }
}

/// 按 id 集合删一批实体行（两段式的第二段）。
///
/// 先整批 `DELETE ... WHERE id IN (...)`；整批失败时**退化逐行**，把影响面收敛到那一行，
/// 并把「被业务表外键钉住」的行软删（保留引用完整性）、其余失败如实计入报告。
async fn delete_by_ids(label: &str, table: &str, ids: &[String]) -> SweepReport {
    let mut report = SweepReport {
        label: format!("{label}({table} 按 id 删 {} 行)", ids.len()),
        ..SweepReport::default()
    };
    if ids.is_empty() {
        return report;
    }
    let sql = format!("DELETE FROM {table} WHERE id IN ({})", uuid_literals(ids));
    match db().execute_with_params(&sql, vec![]).await {
        Ok(rows) => {
            report.deleted = usize::try_from(rows).unwrap_or(0);
            return report;
        }
        Err(error) => {
            println!(
                "WARN[cleanup] {} 整批删除失败，退化逐行：{error}",
                report.label
            );
        }
    }
    for id in ids {
        let sql = format!("DELETE FROM {table} WHERE id = ?::text::uuid");
        match db()
            .execute_with_params(&sql, vec![Value::from(id.clone())])
            .await
        {
            Ok(_) => report.deleted += 1,
            Err(error) => {
                let text = error.to_string();
                if let Some(pinned_by) = business_pin(&text) {
                    let soft = format!(
                        "UPDATE {table} SET deleted_at = now() WHERE id = ?::text::uuid AND deleted_at IS NULL"
                    );
                    match db()
                        .execute_with_params(&soft, vec![Value::from(id.clone())])
                        .await
                    {
                        Ok(_) => {
                            println!(
                                "WARN[cleanup] {} 行 {id} 被业务表 {pinned_by} 引用，已软删",
                                report.label
                            );
                            report.kept_by_business.push(format!("{id}←{pinned_by}"));
                        }
                        Err(soft_error) => {
                            report.failed.push(format!("{id} 软删失败：{soft_error}"));
                        }
                    }
                    continue;
                }
                report.failed.push(format!("{id}: {text}"));
                println!(
                    "WARN[cleanup] {} 行 {id} 硬删失败（非业务表外键）：{text}",
                    report.label
                );
            }
        }
    }
    println!("[cleanup] {}", report.summary());
    report
}

/// 清单个用例身份造的全部夹具：**先按身份 SELECT id，再按 id 集合删**。
///
/// 依赖顺序：`sys_field_mask` / `sys_user_role` / `sys_role_menu` / `sys_role_dept` /
/// `sys_role_mutex` / `sys_user_preference`（引用行）→ 菜单 / 部门 / 角色 / 用户（实体行）。
/// 一张表都没有别人的行：身份是本用例唯一的。
pub(crate) async fn sweep_scope(tag: &str) -> Vec<SweepReport> {
    let users = ids_like("sys_user", "username", tag).await;
    let roles = ids_like("sys_role", "role_code", tag).await;
    let menus = ids_like("sys_menu", "name", tag).await;
    let depts = ids_like("sys_dept", "dept_code", tag).await;
    let masks = ids_like("sys_field_mask", "reason", tag).await;

    let (user_sql, role_sql, menu_sql, dept_sql, mask_sql) = (
        uuid_literals(&users),
        uuid_literals(&roles),
        uuid_literals(&menus),
        uuid_literals(&depts),
        uuid_literals(&masks),
    );

    // 引用行：按外键依赖顺序先清（顺序错会让实体行删不掉）
    let dependents = [
        (
            "sys_role_menu",
            format!(
                "DELETE FROM sys_role_menu WHERE role_id IN ({role_sql}) OR menu_id IN ({menu_sql})"
            ),
        ),
        (
            "sys_user_role",
            format!(
                "DELETE FROM sys_user_role WHERE user_id IN ({user_sql}) OR role_id IN ({role_sql})"
            ),
        ),
        (
            "sys_role_dept",
            format!(
                "DELETE FROM sys_role_dept WHERE role_id IN ({role_sql}) OR dept_id IN ({dept_sql})"
            ),
        ),
        (
            "sys_role_mutex",
            format!(
                "DELETE FROM sys_role_mutex WHERE role_a_id IN ({role_sql}) OR role_b_id IN ({role_sql})"
            ),
        ),
        (
            "sys_user_preference",
            format!("DELETE FROM sys_user_preference WHERE user_id IN ({user_sql})"),
        ),
        (
            "sys_field_mask",
            format!("DELETE FROM sys_field_mask WHERE id IN ({mask_sql})"),
        ),
    ];
    for (label, sql) in dependents {
        let rows = cleanup_sql(&format!("{tag}→{label}"), &sql, vec![]).await;
        if rows > 0 {
            println!("[cleanup] {tag} 先清引用行 {label}：{rows} 行");
        }
    }

    // 实体行：按 id 集合删（菜单/部门先于角色：`sys_role_menu` 已清，这里只是顺序稳妥）
    let mut reports = Vec::new();
    reports.push(delete_by_ids(tag, "sys_menu", &menus).await);
    reports.push(delete_by_ids(tag, "sys_dept", &depts).await);
    reports.push(delete_by_ids(tag, "sys_role", &roles).await);
    reports.push(delete_by_ids(tag, "sys_user", &users).await);
    reports
}

// ==================== 残留自检（跑完 system:: 必须为 0） ====================

/// 已知夹具前缀白名单（与卡片一致；大小写敏感，`~` 是 PostgreSQL 正则）。
///
/// `N01-` / `N11-` / `D03-` / `U12-` / `R07-` / `A01-` / `M02-` / `tc_` / `testc_`
pub(crate) const FIXTURE_PREFIX_REGEX: &str = "^([A-Z][0-9][0-9][-_]|tc_|testc_)";

/// 种子号段排除（`10000000-` / `20000000-` / `21000000-` …）。种子/内置行**永不删**。
pub(crate) const SEED_ID_EXCLUSION: &str = "id::text !~ '^[0-9]{2}000000-'";

/// 本进程「开始跑用例」的时刻（首次连库时钉住，往前留 60 秒缓冲以吸收时钟抖动）。
///
/// 自检用例只清理**早于这个时刻**的行：那些一定是上一轮/历史 run 留下的残留，
/// 不可能是本进程正在用的夹具 —— 即使有人并行跑同一套用例也不会误删。
pub(crate) fn suite_start() -> chrono::DateTime<chrono::Utc> {
    use std::sync::OnceLock;
    static START: OnceLock<chrono::DateTime<chrono::Utc>> = OnceLock::new();
    *START.get_or_init(|| chrono::Utc::now() - chrono::TimeDelta::seconds(60))
}

/// 本进程「开跑时刻」：**首次连库时**（[`ensure_db`]）取一次数据库服务器时钟的 `now()`，零缓冲。
///
/// # 与 [`suite_start`] 的分工（两者不能混用，缓冲方向相反）
///
/// - `suite_start` 服务**残留清扫**：往前留 60s，宁可少删，避免误删兄弟用例正在用的行；
/// - `run_epoch` 服务**全表审计型断言的判据范围**：`created_at >= run_epoch` 的行一定是
///   本次 run 期间造出来的夹具行（种子 / 迁移行永远早于进程启动），必须**排除**，
///   否则并行跑的兄弟用例（甚至别的模块，如 `system::auth::cache_tests` /
///   `system::session_tests` 也往 `sys_menu` 插临时行）的**中间态**会被审计断言当成
///   「库里有、源码里没有」的死码 —— 即 BUG-SYS-05 的假红。
///
/// 取**库时钟**而不是进程时钟，是为了免掉进程与 DB 之间的时钟差：`created_at` 的默认值
/// 也是库时钟，两边同一源才不会出现「夹具行看着早于起点」的边界抖动。
pub(crate) fn try_run_epoch() -> Option<chrono::DateTime<chrono::Utc>> {
    run_epoch_cell().get().copied()
}

/// 同 [`try_run_epoch`]；未初始化视为测试基建错误（审计断言会因此失去范围界定 ⇒ 必须炸，不能静默放宽）。
pub(crate) fn run_epoch() -> chrono::DateTime<chrono::Utc> {
    try_run_epoch()
        .expect("run_epoch 未初始化：必须先 ensure_db() 成功（它负责在首次连库时钉住 run 起点）")
}

fn run_epoch_cell() -> &'static std::sync::OnceLock<chrono::DateTime<chrono::Utc>> {
    static EPOCH: std::sync::OnceLock<chrono::DateTime<chrono::Utc>> = std::sync::OnceLock::new();
    &EPOCH
}

/// 在 [`ensure_db`] 里钉住 run 起点（幂等：`OnceLock` 只认第一次）。
async fn capture_run_epoch() {
    let sql = "SELECT now() AS v";
    match db().find_with_params(sql, vec![]).await {
        Ok(rows) => {
            let value = rows
                .first()
                .and_then(|row| row.get_typed::<chrono::DateTime<chrono::Utc>>("v").ok());
            match value {
                Some(epoch) => {
                    let _ = run_epoch_cell().set(epoch);
                }
                None => println!("WARN[run_epoch] SELECT now() 未返回可解析的时间戳"),
            }
        }
        Err(error) => println!("WARN[run_epoch] 取库时钟失败：{error}"),
    }
}

/// 残留明细：表名 → 行数（附 `id`/名称样例，便于人工定位）。
pub(crate) struct ResidueReport {
    pub per_table: Vec<(String, i64)>,
    pub samples: Vec<String>,
}

impl ResidueReport {
    pub(crate) fn to_string_lines(&self) -> String {
        let mut out = String::new();
        for (table, count) in &self.per_table {
            out.push_str(&format!("\n  {table}: {count}"));
        }
        if !self.samples.is_empty() {
            out.push_str(&format!("\n  样例: {}", self.samples.join(", ")));
        }
        out
    }
}

/// 排除「并行兄弟用例正在用」的行：身份仍登记在 [`active_registry`] 里的一律不算残留。
///
/// 口径没有放宽：用例一结束（`Fixture` Drop）身份即注销，它留下的行立刻重新纳入判据；
/// 只有**正在跑的那一刻**才排除，否则并行跑时「A 用例刚建好、还没清理」会被误判成泄漏。
fn active_identity_regex() -> String {
    let tags = active_tags();
    if tags.is_empty() {
        // 恒不命中（`~ '^$'` 只匹配空串）
        "^$".to_string()
    } else {
        format!("^({})", tags.join("|"))
    }
}

/// 按夹具白名单统计四张实体表的残留行（`created_at < before`，含软删行）。
pub(crate) async fn count_fixture_residue(before: chrono::DateTime<chrono::Utc>) -> ResidueReport {
    let mut report = ResidueReport {
        per_table: Vec::new(),
        samples: Vec::new(),
    };
    let active = active_identity_regex();
    for (table, column) in [
        ("sys_menu", "name"),
        ("sys_role", "role_code"),
        ("sys_user", "username"),
        ("sys_dept", "dept_code"),
    ] {
        let count_sql = format!(
            "SELECT count(*) AS v FROM {table} WHERE {column} ~ ? AND {column} !~ ? AND {SEED_ID_EXCLUSION} AND created_at < ?"
        );
        let count = db()
            .scalar_with_params(
                &count_sql,
                vec![
                    Value::from(FIXTURE_PREFIX_REGEX),
                    Value::from(active.clone()),
                    Value::from(before),
                ],
            )
            .await
            .unwrap_or_else(|error| panic!("统计残留失败：{error}\nSQL: {count_sql}"));
        report.per_table.push((table.to_string(), count));

        if count > 0 {
            let sample_sql = format!(
                "SELECT {column} AS v FROM {table} WHERE {column} ~ ? AND {column} !~ ? AND {SEED_ID_EXCLUSION} AND created_at < ? ORDER BY {column} LIMIT 5"
            );
            if let Ok(rows) = db()
                .find_with_params(
                    &sample_sql,
                    vec![
                        Value::from(FIXTURE_PREFIX_REGEX),
                        Value::from(active.clone()),
                        Value::from(before),
                    ],
                )
                .await
            {
                for row in rows {
                    if let Some(name) = row.get_typed::<Option<String>>("v").unwrap_or(None) {
                        report.samples.push(format!("{table}.{column}={name}"));
                    }
                }
            }
        }
    }
    report
}

/// 清掉**早于 `before`** 的夹具残留（按外键依赖顺序：引用行 → 实体行），返回明细。
///
/// 这是自检用例的「自愈」半步：历史上因为 panic / `let _ =` 攒下来的行，
/// 在清理逻辑修好之后能被一次性收干，而不是永远要人工 psql。
pub(crate) async fn purge_fixture_residue(
    before: chrono::DateTime<chrono::Utc>,
) -> Vec<SweepReport> {
    // 引用行：按依赖顺序先清（不限于本卡前缀 —— 只清指向夹具实体的引用）
    let dependent_sqls = [
        (
            "sys_role_menu",
            "DELETE FROM sys_role_menu WHERE menu_id IN (SELECT id FROM sys_menu WHERE name ~ ? AND created_at < ?) \
             OR role_id IN (SELECT id FROM sys_role WHERE role_code ~ ? AND created_at < ?)",
        ),
        (
            "sys_user_role",
            "DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username ~ ? AND created_at < ?) \
             OR role_id IN (SELECT id FROM sys_role WHERE role_code ~ ? AND created_at < ?)",
        ),
        (
            "sys_role_dept",
            "DELETE FROM sys_role_dept WHERE role_id IN (SELECT id FROM sys_role WHERE role_code ~ ? AND created_at < ?) \
             OR dept_id IN (SELECT id FROM sys_dept WHERE dept_code ~ ? AND created_at < ?)",
        ),
        (
            "sys_field_mask",
            "DELETE FROM sys_field_mask WHERE role_id IN (SELECT id FROM sys_role WHERE role_code ~ ? AND created_at < ?) \
             OR create_by IN (SELECT id FROM sys_user WHERE username ~ ? AND created_at < ?)",
        ),
        (
            "sys_role_mutex",
            "DELETE FROM sys_role_mutex WHERE role_a_id IN (SELECT id FROM sys_role WHERE role_code ~ ? AND created_at < ?) \
             OR role_b_id IN (SELECT id FROM sys_role WHERE role_code ~ ? AND created_at < ?)",
        ),
        (
            "sys_user_preference",
            "DELETE FROM sys_user_preference WHERE user_id IN (SELECT id FROM sys_user WHERE username ~ ? AND created_at < ?)",
        ),
    ];
    for (label, sql) in dependent_sqls {
        // SQL 里的占位符是成对的 `(前缀正则, 时间界)`：`?` #1=regex、#2=before、
        // #3=regex、#4=before …（成对出现，因为每条子查询都用同一套夹具条件）
        let arity = sql.matches('?').count();
        let params = (0..arity)
            .map(|index| {
                if index % 2 == 0 {
                    Value::from(FIXTURE_PREFIX_REGEX)
                } else {
                    Value::from(before)
                }
            })
            .collect::<Vec<_>>();
        cleanup_sql(&format!("purge_fixture_residue→{label}"), sql, params).await;
    }

    // 实体行：逐行删（一行被挡住不连累其余行）
    let mut reports = Vec::new();
    let entity_sql: [(&str, &str, &str); 4] = [
        ("sys_user", "username", "夹具用户"),
        ("sys_role", "role_code", "夹具角色"),
        ("sys_menu", "name", "夹具菜单"),
        ("sys_dept", "dept_code", "夹具部门"),
    ];
    for (table, column, label) in entity_sql {
        let ids_sql = format!(
            "SELECT id::text AS v FROM {table} WHERE {column} ~ ? AND {SEED_ID_EXCLUSION} AND created_at < ?"
        );
        let ids = db()
            .find_with_params(
                &ids_sql,
                vec![Value::from(FIXTURE_PREFIX_REGEX), Value::from(before)],
            )
            .await
            .unwrap_or_else(|error| panic!("查询残留 id 失败：{error}\nSQL: {ids_sql}"));
        let mut report = SweepReport {
            label: format!("{label}({table}, created_at < 上一轮起点)"),
            ..SweepReport::default()
        };
        for row in ids {
            let Some(id) = row.get_typed::<Option<String>>("v").unwrap_or(None) else {
                continue;
            };
            let sql = format!("DELETE FROM {table} WHERE id = ?::text::uuid");
            match db()
                .execute_with_params(&sql, vec![Value::from(id.clone())])
                .await
            {
                Ok(_) => report.deleted += 1,
                Err(error) => {
                    let text = error.to_string();
                    if let Some(pinned_by) = business_pin(&text) {
                        println!(
                            "WARN[cleanup] {table} 行 {id} 被业务表 {pinned_by} 引用，保留（硬删会破坏业务引用）"
                        );
                        report.kept_by_business.push(format!("{id}←{pinned_by}"));
                    } else {
                        println!("WARN[cleanup] {table} 行 {id} 硬删失败：{text}");
                        report.failed.push(format!("{id}: {text}"));
                    }
                }
            }
        }
        if report.deleted > 0 || !report.failed.is_empty() {
            println!("[cleanup] {}", report.summary());
        }
        reports.push(report);
    }
    reports
}

// ==================== 自检用例 ====================

#[cfg(test)]
mod self_check {
    use super::*;

    /// X00：**跑完 `system::` 之后，`cloudpivot_test` 里不得留下任何用例夹具**。
    ///
    /// # 这条用例守的是什么
    ///
    /// 夹具残留是「测试全绿但基线计数失真」的元凶：一致性扫描（死权限码 /
    /// 「注解有但菜单无」）会把残留菜单算进去，`cloudpivot_test` 的任何 COUNT
    /// 断言都会漂。残留的真实成因有三类，都被本卡收口：
    ///
    /// 1. 用例在 cleanup 之前 panic（并发 / 中断 / `connection closed`）——
    ///    本用例的「自愈」半步会把它们收干；
    /// 2. 删除次序不满足外键依赖（`sys_role_menu` → `sys_user_role` → 实体表）；
    /// 3. 清理 SQL 的错误被 `let _ =` 吞掉（本卡改成 `WARN[cleanup]` + SQL + 原文）。
    ///
    /// # 判定口径
    ///
    /// - **自愈**：清掉 `created_at < 本进程起点`（起点 = 首次连库时刻 - 60s）的残留，
    ///   这些一定是上一轮/历史 run 留下的，不可能是本进程正在用的夹具；
    /// - **判定**：`created_at < now - 5s` 的夹具行必须为 0 —— 同一次 run 里更早跑完的
    ///   用例若泄漏，也会在这里被抓到（**并行跑也成立**：仍在运行中的用例身份登记在
    ///   `active_registry` 里，被排除在残留判据之外，见 `active_identity_regex`）；
    /// - **唯一豁免**：仍被业务表（`biz_*`）引用的夹具用户。硬删会破坏别的卡的业务
    ///   数据引用完整性，只能软删；它们按行计入 `kept_by_business` 并在断言里**精确对账**
    ///   （用户残留数必须恰好等于豁免数，多一行都算泄漏）。
    #[tokio::test]
    async fn x00_suite_leaves_zero_fixture_residue() {
        if !ensure_db().await {
            return;
        }
        let start = suite_start();

        // ① 自愈：收干历史残留
        let healed = purge_fixture_residue(start).await;
        let mut healed_rows = 0usize;
        let mut kept_rows = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for report in &healed {
            healed_rows += report.deleted;
            kept_rows += report.kept_by_business.len();
            failures.extend(report.failed.iter().cloned());
        }
        println!("[X00] 历史残留自愈：硬删 {healed_rows} 行，业务外键保留 {kept_rows} 行");
        assert!(
            failures.is_empty(),
            "历史残留清理失败（清理 SQL 真出错了，不是残留本身）：{failures:#?}"
        );

        // ② 判定：5 秒以前造的行必须清零
        let cutoff = chrono::Utc::now() - chrono::TimeDelta::seconds(5);
        let residue = count_fixture_residue(cutoff).await;
        println!(
            "[X00] 残留计数（created_at < now-5s，白名单 {FIXTURE_PREFIX_REGEX}）：{}",
            residue.to_string_lines()
        );

        let count_of = |table: &str| -> i64 {
            residue
                .per_table
                .iter()
                .find(|(name, _)| name == table)
                .map(|(_, count)| *count)
                .unwrap_or(0)
        };

        assert_eq!(
            count_of("sys_menu"),
            0,
            "跑完 system:: 后菜单夹具残留必须为 0（残留会被一致性扫描算进基线）{}",
            residue.to_string_lines()
        );
        assert_eq!(
            count_of("sys_role"),
            0,
            "角色夹具残留必须为 0{}",
            residue.to_string_lines()
        );
        assert_eq!(
            count_of("sys_dept"),
            0,
            "部门夹具残留必须为 0{}",
            residue.to_string_lines()
        );
        assert_eq!(
            count_of("sys_user") as usize,
            kept_rows,
            "用户夹具残留必须只剩「被业务表钉住的 {kept_rows} 行」（精确对账，多一行都是泄漏）{}",
            residue.to_string_lines()
        );

        println!(
            "[X00] 夹具残留自检通过：菜单/角色/部门 0 行，用户 {} 行（全部被业务表引用，按设计保留）✓",
            kept_rows
        );
    }

    /// X01：清理 SQL 失败时**不 panic、也不谎报删成功**，且把失败原样打印出来。
    ///
    /// 这是本卡最核心的行为契约：`cleanup_table` / `cleanup_sql` 是「让残留可见」的
    /// 唯一出口。旧实现用 `let _ = db().execute_with_params(...)` 把 `DbError` 直接
    /// 丢掉——删失败和删成功在调用点完全无法区分，于是「测试全绿、库里一堆夹具」。
    ///
    /// 断言：
    /// 1. 对不存在的表执行清理 → 返回 0（**不能**说自己删了几行）；
    /// 2. 不 panic（清理失败不该掩盖真正的断言结果）；
    /// 3. 断言输出里有 `WARN[cleanup]`（`--nocapture` 时人工可验，这里是契约的机器化版本：
    ///    函数返回 0 就代表「这次清理确实没成功」）。
    #[tokio::test]
    async fn x01_cleanup_failure_is_reported_not_swallowed() {
        if !ensure_db().await {
            return;
        }
        let deleted = cleanup_sql(
            "X01 故意失败的清理",
            "DELETE FROM 这张表不存在 WHERE id = ?",
            vec![Value::from("x01")],
        )
        .await;
        assert_eq!(
            deleted, 0,
            "清理 SQL 失败时必须返回 0 行（谎报删成功会让残留永远看不见）"
        );
        println!("[X01] 清理失败已按 WARN 暴露（返回 0 行，不 panic）✓");
    }
}
