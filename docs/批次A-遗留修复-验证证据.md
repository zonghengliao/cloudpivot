# 批次 A · 遗留问题修复验证证据（6 项）

> 卡号：`t_45238f21`（【批次A修复】6项遗留）
> 代码分支：`fix/batch-a-legacy`（worktree `/var/tmp/fixA`，基线 `9a412ff`）
> 最终 HEAD：`ea7472f`（6 个功能提交 + 2 个证据 docs 提交，HEAD == origin/main）
> 验收环境：`172.18.10.144`，验收实例 `127.0.0.1:8027`，`profile=test`，库 `cloudpivot_test`
> 二进制指纹：`sha256 da3fd04cc83c0ac327b979ab4c25f83b52eb6a982afe21bbb874668e59053fa2`（size 289,968,928）
> 生产未动：prod（`8023` / systemd）与服务源码副本 `/opt/cloudpivot` 均未重启/未合并，等 laoliu 统一部署。
> 原始日志：`144:/var/tmp/fixA-evidence/{part1,part2,part3,smoke-final}.log`（合计 197 行）

## 0. 汇总

| 项 | 结论 | 提交 |
|----|------|------|
| ① 上传超限错误提示 | **已修**：422 + `60003` + 分类/服务端上限文案 | `92caf1d` |
| ② 权限规则冲突 WARN | **已修**：启动 WARN 22 条 → 0 条 | `381efa3` |
| ③ 定时发送 `scheduled_at` | **已实现**：V0051 + 30 秒巡逻任务，到点前用户侧不可见 | `00e1f90` |
| ④ `import:download` 路径不一致 | **已补**：文档路径 `/import/{id}/download` 别名 + 文档校正 | `797eb9b` |
| ⑤ cancel / retry / export 路由 | **接口早已存在（A4）**；真缺口是 `export:retry` 权限种子 + 前端入口 → 已补 | `c302a16` |
| ⑥ SSE 鉴权口径 | **已定稿**：header 优先 + `?token=` 仅白名单端点 + 伪造频道 400 | `1cc7946` |

全量回归：`cargo test` **258 passed / 0 failed**（lib）+ 7 + 2（集成）全绿；
`cargo clippy --all-targets` 零告警；前端 `pnpm typecheck` / `pnpm build` 通过。

---

## 项① 上传超限的错误提示

**现象**：`category=attachment`（策略上限 100 MB）上传 110 MB → `{"code":10006,"msg":"上传文件内容读取失败"}`。

**根因**：框架 `DefaultBodyLimit`（由 `[upload].max_size_mb=20` 推导）在读 body 途中就掐断请求，
而 `upload()` 把「取策略 + `check_upload`」放在读完 body 之后 → 策略校验永远轮不到。

**修法**：`[upload].max_size_mb` 20 → 128（覆盖常用分类上限）；`upload()` 改为
「先取策略 → `Content-Length` 前置判大小 → 再读 multipart」，并把读取阶段的 413 翻译成 `60003`，
按「分类上限 vs 服务端上限」给出不同文案；留 1 MiB multipart 头余量。新增 5 个单测。

**实测**：

```
①-a attachment(上限100MB) 上传 110MB
HTTP 422
{"code":60003,"msg":"文件大小 110.00 MB 超过该分类大小上限 100 MB"}

①-b attachment 上传 50MB
HTTP 200
{"code":0,...,"file_size":52428800,"category":"attachment","storage_code":"local-default","upload_mode":"SERVER"}

①-d document(上限200MB) 上传 150MB（分类允许、服务端扛不住）
HTTP 422
{"code":60003,"msg":"上传内容超过服务端单文件上限 128 MB，请改用预签名直传（POST /business/storage/presign）"}
```

## 项② 权限规则冲突 WARN

**现象**：启动日志刷 22 条 `权限规则冲突：… 已按「静态段优先」匹配，请修正规则`，其中包括
`/api/business/storage/config/page` 与 `/api/business/storage/config/{id}`（两者权限码相同）。

