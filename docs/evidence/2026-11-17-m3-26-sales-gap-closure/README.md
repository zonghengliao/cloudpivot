# M3-26 销售缺口收口（报价 + 信用 + 返利 + 预测）· 汇总证据包

- 卡：`t_69d8e4c0`（M3-26【M3·销售】销售缺口收口：报价 + 信用 + 返利 + 预测）
- 日期：2026-11-17
- 结论：**M3-01…M3-25 共 25 张卡的四域交付全部在位并一次跑通** —— 演练 `PASS=59 / FAIL=0`；
  报价 83 + 返利 104 + 预测 24 + 信用 44 条真库用例 SKIP=0，两条 E2E 全链正向标记在位且夹具零残留，
  销售域整组 361 条回归全绿；菜单 8 条不变量、权限码 ↔ 菜单一一对应、fmt/clippy/typecheck/build 全绿；
  SKIP 守卫自检（`M326_BAD_URL=1`）如实报红（FAIL=10 / rc=1）。**零迁移、零权限码、零前端改动**。
- 本卡只做汇总与点验，不新增业务功能；两处**观察项**（上游直插夹具残留、菜单总数顶格）见 §7。

## 1. 这一卡补的是什么

M3 批是「补销售缺口」：26 张卡把**报价 / 信用 / 返利 / 预测**四个能力从零铺到可用。
到 M3-25 为止它们各自有单测、有前端、有域级演练，但**没有一张证据能同时回答**：

1. 四域的表 / 迁移 / 路由 / 权限码 / 菜单是不是**同时**都在位（不是各自在自己卡里绿）；
2. 四域的真库用例能不能在**同一张干净库上串行**跑完（共用 `biz_seq` 号段、共用审批引擎、共用客户主数据）；
3. 跑完会不会把别的域打红（销售域整组回归）；
4. 这些用例到底是**真跑**还是「连不上 → SKIP → 记 ok」（本仓历史踩过这个假绿）。

本卡的交付是第 1~4 条的**机器可复核证据**：一个演练脚本 + 一份证据包，别的不做。

## 2. 交付矩阵（M3-01…M3-25 → 提交 / 迁移 / 测试 / 前端）

| 卡 | 提交 | 交付物 | 迁移 / 权限 | 本卡点验的组织方式 |
|:--|:--|:--|:--|:--|
| M3-01 | `bd5ae28f` | `biz_quotation` + `biz_quotation_line` | V0432 | 前置断言：两表存在 |
| M3-02 | `a6cbf9c2` | 报价单 CRUD + 状态机（6 条边） | V0433（5 码 + 菜单） | `sales::quotation` 83 条 + 路由 9 条 + 6 菜单行 |
| M3-03 | `ec2955ee` | 报价单 → 销售订单（带行） | V0434（convert 码） | `quotation_convert_tests` 21 条 |
| M3-04 | `8e6e8f32` | 报价单审批流接线 | V0435（`quotation_approve`） | 前置断言：流程 PUBLISHED |
| M3-05 | `f32b1b8d` | 状态机 × 审批闸 × 转单 19 条 | — | `quotation_state_tests` |
| M3-06 | `967c7a90` | 前端：报价单列表 / 详情 / 转单 | V0436（页面行） | 菜单行 `/business/erp/quotation` |
| M3-07 | `601b5145` | 报价单 PDF 模板（复用 pdf 模块） | — | 路由 1 条 + `quotation_pdf::tests` 9 条 |
| M3-08 | `d73649d3` | 转单 21 条（含**部分转单拒绝**口径） | — | `quotation_convert_tests` |
| M3-09 | `ef88231a` | 超限策略三态 BLOCK / WARN / PASS | V0437（额度列 + CHECK） | 前置断言：`credit_policy` 列 + CHECK |
| M3-10 | `8d1e4c78` | 实时占用 / 可用额度接口 | — | 路由 `GET /credit/{customer_id}` + `credit` 44 条 |
| M3-11 | `85429d3b` | 超信用拦截（审批前闸 + 过审兜底） | — | `credit` 域 + M3-25 E2E |
| M3-12 | `46967fde` | 核销即释放（释放回执 + 真库链） | — | `credit_release_tests` |
| M3-13 | `fa7d2808` | 并发下单 / 部分收款 7 条 | — | `credit_concurrency_tests` |
| M3-14 | `26fc2370` | 前端：客户详情内嵌信用面板 | — | 前端现存，本卡零改动 |
| M3-15 | `76aa69bd` | 返利政策（按量 / 按额 / 阶梯 + 重叠闸） | V0438（2 表 + 4 码） | `rebate` 域 + 表断言 |
| M3-16 | `ce2d1a93` | 返利结算（月度 / 季度，政策快照） | V0439 | `rebate_settle_db_tests` 12 条 |
| M3-17 | `a5f4ef14` | 返利红字发票（`OUT_RED` 票 + 红字凭证） | V0440 / V0445（贷 2241.05） | `rebate_invoice_db_tests` 9 条 + 路由 2 条 |
| M3-18 | `c88b83eb` | 三种规则 × 边界矩阵 13 条 | — | `rebate_rule_matrix_tests` |
| M3-19 | `b255d1b2` `f51530d2` `393c3b82` `ccae7514` | 前端返利两页 + 红字发票入口 + 浏览器冒烟 | V0443（页面行 sort 85） | 菜单行 `/business/erp/rebate-policy` |
| M3-20 | `d16f2d57` | 预测表补全（`forecast_qty` / 版本链 / 当前版本视图） | V0444 | 11 条 + 视图 / 两列断言 |
| M3-21 | `c444cc68` | 预测 vs 实际（四粒度 + 漏预测行 + MAPE） | 复用 `sales-forecast:list` | `sales_variance_db_tests` 7 条 + 路由 1 条 |
| M3-22 | `29001e93` | 汇总口径单测（10 纯逻辑 + 6 真库） | — | `sales_variance_summary_db_tests` 6 条 |
| M3-23 | `f89a6b4f` | 前端：第 4 个页签「预测 vs 实际」 | — | 前端现存，本卡零改动 |
| M3-24 | `efb06b72` | E2E：报价 → 审批发出 → 接受 → 转订单 → 发货 → 过账 → 签收 | — | `sales_chain_e2e_tests` 2 条（strict 残留） |
| M3-25 | `83f4878f` | E2E：信用占用 → 拦截 → 调额 → 核销释放 → WARN/PASS → 审批闸 → 兜底 | — | `credit_control_e2e_tests` 2 条（strict 残留） |
| **M3-26** | 本卡 | **汇总演练脚本 + 证据包**（零业务改动） | **零迁移 / 零码** | `m326_sales_gap_closure_drill.sh` |

