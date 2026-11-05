# 2026-11-04 · E2ERES2 证据包（卡 t_142317a2）

清掉 `t_61dca492` 清不掉的**两类**测试痕迹：**36 条软删流程定义 + 16 条孤儿存活实例**。
数据修复包：`scripts/datafix/e2e_residue2/`（做法与坑见该目录 README）。

## 1. 先判性质（本卡的核心，不是"按前缀猜垃圾"）

生产库（144 / `cloudpivot`）2026-11-04 只读盘点，逐条看 `create_by` / `flow_name` /
`created_at` / 业务单据引用，判决表 = 备份目录的 `target/classification.csv`（52 行逐条一行）：

| 桶 | 条数 | `create_by` | 时间窗 | `doc_refs` |
|---|---|---|---|---|
| 软删定义 `d5_*` | 7 | `admin` | 10-29 21:38–22:09 | 0 |
| 软删定义 `e2e_*` / `diag_*` / `diag2_*` / `vrf_*` | 29 | `laoliu_test` | 10-30 08:46–09:30 | 0 |
| 孤儿存活实例 | 16 | `laoliu_test`（发起人「老六测试号」） | 10-30 08:46–09:30 | 0 |

* `flow_name` 全部是 `E2E单签测试 / E2E驳回测试 / E2E会签测试 / 空图 / 诊断 / 诊断2 /
  验证单签 / 验证驳回 / 验证会签 / 验证空图 / D5 报销审批演示 / D5 校验发布演示 / D5 验证流程`。
* 16 条实例的 `title` 是 `E2E*实例 / 诊断实例 / 验证*实例`；`form_data` = `{"reason":"测试"}`（12）/
  `{"reason":"verify"}`（4）/ `{}`；**`biz_id` 与 `biz_no` 全部 NULL**。
* **11 处业务单据引用列逐个查 = 0**：`biz_contract.approval_instance_id`、
  `biz_payable.cancel_instance_id`、`biz_payable.payment_approval_instance_id`、
  `biz_payment.approval_instance_id`、`biz_purchase_order.approval_instance_id`、
  `biz_stock_in/stock_take/stock_transfer.approval_instance_id`、`biz_task.flow_instance_id`、
  `biz_task.approval_instance_id`、`biz_voucher.pending_reverse_instance_id`。
* `biz_status_log` 按 `entity_id` / `entity_no` 查也是 **0 行**。
* 附带：这 16 条实例的 37 个 `biz_flow_instance_node.node_id` **全是 NULL**（之前 flowclean
  删节点时被 SET NULL 过），删它们不会再连带任何行。

**结论：52 条全部是测试/演示，客户真实自定义 0 条。**

## 2. 删前备份

`/root/datafix-backup-e2eres2-20261104-172821/`（144 上）

* `flow_tables.dump` / `biz_status_log.dump` / `biz_task.dump`（`pg_dump -Fc`）
* `target/*.csv` 逐表「即将删除的每一行」（def 37 / version 40 / node 99 / edge 56 /
  assignee 13 / instance 17 / inode 38 / task 9 / cc 1 = 行数含表头）
* `target/classification.csv`（判决表，52 行）
* `target/biz_status_log_target_refs.csv`（0 行，导出即证明「不是忘了导」）
* `target/biz_status_log_FULL.csv`（113 行，删前整表留档）
* `MANIFEST`（库身份 / 行数 / 每个文件 md5 / git HEAD）

## 3. 副本演练（`drill.log`）

生产已被清过，所以先把备份里被删的 301 行灌回一个副本库（`cloudpivot_e2eres2_pre`，
核对后 58/66/217/167/48/128/369/154/0 与删除前逐表一致），再对它跑 drill：

```
① up #1   ：28 条断言全 PASS，0 FAIL（指纹已变化、逐表行数与基线一致）
② up #2   ：走幂等分支，0 行受影响
③ down    ：写回 301 行（36/39/98/12/55/16/37/8/0），
            9 张 flow 表 + biz_status_log 的指纹逐字节 == 演练前，逐表行数 == 演练前
④ 软删状态：回滚后 36 条定义 alive=0 soft_deleted=36（deleted_at 原样还回来，没被"复活"）
⑤ up #3   ：28 条断言全 PASS，证明回滚后还能重来
⑥ 跨批次账本：25_ledger 登记 9 项；down 时 followup_deleted_* 归零
```

## 4. 生产结果（2026-11-04 17:29:27）

