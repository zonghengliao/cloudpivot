// ============================================================================
// task_model_flow.rs —— 任务 ↔ 流程：来源实例/节点查询与审批人解析
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_model_rows::*;
use crate::business::flow::node_config;

/// 把 SQL 里 `string_agg` 拼好的待办人姓名串拆成数组（保持顺序、去重、丢空段）。
///
/// # 为什么走「文本 + 拆分」而不是 `array_agg`
///
/// 驱动解不了 `text[]` 列（`unsupported column type _text`），所以聚合结果一律
/// `::text` 出来自己拆。分隔符用换行：姓名里不会出现换行（`VARCHAR(50)` 的人名），
/// 而逗号可能出现（「王五, 财务」这类备注式姓名）。
pub(super) fn split_approvers(raw: Option<String>) -> Vec<String> {
    let Some(text) = raw else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for part in text.split('\n') {
        let name = part.trim();
        if !name.is_empty() && !names.iter().any(|kept| kept == name) {
            names.push(name.to_string());
        }
    }
    names
}

/// 从快照里的 `config.task` 取一个「流程走向」枚举字段（P-FLOW-1C 补漏）。
///
/// # 为什么要过一遍 `parse` 再 `as_str`
///
/// 快照里的 `config` 是**发布时的原始 JSON**（编辑器原样输出，大小写不做承诺），
/// 而接口对外承诺大写枚举字面量。直接回原文会让前端拿到 `verify` 这种值、走进错误分支；
/// 解析不出来（脏数据 / 将来新增的取值）一律 `None` —— 前端按 `need_review` 推断兜底，
/// 比给它一个自己不认识、也无从判断的字符串安全。
///
/// # 参数
///
/// - `task`：快照节点的 `config.task`（`None` = 该节点没有任务配置，如审批节点）；
/// - `key`：`on_complete` / `on_reject` / `on_cancel`；
/// - `parse` / `as_str`：对应枚举的解析与规范化（**复用 `node_config` 的唯一真源**，
///   不在这里另写一份大小写映射）。
pub(super) fn switch_field<T>(
    task: Option<&serde_json::Value>,
    key: &str,
    parse: fn(&str) -> Option<T>,
    as_str: fn(T) -> &'static str,
) -> Option<String> {
    let raw = task?.get(key)?.as_str()?;
    parse(raw).map(|value| as_str(value).to_string())
}

