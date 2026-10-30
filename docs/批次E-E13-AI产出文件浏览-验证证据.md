# 批次 E · E13：AI 产出文件浏览 —— 验证证据

> 卡面：`t_494dd64b`「AI 产出文件浏览：任务详情能看/下载 workspace 里的原始文件」
> 追加要求（老大原话「最好能打开」）：点文件名先**在线预览**，预览里再下载。
> 交付 commit：`86510fb`（feat）+ E7 单测文件补 fmt（上游债，rebase 时已被上游同款修复吸收）

## 0. 交付范围

| 类别 | 文件 | 说明 |
|------|------|------|
| 后端（新） | `backend/src/business/task/workspace_files.rs` | 3 个接口 + 7 条红线 + 11 个单测 |
| 后端 | `backend/src/business/task/mod.rs` | `pub mod workspace_files;` |
| 前端（新） | `frontend/src/views/business/task/components/AiWorkspaceFiles.vue` | 列表 + 预览抽屉 + Blob 下载 |
| 前端 | `frontend/src/views/business/task/detail.vue` | 「AI 产出文件」tab（仅 `exec_mode === 'AI'`） |
| 前端 | `frontend/src/api/business/task-api.ts` | `TaskCard.exec_mode` + 3 个接口函数 |

迁移 **0**；接口 **3 个**（卡面 2 个 + 追加要求的 preview）；新增权限码 **0**（复用 `business:task:read`）。

### 接口

```
GET /api/business/task/{id}/ai-workspace-files                   列表
GET /api/business/task/{id}/ai-workspace-files/{name}/download   下载（ReaderStream 流式）
GET /api/business/task/{id}/ai-workspace-files/{name}/preview    预览（追加要求）
```

另有两个**查询串别名**（`…/download?name=x`、`…/preview?name=x`，与路径形式同一套实现）：
HTTP 客户端（curl / 浏览器 / axios）会在发请求前把路径里的 `..` 规范化掉，
安全验收里的「路径穿越」只能靠查询串复现，留别名让红线条目有地方可打。

## 1. 七条安全红线 → 实现位置

| 卡面红线 | 实现 | 拒绝码 |
|---|---|---|
| 路径限定在该任务 AI profile 的 workspace 下，`canonicalize()` 后校验前缀，禁 `..` | `resolve_readable`：字符白名单 → `canonicalize` → **前缀 + 父目录双校验** | 400 / 10001 |
| 只读 | 模块内只有 `read_dir` / `metadata` / `File::open`，无任何写删路径 | — |
| 文件名白名单 `^[A-Za-z0-9._-]+$` | `validate_name`（另拒空名、`..` 子串、`.` 开头的隐藏文件、>200 字节） | 400 / 10001 |
| 单文件 ≤ 10MB | `resolve_readable` 在**打开之前**用 `metadata().len()` 拒绝 | 413 / 40007 |
| 扩展名白名单 `.md .txt .csv .json .log .png .jpg .pdf` | `ALLOWED_EXTS` + `ext_allowed`（fail-closed：无扩展名 / 未知扩展名一律不出现在列表） | 400 / 10001 |
| 只对 `exec_mode='AI'` 开放 | `task_workspace_dir` 读 `TaskDetailRow.exec_mode` | 400 / 10001 |
| 可见性走 `assert_task_visible` | 复用 `collab::visible_task`（= 架构里的 `assert_task_visible`，内部 `principal::resolve_for_claims` + `model::find_detail`，是「数据范围 ∪ 参与关系」的唯一实现） | 404 / 63001 |

补充（卡面没写但必须做）：

- **软链接逃逸**：`workspace/leak.md -> /etc/passwd` 会过字符白名单，但 `canonicalize` 后
  解析到 workspace 之外 → 400（有单测 `resolve_readable_blocks_symlink_escape`）。
- **子目录**：`parent()` 必须等于 workspace 根 ⇒ `sub/x.md` 也打不开（本卡只开放一级文件）。
- **profile 解析两级回退**：`sys_ai_agent.hermes_profile`（按任务负责人）
  → 最近一条 `source='AI'` 附件的 `ai_profile` → 都没有则 `400 + 64004`。

