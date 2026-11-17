# M3-14 · 客户信用前端页面（额度 / 占用 / 可用）—— 证据包

卡号 `t_5f9041b6`（M3-14）。本轮只做**前端**：把 M3-10/11/12/13 落地的信用快照搬进
客户详情（编辑弹窗），并把「额度 / 占用 / 可用 / 判定 / 漂移 / 冻结」摆清楚。
零后端改动、零迁移、零权限码。

## 1. 交付物

| 文件 | 作用 |
|---|---|
| `frontend/src/views/business/erp/customer/CreditPanel.vue` | 信用面板组件（自取数、加载/失败降级、占用率进度条、明细行、漂移与冻结提示、口径 tooltip） |
| `frontend/src/views/business/erp/customer/credit-panel-logic.ts` | 纯逻辑：额度/可用文案（`null` ≠ `0`）、占用率与配色、判定与策略标签、明细行定义、漂移/冻结/口径文案 |
| `frontend/src/views/business/erp/customer/index.vue` | 客户详情（编辑弹窗）内嵌面板；打开详情、保存成功后拨 `refreshKey` |
| `frontend/tests/unit/m314-credit-panel.test.ts` | 30 条单测（含「源码即契约」：字段集 / 判定枚举 / 策略字面量 / 路由前缀逐字对拍后端） |
| `docs/evidence/2026-11-17-m314-credit-panel/*` | 本证据包（截图 + 命令输出） |

数据来源**唯一**：`GET /api/business/erp/credit/{customer_id}`（M3-10，权限码
`business:erp:customer:list`）。前端不重算钱 —— 第二份公式与后端 `credit::decide`
（提交守卫用的同一函数）分叉时，屏幕写「额度内」而提交 409，比不显示更糟。

## 2. 六条验收（全部在真浏览器 + 真后端上跑）

环境：后端 `./backend/target/debug/cloudpivot`（profile=test，监听 `127.0.0.1:18089`，
库 = 145 的 `cp_m314_page2`，空库首启 369 条迁移全绿），前端 `vite --port 5189`
（`BACKEND_ORIGIN=127.0.0.1:18089`），账号 `admin`。夹具见 `05-fixtures.sql`。

| # | 场景 | 夹具 | 实见（面板文本） | 截图 |
|---|---|---|---|---|
| ① | 额度内 | 额度 100,000 / 已占 18,080 / 在审 7,540 | 可用 **74,380.00** · 额度内 · 26%（占用 25,620.00 / 额度 100,000.00） | `m314-01-within.png` |
| ② | 超限拦截 + 缓存漂移 | 额度 50,000 / 已占 60,000 / 缓存列留 0 | 已超额度 **10,000.00**（红）· 超限·拦截 · 120% · 超出额度 10,000.00 红行 · 漂移 alert | `m314-02-over.png` |
| ③ | 不控额度 | 额度 0（后端 `available = null`） | 不控额度，下单不校验信用 · **无进度条** · 可用额度=不控额度（不是 0.00） | `m314-03-uncontrolled.png` |
| ④ | 冻结 | `status=FROZEN`，额度 200,000 充足 | 顶部黄条「客户已冻结：与额度无关，任何新单都不能提交」 | `m314-04-frozen.png` |
| ⑤ | 后端不可用（降级） | 杀掉后端后打开另一家客户 | 「信用数据取不到：…（请求 ID：…）」+ 重试；**不显示上一家客户的数**；保存按钮仍可用 | `m314-05-degrade.png` |
| ⑥ | 改额度保存后重开 | 面板内保存把额度 50,000 → 120,000 | 重开显示 可用 **60,000.00** · 额度内 · 50%（占用 60,000.00 / 额度 120,000.00）；DB 侧 `credit_limit=120000.0000` 已核对 | —— |

⑤ 实测 JSON（同一轮）：

