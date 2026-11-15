// ============================================================================
// task_model_statuses_filter_tests.rs —— 状态多选筛选
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
const U_ID: &str = "7a9d0000-0000-4000-8000-0000000000b1";
/// 4 条顶层任务（TODO / DOING / DONE / CANCELLED）+ 1 条 DOING 子任务。
const T_TODO: &str = "7a9d0000-0000-4000-8000-000000000101";
const T_DOING: &str = "7a9d0000-0000-4000-8000-000000000102";
const T_DONE: &str = "7a9d0000-0000-4000-8000-000000000103";
const T_CANCEL: &str = "7a9d0000-0000-4000-8000-000000000104";
/// 子任务（父 = `T_TODO`，状态 DOING）。
const T_CHILD: &str = "7a9d0000-0000-4000-8000-000000000201";

/// 真库用例**串行**执行（共用一个库 + 固定 UUID 夹具）。
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

/// 写一行夹具任务。
async fn insert_task(id: Uuid, task_no: &str, parent: Option<Uuid>, status: &str, me: Uuid) {
    let affected = db()
        .execute_with_params(
            "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, parent_id, \
                                   status, priority, progress, need_review) \
             VALUES (?::uuid, ?, ?, ?::uuid, ?::uuid, ?::uuid, ?, 'P2', 0, false)",
            vec![
                Value::from(&id),
                Value::Text(task_no.to_string()),
                Value::Text(format!("T9A {task_no}")),
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
         VALUES (?::uuid, 't9a_statuses_user', 't9a-not-a-real-hash') \
         ON CONFLICT DO NOTHING",
        vec![Value::from(&me)],
    )
    .await
    .expect("夹具用户写入失败");
    // 自引用 FK 不是 deferrable：先断父指针再删（T8-A 实测）。
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
    insert_task(uuid(T_TODO), "T9A-TODO", None, "TODO", me).await;
    insert_task(uuid(T_DOING), "T9A-DOING", None, "DOING", me).await;
    insert_task(uuid(T_DONE), "T9A-DONE", None, "DONE", me).await;
    insert_task(uuid(T_CANCEL), "T9A-CANCEL", None, "CANCELLED", me).await;
    insert_task(uuid(T_CHILD), "T9A-CHILD", Some(uuid(T_TODO)), "DOING", me).await;
    Some(guard)
}

/// 全可见范围（超管 + `All`）：只验证 `statuses` 本身，不掺数据权限。
fn principal() -> ScopedPrincipal {
    ScopedPrincipal::for_test(uuid(U_ID), None, true, scope::ScopeKind::All, Vec::new(), false)
}

/// 夹具关键词：标题以 `T9A ` 开头、任务号以 `T9A-` 开头。
///
/// 每次查询都带上它 —— 全量套件里别的模块的夹具也住在同一个库，不带关键词时
/// 「应该 5 条」会随别人的夹具数量漂移。
const FIXTURE_KEYWORD: &str = "T9A-";

fn filter(statuses: Option<Vec<&str>>, status: Option<&str>, root_only: bool) -> TaskFilter {
    TaskFilter {
        statuses: statuses.map(|list| list.into_iter().map(str::to_string).collect()),
        status: status.map(str::to_string),
        root_only,
        keyword: Some(FIXTURE_KEYWORD.to_string()),
        ..TaskFilter::default()
    }
}

/// 断言分页结果恰好是这些任务（忽略顺序）。
fn assert_ids(page: &VisiblePage, expected: &[&str]) {
    let mut got = page.list.iter().map(|row| row.id.to_string()).collect::<Vec<_>>();
    got.sort();
    let mut want = expected.iter().map(|id| id.to_string()).collect::<Vec<_>>();
    want.sort();
    assert_eq!(got, want, "返回的任务集合不对（total={}）", page.total);
    assert_eq!(page.total as usize, expected.len(), "total 与 list 必须同口径");
}

// ---------- ① 纯逻辑：IN 子句的占位符个数与参数顺序 ----------
/// `statuses = ["TODO", "DOING"]` → `IN (?,?)` + 两个参数按 vec 顺序 push。
///
/// 这是本卡最容易错的地方：占位符个数必须等于 vec 长度，参数逐个 push，
/// 且片段在业务过滤最前（`status` 那段被跳过）。
#[test]
fn statuses_builds_in_clause_with_ordered_params() {
    let list = vec!["TODO".to_string(), "DOING".to_string()];
    let mut params = Vec::new();
    let sql = build_filters(
        &TaskFilter {
            statuses: Some(list.clone()),
            ..TaskFilter::default()
        },
        &mut params,
    );
    assert_eq!(sql, " AND t.status IN (?,?)", "IN 子句形态不对：{sql}");
    assert_eq!(sql.matches('?').count(), list.len(), "占位符个数必须等于状态个数");
    assert_eq!(params.len(), 2, "每个状态一个绑定参数");
    // 参数必须按 statuses 顺序逐个 push（顺序错 = 静默绑错值）
    let texts = params.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>();
    assert!(
        texts[0].contains("TODO") && texts[1].contains("DOING"),
        "参数顺序必须与 statuses 一致：{texts:?}"
    );
    assert!(
        !sql.contains(" AND t.status = ?"),
        "statuses 非空时不该再拼单值 status 片段：{sql}"
    );
}

/// `Some(空 vec)` 等价于不筛：一个字符都不产生。
#[test]
fn empty_statuses_produces_no_snippet() {
    let mut params = Vec::new();
    let sql = build_filters(
        &TaskFilter {
            statuses: Some(Vec::new()),
            ..TaskFilter::default()
        },
        &mut params,
    );
    assert_eq!(sql, "", "空 vec 不该产生任何 WHERE 片段");
    assert!(params.is_empty(), "空 vec 不该 push 任何参数");

    // 与「完全不传」逐字相同
    let mut params_none = Vec::new();
    let sql_none = build_filters(&TaskFilter::default(), &mut params_none);
    assert_eq!(sql, sql_none);
    assert_eq!(params.len(), params_none.len());
}

/// `statuses` 与 `status` 同时存在 → **statuses 优先**（只拼 IN，不拼 `= ?`）。
#[test]
fn statuses_wins_over_single_status() {
    let mut params = Vec::new();
    let sql = build_filters(
        &TaskFilter {
            statuses: Some(vec!["DONE".to_string()]),
            status: Some("TODO".to_string()),
            ..TaskFilter::default()
        },
        &mut params,
    );
    assert_eq!(sql, " AND t.status IN (?)", "statuses 必须盖住单值 status：{sql}");
    assert_eq!(params.len(), 1);
    let got = format!("{:?}", params[0]);
    assert!(
        got.contains("DONE"),
        "优先取 statuses 的值（DONE），不是 status 的 TODO：{got}"
    );

    // 单值 status 单独使用时行为不变（向后兼容）
    let mut params_single = Vec::new();
    let sql_single = build_filters(
        &TaskFilter {
            status: Some("TODO".to_string()),
            ..TaskFilter::default()
        },
        &mut params_single,
    );
    assert_eq!(sql_single, " AND t.status = ?");
    assert_eq!(params_single.len(), 1);
}

/// 与其它过滤组合时，`IN (?,?)` 的占位符仍在最前（参数顺序不被打乱）。
#[test]
fn statuses_keeps_param_order_with_other_filters() {
    let mut params = Vec::new();
    let sql = build_filters(
        &TaskFilter {
            statuses: Some(vec!["TODO".to_string(), "DOING".to_string()]),
            priority: Some("P1".to_string()),
            keyword: Some("T9A".to_string()),
            root_only: true,
            ..TaskFilter::default()
        },
        &mut params,
    );
    // 2（IN）+ 1（priority）+ 2（keyword）= 5
    assert_eq!(params.len(), 5, "参数个数不对：{sql}");
    let pos_in = sql.find(" AND t.status IN (?,?)").expect("IN 片段");
    let pos_prio = sql.find(" AND t.priority = ?").expect("priority 片段");
    let pos_kw = sql.find(" AND (t.title ILIKE ?").expect("keyword 片段");
    let pos_root = sql.find(" AND t.parent_id IS NULL").expect("root_only 片段");
    assert!(
        pos_in < pos_prio && pos_prio < pos_kw && pos_kw < pos_root,
        "片段顺序必须与参数 push 顺序一致：{sql}"
    );
}

// ---------- ② 真库：多选只返回这些状态 ----------
#[tokio::test]
async fn page_returns_only_selected_statuses() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let page = page_visible(&principal(), &filter(Some(vec!["TODO", "DOING"]), None, false), 1, 50)
        .await
        .expect("分页查询失败");
    // TODO 顶层 + DOING 顶层 + DOING 子任务 = 3
    assert_ids(&page, &[T_TODO, T_DOING, T_CHILD]);
    assert!(
        page.list
            .iter()
            .all(|row| row.status == "TODO" || row.status == "DOING"),
        "不该返回其它状态"
    );
}

