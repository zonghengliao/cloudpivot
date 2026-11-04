//! 合同审批流（ARCH-3 §6 R-P1-1）的真库单测：**提交审批 → 审批通过 → 已签订** 全链。
//!
//! 跑的是 `V0209` 的**真种子**（`contract_approve`：6 节点 6 边，`form.amount < 100000`
//! 单级 / 兜底两级），不是另造测试图 —— 所以这些用例同时验证
//! 「种子能发起 / 阈值真的落在条件边上 / 两级审批要串行 / 内控排除制单人 / 驳回能回草稿」。
//!
//! ```bash
//! export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_p11'
//! cargo test --lib contract_approval -- --test-threads=1
//! ```
//!
//! 夹具身份（卡 `t_f404258c` 的改造规范 R3/R4）：
//! * 账号：`a2ap_` 前缀 + **固定 UUID**（`U_MAKER / U_PM / U_GM`）；供应商：`A2AP-SUP` + 固定 UUID；
//!   合同：`A2AP-<TAG>` 前缀 + **本模块生成的 id**。三者的清场**只按 id 集合删**，
//!   不再出现「按 biz_type 全库删」「按角色 id 把别人的绑定摘掉」这类越权写。
//! * `setup()` 每次先按 id 集合清掉上一轮（crashed 残留也收得走），用例自己跑完不留东西。
//!
//! ⚠️ 与其它真库用例共用全局 `db()` 与同一批 `biz_seq`，所以本模块整体串行
//! （[`DB_LOCK`] = `fixture_hygiene::SEED_ROLE_LOCK`）。
//! ⚠️ `PURCHASE_MANAGER` / `GENERAL_MANAGER` 是**共享种子角色**（审批节点按角色成员展开待办，
//! `join_mode = AND` = 会签：角色上多一个人就多一条待办，且必须所有人都批完节点才推进）。
//! 所以「单级审批一级就过 / 两级审批逐级推进」这两条业务事实的前提是**角色成员 = 本模块夹具**：
//! 本模块在 `SEED_ROLE_LOCK` 里临时**借用**角色成员（登记 → 摘掉别人的 → 用例结束后原样挂回，
//! 见 `borrow_role_membership` / `restore_borrowed_bindings`）。仅靠「把待办过滤到夹具账号」
//! 不够 —— 实测过滤后条数对了，但 `approve` 完实例仍是 `PENDING`（还有别人的待办没批）。
//! 写同一批角色绑定的模块（`purchase_handler`）共用 [`DB_LOCK`] = `fixture_hygiene::SEED_ROLE_LOCK`
//! （同一批全局行 = 同一把锁；实测两把锁并行时它一轮 5 条红）。

use std::str::FromStr;

use axum::Json;
use axum::extract::{Extension, Path};
use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use crate::business::erp::contract_bridge;
use crate::business::erp::contract_dto::{ContractDetailRes, ContractStatusReq, CreateContractReq};
use crate::business::erp::contract_handler::{contract_change_status, contract_create};
use crate::business::erp::contract_model;
use crate::business::flow::engine;
use crate::shared::error_code;

/// 采购经理角色（`V0182` 种子里的固定 UUID）。
pub(crate) const ROLE_PM: &str = "20000000-0000-4000-8000-0000000f0002";
/// 总经理角色（`V0182` 种子里的固定 UUID）。
pub(crate) const ROLE_GM: &str = "20000000-0000-4000-8000-0000000f0003";

/// 制单人（提交审批的人；同时是采购经理 → 用来看内控是否把他从待办里排除）。
const U_MAKER: &str = "a2a90001-0000-4000-8000-000000000001";
/// 纯采购经理（正常一级审批用它）。
const U_PM: &str = "a2a90002-0000-4000-8000-000000000002";
/// 总经理（≥ 10 万的第二级审批 / 小额分支也会兜到超管，这里给个真实账号）。
const U_GM: &str = "a2a90003-0000-4000-8000-000000000003";
/// 夹具供应商。
const SUPPLIER: &str = "a2a90010-0000-4000-8000-000000000010";

/// 真库用例必须串行（共用全局 `db()`、同一批夹具与同一批 `biz_seq`）。
///
/// **必须是 `fixture_hygiene::SEED_ROLE_LOCK` 这把**：`purchase_handler::tests` 也写
/// `ROLE_PM` / `ROLE_GM` 上的 `sys_user_role` 绑定，两把不相识的锁时「谁 setup 谁占位」不成立
/// ⇒ 一方把另一方的夹具账号摘掉 ⇒ 待办展开不到人（实测并行时 `purchase_handler` 5 条红）。
use crate::business::erp::fixture_hygiene::SEED_ROLE_LOCK as DB_LOCK;

fn uid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("测试金额字面量非法")
}

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期夹具")
}

