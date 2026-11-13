# AGENTS.md — 云枢通用后台系统（基础版 CloudPivot）

给 AI 编码 Agent 的项目说明。读完本文件即可安全动手；深入约定见 `skill/SKILL.md`、`docs/README.md`（二开文档集入口）与各分册。

## 项目是什么
基于 **Rust + NIK 框架（nik）+ Vue 3 + TDesign** 的通用后台底座（基础版）。提供用户 / 角色 / 菜单 / 字典 / 配置 / 文件 / 日志 / 在线用户 / 定时任务等通用后台能力。

> 专业版 `cloudpivot-pro`（数据权限 / 工作流 / 对象存储 / 导入导出）与企业版 `cloudpivot-saas`（多租户）是**独立仓库**，不在本目录。

## 技术栈
- 后端：Rust 1.94.1+ / edition 2024，框架 `nik`（门面 crate，能力靠 feature 点单）；PostgreSQL。
- 前端：`@cloudpivot/web` Vue 3.5 + TS + Vite 6 + TDesign Vue Next + Pinia + ECharts。
- 默认端口：后端 `8023`，前端 dev `5173`（代理 `/api`、`/health` → `:8023`）。
- 默认账号：`admin / Admin@123456`（SUPER_ADMIN 跳过权限校验）。

## 目录
```
backend/src/
  system/   support/      # 底座两个容器：每模块 mod+handler+dto+model（不设 service 层）
  business/               # 二开业务容器（与上面平级，不侵入底座）：erp / flow / task / impexp / im / pdf / hr …
                          #   同样四件套；HR 见下方「人力资源模块（HR）口径」
  middleware/ task/ utils/ shared/ bootstrap.rs
  lib.rs                    # 启动链 App::new()?...scan().start()
frontend/src/
  api/{system,support,business}/   views/{system,support,business}/
  router/index.ts  stores/  layouts/  components/
```

## 常用命令
```bash
# 后端（backend/）
cargo run                       # 自动迁移 + 启动 :8023
cargo check                     # 类型检查（零警告才提交）
cargo fmt && cargo clippy -- -D warnings

# 前端（frontend/）
pnpm install && pnpm dev        # :5173
pnpm typecheck                  # vue-tsc --noEmit

# 一键工程（项目根）
bash scripts/dev.sh             # 依赖容器 + 后端 + 前端
bash scripts/check.sh           # 提交前检查
bash scripts/build.sh           # 构建 release + 前端 dist
bash scripts/demo_seed.sh       # 一键灌演示数据（--clean 只删演示数据，--dry-run 只预演）
docker compose -f docker/docker-compose.yml up -d   # PG + Redis

# 人力资源（HR）验收（演练库只建在 145；库名须含 drill/e2e/test，脚本自己 DROP）
bash scripts/verify/hr/hr_49_release_drill.sh fresh   # 空库首启 + 克隆库升级 + 护栏反证
bash scripts/verify/hr/hr_salary_e2e.sh --fresh       # 薪资链路 10 步（98 断言）
bash scripts/verify/hr_invariants.sh                  # §4.9 的 10 条巡检断言（只读，可对生产跑）
```

