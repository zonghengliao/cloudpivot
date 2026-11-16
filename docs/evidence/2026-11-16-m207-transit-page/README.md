# M2-07 在途库存页 — 证据

> 卡：`t_10d54ac1`（M2-07【M2·仓库】在途库存：前端页面）
> 结论：新增独立页面 `/business/erp/inventory-transit`（**在途明细 + 在途汇总** 两个 Tab），
> 只新增 **1 个页面显形码 + 1 行菜单**（迁移 `V0423`）与前端页面 / API / 纯逻辑 / 路由 / 单测；
> **后端生产代码 0 行改动**（读的是 M2-04 已落地的两个只读接口）、**表结构 0 改动**。

## 1. 交付物

| 文件 | 作用 |
|------|------|
| `backend/app/migrations/business/V0423__erp_inventory_transit_page.up.sql` | 页面码 `business:erp:inventory-transit:page` + 菜单页面行（父=仓库管理 …0e01，sort=45）+ 授权 + 祖先闭包 + 7 项自检 |
| `backend/app/migrations/business/V0423__erp_inventory_transit_page.down.sql` | 只删本卡种下的行（角色授权 → 菜单行 → 因本卡才补的父目录），幂等 |
| `backend/app/src/system/menu/permission_audit_tests.rs` | `ROUTE_LEVEL_ONLY` 登记本卡的页面显形码（C 类：纯前端码），并顺带补登记 M1-23 漏登记的 `business:erp:report:receivable-aging`（见 §6） |
| `frontend/src/views/business/erp/inventory-transit/index.vue` | 页面：Tab1 明细（ListPage：物料/仓库/客户/发货单/状态/日期区间 + 分页 + 空态）、Tab2 汇总（筛选 + KPI + 物料×仓库合并行） |
| `frontend/src/views/business/erp/inventory-transit/transit-logic.ts` | 纯逻辑：拼参（`status` 缺省整键缺席、日期区间拆两端）、数量格式化、账龄分级、空态、403 人话 |
| `frontend/src/api/business/erp/inventory-transit.ts` | API 层：`TransitRow` / `TransitSummary` 等类型（逐字抄后端 DTO）+ 两个只读函数 |
| `frontend/src/router/index.ts` | 手工注册路由（`meta.permission` = 页面显形码；漏注册 `system::menu::tests::n08` 判红） |
| `frontend/tests/unit/m207-transit-page.test.ts` | 32 条单测：纯逻辑 + 页面/路由/迁移/API 源码契约 |
| `scripts/verify/m207_transit_page_drill.sh` | 库侧验收 29 条断言（节点形状 / 菜单不变量 / 上游形状未动 / 授权成对 / 三方 path 一致 / up-down 幂等） |
| `scripts/verify/m207_transit_page_fixture.sql` | 真机验收夹具（幂等 SQL：1 单位 / 2 仓 / 3 物料 / 2 客户 / 2 订单 / 3 发货单 / 5 在途行） |

## 2. 两个权限码（本卡最容易踩的一处）

| 码 | 挂在哪 | 管什么 | 本卡动作 |
|----|--------|--------|------|
| `business:erp:inventory-transit:page` | **V0423 菜单页面行**（…4230） | 侧栏显形 + 路由 `meta.permission`，**后端零引用** | 本卡种 |
| `business:erp:inventory-transit:list` | V0421 按钮行（…4210，挂「即时库存」页下） | `/inventory-transit/page` 与 `/summary` 的框架闸 | 不动 |
| `business:erp:inventory-transit:audit` | V0422 按钮行（…4220） | 对账接口（M2-05） | 不动 |

**为什么不把页面行的 `auth_code` 直接写成取数码**（两条硬约束）：

1. 同一个码在 `sys_menu` 出现两行 = 「角色-菜单」授权页两个同名勾选框（勾一个另一个不亮），
   `V0412` / `V0418` 的自检注释已把这种情形判为缺陷；
2. 更硬：M2-04 / M2-05 / M2-06 的验证据本把取数码的**形状**钉死了 ——
   `scripts/verify/m204_transit_query_drill.sh` 断言 `business:erp:inventory-transit:list`
   在 `sys_menu` 里**只有 1 个未删节点、`menu_type=3`、`path IS NULL`、父节点是「即时库存」**。
   把该码挪到页面行会直接判红上游验收。

与 `V0418`（应收账龄页 `business:erp:report:receivable-aging` vs 取数码
`business:erp:receivable:aging`）同形 —— 「页面显形码」与「取数码」分家是 ERP 既有口径（PERM-01 方向）。

