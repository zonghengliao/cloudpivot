# FIX-8 取证：单号取号器并发 + 盘点过账口径 + 预算槽级策略记录（t_6ddb0036）

环境（与 FIX-5/6/7 同一套隔离口径）：

| 项 | 值 |
|----|-----|
| 隔离实例 | 145 `127.0.0.1:8048`（二进制 `backend/target/debug/cloudpivot-fix8`） |
| 数据库 | 145 `cloudpivot_fix8_fresh_20261108022816`（`cloudpivot_dev` 的**拷贝库**，schema 330） |
| 服役实例 | 145 `127.0.0.1:8033`（cloudpivot-test）**全程未动**，收尾 health=200 |
| 生产 | 144 未触碰、未登录 |

## 1. 问题 1：单号取号器——结论 + 修复

### 1.1 不是「全局单行」

`biz_seq` 是**一个 `seq_type` 一行**（`uk_biz_seq_type` 部分唯一索引，V0137 §1.1）。
145 拷库实测 **25 行**（`PZR` 是惰性行：红冲第一张凭证才补），
`PO` / `IN` / `TK` / `STOCK_OUT` / `STOCK_TAKE` … 各占一行：

```
$ cat raw/pre_seq_state.txt      # 10_prepare_145.sh 落盘（E2E 之前）
seq_rows|25
seq_AP|4        seq_IN|4         seq_PO|3      seq_STOCK_OUT|4
seq_AR|2        seq_IV|2         seq_PZ|37     seq_STOCK_TAKE|2
seq_BDG|5       seq_PM|1         seq_RC|1      seq_TK|98
...
```

（段 B 在盘点 E2E 之后跑，那时已多出 `PZR` 行 → 快照 26 行，断言「其余 25 个号段不动」。）

### 1.2 跳号的成因就是并行测试（不是重号，也不是号段混用）

```
$ cat raw/tk_jump_evidence.txt
TK_docs_in_db|2|TK-20261107-000001|TK-20261107-000093
TK_watermark|98|20261107|
```

库内只有 **2 张** TK 单（`…000001` 与 `…000093`），而 `biz_seq(TK).current_no = 98`
—— 中间 96 个号被并行的单测 / 演练取走后回滚或删单。**跳号允许、重号禁止**，
所以本卡不改数据、不调 `current_no`（往下调必撞已存在单号）。口径见
`docs/ERP-单号取号口径.md`。

### 1.3 真机 10 并发（`raw/20_seq_concurrency.txt`，23 ✅ / 0 ❌）

```
· 拿到的单号：['ST-20261108-000002' … 'ST-20261108-000011']   ← 10 个并发 HTTP 建单
✅ 单号两两不同（无重号）
✅ 取到的正是号段里接下来的 10 个连号
✅ STOCK_TAKE 计数器正好前进 10（Δ=10）
✅ 其余 25 个号段的 current_no 纹丝不动     ← 按单据类型独立递增（不是全局单行）
✅ 重号扫描（26 张单据主表）：并发前后都是 0 重号
```

重号扫描只扫**单据主表**（一行 = 一张单）：明细 / 台账 / 日志表的 `*_no` 天然重复
（一张单 5 行明细就有 5 行 `doc_no`），首轮把它们算进来是假阳性，已按白名单收口。

### 1.4 顺手挖出一个真 bug：并发**首次**取号会把事务打成 aborted（已修）

首次跑 10 并发用例时直接红（`raw/` 之外的首次运行日志摘录）：

```
并发取号不应失败: Query { ... DbError { code: SqlState(E25P02),
  message: "current transaction is aborted, commands ignored until end of transaction block" } }
```

根因：`ensure_seq_row` 原来是 `INSERT … SELECT … WHERE NOT EXISTS` +
`.or_else(UniqueViolation => Ok(0))`。号段行**还不存在**时两个请求同时补行，
输的那个 INSERT 被 PG 判失败 → 事务立即 `aborted`；Rust 侧吞掉错误也没用，
紧接着真正取号的 `UPDATE … RETURNING` 只能报 `25P02` → **建单整笔 500**。
触发条件：新单据类型上线首日 / 空库首启的两个并发建单。

修复：`ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING`（与部分唯一索引逐字对应），
10 处 `ensure_seq_row` 全改（voucher 中心入口 + TK/IN/PO/CT/SN/SH/SR/SO/STG 各自的副本）。
修复后：

```
$ cat raw/db_tests.log          # 145 拷库，cargo test --lib
test business::erp::voucher::tests::fix8_concurrent_takes_never_duplicate ... ok
test business::erp::voucher::tests::fix8_seq_types_advance_independently ... ok
test result: ok. 34 passed; 0 failed        ← 含 budget_guard 32 条
```

## 2. 问题 2：盘点「审批 → 待过账 → 过账」= 设计意图（不改代码）

`raw/10_stocktake_two_step.txt`：**36 ✅ / 1 ❌**，唯一的 ❌ 是 p13 脚本自带的 ⑦ 巡检断言
（见 §5，与本卡改动无关）。业务链全绿：

