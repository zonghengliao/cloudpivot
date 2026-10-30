//! B7「角色互斥（SoD）」真库集成测试（**需要真实 PostgreSQL**，已迁移到 V0068）。
//!
//! # 为什么必须有这一层
//!
//! `business/mutex/` 的模块内单测只能断言**纯逻辑**（规范化、文案、错误码映射）。
//! 本批真正要证明的三件事只有真库能证：
//!
//! 1. **对称性由数据库保证**：`(A,B)` 落库后 `(B,A)` 被 `uk_sys_role_mutex_pair`
//!    （`LEAST/GREATEST` 表达式唯一索引）拒绝，且表里只有一行；
//! 2. **反自反由数据库保证**：`(A,A)` 被 `ck_sys_role_mutex_distinct` 拒绝；
//! 3. **三个校验点的正 / 反用例**（含 D3「同次授予两个互斥角色」、过期绑定不算持有、
//!    停用角色不算持有 → 校验点 2 在启用时拦住）。
//!
//! # 用法
//!
//! ```bash
//! # 专用验证库（不要用生产库 / 不要用 cloudpivot_test，避免与并行的卡抢数据）
//! su postgres -c "createdb -O cloudpivot cloudpivot_b7v"
//! # 跑一次迁移建表（看到「数据库迁移完成」即可退出）
//! for k in ALUN_DATABASE_HOST ALUN_DATABASE_PORT ALUN_DATABASE_USER ALUN_DATABASE_PASSWORD \
//!          ALUN_DATABASE_SSL_MODE ALUN_MIDDLEWARE_AUTH_JWT_SECRET \
//!          CLOUDPIVOT_STORAGE_MASTER_KEY ALUN_ADMIN_PASSWORD; do
//!   v=$(grep -m1 "^$k=" /etc/cloudpivot/cloudpivot-test.env | cut -d= -f2-); export "$k=$v";
//! done
//! export ALUN_PROFILE=test ALUN_DATABASE_NAME=cloudpivot_b7v ALUN_SERVER_LISTEN=127.0.0.1:8027
//! timeout 60 ./target/debug/cloudpivot
//!
//! export B7_DB_URL="postgres://cloudpivot:<pw>@127.0.0.1:5432/cloudpivot_b7v"
//! cargo test --test batchB_b7_mutex -- --nocapture --test-threads=1
//! ```
//!
//! 缺 `B7_DB_URL` 时打印 `SKIP` 并返回（`cargo test` 全绿，CI 不需要数据库）。
//!
//! # 夹具与清理
//!
//! 全部固定 UUID / 固定 `B7_` 前缀的编码与用户名：开头 DELETE 一次、结尾 DELETE 一次，
//! 重复跑不会积累垃圾，也不会影响库里其它数据（只删自己插入的行）。

use cloudpivot::business::mutex::{checker, guard, model};
use cloudpivot::shared::error_code;
use nik::prelude::*;
use nik::uuid::Uuid;

// ---------- 夹具标识 ----------
const ROLE_A: &str = "7b700000-0000-0000-0000-000000000001"; // B7_PURCHASER 采购员
const ROLE_B: &str = "7b700000-0000-0000-0000-000000000002"; // B7_PAY_APPROVER 付款审批
const ROLE_OFF: &str = "7b700000-0000-0000-0000-000000000003"; // B7_DISABLED 停用角色

const USER_A: &str = "7b710000-0000-0000-0000-000000000001"; // 只持采购员
const USER_B: &str = "7b710000-0000-0000-0000-000000000002"; // 只持付款审批
const USER_BOTH: &str = "7b710000-0000-0000-0000-000000000003"; // 既有双持（模拟绕过 API 写库）
const USER_NONE: &str = "7b710000-0000-0000-0000-000000000004"; // 无角色
const USER_EXPIRED: &str = "7b710000-0000-0000-0000-000000000005"; // 采购员绑定已过期
const USER_OFF: &str = "7b710000-0000-0000-0000-000000000006"; // 采购员 + 停用角色

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

/// 初始化全局数据库（进程内只初始化一次）；返回 `false` 表示跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("B7_DB_URL") else {
                println!("SKIP: 未设置 B7_DB_URL（不连真实数据库）");
                return false;
            };
            match Db::connect(&url).await {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    println!("B7 已连接验证库");
                    true
                }
                Err(error) => {
                    println!("SKIP: 连接数据库失败：{error}");
                    false
                }
            }
        })
        .await
}