## 必须遵守的约定
1. **后端每模块四件套** `mod.rs + handler.rs + dto.rs + model.rs`；业务编排在 `handler`，数据访问在 `model`，**不设 service 层**。
2. **二开业务放 `business/` 容器**（后端 `backend/src/business/<模块>/`、前端 `api/business/`、`views/business/<模块>/`），与 `system/`、`support/` 平级，**不侵入框架底座**。
3. **依赖铁律**：`Cargo.toml` 只声明 `nik` 一个框架 crate（`nik-utils` 必要时可加）。**不出现** `nik-db` / `nik-db-core` / `nik-web` / `nik-config`。业务代码统一 `use nik::prelude::*`。
4. **feature 语义**：默认集是 `core`（非 `full`）。用数据库必须显式 `postgres`/`mysql`/`sqlite`（`db` 只表能力、不含驱动）。预设 `mini ⊂ lite ⊂ full`；常用 `lite`（= core+postgres+cache+plugin+fs-local）。点单 feature：`fs-local`/`fs-minio`、`xlsx`、`sse`、`metrics`、`template`、`xss`。
5. **错误出口只有三个名字**：handler `ResResult<T>`、model `DbResult<T>`、启动/插件/任务 `nik::Result<T>`。handler 内 `DbError/io::Error/nik_core::Error` 直接 `?`，**不要**写多余 `.map_err(ApiError::internal(...))`。
6. **迁移号段**（动手前先 `ls backend/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3` 核对）：2026-11-13 已到 `V0403`；版本 4 位补零，**跨目录全局唯一**（重号 = 服务启动直接失败），已应用迁移指纹校验、不可改。规律：**先落地者定号，后落地者避让**。
7. **权限码** `{模块}:{资源}:{动作}`，与 `sys_menu.auth_code` 和 `#[nik::permission]` 声明一一对应；`SUPER_ADMIN` 短路全部校验。
8. **路由宏路径不含 `/api` 前缀**（由 `[router] prefix` 注入）；`/health` 手动注册豁免前缀。
9. **前端 API 路径必须写全 `/api/...`**（漏前缀会命中 SPA fallback 返回 HTML 导致"响应体格式不符合"）。
10. **TDesign Grid 是 12 列制**（`t-col-xl-2`=一行 6 个、`-xl-3`=一行 4 个；`t-col-xs-24` 不存在，最大 `xs-12`）。
11. **事务已提交后的失败必须有补偿 —— 不许留死单**。
    - 凡是 `tx.commit()` 之后才执行、且**可能失败**的后续动作（过账、生成凭证、调外部服务、发通知），必须在失败路径上把前置状态**显式恢复**（打回草稿 / 标记失败 / 写补偿记录），不能只 `return Err` 把单据留在中间态。
    - **判据**：任何"提交后失败"的场景，用户必须能从界面继续操作（重试 / 重开 / 修正后重来），**不允许需要工程师改库才能恢复**。
    - **最高危**：免审直过分支（`auto_finished`）—— 它没有审批人兜底，失败后没有任何待办会来推进它。这类分支的**每一个** `Err` 都必须打回草稿。
    - 反例（2026-11-08 生产实际发生）：收货单免审过账被"采购单已收满"拦下，代码只对 SoD 错误打回 DRAFT，其他错误直接返回 → 单据永久卡 `PENDING_POST`，无待办、无手工过账接口，只能删单重建。
12. **菜单（`sys_menu`）两列的语义 —— 别把功能说明当名字**。
    - `name` = 唯一标识（权限码关联），短名，不带空格和长句。
    - `title` = **侧边栏显示名，2-6 个字的菜单名**，不带标点、不带说明、不带章节引用。
    - **禁止**把功能说明写进 `title`。反例：`即时 / 月末汇率维护 + 按月批量录入（ARCH-7 §15.6.3 ②）`（38 字，侧边栏直接换行）。
    - 功能说明放 `description` 列 / `docs/` / 前端 tooltip。
    - 同一父节点下 `sort_order` **必须唯一**（插菜单用 10 递增；插空用中间值如 15/25）。重复会导致侧边栏顺序每次刷新都变。
    - **完整规范见 `docs/编码规范.md` §1.6.1「菜单迁移规范」**；门禁由 `scripts/verify/menu_invariants.sh` 的 **8 条断言**兜底（① 孤儿 parent ② `/system/` 归属 ③ `/business/` 归属 ④ path 唯一 ⑤ 菜单总数 400–520 ⑥ title 长度 ≤12 ⑦ 同父 sort 唯一 ⑧ icon 名在 TDesign 图标库里真实存在）。
    - **提交前自查**（两条都应为 0 行）：
      ```sql
      SELECT path, name, title, length(title) FROM sys_menu
       WHERE deleted_at IS NULL AND menu_type IN (1,2) AND length(title) > 12;
      SELECT parent_id, sort_order, count(*) FROM sys_menu
       WHERE deleted_at IS NULL AND menu_type = 2 GROUP BY 1,2 HAVING count(*) > 1;
      ```
