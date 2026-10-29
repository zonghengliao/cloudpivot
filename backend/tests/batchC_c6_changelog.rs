//! C6「变更日志查询 + 分区运维 + 4 表接入」真库验收（架构 §4.12.3 的 T-CL 场景）。
//!
//! # 用法（144 实测口径）
//!
//! ```bash
//! su postgres -c "createdb -O cloudpivot cloudpivot_c6v"
//! # 先跑一次迁移（起一次实例到「数据库迁移完成」即可退出）
//! export C6_DB_URL="postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_c6v"
//! cargo test --test batchC_c6_changelog -- --nocapture --test-threads=1
//! ```
//!
//! 缺 `C6_DB_URL` 时两个用例都打印 `SKIP` 并返回（`cargo test` 在无库机器上仍全绿）。
//!
//! # 覆盖的场景号（架构 §4.12.3）
//!
//! | 用例 | 断言 |
//! |---|---|
//! | T-CL-02 | 建用户 → `sys_change_log` 1 行，`action=CREATE`，`mobile` 已脱敏 |
//! | T-CL-03 | 改用户 → `changed_fields` **恰好**等于本次实际变更字段 |
//! | T-CL-04 | 传与原值相同的字段 → 该字段**不进** `changed_fields` |
//! | T-CL-05 | 一次请求内两次更新 → **2 行**、`action` 各自不同 |
//! | T-CL-06 | 软删 → `action=DELETE`、`b` 侧字段齐全 |
//! | T-CL-11 | `EXPLAIN` 对象历史只扫**一个**分区（分区裁剪） |
//! | T-CL-13 | 10 并发改同一对象 → **10 行**、无主键冲突 |
//! | 性能 | `GET /timeline` 的服务层查询在 **10 万行**造数上 < 50 ms |
//! | 分区运维 | 预建下月分区 / 清理过期分区（DROP） |
//!
//! # 为什么是「直接调服务层函数」而不是起 HTTP
//!
//! 被测对象是**服务层**（`record_in` + `query::*` + `partition::*`）。handler 只是
//! 把 extractor 的参数搬进来，HTTP 层另有 e2e 脚本覆盖（见交付证据）。这里直连服务层
//! 能精确控制「同一事务内失败」「10 并发」「10 万行造数」这些 HTTP 不好构造的条件。
//!
//! ⚠️ 全部夹具用 `c6-` 前缀，开头结尾各清一次；`cargo test` 多线程下也不会互相踩
//! （整个文件只有两个 `#[tokio::test]`，且分区用例用的是自己造的过期分区名）。

use std::time::Instant;

use alun::prelude::*;
use alun::uuid::Uuid;
use cloudpivot::business::changelog::query;
use cloudpivot::business::changelog::{
    self, Action, ChangeCtx, ChangeTarget, DiffInput, partition,
};
use cloudpivot::shared::error_code;
use serde_json::json;

/// 夹具前缀。
const TAG: &str = "c6-";

/// 初始化全局数据库（进程内只初始化一次）；返回 `false` 表示跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("C6_DB_URL") else {
                println!("SKIP: 未设置 C6_DB_URL（不连真实数据库）");
                return false;
            };
            match Db::connect(&url).await {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    println!("C6 已连接验证库");
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

/// 清掉本文件的夹具（用户行 + 变更日志 + 造数行）。
async fn cleanup() {
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_change_log WHERE resource_id LIKE ?::text",
            vec![Value::from(format!("{TAG}%"))],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_change_log WHERE resource_id LIKE 'perf-%'",
            Vec::new(),
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_user WHERE username LIKE ?::text",
            vec![Value::from(format!("{TAG}%"))],
        )
        .await;
}

/// 上下文（无 HTTP，故无 IP；`operator_name` 固定便于筛选）。
fn ctx() -> ChangeCtx<'static> {
    ChangeCtx::api(None, Some("c6-tester"))
}

