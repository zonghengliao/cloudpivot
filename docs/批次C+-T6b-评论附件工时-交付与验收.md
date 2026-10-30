# 批次 C+ · T6b：补齐 T1「建表未实现」的三张表（评论 / 附件 / 工时）

> 卡号：`t_3fe560e1` ｜ 交付日期：2026-10-29 ｜ 角色：manong（开发团队）
> 依据：卡面「一/二/三/四/五」五段 + `docs/批次C+-工作任务管理-核心设计.md` §八/§九/§十三
> 迁移：`backend/migrations/business/V0113__task_comment_worklog_permissions.{up,down}.sql`

---

## 一、交付了什么（一句话）

T1 建了 7 张表，只实现了 3 张；本卡把 **`biz_task_comment` / `biz_task_attachment` / `biz_task_worklog`** 三张「有表没功能」的表补成可用协作面：**8 个接口 + 三个 UI 区块 + 1 个迁移 + 8 条单测**。

| 方法 | 路径 | 中间件闸 | 说明 |
|---|---|---|---|
| GET | `/business/task/{id}/comments` | `business:task:read` | 评论列表（时间正序、分页、带 `readonly` / `can_delete`） |
| DELETE | `/business/task/{id}/comments/{comment_id}` | `business:task:read` | 软删（作者 / `business:task:comment:delete`；只读类型一律 403） |
| GET | `/business/task/{id}/attachments` | `business:task:read` | 附件列表（来源 HUMAN/AI、上传者、`can_delete`） |
| DELETE | `/business/task/{id}/attachments/{att_id}` | `business:task:read` | 软删绑定行 + `sys_file` 标记删除 |
| GET | `/business/task/{id}/attachments/{att_id}/download` | `business:task:read` | 下载（**先校验任务可见性**，再复用 A 批次的 `download_response`） |
| GET | `/business/task/{id}/worklogs` | `business:task:read` | 工时列表 + 明细合计 + 任务实际工时 |
| POST | `/business/task/{id}/worklogs` | `business:task:read` | 登记工时（服务端 `SUM(hours)` 汇总） |
| DELETE | `/business/task/{id}/worklogs/{wl_id}` | `business:task:read` | 删除工时（本人 / `business:task:worklog`），同事务重算 |

「写」的两个入口（`POST /comment`、`POST /attachment`）沿用 T5b 已建的实现（它们要同时接受 AI 任务令牌，走 `#[nik::no_auth]` 白名单），本卡只补了**跨人删除、列表、下载、工时**四类能力，以及 @ 提及的强制通知。

### 代码落点

| 文件 | 变更 |
|---|---|
| `backend/src/business/task/collab.rs` | **新增**（约 1350 行）：8 个 handler + 子表查询 + 4 条纯判定 + 8 条单测 |
| `backend/src/business/task/dto.rs` | 新增 3 个权限码常量 + 12 个 DTO |
| `backend/src/business/task/handler.rs` | `add_comment` 接 @ 强制通知；`upload_attachment` 加 50 个上限；`parse_uuid_field` / `not_found` 提为 `pub(crate)` |
| `backend/src/business/task/model.rs` | `bind_*_attachment` 支持「软删后重新上传」的复活路径 |
| `backend/src/business/storage/handler.rs` | `download_response` 提为 `pub(crate)`（出流只留一份实现） |
| `backend/migrations/business/V0113__*.sql` | 附件软删列 + 2 条索引 + 工时区间 CHECK + 3 个权限节点 |
| `backend/migrations/seed/S__task_permissions.sql` | 第 4 段：3 个新权限码只授 SUPER_ADMIN |
| `frontend/src/api/business/task-api.ts` | 评论/附件/工时的类型与 11 个方法（含 Blob 下载、multipart 上传） |
| `frontend/src/views/business/task/detail.vue` | 详情抽屉新增「协作」三 Tab（评论时间线 / 附件拖拽上传 / 工时合计与登记弹窗） |
| `frontend/src/views/business/task/components/MentionSelect.vue` | **新增**：评论 @ 的多人选择器（远程搜索，与 `UserSelect` 单选互不影响） |

---

## 二、六个设计决策（评审重点）

### D1 「人写的评论」在库里叫 `PLAIN`，不叫 `COMMENT`
卡面 §一.6 写的是 `COMMENT`（人写的），但 `V0108` 建的值域与 `V0112` 扩后的 CHECK 都是
`PLAIN / STATUS_CHANGE / PROGRESS_AUTO / ASSIGN / AI_REPORT / AI_PROGRESS`。**以库为准**：`PLAIN` 就是人类评论。
没有为了「跟卡面对齐」去改 CHECK —— 那会让存量 6 类数据全部撞约束，收益只是命名好看。

