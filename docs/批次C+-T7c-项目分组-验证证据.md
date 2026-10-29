# 批次 C+ · T7c「项目分组」验证证据

> 卡面：`t_b9f70dab`【任务管理·T7c】项目分组：CRUD + 统计 + 任务归入（可见性不因项目扩大）
> 交付：`fa3d2e3`（功能 + 迁移 + 前端）／生产已部署并复验
> 环境：172.18.10.144；验收实例 `127.0.0.1:8027` + 库 `cloudpivot_t7c`（已清理）；生产 `cloudpivot.service`(:8023) + 库 `cloudpivot`

---

## 0. 一句话结论

T1 建好却一直闲置的 `biz_project` 现在有接口、有页面、能归任务；
**项目仍然只是逻辑分组，不是权限边界** —— 项目负责人 / 项目成员看到的仍是自己有权看的任务，
删除项目也不会删任务（只把 `biz_task.project_id` 置空）。

---

## 1. 交付清单

### 后端（新增模块 `business/project/`）

| 方法 | 路径 | 中间件闸 | 行级判定 |
|---|---|---|---|
| GET | `/business/project/page` | `business:project:read` | 可见性 = 数据范围 ∪ 负责人 ∪ 创建人 ∪ 成员 ∪ 我在该项目下有可见任务 |
| GET | `/business/project/{id}` | `business:project:read` | 同上（**含统计** + 成员） |
| POST | `/business/project` | `business:project:create` | — |
| PUT | `/business/project/{id}` | `business:project:read` | 负责人 / 创建人 ∪ `business:project:update` |
| DELETE | `/business/project/{id}` | `business:project:read` | 负责人 / 创建人 ∪ `business:project:delete` |
| GET | `/business/project/{id}/tasks` | `business:project:read` | 项目可见 **且** 走 T1 任务可见性 |
| GET/POST/DELETE | `/business/project/{id}/members[/{user_id}]` | `business:project:read` | 维护成员需 负责人/创建人 ∪ `project:update` |
| PUT | `/business/task/{id}/project` | `business:task:read` | 任务负责人 / 创建人 ∪ `business:task:update` |

- 错误码新增 **634xx 段**（63401 项目不存在/不可见、63402 编码重复、63403 字段非法、
  63404 无权改删、63405/63406 成员非法/不存在、63407 无权归入任务），
  并加 `project_error_codes_are_stable` 钉住码值。
- 任务侧增量：`TaskCardRes` 下发 `project_id / project_code / project_name`；
  `TaskPageQuery` 增加 `project_id` 过滤；`find_raw` 的列清单补两个项目列
  （否则详情/写路径 `column project_code not found in row` → 500，验收时真实踩到并修掉）。

### 迁移 `V0115__project_grouping.{up,down}.sql`

1. **补建 `biz_project_member`**：卡面背景写「T1 建了 biz_project / biz_project_member」，
   但全仓 `grep -rn project_member` 在 V0100–V0114 与 `src/` **零命中** → 属 T1 漏建，本卡补上。
2. `biz_project.project_no` **未删唯一**（卡面的 `code` ↔ 库列 `project_no`，不新增同义列）。
3. 3 条索引：`idx_biz_project_owner` / `idx_biz_project_status` / `idx_biz_project_member_user`。
4. 5 个权限节点：菜单 `/project`（`component = business/project/index`）+ 4 个按钮。
5. down：撤授权 → 删菜单 → 撤索引 → **任务解绑（`project_id` 置 NULL，任务不删）** → 删成员表，末尾 DO 块断言零残留。

### 种子 `S__task_project_permissions.sql`

- `read` + `create` → 所有未软删角色；`update` + `delete` → 只授 `SUPER_ADMIN`
  （「改别人的东西」是加项，负责人/创建人靠行级关系放行）。
- ⚠️ 文件名刻意用 `S__task_*` 而不是 `S__project_*`：种子按**文件名字典序**执行，
  `S__project_permissions.sql` 会排在 `S__role_data.sql` **之前** —— 空库实测授权 0 行，
  而自检因角色集为空而「真空通过」。改名后重跑空库：`grants_read_create=3 / grants_update_delete=2`。

### 前端

- `api/business/project-api.ts`（含 `pageProjectTasks`）
- `views/business/project/index.vue`：列表（编码/名称/负责人/起止/任务数/完成率/状态）+ 新建·编辑弹窗
  + 详情抽屉（**统计卡片** + 成员 + 项目下任务）+ 删除二次确认
