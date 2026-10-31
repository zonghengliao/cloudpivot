# 前端 52 页全量 E2E 报告（测试E / t_4c9d4e71）

> 生成方式：`python3 e2e/tools/e2e_report.py`（读 `frontend/e2e-probe/probe/*.json` 现场文件自动汇总，每个 ✅/❌ 都能指回一份 JSON + 一张 PNG 截图）。
>
> 运行方式：隔离实例 `http://127.0.0.1:5189`（副本库 `cloudpivot_e2e_iso2` = 生产 2026-10-31 12:44 快照（pg_restore 自 `/root/datafix-backup-p10-*/full.dump`）+ 后端 `127.0.0.1:18089`），**没有打生产**。命令：`E2E_BASE_URL=http://127.0.0.1:5189 pnpm test:e2e`。
>
> 现场目录是 `frontend/e2e-probe/`（**不能**放 `e2e-report/`：那是 html 报告目录，`playwright test` 每次启动都会清空它 —— 单独跑一条 spec 就会把全量现场删光）。

## 一、总览

| 指标 | 数值 |
| --- | --- |
| 目录里可跑页面 | **58** 个（`_catalog.ts`，逐页 6 项全跑） |
| 覆盖页面（本轮真跑过） | **58** 个（含 403/404/500 三个结果页 + 任务四视图） |
| 检查点（页 × 6 项） | **348** |
| 通过 | **345** |
| 未通过 | **3**（其中 3 条是已固化的已知缺陷标注，套件里以 `test.fail` 通过） |
| 真红（非已知缺陷） | **0** |
| 死按钮（点了无任何可观察反应） | **0** |
| 页面加载期 4xx/5xx（非已知缺陷） | **0** |
| 已知缺陷留证（SSE 401） | **58** 页 |
| 渲染 404 结果页的页面 | **0** |

### 整套跑测结果（`pnpm test:e2e` 全量，含本卡之外的历史用例）

| 结果 | 条数 |
| --- | --- |
| 通过 | **398** |
| 失败 | **5** |
| 跳过（历史用例自带 `test.skip`） | **5** |
| 用时 | 8.5m |

失败清单（逐条，见第九节归因）：

- `e2e/08-t8b-task-tree.spec.ts:94:3 › T8-B 任务列表树形改造 › 箭头在标题列、父行 4px 竖条 + 浅底 + 加粗、进度列 1/3 小进度条`
- `e2e/08-t8b-task-tree.spec.ts:132:3 › T8-B 任务列表树形改造 › 逐节点折叠展开：展开出 3 条子任务（连接线 + 缩进 24px），收起后兄弟不受影响`
- `e2e/09-t8c-root-only.spec.ts:66:3 › T8-C 列表只返回顶层任务（root_only） › 列表第 1 页 = 5 个带真箭头的父任务；展开出 3 个子行；total 28`
- `e2e/09-t8c-root-only.spec.ts:169:3 › T8-C 列表只返回顶层任务（root_only） › 看板 / 甘特 / 燃尽图不传 root_only，数据量不变（全部 43 条）`
- `e2e/10-erp-warehouse.spec.ts:118:1 › W4a：仓库树 + 库位页（树渲染 / 按仓筛选 / 折叠 / 新建 / 叶子门禁） › ① 菜单进入「ERP 主数据 → 仓库管理」`

### 404 清单

**逐页 6 项覆盖的 58 个页面里 0 个 404。** 侧栏「采购明细 / 单号序列」两条曾经的 404 菜单，已由前端提交 `cd8b054` 补齐路由并纳入逐页检查（见 `30-menu-alignment.spec.ts`：菜单叶子 0 异常）。卡面旧路径的情况见下一节。

### 卡面旧路径清单（卡面写的路径 vs 真实路由）

| 卡面路径 | 真实路由 | 路由层有 alias | 实际结果 |
| --- | --- | --- | --- |
| `/business/project` | `/project` | 否 | 渲染 404 结果页（无 alias，友好 404 不白屏） |
| `/announcement/center` | `/announcement` | 否 | 渲染 404 结果页（无 alias，友好 404 不白屏） |
| `/announcement/manage` | `/business/announcement` | 否 | 渲染 404 结果页（无 alias，友好 404 不白屏） |
| `/business/erp/purchase` | `/business/erp/purchase-order` | 是 | 渲染真实页面（alias 命中，地址栏保持旧路径） |
| `/business/erp/stockin` | `/business/erp/stock-in` | 是 | 渲染真实页面（alias 命中，地址栏保持旧路径） |
| `/business/help/center` | `/help` | 否 | 渲染 404 结果页（无 alias，友好 404 不白屏） |
| `/business/help/manage` | `/help/manage` | 否 | 渲染 404 结果页（无 alias，友好 404 不白屏） |
| `/system/masking` | `/system/field-mask` | 否 | 渲染 404 结果页（无 alias，友好 404 不白屏） |
| `/system/mutex` | `/system/role-mutex` | 否 | 渲染 404 结果页（无 alias，友好 404 不白屏） |
| `/system/changelog` | `/system/change-log` | 否 | 渲染 404 结果页（无 alias，友好 404 不白屏） |