### D2 只读评论集合 = 4 类（`STATUS_CHANGE`/`ASSIGN`/`PROGRESS_AUTO`/`AI_PROGRESS`），`AI_REPORT` 可删
- 4 类是**机器生成的时间线事实**（谁把状态推到了哪、进度怎么汇总的），删掉等于篡改历史 → 连超管也 **403**；
- `AI_REPORT` 是「AI 写下的一段话」，作者是 `ROBOT`（登不了录），所以留一条管理员可清理的通路。

判定只有一处：`collab::comment_delete_verdict(comment_type, is_author, has_perm)`，且**先判只读**再判作者
（系统评论的作者往往就是操作人本人 —— 先判作者会把只读闸放空）。

### D3 「@ 提及」是强制通知，且**有强证据**
`force = true` 走 `notice::dispatch`；扇出侧 `fanout::respecting_preference` 只在 `force = false` 时才叠加
`notify.inbox` 过滤。验收时**先把被提及人的 `sys_user_preference.notify.inbox` 置为 `false`**，再发评论：
通知照样送达（`sys_notice_receiver` 有该行）—— 这才证明「强制」，否则「偏好本来是开的」也能过。

### D4 附件下载：可见性在前，出流在后（红线）
顺序固定：**任务可见 → 附件属于该任务 → 文件未软删 → 交给 storage 出流**。任何一步失败都是 **404**
（不是 403：403 等于告诉调用者「这个 UUID 存在」，而 UUID v7 是时间有序的，枚举成本比随机 UUID 低得多）。
出流复用 A 批次的 `download_response`（LOCAL 流式 / S3 302 预签名 / `Content-Disposition` / `nosniff` 都在里面），
**没有另造下载通道**。

### D5 工时的 `actual_hours` 只由服务端汇总
登记 / 删除工时的**同一个事务**里跑 `sync_actual_hours`（`biz_task.actual_hours = SUM(未删工时)`）；
请求 DTO 里**没有** `actual_hours` 字段 —— 少一个字段就少一条造假路径（实际工时进成本统计）。
没有记录时写 `NULL` 而不是 0：「还没登记」与「登记了 0」是两件事，详情页靠它决定显示 `-` 还是 `0h`。

### D6 父任务进度**不受**工时影响（卡面 §五 第 15 条的口径说明）
T2/D4 的父任务进度 = **子任务 `progress` × `estimate_hours` 加权**（`state::aggregate_progress`），
工时只回写**本任务**的 `actual_hours`。所以「给有子任务的任务登记工时」不改父任务进度，父任务的
`actual_hours` 也不汇总子任务（否则一个 20 层任务树会被重复累加）。这不是漏实现，是两条口径本来就不同：
**预估工时决定权重，实际工时只做成本统计**。验收第 15 条实测：父 `progress` 前 0 / 后 0，父 `actual_hours` 仍为 `NULL`，子 `actual_hours = 3.0`。

---

## 三、迁移 `V0113`（4 件事）

| # | 内容 | 为什么必须动库 |
|---|---|---|
| ① | `biz_task_attachment.deleted_at` | 卡面要求「删除 = 软删记录 + 存储标记删除」。没这列就只能硬删绑定行，丢掉「谁在什么时候删了哪个附件」的线索（`sys_file.deleted_at` 是文件维度，一个文件可被多个任务引用） |
| ② | `idx_biz_task_comment_task_time`（部分索引，`WHERE deleted_at IS NULL`）/ `idx_biz_task_worklog_task_date` | 评论固定按「任务 + 未删 + 时间正序」取；工时按「任务 + 日期」取并覆盖 `SUM(hours)` |
| ③ | `ck_biz_task_worklog_hours_range CHECK (0.25–24) NOT VALID` | 卡面规则 2 的上限若只在 handler 判，任何直连 SQL / 批量导入都能塞 `hours = 1000`，而 `actual_hours` **由它汇总** —— 一条脏数据污染整个任务。`NOT VALID` = 只约束新写入，存量脏值不阻断部署 |
| ④ | 3 个权限节点 …0772/0773/0774 + 种子第 4 段授权 | `business:task:comment:delete` / `business:task:attachment:delete` / `business:task:worklog` 都是**跨人加项**，默认只授 SUPER_ADMIN（与 V0109 同口径：全员授予 = 没有隔离） |

### 空库 up → down → up 实测（`/root/t6b_mig_test.sh`，全新库 `cloudpivot_t6bmig`）

