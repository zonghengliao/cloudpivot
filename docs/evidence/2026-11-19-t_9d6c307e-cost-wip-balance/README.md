# M5-23 在制品：期末在制余额（结转核对）— 证据

- 卡：`t_9d6c307e`（父卡 M5-22 `t_09d1804d` / 提交 `ce131866`）
- 日期：2026-11-19
- 结论：新增只读 `GET /api/business/erp/cost-wip-balance`，复用 `business:erp:cost-object:list`；
  **零迁移 / 零新增权限码 / 零菜单 / 零前端**。

## 1. 这张表是什么

一行 = 一个成本对象，把余额的来龙去脉摆平，再核对「这笔余额有没有交给下一期」：

```text
期初在制（上期已结算计算单的期末，按上期单位成本还原到料 / 工 / 费）
  + 本期投入（biz_cost_collect 三要素）
  − 完工转出
= 期末在制余额（差额倒挤，三要素合计守恒）    ← 与 M5-22 的 elements[].wip 同一口径
```

**为什么不写库、不建结转表**：设计 §4.6.3 定了期初在制**自动**从上期**已结算**计算单取数
（`cost_period_item_model::prior_settled_item`），库里唯一真相是 `biz_cost_period_item.wip_end_cost`。
再建一张「结转表」就是第二本成本账（ARCH-4 D9 明令禁止的反例：两处同步必漂移）。
本卡因此把「结转到底成没成」**逐行核出来**，而不是再搬一次数。

## 2. 结转八态（判据全部取自库里已有的数）

| 状态 | 判据 | 要人做什么 |
|---|---|---|
| `carried` 已结转 | 本期已结算，且下期计算单的 `wip_beg_cost` / `wip_beg_qty` 与本期账面期末一分不差 | 无 |
| `pending` 待结转 | 本期已结算，下期还没有计算单 | 无（下期算的时候自动取走） |
| `mismatch` 结转不齐 | 本期已结算，但下期期初 ≠ 本期账面期末（差额逐行给数） | 核下期期初来源，确认后红冲下期重算 |
| `unsettled` 未入账 | 本期**没有**已结算计算单（试算余额还没入账） | **最危险**：结算本期 |
| `reversed` 已红冲 | 本期批次被红冲，账面余额作废 | 重算后再结算 |
| `closed_wip` 关批挂账 | 对象已 `CLOSED` 却仍挂期末在制 | 改回 OPEN 或按呆滞 / 报废处理 |
| `cleared` 无余额 | 期末在制为 0（全部完工） | 无 |
| `blocked` 算不出 | 缺完工率 / 无产量 / 期初还原失败 / 负数分配（见 `issues`） | 按行内「怎么修」处理 |

`unsettled` 单列一档的理由：它**看着有余额**（表里给试算数），但库里没有已结算快照，下期期初取 0 ——
这笔钱会凭空变成下期的完工成本。合计里单给 `unbooked_total`。

## 3. 账面优先与「不给假明细」

- 已结算的行：三个金额列（期初 / 完工转出 / 期末余额）取**计算单快照** `SETTLED` 行的
  `wip_beg_cost` / `finished_cost` / `wip_end_cost`（那才是账上的数）。
- 试算与账面一致（M5-22 的 `recalc_required = false`）⇒ 分要素照给，三项加起来 = 余额。
- 试算与账面不一致 ⇒ **分要素一律 `null`** + `LEDGER_DRIFT`：按试算拆出来的三要素与账面余额加不齐，
  展示出来就是一笔读不懂的账。修法是红冲本期重算。
- 算不出来的行：金额一律 `null`（不给 0），且不进金额合计；数量合计含全部行。

## 4. 接口

| # | 方法 | 路径 | 权限码 |
|---|---|---|---|
| 1 | GET | `/business/erp/cost-wip-balance` | `business:erp:cost-object:list` |

查询参数：`period_code`（必填 `yyyy-MM`）、`keyword`、`state`（十档，缺省 `wip`）、`page`、`page_size`（上限 200）。
出参：`batch` / `reversed_batch` / `cost_ready` / `totals`（整表，非当前页）/ `list` / `state_options` / `note`。

数据范围：行由 `biz_cost_object.workshop_dept_id` 裁剪（与分配表 / 约当表 / 归集表同一锚点）；
看不见的对象不出现在表里，也不报 403（单对象探针报 404，不泄漏存在性）。

## 5. 测试证据（本次实现实测）

