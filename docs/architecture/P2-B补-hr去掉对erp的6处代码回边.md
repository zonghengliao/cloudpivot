# P2-B 补 · 摘掉 `hr → erp` 的 6 处代码回边（凭证 / 号段 / 期间键 / 审批闸）

> 由来：`docs/planning/20261113-新卡清单.md` §2 的 P2-B 把 `erp ⇄ hr` 记成「hr 提供只读查询 trait，
> 2 卡（P2-35~36）」。P2-35 做完接缝后实测证明**这对边不会因此变成单向**：
> `erp → hr` 归零的同时，`hr → erp` 还有 **6 处代码引用**（凭证过账 / EMP 号段 / 期间键 / 审批闸），
> 两向一合仍是双向。本卡（`t_3442b4fb`）把剩下这 6 处摘掉。
>
> 上游：P2-35 `EmployeeReader`（`57187ca0`）、P2-36 `erp` 切接缝（`0eb80787`）、
> P2-34 `flow::approval_gate` 下移（`5f7f516d`）、P2-37 `task::approval_port`（`82618985`）。

---

## 1. 结论（一屏看完）

| 事实 | 改前（`82618985`） | 改后 | 用什么钉住 |
|---|---|---|---|
| `hr → erp` 模块边 | **存在（代码 6 / 注释 0）** | **不存在** | `python3 scripts/lint/module_cycle_report.py --edge business::hr business::erp` |
| 跨域有向边总数 | 61 条 / 782 个 `.rs` | **60 条** / 783 个 `.rs` | 同上（少的就是这条边） |
| `hr → erp` 回归闸 | 无 | **基线 0**（含测试文件、含注释） | `hr::no_erp_edge_tests::hr_source_has_no_erp_references` |
| `erp → hr` | 存在（代码 3，全是 `employee_reader` 接缝） | 不变（**允许**：消费方总得调接缝） | `hr::employee_reader_tests::erp_only_depends_on_the_employee_seam` |
| 记账口径 | HR 直调凭证域 6 个入口 | 只认 `shared::voucher_port`，实现仍在凭证域（纯委派） | `erp/voucher/voucher_port_impl.rs` 的等价性表 + 真库用例 |
| 取号 / 期间键 / 审批闸 | 3 处直调 ERP | 基座（`shared::seq` / `shared::period_key`）+ 审批域（`flow::approval_gate`） | 同上 + `no_erp_edge_tests::the_neutral_homes_still_exist` |
| 失败语义 | 过账/红冲失败 → 整笔回滚，单据可重试 | **完全不变**（见 §3 逐条） | `hr::salary_bridge_db_tests` 的 11 条红灯用例 |

**一句话**：HR 需要的四样东西（记账能力 / 取号引擎 / 期间键 / 审批闸）全部离开了 ERP ——
两样是**纯函数或通用件**（下移 `shared`），一样是**能力接口**（定义在 `shared`、由 ERP 实现并注册），
一样**本来就在审批域**（`erp::approval_gate` 只是 `flow::approval_gate` 的再导出，改 import 即可）。
记账/取号的**口径一份没变**，变的只是「谁依赖谁」。

---

## 2. 六处逐一：改前 → 改后 + 等价性

### 2.1 `hr/salary_bridge.rs`（过账 + 红冲两条链路，2 行 import 背后是 7 个调用点）

| | 改前 | 改后 |
|---|---|---|
| import | `use crate::business::erp::voucher::voucher_multi::{self, ExplicitLine, ExplicitVoucherInput};`<br>`use crate::business::erp::voucher::{self, AccountRef};` | `use crate::business::shared::voucher_port::{self, AccountPick, ExplicitVoucherReq, ReversalSpec, VoucherBrief, VoucherLine};`<br>`use crate::business::shared::period_key::today_cst;` |

