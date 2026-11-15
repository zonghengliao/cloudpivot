//! 模块边护栏：**`hr` 的源码里不许出现任何 `erp` 引用**（P2-B 解环 · 卡 `t_3442b4fb`）。
//!
//! # 为什么需要这条用例（而不是只靠报告脚本）
//!
//! `scripts/lint/module_cycle_report.py` 是**报告**工具（恒退出 0），
//! 有人「顺手 import 一行凭证域 / 取号域的路径」时它不会拦任何人。
//! 本卡把 `hr → erp` 的 6 处代码回边全部摘掉（凭证过账 / EMP 号段 / 期间键 / 审批闸），
//! 之后这条边**必须是 0**，所以要在 CI 里钉死 —— 判据与 `erp → hr` 那条
//! `erp_only_depends_on_the_employee_seam` 同款（那条边是**允许**的，因为消费方总得调接缝；
//! 本方向**不允许**，因为 HR 需要的三样东西都已搬到中立位置）。
//!
//! # 那 6 处现在分别走哪
//!
//! | 原回边 | 现在 |
//! |---|---|
//! | `salary_bridge` → `erp::voucher` / `voucher_multi`（过账 + 红冲） | `business::shared::voucher_port`（实现在 ERP，启动期注册） |
//! | `leave_handler` / `salary_sheet_handler` → `erp::approval_gate` | `business::flow::approval_gate`（P2-34 已搬进审批域，`erp::approval_gate` 本来就是它的再导出） |
//! | `leave_model` → `erp::serial_model::period_key_of` | `business::shared::period_key::period_key_of`（纯函数下移基座） |
//! | `employee_no` → `erp::seq`（`EMP` 号段） | `business::shared::seq`（取号引擎下移基座；查重 SQL 一直归 HR 自己） |
//!
//! # 扫描面
//!
//! **连测试文件一起扫**：`hr/*_db_tests.rs` 里 `use` 一次凭证域的实现，就等于把边又接回去了
//! （它在 `module_cycle_report` 里也是一条 code 引用）。所以测试期装接缝的入口放在
//! `bootstrap::install_voucher_port_for_tests`（`bootstrap` 不是业务域，两边都看得到）。

use std::path::{Path, PathBuf};

/// 扫描面自检：`hr/` 下至少这么多 `.rs`（扫空 = 假绿，这类卡最大的坑）。
const HR_SOURCE_MIN_FILES: usize = 60;

/// `hr` 源码里 `erp` 引用的**行数基线**：**0**。
///
/// 为什么是 0 而不是「≤6」：那 6 处不是「暂时留着」，而是**每一处都有中立落点**
/// （见模块注释的表）。留一个允许位，就等于给下一条回边开了一扇门。
const HR_TO_ERP_REFS_BASELINE: usize = 0;

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// 列出 `hr/` 下所有 `.rs`（含测试文件），并断言扫描面够大。
fn hr_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(&path, files)?;
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    walk(&src_root().join("business").join("hr"), &mut files).expect("遍历 business/hr");
    assert!(
        files.len() >= HR_SOURCE_MIN_FILES,
        "扫描面太小：{} 个文件 < {HR_SOURCE_MIN_FILES}（路径写错会静默变绿）",
        files.len()
    );
    files
}

/// 判据用的针：**拼出来**，不能写成本字面量 —— 否则本文件自己就命中，用例永远红。
fn needle() -> String {
    format!("{}::business::erp", "crate")
}

/// ① 燃烧率闸门：`hr` 对 `erp` 的引用必须是 **0 行**（含注释：注释里的路径也会被复制成代码）。
#[test]
fn hr_source_has_no_erp_references() {
    let needle = needle();
    let files = hr_files();

    let mut hits: Vec<String> = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file).unwrap_or_default();
        for (index, line) in source.lines().enumerate() {
            if line.contains(&needle) {
                hits.push(format!(
                    "{}:{}: {}",
                    file.strip_prefix(src_root()).unwrap_or(file).display(),
                    index + 1,
                    line.trim()
                ));
            }
        }
    }

    assert_eq!(
        hits.len(),
        HR_TO_ERP_REFS_BASELINE,
        "hr 对 erp 的引用**回退**了：{} 处（基线 {HR_TO_ERP_REFS_BASELINE}）。\n\
         凭证 → `shared::voucher_port`；期间键 → `shared::period_key`；号段 → `shared::seq`；\n\
         审批闸 → `flow::approval_gate`。命中：\n{}",
        hits.len(),
        hits.join("\n")
    );
    println!(
        "hr → erp 引用：{} 处（基线 {HR_TO_ERP_REFS_BASELINE}）· 扫描 {} 个文件",
        hits.len(),
        files.len()
    );
}

/// ② 中立落点必须**真实存在**：有人在重构里把接缝删了/改名了，这条会先红，
/// 而不是等 `hr` 的编译错误（那时已经有人会「顺手把 import 指回 erp」了）。
#[test]
fn the_neutral_homes_still_exist() {
    for relative in [
        "business/shared/voucher_port.rs",
        "business/shared/period_key.rs",
        "business/shared/seq.rs",
        "business/flow/approval_gate.rs",
    ] {
        let path = src_root().join(relative);
        assert!(path.is_file(), "中立落点不见了：{relative}（hr 靠它解掉对 erp 的依赖）");
    }
}

/// ③ 凭证接缝的**实现**在凭证域里（不是 HR 自己实现，也不是基座实现）。
///
/// 这条钉住的是方向：能力由**持有表的域**实现并注册，消费方只认抽象。
/// 实现如果跑去 `shared/`，那等于基座开始写业务表；跑去 `hr/`，那等于 HR 自己记账。
#[test]
fn voucher_writer_implementation_lives_in_the_voucher_domain() {
    let impl_path = src_root().join("business/erp/voucher/voucher_port_impl.rs");
    assert!(
        impl_path.is_file(),
        "凭证接缝的实现应在 erp/voucher/voucher_port_impl.rs"
    );
    let source = std::fs::read_to_string(&impl_path).expect("读实现");
    assert!(
        source.contains("impl VoucherWriter for ErpVoucherWriter"),
        "实现文件里没有 `impl VoucherWriter`：接缝没被实现（运行期会大声失败）"
    );
    assert!(
        source.contains("pub fn install()"),
        "实现必须提供 install()（bootstrap 的接线入口）"
    );
}
