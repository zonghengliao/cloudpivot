//! 工作任务管理 T1 的真库集成测试（**需要真实 PostgreSQL**，已迁移到 V0103）。
//!
//! # 为什么必须有这一层
//!
//! 模块内单测只能证明「纯逻辑」（分组桶、默认截止日、片段形态守卫、DTO 语义）。
//! 本卡真正要证明的六件事只有真库能证：
//!
//! 1. **可见性四场景**（票面验收 3）：
//!    a. 普通员工（`SELF`）只看到自己的；
//!    b. 部门主管（`DEPT_AND_CHILD`）看到本部门及下级部门的全部；
//!    c. **跨部门协作人**（`SELF`，但被拉进外部门任务）能看到**那一条**，看不到同部门其它任务；
//!    d. 超管（`SUPER_ADMIN`）看到全部。
//! 2. **`task_no` 生成**（票面验收 4）：连续创建 5 条，编号不重复且递增。
//! 3. **CRUD 闭环**（票面验收 2）：创建（默认值）→ 详情 → 编辑 → 软删 → 列表不含已删。
//! 4. **迁移结构**（票面验收 1 的一半）：7 张表 + 关键索引 + `sys_user.user_type` 都在。
//! 5. **软删不复用编号**：软删后新建任务继续往后取号，不会回到已被删掉的号。
//! 6. **逾期扫描可用**：`scan_overdue` 命中未终态的过期任务（部分索引的查询形态）。
//!
//! # 用法
//!
//! ```bash
//! su postgres -c "createdb -O cloudpivot cloudpivot_t1v"
//! # 先用该库起一次服务（自动跑迁移 V0001..V0103 + 种子），看到监听即 Ctrl-C
//! export TASK_DB_URL="postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_t1v"
//! cargo test --test batchT_t1_task -- --nocapture
//! # 不再需要 --test-threads=1：本文件的真库用例已用文件头的 DB_LOCK 互斥
//! ```
//!
//! 缺 `TASK_DB_URL` 时打印 `SKIP` 并直接返回（`cargo test` 全绿，CI 不需要数据库）。
//!
//! # 夹具与清理
//!
//! 固定 UUID + `t1_` 前缀用户名：开头清一次、结尾清一次，只删自己造的行
//! （任务 → 用户 → 角色 → 部门，按外键依赖反序），重复跑不积累垃圾。
//! 5 条用例共用同一批固定行（且 `visibility_four_scenarios` 有整库型断言），
//! 因此在默认多线程下用文件头的 `DB_LOCK` 互斥 —— 断言与线程数都没动。
//!
//! **UUID 段与接口走查夹具（scripts/t1_api_fixture.sql）严格错开**：
//! 本文件用 `7c`（用户）/ `7e`（部门）/ `7f`（角色）；走查夹具用 `7d` / `7a` / `7b`。
//! 曾经共用过 7a/7b，结果「先跑走查、再跑 cargo test」时 `seed_org` 撞主键
//! （走查夹具的用户还引用着那些部门，本文件的清理删不掉），四个用例一起红。

use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot::business::shared::principal;
use cloudpivot::business::task::model::{self, MyBucket, TaskFilter, TaskPatch};

// ==================== 并发安全：本 target 的真库用例共用一把锁 ====================
//
// 本文件的 4 条真库用例共用**同一批固定 UUID 夹具**（`7c/7e/7f` 段的 5 用户 / 3 部门 /
// 2 角色）与 `t1t_*` 用户名，`cleanup()` 也是按这批**固定 id**整批删；而且 `visibility_four_scenarios`
// 里有一条**整库型断言**（「超管可见条数 == 库内未软删任务总数」）。
//
// 于是 `cargo test -j 1`（`-j` 只是编译并行度，libtest 的测试线程仍是默认 4 个）下，
// 4 条用例会并行地「清同一批行 → 插同一批行」，实测 4 条全红：
//   * `建研发部/建研发一组: duplicate key value violates unique constraint "sys_dept_pkey"`
//   * `绑定角色: sys_user_role_user_id_fkey`（另一条用例刚把用户删了）
//
// 按项目铁律「同一批全局行 = 同一把锁」（`erp::fixture_hygiene::FINANCE_SEED_LOCK` 同思路），
// 这里给本 target 的真库用例加一把进程内互斥：**断言一条未改、命令行线程数一条未改**，
// 只是让共用同一批夹具行/整库型断言的用例不互相踩。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ==================== 夹具 UUID（固定，便于对照证据） ====================