/// 业务行快照（与 `system/user/handler.rs::user_snapshot` 同构）。
fn user_snapshot(username: &str, mobile: &str, real_name: &str, status: i16) -> serde_json::Value {
    json!({
        "username": username,
        "real_name": real_name,
        "email": "c6@example.com",
        "mobile": mobile,
        "dept_id": null,
        "status": status,
        "is_locked": false,
        "remark": null,
        "password": "$2b$12$c6fakepasswordhash",
    })
}

/// 在**同一个事务**里插一条 `sys_user` 并记一条 CREATE 日志（复刻接入点的形态）。
async fn create_user_in_tx(user_id: Uuid, username: &str, mobile: &str) -> DbResult<()> {
    let snapshot = user_snapshot(username, mobile, "C6 夹具", 1);
    let name = username.to_string();
    db().transaction(|tx| async move {
        tx.execute_with_params(
            "INSERT INTO sys_user \
                 (id, username, password, real_name, email, mobile, status, login_fail_count, \
                  is_locked, must_change_password, token_version, created_at, updated_at) \
             VALUES (?::uuid, ?::text, ?::text, ?::text, ?::text, ?::text, ?::int, 0, false, false, 0, now(), now())",
            vec![
                Value::from(user_id),
                Value::Text(name),
                Value::Text("$2b$12$c6fakepasswordhash".to_string()),
                Value::Text("C6 夹具".to_string()),
                Value::Text("c6@example.com".to_string()),
                Value::Text(mobile.to_string()),
                // status 走占位符（`?::int`）：与上面 6 个文本参数一起共 7 个
                Value::I32(1),
            ],
        )
        .await?;
        changelog::record_in(
            &tx,
            &ctx(),
            ChangeTarget::of("sys_user", user_id.to_string(), Action::Create, "新增用户"),
            DiffInput::create(&snapshot),
        )
        .await?;
        Ok(())
    })
    .await
}

/// 读某对象的一行日志原文（按时间升序）。
async fn diff_texts(resource_id: &str) -> Vec<String> {
    db().query_sql(
        "SELECT diff::text AS diff FROM sys_change_log \
          WHERE resource = 'sys_user' AND resource_id = ?::text ORDER BY created_at, id"
            .to_string(),
        vec![Value::from(resource_id)],
    )
    .await
    .expect("查询夹具日志")
    .iter()
    .map(|row| row.get_typed::<String>("diff").expect("读 diff"))
    .collect()
}

