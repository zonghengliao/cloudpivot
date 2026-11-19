# M5-28 成本还原：还原后报表（料工费占比）— 证据

- 卡：`t_3e57dec1`（父卡 M5-27 `t_8ea5bf43` / 提交 `f70667e5`；下游 M5-29 单测 `t_bb5ff09c`、M5-30 前端页）
- 日期：2026-11-19
- 结论：新增只读 `GET /api/business/erp/cost-restore-report`，复用 `business:erp:cost-object:list`；
  **零迁移 / 零新增权限码 / 零菜单 / 零前端**。报表**不重查库、不重算卷积** —— 行由 M5-27 的
  `rollup_rows_of` 一次给出，本卡只做「按维度分组 + 加法 + 占比」。

## 1. 这张报表回答什么

M5-27 出的是「**每个成本对象**的钱被还原成了什么」；本卡出的是「**按口径汇总后**的结构占比」：

```text
               还原前（归集口径）              还原后（原始成本项目）        占比怎么变
 车间 A   材料 500（含半成品 400）      →      外购材料 300                      材料 −28.57pp
          人工 100                              人工     140                      人工 +5.71pp
          制造费用 100                          制造费用 260                      费用 +22.85pp
 ────────────────────────────────────────────────────────────────────────────────
 合计     还原前合计 1000                     还原后 700 + 不彻底 300           四项合计 100.00%
```

三张表的分工（**不许重复实现**）：

| 卡片 | 出什么 | 粒度 |
|---|---|---|
| M5-26 `cost-restore-structure` | 每一层**是什么**（结构 / 比例） | 逐对象 × 逐层节点 |
| M5-27 `cost-restore-rollup` | 每块钱**还原成了什么**（料/工/费 + 逐层台账） | 逐对象 |
| **M5-28（本卡）** | **按口径汇总的占比与结构变化** | 车间 / 产品 / 成本对象 / 整表 |

## 2. 三条硬口径（别自己另算）

### 2.1 占比分母 = **还原前合计**（四项目合计 100.00%）

`材料 + 人工 + 制造费用 + 还原不彻底 = 还原前合计`，所以分母取还原前合计时**四项占比恰好 100.00%**。

**与 M5-27 行级 `after_*_share` 的差异要说清**：那边的分母是「还原后三要素合计」（不含不彻底）。
本表刻意换分母，是为了让「还原不彻底」在报表里**占一格**，而不是被悄悄摊到三要素头上 ——
否则一排 `partial` 行会把材料占比算虚高。两者可以互推：

```text
本表 material_share = M5-27 after_material_share × (100 − 不彻底占比) / 100
```

### 2.2 占比变化（pp）= 还原后占比 − 还原前占比

材料减少的百分点 = 人工 + 制造费用增加的百分点（+ 不彻底的百分点）；
三要素 pp 之和 = `− 不彻底占比`（各自 2 位舍入，允许 ±0.01 差，`share_sum` 把实际合计暴露出来）。

### 2.3 金额是真值，占比只做展示

所有占比都由 4 位金额算出后四舍五入到 2 位。四项合计可能落在 99.99 / 100.01 ——
`share_sum` 如实给出，**不假装必然 100.00**（真库用例里就有 100.01 的样本，见 §6）。

## 3. 四档维度 / 三档状态

| `group_by` | key | label | 副标 |
|---|---|---|---|
| `dept`（缺省） | `workshop_dept_id` | 车间名（缺 ⇒「未指定车间」） | — |
| `product` | `product_id` | `产出品编码 名称`（缺 ⇒「未指定产出品」） | — |
| `object` | `object_id` | 对象号 | 产出品 · 批号 |
| `all` | `ALL` | 全部 | — |

排序是**确定的**：还原后合计降序 → key 升序（翻页时行序不会每次刷新都变）。
`state`（`all` / `restored` / `partial` / `no_semi`）在**分组之前**筛行，与 M5-27 同码同义。

## 4. 接口

| # | 方法 | 路径 | 权限码 |
|---|---|---|---|
| 1 | GET | `/business/erp/cost-restore-report` | `business:erp:cost-object:list` |