13. **姓名：存快照还是实时 join —— 按语义分，不能一刀切**。
    - **审计/历史类 → 必须存姓名快照**（用户改名/删号后要能回溯"当时是谁"）。
      例：状态日志 `operator_name`、操作日志 `username`、审批 `assignee_name`、附件 `uploader_name`。
    - **业务展示类 → join 实时取名**（要显示"现在是谁"）。
      例：列表的「创建人」、详情的「负责人」、合同 `owner`、佣金 `salesperson`。
    - **主数据关联 → join 实时取名**（要显示"最新名"）。
      例：客户 / 物料 / 部门 / 凭证 —— 主数据改名后，单据显示新名才是对的。
    - 反例（2026-11-08 发现）：附件表只存 `uploaded_by` 不存 `uploader_name` → 界面上直接显示 UUID。
    - **防 N+1**：批量 join，不能每行查一次；DTO 里凡是 `*_id` 都要同时返回 `*_name`。
14. **页面显形码与全量接口码分开 —— 别让「能进这一页」等于「能看全公司」**（HR 已试点，2026-11-13 定稿）。
    - 页面行（`menu_type=2`）的 `auth_code` 是 `{模块}:{资源}:page`：只判侧栏显隐 + 前端路由 `meta.permission`，**后端零引用**（连常量都别定义，`permission_audit_tests` 的 N14/N18 会扫）。
    - 全量列表接口的 `{模块}:{资源}:list` 挂在**按钮行**（`menu_type=3`），只交给那条接口。
    - 员工只能看自己的数据时，用**登录即可**的自助接口 `/xx/my`（`#[nik::auth_only]` 显式声明，别写 `permission = ""`），不要用「全量接口 + 收窄参数」。
    - 全文见 `docs/编码规范.md` §1.7.2 与 `docs/architecture/PERM-01-页面码与接口码解耦-决策.md`；ERP / 成本 / 系统模块尚未解耦，过渡期条款见该节末尾。

## 人力资源模块（HR）口径（2026-11-13 定稿 · 照 ARCH-9 §5/§6 逐条回写）

HR 是第一个完整业务模块（`backend/src/business/hr/`，75 个 `.rs` / 约 6.2 万行），改它之前先读
`docs/architecture/ARCH-9-人力资源模块设计.md`（**已定稿，别重新设计**）；改代码看
`docs/后端二开指南.md` §十七，改页面看 `docs/前端二开指南.md` §十一。

**实测基数**（空库首启 335 条迁移后统计，可复核）：

```
10 张表 · 50 条已注册路由（49 业务 + /health 探针）· 41 行菜单（1 目录 + 5 页面 + 35 按钮）
40 个权限码 = 5 个页面显形码 *:page + 35 个按钮码（其中 5 个是全量列表码 *:list）
17 个薪资项（7 条公式）· 4 套审批流程（hr_leave / hr_salary / hr_salary_adjust / hr_employee_resign）
错误码段 68001~68012 · 迁移 17 个文件（16 条版本化 V0383~V0403 + 1 条种子 S__hr_employee_grants）
```

### 菜单树 = 权限码表（41 行菜单 / 40 个码，一行都不能漏）

层级用缩进表示；`sort` 是同级排序（约定 12：同父必须唯一）；`auth_code` 全写；接口路径省略公共前缀
`/business/hr`。表里的码就是 `sys_menu.auth_code`，也是后端 `#[nik::permission]` 声明的码；页面行的码只给前端判显形（见下「四种状态」）。

