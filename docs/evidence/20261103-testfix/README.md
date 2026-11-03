# TEST-FIX 证据：收货单作废/过账审批链路 57 条单测与会签种子不一致（卡 t_6d815eda）

- 日期：2026-11-03
- 机器：`172.18.10.99`（工作仓 `/opt/cloudpivot`；本卡在隔离 clone `/root/t6d8-src` 上做，基线 `42f671e`）
- 基线提交：`42f671e`（= DEPLOY-B 的 `418c627` + D3-UI 三个提交）
- 结论：**业务代码一行未改**（`stockin_bridge::settle` / `flow::settle_guard` / 种子流程
  `self_approval=FORBID` 全部原样），只修**测试夹具对「审批人恒为 1 人」的错误假设**。

---

## 1. 根因（库侧可验证，不是猜测）

`stock_in_cancel` 的复核节点 `n_review`：

```
assignee_type = 'ADMIN'      -- 运行时展开**全部超管**（flow/assignee.rs::load_admins）
join_mode     = 'AND'        -- 会签（每个人都要点）
self_approval = 'FORBID'     -- 禁止自审（C1/内控的既有行为，本卡没动）
empty_assignee= 'FALLBACK_ADMIN'
```

`load_admins` 的口径是「`role_code = 'SUPER_ADMIN'` 且未删的用户」。于是**待办条数 = 库里超管人数**：

| 库状态 | 复核待办 | 老夹具（只批第一条）的结果 |
|--------|----------|---------------------------|
| 只有 `admin` 一个超管 | 1 条 | 实例 `APPROVED` → 单据 `CANCELLED`（绿） |
| 多出 `p17_ap1` / `p17_ap2`（`payable_bridge` 的夹具，同为超管） | 3 条 | 实例停在 `PENDING` → 单据停在 `PENDING_CANCEL`（**红**） |

失败现场（与 DEPLOY-B 卡面记录一致）：

```
src/business/erp/stockin_handler_tests_helpers.rs:343
assertion `left == right` failed: 复核通过后单据必须真的作废
  left: "PENDING_CANCEL"
 right: "CANCELLED"
```

`on_cancel_settled` 只在实例落**终态**时回调（`settle_verdict` 对 `PENDING` 返回 `None`），
所以「只批一条」时回调根本没被触发 —— 这也解释了 DEPLOY-B 记下的
「`settle_guard` 留痕 0 条」：**前置校验没拦，是回调压根没跑**。

## 2. 确定性复现（同一份代码、只改库状态）

```bash
# ① 干净库（只有 admin 一个超管）
bash scripts/verify/erp_clean_db.sh            # 见下文 §5 的完整口径
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_t6d8_clean \
  cargo test -j 1                               # lib: 2087 passed / 0 failed

# ② 往库里补 2 个超管（与 payable_bridge 的 p17_ap1/p17_ap2 同形态）→ 3 人会签
#    见 /root/t6d8-repro.sh（插入 sys_user + sys_user_role(SUPER_ADMIN)）
# ③ 只跑作废链路
<cargo test 产出的 lib 测试二进制> stockin_handler_tests_cancel --test-threads=1
```

修前（3 超管）：

```
test result: FAILED. 4 passed; 11 failed; 0 ignored
  cancel_keeps_invariant_e4 / cancel_posted_voids_serials_of_the_receipt /
  cancel_rolls_back_cost_inventory / cancel_rolls_back_location_ledger /
  cancel_still_generates_red_voucher / cancel_twice_is_409 /
  cancel_uses_posted_amount_not_current_avg /
  cancel_voids_only_serials_of_that_receipt / cancel_writes_out_txn /
  cancel_zero_qty_zeroes_amount / concurrent_cancels_do_not_deadlock
```

修后（**同一个库、仍是 3 超管**）：

```
test result: ok. 15 passed; 0 failed; 0 ignored
```

## 3. 改了哪些文件（全部是测试夹具）

