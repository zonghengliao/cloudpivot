# P2-6 多态引用巡检 —— 证据包（2026-11-04）

> 卡：`【P2-6】多态引用巡检（单据软删后，引用它的流水/凭证还挂着没人发现）`
> 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §5 **DC-08**（P2）
> 盘点：`docs/architecture/P2-6-多态引用列盘点.md`
> 巡检定义：`docs/architecture/snapshot-drift/10_checks.sql`（DI-17…DI-22）
> 执行器：`scripts/verify/snapshot_audit.py`（B1 框架，未改只读纪律）

---

## 1. 交付了什么

| 文件 | 改动 |
|---|---|
| `docs/architecture/snapshot-drift/10_checks.sql` | 新增 **DI-17…DI-22** 六个视图 + 两个公共层视图（`di.v_poly_ref_map` 登记表、`di.v_poly_ref_dangling` 取数层）；11 列契约与既有 16 条完全一致 |
| `scripts/verify/snapshot_audit.py` | `CHECKS` 新增 6 项（新分组「多态引用」）；**执行层一行没动**（仍是只读事务 + `statement_timeout` + 锁文件） |
| `docs/architecture/P2-6-多态引用列盘点.md` | 全库 **14 处**多态引用盘点（含「为什么不查其余 9 处」） |
| `scripts/verify/p26_poly_ref_drill.sh` | 脏数据演练夹具（只在隔离库跑，`trap` 自动清场） |
| `docs/evidence/20261104-p2-6/` | 本证据包 |

**编号说明**：卡面写「编号接在现有之后（DI-16 起）」，但实测 **DI-16 已被占用**
（`DI-16 已过账收货单的来源采购单已不可收`，ARCH-3 DC-03 补口，已上线）。
为不制造重号（重号会让聚合卡把两条巡检当一条），本卡顺延为 **DI-17…DI-22**。

---

## 2. 6 条巡检（编号 / 名称 / 级别 / 生产命中）

| 编号 | 名称 | 级别 | 生产命中（2026-11-04 16:25） |
|---|---|---|---|
| DI-17 | 凭证来源悬空 / 有类型无 id | BLOCKER（悬空 ERROR、缺 id WARN） | **0** |
| DI-18 | 库存流水单据悬空 | BLOCKER（悬空 ERROR、无实体表的 doc_type INFO） | **1 ERROR + 1 INFO** |
| DI-19 | 流程实例单据悬空 / 缺 biz_id | WARN | **47** |
| DI-20 | 状态日志实体悬空 | INFO（**故意不告警**） | **31** |
| DI-21 | 序列号来源未记录 | INFO/WARN | **0**（31 个序列号全被清场软删，见 §4.5） |
| DI-22 | 多态引用登记缺口（护栏） | INFO | **0** |

完整输出：`prod-snapshot-audit-full.txt`（人看）/ `prod-snapshot-audit-full.json`（机读）。

```
汇总: BLOCKER 1 / WARN 47 / INFO 32   巡检项 22（通过 19、有命中 3、跳过 0、失败 0）
```

> 既有 16 条（DI-01…DI-16）在生产**全绿**（BLOCKER 0 / WARN 0），本卡的 6 条新增
> 就是那 3 个「有命中」项 —— 说明新巡检不是复制既有口径，是真查了新东西。

---

## 3. 只读验证（AP-8：只报不修）

```bash
$ grep -cwE "UPDATE|INSERT|DELETE" scripts/verify/snapshot_audit.py
0
$ sed -n '378,980p' docs/architecture/snapshot-drift/10_checks.sql | grep -cwE "UPDATE|INSERT|DELETE"
0
```

- 执行器里**唯一的 DDL** 仍是 `--install-views` 的 `CREATE OR REPLACE VIEW`（只落在 `di` 巡检 schema）；
- 每条巡检都在 `BEGIN READ ONLY` 事务里跑（`PGOPTIONS=-c default_transaction_read_only=on` 双保险），
  写操作会被 PG 直接拒绝；