- 路由 `/project`（与 `sys_menu.path` 逐字一致）+ 深链 `?id=` 直接展开详情
- 任务侧：`TaskForm` 加「所属项目」下拉；任务列表加「项目」筛选 + 「项目」列；
  任务详情显示项目名并可点击跳到项目；`constants/permissions.ts` 增加 `PROJECT_PERMISSIONS`

---

## 2. 红线落地方式（不是靠自觉，靠代码结构）

| 红线 | 实现 |
|---|---|
| **项目不是权限边界** | `handler::tasks` 直接调 `task::model::page_visible`（T1 唯一可见性实现），`project/` 里**没有**任何「按 project_id 直查 biz_task」的 SQL；统计 `stats_for_projects` 注入与任务列表同源的可见性片段 |
| **统计也不能泄露** | 项目详情/列表的「任务数 / 完成率 / 工时」只统计**我可见**的任务 → 项目成员 B 看到「任务数 0」，不会推断出「有个你看不见的任务」 |
| **删项目不删任务** | 同一事务：软删项目 → `biz_task.project_id = NULL` → 删成员行；回执带 `unbound_tasks` 数字，与库对账 |
| **门禁** | `scripts/check.sh` 闸② 登记表补入 `biz_project` / `biz_project_member`，两处查询都接 `scope::`（`paginate` / `filter_for`），闸② 命中 0 处 |
| **写权限不用权限码硬编码行级语义** | `PUT/DELETE` 中间件闸用 `project:read`（人人有），行级关系在 `ensure_writable` 里判（T3 的教训：权限码表达不了「负责人」） |

### 一处设计补充（超出卡面字面，需评审知情）

项目可见性比卡面的「成员可见」多了**第 5 支：我在该项目下有可见任务**。

原因：卡面要求「任务详情抽屉显示项目名（可点击跳转到项目）」。任务负责人常常既不是项目负责人、
也不是项目成员；没有第 5 支，这个链接必然 404（用户视角的坏链接）。
第 5 支只放开**项目壳**：项目下任务列表与统计仍各走 T1 —— 验收 6 里 C 打开项目只看到自己那 1 条。

---

## 3. 迁移空库 up → down → up（144 / 库 `cloudpivot_t7c`）

`bash /root/t7c-run/migrate_check.sh` 实测输出（节选）：

```
① 空库启动后（迁移器跑 up + 种子已跑）
schema_row=115/versioned        member_table=1     uk_biz_project_no=1   idx_total=4
project_menus=5                 grants_read_create=3     grants_update_delete=2

② 重复 up（幂等）            up_again_rc=0   NOTICE: V0115 自检通过：成员表 + 唯一索引 + 3 条索引 + 5 个权限节点
③ down                       down_rc=0（脚本内 DO 块断言零残留）
④ down 后                    member_table=0  idx_total=0  project_menus=0  grants_*=0  tasks_bound=0
⑤ 再 up                      up_rc=0         NOTICE: V0115 自检通过 …
⑥ up 后                      member_table=1  idx_total=4  project_menus=5
```
（⑥ 的 grant 仍为 0 属预期：手写 psql 不会重跑种子；应用启动会重跑，见 ①）

---

## 4. 逐条真机验收（`t7c_accept.py`，127.0.0.1:8027 / 库 `cloudpivot_t7c`）

**汇总：断言 46 条，PASS 46，FAIL 0**（日志 `/root/t7c-run/accept_R4.log`）。

| 卡面条目 | 证据（真实输出） |
|---|---|
| 1 建项目 / 列表出现 / code 唯一 | `建项目 200` → `code 归一化大写 PAY-R4` → `列表出现 total=1`；`重复 code → 400 + 63402 项目编码已被占用：PAY-R4` |
| 2 编辑项目 → 值更新 | 详情回读：`名称 T7c 支付中台（改名）R4` / `状态 ARCHIVED` / `结束日 2027-01-31` / `说明 改过的说明` |
| 3 任务归入 / 移出 | `归入 200` → 库 `project_id=01a1875c-920b-…`；`移出 → NULL`；`字段缺席 → 400/10001` |
| 4 **删项目 → 任务还在、project_id 变 NULL** | `删除前项目下任务数=5`（库）→ 回执 `{"deleted":true,"unbound_tasks":5,"removed_members":0}` → `任务 5 条全在 alive=5` / `库里已无任务指向该项目 bound=0` / `5 条任务 project_id 全 NULL` / `项目已软删 t` / 详情 `404 + 63401` |
| 5 统计正确 | 5 个任务（2 完成 / 1 进行 / 2 待开始）+ 工时 16h/6h → `total=5` `done=2` `doing=1` `todo=2` `completion_rate=40.0` `estimate_hours=16.0` `actual_hours=6.0`；列表项同样 `5 / 40.0` |
| 6 **可见性不因项目扩大** | A=项目负责人、B=项目成员、任务 T 负责人=C（B 不是协作人，库证 `member_rows=0`）→ **B 能打开项目**（200）但 `B 的项目统计里任务数=0`、`B 查项目任务列表 total=0 hits=0`；`C（任务负责人）能看见 T total=1`；`A 能看见 T total=1`；`D（与项目无关）→ 404/63401` |
| 7 无 create 权限 → 403 | `D 新建项目 → 403 需要权限: business:project:create`；同账号 `读列表 200`（只有 create 被拒） |
| 401 未登录 | `GET /business/project/page → 401`；`PUT /business/task/{id}/project → 401` |

