# E2E06-FIX-2 执行报表口径：月度闸同源 + effective 按年度期末判定

卡片：`t_24707809`（E2E-06 演练发现的两条 P3，都是**报表口径**）
环境：145 `cloudpivot-test`（`/opt/cloudpivot`，库 `cloudpivot_dev`，:8033）
提交：`a4e82902`（主体代码）+ `abf619ee`（探针 SQL 修正）+ `2d027284`（证据脚本）
`origin/main` 已推送；145 已 `git pull` + `cargo build` + 重启，`health=200`。

---

## 一、修了什么

### ① 报表主列补「月度口径」（原来只有年度口径）

**改前**：`GET /budget/report?fiscal_year=2027` 的 5602.05 行是
`over=false / available=10500`（年度口径），本月已超 500 只藏在 `months[2027-02].available=-500` 里
→ 财务从主列看不出本月已超，而提交侧月度闸早就标记 `over_budget=1 / over_amount=500`（**口径分叉**）。

**改后**：每行增加

| 字段 | 含义 |
|---|---|
| `month_period` | 该行月度口径落在哪个期间（`yyyy-MM`） |
| `month_limit` / `month_used` | 该期间的月度额度 / 已用 |
| `month_available` | 该期间月度可用（**带符号，负数 = 已超支**） |
| `month_over` | 该行本年度**是否有月份已超支**（与提交侧月度闸同源） |
| `month_over_amount` | 超支额（未超 = `null`，不是 0） |

**取哪个月**：本年度**最紧张的那个月** —— 有超支取"超得最多"的月份；都没超则取"可用最小"的月份。
不是"今天这个月"：报表按 `fiscal_year` 查，而查的年度常常不是当前年度（编下一年预算 / 复盘上一年）。
2026-11 打开 FY2027 的报表，问的必然是"2027 年哪个月超了"；而且 2026-11 根本不在这一年的 12 行分解里。

**同源实现（卡面红线）**：月度闸公式抽成 `budget_guard::MonthGate`
（`over / over_amount / available / available_clamped`），**提交侧 `BudgetVerdict::evaluate` 与报表
`budget_model::month_summary` 调的是同一个方法**，仓库里不存在第二套月度算法。

> 为什么 `available` 有"带符号"和"截零"两版：报表要展示 `-500`（已超 500），
> 提交侧算缺口必须用截零值（`max(0, 额度−已用)`）—— 否则 `本次 100` 会减一个负数，
> 把历史超支再算进本次缺口（缺口变 600，其实本次只超 100）。两种用法都在 `MonthGate` 上写明。

### ② `effective` 改按「该行 fiscal_year 的期末」判定

**改前**：`let now = Utc::now()` → 用"本月月末"当报表时点。
2026-11 查 FY2027 报表 → `effective=false`（"2027 年的预算未生效"），但 2027-02 的单据实际已被它控制
→ 同一行自相矛盾，财务会误判"预算没管上"。

**改后**：`budget_guard::fiscal_year_end(fiscal_year)`（= 该年 12-31，年份窗口与库内
`ck_biz_budget_year: 2000..=2100` 一致）+ `effective_in_year(effective_from, fiscal_year)`：

```
FY2027 预算（effective_from=2026-12-01）→ 报表 fiscal_year=2027 → effective=true   ✅
FY2027 预算（effective_from=2028-06-01）→ 报表 fiscal_year=2027 → effective=false  ✅
FY2023 预算（effective_from=2026-12-01）→ 报表 fiscal_year=2023 → effective=false  ✅（年度早于生效日）
```

控制侧不变：仍按"单据期间月末"逐月判（`period_end_of`）。差别只在粒度 ——
报表按年度整年判，月度粒度由 `MonthGate` 表达。

### ③ 前端（报表页）

- 新增「本月」列：`month_available`（超支标红）+ `month_period` + `超 N`；
- 「预算」列徽标**月度优先**（月度闸才是实际控制的闸门）：
  `本月超 500.00` > `年度超` > `未生效` > `正常`，悬停说明写清是哪个口径
  （原生 `title` 挂在包一层的 `<span>` 上 —— `t-tag` 不透传 `title`，直接写在组件上会静默丢失，实测发现后修的）；
- 汇总卡片分开数：`年度超支 N 个 · 本月超 M 个`（`summarizeReport.monthOverCount`）；
- 执行率进度条：年度或月度任一超支都标红。

**真实浏览器实测**（不是只看编译过）：145 后端 + 99 上 dev server，admin 登录后打开报表页：

```
FY2027  5602.05 → 本月 -500.00（红）/ 2027-02 · 超 500.00    预算 = 本月超 500.00（红）
FY2027  5602.02 → 本月 9,320.00 / 2027-02                    预算 = 正常
汇总卡片 → 年度超支 0 个 · 本月超 1 个   ← 验收 ②：年度没超、月度超，主列就能看出来
FY2023  5602.02 → 未生效（黄）           ← 验收 ④
```
截图见 `ui_report_fy2027.png` / `ui_report_fy2023.png`，逐字输出见 `ui_badge_tooltip.out`。

