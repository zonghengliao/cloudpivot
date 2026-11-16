# M3-05 报价单「状态机 + 转单」单测 · 验收证据

- **卡**：`t_a2ba25ed`（M3-05【M3·销售】报价单：单测 / 含转单与状态机）
- **上游**：M3-01 表（`V0432`）/ M3-02 CRUD+状态机 / M3-03 转销售订单 / M3-04 审批流接线
- **新增代码**：`backend/app/src/business/erp/sales/quotation_state_tests.rs`（19 条用例，挂 `quotation_handler` 下以够到私有守卫）
- **共享代码**：`fixture_hygiene` 补登记销售链两类（`QUOTATION` / `SALES_ORDER`）+ `SALES` 常量 —— 之前报价单/销售订单写 `biz_status_log` 却没登记，孤儿行永远收不走
- **生产代码 0 行改动、迁移 0 个、前端 0 改动**

## 一、19 条用例钉住的口径

| # | 用例 | 钉住什么 |
|:--:|:--|:--|
| 1 | `send_guard_refuses_empty_lines_before_touching_customer` | 空明细在**问客户之前**就 400（顺序即口径，文案指名单号与「至少要 1 行明细」） |
| 2 | `accept_guard_expiry_boundary_is_inclusive_on_valid_until` | 有效期**当天**可接受、次日 409；文案带单号/有效期 + 可执行提示 |
| 3 | `cancel_guard_refuses_converted_quote_and_names_the_order` | 已转单不许作废；409 点名订单 id 且指向「作废订单」这条正确出口 |
| 4 | `status_request_validation_happens_before_any_db_read` | 域外状态 / 空白状态 / 非法 UUID 三关都在**读库之前**判掉（400，不是 500） |
| 5 | `convert_is_expiry_blind_while_accept_is_not` | 转单不看过期、接受看（已接受但过期仍可转）——M3-03 的不对称决定 |
| 6 | `guard_and_reason_edges_line_up_with_the_state_machine` | 原因边恰好 3 条（客户拒绝 + 发出后两条作废），草稿作废不要原因 |
| 7 | `issue_without_approved_instance_is_409_66088_and_writes_nothing` | 审批闸：**409 + 66088**，状态/审批指针/`update_by`/轨迹一个字都不许落 |
| 8 | `approved_instance_unlocks_issue_and_reason_names_the_instance` | 有 `APPROVED` 实例即放行，理由写成「审批通过（FLW-…）发出」；详情 `approval` 段对得上 |
| 9 | `skip_gate_issues_and_persists_the_skip_reason` | `Skipped` 闸（没配流程 + 显式跳过）放出，**跳过理由原样进状态日志** |
| 10 | `illegal_edges_and_terminal_states_are_409_with_targets` | 非法边 409 给「当前可流转到」+ 中文标签；终态再流转 409 说「终态」 |
| 11 | `reason_is_required_on_commitment_reversing_edges_only` | 客户拒绝 / 发出后作废 / 已接受作废缺 reason 400；草稿作废 200 且终态无按钮 |
| 12 | `expired_quote_cannot_be_accepted_but_recovers_after_fixing_the_date` | 过期不能接受（409）；续期后同一条边可走通（无死结） |
| 13 | `converted_quote_cannot_be_cancelled_but_plain_accepted_can` | 转单 ↔ 作废互踩：转过 409 点名订单，没转过 200；转单不改状态 |
| 14 | `approver_pending_locks_status_but_cancel_cascades_the_approval` | 审批人占着时状态推不动（409 带审批单号）；作废**同一事务**撤审批、清指针、无孤儿待办 |
| 15 | `resubmit_todo_keeps_the_quote_editable_and_issuable` | `RESUBMIT` 待办**不锁单**（M3-04 踩过的坑：按实例判锁会把单锁死） |
| 16 | `soft_deleted_quote_is_404_on_every_entry` | 软删后详情/流转/转单全部 404，且不留订单 |
| 17 | `concurrent_convert_builds_exactly_one_order` | 并发转单只建**一张**订单：一笔真建单 + 一笔幂等回读（`FOR UPDATE` 生效） |
| 18 | `status_log_records_every_hop_with_operator_snapshot` | 建立→发出→接受 3 行轨迹、from/to 正确、每跳都留操作人 |
| 19 | `zz_teardown_purges_fixtures_and_status_log_orphans` | 跑完夹具 0 残留、销售链孤儿状态日志 0 条 |

**审批实例为什么用 SQL 造**（文件头有完整理由）：本文件验「闸对实例状态怎么反应」，不验「引擎能不能发起」
（那是 M3-04 的 E2E）。走 `submit_approval` 需要已发布的 `quotation_approve` 流程 + 角色 UUID 匹配
（`V0435` 在角色不匹配时整条种子跳过），会让真库用例**依赖环境随机变红**。夹具流程定义是
`DRAFT` + `is_default = false`，不会被 `published_quotation_flow` 摘走。

