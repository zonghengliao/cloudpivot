// ============================================================================
// task_doc_link_tests.rs —— P2-9 / DC-19：任务挂业务单据（真库用例）
//
// 与 `task_model_*_tests.rs` 同口径：`TASK_DB_URL` 指向「已应用 V0238 的隔离库」，
// 未设置则整段 SKIP（打印原因，不静默假绿）。
//
// 覆盖卡面要求的五类断言：
//   ① doc_type 白名单（63022）
//   ② 单据不存在 / 已软删（63023）
//   ③ 反向查询 by-doc（只返回这张单上的任务，且受任务可见性约束）
//   ④ 落库 / 详情下发（doc_type + doc_id + 单号）
//   ⑤ 改任务清单 / 换单（成对语义）
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;
use crate::business::task::doc_ref;
use crate::shared::error_code;

use super::*;

/// 夹具用户：我 / 别人（可见性用例要区分）。
const U_ME: &str = "7b2e0000-0000-4000-8000-0000000000a1";
const U_OTHER: &str = "7b2e0000-0000-4000-8000-0000000000a2";
/// 夹具单据：采购单（在）/ 采购单（已软删）/ 合同（在）。
const PO_OK: &str = "7b2e0000-0000-4000-8000-0000000001d1";
const PO_GONE: &str = "7b2e0000-0000-4000-8000-0000000001d2";
const CONTRACT_OK: &str = "7b2e0000-0000-4000-8000-0000000001c1";
/// 夹具采购单号（`uk_biz_po_no` 唯一索引：活跃行不许重号）。
const PO_OK_NO: &str = "P29-DOC-0001";

/// 真库用例**串行**执行（共用一个库 + 固定 UUID 夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `TASK_DB_URL`。
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

/// 拿锁 + 连库 + 重建夹具（幂等；每次先删自己上一轮造的行）。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    for id in [U_ME, U_OTHER] {
        db().execute_with_params(
            "INSERT INTO sys_user (id, username, password) VALUES (?::uuid, ?, 'p29-not-a-real-hash') \
             ON CONFLICT (id) DO NOTHING",
            vec![Value::from(&uuid(id)), Value::Text(format!("p29_doc_{}", &id[35..]))],
        )
        .await
        .unwrap_or_else(|err| panic!("夹具用户写入失败（{id}）：{err}"));
    }
    // 清任务：先断父指针（自引用 FK 不是 deferrable）
    let me = uuid(U_ME);
    let other = uuid(U_OTHER);
    db().execute_with_params(
        "UPDATE biz_task SET parent_id = NULL WHERE creator_id IN (?::uuid, ?::uuid)",
        vec![Value::from(&me), Value::from(&other)],
    )
    .await
    .expect("夹具清理（断父指针）失败");
    db().execute_with_params(
        "DELETE FROM biz_task WHERE creator_id IN (?::uuid, ?::uuid)",
        vec![Value::from(&me), Value::from(&other)],
    )
    .await
    .expect("夹具清理失败");
    // 清单据（采购单 → 合同：合同是弱引用，删单在前）
    for sql in [
        "DELETE FROM biz_purchase_order WHERE id IN ('7b2e0000-0000-4000-8000-0000000001d1'::uuid, \
         '7b2e0000-0000-4000-8000-0000000001d2'::uuid)",
        "DELETE FROM biz_contract WHERE id = '7b2e0000-0000-4000-8000-0000000001c1'::uuid",
    ] {
        db().execute_with_params(sql, vec![])
            .await
            .unwrap_or_else(|err| panic!("夹具清理失败：{err}\nSQL: {sql}"));
    }
    // 建单据夹具：采购单（在）+ 采购单（已软删）+ 合同（在）
    for sql in [
        format!(
            "INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status) \
             VALUES ('{PO_OK}'::uuid, '{PO_OK_NO}', gen_random_uuid(), DATE '2026-11-01', gen_random_uuid(), 'DRAFT')"
        ),
        "INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status, deleted_at) \
         VALUES ('7b2e0000-0000-4000-8000-0000000001d2'::uuid, 'P29-DOC-0002', gen_random_uuid(), \
         DATE '2026-11-01', gen_random_uuid(), 'DRAFT', now())"
            .to_string(),
        "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, \
                                   party_b_id, party_b_name, amount, currency_code, status) \
         VALUES ('7b2e0000-0000-4000-8000-0000000001c1'::uuid, 'P29-DOC-HT-1', 'P2-9 夹具合同', \
                 'PURCHASE', 'SUPPLIER', gen_random_uuid(), 'P2-9 夹具供应商', 10000.0000, 'CNY', 'DRAFT')"
            .to_string(),
    ] {
        db().execute_with_params(&sql, vec![])
            .await
            .unwrap_or_else(|err| panic!("单据夹具写入失败：{err}\nSQL: {sql}"));
    }
    Some(guard)
}

