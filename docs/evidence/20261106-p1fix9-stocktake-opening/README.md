# P1-FIX-9 验收：盘点单 / 期初余额 两个模块「生产 0 行」的端到端实测

**卡号**：t_446586b6（P1-FIX-9） · **执行**：test-enginner · **时间**：2026-11-06 00:15–01:10（CST）
**被测树**：`/opt/cloudpivot` HEAD = `e73a204`（2026-11-06 00:12:45）

---

## 一、结论（先看这段）

| 项 | 结论 |
|---|---|
| 盘点单全流程 | ✅ **能用**。建单 → 实盘 → 差异 → 提交审批 → 审批 → 过账 → 库存调整 → 作废回滚，**两轮连跑全绿**（HTTP 19/19 断言 ×2 轮）+ 真机 UI 13/13 断言 ×2 轮 |
| 期初余额全流程 | ✅ **能用**（按实现口径）。录入科目期初 + 库存期初 → 建账（唯一期初凭证 POSTED）→ 库存三本账 + 凭证齐；**重复录入=覆盖同一张凭证** 已实测 |
| 三本账核对 | ✅ 成本账 = 库位账 = 流水净额 = 100；`1405` 余额 = 库存金额 = 1000；`1901` 过账/红冲后归零 |
| 阻断性缺陷 | **0 个** |
| 发现问题 | **5 条**（1 条 P2 巡检假红、2 条 P3 体验/口径、2 条环境说明），均不阻断使用，已另开卡 |

> 与卡面唯一的口径差异：**期初模块没有「审核」与「Excel 导入」两步**。实现是「录入即建账」（保存即生成唯一期初凭证并直接 POSTED，期间结账后整页只读）。这是 `opening.rs` 文件头的既定设计，不是缺陷，但**卡面验收标准里的「导入 → 审核 → 建账」在代码里没有对应物**，已开卡请产品确认口径（见 §六）。

---

## 二、环境指纹（可复现）

| 项 | 值 |
|---|---|
| 库 | `cloudpivot_st9`（99，`createdb -O alun` 全新建库，**不碰生产/144/cloudpivot_test**） |
| 实例 | `127.0.0.1:18190`（`NIK_PROFILE=test`，`NIK_ADMIN_PASSWORD=Admin@123456`，二进制品 `/root/st9-bin/cloudpivot`） |
| 迁移 | 198 条（versioned 188 + seed 10），**最新业务迁移 V0256 `erp_report_item_admin`**（= HEAD 的迁移集），116 张表 |
| 前端 | vite dev `127.0.0.1:15190`（`BACKEND_ORIGIN=http://127.0.0.1:18190`，源码 = HEAD 工作树，前端无未提交改动） |
| 账号 | `admin`（首登强制改密 → `St9@Admin#2026`，走真实 `/system/auth/password`）；`st_wh`（新建，`WAREHOUSE_MANAGER`，作**盘点审批人**以避开 `self_approval=FORBID`） |
| 主数据 | 单位 `ST9-PCS`、仓库 `ST9-WH`（level 1）、物料 `ST9-M-001`（`MOVING_AVG`） |
| 会计期间 | `2026-11` `OPEN`（干净库 V0221 自动开当年前 12 期） |

> 二进制说明（诚实登记）：`/root/st9-bin/cloudpivot` 取自共享 `backend/target/debug/`，构建时间 2026-11-06 00:18（晚于 HEAD 提交），迁移集含 V0256 ⇒ 与 HEAD 一致；主仓当时有两处**与本卡无关**的未提交改动（`payable_payment.rs` / `serial_handler.rs`），盘点与期初模块自 2026-11-05 14:57 起无改动，故不影响本卡结论。

---

## 三、验收项逐条对照

卡面 5 项（含 laoliu 10:55 修正后的 ①–⑤）：