#[tokio::test]
async fn c6_change_log_scenarios() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    // ─────────────── T-CL-02：建 → 1 行，mobile 脱敏 ───────────────
    let alice = Uuid::now_v7();
    let mobile_raw = "13800001234";
    create_user_in_tx(alice, &format!("{TAG}alice"), mobile_raw)
        .await
        .expect("T-CL-02 建用户 + 记日志");
    let rows = query::count_for_object(db(), "sys_user", &alice.to_string())
        .await
        .expect("统计变更次数");
    assert_eq!(rows, 1, "T-CL-02：建用户必须恰好 1 行日志");
    let first = &diff_texts(&alice.to_string()).await[0];
    assert!(
        first.contains("\"CREATE\""),
        "T-CL-02：action 必须是 CREATE：{first}"
    );
    assert!(
        first.contains("138****1234"),
        "T-CL-02：mobile 必须已脱敏（实际：{first}）"
    );
    assert!(!first.contains(mobile_raw), "T-CL-02：明文手机号绝不能入库");
    println!("T-CL-02 PASS：1 行，mobile=138****1234（原值 {mobile_raw} 未出现）");

    // ─────────────── T-CL-03：改 → changed_fields 恰好等于实际变更 ───────────────
    let before = user_snapshot(&format!("{TAG}alice"), mobile_raw, "C6 夹具", 1);
    let after = user_snapshot(&format!("{TAG}alice"), "13900005678", "C6 夹具改", 1);
    let outcome = changelog::record_in(
        db(),
        &ctx(),
        ChangeTarget::of("sys_user", alice.to_string(), Action::Update, "修改用户"),
        DiffInput::update(&before, &after),
    )
    .await
    .expect("T-CL-03 记录修改");
    let changelog::RecordOutcome::Recorded { changed_fields, .. } = outcome else {
        panic!("T-CL-03：有变化却不写行");
    };
    assert_eq!(
        changed_fields,
        vec!["mobile".to_string(), "real_name".to_string()],
        "T-CL-03：changed_fields 必须恰好是本次改动的字段（升序）"
    );
    println!("T-CL-03 PASS：changed_fields = {changed_fields:?}");

    // ─────────────── T-CL-04：值没变 → 不记该字段（甚至不写行） ───────────────
    let same = user_snapshot(&format!("{TAG}alice"), "13900005678", "C6 夹具改", 1);
    let outcome = changelog::record_in(
        db(),
        &ctx(),
        ChangeTarget::of("sys_user", alice.to_string(), Action::Update, "修改用户"),
        DiffInput::update(&same, &same),
    )
    .await
    .expect("T-CL-04 记录未变更");
    assert_eq!(
        outcome,
        changelog::RecordOutcome::Skipped,
        "T-CL-04：全字段相等必须 Skipped（不写空日志）"
    );
    let partial = user_snapshot(&format!("{TAG}alice"), "13900005678", "C6 夹具改", 1);
    let mut partial_after = partial.clone();
    partial_after["status"] = json!(0);
    let outcome = changelog::record_in(
        db(),
        &ctx(),
        ChangeTarget::of(
            "sys_user",
            alice.to_string(),
            Action::Status,
            "修改用户状态",
        ),
        DiffInput::update(&partial, &partial_after),
    )
    .await
    .expect("T-CL-04 记录状态变更");
    let changelog::RecordOutcome::Recorded { changed_fields, .. } = outcome else {
        panic!("T-CL-04：status 变了必须写行");
    };
    assert_eq!(changed_fields, vec!["status".to_string()]);
    assert!(
        !changed_fields.contains(&"mobile".to_string()),
        "T-CL-04：未变的 mobile 不得出现在 changed_fields"
    );
    println!("T-CL-04 PASS：未变字段被过滤，changed_fields = {changed_fields:?}");

    // ─────────────── T-CL-05：一次请求两次更新 → 2 行、动作不同 ───────────────
    let bob = Uuid::now_v7();
    create_user_in_tx(bob, &format!("{TAG}bob"), "13700001111")
        .await
        .expect("T-CL-05 建用户");
    let b1 = user_snapshot(&format!("{TAG}bob"), "13700001111", "C6 夹具", 1);
    let mut b2 = b1.clone();
    b2["status"] = json!(0);
    changelog::record_in(
        db(),
        &ctx(),
        ChangeTarget::of("sys_user", bob.to_string(), Action::Status, "修改用户状态"),
        DiffInput::update(&b1, &b2),
    )
    .await
    .expect("T-CL-05 第一次");
    let mut b3 = b2.clone();
    b3["dept_id"] = json!("00000000-0000-0000-0000-000000000001");
    changelog::record_in(
        db(),
        &ctx(),
        ChangeTarget::of("sys_user", bob.to_string(), Action::Assign, "调整用户部门"),
        DiffInput::update(&b2, &b3),
    )
    .await
    .expect("T-CL-05 第二次");
    let bob_diff = diff_texts(&bob.to_string()).await;
    assert_eq!(
        bob_diff.len(),
        3,
        "T-CL-05：建 1 + 改 2 = 3 行（一操作一凭证）"
    );
    assert!(
        bob_diff[1].contains("\"STATUS\""),
        "T-CL-05：第 2 行是 STATUS"
    );
    assert!(
        bob_diff[2].contains("\"ASSIGN\""),
        "T-CL-05：第 3 行是 ASSIGN"
    );
    println!("T-CL-05 PASS：3 行，动作分别 CREATE / STATUS / ASSIGN（未合并）");

    // ─────────────── T-CL-06：软删 → action=DELETE，b 侧齐全 ───────────────
    let before_delete = user_snapshot(&format!("{TAG}bob"), "13700001111", "C6 夹具", 0);
    changelog::record_in(
        db(),
        &ctx(),
        ChangeTarget::of("sys_user", bob.to_string(), Action::Delete, "删除用户"),
        DiffInput::delete(&before_delete),
    )
    .await
    .expect("T-CL-06 记录删除");
    let texts = diff_texts(&bob.to_string()).await;
    let delete_row = texts.last().expect("至少一行");
    assert!(
        delete_row.contains("\"DELETE\""),
        "T-CL-06：action 必须是 DELETE"
    );
    assert!(
        delete_row.contains("\"b\": \"137****1111\""),
        "T-CL-06：删除时 b 侧必须带（已脱敏的）原值：{delete_row}"
    );
    assert!(
        delete_row.contains("\"a\": null"),
        "T-CL-06：删除时 a 侧必须为 null"
    );
    println!("T-CL-06 PASS：DELETE + b 侧齐全 + a 侧 null");

    // ─────────────── T-CL-11：分区裁剪（EXPLAIN 只出现一个分区） ───────────────
    let month = partition::month_start(alice_created_at());
    let expected_partition = partition::partition_name(month);
    let plan_rows = db()
        .query_sql(
            format!(
                "EXPLAIN SELECT id FROM sys_change_log \
                  WHERE resource = 'sys_user' AND resource_id = '{}' \
                    AND created_at >= '{}' AND created_at < '{}' \
                  ORDER BY created_at DESC LIMIT 20",
                alice,
                month.format("%Y-%m-%d"),
                partition::shift_month(month, 1).format("%Y-%m-%d"),
            ),
            Vec::new(),
        )
        .await
        .expect("EXPLAIN 对象历史");
    let plan = plan_rows
        .iter()
        .map(|row| row.get_typed::<String>("QUERY PLAN").unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        plan.contains(&expected_partition),
        "T-CL-11：计划里必须出现 {expected_partition}：\n{plan}"
    );
    let other = partition::partition_name(partition::shift_month(month, -1));
    assert!(
        !plan.contains(&other),
        "T-CL-11：上个月分区不该被扫到（{other}）：\n{plan}"
    );
    println!("T-CL-11 PASS：计划只含 {expected_partition}，未出现 {other}");

    // ─────────────── T-CL-13：10 并发改同一对象 → 10 行 ───────────────
    let carol = Uuid::now_v7();
    create_user_in_tx(carol, &format!("{TAG}carol"), "13600002222")
        .await
        .expect("T-CL-13 建用户");
    let mut handles = Vec::new();
    for index in 0..10 {
        let target = carol;
        handles.push(tokio::spawn(async move {
            let before = user_snapshot("c6-carol", "13600002222", "C6 夹具", 1);
            let mut after = before.clone();
            // 每次并发改一个**不同**的值：值相同会被 diff 引擎判成 Skipped（不写空日志），
            // 那条规则是对的，所以这里必须让 10 次都真有变化（T-CL-13 验的是主键不冲突）
            after["real_name"] = json!(format!("C6 并发 {index}"));
            changelog::record_in(
                db(),
                &ctx(),
                ChangeTarget::of(
                    "sys_user",
                    target.to_string(),
                    Action::Status,
                    "修改用户状态",
                ),
                DiffInput::update(&before, &after),
            )
            .await
        }));
    }
    let mut ok = 0;
    for handle in handles {
        match handle.await.expect("并发任务未 panic") {
            Ok(_) => ok += 1,
            Err(error) => println!("T-CL-13 单次失败（可接受，需看原因）：{error}"),
        }
    }
    let carol_rows = query::count_for_object(db(), "sys_user", &carol.to_string())
        .await
        .expect("统计 carol");
    assert_eq!(ok, 10, "T-CL-13：10 个并发写都该成功（UUIDv7 主键不冲突）");
    assert_eq!(
        carol_rows, 11,
        "T-CL-13：建 1 + 并发 10 = 11 行（实测 {carol_rows}）"
    );
    println!("T-CL-13 PASS：10 并发全部成功，共 {carol_rows} 行，无主键冲突");

    // ─────────────── 错误路径：未登记的表必须 62012 ───────────────
    let error = changelog::record(
        db(),
        &ctx(),
        ChangeTarget::of("sys_not_registered", "x-1", Action::Update, "改点啥"),
        DiffInput::update(&json!({"a": 1}), &json!({"a": 2})),
    )
    .await
    .expect_err("未登记的表必须被拒");
    assert_eq!(error.code, error_code::CHANGE_LOG_RESOURCE_UNREGISTERED);
    println!("错误路径 PASS：未登记表 → 62012");

    cleanup().await;
}

