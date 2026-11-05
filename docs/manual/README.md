# 云枢 CloudPivot 使用手册

面向客户上线培训的操作手册集，分两批交付。每篇结构统一：**这个功能干什么 → 谁能用（角色 + 权限）→ 完整操作步骤（真机截图）→ 常见错误怎么办 → 与哪些功能联动**。

- **使用手册（一）：核心业务 9 个功能**（P2-2A，9 篇 / 38 张截图）
- **使用手册（二）：财务 8 个 + 平台 5 个功能**（P2-2B，13 篇 / 52 张截图）

---

## 一、使用手册（一）：核心业务 9 个功能

| # | 功能 | 手册 | 截图 |
| --- | --- | --- | --- |
| 1 | 任务管理 | [01-任务管理.md](01-任务管理.md) | `images/task/`（6 张） |
| 2 | 审批流 | [02-审批流.md](02-审批流.md) | `images/flow/`（7 张） |
| 3 | 采购订单 | [03-采购订单.md](03-采购订单.md) | `images/purchase/`（5 张） |
| 4 | 收货入库 | [04-收货入库.md](04-收货入库.md) | `images/stockin/`（3 张） |
| 5 | 出库单 | [05-出库单.md](05-出库单.md) | `images/stockout/`（3 张） |
| 6 | 库存查询 | [06-库存查询.md](06-库存查询.md) | `images/inventory/`（3 张） |
| 7 | 库存调拨 | [07-库存调拨.md](07-库存调拨.md) | `images/transfer/`（3 张） |
| 8 | 财务管理（概览） | [08-财务管理.md](08-财务管理.md) | `images/finance/`（5 张） |
| 9 | 报表 | [09-报表.md](09-报表.md) | `images/report/`（3 张） |

`08-财务管理` 是财务全景概览；凭证 / 应付 / 付款 / 总账 / 期间 / 年结 / 辅助核算 / 预算的逐功能详版见手册（二）。

## 二、使用手册（二）：财务 8 个 + 平台 5 个功能

| # | 功能 | 手册 | 截图 |
| --- | --- | --- | --- |
| 10 | 凭证 | [10-凭证.md](10-凭证.md) | `images/p2b/voucher/`（5 张） |
| 11 | 应付 | [11-应付.md](11-应付.md) | `images/p2b/payable/`（3 张） |
| 12 | 付款 | [12-付款.md](12-付款.md) | `images/p2b/payment/`（3 张）+ `images/p2b/seed-payment/`（2 张） |
| 13 | 总账 | [13-总账.md](13-总账.md) | `images/p2b/ledger/`（3 张） |
| 14 | 会计期间 | [14-会计期间.md](14-会计期间.md) | `images/p2b/period/`（4 张） |
| 15 | 年度结转 | [15-年度结转.md](15-年度结转.md) | `images/p2b/yearend/`（2 张） |
| 16 | 辅助核算 | [16-辅助核算.md](16-辅助核算.md) | `images/p2b/aux/`（2 张） |
| 17 | 预算（待上线） | [17-预算.md](17-预算.md) | `images/p2b/budget/`（2 张） |
| 18 | 系统管理 | [18-系统管理.md](18-系统管理.md) | `images/p2b/sys/`（6 张） |
| 19 | 权限 | [19-权限.md](19-权限.md) | `images/p2b/perm/`（7 张） |
| 20 | 通知 | [20-通知.md](20-通知.md) | `images/p2b/notice/`（5 张） |
| 21 | 附件与文件 | [21-附件.md](21-附件.md) | `images/p2b/attach/`（3 张）+ `images/p2b/seed-attach/`（2 张） |
| 22 | 巡检 | [22-巡检.md](22-巡检.md) | `images/p2b/inspect/`（3 张） |

合计 **90 张真机截图**（Playwright 截取，非手绘、非拼图），其中 48 张是手册（二）的常规采集，4 张是同一次真机写库取证（付款登记、附件上传）。

---

## 三、截图是怎么来的（可复现）

