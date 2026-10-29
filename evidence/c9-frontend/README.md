# 批次 C · C9 前端验收证据（公告 / 帮助 / 变更日志）

> 环境：144 真机，`feat/c9-frontend` 工作区 `/root/work/c9b`
> 后端：`target/debug/cloudpivot`（HEAD 构建）→ `:8099`，profile=dev，独立库 `cloudpivot_c9`（从 `cloudpivot_test` 克隆，不碰 prod 库 `cloudpivot` 与 `:8023`）
> 前端：`vite --port 5209`（`BACKEND_ORIGIN=http://127.0.0.1:8099`），经 SSH 隧道在本机浏览器实跑
> 时间：2026-10-29 10:51–11:00 CST

## 0. 一句话结论

6 页 1 组件全部落地，`pnpm typecheck` 0 错误、`pnpm build` 成功（15.78s）；6 条路由可达；
权限在**路由 + 按钮**两层生效（非超管进「变更日志」被踢到 `/result/403`、侧栏不渲染）；
XSS 双保险实测：把**未净化** HTML 直写库后，前端二次净化删掉 `script/iframe/svg/on*/style/javascript:`，
页面无执行；TOC 点击滚动 0 → 9252px 且高亮跟随；diff 表逐字段 `{b,a}` 成对 + 大字段「已截断，共 3333 字节」。

## 1. 交付物清单

| # | 文件 | 说明 |
|---|------|------|
| 1 | `frontend/src/api/business/announcement-api.ts` | 用户侧 7 + 管理侧 10 接口（对齐 `announcement/handler.rs` 注解） |
| 2 | `frontend/src/api/business/help-api.ts` | 分类 5 + 文档 9 + 检索 1 接口（对齐 `help/handler.rs`） |
| 3 | `frontend/src/api/business/changelog-api.ts` | 分页 / 详情 / 时间线 / 统计 / 导出 / 登记表 6 接口 |
| 4 | `frontend/src/views/business/announcement/center/index.vue` | 公告中心（列表 + 详情 + 已读 + 全部已读 + 只看未读 + 附件下载） |
| 5 | `frontend/src/views/business/announcement/manage/index.vue` | 公告管理（CRUD + 发布/下线/置顶 + 范围 + 有效期 + 附件 + 后端预览 + 阅读明细） |
| 6 | `frontend/src/views/business/help/center/index.vue` | 帮助中心（分类树 + 列表 + 检索 + 正文 + TOC） |
| 7 | `frontend/src/views/business/help/manage/index.vue` | 文档编辑（分类树维护 + Markdown 编辑器 + 预览 + 版本/回滚 + 发布） |
| 8 | `frontend/src/views/system/changelog/list/index.vue` | 变更日志列表（筛选 + 分页 + 导出） |
| 9 | `frontend/src/views/system/changelog/detail/index.vue` | 变更详情（**diff 对比表** + 大字段摘要 + 原始 JSON） |
| 10 | `frontend/src/components/MarkdownViewer.vue` | 渲染 `content_html` + TOC 滚动 + 代码块高亮/复制（1 组件） |
| 11 | `frontend/src/utils/sanitize-html.ts` | 白名单净化器（等价 DOMPurify 核心策略，零新依赖） |
| 12 | `frontend/src/utils/markdown-lite.ts` | Markdown 子集渲染（仅管理端本地预览；公告预览走后端） |
| 13 | `frontend/src/constants/permissions.ts` | 追加批次 C 20 个权限码（公告 7 + 帮助 10 + 变更日志 3） |
| 14 | `frontend/src/router/index.ts` | 追加 6 条路由（path 与 `sys_menu.path` 逐字一致） |

## 2. 六条路由（与后端菜单种子一致）

| 路由 | 组件 | 权限码 | 菜单种子 |
|------|------|--------|----------|
| `/announcement` | `business/announcement/center` | `business:announcement:read` | V0087 …0700（全员可见） |
| `/business/announcement` | `business/announcement/manage` | `business:announcement:list` | V0085 …0710（仅超管） |
| `/help` | `business/help/center` | `system:help:read` | V0088 …0740（全员可见） |
| `/help/manage` | `business/help/manage` | `system:help:doc:list` | 无独立菜单，由帮助中心「文档管理」按钮进入 |
| `/system/change-log` | `system/changelog/list` | `system:change-log:list` | V0085 …0730（仅超管） |
| `/system/change-log/detail/:id` | `system/changelog/detail` | `system:change-log:list` | 列表子路由 |

实测（浏览器 `location.pathname` + `document.title`）：

```
/announcement              → 公告中心 - 云枢管理后台
/business/announcement     → 公告管理 - 云枢管理后台
/help                      → 帮助中心 - 云枢管理后台
/help/manage               → 帮助文档管理 - 云枢管理后台
/system/change-log         → 变更日志 - 云枢管理后台
/system/change-log/detail/…→ 变更详情 - 云枢管理后台
```

