//! `hr_leave` 正式种子（V0393）的**借出 → 归还**守卫（`#[cfg(test)]`）。
//!
//! # 为什么需要它（卡 `t_85e005d9`）
//!
//! `uk_biz_flow_def_code` 是 `flow_code` **全局唯一**的部分唯一索引（`V0120`：`WHERE
//! deleted_at IS NULL`），同一时刻库里只许有一条 `hr_leave` 定义。而 HR-25 / HR-27 两套真库
//! 夹具必须把审批人**写死成自己的夹具账号**（否则「待办派给谁」取决于部门负责人现状，
//! 用例会随环境漂移），所以它们历来是「先删掉库里那条 `hr_leave`，再插自己那条」。
//!
//! 问题是那条被删的，正好是 **V0393 种下的正式种子**。旧写法删完就不管了 ⇒
//!
//! ```text
//! 跑过全量套件的库：SELECT count(*) FROM biz_flow_def WHERE flow_code='hr_leave';  -- 0
//! ```
//!
//! 种子的 5 个节点行 / 5 条边 / 审批角色绑定（`n_hr → HR_MANAGER …f0016`）/ 快照 hash 永久
//! 丢失，而这个库后面还要被别的用例当作「有正式流程的库」使用（`leave_handler::submit_leave`
//! 走 `published_leave_flow()`，`stockin` 的 p16 全库自检要扫所有已发布版本）。
//!
//! # 做法：抓成 JSON 借出，收尾原样写回（不改迁移、不改生产代码）
//!
//! 1. **借出**（[`borrow`]）：第一次调用时把种子的五张表整链抓成 `jsonb`
//!    （`SELECT to_jsonb(t) …`），进程内只抓一次；随后从库里删掉整链，让夹具占住编码。
//! 2. **归还**（[`restore`]）：模块收尾（各文件的 `zz_teardown_purges_fixtures`）用
//!    `jsonb_populate_recordset(NULL::<table>, …)` 逐表原样写回 —— **id / 时间戳 / 快照 /
//!    hash / 节点行 / 审批人绑定全部照旧**，与 V0393 种出来的一模一样。
//! 3. **核对**（[`shape`] / [`assert_intact`]）：归还后按迁移里的字面量逐项对账
//!    （定义 id、`PUBLISHED`+`is_default`+`is_system`、当前版本指针、`graph_hash` 非零且等于
//!    种子值、`change_note` 与迁移一致、5 节点 / 5 边 / 1 条审批人绑定）。
//!
//! 为什么不走「保留种子定义 + 只换版本」：`ck_biz_flow_def_version_current_published`
//! 只许 `PUBLISHED` 版本挂 `is_current`，而手写夹具版本（`graph_hash = repeat('0',64)`、
//! 没有 L1 行）一旦是 `PUBLISHED`，`flow::maintain` 的全库三方对账就会报 `RowsEdited`
//! （`stockin` p16 那条用例，t_9c3551b0 实测第二轮红）。要绕开它就得同时写真 hash + L1 行，
//! 那等于把「发布事务」在测试里重写一遍，比抓 JSON 更容易漂。
//!
//! # 口径
//!
//! - 备份是**进程内静态**（[`BACKUP`]）：`--test-threads=1` 时两个模块共用同一份；一个模块
//!   收尾归还后，下一个模块再借出时不重新抓（行是刚写回去的原样，重抓也一样）。
//! - 种子在本进程首次借出时就**已经不在**（库被旧版夹具污染过）⇒ 归还后 [`assert_intact`]
//!   会红，并直接点名「用 `scripts/verify/make_full_suite_db.sh` 重建演练库」。
//!   静默跳过就会变成假绿 —— 那正是这张卡要消灭的东西。
//! - 只碰 `hr_leave` + 种子 def id / 夹具 `change_note` 认领得到的行；别的流程一行不碰。

#![cfg(test)]

use std::sync::Mutex;

use nik::prelude::*;
use nik::uuid::Uuid;

