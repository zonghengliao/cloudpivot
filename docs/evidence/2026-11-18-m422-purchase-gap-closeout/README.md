# M4-22 采购缺口收口（计划 + 框架 + 评估 + 合同号）· 汇总证据包

- 卡：`t_562e8b43`（M4-22【M4·采购】采购缺口收口：计划 + 框架 + 评估 + 合同号）
- 上游：`t_b62c50ae`（M4-21 框架协议 + 合同关联 E2E）
- 日期：2026-11-18 · 主机：99 开发机 · 库：145:5432 模板库 `cloudpivot_test` 克隆出的演练库（脚本自建自删）
- 本卡**不写新功能**：只把 M4 的 21 张卡拼起来，证明四条主线真的填上了，并把「怎么复核」固化成一条命令。

## 一屏结论

| 面 | 结果 | 依据 |
|:--|:--|:--|
| 迁移面 | M4 段 **13 条**（V0446…V0458）全应用，且**第二遍零 ERROR**（幂等） | `01` §1 |
| 结构面 | **6 张表 + 6 个视图** + 采购单 4 个合同/协议列 + 合同对偶 CHECK + 索引 + **3 套新审批流 PUBLISHED** | `01` §2 |
| 接口面 | **40 条**范围内路由全部挂权限码；源码用到的 **26 个码**在 `sys_menu` 全有承载；**25 个按钮码**无摆设；**4 个页面码**后端零引用 | `01` §3 / `04` |
| 用例面 | 主题 **335 条**（117+101+71+20+26）+ E2E **8 条**（3+5）+ 回归 **733 条**（459+109+165），全部 `SKIP=0` | `01` §6 |
| 卫生面 | 本批 8 类业务单据 `M4xx-` **0 残留**、明细无孤儿、4 类审批无在跑 | `01` §7 |
| 门禁面 | `permission_audit_tests` 8/8 + `menu_invariants.sh` 8/8 + `fmt` 0 diff + `clippy --lib -D warnings` rc=0 | `05` |
| 空库重建 | 378 条 up 迁移在空库按版本号全过（199 表），M4 段不依赖「更晚才种下的行」 | `03` |
| 有牙 | 坏连接串下 E2E 两组分别 **3 / 5 条全失败**（拒绝静默 SKIP 假绿） | `02` |

**汇总：`PASS=78 FAIL=0 · ALL PASS`**（`01_closeout_drill.out` 尾行）。零 Rust 改动 / 零迁移 / 零权限码 / 零路由 / 零前端改动。

## 怎么复跑（一条命令）

```bash
# 99 开发机（默认连 145:5432）
bash scripts/verify/m422_purchase_gap_closeout_drill.sh

# 在 145 上跑
PG_HOST=127.0.0.1 bash scripts/verify/m422_purchase_gap_closeout_drill.sh

# 变体
TEETH=1       … 有牙自检：坏连接串必须让 E2E 两组红
FRESH=1       … 加跑空库全链重放（378 条 up 迁移，约 1-2 分钟）
SKIP_GATES=1  … 跳过 permission_audit / menu_invariants（省约 1 分钟）
KEEP_DB=1     … 留库排查（会打印库名）
```

脚本只碰「库名含 `drill`/`e2e`/`test`」的库，自己建、自己 `DROP`；连接串口令一律复用 `$PGPASSWORD` ——
URL 里写占位符会让真库用例连不上、被 `--test-threads=1` 打成 SKIP 还记 ok，那是假绿。历史同类事故真有：
M4-08 的演练把 `ERP_DB_URL` 导到了模板克隆库上，pre-push 先红后绿（见提交 `62669429`）。

## 二、缺口 → 已填（四主题逐条）

`docs/planning/20261113-新卡清单.md` 的 M4 段列了 22 张卡（计划 6 / 框架 5 + M4-11b 补 1 / 评估准入 5 / 合同号 3 / E2E 2 / 收口 1）。四主题落地状态：

### ① 采购计划（原本没有计划单）

