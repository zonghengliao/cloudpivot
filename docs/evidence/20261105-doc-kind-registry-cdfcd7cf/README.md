# 巡检登记表 FIN 域补齐 + 悬空引用分支补口（卡 t_cdfcd7cf）

修复 commit：`67e3ae7`（只改 `docs/architecture/snapshot-drift/10_checks.sql`，+79 行 / −0 行）
基线：`9867fbe`（合并后 main，卡面报告的红点）；接手时 HEAD：`e8a4901`

## 1. 卡面红点：已在 9f0bc75 修好，本卡复核确认

`business::shared::doc_kind::doc_kind_tests::doc_kind_registries_match_doc_kind_table`
在 `9867fbe` 上因为 `10_checks.sql` 登记表缺 4 行而红：

| 域 | 缺的行 | 现状 |
|---|---|---|
| `ATTACHMENT_DOC` | `FIXED_ASSET` | 已在 `9f0bc75` 补齐 |
| `STATUS_ENTITY` | `BUDGET` / `FIXED_ASSET` / `DEPRECIATION` | 同上 |

复核方式不是集合比对，而是**在 `9867fbe` 与 `67e3ae7` 上实跑单测**（见 `acceptance-67e3ae7.txt`）：
`business::shared::doc_kind` **19 passed / 0 failed**。

## 2. 顺带核对（卡面第 2 条）发现的同类真问题

「登记行补齐」只解决了一半。`di.v_poly_ref_dangling`（DI-17/19/20 的公共取数层）
是**逐（域, 类型）显式 UNION 分支**，登记表只提供 `target_table` / `soft_delete_col`。
所以「登记了但没有分支」= 登记了但不查 —— 正是 `10_checks.sql` 自己在 FIN-2 附件分支处
写明的反模式（「比不登记更糟」：DI-22 变绿，人以为查过了）。

`9f0bc75` 补的 4 行里，**只有 `ATTACHMENT_DOC.FIXED_ASSET` 有分支**；
`STATUS_ENTITY` 的 `BUDGET` / `FIXED_ASSET` / `DEPRECIATION` 三行**一个分支都没有**，
即 DI-20 仍然看不到预算 / 固资 / 折旧三类对象。

同时 FIN-1 / FIN-2 给 Rust 词表加值、登记表整域没跟上的还有两个：

| 域 | Rust 词表 | 登记表（接手时 `e8a4901`） | 影响 |
|---|---|---|---|
| `FLOW_BIZ` | `FlowBizType::ALL` = 15 | 13（缺 `BUDGET_APPROVE`、`ASSET_DISPOSAL`） | DI-19 看不到预算 / 资产处置审批实例；DI-22 报「未登记」 |
| `VOUCHER_SOURCE` | `VoucherSourceType::ALL` = 13 | 10（缺 `FIXED_ASSET_ACQ`、`FIXED_ASSET_DISPOSAL`、`DEPRECIATION`） | DI-17 看不到固资购入 / 处置 / 折旧凭证 |

逐 rev 比对输出见 `registry-audit-by-rev.txt`（工具 `registry_audit.py` 随证据提交）：
`9867fbe` 3 域红 → `e8a4901` 2 域红 → `67e3ae7` **exit=0 全绿**。

## 3. 本次改动（全部在 10_checks.sql）

登记表 `di.v_poly_ref_map`（值序与 Rust `ALL` 一致，FIN 值都在块尾）：

```
FLOW_BIZ       + BUDGET_APPROVE   → biz_budget          / deleted_at
               + ASSET_DISPOSAL   → biz_fixed_asset     / deleted_at
VOUCHER_SOURCE + FIXED_ASSET_ACQ      → biz_fixed_asset        / deleted_at
               + FIXED_ASSET_DISPOSAL → biz_fixed_asset        / deleted_at
               + DEPRECIATION         → biz_depreciation_batch / NULL（批次行无 deleted_at）
```

悬空取数层 `di.v_poly_ref_dangling`（7 个新分支）：

* `STATUS_ENTITY`：`BUDGET` / `FIXED_ASSET` / `DEPRECIATION` → DI-20
* `FLOW_BIZ`：`BUDGET_APPROVE` / `ASSET_DISPOSAL` → DI-19
* `VOUCHER_SOURCE`：`FIXED_ASSET_ACQ`+`FIXED_ASSET_DISPOSAL`（合一个分支）/ `DEPRECIATION` → DI-17

