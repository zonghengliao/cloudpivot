// ============================================================================
// task_model_depth_tests.rs —— 父任务链深度校验
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use super::*;

/// 夹具用户（`biz_task.owner_id / creator_id` 是 NOT NULL + FK `sys_user`）。
const U_ID: &str = "7a8a0000-0000-4000-8000-0000000000a1";
/// 8 层链：`L[0]` 是顶层（第 1 层），`L[7]` 最深（第 8 层）。
const L: [&str; 8] = [
    "7a8a0000-0000-4000-8000-000000000101",
    "7a8a0000-0000-4000-8000-000000000102",
    "7a8a0000-0000-4000-8000-000000000103",
    "7a8a0000-0000-4000-8000-000000000104",
    "7a8a0000-0000-4000-8000-000000000105",
    "7a8a0000-0000-4000-8000-000000000106",
    "7a8a0000-0000-4000-8000-000000000107",
    "7a8a0000-0000-4000-8000-000000000108",
];
/// 用例里真建出来的顶层 / 第 3 层任务。
const NEW_ROOT: &str = "7a8a0000-0000-4000-8000-0000000000f1";
const NEW_L3: &str = "7a8a0000-0000-4000-8000-0000000000f3";
/// 独立的顶层任务（与 8 层链无关，用来做「不成环 → 应放行」的反向对照）。
const SOLO: &str = "7a8a0000-0000-4000-8000-0000000001e1";
/// 手工造的环：`CYC_A → CYC_B → CYC_A`（历史脏数据形态，任何接口都造不出来）。
const CYC_A: &str = "7a8a0000-0000-4000-8000-0000000001c1";
const CYC_B: &str = "7a8a0000-0000-4000-8000-0000000001c2";

/// 真库用例**串行**执行：共用一个库，夹具是固定 UUID，并行跑会互相删掉对方的行。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `TASK_DB_URL`，用例直接跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("TASK_DB_URL") else {
                println!("SKIP: 未设置 TASK_DB_URL（不连真实数据库）");
                return false;
            };
            if try_db().is_some() {
                return true;
            }
            // 每个 `#[tokio::test]` 都是**独立 runtime**，而 `db()` 是进程级全局池：
            // 池里空闲连接绑在「创建它的那个 runtime」上，runtime 一销毁就死，
            // 而 `is_available` 不会立刻翻转 ⇒ 下一个用例借到死连接（W1b 实测 36 条挂 5–7 条）。
            // 对策：把 `max_lifetime` / `idle_timeout` 设成 0，逼池每次新建连接。
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.max_lifetime = Some(std::time::Duration::ZERO);
            config.pool.idle_timeout = Some(std::time::Duration::ZERO);
            match Db::connect_with(&url, config).await {
                Ok(connection) => match set_db(connection) {
                    Ok(()) => true,
                    Err(err) => {
                        println!("SKIP: set_db 失败（{err}）");
                        false
                    }
                },
                Err(err) => {
                    println!("SKIP: TASK_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

/// 写一行夹具任务（`task_no` 唯一索引 ⇒ 每个夹具自己的号）。
async fn insert_task(id: Uuid, task_no: &str, parent: Option<Uuid>, me: Uuid) {
    let affected = db()
        .execute_with_params(
            "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, parent_id, \
                                   status, priority, progress, need_review) \
             VALUES (?::uuid, ?, ?, ?::uuid, ?::uuid, ?::uuid, 'TODO', 'P2', 0, false)",
            vec![
                Value::from(&id),
                Value::Text(task_no.to_string()),
                Value::Text(format!("T8A {task_no}")),
                Value::from(&me),
                Value::from(&me),
                Value::from(parent.as_ref()),
            ],
        )
        .await;
    if let Err(error) = affected {
        panic!("夹具写入失败（task_no={task_no}）：{error}");
    }
}

/// 拿锁 + 连库 + 重建夹具（幂等；每次跑都先删自己上次造的行）。
///
/// 返回的 guard 必须活到用例结束（`let _db = ...`），否则锁会被提前释放。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    let me = uuid(U_ID);
    db().execute_with_params(
        "INSERT INTO sys_user (id, username, password) \
         VALUES (?::uuid, 't8a_depth_user', 't8a-depth-not-a-real-hash') \
         ON CONFLICT DO NOTHING",
        vec![Value::from(&me)],
    )
    .await
    .expect("夹具用户写入失败");
    // 清上一次的夹具：先断开父指针（自引用 FK 不是 deferrable，直接 DELETE 会被拦）
    db().execute_with_params(
        "UPDATE biz_task SET parent_id = NULL WHERE creator_id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("夹具清理（断父指针）失败");
    db().execute_with_params(
        "DELETE FROM biz_task WHERE creator_id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("夹具清理失败");
    // 8 层链：L1（顶层）→ … → L8
    for (index, id) in L.iter().enumerate() {
        let parent = if index == 0 {
            None
        } else {
            Some(uuid(L[index - 1]))
        };
        insert_task(uuid(id), &format!("T8A-L{}", index + 1), parent, me).await;
    }
    // 独立顶层任务（不在任何链上）
    insert_task(uuid(SOLO), "T8A-SOLO", None, me).await;
    // 手工造的环：A 的父是 B、B 的父是 A（先建正常链再回填一条边）
    insert_task(uuid(CYC_A), "T8A-CYC-A", None, me).await;
    insert_task(uuid(CYC_B), "T8A-CYC-B", Some(uuid(CYC_A)), me).await;
    db().execute_with_params(
        "UPDATE biz_task SET parent_id = ?::uuid WHERE id = ?::uuid",
        vec![Value::from(&uuid(CYC_B)), Value::from(&uuid(CYC_A))],
    )
    .await
    .expect("夹具造环失败");
    Some(guard)
}

/// 跑一次祖先链查询，返回链长（含起点）。成环时返回 0（由用例自己判）。
async fn chain_of(id: Uuid) -> usize {
    let sql = ancestor_chain_sql();
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&id)])
        .await
        .expect("祖先链查询失败");
    let mut seen = std::collections::BTreeSet::new();
    let mut len = 0usize;
    for row in &rows {
        let node: Uuid = row.get_typed("id").expect("取 id");
        let depth: i64 = row.get_typed("depth").expect("取 depth");
        if !seen.insert(node) {
            return 0;
        }
        len = len.max(depth.max(1) as usize);
    }
    len
}

/// 走**真实的建任务路径**（`create_task`）落一行，返回任务号。
async fn create_via_model(id: Uuid, parent: Option<Uuid>, me: Uuid) -> String {
    let tx = db().begin().await.expect("开启事务失败");
    let new = NewTask {
        id,
        title: format!("T8A 新建 {id}"),
        description: None,
        priority: "P2".to_string(),
        need_review: false,
        owner_id: me,
        creator_id: me,
        assigner_id: None,
        dept_id: None,
        parent_id: parent,
        project_id: None,
        start_date: None,
        due_date: None,
        estimate_hours: None,
        source: "SELF".to_string(),
        add_creator_as_collaborator: false,
    };
    let task_no = create_task(&tx, &new, me, &month_key(today_cst()))
        .await
        .expect("建任务失败");
    tx.commit().await.expect("提交事务失败");
    task_no
}

// ---------- ① 第 1 层（parent_id = null）→ 成功 ----------
#[tokio::test]
async fn level_one_task_can_be_created() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let id = uuid(NEW_ROOT);
    let task_no = create_via_model(id, None, me).await;
    assert!(
        task_no.starts_with("TSK-"),
        "任务号形态应为 TSK-YYYYMM-NNNNNN：{task_no}"
    );
    let row = find_raw(id).await.expect("查任务失败").expect("任务应存在");
    assert!(
        row.card.parent_id.is_none(),
        "顶层任务的 parent_id 必须是 NULL"
    );
    assert_eq!(chain_of(id).await, 1, "顶层任务的祖先链长 = 1 层");
}