## 二、验收实况

### 2.1 本组（19 条，真库）

```
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 5912 filtered out; finished in 0.68s
```

见 `01_card_group.out`（`ERP_DB_URL=cp_m305_test`，`--test-threads=1`，SKIP=0）。

### 2.2 演练脚本（克隆库 → 补 V0405…V0435 → 三组回归 → 清场断言）

```
bash scripts/verify/m305_quotation_state_drill.sh
== 汇总：PASS=19 FAIL=0 ==
ALL PASS
```

见 `00_drill.out`，逐项：

| 段 | 结论 |
|:--|:--|
| 迁移 | 31 条（V0405…V0435）全部应用成功；`biz_quotation` / `biz_quotation_line` 建表在位；6 个权限码；`quotation_approve` 流程种子 1 行 |
| 本卡 | 19 条全绿，SKIP=0 |
| 报价单整组（`business::erp::sales::quotation`） | **53** 条全绿（M3-02 14 + M3-03/02 handler 15 + 审批桥 5 + 本卡 19），SKIP=0 |
| 销售域整组（`business::erp::sales`） | **175** 条全绿，SKIP=0 |
| 清场 | 夹具客户/报价单/审批实例/待办 0 残留；`QUOTATION` / `SALES_ORDER` 孤儿状态日志 0 条 |

### 2.3 变异反证（证明用例真的会红）

同时下三处变异，**恰好 6 条**用例红，与预测逐条对应；变异已全部还原（`git checkout`）：

| 变异 | 红掉的用例 |
|:--|:--|
| `ensure_acceptable` 的过期判断短路成 `false` | `accept_guard_expiry_boundary…` / `expired_quote_cannot_be_accepted…` / `convert_is_expiry_blind…` |
| `ensure_cancellable` 的 `converted_so_id` 判断短路 | `cancel_guard_refuses_converted_quote…` / `converted_quote_cannot_be_cancelled…` |
| `held_by_approver` 把 `RESUBMIT` 待办也当「审批人占着」 | `resubmit_todo_keeps_the_quote_editable_and_issuable` |

### 2.4 全量回归（诚实记录）

`cargo test --lib -- --test-threads=1`（库 `cp_m305_test`）：

```
test result: FAILED. 5888 passed; 42 failed; 1 ignored; 0 measured; finished in 172.53s
```

这 42 条**全部与报价单无关**（`asset` 自动编码 / `cost` 期间审批 / `expense` / `fx` / `ledger` / `period` /
`inventory_aging` 等），且**分组单跑全绿**（例：`business::erp::fx_revaluation::tests` 14 passed、
`business::erp::cost::cost_period_approval_db_tests` 6 passed —— 同在 `cp_m305_test` 上）。
判定为「同一份库上串跑整库时各组夹具互相踩」的既有现象，不是本卡引入；
本卡涉及的两组（本卡 19 条 + 销售域 175 条）在整库串跑里也是绿的。

另有两点环境口径：

1. **HR 组的库名护栏**：`business::hr::fixture_hygiene` 拒绝在库名不含 `drill`/`test` 的库上清场。
   第一次用 `cp_m305_ut` 跑整库时 HR 组 40+ 条因此红；改名 `cp_m305_test` 后即恢复。**不是代码问题**。
2. `system::authz_tests::o13_every_write_route_requires_permission` 是存量红
   （现金流底稿写接口只挂了 `:list` 码，卡 `t_0fd37353`），与本卡无关。

## 三、静态检查

```
cargo fmt --check        → clean
cargo clippy --lib --all-targets -- -D warnings → 0 warning
```

## 四、清理

- 演练库 `cp_m305_state_drill_*` 由脚本自己 `dropdb`（跑完 `pg_database` 里 `cp_m305%` = 0）；
- 真库回归库 `cp_m305_test` 用完即删（含探针库 `cp_m305_probe_*`）；
- 无残留 cloudpivot 进程；`git add` 只按路径加本卡文件（未夹带其它 worker 的 `README.md` /
  `frontend/**` / `scripts/verify/README.md` / `menu_invariants.sh` 改动）。

## 五、留给下游

- **M3-08（报价单转订单单测）**：本卡已覆盖「转单状态守卫 / 逐行整列搬金额自检 / 幂等 / 并发一张单 /
  已转单不许作废 / 软删 404」；转单的**外币与信用占用**口径仍未涉（报价单无币种列、转单按本位币，
  信用控制是 M3-09~M3-13）。
- **M3-06（前端列表/详情页）**：判「待批」看 `approval_instance_id` / `approval.status`，**不要**看 `status`。
