# 批次 A · A7 前端 8 页 + 权限种子 + 菜单联调 —— 交付与验证证据

> 任务卡：`t_c5b92454`（【批次A·A7】前端8页+权限种子+菜单联调）
> 依据：`docs/批次A-技术架构.md` §2.9 / §3.8 / §4.8 / §5.1 / §5.3、`docs/批次A-产品需求PRD.md` 第八章「页面结构与交互」+ 附录 A（67 条文案 E-STG/E-IMP/E-EXP/E-NTC/E-CMN）
> 分支：`feat/a7-frontend`（worktree `/root/work/a7`）
> 环境事实：本机（`172.18.10.99`）**无法访问** 生产仓库机 `172.18.10.144`（`ssh` 公钥被拒，`/opt/cloudpivot` 与 bare `/opt/git/cloudpivot.git` 均不可达），因此本卡在**本地快照仓库**上完成编码与全部可执行验证，UI 取证走本机零依赖 mock 后端（`tools/a7-mock-server.mjs`，仅取证用，不进生产）。推送/合并动作见 §8。

## 0. 交付物（新增 / 修改）

| 类别 | 文件 | 说明 |
|---|---|---|
| API | `frontend/src/api/business/storage/index.ts` | 存储配置 / 策略 / 文件 / 预签名直传 / 带进度的 XHR PUT |
| API | `frontend/src/api/business/impexp/index.ts` | 定义列表 / 导入三步 / 错误明细 / 导出任务 |
| API | `frontend/src/api/business/notice/index.ts` | 消息 / 未读 / 已读 / 模板 / 试渲染 / SSE 地址 |
| 常量 | `frontend/src/constants/permissions.ts` | 批次 A 33 个权限码**单一来源**（后端注解 ↔ `sys_menu.auth_code` ↔ 前端） |
| 状态 | `frontend/src/stores/notice.ts` | 未读数 + SSE 订阅 + 轮询兜底 + 刷新信号 |
| 页面 | `frontend/src/views/business/storage/backend/index.vue` | ① 存储配置（列表 + 抽屉 CRUD + 连通性测试 + AK/SK 掩码 + 上传策略 tab） |
| 页面 | `frontend/src/views/business/storage/file/index.vue` | ② 文件管理（筛选 / 图片 PDF 预览 / 批量删除 / 直传进度） |
| 页面 | `frontend/src/views/business/impexp/import/index.vue` | ③ 导入向导（上传 → 校验报告 → 确认入库） |
| 页面 | `frontend/src/views/business/impexp/import-task/index.vue` | ④ 导入任务（状态/进度/取消/重试/错误明细下载） |
| 页面 | `frontend/src/views/business/impexp/export-task/index.vue` | ⑤ 导出任务（进度轮询 / 有效期 / 下载 / 删除） |
| 页面 | `frontend/src/views/business/notice/message/index.vue` | ⑥ 消息中心（未读/已读/全部 + 详情 + 单条已读 + 全部已读 + 删除） |
| 页面 | `frontend/src/views/business/notice/template/index.vue` | ⑦ 通知模板（变量面板 + 试渲染 + 测试发送） |
| 页面 | `frontend/src/views/business/notice/send/index.vue` | ⑧ 发送通知（用户/角色/部门含下级/全员 + 立即/定时） |
| 组件 | `frontend/src/layouts/components/NoticeBell.vue` | 顶栏铃铛 + 未读角标 + 最近 5 条未读 |
| 工具 | `frontend/src/utils/download.ts` | 鉴权下载（blob → 临时链） |
| 修改 | `frontend/src/layouts/BasicLayout.vue` | 挂载铃铛 + SSE 连接生命周期 |
| 修改 | `frontend/src/router/index.ts` | 8 条路由 + `meta.permission` |
| 迁移 | `backend/migrations/business/V0046__seed_storage_permissions.{up,down}.sql` | 存储 11 权限点 + 目录 / 2 菜单 |
| 迁移 | `backend/migrations/business/V0047__seed_impexp_notice_permissions.{up,down}.sql` | 导入导出 13 权限点 + 3 菜单；修正 V0045 菜单路径；补消息中心/发送通知隐藏菜单 |
| 取证 | `tools/a7-mock-server.mjs`、`scripts/a7_perm_seed_check.sh`、`evidence/*` | mock 后端、迁移实跑脚本、日志与采样 |