| # | 验收项 | 结果 | 证据 |
|---|---|---|---|
| ① | 盘点：建单 → 实盘 → 差异 → 审批 → 过账 → 库存调整 | ✅ | `round1.txt` / `round2.txt`（步骤 2.1–2.12）、`shots/04..10` |
| ② | 期初：录入 → 建账 → 库存 + 凭证齐 | ✅ | `round1.txt` / `round2.txt`（步骤 1.1–1.7）、`shots/01..03` |
| ③ | 三本账核对（库存 / 流水 / 凭证） | ✅ | `dbcheck_round1.txt` / `dbcheck_final.txt`（psql 直读，不经业务代码）+ `report_check.txt` |
| ④ | 作废回滚验证 | ✅ | `round1.txt` 2.13–2.15（盘亏单）、`gain.txt` G6（盘盈单） |
| ⑤ | 截图各 3 张 | ✅ 期初 3 张 + 盘点 9 张 = **12 张** | `shots/01..12` |
| ⑥ | 问题清单 | ✅ 5 条（见 §五） | 本 README §五 |
| ⑦ | 结论 | ✅ 能用 / 0 阻断 / 5 问题 | 本 README §一 |
| ⑧ | git commit（只提交验收文档） | ✅ 见提交记录 | `docs/evidence/20261106-p1fix9-stocktake-opening/` |

### 3.1 盘点全流程实测（每步真实输出，摘自 `round1.txt`）

| 步 | 动作 | 实测结果 |
|---|---|---|
| 2.1 | `POST /business/erp/stock-take`（FULL） | `ST-20261106-000001`，`DRAFT`，`total_book_qty=100.0000`（**账面数由服务端从 `biz_inventory` 快照**） |
| 2.2 | `GET …/{id}` | 明细 1 行：`book_qty=100.0000` `actual_qty=100.0000` `diff_qty=0` `unit_cost=10.000000` |
| 2.3 | `PUT …/{id}` 实盘 92 | `diff_qty=-8.0000`；`diff_amount=0.0000`（**设计**：差异金额在过账时按账上真实差额写，DTO 与前端都注明「过账后才有值」） |
| 2.4 | `PATCH …/status {PENDING_POST}` | 200，起流程 `stock_take_post`，`approval_instance_id` 回写 |
| 2.6 | 仓管主管待办 | `st_wh` 待办里出现该单节点（`n_wh`，ROLE=WAREHOUSE_MANAGER 解析到真实成员，**未走 FALLBACK_ADMIN**） |
| 2.7 | `POST /business/flow/task/{id}/approve` | 200（`admin` 制单、`st_wh` 审批，未撞 `65019 制单≠审批`） |
| 2.9 | `PATCH …/status {POSTED}` | 200 |
| 2.10 | 过账后详情 | `POSTED`，`total_loss_amount=80.0000`，`voucher_id` 已回写，`posted_by/posted_at` 落库 |
| 2.11 | 库存 / 流水 | 成本账 `qty=92.0000 amount=920.0000 avg_cost=10.000000`；新增 `OUT / STOCK_TAKE_LOSS / 8.0000 / 80.0000` |
| 2.12 | 差异凭证 | `PZ-20261106-000002` `AUTO/POSTED`：**借 1901 80 / 贷 1405 80** |
| 2.13 | `PATCH …/status {CANCELLED, reason}` | 200（已过账作废必须写原因，否则 400） |
| 2.14 | 回滚结果 | 成本账回 `100.0000 / 1000.0000`；流水新增 `IN / STOCK_TAKE_CANCEL / 8`；`reverse_voucher_id` 生成 |
| 2.15 | 红冲凭证 | `PZR-20261106-000001` `is_red=1`：**贷 1901 80 / 借 1405 80**（方向相反、指向原凭证） |
| 2.16 | 列表接口复核 | `total=1`，`status=CANCELLED`、`voucher_id` 可见 |

**第二轮**（`round2.txt`，同库同实例）：单号 `ST-20261106-000002`，**19/19 断言再次全绿**，两轮除单号/id 外逐字一致。

