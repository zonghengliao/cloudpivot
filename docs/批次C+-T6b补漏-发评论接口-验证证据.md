# 批次 C+ · T6b 补漏「发评论接口 + 前端输入框」验证证据

> 卡面：`t_3cde801a`【T6b补漏】补发评论接口 POST /comments + 前端输入框（@提及入口）
> 交付：`78f6ae3`（后端 handler + dto + 前端 api/详情页）
> 环境：172.18.10.144；验收实例 `127.0.0.1:8025` + 库 `cloudpivot_t6b`（新建，已清理）；
> 生产 `cloudpivot.service`(:8023) + 库 `cloudpivot`

---

## 0. 一句话结论

缺口是**路径语义**而不是能力：`POST /business/task/{id}/comment`（单数）早就存在
（T5b 给 AI 任务令牌开的，`no_auth` + 中间件校验「令牌 ↔ 任务」），但
`GET|DELETE /business/task/{id}/comments`（复数）那一族里**没有人写评论的入口**，
老六按 REST 语义打 `/comments` 得到 404。

本次补上 `POST /api/business/task/{id}/comments`，与列表 / 删除同前缀、走人（JWT）身份；
**两个入口共用同一份写入实现**（`persist_comment`），没有第二份 `add_comment`。

---

## 1. 接口契约

| 方法 | 路径 | 中间件闸 | 行级判定 |
|---|---|---|---|
| POST | `/business/task/{id}/comments` | `business:task:read` | `collab::visible_task`（看不见 → 404） |

请求：`{ content, parent_comment_id?, mentions?, comment_type? }`
回执：`{ id, comment_type, user_id, user_name, created_at, mentions, mentions_ignored }`

### 四条校验（都是 400 + 63004，逐条有断言）

| 入参 | 规则 | 理由 |
|---|---|---|
| `content` | trim 后 1–**5000** 字，超长 **400**（不截断） | 用户照着字数写，静默砍掉后半段比报错更糟 |
| `mentions` | 非法 UUID → 400；账号不存在 / 停用 / 软删 → **忽略**，回执 `mentions_ignored` | 一人已离职不该让整条评论发不出去，但也不能假装成功 |
| `parent_comment_id` | 必须属于**同一任务**，跨任务 → 400 | 否则能把 A 的评论挂进 B 的回复链 |
| `comment_type` | 只允许 `PLAIN`（缺省 = PLAIN），伪造 `AI_REPORT` / `STATUS_CHANGE` → 400 | 系统类型是「评论的来历」，只读保护靠它 |

### 一份实现，两个入口

| 入口 | 路径 | 身份 | 正文上限 |
|---|---|---|---|
| 复数（本次新增） | `POST /comments` | 人类 JWT | `CommentLimit::Strict(5000)` 超长 400 |
| 单数（T5b 既有） | `POST /comment` | JWT 或 AI 任务令牌 | `CommentLimit::Truncate(8000)` 超长截断 |

`@` 提及仍走 `collab::notify_mentions`，`notice::dispatch(force = true)` ——
**不看收件人 `notify.inbox` 偏好**（修正 8）。

---

## 2. 前端

- `task-api.ts`：新增 `postTaskComment`（复数路径）+ 卡面名 `postComment` 别名；
  回执类型补 `mentions` / `mentions_ignored`；单数 `addTaskComment` 保留（AI 通道语义）。
- `detail.vue` 评论 Tab
  - `t-textarea` `autosize={minRows:3,maxRows:6}`、`maxlength=5000` + 实时字数 `n / 5000`；
  - `@` 提及复用 `MentionSelect.vue`（多选，选中的人进 `mentions`）；
  - 发送按钮 `content.trim()` 为空时 disabled；
  - 成功 → 清空输入 + 按时间位置插入列表 + `total + 1`（不重拉整页）；
  - **失败 → toast 且绝不清空输入**（红线）；
  - `Ctrl / ⌘ + Enter` 快捷发送；监听挂在 `.composer` 容器上
    （TDesign textarea 根节点是包裹层，事件冒泡到容器才 100% 命中）。

---

## 3. 验收（12 项，全部真跑）

### 3.1 接口侧（验收实例 8025 / 库 `cloudpivot_t6b`）

脚本：`144:/root/t6b_accept.py`（只打印状态码与关键字段，不打印口令 / 令牌）