/// 正式种子的 `flow_code`（`leave_handler::FLOW_CODE` / `leave_bridge::FLOW_CODE` 同值）。
pub(crate) const SEED_FLOW_CODE: &str = "hr_leave";
/// 正式种子的流程定义 id（V0393 §1.2 / §2 写死的固定 id）。
pub(crate) const SEED_DEF_ID: &str = "20000000-0000-4000-8000-0000000e0393";
/// 正式种子的版本 id（V0393 §3 写死的固定 id）。
pub(crate) const SEED_VERSION_ID: &str = "20000000-0000-4000-8000-0000000e0394";
/// 正式种子版本的 `graph_hash`（V0393 §3 / §6.4 的字面量）。
pub(crate) const SEED_GRAPH_HASH: &str = "4fc6722abf629a8a9a84580942dd2f6a0a44431786b2e500905531affc0e1f97";
/// 正式种子版本的 `change_note`（V0393 §3 的字面量）。
pub(crate) const SEED_CHANGE_NOTE: &str = "V0393 种子：请假审批（直属上级 →（≥3 天）人事经理）默认流程";
/// 正式种子的流程名（V0393 §2 的字面量）。
pub(crate) const SEED_FLOW_NAME: &str = "请假审批（标准·直属上级 → 长假人事复核）";
const SEED_BIZ_TYPE: &str = "HR_LEAVE";
const SEED_NODE_COUNT: i64 = 5;
const SEED_EDGE_COUNT: i64 = 5;

fn seed_def_uuid() -> Uuid {
    Uuid::parse_str(SEED_DEF_ID).expect("种子定义 id 是合法 uuid")
}

/// 种子五张表的备份（`jsonb_agg` 出来的数组文本；`[]` = 那一层是空的）。
#[derive(Debug, Clone, Default)]
pub(crate) struct SeedBackup {
    defs: String,
    versions: String,
    nodes: String,
    edges: String,
    assignees: String,
}

/// 进程内唯一的备份槽：`--test-threads=1` 时两个 leave 模块共用。
static BACKUP: Mutex<Option<SeedBackup>> = Mutex::new(None);

/// 备份是否已抓过（判断「种子原本在不在」不看这个，看库）。
fn backup_taken() -> bool {
    BACKUP.lock().expect("种子备份锁").is_some()
}

fn take_backup_slot() -> Option<SeedBackup> {
    BACKUP.lock().expect("种子备份锁").clone()
}

fn store_backup(backup: SeedBackup) {
    let mut slot = BACKUP.lock().expect("种子备份锁");
    if slot.is_none() {
        *slot = Some(backup);
    }
}

// ==================== 借出 ====================

/// 「借出」正式种子：抓备份（进程内首次）→ 从库里删掉整链。
///
/// 返回 `true` = 种子原本在库、这次真的借出来了；`false` = 借出前种子就不在（库已被旧版
/// 夹具污染，收尾时 [`assert_intact`] 会报红点名重建库）。
///
/// 种子**还挂着审批实例**时直接 panic：那说明上一个用例没清场（`biz_flow_instance.def_id`
/// 是 `ON DELETE RESTRICT`），硬删会炸在 FK 上；这里提前说清楚比让 `INSERT` 撞
/// `uk_biz_flow_def_code` 好读得多。
pub(crate) async fn borrow() -> bool {
    if !seed_present().await {
        if !backup_taken() {
            println!(
                "!! 警告：借出前库里就没有 hr_leave 正式种子（V0393）—— 这个库被旧版夹具污染过；\
                 收尾会报红，请用 scripts/verify/make_full_suite_db.sh 重建演练库"
            );
        }
        return false;
    }
    if !backup_taken() {
        let backup = capture().await;
        store_backup(backup);
    }
    delete_seed_chain().await;
    true
}

/// 库里是否有未软删的 `hr_leave` 定义（任意 id）。
async fn seed_present() -> bool {
    count_hr_leave_defs().await > 0
}