**盘盈分支**（`gain.txt`，实盘 105 > 账面 100）：差异 `+5`，过账后 `105 / 1050`，凭证 `PZ-…-000004` **借 1405 50 / 贷 1901 50**，随后作废 → 库存回 `100 / 1000` + 红冲 `PZR-…-000003`，**9/9 断言全绿**。

**边界/异常 13 条**（`edge.txt`，全部 PASS，错误码与文案均合理）：

```
E1  全盘带 items            → 400 / 10001  "全盘（FULL）不接受 items…"
E2  抽盘不带 items          → 400 / 10001  "抽盘（PARTIAL）必须传 items（1 行起）…"
E3  仓库不存在              → 400 / 66002  "盘点仓库不存在或已删除"
E4  take_type=HALF          → 400 / 10001  "take_type 只能是 FULL / PARTIAL…"
E5  抽盘建单（正例）        → 200，快照账面 100
E6  实盘 -1                 → 400 / 10001  "实盘数不能为负（盘亏记正数…）"
E7  草稿直接过账            → 409 / 10003  合法路径逐条点名
E8  / E9  草稿删除 → 删后详情 404 / 10002
E10 期初借贷不平            → 400 / 66012  "借方合计 1999.0000 / 贷方合计 1001.0000，差额 998.0000"（点名科目）
E11 有库存期初再手录 1405    → 400 / 10001  "科目 1405 已经有库存期初 1000.0000…会重复计数"
E12 库存期初 qty=0          → 400 / 10001  "期初数量必须 > 0…"
E13 已作废再作废            → 409 / 10003
```

### 3.2 期初全流程实测（`round1.txt` 步骤 1.1–1.7）

| 步 | 动作 | 实测结果 |
|---|---|---|
| 1.1 | `POST /business/erp/opening-balance`（借 1002 5000 / 贷 3001 5000） | `state=ENTERED`，`balanced=true`；此时 `voucher` 已生成（`total_debit=5000`） |
| 1.2 | `POST …/opening-balance/inventory`（`ST9-M-001` × 100 @ 10） | `inventory_amount=1000.0000`，`inventory_debit_account=1405`、`inventory_credit_account=3103`，`combined_debit_total=combined_credit_total=6000.0000`，`balanced=true` |
| 1.3 | `GET …/opening-balance?period=2026-11`（独立复核） | `state=ENTERED`，`voucher={PZ-20261106-000001, OPENING, POSTED, 6000/6000}`，`inventory=[{qty 100, unit_cost 10, amount 1000}]` |
| 1.4 | `GET /business/erp/voucher/{id}` | 4 行分录：**借 1002 5000 / 借 1405 1000 / 贷 3001 5000 / 贷 3103 1000**，`POSTED` |
| 1.5 | `GET …/inventory/page` | `qty=100.0000 amount=1000.0000 avg_cost=10.000000` |
| 1.6 | `GET …/inventory-location/page` | 未分配库位桶 1 行 `qty=100` |
| 1.7 | `GET …/inventory-txn/page` | `IN / OPENING / OPENING-2026-11 / 100 / 1000` |
| 二轮 | 重复录入同一期间 | **同一张凭证** `PZ-20261106-000001`（id 未变，仍 1 张），`state` 仍 `ENTERED` ⇒ 「每期间一张期初凭证，重复录入=覆盖」成立 |

报表侧复核（`report_check.txt`）：试算平衡 `balanced=true`；资产负债表 **存货 1,000.00 / 资产总计 6,000.00 = 负债和所有者权益 6,000.00**；利润表 `营业收入=0`、`净利润=0` ⇒ **期初凭证没被算进本期发生额**（与 `opening.rs` 文档一致）。

### 3.3 三本账核对（psql 直读，`dbcheck_final.txt`）

