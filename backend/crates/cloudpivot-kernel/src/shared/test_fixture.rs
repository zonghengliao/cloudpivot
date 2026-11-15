//! 夹具复用模板（P2-60）：**一次建池，多套测试共用**。
//!
//! ## 它解决什么问题
//!
//! `#[tokio::test]` 默认是 `current_thread` runtime：**每个用例一个 runtime，用例结束即销毁**。
//! 而 `DB`（`nik_web::resources::set_db`）是**进程级** `OnceLock`，整个 lib 套件只有一个池。
//! 于是历史形态是：第一个用例在自己的 runtime 里建池 → 用例结束 runtime 被 drop →
//! 池里的连接（tokio-postgres 的驱动任务挂在**建池时那个 runtime** 上）全部变成死连接 →
//! 第二个用例借到死连接直接报错（W1b 实测）。
//!
//! 当时的逃逸手法是**把池子寿命压成 0**（全仓 144 处，逐字如下）：
//!
//! ```ignore
//! let mut config = DatabaseConfig::from_url(&url);
//! config.pool.max_lifetime = Some(std::time::Duration::ZERO);   // 借出前判过期
//! config.pool.idle_timeout = Some(std::time::Duration::ZERO);
//! match Db::connect_with(&url, config).await { … }
//! ```
//!
//! `max_lifetime = 0` ⇒ `is_available()` 恒假 ⇒ **每条语句都新建一条 TCP + 认证**。
//! 99 连 145 的 PG 单次握手 1~3 ms，5552 条 lib 用例的每条 SQL 都付一次：
//! 全量 lib 套件实测 5000 条用例耗时 **2 500 s 量级**，几乎全部是连接握手。
//!
//! ## 模板做法
//!
//! 把**建池**这一步放到一个**进程级后台 runtime**（[`fixture_runtime`]）上执行：
//! 连接任务从此挂在那条永不退出的 runtime 上，测试自己的 runtime 随便建随便销毁，
//! 借出的连接**始终是活的**。于是池参数可以回到框架默认（`max_lifetime=1800s`、
//! `idle_timeout=600s`），连接**真正被复用**（语句级零握手，还带 stmt 缓存）。
//!
//! 调用方（144 处 `ensure_db()`）只需要把三行换成一行：
//!
//! ```ignore
//! match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
//!     Ok(connection) => set_db(connection).is_ok(),
//!     Err(err) => {
//!         println!("SKIP: ERP_DB_URL 连接失败（{err}）");
//!         false
//!     }
//! }
//! ```
//!
//! 另有 `config.pool.<其他字段>` 需求时用 [`pool_config`] + [`connect_with`]（全仓仅 1 处）。
//!
//! ## 库侧同一件事：**建一次库，多套测试共用**
//!
//! 「建一次库」在 SQL 层的对应物是 `CREATE DATABASE … TEMPLATE cloudpivot_test`（秒级克隆
//! 181 表模板库）。`scripts/verify/fixture_template_suite.sh` 把它固化：**克隆一次**，
//! 然后让 N 套测试（按域 / 按模块过滤）**串在同一份库上**跑，逐套打点耗时；
//! 库状态由各套自己的夹具负责回到空库口径（见 `business::erp::fixture_hygiene` 与
//! `business::hr::fixture_hygiene`）。
//!
//! ## 条件编译
//!
//! 与 [`super::test_helpers`] 同理：本模块是**测试专用**代码，被域包（`app`）在测试配置下
//! 使用，所以是 `#[cfg(any(test, feature = "test-support"))]`（见 `shared/mod.rs`），
//! 生产构建里符号不存在。

use std::sync::OnceLock;

use nik::__db_core::error::DbError;
use nik::prelude::*;

/// 夹具专用后台 runtime（进程内唯一，永不销毁）。
///
/// 存在的唯一理由：**建池动作必须发生在一个活得比测试 runtime 更久的 runtime 上**。
/// `tokio-postgres`（以及 nik 的 PG 驱动）在建连时把驱动任务 spawn 到**当前** runtime；
/// 测试用例的 runtime 一结束，那条连接就再也没人 poll 了。放到这里，连接任务的生命周期
/// 与进程一致，任何测试 runtime 借到的都是活连接。
///
/// worker 数取 2：夹具池 `min_connections=1`，语料量级的并发借出都在测试 runtime 侧等待，
/// 2 条 worker 足够驱动连接 I/O，又不会和 `--test-threads=1` 抢 CPU。
fn fixture_runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("cp-test-fixture-db")
            .enable_all()
            .build()
            .expect("夹具后台 runtime 建不起来（进程级资源耗尽？）")
    })
}