async fn count_hr_leave_defs() -> i64 {
    db().scalar_with_params::<Option<i64>>(
        "SELECT count(*) FROM biz_flow_def WHERE flow_code = ? AND deleted_at IS NULL",
        vec![Value::Text(SEED_FLOW_CODE.to_string())],
    )
    .await
    .expect("查 hr_leave 定义数")
    .unwrap_or(0)
}

/// 把种子整链抓成 JSON（五张表，顺序无关，写回时按 FK 顺序）。
async fn capture() -> SeedBackup {
    let def_id = seed_def_uuid();
    SeedBackup {
        defs: capture_rows("biz_flow_def t WHERE t.id = ?::uuid", &def_id).await,
        versions: capture_rows("biz_flow_def_version t WHERE t.def_id = ?::uuid", &def_id).await,
        nodes: capture_rows(
            "biz_flow_node t WHERE t.version_id IN \
             (SELECT v.id FROM biz_flow_def_version v WHERE v.def_id = ?::uuid)",
            &def_id,
        )
        .await,
        edges: capture_rows(
            "biz_flow_edge t WHERE t.version_id IN \
             (SELECT v.id FROM biz_flow_def_version v WHERE v.def_id = ?::uuid)",
            &def_id,
        )
        .await,
        assignees: capture_rows(
            "biz_flow_node_assignee t WHERE t.node_id IN \
             (SELECT n.id FROM biz_flow_node n \
                JOIN biz_flow_def_version v ON v.id = n.version_id \
               WHERE v.def_id = ?::uuid)",
            &def_id,
        )
        .await,
    }
}

/// `SELECT COALESCE(jsonb_agg(to_jsonb(t)), '[]'::jsonb)::text FROM <from_clause> t`。
///
/// `to_jsonb(行)` 的键就是列名，写回时 `jsonb_populate_recordset` 按列名对位 ⇒ 迁移加列 /
/// 调列序都不用改这里（比手抄列清单抗漂）。
async fn capture_rows(from_clause: &str, def_id: &Uuid) -> String {
    let sql = format!("SELECT COALESCE(jsonb_agg(to_jsonb(t)), '[]'::jsonb)::text AS payload FROM {from_clause}");
    db().scalar_with_params::<Option<String>>(&sql, vec![Value::from(def_id)])
        .await
        .unwrap_or_else(|error| panic!("抓种子备份失败（{from_clause}）：{error}"))
        .unwrap_or_else(|| "[]".to_string())
}

/// 删掉种子的整链（实例 → 待办 → 实例节点 → 边 / 节点 / 审批人 → 版本 → 定义）。
///
/// 与两套夹具的 `purge_fixture_flows` 同序（`t_9c3551b0` 的教训：先抓 id 再逐层删，
/// 不要每一步都用「JOIN 版本」的子查询）。
async fn delete_seed_chain() {
    let def_id = seed_def_uuid();
    let instances = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_flow_instance WHERE def_id = ?::uuid",
            vec![Value::from(&def_id)],
        )
        .await
        .expect("查种子上的审批实例")
        .unwrap_or(0);
    assert_eq!(
        instances, 0,
        "hr_leave 正式种子（V0393 …e0393）上还挂着 {instances} 个审批实例 ⇒ 上一个用例没清场\
         （biz_flow_instance.def_id 是 ON DELETE RESTRICT）。请先修那个用例的收尾；\
         若确认只是上一次失败留下的测试残留，重造演练库（scripts/verify/make_full_suite_db.sh）即可"
    );
    for sql in [
        "DELETE FROM biz_flow_cc WHERE instance_id IN \
         (SELECT id FROM biz_flow_instance WHERE def_id = ?::uuid)",
        "DELETE FROM biz_task WHERE flow_instance_id IN \
         (SELECT id FROM biz_flow_instance WHERE def_id = ?::uuid)",
        "DELETE FROM biz_flow_task WHERE instance_id IN \
         (SELECT id FROM biz_flow_instance WHERE def_id = ?::uuid)",
        "DELETE FROM biz_flow_instance_node WHERE instance_id IN \
         (SELECT id FROM biz_flow_instance WHERE def_id = ?::uuid)",
        "DELETE FROM biz_flow_instance WHERE def_id = ?::uuid",
        "DELETE FROM biz_flow_node_assignee WHERE node_id IN \
         (SELECT id FROM biz_flow_node WHERE version_id IN \
            (SELECT id FROM biz_flow_def_version WHERE def_id = ?::uuid))",
        "DELETE FROM biz_flow_edge WHERE version_id IN \
         (SELECT id FROM biz_flow_def_version WHERE def_id = ?::uuid)",
        "DELETE FROM biz_flow_node WHERE version_id IN \
         (SELECT id FROM biz_flow_def_version WHERE def_id = ?::uuid)",
        "DELETE FROM biz_flow_def_version WHERE def_id = ?::uuid",
        "DELETE FROM biz_flow_def WHERE id = ?::uuid",
    ] {
        db().exec_sql(sql.to_string(), vec![Value::from(&def_id)])
            .await
            .unwrap_or_else(|error| panic!("借出种子时删除失败（{sql}）：{error}"));
    }
}