查询参数：`period_code`（必填 `yyyy-MM`）、`keyword`、`group_by`（`dept` / `product` / `object` / `all`）、
`state`、`page`、`page_size`（**报表口径：缺省 50、上限 500**，与明细表的 20 / 200 不同）、
`max_depth`（默认 6，夹到 1–12）。

出参（`data`）：

| 键 | 说明 |
|---|---|
| `period_code` / `batch` / `cost_ready` | 期间与本期批次 |
| `group_by` / `group_by_label` / `group_options` | 维度回显 + 4 档选项 |
| `state` / `state_label` / `state_options` | 状态回显 + 4 档选项 |
| `keyword` / `max_depth` | 回显 |
| `basis` | 占比分母口径原文（后端唯一真源） |
| `formula_restore` / `formula_unresolved` | 沿用 M5-27 的卷积 / 不彻底口径 |
| `formula_share` / `formula_shift` | 占比 / 占比变化口径 |
| `totals` | 整表合计（**不受分组分页影响**），形状与分组行相同 |
| `groups[]` | 分组行 |
| `total` / `page` / `page_size` | 分组数与分页 |
| `note` | 页脚说明（空表 / 无批次 / 有不彻底 / 节点上限 / 三条口径） |

分组行（`groups[]` 与 `totals` 同形）：

| 键 | 说明 |
|---|---|
| `key` / `label` / `hint` / `object_count` | 分组标识与对象数 |
| `before` | `material` / `labor` / `overhead` / `total` + 三个占比（分母 = 还原前合计） |
| `restored` | `material` / `labor` / `overhead` / `total` / `unresolved` / `denominator` + **四个**占比 |
| `shift` | `material_pp` / `labor_pp` / `overhead_pp` + 口径 |
| `restored_labor` / `restored_overhead` | 从材料里拆出来的工费（金额） |
| `semi_in_material` / `semi_share` | 材料里的半成品及其占材料投入比 |
| `restored_rows` / `partial_rows` / `no_semi_rows` | 三档行数 |
| `dominant` / `dominant_label` | 占比最高的要素（`material` / `labor` / `overhead` / `none`） |
| `share_sum` / `check` | 四项占比实际合计 / 守恒自检（`restored.total + unresolved − before.total`，应为 0） |

## 5. 实现（3 个新文件 + 1 处小重构）

| 文件 | 作用 |
|---|---|
| `cost_restore_report_dto.rs` | 契约 + 纯逻辑：分组键 / 累加 / 占比 / pp / 主导要素 / 出参装配 |
| `cost_restore_report_handler.rs` | 归一化 + 调 M5-27 取行 → 分组 → 分页 → 整表合计 |
| `cost_restore_rollup_handler.rs` | 把「①–③ 取数 + 卷积 + 筛 + 合计」抽成 `rollup_rows_of`(`RollupRows`)；**明细表行为不变**（原 43 条用例一行未改仍全绿） |

**零 SQL 抄写**：数据范围、关键字、逐层结构、卷积全部来自 M5-27 那一次调用；
本卡只在内存里对行做加法。因此「明细表某几行的金额之和 = 报表同一分组的金额」是恒等式，
真库用例与 HTTP 冒烟都逐字段对拍过。

**数据范围**：行仍由 `biz_cost_object.workshop_dept_id` 裁剪（M5-26 取数层做）⇒
看不见的对象不出现在报表里，**分组也不会带出看不见的车间**，且不报 403。

## 6. 测试证据（本次实现实测）