口径：`alias` 命中的 2 条会渲染真实页面（vue-router 的 alias 不改 URL）；其余 8 条落到友好 404（有「404 / 抱歉，您访问的页面不存在 / 返回首页」）。**菜单与通知里存的是真实路由**（`sys_menu.path` 已由 V0141 系列种子对齐），所以普通用户点菜单不会踩到这些旧路径——它们只在旧书签/旧文档里出现。

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
| 16 | 采购明细 | `/business/erp/purchase-order-item` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 17 | 单号序列 | `/business/erp/seq` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 18 | 序列号管理 | `/business/erp/serial` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 19 | 收货入库 | `/business/erp/stock-in` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 20 | 供应商 | `/business/erp/supplier` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 21 | 库存调拨 | `/business/erp/transfer` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 22 | 计量单位 | `/business/erp/unit` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 23 | 记账凭证 | `/business/erp/voucher` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ⚠️ |
| 24 | 仓库管理 | `/business/erp/warehouse` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 25 | 导出任务 | `/business/impexp/export-task` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ⚠️ |
| 26 | 导入向导 | `/business/impexp/import` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 27 | 导入任务 | `/business/impexp/import-task` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 28 | 消息中心 | `/business/notice/message` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 29 | 消息发送 | `/business/notice/send` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 30 | 消息模板 | `/business/notice/template` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ⚠️ |
| 31 | 存储后端 | `/business/storage/backend` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 32 | 文件管理（存储） | `/business/storage/file` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 33 | 工作台 | `/dashboard` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 34 | 流程定义 | `/flow/def` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 35 | 发起审批 | `/flow/start` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 36 | 审批中心（待办） | `/flow/task` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 37 | 帮助中心 | `/help` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 38 | 帮助文档管理 | `/help/manage` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 39 | 项目 | `/project` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 40 | 参数配置 | `/support/config` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 41 | 字典管理 | `/support/dict` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 42 | 文件管理（支撑） | `/support/file` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 43 | 定时任务 | `/support/job` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 44 | 操作日志 | `/support/log` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 45 | 在线用户 | `/support/online` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 46 | 变更日志 | `/system/change-log` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 47 | 数据权限 | `/system/data-scope` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 48 | 部门管理 | `/system/dept` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 49 | 字段脱敏 | `/system/field-mask` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 50 | 菜单管理 | `/system/menu` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 51 | 角色管理 | `/system/role` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 52 | 角色互斥 | `/system/role-mutex` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 53 | 用户管理 | `/system/user` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 54 | 任务看板 | `/task/board` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 55 | 甘特图 | `/task/gantt` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 56 | 任务列表 | `/task/list` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 57 | 任务模板 | `/task/templates` | 列表 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 58 | 燃尽图 | `/task?view=burndown` | 非表格 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

图例补充：⚠️ = 已固化的**已知产品缺陷**（用例用 `test.fail()` 标注，套件整体仍绿；修好后会变红提醒摘标注）。

## 三、★ 死按钮清单（核心产出）

**0 个死按钮**：本轮所有页面里，内容区每个可用按钮点下去都有可观察反应（弹窗/请求/Toast/路由/DOM 变化/下拉/展开/文件选择器）。

说明：`上传文件 / 选择文件` 类按钮点开的是**原生文件选择器**（DOM 不变、不发请求），已用 Playwright 的 `filechooser` 事件单独识别，不误判为死按钮。

## 四、4xx / 5xx 请求清单（页面加载期）

除下面的已知缺陷外，**页面加载期 0 个 4xx/5xx 接口**。

### 已知缺陷：通知 SSE 每次都 401（58 页全部命中）

