// ============================================================================
// task_model_cascade_cancel_tests.rs —— 父任务取消 → 子孙级联取消（T11，原 cascade_tests::t11_cancel）
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use super::cascade_tests::*;
use crate::business::task::state::TaskStatus;

use super::*;

/// 走生产同款写路径的最小子集：**同一事务**里「父任务取消 → 级联取消全部非终态子孙」。
///
/// 与 `handler::change_status` 的事务体同序：先 `apply_status` 写父任务，再
/// `cascade_cancel_descendants`。两者失败一起回滚（用例⑧验证）。
async fn cancel_parent(parent: Uuid, me: Uuid) -> Vec<CancelCascade> {
    let tx = db().begin().await.expect("开启事务失败");
    let update = StatusUpdate {
        to: TaskStatus::Cancelled.code().to_string(),
        progress: None,
        done_at: None,
        reject_reason: None,
        cancel_reason: Some(CASCADE_CANCEL_REASON.to_string()),
    };
    let affected = apply_status(&tx, parent, &update, me).await.expect("父任务取消失败");
    assert_eq!(affected, 1, "父任务必须存在");
    let out = cascade_cancel_descendants(&tx, parent, me, CASCADE_CANCEL_REASON)
        .await
        .expect("级联取消失败");
    tx.commit().await.expect("提交事务失败");
    out
}

fn ids_of(out: &[CancelCascade]) -> std::collections::BTreeSet<Uuid> {
    out.iter().map(|cancel| cancel.task_id).collect()
}

async fn progress_of(id: Uuid) -> i16 {
    let rows = db()
        .find_with_params(
            "SELECT progress FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("查进度失败");
    rows.first()
        .expect("任务应存在")
        .get_typed("progress")
        .expect("取 progress")
}

async fn cancel_reason_of(id: Uuid) -> Option<String> {
    let rows = db()
        .find_with_params(
            "SELECT cancel_reason FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("查取消原因失败");
    rows.first()
        .expect("任务应存在")
        .get_typed("cancel_reason")
        .expect("取 cancel_reason")
}

async fn drop_block_trigger() {
    db().exec_sql(
        "DROP TRIGGER IF EXISTS t11_block_cancel_trg ON biz_task".to_string(),
        Vec::new(),
    )
    .await
    .expect("清触发器失败");
    db().exec_sql("DROP FUNCTION IF EXISTS t11_block_cancel()".to_string(), Vec::new())
        .await
        .expect("清触发器函数失败");
}

// ---------- ⑫ 纯逻辑（不连库）：守卫只在「父已取消 + 子要推进」时拦 ----------

#[test]
fn guard_blocks_only_advance_under_cancelled_parent() {
    // 父已取消 → 推进一律拦
    assert!(blocks_child_advance(Some(TaskStatus::Cancelled), TaskStatus::Doing));
    assert!(blocks_child_advance(Some(TaskStatus::Cancelled), TaskStatus::Review));
    assert!(blocks_child_advance(Some(TaskStatus::Cancelled), TaskStatus::Done));
    // 子任务自己去 CANCELLED → 放行（幂等收敛，不是推进）
    assert!(!blocks_child_advance(
        Some(TaskStatus::Cancelled),
        TaskStatus::Cancelled
    ));
    // 父任务没取消 / 没有父任务 → 放行
    for parent in [
        None,
        Some(TaskStatus::Todo),
        Some(TaskStatus::Doing),
        Some(TaskStatus::Review),
        Some(TaskStatus::Done),
    ] {
        assert!(
            !blocks_child_advance(parent, TaskStatus::Doing),
            "父任务 {parent:?} 不该拦子任务推进"
        );
    }
}

// ---------- ① 父取消 → 1 个 DOING 子任务变 CANCELLED ----------

#[tokio::test]
async fn cancel_parent_cancels_doing_child() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(parent, "T11-1-P", None, "DOING", me).await;
    insert_task(child, "T11-1-C", Some(parent), "DOING", me).await;

    let out = cancel_parent(parent, me).await;
    assert_eq!(status_of(parent).await, TaskStatus::Cancelled);
    assert_eq!(
        status_of(child).await,
        TaskStatus::Cancelled,
        "漏洞 1 的修复：父已取消，子任务必须跟着取消（不许还挂在进行中）"
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].task_id, child);
    assert_eq!(out[0].from, TaskStatus::Doing, "变更日志要能说清从哪个状态取消的");
    assert_eq!(
        cancel_reason_of(child).await.as_deref(),
        Some(CASCADE_CANCEL_REASON),
        "子任务的 cancel_reason 要写清是级联取消（不是人点的）"
    );
}

