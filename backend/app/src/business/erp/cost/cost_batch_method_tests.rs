//! 分批法「全链路单测」（成本核算 · M5-06）—— **含部分完工**。
//!
//! 本文件不新增任何业务能力：它把 M5-02（归集表）/ M5-03（归集到批次）/
//! M5-04（完工结转）/ M5-05（在制余额）四张卡的**接口串起来跑一遍**，钉住
//! 「分批法到底怎么算」这件事在跨卡时不被哪一张卡悄悄改掉。
//!
//! # 为什么要有这一张卡（四张卡各自都有单测了）
//!
//! 四张卡各自的用例都用**直接 SQL 造夹具**验证自己的读口径/写口径；那种用例证明
//! 「这一卡按契约工作」，**不能**证明「A 卡写进去的、B 卡算出来的是同一个数」。
//! 本卡的所有真库用例都走**真实接口**：`collect` → `finish` → `balance` / `ledger`，
//! 夹具里的金额是「领料单 / 工时台账 / 手工费用」三张来源单据的金额，
//! 因此断言的是**链路口径**（Σ来源 = Σ归集 = Σ结转 + Σ在制），不是某个函数。
//!
//! # 分批法的核心口径（本卡逐条钉住）
//!
//! ```text
//! 批没完（wip_qty > 0）  ⇒ 一分不转，累计归集全额留在在制（**不做约当产量分摊**）
//! 批完了（wip_qty = 0）  ⇒ 本次结转 = 累计归集 − 已结转；余额归零
//! 单位成本               = 本次结转 / 累计完工数量（round 6）
//! ```
//!
//! 「部分完工」在分批法里是一个**正常但不可结转**的形态：车间已交了一部分货
//! （`completed_qty > 0`），批还在做（`wip_qty > 0`）。这时成本**一分不转**，
//! 因为分批法的核算单位是「批」——按产出比例摊一刀会让「这批到底花了多少」变成猜的。
//! 要按产出/约当分摊的是**期间结算**（calc 约当产量口径），不是这张卡。
//! 所以本卡的用例断言的是「**部分完工 ⇒ 余额不缩水、结转被拦、提示走期间结算**」，
//! 而不是「部分完工 ⇒ 按比例转出」。
//!
//! # 覆盖的验收项
//!
//! ## A. 跨卡契约（纯逻辑，不连库 —— 没配 `ERP_DB_URL` 也能绿）
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 1 | [`contract::t1_chain_uses_exactly_one_read_code_and_one_write_code`] | 四张卡只用两个权限码（读 `cost-object:list` / 写 `cost-period:calc`），没有第三个码 |
//! | 2 | [`contract::t2_partial_completion_blocks_transfer_for_any_wip_left`] | `wip_qty > 0` 一律拦（产出为 0 / 部分 / 已够计划 / 超产 四种形态），报文写明「不做约当产量分摊」并把人引向期间结算 |
//! | 3 | [`contract::t3_transfer_is_the_difference_not_an_output_ratio`] | 结转 = 累计 − 已转（`plan_transfer` 连「计划数量」都不吃）；写侧夹零、报表侧不夹 |
//! | 4 | [`contract::t4_balance_identity_closes_on_partial_completion`] | 部分完工 ⇒ 余额 = 全额；整批完工 ⇒ 余额归零；任一时点「已转 + 余额 = 归集」 |
//! | 5 | [`contract::t5_element_literals_and_scales_are_single_sourced`] | 料/工/费字面量、金额 4 位、单位成本 6 位四卡同源 |
//!
//! ## B. 真库全链路（`ERP_DB_URL` 开关；不设则全部 SKIP）
//!
//! | # | 用例 | 钉住什么 |
//! |---|---|---|
//! | 6 | [`db::c1_partial_completion_holds_cost_in_wip_until_batch_finishes`] | **主链路**：P1 归集 → 归集表/余额对得上 → P2 部分完工（400/1000）⇒ 结转 409 且零行写入、余额不缩水 → P2 继续投料 → P3 整批完工 ⇒ 一次转全额、余额归零、台账三期滚动、Σ归集 = Σ结转 |
//! | 7 | [`db::c2_two_batches_finished_and_partial_reconcile_at_the_totals`] | A 批完工转出 + B 批部分完工留余额：表尾 `batch_count / wip_batch_count`、逐要素守恒、B 的余额是全额（不是 37.5%） |
//! | 8 | [`db::c3_late_cost_after_finish_moves_only_the_delta`] | 结转后迟到的真成本 ⇒ 余额 > 0 + `FINISHED_NOT_TRANSFERRED`，二次结转只走差额（`transfer_seq = 2`）后归零 |
//! | 9 | [`db::c4_repeat_finish_and_reads_are_idempotent`] | 重复结转 `inserted = false` 不写空单；重复归集 0 新增；余额表 / 台账两次读数逐字相同 |
//! | 10 | [`db::c5_sheet_period_in_matches_balance_and_detail_sums`] | 同一份数据的三种看法：归集表本期合计 == 余额表本期投入 == 明细逐笔之和（跨卡同数） |
//! | 11 | [`db::c6_collected_stock_out_unblocks_the_chain`] | 卫生闸「已过账未归集领料」的**判据形状**：未归集 ⇒ 拦；走真接口归集过 ⇒ 放行（回归：判据曾按出库单头 id 找归集行，而写入侧落的是明细行 id ⇒ 归集过也拦，批永远结不掉） |
//! | 12 | [`db::zz_teardown_leaves_zero_residue`] | 清场后本卡造的行全 0（对象 / 归集 / 结转 / 工时 / 费用 / 领料 / 主数据） |
//!
//! # 夹具纪律
//!
//! * 本卡所有真库用例**走真实接口**造数（领料单 / 工时台账 / 手工费用 → `collect`
//!   → `finish`），只有「成本对象抬头」与三张来源单据是直接 SQL 造的（接口造不出来）；
//! * 每个夹具独占一个期间槽（`year_seq(2700, 60)`，期间取该年 01/02/03），
//!   建前先清该槽上一次运行的残留；
//! * 批次号 / 单号 / 物料编码全部带唯一后缀，**列表断言一律用 `keyword` 收窄到本夹具**，
//!   绝不按「全表有多少行」断言（别的卡的夹具也在同一个库里）；
//! * 金额断言用 `Decimal` 解析后比较，避免 `1000` vs `1000.0000` 的标度假红。

// ==================== A. 跨卡契约（纯逻辑） ====================

