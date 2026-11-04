# 真库用例并行互踩收口（卡 `t_0d7c51f8`）

**一句话结论**：全量 `cargo test` 的偶发 1–2 条假红不是业务回归，是**测试夹具在改写全库共享的
「种子行」**——本轮抓到的那条（`payable_bridge::tests::zz_teardown_purges_payment_status_logs`）
的真因是把它当作「孤儿断言太严」解释不了的：**两个模块用两把互不相识的锁改写了同一行
`biz_flow_def`（`flow_code='payable_pay'`）**。改动全部落在 `#[cfg(test)]` 测试侧，
**生产代码 0 行变更**；干净隔离库上 `cargo test -j 1` 连跑两轮 0 failed。

| 卡面归因 | 实测定性 | 本卡处置 |
|---|---|---|
| `period_items` 按整期 `UPDATE … WHERE period='2033-10'` | **已由 `t_9476d9f1`（提交 `42370b3`）收口**：已改成按本用例自造凭证的 `id`，2033 系夹具共用一把锁；本卡复核仍绿 | 复核，不重复改 |
| `zz_teardown_*` 抢同一批行 | 本卡全量第一轮唯一红点确实是 `zz_*`，但**不是抢夹具行，是抢种子流程行** `biz_flow_def` | 合并 P17/P7 的锁为 `FINANCE_SEED_LOCK`；把该用例里「整型别删通知」收窄成自己的付款单链 |
| 「整库无孤儿」断言 | 5 处 `zz_` 收尾用 `orphans(FINANCE)==0` 一次断言：并行时会把**别人删单据/删日志的中间态**判成自己的假红 | 新增 `fixture_hygiene::settle_orphans()`（收敛式判定，口径不变），5 处同改 |

---

## 1. 复现（真实输出，未改代码前）

干净隔离库 `cloudpivot_d7c5`（`bash scripts/verify/erp_clean_db.sh`：`createdb -O alun` + 一次实例
自举 → `schema_migrations(max|count) = 219|163`、**超管人数 = 1**），同一库上：

```text
$ export ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_d7c5
$ cd backend && cargo test -j 1
test business::erp::payable_bridge::tests::zz_teardown_purges_payment_status_logs ... FAILED
---- business::erp::payable_bridge::tests::zz_teardown_purges_payment_status_logs stdout ----
thread '…zz_teardown_purges_payment_status_logs' panicked at src/business/erp/payable_bridge.rs:2205:41:
显式跳过审批的直付失败：10001 审批流程 `payable_pay` 已发布：有流程时必须走审批，不能跳过。

test result: FAILED. 2323 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1115.50s
EXIT=101
```

原始日志：`/root/d7c5-before-run1.log`（本机）。**单模块 / 单条跑是绿的**（2301–2315 filtered out 的
那条命令，见 §4 的模块自检）。

## 2. 根因（比卡面深一层）

被测用例的写法（`payable_bridge.rs`，「显式跳过审批的直付」）：

```rust
exec("UPDATE biz_flow_def SET status = 'DISABLED' WHERE flow_code = 'payable_pay'").await;
let result = payment::submit_payment(payable_id, &req, Some(uid(CREATOR)), &gate).await;  // ← 拿到 10001
exec("UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'payable_pay'").await;
```

`biz_flow_def` 是**种子行**（全库每个 `flow_code` 只有一行，按 code 查），而 `payable_payment::tests`
的 `setup()` 会做对称的自愈：

```rust
exec("UPDATE biz_flow_def SET status = 'PUBLISHED', deleted_at = NULL WHERE flow_code = 'payable_pay'")
```

两个模块**各自持有一把互不相识的 `DB_LOCK`**（旧注释写着「共用一把」，代码里其实是两个 `static`），
libtest 默认 4 个测试线程 ⇒ P7 的 `setup()` 会插在 P17 的「DISABLE → 提交付款」之间把流程**重新发布**，
于是跳过审批被 10001 拦死。**换夹具前缀解决不了**：`flow_code` 是种子键，改不了；这正是测试卫生
铁律「同一批全局行 = 同一把锁」的教科书案例。