```
② 建单（全盘）：账面数服务端快照 100，状态 DRAFT
③ 填实盘 92：差异 -8（盘亏），三本账仍旧 100 / 800 / 100
④ 提交审批 200 → 状态「待过账」PENDING_POST，挂审批实例
   ✅ 未审批就过账被拒（409）                ← 审批是硬闸门，不是提示
   仓管主管待办里通过（审批 200）
⑤ 过账 200 → POSTED：成本账 92 / 736、库位账 92、盘点流水净额 -8、
   凭证 借 1901 64 / 贷 1405 64
⑥ 作废 200 → CANCELLED：三本账回 100 / 800 / 100，蓝字 + 红字两张凭证
```

结论与理由（不可撤回、`posted_by` 审计、制单≠审批≠过账的岗位分离）写进
`docs/ERP-盘点审批过账口径.md`。卡面写的 `/stock-take/{id}/confirm` **不存在**且不该存在，
真正入口是 `PATCH /api/business/erp/stock-take/{id}/status` + `{"status":"POSTED"}`。

前端（本卡未改，只取证）：`raw/30_frontend_pending_post.txt` —— 「待过账」独立标签、
详情抽屉「过账」按钮由后端 `next_statuses` 驱动、点下去弹二次确认（写明不可撤销）。

### 拷贝库上一个必须先处理的坑（已在 `21_e2e.sh` 里收口）

审批节点是 **ROLE**（`WAREHOUSE_MANAGER`）⇒ **角色里每个成员各拿一个待办**。
`cloudpivot_dev` 里除了 p13 造的 `p13_keeper` 还留着演练账号（演练审批人），
它不点单 → 实例永远 `PENDING` → 过账 409「审批尚未通过」。
首轮就是这么红的（`[FAIL] 过账 200 — 409 10003`）。正式环境「一个角色多人 = 会签」是
正确语义，所以只能在夹具侧把该角色成员收敛成 `p13_keeper` 一个（脚本已加）。

## 3. 问题 3：`biz_expense_report.budget_mode` 记全局策略 —— FIX-2 已修，本卡不重复改

| 证据 | 内容 |
|------|------|
| 提交 | `2389836 fix(erp): 预算控制三连修复（FIX-2）—— … / budget_mode 记槽级`（`git log -S recorded_mode`） |
| 代码 | `budget_guard.rs::recorded_mode()`：取「参与判定的槽级策略里最严的一条」，没有槽才回落全局；`expense_handler.rs:955` 提交时写 `budget_mode` 列、`:1113` 写 `form_data` |
| 单测 | `budget_guard::budget_guard_tests::recorded_mode_takes_strictest_slot_mode`（全局 WARN + 槽级 BLOCK → 记 BLOCK）在本卡拷库跑 **ok** |
| 145 实测数据 | `biz_expense_report` 11 张已提交单 `budget_mode = WARN`，与该库 `erp.budget.control_mode = WARN` 一致（无槽级覆盖的库就是这个值） |

## 4. 复跑

```bash
ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix8-seq-and-stocktake/10_prepare_145.sh'   # 拉码+建二进制副本+建拷贝库
ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix8-seq-and-stocktake/11_start_instance.sh' # 起 8048
ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix8-seq-and-stocktake/12_run_db_tests.sh'   # 真库用例
ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix8-seq-and-stocktake/21_e2e.sh'            # 段 0 基线 + 段 A 盘点 + 段 B 取号器
ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix8-seq-and-stocktake/14_kill_instance.sh'  # 收尾（复核 8033）
```

## 5. 已知问题（都不在本卡范围，单独处理）

1. **p13 的 ⑦ 巡检断言在本卡环境红 1 条**，两个原因都不是 FIX-8 的改动：
   - `B2 库存金额 = 存货科目 1405 余额` 差 20：p13 的 `seed()` 会
     `DELETE FROM biz_voucher WHERE source_type='STOCK_TAKE'`，把拷贝库里演练的
     两张盘点凭证（1405 借 50 / 贷 30，净 +20）删掉，但**不回滚它们的库存影响**
     —— 于是模块 24380 / 总账 24360。基线（`raw/00_invariants_baseline.txt`）本来是
     `73/78 BLOCKER 1`（只有 K5 固资），跑完 p13 变 `BLOCKER 3`（B2/E4b/K5）。
   - `E4b 库存流水 biz_type 已全部登记进 E4 判据`：p13 期初夹具流水记
     `biz_type='SERIAL_BATCH'`，而 `scripts/verify/erp_invariants.py::E4_BIZ_TYPE_AXIS`
     没登记这个取值（`cloudpivot_dev` 里 SERIAL_BATCH 流水 0 条，所以基线不红）。
     这是巡检登记表的缺口（生产上做过「批量生成无来源入库」的库都会踩），建议单独卡补登记。
2. 全量 `cargo test` 仍未跑绿（种子缺失库 / 共享库历史不平数据两类既有红，FIX-6 已建卡
   `t_13ddcb84`）；本卡只跑触及模块（34 条全绿）。
3. 145 服役实例 8033 未重启 → V0330 等迁移与本卡改动尚未应用到共享库 `cloudpivot_dev`
   （本卡全部取证在拷贝库）。
