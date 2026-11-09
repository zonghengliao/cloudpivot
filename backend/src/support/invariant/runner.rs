//! 巡检执行器：跑既有脚本 → 解析 JSON → 落 `sys_invariant_result`。
//!
//! # 为什么是「起子进程跑脚本」而不是在 Rust 里重写检查
//!
//! 闸③（`scripts/check.sh`）判红用的就是 `scripts/verify/erp_invariants.py`。重写一份
//! Rust 版检查，两套口径迟早分叉 —— 看板说绿、闸③说红，比没有看板更糟。这里只搬结果：
//! 子进程的 `--json` 输出就是落库内容，全链路只有一份真相。
//!
//! # 库连接口径（子进程必须连**同一个**库）
//!
//! 脚本读 `NIK_DATABASE_*` 环境变量（默认 127.0.0.1:5432/cloudpivot）。父进程的
//! `cfg().database` 才是权威口径（可能来自 profile 覆盖或环境变量），因此：
//!
//! | 情形 | 做法 |
//! |------|------|
//! | 配置里是**明文**口令 | 显式注入 `NIK_DATABASE_PASSWORD`（子进程不依赖外部脚本） |
//! | 配置里是**密文**口令（`password_encrypted = true`） | 绝不下发密文；依赖父进程继承来的 `NIK_DATABASE_PASSWORD` |
//! | 两者都没有口令 | 直接判 `ERROR` 并写明原因，**不静默假绿**（脚本连不上库会报一堆 BLOCKER） |
//!
//! # 失败语义（与 mod.rs 的约定一致）
//!
//! 脚本跑成但有失败项 → `FAIL`；脚本没跑成 → `ERROR` + `error` 原因；写库失败才往上抛
//! `DbError`。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nik::prelude::*;
use tokio::process::Command;

use crate::support::invariant::dto::CheckItem;
use crate::support::invariant::model::SysInvariantResultModel;

/// 内置任务名（同时是 `sys_job.invoke_target` 与调度器注册名）。
pub const TASK_NAME: &str = "invariant_audit";

/// 内置 cron：每天 02:20。
///
/// 选这个点的理由：与日志清理（03:00-03:50）、审批图快照自检（03:30）错峰，
/// 且早于上班时间 —— 早上打开系统就能看到「昨晚的账有没有问题」。
pub const CRON: &str = "0 20 2 * * *";

/// 覆盖脚本路径的环境变量（测试 / 非标准部署布局用）。
pub const SCRIPT_ENV: &str = "CLOUDPIVOT_INVARIANT_SCRIPT";

/// 覆盖 python 解释器的环境变量（默认 `python3`）。
pub const PYTHON_ENV: &str = "CLOUDPIVOT_PYTHON";

/// 脚本超时上限（秒）。巡检是几十条只读 SQL，正常几秒；300s 是「卡死」与「慢库」的分界。
const SCRIPT_TIMEOUT_SECS: u64 = 300;

/// 相对仓库根的脚本路径（服务以 `backend/` 为工作目录启动）。
const SCRIPT_RELATIVE: &str = "../scripts/verify/erp_invariants.py";

/// 触发方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// 内置 cron 触发。
    Scheduled,
    /// 看板 / 定时任务页手动触发。
    Manual,
}

impl Trigger {
    /// 落库取值（与 `sys_invariant_result.trigger_type` 的 CHECK 约束一致）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "SCHEDULED",
            Self::Manual => "MANUAL",
        }
    }
}

/// 脚本 JSON 结果的解析产物。
#[derive(Debug)]
struct Report {
    /// 检查项全量。
    results: Vec<CheckItem>,
    /// 通过数。
    passed: i32,
    /// 失败数。
    failed: i32,
    /// BLOCKER 数。
    blockers: i32,
    /// WARN 数。
    warns: i32,
}