/// 全可见范围（超管 + `All`）：让用例只验证「挂单」本身，不掺杂数据权限。
fn principal_all() -> ScopedPrincipal {
    ScopedPrincipal::for_test(uuid(U_ME), None, true, scope::ScopeKind::All, Vec::new(), false)
}

/// 只看自己（`SelfOnly`）：可见性用例用。
fn principal_self() -> ScopedPrincipal {
    ScopedPrincipal::for_test(uuid(U_ME), None, false, scope::ScopeKind::SelfOnly, Vec::new(), true)
}

/// 建一条任务（真库，走 `model::create_task`：与线上同一条写入路径）。
async fn create_task_with(owner: Uuid, doc_type: Option<&str>, doc_id: Option<Uuid>, title: &str) -> (Uuid, String) {
    let id = Uuid::now_v7();
    let month = month_key(today_cst());
    let new = NewTask {
        id,
        title: title.to_string(),
        description: None,
        priority: "P2".to_string(),
        need_review: false,
        owner_id: owner,
        creator_id: owner,
        assigner_id: None,
        dept_id: None,
        parent_id: None,
        project_id: None,
        doc_type: doc_type.map(str::to_string),
        doc_id,
        start_date: None,
        due_date: None,
        estimate_hours: None,
        source: "SELF".to_string(),
        add_creator_as_collaborator: false,
    };
    let task_no = db()
        .transaction(|tx| async move { create_task(&tx, &new, owner, &month).await })
        .await
        .expect("建任务失败");
    (id, task_no)
}

/// 改任务的单据关联（拿一把事务，与 handler 同一条写入路径）。
async fn patch_doc(id: Uuid, doc: Option<(Option<String>, Option<Uuid>)>, me: Uuid) -> u64 {
    let patch = TaskPatch {
        doc,
        ..TaskPatch::default()
    };
    db().transaction(|tx| async move { update_task(&tx, id, &patch, me).await })
        .await
        .expect("改任务单据关联失败")
}

/// 直接读库里的 `doc_type / doc_id`（验证「真的落库」，不只看 DTO 回显）。
async fn stored_doc(id: Uuid) -> (Option<String>, Option<String>) {
    let row = db()
        .find_with_params(
            "SELECT doc_type, doc_id::text AS doc_id FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("读 doc_type / doc_id");
    let row = row.first().expect("任务行必须存在");
    (
        row.get_typed("doc_type").expect("doc_type"),
        row.get_typed("doc_id").expect("doc_id"),
    )
}

// ==================== ① 白名单 ====================

/// 白名单外的类型 → `400 + 63022`（在建任务前就被拦掉，库里不留行）。
#[tokio::test]
async fn doc_link_rejects_unknown_type() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let error = doc_ref::resolve(Some("SALES_ORDER"), Some(PO_OK))
        .await
        .expect_err("白名单外的类型必须被拒");
    assert_eq!(error.status, 400, "{}", error.msg);
    assert_eq!(error.code, error_code::TASK_DOC_TYPE_INVALID);
    assert!(error.msg.contains("PURCHASE_ORDER"), "报文要列出白名单：{}", error.msg);
}

// ==================== ② 存在性 ====================

