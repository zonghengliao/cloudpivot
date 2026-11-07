# 全量 lib 套件的「种子完备测试库」取证（卡 `t_13ddcb84`）

> 目的：回答「后端全量套件到底跑在哪种库上」，给出**可复现、可重跑**的库 + 全量 `0 failed` 的原始日志。
> 口径文档：`docs/测试库口径-全量套件.md`　造库脚本：`scripts/verify/make_full_suite_db.sh`

## 0. 结论

| 问题 | 结论 |
|---|---|
| 全量套件跑在哪种库上 | **全量迁移 + 产品自带演示组织种子（`NIK_SEED_DEMO_USERS=true`）+ 零业务单据**；四条 URL 同库；库名以 `cloudpivot_test` 开头 |
| 怎么造 | `bash scripts/verify/make_full_suite_db.sh`（先删后建，约 20 秒，末尾逐项自检，见 `raw/make-db-selfcheck.txt`） |
| 结果 | `cargo test --lib -- --test-threads=1` → **3329 passed / 0 failed / 0 ignored**（连跑两轮都绿：`raw/run1-full.log.txt`、`raw/run2-full.log.txt`） |
| 之前为什么跑不绿 | 两类库各缺一半条件：只跑迁移的库**没有演示组织**（审批链 65018 / 夹具 panic），历史快照库**有别人的单据**（整期/整表口径断言被打红）；种子补上后又暴露 **4 处夹具/接线缺陷**（§4） |

## 1. 环境与基线

| 项 | 值 |
|---|---|
| 机器 | 99 开发机（`ubuntu-svr`，4C / 15G） |
| 基线提交 | `50a9063`（干净树：`git archive HEAD` ⇒ 避开共享主仓里别人的未提交文件） |
| 迁移 | `schema_migrations` **261** 条，最新版本 **330**，`public` 表 **142** 张 |
| 二进制 | 隔离树自编（`CARGO_TARGET_DIR=/root/t13db/target`），`migrations/` 编译期内嵌 |
| 库 | `cloudpivot_test_fs1`（本卡脚本造，owner=`alun`）；`cloudpivot_test_fs2` 用于「造库自检 + 幂等复跑」取证 |
| 跑法 | `ERP_DB_URL=SYS_DB_URL=TASK_DB_URL=P0FIX_DB_URL=postgres://…/<库>` + `cargo test --lib -- --test-threads=1`，**无 `--skip` / `--ignored`** |

> 用例数说明：干净树 `50a9063` 是 **3329** 条；`raw/before-empty-db-full.log.txt` 那轮用的是共享主仓
> （含别人未提交的 `price_inquiry` 模块）的二进制，多 14 条 ⇒ 3343。数字差异来自树，不是用例被跳。

## 2. 修前：两类库各红一批（对照）

| 库 | `test result` | 红的是什么 | 根因 |
|---|---|---|---|
| 只跑 migrations 的空库（`cloudpivot_test_full`，无演示种子） | 3315 passed / **28 failed**（`raw/before-empty-db-full.log.txt`） | `asset_linkage_tests`(11)、`expense_linkage_tests`(6)、`sales_approval_bridge`(1)、`receivable_aging`(3)、`year_end_linkage_tests`(5)、`doc_kind`/`menu`(2) | 前 4 类 = **没有演示组织/部门/角色成员**；`year_end_linkage` = 别人造的脏凭证没收尾；最后 2 条是**共享主仓里别人未提交的改动**（`V0321__price_inquiry` 菜单 + 旧二进制），干净树上不复现 |
| 145 的 `cloudpivot_dev` 拷贝库 | 跑到 930 条时 925 passed / **5 failed**（未跑完） | `cash_flow::r3_real_*`(2)、`income_statement::r2_real_*`(3) | **库里有历史卡 e2e 留下的单据/不平数据**：断言是「本期间恰好 1 条未分类」「基线必须平」 |
| **种子完备库**（加演示组织后） | 3282 passed / **47 failed**（`raw/before-seeded-full.log.txt`） | 一物一码/收货单一组（`stockin_handler_tests_*` 43 条 + cancel/p16 4 条） | **FIX-7 把无源入库记成 `SOURCELESS` 之后**：序列号不再写 `source_id` ⇒ 挂不回收货单（详情 `serial_count` 恒 0、作废退号查不到），夹具按 `source_type='PURCHASE'` 也清不掉 ⇒ 撞 `biz_material_serial_location_id_fkey`，从第二次 `setup()` 起集体红 |

