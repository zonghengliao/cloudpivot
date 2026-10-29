# 云枢 ↔ Hermes kanban 集成设计（T5b）

> 老大 2026-10-29 拍板：**1A**（AI 产出强制上传到云枢）+ **2A**（AI 完成后 → REVIEW 待验收）
> 前置依赖：L1（AI 用户/profile）、T3（分派）、T5（AI 执行者）
> 本设计已查证 Hermes kanban 的真实 JSON 结构，可直接施工

---

## 一、派活契约（云枢 → Hermes kanban）

### 调用
```bash
hermes kanban create "<云枢任务标题>" \
  --assignee <AI用户的 hermes_profile> \
  --body-file <临时文件：描述 + 交付要求> \
  --workspace dir:<AI用户 workspace 绝对路径> \
  --priority <映射：P0→10 / P1→8 / P2→5 / P3→2> \
  --max-runtime 3600 \
  --idempotency-key "cailu-task:<云枢task_id>" \
  --created-by "cailu:<分派人用户名>" \
  --json
```
- **幂等键**：`cailu-task:<云枢task_id>` → 重复派活不会创建两个 Hermes 任务
- **解析 `--json` 输出**取 `task.id`（形如 `t_852858cd`）→ 存 `biz_task.hermes_task_id`
- 失败（非 0 退出）→ 云枢任务保持 TODO + 记录错误，**不允许"以为派出去了其实没派"**

### body 模板（云枢自动生成，含交付要求）
```markdown
# <任务标题>

## 任务描述
<云枢任务的 description>

## 优先级 / 截止
- 优先级：<P0..P3>
- 截止时间：<due_date>
- 预估工时：<estimate_hours> 小时（如有）

## 交付要求（云枢任务管理 · 必须遵守）
完成后请提供：

1. **执行摘要**：做了什么、怎么验证的（≤2000 字）→ 会作为任务评论展示
2. **产出文件必须上传到云枢**（不要只给路径）：
   ```bash
   curl -X POST "<云枢地址>/api/business/task/<task_id>/attachment" \
        -H "Authorization: Bearer <本次任务令牌>" \
        -F "file=@<你的产出文件绝对路径>"
   ```
   任务令牌：`<由云枢注入，仅能上传到本任务，有效期 24h>`
3. **代码改动**：如有，附 commit hash 与分支
4. **结构化产出**（可选但强烈建议）：在 metadata 里给 `new_files`（绝对路径数组）、`commit`、`endpoints` 等

## 约束
- 不要修改本任务之外的任何云枢数据
- 不要删除文件；产出物一律通过上述接口上传
```

---

## 二、任务级令牌（安全关键）

**为什么不用 AI 用户的长期 API Key**：一把 Key 能改所有数据，AI 被劫持/Runaway 会横向扩散。

**方案：任务级短期令牌**
```sql
biz_task_ai_token (
    task_id      uuid PRIMARY KEY REFERENCES biz_task(id) ON DELETE CASCADE,
    token_hash   varchar(128) NOT NULL,   -- sha256(token)，库内不存明文
    expires_at   timestamptz NOT NULL,    -- 派活时间 + max_runtime + 1h
    used_count   int NOT NULL DEFAULT 0,
    created_at   timestamptz NOT NULL DEFAULT now()
)
```
- 云枢派活时生成 32 字节随机 token → **只把明文写进 kanban body**（给 AI 用）→ 库内存 hash
- 该 token 的权限**仅限**：
  - `POST /api/business/task/{task_id}/attachment`（只能传本任务）
  - `POST /api/business/task/{task_id}/comment`（只能评本任务）
  - `PATCH /api/business/task/{task_id}/ai-progress`（只能更新本任务进度）
  - `GET /api/business/task/{task_id}`（只读本任务）
- 过期自动失效；任务 DONE/CANCELLED 后立即失效

---

## 三、结果回传（Hermes → 云枢）

### 轮询（每 30 秒，复用现有 `task/` 调度器）
```
对 biz_task WHERE exec_mode='AI' AND hermes_task_id IS NOT NULL 
             AND status NOT IN ('DONE','CANCELLED')
  → hermes kanban show <hermes_task_id> --json
  → 按 hermes status 分支处理
```

### 状态映射
| Hermes `task.status` | 云枢动作 |
|---|---|
| `ready` / `queued` | 云枢保持 TODO（显示"排队中"） |
| `running` | 云枢 → **DOING**（显示"AI 执行中"），同步 `hermes_synced_at` |
| **`done`** | 见下方「done 处理」 |
| `failed` | 云枢保持 DOING + 标红"AI 执行失败"，把 `runs[0].error` 写成评论，**通知负责人+分派人** |
| `blocked` | 云枢标"AI 需要人工协助"，把 block reason 写成评论，通知分派人 |
| 任务消失（404） | 云枢标"关联的 Hermes 任务已丢失" |