/// 查「这张任务来自哪个流程实例的哪个节点」。
///
/// # 为什么一次 JOIN 到底
///
/// 任务详情页要显示「所属审批单」卡片（单号 / 流程名 / 发起人 / 当前节点 / 状态）。
/// 分三次查会让用户在三次网络往返之间看到互相矛盾的状态；一次 JOIN 拿的是同一个快照。
///
/// # 三个「流程走向」为什么从 `version.graph_snapshot` 取（P-FLOW-1C 补漏）
///
/// `on_complete` / `on_reject` / `on_cancel` 是**任务节点自己的配置**，与 `node_name` /
/// `node_type` 同一个道理：历史任务只有「实例生成当时」的口径才有意义 —— 定义行
/// （`biz_flow_node.config`）会跟着版本保存被改，拿它会让三个月前的任务突然显示
/// 「完成后需审批」。`biz_flow_instance.version_id` 是实例冻结的版本，所以在这里取
/// `graph_snapshot.nodes[].config.task`。
///
/// 取值的规范化交给 [`switch_field`]：只认 `DIRECT`/`VERIFY`/`APPROVE` 这些枚举字面量，
/// 其它一律 `None`（前端按 `need_review` 推断兜底）。`config.task` 不存在的节点
/// （审批节点、START/END）天然给 `None`，不会是空串。
///
/// # 返回值
///
/// 非流程任务（`flow_instance_id IS NULL`）→ `Ok(None)`（**不是错误**：
/// 手工任务没有流程，前端据此隐藏卡片）。
///
/// # 「审批进度」从哪来（P-FLOW-1B 收尾）
///
/// 卡片建议写的是「节点本轮的 `approval_instance_id`（`biz_flow_instance_node` 行）」，
/// 但**那张表没有这一列** —— P-FLOW-1B 写入并复用的一直是 `biz_task.approval_instance_id`
/// （`task_bridge::start_task_node_approval` / `prepare_accept` 都 `UPDATE biz_task`）。
/// 所以这里顺着任务行的指针查：`LEFT JOIN biz_flow_instance ai` 取单号与状态，再用
/// 相关子查询聚合 `biz_flow_task` 里 `status='PENDING'` 的待办人姓名。
///
/// 好处是这个口径**同时覆盖两条审批路径**（验收审批 `TASK_ACCEPT` 与节点审批
/// `approve_biz_type`），且 `None` 的判据是「指针为空 / 实例行不存在」—— 不靠
/// `node_status` 推断。唯一需要记住的不变式：验收审批落终态之后任务才 `DONE`，
/// 所以「`task.status=DONE` 且 `approval.status=PENDING`」只可能是节点审批在等结论。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
// SCOPE-EXEMPT: 按 task_id 单条取这张任务的流程块（WHERE t.id = ?::uuid），不产生行集；权限在端点的单条可见性校验：business/task/handler.rs:4515 principal::resolve_for_claims → handler.rs:4517 find_detail(&me, task_id)（内核 task_model_query.rs:222 scope::filter_for）看不见即 404（handler.rs:4520）。
pub async fn flow_ref_of_task(task_id: Uuid) -> DbResult<Option<TaskFlowDetailRow>> {
    let rows = db()
        .find_with_params(
            "SELECT i.id AS instance_id, i.instance_no, i.flow_code, i.flow_name, i.biz_type, \
                    i.biz_no, i.status AS instance_status, i.initiator_id, \
                    COALESCE(NULLIF(i.initiator_name, ''), '') AS initiator_name, \
                    i.started_at, i.ended_at, \
                    t.flow_node_key, t.flow_round, \
                    COALESCE(n.node_name, '') AS node_name, \
                    COALESCE(n.status, '') AS node_status, \
                    (SELECT node -> 'config' -> 'task' \
                       FROM jsonb_array_elements(\
                            COALESCE(v.graph_snapshot -> 'nodes', '[]'::jsonb)) AS node \
                      WHERE node ->> 'key' = t.flow_node_key \
                      LIMIT 1)::text AS task_config_text, \
                    ai.id AS approval_instance_id, \
                    ai.instance_no AS approval_instance_no, \
                    ai.status AS approval_status, \
                    (SELECT string_agg(COALESCE(NULLIF(x.assignee_name, ''), \
                                               NULLIF(u.real_name, ''), u.username), \
                                       E'\\n' ORDER BY x.created_at, x.id) \
                       FROM biz_flow_task x \
                       LEFT JOIN sys_user u ON u.id = x.assignee_id \
                      WHERE x.instance_id = t.approval_instance_id \
                        AND x.status = 'PENDING') AS pending_approver_text \
               FROM biz_task t \
               JOIN biz_flow_instance i ON i.id = t.flow_instance_id \
               LEFT JOIN biz_flow_def_version v ON v.id = i.version_id \
               LEFT JOIN biz_flow_instance_node n \
                      ON n.instance_id = i.id \
                     AND n.node_key = t.flow_node_key \
                     AND n.round = t.flow_round \
               LEFT JOIN biz_flow_instance ai \
                      ON ai.id = t.approval_instance_id \
                     AND ai.deleted_at IS NULL \
              WHERE t.id = ?::uuid AND t.deleted_at IS NULL",
            vec![Value::from(&task_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    // 用 `::text` + `serde_json` 而不是把 jsonb 直接读成 `Value`：出参只关心三个字符串，
    // 而 jsonb → 驱动的解码是一条独立路径（NULL / 类型推断各有各的坑），文本解析最简单。
    let task_config: Option<serde_json::Value> = row
        .get_typed::<Option<String>>("task_config_text")
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok());
    // 审批进度（P-FLOW-1B 收尾）：只认指针 + 实例行，不做推断。
    // 指针为空 / 指向的实例已不在（`LEFT JOIN` 没命中）→ `None`。
    let approval = match row
        .get_typed::<Option<Uuid>>("approval_instance_id")
        .ok()
        .flatten()
        .zip(
            row.get_typed::<Option<String>>("approval_status")
                .ok()
                .flatten(),
        ) {
        Some((instance_id, status)) => {
            let mut pending_approvers = split_approvers(
                row.get_typed::<Option<String>>("pending_approver_text")
                    .ok()
                    .flatten(),
            );
            if status != "PENDING" {
                // 终态实例上残留的 PENDING 待办（脏数据 / 终止竞态）不得冒充「等审批」
                pending_approvers.clear();
            }
            Some(TaskFlowApprovalRow {
                instance_id,
                instance_no: row
                    .get_typed::<Option<String>>("approval_instance_no")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
                status,
                pending_approvers,
            })
        }
        None => None,
    };
    Ok(Some(TaskFlowDetailRow {
        instance_id: row.get_typed("instance_id")?,
        instance_no: row.get_typed("instance_no")?,
        flow_code: row.get_typed("flow_code")?,
        flow_name: row.get_typed("flow_name")?,
        biz_type: row.get_typed("biz_type")?,
        biz_no: row.get_typed("biz_no").ok().flatten(),
        instance_status: row.get_typed("instance_status")?,
        initiator_id: row.get_typed("initiator_id")?,
        initiator_name: row.get_typed("initiator_name").unwrap_or_default(),
        started_at: row.get_typed("started_at")?,
        ended_at: row.get_typed("ended_at").ok().flatten(),
        node_key: row
            .get_typed::<Option<String>>("flow_node_key")
            .ok()
            .flatten()
            .unwrap_or_default(),
        node_name: row.get_typed("node_name").unwrap_or_default(),
        round: row
            .get_typed::<Option<i32>>("flow_round")
            .ok()
            .flatten()
            .unwrap_or(1),
        node_status: row.get_typed("node_status").unwrap_or_default(),
        on_complete: switch_field(
            task_config.as_ref(),
            "on_complete",
            node_config::OnComplete::parse,
            node_config::OnComplete::as_str,
        ),
        on_reject: switch_field(
            task_config.as_ref(),
            "on_reject",
            node_config::OnReject::parse,
            node_config::OnReject::as_str,
        ),
        on_cancel: switch_field(
            task_config.as_ref(),
            "on_cancel",
            node_config::OnCancel::parse,
            node_config::OnCancel::as_str,
        ),
        approval,
    }))
}

// ==================== 写路径 ====================