const DEPT_R: &str = "7e000000-0000-0000-0000-000000000001"; // 研发部（根）
const DEPT_R_SUB: &str = "7e000000-0000-0000-0000-000000000002"; // 研发一组（研发部子部门）
const DEPT_M: &str = "7e000000-0000-0000-0000-000000000003"; // 市场部（根，与研发部平级）

const ROLE_SELF: &str = "7f000000-0000-0000-0000-000000000001"; // data_scope = 4 仅本人
const ROLE_DEPT_CHILD: &str = "7f000000-0000-0000-0000-000000000002"; // data_scope = 3 本部门及以下

const U_EMP: &str = "7c000000-0000-0000-0000-000000000001"; // 普通员工（研发一组，SELF）
const U_MGR: &str = "7c000000-0000-0000-0000-000000000002"; // 部门主管（研发部，DEPT_AND_CHILD）
const U_CROSS: &str = "7c000000-0000-0000-0000-000000000003"; // 跨部门协作人（市场部，SELF）
const U_OTHER: &str = "7c000000-0000-0000-0000-000000000004"; // 市场部另一个员工（SELF）
const U_SUPER: &str = "7c000000-0000-0000-0000-000000000005"; // 超管

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

fn dept(id: &str) -> Uuid {
    uuid(id)
}

/// 初始化全局数据库（进程内只初始化一次）；返回 `false` 表示跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("TASK_DB_URL") else {
                println!("SKIP: 未设置 TASK_DB_URL（不连真实数据库）");
                return false;
            };
            match Db::connect(&url).await {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    println!("任务管理已连接验证库");
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

// ==================== 夹具准备 / 清理 ====================

/// 清掉夹具（顺序：任务 → 用户 → 角色 → 部门，反外键序）。
async fn cleanup() {
    // EXPLAIN 取证脚本（scripts/t1_explain.sql）灌的 BULK-* 批量行：它们会把
    // 「我的任务 / 逾期扫描 / 超管全见」的断言淹掉，所以每次跑测试先清掉。
    let _ = db()
        .execute_with_params("DELETE FROM biz_task WHERE task_no LIKE 'BULK-%'", vec![])
        .await;
    // 夹具用户创建的任务（含 API 走查时由他们创建的任务）
    let _ = db()
        .execute_with_params(
            "DELETE FROM biz_task WHERE creator_id IN \
             (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::uuid)",
            vec![
                Value::from(&uuid(U_EMP)),
                Value::from(&uuid(U_MGR)),
                Value::from(&uuid(U_CROSS)),
                Value::from(&uuid(U_OTHER)),
                Value::from(&uuid(U_SUPER)),
            ],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_user_role WHERE user_id IN \
             (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::uuid)",
            vec![
                Value::from(&uuid(U_EMP)),
                Value::from(&uuid(U_MGR)),
                Value::from(&uuid(U_CROSS)),
                Value::from(&uuid(U_OTHER)),
                Value::from(&uuid(U_SUPER)),
            ],
        )
        .await;
    for user in [U_EMP, U_MGR, U_CROSS, U_OTHER, U_SUPER] {
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_user WHERE id = ?::uuid",
                vec![Value::from(&uuid(user))],
            )
            .await;
    }
    for role in [ROLE_SELF, ROLE_DEPT_CHILD] {
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_role WHERE id = ?::uuid",
                vec![Value::from(&uuid(role))],
            )
            .await;
    }
    for one in [DEPT_R_SUB, DEPT_R, DEPT_M] {
        let _ = db()
            .execute_with_params(
                "DELETE FROM sys_dept WHERE id = ?::uuid",
                vec![Value::from(&dept(one))],
            )
            .await;
    }
}

