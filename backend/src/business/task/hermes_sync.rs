//! 云枢 ↔ Hermes kanban 同步（T5b：b2 派活 / b3 轮询回传 / b4 失败处理）。
//!
//! 依据：`docs/批次C+-Hermes集成设计-T5b.md` §一（派活契约）/ §三（结果回传）/
//! §六（失败清单）/ §七（可观测性）；老大 2026-10-29 拍板 **1A**（AI 产出必须进云枢
//! 存储）+ **2A**（AI 完成 → `REVIEW` 待人点头）。
//!
//! # 本模块只做三件事
//!
//! | 入口 | 干什么 | 谁调 |
//! |------|--------|------|
//! | [`dispatch`] | 建令牌 → 拼 body → `hermes kanban create` → 回写 Hermes 三件套 | `handler::dispatch_ai`（人类 JWT） |
//! | [`poll_once`] | 逐任务 `hermes kanban show --json` → 状态映射 → done 三层回传 | 定时任务 `task_hermes_sync`（30s） |
//! | [`register_builtin`] | 把轮询挂到调度器 | `task::Scheduler::register_builtin` |
//!
//! # 三处「绝不能做」的事（卡面红线，逐条对应）
//!
//! 1. **派活失败不许假装成功**：CLI 非 0 / 超时 / 输出无法解析 → 任务留在 `TODO`、
//!    令牌当场删除、写一条错误评论并通知分派人，接口返回 `502 + 63305`；
//! 2. **轮询失败不改任务状态**：CLI 跑不起来只记日志（`tracing::error`），下一轮重试；
//!    只有「Hermes 明确说 done/failed/blocked」才动状态；
//! 3. **AI 完成必进 REVIEW**：即使 `need_review = false` 也走 `REVIEW`
//!    （见 `model::apply_ai_done` 的注释），并把摘要 / 结构化结果 / 产出文件三层回传。
//!
//! # 状态映射（设计文档 §三，逐行实现）
//!
//! | Hermes | 云枢 |
//! |--------|------|
//! | `ready` / `queued` / `todo` / `triage` / `scheduled` | 保持 `TODO`（前端显示「排队中」） |
//! | `running` | `TODO → DOING`（只前进不后退） |
//! | `done` | 三层回传 → `REVIEW` |
//! | `failed` | 保持 `DOING` + 错误评论 + 通知 |
//! | `blocked` | 保持当前状态 + 「需人工协助」评论 + 通知 |
//! | 任务不见了（CLI `no such task`） | 标 `MISSING` + 警告评论 + 通知 |
//!
//! # 为什么轮询按「Hermes 原始状态」去重
//!
//! `failed` / `blocked` / `MISSING` 每 30 秒都会读到一次，若每次都写评论 + 发通知，
//! 一小时就是 120 条噪音。判据是 `biz_task.hermes_status`：**状态没变就不重复动作**。

use std::path::{Component, Path, PathBuf};

use alun::prelude::chrono::{DateTime, Duration, Utc};
use alun::prelude::*;
use alun::uuid::Uuid;

use crate::business::aiagent::hermes::{Hermes, KanbanCreate, KanbanSnapshot};
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeDraft, NoticeTarget};
use crate::business::storage;
use crate::business::task::model::{self, SyncTargetRow, TaskDetailRow};
use crate::shared::error_code;
use crate::task::Scheduler;
use crate::utils;

/// 派活时写进 `biz_task.hermes_board` 的看板 slug（默认 `dev`）。
///
/// 可用环境变量 `CLOUDPIVOT_TASK_HERMES_BOARD` 覆盖（test / prod 分开看板时用）。
pub const DEFAULT_BOARD: &str = "dev";

/// 单次运行上限（秒）→ 同时决定令牌有效期（`+ 1h` 余量）。
pub const MAX_RUNTIME_SECS: i64 = 3600;

/// 令牌有效期在 `max_runtime` 之外多给的余量（小时）。
///
/// AI 在最后一分钟才上传产出是常态，令牌若与 runtime 同时归零，那份产出就传不上来。
pub const TOKEN_TTL_SLACK_HOURS: i64 = 1;

/// 轮询任务名（同时用作调度名与日志标识）。
pub const TASK_NAME: &str = "task_hermes_sync";

/// 轮询 cron：每 30 秒一次（设计文档 §三）。
pub const SYNC_CRON: &str = "*/30 * * * * *";

/// 单轮最多同步多少个任务（避免一次轮询拖过 30 秒窗口）。
pub const SYNC_BATCH: i64 = 20;

/// 云枢存储里「AI 产出」的分类（`V0107` 已种策略：100 MB、白名单更严、禁直传）。
pub const AI_UPLOAD_CATEGORY: &str = "task_ai";

/// 派活时 body 里的上传地址前缀（前端/运维可通过 `CLOUDPIVOT_PUBLIC_BASE_URL` 覆盖）。
pub const DEFAULT_BASE_URL: &str = "http://172.18.10.144";

/// 当前看板 slug。
pub fn board() -> String {
    std::env::var("CLOUDPIVOT_TASK_HERMES_BOARD")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_BOARD.to_string())
}

