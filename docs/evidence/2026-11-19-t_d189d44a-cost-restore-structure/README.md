# M5-26 成本还原：半成品成本结构表（逐层结构）— 证据

- 卡：`t_d189d44a`（父卡 M5-25 `t_8d1440e7` / 提交 `d29aad54`；下游 M5-27 `t_8ea5bf43`「逐层还原到料工费」）
- 日期：2026-11-19
- 结论：新增只读 `GET /api/business/erp/cost-restore-structure`，复用 `business:erp:cost-object:list`；
  **零迁移 / 零新增权限码 / 零菜单 / 零前端**（前端页是 M5-30、还原卷积是 M5-27）。

## 1. 这张表回答什么

**一个成本对象本期投进去的材料，逐层拆开是什么结构**：

```text
本期材料投入  M  = Σ 领料行金额（biz_cost_collect.element = 'MATERIAL'）
  ├─ 外购材料 P  = 没有任何成本对象产出过的物料 → 叶子，金额全额进「材料」
  └─ 自制半成品 S = 有成本对象产出过的物料（o.product_id = 该物料）
       └─ 展开：S 的金额 × 所产批次完工成本结构 → 本层 料 / 工 / 费
            └─ S 自己那批的材料投入 → 再下一层…（按批号继续下钻）
```

**与 M5-27 的分工（本卡不越界）**：本卡只出「每一层的结构」，**不动钱的总额**；
把结构按比例卷积成「还原后的料工费」是 M5-27 的事。`layers[]` 也不是加总关系 ——
第 1 层合计 = 本期材料投入，第 2 层起是「各半成品自身的投入」。

## 2. 结构比例的来源（唯一口径，别另外算）

取 `biz_cost_period_item`（计算单）那一行：

```text
完工材料     = round4(完工数量 × unit_material)
完工人工     = round4(完工数量 × unit_labor)
完工制造费用 = 完工转出 − 完工材料 − 完工人工      ← 4 位尾差倒挤进制造费用
```

与 calc 第 6 段 `cost_period_item_dto::compose` 的公式逐字一致（那里
`finished_by_formula = completed×unit_material + completed×unit_labor + completed×unit_overhead`，
三项都 4 位取整；在制为 0 时 `finished_cost` 直接等于流入合计）。所以
**「三要素合计 = 节点金额」是恒等式**，不是约等 —— 真库用例逐个节点断言了它。

节点上的三要素 = 节点金额 × 结构比例（尾差同样进制造费用）。

## 3. 批号对得上 vs 对不上（唯一影响精度的分叉）

| 情况 | 来源 | 能不能下钻 |
|---|---|---|
| 领料明细批号 = 某成本对象的产出批号 | `batch` | **能**（精确指到那一批，继续展开它自己的材料投入） |
| 没有批号 / 批号对不上 | `period_avg`（该物料**本期及以前最近一个**有计算单的期间，多批汇总的结构） | **不能**（多批混在一起的子层金额与父行加不起来，硬展开就是第二本账） |

只认 `period_code <= 本期` 且非红冲（`REVERSED` / `CANCELLED`）的批次：未来期间的计算单不解释本期成本，
作废的账不拿来拆钱。

## 4. 五档行状态（**不画假 0**）

| 状态 | 判据 | 显示 |
|---|---|---|
| `semi` | 有自制半成品，且每层结构都取到 | 逐层可还原 |
| `no_semi` | 材料全是外购料 | 一层到底 |
| `no_structure` | 有自制半成品但取不到其完工成本结构（未计算 / 已红冲 / 完工成本 0） | 该节点三要素给 `null` + `issues` |
| `cycle` | 结构闭环（A 用 B、B 又用 A） | 闭环处 `truncated = true` |
| `truncated` | 到 `max_depth` 还没钻完（且下钻有意义） | 截断处 `truncated = true` |

一行同时命中多档时按 `cycle > no_structure > truncated > semi > no_semi` 取一档，其余事实都在
`issues` / `warnings` 里。

