# M4-15 供应商评估「单测」证据包（含评级边界）

* **卡**：`t_73b6adf8` —【M4·采购】供应商评估：单测（含评级边界）
* **前置**：M4-12（表结构 `V0454`）、M4-13（打分 + 评级 `supplier_evaluation_grading.rs`）——本卡**零新迁移、零新接口、零前端改动**
* **一键复跑**：

```bash
bash scripts/verify/m415_supplier_evaluation_unit_drill.sh          # 99 → 145 的 PG
PG_HOST=127.0.0.1 bash scripts/verify/m415_supplier_evaluation_unit_drill.sh   # 在 145 上跑
```

**结果：39 条断言全过（PASS=39 / FAIL=0，`RESULT: PASS`）** —— 见 `10_drill.log`。

## 一、交付物

```
backend/app/src/business/erp/purchase/supplier_evaluation_suite_tests.rs    （新）12 条用例（5 纯逻辑 + 7 真库）
backend/app/src/business/erp/purchase/supplier_evaluation_grading.rs        （改）读回对拍改用「行内数据」—— **修掉并发误报 400**（见 §三）
backend/app/src/business/erp/purchase/mod.rs                               （+5 行注册）
scripts/verify/m415_supplier_evaluation_unit_drill.sh                       （新）39 条断言的演练
scripts/verify/m415_mutations.py                                            （新）4 个变异反证的施加/恢复
docs/evidence/2026-11-18-m415-supplier-evaluation-unit/                     （新）本证据包
```

## 二、为什么不抄第二遍：M4-13 README 点名的三处缺口

`docs/evidence/2026-11-18-m413-supplier-evaluation-grading/README.md` §四逐字写着：
「本卡已覆盖阈值 8 个边界点、regrade 幂等/scope、交期统计口径、入参五类拒绝；
**仍未覆盖的是「并发打分同一期间」「跨期重打分」「超大方量性能」**」。
本卡的 12 条就是照这三处 + 卡面「含评级边界」补齐的：

| 用例 | 钉住的口径 |
|---|---|
| `grade_policy_holds_on_every_hundredth_point` | 评级边界**穷举**：0.00–100.00 逐 0.01 共 10001 点，等级与政策逐点一致；全区间只有 3 次跳变、落在 70.00/80.00/90.00；越界（-0.01 / 100.01）报错而不是给 D |
| `rounding_happens_before_grading` | 政策比的是**舍入后**总分：69.995/79.995/89.995 三个中点抬进上一档（C/B/A），69.994 不抬；先评级再舍入会判低一档 |
| `single_dimension_weights_reach_every_band` | 权重允许 0：单维 100/90/89.99/80/79.99/70/69.99/0 八个点等级不因权重结构而漂 |
| `case_sql_and_rust_share_one_threshold_table` | 阈值只有一份：CASE 里每个 `>=` 后面的数字**逐字**等于 `GRADE_THRESHOLDS` 且顺序一致；WHEN 恰 5 条（NULL + 4 档） |
| `weight_and_score_domains_are_validated_at_the_edges` | 权重各 0–1 且和精确 = 1（0/1 是合法端点、0.9999 与负值不合法）；分数 0/100 合法、100.01/-0.01 不合法 |
| `rating_boundaries_hold_in_the_database` | 12 个边界点（含 3 个 .995 中点）走 `save_evaluation` 落库：库层生成列总分 / 写入等级 / `grade_for_total(总分)` 三者逐点相同；每点只有 1 份单据 |
| `sql_case_agrees_with_rust_on_every_hundredth_point` | 10001 点一次性交给 PG，用**生产同一份** `grade_case_sql` 求值，与 Rust `grade_for_total` 逐点相同 —— 打分路径与 regrade 路径分叉必红 |
| `concurrent_scoring_of_one_period_keeps_one_document` | **并发打分同一期间**：5 轮 × 4 路同时写同一供应商同一期间（分数各不同），四路全成功、同 id、库里只 1 行、总分/等级/备注都自洽（无撕裂行）、同一期间只有 1 个单号 |
| `cross_period_scoring_makes_independent_documents` | **跨期重打分**：三个期间三份单据（单号不同）；重打中间那期只改它（id/单号不变），另两期一字不动；期间截止差一天 = 另一份单据；列表按期末倒序、最近一期 = 期末最晚那份 |
| `rejected_inputs_never_touch_an_existing_evaluation` | 七类非法入参（权重和 ≠ 1 / 权重越界 / 分数 100.01 / 分数 -0.01 / 期间颠倒 / 评估日早于期间 / 供应商不存在）+ 供应商已软删 → 全部 `InvalidValue`，且已有那份评估的**单号/总分/分数/等级/备注一个字节不动**、表里不多一行 |
| `bulk_regrade_is_one_pass_idempotent_and_leaves_scores_alone` | **超大方量**：`generate_series` 造 1200 行（真值 50.00=D，故意写成 A）→ 一次 regrade 全改对、第二遍 0 改动、`sum(total_score)` 前后相等（分数没被动）、抽行比对单号/权重/等级；耗时 28.6ms（只做 60s 粗上限，不当基准） |
| `zz_teardown_purges_fixtures` | 清场：`M415` 夹具 0 残留 |

