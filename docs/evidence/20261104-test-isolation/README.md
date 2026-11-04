# 测试隔离：`cargo test` 两条恒定假红收口（卡 t_9476d9f1）

**结论先说**：两条「恒定假红」都是**测试夹具隔离问题**，不是产品缺陷。改动全部落在
`#[cfg(test)]` 测试侧（8 个文件），**生产代码 0 行变更**。修后干净库上
`cargo test -j 2 --no-fail-fast` **两轮连续 0 failed**。

| 假红 | 根因（一句话） | 修法 |
|---|---|---|
| `cash_flow::tests::r3_real_config_covers_all_accounts` | `aux_tests` 造的 `P21AUXA/B` 科目**只在下一个用例开工时和 `zz_` 收尾用例里清**，多线程下 libtest 不保证 `zz_` 最后跑 ⇒ 全表断言遍历到没分类行的夹具科目 | 每条用例包进 `case()` 外壳：正常返回 / panic **都**清场；另把 5 个「会往 `biz_account` 造夹具」的模块统一到同一把锁 |
| `period_items::tests::can_close_is_unaffected_by_items` | `period::tests` 与 `period_items/period_remind/stocktake` **各拿一把互不相识的锁**，`period::tests::cleanup()` 的 `period LIKE '2033-%'` 会把对方正在用的 2033-10 草稿凭证删掉；用例自己还写 `UPDATE ... WHERE period='2033-10'` 整期过账 | 三处 2033 夹具共用一把锁；整期 UPDATE 收窄成本用例自造凭证的 `id` |

---

## 1. 修前复现（真实输出）

干净库（`cp_fixt9476`，V0217 / 161 条迁移 / 超管 1 人）+ 按模块过滤跑，稳定复现两条：

```text
# ① aux_tests + cash_flow（同一把锁，不并发；红的原因是「残留」不是「争抢」）
$ ERP_DB_URL=… cargo test --lib --no-fail-fast -- business::erp::aux_tests business::erp::cash_flow::tests
test business::erp::cash_flow::tests::r3_real_config_covers_all_accounts ... FAILED
thread '…' panicked at src/business/erp/cash_flow.rs:2539:9:
分类配置漏科目：["P21AUXA P2-1 测试资产 没有任何分类行（会全部落到「其他」）", "P21AUXB P2-1 测试负债 没有任何分类行（会全部落到「其他」）"]
test result: FAILED. 40 passed; 1 failed

# ② period::tests + period_items::tests + period_remind_tests
$ ERP_DB_URL=… cargo test --lib --no-fail-fast -- business::erp::period::tests business::erp::period_items::tests business::erp::period_remind_tests
test business::erp::period_items::tests::can_close_is_unaffected_by_items ... FAILED
thread '…' panicked at src/business/erp/period_items_tests.rs:459:5:
test result: FAILED. 27 passed; 1 failed
```

## 2. 根因（比卡上的归因各深一层）

### ① 不是「并行争抢」，是「**残留 + 名字排序兜底**」

`aux_tests` 与 `cash_flow` 用的是**同一把** `voucher_query::tests::DB_LOCK`，两者本来就不并发。
真正的漏洞是清场时机：

* `setup()` → `seed()` → `cleanup()`：只在**下一个用例开工时**清；
* `zz_p21_aux_teardown_purges_fixtures`：靠 `zz_` 名字排序「最后跑」。

`cargo test` 默认 4 个测试线程（注意 `cargo test -j 2` 的 `-j` 是**编译并行度**，不是
`--test-threads`），libtest **不保证**函数名顺序，`zz_` 完全可能排到前面 ⇒ 本组跑完后两个
无分类行的科目留在库里 ⇒ 任何时刻执行到 `r3_real_config_covers_all_accounts` 就红。

### ② 不是「固定期次」，是「**两把锁 + 整期 UPDATE**」

`period::tests` 有自己的 `DB_LOCK`，`period_test_support`（`period_items` / `period_remind` 用）
有另一把，`stocktake_tests`（夹具期也是 `2033-03`）第三把。于是：