### `done` 处理（三层回传）
```
① latest_summary（长文本执行报告）
     → biz_task_comment（type='AI_REPORT', author = AI 用户）
     → biz_task.ai_summary（冗余，便于列表展示）

② runs[0].metadata（结构化：commit / migrations / endpoints / new_files）
     → biz_task.ai_result（jsonb，原样存）
     → 前端渲染成「交付清单」卡片

③ 产出文件
     · 路径 A（推荐，契约要求）：AI 已通过接口上传 → biz_task_attachment 已存在（source='AI', ai_profile=<profile>）
     · 路径 B（兜底）：metadata.new_files 里的路径若在 /root/.hermes/workspace/<profile>/ 下（同机）
         → 云枢直接读取并上传到 storage（category='task_ai'）→ 建 attachment
     · 路径 C（找不到）：记录警告"AI 声明的产出文件未找到：<path>"，写评论，**不阻塞流转**

④ 状态流转（老大 2A）
     → biz_task.status = **REVIEW**（待验收）
     → 若 need_review=false → 仍走 REVIEW（AI 的活必须人点头）
     → 通知验收人（分派人/创建人）"AI 已完成，请验收"
     → 验收通过 → DONE；打回 → DOING（并重新派活给 AI？**本版不自动重派**，人工决定）
```

---

## 四、数据模型增量

```sql
-- biz_task 加字段
ALTER TABLE biz_task ADD exec_mode        varchar(16) NOT NULL DEFAULT 'HUMAN';  -- HUMAN|AI
ALTER TABLE biz_task ADD hermes_board     varchar(32);                            -- dev
ALTER TABLE biz_task ADD hermes_task_id   varchar(64);                            -- t_xxxx
ALTER TABLE biz_task ADD hermes_status    varchar(32);                            -- 同步来源
ALTER TABLE biz_task ADD hermes_synced_at timestamptz;
ALTER TABLE biz_task ADD ai_summary       text;                                   -- 冗余 latest_summary
ALTER TABLE biz_task ADD ai_result        jsonb;                                  -- runs[0].metadata
CREATE INDEX idx_biz_task_hermes ON biz_task(hermes_task_id) WHERE hermes_task_id IS NOT NULL;

-- biz_task_attachment 加字段
ALTER TABLE biz_task_attachment ADD source     varchar(16) NOT NULL DEFAULT 'HUMAN';  -- HUMAN|AI
ALTER TABLE biz_task_attachment ADD ai_profile varchar(64);                            -- 哪个 AI 产出
```

---

## 五、AI 进度回写（可选但推荐）
AI 执行中可主动更新进度（长任务很有用）：
```
PATCH /api/business/task/{task_id}/ai-progress
  Body: { "percent": 45, "note": "已完成数据迁移，正在跑测试" }
  → 用任务级令牌认证
  → 云枢更新 biz_task.progress + 写评论
```
**限制**：有子任务时忽略 percent（进度由子任务汇总，D4 规则）。

---

## 六、失败/异常处理清单

| 场景 | 云枢行为 |
|---|---|
| `hermes kanban create` 失败 | 任务保持 TODO + 记录错误 + 通知分派人，**不置 DOING** |
| AI 超时（超过 max-runtime） | Hermes 会 kill → 云枢收到 failed → 标失败 + 通知 |
| AI 交的产出为空（无 summary 无附件） | 进 REVIEW 但标"AI 未提供产出"，验收人可打回 |
| 同一个任务重复派活 | 幂等键保证只有一个 Hermes 任务；若需重派，生成新幂等键（`cailu-task:<id>:retry2`） |
| 云枢轮询失败（hermes CLI 不可用） | 记 error 日志，**任务状态不改**（避免误判），下次轮询重试 |
| Hermes board 被删 | 任务标"关联 board 丢失" |

---

## 七、可观测性
- 每次轮询记一行日志：`task=<id> hermes=<t_id> status=<old>→<new>`
- AI 任务的执行耗时 = `runs[0].ended_at - runs[0].started_at`
- 统计接口增加维度：`exec_mode`（人类 / AI 分别统计完成率、平均耗时）

---

## 八、实施拆分（T5b 内部 4 小步）

| 步 | 内容 | 验收 |
|---|---|---|
| **b1** | 建表（hermes 字段 + 任务令牌表）+ 任务级令牌认证中间件 | 用任务令牌能传附件/评论，**不能访问其它任务**（403） |
| **b2** | 派活：`hermes kanban create` + body 模板 + 幂等键 | 派活后 `hermes kanban show` 能查到；重复派活只有一个 |
| **b3** | 轮询同步：状态映射 + done 三层回传 | 真跑一个 AI 任务 → summary 变评论、状态进 REVIEW、附件进云枢 |
| **b4** | 失败处理 + AI 进度回写 + 统计维度 | failed 场景通知正确；进度回写生效（有子任务时忽略） |