```
① biz_inventory             ST9-M-001 | ST9-WH | 100.0000 | 1000.0000 | 10.000000
② biz_inventory_location 合计                                        100.0000
③ biz_inventory_txn 净额                                            100.0000
   biz_type = OPENING, STOCK_TAKE_CANCEL, STOCK_TAKE_GAIN, STOCK_TAKE_LOSS
④ 总账（只认 POSTED）：1405 借 1210 / 贷 210 → 余额 1000.0000（= 库存金额）
                        1901 借 210 / 贷 210 → 余额 0（盘亏过账 → 作废红冲后归零）
⑥ 凭证 7 张全部 POSTED：1 张 OPENING(6000) + 3 张盘点差异 + 3 张红冲（is_red=1，reverse_of 指回原凭证）
```

### 3.4 真机 UI（Playwright，`e2e_run2.txt` / `e2e_run3.txt`）

隔离实例 15190 + 18190，Chromium headless，**13 条断言连跑两轮 FAIL 0**（两轮差异仅单号/id）：

```
① 期初页（切到 2026-11）：状态「已录」+ 凭证信息非空           → shots/01
② 库存期初 tab：数量 100、合计 1,000.00                        → shots/02
③ 资产负债表：存货 1,000.00 / 资产总计 6,000.00                → shots/03
④ 盘点列表                                                     → shots/04
⑤ 点「新建盘点单」→ 选仓库/备注 → 保存 → 详情抽屉自动打开      → shots/05、06
⑥ 详情账面数 100；录实盘 90 → 差异行 -10                       → shots/07
⑦ 点「提交审批」→ 状态「待过账」                               → shots/08
⑧ 仓管主管审批（引擎接口）→ 点「过账」→ 状态「已过账」+ 凭证已回写 → shots/09
⑨ 库存页：ST9-M-001 数量 90                                    → shots/10
⑩ 详情点「作废」+ 原因 → 状态「已作废」                        → shots/11
⑪ 库存页：回滚到 100                                           → shots/12
```

---

## 四、独立巡检（`erp_invariants.py`）结果与一次**假红**归因

`invariants_after_r1.txt` / `invariants_final.txt`（同一脚本、指向 `cloudpivot_st9`）：

```
结果：64/66 通过   🚨 BLOCKER 1   ⚠️ WARN 1
🚨 [状态] E4 作废单的库存已回滚 — 1 组库存大于有效入库量（有作废单没退库存）
   ↳ 定位：ST9-M-001 | P1-FIX-9 验收总仓 | 有效入库 0 | 调拨序列净入 0 | 库存 100.0000 | 多出 100.0000
⚠️ [权限] N2 在用 HUMAN 账号必须有部门归属 — admin、st_wh 无部门（隔离库自建账号）
```

**E4 是假红，不是账错**：§3.3 已用 psql 证明三本账自洽（100 = 100 = 100，1405 余额 1000 = 库存金额 1000）。
根因在脚本白名单：`erp_invariants.py` L588-593 的「合法入库来源」只认
`biz_stock_in(POSTED)` + `TRANSFER_IN/OUT` + `SERIAL_IN/OUT`，**漏了 `OPENING`（库存期初）、`STOCK_TAKE_GAIN`（盘盈）、`STOCK_TAKE_CANCEL`（盘亏作废回补）**。
⇒ 任何用「库存期初」或「盘盈」入账的库都会被判 **BLOCKER**（假红），而真正的「作废没退库存」会被这条假红淹没。详见问题 F1。

---

## 五、问题清单（均不阻断，已另开卡）

