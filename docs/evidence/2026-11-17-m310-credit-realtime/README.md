# M3-10 客户信用：下单时实时占用（未清应收 + 本次订单）· 可用额度

卡：`t_8729b656`（上游 `t_464b7265` = M3-09 客户信用超限策略）
分支 / 提交：见卡内 `git log -1`

## 一句话

新增只读接口 `GET /api/business/erp/credit/{customer_id}`：**现算**「已占 + 在审 + 本单」
三个分量，给出**可用额度**与**本单会不会超**（判定复用提交守卫的同一个 `credit::decide`，
不写第二份「超没超」）；销售订单表单在选客户 / 改数量时拉它，显示
「可用额度 X / 额度 Y」「本单将超额 Z」。**本卡不加迁移、不造新权限码。**

## 改了什么

| 文件 | 改动 |
|------|------|
| `backend/app/src/business/erp/credit.rs` | 新增实时快照层：`OPEN_RECEIVABLE_STATUSES` / `open_receivable()`（未清应收）/ `available_of()`（可用额度，下限 0）/ `CreditSnapshot`（含 `occupied` / `total` / `available` / `available_after` / `over` / `cache_drift` / `verdict`）/ `snapshot_of()`（现算 used + 在审 + 未清，Optional 客户） |
| `backend/app/src/business/erp/credit_handler.rs`（新） | 1 个路由 + DTO + 查询参数解析（未知参数 400）+ 9 条单测（3 条真库：快照算术 / 策略三态 / 边界与错误） |
| `backend/app/src/business/erp/mod.rs` | 注册 `pub mod credit_handler;` |
| `frontend/src/api/business/erp/credit.ts`（新） | `fetchCreditSnapshot()` + `CreditSnapshot` 类型（字段逐字抄后端） |
| `frontend/src/views/business/erp/sales-order/sales-order-query.ts` | `creditCapsule()` 纯函数（四种态文案）+ `CreditCapsule` 类型 |
| `frontend/src/views/business/erp/sales-order/index.vue` | 客户选择下方「可用额度胶囊」；换客户立刻拉、金额变动 400ms 防抖拉；防乱序（请求序号）；取不到就不显示（不挡开单） |
| `frontend/tests/unit/erp-sales-order.test.ts` | 5 条胶囊用例（未超 / 不控额度 / BLOCK·WARN·PASS / 冻结 / 未取到） |
| `scripts/verify/m310_credit_realtime_drill.sh`（新） | 真库演练 23 断言（4 组用例 + 死权限码闸 + 夹具清场） |
| `scripts/verify/m310_credit_http_e2e.sh`（新） | HTTP 端到端 35 断言（真空库全量迁移 → 登录 → 建客户 → 打接口 → 注入占用对账 → 400/404） |

## 口径（唯一真相在代码里，这里只是索引）

```text
已占 used    = 有效订单含税(APPROVED/PARTIAL_OUT/OUTBOUND) − 已核销应收 − 已过账退货，下限 0   ← 现算
在审 pending = SUBMITTED 订单价税合计（不含本单）
占用 occupied = used + pending
合计 total    = occupied + amount（本次订单，可选参）
可用 available = 额度 − occupied，下限 0；额度 0（不控额度）→ null
超限 over      = max(0, total − 额度)
未清应收 open_receivable = OPEN/PARTIAL 应收的未结金额 —— **对照口径，不重复计入占用**
判定 verdict   = erp::credit::decide(policy, limit, used, pending, amount)   （与提交守卫同一个）
allowed        = verdict 放行 ∧ 客户未冻结
```

### 为什么「未清应收」不进占用合计（防双算）

一张已出库未收款的订单，**既**在「有效订单」里（`OUTBOUND` 仍在占用集合），
**也**在「未清应收」里。两笔相加就是 PRD §4.7.2 ③ 明令禁止的双算。所以：

* 计入占用的是 `used`（订单口径，天然覆盖「已发货未收款」）；
* `open_receivable` 单独返回，用于①界面展示「这客户还欠多少」②与 `used` 对拍排障
  （健康库 `used ≥ 未清应收`）。

### 为什么快照用现算、守卫用缓存列

* 守卫（`sales_order_handler::ensure_submittable`）在事务里必须当场判定，读缓存列 `credit_used`；
* 本接口叫「实时」，`used` 一律 `usage_of()` 三表现算 —— 缓存列万一漂移（S9 报红那种），
  这里看到的仍是真账，并把差异作为 `cache_drift=true` **显式**告诉调用方（运维信号，
  每日重算会修），不静默抹平。

## 证据（本目录）

| 文件 | 内容 |
|------|------|
| `00-drill-unit.out` | 真库演练 23/23 PASS（credit_handler 9 + credit 29 + customer 20 + sales 206，四组 SKIP=0；死权限码闸绿；夹具 0 残留） |
| `01-drill-http-e2e.out` | HTTP 端到端 35/35 PASS（真报文：`used 500.00 / pending 200.0000 / open_receivable 200.0000 / occupied 700.00 / available 300.00`；`amount=500` → `total 1200.00 / over 200.00 / verdict BLOCK / allowed false`；`amout=` 400、负数 400、未知客户 404） |
| `02-frontend-check.out` | `pnpm typecheck` 0 error；`pnpm test:unit` 2164 pass / 0 fail（+5 条本卡用例） |
| `03-backend-lint.out` | `cargo fmt --check` 0 差异；`cargo clippy --lib -- -D warnings` 0 warning |

（`pnpm build`：5618 modules transformed，`✓ built in 51.51s`，产物 `frontend/dist`。）

## 复现

```bash
# 真库单测演练（99 → 145；145 上跑时加 PG_HOST=127.0.0.1）
bash scripts/verify/m310_credit_realtime_drill.sh          # 期望 ALL PASS，PASS=23
# HTTP 端到端（空库全量迁移 + 真打接口；需要 backend/target/debug/cloudpivot 已构建）
bash scripts/verify/m310_credit_http_e2e.sh                # 期望 ALL PASS，PASS=35
```

## 踩到的坑（写给下一个人）

1. **`setsid nohup app & echo $!` 拿到的不是应用 PID** —— `setsid` 会 fork，`$!` 是立即退出的那个进程，
   `kill` 杀不掉真身，于是「上一轮的实例」占着端口应答 `/health`，脚本以为自己的实例起来了，
   所有 API 都打在了**上一个库**上（第一版就是这样把 4 条断言跑成红的，而代码其实是对的）。
   现在：端口随机 + 启动前先探测 + 收尾断言端口已释放。
2. **`used` 里减的是「所有应收的已核销额」**，含已收清（`SETTLED`）那张 —— 它虽然不进
   「未清应收」，但已核销的钱照样抵减占用。写夹具断言时容易漏这一笔。
3. **未知查询参数必须 400**：`amout=100` 被静默忽略的话，界面显示的是「不含本单」的数，
   而用户以为已经按本单试算过了 —— 比报错危险得多。
4. 复用的权限码 `business:erp:customer:list` 在 `sys_menu` 有承载行（V0261/V0267），
   `perm_menu_gate.py` 绿；单据里**不要**再造一个 `business:erp:credit:read`（那会给
   5 个销售角色补授权，否则能建单的人点开订单页反而 403）。