#[cfg(test)]
mod contract {
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    use crate::business::erp::cost::cost_batch_collect_dto as collect_dto;
    use crate::business::erp::cost::cost_batch_finish_dto::{self as finish_dto, Amounts, FinishFacts};
    use crate::business::erp::cost::cost_batch_wip_dto as wip_dto;
    use crate::business::erp::cost::cost_collect_sheet_dto as sheet_dto;
    use crate::business::erp::cost::cost_object_dto;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap_or_else(|_| panic!("金额字面量无法解析：{text}"))
    }

    fn amounts(material: &str, labor: &str, overhead: &str) -> Amounts {
        Amounts {
            material: dec(material),
            labor: dec(labor),
            overhead: dec(overhead),
        }
    }

    /// 造一份「查库后的事实」（与 `cost_batch_finish::gather` 同形状），只填判定关心的项。
    fn facts(status: &str, wip: &str, completed: &str, collected: Amounts) -> FinishFacts {
        FinishFacts {
            object_status: status.to_string(),
            wip_qty: dec(wip),
            completed_qty: dec(completed),
            collected,
            transferred: Amounts::default(),
            settled_items: 0,
            draft_labor: 0,
            uncollected_stock_out: 0,
            unposted_stock_out: 0,
            locked_periods: Vec::new(),
            period_batch: None,
        }
    }

    fn codes(blockers: &[finish_dto::FinishBlocker]) -> Vec<&'static str> {
        blockers.iter().map(|blocker| blocker.code).collect()
    }

    // ==================== 1. 权限码只有一个读 + 一个写 ====================

    /// 四张卡（M5-02/03/04/05）的权限码必须收敛到两个：读 `cost-object:list`、
    /// 写 `cost-period:calc`。多出一个码就意味着多一条指向 M5-07 页面的死菜单
    /// （菜单先于页面出现 = 侧栏点开 404），或者「能看数」与「能改数」被拆成了两种人。
    #[test]
    fn t1_chain_uses_exactly_one_read_code_and_one_write_code() {
        let reads = [
            sheet_dto::PERM_LIST,
            collect_dto::PERM_READ,
            finish_dto::PERM_READ,
            wip_dto::PERM_READ,
        ];
        for code in reads {
            assert_eq!(code, "business:erp:cost-object:list", "读口径四卡必须同一个码");
        }
        assert_eq!(collect_dto::PERM_COLLECT, "business:erp:cost-period:calc");
        assert_eq!(
            finish_dto::PERM_FINISH,
            collect_dto::PERM_COLLECT,
            "归集与结转是同一个写动作的两种形态（都改成本数），不能拆成两个码"
        );

        let mut all: Vec<&'static str> = Vec::new();
        all.extend(sheet_dto::PERMISSIONS);
        all.extend(collect_dto::PERMISSIONS);
        all.extend(finish_dto::PERMISSIONS);
        all.extend(wip_dto::PERMISSIONS);
        all.sort_unstable();
        all.dedup();
        assert_eq!(
            all,
            ["business:erp:cost-object:list", "business:erp:cost-period:calc"],
            "全链路只允许这两个码；新增码要连带页面（M5-07）一起落地"
        );
    }

    // ==================== 2. 部分完工一律不结转 ====================

    /// **本卡的核心断言**：只要 `wip_qty > 0`，无论产出是 0、部分、已够计划还是超产，
    /// 结转一律被拦，且报文必须说清「分批法不按约当产量分摊」并把人引向期间结算。
    ///
    /// 反例（会被这条用例抓住）：把在制判定改成「完工数 ≥ 计划数就放行」——
    /// 那会让超产批带着在制把成本全转出去，在制数量还在、成本已经没了。
    #[test]
    fn t2_partial_completion_blocks_transfer_for_any_wip_left() {
        // (累计完工, 期末在制)：0/1000 开工未产出、400/600 部分完工、
        // 999/1 只差一件、1200/300 超产且仍有在制
        for (completed, wip) in [("0", "1000"), ("400", "600"), ("999", "1"), ("1200", "300")] {
            let blocked = finish_dto::evaluate_finish(&facts("OPEN", wip, completed, amounts("2000", "1000", "600")));
            assert!(
                codes(&blocked).contains(&finish_dto::BLOCK_WIP_NOT_ZERO),
                "completed={completed} wip={wip} 必须被在制拦住，实际：{:?}",
                codes(&blocked)
            );
            if completed == "0" {
                assert!(
                    codes(&blocked).contains(&finish_dto::BLOCK_NO_FINISHED_QTY),
                    "一条都没产出时还要点名「没有完工转出可结」"
                );
            } else {
                assert!(
                    !codes(&blocked).contains(&finish_dto::BLOCK_NO_FINISHED_QTY),
                    "已经交了 {completed} 件就别再报「没产出」"
                );
            }
            let block = blocked
                .iter()
                .find(|blocker| blocker.code == finish_dto::BLOCK_WIP_NOT_ZERO)
                .expect("在制拦截");
            assert!(
                block.message.contains("不做约当产量分摊"),
                "报文必须写明分批法不做约当分摊：{}",
                block.message
            );
            assert!(
                block.fix.contains("期间结算"),
                "修法必须把人引向期间结算（约当产量那条路）：{}",
                block.fix
            );
        }

        // 对照：整批完工（wip = 0）即使完工数没到计划数（400/1000）也不再有在制拦截
        // —— 拦的是「还有在制」，不是「产出没到计划数」。
        let finished = finish_dto::evaluate_finish(&facts("OPEN", "0", "400", amounts("2000", "0", "0")));
        assert!(!codes(&finished).contains(&finish_dto::BLOCK_WIP_NOT_ZERO));
        assert!(
            finished.is_empty(),
            "整批完工 + 有成本 ⇒ 应放行：{:?}",
            codes(&finished)
        );

        // 拦截码同源：M5-04 不自己造一个字面量（否则 M5-05 的警告与它对不上）
        assert_eq!(finish_dto::BLOCK_WIP_NOT_ZERO, cost_object_dto::BLOCK_WIP_NOT_ZERO);
    }

    // ==================== 3. 结转是差额，不是比例 ====================

    /// `plan_transfer` **连「计划数量」这个入参都没有** —— 这在签名层面就决定了
    /// 分批法不可能按产出比例摊。用例把这个签名事实变成可执行的断言。
    #[test]
    fn t3_transfer_is_the_difference_not_an_output_ratio() {
        let collected = amounts("2000", "1000", "600"); // 合计 3600

        // 首次结转：无论计划多少件，转的都是累计全额
        let first = finish_dto::plan_transfer(&collected, &Amounts::default(), dec("400"), 0);
        assert_eq!(first.transfer, collected, "首次结转 = 累计全额（不是 400/1000 的 40%）");
        assert_eq!(first.unit_cost, dec("9.000000"), "3600 / 已完工 400 = 9（round 6）");
        assert_eq!(first.transfer_no, 1);
        assert!(first.transferable);

        // 差额结转：已转过一部分，只走余量
        let delta = finish_dto::plan_transfer(&collected, &amounts("1000", "0", "0"), dec("400"), 1);
        assert_eq!(delta.transfer, amounts("1000", "1000", "600"));
        assert_eq!(delta.transfer_no, 2);
        assert_eq!(delta.cumulative_unit_cost, dec("9.000000"), "累计单位成本按累计结转算");

        // 写侧**逐要素**夹零：某要素「已结转 > 累计归集」时那一项归 0，其余要素照常转
        let partially_dirty = finish_dto::plan_transfer(&collected, &amounts("2500", "1200", "0"), dec("400"), 3);
        assert_eq!(
            partially_dirty.transfer,
            amounts("0", "0", "600"),
            "只夹超出的要素（料 / 工归 0），费还要转 600"
        );
        assert!(partially_dirty.transferable, "只要还有一项可转就照转");

        // 三项全部超出 ⇒ 无钱可转（宁可少转，不产生负结转）
        let dirty = finish_dto::plan_transfer(&collected, &amounts("2500", "1200", "700"), dec("400"), 3);
        assert_eq!(dirty.transfer, Amounts::default(), "逐要素夹零后无可转");
        assert!(!dirty.transferable);
        assert_eq!(dirty.transfer_no, 4, "序号照样往前走，便于看清「试过但没转」");
    }

    // ==================== 4. 余额守恒（含部分完工） ====================

    /// 分批法的余额是推出来的：`余额 = 累计归集 − 累计结转`（逐要素）。
    /// 部分完工时一分没转 ⇒ 余额就是全额；整批完工全额转出 ⇒ 归零。
    /// 报表侧**不夹零**（负数 = 结转后归集被作废，夹掉就把错账藏起来了）。
    #[test]
    fn t4_balance_identity_closes_on_partial_completion() {
        let collected = amounts("2000", "1000", "600");

        // 部分完工（产出 400 / 在制 600）：一分没转 ⇒ 余额是 3600，不是 60% / 40% 里的任何一个
        let partial = wip_dto::balance_of(&collected, &Amounts::default());
        assert_eq!(partial, collected, "部分完工不缩水：成本全留在在制");

        // 整批完工后一次转出全额 ⇒ 余额归零
        let done = wip_dto::balance_of(&collected, &collected);
        assert!(done.is_zero(), "整批结转后余额必须归零");
        assert!(!wip_dto::any_negative(&done));

        // 任一时点：已转 + 余额 = 累计归集（逐要素，不多不少）
        let transferred = amounts("1200", "600", "100");
        let balance = wip_dto::balance_of(&collected, &transferred);
        assert_eq!(wip_dto::plus(&transferred, &balance), collected);

        // 报表不夹零：结转超过归集 ⇒ 负余额，且必须被 `any_negative` 看见
        let negative = wip_dto::balance_of(&collected, &amounts("2500", "4500", "0"));
        assert_eq!(negative.material, dec("-500"));
        assert_eq!(negative.labor, dec("-3500"));
        assert_eq!(negative.overhead, dec("600"));
        assert!(wip_dto::any_negative(&negative), "负余额必须能被警告判据看见");
    }

    // ==================== 5. 字面量与标度同源 ====================

    #[test]
    fn t5_element_literals_and_scales_are_single_sourced() {
        assert_eq!(collect_dto::ELEMENT_MATERIAL, "MATERIAL");
        assert_eq!(collect_dto::ELEMENT_LABOR, "LABOR");
        assert_eq!(collect_dto::ELEMENT_OVERHEAD, "OVERHEAD");
        assert_eq!(finish_dto::ELEMENT_MATERIAL, collect_dto::ELEMENT_MATERIAL);
        assert_eq!(finish_dto::ELEMENT_LABOR, collect_dto::ELEMENT_LABOR);
        assert_eq!(finish_dto::ELEMENT_OVERHEAD, collect_dto::ELEMENT_OVERHEAD);

        assert_eq!(finish_dto::AMOUNT_SCALE, 4, "金额 NUMERIC(18,4)");
        assert_eq!(wip_dto::AMOUNT_SCALE, 4, "余额表与结转同标度");
        assert_eq!(finish_dto::UNIT_COST_SCALE, 6, "单位成本 NUMERIC(18,6)");
        assert_eq!(finish_dto::unit_cost_of(dec("100"), dec("3")), dec("33.333333"));
        assert_eq!(
            finish_dto::unit_cost_of(dec("100"), Decimal::ZERO),
            Decimal::ZERO,
            "数量为 0 时单位成本是 0，不造 NaN 语义"
        );
    }
}

