# M4-05【M4·采购】采购计划：单测（含执行率）

- 卡：`t_2bfb7729`（M4-05）
- 分支/提交：`main`
- 演练库：`cp_m405_ut_drill`（建在 `172.18.10.145:5432`，空库首启自己跑完 381 个迁移；脚本退出即 DROP）
- 一键复跑：`bash scripts/verify/m405_purchase_plan_unit_drill.sh`

## 交付了什么

| 文件 | 内容 |
|---|---|
| `backend/app/src/business/erp/purchase/purchase_plan_state_tests.rs`（新） | **24 条纯逻辑用例**：状态守卫矩阵 / 四个错误出口 / 审批表单快照（条件边刀口）/ 行 → DTO 映射 / **执行率矩阵与舍入边界** |
| `backend/app/src/business/erp/purchase/purchase_plan_handler.rs` | 末尾挂 `#[cfg(test)] #[path = "purchase_plan_state_tests.rs"] mod purchase_plan_state_tests;`（挂成 handler 子模块，才够得着 `ensure_draft` / `form_data` / `to_item_res` 这些私有件） |
| `backend/app/src/business/erp/purchase/purchase_plan_handler_db_tests.rs` | `setup()` 补 `test_seed::ensure_approver_roles()` —— 修「空库真红」 |
| `backend/app/src/business/erp/purchase/purchase_plan_convert_db_tests.rs` | 同上 |
| `scripts/verify/m405_purchase_plan_unit_drill.sh`（新） | 单测演练：纯逻辑 → 假绿对照 → 空库首启 → 真库（SKIP=0）→ fmt/clippy 两条门禁，20 条断言 |
| 本目录 | 8 份原始证据 |

## 覆盖口径（每条都对应卡面 / 迁移 / 流程定义里的一句原话）

**状态机 × 状态守卫**（`ensure_draft` / `status_drift`，改 / 删 / 提交三处共用）

1. 草稿：修改 / 删除 / 提交 / 转申请四个动作全部放行；
2. 非草稿（`SUBMITTED` / `APPROVED` + 两个库外值）× 四个动作：一律 `409 + 66049`，报文必须**点名单号 / 原始状态 / 中文状态名 / 动作 / 下一步（新建一张）**，且不许出现 500 字样；
3. 并发漂移（CAS 撞车）：与入口守卫**同一个错误码**（客户端一套处理），但恢复动作是「刷新后重试」而不是「新建一张」；
4. 三个库内状态都有中文名，且报文里出现的就是这个名字。

**错误出口**（约定 5：`bad_request` / `not_found` / `reference_missing` / `conflict` / `write_error` / `submit_conflict`）

5. 四个出口的状态码与业务码各就各位（400/10001、404/10002 + id、400/66002、409/10003）；
6. 写库失败 = `500 + 10004` 且保留「哪一步 + 底层原因」；
7. 并发提交撞 `uk_biz_flow_instance_biz_active` → **409 + 单号 + 撤回提示**；其它唯一/检查约束冲突仍是 500（不许伪装成 409）。

**审批表单快照（`pp_approve` 条件边的作用域）**

8. 单头 `total_planned_amount` / `total_planned_qty` 必须是 **JSON 数字**（引擎按 `as_f64()` 比较，字符串会 TypeMismatch 静默走兜底边）；明细金额是展示用字符串快照；
9. 与 `V0447` 逐字对拍：`"field":"form.total_planned_amount"`、`"cmp":"lt"`、`"value":500000` —— 键名改了就是「大额计划悄悄跳过财务经理」；
10. 阈值刀口：`499999.99 / 499999.9999` 不加签，`500000 / 500000.0000 / 500000.0001` 走财务复核；快照先按 **4 位小数**舍入（`499999.99999` → 加签，`499999.99994` → 不加签）；
11. `dept_name` / `planner_name` 缺失给空串（不是 null）；月度计划 `plan_month` 是数字、年度是 null；
12. 明细逐行、行序保持；`item_count` 取单头值（不是 `items.len()`）；
13. `json_number`：正常值可往返，空 / 垃圾 / `NaN` / `inf` / `1e999` 一律降级 null（不 panic、不串型）。