目标表与软删列的语义都在代码里核过，不是猜的：

* `ASSET_DISPOSAL` → `asset_handler.rs` 的 `biz_instance_running(asset_bridge::BIZ_TYPE, id)`，`id` 是资产卡片 id
* `BUDGET_APPROVE` → `budget_bridge::internal_control_creator` 按 `biz_budget.id` 取制单人
* `FIXED_ASSET_ACQ` → `asset_handler` 购入凭证 `source_id: Some(*asset_id)`
* `FIXED_ASSET_DISPOSAL` → `asset_bridge::post_disposal_vouchers` 传 `Some(asset.id)`
* `DEPRECIATION` → `depreciation_model` 传 `source_id: Some(batch_id)`
* `biz_depreciation_batch` **没有** `deleted_at` 列（V0230 DDL），所以软删列写 `NULL`、分支只判存在

**没有**改 Rust 词表，**没有**放宽 `doc_kind_registries_match_doc_kind_table` 断言。

## 4. 功能验证：同一批脏数据 A/B（`fixture-ab-di17-19-20.txt`）

两个同源库（`TEMPLATE cloudpivot_regress_20261105`，112 表 / 182 迁移）：

* A = `cloudpivot_dicheck_base_cdfcd7cf`（`e8a4901` 原 SQL）
* B = `cloudpivot_dicheck_cdfcd7cf`（`67e3ae7`，与提交文件 sha256 一致：`52cf35f…`）

夹具（事务内插入、结束 `ROLLBACK`）：预算 / 资产处置审批实例各 1（biz_id 悬空）、
状态日志 3 条（BUDGET / FIXED_ASSET / DEPRECIATION，entity_id 悬空）、
凭证 3 张（购入 / 处置 / 折旧，source_id 悬空）。

| | A 改前 | B 改后 |
|---|---|---|
| DI-17 凭证来源 | **0 行** | 3 行 ERROR（`FIXED_ASSET_ACQ` / `FIXED_ASSET_DISPOSAL` / `DEPRECIATION`） |
| DI-19 流程实例 | **0 行** | 2 行 WARN（`BUDGET_APPROVE` / `ASSET_DISPOSAL`，PENDING） |
| DI-20 状态日志实体 | **0 行** | 3 行 INFO（`BUDGET` / `FIXED_ASSET` / `DEPRECIATION`） |
| DI-22 登记缺口 | 2 行 INFO（`FLOW_BIZ:BUDGET_APPROVE`、`VOUCHER_SOURCE:DEPRECIATION` 未登记） | **0 行** |

改前 0 行 = 这批悬空引用**完全静默漏报**，与卡面担心的是同一类问题。

26 个巡检视图在改后 schema 上逐条执行无报错：`allviews-26-exec.txt`。

## 5. 未覆盖 / 建议

1. **「登记了但不查」没有机械护栏**。现有单测只钉住「词表 ↔ 登记表」两端，
   钉不住「登记表 ↔ 分支」。建议后续加一条结构自检（新 DI 编号）：
   登记表里 `target_table IS NOT NULL` 的行，必须在 `v_poly_ref_dangling` 里有对应分支。
   本卡没加：新建 DI 会改变「合计 26 项」口径，与卡面验收冲突。
2. **「复式记账一致性」类巡检目前对 FIN 三张表是空白**。26 项里没有任何借贷平衡 /
   折旧批次合计 = 明细合计 / 资产净值恒等等条目；这些现在只靠库上的 CHECK
   （`ck_biz_fa_net`、`ck_biz_dep_batch_*`）与单测兜着。要补属于新巡检项，建议另开卡。
3. 本次在回归库上跑，库里**没有** FIN 三类对象的存量数据，所以真实数据行数改前改后一致；
   功能有效性证据是上面的夹具 A/B，不是存量数据。
4. 与 FIN-3（`EXPENSE_REPORT`）在途改动的关系：该 worker 在本卡期间正改同一文件
   （`FLOW_BIZ.EXPENSE` → `biz_expense_report` + 4 行 `EXPENSE_REPORT`）。本卡提交时
   **只暂存自己的 hunk**，其未提交改动原样留在工作区（见 commit 的 `git show --stat`：
   1 file changed, 79 insertions(+)）。