| 缺口 | 现在有什么 | 复核 |
|:--|:--|:--|
| 没有「年度/月度 × 物料 × 仓库」的计划载体 | `biz_purchase_plan` / `biz_purchase_plan_item`（V0446），取号 `PPL`，计划量 + 已执行量 | 表/列/索引 + 计划组 117 条用例 |
| 计划不能审批、不能转申请 | `pp_approve` 流程（V0447，PUBLISHED）+ 转申请一仓一单 + 已执行量回写（V0448） | 菜单 8 行 / 路由 9 条 / 用例 |
| 看不到执行率 | `v_purchase_plan_execution` / `v_purchase_plan_item_execution`（V0449）+ 2 个只读接口 | 两视图存在 + 用例 |
| 没有页面 | `/business/erp/purchase-plan`（页面码 `…:page` 只给前端，页面 + api + 路由三件齐） | 三件齐断言；浏览器档见 `2026-11-18-m406-purchase-plan-ui/` |

### ② 框架协议（原本没有协议价 / 数量上限的定价约束）

| 缺口 | 现在有什么 | 复核 |
|:--|:--|:--|
| 没有协议载体（有效期 + 价格条款 + 数量上限 + 供应商/物料范围） | `biz_framework_agreement` / `_item`（V0450），取号 `FWA` | 框架组 101 条 |
| 下单不认协议价，可无限量超买 | 下单校验：可用性 / 单价 / 累计量 / 累计额，超限拒绝（`framework_agreement_ref` + V0451 订单侧 2 列） | 超限反例 12 条 + 变异反证 4 处 |
| 看不到协议用了多少 | `v_framework_agreement_execution` / `_item_execution`（V0452，V0453 修除零）+ 2 个只读接口 | 视图存在 + 用例 |
| 协议不能维护（CRUD / 提交审批） | V0456 + `fw_agreement` 两节点审批 + 页面写入口 | 路由 8 条 + 流程 PUBLISHED + 页面三件齐 |

### ③ 供应商评估 / 准入（原本没有四维打分与黑名单流程）

| 缺口 | 现在有什么 | 复核 |
|:--|:--|:--|
| 没有四维打分与评级 | `biz_supplier_evaluation`（V0454，质量/交期/价格/服务 + 权重，总分是**生成列**），评级 A/B/C/D 阈值唯一出处 | 评估组 71 条（含边界穷举） |
| 供应商进出没有闸 | `biz_supplier_access`（V0455）+ `supplier_access` 审批流 + 启停闸（黑名单） | 准入组 20 条 + 路由 6 条 |
| 看不到台账 / 排名 / 交期参考分 | V0457 页面接口面：台账分页 / 详情 / 打分录入 / 本期排名 / 交期建议 | 路由 5 条 + 菜单 3 行 + 页面三件齐 |

### ④ 订单挂合同号（合同额度约束）

| 缺口 | 现在有什么 | 复核 |
|:--|:--|:--|
| 采购单不知道自己执行哪份合同 | `biz_purchase_order` 的 `contract_id` / `contract_no_snapshot`（**V0198 早已落地**，M4-17 是核验卡，本批不重复加迁移）+ `ck_biz_po_contract_pair`（CONTRACT⇔有合同号 / SPOT⇔无） | 列/CHECK/索引断言 |
| 挂合同后可以买超合同额 | `guard_contract_amount`：事务内 `FOR UPDATE` 锁合同行 + 现算已用量，`after > 合同额` → 409 / 66019 | 采购域 459 条（含 8 条额度守卫真库用例） |
| 看不到合同执行了多少 | `v_contract_execution` / `v_contract_execution_order`（V0458）+ 2 个只读接口（口径与守卫逐字同源：Σ `total_with_tax`，排除作废/软删） | 合同执行组 26 条 + 路由 2 条 |

### 两条 E2E（把四主题串成业务，而不是四个孤立模块）

- **M4-20**：计划 → `pp_approve` 真批 → 转申请 → `pr_approve` 真批 → 转采购单 → `po_approve` 真批 → 建收货单 → 过账，钉住三本账（成本账不含税 / 库位账同量 / `PURCHASE_IN` 流水）+ 进项凭证 + 6 条顺序反证。
- **M4-21**：协议链 14 步（超限夹在链中间）、合同链 8 步（边界 67.80 放行 / 超限 66019 不落库 / 作废释放额度）、10 条互斥与不可用引用闸，并证明**收货过账不改协议/合同已用量**（重复计额即超卖）。

