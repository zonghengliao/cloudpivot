# 批次 C · C3 公告管理侧 —— 验证证据

> 卡号：t_11c50208 ｜ 分支：`feat/c3-announcement-manage` ｜ 依据：`docs/批次C-技术架构.md` §2.8.2 / §2.8.3 / §2.2 / §2.6 / §5.5.2，PRD A1-01 ~ A1-10
> 验收环境：`172.18.10.144`，全新库 `cloudpivot_c3v`（49 条迁移全跑，含 V0080/V0081/V0087），验收实例 `127.0.0.1:8027`（profile=test，独立 target 目录）

## 1. 交付范围

### 1.1 接口（票面 10 个，全部实现并真机打通）

| # | 方法 | 路径（`/api` 由框架注入） | 权限码 |
|---|------|--------------------------|--------|
| 1 | GET | `/business/announcement/page` | `business:announcement:list` |
| 2 | POST | `/business/announcement` | `business:announcement:create` |
| 3 | PUT | `/business/announcement/{id}` | `business:announcement:update` |
| 4 | DELETE | `/business/announcement/{id}` | `business:announcement:delete` |
| 5 | POST | `/business/announcement/{id}/publish` | `business:announcement:publish` |
| 6 | POST | `/business/announcement/{id}/offline` | `business:announcement:offline` |
| 7 | POST | `/business/announcement/preview` | `business:announcement:create` |
| 8 | POST | `/business/announcement/{id}/attachment` | `business:announcement:update` |
| 9 | GET | `/business/announcement/{id}/readers` | `business:announcement:list` |
| 10 | PUT | `/business/announcement/{id}/top` | `business:announcement:update` |

**配套 3 条**（不是新增能力，只为契约可用性，均在模块文档里写明理由）：

| 方法 | 路径 | 为什么加 |
|------|------|----------|
| GET | `/business/announcement/{id}/manage` | 架构 §2.8.2 的管理详情写在 `GET /{id}`，该路径已被 **C2 用户侧**占用（权限码 `:read` + 可见性判定），matchit 不允许同路径注册两次 → 管理侧换段名 `/manage`。编辑器回填 `content_md` + 范围明细 + 附件靠它 |
| GET | `/business/announcement/{id}/read-stats` | 架构 §2.8.2 的路径名（与 `readers` 同源同实现） |
| POST | `/business/announcement/{id}/top` | 架构 §2.8.2 写 POST、票面写 PUT，两个动词都注册，同一实现 |

### 1.2 定时任务（C0 占位 → C3 接真实任务体）

| 任务 | cron | 任务体 |
|------|------|--------|
| `announcement_auto_expire` | `0 5 0 * * *` | `PUBLISHED` 且 `expire_at <= now()` → `EXPIRED` |
| `announcement_top_expire` | `0 10 0 * * *` | `is_top` 且 `top_expire_at <= now()` → 取消置顶并清空到期时间 |
| `announcement_dispatch_retry` | `0 */10 * * * *` | `PUBLISHED` 且 `notice_id IS NULL` → 重放扇出（**幂等**） |

启动日志（`/var/tmp/c3/run3.log`，节选）证明三者由业务模块注册、并已排程：

```text
INFO alun_plugin::scheduler: 定时任务 announcement_top_expire [0 10 0 * * *] 已排程
INFO alun_plugin::scheduler: 定时任务 announcement_dispatch_retry [0 */10 * * * *] 已排程
INFO alun_plugin::scheduler: 定时任务 announcement_auto_expire [0 5 0 * * *] 已排程
INFO cloudpivot::task: 批次C内置任务已注册（空实现） invoke_target="changelog_partition_maintain" ...
INFO cloudpivot::task: 批次C内置任务已注册（空实现） invoke_target="help_version_cleanup" ...
```

`task/mod.rs` 的 `BATCH_C_TASKS` 从 5 条减到 2 条（公告 3 条已搬进
`business/announcement/maintenance.rs`，注册点仍是 `Scheduler::register_builtin`）。

### 1.3 状态机（唯一一份定义在 `model::ensure_*`）

```text
                publish                offline
  DRAFT ───────────────────▶ PUBLISHED ─────────▶ OFFLINE
                               ▲   │                │
                               │   │ expire_at<=now │ publish（重新发布，version+1）
                               │   ▼                │
                               │ EXPIRED ◀──────────┘
                               └──── publish ────────┘
```

