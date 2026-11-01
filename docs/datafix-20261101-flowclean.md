# 数据修复留痕 · 清理 6 个测试/演示审批流（2026-11-01）

> 卡片：`t_eda42cad`【清理】删掉 6 个测试/演示流程（保留 6 个业务流，备份+幂等+可逆）
> 执行：backend-developer · 生产库 `cloudpivot` @ 172.18.10.144 · 2026-11-01 16:19–16:20 CST
> 脚本：`scripts/datafix/flowclean/`；证据：`docs/evidence/20261101-flowclean/`

## 一、结论

生产库 `biz_flow_def` 的存活定义从 **12 条 → 6 条**，剩下的 6 条全部是 `PUBLISHED + is_default=true` 的真业务流。
被删的 6 条在 9 张 flow 表里 **0 行残留**；孤儿待办 **0 条**；不变量套件 **31/32（BLOCKER 0）不退**。

接口复核（真实 HTTP，admin 登录后 `GET /api/business/flow/def/page`）：`total = 6`，恰好这 6 条 —— 前端流程列表不会再有测试流程。

## 二、删了哪 6 个（各自实例/待办）

| flow_code | 名称 | 原状态 | 版本 | 节点 | 边 | 实例 | 实例节点 | 待办 |
|---|---|---|---|---|---|---|---|---|
| `acc_tasknode_141800` | 验收-TASK_NODE-141800 | DRAFT | 1 | 4 | 3 | 0 | 0 | 0 |
| `acc_tn_141831` | 验收TASK节点-141831 | PUBLISHED | 1 | 4 | 3 | 1 | 4 | 1 |
| `d5_281866` | D5 校验发布演示 | PUBLISHED | 5 | 15 | 10 | 3 | 9 | 3 |
| `d5_282996` | D5 报销审批演示 | DRAFT | 1 | 4 | 3 | 0 | 0 | 0 |
| `e2e_cs_084913` | E2E会签测试 | PUBLISHED | 2 | 6 | 4 | 1 | 2 | 3 |
| `p12_canvas_demo` | P12 画布验收演示 | PUBLISHED | 2 | 22 | 24 | 0 | 0 | 0 |
| **合计** | | | **12** | **55** | **47** | **5** | **15** | **7** |

外加 6 行 `biz_flow_node_assignee`（都挂在 `e2e_cs_084913` 的节点上）、0 行 `biz_flow_cc`，
以及 **1 条** `biz_task` 的外部引用（见 §六）。

## 三、保留的 6 个（一个字节都没动）

删除前后都做了整行指纹基线（`datafix_flowclean_guard`），`30_verify.sql` 逐条对账 PASS：

```
PASS|keepers_def_md5|payable_pay=a534f6a0 po_approve=31197f7b stock_in_cancel=0c7f944b
                     stock_in_post=2f5684d8 stock_transfer=b8413a2f task_accept=4dc3b749
PASS|keepers_versions_md5|基线 aeb3a235
PASS|keepers_nodes_md5|基线 792fdf34
PASS|keepers_edges_md5|基线 36d16c09
PASS|keepers_instances_md5|基线 89a8c6a6
PASS|keepers_tasks_md5|基线 56ebd04c
```

| flow_code | 名称 | 状态 | default |
|---|---|---|---|
| `payable_pay` | 应付付款审批 | PUBLISHED | true |
| `po_approve` | 采购订单审批（标准·金额分级）← P-FLOW-3A | PUBLISHED | true |
| `stock_in_cancel` | 收货单作废审批 | PUBLISHED | true |
| `stock_in_post` | 收货过账审批（标准·金额分级）← P-FLOW-3A | PUBLISHED | true |
| `stock_transfer` | 库存调拨审批 | PUBLISHED | true |
| `task_accept` | 任务验收 | PUBLISHED | true |

## 四、备份在哪

```
/root/datafix-backup-flowclean-20261101-161944/        （144 上）
├── flow_tables.dump      9 张 flow 表 custom-format（224K，pg_restore 用）
├── <表>.sql              逐表纯 SQL（含 CREATE TABLE，可单独回放到空库）
├── <表>.rows             每张表总行数
├── target/<表>.csv       只含这 6 个流程的行（csv header，人工可读）
├── keeper/<表>.csv       只含 6 个保留流程的行（事后校对「没动过」）
└── MANIFEST.txt          库名/主机/迁移版本(182)/逐表行数/md5/目标范围行数
```

目标范围行数（与 drill、与删除时的硬校验完全一致）：