## 3. 四域实测基数（空库首启 + V0405…V0445 后实查，可复核）

| 维度 | 实测值 | 复核方式 |
|:--|:--|:--|
| 表 | **6 张**：`biz_quotation`、`biz_quotation_line`、`biz_sales_rebate_policy`、`biz_sales_rebate_rule`、`biz_sales_rebate_settlement`、`biz_sales_forecast` | §4 步骤 2 的 `to_regclass` 断言 |
| 视图 | **1 个**：`v_biz_sales_forecast_current`（M3-20 当前版本口径） | 同上 |
| 客户信用列 | `credit_limit` / `credit_used` / `credit_days` / `credit_policy`（白名单 CHECK） | 同上 |
| 路由（宏内，不含 `/api`） | **29 条**：报价 9（含 PDF）/ 返利政策 5 / 返利结算+红字发票 9 / 预测 5（含 variance）/ 信用快照 1 | §4 步骤 3c 逐域钉死 + `01_route_inventory.txt` |
| 权限码 | **23 个**：四域专属 18（报价 6 / 返利政策 4 / 返利结算 4 / 预测 4）+ 信用复用 `customer:list` + 预测所在页 `sales-target` 4 | §4 步骤 3（源码解析 → 逐个查 `sys_menu`） |
| 菜单行 | **18 行**：报价页面 1 + 按钮 5；返利页面 1 + 按钮 3 + 4；预测按钮 4 | `03_menu_inventory.txt` |
| 用例 | **405 条**（销售域 361 + 信用域 44），其中四域定向组 259 条，另有两条 E2E 4 条重叠在销售域内 | §4 步骤 5 |

## 4. 闭环验证：`scripts/verify/m326_sales_gap_closure_drill.sh`（PASS=59 / FAIL=0）

演练库一律**克隆 → 跑 → DROP**（库名含 `e2e`，脚本自己清），完整输出见 `04_drill_full.out`。

