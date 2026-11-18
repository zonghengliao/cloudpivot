# M5-06 分批法：单测（含部分完工）— 证据与口径

- 卡号：`t_d77cbe01`
- 日期：2026-11-19
- 运行机：99（开发机），PG = `172.18.10.145:5432`
- 演练库：`cp_m506_fresh`（`make_full_suite_db.sh` 从零迁移 + 演示种子自检通过）、
  `cp_m506_dev`（`CREATE DATABASE … TEMPLATE cloudpivot_test` + 启动实例补 57 条迁移）
- 提交：见卡内汇报（`backend/app/src/business/erp/cost/cost_batch_method_tests.rs` 为主要交付物）

## 1. 这张卡做了什么

M5-02 / M5-03 / M5-04 / M5-05 四张卡各自都有单测，但它们的真库用例都是**直接 SQL 造夹具**
验证自己那一卡的读口径 / 写口径。这种用例证明「这一卡按契约工作」，**不能**证明
「A 卡写进去的、B 卡算出来的是同一个数」。本卡把四条链路串起来，全部**走真实接口**：

```text
领料单 / 工时台账 / 手工费用（三张来源单据，直接 SQL 造：接口造不出来）
      ↓ cost_batch_collect::collect     （M5-03 真接口）
   biz_cost_collect
      ↓ cost_collect_sheet_handler       （M5-02 真接口：归集表 + 明细）
      ↓ cost_batch_wip_handler           （M5-05 真接口：余额表 + 逐期台账）
      ↓ cost_batch_finish::{preview,finish} （M5-04 真接口）
   biz_cost_batch_finish
```

**核心是「含部分完工」**：分批法的核算单位是「批」，`wip_qty > 0` 时**一分不转**
（不做约当产量分摊）。本卡的用例断言的是「部分完工 ⇒ 余额不缩水、结转被拦、提示走期间结算」，
而不是「部分完工 ⇒ 按产出比例转出」——后者是期间结算（calc 约当产量）的活。

## 2. 交付物

| 文件 | 性质 | 说明 |
|---|---|---|
| `backend/app/src/business/erp/cost/cost_batch_method_tests.rs` | **新增** | 12 条用例（5 条纯逻辑 + 7 条真库全链路），约 1 350 行 |
| `backend/app/src/business/erp/cost/mod.rs` | 改动 +5 行 | 登记 `#[cfg(test)] mod cost_batch_method_tests;` |
| `backend/app/src/business/erp/cost/cost_object_model.rs` | **缺陷修复**（1 处 SQL 判据） | 见 §5 |
| `backend/app/src/business/erp/cost/cost_object_db_tests.rs` | 夹具修正 | B1-8 关批用例的「补归集」夹具改成真实形状（明细行 id），cleanup 先删明细 |
| `backend/app/src/business/erp/cost/cost_batch_finish_db_tests.rs` | 夹具修正 | M5-04 的 `posted_stock_out` 改成单头 + 明细并返回明细 id，cleanup 先删明细 |

零迁移、零新接口、零新权限码、零菜单、零前端（页面属 M5-07）。

## 3. 覆盖矩阵（12 条）

### A. 跨卡契约（纯逻辑，不连库 —— 没配 `ERP_DB_URL` 也照跑）

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `t1_chain_uses_exactly_one_read_code_and_one_write_code` | 四卡只用两个码：读 `cost-object:list` / 写 `cost-period:calc`；多一个码 = 一条指向 M5-07 的死菜单 |
| 2 | `t2_partial_completion_blocks_transfer_for_any_wip_left` | `wip_qty > 0` 一律拦：产出 0 / 部分 / 只差一件 / 超产 四种形态全拦；报文写「不做约当产量分摊」、修法指向期间结算；`wip=0` 才放行（拦的是在制，不是产出没到计划数） |
| 3 | `t3_transfer_is_the_difference_not_an_output_ratio` | `plan_transfer` 的签名里**没有计划数量**；首次转全额、二次转差额；写侧逐要素夹零（部分超出只夹那一项） |
| 4 | `t4_balance_identity_closes_on_partial_completion` | 部分完工余额 = 全额；整批完工余额归零；「已转 + 余额 = 归集」逐要素成立；报表侧不夹零（负余额能被 `any_negative` 看见） |
| 5 | `t5_element_literals_and_scales_are_single_sourced` | 料/工/费字面量四卡同源；金额 4 位、单位成本 6 位；`qty=0` 不造 NaN |

### B. 真库全链路（`ERP_DB_URL` 开关）

| # | 用例 | 钉住什么 |
|---|---|---|
| 6 | `c1_partial_completion_holds_cost_in_wip_until_batch_finishes` | 主链路：P1 归集 2000 → 归集表 = 余额表；P2 **部分完工 400/1000**（在制 600）⇒ 结转 409 + 零行写入、余额仍 2000（不缩水）；P2 再投 500 ⇒ 余额 2500；P3 整批完工 ⇒ 一次转 2500、单位成本 2.5、余额归零、台账三期滚动、Σ归集 = Σ结转 |
| 7 | `c2_two_batches_finished_and_partial_reconcile_at_the_totals` | A 完工转出 + B 部分完工留余额：`batch_count=2 / wip_batch_count=1`；表尾逐要素守恒；B 的余额是 **600**（按 300/800 摊会变成 225） |
| 8 | `c3_late_cost_after_finish_moves_only_the_delta` | 结转后迟到的真成本 ⇒ 余额 400 + `FINISHED_NOT_TRANSFERRED`；二次结转 `transfer_seq=2` 只走差额，累计单位成本 2.25，转完归零 |
| 9 | `c4_repeat_finish_and_reads_are_idempotent` | 重放 `inserted=false` 不写空单；重跑归集 0 新增；余额表 / 台账两次读数逐字相同 |
| 10 | `c5_sheet_period_in_matches_balance_and_detail_sums` | 同一份数据的三种看法：归集表本期合计 == 余额表本期投入 == 明细逐笔之和（P1 1700 / P2 600，累计 2300） |
| 11 | `c6_collected_stock_out_unblocks_the_chain` | 卫生闸判据形状：未归集 ⇒ 409；走真接口归集过 ⇒ 放行。**本卡发现的缺陷的回归用例**（§5） |
| 12 | `zz_teardown_leaves_zero_residue` | 清场后对象 / 归集 / 结转全 0（且清场前断言残留 > 0，防假绿） |

