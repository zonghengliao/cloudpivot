# COST-B4-2 成本对象 ↔ 出库/入库反查（「这个批的料从哪来、货去哪了」）

卡：`t_2f5940fc` · 批次 B4 · 依据 `docs/architecture/ARCH-9-成本核算升级设计.md` §8.1（B4 卡清单）
与 §7.4（分批上线门槛）。
前置：B3-11（前端成本核算页，本卡复用它建立的详情抽屉与 `shared/source-route` 跳转表）。

## 一句话

成本对象详情页新增一块**反查**：左边「关联领料单」（料从哪来）、右边「完工入库单」（货去哪了），
每行单号可点跳到原单据，顶部 **5 条现算判据**把「两条链对不对得上」逐条打勾/点名，
合计全部由后端**逐行相加**（不读单头快照、不在前端重算）。

## 交付物

| 文件 | 作用 |
|---|---|
| `backend/src/business/erp/cost_object_dto.rs` | 报文契约（`CostObjectSourcesRes` / `SourceDocRes` / `SourceTotals` / `SourceCheck`）+ **5 条判据的纯函数** `build_source_checks`；3 条新单测 |
| `backend/src/business/erp/cost_object_model.rs` | 两条反查链的取数：`source_issues`（出库单+明细+归集）、`source_receipts`（`in_type='PRODUCTION'` 入库单+明细+结算批次）、`collected_material_of`、`settled_output_of` |
| `backend/src/business/erp/cost_object_handler.rs` | `GET /business/erp/cost-object/{id}/sources`（权限码复用 `cost-object:list`）+ `to_sources_res` 纯函数 + 3 条单测（含报文形状） |
| `frontend/src/api/business/erp/cost-object-kinds.ts` | 反查报文类型 + 单据类型/状态译名表 |
| `frontend/src/api/business/erp/cost-object.ts` | `fetchCostObjectSources(id)` |
| `frontend/src/views/business/erp/cost-object/cost-object-logic.ts` | 8 个纯逻辑函数：状态/类型译名、归集情况文案与配色、跳转目标（走 `shared/source-route`）、判据汇总、标题总述 |
| `frontend/src/views/business/erp/cost-object/index.vue` | 详情抽屉新增「料从哪来 / 货去哪了」：标题总述 + 判据栅格 + 两张表 + 刷新 + 空态/红冲提示 |
| `frontend/tests/unit/erp-cost-object-source.test.ts` | 8 条纯逻辑单测 |
| `docs/evidence/2026-11-11-cost-b4-2-cost-object-sources/` | 本目录：验收脚本、原始报文、截图、门禁输出 |

## 接口契约

