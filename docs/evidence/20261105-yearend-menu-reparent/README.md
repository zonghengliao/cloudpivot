# 年度结转菜单归属：从「采购管理」归位到「财务管理」（t_d1a5324f / P2-2B-FIX）

## 一、判定结论：**种子写错**，不是有意归组

| 项 | 内容 |
| --- | --- |
| 现象 | `sys_menu` 里 `path=/business/erp/year-end`（年度结转，菜单页 `menu_type=2`）的父节点是 `…946` = 「采购管理」（`path=/business/erp/purchase-mgmt`） |
| 结论 | **写错**。两条节点（年度结转 + 同根因的期初余额）都是照抄 V0141 时代模板片段留下的过期 `parent_id`，与「财务页挂财务管理组」这一仓库既有约定冲突 |
| 修法 | 新增迁移 `backend/migrations/business/V0236__erp_finance_menu_reparent.{up,down}.sql`，改 `parent_id` / `sort_order`；**路由 `/business/erp/year-end` 与权限码 `business:erp:yearend:*` 一个字都没动** |

### 依据（行号可复核）

1. **父节点 UUID 的来源**（两处写死同一个 `…946`）：
   - `backend/migrations/business/V0189__erp_opening_balance.up.sql:132`（期初余额 `/business/erp/opening`）
   - `backend/migrations/business/V0190__erp_year_end.up.sql:86`（年度结转 `/business/erp/year-end`）
2. **两处头注释一字不差地写着同一个已不存在的目录名**：
   - `V0189:38` 与 `V0190:35`：`-- # 权限节点（menu_type 3 = 按钮，page 挂「ERP 台账」目录 …0946 下）`
   - 而「ERP 台账」在 `V0154` 就被改名了：
     - `V0141__erp_permissions.up.sql:60` 把 `…946` 建成 `'ERP 台账'`
     - `V0154__erp_menu_reorg_business_lines.up.sql:27-30` 把 `…946` 改名 `'采购管理'`（`path` 改 `/business/erp/purchase-mgmt`）
   - ⇒ V0189/V0190 是拿 V0141 时代的模板改的，没回头核对 `…946` 现在叫什么。
3. **V0154 的归组是显式逐条挂载**（`V0154:48-68`）：财务五页 `account/period/voucher/ledger/seq` 全部挂 `…e02`=财务管理；
   年结与期初**不在**该清单里（它们晚于 V0154），父节点只能来自自己的 INSERT —— 即第 1 条的过期 UUID。
4. **没有第二处迁移移过这两条节点**：`grep -rn "year-end" backend/migrations/` 只命中 V0190（种菜单/权限）与 V0208（加 `business:erp:year-end:remind` 催办按钮），没有任何 `parent_id` 改写。
5. **后续财务模块一律挂 `…e02`**：报表 `V0183:259` / `V0184:95` / `V0185:138`、预算 `V0227:230`、资产 `V0229:240` —— 财务页 → 财务管理 才是约定。
6. **按「采购侧单据」有意挂 `…946` 的是另一批**（`V0154:48-52`）：`purchase / stock-in / payable / supplier / purchase-order-item`。年结是「损益结转 + 生成下年期初」、期初是财务手工开账台账（`biz_opening_balance`，V0190 又加了 `source=MANUAL|YEAR_END`），不属于采购侧。

### 为什么把「期初余额」一起挪

卡面只点名年度结转，但期初余额是**同一次复制粘贴**的产物（`V0189:132` 与 `V0190:86` 同源、注释同为「ERP 台账」），
而且是年结的直接上游（年结生成 `source='YEAR_END'` 的期初）。只挪年结会让同一件事的两个页面分属两个组，
比现状更难解释。迁移里两条一起改，`down` 也会一起还原。

## 二、改前 / 改后（真机）

采集脚本：`scripts/evidence/yearend_menu_sidebar.mjs`（只读：登录 → 取 `/api/system/auth/menus` → 展开目录截图）。
实例：前端 `http://127.0.0.1:5180` + 后端 `127.0.0.1:18097` + 库 `cloudpivot_manual2`，账号 `admin`。

```bash
MANUAL_BASE=http://127.0.0.1:5180 node scripts/evidence/yearend_menu_sidebar.mjs before|after
```

| 文件 | 内容 |
| --- | --- |
| `before/03-sidebar-purchase.png` | 改前：`采购管理` 展开，子项含 **年度结转**（高亮）与 **期初余额** |
| `before/04-sidebar-finance.png` | 改前：`财务管理` 展开，子项里**没有**年度结转 |
| `after/03-sidebar-purchase.png` | 改后：`采购管理` 展开，子项 = 收货入库 / 应付单 / 供应商 / 采购订单 / 采购明细 / 盘点单（年度结转、期初余额已不在） |
| `after/04-sidebar-finance.png` | 改后：`财务管理` 展开，子项 = 会计科目 / 会计期间 / **期初余额** / **年度结转** / 预算管理 / 固定资产 / 凭证查询 … |
| `after/05-page-yearend.png` | 改后：年度结转页面本体正常（前置检查五项、按钮 disabled 口径不变） |
| `*/01-menu-api-parent.txt` | 菜单 API 解析结果（侧边栏数据源）：改前父节点 `采购管理`，改后父节点 `财务管理` |
| `*/02-sidebar-dom.txt` | 侧边栏完整层级文本（含未展开子树），可对照分组 |

### 菜单 API 摘要（改前 → 改后）

