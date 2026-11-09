// ============================================================================
// task_attachment_snapshot_tests.rs —— AUD-01：任务附件**上传人姓名快照**（真库用例）
//
// 判据（AGENTS.md 约定 13）：附件上传人是**审计属性** —— 用户改名 / 销号后，
// 回看旧附件必须显示「上传当时是谁」。所以：
//   ① 写入时把 `sys_user` 的现值落进 `biz_task_attachment.uploader_name`（快照）；
//   ② 改名后旧附件仍显示**改名前的名字**（快照不随 sys_user 变）；
//   ③ 没有快照的历史行（V0352 之前 / 夹具）回落实时 join 现值，**不回落 UUID、不留空白**；
//   ④ 「复活」分支（删掉附件后重传同一份字节）把 `create_by` 与姓名**一起**更新 ——
//      否则会出现「界面显示 B、B 却删不掉」。
//
// `TASK_DB_URL` 未设置则整段 SKIP（打印原因，不静默假绿）。
//
// ```bash
// TASK_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//   cargo test --lib business::task::model::attachment_snapshot_tests -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::task::collab;
use crate::business::task::model;

/// 夹具用户（上传人）。
const U_ME: &str = "a0d10000-0000-4000-8000-0000000000a1";
/// 夹具任务。
const TASK_ID: &str = "a0d10000-0000-4000-8000-0000000000b1";
/// 夹具文件（人工上传 / AI 产出 / 历史行）。
const FILE_HUMAN: &str = "a0d10000-0000-4000-8000-0000000000c1";
const FILE_AI: &str = "a0d10000-0000-4000-8000-0000000000c2";
const FILE_LEGACY: &str = "a0d10000-0000-4000-8000-0000000000c3";

/// 真库用例**串行**（共用一个库 + 固定 UUID 夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn uid(text: &str) -> Uuid {
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

/// 拿锁 + 连库 + 重建夹具（幂等：先删自己上一轮造的行）。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    let me = uid(U_ME);

    // 清理（外键顺序：附件 → 文件 → 任务 → 用户）
    for sql in [
        format!("DELETE FROM biz_task_attachment WHERE task_id = '{TASK_ID}'::uuid"),
        format!("DELETE FROM sys_file WHERE id IN ('{FILE_HUMAN}'::uuid, '{FILE_AI}'::uuid, '{FILE_LEGACY}'::uuid)"),
        format!("DELETE FROM biz_task WHERE id = '{TASK_ID}'::uuid"),
        format!("DELETE FROM sys_user WHERE id = '{U_ME}'::uuid"),
    ] {
        db().execute_with_params(&sql, vec![])
            .await
            .unwrap_or_else(|err| panic!("夹具清理失败：{err}\nSQL: {sql}"));
    }

    // 用户（姓名是快照的源头）
    db().execute_with_params(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES (?::uuid, 'aud01_uploader', 'not-a-real-hash', '上传人甲', 1)",
        vec![Value::from(&me)],
    )
    .await
    .expect("夹具用户写入失败");

    // 任务
    db().execute_with_params(
        "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id) \
         VALUES (?::uuid, 'AUD01-TASK-0001', 'AUD-01 附件姓名快照夹具', ?::uuid, ?::uuid)",
        vec![Value::from(&uid(TASK_ID)), Value::from(&me), Value::from(&me)],
    )
    .await
    .expect("夹具任务写入失败");

    // 三个 sys_file（直接插行，不写物理对象：本用例验的是绑定关系与姓名口径）
    for (file_id, hash) in [
        (FILE_HUMAN, "aud01-hash-human"),
        (FILE_AI, "aud01-hash-ai"),
        (FILE_LEGACY, "aud01-hash-legacy"),
    ] {
        db().execute_with_params(
            "INSERT INTO sys_file (id, file_name, original_name, file_key, file_size, file_hash, \
                 mime_type, extension, category, upload_mode, is_shared) \
             VALUES (?::uuid, ?, 'AUD-01 夹具.pdf', ?, 1024, ?, 'application/pdf', 'pdf', \
                 'task', 'SERVER', FALSE)",
            vec![
                Value::from(&uid(file_id)),
                Value::Text(format!("aud01/{file_id}.pdf")),
                Value::Text(format!("aud01/{file_id}.pdf")),
                Value::Text(hash.to_string()),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("夹具文件写入失败（{file_id}）：{err}"));
    }

    Some(guard)
}