// ---------- ③ 真库：单值 statuses 等价于旧 status ----------
#[tokio::test]
async fn single_statuses_equals_legacy_status() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let multi = page_visible(&principal(), &filter(Some(vec!["DONE"]), None, false), 1, 50)
        .await
        .expect("statuses 分页失败");
    let legacy = page_visible(&principal(), &filter(None, Some("DONE"), false), 1, 50)
        .await
        .expect("status 分页失败");
    assert_ids(&multi, &[T_DONE]);
    assert_eq!(
        format!("{:?}", multi.list.iter().map(|r| r.id).collect::<Vec<_>>()),
        format!("{:?}", legacy.list.iter().map(|r| r.id).collect::<Vec<_>>()),
        "statuses=DONE 必须与旧 status=DONE 返回同一行"
    );
    assert_eq!(multi.total, legacy.total);
}

// ---------- ④ 真库：statuses 与 root_only 组合，两个条件都生效 ----------
#[tokio::test]
async fn statuses_combines_with_root_only() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 不带 root_only：TODO + DOING 三行（含 DOING 子任务）
    let all = page_visible(&principal(), &filter(Some(vec!["TODO", "DOING"]), None, false), 1, 50)
        .await
        .expect("分页失败");
    // 带 root_only：子任务被排除，只剩两个顶层
    let roots = page_visible(&principal(), &filter(Some(vec!["TODO", "DOING"]), None, true), 1, 50)
        .await
        .expect("分页失败");
    assert_eq!(all.total, 3, "不带 root_only 时含 DOING 子任务");
    assert_ids(&roots, &[T_TODO, T_DOING]);
    assert!(
        roots.list.iter().all(|row| row.parent_id.is_none()),
        "root_only=true 时不该出现子任务"
    );
}

// ---------- ⑤ 真库：空 statuses / 不传 = 返回全部 ----------
#[tokio::test]
async fn empty_statuses_returns_all_rows() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let none = page_visible(&principal(), &filter(None, None, false), 1, 50)
        .await
        .expect("分页失败");
    let empty = page_visible(&principal(), &filter(Some(Vec::new()), None, false), 1, 50)
        .await
        .expect("分页失败");
    assert_eq!(none.total, 5, "夹具共 5 行（4 顶层 + 1 子）");
    assert_ids(&empty, &[T_TODO, T_DOING, T_DONE, T_CANCEL, T_CHILD]);
    assert_eq!(empty.total, none.total, "空 vec 与不传必须等价");
}
