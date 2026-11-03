# P1-5 操作便捷三件套 —— 真机证据（2026-11-03）

**卡片**：`t_95ff797e` 【P1-5】操作便捷三件套（联动作废编排 + 调拨库存选择器 + 满屏找按钮）
**依据**：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §6 R-P1-5（断链 DC-24）
**用户原话**：「操作便捷」「操作员操作的时候不能满屏去找按钮」

---

## 0. 验收环境（**不是生产**）

| 项 | 值 |
|---|---|
| 后端 | `http://127.0.0.1:18098`（profile=test，库 `cloudpivot_p15_e2e`） |
| 前端 | `http://127.0.0.1:5198`（vite dev，`/api` 反代 → 18098） |
| 库来源 | `scripts/verify/e2e_isolated_instance.sh`：**144 prod 库的 pg_dump 副本**（只读导出） |
| 造数 | 全部落在副本库；生产 144 **一行未写**（本卡不发生产，发布另有卡） |
| 登录态 | 用实例自己的 `jwt_secret`（`/root/tmp/e2e-p15/jwt_secret`）签的 admin access token 写进 `localStorage`；**没有猜口令、没有改口令** |

---

## 1. 交付①：联动作废编排（影响面 → 一键按顺序处理）

### 真机路径（截图 01→03）

1. `01-purchase-detail-action-bar.png` —— 采购单详情，顶部吸顶条
   `关键操作 提交审批 / 生成收货单 / 作废`（`position: sticky`，`z-index: 30`）。
2. 点「作废」→ **先出影响面**（`GET /business/erp/purchase-order/{id}/cancel-impact`），
   `02-cancel-impact-dialog.png`：

   ```
   作废 PO-20261031-000011 将影响以下 4 张单据（本次会执行 4 个动作）
   收货入库单 IN-20261102-000001  ¥16.95  作废（回滚三本账 + 退序列号）
   凭证       PZ-20261102-000001  ¥16.95  自动红冲（生成红字凭证）
   应付单     AP-20261102-000001  ¥16.95  自动作废
   采购订单   PO-20261031-000011  ¥16.95  作废采购单
   ```

   弹窗初始「一键按顺序处理」**置灰**（`confirmDisabled: true`），填了作废原因才可点
   （`confirmEnabledAfterReason: true`）—— 卡面「必须先让用户确认影响面」的硬红线。
3. 填原因 → 一键处理 → `POST …/cancel-cascade`（一个事务）→
   提示 `PO-20261031-000011 已联动作废：2 个动作在一个事务里完成`，
   详情头部变 `已取消`（`03-cancel-cascade-done.png`）。

### 三账回基线 + 凭证红冲（`db-before.txt` → `db-after.txt`）

| 观测点 | 作废前 | 作废后 | 结论 |
|---|---|---|---|
| 采购单 PO-20261031-000011 | `RECEIVED` | `CANCELLED` | 状态机 `RECEIVED → CANCELLED` 由联动作废专用边放行 |
| 收货单 IN-20261102-000001 | `POSTED` | `CANCELLED` | 复用 `stockin_handler_cancel::cancel_in_tx`，回滚逻辑一行未重写 |
| 采购明细 received_qty | `3.0000` | `0.0000` | 已收数量退回 |
| 成本账 `biz_inventory`（M-W1A-001 / WH01） | 数量 21 / 金额 152.00 | **18 / 137.00** | 21−3、152−15（成本 5/件）→ 回到收货前基线 |
| 库位账 `biz_inventory_location` | 21 | **18** | 第三本账同步回滚 |
| 库存流水 | `IN PURCHASE_IN 3` | `IN PURCHASE_IN 3` + `OUT PURCHASE_IN_CANCEL 3` | 反向冲减流水（净额 0） |
| 进项凭证 PZ-20261102-000001 | `reversed_by=-` | `reversed_by=ae4a028e…` | 原凭证打红冲标记（仍保持已记账） |
| 红字凭证 | 无 | `PZR-20261104-000001` ¥16.95（`is_red=1`） | 自动红冲真实生成 |
| 应付单 AP-20261102-000001 | `OPEN` ¥16.95 | `CANCELLED` | 未付款的应付随收货单作废 |

**不变量**（同一副本库，跑 `scripts/verify/erp_invariants.py`）：

```
结果：40/40 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。
```

即 D1/E1 一类的账务巡检在联动作废之后依然全绿 —— 说明「一次点掉四张单」没有留下孤儿账。

---

## 2. 交付②：调拨单「从库存选择」

`07-transfer-stock-picker.png` / `08-…-selected.png` / `09-transfer-items-from-stock.png`

