//! 管理员人工终止（E2E01-FIX-3）的**真库**用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 SKIP（与本仓其它真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//!   cargo test --lib business::flow::terminate_db_tests -- --test-threads=1
//! ```
//!
//! # 为什么这些用例必须走真库
//!
//! 本卡交付的是「**卡住的实例有出路**」：实例落 `CANCELLED`、待办被撤、已审批的人收到通知、
//! 关联单据退回。这些都是**多表写入 + 库级 CHECK**（`ck_biz_flow_instance_end`
//! 要求 `(status='PENDING') = (ended_at IS NULL)`、`ck_biz_flow_task_done` 同款），
//! 纯逻辑单测证明不了「真的写进去了、真的没留下 PENDING 待办」。
//!
//! # 覆盖（卡面验收逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | ② | `terminate_moves_pending_instance_to_cancelled` | 实例 `PENDING → CANCELLED` + `ended_at` + `result_comment` = 原因 |
//! | ② | …同上 | 名下 `PENDING` 待办全部 `CANCELLED`、已 `APPROVED` 的票**不动**（审计要留） |
//! | ③ | …同上 | 活动节点 `ACTIVE → CANCELLED`（不留「处理中」的孤儿节点） |
//! | ⑤ | …同上 | 通知收件人 = 发起人 + **已审批过的人**（去重、排除操作人） |
//! | ⑥ | `terminate_rejects_finished_instance` | 终态实例 → 引擎回 `409 + 65024`，**一个字段都不改** |
//! | — | `terminate_missing_instance_is_404` | 实例不存在 → `404 + 65003` |
//! | — | `normalize_reason_requires_text` | 空 / 纯空白原因 → 400 口径的校验错误（纯逻辑） |
//!
//! # 夹具口径
//!
//! 用户用 `TERM3_` 前缀新建（不给任何角色 ⇒ 不影响「超管人数」类断言），
//! 流程定义 / 版本用 `term3_` 前缀独立库内对象，清理按主键 id 逆外键序删。

use nik::prelude::*;
use nik::uuid::Uuid;

use super::engine;

/// 真库用例串行（共用全局 `db()`）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("ERP_DB_URL") else {
                println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                return false;
            };
            if try_db().is_some() {
                return true;
            }
            let mut config = DatabaseConfig::from_url(&url);
            // 每个 `#[tokio::test]` 是独立 runtime，`db()` 是进程级全局池：
            // `max_lifetime = 0` 逼池子每次新建连接（与 flow 其它真库用例同口径）。
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
                    println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

/// 一个用例独占的夹具。
struct Fixture {
    def_id: Uuid,
    version_id: Uuid,
    instance_id: Uuid,
    /// 发起人（会被通知）
    initiator: Uuid,
    /// 已经投过同意票的人（会被通知）
    approver: Uuid,
    /// 还在等的人（待办被撤）
    waiter: Uuid,
    /// 流程管理员（执行终止的人，**不该**收到通知）
    operator: Uuid,
    node_id: Uuid,
    approved_task_id: Uuid,
    pending_task_id: Uuid,
}

