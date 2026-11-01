# FLOWCLEAN · 清掉 6 个测试/演示审批流（保留 6 个真业务流）

一次性生产数据修复（2026-11-01）。**不是迁移**，不进 `backend/migrations/`。

## 背景

生产库 `biz_flow_def` 有 **12 条存活流程定义**，其中 6 条是历次验收/演示留下的：

| flow_code | 名称 | 状态 | 实例 | 待办 |
|---|---|---|---|---|
| `acc_tasknode_141800` | 验收-TASK_NODE-141800 | DRAFT | 0 | 0 |
| `acc_tn_141831` | 验收TASK节点-141831 | PUBLISHED | 1 | 1 |
| `d5_281866` | D5 校验发布演示 | PUBLISHED | 3 | 3 |
| `d5_282996` | D5 报销审批演示 | DRAFT | 0 | 0 |
| `e2e_cs_084913` | E2E会签测试 | PUBLISHED | 1 | 3 |
| `p12_canvas_demo` | P12 画布验收演示 | PUBLISHED | 0 | 0 |

保留的 6 条真业务流（一个字节都不许动）：
`task_accept` / `stock_transfer` / `stock_in_cancel` / `payable_pay` / `po_approve`（P-FLOW-3A）/
`stock_in_post`（P-FLOW-3A）。

## 脚本

| 文件 | 作用 |
|---|---|
| `00_backup.sh` | 删之前的全量备份：9 张 flow 表 `pg_dump -Fc` + 逐表 SQL + 目标/保留两套 CSV + MANIFEST（行数 / md5 / 库名 / 迁移版本） |
| `10_precheck.sql` | 硬闸 + 建快照表/基线表。闸①待清理必须 6 条（0 条 = 已清理，幂等放行）闸②保留的 6 条必须 PUBLISHED 闸③存活流程不许出现计划外条目 |
| `20_delete_test_flows.up.sql` | 整行快照 → 按「子表→主表」顺序物理删；每张表的删除行数与备份实测口径硬对账 |
| `20_delete_test_flows.down.sql` | 按快照原样写回（`jsonb_populate_record`），含 `biz_task.flow_instance_id` 回填 |
| `30_verify.sql` | 删后自检，每行 `PASS/FAIL/WARN`；有 FAIL 即退出码 1 |
| `apply.sh` | 备份闸 → precheck → up → verify；`--rollback` 走 down；`--check` 只读 |
| `drill.sh` | 在 **prod 库副本** 上跑「up → up（幂等）→ down（回滚后整表指纹逐字节等于 drill 前）→ up」 |

## 删除顺序（先子后父，与 FK 方向一致）

```
biz_task.flow_instance_id（外部引用，FK 是 ON DELETE SET NULL）
  → biz_flow_task → biz_flow_cc → biz_flow_instance_node → biz_flow_instance
  → biz_flow_edge → biz_flow_node_assignee → biz_flow_node
  → biz_flow_def_version → biz_flow_def
```

命中行数（2026-11-01 生产实测，脚本里做成硬校验）：

```
def 6 / version 12 / node 55 / node_assignee 6 / edge 47
instance 5 / instance_node 15 / task 7 / cc 0 / 外部引用 biz_task 1
```

## 为什么是物理删

- 老板要求「不要有残留」：这 6 条是验收/演示数据，无业务挂靠（实测 0 张收货/调拨/采购/应付引用它们的实例；
  只有 1 条测试任务 `验收任务-ACC-141831` 用 `flow_instance_id` 指着 `acc_tn_141831` 的实例，外键本身是 `ON DELETE SET NULL`，脚本显式解引用并留快照）。
- 可逆性不靠软删标志，靠 `datafix_flowclean_snapshot` 的整行快照 + `down` 脚本。

## 怎么跑

```bash
ssh root@172.18.10.144     # 生产库在 144
cd /opt/cloudpivot           # 或把本目录 scp 到 /tmp
bash scripts/datafix/flowclean/00_backup.sh          # ① 备份（必须）
bash scripts/datafix/flowclean/drill.sh              # ② 演练（prod 副本上 up/幂等/down）
bash scripts/datafix/flowclean/apply.sh              # ③ 生产应用（自带备份闸 + 前置校验 + 自检）
bash scripts/datafix/flowclean/apply.sh --check      # ④ 随时只看现状
```

## 回滚

```bash
bash scripts/datafix/flowclean/apply.sh --rollback
# 或手工： psql -1 -f 20_delete_test_flows.down.sql
```

- 数据来源是快照表 `datafix_flowclean_snapshot`（batch = `FLOWCLEAN-20261101`），
  整行写回，不是「按 id 重新插一遍」。
- 快照表确认不再需要回滚后可 `DROP TABLE datafix_flowclean_snapshot;`
  （`datafix_flowclean_guard` 是基线，可留作审计）。
- 兜底：`/root/datafix-backup-flowclean-<ts>/flow_tables.dump` 可用 `pg_restore` 整表恢复。

## 已知范围外残留（不在本卡范围）

存活实例里还有 **53 条孤儿实例**（`flow_code` 形如 `e2e_ap_*` / `e2e_cs_0846xx` / `e2e_single_*` /
`e2e_reject_*` / `diag_*` / `vrf_*`），它们的流程定义**早就被软删**（`deleted_at` 非空），当年只删定义没删实例。
本卡红线是「只删这 6 个，精确匹配」，所以没有动它们；`30_verify.sql` 的口径是
「孤儿实例数不得比删除前基线更多」，而不是「必须 0」。要收干净需另开一张卡（同样先备份再动手）。