- 现象：每次页面加载都会发 `GET /api/business/notice/stream?token=…`，后端回 **401**，而且 `EventSource` 还会自动重连、反复 401。
- 证据链：`frontend/src/stores/notice.ts#connect()` → `buildNoticeSseUrl()` 用 `?token=` 订阅；后端认证中间件（`alun-web 0.4.2 src/middleware/auth.rs`）**只读 `Authorization` 头**，仓库里也搜不到 `NIK_AUTH_QUERY_TOKEN_PATHS` 这类白名单实现 → 令牌永远不被采纳。
- 影响：实时推送从未工作过；未读角标靠 `notice store` 的 30s 轮询兜底，所以用户不容易发现。
- 用例固化：`34-error-boundary.spec.ts` 的「已知缺陷留证」一条会断言「SSE 请求确实发生 + 状态恒为 401 + 未读数兜底接口 200」；后端一旦修好，这条会变红提醒摘掉白名单。

## 五、未通过项与原因（按页）

| 页面 | 路径 | 未通过项 | 性质 | 现场 |
| --- | --- | --- | --- | --- |
| 记账凭证 | `/business/erp/voucher` | ⑥ | 已知缺陷（`test.fail` 标注） | form={"opened": true, "submitted": true, "validation": [], "writeRequests": [], "closed": true, "dialogButtons": ["加一行", "取消", "保存并记账"], "forceClick": true} |
| 导出任务 | `/business/impexp/export-task` | ⑥ | 已知缺陷（`test.fail` 标注） | form={"opened": true, "submitted": true, "validation": ["导出任务 EXP20261031-3491 已创建，完成后会收到站内信通知"], "writeRequests": ["POST /api/business/impexp/export"], "closed": true, "dialogButtons": ["取消", "开始导出"]} |
| 消息模板 | `/business/notice/template` | ⑥ | 已知缺陷（`test.fail` 标注） | form={"opened": true, "submitted": true, "validation": ["template_code: 模板编码长度需为 2-64 个字符; content_template: 正文模板不能为空; title_template: 标题模板长度需为 1-255 个字符; template_name: 模板名称长度需为 1-100 个字符"], "writeRequests": ["POST /api/busi |

## 六、专项测试（7 类）

| 专项 | 文件 | 覆盖内容 |
| --- | --- | --- |
| 1 菜单路径 ↔ 路由全量对齐 | `30-menu-alignment.spec.ts` | 拉真实菜单树 → 逐个 navigate → 断言无 404、无 4xx/5xx；已知缺口（`/business/erp/purchase-order-item`、`/business/erp/seq`）单列断言 |
| 2 按钮可达性全量 | `31-dead-buttons.spec.ts` | 汇总 58 页现场断死按钮为空；危险按钮在 **abort 写请求**前提下真点，验有确认框/提示 |
| 3 多角色视角 | `32-multi-role.spec.ts` | 超管/仓管员/财务/只读员/受限管理员：菜单收窄、菜单都能开、直达无权页被拒 |
| 4 徽标联动 | `33-badge-linkage.spec.ts` | 徽标口径 == `/api/system/badge/counts`；消息已读、任务流转、审批处理各 -1 |
| 5 错误与边界 | `34-error-boundary.spec.ts` | 接口 500 不白屏、token 失效回登录、无权账号 403、连点两次只开一个弹窗、SSE 401 留证 |
| 6 卡面旧路径 | `35-legacy-paths.spec.ts` | 卡面里写的 10 条旧路径逐条真开：有 alias 的渲染真实页，无 alias 的必须落到友好 404（不白屏） |
| 7 覆盖收口自检 | `99-coverage-audit.spec.ts` | 目录里每个可跑页面都必须有现场文件 + navStatus 200 + 无 error；检查点总数 ≥ 330（防漏页/防 skip） |

## 七、过程中发现的产品缺陷（只报告，未改 src）

### 7.1 已固化标注（`test.fail()`，修好后会变红提醒摘标注）

| 页面 | 路径 | 未通过项 | 原因 |
| --- | --- | --- | --- |
| 记账凭证 | `/business/erp/voucher` | ⑥ | 新增凭证弹窗空提交后未观察到任何必填校验文案（按钮「保存并记账」），需要人工复核是否漏了必填校验 |
| 导出任务 | `/business/impexp/export-task` | ⑥ | 「开始导出」弹窗空提交后仍直接 POST /api/business/impexp/export（前端必填校验缺位，靠后端 422 兜底）并真的建出任务 |
| 消息模板 | `/business/notice/template` | ⑥ | 新建模板弹窗空提交直接打 POST /api/business/notice/template（前端缺必填校验，靠后端 422 兜底） |

### 7.2 人工复核发现（逐条可回溯现场）

1. **登录页空提交不拦前端**：`views/login/index.vue` 的 `<t-form @submit="handleSubmit">` 里，`handleSubmit` 没接 `validateResult`，红字校验会显示但请求照样发（`POST /api/system/auth/login`）。用例：`20-pages-home-error-task-flow.spec.ts` 的「★已知缺陷」一条（`test.fail` 标注）。
2. **通知 SSE 永远 401**：见第四节，58 页全部命中；实时推送从未生效，靠 30s 轮询兜底。
3. **消息模板新建：空提交直接打写请求**（POST /api/business/notice/template），前端必填校验缺位，靠后端 422 兜底。现场见 `e2e-report/probe/_business_notice_template.json` 的 `interact.form.writeRequests`。
4. **登录限流计数在本地缓存（profile=dev/test）下永不过期**：被拒的 429 也 `incr` 计数，而 `Cache::set_ex` 只在 count==1 时设置 TTL；实测触发一次之后数分钟仍 429，必须**重启后端进程**才恢复（本轮完整跑测前重启过一次，日志见 `/tmp/e2e-iso/backend.log`）。生产用 redis 时 TTL 正常，但开发/联调会被锁号。
5. **库存调拨列表接口 500：本轮已不再复现**（上一轮（14:00 前后）在 HEAD 上稳定 500；本轮重编译后端 + 生产 12:44 快照后 `GET /api/business/erp/transfer/page` 恒 200，对应 `knownBug` 标注已摘掉）。

## 八、截图与现场文件

- 每个页面一张截图：`frontend/e2e-probe/probe/<path>.png`
- 每个页面一份现场 JSON（接口流水、按钮清单、点击反应、表单校验、console 错误）：`frontend/e2e-probe/probe/<path>.json`
- 卡面旧路径采集：`frontend/e2e-probe/legacy-paths.json`（`35-legacy-paths.spec.ts` 落盘）
- 失败用例的 trace / 截图：`frontend/test-results/`
- 复跑命令：`cd frontend && E2E_BASE_URL=http://127.0.0.1:5189 pnpm test:e2e --reporter=list | tee /tmp/e2e-list.log`；重算报告：`python3 e2e/tools/run_summary.py /tmp/e2e-list.log && npx tsx e2e/tools/dump-catalog.ts && python3 e2e/tools/e2e_report.py`

## 九、历史用例（01–11）在副本库上的状态（非本卡新增，仅登记不隐瞒）

这些 spec 是别的卡（E4 / T8-B / T8-C / P4-A / P4-B）写的验收用例，**夹具数字写成生产当时的值**。
本卡的隔离副本库是「生产 2026-10-31 12:44 快照」（`pg_restore` 自 `/root/datafix-backup-p10-20261031-124437/full.dump`，任务 79 条 / 顶层 31 条），所以其中几条会因为**数据量变了**而红——不是页面坏了。

| spec | 用例 | 状态 | 原因（可复核） |
| --- | --- | --- | --- |
| `08-t8b-task-tree` | 箭头在标题列 / 逐节点折叠展开 | ❌ | 用例要在树里找 8 号卡造的 `[T8B-…]根任务` 父行并对箭头断言，快照库里该夹具的 `t-table__tree-op-icon` 计数为 0（父行由 09 号用例自己创建，顺序/数据变了就不成立） |
| `09-t8c-root-only` | total 28 / 全部 43 条 | ❌ | 用例把 `EXPECT_TOTAL_ROOTS=28`、`EXPECT_TOTAL_ALL=43` 写死；快照库实际 **31 顶层 / 79 条**（生产数据已增长），断言必然不等 |
| `10-erp-warehouse` | ① 菜单进入「ERP 主数据 → 仓库管理」 | ❌ | 侧栏在 `cd8b054` 之后重构为「采购管理 / 财务管理 / 通用底座 / 基础数据」，`getByText('ERP 主数据')` 已不存在 → 点击超时（菜单改名，不是路由坏） |
| `03-board-drag` / `11-erp-location` / `11-erp-master-data` | 拖拽 / 库位 / 单位供应商 | ✅ | 本轮通过；`11-erp-master-data` 的网络体检原先不认「SSE 401」这条已知缺陷，已补齐白名单（否则 4 条会因已知缺陷恒红） |

口径：**不动**这些用例的期望值（把 28 改成 31 等于掩盖「用例与生产数据脱钩」这个真问题），在本卡范围内只登记现状 + 给出复核方法（`psql -c "select count(*) from biz_task where deleted_at is null"`）。