// ---------- ② 第 2 层（parent 是顶层）→ 成功 ----------
#[tokio::test]
async fn level_two_under_root_is_allowed() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    validate_parent(uuid(L[0]), None)
        .await
        .expect("顶层任务下建第 2 层应成功");
    assert_eq!(chain_of(uuid(L[1])).await, 2, "L2 的链长应为 2");
}

// ---------- ③ 第 3 层 → 成功（★ 本卡核心：以前被 63005 拒） ----------
#[tokio::test]
async fn level_three_is_allowed_and_really_created() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    validate_parent(uuid(L[1]), None)
        .await
        .expect("第 3 层以前被「最多 2 层」拒，现在必须放行");
    let id = uuid(NEW_L3);
    create_via_model(id, Some(uuid(L[1])), me).await;
    let row = find_raw(id)
        .await
        .expect("查任务失败")
        .expect("第 3 层任务应落库");
    assert_eq!(row.card.parent_id, Some(uuid(L[1])), "第 3 层的父应是 L2");
    assert_eq!(chain_of(id).await, 3, "新建的任务应为第 3 层");
}

// ---------- ④ 第 8 层 → 成功 ----------
#[tokio::test]
async fn level_eight_is_allowed() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    validate_parent(uuid(L[6]), None)
        .await
        .expect("第 8 层必须建得出来");
    assert_eq!(
        chain_of(uuid(L[7])).await,
        MAX_TASK_DEPTH,
        "L8 的链长应为 8"
    );
    assert_eq!(chain_of(uuid(L[0])).await, 1, "L1 的链长应为 1");
}

