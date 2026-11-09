//! `budget_model` 的单测：报表口径（纯函数）+ 真库版本链 / 占用生命周期（`ERP_DB_URL` 开关）。
//!
//! 真库用例与其它 ERP 用例同口径：**不设 `ERP_DB_URL` 就 SKIP**（交付前必须真跑一遍，
//! 见卡面「真机全链路」）。

use super::*;

fn dec(text: &str) -> Decimal {
    text.parse::<Decimal>().expect("金额字面量非法")
}

fn report_row(total: &str, occupied: &str, consumed: &str) -> ReportRow {
    ReportRow {
        budget_id: Uuid::now_v7(),
        fiscal_year: 2026,
        dept_id: Uuid::now_v7(),
        dept_name: "生产部".to_string(),
        account_id: Uuid::now_v7(),
        account_code: "5602.02".to_string(),
        account_name: "管理费用-差旅费".to_string(),
        total_amount: dec(total),
        occupied: dec(occupied),
        consumed: dec(consumed),
        effective_from: None,
        effective: true,
        months: vec![],
        month: MonthSummary::default(),
    }
}

/// ① 报表口径：已用 = 占用中 + 已执行；可用 = 额度 − 已用（负数保留，报表要能看见超支）。
#[test]
fn report_row_takes_occupied_plus_consumed() {
    let row = report_row("10000", "3000", "2000");
    assert_eq!(row.used(), dec("5000"));
    assert_eq!(row.available(), dec("5000"));
    assert_eq!(row.exec_ratio(), dec("50.00"));

    let over = report_row("10000", "12000", "0");
    assert_eq!(over.available(), dec("-2000"), "超支必须显示负数（不截零）");
    assert_eq!(over.exec_ratio(), dec("120.00"));
}

/// ② 额度为 0（无预算）时执行率不除零。
#[test]
fn exec_ratio_guards_zero_limit() {
    let row = report_row("0", "0", "0");
    assert_eq!(row.exec_ratio(), Decimal::ZERO);
    assert_eq!(row.available(), Decimal::ZERO);
}

// ==================== E2E06-FIX-2：报表的月度口径（`month_summary`，纯函数） ====================

fn month_line(period: &str, limit: &str, used: &str) -> (String, Decimal, Decimal) {
    (period.to_string(), dec(limit), dec(used))
}

/// ⑤ WARN 槽场景（卡面验收 ①）：月额度 1000，2027-02 报 1500
/// → `month_over=true / month_over_amount=500 / month_available=-500`。
#[test]
fn month_summary_picks_the_over_month() {
    let months = vec![
        month_line("2027-01", "1000", "0"),
        month_line("2027-02", "1000", "1500"),
        month_line("2027-03", "1000", "300"),
    ];
    let summary = month_summary(&months);
    assert_eq!(summary.period_code.as_deref(), Some("2027-02"));
    assert!(summary.over, "2027-02 已用 1500 > 额度 1000，必须报本月超支");
    assert_eq!(summary.over_amount, Some(dec("500")), "超支额 = 1500 − 1000");
    assert_eq!(summary.available, Some(dec("-500")), "本月可用保留负数");
    assert_eq!((summary.limit, summary.used), (Some(dec("1000")), Some(dec("1500"))));
}

/// ⑥ 年度没超、月度超（卡面验收 ②）：年度可用 10500 > 0，但本月已超 500。
/// 这是「主列看不出来」的那个 bug 的钉子：两个口径必须同时给出且结论不同。
#[test]
fn month_summary_shows_month_over_while_year_is_fine() {
    let months = vec![
        month_line("2027-01", "1000", "0"),
        month_line("2027-02", "1000", "1500"),
    ];
    let summary = month_summary(&months);
    let mut row = report_row("12000", "0", "1500");
    row.fiscal_year = 2027;
    row.months = months;
    row.month = summary;
    assert!(
        row.available() > Decimal::ZERO,
        "年度可用 = 12000 − 1500 = 10500（没超）"
    );
    assert!(
        !(row.available() < Decimal::ZERO),
        "年度 over = false（保持年度口径：DTO 里就是 available < 0）"
    );
    assert!(row.month.over, "月度 over = true（主列要能看出来）");
    assert_eq!(row.month.over_amount, Some(dec("500")));
}