/// 跑一次巡检并落库。
///
/// # 参数
///
/// - `trigger`：触发方式（落库用）。
///
/// # 返回值
///
/// 写入成功的结果行（含 `ERROR` 行 —— 脚本没跑成也是一条必须被看见的事实）。
///
/// # 异常情况
///
/// 仅当**写库本身**失败时返回 [`DbError`]。
pub async fn run_and_store(trigger: Trigger) -> std::result::Result<SysInvariantResultModel, DbError> {
    let started = Instant::now();
    let run_at = chrono::Utc::now();
    let database_name = cfg().database.name.clone();
    let script = resolve_script();
    let script_path = script.as_ref().map(|p| p.display().to_string()).unwrap_or_default();

    let outcome = match script.as_deref() {
        Some(path) => execute(path).await,
        None => Err(format!(
            "找不到巡检脚本 scripts/verify/erp_invariants.py（工作目录 {}；可用环境变量 {SCRIPT_ENV} 指定绝对路径）",
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "未知".to_string())
        )),
    };

    let duration_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;

    let (status, total, passed, failed, blockers, warns, detail, failures, error) = match outcome {
        Ok(report) => {
            let failed_items = report
                .results
                .iter()
                .filter(|item| !item.ok)
                .cloned()
                .collect::<Vec<_>>();
            let status = if failed_items.is_empty() { "OK" } else { "FAIL" };
            (
                status.to_string(),
                report.results.len() as i32,
                report.passed,
                report.failed,
                report.blockers,
                report.warns,
                Some(to_json(&report.results)),
                Some(to_json(&failed_items)),
                None,
            )
        }
        Err(error) => {
            tracing::error!(%error, "账务不变量巡检未跑成（落 ERROR 行）");
            ("ERROR".to_string(), 0, 0, 0, 0, 0, None, None, Some(error))
        }
    };

    let mut record = SysInvariantResultModel {
        id: uuid::Uuid::now_v7(),
        run_at,
        trigger_type: trigger.as_str().to_string(),
        status: status.clone(),
        total,
        passed,
        failed,
        blockers,
        warns,
        duration_ms,
        database_name: database_name.clone(),
        script_path,
        detail,
        failures,
        error: error.clone(),
        created_at: chrono::Utc::now(),
    };
    record.insert(db()).await?;

    if status == "OK" {
        tracing::info!(
            database = %database_name,
            passed,
            total,
            duration_ms,
            "账务不变量巡检通过"
        );
    } else {
        tracing::warn!(
            database = %database_name,
            status = %status,
            passed,
            total,
            failed,
            blockers,
            warns,
            error = %error.clone().unwrap_or_default(),
            "账务不变量巡检未全绿（结果已落库）"
        );
    }

    Ok(record)
}

/// 供 `task::run_handler` 返回的人类可读摘要。
///
/// # 参数
///
/// - `record`：刚落库的结果行。
///
/// # 返回值
///
/// 形如「巡检完成：40/41 通过，BLOCKER 0，WARN 1，耗时 3210ms（结果 #uuid）」。
pub fn summary(record: &SysInvariantResultModel) -> String {
    match record.status.as_str() {
        "ERROR" => format!(
            "巡检未跑成（已落 ERROR 行 #{}）：{}",
            record.id,
            record.error.clone().unwrap_or_default()
        ),
        _ => format!(
            "巡检完成：{}/{} 通过，BLOCKER {}，WARN {}，耗时 {}ms（结果 #{}）",
            record.passed, record.total, record.blockers, record.warns, record.duration_ms, record.id
        ),
    }
}

/// 定位巡检脚本。
///
/// # 返回值
///
/// 依次尝试：环境变量 `CLOUDPIVOT_INVARIANT_SCRIPT` → `../scripts/verify/erp_invariants.py`
/// （服务以 `backend/` 为工作目录）→ `scripts/verify/erp_invariants.py`（以仓库根为工作目录）
/// → 可执行文件相邻的 `../../scripts/...`。全部不存在时返回 `None`（由调用方落 ERROR 行）。
pub fn resolve_script() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var(SCRIPT_ENV) {
        let path = PathBuf::from(custom.trim());
        if path.is_file() {
            return Some(canonical(path));
        }
    }
    let mut candidates = vec![
        PathBuf::from(SCRIPT_RELATIVE),
        PathBuf::from("scripts/verify/erp_invariants.py"),
    ];
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("../../scripts/verify/erp_invariants.py"));
        candidates.push(dir.join("../../../scripts/verify/erp_invariants.py"));
    }
    candidates.into_iter().find(|path| path.is_file()).map(canonical)
}

