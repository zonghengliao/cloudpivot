//! 迁移 `V0394__hr_salary_flow` / `V0395__hr_adjust_resign_flow` 的静态不变量。
//!
//! 卡：t_37006311（HR-38【HR】迁移 V0385，设计号 → 实落 **V0394 + V0395**）。
//! 设计依据 `docs/architecture/ARCH-9-人力资源模块设计.md`
//! §3.2 ⑨（4 套流程的拓扑）：
//!   · `hr_salary`          = 人事经理 → 财务经理（会签）→（应发 ≥ 50 万）总经理 → END
//!   · `hr_salary_adjust`   = 直属上级 → 人事经理 → 财务经理 → END
//!   · `hr_employee_resign` = 直属上级 → 人事经理 →（关键岗位）总经理 → END
//! §7.1 的 `hr_salary_flow` / `hr_adjust_resign_flow` 行（四重守卫）、§8.1 的 HR-38 行。
//!
//! # 号段为什么是 V0394 + V0395（卡面写 V0385 + V0386）
//!
//! `V0385` 被成本灰度开关实占、`V0391` 被成本流程实占、`V0392` 被 HR-23 考勤锁定守卫实占、
//! `V0393` 被 HR-26 请假流程实占。nik 迁移加载器对版本号做**跨目录全局唯一**校验，
//! 重号 = 服务启动直接失败（AGENTS 约定 6）。全局不变量（四位补零 / up 必有 down /
//! 版本号唯一）由 `tests/flow_all_role_multiversion_migration.rs` 统一覆盖，这里不重复。
//!
//! # 这层测试钉死什么
//!
//! 1. ★ up/down 成对，且两套文件都只碰本卡固定 id（改名忘配 down = 回滚做不成）。
//! 2. ★ 不许 `+no-transaction`：自检 RAISE 之后前面已执行的语句必须整份回滚。
//! 3. ★ `flow_code` / `biz_type` / 分类 / 流程名与设计逐格对得上，
//!    且 `biz_type` ↔ `FlowBizType`（`doc_kind.rs`）双向一致 —— 否则提交审批时
//!    「哪条流程能发起这类单据」对不上，单据永远 409。
//! 4. ★ `is_default` + `is_system` + `allow_auto_approve = FALSE`（免审直过是
//!    AGENTS 约定 11 的最高危分支；系统预置流程客户不能删）。
//! 5. ★ 四重守卫齐全：同名 flow_code / 同 biz_type 默认流 / 角色缺失 / 版本重复。
//! 6. ★ 版本号 `COALESCE(MAX(version_no),0)+1`；`is_current` 只在没有当前版本时置真。
//! 7. ★ `APPROVAL` 节点一律**显式 ANY**；V0395 里**没有**任何 `'ALL'::varchar`；
//!    V0394 的 `ALL` **只**出现在 COUNTERSIGN 会签节点上（角色型 APPROVAL + ALL
//!    = 一人停用即永久卡单，V0359/V0367 的 P0）。
//! 8. ★ 会签节点 `n_fin` 绑**两个角色**（财务经理 + 财务总监）+ 有兜底 + 有超时
//!    （R28 / `flow_ countersign_invariants.sh` 断言①）。
//! 9. ★ `INITIATOR_SUPERIOR` **不落枚举行**（落行等于把「谁的上级」写死，V0355 的教训）。
//! 10. ★ `graph_hash` = sha256(canonical(快照))：用仓里同一套 canonical 规则**重算**，
//!     改快照不改 hash（或反之）立刻红；三套图各一份。
//! 11. ★ 快照数组已按引擎的确定性排序（节点按 key、边按 from,to,key），
//!     否则「行重算 hash」与「快照 hash」会分叉（`flow::maintain` 每日自检报 drift）。
//! 12. ★ 条件边的类型契约：`form.gross_total_num` 必须是 **number**（gte 500000）、
//!     `form.is_key_position` 必须是 **boolean**（eq true）。用引擎求值器实证：
//!     字符串形态要么报错、要么静默 false —— 两种都会让**大额工资 / 关键岗位离职
//!     绕过上级审批且不报错**。
//! 13. down 的安全阀 + 只碰本卡 id + 自底向上删。
//!
//! 纯文件解析，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库形态与引擎校验见 `tests/hr_38_salary_adjust_resign_flow_seed_db.rs`。

use std::path::PathBuf;

use cloudpivot::business::flow::graph;
use cloudpivot::business::shared::doc_kind::FlowBizType;
use serde_json::{Map, Value};

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

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（`approval_mode = 'ALL'`、
/// `FORBID` 的死单风险等），「禁止出现」类断言必须只看真正会执行的 SQL。
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