// ==================== B. 真库全链路 ====================

#[cfg(test)]
mod db {
    use nik::prelude::rust_decimal::Decimal;
    use nik::prelude::*;
    use nik::uuid::Uuid;
    use std::str::FromStr;
    use tokio::sync::OnceCell;

    use crate::business::erp::cost::cost_batch_collect::{self as batch_collect};
    use crate::business::erp::cost::cost_batch_collect_dto::BatchCollectReq;
    use crate::business::erp::cost::cost_batch_finish::{self as finish_api};
    use crate::business::erp::cost::cost_batch_finish_dto::{
        self as finish_dto, BatchFinishPreviewRes, BatchFinishQuery, BatchFinishReq,
    };
    use crate::business::erp::cost::cost_batch_wip_dto::{
        self as wip_dto, WipBalanceRes, WipLedgerQuery, WipLedgerRes, WipQuery, WipRowRes,
    };
    use crate::business::erp::cost::cost_batch_wip_handler as wip_handler;
    use crate::business::erp::cost::cost_collect_sheet_dto::{
        SheetDetailQuery, SheetDetailRes, SheetQuery, SheetRes, SheetRowRes,
    };
    use crate::business::erp::cost::cost_collect_sheet_handler as sheet_handler;
    use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
    use cloudpivot_kernel::business::shared::scope::ScopeKind;