/// 单据不存在 → `400 + 63023`（弱引用脏数据在这里被挡）。
#[tokio::test]
async fn doc_link_rejects_missing_doc() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let ghost = Uuid::now_v7();
    let error = doc_ref::resolve(Some("PURCHASE_ORDER"), Some(&ghost.to_string()))
        .await
        .expect_err("不存在的单据必须被拒");
    assert_eq!(error.status, 400, "{}", error.msg);
    assert_eq!(error.code, error_code::TASK_DOC_NOT_FOUND);
    assert!(
        error.msg.contains("采购订单"),
        "报文要点名单据类型（DocKind::label）：{}",
        error.msg
    );
}

/// 单据已软删 → 同样 `63023`（软删后不许再被新任务挂上）。
#[tokio::test]
async fn doc_link_rejects_soft_deleted_doc() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let error = doc_ref::resolve(Some("PURCHASE_ORDER"), Some(PO_GONE))
        .await
        .expect_err("已软删的单据必须被拒");
    assert_eq!(error.code, error_code::TASK_DOC_NOT_FOUND, "{}", error.msg);
}

// ==================== ④ 落库 + 详情下发 ====================

/// 建任务带单据 → 两列真落库，详情下发 `doc_type / doc_id / doc_no`。
#[tokio::test]
async fn doc_link_create_persists_and_detail_reports_no() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ME);
    let (id, _) = create_task_with(me, Some("PURCHASE_ORDER"), Some(uuid(PO_OK)), "P2-9 催货（挂采购单）").await;

    let (doc_type, doc_id) = stored_doc(id).await;
    assert_eq!(doc_type.as_deref(), Some("PURCHASE_ORDER"), "doc_type 必须落库");
    assert_eq!(doc_id.as_deref(), Some(PO_OK), "doc_id 必须落库");

    let detail = find_detail(&principal_all(), id)
        .await
        .expect("查详情")
        .expect("任务必须存在");
    assert_eq!(detail.card.doc_type.as_deref(), Some("PURCHASE_ORDER"));
    assert_eq!(detail.card.doc_id.map(|id| id.to_string()).as_deref(), Some(PO_OK));
    // 单号由 handler 现查（这里直接验 doc_ref 的解析，handler 侧同一函数）
    let doc_no = doc_ref::doc_no_of(Some("PURCHASE_ORDER"), Some(PO_OK)).await;
    assert_eq!(doc_no.as_deref(), Some(PO_OK_NO), "详情要能取到单号");
}

/// 不传单据 → 两列都是 NULL（自由任务语义不变）。
#[tokio::test]
async fn doc_link_absent_keeps_columns_null() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (id, _) = create_task_with(uuid(U_ME), None, None, "P2-9 自由任务").await;
    let (doc_type, doc_id) = stored_doc(id).await;
    assert!(doc_type.is_none() && doc_id.is_none(), "自由任务不许有半对");
    assert!(
        doc_ref::doc_no_of(doc_type.as_deref(), doc_id.as_deref())
            .await
            .is_none(),
        "没有挂单就没有单号"
    );
}

// ==================== ③ 反向查询 ====================

/// by-doc 只返回挂在这张单上的任务（别的单 / 自由任务都不许混进来）。
#[tokio::test]
async fn doc_link_by_doc_lists_only_that_doc() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ME);
    let (hit_a, no_a) = create_task_with(me, Some("PURCHASE_ORDER"), Some(uuid(PO_OK)), "挂 A").await;
    let (hit_b, _) = create_task_with(me, Some("PURCHASE_ORDER"), Some(uuid(PO_OK)), "挂 B").await;
    let (other_doc, _) = create_task_with(me, Some("CONTRACT"), Some(uuid(CONTRACT_OK)), "挂合同").await;
    let (free, _) = create_task_with(me, None, None, "自由任务").await;

    let rows = list_by_doc(&principal_all(), "PURCHASE_ORDER", uuid(PO_OK), DOC_TASK_LIMIT)
        .await
        .expect("反向查询");
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    assert_eq!(rows.len(), 2, "只应返回挂在这张采购单上的 2 条，实际 {ids:?}");
    assert!(ids.contains(&hit_a) && ids.contains(&hit_b));
    assert!(!ids.contains(&other_doc), "别的单据上的任务不许混进来");
    assert!(!ids.contains(&free), "自由任务不许混进来");
    assert!(
        rows.iter().any(|row| row.task_no == no_a),
        "任务卡要带 task_no（前端直接显示）"
    );

    // 挂合同的那条：按 CONTRACT 查得到，按采购单查不到
    let contract_rows = list_by_doc(&principal_all(), "CONTRACT", uuid(CONTRACT_OK), DOC_TASK_LIMIT)
        .await
        .expect("反向查询（合同）");
    assert_eq!(contract_rows.len(), 1);
    assert_eq!(contract_rows[0].id, other_doc);
}