/// 删掉「夹具内建、且已经没人引用」的 `hr_leave` 定义（两套夹具共用一份实现）。
///
/// 认领口径：版本 `change_note = note`（本文件自己的夹具），或**一个版本都没有**（半删残留）。
/// 删完若还剩下「不是本夹具 / 也不是种子」的 `hr_leave` 定义，**直接 panic** ——
/// 那种库占着 `flow_code` 编码，夹具硬插会撞 `uk_biz_flow_def_code`，报「duplicate key」
/// 会把人引到错误的方向（真因是库里有一条别人家的流程）。
///
/// 返回删掉的定义条数（打证据用）。
pub(crate) async fn purge_fixture_defs(note: &str) -> i64 {
    // 先抓 id 再逐层删（见 `t_9c3551b0` 的踩坑：删完版本后子查询就 JOIN 不出定义了）
    let rows = db()
        .find_with_params(
            "SELECT d.id::text AS id FROM biz_flow_def d \
              WHERE d.flow_code = ? \
                AND (NOT EXISTS (SELECT 1 FROM biz_flow_def_version v WHERE v.def_id = d.id) \
                     OR EXISTS (SELECT 1 FROM biz_flow_def_version v \
                                 WHERE v.def_id = d.id AND v.change_note = ?))",
            vec![Value::Text(SEED_FLOW_CODE.to_string()), Value::Text(note.to_string())],
        )
        .await
        .expect("查夹具流程定义");
    let ids: Vec<Uuid> = rows
        .iter()
        .map(|row| Uuid::parse_str(&row.get_typed::<String>("id").expect("流程 id")).expect("流程 id 是 uuid"))
        .collect();
    for id in &ids {
        for sql in [
            "DELETE FROM biz_flow_edge WHERE version_id IN \
             (SELECT id FROM biz_flow_def_version WHERE def_id = ?::uuid)",
            "DELETE FROM biz_flow_node WHERE version_id IN \
             (SELECT id FROM biz_flow_def_version WHERE def_id = ?::uuid)",
            "DELETE FROM biz_flow_def_version WHERE def_id = ?::uuid",
            "DELETE FROM biz_flow_def WHERE id = ?::uuid",
        ] {
            db().exec_sql(sql.to_string(), vec![Value::from(id)])
                .await
                .unwrap_or_else(|error| panic!("清夹具流程定义失败（{sql}）：{error}"));
        }
    }
    // 剩下的 hr_leave 定义：只能是没有版本的空壳以外的「别人家的」
    let leftovers = db()
        .find_with_params(
            "SELECT d.id::text AS id, d.flow_name, d.status, \
                    (SELECT count(*) FROM biz_flow_def_version v WHERE v.def_id = d.id) AS versions \
               FROM biz_flow_def d \
              WHERE d.flow_code = ? AND d.deleted_at IS NULL",
            vec![Value::Text(SEED_FLOW_CODE.to_string())],
        )
        .await
        .expect("查 hr_leave 残留定义");
    for row in &leftovers {
        let id: String = row.get_typed("id").expect("id");
        let name: String = row.get_typed("flow_name").expect("flow_name");
        let status: String = row.get_typed("status").expect("status");
        let versions: i64 = row.get_typed("versions").expect("versions");
        // 种子定义还留着是允许的（本进程没借出 / 已归还）：它不是夹具，不该被删
        if id == SEED_DEF_ID {
            continue;
        }
        panic!(
            "库里有一条不是本夹具的 hr_leave 流程定义（id={id} / {name} / {status} / 版本 {versions} 个）：\
             它占着 flow_code=hr_leave 编码，夹具插不进去。请人工确认这条定义该不该在测试库上"
        );
    }
    ids.len() as i64
}