**行 → DTO（列表 / 详情 / 转换共用一份赋值路径）**

14. 只有 `DRAFT` 的 `editable = true`；状态 / 类型都带中文名；合计原样透传；
15. 可空 uuid 列：有值字符串、无值 null；
16. 明细读路径**不重算金额**（故意造「金额 ≠ 数量 × 单价」的夹具）。

**执行率（含）**

17. 6 计划量 × 10 已下单量的矩阵：状态四态判据、`rate = ordered/planned` 舍 6 位、`pct = rate × 100` 舍 2 位、率反算误差 ≤ 6 位舍入、`率 = 0 ⟺ NOT_STARTED`，并断言网格覆盖全部四态；
18. **展示舍入不翻转状态**：`999999.9999996 / 1000000` 率的展示值是 `1.000000`（100.00%）但状态必须 `PARTIAL`；`3.0000004 / 3` 展示 `1.000000` 但状态 `OVER` —— 前端不许拿百分比反推状态；
19. 汇总 = 逐行之和；率按**合计**算（48/117 = 0.410256，不是各行率的平均 0.725）；四个行状态计数之和 = 行数；
20. 一行超 + 一行部分、合计恰好下满 → 整单 `DONE`（不是「行状态多数」也不是「有一行超就整单超」）；
21. 「转申请 ≠ 下单」：只转申请时 `req_rate = 100%`、`exec_rate = 0`、状态 `NOT_STARTED`、剩余仍是全量；采购单作废后已下单回落到 0（`source_verdict` 链断即不算）；
22. 剩余量两套口径：报表 `remaining_of` 保留负数、转申请 `convertible_of` 夹到 0，并与转单模块 `ConvertibleLine::remaining` 逐例对拍（≤ 4 位标度完全一致；> 4 位标度的差异是各自契约，明确钉住而不是「顺手抹平」）;
23. 六条接口权限码与 `dto::PLAN_PERMISSIONS` 双向一致，且**没有一个是 `…:page`**（约定 14）；执行率码单独一个；
24. 纯函数状态判据与 `V0449` 的 CASE **逐词对拍**（四态、率的 6 位舍入、剩余量不许 `GREATEST`、单头空分母保护、四个 `*_lines` 列名）。

## 顺手修掉的「空库真红」（测试夹具无自足性）

`purchase_plan_handler_db_tests`（M4-02）与 `purchase_plan_convert_db_tests`（M4-03）的 `setup()` **没有**准备「非制单人」的审批岗位成员。空库首启只有 `admin`，而 `pp_approve` 带职责分离内控 → 提交直接 `65018`：

```
节点「采购经理审批」的审批人全部是制单人 / 发起人本人 … 本节点没有一个可用审批人
```

首轮演练实测（真库整组）：`107 passed / **10 failed**`，10 条全在 `purchase_plan_convert_db_tests`，报错都是 `65018`。

修法与其他模块同口径（M4-04 的用例早就这么做了）：`setup()` 里加一行 `test_seed::ensure_approver_roles().await;`。

**双向证据**（同一空库首启的演练库，只改这两行）：

| 证据 | 内容 |
|---|---|
| `09_fixture_removed_red.out` | 去掉那两行 → **1 passed / 10 failed**，日志里 10 处 `65018` |
| `10_fixture_restored_green.out` | 加回 → **11 passed / 0 failed** |
| `11_fixture_proof_run.out` | 两次运行的 rc、`sha256sum -c` 恢复校验、建库/起停全过程 |

修后整组真库：**117 passed / 0 failed**、四份真库用例 46 条逐条执行、`SKIP=0`（`02_module_with_db.out`）。