## 三、本卡新增的「拼合后一致性」审计（为什么必须做）

每张卡都验过自己那一段，但**没有人**验过拼起来之后接口面还对不对。本卡新增
`scripts/verify/m422_purchase_scope_audit.py`（离线：读源码 + 读库导出），四主题 + 合同执行 + 采购单承载面一起对拍，17 条断言（见 `04_scope_audit.out`）：

1. 每个主题的**路由条数**精确（9 / 8 / 5 / 6 / 2 / 10）；
2. **没有裸奔路由**（每条都挂 `#[nik::permission]`，无「登录即可」）；
3. **权限声明 path == 路由宏 path**（防「声明了但路由没注册」——这类错编译能过、接口 404）；
4. 每个主题的**菜单行数**精确（8 / 7 / 3 / 5 / 1）；
5. 源码用到的 **26 个码**在 `sys_menu` 全有承载（否则超管之外全 403）；
6. **25 个按钮码无摆设**（每个码至少一条路由用，杜绝死权限）；
7. **4 个页面显形码后端零引用**（AGENTS 约定 14；判据只认 `permission = "…:page"` 这种真接线，文档注释与
   `!PERMISSIONS.contains("…:page")` 反向守卫不算）；
8. 四主题菜单 `title` 全部 ≤ 12 字。

> 审计写第一版时红了三条，逐条查清都是**判据自己的毛病**而不是代码毛病：`/contract/{id}/execution` 前缀没覆盖到、
> `purchase-order` 页面码没进导出（筛漏）、页面码扫描把文档注释与反向守卫当成引用。修判据后全绿 —— 这三条也一并记在
> `04_scope_audit.out` 的注释里，防止下次有人「照着红修代码」。

## 四、数字账（基线口径）

| 分组 | 模块 | 本次实测 | 阈值 |
|:--|:--|--:|:--|
| ① 计划 | `business::erp::purchase::purchase_plan` | 117 | ≥117 |
| ② 框架 | `business::erp::purchase::framework_agreement` | 101 | ≥101 |
| ③ 评估 | `business::erp::purchase::supplier_evaluation` | 71 | ≥71 |
| ③ 准入 | `business::erp::purchase::supplier_access` | 20 | ≥20 |
| ④ 合同执行 | `business::erp::contract::contract_execution` | 26 | ≥26 |
| E2E-20 | `…::purchase_chain_e2e_tests` | 3 | ≥3 |
| E2E-21 | `…::framework_contract_e2e_tests` | 5 | ≥5 |
| 回归·采购域 | `business::erp::purchase` | 459 | ≥459 |
| 回归·合同域 | `business::erp::contract` | 109 | ≥109 |
| 回归·收货域 | `business::erp::stockin` | 165 | ≥165 |

阈值 = 本次实测（「只许涨不许降」）：任何一条掉下来都要人来说明，不允许悄悄少跑。
每组另断 `SKIP=0` —— 真库用例被「无 `ERP_DB_URL`」打成 SKIP 时 `cargo test` 仍会报 ok，那是假绿。

## 五、卫生口径（业务单据 vs 主数据，别一刀切）

- **业务单据必须 0 残留**：`biz_purchase_plan` / `biz_framework_agreement` / `biz_supplier_evaluation` /
  `biz_supplier_access` / `biz_purchase_order` / `biz_purchase_requisition` / `biz_stock_in` / `biz_contract`
  的 `M4xx-` 单号全部为 0，明细无孤儿，`PP/FWA/SUPPLIER_ACCESS/PO_APPROVE` 无在跑实例。
- **主数据不清场是设计**：物料 / 供应商夹具一律走 `test_seed::ensure_*` 共享基线（幂等 upsert，重复跑不增行）。
  本次演练库留下 20 条物料 + 12 条供应商 `M4xx-`，脚本**只报计数、不设闸**，避免把「共享基线的正常状态」判成残留。