/// ⑦ 多个月都超 → 取超得最多的那个（不是第一个、也不是最后一个）。
#[test]
fn month_summary_picks_worst_over_month() {
    let months = vec![
        month_line("2027-05", "1000", "1200"),
        month_line("2027-08", "1000", "1900"),
        month_line("2027-11", "1000", "1600"),
    ];
    let summary = month_summary(&months);
    assert_eq!(summary.period_code.as_deref(), Some("2027-08"));
    assert_eq!(summary.over_amount, Some(dec("900")));
}

/// ⑧ 一个月都没超 → 报最紧的那个月（可用最小），且 `over_amount = None`（不是 0）。
#[test]
fn month_summary_without_over_reports_tightest_month() {
    let months = vec![
        month_line("2027-01", "1000", "100"),
        month_line("2027-02", "1000", "1000"),
        month_line("2027-03", "1000", "400"),
    ];
    let summary = month_summary(&months);
    assert_eq!(
        summary.period_code.as_deref(),
        Some("2027-02"),
        "用满不算超，但可用最小"
    );
    assert!(!summary.over);
    assert_eq!(summary.over_amount, None);
    assert_eq!(summary.available, Some(Decimal::ZERO));
}

/// ⑨ 没有月度分解行 → 全 None、不报超支（「没分解」不等于「超支」）。
#[test]
fn month_summary_without_breakdown_is_empty() {
    let summary = month_summary(&[]);
    assert_eq!(summary, MonthSummary::default());
    assert!(!summary.over);
    assert_eq!(summary.available, None);
    assert_eq!(summary.period_code, None);
}

/// ⑩ 月度超支判定与提交侧月度闸**同一个函数**（卡面验收 ⑤）：
/// 报表用的 `MonthGate::over` 与 `BudgetVerdict::evaluate` 算出的月度缺口必须同向 ——
/// 只要 gate 说「已超」，提交侧就一定判超支（本次金额 > 0）。
#[test]
fn month_summary_agrees_with_guard_month_gate() {
    use crate::business::erp::budget_guard::{BudgetVerdict, ControlMode, MonthGate};
    let cases = [
        ("1000", "999"),
        ("1000", "1000"),
        ("1000", "1001"),
        ("1000", "1500"),
        ("0", "1"),
        ("1000", "0"),
    ];
    for (limit, used) in cases {
        let gate = MonthGate::of(dec(limit), dec(used));
        let summary = month_summary(&[month_line("2027-02", limit, used)]);
        assert_eq!(summary.over, gate.over(), "报表月度 over 必须等于 MonthGate::over");
        assert_eq!(summary.over_amount, gate.over_amount());
        assert_eq!(summary.available, Some(gate.available()));

        // 提交侧的月度缺口用的是截零后的可用：必须与 gate.available_clamped() 一致
        let verdict = BudgetVerdict::evaluate(
            ControlMode::Warn,
            Some(Uuid::now_v7()),
            dec("100000"),
            Decimal::ZERO,
            gate.limit,
            gate.used,
            dec("100"),
        );
        let expected_gap = (dec("100") - gate.available_clamped()).max(Decimal::ZERO);
        assert_eq!(verdict.month_gap, expected_gap, "月度缺口公式必须与 MonthGate 同源");
        if gate.over() {
            assert!(verdict.over, "月度闸说已超 → 提交侧必须判超支（{limit}/{used}）");
        }
    }
}

/// ③ 状态常量与 DDL / 状态机一致（防三处漂移）。
#[test]
fn status_constants_match_state_machine() {
    let edges = crate::business::shared::state_machine::Entity::Budget.edges();
    let mut nodes: Vec<&str> = Vec::new();
    for (from, to) in edges {
        nodes.push(from);
        nodes.push(to);
    }
    for status in [
        STATUS_DRAFT,
        STATUS_SUBMITTED,
        STATUS_ACTIVE,
        STATUS_SUPERSEDED,
        STATUS_CLOSED,
        STATUS_CANCELLED,
    ] {
        assert!(nodes.contains(&status), "状态 {status} 不在状态机边表的节点集合里");
    }
}

/// ④ 预算单号格式（`BDG-yyyyMMdd-000001`）。
#[test]
fn budget_no_format() {
    let no = crate::business::erp::voucher::format_doc_no(SEQ.prefix, "20261104", 12);
    assert_eq!(no, "BDG-20261104-000012");
}

// ==================== 真库用例（`ERP_DB_URL` 开关） ====================

#[cfg(test)]
mod db {
    use super::*;