**`is_semi` 与「有没有计算单」是两件事**（本卡实现里的一个关键修正）：判「自制半成品」只看
**有没有成本对象产出过这个物料**（`biz_cost_object.product_id`），与计算单无关。
否则「本月刚投产、还没算成本的半成品」会被当成外购料，金额全额记进材料 ——
还原出来的料工费就少算了工和费。

## 5. 接口

| # | 方法 | 路径 | 权限码 |
|---|---|---|---|
| 1 | GET | `/business/erp/cost-restore-structure` | `business:erp:cost-object:list` |

查询参数：`period_code`（必填 `yyyy-MM`）、`keyword`、`state`（`all` / `semi` / `no_semi` / `blocked`）、
`page`、`page_size`（上限 200）、`max_depth`（默认 6，夹到 1–12）。

出参：`batch` / `cost_ready` / `totals`（整表，非当前页）/ `list[]`（`tree` 逐层结构 + `layers` 逐层汇总 +
`issues` / `warnings`）/ `state_options` / `note`。每个节点带 `qty` / `unit_cost` / `amount` /
`share_pct` / `is_semi` / `source` / `producer`（含该批完工成本结构与单位成本）/ `elements` / `children`。

**行**的数据范围：`biz_cost_object.workshop_dept_id`（与归集表 / 约当表 / 分配表 / 余额表同一锚点）。
**半成品的产出批次不再二次裁剪**（有意）：那笔结构就是**已经算进本行材料成本**的钱，
跨车间半成品流转（冲压 → 焊装 → 总装）是离散制造常态；二次裁剪会让「材料投入 100 万、结构全是 null」
变成常见状态。代价与边界写在取数层模块头。

**取数形状**：BFS 逐层展开，每层两条查询（材料投入 + 自制判定 + 生产者候选，分片批量）；
节点数上限 20000（超了报 400 让人收窄）。

## 6. 测试证据（本次实现实测）

| 项 | 命令 | 结果 |
|---|---|---|
| 纯逻辑单测（dto 44 + handler 8 + model 9 = 61 中本卡 54） | `cargo test --lib cost_restore_structure` | **54 passed / 0 failed**（含 10 条真库用例） |
| 真库用例 | `ERP_DB_URL=…145:5432/cp_m526_dev cargo test --lib cost_restore_structure -- --test-threads=1` | **10 passed / 0 failed** |
| 权限审计 | `cargo test --lib permission_audit` | 见 §6.1 |
| cost 域回归 | `ERP_DB_URL=… cargo test --lib business::erp::cost` | 见 §6.1 |
| fmt / clippy | `cargo fmt` / `cargo clippy --lib -- -D warnings` | rc=0 / rc=0 |

真库用例覆盖（`cp_m526_dev` = 145 模板库克隆，跑完已 DROP）：

1. **三层结构**：成品 → 半成品 → 二级半成品 → 外购料，逐层金额 / 结构比例 / 批号命中 / 每个节点三要素合计 = 金额
2. **批号对不上**：手工料行（无来源单据）⇒ 同月平均结构、**不下钻**、给出提醒
3. **半成品没计算**：`is_semi = true` + `elements = null` + 行状态 `no_structure`（**不当外购料**）
4. **全外购料**：`no_semi`、一层到底、金额全额进材料
5. **闭环**：A 用 B、B 用 A ⇒ 第 2 层截断、行状态 `cycle`、不死循环
6. **层数上限**：`max_depth=1` 截断并给 `DEPTH_CUT`；`max_depth=6` 同一行**不**误报截断（探针精度）
7. **数据范围**：别车间用户看不到本车间的行（不出现，不报 403 探测口）
8. **整表合计与筛选**：`page_size=1` 时 `row_count` 仍是全表；`semi` / `blocked` 分档；关键字收窄
9. **红冲批次不作结构来源**：产出批次被红冲 ⇒ `no_structure`
10. **清场零残留**

### 6.1 门禁与真实 HTTP 冒烟（本次交付实测回填）

