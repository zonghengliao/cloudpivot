# t_fc256a30 · 薪资管理列表页（HR 第 5 页）落地证据

卡：`t_fc256a30`【HR·前端】薪资管理列表页缺失 → 菜单 `/business/hr/salary` 点进去 404（闸 n08 红）
上游：`t_fb2e95f4`（HR-48 全量回归）报的 `system::menu::tests::n08` 红项。

## 做了什么

| 文件 | 变更 |
|---|---|
| `frontend/src/views/business/hr/salary/index.vue`（新，1233 行） | 列表页：期间 / 状态 / 单号筛选 + 分页 + 进详情 + 试算 / 建单抽屉 + 行内导出 + 「我的薪资」tab |
| `frontend/src/api/business/hr/salary-list-kinds.ts`（新，680 行） | 本页纯逻辑与契约：筛选查询串、期间码校验、试算/建单报文（`dry_run` 由 mode 决定）、当前页合计（整数分）、我的薪资分组、空态文案、权限码表 |
| `frontend/src/api/business/hr/salary.ts`（改，+67/−4） | 补 #36 `salary/page`、#39 `salary/calc`、#40 `salary`、#46 `salary/my` 四个请求函数与文件头契约表 |
| `frontend/src/router/index.ts`（改，+23） | 注册 `business/hr/salary` → `@/views/business/hr/salary/index.vue`，`meta.permission = business:hr:salary:list`，排在 `salary/:id` 之前（静态段优先） |
| `frontend/src/views/business/erp/shared/source-route.ts`（改，−2/+2 ×2） | 两张工资凭证（`PAYROLL_ACCRUAL` / `PAYROLL_PAYMENT`）的 `deepLink` 从 `page-missing` 改 `filter`：目标页已存在，页面真读 `?sheet_no=` 当单号筛选（`page-missing` 会渲染成「不给链接」） |
| `frontend/tests/unit/vtrace-source-route.test.ts`（改） | 同上：`page-missing` 名单清空 + 补 `sheet_no` 断言 |
| `frontend/tests/unit/hr-salary-list.test.ts`（新，532 行，23 条） | 词表 / 权限码 / 期间码 / 报文形状 / 口径 / 接线（含 n08 判据：路由 component 与 `sys_menu.component` 同指一视图） |

不碰后端、不碰其他 HR 页面。

## 实测（本机，2026-11-13）

```
$ pnpm typecheck                       EXIT=0     # 02_typecheck.out
$ vite build（pnpm build 的第二步）     EXIT=0     # 03_build.out，产物含本页 chunk
$ npx tsx --test tests/unit/hr-salary-list.test.ts
  # tests 23 / # pass 23 / # fail 0                # 04_unit_tests.out
```

后端既有闸 n08（真库，145 `hr_n08salary_drill_114501`，库内 502 条 `sys_menu`、130 条 `menu_type=2`）：

```
# 修前（把本卡的 path 临时改成 …-ABSENT-PROBE 反证，不是猜）
$ SYS_DB_URL=… ./target/debug/deps/cloudpivot-… system::menu::tests::n08 --nocapture
  测试C 已连接验证库（cloudpivot_test）
  thread '…n08…' panicked: 以下菜单前端未注册路由，点进去必然 404：
  薪资管理 (/business/hr/salary, component=Some("business/hr/salary/index"))
  test result: FAILED. 0 passed; 1 failed           # 01b_n08_red_before_route.out，EXIT=101

# 修后（恢复 path）
$ SYS_DB_URL=… ./target/debug/deps/cloudpivot-… system::menu::tests::n08 --nocapture
  [N08] 130 条菜单路径全部能在前端路由里解析 ✓
  test result: ok. 1 passed; 0 failed               # 01c_n08_green_restored.out，EXIT=0
```

`pnpm test:unit` 全量：1744 passed / 1 failed —— 唯一红是 `flow-biz-link.test.ts`
（后端 `FlowBizType` 24 个字面量 vs 前端 20），属卡 `t_c49010bf` 的闸④ 存量红，
与本页无关（本页未触碰 `doc-kinds.ts` / `FlowBizType`）。

## 一处待办耦合（已在路由注释里写明）

`V0401__hr_page_code_split`（卡 `t_0af7ec46`，在途）把 5 个 HR 页面行的 `auth_code`
从 `*:list` 改成 `*:page`。本卡立项时该迁移未落库，故 `meta.permission` 按卡面写
`business:hr:salary:list`。**该迁移落地时必须同步改成 `business:hr:salary:page`**
（连同单测里那条断言）。否则「只有页面显形、没有全量列表权限」的员工会被路由挡住，
连自己的「我的薪资」tab 都进不去 —— 那正是 `V0401` 要修的越权面的反面。

## 环境

- 演练库：145 `hr_n08salary_drill_114501`（上一轮崩溃的 attempt 建的，已迁移到 V0401；跑完即 DROP）。
- 无新增迁移、无后端改动、无实例常驻。
