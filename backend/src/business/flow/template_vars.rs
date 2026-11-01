//! 模板变量构造器（P-FLOW-1A · 设计 §3.5）。
//!
//! # 一处实现，四处调用
//!
//! | 调用点 | 用哪份键 |
//! |---|---|
//! | `TASK_NODE.title_template` / `description_template` | [`TemplateVars::dotted`] + `render_paths()` |
//! | `NOTIFY_NODE` 的内联模板（P-FLOW-2） | 同上 |
//! | 引用 `template_code` 的通知（`notice::dispatch` → `render()`） | [`TemplateVars::flat`] |
//! | 预演接口 `POST /flow/node/task/preview` | 两份都产出，按请求的 `vars` 覆盖 |
//!
//! # 为什么同时产出「点号键」和「扁平键」
//!
//! - `render_paths()`（新）吃点号键：`{{form.amount}}` → key `"form.amount"`；
//! - `render()`（旧，被 7 条 `sys_notice_template` 用）只认无点号名：`{{form_amount}}`；
//!
//! 一个构造器产出两份键，比两个构造器少一个漂移面。扁平规则固定为「把**唯一的一个点**
//! 换成下划线」，因此 `form.amount ↔ form_amount` 一一对应，不存在歧义
//! （点号路径本来就只允许一层，见 `notice::render::validate_var_path`）。
//!
//! 另外 `flat` 里额外给 4 个**业务别名**（`biz_no` / `instance_title` / `initiator_name` /
//! `node_name`）：现有 4 条 `FLOW_*` 通知模板里写的就是 `{{biz_no}}`，不改它们的
//! `variables` 声明也能继续跑 —— 这条设计把任务书里的口语变量「扶正」了。

use serde_json::{Map, Value as Json};
use std::collections::BTreeMap;

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::graph::SnapshotNode;
use crate::business::flow::model;

/// 一次模板渲染要用的变量表（两份键 + 构造期告警）。
#[derive(Debug, Clone, Default)]
pub struct TemplateVars {
    /// 点号键：`{"form.amount": …, "instance.biz_no": …, "node.name": …}`。
    pub dotted: Json,
    /// 扁平键：`{"form_amount": …, "instance_biz_no": …, "biz_no": …}`。
    pub flat: Json,
    /// 构造过程中的告警（`prev` 为空 / 表单里没有某字段 / 日期已过期…）。
    ///
    /// **绝不写「已自动填充默认值」这类假安慰**（设计 §3.3）。
    pub warnings: Vec<String>,
}

impl TemplateVars {
    /// 由「一层嵌套的 JSON」（`{"form":{…},"initiator":{…}}`）构造两份键。
    ///
    /// 预演接口用它：请求体里的 `vars` 就是这种形状，与运行期 `EvalContext` 的四棵树同构。
    pub fn from_nested(source: &Json, warnings: &mut Vec<String>) -> Self {
        let mut dotted: Map<String, Json> = Map::new();
        let mut flat: Map<String, Json> = Map::new();
        if let Json::Object(roots) = source {
            for (root, leaves) in roots {
                let Json::Object(leaves) = leaves else {
                    // 非对象（如 `{"biz_no": "X"}` 这种已经扁平的写法）按扁平键原样收下。
                    dotted.insert(root.clone(), leaves.clone());
                    flat.insert(root.clone(), leaves.clone());
                    continue;
                };
                for (leaf, value) in leaves {
                    dotted.insert(format!("{root}.{leaf}"), value.clone());
                    flat.insert(format!("{root}_{leaf}"), value.clone());
                }
            }
        }
        // 业务别名（设计 §3.5 的四行表）：只从点号键派生，缺就不放（渲染期会报缺变量）。
        for (alias, path) in [
            ("biz_no", "instance.biz_no"),
            ("instance_no", "instance.instance_no"),
            ("instance_title", "instance.title"),
            ("initiator_name", "initiator.name"),
            ("node_name", "node.name"),
        ] {
            if let Some(value) = dotted.get(path) {
                flat.insert(alias.to_string(), value.clone());
            }
        }
        if dotted.get("prev.approver_name").is_none() {
            warnings.push(
                "上一审批人（prev.approver_name）为空：本节点之前没有已通过的审批节点".to_string(),
            );
        }
        Self {
            dotted: Json::Object(dotted),
            flat: Json::Object(flat),
            warnings: warnings.clone(),
        }
    }
}

