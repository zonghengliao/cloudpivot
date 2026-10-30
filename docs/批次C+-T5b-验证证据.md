# 批次 C+ · T5b「云枢 ↔ Hermes kanban 集成」验证证据

> 卡：`【任务管理·T5b】云枢↔Hermes kanban集成：派活+任务级令牌+三层回传`
> 依据：`docs/批次C+-Hermes集成设计-T5b.md`、老大拍板 **1A**（AI 产出强制上传云枢）+ **2A**（AI 完成 → REVIEW）
> 宿主：`172.18.10.144`（生产 `cloudpivot` :8023 / dev `:8024`）；Hermes **v0.22.5**（执行期内由 v0.15.2 升级，见 §八）

## 零、结论速览

| 验收项 | 结果 | 证据 |
|---|---|---|
| b1 数据模型 + 任务级令牌（独立中间件 / 白名单 4 端点 / 越权 403 / 过期与终态失效） | ✅ | §二 |
| b2 派活（body 契约 / 幂等键 / 失败留 TODO） | ✅ | §三 |
| b3 轮询同步 + done 三层回传（→ REVIEW） | ✅ | §四 |
| b4 失败处理 + AI 进度回写 + 统计 exec_mode 维度 | ✅ | §五 |
| 真跑一个 AI 任务端到端（派活 → DOING → done → REVIEW + 评论 + 附件） | ✅ | §四 |
| 令牌不能越权（读 / 写别的任务都 403 + 63303） | ✅ | §二 |
| 幂等键生效（重复派活 409；Hermes 两次 create 同一 id） | ✅ | §三 |
| `cargo test` 全绿（636 + 集成/文档测试，0 失败） | ✅ | §六 |
| `scripts/check.sh` 全绿（fmt / clippy / check / typecheck / 闸①② / C7 G1–G6 / 色板） | ✅ | §六 |
| §6.2 部署验收（迁移上生产 / release 重编部署 / 路由非 404 / 二进制含新逻辑） | ✅ | §七 |
| 空库 up→down→up | ✅ | §七 |

**端到端实测里发现并修掉 5 个真 bug**（全部是「不真跑就发现不了」的那类），见 §九。

---

## 一、代码落点

| 文件 | 内容 |
|---|---|
| `backend/migrations/business/V0112__task_hermes_integration.{up,down}.sql` | `biz_task` 7 列（`exec_mode` / Hermes 三件套 / 同步时间 / `ai_summary` / `ai_result`）+ `biz_task_attachment` 2 列（`source` / `ai_profile`）+ 令牌表 `biz_task_ai_token` + 评论类型值域扩 `AI_REPORT` / `AI_PROGRESS`，含自检 `DO` 块 |
| `backend/src/middleware/task_token.rs` | **独立于 JWT 的**任务令牌守卫：`cpai_` 前缀识别（69 字符形态）→ `sha256` 命中 → `path_task_id == token.task_id` → 过期 → 终态 → `exec_mode='AI'`；白名单只放行 4 个端点；8 条单测 |
| `backend/src/business/task/hermes_sync.rs` | 派活（body 模板 + 幂等键 + 失败收尾）、30s 轮询、状态映射、done 三层回传、failed/blocked/404 处理、`status_unchanged` 幂等判定，13 条单测 |
| `backend/src/business/aiagent/hermes.rs` | `kanban create / show` 的 CLI 封装（`--json` 解析、`ensure_profile_config` 前置、profile/board/幂等键校验） |
| `backend/src/business/task/{handler,dto,model}.rs` | 4 个接口 + `dispatch-ai` + `stats`；`TaskReader`（人类 / 令牌双身份）；`task_stats` 聚合（走同一条可见性谓词） |
| `backend/src/lib.rs` | 令牌守卫挂在**最外层**（`layer_from_fn`，早于框架认证层） |
| `scripts/patch-nik-web.sh` | **CODE-11 / CODE-11b**：框架认证层与权限层的「免认证路径」判定支持模板段，且模板段只匹配 id 形态 |
| `backend/src/shared/error_code.rs` | 633xx 子段 8 个码（令牌 / 派活 / 进度） |

