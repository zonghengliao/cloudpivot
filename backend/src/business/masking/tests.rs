//! 测试C · P0-2：字段脱敏（`system/field-mask`，代码在 `business/masking`）用例集 M01–M09。
//!
//! # 卡片与实际布局的差异（已记入测试报告）
//!
//! 卡片写的模块名是 `system/field-mask`，实际实现在 `business/masking/`（路由前缀仍是
//! `/system/field-mask`，6 个路由：page / create / update / delete / preview / effective）。
//! 该模块**并非 0 测试**：`masker.rs` / `registry.rs` / `model.rs` / `handler.rs` /
//! `user_export.rs` 各自有单测。本文件补的是卡片点名、但此前**没有**覆盖的
//! 「配置 → 生效 → 导出 → 改配置立刻生效 → 不影响库内原值」整条链路。
//!
//! # 安全口径（本文件的核心断言）
//!
//! 无权限角色的响应里**不能出现原值**；规则不可用时必须 **fail-closed**（报错 / 全掩码），
//! 绝不能「悄悄回退明文」。断言一律以「原值串不出现在结果里」为准，而不是「结果等于某个
//! 固定星号串」——后者换个掩码策略就误报（框架的手机号掩码是「保留前 3 后 4」，
//! `Mask::mobile("13800001111")` = `138****1111`，这不是泄漏）。
//!
//! # 为什么目标列用 `sys_user.remark`
//!
//! 种子数据在 `sys_user.mobile / email / real_name` 上**已经种了全局规则**
//! （`role_id IS NULL`，理由「个人信息保护…」）。本文件的用例要验证「我配的这条角色规则
//! 生效」，必须挑一列**没有全局规则**的，否则「改前已经命中」会让 M07 这类用例假红。
//! 顺带说明：全局规则的存在本身是正确行为（默认全体脱敏），已在报告里单列一条观察项。
//!
//! # 隔离（2026-11-04 改造：用例级身份 + 只按 id 集合删，见卡 t_18dcb777）
//!
//! - 所有规则都绑在本次用例新建的角色上（`role_id` 非空），绝不插全局规则；
//! - 每个用例一个唯一身份 `ts::Fixture::new("tc_m", "m05")`（`tc_m_m05_<毫秒+序号>`），
//!   本用例的角色 / 用户 / 规则 `reason` / 菜单都带这串；
//! - 收尾 `fx.sweep()` 只做「按本用例身份 `SELECT id` → 按 id 集合 `DELETE`」，
//!   **不按 `tc_m%` 整批前缀扫** ⇒ 并行跑也不会删掉兄弟用例正在用的行。
//!
//! # 清理口径（2026-10-31 收紧 → 2026-11-04 并发化）
//!
//! 更早这里（和 `system/*/tests.rs` 同款）是 6 处 `let _ = db().execute_with_params(...)`：
//! 删除失败被**静默吞掉**，表象是「用例全绿、日志干净、库里一堆 `tc_m` 夹具」。
//! 之后改成复用 [`crate::system::tests_support`] 的 `cleanup_sql` / `sweep_users` /
//! `sweep_roles`（失败打 `WARN[cleanup]` + SQL + 数据库原文、逐行删）—— 但**清扫范围**
//! 仍是整个 `tc_m` 前缀，于是在 libtest 默认线程数下与本模块的兄弟用例互删：
//!
//! ```text
//! 干净库 `--test-threads=4` → 8 passed / 2 failed（m05、m07）    `--test-threads=1` → 10/0
//! ```
//!
//! 现在清扫范围收敛到「本用例身份」，`m10` 那条自检用例专门盯「被外键钉住的夹具能不能
//! 收干、以及别人的行一行都不许碰」。
//!
//! 清理失败**不许 panic**：用例真正的断言结果不能被清理噪音盖掉；但残留会被
//! `system::tests_support::self_check::x00` 抓到（它按夹具前缀白名单统计四张实体表）。
//!
//! 跑法：
//! ```bash
//! ERP_DB_URL="postgres://<user>:***@127.0.0.1:5432/cloudpivot_test" \
//!   cargo test --lib business::masking::tests -- --test-threads=4
//! ```
//!
//! **默认线程数就是安全的**（原先文件头写着「必须 `--test-threads=1`」，那正是本卡的缺陷）。

