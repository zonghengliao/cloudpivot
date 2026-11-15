//! 员工只读查询接缝的**离线**用例（P2-35）。
//!
//! 这里证明的是接缝本身的契约（注册表语义 / 未安装时的降级 / 接缝不自带业务域依赖），
//! 以及 P2-B 解环的**燃烧率闸门**（`erp` 生产面还有多少行直连 `biz_employee`）。
//! 「SQL 真能跑」「软删 / 多组织取法对不对」属真库范围，见 `employee_reader_db_tests`。
//!
//! 全程不连库：`cargo test --lib business::hr::employee_reader` 即可。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::employee_reader::{self, EmployeeBrief, EmployeeReader, GLOBAL_LOCK};
use super::employee_reader_impl;

/// 假实现：不碰数据库，记录调用次数并返回固定数据。
///
/// 它同时是「消费方能脱库测试」这条设计目标的**证据**：P2-36 在 `erp` 侧
/// 可以照这个形状注入员工，而不必建库。
struct FakeReader {
    briefs: Vec<EmployeeBrief>,
    by_ids_calls: AtomicUsize,
    by_nos_calls: AtomicUsize,
    /// `None` = 用 trait 默认实现（`Ok(true)`）
    available: Option<bool>,
}

impl FakeReader {
    fn new(briefs: Vec<EmployeeBrief>) -> Self {
        Self {
            briefs,
            by_ids_calls: AtomicUsize::new(0),
            by_nos_calls: AtomicUsize::new(0),
            available: None,
        }
    }
}

#[async_trait]
impl EmployeeReader for FakeReader {
    async fn employees_by_ids(&self, ids: &[Uuid]) -> DbResult<Vec<EmployeeBrief>> {
        self.by_ids_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .briefs
            .iter()
            .filter(|brief| ids.contains(&brief.id))
            .cloned()
            .collect())
    }

    async fn employees_by_emp_nos(&self, emp_nos: &[String]) -> DbResult<Vec<EmployeeBrief>> {
        self.by_nos_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .briefs
            .iter()
            .filter(|brief| emp_nos.iter().any(|no| no == &brief.emp_no))
            .cloned()
            .collect())
    }

    async fn is_available(&self) -> DbResult<bool> {
        match self.available {
            Some(value) => Ok(value),
            None => Ok(true),
        }
    }
}

fn brief(emp_no: &str, seed: u128) -> EmployeeBrief {
    EmployeeBrief {
        id: Uuid::from_u128(seed),
        emp_no: emp_no.to_string(),
        name: format!("员工{emp_no}"),
        // 部门只需是个稳定 UUID（弱引用，不建 FK；真实取法由真库用例覆盖）
        dept_id: Uuid::from_u128(0xd0de_0000_0000_0000_0000_0000_0000_0001),
        status: "ACTIVE".to_string(),
    }
}

// ==================== 注册表语义 ====================

/// ① 未安装：三个入口都降级成「查无」而不是报错（语义见 `employee_reader` 模块注释）。
#[tokio::test]
async fn empty_registry_degrades_to_not_found_instead_of_erroring() {
    let _guard = GLOBAL_LOCK.lock().await;
    employee_reader::reset_for_tests();

    assert!(!employee_reader::is_installed(), "reset 之后应是未安装");
    assert!(employee_reader::reader().is_none());

    let ids = vec![Uuid::from_u128(1)];
    assert!(
        employee_reader::employees_by_ids(&ids)
            .await
            .expect("未安装不许报错")
            .is_empty(),
        "未安装 → 空结果（与「查不到人」同形）"
    );
    assert!(
        employee_reader::employees_by_emp_nos(&["EMP-0001".to_string()])
            .await
            .expect("未安装不许报错")
            .is_empty()
    );
    assert!(
        employee_reader::employee_by_id(Uuid::from_u128(1))
            .await
            .expect("未安装不许报错")
            .is_none()
    );
    assert!(
        !employee_reader::is_available().await.expect("探针不许报错"),
        "未安装 → 主档不可读（这是唯一能区分「没这个能力」与「没这个人」的入口）"
    );
}

/// ② 重复安装是错误，且**不覆盖**先装进来的那个（表的属主只能有一个）。
#[tokio::test]
async fn install_is_exclusive_and_duplicate_install_is_rejected() {
    let _guard = GLOBAL_LOCK.lock().await;
    employee_reader::reset_for_tests();

    let first = Arc::new(FakeReader::new(vec![brief("EMP-0001", 1)]));
    employee_reader::install(first).expect("首次安装应成功");
    assert!(employee_reader::is_installed());

    let second = Arc::new(FakeReader::new(vec![brief("EMP-0002", 2)]));
    let error = employee_reader::install(second).expect_err("重复安装必须报错（不静默覆盖）");
    assert!(error.contains("重复安装"), "{error}");

    // 先装的那个还在
    let rows = employee_reader::employees_by_ids(&[Uuid::from_u128(1)])
        .await
        .expect("查询");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].emp_no, "EMP-0001", "被拒的第二次安装不许影响已有实现");
}

