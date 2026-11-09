// ============================================================================
// task_model_ai.rs —— Hermes 同步：令牌 / 派活 / 状态回写 / AI 附件绑定
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 登记 / 覆盖任务级令牌（一任务一令牌：重复派活用 `ON CONFLICT DO UPDATE` 覆盖）。
///
/// # 为什么重派要覆盖而不是新增
///
/// 主键是 `task_id`（见 V0112 的设计说明）。重派时覆盖 hash 并**把 `used_count` 归零**，
/// 于是旧令牌立刻失效 —— 它的明文还躺在上一张 kanban body 里，但它再也换不出任何东西。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn upsert_ai_token(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    token_hash: &str,
    expires_at: chrono::DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        "INSERT INTO biz_task_ai_token (task_id, token_hash, expires_at, used_count, created_at) \
         VALUES (?::uuid, ?, ?::timestamptz, 0, now()) \
         ON CONFLICT (task_id) DO UPDATE \
            SET token_hash = EXCLUDED.token_hash, \
                expires_at = EXCLUDED.expires_at, \
                used_count = 0, \
                created_at = now()"
            .to_string(),
        vec![
            Value::from(&task_id),
            Value::Text(token_hash.to_string()),
            TypeMapper::to_value(&expires_at),
        ],
    )
    .await
}

/// 撤销任务令牌（派活失败 / 任务终结时调用）。
pub async fn delete_ai_token(ex: &impl SqlExecutor, task_id: Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "DELETE FROM biz_task_ai_token WHERE task_id = ?::uuid".to_string(),
        vec![Value::from(&task_id)],
    )
    .await
}

/// 派活成功：写 Hermes 三件套 + `exec_mode='AI'`。
///
/// **只由派活接口在 CLI 成功之后调用**（派活失败绝不允许写这一笔，否则云枢会显示
/// 「已派出」而 Hermes 侧什么都没有 —— 卡面红线）。
pub async fn mark_dispatched(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    board: &str,
    hermes_task_id: &str,
    hermes_status: &str,
    operator: Uuid,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_task \
            SET exec_mode = 'AI', hermes_board = ?, hermes_task_id = ?, hermes_status = ?, \
                hermes_synced_at = now(), updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(board.to_string()),
            Value::Text(hermes_task_id.to_string()),
            Value::Text(hermes_status.to_string()),
            Value::from(&operator),
            Value::from(&task_id),
        ],
    )
    .await
}

// SCOPE-EXEMPT: 轮询目标是**系统级扫描**（所有 AI 任务都必须同步），按 exec_mode /
// hermes_task_id 过滤，没有「当前用户」语义；注入数据范围会让别人的 AI 任务永远停在 DOING。
/// 记录 Hermes 原始状态（**不改云枢状态**），用于去重与排障。
pub async fn touch_hermes(ex: &impl SqlExecutor, task_id: Uuid, hermes_status: &str) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_task SET hermes_status = ?, hermes_synced_at = now() \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![Value::Text(hermes_status.to_string()), Value::from(&task_id)],
    )
    .await
}

/// 同步推进云枢状态（`running → DOING` 等），同时记 Hermes 原始状态。
///
/// # 参数
///
/// - `cloud_status`：要推进到的云枢状态；`None` = 只同步时间戳与 Hermes 状态。
/// - `progress`：同时写进度（`running` 时给 10%），`None` = 不动进度。
pub async fn sync_progress_of_ai(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    hermes_status: &str,
    cloud_status: Option<&str>,
    progress: Option<i16>,
    operator: Uuid,
) -> DbResult<u64> {
    let mut sets = vec![
        "hermes_status = ?".to_string(),
        "hermes_synced_at = now()".to_string(),
        "updated_at = now()".to_string(),
    ];
    let mut params = vec![Value::Text(hermes_status.to_string())];
    if let Some(status) = cloud_status {
        sets.push("status = ?".to_string());
        params.push(Value::Text(status.to_string()));
    }
    if let Some(value) = progress {
        sets.push("progress = ?::smallint".to_string());
        params.push(Value::I16(value));
    }
    sets.push("update_by = ?::uuid".to_string());
    params.push(Value::from(&operator));
    params.push(Value::from(&task_id));
    let sql = format!(
        "UPDATE biz_task SET {} WHERE id = ?::uuid AND deleted_at IS NULL",
        sets.join(", ")
    );
    ex.exec_sql(sql, params).await
}