/// 清理夹具（先删互斥规则 → 绑定 → 用户 → 角色，遵守外键顺序）。
const CLEANUP_SQL: &str = "
DELETE FROM sys_role_mutex
 WHERE role_a_id IN ('7b700000-0000-0000-0000-000000000001'::uuid,
                     '7b700000-0000-0000-0000-000000000002'::uuid,
                     '7b700000-0000-0000-0000-000000000003'::uuid)
    OR role_b_id IN ('7b700000-0000-0000-0000-000000000001'::uuid,
                     '7b700000-0000-0000-0000-000000000002'::uuid,
                     '7b700000-0000-0000-0000-000000000003'::uuid);
DELETE FROM sys_user_role
 WHERE user_id IN ('7b710000-0000-0000-0000-000000000001'::uuid,
                   '7b710000-0000-0000-0000-000000000002'::uuid,
                   '7b710000-0000-0000-0000-000000000003'::uuid,
                   '7b710000-0000-0000-0000-000000000004'::uuid,
                   '7b710000-0000-0000-0000-000000000005'::uuid,
                   '7b710000-0000-0000-0000-000000000006'::uuid);
DELETE FROM sys_user WHERE username LIKE 'b7\\_%';
DELETE FROM sys_role WHERE role_code LIKE 'B7\\_%';
";

/// 灌入夹具：3 个角色（其中一个停用）+ 6 个用户 + 绑定（USER_BOTH 的双持**稍后**插入）。
const SEED_SQL: &str = "
INSERT INTO sys_role (id, role_code, role_name, status, data_scope) VALUES
    ('7b700000-0000-0000-0000-000000000001'::uuid, 'B7_PURCHASER',    '采购员',   1, 1),
    ('7b700000-0000-0000-0000-000000000002'::uuid, 'B7_PAY_APPROVER', '付款审批', 1, 1),
    ('7b700000-0000-0000-0000-000000000003'::uuid, 'B7_DISABLED',     '停用角色', 0, 1);

INSERT INTO sys_user (id, username, password, real_name) VALUES
    ('7b710000-0000-0000-0000-000000000001'::uuid, 'b7_u_holder_a', 'x', '采购张三'),
    ('7b710000-0000-0000-0000-000000000002'::uuid, 'b7_u_holder_b', 'x', '付款李四'),
    ('7b710000-0000-0000-0000-000000000003'::uuid, 'b7_u_both',     'x', '双持王五'),
    ('7b710000-0000-0000-0000-000000000004'::uuid, 'b7_u_none',     'x', NULL),
    ('7b710000-0000-0000-0000-000000000005'::uuid, 'b7_u_expired',  'x', '过期赵六'),
    ('7b710000-0000-0000-0000-000000000006'::uuid, 'b7_u_off',      'x', '停用孙七');

INSERT INTO sys_user_role (id, user_id, role_id, expire_time) VALUES
    (gen_random_uuid(), '7b710000-0000-0000-0000-000000000001'::uuid, '7b700000-0000-0000-0000-000000000001'::uuid, NULL),
    (gen_random_uuid(), '7b710000-0000-0000-0000-000000000002'::uuid, '7b700000-0000-0000-0000-000000000002'::uuid, NULL),
    (gen_random_uuid(), '7b710000-0000-0000-0000-000000000003'::uuid, '7b700000-0000-0000-0000-000000000001'::uuid, NULL),
    (gen_random_uuid(), '7b710000-0000-0000-0000-000000000005'::uuid, '7b700000-0000-0000-0000-000000000001'::uuid, now() - interval '1 day'),
    (gen_random_uuid(), '7b710000-0000-0000-0000-000000000006'::uuid, '7b700000-0000-0000-0000-000000000001'::uuid, NULL),
    (gen_random_uuid(), '7b710000-0000-0000-0000-000000000006'::uuid, '7b700000-0000-0000-0000-000000000003'::uuid, NULL);
";

/// USER_BOTH 的第二条绑定（模拟「历史双持 / 直接写库绕过 API」）。
const BIND_BOTH_SQL: &str = "
INSERT INTO sys_user_role (id, user_id, role_id) VALUES
    (gen_random_uuid(), '7b710000-0000-0000-0000-000000000003'::uuid,
     '7b700000-0000-0000-0000-000000000002'::uuid);
";

async fn exec_script(label: &str, sql: &str) {
    db().execute_script(sql)
        .await
        .unwrap_or_else(|error| panic!("{label} 失败：{error}"));
}

