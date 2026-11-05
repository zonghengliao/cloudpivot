# 手册「左侧菜单路径」与实测菜单树对齐（t_ff4f92eb / P2-2B-FIX-2）

## 一、结论

手册里 7 处「分组 → 页面」写错（4 处「左侧…」+ 3 处行内引用），**全部以实测菜单树为准改手册，未动 `sys_menu`、未动迁移、未动路由与权限码**。
`sys_menu` 的挂载是 `V0154` 显式设计的采购/仓库分组，属产品口径；错的是手册文字，不是菜单。

| 页面 | 路由 | 手册原写 | 实测父链（本目录 `01-menu-api-parent.txt`） | 处理 |
| --- | --- | --- | --- | --- |
| 应付单 | `/business/erp/payable` | 财务管理 → 应付单 | **采购管理** | 改手册（08 / 11 ×2 / 12 / 21） |
| 即时库存 | `/business/erp/inventory` | 采购管理 → 即时库存 | **仓库管理** | 改手册（06） |
| 库存调拨 | `/business/erp/transfer` | 采购管理 → 库存调拨 | **仓库管理** | 改手册（07） |
| 出库单 | `/business/erp/stock-out` | 采购管理 → 出库单（缺层级） | **采购管理 → 采购订单 → 出库单** | 补全层级（05） |
| 盘点单 | `/business/erp/stock-take` | 手册未覆盖 | 采购管理（仅取证） | **不动**（归组待业务口径） |

## 二、取证方式（可复跑）

实例：前端 `http://127.0.0.1:5180`（vite，`/api` 代理到 `127.0.0.1:18097`）+ 后端 `127.0.0.1:18097` + 库 `cloudpivot_manual2`，账号 `admin`。

```bash
# 后端（本轮按需拉起；卡面记录该实例此前未在跑）
cd backend && NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18097 \
  NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_NAME=cloudpivot_manual2 \
  NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=*** \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=*** CLOUDPIVOT_STORAGE_MASTER_KEY=*** NIK_REDIS_ENABLED=false \
  ./target/debug/cloudpivot

# 取证（只读：登录 → /api/system/auth/menus → 展开侧边栏 → 打开列表页）
MANUAL_BASE=http://127.0.0.1:5180 OUT=docs/evidence/20261105-menu-path-fix/after \
  node scripts/evidence/menu_path_sidebar.mjs
```

脚本 `scripts/evidence/menu_path_sidebar.mjs` 是 t_d1a5324f 的 `yearend_menu_sidebar.mjs` 泛化版：
目标页面、期望父链写在文件头 `TARGETS` 里，任一页面的 API 父链与期望不符，或侧边栏一级分组与 API 父链不一致，脚本 `exit 1`（fail-closed）。

## 三、产物

| 文件 | 内容 |
| --- | --- |
| `01-menu-api-parent.txt` | 5 个页面的父链断言（4 PASS + 盘点单仅取证）+ 采购/仓库/财务三个分组当前全部子节点 |
| `menus-api.json` | `/api/system/auth/menus` 原始响应（侧边栏数据源） |
| `02-sidebar-dom.txt` | 侧边栏完整 DOM 层级文本（采购管理 / 仓库管理 / 财务管理 逐组展开） |
| `02-sidebar-{1,2,3}-*.png` | 三个分组展开后的侧边栏 |
| `03-sidebar-<页面>.png` | 打开该页面后，所在分组逐级展开的侧边栏（页面条目高亮） |
| `04-page-<页面>.png` | 该页面全屏（侧边栏 + 列表） |
| `crumb-<页面>.txt` | 面包屑原文（路由口径，与分组无关，只作参考） |
| `page-results.json` | 每页采集结果（API 父链 / 侧边栏分组 / 截图路径 / 错误） |

## 四、脚本输出（本轮）

```text
[API] 应付单（/business/erp/payable）→ 父链 采购管理  menu_type=2  PASS
[API] 即时库存（/business/erp/inventory）→ 父链 仓库管理  menu_type=2  PASS
[API] 库存调拨（/business/erp/transfer）→ 父链 仓库管理  menu_type=2  PASS
[API] 出库单（/business/erp/stock-out）→ 父链 采购管理 → 采购订单  menu_type=2  PASS
[API] 盘点单（/business/erp/stock-take）→ 父链 采购管理  menu_type=2  （仅取证）

[API] 分组 采购管理 子节点 = 收货入库 / 应付单 / 采购订单 / 供应商 / 采购明细 / 盘点单
[API] 分组 仓库管理 子节点 = 仓库管理 / 库位管理 / 序列号管理 / 即时库存 / 库存调拨 / 库位库存 / 库存流水
[API] 分组 财务管理 子节点 = 会计科目 / 会计期间 / 期初余额 / 年度结转 / 预算管理… / 固定资产… / 凭证查询 / 总账 / 科目余额表 / 资产负债表 / 利润表 / 现金流量表 / 单号序列

[page] 应付单   侧边栏分组=采购管理  面包屑=首页 应付单
[page] 即时库存 侧边栏分组=仓库管理  面包屑=首页 即时库存
[page] 库存调拨 侧边栏分组=仓库管理  面包屑=首页 调拨管理
[page] 出库单   侧边栏分组=采购管理  面包屑=首页 采购订单
[page] 盘点单   侧边栏分组=采购管理  面包屑=首页 盘点单

API 断言失败: 0
侧边栏与 API 父链不一致/采集失败: 0
```

## 五、手册截图是否需要重拍：**不需要**

手册里这 5 个页面的真机截图（`images/finance/01-finance-payable.png`、`images/p2b/payable/01-payable-list.png`、
`images/inventory/01-inventory-stock.png`、`images/transfer/01-transfer-list.png`、`images/stockout/01-stockout-list.png`）
都是真机采集，侧边栏语义天然正确。逐张目视核验（父分组高亮）结果：

| 截图 | 侧边栏高亮的分组 | 与实测菜单树 |
| --- | --- | --- |
| `images/finance/01-finance-payable.png` | 采购管理（展开态收起，一级分组高亮） | 一致（应付单在采购管理下） |
| `images/p2b/payable/01-payable-list.png` | 采购管理 | 一致 |
| `images/inventory/01-inventory-stock.png` | 仓库管理 | 一致 |
| `images/transfer/01-transfer-list.png` | 仓库管理 | 一致 |
| `images/stockout/01-stockout-list.png` | 采购管理（出库单在采购管理→采购订单下） | 一致 |

即：**错的是手册文字，截图本来就对**，因此不重拍、不替换任何 `images/**`。
若日后要求截图把「分组 → 子项」两级层级也拍进去，可用本目录 `03-sidebar-*.png` 作为参照（这些已含展开层级）。

## 六、边界

- 未改 `backend/`、未改任何迁移文件（尤其**盘点单**归组：`/business/erp/stock-take` 实测挂「采购管理」，
  按功能更像「仓库管理」，但归组属产品决策，本卡只取证不改）。
- 未改 `/api/...` 路由、未改任何权限码。
- 本轮为取证按需拉起 18097 后端（原进程未在运行），取证结束后已关闭，不留后台进程。