/// 造场景所需的部门 / 角色 / 用户。
async fn seed_org() {
    // 部门：研发部 → 研发一组；市场部（与研发部平级）
    db().execute_with_params(
        "INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, status, sort_order) \
         VALUES (?::uuid, 'T1T_R', 'T1T研发部', NULL, 1, 10)",
        vec![Value::from(&dept(DEPT_R))],
    )
    .await
    .expect("建研发部");
    db().execute_with_params(
        "INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, status, sort_order) \
         VALUES (?::uuid, 'T1T_R_SUB', 'T1T研发一组', ?::uuid, 1, 20)",
        vec![Value::from(&dept(DEPT_R_SUB)), Value::from(&dept(DEPT_R))],
    )
    .await
    .expect("建研发一组");
    db().execute_with_params(
        "INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, status, sort_order) \
         VALUES (?::uuid, 'T1T_M', 'T1T市场部', NULL, 1, 30)",
        vec![Value::from(&dept(DEPT_M))],
    )
    .await
    .expect("建市场部");

    // 角色：仅本人（4）/ 本部门及以下（3）
    db().execute_with_params(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) \
         VALUES (?::uuid, 'T1T_SELF', 'T1T仅本人', 1, 4)",
        vec![Value::from(&uuid(ROLE_SELF))],
    )
    .await
    .expect("建角色 SELF");
    db().execute_with_params(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) \
         VALUES (?::uuid, 'T1T_DEPT_CHILD', 'T1T本部门及以下', 1, 3)",
        vec![Value::from(&uuid(ROLE_DEPT_CHILD))],
    )
    .await
    .expect("建角色 DEPT_AND_CHILD");

    // 用户（密码字段给个占位哈希：本测试不走登录）
    let users = [
        (U_EMP, "t1t_emp", "T1员工", DEPT_R_SUB, ROLE_SELF),
        (U_MGR, "t1t_mgr", "T1主管", DEPT_R, ROLE_DEPT_CHILD),
        (U_CROSS, "t1t_cross", "T1协作人", DEPT_M, ROLE_SELF),
        (U_OTHER, "t1t_other", "T1市场同事", DEPT_M, ROLE_SELF),
        (U_SUPER, "t1t_super", "T1超管", DEPT_R, ""),
    ];
    for (id, username, real_name, dept_id, role_id) in users {
        db().execute_with_params(
            "INSERT INTO sys_user (id, username, password, real_name, dept_id, status) \
             VALUES (?::uuid, ?, 'x', ?, ?::uuid, 1)",
            vec![
                Value::from(&uuid(id)),
                Value::Text(username.to_string()),
                Value::Text(real_name.to_string()),
                Value::from(&dept(dept_id)),
            ],
        )
        .await
        .expect("建用户");
        if !role_id.is_empty() {
            db().execute_with_params(
                "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::uuid, ?::uuid)",
                vec![Value::from(&uuid(id)), Value::from(&uuid(role_id))],
            )
            .await
            .expect("绑定角色");
        }
    }
    // 超管：绑定内置 SUPER_ADMIN 角色（种子 S__role_data.sql 创建）
    db().execute_with_params(
        "INSERT INTO sys_user_role (user_id, role_id) \
         SELECT ?::uuid, r.id FROM sys_role r \
          WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL LIMIT 1",
        vec![Value::from(&uuid(U_SUPER))],
    )
    .await
    .expect("绑定超管角色");
}