## 3. 修后：种子完备库上全量

```
# 第一轮（新建库 cloudpivot_test_fs1）
test result: ok. 3329 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1694.72s
```

原始日志：`raw/run1-full.log.txt`（03:16:16 → 03:44:31，`exit=0`）。

### 3.1 同一个库连跑第二轮（「重跑不变红」）

第一版的第二轮是 **3324 passed / 5 failed**（`raw/run2-full-before-module-teardown.log.txt`），
5 条红**同一个根因**：`flow::internal_control::db_tests` 的 `p0fix_*` 夹具跑完不收尾 ——
7 条 `biz_type=PO_APPROVE` + `status=PUBLISHED` 的流程图（`graph_hash` 还是全 0）、3 条 `PENDING` 实例
留在库里。第二轮里 erp 段（排在 flow 段之前）先跑，于是：

| 红 | 判据被什么打穿 |
|---|---|
| `purchase_handler::dc01_submit_without_published_flow_is_409` | 「没配流程必须 409」——库里还有 7 条 PUBLISHED 的 `PO_APPROVE` 流程 |
| `purchase_handler::p2_2_credit_over_limit_cannot_skip_approval` / `p2_2_credit_within_limit_can_skip_...` | 同上：`po_approve` 之外还有别的已发布流程 ⇒ 不允许跳过审批 |
| `purchase_handler::zz_teardown_purges_po_approval_residue` | 「不许剩 PENDING 的采购审批实例」——上一轮 `P0FIX-todo/bypass/role_fix` 三条 PENDING |
| `stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean` | 全局「审批图自检」把这 7 个零 `graph_hash` 的 p0fix 版本报成 7 条 `RowsEdited` 漂移 |

**修法**：给该模块加一条 `zz_purge_module_fixtures`（用例名 `zz_` ⇒ 串行跑时排在本模块最后），
按外键顺序把 `p0fix_*` / `P0FIX-*` 的实例、任务、采购单、流程图、定义、夹具用户/角色全删，并**库侧复查**残留为 0。
修后在同一个库上复跑：

```text
business::flow::internal_control            → 15 passed / 0 failed（raw/after-teardown-internal-control-rerun.log.txt）
business::erp::purchase_handler + p16       → 82 passed / 0 failed（raw/after-teardown-purchase-and-p16-rerun.log.txt）
库侧复查：p0fix 定义 0 / P0FIX 实例 0 / p0fix 用户 0
```

全量两轮（同一份库、同一二进制）的最终结果见 §3.2。

### 3.2 最终两轮全量