| 调用点 | 改前 | 改后 | 等价性 |
|---|---|---|---|
| `dept_aware_accounts` | HR 里自己写 `SELECT account_code FROM biz_account … LIKE '%DEPT%'` | `voucher_port::dept_aware_account_codes(tx)` | **SQL 逐字搬进凭证域**（`erp/voucher/voucher_port_impl.rs::DEPT_AWARE_ACCOUNTS_SQL`）：软删 / `status = 1` / `upper(coalesce(...)) LIKE '%DEPT%'` / 零参数，一字未改。顺带把「HR 认 ERP 表名」这条**依赖报告看不见的表级直连**也消掉了 |
| `bank_account` | `voucher_multi::rule_accounts` → `.credit`，缺失回落 `voucher_multi::leaf_required(1002)` | `voucher_port::rule_credit_account` → `voucher_port::leaf_account` | 同一对函数的两级回落（规则 → 按编码取末级），`Ok(None)` / `Err` 的两种降级分支与日志文案都保留 |
| `resolve_lines` | `voucher_multi::leaf_required` + `ExplicitLine::debit/credit` | `voucher_port::leaf_account` + `VoucherLine { debit, credit }` | 借贷单向：`line.debit > 0` ⇒ `(debit, 0)`，否则 `(0, credit)` —— 与 `ExplicitLine::debit/::credit` 同口径；`aux_dept_id` 原样带过；科目缓存（同编码只解析一次）保留 |
| `post_one` | `voucher_multi::post_explicit_lines` | `voucher_port::post_explicit` | 实现里就是 `post_explicit_lines` 本身（`AccountPick` ↔ `AccountRef` 三字段一一对应，`ExplicitVoucherReq` ↔ `ExplicitVoucherInput` 九项一一对应） |
| `voucher_no` | `voucher::find_voucher(...).voucher_no` | `voucher_port::voucher_no` | 实现里是一次 `find_voucher`；「查不到 → 500 + 联系人话」的文案留在 HR |
| `lock_original` | `voucher::find_voucher_locked` → `voucher::VoucherRow` | `voucher_port::lock_voucher` → `VoucherBrief{id, voucher_no, source_type, source_id}` | 只取红冲核对要用的四列；「来源不符 → 409」的文案与判据（`source_type` + `source_id == 单据 id`）一字未改 |
| `reverse_one` | `voucher::reverse_posted(tx, &row, ReversalInput{…}, op, now)` | `voucher_port::reverse_posted(tx, id, ReversalSpec{…}, op, now)` | 实现里先 `find_voucher_locked`（**同一事务内的二次 `FOR UPDATE` 是幂等的**，不会重复取锁）+ `reverse_posted`；`DbError::InvalidValue → 409 + HR_SALARY_RULE_BLOCKED` 的错误翻译留在 HR，未变 |
| `voucher::today_cst(now)` ×2（发放凭证日期 / 红字凭证日期） | ERP 的周期键 | `shared::period_key::today_cst`（`erp::voucher::today_cst` 现在反过来委托它） | 同一份实现：`period_key_of` + `parse("%Y%m%d")`，失败回落 `now.date_naive()` |

**为什么接缝定在 `shared` 而不是 `erp`**：能力是「写命令」，若 trait 放 `erp`，HR 仍要 import `erp`
（边还在，等于没解）。放进 `shared` 后，将来别的域要出凭证也走同一条接缝，不必各自 import 一次 ERP；
`shared` 自己的「不许反向依赖业务域」纪律由 `shared/layering_tests` + 新增的
`shared::voucher_port::tests::seam_source_is_free_of_business_paths` 兜住。

### 2.2 `hr/leave_handler.rs` / `hr/salary_sheet_handler.rs`（审批闸，各 1 行）

```rust
// 改前
use crate::business::erp::approval_gate::{self, ApprovalGate};
// 改后
use crate::business::flow::approval_gate::{self, ApprovalGate};
```

等价性：`erp::approval_gate` **本来就是对 `flow::approval_gate` 的再导出**
（`pub use crate::business::flow::approval_gate::{ApprovalGate, authorize_missing_flow, …}`，
P2-34 把判据搬进审批域时留下的兼容层）。调用点 `approval_gate::authorize_missing_flow(...)` /
`ApprovalGate::none()` 指向的就是同一份函数与类型 —— **零行为差异**，`cargo check` 即可证明。

### 2.3 `hr/leave_model.rs`（期间键，1 行）

```rust
// 改前
let period_key = crate::business::erp::serial_model::period_key_of(now);
// 改后
let period_key = crate::business::shared::period_key::period_key_of(now);
```

等价性：实现在 `shared/period_key.rs`，**逐字迁自** `erp/serial_model.rs`：