```
== 迁移完成后的基线（应 col=1 idx=2 ck=1 menu=3 grant=3）
1|2|1|3|3
== 第一次 up（应用后的状态，验幂等）
-- up1 rc=0
   NOTICE: column "deleted_at" ... already exists, skipping
   NOTICE: relation "idx_biz_task_comment_task_time" already exists, skipping
   NOTICE: relation "idx_biz_task_worklog_task_date" already exists, skipping
   NOTICE: V0113 自检通过：附件软删列 + 2 条索引 + 工时区间约束 + 3 个权限节点
1|2|1|3|3
== down
-- down rc=0
0|0|0|0|0          ← 列 / 索引 / 约束 / 菜单 / 授权 全部 0 残留
== 第二次 up
-- up2 rc=0
   NOTICE: V0113 自检通过：附件软删列 + 2 条索引 + 工时区间约束 + 3 个权限节点
1|2|1|3|0          ← 授权 0：见下面的说明
```

> **`grant=0` 是预期**：授权语句在**种子**迁移 `S__task_permissions.sql` 里（版本化迁移先跑、种子后跑，
> 空库上 V0113 执行时 SUPER_ADMIN 还没建 —— 与 V0103/V0109 同一时序约束）。
> 手写 psql 跑 down→up 不会重跑种子；**应用下次启动会重跑全部种子**（`S__*.sql` 是 rerunnable），
> 授权自动回来。生产部署实测：重启后 `menu=3` 且 `schema_migrations` 里 `113` 已应用。

---

## 四、验收：18 项逐条真机实测（全部 PASS）

环境：`cloudpivot_t6bv`（空库全量迁移）+ 验收实例 `127.0.0.1:18083` + 脚本 `/root/t6b_accept.py`。
5 个验收账号：`t6b_owner`（负责人）/ `t6b_other`（协作人）/ `t6b_mention`（被 @，**偏好已关**）/ `t6b_outsider`（无关人，`data_scope=4`）/ `t6b_peer`（全员可见，`data_scope=1`）。

```
[PASS] 1. 发评论 → 列表出现且内容完整 :: total=1 comment_type=PLAIN readonly=False
[PASS] 2. @两个人 → 两人各收到通知（含已关偏好者，证明强制）
       notice=['01a18718-92e5-...'] title=t6b_owner 在任务「T6b 评论附件工时验收任务」里提到了你
       receiver_count=2 receivers=['…5ea9…','…76aa…']
[PASS] 6a. Markdown 原文按设计入库（净化在前端渲染层）:: 库内原文片段=危险内容 <script>alert('xss')</script> 与 <img src=x onerror=aler
[PASS] 4. 删别人的评论（非管理员）→ 403 :: HTTP 403 code=63002 msg=只能删除自己的评论，或持有 business:task:comment:delete 权限
[PASS] 3. 删自己的评论 → 列表消失 + 库里有 deleted_at :: HTTP 200 列表仍有=False deleted_at非空=t
[PASS] 5. 系统类型评论（STATUS_CHANGE）不可删 → 403 :: HTTP 403 code=63002 msg=系统生成的评论（状态变更 / 分派 / 进度汇总 / AI 进度）不可删除 库里仍存在=t
[PASS] 7. 上传 .md → 能下载、内容一致（sha256） :: HTTP 200 source_sha256=3cc2eb17c5ab3ad3… download_sha256=3cc2eb17c5ab3ad3… len=45
[PASS] 8. 上传 .sh → 拒绝（task 策略白名单外 + 可执行硬禁止）:: HTTP 400 code=60004 msg=禁止上传可执行/脚本文件 .sh：该类型在硬禁止黑名单内，任何分类都不放行
[PASS] 9a. 删别人的附件（非管理员）→ 403 :: HTTP 403 code=63002 msg=只能删除自己上传的附件，或持有 business:task:attachment:delete 权限
[PASS] 10. 越权下载：无关用户拿 att_id → 404（校验任务可见性）:: HTTP 404 {"code":63001,"msg":"任务不存在或已删除"}
[PASS] 9. 删附件 → 列表消失 + 存储记录标记删除 :: HTTP 200 列表仍有=False sys_file.deleted_at非空=t
[PASS] 11. 单任务第 51 个附件 → 400 :: 前 50 个上传成功=50 第 51 个 HTTP 400 code=63004 msg=单任务附件最多 50 个，请先删除不再需要的附件
[PASS] 12. 登记 2h + 3.5h → actual_hours = 5.5 :: 库 actual_hours=5.5 接口 sum_hours=5.5 actual_hours=5.5 total=2
[PASS] 13a. 登记 26h → 400 :: HTTP 400 code=63004 msg=单条工时不能超过 24 小时（一个工作日）
[PASS] 13b. work_date 明天 → 400 :: HTTP 400 code=63004 msg=工时日期不能晚于今天（2026-10-29）
[PASS] 13c. 非负责人非协作人（可见但无权限）登记工时 → 403 :: HTTP 403 code=63002 msg=只有任务负责人、协作人，或持有 business:task:worklog 权限的人可以登记工时
[PASS] 14. 删一条 → actual_hours 重算正确（5.5 → 2.0）:: HTTP 200 库 actual_hours=2.0 回执 sum_hours=2.0
[PASS] 15. 有子任务的任务登记工时 → 父任务进度不受影响（actual_hours 只算本任务）
       父 progress 前=0 后=0 父 actual_hours=None 子 actual_hours=3.0

===== 真机验收：PASS 18 / FAIL 0 =====
```

