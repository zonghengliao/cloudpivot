// ============================================================================
// flow_model_def.rs —— 流程定义与版本（含模板）：定义/版本的只读查询、编码占用、默认流程、草稿与版本号推进、定义增改删与启停、模板列表
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::flow_model_base::*;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 流程定义分页（未软删；关键词匹配编码或名称）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn page_defs(filter: &DefFilter, page: u64, page_size: u64) -> DbResult<DefPage> {
    let mut where_sql = String::from(" WHERE d.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(keyword) = filter.keyword.as_deref() {
        where_sql.push_str(" AND (d.flow_code ILIKE ? OR d.flow_name ILIKE ?)");
        let pattern = format!("%{}%", keyword.replace('%', "\\%").replace('_', "\\_"));
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }
    if let Some(status) = filter.status.as_deref() {
        where_sql.push_str(" AND d.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(biz_type) = filter.biz_type.as_deref() {
        where_sql.push_str(" AND d.biz_type = ?");
        params.push(Value::Text(biz_type.to_string()));
    }

    let count_sql = format!("SELECT count(*) AS total FROM biz_flow_def d{where_sql}");
    let total_row = db().find_with_params(&count_sql, params.clone()).await?;
    let total: i64 = match total_row.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };

    let mut sql = String::with_capacity(512);
    sql.push_str("SELECT ");
    sql.push_str(DEF_COLUMNS);
    sql.push_str(DEF_FROM);
    sql.push_str(&where_sql);
    sql.push_str(" ORDER BY d.updated_at DESC, d.id DESC LIMIT ? OFFSET ?");
    let mut page_params = params;
    page_params.push(Value::from(page_size as i64));
    page_params.push(Value::from(((page.saturating_sub(1)) * page_size) as i64));

    let rows = db().find_with_params(&sql, page_params).await?;
    let list = rows
        .iter()
        .map(DefRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(DefPage {
        list,
        total: total.max(0) as u64,
        page,
        size: page_size,
    })
}

// ==================== 模板库（P-FLOW-5） ====================

/// 模板库条目行（`GET /business/flow/def/templates` 的一行）。
///
/// 比 [`DefRow`] 多三样模板库独有的东西：
/// - `is_system`：系统预置（前端标「标准」）；
/// - `view_version_id` / `view_version_no`：**能拿去看图的那个版本** ——
///   有发布版就是发布版，没发布版（客户另存为出来的 DRAFT）退到最新草稿，
///   不这样处理，模板库里但凡有草稿就会「点查看流程图一片空白」；
/// - `usage_count` / `last_used_at`：该定义被发起过多少次（查 `biz_flow_instance`，
///   按 `def_id` 归组；删掉的实例不计）。
#[derive(Debug, Clone)]
pub struct TemplateRow {
    /// 主键。
    pub id: Uuid,
    /// 编码。
    pub flow_code: String,
    /// 名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 分类（分组依据）。
    pub category: Option<String>,
    /// 说明。
    pub description: Option<String>,
    /// 状态（`DRAFT` / `PUBLISHED` / `DISABLED`）。
    pub status: String,
    /// 是否该业务类型的默认流程。
    pub is_default: bool,
    /// 是否系统预置。
    pub is_system: bool,
    /// 可查看流程图的版本（发布版 > 最新草稿；一个版本都没有 → `None`）。
    pub view_version_id: Option<Uuid>,
    /// 该版本号（没有版本 → 0）。
    pub view_version_no: i32,
    /// 节点数。
    pub node_count: i64,
    /// 连线数。
    pub edge_count: i64,
    /// 已用次数（发起的审批实例数）。
    pub usage_count: i64,
    /// 最近一次使用时间。
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl TemplateRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            category: row.get_typed("category")?,
            description: row.get_typed("description")?,
            status: row.get_typed("status")?,
            is_default: row.get_typed("is_default")?,
            is_system: row.get_typed("is_system")?,
            view_version_id: row.get_typed("view_version_id")?,
            view_version_no: row.get_typed("view_version_no")?,
            node_count: row.get_typed("node_count")?,
            edge_count: row.get_typed("edge_count")?,
            usage_count: row.get_typed("usage_count")?,
            last_used_at: row.get_typed("last_used_at")?,
        })
    }

    /// 分组名：分类为空 → `未分类`（绝不把空串当分组名丢给前端）。
    pub fn category_label(&self) -> String {
        match self.category.as_deref().map(str::trim) {
            Some(text) if !text.is_empty() => text.to_string(),
            _ => "未分类".to_string(),
        }
    }
}