| 函数 | 口径 | 用例 |
|---|---|---|
| `period_key_of` | `(now + 8h).format("%Y%m%d")` | 跨天边界 `15:59:59Z → 20261030` / `16:00:00Z → 20261031` |
| `today_period_key` | `period_key_of(Utc::now())` | 8 位数字 |
| `period_code_of` | `(now + 8h).format("%Y-%m")` | 月末 23:30 记到本月 |
| `today_cst` | 周期键 → `NaiveDate` | 与 `period_key_of` 永远同一天（凭证日期 = 凭证号日期段） |

ERP 侧 `serial_model::period_key_of` / `today_period_key` / `period_code_of` **保留同名函数**转调基座
（10 余处 ERP 调用点一行未改），`voucher::today_cst` 同款。也就是说**口径只有一份实现**，
依赖方向从「业务域 → 业务域」变成「业务域 → 基座」。

### 2.4 `hr/employee_no.rs`（EMP 号段，1 行 + 1 条用例搬迁）

```rust
// 改前
use crate::business::erp::seq;
// 改后
use crate::business::shared::seq;
```

等价性：**取号引擎整体下移** `shared/seq.rs`，`erp/seq.rs` 变成「引擎再导出 + 本域六张主档的查重表」：

| 层 | 改前 | 改后 |
|---|---|---|
| 读规则 / 推进流水 / 撞号顺延 / 预览（`load_rule` / `bump_master_no` / `next_master_code*` / `next_master_code_free_with` / `peek_master_code_with`） | `erp::seq` | **`shared::seq`**（逐字搬，SQL 与错误文案一字未改） |
| 手工编码解析（`manual_code` / `validate_manual_code` / `resolve_manual_code` / `format_master_code`） | `erp::seq` | **`shared::seq`**（纯函数，7 个主数据入口共用一份口径） |
| 主数据号段清单（`MASTER_CODE_TYPES`，7 类含 `EMP`） | `erp::seq` | **`shared::seq`**（「是不是主数据号段」是全公司口径，与表属主无关） |
| 六张 **ERP 主档**的查重 SQL + `master_code_taken` / `next_master_code_free` / `peek_master_code`（自动选表） | `erp::seq` | **仍在 `erp::seq`**（表名 / 列名 / 软删口径归表属主） |
| 员工主档的查重 SQL（`EMP_EXISTS_SQL`） | `hr/employee_no.rs`（P2-36 已自持） | 不变 |

ERP 侧 40 余处 `seq::xxx` 调用点**一行都没改**（`pub use` 再导出）。HR 侧只用到通用件
（`next_master_code_free_with` / `peek_master_code_with` / `FREE_CODE_MAX_ATTEMPTS` /
`is_master_code_type` / `format_master_code`），因此 `erp` 域（含它的查重表）完全从 HR 视野里消失。

**用例搬迁**：`employee_no::tests::emp_is_registered_as_master_code` 原先断言
「`EMP` 不在 ERP 的查重表里」—— 那句断言要 import ERP 的符号，本身就是一条回边。现在这条断言
留在 ERP 自己的 `seq::tests::t02_exists_sql_covers_every_erp_owned_master_type` 里（它一直在那儿），
HR 侧只钉「EMP 是主数据号段」+「查重 SQL 在本域」（`emp_exists_sql_matches_the_owner_key`）。

---

## 3. 过账 / 红冲的**失败补偿路径逐条**（AGENTS 约定 11）

本卡的改造**没有移动事务边界**：接缝的每个写方法都收 `&Transaction`，仍然在 HR 开的那笔事务里执行。

```text
BEGIN                                    ← hr::salary_bridge::post_salary
  ① lock_sheet（FOR UPDATE）+ 状态闸 APPROVED
  ② 考勤期间必须 LOCKED
  ③ 明细恒等式复算（A6 在线版）
  ④ dept_aware_account_codes / leaf_account / rule_credit_account  ← 接缝（只读）
  ⑤ plan_accrual / plan_payment（纯函数，净额法）
  ⑥ post_explicit(计提) / post_explicit(发放)                      ← 接缝（写）
  ⑦ voucher_no ×2                                                  ← 接缝（只读）
  ⑧ state_machine::transition(APPROVED → POSTED)（写 biz_status_log）
  ⑨ model::mark_posted（CAS 回写两张凭证 id/no）
COMMIT
```