> 这类红的隐蔽处：它**只在干净库上红**。在已有角色成员的库（或先跑过 `execution_db_tests` 的库）里顺序凑巧就绿 —— 典型「换个环境就红」的假绿。

## 反证（证明用例真的会咬人）

`06_mutation_counterproof.out`：把两条判据故意改错 ——

1. `exec_state_of` 的 `ordered > planned → OVER` 改成 `>=`（等价处从 DONE 变 OVER）；
2. `ensure_draft` 的状态判断改成恒真（非草稿也能改 / 删 / 提交）。

结果 **19 passed / 5 failed**，红的正好是想钉的那 5 条：

```
every_non_draft_status_is_rejected_with_actionable_text   （SUBMITTED 不允许修改：( )）
exec_rate_and_state_are_consistent_over_a_grid            （plan=1 / ordered=1：OVER ≠ DONE）
guard_message_labels_every_known_status                   （非草稿必拒）
summary_mixed_over_and_under_can_land_done                （整单 OVER ≠ DONE）
summary_totals_equal_per_line_sums_and_state_follows_totals
```

两处源码已按备份字节级恢复：`sha256sum -c` 两个文件 `OK`，`git status` 只看到本卡的文件；恢复后复跑 **24/24 绿**（`07_restored_green.out`）。

## 实测数字

| 项 | 结果 | 证据 |
|---|---|---|
| 本卡纯逻辑（不连库） | **24 passed / 0 failed** | `00_pure_suite.out`、`07_restored_green.out` |
| 整组 `purchase_plan`（不连库，假绿对照） | 117 passed / 0 failed / 1 条 `SKIP:` 提示 | `01_module_without_db.out` |
| 整组 `purchase_plan`（真库，`cp_m405_ut_drill`） | **117 passed / 0 failed / SKIP=0 / 真库用例 46 条全跑** | `02_module_with_db.out` |
| 空库首启 | `/health` 200，迁移跑完，两个执行率视图 + 两张表 + `pp_approve` 各就位 | `05_drill_full.out` |
| 演练脚本 | **PASS=20 FAIL=0，RESULT: PASS** | `05_drill_full.out` |
| `cargo fmt --check` | rc=0 | `03_fmt.out` |
| `cargo clippy --all-targets -- -D warnings` | rc=0，0 条 warning | `04_clippy_all_targets.out` |
| 反证（两处判据改错） | 19 passed / **5 failed**（4 条点名）；恢复后 24/24 | `06_mutation_counterproof.out` |
| `test_seed_gate` | 全过（主数据手写 INSERT 109 = 预算 109，未上升） | `08_test_seed_gate.out` |

## 为什么这些用例挂在 handler 子模块里

`ensure_draft` / `status_drift` / `form_data` / `json_number` / `to_row_res` / `to_item_res` / `write_error` / `submit_conflict` 全是 `purchase_plan_handler` 的私有件。写到 `purchase/mod.rs` 里够不着，只能通过公开接口间接覆盖 —— 而「报文里有没有下一步」「金额是字符串还是数字」这类断言，只有直接调函数才钉得住。

代价是：`cargo test` 的过滤器要写**完整模块路径**（含 `purchase_plan_handler::`），只写 `…::purchase_plan_state_tests` 会命中 0 条（演练脚本里已按完整路径写）。

## 复跑

```bash
# 只跑本卡纯逻辑（不连库，秒级）
cd backend && cargo test --lib \
  business::erp::purchase::purchase_plan_handler::purchase_plan_state_tests -- --test-threads=1

# 全套演练（建空库 → 跑迁移 → 真库用例 → 门禁 → DROP 库）
bash scripts/verify/m405_purchase_plan_unit_drill.sh
DBHOST=127.0.0.1 bash scripts/verify/m405_purchase_plan_unit_drill.sh   # 在 145 上跑
```