/// 取响应体（`Res<T>` 的 `data`）。
fn data<T: Serialize>(res: Res<T>) -> T {
    res.data.expect("响应体应带 data")
}

/// 以某个真实用户身份造 claims（`sub` 必须是 `sys_user.id`：审批实例的 `initiator_id` 有外键）。
pub(crate) fn claims_of(user: &str) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some(format!("a2ap_{}", &user[..8])),
        roles: vec!["SUPER_ADMIN".to_string()],
        permissions: vec!["*".to_string()],
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
}

async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("ERP_DB_URL") else {
                println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
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
                    println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

async fn exec(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 查一列 `id`（清场第一半：**只在本模块拥有的号段里取 id**，不做任何写操作）。
async fn ids_of(query: &str) -> Vec<Uuid> {
    let rows = db()
        .query_sql(query.to_string(), vec![])
        .await
        .unwrap_or_else(|err| panic!("查夹具 id 失败：{err}\nSQL: {query}"));
    rows.iter()
        .map(|row| row.get_typed::<Uuid>("id").expect("夹具 id"))
        .collect()
}

/// `IN ('<uuid>', …)`（uuid 由库/夹具自己生成，拼进 SQL 安全）。
fn uuid_in(ids: &[Uuid]) -> String {
    ids.iter()
        .map(|id| format!("'{id}'::uuid"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `IN ('<uuid>', …)`（`biz_id` 是 text 弱引用列）。
fn text_in(ids: &[Uuid]) -> String {
    ids.iter()
        .map(|id| format!("'{id}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 本模块夹具的合同 id 集合（`A2AP-` 是**模块独占**的合同名号段，别处不会造）。
async fn fixture_contract_ids() -> Vec<Uuid> {
    ids_of("SELECT id FROM biz_contract WHERE contract_name LIKE 'A2AP-%'").await
}

/// 挂在本模块合同上的审批实例 id 集合（`biz_id` 是合同的 text 形态）。
///
/// 只认「挂在 `A2AP-` 合同上」的实例：过去是 `WHERE biz_type = 'CONTRACT_APPROVE'`
/// 按业务类型**全库删**，那是 class(c) 越权写（别的进程/别的用例的实例也可能落进这一句）。
async fn fixture_instance_ids() -> Vec<Uuid> {
    ids_of(
        "SELECT i.id FROM biz_flow_instance i \
          WHERE i.biz_type = 'CONTRACT_APPROVE' \
            AND i.biz_id IN (SELECT c.id FROM biz_contract c \
                              WHERE c.contract_name LIKE 'A2AP-%')",
    )
    .await
}

/// 清本模块夹具（**只删本模块的 id 集合**，不按 `biz_type` / 角色 id / 前缀批量删）。
///
/// 顺序受外键约束：通知 → 审批实例（级联带走待办/节点实例）→ 合同状态日志 → 合同
/// → 供应商 → 用户角色 → 用户。
///
/// ⚠️ 合同是先删**状态日志**再删单据：`biz_status_log` 没有外键，单据一删就再也
/// 定位不到那些日志（skill「审批痕迹不清理」那一条）。
async fn purge_fixtures() {
    let contracts = fixture_contract_ids().await;
    let instances = fixture_instance_ids().await;

    if !instances.is_empty() {
        let notices = ids_of(&format!(
            "SELECT id FROM sys_notice \
              WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN ({})",
            text_in(&instances)
        ))
        .await;
        if !notices.is_empty() {
            exec(&format!(
                "DELETE FROM sys_notice_receiver WHERE notice_id IN ({})",
                uuid_in(&notices)
            ))
            .await;
            exec(&format!(
                "DELETE FROM sys_notice WHERE id IN ({})",
                uuid_in(&notices)
            ))
            .await;
        }
        // 弱引用先解开（自引用无 ON DELETE）
        exec(&format!(
            "UPDATE biz_flow_instance SET prev_instance_id = NULL \
              WHERE prev_instance_id IN ({})",
            uuid_in(&instances)
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_flow_instance WHERE id IN ({})",
            uuid_in(&instances)
        ))
        .await;
    }

    if !contracts.is_empty() {
        // 合同状态日志（无外键，必须抢在删单据之前）
        exec(&format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'CONTRACT' \
             AND entity_id IN ({})",
            uuid_in(&contracts)
        ))
        .await;
        exec(&format!(
            "DELETE FROM biz_contract WHERE id IN ({})",
            uuid_in(&contracts)
        ))
        .await;
    }

    // 供应商 / 夹具账号：都是固定 UUID，按 id 删（不按前缀扫库）。
    exec(&format!(
        "DELETE FROM biz_supplier WHERE id = '{SUPPLIER}'::uuid"
    ))
    .await;
    exec(&format!(
        "DELETE FROM sys_user_role WHERE user_id IN \
         ('{U_MAKER}'::uuid, '{U_PM}'::uuid, '{U_GM}'::uuid)"
    ))
    .await;
    exec(&format!(
        "DELETE FROM sys_user WHERE id IN \
         ('{U_MAKER}'::uuid, '{U_PM}'::uuid, '{U_GM}'::uuid)"
    ))
    .await;
}

/// 拿锁 + 连库 + 清上一轮 + 建夹具（供应商 + 3 个真实账号 + 角色绑定）。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    // 先把上一轮「借用」的共享角色成员挂回去，再清自己的夹具（顺序反了会把它们也当成残留）。
    restore_borrowed_bindings().await;
    purge_fixtures().await;

    // 夹具身份唯一：供应商 / 三个账号 / 角色绑定都是**固定 UUID**，清场按 id 集合删（R3/R4）。
    // 共享种子角色 ROLE_PM / ROLE_GM 的成员在下一步**锁内借用**（见 `borrow_role_membership`）。
    exec(&format!(
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, credit_days) \
         VALUES ('{SUPPLIER}'::uuid, 'A2AP-SUP', 'A2AP 合同审批测试供应商', 1, 30) \
         ON CONFLICT DO NOTHING"
    ))
    .await;
    exec(&format!(
        "INSERT INTO sys_user (id, username, password, real_name, status) VALUES \
         ('{U_MAKER}'::uuid, 'a2ap_maker', 'x', 'A2AP 制单人', 1), \
         ('{U_PM}'::uuid, 'a2ap_pm', 'x', 'A2AP 采购经理', 1), \
         ('{U_GM}'::uuid, 'a2ap_gm', 'x', 'A2AP 总经理', 1) \
         ON CONFLICT DO NOTHING"
    ))
    .await;
    exec(&format!(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES \
         ('{U_MAKER}'::uuid, '{ROLE_PM}'::uuid), \
         ('{U_PM}'::uuid, '{ROLE_PM}'::uuid), \
         ('{U_GM}'::uuid, '{ROLE_GM}'::uuid) \
         ON CONFLICT DO NOTHING"
    ))
    .await;
    // 独占共享角色成员（只留上面三个账号）：见函数注释 —— 会签语义下这是「单级审批一级就过」
    // 与「两级审批逐级推进」两个断言成立的前提。
    borrow_role_membership().await;
    Some(guard)
}

/// 本模块在共享角色上**唯一允许出现**的三个夹具账号。
fn fixture_user_ids() -> [Uuid; 3] {
    [uid(U_MAKER), uid(U_PM), uid(U_GM)]
}

/// 被本模块临时「借用」的共享角色绑定：`(user_id, role_id)`，下一次 `setup()` / `zz_teardown` 原样挂回。
static BORROWED_BINDINGS: std::sync::Mutex<Vec<(Uuid, Uuid)>> = std::sync::Mutex::new(Vec::new());

/// 把上一轮借用的共享角色绑定**原样挂回**（幂等）。
///
/// 为什么必须归还（改造规范 C5：禁止把别人的绑定永久摘掉）：这些行属于**别人的夹具账号**
/// （`purchase_handler` 的 `p3a_*`、`stocktake` 的 `stk_*` 等），永久摘掉会让对方下一次
/// 「角色成员只有一个」的前提凭空消失。本模块只在用例运行期间借用，随后归还。
async fn restore_borrowed_bindings() {
    let pairs = {
        let mut slot = BORROWED_BINDINGS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *slot)
    };
    for (user, role) in pairs {
        // `WHERE EXISTS (sys_user)`：对方模块如果连夹具账号一起删了，绑定就没法挂回（外键），
        // 这里静默跳过 —— 账号都没了，绑定本来也没有意义。
        exec(&format!(
            "INSERT INTO sys_user_role (user_id, role_id) \
             SELECT '{user}'::uuid, '{role}'::uuid \
              WHERE EXISTS (SELECT 1 FROM sys_user WHERE id = '{user}'::uuid) \
             ON CONFLICT DO NOTHING"
        ))
        .await;
    }
}

/// 借用共享角色成员：把 `ROLE_PM` / `ROLE_GM` 上**不属于本模块夹具**的绑定登记后临时摘掉，
/// 每个用例结束（`restore_borrowed_bindings` 出现在每条用例尾部 / 下一次 `setup()`）原样挂回。
///
/// # 为什么「按夹具账号过滤待办」不足够（实测）
///
/// 审批节点按**角色成员**展开，种子里的 `join_mode = AND` 是**会签**语义：角色上多一个人
/// 就多一条待办，而且节点必须**所有人都批完**才推进。所以残留库上只要有第二个成员：
///
/// * `assert_eq!(tasks.len(), 1)`（过滤到夹具账号后）仍然通过；
/// * 但 `approve` 之后实例还是 `PENDING`（`left: "PENDING", right: "APPROVED"`），
///   两级用例的第二级更是拿不到总经理待办（`left: 0, right: 1`）。
///
/// 也就是说「单级审批一级审完就通过」这条**业务事实**只有在「角色成员 = 本模块夹具」时才成立
/// —— 夹具必须独占角色成员，只过滤待办是不够的。
///
/// # 与过去那句 `DELETE … user_id NOT IN (本模块三个)` 的区别
///
/// ① **登记 + 归还**：摘掉的每一行都会挂回去，不再永久破坏别人的夹具前提；
/// ② **只在 [`DB_LOCK`]（= `fixture_hygiene::SEED_ROLE_LOCK`）里做**：写同一批角色绑定的
///    模块（`purchase_handler`）共用这把锁 ⇒ 不会在它跑到一半时摘它的账号
///    （实测：两把锁时它一轮 5 条红）；
/// ③ **只动 `sys_user_role` 的绑定关系**，不碰 `sys_user` / `sys_role` 主体，也不按前缀扫。
async fn borrow_role_membership() {
    let ours = fixture_user_ids()
        .iter()
        .map(|id| format!("'{id}'::uuid"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT user_id, role_id FROM sys_user_role \
                  WHERE role_id IN ('{ROLE_PM}'::uuid, '{ROLE_GM}'::uuid) \
                    AND user_id NOT IN ({ours}) \
                  ORDER BY user_id, role_id"
            ),
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("读共享角色成员失败：{err}"));
    let pairs: Vec<(Uuid, Uuid)> = rows
        .iter()
        .map(|row| {
            (
                row.get_typed::<Uuid>("user_id").expect("成员 user_id"),
                row.get_typed::<Uuid>("role_id").expect("成员 role_id"),
            )
        })
        .collect();
    if pairs.is_empty() {
        return;
    }
    for (user, role) in &pairs {
        exec(&format!(
            "DELETE FROM sys_user_role WHERE user_id = '{user}'::uuid AND role_id = '{role}'::uuid"
        ))
        .await;
    }
    BORROWED_BINDINGS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .extend(pairs);
}

// ==================== 夹具：建合同 / 提交 / 审批 ====================

/// 建一张草稿合同（金额由调用方给；三个日期齐全 —— 审批通过会直接落 SIGNED）。
async fn make_contract(tag: &str, amount: Decimal) -> Uuid {
    let req = CreateContractReq {
        name: Some(format!("A2AP-{tag}")),
        contract_type: Some("PURCHASE".to_string()),
        party_b_id: Some(SUPPLIER.to_string()),
        amount: Some(amount),
        sign_date: Some(date("2026-01-01")),
        start_date: Some(date("2026-01-02")),
        end_date: Some(date("2026-12-31")),
        remark: Some("A2AP 合同审批用例夹具".to_string()),
        ..Default::default()
    };
    let detail: ContractDetailRes = data(
        contract_create(Extension(AuthClaims(claims_of(U_MAKER))), Json(req))
            .await
            .unwrap_or_else(|err| panic!("建合同失败：{}", err.msg)),
    );
    Uuid::parse_str(&detail.base.id).expect("合同 id 非法")
}

/// 提交审批（`PATCH /status` 目标态 = PENDING_APPROVAL），返回详情。
async fn submit(id: Uuid) -> std::result::Result<ContractDetailRes, ApiError> {
    contract_change_status(
        Extension(AuthClaims(claims_of(U_MAKER))),
        Path(id.to_string()),
        Json(ContractStatusReq {
            target_status: Some("PENDING_APPROVAL".to_string()),
            ..Default::default()
        }),
    )
    .await
    .map(data)
}

/// 某合同的**在跑**审批实例 id。
async fn running_instance(contract_id: Uuid) -> Uuid {
    db().scalar_with_params::<Option<Uuid>>(
        "SELECT id FROM biz_flow_instance \
          WHERE biz_type = 'CONTRACT_APPROVE' AND biz_id = ?::uuid \
            AND status = 'PENDING' AND deleted_at IS NULL",
        vec![Value::from(&contract_id)],
    )
    .await
    .expect("查审批实例失败")
    .expect("应当有在跑的审批实例")
}

/// 某实例的 `PENDING` 待办：`(task_id, assignee_id)`，按 assignee 排序。
async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT id, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' ORDER BY assignee_id, id",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("读待办失败");
    rows.iter()
        .map(|row| {
            (
                row.get_typed("id").unwrap(),
                row.get_typed("assignee_id").unwrap(),
            )
        })
        .collect()
}

/// 某实例的待办里**属于本模块夹具账号**的那部分（`U_PM` / `U_GM` / `U_MAKER`）。
///
/// 为什么要这一层过滤（`t_e2a368f0` 测试卫生）：审批节点的审批人是**按角色展开**的
/// （`PURCHASE_MANAGER` / `GENERAL_MANAGER` 是共享资源，`purchase_handler` 的
/// `p3a_*` 夹具账号也挂在上面）。同一个库连跑第二遍时那些账号还在 → 一个节点被展开成
/// **2 条待办**，于是「单级审批只有一条待办」这种**按条数**的断言假红（实测：erp 段第二轮
/// `large_amount_…` / `small_amount_…` 各红一条，见 docs/evidence/20261104-test-hygiene/raw_prefix_2failed/ 里的 erp_run2 原始日志）。
///
/// 本用例要证明的业务事实是「**走到哪一级**」：单级分支只该有采购经理这一级的待办，
/// 不该有总经理的、也不该有制单人的（内控）。按夹具账号过滤后这两条照样成立，而「角色上
/// 有几个人」不再影响判定 —— 这是**按级判**（更贴合业务事实），不是放宽。
async fn pending_tasks_of_fixtures(instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
    let fixtures = [uid(U_PM), uid(U_GM), uid(U_MAKER)];
    pending_tasks(instance_id)
        .await
        .into_iter()
        .filter(|(_, assignee)| fixtures.contains(assignee))
        .collect()
}

/// 以某人身份同意一条待办（**只动引擎**，业务回调由调用点自己调 —— 与
/// `flow::handler::approve_task` 的顺序一致：先审批事务，再回调）。
async fn approve(task_id: Uuid, operator: Uuid) -> engine::ActionOut {
    db().transaction(|tx| async move {
        engine::approve(&tx, task_id, operator, Some("同意（A2AP 单测）")).await
    })
    .await
    .unwrap_or_else(|err| panic!("审批同意失败：{err}"))
}

/// 以某人身份驳回并终止一条待办。
async fn reject_final(task_id: Uuid, operator: Uuid) -> engine::ActionOut {
    db().transaction(|tx| async move {
        engine::reject(
            &tx,
            task_id,
            operator,
            "驳回：A2AP 单测（金额与合同不符，请复核后重提）",
            engine::ReturnTo::Final,
        )
        .await
    })
    .await
    .unwrap_or_else(|err| panic!("审批驳回失败：{err}"))
}

/// 合同当前状态（读库，不走 DTO）。
async fn status_of(contract_id: Uuid) -> String {
    contract_model::contract_find(contract_id)
        .await
        .expect("查合同失败")
        .expect("合同应存在")
        .status
}

// ==================== 用例 ====================

/// ① 提交审批：`DRAFT → PENDING_APPROVAL` + 挂上实例 + 待办落到采购经理。
#[tokio::test]
async fn submit_moves_draft_to_pending_and_attaches_instance() {
    let Some(_guard) = setup().await else { return };
    let id = make_contract("SUBMIT", dec("50000.00")).await;

    let detail = submit(id).await.expect("提交审批应成功");
    assert_eq!(detail.base.status, "PENDING_APPROVAL");
    assert_eq!(detail.base.status_label, "审批中");
    assert!(!detail.can_edit, "审批中不可改（审 A 改 B 的洞要堵上）");
    assert!(!detail.usable_for_order, "审批中不可被采购单引用");

    let instance = running_instance(id).await;
    assert_eq!(
        detail.base.approval_instance_id,
        Some(instance.to_string()),
        "详情必须下发实例 id"
    );
    assert!(
        detail
            .base
            .approval_instance_no
            .as_deref()
            .is_some_and(|no| no.starts_with("FLW-")),
        "详情必须下发 FLW 单号：{:?}",
        detail.base.approval_instance_no
    );

    // 库侧：db 里的实例 id 与详情一致（不是只在内存里挂上）
    let stored: Option<Uuid> = db()
        .scalar_with_params::<Option<Uuid>>(
            "SELECT approval_instance_id FROM biz_contract WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("查合同实例 id");
    assert_eq!(stored, Some(instance), "approval_instance_id 必须落库");

    // 待办：制单人（也是采购经理）被内控排除，落到另一个采购经理
    let tasks = pending_tasks(instance).await;
    assert!(!tasks.is_empty(), "必须生成待办");
    assert!(
        !tasks.iter().any(|(_, assignee)| *assignee == uid(U_MAKER)),
        "制单人不能拿到自己合同的待办（内控）"
    );
    assert!(
        tasks.iter().any(|(_, assignee)| *assignee == uid(U_PM)),
        "采购经理应当拿到待办"
    );
    // 状态日志留痕（谁在什么时候提交的）
    let logged: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_status_log WHERE entity_type = 'CONTRACT' \
               AND entity_id = ?::uuid AND to_status = 'PENDING_APPROVAL'",
            vec![Value::from(&id)],
        )
        .await
        .expect("查状态日志");
    assert_eq!(logged, 1, "提交审批必须写一条状态日志");
    // 用例结束：把借用的共享角色成员挂回去（进程内最后一个用例也就不用靠下一轮 setup 收尾）。
    restore_borrowed_bindings().await;
}

/// ② 金额 < 10 万：单级审批通过 → `SIGNED`（此后才可被采购单引用）。
#[tokio::test]
async fn small_amount_single_level_approval_signs_contract() {
    let Some(_guard) = setup().await else { return };
    let id = make_contract("SMALL", dec("90000.00")).await;
    submit(id).await.expect("提交审批应成功");
    let instance = running_instance(id).await;

    let tasks = pending_tasks_of_fixtures(instance).await;
    assert_eq!(
        tasks.len(),
        1,
        "小额分支是**单级**审批：只有一个节点应该有待办（实际 {:?}）",
        tasks
    );
    assert_eq!(tasks[0].1, uid(U_PM), "单级审批的待办必须落到采购经理");
    assert!(
        !tasks.iter().any(|(_, assignee)| *assignee == uid(U_GM)),
        "小额分支**不该**给总经理派待办（否则单级/两级被搞反）"
    );
    let out = approve(tasks[0].0, tasks[0].1).await;
    assert_eq!(out.instance_status, "APPROVED", "一级审完就应通过");
    contract_bridge::on_contract_settled(out.instance_id, &out.instance_status).await;

    assert_eq!(status_of(id).await, "SIGNED", "审批通过 → 已签订");
    let row = contract_model::contract_find(id)
        .await
        .expect("查合同")
        .expect("合同在");
    assert_eq!(
        row.approval_instance_id,
        Some(instance),
        "审批通过后实例 id 保留作审计（详情显示「审批通过 FLW-xxx」）"
    );

    // 已签订之后才能生效（ACTIVE 的唯一入口）
    let active: ContractDetailRes = data(
        contract_change_status(
            Extension(AuthClaims(claims_of(U_MAKER))),
            Path(id.to_string()),
            Json(ContractStatusReq {
                target_status: Some("ACTIVE".to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("SIGNED→ACTIVE 应放行"),
    );
    assert_eq!(active.base.status, "ACTIVE");
    assert!(active.usable_for_order);
    // 用例结束：把借用的共享角色成员挂回去（进程内最后一个用例也就不用靠下一轮 setup 收尾）。
    restore_borrowed_bindings().await;
}

/// ③ 金额 ≥ 10 万：**两级串行**审批（采购经理 → 总经理），两级都批完才 SIGNED。
#[tokio::test]
async fn large_amount_requires_two_levels_in_sequence() {
    let Some(_guard) = setup().await else { return };
    let id = make_contract("LARGE", dec("200000.00")).await;
    submit(id).await.expect("提交审批应成功");
    let instance = running_instance(id).await;

    // 第一级：采购经理
    let tasks = pending_tasks_of_fixtures(instance).await;
    assert_eq!(tasks.len(), 1, "两级审批的第一级只有一条待办");
    let out = approve(tasks[0].0, tasks[0].1).await;
    contract_bridge::on_contract_settled(out.instance_id, &out.instance_status).await;
    assert_eq!(
        status_of(id).await,
        "PENDING_APPROVAL",
        "第一级通过后合同必须还在审批中（两级审批不能一级就放行）"
    );

    // 第二级：总经理（第一级的待办批完，节点推进到 n_gm）
    let tasks = pending_tasks_of_fixtures(instance).await;
    assert_eq!(tasks.len(), 1, "第二级应有一条总经理待办");
    assert_eq!(tasks[0].1, uid(U_GM), "第二级必须落到总经理这一级");
    let out = approve(tasks[0].0, tasks[0].1).await;
    assert_eq!(out.instance_status, "APPROVED");
    contract_bridge::on_contract_settled(out.instance_id, &out.instance_status).await;
    assert_eq!(status_of(id).await, "SIGNED", "两级都批完才落已签订");
    // 用例结束：把借用的共享角色成员挂回去（进程内最后一个用例也就不用靠下一轮 setup 收尾）。
    restore_borrowed_bindings().await;
}

/// ④ 驳回 → 回草稿 + 清实例（否则下次提交撞 `uk_biz_flow_instance_biz_active`）。
#[tokio::test]
async fn reject_returns_contract_to_draft_and_clears_instance() {
    let Some(_guard) = setup().await else { return };
    let id = make_contract("REJECT", dec("50000.00")).await;
    submit(id).await.expect("提交审批应成功");
    let instance = running_instance(id).await;

    let tasks = pending_tasks(instance).await;
    let out = reject_final(tasks[0].0, tasks[0].1).await;
    assert_eq!(out.instance_status, "REJECTED");
    contract_bridge::on_contract_settled(out.instance_id, &out.instance_status).await;

    assert_eq!(status_of(id).await, "DRAFT", "驳回必须回草稿");
    let row = contract_model::contract_find(id)
        .await
        .expect("查合同")
        .expect("合同在");
    assert_eq!(row.approval_instance_id, None, "驳回后必须清空实例引用");
    // 驳回意见进状态日志（不打进用户自己的 remark）
    let reason: Option<String> = db()
        .scalar_with_params::<Option<String>>(
            "SELECT reason FROM biz_status_log WHERE entity_type = 'CONTRACT' \
               AND entity_id = ?::uuid AND to_status = 'DRAFT' \
             ORDER BY created_at DESC LIMIT 1",
            vec![Value::from(&id)],
        )
        .await
        .expect("查状态日志");
    assert!(
        reason.as_deref().is_some_and(|text| text.contains("驳回")),
        "状态日志要写明驳回：{reason:?}"
    );

    // 回草稿后可以再提交（实例已清空 → 不会撞唯一索引）
    let again = submit(id).await.expect("回草稿后应能重新提交");
    assert_eq!(again.base.status, "PENDING_APPROVAL");
    // 用例结束：把借用的共享角色成员挂回去（进程内最后一个用例也就不用靠下一轮 setup 收尾）。
    restore_borrowed_bindings().await;
}

/// ⑤ 审批中直接 PATCH 回草稿 = 撤回：在跑的实例必须被终止（不留孤儿待办）。
#[tokio::test]
async fn manual_withdraw_cancels_running_instance() {
    let Some(_guard) = setup().await else { return };
    let id = make_contract("WITHDRAW", dec("50000.00")).await;
    submit(id).await.expect("提交审批应成功");
    let instance = running_instance(id).await;
    assert!(!pending_tasks(instance).await.is_empty(), "撤回前应有待办");

    let back: ContractDetailRes = data(
        contract_change_status(
            Extension(AuthClaims(claims_of(U_MAKER))),
            Path(id.to_string()),
            Json(ContractStatusReq {
                target_status: Some("DRAFT".to_string()),
                ..Default::default()
            }),
        )
        .await
        .expect("审批中 → 草稿（撤回）应放行"),
    );
    assert_eq!(back.base.status, "DRAFT");
    assert_eq!(back.base.approval_instance_id, None, "撤回后清空实例");

    let instance_status: Option<String> = db()
        .scalar_with_params::<Option<String>>(
            "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance)],
        )
        .await
        .expect("查实例状态");
    assert_eq!(
        instance_status.as_deref(),
        Some("CANCELLED"),
        "撤回必须终止在跑的实例（否则留下永远等不到回调的孤儿）"
    );
    let pending: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
            vec![Value::from(&instance)],
        )
        .await
        .expect("查待办");
    assert_eq!(pending, 0, "撤回后不许有 PENDING 待办");
    // 用例结束：把借用的共享角色成员挂回去（进程内最后一个用例也就不用靠下一轮 setup 收尾）。
    restore_borrowed_bindings().await;
}

/// ⑥ 不审批直接点签订 / 生效 → 409 + 66015，文案点名「先提交审批」。
#[tokio::test]
async fn draft_cannot_be_signed_or_activated_without_approval() {
    let Some(_guard) = setup().await else { return };
    let id = make_contract("NOSIGN", dec("50000.00")).await;

    for target in ["SIGNED", "ACTIVE"] {
        let err = submit_with_target(id, target)
            .await
            .expect_err("{target} 必须被拒");
        assert_eq!(err.status, 409, "{target}：{err:?}");
        assert_eq!(err.code, error_code::ERP_CONTRACT_STATUS_ILLEGAL);
        assert!(
            err.msg.contains("PENDING_APPROVAL") && err.msg.contains("审批"),
            "{target} 的文案必须点名「先提交审批」：{}",
            err.msg
        );
    }
    assert_eq!(status_of(id).await, "DRAFT", "被拒后状态不许变");
    let instances: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'CONTRACT_APPROVE' \
               AND biz_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("查实例");
    assert_eq!(instances, 0, "被拒时不许起任何审批实例");
    // 用例结束：把借用的共享角色成员挂回去（进程内最后一个用例也就不用靠下一轮 setup 收尾）。
    restore_borrowed_bindings().await;
}

/// 以任意目标态调一次状态流转（用例内部用）。
async fn submit_with_target(
    id: Uuid,
    target: &str,
) -> std::result::Result<ContractDetailRes, ApiError> {
    contract_change_status(
        Extension(AuthClaims(claims_of(U_MAKER))),
        Path(id.to_string()),
        Json(ContractStatusReq {
            target_status: Some(target.to_string()),
            ..Default::default()
        }),
    )
    .await
    .map(data)
}

/// ⑦ 三个日期不齐 → 提交审批 400（审批通过会直接落 SIGNED，DDL 要求三件套）。
#[tokio::test]
async fn submit_without_dates_returns_400() {
    let Some(_guard) = setup().await else { return };
    let req = CreateContractReq {
        name: Some("A2AP-NODATE".to_string()),
        contract_type: Some("PURCHASE".to_string()),
        party_b_id: Some(SUPPLIER.to_string()),
        amount: Some(dec("50000.00")),
        remark: Some("A2AP 缺日期夹具".to_string()),
        ..Default::default()
    };
    let detail: ContractDetailRes = data(
        contract_create(Extension(AuthClaims(claims_of(U_MAKER))), Json(req))
            .await
            .expect("缺日期也能建草稿"),
    );
    let id = Uuid::parse_str(&detail.base.id).expect("合同 id 非法");

    let err = submit(id).await.expect_err("缺日期必须拒");
    assert_eq!(err.status, 400);
    assert_eq!(err.code, error_code::PARAM_INVALID);
    for field in ["sign_date", "start_date", "end_date"] {
        assert!(err.msg.contains(field), "文案要点名缺哪个日期：{}", err.msg);
    }
    assert_eq!(status_of(id).await, "DRAFT", "被拒后仍是草稿");
    // 用例结束：把借用的共享角色成员挂回去（进程内最后一个用例也就不用靠下一轮 setup 收尾）。
    restore_borrowed_bindings().await;
}

/// ⑧ 流程被下线 → 提交审批 409 + 66033（fail-closed，不静默放行）。
#[tokio::test]
async fn submit_without_published_flow_returns_409() {
    let Some(_guard) = setup().await else { return };
    let id = make_contract("NOFLOW", dec("50000.00")).await;
    exec("UPDATE biz_flow_def SET status = 'DISABLED' WHERE flow_code = 'contract_approve'").await;
    let result = submit(id).await;
    // 无论如何先把流程恢复（断言失败也要恢复，否则后面所有用例全红）
    exec("UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'contract_approve'").await;

    let err = result.expect_err("没流程必须 409（不许静默直接生效）");
    assert_eq!(err.status, 409);
    assert_eq!(err.code, error_code::APPROVAL_FLOW_NOT_CONFIGURED);
    assert!(
        err.msg.contains("contract_approve"),
        "文案要点名流程编码：{}",
        err.msg
    );
    assert_eq!(status_of(id).await, "DRAFT", "被拒后仍是草稿");
    // 用例结束：把借用的共享角色成员挂回去（进程内最后一个用例也就不用靠下一轮 setup 收尾）。
    restore_borrowed_bindings().await;
}

/// ⑨ 模块收尾：清场并断言零残留（`zz_` 前缀保证排在本模块最后跑）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    // 先造一张带在跑审批单的合同，否则「清场成功」可能只是因为空跑
    let id = make_contract("TEARDOWN", dec("50000.00")).await;
    submit(id).await.expect("提交审批应成功");
    let instance = running_instance(id).await;
    assert!(
        !pending_tasks(instance).await.is_empty(),
        "清场前应确有在跑待办"
    );

    // 清场前先把**本模块**的 id 记下来 —— 清完就查不到它们了，这样断言才是「真的删干净了」
    // 而不是「本来就没有」。
    let contract_ids = fixture_contract_ids().await;
    let instance_ids = fixture_instance_ids().await;
    assert!(!contract_ids.is_empty(), "清场前应确有本模块合同");
    assert!(!instance_ids.is_empty(), "清场前应确有在跑审批实例");

    purge_fixtures().await;
    // 本用例自己借用的共享角色成员也在这里归还（其余用例由下一次 `setup()` 归还）。
    restore_borrowed_bindings().await;

    let leftovers: i64 = db()
        .scalar_with_params(
            &format!(
                "SELECT (SELECT count(*) FROM biz_contract WHERE id IN ({contracts})) \
                      + (SELECT count(*) FROM biz_flow_instance WHERE id IN ({instances})) \
                      + (SELECT count(*) FROM biz_status_log \
                          WHERE entity_type = 'CONTRACT' AND entity_id IN ({contracts}))",
                contracts = uuid_in(&contract_ids),
                instances = uuid_in(&instance_ids),
            ),
            vec![],
        )
        .await
        .expect("查残留");
    assert_eq!(leftovers, 0, "清场后本模块的合同/实例/状态日志必须零残留");
    // 用例结束：把借用的共享角色成员挂回去（进程内最后一个用例也就不用靠下一轮 setup 收尾）。
    restore_borrowed_bindings().await;
}
