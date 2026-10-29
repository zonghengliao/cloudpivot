//! 工作任务管理 T5「AI 执行者规则」的真库集成测试（**需要真实 PostgreSQL**，已迁移到 V0111）。
//!
//! # 这个文件证明什么（其余靠 HTTP 实测与纯单测）
//!
//! | 用例 | 证明什么 | 为什么只有真库能证 |
//! |------|----------|--------------------|
//! | `migration_shape` | V0111 的列 / 默认值 / NOT NULL / CHECK / 部分索引都在 | 结构在库里，不在代码里 |
//! | `record_writes_robot_operator_type` | ROBOT 操作人写的日志行 `operator_type='ROBOT'`（卡面验收 8） | 必须走 `record` → `INSERT` 的真实链路 |
//! | `record_writes_human_operator_type` | 人类操作人 → `'HUMAN'`（卡面验收 9）+ 对照上一条不是「怎么写都是 ROBOT」 | 同上 |
//! | `operator_type_of_defaults` | 解析器：`None`（系统任务）/ 不存在的账号 → `HUMAN`；ROBOT → `ROBOT` | 查的是 `sys_user` |
//! | `explicit_operator_type_skips_lookup` | `with_operator_type` 的逃生口真的能覆盖查库结果 | 写入值只在库里看得见 |
//!
//! 四条**硬拒**（`63201`/`63202`/`63203`/`63204`）的判定逻辑是纯函数，
//! 单测在 `src/shared/ai_actor.rs`；真实 HTTP 行为（403 + 错误码 + `auth_logs` 留痕）
//! 由 `scripts/t5_e2e.py` 在跑起来的实例上断言 —— 这里不重复造一半的 HTTP 客户端。
//!
//! # 用法
//!
//! ```bash
//! set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
//! export T5_DB_URL="postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_test"
//! cargo test --test batchT_t5_ai_rules -- --nocapture --test-threads=1
//! ```
//!
//! 缺 `T5_DB_URL`（且没有 `ALUN_DATABASE_*`）时打印 `SKIP` 并直接返回：
//! `cargo test` 在无库环境仍然全绿。
//!
//! # 夹具与清理
//!
//! 固定 UUID（`t5-b001` / `t5-h001` 段）+ `t5_` 前缀用户名：每个用例开头清一次、
//! 结尾清一次，只删自己造的行（`sys_change_log` 按 `resource_id`、`sys_user` 按 id）。
//! `sys_change_log` 是分区表，清理按 `resource_id` 走 `purge_for_object`（业务代码
//! 不许删审计表，所以用测试专用函数）。

use alun::prelude::*;
use alun::uuid::Uuid;

use cloudpivot::business::changelog::model::NewChangeLog;
use cloudpivot::business::changelog::{
    self, Action, ChangeCtx, ChangeTarget, DiffInput, OperatorType,
};

// ==================== 夹具 ====================

/// ROBOT 操作人（AI 执行者）。
const U_ROBOT: &str = "5e000000-0000-0000-0000-00000000b001";
/// HUMAN 操作人。
const U_HUMAN: &str = "5e000000-0000-0000-0000-00000000b002";
/// 日志行的业务主键（`resource_id`），清理按它匹配。
const TARGET_ID: &str = "t5-log-target-001";

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

/// 串行闸：本文件的用例共用一个 `sys_user` 夹具（固定 UUID + 固定用户名），
/// `cargo test` 默认并行跑会让「A 清理、B 插入」交错，报
/// `duplicate key value violates unique constraint "uk_sys_user_username"`（实测）。
///
/// 每个用例第一行取锁；**清理函数不取锁**（同一任务内再取会自锁，C4 踩过）。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn robot_id() -> Uuid {
    uuid(U_ROBOT)
}

fn human_id() -> Uuid {
    uuid(U_HUMAN)
}