| 文件 | 改什么 | 为什么 |
|------|--------|--------|
| `backend/src/business/flow/test_support.rs`（新增，`#[cfg(test)]`） | `pending_tasks` / `instance_status` / `assert_instance_status` / `super_admin_count` / `approve_all_pending` | 一处实现「把实例批到终态」：逐轮重查 `PENDING` 待办、各自独立事务 `engine::approve`、提交后 `flush_effects`/`flush_drafts`（与 `flow::handler::approve_task` 同序），返回每次动作的出参供调用方跑业务回调 |
| `backend/src/business/erp/stockin_handler_tests_helpers.rs` | `approve_cancel` 批到终态 + 断言实例 `APPROVED`；`reject_cancel` 驳回后断言实例 `REJECTED`；抽出 `cancel_instance_of` | 本卡主因（12 条 `cancel_*` + a4/d4/p16 全部走它） |
| `backend/src/business/erp/voucher.rs` | `cancel_via_approval`（`voucher::tests` 的作废夹具）同上 | 同一类假设：只批一条 → 红字凭证/应付作废断言全红 |
| `backend/src/business/erp/transfer_handler.rs` | `approve_flow_instance` 批到终态 + 断言 `APPROVED`；`submit_creates_pending_todo_for_super_admin` 的「待办恰好 1 条」改成「= 超管人数且不重复派单」 | `n_approve` 也是 `ADMIN` 展开 |
| `backend/src/business/erp/payable_bridge.rs` | `approve_all` 改成逐轮重查（原实现只取一次快照，串行的 `n_dept → n_fin` 第二轮拿不到）+ 断言 `APPROVED` | `payable_pay` 两个节点都是 `ADMIN` |
| `backend/src/business/erp/stockin_handler_tests_p16.rs` | 「发起后审批中心必须有 1 条待办」→「= 超管人数」 | 同上（`p16_submit_moves_to_pending_cancel_with_instance` 在 3 超管时红：3 ≠ 1） |
| `backend/src/business/flow/edge_admin_tests.rs` | `approve_task(task_id)` → `approve_all(instance_id)`；去掉「应当一次通过」的硬断言，改为批到终态后断言 `APPROVED` | `flow_edge_config` 的 `n_cfg` 是 `ROLE + FALLBACK_ADMIN`：角色 0 人时回退**全部超管**，3 超管时 `PENDING` 假红（`change_takes_effect_only_after_approval` / `delete_config_also_goes_through_approval` 两条） |
| `backend/src/business/flow/mod.rs` | 一行 `#[cfg(test)] pub mod test_support;` | 挂载上面的夹具模块 |
| `scripts/verify/erp_clean_db.sh`（新增） | `createdb -O <表 owner>` + 起一次实例迁移 + 打印**库指纹**（迁移版本/条数、超管人数、各流程审批节点策略） | 把「干净库怎么建」钉死，避免再出现「同一提交两种结果」 |

**没有改的**（红线保持）：

- `stockin_bridge::settle` / `flow::settle_guard` 的业务判定（C1 的正确行为）；
- 种子流程 `stock_in_cancel`（`self_approval=FORBID`、3 人复核语义原样，「作废本来就该多人复核」）；
- 任何生产代码路径（`test_support` 整模块 `#[cfg(test)]`，不进二进制）。

## 4. 为什么会出现「同一提交两种结果」（2087/0 vs 2030/57）

两边的「干净库」并不是同一个状态：

- **超管人数不同** ⇒ 会签人数不同 ⇒ 老夹具「只批一条」在两个库上结论相反（§1/§2）。
  超管可能来自别的模块的夹具（`payable_bridge` 的 `p17_ap1/p17_ap2` 就是超管，
  它自己的 `cleanup_fixtures` 会删，但**并行跑时删与建的窗口里**兄弟模块读到的是 3 人）；
- 另外 `cargo test` 默认**按 CPU 数并行**（`-j 1` 只限制构建任务数，不限制测试线程数），
  `backend/tests/testd` 的收口用例 `zz_no_fixture_residue_after_full_suite` 只在
  `--test-threads=1` 下才是「最后执行」，4 线程时会与兄弟用例并发而偶发 1 条红
  （本次实测：4 线程 66 passed / 1 failed，`sys_file=1`；`--test-threads=1` 67 passed / 0 failed）。

结论：**夹具必须对「N 人会签」成立**，并且**跑库必须按 `erp_clean_db.sh` 的指纹口径记录**，
不能只看「我建了一个空库」。

## 5. 干净库建库（本条命令已固化成脚本）

```bash
cd /opt/cloudpivot
RECREATE=1 ERP_DB=cloudpivot_<卡号> bash scripts/verify/erp_clean_db.sh
export ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_<卡号>'
cd backend && cargo test -j 1
```

脚本做的事（缺一不可）：

1. `su - postgres -c "createdb -O <表 owner> <db>"` —— **owner 必须是应用账号**
   （99 上不是 `cloudpivot` 而是 `alun`，owner 错了启动只打一行「数据库迁移失败」，无细节）；
2. `setsid nohup <binary>` 起**一次**实例，让引擎把 `migrations/` 全部应用 + 跑 bootstrap
   （`profile=test` 的安全守卫要求注入 `NIK_MIDDLEWARE_AUTH_JWT_SECRET` 与
   `CLOUDPIVOT_STORAGE_MASTER_KEY`，缺一个就 FATAL、迁移一条都不跑）；
3. 打印库指纹。本次实测：

```
schema_migrations(max|count) = 206|150
超管人数（= assignee_type=ADMIN 节点的会签人数） = 1
flow_edge_config / n_cfg      / ROLE  / self_approval=ALLOW  / AND
payable_cancel  / n_fm        / ROLE  / self_approval=ALLOW  / AND
payable_pay     / n_dept      / ADMIN / self_approval=ALLOW  / AND
payable_pay     / n_fin       / ADMIN / self_approval=ALLOW  / AND
po_approve      / n_dept      / ROLE  / self_approval=ALLOW  / AND
po_approve      / n_pm        / ROLE  / self_approval=ALLOW  / AND
stock_in_cancel / n_review    / ADMIN / self_approval=FORBID / AND
stock_in_post   / n_wh        / ROLE  / self_approval=ALLOW  / AND
stock_transfer  / n_approve   / ADMIN / self_approval=ALLOW  / AND
task_accept     / n_creator   / TASK_CREATOR / self_approval=ALLOW / AND
voucher_reverse / n_fm        / ROLE  / self_approval=ALLOW  / AND
```

> 注：`tests/testd` 的收口用例要求 `--test-threads=1`（见 §4），
> 只看 lib 目标时 `cargo test -j 1` 即可。

## 6. 复跑结果

见同目录 `runs.md`（原始输出的关键行）。