/// 尽量转成绝对路径（落库快照用；失败时保留原样）。
fn canonical(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// 起子进程执行脚本并解析 JSON。
///
/// # 参数
///
/// - `script`：脚本绝对路径。
///
/// # 返回值
///
/// 解析成功返回 [`Report`]。
///
/// # 异常情况
///
/// 口令缺失 / 启动失败 / 超时 / 输出解析失败时返回中文原因（由调用方落 `ERROR` 行）。
async fn execute(script: &Path) -> std::result::Result<Report, String> {
    let python = std::env::var(PYTHON_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "python3".to_string());

    let database = &cfg().database;
    let mut cmd = Command::new(&python);
    cmd.arg(script).arg("--json");
    cmd.kill_on_drop(true);
    if let Some(root) = script.parent().and_then(Path::parent) {
        cmd.current_dir(root);
    }

    cmd.env("NIK_DATABASE_HOST", &database.host);
    cmd.env("NIK_DATABASE_NAME", &database.name);
    cmd.env("NIK_DATABASE_USER", &database.user);
    if let Some(port) = database.port {
        cmd.env("NIK_DATABASE_PORT", port.to_string());
    }
    match password_source(&database.password, database.password_encrypted) {
        Ok(Some(pass)) => {
            cmd.env("NIK_DATABASE_PASSWORD", pass);
        }
        // 密文配置：不下发密文，依赖父进程继承的 NIK_DATABASE_PASSWORD
        Ok(None) => {}
        Err(reason) => return Err(reason),
    }

    let output = match tokio::time::timeout(Duration::from_secs(SCRIPT_TIMEOUT_SECS), cmd.output()).await {
        Err(_) => {
            return Err(format!(
                "巡检脚本执行超时（>{SCRIPT_TIMEOUT_SECS}s，已终止）：{}",
                script.display()
            ));
        }
        Ok(Err(e)) => {
            return Err(format!(
                "无法启动巡检脚本（解释器 `{python}`，脚本 {}）：{e}",
                script.display()
            ));
        }
        Ok(Ok(output)) => output,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_report(&stdout).map_err(|reason| {
        format!(
            "{}（退出码 {}；stderr 尾部：{}）",
            reason,
            output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "被信号终止".to_string()),
            tail(&stderr, 300)
        )
    })
}

/// 决定子进程的库口令来源。
///
/// # 参数
///
/// - `password`：配置里的口令原文（可能是密文）。
/// - `encrypted`：该口令是否为密文。
///
/// # 返回值
///
/// - `Ok(Some(pass))`：配置里是明文口令，显式注入；
/// - `Ok(None)`：配置里是密文，依赖父进程继承的环境变量；
/// - `Err(reason)`：两种来源都没有 → 子进程必然连不上库，直接判 ERROR。
fn password_source(password: &str, encrypted: bool) -> std::result::Result<Option<String>, String> {
    if !password.is_empty() && !encrypted {
        return Ok(Some(password.to_string()));
    }
    let inherited = std::env::var("NIK_DATABASE_PASSWORD")
        .ok()
        .filter(|value| !value.is_empty());
    if inherited.is_some() {
        return Ok(None);
    }
    Err(format!(
        "数据库口令未注入：配置里口令为空或为密文（password_encrypted={encrypted}），\
         且进程环境没有 NIK_DATABASE_PASSWORD —— 子进程连不上库，本次判 ERROR（不静默假绿）"
    ))
}

/// 从脚本 stdout 里解析末尾的 JSON 结果段。
///
/// 脚本先打印人读的报告、最后打印 `json.dumps(..., indent=2)`，因此取**最后一个**
/// `{\n  "total"` 起点即可，不依赖前面的中文输出格式。
///
/// # 参数
///
/// - `stdout`：脚本标准输出。
///
/// # 返回值
///
/// 解析成功返回 [`Report`]（计数以 `results` 数组为真相，JSON 顶层计数仅用于校验）。
///
/// # 异常情况
///
/// 找不到 JSON 段 / JSON 非法 / `results` 结构不符时返回中文原因。
fn parse_report(stdout: &str) -> std::result::Result<Report, String> {
    const NEEDLE: &str = "{\n  \"total\"";
    let start = stdout
        .rfind(NEEDLE)
        .ok_or_else(|| "巡检脚本输出里找不到 JSON 结果段（未加 --json 或脚本中途崩溃）".to_string())?;
    let payload = stdout[start..].trim_end();

    let value: JsonValue = serde_json::from_str(payload).map_err(|e| format!("解析巡检 JSON 失败：{e}"))?;
    let raw_results = value
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "巡检 JSON 缺少 results 数组".to_string())?;
    let results: Vec<CheckItem> = serde_json::from_value(JsonValue::Array(raw_results.clone()))
        .map_err(|e| format!("巡检结果结构与 CheckItem 不符（脚本被改过？）：{e}"))?;
    if results.is_empty() {
        return Err("巡检结果的 results 为空：脚本一条检查都没跑（疑似连不上库或参数错误）".to_string());
    }

    let total = results.len() as i32;
    let passed = results.iter().filter(|item| item.ok).count() as i32;
    let failed = total - passed;
    let blockers = results
        .iter()
        .filter(|item| !item.ok && item.level.eq_ignore_ascii_case("BLOCKER"))
        .count() as i32;
    let warns = results
        .iter()
        .filter(|item| !item.ok && item.level.eq_ignore_ascii_case("WARN"))
        .count() as i32;

    // JSON 顶层计数与 results 不一致 → 以 results 为准，但记警告（脚本自身有 bug 的信号）
    let declared = value.get("total").and_then(|v| v.as_i64());
    if let Some(declared) = declared
        && declared != i64::from(total)
    {
        tracing::warn!(
            declared,
            actual = total,
            "巡检 JSON 顶层 total 与 results 长度不一致，以 results 为准"
        );
    }

    Ok(Report {
        results,
        passed,
        failed,
        blockers,
        warns,
    })
}