- 6 条新视图只有 `SELECT` + `LEFT JOIN`，**发现悬空绝不回写业务数据**；
- 演练脚本 `p26_poly_ref_drill.sh` 里的 DML 是**造脏/清脏的夹具**（只对隔离库），不属于巡检链路。

---

## 4. 生产库真实命中与判断（卡面必答）

### 4.1 DI-18：1 条 ERROR —— **真缺陷（手工修数留下的悬空）**

```
INVENTORY_TXN 01a19108-ee53-73c6-bdaa-0109614ea963 (SN-20261031-000011)
  doc_type=SERIAL_BATCH → biz_material_serial  SOFT_DELETED  [ERROR]
```

- 2026-10-31 16:17 序列号批量生成 10 件 → 写了 `SERIAL_BATCH` 流水（doc_id = 批次首个序列号）；
- 2026-11-01 00:20:44 一次**手工修数**（`DATAFIX-20261101-P14`）把 31 个序列号全部软删，
  流水没有跟着处理 → 这条流水的「来源序列号」再也点不开。
- **判断：真缺陷，但不是代码 bug，是数据修复没闭环。** 按卡面要求**不自动修**
  （流水是审计证据），需要人工决定：给这条流水补一条说明 / 或把该批次标记为「已作废」。

### 4.2 DI-18：1 条 INFO —— 手工修数痕迹（**不是账务悬空**）

```
INVENTORY_TXN 01a1f140-0000-4000-8000-000000000001 (DATAFIX-20261101-P14)
  doc_type=ADJUST → (无实体表)  NO_TARGET_TABLE  [INFO]
```

`doc_type='ADJUST'` 本来就没有对应单据表（库存手工调整），却填了一个**根本不存在的 uuid**（`…-0000`）。
账是完整的（qty/成本/前后快照都在），所以定 INFO：**「来源」栏位是假的，审计要能一眼看到这里有人动过手**。

### 4.3 DI-19：47 条 WARN —— **历史正常（演示/测试对象被清理），但暴露了体验问题**

| 来源 | 条数 | 成因 |
|---|---|---|
| `TASK_ACCEPT → biz_task` 软删 | 38 | 生产 846 个任务里 766 个是软删（大量演示/测试任务被清），它们的验收审批实例还在 |
| `CONTRACT_APPROVE → biz_contract` 软删 | 5 | 11 份合同里 10 份软删（P2-x 演示数据清理） |
| `FLOW_EDGE_CFG → biz_flow_edge` 行不存在 | 4 | 状态机边配置「被审批驳回」的记录：`entity_id` 是**申请时生成的草稿 id**，从没落成配置行（`state_machine.rs::note_edge_config` 注释里写明 REJECTED 场景） |

**判断：账不受影响，属历史正常**；但对用户是「审批中心点进去 404」的真实体验问题。
按卡面口径保持 WARN（不升 BLOCKER），并把「清场要连带处理审批实例」写进跟进卡。

### 4.4 DI-20：31 条 INFO —— **正常（日志就该保留已删实体）**

`VOUCHER` 13（凭证归档/清场后日志仍在）、`CONTRACT` 9、`FLOW_EDGE_CFG` 4、`PURCHASE_ORDER` 4、`STOCK_IN` 1。
> 这正是「账务悬空 = BLOCKER、日志悬空 = INFO」这条分级线的实证：
> 同样一批已删对象，**凭证/流水的悬空要报 P0，日志的悬空一条都不该告警**。

### 4.5 DI-21：0 命中，但**不能当绿灯**

生产 31 个序列号 `source_type` 有值（PURCHASE 21 / PRODUCE 10），`source_id` **0 行有值**；
且 31 个序列号在 2026-11-01 全部被软删 → 巡检按「不查已软删的源行」跳过，故 0 命中。
**问题本身是设计缺口**（代码 `serial_handler.rs` 4 处写入点全是 `source_id: None`，
4 种来源类型也没有目标表映射），已建跟进卡，不在本卡猜映射。

