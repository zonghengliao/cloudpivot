# 批次 C+ · T4「FS 依赖 + 只读甘特图」验证证据

> 卡片：`t_687d35b1`｜执行人：manong｜日期：2026-10-29｜环境：`172.18.10.144`（Ubuntu 26.04 arm64）
> 依据：`docs/批次C+-工作任务管理-核心设计.md` §四 / §五、`docs/批次C+-工作任务管理-详细架构.md`、
> `docs/云枢设计修正补丁-20261029.md` 修正 10。

---

## 一、交付清单

### 迁移（3 个）

| 文件 | 内容 |
|---|---|
| `backend/migrations/business/V0110__task_dependency_constraints.up.sql` | ① 补 `lag_days` 列（`NOT NULL DEFAULT 0` + `CHECK >= 0`）；② 反自反约束**存在性自检**（V0100 已建，缺了才补）；③ `idx_biz_task_dependency_on`（反查「谁依赖我」）+ `uk_biz_task_dependency_triplet`（`task_id, depends_on_id, dep_type` 防重）；④ 2 个菜单节点（甘特页 `…0770` / 依赖管理按钮 `…0771`）；⑤ 5 条自检（列 / 约束 / 两个索引 / 节点数量 / 节点字段逐字比对） |
| `backend/migrations/business/V0110__task_dependency_constraints.down.sql` | 撤授权 → 删节点 → 删索引 → 删 `lag` 约束 → 删列（**保留 V0100 的反自反约束**） |
| `backend/migrations/seed/S__task_dependency_permissions.sql` | 甘特页授**所有未软删角色**；`business:task:dependency` **只授 SUPER_ADMIN**（与 T3 的 assign/member 同口径：权限码是「加项」，行级关系才是主口径） |

### 后端

| 文件 | 变更 |
|---|---|
| `backend/src/business/task/dep.rs`（新增，~700 行含 11 条单测） | 纯逻辑：`find_cycle`（BFS + 100 层深度上限 + 闭合路径）、`orient_cycle_from`（路径从被编辑的任务读起）、`render_cycle`（点名每个任务）、`is_blocking_status`（`DONE`/`CANCELLED` 解除，其余阻塞，**状态写坏 fail-closed**）、`resolve_range` / `bar_span` / `today_offset` / `is_overdue`（甘特排期） |
| `backend/src/business/task/model.rs` | 依赖 SQL：`list_prerequisites` / `list_successors` / `blocking_prerequisites` / `dependency_edges`（事务内全量边）/ `find_dependency` / `insert_dependency` / `delete_dependency`（**pair 双向匹配**）/ `task_relation`（父子 + 跨项目）/ `dependency_labels`；甘特 SQL：`gantt_tasks`（复用 `VISIBLE_OR_TAIL` 可见性 + 窗口重叠 + `LIMIT 501`）/ `blocked_task_ids`（一条 SQL 批量算，避免 N+1）/ `gantt_edges` |
| `backend/src/business/task/handler.rs` | 4 个新接口 + `check_dependencies_ready` 真实实现（T2 留的 hook）+ `notify_dependents`（前置 DONE/CANCELLED 通知后继） |
| `backend/src/business/task/dto.rs` | 依赖 / 甘特 DTO 共 11 个 + `PERM_DEPENDENCY` |
| `backend/src/business/task/state.rs` | 三处 `H-REL` / `H-DEP` 占位文案改为真实语义（流转表本身未改） |
| `backend/src/shared/error_code.rs` | `63101`–`63105`（T4 子段）+ 码表单测同步（610 条 lib 单测全绿） |

### 接口（4 个，`/api` 前缀由 `[router].prefix` 注入）

