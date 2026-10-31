# 前端 52 页全量 E2E 报告（测试E / t_4c9d4e71）

> 生成方式：`python3 e2e/tools/e2e_report.py`（读 `frontend/e2e-report/probe/*.json` 现场文件自动汇总，每个 ✅/❌ 都能指回一份 JSON + 一张 PNG 截图）。
>
> 运行方式：隔离实例 `http://127.0.0.1:5189`（副本库 `cloudpivot_e2e_iso` + 后端 `127.0.0.1:18089`），**没有打生产**。命令：`E2E_BASE_URL=http://127.0.0.1:5189 pnpm test:e2e`。

## 一、总览

| 指标 | 数值 |
| --- | --- |
| 覆盖页面 | **55** 个（含 403/404/500 三个结果页） |
| 检查点（页 × 6 项） | **330** |
| 通过 | **324** |
| 未通过 | **6** |
| 死按钮（点了无任何可观察反应） | **0** |
| 页面加载期 4xx/5xx（非已知缺陷） | **1** |
| 已知缺陷留证（SSE 401） | **55** 页 |
| 渲染 404 结果页的页面 | **0** |

### 404 清单

**本目录（56 条真实路由）里 0 个 404。** 侧栏里「采购明细 / 单号序列」两条菜单没有对应路由的问题由 `30-menu-alignment.spec.ts` 专项单列（见第四节）。

## 二、逐页 6 项检查结果

图例：✅ 通过 ／ ❌ 未通过（下方「问题清单」有原因）。

| # | 页面 | 路径 | 类型 | ① | ② | ③ | ④ | ⑤ | ⑥ |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 403 无权限页 | `/result/403` | 结果页 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 2 | 404 页面不存在 | `/result/404` | 结果页 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 3 | 500 服务异常页 | `/result/500` | 结果页 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 4 | 公告中心 | `/announcement` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 5 | 公告管理 | `/business/announcement` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 6 | 会计科目 | `/business/erp/account` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 7 | 即时库存 | `/business/erp/inventory` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 8 | 库位库存 | `/business/erp/inventory-location` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 9 | 库存流水 | `/business/erp/inventory-txn` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 10 | 总账 | `/business/erp/ledger` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 11 | 库位管理 | `/business/erp/location` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 12 | 物料管理 | `/business/erp/material` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 13 | 应付单 | `/business/erp/payable` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 14 | 会计期间 | `/business/erp/period` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 15 | 采购订单 | `/business/erp/purchase-order` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 16 | 序列号管理 | `/business/erp/serial` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 17 | 收货入库 | `/business/erp/stock-in` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 18 | 供应商 | `/business/erp/supplier` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 19 | 库存调拨 | `/business/erp/transfer` | 列表 | ❌ | ✅ | ❌ | ✅ | ✅ | ✅ |
| 20 | 计量单位 | `/business/erp/unit` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 21 | 记账凭证 | `/business/erp/voucher` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ |
| 22 | 仓库管理 | `/business/erp/warehouse` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 23 | 导出任务 | `/business/impexp/export-task` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ |
| 24 | 导入向导 | `/business/impexp/import` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 25 | 导入任务 | `/business/impexp/import-task` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 26 | 消息中心 | `/business/notice/message` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 27 | 消息发送 | `/business/notice/send` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 28 | 消息模板 | `/business/notice/template` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ |
| 29 | 存储后端 | `/business/storage/backend` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 30 | 文件管理（存储） | `/business/storage/file` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 31 | 工作台 | `/dashboard` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 32 | 流程定义 | `/flow/def` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 33 | 发起审批 | `/flow/start` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 34 | 审批中心（待办） | `/flow/task` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 35 | 帮助中心 | `/help` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 36 | 帮助文档管理 | `/help/manage` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 37 | 项目 | `/project` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 38 | 参数配置 | `/support/config` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 39 | 字典管理 | `/support/dict` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 40 | 文件管理（支撑） | `/support/file` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 41 | 定时任务 | `/support/job` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 42 | 操作日志 | `/support/log` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 43 | 在线用户 | `/support/online` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 44 | 变更日志 | `/system/change-log` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 45 | 数据权限 | `/system/data-scope` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 46 | 部门管理 | `/system/dept` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 47 | 字段脱敏 | `/system/field-mask` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 48 | 菜单管理 | `/system/menu` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 49 | 角色管理 | `/system/role` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 50 | 角色互斥 | `/system/role-mutex` | 列表 | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ |
| 51 | 用户管理 | `/system/user` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 52 | 任务看板 | `/task/board` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 53 | 甘特图 | `/task/gantt` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 54 | 任务列表 | `/task/list` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 55 | 任务模板 | `/task/templates` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

## 三、★ 死按钮清单（核心产出）

**0 个死按钮**：本轮所有页面里，内容区每个可用按钮点下去都有可观察反应（弹窗/请求/Toast/路由/DOM 变化/下拉/展开/文件选择器）。

说明：`上传文件 / 选择文件` 类按钮点开的是**原生文件选择器**（DOM 不变、不发请求），已用 Playwright 的 `filechooser` 事件单独识别，不误判为死按钮。

## 四、4xx / 5xx 请求清单（页面加载期）

| 页面 | 路径 | 接口 |
| --- | --- | --- |
| 库存调拨 | `/business/erp/transfer` | `500 GET /api/business/erp/transfer/page` |