截图：`01`–`06`。

## 3. 权限生效（非超管）

测试账号 `c9reader`，角色 `C9_READER` 只授予 4 个节点（公告中心 0700/0701、帮助中心 0740/0741）。

- 侧栏渲染结果：`["公告中心","帮助中心"]` —— 「公告管理 / 变更日志」不渲染；
- 直达 `/system/change-log` → `/result/403`，标题「无访问权限」，正文「抱歉，您没有权限访问该页面」；
- 直达 `/business/announcement` → `/result/403`；`/help/manage` → `/result/403`；
- 帮助中心「文档管理」按钮（`v-privilege="[PERM.helpDocList]"`）`offsetParent === null` → **不渲染**。

截图：`08-permission-403-changelog.png`。

## 4. XSS 双保险（前端净化实测）

三层证据：

**① 正常路径**：公告正文里的 `<script>` 被后端 `ammonia` 转义成文本（`content_html` 里是 `&lt;script&gt;…`），
页面渲染后 `.mdv__body script` = 0，`window.__xss_executed` / `__xss_executed2` 均为 `undefined`。

**② 极端路径（模拟后端回归）**：直接把**未净化**的 HTML 写进 `sys_announcement.content_html`：

```sql
<script>window.__xss_db = true; alert("db-script");</script>
<img src="x" onerror="window.__xss_db2 = true; alert('db-onerror');">
<a href="javascript:window.__xss_db3=true">点我会执行吗</a>
<iframe src="https://evil.example/x"></iframe>
<svg onload="window.__xss_db4=true"></svg>
<p style="position:fixed;top:0;left:0" onclick="window.__xss_db5=true">带 style 与 onclick 的段落</p>
```

页面打开该公告详情后，DOM 实测：

```json
{"scriptsInBody":0,"iframes":0,"svgs":0,"onAttrs":0,"styleAttrs":0,"jsHrefs":0,
 "sentinels":["undefined","undefined","undefined","undefined","undefined"]}
```

净化后保留的 HTML（`MarkdownViewer` 内 `v-html` 的唯一内容）：

```html
<h2 id="mdv-h-2-0">XSS 双保险测试</h2>
<p>下面这段是**未经后端净化**的原始 HTML（直接写库）。</p>
<img src="x">
<a>点我会执行吗</a>
<p>带 style 与 onclick 的段落</p>
<blockquote><p>正常内容必须保留：<strong>加粗</strong>、<em>斜体</em>、<code>行内代码</code>。</p></blockquote>
```

**③ 净化器单测式实测**（在页面里 `await import('/src/utils/sanitize-html.ts')` 直接喂 payload）：

```json
{ "html": "<h2 id=\"h-2-1\">标题</h2><img src=\"x\"><a>点我</a><p>正文</p><a href=\"https://ok.example/doc\" rel=\"noopener noreferrer\" target=\"_blank\">安全外链</a>",
  "removed": 2, "strippedAttrs": 4 }
```

> DOMPurify **不可用**（`package.json` 里没有，红线不允许新增依赖）：改为用浏览器原生 `DOMParser` 实现的
> 白名单净化器（标签白名单 / 属性白名单 / 协议白名单 / `id` 字符集校验），把「未知标签」拆壳保文本、
> 把 `script|style|iframe|object|embed|svg|math|form…` 整棵删除。

截图：`07-xss-sanitized-detail.png`。

## 5. TOC 点击滚动

帮助中心 `/help`，文档 8 个目录项（`如何申请年假 / 申请入口 / 审批流程 / 加急通道 / 常见问题 / 细则 / 相关文档 / 版本记录`），
点击最后一项后：

```json
{"scrollTopBefore":0,"scrollTopAfter":9252,"active":"细则","firstHeadingTop":-8854}
```

即：容器发生真实滚动、高亮项切到目标小节（`active` 跟随滚动位置更新，不是只改类名）。

## 6. diff 对比表 + 大字段摘要

`/system/change-log/detail/01a18592-7657-70d0-b6d9-f53ccd519e6a`（帮助文档保存：v1 → v2）：

| 字段 | 旧值（b） | 新值（a） |
|------|-----------|-----------|
| `status` | `PUBLISHED` | `DRAFT` |
| `summary` | 年假申请入口、审批流程与常见问题 | 年假申请入口、审批流程与常见问题（v2） |
| `version` | `1` | `2` |

大字段区块（`content_md`，> 2 KiB 不落原值）：

```
旧值（b）：已截断，共 3333 字节   sha256: d23659e24f478377…（64 字符）
新值（a）：已截断，共 3386 字节   sha256: c8d58e78e4dc4f00…（64 字符）
```