impl Fixture {
    /// 建「定义 + 版本 + 实例 + 1 个活动节点 + 3 条待办」。
    ///
    /// `instance_status`：`PENDING`（默认可终止）或任一终态（用于「已结束不能终止」）；
    /// `with_countersign`：是否额外建一条 `SKIPPED` 票（不该进通知名单）。
    async fn new(instance_status: &str, approved_count: usize) -> Self {
        let initiator = insert_user("init").await;
        let approver = insert_user("apr").await;
        let waiter = insert_user("wait").await;
        let operator = insert_user("op").await;

        let def_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let instance_id = Uuid::now_v7();
        let node_id = Uuid::now_v7();
        let suffix = instance_id.to_string().to_ascii_uppercase();
        let suffix = &suffix[suffix.len() - 8..];

        db().execute_with_params(
            "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, status) \
             VALUES (?::uuid, ?, 'TERM3 测试流程', 'TERM3_TEST', 'DRAFT')",
            vec![
                Value::from(&def_id),
                Value::Text(format!("term3_{}", suffix.to_ascii_lowercase())),
            ],
        )
        .await
        .expect("插流程定义");

        db().execute_with_params(
            "INSERT INTO biz_flow_def_version (id, def_id, version_no, version_status) \
             VALUES (?::uuid, ?::uuid, 1, 'DRAFT')",
            vec![Value::from(&version_id), Value::from(&def_id)],
        )
        .await
        .expect("插流程版本");

        // 实例：PENDING ⇒ ended_at 必须为 NULL（ck_biz_flow_instance_end 等值约束）
        db().execute_with_params(
            "INSERT INTO biz_flow_instance \
                 (id, instance_no, def_id, version_id, version_no, flow_code, flow_name, \
                  biz_type, title, status, initiator_id, ended_at) \
             VALUES (?::uuid, ?, ?::uuid, ?::uuid, 1, 'term3_test_flow', 'TERM3 测试流程', \
                     'TERM3_TEST', 'TERM3 会签缺人夹具', ?, ?::uuid, \
                     CASE WHEN ? = 'PENDING' THEN NULL ELSE now() END)",
            vec![
                Value::from(&instance_id),
                Value::Text(format!("TERM3-{suffix}")),
                Value::from(&def_id),
                Value::from(&version_id),
                Value::Text(instance_status.to_string()),
                Value::from(&initiator),
                Value::Text(instance_status.to_string()),
            ],
        )
        .await
        .expect("插审批实例");

        // 活动节点（会签：应到 3 人）
        let node_status = if instance_status == "PENDING" {
            "ACTIVE"
        } else {
            "CANCELLED"
        };
        db().execute_with_params(
            "INSERT INTO biz_flow_instance_node \
                 (id, instance_id, node_key, node_name, node_type, round, status, \
                  approval_mode, approver_count, approved_count, activated_at) \
             VALUES (?::uuid, ?::uuid, 'n_cs', '会签复核', 'COUNTERSIGN', 1, ?, 'ALL', 3, ?, \
                     CASE WHEN ? = 'ACTIVE' THEN now() ELSE NULL END)",
            vec![
                Value::from(&node_id),
                Value::from(&instance_id),
                Value::Text(node_status.to_string()),
                Value::I32(approved_count as i32),
                Value::Text(node_status.to_string()),
            ],
        )
        .await
        .expect("插实例节点");

        // 已审批的那一票（APPROVED）
        let approved_task_id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_flow_task \
                 (id, instance_id, instance_node_id, node_key, round, assignee_id, \
                  assignee_name, assignee_source, status, completed_at) \
             VALUES (?::uuid, ?::uuid, ?::uuid, 'n_cs', 1, ?::uuid, 'TERM3 已审人', 'USER', \
                     'APPROVED', now())",
            vec![
                Value::from(&approved_task_id),
                Value::from(&instance_id),
                Value::from(&node_id),
                Value::from(&approver),
            ],
        )
        .await
        .expect("插已审批待办");