```text
# 第一轮（新建库）
test result: ok. … passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
# 第二轮（同一个库，带第一轮残留）
test result: ok. … passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

原始日志：`raw/final-run1-full.log.txt`、`raw/final-run2-full.log.txt`（提交后的树，见 §1 基线）。

造库自检（`raw/make-db-selfcheck.txt`）：迁移 261/330、表 142、`sys_user` 10（admin + 9 个 demo）、
`sys_dept` 1（`DEPT_DEMO`）、`biz_flow_def(PUBLISHED)` 16、`biz_account` 66、`biz_report_item` 53、
6 个关键角色在职成员数 1/2/2/1/1/1、`biz_voucher`/`biz_sales_order`/`biz_purchase_order`/`biz_flow_instance` 全 0。

种子幂等（`raw/make-db-keeepdb-reseed.txt` + `raw/idem-counts-{before,after}.txt`）：
`KEEP_DB=1` 再跑一次造库脚本，`sys_user|sys_user_role|sys_dept|sys_role` = `10|10|1|10` 前后**逐字一致**。

## 4. 改了什么（4 处夹具/接线 + 1 个库脚本 + 1 篇口径）

| 文件 | 改动 | 为什么不是「放宽判据」 |
|---|---|---|
| `backend/src/business/erp/year_end.rs` | `r4_17_close_year_refuses_non_leaf_balance` 末尾删掉自己造的脏凭证（借 5602 100） | 该用例本来就是「造脏数据 → 断言 fail-closed」。断言一字未改；只是**造完收干净**（年结的非末级余额判据是累计口径 `voucher_date <= end`，不按年度切分，漏在库里会拦住后面所有年结） |
| `backend/src/business/erp/receivable_aging.rs` | 清整表前先解绑 `biz_invoice` / `biz_receipt_allocation` / `biz_receipt_refund_allocation` 的 `receivable_id` | 账龄报表仍是**全库口径**、夹具仍清整表；只是不再被别人的 RESTRICT 外键残留炸成 `23503` |
| `backend/src/business/erp/sales_approval_bridge.rs` | 待办查询带 `assignee_id`；用每条待办**自己的受派人**去点；同节点待办点完再判状态 | 判据（节点顺序、终态、自审 403）一字未减；只是不再假设「审批人 = admin」「每跳恰好 1 条待办」——`join_mode=AND` 本来按角色成员数展开 |
| `backend/src/business/erp/stockin_cancel.rs`（接线）+ `stockin_query.rs` + `stockin_handler_tests_*` | `insert_serials` 改为 **`source_id` 恒写本单 id**（`source_type` 仍可空）；`serial_count_of` 与夹具序列号查询不再按 `source_type='PURCHASE'` 过滤；夹具清序列号改「按夹具物料 × 夹具仓库」；断言 `source_type=="PURCHASE"` 的用例改为「无源入库 = NULL + `source_id` 回指本单」 | 修的是**产品接线缺口**：`source_id` 是「号挂在哪张单」的唯一线索，留空会让详情计数恒 0、作废退号查不到（库存与序列号台账漂移）。来源**类型**仍按 FIX-7 口径（无源入库不记类型），断言没有放松，只是跟着口径更新 |
| `backend/src/business/flow/internal_control.rs` | 新增 `zz_purge_module_fixtures`：模块跑完把 `p0fix_*` 流程图/实例/采购单/夹具用户角色删净并库侧复查 | 它原来只在「每条用例开头」按前缀自清，模块结束就留在库里（7 条 PUBLISHED 的 `PO_APPROVE` 流程 + 3 条 PENDING 实例 + 零 `graph_hash`）⇒ 同一个库跑第二轮时 erp 段的 4 条 + stockin 图自检 1 条假红。断言强度不变（删的就是本模块自己的行） |
| `scripts/verify/make_full_suite_db.sh` | 新增：建库 → 起实例跑迁移 + 演示种子 → 自检 → 打印四条同库 URL（`RUN=1` 可直接接着跑全量） | — |
| `docs/测试库口径-全量套件.md` | 新增：口径 / 为什么四条 URL 同库 / 判据与反模式 / 不属于这条口径的东西 | — |

## 5. 复现

```bash
bash scripts/verify/make_full_suite_db.sh                       # 造库（默认 cloudpivot_test_full）
export ERP_DB_URL='postgres://alun:<口令>@127.0.0.1:5432/cloudpivot_test_full'
export SYS_DB_URL="$ERP_DB_URL" TASK_DB_URL="$ERP_DB_URL" P0FIX_DB_URL="$ERP_DB_URL"
cd backend && cargo test --lib -- --test-threads=1              # 期望 0 failed
```

重跑（同一库、带残留）：再执行最后一行即可。

## 6. 边界 / 遗留

* 集成 target（`cargo test --test testd` 等）不走这条口径，各自有 `*_DB_URL`，不设就 SKIP（**仍计 passed**）。
* 共享主仓里别人的**未提交**迁移/模块会让「同一棵工作树」跑不绿（本卡基线用 `git archive HEAD` 的干净树取证）。
* 遗留（产品口径待定，不影响本卡）：FIX-7 起「无源入库」的序列号 `source_type` 为空 —— 详情页「来源类型」列
  对这类号没有值可显示（`SerialSourceType` 域只有 PURCHASE/RETURN/SALES/PRODUCE）。功能链路已通（`source_id` 恒写），
  是否需要给该域加一个 `SOURCELESS` 值属于产品决策，建议单独立卡。