## 1. 硬指标 ①：`pnpm typecheck` 零错误 + `pnpm build` 成功

`evidence/typecheck.log`（尾部）：

```
> @cloudpivot/web@0.1.0 typecheck
> vue-tsc --noEmit

TYPECHECK_EXIT=0
```

`evidence/build.log`（关键行）：

```
4:> vue-tsc --noEmit && vite build
6:vite v6.3.7 building for production...
8:✓ 4583 modules transformed.
88:✓ built in 17.17s
BUILD_EXIT=0
```

`pnpm check:palettes` 也通过（330 条色阶断言、0 失败），新增文件**零硬编码颜色**（`grep -E '#[0-9a-fA-F]{3,8}|rgb\(|rgba\(|hsl\('` 无命中），全部走 `--td-*` / `--app-*`。

## 2. 硬指标 ②：每页「列表能加载 / 新增 / 编辑 / 删除（任务页为对应动作）/ 无权限按钮隐藏」

> 取证方式：本机 `mock` 后端 + 生产构建产物，真实浏览器（Chrome DevTools）操作页面并读 DOM。三套令牌模拟三种权限面：`super`（`*:*:*`）、`ops`（导入导出主链路 + 存储上传/列表/下载 + 消息中心）、`staff`（仅 `business:notice:mine`）。

| # | 页面 | 列表加载 | 新增 | 编辑 | 删除 | 无权限隐藏 |
|---|---|---|---|---|---|---|
| 1 | 存储配置 `/business/storage/backend` | `共 2 条`，两行含「默认 / MinIO / AKIA****7ZP2 / 正常 / 失败」 | 点「新增存储配置」→ 抽屉填表 → 保存：`共 3 条`、新行存在 | 第 3 行「编辑」→ 改名保存：`验收测试 MinIO（已编辑）` 出现 | 第 3 行「删除」→ 二次确认（`确定删除存储配置「…」？删除后不可恢复。`）→ `共 2 条` | `ops` 访问该页 → 路由守卫重定向 `/result/403`（页面级权限 `business:storage:config:list` 缺失） |
| 2 | 文件管理 `/business/storage/file` | `共 2 条`，含「直传 / 中转」列 | 「上传文件」→ 100MB 直传成功（详见 §4），列表增长到 5 行 | 文件无编辑 API（后端只有 `rebind`），页面不提供编辑入口（如实说明） | 行内「删除」→ 二次确认 → `5 → 4` 行 | `ops` 无 `business:storage:delete`：批量删除按钮**不渲染**，行内「删除」`getComputedStyle().display === 'none'` |
| 3 | 导入向导 `/business/impexp/import` | 定义下拉加载（`support.dict_item`） | 上传 → 校验报告：`数据总行数 120 / 校验通过 115 / 失败 5 / 耗时 812 ms` | 步骤 2 失败明细表：`第 12 行 字典标签 编码已存在（与第 88 行重复）` 等 5 条（行号 + 列 + 原始值 + 原因） | 严格模式有失败行时「下一步」`disabled=true`，并提示 E-IMP-19 | `staff` 访问 → `/result/403` |
| 4 | 导入任务 `/business/impexp/import-task` | `4 条`，状态列（待确认 / 成功 / 失败）+ 进度条 | 「发起导入」跳向导 | 「确认入库」→ 二次确认（`即将把 60 行数据写入业务表，不可撤回。`）→ `成功 60，失败 0`，行状态变「成功」 | 「重试」→ 新任务入列；「取消」→ 状态变「已取消」；详情抽屉含错误明细分页（`共 5 条数据`） | 无 `impexp:import:confirm` 的角色不渲染「确认入库」；`staff` 整页 403 |
| 5 | 导出任务 `/business/impexp/export-task` | `3 条`：`并发中 42% 85,400 / 203,481`、`已完成 10-29 17:00（剩 19 小时）`、`已过期，请重新导出` | 「新建导出」→ 选业务 → `导出任务 EXP… 已创建` | 状态轮询 3 s 刷新进度；「取消」置为已取消 | 「删除」→ 二次确认 → 行消失 | `ops` 无 `export:delete`：删除链接 `display:none`；`staff` 整页 403 |
| 6 | 消息中心 `/business/notice/message` | 未读筛选默认：`2 条`（`2 条未读` 标签） | 新消息由发送/Send 进入（见 §2.8） | 点开详情**自动已读**：`2 条未读 → 1 条未读`；「全部已读」→ `没有未读消息` 空态 | 详情区「删除」→ `已删除`，列表 `3 → 2` | `staff`（只有 `business:notice:mine`）**可正常进入**并看到自己的消息（见 §3.3） |
| 7 | 通知模板 `/business/notice/template` | `2 条`（导入完成 / 导出完成，变量列展示 `userName rowCount …`） | 「新增模板」→ 填表 → `模板已新增`，`2 → 3 条`，变量面板自动识别 `userName / rowCount / ttl` | 第 3 行「编辑」→ 改标题 → 保存；「试渲染」输出 `张三 的导出已完成 共导出 4800 行，链接 24 小时有效。` | 第 3 行「删除」→ 二次确认 → `3 → 2 条` | `staff` 无 `notice:template:list`；`ops` 同样无 → 两者都被守卫挡到 `/result/403` |
| 8 | 发送通知 `/business/notice/send` | 打开即带模板预选（`?template=`），变量输入框按模板声明生成 | 填变量 → 「预览渲染」→ `李四 的导出已完成（已编辑）共导出 4800 行，链接 24 小时有效。` | 目标可切 用户/角色/部门（树，含下级）/全员；发送时间 立即/定时（定时给出后端不支持提示） | 点「发送」→ 二次确认 `即将向 全部启用用户 发送通知，不可撤回。标题：…（E-NTC-12）` → `已发送 42 人` | `staff`/`ops` 无 `business:notice:send` → `/result/403`；按钮另加 `v-privilege` |