## 2. 单测（隔离 clone / 独立 target）

```
rustfmt --edition 2024 --check src/business/task/workspace_files.rs   → 无 diff
cargo clippy --all-targets -- -D warnings                              → 0 warning / 0 error
```

```
running 11 tests
test business::task::workspace_files::tests::base64_matches_rfc4648_vectors ... ok
test business::task::workspace_files::tests::ext_whitelist_rejects_executables ... ok
test business::task::workspace_files::tests::csv_preview_keeps_first_hundred_lines ... ok
test business::task::workspace_files::tests::preview_kind_matches_card ... ok
test business::task::workspace_files::tests::name_whitelist_blocks_traversal ... ok
test business::task::workspace_files::tests::profile_name_validation ... ok
test business::task::workspace_files::tests::resolve_readable_blocks_symlink_escape ... ok
test business::task::workspace_files::tests::resolve_readable_rejects_oversized_file ... ok
test business::task::workspace_files::tests::resolve_readable_blocks_escape_and_bad_ext ... ok
test business::task::workspace_files::tests::list_workspace_filters_and_sorts ... ok
test business::task::workspace_files::tests::markdown_preview_truncates_by_chars ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 825 filtered out
```

### 全量回归（rebase 到 origin/main = f05632f 之后复跑）

```
===== cargo fmt --all -- --check =====
FMT-OK
===== clippy --all-targets -D warnings =====
CLIPPY-DONE          （无 warning / error）
===== 全量 lib 单测 =====
test result: ok. 843 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

> 843 = 上游当时基线 832 + 本卡 11。**0 回归**（先在 d22d8a6 树上跑过 836/836，
> rebase 到 f05632f 后 E12 又带来 7 个用例，共 843）。

## 3. 隔离实例 e2e：34/34 PASS

环境：库 `cloudpivot_e13`（本卡专用）+ `127.0.0.1:18085` + redis db10 +
`CLOUDPIVOT_AIAGENT_WORKSPACE_ROOT=/root/e13-ws`（**不碰真实 `~/.hermes/workspace`**）。
夹具：`fixture.sql`（AI 员工 `e13-agent` / AI 任务 `TSK-E13-AI-001` / 人工任务
`TSK-E13-HUMAN-001` / 旁观者 `e13_outsider`，角色 `E13_OBSERVER` + `data_scope=4` SELF）。

workspace 里放 6 个文件：`report.md` / `data.txt` / `table.csv`（129 行）/ `pic.png` /
`malware.exe` / `big.log`（11MB 稀疏）。

```
[PASS] ② 列表 HTTP 200
[PASS] ③ 只返回白名单扩展名（.exe 被拒） — names=['pic.png', 'table.csv', 'data.txt', 'report.md']
[PASS] ④ 只返回白名单扩展名（>10MB 的 big.log 被拒） — 同上
[PASS] ⑤ 合法文件全部列出
[PASS] ⑥ profile 正确下发 — profile=e13-agent
[PASS] ⑦ workspace_exists = true
[PASS] ⑧ 列表行字段齐全 — {"name": "report.md", "path": "e13-agent/report.md", "size": 90,
        "mtime": "2026-10-30T02:57:34.768446447+00:00", "is_text": true, "preview_kind": "markdown"}