| 失败点 | 谁会失败 | 结果 | 单据状态 | 用户下一步 |
|---|---|---|---|---|
| ①–③ | 单据不存在 / 状态不对 / 考勤未锁定 / 明细对不上 | `Err` → `ROLLBACK` | 仍是 `APPROVED` / 原状态 | 按提示改数据后重新过账（按钮还在） |
| ④ 接缝**只读**方法 | 科目缺失 / 非末级 / 数据库错 | `Err` → `ROLLBACK` | 仍是 `APPROVED` | 补科目 / 修配置后重试 |
| ④ 接缝**未安装** | 启动链漏步（编程错误） | `Err(InvalidValue("凭证写入接缝未安装…"))` → 500 | 仍是 `APPROVED` | 不产生半成品；`bootstrap` 的 `register_voucher_port` 在 prod 会 `exit(1)`，请求期不该出现 |
| ⑥ 第一张凭证 | 取号失败 / 借贷不平 / 期间已结账 | `Err` → `ROLLBACK` | 仍是 `APPROVED` | **一张凭证都不留**（不是「凭证生成了、单据没动」） |
| ⑥ 第二张凭证 | 同上 | `Err` → `ROLLBACK` | 仍是 `APPROVED` | 第一张一起消失（同事务） |
| ⑦ | 凭证刚落库查不到 | `Err(500 + 人话)` → `ROLLBACK` | 仍是 `APPROVED` | 重试；若稳定复现说明库异常 |
| ⑧ / ⑨ | 状态机拒绝 / CAS 命中 0 行 | `Err` → `ROLLBACK` | 仍是 `APPROVED` | 刷新后重试 |
| **COMMIT 之后** | `post_committed` 只**读回**单据，不写库、不发通知、不调外部服务 | 无失败面 | `POSTED` | —— |

红冲（`reverse_salary`）同构：锁单 → 状态闸 `POSTED` → 两张原凭证各 `lock_voucher`（核对来源）→
`reverse_posted` ×2 → 状态机 `POSTED → REVERSED`（必填原因）→ CAS 回写。
任一步失败整笔回滚，两张红字凭证一起消失，单据仍是 `POSTED`，「红冲」按钮可再点。

**新增的失败形态只有一种**：接缝未安装 → 500（见上表）。它**不是**降级路径：
写路径未安装时**绝不返回成功**（对比只读接缝 `hr::employee_reader` 的「查无」降级）。
理由：「凭证没记」不是业务状态 —— 返回 `Ok` 会让单据带着「已过账」而没有账。

---

## 4. 门禁与用例（真跑，非声明）

### 4.1 静态 / 编译

| 门 | 命令 | 结果 |
|---|---|---|
| 类型检查 | `cargo check --lib` | 零 warning（13.7s 增量） |
| 格式 | `cargo fmt --check` | `FMT_OK` |
| Lint | `cargo clippy --all-targets -- -D warnings` | 零告警（2m10s） |
| 闸②（数据范围） | `python3 scripts/lint/scope_gate.py --root .` | PASS（生产面 626 / 测试面跳过 **157** 条，清单逐条一致；新增 `business/hr/no_erp_edge_tests.rs` 已登记） |
| 模块边 | `module_cycle_report.py --edge business::hr business::erp` | **不存在** |

改前 / 改后对照（同一工具、同一命令）：

```console
# 改前（82618985）
边 business::hr → business::erp：存在（代码 6 / 注释 0）
  [code] business/hr/employee_no.rs:79          （EMP 号段 → erp::seq）
  [code] business/hr/leave_handler.rs:75        （审批闸 → erp::approval_gate）
  [code] business/hr/leave_model.rs:498         （期间键 → erp::serial_model）
  [code] business/hr/salary_bridge.rs:93/94     （凭证过账 → erp::voucher / voucher_multi）
  [code] business/hr/salary_sheet_handler.rs:56 （审批闸 → erp::approval_gate）

# 改后
边 business::hr → business::erp：**不存在**（该域内无任何 crate:: 引用指向 business::erp）
扫描面：783 个 .rs / 跨域有向边 60 条（改前 782 / 61）

# 反方向（消费接缝，允许且必须存在）
边 business::erp → business::hr：存在（代码 3 / 注释 0）
  [code] business/erp/cost/cost_labor_db_tests.rs:65
  [code] business/erp/cost/cost_labor_import.rs:1153
  [code] business/erp/cost/cost_labor_import.rs:67
```

