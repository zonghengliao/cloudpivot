// ============================================================================
// task_model_root_only_tests.rs —— 列表 root_only
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;

use super::*;

/// 夹具用户（`biz_task.owner_id / creator_id` 是 NOT NULL + FK `sys_user`）。
const U_ID: &str = "7a9c0000-0000-4000-8000-0000000000a1";
/// 顶层 R1（带 3 个子）/ 顶层 R2（独立）。
const R1: &str = "7a9c0000-0000-4000-8000-000000000101";
const R2: &str = "7a9c0000-0000-4000-8000-000000000102";
/// R1 的 3 个子任务。
const C1: &str = "7a9c0000-0000-4000-8000-000000000201";
const C2: &str = "7a9c0000-0000-4000-8000-000000000202";
const C3: &str = "7a9c0000-0000-4000-8000-000000000203";

/// 真库用例**串行**执行（共用一个库 + 固定 UUID 夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `TASK_DB_URL`，用例直接跳过。
///
/// 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级池 —— 池里的空闲连接绑在
/// 创建它的那个 runtime 上，`max_lifetime` / `idle_timeout` 设 0 逼池每次新建（W1b 实测）。
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
async fn insert_task(id: Uuid, task_no: &str, parent: Option<Uuid>, status: &str, me: Uuid) {
    let affected = db()
        .execute_with_params(
            "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, parent_id, \
                                   status, priority, progress, need_review) \
             VALUES (?::uuid, ?, ?, ?::uuid, ?::uuid, ?::uuid, ?, 'P2', 0, false)",
            vec![
                Value::from(&id),
                Value::Text(task_no.to_string()),
                Value::Text(format!("T8C {task_no}")),
                Value::from(&me),
                Value::from(&me),
                Value::from(parent.as_ref()),
                Value::Text(status.to_string()),
            ],
        )
        .await;
    if let Err(error) = affected {
        panic!("夹具写入失败（task_no={task_no}）：{error}");
    }
}

/// 拿锁 + 连库 + 重建夹具（幂等；每次跑都先删自己上次造的行）。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    let me = uuid(U_ID);
    db().execute_with_params(
        "INSERT INTO sys_user (id, username, password) \
         VALUES (?::uuid, 't8c_root_only_user', 't8c-not-a-real-hash') \
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
    insert_task(uuid(R1), "T8C-R1", None, "TODO", me).await;
    insert_task(uuid(R2), "T8C-R2", None, "DONE", me).await;
    insert_task(uuid(C1), "T8C-C1", Some(uuid(R1)), "DOING", me).await;
    insert_task(uuid(C2), "T8C-C2", Some(uuid(R1)), "TODO", me).await;
    insert_task(uuid(C3), "T8C-C3", Some(uuid(R1)), "TODO", me).await;
    Some(guard)
}

/// 全可见范围（超管 + `All`）：让用例只验证 `root_only` 本身，不掺杂数据权限。
fn principal() -> ScopedPrincipal {
    ScopedPrincipal::for_test(
        uuid(U_ID),
        None,
        true,
        scope::ScopeKind::All,
        Vec::new(),
        false,
    )
}

/// 夹具关键词：所有夹具标题都以 `T8C ` 开头、任务号都以 `T8C-` 开头。
///
/// 每次查询都带上它 —— 全量套件里其它模块的夹具也住在同一个库，不带关键词时
/// 「total 应该是 5」会随别人的夹具数量漂移（本卡第一次跑全量就挂在这里）。
const FIXTURE_KEYWORD: &str = "T8C-";

fn filter(root_only: bool) -> TaskFilter {
    TaskFilter {
        root_only,
        keyword: Some(FIXTURE_KEYWORD.to_string()),
        ..TaskFilter::default()
    }
}