// ---------- ② 三子任务（TODO / DOING / REVIEW）全部 CANCELLED ----------

#[tokio::test]
async fn cancel_parent_cancels_all_open_children() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let todo = Uuid::now_v7();
    let doing = Uuid::now_v7();
    let review = Uuid::now_v7();
    insert_task(parent, "T11-2-P", None, "DOING", me).await;
    insert_task(todo, "T11-2-C1", Some(parent), "TODO", me).await;
    insert_task(doing, "T11-2-C2", Some(parent), "DOING", me).await;
    insert_task(review, "T11-2-C3", Some(parent), "REVIEW", me).await;

    let out = cancel_parent(parent, me).await;
    assert_eq!(
        ids_of(&out),
        [todo, doing, review].into_iter().collect(),
        "三个非终态子任务一个都不能漏：{out:?}"
    );
    for child in [todo, doing, review] {
        assert_eq!(status_of(child).await, TaskStatus::Cancelled);
    }
}

// ---------- ③ DONE 的子任务保持 DONE；它下面没结束的孙任务照样取消 ----------

#[tokio::test]
async fn cancel_parent_keeps_done_child_but_cancels_its_open_descendant() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let done = Uuid::now_v7();
    let grandchild = Uuid::now_v7();
    insert_task(parent, "T11-3-P", None, "DOING", me).await;
    insert_task(done, "T11-3-C", Some(parent), "DONE", me).await;
    insert_task(grandchild, "T11-3-G", Some(done), "DOING", me).await;

    let out = cancel_parent(parent, me).await;
    assert_eq!(
        status_of(done).await,
        TaskStatus::Done,
        "已完成的事实不能被抹掉：DONE 不回退成 CANCELLED"
    );
    assert_eq!(
        status_of(grandchild).await,
        TaskStatus::Cancelled,
        "DONE 的中间层照样往下递归：未完成的孙任务必须取消"
    );
    assert_eq!(
        ids_of(&out),
        [grandchild].into_iter().collect(),
        "DONE 的行不能出现在「本次取消」的名单里（否则会误写变更日志）"
    );
}

// ---------- ④ 三层：祖父取消 → 子、孙全部 CANCELLED ----------

#[tokio::test]
async fn cancel_grandparent_cancels_three_levels() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let grand = Uuid::now_v7();
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(grand, "T11-4-G", None, "DOING", me).await;
    insert_task(parent, "T11-4-P", Some(grand), "DOING", me).await;
    insert_task(child, "T11-4-C", Some(parent), "REVIEW", me).await;

    let out = cancel_parent(grand, me).await;
    assert_eq!(status_of(grand).await, TaskStatus::Cancelled);
    assert_eq!(status_of(parent).await, TaskStatus::Cancelled);
    assert_eq!(status_of(child).await, TaskStatus::Cancelled);
    assert_eq!(
        ids_of(&out),
        [parent, child].into_iter().collect(),
        "递归要一路到底（一次 CTE，不是只取消直接子任务）"
    );
}

// ---------- ⑤ 父已取消 → 子任务推 DOING 被拒（规则②） ----------

#[tokio::test]
async fn cancelled_parent_blocks_child_to_doing() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(parent, "T11-5-P", None, "DOING", me).await;
    insert_task(child, "T11-5-C", Some(parent), "DOING", me).await;
    cancel_parent(parent, me).await;
    // 造出「父已取消、子还是 TODO」的脏状态（级联之后不会出现，但历史数据可能有）
    db().execute_with_params(
        "UPDATE biz_task SET status = 'TODO' WHERE id = ?::uuid",
        vec![Value::from(&child)],
    )
    .await
    .expect("回填子任务状态失败");

    let parent_now = parent_status(db(), child).await.expect("查父任务状态失败");
    assert_eq!(parent_now, Some(TaskStatus::Cancelled));
    assert!(
        blocks_child_advance(parent_now, TaskStatus::Doing),
        "父已取消 → 子任务推 DOING 必须被拦（漏洞 2 的修复）"
    );
}