**授权成对**（自检 7.1 / 7.2）：凡持有取数码的角色一并授予页面码（数据驱动，不手抄名单）+ 超管显式补一行；
并给拿到页面码的角色补祖先目录「仓库管理」（缺父目录时 `build_menu_tree` 静默丢子树，侧栏整棵消失）。

## 3. 迁移

```
V0423 页面行： id=…4230  path=/business/erp/inventory-transit
              component=business/erp/inventory-transit/index
              parent=…0e01（仓库管理） sort_order=45  title=在途库存  icon=swap
授权：持 business:erp:inventory-transit:list 的角色 + SUPER_ADMIN
      （实测演练库命中 5 个角色：FINANCE_MANAGER / GENERAL_MANAGER / PURCHASE_MANAGER /
        WAREHOUSE_MANAGER / SUPER_ADMIN）
```

* **sort_order = 45**：同父 menu_type=2 占 5/10/20/30/40/50/60/70/80/90/95，45 空着且正好接在「即时库存」后面
  （约定 12：同父 sort 必须唯一）。
* **id = …4230**：4210/4220 已被 V0421/V0422 的按钮占用（先落地者定号）。
* **icon = swap**：TDesign 图标库里真实存在（`menu_invariants` ⑧ 校验通过）。
* **幂等实测**：`03_drill.out` ⑥ 段 —— 重放 up 节点/授权行数不变；down 归零；down 再跑仍 0；重新 up 恢复。
* **空库首启**：角色种子晚于版本化迁移 ⇒ V0421/V0422/V0423 的授权段在**首次启动**时是空操作
  （迁移只打 NOTICE，不抛异常，与 V0421/V0422 同款）。实测：空库首启 355 个迁移通过，
  随后按 V0421 → V0422 → V0423 重放一遍即补齐 5 个角色的授权（`V0423 自检通过：页面授权 5/12 角色`）。

## 4. 页面口径（不算账）

* `qty_in_transit` 是 PG **生成列**（`shipped_qty - received_qty`），任何写入路径都写不出「不符」的值；页面直接用。
* `age_days` / `max_age_days` 由 PG 按 `CURRENT_DATE - ship_date` 算（浏览器时区不参与）；页面只按
  7/8~/30/31+ 上色（<8 绿、8~30 橙、≥31 红）。
* 汇总的 `total_qty_in_transit` / `total_shipments` / `total_lines` 是后端**另一条聚合查询**算的，
  `truncated=true` 时「行不全、合计仍是全量」；页面**没有任何 `reduce`**（单测断言源码里不出现），
  截断时给一句明确提示。
* 缺省只看 `IN_TRANSIT`（后端默认值）：想看已签收 / 已作废必须显式选状态 —— 这是 M2-06
  `downstream_contract` 里写明的期望文案，页面照此实现（`02_detail_received.png` 是反证）。
* 主数据下拉（物料/仓库/客户/发货单）**先判权限再拉**：持在途码但没有 `material:list` 等码的角色，
  下拉为空 + 顶部一句人话，列表本身照常。

## 5. 门禁与实测（全部真实输出）

### 5.1 门禁

| 门禁 | 结果 | 证据 |
|------|------|------|
| `cd backend && cargo fmt -- --check` | rc=0 | `06_gates.out` |
| `cd backend && cargo clippy --lib --all-targets -- -D warnings` | rc=0（56s，0 warning） | `06_gates.out` |
| `cd frontend && pnpm typecheck` | rc=0 | `06_gates.out` |
| `cd frontend && pnpm typecheck:unit` | rc=0 | `06_gates.out` |
| `cd frontend && pnpm build` | 成功（1m31s），页面 chunk 产出（`dist/assets/index-D9idlnyF.js` 含「在途汇总」/`inventory-transit`） | 本次会话实测 |
| `cd frontend && pnpm test:unit`（全量） | **2007 / 2007 pass**（本卡新增 32 条） | 本次会话实测 |
| `cargo test --lib system::menu::permission_audit_tests` | **8 passed**（N14~N21 全绿） | 本次会话实测 |
| `cargo test --lib system::menu::tests` | **14 passed**（含 n08：菜单 path 必须能在前端路由里解析） | 本次会话实测 |
| `bash scripts/verify/m207_transit_page_drill.sh cp_m207_fresh` | **PASS=29 FAIL=0 / ALL PASS** | `03_drill.out` |
| `bash scripts/verify/menu_invariants.sh cp_m207_fresh` | **8 条全绿**，菜单总数 **516**（400–520） | `04_menu_invariants.out` |
| `node scripts/shell-variant-report.cjs --baseline scripts/shell-variant-allowlist.json` | **无新增页面壳类声明**（首次 push 时被判 1 处 `.tool-row` 声明 → 改成复用 `src/style/shell.less` 的壳类，页面不再重新定义壳类） | 本次会话实测 |