/// 云枢对外地址（写进 body 的 curl 示例）。
pub fn base_url() -> String {
    std::env::var("CLOUDPIVOT_PUBLIC_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

/// 优先级 → Hermes `--priority` 数值（P0→10 / P1→8 / P2→5 / P3→2）。
///
/// Hermes 的 priority 是「越大越优先」的 tiebreaker，P2（默认优先级）落 5 是
/// 设计文档给定的映射；未知取值按 P2 处理（不 panic：脏数据不该让派活 500）。
pub fn priority_weight(priority: &str) -> i32 {
    match priority.trim().to_uppercase().as_str() {
        "P0" => 10,
        "P1" => 8,
        "P3" => 2,
        _ => 5,
    }
}

/// 幂等键：`cailu-task:<云枢 task_id>`。
///
/// 它是「重复派活只产生一个 Hermes 任务」的唯一保证（验收项 4）：Hermes 侧按这个键
/// 去重，重复 create 会把**已存在**的任务 id 返回回来，而不是新建第二个。
pub fn idempotency_key(task_id: Uuid) -> String {
    format!("cailu-task:{task_id}")
}

/// 重派时的幂等键：`cailu-task:<云枢 task_id>:retry`。
///
/// # 为什么重派要换键
///
/// 关联的 Hermes 任务已经被人删掉（`hermes_status = 'MISSING'`）时，旧幂等键在
/// Hermes 侧仍然「占着」那条被删任务的记录（非归档任务才参与去重，但已删任务在
/// 某些版本里仍会被返回 id），继续用同一个键可能拿回一个不存在的任务 id。
/// 加 `:retry` 后缀 = 明确「这是一次重派」，Hermes 侧会建一条新任务。
pub fn idempotency_key_for_retry(task_id: Uuid) -> String {
    format!("cailu-task:{task_id}:retry")
}

/// 按「是否重派」取幂等键。
pub fn idempotency_key_of(task_id: Uuid, retry: bool) -> String {
    if retry {
        idempotency_key_for_retry(task_id)
    } else {
        idempotency_key(task_id)
    }
}

/// 一次同步要执行的动作（把「Hermes 状态」翻译成「云枢该干什么」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    /// 排队中：保持 `TODO`。
    Queued,
    /// 执行中：`TODO → DOING`。
    Running,
    /// 完成：三层回传 + `REVIEW`。
    Done,
    /// 失败：保持 `DOING` + 错误评论 + 通知。
    Failed,
    /// 阻塞：需人工协助（评论 + 通知）。
    Blocked,
    /// Hermes 侧任务不见了。
    Missing,
    /// 未知状态：只记录，不动云枢。
    Unknown,
}

/// Hermes 原始状态 → 云枢动作。
pub fn map_status(hermes_status: &str) -> SyncAction {
    match hermes_status.trim().to_ascii_lowercase().as_str() {
        "ready" | "queued" | "todo" | "triage" | "scheduled" => SyncAction::Queued,
        "running" | "claimed" => SyncAction::Running,
        "done" | "completed" => SyncAction::Done,
        "failed" | "error" => SyncAction::Failed,
        "blocked" | "needs_input" => SyncAction::Blocked,
        "missing" => SyncAction::Missing,
        _ => SyncAction::Unknown,
    }
}

/// 任务标题里可能出现的换行会破坏 Markdown 标题，统一压成单行。
fn single_line(raw: &str) -> String {
    raw.split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 派活 body 的入参。
pub struct DispatchBody<'a> {
    /// 云枢任务 id。
    pub task_id: Uuid,
    /// 任务编号。
    pub task_no: &'a str,
    /// 标题。
    pub title: &'a str,
    /// 描述（Markdown 源文，可空）。
    pub description: Option<&'a str>,
    /// 优先级（`P0`–`P3`）。
    pub priority: &'a str,
    /// 截止日（可空）。
    pub due_date: Option<chrono::NaiveDate>,
    /// 预估工时（可空）。
    pub estimate_hours: Option<f64>,
    /// 任务级令牌明文（**只写进这份 body，永不入库**）。
    pub token: &'a str,
    /// 云枢对外地址（curl 示例用）。
    pub base_url: &'a str,
    /// 执行者 profile（写进 body 便于 AI 自述）。
    pub profile: &'a str,
}

/// 渲染派活 body（设计文档 §一 的模板，含交付要求与令牌）。
///
/// # 为什么模板里必须有「上传命令 + 令牌」
///
/// 老大拍板 1A：AI 的产出**必须进云枢存储**，而不是只在 summary 里写个路径。
/// 唯一能让 AI 自己做到这件事的办法，就是把带令牌的 `curl` 命令直接写给它 ——
/// 令牌是任务级的，AI 拿它也只能写这一个任务（见 `middleware::task_token`）。
pub fn render_body(input: &DispatchBody<'_>) -> String {
    let description = input
        .description
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("（云枢任务未填写描述，请按标题与交付要求执行）");
    let due = input
        .due_date
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "未设置".to_string());
    let estimate = input
        .estimate_hours
        .map(|hours| format!("{hours} 小时"))
        .unwrap_or_else(|| "未设置".to_string());
    format!(
        r#"# {title}

> 云枢任务：{task_no}（{task_id}） · 执行者：{profile}

## 任务描述

{description}

## 优先级 / 截止

- 优先级：{priority}
- 截止时间：{due}
- 预估工时：{estimate}

## 交付要求（云枢任务管理 · 必须遵守）

完成后请提供：

1. **执行摘要**：做了什么、怎么验证的（≤2000 字）→ 会作为任务评论展示。
   写成 kanban 任务的 `complete` summary（或 run metadata 的 `summary`）即可，云枢会原样
   抄成 `AI_REPORT` 评论。
2. **产出文件必须上传到云枢**（不要只给路径）：每条命令传一个文件，
   `{base}/api/business/task/{task_id}/attachment`（POST，multipart，字段名 `file`），
   请求头 `Authorization: Bearer <任务令牌>`，表单字段名 `file`。
   任务令牌：`{token}`
   （仅能上传到**本任务**，有效期 24 小时；访问别的任务会 403。）
3. **代码改动**：如有，附 commit hash 与分支。
4. **结构化产出**（强烈建议）：在 run metadata 里给 `new_files`（绝对路径数组）、
   `commit`、`endpoints` 等字段，云枢会把它存成 `ai_result` 供前端渲染「交付清单」；
   `new_files` 里落在本机 workspace 下的文件，云枢会自己读盘再上传（兜底路径）。

示例（把 `<文件>` 换成你的产出物绝对路径）：

```bash
curl -sS -X POST "{base}/api/business/task/{task_id}/attachment" \
     -H "Authorization: Bearer {token}" \
     -F "file=@<文件>"
curl -sS -X POST "{base}/api/business/task/{task_id}/comment" \
     -H "Authorization: Bearer {token}" \
     -H "Content-Type: application/json" \
     -d '{{"content":"执行摘要：……"}}'
```

## 约束

- 不要修改本任务之外的任何云枢数据（令牌只对这一个任务有效，越权会被 403）；
- 不要删除文件；产出物一律通过上述接口上传；
- 任务完成后**不要**自己把云枢任务标成已完成 —— 云枢会把它推进到「待验收」由人点头。
"#,
        title = single_line(input.title),
        task_no = input.task_no,
        task_id = input.task_id,
        profile = input.profile,
        description = description,
        priority = input.priority.trim().to_uppercase(),
        due = due,
        estimate = estimate,
        base = input.base_url.trim_end_matches('/'),
        token = input.token,
    )
}

