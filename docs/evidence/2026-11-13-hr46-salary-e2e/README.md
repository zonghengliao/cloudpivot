# HR-46 · 薪资链路 10 步 E2E 脚本

日期：2026-11-13 · 卡片：`t_f9d58f11` · 交付：`scripts/verify/hr/hr_salary_e2e.sh`（+`hr_e2e_xlsx.py`）
对应设计：`docs/architecture/ARCH-9-人力资源模块设计.md` §8.1（HR-46 行）、§4.3（审批接入）、
§4.6（工资凭证勾稽）、§4.8.2（红冲）、§4.9（10 条巡检断言）、§5.2（API #1~#46）

## 一句话

一条命令（`bash scripts/verify/hr/hr_salary_e2e.sh --fresh`）在 **145 建演练库 → 起隔离后端 →
走完 10 步真 HTTP 链路（建员工 → 考勤期间 → 导入考勤 → 请假 → 锁期间 → 试算 → 建单 → 审批 →
过账 → 红冲 → 对账）→ 自己 DROP 演练库**，每步都有断言（本次 **98 项**），失败打印
「哪个请求 / 返回什么 / 服务端日志尾 / 夹具 id / 库地址」，并能跑出**有牙**的红。

## 十步与断言（实测输出见 `01_e2e_green.out`）

| # | 步骤 | 关键断言（摘要） |
|:--:|---|---|
| 1 | 建员工（档案 + 劳动合同 + 薪资标准） | 工号 `EMP-` 取号 / 合同 `ACTIVE` / 标准 3 项落库 |
| 2 | 建考勤期间（当月） | `DRAFT` / 区间=自然月 / 未锁定 |
| 3 | 导入考勤（真 xlsx → impexp 预检 → 确认） | `total=1 / error=0` / 入库 1 行 / 台账 21 天 + 10 加班小时 / **期间头汇总被重算**（不调 recalc 接口） |
| 4 | 请假（建单 → 提交 → 直属上级审批） | `DRAFT→SUBMITTED→APPROVED` / 实例终态 `APPROVED`（§4.9 A10） |
| 5 | 锁定考勤期间 | `DRAFT→SUBMITTED→LOCKED` / 库内 `locked_at` 已写 |
| 6 | 试算（`dry_run`） | 应发 15834.48（含公式项 `OT_WD=1034.48`、`ATT_BONUS=200`）/ 实发 13534.48 / 计提 19034.48 / **库里零行** |
| 7 | 建单（落库） | `CALCULATED` / 单号 `SAL-` / 明细 1 行 + 明细项 15 项 / 应发实发与试算一致 |
| 8 | 审批（提交 → 人事经理 → 财务会签） | `SUBMITTED→APPROVED` / 实例 `APPROVED` / **审批通过不出凭证**（§4.3）/ 不占凭证号段 |
| 9 | 过账 + 红冲 | 两张蓝字（`PAYROLL_ACCRUAL`/`PAYROLL_PAYMENT`，借贷平衡，借 `5602.10`=应发、贷 `1002`=实发，借方带部门辅助核算）→ 两张红字（`is_red=1`、`reverse_of`、`reversed_by`、**逐科目借贷互换**）→ `REVERSED` → **二次红冲 409** |
| 10 | 对账 | 4 张凭证**逐科目净额 0**（10 个工资科目）/ §4.9 A6、A7 恒等式 / 状态日志轨迹 / **`hr_invariants.sh` 12 行全绿（blocker_fail=0）** |

## 为什么是「真 HTTP + 真库 + 真实例」

直调 handler 函数只能证明业务编排，证明不了这四件事，而它们恰是线上故障的来源：

1. 路由真的注册上了、`#[nik::permission]` 权限码真的能过（拼错只在线上 403）；
2. JSON 字段名与前端/脚本消费的形状一致（DTO 改名不会编译失败）；
3. 审批真走**流程引擎**：本脚本按库里真实的 `biz_flow_task.assignee_id` 逐个登录投票
   （不假设节点数 / 会不会签），薪资流的「人事经理 → 财务会签（COUNTERSIGN ALL）」两签
   都真的投了；
4. 过账写的是 `biz_voucher` 真凭证 —— 第 10 步直接拿 SQL 对科目、金额、红冲指针。

## 演练库纪律（卡面 2026-11-12 强制）

