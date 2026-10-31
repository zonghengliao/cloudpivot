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
            if try_db().is_some() {
                return true;
            }
            match connect_in_dedicated_runtime(&url) {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
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

/// 清理本次用例造的数据：按 `like` 前缀删（`sys_dept` / `sys_user` / `sys_role` 等）。
///
/// 用前缀而不是「全表删除」：`cloudpivot_test` 是多卡共用的验证库，
/// 全局 `DELETE` / `COUNT(*)` 都会把别的卡的数据卷进来（卡片硬红线 6）。
pub(crate) async fn cleanup_table(table: &str, column: &str, prefix: &str) {
    let sql = format!("DELETE FROM {table} WHERE {column} LIKE ?");
    let _ = db()
        .execute_with_params(&sql, vec![Value::from(format!("{prefix}%"))])
        .await;
}