| 项 | 命令 | 结果 |
|---|---|---|
| fmt / clippy | `cargo fmt && cargo fmt --check` / `cargo clippy --lib -- -D warnings` | rc=0 / rc=0（零警告） |
| 权限审计 8 条 | `cargo test --lib permission_audit` | **8 passed / 0 failed**（零新增码 ⇒ n14/n18 不动） |
| 纯逻辑 + 真库（本卡 54 条） | `ERP_DB_URL=…/cp_m526_dev cargo test --lib cost_restore_structure -- --test-threads=1` | **54 passed / 0 failed**（含 10 条真库），日志 `cargo-test-db.log` |
| cost 域回归（空库 HEAD 版） | `ERP_DB_URL=…/cp_m526_pristine cargo test --lib business::erp::cost` | 1383 passed / 4 failed（见 §6.2），日志 `cargo-test-cost-domain-head-empty.log` |
| cost 域回归（demo_seed 版） | 同上，库换成 `cp_m526_head`（空库首启 + `scripts/demo_seed.sh`） | 1382 passed / 5 failed（见 §6.2），日志 `cargo-test-cost-domain-head-demo-seeded.log` |
| HTTP 冒烟（真接口 + 真库） | `RS_BASE=http://127.0.0.1:18026 python3 docs/evidence/…/smoke_restore_structure.py` | **32 PASS / 0 FAIL**，日志 `smoke.log` |
| 空库首启 | 空库 `cp_m526_pristine` 直接 `cargo run` | 400 条迁移 applied、203 张表、`/health` 200，**无需任何新迁移** |

**冒烟覆盖（32 条断言，逐字段）**：401 未登录 / 15 个页面级键 / 行头 材料 1500 · 半成品 1000 · 外购 500 ·
`semi_share` 66.67% / 行状态 `semi` + 中文标签 / 一级节点 2 个（金额降序）/ 节点 18 键 /
S 节点 `is_semi=true` `source=batch` 三要素 600·250·150 合计 = 1000 / `producer` 指到产出批号与完工成本 10000 · 单位 100 /
S 无下一层且未截断 / RAW 全额进材料 500·0·0 / `layers` 恰 1 层且三要素 1100·250·150 / `totals` 16 键与整表金额 /
`page_size=1` 时 `total` 与 `totals` 仍是整表 / 400 三连（缺期间、期间形状、状态拼错）/ `max_depth` 0→6、99→12 夹取 / 清场零残留。

### 6.2 cost 域回归的 4 / 5 条红：**环境耦合，与本草无关**（两态互补，我的 54 条在两态都全绿）

| 库状态 | 红的是谁 | 原因（原文断言） |
|---|---|---|
| 空库首启（`cp_m526_pristine`，400 迁移，无演示成员） | `cost_standard_db_tests` 4 条 | `「财务经理审批」节点的审批人全部是制单人 / 发起人本人 … 角色无成员`：**空库没有任何非制单人可当审批人**，需要演示成员 |
| 空库首启 + `demo_seed.sh`（`cp_m526_head`） | `cost_period_approval_db_tests` 5 条 | `夹具污染：FINANCE_MANAGER 上有夹具与演示种子之外的在职成员 ["wang.lei"]`：那条用例只允许「夹具账号」或 `demo_` 前缀成员，demo_seed 的 `wang.lei` 名字不带前缀 |

两态互为反面 ⇒ 这套 cost 回归**在任何单一库状态下都不会全绿**，属既有测试的环境耦合（一条要求「角色里得有人」，
另一条要求「角色里不许有别人」），**不是本草改动引入**：本草只新增 5 个文件 + `mod.rs` 8 行声明，
`cost_standard_*` / `cost_period_approval_*` 一行未动，且 `cost_restore_structure` 的 54 条在两态均 **54 passed**。
（首轮还遇到过 145 模板库 `cloudpivot_test` 漂移：缺 `biz_account.is_foreign_currency`，138 条假红 → 已改用空库首启的 HEAD 库重跑，见 `cargo-test-cost-domain.log` 与之对照。）