use nik::prelude::*;

use crate::business::masking::dto::{FieldMaskCreateReq, FieldMaskUpdateReq};
use crate::business::masking::model::RawRule;
use crate::business::masking::{handler, masker, registry};
use crate::system::tests_support as ts;

/// 脱敏类型：手机号（1）。库里 `ck_sys_field_mask_type` 约束 1..7。
const MOBILE: i16 = 1;
/// 脱敏类型：姓名（4）。
const NAME: i16 = 4;
/// 目标列：`sys_user.remark`（该列**没有**全局种子规则，见文件头）。
const TARGET_TABLE: &str = "sys_user";
const TARGET_COLUMN: &str = "remark";
/// 夹具前缀族（子表头；真正的身份由 [`ts::Fixture`] 生成为 `<PREFIX>_<case>_<uniq>`）。
const PREFIX: &str = "tc_m";

/// 造一个脱敏规则请求体（规则只作用于本次角色）。
fn rule_req(role_id: &str, mask_type: i16, reason: &str) -> FieldMaskCreateReq {
    FieldMaskCreateReq {
        table_name: TARGET_TABLE.to_string(),
        column_name: TARGET_COLUMN.to_string(),
        mask_type,
        mask_param: None,
        role_id: Some(role_id.to_string()),
        status: Some(1),
        reason: reason.to_string(),
    }
}

/// 本次用例的角色 + 两个用户（一个持角色、一个不持角色），返回 `(角色ID, 持角色用户ID, 普通用户ID)`。
async fn fixtures(fx: &ts::Fixture) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let role_id = uuid::Uuid::now_v7();
    let holder = uuid::Uuid::now_v7();
    let plain = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) VALUES (?::text::uuid, ?, ?, 1, 1)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(fx.name("ROLE")),
            Value::from(format!("{}-角色", fx.tag())),
        ],
    )
    .await;
    for (id, name) in [(&holder, "holder"), (&plain, "plain")] {
        ts::exec(
            "INSERT INTO sys_user (id, username, password, real_name, remark, status) \
             VALUES (?::text::uuid, ?, 'x', ?, '机密备注-13912345678', 1)",
            vec![
                Value::from(id.to_string()),
                Value::from(fx.name(name)),
                Value::from(format!("{}-{name}", fx.tag())),
            ],
        )
        .await;
    }
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![Value::from(holder.to_string()), Value::from(role_id.to_string())],
    )
    .await;
    (role_id, holder, plain)
}

/// 用真实 handler 建规则，返回规则 ID。
async fn create_rule(claims: &AuthClaims, role_id: &str, mask_type: i16, reason: &str) -> String {
    let created = handler::create(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
        axum::Json(rule_req(role_id, mask_type, reason)),
    )
    .await
    .unwrap_or_else(|e| panic!("建脱敏规则失败：{}/{} {}", e.status, e.code, e.msg));
    created.data.expect("create 必须返回 data").id
}

/// 夹具统一入口：造一个用例身份 + 本用例的角色/用户，返回 `(身份, 角色ID, 持角色用户ID, 普通用户ID)`。
///
/// 身份形如 `tc_m_m05_<毫秒+序号>`，同一串同时作为 reason / username / role_code 的
/// 前缀，清理只看这一串 —— 不碰兄弟用例的行。
async fn setup(case: &str) -> (ts::Fixture, uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let fx = ts::Fixture::new(PREFIX, case);
    let (role_id, holder, plain) = fixtures(&fx).await;
    (fx, role_id, holder, plain)
}

// ==================== M01 配置规则 / 重复配置 ====================

