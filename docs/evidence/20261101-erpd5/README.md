# D5：应付单 → 发起付款（带出未付金额）

> 卡：`t_3f47d9ed` ｜ 交付提交：`a87f6a9` ｜ 生成时间：2026-11-01 21:40（CST）

| 卡面自检项 | 落地 | 证据 |
|---|---|---|
| F1 单据链路：应付详情「发起付款」→ 带 `payable_id` → 自动带出未付 | ✅ | §3 §5 §7（真机 42 项 + 生产只读 46 项） |
| F2 数量口径：带「未付」不是「总额」 | ✅ | `default_amount` 由后端算（§2.1）；页面断言 126 ≠ 226（§6 ②） |
| F3 可追溯：付款记录显示应付单号 + 可点回 | ✅ | 付款记录带「应付单号 / 凭证 PZ-xxx」两个深链（§3.2、§6 F3 用例） |
| O1 默认值：付款金额默认 = 未付 | ✅ | 单测 `d5_default_amount_is_unpaid_not_total` + 页面 8 项 |
| O5 错误提示：超付报「本次 X > 未付 Y」 | ✅ | 单测 `d5_overpay_is_rejected_with_exact_message`、真机 ⑤ 原文 |
| L9 幂等：已结清不显示按钮 | ✅ | 单测 `d5_settled_payable_is_blocked_with_reason` + 页面 ③ |

---

## 1. 动手前实测到的「现状」（不是照抄卡面）

```
GET  /business/erp/payable/{id}             已有（P2-C2，PayableDetailRes 含 payments）
POST /business/erp/payable/{id}/pay         已有（P17：有已发布流程 → 202 发起审批；没有 → 降级直付）
POST /business/erp/payable/{id}/cancel      已有（P17 反向：作废时 cancel_pending_of_payable 终止在跑审批）
前端 erp/payable/index.vue                  已有付款弹窗 / 付款记录时间线 / 审批进度
```

**真正的缺口只有两处**：

1. 没有「一次读全可付信息」的接口 —— 前端要靠状态自己推「能不能付」，与后端
   `validate_payment_target` 的口径各写一份；
2. **详情抽屉里没有「发起付款」按钮**（原来只有列表行上的「付款」），
   且付款记录没有回链（应付单号 / 凭证号都是纯文本）。

所以本卡**不重写付款逻辑**（硬红线 2）：新增一个只读接口 + 抽屉交互，写路径一行未改。

---

## 2. 后端

### 2.1 `GET /business/erp/payable/{id}/payment-info`

`backend/src/business/erp/payable_payment.rs`（与 P7/P17 同模块，复用 `payments_res` /
`pending_payment_of` / `status_label` / `is_overdue`）：

```jsonc
{
  "payable": {
    "id", "payable_no", "supplier_id", "supplier_name",
    "source_type", "source_id", "source_no",        // source_id 给前端跳收货单
    "currency_code", "amount", "paid_amount", "unpaid_amount",
    "status", "status_label", "due_date", "overdue", "period_code",
    "can_pay": true,
    "blocked_reason": null,
    "default_amount": "226.0000",                    // ← = 未付（卡面 F2/O1 的唯一落点）
    "pending_payment_no": null, "pending_amount": null,
    "overpay_risk": null                             // 审批中那笔已超付时的提示
  },
  "payments": [ { "payment_no", "amount", "pay_date", "approval_status",
                  "approval_status_label", "voucher_no", "pending", … } ]
}
```

**判定表**（`payment_capability()` 纯函数，单测直接打，不碰库）：

| # | 条件 | `can_pay` | `blocked_reason` |
|---|---|---|---|
| ① | `CANCELLED` | false | 已作废（CANCELLED），不能再付款 |
| ② | `SETTLED` | false | 已结清（未付金额 0），无需再付款 |
| ③ | `unpaid_amount ≤ 0` | false | 未付金额为 0，没有可付的余额 |
| ④ | 有一笔 `PENDING` 付款 | false | 该笔在审批中（PM-xxx，金额 X） |
| ⑤ | 其余（OPEN / PARTIAL_PAID） | true | — |

> ④ 是卡片没写但**必须**有的一条：后端 `submit_for_approval_tx` 对「已有审批中的付款」返回 409
> （`uk_biz_payment_pending` 的软闸）。不在这里挡住，用户填完金额点确认才被拒一次。

**权限**：用既有的 `business:erp:payable:detail`（与 `GET /payable/{id}` 同权）。
不新种权限码 ⇒ **本卡零迁移**（生产部署日志「应用 0 个迁移」即为证据）；
真正的付款仍由 `POST /payable/{id}/pay` 的 `business:erp:payable:pay` 把关（真机 ⑦ 验到 403 原文）。