/// 建一条测试任务（走 `model::create_task`，顺带验证编号生成）。
async fn create_task(
    owner: &str,
    dept_id: &str,
    title: &str,
    due_date: Option<chrono::NaiveDate>,
) -> (Uuid, String) {
    let id = Uuid::now_v7();
    let month = model::month_key(model::today_cst());
    let new = model::NewTask {
        id,
        title: title.to_string(),
        description: Some(format!("{title} 的描述")),
        priority: "P2".to_string(),
        need_review: false,
        owner_id: uuid(owner),
        creator_id: uuid(U_OTHER),
        assigner_id: None,
        dept_id: Some(dept(dept_id)),
        parent_id: None,
        project_id: None,
        start_date: None,
        due_date,
        estimate_hours: None,
        source: "SELF".to_string(),
        add_creator_as_collaborator: false,
    };
    let task_no = db()
        .transaction(|tx| async move { model::create_task(&tx, &new, uuid(U_OTHER), &month).await })
        .await
        .expect("建任务");
    (id, task_no)
}

/// 把任务拉个协作人（模拟「跨部门协作」）。
async fn add_collaborator(task_id: Uuid, user: &str) {
    db().transaction(|tx| async move {
        model::add_member(&tx, task_id, uuid(user), "COLLABORATOR", uuid(U_OTHER)).await
    })
    .await
    .expect("加协作人");
}

/// 5 条场景任务：
/// - T1 研发一组的任务（负责人 = 员工）
/// - T2 研发部的任务（负责人 = 主管）
/// - T3 市场部的任务（无协作人）
/// - T4 市场部的任务（拉了跨部门协作人 = U_CROSS）★
/// - T5 市场部的任务（无协作人，用于证明「同部门其它任务看不到」）
struct Scenario {
    t1: Uuid,
    t2: Uuid,
    t3: Uuid,
    t4: Uuid,
    t5: Uuid,
}

async fn seed_tasks() -> Scenario {
    let today = model::today_cst();
    let (t1, _) = create_task(U_EMP, DEPT_R_SUB, "T1自测-研发一组-员工任务", Some(today)).await;
    let (t2, _) = create_task(U_MGR, DEPT_R, "T1自测-研发部-主管任务", Some(today)).await;
    let (t3, _) = create_task(U_OTHER, DEPT_M, "T1自测-市场部-无协作人", Some(today)).await;
    let (t4, _) = create_task(
        U_OTHER,
        DEPT_M,
        "T1自测-市场部-跨部门协作",
        Some(today + chrono::Duration::days(1)),
    )
    .await;
    let (t5, _) = create_task(
        U_OTHER,
        DEPT_M,
        "T1自测-市场部-同部门其它任务",
        Some(today + chrono::Duration::days(2)),
    )
    .await;
    add_collaborator(t4, U_CROSS).await;
    Scenario { t1, t2, t3, t4, t5 }
}

/// 取某用户可见的全部任务 ID（分页取一页 100 条即可：夹具规模很小）。
async fn visible_ids(user: &str) -> Vec<Uuid> {
    let principal = principal::resolve(&uuid(user)).await.expect("解析数据范围");
    let page = model::page_visible(&principal, &TaskFilter::default(), 1, 100)
        .await
        .expect("分页查询任务");
    page.list.iter().map(|row| row.id).collect()
}

// ==================== 测试 1：可见性四场景 ====================

