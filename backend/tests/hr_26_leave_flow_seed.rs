//! 迁移 `V0393__hr_leave_flow`（请假审批流种子 `hr_leave`）的静态不变量。
//!
//! 卡：t_10899fe4（HR-26【HR】迁移 V0393，设计号 V0384 / 曾映射 V0390）。
//! 设计依据 `docs/architecture/ARCH-9-人力资源模块设计.md`
//! §3.2 ⑨（4 套流程的拓扑：`hr_leave` = 直属上级 →（≥3 天）人事经理 → END）、
//! §4「自审批」行（`INITIATOR_SUPERIOR` + `FALLBACK_ADMIN`，不用 SKIP）、
//! §7.1 的 `hr_leave_flow` 行（四重守卫）、§8.1 的 HR-26 行。
//!
//! # 号段为什么是 V0393（卡面写 V0384，设计映射写 V0390）
//!
//! `V0384` 被 HR 系列自己的 `hr_accounts` 实占；`V0390/V0391/V0392` 被成本菜单分组 /
//! 成本流程 / HR-23 考勤锁定守卫实占。nik 迁移加载器对版本号做**跨目录全局唯一**校验，
//! 重号 = 服务启动直接失败（AGENTS 约定 6）。全局不变量（四位补零 / up 必有 down /
//! 版本号唯一）由 `tests/flow_all_role_multiversion_migration.rs` 统一覆盖，这里不重复。
//!
//! # 这层测试钉死什么（13 条）
//!
//! 1. ★ up/down 成对，且文件名与 `flow_code` 一致（改名忘配 down = 回滚做不成）。
//! 2. ★ 不许 `+no-transaction`：自检 RAISE 之后前面已执行的语句必须整份回滚。
//! 3. ★ `biz_type` / `flow_code` / 分类 / 节点名与设计逐格对得上。
//! 4. ★ `is_default` + `is_system` + `allow_auto_approve = FALSE`（免审直过是
//!    AGENTS 约定 11 的最高危分支；系统预置流程客户不能删）。
//! 5. ★ 四重守卫齐全：同名 flow_code / 同 biz_type 默认流 / 角色缺失 / 版本重复。
//! 6. ★ 版本号 `COALESCE(MAX(version_no),0)+1`（写死 1 撞客户已有的 v1）。
//! 7. ★ `is_current` 只在「还没有当前版本」时才置真（种子的职责是补缺口）。
//! 8. ★ 审批节点 **approval_mode = ANY**，且执行 SQL 里**没有** `'ALL'`
//!    （角色型审批 + ALL = 全员会签，一人停用就永久卡单：V0359/V0367 的 P0）。
//! 9. ★ 审批人来源：`n_sup` = `INITIATOR_SUPERIOR`（随发起人变化，**不落枚举行**）、
//!    `n_hr` = 角色 `HR_MANAGER`（…f0016）。
//! 10. ★ `graph_hash` = sha256(canonical(快照))：用仓里同一套 canonical 规则**重算**，
//!     改快照不改 hash（或反之）立刻红。
//! 11. ★ 快照数组**已按引擎的确定性排序**（节点按 key、边按 from,to,key）：
//!     否则库里「行重算 hash」与「快照 hash」会分叉，`flow::maintain` 每日自检报 drift。
//! 12. ★ 长假条件边的字段是**数值镜像** `form.duration_days_num`（字符串会让求值器
//!     TypeMismatch → 静默走兜底 → 长假绕过人事复核），且该字段被条件 DSL 白名单接受。
//! 13. down 的三道安全阀 + 只碰本卡固定 id。
//!
//! 纯文件解析，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up 幂等 / down 幂等 / 守卫反证 / down 安全阀）见
//! `docs/evidence/2026-11-12-hr26-leave-flow-v0393/02_drill.sh`；
//! 真库形态与引擎校验（`FlowGraph::validate` / `graph_hash` 三方对账 / 条件求值）见
//! `tests/hr_26_leave_flow_seed_db.rs`。

