# E2ERES · 清掉生产库里的 e2e 测试残留（+ 环境守卫）

一次性生产数据修复（2026-11-04，卡 `t_61dca492`）。**不是迁移**，不进 `backend/migrations/`。

## 背景：37 行测试残留 + 85 条测试流程定义躺在生产库

P2-5（`t_e35c82ea`）做「业务类型词表值域巡检」时，对生产库（144 / `cloudpivot`）逐列 distinct，
发现 `biz_type_domain.py --strict` 常年红：

```
[FAIL] ② 审批动作  biz_flow_instance.biz_type  取值 44 种
       ✗ 域外值 'E2E_AP_E2EMVUGIUH0' × 1     （……共 37 个）
```

实测（2026-11-04 16:22，生产库只读盘点）：

| 表 | 命中 | 说明 |
|---|---|---|
| `biz_flow_instance` | **37 行，全部活行** | `biz_type` = `E2E_AP_<12位随机>`（36）+ 历史冒烟遗类；**用户能在「我发起的」里看见** |
| `biz_flow_def` | **85 行，全部已软删** | 82 个 `E2E_*` + 3 个 `D2_SMOKE`（`d2_prod_smoke` / 「D2 生产冒烟流程」） |
| `biz_flow_def_version` | 85 行 | 跟着定义走 |
| `biz_flow_node` / `biz_flow_edge` / `biz_flow_node_assignee` | 330 / 245 / 3 | 跟着版本走 |
| `biz_flow_instance_node` / `biz_flow_task` / `biz_flow_cc` | 107 / 37 / 0 | 跟着实例走 |
| `biz_status_log` | **0 行** | 审计证据表：命中测试残留口径的行是 0，**本卡一行未删** |

时间分布指向 2026-10-30 09:00–15:00 与 10-31 11:00–12:00 两批 e2e 跑批。
`frontend/e2e/05-approval.spec.ts` 里写着 `flow_code: e2e_ap_${RUN}` /
`biz_type: E2E_AP_${RUN.toUpperCase()}` —— 就是它的形状。

> 卡面写的是「`biz_flow_def` 里同类 E2E_* 共 132 行」。实测 **132 = 全部软删定义**（143 - 11 活），
> 其中命中测试残留口径的是 **85 行**；剩下 47 行是 `d5_*` / `diag_*` / `acc_*` / 以及
> `flow_code` 带 `e2e_` 前缀但 `biz_type` 是业务值（`EXPENSE` / `OTHER` / `TASK_ACCEPT`）的历史行。
> 本卡**只删前者**（判据见下），后者列进「范围外跟进」。

## 判据（唯一）：`biz_type` 是不是**测试脚本写的值**

```sql
left(biz_type, 4) = 'E2E_' OR biz_type = 'D2_SMOKE'
```

与 `scripts/verify/biz_type_domain.py::is_test_residue` 逐字一致；Rust 侧同一口径在
`backend/src/business/shared/doc_kind.rs`（`is_test_residue()` / `TEST_RESIDUE_SQL`），
`doc_kind_tests::test_residue_predicate_matches_python_and_sql` 钉住三边一致。

⚠️ **不是**「不在内置词表里就算垃圾」：`biz_type` 允许客户自定义，客户真流程同样不在
`COLUMN_DOMAINS` 的域里。本卡一个 `LIKE 'E2E%'` 的模糊匹配都没写。

## 脚本

| 文件 | 作用 |
|---|---|
| `00_backup.sh` | 删前备份：9 张 flow 表 + `biz_status_log` + `biz_task` 的 `pg_dump -Fc`，加**逐表 CSV 导出将要删掉的每一行**、`biz_status_log` 残留子集的导出（0 行也导，证明「不是忘了」）、MANIFEST（库身份 / 行数 / md5 / git HEAD） |
| `10_precheck.sql` | 7 道硬闸 + 快照表 / 基线表。闸①表都在 ②命中量 == 基线（0/0 = 幂等放行）③无在飞 PENDING ④测试定义不许被真业务实例引用 ⑤无 `prev_instance_id` 串链 ⑥`biz_task` 外键不许指向测试实例 ⑦存活流程定义正好 11 条 / 全 PUBLISHED / 无一命中残留口径 |
| `20_purge_test_residue.up.sql` | 整行快照 → 按「子 → 父」物理删（task → cc → instance_node → instance → edge → node_assignee → node → version → def），每张表受影响行数与基线硬对账；结尾断言 `biz_status_log` 行数没变 |
| `20_purge_test_residue.down.sql` | 按快照**整行**写回（不是按 id 重造），父 → 子顺序 |
| `30_verify.sql` | 删后自检：残留 0 / 无孤儿 / 11 条存活定义**指纹逐字节一致** / 静态表总量 == 删除前 - 删除数 / 运行期表只允许「比预期多」/ 审计表行数不变；有 `FAIL` 即退出码 1 |
| `apply.sh` | 备份闸 → 10 → 20 → 30；`--check` 只读；`--rollback` 走 down |
| `drill.sh` | 在**生产库副本**（`cloudpivot_e2eres_drill`）上跑 up → up（幂等）→ down（整表指纹逐字节对比）→ up |

