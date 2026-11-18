# M5-05 分批法「在制批次余额」证据（t_712f9998）

> 验证库：`cp_m505_dev`（`createdb -T cloudpivot_test_m502`，基线 V0460；V0461 手动 + 后端实例登记，`schema_migrations` 已记 `cost_batch_finish/461`）；
> 跑在 99（开发机），PG 走 `172.18.10.145:5432`（用户 `cloudpivot`）。用完即 drop。
> 证据里每个数字都来自下面这些真跑输出，没有一个是估的。

## 一、这张卡交付了什么

**一句话**：一条只读口径 —— 未完工批次的钱在哪儿、还剩多少。

| 接口 | 作用 | 权限码 | 写库 |
|---|---|---|---|
| `GET /business/erp/cost-collect-sheet/wip-balance?period_code=yyyy-MM` | **在制余额表**：一行一批，截止某期 | `business:erp:cost-object:list`（复用） | ❌ |
| `GET /business/erp/cost-collect-sheet/{object_id}/wip-ledger?period_code=yyyy-MM` | **单批在制台账**：逐期 期初 / 投入 / 转出 / 期末 | 同上 | ❌ |

口径（与 V0461 表注释、M5-04 交接面逐字一致）：

```text
在制余额(截至 P)   = Σ 归集(period_code ≤ P, 活行) − Σ 结转(period_code ≤ P, 活行)   逐要素，不夹零
逐期滚动           期末(P) = 期初(P) + 本期投入 − 本期转出；期初(P) = 期末(P−1)
```

## 二、验收结果（全部真跑）

| 项 | 命令 | 结果 | 证据 |
|---|---|---|---|
| 本卡单测 | `ERP_DB_URL=…cp_m505_dev cargo test --lib cost_batch_wip -- --test-threads=1` | **46 passed / 0 failed**（32 纯 + 14 真库，0 SKIP） | `01_module_tests_46.out` |
| 成本域回归 | `cargo test --lib business::erp::cost::` | **812 passed / 0 failed**（M5-04 时 766，本卡 +46） | `02_cost_domain_regression.out` |
| 格式化 | `rustfmt --edition 2024 --check`（5 个文件） | rc=0 | 本节下方 |
| 静态检查 | `cargo clippy --lib -- -D warnings` | **0 告警** rc=0 | 本节下方 |
| HTTP 真跑 | 隔离实例 `:18026` + admin 登录 | 401 / 400×3 / 200×8 / 404×2 全符合契约；清场残留 0 | `04_http_probe.out`、`05_http_probe_warnings.out` |
| 反证（牙齿） | 把「累计归集」的 `period_code <= ?` 改成 `= ?` | **2 条真库用例当场红**（V1 截止口径、V6 余额表 vs 台账），还原 sha256 一致后复绿 | `06_teeth_cutoff_mutation.out` |

```
$ rustfmt --edition 2024 --check cost_batch_wip_{dto,model,handler,db_tests}.rs mod.rs ; echo $?
0
$ cargo clippy --lib -- -D warnings ; echo $?
0        （Finished dev profile in 37.03s，0 warning）
```

## 三、HTTP 真跑（`:18026`，库 `cp_m505_dev`）

夹具：A（部分完工：完工 600 / 在制 200）11 月 料 12000·工 3000·费 2000，12 月 料 500·工 100 + 12 月结转单 料 1000；
B（整批完工 800 / 在制 0）11 月 料 1000。

| # | 请求 | 结果 |
|---|---|---|
| ① | 无 token 余额表 | **401** |
| ② | 余额表缺 `period_code` | **400 / 10001**「period_code 必填（yyyy-MM…）」 |
| ③ | `period_code=2026-13` | **400 / 10001**「期间编码必须是 yyyy-MM（收到「2026-13」）」 |
| ④ | 台账缺 `period_code` | **400 / 10001**（同句报文） |
| ⑤ | 余额表 `?period_code=2026-12&keyword=M505PROBE` | **200**：totals 归集 18600（料13500/工3100/费2000）、已结转 1000、**在制余额 17600**、本期投入 600、本期转出 1000、`batch_count 2 / wip_batch_count 2 / negative_batch_count 0` |
| ⑥ | 余额表 `?period_code=2026-11`（**结转发生在 12 月**） | **200**：归集 18000、已结转 **0**、在制余额 **18000** ⇒ 12 月的结转不冲减 11 月末余额（时点量口径） |
| ⑦ | 余额表 `only_wip=true` | **200**：两批都还有余额，`batch_count 2` |
| ⑧ | 台账 A（截至 2026-12） | **200**：`summary.balance 16600`；`periods = [11月 入17000 出0 期末17000, 12月 入600 出1000 期末16600]`；`after_period null`；`truncated false` |
| ⑨ | 台账 A（截至 2026-11） | **200**：`summary.balance 17000`；`after_period = {collected 600, transferred 1000}` + `LATER_PERIOD_ACTIVITY`（INFO） |
| ⑩ | 台账 B（整批完工未结转） | **200**：余额 1000 + `FINISHED_NOT_TRANSFERRED`（WARN），hint 指向 `…/{id}/finish` |
| ⑪ | 台账（不存在的 uuid） | **404 / 10002** |
| ⑫ | 台账（路径不是 uuid） | **404 / 10002** |
| ⑬ | 路由共存：M5-02 归集表 / M5-02 明细 `/{id}` / M5-03 `…/sources` / M5-04 `…/finish-preview` | **四条同时 200**（字面量 `wip-balance` 不遮蔽 `{object_id}`） |

