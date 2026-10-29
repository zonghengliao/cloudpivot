# T7a 甘特拖拽改期 · 交付与验收

- 任务：`t_52ce6d07`（`【任务管理·T7a】甘特拖拽改期（必须重跑依赖校验+失败回弹+点名冲突）`）
- 依据：`docs/云枢设计修正补丁-20261029.md` **修正 10**（拖拽改期必须重跑依赖校验）、
  `docs/批次C+-工作任务管理-核心设计.md` §四 / §五、T4 的只读甘特与依赖表
- 功能提交：`362dce0` — `feat(task): T7a 甘特拖拽改期 —— PATCH /schedule 重跑四类校验 + 前端乐观更新/失败回弹 + V0114`
  （**11 files changed, 2197 insertions(+), 53 deletions(-)**：后端 7（含 2 个迁移）+ 前端 2 接口/页面，
  只碰 `business/task/`、`business/changelog/diff.rs`、`shared/error_code.rs` 与两个新迁移文件）
- 真机缺陷修复：`1e7ba15` — `fix(task): T7a 甘特拖拽两个真机缺陷 —— transition 声明非法导致回弹瞬移 + 拖动中条形越出时间窗`
  （只改 `frontend/src/views/business/task/gantt.vue`；生产 dist 就是用这个提交编的）
- 文档 + 复现脚本提交：本文件所在提交
- 生产二进制 `sha256`：`e3f7ff4bb16e0d7f2a1f88606c2f98308717d3d00442cf2e7bcfb24c4882f214`
- 生产前端：`index.html` = `9f3c6f85e4a7acbab5340698ff352f9b49b73477f30a5852a608a55c55564043`，
  入口 chunk `index-BQ7HsbTT.js`，甘特 chunk `gantt-CRuYXmdD.js`（内含 `transition: x 0.28s, width 0.28s` 签名）

## 一、交付文件

```
backend/src/business/task/schedule.rs                       新增：PATCH /business/task/{id}/schedule
                                                            四类校验（纯逻辑可单测）+ 14 条单测
backend/src/business/task/model.rs                          +排期快照 / 对端查询（前置·后继·父·子）
                                                            / update_schedule / COMMENT_TYPE_SCHEDULE
backend/src/business/task/dto.rs                            +TaskScheduleReq / ScheduleMode / TaskScheduleRes
backend/src/business/task/mod.rs                            +pub mod schedule;
backend/src/business/task/handler.rs                        三个 helper 提升为 pub(crate)（current_user /
                                                            param_invalid / display_name）
backend/src/shared/error_code.rs                            +63110 / 63111 / 63112，进码表自检数组
backend/src/business/changelog/diff.rs                      BIZ_TASK 白名单 + start_date / due_date
backend/migrations/business/V0114__task_schedule_comment_type.{up,down}.sql
                                                            comment_type 扩到 7 值（新增 SCHEDULE）
frontend/src/views/business/task/gantt.vue                  拖拽改期（平移 / 左边缘 / 右边缘 + 浮动标签
                                                            + 乐观更新 + 过渡回弹 + 禁用态 + 今天按钮）
frontend/src/api/business/task-api.ts                       +scheduleTask / TaskScheduleReq / TASK_ERROR_CODE.SCHEDULE_*
scripts/verify/t7a_schedule_accept.py                       后端 14 组断言（真机 + 贴库值）
scripts/verify/t7a_prod_fixture.py                          生产真机夹具（5 条任务 + 依赖 + 终态）
scripts/verify/t7a_make_done.py                             补一条 DONE 任务（验「已完成不可拖」）
scripts/verify/t7a_instance.sh                              验收实例启停（独立库 + 18086 端口）
```

> 未新增任何依赖（`Cargo.toml` / `package.json` 零改动）；拖拽用**原生 pointer 事件**，
> 不用 HTML5 拖拽（HTML5 拖拽只能给「拖到哪一格」的语义，给不了「拖了几个像素 → 平移几天」）。

