// ============================================================================
// task_handler_copy_db_tests.rs —— 复制任务真库用例（D9）
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::task::dto;
use crate::business::task::model;

use super::*;

use crate::business::task::model::NewTask;

/// 夹具用户（复制人 = 创建人也 = 超管，范围 = 全部）。
const U_ME: &str = "d9c00001-0000-4000-8000-000000000001";

/// 真库用例串行（共用全局 `db()` 与任务取号器）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("ERP_DB_URL").or_else(|_| std::env::var("TASK_DB_URL"))
            else {
                println!("SKIP: 未设置 ERP_DB_URL / TASK_DB_URL（不连真实数据库）");
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
                    println!("SKIP: 数据库连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    let me = uuid(U_ME);
    db().execute_with_params(
        "INSERT INTO sys_user (id, username, password, real_name) \
         VALUES (?::uuid, 'd9_copy_user', 'x', 'D9 复制人') ON CONFLICT DO NOTHING",
        vec![Value::from(&me)],
    )
    .await
    .expect("夹具用户写入失败");
    // 超管角色 → `principal::resolve` 判 super（范围 = 全部），否则 find_detail 会把任务挡成 404
    db().execute_with_params(
        "INSERT INTO sys_user_role (user_id, role_id) \
         SELECT ?::uuid, r.id FROM sys_role r WHERE r.role_code = ? \
         ON CONFLICT DO NOTHING",
        vec![
            Value::from(&me),
            Value::Text(crate::shared::constants::SUPER_ADMIN_ROLE_CODE.to_string()),
        ],
    )
    .await
    .expect("夹具角色绑定失败");
    // 清上一轮：评论 → 断父指针 → 任务（自引用 FK 非 deferrable，必须先断开）
    db().execute_with_params(
        "DELETE FROM biz_task_comment WHERE task_id IN \
         (SELECT id FROM biz_task WHERE creator_id = ?::uuid)",
        vec![Value::from(&me)],
    )
    .await
    .expect("清理评论失败");
    db().execute_with_params(
        "UPDATE biz_task SET parent_id = NULL WHERE creator_id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("清理父指针失败");
    db().execute_with_params(
        "DELETE FROM biz_task WHERE creator_id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("清理任务失败");
    Some(guard)
}

fn claims_with(perms: &[&str]) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: U_ME.to_string(),
        username: Some("d9_copy_user".to_string()),
        roles: Vec::new(),
        permissions: perms.iter().map(|p| (*p).to_string()).collect(),
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
}

/// 建一张任务（走 `model::create_task`，与生产同一条落库路径），返回 `(id, task_no)`。
async fn seed_task(
    title: &str,
    parent: Option<Uuid>,
    priority: &str,
    estimate: Option<f64>,
    need_review: bool,
) -> (Uuid, String) {
    let me = uuid(U_ME);
    let id = Uuid::now_v7();
    let new = NewTask {
        id,
        title: title.to_string(),
        description: Some("D9 复制用例的描述".to_string()),
        priority: priority.to_string(),
        need_review,
        owner_id: me,
        creator_id: me,
        assigner_id: None,
        dept_id: None,
        parent_id: parent,
        project_id: None,
        doc_type: None,
        doc_id: None,
        start_date: None,
        due_date: None,
        estimate_hours: estimate,
        source: "SELF".to_string(),
        add_creator_as_collaborator: false,
    };
    let month = model::month_key(model::today_cst());
    let task_no = db()
        .transaction(|tx| async move { model::create_task(&tx, &new, me, &month).await })
        .await
        .expect("夹具任务建不出来");
    (id, task_no)
}

async fn count(sql: &str, id: Uuid) -> i64 {
    db().scalar_with_params(sql, vec![Value::from(&id)])
        .await
        .expect("计数失败")
}

/// ⑤ 复制任务：标题带「（副本）」、负责人/优先级/工时一致，子任务与评论不复制。
#[tokio::test]
async fn d9_task_copy_keeps_fields_but_not_children() {
    let Some(_guard) = setup().await else {
        return;
    };
    let me = uuid(U_ME);
    let (origin_id, origin_no) = seed_task("D9 月度对账", None, "P1", Some(3.5), true).await;
    // 子任务 + 评论：复制时必须一个都不带
    let (_child_id, _child_no) =
        seed_task("D9 月度对账-子任务", Some(origin_id), "P2", None, false).await;
    db().execute_with_params(
        "INSERT INTO biz_task_comment \
             (id, task_id, user_id, content, mentions, parent_comment_id, comment_type, \
              created_at, updated_at, create_by, update_by) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, 'D9 源任务的评论', '[]'::jsonb, NULL, \
                 'PLAIN', now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&origin_id),
            Value::from(&me),
            Value::from(&me),
            Value::from(&me),
        ],
    )
    .await
    .expect("夹具评论写入失败");

    let before = model::find_raw(origin_id)
        .await
        .unwrap()
        .expect("原任务必须存在");

    let new_id = copy_task(origin_id, &claims_with(&["*:*:*"]))
        .await
        .expect("复制任务应成功");
    let copied = model::find_raw(new_id)
        .await
        .unwrap()
        .expect("副本必须存在");

    assert_ne!(copied.card.id, before.card.id);
    assert_ne!(copied.card.task_no, origin_no, "任务编号必须是新的");
    assert!(
        copied.card.task_no.starts_with("TSK-"),
        "{}",
        copied.card.task_no
    );
    assert_eq!(
        copied.card.title,
        format!("{}{}", before.card.title, COPY_TITLE_SUFFIX),
        "卡面 ⑤：标题要带（副本）"
    );
    assert_eq!(copied.description, before.description, "描述要带出");
    assert_eq!(copied.card.priority, "P1", "优先级要带出");
    assert_eq!(copied.card.owner_id, me, "负责人要带出");
    assert_eq!(
        copied.estimate_hours,
        Some(3.5),
        "预估工时（NUMERIC(6,1)）要带出"
    );
    assert!(copied.card.need_review, "需验收标记要带出");
    assert_eq!(copied.card.status, dto::STATUS_TODO, "副本从 TODO 开始");
    assert_eq!(copied.card.progress, 0, "进度归零");
    assert!(copied.card.parent_id.is_none(), "副本是独立新任务");
    assert!(copied.done_at.is_none());
    assert!(copied.approval_instance_id.is_none());
    // 子任务 / 评论一个都没复制
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_task WHERE parent_id = ?::uuid",
            new_id
        )
        .await,
        0,
        "子任务不许复制"
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_task_comment WHERE task_id = ?::uuid",
            new_id
        )
        .await,
        0,
        "评论不许复制"
    );
    // 原任务一个字都不改（标题 / 状态 / 编号 / updated_at）
    let after = model::find_raw(origin_id)
        .await
        .unwrap()
        .expect("原任务还在");
    assert_eq!(after.card.title, before.card.title);
    assert_eq!(after.card.status, before.card.status);
    assert_eq!(after.card.task_no, before.card.task_no);
    assert_eq!(
        after.card.updated_at, before.card.updated_at,
        "原任务 updated_at 不许变"
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_task WHERE parent_id = ?::uuid",
            origin_id
        )
        .await,
        1,
        "原任务的子任务还在"
    );
}

