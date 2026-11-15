//! 员工只读查询接缝的**真库**用例（P2-35）。
//!
//! 离线用例（`employee_reader_tests`）证明不了三件事，它们全在 SQL 与 PG 之间：
//!
//! 1. 批量 SQL 真能跑（占位符个数、`IN (...)` 绑定、`get_typed` 的列名与类型）；
//! 2. 软删口径真的过滤掉了（`deleted_at IS NULL` 不是写在注释里）；
//! 3. 超过 [`IN_CHUNK`](crate::business::hr::employee_reader_impl::IN_CHUNK) 的批量
//!    **跨批结果仍是确定性全序**（分片方式不许泄漏成返回顺序）。
//!
//! 不设环境变量时全部 SKIP（不连真库，也不假装绿）：
//!
//! ```bash
//! HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_p235_hr \
//!   cargo test --lib business::hr::employee_reader_db -- --test-threads=1
//! ```
//!
//! 环境变量优先取 `HR_DB_URL`，回退 `ERP_DB_URL`（与其余 HR / ERP 真库用例同一口径）。
//!
//! # 夹具纪律
//!
//! 工号统一 `P235-<唯一后缀>-NN`，清场**按 id 精确删**；唯一例外是 501 行那一组
//! （按带唯一后缀的工号前缀删，前缀本身也带后缀，不会误伤种子数据）。
//! `biz_employee` 的 `status='RESIGNED'` 与 `leave_date` 有双向 CHECK，夹具里成对给。

use nik::prelude::*;
use nik::uuid::Uuid;

mod db {
    use super::*;
    use crate::business::hr::employee_reader::{self, EmployeeReader, GLOBAL_LOCK};
    use crate::business::hr::employee_reader_impl::{self, HrEmployeeReader};
    use crate::business::hr::model::SENTINEL_ORG_ID;
    use cloudpivot_kernel::shared::test_helpers;
    use tokio::sync::OnceCell;

