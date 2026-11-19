# M5-27 成本还原：逐层还原到料工费 — 证据

- 卡：`t_8ea5bf43`（父卡 M5-26 `t_d189d44a` / 提交 `b26e4ec7`；上游结构表 `GET /business/erp/cost-restore-structure`）
- 日期：2026-11-19
- 结论：新增只读 `GET /api/business/erp/cost-restore-rollup`，复用 `business:erp:cost-object:list`；
  **零迁移 / 零新增权限码 / 零菜单 / 零前端**（前端页是 M5-30）。逐层结构**不重查一遍库** ——
  直接消费 M5-26 的树，在内存里递归卷积。

## 1. 这张表回答什么

M5-26 出的是「**每一层的结构**」（这个半成品自己是什么料/工/费构成）；
M5-27 出的是「**钱被还原成什么**」：

```text
还原前（归集口径）              递归卷积                        还原后（原始成本项目）
  材料（含半成品）   ──┐  材料里的自制半成品按「所产批次完工成本        外购材料
  人工              ──┤  结构」拆成 料/工/费；拆出来的「料」若      →   人工
  制造费用          ──┘  仍是半成品就继续拆，直到全是外购料     →   制造费用
                                                                （+ 还原不彻底，单列）
```

**总额一分不动**：`还原后合计 + 还原不彻底 = 还原前合计`，这是恒等式（每一层拆分都守恒），
不是约等 —— 真库用例逐行断言，整表合计还有 `totals.check`（恒为 0）。

## 2. 三条硬口径（别自己另算）

### 2.1 流入金额：第 1 层 = 材料投入，第 2 层起 = 上一层下拨的钱

| 层级 | `flows[].imputed` 从哪来 |
|---|---|
| 第 1 层 | 节点自己的 `amount`（加总 = 本期材料投入） |
| 第 N 层 | 上一层节点的「材料部分」按**兄弟金额占比**分下来的份额 |

所以逐层台账**不是加总关系**：第 1 层 500 里，只有 200 是「还是半成品形态」的部分被拨到第 2 层；
把各层金额加起来当总额就是算两遍（`totals.note` 与每层 `flows[].note` 都写明了）。

### 2.2 拆分：比例只从 M5-26 的 `elements` 来，尾差进制造费用

```text
材料部分 = round4(流入 × elements.material / elements.total)
人工部分 = round4(流入 × elements.labor    / elements.total)
制造费用 = 流入 − 材料部分 − 人工部分          ← 4 位尾差进制造费用
```

`elements` 就是 M5-26 从 `biz_cost_period_item` 取好的那一行（完工材料 / 完工人工 /
完工转出倒挤的制造费用），两边同源 ⇒ 两张表不可能吵架。

### 2.3 拆不动 ⇒ 「还原不彻底」，**不画假 0**

| 情况（M5-26 的节点事实） | 本表怎么处理 |
|---|---|
| 外购料叶子（`is_semi = false`） | 材料部分全额确认为**外购材料** |
| 自制半成品 + 有下一层且未截断 | 工/费当场确认，材料部分继续往下卷积 |
| 自制半成品但 `elements = null`（没计算单 / 已红冲） | 整笔进**还原不彻底**（不当外购料） |
| 自制半成品 `truncated`（闭环 / 层数上限） | 工/费照确认，材料部分进**还原不彻底** |
| 自制半成品有结构但没有下一层投入（同月平均 / 空批） | 同上 |

`还原不彻底` 既不算进外购材料、也不丢掉：它就是「还是半成品形态的那笔钱」，
留给用户按行内 `issues` / `warnings`（沿用 M5-26 的 `NO_STRUCTURE` / `CYCLE` / `DEPTH_CUT`）去修。

## 3. 三档行状态（这张表只关心「能不能还原」）

| 状态 | 判据 | 含义 |
|---|---|---|
| `no_semi` | 材料里没有自制半成品 | 本来就全是外购料，还原前后一致 |
| `restored` | 有半成品且 `unresolved = 0` | 逐层还原干净 |
| `partial` | 有半成品且 `unresolved > 0` | 有金额停在半成品形态 |