/// 卡面验收②⑤（任务附件半边）：写入即快照；改名后旧附件仍显示旧名；
/// 无快照的历史行回落实时 join；「复活」时 id 与姓名一起更新。
#[tokio::test]
async fn db_task_attachment_uploader_name_is_snapshot() {
    let Some(_guard) = setup().await else {
        return;
    };
    let me = uid(U_ME);
    let task = uid(TASK_ID);

    // ① 人工上传：带快照
    let human = model::bind_human_attachment(db(), task, uid(FILE_HUMAN), me, Some("上传人甲"))
        .await
        .expect("绑定人工附件");
    // ② AI 产出：同样带快照（AI 以任务负责人身份落库）
    let ai = model::bind_ai_attachment(db(), task, uid(FILE_AI), Some("ai-worker"), me, Some("上传人甲"))
        .await
        .expect("绑定 AI 附件");
    // ③ 历史行（V0352 之前的行 / 夹具）：姓名列为空，只有 create_by
    db().execute_with_params(
        "INSERT INTO biz_task_attachment (id, task_id, file_id, sort_order, source, created_at, create_by) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, 99, 'HUMAN', now(), ?::uuid)",
        vec![Value::from(&task), Value::from(&uid(FILE_LEGACY)), Value::from(&me)],
    )
    .await
    .expect("插入历史行夹具");

    // ④ 用户改名（审计列的判据就在这里）
    db().execute_with_params(
        "UPDATE sys_user SET real_name = '上传人乙' WHERE id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("改用户姓名失败");

    let rows = collab::list_attachment_rows(task).await.expect("查任务附件列表");
    assert_eq!(rows.len(), 3, "三条附件都应在列表里：{rows:?}");

    let human_row = rows.iter().find(|row| row.id == human).expect("人工附件在列表里");
    assert_eq!(
        human_row.uploader_name, "上传人甲",
        "快照生效：改名后旧附件必须显示上传当时的名字"
    );
    let ai_row = rows.iter().find(|row| row.id == ai).expect("AI 附件在列表里");
    assert_eq!(ai_row.uploader_name, "上传人甲", "AI 附件同样走快照");
    assert_eq!(ai_row.source, "AI");

    let legacy_row = rows
        .iter()
        .find(|row| row.uploader_id == Some(me) && row.id != human && row.id != ai)
        .expect("历史行在列表里");
    assert_eq!(
        legacy_row.uploader_name, "上传人乙",
        "没有快照的历史行回落实时 join 现值（不是 UUID、不是空白）"
    );

    // ⑤「复活」分支：删掉人工附件，再用新名字重传同一份文件
    db().execute_with_params(
        "UPDATE biz_task_attachment SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&human)],
    )
    .await
    .expect("软删人工附件失败");
    let revived = model::bind_human_attachment(db(), task, uid(FILE_HUMAN), me, Some("上传人乙"))
        .await
        .expect("复活人工附件");
    assert_eq!(revived, human, "同一 (task_id, file_id) 复活回原行");

    let rows = collab::list_attachment_rows(task)
        .await
        .expect("查任务附件列表（复活后）");
    let revived_row = rows.iter().find(|row| row.id == revived).expect("复活的行在列表里");
    assert_eq!(
        revived_row.uploader_name, "上传人乙",
        "复活后上传人姓名必须跟着换成这次上传的人（否则 id 与姓名不一致）"
    );
    assert_eq!(revived_row.uploader_id, Some(me));

    // 复原夹具姓名（后续用例 / 手工排查都依赖初始值）
    db().execute_with_params(
        "UPDATE sys_user SET real_name = '上传人甲' WHERE id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("复原用户姓名失败");

    println!("[AUD-01] 任务附件上传人姓名快照（写入 / 改名 / 历史行回落 / 复活）✓");
}