| # | 级别 | 问题 | 证据 | 处置 |
|---|---|---|---|---|
| **F1** | **P2**（会误伤门禁） | `scripts/verify/erp_invariants.py` 的 **E4 白名单缺 `OPENING` / `STOCK_TAKE_GAIN` / `STOCK_TAKE_CANCEL`** ⇒ 用库存期初或盘盈入账的库必判 BLOCKER 假红。本卡实测：三本账完全自洽（100=100=100、1405 余额=1000），E4 仍报「多出 100」 | `invariants_final.txt` L51-54 + `dbcheck_final.txt` | 另开卡（test-enginner）：补齐白名单 + 加「期初/盘盈后 E4 必须绿」的回归用例 |
| **F2** | P3 | 盘点详情「差异凭证」显示 `shortId(voucher_id)`（`9256f0e…`），**不显示凭证号 `PZ-…`**；凭证列表按凭证号检索，财务无法据此对账 | `shots/09`、`stock-take/index.vue:432` | 另开卡（frontend-developer）：`StockTakeRes` 回填 `voucher_no` 或点开跳凭证 |
| **F3** | P3 | 期初页默认期间取「最新已开启期间」= `2026-12`（今 2026-11-06），进页面看到全 0.00 会误以为「期初没数据」；本次必须先手动切到 `2026-11` 才看到已录数据 | `shots/01` 首帧（`opening/index.vue:543`：`list.find(status==='OPEN')`） | 建议默认「当前月」或「最早未录期间」 |
| **F4** | 口径（非缺陷） | 期初**没有审批/审核环节**，也没有 Excel 导入：实现是「录入即建账（保存即生成唯一期初凭证并 POSTED），期间结账后整页只读」 | `opening.rs` 文件头 + §3.2 | 卡面「导入 → 审核 → 建账」缺对应物，**需产品确认口径**（另开卡 product-manager） |
| **F5** | 环境说明 | `N2`：`admin` / `st_wh` 无部门归属（隔离库自建账号）。生产 admin 已落位 HQ；提醒建审批账号时要挂部门，否则部门类角色被静默降级为「仅本人」 | `invariants_final.txt` L121 | 已在报告中说明，不必修 |

**已确认「不是缺陷」的三处**（避免下一手重复排查）：

1. 草稿阶段 `diff_amount = 0`、过账后才写 —— `stocktake_dto.rs` 文件头已声明，前端也标「过账后才有值」。
2. 干净库 `WAREHOUSE_MANAGER` 0 成员时 `n_wh` 走 `FALLBACK_ADMIN` —— 本卡建了真实仓管主管，实测**走真实成员**、未触发兜底。
3. 期初凭证不进本期发生额（利润表 0）—— 设计口径，`report::load_accounts` 已剔除。

---

## 六、复现命令（全部在 `scripts/` 内，均已实跑）

```bash
# 1) 建隔离库 + 起实例（端口 18190）
bash scripts/reset.sh            # dropdb/createdb cloudpivot_st9 + 启动 + 等 health=200
python3 scripts/step1_login_and_users.py   # 首登改密 + 建审批人 st_wh（真实接口）
# 2) HTTP 全流程两轮
python3 scripts/flow.py 1 && python3 scripts/flow.py 2
# 3) 盘盈分支 + 13 条负向用例
python3 scripts/gain.py && python3 scripts/edge.py
# 4) 三本账 + 独立巡检 + 报表
psql "postgres://alun:alun@127.0.0.1:5432/cloudpivot_st9" -f scripts/check.sql
bash scripts/invariants.sh final
# 5) 真机 UI（vite 15190 + Chromium）
bash scripts/start_fe.sh && node scripts/e2e.cjs
```

---

## 七、本轮未做 / 明确边界

1. **没碰生产 144**：验收按卡面在隔离库做；生产上线（V0256 之后）后建议用同一套 `flow.py` 打只读实例复核一次。
2. **抽盘（PARTIAL）只验到「建单 + 删除」**：全流程走通的是全盘（FULL）；抽盘的过账路径与全盘共用 `post_tx`，未单独跑一遍。
3. **审批动作走引擎接口而非「审批中心」页面**：盘点详情页的提交/过账/作废都在 UI 上点了，审批人 `st_wh` 的「同意」走 `POST /business/flow/task/{id}/approve`（同一实例真库）。审批中心页面属 flow 模块，不在本卡范围。
4. `erp_invariants` 的 64/66 **不是**「账错」：1 条为本报告 §四 的 E4 假红，1 条为隔离库自建账号无部门（N2）。
