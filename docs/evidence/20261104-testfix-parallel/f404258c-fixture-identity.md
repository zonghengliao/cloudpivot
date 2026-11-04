# t_f404258c — period_items / contract_approval 夹具身份唯一化（实测留档）

> 基线：`/opt/cloudpivot` 工作树（HEAD = `ed5b746`，含兄弟卡未提交 WIP）。
> 库：`cloudpivot_d7c5`（`schema_migrations` 163 条 = 最新；**带残留**，正好验证隔离）。
> 所有数字都是本机真跑输出，未改写。

## 1. 改了什么（只改夹具身份与写操作范围，断言 0 改动）

| # | 文件:行号 | 类型 | 改法 |
|---|---|---|---|
| P1 | `period_items_tests.rs:26,87,128,192,340,364,402,431,448` | (a) 期次不唯一 | `open_test_period(3..10)` → `open_case_period(1..9)`；期次字符串改由返回行的 `period_code` 派生（无字面量），用例独占 `2040-0N` |
| P1' | `period_test_support.rs:308-342` | 新增门禁 | `ITEMS_PERIOD_YEAR = 2040` + `open_case_period()`：同进程重复申请同一序号 **直接 panic** ⇒「两条用例同期次」从约定变断言 |
| P2 | `period_items_tests.rs:461` | (b) 已修（复核） | `set_voucher_status(voucher_id, "POSTED")`（42370b3 已按 id），本次未回改 |
| P3 | `period_items_tests.rs:49` | (a) 读取范围 | `draft_voucher_items(db(), &period, …)` + `count==3`：期次独占后读取范围 ≡ 本用例 3 张（生产函数签名不动） |
| P4 | `period_test_support.rs:157-257` | (b)+(c) | `cleanup()` 改两段式：先在**本夹具拥有的**期次/号段里 `SELECT id`（`:118` `OWNED_PERIOD_PREFIXES = 2033-%, 2040-%`），再 `DELETE … WHERE id IN (…)`；不再出现按 period 整批 UPDATE/DELETE |
| P5 | `period_test_support.rs:158-186` | (b)+(c) | 期间/通知/状态日志改为「凭证 id / 期间 id 派生删除」 |
| P6 | `period_test_support.rs:193-213` | (c) | `P2-PO-/P2-IN-/P2-TR-` 前缀删除 → 取 id 后按 id 删 |
| P7 | `period_test_support.rs:215-256` | (c) | 角色/用户/菜单同改 id 集合（`P2-/P2I-` 号段取 id，写操作按 id） |
| C1 | `contract_approval_tests.rs:165-181,190-257` | (c) | `DELETE … biz_type='CONTRACT_APPROVE'` 全库删 → 只删「挂在 `A2AP-` 合同上」的实例/通知/日志（先取 id，再按 id 删） |
| C5 | `contract_approval_tests.rs:295,300-390` | (a) | 共享种子角色 `ROLE_PM/ROLE_GM`：改为**锁内借用 + 登记 + 归还**（`borrow_role_membership` / `restore_borrowed_bindings`），不再永久摘别人的绑定 |
| C5' | `contract_approval_tests.rs:65` + `fixture_hygiene.rs:147` + `purchase_handler.rs:1753` | R6 锁组 | 新增 `fixture_hygiene::SEED_ROLE_LOCK`；`contract_approval_tests` 与 `purchase_handler::tests` 共用同一把锁 |
| C7 | `contract_approval_tests.rs:857-880` | (c) 断言 | `zz_teardown` 残留断言从「整库 CONTRACT_APPROVE 实例」收窄到「清场前记下的本模块 id 集合」 |

## 2. 为什么「只过滤待办」不够（C8 的实测证据）

去掉「摘别人绑定」后第一次跑：`pending_tasks_of_fixtures` 过滤后条数对了，但

```text
---- small_amount_single_level_approval_signs_contract
assertion `left == right` failed: 一级审完就应通过
  left: "PENDING"
 right: "APPROVED"
---- large_amount_requires_two_levels_in_sequence
assertion `left == right` failed: 第二级应有一条总经理待办
  left: 0
 right: 1
```

原因：审批节点按**角色成员**展开，种子 `join_mode = AND` = 会签 —— 角色上多一个人就多一条待办，
且必须所有人都批完节点才推进。所以「角色成员 = 本模块夹具」是这两条业务事实的**前提**，
只能靠独占成员（借用）实现，不能靠过滤。

## 3. 验证（本机真跑）

```text
period_items   round1/2/3: ok. 13 passed; 0 failed     (--test-threads=4)
contract       round1/2/3: ok.  9 passed; 0 failed     (--test-threads=4)
purchase_handler（与 contract 两个进程并行，同库）
               contract: ok. 9 passed;   purchase: ok. 50 passed; 0 failed
period 簇（period::tests + period_items + period_remind）round1/2: ok. 39 passed; 0 failed
```

改前同库并行复现（两把锁）：`purchase_handler` 一轮 5 条红
（`committed_amount_sums_previous_orders` / `contract_amount_limit_is_enforced` /
`cancel_submitted_requires_reason_then_appends_remark` / `contract_order_outside_validity_is_rejected` /
`contract_order_with_mismatched_supplier_is_rejected`）。

运行后库侧复查：借用的 `p3a_0003/p3a_0004` 绑定已归还，本模块夹具零残留：

```text
 role_code        | username
------------------+----------
 GENERAL_MANAGER  | p3a_0004
 PURCHASE_MANAGER | p3a_0003
```

## 4. 门禁

```text
gate ①（本任务三文件）: 0 命中
gate ①（全目录，他卡范围）: 14 行（基线 17，本次 -3 行纯注释）
cargo clippy --all-targets -- -D warnings: EXIT=0
rustfmt --check（5 个改动文件）: 0 diff
```

## 5. 遗留（不在本卡范围）

* `period.rs:1584-1596`（P8）仍按 `2031/2032/2033-%` 整期清场；`period_items` 已迁出 2033，
  所以它不再影响本模块，但 `period_remind_tests`（2033-02..08）仍在该语句范围内。
* `stocktake_tests` 的 GM/WM 成员只增不减（不自摘），在本模块「借用」窗口内会被一并借走再归还，
  语义无影响（其 `setup()` 会重新挂回）。