| 项 | 命令 | 结果 |
|---|---|---|
| 纯单测（dto + model + handler） | `cargo test --lib business::erp::cost::cost_wip_balance` | **51 passed / 0 failed** |
| 真库用例 | `ERP_DB_URL=…145:5432/cp_m523_dev cargo test --lib cost_wip_balance_db_tests -- --test-threads=1` | **10 passed / 0 failed** |
| cost 域回归 | `cargo test --lib business::erp::cost` | **1313 passed / 0 failed**（= M5-22 的 1262 + 本卡 51） |
| 权限审计 | `cargo test --lib permission_audit` | **8 passed**（N14/N16/N18 全绿） |
| fmt / clippy | `cargo fmt -- --check` / `cargo clippy --lib --tests -- -D warnings` | **rc=0 / rc=0** |
| 提交前检查 | `bash scripts/check.sh --fast` | **L0 + L1 全部通过** |

真库用例覆盖（10 条，`cp_m523_dev` 用 145 模板库克隆，跑完已 DROP）：

1. 没批次：余额是试算（`source=live`）并标「未入账」，三要素加起来 = 余额
2. 已结算：三个金额列取计算单快照，结转「待下期」
3. 下期算完且期初一分不差 ⇒ **已结转**
4. 下期期初被改 0.0001 ⇒ **结转不齐**（差额给数）
5. 关批仍挂余额 ⇒ **关批挂账**
6. 本期批次红冲 ⇒ **已红冲**
7. 数据范围：别车间用户看不到本车间的行
8. 合计是整张表（`page_size=1` 时 `row_count=3`）；十档筛选收窄
9. 算不出来的行：余额给空值不给 0，且不进金额合计
10. 清场零残留

### 5.1 真实 HTTP 冒烟（隔离实例 + 真库，非单测）

隔离实例：`profile=test`，库 `cp_m523_dev`（145），端口 8023（本机无其它实例），注入三把密钥。

夹具：成本对象 `CB-20261112-000001`，`plan_qty=9 / completed_qty=3 / wip_qty=2 / completion_pct=50%`；
本期归集 材料 100 / 人工 60 / 制造费用 40（无批次、无计算单 ⇒ 试算视图）。

`GET /api/business/erp/cost-wip-balance?period_code=2026-11&state=all` → **200**：

```
cur_total=200.0000  finished_total=135.0000  wip_total=65.0000
wip_material=40.0000  wip_labor=15.0000  wip_overhead=10.0000   element_row_count=1
row_count=1  unsettled_count=1  unbooked_total=65.0000
要素行：MATERIAL 100→完工 60 / 在制 40；LABOR 60→45 / 15；OVERHEAD 40→30 / 10
carry.state=unsettled  to_period=2026-12  source=live
```

手算对拍：材料约当 3 + 2×100% = 5 ⇒ 单位 20 ⇒ 完工 60、在制 40；加工约当 3 + 2×50% = 4
⇒ 人工单位 15（完工 45 / 在制 15）、费用单位 10（完工 30 / 在制 10）。
**期初 0 + 投入 200 − 完工 135 = 期末 65 = 40 + 15 + 10**，与 M5-22 分配表同口径。

HTTP 契约负向：

| 请求 | 期望 | 实得 |
|---|---|---|
| 缺 `period_code` | 400 + 人话 | `400 {"code":10001,"msg":"period_code（yyyy-MM）不能为空…"}` |
| `period_code=2026-13` | 400 | `400 {"code":10001,"msg":"期间编码必须是 yyyy-MM（收到「2026-13」）"}` |
| 无令牌 | 401 | `401 {"code":401,"msg":"未授权访问，请先登录"}` |
| `state=all` | 200 且 `state=all` | 200 / `"state":"all"` |

## 6. 清场

- `cp_m523_dev` 已 DROP：`select count(*) from pg_database where datname like 'cp_m523%'` = **0**
- 隔离实例已停（`ss -ltn :8023` 无监听）
- 冒烟用的临时 example（生成口令哈希）已移出仓库，**未入库**

## 7. 交接给下游（M5-24 单测：约当产量法，含多工序）

- 余额口径直接吃 M5-22 的 `elements[].wip` / 本卡 `totals.wip_*`，**不要另起公式**；
- 要验「结转」只看 `carry.state` + `diff_cost`（`carried` / `mismatch` 两档），不要手工比对计算单；
- `unsettled` 与 `blocked` 是两回事：前者有余额没入账，后者连余额都没有（金额为 `null`）。