/// 夹具池参数：框架默认（`max_connections=10` / `min_connections=1` /
/// `connect_timeout=30s` / `idle_timeout=600s` / `max_lifetime=1800s`）。
///
/// **不要**再把它压成 `ZERO`：那等于「每条语句一条新连接」，见模块文档。
/// 需要额外池参数时在返回值上改，然后交给 [`connect_with`]。
pub fn pool_config(url: &str) -> DatabaseConfig {
    DatabaseConfig::from_url(url)
}

/// 用夹具池参数建池（绝大多数调用方用这个）。
///
/// # 错误
///
/// 连接失败 / 认证失败 / 后台 runtime 任务异常时返回 [`DbError`]，调用方按既有约定
/// 打印 `SKIP:`（或断言失败）即可，语义与直接 `Db::connect_with` 一致。
pub async fn connect(url: &str) -> DbResult<Db> {
    connect_with(url, pool_config(url)).await
}

/// 建池（自定义池参数）。
///
/// 建池动作被搬到 [`fixture_runtime`] 上执行，返回值是普通的 `Db` 句柄：
/// 调用方可以照旧 `set_db(db)` / 直接在任意 runtime 里用。
pub async fn connect_with(url: &str, config: DatabaseConfig) -> DbResult<Db> {
    let url = url.to_string();
    let joined = fixture_runtime()
        .spawn(async move { Db::connect_with(&url, config).await })
        .await;
    match joined {
        Ok(result) => result,
        // 后台 runtime 被 panic / 关闭时任务 join 失败：按连接错误上报，别伪装成业务失败。
        Err(err) => Err(DbError::Connection(
            format!("夹具后台 runtime 建池任务 join 失败：{err}").into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 夹具池的库：`ERP_DB_URL` 优先（ERP 用例的主库），退回 `SYS_DB_URL`。
    fn db_url() -> Option<String> {
        std::env::var("ERP_DB_URL")
            .or_else(|_| std::env::var("SYS_DB_URL"))
            .ok()
            .filter(|url| !url.is_empty())
    }

    fn current_backend_pid(db: &Db, rt: &tokio::runtime::Runtime) -> i64 {
        rt.block_on(async {
            db.scalar::<i64>("SELECT pg_backend_pid()::bigint")
                .await
                .expect("查 pg_backend_pid 失败")
        })
    }

    /// 模板的**唯一硬约束**，三条断言各自对应一类真实故障：
    ///
    /// 1. 池在 runtime #1 上建、在 runtime #1 里查 → 拿到 pid；
    /// 2. **runtime #1 被 drop 之后**，runtime #2 用同一个池查 → 必须成功（否则就是
    ///    「借到上一个 runtime 的死连接」，历史 `max_lifetime=ZERO` 的成因）；
    /// 3. 同一 runtime 里连查两次 → **backend pid 必须相同**，即连接被复用
    ///    （若有人把 `max_lifetime/idle_timeout` 又改回 ZERO，这条立刻红）。
    #[test]
    fn t01_pool_survives_runtime_swap_and_reuses_connections() {
        let Some(url) = db_url() else {
            println!("SKIP: 未设置 ERP_DB_URL / SYS_DB_URL（模板用例需要真库）");
            return;
        };

        // —— runtime #1：模拟第一条 #[tokio::test]
        let rt1 = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime #1");
        let db = rt1
            .block_on(connect(&url))
            .unwrap_or_else(|err| panic!("夹具建池失败：{err}"));
        let pid_before = current_backend_pid(&db, &rt1);
        drop(rt1); // 用例结束：runtime 销毁（连接任务若挂在这里就死了）

        // —— runtime #2：模拟下一条 #[tokio::test]，同一个进程级池
        let rt2 = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime #2");
        let pid_after = current_backend_pid(&db, &rt2);
        let pid_again = current_backend_pid(&db, &rt2);

        assert_eq!(
            pid_before, pid_after,
            "池里的连接必须活过第一个测试 runtime：第一条 runtime 里是 {pid_before}，\
             换 runtime 后变成 {pid_after} ⇒ 说明借到了别的连接（连接被重建 / 死连接被换掉）"
        );
        assert_eq!(
            pid_after, pid_again,
            "同一 runtime 内连续两次取用必须复用同一条连接（{pid_after} vs {pid_again}）\
             —— 不等说明池又被配成 max_lifetime/idle_timeout = ZERO"
        );
    }
}