/// 该角色对在表里的行数（双向都算，验证「只存一条」）。
async fn count_pair_rows(a: &Uuid, b: &Uuid) -> i64 {
    db().scalar_with_params(
        "SELECT count(*) FROM sys_role_mutex \
         WHERE deleted_at IS NULL \
           AND LEAST(role_a_id, role_b_id) = LEAST(?::uuid, ?::uuid) \
           AND GREATEST(role_a_id, role_b_id) = GREATEST(?::uuid, ?::uuid)",
        vec![
            Value::from(*a),
            Value::from(*b),
            Value::from(*a),
            Value::from(*b),
        ],
    )
    .await
    .expect("统计互斥行数失败")
}

#[tokio::test]
async fn b7_real_database_acceptance() {
    if !ensure_db().await {
        return;
    }
    exec_script("清理旧夹具", CLEANUP_SQL).await;
    exec_script("灌入夹具", SEED_SQL).await;

    let role_a = uuid(ROLE_A);
    let role_b = uuid(ROLE_B);
    let role_off = uuid(ROLE_OFF);
    let user_a = uuid(USER_A);
    let user_b = uuid(USER_B);
    let user_none = uuid(USER_NONE);
    let user_expired = uuid(USER_EXPIRED);

    println!("\n================ B7 真库验收：对称索引 + 三校验点 ================");

    // ---------- ① 反自反：A 与自己互斥 ----------
    let error = guard::ensure_not_self_pair(role_a, role_a).expect_err("guard 必须拒绝自反互斥");
    assert_eq!(
        (error.status, error.code),
        (400, error_code::ROLE_MUTEX_SELF_PAIR)
    );
    assert_eq!(error.msg, "不能建立角色与自身的互斥关系。");
    // 库层同样拒绝（`ck_sys_role_mutex_distinct`）——两层都不给绕过机会
    let db_error = db()
        .execute_with_params(
            "INSERT INTO sys_role_mutex (id, role_a_id, role_b_id, reason) VALUES (?, ?, ?, ?)",
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(role_a),
                Value::from(role_a),
                Value::from("自反测试".to_string()),
            ],
        )
        .await
        .expect_err("库层 CHECK 必须拒绝自反互斥");
    println!("  ① 反自反：guard=400/61012，库层=CHECK 拒绝（{db_error}）");

    // ---------- ② 对称插入：(A,B) 后 (B,A) 只存一条 ----------
    let rule_id = model::insert_mutex(&role_a, &role_b, "不得同时持有采购与付款权限", None)
        .await
        .expect("首次插入必须成功");
    assert_eq!(count_pair_rows(&role_a, &role_b).await, 1);

    let reverse = model::insert_mutex(&role_b, &role_a, "反向重复", None)
        .await
        .expect_err("反向插入必须被 uk_sys_role_mutex_pair 拒绝");
    assert!(
        matches!(reverse, DbError::UniqueViolation { .. }),
        "反向插入应报 UniqueViolation（handler 会翻译成 409），实际：{reverse:?}"
    );
    assert_eq!(
        count_pair_rows(&role_a, &role_b).await,
        1,
        "(A,B) 与 (B,A) 只能存一行"
    );
    // 读侧不假设写入顺序：反向查也能命中
    let found = model::find_active_pair(&role_b, &role_a)
        .await
        .expect("查询互斥对失败")
        .expect("反向查询必须命中同一条规则");
    assert_eq!(found.id, rule_id);
    // 落库规范化：存储顺序按 UUID 字节序（与 `uk_sys_role_mutex_pair` 的 LEAST/GREATEST 同口径）
    let stored = db()
        .find_first_with_params(
            "SELECT role_a_id, role_b_id FROM sys_role_mutex WHERE id = ?",
            vec![Value::from(rule_id)],
        )
        .await
        .expect("查落库顺序失败")
        .expect("规则行必须存在");
    let stored_a: Uuid = stored.get_typed("role_a_id").expect("取 role_a_id");
    let stored_b: Uuid = stored.get_typed("role_b_id").expect("取 role_b_id");
    let (low, high) = guard::normalize_pair(role_a, role_b);
    assert_eq!(
        (stored_a, stored_b),
        (low, high),
        "落库必须按 UUID 字节序规范化"
    );
    // 展示侧另有一套顺序：按 `role_code` 字典序（B7_PAY_APPROVER < B7_PURCHASER）
    assert_eq!(
        (found.role_a_code.as_str(), found.role_b_code.as_str()),
        ("B7_PAY_APPROVER", "B7_PURCHASER")
    );
    println!(
        "  ② 对称插入：首插成功；反向 UniqueViolation；表内 {} 行；反向查询命中同一条",
        count_pair_rows(&role_a, &role_b).await
    );

    // ---------- ③ 校验点 1：授予角色 ----------
    // 3a 反例：已持有采购员，本次提交「采购员 + 付款审批」→ 409，文案点名两个角色
    let error = guard::ensure_no_mutex(&user_a, &[role_a, role_b])
        .await
        .expect_err("已持有 A 再授予 B 必须被拒");
    assert_eq!(
        (error.status, error.code),
        (409, error_code::ROLE_MUTEX_CONFLICT)
    );
    assert!(
        error
            .msg
            .starts_with("角色「付款审批」与用户已持有的「采购员」互斥"),
        "文案必须点明「本次新增的 B」与「已持有的 A」，实际：{}",
        error.msg
    );
    println!("  ③-1 正例：仅授已持有的「采购员」→ 通过");
    guard::ensure_no_mutex(&user_a, &[role_a])
        .await
        .expect("只授已持有的角色不应命中互斥");
    println!(
        "  ③-1 反例：已持有「采购员」再授「付款审批」→ 409 {}",
        error.msg
    );

    // 3b 对称性：先持有 B，再授 A
    let error = guard::ensure_no_mutex(&user_b, &[role_b, role_a])
        .await
        .expect_err("已持有 B 再授予 A 必须被拒（对称性）");
    assert_eq!(
        (error.status, error.code),
        (409, error_code::ROLE_MUTEX_CONFLICT)
    );
    assert!(
        error
            .msg
            .starts_with("角色「采购员」与用户已持有的「付款审批」互斥"),
        "反向文案必须跟着方向互换，实际：{}",
        error.msg
    );
    println!(
        "  ③-2 对称性：先持「付款审批」再授「采购员」→ 409 同样被拒 {}",
        error.msg
    );

    // 3c D3：同一次请求里同时授予两个互斥角色（无既有角色）→ 必须命中
    let error = guard::ensure_no_mutex(&user_none, &[role_a, role_b])
        .await
        .expect_err("同次授予两个互斥角色必须被拒（D3）");
    assert_eq!(error.code, error_code::ROLE_MUTEX_CONFLICT);
    assert!(
        error.msg.contains("同时包含互斥角色"),
        "D3 文案应说明「同次授予」，实际：{}",
        error.msg
    );
    println!("  ③-3 D3：同次授予 A+B → 409 {}", error.msg);

    // 3d 正向：无角色用户只授一个互斥角色 → 通过
    guard::ensure_no_mutex(&user_none, &[role_a])
        .await
        .expect("只授单个角色不应命中互斥");
    // 3e 过期绑定不算持有：已过期的采购员 + 本次授付款审批 → 通过
    guard::ensure_no_mutex(&user_expired, &[role_b])
        .await
        .expect("过期绑定不算持有，不应命中互斥");
    println!("  ③-4 正例：无角色用户授单个角色、过期绑定用户授对侧角色 → 均通过");

    // ---------- ④ 校验点 3：已有双持时建规则 ----------
    exec_script("插入历史双持绑定", BIND_BOTH_SQL).await;
    let (total, sample) = checker::pair_holders(&role_a, &role_b)
        .await
        .expect("统计双持用户失败");
    assert_eq!(total, 1, "夹具里恰有 1 个历史双持用户");
    assert_eq!(
        sample.first().map(|row| row.username.as_str()),
        Some("b7_u_both")
    );
    let error = guard::ensure_pair_has_no_holders(&role_a, &role_b, "采购员", "付款审批")
        .await
        .expect_err("存在双持用户时必须阻止配置");
    assert_eq!(
        (error.status, error.code),
        (409, error_code::ROLE_MUTEX_HAS_HOLDERS)
    );
    assert!(
        error.msg.contains("1 个用户") && error.msg.contains("b7_u_both"),
        "文案必须含用户数与示例用户名，实际：{}",
        error.msg
    );
    println!("  ④ 校验点 3：存量双持 → 409 {}", error.msg);

    // 体检：该用户命中 1 组；扫描：清单里出现这条规则
    let conflicts = checker::user_conflicts(&uuid(USER_BOTH))
        .await
        .expect("体检失败");
    assert_eq!(conflicts.len(), 1, "双持用户必须命中 1 组互斥");
    assert_eq!(conflicts[0].mutex_id, rule_id);
    let scan = checker::scan().await.expect("全量扫描失败");
    let hit = scan
        .items
        .iter()
        .find(|item| item.rule.id == rule_id)
        .expect("扫描清单必须包含该规则");
    assert_eq!(hit.total, 1);
    let (violated_pairs, violated_holders) = checker::summarize(&scan.items);
    assert!(violated_pairs >= 1 && violated_holders >= 1);
    println!(
        "  ⑤ 体检/扫描：双持用户命中 {} 组；扫描 {} 条规则 → {} 组违规 / {} 人次",
        conflicts.len(),
        scan.scanned_pairs,
        violated_pairs,
        violated_holders
    );

    // 解开双持后，配置校验恢复通过（架构 §4.3 的「先解绑再配置」）
    db().execute_with_params(
        "DELETE FROM sys_user_role WHERE user_id = ?::uuid AND role_id = ?::uuid",
        vec![Value::from(uuid(USER_BOTH)), Value::from(role_b)],
    )
    .await
    .expect("解绑失败");
    guard::ensure_pair_has_no_holders(&role_a, &role_b, "采购员", "付款审批")
        .await
        .expect("解绑后应允许配置");
    println!("  ④ 解绑后：配置校验恢复通过（先解绑再配置）");

    // ---------- ⑤ 停用 / 恢复 + 校验点 2 ----------
    // 先确认夹具：`b7_u_off` 同时持有「采购员」（启用）+「停用角色」（停用）——
    // 这正是「先停用 → 授完 → 再启用」绕过路径的状态。
    let user_off = uuid(USER_OFF);
    let bindings: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM sys_user_role WHERE user_id = ?::uuid AND role_id = ?::uuid",
            vec![Value::from(user_off), Value::from(role_off)],
        )
        .await
        .expect("查夹具绑定失败");
    assert_eq!(bindings, 1, "夹具：停用角色持有者的绑定必须存在");
    // 停用（软删）后规则不再参与判定
    let affected = model::set_enabled(&rule_id, false).await.expect("停用失败");
    assert_eq!(affected, 1);
    assert!(
        model::find_active_pair(&role_a, &role_b)
            .await
            .expect("查")
            .is_none()
    );
    guard::ensure_no_mutex(&user_a, &[role_a, role_b])
        .await
        .expect("规则停用后不应再命中互斥");
    // 恢复启用（仍无双持用户）→ 重新生效
    let affected = model::set_enabled(&rule_id, true).await.expect("恢复失败");
    assert_eq!(affected, 1);
    assert!(
        model::find_active_pair(&role_a, &role_b)
            .await
            .expect("查")
            .is_some()
    );
    println!("  ⑥ 启停：停用后不判定、恢复后重新生效");

    // 校验点 2：停用角色不计入持有 → 允许建规则；但启用该角色必须被拦
    let (total, _) = checker::pair_holders(&role_a, &role_off)
        .await
        .expect("统计失败");
    assert_eq!(total, 0, "停用角色不计入有效持有（校验点 3 因此放行）");
    let off_rule = model::insert_mutex(&role_a, &role_off, "停用角色互斥测试", None)
        .await
        .expect("停用角色对可以建规则（无有效双持）");
    let error = guard::ensure_role_enable_no_mutex(&role_off, "B7_DISABLED", "停用角色")
        .await
        .expect_err("启用角色会让互斥成立，必须被拦");
    assert_eq!(
        (error.status, error.code),
        (409, error_code::ROLE_MUTEX_CONFLICT)
    );
    assert!(
        error.msg.contains("停用角色") && error.msg.contains("b7_u_off"),
        "文案必须含被启用角色名与示例用户名，实际：{}",
        error.msg
    );
    // 反向对照：角色 B 仍在互斥规则里，但它是启用状态 → 校验点 2 只在「停用→启用」时触发，
    // 这里直接构造触发条件的正例已在上面的 `user_off` 上验证过，不再重复。
    println!(
        "  ⑦ 校验点 2：启用「停用角色」→ 409 {}（规则 {off_rule}）",
        error.msg
    );
    // 清理这条临时规则，避免影响后续扫描断言
    model::set_enabled(&off_rule, false)
        .await
        .expect("停用临时规则");

    // ---------- 收尾：清夹具 ----------
    exec_script("清理夹具", CLEANUP_SQL).await;
    println!("\n================ B7 真库验收全部通过 ================");
}
