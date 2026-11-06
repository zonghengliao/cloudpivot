# SO-23 销售链不变量（S1–S8）交付与实测证据

- 卡：`t_f21b6927`【SO-23】销售链不变量（8 条）
- 口径来源：`docs/architecture/ARCH-7-销售模块设计.md` §9.2（逐字对号）
- 改动主体：`scripts/verify/erp_invariants.py` 新增 `group_s()`（组名「销售」，8 条，全 BLOCKER）
- 产品代码零改动；**没有**新增迁移（原因见 §5）

---

## 1. 交付清单（改了哪些文件 / 函数）

| 文件 | 性质 | 内容 |
|---|---|---|
| `scripts/verify/erp_invariants.py` | 修改 | 新增 `S. 销售链不变量` 整组：常量 `SALES_*`、`S_CHECK_NAMES`、`group_s()`；`GROUPS` 增加 `"销售": group_s`；顺手把 H6 的静态扫描改为**忽略 `--` 注释**（`_blank_sql_comments()`，见 §6） |
| `scripts/verify/so23_fresh_db.sh` | 新增 | 建「空库 + 全迁移」基线库 `cloudpivot_so23fresh`（真后端跑迁移，端口 18223） |
| `scripts/verify/so23_sales_fixture.sql` | 新增 | 正向夹具：一条**完整且自洽**的销售链（1 单/2 行/2 发货/2 出库/2 应收/1 收款/1 发票/5 凭证/11 分录 + 三本账） |
| `scripts/verify/so23_sales_drill.sql` | 新增 | 负向演练：对正向夹具逐条注入 8 处违约（S1..S8 各 1 处） |
| `scripts/verify/so23_sales_drill.sh` | 新增 | pos/neg 两个库的建库 + 夹具 + 巡检一键跑 |
| `scripts/verify/so23_accept.sh` | 新增 | 验收脚本：干净库连跑两轮 + 正向 + 负向 + 会计组回归，日志落本目录 `raw/` |
| `scripts/verify/so23_pflow1a_probe.sh` | 新增 | §5 的探针：用隔离库跑 `pflow1a::publish_regression_keeps_graph_hash` |
| `scripts/verify/so23_dbsetup.sh` / `so23_pick_db.sh` / `so23_schema_dump.sh` | 新增 | 建库 / 选库 / 打表结构的辅助脚本 |

`group_s()` 内部结构（每条一个 `check(...)`，失败带「差多少 + 定位 SQL」）：

| 检查 | 判据（数据库真相） | 备注 |
|---|---|---|
| S1 | `total_with_tax = total_amount + total_tax`；**且** 明细 `Σamount / Σtax_amount = 头` | 头恒等有 `ck_sales_order_amount` 兜底；**明细汇总无 CHECK 兜底**（CHECK 不能跨表） |
| S2 | 每行 `0 ≤ delivered_qty ≤ qty`（`delivered_qty` 本身就是该行累计已发，即 §9.2 的 `sum(delivered_qty) <= qty`） | 库层有 `ck_sales_order_item_delivered`；巡检防「约束被摘掉/老库」 |
| S3 | `total_with_tax = amount + tax_amount`（正票 OUT 与红字 OUT_RED 同一判据） | 库层有 `ck_biz_invoice_amount` |
| S4 | 每张应收 `0 ≤ received_amount ≤ amount` | 库层有 `ck_biz_ar_amount` |
| S5 | 每张收款 `allocated_amount ≤ amount`；**且** `Σ核销明细 = allocated_amount` | 后者**无 CHECK 兜底**（跨表求和） |
| S6 | 每张 `source_type='SALES'` 且已过账未作废的出库单必有 `source_type='SALES_OUT'`、`source_id=该出库单`、未作废的应收；反向：SALES_OUT 应收不许挂空 | 见 §3 的两个口径坑（不是 `out_type`、赠品免挂应收） |
| S7 | 销售相关凭证（科目 1122/2221.02/5001/6401 **∪** 来源 STOCK_OUT/RECEIPT/INVOICE/SALES_OUT/RECEIVABLE）`Σ借 = Σ贷`，且头金额 = 分录取和 | A1/A2 是全账口径；S7 是销售口径，红了能直接指向销售链 |
| S8 | 受限(物料×仓库)集合（销售出库流水覆盖到的）：`库存 = 库位账 = 流水`；**另一轴**：出库单明细 `Σqty = 该单流水净额` | B1/B4 是全库口径；S8 按销售口径收窄 + 加了单据级半截过账判据 |

