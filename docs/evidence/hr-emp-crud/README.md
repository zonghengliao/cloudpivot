# t_31b19b68 · HR 员工档案「新建 / 编辑 / 删除」（纯前端补齐写入口）

> 用户反馈（2026-11-12 19:20）：「员工档案页面找不到新建档案的按钮。也没有导入档案的按钮。不知道怎么录入员工档案。」
> 后端接口（`POST/PUT/DELETE /api/business/hr/employee`，权限码 `business:hr:employee:create|update|delete`）**早就有**，
> 前端只做了「看」（列表 + 详情抽屉，工具栏零按钮）—— 本卡补齐写入口。

## 一、改了什么

| 层 | 文件 | 说明 |
|---|---|---|
| API 契约 | `frontend/src/api/business/hr/employee-kinds.ts` | 新增 `EmployeeCreateReq` / `EmployeeUpdateReq` / `EmployeeCreatedRes` / `EmployeeIdRes`（逐字段对齐 `employee_handler.rs` 的 `*Req`）；`GENDER_LABELS` / `GENDER_OPTIONS` / `ID_CARD_TYPE_OPTIONS` / `EMPLOYEE_FIELD_MAX`（18 个长度上限，镜像后端 `MAX_*`）/ `EMPLOYEE_MIN_NAME` / `EMPLOYEE_DICT_CODES`（`hr_position` / `hr_job_level` / `hr_education`） |
| 请求层 | `frontend/src/api/business/hr/employee.ts` | `createEmployee` / `updateEmployee` / `deleteEmployee`（路径写全 `/api/...`）；头注接口表补 #3/#4/#5 |
| 纯逻辑 | `frontend/src/views/business/hr/employee/employee-form.ts` | 表单状态 / 空表单 / 详情预填（`formFromDetail`）/ 校验（`validateEmployeeForm`，含身份证 18 位 + GB 11643 校验位、业务日 `+08:00`）/ 请求体（`buildCreatePayload`、**diff 提交** `buildUpdatePayload`）/ 删除确认（`isDeleteConfirmed`）/ 只读判定（`isReadonlyEmployee`） |
| 页面 | `frontend/src/views/business/hr/employee/index.vue` | ① 工具栏「新建员工」（`v-if="canCreate"`）；② 新建/编辑共用表单对话框（5 段分组 + 备注，双列栅格，窄屏塌单列）；③ 详情抽屉底部 sticky 操作条「编辑 / 删除 / 关闭」；④ 删除二次确认（输姓名，输错禁用确认按钮）；⑤ 权限：三个按钮各挂 `create`/`update`/`delete`，**无权限不渲染**并给一句说明；⑥ 已离职档案只读（后端对 `RESIGNED` 409） |
| 单测 | `frontend/tests/unit/hr-employee-form.test.ts` | 23 条：长度上限与后端 `MAX_*` 逐条对拍、证件类型/性别码表对拍、业务日 +08:00、身份证校验位、必填与跨字段校验、**请求体形状（含「身份证留空绝不传空串」）**、删除确认、接线断言 |

## 二、三条容易踩的口径（已在代码里钉住）

1. **编辑时身份证 / 工资卡号拿不到明文**（详情只有 `sensitive_masked.*_masked`），而 `PUT` 的语义是
   「不传 = 保持原值、传空串 = 清空」。所以这两个字段**只在用户真的重填时**才进请求体 ——
   否则一次「只改手机号」会把身份证和工资卡号清成 NULL。
2. **编辑按 diff 提交**：只带真的改过的字段（清空 = 显式传空串）。除了防上面那件事，
   也避免把打码值回写进库。
3. **业务日按 `+08:00`**：`businessToday()` 不用 `new Date().toISOString()`（那是 UTC，
   北京时间 08:00 前会返回前一天，「入职日期 ≤ 今天」就会误拦当天入职）。

## 三、验收逐条