/// 记录 AI 的活跃信号：`TODO → DOING`（其余状态不动）。
///
/// # 为什么进度回写要顺手推进状态
///
/// `PATCH /ai-progress` 意味着 AI **已经在干活了**。若云枢还显示「待开始」，
/// 看板上看不到「AI 执行中」（设计文档 §三 running 那一行要求 DOING）。
/// 这里只做**单向**推进：已经是 `DOING` / `REVIEW` 的一律不动（不后退）。
///
/// 注意**不改** `hermes_status`：那个列是「最近一次从 Hermes 读到的原始状态」，
/// 让进度回写去改写它，会让轮询的去重判断（`hermes_status` 有没有变）失真。
pub async fn mark_ai_active(ex: &impl SqlExecutor, task_id: Uuid, operator: Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_task \
            SET status = CASE WHEN status = 'TODO' THEN 'DOING' ELSE status END, \
                hermes_synced_at = now(), \
                updated_at = now(), \
                update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(&operator), Value::from(&task_id)],
    )
    .await
}

/// AI 完成回传：写摘要 / 结构化结果，并把状态推进到 **REVIEW**（老大 2A 拍板）。
///
/// # 为什么「即使 `need_review = false` 也走 REVIEW」
///
/// AI 的活必须有人点头。`need_review = FALSE` 的语义是「人类自己干的活不用别人验收」，
/// 它不该变成「AI 可以直接把任务结掉」的旁路。所以这里**不读** `need_review`。
///
/// # 参数
///
/// - `summary`：执行摘要（`latest_summary`），空则把 `ai_summary` 置 NULL；
/// - `result`：`runs[..].metadata` 原样（jsonb）；`None` 时保留原值（不覆盖已有结果）。
pub async fn apply_ai_done(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    summary: Option<&str>,
    result: Option<&serde_json::Value>,
    operator: Uuid,
) -> DbResult<u64> {
    let mut sets = vec![
        "status = 'REVIEW'".to_string(),
        "progress = 100".to_string(),
        "hermes_status = 'done'".to_string(),
        "hermes_synced_at = now()".to_string(),
        "ai_summary = ?".to_string(),
        "updated_at = now()".to_string(),
    ];
    let mut params = vec![match summary {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Null,
    }];
    if let Some(json) = result {
        sets.push("ai_result = ?::jsonb".to_string());
        // JSONB 列必须用 Value::Json(JSON 文本)：绑 Value::Text / Value::from 会报
        // `error serializing parameter`（本仓库 A5 卡实测踩过）
        params.push(Value::Json(json.to_string()));
    }
    sets.push("update_by = ?::uuid".to_string());
    params.push(Value::from(&operator));
    params.push(Value::from(&task_id));
    let sql = format!(
        "UPDATE biz_task SET {} WHERE id = ?::uuid AND deleted_at IS NULL",
        sets.join(", ")
    );
    ex.exec_sql(sql, params).await
}