/// 取一个「当前时刻」的辅助（避免在断言里直接用 `Utc::now()` 两次取值跨月）。
fn alice_created_at() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[tokio::test]
async fn c6_timeline_performance_and_partitions() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    // ─────────────── 造数：10 万行（含目标对象 20 行） ───────────────
    let start = Instant::now();
    db().execute_with_params("INSERT INTO sys_change_log \
             (id, module, resource, resource_id, action, action_label, diff, changed_fields, created_at) \
         SELECT gen_random_uuid(), 'system', 'sys_user', 'perf-' || (i % 5000), 'UPDATE', '修改用户', \
                jsonb_build_object('v', 1, 'mode', 'WHITELIST', \
                    'fields', jsonb_build_object('mobile', jsonb_build_object('b', '138****0000', 'a', '139****1111', 'masked', true)), \
                    'meta', jsonb_build_object('table', 'sys_user', 'pk', 'perf-' || (i % 5000), 'action', 'UPDATE')), \
                ARRAY['mobile'], \
                date_trunc('month', CURRENT_TIMESTAMP) \
                    + ((i % 3) || ' months')::interval \
                    + ((i % 26) || ' days')::interval \
                    + INTERVAL '1 hour' \
           FROM generate_series(1, 100000) AS i",
        Vec::new(),
    )
    .await
    .expect("造 10 万行");
    db().execute_with_params("ANALYZE sys_change_log", Vec::new())
        .await
        .expect("ANALYZE");
    let total: i64 = db()
        .scalar_sql(
            "SELECT COUNT(*) FROM sys_change_log WHERE resource_id LIKE 'perf-%'".to_string(),
            Vec::new(),
        )
        .await
        .expect("统计造数");
    println!("造数完成：{total} 行，耗时 {:?}", start.elapsed());
    assert!(total >= 90_000, "造数应 ≥ 9 万行（实际 {total}）");

    // ─────────────── 性能：timeline 服务层查询 < 50 ms ───────────────
    let mut samples = Vec::new();
    for _ in 0..5 {
        let began = Instant::now();
        let rows = query::timeline(db(), "sys_user", "perf-42", None, None, 50)
            .await
            .expect("timeline 查询");
        let elapsed = began.elapsed();
        assert_eq!(
            rows.len(),
            20,
            "perf-42 应有 20 条历史（实际 {}）",
            rows.len()
        );
        samples.push(elapsed);
    }
    let best = samples.iter().min().copied().unwrap_or_default();
    println!(
        "timeline 性能：5 次采样 {:?}，最快 {best:?}（阈值 50 ms，10 万行）",
        samples
    );
    assert!(
        best.as_millis() < 50,
        "timeline 必须 < 50 ms（最快 {best:?}）"
    );

    // ─────────────── 分区运维：预建下月 + 清理过期 ───────────────
    let now = chrono::Utc::now();
    let next_month = partition::shift_month(partition::month_start(now), 1);
    let next_name = partition::partition_name(next_month);
    // 造一个「过期」分区（2020_01），验证 DROP 路径真的会删
    let expired_name = "sys_change_log_2020_01".to_string();
    db().execute_with_params(
        &format!(
            "CREATE TABLE IF NOT EXISTS {expired_name} PARTITION OF sys_change_log \
             FOR VALUES FROM ('2020-01-01') TO ('2020-02-01')"
        ),
        Vec::new(),
    )
    .await
    .expect("造过期分区");
    let report = partition::run_once(db(), now, partition::RETENTION_MONTHS)
        .await
        .expect("分区运维");
    println!("分区运维报告：{}", report.summary());
    assert!(
        report.created.contains(&next_name) || report.existing.contains(&next_name),
        "下月分区 {next_name} 必须被创建（实际 created={:?} existing={:?}）",
        report.created,
        report.existing
    );
    assert!(
        report.dropped.contains(&expired_name),
        "过期分区 {expired_name} 必须被 DROP（实际 {:?}）",
        report.dropped
    );
    let remaining = partition::list_partitions(db()).await.expect("分区清单");
    assert!(
        !remaining.iter().any(|info| info.name == expired_name),
        "过期分区应已不存在"
    );
    println!(
        "分区运维 PASS：created={:?} dropped={:?} kept={}（现存 {} 个分区）",
        report.created,
        report.dropped,
        report.kept,
        remaining.len()
    );

    cleanup().await;
}

