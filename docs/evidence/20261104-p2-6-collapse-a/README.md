# P2-6 收口 A · 巡检口径（DI-19 按状态分级 + DI-18 精确豁免台账）

- 卡号：`t_237f1caa`（后台 backend-developer）
- 上游决定：卡 `t_3de61e4b` 的 **D-1（BLOCKER 1）/ D-2（WARN 47）/ D-3（INFO 32）** —— 生产库**一行数据都不改**，
  只改巡检分级 + 加精确豁免台账
- 代码提交：`7f0fe5c`（bare `144:/opt/git/cloudpivot.git` 与 github `zonghengliao/cloudpivot` 的 `main` 均已同步）
- 改动文件：`docs/architecture/snapshot-drift/10_checks.sql`、`docs/architecture/快照一致性巡检与告警设计.md`、`scripts/verify/snapshot_audit.py`

---

## 1. 改了什么（一句话各一条）

### ① DI-19 从「恒 WARN」改成「按实例状态分级」

`di.c_di19` `LEFT JOIN biz_flow_instance i ON d.src_table='biz_flow_instance' AND i.id = d.src_id`，按 4 档定级：

| 命中形态 | 级别 | 理由 |
|---|---|---|
| `DANGLING` + `i.status='PENDING'` | **WARN** | 活待办挂在死对象上：审批人批的是一个不存在的东西 |
| `DANGLING` + 终态（APPROVED/CANCELLED/WITHDRAWN/REJECTED/将来新增的） | **INFO** | 终态实例是审计证据，不随业务对象删除（T12 / ARCH-3 R-P1-1）；**仍逐条列出** |
| `MISSING_ID`（有 `biz_type` 无 `biz_id`）/ `NO_TARGET` | **WARN** | 与实例状态无关，不能用「终态」解释 |
| 实例行读不到（LEFT JOIN 落空） | **WARN** | 判不出状态就不降级（宁可报） |

依据（生产代码本就是这个口径）：`backend/src/business/flow/task_bridge.rs:650` 任务删除只终止 PENDING 的审批；
`backend/src/business/erp/contract_handler.rs::cancel_approval_of` 合同删除同理。

### ② DI-18 新增 `di.v_poly_ref_waiver`（精确豁免台账）

`CREATE OR REPLACE VIEW ... FROM (VALUES ...)`（**不是表**：豁免必须走 git review + 发版，不能让谁在库里加一行就关掉告警），
7 列 `check_id, src_table, src_id, reason, checked_by, checked_on, expires_on`。

`di.c_di18` 对命中行 `LEFT JOIN` 台账，**命中且 `expires_on > current_date`** ⇒ severity 降 `INFO`，
`snapshot_value` 前缀 `已核查豁免#<checked_on>：`（原值保留）；过期后 JOIN 不再命中 ⇒ **自动回 ERROR**，无任一额外逻辑。

首条（唯一一条）：

```
DI-18 / biz_inventory_txn / 01a19108-ee53-73c6-bdaa-0109614ea963
reason：DATAFIX-20261101-P14 修数留档：+10/-10 对平、库存净 0、成本 0、修数前快照在 datafix_p14_snapshot；流水保留作审计证据
checked_by=manong  checked_on=2026-11-04  expires_on=2027-02-04
```

### ③ 文档与脚本口径同步

- 设计文档新增 **§2.4c 已核查豁免与按状态分级**（豁免三硬约束 / 到期自动回红 / 终态降 INFO 的理由 / INFO 不是待办），
  并同步 §2.4b 分级论证表、§2.4b 清单表、§2.5 总表里 DI-18 / DI-19 两行；
- `scripts/verify/snapshot_audit.py`：DI-19 的 `level="WARN"` **保留**（最坏级别声明），`name`/`meaning` 按新分级改写；
  DI-18 的 `name`/`meaning` 补上豁免口径；
- `COMMENT ON VIEW di.c_di18 / di.c_di19` 同步改写 —— 旧注释「审批中心点进去 404」经查**不成立**（实例详情页读 `form_data`
  冻结快照，不请求业务单），已删除。

**11 列契约未动**（`issue_code…affected_count`，25 条 `c_di*` 视图实测全部 11 列）；`di.v_poly_ref_map` 既有登记未动。

---

## 2. 验收逐条对照（全部真实输出，文件在同目录）