错误文案均取自 PRD 附录 A 原文（E-STG-03 连通性失败、E-IMP-05/19、E-NTC-05/07/12 等），后端 `msg` 原样展示（8.0 约定第 7 条）。

## 3. 硬指标 ③：权限种子正确 + 非超管账号行为

### 3.1 迁移在真实 PostgreSQL 18.6 上跑通（含幂等复跑与 down 回滚）

脚本：`scripts/a7_perm_seed_check.sh`；日志：`evidence/perm_seed_check.log`。
做法：新建空库 `a7_permcheck` → 按版本序应用 `migrations/{system,support,business}` 与 `seed` → 造一个**非超管角色** `OPS_ONLY` → 首次执行 V0046/V0047 → **再执行一次**（幂等）→ 核对 → down 回滚 → 核对。

```
==== 3. 首次执行 V0046 / V0047 ====
  OK   V0046__seed_storage_permissions.up.sql
  OK   V0047__seed_impexp_notice_permissions.up.sql
==== 4. 幂等复跑（第二次执行不应报错、不产生重复行） ====
  OK   V0046__seed_storage_permissions.up.sql (2nd)
  OK   V0047__seed_impexp_notice_permissions.up.sql (2nd)
==== 5. 结果核对 ====
 menu_type | rows | distinct_codes
-----------+------+----------------
         1 |    1 |              1     ← 一级目录「通用底座」
         2 |    7 |              7     ← 存储管理/文件管理/导入向导/导入任务/导出任务/通知模板/消息中心
         3 |   25 |             25     ← 11 存储 + 13 导入导出 + 1（V0045 已有 9 个 notice，此处只补 menu 载体）
==== 6. 回滚 V0047 / V0046（down） ====
  OK   V0047__seed_impexp_notice_permissions.down.sql
  OK   V0046__seed_storage_permissions.down.sql
==== 7. 回滚后应为 0 行 ====
 leftover_batch_a_menus | 0
 leftover_grants        | 0
```

> 实跑发现并修正了一个会直接报错的细节：`sys_menu.auth_code` 是 `NOT NULL`，一级目录必须写 `''` 而不是 `NULL`（底座种子 `S__menu_data.sql` 就是这么写的）。第一次执行报 `null value in column "auth_code"`，已在 V0046 内注释说明。

### 3.2 授权分支核对（超管 vs 其它角色）

```
  role_code  | granted_menus
-------------+---------------
 OPS_ONLY    |            21
 SUPER_ADMIN |            35
```