/// 序列化检查项数组为 JSONB 值（序列化不可能失败，失败则退化为 Null）。
fn to_json(items: &[CheckItem]) -> JsonValue {
    serde_json::to_value(items).unwrap_or(JsonValue::Null)
}

/// 取字符串尾部若干字符（错误消息里保留最有信息量的末尾）。
fn tail(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= limit {
        return trimmed.to_string();
    }
    chars[chars.len() - limit..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 脚本输出的真实形态：中文报告 + 末尾缩进 JSON。解析必须只看 JSON 段。
    #[test]
    fn parses_trailing_json_block() {
        let stdout = concat!(
            "==========\n云枢 ERP 不变量审计套件\n==========\n",
            "  ✅ [会计] A1 借贷平衡\n  🚨 [账账] B2 总账明细\n",
            "\n==========\n结果：1/2 通过   🚨 BLOCKER 1   ⚠️ WARN 0\n==========\n",
            "\n",
            "{\n  \"total\": 2,\n  \"passed\": 1,\n  \"blockers\": 1,\n  \"warns\": 0,\n",
            "  \"results\": [\n",
            "    {\"name\": \"A1 借贷平衡\", \"group\": \"会计\", \"level\": \"BLOCKER\", \"ok\": true, \"detail\": \"\"},\n",
            "    {\"name\": \"B2 总账明细\", \"group\": \"账账\", \"level\": \"BLOCKER\", \"ok\": false, \"detail\": \"差 12.50\"}\n",
            "  ]\n}\n"
        );
        let report = parse_report(stdout).expect("应解析成功");
        assert_eq!(report.results.len(), 2);
        assert_eq!(report.passed, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.blockers, 1);
        assert_eq!(report.warns, 0);
        assert_eq!(report.results[1].detail, "差 12.50");
    }

    /// 没有 JSON 段（脚本崩在中间）必须报错，而不是当成「0 条检查全通过」。
    #[test]
    fn rejects_output_without_json_block() {
        let err = parse_report("boom: connection refused").expect_err("应报错");
        assert!(err.contains("找不到 JSON 结果段"), "实际：{err}");
    }

    /// 空 results 不能算 OK：连不上库时脚本会一条检查都跑不出来。
    #[test]
    fn rejects_empty_results() {
        let stdout =
            "{\n  \"total\": 0,\n  \"passed\": 0,\n  \"blockers\": 0,\n  \"warns\": 0,\n  \"results\": []\n}\n";
        let err = parse_report(stdout).expect_err("应报错");
        assert!(err.contains("results 为空"), "实际：{err}");
    }
}
