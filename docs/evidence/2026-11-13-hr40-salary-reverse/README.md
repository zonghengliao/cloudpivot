# HR-40 · 薪资单红冲（两张红字凭证 + 状态机 + 原因必填）

日期：2026-11-13 · 卡片：`t_b17e6c44` · 提交：`83a4cfab`（代码 + 迁移 + 文档）、本目录为随行证据提交
对应设计：`docs/architecture/ARCH-9-人力资源模块设计.md` §4.8.2（红冲 / 重发）、§5.2 #44、§4.9 A8

## 一句话

`POST /business/hr/salary/{id}/reverse` 把「计提 + 发放」**两张凭证各冲一张红字**（`PZR-` 号段、
借贷互换、`reverse_of` 指原凭证、原凭证回写 `reversed_by`），单据 `POSTED → REVERSED` 终态并记
`reversal_voucher_id` + `reverse_reason`；**原因必填**（DTO 与状态机两道闸）；迁移 `V0397` 让
红冲单**不再占期间位**，所以「红冲后可重算」——同期间能直接重新试算建单。

## 落地内容

| 层 | 文件 | 说明 |
|---|---|---|
| 编排 | `backend/src/business/hr/salary_bridge.rs`（+364） | `reverse_salary` / `reverse_in_tx` / `lock_original` / `reverse_one` / `reverse_committed` / `reverse_blocked`；逐张走 `erp/voucher.rs::reverse_posted`（**不手写红冲分录**） |
| 路由 | `salary_sheet_handler.rs`（+36） | #44 `POST /business/hr/salary/{id}/reverse`，权限 `business:hr:salary:reverse` |
| 入参 | `salary_sheet_dto.rs`（+129） | `SalaryReverseReq` + `parse_reverse_reason`（trim 后 ≥5 字、≤255 字 = `reverse_reason` 列宽） |
| 回写 | `salary_sheet_model.rs`（+45） | `SHEET_REVERSED_SQL` + `mark_reversed`（一条 CAS 写齐状态 / 指针 / 原因 / 时间戳） |
| 迁移 | `V0397__hr_salary_period_after_reverse.{up,down}.sql` | `uk_biz_sal_sheet_period` 谓词 → `deleted_at IS NULL AND status <> 'REVERSED'`；up 自带四条自检（谓词含 EXCLUDED / 软删条件 / 红冲单不占位 / 有效单仍唯一），down 有「同期间多张单」RAISE 护栏 |
| 前置检查 | `salary_compute.rs`（+16） | `existing_sheet_no` 与 `duplicate_sheet_error` 同步排除 `REVERSED`（否则建单前置就 409，迁移白改） |
| 文档 | `ARCH-9-…设计.md`（+16−1） | §4.8.2 落地记录（含**具名偏差**：红冲单不占期间位）+ §8.1 HR-40 标记已落地 |

## 验收对照（卡面三条）

| # | 卡面验收 | 证据 |
|:--:|---|---|
| ① | **两张都红冲** | 真库 `reverse_creates_two_red_vouchers_for_accrual_and_payment`：按 `reverse_of` 反查两张红字各自存在、`is_red=1`、`voucher_no` 以 `PZR-` 开头、`source_type` 分别是 `PAYROLL_ACCRUAL`/`PAYROLL_PAYMENT`、`source_no` 仍指向薪资单、`remark` = 原因；原凭证 `reversed_by` 各自回写；**逐科目对账借贷互换**（红字的借 = 原凭证的贷，反之亦然）且每行金额恒正；一张单共 4 张凭证（2 蓝 + 2 红）；状态日志一条 `POSTED → REVERSED` 且带原因 |
| ② | **原因必填** | DTO 单测 3 条（缺 / 空 / 全空白 → 400 + 「reason 必填」；<5 字、>255 字各 400 并报「收到 N 个」；trim + 边界 5 字放行）+ bridge 单测「空原因不触库就 400」+ 状态机必填原因边断言 + 真库 `reverse_without_reason_is_rejected_and_leaves_ledger_untouched`（空白原因 400，单据仍 `POSTED`、无指针、**一张红字凭证都不留**） |
| ③ | **红冲后可重算** | 迁移 `V0397` + `existing_sheet_no` 排除 `REVERSED`；真库 `reversed_sheet_frees_the_period_for_a_new_sheet`：红冲后同期间 `persist` 建新单成功（新 id / 新单号）、应发口径与红冲前一致、旧单仍 `REVERSED` 且 4 张凭证留存，**新单可继续推到 `APPROVED` 并过账**（闭环） |

## 具名偏差（需要架构/测试确认的一条）

设计 §4.8.2 原文：「红冲后同期间**不能再开新单**（`uk_biz_salary_sheet_period`）；重发要么把红冲单
软删再开新单，要么用补差单」。

本卡按卡面验收③把唯一键**放宽**为「红冲单不占期间位」，理由：

1. `REVERSED` 是终态（设计 §4.2.4，不回 `CALCULATED`）；
2. 设计 §5.2 的接口清单里**没有 DELETE**，前端也没有删单入口；
3. 唯一键只要仍把 `REVERSED` 算占位，客户 11 月工资算错并红冲后，**当月工资就再也发不出去**，
   只能让研发改库 —— 正撞 `AGENTS.md` 约定 11「用户必须能从界面继续操作」的判据。

同款先例：成本期间 `uk_biz_cost_period`（`V0373`）早已排除 `REVERSED`/`CANCELLED`。
放宽后「补差单」「软删重开」两条老路都仍然可用（没有堵）；回滚 `V0397.down.sql` 若遇同期间
多张未删除单会 `RAISE EXCEPTION` 并给出可操作路径，不会硬建索引报 PG 原文。

## 门禁（原始输出见 `03_gates.out`）

| 项 | 结果 |
|---|---|
| 145 演练库真库用例（`hr40_drill`，回放 318 条迁移含 V0397） | **11 passed / 0 failed**（HR-39 七条 + HR-40 四条），残留复核 3 项全 0，库已 DROP（`pg_database=0`） |
| `cargo test --lib business::hr::` | **567 passed / 0 failed** |
| `cargo fmt -- --check` | 0 diff |
| `cargo clippy --all-targets -- -D warnings` | 0 warning / 0 error |
| `scripts/check.sh --fast`（L0+L1） | **EXIT=0**（含迁移授权段角色码闸扫 318 个 `.up.sql`） |
| 前端 `pnpm typecheck` / `vite build` | EXIT=0 / EXIT=0（构建到私有 outDir，未覆盖共享 `dist`） |

> 门禁都在 **worktree `/root/hr40-wt` @ `83a4cfab`** 上跑：共享工作区同时挂着邻居卡
> （HR-41 `self_service_*` / HR-42 `salary_export_*`）**未提交**的文件，它们的 `cargo fmt` 差异
> 与本卡无关（本卡 6 个 `.rs` 单独 `rustfmt --check` 全过）。worktree 已 remove。

## 复现

```bash
export PGPASSWORD=$(ssh root@172.18.10.145 \
  'grep "^NIK_DATABASE_PASSWORD" /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-')
bash docs/evidence/2026-11-13-hr40-salary-reverse/01_db_tests.sh   # 建库 → 318 迁移 → 11 条真库 → DROP
```