/// 构造某一轮、某一节点的模板变量表（**在事务内调用**，`ex` 传 `&tx`）。
///
/// # 参数
///
/// - `ex`：执行器（事务），用于查「上一审批人」；
/// - `instance`：流程实例行（提供 `biz_no` / `title` / `initiator_*` / `form_data`）；
/// - `node`：快照节点（提供 `node.name` / `node.key`）；
/// - `round`：当前轮次（退回重进时 `round+1`，「上一审批人」按同一轮取）；
/// - `strict`：`true` 时「上一审批人缺失」只记 warning 不报错 —— 变量表本身**不**决定报错，
///   报错由渲染器（缺变量 → 422）决定。保留本参数是为了让调用方签名与设计文档一致，
///   并让「严格模式」的告警措辞更明确。
///
/// # 返回值
///
/// [`TemplateVars`]（点号键 + 扁平键 + 告警）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn build(
    ex: &impl SqlExecutor,
    instance: &model::InstanceRow,
    node: &SnapshotNode,
    round: i32,
    strict: bool,
) -> DbResult<TemplateVars> {
    let mut warnings: Vec<String> = Vec::new();
    let mut dotted: Map<String, Json> = Map::new();
    let mut flat: Map<String, Json> = Map::new();

    // ── form.*：发起时冻结的 form_data（一层 KV） ──
    if let Json::Object(form) = &instance.form_data {
        for (key, value) in form {
            dotted.insert(format!("form.{key}"), value.clone());
            flat.insert(format!("form_{key}"), value.clone());
        }
    }

    // ── instance.* ──
    dotted.insert(
        "instance.biz_type".to_string(),
        Json::String(instance.biz_type.clone()),
    );
    if let Some(biz_no) = &instance.biz_no {
        dotted.insert("instance.biz_no".to_string(), Json::String(biz_no.clone()));
    } else {
        warnings.push("业务单号（instance.biz_no）为空：本流程没有关联业务单号".to_string());
    }
    dotted.insert(
        "instance.title".to_string(),
        Json::String(instance.title.clone()),
    );

    // ── initiator.* ──
    dotted.insert(
        "initiator.id".to_string(),
        Json::String(instance.initiator_id.to_string()),
    );
    if let Some(name) = &instance.initiator_name {
        dotted.insert("initiator.name".to_string(), Json::String(name.clone()));
    }
    if let Some(dept) = instance.initiator_dept_id {
        dotted.insert(
            "initiator.dept_id".to_string(),
            Json::String(dept.to_string()),
        );
    }

    // ── node.*（只有模板能用，条件 DSL 白名单里没有这两个） ──
    dotted.insert("node.name".to_string(), Json::String(node.name.clone()));
    dotted.insert("node.key".to_string(), Json::String(node.key.clone()));

    // ── prev.*：本实例、本轮、当前节点之前最后一个已通过审批节点上的最后签字人 ──
    match prev_approver(ex, instance.id, round).await? {
        Some((name, id)) => {
            dotted.insert("prev.approver_name".to_string(), Json::String(name));
            dotted.insert("prev.approver_id".to_string(), Json::String(id.to_string()));
        }
        None => {
            warnings.push(format!(
                "上一审批人（prev.approver_name）为空：第 {round} 轮里本节点之前没有已通过的审批节点{}",
                if strict {
                    "（严格模式下渲染会因此失败）"
                } else {
                    ""
                }
            ));
        }
    }

    // ── 复制一遍扁平键（`根_叶`），再补 4 个业务别名 ──
    for (key, value) in &dotted {
        if let Some((root, leaf)) = key.split_once('.') {
            flat.insert(format!("{root}_{leaf}"), value.clone());
        }
    }
    for (alias, path) in [
        ("biz_no", "instance.biz_no"),
        ("instance_title", "instance.title"),
        ("initiator_name", "initiator.name"),
        ("node_name", "node.name"),
    ] {
        if let Some(value) = dotted.get(path) {
            flat.insert(alias.to_string(), value.clone());
        }
    }

    Ok(TemplateVars {
        dotted: Json::Object(dotted),
        flat: Json::Object(flat),
        warnings,
    })
}