```bash
# 1) 手册专用实例（独立库，不动生产库）
#    后端：NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_manual2 NIK_SERVER_LISTEN=127.0.0.1:18097
#    前端：BACKEND_ORIGIN=http://127.0.0.1:18097 vite --port 5180

# 2) 手册（一）9 个核心业务
MANUAL_BASE=http://127.0.0.1:5180 node scripts/manual/capture.mjs \
  task flow purchase stockin stockout inventory transfer finance report
MANUAL_BASE=http://127.0.0.1:5180 node scripts/manual/capture.mjs stockout_create

# 3) 手册（二）13 个功能
MANUAL_BASE=http://127.0.0.1:5180 MANUAL_API=http://127.0.0.1:18097 \
MANUAL_PASSWORD='Manual@123456' node scripts/manual/capture2.mjs \
  voucher payable payment ledger period yearend aux budget sys perm notice attach inspect

# 4) （可选）真机写库取证：登记一笔部分付款 + 上传一份附件
MANUAL_WRITE=1 MANUAL_BASE=http://127.0.0.1:5180 ... node scripts/manual/capture2.mjs seed_payment seed_attach
```

脚本行为：

- 用 `POST /api/system/auth/login` 取 token 写入 localStorage 跳过手点登录；
- 自动关闭「你有 N 条待处理」登录提醒弹窗；
- 每个模块输出到 `docs/manual/images/<phase>/<nn>-<name>.png`（手册（二）在 `images/p2b/<phase>/`）；
- 单步失败只记 `!!` 并留一张 `_fail-*.png`，不中断整轮，便于一次跑完全部模块；
- 默认**只读**（不改库）；`MANUAL_WRITE=1` 才允许真机写库取证；
- 同时打印 `[note]` 行（表头 / 行数 / 按钮 / 页签 / 关键文案），手册正文的列名、按钮名、系统原话均以此为准。

## 四、截图环境实况（截图里能看到的数据）

| 项 | 值 |
| --- | --- |
| 实例库 | `cloudpivot_manual2`（迁移至最新，独立于生产库） |
| 演示账号 | `admin / Manual@123456`（仅手册实例） |
| 数据量（截图当时） | 任务 6 条、流程定义 20 条、流程实例 14 条、待我审批 8 条、采购单 13 张、收货单 13 张、出库单 1 张、调拨单 2 张、应付单 15 张、凭证 108 张、会计期间 77 个、科目 56 个、通知模板 10 条、即时库存当页 20 行 |
| 写库取证样例 | 付款 `PM-20261104-000001`（30.00，部分付款，凭证 `PZ-20261104-000001`）；附件 `发票样例-202611.pdf`（821 B，分类 `发票影像`） |
| 控制台异常 | 仅 `401 /api/business/notice/stream`（站内信 SSE 探针鉴权，与业务无关） |

## 五、连跑两轮的一致性（证据）

| 批次 | 日志 | 结果 |
| --- | --- | --- |
| 手册（一） | `docs/manual/evidence/capture-run1.log.txt` / `capture-run2.log.txt` | 两轮均 38 张 / 9 个模块，0 失败，`[note]`/`[shot]` 行 0 差异 |
| 手册（二） | `docs/manual/evidence/p2b-capture-run1.log.txt` / `p2b-capture-run2.log.txt` | 两轮均 48 张 / 13 个模块，0 失败，`[note]`/`[shot]` 行仅 `/health` 时间戳 1 行差异（其余全一致） |

自检脚本（提交前跑）：

```bash
python3 scripts/manual/check-structure.py   # 22 篇统一五段结构 + 每篇截图数/步骤数，不合格退出码 1
python3 scripts/manual/check-links.py       # 图片引用与文件一一对应，无缺失、无未引用
```

当前结果：**22 篇结构全合格**；图片引用 90 条 / 文件 90 张，**缺失 0、未引用 0、重复（md5 相同）0**。

## 六、修订记录

| 日期 | 修订 | 证据 |
| --- | --- | --- |
| 2026-11-04 | 手册（一）初版 9 篇 + 38 张真机截图 | `02a7c38` |
| 2026-11-04 | 修正 04 收货入库 / 07 库存调拨新建表单截图：原截图误截列表页（`dismissOverlays()` 把刚打开的弹窗一起关掉），改为等待 `.t-dialog:visible` 后再截；同步修正「弹窗/抽屉」文案 | `03f81df` + 两轮日志 |
| 2026-11-04 | 03 采购订单补全弹窗字段（合同、下单日期、期望到货、账期、备注）与按钮口径 | `v0.x 提交` |
| 2026-11-05 | 手册（二）新增 13 篇（财务 8 + 平台 5）+ `capture2.mjs` 采集脚本 + 52 张真机截图；`check-structure.py` 扩展覆盖 22 篇并给出退出码，`check-links.py` 改为递归扫 `images/**` | 本文件第五节两轮日志 |
| 2026-11-05 | 15-年度结转 步骤 1 菜单路径改为「财务管理 → 年度结转」；第八节缺口 #2 标注已修复 | 迁移 `V0236`；改前/改后侧边栏见 `docs/evidence/20261105-yearend-menu-reparent/` |
| 2026-11-05 | 左侧菜单路径按**实测菜单树**对齐（只改手册、不改菜单）：08/11/12/21 应付单 → 「采购管理 → 应付单」；06 即时库存、07 库存调拨 → 「仓库管理 → …」；05 出库单补全为「采购管理 → 采购订单 → 出库单」；08 步骤 1 增补一段菜单口径说明；08 步骤 3 按侧边栏实际条目名补全为「财务管理 → 总账 / 科目余额表」（与 13-总账 一致）。截图本就真机采集、侧边栏高亮分组即实测分组，**不重拍** | `docs/evidence/20261105-menu-path-fix/`（`01-menu-api-parent.txt` 5 条断言 + `03-sidebar-*.png` 逐页展开层级）；`scripts/evidence/menu_path_sidebar.mjs` |