```text
改前 [API] 年度结转 父节点 → title=采购管理 path=/business/erp/purchase-mgmt id=…0946   sort=26
改后 [API] 年度结转 父节点 → title=财务管理 path=/business/erp/finance-mgmt  id=…0e02   sort=22
改前 [API] 采购管理 子节点 = 收货入库 / 期初余额 / 年度结转 / 应付单 / 供应商 / 采购订单 / 采购明细 / 盘点单
改后 [API] 采购管理 子节点 = 收货入库 / 应付单 / 供应商 / 采购订单 / 采购明细 / 盘点单
改后 [API] 财务管理 子节点 = 会计科目 / 会计期间 / 期初余额 / 年度结转 / 预算管理 / 固定资产 / 凭证查询 / 总账 / 科目余额表 / 资产负债表 / 利润表 / 现金流量表 / 单号序列
```

排序口径：迁移里取 `会计期间.sort_order + 1 / + 2`（按 `path` 动态取，不硬编码数字）——
财务管理组内 会计科目(10) → 会计期间(20) → **期初余额(21)** → **年度结转(22)** → 预算(25) → 固定资产(29) → 凭证(30) → 总账(40) → 报表(45-47) → 单号(50)。

## 三、迁移验证（真引擎 + 独立库）

独立库 `cloudpivot_yemenu_a`：`pg_dump cloudpivot_manual2 | psql cloudpivot_yemenu_a`（175 条迁移记录，改前状态两条节点都在采购管理组）。
用**内存里现成的 `backend/target/debug/cloudpivot`**（不再重编译，迁移是启动期从 `migrations/` 目录读）指向该库启动：

| 轮次 | 结果 |
| --- | --- |
| 首启（改前状态） | `INFO nik_web::app: 数据库迁移完成: 应用 2 个迁移`；`schema_migrations` 出现 `236 / erp_finance_menu_reparent / success=t / execution_ms=6`；`/health` = ok；两条节点父节点 = 财务管理 |
| 二启（幂等） | `INFO nik_web::app: 数据库迁移完成: 应用 0 个迁移`，无 checksum 冲突、无报错；菜单状态不变 |
| `up → up`（psql 直跑） | 第二次 `UPDATE 1 / UPDATE 1` 且自检 DO 全通过（断言的是改完的稳定状态，不是「改了 N 行」） |
| `down` | 两条节点还原 `采购管理 / 25`、`采购管理 / 26`（= V0189:133、V0190:87 原值），自检通过 |
| `down → down` | 幂等，仍 `UPDATE 1 / UPDATE 1`，自检通过 |
| `down → up` | 回到 `财务管理 / 21`、`财务管理 / 22`，自检通过 |

> 真实教训：第一版自检写「年度结转下的按钮节点必须 = 2」，真机跑红——`V0208` 又加了第三个按钮
> `business:erp:year-end:remind`（催办，注意这个码带连字符）。改成**点名三个权限码** + 个数 `>= 3`，
> 这才是「挪父节点不许把按钮甩成孤儿」的正确断言。只用 psql 试跑不会发现，必须用真引擎跑迁移文件。

## 四、迁移设计要点

- **前置 fail-closed**：`财务管理 / 会计期间 / 期初余额 / 年度结转 / 采购管理` 五个定位点必须各**恰好 1 个**在用行；
  会计期间必须已在财务管理组内（否则 `+1/+2` 排序会插到别的组的坐标系里）；待挪节点 `menu_type` 必须是 2（页面）——
  任一条不满足直接 `RAISE EXCEPTION`，绝不静默跳过。
- **改后 fail-closed**：父节点 `path` 断言、`会计期间 < 期初余额 < 年度结转` 排序断言、
  年度结转下三个按钮权限码（`yearend:close` / `yearend:reopen` / `year-end:remind`）仍在、
  拒绝「财务管理目录挂到页面上」这种父子成环（成环会让整棵侧边栏消失）。
- **按 path 定位，不按 title**：`/business/erp/finance-mgmt`、`/business/erp/purchase-mgmt`、`/business/erp/period`
  全部用 path 查，运维改过中文标题也不影响 up/down。
- **号段**：动手前 `ls backend/migrations/*/`，已应用最大 V0235、business/ 内最大 V0234，本迁移取 **V0236**，未改任何已应用文件。

## 五、手册同步

| 文件 | 改动 |
| --- | --- |
| `docs/manual/15-年度结转.md` | 步骤 1 菜单路径 `采购管理 → 年度结转` → `财务管理 → 年度结转`；「口径提醒」改为「已修复 + 未跑 V0236 的老实例仍按老菜单说明」 |
| `docs/manual/README.md` | 第八节缺口表第 2 行标注**已修复（V0236）**并指向本目录证据 |

## 六、相邻发现（不在本卡范围，未改）

1. `应付单`（`/business/erp/payable`）挂在「采购管理」组下 —— 这是 `V0154:50` **显式**挂的（采购侧单据），
   但手册 `08-财务管理.md:44` / `11-应付.md:43` 写的是「财务管理 → 应付单」（手册与实测不一致）。
2. 手册 `06-库存查询.md:37` / `07-库存调拨.md:38` 写「采购管理 → 即时库存 / 库存调拨」，
   实测这两个页面在「仓库管理」组下（`V0154:59-60` 挂 `…e01`）。
3. `盘点单`（`/business/erp/stock-take`，V0211 种）挂在「采购管理」组下，按功能（仓库盘点）更像仓库管理组 —— 需业务口径确认。

以上 1、2 属**手册口径与实测不一致**（文档问题），3 属**菜单归属待定**（业务决策），均未在本卡改动。