**根因**：CODE-09 补丁加的冲突自检把「静态段 + 模板段」的**所有**兄弟路由组合都算冲突；
但同一补丁已把匹配改为「完全相等(0) > 纯静态(1) > 含参数(2)」打分，静态段永远胜出，
同权限时更无鉴权差异 → 纯噪音。真正该报的是「同一路径模式被声明了不同权限」。

**修法**：`detect_conflicts` 改为按归一化路径模式（`{id}`/`{uuid}` → `{}`）两两比较，
仅「同模式 + 方法重叠 + 权限不同」才报；新增 `normalized_pattern()`。
补丁脚本用「方法起点 → 下一方法文档注释」区间替换，兼容历史三态；顺手修 `--check` 也写盘的缺陷。

**实测**：

```
启动日志「权限规则冲突」行数 = 0
②-a GET /business/storage/config/page → HTTP 200 {"code":0,...}
②-b GET /business/storage/config/{id} → HTTP 200 {"code":0,...}
```
补丁可用性（用干净 crate 源码验证首次安装路径）：12 个补丁步骤全 `[ok]`，幂等复跑 12 条 `[skip]`
且文件 md5 不变，`--check` 不写盘。

## 项③ 定时发送（`scheduled_at`）

**现象**：PRD N3 要求「立即 / 定时」，DTO/表/调度都没有 `scheduled_at`。

**修法**：V0051 加 `scheduled_at` / `dispatch_status` / `dispatch_attempts` + 部分索引
`idx_sys_notice_due`；`dispatch()` 对未来时间只建 `PENDING` 通知（**不写接收行**，因此用户侧到点前
天然不可见）；新增巡逻任务 `notice_scheduled_dispatch`（`*/30 * * * * *`，`FOR UPDATE SKIP LOCKED`
抢占，失败计数达 3 置 `FAILED`，到点前被软删置 `CANCELED`）。新增 5 个单测。

**实测**：

```
③-0 scheduled_at=now+400d → HTTP 400 {"code":10001,"msg":"定时发送时间最多提前 366 天"}

③-1 建 scheduled_at=now+70s
{"code":0,"data":{"notice_id":"01a18306-15c8-7172-b796-199a66e505f2","receiver_count":0,
 "duplicated":false,"channels":[],"scheduled":true,"scheduled_at":"2026-10-28T15:00:46Z"}}

③-2 当时（未到点）
  DB: PENDING / receiver_count=0 / scheduled_at=2026-10-28 23:00:46
  DB 接收行数：0
  /notice/mine 是否包含该通知：0
  管理侧 /notice/page：{"id":"01a18306-…","title":"定时发送验收-1分钟后",
    "dispatch_status":"PENDING","scheduled_at":"2026-10-28T15:00:46Z","receiver_count":0}

③-3 到点（+85s）
  +85s dispatch_status=SENT receiver_count=1

③-4 投递后
  /notice/mine 中该通知条数：1
  {"notice_id":"01a18306-…","title":"定时发送验收-1分钟后","receiver_id":"4a784368-…","read_flag":false}
  DB 接收行数：1
  日志：定时通知已到点投递 notice_id=01a18306-15c8-7172-b796-199a66e505f2
        notice_scheduled_dispatch: scanned=1 dispatched=1 skipped=0 failed=0 canceled=0

③-5 scheduled_at=now-5m → code=0 scheduled=false receiver_count=1（按立即发送处理）
```

## 项④ `import` 下载路径与文档不一致

**现象**：文档承诺 `GET /business/impexp/import/{id}/download`，实现只有 `/import/{id}/errors/file`
→ 按文档调用 404；另外架构文档 §3.8 的下载权限码写成 `import:download-error`，而注解/种子/前端都是
`import:download`。