### 4.2 真库用例（演练库 `cp_p2b_hr_drill_v1` @ `172.18.10.145`）

| 范围 | 结果 | 耗时 |
|---|---|---|
| `business::hr::` | **695 passed / 0 failed** | 213.8s |
| `business::flow::` | **487 passed / 0 failed** | 54.8s |
| `business::erp::voucher::`（含新增 `voucher_port_impl` 3 条） | **97 passed / 0 failed** | 94.4s |
| `business::erp::seq::`（引擎搬家后的 8 条真库用例） | **8 passed / 0 failed** | 2.9s |
| `business::shared::`（含新增 `period_key` 6 / `seq` 4 / `voucher_port` 4） | **163 passed / 0 failed** | 4.4s |
| 全量离线 `env -u ERP_DB_URL … cargo test --lib -- --test-threads=1` | **5680 passed / 0 failed** | 8.6s |

过账 / 红冲**真的走通了接缝**（不是「编译过就算」）：

```console
test business::hr::salary_bridge_db_tests::db::post_creates_two_balanced_vouchers_with_net_method ... ok
test business::hr::salary_bridge_db_tests::db::post_requires_locked_attendance_period ... ok
test business::hr::salary_bridge_db_tests::db::post_requires_approved_status_and_leaves_nothing ... ok
test business::hr::salary_bridge_db_tests::db::post_is_rejected_twice_and_leaves_no_extra_vouchers ... ok
test business::hr::salary_bridge_db_tests::db::reverse_creates_two_red_vouchers_for_accrual_and_payment ... ok
test business::hr::salary_bridge_db_tests::db::reverse_without_reason_is_rejected_and_leaves_ledger_untouched ... ok
test business::hr::salary_bridge_db_tests::db::reversed_sheet_frees_the_period_for_a_new_sheet ... ok
```

### 4.3 演练库怎么造的（如实记录两次返工）

1. `cp_p2b_hr_drill_v1` = `TEMPLATE cloudpivot_test` 克隆（旧卡惯用做法）。
   ⇒ `business::hr::` 695/0、`business::flow::` 487/0 全绿。
2. 但 `erp::voucher::tests::account_mapping_uses_voucher_rule` **红了**。取证：
   `select count(*) from biz_voucher_rule` 在 `cloudpivot_test` 与其克隆里都是 **0**
   —— 模板库没有凭证规则种子（V0149/V0384），与本次改动无关。
   ⇒ 另建空库 `cp_p2b_fresh_drill` 跑一次启动把 **336 条迁移**应用完（`biz_voucher_rule` 9 条、
   `biz_salary_item` 17 条、`biz_account` 83 条、`biz_seq(EMP)` 1 条），
   `erp::voucher::` 在该库 **97/0** —— 证明上面那条红是模板数据缺种子，不是代码。
3. 把 9 条规则补进克隆库后，`erp::voucher::` 在克隆库同样 **97/0**，`hr::` 695/0、`flow::` 487/0
   也复测通过（此时薪资发放走的是**规则命中**分支而非 `1002` 回落分支，两条分支都覆盖到了）。

> 空库 `cp_p2b_fresh_drill` 上 `hr::` 3 条 / `flow::` 17 条红，是那些用例依赖演示种子
> （超管 / 角色 / 菜单授权）所致 —— 该库只跑到迁移、没跑 seed。数字取自克隆库，不用它下结论。

---

## 5. 改了什么文件

