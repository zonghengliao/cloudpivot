# E2ERES2 · 清掉「上一张卡判不了」的两类测试痕迹（36 软删定义 + 16 孤儿实例）

一次性生产数据修复（2026-11-04，卡 `t_142317a2`）。**不是迁移**，不进 `backend/migrations/`。
承接 `scripts/datafix/e2e_residue/`（卡 `t_61dca492`）—— 那张卡只清了 `biz_type` 能证明是测试脚本写的行。

## 背景：清完 E2E_* 之后，`30_verify.sql` 还留着两条 WARN

| 痕迹 | 数量 | 为什么上一张卡不敢删 |
|---|---|---|
| 软删定义，`flow_code` 带 `e2e_/diag_/diag2_/vrf_/d5_` 前缀，但 `biz_type` 是**业务值** | **36** | 判据 `left(biz_type,4)='E2E_' OR biz_type='D2_SMOKE'` 抓不到；只按前缀删 = 猜垃圾 |
| **活**实例，但 `def_id` 指向已软删的定义（孤儿实例），`biz_type` 也是业务值 | **16** | 同上 |

2026-11-04 生产只读盘点（`cloudpivot` @ 144）：

| 表 | 命中 | 说明 |
|---|---|---|
| `biz_flow_def` | 36 行，**全部已软删** | 29 个 `laoliu_test` 建的 e2e/diag/vrf + 7 个 `admin` 建的 `d5_*` 演示 |
| `biz_flow_def_version` / `biz_flow_node` / `biz_flow_edge` / `biz_flow_node_assignee` | 39 / 98 / 55 / 12 | 跟着定义走 |
| `biz_flow_instance` | 16 行，**全部活行** | 状态 APPROVED 13 / CANCELLED 3，**PENDING 0** |
| `biz_flow_instance_node` / `biz_flow_task` / `biz_flow_cc` | 37 / 8 / 0 | 跟着实例走 |
| `biz_status_log` | **0 行** | 审计证据表：指向这 16 条实例的行是 0，**本卡一行未删** |

合计要删 **301 行**（36+39+98+55+12+16+37+8+0）。
去掉后 `biz_flow_def` 从 58 → 22 行（11 活 + 11 其它软删，见 §5）。

## 判据：**先判性质，再动手**（前缀只是候选）

本包**没有**一条「`flow_code` 像测试就删」的规则。能删是因为逐条看了四件事，
证据在备份目录的 `target/classification.csv`（`00_backup.sh` 生成）：

1. **`create_by`** — 29 条是 `laoliu_test`（专用测试账号），7 条是 `admin`（D5 卡演示时建的）。
2. **`flow_name`** — 全部是 `E2E单签测试` / `E2E驳回测试` / `E2E会签测试` / `空图` / `诊断` /
   `诊断2` / `验证单签` / `验证驳回` / `验证会签` / `验证空图` / `D5 报销审批演示` /
   `D5 校验发布演示` / `D5 验证流程`。没有一个像客户真实业务名。
3. **`created_at`** — 集中在 2026-10-30 08:46–09:30（e2e 跑批）与 2026-10-29 21:38–22:09（D5 卡演示），
   与 `docs/evidence/20261104-e2e-residue/README.md` 里那两批时间分布吻合。
4. **业务单据引用 = 0** — 11 处多态引用列全查过（`biz_purchase_order.approval_instance_id`、
   `biz_stock_in/stock_take/stock_transfer.approval_instance_id`、`biz_contract`、`biz_payable`(×2)、
   `biz_payment`、`biz_task.flow_instance_id/approval_instance_id`、`biz_voucher.pending_reverse_instance_id`），
   合计 0 条；`biz_status_log` 按 `entity_id` / `entity_no` 也 0 条。

判决：**16 + 36 = 全部是测试/演示数据，客户真实自定义 0 条**。

## 判据的代码形态（单一来源）