* 演练库**只建在 145**（`HR46_DBHOST` 默认 `172.18.10.145`），99 上不建任何库；
* 库名必须含 `drill`/`e2e`/`test`，**先于任何写操作**硬校验（不匹配 `rc=2`）；
* `--fresh` 自己建库、跑完自己 DROP（失败也 DROP —— `trap cleanup EXIT`）；
* 给了 `HR_DB_URL` 则只跑不清（`OWN_DB` 守卫，绝不删别人的库）；
* 后端端口必须在隔离段 `18000-18999`（默认 18146），与 `e2e_isolated_instance.sh` 同一判据。

实测：本卡 3 次跑库（主跑 / teeth 演练 / 护栏演练后复核）后，145 上 `hr46%` 计数 = **0**，
99 上 `hr46%` 计数 = **0**（见 `02_guards_and_cleanup.out`）。

## 「有牙」证明（`03_teeth.out`）

把 `EXPECT_GROSS` 故意改成 `99999.00` 再跑一遍（临时副本，跑完即删，未入库）：

* 3 条断言报红：第 6 步「应发合计」、第 7 步「单头应发 = 试算应发」、第 9 步「计提凭证借方 5602.10」；
* 每次报红后面都跟着定位块：请求（`POST /api/business/hr/salary/calc`）、请求体、HTTP 回执、
  服务端日志尾、夹具 id、库地址；
* 第 6 步标 `❌`，末尾汇总 `失败 3 项`，**退出码 1**；演练库仍被 DROP（清理不受失败影响）。

## 复现

```bash
# 一条命令（口令由脚本自己经 ssh 从 145 的 /etc/cloudpivot/cloudpivot-dev.env 读）
bash scripts/verify/hr/hr_salary_e2e.sh --fresh

# 用现成演练库（不删）
HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr46_drill_x \
  bash scripts/verify/hr/hr_salary_e2e.sh

# 失败排查：留实例与库
KEEP=1 KEEP_DB=1 bash scripts/verify/hr/hr_salary_e2e.sh --fresh
```

## 门禁与证据

| 项 | 结果 | 文件 |
|---|---|---|
| 主跑（空库回放 300+ 迁移 + 10 步） | **98 项断言通过 / 0 失败，rc=0**，演练库已 DROP | `01_e2e_green.out` |
| 护栏与清理复核 | 非演练库名 `rc=2` 拒绝；145/99 上 `hr46%` 库计数均 0 | `02_guards_and_cleanup.out` |
| 有牙演练（改坏期望值） | 3 条断言报红 + 定位块 + `rc=1`，库仍 DROP | `03_teeth.out` |
| 契约测试 `cargo test --lib business::hr::hr_e2e_contract_tests` | **8 passed / 0 failed** | `04_contract_tests.out` |
| `cargo fmt` / `clippy --all-targets -D warnings` / `pnpm typecheck` | 见文件 | `05_gates.out` |

## 已知边界（诚实说明）

1. **请假不改薪资数字**：设计 §4.3 明确「请假审批不改考勤（考勤由导入/人工体现）」，而卡面
   把「导入考勤」排在「请假」之前 —— 故本卡用**带薪年假**（`paid=1`，不进扣款口径），
   请假环节验的是审批流 / 状态机 / 实例终态。**无薪假扣款**那条口径由
   `hr.attendance.require_leave_match`（导入时必须已有已批无薪假）管，与本链路顺序互斥，
   不在本卡范围。
2. **只造 1 个员工**：设计 §8.2 说「10 步 E2E 要造 42 人规模数据」，但本卡验收是
   「一条命令跑完 10 步 + 每步有断言 + 失败能定位」。42 人规模会把单次跑时从 ~2 分钟拉到
   十几分钟（迁移回放占大头），且不增加任何**链路**覆盖（净额法汇总 / 部门辅助核算的多部门
   分支由 HR-39 的真库用例覆盖）。本脚本把夹具金额集中成一组变量
   （`BASE_AMT`/`SI_PERSONAL`…），要扩规模只改头部常量。
3. **`hr.attendance` 没有 HR 专属路由**：设计 §5.2 #23 的 `POST /business/hr/attendance/import`
   在实现上是 impexp 通用三段（`upload` → 预检 → `confirm`），脚本走的就是这条真通道
   （不是 SQL 直插台账）。
4. **审批人是夹具账号**：`hr46_approver` 同时持 `HR_MANAGER` / `FINANCE_MANAGER` /
   `FINANCE_DIRECTOR`（三层去重后会签候选人只有 1 个），只存在于演练库；密码哈希抄自 admin，
   不引入任何新的密文逻辑。