    /// 连库（进程内只做一次初始化）。`false` = 没配 `ERP_DB_URL`，用例直接跳过。
    async fn ensure_db() -> bool {
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
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 金额字符串 → `Decimal`（出参一律字符串，避免标度假红）。
    fn amount(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap_or_else(|_| panic!("金额字符串无法解析：{text}"))
    }

    struct Fixture {
        suffix: String,
        /// 三个连续期间 01 / 02 / 03（链路的期间轴）。
        periods: [String; 3],
        dept: Uuid,
        dept_name: String,
        product: Uuid,
        product_code: String,
        warehouse: Uuid,
        warehouse_code: String,
        unit: Uuid,
    }

    impl Fixture {
        async fn build() -> Self {
            let suffix = crate::test_seed::uniq_suffix();
            let year = crate::test_seed::year_seq(2700, 60);
            let periods = [
                format!("{year:04}-01"),
                format!("{year:04}-02"),
                format!("{year:04}-03"),
            ];
            Self::reset(&periods).await;

            let dept = cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await;
            let dept_name: String = db()
                .scalar_with_params(
                    "SELECT dept_name FROM sys_dept WHERE id = ?::uuid",
                    vec![Value::from(&dept)],
                )
                .await
                .expect("取部门名");
            let unit = crate::test_seed::ensure_unit().await;
            let product_code = format!("M506-MAT-{suffix}");
            let product = crate::test_seed::ensure_material_kind(
                &product_code,
                &format!("M5-06 夹具产出品 {suffix}"),
                "FINISHED",
            )
            .await;
            let warehouse_code = format!("M56W{}", crate::test_seed::tail_code(&suffix, 8));
            let warehouse =
                crate::test_seed::ensure_warehouse(&warehouse_code, &format!("M5-06 夹具仓 {suffix}")).await;

            Self {
                suffix,
                periods,
                dept,
                dept_name,
                product,
                product_code,
                warehouse,
                warehouse_code,
                unit,
            }
        }

        fn super_principal(&self) -> ScopedPrincipal {
            ScopedPrincipal::for_test(Uuid::now_v7(), Some(self.dept), true, ScopeKind::All, Vec::new(), false)
        }

        /// 本夹具的尾巴（对象号 / 单号里都带它，列表断言靠它收窄）。
        fn tail(&self) -> String {
            crate::test_seed::last_chars(&self.suffix, 8)
        }

        /// 关键字收窄到**本夹具的某一个批次**。
        fn tag(&self, name: &str) -> String {
            format!("{name}-{}", self.tail())
        }

        /// 关键字收窄到**本夹具的全部批次**。
        fn tag_all(&self) -> String {
            self.tail()
        }

        fn object_no(&self, name: &str) -> String {
            format!("CB-M506-{}", self.tag(name))
        }

        fn day(&self, period_index: usize, day: u32) -> String {
            format!("{}-{day:02}", self.periods[period_index])
        }

        /// 造成本对象（产量 / 在制 / 状态由用例给）。
        async fn object(&self, name: &str, status: &str, plan: &str, completed: &str, wip: &str) -> Uuid {
            let id = Uuid::now_v7();
            let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
            let start_date = format!("{}-01", self.periods[0]);
            db().exec_sql(
                format!(
                    "INSERT INTO biz_cost_object (id, object_no, object_type, product_id, lot_no, \
                                                  target_warehouse_id, plan_qty, completed_qty, wip_qty, \
                                                  workshop_dept_id, workshop_dept_name, start_date, \
                                                  status, closed_at, created_at, updated_at) \
                     VALUES (?::uuid, ?, 'BATCH', ?::uuid, ?, ?::uuid, ?::text::numeric, \
                             ?::text::numeric, ?::text::numeric, ?::uuid, ?, '{start_date}'::text::date, ?, {closed_at}, now(), now())"
                ),
                vec![
                    Value::from(&id),
                    Value::Text(self.object_no(name)),
                    Value::from(&self.product),
                    Value::Text(format!("LOT-M506-{name}-{}", self.tail())),
                    Value::from(&self.warehouse),
                    Value::Text(plan.to_string()),
                    Value::Text(completed.to_string()),
                    Value::Text(wip.to_string()),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造成本对象夹具");
            id
        }

        /// 产量与在制（车间在「产量与在制」里填的那两个数）。
        async fn set_progress(&self, object: Uuid, completed: &str, wip: &str) {
            db().exec_sql(
                "UPDATE biz_cost_object SET completed_qty = ?::text::numeric, wip_qty = ?::text::numeric, \
                        updated_at = now() WHERE id = ?::uuid"
                    .to_string(),
                vec![
                    Value::Text(completed.to_string()),
                    Value::Text(wip.to_string()),
                    Value::from(&object),
                ],
            )
            .await
            .expect("改产量与在制");
        }

        /// 造一张已过账领料单（一行明细），返回明细 id。
        async fn issue(&self, object: Uuid, date: &str, qty: &str, amount: &str) -> Uuid {
            let header = Uuid::now_v7();
            let out_no = format!(
                "M506OUT-{}",
                crate::test_seed::last_chars(&header.simple().to_string(), 10)
            );
            db().exec_sql(
                "INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, \
                                            total_qty, total_amount, reason, cost_object_id, created_at, updated_at) \
                 VALUES (?::uuid, ?, 'ISSUE', ?::uuid, ?::text::date, 'POSTED', 0, 0, 'M5-06 夹具', ?::uuid, now(), now())"
                    .to_string(),
                vec![
                    Value::from(&header),
                    Value::Text(out_no),
                    Value::from(&self.warehouse),
                    Value::Text(date.to_string()),
                    Value::from(&object),
                ],
            )
            .await
            .expect("造领料单头夹具");
            let item = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_stock_out_item (id, stock_out_id, line_no, material_id, unit_id, qty, \
                                                  unit_cost, amount, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, ?::text::numeric, \
                         ?::text::numeric, ?::text::numeric, 'M5-06 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&item),
                    Value::from(&header),
                    Value::from(&self.product),
                    Value::from(&self.unit),
                    Value::Text(qty.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await
            .expect("造领料明细夹具");
            item
        }

        /// 造一条**已确认**的工时台账行（`amount = round(hours × rate, 4)`，库层 CHECK 要求）。
        async fn labor(&self, object: Uuid, period: &str, day: &str, hours: &str, rate: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_labor (id, period_code, work_date, cost_object_id, employee_name, \
                                             dept_id, dept_name, hours, machine_hours, rate, amount, \
                                             source_type, status, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::text::date, ?::uuid, ?, ?::uuid, ?, \
                         ?::text::numeric, 0, ?::text::numeric, \
                         round(?::text::numeric * ?::text::numeric, 4), \
                         'MANUAL', 'CONFIRMED', 'M5-06 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(period.to_string()),
                    Value::Text(day.to_string()),
                    Value::from(&object),
                    Value::Text(format!("M5-06 人员 {}", self.tail())),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(hours.to_string()),
                    Value::Text(rate.to_string()),
                    Value::Text(hours.to_string()),
                    Value::Text(rate.to_string()),
                ],
            )
            .await
            .expect("造工时台账夹具");
            id
        }

        /// 造一条手工费用台账行（挂在批次上 ⇒ 直接归集，不进池）。
        async fn overhead(&self, object: Uuid, period: &str, amount_text: &str) -> Uuid {
            let id = Uuid::now_v7();
            db().exec_sql(
                "INSERT INTO biz_cost_overhead (id, period_code, cost_object_id, dept_id, dept_name, \
                                                expense_item, debit_account_code, credit_account_code, \
                                                amount, source_type, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?, 'REPAIR', '4101.01', '1002', \
                         ?::text::numeric, 'MANUAL', 'M5-06 夹具', now(), now())"
                    .to_string(),
                vec![
                    Value::from(&id),
                    Value::Text(period.to_string()),
                    Value::from(&object),
                    Value::from(&self.dept),
                    Value::Text(self.dept_name.clone()),
                    Value::Text(amount_text.to_string()),
                ],
            )
            .await
            .expect("造手工费用夹具");
            id
        }

        // ---------- 真实接口（本卡的主角） ----------

        /// 执行归集（M5-03 真接口）。
        async fn collect(
            &self,
            object: Uuid,
            period: &str,
        ) -> crate::business::erp::cost::cost_batch_collect_dto::BatchCollectRes {
            batch_collect::collect(
                &self.super_principal(),
                None,
                &object.to_string(),
                BatchCollectReq {
                    period_code: Some(period.to_string()),
                },
            )
            .await
            .unwrap_or_else(|error| panic!("归集 {period} 应成功：{:?}", error))
        }

        /// 结转试算（M5-04 真接口；被拦也返回数）。
        async fn preview(&self, object: Uuid, period: &str) -> BatchFinishPreviewRes {
            finish_api::preview(
                &self.super_principal(),
                &object.to_string(),
                BatchFinishQuery {
                    period_code: Some(period.to_string()),
                },
            )
            .await
            .unwrap_or_else(|error| panic!("结转试算 {period} 应成功（看数不拦）：{:?}", error))
        }

        /// 执行结转（M5-04 真接口）。
        async fn finish(
            &self,
            object: Uuid,
            period: &str,
        ) -> std::result::Result<finish_dto::BatchFinishRes, ApiError> {
            finish_api::finish(
                &self.super_principal(),
                None,
                &object.to_string(),
                BatchFinishReq {
                    period_code: Some(period.to_string()),
                    remark: Some("M5-06 夹具 结转".to_string()),
                },
            )
            .await
        }

        /// 归集表（M5-02 真接口，关键字收窄到本夹具的一个批次）。
        async fn sheet(&self, period: &str, name: &str) -> SheetRes {
            sheet_handler::sheet_of(
                &self.super_principal(),
                SheetQuery {
                    period_code: Some(period.to_string()),
                    keyword: Some(self.tag(name)),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|error| panic!("归集表 {period} 应成功：{:?}", error))
        }

        /// 归集明细（M5-02 真接口）。
        async fn detail(&self, object: Uuid, period: &str) -> SheetDetailRes {
            sheet_handler::detail_of(
                &self.super_principal(),
                &object.to_string(),
                SheetDetailQuery {
                    period_code: Some(period.to_string()),
                },
            )
            .await
            .unwrap_or_else(|error| panic!("归集明细 {period} 应成功：{:?}", error))
        }

        /// 在制余额表（M5-05 真接口，关键字收窄到本夹具的一个批次）。
        async fn balance(&self, period: &str, name: &str) -> WipBalanceRes {
            wip_handler::balance_of(
                &self.super_principal(),
                WipQuery {
                    period_code: Some(period.to_string()),
                    keyword: Some(self.tag(name)),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|error| panic!("余额表 {period} 应成功：{:?}", error))
        }

        /// 在制余额表（关键字收窄到**本夹具全部批次**）。
        async fn balance_all(&self, period: &str) -> WipBalanceRes {
            wip_handler::balance_of(
                &self.super_principal(),
                WipQuery {
                    period_code: Some(period.to_string()),
                    keyword: Some(self.tag_all()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|error| panic!("余额表 {period} 应成功：{:?}", error))
        }

        /// 逐期台账（M5-05 真接口）。
        async fn ledger(&self, object: Uuid, period: &str) -> WipLedgerRes {
            wip_handler::ledger_of(
                &self.super_principal(),
                &object.to_string(),
                WipLedgerQuery {
                    period_code: Some(period.to_string()),
                },
            )
            .await
            .unwrap_or_else(|error| panic!("台账 {period} 应成功：{:?}", error))
        }

        // ---------- 库内回查 ----------

        /// 结转单行数（按批）。
        async fn finish_rows(&self, object: Uuid) -> i64 {
            db().scalar_with_params(
                "SELECT count(*) FROM biz_cost_batch_finish WHERE object_id = ?::uuid",
                vec![Value::from(&object)],
            )
            .await
            .expect("查结转单行数")
        }

        /// 活归集行金额合计（Σ归集）。
        async fn collected_total(&self, object: Uuid) -> Decimal {
            let text: String = db()
                .scalar_with_params(
                    "SELECT COALESCE(sum(amount), 0)::text FROM biz_cost_collect \
                      WHERE cost_object_id = ?::uuid AND deleted_at IS NULL",
                    vec![Value::from(&object)],
                )
                .await
                .expect("查累计归集");
            amount(&text)
        }

        /// 结转单金额合计（Σ结转）。
        async fn transferred_total(&self, object: Uuid) -> Decimal {
            let text: String = db()
                .scalar_with_params(
                    "SELECT COALESCE(sum(total_amount), 0)::text FROM biz_cost_batch_finish \
                      WHERE object_id = ?::uuid",
                    vec![Value::from(&object)],
                )
                .await
                .expect("查累计结转");
            amount(&text)
        }

        /// 清掉该槽上一次运行的残留（只清本卡能造的形态）。
        async fn reset(periods: &[String; 3]) {
            let from = format!("{}-01", periods[0]);
            let to = format!("{}-01", periods[2]);
            let objects = format!(
                "(SELECT id FROM biz_cost_object WHERE object_no LIKE 'CB-M506-%' \
                   AND start_date >= '{from}'::date AND start_date < ('{to}'::date + interval '1 month'))"
            );
            for sql in [
                format!("DELETE FROM biz_cost_batch_finish WHERE object_id IN {objects}"),
                format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN {objects}"),
                format!(
                    "DELETE FROM biz_cost_collect WHERE source_id IN \
                       (SELECT id FROM biz_stock_out_item WHERE stock_out_id IN \
                          (SELECT id FROM biz_stock_out WHERE cost_object_id IN {objects}))"
                ),
                format!("DELETE FROM biz_cost_period_item WHERE cost_object_id IN {objects}"),
                format!("DELETE FROM biz_cost_labor WHERE cost_object_id IN {objects}"),
                format!("DELETE FROM biz_cost_overhead WHERE cost_object_id IN {objects}"),
                format!(
                    "DELETE FROM biz_stock_out_item WHERE stock_out_id IN \
                       (SELECT id FROM biz_stock_out WHERE cost_object_id IN {objects})"
                ),
                format!("DELETE FROM biz_stock_out WHERE cost_object_id IN {objects}"),
                format!("DELETE FROM biz_cost_object WHERE id IN {objects}"),
            ] {
                let _ = db().exec_sql(sql, vec![]).await;
            }
        }

        /// 清场（FK 顺序：结转单 → 归集 → 计算单 → 工时 / 费用 → 领料明细 / 头 → 对象 → 主数据）。
        async fn cleanup(&self, objects: &[Uuid]) {
            if objects.is_empty() {
                return;
            }
            let list = objects
                .iter()
                .map(|id| format!("'{id}'::uuid"))
                .collect::<Vec<_>>()
                .join(",");
            for sql in [
                format!("DELETE FROM biz_cost_batch_finish WHERE object_id IN ({list})"),
                format!("DELETE FROM biz_cost_collect WHERE cost_object_id IN ({list})"),
                format!(
                    "DELETE FROM biz_cost_collect WHERE source_id IN \
                       (SELECT id FROM biz_stock_out_item WHERE stock_out_id IN \
                          (SELECT id FROM biz_stock_out WHERE cost_object_id IN ({list})))"
                ),
                format!("DELETE FROM biz_cost_period_item WHERE cost_object_id IN ({list})"),
                format!("DELETE FROM biz_cost_labor WHERE cost_object_id IN ({list})"),
                format!("DELETE FROM biz_cost_overhead WHERE cost_object_id IN ({list})"),
                format!(
                    "DELETE FROM biz_stock_out_item WHERE stock_out_id IN \
                       (SELECT id FROM biz_stock_out WHERE cost_object_id IN ({list}))"
                ),
                format!("DELETE FROM biz_stock_out WHERE cost_object_id IN ({list})"),
                format!("DELETE FROM biz_cost_object WHERE id IN ({list})"),
            ] {
                db().exec_sql(sql, vec![])
                    .await
                    .unwrap_or_else(|error| panic!("清场失败：{error}"));
            }
            for (sql, param) in [
                ("DELETE FROM biz_material WHERE material_code = ?", &self.product_code),
                (
                    "DELETE FROM biz_warehouse WHERE warehouse_code = ?",
                    &self.warehouse_code,
                ),
            ] {
                let _ = db().exec_sql(sql.to_string(), vec![Value::Text(param.clone())]).await;
            }
        }

        /// 本夹具残留行数（清场断言用：按批次号后缀数，不扫全库）。
        async fn residue(&self) -> i64 {
            db().scalar_with_params(
                "SELECT (SELECT count(*) FROM biz_cost_object WHERE object_no LIKE ?) + \
                        (SELECT count(*) FROM biz_cost_collect WHERE cost_object_id IN \
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?)) + \
                        (SELECT count(*) FROM biz_cost_batch_finish WHERE object_id IN \
                           (SELECT id FROM biz_cost_object WHERE object_no LIKE ?))",
                vec![
                    Value::Text(format!("%{}", self.tag_all())),
                    Value::Text(format!("%{}", self.tag_all())),
                    Value::Text(format!("%{}", self.tag_all())),
                ],
            )
            .await
            .expect("查残留")
        }
    }

    /// 余额表里按批次号取一行（找不到就是夹具没造对，直接失败）。
    fn wip_row<'a>(sheet: &'a WipBalanceRes, object_no: &str) -> &'a WipRowRes {
        sheet
            .list
            .iter()
            .find(|row| row.object_no == object_no)
            .unwrap_or_else(|| panic!("余额表里找不到 {object_no}（共 {} 行）", sheet.list.len()))
    }

    /// 归集表里按批次号取一行。
    fn sheet_row<'a>(sheet: &'a SheetRes, object_no: &str) -> &'a SheetRowRes {
        sheet
            .list
            .iter()
            .find(|row| row.object_no == object_no)
            .unwrap_or_else(|| panic!("归集表里找不到 {object_no}（共 {} 行）", sheet.list.len()))
    }

    // ==================== 6. 主链路：部分完工 → 整批完工 ====================

    /// P1 归集 2000（料 1200 / 工 500 / 费 300）→ 归集表与余额表对得上；
    /// P2 部分完工（完工 400 / 在制 600）⇒ 结转 409 + 零行写入，余额仍是**全额 2000**；
    /// P2 继续投料 500 ⇒ 余额 2500（不按 400/1000 摊）；
    /// P3 整批完工 ⇒ 一次转出 2500、单位成本 2.5、余额归零、台账三期滚动、Σ归集 = Σ结转。
    #[tokio::test]
    async fn c1_partial_completion_holds_cost_in_wip_until_batch_finishes() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("CHAIN", "OPEN", "1000", "0", "1000").await;

        // ---- P1：三路来源 → 真实归集 ----
        f.issue(object, &f.day(0, 5), "120", "1200").await;
        f.labor(object, &f.periods[0], &f.day(0, 6), "10", "50").await;
        f.overhead(object, &f.periods[0], "300").await;
        let collected1 = f.collect(object, &f.periods[0]).await;
        assert_eq!(amount(&collected1.material.collected_amount), amount("1200"));
        assert_eq!(amount(&collected1.labor.collected_amount), amount("500"));
        assert_eq!(amount(&collected1.overhead.collected_amount), amount("300"));
        assert_eq!(amount(&collected1.totals.total), amount("2000"));

        // 归集表（M5-02）与余额表（M5-05）在 P1 对同一批给出同一个数
        let sheet1 = f.sheet(&f.periods[0], "CHAIN").await;
        let row1 = sheet_row(&sheet1, &f.object_no("CHAIN"));
        assert_eq!(amount(&row1.amounts.mat), amount("1200"));
        assert_eq!(amount(&row1.amounts.labor), amount("500"));
        assert_eq!(amount(&row1.amounts.oh), amount("300"));
        assert_eq!(amount(&row1.amounts.total), amount("2000"));
        assert_eq!(row1.line_count, 3, "料 / 工 / 费各一行");

        let bal1 = f.balance(&f.periods[0], "CHAIN").await;
        let wip1 = wip_row(&bal1, &f.object_no("CHAIN"));
        assert_eq!(amount(&wip1.collected.total), amount("2000"));
        assert_eq!(amount(&wip1.balance.total), amount("2000"));
        assert_eq!(amount(&wip1.period_in.total), amount("2000"));
        assert_eq!(wip1.transfer_times, 0);
        assert!(wip1.has_balance);
        assert!(
            wip1.warnings.is_empty(),
            "在产且余额正常不该报警告：{:?}",
            wip1.warnings
        );

        // ---- P2：部分完工（400 / 在制 600）----
        f.set_progress(object, "400", "600").await;

        let blocked_preview = f.preview(object, &f.periods[1]).await;
        assert!(!blocked_preview.transferable, "部分完工不许结转");
        assert_eq!(
            blocked_preview.blockers[0].code,
            finish_dto::BLOCK_WIP_NOT_ZERO,
            "拦在「还有在制」，不是「没产出」"
        );
        assert_eq!(amount(&blocked_preview.pending.total), amount("2000"), "被拦也照样给数");

        let error = f.finish(object, &f.periods[1]).await.expect_err("部分完工必须拒结转");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, finish_dto::BLOCKED_CODE);
        assert!(error.msg.contains("在制"), "{}", error.msg);
        assert!(error.data.is_some(), "错误体要带结构化 blockers");
        assert_eq!(f.finish_rows(object).await, 0, "被拦 ⇒ 一行结转单都不写");

        // 部分完工时又投了料 / 报了工（P2 继续投入 500）
        f.issue(object, &f.day(1, 5), "40", "400").await;
        f.labor(object, &f.periods[1], &f.day(1, 6), "2", "50").await;
        let collected2 = f.collect(object, &f.periods[1]).await;
        assert_eq!(amount(&collected2.totals.total), amount("500"));

        let bal2 = f.balance(&f.periods[1], "CHAIN").await;
        let wip2 = wip_row(&bal2, &f.object_no("CHAIN"));
        assert_eq!(amount(&wip2.collected.total), amount("2500"), "累计归集跨期累加");
        assert_eq!(amount(&wip2.period_in.total), amount("500"), "本期投入只算 P2");
        assert_eq!(
            amount(&wip2.balance.total),
            amount("2500"),
            "部分完工不缩水：余额是全额 2500，不是 60%（1500）也不是 40%（1000）"
        );
        assert_eq!(amount(&wip2.completed_qty), amount("400"));
        assert_eq!(amount(&wip2.wip_qty), amount("600"));
        assert!(wip2.has_balance);
        assert!(
            wip2.warnings.is_empty(),
            "部分完工且有成本是正常形态，不该报警告：{:?}",
            wip2.warnings
        );

        // ---- P3：整批完工（1000 / 在制 0）→ 一次转出全额 ----
        f.set_progress(object, "1000", "0").await;
        let ok_preview = f.preview(object, &f.periods[2]).await;
        assert!(
            ok_preview.blockers.is_empty(),
            "整批完工 + 有成本 ⇒ 无拦截：{:?}",
            ok_preview.blockers
        );
        assert!(ok_preview.transferable);
        assert_eq!(
            amount(&ok_preview.pending.total),
            amount("2500"),
            "本次 = 累计 − 已转（0）"
        );
        assert_eq!(ok_preview.finished_qty, "1000.0000");
        assert_eq!(amount(&ok_preview.unit_cost), amount("2.5"), "2500 / 1000 = 2.5");

        let done = f.finish(object, &f.periods[2]).await.expect("整批完工应能结转");
        assert!(done.inserted);
        assert_eq!(amount(&done.transfer.total), amount("2500"));
        assert_eq!(amount(&done.remaining.total), amount("0"), "一次转干净");
        assert_eq!(done.preview.transferred.total, done.preview.collected.total);

        // 余额归零
        let bal3 = f.balance(&f.periods[2], "CHAIN").await;
        let wip3 = wip_row(&bal3, &f.object_no("CHAIN"));
        assert_eq!(amount(&wip3.balance.total), amount("0"), "整批结转后余额归零");
        assert!(!wip3.has_balance);
        assert_eq!(wip3.transfer_times, 1);
        assert!(
            wip3.warnings.is_empty(),
            "结转干净的批次不该留警告：{:?}",
            wip3.warnings
        );

        // 台账逐期滚动：期初 = 上期期末，末行 = 截止期
        let ledger = f.ledger(object, &f.periods[2]).await;
        assert_eq!(amount(&ledger.summary.collected.total), amount("2500"));
        assert_eq!(amount(&ledger.summary.transferred.total), amount("2500"));
        assert_eq!(amount(&ledger.summary.balance.total), amount("0"));
        assert_eq!(ledger.summary.collect_lines, 5, "P1 三行 + P2 两行");
        assert_eq!(ledger.summary.transfer_times, 1);
        assert_eq!(ledger.periods.len(), 3, "P1 / P2 / P3 三期都要有行（空月补 0）");
        let expected = [
            (f.periods[0].as_str(), "0", "2000", "0", "2000"),
            (f.periods[1].as_str(), "2000", "500", "0", "2500"),
            (f.periods[2].as_str(), "2500", "0", "2500", "0"),
        ];
        for (row, (period, opening, inflow, outflow, closing)) in ledger.periods.iter().zip(expected) {
            assert_eq!(row.period_code, period);
            assert_eq!(amount(&row.opening.total), amount(opening), "{period} 期初");
            assert_eq!(amount(&row.inflow.total), amount(inflow), "{period} 本期投入");
            assert_eq!(amount(&row.outflow.total), amount(outflow), "{period} 本期转出");
            assert_eq!(amount(&row.closing.total), amount(closing), "{period} 期末");
            // 滚动式恒等式（每行都成立）
            assert_eq!(
                amount(&row.closing.total),
                amount(&row.opening.total) + amount(&row.inflow.total) - amount(&row.outflow.total),
                "{period} 期末 = 期初 + 投入 − 转出"
            );
        }

        // 守恒：链路末端「Σ来源 = Σ归集 = Σ结转 + Σ在制」
        let collected_total = f.collected_total(object).await;
        let transferred_total = f.transferred_total(object).await;
        assert_eq!(collected_total, amount("2500"));
        assert_eq!(transferred_total, amount("2500"));
        assert_eq!(transferred_total + amount(&wip3.balance.total), collected_total);

        f.cleanup(&[object]).await;
    }

    // ==================== 7. 两批：一批完工、一批部分完工 ====================

    /// 同一个表尾合计里同时有「已结转干净的批」和「部分完工还在制的批」：
    /// 合计的三个数（归集 / 已转 / 余额）必须逐要素守恒，`wip_batch_count` 只数后者，
    /// 且后者的余额是**全额**（按 300/800 = 37.5% 摊会得到 225，明显不同）。
    #[tokio::test]
    async fn c2_two_batches_finished_and_partial_reconcile_at_the_totals() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let a = f.object("A", "OPEN", "1000", "1000", "0").await;
        let b = f.object("B", "OPEN", "800", "300", "500").await;

        // A 批：料 900 + 工 300 = 1200（整批完工）
        f.issue(a, &f.day(0, 5), "90", "900").await;
        f.labor(a, &f.periods[0], &f.day(0, 6), "6", "50").await;
        // B 批：料 500 + 费 100 = 600（部分完工）
        f.issue(b, &f.day(0, 5), "50", "500").await;
        f.overhead(b, &f.periods[0], "100").await;

        assert_eq!(amount(&f.collect(a, &f.periods[0]).await.totals.total), amount("1200"));
        assert_eq!(amount(&f.collect(b, &f.periods[0]).await.totals.total), amount("600"));

        let done = f.finish(a, &f.periods[0]).await.expect("A 批整批完工");
        assert!(done.inserted);
        assert_eq!(amount(&done.transfer.total), amount("1200"));

        let error = f.finish(b, &f.periods[0]).await.expect_err("B 批部分完工必须拒");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, finish_dto::BLOCKED_CODE);

        let sheet = f.balance_all(&f.periods[0]).await;
        let row_a = wip_row(&sheet, &f.object_no("A"));
        let row_b = wip_row(&sheet, &f.object_no("B"));
        assert_eq!(amount(&row_a.collected.total), amount("1200"));
        assert_eq!(amount(&row_a.transferred.total), amount("1200"));
        assert_eq!(amount(&row_a.balance.total), amount("0"));
        assert!(!row_a.has_balance);
        assert_eq!(amount(&row_b.collected.total), amount("600"));
        assert_eq!(amount(&row_b.transferred.total), amount("0"));
        assert_eq!(
            amount(&row_b.balance.total),
            amount("600"),
            "部分完工留全额；按产出 300/800 摊会变成 225"
        );
        assert!(row_b.has_balance);
        assert!(row_b.warnings.is_empty(), "部分完工是正常形态：{:?}", row_b.warnings);

        // 表尾合计 = 两行之和，且逐要素守恒
        assert_eq!(sheet.totals.batch_count, 2);
        assert_eq!(sheet.totals.wip_batch_count, 1, "只有 B 还有余额");
        assert_eq!(sheet.totals.negative_batch_count, 0);
        assert_eq!(amount(&sheet.totals.collected.total), amount("1800"));
        assert_eq!(amount(&sheet.totals.transferred.total), amount("1200"));
        assert_eq!(amount(&sheet.totals.balance.total), amount("600"));
        assert_eq!(
            amount(&sheet.totals.collected.material),
            amount(&sheet.totals.transferred.material) + amount(&sheet.totals.balance.material),
            "料：已转 + 余额 = 累计归集"
        );
        assert_eq!(
            amount(&sheet.totals.collected.labor),
            amount(&sheet.totals.transferred.labor) + amount(&sheet.totals.balance.labor),
            "工：已转 + 余额 = 累计归集"
        );
        assert_eq!(
            amount(&sheet.totals.collected.overhead),
            amount(&sheet.totals.transferred.overhead) + amount(&sheet.totals.balance.overhead),
            "费：已转 + 余额 = 累计归集"
        );

        f.cleanup(&[a, b]).await;
    }