// ============================================================================
// 三个流程的常量（与迁移里的字面量逐字对应）
// ============================================================================

const SALARY_UP: &str = "business/V0394__hr_salary_flow.up.sql";
const SALARY_DOWN: &str = "business/V0394__hr_salary_flow.down.sql";
const ADJ_UP: &str = "business/V0395__hr_adjust_resign_flow.up.sql";
const ADJ_DOWN: &str = "business/V0395__hr_adjust_resign_flow.down.sql";

const HR_MANAGER: &str = "20000000-0000-4000-8000-0000000f0016";
const FINANCE_MANAGER: &str = "20000000-0000-4000-8000-0000000f0005";
const FINANCE_DIRECTOR: &str = "20000000-0000-4000-8000-0000000f0006";
const GENERAL_MANAGER: &str = "20000000-0000-4000-8000-0000000f0003";

/// 流程固定 id：三套各一对（def / version）。
const SALARY_DEF: &str = "20000000-0000-4000-8000-0000000e1401";
const SALARY_VER: &str = "20000000-0000-4000-8000-0000000e1402";
const ADJ_DEF: &str = "20000000-0000-4000-8000-0000000e1501";
const ADJ_VER: &str = "20000000-0000-4000-8000-0000000e1502";
const RESIGN_DEF: &str = "20000000-0000-4000-8000-0000000e1531";
const RESIGN_VER: &str = "20000000-0000-4000-8000-0000000e1532";

/// 迁移里写死的 `graph_hash`（sha256 of canonical 快照）。改图必须同步改它 + 重算。
const SALARY_HASH: &str = "0a296af0477bd307f7d7687b447d50f2f813da9eeef1bbe57711b1e8a09bd5d0";
const ADJ_HASH: &str = "a413971a5a5de2bdb11798bfbfa140d927573e541cf4fd7e4f99a99dd6a99171";
const RESIGN_HASH: &str = "ec42328b209441d5ea6e403f8bfaef9b8d5dc13f5a83ec1319a36d9864558496";

/// 从 `$snapshot$…$snapshot$` 里取出**全部**发布快照（V0395 一个文件里有两份）。
fn snapshot_values(sql: &str) -> Vec<Value> {
    let tag = "$snapshot$";
    let mut out = Vec::new();
    let mut rest = sql;
    while let Some(start) = rest.find(tag) {
        let after = &rest[start + tag.len()..];
        let end = after.find(tag).unwrap_or_else(|| panic!("快照字面量没有闭合的 {tag}"));
        out.push(serde_json::from_str(&after[..end]).unwrap_or_else(|e| panic!("快照不是合法 JSON：{e}")));
        rest = &after[end + tag.len()..];
    }
    assert!(!out.is_empty(), "迁移里找不到任何 {tag} 快照字面量");
    out
}

fn canonical_hash(snapshot: &Value) -> String {
    use sha2::Digest;
    let canonical = graph::canonical_json(snapshot);
    sha2::Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

/// 取某个节点（快照数组已按 key 升序）。
fn node<'a>(snapshot: &'a Value, key: &str) -> &'a Value {
    snapshot["nodes"]
        .as_array()
        .expect("nodes 必须是数组")
        .iter()
        .find(|node| node["key"] == key)
        .unwrap_or_else(|| panic!("快照里没有节点 {key}"))
}

/// 取某个边。
fn edge<'a>(snapshot: &'a Value, key: &str) -> &'a Value {
    snapshot["edges"]
        .as_array()
        .expect("edges 必须是数组")
        .iter()
        .find(|edge| edge["key"] == key)
        .unwrap_or_else(|| panic!("快照里没有边 {key}"))
}

// ============================================================================
// ① 文件成对 / 事务性
// ============================================================================

