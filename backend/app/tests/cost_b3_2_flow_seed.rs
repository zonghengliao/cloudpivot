//! 迁移 `V0381__cost_flow_def`（成本结算审批流种子 `cost_settle_approve`）的静态不变量。
//!
//! 卡：t_bef918b4（COST-B3-2【成本】审批流种子）—— 设计依据
//! `docs/architecture/ARCH-9-成本核算升级设计.md` §4.2（审批流怎么接：biz_type / flow_code /
//! 两级节点 / 驳回打回 DRAFT / 自审批 / 免审 FALSE）、§7.1 的 V0378 行（四重守卫）、
//! §8.1 的 B3-2。范本 = `V0332__purchase_return.up.sql` §⑦。
//!
//! # 号段为什么是 V0381 而不是卡面写的 V0378
//!
//! V0378/V0379/V0380 已被前置卡 B1-7 占用（成本菜单权限 / 号段字典 / 批号回填）。
//! nik 迁移加载器对版本号做跨目录全局唯一校验，重号 = 服务启动直接失败（AGENTS 约定 6）。
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 统一覆盖，这里不重复。
//!
//! # 这层测试钉死什么（12 条）
//!
//! 1. ★ **up/down 成对**，且文件命名与 `flow_code` 一致（改名忘配 down = 回滚做不成）。
//! 2. ★ **`-- +no-transaction`**：自检 RAISE 之后前面已执行的语句必须整份回滚
//!    （本迁移改了 `biz_flow_def` 等全局配置表，绝对不能留半成品）。
//! 3. ★ **biz_type / flow_code / 两级节点名**：设计 §4.2 的表格逐格对得上
//!    （`COST_SETTLE` / `cost_settle_approve` / 财务经理 → 财务总监）。
//! 4. ★ **`allow_auto_approve = FALSE`**：结算动库存与凭证，免审直过一旦存在就必须
//!    自带全套补偿（AGENTS 约定 11），本流程从源头不给这个口子。
//! 5. ★ **四重守卫齐全**：同名 flow_code / 同 biz_type 默认流 / 角色不齐 / 版本重复
//!    各有一条守卫，少一条就会在生产客户库上覆盖配置或撞唯一索引（V0359 事故的镜像）。
//! 6. ★ **版本号取 `COALESCE(MAX(version_no),0)+1`**，不是写死 1（V0367 复盘：
//!    写死 1 撞客户已有的 v1 ⇒ `duplicate key value violates uk_biz_flow_def_version`）。
//! 7. ★ **`is_current` 只在「还没有当前版本」时才置真**：种子的职责是补缺口，不是抢客户版本。
//! 8. ★ **节点 `approval_mode` 必须是 `ANY`**（或签），且**全文不出现 `'ALL'`**：
//!    角色型审批 + ALL = 「岗位上所有人都要点头」，一人停用就永久卡单（V0359/V0367 的 P0）。
//! 9. ★ **审批人绑角色**（…f0005 财务经理 / …f0006 财务总监），不是写死某个人、也不是全员会签。
//! 10. ★ **`graph_hash` = sha256(canonical(快照))**：用仓里同一套 canonical 规则重算，
//!     改快照不改 hash（或反之）立刻红 —— 否则 `flow::maintain` 每日自检才会报 drift。
//! 11. **down 的三道安全阀**：有 COST_SETTLE 实例 / 结算批次挂了实例 / 本流程下还有别的版本
//!     → 拒绝回滚（删版本会留下指向空气的审批单）。
//! 12. **down 只碰本卡固定 id**：客户自建的同名流程不在删除范围内。
//!
//! 纯文件解析，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up 幂等 / down 幂等 / 三重守卫反证 / 版本递增 / down 安全阀）见
//! `docs/evidence/2026-11-11-cost-b3-2-flow-seed-v0381/02_drill.sh`；
//! 真库形态与引擎校验（`FlowGraph::validate`）见 `tests/cost_b3_2_flow_seed_db.rs`。

use std::path::PathBuf;

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

const UP_PATH: &str = "business/V0381__cost_flow_def.up.sql";
const DOWN_PATH: &str = "business/V0381__cost_flow_def.down.sql";