// ---------- ⑥ 父已取消 → 子任务推 REVIEW 被拒（规则②） ----------

#[tokio::test]
async fn cancelled_parent_blocks_child_to_review() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(parent, "T11-6-P", None, "DOING", me).await;
    insert_task(child, "T11-6-C", Some(parent), "DOING", me).await;
    cancel_parent(parent, me).await;
    db().execute_with_params(
        "UPDATE biz_task SET status = 'DOING' WHERE id = ?::uuid",
        vec![Value::from(&child)],
    )
    .await
    .expect("回填子任务状态失败");

    let parent_now = parent_status(db(), child).await.expect("查父任务状态失败");
    assert!(blocks_child_advance(parent_now, TaskStatus::Review));
}

// ---------- ⑦ 父已取消 → 子任务自己取消放行（幂等） ----------

#[tokio::test]
async fn cancelled_parent_still_allows_child_cancel() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(parent, "T11-7-P", None, "DOING", me).await;
    insert_task(child, "T11-7-C", Some(parent), "DOING", me).await;
    cancel_parent(parent, me).await;

    // 级联之后子任务已是 CANCELLED：再取消一次必须是幂等的空操作
    let again = cancel_parent(child, me).await;
    assert!(again.is_empty(), "再取消一次不该产出新的级联：{again:?}");
    assert_eq!(status_of(child).await, TaskStatus::Cancelled);

    // 守卫本身也必须放行「子任务自己取消」
    let parent_now = parent_status(db(), child).await.expect("查父任务状态失败");
    assert!(
        !blocks_child_advance(parent_now, TaskStatus::Cancelled),
        "子任务自己被取消是收敛，不是推进 → 不许拦"
    );
}

// ---------- ⑧ 事务性：级联中途失败 → 父任务也不变（回滚） ----------

#[tokio::test]
async fn cascade_failure_rolls_back_parent_cancel() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let blocked = Uuid::now_v7();
    let other = Uuid::now_v7();
    insert_task(parent, "T11-8-P", None, "DOING", me).await;
    insert_task(blocked, "T11-8-C1", Some(parent), "DOING", me).await;
    insert_task(other, "T11-8-C2", Some(parent), "TODO", me).await;
    drop_block_trigger().await;

    // 让「取消这一个子任务」必失败：BEFORE UPDATE 触发器只挂在这一行
    db().exec_sql(
        "CREATE OR REPLACE FUNCTION t11_block_cancel() RETURNS trigger AS $$ \
             BEGIN \
               RAISE EXCEPTION 'T11 单测：禁止级联取消该子任务'; \
             END; \
             $$ LANGUAGE plpgsql"
            .to_string(),
        Vec::new(),
    )
    .await
    .expect("建触发器函数失败");
    db().exec_sql(
        format!(
            "CREATE TRIGGER t11_block_cancel_trg BEFORE UPDATE ON biz_task \
                 FOR EACH ROW WHEN (OLD.id = '{blocked}'::uuid) \
                 EXECUTE FUNCTION t11_block_cancel()"
        ),
        Vec::new(),
    )
    .await
    .expect("建触发器失败");

    let tx = db().begin().await.expect("开启事务失败");
    let update = StatusUpdate {
        to: TaskStatus::Cancelled.code().to_string(),
        progress: None,
        done_at: None,
        reject_reason: None,
        cancel_reason: Some(CASCADE_CANCEL_REASON.to_string()),
    };
    apply_status(&tx, parent, &update, me)
        .await
        .expect("父任务状态先写成功");
    let error = cascade_cancel_descendants(&tx, parent, me, CASCADE_CANCEL_REASON)
        .await
        .expect_err("被触发器拦住时必须返回错误");
    println!("级联取消失败（预期）：{error}");
    tx.rollback().await.expect("回滚事务失败");

    assert_eq!(
        status_of(parent).await,
        TaskStatus::Doing,
        "级联失败 → 父任务的取消也必须回滚（不接受「父 CANCELLED、子 DOING」）"
    );
    assert_eq!(status_of(blocked).await, TaskStatus::Doing);
    assert_eq!(status_of(other).await, TaskStatus::Todo, "同批子任务一起回滚");
    drop_block_trigger().await;
}

// ---------- ⑨ 防环：脏数据 A→B→A + 子任务 → 不死循环 ----------

