# COST-B4-4 · 成本核算模块总开关 `erp.cost.enabled`（ARCH-9 §7.4 灰度 / §8.1 B4-4）

**卡**：t_e757c1e7 · 批次 B4 收口 · 依赖 B3-11（已完成，commit `31a92986`）
**结论**：开关已落地并**真例实测通过**——参数配置页可改（默认 `false`）、关闭时菜单隐藏 + 接口 `403 + 66079` + 领料单成本对象字段隐藏、打开后全部恢复、全程**不重启**（同一个 PID）。
**证据**：`02_smoke.out`（56 条断言 0 失败，exit 0）· `11_ui_switch_state.out`（UI ↔ 库值 ↔ 探针三联对照）· `03/04/05/06/07/08/09/10_*.png`（8 张浏览器截图）。

---

## 1. 改了哪些文件

### 后端（`backend/`）

| 文件 | 内容 |
|---|---|
| `migrations/business/V0385__cost_module_switch.up.sql`（新） | 种 `sys_config` 行 `erp.cost.enabled`（`BOOLEAN` / **默认 `false`** / `config_level = 2` 业务级 / 分类 `erp_cost`）+ `config_category` 字典项「ERP 成本」+ 4 组自检（值只能是 `true`/`false`、类型/级别/可修改/分组口径、字典项存在）。**不动 `sys_menu`**（菜单是运行期过滤，见 §3） |
| `migrations/business/V0385__cost_module_switch.down.sql`（新） | 删参数行 + 字典项，带「开关当前为 true → 打 NOTICE 提醒关掉会整体下线」的护栏与删净自检 |
| `src/business/shared/cost_switch.rs`（新，456 行含测试） | 开关判定与三处接入点：`CONFIG_KEY` / `enabled_from_raw`（**默认关**）/ `is_enabled`（读 `sys_config`，命中进程内快照）/ `is_cost_api_path` / `is_cost_auth_code` / `filter_menus` / `filter_permissions` / `guard`（中间件，`403 + 66079`）。**9 条单测**：默认值口径、`true`/`1` 口径、路径闸（含 `/api` 前后缀、大小写、探针豁免、`lot`/`stock-out` 不拦）、权限码闸（含 `costume`/`lottery` 形近反例）、菜单过滤、权限过滤、迁移守卫（`include_str!` 读 V0385 断言默认值与类型） |
| `src/business/erp/cost_switch_handler.rs`（新） | 探针接口 `GET /business/erp/cost-switch` → `{enabled, config_key, hint}`（登录即可读、无权限码；关闭态提示语点名参数键与开启路径）。**3 条单测**：探针路径必须被豁免（且豁免是精确路径而非前缀）、响应体形状、提示语含参数键 |
| `src/business/shared/mod.rs` | 注册 `pub mod cost_switch` + 模块表加一行 |
| `src/business/erp/mod.rs` | 注册 `pub mod cost_switch_handler` |
| `src/system/auth/handler.rs` | `me`：`filter_permissions`（关闭时不把 `business:erp:cost-*` / `business:erp:lot:*` 下发给前端）；`menus`：`filter_menus`（关闭时 6 条成本页不进侧边栏树）。**过滤放在读路径**而非缓存里 —— 菜单/权限按用户缓存在 Redis，写进缓存就得按用户批量清，读路径现算才能「改完即生效」 |
| `src/lib.rs` | 注册中间件 `.route_layer(from_fn(business::shared::cost_switch::guard))`（链内、认证层之后：未登录仍是 401，不泄露模块是否存在） |
| `src/shared/error_code.rs` | 新增 `ERP_COST_MODULE_DISABLED = 66079`（HTTP 403）+ 66xxx 段说明与码表稳定性测试同步 |

### 前端（`frontend/`）

| 文件 | 内容 |
|---|---|
| `src/api/business/erp/cost-switch.ts`（新） | `fetchCostSwitch()` + `CostSwitchRes` 类型（路径写全 `/api/...`） |
| `src/stores/cost.ts`（新） | `useCostSwitchStore`：`enabled/loaded/hint/configKey` + `load(force)/ensure()/reset()`；**fail-safe**（取不到按关闭处理、不置 `loaded` 下次重试）；并发去重（`inflight`） |
| `src/stores/user.ts` | `reset()` 里 `useCostSwitchStore().reset()` —— 换账号不把上一个会话的开关状态带过去 |
| `src/views/business/erp/stock-out/index.vue` | 「成本对象」表单项 `v-if="costSwitch.enabled"`；`onMounted` 先 `ensure()`；`loadCostObjects()` 关闭时直接返回（不制造一条必然 403 的请求） |