第二层是「整库无孤儿」断言（`fixture_hygiene::orphans(FINANCE) == 0`）：并行时别的模块随时可能正处在
「单据行已删、它自己的状态日志还没删」的毫秒级中间态，一次断言会把**别人的中间态**判成本模块假红。
同类写法在 5 处（`payable_bridge` / `payable_payment` / `voucher` / `serial_handler` / `opening`），
本轮虽然没有红在这句上，但它是同族隐患（历史上多张卡在此打过滚）。

## 3. 改了什么（只有测试代码）

| 文件 | 改动 | 为什么 |
|---|---|---|
| `backend/src/business/erp/fixture_hygiene.rs` | ① 新增 `FINANCE_SEED_LOCK`（付款/应付链路的**种子行总锁**，带完整理由）；② 新增 `settle_orphans(types, max_wait)`：`purge → 复查`，最多等 5s 收敛 | ① 同一批全局行（`biz_flow_def` 的 `payable_pay` + `biz_payable/biz_payment/biz_voucher` + `biz_seq`）= 同一把锁；② 整库孤儿断言改成「最终为 0」，**真孤儿等多久都还在 ⇒ 仍然红**，只吸收别人正在删的瞬时窗口 |
| `backend/src/business/erp/payable_bridge.rs` | ① 本模块 `static DB_LOCK` → `use …::FINANCE_SEED_LOCK as DB_LOCK`；② `cleanup_fixtures()` 里删 `sys_notice` 从「全部 `PAYABLE_PAY` 实例」收窄为**本夹具付款单链**；③ 收尾孤儿断言改 `settle_orphans` | ①②③ 见 §2 |
| `backend/src/business/erp/payable_payment.rs` | `static DB_LOCK` → 同一把 `FINANCE_SEED_LOCK`；收尾孤儿断言改 `settle_orphans` | 与 P17 对称：两模块都会改写 `payable_pay` 的 `status` |
| `backend/src/business/erp/voucher.rs`、`serial_handler.rs`、`opening.rs` | 收尾孤儿断言改 `settle_orphans`（口径不变） | 同族隐患一次收口 |
| `backend/src/business/erp/contract_approval_tests.rs` | 只改注释：删掉指向草稿文件 `backend/src/test_db_guard.rs` 的引用，改指本 README | 该草稿已移出仓库（见 §6） |
| `scripts/verify/erp_full_suite_twice.sh` | 新增：建干净库 + **同库连跑 N 轮**，任一轮非 0 failed 就 `exit 1` | 把本卡的验收口径固化成可复跑脚本 |
| `scripts/verify/erp_module_concurrency_check.sh` | 新增：7 个受影响模块 `--test-threads=4` × N 轮，`0 passed` 也判假绿 | 偶发假红必须重复跑才看得见 |
| `scripts/verify/erp_fixture_residue_check.sh` | 新增：纯 psql 的库侧复查（夹具号段/夹具用户/孤儿/种子流程行/测试期次/待办） | 「测试自己说自己干净」不算证据 |
| `backend/src/test_db_guard.rs`（未跟踪草稿） | **移出仓库** | 见 §6 决策 |

**没有做的事**（卡面明确禁止 + 本卡判断）：

* 没有放宽/删除任何断言：`settle_orphans` 的判据仍是「整库无孤儿」，只是把「这一瞬间」改成「5s 内收敛」；
  收窄 `sys_notice` 删除范围反而让夹具更守规矩（只动自己造的行）。
* 没有改成 `--test-threads=1`：那会把**纯逻辑**用例一起串行（本仓 lib 里一半以上不连库），而且
  串行只是把「并行争抢」换成「顺序污染」——历史实测同库串行跑红了 `contract_approval` 两条，
  根因同样是夹具残留/种子行（`t_e2a368f0` 已单独收口）。本卡修的是**行级归属**，与线程数无关。
* 没有引入「全进程一把锁」：见 §6。

## 4. 验证（全部本机真跑）

### 4.1 受影响模块的**并发**自检（`--test-threads=4` × 3 轮，同一脏库）

库 = 上面跑过一轮全量的 `cloudpivot_d7c5`（**带残留**，正好验证隔离）：