/// 上一审批人（姓名, id）：本实例 + 本轮 + 当前节点之前最后一个「已通过」的审批 / 会签节点。
///
/// # 口径（设计 §3.5 的 SQL，逐条落地）
///
/// - 只取 `status='APPROVED'` 的节点（`SKIPPED` 的节点不算「审批人」）；
/// - 按 `ended_at DESC` 取最近一个（「上一」= 时间上最近的）；
/// - 该节点上按 `completed_at DESC` 取最后一个 `APPROVED` 的待办。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn prev_approver(
    ex: &impl SqlExecutor,
    instance_id: Uuid,
    round: i32,
) -> DbResult<Option<(String, Uuid)>> {
    let rows = ex
        .query_sql(
            "SELECT u.real_name, u.username, u.id \
               FROM biz_flow_instance_node n \
               JOIN biz_flow_task t ON t.instance_node_id = n.id AND t.status = 'APPROVED' \
               JOIN sys_user u ON u.id = t.assignee_id \
              WHERE n.instance_id = ?::uuid \
                AND n.round = ? \
                AND n.status = 'APPROVED' \
                AND n.node_type IN ('APPROVAL', 'COUNTERSIGN') \
                AND n.ended_at IS NOT NULL \
              ORDER BY n.ended_at DESC, t.completed_at DESC \
              LIMIT 1"
                .to_string(),
            vec![Value::from(&instance_id), Value::from(round)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let real_name: Option<String> = row.get_typed("real_name").ok();
    let username: String = row.get_typed("username")?;
    let id: Uuid = row.get_typed("id")?;
    let name = real_name
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or(username);
    Ok(Some((name, id)))
}

/// 把「缺失的变量」填成可见占位 `〔path〕`（**只给预演用**，运行期永远走严格渲染）。
///
/// # 为什么要有它
///
/// `render()` / `render_paths()` 的规则 2 是「缺变量 → 422，**不静默替换成空串**」。
/// 预演接口如果也照这个口径，用户在还没填模拟变量时点预览会满屏红 —— 体验极差，
/// 而且会让人误以为配置错了。
///
/// `strict=false` **不是静默**：它把缺失显式渲染成 `〔form.amount〕` 并给出 warning，
/// 用户一眼就能看到「这里没变量」。`strict=true` 才是运行期的真实行为。
///
/// # 返回值
///
/// `(填好占位的变量表, 缺失的路径列表)`。
pub fn fill_missing_with_placeholders(vars: &Json, templates: &[&str]) -> (Json, Vec<String>) {
    let mut filled: Map<String, Json> = vars.as_object().cloned().unwrap_or_default();
    let mut missing: Vec<String> = Vec::new();
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    for template in templates {
        // 语法错误在这里**吞掉**：它由调用方用 `collect_placeholders_paths` 单独报 65036，
        // 这里只负责「缺变量的可见化」。
        let Ok(paths) = crate::business::notice::render::collect_placeholders_paths(template)
        else {
            continue;
        };
        for path in paths {
            if filled.get(&path).is_some_and(|value| !value.is_null()) {
                continue;
            }
            if seen.insert(path.clone(), ()).is_some() {
                continue;
            }
            filled.insert(path.clone(), Json::String(format!("〔{path}〕")));
            missing.push(path);
        }
    }
    (Json::Object(filled), missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 一份嵌套变量 → 点号键 + 扁平键**双份**（设计 §3.5 的核心约定）。
    #[test]
    fn from_nested_produces_both_key_shapes() {
        let mut warnings = Vec::new();
        let vars = TemplateVars::from_nested(
            &json!({
                "form": {"amount": 125000, "supplier_name": "华东金属"},
                "instance": {"biz_no": "PO202611010001", "title": "采购申请"},
                "initiator": {"name": "张三"},
                "node": {"name": "催货", "key": "n_urge"},
                "prev": {"approver_name": "李四"}
            }),
            &mut warnings,
        );
        // 点号键：给 `render_paths` 用
        assert_eq!(vars.dotted["form.amount"], json!(125000));
        assert_eq!(vars.dotted["instance.biz_no"], json!("PO202611010001"));
        assert_eq!(vars.dotted["prev.approver_name"], json!("李四"));
        // 扁平键：给老的 `notice::dispatch` → `render()` 用
        assert_eq!(vars.flat["form_amount"], json!(125000));
        assert_eq!(vars.flat["instance_biz_no"], json!("PO202611010001"));
        // 4 个业务别名（现有 FLOW_* 模板里写的就是 {{biz_no}}）
        assert_eq!(vars.flat["biz_no"], json!("PO202611010001"));
        assert_eq!(vars.flat["instance_title"], json!("采购申请"));
        assert_eq!(vars.flat["initiator_name"], json!("张三"));
        assert_eq!(vars.flat["node_name"], json!("催货"));
        assert!(warnings.is_empty(), "变量齐全时不该有告警：{warnings:?}");
    }

    /// 上一审批人缺失 → 只记告警（**不报错**：报错由渲染器按 strict 决定）。
    #[test]
    fn from_nested_warns_when_prev_approver_missing() {
        let mut warnings = Vec::new();
        let vars = TemplateVars::from_nested(&json!({"form": {}}), &mut warnings);
        assert!(
            warnings.iter().any(|w| w.contains("prev.approver_name")),
            "必须点名缺哪个变量：{warnings:?}"
        );
        assert!(vars.dotted.get("prev.approver_name").is_none());
    }

    /// 预演的非严格模式：缺变量换成可见占位 `〔path〕`，并回报缺了哪些。
    #[test]
    fn fill_missing_uses_visible_placeholders() {
        let vars = json!({"form.amount": 1});
        let (filled, missing) = fill_missing_with_placeholders(
            &vars,
            &["{{form.amount}} / {{form.supplier_name}} / {{node.name}}"],
        );
        assert_eq!(filled["form.amount"], json!(1), "已有的值不许被覆盖");
        assert_eq!(
            filled["form.supplier_name"],
            json!("〔form.supplier_name〕")
        );
        assert_eq!(filled["node.name"], json!("〔node.name〕"));
        assert_eq!(
            missing,
            vec!["form.supplier_name".to_string(), "node.name".to_string()],
            "缺失路径按字典序返回，且不重复"
        );
        // 占位渲染出来必须看得见（不是空串 —— 那就成了「静默替换」）
        let out = crate::business::notice::render::render_paths(
            "{{form.supplier_name}}",
            &filled,
            crate::business::notice::dto::ContentFormat::Text,
        )
        .expect("填完占位必须能渲染");
        assert_eq!(out, "〔form.supplier_name〕");
    }
}