#[test]
fn up_and_down_are_paired_for_both_migrations() {
    for (up_path, down_path, flows) in [
        (SALARY_UP, SALARY_DOWN, vec!["hr_salary"]),
        (ADJ_UP, ADJ_DOWN, vec!["hr_salary_adjust", "hr_employee_resign"]),
    ] {
        let up = read_migration(up_path);
        let down = read_migration(down_path);
        assert!(!up.trim().is_empty(), "{up_path} 不能是空文件");
        assert!(!down.trim().is_empty(), "{down_path} 不能是空文件");
        for flow in flows {
            assert_has(&up, &format!("'{flow}'"), &format!("{up_path} 必须种 {flow}"));
            // down 里流程编码可能出现在注释 / NOTICE 里（不带引号），故只断言出现
            assert!(down.contains(flow), "{down_path} 必须回滚 {flow}（找不到流程编码）");
        }
    }
    // down 只碰本卡固定 id（客户自建的同名流程不在删除范围内）
    let salary_down = read_migration(SALARY_DOWN);
    assert_has(&salary_down, SALARY_DEF, "V0394 down 只碰本卡 def id");
    assert_has(&salary_down, SALARY_VER, "V0394 down 只碰本卡 version id");
    let adj_down = read_migration(ADJ_DOWN);
    for id in [ADJ_DEF, ADJ_VER, RESIGN_DEF, RESIGN_VER] {
        assert_has(&adj_down, id, "V0395 down 只碰本卡固定 id");
    }
}

#[test]
fn migrations_are_transactional() {
    for path in [SALARY_UP, SALARY_DOWN, ADJ_UP, ADJ_DOWN] {
        let sql = read_migration(path);
        assert_lacks(
            &sql,
            "+no-transaction",
            &format!("{path}：流程配置/回滚必须整份在一个事务里（自检 RAISE 后不许留半成品）"),
        );
    }
}

// ============================================================================
// ③–④ 定义口径（含 biz_type ↔ FlowBizType 双向对拍）
// ============================================================================

#[test]
fn definitions_match_design_section_3_2_nine() {
    let salary = read_migration(SALARY_UP);
    assert_has(&salary, "'hr_salary'", "flow_code 见设计 §3.2 ⑨");
    assert_has(&salary, "'HR_SALARY'", "biz_type 见设计 §3.2 ⑨");
    assert_has(&salary, "'人力资源'", "分类：人事流程落在「人力资源」域");
    assert_has(
        &salary,
        "'薪资单审批（标准·人事 → 财务会签 → 大额总经理）'",
        "流程名（三级：人事 → 财务会签 → 大额总经理）",
    );
    assert_has(&salary, "'人事经理审核'", "第一级：人事经理（HR_MANAGER）");
    assert_has(&salary, "'财务会签'", "第二级：财务会签（COUNTERSIGN）");
    assert_has(&salary, "'应发合计判定（≥50 万）'", "条件节点：应发 ≥ 50 万");
    assert_has(&salary, "'总经理审批'", "第三级：总经理（GENERAL_MANAGER）");

    let adj = read_migration(ADJ_UP);
    assert_has(&adj, "'hr_salary_adjust'", "flow_code 见设计 §3.2 ⑨");
    assert_has(&adj, "'HR_SALARY_ADJUST'", "biz_type 见设计 §3.2 ⑨");
    assert_has(
        &adj,
        "'调薪审批（标准·上级 → 人事 → 财务三签）'",
        "流程名（三签：上级 → 人事 → 财务）",
    );
    assert_has(&adj, "'直属上级审批'", "第一级：直属上级（INITIATOR_SUPERIOR）");
    assert_has(&adj, "'财务经理审核'", "第三级：财务经理（FINANCE_MANAGER）");

    assert_has(&adj, "'hr_employee_resign'", "flow_code 见设计 §3.2 ⑨");
    assert_has(&adj, "'HR_EMPLOYEE_RESIGN'", "biz_type 见设计 §3.2 ⑨");
    assert_has(
        &adj,
        "'离职审批（标准·上级 → 人事 → 关键岗位总经理）'",
        "流程名（关键岗位才到总经理）",
    );
    assert_has(&adj, "'关键岗位判定'", "条件节点：关键岗位");
}

#[test]
fn biz_types_line_up_with_the_flow_biz_type_enum() {
    // 种子里写的 biz_type / flow_code 必须与 `doc_kind.rs::FlowBizType` 的映射逐字一致。
    // 对不上 = 提交审批时「哪条流程能发起这类单据」查不到 ⇒ 单据永远 409 + 66033。
    for (biz_type, flow_code) in [
        ("HR_SALARY", "hr_salary"),
        ("HR_SALARY_ADJUST", "hr_salary_adjust"),
        ("HR_EMPLOYEE_RESIGN", "hr_employee_resign"),
    ] {
        let kind = FlowBizType::parse(biz_type)
            .unwrap_or_else(|| panic!("FlowBizType 不认识 {biz_type}（HR-06 的 +4 是否回归？）"));
        assert_eq!(kind.as_str(), biz_type, "as_str 往返");
        assert_eq!(kind.flow_code(), flow_code, "{biz_type} 的默认流程编码");
    }
    // 三套流程都挂在 EMPLOYEE 这个对象轴上（调薪/离职没有独立单表，biz_id = employee.id）
    for biz_type in ["HR_SALARY_ADJUST", "HR_EMPLOYEE_RESIGN"] {
        let kind = FlowBizType::parse(biz_type).expect("枚举值");
        assert!(
            kind.doc_kind().is_some(),
            "{biz_type} 必须能映射到业务对象（审批轴 → 对象轴）"
        );
    }
}

