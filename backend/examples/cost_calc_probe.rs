//! 成本核算 calc 六段流水线的**演练探针**（COST-B4-5 · `scripts/verify/cost_e2e.sh` 调用）。
//!
//! ```bash
//! NIK_DATABASE_NAME=cost_e2e_drill cargo run --quiet --example cost_calc_probe -- 2026-11
//! ```
//!
//! 输出（stdout，一行一个 marker，脚本据此断言；诊断信息一律走 stderr）：
//!
//! ```text
//! CALC_STAGE=MATERIAL <summary_line>
//! CALC_STAGE=LABOR    <summary_line>
//! CALC_STAGE=OVERHEAD <summary_line>
//! CALC_STAGE=RECONCILE <summary_line>
//! CALC_STAGE=ALLOC    <summary_line>
//! CALC_STAGE=ITEM     <summary_line>
//! CALC_OK period=<yyyy-MM> batch=<batch_no> items=<n> material=<m> labor=<l> overhead=<o>
//!            cost=<c> finished=<f> wip=<w>
//! ```
//!
//! 失败时 stderr 打 `CALC_FAIL stage=<段> code=<code> msg=<报文>`，退出码 1。
//!
//! # 为什么是 example 而不是 `POST /cost-period/calc`
//!
//! 设计 §5.2 #27 的编排接口（`POST /business/erp/cost-period/calc`）**尚未落地**（属编排卡），
//! 六段只有库内函数（`cost_calc_material` / `cost_calc_labor` / … 每个模块头都写着「没有 HTTP
//! 入口，这是有意的」）。演练要跑**真实代码**而不是手写归集行，所以这里按各模块文档约定的
//! 「编排方在一次『逐段清 + 重灌』里按序调用」把六段串起来，跑的是与将来 #27 完全相同的函数。
//!
//! 为什么用 `[[example]]` 而不是 `[[bin]]`：
//!
//! * 不进生产二进制（`cargo build --bin cloudpivot` 不含本文件），不新增 HTTP/权限面；
//! * 与 `c7_rollback_probe` 同一套做法（探针由 `scripts/verify/*` 调用）。
//!
//! 段序不可交换（`cost_calc_item` 模块头的「调用顺序契约」）：料 → 工 → 费 → 池对账 →
//! 分摊 → 计算单。分摊必须早于计算单，否则制造费用进不了对象。

use cloudpivot::business::erp::cost_alloc_calc_dto;
use cloudpivot::business::erp::cost_calc_alloc;
use cloudpivot::business::erp::cost_calc_item;
use cloudpivot::business::erp::cost_calc_labor;
use cloudpivot::business::erp::cost_calc_material;
use cloudpivot::business::erp::cost_calc_overhead;
use cloudpivot::business::erp::cost_calc_reconcile;
use cloudpivot::business::erp::cost_reconcile_dto::Stage;
use nik::prelude::*;

/// 连接串：`NIK_DATABASE_URL` > `NIK_DATABASE_{USER,PASSWORD,HOST,PORT,NAME}` > 默认 alun/alun。
fn database_url() -> String {
    if let Ok(url) = std::env::var("NIK_DATABASE_URL")
        && !url.trim().is_empty()
    {
        return url;
    }
    let pick = |key: &str, fallback: &str| {
        std::env::var(key)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| fallback.to_string())
    };
    let user = pick("NIK_DATABASE_USER", "alun");
    let password = pick("NIK_DATABASE_PASSWORD", "alun");
    let host = pick("NIK_DATABASE_HOST", "127.0.0.1");
    let port = pick("NIK_DATABASE_PORT", "5432");
    let name = pick("NIK_DATABASE_NAME", "cost_e2e_drill");
    format!("postgres://{user}:{password}@{host}:{port}/{name}")
}

/// 打一段的结论；`stage` 是脚本断言用的固定词。
fn stage_line(stage: &str, line: &str) {
    println!("CALC_STAGE={stage} {line}");
}

/// 失败出口：stderr 打可定位的一段（段名 + 错误码 + 报文），退出码 1。
fn fail(stage: &str, error: &ApiError) -> ! {
    eprintln!("CALC_FAIL stage={stage} code={} msg={}", error.code, error.msg);
    std::process::exit(1);
}

#[tokio::main]
async fn main() {
    let period = match std::env::args().nth(1) {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => {
            eprintln!("用法：cost_calc_probe <yyyy-MM>（期间码）");
            std::process::exit(2);
        }
    };
    let url = database_url();
    let mut config = DatabaseConfig::from_url(&url);
    // 一次性探针：禁止池里留活连接，进程退出即断开（不留 idle 会话挡住脚本清场）。
    config.pool.max_lifetime = Some(std::time::Duration::ZERO);
    config.pool.idle_timeout = Some(std::time::Duration::ZERO);
    let connection = match Db::connect_with(&url, config).await {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("探针连库失败：{error}");
            std::process::exit(3);
        }
    };
    set_db(connection).expect("设置全局连接");

    // ① 料归集
    let material = match cost_calc_material::collect_material(&period, None).await {
        Ok(outcome) => outcome,
        Err(error) => fail("MATERIAL", &error),
    };
    stage_line("MATERIAL", &format!("{material:?}"));

    // ② 工归集
    let labor = match cost_calc_labor::collect_labor(&period, None).await {
        Ok(outcome) => outcome,
        Err(error) => fail("LABOR", &error),
    };
    stage_line("LABOR", &format!("{labor:?}"));

    // ③ 费归集（池行 + 直接行）
    let overhead = match cost_calc_overhead::collect_overhead(&period, None).await {
        Ok(outcome) => outcome,
        Err(error) => fail("OVERHEAD", &error),
    };
    stage_line("OVERHEAD", &format!("{overhead:?}"));

    // ④ 池 ↔ 4101 借方对账（calc 阶段：手工台账的凭证在结算时才生成，故只认已有账面背书的路）
    let reconcile = match cost_calc_reconcile::reconcile_pool_with_ledger(&period, Stage::Calc).await {
        Ok(outcome) => outcome,
        Err(error) => fail("RECONCILE", &error),
    };
    stage_line("RECONCILE", &reconcile.summary_line());

    // ⑤ 分摊（必须早于 ⑥，否则制造费用进不了对象）
    let alloc = match cost_calc_alloc::allocate_overhead(&period, None).await {
        Ok(outcome) => outcome,
        Err(error) => fail("ALLOC", &error),
    };
    stage_line("ALLOC", &alloc.summary_line());

    // ⑥ 约当产量 + 成本计算单
    let items = match cost_calc_item::calc_items(&period, None).await {
        Ok(outcome) => outcome,
        Err(error) => fail("ITEM", &error),
    };
    stage_line("ITEM", &items.summary_line());

    // 校验分摊结果与池守恒（`cost_alloc_calc_dto` 的记账口径）：
    // 有计算单就必须有分摊（除非池本来就是 0），这里只把关键数字交给调用方。
    let _: &cost_alloc_calc_dto::AllocRunOutcome = &alloc;
    println!(
        "CALC_OK period={} batch={} items={} material={} labor={} overhead={} cost={} finished={} wip={}",
        items.period_code,
        items.batch_no,
        items.items.len(),
        items.total_material,
        items.total_labor,
        items.total_overhead,
        items.total_cost,
        items.total_finished,
        items.total_wip,
    );
}
