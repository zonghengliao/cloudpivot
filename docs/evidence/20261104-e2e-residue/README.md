# 20261104 · 生产库 e2e 测试残留清理 + 环境守卫（`t_61dca492`）

> 环境：生产 `172.18.10.144`（`cloudpivot` 库，PostgreSQL 18.6）/ 开发机 `172.18.10.99`
> 执行时刻：2026-11-04 16:22–16:30 CST
> 代码基线：仓库 `main`（`21761b1` → 提交后见 `git log`）

## 1. 先量清楚（只读）

原始输出：`inventory-step1.txt` / `inventory-step2.txt` / `inventory-step3.txt`。

### 1.1 命中「测试残留」口径的行（`biz_type` = `E2E_*` / `D2_SMOKE`）

| 表 | 命中 | 软删情况 |
|---|---|---|
| `biz_flow_instance` | **37** | **全部活行**（`deleted_at IS NULL`），用户可见 |
| `biz_flow_def` | **85**（82 `E2E_*` + 3 `D2_SMOKE`） | **全部已软删** |
| `biz_flow_def_version` | 85 | — |
| `biz_flow_node` / `biz_flow_edge` / `biz_flow_node_assignee` | 330 / 245 / 3 | — |
| `biz_flow_instance_node` / `biz_flow_task` / `biz_flow_cc` | 107 / 37 / 0 | — |
| `biz_status_log`（`entity_type` / `entity_no` / `biz_ref_no`） | **0** | 审计证据表 |

37 条实例状态分布：`APPROVED` 33 / `WITHDRAWN` 4，**没有 PENDING（无在飞审批）**。

### 1.2 哪一些**不是**垃圾（不动）

* **11 条存活流程定义**全部是系统预置业务流（`contract_approve` / `flow_edge_config` /
  `payable_cancel` / `payable_pay` / `po_approve` / `stock_in_cancel` / `stock_in_post` /
  `stock_take_post` / `stock_transfer` / `task_accept` / `voucher_reverse`），全部 `PUBLISHED`，
  指纹 `md5 = 7fcb32a38a193135a8a7e370db1839d5`（删除前后一致）。
* **132 条软删定义 ≠ 132 条测试残留**：卡面数字来自「143 - 11 活」，其中只有 85 条命中
  测试残留口径；其余 47 条是 `d5_*` / `diag_*` / `acc_*`，以及 `flow_code` 带 `e2e_` 前缀但
  `biz_type` 是业务值（`EXPENSE` / `OTHER` / `TASK_ACCEPT`）的历史行。
  **按卡面红线，本卡不按 `flow_code` 猜垃圾**，只按 `biz_type` 判。
* `biz_status_log` 113 行**一行未删**（它是审计证据表；命中测试残留口径的行本来就是 0）。

### 1.3 删除窗口检查

| 检查 | 结果 |
|---|---|
| 非测试实例里的 PENDING | 1 条（`PO-20261102-000001` 采购订单审批，2026-11-02 起） |
| 未授予的锁 | 0 |
| 生产库活动连接 | 1 active / 8 idle |
| 测试实例是否 PENDING | 0 |

删除范围**严格限定在测试残留行**（前置校验还硬断言了「测试定义不被真业务实例引用」
「`biz_task` 外键不指向测试实例」`prev_instance_id` 不串链），因此对上面那条在飞采购单零影响。

## 2. 演练（生产库副本）

`drill-prod-copy.log`（`cloudpivot_e2eres_drill`，从生产库全量 dump 还原）：

```
① up 生效        ：指纹已变化（def/ver/node/edge/asg/inst/inode/task 全变）
② up 幂等        ：第二次 0 行受影响
③ down 原样恢复  ：9 张 flow 表 + biz_status_log 指纹逐字节等于演练前
④ 回滚后可重来   ：up #3 自检无 FAIL
```

演练库最后已删除；演练库的备份目录留在 `/root/datafix-backup-e2eres-20261104-162235`。

## 3. 生产应用

`prod-backup.log`（备份）+ `prod-apply.log`（10 前置校验 → 20 删除 → 30 自检）。

备份产物：`/root/datafix-backup-e2eres-20261104-162243/`
（`flow_tables.dump` 9 张 flow 表整表 dump、`biz_status_log.dump` 审计表整表 dump、
`biz_task.dump`、`target/*.csv` 逐表导出**将要删掉的每一行**、
`target/biz_status_log_residue.csv` = 只有表头（= 0 行，证明审计表没有要删的行）、MANIFEST）。

删除行数（脚本硬对账，任一不符即整事务回滚）：

```
biz_flow_def 85 / biz_flow_def_version 85 / biz_flow_node 330 / biz_flow_edge 245
biz_flow_node_assignee 3 / biz_flow_instance 37 / biz_flow_instance_node 107
biz_flow_task 37 / biz_flow_cc 0
快照 929 行（回滚底气）
biz_status_log 113 → 113（未动）
```

`30_verify.sql` 全绿（`PASS|...` 25 行，2 条 `WARN` 为范围外提示）：