#[tokio::test]
async fn dirty_cycle_terminates() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(a, "T11-9-A", None, "DOING", me).await;
    insert_task(b, "T11-9-B", Some(a), "DOING", me).await;
    insert_task(child, "T11-9-C", Some(a), "TODO", me).await;
    // A → B 之后再回填 B → A（`ck_biz_task_parent_not_self` 只拦自指，拦不住环）
    db().execute_with_params(
        "UPDATE biz_task SET parent_id = ?::uuid WHERE id = ?::uuid",
        vec![Value::from(&b), Value::from(&a)],
    )
    .await
    .expect("夹具造环失败");

    // 这条用例能跑完本身就是断言（成环且没有深度上限时它会不返回）
    let out = cancel_parent(a, me).await;
    assert_eq!(status_of(a).await, TaskStatus::Cancelled);
    assert_eq!(status_of(b).await, TaskStatus::Cancelled);
    assert_eq!(status_of(child).await, TaskStatus::Cancelled);
    assert_eq!(
        ids_of(&out),
        [b, child].into_iter().collect(),
        "环上的任务只算一次（父任务自己不出现在取消名单里）：{out:?}"
    );
}

// ---------- ⑩ 变更日志口径：每个被取消的子任务各一条 + 父任务一条汇总评论 ----------

#[tokio::test]
async fn cancelled_children_are_reported_one_by_one() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let c1 = Uuid::now_v7();
    let c2 = Uuid::now_v7();
    insert_task(parent, "T11-10-P", None, "DOING", me).await;
    insert_task(c1, "T11-10-C1", Some(parent), "TODO", me).await;
    insert_task(c2, "T11-10-C2", Some(parent), "REVIEW", me).await;

    let out = cancel_parent(parent, me).await;
    // handler 按这个列表**逐条**写 `sys_change_log`（父任务那条由既有逻辑写），
    // 所以「列表条数 == 被取消的子任务数」就是「父 1 条 + 子 N 条」的证据。
    // （`sys_change_log` 的端到端断言在 `scripts/verify/t11_cancel_cascade.sh`：
    //   C7 G1 门禁不许 `business/task/` 里出现该表名，只能走 HTTP 接口验。）
    assert_eq!(out.len(), 2, "{out:?}");
    let mut by_id: std::collections::BTreeMap<Uuid, TaskStatus> = Default::default();
    for cancel in &out {
        by_id.insert(cancel.task_id, cancel.from);
    }
    assert_eq!(by_id.get(&c1), Some(&TaskStatus::Todo));
    assert_eq!(by_id.get(&c2), Some(&TaskStatus::Review));

    // 父任务时间线上的汇总评论（handler 在事务里补写）
    insert_comment(
        db(),
        parent,
        me,
        &format!(
            "级联取消：本任务取消后，{} 个未完成的子孙任务已随之取消（{CASCADE_CANCEL_REASON}）",
            out.len()
        ),
        COMMENT_TYPE_STATUS_CHANGE,
        me,
    )
    .await
    .expect("写汇总评论失败");
    let comments = db()
        .find_with_params(
            "SELECT content FROM biz_task_comment WHERE task_id = ?::uuid",
            vec![Value::from(&parent)],
        )
        .await
        .expect("查评论失败");
    let content: String = comments
        .first()
        .expect("必须有一条汇总评论")
        .get_typed("content")
        .expect("取 content");
    assert!(
        content.contains("级联取消") && content.contains('2'),
        "汇总评论要写清取消了多少个：{content}"
    );
}

// ---------- ⑪ 不变式：progress 不受级联取消影响 ----------

#[tokio::test]
async fn cancel_keeps_progress_untouched() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(parent, "T11-11-P", None, "DOING", me).await;
    insert_task(child, "T11-11-C", Some(parent), "DOING", me).await;
    for (id, value) in [(parent, 40_i16), (child, 65)] {
        db().execute_with_params(
            "UPDATE biz_task SET progress = ?::smallint WHERE id = ?::uuid",
            vec![Value::I16(value), Value::from(&id)],
        )
        .await
        .expect("改进度失败");
    }

    cancel_parent(parent, me).await;
    assert_eq!(
        progress_of(parent).await,
        40,
        "红线：progress 公式不受 CANCELLED 影响，级联只写 status"
    );
    assert_eq!(progress_of(child).await, 65);
}