/// 一次派活的结果（handler 用它回填响应）。
#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    /// Hermes 任务 id。
    pub hermes_task_id: String,
    /// 看板 slug。
    pub board: String,
    /// Hermes 侧回报的状态（正常是 `ready`）。
    pub hermes_status: String,
    /// 令牌过期时间（回填给前端展示「AI 的凭据到什么时候有效」）。
    pub expires_at: DateTime<Utc>,
}

/// 派活：建令牌 → 拼 body → create → 成功才回写。
///
/// # 参数
///
/// - `task`：任务详情行（调用方已做完权限校验）；
/// - `profile`：AI 员工的 Hermes profile（调用方已校验负责人是 ROBOT 且有 `sys_ai_agent`）；
/// - `workspace`：AI 的 workspace 绝对路径（`dir:<path>`，让 AI 的产出落在可预期目录下）；
/// - `operator` / `operator_name`：派活人；
/// - `reason`：可选的派活说明（写进评论）。
///
/// # 返回值
///
/// 成功返回 [`DispatchOutcome`]；失败返回 `502 + 63305`（**任务仍在 `TODO`**）。
///
/// # 失败时的清理
///
/// 令牌行当场删除：明文已经写进 body 但 create 没成功，留一个能用的令牌等于留一个
/// 「不知道谁能用、也不知道在谁手里」的凭据。
pub async fn dispatch(
    task: &TaskDetailRow,
    profile: &str,
    workspace: Option<&str>,
    operator: Uuid,
    operator_name: &str,
    reason: Option<&str>,
    retry: bool,
) -> std::result::Result<DispatchOutcome, ApiError> {
    let task_id = task.card.id;
    let board = board();
    let (token, token_hash) = crate::middleware::task_token::new_token();
    let expires_at =
        Utc::now() + Duration::seconds(MAX_RUNTIME_SECS) + Duration::hours(TOKEN_TTL_SLACK_HOURS);

    // ① 先登记令牌（明文进 body、hash 进库）：create 失败会删掉它
    model::upsert_ai_token(db(), task_id, &token_hash, expires_at)
        .await
        .map_err(|error| utils::db_error(error, "登记任务令牌"))?;

    // ②③④ 全部收进同一个 `Result`：**任何一步失败都走同一条失败收尾**
    // （删令牌 + 留痕评论 + 通知分派人 + 任务状态一字不改）。
    // 早前版本把 ② 的 `?` 直接 return 出去，于是「profile 缺模型凭据」这类前置失败
    // 既不写评论也不发通知，库里还残留一条令牌 —— 生产故障注入实测发现。
    let hermes = Hermes::from_env();
    let outcome = async {
        // ② 派活前的前置：profile 必须有**模型凭据**（config.yaml）。
        //    Hermes 的 `profile create` 只复制 SOUL.md，worker 的 HERMES_HOME 指向 profile 目录
        //    —— 没有 config.yaml 时 worker 一启动就退出（144 实测），派活会变成「假成功」。
        //    这里补一次复制；失败直接挡住派活（比让 AI 静默跑不起来强）。
        hermes
            .ensure_profile_config(profile)
            .await
            .map_err(|error| {
                tracing::error!(profile, error = %error.msg, "AI profile 缺模型凭据，拒绝派活");
                ApiError::new(
                    500,
                    error.code,
                    format!("AI profile 缺少模型凭据，派活会变成假成功：{}", error.msg),
                )
            })?;

        // ③ 拼 body（含交付要求与令牌明文）
        let body = render_body(&DispatchBody {
            task_id,
            task_no: &task.card.task_no,
            title: &task.card.title,
            description: task.description.as_deref(),
            priority: &task.card.priority,
            due_date: task.card.due_date,
            estimate_hours: task.estimate_hours,
            token: &token,
            base_url: &base_url(),
            profile,
        });

        // ④ 调 Hermes（唯一允许起进程的地方在 aiagent::hermes）
        let created_by = format!("cailu:{operator_name}");
        let key = idempotency_key_of(task_id, retry);
        hermes
            .kanban_create(KanbanCreate {
                board: &board,
                title: &task.card.title,
                assignee: profile,
                body: &body,
                priority: priority_weight(&task.card.priority),
                max_runtime_secs: MAX_RUNTIME_SECS,
                idempotency_key: &key,
                created_by: Some(&created_by),
                workspace,
            })
            .await
    }
    .await;

    match outcome {
        Ok(hermes_task_id) => {
            // ⑤ 只有 create 成功才回写三件套（红线：失败不许假装成功）
            db().transaction(|tx| {
                let board = board.clone();
                let hermes_task_id = hermes_task_id.clone();
                async move {
                    model::mark_dispatched(&tx, task_id, &board, &hermes_task_id, "ready", operator)
                        .await?;
                    let mut content = format!(
                        "已派发给 AI 员工（profile：{profile}），Hermes 任务 {hermes_task_id}（看板 {board}）"
                    );
                    if let Some(reason) = reason.map(str::trim).filter(|text| !text.is_empty()) {
                        content.push_str(&format!("；说明：{reason}"));
                    }
                    model::insert_comment(
                        &tx,
                        task_id,
                        operator,
                        &content,
                        model::COMMENT_TYPE_PLAIN,
                        operator,
                    )
                    .await?;
                    Ok(())
                }
            })
            .await
            .map_err(|error| utils::db_error(error, "回写派活结果"))?;
            tracing::info!(
                task_id = %task_id,
                hermes_task_id = %hermes_task_id,
                board = %board,
                profile,
                "派活成功"
            );
            Ok(DispatchOutcome {
                hermes_task_id,
                board,
                hermes_status: "ready".to_string(),
                expires_at,
            })
        }
        Err(error) => {
            // ⑥ 失败：删令牌 + 留痕评论 + 通知分派人（任务状态一字不改）
            let failure = error.msg.clone();
            let failed = db()
                .transaction(|tx| {
                    let failure = failure.clone();
                    async move {
                        model::delete_ai_token(&tx, task_id).await?;
                        let content = format!(
                            "派活到 Hermes 失败（任务仍留在当前状态，可重新派活）：{failure}"
                        );
                        model::insert_comment(
                            &tx,
                            task_id,
                            operator,
                            &content,
                            model::COMMENT_TYPE_PLAIN,
                            operator,
                        )
                        .await?;
                        Ok(())
                    }
                })
                .await;
            if let Err(rollback_error) = failed {
                tracing::error!(
                    task_id = %task_id,
                    error = %rollback_error,
                    "派活失败后的清理/留痕也失败（令牌可能残留，需人工确认）"
                );
            }
            tracing::error!(task_id = %task_id, error = %error.msg, "派活失败");
            let mut recipients = vec![task.assigner_id.unwrap_or(task.creator_id)];
            if task.creator_id != recipients[0] {
                recipients.push(task.creator_id);
            }
            notify(
                task_id,
                recipients,
                "派活失败（AI 未接单）",
                &format!(
                    "任务「{}」（{}）派给 AI 失败：{}。任务仍在原状态，请检查 Hermes 后重试。",
                    task.card.title, task.card.task_no, error.msg
                ),
                operator,
                operator_name,
                true,
            )
            .await;
            Err(ApiError::new(
                502,
                error_code::TASK_DISPATCH_FAILED,
                format!("派活失败：{}", error.msg),
            ))
        }
    }
}