## 二、后端验收（逐条真机，贴完整响应）

环境：`/var/tmp/t7a-run` 独立实例 `127.0.0.1:18086` + 独立库 `cloudpivot_t7a`（空库起，
迁移自动跑到 V0114），admin 首登改密后走真实登录。脚本 `scripts/verify/t7a_schedule_accept.py`
（`T7A_ACCEPT_OK`，RC=0）。

### 0. 无令牌 → 401

```
PATCH /api/business/task/{id}/schedule（无 Authorization）→ HTTP 401
{"code": 401, "msg": "未授权访问，请先登录"}
```

### 1. 拖平移 3 天 → 起止日都 +3（贴库值）

```
PATCH {"shift_days": 3} → HTTP 200
{"code":0,"data":{"task_no":"TSK-202610-000002","old_start_date":"2026-11-01","old_due_date":"2026-11-05",
 "start_date":"2026-11-04","due_date":"2026-11-08","shift_days":3,"changelog_written":true}}
库值 before=['2026-11-01','2026-11-05'] after=['2026-11-04','2026-11-08']
```

### 2. 拖到前置未完成之前 → **400 + 63110**（点名 task_no + 标题 + 截止日）

```
PATCH {"start_date":"2026-10-28"} → HTTP 400
{"code":63110,"msg":"改期违反依赖：前置 TSK-202610-000001「T7a验收-前置（数据库设计）」（截止 2026-10-30）尚未完成（本次改期后开始于 2026-10-28）"}
库值（必须没变）：['2026-11-04','2026-11-08']
```

### 3. 拖到后继开始之后 → **400 + 63111**（点名后继）

```
PATCH {"due_date":"2026-11-12"} → HTTP 400
{"code":63111,"msg":"改期违反依赖：后继 TSK-202610-000003「T7a验收-后继（接口联调）」计划 2026-11-10 开始（本次改期后截止于 2026-11-12）"}
库值（必须没变）：['2026-11-04','2026-11-08']
```

### 4. 左边缘只改 `start_date` / 右边缘只改 `due_date`

```
PATCH {"start_date":"2026-11-05"} → 200；库值 ['2026-11-05','2026-11-08']（截止日未动）
PATCH {"due_date":"2026-11-09"}   → 200；库值 ['2026-11-05','2026-11-09']（开始日未动）
```

### 5. `start_date > due_date` → 400

```
PATCH {"start_date":"2026-12-01"} → HTTP 400
{"code":63004,"msg":"日期非法：计划开始日 2026-12-01 不能晚于截止日 2026-11-09（本次改期目标 2026-12-01 ~ 2026-11-09）"}
库值（必须没变）：['2026-11-05','2026-11-09']
```

### 6. 子任务拖出父范围 → **400 + 63112**

```
PATCH child {"start_date":"2026-10-28"} → HTTP 400
{"code":63112,"msg":"子任务日期必须在父任务范围内：父任务 TSK-202610-000004「T7a验收-父任务」（2026-11-01 ~ 2026-11-30），本次改期 2026-10-28 ~ 2026-11-10：开始日 2026-10-28 早于父任务开始日 2026-11-01"}
库值（必须没变）：['2026-11-05','2026-11-10']
```

**反向（本卡不级联）**：父任务整体 +10 天会把子任务甩出区间 → 同样 63112，并点名子任务：

```
PATCH parent {"shift_days":10} → HTTP 400
{"code":63112,"msg":"子任务日期必须在父任务范围内：本次改期后父任务为 2026-11-11 ~ 2026-12-10（本版不级联改子任务），会被甩出区间的有 子任务 TSK-202610-000010「T7a验收-子任务」（2026-11-05 ~ 2026-11-10），请先调整子任务日期"}
```

### 7. 改期成功 → changelog + `SCHEDULE` 评论（贴库值）