// ==================== 归还 ====================

/// 把借出的种子原样写回，并按迁移字面量核对形态；任何一项不符直接 panic。
///
/// 幂等：库里已经有同 id 的种子（上一轮已归还 / 本进程没借出）就只核对不写回。
pub(crate) async fn restore() -> SeedShape {
    let Some(backup) = take_backup_slot() else {
        // 本进程没借出过：要么没跑过夹具用例，要么种子从一开始就不在。
        let shape = shape().await;
        shape.assert_intact();
        return shape;
    };
    if count_hr_leave_defs().await == 0 {
        // 没有 hr_leave 定义 ⇒ 直接原样写回；有「别人家的」定义 ⇒ 让下面 insert 撞索引之前
        // 先给出人话（这里只判「完全没有」，别的情形由 upsert 的报错 + 形态核对兜住）
        insert_payload("biz_flow_def", &backup.defs).await;
        insert_payload("biz_flow_def_version", &backup.versions).await;
        insert_payload("biz_flow_node", &backup.nodes).await;
        insert_payload("biz_flow_edge", &backup.edges).await;
        insert_payload("biz_flow_node_assignee", &backup.assignees).await;
    }
    let shape = shape().await;
    shape.assert_intact();
    shape
}

/// 逐表写回（`SELECT * FROM jsonb_populate_recordset` 保证列名对位、不手抄列清单）。
async fn insert_payload(table: &str, payload: &str) {
    let payload = payload.trim();
    if payload.is_empty() || payload == "[]" {
        return;
    }
    let sql = format!("INSERT INTO {table} SELECT * FROM jsonb_populate_recordset(NULL::{table}, ?::text::jsonb)");
    db().exec_sql(sql, vec![Value::Text(payload.to_string())])
        .await
        .unwrap_or_else(|error| panic!("归还种子失败（{table}）：{error}"));
}

// ==================== 形态核对 ====================

/// 种子的形态快照（`describe()` 打证据，`assert_intact()` 下判据）。
#[derive(Debug, Clone)]
pub(crate) struct SeedShape {
    /// 未软删的 `hr_leave` 定义条数（期望 1）。
    pub defs: i64,
    pub def_id: Option<String>,
    pub flow_name: Option<String>,
    pub status: Option<String>,
    pub biz_type: Option<String>,
    pub is_default: bool,
    pub is_system: bool,
    pub allow_auto_approve: bool,
    pub current_version_id: Option<String>,
    pub current_version_no: Option<i32>,
    pub version_id: Option<String>,
    pub version_status: Option<String>,
    pub is_current: bool,
    pub graph_hash: Option<String>,
    pub change_note: Option<String>,
    pub node_count: Option<i32>,
    pub edge_count: Option<i32>,
    pub node_rows: i64,
    pub edge_rows: i64,
    pub assignee_rows: i64,
}