**修法（双向、各有理由）**：新增 `/import/{id}/download` 别名路由（与 `errors/file` 同一 handler、
同一权限码）；权限码**改文档不改码**（改码要动注解/种子/前端三处，且会让已授权角色立刻失效）。
同步更新 `docs/批次A-导入导出接口.md` 与 `docs/批次A-技术架构.md` §3.8 / §5.1。

**实测**：

```
④-0 GET /business/impexp/export/sync?biz_type=system.dict_item
HTTP 200 type=application/vnd.openxmlformats-officedocument.spreadsheetml.sheet bytes=8683
④-1 上传建导入任务：code=0 task_id=01a18305-7082-75a6-9f30-bab421925c56 status=VALIDATED total=71 error=0
④-b 文档别名 GET /import/{id}/download        → HTTP 200 xlsx bytes=5690
④-c 推荐路径 GET /import/{id}/errors/file     → HTTP 200 xlsx bytes=5690
    两个产物 cmp 内容一致 = yes
```

## 项⑤ cancel / retry / export

**现象/核对结论**：A4 早已把三组路由全部落地（`import/{id}/cancel|retry`、`POST /export`、
`export/{id}/cancel|retry|delete`）—— A7 报「缺路由」是因为当时看的是 A3 快照。
**真正的缺口**：`business:impexp:export:retry` 权限码只有注解、没有种子（V0047 13 个权限点不含它）
→ 非超管角色点「重试导出」必然 403；且前端没有重试入口。

**修法**：V0052 补种权限点（按钮级菜单 + 授 SUPER_ADMIN + 授其余所有角色）；前端补
`PERM.impexpExportRetry`（33→34）、`retryExport()` api、导出任务页「重试」按钮（仅 FAILED /
CANCELLED / 产物过期显示，与后端 requeue 前置状态严格一致）。

**实测**：

```
⑤-a POST /export → task_id=01a18304-e773-… status=PENDING
⑤-b POST /export/{id}/cancel → HTTP 200 {"status":"CANCELED","message":"任务已取消"}
⑤-c 再次 cancel → HTTP 409 {"code":60032,"msg":"当前状态为 CANCELED，不允许取消"}
⑤-d POST /export/{id}/retry → HTTP 200 {"status":"PENDING",…}（复用同一任务行）
⑤-e 对 PENDING 再 retry → HTTP 409 {"code":60032,"msg":"当前状态为 PENDING，只有失败 / 取消 / 过期的任务才能重试"}
⑤-f POST /import/{id}/cancel（VALIDATED）→ HTTP 200 {"status":"CANCELED"}
⑤-f2 再次 cancel → HTTP 409 {"code":60013,"msg":"当前状态为 CANCELED，不允许取消"}
⑤-g POST /import/{id}/retry（CANCELED）→ code=0 新 task_id=01a18305-70d0-… 原 task_id=01a18305-7082-…（不同）
⑤-h cancel 不存在的任务 → HTTP 404 {"code":10002,"msg":"导入任务不存在"}
⑤-i sys_menu：business:impexp:export:retry | 重试导出任务
⑤-j sys_role_menu 授权行数：8
```

## 项⑥ SSE 鉴权口径

**修法**：框架补丁 CODE-10 —— `AuthLayer` 支持「头优先、无头时对白名单路径接受 `?token=`」，
两条来源汇入同一 JWT 校验；白名单来自 `NIK_AUTH_QUERY_TOKEN_PATHS`（默认空 = 关闭）。
业务侧 `stream()` 对 `?user_id=` / `?receiver_id=` 一律 400（频道归属只认令牌）。
文档：`docs/批次A-SSE鉴权口径.md`。

**实测**：