| 菜单（层级） | type | path | sort | auth_code | 能做什么 / 承载接口 |
|:--|:--:|:--|:--:|:--|:--|
| 人力资源 | 1 | `/business/hr` | 77 | —（目录不挂码） | 顶级目录，`component=Layout`、`icon=user-business`；授页面必须连祖先目录一起授（祖先闭包，缺了非超管侧栏整棵子树静默消失） |
| └ 员工档案 | 2 | `/business/hr/employee` | 10 | `business:hr:employee:page` | 侧栏显形 + 前端路由 `meta.permission`；**后端零引用、无接口** |
| 　├ 新建员工 | 3 | — | 10 | `business:hr:employee:create` | 建档案 `POST /employee`；取自用号不耗号 `GET /employee/next-no`；从账号批量建档 `POST /employee/seed-from-users` |
| 　├ 编辑员工 | 3 | — | 20 | `business:hr:employee:update` | 改档案 `PUT /employee/{id}`；状态流转（转正/停薪/复工）`POST /employee/{id}/status`；绑定或解绑账号 `POST /employee/{id}/bind-user` |
| 　├ 删除员工 | 3 | — | 30 | `business:hr:employee:delete` | 物理删档案 `DELETE /employee/{id}` |
| 　├ 员工详情 | 3 | — | 40 | `business:hr:employee:detail` | 详情（含合同台账 + 薪资标准）`GET /employee/{id}` |
| 　├ 查看明文（身份证/银行账号） | 3 | — | 50 | `business:hr:employee:sensitive` | 身份证 / 银行账号明文 `GET /employee/{id}/sensitive`（每次访问写 `audit_logs`） |
| 　├ 发起离职 | 3 | — | 60 | `business:hr:employee:resign` | 离职走「离职单 + 流程」；**无独立路由**（状态接口只认 `employee:update`） |
| 　├ 导入员工 | 3 | — | 70 | `business:hr:employee:import` | 导入走通用 impexp（`biz_type=hr.employee`）；HR 侧只有模板下载 `GET /employee/import-template`，契约行 `POST /employee/import` **未落地** |
| 　├ 导出员工 | 3 | — | 80 | `business:hr:employee:export` | 契约行 `GET /employee/export` 有、`ROUTE_CONTRACT` 的 `card=TBD`，**没落地**：前端照契约画导出按钮点了必然 404 |
| 　├ 合同台账维护 | 3 | — | 90 | `business:hr:contract:manage` | 签订 `POST /employee/{id}/contract`；改合同 `PUT /contract/{contract_id}`；续签（旧合同转 `RENEWED` + 链 `renewed_from_id`）`POST /contract/{contract_id}/renew` |
| 　└ 查看全量列表 | 3 | — | 100 | `business:hr:employee:list` | 全量员工列表（关键字/部门/状态筛选 + 数据范围裁剪）`GET /employee/page` |
| └ 考勤管理 | 2 | `/business/hr/attendance` | 20 | `business:hr:attendance:page` | 侧栏显形（两个 tab：考勤期间 / 考勤台账）；后端零引用 |
| 　├ 考勤期间维护 | 3 | — | 10 | `business:hr:attendance:period` | 建期间 `POST /attendance/period`；改 `PUT /attendance/period/{id}`；删 `DELETE /attendance/period/{id}`；汇总重算（幂等）`POST /attendance/period/{id}/recalc` |
| 　├ 考勤记录维护 | 3 | — | 20 | `business:hr:attendance:record` | 台账查询 `GET /attendance/record/page`；单格修正 `PUT /attendance/record/{id}` |
| 　├ 导入考勤 | 3 | — | 30 | `business:hr:attendance:import` | xlsx 导入（impexp `hr.attendance`，带 `period_code`）`POST /attendance/import` |
| 　├ 导出考勤 | 3 | — | 40 | `business:hr:attendance:export` | 导出 xlsx `GET /attendance/export` |
| 　├ 锁定/解锁考勤期间 | 3 | — | 50 | `business:hr:attendance:lock` | 提交 / 锁定 / 解锁 `POST /attendance/period/{id}/status`（锁定后不可改，薪资按锁定版本算） |
| 　└ 查看全量列表 | 3 | — | 60 | `business:hr:attendance:list` | 全量期间批次列表 `GET /attendance/period/page` |
| └ 请假单 | 2 | `/business/hr/leave` | 30 | `business:hr:leave:page` | 侧栏显形（两个 tab：全部请假 / 我的请假）；后端零引用 |
| 　├ 新建请假单 | 3 | — | 10 | `business:hr:leave:create` | 建单（类型/起止/天数/是否带薪/事由）`POST /leave` |
| 　├ 编辑请假单 | 3 | — | 20 | `business:hr:leave:update` | 改草稿 `PUT /leave/{id}` |
| 　├ 删除请假单 | 3 | — | 30 | `business:hr:leave:delete` | **无路由**：请假单不做物理删，草稿作废走 `leave:cancel` |
| 　├ 提交审批 | 3 | — | 40 | `business:hr:leave:submit` | 发起 `hr_leave` 流程 `POST /leave/{id}/submit` |
| 　├ 作废请假单 | 3 | — | 50 | `business:hr:leave:cancel` | 作废 / 销假（理由 ≥5 字，`APPROVED` 也可作废）`POST /leave/{id}/cancel` |
| 　├ 请假单详情 | 3 | — | 60 | `business:hr:leave:detail` | 详情 + 审批轨迹 `GET /leave/{id}`；缺本码时列表自动收窄成 `mine=true`（不是 403） |
| 　└ 查看全量列表 | 3 | — | 70 | `business:hr:leave:list` | 全量请假列表（`data_scope` 裁剪）`GET /leave/page` |
| └ 薪资管理 | 2 | `/business/hr/salary` | 40 | `business:hr:salary:page` | 侧栏显形（两个 tab：薪资单 / 我的薪资）；后端零引用 |
| 　├ 薪资单详情 | 3 | — | 10 | `business:hr:salary:detail` | 单头+部门汇总 `GET /salary/{id}`；明细分页 `GET /salary/{id}/detail/page`；员工薪资标准与调薪历史 `GET /employee/{id}/salary`；前端独立页 `/business/hr/salary/:id` 也判本码 |
| 　├ 试算/重算 | 3 | — | 20 | `business:hr:salary:calc` | 调薪 `POST /employee/{id}/salary`；试算不落库 `POST /salary/calc`；建单落库 `POST /salary`；明细覆盖（改项金额 + 理由）`PUT /salary/{id}/detail/{detail_id}` |
| 　├ 提交审批 | 3 | — | 30 | `business:hr:salary:submit` | 发起 `hr_salary` 流程 `POST /salary/{id}/submit` |
| 　├ 过账（生成凭证） | 3 | — | 40 | `business:hr:salary:post` | 过账生成两张凭证（计提 + 发放）`POST /salary/{id}/post` |
| 　├ 红冲 | 3 | — | 50 | `business:hr:salary:reverse` | 红冲（`reason` 必填，过账后不可改只能红冲）`POST /salary/{id}/reverse` |
| 　├ 导出工资表 | 3 | — | 60 | `business:hr:salary:export` | 按部门分组的 xlsx（员工列 + 全部启用薪资项列）`GET /salary/{id}/export` |
| 　├ 我的薪资（自助） | 3 | — | 70 | `business:hr:salary:my` | 本人薪资列表 + 明细项（按 `employee.user_id` 过滤）`GET /salary/my` |
| 　└ 查看全量列表 | 3 | — | 80 | `business:hr:salary:list` | 全量薪资单列表 `GET /salary/page` |
| └ 薪资项 | 2 | `/business/hr/salary-item` | 50 | `business:hr:salary-item:page` | 侧栏显形（列表 + 公式编辑抽屉）；后端零引用 |
| 　├ 新建薪资项 | 3 | — | 10 | `business:hr:salary-item:create` | 新建（校验公式可解析 + 变量可达 + 无环）`POST /salary-item` |
| 　├ 编辑薪资项 | 3 | — | 20 | `business:hr:salary-item:update` | 改项（改公式后旧单不受影响，靠 `expr_snapshot`）`PUT /salary-item/{id}`；公式校验/依赖图 `POST /salary-item/validate` |
| 　├ 删除薪资项 | 3 | — | 30 | `business:hr:salary-item:delete` | **无路由**：停用走 `update` 的 `is_active`，不删定义（删了历史工资条项名就没了） |
| 　└ 查看全量列表 | 3 | — | 40 | `business:hr:salary-item:list` | 薪资项配置列表（含 `formula`）`GET /salary-item/page` |