| # | 卡面判据 | 结果 | 证据 |
|---|---|---|---|
| ① | `pnpm typecheck` 0 error | ✅ exit 0（44s） | `00-verification-log.txt` |
| ② | `pnpm build` 成功 | ✅ ✓ built in 53.56s，exit 0 | 同上 |
| ③ | 单测（含与后端逐条对拍） | ✅ 44 tests / 44 pass / 0 fail | 同上 |
| ④ | admin 登录 → 员工档案页看到「新建员工」 | ✅ 按钮可见 | `01-list-create-button.png` |
| ⑤ | 点开弹窗，填必填（姓名/性别/部门/用工类型/入职日期）→ 保存成功 | ✅ `POST` 200，`code=0` | `02` `04` `05` |
| ⑥ | 列表刷新后看到新员工，工号 `EMP-XXXX` | ✅ `EMP-0019`（后端取号），接口回读一致 | `05-created-list.png` |
| ⑦ | 详情 → 编辑 → 改手机号 → 保存 → 详情显示新值 | ✅ 详情显示 `13900004321`；**请求体只含 `mobile`**（无 `id_card` / `emp_no`），库里身份证打码值仍在 | `06` `07` `08` |
| ⑧ | 删除 → 二次确认 → 列表里消失 | ✅ 输错名字时确认按钮 disabled；`DELETE` 200；列表查不到；按 id 取 404 | `09` `10` |
| ⑨ | 无 `:create` 权限账号看不到按钮 | ✅ 按钮 0 个节点（`DEPT_LEADER` 账号，有 `:list` 无 `:create`） | `11-no-create-permission.png` |
| ⑩ | 截图存 `docs/evidence/hr-emp-crud/` | ✅ 本目录 11 张 | — |

Playwright 4 条用例全绿（`4 passed (17.2s)`，见 `00-verification-log.txt` 末尾）：
`① 新建 / ② 编辑 / ③ 删除 / ④ 权限`，全程打真接口，无 mock。

## 四、验证环境（不是 prod）

- 前端：`vite dev :5189`（`BACKEND_ORIGIN=http://127.0.0.1:18089`）
- 后端：本机隔离实例 `:18089`，`profile=test`，库 `cp_hr_emp_crud`（**新库**，非 prod 数据）
- 造的数据：`E2E*` 前缀员工，用例内部创建、`afterAll` 删除；截图里出现的 `EMP-00xx` 都出自隔离库。

## 五、顺带发现的阻塞问题（**不属本卡，未修**）

后端 `cargo run` 起不来：迁移号段撞车 —

```
duplicate up migration for version 397:
  migrations/system/V0397__preference_palette_ids_gallery77.up.sql
  migrations/business/V0397__hr_salary_period_after_reverse.up.sql
```

两条 V0397 都在 `main` 上（配色卡与 HR-40 各加了一条），nik 要求版本号全局唯一 ⇒
**任何一次后端重启都会 FATAL**（现网进程是旧二进制所以还没炸）。
本卡为跑真机验收，把迁移目录复制到仓库外、摘掉 system/V0397 后才起得来
（`[database.migration] path` 指过去，`config-test.toml` 是 gitignore 的本地文件）。
修法要动迁移号段（V0399 + 已应用库的指纹处理），属后端范围，已另开卡。

> **后续（2026-11-13，卡 t_83977c43 判定）**：已确认 **144 生产库最高只应用过 V0391**，
> 两条 V0397 都**没有**上过生产 ⇒ 允许改号。判定给「配色那条」改号为
> `system/V0399__preference_palette_ids_gallery77`（内容不变，只换号），HR-40 的
> `business/V0397` 保持不动。另实测：改号后**不需要**为「已应用旧号的库」做任何指纹处置 ——
> 旧记录会变成 `Missing`，新号会被幂等补跑（本条是本卡的实测结论，见
> `docs/architecture/ADR-003-迁移版本号撞号处置.md`）。