/// 一轮轮询的统计（`tracing` 与验收脚本都用它）。
#[derive(Debug, Default, Clone)]
pub struct PollReport {
    /// 本轮扫描到的任务数。
    pub scanned: usize,
    /// 状态有变化并已落库的任务数。
    pub updated: usize,
    /// 完成回传（→ REVIEW）的数量。
    pub completed: usize,
    /// Hermes 侧任务丢失的数量。
    pub missing: usize,
    /// CLI 调用失败的数量（这些任务**状态未改**）。
    pub failed_calls: usize,
}

impl PollReport {
    /// 一行摘要（日志用）。
    pub fn summary(&self) -> String {
        format!(
            "扫描 {} 个 AI 任务；状态更新 {}；完成回传 {}；任务丢失 {}；CLI 失败 {}（状态未改）",
            self.scanned, self.updated, self.completed, self.missing, self.failed_calls
        )
    }
}

/// 轮询一轮：对所有「已派活且未终态」的 AI 任务拉一次 Hermes 状态。
///
/// # 失败口径
///
/// 单个任务的 CLI 失败只让 `failed_calls` 加一，**不动该任务的状态**（下一轮重试）；
/// 查询目标列表失败才把整轮的 `Err` 抛给调用方（那一轮什么也没做）。
pub async fn poll_once() -> std::result::Result<PollReport, ApiError> {
    let targets = model::sync_targets(SYNC_BATCH)
        .await
        .map_err(|error| utils::db_error(error, "查询待同步的 AI 任务"))?;
    let mut report = PollReport {
        scanned: targets.len(),
        ..PollReport::default()
    };
    let hermes = Hermes::from_env();
    for target in &targets {
        let board = if target.board.is_empty() {
            board()
        } else {
            target.board.clone()
        };
        match hermes.kanban_show(&board, &target.hermes_task_id).await {
            Ok(None) => {
                if handle_missing(target).await {
                    report.missing += 1;
                    report.updated += 1;
                }
            }
            Ok(Some(snapshot)) => match sync_one(target, &snapshot).await {
                Ok(true) => {
                    report.updated += 1;
                    if map_status(&snapshot.status) == SyncAction::Done {
                        report.completed += 1;
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::error!(
                        task_id = %target.id,
                        hermes_task_id = %target.hermes_task_id,
                        error = %error.msg,
                        "AI 任务状态同步失败（状态未改，下一轮重试）"
                    );
                    report.failed_calls += 1;
                }
            },
            Err(error) => {
                tracing::error!(
                    task_id = %target.id,
                    hermes_task_id = %target.hermes_task_id,
                    error = %error.msg,
                    "读取 Hermes 任务失败（状态未改，下一轮重试）"
                );
                report.failed_calls += 1;
            }
        }
    }
    tracing::info!(report = %report.summary(), "AI 任务轮询完成");
    Ok(report)
}

/// 单个任务的状态同步。返回 `true` 表示「有落库动作」。
async fn sync_one(
    target: &SyncTargetRow,
    snapshot: &KanbanSnapshot,
) -> std::result::Result<bool, ApiError> {
    let action = map_status(&snapshot.status);
    // 去重：同一个 Hermes 状态只处理一次（评论 / 通知 / 附件上传都是副作用，重放会翻倍）。
    // **`done` 不例外**：REVIEW 不是终态，任务下一轮仍在扫描集合里，
    // 没有这条判定就会每 30 秒重放一遍三层回传（生产实测：90 秒写出 3 组重复评论）。
    let unchanged = status_unchanged(target.hermes_status.as_deref(), &snapshot.status);
    match action {
        SyncAction::Queued => {
            if unchanged {
                return Ok(false);
            }
            model::sync_progress_of_ai(
                db(),
                target.id,
                &snapshot.status,
                None,
                None,
                target.owner_id,
            )
            .await
            .map_err(|error| utils::db_error(error, "记录 Hermes 排队状态"))?;
            Ok(true)
        }
        SyncAction::Running => {
            // 只前进不后退：REVIEW 中的任务被返工时可能又 running，那也不该打回 TODO
            let cloud_status = if target.status == "TODO" {
                Some("DOING")
            } else {
                None
            };
            let progress = if target.status == "TODO" {
                Some(10_i16)
            } else {
                None
            };
            model::sync_progress_of_ai(
                db(),
                target.id,
                &snapshot.status,
                cloud_status,
                progress,
                target.owner_id,
            )
            .await
            .map_err(|error| utils::db_error(error, "记录 AI 执行中"))?;
            tracing::info!(
                task_id = %target.id,
                hermes_status = %snapshot.status,
                from = %target.status,
                "AI 任务同步：执行中"
            );
            Ok(!unchanged || cloud_status.is_some())
        }
        SyncAction::Done => {
            if unchanged {
                tracing::debug!(
                    task_id = %target.id,
                    hermes_status = %snapshot.status,
                    "AI 任务已完成且已回传过（幂等跳过）"
                );
                return Ok(false);
            }
            relay_done(target, snapshot).await.map(|_| true)
        }
        SyncAction::Failed => {
            if unchanged {
                return Ok(false);
            }
            handle_failed(target, snapshot).await?;
            Ok(true)
        }
        SyncAction::Blocked => {
            if unchanged {
                return Ok(false);
            }
            handle_blocked(target, snapshot).await?;
            Ok(true)
        }
        SyncAction::Missing => Ok(handle_missing(target).await),
        SyncAction::Unknown => {
            tracing::warn!(
                task_id = %target.id,
                hermes_status = %snapshot.status,
                "Hermes 返回未知状态，云枢不改任务状态"
            );
            Ok(false)
        }
    }
}

/// 上一轮记录的 Hermes 状态是否与本次相同。
///
/// 幂等判定的唯一实现：同一个状态只允许产生一次副作用（评论 / 通知 / 上传附件）。
/// 大小写不敏感 —— Hermes 各版本对状态的写法不完全统一（`done` / `DONE`）。
fn status_unchanged(previous: Option<&str>, current: &str) -> bool {
    previous.is_some_and(|previous| previous.eq_ignore_ascii_case(current))
}

/// `done` 的三层回传 + 状态推进到 `REVIEW`。
///
/// # 三层（设计文档 §三 done 处理）
///
/// ① `latest_summary` → `AI_REPORT` 评论 + `biz_task.ai_summary`；
/// ② `metadata` → `biz_task.ai_result`（jsonb）；
/// ③ 产出文件：
///    - 路径 A（契约）：AI 已用令牌上传 → `biz_task_attachment` 里已有 `source='AI'` 的行；
///    - 路径 B（兜底）：`metadata.new_files` 是本机 workspace 下的真实文件 → 云枢读盘上传
///      （`category='task_ai'`）并绑定附件；
///    - 路径 C（找不到）：写一条警告评论，**不阻塞**流转。
async fn relay_done(
    target: &SyncTargetRow,
    snapshot: &KanbanSnapshot,
) -> std::result::Result<(), ApiError> {
    let report = snapshot.report_text();
    let metadata = snapshot.metadata.clone();

    // ---------- ③ 产出文件（先做，失败也不阻塞流转） ----------
    let mut uploaded: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let existing_ai = model::count_ai_attachments(target.id)
        .await
        .map_err(|error| utils::db_error(error, "统计 AI 附件"))?;
    let declared = snapshot.new_files();
    if declared.is_empty() && existing_ai == 0 {
        warnings.push(
            "AI 未提供产出文件（既没有用令牌上传附件，metadata.new_files 也是空的），请验收人确认交付内容"
                .to_string(),
        );
    }
    let profile = ai_profile_of(target.owner_id).await;
    let workspace_root = workspace_root();
    for raw in &declared {
        match resolve_workspace_file(&workspace_root, raw).await {
            Ok(Some((path, name, bytes, mime))) => {
                match storage::handler::store_server_upload(
                    AI_UPLOAD_CATEGORY,
                    &name,
                    &mime,
                    &bytes,
                    Some(target.owner_id),
                )
                .await
                {
                    Ok(record) => {
                        if let Err(error) = model::bind_ai_attachment(
                            db(),
                            target.id,
                            record.id,
                            profile.as_deref(),
                            target.owner_id,
                        )
                        .await
                        {
                            warnings
                                .push(format!("AI 产出「{name}」已上传但绑定附件失败：{error}"));
                        } else {
                            uploaded.push(format!("{name}（{}）", path.display()));
                        }
                    }
                    Err(error) => warnings.push(format!(
                        "AI 产出「{name}」上传云枢失败：{}（文件：{}）",
                        error.msg,
                        path.display()
                    )),
                }
            }
            Ok(None) => warnings.push(format!(
                "AI 声明的产出文件未找到（不在 workspace 下或不是普通文件）：{raw}"
            )),
            Err(error) => warnings.push(format!("读取 AI 产出文件失败（{raw}）：{error}")),
        }
    }

    // ---------- ①② + 状态推进（一个事务） ----------
    let summary_text = report.clone().unwrap_or_default();
    db().transaction(|tx| {
        let summary_text = summary_text.clone();
        let metadata = metadata.clone();
        let warnings = warnings.clone();
        let uploaded = uploaded.clone();
        let profile = profile.clone();
        async move {
            model::apply_ai_done(
                &tx,
                target.id,
                Some(summary_text.as_str()).filter(|text| !text.is_empty()),
                metadata.as_ref(),
                target.owner_id,
            )
            .await?;
            if !summary_text.is_empty() {
                model::insert_comment(
                    &tx,
                    target.id,
                    target.owner_id,
                    &summary_text,
                    model::COMMENT_TYPE_AI_REPORT,
                    target.owner_id,
                )
                .await?;
            }
            let profile_label = profile.as_deref().unwrap_or("未登记 profile");
            let mut tail = format!(
                "AI 已完成执行（profile：{profile_label}，Hermes 状态 done），任务已进入「待验收」"
            );
            if !uploaded.is_empty() {
                tail.push_str(&format!(
                    "；本次回传 {} 个产出文件：{}",
                    uploaded.len(),
                    uploaded.join("、")
                ));
            }
            for warning in &warnings {
                tail.push_str(&format!("\n- ⚠️ {warning}"));
            }
            model::insert_comment(
                &tx,
                target.id,
                target.owner_id,
                &tail,
                model::COMMENT_TYPE_AI_REPORT,
                target.owner_id,
            )
            .await?;
            Ok(())
        }
    })
    .await
    .map_err(|error| utils::db_error(error, "AI 完成回传落库"))?;

    // ---------- ④ 通知验收人（创建人 + 分派人） ----------
    let mut recipients = vec![target.creator_id];
    if let Some(assigner) = target.assigner_id
        && assigner != target.creator_id
    {
        recipients.push(assigner);
    }
    notify(
        target.id,
        recipients,
        "AI 已完成，请验收",
        &format!(
            "任务「{}」（{}）已由 AI 执行完成，产出与摘要已回传，请验收（通过 → 完成；打回 → 进行中）。",
            target.title, target.task_no
        ),
        target.owner_id,
        "AI 员工",
        true,
    )
    .await;
    tracing::info!(
        task_id = %target.id,
        hermes_task_id = %target.hermes_task_id,
        files = uploaded.len() as i64,
        warnings = warnings.len() as i64,
        "AI 任务完成回传 → REVIEW"
    );
    Ok(())
}

/// `failed`：保持 `DOING`，写错误评论 + 通知分派人。
async fn handle_failed(
    target: &SyncTargetRow,
    snapshot: &KanbanSnapshot,
) -> std::result::Result<(), ApiError> {
    let reason = snapshot
        .run_error
        .clone()
        .or_else(|| snapshot.report_text())
        .unwrap_or_else(|| "Hermes 未给出错误信息".to_string());
    // 保持 DOING（若还在 TODO，至少推进到 DOING，让看板能红起来）
    model::sync_progress_of_ai(db(), target.id, "failed", None, None, target.owner_id)
        .await
        .map_err(|error| utils::db_error(error, "记录 AI 失败状态"))?;
    let content =
        format!("AI 执行失败：{reason}\n\n任务保持「进行中」，请分派人决定重派或转人工。");
    model::insert_comment(
        db(),
        target.id,
        target.owner_id,
        &content,
        model::COMMENT_TYPE_PLAIN,
        target.owner_id,
    )
    .await
    .map_err(|error| utils::db_error(error, "写入 AI 失败评论"))?;
    let mut recipients = vec![target.assigner_id.unwrap_or(target.creator_id)];
    if target.creator_id != recipients[0] {
        recipients.push(target.creator_id);
    }
    notify(
        target.id,
        recipients,
        "AI 执行失败",
        &format!(
            "任务「{}」（{}）的 AI 执行失败：{reason}。任务仍在「进行中」，请决定重派或转人工。",
            target.title, target.task_no
        ),
        target.owner_id,
        "AI 员工",
        true,
    )
    .await;
    Ok(())
}

/// `blocked`：写「需人工协助」评论 + 通知分派人（状态不动）。
async fn handle_blocked(
    target: &SyncTargetRow,
    snapshot: &KanbanSnapshot,
) -> std::result::Result<(), ApiError> {
    let reason = snapshot
        .report_text()
        .or_else(|| snapshot.run_error.clone())
        .unwrap_or_else(|| "Hermes 把任务标为 blocked，但未给出原因".to_string());
    model::touch_hermes(db(), target.id, "blocked")
        .await
        .map_err(|error| utils::db_error(error, "记录 AI 阻塞状态"))?;
    let content = format!("AI 需要人工协助：{reason}\n\n请在 Hermes 侧解除阻塞或把任务转回人工。");
    model::insert_comment(
        db(),
        target.id,
        target.owner_id,
        &content,
        model::COMMENT_TYPE_PLAIN,
        target.owner_id,
    )
    .await
    .map_err(|error| utils::db_error(error, "写入 AI 阻塞评论"))?;
    let mut recipients = vec![target.assigner_id.unwrap_or(target.creator_id)];
    if target.creator_id != recipients[0] {
        recipients.push(target.creator_id);
    }
    notify(
        target.id,
        recipients,
        "AI 需要人工协助",
        &format!(
            "任务「{}」（{}）在 Hermes 侧被阻塞：{reason}。",
            target.title, target.task_no
        ),
        target.owner_id,
        "AI 员工",
        true,
    )
    .await;
    Ok(())
}

/// Hermes 侧任务不见了（`no such task`）：标 `MISSING` + 警告评论 + 通知。
///
/// # 返回值
///
/// 是否真的做了动作（状态没变时返回 `false`，避免每 30 秒重复写评论）。
async fn handle_missing(target: &SyncTargetRow) -> bool {
    if target
        .hermes_status
        .as_deref()
        .is_some_and(|previous| previous.eq_ignore_ascii_case("MISSING"))
    {
        return false;
    }
    if let Err(error) = model::touch_hermes(db(), target.id, "MISSING").await {
        tracing::error!(task_id = %target.id, error = %error, "记录 Hermes 任务丢失失败");
        return false;
    }
    let content = format!(
        "关联的 Hermes 任务已丢失（{} / {}）：可能被人删除或看板被重建。任务状态保持不变，请人工确认后重派。",
        target.board, target.hermes_task_id
    );
    if let Err(error) = model::insert_comment(
        db(),
        target.id,
        target.owner_id,
        &content,
        model::COMMENT_TYPE_PLAIN,
        target.owner_id,
    )
    .await
    {
        tracing::error!(task_id = %target.id, error = %error, "写入任务丢失评论失败");
    }
    let mut recipients = vec![target.assigner_id.unwrap_or(target.creator_id)];
    if target.creator_id != recipients[0] {
        recipients.push(target.creator_id);
    }
    notify(
        target.id,
        recipients,
        "AI 任务丢失",
        &format!(
            "任务「{}」（{}）关联的 Hermes 任务 {} 已消失（看板 {}）。",
            target.title, target.task_no, target.hermes_task_id, target.board
        ),
        target.owner_id,
        "云枢",
        true,
    )
    .await;
    true
}

/// 发一条站内信（失败只记 WARN —— 通知不该让同步失败）。
async fn notify(
    task_id: Uuid,
    recipients: Vec<Uuid>,
    title: &str,
    content: &str,
    operator: Uuid,
    operator_name: &str,
    force: bool,
) {
    let mut targets: Vec<Uuid> = recipients.into_iter().collect();
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        return;
    }
    let mut draft = NoticeDraft::text(
        NoticeType::Todo,
        NoticeLevel::Info,
        title.to_string(),
        content.to_string(),
        NoticeTarget::User(targets),
    );
    draft.link_url = Some(format!("/task/{task_id}"));
    draft.biz_type = Some("task".to_string());
    draft.biz_id = Some(task_id.to_string());
    draft.sender_id = Some(operator);
    draft.sender_name = Some(operator_name.to_string());
    draft.force = force;
    if let Err(error) = notice::dispatch(draft).await {
        tracing::warn!(task_id = %task_id, error = %error, "AI 任务通知发送失败（不影响业务）");
    }
}