### 第 6 条（Markdown `<script>` 净化）的 **UI 侧**证据

后端按卡面 §一.5 存**原文**（前端渲染时净化），所以「净化」必须在浏览器里验。生产页面实测（详情抽屉 → 评论 Tab）：

```
script 元素数（应为 0）: 0
mdv 内是否含可执行 img: 0
脚本原文是否以文本呈现: True      ← <script>alert('xss-from-comment')</script> 作为文本显示
系统评论样式/删除按钮: {"isSystem":true,"hasDelete":false,"opacity":"0.72","borderStyle":"dashed"}
```

### 前端三个区块（真实登录、生产构建）

| Tab | 实测内容（DOM 文本） |
|---|---|
| 评论 4 | `超级管理员 2026-10-29 18:00:38 删除` / Markdown 渲染出列表与 `code` / `状态变更` 标签的系统评论（淡化 + 无删除按钮）/ 通过抽屉发表框**新发一条**成功（计数 3→4） |
| 附件 1 | `拖拽文件到此处，或点击选择（单个按策略上限；最多 50 个）` / `已上传 1 / 50` / `t6b-deploy.md 人工 44 B 超级管理员 下载 删除` |
| 工时 3 | `明细合计 6.5 小时 / 任务实际工时 6.5` / 明细行（日期、填报人、小时数、说明、删除）/ 通过「登记工时」弹窗**提交 1h** 成功（5.5 → 6.5，实际工时同步重算） |

---

## 五、其它验收

| 项 | 结果 |
|---|---|
| `cargo test` | **644 passed / 0 failed**（T5b 基线 636 + 本卡 8 条：评论权限矩阵 / @ 去重剔除作者 / 摘要按字符截断 / 工时值域 / 工时日期 / 日期解析 / 两份配额 / `mentions` 脏值容错） |
| `scripts/check.sh` | **全部检查通过**（fmt / clippy `-D warnings` / check / typecheck / 批次B 闸①=0 闸②=0 / C7 G1–G6 / 色板一致性） |
| `pnpm typecheck` | 0 error |
| `pnpm build` | 通过（入口 `index-Cxm8CS2X.js`） |
| §6.2 部署验收 | `systemctl restart cloudpivot` → 生产库 `col=1 idx=2 ck=1 menu=3 113`；三个新接口无令牌全部 **401**；真实登录后三个区块可用（见上表） |

生产部署指纹：

```
HEAD          = 见下方「收尾」提交号
二进制 sha256 = 2064d1caf3f44f355062093b0f333b34da0f816068646784daaa28c01835ba1e
生产迁移      = version 113 已应用；附件软删列=1 / 索引=2 / 工时CHECK=1 / 新权限节点=3
无令牌探测    = /comments 401 · /attachments 401 · /worklogs 401
```

验收数据已清理（生产上的演示任务软删、附件软删、`frontend/dist/t6b-token.json` 一次性令牌文件已删除）；
验收实例 `18083` 与验收库 `cloudpivot_t6bv` 在收尾时停用并删除。

---

## 六、已知边界（诚实说明）

1. **评论接口不接受 `mentions` 以外的人群语义**：`mentioned_user_ids`（卡面写法）在代码里从来不存在，
   实际字段是 `mentions`（T5b 定的，与列同名）。前端按 `mentions` 传。
2. **工时是硬删**（`biz_task_worklog` 没有 `deleted_at`，日志类表约定硬删 + 无 `updated_at`）：
   删掉的工时只留操作日志，不做软删审计。卡面未要求软删。
3. **附件删除的物理回收是异步的**：绑定行立刻消失、`sys_file` 立刻标记删除，物理对象由 storage 的
   清理任务（每日 04:30，软删 7 天后）回收 —— 复用 A 批次策略，本卡不另造回收通道。
4. **AI 产出附件（`source=AI`）的删除权限**：与人类上传同口径（上传者本人 / `business:task:attachment:delete`）。
   AI 上传时 `create_by` 是 AI 账号，人删不了自己没传的 AI 附件 —— 需要该权限码或超管，符合「跨人操作要授权」。
5. **`t-tabs` 里的三个区块共用一个抽屉滚动区**：Tab 切换不重置滚动位置（TDesign 行为），
   长评论列表切 Tab 后会停在上一个位置。属交互细节，不影响功能，留待 T6 详情页定稿时统一处理。