// ---------- ⑤ 第 9 层 → 400 + 63005 ----------
#[tokio::test]
async fn level_nine_is_rejected_with_63005() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let error = validate_parent(uuid(L[7]), None)
        .await
        .expect_err("第 9 层必须被拒");
    let message = format!("{error}");
    assert!(
        message.contains("最多 8 层"),
        "文案应写明 8 层上限：{message}"
    );
    assert!(
        message.contains("第 8 层"),
        "文案应点明父任务链已到第 8 层：{message}"
    );
    // handler 侧的口径：DbError::InvalidValue → 400 + TASK_PARENT_INVALID(63005)
    assert_eq!(crate::shared::error_code::TASK_PARENT_INVALID, 63005);
}

// ---------- ⑥ 自指（parent_id = 自己）→ 400 + 63005 ----------
#[tokio::test]
async fn self_parent_is_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = uuid(L[2]);
    let error = validate_parent(id, Some(id))
        .await
        .expect_err("自己当自己的父任务必须被拒");
    assert!(
        format!("{error}").contains("父任务不能是任务自己"),
        "文案不对：{error}"
    );
}

// ---------- ⑦ 成环（A→B，然后 B→A）→ 400 + 63005 ----------
#[tokio::test]
async fn cycle_edge_is_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 现状：B 的父是 A（L2 → L1）。把 A 的父设成 B ⇒ 挂上去就成环
    let error = validate_parent(uuid(L[1]), Some(uuid(L[0])))
        .await
        .expect_err("把父任务挂到自己的子任务下面必须被拒");
    assert!(format!("{error}").contains("会成环"), "文案不对：{error}");
    // 反向对照：把**独立顶层任务 SOLO** 挂到 L5 下面并不成环（SOLO 不在 L5 的祖先链里）
    // ⇒ 应放行。证明上一条拒绝是「因为成环」，而不是「只要 self_id 有值就拒」。
    validate_parent(uuid(L[4]), Some(uuid(SOLO)))
        .await
        .expect("SOLO 不在 L5 的祖先链里，挂上去不成环 → 应放行");
}

// ---------- ⑧ 祖先链已有环（手工造）→ 不死循环，报错退出 ----------
#[tokio::test]
async fn ancestor_chain_cycle_terminates_with_error() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // CYC_A → CYC_B → CYC_A（库里真的有环；handler 任何路径都造不出来，只能手工改库）
    assert_eq!(
        chain_of(uuid(CYC_A)).await,
        0,
        "夹具应是一条真环（链长判定为 0）"
    );
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        validate_parent(uuid(CYC_A), Some(uuid(L[0]))),
    )
    .await;
    let error = outcome
        .expect("祖先链有环时必须在 5s 内返回，不许死循环")
        .expect_err("祖先链有环必须报错，不能猜一个深度放行");
    assert!(
        format!("{error}").contains("成环"),
        "文案应说明成环：{error}"
    );
}

// ---------- ⑨ 不连库的结构守卫：祖先链只有一条递归 SQL ----------
#[test]
fn ancestor_chain_query_is_one_recursive_cte() {
    let sql = ancestor_chain_sql();
    assert!(
        sql.contains("WITH RECURSIVE"),
        "必须用 WITH RECURSIVE：{sql}"
    );
    assert_eq!(
        sql.matches("?::uuid").count(),
        1,
        "整条链只允许一个绑定参数（一次查询拿全链，不许 N+1）：{sql}"
    );
    assert!(
        sql.contains(&format!("depth < {}", MAX_TASK_DEPTH + 1)),
        "递归上限必须是 MAX_TASK_DEPTH + 1：{sql}"
    );
    assert!(!sql.contains(';'), "不许在一个参数里拼多条语句：{sql}");
    assert_eq!(MAX_TASK_DEPTH, 8, "老大拍板 8 层");
    assert_eq!(MAX_PARENT_DEPTH, MAX_TASK_DEPTH + 1, "向上重算留一层余量");
    // 查询是按 parent_id 逐级 JOIN 的（走 idx_biz_task_parent 这类索引），不是全表扫
    assert!(
        sql.contains("p.id = c.parent_id"),
        "递归步骤必须按 parent_id 收敛：{sql}"
    );
}

// ---------- ⑩ 父任务不存在 / 已软删 仍按原口径拒绝 ----------
#[tokio::test]
async fn missing_or_deleted_parent_is_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let ghost = uuid("7a8a0000-0000-4000-8000-00000000dead");
    let error = validate_parent(ghost, None)
        .await
        .expect_err("不存在的父任务必须被拒");
    assert!(
        format!("{error}").contains("父任务不存在"),
        "文案不对：{error}"
    );

    db().execute_with_params(
        "UPDATE biz_task SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&uuid(L[3]))],
    )
    .await
    .expect("软删夹具失败");
    let error = validate_parent(uuid(L[3]), None)
        .await
        .expect_err("已软删的父任务必须被拒");
    assert!(
        format!("{error}").contains("父任务已删除"),
        "文案不对：{error}"
    );
}