| 方法 | 路径 | 中间件闸 | 说明 |
|---|---|---|---|
| GET | `/business/task/{id}/dependencies` | `business:task:read` | 前置 + 后继两组 + `blocking_count`，**带可见性**（看不见 → 404） |
| POST | `/business/task/{id}/dependencies` | `business:task:read` | 加前置；行级矩阵（负责人/创建人/分派人）∪ `business:task:dependency` |
| DELETE | `/business/task/{id}/dependencies/{dep_id}` | `business:task:read` | `dep_id` = **对端任务 id**，pair 双向匹配 |
| GET | `/business/task/gantt` | `business:task:read` | 甘特数据（时间窗 ≤ 180 天 + 任务 + 依赖边 + 后端算好的条形几何） |

### 前端

| 文件 | 变更 |
|---|---|
| `frontend/src/views/business/task/gantt.vue`（新增 ~660 行） | **纯 SVG 手绘**只读甘特：左列任务名（父子折叠）/ 右侧日·周两档刻度 / 状态色任务条（`--td-*`）/ 进度内条 / 依赖折线 + 箭头 / 今天竖线 / 逾期红描边 / 被阻塞斜纹 / 点条开详情抽屉（复用 T6a `detail.vue`）/ 空态 |
| `frontend/src/api/business/task-api.ts` | 依赖 + 甘特 4 个方法与 8 个类型 |
| `frontend/src/router/index.ts` | `/task/gantt`（`task/gantt` 子路由，`meta.permission = business:task:read`） |
| `frontend/src/views/business/task/index.vue` | 工具条加「甘特图」入口按钮 |

---

## 二、对卡面的三处**有意偏差**（均已在本文件与代码注释写明）

1. **不加 `id` 代理主键**：T1 落地的 `biz_task_dependency` 是业务主键 `PRIMARY KEY (task_id, depends_on_id)`（同一对任务之间只可能有 1 条 FS 依赖）。再补一个 `id` 会形成两个主键候选。因此 `DELETE /dependencies/{dep_id}` 的 `dep_id` 定义为**对端任务 id**（前置组 = 前置 id、后继组 = 后继 id），删除按 pair 双向匹配 —— 前端不需要知道当初谁指向谁。
2. **「部分唯一索引防重」→ 普通唯一索引**：本表没有软删列（依赖删掉即真删），「部分」没有可用谓词；改用 `uk_biz_task_dependency_triplet (task_id, depends_on_id, dep_type)`，与主键的差别是**把 `dep_type` 纳入唯一性**（将来放开 SS/FF/SF 时同一对任务可并存不同类型的依赖）。
3. **重复加 = 幂等成功**（不是 400）：返回 `200 + created:false`，不新增行。前端重复点击（网络重试 / 手抖双击）不该弹红条；同时用 `ON CONFLICT DO NOTHING` 兜底并发。

另：`board_id` 参数本版**不支持**（没有自定义看板列，属 P1），传了就 `400 + 63103` 明确拒绝，**不静默忽略**。

---

## 三、接口验收（36/36 全通过，逐条真机）

> 环境：`/opt/cloudpivot/backend/target/release/cloudpivot`（**部署用 release 二进制**）+ profile=test + 独立库 `cloudpivot_t4v2` + `127.0.0.1:8026`。
> 脚本：`/root/t4_accept.py`；原始输出：`/root/t4_accept_release.out`（本文件第四节为逐条粘贴）。

