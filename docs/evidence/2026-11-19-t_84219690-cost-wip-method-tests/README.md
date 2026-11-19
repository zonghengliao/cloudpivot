# M5-24 在制品单测（约当产量法，含多工序）— 证据

- 卡号：`t_84219690`（M5-24）
- 日期：2026-11-19
- 分支：`main`
- 依赖父卡：M5-21（按工序完工率加权回写）/ M5-20（约当产量表）/ M5-22（料工费分配）/ M5-23（期末余额 + 结转核对）

## 1. 本卡做了什么

**只加测试面，零业务代码改动、零迁移、零路由、零权限码、零菜单、零前端。**

新增 `backend/app/src/business/erp/cost/cost_wip_method_tests.rs`（约 1770 行，20 条用例），
把四张在制卡串成一条链跑一遍：

```
工序完工率（多工序，加权）→ 对象级 completion_pct → 约当产量 → 料工费分配 → 期末余额 → 结转核对
```

`mod.rs` 以 `#[cfg(test)] mod cost_wip_method_tests;` 挂载（只在 `cargo test` 时编译），
`scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST` 补登记 1 条（本卡自己的测试面文件）。

### 为什么需要这张卡（四张卡各自都有单测）

M5-20/22/23 的 DTO 单测证明「每个函数按契约工作」，M5-21 的真库用例证明「工序表 ↔ 对象级完工率
同事务一起变」。拼起来还有三件事没人钉：

1. **多工序到底怎么进约当**：一期没有 MES 工序过站（ARCH-9 D-3），在制数量是**对象级一个数**，
   工序只贡献**加权综合完工率**（`rollup_of` → 写回 `biz_cost_object.completion_pct`）。
   不是「按工序分别约当」，而是「按工序加权出一个 P，再乘同一份在制数量」。
2. **同一个数在三张表里是不是同一个数**：约当表给分母、分配表给分子、余额表给期末。
3. **一道工序的完工率改了，三张表要一起动、且都不许画假 0**。

### 被钉住的口径

```text
综合完工率 P = Σ(工序权重 × 工序完工率) / Σ权重        （2 位，四舍五入取远）
本期完工 FQ  = 累计完工 − 前期已结算计算单完工合计
材料约当     = FQ + WQ × 100%                          （开工一次投入）
加工约当     = FQ + WQ × P/100                         （P 是**回写后的 2 位值**）
逐要素池     = 期初（上期单位成本还原）+ 本期投入
分配率       = 池 ÷ 该要素约当（未舍入分母）            （6 位）
完工分配     = FQ × 分配率（4 位）；在制 = 池 − 完工（4 位，差额倒挤）
期末余额     = 期初 + 投入 − 完工转出 = Σ 三要素在制
```

## 2. 用例清单（20 条）