| # | 验收要求 | 结果 | 证据文件 |
|---|---|---|---|
| 1 | 隔离库 `cloudpivot_p26` `--group 多态引用` 跑到 22/22 通过 | **25/25 通过**（22 条本卡口径 + P2-6b 在本卡之前已落地的 DI-23/24/25 —— 见 §4 偏差说明） | `iso-00-baseline-green.txt`、`iso-09-cleanup-green.txt` |
| 2① | PENDING 实例 + 软删业务对象 ⇒ DI-19 **WARN** | `FLW-ISO-0001`（PENDING）`[DEFECT/WARN]` | `iso-02-graded-and-waived.txt` |
| 2② | 终态实例 + 软删业务对象 ⇒ DI-19 **INFO** | `FLW-ISO-0002`（CANCELLED）`[DEFECT/INFO]`，同一批次同时输出 | `iso-02-graded-and-waived.txt` |
| 2③ | 豁免行 `expires_on` 改成昨天 ⇒ DI-18 回 **ERROR** | `expires_on > current_date` 由 `true`→`false`，DI-18 `[DEFECT/ERROR]`、汇总 `BLOCKER 1`、退出码 1，`已核查豁免#` 前缀消失 | `iso-03-waiver-expiry-*.txt`、`iso-04-waiver-expired-error.txt` |
| 2+ | （加做）精确性反证：同 `doc_type=SERIAL_BATCH` 同目标表、仅行 id 不在台账 ⇒ 仍 ERROR | 2 条命中：`ISO-DI18-NOWAIVER` = ERROR + 被豁免那条 = INFO（同屏对比） | `iso-06-precision-fixture.txt`、`iso-07-precision-error.txt` |
| 3 | 生产只读复跑（144）⇒ **BLOCKER 0 / WARN 0**，DI-18 带 `已核查豁免#`，DI-19 47 条 INFO | **BLOCKER 0 / WARN 0 / INFO 80**；DI-18 2 条 INFO（1 条 `已核查豁免#2026-11-04：` + 1 条本来就 INFO 的 ADJUST/NO_TARGET）；DI-19 **47 条 INFO** | `prod-snapshot-audit-polyref.txt`、`prod-verify-readonly.txt` |
| 4 | 新增 SQL 段 `grep -cwE "UPDATE\|INSERT\|DELETE"` = 0 | **0**（整文件仅 1 处，是第 12 行既有注释提到 `20_upsert_converge.sql` 的写法，非本文件内容） | `readonly-proof.txt` |

生产 DI-19 的 47 条按实例状态分布（只读实测，`prod-verify-readonly.txt` §C）：

```
 APPROVED  | 27
 CANCELLED | 16
 WITHDRAWN |  3
 REJECTED  |  1
severity: INFO 47        ← PENDING 0 条
```

生产 DI-18 两条命中（`prod-verify-readonly.txt` §D）：

```
01a19108-ee53-73c6-bdaa-0109614ea963 | DEFECT | INFO | 已核查豁免#2026-11-04：SERIAL_BATCH → biz_material_serial
01a1f140-0000-4000-8000-000000000001 | DEFECT | INFO | ADJUST → (无实体表)      ← doc_type 无实体表，本来就是 INFO
```

---

## 3. 怎么复现

```bash
# 隔离库演练（会自动造脏 → 验三种形态 → 过期回红 → 还原 → 反证 → 清场复绿）
DB_NAME=cloudpivot_p26 bash docs/evidence/20261104-p2-6-collapse-a/drill.sh

# 生产只读复跑（144）
ssh root@172.18.10.144 'cd /opt/cloudpivot && set -a; . /etc/cloudpivot/cloudpivot.env; set +a;
  python3 scripts/verify/snapshot_audit.py --install-views --group 多态引用'
```

只读纪律：巡检全程 `BEGIN READ ONLY` + `PGOPTIONS=-c default_transaction_read_only=on` 双保险；
`--install-views` 只做 `CREATE OR REPLACE VIEW`，全部落在 `di` 巡检 schema（回滚 `DROP SCHEMA di CASCADE`）。

---

## 4. 与卡面的两处偏差（照实记，别当没发生）

1. **隔离库数字是 25/25，不是卡面的 22/22。** 卡面撰写时本组是 22 条；P2-6b（`261e487`）在本卡之前已合入
   DI-23（附件）/ DI-24（提醒）/ DI-25（抄送）三条，所以 `--group 多态引用` 现在是 9 条、全量 25 条。
   本卡的 22 条全部通过 + P2-6b 的 3 条也通过（都是 0 命中）。
2. **生产 DI-18 是 2 条 INFO，不是卡面写的 1 条。** 卡面漏算了那条 `doc_type=ADJUST`（无实体表却填了 doc_id）——
   它在改之前就是 INFO（属「生产 1 ERROR + 1 INFO」里的 INFO）。本次真正被豁免的 ERROR 只有 1 条，
   现在带 `已核查豁免#2026-11-04：` 前缀呈现，`BLOCKER` 归零。

## 5. 红线遵守

- 生产业务数据**零改动**：未 `UPDATE biz_inventory_txn`、未恢复 `biz_material_serial`；
  抽验流水 `01a19108-ee53-…4ea963` 原样（`qty=10 / amount=0 / remark="批量生成 10 件（SN-20261031-000011 起）"`，见 `prod-verify-readonly.txt` §E）
- DI-19 的终态命中**没有从输出里删掉**：降 INFO 但 47 条仍逐条列出（输出里明写「… 另有 42 条未展开」）
- `di.v_poly_ref_map` 既有登记**未改**（`git show HEAD -- 10_checks.sql` 可核对：本次新增只有 `v_poly_ref_waiver` 段与 c_di18/c_di19 两段）
- bare（144）+ github 均已推到 `7f0fe5c`，并在 144 上 `git show HEAD` 回读核对（`prod-verify-readonly.txt` §A，工作区与 HEAD 空 diff）