| 步骤 | 断言内容 | 结果 |
|:--|:--|:--|
| 0 | 模板库存在 → 克隆 `cp_m326_sales_gap_e2e_*` | 2 PASS |
| 1 | 补 V0405 之后 **41 条**业务迁移（逐条 `ON_ERROR_STOP`，幂等） | 1 PASS |
| 2 | 四域 6 表 / 1 视图 / `forecast_qty` / `revised_from_id` / 信用 4 列 + CHECK / 两条审批流 PUBLISHED | 13 PASS |
| 3 | 源码解析出的 **23 个权限码**逐个在 `sys_menu` 命中 **1 行**（无孤儿码 / 无重码） | 2 PASS |
| 3b | 报价页面行 + 5 按钮、返利页面行 + 3 + 4 按钮、预测 4 按钮、按钮不悬空、title ≤ 12、同父 sort 唯一 | 9 PASS |
| 3c | 路由条数逐域钉死：9 / 5 / 9 / 5 / 1（源码宏，丢接口立刻红） | 5 PASS |
| 4 | `menu_invariants.sh` 8 条断言（孤儿 parent / 归属 / path 唯一 / 总数 / title / sort / icon） | 1 PASS（总数 540，区间 400–540） |
| 4b | 四域菜单 + 表清单落盘（`03_menu_inventory.txt`） | 1 PASS |
| 4c | 夹具基线快照 | — |
| 5 | 见下表 8 个测试组，每组「全绿 / SKIP=0」两条 + 两条 E2E 正向标记 + 夹具增量 | 21 PASS（5 个域级组的夹具增量按设计只报告，E2E 与伞形组判 0） |
| 6 | 夹具：E2E 组 strict 净增量 0；M324/M325 客户/订单/账号/台账 0 残留 | 4 PASS |
| 7 | 汇总 | **PASS=59 FAIL=0 / ALL PASS** |

**测试组（`--test-threads=1 --nocapture`，真库）**

| 组 | 条数 | 覆盖 |
|:--|--:|:--|
| `business::erp::sales::quotation` | 83 | 状态机 19 + 转单 21 + model 16 + handler 13 + PDF 9 + 审批桥 5 |
| `business::erp::sales::rebate` | 104 | 政策 13 + 规则矩阵 13 + 结算 12/12 + 红字发票 9 + dto/model/handler 45 |
| `business::erp::sales::sales_forecast_db_tests` | 11 | 预测数量 / 版本链 / 当前版本视图 |
| `business::erp::sales::sales_variance` | 13 | 偏差 7 + 汇总口径 6 |
| `business::erp::credit` | 44 | 占用 / 拦截 / 释放 / 并发 / 快照 |
| `business::erp::sales::sales_chain_e2e_tests` | 2 | M3-24 全链（正向标记 `[M3-24]`，strict 夹具） |
| `business::erp::sales::credit_control_e2e_tests` | 2 | M3-25 全链（正向标记 `[M3-25] 全链通过`，strict 夹具） |
| `business::erp::sales`（伞形回归） | 361 | 订单 / 报价 / 退货 / 目标 / 返利 / 预测整域 |

**为什么「SKIP=0 + 正向标记」两条都要**：真库用例在「连不上」时打印 `SKIP:` 后早退，
Rust test harness 仍记 `ok` —— 只数 `passed` 会把假绿看成全绿。所以每组既数 `SKIP:` 行
（**不带行首锚点**，`--nocapture` 下测试名与 SKIP 同行），又要求 E2E 组出现步骤轨迹标记。

## 5. 有牙证明：`M326_BAD_URL=1`（把连接串写坏）

```
$ M326_BAD_URL=1 bash scripts/verify/m326_sales_gap_closure_drill.sh
  FAIL 报价域…：本组出现 4 行 SKIP（真库用例被跳过 = 假绿）
  FAIL 返利域…：本组出现 1 行 SKIP
  FAIL 预测域…（两组各 1 行 SKIP）
  FAIL 信用域…：本组出现 3 行 SKIP
  FAIL E2E：报价单 → 订单 → 出库（M3-24）：缺正向标记「[M3-24]」
  FAIL E2E：信用控制超限全链（M3-25）：缺正向标记「[M3-25] 全链通过」
  FAIL 销售域整组…：本组出现 11 行 SKIP
== 汇总：PASS=55 FAIL=10 ==   →  rc=1
```

完整输出见 `05_skip_guard_teeth.out`。演练库照样 DROP（`cp_m326%` 计数 0，见 §8 最后一条）。

## 6. 门禁

| 门禁 | 命令 | 结果 |
|:--|:--|:--|
| 后端格式 | `cargo fmt -- --check` | rc=0 |
| 后端 lint | `cargo clippy --lib -- -D warnings` | rc=0（0 告警） |
| 权限审计 | `cargo test --lib system::menu::permission_audit_tests` | **8/8 pass**（N14/N15/N16/N17/N18/N19/N20/N21） |
| 菜单结构 | `bash scripts/verify/menu_invariants.sh <演练库>` | **8 条全绿**（演练内跑，见 §4 步骤 4） |
| 前端类型 | `pnpm typecheck` | rc=0 |
| 前端构建 | `pnpm build` | 见 `06_gates.out` |

原始输出合并见 `06_gates.out`。

## 7. 剩余缺口与观察项（如实）

### 7.1 观察项（P3）：上游「域级单测」直插夹具不自清场