* 明细区「**从库存选择**」按钮（`data-testid=btn-transfer-pick-stock`）；未选调出仓时点它 →
  提示 `先选「调出仓」：可调拨的库存是按调出仓算的`（不弹空窗）。
* 弹窗列：`物料 / 库位 / 可用量 / 成本价 / 本次调拨`（实测行：
  `原料A（种子）（M-W1A-001） —（未分配库位） 18 ¥7.6111`）。
  * 可用量 = `qty − locked_qty`（把「锁定量」算成可用就是超发）；
  * 成本价取本仓该物料的移动加权均价（`biz_inventory.avg_cost`），无成本价显示「—（无成本价）」；
  * 勾选后「本次调拨」默认等于可用量，可改小（上限 = 可用量）。
* 勾选 → 带入明细：`materialTexts: ["原料A（种子）（M-W1A-001）"]`、`qtyValues: ["18"]`，
  提示 `已从库存带入 1 条明细`。
* 带入的是 **`biz_inventory_location.id`**（库位库存行），不是库位主数据 id ——
  两个同名 id 反着指，这是 `stock-pick.ts` 里专门钉住的一处（单测 `from_location_id 口径`）。

合并规则（`mergeStockPicks`，纯函数 + 单测）：同物料同库位累加、不同库位各占一行、
表单默认空行先复用。

---

## 3. 交付③：4 个详情页关键按钮固定位

四个详情页都在内容区顶部插了同一条 `DocActionBar`（`position: sticky; top: -12px; z-index: 30`），
往下翻明细 / 审批时间线 / 附件时它一直贴在视口顶部：

| 页面 | 截图 | 关键动作（实测文本） | 吸顶判据 |
|---|---|---|---|
| 采购单详情 | `01-…` | 提交审批 / 生成收货单 / 作废 | `position: sticky`，滚动 146px 后 top 68→60（贴住不动） |
| 收货单详情 | `04-…` | 过账 / 作废 / 撤回作废申请 | `position: sticky`，top 68 |
| 应付单详情 | `05-…` | 发起付款（未付 226.00）/ 作废 | `position: sticky`，top 69 |
| 调拨单详情 | `06-…` | 提交审批 / 发货 / 完成（收货）/ 作废 | `position: sticky`，top 68 |

> 应付单的「发起付款」原来埋在抽屉正文中段（要滚才能看见），这次整块搬到吸顶条；
> 原位置只留一句「已上移到吸顶动作条」，不重复放两个按钮。
> 调拨单多给了「发货」—— 调拨的状态链是 提交 → 发货 → 完成，只固定「完成」会让作业员
> 又回到列表行去找「发货」，与卡面初衷相反。

---

## 4. 代码与测试

| 项 | 结果 |
|---|---|
| 后端单测（真库 `cloudpivot_p15`，`ERP_DB_URL`） | `cargo test --lib p15_` → **14 passed / 0 failed**（30s 量级，不是「0.7s 假绿」） |
| 前端单测 | `tsx --test "tests/unit/**/*.test.ts"` → **590 passed / 0 failed**（含本卡新增 15 条） |
| 前端类型 | `vue-tsc --noEmit` → 0 错误 |
| 提交前检查 | `bash scripts/check.sh` → fmt / clippy(`-D warnings`) / cargo check / typecheck **全过**；闸① 0、闸② 0（`SCOPE-EXEMPT` 扫描面 313 文件）、C7 G1–G6 全过、色板一致 |
| 闸③ 不变量 | 本机没有 prod 库凭据 → check.sh 里按 `ERP_INVARIANTS_GATE=warn` 跑；**真跑**用的是隔离副本库：40/40 通过 |

后端 14 条单测覆盖：影响分析准确性（列收货单/凭证/应付 + 金额 + 处理方式）、
执行顺序（`seq` 就是顺序）、中途失败整笔回滚、无下游时直接作废、拦路虎（已付款应付）409、
幂等（重复点）、`doc_type` 归一与未知类型 400、调拨单改用状态流入口、影响分析只读、
已作废收货单标 SKIP、应付单作废红冲凭证、已收货采购单可作废、原因必填且长度校验。

---

## 5. 已知限制 / 说明

* **视觉配额耗尽**（`vision_analyze` 429，周配额到 2026-11-09 才恢复）：截图未用人眼描述，
  改用 DOM 硬断言（`assertions.json`）+ PNG 解码后的像素统计（`std` 69.97~87.16、唯一色 1336~2009、
  单张 108~184 KB）证明「不是空白图」。