        // 还在等的那一票（PENDING）—— 终止后必须被撤
        let pending_task_id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_flow_task \
                 (id, instance_id, instance_node_id, node_key, round, assignee_id, \
                  assignee_name, assignee_source, status) \
             VALUES (?::uuid, ?::uuid, ?::uuid, 'n_cs', 1, ?::uuid, 'TERM3 待审人', 'USER', \
                     'PENDING')",
            vec![
                Value::from(&pending_task_id),
                Value::from(&instance_id),
                Value::from(&node_id),
                Value::from(&waiter),
            ],
        )
        .await
        .expect("插待审批待办");

        Self {
            def_id,
            version_id,
            instance_id,
            initiator,
            approver,
            waiter,
            operator,
            node_id,
            approved_task_id,
            pending_task_id,
        }
    }

    /// 实例当前状态。
    async fn instance_status(&self) -> String {
        db().scalar_with_params::<String>(
            "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&self.instance_id)],
        )
        .await
        .expect("查实例状态")
    }

    /// 实例是否已结束 + 结束说明。
    async fn instance_end(&self) -> (bool, Option<String>) {
        let rows = db()
            .find_with_params(
                "SELECT (ended_at IS NOT NULL) AS has_end, result_comment \
                   FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&self.instance_id)],
            )
            .await
            .expect("查实例结束信息");
        let row = rows.first().expect("实例应当存在");
        (
            row.get_typed("has_end").expect("has_end"),
            row.get_typed("result_comment").expect("result_comment"),
        )
    }

    /// 某条待办的状态。
    async fn task_status(&self, task_id: Uuid) -> String {
        db().scalar_with_params::<String>(
            "SELECT status FROM biz_flow_task WHERE id = ?::uuid",
            vec![Value::from(&task_id)],
        )
        .await
        .expect("查待办状态")
    }

    /// 活动节点状态。
    async fn node_status(&self) -> String {
        db().scalar_with_params::<String>(
            "SELECT status FROM biz_flow_instance_node WHERE id = ?::uuid",
            vec![Value::from(&self.node_id)],
        )
        .await
        .expect("查节点状态")
    }

    async fn cleanup(&self) {
        // 逆外键序：实例（级联删节点/待办）→ 版本 → 定义 → 用户
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&self.instance_id)],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_flow_def_version WHERE id = ?::uuid",
                vec![Value::from(&self.version_id)],
            )
            .await;
        let _ = db()
            .execute_with_params(
                "DELETE FROM biz_flow_def WHERE id = ?::uuid",
                vec![Value::from(&self.def_id)],
            )
            .await;
        for user in [self.initiator, self.approver, self.waiter, self.operator] {
            let _ = db()
                .execute_with_params(
                    "DELETE FROM sys_user_role WHERE user_id = ?::uuid",
                    vec![Value::from(&user)],
                )
                .await;
            let _ = db()
                .execute_with_params("DELETE FROM sys_user WHERE id = ?::uuid", vec![Value::from(&user)])
                .await;
        }
    }
}

/// 建一个不带任何角色的夹具用户（`TERM3_` 前缀，用户名唯一）。
async fn insert_user(tag: &str) -> Uuid {
    let id = Uuid::now_v7();
    let suffix = id.to_string().to_ascii_uppercase();
    let username = format!("TERM3_{tag}_{}", &suffix[suffix.len() - 8..]);
    db().execute_with_params(
        "INSERT INTO sys_user (id, username, password, real_name) \
         VALUES (?::uuid, ?, 'placeholder-not-a-real-hash', ?)",
        vec![
            Value::from(&id),
            Value::Text(username),
            Value::Text(format!("TERM3 {tag}")),
        ],
    )
    .await
    .expect("插夹具用户");
    id
}

// ==================== ① 纯逻辑：终止原因必填 ====================

#[test]
fn normalize_reason_requires_text() {
    use super::dto::normalize_terminate_reason;

    assert!(normalize_terminate_reason("").is_err(), "空原因必须挡下");
    assert!(normalize_terminate_reason("   \n\t ").is_err(), "纯空白必须挡下");
    assert_eq!(
        normalize_terminate_reason("  会签节点缺人，人工终止  ").expect("合法原因"),
        "会签节点缺人，人工终止",
        "首尾空白要去掉"
    );
    let too_long = "终".repeat(super::dto::MAX_FLOW_COMMENT_CHARS + 1);
    assert!(normalize_terminate_reason(&too_long).is_err(), "超长必须挡下");
}

// ==================== ② 主路径：PENDING → CANCELLED ====================