/// 标准流程清单的口径（P2-6b 收口后语义 = 「标准流程」列表）。
///
/// 「模板」不是实体，是 `biz_flow_def.is_system = TRUE` 这个**角色**（决策 A：合并概念），
/// 所以这里不引入任何模板表，只区分「要不要混入客户另存为的副本」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateScope {
    /// 只看标准流程（`is_system = TRUE`）—— 接口默认口径。
    Standard,
    /// 标准流程 + 客户「另存为」出来的副本（`is_system = FALSE`）。
    All,
}

/// 标准流程清单（未软删的定义，标准流程排前、再按分类 / 编码）。
///
/// **一条 SQL 出全部数据**（含 `usage_count` 与 `view_version_id`）：
/// 模板库是「一次看全」，逐条查使用次数会变成 N+1。
///
/// # 口径（P2-6b：语义收口为「标准流程」，不是独立实体）
///
/// - [`TemplateScope::Standard`]：`WHERE d.deleted_at IS NULL AND d.is_system`；
/// - [`TemplateScope::All`]：不过滤 `is_system`（标准流程 + 客户副本）；
/// - **两种口径都不按 `status` 过滤**：标准流程可能被客户停用（`DISABLED`），
///   一旦按状态过滤，它就从管理视图里消失、再也启用不回来；`status` 只给前端做标签展示。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn templates(scope: TemplateScope) -> DbResult<Vec<TemplateRow>> {
    // 只切 `is_system` 这一句（字面量常量拼接，无外部输入 → 无注入面），
    // 其余 SQL 两种口径逐字一致，避免两份 SQL 漂移。
    let scope_clause = match scope {
        TemplateScope::Standard => " AND d.is_system",
        TemplateScope::All => "",
    };
    let sql = format!(
        "SELECT d.id, d.flow_code, d.flow_name, d.biz_type, d.category, d.description, \
                d.status, d.is_default, d.is_system, \
                COALESCE(d.current_version_id, lv.id) AS view_version_id, \
                CAST(COALESCE(v.version_no, lv.version_no, 0) AS int) AS view_version_no, \
                CAST(COALESCE(v.node_count, lv.node_count, 0) AS bigint) AS node_count, \
                CAST(COALESCE(v.edge_count, lv.edge_count, 0) AS bigint) AS edge_count, \
                CAST(COALESCE(u.usage_count, 0) AS bigint) AS usage_count, u.last_used_at \
           FROM biz_flow_def d \
           LEFT JOIN biz_flow_def_version v ON v.id = d.current_version_id \
           LEFT JOIN LATERAL ( \
                SELECT l.id, l.version_no, l.node_count, l.edge_count \
                  FROM biz_flow_def_version l WHERE l.def_id = d.id \
                 ORDER BY l.version_no DESC LIMIT 1 \
           ) lv ON TRUE \
           LEFT JOIN ( \
                SELECT i.def_id, count(*) AS usage_count, max(i.started_at) AS last_used_at \
                  FROM biz_flow_instance i WHERE i.deleted_at IS NULL GROUP BY i.def_id \
           ) u ON u.def_id = d.id \
          WHERE d.deleted_at IS NULL{scope_clause} \
          ORDER BY d.is_system DESC, d.category NULLS LAST, d.flow_code"
    );
    let rows = db().find(&sql).await?;
    rows.iter().map(TemplateRow::from_row).collect()
}

/// 模板库「一句话说明」素材：各定义可查看版本的**条件连线标签**（按优先级）。
///
/// 返回 `(def_id, 标签)` 列表，调用方按 def_id 归组后拼成
/// 「金额 < 10000 / 10000 ≤ 金额 < 100000 / 金额 ≥ 100000」这种一眼看懂的分流说明。
///
/// **只保留「分流到审批节点」的边**（`to_node.node_type <> 'CONDITION'`）：
/// 金额分级流程里，网关 A 的兜底边写的是「金额 ≥ 10000」而它指向的是网关 B，
/// 这条边不是一档业务分层（真分层是同网关的「金额 < 10000」和网关 B 的
/// 「10000 ≤ 金额 < 100000」「金额 ≥ 100000」）。把网关之间的中转边拼进摘要，
/// 就会出现「≥ 1 万」和「< 1 万」并列这种自相矛盾的说明。
/// 判据用「目标节点类型」而不是 `is_default`：网关 B 的兜底边「金额 ≥ 100000」
/// 虽然也是 `is_default = true`，但它真的指向会签节点，是有效的一档。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn template_condition_labels() -> DbResult<Vec<(Uuid, String)>> {
    let rows = db()
        .find(
            "SELECT v.def_id, e.condition_label \
               FROM biz_flow_edge e \
               JOIN biz_flow_def_version v ON v.id = e.version_id \
               JOIN biz_flow_def d ON d.id = v.def_id \
               JOIN biz_flow_node n ON n.id = e.to_node_id \
              WHERE d.deleted_at IS NULL \
                AND v.id = COALESCE(d.current_version_id, ( \
                        SELECT l.id FROM biz_flow_def_version l WHERE l.def_id = d.id \
                         ORDER BY l.version_no DESC LIMIT 1)) \
                AND n.node_type <> 'CONDITION' \
                AND e.condition_label IS NOT NULL AND btrim(e.condition_label) <> '' \
              ORDER BY v.def_id, e.priority, e.edge_key",
        )
        .await?;
    let mut out: Vec<(Uuid, String)> = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push((row.get_typed("def_id")?, row.get_typed("condition_label")?));
    }
    Ok(out)
}

