# P3-07 SoD 扩展三开关 — 证据

> 卡：`t_49dcd588`（P3-07）｜分支 `main`｜隔离实例 `127.0.0.1:18311` / 库 `cloudpivot_p307_ut`
> 结论：**三个新开关（`approve_execute` / `stocktake` / `purchase_accept`）全部落地并通过真 HTTP 实测；
> 默认全部 `false`（升级零行为变化），总闸 `enabled=true` 时一键变「拦」；
> 原有 `approve` / `post` / `pay` 三闸行为逐条回归不变。**
> 实测结果：`20_e2e_sod.sh` **PASS=103 FAIL=0**；`10_restart.sh` **PASS=7 FAIL=0**。

---

## 1. 做了什么（代码侧）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/separation.rs` | `Duty` 枚举加 `ApproveExecute` / `Stocktake` / `PurchaseAccept` + 三个开关键常量 + `label/action/counterpart_clause`；新增 `check_among()` / `guard_among()`（多审批人判据）与 `approvers_of()`（取审批实例里投过同意票的人）；`rejection_text()` 改成按职责对拼「由你本人制作 / 审批 / 采购」；新增 6 条单测 |
| `backend/src/business/erp/stocktake_handler.rs` | ① `submit_for_approval`（草稿→待过账 = **盘点确认**）加 `Duty::Stocktake` 闸；② `post_tx`（过账 = **执行**）加 `Duty::ApproveExecute` 闸（审批人取该实例全部同意票） |
| `backend/src/business/erp/stockin_handler_write.rs` | 建单 / 改单把采购单挂到收货单时加 `Duty::PurchaseAccept` 闸（采购员 = PO 的 `create_by`） |
| `backend/src/business/erp/payable_payment.rs` | `settle_approved_in_tx` 加 `Duty::ApproveExecute` 闸 → 命中时**不记账**，返回新的 `SettleOutcome::AwaitingExecutor`；新增**人工执行入口** `execute_approved_payment()` + `POST /business/erp/payable/payment/{id}/execute`（权限码复用 `business:erp:payable:pay`） |
| `backend/src/business/erp/payable_bridge.rs` | 审批回调遇到 `AwaitingExecutor` → `warn` 日志 + 通知发起人「这笔钱还没付，需要另一位有付款权限的人执行」，账一分不动 |
| `backend/migrations/business/V0340__erp_sod_ext_switches.up.sql` / `.down.sql` | `sys_config` 三条参数（默认 `false`、`BOOLEAN`、分组 `erp_internal_control`）+ 自检；down 只删这三行并自检 |

**判定只有一个入口**：接入点只做「拿两个人的 id（或一组审批人）+ 一次 `separation::guard*` 调用 + 错误原样 `?` 冒泡」，
没有任何散落的 if-else；错误码沿用 `409 + 66013（ERP_SEPARATION_OF_DUTIES）`，**没有新开号段**。

### 迁移号（决策 D4）
动手前核对：`ls backend/migrations/business/ | grep -o '^V[0-9]*' | sort -u | tail` → 最大 **V0339**（P3-06 占用），
卡面预计的 V0338 已被 P3-05 占用 → 本卡取 **V0340**（P3-08 从 V0341 起）。

---

## 2. 三条验收：真 HTTP 实测（`raw/20_e2e_sod.txt`）

用户：`p307_maker`（采购员/制单人）、`p307_boss`（审批人）、`p307_cash`（仓管/出纳）。
每条都跑「开关关 → 放行 / 开关开 → 409 / 开关开 + 换人 → 通过」三态。

### ① 采购 ↔ 验收（`erp.separation_of_duties.purchase_accept`）

| 步骤 | 请求 | 断言 |
|---|---|---|
| 关（默认） | maker `POST /business/erp/stock-in`（source = 自己的 PO） | **200**，收货单建成 |
| 开 | maker 同上 | **409 + 66013**，文案含「采购与验收分离」+ 采购单号 + 开关键名 |
| 开 | cash 同上（换人） | **200** |
| 开 | 被拦后查库 | 该 PO 下收货单数仍为 1（没有半截落库） |