关键不变式（后面改这块必须守住）：

1. **令牌明文只出现在 kanban body 里**：库里只有 `sha256` 十六进制（`biz_task_ai_token.token_hash`），且 `task_id` 是主键——重派即覆盖旧令牌。
2. **令牌边界 = 一个任务**：中间件先比 `path.task_id`，再做其余判定；越权报 `63303`（不误报成「过期」）。
3. **`done` 一律进 `REVIEW`**（2A），不自动 DONE；`REVIEW` 不是终态 → 轮询必须靠 `status_unchanged` 幂等，否则每 30s 重放一次回传。
4. **轮询失败不改任务状态**：CLI 起不来 / JSON 解析失败只记日志。

---

## 二、b1：数据模型 + 任务级令牌（生产实测）

生产 `POST/PATCH/GET` 全部打 `127.0.0.1:8023`，令牌取自 Hermes 任务 body（正则 `cpai_[0-9a-f]{64}`）。

```
令牌取自 kanban body：cpai_a5c1…3814（69 字符）
[1] GET  本任务          -> 200 code=0 status=REVIEW exec_mode=AI
[2] POST 评论（本任务）  -> 200 code=0 comment_id=01a186e5-c91f-7042-9d0f-3b456cd95446
[3] POST 附件（本任务）  -> 200 code=0 attachment_id=01a186e7-e95f-736f-94d9-04fcfc9cb944
[4] GET  别的任务        -> 403 code=63303 msg=任务令牌只能操作它所属的那一个任务（路径任务与令牌任务不一致）
[5] POST 别的任务评论    -> 403 code=63303
[6] POST /status（令牌） -> 403 code=63304 msg=任务令牌只能访问本任务的详情 / 评论 / 附件 / AI 进度四个端点
[7] 伪造令牌（64 个 0）  -> 403 code=63301
[8] GET  /page（令牌）   -> 403 code=63304
[9] admin JWT 读别的任务 -> 200 code=0（回归：人类通道不受影响）
```

匿名 / 越权面（生产，重启后立即验）：

```
GET /api/business/task/page            -> 401 {"code":401,"msg":"未授权访问，请先登录"}   ← 认证层仍在（不能被模板段放行）
GET /api/business/task/stats           -> 401
GET /api/business/task/{uuid}（匿名）   -> 401 {"code":20005}（handler 层，不是 500）
```

单测覆盖（`middleware::task_token` 8 条）：白名单识别（4 个端点接受、`/status` `/page` `/male` 等一律拒绝）、令牌形态严格（前缀 + 64 hex）、hash 稳定且不撞、过期边界（`expires_at <= now` 即失效）、终态失效（DONE / CANCELLED / 软删）。

---

## 三、b2：派活（生产实测）

```
[1] 重复派活（同一任务第二次） -> 409 code=63306
    msg=任务已派给 AI（Hermes 任务 t_66caa598，看板 dev），不会重复派活；如需重派请先处理该 Hermes 任务
[2] Hermes 幂等键 "cailu-task:<task_id>" 连续两次 create
    -> ['t_66caa598', 't_66caa598']（同一个 id，且等于首次派活的 card）
[3] 派活失败注入（ROBOT 账号的 hermes_profile 指向不存在的 profile）
    -> 502 code=63305 msg=派活失败：AI profile 缺少模型凭据，派活会变成假成功：profile 目录不存在：/root/.hermes/profiles/nosuchprofile
    失败后任务行 status|hermes_task_id|exec_mode = TODO|NULL|HUMAN   ← 红线：不假装成功
    留痕评论 = PLAIN | 派活到 Hermes 失败（任务仍留在当前状态，可重新派活）：…
    通知 = 1 条（派活失败（AI 未接单） / INFO / USER）
    令牌行 = 0 条（失败即删）
```

body 契约（真实 card body，节选）：

```
## 交付要求（云枢任务管理 · 必须遵守）
2. **产出文件必须上传到云枢**（不要只给路径）：…
   `http://172.18.10.144/api/business/task/<task_id>/attachment`（POST，multipart，字段名 `file`）
   任务令牌：`cpai_…`