/// by-doc **不绕过任务可见性**：别人挂在同一张单上的任务，我看采购单也看不到。
#[tokio::test]
async fn doc_link_by_doc_respects_visibility() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ME);
    let (mine, _) = create_task_with(me, Some("PURCHASE_ORDER"), Some(uuid(PO_OK)), "我的").await;
    let (theirs, _) = create_task_with(uuid(U_OTHER), Some("PURCHASE_ORDER"), Some(uuid(PO_OK)), "别人的").await;

    // 超管 / All：两条都看得见
    let all = list_by_doc(&principal_all(), "PURCHASE_ORDER", uuid(PO_OK), DOC_TASK_LIMIT)
        .await
        .expect("反向查询（全范围）");
    assert_eq!(all.len(), 2, "全范围应看到两条");

    // 只看自己：别人的那条必须消失（挂单不是共享权限）
    let mine_only = list_by_doc(&principal_self(), "PURCHASE_ORDER", uuid(PO_OK), DOC_TASK_LIMIT)
        .await
        .expect("反向查询（仅自己）");
    let ids: Vec<Uuid> = mine_only.iter().map(|row| row.id).collect();
    assert_eq!(ids, vec![mine], "仅自己范围只该看到自己的任务");
    assert!(!ids.contains(&theirs));
}

// ==================== ⑤ 改任务：清单 / 换单 ====================

/// 改任务：`(None, None)` 清单、`(Some, Some)` 换单，两次都真落库。
#[tokio::test]
async fn doc_link_update_clears_and_swaps() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ME);
    let (id, _) = create_task_with(me, Some("PURCHASE_ORDER"), Some(uuid(PO_OK)), "改单用例").await;

    // ① 换到合同
    let affected = patch_doc(id, Some((Some("CONTRACT".to_string()), Some(uuid(CONTRACT_OK)))), me).await;
    assert_eq!(affected, 1);
    let (doc_type, doc_id) = stored_doc(id).await;
    assert_eq!(doc_type.as_deref(), Some("CONTRACT"));
    assert_eq!(doc_id.as_deref(), Some(CONTRACT_OK));

    // ② 清单（成对 NULL）
    let affected = patch_doc(id, Some((None, None)), me).await;
    assert_eq!(affected, 1);
    let (doc_type, doc_id) = stored_doc(id).await;
    assert!(doc_type.is_none() && doc_id.is_none(), "清单后两列必须都为空");

    // ③ 清单后反向查询不再返回它
    let rows = list_by_doc(&principal_all(), "CONTRACT", uuid(CONTRACT_OK), DOC_TASK_LIMIT)
        .await
        .expect("反向查询");
    assert!(rows.is_empty(), "清单后不该再出现在单据的关联任务里");
}

/// 库侧 `ck_biz_task_doc_pair` 兜底：绕过 handler 直写半对 → 约束报错。
#[tokio::test]
async fn doc_link_db_rejects_half_pair() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let error = db()
        .execute_with_params(
            "INSERT INTO biz_task (task_no, title, owner_id, creator_id, doc_type) \
             VALUES ('P29-DOC-HALF', '半对', ?::uuid, ?::uuid, 'PURCHASE_ORDER')",
            vec![Value::from(&uuid(U_ME)), Value::from(&uuid(U_ME))],
        )
        .await
        .expect_err("只给 doc_type 必须被库拒绝");
    let text = format!("{error}");
    assert!(
        text.contains("ck_biz_task_doc_pair") || text.contains("check constraint"),
        "期望被 ck_biz_task_doc_pair 拦下，实际：{text}"
    );
}