```
PASS  0.1 迁移 V0110 已应用   | schema_migrations version=110
PASS  0.2 lag_days 列 / 反自反约束 / 两个索引都在   | lag_days + ck_not_self + idx_on + uk_triplet
PASS  0.3 权限节点 business:task:dependency + 甘特页节点均在   | …0770 甘特页 / …0771 dependency
PASS  0.4 未带令牌访问甘特接口 → 401   | HTTP 401
PASS  1.1 业务账号 t4owner 就绪（后继负责人）   | owner_id=01a1867f-7779-75e2-9489-fbedce34e047
PASS  2.1 加前置 A→B 成功（B 依赖 A）   | {"code":0,...,"created":true,"lag_days":0,...}
PASS  2.2 B 的前置含 A   | prerequisites=['TSK-202610-000001']
PASS  2.3 A 的后继含 B   | successors=['TSK-202610-000002']
PASS  2.4 B 的 blocking_count=1（A 未完成）   | blocking_count=1
PASS  3.1 自依赖 → 400 + 63103   | {"code": 63103, "msg": "任务不能依赖自己（自依赖会让「开始」永远等不到「完成」）"}
PASS  4.1 重复加同样的依赖 → 200 + created=false（幂等，不重复插入）   | {"code":0,...,"created":false,...}，库里 1 行
PASS  5.1 成环 → 400 + 63101   | {"code": 63101, "msg": "依赖成环，已拒绝：TSK-202610-000001「T4-A 数据库设计」 → TSK-202610-000003「T4-C 上线验收」 → TSK-202610-000002「T4-B 接口联调」 → TSK-202610-000001「T4-A 数据库设计」（箭头读作「依赖」）"}
PASS  5.2 响应点名环路径（含 A/B/C 三个任务编号）   | 同上（三个编号 + 闭合箭头齐全）
PASS  5.3 环没被落库（依赖数仍为 2）   | count=2
PASS  6.1 前置未完成 → 409 + 63102   | {"code": 63102, "msg": "前置任务未完成：TSK-202610-000001「T4-A 数据库设计」（待开始）。等它们完成（或取消）后再开始；管理员可强开并留痕（T7）"}
PASS  6.2 响应点名未完成前置（A 的编号 + 标题 + 状态）   | 同上
PASS  7.1 A 置 DONE 成功   | {"status":"DONE","progress":100,...}
PASS  7.2 前置 DONE 后 B 进 DOING 成功   | {"from":"TODO","status":"DOING","progress":10,...}
PASS  8.1 前置 CANCELLED：依赖保留但 blocking=false   | prerequisites 仍 1 条，status=CANCELLED，blocking=false，blocking_count=0
PASS  8.2 前置 CANCELLED 不阻塞，D 可以进 DOING   | {"from":"TODO","status":"DOING",...}
PASS  9.1 前置完成 → 后继负责人（t4owner）收到站内信   | rows=['前置任务已完成，可以开始了|01a1867f-77e8-...|t4owner']
PASS  9.2 通知正文包含前置任务编号与「可以开始了」   | 前置任务「T4-A 数据库设计」（TSK-202610-000001）已完成，任务「T4-B 接口联调」（TSK-202610-000002）可以开始了
PASS  10.1 父子任务之间加依赖 → 400 + 63103（语义拒绝）   | {"code": 63103, "msg": "「TSK-202610-000006」与「TSK-202610-000007」是父子任务，父子之间不能建依赖（父任务进度由子任务汇总，加依赖会语义混乱）"}
PASS  11.1 甘特接口 200 + 时间窗回显正确   | range={'start': '2027-01-05', 'end': '2027-01-20', 'days': 16, 'today': '2026-10-29', 'today_offset': None}
PASS  11.2 tasks 恰含 3 个有档期任务（2 个无档期的不返回）   | tasks=['TSK-202610-000008', 'TSK-202610-000009', 'TSK-202610-000010']
PASS  11.3 dependencies 恰 2 条（两端都在结果里）   | dependencies=[{from:G1,to:G2},{from:G2,to:G3}]
PASS  11.4 条形位置与日期对应（G1 偏移 1 天 / 长 3 天；G3 偏移 8 天 / 长 3 天）   | G1={'offset_days': 1, 'length_days': 3} G3={'offset_days': 8, 'length_days': 3}
PASS  11.5 今天偏移在窗口外 → today_offset=null（不画今天线）   | today=2026-10-29
PASS  11.6 被阻塞标记：G2/G3 有未完成前置 → blocked=true，G1=false   | G1=False G2=True G3=True
PASS  12.1 时间窗 365 天 → 400 + 63105   | {"code": 63105, "msg": "时间窗过大：365 天，上限 180 天（请缩小区间后再查）"}
PASS  13.1 缺省时间窗 = 今天起 30 天（today_offset=0）   | range={start:2026-10-29,end:2026-11-27,days:30,today_offset:0}
PASS  13.2 scope 取值非法 → 400（不静默降级）   | {"code": 10001, "msg": "scope 取值非法：bogus（应为 my / all）"}
PASS  13.3 board_id 本版不支持 → 400 + 63103   | 明确拒绝，不静默忽略
PASS  14.1 删依赖成功（dep_id=对端任务 id）   | removed=true，两组回执同步刷新
PASS  14.2 再删同一条 → 404 + 63104（不静默成功）   | {"code": 63104, "msg": "这两个任务之间没有依赖关系（可能已被他人解除，请刷新）"}
PASS  14.3 解除后甘特依赖边剩 1 条   | dependencies 1 条

================ 汇总 ================
共 36 条，通过 36，失败 0
```