impl SeedShape {
    /// 一行摘要（`--nocapture` 时打出来当证据）。
    pub(crate) fn describe(&self) -> String {
        format!(
            "hr_leave 定义 {} 条（id={} name={:?} status={:?} default={} system={} auto_approve={}）\
             当前版本={:?}/{:?}（{} status={:?} current={} hash={:?} note={:?} 计数 {}·{}）\
             行 {} 节点 / {} 边 / {} 审批人绑定",
            self.defs,
            self.def_id.as_deref().unwrap_or("<无>"),
            self.flow_name,
            self.status,
            self.is_default,
            self.is_system,
            self.allow_auto_approve,
            self.current_version_id,
            self.current_version_no,
            self.version_id.as_deref().unwrap_or("<无>"),
            self.version_status,
            self.is_current,
            self.graph_hash,
            self.change_note,
            self.node_count.unwrap_or(-1),
            self.edge_count.unwrap_or(-1),
            self.node_rows,
            self.edge_rows,
            self.assignee_rows,
        )
    }

    /// 按 V0393 的字面量逐项对账；不符就 panic（消息里带实际值，排障不用再查库）。
    pub(crate) fn assert_intact(&self) {
        assert_eq!(
            self.defs,
            1,
            "hr_leave 正式种子（V0393）必须**恰好 1 条**未删定义，实际 {} 条（{}）：跑完夹具后种子\
             没回来就是这张卡的 bug；种子从头就不在 ⇒ 用 scripts/verify/make_full_suite_db.sh 重建演练库",
            self.defs,
            self.describe()
        );
        assert_eq!(
            self.def_id.as_deref(),
            Some(SEED_DEF_ID),
            "hr_leave 定义不是 V0393 的固定 id（实际 {:?}）：{}",
            self.def_id,
            self.describe()
        );
        assert_eq!(
            self.flow_name.as_deref(),
            Some(SEED_FLOW_NAME),
            "种子流程名被改过（实际 {:?}）",
            self.flow_name
        );
        assert_eq!(
            self.status.as_deref(),
            Some("PUBLISHED"),
            "种子必须是 PUBLISHED（否则 submit 会 fail-closed）"
        );
        assert_eq!(self.biz_type.as_deref(), Some(SEED_BIZ_TYPE), "种子 biz_type");
        assert!(
            self.is_default,
            "种子必须是 HR_LEAVE 的默认流程（uk_biz_flow_def_default）"
        );
        assert!(self.is_system, "种子是预置流程（is_system = TRUE）");
        assert!(
            !self.allow_auto_approve,
            "请假不许免审直过（allow_auto_approve = FALSE）"
        );
        assert_eq!(
            self.current_version_id.as_deref(),
            Some(SEED_VERSION_ID),
            "种子当前版本指针必须指回 V0393 那一版（实际 {:?}）",
            self.current_version_id
        );
        assert_eq!(self.current_version_no, Some(1), "种子当前版本号 = 1");
        assert_eq!(
            self.version_id.as_deref(),
            Some(SEED_VERSION_ID),
            "版本行必须是 V0393 的固定 id"
        );
        assert_eq!(self.version_status.as_deref(), Some("PUBLISHED"), "种子版本状态");
        assert!(self.is_current, "种子版本必须 is_current");
        assert_eq!(
            self.graph_hash.as_deref(),
            Some(SEED_GRAPH_HASH),
            "种子 graph_hash 必须与迁移字面量一致（实际 {:?}）：hash 丢了/被清零，\
             flow::maintain 的全库对账会报 SnapshotEdited",
            self.graph_hash
        );
        assert!(
            !self.graph_hash.as_deref().unwrap_or_default().chars().all(|c| c == '0'),
            "种子 graph_hash 不许是全 0（手写夹具才那样）"
        );
        assert_eq!(
            self.change_note.as_deref(),
            Some(SEED_CHANGE_NOTE),
            "种子版本的 change_note 必须与迁移一致（实际 {:?}）",
            self.change_note
        );
        assert_eq!(self.node_count, Some(SEED_NODE_COUNT as i32), "版本行 node_count = 5");
        assert_eq!(self.edge_count, Some(SEED_EDGE_COUNT as i32), "版本行 edge_count = 5");
        assert_eq!(self.node_rows, SEED_NODE_COUNT, "biz_flow_node 行数");
        assert_eq!(self.edge_rows, SEED_EDGE_COUNT, "biz_flow_edge 行数");
        assert_eq!(
            self.assignee_rows, 1,
            "biz_flow_node_assignee 行数（n_hr → HR_MANAGER）"
        );
    }
}