- 模板库 `cloudpivot_test` 自带 2 条 `PR_APPROVE | PENDING`（2026-11-09 种子）与 3 条 `EXPENSE`、3 条
  `STOCK_TAKE_POST` PENDING，与本批无关：`PR_APPROVE` 按**基线值 2** 比（多一条就红），另两类不在本批断言范围。
- 模板库自带一条 `PO_APPROVE | PENDING`（订单 `W1B-PO-0001`，2026-11-10）会顶红采购域收尾用例，脚本按**单号白名单**
  清它并打印影响行数（本次 3 待办 / 4 节点 / 1 实例），别的单号一行不碰。

## 六、已知边界与遗留（诚实清单，都不是本卡要修的）

1. **3 个前端卡的浏览器档本卡没有重跑**：M4-06 / M4-11 / M4-16 的 Playwright 用例要临时后端（18101）+ vite dev（5190）
   + Chromium，本卡只复核「页面 + api + 路由登记三件齐」+ 后端接口面；浏览器证据在各自 `docs/evidence/2026-11-18-m406 / m411 / m416`。
2. **21 个历史 drill（m401…m421）没有逐个重跑**：本卡重跑的是 Rust 用例（唯一权威口径）+ 两条 E2E；历史 drill 作为按卡证据存档。
3. **`down` 迁移本卡没验**：V0446…V0458 的回滚三段只在 M4-01 卡对 V0446 做过（有数据→拒 / 清空→成功 / 再 up→复原）。
4. **AGENTS.md 与门禁脚本的口径漂移**：AGENTS.md 约定 12 写「菜单总数 400–520」，而 `menu_invariants.sh` 实际默认
   `MENU_TOTAL_MIN=400 / MENU_TOTAL_MAX=580`（脚本头注释记了调整史 420→460→520→580），演练库实测 **564 条** ——
   按文档口径已超、按脚本口径合规。**只报不改**：AGENTS.md 受保护（Agent 改它需人工批准），建议人工把该行改成 400–580。
5. **采购单两个按钮的 `title` 偏长**（`business:erp:purchase-order:status` = 22 字、
   `:submit-approval` = 26 字）：门禁第 ⑥ 条只查 `menu_type IN (1,2)`（目录/页面），按钮行不在口径内，故不违规；
   但按约定 12 的精神（title 是显示名）值得清理。属**既有域**（非本批新增），本卡只登记。
6. **`contract_id` 不是本批新增**：V0198 就已落地，M4-17 是核验卡；本批在合同号主题上的真增量是 M4-18（额度守卫）与
   M4-19（执行量统计），别把「字段早就有了」误读成「本批没干活」。
7. **框架协议 `framework_agreement_id` 没有库层 CHECK 对偶**（合同有 `ck_biz_po_contract_pair`，协议没有）：
   可用性/超限全靠 `framework_agreement_ref` 的代码闸（已有 12 条超限反例 + 10 条互斥闸兜底）。若以后有人绕过 handler
   直插订单，库层不会拦。属设计取舍，登记备查。

## 七、证据文件

| 文件 | 内容 |
|:--|:--|
| `01_closeout_drill.out` | 收口演练全量输出（`PASS=78 FAIL=0 · ALL PASS`） |
| `02_teeth_bad_url.out` | 有牙自检：坏连接串下 E2E 两组 3 / 5 条全失败（`PASS=44 FAIL=0`） |
| `03_fresh_replay.out` | 空库全链重放：378 条 up 迁移全过、199 表（`PASS=79 FAIL=0`） |
| `04_scope_audit.out` | 三向对拍审计 17 条断言全绿（路由 ↔ 权限码 ↔ `sys_menu`） |
| `05_gates.txt` | `fmt` / `clippy --lib -D warnings` / `permission_audit_tests` / `menu_invariants.sh` 原始输出 |
| `06_card_ledger.tsv` | **22 张卡账本**（卡号 / 主题 / 目标 / 提交 / 迁移号 / 主要产物 / 本卡复核口径） |

脚本：`scripts/verify/m422_purchase_gap_closeout_drill.sh`（演练）+ `scripts/verify/m422_purchase_scope_audit.py`（审计）。