// ---------- ① 无参数片段：不新增绑定参数，也不改变 `?` 的个数 ----------
/// `root_only` 只追加一个**常量** SQL 片段。
///
/// 这是本卡最容易被忽略的红线：`scope::paginate` 把数据权限片段参数**恒前置**，
/// 若这里 push 一个 `Value`，`$1/$2` 的位置就会整体错一位 —— 静默绑错值。
#[test]
fn root_only_snippet_adds_no_bind_params() {
    // 纯逻辑用例：直接构造「无任何过滤」的 TaskFilter（不带夹具关键词）
    let mut params_off = Vec::new();
    let sql_off = build_filters(&TaskFilter::default(), &mut params_off);
    assert_eq!(sql_off, "", "root_only=false 不该产生任何 WHERE 片段");
    assert!(params_off.is_empty());

    let mut params_on = Vec::new();
    let only_root = TaskFilter {
        root_only: true,
        ..TaskFilter::default()
    };
    let sql_on = build_filters(&only_root, &mut params_on);
    assert_eq!(sql_on, " AND t.parent_id IS NULL");
    assert!(
        params_on.is_empty(),
        "root_only 是无参数片段，不许 push 任何绑定值（实际 {} 个）",
        params_on.len()
    );
    assert_eq!(
        sql_on.matches('?').count(),
        0,
        "片段里不许有占位符：{sql_on}"
    );
}

// ---------- ② 片段追加在业务过滤之后：参数顺序与文本顺序仍一致 ----------
/// `status`（1 个参数）+ `keyword`（2 个参数）+ `root_only`（0 个参数）。
///
/// 断言三件事：参数总数不变、片段位置在最末（与 `?` 文本顺序一致）、
/// 关掉 `root_only` 后 SQL 只是少掉尾部那一段（前缀逐字相同）。
#[test]
fn root_only_snippet_keeps_param_order() {
    let base = TaskFilter {
        status: Some("TODO".to_string()),
        keyword: Some("T8C".to_string()),
        ..TaskFilter::default()
    };
    let mut params_on = Vec::new();
    let mut on = base.clone();
    on.root_only = true;
    let sql_on = build_filters(&on, &mut params_on);

    let mut params_off = Vec::new();
    let mut off = base.clone();
    off.root_only = false;
    let sql_off = build_filters(&off, &mut params_off);

    assert_eq!(
        params_on.len(),
        3,
        "status 1 + keyword 2 = 3，root_only 不占位"
    );
    assert_eq!(params_off.len(), 3);
    assert_eq!(
        format!("{params_on:?}"),
        format!("{params_off:?}"),
        "开了 root_only 之后绑定值列表必须逐项不变（顺序也不能变）"
    );
    assert_eq!(
        sql_on.matches('?').count(),
        sql_off.matches('?').count(),
        "占位符个数也不许变"
    );

    let pos_status = sql_on.find(" AND t.status = ?").expect("status 片段");
    let pos_keyword = sql_on.find(" AND (t.title ILIKE ?").expect("keyword 片段");
    let pos_root = sql_on
        .find(" AND t.parent_id IS NULL")
        .expect("root_only 片段");
    assert!(
        pos_status < pos_keyword && pos_keyword < pos_root,
        "root_only 必须追加在业务过滤之后（否则 `?` 文本顺序对不上参数顺序）：{sql_on}"
    );
    assert_eq!(
        sql_on,
        format!("{sql_off} AND t.parent_id IS NULL"),
        "开关只影响尾部那一个片段"
    );
}

// ---------- ③ 真库：list 只含顶层 ----------
#[tokio::test]
async fn root_only_lists_top_level_rows_only() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let page = page_visible(&principal(), &filter(true), 1, 50)
        .await
        .expect("分页查询失败");
    let mut ids = page.list.iter().map(|row| row.id).collect::<Vec<_>>();
    ids.sort();
    let mut expected = vec![uuid(R1), uuid(R2)];
    expected.sort();
    assert_eq!(ids, expected, "只应返回两个顶层任务（R1 / R2）");
    assert!(
        page.list.iter().all(|row| row.parent_id.is_none()),
        "顶层列表里不该出现任何 parent_id 有值的行"
    );
    // R1 的 child_count 仍要如实下发（前端据此渲染「有子任务」的箭头 + 懒加载）
    let r1 = page.list.iter().find(|row| row.id == uuid(R1)).expect("R1");
    assert_eq!(r1.child_count, 3, "child_count 不能因为 root_only 变成 0");
}