应用顺序（与 FK 方向一致）：

```
biz_flow_task → biz_flow_cc → biz_flow_instance_node → biz_flow_instance
  → biz_flow_edge → biz_flow_node_assignee → biz_flow_node
  → biz_flow_def_version → biz_flow_def
```

`biz_flow_instance.version_id → biz_flow_def_version` 是 `RESTRICT`，所以**实例必须先删**；
`biz_flow_def.current_version_id` 没有外键，版本删完再删定义即可。

## 怎么跑（生产 = 144）

```bash
ssh root@172.18.10.144
cd /opt/cloudpivot
bash scripts/datafix/e2e_residue/00_backup.sh          # ① 备份（必须）
bash scripts/datafix/e2e_residue/drill.sh             # ② 副本上演练（up/幂等/down/再 up）
bash scripts/datafix/e2e_residue/apply.sh             # ③ 生产应用（自带备份闸 + 前置校验 + 自检）
bash scripts/datafix/e2e_residue/apply.sh --check     # ④ 随时只看现状
```

## 回滚

```bash
bash scripts/datafix/e2e_residue/apply.sh --rollback
# 或： psql -1 -f scripts/datafix/e2e_residue/20_purge_test_residue.down.sql
```

- 数据来源是 `datafix_e2e_residue_snapshot`（batch = `E2ERES-20261104`，929 行整行 jsonb）。
- 兜底：`/root/datafix-backup-e2eres-20261104-162243/flow_tables.dump`（`pg_restore` 整表恢复）。
- 快照表确认不再需要后可 `DROP TABLE datafix_e2e_residue_snapshot;`
  （`datafix_e2e_residue_guard` 是基线，留着审计）。

## 演练结果（副本，2026-11-04 16:22）

```
① up 生效       ：指纹已变化
② up 幂等       ：第二次 0 行受影响
③ down 原样恢复 ：9 张 flow 表 + biz_status_log 指纹逐字节等于演练前
④ 回滚后可重来  ：up #3 自检无 FAIL
```

## 生产结果（2026-11-04 16:22:46）

```
删除：def 85 / version 85 / node 330 / edge 245 / node_assignee 3
      instance 37 / instance_node 107 / task 37 / cc 0
未动：biz_status_log 113 → 113（审计证据表一行未删）
保留：11 条存活流程定义，指纹 7fcb32a3… 前后一致；8/8 已发布
巡检：biz_type_domain.py --strict  迁移前 FAIL（37 个 E2E_AP_* 域外值）→ 迁移后 PASS
```

## 范围外残留（点名，不在本卡）

清场后仍有 16 条**存活实例**的流程定义已被软删（清场前 53 条，其中 37 条就是本卡删掉的
E2E 实例），它们的 `flow_code` 是 `e2e_reject_*` / `e2e_single_*` / `e2e_cs_*` / `diag*` / `vrf_*`，
但 `biz_type` 是业务值（`OTHER`），按本卡判据**不算测试残留**，动它们就等于「按 flow_code 猜垃圾」。
另有 36 条 `flow_code` 带 `e2e_/diag_/vrf_/d5_/acc_` 前缀的软删定义同属此列。
跟进卡见 `t_61dca492` 的完成说明（`30_verify.sql` 会把这两项以 `WARN` 打出来，不会悄悄消失）。

## 跨批次对账（2026-11-04 补充，卡 t_142317a2 / `scripts/datafix/e2e_residue2/`）

本卡的 `30_verify.sql` 有 8 条「静态表总量 == 删除前 − 本卡删除数」的断言。
后续的 E2ERES2 又在同 9 张表上物理删了 301 行（36 定义 / 39 版本 / 98 节点 / 55 边 /
12 审批人 / 16 实例 / 37 实例节点 / 8 待办 / 0 抄送），所以：

* `30_verify.sql` 的 8 处 expected 现在多减一项 `followup_deleted_<表名>`；
* 这一项由 `e2e_residue2/25_ledger.sql` 写进 `datafix_e2e_residue_guard`
  （batch 仍是 `E2ERES-20261104`），`e2e_residue2` 的 down 会把它归零；
* 本卡的 `before` / `deleted_*` / `keepers_*` 基线**一个字节都没改**（历史留痕不动）。

不这么做的话，本卡的 `apply.sh --check` 在 E2ERES2 之后会永久 FAIL（假红），
哨兵就废了。补完之后实测 `apply.sh --check` = 25 PASS / 0 FAIL，
原来那两条 `out_of_scope_*` WARN 也变成 PASS（0 行）—— 正是 E2ERES2 消灭的两类。