/// 回滚零残留（验收 #4）：**接入后的真实路径**上，业务写失败 → 日志也一起消失。
///
/// # 怎么造「后置失败」
///
/// 用数据库约束而不是 mock（同 C3 的做法）：在 `sys_user_role` 上加一条
/// `NOT VALID` 的 CHECK，它只拦**新插入**的行。于是事务内的顺序
/// 「插用户 → 写日志 → 绑角色」走到第三步必然失败，业务整体回滚。
///
/// 这个顺序是刻意选的：如果日志写在业务之前就没意义了 —— 要证明的正是
/// 「日志已经写进去了，业务失败后它同样消失」。
#[tokio::test]
async fn c6_rollback_keeps_zero_residue() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    db().execute_with_params(
        "ALTER TABLE sys_user_role DROP CONSTRAINT IF EXISTS ck_c6_force_fail",
        Vec::new(),
    )
    .await
    .expect("清掉可能残留的约束");
    db().execute_with_params(
        "ALTER TABLE sys_user_role ADD CONSTRAINT ck_c6_force_fail CHECK (false) NOT VALID",
        Vec::new(),
    )
    .await
    .expect("加上「插角色绑定必失败」的约束");

    let dave = Uuid::now_v7();
    let username = format!("{TAG}dave");
    let snapshot = user_snapshot(&username, "13500003333", "C6 夹具", 1);
    let name = username.clone();
    let outcome: DbResult<()> = db()
        .transaction(|tx| async move {
            tx.execute_with_params(
                "INSERT INTO sys_user \
                     (id, username, password, real_name, email, mobile, status, login_fail_count, \
                      is_locked, must_change_password, token_version, created_at, updated_at) \
                 VALUES (?::uuid, ?::text, ?::text, ?::text, ?::text, ?::text, ?::int, 0, false, false, 0, now(), now())",
                vec![
                    Value::from(dave),
                    Value::Text(name),
                    Value::Text("$2b$12$c6fakepasswordhash".to_string()),
                    Value::Text("C6 夹具".to_string()),
                    Value::Text("c6@example.com".to_string()),
                    Value::Text("13500003333".to_string()),
                    Value::I32(1),
                ],
            )
            .await?;
            changelog::record_in(
                &tx,
                &ctx(),
                ChangeTarget::of("sys_user", dave.to_string(), Action::Create, "新增用户"),
                DiffInput::create(&snapshot),
            )
            .await?;
            // 第三步必然失败：CHECK(false) 拦下新插入的角色绑定
            tx.execute_with_params(
                "INSERT INTO sys_user_role (user_id, role_id) \
                 VALUES (?::uuid, '00000000-0000-0000-0000-000000000001'::uuid)",
                vec![Value::from(dave)],
            )
            .await?;
            Ok(())
        })
        .await;
    assert!(outcome.is_err(), "第三步必须失败（否则这个用例没测到回滚）");
    println!("回滚用例：事务按预期失败 → {outcome:?}");

    // 干净地收尾（后面的断言不该因为约束还在而受干扰）
    db().execute_with_params(
        "ALTER TABLE sys_user_role DROP CONSTRAINT IF EXISTS ck_c6_force_fail",
        Vec::new(),
    )
    .await
    .expect("去掉约束");

    let users: i64 = db()
        .scalar_sql(
            "SELECT COUNT(*) FROM sys_user WHERE id = ?::uuid".to_string(),
            vec![Value::from(dave)],
        )
        .await
        .expect("查业务行");
    let logs = query::count_for_object(db(), "sys_user", &dave.to_string())
        .await
        .expect("查日志行");
    println!("回滚后：sys_user={users} 行，sys_change_log={logs} 行");
    assert_eq!(users, 0, "业务必须回滚（用户不该存在）");
    assert_eq!(
        logs, 0,
        "sys_change_log 必须零残留（这就是 D11 的核心断言）"
    );

    cleanup().await;
}

