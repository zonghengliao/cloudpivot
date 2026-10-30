//! E7 真库集成测试：**软删任务时清理依赖记录**（`ON DELETE CASCADE` 对软删不生效）。
//!
//! # 为什么必须有这一层
//!
//! `biz_task_dependency` 的两个外键都写了 `ON DELETE CASCADE`，但任务是软删
//! （`deleted_at`）——CASCADE 只在**物理**删除时触发。所以「软删 A / 软删 B 之后依赖行
//! 是否真的消失」这条验收，纯逻辑单测根本证不了（它们碰的不是 SQL）。本文件用真库证明四件事：
//!
//! 1. 建 A→B 依赖 → **软删 B（前置）** → 该依赖行消失；
//! 2. 建 A→B 依赖 → **软删 A（后继）** → 该依赖行消失；
//! 3. 软删**与依赖无关**的任务 → 别的依赖一行不少（不是「把整表删了」）；
//! 4. 启动自检 `count_orphan_task_dependencies()`：干净库为 0；绕过接口手工软删造成
//!    1 条孤儿时**恰好报 1**（WARN 分支的依据）。
//!
//! 另含两个边界：重复软删同一任务（幂等，`affected = 0` 且不报错）、无依赖任务软删（不影响他人）。
//!
//! # 用法
//!
//! ```bash
//! su postgres -c "createdb -O cloudpivot cloudpivot_e7v"
//! # 先用该库起一次服务（自动跑完迁移 + 种子），看到监听即 Ctrl-C
//! export E7_DB_URL="postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_e7v"
//! cargo test --test batchE_e7_dep_cleanup -- --nocapture --test-threads=1
//! ```
//!
//! 缺 `E7_DB_URL` 时打印 `SKIP` 直接返回（`cargo test` 全绿，CI 不需要数据库）。
//!
//! # 夹具与清理
//!
//! 固定 UUID（`8a`/`8b` 段，与 T1 的 `7c`/`7e`/`7f`、B7 的 `B7_` 前缀都错开）+ 任务号
//! `E7T-` 前缀：开头清一次、结尾清一次，只删自己造的行（依赖 → 任务 → 用户 → 部门反序）。

use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot::business::task::model;
use cloudpivot::shared::startup_guard;

// ==================== 夹具 UUID ====================

const DEPT: &str = "8a000000-0000-0000-0000-000000000001"; // E7 测试部
const USER: &str = "8b000000-0000-0000-0000-000000000001"; // E7 夹具用户（任务负责人 / 创建人 / 操作人）

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

/// 初始化全局数据库（进程内只初始化一次）；返回 `false` 表示跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("E7_DB_URL") else {
                println!("SKIP: 未设置 E7_DB_URL（不连真实数据库）");
                return false;
            };
            match Db::connect(&url).await {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    println!("E7 依赖清理已连接验证库");
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

/// 清掉夹具（顺序：依赖 → 任务 → 用户 → 部门，反外键序）。只删 `E7T-` / 固定 UUID 的行。
async fn cleanup() {
    let _ = db()
        .execute_with_params(
            "DELETE FROM biz_task_dependency d \
              WHERE EXISTS (SELECT 1 FROM biz_task t \
                             WHERE t.id = d.task_id AND t.task_no LIKE 'E7T-%') \
                 OR EXISTS (SELECT 1 FROM biz_task t \
                             WHERE t.id = d.depends_on_id AND t.task_no LIKE 'E7T-%')",
            vec![],
        )
        .await;
    let _ = db()
        .execute_with_params("DELETE FROM biz_task WHERE task_no LIKE 'E7T-%'", vec![])
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_user WHERE id = ?::uuid",
            vec![Value::from(&uuid(USER))],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_dept WHERE id = ?::uuid",
            vec![Value::from(&uuid(DEPT))],
        )
        .await;
}

/// 造一条部门 + 一个用户（本测试不走登录，密码给占位哈希）。
async fn seed_org() {
    db().execute_with_params(
        "INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, status, sort_order) \
         VALUES (?::uuid, 'E7T_D', 'E7T测试部', NULL, 1, 90)",
        vec![Value::from(&uuid(DEPT))],
    )
    .await
    .expect("建测试部");
    db().execute_with_params(
        "INSERT INTO sys_user (id, username, password, real_name, dept_id, status) \
         VALUES (?::uuid, 'e7t_user', 'x', 'E7夹具用户', ?::uuid, 1)",
        vec![Value::from(&uuid(USER)), Value::from(&uuid(DEPT))],
    )
    .await
    .expect("建夹具用户");
}

/// 建一条只带必填列的任务（`task_no` 用 `tag` 拼出 `E7T-<tag>`，便于按前缀清理）。
async fn create_task(tag: &str) -> Uuid {
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, dept_id) \
         VALUES (?::uuid, ?, ?, ?::uuid, ?::uuid, ?::uuid)",
        vec![
            Value::from(&id),
            Value::Text(format!("E7T-{tag}")),
            Value::Text(format!("E7测试任务-{tag}")),
            Value::from(&uuid(USER)),
            Value::from(&uuid(USER)),
            Value::from(&uuid(DEPT)),
        ],
    )
    .await
    .expect("建任务");
    id
}