**分组计数（自检用，合计必须等于 40）**

| 域 | 页面码 | 按钮码 | 小计 |
|:--|:--:|:--:|:--:|
| 员工档案（含合同） | 1 | 10 | 11 |
| 考勤管理 | 1 | 6 | 7 |
| 请假单 | 1 | 7 | 8 |
| 薪资管理（含薪资单） | 1 | 8 | 9 |
| 薪资项 | 1 | 4 | 5 |
| **合计** | **5** | **35** | **40** |

**40 个码的四种状态（是决定，不是待办；别当 bug 去「修」）**

- **30 个有真路由** —— 上表「能做什么」列已逐条标出方法与路径（`ROUTE_CONTRACT` 编译期契约，50 行；新路由不登记会被单测判红）。
- **2 个契约行未落地**：`employee:import`（走通用 impexp，HR 侧只有模板下载）、`employee:export`（`card=TBD`）。
- **3 个决定不接路由**（`perm_matrix_tests::ROUTELESS_CODES` 钉死）：`employee:resign`、`leave:delete`、`salary-item:delete`。
- **5 个页面码只给前端**（`PAGE_ONLY_CODES`）：`{employee,attendance,leave,salary,salary-item}:page` —— 后端源码里出现即为违约，`permission_audit_tests` 的 N14/N18 会扫。
- 另：**登录即可（无码）**的自助/探针路由 3 条 —— `GET /attendance/my`、`GET /leave/my`（`#[nik::auth_only]`，只出本人，别写成 `permission = ""`）、`GET /health`（模块体检探针，排障用，不挂业务码）。