/// 从 `$snapshot$…$snapshot$` 里取出发布快照文本（迁移里就这一处）。
fn snapshot_literal(sql: &str) -> &str {
    let start_tag = "$snapshot$";
    let start = sql
        .find(start_tag)
        .unwrap_or_else(|| panic!("迁移里找不到 {start_tag} 快照字面量"))
        + start_tag.len();
    let rest = &sql[start..];
    let end = rest
        .find(start_tag)
        .unwrap_or_else(|| panic!("快照字面量没有闭合的 {start_tag}"));
    &rest[..end]
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
    assert_has(
        &up,
        "cost_settle_approve",
        "迁移必须种 cost_settle_approve（设计 §4.2 flow_code）",
    );
    assert_has(
        &down,
        "cost_settle_approve",
        "down 必须能定位到本卡流程（安全阀 / 提示文案）",
    );
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
fn definition_matches_design_section_4_2() {
    let up = read_migration(UP_PATH);
    // biz_type / flow_code / 分类 / 状态
    assert_has(&up, "'cost_settle_approve'", "flow_code 见设计 §4.2");
    assert_has(&up, "'COST_SETTLE'", "biz_type 见设计 §4.2 / §4 状态机");
    assert_has(&up, "'财务管理'", "分类与其它财务流程一致（成本属财务管理）");
    assert_has(&up, "'成本结算审批（标准·两级）'", "流程名（两级）");
    // 两级审批节点名
    assert_has(&up, "'财务经理审批'", "第一级：财务经理（FINANCE_MANAGER）");
    assert_has(&up, "'财务总监审批'", "第二级：财务总监（FINANCE_DIRECTOR）");
    // 默认流程 + 已发布 + 不免审（三个值在 def 的插入列里是连续的）
    assert_has(
        &up,
        "'PUBLISHED', '20000000-0000-4000-8000-0000000b0382'::uuid, 1, TRUE, FALSE, NULL",
        "def 行必须是 PUBLISHED + current_version_no=1 + is_default=TRUE + allow_auto_approve=FALSE",
    );
}

#[test]
fn auto_approve_is_forbidden() {
    let up = sql_only(&read_migration(UP_PATH));
    // 设计 §4.2 末行：「免审 不允许。allow_auto_approve = FALSE」
    // 只要有一处把它写成 TRUE，就是给结算开了免审直过（AGENTS 约定 11 的最高危分支）。
    assert_lacks(&up, "allow_auto_approve = TRUE", "免审必须 FALSE");
    assert_lacks(&up, "allow_auto_approve = true", "免审必须 FALSE（小写形态也拦）");
    assert_has(
        &up,
        "'FALLBACK_ADMIN'",
        "角色空了要有兜底审批人（否则节点 0 人 = 死单）",
    );
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
        "NOT EXISTS ( SELECT 1 FROM biz_flow_def WHERE flow_code = 'cost_settle_approve' AND deleted_at IS NULL )",
        "守卫①：已有同名 flow_code（客户自建 / 上次已种）→ 整条跳过",
    );
    assert_has(
        &up,
        "WHERE biz_type = 'COST_SETTLE' AND is_default AND deleted_at IS NULL",
        "守卫②：同 biz_type 已有默认流程（uk_biz_flow_def_default）→ 整条跳过",
    );
    assert_has(
        &up,
        "SELECT count(*) FROM sys_role WHERE id IN ('20000000-0000-4000-8000-0000000f0005'::uuid, '20000000-0000-4000-8000-0000000f0006'::uuid) AND deleted_at IS NULL ) = 2",
        "守卫③：两个审批角色都在才种（角色缺失种出来的流程派不出去）",
    );
    assert_has(
        &up,
        "x.change_note LIKE 'V0381 种子%'",
        "守卫④a：本卡版本已在 → 不重复插",
    );
    assert_has(
        &up,
        "x.graph_hash = '8a4fbd64cf5fb5cfc3b133ef2e4ff6103cd14a9afbabfae2b6f63f5845eb4171'",
        "守卫④b：同 hash 的版本已在 → 不重复发版",
    );
    // 守卫跳过只提示，不阻断升级（覆盖客户配置比种子没生效更严重）
    assert_has(
        &raw,
        "RAISE NOTICE 'COST-B3-2: 已存在 flow_code",
        "守卫① 跳过时只 NOTICE",
    );
    assert_has(&raw, "RAISE NOTICE 'COST-B3-2: 审批角色只找到", "守卫③ 跳过时只 NOTICE");
}

#[test]
fn version_number_is_incremented_not_hardcoded() {
    let up = read_migration(UP_PATH);
    assert_has(
        &up,
        "COALESCE((SELECT MAX(x.version_no) FROM biz_flow_def_version x WHERE x.def_id = d.id), 0) + 1",
        "版本号必须 MAX+1（V0367 复盘：写死 1 撞客户已有 v1 ⇒ duplicate key）",
    );
    assert_has(
        &up,
        "NOT EXISTS (SELECT 1 FROM biz_flow_def_version x WHERE x.def_id = d.id AND x.is_current)",
        "is_current 只在「还没有当前版本」时置真（种子补缺口，不抢客户版本）",
    );
    assert_has(
        &up,
        "ON CONFLICT DO NOTHING",
        "版本 / 节点 / 边插入必须能容忍重复（幂等第二道闸）",
    );
}