* **验收消耗了这次联动的目标单据**：PO-20261031-000011 已经是 `CANCELLED`。
  要重跑本套证据，先 `E2E_ISO_FRESH=1 … e2e_isolated_instance.sh up` 重灌副本库。
  重跑第二遍会走「幂等分支」（提示 `已处理过，本次没有需要执行的动作`），不会二次动账。
* **联动作废不逐张走审批**（设计取舍，写在 `cancel_cascade.rs` 文件头）：
  一个事务里开不出三张审批单、也等不到三个人批。取而代之的是「影响面弹窗 = 显式确认」+
  权限码 `business:erp:doc:cancel-cascade`（V0213 只发给 SUPER_ADMIN / ADMIN）+
  每张单仍然写 `biz_status_log`（reason 统一 `联动作废：` 前缀）。
* **V0213 的自检抓到一处真实迁移 bug**：第一版按 `sys_menu.path` 找三条业务线菜单，
  全新库的采购菜单是 `/business/erp/purchase-order`、生产库是 `/business/erp/purchase`，
  在生产库副本上只命中 2/3 → 迁移自检 `RAISE EXCEPTION` → 实例起不来。
  现改为按 `auth_code`（`business:erp:{单据}:list`）匹配，两个库都成立。

---

## 6. 文件清单

```
docs/evidence/20261103-p15-cancel-cascade/
├── 01-purchase-detail-action-bar.png      采购单详情吸顶关键操作
├── 02-cancel-impact-dialog.png            影响面弹窗（4 张单据 + 一键处理置灰）
├── 03-cancel-cascade-done.png             一键处理完成（详情已取消）
├── 04-stockin-detail-action-bar.png       收货单详情吸顶关键操作
├── 05-payable-detail-action-bar.png       应付单详情吸顶关键操作
├── 06-transfer-detail-action-bar.png      调拨单详情吸顶关键操作
├── 07-transfer-stock-picker.png           从库存选择（物料/库位/可用量/成本价）
├── 08-transfer-stock-picker-selected.png  勾选一行
├── 09-transfer-items-from-stock.png       带入明细（物料 + 数量 18）
├── assertions.json                        DOM 断言原始值（吸顶位置 / 影响面 / 明细回填）
├── db-before.txt / db-after.txt            作废前后三账 / 流水 / 凭证 / 应付快照
└── run.log                                一次完整运行的文本日志
```

---

## 7. 复验记录（第二次跑，用**最终提交版源码**）

第一次跑（2026-11-04 00:06）与提交之间源码还有过一次改动，二进制（23:59）比 `.rs`（00:06）旧，
所以**上面那批证据不能算最终代码的**。本卡交付前用最终源码重建、重灌副本库、重跑了一遍，
截图 / 断言 / 快照 / 日志已全部按这次结果覆盖：

| 步骤 | 结果 |
|---|---|
| `cargo build --bin cloudpivot`（最终源码） | 07:10–07:11，`BUILD_EXIT=0` |
| `E2E_ISO_FRESH=1 … up`（重新 `pg_dump` prod → `cloudpivot_p15_e2e`，后端 18098 / 前端 5198） | 副本库 `sys_user=25` 行、后端 `/health` 通、前端探测码 401 |
| 作废前快照 | PO `RECEIVED` / IN `POSTED` / 库存 21 = 152.00 / 库位账 21 / 流水 `IN PURCHASE_IN 3` / PZ `reversed_by=-` / AP `OPEN` |
| Playwright 真机 9 项断言 | 全过（见 `run.log` ①②③④⑤） |
| 作废后快照 | PO `CANCELLED` / IN `CANCELLED` / received_qty `0.0000` / 库存 18 = 137.00 / 库位账 18 / 流水 `+OUT PURCHASE_IN_CANCEL 3`（净 0）/ PZ `reversed_by=e5cb78bb…` + `PZR-20261104-000001 ¥16.95 红字=1` / AP `CANCELLED` |
| `erp_invariants.py`（同一副本库，作废后） | **40/40 通过，BLOCKER 0 / WARN 0** |
| `cargo test --lib p15_`（`ERP_DB_URL` → `cloudpivot_p15`，`--test-threads=1`） | **14 passed / 0 failed，20.85s** |

> 复验期间发现并修掉一处**门禁红**：新增的 `business/erp/cancel_cascade_tests.rs` 没登记进
> `scripts/lint/scope_gate.py::SKIP_MANIFEST`（闸② 的防假绿硬断言，多一个少一个都不放行）→
> 补一行后 `python3 scripts/lint/scope_gate.py --root .` 退出码 0（跳过清单 52 条一致）。