/// 读形态（只读，不写库）。
pub(crate) async fn shape() -> SeedShape {
    let defs = count_hr_leave_defs().await;
    let row = db()
        .query_first_sql(
            "SELECT d.id::text AS def_id, d.flow_name, d.status, d.biz_type, d.is_default, \
                    d.is_system, d.allow_auto_approve, d.current_version_id::text AS current_version_id, \
                    d.current_version_no, \
                    v.id::text AS version_id, v.version_status, v.is_current, v.graph_hash, \
                    v.change_note, v.node_count, v.edge_count, \
                    (SELECT count(*) FROM biz_flow_node n WHERE n.version_id = d.current_version_id) \
                        AS node_rows, \
                    (SELECT count(*) FROM biz_flow_edge e WHERE e.version_id = d.current_version_id) \
                        AS edge_rows, \
                    (SELECT count(*) FROM biz_flow_node_assignee a \
                       JOIN biz_flow_node n ON n.id = a.node_id \
                      WHERE n.version_id = d.current_version_id) AS assignee_rows \
               FROM biz_flow_def d \
               LEFT JOIN biz_flow_def_version v ON v.id = d.current_version_id \
              WHERE d.flow_code = 'hr_leave' AND d.deleted_at IS NULL \
              ORDER BY (d.id = '20000000-0000-4000-8000-0000000e0393'::uuid) DESC, d.id \
              LIMIT 1"
                .to_string(),
            vec![],
        )
        .await
        .expect("读 hr_leave 种子形态");
    let Some(row) = row else {
        return SeedShape {
            defs,
            def_id: None,
            flow_name: None,
            status: None,
            biz_type: None,
            is_default: false,
            is_system: false,
            allow_auto_approve: false,
            current_version_id: None,
            current_version_no: None,
            version_id: None,
            version_status: None,
            is_current: false,
            graph_hash: None,
            change_note: None,
            node_count: None,
            edge_count: None,
            node_rows: 0,
            edge_rows: 0,
            assignee_rows: 0,
        };
    };
    SeedShape {
        defs,
        def_id: row.get_typed("def_id").expect("def_id"),
        flow_name: row.get_typed("flow_name").expect("flow_name"),
        status: row.get_typed("status").expect("status"),
        biz_type: row.get_typed("biz_type").expect("biz_type"),
        is_default: row.get_typed("is_default").expect("is_default"),
        is_system: row.get_typed("is_system").expect("is_system"),
        allow_auto_approve: row.get_typed("allow_auto_approve").expect("allow_auto_approve"),
        current_version_id: row.get_typed("current_version_id").expect("current_version_id"),
        current_version_no: row.get_typed("current_version_no").expect("current_version_no"),
        version_id: row.get_typed::<Option<String>>("version_id").expect("version_id"),
        version_status: row
            .get_typed::<Option<String>>("version_status")
            .expect("version_status"),
        is_current: row
            .get_typed::<Option<bool>>("is_current")
            .expect("is_current")
            .unwrap_or(false),
        graph_hash: row.get_typed::<Option<String>>("graph_hash").expect("graph_hash"),
        change_note: row.get_typed::<Option<String>>("change_note").expect("change_note"),
        node_count: row.get_typed::<Option<i32>>("node_count").expect("node_count"),
        edge_count: row.get_typed::<Option<i32>>("edge_count").expect("edge_count"),
        node_rows: row.get_typed("node_rows").expect("node_rows"),
        edge_rows: row.get_typed("edge_rows").expect("edge_rows"),
        assignee_rows: row.get_typed("assignee_rows").expect("assignee_rows"),
    }
}

/// 只核对、不写回（收尾里「清理夹具之后种子还在」的第二次对账用）。
pub(crate) async fn assert_intact() -> SeedShape {
    let shape = shape().await;
    shape.assert_intact();
    shape
}