**可观测性**：每条前面打一行 `· 覆盖：…`（实际查了多少张单/多少行）。干净库上 8 条会「绿色地空跑」，这行让「绿」和「没查」在报告里长得不一样。

---

## 2. 正向实测：完整销售链 → S1–S8 全绿（**非空覆盖**）

```
bash scripts/verify/so23_sales_drill.sh pos
```

```
  · 覆盖：销售订单 1 张 / 明细 2 行
  ✅ [销售] S1 销售订单 价税合计 = 不含税 + 税额（且明细 Σ = 头）
  · 覆盖：订单明细 2 行，其中已发货 2 行
  ✅ [销售] S2 订单发货一致性 0 ≤ delivered_qty ≤ qty
  · 覆盖：发票 1 张（其中红字 0 张）
  ✅ [销售] S3 发票 价税合计 = 不含税 + 税额
  · 覆盖：应收 2 张，其中已收款 1 张
  ✅ [销售] S4 应收核销 0 ≤ received_amount ≤ amount
  · 覆盖：收款 1 张 / 核销明细 1 行
  ✅ [销售] S5 收款核销 allocated_amount ≤ amount 且 Σ核销 = 已核销
  · 覆盖：已过账销售出库 2 张（赠品/零价免挂应收 0 张） / SALES_OUT 来源应收 2 张
  ✅ [销售] S6 销售出库 ↔ 应收勾稽（已过账出库必有应收）
  · 覆盖：全账凭证 5 张，其中销售相关 4 张
  ✅ [销售] S7 销售相关凭证 Σ借 = Σ贷（且头金额 = 分录取和）
  · 覆盖：销售出库涉及的 物料×仓库 2 组 / 已过账出库单 2 张
  ✅ [销售] S8 三账一致（销售出库：库存 = 库位账 = 流水）

结果：8/8 通过   🚨 BLOCKER 0   ⚠️ WARN 0
```

同一夹具库跑**全量**：`73/76 通过 / BLOCKER 1 / WARN 2`，非绿的 3 条与干净库**完全同一条**（H6 / H5 / N2，见 §4）——说明夹具本身自洽（B1/B2/B4/B5/C1/D1/E4 等旧判据全绿，不是靠脏数据换来的绿）。原始日志：`raw/pos-full.log`。

---

## 3. 负向实测：逐条注入违约 → S1–S8 **全部变红**

```
bash scripts/verify/so23_sales_drill.sh neg
```

```
结果：0/8 通过   🚨 BLOCKER 8   ⚠️ WARN 0
```

逐条失败明细（节选，完整见 `raw/neg-sales-group.log`）：

| 检查 | 注入的违约 | 巡检报出的证据 |
|---|---|---|
| S1 | 明细 `amount` 1000→900 | `SO23-SO-0001 明细 1900.0000 / 头 2000.0000` |
| S2 | `delivered_qty` 5→6（先摘 `ck_sales_order_item_delivered`） | `SO23-SO-0001 行2 | qty=5.0000 | delivered=6.0000` |
| S3 | 发票 `total_with_tax` 1130→1140（先摘 `ck_biz_invoice_amount`） | `SO23-IV-0001 OUT | 1000.0000 | 130.0000 | 1140.0000` |
| S4 | 应收 `received_amount` 565→2000（先摘 `ck_biz_ar_amount`） | `SO23-AR-0001 | 1130.0000 | 2000.0000` |
| S5 | 已核销 565→700 且核销明细 565→500（先摘 `ck_biz_receipt_amount`） | `SO23-RC-0001 | 565.0000 | 700.0000` + `Σ明细 500 ≠ 已核销 700` |
| S6 | 软删第 2 张出库的应收 | `已过账销售出库缺应收 1 张 · SO23-OUT-0002` |
| S7 | 收入凭证借方 1130→1200 | `SO23-V-INC-1 | Σ借 1200.0000 / Σ贷 1130.0000`；`头 1130 ≠ Σ借 1200` |
| S8 | 库位账 10→9；第 2 张出库流水 5→4 | `M-W1A-001 库存10/库位9/流水10`；`M-W1A-002 15/15/16`；`SO23-OUT-0002 明细5 ≠ 流水4` |