```
biz_flow_def.csv 6            biz_flow_def_version.csv 12
biz_flow_node.csv 55          biz_flow_node_assignee.csv 6
biz_flow_edge.csv 47          biz_flow_instance.csv 5
biz_flow_instance_node.csv 15 biz_flow_task.csv 7
biz_flow_cc.csv 0             biz_task_ref.csv 1
```

另有一份快照在库里：`datafix_flowclean_snapshot`（batch = `FLOWCLEAN-20261101`，**154 行整行快照**），
这是 `down` 脚本的数据来源。

## 五、怎么回滚

```bash
ssh root@172.18.10.144
cd /opt/cloudpivot    # 或 /tmp/flowclean
bash scripts/datafix/flowclean/apply.sh --rollback
```

- `down` 按 `datafix_flowclean_snapshot` 的整行快照**原样写回**（`jsonb_populate_record`），
  不是「按 id 重新插一遍」：`created_at / updated_at / graph_hash / graph_snapshot / create_by` 全部还原。
- 演练实测：回滚后 10 张表（含 `biz_task`）的整表 md5 **逐字节等于**删除前（见 §七 ④）。
- 兜底方案：`pg_restore -t biz_flow_def ... flow_tables.dump` 整表恢复。
- 快照表确认不再需要可 `DROP TABLE datafix_flowclean_snapshot;`（`datafix_flowclean_guard` 建议留作审计）。

## 六、两个需要知道的副作用（都已快照，回滚可还原）

1. **`biz_task` 有一条测试任务被解引用**：`TSK-202611-000001 / 验收任务-ACC-141831`（DONE，ACC-141831 那次验收造的任务）
   的 `flow_instance_id` 指着 `acc_tn_141831` 的实例。`biz_task` 上有两条 CHECK：
   - `ck_biz_task_flow_triplet`：`(flow_instance_id, flow_node_key, flow_round)` 必须同空同非空；
   - `ck_biz_task_source_flow`：`(source='FLOW')` 等价于 `(flow_instance_id IS NOT NULL)`。

   所以**只把 `flow_instance_id` 置空会直接违约**（第一版脚本就是这么在演练库上炸的）。
   最终做法：四列一起改 `flow_instance_id / flow_node_key / flow_round → NULL`、`source FLOW → SELF`，
   整行进快照，`down` 四列原样写回。删除后该任务：

   ```
   TSK-202611-000001|验收任务-ACC-141831|status=DONE|source=SELF|flow_instance_id=NULL|flow_node_key=NULL|flow_round=NULL
   ```

2. **为什么物理删而不是软删**：老板要「不要有残留」；这 6 条无业务挂靠（实测 0 张收货/调拨/采购/应付引用了它们的实例）。
   可逆性不靠软删标志，靠整行快照 + `down`。

## 七、演练 → 生产（原始输出）

演练库 `cloudpivot_flowclean_drill`（prod 的 `pg_dump` 副本）上跑完整四步，全绿后才动生产。
日志：`docs/evidence/20261101-flowclean/flowclean-drill-*.log.txt`。

### ① 第一遍 apply（drill）

```
NOTICE: FLOWCLEAN/10: 前置校验通过（待清理 6 条 / 保留 6 条），基线 14 项已就位
NOTICE: FLOWCLEAN/20: 解除 1 条 biz_task 的 flow 三元组引用（… 快照已存，down 可写回）
NOTICE: FLOWCLEAN/20: 已清理 6 个流程（def 6 / version 12 / node 55 / assignee 6 / edge 47 /
        instance 5 / instance_node 15 / task 7 / cc 0），快照新增 154 行
```

### ② 第二遍 apply（幂等）

```
NOTICE: FLOWCLEAN/10: 6 个待清理流程已不存在 —— 幂等放行（本步不改任何数据）
NOTICE: FLOWCLEAN/20: 6 个流程已清理完毕 —— 幂等跳过（本步 0 行受影响）
✓ 幂等：第二遍 0 行受影响
```

### ③ rollback（可逆）

```
biz_flow_def 6 / version 12 / node 55 / assignee 6 / edge 47 / instance 5 / instance_node 15 /
task 7 / cc 0 / biz_task 三元组 1 —— 写回 154 行
✓ 可逆：回滚后 10 张表逐字节等于 drill 之前
```

### ④ 回滚后再 apply：仍全绿（证明可重复施加）

### ⑤ 生产 apply（16:19:48，`flowclean-prod-apply.log.txt`）