/// 取某个用户登记的执行者 profile（`sys_ai_agent.hermes_profile`）。
async fn ai_profile_of(user_id: Uuid) -> Option<String> {
    match crate::business::aiagent::model::find_by_user(&user_id).await {
        Ok(Some(row)) => Some(row.hermes_profile),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%user_id, error = %error, "查询 AI 员工 profile 失败（附件按未登记处理）");
            None
        }
    }
}

/// AI workspace 根目录（`CLOUDPIVOT_AIAGENT_WORKSPACE_ROOT`，默认 `/root/.hermes/workspace`）。
fn workspace_root() -> PathBuf {
    std::env::var("CLOUDPIVOT_AIAGENT_WORKSPACE_ROOT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root/.hermes/workspace"))
}

/// 把 `metadata.new_files` 里的一条声明解析成「可上传的文件」。
///
/// # 安全边界（路径 B 的闸）
///
/// `new_files` 是 **AI 自己写的**内容，属于不可信输入。这里做三道闸：
///
/// 1. 必须是**绝对路径**（相对路径一律拒，避免被解析到进程 CWD）；
/// 2. 路径里不允许 `..` 之类的父级分量，且 canonicalize 后必须仍在 workspace 根下
///    （符号链接同理被 canonicalize 拆穿）；
/// 3. 必须是普通文件且不超过 [`MAX_RELAY_FILE_BYTES`]。
///
/// # 返回值
///
/// - `Ok(Some((路径, 文件名, 字节, MIME)))`：可以上传；
/// - `Ok(None)`：文件不存在或不在 workspace 下（路径 C：记警告，不阻塞）；
/// - `Err(原因)`：读盘失败。
pub(crate) async fn resolve_workspace_file(
    workspace_root: &Path,
    raw: &str,
) -> std::result::Result<Option<(PathBuf, String, Vec<u8>, String)>, String> {
    let declared = Path::new(raw.trim());
    if !declared.is_absolute() {
        return Ok(None);
    }
    if declared
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Ok(None);
    }
    let Ok(root) = tokio::fs::canonicalize(workspace_root).await else {
        return Ok(None);
    };
    let Ok(real) = tokio::fs::canonicalize(declared).await else {
        // 不存在（AI 写错 / 文件已删）：按路径 C 处理
        return Ok(None);
    };
    if !real.starts_with(&root) {
        return Ok(None);
    }
    let Ok(metadata) = tokio::fs::metadata(&real).await else {
        return Ok(None);
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    if metadata.len() > MAX_RELAY_FILE_BYTES {
        return Err(format!(
            "文件 {} MB 超过回传上限 {} MB",
            metadata.len() / (1024 * 1024),
            MAX_RELAY_FILE_BYTES / (1024 * 1024)
        ));
    }
    let bytes = tokio::fs::read(&real)
        .await
        .map_err(|error| format!("读取失败：{error}"))?;
    let name = real
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "ai-output".to_string());
    let mime = mime_of(&name);
    Ok(Some((real, name, bytes, mime)))
}

