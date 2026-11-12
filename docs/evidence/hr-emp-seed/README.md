# t_f0b3df83 · HR 员工档案「从系统用户生成」入口（seed-from-users 前端化）

> 后端 HR-17 早就实现了 `POST /api/business/hr/employee/seed-from-users`（dry-run 预览 + 逐条事务落库），
> 但**页面上没有入口**，HR 只能靠接口文档知道有这个能力。本卡把它接到员工档案页：
> 工具栏按钮 → 参数 → 预览（不写库）→ 确认生成（写库）→ 结果。
>
> 本文件是**可复查**的验收记录：改了哪几个文件、每条卡面判据对着哪张图 / 哪条命令、
> 以及三处「卡面描述与后端实现不一致」的地方（照后端契约落地，理由写在下面第三节）。

## 一、改了什么

| 层 | 文件 | 说明 |
|---|---|---|
| API 契约 + 纯逻辑 | `frontend/src/api/business/hr/employee-seed-kinds.ts`（新） | `SeedFromUsersReq` / `SeedItem` / `SeedResult` 逐字段对齐 `employee_seed.rs` 的 `EmployeeSeedReq` / `SeedItem` / `EmployeeSeedRes`；`SEED_ROUTE` / `SEED_PERM` / `SEED_MAX_CANDIDATES` / `SEED_ACTIONS` / 6 个 `SEED_SKIP_REASONS` / `SEED_REASON_LABELS`；纯函数 `summarizeSeed`（四桶 + 跳过原因分组）、`filterSeedRows`、`canApplySeed`、`applyButtonText`、`seedHeadline`、`emptyPlanHint`、`seedNoticeText`、`rowActionLabel` / `rowActionTheme`、`reasonLabel`、`cellOrDash` |
| 请求层 | `frontend/src/api/business/hr/employee.ts` | `seedFromUsersPreview`（显式 `dry_run: true`）/ `seedFromUsersApply`（`dry_run: false`），路径写全 `/api/business/hr/employee/seed-from-users`；头注接口表补 #10；`export * from './employee-seed-kinds'` |
| 页面 | `frontend/src/views/business/hr/employee/index.vue` | ① 工具栏「从系统用户生成」(`variant="outline"`、图标 `usergroup-add`、`v-if="canCreate"`)；② 三步对话框：参数（部门 `DeptTreeSelect` + 入职日期，默认业务日 `+08:00`）→ 预览（横幅「不会写库」+ 四桶数字 + 跳过原因标签 + 明细表 + 分桶）→ 已落库（成功/失败分开展示、工号列回填、列表自动刷新）；③ 权限门：无 `business:hr:employee:create` 时按钮不渲染；④ 预览/执行两步，`seedFromUsersApply` 在整份页面里**只有一个调用点**（`runSeedApply`） |
| 单测 | `frontend/tests/unit/hr-employee-seed.test.ts`（新） | 16 条：与 Rust 源码逐条对拍（路由 / 权限码 / `MAX_CANDIDATES` / 6 个原因码 / `CREATE_FAILED` / `dry_run` 缺省 `unwrap_or(true)` / 出参字段名）；汇总算术（预览、落库、未知原因码不丢计数）；两段式语义（`canApplySeed` 四种情形）；横幅文案不混用；**接线断言**（预览传 true、执行传 false、执行只有一个调用点、按钮挂 `canCreate`） |

## 二、验收逐条（卡面判据 → 证据）

| # | 卡面判据 | 结果 | 证据 |
|---|---|---|---|
| ① | typecheck + build 通过 | ✅ `typecheck_exit=0`；`✓ built in 51.25s`（`build_exit=0`） | `00-verification-log.txt` |
| ② | 点「从系统用户生成」→ 看到预览列表 | ✅ 按钮在工具栏（`01`），对话框参数步（`02`），预览表 7 行（`04`） | `01-toolbar-seed-button.png` `02-dialog-config.png` `04-preview-all-buckets.png` |
| ③ | 预览里的人数 = `sys_user` 里未建档案的人数 | ✅ `scanned=7`（= 隔离库 `sys_user` 活账号 7 个）；`planned_create=2`（可建档）；`skipped=5`（其中 `HAS_EMPLOYEE` 1 = admin 已绑 `EMP-0001`）；**`未建档案 6 = 将新建 2 + 其它原因跳过 4`** | `04-preview-all-buckets.png` + `00-verification-log.txt` 的接口回执 |
| ④ | 点确认 → 生成成功，列表刷新能看到新员工 | ✅ `created=2`（`EMP-0008` / `EMP-0009`，后端取号），列表 `共 3 人`，两行带「已绑」图标，接口回读 `user_id` 非空、`status=ACTIVE` | `05-applied-dialog.png` `06-list-after-apply.png` |
| ⑤ | **再点一次预览 → 「将新建 0 人 / 跳过 7 人」** | ✅ `planned_create=0`、`skipped=7`、`next_no=null`；说明句「其中 3 人已有员工档案，其余 4 人见下表原因」；「确认生成」变灰 + 文案「无可生成账号」 | `07-preview-idempotent-empty.png` |
| ⑥ | 截图存 `docs/evidence/hr-emp-seed/` | ✅ 本目录 9 张 + 验证日志 | — |

真机 Playwright **5/5 绿**（`17.7s`，见 `00-verification-log.txt` 末尾），全程打真接口、无 mock：