```
== 现状快照 ==
存活流程定义 = 12 / 待清理 6 个 = 6 / 保留 6 个（PUBLISHED）= 6
实例总数 / 孤儿实例 = 150 / 53        待办总数 / 孤儿待办 = 148 / 0
flow 家族行数 def/ver/node/edge/asg/inst/inode/task = 144/153/543/397/27/150/423/148
==> 使用指定备份：/root/datafix-backup-flowclean-20261101-161944
NOTICE: FLOWCLEAN/10: 前置校验通过（待清理 6 条 / 保留 6 条），基线 14 项已就位
NOTICE: FLOWCLEAN/20: 已清理 6 个流程（…），快照新增 154 行
---- 30_verify.sql 自检 ----
PASS|alive_flow_def=6|实际 6 条存活流程定义
PASS|keepers_published_default|匹配 6 条（期望 6）
PASS|no_unexpected_alive_def|无计划外流程
PASS|deleted_flows_zero_residue|合计 0 行残留 [biz_flow_cc=0 biz_flow_def=0 biz_flow_def_version=0
     biz_flow_edge=0 biz_flow_instance=0 biz_flow_instance_node=0 biz_flow_node=0
     biz_flow_node_assignee=0 biz_flow_task=0 biz_task_flow_ref=0]
PASS|keepers_def_md5 / versions_md5 / nodes_md5 / edges_md5 / instances_md5 / tasks_md5
PASS|orphan_pending_tasks=0|实际 0 条
PASS|no_new_orphan_instances|现在 53 条 / 删除前基线 53 条（不变或减少才算过）
PASS|rollback_snapshot_rows|154 行快照
✓ 自检全绿（FAIL=0）
---- 现状快照（删后）----
存活流程定义 = 6 / 待清理 6 个（存活）= 0 / 保留 6 个（PUBLISHED）= 6
实例总数 / 孤儿实例 = 145 / 53       待办总数 / 孤儿待办 = 141 / 0
flow 家族行数 = 138/141/488/350/21/145/408/141
```

### ⑥ 生产第二遍（幂等，`flowclean-prod-apply2.log.txt`）

```
NOTICE: FLOWCLEAN/20: 6 个流程已清理完毕 —— 幂等跳过（本步 0 行受影响）
PASS|alive_flow_def=6 / PASS|deleted_flows_zero_residue
✓ 自检全绿（FAIL=0）
```

### ⑦ 接口复核（`flowclean-http-check.txt`）

```
登录 = 200 code = 0
GET /api/business/flow/def/page = 200 code = 0
total = 6  items = 6
  stock_in_post      PUBLISHED default=True v1 收货过账审批（标准·金额分级）
  po_approve         PUBLISHED default=True v1 采购订单审批（标准·金额分级）
  payable_pay        PUBLISHED default=True v1 应付付款审批
  stock_in_cancel    PUBLISHED default=True v1 收货单作废审批
  stock_transfer     PUBLISHED default=True v1 库存调拨审批
  task_accept        PUBLISHED default=True v1 任务验收
PASS 存活流程 == 6 个业务流
```

### ⑧ 不变量套件（`erp-invariants-after.txt`）

删除前 / 删除后都是 **31/32 通过，BLOCKER 0，WARN 1**（WARN 是历史项 `F2 凭证期间必有期间记录 — 2 张凭证的期间无记录`，与本次无关）。

### ⑨ 最终状态（`final-state.txt`）

```
存活流程 = 6（全部 PUBLISHED + default）
6 个已删流程残留 = 0
flow 家族行数 def/ver/node/edge/asg/inst/inode/task/cc = 138/141/488/350/21/145/408/141/0
snapshot=154  guard=14
```

## 八、范围外残留（不在本卡红线内，需另开卡）

存活实例里还有 **53 条孤儿实例**（`e2e_ap_*` 37 条、`e2e_cs_0846xx/0847xx` 3 条、
`e2e_single_*` 4 条、`e2e_reject_*` 4 条、`diag_*`/`diag2_*` 2 条、`vrf_*` 3 条 —— 共 53；
实测每一条的 flow_code 都能对上一个 `deleted_at` 非空的定义：全库 132 条软删定义）。
它们的流程定义**当年就被软删了**（`biz_flow_def.deleted_at` 非空，全库 132 条软删定义），
那批操作只软删定义、没删实例，于是留下这些「孤儿」；它们的待办已全部终态（孤儿待办 = 0）。

本卡红线是「只删这 6 个 + 用 flow_code 精确匹配 + 不许模糊删」，所以**没有动它们**，
`30_verify.sql` 的口径也写成「孤儿实例数不得比删除前基线更多（53 → 53）」而不是「必须 0」。
要彻底收干净建议另开一张卡，做法同本卡：先 `00_backup.sh`，再按「def_id 指向已软删定义」的实例集收敛删除，
down 仍用整行快照。