    /// 连库（进程内只做一次初始化）。`false` = 没配 `ERP_DB_URL`，用例直接跳过。
    ///
    /// ⚠️ 不设 `ERP_DB_URL` 时这些用例是 **SKIP 假绿**：交付前必须
    /// `ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_fin1 cargo test --lib business::erp::budget -- --test-threads=1` 真跑一遍。
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
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 取一个部门 id：**库里有就用，没有就造夹具部门**（TST-02b A 方案）。
    async fn any_dept() -> Uuid {
        crate::shared::test_helpers::ensure_any_dept().await
    }

    /// 取 5602.02 科目 id（V0226 迁移种子；查不到 = 库版本不对）。
    async fn expense_account() -> Uuid {
        let rows = db()
            .query_sql(
                "SELECT id FROM biz_account WHERE account_code = '5602.02' AND deleted_at IS NULL".to_string(),
                vec![],
            )
            .await
            .expect("查 5602.02 科目");
        rows.first()
            .and_then(|row| row.get_typed("id").ok())
            .unwrap_or_else(|| panic!("库里没有 5602.02 科目（V0226 迁移种子未应用？）"))
    }

    /// 清场：删本用例建的预算（按 id 集合，绝不按前缀扫全库）。
    async fn cleanup(ids: &[Uuid]) {
        if ids.is_empty() {
            return;
        }
        let list = ids
            .iter()
            .map(|id| format!("'{id}'::uuid"))
            .collect::<Vec<_>>()
            .join(",");
        let _ = db()
            .exec_sql(
                format!("DELETE FROM biz_budget_usage WHERE source_id IN ({list})"),
                vec![],
            )
            .await;
        let _ = db()
            .exec_sql(
                format!("DELETE FROM biz_budget_period WHERE budget_id IN ({list})"),
                vec![],
            )
            .await;
        let _ = db()
            .exec_sql(format!("DELETE FROM biz_budget WHERE id IN ({list})"), vec![])
            .await;
    }