**角色矩阵**（码集合由 `scripts/verify/hr/hr_perm_expect.py` 解析 `V0387`+`V0401` 得出，**不手抄**；`cargo test --lib business::hr::perm_matrix_tests` 离线兜底）

| 角色 | 码数 | 具体码与理由 |
|:--|:--:|:--|
| SUPER_ADMIN | 40 | 全量。超管本可 `*:*:*` 短路，显式授予只为角色树上能看清范围；空库首启时超管种子晚于迁移，实际靠通配 |
| HR_MANAGER | 40 | 全量 40（5 页面 + 35 按钮），`data_scope=3`（本部门及以下），`sort_order=25` |
| FINANCE_MANAGER | 5 | `salary:{page,list,detail,post,export}`；**不给 `reverse`**（红冲只给人事经理） |
| DEPT_LEADER | 5 | `employee:{page,list,detail}` + `leave:{page,detail}`；**刻意不给 `leave:list`**：它 `data_scope=1`（全部），补了就是看全公司请假，要放开得先给 `biz_leave_request` 补数据范围裁剪 |
| EMPLOYEE | 4 | `leave:{page,create}` + `salary:{page,my}`；**一个 `*:list` 都没有**，自助只走 `/leave/my`、`/salary/my` |
| FINANCE_DIRECTOR | 0 | 零授权对照列（证明 403 来自「没码」，不是「账号不存在」） |

**与 ARCH-9 §5/§6 的交叉自查（2026-11-13 逐条对拍，缺 = 0）**