// ============================================================================
// ⑧–⑨ 节点形状：或签 + 绑角色
// ============================================================================

#[test]
fn approval_nodes_are_any_not_all() {
    let raw = read_migration(UP_PATH);
    let up = sql_only(&raw);
    // 两个审批节点必须显式落 ANY（不靠 NULL 兜底语义）
    let any_count = up.matches("'ANY'::varchar").count();
    assert_eq!(
        any_count, 2,
        "必须恰好 2 个节点显式写 'ANY'::varchar（两级审批），实际 {any_count}"
    );
    // 节点取值里不许出现 'ALL'（角色型审批 + ALL = 全员会签，V0359/V0367 的 P0）
    assert!(
        !up.contains("'ALL'::varchar"),
        "角色型审批节点写 'ALL' = 全员会签（一人停用就永久卡单）"
    );
    // 自检里还有一道「ALL / NULL 一律报错」的行级兜底（它在 sql_only 里是正常语句）
    assert_has(
        &raw,
        "AND (approval_mode = 'ALL' OR approval_mode IS NULL)",
        "自检必须断言「没有 ALL / 没有未落库的 approval_mode」",
    );
}

#[test]
fn approval_nodes_bind_roles_not_people() {
    let up = read_migration(UP_PATH);
    // 节点行：assignee_type = 'ROLE'
    let role_nodes = up.matches("'APPROVAL'::varchar, 'ROLE'::varchar").count();
    assert_eq!(
        role_nodes, 2,
        "两个审批节点都必须 assignee_type='ROLE'，实际 {role_nodes}"
    );
    // 审批人绑定表：n_fm → 财务经理、n_fd → 财务总监
    assert_has(
        &up,
        "('20000000-0000-4000-8000-0000000b0392'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int)",
        "n_fm 必须绑 FINANCE_MANAGER（…f0005）",
    );
    assert_has(
        &up,
        "('20000000-0000-4000-8000-0000000b0393'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0006'::uuid, 0::int)",
        "n_fd 必须绑 FINANCE_DIRECTOR（…f0006）",
    );
    // 不许把审批人写成具体用户（USER 型绑定）
    assert_lacks(
        &up,
        "'USER'::varchar, '20000000-0000-4000-8000-000000",
        "审批人必须是角色，不是写死某个人",
    );
}

#[test]
fn self_check_covers_two_levels_and_chain() {
    let up = read_migration(UP_PATH);
    assert_has(
        &up,
        "'START,APPROVAL,APPROVAL,END'",
        "自检必须钉住节点类型序列（两级审批）",
    );
    assert_has(&up, "node_count = 4 AND edge_count = 3", "自检必须钉住 4 节点 / 3 边");
    assert_has(
        &up,
        "AND node_type = 'APPROVAL' AND assignee_type = 'ROLE' AND approval_mode = 'ANY' AND empty_assignee = 'FALLBACK_ADMIN'",
        "自检必须断言「绑角色 + 或签 + 有兜底」的审批节点恰好 2 个",
    );
}

// ============================================================================
// ⑩ graph_hash 自洽（用仓里同一套 canonical 规则重算）
// ============================================================================

/// 期望的 `graph_hash`：快照 canonical JSON 的 sha256（小写 hex）。
const EXPECTED_HASH: &str = "8a4fbd64cf5fb5cfc3b133ef2e4ff6103cd14a9afbabfae2b6f63f5845eb4171";