    // ==================== 8. 结转后迟到的成本：只转差额 ====================

    /// 结转之后又确认了真成本（迟过账的领料 / 迟确认的工时）：余额重新 > 0，
    /// 余额表要打 `FINISHED_NOT_TRANSFERRED`（整批已完工却还有余额），
    /// 第二次结转只走差额（`transfer_seq = 2`），转完归零。
    #[tokio::test]
    async fn c3_late_cost_after_finish_moves_only_the_delta() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("LATE", "OPEN", "800", "800", "0").await;

        f.issue(object, &f.day(0, 5), "100", "1000").await;
        f.labor(object, &f.periods[0], &f.day(0, 6), "8", "50").await;
        assert_eq!(
            amount(&f.collect(object, &f.periods[0]).await.totals.total),
            amount("1400")
        );
        let first = f.finish(object, &f.periods[0]).await.expect("首次结转");
        assert!(first.inserted);
        assert_eq!(amount(&first.transfer.total), amount("1400"));
        assert_eq!(
            amount(&first.preview.cumulative_unit_cost),
            amount("1.75"),
            "累计单位成本 1400 / 800（结转后的回查口径）"
        );

        // 迟到的成本落在 P2
        f.issue(object, &f.day(1, 5), "30", "300").await;
        f.labor(object, &f.periods[1], &f.day(1, 6), "2", "50").await;
        assert_eq!(
            amount(&f.collect(object, &f.periods[1]).await.totals.total),
            amount("400")
        );