| 迁移 | 允许的源状态 | 非法时 | 依据 |
|------|--------------|--------|------|
| publish | `DRAFT` / `OFFLINE` / `EXPIRED` | **400** + 62009 | 票面验收③要求贴 400 证据 |
| offline | `PUBLISHED` | **400** + 62009 | 同上 |
| 编辑 PUT | `DRAFT` / `OFFLINE` / `EXPIRED` | **409** + 62009 | §2.8.3 校验表逐字写 409；A-18 也要求 409 |
| 删除 DELETE | `DRAFT` / `OFFLINE` / `EXPIRED` | **409** + 62009 | 先下线再删，避免「读者点进来 404」 |

两者都带 `62009`，前端按 code 分支即可，不必依赖状态码。

## 2. 验收逐条（真机 85 项全绿，脚本 `/var/tmp/c3/e2e_c3.py`，退出码 0）

```text
======================================================================
PASS 85 / FAIL 0
```

### 2.1 A-18 状态机（含 400 / 409 证据原文）

```text
S1 状态机（A-18）
  PASS  新建 → DRAFT / version=1 DRAFT/1
  PASS  公告编号形态 AN+日期+6位 AN2026102935C7BC
  PASS  DRAFT 可编辑 → 200 200
  PASS  DRAFT → publish 成功 200/PUBLISHED
  PASS  发布后 version=2（version+1） 2
  PASS  PUBLISHED 再发布 → 400 + 62009 400/62009          ← 非法迁移 400 证据
  PASS  PUBLISHED 直接 PUT → 409 + 62009 409/62009        ← A-18 原文要求
  PASS  PUBLISHED 直接 DELETE → 409 + 62009 409/62009
  PASS  下线缺 reason → 400 400/10001
  PASS  PUBLISHED → offline 成功 200
  PASS  OFFLINE 再下线 → 400 + 62009 400/62009            ← 非法迁移 400 证据
  PASS  OFFLINE → publish 成功且 version+1 200/v3         ← A-18「OFFLINE → publish 成功且 version+1」
```

### 2.2 A-05 发布扇出（一次发布 1 行、编辑后再发布第 2 行）

```text
S2 发布扇出（A-05）
  PASS  两次发布 → sys_notice 2 行 rows=2
  PASS  幂等键 = announcement:{id}:v2,v3
         announcement:01a18561-...:v2,announcement:01a18561-...:v3
  PASS  每行通知接收人 2 行 × 2 receivers=4
```

### 2.3 A-07 + A-06 扇出失败 → 补偿 → 幂等（**票面验收②**）

先注入数据库级故障让扇出真的失败（`ALTER TABLE sys_notice ADD CONSTRAINT c3_force_fail
CHECK (biz_type <> 'announcement') NOT VALID`），证明「发布成功但扇出失败」的中间态：

```text
S3 扇出失败补偿（A-07）
  PASS  扇出失败时发布仍返回 200（公告可见） 200
  PASS  扇出失败 → fanout=FAILED FAILED
  PASS  扇出失败 → notice_id 为空 None
  PASS  库里 PUBLISHED 且 notice_id IS NULL PUBLISHED|2|NULL
```

故障排除后连跑补偿任务（**同一个** `run_dispatch_retry()`，cron 与手动触发等价）：

```text
   round 1: announcement_dispatch_retry: scanned=1 repaired=1 failed=0 skipped=0
  PASS  首轮补偿后 sys_notice 恰好 1 行 rows=1
  PASS  首轮补偿后接收行 2 行 rows=2
  PASS  首轮补偿回填 notice_id 01a18562-041c-73b8-aa9f-3044c27795e3
   round 1: announcement_dispatch_retry: scanned=1 repaired=1 failed=0 skipped=0
   round 2: announcement_dispatch_retry: scanned=0 repaired=0 failed=0 skipped=0
  PASS  第 2 轮重放后 sys_notice 仍 1 行 rows=1
  PASS  第 2 轮重放后接收行仍 2 行 rows=2
  PASS  第 2 轮回填同一个通知 01a18562-... vs 01a18562-...
   round 1: announcement_dispatch_retry: scanned=1 repaired=1 failed=0 skipped=0
   round 2: announcement_dispatch_retry: scanned=0 repaired=0 failed=0 skipped=0
  PASS  第 3 轮重放后 sys_notice 仍 1 行 rows=1
  PASS  第 3 轮重放后接收行仍 2 行 rows=2
  PASS  第 3 轮回填同一个通知 01a18562-... vs 01a18562-...
```

> 「跑两次仍 1 行/人」的实现依据：幂等键 `announcement:{id}:v{version}` 在首次发布时写入
> `sys_notice.idempotency_key`（`uk_sys_notice_idem`），补偿重放命中同一键 → `dispatch`
> 直接回吐首次的 `notice_id`（`duplicated=true`），**不再插通知、不再扇出**，只把
> `notice_id` 回填到公告上。第二轮清空 `notice_id` 是为了模拟「回执丢失」，否则任务扫不到行。