* `period::tests::cleanup()` 的 `DELETE FROM biz_voucher WHERE period LIKE '2033-%'` 与
  `biz_accounting_period LIKE '2033-%'` 会在 `period_items` 用例体跑到一半时把它刚种的
  2033-10 草稿凭证删掉 ⇒ `assert!(!blocked.can_close)` 失败（没有挡路单据了）；
* 用例自己那句 `UPDATE biz_voucher SET status='POSTED' WHERE period='2033-10'` 反过来会把
  同一期间里别的用例刚种的草稿一并过账 ⇒ 它们的「必须有草稿挡路」断言随机红。

## 3. 改了什么

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/aux_tests.rs` | 新增 `case()` 外壳（`AssertUnwindSafe(...).catch_unwind()` → `cleanup()` → `resume_unwind`），10 条真库用例全部包进去；`zz_p21_aux_teardown_purges_fixtures` 降级为**外部交叉验证** |
| `backend/src/business/erp/period_test_support.rs` | `DB_LOCK` 升级为「2033 夹具共享锁」并写清理由；新增 `set_voucher_status(id, status)`（按 id 改状态） |
| `backend/src/business/erp/period.rs` | `mod tests` 的 `DB_LOCK` 改为 `use …::period_test_support::DB_LOCK`（同一个对象） |
| `backend/src/business/erp/stocktake_tests.rs` | 同上（夹具期 2033-03，与 period 清场冲突） |
| `backend/src/business/erp/period_items_tests.rs` | 整期 `UPDATE … WHERE period='2033-10'` → `support::set_voucher_status(voucher_id, "POSTED")` |
| `backend/src/business/erp/model_tests.rs` | `DB_LOCK` → 共用 `voucher_query::tests::DB_LOCK`（夹具科目 `W1B1001/W1B100101` 只在 `zz_` 里删，同源隐患） |
| `backend/src/business/erp/pflow3b_tests.rs` | 同上（夹具科目 `P3B.1002/P3B.2202`，`purge_fixtures()` 只在开工时跑） |
| `backend/src/business/erp/voucher.rs` | 同上（夹具科目 `P2C1.1405`） |

**为什么「共用锁」而不是「改成 `--test-threads=1`」**：`-j`/`--test-threads=1` 会把**纯逻辑单测**
也一起串行（本仓 lib 套件里一半以上是不连库的纯函数用例），而且历史上串行跑会把「争抢」换成
「顺序污染」（`contract_approval_tests` 另两条）。共用锁只让**碰同一批全局行**的真库用例互斥，
纯逻辑用例照旧跑满 4 个线程。

## 4. 同类隐患审计（本卡顺手核过）

`grep -n "from biz_account where" src/business/erp/cash_flow.rs` **无命中** —— 卡上指的位置不准，
真正的「全表」出现在被调用的取数函数里：`report::load_accounts()`（`report.rs:938`
`FROM biz_account a … WHERE a.deleted_at IS NULL`，无其它收窄），由 `cash_flow.rs:2499` 调用。

| # | 全表 / 全库型断言 | 作用域 | 会不会被并行夹具污染 | 处置 |
|---|---|---|---|---|
| 1 | `cash_flow::tests::r3_real_config_covers_all_accounts` | `biz_account` **全表** | **会**（已实测红：P21AUX；同源：W1B / P3B. / P2C1. / P2D1） | 已收口（见 §3） |
| 2 | `purchase_handler::tests` 收尾「全库无 PENDING PO_APPROVE」 | `biz_type='PO_APPROVE'` | 不会：该类型实例只有本模块造（`grep -rln "'PO_APPROVE'" src/` → 仅 purchase_handler/approval_gate/purchase_bridge/internal_control） | 无需改 |
| 3 | `stockin_handler_tests_fixtures` 收尾「全库无 PENDING STOCK_IN_POST」 | `biz_type` 限定 | 不会：仅 stockin_* 系列造，且共用一个 `pub(super) static DB_LOCK` | 无需改 |
| 4 | `transfer_handler::tests` 收尾「全库无 PENDING STOCK_TRANSFER」 | `biz_type` 限定 | 不会：仅 transfer_* 造 | 无需改 |
| 5 | `pflow3b_tests` 收尾「全库无 PAYABLE_CANCEL / VOUCHER_REVERSE 实例」 | `biz_type` 限定 | 不会：`payable_bridge.rs` 只**读**这两个类型（633 行是取流程定义） | 无需改 |
| 6 | `contract_approval_tests` 收尾「全库 CONTRACT_APPROVE 计数」 | `biz_type` 限定 | 不会：仅本模块造 | 无需改 |

## 5. 验证（全部是本机真跑输出）

* 修前两条红 → **同一条命令**修后全绿（见 §7 对照表）；
* 同一干净库 `cargo test -j 2 --no-fail-fast` **连跑三轮，lib 目标每轮 2295 passed / 0 failed**；
  其中 A、C 两轮 22/22 target 全绿（B 轮唯一红来自**共享 testd 库被别的 worker 并行占用**，非本卡）；
* `case()` 的 panic 路径**演练过**：故意 `panic!` → 用例仍 `FAILED`（不吞红），且库里
  `P21AUX%` 科目/凭证残留均为 **0**。

## 6. 遗留隐患（**不在本卡范围**，留给总管派卡）

1. **`payable_bridge` 夹具在库里挂 2 个 `SUPER_ADMIN` 用户**（`p17_ap1/p17_ap2`，
   `payable_bridge.rs:1144-1170`），而审批节点的 `assignee_type='ADMIN'` 是**运行时展开全库超管**。
   它跑用例期间，任何依赖「ADMIN 节点会签人数」的用例（`stock_in_cancel` / `stock_transfer` /
   `payable_pay`）都会从 1 人变 3 人。这与 `scripts/verify/erp_clean_db.sh` 头注释记录的历史事故同源，
   只是从「残留」变成了「并发」。修法二选一：把这三类模块的锁合并，或夹具改用专用角色而不是 `SUPER_ADMIN`。
2. **`SYS_DB_URL` 一设，18 条 `system::*` / `business::masking::*` 用例会跑且红**（本次实测
   2277 passed / 18 failed）。标准门禁只给 `ERP_DB_URL`，它们按设计 SKIP，所以不计入；
   但这意味着这一大块**长期没有门禁**，建议单独立卡按 `system` 模块自己的夹具口径收口。
3. `backend/src/test_db_guard.rs`（未跟踪，11-03 22:16 的草稿）主张「全进程一把锁」，与本卡
   「按共享行分组」的结论方向一致但更激进（等于把全部真库用例串行）。**本卡未采用也未删除**，
   留给下一张卡决策。

## 7. 复跑记录（真实输出）

**环境**：本机 4 核 / 15G。隔离库 `cp_iso9476`（V0217 / 161 条迁移 / 超管 1 人，由
`scripts/verify/erp_clean_db.sh` 建），隔离 testd 库 `cloudpivot_test_9476`。
代码树 = `git archive HEAD(74df3b3)` + 本次 8 个测试文件的改动
（`git diff 74df3b3..HEAD -- backend/` 为空，①后端源码未变②验证期间主仓已被别的卡推到
`4624a80`，但那次提交只碰 frontend/docs/scripts）。

> 为什么在独立树里跑：`/opt/cloudpivot` 是**多 worker 共享主仓**，验证期间另一张卡（P2-4 库存货位）
> 正在改 `stockin_*`，主仓一度**编译不过**（`StockInItemReq` 少 `location_dict_id`）。用
> `git archive` + 独立 `CARGO_TARGET_DIR=/root/t9476-target` 隔离，结果才可复现。
> 隔离树唯一副作用：`config/config-dev.toml`、`config/config-test.toml` 是 **gitignore 的本地配置**，
> archive 里没有 → 第一轮多出 1 条 `shared::startup_guard::tests::merge_semantics_or_base_and_profile`
> 红；把这两份本地配置补进隔离树后单跑 `ok`（**隔离手法问题，不是代码问题**）。

| 轮次 | 命令 | lib 目标 | 全部 target | 墙上时间 |
|---|---|---|---|---|
| A | `ERP_DB_URL=…/cp_iso9476 cargo test -j 2 --no-fail-fast` | **2295 passed / 0 failed**（1007s） | **22/22 target 0 failed** | 19m39s |
| B | 同上（同一库第 2 轮） | **2295 passed / 0 failed**（1013s） | 21/22：`testd::storage::zz_no_fixture_residue_after_full_suite` 红 | 20m12s |
| C | 同上 + `TESTD_DB_URL=…/cloudpivot_test_9476`（第 3 轮） | **2295 passed / 0 failed**（1004s） | **22/22 target 0 failed**（`testd` 67/67） | 19m48s |

B 轮那条 testd 红的定性证据：`zz_no_fixture_residue_after_full_suite` 报
`cloudpivot_test 存在本卡夹具残留（sys_user=1, sys_file=1）`，而——
（a）该目标用的是**共享库** `cloudpivot_test`（`TESTD_DB_URL` 缺省值），别的 worker 也会跑 testd；
（b）事后直接查库 `select username, created_at from sys_user where username like 'TD-%'` → **0 行**
（残留是别人进程的**在跑夹具**，它收尾时自己删了）；
（c）本卡**一个 testd 文件都没碰**（`git diff --stat` 只有 8 个 `business/erp/*` 测试文件）；
（d）C 轮把 testd 指到自己的库 → 67/67 全绿。
结论：共享库跨进程撞车，与本卡无关。

**修前 / 修后：同一条命令对照（这是最直接的证据）**

| 命令 | 修前 | 修后 |
|---|---|---|
| `-- business::erp::aux_tests business::erp::cash_flow::tests` | 40 passed / **1 failed** | 41 passed / **0 failed**（24.6s） |
| `-- business::erp::period::tests business::erp::period_items::tests business::erp::period_remind_tests` | 27 passed / **1 failed** | 28 passed / **0 failed**（31.1s） |
| `-- business::erp::model::tests business::erp::cash_flow::tests business::erp::pflow3b_tests` | （同源隐患，见下） | 68 passed / **0 failed**（67.8s） |

**panic 路径演练**（证明「清场挂在壳上」不是只覆盖正常返回）：

```text
注入 panic!  → test business::erp::aux_tests::p21_entries_roundtrip_all_four_aux_columns ... FAILED
               DRILL: 故意 panic，验证 case() 仍然清场且用例仍然红
库侧复查（psql，不经测试代码）
             → biz_account P21AUX% = 0，biz_voucher P21AUX% = 0
撤掉注入     → 同一条 ... ok
```

**同源隐患实测**（证明「5 个造科目的模块共用一把锁」不是过度设计）：把 `model_tests` 夹具里
那两行科目按**同一条 SQL** 直接插进干净库 →

```text
INSERT 0 2      -- W1B1001 / W1B100101
test …r3_real_config_covers_all_accounts ... FAILED
分类配置漏科目：["W1B1001 W1B 库存现金 没有任何分类行（会全部落到「其他」）",
                "W1B100101 W1B 人民币现金 没有任何分类行（会全部落到「其他」）"]
DELETE 2        → test … ok
```

**静态检查**：`CARGO_BUILD_JOBS=2 cargo clippy --all-targets -- -D warnings` → **EXIT=0**
（`Finished dev profile … in 1m 53s`，0 warning / 0 error）。

**库侧残留复查**（跑完三轮后，psql 直查，不经测试代码）：

```sql
select count(*) from biz_account where account_code ~ '^(P21AUX|W1B|P3B|P2C1|P2D1)';      -- 0
select count(*) from biz_voucher where period like '2033-%' or source_no like 'P21AUX%';   -- 0
select period_code, status from biz_accounting_period where period_code like '2033-%';     -- 1 行（见下）
```

**已知残留（无害，记录在案）**：`biz_accounting_period` 会留 **1 行** `2033-xx`。
原因同 §2 ①：`period_test_support::setup()` 只在**开工时**清，所以该组最后一条用例的期间行留在库里。
它**不影响连跑**——B、C 两轮都是带着这行跑全绿（下一轮开工时 `cleanup()` 会按 `2033-%` 删掉，
`open_test_period()` 不会撞已存在的期间）。要彻底清干净得把 `period_items_tests` /
`period_remind_tests` 也套上 `case()` 外壳（约 20 条用例的机械改动），建议随下一次夹具整备一起做，
不与本卡混在一起放大 diff。