#[test]
fn definitions_are_published_default_system_and_never_auto_approve() {
    let salary = sql_only(&read_migration(SALARY_UP));
    assert_has(
        &salary,
        format!("'PUBLISHED', '{SALARY_VER}'::uuid, 1, TRUE, FALSE, TRUE, NULL").as_str(),
        "V0394 def 行必须是 PUBLISHED + v1 + is_default + allow_auto_approve=FALSE + is_system",
    );
    assert_lacks(&salary, "allow_auto_approve = TRUE", "免审必须 FALSE（AGENTS 约定 11）");
    assert_lacks(&salary, "allow_auto_approve = true", "免审必须 FALSE（小写形态也拦）");

    let adj = sql_only(&read_migration(ADJ_UP));
    for ver in [ADJ_VER, RESIGN_VER] {
        assert_has(
            &adj,
            format!("'PUBLISHED', '{ver}'::uuid, 1, TRUE, FALSE, TRUE, NULL").as_str(),
            "V0395 两套 def 行都必须是 PUBLISHED + v1 + is_default + 免审 FALSE + is_system",
        );
    }
    assert_lacks(&adj, "allow_auto_approve = TRUE", "免审必须 FALSE（AGENTS 约定 11）");
    assert_lacks(&adj, "allow_auto_approve = true", "免审必须 FALSE（小写形态也拦）");
    // 超时一律只提醒（自动通过 = 静默放行工资单 / 调薪 / 离职）
    assert_lacks(&salary, "'AUTO_APPROVE'", "超时不许自动通过");
    assert_lacks(&adj, "'AUTO_APPROVE'", "超时不许自动通过");
}

// ============================================================================
// ⑤–⑥ 四重守卫与版本递增
// ============================================================================

#[test]
fn four_guards_are_present_for_every_flow() {
    let salary = sql_only(&read_migration(SALARY_UP));
    assert_has(
        &salary,
        "NOT EXISTS ( SELECT 1 FROM biz_flow_def WHERE flow_code = 'hr_salary' AND deleted_at IS NULL )",
        "守卫①：已有同名 flow_code → 整条跳过",
    );
    assert_has(
        &salary,
        "WHERE biz_type = 'HR_SALARY' AND is_default AND deleted_at IS NULL",
        "守卫②：同 biz_type 已有默认流程（uk_biz_flow_def_default）→ 整条跳过",
    );
    assert_has(
        &salary,
        "('20000000-0000-4000-8000-0000000f0016'::uuid, '20000000-0000-4000-8000-0000000f0005'::uuid",
        "守卫③必须真的点名角色（人事经理 / 财务经理 …）",
    );
    assert_has(&salary, ") = 4;", "守卫③：四个角色都在才种");
    assert_has(
        &salary,
        "x.change_note LIKE 'V0394 种子%'",
        "守卫④a：本卡版本已在 → 不重复插",
    );
    assert_has(
        &salary,
        format!("x.graph_hash = '{SALARY_HASH}'").as_str(),
        "守卫④b：同 hash 的版本已在 → 不重复发",
    );

    let adj = sql_only(&read_migration(ADJ_UP));
    for (code, biz) in [
        ("hr_salary_adjust", "HR_SALARY_ADJUST"),
        ("hr_employee_resign", "HR_EMPLOYEE_RESIGN"),
    ] {
        assert_has(
            &adj,
            format!("FROM biz_flow_def WHERE flow_code = '{code}' AND deleted_at IS NULL").as_str(),
            &format!("{code} 守卫①"),
        );
        assert_has(
            &adj,
            format!("WHERE biz_type = '{biz}' AND is_default AND deleted_at IS NULL").as_str(),
            &format!("{code} 守卫②"),
        );
    }
    assert_has(
        &adj,
        ") = 2;",
        "调薪 / 离职各要 2 个角色（人事 + 财务 / 人事 + 总经理）",
    );
    assert_has(&adj, "x.change_note LIKE 'V0395 种子%'", "守卫④a");
    assert_has(&adj, format!("x.graph_hash = '{ADJ_HASH}'").as_str(), "守卫④b（调薪）");
    assert_has(
        &adj,
        format!("x.graph_hash = '{RESIGN_HASH}'").as_str(),
        "守卫④b（离职）",
    );
}