---

## 二、证据索引（全部是本目录下的真实输出）

| 文件 | 内容 |
|---|---|
| `report_curl.out` | 145 部署实况 + 真实 token `curl` 打报表（2027 / 2023 两年度）+ 提交侧冻结 `over_amount` + 分月明细 |
| `report_2027.json` / `report_2023.json` | 两次 `GET /business/erp/budget/report` 的原始报文 |
| `e2e06_run_all.out` | 145 上 `e2e06_run_all.sh` 全量输出（阶段 A + B + FIX-2 探针 + 13 组勾稽） |
| `asserts.out` | 全部断言明细（`PASS=228 / FAIL=0`） |
| `erp_invariants_expense.out` | 巡检「报销」组：9/9 通过（预算占用 ↔ 报销口径） |
| `erp_invariants_acct.out` | 巡检「会计」组：见 §五「已知残留」（不是本卡引入） |
| `cargo_test.out` | 99 本机 `cargo test --lib`：**3898 passed / 0 failed** |
| `frontend_unit.out` | 新增前端单测：7/7 通过 |
| `frontend_typecheck.out` / `frontend_build_tail.out` | `pnpm typecheck` / `pnpm build` 通过 |
| `ui_report_fy2027.png` | 真实浏览器（145 后端 + dev server）FY2027 报表页：「本月」列 `-500.00 / 2027-02 · 超 500.00`，「预算」列红标 `本月超 500.00`，汇总「年度超支 0 个 · 本月超 1 个」 |
| `ui_report_fy2023.png` | 同页 FY2023：`未生效` 黄标（`effective_from=2026-12-01 > 2023 年末`），本月列 `5,000.00 / 2023-01` |
| `ui_badge_tooltip.out` | 徽标悬停文案实测（`2027-02 月度已用 1,500.00 / 额度 1,000.00，超 500.00（提交侧月度闸同口径）`） |

### 关键报文（原文摘自 `report_curl.out`）

```
# FY2027（5602.05 通讯费，月额度 1000，2027-02 报了 1500）
{"fiscal_year":2027,"account_code":"5602.05","total_amount":"12000.0000","used":"1500.0000",
 "available":"10500.0000","over":false,
 "effective":true,"effective_from":"2026-12-01",
 "month_period":"2027-02","month_limit":"1000.0000","month_used":"1500.0000",
 "month_available":"-500.0000","month_over":true,"month_over_amount":"500.0000"}

# 提交侧冻结的超支快照（同一套口径的直接对照）
EXP-20261109-000062|2027-02|1500.0000|1|500.0000

# FY2027（5602.02 差旅费，月度未超 → 不许误报）
{"account_code":"5602.02","available":"146820.0000","over":false,"effective":true,
 "month_period":"2027-02","month_available":"9320.0000","month_over":false,"month_over_amount":null}

# FY2023（年度早于生效日 2026-12-01）
{"fiscal_year":2023,"account_code":"5602.02","over":false,"available":"60000.0000",
 "effective":false,"effective_from":"2026-12-01","month_period":"2023-01",
 "month_over":false,"month_available":"5000.0000"}
```

### 145 探针断言（`e2e06_fix2_report_probe.sh`，17 条全过）

```
✅ 报表 http = 200 / code = 0
✅ month_over=true（本月已超，主列要能看出来）
✅ month_over_amount = 500 / month_available = -500 / month_period = 2027-02
✅ month_limit = 1000 / month_used = 1500
✅ over=false（年度口径不变）/ available = 10500（年度）
✅ 报表 month_over_amount == 提交侧冻结的 over_amount（DB=500.0000）
✅ 分月明细 2027-02 可用仍为 −500（是**加**一列，不是替换 months）
✅ 5602.09 槽 month_over=false（没超不误报）/ month_over_amount=null
✅ FY2027 5602.02 effective=true（修复前为 false）/ effective_from 原样回显
✅ FY2023 5602.02 effective=false（生效日晚于该年度期末）
```

全量演练：`PASS=228 FAIL=0`（13 组勾稽 0 差异），其中 `e2e06_budget_b.sh` 里原有的
"报表 effective=false（本月月末口径）"断言已按新口径改为 `true`。

---

## 三、代码改动

后端（`#[cfg(test)]` 覆盖见 §四）：

| 文件 | 改动 |
|---|---|
| `budget_guard.rs` | 新增 `fiscal_year_end` / `effective_in_year` / `MonthGate`（唯一的月度闸公式）+ `FISCAL_YEAR_MIN/MAX`；`BudgetVerdict::evaluate`、`month_available()` 改为调 `MonthGate`；模块文档补「报表时点不同」 |
| `budget_model.rs` | `ReportRow` 增加 `month: MonthSummary`；新增纯函数 `month_summary()`（挑最紧张月）；`report()` 的 `effective` 改按 `effective_in_year` |
| `budget_dto.rs` | `ReportRes` 增加 6 个月度字段 + 双口径文档；`From<&ReportRow>` 映射 |
| 三个 `*_tests.rs` | 见 §四 |

