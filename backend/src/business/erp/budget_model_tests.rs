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
        months: vec![],
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
        assert!(
            nodes.contains(&status),
            "状态 {status} 不在状态机边表的节点集合里"
        );
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

    /// 取一个已存在的部门 id（夹具用：不新建部门，避免与其它模块抢 sys_dept）。
    async fn any_dept() -> Option<Uuid> {
        let rows = db()
            .query_sql(
                "SELECT id FROM sys_dept WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1"
                    .to_string(),
                vec![],
            )
            .await
            .ok()?;
        rows.first().and_then(|row| row.get_typed("id").ok())
    }

    /// 取 5602.02 科目 id（V0226 种子）。
    async fn expense_account() -> Option<Uuid> {
        let rows = db()
            .query_sql(
                "SELECT id FROM biz_account WHERE account_code = '5602.02' AND deleted_at IS NULL"
                    .to_string(),
                vec![],
            )
            .await
            .ok()?;
        rows.first().and_then(|row| row.get_typed("id").ok())
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
            .exec_sql(
                format!("DELETE FROM biz_budget WHERE id IN ({list})"),
                vec![],
            )
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
        let (Some(dept), Some(account)) = (any_dept().await, expense_account().await) else {
            println!("[SKIP] 库里没有部门或 5602.02 科目（先跑迁移）");
            return;
        };
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
        insert_budget(db(), v1, "BDG-TEST-V1", &new)
            .await
            .expect("插 v1");
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
        insert_budget(db(), dup, "BDG-TEST-V2", &new)
            .await
            .expect("插 v2");
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
}