`05_views.sql` 建两个视图，`10 / 20 / 30 / 00_backup.sh` 全部照这一份口径：

```sql
-- SET A-1：目标定义
deleted_at is not null
and not (left(biz_type,4) = 'E2E_' or biz_type = 'D2_SMOKE')   -- 上一张卡的口径，本卡不碰
and flow_code ~ '^(e2e|diag|diag2|vrf|d5|acc|tn)_'

-- SET A-2：目标实例（孤儿实例）
deleted_at is null and def_id in (SET A-1)
```

⚠️ 前缀枚举里的 `acc` / `tn` 实测**命中 0 行**（卡面提到它们只是因为复用了 `30_verify.sql` 的正则）。

## 脚本

| 文件 | 作用 |
|---|---|
| `05_views.sql` | 判据视图（`datafix_e2eres2_v_target_def` / `_inst`）。`create or replace`，幂等 |
| `00_backup.sh` | 删前备份：9 张 flow 表 + `biz_status_log` + `biz_task` 的 `pg_dump -Fc`，逐表 CSV 导出将要删的每一行，**判决表 `classification.csv`**，`biz_status_log` 指向目标实例的行（0 行也导），MANIFEST（库身份 / 行数 / md5 / git HEAD） |
| `10_precheck.sql` | 9 道硬闸 + **目标集钉表** + 基线表。闸①表都在 ②命中量==基线（或全 0 幂等）且判据没漂移 ③目标实例不许 PENDING / 在飞审批不许挂目标定义 ④目标定义下的实例必须全在目标集内 ⑤`prev_instance_id` 不许串链 ⑥11 处单据引用 + `biz_task` 外键必须 0 ⑦存活定义 11 条 / 全 PUBLISHED / 指纹 ⑧审计表行数与指纹 ⑨`transferred_from` / 实例节点 `node_id` 不许指向目标行 |
| `20_purge_residue2.up.sql` | 整行快照 → 按「子 → 父」物理删 → 每张表受影响行数与基线硬对账 → 断言 `biz_status_log` 行数与指纹没变 |
| `20_purge_residue2.down.sql` | 按快照**整行**写回（不是按 id 重造），父 → 子顺序；末尾把跨批次账本 `followup_deleted_*` 归零 |
| `25_ledger.sql` | **跨批次对账**：把本批次实际删除量登记进上一张卡（E2ERES）的基线表，让 `e2e_residue/30_verify.sql` 的「总量 == 删除前 − 删除数」断言在跨批次之后仍然成立（否则那张卡的 `--check` 会永久假红）。幂等，可单独重跑 |
| `30_verify.sql` | 删后自检：目标行 0 / 无孤儿 / 活实例的父定义已软删 == 0 / 11 条 keeper 指纹逐字节一致 / 静态表总量==删除前-删除数 / 运行期表只允许更多 / 审计表行数+指纹不变；有 `FAIL` 即退出码 1 |
| `40_http_check.py` | 生产接口复核（只读）：登录 admin → `approval-flow/health` 8/8 + `flow/def/page` 11 条无测试前缀 + `flow/task/todo` 200 |
| `apply.sh` | 备份闸 → 10 → 20 → 25 → 30；`--check` 只读；`--rollback` 走 down |
| `drill.sh` | 在**生产库副本**（`cloudpivot_e2eres2_drill`）上跑 up → up（幂等）→ down（指纹**和逐表行数**逐字节对比 + 软删状态校验）→ 再 up。`E2ERES2_SRC_DB=` 可指定源库（生产已清过时用它指向「删除之前」的副本） |

应用顺序（与 FK 方向一致）：

```
biz_flow_task → biz_flow_cc → biz_flow_instance_node → biz_flow_instance
  → biz_flow_edge → biz_flow_node_assignee → biz_flow_node
  → biz_flow_def_version → biz_flow_def
```

外键实测（`pg_constraint`）：