库内实况（探针自打）：`A collected=17600.0000 / A transferred=1000.0000 / A balance=16600.0000 / B balance=1000.0000` —— 与接口返回逐字相同。
清场后：`finish=0 / collects=0 / objects=0`。

## 四、14 条真库用例覆盖（`cost_batch_wip_db_tests.rs`）

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `v1_cutoff_excludes_later_periods` | 余额是时点量：1 月 1000 / 2 月 1500（累计式），本期投入只算当月 |
| 2 | `v2_balance_is_collected_minus_transferred` | 余额 = 归集 − 结转（逐要素）；表尾合计与行同源 |
| 3 | `v3_full_transfer_leaves_zero_balance` | 全额结转 ⇒ 余额 0、`has_balance=false`、`only_wip` 不再列它 |
| 4 | `v4_real_finish_transfer_zeroes_the_balance` | **走 M5-04 真接口 `finish`** 结转一次 ⇒ 台账末行期末 = 0（两卡口径闭环） |
| 5 | `v5_ledger_rolls_across_periods` | 每行期初 = 上一行期末；空月补 0 行；末行 = 截止期间 |
| 6 | `v6_ledger_summary_equals_balance_sheet_row` | 台账汇总 == 余额表同一行（料/工/费/合计/行数/次数逐项） |
| 7 | `v7_partially_completed_batch_keeps_its_balance` | **部分完工**（完工 500 / 在制 300）成本全留在在制、无警告；M5-04 试算 `transferable=false` |
| 8 | `v8_finished_but_not_transferred_is_flagged` | 完工未结转 ⇒ 台账与列表行都带 `FINISHED_NOT_TRANSFERRED` |
| 9 | `v9_voided_collect_after_transfer_turns_balance_negative` | 结转后归集被软删 ⇒ 余额 −1000 原样显示（不夹零）+ `NEGATIVE_BALANCE` + `negative_batch_count=1` |
| 10 | `v10_out_of_scope_is_404_and_absent_from_the_sheet` | 别车间：台账 404、余额表不列（不是 403）；本车间对照组可见 |
| 11 | `v11_later_activity_is_reported` | 截止期之后的归集/结转 ⇒ `after_period` + `LATER_PERIOD_ACTIVITY` |
| 12 | `v12_settled_item_is_attached_and_explained` | 已结算计算单挂出 `wip_end_cost` + `SETTLED_BY_PERIOD`（INFO）；截止期早于结算期看不到 |
| 13 | `v13_totals_are_the_whole_table_not_the_page` | `page_size=1` 时 `list.len()=1` 而 `totals.batch_count=3`、余额合计 6000 |
| 14 | `zz_teardown_leaves_zero_residue` | 清场后本夹具对象与归集行全 0 |

## 五、反证（牙齿）

把 `joins_sql()` 里累计归集的 `c.period_code <= ?` 改成 `c.period_code = ?`（一步「看起来无伤」的退化：余额变成只剩本月）：

```
test result: FAILED. 44 passed; 2 failed
  × v1_cutoff_excludes_later_periods        left 500.0000  right 1500  ← 2 月的累计少了 1 月
  × v6_ledger_summary_equals_balance_sheet_row                ← 余额表与台账开始不一致
```

还原后用 `sha256sum` 与改动前逐字节一致（`31c19d1d…671a`），随即 46 passed 复绿。
并且把「纯文本」的 SQL 形状用例也加了牙：`list_sql_has_exactly_four_period_placeholders` 现在逐个断言
`c./f.period_code <= ?`、`ci./fi.period_code = ?`，同样的 mutation 在**不连库**的情况下也会红。

## 六、边界（决定，不是待办）

- **零迁移**：本卡不新建表、不改任何列与约束（余额是**推出来的**，不是存出来的）。M5-04 的迁移注释里
  「M5-05 在制余额 = 累计归集 − Σ 本表活行」就是本卡的实现。
- **零写接口**：在制余额没有 `POST`。写入口只有 M5-03（归集进来）与 M5-04（结转出去）；
  在这里开「改余额」等于让在制成本凭空出现。单测 `both_routes_are_get_and_none_is_post` 钉住这一点。
- **零新权限码 / 零新菜单 / 零前端**：复用 `cost-object:list`（列表页属 M5-07）。
  菜单八条断言（总数 / 同父 sort / title 长度 / icon 等）判据面完全没动。
- **不碰** settle / reverse / report / alloc 一行；不改 M5-02 / M5-03 / M5-04 任何文件（只读复用它们的取数原语）。
- 余额**不夹零**（与 M5-04 写入侧夹零刻意相反）：负数只有一个来源 —— 结转后归集被作废/软删，
  夹掉就等于把「账错了」藏起来。
- 演练库 `cp_m505_dev` 用完即 drop。