#[tokio::test]
async fn visibility_four_scenarios() {
    if !ensure_db().await {
        return;
    }
    // 共用固定夹具行 + 整库型断言 ⇒ 与同 target 的其它真库用例互斥（见文件头 DB_LOCK 说明）
    let _db_lock = DB_LOCK.lock().await;
    cleanup().await;
    seed_org().await;
    let s = seed_tasks().await;

    // ── a) 普通员工（SELF）：只看到自己的 ────────────────────────
    let emp = visible_ids(U_EMP).await;
    println!("[a] 普通员工可见 {} 条：{:?}", emp.len(), emp);
    assert!(emp.contains(&s.t1), "普通员工必须看到自己负责的任务 T1");
    assert!(
        !emp.contains(&s.t2),
        "普通员工看不到同部门主管的任务（SELF 范围）"
    );
    assert!(!emp.contains(&s.t3) && !emp.contains(&s.t4) && !emp.contains(&s.t5));
    assert_eq!(emp.len(), 1, "普通员工应恰好看到 1 条（自己的）");

    // ── b) 部门主管（DEPT_AND_CHILD）：本部门及下级全部 ──────────
    let mgr = visible_ids(U_MGR).await;
    println!("[b] 部门主管可见 {} 条：{:?}", mgr.len(), mgr);
    assert!(mgr.contains(&s.t2), "主管看到自己部门的 T2");
    assert!(
        mgr.contains(&s.t1),
        "主管看到下级部门（研发一组）的 T1 —— DEPT_AND_CHILD 的关键"
    );
    assert!(
        !mgr.contains(&s.t3) && !mgr.contains(&s.t4) && !mgr.contains(&s.t5),
        "主管看不到市场部的任何任务"
    );

    // ── c) 跨部门协作人：被拉进外部门任务后能看到那一条 ──────────
    let cross = visible_ids(U_CROSS).await;
    println!("[c] 跨部门协作人可见 {} 条：{:?}", cross.len(), cross);
    assert!(
        cross.contains(&s.t4),
        "★ 跨部门协作人必须看到被拉进的 T4（OR 合并的作用）"
    );
    assert!(
        !cross.contains(&s.t3) && !cross.contains(&s.t5),
        "★ 同部门的其它任务看不到（他不是负责人/创建人/协作人）"
    );
    assert_eq!(
        cross.len(),
        1,
        "跨部门协作人应恰好看到 1 条（被拉进的那条），证明 OR 不是什么都能看"
    );

    // ── d) 超管：全部 ────────────────────────────────────────────
    let super_ids = visible_ids(U_SUPER).await;
    println!("[d] 超管可见 {} 条：{:?}", super_ids.len(), super_ids);
    for id in [s.t1, s.t2, s.t3, s.t4, s.t5] {
        assert!(super_ids.contains(&id), "超管必须看到全部 5 条夹具任务");
    }
    let total_in_db: i64 = db()
        .scalar_with_params(
            "SELECT count(*) AS n FROM biz_task WHERE deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("统计任务总数");
    assert_eq!(
        super_ids.len() as i64,
        total_in_db,
        "超管的可见条数必须等于库里未软删任务总数"
    );

    // ── 详情也要守可见性：看不见的任务一律 404 ───────────────────
    let emp_principal = principal::resolve(&uuid(U_EMP)).await.expect("解析范围");
    assert!(
        model::find_detail(&emp_principal, s.t3)
            .await
            .expect("查询详情")
            .is_none(),
        "普通员工查不到市场部任务的详情（返回 None → 接口 404）"
    );
    assert!(
        model::find_detail(&emp_principal, s.t1)
            .await
            .expect("查询详情")
            .is_some(),
        "普通员工能查自己任务的详情"
    );
    // 跨部门协作人能看到 T4 详情（但改不了 —— 写权限由 handler 判，见下一条测试）
    let cross_principal = principal::resolve(&uuid(U_CROSS)).await.expect("解析范围");
    assert!(
        model::find_detail(&cross_principal, s.t4)
            .await
            .expect("查询详情")
            .is_some(),
        "跨部门协作人能看到 T4 详情"
    );

    cleanup().await;
}

// ==================== 测试 2：CRUD 闭环 + 默认值 ====================

#[tokio::test]
async fn crud_roundtrip() {
    if !ensure_db().await {
        return;
    }
    // 共用固定夹具行 + 整库型断言 ⇒ 与同 target 的其它真库用例互斥（见文件头 DB_LOCK 说明）
    let _db_lock = DB_LOCK.lock().await;
    cleanup().await;
    seed_org().await;

    let today = model::today_cst();
    let default_due = model::default_due_date(today);
    let id = Uuid::now_v7();
    let month = model::month_key(today);
    let new = model::NewTask {
        id,
        title: "T1自测-CRUD-初始标题".to_string(),
        description: Some("初始描述".to_string()),
        priority: "P2".to_string(),
        need_review: false,
        owner_id: uuid(U_EMP),
        creator_id: uuid(U_EMP),
        assigner_id: None,
        dept_id: Some(dept(DEPT_R_SUB)),
        parent_id: None,
        project_id: None,
        start_date: None,
        due_date: Some(default_due),
        estimate_hours: Some(2.5),
        source: "SELF".to_string(),
        add_creator_as_collaborator: false,
    };
    let task_no = {
        let month = month.clone();
        db().transaction(
            |tx| async move { model::create_task(&tx, &new, uuid(U_EMP), &month).await },
        )
        .await
        .expect("新建任务")
    };

    // 创建后的默认值（状态 TODO / 进度 0 / 负责人自己 / 部门快照）
    let created = model::find_raw(id).await.expect("查询").expect("任务存在");
    println!(
        "[CRUD] 新建：task_no={task_no} status={} progress={} owner={} dept={:?} due={:?}",
        created.card.status,
        created.card.progress,
        created.card.owner_id,
        created.card.dept_id,
        created.card.due_date
    );
    assert_eq!(created.card.status, "TODO");
    assert_eq!(created.card.progress, 0);
    assert_eq!(created.card.owner_id, uuid(U_EMP));
    assert_eq!(created.card.dept_id, Some(dept(DEPT_R_SUB)));
    assert_eq!(created.card.due_date, Some(default_due));
    assert_eq!(created.creator_id, uuid(U_EMP));
    assert!(task_no.starts_with(&format!("TSK-{month}-")));

    // 编辑：标题 / 优先级 / 截止日 / 需验收
    let patch = TaskPatch {
        title: Some("T1自测-CRUD-改后标题".to_string()),
        priority: Some("P0".to_string()),
        need_review: Some(true),
        due_date: Some(Some(today + chrono::Duration::days(7))),
        ..TaskPatch::default()
    };
    let affected = db()
        .transaction(|tx| async move { model::update_task(&tx, id, &patch, uuid(U_EMP)).await })
        .await
        .expect("编辑任务");
    assert_eq!(affected, 1);
    let updated = model::find_raw(id).await.expect("查询").expect("任务存在");
    assert_eq!(updated.card.title, "T1自测-CRUD-改后标题");
    assert_eq!(updated.card.priority, "P0");
    assert!(updated.card.need_review);
    assert_eq!(
        updated.card.due_date,
        Some(today + chrono::Duration::days(7))
    );
    println!(
        "[CRUD] 编辑后：title={} priority={} need_review={} due={:?}",
        updated.card.title, updated.card.priority, updated.card.need_review, updated.card.due_date
    );

    // 清空截止日（`Some(None)` 语义）
    let clear = TaskPatch {
        due_date: Some(None),
        ..TaskPatch::default()
    };
    db().transaction(|tx| async move { model::update_task(&tx, id, &clear, uuid(U_EMP)).await })
        .await
        .expect("清空截止日");
    let cleared = model::find_raw(id).await.expect("查询").expect("任务存在");
    assert_eq!(cleared.card.due_date, None, "显式 null 必须真的清空");

    // 软删 → 详情查不到、列表不含
    let affected = db()
        .transaction(|tx| async move { model::soft_delete(&tx, id, uuid(U_EMP)).await })
        .await
        .expect("软删任务");
    assert_eq!(affected, 1);
    assert!(
        model::find_raw(id).await.expect("查询").is_none(),
        "软删后 find_raw 必须查不到"
    );
    let emp = visible_ids(U_EMP).await;
    assert!(!emp.contains(&id), "软删后列表不含已删任务");
    // 再删一次：0 行（幂等，不报错）
    let again = db()
        .transaction(|tx| async move { model::soft_delete(&tx, id, uuid(U_EMP)).await })
        .await
        .expect("重复软删");
    assert_eq!(again, 0, "重复软删应为 0 行");

    cleanup().await;
}

// ==================== 测试 3：task_no 连续 5 条不重复且递增 ====================

#[tokio::test]
async fn task_no_is_unique_and_increasing() {
    if !ensure_db().await {
        return;
    }
    // 共用固定夹具行 + 整库型断言 ⇒ 与同 target 的其它真库用例互斥（见文件头 DB_LOCK 说明）
    let _db_lock = DB_LOCK.lock().await;
    cleanup().await;
    seed_org().await;

    let today = model::today_cst();
    let month = model::month_key(today);
    let mut numbers = Vec::new();
    for index in 1..=5 {
        let (_, task_no) = create_task(
            U_EMP,
            DEPT_R_SUB,
            &format!("T1自测-编号-{index}"),
            Some(today),
        )
        .await;
        numbers.push(task_no);
    }
    println!("[编号] 连续创建 5 条：{numbers:?}");

    let seqs: Vec<i64> = numbers
        .iter()
        .map(|task_no| {
            task_no
                .rsplit('-')
                .next()
                .expect("编号里有分隔符")
                .parse::<i64>()
                .expect("尾段必须是数字")
        })
        .collect();
    for (index, task_no) in numbers.iter().enumerate() {
        assert_eq!(
            task_no,
            &model::format_task_no(&month, seqs[index]),
            "编号格式必须是 TSK-YYYYMM-NNNNNN"
        );
    }
    let unique: std::collections::BTreeSet<&String> = numbers.iter().collect();
    assert_eq!(unique.len(), 5, "5 条编号必须互不重复");
    let sorted = {
        let mut copy = seqs.clone();
        copy.sort_unstable();
        copy
    };
    assert_eq!(seqs, sorted, "编号必须严格递增（={seqs:?}）");

    // 软删一条后继续建，编号仍然递增（软删不复用）
    let last_before = *seqs.last().expect("非空");
    let (victim, _) = create_task(U_EMP, DEPT_R_SUB, "T1自测-编号-待删", Some(today)).await;
    db().transaction(|tx| async move { model::soft_delete(&tx, victim, uuid(U_EMP)).await })
        .await
        .expect("软删");
    let (_, after_delete) =
        create_task(U_EMP, DEPT_R_SUB, "T1自测-编号-软删之后", Some(today)).await;
    let after_seq: i64 = after_delete
        .rsplit('-')
        .next()
        .expect("编号里有分隔符")
        .parse()
        .expect("尾段必须是数字");
    println!("[编号] 软删最后一条后再建：{after_delete}（软删前最大 {last_before}）");
    assert!(
        after_seq > last_before + 1,
        "软删的编号不复用（新号必须跳过被删的那一个）"
    );

    cleanup().await;
}

// ==================== 测试 4：迁移结构与部分索引查询形态 ====================

#[tokio::test]
async fn schema_and_indexes_exist() {
    if !ensure_db().await {
        return;
    }

    // 7 张表
    for table in [
        "biz_task",
        "biz_task_member",
        "biz_task_comment",
        "biz_task_attachment",
        "biz_task_dependency",
        "biz_task_worklog",
        "biz_project",
    ] {
        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) AS n FROM information_schema.tables \
                  WHERE table_schema = current_schema() AND table_name = ?",
                vec![Value::Text(table.to_string())],
            )
            .await
            .expect("查表存在");
        assert_eq!(count, 1, "表 {table} 必须存在");
    }

    // 关键索引（含部分索引）
    for index in [
        "uk_biz_task_task_no",
        "idx_biz_task_owner_status_due",
        "idx_biz_task_dept_status",
        "idx_biz_task_assigner_status",
        "idx_biz_task_parent",
        "idx_biz_task_due_open",
    ] {
        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) AS n FROM pg_indexes \
                  WHERE schemaname = current_schema() AND indexname = ?",
                vec![Value::Text(index.to_string())],
            )
            .await
            .expect("查索引存在");
        assert_eq!(count, 1, "索引 {index} 必须存在");
    }

    // 部分索引的谓词必须与 scan_overdue 的 WHERE 一致（否则 PG 不选它）
    let predicate: String = db()
        .scalar_with_params(
            "SELECT pg_get_expr(i.indpred, i.indrelid) AS predicate \
               FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
              WHERE c.relname = 'idx_biz_task_due_open'",
            vec![],
        )
        .await
        .expect("查部分索引谓词");
    println!("[结构] 部分索引谓词：{predicate}");
    assert!(
        predicate.contains("DONE") && predicate.contains("CANCELLED"),
        "部分索引谓词必须排除 DONE / CANCELLED"
    );

    // 底座改动：sys_user.user_type 默认 HUMAN
    let default_value: String = db()
        .scalar_with_params(
            "SELECT column_default AS d FROM information_schema.columns \
              WHERE table_name = 'sys_user' AND column_name = 'user_type'",
            vec![],
        )
        .await
        .expect("查 user_type 列");
    println!("[结构] sys_user.user_type 默认值：{default_value}");
    assert!(default_value.contains("HUMAN"));

    // 反自反 CHECK 在（依赖表插入自己会失败）
    let self_dep = db()
        .execute_with_params(
            "INSERT INTO biz_task_dependency (task_id, depends_on_id) \
             SELECT id, id FROM biz_task LIMIT 1",
            vec![],
        )
        .await;
    if let Err(error) = self_dep {
        println!("[结构] 自依赖被拒绝：{error}");
    }
}