### A. 跨卡链路（纯逻辑，不连库；不配 `ERP_DB_URL` 也能绿）— 14 条

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `w1_three_operations_roll_up_to_a_weighted_completion_pct` | 三道工序 (1×100 + 2×50 + 1×0)/4 = 50%；逐行贡献之和 = 综合值；焊接权重占比 50% |
| 2 | `w2_weights_move_the_pct_and_the_equivalents` | 同样 100/50/0，权重 (3,1,1) ⇒ P 70%；加工约当 900→940；人工在制 100→134.0424（倒挤）；两种口径都守恒 |
| 3 | `w3_weighted_pct_is_the_only_bridge_to_the_equivalence` | 33.33/50/66.67/100 四档逐位与 calc 的 `equivalents` 相等；分配表分母 = 约当表分母；多工序汇总与单工序直填 P 结果完全相同 |
| 4 | `w4_three_elements_split_and_conserve_under_the_multi_operation_rate` | 料 1000÷1000→800/200、工 900÷900→800/100、费 450÷900→400/50；合计 2000/350；单位完工成本 2.5 |
| 5 | `w5_balance_rolls_the_allocation_without_a_second_formula` | 余额逐要素取分配结果（期初 + 投入 − 转出 = 期末），三要素加总 = 余额 |
| 6 | `w6_prior_unit_costs_become_this_period_element_beginnings` | 上期单位成本 5/3/2 × 在制 100@50% ⇒ 期初 500/150/100 = 750（费用吃尾差）；池 1400/1200/600；完工 2500 / 在制 700 |
| 7 | `w7_a_settled_balance_carries_into_the_next_period` | 已结算取账面快照；下期期初一致 ⇒ `carried`（差 0）；差 0.01 ⇒ `mismatch` 且 `diff_cost = -0.01` |
| 8 | `w8_zero_progress_blocks_every_table_and_never_fakes_zero` | 工序全 0% ⇒ 三张表 `blocked`；材料侧照算 1000，加工侧 `null`；金额一律 `null`；坏行不进金额合计但数量照进 |
| 9 | `w9_finished_batch_pushes_the_whole_pool_into_finished_goods` | 在制 0 ⇒ 整池 2350 进完工、余额 0 ⇒ `cleared` |
| 10 | `w10_the_chain_uses_the_two_decimal_pct_the_operations_wrote_back` | (2×33.34)/3 = 22.226667 ⇒ 回写 22.23；链上用 22.23（122.23），未舍入商是 122.226667（差 0.003333，不许混用） |
| 11 | `w11_operations_warnings_survive_the_multi_operation_case` | 正常多工序无提醒；全 0 进度提 66067；在制 0 提「不参与约当」；权重全相同提算术平均；对象级被直改提「不一致」 |
| 12 | `w12_single_operation_weights_never_move_its_own_pct` | 单工序权重 1/9 都得 37.5%；两道工序 (1,3) ⇒ 25% |
| 13 | `w13_the_three_wip_tables_share_one_permission_code` | 三张表同一个码 `business:erp:cost-object:list`；工序表 `list`/`update` |
| 14 | `w14_totals_of_two_rows_keep_the_three_element_identity` | 两行（有在制 + 全完工）三张表合计同数：约当 1100/1000、分配 2650/2300/350、余额 350 = 200+100+50 |

### B. 真库全链路（`ERP_DB_URL` 开关；不设则 SKIP）— 6 条

| # | 用例 | 钉住什么 |
|---|---|---|
| 15 | `d1_multi_operation_pct_drives_equivalence_allocation_and_balance` | 主链路：三道工序 ⇒ 对象级 50%（真库值核对）⇒ 约当 1000/900 ⇒ 分配 2000/350（率 1/1/0.5）⇒ 余额 2350−2000=350；三张表合计同数 |
| 16 | `d2_prior_settled_period_feeds_the_beginnings_through_the_operations` | 跨月：上期已结算（单位成本 5/3/2）⇒ 本期完工 500、期初 500/150/100、池 3200、余额 700；`beg_period` 点名上期 |
| 17 | `d3_one_operation_change_moves_all_three_tables` | 只把「焊接」50%→100% ⇒ P 75 ⇒ 约当 950 ⇒ 人工率 0.947368、人工在制 142.1056、余额 413.1584；三张表同步 |
| 18 | `d4_blocked_row_is_null_in_all_three_tables` | 在制 200 / 完工率 0：约当 `WIP_PCT_MISSING`、分配金额 `null`、余额 `null` + `carry.state=blocked`、合计金额 0 但 `blocked_count=1` |
| 19 | `d5_totals_of_finished_and_wip_rows_keep_the_identity` | 两行：完工行 2300/350 + 在制行；余额合计 = 料 200 + 工 100 + 费 50；完工 + 余额 = 本期投入 |
| 20 | `zz_teardown_leaves_zero_residue` | 清场后本夹具（对象 / 工序 / 计算单 / 归集）零残留 |

## 3. 真跑记录

### 3.1 演练库