```
⑥-a 无 token                    → HTTP 401 {"code":401,"msg":"未授权访问，请先登录"}
⑥-b 伪造 token（header）        → HTTP 401 {"code":401,"msg":"Token 无效或已过期"}
⑥-c 伪造 token（query）         → HTTP 401 {"code":401,"msg":"Token 无效或已过期"}
⑥-d 合法 token（header）+ 发通知 → HTTP 200 text/event-stream，收到
     event: notice
     data: {"notice_id":"01a18305-78e4-…","title":"SSE-header-鉴权验收","receiver_id":"0edad7ea-…"}
⑥-e 合法 token（query）+ 发通知  → HTTP 200 text/event-stream，收到
     event: notice
     data: {"notice_id":"01a18305-9490-…","title":"SSE-query-鉴权验收","receiver_id":"a69379b4-…"}
⑥-f2 query token + 别人的 user_id → HTTP 400
     {"code":10001,"msg":"不接受客户端指定频道归属（user_id / receiver_id），用户身份只取自令牌"}
⑥-h run.log 中 token 前 24 字符出现次数 = 0；出现 'token=' 次数 = 0
     日志样例：请求完成 method=GET path=/api/business/notice/stream status=200
```

---

## 通用检查

| 检查 | 结果 |
|------|------|
| `cargo test`（最终树） | `258 passed; 0 failed`（lib）+ `7 passed` + `2 passed`（集成），4 个 doctest ignored（既有） |
| `cargo clippy --all-targets` | 零告警（含 `-D warnings` 语义下我改动文件无告警） |
| `pnpm typecheck` / `pnpm build` | 通过（在 `/var/tmp/fixA-fe` 副本上跑，未触碰 prod `frontend/dist`） |
| 迁移 | `V0051`（定时发送列/索引）、`V0052`（export:retry 种子）已在 `cloudpivot_test` 应用；未改 V0043 等历史迁移 |
| 不破坏既有验收 | 旧测试 248 → 258（新增 10 个），无一条既有用例失败 |
| 生产 | 未重启 prod 服务、未合并 `/opt/cloudpivot`、未动 prod 库 |

## 部署注意（给 laoliu）

1. **必须加环境变量**（三套 env，值相同），否则 `?token=` 订阅返回 401（前端会自动退化为 30 秒轮询）：
   ```bash
   NIK_AUTH_QUERY_TOKEN_PATHS=/api/business/notice/stream
   ```
2. **必须重跑框架补丁并强制重编 nik-web**：
   ```bash
   bash scripts/patch-nik-web.sh          # 幂等，12 步应全 [ok]/[skip]
   cd backend && cargo clean -p nik-web && cargo build --release
   ```
   `cargo` **不**跟踪 registry 源码 mtime —— 打完补丁不 `clean -p nik-web`，跑的还是旧 rlib（本次实测踩到）。
3. 部署启动时会自动应用 `V0051` / `V0052`（新迁移只加列/索引/种子，不改历史行）。
4. `[upload].max_size_mb` 20 → 128 会同时抬高服务端中转上传上限（`/business/storage/upload`、
   `/business/impexp/import/upload`）。若希望更保守，可只改这一处配置后再验证 50 MB 上传。
5. 可选加固：nginx 对 `/api/business/notice/stream` 关闭 access_log 或改用不含 query 的日志格式。

## 已知边界 / 保留意见

1. **query token 仍会把 access token 放进 URL**：本方案把它限制在白名单端点 + 同一条校验路径 +
   日志脱敏，但 nginx 侧 `$request` 仍会记录 query。彻底解决要 frontend 改 `fetch`+`ReadableStream`
   （后端无需改动），已写进文档作为后续项。
2. **`import/{id}/download` 是别名**：语义上 `errors/file` 更准确，别名仅为兑现历史文档承诺。
3. **补丁标记差异**：本次前已打过补丁的机器（含 144）文件里是旧标记 `[cailu-patch code-09]`，
   干净机器会写成 `[cloudpivot-patch code-09]`；仅注释文本不同，逻辑逐字节等价（已用干净 crate 复核）。
4. **定时通知没有「立即发送」接口**：要到点前改时间，目前只能软删后重建；PRD 未要求，未实现。