/// ⑤b 权限：没有 `business:task:create` → 403（复制 = 建单）；权限码与建单同一个。
#[tokio::test]
async fn d9_task_copy_requires_create_permission() {
    let Some(_guard) = setup().await else {
        return;
    };
    assert_eq!(COPY_PERMISSION, "business:task:create");
    let (origin_id, _) = seed_task("D9 权限用例", None, "P2", None, false).await;

    let err = copy_task(origin_id, &claims_with(&[dto::PERM_TASK_READ]))
        .await
        .expect_err("没有建单权限必须 403");
    assert_eq!(err.status, 403, "{}", err.msg);
    assert!(
        err.msg.contains("business:task:create"),
        "403 文案要点名权限码：{}",
        err.msg
    );
    // 一个副本都不许建出来（仍只有原任务 + 它自己）
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_task WHERE creator_id = ?::uuid",
            uuid(U_ME)
        )
        .await,
        1
    );
}

/// ㉓ 收尾清场：本模块的**超管夹具用户**必须删干净（t_06c528fc）。
///
/// # 为什么这条不是「测试卫生」而是真 bug
///
/// 审批节点的 `assignee_source = ADMIN` 会**动态展开成「当前全部超管」**。
/// 这个夹具用户（`d9_copy_user`）挂着 `SUPER_ADMIN` 而 `setup()` 从不删他，
/// 于是**全量套件在同一个库上跑第二遍**时：
/// `approve_flow_instance` 只批了「第一条待办」，实例还剩一条待办 → 停在 `PENDING`
/// → transfer / stockin / voucher 的「审批通过即落终态」用例集体变红
/// （2026-11-02 实测 **53 条**，与 D7 交接里记的「同一库连跑第二遍红 53 条」完全同源）。
///
/// 名字以 `zz_` 开头 → 排序排在本模块最后，等于给模块加了 teardown。
#[tokio::test]
async fn zz_teardown_purges_super_admin_fixture_user() {
    let Some(_guard) = setup().await else {
        return;
    };
    let me = uuid(U_ME);
    // ① 子表先清（评论 / 工时 / 成员都挂在任务或用户上）
    db().execute_with_params(
        "DELETE FROM biz_task_comment WHERE user_id = ?::uuid \
            OR task_id IN (SELECT id FROM biz_task WHERE creator_id = ?::uuid OR owner_id = ?::uuid)",
        vec![Value::from(&me), Value::from(&me), Value::from(&me)],
    )
    .await
    .expect("清场删评论失败");
    db().execute_with_params(
        "DELETE FROM biz_task_worklog WHERE user_id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("清场删工时失败");
    db().execute_with_params(
        "DELETE FROM biz_task_member WHERE user_id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("清场删任务成员失败");
    // ② 自引用 FK 先断开，再删任务
    db().execute_with_params(
        "UPDATE biz_task SET parent_id = NULL \
          WHERE parent_id IN (SELECT id FROM biz_task WHERE creator_id = ?::uuid OR owner_id = ?::uuid)",
        vec![Value::from(&me), Value::from(&me)],
    )
    .await
    .expect("断开父指针失败");
    db().execute_with_params(
        "DELETE FROM biz_task WHERE creator_id = ?::uuid OR owner_id = ?::uuid OR assigner_id = ?::uuid",
        vec![Value::from(&me), Value::from(&me), Value::from(&me)],
    )
    .await
    .expect("清场删任务失败");
    // ③ 角色关联 → 用户
    db().execute_with_params(
        "DELETE FROM sys_user_role WHERE user_id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("清场删角色关联失败");
    db().execute_with_params(
        "DELETE FROM sys_user WHERE id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("清场删用户失败");

    // ④ 断言：用户 / 角色关联 / 任务 全为 0
    assert_eq!(
        count("SELECT count(*) FROM sys_user WHERE id = ?::uuid", me).await,
        0,
        "超管夹具用户必须删掉（留着会让兄弟模块的第二遍全量变红）"
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM sys_user_role WHERE user_id = ?::uuid",
            me
        )
        .await,
        0,
        "角色关联必须删掉"
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_task WHERE creator_id = ?::uuid",
            me
        )
        .await,
        0,
        "夹具任务必须删掉"
    );
}
