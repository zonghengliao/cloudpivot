# P2-6b 序列号来源映射 + 三类多态引用补口 —— 证据包（2026-11-04）

> 卡：`【P2-6b】序列号来源 source_id 写入点恒空（4 类来源无目标表）+ 3 类多态引用待补巡检`
> 父卡：`【P2-6】多态引用巡检`（commit `d930ced`）
> 依据：`docs/architecture/P2-6-多态引用列盘点.md` §2.3 / §5.3
> 巡检定义：`docs/architecture/snapshot-drift/10_checks.sql`（DI-21 升级 + 新增 DI-23/24/25）
> 执行器：`scripts/verify/snapshot_audit.py`（B1 框架，执行层一行未改）

---

## 1. 交付了什么

| 文件 | 改动 |
|---|---|
| `backend/src/business/shared/doc_kind.rs` | `SerialSourceType::target_table()`：四类来源 → 目标表的**唯一映射**（PURCHASE/RETURN→`biz_stock_in`、TRANSFER→`biz_stock_transfer`、PRODUCE→无表） |
| `backend/src/business/erp/serial_model.rs` | `ensure_source_pair()`：批量生成的写入闸门（类型/ id 必须成对 + 目标行必须真实存在）；`source_target_table()` |
| `backend/src/business/erp/serial_handler.rs` | `DbError::InvalidValue` → **400 + 10001**（不再 500）；夹具改成「无来源入库」；新增 4 类坏组合单测 |
| `backend/src/business/erp/stockin_base.rs` | `serial_source_of_in_type()`：收货单 `in_type` → 序列号来源（PURCHASE/RETURN；OTHER 不记来源） |
| `backend/src/business/erp/stockin_cancel.rs` | 写序列号时按本单 `in_type` 写 `source_type` + `source_id`（**以前退货入库也写 PURCHASE，是错的**）；作废退号判据从「类型+id」改为「id」，否则退货入库退号会静默漏掉 |
| `backend/src/business/erp/stockin_handler_post.rs` | 过账时把 `header.in_type` 传进 `SerialBatch` |
| `backend/src/business/shared/doc_kind_tests.rs` | 3 个**机械核对**测试：Rust 映射 ↔ `10_checks.sql` 登记表逐行比对（序列号 / 附件+提醒+状态日志 / 抄送） |
| `docs/architecture/snapshot-drift/10_checks.sql` | `SERIAL_SOURCE` 四行补实表；新增 `ATTACHMENT_DOC`(5) / `REMINDER_DOC`(4) / `FLOW_CC_SOURCE`(11) 登记行；取数层加 9 个分支；DI-21 升级（悬空可查）；新增 **DI-23/24/25**；DI-22 覆盖域 5 → 8 |
| `scripts/verify/snapshot_audit.py` | `CHECKS` 新增 DI-23/24/25（新分组仍是「多态引用」）；DI-21 文案/口径更新 |
| `docs/architecture/P2-6-多态引用列盘点.md` | §2.1 / §2.3 / §3 / §5.3 按实测更新（含「原估抄送来源域与代码不符」的更正） |
| `docs/architecture/快照一致性巡检与告警设计.md` | 清单 22 → 25 条，补分级论证与总表 |
| `docs/evidence/20261104-p2-6b/drill.sh` | 脏数据演练夹具（只在隔离库跑，`trap` 自动清场） |

---

## 2. 序列号来源：四类来源的映射（卡面必答）

**先纠正父卡的一个事实**：`serial_handler.rs` 里那 4 处 `source_id: None`（616/639/933/1806 行附近）
**全部是单测夹具**，不是生产写入点。真正的写入点有两处：

| 写入点 | 谁调 | 写的值 |
|---|---|---|
| `stockin_cancel::insert_serials` | 收货过账 `stockin_handler_post` | `source_type` = 本单 `in_type` 映射（PURCHASE/RETURN），`source_id` = 收货单 id |
| `serial_model::batch_generate` | 手工批量生成 API（「无来源入库」） | 由请求带入，**必须过 `ensure_source_pair` 闸门** |

| 来源 | 目标表 | 现状 |
|---|---|---|
| `PURCHASE` | `biz_stock_in` | 已写（P2-6b 起按 `in_type` 决定，不再硬编码） |
| `RETURN` | `biz_stock_in`（同表，`in_type=RETURN`） | **P2-6b 新增**（以前一律写 PURCHASE ⇒ 退货入库来源是错的） |
| `TRANSFER` | `biz_stock_transfer` | 无写入点（调拨只搬已有号，不新建）；登记备查 |
| `PRODUCE` | 无（生产模块未落地） | 只允许「有类型、无 id」；带 id 直接 400 |

生产现状（只读实测）：31 行全部 `deleted_at` 非空（2026-11-01 清场），`source_id` 0 行有值 ——
不会再被 DI-21 报（已软删的源行按口径跳过），但**新写入的点从今天起都过闸门**。

---

## 3. 只读纪律（沿用 B1 口径，未破坏）

```bash
$ grep -cwE "UPDATE|INSERT|DELETE" scripts/verify/snapshot_audit.py
0
$ grep -cwE "UPDATE|INSERT|DELETE" docs/architecture/snapshot-drift/10_checks.sql
0
```