```json
{"header":"编辑客户 · M314-PAGE-02",
 "panel":"信用数据取不到：服务端处理失败，请稍后重试；若持续失败请联系管理员（请求 ID：6f8146d5-…） | 重试",
 "leakedPrevCustomer":false,
 "saveBtnEnabled":true}
```

真实接口报文（`03-api.out` 全文）：四个客户的 `verdict` 分别为
`WITHIN` / `BLOCK` / `UNCONTROLLED` / `WITHIN`，`available` 分别为
`"74380.00"` / `"0"` / `null` / `"200000.00"`；未知参数 `?amout=100` → **400**。

## 3. 真浏览器跑出来的两个 bug（都已修 + 单测钉住）

这不是「设计稿没想清楚」，是**实测才暴露**的，记录在此避免后续卡重犯：

### ① 换客户会带出上一家的信用数（数据串户）

TDesign 的弹窗**关闭后不销毁内容**：关掉「编辑客户 A」再打开「编辑客户 B」时，
面板实例被复用、`customerId` 变了但请求失败路径按「刷新失败保留旧值」处理
⇒ B 的表头下显示 A 的额度 / 冻结条。实测触发：停后端后 A→B 切换，B 的面板显示
`200,000 / 冻结`（A 的数据）。

修法两道：面板记 `snapshotCustomerId`，**换客户先清空**（同客户刷新失败才保留旧值）；
父页面 `<CreditPanel :key="editing.id">` 换客户直接重建组件。

### ② 同一家客户保存后不刷新（刚改额度看不到）

同一个「弹窗不销毁」根因：同一家客户再打开时 `customerId` 没变、watch 不触发
⇒ 把额度从 5 万改成 12 万，重开仍显示「已超 1 万」。实测（修前）：
DB 已是 `120000.0000`，面板仍 `50,000.00 / 超限·拦截`。

修法：父页面在 `openEdit()` 与保存成功后各 `creditRefreshKey += 1`，面板把它放进
watch 依赖（单测断言 watch 数组与两处拨动都在）。

## 4. 口径三条（写错就显示错的钱）

1. **`credit_limit = 0` 是「不控额度」**，此时后端 `available = null` —— 显示「不控额度（0.00）」，
   不能显示「可用 0.00」（那会被读成额度用光）；
2. **`available` 已被后端夹到 0 以上** ⇒ 「已超额」只认 `is_over` / `over`，
   写 `available < 0` 永远不成立（单测扫代码禁止该写法）；
3. **`open_receivable` 是对照口径**，不进占用合计，行上必须标「对照口径，不重复计入」。
   占用 = 已占（`used`）+ 在审（`pending`），与后端同源。

## 5. 门禁输出

| 命令 | 结果 | 文件 |
|---|---|---|
| `tsx --test tests/unit/m314-credit-panel.test.ts` | **30 passed / 0 failed** | `00-unit-m314.out` |
| `pnpm test:unit`（全量前端单测） | **2194 passed / 0 failed** | `00b-unit-all.out` |
| `pnpm typecheck` | exit=0，无输出（零错） | `01-typecheck.out` |
| `pnpm typecheck:unit` | exit=0 | `01-typecheck.out` |
| `pnpm build` | ✓ built in 52.18s（`components.d.ts` 无 diff） | `02-build.out` |
| 真接口探针（4 客户 + 未知参数 400） | 全绿 | `03-api.out` |

后端零改动 ⇒ 未跑 `cargo fmt/clippy`（本卡不涉及 `.rs`）。

## 6. 演练环境与清理

- 库：145 的 `cp_m314_page2`（**空库**首启，369 条迁移自动应用；用模板克隆的
  `cp_m314_page` 因模板库的 admin 口令不可知、且 `sys_user` 被 `biz_flow_instance`
  外键引用无法删重建，已弃用并 DROP）；
- 夹具：`05-fixtures.sql` 里 4 家人造客户（`M314-PAGE-01..04`）+ 订单/应收，全部按前缀可清；
- 收尾：验证完 `dropdb` 两个演练库、停本地实例与 vite（本机不留进程）。