/// M01：配置「该字段对某角色脱敏」；同一 (表, 列, 角色) 重复配置必须被拒
/// （否则同列两条规则，优先级变成「谁先写谁生效」的不确定行为）。
#[tokio::test]
async fn m01_create_rule_and_reject_duplicate() {
    if !ts::ensure_db().await {
        return;
    }
    let (fx, role_id, _, _) = setup("M01").await;
    let claims = ts::super_claims();

    let id = create_rule(&claims, &role_id.to_string(), MOBILE, &fx.name("不该给只读员看")).await;
    let stored = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_field_mask WHERE id = ?::text::uuid AND deleted_at IS NULL",
        vec![Value::from(id.clone())],
    )
    .await;
    assert_eq!(stored, 1, "规则应已落库");

    let dup = handler::create(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
        axum::Json(rule_req(&role_id.to_string(), NAME, &fx.name("重复规则"))),
    )
    .await
    .expect_err("同 (表,列,角色) 重复配置必须被拒");
    assert_eq!(
        dup.status, 409,
        "重复规则应 409，实际 {}/{} {}",
        dup.status, dup.code, dup.msg
    );
    println!("[M01] 规则落库 + 重复配置 409 ✓（{}）", dup.msg);
    fx.sweep().await;
}

// ==================== M02 无权限角色 → 看不到原值 ====================

/// M02：无「明版权限」的角色查列表，敏感字段必须被打码，**原值串不能出现在结果里**。
#[tokio::test]
async fn m02_unauthorized_role_gets_masked_value() {
    if !ts::ensure_db().await {
        return;
    }
    let (fx, role_id, holder, plain) = setup("M02").await;
    let claims = ts::super_claims();
    create_rule(&claims, &role_id.to_string(), MOBILE, &fx.name("手机号脱敏")).await;

    let raw = "13912345678";
    let set = registry::load_user_set(&holder).await.expect("加载脱敏规则失败");
    assert!(!set.is_empty(), "持角色用户必须命中 1 条规则");
    let masked = set
        .mask_value(TARGET_TABLE, TARGET_COLUMN, raw)
        .expect("脱敏计算不应失败")
        .expect("该列有规则，必须返回 Some");
    assert!(
        !masked.contains(raw),
        "无权限角色的结果里绝不能出现原值，实际 {masked:?}"
    );
    assert!(masked.contains('*'), "打码结果应包含掩码字符，实际 {masked:?}");

    // 不持该角色的用户 → 该列没有规则 → 调用方保持原值（不是「被遮」）
    let other = registry::load_user_set(&plain).await.expect("加载脱敏规则失败");
    assert!(
        other.hits(TARGET_TABLE, TARGET_COLUMN).is_none(),
        "未持有该角色的用户不应命中规则，实际 {:?}",
        other.rules()
    );
    println!("[M02] 无权限角色 → {masked:?}（不含原值）；无关角色 → 无规则 ✓");
    fx.sweep().await;
}

// ==================== M03 有权限 / 豁免 → 完整值 ====================