### 4.6 DI-17：0 命中 —— 真的干净

生产 21 张业务凭证（`STOCK_IN` 15 / `STOCK_OUT` 6）**全部**能回跳到来源单，
逐条 `LEFT JOIN` 验过（不是「没数据所以 0」）。

---

## 5. 脏数据演练（红 → 绿，两次输出都在）

夹具：`scripts/verify/p26_poly_ref_drill.sh`（隔离库 `cloudpivot_p26`，真机造出的收货/凭证/流水）。
输出：`drill/*.txt`（7 次运行）。

| 步骤 | 造了什么脏数据 | 结果 |
|---|---|---|
| `00-baseline-green` | 无（真机数据：4 张过账收货单 → 4 条流水 + 4 张凭证 + 12 条日志 + 4 个审批实例） | ✅ BLOCKER 0 / WARN 0 / INFO 0，exit 0 |
| `01-dirty-softdeleted-doc` | 软删一张**已过账收货单**（`IN-20261104-000001`） | ✗ **DI-17 1 ERROR、DI-18 1 ERROR、DI-19 1 WARN、DI-20 3 INFO**，exit 1 |
| `02-restored-green` | 撤销软删 | ✅ 全绿，exit 0 |
| `03-dirty-missing-id-and-row` | 凭证「有 `STOCK_IN` 无 `source_id`」+ 流水 `doc_id` 指向不存在的收货单 | ⚠ DI-17 1 **WARN**（缺 id）+ ✗ DI-18 1 **ERROR**（ROW_MISSING），exit 1 |
| `04-dirty-serial-source` | 序列号 `source_type=PURCHASE`、`source_id=NULL` | ℹ DI-21 1 INFO，exit 0 |
| `05-dirty-unregistered-type` | 状态日志 `entity_type='P26_UNKNOWN_DOC'`（登记表没有的类型） | ℹ DI-22 1 INFO（护栏自检生效），exit 0 |
| `06-clean-green` | 全部清场 | ✅ 全绿，exit 0 |

一次软删收货单同时打红 4 条巡检（凭证/流水/审批/日志），正是本卡要证的事：
**单据删了，引用它的地方还挂着，以前没人发现 —— 现在一条命令就能看见。**

---

## 6. 不变量与全量巡检（隔离库 `cloudpivot_p26`，真机数据）

| 套件 | 结果 | 证据文件 |
|---|---|---|
| `snapshot_audit.py`（全 22 条） | **22/22 通过**，BLOCKER 0 / WARN 0 / INFO 0，exit 0 | `isolated-p26-snapshot-audit-full.txt` |
| `erp_invariants.py` | **40/40 通过**，BLOCKER 0 / WARN 0 | `isolated-p26-erp-invariants.txt` |

> 卡面写「原有 32 条」，实测已是 40 条（P2-x 各卡陆续加的 I 组盘点单 3 条等），
> 这 40 条**一条都没改**，全绿。

---

## 7. 复跑命令

```bash
# 隔离库（干净重建 + 迁移）
RECREATE=1 D1_DB=cloudpivot_p26 bash scripts/verify/d1_dbsetup.sh
D1_DB=cloudpivot_p26 bash scripts/verify/d1_migrate.sh 18077

# 只跑多态引用
NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME=cloudpivot_p26 \
  python3 scripts/verify/snapshot_audit.py --install-views --group 多态引用

# 脏数据演练（造脏 → 变红 → 清场 → 变绿）
D1_DB=cloudpivot_p26 bash scripts/verify/p26_poly_ref_drill.sh

# 生产（只读，装 di 巡检视图）
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
NIK_DATABASE_* 已由 env 注入 → python3 scripts/verify/snapshot_audit.py --install-views --group 多态引用
```