### 环检测「不许只判一层」的证据

`5.1` 的三任务环（A 依赖 C、C 依赖 B、B 依赖 A）是**跨 3 层**才回到起点的环；单层判定会漏。
另有 6 条纯逻辑单测覆盖更长的环与边界（见第五节）。深度超 100 层时 **fail-closed**（`CycleVerdict::TooDeep` → 同样 `400 + 63101`），不会放行。

### 并发安全

环检测与插入在**同一事务**内，前置 `SELECT pg_advisory_xact_lock(7340105)` 串行化
（`dep.rs` 注释记录了「两个请求各自看不环、一起闭圈」的真实风险）。

> 实测坑：`pg_advisory_xact_lock` 返回 `void`，用 `query_sql` 会 `error deserializing column 0`（500/10004），
> 必须用 `exec_sql`。已在 `handler::add_dependency` 注释中固化。

---

## 四、甘特前端实测（真机浏览器，生产环境 172.18.10.144）

登录方式：真实管理员登录拿到的令牌注入 `localStorage['x_access_token']`（页面禁止在浏览器里输入口令）。

### 4.1 渲染与几何（`svg.gantt-svg` DOM 实测）

页面文本（只列出关键部分）：

```
返回列表 | 甘特图 | 只读 | 上一屏 今天 下一屏 | 日 周 | 全部可见 只看我的 | 刷新
窗口 2026-10-29 ~ 2026-11-27（30 天） · 任务 5 条 · 依赖 2 条
任务（5） 10-29 11-01 11-02 11-09 11-16 11-23
归档旧版操作手册 / 对接 ERP 库存接口 / T4验收-需求确认 / T4验收-接口开发 / T4验收-联调验收 / 今天
```

SVG 几何（`svgWidth=1180`、`svgHeight=228`、`LEFT_W=280`、`ROW_H=34`、日视图 `30px/天`）：

| 任务 | 条形 `x` | 条形 `width` | 说明 |
|---|---|---|---|
| 归档旧版操作手册（10-29~11-09） | 280 | 328 | `280 + 0×30`；11 天 → `11×30-2 = 328` |
| T4验收-需求确认（10-30~11-01） | 310 | 88 | `280 + 1×30`；3 天 → `88` |
| T4验收-接口开发（11-02~11-05） | 400 | 118 | `280 + 4×30`；4 天 → `118` |
| T4验收-联调验收（11-06~11-09） | 520 | 118 | `280 + 8×30`；4 天 → `118` |

- **进度内条**：首条任务进度 90% → 内条宽 `295.2 = 328 × 0.9` ✓
- **今天竖线**：`x1 = 295`（`280 + 0×30 + 15`），`stroke = #c17b7e`，与 `--td-error-color` 一致 ✓
- **依赖线**：2 条折线 `398,131 408,131 408,165 396,165` / `518,165 528,165 528,199 516,199`（前置右端 → 折 → 后继左端，带 `marker-end` 箭头）✓
- **被阻塞斜纹**：两个后继任务各多一块 `fill="url(#gantt-blocked)"` 覆盖层 ✓
- **状态色**：`#67959e`（进行中 / brand 系）、`#c1ac7b`（待验收 / warning 系）—— 全部来自主题变量，不是写死 hex ✓

