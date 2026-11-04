# 云枢 CloudPivot 使用手册（一）：核心业务 9 个功能

面向客户上线培训的操作手册集。每篇结构统一：**这个功能干什么 → 谁能用（角色 + 权限）→ 完整操作步骤（真机截图）→ 常见错误怎么办 → 与哪些功能联动**。

| # | 功能 | 手册 | 截图 |
| --- | --- | --- | --- |
| 1 | 任务管理 | [01-任务管理.md](01-任务管理.md) | `images/task/`（6 张） |
| 2 | 审批流 | [02-审批流.md](02-审批流.md) | `images/flow/`（7 张） |
| 3 | 采购订单 | [03-采购订单.md](03-采购订单.md) | `images/purchase/`（5 张） |
| 4 | 收货入库 | [04-收货入库.md](04-收货入库.md) | `images/stockin/`（3 张） |
| 5 | 出库单 | [05-出库单.md](05-出库单.md) | `images/stockout/`（3 张） |
| 6 | 库存查询 | [06-库存查询.md](06-库存查询.md) | `images/inventory/`（3 张） |
| 7 | 库存调拨 | [07-库存调拨.md](07-库存调拨.md) | `images/transfer/`（3 张） |
| 8 | 财务管理 | [08-财务管理.md](08-财务管理.md) | `images/finance/`（5 张） |
| 9 | 报表 | [09-报表.md](09-报表.md) | `images/report/`（3 张） |

合计 **38 张真机截图**（Playwright 截取，非手绘、非拼图）。

---

## 一、截图是怎么来的（可复现）

```bash
# 1) 手册专用实例（独立库，不动生产库）
#    后端：NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_manual2 NIK_SERVER_LISTEN=127.0.0.1:18097
#    前端：BACKEND_ORIGIN=http://127.0.0.1:18097 vite --port 5180

# 2) 抓图（真机点击 + 截图 + 打印表头/行数/按钮作为正文取证）
MANUAL_BASE=http://127.0.0.1:5180 node scripts/manual/capture.mjs \
  task flow purchase stockin stockout inventory transfer finance report

# 3) 出库单需要一条演示单据（空库时自动真机录入）
MANUAL_BASE=http://127.0.0.1:5180 node scripts/manual/capture.mjs stockout_create
```

脚本行为：

- 用 `POST /api/system/auth/login` 取 token 写入 localStorage 跳过手点登录；
- 自动关闭「你有 N 条待处理」登录提醒弹窗；
- 每个模块输出到 `docs/manual/images/<phase>/<nn>-<name>.png`；
- 单步失败只记 `!!`，不中断整轮，便于一次跑完 9 个模块；
- 默认**只读**（不改库）；`MANUAL_WRITE=1` 才允许真机建单取证（任务创建见 `docs/manual/evidence/task-create-verify.log`，返回 `任务已创建：TSK-202611-000007`）；
- 同时打印 `SUMMARY` 行（表头 / 行数 / 按钮 / 页签），手册正文的列名、按钮名均以此为准。

## 二、截图环境实况（截图里能看到的数据）

| 项 | 值 |
| --- | --- |
| 实例库 | `cloudpivot_manual2`（迁移至最新，独立于生产库） |
| 演示账号 | `admin / Manual@123456`（仅手册实例） |
| 数据量（截图当时） | 任务 6 条、流程定义 20 条、流程实例 14 条、待我审批 8 条、采购单 13 张、收货单 13 张、出库单 1 张、调拨单 2 张、应付单 15 张、凭证 107 张、会计期间 20 个、库存 22 行 |
| 出库单样例 | `OUT-20261104-000001`（领用 10，脚本真机录入） |
| 控制台异常 | 仅 `401 /api/business/notice/stream`（站内信 SSE 探针鉴权，与业务无关） |

## 三、连跑两轮的一致性（证据）

`docs/manual/evidence/capture-run1.log` / `capture-run2.log` 是同一脚本连续两轮的原始输出：

- 两轮均 **38 张 / 9 个模块目录（task flow purchase stockin stockout inventory transfer finance report）**，模块级 SUMMARY（表头、按钮、页签）逐行一致；
- 两轮 **0 失败（0 个 `!!`）**，逐行 SUMMARY 完全相同；
- 逐图差异仅时间戳/滚动位置，结构一致。

## 四、口径说明

- **权限码**取自 `sys_menu.auth_code`（手册实例真实数据），非臆造。
- **角色**取自 `sys_role`：`SUPER_ADMIN` 为内置超级管理员；`DEPT_LEADER` / `PURCHASE_MANAGER` / `GENERAL_MANAGER` / `WAREHOUSE_MANAGER` / `FINANCE_MANAGER` / `FINANCE_DIRECTOR` / `FLOW_CONFIG_APPROVER` 为审批岗角色，职责描述来自角色记录。**具体业务操作权限需在「角色管理」中授予**，没有内置"采购员"这类开箱角色。
- **错误提示**只引用系统真实文案（从后端源码与真机返回中取），未逐条复现的错误在同一列里不写"系统提示"，只写原因与处理。
- 截图里的数据是演示数据，单号、金额、供应商与真实客户无关。

## 五、尚未覆盖（与本手册范围无关）

- 基础数据（物料、供应商、仓库、库位、单位、科目）维护，见《后端二开指南》《项目结构与设计亮点》。
- 库存盘点、序列号管理、合同管理、导入导出、工作流状态机边配置：属于下一批手册（P2-2B）。
- 系统管理（用户 / 角色 / 菜单 / 字典 / 配置 / 日志）与支撑功能：属于系统管理手册。