    /// 连库（进程内只做一次初始化）。`false` = 没配库地址，用例直接跳过。
    async fn ensure_db() -> bool {
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("HR_DB_URL").or_else(|_| std::env::var("ERP_DB_URL")) else {
                    println!("SKIP: 未设置 HR_DB_URL / ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                // 池参数 / 建池 runtime 由夹具复用模板统一提供（P2-60）：旧写法把 `max_lifetime`
                // 与 `idle_timeout` 压成 ZERO ⇒ **每条语句**新建连接（99→145 每次 1~3 ms 握手）。
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: HR_DB_URL / ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 唯一后缀（纳秒 + 进程内计数器，避免并发用例互相踩）。
    fn uniq() -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{}{seq:02}", now % 1_000_000_000)
    }

    /// 夹具：往 `biz_employee` 里插一行的形状。
    ///
    /// 只给**读**需要的列（跨域读用不到身份证 / 银行账号），也就不用造敏感数据。
    struct Fixture {
        id: Uuid,
        emp_no: String,
    }

    /// 插一行（`status` 给 `RESIGNED` 时成对给 `leave_date` + `leave_type`，否则撞
    /// `ck_biz_employee_leave` / `ck_biz_employee_leavetype2` 两条 CHECK）。
    async fn insert(tag: &str, index: usize, name: &str, status: &str, dept: Uuid) -> Fixture {
        let id = Uuid::now_v7();
        let emp_no = format!("P235-{tag}-{index:02}");
        let (leave_date, leave_type) = if status == "RESIGNED" {
            ("now()::date", "'RESIGN'")
        } else {
            ("NULL::date", "NULL")
        };
        let sql = format!(
            "INSERT INTO biz_employee \
                (id, org_id, emp_no, name, dept_id, hire_date, status, leave_date, leave_type) \
             VALUES (?, ?, ?, ?, ?, '2026-01-05'::date, ?, {leave_date}, {leave_type})"
        );
        db().execute_with_params(
            &sql,
            vec![
                Value::from(&id),
                Value::from(&SENTINEL_ORG_ID),
                Value::Text(emp_no.clone()),
                Value::Text(name.to_string()),
                Value::from(&dept),
                Value::Text(status.to_string()),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("插夹具员工失败：{err}\nSQL: {sql}"));
        Fixture { id, emp_no }
    }

    /// 软删一行（真库用例要证明「软删的读不到」）。
    async fn soft_delete(id: &Uuid) {
        db().execute_with_params(
            "UPDATE biz_employee SET deleted_at = now() WHERE id = ?",
            vec![Value::from(id)],
        )
        .await
        .expect("软删夹具员工");
    }

    /// 清场：按 id 精确删。
    async fn cleanup(ids: &[Uuid]) {
        if ids.is_empty() {
            return;
        }
        let mut sql = String::from("DELETE FROM biz_employee WHERE id IN (");
        for index in 0..ids.len() {
            if index > 0 {
                sql.push(',');
            }
            sql.push('?');
        }
        sql.push(')');
        db().execute_with_params(&sql, ids.iter().map(Value::from).collect())
            .await
            .expect("清场夹具员工");
    }

    /// ① 按 id 批量：软删的读不到、字段逐列对得上、返回全序。
    #[tokio::test]
    async fn employees_by_ids_returns_briefs_and_skips_soft_deleted() {
        if !ensure_db().await {
            return;
        }
        let tag = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let active = insert(&tag, 1, &format!("在岗{tag}"), "ACTIVE", dept).await;
        let probation = insert(&tag, 2, &format!("试用{tag}"), "PROBATION", dept).await;
        let deleted = insert(&tag, 3, &format!("已删{tag}"), "ACTIVE", dept).await;
        soft_delete(&deleted.id).await;

        let reader = HrEmployeeReader;
        let rows = reader
            .employees_by_ids(&[active.id, probation.id, deleted.id])
            .await
            .expect("按 id 批量");

        let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
        assert_eq!(ids.len(), 2, "软删那一行不许回来：{rows:?}");
        assert!(!ids.contains(&deleted.id), "软删的 id 不许出现");
        assert_eq!(ids, vec![active.id, probation.id], "全序 = (emp_no, id)：工号递增");

        let row = rows.iter().find(|row| row.id == active.id).expect("在岗那行");
        assert_eq!(row.emp_no, active.emp_no);
        assert_eq!(row.name, format!("在岗{tag}"));
        assert_eq!(row.dept_id, dept, "dept_id 原样带出（数据范围裁剪靠它）");
        assert_eq!(row.status, "ACTIVE");
        assert!(row.is_on_duty());
        assert!(
            rows.iter()
                .find(|row| row.id == probation.id)
                .expect("试用那行")
                .is_on_duty(),
            "试用期也算在岗"
        );

        cleanup(&[active.id, probation.id, deleted.id]).await;
    }

    /// ② 空入参与重复入参：不查库 / 不重复返回。
    #[tokio::test]
    async fn employees_by_ids_tolerates_empty_and_duplicate_input() {
        if !ensure_db().await {
            return;
        }
        let tag = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let one = insert(&tag, 1, &format!("去重{tag}"), "ACTIVE", dept).await;

        let reader = HrEmployeeReader;
        assert!(
            reader.employees_by_ids(&[]).await.expect("空入参").is_empty(),
            "空入参必须直接返回空（`IN ()` 是语法错误）"
        );

        let rows = reader
            .employees_by_ids(&[one.id, one.id, one.id])
            .await
            .expect("重复入参");
        assert_eq!(rows.len(), 1, "重复 id 只回一次：{rows:?}");

        cleanup(&[one.id]).await;
    }

    /// ③ 按工号批量：trim 生效、空串与未知工号不返回、软删的不返回、离职按原值带出。
    #[tokio::test]
    async fn employees_by_emp_nos_resolves_and_ignores_unknown() {
        if !ensure_db().await {
            return;
        }
        let tag = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let active = insert(&tag, 1, &format!("在岗{tag}"), "ACTIVE", dept).await;
        let resigned = insert(&tag, 2, &format!("离职{tag}"), "RESIGNED", dept).await;
        let deleted = insert(&tag, 3, &format!("已删{tag}"), "ACTIVE", dept).await;
        soft_delete(&deleted.id).await;

        let reader = HrEmployeeReader;
        let rows = reader
            .employees_by_emp_nos(&[
                format!("  {}  ", active.emp_no), // Excel 里的工号带空格是常态
                resigned.emp_no.clone(),
                String::new(),              // 空串不是工号
                format!("P235-{tag}-NOPE"), // 不存在的工号
                deleted.emp_no.clone(),     // 软删的
            ])
            .await
            .expect("按工号批量");

        let nos: Vec<&str> = rows.iter().map(|row| row.emp_no.as_str()).collect();
        assert_eq!(
            nos,
            vec![active.emp_no.as_str(), resigned.emp_no.as_str()],
            "trim 后解析成功、未知 / 空串 / 软删都不返回：{rows:?}"
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.emp_no == resigned.emp_no)
                .expect("离职那行")
                .status,
            "RESIGNED",
            "状态给落库原值（调用方要跟自己的状态机逐字比对）"
        );

        cleanup(&[active.id, resigned.id, deleted.id]).await;
    }

    /// ④ 跨批（> `IN_CHUNK`）结果仍是确定性全序 —— 分片方式不许泄漏成返回顺序。
    #[tokio::test]
    async fn employees_by_ids_is_stable_across_in_chunks() {
        if !ensure_db().await {
            return;
        }
        let tag = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let prefix = format!("P235BULK-{tag}-");
        // 一次 SQL 造 501 行（> IN_CHUNK=500，必然跨两批）
        db().execute_with_params(
            "INSERT INTO biz_employee (id, org_id, emp_no, name, dept_id, hire_date, status) \
             SELECT gen_random_uuid(), ?, ? || lpad(g::text, 4, '0'), '批量' || g, ?, '2026-01-05'::date, 'ACTIVE' \
               FROM generate_series(1, 501) g",
            vec![
                Value::from(&SENTINEL_ORG_ID),
                Value::Text(prefix.clone()),
                Value::from(&dept),
            ],
        )
        .await
        .expect("批量造 501 行");

        let rows = db()
            .query_sql(
                "SELECT id FROM biz_employee WHERE emp_no LIKE ? ORDER BY emp_no".to_string(),
                vec![Value::Text(format!("{prefix}%"))],
            )
            .await
            .expect("回读 id");
        let ids: Vec<Uuid> = rows
            .iter()
            .map(|row| row.get_typed::<Uuid>("id").expect("id"))
            .collect();
        assert_eq!(ids.len(), 501, "夹具应正好 501 行");

        // 入参故意逆序 + 打乱（模拟导入顺序），返回必须是 (emp_no, id) 全序
        let mut shuffled = ids.clone();
        shuffled.reverse();
        let reader = HrEmployeeReader;
        let briefs = reader.employees_by_ids(&shuffled).await.expect("跨批批量");
        assert_eq!(briefs.len(), 501, "跨批不许丢行");

        let returned: Vec<String> = briefs.iter().map(|brief| brief.emp_no.clone()).collect();
        let mut expected = returned.clone();
        expected.sort();
        assert_eq!(returned, expected, "返回必须按 emp_no 全序，与入参顺序无关");

        db().execute_with_params(
            "DELETE FROM biz_employee WHERE emp_no LIKE ?",
            vec![Value::Text(format!("{prefix}%"))],
        )
        .await
        .expect("清场批量夹具");
    }

    /// ⑤ 探针：迁移跑过的库上 `is_available() == true`（消费方用它替换原来的 `to_regclass` 探测）。
    #[tokio::test]
    async fn is_available_is_true_on_a_migrated_database() {
        if !ensure_db().await {
            return;
        }
        let reader = HrEmployeeReader;
        assert!(
            reader.is_available().await.expect("探针"),
            "模板库 / 演练库都应已跑过 V0383，biz_employee 必须存在"
        );
    }

    /// ⑥ 注册表路径与直调实现**同源**：装上之后模块级入口返回一模一样的结果。
    ///
    /// 这条钉住 `bootstrap` 的接线形状（P2-36 的消费方走的是模块级入口，不是 `HrEmployeeReader`）。
    #[tokio::test]
    async fn registry_path_matches_the_impl() {
        let _guard = GLOBAL_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let tag = uniq();
        let dept = test_helpers::ensure_any_dept().await;
        let one = insert(&tag, 1, &format!("注册表{tag}"), "ACTIVE", dept).await;

        // 与 bootstrap 同样的动作：清空 → 装 HR 实现
        employee_reader::reset_for_tests();
        employee_reader_impl::install().expect("安装 HR 实现");
        assert!(employee_reader::is_installed());

        let direct = HrEmployeeReader.employees_by_ids(&[one.id]).await.expect("直调实现");
        let through_registry = employee_reader::employees_by_ids(&[one.id]).await.expect("模块级入口");
        assert_eq!(direct, through_registry, "注册表路径必须与实现逐字段一致");
        assert_eq!(
            employee_reader::employee_by_id(one.id)
                .await
                .expect("单条")
                .expect("应查到")
                .emp_no,
            one.emp_no
        );
        assert!(
            employee_reader::is_available().await.expect("模块级探针"),
            "装上 reader 之后模块级探针应为 true"
        );

        cleanup(&[one.id]).await;
        employee_reader::reset_for_tests();
    }
}