> 关于 4 处 `ALTER TABLE … DROP CONSTRAINT`：S2/S3/S4/S5a 在库层本来就有 CHECK，**正常路径写不进坏值**。演练先摘约束再写坏值，为的是验「巡检自己能不能抓到」，而不是验「约束能不能挡住」（后者是迁移自测的事）。这也正是 `erp_invariants.py` 文件头列的「老库/约束被摘掉」情形②。

**副产品（真实库上抓到的真问题）**：把同一套 S1–S8 打到 SO-14/SO-10 的 e2e 夹具库（`cloudpivot_so23`，从 `cloudpivot_so22fresh2` 复制）上，S1 抓到 2 张订单**头金额=0 而明细有金额**（`SO14T-SO-74bb7c18ee3c`、`SO14T-SO-e437f3b972f8`），S6 抓到 5 张 `SALES_OUT` 应收**挂空**（来源出库单在库里不存在）。都是夹具直写绕过 handler 的产物 —— 证明这两条不是恒绿的装饰。

---

## 4. 验收口径与「全绿」的差距（诚实报告）

```
bash scripts/verify/so23_accept.sh
```

| 场景 | 结果 | 说明 |
|---|---|---|
| 干净库 `cloudpivot_so23fresh` 轮 1 | **73/76 通过 / BLOCKER 1 / WARN 2** | 本卡新增 8 条全绿 |
| 干净库轮 2（同库再跑） | **73/76 通过 / BLOCKER 1 / WARN 2** | 两轮逐字节一致（`raw/round1-vs-round2.diff` 为空） |
| 销售组（干净库） | **8/8 绿** | 覆盖全 0 行（空库） |
| 销售组（正向夹具库） | **8/8 绿** | 覆盖非空（§2） |
| 销售组（负向演练库） | **0/8（BLOCKER 8）** | 逐条都抓得到（§3） |
| 会计组（干净库，旧组回归） | **5/5 绿** | 旧判据未受影响 |

**卡面验收「48 条全绿 + BLOCKER 0」在干净库上还差 3 条，且 3 条都不是本卡引入的**（改动前后逐条一致；改动前是 `65/68`，这 3 条同样红）：

| 检查 | 级别 | 根因（已定位到行） |
|---|---|---|
| H6 预置流程种子必须显式写 `is_system` | **BLOCKER** | `V0281__sales_order_flow.up.sql:88` 的 `INSERT INTO biz_flow_def` 列清单止于 `owner_dept_id`，漏写 `is_system` ⇒ 吃 DEFAULT FALSE，且之后无补偿迁移 |
| H5 名字含「标准」必须 `is_system=TRUE` | WARN | 同一条：`so_approve`（销售订单审批（标准·金额分级））实际 is_system=FALSE |
| N2 在用 HUMAN 账号必须有部门 | WARN | 干净库 `sys_dept` **一行为空**（组织树未落地），`admin` 无部门可落；该检查自己注明「身份未确认期间长期亮，升级是 C 卡的事」 |

### 为什么本卡**没有**顺手补 V0298 修 H6（重要，别重犯）

试过，两个缺陷是**连体**的，必须一起修，否则只是把红挪个地方：

1. 新增 `V0298`（照 V0220/V0233 的先例 `UPDATE biz_flow_def SET is_system=TRUE WHERE flow_code='so_approve'`）→ H6/H5 **变绿**，干净库 75/76、BLOCKER 0；
2. 但 `so_approve` 一进 `is_system=TRUE`，就落进 `backend/tests/pflow1a.rs::publish_regression_keeps_graph_hash` 的遍历（该用例按 `is_system = TRUE AND status='PUBLISHED'` 取存量流程，要求「另存草稿 → 重新发布」后 `graph_hash` 逐字节不变）。实测**当场红**：

   ```
   assertion `left == right` failed: so_approve 重新发布后 graph_hash 变了
   （da3c2eed…fa9a2 → 97fe9377…6e80）：说明快照口径被改动，存量 PUBLISHED 版本全部失效
   ```