/// 按 id 取流程定义（未软删；不存在 / 已删 → `None`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_def(id: Uuid) -> DbResult<Option<DefRow>> {
    let sql =
        format!("SELECT {DEF_COLUMNS}{DEF_FROM} WHERE d.id = ?::uuid AND d.deleted_at IS NULL");
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(DefRow::from_row).transpose()
}

/// 编码是否已被占用（软删过滤；`exclude` 用于编辑自身时排除）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn flow_code_taken(code: &str, exclude: Option<Uuid>) -> DbResult<bool> {
    let mut sql = String::from(
        "SELECT count(*) AS total FROM biz_flow_def WHERE flow_code = ? AND deleted_at IS NULL",
    );
    let mut params = vec![Value::Text(code.to_string())];
    if let Some(id) = exclude {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&id));
    }
    let rows = db().find_with_params(&sql, params).await?;
    let total: i64 = match rows.first() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };
    Ok(total > 0)
}

/// 按 id 取版本（含发布人姓名）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_version(id: Uuid) -> DbResult<Option<VersionRow>> {
    find_version_in(db(), id).await
}

/// [`find_version`] 的「任意执行器」版本（P-FLOW-1B，用于事务内发起审批单时取版本号）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_version_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<VersionRow>> {
    let sql = format!("SELECT {VERSION_COLUMNS}{VERSION_FROM} WHERE v.id = ?::uuid");
    ex.query_first_sql(sql, vec![Value::from(&id)])
        .await?
        .map(|row| VersionRow::from_row(&row))
        .transpose()
}

/// 某流程的全部版本（版本号倒序）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn versions_of(def_id: Uuid) -> DbResult<Vec<VersionRow>> {
    let sql = format!(
        "SELECT {VERSION_COLUMNS}{VERSION_FROM} WHERE v.def_id = ?::uuid ORDER BY v.version_no DESC"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&def_id)])
        .await?;
    rows.iter().map(VersionRow::from_row).collect()
}

/// 某流程最新的 DRAFT 版本（没有 → `None`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn latest_draft(def_id: Uuid) -> DbResult<Option<VersionRow>> {
    let sql = format!(
        "SELECT {VERSION_COLUMNS}{VERSION_FROM} \
          WHERE v.def_id = ?::uuid AND v.version_status = 'DRAFT' \
          ORDER BY v.version_no DESC LIMIT 1"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&def_id)])
        .await?;
    rows.first().map(VersionRow::from_row).transpose()
}

/// 取某流程的业务类型（编辑 `is_default` 时需要知道同组还有谁）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn biz_type_of(ex: &impl SqlExecutor, def_id: Uuid) -> DbResult<Option<String>> {
    let rows = ex
        .query_sql(
            "SELECT biz_type FROM biz_flow_def WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&def_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(row.get_typed("biz_type")?)),
        None => Ok(None),
    }
}

/// 把同业务类型的其它默认流程清掉（部分唯一索引 `uk_biz_flow_def_default` 的前置动作）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn clear_default_of_biz_type(
    ex: &impl SqlExecutor,
    biz_type: &str,
    keep: Uuid,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_flow_def SET is_default = FALSE, updated_at = now() \
          WHERE biz_type = ? AND deleted_at IS NULL AND is_default AND id <> ?::uuid"
            .to_string(),
        vec![Value::Text(biz_type.to_string()), Value::from(&keep)],
    )
    .await
}

/// 某流程最大的版本号（没有版本 → 0）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn max_version_no(ex: &impl SqlExecutor, def_id: Uuid) -> DbResult<i32> {
    let rows = ex
        .query_sql(
            "SELECT COALESCE(max(version_no), 0) AS max_no \
               FROM biz_flow_def_version WHERE def_id = ?::uuid"
                .to_string(),
            vec![Value::from(&def_id)],
        )
        .await?;
    let value: i32 = match rows.first() {
        Some(row) => row.get_typed("max_no")?,
        None => 0,
    };
    Ok(value)
}