        let bal = f.balance(&f.periods[1], "LATE").await;
        let row = wip_row(&bal, &f.object_no("LATE"));
        assert_eq!(amount(&row.balance.total), amount("400"), "迟到成本重新挂在在制");
        assert!(
            row.warnings
                .iter()
                .any(|code| code == wip_dto::WARN_FINISHED_NOT_TRANSFERRED),
            "整批已完工却还有余额必须报警：{:?}",
            row.warnings
        );

        let preview = f.preview(object, &f.periods[1]).await;
        assert!(preview.transferable, "第二次结转允许（迟到的真成本）");
        assert_eq!(amount(&preview.pending.total), amount("400"), "只走差额");
        assert_eq!(amount(&preview.unit_cost), amount("0.5"), "试算口径：本次 400 / 800");
        assert_eq!(preview.transfer_no, 2);

        let second = f.finish(object, &f.periods[1]).await.expect("二次结转");
        assert!(second.inserted);
        assert_eq!(amount(&second.transfer.total), amount("400"));
        assert_eq!(second.preview.history.len(), 2, "两次结转各一张单");
        let seqs: Vec<i32> = second.preview.history.iter().map(|item| item.transfer_seq).collect();
        assert!(seqs.contains(&1) && seqs.contains(&2), "历史里要有第 1、2 次：{seqs:?}");
        assert_eq!(
            amount(&second.preview.cumulative_unit_cost),
            amount("2.25"),
            "累计 1800 / 800（回查口径）"
        );