```
GET /api/business/erp/cost-object/{id}/sources     权限码：business:erp:cost-object:list
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `object_id` / `object_no` / `product_name` / `status` | string | 与详情接口同源（同一行 `biz_cost_object`） |
| `completed_qty` / `wip_qty` | string | 对象累计完工 / 期末在制（用户填的值） |
| `issues[]` | `SourceDocRes` | 挂本对象的领料出库单（**含草稿**，按出库日期升序） |
| `issue_totals` | `SourceTotals` | 列出的全部领料单合计（= 各行相加） |
| `issue_posted_totals` | `SourceTotals` | 只算 `POSTED`（参与归集的那些） |
| `receipts[]` | `SourceDocRes` | `in_type='PRODUCTION'` 的完工入库单（**含被红冲作废的**） |
| `receipt_totals` | `SourceTotals` | 只算 `POSTED`（`CANCELLED` 不计量不计钱） |
| `receipt_cancelled_count` | number | 被红冲作废的张数（>0 时页面顶部提示） |
| `checks[]` | `SourceCheck` | 5 条判据：`key/label/left_label/left/right_label/right/diff/ok/hint` |
| `all_ok` | bool | 5 条是否全绿 |

5 条判据（容差 `0.0001`，与巡检红色阈值同口径）：

| key | 左 | 右 |
|---|---|---|
| `issue_collected_amount` | 已过账领料明细金额 | `biz_cost_collect` 料行金额 |
| `issue_collected_qty` | 已过账领料明细数量 | `biz_cost_collect` 料行数量 |
| `receipt_settled_qty` | `POSTED` 完工入库数量 | 已结算（`SETTLED`）计算单累计完工数量 |
| `receipt_settled_amount` | `POSTED` 完工入库金额 | 已结算计算单完工转出成本 |
| `issue_all_collected` | 未（足额）归集的领料单张数 | 0 |

失败口径：对象不存在/已软删 → **404**；`id` 非 UUID → **400**；查库失败 → 500。

## 三个口径（为什么这么取数）

1. **不读单头合计**：`biz_stock_out.total_qty/amount` 是过账那一刻的快照，明细改过就滞后。
   这里逐行 `SUM(明细)`，于是「合计 = 表格里各行相加」恒成立。
2. **作废单要列出来但不计合计**：红冲把完工入库单打成 `CANCELLED`，页面必须看得到「这批货被红冲了」；
   而「已结算计算单」只数 `SETTLED` —— 红冲会把计算单行打回 `CALC`，两条链**同时**退出合计，红冲后依然对得平。
3. **两个 `GROUP BY` 子查询各自聚合再 join**：明细与归集行是 1:N，直接 join 会把金额乘起来。

## 验收对照（卡面三条）

| 卡面验收 | 证据 |
|---|---|
| 详情页可看到关联领料单列表 + 完工入库单列表 | `10_detail_sources.png`：领料 1 张（E2E-OUT-0001，1000 / 19,000.00，已归集 · 2026-11）+ 入库 2 张（1 张 `已作废`、1 张 `已入账`，各 800 / 22,311.11，带结算批次号） |
| 每行可点跳到原单据 | `11_jump_stock_out.png`（出库单页按 `?out_no=E2E-OUT-0001` 筛到那一张，详情抽屉里「成本对象 E2E-CB-0001 / 过账借方 4001.01」）、`12_jump_stock_in.png`（收货入库页按 `?in_id=` 直开 `IN-20261111-000004` 详情，备注写着「完工入库 · 结算批次 CS-202611-0004 · 成本对象 E2E-CB-0001」） |
| 数量与金额合计对得上 | `02_sources_check.out`：**64 条断言全过 / 0 失败** —— 合计=各行相加、合计=库明细汇总、5 条判据左右值=库真值（判据① 19000.0000=19000.0000；③ 800=800；④ 22311.1104=22311.1104） |

## 实测输出

| 项 | 命令 | 结果 |
|---|---|---|
| 后端类型检查 | `cargo check --all-targets` | `Finished dev profile` exit 0（零警告） |
| 后端单测 | `cargo test --lib cost_object` | **97 passed / 0 failed**（本卡新增 6 条：3 dto + 3 handler） |
| 前端类型检查 | `pnpm typecheck` | exit 0（`06_typecheck.out`） |
| 前端构建 | `pnpm build` | 见 `07_build.out` |
| 前端单测 | `pnpm test:unit` | **1507 passed / 0 failed**（本卡新增 8 条） |
| 真库接口验收 | `python3 01_sources_check.py` | **64 通过 / 0 失败**（`02_sources_check.out`、原始报文 `03_sources_raw.json`） |

### 真库数据怎么来的（重要）

反查页要有「真实的料与货」，而这只有跑完 `calc → 审 → 结算` 才存在。本卡**不另造夹具**，
而是把 B4-5 的端到端演练当库用：`04_run_drill_keep.py` 加载
`scripts/verify/cost/cost_e2e.py`（不改那个文件），把它的收尾 `clean_fixture` 打成空函数，
于是跑完留下：成本对象 `E2E-CB-0001`、领料单 `E2E-OUT-0001`（POSTED，1000/19000）、
两张完工入库单（`IN-20261111-000003` 红冲作废 + `IN-20261111-000004` POSTED）、结算批次
`CS-202611-0003`（REVERSED）/ `CS-202611-0004`（SETTLED）。

> 该轮 `05_drill_keep.out` 是 **69 通过 / 9 失败**：失败的 9 条全是库存三本账的**绝对值**断言
> （期望 800、实际 1600）—— 因为这一轮复用了上一轮演练已经留下 800 件库存的库，而
> `clean_fixture` 被本脚本禁用，起始状态不再是 0。**不影响本卡**：不跑 keep 的干净库演练
> 在同日 17:49 跑到 **78/78 全绿**（`drill-20261111-175102.out`），且本卡验收脚本的判据全部
> 是「接口 vs 库」的交叉比对，不含写死的库存绝对值。

## 已知边界 / 偏差

1. **领料行的「凭证」列是空的**：演练夹具用 SQL 直接写了领料凭证，没回填
   `biz_stock_out.voucher_id`（真流程过账会回填）。所以 `03_sources_raw.json` 里领料行
   `voucher_no=null`，入库行有（结算真实生成并回填）。属夹具口径，不是接口缺陷。
2. **权限码复用 `business:erp:cost-object:list`**：反查就是详情页的一块内容，能看详情就该看到它的料和货；
   V0378 也只把 `cost-object:list` 授给财务角色。新开权限码要连带种 `sys_permission`/`sys_menu`，本卡无迁移。
3. **不分页**：单个成本对象下的领料/入库单据量级是「几十张」，一次全给；若将来单对象上千张再补分页。
4. **`issue_all_collected` 只数「已过账但未足额归集」**：草稿单据不参与（它本来就不该有归集），
   页面在行上标「草稿，不计入成本」。
5. **计算单里 REVERSED 与 SETTLED 两行都在**：判据只数 `SETTLED`（`settled_output_of` 的 where），
   所以红冲后仍平 —— 页面截图里能看到两条计算单行，这是批次历史的正常展示。