同一结论有**可复跑**的真库集成测试兜底（`backend/tests/batchC_c3_dispatch.rs`，无
`C3_DB_URL` 时打印 `SKIP` 直接返回，不影响 `cargo test`）：

```text
[A-06/A-07] 补偿回填 + 连续两轮重放：sys_notice 恒 1 行、接收行恒 2 行 ✓
[止血开关] 关闭时 skipped=1 不补发；打开后 repaired=1 且仍 1 行 ✓
[auto_expire] expire_at 已过 → status=EXPIRED ✓
test retry_compensation_is_idempotent ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### 2.4 A-08 / A-09 已读幂等与计数不越界

```text
S6a 已读幂等 + 计数不越界（A-08/A-09）
  PASS  连点 10 次已读都成功 10
  PASS  只有第一次 first_read=true [True, False, ...]
  PASS  已读回执恰好 1 行 rows=1
  PASS  read_count 稳定为 1（不重复计数） 1
  PASS  read_count <= receiver_count（A-09 约束成立） 2/1
```

### 2.5 A-11 附件越权 / A-12 附件响应头

```text
S5 附件越权 + 响应头（A-11/A-12）
  PASS  存储上传成功（拿到 file_id） ...
  PASS  绑定附件 → 200 200
  PASS  附件下载地址是代理地址 /api/business/announcement/...
  PASS  可见公告的附件可下载 → 200 200
  PASS  A-12 Content-Disposition 以 attachment 开头 attachment; filename="..."; filename*=UTF-8''...
  PASS  A-12 Content-Type 不是 text/html application/octet-stream
  PASS  A-11 跨公告附件 → 404 + 62003 404/62003
  PASS  A-11 不可见公告的附件 → 403 + 62004 403/62004
```

> 跨公告用例特意用**两条都可见**的公告（`scope=ALL`）：若目标公告本身不可见，判定顺序会先
> 撞 `62004`（不可见）而不是 `62003`（附件不属于该公告），断言就测不到想测的分支 —— 首次
> 跑就是撞在这里，已修正。

### 2.6 XSS（预览 + 管理详情出参，票面验收⑤）

```text
S4 预览净化（A-13/A-14/A-15/A-16）
  PASS  content_html 不含真实标签 <script
  PASS  content_html 不含真实标签 <iframe
  PASS  content_html 不含真实标签 <svg
  PASS  content_html 不含真实标签 <img
  PASS  危险协议 javascript: 不在任何 href/src 里
  PASS  原始 HTML 被转义（&lt;img / &lt;svg 作为文本存在）
  PASS  Markdown 正常渲染（<h2>/<strong>）
  PASS  内联 HTML 被转义（&lt;div）
  PASS  warnings 非空（外链图片/危险链接）
         ['1 张外链图片已被移除（公告只允许站内存储的图片）',
          '1 个不安全链接已降级为纯文本（只允许 http / https / 站内相对路径）',
          '6 段原始 HTML 已转义为文本（公告正文不支持内联 HTML）']
  PASS  summary 自动生成非空
S9 管理详情 XSS（闸 3，脏快照）
  PASS  脏快照出参不含 <script / onerror <p>看图</p><img src="x">
```

> 断言口径（C2 已记过的坑）：`ENABLE_HTML=false` 会把原始 HTML **转义成文本节点**，
> 文本里仍能看到 `onerror=alert(1)` 这串字符 —— 浏览器不会解析成元素。因此断言「没有真实
> 标签 / 没有危险协议」，而不是断言「没有这个子串」。

### 2.7 校验规则（§2.8.3 表格逐条）

```text
S8 校验规则（§2.8.3）
  PASS  空标题 → 400 + 62001 400/62001
  PASS  正文 128KiB+1 → 400 + 62001 400/62001
  PASS  ROLE 范围缺 scope_ids → 400 + 62006 400/62006
  PASS  置顶缺 top_expire_at → 400 + 62007 400/62007
  PASS  有效期已过 → 400 + 62008 400/62008
  PASS  预览超 128KiB → 400 + 62001 400/62001
```

### 2.8 管理列表 / 置顶 / 阅读名单

```text
S7 管理列表 + 置顶（A-04 口径）
  PASS  管理列表 200 且有数据 4
  PASS  列表同时含 DRAFT 与 PUBLISHED（管理端不过滤状态） ['DRAFT', 'PUBLISHED']
  PASS  status 多选 + keyword 过滤生效 total=1 ['草稿：食堂菜单更新']
  PASS  过滤后都命中关键词
  PASS  keyword 命中公告编号
  PASS  非法 status → 400 + 10001 400/10001
  PASS  置顶 → 200 且 is_top=true 200
  PASS  列表里置顶生效（is_top=true） True
  PASS  POST 动词别名可取消置顶 200
  PASS  取消置顶后 top_expire_at 为 NULL（不撞 CHECK）