/// 建一条 FS 依赖：`task` 依赖 `prerequisite`（库里一行 `(task_id=task, depends_on_id=prerequisite)`）。
async fn add_dependency(task: Uuid, prerequisite: Uuid) {
    db().transaction(|tx| async move {
        model::insert_dependency(&tx, task, prerequisite, 0, uuid(USER)).await
    })
    .await
    .expect("建依赖");
}

/// 走**生产路径**软删（`model::soft_delete`，handler 的 `delete` 就是包在事务里调它）。
async fn soft_delete(task: Uuid) -> u64 {
    db().transaction(|tx| async move { model::soft_delete(&tx, task, uuid(USER)).await })
        .await
        .expect("软删任务")
}

/// 指定依赖对在库里还有几行（0 / 1）。
async fn dependency_rows(task: Uuid, prerequisite: Uuid) -> i64 {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_task_dependency \
          WHERE task_id = ?::uuid AND depends_on_id = ?::uuid",
        vec![Value::from(&task), Value::from(&prerequisite)],
    )
    .await
    .expect("统计依赖行")
}

/// 夹具范围内的依赖总数（用于证明「只删自己那一对」）。
async fn total_fixture_dependencies() -> i64 {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_task_dependency d \
          WHERE EXISTS (SELECT 1 FROM biz_task t \
                         WHERE t.id = d.task_id AND t.task_no LIKE 'E7T-%')",
        vec![],
    )
    .await
    .expect("统计夹具依赖")
}

/// 验收四条 + 两个边界，一个用例串行跑完（`cargo test` 默认多线程会让共用夹具互相踩）。
#[tokio::test]
async fn soft_delete_purges_dependencies_and_guard_counts_orphans() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    seed_org().await;

    // 三对依赖：a→b（软删前置）、c→d（软删后继）、e→f（全程不动，作为对照）
    let a = create_task("A").await;
    let b = create_task("B").await;
    let c = create_task("C").await;
    let d = create_task("D").await;
    let e = create_task("E").await;
    let f = create_task("F").await;
    let g = create_task("G").await; // 无任何依赖
    add_dependency(a, b).await;
    add_dependency(c, d).await;
    add_dependency(e, f).await;
    assert_eq!(dependency_rows(a, b).await, 1, "前置检查：a→b 应已建立");
    assert_eq!(
        total_fixture_dependencies().await,
        3,
        "前置检查：应有 3 条依赖"
    );

    // ① 软删**前置** B → a→b 消失，另两条不受影响
    assert_eq!(soft_delete(b).await, 1, "软删 B 应命中 1 行");
    assert_eq!(dependency_rows(a, b).await, 0, "软删前置 B 后 a→b 必须消失");
    assert_eq!(dependency_rows(c, d).await, 1, "c→d 不应被牵连");
    assert_eq!(dependency_rows(e, f).await, 1, "e→f 不应被牵连");

    // ② 软删**后继** C → c→d 消失
    assert_eq!(soft_delete(c).await, 1, "软删 C 应命中 1 行");
    assert_eq!(dependency_rows(c, d).await, 0, "软删后继 C 后 c→d 必须消失");
    assert_eq!(dependency_rows(e, f).await, 1, "e→f 仍不受影响");

    // ③ 软删**与依赖无关**的任务 G → 一条依赖都不动
    assert_eq!(soft_delete(g).await, 1, "软删 G 应命中 1 行");
    assert_eq!(
        total_fixture_dependencies().await,
        1,
        "只剩 e→f 一条，说明清理没有误删别人的依赖"
    );

    // ④ 边界：重复软删同一个任务 → 幂等（affected = 0、不报错、不改动他人依赖）
    assert_eq!(soft_delete(b).await, 0, "重复软删应命中 0 行");
    assert_eq!(dependency_rows(e, f).await, 1, "重复软删不应动 e→f");

    // ⑤ 启动自检：此时全库无孤儿（e→f 两端都未软删）→ 必须为 0
    assert_eq!(
        startup_guard::count_orphan_task_dependencies()
            .await
            .expect("孤儿依赖自检"),
        0,
        "干净状态下孤儿依赖应为 0（0 条不告警）"
    );

    // ⑥ 造 1 条孤儿（模拟历史残留：绕过接口直接软删，CASCADE 不触发）→ 恰好报 1
    db().execute_with_params(
        "UPDATE biz_task SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&e)],
    )
    .await
    .expect("手工软删 E（模拟脏数据）");
    assert_eq!(
        startup_guard::count_orphan_task_dependencies()
            .await
            .expect("孤儿依赖自检"),
        1,
        "手工软删后应恰好检出 1 条孤儿依赖"
    );
    let warning = startup_guard::orphan_dependency_warning(1).expect("1 条必须告警");
    assert!(warning.contains('1'), "告警文案应点名数量：{warning}");

    cleanup().await;
}