## 七、口径说明

- **权限码**取自 `sys_menu.auth_code`（手册实例真实数据），非臆造；后端注解与种子菜单三方一致。
- **角色**取自 `sys_role`：`SUPER_ADMIN` 为内置超级管理员；`DEPT_LEADER` / `PURCHASE_MANAGER` / `GENERAL_MANAGER` / `WAREHOUSE_MANAGER` / `FINANCE_MANAGER` / `FINANCE_DIRECTOR` / `FLOW_CONFIG_APPROVER` 为审批岗角色。**具体业务操作权限需在「角色管理」中授予**，没有内置"采购员"这类开箱角色。
- **错误提示**只引用系统真实文案（后端源码字符串 + 真机返回），未逐条复现的只写原因与处理，不编"系统提示"。
- 截图里的数据是演示数据，单号、金额、供应商与真实客户无关。
- 手册（二）中 `17-预算` 按「当前未上线」口径撰写：菜单入口在、接口与权限码齐备、页面组件缺失，点进去是 404；正文如实记录，并在功能上线后需要重写第三节。

## 八、本轮发现的产品/交付缺口（写进手册、未在本卡修复）

| # | 现象 | 证据 | 归属 |
| --- | --- | --- | --- |
| 1 | 预算菜单可点但页面 404：`sys_menu.component=business/erp/budget/index`，前端只有 `budget-logic.ts`，无 `.vue` | `docs/manual/17-预算.md` 第三节；`images/p2b/budget/01-budget-404.png` | 前端页面未交付 |
| 2 | ~~年结菜单挂在「采购管理」组下（功能属财务）~~ **已修复** | `sys_menu` 实测：`/business/erp/year-end` 父节点原为 `采购管理`（种子 `V0190:86` 的过期 `parent_id`）→ 迁移 `V0236__erp_finance_menu_reparent` 移到 `财务管理`（同根因的 `期初余额` 一并归位）；改前/改后侧边栏截图见 `docs/evidence/20261105-yearend-menu-reparent/` | 已闭环（V0236） |
| 3 | 巡检提醒的「去看看」落点 `/business/erp/inventory?audit=1`，前端未消费 `audit` 参数，页面与普通即时库存一致 | `docs/manual/22-巡检.md` 步骤 2；`grep audit frontend/src/views/business/erp/inventory/` 无命中 | 前端缺口 |
| 4 | 文件管理「上传人」列显示账号 UUID 而非姓名 | `images/p2b/attach/01-attach-file-list.png`、`seed-attach/01` | 前端展示 |
| 5 | 付款提交后详情抽屉不自动刷新（需关闭重开） | `docs/manual/12-付款.md` 步骤 3；`images/p2b/seed-payment/02` | 交互体验 |
| 6 | 辅助核算维度已落库（科目 `aux_dimensions`、分录 `aux_*_id`）但界面无维度列与按维度筛选；辅助账接口 `ledger:aux-balance` 前端无入口 | `docs/manual/16-辅助核算.md`；`grep aux-balance frontend/src` 无命中 | 前端缺口 |

## 九、尚未覆盖（与本手册范围无关）

- 基础数据（物料、供应商、仓库、库位、单位、科目）维护，见《后端二开指南》《项目结构与设计亮点》。
- 库存盘点、序列号管理、合同管理、导入导出、工作流状态机边配置：后续批次。
- 支撑功能组（字典管理、参数配置、文件管理（`/support/file` 旧版）、操作日志、定时任务、在线用户）：后续批次（手册（二）的 18 只在「相邻模块」处提及）。