/// ③ 模块级入口真的走已安装的实现（不是每次都返回空）。
#[tokio::test]
async fn module_helpers_go_through_the_installed_reader() {
    let _guard = GLOBAL_LOCK.lock().await;
    employee_reader::reset_for_tests();

    let reader = Arc::new(FakeReader::new(vec![brief("EMP-0001", 1), brief("EMP-0002", 2)]));
    employee_reader::install(reader.clone()).expect("安装");

    let rows = employee_reader::employees_by_ids(&[Uuid::from_u128(1), Uuid::from_u128(2)])
        .await
        .expect("按 id 批量");
    assert_eq!(rows.len(), 2, "两条都要回来");
    assert_eq!(reader.by_ids_calls.load(Ordering::SeqCst), 1, "批量只查一次");

    let by_nos = employee_reader::employees_by_emp_nos(&["EMP-0002".to_string()])
        .await
        .expect("按工号批量");
    assert_eq!(by_nos.len(), 1);
    assert_eq!(by_nos[0].id, Uuid::from_u128(2));

    assert_eq!(
        employee_reader::employee_by_id(Uuid::from_u128(1))
            .await
            .expect("单条")
            .expect("应查到")
            .emp_no,
        "EMP-0001"
    );
    assert!(
        employee_reader::employee_by_id(Uuid::from_u128(999))
            .await
            .expect("单条")
            .is_none(),
        "查不到是 None，不是错误"
    );
    assert!(employee_reader::is_available().await.expect("探针"));
}

/// ④ 单条是**默认实现**（走批量，实现方不需要各写一份）。
#[tokio::test]
async fn employee_by_id_falls_back_to_the_batch_method() {
    let _guard = GLOBAL_LOCK.lock().await;
    employee_reader::reset_for_tests();

    let reader = Arc::new(FakeReader::new(vec![brief("EMP-0003", 3)]));
    employee_reader::install(reader.clone()).expect("安装");

    let one = employee_reader::employee_by_id(Uuid::from_u128(3))
        .await
        .expect("单条")
        .expect("应查到");
    assert_eq!(one.emp_no, "EMP-0003");
    assert_eq!(
        reader.by_ids_calls.load(Ordering::SeqCst),
        1,
        "单条必须复用批量方法（默认实现），不是第二套 SQL"
    );
}

/// ⑤ HR 的实现能装上、且装上之后 `is_installed` 为真（bootstrap 的接线形状）。
#[tokio::test]
async fn hr_reader_can_be_installed_by_bootstrap() {
    let _guard = GLOBAL_LOCK.lock().await;
    employee_reader::reset_for_tests();

    employee_reader_impl::install().expect("HR 实现安装应成功");
    assert!(employee_reader::is_installed());
    assert!(employee_reader::reader().is_some());
    let error = employee_reader_impl::install().expect_err("二次安装必须失败");
    assert!(error.contains("重复安装"), "{error}");
}

/// ⑥ 在岗判据：只有 `PROBATION` / `ACTIVE` 算在岗（停薪留职与离职都不算）。
#[test]
fn on_duty_covers_exactly_two_statuses() {
    let mut row = brief("EMP-0001", 1);
    for (status, on_duty) in [
        ("PROBATION", true),
        ("ACTIVE", true),
        ("SUSPENDED", false),
        ("RESIGNED", false),
    ] {
        row.status = status.to_string();
        assert_eq!(row.is_on_duty(), on_duty, "status={status}");
    }
}

// ==================== 接缝自身的依赖纪律 ====================

/// 源码根（`backend/src`）。
fn src_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// 读一个文件的源码（读不到直接 panic：路径写错时不许静默变绿）。
fn read_source(relative: &str) -> String {
    let path = src_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display()))
}

/// ⑦ 接缝的两个文件**不 import 任何业务域** —— 那正是要解的那条边的形状。
///
/// 为什么连注释一起管：注释里的 `crate::business::xxx` 会被人复制成代码，
/// 也会让 `scripts/lint/module_cycle_report.py` 的「注释引用」一直报这条边。
#[test]
fn seam_source_is_free_of_business_paths() {
    for file in ["business/hr/employee_reader.rs", "business/hr/employee_reader_impl.rs"] {
        let source = read_source(file);
        assert!(!source.is_empty(), "{file} 读到空内容（路径错 = 假绿）");
        assert!(
            !source.contains("crate::business::"),
            "{file} 出现了业务域绝对路径：这个接缝只能依赖框架和本域，跨域读靠注册表反向注入"
        );
    }
}