/// 连接真实数据库（`T5_DB_URL` 优先，其次拼 `ALUN_DATABASE_*`；都没有就 SKIP）。
async fn connect() -> Option<Db> {
    let url = if let Ok(url) = std::env::var("T5_DB_URL") {
        url
    } else if let (Ok(user), Ok(name)) = (
        std::env::var("ALUN_DATABASE_USER"),
        std::env::var("ALUN_DATABASE_NAME"),
    ) {
        let host = std::env::var("ALUN_DATABASE_HOST").unwrap_or_else(|_| "127.0.0.1".into());
        let port = std::env::var("ALUN_DATABASE_PORT").unwrap_or_else(|_| "5432".into());
        let password = std::env::var("ALUN_DATABASE_PASSWORD").unwrap_or_default();
        if password.is_empty() {
            format!("postgres://{user}@{host}:{port}/{name}")
        } else {
            format!("postgres://{user}:{password}@{host}:{port}/{name}")
        }
    } else {
        println!("SKIP: 未设置 T5_DB_URL / ALUN_DATABASE_*（不连真实数据库）");
        return None;
    };
    match Db::connect(&url).await {
        Ok(connection) => Some(connection),
        Err(error) => {
            println!("SKIP: 连接数据库失败：{error}");
            None
        }
    }
}

/// 清理夹具（只删本文件造的行；重复调用安全）。
async fn cleanup(db: &Db) {
    let _ = changelog::model::purge_for_object(db, "sys_user", TARGET_ID).await;
    let _ = db
        .exec_sql(
            "DELETE FROM sys_ai_agent WHERE user_id IN (?::uuid, ?::uuid)".to_string(),
            vec![Value::from(robot_id()), Value::from(human_id())],
        )
        .await;
    let _ = db
        .exec_sql(
            "DELETE FROM sys_user WHERE id IN (?::uuid, ?::uuid)".to_string(),
            vec![Value::from(robot_id()), Value::from(human_id())],
        )
        .await;
}

/// 造两个操作人（一个 ROBOT、一个 HUMAN）。
///
/// # 为什么不走 `business/aiagent` 的建号接口
///
/// 那会调 Hermes CLI（本机没有 / 慢 / 有副作用）。本文件要验证的只是
/// 「`sys_user.user_type` 是 ROBOT 时日志行落 `ROBOT`」，直接插两行最诚实：
/// 不引入 CLI、不依赖 profile 目录。
///
/// `sys_ai_agent` 那行**也一起插**：不变式（修正 1）要求 `ROBOT` ⇔ 有配置行，
/// 虽然本测试不起服务、不跑启动自检，但留下一个「ROBOT 无配置」的行会让
/// 下一次真正启动 test 实例时自检报警 —— 自己造的脏数据自己收拾干净。
async fn seed_operators(db: &Db) -> DbResult<()> {
    for (id, username, user_type) in [
        (robot_id(), "t5_bot", "ROBOT"),
        (human_id(), "t5_human", "HUMAN"),
    ] {
        db.exec_sql(
            "INSERT INTO sys_user (id, username, password, real_name, status, user_type) \
             VALUES (?::uuid, ?::text, ?::text, ?::text, 1, ?::text) \
             ON CONFLICT (id) DO UPDATE SET user_type = EXCLUDED.user_type, \
                                            deleted_at = NULL"
                .to_string(),
            vec![
                Value::from(id),
                Value::from(username),
                // 机器人/夹具账号不登录，口令给一个不可解的占位串（不是任何真实哈希）
                Value::from("!fixture-no-login"),
                Value::from(username),
                Value::from(user_type),
            ],
        )
        .await?;
    }
    // 不变式：ROBOT ⇔ sys_ai_agent 有行（PK 是 user_id，没有独立 id 列）
    db.exec_sql(
        "INSERT INTO sys_ai_agent \
             (user_id, hermes_profile, workspace_path, soul_md, agent_description) \
         VALUES (?::uuid, ?::text, ?::text, ?::text, ?::text) \
         ON CONFLICT (user_id) DO UPDATE SET deleted_at = NULL, \
                                             soul_md = EXCLUDED.soul_md"
            .to_string(),
        vec![
            Value::from(robot_id()),
            Value::from("t5-bot"),
            Value::from("/tmp/t5-bot-workspace/"),
            Value::from("# T5 fixture\n"),
            Value::from("T5 夹具机器人"),
        ],
    )
    .await?;
    Ok(())
}

/// 写一条 `sys_user` 的变更日志（before/after 只有 `remark` 不同，保证 diff 非空）。
async fn record_once(db: &Db, ctx: &ChangeCtx<'_>) -> DbResult<()> {
    let before = serde_json::json!({ "username": "t5_log_subject", "remark": "旧备注" });
    let after = serde_json::json!({ "username": "t5_log_subject", "remark": "新备注" });
    db.transaction(|tx| {
        let before = before.clone();
        let after = after.clone();
        let ctx = ctx.clone();
        async move {
            changelog::record(
                &tx,
                &ctx,
                ChangeTarget::of("sys_user", TARGET_ID, Action::Update, "修改用户"),
                DiffInput::update(&before, &after),
            )
            .await
            .map_err(|error| {
                DbError::InvalidValue(format!("record 失败 [{}]：{}", error.code, error.msg))
            })?;
            Ok(())
        }
    })
    .await
}