[PASS] ⑨ 列表不含服务器绝对路径
[PASS] ⑩ 下载 .md → 200
[PASS] ⑪ 下载内容逐字节一致 — 90 bytes
[PASS] ⑫ 下载头：attachment + 正确 MIME
[PASS] ⑬ 预览 .md → markdown + 原文
[PASS] ⑭ 预览 .csv → 前 100 行 + truncated — lines=100 truncated=True
[PASS] ⑮ 预览 .png → image + base64 可还原 — mime=image/png
[PASS] ⑯ 预览 .txt → text
[PASS] ⑰ path 形式穿越 `..%2F..%2Fetc%2Fpasswd` → 400 — msg=文件名只能包含字母、数字、点、下划线、连字符
[PASS] ⑱ path 形式穿越 `%2e%2e%2fSOUL.md` → 400
[PASS] ⑲ query 形式穿越 `?name=../../etc/passwd` → 400
[PASS] ⑳ query 形式穿越 `?name=../SOUL.md` → 400
[PASS] ㉑ 预览也拦穿越 `?name=../../etc/passwd` → 400
[PASS] ㉒ 绝对路径 `%2Fetc%2Fpasswd` → 400
[PASS] ㉓ 下载 .exe → 400 — msg=该扩展名不在白名单内（只允许 .md .txt .csv .json .log .png .jpg .pdf）
[PASS] ㉔ 下载 11MB 的 .log → 413 — code=40007 msg=文件 11.0 MB 超过单文件上限 10 MB，请从服务器直接取
[PASS] ㉕ workspace 里不存在的文件 → 404 — code=40005
[PASS] ㉖ 人工任务（exec_mode=HUMAN）→ 400 — msg=该任务是人类任务（exec_mode=HUMAN），没有 AI workspace
[PASS] ㉗ 人工任务下载 → 400
[PASS] ㉘ 无关用户（SELF 范围）列表 → 404 — code=63001
[PASS] ㉙ 无关用户下载 → 404
[PASS] ㉚ 无关用户预览 → 404
[PASS] ㉛ 不存在的任务 → 404 — code=63001
[PASS] ㉜ 无令牌 → 401
[PASS] ㉝ workspace 为空目录 → 空列表 + workspace_exists=true
[PASS] ㉞ workspace 目录不存在 → 空列表 + workspace_exists=false

===== E13 e2e：34/34 PASS =====
```

（① 两个账号登录也 PASS，为节省篇幅略。）

## 4. 生产验收（§6.2）

### 4.1 部署

```
git -C /opt/cloudpivot pull --ff-only        → 86510fb（7 files changed, 1695 insertions）
cargo build --release -j 2                    → Finished；二进制 sha 从 adbdd745… 变为 4792d36b…
  含 E13 文案（grep -ac "文件路径越界"）          → 1
  含 ai-workspace-files 路由                    → 1
pnpm build（前端）                             → dist/assets/detail-Cks_0_7v.js 含「AI 产出文件」
systemctl restart cloudpivot                   → active
日志                                           → 数据库迁移完成: 应用 0 个迁移
```

### 4.2 路由在位（无令牌 → 401，不是 404）

```
GET /health                                                          → HTTP=200
GET /api/business/task/<uuid>/ai-workspace-files                     → HTTP=401
GET /api/business/task/<uuid>/ai-workspace-files/x.md/preview        → HTTP=401
GET /api/business/task/<uuid>/ai-workspace-files/x.md/download       → HTTP=401
```

### 4.3 真实数据（生产库 `cloudpivot`，任务 `TSK-202610-000328` / profile `verify-ai`）

```
[PASS] ① 生产列表 200
[PASS] ② profile / workspace_exists — profile=verify-ai exists=True
[PASS] ③ 列出了 workspace 里的产出 — 23 个（含 TSK-202610-000329-selfcheck.md 等）
[PASS] ④ .py 等非白名单扩展名被过滤 — TSK-202610-000329-namecheck.py 未出现
[PASS] ⑤ 全是白名单扩展名
[PASS] ⑥ 有 .md 可验证 — md=TSK-202610-000329-selfcheck.md
[PASS] ⑦ 生产下载 .md → 200 — 11533 bytes
[PASS] ⑧ 下载头 — attachment; filename="TSK-202610-000329-selfcheck.md" / text/markdown; charset=utf-8
[PASS] ⑨ 下载内容非空且是文本 — # TSK-202610-000329 产出命名规范 · 自检报告（E14
[PASS] ⑩ 生产预览 .md → markdown + 原文 — chars=9058
[PASS] ⑪ 穿越 ?name=../../etc/passwd → 400
[PASS] ⑫ 穿越 ?name=../SOUL.md → 400
[PASS] ⑬ 穿越 path 形式 %2e%2e%2fSOUL.md → 400
[PASS] ⑭ 预览穿越 → 400
[PASS] ⑮ 生产 workspace 里不存在的文件 → 404
[PASS] ⑯ 生产人工任务 → 400 — msg=该任务是人类任务（exec_mode=HUMAN），没有 AI workspace
[PASS] ⑰ 无令牌 → 401