### ② 制单 ↔ 盘点确认（`erp.separation_of_duties.stocktake`）

| 步骤 | 请求 | 断言 |
|---|---|---|
| 关（默认） | maker `PATCH /stock-take/{id}/status {PENDING_POST}`（确认自己建的盘点单） | **200**，状态 `PENDING_POST`，起了审批实例 |
| 开 | maker 同上 | **409 + 66013**，文案含「制单与盘点确认分离」 |
| 开 | 被拦后查库 | 单据仍 `DRAFT`、`approval_instance_id` 为空（不许半推半就） |
| 开 | cash 同上（换人） | **200** |

### ③ 审批 ↔ 执行（`erp.separation_of_duties.approve_execute`）

两条人工执行点都实测：**盘点过账**（审批后人工点过账）与**付款执行**（`POST /payable/payment/{id}/execute`）。

| 步骤 | 请求 | 断言 |
|---|---|---|
| 开 | boss（审批人）`PATCH /stock-take/{id}/status {POSTED}` | **409 + 66013**「审批与执行分离」，单据仍 `PENDING_POST`（账一分没动） |
| 开 | cash（没批过这张单）同上 | **200** → `POSTED` |
| 关（默认） | boss 审批完自己过账 | **200** → `POSTED`（改动前行为） |
| 开 | maker 发起付款 → boss 审批通过 | 审批 **200**，但付款记录仍 `PENDING`、应付 `paid_amount` 仍 0.0000（**不自动记账**） |
| 开 | boss（审批人）`POST /payable/payment/{id}/execute` | **409 + 66013**「审批与执行分离」 |
| 开 | cash（出纳）同上 | **200** → 付款 `APPROVED`、应付 `paid_amount` +10、付款凭证已生成 |
| 关（默认） | 另起一笔付款 → boss 审批通过 | 自动记账（付款 `APPROVED`、`paid_amount` 15.0000）**= 改动前行为** |

### ④ 默认不改行为（开关关闭时三条都放行）
上表每组的「关（默认）」行即为证据：三条在关闭态全部 **200**。

### ⑤ 总闸 `enabled=true`（`raw/20_e2e_sod.txt` 末尾）
- 子键**显式 `false`** + 总闸 true → 制单人确认盘点单 **200**（显式优先于总闸，口径与 V0196 一致）；
- 子键**未配（NULL）** + 总闸 true → 制单人确认 **409**「制单与盘点确认分离」、采购员收货 **409**「采购与验收分离」（一键开启生效）；
- 复原（总闸 false + 三键 false）→ 采购员收货 **200**。

---

## 3. 回归：原有 approve / post / pay 三闸（`20_e2e_sod.txt` §S5）

| 闸 | 用例 | 结果 |
|---|---|---|
| `approve`（默认 true） | 制单人审批自己提交的盘点单 | **403**，文案仍含「制单与审批分离」 |
| `post`（默认 false） | 打开后制单人自己过账 | **409**，文案仍含「制单与过账分离」 |
| `pay`（默认 true） | 制单人审批自己提交的付款 | **403**，文案仍含「不能审批自己提交的付款申请」+ `pay` 开关键名 |

---

## 4. 迁移：应用成功 + 幂等（`raw/10_restart.txt`）

```
空库首启：应用 273 个迁移        # 含 V0340
重启    ：应用 0 个迁移          # V0340 幂等
down.sql → 三条删除（count=0）；up.sql 重放 → 三条回来且仍 false（ON CONFLICT 幂等）
erp.separation_of_duties.* = 8 条（V0196 四条 + P3-05 一条 + 本卡三条）
```

---

## 5. 门禁（`raw/30_checks.txt`、`raw/30_clippy_baseline.txt`、`raw/30_unit_separation.txt`）