## 三、本卡抓到并修掉的真实缺陷：并发打分给用户回 400

**现象**（`00_defect_before_fix.out`，用 HEAD 版本的 `save_evaluation` 跑本卡并发用例）：

```
第 0 轮第 1 路并发打分失败（并发不许给用户报错）：invalid value: 加权总分漂移：
库层生成列 96.00 vs Rust 试算 74.00（生成列表达式与 weighted_total 不一致）
```

**根因**：`save_evaluation` 落库后读回再对拍，比的是**本次入参**算出的 `rust_total`。
两路并发写同一供应商同一期间时，后写者的 `UPDATE` 可能落在先写者的 `UPDATE` 与读回 `SELECT` 之间 ——
先写者读回的是别人的一次完整写入，于是被判定成「生成列表达式与 weighted_total 不一致」，
把一条**莫须有的** `InvalidValue`（前端 400）甩给用户。第一轮就复现，不是小概率。

**修法**（`supplier_evaluation_grading.rs`，+14/−4）：读回后用**这一行自己的**权重/分数复算
（`EvaluationRow::recomputed_total()`）。整行 `UPDATE` 原子，不存在「分数是甲的、等级是乙的」撕裂行，
因此这个判据既保住原来的护栏（生成列表达式与 Rust 公式漂移时任何一行都对不上），
又不会把「并发」误判成「漂移」。修复后同一用例绿：`00b_after_fix.out`。

复现方法（把打分模块临时换回 HEAD 版本跑同一用例，跑完 `cp` 回来并 `sha256sum -c`）：

```bash
F=backend/app/src/business/erp/purchase/supplier_evaluation_grading.rs
cp "$F" /tmp/grading.fixed.rs && sha256sum "$F" > /tmp/grading.sha
git show HEAD:"$F" > "$F"
ERP_DB_URL="postgres://…/cp_m415_chk" cargo test -p cloudpivot --lib \
  business::erp::purchase::supplier_evaluation_suite_tests::concurrent -- --test-threads=1   # → FAILED
cp /tmp/grading.fixed.rs "$F" && sha256sum -c /tmp/grading.sha                              # → OK
```

> 判据没被削弱：变异 B（把舍入改成截断）仍会让 `rating_boundaries_hold_in_the_database` 因这条对拍转红。

## 四、假绿对照 + 变异反证

| 证据 | 结论 |
|---|---|
| `01_suite_without_db.out` | 不连库：**passed 仍是 12**、0 failed —— 但本卡正向标记只有 **5** 条（真库用例提前 `return`）。所以「passed 数」判不了真假绿 |
| `02_suite_with_db.out` | 连库：12 条标记全出、0 failed、**SKIP=0** |
| `03_group_with_db.out` | 回归整组 `supplier_evaluation`（M4-12 表结构 + M4-13 打分评级 + 本卡）= **53 passed / 0 failed / 0 SKIP**（本卡改的读回对拍没弄坏旧用例） |
| `04_mutation_A.out` | 阈值 `>=` 改 `>`（边界不含下界）→ `grade_policy_…` 与 `rating_boundaries_…` 等 7 条转红 |
| `04_mutation_B.out` | 舍入「中点远离零」改「向零截断」→ `rounding_happens_before_grading` 转红 |
| `04_mutation_C.out` | 阈值表 B 档下界 80 → 85 → `grade_policy_…` 转红 |
| `04_mutation_D.out` | `grade_case_sql` 里塞一条硬编码 91 分→B 的第二份阈值 → `sql_case_agrees_…` 报「901 个点分叉」转红 |
| `05_restored_*.out` | 四个变异恢复后各自 12 条全绿；恢复按 sha256 校验与基线逐字一致 |

## 五、门禁（全部 rc=0，见 `06`–`09`）

| 门禁 | 结果 |
|---|---|
| `cargo fmt -p cloudpivot -- --check` | 0 差异 |
| `cargo clippy -p cloudpivot --lib -- -D warnings` | 0 warning |
| `cargo clippy -p cloudpivot --all-targets -- -D warnings` | 0 warning（**测试目标也被查**） |
| `scripts/verify/test_seed_gate.sh`（P2-62） | 过：本文件零主数据手写 `INSERT`，供应商一律 `test_seed::ensure_supplier` |

另：本卡**不动任何迁移文件**、**不碰前端**（演练 §6 断言），生产代码只改 `save_evaluation` 的读回对拍一处。

## 六、已知边界（不是待办，是决定）

1. **性能只做粗上限**：1200 行 regrade 28.6ms、断言 < 60s。真实量级（万级/百万级）的基准不在本卡，
   也不该用单次耗时做 CI 断言（机器负载会抖）。
2. **并发只到 4 路 × 5 轮**：够逼出「唯一键退化成覆盖」与「读回误报」这两类结构性缺陷；
   更高并发下的连接池耗尽属压测范畴。
3. **只覆盖到数据层**：本卡不测 M4-16 的接口/权限（那些码与路由还不存在）。
4. **`total_score` 为 NULL 的分支不可达**：它是 `GENERATED ALWAYS … STORED` 且四维分数 `NOT NULL`，
   所以 `grade_case_sql` 的 `IS NULL THEN NULL` 只是防御式写法（M4-13 另有覆盖）。