### 4.2 逾期标红

新增一条 `start=2026-10-24 / due=2026-10-25 / TODO` 任务，点「上一屏」（窗口 2026-09-29~10-28）后实测：

```
red = [{x: 1030, w: 58, stroke: "#c17b7e"}, ...]   # 与 --td-error-color 逐字相同
--td-error-color = #c17b7e；stroke-width = 2
```

（默认窗口从今天起，纯逾期任务落在窗口外属预期 —— 甘特只画与窗口有交集的任务。）

### 4.3 只读确认（拖拽无反应）

```
drag = {
  before: {x: "280", y: "54"},
  after:  {x: "280", y: "54"},
  moved: false,
  wroteRequests: [],      # 期间没有任何 /api/business/task 写请求
  hasDraggable: 0,        # SVG 内没有 draggable="true"
}
svg cursor = "default"    # 不是 grab / move
```

同时派发了 `pointerdown → pointermove → pointerup` 与 `mousedown → mousemove → mouseup` 两种序列，
条形坐标与依赖线均未变化 —— **本版没有实现拖拽**（拖拽改期属 T7，且修正 10 要求改期后重跑依赖校验）。

### 4.4 点任务条 → 详情抽屉（复用 T6a）

```
.task-detail 存在，class = "t-drawer t-drawer--right t-drawer--open task-detail"，display=block
内容：任务详情 · TSK-202610-000003 | 归档旧版操作手册 | 待验收 | P2 | 需验收 | 进度 90% | 预估 4 小时 …
```

---

## 五、迁移 up → down → up（真实 PG，独立空库 `cloudpivot_t4m`）

```
== up（首次迁移）
   version=110: 1
   lag_days 列: 1
   索引: idx_biz_task_dependency_on,uk_biz_task_dependency_triplet
   菜单节点: 2
== down（回滚 V0110）
   lag_days 列（应为 0）: 0
   索引（应为空）: (无)
   菜单节点（应为 0）: 0
   反自反约束仍在（V0100 的，不应被 down 删）: 1
== up（重放 V0110）
   lag_days 列（应为 1）: 1
   索引（应两个）: idx_biz_task_dependency_on,uk_biz_task_dependency_triplet
   菜单节点（应为 2）: 2
== 约束负例
   首次插入 OK
   重复插入被拒: ERROR: duplicate key value violates unique constraint "pk_biz_task_dependency"
   自依赖被拒:   ERROR: new row for relation "biz_task_dependency" violates check constraint "ck_biz_task_dependency_not_self"
== 验证库已删除
```

---

## 六、质量门禁

### 6.1 `cargo test`（T3 后基线 + 本卡新增 11 条）

```
EXIT=0
lib: 610 passed; 0 failed
14 个测试二进制合计 passed = 651，全部 ok，0 failed / 0 ignored（7 条 DB 集成测试按设计 SKIP）
新增（business::task::dep::tests）：
  acyclic_and_diamond_are_allowed / direct_cycle_is_rejected / long_cycle_is_rejected_with_full_path /
  self_cycle_is_rejected / depth_limit_fails_closed / cycle_path_is_oriented_from_the_task_being_edited /
  cycle_label_renders_every_node / only_done_and_cancelled_release_the_block /
  blocking_prerequisites_filters_terminals / range_defaults_and_limits / bar_span_maps_dates_to_offsets /
  today_line_and_overdue / gantt_edges_keep_only_both_ends_visible
```

### 6.2 `scripts/check.sh`

```
==> 后端：cargo fmt --check          OK
==> 后端：cargo clippy -- -D warnings OK（0 warning）
==> 后端：cargo check                OK
==> 前端：pnpm typecheck             OK（0 error）
==> 批次B 闸①：0 处 ／ 闸②：0 处（biz_task_dependency 的查询在 model.rs，已随 `scope::` 一并计入）
==> C7 G1–G6：全部 PASS
==> 偏好字典与色板一致性：PASS
==> 全部检查通过
```