```
[ok] admin 登录 HTTP=200
[ok] 用户 B 已建 id=01a1876e-5846-7247-b4af-eeeedadfef23
[ok] B 的 notify.inbox = false（已关闭）
[PASS] 建验收任务 A/B :: a=01a1876e-598e-70ed-9241-02754e96c7d3 b=01a1876e-5993-72f5-af0c-c5ba50fa0e58
[PASS] 12 匿名发评论 → 401（不是 404，说明路由已注册） :: HTTP=401 code=401 msg=未授权访问，请先登录
[PASS] 1 发评论 → code=0 且 comment_type=PLAIN :: HTTP=200 code=0 data={"id": "01a1876e-599c-...", "comment_type": "PLAIN", "user_name": "admin", ...}
[PASS] 2 GET /comments 列表包含新评论 :: HTTP=200 total=1 命中=True
[PASS] 3 @B（B 已关闭 notify.inbox）仍收到强制通知 :: HTTP=200 通知行数 0 → 1；记录=['NTT20261029113158554da270|admin 在任务「T6b补漏-验收任务A」里提到了你|01a1876e-5846-...|t']
[PASS] 4 @不存在的用户 → 忽略且在 mentions_ignored 里 :: mentions=['01a1876e-5846-...'] ignored=['00000000-0000-7000-8000-00000000dead']
[PASS] 5 伪造 comment_type=AI_REPORT → 400 且没落库 :: HTTP=400 code=63004 msg=用户只能发表 PLAIN 类型的评论，AI_REPORT 只由系统写入 落库行数=0
[PASS] 6 用任务 A 的评论作任务 B 的 parent → 400 :: HTTP=400 code=63004 msg=上级评论不存在，或不属于当前任务
[PASS] 6b 同任务 parent → 200 且列表里 parent_comment_id 正确 :: HTTP=200 命中回复=True
[PASS] 7 5001 字 → 400 / 空内容 → 400 / 正好 5000 字 → 200 :: 5001→400 code=63004 msg=评论正文最多 5000 字，当前 5001 字；空→400 code=63004 msg=评论正文不能为空；5000→HTTP=200
[PASS] 8 不存在的任务 → 404（不暴露 ID 是否存在） :: HTTP=404 code=63001 msg=任务不存在或已删除

RESULT: ALL PASS
```

第 3 项的关键点：`sys_user_preference(user_id, 'notify.inbox') = false` **先置上**，
再看 `sys_notice_receiver` 是否新增 —— 只验「偏好默认开 → 收到」等于没验 `force=true`。

### 3.2 前端真机（生产 UI，Chromium + CDP 真实事件）

1. 令牌注入 `localStorage['x_access_token']` → `GET /task` 列表 6 条任务；
2. 点「整理季度绩效材料」打开详情抽屉 → 评论 Tab 显示
   `placeholder="写下评论…（支持 Markdown；Ctrl / ⌘ + Enter 快捷发送）"`、`maxlength=5000`、
   `0 / 5000`、`发送` 按钮、`@ 提醒同事（可多选）`；
3. 输入 `T6b 前端验收：**粗体生效** + <script>alert(1)</script> 结尾` → 计数 `48 / 5000`，按钮可点；
4. `@` 选 `老六测试号（laoliu_test）` → 选中态 1 人 → 点发送：

```json
{"tabLabel":["评论 1","附件","工时"],
 "items":[{"author":"admin","text":"T6b 前端验收：粗体生效 + <script>alert(1)</script> 结尾",
           "strong":1,"script":0,"mentions":" 提及 1 人（已发强制通知） "}],
 "msg":["评论已发表"]}
```

   - **Markdown**：`<strong>` 1 个（粗体生效）；
   - **XSS**：评论条目内 `script` 元素 **0 个**，`<script>alert(1)</script>` 以纯文本呈现
     （`MarkdownViewer` → `sanitizeHtml` 白名单兜住）；
   - 列表出现新评论、tab 角标 `评论 1`。

5. 被 @ 的人真收到通知（生产库 `cloudpivot`）：

```
notice_no=NTT20261029113536ea26f01d | admin 在任务「整理季度绩效材料」里提到了你 | laoliu_test | unread=t
```

6. `Ctrl + Enter` 快捷发送（CDP `Input.dispatchKeyEvent`，`modifiers=2` + Enter）：

```json
{"count":2,"items":["…粗体生效…","T6b 前端验收：Ctrl+Enter 快捷发送"],
 "textarea":"","tab":["评论 2","附件","工时"]}
```

7. 红线「失败不清空输入」：CDP `Network.setBlockedURLs` 阻断 `*business/task/*/comments*` 后点发送：