```
PASS|residue_biz_flow_def|残留 0 行
PASS|residue_biz_flow_instance|残留 0 行
PASS|alive_flow_def|实际 11 条（基线 11）
PASS|keepers_fingerprint|7fcb32a38a193135a8a7e370db1839d5
PASS|count_biz_flow_def|actual=58 expected=58
PASS|count_biz_flow_def_version|actual=66 expected=66
PASS|count_biz_flow_node|actual=217 expected=217
PASS|count_biz_flow_edge|actual=167 expected=167
PASS|count_biz_flow_node_assignee|actual=48 expected=48
PASS|count_biz_flow_instance|actual=128 expected>=128
PASS|count_biz_flow_instance_node|actual=369 expected>=369
PASS|count_biz_flow_task|actual=154 expected>=154
PASS|biz_status_log_untouched|实际 113 行
WARN|out_of_scope_deleted_e2e_like_defs|36 条…
WARN|out_of_scope_orphan_instances|16 条…
```

## 4. 回归

### 4.1 词表巡检 `--strict`：FAIL → PASS

| | 输出 | 退出码 |
|---|---|---|
| 清场前（`strict-before.txt`） | `[FAIL] ② 审批动作 biz_flow_instance.biz_type 取值 44 种` + 37 个 `✗ 域外值 'E2E_AP_*' × 1`；`结果：FAIL（1 列有域外值）` | **1** |
| 清场后（`strict-after.txt`） | 14 列全 `[PASS]`；`结果：PASS（所有列的值都在 doc_kind.rs 的词表域内）` | **0** |

巡检行数 395 → 358（正好少 37）。

### 4.2 审批流体检 `GET /business/erp/approval-flow/health`：8/8

`health-after.txt`：

```
HTTP 200  total=8 healthy=8 unhealthy=0
OK PO_APPROVE/STOCK_IN_POST/STOCK_IN_CANCEL/PAYABLE_PAY/PAYABLE_CANCEL
   /VOUCHER_REVERSE/STOCK_TRANSFER/TASK_ACCEPT  （published=True, graph_ok=True）
```

### 4.3 不动真实流程

| 指标 | 清场前 | 清场后 | 差值 |
|---|---|---|---|
| `biz_flow_def` 活行（`deleted_at IS NULL`） | 11 | 11 | **0** |
| 其中命中测试残留口径 | 0 | 0 | 0 |
| 存活流程指纹 | `7fcb32a3…` | `7fcb32a3…` | 一致 |
| `biz_status_log` 行数 | 113 | 113 | 0 |

卡面要求「活行差值 == 删除的 E2E 行数」：删除的 85 条定义**全部本来就是软删行**，
所以活行删除数 = 0，差值 0 = 0 ✔（真正的「活行残留」是 37 条**实例**，37 → 0）。

## 5. 守卫（落成代码，不是口头约定）

| 入口 | 实现 | 卡住什么 |
|---|---|---|
| 浏览器 e2e | `frontend/e2e/target-guard.mjs`（既有，卡 `t_d75b02e7`） | 目标不是「回环 + 隔离端口」就拒绝；放行需 `E2E_ALLOW_PROD=1 E2E_PROD_REASON=…` |
| Python 验收 / e2e 脚本 | **新增** `scripts/verify/db_guard.py`，插桩到 `scripts/verify/*e2e*.py`（39 个）与 `common.py` | 目标库 == 生产库名 / 含 `prod` 段 / 解析不出 → **退出码 3**；放行规则与上面同一对变量 |
| 服务端 | **新增** `shared/startup_guard::{count_test_residue, test_residue_warning}` + `bootstrap::test_residue_guard` | 启动日志 `WARN` 点名「N 条测试残留定义 + M 条实例」并给出清理入口 |
| 服务端体检 | **新增** `approval_gate::FlowHealthRes.test_residue` | `GET /business/erp/approval-flow/health` 直接带 `test_residue{defs,instances,message}` |

判据唯一性：`doc_kind.rs::TEST_RESIDUE_SQL` / `is_test_residue()` 是 Rust 侧唯一真相，
`doc_kind_tests::test_residue_predicate_matches_python_and_sql` 钉住 Rust ↔ Python ↔ SQL 三边一致。

验证（细节见 `guard-runtime.txt`，全部在**生产库副本**上跑，不碰生产）：

| 验证 | 结果 |
|---|---|
| `db_guard.py --self-test` | 11/11 |
| `DB_NAME=cloudpivot python3 scripts/verify/p9_cancel_e2e.py` | 退出码 3，且在**任何网络/库动作之前**退出 |
| `E2E_ALLOW_PROD=1 + reason` | 横幅 + 留痕后放行 |
| 2 条造流程定义的 spec 在「非隔离目标」下真跑 | `5 skipped`（桩上零业务请求） |
| 新二进制 + 副本库里塞 1 行 `E2E_VERIFY` | 启动 `WARN` 点名「1 条定义 + 0 条实例」；`/health.test_residue.defs = 1`，`healthy` 仍 8/8 |
| 删掉那行后同一进程再查 | `defs = 0`（实时查库，不是启动缓存） |
| 重启（残留已清） | `INFO 测试残留自检通过：… 均 0 行` |

> `test_residue` 字段与启动 WARN 需要**部署新二进制**后才会在生产出现（本卡未部署生产：
> 数据清理与代码修复分开交付，部署走常规发布）。清理前就存在的 37 行已经在本次清掉，
> 上线后该字段应返回 `defs=0, instances=0`。

## 6. 范围外（已另开跟进卡）

* 16 条存活实例挂已软删定义（`flow_code` = `e2e_reject_*` / `e2e_single_*` / `e2e_cs_*` /
  `diag*` / `vrf_*`，`biz_type` 是 `OTHER`）；
* 36 条 `flow_code` 带 `e2e_/diag_/vrf_/d5_/acc_` 前缀的软删定义。

两条都由 `30_verify.sql` 以 `WARN` 点出来，不会被悄悄忽略。