/// 终止一个「会签 3 人、只 1 人会签」的卡住实例：实例落 `CANCELLED`、
/// 待办被撤、已审的票留着、节点收口、发起人 + 已审批过的人收到通知。
#[tokio::test]
async fn terminate_moves_pending_instance_to_cancelled() {
    let _guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return;
    }
    let fx = Fixture::new("PENDING", 1).await;

    // 事务内执行（与 handler 的 `db().transaction(...)` 同口径）
    let out = db()
        .transaction(|tx| {
            let instance_id = fx.instance_id;
            let operator = fx.operator;
            async move {
                engine::terminate_by_admin(
                    &tx,
                    instance_id,
                    operator,
                    "会签节点缺人（3 人会签只有 1 人在职），人工终止",
                )
                .await
            }
        })
        .await
        .expect("终止不应抛库错误");

    assert!(out.rejected.is_none(), "PENDING 实例不该被引擎拒绝");
    assert_eq!(out.instance_status, "CANCELLED", "行政终止的口径是 CANCELLED");

    // 实例：状态 + 结束时间 + 结束说明（= 终止原因，详情页直接显示）
    assert_eq!(fx.instance_status().await, "CANCELLED");
    let (has_end, result_comment) = fx.instance_end().await;
    assert!(has_end, "终态实例必须有结束时间（ck_biz_flow_instance_end）");
    assert_eq!(
        result_comment.as_deref(),
        Some("会签节点缺人（3 人会签只有 1 人在职），人工终止"),
        "终止原因要落进 result_comment"
    );

    // 待办：还在等的那张被撤；已投的票保留（审计要看「谁批过」）
    assert_eq!(fx.task_status(fx.pending_task_id).await, "CANCELLED");
    assert_eq!(
        fx.task_status(fx.approved_task_id).await,
        "APPROVED",
        "已经投过的票不许被终止改成 CANCELLED"
    );

    // 节点：不留「处理中」的孤儿
    assert_eq!(fx.node_status().await, "CANCELLED");

    // 通知：收件人 = 发起人 + 已审批过的人，且排除操作人自己
    assert_eq!(out.effects.notices.len(), 1, "终止只该有一条结果通知");
    let recipients = &out.effects.notices[0].recipients;
    assert!(recipients.contains(&fx.initiator), "发起人必须收到");
    assert!(recipients.contains(&fx.approver), "已审批过的人必须收到");
    assert!(
        !recipients.contains(&fx.waiter),
        "还没投票的人由「待办被撤」感知，不塞进终止通知"
    );
    assert!(
        !recipients.contains(&fx.operator),
        "执行终止的人不需要收到「我终止了」的通知"
    );
    assert!(
        out.effects.notices[0].content.contains("人工终止"),
        "通知正文要带原因：{}",
        out.effects.notices[0].content
    );

    fx.cleanup().await;
}

// ==================== ③ 终态实例不能再终止 ====================

/// 已结束的实例调终止 → 引擎回 `409 + 65024`，且**一个字段都不改**
/// （handler 层会把它翻成对外的 400，见 `handler::terminate_instance` 的注释）。
#[tokio::test]
async fn terminate_rejects_finished_instance() {
    let _guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return;
    }
    let fx = Fixture::new("APPROVED", 1).await;

    let out = db()
        .transaction(|tx| {
            let instance_id = fx.instance_id;
            let operator = fx.operator;
            async move { engine::terminate_by_admin(&tx, instance_id, operator, "过期后再点一次").await }
        })
        .await
        .expect("引擎以 rejected 表达业务拒绝，不抛库错误");

    let error = out.rejected.expect("终态实例必须被拒");
    assert_eq!(error.status, 409, "引擎口径是 409（handler 对外翻成 400）");
    assert_eq!(error.code, crate::shared::error_code::FLOW_INSTANCE_NOT_PENDING);
    assert_eq!(fx.instance_status().await, "APPROVED", "拒绝时不许改状态");
    assert_eq!(fx.task_status(fx.pending_task_id).await, "PENDING", "拒绝时不许动待办");

    fx.cleanup().await;
}

// ==================== ④ 实例不存在 ====================

/// 实例 id 不存在 → `404 + 65002`。
#[tokio::test]
async fn terminate_missing_instance_is_404() {
    let _guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return;
    }
    let ghost = Uuid::now_v7();
    let operator = Uuid::now_v7();

    let out = db()
        .transaction(|tx| async move { engine::terminate_by_admin(&tx, ghost, operator, "x").await })
        .await
        .expect("不存在的实例不抛库错误");

    let error = out.rejected.expect("必须回 404");
    assert_eq!(error.status, 404);
    assert_eq!(error.code, crate::shared::error_code::FLOW_INSTANCE_NOT_FOUND);
}