同一张演练库串行跑完，客户表多出 **56 个 `M3xx-` 夹具客户**（`M302/M303/M312/M313/M315×12/M316×8/M317×8/M320×10/M321×3/M322×4`），
报价单多 4 张、销售订单多 1 张。两条 E2E（M3-24/25）与销售域整组**净增量均为 0**（自己收干净）。
逐组数字与完整编码见 `07_fixture_residue.txt`。
影响：演练库用完即删 → **生产零影响**；但把不同域单测塞进同一个共享库连跑时，客户表会看到这些 `M3xx-` 客户。
处置：本卡按「只做汇总」不修上游 8 个测试文件，已开 follow-up 卡 **t_20e958fc**（M3-27，后端）做清场收口，
验收口径就是本演练脚本的「全演练净增量为 0」。

### 7.2 风险（P2，需要决策）：菜单总数 **540 / 540**，正好顶格

`menu_invariants.sh` 第 ⑤ 条区间是 `[400, 540]`（2026-11-16 因仓库域三族落地从 520 上调到 540），
本次实测**正好 540** —— 再加任意一条业务菜单就会报红。这不是功能缺口，是**门禁阈值与菜单增量的节奏问题**：
下一个模块（如 M4 采购计划）落地前，要么按「真实功能增长」同步上调并写清调整史，
要么先合并 / 清理冗余菜单行。建议由架构/治理侧定口径，本卡只报数。

### 7.3 按设计不做的事（不是缺口，避免被当 bug「修」）

| 项 | 决策与依据 |
|:--|:--|
| 返利 **不生成应付单**，走红字发票 | M3-17 定稿口径（`V0440` 头注）；`V0445` 只把贷方科目从被佣金占用的 `2241.02` 分出 `2241.05`，历史凭证不追溯 |
| 报价单转单**只支持整单**转 | M3-08 卡面点名的「部分转单」落点是**拒绝口径**（`quotation_convert_tests` 头注） |
| 信用**没有独立菜单页** | M3-14 是客户详情内嵌面板（`CreditPanel.vue`），额度/占用/可用同屏，不需要侧栏入口 |
| 预测**没有独立菜单页** | M3-23 是「销售目标」页的第 4 个页签（`ForecastVariancePanel.vue`），权限挂在 `sales-target` 页面下 |
| `sales-forecast:variance` 不新增权限码 | M3-21 复用 `business:erp:sales-forecast:list`（同一页签内的派生视图） |

## 8. 复现

```bash
# 99 → 145（默认）；在 145 上跑加 PG_HOST=127.0.0.1
bash scripts/verify/m326_sales_gap_closure_drill.sh                 # 期望 PASS=59 FAIL=0 / ALL PASS
M326_BAD_URL=1 bash scripts/verify/m326_sales_gap_closure_drill.sh  # 期望 FAIL=10 / rc=1（有牙）
KEEP_DB=1 bash scripts/verify/m326_sales_gap_closure_drill.sh       # 留库排查（打印库名，自己 DROP）

# 单独看某一段
bash scripts/verify/menu_invariants.sh <库名>
cd backend && cargo test -p cloudpivot --lib business::erp::sales -- --test-threads=1
```

演练库只建在 **145**（`172.18.10.145`），库名必须是 `cp_m326_*e2e*` 形态，脚本自己 DROP；
`psql -h 145 -U cloudpivot -d postgres -tAc "select count(*) from pg_database where datname like 'cp_m326%'"` 应为 0。

## 9. 交付物

| 文件 | 作用 |
|:--|:--|
| `backend/…/sales`（未改） | 本卡零业务改动 —— 四域代码全部是 M3-01…M3-25 的既交付 |
| `scripts/verify/m326_sales_gap_closure_drill.sh`（新） | 汇总演练：克隆 → 补迁移 → 四域前置 + 权限码/菜单/路由条数 → 菜单 8 条不变量 → 8 组用例 → 夹具增量 → PASS/FAIL |
| `docs/evidence/2026-11-17-m3-26-sales-gap-closure/01_route_inventory.txt` | 四域 29 条路由（方法 / 路径 / 权限码 / handler / 源码行，源码解析生成） |
| `…/02_migrations.txt` | 四域 12 条迁移 → 卡 → 落地内容 |
| `…/03_menu_inventory.txt` | 演练库实查的四域菜单行 + 菜单总数 + 四域码数 + 表/视图清单 |
| `…/04_drill_full.out` | 演练全量输出（PASS=59 / FAIL=0） |
| `…/05_skip_guard_teeth.out` | SKIP 守卫自检（PASS=55 / FAIL=10 / rc=1） |
| `…/06_gates.out` | fmt / clippy / typecheck / build / 权限审计 原始输出 |
| `…/07_fixture_residue.txt` | 上游直插夹具残留逐组数字 + 56 个编码 |
| 上游证据（不复制，指针） | `docs/evidence/2026-11-17-m3-2{0,1,2,3,4,5}-*` 六份原卡证据 |