```
删除：def 36 / version 39 / node 98 / edge 55 / node_assignee 12
      instance 16 / instance_node 37 / task 8 / cc 0        = 301 行
未动：biz_status_log 113 → 113（审计证据表一行未删；pin 前那段指纹不变）
保留：11 条存活流程定义，指纹 7fcb32a3… 前后一致；8/8 已发布
包内自检：28 条 PASS · 0 FAIL · 1 WARN（见 §6）
```

前后对比：

| 表 | 删除前 | 删除后 |
|---|---|---|
| `biz_flow_def`（总 / 软删） | 58 / 47 | **22 / 11** |
| `biz_flow_def_version` | 66 | 27 |
| `biz_flow_node` | 217 | 119 |
| `biz_flow_edge` | 167 | 112 |
| `biz_flow_node_assignee` | 48 | 36 |
| `biz_flow_instance` | 128 | 112 |
| `biz_flow_instance_node` | 369 | 332 |
| `biz_flow_task` | 154 | 146 |
| `biz_status_log` | 113 | **113** |

## 5. 复跑（全部退出码 0）

| 检查 | 结果 | 证据文件 |
|---|---|---|
| `python3 scripts/verify/biz_type_domain.py --strict` | **PASS**（14/14 列） | `biz_type_domain.strict.txt` |
| `GET /api/business/erp/approval-flow/health` | **200 / total=8 / healthy=8 / unhealthy=0** | `http_check.txt` |
| `GET /api/business/flow/def/page` | total=11，无 e2e/diag/vrf/d5 前缀 | 同上 |
| `GET /api/business/flow/task/todo` | 200 | 同上 |
| `python3 scripts/verify/erp_invariants.py` | **40/40 通过 · BLOCKER 0 · WARN 0** | `erp_invariants.txt` |
| `bash scripts/datafix/e2e_residue2/apply.sh --check` | 28 PASS / 0 FAIL / 1 WARN | `apply_check.txt` |
| `bash scripts/datafix/e2e_residue/apply.sh --check`（上一张卡） | **25 PASS / 0 FAIL** | `prev_package_check.txt` |

## 6. 顺手修的一处「假红」（跨批次对账）

上一张卡的 `scripts/datafix/e2e_residue/30_verify.sql` 里有 8 条断言形如
「静态表总量 == 删除前 − 本卡删除数」。本卡又在同 9 张表上删了 301 行，
那 8 条断言的 expected 会立刻不符 —— 清完之后 `e2e_residue/apply.sh --check`
**永久 FAIL**（假红），那张卡的哨兵就废了。实测确实发生了（本次开发中亲眼见到
`FAIL|count_biz_flow_def|actual=22 expected=58` 等 8 条）。

修法（最小改动，不碰其它断言）：

1. 新增 `e2e_residue2/25_ledger.sql`：把本批次**实际删除行数**（`datafix_e2eres2_guard.deleted_*`，
   up 里已逐表与基线硬对账过）写成上一张卡基线表里的 `followup_deleted_<表名>` 项；幂等，
   单独成文件是因为 up 幂等空转时会提前 return，走不到账本那一段。
2. `e2e_residue/30_verify.sql` 的 8 处 expected 各多减一项 `followup_deleted_<表名>`。
3. `e2e_residue2/20_purge_residue2.down.sql` 回滚时把 `followup_deleted_*` 归零
   （回滚后上一张卡的口径要回到只减它自己那 85/37 行）。

结果：`e2e_residue/apply.sh --check` 回到 **25 PASS / 0 FAIL**；
它原来那两条 `out_of_scope_*` WARN 也变成了 **PASS（0 行）** —— 正是本卡消灭的两类。

## 7. 范围外（点名，本卡不动）

清完之后 `biz_flow_def` 里还有 **11 条软删定义**，全部 `laoliu_test` 建的、0 实例、0 引用：

```
eval_101604(评估流程)                probe_084617(探针)
verify_422_093032(422验证)           verify_r14_093032(R14验证)
p12_canvas_shot(P12 画布截图演示)     p12_e2e_*(×6, [P12-E2E] 发布回归)
```

同性质（测试/演示），但 `flow_code` **不在**卡面的前缀枚举里。按卡面红线
「不按前缀 / 不在枚举里直接判垃圾」，本卡不删；`e2e_residue2/30_verify.sql` 的
`out_of_scope_test_account_defs` 这条 WARN 会把它们点数（现在报 11 条），不会悄悄消失。

## 8. 回滚

```bash
bash scripts/datafix/e2e_residue2/apply.sh --rollback
```

数据来源 `datafix_e2eres2_snapshot`（batch = `E2ERES2-20261104`，301 行整行 jsonb）；
兜底 `pg_restore -d cloudpivot /root/datafix-backup-e2eres2-20261104-172821/flow_tables.dump`。