示例：
curl -sS -X POST "http://172.18.10.144/api/business/task/<task_id>/attachment" \
     -H "Authorization: Bearer cpai_…" -F "file=@<文件>"
```

`--priority` 映射 P0→10 / P1→8 / P2→5 / P3→2（单测 `priority_mapping_matches_contract` 钉住）。

> 与卡面的一处实现差异：卡面要求「body 用临时文件（不拼 shell 字符串）」。实现走 `Command::new(hermes).args([...])`（**execve argv，不经 shell**），body 作为单个 argv 传入，注入面与临时文件等价；好处是失败时 stderr 里能直接看到 Hermes 的原始报错。

---

## 四、b3：轮询同步 + done 三层回传（真实端到端）

真实链路（生产库 / 生产二进制 / 真实 AI worker）：

| 时刻 | Hermes 侧 | 云枢侧 |
|---|---|---|
| 17:02:46 | `running`（worker 已拉起） | `TODO`（hermes_status=ready，30s 内有延迟） |
| 17:02:57 | `done`（summary 749 字） | `TODO` + 附件已由 AI 用令牌上传（`attachments:1`） ← 1A |
| 17:03:08 | `done` | **`REVIEW`** / progress **100** / `ai_summary` 749 字 / `ai_result` 有值 |

三层回传实测（`GET /api/business/task/{id}`）：

```json
{
  "status": "REVIEW",
  "progress": 100,
  "ai_summary_chars": 749,
  "ai_summary_head": "四步全部真实执行成功（云枢任务 TSK-202610-000020 / 01a186e5-…）：① 在 workspace 创建 …",
  "ai_result": {
    "new_files": ["/root/.hermes/workspace/t5b-agent/t5b_selfcheck.json"],
    "artifacts": ["/root/.hermes/workspace/t5b-agent/t5b_selfcheck.json"],
    "endpoints": ["POST …/attachment", "POST …/comment"],
    "results": {"attachment": {"http_code": 200, "category": "task_ai", "source": "AI", "file_size": 78}}
  },
  "attachments": [{"file_name": "t5b_selfcheck.json", "source": "AI", "ai_profile": "t5b-agent", "category": "task_ai"}]
}
```

- **①** `latest_summary` → `AI_REPORT` 评论 + `biz_task.ai_summary`：✅（749 字，正文是 AI 自己的执行摘要）
- **②** `runs[0].metadata` → `biz_task.ai_result`（jsonb）：✅（前端可直接渲染「交付清单」）
- **③** 产出文件：**走的是契约路径 A**（AI 自己用令牌 `POST /attachment`，`source='AI'`、`category='task_ai'`）；路径 B（`metadata.new_files` 兜底读盘上传）与路径 C（找不到 → 警告评论不阻塞）由代码实现 + 单测 `relay_path_guards_reject_untrusted_paths` / `relay_path_accepts_file_inside_workspace` 覆盖
- **④** 状态 → `REVIEW` + 通知验收人（创建人 + 分派人）：✅（`sys_notice` 1 条，标题「AI 已完成，请验收」）

**幂等**（这轮修复的直接证据）：同一 `done` 状态在多个 30s 轮询周期里只回传一次 ——

```
AI_REPORT 评论 = 2 条（摘要 + 「AI 已完成执行…任务已进入待验收」尾条）
评论分布 = AI_REPORT×2 / PLAIN×3
通知 = 1 条
```

对比修复前：`TSK-202610-000018` 在 90 秒内写出了 **3 组重复** AI_REPORT（见 §九 bug 4）。

失败 / 异常映射实测：

| 场景 | 实测 |
|---|---|
| `failed` | 保持 `DOING` + 写 `runs[0].error` 评论 + 通知（代码路径 `handle_failed`，单测 `hermes_status_maps_to_expected_actions` 钉住映射表） |
| `blocked` | 任务留 `DOING`，`hermes_status=blocked`，评论「AI 需要人工协助：Iteration budget exhausted (80/80)…请在 Hermes 侧解除阻塞…」，通知分派人（`TSK-202610-000018` 的真实一次 blocked） |
| 404（关联 card 丢失） | `handle_missing` → `hermes_status='MISSING'` + 评论 + 允许重派（幂等键加 `:retry` 后缀，避免撞已删 card） |
| 轮询 CLI 起不来 | 整轮失败只记日志，**任务状态一字不改**（Hermes 升级后实测：任务停在 DOING，没有误判） |

---

## 五、b4：失败处理 + AI 进度回写 + 统计维度

### 5.1 `PATCH /business/task/{id}/ai-progress`（令牌认证）

```
[3] 仅 note                      -> 200 code=0 data={progress:100, ignored_percent:false}
[4] percent=66（无子任务）        -> 200 code=0 ignored_percent=false，库内 progress=66（生效）
[5] percent=80（**有子任务**）     -> 200 code=0 ignored_percent=true，库内 progress=0（**被忽略**）
[6] 用别的任务令牌改本任务进度     -> 403 code=63303（越权）
```

评论正文按场景分三种（`AI 进度 66%：…` / `AI 进度由子任务汇总（本次 percent 已忽略）：…` / `AI 进度更新：…`），`percent` 越界 → 400 + `63308`。

### 5.2 统计 `GET /business/task/stats`（**新增**，exec_mode 维度）

```
GET /api/business/task/stats?start=2026-10-01&end=2026-10-29
{
  "start": "2026-10-01", "end": "2026-10-29",
  "total": {"exec_mode":"TOTAL","total":8,"todo":2,"doing":1,"review":5,"done":0,"cancelled":0,
            "completion_rate":0.0,"avg_done_hours":null,"avg_progress":66.3},
  "by_exec_mode": [
    {"exec_mode":"HUMAN","total":4,"todo":2,"doing":1,"review":1,"done":0,"cancelled":0,"completion_rate":0.0,"avg_progress":32.5},
    {"exec_mode":"AI",   "total":4,"todo":0,"doing":0,"review":4,"done":0,"cancelled":0,"completion_rate":0.0,"avg_progress":100.0}
  ]
}
```

口径（写在 handler 文档里，前端卡片照抄）：
- 只统计**顶层任务**（`parent_id IS NULL`）：子任务两边都算会让完成率随拆解粗细漂移；
- 窗口按 `created_at` 的 CST 日区间（`>= start::date AND < end::date + 1`，能吃 `idx_biz_task_created`），默认最近 30 天，上限 366 天，倒置/超限 → 400；
- `completion_rate = done / (total - cancelled)`（取消是分派人的动作，不该算进 AI 的完成率），保留 4 位小数，分母 0 → 0.0（不是 NaN）；
- 合计行按「已完成数 / 有效任务数」**加权**（直接平均会把 1 条 AI 任务的权重放大到 50%，单测 `total_is_weighted_by_done_count_not_a_plain_mean` 钉住：10.3 h vs 25.0 h）；
- 可见性：与列表页同一条谓词（`scope ∪ 参与 id 数组`）—— 统计不能绕开数据权限；
- `by_exec_mode` **恒含 HUMAN / AI 两行**（窗口内无任务给 0），历史脏值另起行如实列出。

---

## 六、测试与门禁

```
cargo test            636 passed（unit）+ 集成 11/8/7/6/5/4/2/1/1 全 ok + doctest 7 ignored，0 failed
cargo fmt -- --check  PASS
cargo clippy --all-targets -- -D warnings  PASS
cargo check           PASS
pnpm typecheck        PASS
批 B 闸① / 闸②         PASS（0 处 / 0 处）
C7 G1–G6              PASS（21 个 record 调用点全部在事务内）
色板一致性             PASS（22 套）
```

新增单测（本卡）：

| 模块 | 测试 |
|---|---|
| `middleware::task_token` | 白名单 4 端点 / 拒绝其余 / 令牌形态 / hash 稳定 / 唯一性 / 过期边界 / 终态失效（8 条） |
| `task::hermes_sync` | 优先级映射、幂等键格式、状态映射、body 契约（含**「主机名后不许紧跟引号」**回归断言）、多行标题展平、`metadata.new_files` 路径白名单（拒 `/etc/passwd`、拒 `..`）、mime 保守推断、**done 回传幂等** |
| `task::model::stats_tests` | 完成率（剔取消 / 钳 0–1 / 4 位小数）、默认 30 天窗口（跨月跨年）、窗口倒置与超 366 天 |
| `task::handler::stats_handler_tests` | 合计加权、空窗口（null 而非 0）、加权函数边界（0 / 负权重） |

---

## 七、迁移与部署验收

### 7.1 空库 up → down → up（脚本 `/root/t5b_verify_migration.sh`，应用账号执行）

```
== 0) 空库 public 表数 = 0
== 1) 用应用自身把空库推到最新（启动即迁移）：health ok，日志「应用 1 个迁移」
== 2) up 后自检
   biz_task 新列 7/7（ai_result jsonb / ai_summary text / exec_mode varchar NOT NULL DEFAULT 'HUMAN' / hermes_board / hermes_task_id / hermes_status / hermes_synced_at）
   附件新列 2/2（source DEFAULT 'HUMAN' / ai_profile）
   idx_biz_task_hermes = CREATE INDEX … USING btree (hermes_task_id) WHERE (hermes_task_id IS NOT NULL)
   令牌表 5 列，主键 = task_id，唯一索引 uk_biz_task_ai_token_hash = 1
   评论值域 = PLAIN / STATUS_CHANGE / PROGRESS_AUTO / ASSIGN / AI_REPORT / AI_PROGRESS
   ck_biz_task_exec_mode = HUMAN,AI；ck_biz_task_hermes_pair = (hermes_task_id IS NULL) = (hermes_board IS NULL)