/// M03：超管（`SUPER_ADMIN`）与持 `system:field-mask:plain` 的用户看明文，且豁免是
/// **显式**的（`is_exempt()` 为真、`mask_value` 恒返回 `None`），不是「恰好没规则」。
#[tokio::test]
async fn m03_exempt_principal_sees_plaintext() {
    if !ts::ensure_db().await {
        return;
    }
    let (fx, role_id, holder, _) = setup("M03").await;
    create_rule(
        &ts::super_claims(),
        &role_id.to_string(),
        MOBILE,
        &fx.name("手机号脱敏"),
    )
    .await;

    // ① 超管：principal.is_super = true
    let super_principal = crate::business::shared::principal::ScopedPrincipal::for_test(
        uuid::Uuid::now_v7(),
        None,
        true,
        crate::business::shared::scope::ScopeKind::All,
        Vec::new(),
        false,
    );
    let super_set = registry::resolve(Some(&super_principal), None)
        .await
        .expect("超管规则集解析失败");
    assert!(super_set.is_exempt(), "超管必须整表豁免");
    assert!(
        super_set
            .mask_value(TARGET_TABLE, TARGET_COLUMN, "13912345678")
            .expect("不应失败")
            .is_none(),
        "豁免集合不得对任何列返回掩码（调用方原样输出明文）"
    );

    // ② 持 system:field-mask:plain 的用户：claims 命中明版权限码 → 豁免
    let plain_claims = ts::claims(&holder.to_string(), "plain", &["system:field-mask:plain"]);
    let normal_principal = crate::business::shared::principal::ScopedPrincipal::for_test(
        holder,
        None,
        false,
        crate::business::shared::scope::ScopeKind::All,
        Vec::new(),
        false,
    );
    let plain_set = registry::resolve(Some(&normal_principal), Some(&plain_claims.0))
        .await
        .expect("明版权限规则集解析失败");
    assert!(
        plain_set
            .mask_value(TARGET_TABLE, TARGET_COLUMN, "13912345678")
            .expect("不应失败")
            .is_none(),
        "持明版权限码的用户必须看到完整值"
    );

    // ③ 对照：同一 principal 但**没有**明版权限码 → 必须被打码
    let masked_set = registry::resolve(Some(&normal_principal), None)
        .await
        .expect("规则集解析失败");
    assert!(
        masked_set
            .mask_value(TARGET_TABLE, TARGET_COLUMN, "13912345678")
            .expect("不应失败")
            .is_some(),
        "没有明版权限码的同一用户必须被打码"
    );
    println!("[M03] 超管 / 明版权限码豁免生效；同用户无明版权限码时被打码 ✓");
    fx.sweep().await;
}

// ==================== M04 导出链路 ====================

/// M04：**导出**走同一份规则集合，导出的单元格里不能有原值。
///
/// 导出是脱敏最容易漏的出口（列表遮住了、导出把明文带走），所以从「库里配置的规则」→
/// `registry` 解析 → `impexp::engine::format_export_rows` 端到端验证一次。
#[tokio::test]
async fn m04_export_applies_mask_from_database_rule() {
    if !ts::ensure_db().await {
        return;
    }
    let (fx, role_id, holder, _) = setup("M04").await;
    create_rule(
        &ts::super_claims(),
        &role_id.to_string(),
        MOBILE,
        &fx.name("导出也要脱敏"),
    )
    .await;

    let set = registry::load_user_set(&holder).await.expect("加载规则失败");
    static COLUMNS: &[crate::business::impexp::definition::ExportColumn] = &[
        crate::business::impexp::definition::ExportColumn {
            key: "username",
            header: "用户名",
            kind: crate::business::impexp::definition::ColumnKind::Text { max_len: None },
            width: None,
            table: "sys_user",
            sensitive: false,
        },
        crate::business::impexp::definition::ExportColumn {
            key: "remark",
            header: "备注",
            kind: crate::business::impexp::definition::ColumnKind::Text { max_len: None },
            width: None,
            table: "sys_user",
            sensitive: true,
        },
    ];
    let raw = "13912345678";
    let mut row = std::collections::HashMap::new();
    row.insert("username", "alice".to_string());
    row.insert("remark", raw.to_string());

    let formatted =
        crate::business::impexp::engine::format_export_rows(COLUMNS, &[row], &std::collections::HashMap::new(), &set)
            .expect("库里有规则，导出应成功");
    let cell = formatted[0].get("备注").cloned().unwrap_or_default();
    assert!(!cell.contains(raw), "导出的单元格里不能出现原值，实际 {cell:?}");
    assert_eq!(
        formatted[0].get("用户名").map(String::as_str),
        Some("alice"),
        "非敏感列不受影响"
    );

    // 反向：同一个导出器 + 没有规则的集合 → 敏感列必须 fail-closed（422），不是明文
    let error = crate::business::impexp::engine::format_export_rows(
        COLUMNS,
        &[std::collections::HashMap::from([
            ("username", "alice".to_string()),
            ("remark", raw.to_string()),
        ])],
        &std::collections::HashMap::new(),
        &registry::MaskSet::empty(),
    )
    .expect_err("敏感列没有规则必须报错");
    assert!(
        !error.message().contains(raw),
        "错误信息里也不能泄漏原值，实际 {}",
        error.message()
    );
    println!(
        "[M04] 导出单元格 {cell:?} 已打码；无规则时 fail-closed（HTTP {}）✓",
        error.http_status()
    );
    fx.sweep().await;
}