**为什么不开事务 / 不加锁**：这是一次只读快照，`payable` 与 `payments` 之间允许毫秒级并发差；
护栏在写路径（`lock_payable` + `validate_payment_target`）。加锁读只会让「开抽屉」和「正在付款」互相等。

**超付风险（卡面 ③ 反向）**：`overpay_risk` = 审批中那笔 > 当前未付时给一句人话
（「审批通过时这笔会被拒绝（账不会动）」）。只是提示 —— 真正的拦截在 `settle_approved_in_tx`
（执行时 `amount > unpaid` → 付款记录标 `REJECTED`，账一分不动）。

---

## 3. 前端（`views/business/erp/payable/`）

### 3.1 详情抽屉顶部

* `[发起付款（未付 226.00）]`：`can_pay` **且** 持有 `business:erp:payable:pay` 才渲染（`v-if`，不是置灰）；
* 不能付时同一位置换成一句原因（`payable-pay-blocked`），文案**直接用后端 `blocked_reason`**；
* 审批中那笔已超付时，抽屉顶部再出一条黄色 `payable-overpay-risk`；
* 付款后重拉 `payment-info`：预填金额自动从 226 变 126（第二次发起带剩余），结清后按钮消失。

### 3.2 付款记录（F3 可追溯）

每条付款记录：`金额 / 付款单号 / 审批状态 / 付款方式 / 应付单 AP-xxx（可点回）/ 凭证 PZ-xxx（可点回）/ 重新提交`。

* 「应付单 AP-xxx」→ `/business/erp/payable?payable_no=AP-xxx`（应付页新增深链，自动开抽屉）；
* 「凭证 PZ-xxx」→ `/business/erp/voucher?voucher_no=PZ-xxx`（凭证页新增深链，自动按凭证号筛）；
* 来源单号 → `/business/erp/stock-in?in_id=<source_id>`（收货单页原生支持 `in_id`）。

> 卡面写「点付款单号 → 可跳到付款详情（**如果有页面**）」：系统里没有独立付款详情页，
> 付款单号保持纯文本；可追溯由「付款凭证 + 应付单号」两条链承担（都真的能点进去）。

---

## 4. 后端单测（14 条，`payable_payment::tests::d5_*`）

```bash
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_d5 \
  cargo test --lib business::erp::payable_payment::tests::d5_ -- --test-threads=1
```

```
running 14 tests
d5_cancel_payable_revokes_pending_payment ... ok            ← 卡面 ⑧ 作废撤审批（+ 实例终态 CANCELLED）
d5_cancelled_payable_is_blocked_with_reason ... ok          ← ④ 已作废 can_pay=false
d5_capability_table_without_db ... ok                       ← ⓪ 判定表（纯函数，无库也真跑）
d5_default_amount_follows_unpaid_after_payment ... ok       ← ⑥ 付 100 后预填变 126
d5_default_amount_is_unpaid_not_total ... ok                ← ⓪ 500/200 → 预填 300 而非 500（F2）
d5_missing_payable_is_not_found ... ok                      ← ⑪ 404 + 10002
d5_open_payable_can_pay_with_full_unpaid ... ok             ← ① OPEN：can_pay=true, unpaid=amount
d5_overpay_is_rejected_with_exact_message ... ok            ← ⑦ 400「本次付款 126.01 超过未付余额 126.0000」
d5_overpay_risk_only_when_pending_exceeds_unpaid ... ok     ← ⓪ overpay_risk 触发条件
d5_overpay_risk_when_unpaid_shrinks_during_approval ... ok   ← ⑩ 审批期间被别处付掉 → 提示
d5_partial_paid_unpaid_is_amount_minus_paid ... ok          ← ② unpaid = amount - paid
d5_pay_from_payable_backfills_payable_id ... ok             ← ⑤ 付款单 payable_id 回填 + 账未动
d5_pending_payment_blocks_second_submit ... ok              ← ⑨ 审批中 can_pay=false + 再提交 409
d5_settled_payable_is_blocked_with_reason ... ok            ← ③ 已结清 can_pay=false + 原因
test result: ok. 14 passed; 0 failed
```

同模块回归（同一把 `DB_LOCK`，一起跑）：

```
business::erp::payable_payment::tests::p7_*  → 10 passed; 0 failed
business::erp::payable_bridge::tests::p17_*  → 22 passed; 0 failed
```

全量（干净隔离库 `cloudpivot_d5full`，V0137–V0188 全量迁移 + admin 已种）：

```bash
cargo test --lib -- --test-threads=1
test result: ok. 1758 passed; 0 failed    # 451.73s（基线 1744 + 本卡 14）
```

## 5. 前端单测（14 条）

```bash
pnpm typecheck      # vue-tsc --noEmit：绿
pnpm test:unit      # 364 passed / 0 failed（基线 350 + 本卡 14）
```