## 7. 每页五件事（列表 / 增 / 改 / 删 / 权限隐藏）

| 页面 | 列表 | 增 | 改 | 删 | 权限隐藏 |
|------|------|----|----|----|----------|
| 公告中心 | 分页 + 关键字 + 只看未读（切开关→「没有未读公告」） | —（用户侧无增删，业务如此） | 已读标记 / 全部已读（按钮变 disabled） | — | 无管理按钮；非读者角色无此菜单 |
| 公告管理 | 2 行 → 4 行 → 3 行 | UI 新建草稿成功（新行 `AN2026102915C76D`「草稿」） | UI 编辑标题 → 「已改名，验证改」 | UI 删除，弹确认框「确认删除「C9 UI 新建草稿（已改名，验证改）」？」→ 删除后 3 行 | reader 直达 403；6 个按钮 `v-privilege` |
| 帮助中心 | 分类树 + 列表 + 检索 | — | 阅读量上报（去重） | — | 「文档管理」按钮 reader 不可见 |
| 帮助文档管理 | 1 篇 → 2 篇 | UI 新建「如何重置登录密码」+ 生成 slug + 保存并发布（v1） | 编辑/「重新发布」；版本抽屉 4 行 | 文档删除（确认框）；分类删除（确认框） | reader 直达 403；分类 4 码 + 文档 5 码逐按钮 gate |
| 变更日志列表 | 35 条 → 筛选 | —（只读台账） | — | — | reader 直达 403；导出按钮 `changelog:export` |
| 变更详情 | 3 字段 diff + 1 大字段 | — | — | — | 同列表码（后端无独立 detail 码） |

补充：变更日志导出走后端 xlsx 字节流（实测 `200` + `content-type: …spreadsheetml.sheet` + `PK` 魔数 + 10 102 字节），
前端 `saveBlob` 落盘；`<a href>` 会 401（无鉴权头），所以必须取 blob。

### 7.1 发布 / 下线 / 附件（真实 UI 点击，非仅接口）

- **发布**：草稿行点「发布」→ 确认框「发布后…将推送给范围内 ? 位用户（含站内信），确认发布？」→ 确认后行状态
  `草稿 → 已发布`、受众 34；后端同时落 `PUBLISH` 变更日志。
- **下线**：点「下线」→ 弹「下线公告」对话框；**空原因提交被前端拦下**（toast「下线原因必填」），
  填写原因后确认 → 状态 `已下线`；变更日志 `action_label` = `下线公告：C9 验收：验证下线留痕（原因会进变更日志 action_label）`
  （原因随凭证留痕）。
- **附件**：`POST /business/announcement/{id}/attachment` 绑定 `c9-attach.pdf`（192 B，后端按存储策略只收
  jpg/pdf/docx/xlsx/zip，`.txt` 直接被拒 60004）；公告中心详情底部出现「附件（1）」区，
  行内展示 `c9-attach.pdf 192 B 下载`；点「下载」走鉴权 blob 路径（页面无报错），
  代理接口实测 `200 + application/pdf + content-length: 192 + disposition`，响应体以 `%PDF-1.4` 开头。

截图：`09-announcement-attachment.png`。


## 8. 与 PRD 的偏差（以后端已验收的注解 / 种子为准，已写入 `constants/permissions.ts` 头注释）

1. 帮助码是 `system:help:*`（PRD 写 `business:help:*`），分类四码 `category:*`；
2. 变更日志码是 `system:change-log:{list,export,clean}`，**没有** `detail` 码（详情复用 `list`）；
3. 公告读码是 `business:announcement:read`（PRD 写 `view`），下线是 `offline`（PRD 写 `revoke`），
   **没有** `publish-all` / `top` / `read-stat`（全员发布未拆码；置顶/阅读明细分别复用 `update`/`list`）；
4. `changelog` 的 `keyword` **不搜表名**（实测 `keyword=sys_help_doc` → 0 条；`resource` 参数才行）；
5. 帮助文档管理没有独立菜单种子，只能从帮助中心按钮进入（本卡未改后端种子）。

## 9. 后端侧观察（未改后端，仅记录）

- **全新建库跑不起来**：从空库执行迁移时 `V0085__seed_changelog_permissions` 自检失败
  （`SUPER_ADMIN` 角色由 `S__role_data` 种子创建，版本迁移先于种子执行 → 断言「SUPER_ADMIN 缺授权」抛错，
  报错被包成 `query failed: : db error`，看不出根因）。本次验证改用「克隆 `cloudpivot_test`」的库。
  建议后端后续把该自检改成「角色不存在则跳过」或调整迁移/种子执行顺序。
- `help/doc/page?status=`（空串）被当成「status 等于空串」，返回 0 条 —— 前端已在
  `asParams()` 统一丢弃空串 / null / undefined（三个 api 模块共用同一实现）。