S6 阅读名单（A-08/A-09 观测口径）
  PASS  名单 total=2
  PASS  read_count=1 / read_rate=0.5 1/0.5
  PASS  state=UNREAD 只列未读（total=1） 1
  PASS  read-stats 别名可用 200
  PASS  非法 state → 400 + 10001 400/10001
```

## 3. 质量闸

```text
$ cargo test            → 456 passed（lib）+ 20 passed（集成/文档）= 476 passed / 0 failed
                          （C2 基线 415 → 456，本卡 +41 单测；+1 真库集成测试默认 SKIP 计为 pass）
$ cargo fmt -- --check  → 0（本卡只对改动文件跑 rustfmt，未重排无关文件）
$ cargo clippy --all-targets -- -D warnings → 0 warning / 0 error
$ bash scripts/check.sh → EXIT=0
    ==> 后端：cargo fmt --check                    通过
    ==> 后端：cargo clippy -- -D warnings          通过
    ==> 后端：cargo check                          通过
    ==> 前端：pnpm typecheck                       通过
    ==> 批次B 闸①：0 处   闸②：0 处               全部通过
```

## 4. 实现要点与取舍（给评审与下游）

1. **渲染/净化只有一条管线**：预览、发布时重算快照、管理详情出参三处都走
   `business::shared::content::{render, sanitize_html}`（`Recipe::Announcement`），前端不自行渲染。
2. **扇出只有一个实现**：`maintenance::dispatch_announcement()` → `notice::dispatch()`。
   发布接口与补偿任务共用它，幂等键也只在那里生成 —— 两处各写一份必然漂移。
3. **发布永远返回成功**：扇出失败只体现在 `fanout=FAILED` + `warning`，公告本身已可见
   （A-07 定义的合法中间态）。补偿任务每 10 分钟扫一遍补齐。
4. **全局止血开关**：`sys_config.announcement.fanout_enabled`（V0086 由 C8 种，缺省视为 `true`）。
   关闭时发布不扇出、补偿整轮 `skipped` —— 否则「止血」会被补偿任务无声推翻。
5. **PUT 省略可选字段 = 保持原值**（`dto::merge_with_existing`）：这是真机验收逼出来的修正 ——
   首版 `PUT` 只传标题+正文时，`scope_type` 会按默认值 `ALL` 落库，**把定向公告静默改成全员可见**。
   现在可清空字段一律走三态（缺席 = 保持 / `null` = 清空 / 有值 = 赋值），并有 3 个单测钉死。
6. **附件只写关联行**：文件本体走批次 A 的 `POST /api/business/storage/upload`（策略校验 / 秒传 /
   多后端路由都在那里），公告侧只写 `sys_announcement_attachment`。分类策略按
   `announcement` → `document` → 内置默认**三级回退**，V0086 落地后自动生效，不需改代码。
   额外硬拦可执行扩展名（`html/htm/xhtml/svg/js/exe/msi/bat/cmd/sh/jar/vbs/ps1`）——
   PRD §3.10 明确「即使白名单里有 .html 也不许挂」。
7. **下线原因只进日志**：`sys_announcement` 没有下线原因列（V0080 DDL 不在本卡可改范围，
   V0082/V0083 已分配给 C4、V0085/V0086 分配给 C8），所以 `reason` 必填后写 `tracing`
   日志与审计，不新开列。

## 5. 未在本卡范围（明确交接）

| 项 | 归属 | 说明 |
|----|------|------|
| `business:announcement:list/create/update/delete/publish/offline` 6 个权限码 + 公告管理菜单 | **C8 · V0085** | V0087 只种了用户侧的 `:read`（C2）。当前非超管访问管理接口会 403，属预期；本卡验收用 `SUPER_ADMIN` 令牌 |
| `sys_storage_policy` 的 `announcement` 分类行 + `announcement.fanout_enabled` 等 5 个 `sys_config` | **C8 · V0086** | 代码已按「未种则回退 `document` / 默认 true」处理 |
| 公告增删改与附件变更的**变更日志**（`changelog::record`） | **C6** | 本卡未接线：`sys_announcement` 尚未登记进 changelog 白名单（登记属 C6 的 registry 工作），现在调用会返回 62012 |
| 前端公告管理页（`business/announcement/index.vue`） | 后续前端卡 | 本卡只交付后端；`preview` 已按「前端不自行渲染」的契约提供 |