> 未改：`sys_menu`（不需要）、`cost-object` 等 6 个成本页（后端过滤已足够，直接输 URL 会被路由守卫送 `/result/403`，见 `10_*.png`）。

---

## 2. 四条验收 → 证据映射

| 卡面验收 | 结论 | 证据 |
|---|---|---|
| ① 开关在参数配置页可改（默认 false） | ✅ 迁移种入 `BOOLEAN` / `false` / `config_level=2` / 分类「ERP 成本」；配置页按分类自动出现「ERP 成本」页签，控件是 `t-switch`，改完点「保存」即落库 | `02_smoke.out` 第 0 段（库值/类型/级别/分类/字典项 5 条）；`03_config_switch_off.png`（灰轨道 = 关）；`06_config_switch_on.png`（蓝轨道 = 开）；`11_ui_switch_state.out`（DOM `t-is-checked` ↔ 库值 ↔ 探针三方一致） |
| ② 关闭时：菜单隐藏 | ✅ `/system/auth/menus` 里 6 条成本页（含「批次管理」）为 0；侧边栏「财务管理」子菜单从「报表项维护」直接跳到「合同管理」；「仓库管理」末尾无「批次管理」 | `02_smoke.out` 第 2 段（admin/fm01 各 1 条 + 出库单菜单仍在）；`05_sidebar_no_cost_menus.png` |
| ② 关闭时：接口返回 403 | ✅ `/business/erp/cost-*` 与 `/business/erp/stock-out/cost-object-*` 一律 `403 + 66079`（**业务码不是权限 403**，报文点名参数键与开启路径）；未登录仍是 `401`；未配成本权限的非超管也先拿到 66079（模块未启用优先于权限不足） | `02_smoke.out` 第 2 段（5 个 cost-* + 成本对象下拉 + 未登录 401）；`10_fm01_cost_page_403.png`（非超管直接输 URL → `/result/403`） |
| ② 关闭时：领料单成本对象字段隐藏 | ✅ 出库单新建表单「出库事由 → 备注」之间没有「成本对象」；页面也不再去请求 `/stock-out/cost-object-options`（该请求此时必 403，`loadCostObjects` 直接短路） | `04_stockout_form_no_cost_field.png`（关态表单）；`07_stockout_form_with_cost_field.png`（开态同一位置出现字段）；浏览器内 DOM 断言 `hasField` 关态 `false` / 开态 `true`（脚本见 README §4 复演序列） |
| ③ 打开后全部生效 | ✅ 5 个 cost-* 页面接口 200、成本对象下拉 200、财务菜单 5 条 / admin 6 条回来、fm01 权限码回来 | `02_smoke.out` 第 3 段（14 条）；`07_stockout_form_with_cost_field.png`；`08_sidebar_fin_cost_menus.png`；`09_sidebar_lot_menu.png` |
| ④ 切换不需要重启 | ✅ 全程同一个 PID（`2755882`），只 `PUT /support/config/{id}`；改完立刻生效（探针、接口、菜单三处同时翻） | `02_smoke.out` 第 3/4 段的「实例 pid 未变」断言 |

**回归闸**：`cargo check --all-targets` exit 0；`cargo test --lib` **4778 passed / 0 failed**（本卡新增 12 条）；`pnpm typecheck` exit 0；`pnpm build` exit 0。

---

## 3. 三条实现口径（改这块代码前先读）

1. **菜单/权限是运行期过滤，不是持久改写**。`filter_menus` 在 `/system/auth/menus` 的读路径上做，`filter_permissions` 在 `/system/auth/me` 上做。好处：① 切换即时生效；② 不动 `sys_menu.is_visible`，开关重开后不会覆盖管理员手工调过的可见性；③ 角色授权树（`/system/menu/tree`、`/system/role/{id}/menus`）**不受影响** —— 管理员照样能给角色配这些菜单。
2. **接口闸是前缀匹配的中间件**，挂在 `App::route_layer`（链内、认证层之后）：`/business/erp/cost-` 与 `/business/erp/stock-out/cost-object-`。新增任何 `cost-xxx` 接口**自动纳入**，不需要回来加一行（漏一个就是「关了开关还能调」）。
   **唯一豁免**：`GET /business/erp/cost-switch`（前端必须先知道开关是开是关才能决定渲染什么；精确路径豁免，`/cost-switch/other` 仍被拦）。