---

## 5. 前端验收（生产环境真实登录，浏览器实测）

| 卡面条目 | 证据 |
|---|---|
| 8 项目列表 / 详情 / 统计渲染正确 | 列表表头 `编码 \| 项目名称 \| 负责人 \| 起止日期 \| 任务数 \| 完成率 \| 状态 \| 操作`；行 `T7C-PROD-DEMO … 1（完成 0） 0%`；详情抽屉 `项目详情` + 统计卡片 `任务总数1 \| 待开始1 \| 进行中0 \| 待验收0 \| 已完成0 \| 已取消0 \| 预估工时0h \| 实际工时0h` + 任务行 `TSK-202610-000104 … 打开任务` |
| 9 任务表单能选项目、列表能按项目筛选 | 表单标签含「所属项目」，下拉选项 `T7C-PROD-DEMO T7c 部署验收演示项目`；列表新增「项目」列（行显示 `T7c 部署验收演示项目`）；`/task?project_id=…` → `行数 1`、`全部可见任务 · 共 1 条`、筛选框回显 `T7C-PROD-DEMO T7c 部署验收演示项目` |
| 10 删除文案正确 | 确认框：`删除项目「T7c 部署验收演示项目」 任务不会被删除，只会移出项目（project_id 置空，任务仍留在任务列表里）。确定删除吗？` |

---

## 6. §6.2 部署验收（生产）

```
HEAD=fa3d2e3b6d8f8f704274e7193d5dc05fc5429c99
release 二进制 sha256=80d2f30e54ce49e968961c35c4c4d197b78f5888770c408440c64cfbc6dfcb87
systemd: cloudpivot.service → active；/health → profile=prod ok
库 cloudpivot：schema_row=115/versioned/project_grouping、member_table=1、project_menus=5、
             grants_read_create=15（5 角色 × 3）、grants_update_delete=2（SUPER_ADMIN）
接口未登录：GET /api/business/project/page → 401；GET /api/business/project/{id} → 401
           PUT /api/business/task/{id}/project → 401（三条全 401，非 404，证明路由已注册且鉴权生效）
前端：dist index.html 609B（19:17 重建）、chunk project-api-Ca79TB3I.js；nginx / → 200
真实登录看到项目页 + 任务能选项目：见 §5（浏览器实测）
```

生产验收数据**已清理**（不留演示数据）：

```
删演示项目 → {"deleted":true,"unbound_tasks":1,"removed_members":0}   ← 恰好又是一次「删项目不删任务」实证
软删演示任务 → {"deleted":true}
复查：项目列表 total=0；库证 T7C-PROD-DEMO soft_deleted=t、演示任务 project_id=NULL 且 deleted_at 非空
```

---

## 7. 质量门禁

| 项 | 结果 |
|---|---|
| `cargo test` | **689 passed / 0 failed**（T7a 基线 667 → +22） |
| `cargo clippy --all-targets -- -D warnings` | 0 告警 |
| `cargo fmt --check` | 通过（只格式化本卡改动文件） |
| `scripts/check.sh` | **全部检查通过**（闸① 0 处、闸② 0 处含新登记的 `biz_project`/`biz_project_member`、C7 G1–G6 全通过、色板一致） |
| `pnpm typecheck && pnpm build` | 通过 |

---

## 8. 已知取舍 / 后续

1. **统计口径**：完成率分母**剔除 `CANCELLED`**（与 T5b 统计接口一致）；「没有任务」记 0%，不是 100%。
2. **成员角色** `MEMBER` / `VIEWER` 本卡权限等价（`VIEWER` 为预留），表上 CHECK 已收口。
3. **项目列表的成员数**不按可见性裁剪（成员名册不是任务数据），任务数/完成率则严格裁剪。
4. `SCOPED_TABLES` 未登记 `biz_project`（与 `biz_task` 同款决策：可见性是 OR 语义，
   登记进「数据权限页」会让启动自检误判），锚点用本地 `PROJECT_TARGET` 常量。