（M5-26 的五档状态是「结构取到的程度」，本表的三档是「钱还原到什么程度」；两者不是一回事，
所以还原表的 `state` 参数**不会**透传给结构表 —— 透传会让 `no_structure` 的行提前消失。）

## 4. 接口

| # | 方法 | 路径 | 权限码 |
|---|---|---|---|
| 1 | GET | `/business/erp/cost-restore-rollup` | `business:erp:cost-object:list` |

查询参数：`period_code`（必填 `yyyy-MM`）、`keyword`、`state`（`all` / `restored` / `partial` / `no_semi`）、
`page`、`page_size`（上限 200）、`max_depth`（默认 6，夹到 1–12）。

出参（`data`）：

| 键 | 说明 |
|---|---|
| `period_code` / `batch` / `cost_ready` / `state` / `keyword` / `max_depth` | 回显 |
| `formula_restore` / `formula_unresolved` | 页脚口径原文（后端唯一真源） |
| `totals` | 整表合计（不受分页影响）：before/after 各三要素 + `unresolved` + `check`（= 0）+ 分档行数 + 占比 |
| `list[]` | 行：行头 + `item` + `before`（材料含半成品）+ `semi_in_material` / `purchased_in_material` / `semi_share` + `after`（材料只含外购料）+ `delta`（total 恒 0）+ `restored_labor` / `restored_overhead` + `unresolved` + `state` + `depth` / `node_count` + `flows[]` + `semis[]` + `issues[]` / `warnings[]` |
| `total` / `page` / `page_size` / `state_options` / `note` | 分页与页脚 |

`flows[]`（逐层台账，9 个键）：`level` / `node_count` / `imputed` / `purchased` / `labor` /
`overhead` / `carried_down` / `unresolved` / `note` —— 每层满足
`imputed = purchased + labor + overhead + carried_down + unresolved`。

`semis[]`（半成品还原明细）：`level` / `material_*` / `lot_no` / `imputed` / `source` / `source_label` /
`producer_nos` / `elements`（还原结果）/ `unresolved` / `stopped`（停在这一层 = 没下钻）。

## 5. 实现（为什么只有 3 个新文件 + 1 处小重构）

| 文件 | 作用 |
|---|---|
| `cost_restore_rollup_dto.rs` | 契约 + 纯逻辑：`restore_of` 递归卷积 / 尾差 / 台账 / 三档状态 / 出参装配 |
| `cost_restore_rollup_model.rs` | 唯一新增取数：本期 `LABOR` / `OVERHEAD` 归集（口径与 calc 的 `objects_of_period` 逐字一致） |
| `cost_restore_rollup_handler.rs` | 归一化 + 调 M5-26 → 逐行卷积 → 筛 → 分页 → 合计 |
| `cost_restore_structure_handler.rs` | 把「①–⑤ 展开 + 筛 + 合计」抽成 `expanded_rows_of`（`Expanded`），结构表本身**行为不变**（原 54 条用例不改一字仍全绿） |

**零 SQL 抄写**：材料投入、谁是自制半成品、产出批次、完工成本结构全部来自 M5-26 的取数层；
本卡只在内存里对 `tree[]` 做卷积。

**数据范围**：行仍由 `biz_cost_object.workshop_dept_id` 裁剪（M5-26 取数层做）；
手工/制造费用归集只按本页已有对象回填，**不再二次裁剪**（否则跨车间对象的人工会被抹成 0，
还原前的分母就错了）。

## 6. 测试证据（本次实现实测）