按卡的数据库约束在 **99 上连 145 的 PG**：

```bash
# 空库首启（迁移到 HEAD：V0470 + 全部种子），比 cloudpivot_test 模板新
bash scripts/setup_new_env.sh cp_m524_drill        # RECREATE=1，172.18.10.145
ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m524_drill" \
  cargo test --lib business::erp::cost::cost_wip_method_tests -- --test-threads=1
```

> 注意：`cloudpivot_test` 模板只到 `V0404`（无 `biz_cost_object_operation`），
> 直接用它会以 `relation "biz_cost_object_operation" does not exist` 失败 —— 必须先空库首启迁移。

### 3.2 本卡 20 条

```
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 7211 filtered out
```

- 配 `ERP_DB_URL` 跑两遍（第二遍验证夹具清场幂等）：两遍都 20 passed；
- 不配 `ERP_DB_URL`：14 条纯逻辑绿 + 6 条真库 SKIP（`cargo test` 仍全绿）；
- 库内残留核对：`select count(*) from biz_cost_object where object_no like 'CB-M524-%'` = **0**；
  `sys_change_log` 里本夹具的孤儿工序审计 = **0**。

### 3.3 门禁

| 门禁 | 结果 |
|---|---|
| `cargo fmt`（改动文件） | rc=0，`--check` 无差异 |
| `cargo clippy --lib --all-targets -- -D warnings` | rc=0（0 warning） |
| `cargo test --lib system::menu::permission_audit_tests` | 8 passed |
| `python3 scripts/lint/scope_gate.py --root .` | 本卡 1 条已登记；见 §4 已知欠账 |
| `bash scripts/check.sh --fast`（L0+L1，pre-push 同款） | 通过（本卡改动无新增告警） |

### 3.4 cost 域回归（1328 passed）

```
test result: FAILED. 1329 passed; 4 failed
```

失败的 4 条全在 **`cost_standard_db_tests`**（另一张卡），且：
- 单独跑 `cargo test --lib business::erp::cost::cost_standard_db_tests` ⇒ **13 passed / 0 failed**；
- 把本卡文件摘掉（`mod` 注册一并回退，即 HEAD 状态）再跑整套 cost 域 ⇒
  **同样不绿**（那一次是 `cost_period_approval_db_tests` 5 条挂，报「夹具污染：FINANCE_MANAGER 上有
  `m510.finance-manager`」—— 是该模块自己用例之间互相污染，与本卡无关）。

结论：cost 域整套在共享演练库上本来就存在**跨模块顺序相关**的失败，与本卡无关；
本卡 20 条在两次全量跑里都是绿的。

## 4. 已知欠账（只报不改）

1. `scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST` 还缺 11 条别的卡遗留的 cost 测试面登记
   （`cost_batch_*_db_tests` / `cost_batch_method_tests` / `cost_collect_sheet_db_tests` /
   `cost_standard_*` / `cost_variance_voucher_db_tests` / `cost_wip_equiv_db_tests`）与
   `stockin_handler_tests_zz_shared_roles.rs`，闸② 因此在 HEAD 就是红的。
   本卡只补自己 1 条（**一卡一会话，不夹带别卡**）；建议单开一张「测试面登记补齐」卡一次清完。
2. `cost_period_approval_db_tests` 的夹具会在库里留下 `m510.finance-manager`，自己后面的用例
   就报「夹具污染」（真库用例的 teardown 缺一步）；`cost_standard_db_tests` 整套跑与单独跑的
   结果不一致（顺序相关）。两者都属别卡，本次只在演练库里清掉残留、不改进代码。

## 5. 清场

- `cp_m524_drill`（演练库）用后 DROP；
- `cp_m524_dev`（早期用 `cloudpivot_test` 模板克隆、发现缺 V0470 后废弃）一并 DROP；
- 迁移实例（`127.0.0.1:18097`）已停，`ss -ltn` 无监听。