== 3) 造存量任务（TSK-202610-900001）→ exec_mode 取默认 HUMAN
== 4) down 后：新列 0 / 令牌表 0 / 附件新列 0 / 索引 0 / 评论值域收敛回 4 值 / **存量任务仍在（1 行）**
== 5) up 后：7 列、令牌表 5 列、存量任务 exec_mode 仍 HUMAN；自检 NOTICE「V0112 自检通过：…」
== 6) 再跑一次 up：成功（幂等，只有 NOTICE）
== 8) 临时库删除
```

### 7.2 §6.2 生产部署

| 步骤 | 结果 |
|---|---|
| `cargo build --release` | 成功；`backend/target/release/cloudpivot`，sha256 `db626c0097b1cd99c4a7661cbf7f76705173c09a1adb23e54432a7edba512f53` |
| `systemctl restart cloudpivot` | `active`；日志「数据库迁移完成: 应用 1 个迁移」 |
| 生产库迁移 | `schema_migrations` 71 行，`max(version::int) = 112`（dev 同为 112） |
| 路由存在性（生产） | `/business/task/page` `/stats` `/gantt`、`{id}/comment` `{id}/attachment` `{id}/ai-progress` `{id}/dispatch-ai` 全部 **401**（= 已注册，不是 404） |
| 二进制含新逻辑 | `任务令牌只能访问本任务的详情`=1、`派活失败`=3、`/business/task/stats`=1、`task_hermes_sync`=1、框架补丁符号 `is_ignored_path`=1、`wildcard_segment_matches`=1 |
| 人类账号 | `admin` 生产登录 200 / `has access_token`；`GET /api/business/task/{id}` 200（令牌测试 §二 [9]） |
| 真实端到端 | §四（生产库、生产二进制、真实 AI worker） |

### 7.3 生产运行前置（运维必读，写进 `docs/Hermes集成部署记录-144.md`）
1. **`CLOUDPIVOT_AIAGENT_HERMES_BIN`**：Hermes 升级会换掉 CLI 路径。v0.15.2 是 `…/venv/bin/hermes`，v0.22.5 是 `/root/.hermes/hermes-agent/hermes`。路径错 → 轮询整轮失败（按红线不改任务状态），表象是「AI 跑完了但云枢一直 DOING」。
2. **AI profile 必须开 `approvals.mode: "off"`**：kanban worker 无人值守，任何「危险命令」都会等审批，60 秒后返回 `BLOCKED: User denied this command`，任务只会烧完迭代预算。同时把 `agent.max_turns` 调到 ≥200（真实任务要跑几十轮工具调用）。
3. **改框架补丁后必须 `cargo clean -p nik-web --release`**：`scripts/patch-nik-web.sh` 改的是 registry 源码，而 cargo 对 registry 依赖**不按 mtime 重编**（按 crate 校验和），release 会静默继续链旧 rlib。

### 7.4 §6.2 第 4/5 条：前端产物与真实可见性（走 nginx :80）

本卡**没有前端改动**（T6 的 4 个页面仍待做），所以 `frontend/dist` 沿用上一版构建产物；可见性按
「真实账号走 nginx 能否拿到数据」验证：

```
[1] nginx 登录 admin                        -> HTTP 200 code=0 token_chars=461 user=admin
[2] GET /api/business/task/stats            -> HTTP 200 code=0 bytes=539
    GET /api/business/task/page?page_size=1 -> HTTP 200 code=0 bytes=544
    GET /api/business/task/my               -> HTTP 200 code=0 bytes=1048