#[test]
fn version_increments_and_never_steals_current() {
    for path in [SALARY_UP, ADJ_UP] {
        let up = sql_only(&read_migration(path));
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
}

// ============================================================================
// ⑦–⑨ 审批节点形态 / 会签 / 审批人来源
// ============================================================================

#[test]
fn approval_nodes_are_explicit_any_and_countersign_is_the_only_all() {
    // V0394：2 个 APPROVAL（人事经理 / 总经理）都显式 ANY；ALL 只出现在会签行
    let salary = sql_only(&read_migration(SALARY_UP));
    assert_eq!(
        salary.matches("'ANY'::varchar").count(),
        2,
        "薪资流程的 2 个 APPROVAL 节点都要显式 approval_mode = ANY"
    );
    assert_eq!(
        salary.matches("'ALL'::varchar").count(),
        1,
        "V0394 只允许一处 ALL（财务会签节点）"
    );
    let all_line = salary
        .lines()
        .find(|line| line.contains("'ALL'::varchar"))
        .expect("必须有一行 ALL");
    assert!(
        all_line.contains("'COUNTERSIGN'::varchar"),
        "ALL 只能落在 COUNTERSIGN 会签节点上（角色型 APPROVAL + ALL = 一人停用即永久卡单）：{all_line}"
    );
    assert_has(
        &salary,
        "AND (approval_mode = 'ALL' OR approval_mode IS NULL)",
        "§6 自检必须断言「APPROVAL 没有 ALL / 没有 NULL」",
    );

    // V0395：全是显式 ANY，一处 ALL 都不许有
    let adj = sql_only(&read_migration(ADJ_UP));
    assert_eq!(
        adj.matches("'ANY'::varchar").count(),
        6,
        "V0395 共 6 个审批节点（调薪 3 + 离职 3），都要显式 ANY"
    );
    assert_lacks(&adj, "'ALL'::varchar", "调薪 / 离职都不建会签节点，不许出现 ALL");
    assert_lacks(&adj, "'COUNTERSIGN'::varchar", "V0395 不需要会签节点");
}

#[test]
fn countersign_node_binds_two_roles_with_fallback_and_timeout() {
    let up = sql_only(&read_migration(SALARY_UP));
    // 节点行：COUNTERSIGN + ALL + 兜底 + 有超时
    let countersign_line = up
        .lines()
        .find(|line| line.contains("'n_fin'::varchar"))
        .expect("必须有财务会签节点");
    assert!(countersign_line.contains("'COUNTERSIGN'::varchar"), "n_fin 必须是会签");
    assert!(countersign_line.contains("'ALL'::varchar"), "会签必须是全员签（ALL）");
    assert!(
        countersign_line.contains("'FALLBACK_ADMIN'::varchar"),
        "会签节点没人可派必须有兜底（SKIP = 自动通过）"
    );
    assert!(
        countersign_line.contains("'REMIND'::varchar") && countersign_line.contains("48::int"),
        "会签节点必须有超时小时（R28：没有超时/兜底的 ALL = 永久卡死）"
    );
    // 审批人两行（财务经理 + 财务总监）
    assert_has(
        &up,
        format!("'ROLE'::varchar, '{FINANCE_MANAGER}'::uuid, 0::int").as_str(),
        "会签第一行：财务经理",
    );
    assert_has(
        &up,
        format!("'ROLE'::varchar, '{FINANCE_DIRECTOR}'::uuid, 1::int").as_str(),
        "会签第二行：财务总监",
    );
}

#[test]
fn roles_are_bound_and_superior_is_never_enumerated() {
    let salary = sql_only(&read_migration(SALARY_UP));
    assert_has(
        &salary,
        format!("'ROLE'::varchar, '{HR_MANAGER}'::uuid, 0::int").as_str(),
        "n_hr → HR_MANAGER（…f0016）",
    );
    assert_has(
        &salary,
        format!("'ROLE'::varchar, '{GENERAL_MANAGER}'::uuid, 0::int").as_str(),
        "n_gm → GENERAL_MANAGER（…f0003）",
    );

    let adj = sql_only(&read_migration(ADJ_UP));
    assert_has(
        &adj,
        "'INITIATOR_SUPERIOR'::varchar, 1::int",
        "直属上级必须 INITIATOR_SUPERIOR + superior_levels=1（随发起人变化）",
    );
    // 两套流程的审批人绑定块里都不许出现 INITIATOR_SUPERIOR 枚举行
    let mut blocks = 0;
    for block in adj.split("INSERT INTO biz_flow_node_assignee").skip(1) {
        blocks += 1;
        let head: String = block.lines().take(12).collect::<Vec<_>>().join("\n");
        assert!(
            !head.contains("'INITIATOR_SUPERIOR'"),
            "n_sup 不许落审批人枚举行（落行 = 把「谁的上级」写死）：{head}"
        );
    }
    assert_eq!(blocks, 2, "V0395 应有两段审批人绑定插入（调薪 / 离职各一段）");
    // 每一段的角色行数：调薪 2（人事 + 财务）、离职 2（人事 + 总经理）
    let role_rows = adj
        .matches("'ROLE'::varchar, '20000000-0000-4000-8000-0000000f")
        .count();
    assert_eq!(role_rows, 4, "V0395 共 4 行角色绑定（调薪 2 + 离职 2）");
}

// ============================================================================
// ⑩ hash / 排序 / 拓扑
// ============================================================================

#[test]
fn graph_hashes_match_the_snapshots_they_ship_with() {
    let salary_up = read_migration(SALARY_UP);
    let salary_snaps = snapshot_values(&salary_up);
    assert_eq!(salary_snaps.len(), 1, "V0394 只有一份快照");
    assert_eq!(
        canonical_hash(&salary_snaps[0]),
        SALARY_HASH,
        "V0394 快照重算的 hash 必须等于常量（改快照不改 hash = flow::maintain 报 drift）"
    );
    assert_eq!(
        salary_up.matches(SALARY_HASH).count(),
        5,
        "薪资流程 hash 应出现 5 次：文件头注释 1 + §3 注释 1 + 列值 1 + 守卫④b 1 + §6 自检常量 1"
    );

    let adj_up = read_migration(ADJ_UP);
    let adj_snaps = snapshot_values(&adj_up);
    assert_eq!(adj_snaps.len(), 2, "V0395 有两份快照（调薪 / 离职）");
    // 快照按文件里的出现顺序：调薪在前、离职在后
    assert_eq!(
        canonical_hash(&adj_snaps[0]),
        ADJ_HASH,
        "调薪快照重算的 hash 必须等于常量"
    );
    assert_eq!(
        canonical_hash(&adj_snaps[1]),
        RESIGN_HASH,
        "离职快照重算的 hash 必须等于常量"
    );
    assert_eq!(
        adj_up.matches(ADJ_HASH).count(),
        4,
        "调薪 hash 应出现 4 次（注释 / 列值 / 守卫 / 自检）"
    );
    assert_eq!(
        adj_up.matches(RESIGN_HASH).count(),
        4,
        "离职 hash 应出现 4 次（注释 / 列值 / 守卫 / 自检）"
    );
}

#[test]
fn snapshot_arrays_are_in_engine_deterministic_order() {
    let salary = snapshot_values(&read_migration(SALARY_UP));
    let adj = snapshot_values(&read_migration(ADJ_UP));
    for (label, snapshot) in [
        ("hr_salary", &salary[0]),
        ("hr_salary_adjust", &adj[0]),
        ("hr_employee_resign", &adj[1]),
    ] {
        let nodes = snapshot["nodes"].as_array().expect("nodes");
        let keys: Vec<&str> = nodes.iter().map(|n| n["key"].as_str().expect("node.key")).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "{label} 快照 nodes 必须按 key 升序");

        let edges = snapshot["edges"].as_array().expect("edges");
        let order: Vec<(&str, &str, &str)> = edges
            .iter()
            .map(|e| {
                (
                    e["from"].as_str().expect("edge.from"),
                    e["to"].as_str().expect("edge.to"),
                    e["key"].as_str().expect("edge.key"),
                )
            })
            .collect();
        let mut sorted_order = order.clone();
        sorted_order.sort();
        assert_eq!(order, sorted_order, "{label} 快照 edges 必须按 (from,to,key) 升序");
    }
}