/// 读回刚写的日志行的 `operator_type`。
async fn read_operator_type(db: &Db) -> DbResult<String> {
    let rows = db
        .find_with_params(
            "SELECT operator_type FROM sys_change_log \
             WHERE resource = 'sys_user' AND resource_id = ?::text \
             ORDER BY created_at DESC LIMIT 1",
            vec![Value::from(TARGET_ID)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(row.get_typed::<String>("operator_type")?),
        None => Err(DbError::InvalidValue("没有找到刚写的日志行".into())),
    }
}

// ==================== 用例 ====================

/// 迁移结构（卡面 2.4）：列 + 类型 + NOT NULL + 默认值 + CHECK + 部分索引。
#[tokio::test]
async fn migration_shape() {
    let Some(db) = connect().await else { return };
    let _guard = SERIAL.lock().await; // 夹具串行（见 SERIAL 的说明）

    let rows = db
        .find_with_params(
            "SELECT data_type, character_maximum_length, is_nullable, column_default \
               FROM information_schema.columns \
              WHERE table_name = 'sys_change_log' AND column_name = 'operator_type'",
            vec![],
        )
        .await
        .expect("查 information_schema");

    let row = rows
        .first()
        .expect("V0111 未应用：sys_change_log.operator_type 不存在");
    let data_type: String = row.get_typed("data_type").expect("data_type");
    let max_len: Option<i32> = row.get_typed("character_maximum_length").ok().flatten();
    let nullable: String = row.get_typed("is_nullable").expect("is_nullable");
    let default: Option<String> = row.get_typed("column_default").ok().flatten();

    println!(
        "[结构] operator_type: {data_type}({max_len:?}) NOT NULL={} default={default:?}",
        nullable == "NO"
    );
    assert_eq!(data_type, "character varying");
    assert_eq!(max_len, Some(16));
    assert_eq!(nullable, "NO", "operator_type 必须 NOT NULL");
    assert!(
        default.unwrap_or_default().contains("HUMAN"),
        "默认值必须是 HUMAN"
    );

    // CHECK 约束
    let check: i64 = db
        .scalar_with_params(
            "SELECT COUNT(*) FROM pg_constraint \
              WHERE conname = 'ck_sys_change_log_operator_type' \
                AND conrelid = 'sys_change_log'::regclass",
            vec![],
        )
        .await
        .expect("查约束");
    println!("[结构] CHECK 约束条数：{check}");
    assert_eq!(check, 1, "缺少 ck_sys_change_log_operator_type");

    // 部分索引（父表上，分区的同构索引由 PG 自动建）
    let index: i64 = db
        .scalar_with_params(
            "SELECT COUNT(*) FROM pg_indexes \
              WHERE tablename = 'sys_change_log' AND indexname = 'idx_sys_change_log_ai_operator'",
            vec![],
        )
        .await
        .expect("查索引");
    println!("[结构] 部分索引 idx_sys_change_log_ai_operator：{index}");
    assert_eq!(index, 1, "缺少 idx_sys_change_log_ai_operator");
}

/// 卡面验收 8：AI 用户做一次修改 → 日志行 `operator_type='ROBOT'`。
#[tokio::test]
async fn record_writes_robot_operator_type() {
    let Some(db) = connect().await else { return };
    let _guard = SERIAL.lock().await; // 夹具串行（见 SERIAL 的说明）
    cleanup(&db).await;
    seed_operators(&db).await.expect("造操作人");

    let ctx = ChangeCtx::api(Some(robot_id()), Some("t5_bot"));
    record_once(&db, &ctx).await.expect("record（ROBOT）");
    let value = read_operator_type(&db).await.expect("读回 operator_type");
    println!("[changelog] ROBOT 操作人的日志行 operator_type = {value}");
    assert_eq!(value, "ROBOT");

    cleanup(&db).await;
}

/// 卡面验收 9：人类操作 → `operator_type='HUMAN'`。
///
/// 与上一条成对：只证明 ROBOT 是 ROBOT 不够 —— 如果实现是「写死 ROBOT」，
/// 上一条照样绿。
#[tokio::test]
async fn record_writes_human_operator_type() {
    let Some(db) = connect().await else { return };
    let _guard = SERIAL.lock().await; // 夹具串行（见 SERIAL 的说明）
    cleanup(&db).await;
    seed_operators(&db).await.expect("造操作人");

    let ctx = ChangeCtx::api(Some(human_id()), Some("t5_human"));
    record_once(&db, &ctx).await.expect("record（HUMAN）");
    let value = read_operator_type(&db).await.expect("读回 operator_type");
    println!("[changelog] HUMAN 操作人的日志行 operator_type = {value}");
    assert_eq!(value, "HUMAN");

    cleanup(&db).await;
}

/// 解析器语义：`None`（系统任务）/ 不存在的账号 → `HUMAN`；ROBOT → `ROBOT`。
#[tokio::test]
async fn operator_type_of_defaults() {
    let Some(db) = connect().await else { return };
    let _guard = SERIAL.lock().await; // 夹具串行（见 SERIAL 的说明）
    cleanup(&db).await;
    seed_operators(&db).await.expect("造操作人");

    let robot = changelog::model::operator_type_of(&db, Some(robot_id()))
        .await
        .expect("解析 ROBOT");
    let human = changelog::model::operator_type_of(&db, Some(human_id()))
        .await
        .expect("解析 HUMAN");
    let system = changelog::model::operator_type_of(&db, None)
        .await
        .expect("解析 None");
    let ghost = changelog::model::operator_type_of(&db, Some(Uuid::now_v7()))
        .await
        .expect("解析不存在的账号");

    println!(
        "[解析] ROBOT={:?} HUMAN={:?} None={:?} 不存在={:?}",
        robot.code(),
        human.code(),
        system.code(),
        ghost.code()
    );
    assert_eq!(robot, OperatorType::Robot);
    assert_eq!(human, OperatorType::Human);
    assert_eq!(system, OperatorType::Human, "系统任务按 HUMAN 落默认值");
    assert_eq!(
        ghost,
        OperatorType::Human,
        "找不到账号时不报错，回 HUMAN（日志是事后凭证）"
    );

    cleanup(&db).await;
}

/// `with_operator_type` 的逃生口：显式指定时**不查库**，写进去的就是指定的值。
#[tokio::test]
async fn explicit_operator_type_skips_lookup() {
    let Some(db) = connect().await else { return };
    let _guard = SERIAL.lock().await; // 夹具串行（见 SERIAL 的说明）
    cleanup(&db).await;
    seed_operators(&db).await.expect("造操作人");

    // 用 HUMAN 账号，但显式声明成 ROBOT：证明显式值压过查库结果
    let ctx =
        ChangeCtx::api(Some(human_id()), Some("t5_human")).with_operator_type(OperatorType::Robot);
    record_once(&db, &ctx).await.expect("record（显式 ROBOT）");
    let value = read_operator_type(&db).await.expect("读回 operator_type");
    println!("[changelog] 显式指定后 operator_type = {value}（账号本身是 HUMAN）");
    assert_eq!(value, "ROBOT");

    cleanup(&db).await;
}

/// `NewChangeLog` 的唯一构造点必须带 `operator_type`（防「新增列后忘记填」这条静默缺陷）。
///
/// 这是**编译期就成立**的断言（结构体字面量少字段编不过），保留成测试是为了
/// 在 `cargo test` 的输出里留下一条可引用的证据，而不是只靠「编译器没报错」。
#[test]
fn new_change_log_carries_operator_type() {
    let row = NewChangeLog {
        id: Uuid::now_v7(),
        request_id: None,
        trace_source: "API",
        operator_id: Some(robot_id()),
        operator_name: Some("t5_bot"),
        operator_type: OperatorType::Robot.code(),
        operator_dept_id: None,
        client_ip: None,
        module: "system",
        resource: "sys_user",
        resource_id: TARGET_ID,
        action: "UPDATE",
        action_label: "修改用户",
        diff: "{}".to_string(),
        diff_mode: "WHITELIST",
        changed_fields: &[],
        error_code: None,
        duration_ms: None,
        created_at: chrono::Utc::now(),
    };
    assert_eq!(row.operator_type, "ROBOT");
    assert_eq!(OperatorType::Human.code(), "HUMAN");
    assert_eq!(OperatorType::ALL.len(), 2, "取值域只有 HUMAN / ROBOT");
    assert!(OperatorType::Robot.is_robot());
    assert!(!OperatorType::Human.is_robot());
}