3. **有意不拦的**（写清楚免得被当成漏网）：`/business/erp/lot*`（出库单/领料单的批号下拉要用，关掉成本核算不该让仓库连批号都选不了；批次**页面**的菜单仍隐藏）、`/business/erp/stock-out` 本体（领料单是仓库日常单据，只隐藏其中的成本对象字段）、其它模块对 `biz_cost_*` 的内部调用（如结账 blocker `COST_UNSETTLED`，那是数据事实与界面开关无关）。

---

## 4. 怎么重跑（都能一条命令复现）

```bash
# ① 起隔离实例（建库 costb44_switch → 自动迁移含 V0385 → 监听 :18051）
bash docs/evidence/2026-11-11-cost-b4-4-cost-switch/00_up.sh up
# ② 真例冒烟：关闭态 → 打开（不重启）→ 再关闭，56 条断言
bash docs/evidence/2026-11-11-cost-b4-4-cost-switch/01_smoke.sh
# ③ 前端（浏览器取证用）：dev :5199 代理到 :18051
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18051 pnpm dev --port 5199
# ④ 收尾
bash docs/evidence/2026-11-11-cost-b4-4-cost-switch/00_up.sh down
```

冒烟脚本自带登录/改密、建一个 `FINANCE_MANAGER` 非超管账号（`fm01`）验证「权限码过滤」，并在收尾把开关拨回 `false`（= 迁移默认值）。

**浏览器取证的操作序列**（截图对应的动作，可复演）：
1. `/support/config` → 页签「ERP 成本」（`03`：灰轨道 = 关）→ 点开关 → 点该行「保存」→ `保存成功`（`06`：蓝轨道 = 开、11 的 DOM 为 `t-is-checked`）；
2. `/business/erp/stock-out` → 新建出库单：关态无「成本对象」（`04`）、开态有「成本对象」+ 借方预览（`07`）；
3. 首页侧边栏：关态「财务管理」子菜单末尾是「报表项维护 → 合同管理」（`05`）；开态多出「成本对象 / 工时台账 / 费用台账 / 分摊规则 / 成本核算」（`08`）与「仓库管理 → 批次管理」（`09`）；
4. 换 `fm01`（财务经理）令牌后直接访问 `/business/erp/cost-object` → 落到 `/result/403`（`10`）。

---

## 5. 发现但**没动**的问题（属别的模块，已上报）

1. **参数配置页的开关文案是反的**（`frontend/src/views/support/config/index.vue:104`）：
   `:label="['关闭', '开启']"`，而 TDesign 的 `label[0]` 是「**开启态**」文案 ⇒ 开关**打开时显示「关闭」、关闭时显示「开启」**（所有布尔参数都受影响，非本卡引入）。
   判定真实状态请看轨道颜色 / `t-is-checked` / 库值（`11_ui_switch_state.out` 是三方对照）。修法一行：`['开启', '关闭']`。**已开卡给前端**，本卡未改。
2. **超管直接输成本页 URL 会看到 403 报文而不是「模块未启用」提示**：超管权限是 `*:*:*`，权限过滤摘不掉它，所以页面能渲染、接口被 403 拦（报文已点名参数键与开启路径，可自查）。这与「管理员需要看见模块存在但被关掉」是有意取舍；若产品要更友好，可另开卡在成本页读探针显示占位。

---

## 6. 环境与残留

| 项 | 值 |
|---|---|
| 隔离实例 | 库 `costb44_switch` · `127.0.0.1:18051`（`00_up.sh` 起停；跑完已 `down`，库已删） |
| 前端 dev | `127.0.0.1:5199`，`BACKEND_ORIGIN=http://127.0.0.1:18051` |
| 账号 | `admin / Admin@12345678`、`fm01 / Fm@12345678`（FINANCE_MANAGER，仅存在于该测试库） |
| 浏览器令牌 | 走接口登录 + `localStorage` 注入（`x_access_token` / `x_refresh_token`），**未在页面输口令** |
| 迁移号 | `V0385`（动手前 `ls backend/migrations/business/` 核对：基础版当时最大 `V0384`） |