/// 新建流程定义（事务内）。
///
/// # 异常情况
///
/// 数据库异常（含 `uk_biz_flow_def_code` / `uk_biz_flow_def_default` 冲突）→ [`DbError`]。
pub async fn create_def(ex: &Transaction, new: &NewDef, operator: Uuid) -> DbResult<()> {
    ex.execute_with_params(
        "INSERT INTO biz_flow_def \
             (id, flow_code, flow_name, biz_type, category, description, status, \
              current_version_no, is_default, allow_auto_approve, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?, ?, ?, 'DRAFT', 0, ?, ?, now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&new.id),
            Value::Text(new.flow_code.clone()),
            Value::Text(new.flow_name.clone()),
            Value::Text(new.biz_type.clone()),
            opt_text(new.category.as_deref()),
            opt_text(new.description.as_deref()),
            Value::from(new.is_default),
            Value::from(new.allow_auto_approve),
            Value::from(&operator),
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(())
}

/// 改流程元信息（只改 `biz_flow_def`，不碰版本）。
///
/// # 异常情况
///
/// 数据库异常（含唯一索引冲突）→ [`DbError`]。
pub async fn update_def(id: Uuid, patch: &DefPatch, operator: Uuid) -> DbResult<u64> {
    let mut sets: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(name) = patch.flow_name.as_deref() {
        sets.push("flow_name = ?");
        params.push(Value::Text(name.to_string()));
    }
    if let Some(biz_type) = patch.biz_type.as_deref() {
        sets.push("biz_type = ?");
        params.push(Value::Text(biz_type.to_string()));
    }
    if let Some(category) = patch.category.as_ref() {
        sets.push("category = ?");
        params.push(opt_text(category.as_deref()));
    }
    if let Some(description) = patch.description.as_ref() {
        sets.push("description = ?");
        params.push(opt_text(description.as_deref()));
    }
    if let Some(is_default) = patch.is_default {
        sets.push("is_default = ?");
        params.push(Value::from(is_default));
    }
    if let Some(allow) = patch.allow_auto_approve {
        sets.push("allow_auto_approve = ?");
        params.push(Value::from(allow));
    }
    if sets.is_empty() {
        return Ok(0);
    }
    sets.push("updated_at = now()");
    sets.push("update_by = ?::uuid");
    params.push(Value::from(&operator));
    params.push(Value::from(&id));
    let sql = format!(
        "UPDATE biz_flow_def SET {} WHERE id = ?::uuid AND deleted_at IS NULL",
        sets.join(", ")
    );
    db().execute_with_params(&sql, params).await
}

/// 软删流程定义（版本 / 节点 / 边保留 —— 历史实例仍可引用它们）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn soft_delete_def(id: Uuid, operator: Uuid) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_flow_def SET deleted_at = now(), updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&operator), Value::from(&id)],
    )
    .await
}

/// 停用流程（`status = 'DISABLED'`，不动版本、不动在跑实例）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn mark_disabled(id: Uuid, operator: Uuid) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_flow_def SET status = 'DISABLED', updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&operator), Value::from(&id)],
    )
    .await
}

/// 恢复启用（`POST /def/{id}/enable` 的写路径）：`DISABLED → PUBLISHED`。
///
/// 三个 `WHERE` 条件都是**语义闸门**，不是性能优化：
/// - `status = 'DISABLED'`：只有被停用的流程才谈得上「恢复启用」；
///   `DRAFT` 要走发布（图校验 + 快照），这里放过去就等于绕过 12 条发布校验。
/// - `current_version_id IS NOT NULL`：没有已发布版本就「启用」= 让发起接口拿不到图，
///   必须在写之前拦下来（返回 0 行，handler 据此报 409）。
/// - `deleted_at IS NULL`：软删的流程不能被复活。
///
/// 备注：`is_default` 不动 —— 停用没清默认标记（历史口径），恢复后默认关系自然回来。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn mark_enabled(id: Uuid, operator: Uuid) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_flow_def SET status = 'PUBLISHED', updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL \
            AND status = 'DISABLED' AND current_version_id IS NOT NULL",
        vec![Value::from(&operator), Value::from(&id)],
    )
    .await
}

// ==================== 写路径：版本 ====================