/// 回传文件大小上限（与 `task_ai` 策略的 100 MB 对齐，留 1 MB 余量给 multipart 语义差异）。
pub const MAX_RELAY_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// 按扩展名猜 MIME（`task_ai` 策略 `allowed_mimes` 为空 ⇒ 不校验，这里只为了记录准确）。
fn mime_of(file_name: &str) -> String {
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" => "text/markdown",
        "txt" | "log" => "text/plain",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// 把轮询挂到调度器（在 `task::Scheduler::register_builtin` 里调用一次）。
pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        SYNC_CRON,
        "业务任务：AI 任务状态同步（每 30 秒扫一遍已派活的 AI 任务）",
        || {
            tokio::spawn(async move {
                if let Err(error) = poll_once().await {
                    tracing::error!(error = %error.msg, "AI 任务轮询失败（整轮未执行）");
                }
            })
        },
    );
    tracing::info!(
        task = TASK_NAME,
        cron = SYNC_CRON,
        "AI 任务状态同步任务已注册（T5b）"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task_id() -> Uuid {
        Uuid::parse_str("0199f0a0-2222-7000-8000-000000000002").expect("固定 uuid")
    }

    /// 优先级映射逐档断言（卡面契约：P0→10 / P1→8 / P2→5 / P3→2）。
    #[test]
    fn priority_mapping_matches_contract() {
        assert_eq!(priority_weight("P0"), 10);
        assert_eq!(priority_weight("p0"), 10, "大小写不该影响映射");
        assert_eq!(priority_weight("P1"), 8);
        assert_eq!(priority_weight("P2"), 5);
        assert_eq!(priority_weight("P3"), 2);
        assert_eq!(priority_weight(""), 5, "脏值按 P2 处理，不该 panic");
        assert_eq!(priority_weight("X9"), 5);
    }

    /// 幂等键形状：`cailu-task:<uuid>` 且过自家校验。
    #[test]
    fn idempotency_key_is_stable_and_valid() {
        let key = idempotency_key(sample_task_id());
        assert_eq!(key, "cailu-task:0199f0a0-2222-7000-8000-000000000002");
        assert!(crate::business::aiagent::hermes::validate_idempotency_key(&key).is_ok());
        // 同一任务重复派活 → 同一个键（这才是「只产生一个 Hermes 任务」的前提）
        assert_eq!(key, idempotency_key(sample_task_id()));
    }

    /// 状态映射：设计文档 §三 的 6 行逐行断言。
    #[test]
    fn hermes_status_maps_to_expected_actions() {
        for status in ["ready", "queued", "todo", "triage", "scheduled"] {
            assert_eq!(map_status(status), SyncAction::Queued, "{status}");
        }
        assert_eq!(map_status("running"), SyncAction::Running);
        assert_eq!(map_status("RUNNING"), SyncAction::Running, "大小写不敏感");
        assert_eq!(map_status("done"), SyncAction::Done);
        assert_eq!(map_status("failed"), SyncAction::Failed);
        assert_eq!(map_status("blocked"), SyncAction::Blocked);
        assert_eq!(map_status("missing"), SyncAction::Missing);
        assert_eq!(map_status("whatever"), SyncAction::Unknown);
        assert_eq!(map_status(""), SyncAction::Unknown);
    }

    /// body 模板必须含：标题 / 令牌 / 上传地址 / 任务 id / 交付要求四要素。
    #[test]
    fn dispatch_body_contains_token_and_upload_contract() {
        let task_id = sample_task_id();
        let body = render_body(&DispatchBody {
            task_id,
            task_no: "TSK-202610-000042",
            title: "生成月度报告",
            description: Some("按模板产出 PDF"),
            priority: "P1",
            due_date: None,
            estimate_hours: Some(2.5),
            token: "cpai_deadbeef",
            base_url: "http://172.18.10.144",
            profile: "agent-report",
        });
        assert!(body.contains("# 生成月度报告"));
        assert!(body.contains("TSK-202610-000042"));
        assert!(body.contains("按模板产出 PDF"));
        assert!(body.contains("优先级：P1"));
        assert!(body.contains("预估工时：2.5 小时"));
        assert!(body.contains("截止时间：未设置"));
        assert!(body.contains(
            "http://172.18.10.144/api/business/task/0199f0a0-2222-7000-8000-000000000002/attachment"
        ));
        assert!(
            body.contains("cpai_deadbeef"),
            "令牌必须写进 body（1A 契约）"
        );
        // 端到端实测踩过：把 URL 写成 `{base}"/api/…"`（多一个引号）时，curl 会把
        // 引号当成主机名的一部分，AI 侧反复失败直到迭代预算耗尽 —— 而上面那条 substring
        // 断言因为**示例块里的正确 URL** 依然通过。所以这里专门盯「主机名后紧跟引号」。
        assert!(
            !body.contains("http://172.18.10.144\""),
            "主机名后不许紧跟引号：URL 必须是 `{{base}}/api/…`，否则 curl 拿到的地址是坏的\n{body}"
        );
        assert!(body.contains("agent-report"));
        assert!(body.contains("new_files"));
        assert!(body.contains("授权") || body.contains("Authorization"));
    }

    /// 标题里的换行必须被压平（否则 Markdown 标题会被截断）。
    #[test]
    fn dispatch_body_flattens_multiline_title() {
        let body = render_body(&DispatchBody {
            task_id: sample_task_id(),
            task_no: "TSK-202610-000043",
            title: "第一行\n第二行",
            description: None,
            priority: "P2",
            due_date: None,
            estimate_hours: None,
            token: "cpai_x",
            base_url: "http://host",
            profile: "agent-x",
        });
        assert!(
            body.starts_with("# 第一行 第二行"),
            "标题应压成单行：{body}"
        );
        assert!(body.contains("（云枢任务未填写描述"));
    }

    /// 路径 B 的三道闸：相对路径 / 含 `..` / 不存在 一律返回 `Ok(None)`（记警告不阻塞）。
    #[tokio::test]
    async fn relay_path_guards_reject_untrusted_paths() {
        let root = std::env::temp_dir();
        assert!(
            resolve_workspace_file(&root, "relative/path.txt")
                .await
                .expect("不该报错")
                .is_none(),
            "相对路径必须拒绝"
        );
        assert!(
            resolve_workspace_file(&root, "/root/.hermes/workspace/../../etc/passwd")
                .await
                .expect("不该报错")
                .is_none(),
            "含 .. 的路径必须拒绝"
        );
        assert!(
            resolve_workspace_file(&root, "/definitely/not/here.txt")
                .await
                .expect("不该报错")
                .is_none(),
            "不存在的文件按路径 C 处理"
        );
    }

    /// 路径 B 的正面用例：workspace 下的真实文件能读出来，MIME 按扩展名给出。
    #[tokio::test]
    async fn relay_path_accepts_file_inside_workspace() {
        let root = std::env::temp_dir().join(format!("t5b-relay-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.expect("建临时目录");
        let file = root.join("report.md");
        tokio::fs::write(&file, b"# hello").await.expect("写文件");
        let (path, name, bytes, mime) = resolve_workspace_file(&root, &file.to_string_lossy())
            .await
            .expect("不该报错")
            .expect("应当命中");
        assert_eq!(name, "report.md");
        assert_eq!(bytes, b"# hello");
        assert_eq!(mime, "text/markdown");
        assert_eq!(path, file.canonicalize().expect("canonicalize"));
        tokio::fs::remove_dir_all(&root).await.ok();
    }

    /// MIME 兜底：未知扩展名走 octet-stream，不 panic。
    #[test]
    fn done_relay_is_idempotent_across_polling_cycles() {
        // 同一状态：不重复回传（REVIEW 不是终态，任务还在扫描集合里）
        assert!(status_unchanged(Some("done"), "done"));
        assert!(status_unchanged(Some("DONE"), "done"));
        // 首次 done / 返工后再次 done：必须回传
        assert!(!status_unchanged(None, "done"));
        assert!(!status_unchanged(Some("running"), "done"));
        // 其它状态沿用同一判定
        assert!(status_unchanged(Some("blocked"), "blocked"));
        assert!(!status_unchanged(Some("blocked"), "running"));
    }

    #[test]
    fn mime_guess_is_conservative() {
        assert_eq!(mime_of("a.pdf"), "application/pdf");
        assert_eq!(mime_of("a.PNG"), "image/png");
        assert_eq!(mime_of("noext"), "application/octet-stream");
        assert_eq!(mime_of("a.sh"), "application/octet-stream");
    }
}