- `rustfmt --check --edition 2024 <本卡 5 个文件>`：**干净**
  （注：`cargo fmt --check -- <paths>` 不按路径过滤，会把别人正在改的 `asset_dto.rs` 也报出来，故用 rustfmt 直跑）；
- `cargo clippy --lib -- -D warnings`：**只剩既有基线** `erp/sales_order_model.rs:699 too_many_arguments (8/7)`；
  本卡 5 个文件在 clippy 输出里**零命中**（脚本按路径逐条核对，见 `raw/30_checks.txt` 的「本卡文件是否出现在 clippy 输出里」）；
- `cargo test --lib business::erp::separation::`：**17 passed / 0 failed**（旧 11 + 新 6）；
- 新增单测覆盖：三开关「打开→拦 / 关闭→放」、总闸 true 且子键未配→默认拦、文案含职责对、
  多审批人整体比对（含「早期审批人执行也要拦」）、六职责矩阵自洽（键唯一 / 中文名唯一 / 默认值口径）。

---

## 6. 口径决定（读代码后定的，不是猜的）

1. **采购员 = `biz_purchase_order.create_by`**：`biz_purchase_order` **没有 `buyer_id` 列**（全库只有比价单
   `biz_price_inquiry` 有）；`submitted_by` 是提交人、`approved_by` 是审批人，都不是采购员。
2. **验收人 = 建 / 改收货单的人**（确认「收到哪些料、收多少」的那一步）。过账（真正入库）由既有
   `Duty::Post` 管「制单 ≠ 过账」，两者不重复。改单也拦：改数量 / 换来源单同样改变「验收了什么」。
3. **盘点「确认」= `DRAFT → PENDING_POST`**（实盘数与差异就此定稿，后面只剩审批与记账）。过账
   （`PENDING_POST → POSTED`）是**执行**，归 `approve_execute`。
4. **`approve_execute` 只管「人工执行」点**：收货单过账、付款自动记账这类「审批通过后引擎替已批准的决定动账」
   的路径**不拦**（没有第二个人的自由裁量可分离），只留日志。付款路径因此补了一个真正的人工执行入口
   `POST /business/erp/payable/payment/{id}/execute` —— 否则开关一开，付款既不能自动记账、也没有人能执行，
   等于把钱卡死。这也是本卡唯一一处**新增接口**（权限码复用 `business:erp:payable:pay`，不新造死码）。
5. **审批人取「该实例所有投过同意票的人」**（`biz_flow_task.status='APPROVED'`），不是只比最后一个：
   多级审批下只比最后一人会漏掉「A 批、B 批、A 执行」。
6. **查审批人失败时放行**（`warn` 出声）：这是内控附加闸，一次 DB 抖动不该把付款/过账全卡死；
   钱账安全由各自的状态机与唯一记账实现保证。

---

## 7. 复跑

```bash
export PGPASSWORD=<本机测试库口令>          # alun
cd /opt/cloudpivot/docs/evidence/20261108-p307-sod-ext
bash scripts/20_e2e_sod.sh                  # 空库首启 + 全场景（自带实例生命周期，可整体复跑）
bash scripts/10_restart.sh                  # 重启「应用 0 个迁移」+ down/up 幂等
bash scripts/30_checks.sh                   # fmt / clippy / 单测
```
实例环境文件 `/root/p307/instance.env`（600，含三把密钥与 admin 初始口令，**不入库**），
二进制快照 `/root/p307/bin/cloudpivot`（本卡构建产物）。

---

## 8. 已知边界 / 后续

- **前端**：付款「待他人执行」目前只有后端入口（`POST …/payment/{id}/execute`），应付详情页还没有「执行付款」按钮；
  开关默认关，不影响存量用户。建议单独开一张前端卡（`frontend-developer`）补按钮 + `payment-info` 的「待执行」标记。
- **未部署 145/144**：卡面验收只要求隔离实例实测；开关默认 `false`，上线不需要额外动作。
- `stock_in_*`（收货单）过账仍是「审批通过即自动过账」，本卡的 `approve_execute` 按上文 4 的口径不介入。