`OPS_ONLY`（非超管）实际拿到的权限码（节选，详见日志）：

```
business:notice:mine          | 消息中心
business:storage:upload/list/download   （无 storage:config:* / policy:* / delete）
business:impexp:*（12 个，无 export:delete）
```

即：**非超管能进消息中心，看不到存储配置页**——与 §5.1 的授权策略一致。

### 3.3 前端侧同一结论（浏览器 DOM 证据）

`staff`（仅 `business:notice:mine`）：

```
登录: /dashboard
侧边栏: ['首页']                      ← 通用底座目录下无任何它无权访问的菜单
/business/notice/message   -> /business/notice/message  | 消息中心 - 云枢管理后台      ← 可打开
/business/storage/backend  -> /result/403 | 无访问权限 - 云枢管理后台
/business/storage/file     -> /result/403 | 无访问权限 - 云枢管理后台
/business/impexp/import-task -> /result/403 | 无访问权限 - 云枢管理后台
```

`ops`（有存储上传/列表/下载，无配置管理）：

```
侧边栏: ['首页', '通用底座', '文件管理', '导入向导', '导入任务', '导出任务']
-- 文件管理 --  行数: 2  有「上传文件」: True  有「批量删除」: False
行内: [{'t':'下载','display':'inline-flex'}, {'t':'删除','style':'display: none;','display':'none'}]
```

## 4. 硬指标 ④：直传进度条真实可用（100 MB）

- 文件：`evidence/a7-direct-100mb.bin`（`104,857,600` 字节，`ls -lh` 显示 100M）；
- 链路：`POST /api/business/storage/presign`（2 ms 返回）→ 浏览器 `XMLHttpRequest` 直 PUT 到预签名地址 → `POST /complete` 登记；
- 服务端限速读取（≈5 MB/s）以便观测，前端进度来自 `xhr.upload.onprogress`（`evidence` 采样如下，每秒一次）：

```
t+1s   a7-direct-100mb.bin | 8%  · 直传中
t+2s   a7-direct-100mb.bin | 12% · 直传中
t+3s   a7-direct-100mb.bin | 17% · 直传中
t+5s   a7-direct-100mb.bin | 25% · 直传中
t+7s   a7-direct-100mb.bin | 34% · 直传中
t+9s   a7-direct-100mb.bin | 43% · 直传中
t+11s  a7-direct-100mb.bin | 51% · 直传中
t+13s  a7-direct-100mb.bin | 60% · 直传中
t+16s  a7-direct-100mb.bin | 74% · 直传中
t+19s  a7-direct-100mb.bin | 87% · 直传中
t+21s  a7-direct-100mb.bin | 95% · 直传中
t+22s  a7-direct-100mb.bin | 98% · 直传中
t+23s  a7-direct-100mb.bin | 直传完成
```

进度单调递增、无回退，终态「直传完成」，随后列表出现该文件（`共 5 条`）。

**本卡实测修掉的两个真 bug（不是环境问题）**：

1. **进度条不动**：`uploadTasks.value.push(task)` 之后继续改 `task`（原始对象）不会触发视图更新，界面永远停在「0%」。改为取回数组里的响应式代理再改字段（`views/business/storage/file/index.vue`）。
2. **大文件迟迟不发第一个字节**：原先对所有文件整文件算 sha256 做秒传，100 MB 要读完整文件才开始上传。改为 `> 32 MB 跳过哈希`（`HASH_LIMIT`），秒传只服务中小文件。

## 5. 配色与风格

- 新增页面**零硬编码颜色**，只用 `--td-*` / `--app-*`（品牌 `var(--td-brand-color-7)`、成功/警告/错误/文字/边框/页面底同 8.0 约定第 2 条）；
- 状态色语义固定（成功=success、进行中=brand、警告=warning、失败/过期=error、历史=default）；
- 列表页沿用 `views/system/user/index.vue` 范式：`t-card :bordered="false"` 筛选卡 + 表格卡 + 工具栏「共 N 条」+ 空态三段式（图标 + 主文案 + 引导按钮）；
- 破坏性操作全部 `DialogPlugin.confirm` 且文案带数字（`删除选中的 12 个文件`、`删除存储配置「…」`）。