- 演练夹具（DML）刻意放在 `docs/evidence/`，不在 `scripts/verify/` —— 目录边界一眼可验；
- 每条巡检仍是 `BEGIN READ ONLY` + `statement_timeout`；`--install-views` 仍是唯一的 DDL 入口。

---

## 4. 隔离库演练（cloudpivot_p26b）

前置：`createdb`（superuser）→ release 二进制启动迁移（163 条 system + business 迁移，102 张表）→
`snapshot_audit.py --install-views`（0 → 25 个 `c_di*` 视图，实测 `information_schema.views` 共 31 行含公共层）。

```
00-baseline-green                BLOCKER 0 / WARN 0 / INFO 0   巡检项 9（通过 9、有命中 0）
01-dirty-serial-and-refs         BLOCKER 0 / WARN 1 / INFO 4   巡检项 9（通过 6、有命中 3）
02-dirty-stock-in-soft-deleted   BLOCKER 0 / WARN 3 / INFO 4   巡检项 9（通过 5、有命中 4）
03-cleanup-green                 BLOCKER 0 / WARN 0 / INFO 0   巡检项 9（通过 9、有命中 0）
```

三种故意造出的形态（`drill/01-*.txt`、`drill/02-*.txt` 有逐行明细）：

| 形态 | 夹具有什么 | 谁报 | 级别 |
|---|---|---|---|
| 序列号指向不存在的调拨单 | `source_type=TRANSFER, source_id=<假 uuid>` | DI-21 `ROW_MISSING` | WARN |
| 序列号有类型无 id | `source_type=PURCHASE, source_id=NULL` | DI-21 `MISSING_ID` | INFO |
| 收货单被软删后序列号还挂着 | 先建 PURCHASE→收货单的号，再软删该单 | DI-21 `SOFT_DELETED` | WARN |
| 附件挂在被软删的收货单上 | `biz_doc_attachment(doc_type=STOCK_IN)` | DI-23 `SOFT_DELETED` | WARN |
| 提醒台账指向不存在的采购单 | `biz_reminder_log(doc_type=PURCHASE_ORDER)` | DI-24 `ROW_MISSING` | INFO |
| 抄送来源角色不存在 / 角色来源没记 ref | `biz_flow_cc(source_type=ROLE)` 两条 | DI-25 `ROW_MISSING` / `MISSING_ID` | INFO |

清场后再跑 = 全绿（`drill/03-cleanup-green.txt`），说明不是「按行数恒红」的假巡检。

---

## 5. 单测（真库指向隔离库 cloudpivot_p26b）

| 组 | 命令 | 结果 |
|---|---|---|
| doc_kind（含 3 个巡检登记表机械核对） | `cargo test --lib doc_kind` | **20 passed / 0 failed** |
| 序列号模块全量（含 P2-6b 新增的来源闸门 4 形态） | `cargo test --lib business::erp::serial_handler` | **49 passed / 0 failed** |
| 收货过账写序列号（断言 `source_type`+`source_id`） | `cargo test --lib serial_material_generates_serials_in_receipt_warehouse` | **1 passed** |
| fmt / clippy | `rustfmt +1.94.1 --check`；`cargo clippy --all-targets` | 本卡改动文件 **0 告警** |

明细：`tests/doc-kind.txt`、`tests/serial-handler.txt`。

> 隔离库准备：`CREATE DATABASE cloudpivot_p26b`（superuser 通道，`cloudpivot` 账号无 CREATEDB）
> → release 二进制启动迁移（163 条、102 张表）→ `--install-views`。
> 另外把科目 `1901` 置为启用（迁移里是停用态，生产是现场启用的）—— 只改隔离库。

---

## 6. 生产只读首跑（2026-11-04 17:09，部署后）

```
汇总: BLOCKER 1 / WARN 47 / INFO 32   巡检项 25（通过 22、有命中 3、跳过 0、失败 0）
多态引用组 9 条：DI-17 ✓ · DI-19 47 WARN · DI-20 31 INFO ·
                DI-21 ✓ · DI-22 ✓ · DI-23 ✓ · DI-24 ✓ · DI-25 ✓
ERP 不变量：40/40 通过（BLOCKER 0 / WARN 0）
```

- **本卡 4 条新/改巡检全部 0 命中**：DI-21（31 个序列号全被 2026-11-01 清场软删，按口径跳过）、
  DI-23/24/25（三张表生产 0 行）；
- BLOCKER 1 / WARN 47 / INFO 32 与父卡 P2-6 完全一致 ⇒ **没有新增命中，也没有把旧命中「修没」**
  （那 1 条 ERROR 是 `t_3de61e4b` 跟进的「手工修数未闭环」，不是本卡引入）；
- 完整输出：`prod-snapshot-audit-full.txt` / `prod-snapshot-audit-polyref.txt` / `prod-erp-invariants.txt`。

**部署核对**（本卡改了后端代码，必须换二进制才算上线）：

```
发布提交 261e487；systemd active；/health = 200
md5(磁盘二进制) == md5(/proc/<MainPID>/exe) = 1757ad713b5b9ff54c1eb008e4698492
grep -ac "必须同时给 source_id" 二进制 = 1（新闸门文案真的进了产物）
di 巡检视图 22 → 25 个 c_di*
```