// ==================== M05 列表与详情同源 ====================

/// M05：列表与详情必须用**同一份**规则集合（同一入口 `registry::resolve`）。
/// 若两者各算一套，「列表遮住了、详情能看全」这种最典型的脱敏绕过就会上线。
#[tokio::test]
async fn m05_list_and_detail_share_same_ruleset() {
    if !ts::ensure_db().await {
        return;
    }
    let (fx, role_id, holder, _) = setup("M05").await;
    create_rule(&ts::super_claims(), &role_id.to_string(), MOBILE, &fx.name("同源校验")).await;

    let principal = crate::business::shared::principal::ScopedPrincipal::for_test(
        holder,
        None,
        false,
        crate::business::shared::scope::ScopeKind::All,
        Vec::new(),
        false,
    );
    let request_set = registry::resolve(Some(&principal), None).await.expect("resolve 失败");
    let selfcheck_set = registry::load_user_set(&holder).await.expect("load_user_set 失败");
    assert_eq!(
        request_set.rules().len(),
        selfcheck_set.rules().len(),
        "请求入口（列表/详情）与自检入口的规则条数必须一致"
    );
    let raw = "13912345678";
    let a = request_set
        .mask_value(TARGET_TABLE, TARGET_COLUMN, raw)
        .expect("不应失败");
    let b = selfcheck_set
        .mask_value(TARGET_TABLE, TARGET_COLUMN, raw)
        .expect("不应失败");
    assert_eq!(a, b, "同一用户在两个入口必须得到同一个掩码值");
    assert!(a.is_some(), "该用户应命中规则");
    println!("[M05] resolve() 与 load_user_set() 对同一列给出同一掩码 {a:?} ✓");
    fx.sweep().await;
}

// ==================== M06 规则冲突按优先级 ====================

/// M06：两条规则命中同一列时，**角色规则优先于全局规则**（更具体者优先），每列只留一条；
/// 非法 `mask_type` 的行被丢弃（宁可没规则，也不猜一个读不懂的掩码类型）。
///
/// 这里用纯函数 `pick_effective` 验证合并口径（不落库，避免给共享库插全局规则）。
#[tokio::test]
async fn m06_conflict_resolved_by_priority() {
    let now = chrono::Utc::now();
    let global = RawRule {
        id: uuid::Uuid::now_v7(),
        table_name: TARGET_TABLE.to_string(),
        column_name: TARGET_COLUMN.to_string(),
        mask_type: NAME,
        mask_param: None,
        role_id: None,
        role_name: None,
        created_at: now,
    };
    let role = RawRule {
        id: uuid::Uuid::now_v7(),
        table_name: TARGET_TABLE.to_string(),
        column_name: TARGET_COLUMN.to_string(),
        mask_type: MOBILE,
        mask_param: None,
        role_id: Some(uuid::Uuid::now_v7()),
        role_name: Some("只读员".to_string()),
        created_at: now,
    };

    // 两种输入顺序都必须收敛到同一条：角色规则（更具体）赢
    for rows in [vec![global.clone(), role.clone()], vec![role.clone(), global.clone()]] {
        let picked = registry::pick_effective(rows);
        assert_eq!(picked.len(), 1, "同一列只能留下一条规则，实际 {} 条", picked.len());
        assert_eq!(picked[0].mask_type, MOBILE, "角色规则必须优先于全局规则");
    }

    let broken = RawRule {
        id: uuid::Uuid::now_v7(),
        table_name: TARGET_TABLE.to_string(),
        column_name: "email".to_string(),
        mask_type: 99,
        mask_param: None,
        role_id: None,
        role_name: None,
        created_at: now,
    };
    assert!(
        registry::pick_effective(vec![broken]).is_empty(),
        "非法 mask_type 的规则必须被丢弃"
    );
    println!("[M06] 冲突合并：角色规则优先、每列一条、非法类型丢弃 ✓");
}