/// 新建版本行（事务内）。
///
/// # 异常情况
///
/// 数据库异常（含 `uk_biz_flow_def_version` 冲突）→ [`DbError`]。
pub async fn create_version(
    ex: &Transaction,
    def_id: Uuid,
    version_no: i32,
    change_note: Option<&str>,
    operator: Uuid,
) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    ex.execute_with_params(
        "INSERT INTO biz_flow_def_version \
             (id, def_id, version_no, version_status, is_current, node_count, edge_count, \
              change_note, created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?::uuid, ?, 'DRAFT', FALSE, 0, 0, ?, now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&id),
            Value::from(&def_id),
            Value::from(version_no),
            opt_text(change_note),
            Value::from(&operator),
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(id)
}

/// 更新版本的变更备注与行数（保存整图后回写）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn touch_version(
    ex: &Transaction,
    version_id: Uuid,
    change_note: Option<&str>,
    node_count: i64,
    edge_count: i64,
    operator: Uuid,
) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_flow_def_version \
            SET change_note = COALESCE(?, change_note), node_count = ?, edge_count = ?, \
                updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND version_status = 'DRAFT'",
        vec![
            opt_text(change_note),
            Value::from(node_count),
            Value::from(edge_count),
            Value::from(&operator),
            Value::from(&version_id),
        ],
    )
    .await
}

/// 按 `flow_code` 取流程定义（未软删）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_def_by_code(code: &str) -> DbResult<Option<DefRow>> {
    let sql =
        format!("SELECT {DEF_COLUMNS}{DEF_FROM} WHERE d.flow_code = ? AND d.deleted_at IS NULL");
    let rows = db()
        .find_with_params(&sql, vec![Value::Text(code.to_string())])
        .await?;
    rows.first().map(DefRow::from_row).transpose()
}

/// 取某业务类型的**默认已发布流程**（发起时只给 `biz_type` 用）。
///
/// 排序口径：`is_default` 优先，其次当前版本号大的（最新发布的那个）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn default_def_of_biz_type(biz_type: &str) -> DbResult<Option<DefRow>> {
    default_def_of_biz_type_in(db(), biz_type).await
}

/// [`default_def_of_biz_type`] 的「任意执行器」版本（P-FLOW-1B）。
///
/// # 为什么需要它
///
/// `on_complete=APPROVE` 的发起发生在 `change_status` 的**事务内**（任务状态与审批单必须
/// 同生共死）。在事务闭包里调 `default_def_of_biz_type()` 会走 `db()` 的另一条连接 ——
/// 同一行在两边读写就是自锁风险。这里把执行器变成入参，`&Transaction` 与 `&Db` 都能传。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn default_def_of_biz_type_in(
    ex: &impl SqlExecutor,
    biz_type: &str,
) -> DbResult<Option<DefRow>> {
    let sql = format!(
        "SELECT {DEF_COLUMNS}{DEF_FROM} \
          WHERE d.biz_type = ? AND d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
            AND d.current_version_id IS NOT NULL \
          ORDER BY d.is_default DESC, d.current_version_no DESC, d.updated_at DESC \
          LIMIT 1"
    );
    ex.query_first_sql(sql, vec![Value::Text(biz_type.to_string())])
        .await?
        .map(|row| DefRow::from_row(&row))
        .transpose()
}

// ---------- 运行侧批注：run-time runtime block (D2) ----------
// ==================== 运行侧：实例 / 节点 / 待办 / 抄送（D2） ====================
//
// 依据：`docs/批次D-工作流审批-技术架构.md` §6.2（4 张表的列清单）、§4.4（一次流转算法）、
// §4.5（并发三件套）、§4.6（退回 round）、§9.1（实例可见性谓词）。
//
// # 本段的三条纪律
//
// 1. **锁顺序固定** `biz_flow_task` → `biz_flow_instance_node` → `biz_flow_instance`
//    （架构 §4.5.1）。所有 `*_for_update` 函数只做 `SELECT … FOR UPDATE`，
//    由 `engine.rs` 按上述顺序调用；这里不隐含任何加锁顺序，避免「函数内部顺手锁了实例」。
// 2. **计票一律现算**：`tally_of_node` 是唯一的票数来源，`approved_count` / `rejected_count`
//    只是 `update_node_counts` 写进去的展示冗余（**没有任何 `+=`**）。
// 3. **`auto_passed_nodes` 也是派生的**：`sync_auto_passed(instance_id)` 用
//    `count(*) WHERE is_auto_passed` 覆写，而不是 `auto_passed_nodes + 1`
//    —— 审计计数一旦能漂移，坑 3 的「本单有 1 个节点自动通过」就不可信了。

// ---------- 列清单 ----------