## 4. 验收数字（真跑）

| 项 | 命令 | 结果 | 证据文件 |
|---|---|---|---|
| 本卡用例 | `cargo test --lib business::erp::cost::cost_batch_method_tests -- --test-threads=1` | **12 passed / 0 failed**（5 纯 + 7 真库，无 SKIP） | `01_module_tests_12.out` |
| 成本域回归 | `cargo test --lib business::erp::cost:: -- --test-threads=1` | **824 passed / 0 failed**（M5-05 时 812，本卡 +12） | `02_cost_domain_regression.out` |
| 格式 / 静态检查 | `cargo fmt -p cloudpivot -- --check` + `cargo clippy --all-targets -- -D warnings`（pre-push 门禁口径） | fmt 通过 / clippy 0 告警（rc=0） | `03_fmt_clippy.out` |
| 反证（判据改回错误形状） | 见 `04b_teeth_console.out` | **5 条真库用例变红**（c1/c2/c3/c4/c6）；sha256 逐字节还原后 12 条复绿 | `04_teeth_header_id_mutation.out`、`05_restored_green.out` |
| 判据形状 SQL 探针 | `psql -f probe_judgement_shape.sql` | 同一份数据：正确归集行 1 条；`cc.source_id = 单头 id` 判成「未归集 1 张」；按明细反查 = 0 张 | `06_probe_judgement_shape.out` |

## 5. 本卡发现并修复的缺陷（链路原本是死的）

**症状**：只要批次上有一张**已过账的领料单**，`POST …/finish` 与「关批」就永远 409
`UNCOLLECTED_STOCK_OUT`，**即使已经归集过**。批永远结不掉、结转不掉，只能改库——
正是 AGENTS 约定 11 点名的「单据卡在中间态」形态。

**根因**：归集写入侧的幂等键是 `(source_type='STOCK_OUT', source_id=出库**明细**行 id, element)`，
`cost_batch_collect::collect` 与 `cost_calc_material` 落的都是 `biz_stock_out_item.id`；
而 `cost_object_model::close_facts` 的卫生判据写成了 `cc.source_id = 出库单头.id`。
两个 id 形状不同 ⇒ `NOT EXISTS` 永远为真 ⇒ 「归集过」在判据里看不见。

**证据**：`06_probe_judgement_shape.out`（`by_header_id=1` / `by_item_id=0`，同一份数据）。
同文件 `cost_object_model.rs:493` 的「已归集部分」用的是明细反查（正确形状），本判据是唯一的例外。

**修复**：判据改成 `JOIN biz_stock_out_item i ON i.id = cc.source_id … i.stock_out_id = so.id`，
并把两处夹具（B1-8 关批用例、M5-04 结转用例）改成真实形状——它们原来手工插的归集行
`source_id = 单头 id` 是库里不会出现的形状，正好把这条缺陷盖住了。

**反证**：把判据改回错误形状 ⇒ 5 条真库用例立刻红（`04_teeth_header_id_mutation.out`），
逐字节还原后复绿（`05_restored_green.out`，sha256 `1bfef563…1775`）。

## 6. 边界（本卡没做的事）

- 不改 M5-02 / M5-03 / M5-05 的任何业务逻辑；M5-04 的判定与出参一行未动。
- 不新增接口 / 权限码 / 菜单 / 迁移 / 前端；没有「改余额」「补金额」这类口子（余额是推出来的）。
- 期间结算（calc 约当产量）那条路没有动 —— 分批法与它是两条口径，本卡只在报文断言里确认
  拦截提示把人引向它。

## 7. 复现命令

```bash
# 干净库（迁移 + 演示种子 + 21 项自检）
cd /opt/cloudpivot && ERP_DB=cp_m506_fresh PORT=18243 bash scripts/verify/make_full_suite_db.sh

# 本卡用例（99 上连 145 的 PG）
cd /opt/cloudpivot/backend
ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m506_fresh" \
  cargo test --lib business::erp::cost::cost_batch_method_tests -- --test-threads=1

# 成本域回归
ERP_DB_URL="…/cp_m506_fresh" cargo test --lib business::erp::cost:: -- --test-threads=1

# 反证（改形状→红→还原→绿）
bash <scratch>/m506_teeth.sh mutate
```

## 8. 已知的、与本卡无关的模板库污染

`cloudpivot_test`（模板库）里 `FINANCE_MANAGER` 上挂着 4 个 e2e 时代的临时账号
（`e2e01_appr` / `e2e02_appr` / `e2e04_fin` / `e2e06_mgr`），导致
`cost_period_approval_db_tests` 的 6 条用例在**从模板克隆的库**上红（断言原文就写着
「别的用例把该角色绑给了临时账号却没清场」）。证据与对比见
`07_template_clone_preexisting_pollution.out`；在**从零建的干净库** `cp_m506_fresh` 上
成本域 824 条全绿。本卡没有改任何 `sys_user / sys_role / sys_user_role` 相关代码或数据。