### 已知缺陷：通知 SSE 每次都 401（55 页全部命中）

- 现象：每次页面加载都会发 `GET /api/business/notice/stream?token=…`，后端回 **401**，而且 `EventSource` 还会自动重连、反复 401。
- 证据链：`frontend/src/stores/notice.ts#connect()` → `buildNoticeSseUrl()` 用 `?token=` 订阅；后端认证中间件（`alun-web 0.4.2 src/middleware/auth.rs`）**只读 `Authorization` 头**，仓库里也搜不到 `NIK_AUTH_QUERY_TOKEN_PATHS` 这类白名单实现 → 令牌永远不被采纳。
- 影响：实时推送从未工作过；未读角标靠 `notice store` 的 30s 轮询兜底，所以用户不容易发现。
- 用例固化：`34-error-boundary.spec.ts` 的「已知缺陷留证」一条会断言「SSE 请求确实发生 + 状态恒为 401 + 未读数兜底接口 200」；后端一旦修好，这条会变红提醒摘掉白名单。

## 五、未通过项与原因（按页）

| 页面 | 路径 | 未通过项 | 现场 |
| --- | --- | --- | --- |
| 库存调拨 | `/business/erp/transfer` | ① ③ | api=['/api/business/erp/material/page', '/api/business/erp/warehouse/page', '/api/business/erp/transfer/page', '/api/business/erp/transfer/page'] |
| 记账凭证 | `/business/erp/voucher` | ⑥ | form={"opened": true, "submitted": true, "validation": [], "writeRequests": [], "closed": true, "dialogButtons": ["加一行", "取消", "保存并记账"], "forceClick": true} |
| 导出任务 | `/business/impexp/export-task` | ⑥ | form={"opened": true, "submitted": true, "validation": ["导出任务 EXP20261031-71FA 已创建，完成后会收到站内信通知"], "writeRequests": ["POST /api/business/impexp/export"], "closed": true, "dialogButtons": ["取消", "开始导出"]} |
| 消息模板 | `/business/notice/template` | ⑥ | form={"opened": true, "submitted": true, "validation": ["template_code: 模板编码长度需为 2-64 个字符; template_name: 模板名称长度需为 1-100 个字符; content_template: 正文模板不能为空; title_template: 标题模板长度需为 1-255 个字符"], "writeRequests": ["POST /api/busi |
| 角色互斥 | `/system/role-mutex` | ④ |  |

## 六、专项测试（比逐页更重要的 5 类）

| 专项 | 文件 | 覆盖内容 |
| --- | --- | --- |
| 1 菜单路径 ↔ 路由全量对齐 | `30-menu-alignment.spec.ts` | 拉真实菜单树 → 逐个 navigate → 断言无 404、无 4xx/5xx；已知缺口（`/business/erp/purchase-order-item`、`/business/erp/seq`）单列断言 |
| 2 按钮可达性全量 | `31-dead-buttons.spec.ts` | 汇总 55 页现场断死按钮为空；危险按钮在 **abort 写请求**前提下真点，验有确认框/提示 |
| 3 多角色视角 | `32-multi-role.spec.ts` | 超管/仓管员/财务/只读员/受限管理员：菜单收窄、菜单都能开、直达无权页被拒 |
| 4 徽标联动 | `33-badge-linkage.spec.ts` | 徽标口径 == `/api/system/badge/counts`；消息已读、任务流转、审批处理各 -1 |
| 5 错误与边界 | `34-error-boundary.spec.ts` | 接口 500 不白屏、token 失效回登录、无权账号 403、连点两次只开一个弹窗、SSE 401 留证 |

## 七、过程中发现的产品缺陷（只报告，未改 src）

1. **登录页空提交不拦前端**：`views/login/index.vue` 的 `<t-form @submit="handleSubmit">` 里，`handleSubmit` 没接 `validateResult`，红字校验会显示但请求照样发（`POST /api/system/auth/login`）。用例：`20-pages-home-error-task-flow.spec.ts` 的「★已知缺陷」一条（`test.fail` 标注）。
2. **通知 SSE 永远 401**（见第四节）。
3. **消息模板新建：空提交直接打 POST**（`/api/business/notice/template`），前端必填校验缺位，靠后端 422 兜底。现场见 `e2e-report/probe/_business_notice_template.json` 的`interact.form.writeRequests`。
4. **登录限流计数在本地缓存（profile=dev/test）下永不过期**：每次被拒的 429 也 `incr` 计数，而 `Cache::set_ex` 只在 count==1 时设置 TTL；实测触发一次之后 5 分钟仍 429，必须重启后端进程才恢复。生产用 redis 时 TTL 正常，但开发/联调会被永久锁号。
5. **库存调拨列表接口 500**：`GET /api/business/erp/transfer/page` 在 HEAD 代码 + 副本库上稳定 500（页面显示「调拨单列表加载失败：服务器内部错误」）。后端日志见 `/tmp/e2e-iso/backend.log`；P10/P11 正在改 transfer 相关代码，这条需要他们确认。

## 八、截图与现场文件

- 每个页面一张截图：`frontend/e2e-report/probe/<path>.png`
- 每个页面一份现场 JSON（接口流水、按钮清单、点击反应、表单校验、console 错误）：`frontend/e2e-report/probe/<path>.json`
- 失败用例的 trace / 截图：`frontend/test-results/`