===== E13 生产验收：17/17 PASS =====
```

## 5. 真机（浏览器，http://172.18.10.144 生产站点）

### 5.1 AI 任务详情 →「AI 产出文件」tab 有列表

```
[CLICKED] TSK-202610-000328 标题 → 抽屉打开
[CLICKED] AI 产出文件（tab）

只读浏览 AI 执行者的 workspace（profile：verify-ai） —— 这里的文件是 AI 直接写在磁盘上的
原始产出，不经 storage；单个不超过 10.00 MB。            刷新
TSK-202610-000329-attachment-list-final.txt   1.2 KB   2026-10-30 10:55   下载
TSK-202610-000329-selfcheck.md               11.3 KB   2026-10-30 10:54   下载
TSK-202610-000328-selfcheck.md                5.7 KB   2026-10-30 10:54   下载
TSK-202610-000328-naming-check.json           4.3 KB   2026-10-30 10:54   下载
report.md                                     980 B    2026-10-29 18:13   下载
…（共 23 个，按修改时间倒序）
```

### 5.2 点文件名 → 预览抽屉（Markdown 真渲染）

```
点 .aiws__name「TSK-202610-000328-selfcheck.md」
预览抽屉：meta = "5.7 KB / markdown / [下载原文件]"
MarkdownViewer 渲染出的标题层级（真实 DOM）：
  H1:E14 产出命名验收 · 自检报告（A）
  H2:一、结论摘要
  H2:二、检查项与实测证据
  H3:A. 落盘命名规范检查（TSK-202610-000328-naming-check.json）
  H3:B. 带前缀上传（正常路径）
  H3:C./D. 无前缀 / 通用名上传（兜底路径，探针文件放在 scratch 目录，不污染 workspace）
  H3:E. 本地文件不可下载（须知）
  H2:三、给派活 body 的修订建议
```

### 5.3 预览里「下载原文件」→ 真 Blob 下载

（钩住 `URL.createObjectURL` 观察真实字节数）

```
{"downloads":[{"size":5868,"type":"text/markdown"}],"errorToast":null}
```

5868 字节 = 5.7 KB，与列表一致；无错误提示。

### 5.4 反证：人工任务没有这个 tab

```
打开 TSK-202610-000002（exec_mode=HUMAN）详情：
tabs = ["任务列表","任务看板","甘特图","评论 2","附件","工时 1"]
hasAiTab = false
```

## 6. 与卡面的差异（逐条说明，不是漏做）

1. **函数名**：卡面写 `assert_task_visible`，仓里实际叫 `collab::visible_task`
   （`pub(crate)`，已被评论 / 附件 / 工时复用）。语义完全一致（`scope ∪ 参与关系`），**复用而非另写**。
2. **接口数 2 → 3**：老大追加「最好能打开」，多了 `preview`；另有两个查询串别名（同实现）。
3. **`path` 字段下发的是 workspace 内相对路径** `<profile>/<name>`，不是服务器绝对路径
   （绝对路径对验收没价值，对攻击者有价值）。
4. **扩展名白名单比卡面多 `json`**：卡面列 `.md .txt .csv .json .log .png .jpg .pdf` 八项
   （原本就含 json），预览类型把 json 归到 `text`。
5. **CSV 截前 100 行、文本截 20 万字符**：`truncated=true` 时前端明示「完整内容请下载」，
   避免 1 万行 CSV 进抽屉把浏览器卡死。
6. **预览的 `content_type` 用的是卡面定义的「种类」**（markdown/text/csv/image/pdf），
   真实 MIME 另用 `mime_type` 字段下发（`<img src="data:...">` 要用）。

## 7. 收尾

- 隔离实例 `127.0.0.1:18085` 已 kill、专用库 `cloudpivot_e13` 已 drop，
  `/root/e13-ws`（假的 workspace 根）已删；生产 `~/.hermes/workspace/verify-ai` **未动**。
- 生产库零改动（本卡无迁移、无夹具写入）。