```json
{"textarea":"T6b 红线：这条必须失败但内容要留住","messages":["Network Error"],"count":2}
```

   内容保留、只弹错误、列表不多条。

### 3.3 质量门禁

| 检查 | 结果 |
|---|---|
| `cargo test` | **692 passed / 0 failed**（新增 3 条：`user_comment_content_limit_is_strict` / `ai_comment_content_is_truncated` / `comment_type_forgery_is_rejected` 全 ok） |
| `cargo clippy --all-targets -- -D warnings` | 无告警 |
| `cargo fmt --check` | 通过（只对改动的 3 个文件跑 `rustfmt`，未重排无关文件） |
| `pnpm typecheck` | 零错误 |
| `pnpm build` | 通过（入口 `assets/index-xJiQGmsn.js`） |
| `bash scripts/check.sh` | 全部通过（闸①=0 处 / 闸②=0 处 / C7 G1–G6 6 条全过 / 22 套色板一致） |

### 3.4 §6.2 生产部署验收

`144:/root/t6b_prod_accept.py`：

```
[PASS] §6.2-a 匿名 POST /comments → 401 :: HTTP=401 code=401 msg=未授权访问，请先登录
[PASS] §6.2-b 真实登录成功 :: HTTP=200 code=0
[PASS] §6.2-c 能读生产任务列表 :: HTTP=200 total=6
  · 验收用生产任务：TSK-202610-000002 / 整理季度绩效材料
[PASS] §6.2-d 真实登录后发评论成功（comment_type=PLAIN） :: HTTP=200 code=0 id=01a18770-df93-... type=PLAIN
[PASS] §6.2-e 列表能看到该评论 :: HTTP=200 total=1 命中=True
[PASS] §6.2-f 验收评论已清理（软删） :: HTTP=200 code=0
RESULT: ALL PASS
```

部署指纹：

| 项 | 值 |
|---|---|
| 代码 | `78f6ae3`（HEAD = origin/main = bare main） |
| release 二进制 | `sha256=e1f629d416dcee6333fa69761536c714d95113d36f02fcec266f81e392eaab45` |
| 前端入口 chunk | `assets/index-xJiQGmsn.js` |
| 服务状态 | `cloudpivot.service` active，`/health` → `{"code":0,...,"profile":"prod"}` |
| 迁移 | 无新增（本卡不涉及 schema） |

### 3.5 生产数据清理

- 生产上两条前端验收评论 + 一条 §6.2 联通性评论**全部软删**，该任务评论数回到 `total=0`；
- 临时令牌文件 `frontend/dist/__t6b_token.txt` 已删除；
- 验收实例 8025、库 `cloudpivot_t6b`、运行目录 `/root/t6b-run` 均已删除
  （`killed pid=1224854；端口剩余：0` / `dropdb ok` / `rm /root/t6b-run ok`）；
- 生产那条 @ 提及通知也一并删掉（`DELETE 1` / `DELETE 1`），未碰 dev:8024 / prod:8023；
- 收尾复核：release 二进制 `sha256` 未变、`strings | grep -c 'business/task/{id}/comments'` = 3、
  匿名 POST 仍 `401`、`cloudpivot.service` active。

---

## 4. 踩到的坑（写进 skill 备查）

1. **`cloudpivot_test` 库不能直接用**：并行卡改过 `S__changelog_permissions.sql`，启动报
   `migration version 85 was modified after it was applied`（种子的 checksum 每次 `run` 都比对）。
   验收一律**新建库**（`su postgres -c "createdb -O cloudpivot cloudpivot_t6b"`）+ 独立运行目录。
2. **新建库的 admin 带 `must_change_password`**：登录成功但任何写接口 403
   「首次登录必须先修改初始密码」。脚本要「登录 → 查 `must_change_password` → 改密 → 重新登录」，
   且口令要能从「env 口令 / 上次改过的固定口令」两条路回退（否则重跑脚本必挂）。
3. **nginx 不服务 0600 的临时文件**：往 `frontend/dist/` 放一次性令牌文件必须 `chmod 644`，
   否则 `fetch` 拿到 403 页面（症状是「令牌长度 562、内容以 `<html>` 开头」）。
4. **浏览器 `page_info()` 返回 dict**，`page_info()[:600]` 会 `KeyError: slice`。
5. **`Network.setBlockedURLs` 是验「失败路径」的干净手段**：比在后端插 mock 开关好得多，
   不碰生产代码就能证明「失败不清空输入」。