use std::path::PathBuf;

use cloudpivot::business::flow::graph;
use serde_json::Value;

fn backend_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_migration(relative: &str) -> String {
    let path = backend_root().join("migrations").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到迁移文件 {}: {e}", path.display()))
}

/// 把连续空白压成单个空格：SQL 断言只关心语句形状，不该被缩进/换行绑死。
fn squash(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（比如 `approval_mode = 'ALL'`
/// 这个 P0 形态、`FORBID` 的死单风险），「禁止出现」类断言必须只看真正会执行的 SQL。
fn sql_only(sql: &str) -> String {
    sql.lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_has(sql: &str, snippet: &str, why: &str) {
    let squashed = squash(sql);
    assert!(
        squashed.contains(&squash(snippet)),
        "{why}\n期望包含：{}\n（SQL 全文 {} 字符，请直接看迁移文件；这里只回显期望片段）",
        squash(snippet),
        squashed.len()
    );
}

fn assert_lacks(sql: &str, snippet: &str, why: &str) {
    let squashed = squash(sql);
    assert!(
        !squashed.contains(&squash(snippet)),
        "{why}\n不该包含：{}",
        squash(snippet)
    );
}

const UP_PATH: &str = "business/V0393__hr_leave_flow.up.sql";
const DOWN_PATH: &str = "business/V0393__hr_leave_flow.down.sql";

const FLOW_CODE: &str = "hr_leave";
const BIZ_TYPE: &str = "HR_LEAVE";
const DEF_ID: &str = "20000000-0000-4000-8000-0000000e0393";
const VERSION_ID: &str = "20000000-0000-4000-8000-0000000e0394";
const HR_MANAGER: &str = "20000000-0000-4000-8000-0000000f0016";

/// 迁移里写死的 `graph_hash`（sha256 of canonical 快照）。改图必须同步改它 + 重算。
const EXPECTED_HASH: &str = "4fc6722abf629a8a9a84580942dd2f6a0a44431786b2e500905531affc0e1f97";

/// 从 `$snapshot$…$snapshot$` 里取出发布快照文本（迁移里就这一处）。
fn snapshot_literal(sql: &str) -> &str {
    let tag = "$snapshot$";
    let start = sql.find(tag).unwrap_or_else(|| panic!("迁移里找不到 {tag} 快照字面量")) + tag.len();
    let rest = &sql[start..];
    let end = rest.find(tag).unwrap_or_else(|| panic!("快照字面量没有闭合的 {tag}"));
    &rest[..end]
}

fn snapshot_value() -> Value {
    let up = read_migration(UP_PATH);
    let raw = snapshot_literal(&up);
    serde_json::from_str(raw).unwrap_or_else(|e| panic!("快照不是合法 JSON：{e}"))
}

// ============================================================================
// ①–② 文件与事务性
// ============================================================================

#[test]
fn up_and_down_are_paired_and_named_for_the_flow() {
    let up = read_migration(UP_PATH);
    let down = read_migration(DOWN_PATH);
    assert!(!up.trim().is_empty(), "up 迁移不能是空文件");
    assert!(!down.trim().is_empty(), "down 迁移不能是空文件");
    assert_has(&up, "hr_leave", "迁移必须种 hr_leave（设计 §3.2 ⑨ flow_code）");
    assert_has(&down, "hr_leave", "down 必须回滚同一条流程");
    assert_has(&down, DEF_ID, "down 只碰本卡固定 def id");
    assert_has(&down, VERSION_ID, "down 只碰本卡固定 version id");
}

#[test]
fn migration_is_transactional() {
    let up = read_migration(UP_PATH);
    let down = read_migration(DOWN_PATH);
    assert_lacks(
        &up,
        "+no-transaction",
        "迁移改的是全局流程配置：自检 RAISE 后必须整份回滚",
    );
    assert_lacks(
        &down,
        "+no-transaction",
        "down 同样不许非事务执行（安全阀抛错后不能留半删状态）",
    );
}

// ============================================================================
// ③–④ 定义口径
// ============================================================================

#[test]
fn definition_matches_design_section_3_2_nine() {
    let up = read_migration(UP_PATH);
    assert_has(&up, &format!("'{FLOW_CODE}'"), "flow_code 见设计 §3.2 ⑨");
    assert_has(
        &up,
        &format!("'{BIZ_TYPE}'"),
        "biz_type 见设计 §3.2 ⑨（与 leave_handler::FLOW_BIZ_TYPE 同值）",
    );
    assert_has(&up, "'人力资源'", "分类：人事流程落在「人力资源」域");
    assert_has(
        &up,
        "'请假审批（标准·直属上级 → 长假人事复核）'",
        "流程名（两级：直属上级 → 长假人事复核）",
    );
    // 节点名与设计拓扑逐格对得上
    assert_has(&up, "'直属上级审批'", "第一级：直属上级（INITIATOR_SUPERIOR）");
    assert_has(&up, "'请假天数判定（≥3 天）'", "条件节点：≥3 天判定");
    assert_has(&up, "'人事经理复核'", "第二级：人事经理（HR_MANAGER）");
    // 默认流程 + 已发布 + 系统预置 + 不免审（def 插入列里是连续的）
    assert_has(
        &up,
        format!("'PUBLISHED', '{VERSION_ID}'::uuid, 1, TRUE, FALSE, TRUE, NULL").as_str(),
        "def 行必须是 PUBLISHED + current_version_no=1 + is_default + allow_auto_approve=FALSE + is_system",
    );
}

#[test]
fn auto_approve_is_forbidden() {
    let up = sql_only(&read_migration(UP_PATH));
    assert_lacks(
        &up,
        "allow_auto_approve = TRUE",
        "免审必须 FALSE（AGENTS 约定 11 的最高危分支）",
    );
    assert_lacks(&up, "allow_auto_approve = true", "免审必须 FALSE（小写形态也拦）");
    assert_has(&up, "'FALLBACK_ADMIN'", "审批节点 0 人要有兜底（否则派不出去 = 死单）");
}

// ============================================================================
// ⑤–⑦ 四重守卫与版本递增
// ============================================================================

#[test]
fn four_guards_are_all_present() {
    let raw = read_migration(UP_PATH);
    // 守卫的谓词之间夹着说明性 `--` 注释，断言必须只看会执行的 SQL
    let up = sql_only(&raw);
    assert_has(
        &up,
        &format!("NOT EXISTS ( SELECT 1 FROM biz_flow_def WHERE flow_code = '{FLOW_CODE}' AND deleted_at IS NULL )"),
        "守卫①：已有同名 flow_code（客户自建 / 上次已种）→ 整条跳过",
    );
    assert_has(
        &up,
        &format!("WHERE biz_type = '{BIZ_TYPE}' AND is_default AND deleted_at IS NULL"),
        "守卫②：同 biz_type 已有默认流程（uk_biz_flow_def_default）→ 整条跳过",
    );
    assert_has(
        &up,
        format!("SELECT count(*) FROM sys_role WHERE id = '{HR_MANAGER}'::uuid AND deleted_at IS NULL ) = 1").as_str(),
        "守卫③：HR_MANAGER 角色不存在 / 已软删 → 整条跳过（不种派不出去的流程）",
    );
    assert_has(
        &up,
        "x.change_note LIKE 'V0393 种子%'",
        "守卫④a：本卡版本已在 → 不重复插（幂等的核心）",
    );
    assert_has(
        &up,
        format!("x.graph_hash = '{EXPECTED_HASH}'").as_str(),
        "守卫④b：同 hash 的版本已在 → 不重复发（换了 change_note 也不重复）",
    );
}

#[test]
fn version_increments_and_never_steals_current() {
    let up = sql_only(&read_migration(UP_PATH));
    assert_has(
        &up,
        "COALESCE((SELECT MAX(x.version_no) FROM biz_flow_def_version x WHERE x.def_id = d.id), 0) + 1",
        "版本号必须 MAX+1（写死 1 会撞客户已有的 v1：uk_biz_flow_def_version）",
    );
    assert_has(
        &up,
        "NOT EXISTS (SELECT 1 FROM biz_flow_def_version x WHERE x.def_id = d.id AND x.is_current)",
        "is_current 只在「还没有当前版本」时置真（不抢客户版本）",
    );
}

// ============================================================================
// ⑧–⑨ 节点：或签 + 审批人来源
// ============================================================================

#[test]
fn approval_nodes_are_any_not_all() {
    let up = sql_only(&read_migration(UP_PATH));
    // 每个审批节点都必须显式 ANY（不靠 NULL 兜底）
    assert_eq!(
        up.matches("'ANY'::varchar").count(),
        2,
        "两个审批节点都要显式 approval_mode = ANY（或签）"
    );
    // 任何一处把 approval_mode 写成 'ALL' 值都是全员会签事故（V0359/V0367 的 P0）。
    // 注意不能直接搜 "ALL"：`FALLBACK_ADMIN` 里就有 ALL，会假红。
    assert_lacks(&up, "'ALL'::varchar", "审批节点的 approval_mode 不许是 ALL");
    // 自检里那条「ALL 必须一条都没有」的反向断言本身要留着
    assert_has(
        &up,
        "AND (approval_mode = 'ALL' OR approval_mode IS NULL)",
        "§6 自检必须断言「没有 ALL / 没有 NULL 的审批节点」",
    );
    assert_lacks(
        &up,
        "'COUNTERSIGN'::varchar",
        "不建会签节点（要多人共识必须显式建 COUNTERSIGN，而不是把 APPROVAL 改成 ALL）",
    );
    // 兜底 + 超时只提醒（不许自动通过）
    assert_eq!(
        up.matches("'FALLBACK_ADMIN'::varchar").count(),
        2,
        "两个审批节点都要 FALLBACK_ADMIN 兜底"
    );
    assert_lacks(
        &up,
        "'SKIP'::varchar, 'ALLOW'",
        "审批节点不许 SKIP 兜底（SKIP = 没人可派就自动通过）",
    );
}

#[test]
fn approvers_are_superior_and_hr_role() {
    let up = sql_only(&read_migration(UP_PATH));
    assert_has(
        &up,
        "'INITIATOR_SUPERIOR'::varchar, 1::int",
        "n_sup 必须是 INITIATOR_SUPERIOR + superior_levels=1（直属上级随发起人变化）",
    );
    assert_has(
        &up,
        format!("'ROLE'::varchar, '{HR_MANAGER}'::uuid, 0::int").as_str(),
        "n_hr 必须绑角色 HR_MANAGER（…f0016），不是写死某个人、也不是全员会签",
    );
    // INITIATOR_SUPERIOR 是**不可枚举**来源：不许给它落枚举行（落行等于把「谁的上级」写死）
    let down = read_migration(DOWN_PATH);
    assert_has(&down, "biz_flow_node_assignee", "down 要先删审批人绑定（外键顺序）");
    let assignee_block: Vec<&str> = up
        .lines()
        .skip_while(|line| !line.contains("INSERT INTO biz_flow_node_assignee"))
        .take(8)
        .collect();
    let block = assignee_block.join("\n");
    assert_eq!(
        block.matches("'ROLE'::varchar").count(),
        1,
        "审批人绑定只该有 1 行（n_hr → HR_MANAGER）"
    );
    assert!(
        !block.contains("'INITIATOR_SUPERIOR'"),
        "n_sup 不许落审批人枚举行：{block}"
    );
}

// ============================================================================
// ⑩–⑫ hash / 排序 / 条件边
// ============================================================================

#[test]
fn graph_hash_matches_the_snapshot_it_ships_with() {
    let up = read_migration(UP_PATH);
    let snapshot = snapshot_value();
    let canonical = graph::canonical_json(&snapshot);
    use sha2::Digest;
    let digest = sha2::Sha256::digest(canonical.as_bytes());
    let recomputed = digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();

    assert_eq!(
        recomputed, EXPECTED_HASH,
        "快照重算的 hash 必须等于常量（改快照不改 hash = flow::maintain 每日自检报 drift）"
    );
    assert_has(
        &up,
        EXPECTED_HASH,
        "迁移 SQL 里必须写入同一个 graph_hash（列值 + 自检常量两处）",
    );
    assert_eq!(
        up.matches(EXPECTED_HASH).count(),
        4,
        "hash 应出现 4 次：注释 1 + 列值 1 + 守卫④b 1 + §6 自检常量 1"
    );
}

#[test]
fn snapshot_arrays_are_in_engine_deterministic_order() {
    let snapshot = snapshot_value();
    let nodes = snapshot["nodes"].as_array().expect("nodes 必须是数组");
    let keys: Vec<&str> = nodes
        .iter()
        .map(|node| node["key"].as_str().expect("node.key"))
        .collect();
    let mut sorted_keys = keys.clone();
    sorted_keys.sort();
    assert_eq!(
        keys, sorted_keys,
        "快照 nodes 必须按 key 升序（graph::snapshot() 的确定性排序）；否则行 hash 与快照 hash 分叉"
    );

    let edges = snapshot["edges"].as_array().expect("edges 必须是数组");
    let order: Vec<(&str, &str, &str)> = edges
        .iter()
        .map(|edge| {
            (
                edge["from"].as_str().expect("edge.from"),
                edge["to"].as_str().expect("edge.to"),
                edge["key"].as_str().expect("edge.key"),
            )
        })
        .collect();
    let mut sorted_order = order.clone();
    sorted_order.sort();
    assert_eq!(
        order, sorted_order,
        "快照 edges 必须按 (from, to, key) 升序（同上，确定性排序）"
    );

    // 拓扑与设计一致：s → n_sup → c_days → {e | n_hr → e}
    assert_eq!(
        order,
        vec![
            ("c_days", "e", "e_short"),
            ("c_days", "n_hr", "e_long"),
            ("n_hr", "e", "e_hr_end"),
            ("n_sup", "c_days", "e_sup_days"),
            ("s", "n_sup", "e_start"),
        ],
        "骨架边必须恰好是这 5 条（直属上级 → 天数判定 →（≥3 天）人事经理 → 结束）"
    );
    let node_types: Vec<&str> = nodes
        .iter()
        .map(|node| node["node_type"].as_str().expect("node.node_type"))
        .collect();
    assert_eq!(
        node_types,
        vec!["CONDITION", "END", "APPROVAL", "APPROVAL", "START"],
        "5 个节点：条件 / 结束 / 人事经理 / 直属上级 / 开始（按 key 排序后）"
    );
}

#[test]
fn long_leave_condition_uses_the_numeric_mirror_and_is_dsl_legal() {
    let snapshot = snapshot_value();
    let edges = snapshot["edges"].as_array().expect("edges");
    let long_edge = edges
        .iter()
        .find(|edge| edge["key"] == "e_long")
        .expect("必须有长假条件边 e_long");
    let condition = long_edge["condition"].clone();
    assert!(!condition.is_null(), "e_long 必须带条件（否则 R13：非兜底边没有条件）");
    assert_eq!(condition["field"], "form.duration_days_num", "条件字段必须是数值镜像");
    assert_eq!(condition["cmp"], "gte", "≥3 天用 gte（恰好 3 天也要复核）");
    assert_eq!(condition["value"], 3, "阈值 3 天（设计 §3.2 ⑨「（≥3 天）」）");
    assert_eq!(long_edge["is_default"], false, "条件边不能是兜底边");

    // 条件必须能被引擎的 DSL 接受（字段在白名单里、算子合法）
    graph::parse_condition(&condition).expect("条件 DSL 必须合法（否则提交审批时 500 / 发布被 R10 拦）");
    assert!(
        graph::field_allowed("form.duration_days_num"),
        "form.* 白名单必须接受 duration_days_num"
    );

    // 兜底边：恰一条、不带条件（R13）
    let defaults: Vec<&Value> = edges.iter().filter(|edge| edge["is_default"] == true).collect();
    assert_eq!(defaults.len(), 1, "条件节点的兜底边必须恰好 1 条");
    assert_eq!(defaults[0]["key"], "e_short", "兜底边是「请假 < 3 天 → 直接结束」");
    assert!(defaults[0]["condition"].is_null(), "兜底边不许带条件");
}

#[test]
fn condition_field_is_a_number_produced_by_the_handler() {
    // 种子里的条件字段必须真的由 `leave_handler::leave_form_data` 产出**数值**。
    // 这里做源码级对拍（handler 的单元测试断言它是 JSON number）：
    // 改名 / 改成字符串都会让这条红 —— 否则长假会静默绕过人事复核。
    let handler = std::fs::read_to_string(backend_root().join("src/business/hr/leave_handler.rs"))
        .expect("读不到 leave_handler.rs");
    assert!(
        handler.contains("\"duration_days_num\""),
        "leave_handler::leave_form_data 必须产出 duration_days_num（种子条件边的字段）"
    );
    assert!(
        handler.contains("form[\"duration_days_num\"] = serde_json::Value::Number"),
        "duration_days_num 必须以 JSON number 落库（字符串 → 求值器 TypeMismatch → 静默走兜底边）"
    );
}

// ============================================================================
// ⑬ down：安全阀 + 只碰本卡 id
// ============================================================================

#[test]
fn down_has_safety_valves_and_touches_only_this_card() {
    let down = read_migration(DOWN_PATH);
    assert_has(
        &down,
        "FROM biz_flow_instance WHERE biz_type = ''HR_LEAVE''",
        "安全阀 a：已有 HR_LEAVE 审批实例 → 拒绝回滚（version_id 是 RESTRICT 外键，删了就是孤儿单）",
    );
    assert_has(
        &down,
        "FROM biz_leave_request WHERE approval_instance_id IS NOT NULL",
        "安全阀 b：请假单已挂实例 → 拒绝回滚（链路已被用过）",
    );
    assert_has(
        &down,
        "FROM biz_flow_def_version WHERE def_id = $1 AND id <> $2",
        "安全阀 c：本流程下还有别的版本（客户发过新版）→ 拒绝回滚",
    );
    // 删除顺序：先子后父（外键）
    let assignee_pos = down.find("DELETE FROM biz_flow_node_assignee").expect("删审批人绑定");
    let edge_pos = down.find("DELETE FROM biz_flow_edge").expect("删边");
    let node_pos = down.find("DELETE FROM biz_flow_node\n").expect("删节点");
    let version_pos = down.find("DELETE FROM biz_flow_def_version").expect("删版本");
    let def_pos = down.rfind("DELETE FROM biz_flow_def\n").expect("删定义");
    assert!(
        assignee_pos < edge_pos && edge_pos < node_pos && node_pos < version_pos && version_pos < def_pos,
        "down 必须自底向上删（审批人 → 边 → 节点 → 版本 → 定义），否则被外键挡"
    );
    assert_has(
        &down,
        "current_version_id = NULL",
        "删版本前必须摘掉 def 的当前版本指针（否则指针指向不存在的版本）",
    );
}