        let bal_done = f.balance(&f.periods[1], "LATE").await;
        let row_done = wip_row(&bal_done, &f.object_no("LATE"));
        assert_eq!(amount(&row_done.balance.total), amount("0"), "二次结转后归零");
        assert!(!row_done.has_balance);
        assert_eq!(row_done.transfer_times, 2);
        assert!(
            row_done.warnings.is_empty(),
            "转干净后不该再有警告：{:?}",
            row_done.warnings
        );

        assert_eq!(f.finish_rows(object).await, 2, "两次结转两张单");
        assert_eq!(f.collected_total(object).await, amount("1800"));
        assert_eq!(f.transferred_total(object).await, amount("1800"));

        f.cleanup(&[object]).await;
    }

    // ==================== 9. 幂等：重复结转 / 重复归集 / 重复读 ====================

    #[tokio::test]
    async fn c4_repeat_finish_and_reads_are_idempotent() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("IDEM", "OPEN", "500", "500", "0").await;
        f.issue(object, &f.day(0, 5), "80", "800").await;
        assert_eq!(
            amount(&f.collect(object, &f.periods[0]).await.totals.total),
            amount("800")
        );

        let first = f.finish(object, &f.periods[0]).await.expect("首次结转");
        assert!(first.inserted);

        // 重放同一请求：不写空单，也不报错
        let replay = f.finish(object, &f.periods[0]).await.expect("重放应幂等成功");
        assert!(!replay.inserted, "没有差额 ⇒ 不写空单");
        assert!(replay.finish_id.is_none());
        assert!(replay.message.contains("无可结转差额"), "{}", replay.message);
        assert_eq!(amount(&replay.transfer.total), amount("0"));
        assert_eq!(f.finish_rows(object).await, 1, "重放不许多写一张单");
        assert_eq!(f.transferred_total(object).await, amount("800"));

        // 重跑归集：一行不新增
        let again = f.collect(object, &f.periods[0]).await;
        assert_eq!(again.material.inserted, 0);
        assert_eq!(amount(&again.totals.total), amount("800"));

        // 余额表 / 台账两次读数逐字相同（只读接口不该因时点漂移）
        let bal_first = f.balance(&f.periods[0], "IDEM").await;
        let bal_second = f.balance(&f.periods[0], "IDEM").await;
        let rows = |sheet: &WipBalanceRes| -> Vec<String> {
            sheet
                .list
                .iter()
                .map(|row| {
                    format!(
                        "{}|{}|{}|{}|{}",
                        row.object_no,
                        row.collected.total,
                        row.transferred.total,
                        row.balance.total,
                        row.transfer_times
                    )
                })
                .collect()
        };
        assert_eq!(rows(&bal_first), rows(&bal_second));
        assert_eq!(bal_first.totals.balance.total, bal_second.totals.balance.total);

        let ledger_first = f.ledger(object, &f.periods[0]).await;
        let ledger_second = f.ledger(object, &f.periods[0]).await;
        assert_eq!(ledger_first.periods.len(), ledger_second.periods.len());
        assert_eq!(ledger_first.summary.balance.total, ledger_second.summary.balance.total);
        assert_eq!(amount(&ledger_first.summary.balance.total), amount("0"));

        f.cleanup(&[object]).await;
    }

    // ==================== 10. 归集表 ↔ 余额表 ↔ 明细：同一份数据三种看法 ====================

    #[tokio::test]
    async fn c5_sheet_period_in_matches_balance_and_detail_sums() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("CROSS", "OPEN", "600", "0", "600").await;

        // P1：三要素齐全 1700；P2：只有料 600
        f.issue(object, &f.day(0, 5), "100", "1000").await;
        f.labor(object, &f.periods[0], &f.day(0, 6), "10", "50").await;
        f.overhead(object, &f.periods[0], "200").await;
        f.issue(object, &f.day(1, 5), "60", "600").await;
        f.collect(object, &f.periods[0]).await;
        f.collect(object, &f.periods[1]).await;

        // P1：归集表本期合计 == 余额表本期投入 == 明细逐笔之和
        let sheet1 = f.sheet(&f.periods[0], "CROSS").await;
        let srow = sheet_row(&sheet1, &f.object_no("CROSS"));
        assert_eq!(amount(&srow.amounts.total), amount("1700"));
        assert_eq!(srow.line_count, 3);
        assert!(
            srow.last_occur_date
                .as_deref()
                .is_some_and(|day| day.starts_with(&f.periods[0])),
            "最后一笔归集日期要落在本期：{:?}",
            srow.last_occur_date
        );

        let detail1 = f.detail(object, &f.periods[0]).await;
        assert_eq!(
            amount(&detail1.totals.total),
            amount(&srow.amounts.total),
            "明细和 = 表行"
        );
        assert_eq!(detail1.lines.len(), 3);
        let line_sum: Decimal = detail1.lines.iter().map(|line| amount(&line.amount)).sum();
        assert_eq!(line_sum, amount(&srow.amounts.total));

        let bal1 = f.balance(&f.periods[0], "CROSS").await;
        let wip1 = wip_row(&bal1, &f.object_no("CROSS"));
        assert_eq!(
            amount(&wip1.period_in.total),
            amount(&srow.amounts.total),
            "本期投入 == 归集表本期"
        );
        assert_eq!(amount(&wip1.collected.total), amount(&srow.amounts.total));

        // P2：本期只有料 600，但累计是 2300（跨期口径不能只算本期）
        let sheet2 = f.sheet(&f.periods[1], "CROSS").await;
        let srow2 = sheet_row(&sheet2, &f.object_no("CROSS"));
        assert_eq!(amount(&srow2.amounts.total), amount("600"));
        assert_eq!(srow2.line_count, 1);

        let bal2 = f.balance(&f.periods[1], "CROSS").await;
        let wip2 = wip_row(&bal2, &f.object_no("CROSS"));
        assert_eq!(amount(&wip2.period_in.total), amount("600"), "本期投入 = 归集表 P2 行");
        assert_eq!(amount(&wip2.collected.total), amount("2300"), "累计归集 = 1700 + 600");
        assert_eq!(amount(&wip2.balance.total), amount("2300"), "一条都没转 ⇒ 余额 == 累计");

        let detail2 = f.detail(object, &f.periods[1]).await;
        assert_eq!(amount(&detail2.totals.total), amount("600"));
        assert_eq!(detail2.lines.len(), 1);

        f.cleanup(&[object]).await;
    }

    // ==================== 11. 卫生闸判据形状：归集过就放行 ====================

    /// 「已过账未归集的领料」这条卫生闸的**判据形状**回归用例。
    ///
    /// 归集写入侧的幂等键是 `(source_type='STOCK_OUT', source_id=出库**明细**行 id, element)`，
    /// 但判据一度写成 `cc.source_id = 出库单头.id` —— 两个 id 形状不同，于是**归集过也判成没归集**：
    /// 批只要有张已过账领料单就永远结不掉（结转 409 + 关批 409），只能改库。
    /// 本用例两个方向都钉：未归集 ⇒ 拦；走真接口归集过 ⇒ 放行。
    #[tokio::test]
    async fn c6_collected_stock_out_unblocks_the_chain() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("GATE", "OPEN", "300", "300", "0").await;
        f.issue(object, &f.day(0, 5), "30", "300").await;

        // 方向一：已过账但还没归集 ⇒ 材料成本还没进筐，必须拦
        let blocked = f.preview(object, &f.periods[0]).await;
        assert!(!blocked.transferable);
        assert!(
            blocked
                .blockers
                .iter()
                .any(|blocker| blocker.code == finish_dto::BLOCK_UNCOLLECTED_STOCK_OUT),
            "未归集的领料必须拦：{:?}",
            blocked.blockers.iter().map(|blocker| blocker.code).collect::<Vec<_>>()
        );
        let error = f.finish(object, &f.periods[0]).await.expect_err("未归集领料必须拒结转");
        assert_eq!(error.status, 409);
        assert_eq!(f.finish_rows(object).await, 0);

        // 方向二：走真接口归集（source_id 落的是出库明细行 id）⇒ 同一张单不再拦
        f.collect(object, &f.periods[0]).await;
        let allowed = f.preview(object, &f.periods[0]).await;
        assert!(
            allowed.blockers.is_empty(),
            "归集过就不该再报未归集：{:?}",
            allowed.blockers.iter().map(|blocker| blocker.code).collect::<Vec<_>>()
        );
        let done = f.finish(object, &f.periods[0]).await.expect("归集后应能结转");
        assert!(done.inserted);
        assert_eq!(amount(&done.transfer.total), amount("300"));

        let bal = f.balance(&f.periods[0], "GATE").await;
        assert_eq!(
            amount(&wip_row(&bal, &f.object_no("GATE")).balance.total),
            amount("0"),
            "链路走通后余额归零"
        );

        f.cleanup(&[object]).await;
    }

    // ==================== 12. 清场零残留 ====================

    #[tokio::test]
    async fn zz_teardown_leaves_zero_residue() {
        if !ensure_db().await {
            return;
        }
        let f = Fixture::build().await;
        let object = f.object("ZZ", "OPEN", "300", "100", "200").await;
        f.issue(object, &f.day(0, 5), "10", "100").await;
        f.labor(object, &f.periods[0], &f.day(0, 6), "1", "50").await;
        f.overhead(object, &f.periods[0], "30").await;
        f.collect(object, &f.periods[0]).await;
        assert!(f.residue().await > 0, "清场前应有残留（否则这条用例是假绿）");

        f.cleanup(&[object]).await;
        assert_eq!(f.residue().await, 0, "清场后对象 / 归集 / 结转应全为 0");
    }
}