## 6. 发现的缺陷（**未在本卡修**）

### 6.1 后端

1. **定时发送无字段**：`NoticeSendReq` 没有 `scheduled_at`，`notice::dispatch` 也不支持延时。PRD N3 要求「立即/定时」，前端已实现定时选择器与 E-NTC-07 校验，但提交会被前端拦下并提示「后端尚未支持」（`views/business/notice/send/index.vue` 的 `SYSTEM_SCHEDULE_UNSUPPORTED`）。
2. **权限码文档与实现不一致**：架构文档 §3.8 写 `business:impexp:import:download-error`，A3 后端注解实际是 `business:impexp:import:download`。前端与种子**以后端注解为准**；建议 A 批次收尾时统一文档。
3. **A3 handler 缺 cancel/retry/export 系列路由**（本地 A3 快照里只有 def/list、import/{template,upload,page,detail,errors,errors/file,confirm}、export/sync）。前端已按架构 §3.8 契约实现 `import/{id}/cancel|retry`、`export/{page,{id},download,cancel}`，若 A4 未补则这些按钮会 404。
4. **PRD 第七章菜单树的权限码与后端注解不一致**（如 PRD 用 `business:storage:backend:list`，后端用 `business:storage:config:list`）。种子按后端注解落地。
5. **`GET /notice/sse` 的鉴权方式未定**：`EventSource` 不能带 `Authorization` 头，前端按 `?token=` 订阅并在失败时降级为 30 s 轮询未读数；A6 若只接受 Header，前端会自动落到轮询（角标仍会更新，只是实时性下降）。

### 6.2 前端（既有、非本卡引入）

6. `BasicLayout.vue` 的 `<transition name="page-fade" mode="out-in">` 依赖 `requestAnimationFrame`，在**后台标签页**（`document.hidden === true`）里 rAF 不推进，路由切换会卡在 `page-fade-leave-from/leave-active`（DOM 实测：URL 变了但视图不换）。真实可见标签页不受影响；如需强健，可改为按 `visibilitychange` 跳过动画。

## 7. 未覆盖 / 明确不做

- 未改后端逻辑（红线），未引入新前端依赖（`package.json` 未变）；
- 按钮级鉴权用 `v-privilege`（项目既有指令），隐藏方式为 `display:none`，与现有页面一致；
- 文件管理页无「编辑」（后端无更新接口，只有 `rebind`），列表页如实不提供编辑入口；
- 「导入向导」的校验报告来自后端 `PrecheckReport`，前端只做展示与截断提示，不在前端重算。

## 8. 集成状态（HEAD 与 origin/main 核对）

本机无法访问代码仓库机，`ssh` 实测：

```
$ ssh -o BatchMode=yes root@172.18.10.144 true
root@172.18.10.144: Permission denied (publickey,password).

$ git -C /root/work/cp-pristine2 ls-remote origin
Permission denied, please try again.
root@172.18.10.144: Permission denied (publickey,password).
fatal: Could not read from remote repository.
```

因此本卡的代码提交在**本地快照仓库** `feat/a7-frontend` 分支（基线 `6ea1896`：从 `/root/work/a2/cp` 复制的工作副本，内容是 A1/A2 之后、含 A 批次文档与 palette 的仓库状态），并提供可直接落库的产物：

- `evidence/a7-frontend.bundle` —— `git bundle`，可在有权限的机器上 `git fetch <bundle> feat/a7-frontend`；
- `evidence/a7-frontend.patch` —— `git format-patch` 全量补丁。

**待人工执行（有权限的机器上，1 条命令）**：

```bash
cd /opt/cloudpivot
git fetch /path/to/a7-frontend.bundle feat/a7-frontend:feat/a7-frontend
git merge --ff-only feat/a7-frontend      # 或按项目惯例推 main
git rev-parse HEAD origin/main            # 贴出两者相等
```

基线差异提示：本卡基线的 `backend/migrations/business/` 只到 V0040 / V0048（A3–A6 的 V0041–V0045、V0049 不在快照内），因此**若上游 main 已有 V0041–V0045，请确认版本号不冲突**：本卡新增的是 V0046 / V0047，与架构文档 §5.2 的预留号一致，与 A5 实际落地的 V0043/V0044/V0045/V0049 不冲突。