- §5.2 的 API 去重得 **32 个权限码 → 32 个全部落在上表 40 码内**，没有「文档里有、这里没有」。
- §6.1 的 5 个菜单页 + 1 个不进菜单的详情路由（`/business/hr/salary/:id`）+ 4 个自助视图 → 逐条有落点：5 页 = 5 个 `*:page`；详情路由判 `salary:detail`；我的考勤 / 我的请假 = 登录即可；我的薪资 = `salary:my`；**我的档案至今无接口、无码**（见下偏差 ②）。
- 差额 8 个码 = 设计 §3.2 ④ 单列而 §5.2 无接口的 3 个（上「决定不接路由」）+ `V0401` 解耦新增的 5 个 `*:page`。
- 设计文档自身 3 处口径偏差（**只报不改**，以本表与迁移为准）：① §5.2 表头写「42 条」，正文编号到 46、脚注也写 46 → **实际 46**（`ROUTE_CONTRACT` 50 = 46 + HR-19 补 3 + HR-41 补 `/leave/my`）；② §5.2 脚注「4 处 `/my`」只列了 2 个（第 3 个 `/leave/my` 由 HR-41 补；第 4 处「我的档案」`GET /employee/{my_employee_id}` 至今无卡、无码、无路由）；③ §5.2 说「权限码共 30 个」指业务按钮码，`V0401` 解耦后全量 **40** 个。

- **错误码段 `68001~68012`**（`shared/error_code.rs` 的 68xxx HR 段）：加新码从 `68013` 续，禁跨段复用。
- **数据范围**：列表走 `scope::paginate`（`shared/scope.rs` 的 `SCOPED_TABLES` 登记 + `guard.rs` 条数断言）；单条入口同一份探针，不可见报 404（不是 403）。
- **门禁/验收**（改权限或动业务后）：`cargo test --lib system::menu::permission_audit_tests` + `cargo test --lib business::hr::perm_matrix_tests` + `bash scripts/verify/menu_invariants.sh <库>` + `bash scripts/verify/hr/hr_permission_matrix.sh --fresh` + `bash scripts/verify/hr_invariants.sh`；上线前跑 `bash scripts/verify/hr/hr_49_release_drill.sh fresh`，业务链路跑 `bash scripts/verify/hr/hr_salary_e2e.sh --fresh`。
- **演练库只建在 145（`172.18.10.145`）**，99 是 Agent 主机；库名必须含 `drill`/`e2e`/`test`，脚本自己 DROP。

## 高频踩坑（先读）
- 报表/接口不显示：先查前端 api 路径是否漏 `/api`。
- 图标渲染空白：`user-check`→`user-checked`、`speed`→`rocket`（TDesign 无这两个名）。
- **单据停在中间态（如 `PENDING_POST`）且界面上没有可点的按钮**：查是不是「事务提交后失败没补偿」（见约定 11）。这类单通常审批已过、账没动、没有待办，**只能改库**——遇到就是 bug，要修代码不是修数据。
- **登录后所有接口 400、但后端日志里没有 400**：请求头（尤其累积的 cookie）超过 nginx `large_client_header_buffers`，nginx 在转给后端之前就拒了。查 nginx access.log 的 ` 400 ` 行；容忍上限见 `/etc/nginx/nginx.conf`（当前 4×32k）。

## 详细文档（二开文档集，见 `docs/README.md` 总入口）
- `docs/README.md` — 二开文档集总入口（结构 / 设计亮点 / 前后端二开 / 编码规范 / AI 指引）
- `docs/项目结构与设计亮点.md` — 目录树、17 张表、NIK 设计亮点、部署模型
- `docs/后端二开指南.md` — 后端四件套模板、新增模块流程、迁移/事务/错误出口、§十七 人力资源（HR）
- `docs/前端二开指南.md` — 前端 api/列表页/路由/主题二开、§十一 HR 页面模板
- `docs/编码规范.md` — 后端+前端编码规范与提交前检查（§1.6.1 菜单 / §1.7.2 页面码与接口码解耦）
- `docs/AI二开指引.md` — AI 二开强制约定、反魔幻清单、范本指针
- `docs/architecture/ARCH-9-人力资源模块设计.md` — 人力资源（HR）定稿设计（改 HR 前必读）
- `skill/SKILL.md` — 本仓库 AI 知识包（浓缩速查）