1. `① 入口可见 + 预览列表`（含**接口级**证据：请求体 `dry_run: true`、回执 `created=0`、预览前后员工总数都 = 1 → 确实没写库）
2. `② 确认生成`（请求体 `dry_run: false`、`created=2`、工号 `^EMP-\d{4}$`、列表刷新 3 人、`user_id` 回写）
3. `③ 幂等`（第二遍预览 0/7，按钮禁用）
4. `④ 部门筛选`（选「总部」后王五由 `缺部门` 变 `将建档`，部门显示「总部」——`dept_id` 兼作筛选与缺省部门）
5. `⑤ 权限门`（`DEPT_LEADER` 账号：有 `:list`、无 `:create` → 两个写入口 0 个节点 + 「无建档权限」提示）

单测：**1704 tests / 0 fail**（新增 16 条）。

## 三、三处「卡面与后端不一致」（照后端契约落地）

1. **「冲突 K 人（身份证 / 手机重复）」在后端不存在。**
   `employee_seed.rs` 的判定只有 6 个跳过原因码（`HAS_EMPLOYEE` / `NOT_HUMAN` / `USER_DISABLED` /
   `NAME_MISSING` / `NAME_TOO_LONG` / `DEPT_MISSING`），而且 `build_write` **不猜敏感字段**
   （`id_card` 留空 → 不可能撞 `uk_biz_employee_idcard`），`biz_employee` 也没有手机号唯一键。
   真正可能撞的是 `emp_no`（取号器保证）与 `user_id`（候选查询已 `LEFT JOIN` 排除），
   撞了会落 `FAILED` 桶（`reason=CREATE_FAILED` + 库层人话 + 业务码）。
   → 面板给的是**四桶**：扫描 / 将新建 / 跳过 / 失败，跳过再按 6 个原因码分组。
   若将来后端真加了「冲突预检」，只需在 `SEED_SKIP_REASONS` + `SEED_REASON_LABELS` 各加一项，界面自动多一个分组。
2. **`NAME_TOO_LONG` 实际不可达**：`sys_user.real_name` 本身就是 `VARCHAR(50)`，库里放不下 >50 字姓名，
   后端这条是对 `biz_employee.name` 列宽的**防御性检查**。前端仍保留该码的中文标签（契约里有就得能显示）。
3. **`dept_id` 是「筛选 + 缺省部门」双重语义**（不是纯筛选）：传了就只处理「本部门 + 无部门」的账号，
   无部门的账号挂到该部门下。面板的提示句按这个语义写，用例 ④ 真机验证了「王五（无部门）选了总部后变可建档」。

## 四、验证环境（**不是 prod**）

| 项 | 值 |
|---|---|
| 前端 | `vite dev :5189`（`BACKEND_ORIGIN=http://127.0.0.1:18089`） |
| 后端 | 本机隔离实例 `:18089`，`profile=test`，库 `cp_hr_emp_seed`（空库起、跑全量迁移） |
| 账号 | `admin`（SUPER_ADMIN，初始口令由 `NIK_ADMIN_PASSWORD` 注入后首次登录改密为 `Seed@123456`）；`hr_readonly_verify`（`DEPT_LEADER`，用例 ⑤ 现造现删） |
| 起始数据 | 7 个 `sys_user`（见 `02-seed-users.sql`：2 可建档、1 无部门、1 停用、1 ROBOT、1 无姓名、admin 已绑 `EMP-0001`）+ 1 份员工档 |
| 迁移号段撞车绕过 | 现网 `main` 上 `system/V0397` 与 `business/V0397` 撞号（后端 P0，卡 `t_b4cc9e22` 已开）⇒ 把 `migrations/` 复制到仓库外、摘掉 `system/V0397__preference_palette_ids_gallery77` 后用它启动（`[database.migration] path`），**没有改任何被 git 跟踪的迁移文件** |

## 五、复跑方法（谁都能重现）

```bash
WS=<scratch>/hr-emp-seed-verify
# 1. 后端（隔离库 + 摘掉撞号迁移后的迁移目录）
cd /opt/cloudpivot/backend
NIK_PROFILE=test NIK_DATABASE_PASSWORD=… NIK_MIDDLEWARE_AUTH_JWT_SECRET=… \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) NIK_ADMIN_PASSWORD='Admin@123456' \
  ./target/debug/cloudpivot
# 2. 前端 dev（反代到隔离后端）
cd /opt/cloudpivot/frontend && BACKEND_ORIGIN=http://127.0.0.1:18089 ./node_modules/.bin/vite --port 5189 --strictPort
# 3. 起始数据 → 跑用例（工程在仓库外，node_modules 软链到 frontend/node_modules）
psql … -f $WS/02-seed-users.sql        # 造 6 个候选账号
psql … -f $WS/03-reset-seeded.sql      # 把上一轮 seed 出来的档案软删（用例可反复重跑）
cd $WS/pw && ./node_modules/.bin/playwright test -c playwright.config.ts
```

隔离库与进程：跑完 `cp_hr_emp_seed` 库保留供复查；后端 / vite 进程已停；
`backend/config/config-test.toml`（gitignore 的本地文件）里本卡临时追加的
`[server] listen` / `[database] name,user` / `[database.migration] path` **已还原**，
避免下一个人的隔离实例指向本卡的 scratch 目录。

## 六、顺带发现（不属本卡，未修）

- 迁移撞号 `system/V0397` vs `business/V0397`：任何后端重启 FATAL（`t_b4cc9e22` 已开给 backend-developer）。
- `sys_user.real_name` 与 `biz_employee.name` 都是 50 字，`NAME_TOO_LONG` 只可能是脏数据/别处写入才会命中，
  可以留作防御，但不必在验收里造。