#[test]
fn snapshot_is_canonicalizable_and_hash_matches() {
    let up = read_migration(UP_PATH);
    let text = snapshot_literal(&up);
    let value: serde_json::Value = serde_json::from_str(text).unwrap_or_else(|e| panic!("快照不是合法 JSON：{e}"));
    // 用**仓里同一个** canonical 实现（`graph.rs::canonical_json`），不在这里另写一套：
    // 两套实现只要有一处不一致，就会出现「迁移里 hash 对、引擎重算对不上」的幽灵 drift。
    let canonical = cloudpivot::business::flow::graph::canonical_json(&value);
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(canonical.as_bytes());
    let hex = digest.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(
        hex, EXPECTED_HASH,
        "迁移里写的 graph_hash 与快照重算不一致（改快照必须同步改 hash，反之亦然）"
    );
    assert_has(&up, EXPECTED_HASH, "迁移必须把这同一个值写进 graph_hash 列与自检常量");

    // 快照结构：4 节点 / 3 边 / schema_version 1，审批节点是 ANY 且带角色
    let nodes = value["nodes"].as_array().expect("快照缺 nodes");
    let edges = value["edges"].as_array().expect("快照缺 edges");
    assert_eq!(nodes.len(), 4, "快照节点数");
    assert_eq!(edges.len(), 3, "快照边数");
    assert_eq!(value["schema_version"], 1, "快照格式版本");
    let approvals: Vec<&serde_json::Value> = nodes.iter().filter(|n| n["node_type"] == "APPROVAL").collect();
    assert_eq!(approvals.len(), 2, "快照里必须恰好 2 个审批节点");
    for node in approvals {
        assert_eq!(node["approval_mode"], "ANY", "快照里审批节点必须是或签 ANY");
        assert_eq!(node["assignee_type"], "ROLE", "快照里审批人来源必须是角色");
        let refs: Vec<&str> = node["assignees"]
            .as_array()
            .expect("审批节点缺 assignees")
            .iter()
            .map(|a| a["ref_id"].as_str().expect("assignee 缺 ref_id"))
            .collect();
        assert_eq!(refs.len(), 1, "每个审批节点恰好绑一个角色");
        assert!(
            matches!(
                refs[0],
                "20000000-0000-4000-8000-0000000f0005" | "20000000-0000-4000-8000-0000000f0006"
            ),
            "快照绑的角色必须是财务经理 / 财务总监，实际 {}",
            refs[0]
        );
    }
    // 链路形状：s → n_fm → n_fd → e
    let mut chain: Vec<(String, String)> = edges
        .iter()
        .map(|e| {
            (
                e["from"].as_str().unwrap_or_default().to_string(),
                e["to"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    chain.sort();
    assert_eq!(
        chain,
        vec![
            ("n_fd".to_string(), "e".to_string()),
            ("n_fm".to_string(), "n_fd".to_string()),
            ("s".to_string(), "n_fm".to_string()),
        ],
        "审批链必须是 s → n_fm → n_fd → e（两级顺序审批）"
    );
}

// ============================================================================
// ⑪–⑫ down：安全阀 + 只碰自己的行
// ============================================================================

#[test]
fn down_has_safety_valves() {
    let down = read_migration(DOWN_PATH);
    assert_has(
        &down,
        "SELECT count(*) FROM biz_flow_instance WHERE biz_type = ''COST_SETTLE''",
        "安全阀 a：已有 COST_SETTLE 审批实例 → 拒绝回滚（版本是 RESTRICT 外键）",
    );
    assert_has(
        &down,
        "SELECT count(*) FROM biz_cost_period WHERE approval_instance_id IS NOT NULL",
        "安全阀 b：结算批次已挂实例 → 拒绝回滚",
    );
    assert_has(
        &down,
        "WHERE def_id = $1 AND id <> $2",
        "安全阀 c：本流程下还有别的版本（客户发过新版）→ 拒绝回滚",
    );
    // 表可能先被别的迁移回滚掉 → 读之前先看它在不在，否则 down 不幂等
    assert_eq!(
        down.matches("to_regclass('public.").count(),
        2,
        "两处跨模块表读取都要有 to_regclass 存在性护栏"
    );
    assert_has(&down, "RAISE EXCEPTION 'V0381 down 中止", "安全阀必须抛错并点名");
}

#[test]
fn down_only_touches_its_own_fixed_ids() {
    let down = sql_only(&read_migration(DOWN_PATH));
    for id in [
        "20000000-0000-4000-8000-0000000b0381",
        "20000000-0000-4000-8000-0000000b0382",
    ] {
        assert!(
            down.contains(id),
            "down 必须按本卡固定 id 删除（客户自建的同名流程不在删除范围内）：缺 {id}"
        );
    }
    // 删除语句只许按 id / version_id 精确命中，不许按 flow_code 批量删（会误删客户流程）
    assert_lacks(
        &down,
        "DELETE FROM biz_flow_def WHERE flow_code =",
        "down 不许按 flow_code 批量删定义（客户可能有同名自建流程）",
    );
    // 删除顺序：审批人 → 边 → 节点 → 版本 → 定义（反了会被外键挡住）
    // 注意 `biz_flow_node` 是 `biz_flow_node_assignee` 的前缀 —— 匹配要带上 ` WHERE`，
    // 否则两次定位命中同一处，顺序断言永远「相等」而看不出真错。
    let squashed = squash(&down);
    let pos = |snippet: &str| {
        squashed
            .find(snippet)
            .unwrap_or_else(|| panic!("down 里找不到 {snippet}"))
    };
    assert!(
        pos("DELETE FROM biz_flow_node_assignee WHERE") < pos("DELETE FROM biz_flow_edge WHERE")
            && pos("DELETE FROM biz_flow_edge WHERE") < pos("DELETE FROM biz_flow_node WHERE")
            && pos("DELETE FROM biz_flow_node WHERE") < pos("DELETE FROM biz_flow_def_version WHERE")
            && pos("DELETE FROM biz_flow_def_version WHERE") < pos("DELETE FROM biz_flow_def WHERE id ="),
        "down 的删除顺序必须自底向上（审批人 → 边 → 节点 → 版本 → 定义）"
    );
}