// ==================== G6：按 id 查详情必须带分区键（find 的两条分支） ====================

/// `query::find` 的两种形态（带 / 不带 `created_at`）必须返回**同一行**。
///
/// # 为什么 C7 要在这里补一条用例
///
/// 门禁 G6 要求「按 id 查 `sys_change_log`」的语句文本里必须出现分区键 `created_at`
/// （架构 §5.7 R14：主键是 `(id, created_at)`，纯 id 查询会扫全部 39 个月分区）。
/// 为满足它，`find()` 从「拼半句 + 按需追加谓词」改成了两条完整语句。
/// grep 只能证明**文本**里有分区键，证明不了**行为**没变——所以这里做运行时对照：
/// 带时间走单分区、不带时间走全分区兜底，两者结果逐字段一致。
#[tokio::test]
async fn c6_find_detail_with_and_without_created_at() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;

    let eve = Uuid::now_v7();
    let username = format!("{TAG}eve");
    create_user_in_tx(eve, &username, "13600004444")
        .await
        .expect("造夹具用户 + 日志");

    let rows = db()
        .query_sql(
            "SELECT id, created_at FROM sys_change_log \
             WHERE resource = 'sys_user' AND resource_id = ?::text ORDER BY created_at, id"
                .to_string(),
            vec![Value::from(eve.to_string())],
        )
        .await
        .expect("查夹具日志");
    assert_eq!(rows.len(), 1, "夹具应恰有 1 行日志");
    let log_id: Uuid = rows[0].get_typed("id").expect("id");
    let created_at: chrono::DateTime<chrono::Utc> =
        rows[0].get_typed("created_at").expect("created_at");

    // ① 带 created_at：`WHERE created_at = ? AND id = ?` → 只扫一个分区
    let with_key = query::find(db(), log_id, Some(created_at))
        .await
        .expect("带分区键查询不得报错")
        .expect("带分区键应能找到该行");
    // ② 不带 created_at：`WHERE created_at BETWEEN '-infinity' AND 'infinity' AND id = ?`
    let without_key = query::find(db(), log_id, None)
        .await
        .expect("不带分区键查询不得报错")
        .expect("不带分区键也应能找到该行");

    assert_eq!(with_key.id, log_id);
    assert_eq!(without_key.id, log_id);
    assert_eq!(
        with_key.resource_id, without_key.resource_id,
        "两条分支必须返回同一行业务数据"
    );
    assert_eq!(with_key.action, "CREATE");
    assert_eq!(with_key.created_at, created_at);

    // ③ 反向：不存在的 id 必须是 None（不是 Err，也不是「随便找一行」）
    let missing = query::find(db(), Uuid::now_v7(), None)
        .await
        .expect("查不存在的 id 不得报错");
    assert!(missing.is_none(), "不存在的 id 必须返回 None");

    println!(
        "[PASS] c6_find_detail_with_and_without_created_at: 两条分支同一行（{}），缺失返回 None",
        log_id
    );
    cleanup().await;
}