[3] 前端首页 /                              -> HTTP 200 bytes=609 含 vue 入口=True
[4] 匿名 GET /api/business/task/stats       -> HTTP 401 {"code":401,"msg":"未授权访问，请先登录"}
```

（`/stats` 返回 539 字节是因为 §十一 已清理夹具，窗口内无任务 —— HUMAN / AI 两行仍按契约返回。）

---

## 八、执行期环境变化（Hermes 升级）

执行过程中（16:51）144 上的 Hermes 由 **v0.15.2 升到 v0.22.5**（venv 变源码树、profile 配置格式 v0 → v51、board 数据保留）。影响与处理：

- 云枢侧 CLI 绝对路径失效 → 轮询失败（任务停在 DOING，未误判）→ 改 `CLOUDPIVOT_AIAGENT_HERMES_BIN` 并重启，随后积压的 done 立即回传成功；
- `kanban show --json` 返回结构在 v0.22.5 下**未变**（`task.status` / `latest_summary` / `runs[].metadata` 都在），解析无需改动；
- `kanban create` 的 `--body / --idempotency-key / --workspace / --max-runtime / --priority / --json` 参数均保留；
- `t5b-agent` profile 的 `approvals` / `agent.max_turns` 在配置格式迁移后仍在（`_config_version: 51`）。

---

## 九、端到端实测发现并修掉的 5 个 bug

| # | 现象 | 根因 | 修法 |
|---|---|---|---|
| 1 | AI 反复失败、烧完迭代预算 | 派活 body 的 URL 模板写成 `{base}"/api/…"`（多一个引号），curl 把引号当主机名的一部分 | 修正模板 + 加回归断言「主机名后不许紧跟引号」（`dispatch_body_contains_token_and_upload_contract`） |
| 2 | 令牌一律 401「Token 无效或已过期」 | 框架补丁只改了 registry 源码，release 一直链**旧的 nik-web rlib**（cargo 对 registry 依赖不按 mtime 重编） | `cargo clean -p nik-web --release` 后重编；用 `grep -ac is_ignored_path` 校验符号真进了二进制（写进部署注意事项） |
| 3 | `/business/task/page`、`/my`、`/gantt`、`/stats` 匿名访问从 401 变成 500 | CODE-11 的模板段匹配「任意单段」⇒ `{id}` 把静态端点也放行了（认证层跳过） | CODE-11b：模板段只匹配 UUID / 纯数字 / 16–64 位不透明串；匿名 `/page` `/stats` 恢复 401 |
| 4 | 同一 done 每 30 秒重放一次回传（90 秒写出 3 组重复评论 + 通知） | `sync_one` 的 `Done` 分支没有幂等守卫，注释里「任务之后不在扫描集合」是错的（REVIEW 不是终态） | 抽 `status_unchanged()` 并在 `Done` 分支前置判定；单测 `done_relay_is_idempotent_across_polling_cycles`；复测：AI_REPORT 恰好 2 条、通知 1 条 |
| 5 | 派活失败（profile 缺凭据）时不写评论、不通知、令牌残留 | `ensure_profile_config(...)?` 在失败收尾之前就 return 了 | 把 ②③④ 收进同一个 `Result`，失败统一走「删令牌 + 留痕评论 + 通知分派人 + 任务状态不变」；复测：502/63305 + `TODO|NULL|HUMAN` + 评论 + 通知 + 令牌 0 行 |

---

## 十、可复现脚本（144 上，验收现场用过的原样命令）

> 这些是**一次性脚本**（验收完已从 144 删除，避免把 admin 口令 / 任务令牌留在盘上）。
> 下表保留它们的原样路径与职责，配合 §二–§五 的逐条输出即可重写；任务令牌的明文只存在于
> 当时的 kanban body 与 `/root/t5b_e2e/task_token.txt`（600），两者都已随夹具清理删除。

| 脚本 | 作用 |
|---|---|
| `/root/t5b_verify_migration.sh` | 空库 up→down→up（建临时库 `t5bmig`，跑完删库） |
| `/root/t5b_deploy.sh` | 重启 + 健康检查 + 迁移版本 + 路由存在性 + 二进制内容 |
| `/root/t5b_fix_alun_web.sh` | `cargo clean -p nik-web --release` + 重编 + 符号校验 + 重启 |
| `/root/t5b_auth_verify.sh` | 匿名 / 伪造令牌 / 白名单外 五连验证 |
| `/root/t5b_e2e_setup.py`、`/root/resume_attempt4.py` | 造任务 → 分派 ROBOT → 派活 → 取令牌 → 拉起 Hermes worker |
| `/root/t5b_e2e_poll.py` | 双侧状态轮询（Hermes card + 云枢任务） |
| `/root/verify_token.py` | 令牌 9 项（正向 3 + 拒绝 5 + 人类回归 1） |
| `/root/verify_b2_b4.py` | 重复派活 / Hermes 幂等键 / 派活失败 / 统计 |
| `/root/verify_failure2.py` | 派活失败收尾复验（评论 + 通知 + 令牌清理） |
| `/root/verify_relay_progress.py` | 三层回传明细 + 回传幂等 + ai-progress 四种情形 |

证据 JSON：`/root/t5b_e2e/evidence_*.json`（含 attempt1 blocked 的现场、令牌 9 项、b2/b4、回传与进度）。
夹具清理：验收后删除 `t5b_ai` / `t5b_ai_noprof` 两个 ROBOT 账号、`sys_ai_agent` 两行、`TSK-202610-000017…000023` 等验证任务与其评论 / 附件 / 通知、归档 board `dev` 上的验证 card（见 §十一 清理记录）。

---

## 十一、夹具清理记录（验收后）

```
=== 清理前 ===
AI 任务 = 5（其中 4 个走完 REVIEW、1 个派活失败留 TODO）
ROBOT 账号 = 5（其中 2 个是本卡夹具，另 3 个是 L1/T5 早先建立的真实 AI 员工，未动）
令牌行 = 6
task_ai 附件文件 = 8

=== 清理（单事务）===
biz_task_ai_token    -6
biz_task_comment     -40
biz_task_member      -7
biz_task_attachment  -11
biz_task             -8（含 1 个子任务）
sys_file             -8（category='task_ai'）
sys_ai_agent         -2（t5b_ai / t5b_ai_noprof）
sys_user             -2（同上）

=== 清理后 ===
剩余 AI 任务 = 0
剩余 T5b 标题任务 = 0
剩余令牌行 = 0
剩余 task_ai 文件 = 0
剩余验证账号 = 0
剩余 ROBOT 账号 = 3（早先的真实 AI 员工，未触碰）
uploads/2026/10/29 下 AI 产出文件 = 已删（保留早先的 3 个 png）
kanban board dev = 6 张验证 card 全部 archived（board 显示 "no matching tasks"）
```

生产未留任何 T5b 夹具；`V0112` 迁移、`CLOUDPIVOT_AIAGENT_HERMES_BIN` 环境变量、`t5b-agent` profile
与 `approvals` 配置**保留**（前者是 schema，后两者是「派活能力」的运行前提，属交付物而非夹具）。