```
biz_task_comment：
  SCHEDULE|改期：2026-11-01 ~ 2026-11-05 → 2026-11-04 ~ 2026-11-08（整体平移 +3 天）
  SCHEDULE|改期：2026-11-04 ~ 2026-11-08 → 2026-11-05 ~ 2026-11-08（手动指定日期）
  SCHEDULE|改期：2026-11-05 ~ 2026-11-08 → 2026-11-05 ~ 2026-11-09（手动指定日期）

sys_change_log（action=UPDATE，label=甘特拖拽改期，a=新值 / b=旧值）：
  changed_fields={due_date,start_date}
  {"fields":{"due_date":{"a":"2026-11-08","b":"2026-11-05"},"start_date":{"a":"2026-11-04","b":"2026-11-01"}}}
```

> 本项目 `sys_change_log.diff` 的 `a` 是**新值**、`b` 是**旧值**（`diff.rs::build` 里
> `b ← before`、`a ← after`），与常见直觉相反，核对审计时别读反。

### 8. 请求形态 / 边界

```
{"shift_days":3,"start_date":"2026-11-05"} → 400 10001 shift_days 与 start_date / due_date 只能二选一
{}                                          → 400 10001 请求体必须给 shift_days 或 start_date / due_date 之一
{"shift_days":0}                            → 400 10001 shift_days = 0 等于没改期
前置做完（DONE）后再改 start=2026-10-28     → 200（63110 不再拦，前置已解除）
```

## 三、前端验收（生产真机，DOM 级证据）

环境：`http://172.18.10.144/task/gantt`（nginx root = `/opt/cloudpivot/frontend/dist`，
dist 就是本次提交编出来的）；登录态用**真实登录拿到的令牌**注入
`localStorage.x_access_token`（口令不进浏览器，一次性 `t7a-token.json` 用完即删）；
拖拽用 **CDP `Input.dispatchMouseEvent`**（真实指针事件，不是合成 DOM 事件）。

### 8. 拖动时日期标签实时跟随

```
BEFORE   084 attrX=460  label=null
DRAG+3   084 attrX=550  label="整体平移：2026-11-07 ~ 2026-11-11（+3 天）"  label.left=636px
         （+1 天那一帧 label=…（+1 天）、left=486px —— 标签跟着指针走，不跟条形走）
```

### 9. 请求失败 → 条形回弹（逐帧插值证据）

拖 084 左边缘 −6 天（开始日 11-07 → 11-01，早于前置截止 11-03）：

```
DRAGGING  attrX=370  attrW=328  style="transition: none;"  label="改开始日：2026-11-01 ~ 2026-11-11（-6 天）"
松手后 MutationObserver：
  t=4ms   style: "transition: none;" → "transition: x 0.28s, width 0.28s;"
  t=13ms  x: 370 → 550      （回弹开始：370 是乐观位置，550 是原位置）
  t=13ms  width: 328 → 148
30ms 采样（computed style 的 x 是插值中的值）：
  t=31  attrX=550 computedX=377.632px
  t=61  attrX=550 computedX=392.720px
  t=91  attrX=550 computedX=438.754px
  t=120 attrX=550 computedX=481.006px
  t=151 attrX=550 computedX=509.976px
  t=180 attrX=550 computedX=528.751px
  t=210 attrX=550 computedX=535.334px
  t=240 attrX=550 computedX=544.277px
  t=271 attrX=550 computedX=548.819px
  t=300 attrX=550 computedX=550px          ← 0.28s 平滑回弹结束
toast：改期违反依赖：前置 TSK-202610-000083「T7a真机-前置（数据库设计）」（截止 2026-11-03）尚未完成（本次改期后开始于 2026-11-01）
```

> 库里**没有**留下假状态：回弹后 `biz_task` 仍是 `2026-11-07 ~ 2026-11-11`（下一节 §6.2 复核）。
> 一个坑记在这里：`<rect :style="'x 0.28s ease'">` 是**非法声明**（缺属性名），会被浏览器整条丢掉
> （`style` 属性为空、回弹瞬移）—— 必须写 `transition: x 0.28s ease, width 0.28s ease`。