覆盖：预填金额取 `default_amount`（含灰度期无该字段的兜底）、`can_pay=false` ⇒ 不显示按钮、
无付款权限 ⇒ 不显示、可付信息未到时不出按钮、三段深链（收货单 / 凭证 / 应付单）与空值不造假链接。

---

## 6. 隔离实例真机（`cloudpivot_d5`，18095，**真实 HTTP + 真审批**）

```bash
bash scripts/verify/d5_instance.sh up
D5_BASE_URL=http://127.0.0.1:18095 D5_DB_NAME=cloudpivot_d5 \
  python3 scripts/verify/d5_payable_payment_e2e.py
# == 汇总：42 PASS / 0 FAIL ==
```

两个账号（硬红线 2：**走审批不绕过**）：

* `d5_maker`：非超管，只持 `business:erp:payable:{pay,list,detail}` —— 制单人；
* admin / `d5_approver`：超管 —— 按待办上的真实 assignee 逐节点投票（制单人不能批自己）。

关键输出（节选）：

```
① OPEN 应付 AP-D5E2E-A → payment-info
  [PASS] ① can_pay = true
  [PASS] ① 预填金额 = 未付 226（F2/O1） —— 226.0000
  [PASS] ① 供应商名称带出 —— D5 验收供应商   [PASS] ① 来源收货单号带出 —— IN-D5E2E-A
② 付 100（提交审批 → 审批人逐节点通过）
  [PASS] ② 付款走审批（HTTP 202 + PENDING_APPROVAL）
  [PASS] ② 审批中 can_pay = false（按钮消失）
  [PASS] ② 原因点名「审批中」 —— 该应付单已有一笔付款在审批中（PM-20261101-000030，金额 100.0000）…
  [PASS] ② 审批中账一分没动（已付仍是 0） —— 0.0000
  [PASS] ② 状态 = PARTIAL_PAID   [PASS] ② 未付 = 126   [PASS] ② 付款记录 1 条且审批已通过
  [PASS] ② 付款记录带出付款凭证号 —— PZ-20261101-000016
③ [PASS] 预填 = 剩余未付 126（不是应付总额 226）
④ [PASS] 状态 = SETTLED  [PASS] 结清后 can_pay = false（按钮消失）  [PASS] 两笔金额分别是 100 / 126
⑤ [PASS] 超付 400 —— 本次付款 226.0100 超过未付余额 226.0000（应付单 AP-D5E2E-B，应付 226.0000 / 已付 0.0000）
⑥ [PASS] 审批实例已终止 —— CANCELLED  [PASS] 付款记录标 CANCELLED（作废撤审批）
⑦ [PASS] 只读账号能看 payment-info（200）  [PASS] 只读账号不能付款（403 无权限访问，需要权限: business:erp:payable:pay）
   [PASS] 清理：夹具应付单残留 = 0
```

## 7. 页面验收（Playwright，只读，不提交付款）

隔离实例（`vite` 5195 → 后端 18095，夹具三张应付单）：

```
[PASS] ① OPEN 应付：详情抽屉出现「发起付款」按钮
[PASS] ① 按钮文案带出未付金额（发起付款（未付 226.00））
[PASS] ① 付款弹窗金额预填未付（226.00）
[PASS] ② 部分已付：按钮带的是**未付 126**（发起付款（未付 126.00））
[PASS] ② 预填金额 = 未付 126（不是应付总额 226）
[PASS] ③ 已结清：没有「发起付款」按钮（L9）
[PASS] ③ 已结清：给出一句原因（应付单 AP-D5PG-SETTLED 已结清（未付金额 0.0000），无需再付款）
[PASS] ④ 无 console error（除通知 SSE 的 3 次 401，与应付页无关）
== 汇总：8 PASS / 0 FAIL ==
```

**生产页面（144 现网数据、现网 dist）**：

```
样本：可付 AP-20261101-000002（未付 226.0000） / 不可付 AP-20261031-000012（已作废）
[PASS] ① AP-20261101-000002 详情出现「发起付款」按钮
[PASS] ① 按钮文案带出未付金额（发起付款（未付 226.00））
[PASS] ① 付款弹窗预填 = 未付 226.0000
[PASS] ② AP-20261031-000012（CANCELLED）没有「发起付款」按钮
[PASS] ② 页面原因与接口一致
[PASS] ③ 无 console error
== 汇总：6 PASS / 0 FAIL ==
```

> 生产没有 PARTIAL_PAID / SETTLED 的应付单（11 张凭证里没有一笔付款），
> 所以「部分已付 → 预填剩余 → 结清后按钮消失」这三步的真机证据来自隔离实例 ②③④ + 隔离页面 ①②③。

---