/// 绑定一个 AI 产出的附件（幂等：`(task_id, file_id)` 唯一，重复绑定不报错）。
///
/// # 参数
///
/// - `uploader_name`：上传人姓名**快照**（AUD-01，
///   [`crate::business::shared::user_name::snapshot_name`] 的返回值）。落库后永不回改；
///   `None` 时该列留空，读路径回落实时 join（仅历史行 / 夹具）。
///
/// # 返回值
///
/// 新绑定的附件 id；已绑定过则返回既有 id。
///
/// # 「复活」分支为什么连 `create_by` 与 `uploader_name` 一起改
///
/// `uk_biz_task_attachment (task_id, file_id)` 是全表唯一，删掉附件后重传同一份字节
/// 会走「复活」（清 `deleted_at`）。复活之后这条附件的上传人是**这次传的人**，
/// 所以 id（`create_by`，行级删除判据）与姓名快照必须同时更新 ——
/// 只改姓名会出现「界面显示 B、B 却删不掉」；只改 id 会出现「B 能删，界面显示 A」。
pub async fn bind_ai_attachment(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    file_id: Uuid,
    ai_profile: Option<&str>,
    operator: Uuid,
    uploader_name: Option<&str>,
) -> DbResult<Uuid> {
    if let Some(existing) = ex
        .query_sql(
            "SELECT id FROM biz_task_attachment WHERE task_id = ?::uuid AND file_id = ?::uuid".to_string(),
            vec![Value::from(&task_id), Value::from(&file_id)],
        )
        .await?
        .first()
    {
        let id: Uuid = existing.get_typed("id")?;
        // T6b：`uk_biz_task_attachment (task_id, file_id)` 是**全表**唯一，
        // 所以「删掉附件后重新上传同一个文件」必须走**复活**这条路
        // （清 `deleted_at`），否则 bind 会返回一条已删行、列表里永远看不到它。
        ex.exec_sql(
            "UPDATE biz_task_attachment \
                SET source = 'AI', ai_profile = ?, create_by = ?::uuid, \
                    uploader_name = COALESCE(?, uploader_name), deleted_at = NULL \
              WHERE id = ?::uuid"
                .to_string(),
            vec![
                match ai_profile {
                    Some(profile) => Value::Text(profile.to_string()),
                    None => Value::Null,
                },
                Value::from(&operator),
                uploader_name.map_or(Value::Null, |name| Value::Text(name.to_string())),
                Value::from(&id),
            ],
        )
        .await?;
        return Ok(id);
    }
    let id = Uuid::now_v7();
    let sort_order: i64 = ex
        .scalar_sql(
            "SELECT COALESCE(max(sort_order), -1) + 1 FROM biz_task_attachment WHERE task_id = ?::uuid".to_string(),
            vec![Value::from(&task_id)],
        )
        .await
        .unwrap_or(0);
    ex.exec_sql(
        "INSERT INTO biz_task_attachment \
             (id, task_id, file_id, sort_order, source, ai_profile, created_at, create_by, uploader_name) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?::int4, 'AI', ?, now(), ?::uuid, ?)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(&task_id),
            Value::from(&file_id),
            Value::I32(sort_order as i32),
            match ai_profile {
                Some(profile) => Value::Text(profile.to_string()),
                None => Value::Null,
            },
            Value::from(&operator),
            uploader_name.map_or(Value::Null, |name| Value::Text(name.to_string())),
        ],
    )
    .await?;
    Ok(id)
}

/// 绑定一个人工上传的附件（幂等）。
///
/// # 参数
///
/// - `uploader_name`：上传人姓名快照（口径同 [`bind_ai_attachment`]；复活分支同样
///   `create_by` 与姓名一起更新，理由见该函数文档）。
///
/// # 返回值
///
/// 新绑定 / 复活的附件 id。
pub async fn bind_human_attachment(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    file_id: Uuid,
    operator: Uuid,
    uploader_name: Option<&str>,
) -> DbResult<Uuid> {
    if let Some(existing) = ex
        .query_sql(
            "SELECT id FROM biz_task_attachment WHERE task_id = ?::uuid AND file_id = ?::uuid".to_string(),
            vec![Value::from(&task_id), Value::from(&file_id)],
        )
        .await?
        .first()
    {
        let id: Uuid = existing.get_typed("id")?;
        // 同 `bind_ai_attachment`：全表唯一 → 重新上传同一个文件走「复活」。
        ex.exec_sql(
            "UPDATE biz_task_attachment \
                SET create_by = ?::uuid, uploader_name = COALESCE(?, uploader_name), \
                    deleted_at = NULL \
              WHERE id = ?::uuid"
                .to_string(),
            vec![
                Value::from(&operator),
                uploader_name.map_or(Value::Null, |name| Value::Text(name.to_string())),
                Value::from(&id),
            ],
        )
        .await?;
        return Ok(id);
    }
    let id = Uuid::now_v7();
    let sort_order: i64 = ex
        .scalar_sql(
            "SELECT COALESCE(max(sort_order), -1) + 1 FROM biz_task_attachment WHERE task_id = ?::uuid".to_string(),
            vec![Value::from(&task_id)],
        )
        .await
        .unwrap_or(0);
    ex.exec_sql(
        "INSERT INTO biz_task_attachment \
             (id, task_id, file_id, sort_order, source, created_at, create_by, uploader_name) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?::int4, 'HUMAN', now(), ?::uuid, ?)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(&task_id),
            Value::from(&file_id),
            Value::I32(sort_order as i32),
            Value::from(&operator),
            uploader_name.map_or(Value::Null, |name| Value::Text(name.to_string())),
        ],
    )
    .await?;
    Ok(id)
}
