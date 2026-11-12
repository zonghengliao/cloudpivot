# HR-39 `salary_bridge` 证据（2026-11-12）

卡：`t_f5ac25b8`（批 4 薪资）。设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md`
§4.3（审批回调）/ §4.6（工资凭证勾稽）/ §5.2 #42 #43。

## 交付物

| 文件 | 内容 |
|---|---|
| `backend/src/business/hr/salary_bridge.rs` | **过账 + 审批回调桥**：净额法汇总（`plan_accrual` / `plan_payment`）+ 两张凭证（`post_explicit_lines`）+ `on_salary_settled` + `guard_self_approval` + `cancel_approval_of` + 15 条单测 |
| `backend/src/business/hr/salary_sheet_handler.rs` | 新增 `POST /business/hr/salary/{id}/submit`（#42，冻 `gross_total_num` 为 JSON number）与 `POST /business/hr/salary/{id}/post`（#43）；3 条新单测（路由契约 / 落地卡 / 条件边表单契约） |
| `backend/src/business/hr/salary_sheet_model.rs` | 4 个状态回写 CAS（`set_status` / `set_submitted` / `mark_posted` / `set_approval_instance`）+ `write_error` 人话翻译 |
| `backend/src/business/flow/handler.rs` | 审批回调注册 4 处（approve / reject / withdraw / terminate）+ 自审批守卫 1 处 |
| `backend/src/business/hr/mod.rs` | `pub mod salary_bridge;` + `#[cfg(test)] mod salary_bridge_db_tests;` |
| `backend/src/business/hr/salary_bridge_db_tests.rs` | 7 条真库用例（无库地址 → SKIP，不假绿） |

**无迁移**：本卡不新增表 / 列，凭证走既有 `biz_voucher` / `biz_voucher_entry`，科目用 HR-02
（V0384）种的 9 个末级科目，来源类型用 HR-06（`doc_kind.rs`）的 `PAYROLL_ACCRUAL` /
`PAYROLL_PAYMENT`。**未修改任何已应用迁移的字节。**

## 验收对照

| 卡面验收 | 证据 |
|---|---|
| 两张凭证借贷平衡 | `01_db_tests.out`：`post_creates_two_balanced_vouchers_with_net_method` 断言 `biz_voucher.total_debit = total_credit`（两张都查），且 `借 5602.10 + 5602.11 + 5602.12 = 贷 2211.01 + 2211.02 + 2211.03` |
| 走 `post_explicit_lines` 不手写分录 | 代码里分录只由 `voucher_multi::post_explicit_lines` 落库（`post_one` 唯一出口），取号 / 平衡 / 期间闸 / 分录落库全部复用 `voucher::post_voucher`；`salary_bridge` 里没有一条 `INSERT INTO biz_voucher*` |
| 净额法汇总正确 | `02_static_tests.out` 的 `pre_deduct_is_netted_into_the_same_expense_line`（8200 − 735.63 → 一行 7464.37）+ `zero_net_group_is_dropped` + `negative_net_is_rejected_with_department_in_message`；真库侧 `借 5602.10 合计 = 单头 gross_total = Σ(earning − pre_deduct)` |

## 环境与命令

演练库建在 **145（172.18.10.145）**，跑完 DROP（99 上不建任何测试库）：

```bash
export PGPASSWORD=$(ssh root@172.18.10.145 \
  'grep "^NIK_DATABASE_PASSWORD" /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-')
bash 01_db_tests.sh          # 建库 → 回放 317 条迁移 → 种夹具 → 跑 7 条真库用例 → DROP
```

被测代码用 **worktree**（`/root/hr39-wt`，从 `a3df266c` 检出）：
共享工作区 `/opt/cloudpivot` 里同时有邻居卡（HR-41 自助三接口）的在途文件，
`mod.rs` 已声明但文件未完成 → 共享目录 `cargo check` 是红的。worktree 只带本卡的 6 个文件，
保证「绿的就是我这一版」。脚本支持 `BACKEND=... CARGO_TARGET_DIR=...` 覆盖。

## 结果

* `01_db_tests.out`：145 `hr39_drill` 回放 **317 条**迁移后跑 7 条真库用例 →
  `7 passed; 0 failed`；前置复核 6 项（9 科目 / 6 个 DEPT 维度 / 17 薪资项 / `hr_salary`
  流程 PUBLISHED / 大额条件边 `form.gross_total_num gte 500000` / 2 条凭证规则）全过；
  残留复核 3 项（凭证 / 薪资单 / 员工）均 0；**演练库已 DROP**（`pg_database` 复查 0）。
* `02_static_tests.out`：`salary_bridge` 单测 15 passed；HR 全模块 556 passed；
  `cargo clippy --all-targets -D warnings` → EXIT 0；`cargo fmt --check` → 0 diff。

## 前端

**零改动**：本卡只有两条后端路由（提交 / 过账），按钮在 HR-43 的薪资单详情页落地。
故 `pnpm typecheck && pnpm build` 对本卡 N/A（与 HR-38 同口径）。

## 已知边界（不属本卡）

* 红冲（两张红字凭证 + `reversal_voucher_id` + `POSTED → REVERSED`）→ **HR-40**；
  `POST /salary/{id}/reverse` 路由仍在 `ROUTE_CONTRACT` 里挂 HR-40。
* 撤回路由（`SUBMITTED → CALCULATED` 的「HR 自己拿回」入口）设计 §5.2 未列接口，
  本卡只提供 `salary_bridge::cancel_approval_of`（终止在跑实例）给将来的撤回 / 作废路径用，
  不擅自加第 47 条接口。
* 巡检脚本 `hr_invariants.sql`（A6/A8 的离线版）→ **HR-45**；本卡在过账闸里做了 A6 的在线版。
