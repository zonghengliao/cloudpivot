# HR-18 员工档案页 + 详情抽屉 —— 验收证据

卡：`HR-18【HR】员工列表页 + 详情抽屉（含合同时间线/薪资标准/附件面板）`
设计：`docs/architecture/ARCH-9-人力资源模块设计.md` §5.2 #1/#2/#7 · §6.1 #1 · §6.2.1 / §6.2.2
日期：2026-11-12（CST）

## 一、交付了什么

| 文件 | 作用 |
|---|---|
| `frontend/src/api/business/hr/employee-kinds.ts`（新） | 出参契约（列表行 21 键 / 详情 49 键 / 合同 / 薪资标准 / `sensitive_masked`）+ 6 张「码值 → 中文」表 + 纯函数（状态配色、合同到期文案、筛选序列化、薪资整数分合计、明文解析） |
| `frontend/src/api/business/hr/employee.ts`（新） | 三个请求函数：`fetchEmployeePage` / `fetchEmployeeDetail` / `fetchEmployeeSensitive`；`export *` 词表，页面只认一个入口 |
| `frontend/src/views/business/hr/employee/index.vue`（新） | 列表（分页 + 5 类筛选 + 列表记忆）+ 详情抽屉四段（基本信息 / 劳动合同时间线 + 扫描件 / 薪资标准 / 附件） |
| `frontend/src/router/index.ts` | 注册 `/business/hr/employee`（`meta.permission = business:hr:employee:list`，与 `V0387` 的 `sys_menu` 种子逐字一致） |
| `frontend/tests/unit/hr-employee-page.test.ts`（新） | 21 条：词表与 Rust 源码对拍（`STATUS_LABELS` 等 6 张表）、到期文案 / 阈值、筛选序列化、薪资合计、明文三形态、源码接线断言 |

**没有**新建迁移、没有改后端（HR-12 的 `employee_handler.rs` 由并发卡落地，本卡只对齐它的契约）。

## 二、验收三条，逐条对应证据

### ① 列表分页 / 筛选 —— 真库真接口

`07_api.out`（后端 `127.0.0.1:18023`、库 `cloudpivot_hr18demo`、22 名员工）：

| 用例 | 结果 |
|---|---|
| `page=1&page_size=20` | `total=22, rows=20` |
| `page=2` | `rows=2`（李静 / 张伟） |
| `keyword=李` / `EMP-0002` / `13800138001` | 各命中 1 条（工号 / 姓名 / 手机三列都走通） |
| `status=ACTIVE` / `status=PROBATION,ACTIVE` | 3 条 / 4 条（**逗号分隔**，与 `EmployeePageQuery.status: Option<String>` 的落地形态一致） |
| `dept_id=生产部` | 2 条 |
| `employment_type=PROBATION` | 1 条 |
| `include_resigned=true` | 22 条（默认不含离职） |

浏览器实拍：`01-list.png`（22 人第 1 页、状态标签、合同到期橙字「还有 23 天到期」）、`06-list-page2.png`（第 2 页 2 条 + 列表记忆提示条）。

### ② 详情抽屉三块内容完整 —— 真库真详情

`07_api.out` ⑥：`GET /employee/{id}` 返回 **49 个键**、`contracts` 2 份（含 `renewed_from_no` 续签链）、`salary_items` 4 条（2 现行 / 2 历史）、`sensitive_masked.{id_card_masked,bank_account_masked}`。

浏览器实拍（张伟 `EMP-0001`）：