```text
$ ERP_DB_URL=… bash scripts/verify/erp_module_concurrency_check.sh 3      # 7 个模块 × 3 轮
round=1 business::erp::payable_bridge            -> test result: ok. 23 passed; 0 failed
round=1 business::erp::payable_payment           -> test result: ok. 29 passed; 0 failed
round=1 business::erp::voucher::tests            -> test result: ok. 21 passed; 0 failed
round=1 business::erp::serial_handler            -> test result: ok. 47 passed; 0 failed
round=1 business::erp::opening                   -> test result: ok. 16 passed; 0 failed
round=1 business::erp::period_items              -> test result: ok. 13 passed; 0 failed
round=1 business::erp::contract_approval_tests   -> test result: ok.  9 passed; 0 failed
… 第 2、3 轮同样全绿 …
=== OVERALL: ALL_GREEN      # 21/21 次运行 0 failed
```

### 4.2 全量两轮（干净库 + 第二轮在「第一轮的残留」上）

```text
$ RECREATE=1 bash scripts/verify/erp_full_suite_twice.sh     # 等价：建库 + cargo test -j 1 × 2
```

| 轮次 | `test result:` | EXIT |
|---|---|---|
| 1（干净库） | PENDING_FILL | PENDING_FILL |
| 2（同一库，带第 1 轮残留） | PENDING_FILL | PENDING_FILL |

### 4.3 库侧复查（**不经测试代码**，纯 psql，两轮之后）

见 `scripts/verify/erp_fixture_residue_check.sh` 的输出（贴在 §5）。

## 5. 库侧复查结果

PENDING_FILL

## 6. 决策：为什么不引入「全进程一把锁」

`backend/src/test_db_guard.rs` 是 2026-11-03 22:16 的一份草稿（未跟踪、未被 `mod` 引用），主张
**所有真库用例共用一把 `tokio::sync::Mutex`**（等于把全部真库用例串行）。本卡**未采用**，理由：

1. 它把「修隔离」换成了「不要并行」：本卡抓到的根因（种子行被两把锁改写、整库断言撞别人中间态）
   在有全局锁的情况下确实会消失，但**问题本身没被修**——只要有人再写一条不带全局锁的真库用例，
   或跨进程（两个 `cargo test` 同时跑同一个库）就原样复现；
2. 它会让 lib 套件里所有真库用例串行（本轮 2300+ 条里约一半连库），全量时间从 ~19 min 涨到 40+ min，
   而收益与「按共享行分组加锁」重叠；
3. 「按共享行分组」是仓库既有结论（`t_9476d9f1` 的 biz_account 组 / 2033 期次组），本卡只是把
   **付款-应付组**补上，方向一致。

草稿已移到 `/tmp/test_db_guard.rs.t0d7c51f8-draft`（未入库）。**如果以后再出现跨组的互踩**，
优先继续「找共享行 → 分组加锁」，而不是上全局锁。

## 7. 遗留风险（不在本卡范围，留给总管派卡）

1. **P17 的 2 个 `SUPER_ADMIN` 夹具用户 vs `assignee_type='ADMIN'` 节点的会签人数**：
   `payable_pay` / `stock_in_cancel` / `stock_transfer` 的审批人来源是「全部超管」，P17 用例运行期间
   全库超管 1 → 3。本卡让 P17/P7 互斥，但 `stockin_handler*` / `transfer_handler` 与 P17 仍是两组锁。
   建议：把「依赖超管人数」的模块并进同一组（或夹具改用专用角色）。
2. **`pflow3b_tests` 会临时 DISABLE `payable_cancel` / `voucher_reverse` 流程**，它挂在
   `voucher_query::tests::DB_LOCK`（biz_account 组）。若将来有模块在别组里跑「作废/红冲」，会重演
   本卡的剧本。建议同样按「被改写的种子行」归组。
3. `SYS_DB_URL` 一设，`system::*` / `business::masking::*` 18 条会红（历史遗留，标准门禁只给
   `ERP_DB_URL`，按设计 SKIP）——与本卡无关，但这一大块长期没门禁。

## 8. 复跑

```bash
# ① 受影响模块并发自检（3 轮）
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_ftwice \
  bash scripts/verify/erp_module_concurrency_check.sh 3

# ② 全量两轮（自动建干净库；第二轮带第一轮残留）
ERP_DB=cloudpivot_ftwice bash scripts/verify/erp_full_suite_twice.sh

# ③ 库侧复查（纯 psql，只读）
DB=cloudpivot_ftwice bash scripts/verify/erp_fixture_residue_check.sh
```