/// `erp` 生产面里出现 `biz_employee` 的**行数基线**。
///
/// # 这个数字怎么用
///
/// - P2-35 建接缝时**不动** `erp` 的直连 ⇒ 基线是 19；
/// - P2-36 切到接缝后基线 = **0**（本卡实测 0，常量钉死为 0）；
/// - 之后任何「又往 `erp` 里写一条 `biz_employee` SQL / 注释」的改动都会被这条用例拦下。
///
/// 为什么基线是 0 而不是「≤19」：`erp` 生产面**根本不该知道**员工主档的表名 ——
/// 表属主是 HR，读员工走 `hr::employee_reader` 接缝。0 是唯一能表达这件事的数。
///
/// 统计口径与 `grep -rn biz_employee --include=*.rs business/erp/ | grep -v _tests.rs | wc -l`
/// 对齐（**只算生产面**，测试夹具要造员工行是合法的）。
const ERP_BIZ_EMPLOYEE_REFS_BASELINE: usize = 0;

/// 扫描面自检：`erp` 生产面 `.rs` 文件数（扫空 = 假绿，本类卡最大的坑）。
const ERP_SOURCE_MIN_FILES: usize = 250;

/// 列出 `erp` 生产面的 `.rs` 文件（排除 `*_tests.rs`），并断言扫描面够大。
fn erp_production_files() -> Vec<std::path::PathBuf> {
    fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(&path, files)?;
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("_tests.rs"))
            {
                files.push(path);
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    walk(&src_root().join("business/erp"), &mut files).expect("遍历 business/erp");
    assert!(
        files.len() >= ERP_SOURCE_MIN_FILES,
        "扫描面太小：{} 个生产面文件 < {ERP_SOURCE_MIN_FILES}（路径写错会静默变绿）",
        files.len()
    );
    files
}

/// ⑧ 燃烧率闸门：`erp` 生产面的 `biz_employee` 直连**必须是 0 行**。
#[test]
fn erp_biz_employee_reference_budget_is_not_exceeded() {
    let files = erp_production_files();

    let mut refs = 0usize;
    let mut worst: Vec<String> = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file).unwrap_or_default();
        for (index, line) in source.lines().enumerate() {
            if line.contains("biz_employee") {
                refs += 1;
                if worst.len() < 10 {
                    worst.push(format!(
                        "{}:{}: {}",
                        file.strip_prefix(src_root()).unwrap_or(file).display(),
                        index + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        refs == ERP_BIZ_EMPLOYEE_REFS_BASELINE,
        "erp 生产面对 biz_employee 的直连**回退**了：{refs} 行，基线 {ERP_BIZ_EMPLOYEE_REFS_BASELINE} 行。\n\
         员工基础信息必须走 `hr::employee_reader`（P2-35 的接缝），不要在 erp 里再写 biz_employee 的 SQL。\n\
         前 10 处命中：\n{}",
        worst.join("\n")
    );
    println!(
        "erp 生产面 biz_employee 直连：{refs} 行（基线 {ERP_BIZ_EMPLOYEE_REFS_BASELINE}）· 扫描 {} 个文件",
        files.len()
    );
}

/// ⑨ `erp` 生产面对 `hr` 的引用**只允许**是接缝模块 `hr::employee_reader`（P2-36）。
///
/// 为什么单独钉一条：`erp → hr` 这条边在 P2-36 之后**仍然存在**（消费方要调接缝），
/// 所以「边消失」不能用「零引用」来表达。可以钉住的是**边的形状**：只认接缝，
/// 不认 `hr::employee_model` / `hr::employee_reader_impl` / `hr::model` 这些实现面 ——
/// 否则今天「就调一下 model 省事」，明天接缝就形同虚设。
#[test]
fn erp_only_depends_on_the_employee_seam() {
    let files = erp_production_files();
    let prefix = "business::hr::";
    let mut offenders: Vec<String> = Vec::new();
    let mut seam_hits = 0usize;

    for file in &files {
        let source = std::fs::read_to_string(file).unwrap_or_default();
        for (index, line) in source.lines().enumerate() {
            let mut rest = line;
            while let Some(pos) = rest.find(prefix) {
                let tail = &rest[pos + prefix.len()..];
                let head: &str = tail
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or("");
                if head == "employee_reader" {
                    seam_hits += 1;
                } else {
                    offenders.push(format!(
                        "{}:{}: {}",
                        file.strip_prefix(src_root()).unwrap_or(file).display(),
                        index + 1,
                        line.trim()
                    ));
                }
                rest = tail;
            }
        }
    }

    assert!(
        seam_hits >= 1,
        "erp 生产面一处都没引用 `hr::employee_reader` —— 接缝调用面不见了（扫描 {} 个文件）",
        files.len()
    );
    assert!(
        offenders.is_empty(),
        "erp 只许依赖员工接缝（`business::hr::employee_reader`），下面这些是绕过接缝的引用：\n{}",
        offenders.join("\n")
    );
    println!(
        "erp → hr 接缝引用：{seam_hits} 处，越界 0 处 · 扫描 {} 个文件",
        files.len()
    );
}