### 5.2 真接口（35 条断言，`05_api_check.out`）

跑法：99 上起隔离实例（`backend/target/debug/cloudpivot`，库 `cp_m207_fresh@172.18.10.145`，端口 `18207`），
灌夹具后跑 `m207_api_check.py`：

* 登录 → `/system/auth/menus` 菜单树含 `/business/erp/inventory-transit`，`title=在途库存`、
  `auth_code=页面显形码`、`component=business/erp/inventory-transit/index` 逐字一致；
* `/inventory-transit/page` 缺省 3 行（全 `IN_TRANSIT`），Σ`qty_in_transit`=130，物料/仓库/客户名与 `age_days` 均已带；
* 显式 `status=RECEIVED` → 1 行已签收（在途量 0）；`status=CANCELLED` → 1 行已作废；
* `/inventory-transit/summary` → 2 组（螺丝×主仓 110 / 钢板×中转仓 20），合计 130、3 张单、3 行、`truncated=false`；
* 筛选：`material_id` / `warehouse_id` / `customer_id` / `shipment_id` / 日期区间各命中预期；
* 参数非法 4 例（非法 UUID / 非法日期 / 未知状态 / 区间反了）→ **400 + 66006**；
* 明细全量 Σ == 汇总 `total_qty_in_transit`（同一 WHERE 口径的两个面读数相等）；
* 匿名调两个面 → 401。

### 5.3 真机（Playwright/CDP，Vite dev + 真后端）

| 证据 | 内容 |
|------|------|
| `00_detail.png` | 明细 Tab：6 个筛选 + 3 行在途（35 天红 / 10 天橙 / 3 天绿 tag）、已发货/已签收/在途量、分页「共 3 条数据」、侧栏「仓库管理 → 在途库存」高亮 |
| `01_summary.png` | 汇总 Tab：KPI（在途合计 130 / 发货单 3 张 / 在途行 3 行 / 组合 2 组）+ 2 行合并（螺丝×主仓 110、钢板×中转仓 20） |
| `02_detail_received.png` | 明细把状态切到「已签收」→ 查询后只剩 1 行历史行（在途量 0），证明「缺省只看在途」不是数据缺失 |

夹具（幂等 SQL，跑完随库 DROP）：2 仓 / 3 物料 / 2 客户 / 2 订单 / 3 发货单 / 5 在途行
（3 行在途 + 1 行已签收 + 1 行已作废；同一物料×仓库跨 2 张单 → 汇总合并成 110）。

## 6. 顺带修复：M1-23 漏登记的页面码（**不是本卡引入**）

跑 `permission_audit_tests` 时 N14 实测报红：

```
以下菜单权限码在后端源码里 0 落地（勾给角色永不生效，BUG-SYS-05）：
business:erp:report:receivable-aging
```

该码是 `V0418`（M1-23 应收账龄页）种的页面显形码；V0418 自己写明「页面码后端零引用」，
但**没有登记到 `ROUTE_LEVEL_ONLY`**，于是 N14 在 V0418 之后一直红（M1-23 的门禁清单里没有这一组）。
本卡按 N14 给的正规修法 ③（C 类 → 登记台账 + 写理由）补了一行登记，**不动它的菜单/授权/页面**，
补后 N14~N21 全绿。M1-23 若认为该码将来要落地接口，可把登记换成注解并删这一条（N18 会持续校验台账活性）。

## 7. 已知形态 / 诚实缺口

1. **V0421/V0422 的按钮行仍挂在「即时库存」页下**（授权树上与「在途库存」页面行并列）。
   动它们的父节点会判红上游 M2-04/M2-05/M2-06 的验证据本（那三份脚本按父节点名断言），
   所以本卡**刻意不动**；等 ERP 模块统一解耦（PERM-01 §6）时一并收。
2. **对账（M2-05）没有页面**：`/inventory-transit/reconcile` 接口与 `:audit` 码都在，但本卡范围是
   「明细 + 汇总」，不夹带；对账 UI 需要单独一张卡（读 `TransitReconcileRes` 的差异码 + 汇总口径）。
3. **空库首启后需重放 V0421/V0422/V0423** 才能把授权发给业务角色（角色种子晚于版本化迁移，这是既有的
   上游口径，不是本卡引入）；超管不受影响（`*:*:*` 短路 + 显式授予）。
4. **演练库已 DROP**（`cp_m207_page` / `cp_m207_fresh`），复现按 §5 的命令重建即可；
   演练库只建在 145，库名含 `drill`/`e2e`/`test`/`cp_` 前缀。
5. HTTP/真机验收用的是**空库首启 + 夹具**，不是 144 生产数据；生产上的在途结构由 M2-02/M2-03 的真实链路产生。