    /// ⑤ 版本链：v1 → 调整 v2，`supersedes_id` 指向前一版、`version_no` 递增、
    ///    同槽只能有一个 ACTIVE（库级 `uk_biz_budget_slot_active`）。
    #[tokio::test]
    async fn version_chain_and_single_active() {
        if !ensure_db().await {
            println!("[SKIP] 未设 ERP_DB_URL");
            return;
        }
        let dept = any_dept().await;
        let account = expense_account().await;
        let year = 2099i16; // 用一个不会被真实业务占用的年度
        let mut ids: Vec<Uuid> = Vec::new();
        let v1 = Uuid::now_v7();
        ids.push(v1);
        let mut new = BudgetMut {
            fiscal_year: year,
            dept_id: dept,
            dept_name: "夹具部门".to_string(),
            account_id: account,
            account_code: "5602.02".to_string(),
            account_name: "管理费用-差旅费".to_string(),
            total_amount: dec("12000"),
            control_mode: None,
            version_no: 1,
            supersedes_id: None,
            change_type: CHANGE_ORIGINAL.to_string(),
            change_reason: None,
            remark: None,
            operator: None,
        };
        insert_budget(db(), v1, "BDG-TEST-V1", &new).await.expect("插 v1");
        // v1 直接置 ACTIVE（不跑审批，只验版本链与唯一索引）
        apply_status(db(), v1, STATUS_DRAFT, STATUS_ACTIVE, None, None, None)
            .await
            .expect("v1 生效");

        // 同槽再置一个 ACTIVE 必须撞唯一索引
        let dup = Uuid::now_v7();
        ids.push(dup);
        new.version_no = 2;
        new.supersedes_id = Some(v1);
        new.change_type = CHANGE_ADJUST.to_string();
        new.change_reason = Some("测试调整".to_string());
        insert_budget(db(), dup, "BDG-TEST-V2", &new).await.expect("插 v2");
        let conflict = apply_status(db(), dup, STATUS_DRAFT, STATUS_ACTIVE, None, None, None).await;
        assert!(
            conflict.is_err(),
            "同槽两个 ACTIVE 必须被 uk_biz_budget_slot_active 挡住"
        );

        // 正确顺序：旧版先退位，再置新版
        let retired = supersede_active(db(), dept, account, year, dup)
            .await
            .expect("旧版退位");
        assert_eq!(retired, 1);
        apply_status(db(), dup, STATUS_DRAFT, STATUS_ACTIVE, None, None, None)
            .await
            .expect("v2 生效");
        let chain = version_chain(dept, account, year).await.expect("版本链");
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].version_no, 2, "倒序：最新在前");
        assert_eq!(chain[0].status, STATUS_ACTIVE);
        assert_eq!(chain[1].status, STATUS_SUPERSEDED);
        assert_eq!(chain[0].supersedes_id, Some(v1));
        cleanup(&ids).await;
    }

    /// 清场：删本用例建的映射行（按 id 集合，绝不按 map_type 扫全库 ——
    /// 隔离库上也可能有别人手工配的映射）。
    async fn cleanup_maps(ids: &[Uuid]) {
        if ids.is_empty() {
            return;
        }
        let list = ids
            .iter()
            .map(|id| format!("'{id}'::uuid"))
            .collect::<Vec<_>>()
            .join(",");
        let _ = db()
            .exec_sql(
                format!("DELETE FROM biz_budget_subject_map WHERE id IN ({list})"),
                vec![],
            )
            .await;
    }

    /// 清掉「同一目标」的残留映射（上一次跑挂掉、或 HTTP 验收脚本留下的同目标行）。
    ///
    /// 只按本用例自己用的目标精确删，不扫全表 —— 共享库上别人的配置一行不碰。
    /// 存在的理由：`uk_biz_bsm_target` 只对 `deleted_at IS NULL` 生效，残留行会让
    /// 「① 能落库」这条第一断言直接撞唯一键，用例就成了不可重复跑。
    async fn purge_target(map_type: &str, ref_code: &str, dept: Option<Uuid>) {
        db().exec_sql(
            "DELETE FROM biz_budget_subject_map
                  WHERE map_type = ?
                    AND upper(btrim(ref_code)) = upper(btrim(?))
                    AND COALESCE(dept_id::text, '00000000-0000-0000-0000-000000000000')
                      = COALESCE(NULLIF(?::text, ''),
                                 '00000000-0000-0000-0000-000000000000')"
                .to_string(),
            vec![
                Value::from(map_type.to_string()),
                Value::Text(ref_code.to_string()),
                uuid_text(dept),
            ],
        )
        .await
        .expect("清同目标残留映射（失败会让「能落库」断言假红）");
    }

    /// 除 `exclude` 之外的另一个部门（部门覆盖用例用；库里只有一个部门就跳过）。
    /// 「另一个部门」：库里有别的就用，没有就造夹具（TST-02b A 方案 ⇒ 部门覆盖子断言**不再跳过**）。
    async fn other_dept(exclude: Uuid) -> Uuid {
        let rows = db()
            .query_sql(
                "SELECT id FROM sys_dept WHERE deleted_at IS NULL AND id <> ?::uuid
                  ORDER BY created_at LIMIT 1"
                    .to_string(),
                vec![Value::from(&exclude)],
            )
            .await
            .expect("查第二个部门");
        if let Some(id) = rows.first().and_then(|row| row.get_typed("id").ok()) {
            return id;
        }
        // 库里只有一个部门（很可能就是 `TST-DEPT`）⇒ 造另一个夹具部门；
        // 若那个「唯一部门」恰好是 `TST-DEPT-2`，就反过来造 `TST-DEPT`。
        let (code, name) = if crate::shared::test_helpers::fixture_uuid(&format!(
            "dept:{}",
            crate::shared::test_helpers::FIXTURE_DEPT2_CODE
        )) == exclude
        {
            (
                crate::shared::test_helpers::FIXTURE_DEPT_CODE,
                crate::shared::test_helpers::FIXTURE_DEPT_NAME,
            )
        } else {
            (
                crate::shared::test_helpers::FIXTURE_DEPT2_CODE,
                crate::shared::test_helpers::FIXTURE_DEPT2_NAME,
            )
        };
        let id = crate::shared::test_helpers::ensure_dept(code, name).await;
        assert_ne!(id, exclude, "第二个部门必须与第一个不同");
        id
    }

    /// ⑥ MATERIAL_TYPE 映射全链路（t_ce73ae26 的回归用例）：
    ///
    /// 1. **能落库**：只给 `ref_code`（物料类型编码）就能建，`ref_id` 落库恒为 NULL
    ///    —— 修复前这一步必然 400「必须给 ref_id」；
    /// 2. **能命中**：`resolve_for_po(material_type=RAW)` 取到这条映射的科目，大小写/空白不敏感；
    /// 3. **撞唯一键**：同目标（同部门或同为公司级）再建一条 → `UniqueViolation`
    ///    （handler 翻成可读 409）；
    /// 4. **互不干扰**：同目标下另一种物料类型可以共存；有第二个部门时可以各配一条；
    /// 5. **可重建**：软删后同一目标可以重新建（`uk_biz_bsm_target` 只覆盖 `deleted_at IS NULL`）。
    ///
    /// 干净库上 `sys_dept` 可能是 0 行（bootstrap 不种部门），此时用「全公司映射」
    /// （`dept_id = NULL`，索引里 COALESCE 成零 uuid）跑同一条链路 —— 一样命中同一条
    /// 唯一索引，不会因为缺夹具变成 SKIP 假绿。
    #[tokio::test]
    async fn material_type_mapping_roundtrip() {
        if !ensure_db().await {
            println!("[SKIP] 未设 ERP_DB_URL");
            return;
        }
        let account = expense_account().await;
        let dept_id = any_dept().await;
        // A 方案：部门恒有（库里有就用现成的，没有就造 `TST-DEPT`）⇒ 不再退化成全公司映射
        let dept: Option<Uuid> = Some(dept_id);
        println!("[信息] 用部门 {dept_id} 跑部门级映射");
        let mut ids: Vec<Uuid> = Vec::new();

        // 可重复跑：先把本用例要用的目标上的残留（上次失败 / e2e 留下的）精确清掉
        purge_target("MATERIAL_TYPE", "RAW", dept).await;
        purge_target("MATERIAL_TYPE", "SEMI", dept).await;

        // ① 只给 ref_code 就能落库；就算硬塞了 ref_id 也会被 normalize 丢掉
        let raw = Uuid::now_v7();
        ids.push(raw);
        map_insert(
            db(),
            raw,
            "MATERIAL_TYPE",
            Some(Uuid::now_v7()),
            Some(" raw "),
            account,
            dept,
            0,
            Some("单测 MATERIAL_TYPE"),
            None,
        )
        .await
        .expect("MATERIAL_TYPE 映射必须能落库（t_ce73ae26 修的就是这条）");

        let rows = db()
            .query_sql(
                "SELECT map_type, ref_id, ref_code FROM biz_budget_subject_map WHERE id = ?::uuid".to_string(),
                vec![Value::from(&raw)],
            )
            .await
            .expect("查回落库行");
        let saved = rows.first().expect("刚插的行必须存在");
        assert_eq!(
            saved.get_typed::<String>("map_type").expect("map_type"),
            "MATERIAL_TYPE"
        );
        assert_eq!(
            saved.get_typed::<Option<Uuid>>("ref_id").expect("ref_id"),
            None,
            "MATERIAL_TYPE 的 ref_id 落库必须是 NULL（V0240 ck_biz_bsm_ref）"
        );
        assert_eq!(
            saved
                .get_typed::<Option<String>>("ref_code")
                .expect("ref_code")
                .as_deref(),
            Some("RAW"),
            "物料类型编码入库时归一化成大写"
        );

        // ② 解析侧命中：传 material_type 能取到该类型映射的科目
        let (hit_account, hit_type) = resolve_for_po(db(), dept, None, None, Some("RAW"))
            .await
            .expect("解析不该报错")
            .expect("物料类型 RAW 必须命中一条映射");
        assert_eq!(hit_account, account, "命中的应是这条映射的科目");
        assert_eq!(hit_type, "MATERIAL_TYPE");

        let lower = resolve_for_po(db(), dept, None, None, Some(" raw "))
            .await
            .expect("解析不该报错")
            .expect("物料类型入参大小写/空白不一致也要命中");
        assert_eq!(lower.0, account);

        // ③ 同目标同类型再建一条 → 必须撞唯一键（不是静默多一条）
        let dup = Uuid::now_v7();
        ids.push(dup);
        let err = map_insert(
            db(),
            dup,
            "MATERIAL_TYPE",
            None,
            Some("RAW"),
            account,
            dept,
            0,
            None,
            None,
        )
        .await
        .expect_err("同目标同物料类型必须撞 uk_biz_bsm_target");
        assert!(
            matches!(err, DbError::UniqueViolation { .. }),
            "应为唯一键冲突（handler 会翻成可读 409），实际 {err:?}"
        );

        // ④ 同一目标下另一种物料类型可以共存
        let semi = Uuid::now_v7();
        ids.push(semi);
        map_insert(
            db(),
            semi,
            "MATERIAL_TYPE",
            None,
            Some("SEMI"),
            account,
            dept,
            0,
            None,
            None,
        )
        .await
        .expect("同一目标下的另一种物料类型必须能落库（唯一性按 ref_code 而非 map_type）");

        // ④-b 部门覆盖：同类型在另一个部门可以再配一条（A 方案保证一定有第二个部门）
        let dept2 = other_dept(dept_id).await;
        purge_target("MATERIAL_TYPE", "RAW", Some(dept2)).await;
        let covered = Uuid::now_v7();
        ids.push(covered);
        map_insert(
            db(),
            covered,
            "MATERIAL_TYPE",
            None,
            Some("RAW"),
            account,
            Some(dept2),
            0,
            None,
            None,
        )
        .await
        .expect("另一个部门覆盖同一物料类型必须能落库（部门进唯一键）");

        // ⑤ 软删后同一目标可以重建（部分索引只覆盖未删除行）
        map_delete(db(), raw, None).await.expect("软删");
        let again = Uuid::now_v7();
        ids.push(again);
        map_insert(
            db(),
            again,
            "MATERIAL_TYPE",
            None,
            Some("RAW"),
            account,
            dept,
            0,
            None,
            None,
        )
        .await
        .expect("软删后同一目标应可重建（uk 仅覆盖 deleted_at IS NULL）");

        cleanup_maps(&ids).await;
    }

    // ==================== SCOPE-PAGE 卡同款隐患回归：预算分页的 `{FILTER}` 契约（不连库） ====================

    /// 与 `budget_page` 同一条组装路径：基础 SQL（带 `{FILTER}`）+ 业务过滤片段。
    fn budget_page_sql(q: &BudgetFilter) -> (String, Vec<Value>) {
        let mut params: Vec<Value> = Vec::new();
        let filters =
            crate::business::erp::budget_model::budget_page_filters(q, &mut params).expect("组装预算分页过滤");
        let mut sql = crate::business::erp::budget_model::budget_page_base_sql();
        sql.push_str(&filters);
        (sql, params)
    }

    /// 预算分页的 `base_sql` 必须恰好一个 `{FILTER}`，且它排在所有业务 `?` 之前
    /// （片段参数恒前置，架构 §2.6）—— 否则非超管一带筛选就 500（t_be428a39 同类）。
    #[test]
    fn budget_page_sql_keeps_filter_marker_first() {
        let dept = Uuid::now_v7().to_string();
        let account = Uuid::now_v7().to_string();
        let cases: Vec<(&str, BudgetFilter)> = vec![
            ("无筛选", BudgetFilter::default()),
            (
                "fiscal_year",
                BudgetFilter {
                    fiscal_year: Some(2026),
                    ..Default::default()
                },
            ),
            (
                "dept_id",
                BudgetFilter {
                    dept_id: Some(dept.clone()),
                    ..Default::default()
                },
            ),
            (
                "account_id",
                BudgetFilter {
                    account_id: Some(account.clone()),
                    ..Default::default()
                },
            ),
            (
                "status+change_type",
                BudgetFilter {
                    status: Some("ACTIVE".into()),
                    change_type: Some("ORIGINAL".into()),
                    ..Default::default()
                },
            ),
            (
                "keyword",
                BudgetFilter {
                    keyword: Some("差旅".into()),
                    ..Default::default()
                },
            ),
            (
                "全部同时",
                BudgetFilter {
                    fiscal_year: Some(2026),
                    dept_id: Some(dept),
                    account_id: Some(account),
                    status: Some("ACTIVE".into()),
                    change_type: Some("ORIGINAL".into()),
                    keyword: Some("差旅".into()),
                    page: Some(1),
                    page_size: Some(20),
                },
            ),
        ];
        for (name, filter) in cases {
            let (sql, params) = budget_page_sql(&filter);
            assert_eq!(
                sql.matches(crate::business::shared::scope::FILTER_MARKER).count(),
                1,
                "【{name}】预算分页 base_sql 必须恰好一个 {{FILTER}}：{sql}"
            );
            assert_eq!(
                sql.matches('?').count(),
                params.len(),
                "【{name}】`?` 个数必须等于参数个数：{sql}"
            );
            let marker = sql
                .find(crate::business::shared::scope::FILTER_MARKER)
                .expect("标记位置");
            if let Some(first) = sql.find('?') {
                assert!(marker < first, "【{name}】{{FILTER}} 必须在第一个业务 `?` 之前：{sql}");
            }
            crate::business::shared::scope::ensure_injection_point(&sql, params.len())
                .unwrap_or_else(|err| panic!("【{name}】注入位置契约校验失败：{err}"));
        }
    }
}