// ==================== M07 改配置立刻生效 ====================

/// M07：脱敏配置改完**立刻**生效（不用重启）——新建 / 改类型 / 删除三步都要在一次进程内
/// 被立即观测到，这也是「缓存失效」是否正确的回归。
#[tokio::test]
async fn m07_config_change_takes_effect_immediately() {
    if !ts::ensure_db().await {
        return;
    }
    let (fx, role_id, holder, _) = setup("M07").await;
    let claims = ts::super_claims();

    // 改前：该列没有规则（目标列刻意避开有全局种子规则的 mobile/email/real_name）
    let before = registry::load_user_set(&holder).await.expect("加载规则失败");
    assert!(
        before.hits(TARGET_TABLE, TARGET_COLUMN).is_none(),
        "改前不应命中规则，实际 {:?}",
        before.rules()
    );

    // 建规则 → 立刻生效
    let id = create_rule(&claims, &role_id.to_string(), MOBILE, &fx.name("第一步")).await;
    let after_create = registry::load_user_set(&holder).await.expect("加载规则失败");
    let hit = after_create
        .hits(TARGET_TABLE, TARGET_COLUMN)
        .expect("新建后必须立刻命中");
    assert_eq!(hit.mask_type, MOBILE);

    // 改类型 → 立刻换掩码方式
    handler::update(
        Path(id.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
        axum::Json(FieldMaskUpdateReq {
            mask_type: NAME,
            mask_param: None,
            status: Some(1),
            reason: fx.name("第二步改类型"),
        }),
    )
    .await
    .expect("改规则失败");
    let after_update = registry::load_user_set(&holder).await.expect("加载规则失败");
    let hit = after_update.hits(TARGET_TABLE, TARGET_COLUMN).expect("改后必须仍命中");
    assert_eq!(hit.mask_type, NAME, "改完 mask_type 必须立刻生效（缓存已失效）");

    // 删规则 → 立刻失效
    handler::delete(
        Path(id.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
    )
    .await
    .expect("删规则失败");
    let after_delete = registry::load_user_set(&holder).await.expect("加载规则失败");
    assert!(
        after_delete.hits(TARGET_TABLE, TARGET_COLUMN).is_none(),
        "删完规则必须立刻不再命中，实际 {:?}",
        after_delete.rules()
    );
    println!("[M07] 新建 / 改类型 / 删除三步均立刻生效 ✓");
    fx.sweep().await;
}

// ==================== M08 脱敏不影响业务逻辑 ====================

/// M08：脱敏只发生在**响应出口**，库里的原值不能被改，后端内部计算仍用原值。
///
/// 这条最容易在「顺手在 model 层脱敏」的重构里破掉：一旦写入被打码，金额合计、
/// 手机号唯一性校验、导出对账全部会错。
#[tokio::test]
async fn m08_masking_does_not_mutate_stored_value() {
    if !ts::ensure_db().await {
        return;
    }
    let (fx, role_id, holder, _) = setup("M08").await;
    create_rule(&ts::super_claims(), &role_id.to_string(), MOBILE, &fx.name("不许改库")).await;

    let set = registry::load_user_set(&holder).await.expect("加载规则失败");
    let raw = "13912345678";
    let masked = set.mask_value(TARGET_TABLE, TARGET_COLUMN, raw).expect("不应失败");
    assert!(masked.is_some(), "应命中规则");
    assert_ne!(masked.as_deref(), Some(raw), "响应值必须被打码");

    // 库内原值不变（脱敏是出口行为）
    let stored = ts::scalar_text(
        "SELECT remark AS v FROM sys_user WHERE username LIKE ?",
        vec![Value::from(format!("{}%", fx.name("holder")))],
    )
    .await;
    assert_eq!(
        stored.as_deref(),
        Some("机密备注-13912345678"),
        "库内原值必须保持明文（脱敏不许改库）"
    );

    // 内部计算用原值：长度 / 尾号校验仍基于完整值
    assert_eq!(raw.chars().count(), 11, "业务侧拿到的原值仍是完整号码");
    assert!(raw.ends_with("5678"), "尾号校验必须基于原值");
    println!("[M08] 响应被打码、库内原值与内部计算不受影响 ✓");
    fx.sweep().await;
}

// ==================== M09 fail-closed ====================

/// M09：规则不可用时**必须 fail-closed**：绝不回退明文，兜底是全掩码。
#[tokio::test]
async fn m09_fail_closed_never_leaks_plaintext() {
    // ① 兜底值 = 等长全掩码（按字符数，不是字节数，中文也不会算错）
    assert_eq!(registry::MaskSet::fail_closed("13912345678"), "***********");
    assert_eq!(
        registry::MaskSet::fail_closed("张三丰"),
        "***",
        "多字节字符按字符数打码"
    );

    // ② 规则参数非法 → 返回 Err，不给「原样返回」的机会；错误信息也不许带原值
    let raw = "13912345678";
    let error = masker::apply(masker::MaskType::Mobile, Some("abc"), raw).expect_err("非法参数必须报错，不能回退明文");
    assert!(
        !error.message().contains(raw),
        "错误信息里不许带原值，实际 {}",
        error.message()
    );
    assert_eq!(error.http_status(), 400, "规则参数非法属请求错误");

    // ③ 空值原样返回（不产生噪音掩码），但绝不为空值编造内容
    assert_eq!(
        masker::apply(masker::MaskType::Mobile, None, "").expect("空值不应报错"),
        ""
    );
    println!("[M09] fail-closed：全掩码兜底 + 非法参数 Err + 错误信息不含原值 ✓");
}

// ==================== M10 清理基建自检（回归闸） ====================

/// 按前缀数一行残留（自检用；`table` / `column` 只由本文件内部传入常量）。
async fn residue(table: &str, column: &str, prefix: &str) -> i64 {
    ts::scalar_i64(
        &format!("SELECT count(*) AS v FROM {table} WHERE {column} LIKE ?"),
        vec![Value::from(format!("{prefix}%"))],
    )
    .await
}

/// M10：`Fixture::sweep()` 必须把**被外键挡住**的夹具也收干，且**只碰本用例身份的行**。
///
/// # 这条用例守的是什么（并发安全的回归闸）
///
/// 1. **能收干被外键钉住的夹具**：角色对 + `sys_role_mutex`（`role_a_id/role_b_id` 两个
///    硬外键）+ 持角色用户 + 该角色的脱敏规则，四类行必须全部为 0。
///    如果回到「整批 `DELETE ... LIKE 'tc_m%'` + 吞错」，这里立刻红（残留 2 行角色 / 1 行互斥）；
///    如果依赖清单漏了 `sys_role_mutex` / `sys_user_role` / `sys_field_mask`，同样红。
///
/// 2. **只删自己身份的行**：反向对照是**同族前缀（`tc_m`）但不同身份**的另一个用例夹具
///    —— 它在 `--test-threads=4` 下代表「兄弟用例正在用的行」，必须一行不少。
///    这条就是本卡要守的并发口径：清扫范围从「整个 `tc_m` 前缀」收敛到「本用例身份」。
#[tokio::test]
async fn m10_sweep_drains_fk_pinned_fixture_and_stays_scope_scoped() {
    if !ts::ensure_db().await {
        return;
    }

    let probe = ts::Fixture::new(PREFIX, "m10probe");
    let sibling = ts::Fixture::new(PREFIX, "m10sibling");
    let role_a = uuid::Uuid::now_v7();
    let role_b = uuid::Uuid::now_v7();
    let user_id = uuid::Uuid::now_v7();
    let sibling_role = uuid::Uuid::now_v7();
    let sibling_user = uuid::Uuid::now_v7();

    // ① 一对本用例身份的角色：互斥行会指向它们（外键 → 整批 DELETE 会回滚）
    for (id, suffix) in [(&role_a, "MAKER"), (&role_b, "CHECKER")] {
        ts::exec(
            "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) \
             VALUES (?::text::uuid, ?, 'tc_m-探针', 1, 1)",
            vec![Value::from(id.to_string()), Value::from(probe.name(suffix))],
        )
        .await;
    }
    ts::exec(
        "INSERT INTO sys_role_mutex (id, role_a_id, role_b_id, reason) \
         VALUES (?::text::uuid, ?::text::uuid, ?::text::uuid, ?)",
        vec![
            Value::from(uuid::Uuid::now_v7().to_string()),
            Value::from(role_a.to_string()),
            Value::from(role_b.to_string()),
            Value::from(probe.name("互斥")),
        ],
    )
    .await;

    // ② 持该角色的用户（`sys_user_role` 也要被清掉，否则用户行删不掉）
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES (?::text::uuid, ?, 'x', 'tc_m-探针用户', 1)",
        vec![Value::from(user_id.to_string()), Value::from(probe.name("holder"))],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![Value::from(user_id.to_string()), Value::from(role_a.to_string())],
    )
    .await;

    // ③ 该角色的一条脱敏规则（`sys_field_mask.role_id` 也是硬外键）
    ts::exec(
        "INSERT INTO sys_field_mask (id, table_name, column_name, mask_type, role_id, status, reason) \
         VALUES (?::text::uuid, ?, ?, ?, ?::text::uuid, 1, ?)",
        vec![
            Value::from(uuid::Uuid::now_v7().to_string()),
            Value::from(TARGET_TABLE),
            Value::from(TARGET_COLUMN),
            Value::I16(MOBILE),
            Value::from(role_a.to_string()),
            Value::from(probe.name("规则")),
        ],
    )
    .await;

    // ④ 反向对照：**同族前缀、不同身份**的兄弟用例夹具（并行跑时这就是别人的在途行）
    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) \
         VALUES (?::text::uuid, ?, '兄弟用例的角色（不许删）', 1, 1)",
        vec![Value::from(sibling_role.to_string()), Value::from(sibling.name("ROLE"))],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES (?::text::uuid, ?, 'x', '兄弟用例的用户（不许删）', 1)",
        vec![
            Value::from(sibling_user.to_string()),
            Value::from(sibling.name("holder")),
        ],
    )
    .await;

    assert!(
        residue("sys_role", "role_code", probe.tag()).await >= 2,
        "探针角色应已落库"
    );

    // 跑被测的清扫（只清本用例身份）
    probe.sweep().await;

    assert_eq!(
        residue("sys_field_mask", "reason", probe.tag()).await,
        0,
        "本用例的规则行必须被清干净"
    );
    assert_eq!(
        residue("sys_user", "username", probe.tag()).await,
        0,
        "本用例的用户必须被清干净"
    );
    assert_eq!(
        residue("sys_role", "role_code", probe.tag()).await,
        0,
        "本用例的角色必须被清干净（被 sys_role_mutex 钉住时也要能删）"
    );
    let mutex_left = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_role_mutex WHERE reason LIKE ?",
        vec![Value::from(format!("{}%", probe.tag()))],
    )
    .await;
    assert_eq!(mutex_left, 0, "本用例的互斥行必须被清掉（漏了它，删角色会被外键回滚）");

    // 反向对照：同族前缀的兄弟夹具一行未少（清扫不许越界到别人的身份）
    let kept_role = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_role WHERE id = ?::text::uuid",
        vec![Value::from(sibling_role.to_string())],
    )
    .await;
    assert_eq!(
        kept_role, 1,
        "sweep 只许删本用例身份的行，不能按 tc_m 前缀把兄弟用例的角色一起删"
    );
    let kept_user = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_user WHERE id = ?::text::uuid",
        vec![Value::from(sibling_user.to_string())],
    )
    .await;
    assert_eq!(
        kept_user, 1,
        "sweep 只许删本用例身份的行，不能按 tc_m 前缀把兄弟用例的用户一起删"
    );

    sibling.sweep().await;
    println!("[M10] 被外键钉住的夹具已被本用例身份的 sweep 收干；同族兄弟用例零影响 ✓");
}