`scripts/check.sh` 的闸② 清单本来就含 `biz_task_dependency`（T1 时列入），本卡的依赖查询写在
`business/task/model.rs`（文件已接入 `scope::`），因此**无需豁免**。

---

## 七、§6.2 部署验收（生产）

| 步骤 | 结果 |
|---|---|
| `cargo build --release` | 成功；部署二进制 `backend/target/release/cloudpivot`（本次验收脚本就是用它跑的） |
| `pnpm build` → `frontend/dist` | 成功（nginx :80 直接托管，不宕机换内容） |
| `systemctl restart cloudpivot` | `active`；启动日志 `215 条 #[permission] 规则`、`cloudpivot 启动 -> http://127.0.0.1:8023` |
| 生产库迁移 | `schema_migrations version=110` = 1；`biz_task_dependency.lag_days` 存在；菜单节点 2 个 |
| 接口 401 | 匿名 `GET /api/business/task/gantt` → **401** |
| 真实登录看甘特页 | `http://172.18.10.144/task/gantt` → 标题「甘特图 - 云枢管理后台」，侧边栏有「甘特图」子菜单（V0110 节点 …0770），页面渲染 5 条任务 / 2 条依赖（见第四节） |
| 收尾 | 本轮在生产造的 4 条 `T4验收-*` 任务已**软删**（`remaining: 0`），临时 token 文件已删 |

---

## 八、清理（无残留）

- 验收实例 `127.0.0.1:8026` 已停；验收库 `cloudpivot_t4v` / `cloudpivot_t4v2` / `cloudpivot_t4m` 已删。
- 隔离编译目录 `/var/tmp/t4-target`、运行目录 `/root/t4-run`、`/root/t4-mig` 已删。
- 生产库只留结构性变更（V0110 + 权限节点），无残留业务数据。

---

## 九、已知边界与后续

1. **拖拽改期属 T7**：本版甘特**只读**（`hasDraggable=0`、无写请求）。T7 加拖拽时**必须**在改期后重跑环检测 + 「前置不得晚于后继开始」校验（修正 10）。
2. **强开前置（`force=true` 绕过依赖）属 T7/管理员能力**：本次只做「拒绝并点名」，错误消息里已提示「管理员可强开并留痕（T7）」。
3. **`lag_days` 只存不算排程**：`FS+lag` 的滞后语义不参与本次条形几何；T7 改期校验时才会用到。
4. **`board_id` 维度不做**：无自定义看板列（P1），接口显式 400 拒绝。
5. **甘特单次上限 500 条**（超过按排期升序截断并置 `truncated=true`），时间窗上限 180 天 —— 两者都是防「一次查询把整表画出来」。

---

## 十、产物指纹（供安全 / 运维复核）

| 项 | 值 |
|---|---|
| 功能提交 | `29ed91d` — `feat(task): T4 FS 依赖（环检测/前置阻塞/联动通知）+ 只读 SVG 甘特图` |
| 提交内容 | 15 files changed, 3603 insertions(+), 17 deletions(-) |
| 三方一致 | `HEAD` == `origin/main` == `bare main` == `29ed91d692…db931d` |
| 后端产物 | `backend/target/release/cloudpivot`，sha256 `3d3aab25563b6ea75b7816ad47de6ae21d29f338dbd08be5832f854f4e4a5564`（构建于 15:08，即本次功能代码；验收脚本跑的就是这个二进制） |
| 前端产物 | 甘特页独立 chunk `assets/gantt-Bl7qznxS.js`（11420 B，内含 `gantt-blocked` 斜纹定义），构建于 15:09 |
| 生产库 | `schema_migrations` 含 `110`；`sys_menu` 含 `…0770` / `…0771` |