| 项 | 命令 | 结果 |
|---|---|---|
| fmt | `cargo fmt && cargo fmt --check` | rc=0 |
| clippy | `cargo clippy --lib -- -D warnings` | rc=0（零警告） |
| 纯逻辑 + 真库（本卡 43 条） | `ERP_DB_URL=…145:5432/cp_m527_dev cargo test --lib cost_restore_rollup -- --test-threads=1` | **43 passed / 0 failed**（含 9 条真库），日志 `cargo-test-db.log` |
| 权限审计 8 条 | `cargo test --lib permission_audit` | **8 passed / 0 failed**（零新增码 ⇒ n14/n18 不动） |
| 结构表回归（重构后） | 上面那次 cost 域回归里的 `cost_restore_structure` | **54 passed / 0 failed**（用例一行未改，行为未变） |
| cost 域回归（空库首启 HEAD 库） | `ERP_DB_URL=…/cp_m527_head cargo test --lib business::erp::cost` | **1426 passed / 4 failed**（见 §6.1），日志 `cargo-test-cost-domain-head-empty.log` |
| HTTP 冒烟（真接口 + 真库） | `RS_BASE=http://127.0.0.1:18031 python3 docs/evidence/…/smoke_restore_rollup.py` | **40 PASS / 0 FAIL**，日志 `smoke.log` |
| 空库首启 | 空库 `cp_m527_head` 直接跑二进制 | **400 条迁移 applied、203 张表（149 张 `biz_*`）、`/health` 200**，无需任何新迁移 |

### 6.1 cost 域回归的 4 条红：**空库无演示成员，与本卡无关**

4 条全在 `cost_standard_db_tests`，断言原文（节选）：

```text
提交发布应成功: ApiError { code: 65018, msg: "节点「财务经理审批」的审批人全部是制单人 / 发起人本人…
升级链已走完：财务经理（原角色，角色无成员） → 财务总监（0/0 人） → 总经理（0/0 人）…
（标准成本卡保持草稿、没有生成审批单）" }
```

空库首启的 HEAD 库里没有任何「非制单人」可以当审批人，这套用例本来就要演示成员才能过
（M5-26 在 `cp_m526_pristine` 上跑出的同样是这 4 条）。本卡不碰 `cost_standard_*` / 审批任何一行；
**旁证**：同一次回归里本卡 43 条全绿、结构表 54 条全绿，1426 = 1383（M5-26 那次）+ 43（本卡的用例）。

### 6.2 冒烟覆盖（40 条断言，逐字段）

401 未登录 / 15 个页面级键 / 4 档 `state_options` / 行头 31 键 /
还原前 500·100·100=700 / 材料里半成品 400 · 外购 100 · 占比 80.00% /
还原后 300·140·260=700 / 状态 `restored` / 不彻底 0 /
台账两层（第 1 层 500 = 外购 100 + 人工 40 + 费 160 + 下拨 200；第 2 层 200 全成外购料，逐层自洽）/
半成品明细 S 被卷积 400 ⇒ 200·40·160、`stopped=false`、`source=batch`、`producer_nos` 指到产出对象号 /
差额 材料 −200 · 人工 +40 · 费 +160 · 合计 0 /
缺结构行 `partial`（300 停在半成品形态、不当外购料、仍守恒、行内有原因）/
合计 row_count 2 · 还原前 1000 · 还原后 700 · 不彻底 300 · check 0 · 分档 1/1/0 · max_depth 2 /
400 三连（缺期间 / 期间形状 / 状态拼错）+ `max_depth` 0⇒6、99⇒12 /
`page_size=1` 时合计仍是整表 / `state=partial`、`state=restored` 各收窄 1 行 / 清场零残留。

## 7. 与上游口径的交叉自查

- M5-26 的 `elements.total == node.amount`（恒等式）⇒ 本表的「流入 × 比例」不需要再对齐尾差，
  只吃 4 位舍入差，已经进制造费用；
- M5-26 的 `source = period_avg` 节点 `children` 为空 ⇒ 本表自动落到「工/费确认、材料部分不彻底」，
  不会拿多批混算的子层去对一个人的金额（第二本账）；
- M5-26 的 `truncated` 节点即使有 `children` 也不下钻（本表显式判断 `node.truncated`），
  闭环处不会被重复展开。