| 项 | 命令 | 结果 |
|---|---|---|
| fmt | `cargo fmt && cargo fmt --check` | rc=0 |
| clippy（lib） | `cargo clippy --lib -- -D warnings` | rc=0（零警告） |
| clippy（全 target） | `cargo clippy --all-targets -- -D warnings` | rc=0（零警告） |
| 本卡 24 条（12 纯逻辑 + 6 handler + 6 真库） | `ERP_DB_URL=…145:5432/cp_m528_head cargo test --lib cost_restore_report -- --test-threads=1` | **24 passed / 0 failed**，日志 `cargo-test-restore.log` |
| 成本还原三卡回归（结构 54 + 还原 43 + 报表 24） | `ERP_DB_URL=…/cp_m528_head cargo test --lib cost_restore` | **121 passed / 0 failed**（重构后明细表用例一行未改仍全绿） |
| 权限审计 8 条 | `cargo test --lib permission_audit` | **8 passed / 0 failed**（零新增码 ⇒ n14/n16/n18 不动） |
| cost 域回归（空库首启 HEAD 库） | `ERP_DB_URL=…/cp_m528_head cargo test --lib business::erp::cost` | **1450 passed / 4 failed**（见 §6.1），日志 `cargo-test-cost-domain-head.log` |
| HTTP 冒烟（真接口 + 真库，空库首启库） | `RS_BASE=http://127.0.0.1:18033 python3 docs/evidence/…/smoke_restore_report.py` | **58 PASS / 0 FAIL**，日志 `smoke.log` |
| 空库首启 | 空库 `cp_m528_head` / `cp_m528_smoke` 直接跑二进制 | **400 条迁移 applied、203 张表、`/health` 200**（`empty-db-first-boot.log`），无需任何新迁移 |

### 6.1 cost 域回归的 4 条红：**空库无演示成员，与本卡无关**

4 条全在 `cost_standard_db_tests`，断言原文（节选）：

```text
提交发布应成功: ApiError { code: 65018, msg: "节点「财务经理审批」的审批人全部是制单人 / 发起人本人…
升级链已走完：财务经理（原角色，角色无成员） → 财务总监（0/0 人） → 总经理（0/0 人）…
（标准成本卡保持草稿、没有生成审批单）" }
```

空库首启的 HEAD 库里没有任何「非制单人」可以当审批人，这套用例本来就要演示成员才能过
（M5-27 在 `cp_m527_head` 上跑出的同样是这 4 条）。**旁证**：同一次回归里本卡 24 条全绿、
结构表 54 + 还原表 43 全绿，1450 = 1426（M5-27 那次）+ 24（本卡）。

### 6.2 冒烟覆盖（58 条断言，逐字段）

401 未登录 / 22 个页面级键 / 4 档 `group_options` / 4 档 `state_options` / 5 条口径文案 /
缺省 `dept` + 50 + 6 / 成本开关走**参数接口**置位（直连 SQL 不刷新快照，实测 2 分钟仍 403）/
一车间组 500·100·100=700 → 300·140·260=700、占比 42.86 / 20.00 / 37.14 = 100.00、
还原前 71.43 / 14.29 / 14.29、pp −28.57 / +5.71 / +22.85、拆出工费 40 / 160、半成品占比 80.00%、
主导要素 material / 三档行数 1/0/0 / check 0 /
二车间组 `partial`：0·0·0 + 不彻底 300、不彻底占比 100.00%、主导要素 none、守恒 /
分组按还原后合计降序 / 合计 800·100·100=1000 → 300·140·260=700 + 不彻底 300、占比 30.00 / 14.00 / 26.00 / 30.00、
合计 = 各组之和 /
按产品 1 组合并（FG 与 FG2 同一产出品）= 1000 / 按对象 2 组逐字段对拍明细表（8 个字段）/ 对象组带副标 /
整表 1 组 = totals /
400 四连（缺期间 / 期间形状 / group_by 拼错 / state 拼错）+ `max_depth` 99⇒12、`page_size` 9999⇒500、0⇒6/50 /
`page_size=1` 当前页 1 组但合计仍是整表 / `state=partial` 收窄到二车间 / `state=restored` 收窄到一车间 /
空结果占比 `None`（不画假 0.00%）且页脚给可读说明 / 清场零残留。

## 7. 与上游口径的交叉自查

- M5-27 的守恒式 `after.total + unresolved = before.total` 逐行成立 ⇒ 本表分组的
  `check` 必然为 0（真库用例逐组断言，冒烟也查了）；
- 本表 `totals` 与 `groups` 用**同一份加法**（`GroupAcc::add_row`）⇒ 合计 = 分组之和，
  不可能出现「页脚与行对不上」；
- 明细表某几行之和 = 报表同一分组：冒烟第 ⑥ 段用 `group_by=object` 与 `cost-restore-rollup`
  的出参逐字段（8 个）对拍；
- 空表不画假 0.00%：分母 0 ⇒ 所有占比 `None`、`share_sum` `None`、主导要素 `none`。