#[test]
fn topologies_match_design() {
    let salary = snapshot_values(&read_migration(SALARY_UP));
    let s = &salary[0];
    let types: Vec<&str> = s["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .map(|n| n["node_type"].as_str().expect("node_type"))
        .collect();
    assert_eq!(
        types,
        vec!["CONDITION", "END", "COUNTERSIGN", "APPROVAL", "APPROVAL", "START"],
        "hr_salary：6 节点（按 key 升序）"
    );
    let chain: Vec<(&str, &str, &str)> = s["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .map(|e| {
            (
                e["from"].as_str().unwrap(),
                e["to"].as_str().unwrap(),
                e["key"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        chain,
        vec![
            ("c_amt", "e", "e_small"),
            ("c_amt", "n_gm", "e_big"),
            ("n_fin", "c_amt", "e_fin_amt"),
            ("n_gm", "e", "e_gm_end"),
            ("n_hr", "n_fin", "e_hr_fin"),
            ("s", "n_hr", "e_start"),
        ],
        "hr_salary：s → n_hr → n_fin → c_amt →（≥50 万）n_gm → e"
    );
    assert_eq!(
        node(s, "n_fin")["assignees"].as_array().expect("assignees").len(),
        2,
        "财务会签的候选人是两个角色（不是一个角色的全部成员）"
    );
    assert_eq!(node(s, "n_fin")["approval_mode"], "ALL", "会签 = 全员签");
    assert!(node(s, "n_fin")["timeout_hours"].is_number(), "会签必须有超时小时");

    let adj = snapshot_values(&read_migration(ADJ_UP));
    let a = &adj[0];
    assert_eq!(
        a["nodes"].as_array().expect("nodes").len(),
        5,
        "hr_salary_adjust：5 节点"
    );
    assert_eq!(a["edges"].as_array().expect("edges").len(), 4, "hr_salary_adjust：4 边");
    assert_eq!(
        node(a, "n_sup")["assignee_type"],
        "INITIATOR_SUPERIOR",
        "调薪第一级是直属上级"
    );
    assert!(
        node(a, "n_sup")["assignees"].as_array().expect("assignees").is_empty(),
        "直属上级是不可枚举来源，快照里不能有候选人行"
    );

    let r = &adj[1];
    assert_eq!(
        r["nodes"].as_array().expect("nodes").len(),
        6,
        "hr_employee_resign：6 节点"
    );
    assert_eq!(
        r["edges"].as_array().expect("edges").len(),
        6,
        "hr_employee_resign：6 边"
    );
    assert_eq!(node(r, "c_key")["node_type"], "CONDITION", "离职有「关键岗位」判定节点");
    let defaults: Vec<&Value> = r["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .filter(|e| e["is_default"] == true)
        .collect();
    assert_eq!(defaults.len(), 1, "条件节点必须恰有 1 条兜底边（R13）");
    assert_eq!(defaults[0]["key"], "e_plain", "兜底边 = 非关键岗位 → 直接结束");
}

// ============================================================================
// ⑫ 条件边的类型契约（用引擎求值器实证）
// ============================================================================

#[test]
fn salary_amount_condition_needs_a_json_number() {
    let snaps = snapshot_values(&read_migration(SALARY_UP));
    let condition_raw = edge(&snaps[0], "e_big")["condition"].clone();
    assert!(!condition_raw.is_null(), "e_big 必须带条件（R13：非兜底边没有条件）");
    assert_eq!(condition_raw["field"], "form.gross_total_num", "条件字段必须是数值镜像");
    assert_eq!(condition_raw["cmp"], "gte", "≥ 50 万用 gte（恰好 50 万也要总经理）");
    assert_eq!(condition_raw["value"], 500000, "阈值 50 万（设计 §3.2 ⑨）");
    assert_eq!(edge(&snaps[0], "e_big")["is_default"], false, "条件边不能是兜底边");
    assert!(
        graph::field_allowed("form.gross_total_num"),
        "form.* 白名单必须接受 gross_total_num"
    );
    let condition = graph::parse_condition(&condition_raw).expect("条件 DSL 必须合法（否则提交审批 500）");

    for (amount, expected) in [
        (0.0_f64, false),
        (999.99, false),
        (499_999.99, false),
        (500_000.0, true),
        (800_000.0, true),
    ] {
        let mut form = Map::new();
        form.insert(
            "gross_total_num".to_string(),
            serde_json::Number::from_f64(amount).map(Value::Number).unwrap(),
        );
        let context = graph::EvalContext {
            form,
            ..Default::default()
        };
        let hit = graph::eval(&condition, &context).unwrap_or_else(|e| panic!("{amount} 求值失败：{e}"));
        assert_eq!(hit, expected, "应发 {amount} → 走总经理审批应为 {expected}");
    }

    // 反证：字符串形态（`gross_total = "800000.0000"` 这种展示口径）会让求值器报错，
    // 引擎随后按「条件不成立」静默走兜底边 ⇒ 80 万的工资单绕过总经理审批且不报错。
    let mut form = Map::new();
    form.insert("gross_total".to_string(), Value::String("800000.0000".to_string()));
    let context = graph::EvalContext {
        form,
        ..Default::default()
    };
    let error = graph::eval(&condition, &context).expect_err("只给字符串字段时求值必须报 Missing，不能静默 false");
    println!("字符串口径的求值错误（预期，说明为什么必须有数值镜像）：{error}");
}

#[test]
fn key_position_condition_needs_a_json_boolean() {
    let snaps = snapshot_values(&read_migration(ADJ_UP));
    let condition_raw = edge(&snaps[1], "e_key")["condition"].clone();
    assert!(!condition_raw.is_null(), "e_key 必须带条件");
    assert_eq!(condition_raw["field"], "form.is_key_position", "条件字段");
    assert_eq!(condition_raw["cmp"], "eq", "布尔判定用 eq");
    assert_eq!(condition_raw["value"], true, "关键岗位 = true");
    let condition = graph::parse_condition(&condition_raw).expect("条件 DSL 必须合法");

    for (flag, expected) in [(true, true), (false, false)] {
        let mut form = Map::new();
        form.insert("is_key_position".to_string(), Value::Bool(flag));
        let context = graph::EvalContext {
            form,
            ..Default::default()
        };
        assert_eq!(
            graph::eval(&condition, &context).expect("布尔求值不应报错"),
            expected,
            "is_key_position={flag} → 总经理审批应为 {expected}"
        );
    }

    // 反证：字符串 "true" 与布尔 true **不相等**（json_eq 只在两侧都是数字时做数值比较），
    // 于是求值返回 false —— 不是报错，是**静默**跳到兜底边（关键岗位离职没有总经理签字）。
    let mut form = Map::new();
    form.insert("is_key_position".to_string(), Value::String("true".to_string()));
    let context = graph::EvalContext {
        form,
        ..Default::default()
    };
    assert!(
        !graph::eval(&condition, &context).expect("字符串不报错，只是不相等"),
        "字符串 \"true\" 不等于布尔 true ⇒ 静默跳过总经理审批（这就是类型契约必须钉死的原因）"
    );
}

// ============================================================================
// ⑬ down：安全阀 + 删除顺序
// ============================================================================

#[test]
fn down_has_safety_valves_and_deletes_bottom_up() {
    let salary = read_migration(SALARY_DOWN);
    assert_has(
        &salary,
        "FROM biz_flow_instance WHERE biz_type = ''HR_SALARY''",
        "安全阀：已有 HR_SALARY 审批实例 → 拒绝回滚（version_id 是 RESTRICT 外键）",
    );
    assert_has(
        &salary,
        "FROM biz_salary_sheet WHERE approval_instance_id IS NOT NULL",
        "安全阀：薪资单已挂实例 → 拒绝回滚（链路已被用过）",
    );
    assert_has(
        &salary,
        "FROM biz_salary_sheet WHERE status IN (''POSTED'', ''REVERSED'')",
        "安全阀：已过账 / 已红冲的工资单（凭证已生成、钱已动）→ 拒绝回滚",
    );
    assert_has(
        &salary,
        "FROM biz_flow_def_version WHERE def_id = $1 AND id <> $2",
        "安全阀：本流程下还有别的版本 → 拒绝回滚",
    );

    let adj = read_migration(ADJ_DOWN);
    assert_has(
        &adj,
        "WHERE biz_type IN (''HR_SALARY_ADJUST'', ''HR_EMPLOYEE_RESIGN'')",
        "安全阀：两套流程的审批实例都查",
    );
    assert_has(
        &adj,
        "WHERE def_id = $1 AND id <> $2",
        "安全阀：两套流程各自还有别的版本都拒绝回滚",
    );

    for (down, version) in [(&salary, SALARY_VER), (&adj, ADJ_VER)] {
        let assignee_pos = down.find("DELETE FROM biz_flow_node_assignee").expect("删审批人绑定");
        let edge_pos = down.find("DELETE FROM biz_flow_edge").expect("删边");
        let node_pos = down.find("DELETE FROM biz_flow_node\n").expect("删节点");
        let version_pos = down.find("DELETE FROM biz_flow_def_version").expect("删版本");
        let def_pos = down.rfind("DELETE FROM biz_flow_def\n").expect("删定义");
        assert!(
            assignee_pos < edge_pos && edge_pos < node_pos && node_pos < version_pos && version_pos < def_pos,
            "down 必须自底向上删（审批人 → 边 → 节点 → 版本 → 定义），否则被外键挡"
        );
        assert_has(down, "current_version_id = NULL", "删版本前必须摘掉当前版本指针");
        assert_has(down, version, "down 必须点到本卡版本 id");
    }
}