### 10. 依赖线在拖动时跟随

```
BEFORE    link(前置→084) ends at 456 ；link(084→后继) starts at 608
DRAG+3    link(前置→084) ends at 546 ；link(084→后继) starts at 698
```

（两条线都从同一个 `barGeom()` 推导，没有第二套「拖拽专用」坐标。）

### 11. `CANCELLED` / `DONE` 不可拖（cursor + 拖拽无响应 + 说明原因）

```
087（CANCELLED） cursor=not-allowed  class=…--locked  handles=0  attrX 400→400（拖动无位移）
                 toast：「已取消的任务不可拖拽改期」
088（DONE）      cursor=not-allowed  class=…--locked  handles=0  attrX 430→430  label 不出现
                 toast：「已完成的任务不可拖拽改期」
```

### 12. 无档期任务不可拖 + 提示

```
086（只有截止日 2026-11-08，没有计划开始）cursor=not-allowed handles=0 attrX 580→580
toast：「请先设置起止日期（任务需要「计划开始」和「截止日期」都有值才能拖拽改期）」
```

### 其他交互

| 项 | 实测 |
|---|---|
| 右边缘拖（任务开始日**在时间窗之前**） | attrX 恒为 280（裁到窗口左边），宽度 118→178（+2 天）；label「改截止日：2026-10-25 ~ 2026-11-03（+2 天）」；toast 确认开始日未变 |
| 「今天」按钮 | 点「下一屏」→ 窗口 `2026-11-28 ~ 2026-12-27`（0 条）→ 点「今天」→ 回到 `2026-10-29 ~ 2026-11-27`（10 条），`scrollLeft=0`（今天本来就在窗口左端，滚动目标是 0，属预期） |
| 点条形（没拖） | 仍打开详情抽屉（`suppressClick` 只在真的拖动过时生效） |

## 四、其它检查

| 检查 | 结果 |
|---|---|
| `cargo test` | **658 passed / 0 failed**（T6b 基线 644 + 本卡 14 条；`business::task::schedule::tests` 全绿） |
| `cargo clippy --all-targets -- -D warnings` | RC=0（零告警） |
| `scripts/check.sh` | RC=0，「全部检查通过」（fmt / clippy / check / typecheck / 批次B 闸①=0 闸②=0 / C7 G1–G6 / 色板同步） |
| `pnpm typecheck && pnpm build` | vue-tsc 0 error；vite build ✓ |
| 新增单测覆盖 | 形态（二选一/0/上限）、解析（平移/只改一边/无基准）、①日期合法性（倒置/下界/边界）、②前置（点名/已解除/无截止日）、③后继（点名/相等放行/取消不参与）、④父子（子越界/父甩子）、顺序、编码往返、文案签名 |

## 五、§6.2 部署验收（生产）