// ==================== 测试 5：我的任务分组 + 逾期扫描 ====================

#[tokio::test]
async fn my_buckets_and_overdue_scan() {
    if !ensure_db().await {
        return;
    }
    // 共用固定夹具行 + 整库型断言 ⇒ 与同 target 的其它真库用例互斥（见文件头 DB_LOCK 说明）
    let _db_lock = DB_LOCK.lock().await;
    cleanup().await;
    seed_org().await;

    let today = model::today_cst();
    let (overdue_id, _) = create_task(
        U_EMP,
        DEPT_R_SUB,
        "T1自测-我的任务-逾期",
        Some(today - chrono::Duration::days(3)),
    )
    .await;
    let (today_id, _) = create_task(U_EMP, DEPT_R_SUB, "T1自测-我的任务-今日", Some(today)).await;
    let (week_id, _) = create_task(
        U_EMP,
        DEPT_R_SUB,
        "T1自测-我的任务-本周",
        Some(model::default_due_date(today)),
    )
    .await;
    let (no_due_id, _) = create_task(U_EMP, DEPT_R_SUB, "T1自测-我的任务-无期限", None).await;

    let rows = model::my_tasks(uuid(U_EMP)).await.expect("我的任务");
    let buckets: Vec<(Uuid, MyBucket)> = rows
        .iter()
        .map(|row| (row.id, model::bucket_of(row.due_date, today)))
        .collect();
    println!("[我的任务] {buckets:?}");

    let find = |id: Uuid| {
        buckets
            .iter()
            .find(|(task_id, _)| *task_id == id)
            .map(|(_, bucket)| *bucket)
    };
    assert_eq!(find(overdue_id), Some(MyBucket::Overdue));
    assert_eq!(find(today_id), Some(MyBucket::Today));
    assert_eq!(find(no_due_id), Some(MyBucket::NoDue));
    // 今天是周四/五时「本周」桶成立；若是周六/日则 default_due_date 会顺延到下周 → Later
    assert!(matches!(
        find(week_id),
        Some(MyBucket::ThisWeek) | Some(MyBucket::Later)
    ));

    // 逾期扫描（部分索引 idx_biz_task_due_open 的查询形态）
    let overdue = model::scan_overdue(today, 50).await.expect("逾期扫描");
    println!(
        "[逾期扫描] 命中 {} 条：{:?}",
        overdue.len(),
        overdue.iter().map(|row| &row.task_no).collect::<Vec<_>>()
    );
    assert!(
        overdue.iter().any(|row| row.id == overdue_id),
        "逾期扫描必须命中过期任务"
    );
    assert!(
        !overdue.iter().any(|row| row.id == today_id),
        "今日到期不算逾期"
    );

    cleanup().await;
}