## 8. 生产部署（172.18.10.144）

```
代码：a87f6a9（origin/main 一致，ls-remote 已核）
迁移：**0 个**（本卡不新增权限码 / 不改表）
二进制：/opt/cloudpivot/backend/target/release/cloudpivot  md5 014d6b157ac32b70cecd9862c44b4de3
进程==磁盘：md5(/proc/3608911/exe) = 014d6b15… = md5(磁盘)      PROC_EQ_DISK=OK
旧二进制备份：/root/cloudpivot.bak.20261101212814
health：200   首页：200
前端 dist：入口 chunk BasicLayout-DH-WQFgr.js → BasicLayout-BvIBdKu5.js
           新包特征逐个 grep 命中（各 1 个文件）：
           btn-payable-drawer-pay / payable-pay-blocked / payable-overpay-risk /
           payable-payment-voucher- / payable-payment-payable- / payable-drawer-actions
启动公告：数据库迁移完成: 应用 0 个迁移 + 四条既有自检全通过
```

### 生产只读验收（`scripts/verify/d5_prod_readonly_check.py`，46 项全绿）

对生产全部 8 张应付单逐张核对（只发 GET，跑完核对付款/凭证条数未变）：

```
[PASS] AP-20261101-000002 可付：default_amount = 未付 226.0
[PASS] AP-20261031-000012 不可付：default_amount = 0 / 原因点名「已作废」
[PASS] AP-20261031-000004 不可付：default_amount = 0 / 原因点名「已作废」
[PASS] …（共 8 张：金额恒等 amount = paid + unpaid、付款记录条数与库一致、payable_id 指回本单）
[PASS] 只读核对：付款记录数未变 —— 0    [PASS] 只读核对：凭证数未变 —— 11
== 汇总：46 PASS / 0 FAIL ==
```

> **为什么生产不真付款**：那是真金白银 + 真凭证（借 2202 / 贷 1002）。写路径的完整链路
> （提交审批 → 审批通过 → 记账 → 部分付款 → 结清 / 超付 400 / 作废撤审批）已在隔离实例上
> 用真实 HTTP 打过 42 项；生产这一步证明「新接口在真实业务数据上不出错、口径与库列一致」。

### ERP 不变量

```
生产 144：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0     （与上线前一致，未退）
隔离库 cloudpivot_d5：31/32，BLOCKER 0，WARN 1
        └ [账账] B3 应付未付 vs 2202 贷方余额 差 226：**单测/页面夹具**直接 INSERT 的应付单
          没有对应的进项凭证（真实来源是收货过账，P2-C1 才生成 2202），属夹具固有差异，非代码问题；
          清掉夹具后差值随夹具消失（生产 32/32 即无此情况）。
```

---

## 9. 与卡面的差异（逐条写明理由）

| 卡面 | 落地 | 理由 |
|---|---|---|
| 只写「已结清 / 已作废时 `can_pay=false`」 | 另加「有一笔付款在审批中」也 `can_pay=false` | 后端 `submit_payment` 本就 409；不挡就是「填完金额才被拒」，卡面 L9 的意图是「不给点不动的按钮」 |
| 未规定 `default_amount` 在不能付时取什么 | 不能付时 = 0 | 前端因此不需要再判一次状态；`canStartPay` 还要求金额 > 0，避免出现「预填 0 的付款弹窗」 |
| 「点付款单号 → 付款详情（如果有页面）」 | 付款单号纯文本，改由「凭证 PZ-xxx + 应付单号」两个深链承担可追溯 | 系统里确实没有付款详情页；不做假链接 |
| 接口权限码未指定 | 复用 `business:erp:payable:detail` | 不新种权限码 ⇒ 零迁移、零菜单节点；付款动作仍由 `:pay` 把关（真机 ⑦ 验过 403） |

## 10. 已知缺口 / 风险

1. **生产没有「部分已付 / 已结清」样本**，所以卡面真机 ②③④（未付递减 / 预填剩余 / 结清后按钮消失）
   的**生产**证据缺位，改由隔离实例 + 真审批补足；一旦生产出现第一笔付款，这些路径就自然被覆盖。
2. `payment-info` 是只读快照，不锁行：极端并发下「`can_pay` 说能付」与「点了付款返回 409」可能同时出现，
   这时报错文案（409「已有一笔付款在审批中」/ 400「本次 X 超过未付 Y」）已经把话说清楚。
3. 应付单**没有「付款申请草稿」态**：从抽屉发起付款是「一次提交一条付款记录（PENDING）」，
   不支持分次保存草稿。卡面未要求。
4. 早期（P17 之前）的老库若存在 `approval_status` 为空的历史付款记录，`payments_res` 会按原值透传
   （标签原样返回）；当前生产 0 条付款记录，无此数据。