* `biz_flow_instance.def_id → biz_flow_def` 是 **RESTRICT** ⇒ 实例必须先删
* `biz_flow_instance.version_id → biz_flow_def_version` 是 **RESTRICT** ⇒ 实例必须先删
* `biz_flow_def.current_version_id` **没有**外键，版本删完再删定义即可
* `biz_flow_instance_node.node_id → biz_flow_node` 是 **SET NULL** ⇒ 实例节点必须先删
* `biz_task.flow_instance_id → biz_flow_instance` 是 **SET NULL** ⇒ 闸⑥ 断言 0 条（会动业务表）

## 怎么跑（生产 = 144）

```bash
ssh root@172.18.10.144
cd /opt/cloudpivot
bash scripts/datafix/e2e_residue2/00_backup.sh     # ① 备份（必须）
bash scripts/datafix/e2e_residue2/drill.sh         # ② 副本上演练（up/幂等/down/再 up）
bash scripts/datafix/e2e_residue2/apply.sh         # ③ 生产应用（备份闸 + 前置校验 + 自检）
bash scripts/datafix/e2e_residue2/apply.sh --check # ④ 随时只看现状
```

## 回滚

```bash
bash scripts/datafix/e2e_residue2/apply.sh --rollback
# 或： psql -1 -f scripts/datafix/e2e_residue2/20_purge_residue2.down.sql
```

* 数据来源是 `datafix_e2eres2_snapshot`（batch = `E2ERES2-20261104`，301 行整行 jsonb）。
* 兜底：`/root/datafix-backup-e2eres2-<TS>/flow_tables.dump`（`pg_restore` 整表恢复）。
* 确认不再需要后可 `DROP TABLE datafix_e2eres2_snapshot;` 和两张目标/基线表；
  `datafix_e2eres2_guard` / `_target` 建议留着审计。

## 结果

见 `docs/evidence/20261104-e2eres2/README.md`。

生产实删：def 36 / version 39 / node 98 / edge 55 / asg 12 / inst 16 / inode 37 / task 8 / cc 0（= 301 行），
`biz_status_log` 113 → 113 一行未删，11 条 keeper 指纹 `7fcb32a3…` 前后一致。

## 跨批次对账（不修就会留下一条永久假红）

上一张卡的 `e2e_residue/30_verify.sql` 有 8 条「静态表总量 == 删除前 − 本卡删除数」的断言。
本包又在同 9 张表上删了 301 行 ⇒ 清完之后 `e2e_residue/apply.sh --check` 会 **永久 FAIL**
（实测 `FAIL|count_biz_flow_def|actual=22 expected=58` 等 8 条），那张卡的哨兵就废了。

修法是三小步，不动其它断言：

1. `25_ledger.sql` 把本批次实际删除量写成 E2ERES 基线表的 `followup_deleted_<表名>`；
2. `e2e_residue/30_verify.sql` 的 8 处 expected 各多减一项 `followup_deleted_<表名>`；
3. `20_purge_residue2.down.sql` 回滚时把 `followup_deleted_*` 归零。

修完后 `e2e_residue/apply.sh --check` = **25 PASS / 0 FAIL**，它原来那两条
`out_of_scope_*` WARN 也变成 PASS（0 行）—— 正是本卡消灭的两类。

## 范围外（点名，本卡不动）

清完之后 `biz_flow_def` 里还有 **11 条软删定义**，全部是 `laoliu_test` 建的、0 实例、0 引用：

```
eval_101604(评估流程)  probe_084617(探针)  verify_422_093032(422验证)  verify_r14_093032(R14验证)
p12_canvas_shot(P12 画布截图演示)  p12_e2e_*(×6, [P12-E2E] 发布回归)
```

它们**同性质**（测试/演示），但 `flow_code` **不在**卡面的前缀枚举里 ——
按卡面红线「不按前缀 / 不在枚举里直接判垃圾」，本卡不删，`30_verify.sql` 用
`out_of_scope_test_account_defs` 这条 WARN 把它们点出来，不会悄悄消失。