3. 根因查清了：`biz_flow_def_version` 两个版本的 `graph_snapshot` **长度同为 5008**，但 **edges 顺序不同**（v1 以 `e_finance_mid` 打头，v2 以 `e_low` 打头）—— V0281 种下的快照边序与当前 canonical 序列化不一致 ⇒ 存量 hash 不可重放。修它得**用应用的 canonical 序列化重新生成该版本的 `graph_snapshot` + `graph_hash`**（后端的事，不是巡检脚本能修的）。
4. 于是**回退 V0298**：本卡只交付「新增 8 条销售不变量，不破坏既有判据」，H6/H5 与 `so_approve` 快照不可重放打包进后续卡（见卡内汇报的 follow-up）。`raw/pflow1a-baseline.log` 是回退后的对照：同一隔离库上该用例 `1 passed`，证明失败确实由 V0298 引入。

---

## 5. 迁移 / 产品代码

- **未新增迁移**：8 条不变量只读库、不建表（`V0298` 试做后已回退，文件不在工作区）。
- 迁移口径自查：基线库 `schema_migrations` 225 条，`applied_via` 全部 `executed`（非 `executed/imported` 计数 = 0）。
- 产品代码（Rust / 前端）零改动 ⇒ 无 `cargo test` 新增面、无 `pnpm typecheck/build` 面、无 API 改动（因此无 curl 实测项）。

## 6. 顺手加固：H6 静态扫描忽略 SQL 注释

写补偿迁移时踩到：H6 用正则扫 migrations 里的 `INSERT INTO biz_flow_def (…)`，**连注释里的示范写法也算**。V0298 的表头注释里写了一句「扫 migrations 的 `INSERT INTO biz_flow_def (…)` 列清单」，H6 当场报 `flow_code=(未解析)` 假红；反向更危险 —— 注释里写一句 `UPDATE biz_flow_def … is_system = TRUE` 就能把真实的漏列当成「已被补偿」静默放过。

修法：`_blank_sql_comments(text)` 把 `--` 行注释**按等长空格抹掉**（行号不漂），`_flow_def_inserts()`、隐式列清单扫描、`_compensating_migration()` 三处都改用抹过的文本。判据强度不变（该红的照样红：§4 的 H6 依旧红）。

## 7. 复现命令（全部真库）

```bash
# ① 基线库：空库 + 全部迁移（真后端跑迁移，端口 18223，跑完即停）
bash scripts/verify/so23_fresh_db.sh up

# ② 正向 / 负向演练
bash scripts/verify/so23_sales_drill.sh both

# ③ 验收：干净库连跑两轮 + 正向 + 负向 + 旧组回归（日志落本目录 raw/）
bash scripts/verify/so23_accept.sh

# ④ 手动指定库跑某一组
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_DATABASE_NAME=cloudpivot_so23fresh python3 scripts/verify/erp_invariants.py --group 销售
```

环境：本机 PG `127.0.0.1:5432`（账号 `alun`），后端 `backend/target/debug/cloudpivot`（应用 225 条迁移）；库 `cloudpivot_so23fresh`（基线）/ `cloudpivot_so23pos`（正向）/ `cloudpivot_so23neg`（负向）/ `cloudpivot_pflow1a`（§4 探针）。

## 8. 遗留 / 下一步

1. **H6（BLOCKER）+ H5**：按 V0220/V0233 先例补 `is_system=TRUE` 的补偿迁移，**同时**修 `so_approve` 版本的 `graph_snapshot` 边序（canonical 序列化重生成 `graph_snapshot` + `graph_hash`），否则 `pflow1a` 会红。两件事必须在同一张卡里做。
2. **N2（WARN）**：干净库无 `sys_dept` 行；要全绿得先落地组织树（`scripts/datafix/org_landing/`）或让 N2 对「完全没有组织树」的库降级成 INFO（当前是 WARN）。
3. **SO-24**（子卡）：按 ARCH-7 §12.2 走 10 步 API 全链后，直接跑 `--group 销售` 即可验收；届时 S1/S6/S8 会拿到真实 API 产生的非空覆盖。