// ---------- ④ 真库：total 与 list 同口径 ----------
#[tokio::test]
async fn root_only_total_counts_top_level_only() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let roots = page_visible(&principal(), &filter(true), 1, 50)
        .await
        .expect("分页查询失败");
    let all = page_visible(&principal(), &filter(false), 1, 50)
        .await
        .expect("分页查询失败");
    assert_eq!(roots.total, 2, "顶层只有 R1 / R2");
    assert_eq!(all.total, 5, "全部 5 条（2 顶层 + 3 子）");
    assert_eq!(
        roots.total as usize,
        roots.list.len(),
        "total 必须与 list 同口径（否则分页器会多出空白页）"
    );
    assert_eq!(all.total as usize, all.list.len());
}

// ---------- ⑤ 真库：不传 / false → 全部（向后兼容） ----------
#[tokio::test]
async fn root_only_absent_is_backward_compatible() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 「不传 root_only」= TaskFilter 里 root_only 取缺省值 false（handler 用 unwrap_or(false)）
    let unset_filter = TaskFilter {
        keyword: Some(FIXTURE_KEYWORD.to_string()),
        ..TaskFilter::default()
    };
    let unset = page_visible(&principal(), &unset_filter, 1, 50)
        .await
        .expect("分页查询失败");
    let explicit_false = page_visible(&principal(), &filter(false), 1, 50)
        .await
        .expect("分页查询失败");
    assert_eq!(unset.total, 5, "不传 → 返回全部（T8-B 之前的行为）");
    assert_eq!(explicit_false.total, 5, "显式 false → 与不传完全一致");
    let children = unset
        .list
        .iter()
        .filter(|row| row.parent_id.is_some())
        .count();
    assert_eq!(children, 3, "不带 root_only 时子任务照旧出现在列表里");
}

// ---------- ⑥ 真库：keyword 在顶层里搜 ----------
#[tokio::test]
async fn root_only_keyword_searches_within_top_level() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let hit_root = TaskFilter {
        keyword: Some("T8C-R1".to_string()),
        root_only: true,
        ..TaskFilter::default()
    };
    let page = page_visible(&principal(), &hit_root, 1, 50)
        .await
        .expect("分页查询失败");
    assert_eq!(page.total, 1, "关键词命中顶层 R1");
    assert_eq!(page.list.len(), 1);
    assert_eq!(page.list[0].id, uuid(R1));

    // 关键词只命中**子任务**标题 → 顶层集合里没有 → 0 条（证明是在顶层里搜，不是搜完再拼父）
    let hit_child = TaskFilter {
        keyword: Some("T8C-C1".to_string()),
        root_only: true,
        ..TaskFilter::default()
    };
    let page = page_visible(&principal(), &hit_child, 1, 50)
        .await
        .expect("分页查询失败");
    assert_eq!(page.total, 0, "子任务命中不该把父任务带出来");
    assert!(page.list.is_empty());
}

// ---------- ⑦ 真库：status 在顶层里筛 ----------
#[tokio::test]
async fn root_only_status_filters_within_top_level() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 夹具状态：R1=TODO、R2=DONE、C1=DOING、C2=TODO、C3=TODO
    let todo_roots = TaskFilter {
        status: Some("TODO".to_string()),
        root_only: true,
        keyword: Some(FIXTURE_KEYWORD.to_string()),
        ..TaskFilter::default()
    };
    let page = page_visible(&principal(), &todo_roots, 1, 50)
        .await
        .expect("分页查询失败");
    assert_eq!(
        page.total, 1,
        "顶层里只有 R1 是 TODO（C2/C3 也是 TODO 但它们是子任务）"
    );
    assert_eq!(page.list[0].id, uuid(R1));

    let todo_all = TaskFilter {
        status: Some("TODO".to_string()),
        root_only: false,
        keyword: Some(FIXTURE_KEYWORD.to_string()),
        ..TaskFilter::default()
    };
    let page = page_visible(&principal(), &todo_all, 1, 50)
        .await
        .expect("分页查询失败");
    assert_eq!(
        page.total, 3,
        "不开 root_only 时 TODO 有 3 条（R1 + C2 + C3）"
    );
}