| 截图 | 内容 |
|---|---|
| `02-detail-basic.png` | 身份与联系 / 敏感信息（`110101****1234`、`6222 **** 7890` + 「查看明文」）/ 任职 / 教育与紧急联系人 / 工资卡与备注 |
| `03-detail-contract.png` | 现行合同条（`HT-2025-001`，还有 108 天到期）+ 时间线两条（`HT-2025-001` 生效中「续签自 HT-2022-001」、`HT-2022-001` 已续签）+ 该合同的**扫描件附件面板**（`doc_type=EMPLOYEE_CONTRACT`） |
| `04-detail-salary.png` | 现行标准（基本工资 8,000.00 + 岗位津贴 500.50）+ **现行合计 8,500.50**（整数分累加）+ 调薪历史 2 条 |
| `05-detail-attach.png` | 员工附件面板（`doc_type=EMPLOYEE`，提示「身份证复印件 / 学历证明 / 入职登记表」） |

### ③ typecheck + build

`08_gates.out`：`pnpm typecheck → EXIT=0`、`pnpm build → EXIT=0`（`✓ built in 1m 31s`）。
`09_unit_test_new.out`：新增 21 条单测 `21 passed / 0 failed`。
`10_unit_test_all.out`：全量 `pnpm test:unit → 1533 passed / 0 failed`（基线 1512 + 本卡 21）。

## 三、口径上的三处「别想当然」（都写进代码注释并有单测）

1. **敏感字段只出打码值**：JSON 形状是 `sensitive_masked.{id_card_masked, bank_account_masked}`（§5.2 字段表，HR-11 用 census 单测钉死），不是 §5.4 示例里的顶层平铺。
2. **合同到期天数是服务端算的**：页面直接用 `contract_expire_in_days`（后端按 `+08:00` 业务日算），单测断言「页面里不许出现 `new Date()`」——用本机时钟会跨时区 / 改系统时间就飘。
3. **金额是字符串**：`"8000.0000"`，合计走整数分（`sumSalary`），8,000.00 + 500.50 = 8,500.50 不出 `8500.499999`。

## 四、依赖 / 上报（不自己动别的模块）

1. **「查看明文」按钮走 `GET /employee/{id}/sensitive`（HR-13，卡仍在 todo）**：现在点击是 404。
   页面按 `error.status === 404` 给**人话提示**「查看明文功能暂不可用，打码值照常显示；如需核对请联系系统管理员」，
   打码值始终在、页面照常可用；HR-13 落地后这行代码自动变成正常路径（`plainValueOf` 已兜住三种候选响应形态）。
2. **合同 / 薪资标准两块现在是只读**：`POST /employee/{id}/contract`（HR-15）、`POST /employee/{id}/salary`（HR-16）
   都还没落地，所以本卡不做写入口；详情出参里的 `contracts[]` / `salary_items[]` 已经够渲染两块内容。
   本卡验证用的 5 份合同 / 5 条薪资标准是**SQL 夹具**（`07_api.out` ⑥ 的真实出参就是读它们）。
3. **员工新建 / 编辑表单在 §8.1 的卡清单里没有对应卡**（HR-12 只交了 6 个写接口）。本卡按纪律不越界做表单，
   建议补一张「员工表单抽屉（新建/编辑/绑定账号/状态流转）」的卡，否则客户只能靠 `seed-from-users` 或接口建档。
4. **列表行操作只留「详情」**：设计 §6.2.1 还有「编辑 / 调岗 / 调薪 / 离职」，它们分别依赖上一条的表单、
   HR-16 调薪、HR-26 离职流程 —— 现在挂上去只能是灰按钮，故不做。

## 五、环境与残留

- 后端：`127.0.0.1:18023`（`NIK_PROFILE=test`），库 **`cloudpivot_hr18demo`**（新建，22 名员工 + 2 部门 + 5 合同 + 5 薪资）；Vite `127.0.0.1:5517`（`BACKEND_ORIGIN` 指向 18023）。
- 两个进程与演练库**为复核保留**（`docs/evidence/.../07_api.out` 可原样重跑）；如需清理：
  `kill` 两个 pid、`psql -c 'drop database cloudpivot_hr18demo'`。
- 未改任何他人文件；工作区里他人的未提交改动一律按路径 `git add`，没有被带进本卡提交。