| 步骤 | 结果 |
|---|---|
| 二进制就位 | `/opt/cloudpivot/backend/target/release/cloudpivot` ← 本次提交编译；`sha256=e3f7ff4b…82f214`（替换正在运行的二进制用**同盘 rename**，`cp` 会 `Text file busy`） |
| 迁移 | 生产库 `cloudpivot` 的 `ck_biz_task_comment_type` 已含 `SCHEDULE`（7 值）；`V0114` 自检 NOTICE「comment_type 7 种类型（新增 SCHEDULE）」 |
| 前端 dist | `index.html` = `9f3c6f85…564043`，甘特 chunk `gantt-CRuYXmdD.js` 内含 `transition: x 0.28s, width 0.28s` 签名 |
| 服务 | `systemctl restart cloudpivot` → active；`/health` = 200；`http://172.18.10.144/` = 200 |
| 接口 401 | 无令牌 `PATCH /api/business/task/{id}/schedule` → **401** `{"code":401,"msg":"未授权访问，请先登录"}` |
| 真实登录 | `POST /api/system/auth/login`（admin，生产口令来自 `/etc/cloudpivot/cloudpivot.env`）→ 200 + 461 字符 token |
| 真机拖拽成功 + 库值变更 | 生产页面上拖 `TSK-202610-000084` 中部 +3 天 → toast「已改期：2026-11-04 ~ 2026-11-08 → 2026-11-07 ~ 2026-11-11」；库 `start_date=2026-11-07 due_date=2026-11-11`；`SCHEDULE` 评论 + `sys_change_log`（`甘特拖拽改期`）各 1 行 |
| 失败不落库 | 63110 的那次拖动回弹后，库值仍是 `2026-11-07 ~ 2026-11-11`（没有任何半写状态） |
| 清理后复核 | 6 条 `T7a真机-*` 全部软删（`left_live=0`），生产剩余 5 条真实任务；一次性令牌文件已删；验收实例 18086 已停、验收库 `cloudpivot_t7a` 已删 |

## 六、偏差与已知边界（诚实说明）

1. **错误码复用**：卡面只写「任一失败 400」，所以「形态非法 / 日期非法」复用既有
   `10001`（参数非法）与 `63004`（字段非法），只有三类冲突按卡面固定成
   `63110 / 63111 / 63112`。前端一律直接展示后端 `msg`（已点名冲突对象），不另拼文案。
2. **后继判定只有 `CANCELLED` 被排除**（`DONE` 仍参与）：已完成的后继仍记录着
   「它当时从哪天开始」，前置拖到它之后会让历史自相矛盾；`CANCELLED` 已无排期意义。
3. **父任务改期也做反向校验**（卡面只写了「子任务不许出父范围」）：本卡不级联，
   如果父任务把已有子任务甩出区间，直接 `400 + 63112` 并点名子任务 —— 否则父子区间立刻不一致。
4. **「今天」按钮在默认窗口下不产生横向滚动**：默认窗口以今天为起点，今天就在最左端，
   滚动目标是 0；窗口包含今天但今天不在左端时才会把今天滚到视口中间（代码在
   `scrollToToday()`，默认入口下这条分支没有 UI 可达路径）。
5. **前端不引入拖拽库**：`package.json` 零改动，pointer 事件 + 天数换算（30px/天、12px/天）
   全部自己算；几何量（偏移天数 / 长度天数）仍由后端给，前端只乘列宽。
6. **乐观更新的中间态极短**：后端拒绝只需 13–18ms，`MutationObserver` 抓到的序列是
   「拖动中 370 →（松手，乐观值不变）→ 13ms 后 550 → 0.28s 插值到 550」。
   这也说明「先移动条形、再请求」的顺序是真的（请求前条形已在目标位）。
7. **部署与 git 合并的先后**：本卡开工时主仓被并行卡（`business/aiagent/`，L1）的未提交改动
   占着，其中 `shared/error_code.rs` 与本卡同文件 → `git merge --ff-only` 被 git 拒绝。
   因此部署用的是**从本卡提交 `362dce0` 编出的二进制与 dist**（内容与该 commit 一致），
   合并等并行卡提交后再执行（见 §七 的收尾核对）。生产库的 `V0114` 也因此先手工 `psql -f`
   应用（DDL 幂等：`DROP CONSTRAINT IF EXISTS` + `ADD`），合并后应用启动会再跑一遍并写入
   `schema_migrations` 的 114 行。

## 七、收尾核对

```
本卡功能提交        362dce0
分支                feat/t7a-gantt-schedule
三方一致            HEAD == origin/main == bare main（收尾时按 §6 的说明合并后核对；
                    本文件自身提交会推高 HEAD，故此处不写死最终哈希）
主仓工作区          仅并行卡（L1 · business/aiagent）的未提交改动，本卡未碰
```