| 文件 | 动作 |
|---|---|
| `backend/src/business/shared/period_key.rs` | **新增**：周期键 / 期间码 / CST 日期（纯函数，6 条单测） |
| `backend/src/business/shared/seq.rs` | **新增**：取号引擎（只发号、不认表）+ 4 条单测 |
| `backend/src/business/shared/voucher_port.rs` | **新增**：`VoucherWriter` 接缝 + 注册表 + 未安装即大声失败 + 4 条纪律用例 |
| `backend/src/business/shared/mod.rs` | 挂 3 个新模块 |
| `backend/src/business/erp/voucher/voucher_port_impl.rs` | **新增**：接缝的**唯一实现**（7 个方法纯委派 + 3 条单测） |
| `backend/src/business/erp/voucher/mod.rs` | 挂 `voucher_port_impl`；`today_cst` 改为委托基座 |
| `backend/src/business/erp/seq.rs` | 引擎搬走，变「再导出 + 本域查重表 + 4 个便捷入口」（-364 行） |
| `backend/src/business/erp/serial_model.rs` | `period_key_of` / `today_period_key` / `period_code_of` / `CST_OFFSET_HOURS` 改为委托基座 |
| `backend/src/business/hr/salary_bridge.rs` | 7 个调用点改走接缝；`biz_account` SQL 搬去凭证域 |
| `backend/src/business/hr/leave_handler.rs`、`salary_sheet_handler.rs` | 审批闸改指 `flow::approval_gate` |
| `backend/src/business/hr/leave_model.rs` | 期间键改指 `shared::period_key` |
| `backend/src/business/hr/employee_no.rs` | 号段改指 `shared::seq`；删掉一条要 import ERP 的断言（等价断言在 ERP 侧） |
| `backend/src/business/hr/salary_bridge_db_tests.rs` | 夹具初始化时装接缝（经 `bootstrap`，不 import erp） |
| `backend/src/business/hr/no_erp_edge_tests.rs` | **新增**：`hr → erp` 基线 0 的回归闸 + 中立落点存在性 + 实现位置 |
| `backend/src/business/hr/mod.rs` | 挂新用例模块；凭证口径文档改写 |
| `backend/src/bootstrap.rs` | `register_voucher_port()`（prod 失败 `exit(1)`）+ `#[cfg(test)] install_voucher_port_for_tests()` |
| `scripts/lint/scope_gate.py` | `SKIP_MANIFEST` 登记 1 个新测试面文件 |

---

## 6. 边界（本卡**没做**的事）

- **不合并** `flow` / `task` / `erp` 各自的 `CST_OFFSET_HOURS` 私有常量（`flow/task_node.rs`、
  `task/recurring.rs` 各有一份 `const = 8`）。它们不是模块回边，改动会牵动两张别人的卡；
  要做就单开卡，落点已是现成的 `shared::period_key::CST_OFFSET_HOURS`。
- **不动** `hr → flow` / `hr → impexp` / `hr → masking` / `hr → changelog` 这几条边：
  本卡只对 `erp` 这一对，其他边的解环是 P2-B 后续卡（`hr → flow` 从 5 涨到 7，涨的 2 处就是
  审批闸改指 `flow`，方向正确）。
- **不给**只读接缝加「写」能力，也不给写接缝加只读方法：`EmployeeReader` 是读、
  `VoucherWriter` 是写，混在一起会让「谁在读别人的表」重新变模糊。
- 演练库 `cp_p2b_hr_drill_v1` / `cp_p2b_fresh_drill` **留在 145**（人工清理入口不可用，
   unattended 会话不执行 DROP DATABASE）。两者都在演练主机且库名含 `drill`，
   `hr::fixture_hygiene` 的库名硬闸不拦。

---

## 7. 复现

```bash
# ① 模块边（本卡的验收口径）
python3 scripts/lint/module_cycle_report.py --edge business::hr business::erp   # 期望「不存在」
python3 scripts/lint/module_cycle_report.py --edge business::erp business::hr   # 期望存在（代码 3）

# ② 编译 / 静态
cd backend && cargo check --lib && cargo fmt --check && cargo clippy --all-targets -- -D warnings
python3 ../scripts/lint/scope_gate.py --root ..   # 在项目根跑

# ③ 真库（库名必须含 drill/test）
ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_p2b_hr_drill_v1 \
  cargo test --lib business::hr::   -- --test-threads=1
ERP_DB_URL=… cargo test --lib business::flow::        -- --test-threads=1
ERP_DB_URL=… cargo test --lib business::erp::voucher:: -- --test-threads=1
ERP_DB_URL=… cargo test --lib business::erp::seq::     -- --test-threads=1

# ④ 全量离线
env -u ERP_DB_URL cargo test --lib -- --test-threads=1
```