前端：

| 文件 | 改动 |
|---|---|
| `api/business/erp/budget.ts` | `BudgetReportRow` 增加 6 个月度字段（逐字抄后端）+ 头部字段表 |
| `views/business/erp/budget/budget-logic.ts` | `reportBadge()`（月度优先徽标）、`summarizeReport.monthOverCount` |
| `views/business/erp/budget/report.vue` | 「本月」列 + 预算列徽标 + 汇总文案 + `.month-cell` 样式 |
| `tests/unit/erp-budget-report-month.test.ts` | 新增 7 条纯逻辑单测 |

脚本：`e2e06_budget_b.sh`（断言按新口径更新 + 3 条月度断言）、`e2e06_run_all.sh`（挂上探针）、
`e2e06_fix2_report_probe.sh`（新）、`e2e06_fix2_collect_evidence.sh`（新）。

无需迁移（不动表结构）。

---

## 四、单测钉子（卡面验收 ⑤「同一套逻辑」）

`budget_guard_tests.rs`：

- ㉓ `month_gate_is_the_single_monthly_formula`：`over` 边界（用满不算超）、`over_amount`、带符号 / 截零两种可用；
- ㉔ `verdict_month_gap_comes_from_month_gate`：6 组（额度, 已用）× 4 组（本次）全遍历，
  断言 `verdict.month_gap == max(0, 本次 − MonthGate 截零可用)`，且 `MonthGate::over()` 为真时提交侧必判超支；
- ㉕ `fiscal_year_end_is_december_31`：含 2000 / 2100 端点与越界 `None`；
- ㉖ `report_effective_is_judged_at_fiscal_year_end`：验收 ③④ + 期末当天 + NULL 生效日 + 与「期间月末」口径的对照。

`budget_model_tests.rs`：

- ⑤ `month_summary_picks_the_over_month`（WARN 槽场景 → 500）；
- ⑥ `month_summary_shows_month_over_while_year_is_fine`（年度 10500 没超、月度超 500 —— bug 的正面钉子）；
- ⑦ 多个月都超取超得最多的；⑧ 都没超取最紧的且 `over_amount=None`；
- ⑨ 无分解 → 全 `None` 不报超支；⑩ `month_summary_agrees_with_guard_month_gate`（报表 vs 提交侧逐值对齐）。

`budget_dto_tests.rs`：⑬ `report_res_carries_both_year_and_month_view`（出参同时带两套口径，
`over=false` 且 `month_over=true` 且 `effective=true`）。

前端：7 条（月度优先徽标 / 年度超 / 未生效 / 正常 / 无分解 / 汇总两个计数分开）。

---

## 五、已知残留 / 需要知道的

1. **144 生产未部署**：本次只动代码，无迁移；下次发布随 `origin/main` 生效。
2. **145 演练库有别的卡片的残留数据**：`erp_invariants.py --group 会计` 报
   `A2/A3` BLOCKER —— 凭证 `P2C2-V-0001`（`source_type=STOCK_IN`，`created_at=2026-11-09 03:12`，
   头金额 226 / 分录 0 条）。这是**另一张卡（P2C2）早上演练的残留**，早于本卡演练（14:30+），
   与本次改动无关（本次只读报表 + 纯函数，不写凭证）。本次相关的「报销」组 9/9 通过。
   建议由 P2C2 那张卡或运维同志清掉这张孤儿凭证，否则任何跑全量巡检的人都会看到这条 BLOCKER。
3. **共享工作区并发**：99 的 `/opt/cloudpivot` 是多 worker 共用，本次提交一律按路径 `git add`，
   未触碰他人文件；提交时刻另有 worker 的 `serial_handler.rs` 处于未完成的编辑状态（语法未闭合），
   与本次提交无关（`origin/main` 上该文件是完整的；145 的 `cargo build` 成功即证明）。
4. **探针依赖 A/B 夹具**：`e2e06_fix2_report_probe.sh` 读的是 `e2e06_budget_a/b.sh` 建的槽
   （部门 `E2E06-BDG`：5602.05 FY2027 月超、5602.02 FY2027 / FY2023）。没跑过 A/B 时它会明确输出
   "缺夹具"并 `exit 1`，不会给假绿。

---

## 六、复跑方式（145）

```bash
cd /opt/cloudpivot && git pull origin main
export PATH=/root/.cargo/bin:$PATH
cargo build --bin cloudpivot --manifest-path backend/Cargo.toml   # 仅后端变了才需要
bash scripts/verify/e2e06_deploy_145.sh        # pull + build + restart + health
bash scripts/verify/e2e06_run_all.sh           # A + B + FIX-2 探针 + 勾稽（期望 PASS=228 FAIL=0）
bash scripts/verify/e2e06_fix2_collect_evidence.sh   # 汇总到 /tmp/fix2_evidence/
```
