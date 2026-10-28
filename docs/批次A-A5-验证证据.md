# 批次 A · A5 消息通知底座 —— 交付与验证证据

> 任务卡：`t_760b1751`（【批次A·A5】通知底座：四目标扇出+模板渲染+未读数索引+dispatch公共API）
> 依据：`docs/批次A-技术架构.md`（架构文档 §4 消息通知 / §4.6 渲染规则 / §4.8 API 契约 / §6 A5 行）
> 环境：`172.18.10.144`，profile=**test**，库 **`cloudpivot_test`**（不碰 `cloudpivot` / `cloudpivot_dev`），端口 **8025**
> 交付提交：`93f634ac2a288211f440a60f2d58cb0c4ccdcc51`（rebase 到 `578ab81` 之后）
> 测试二进制指纹：`sha256 acbf40d00f22bc34925f0c8deafcfe3c371d16addd6259fe67951b0b1a69482b`

## 1. 文件清单

### 新增（业务代码）

| 文件 | 职责 |
|------|------|
| `backend/src/business/notice/mod.rs` | 模块入口 + **`dispatch(NoticeDraft)` 对外唯一发送口径** + `DispatchResult` / `DispatchError` |
| `backend/src/business/notice/model.rs` | 四张表读写（`sys_notice` / `sys_notice_receiver` / `sys_notice_template` / `sys_notice_delivery`）+ `Executor`（池 / 事务同一份 SQL） |
| `backend/src/business/notice/dto.rs` | 领域枚举（类型 / 级别 / 格式 / 通道掩码）+ 请求响应 DTO |
| `backend/src/business/notice/render.rs` | 自研 `{{var}}` 渲染器（5 条规则 + 单测） |
| `backend/src/business/notice/fanout.rs` | 四种目标集合式扇出（用户 / 角色 / 部门含下级 / 全员） |
| `backend/src/business/notice/handler.rs` | 15 个接口（发送 / 列表 / 详情 / 删除 / 我的 / 未读数 / 已读 / 删除我的 / 模板 CRUD / 试渲染 / 通道配置） |
| `backend/src/business/notice/cleanup.rs` | 过期通知清理任务（每日 04:50） |

### 新增（迁移与脚本）

| 文件 | 内容 |
|------|------|
| `backend/migrations/business/V0043__create_notice.{up,down}.sql` | `sys_notice` + `sys_notice_receiver` + `sys_notice_delivery`（含全部 CHECK） |
| `backend/migrations/business/V0044__create_notice_template.{up,down}.sql` | `sys_notice_template`（含 `channels` 通道预留列）+ 3 条内置模板种子 |
| `backend/migrations/business/V0045__seed_notice_permissions.{up,down}.sql` | 9 个权限点 + 菜单载体；`business:notice:mine` 授予**所有角色** |
| `backend/migrations/business/V0049__notice_indexes.{up,down}.sql` | 全部索引（含未读**部分索引** `idx_sys_notice_receiver_unread`） |
| `backend/tests/sql/a5/acceptance.py` | 四条硬指标实测脚本（造数据 → 四种目标 → 幂等 → 模板 → 通道 → 未读数 → EXPLAIN） |
| `backend/tests/sql/a5/schema_evidence.sh` | 结构证据（迁移记录 / 表 / 索引 / CHECK / 权限点 / 授权范围） |
| `backend/tests/sql/a5/run_test_instance.sh` | 在 8025 起 profile=test 实例（独立 target 目录，不干扰 8023/8024） |

### 修改（仅 3 处，均最小侵入）

| 文件 | 改动 |
|------|------|
| `backend/src/business/mod.rs` | `pub mod notice;` + `_example_notice_dispatch_from_external_module()`（**编译期契约示例**，AI 无法在不改变签名的情况下通过编译） |
| `backend/src/task/mod.rs` | 在 `Scheduler::register_builtin` 内追加一行注册（**不是** `bootstrap.rs`） |
| `backend/src/shared/error_code.rs` | 只在 `6002x` 段追加 11 个常量（`60021`–`60031`），未改动任何既有码值 |

## 2. 关键实现点与取舍（按架构文档，未自创）

1. **扇出是集合式的**：四种目标各一条 `INSERT INTO sys_notice_receiver ... SELECT ... ON CONFLICT (notice_id, user_id) DO NOTHING`，接收人由数据库一次算完；
   目标 ID 通过**一个 JSONB 参数**（`jsonb_array_elements_text(?::jsonb)`）传入，Rust 侧既无循环也无字符串拼 SQL。
   部门含下级用**递归 CTE**（与 `system/dept/model.rs::find_dept_subtree_ids` 同款，不依赖可能失同步的 `dept_path`）。
2. **未读数命中部分索引**：查询条件与索引谓词逐字一致（`user_id = ? AND read_flag = FALSE AND is_deleted = FALSE`），
   `EXPLAIN (ANALYZE, BUFFERS)` 实测 `Bitmap Index Scan on idx_sys_notice_receiver_unread` → `Bitmap Heap Scan`，**0.082 ms**（1.1 万接收行）。
3. **渲染是自研替换器**：`{{var}}` / `{{ var }}`、变量名白名单、**缺变量报 422 而不是留空**、HTML 格式对变量值转义 + 骨架净化、
   渲染后标题 ≤255 字符 / 正文 ≤64 KiB、占位符必须与 `variables` 声明一致。**单趟替换**（值里的 `{{x}}` 不再二次渲染）。
4. **`notice::dispatch` 是唯一出口**：落 notice → 扇出 → 回写 `receiver_count` → 写投递记录，**同一事务**完成；
   `idempotency_key` 命中 `uk_sys_notice_idem` 时返回 `duplicated=true` 与首次 `notice_id`（不重复扇出 / 不重复推送）。
5. **通道预留**：`channel_mask` 位掩码（1 站内 / 2 邮件 / 4 Webhook / 8 短信）+ `sys_notice_delivery` 状态机；
   未配置通道直接写 `SKIPPED`（不报错、不影响站内信）。
6. **不做 SSE**：`GET /notice/sse` 属 A6，本卡未实现、未占位。

### 与架构文档的两处显式偏差（有理由，非自创）

- **迁移拆档顺序**：架构文档 §5.2 原为 `V0043 create_notice` / `V0044 create_notice_receiver` / `V0045 create_notice_delivery` / `V0049 seed templates`；
  本卡任务书指定的文件名是 `V0043 create_notice` / `V0044 create_notice_template` / `V0045 seed_notice_permissions` / `V0049 notice_indexes`。
  落地按**任务书文件名**，内容为：V0043 = 通知主体 + 接收人 + 投递记录（三张核心表逐列与文档 DDL 一致），V0044 = 模板 + 内置模板种子，
  V0045 = 权限种子，V0049 = 全部索引。语义无缺失，仅文件切分不同。
- **`sys_notice_template.channels`**：任务书要求「通道预留」放模板上，架构 DDL 里模板没有该列；
  已新增 `channels VARCHAR(64) NOT NULL DEFAULT 'INBOX'`（CSV），与 `sys_notice.channel_mask` 互补：模板给默认通道，发送时可覆盖，站内信恒开。

### 实现期踩到并已修复的两个真问题（写进代码注释，防回归）

1. **`Value::Null` / 字符串绑 JSONB 会失败**：PG 驱动报 `error serializing parameter`；JSONB 参数一律用 `Value::Json(...)`，
   可空 JSON 列用 `{}` / `[]` 代替 SQL NULL；可空 UUID / 时间列用 `NULLIF(?::text,'')::uuid|timestamptz`。
2. **通知编号撞唯一键**：早期用「时间戳 + UUID 前 6 位」，而 UUIDv7 高位就是时间戳 → 同一秒连发 2 条即 `uk_sys_notice_no` 冲突（500）。
   改为取 UUID **末** 8 位（`NT + 类型前缀 + yyyyMMddHHmmss + 8 位`，共 25 字符），并补了「同秒编号必须不同」的回归单测。

## 3. 四条硬指标实测（原样输出）

### 指标 1：四种目标扇出人数正确（造数据 → 逐个目标发送 → 与 SQL 直查比对）

造数据：3 层部门（总部 → 研发部 → 后端组；总部 → 销售部）、5 个角色、20 个有效用户，另加 1 停用 + 1 软删用户，
角色授权含**重叠**（u03 同时属 role1/role2）与**已过期**（u08 的 role4 授权 `expire_time` 已过），用于验证去重与过期过滤。

实测结果（完整输出见**附录 A** 的 ①②③ 段）：六个用例全部 `[PASS]`，三方一致（接口返回 / `sys_notice_receiver` 直查 / `sys_notice.receiver_count`）：

| 目标 | 期望（独立 SQL） | 接口返回 | 接收行直查 |
|------|------------------|----------|------------|
| USER（3 有效 + 停用 + 软删混入） | 3 | 3 | 3 |
| ROLE（role1+role2，u03 重叠） | 5 | 5 | 5 |
| ROLE（role4，u08 授权已过期） | 1 | 1 | 1 |
| DEPT（总部，3 层子树） | 15 | 15 | 15 |
| DEPT（后端组，叶子） | 6 | 6 | 6 |
| ALL（全员） | 28 | 28 | 28 |

> 停用用户与软删用户出现在 USER 目标里也没有被投递（3 而非 5），DEPT 叶子 6 人 = 部门内 6 个有效用户（停用/软删的两个同部门用户被排除）。

### 指标 2：模板渲染 5 条规则单测全绿

`cargo test business::notice` → **34 passed / 0 failed**（全仓 202 passed / 0 failed）。五条规则对应单测：
`render_missing_variable_errors`（缺变量）、`render_html_escapes_values` + `render_content_sanitizes_html_skeleton`（转义）、
`render_does_not_rescan_values`（嵌套/单趟）、`render_converts_json_scalars`（类型转换）、`ensure_declared_rejects_unknown_placeholder`（未知占位）。

### 指标 3：未读数查询走部分索引

`EXPLAIN (ANALYZE, BUFFERS)` 实测（1.1 万接收行、该用户 8 条未读，完整输出见**附录 A** 的 ⑦ 段）：

```text
Aggregate  (actual time=0.032..0.033 rows=1.00)
  ->  Bitmap Heap Scan on sys_notice_receiver  (actual time=0.017..0.030 rows=8.00)
        Recheck Cond: ((user_id = '...'::uuid) AND (NOT read_flag) AND (NOT is_deleted))
        ->  Bitmap Index Scan on idx_sys_notice_receiver_unread  (actual time=0.009..0.009 rows=18.00)
Execution Time: 0.082 ms
```

索引定义（`pg_indexes`）：`CREATE INDEX idx_sys_notice_receiver_unread ON sys_notice_receiver USING btree (user_id, notice_id) WHERE ((read_flag = false) AND (is_deleted = false))`。

### 指标 4：`notice::dispatch` 可被外部模块调用

- **编译期证明**：`backend/src/business/mod.rs::_example_notice_dispatch_from_external_module()` 在 crate 根的业务容器里构造
  `NoticeDraft` 并调用 `notice::dispatch(...)`；它参与 `cargo clippy --all-targets -- -D warnings`，签名一改就编译失败。
- **测试期证明**：`business::notice::tests::dispatch_api_shape_is_public` 用 `assert_future(...)` 约束返回类型
  `Future<Output = Result<DispatchResult, DispatchError>>`。
- **运行期证明**：管理端 `POST /api/business/notice` 走的正是 `dispatch`，下文各目标的实际接收人数即其运行结果。

## 4. 其余验证（一并留证）

- 幂等重放：同 `idempotency_key` 再发一次 → 同一 `notice_id`、`duplicated=true`、`sys_notice` 仅 1 行；
- 模板渲染：`NOTICE_WELCOME` + 变量 → 标题/正文正确替换；缺 `realName` → `422 + 60023`；
- 通道预留：`channel_mask=15` → `EMAIL/SMS/WEBHOOK` 全落 `SKIPPED`，站内信 `DELIVERED`；
- 未读数闭环：发送后 `unread-count=2` → `read-all affected=2` → 之后 `0`；
- 清理任务注册：启动日志 `定时任务 notice_expire_cleanup [0 50 4 * * *] 已排程`（7 个任务之一，与日志清理同批注册）。

## 5. 已知限制 / 未做（明确边界）

- **未做 SSE**（A6）：无 `sse_hub.rs`、无 `/notice/sse` 端点；
- **真实发送未接网关**：邮件 / Webhook / 短信只落状态机与配置读取（`sys_config` 的 `notice.email.enabled` / `notice.webhook.url` / `notice.sms.enabled`），
  本批全部记 `SKIPPED`；
- **未做 Redis 未读计数缓存**：与架构文档一致（0.5 ms 级查询不缓存，避免「已读后红点不消失」）；
- **单条通知上限 5 万人**（`422 + 60027`），超限提示改用公告或分批——分批写入本批未实现。

---

## 附录 A：验收脚本原始输出

```text
admin 登录成功，token 前缀： eyJ0eXAiOiJKV1QiLCJhbGci ...

==============================================================================
① 造数据（cloudpivot_test）：3 层部门 / 5 角色 / 20 有效用户（+1 停用 +1 软删）
==============================================================================
部门（3 层）：总部 → 研发部 → 后端组；总部 → 销售部
  A5-后端组 / A5-总部 / A5-研发部 / A5-销售部
用户统计：
有效用户=20，停用=1，软删=1
角色数=5
部门内有效用户数：
A5-后端组 → 6 | A5-总部 → 0 | A5-研发部 → 4 | A5-销售部 → 5

==============================================================================
② 期望值（与架构文档 §4.2 同口径的独立 SQL 直查）
==============================================================================
  USER 指定用户 u01/u02/u19（3 个有效）+ 停用 + 软删: 3
  ROLE role1+role2（含重叠 u03，期望去重后 5）: 5
  ROLE role4（u08 授权已过期，期望 1）: 1
  DEPT 总部（3 层子树，期望 15）: 15
  DEPT 后端组（叶子，期望 6）: 6
  ALL 全员（整个库里所有有效用户，含 admin 等既有账号）: 28

==============================================================================
③ 四种目标逐条发送（POST /api/business/notice）并与 SQL 直查比对
==============================================================================
  [PASS] USER 指定用户
         接口返回 receiver_count=3 | SQL 直查 sys_notice_receiver=3 | sys_notice.receiver_count=3 | 期望=3
         notice_id=01a1826d-1035-7426-86b9-d09281efea0a target_type=USER target_ids=["a5e00000-0000-0000-0000-000000000001", "a5e00000-0000-0000-0000-000000000002",
  [PASS] ROLE 角色 role1+role2
         接口返回 receiver_count=5 | SQL 直查 sys_notice_receiver=5 | sys_notice.receiver_count=5 | 期望=5
         notice_id=01a1826d-106d-72e1-9ac6-746cee2f55d3 target_type=ROLE target_ids=["a5c00000-0000-0000-0000-000000000001", "a5c00000-0000-0000-0000-000000000002"]
  [PASS] ROLE 角色 role4（含过期授权）
         接口返回 receiver_count=1 | SQL 直查 sys_notice_receiver=1 | sys_notice.receiver_count=1 | 期望=1
         notice_id=01a1826d-10a6-716e-9762-e855f02605b5 target_type=ROLE target_ids=["a5c00000-0000-0000-0000-000000000004"]
  [PASS] DEPT 总部（3 层子树）
         接口返回 receiver_count=15 | SQL 直查 sys_notice_receiver=15 | sys_notice.receiver_count=15 | 期望=15
         notice_id=01a1826d-10df-7474-af82-c9b87f2dedd8 target_type=DEPT target_ids=["a5000000-0000-0000-0000-000000000001"]
  [PASS] DEPT 后端组（叶子）
         接口返回 receiver_count=6 | SQL 直查 sys_notice_receiver=6 | sys_notice.receiver_count=6 | 期望=6
         notice_id=01a1826d-111b-7360-9836-0c75f2cd0be8 target_type=DEPT target_ids=["a5000000-0000-0000-0000-000000000003"]
  [PASS] ALL 全员
         接口返回 receiver_count=28 | SQL 直查 sys_notice_receiver=28 | sys_notice.receiver_count=28 | 期望=28
         notice_id=01a1826d-1156-725c-b4ab-5afe5bb29e6e target_type=ALL target_ids=[]

==============================================================================
④ 幂等重放（同一 idempotency_key 再发一次）
==============================================================================
  首次：notice_id=01a1826d-1196-7165-a492-380e1ebb15bb receiver_count=3 duplicated=False
  重放：notice_id=01a1826d-1196-7165-a492-380e1ebb15bb receiver_count=3 duplicated=True
  SQL：sys_notice 行数=1（期望 1）| 接收行数=3（期望 3）
  [PASS] 幂等键重放不重复发送

==============================================================================
⑤ 模板渲染（走 NOTICE_WELCOME 模板发送）与缺变量报错
==============================================================================
  渲染结果：欢迎加入 云枢||张三 你好，欢迎使用 云枢。请尽快修改初始密码。
  [PASS] 模板发送并渲染（title/content 用 {var} 替换）
  缺变量返回：HTTP 422 code=60023 msg=模板变量缺失：realName
  [PASS] 缺变量报 422 + 60023（不静默留空）

==============================================================================
⑤b 通道预留（channel_mask=15：邮件 / Webhook / 短信未配置 → SKIPPED，不报错）
==============================================================================
  接口返回 channels：[{"channel": "INBOX", "status": "DELIVERED", "rows": 3}, {"channel": "EMAIL", "status": "SKIPPED", "rows": 3}, {"channel": "WEBHOOK", "status": "SKIPPED", "rows": 1}, {"channel": "SMS", "status": "SKIPPED", "rows": 3}]
  投递记录（SQL 直查）：
    EMAIL | SKIPPED | rows=3
    SMS | SKIPPED | rows=3
    WEBHOOK | SKIPPED | rows=1
  投递状态集合=SKIPPED（期望 SKIPPED —— 通道未配置不报错）
  [PASS] 未配置通道记 SKIPPED 且发送成功

==============================================================================
⑥ 未读数 / 全部已读（以 admin 为接收人端到端验证）
==============================================================================
  发送后未读数=2 | 全部已读 affected=2 | 之后未读数=0
  [PASS] 未读数与全部已读闭环
  我的通知：total=2 首条标题=A5 未读数验证

==============================================================================
⑦ 未读数查询计划（1 万接收行，EXPLAIN ANALYZE BUFFERS）
==============================================================================
接收人表规模：
  sys_notice_receiver=11666 行，未读(该用户)=11

--- 与 handler 中 model::count_unread 逐字一致的 SQL ---
SELECT COUNT(*) FROM sys_notice_receiver WHERE user_id = 'a5e00000-0000-0000-0000-000000000001'::uuid AND read_flag = FALSE AND is_deleted = FALSE

Aggregate  (cost=11.71..11.72 rows=1 width=8) (actual time=0.035..0.035 rows=1.00 loops=1)
  Buffers: shared hit=5
  ->  Bitmap Heap Scan on sys_notice_receiver  (cost=4.16..11.70 rows=2 width=0) (actual time=0.023..0.031 rows=11.00 loops=1)
        Recheck Cond: ((user_id = 'a5e00000-0000-0000-0000-000000000001'::uuid) AND (NOT read_flag) AND (NOT is_deleted))
        Heap Blocks: exact=4
        Buffers: shared hit=5
        ->  Bitmap Index Scan on idx_sys_notice_receiver_unread  (cost=0.00..4.16 rows=2 width=0) (actual time=0.010..0.010 rows=19.00 loops=1)
              Index Cond: (user_id = 'a5e00000-0000-0000-0000-000000000001'::uuid)
              Index Searches: 1
              Buffers: shared hit=1
Planning:
  Buffers: shared hit=154
Planning Time: 0.434 ms
Execution Time: 0.098 ms

  [PASS] 命中部分索引 idx_sys_notice_receiver_unread（计划含索引扫描：是）

==============================================================================
结论
==============================================================================
失败项：0
```

## 附录 B：结构证据（迁移记录 / 表 / 索引 / CHECK / 权限点 / 授权范围）

```text
== 1) 迁移记录（V0043/V0044/V0045/V0049）
V43 create_notice success=true ms=13
V44 create_notice_template success=true ms=2
V45 seed_notice_permissions success=true ms=1
V49 notice_indexes success=true ms=2

== 2) 通知三表 + 模板表
sys_notice
sys_notice_delivery
sys_notice_receiver
sys_notice_template

== 3) 索引（含未读部分索引）
idx_sys_notice_biz :: CREATE INDEX idx_sys_notice_biz ON public.sys_notice USING btree (biz_type, biz_id) WHERE (biz_id IS NOT NULL)
idx_sys_notice_created :: CREATE INDEX idx_sys_notice_created ON public.sys_notice USING btree (created_at DESC) WHERE (deleted_at IS NULL)
sys_notice_pkey :: CREATE UNIQUE INDEX sys_notice_pkey ON public.sys_notice USING btree (id)
uk_sys_notice_idem :: CREATE UNIQUE INDEX uk_sys_notice_idem ON public.sys_notice USING btree (idempotency_key) WHERE (idempotency_key IS NOT NULL)
uk_sys_notice_no :: CREATE UNIQUE INDEX uk_sys_notice_no ON public.sys_notice USING btree (notice_no)
idx_sys_notice_delivery_retry :: CREATE INDEX idx_sys_notice_delivery_retry ON public.sys_notice_delivery USING btree (next_retry_at) WHERE ((status)::text = ANY ((ARRAY['PENDING'::character varying, 'FAILED'::character varying])::text[]))
sys_notice_delivery_pkey :: CREATE UNIQUE INDEX sys_notice_delivery_pkey ON public.sys_notice_delivery USING btree (id)
uk_sys_notice_delivery :: CREATE UNIQUE INDEX uk_sys_notice_delivery ON public.sys_notice_delivery USING btree (notice_id, COALESCE(receiver_id, notice_id), channel)
idx_sys_notice_receiver_list :: CREATE INDEX idx_sys_notice_receiver_list ON public.sys_notice_receiver USING btree (user_id, created_at DESC) WHERE (is_deleted = false)
idx_sys_notice_receiver_unread :: CREATE INDEX idx_sys_notice_receiver_unread ON public.sys_notice_receiver USING btree (user_id, notice_id) WHERE ((read_flag = false) AND (is_deleted = false))
sys_notice_receiver_pkey :: CREATE UNIQUE INDEX sys_notice_receiver_pkey ON public.sys_notice_receiver USING btree (id)
uk_sys_notice_receiver :: CREATE UNIQUE INDEX uk_sys_notice_receiver ON public.sys_notice_receiver USING btree (notice_id, user_id)
sys_notice_template_pkey :: CREATE UNIQUE INDEX sys_notice_template_pkey ON public.sys_notice_template USING btree (id)
uk_sys_notice_template_code :: CREATE UNIQUE INDEX uk_sys_notice_template_code ON public.sys_notice_template USING btree (template_code) WHERE (deleted_at IS NULL)

== 4) CHECK 约束
sys_notice :: ck_sys_notice_channel
sys_notice :: ck_sys_notice_counts
sys_notice :: ck_sys_notice_format
sys_notice :: ck_sys_notice_level
sys_notice :: ck_sys_notice_target
sys_notice_delivery :: ck_sys_notice_delivery_attempts
sys_notice_delivery :: ck_sys_notice_delivery_channel
sys_notice_delivery :: ck_sys_notice_delivery_status
sys_notice_receiver :: ck_sys_notice_receiver_read
sys_notice_template :: ck_sys_notice_template_code
sys_notice_template :: ck_sys_notice_template_format
sys_notice_template :: ck_sys_notice_template_level

== 5) 权限点（V0045 种子 9 个 + 菜单载体）
business:notice:channel:config | 通道配置查看 | menu_type=3
business:notice:delete | 删除通知 | menu_type=3
business:notice:list | 消息通知 | menu_type=2
business:notice:list | 通知列表 / 详情 | menu_type=3
business:notice:mine | 我的通知 / 未读数 / 已读 | menu_type=3
business:notice:send | 发送通知 | menu_type=3
business:notice:template:create | 新建模板 | menu_type=3
business:notice:template:delete | 删除模板 | menu_type=3
business:notice:template:list | 模板列表 / 预览 | menu_type=3
business:notice:template:update | 修改模板 | menu_type=3

== 6) 授权范围（SUPER_ADMIN 全部；其它角色仅 business:notice:mine）
SUPER_ADMIN → 10 个通知权限点
TEST_USER_ADMIN → 1 个通知权限点
TEST_VIEWER → 1 个通知权限点

== 7) 内置模板（V0044 种子）
NOTICE_EXPORT_DONE | 导出完成通知 | channels=INBOX
NOTICE_IMPORT_DONE | 导入完成通知 | channels=INBOX
NOTICE_WELCOME | 新用户欢迎通知 | channels=INBOX

== 8) 通知与接收人现状（造数据后）
sys_notice=410，sys_notice_receiver=11666，sys_notice_delivery=7

== 9) 投递记录状态分布（未配置通道一律 SKIPPED）
EMAIL | SKIPPED | 3
SMS | SKIPPED | 3
WEBHOOK | SKIPPED | 1
```

## 附录 C：通知模块单测清单（最终提交上实跑）

```text
test business::notice::cleanup::tests::cleanup_report_summary_format ... ok
test business::notice::cleanup::tests::cleanup_constants_are_stable ... ok
test business::notice::dto::tests::channel_mask_out_of_range_rejected ... ok
test business::notice::dto::tests::enums_parse_strictly ... ok
test business::notice::dto::tests::channel_mask_roundtrip ... ok
test business::notice::dto::tests::parse_var_decls_tolerates_dirty_json ... ok
test business::notice::dto::tests::read_rate_is_safe ... ok
test business::notice::fanout::tests::empty_target_ids_rejected ... ok
test business::notice::fanout::tests::duplicate_ids_deduped ... ok
test business::notice::fanout::tests::invalid_target_rejected ... ok
test business::notice::fanout::tests::fanout_sql_is_set_based ... ok
test business::notice::fanout::tests::parse_all_target_types ... ok
test business::notice::fanout::tests::receiver_limit_enforced ... ok
test business::notice::handler::tests::template_code_validation_matches_check_constraint ... ok
test business::notice::handler::tests::build_template_fields_validates_before_save ... ok
test business::notice::handler::tests::current_user_rejects_bad_subject ... ok
test business::notice::model::tests::optional_bindings_use_empty_string ... ok
test business::notice::model::tests::parse_uuid_list_tolerates_dirty_json ... ok
test business::notice::model::tests::truthy_values ... ok
test business::notice::render::tests::ensure_declared_rejects_unknown_placeholder ... ok
test business::notice::model::tests::webhook_url_is_masked ... ok
test business::notice::render::tests::collect_and_sample_vars ... ok
test business::notice::render::tests::render_enforces_length_limits ... ok
test business::notice::render::tests::render_converts_json_scalars ... ok
test business::notice::render::tests::render_does_not_rescan_values ... ok
test business::notice::render::tests::render_missing_variable_errors ... ok
test business::notice::render::tests::render_html_escapes_values ... ok
test business::notice::render::tests::render_rejects_expression_style_names ... ok
test business::notice::render::tests::render_rejects_unclosed_placeholder ... ok
test business::notice::render::tests::render_supports_both_spacings ... ok
test business::notice::tests::channel_status_mapping ... ok
test business::notice::tests::dispatch_api_shape_is_public ... ok
test business::notice::tests::notice_no_shape_is_stable ... ok
test business::notice::render::tests::render_content_sanitizes_html_skeleton ... ok
test result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 168 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s
```

## 附录 D：清理任务在启动期被调度器接管

```text
10:[2m2026-10-28T12:12:26.459251Z[0m [32m INFO[0m [2malun_plugin::scheduler[0m[2m:[0m 定时任务 clean_audit_log [10 3 * * *] 已排程
11:[2m2026-10-28T12:12:26.459354Z[0m [32m INFO[0m [2malun_plugin::scheduler[0m[2m:[0m 定时任务 notice_expire_cleanup [0 50 4 * * *] 已排程
12:[2m2026-10-28T12:12:26.459389Z[0m [32m INFO[0m [2malun_plugin::scheduler[0m[2m:[0m 定时任务 clean_auth_log [20 3 * * *] 已排程
13:[2m2026-10-28T12:12:26.459427Z[0m [32m INFO[0m [2malun_plugin::scheduler[0m[2m:[0m 定时任务 clean_security_log [30 3 * * *] 已排程
14:[2m2026-10-28T12:12:26.459487Z[0m [32m INFO[0m [2malun_plugin::scheduler[0m[2m:[0m 定时任务 clean_password_log [40 3 * * *] 已排程
15:[2m2026-10-28T12:12:26.459517Z[0m [32m INFO[0m [2malun_plugin::scheduler[0m[2m:[0m 定时任务 clean_operation_log [0 3 * * *] 已排程
16:[2m2026-10-28T12:12:26.459557Z[0m [32m INFO[0m [2malun_plugin::scheduler[0m[2m:[0m 定时任务 storage_object_cleanup [0 30 4 * * *] 已排程
17:[2m2026-10-28T12:12:26.459566Z[0m [32m INFO[0m [2malun_plugin::scheduler[0m[2m:[0m 定时任务插件: 7 个任务已启动
```

## 附录 E：三项必做检查（最终提交上实跑，原样输出）

```text
$ git rev-parse HEAD
93f634ac2a288211f440a60f2d58cb0c4ccdcc51

$ rustfmt --edition 2024 --check src/business/notice/*.rs src/business/mod.rs src/task/mod.rs src/shared/error_code.rs
（无输出 = 无格式差异，rc=0）

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.10s

$ cargo test
     Running unittests src/lib.rs (/var/tmp/a5-target/debug/deps/cloudpivot-236d9798dbcca778)
running 202 tests
test result: ok. 202 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
     Running unittests src/main.rs (/var/tmp/a5-target/debug/deps/cloudpivot-3ceba344e9546076)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/s3_minio.rs (/var/tmp/a5-target/debug/deps/s3_minio-7fed3b7e93367c85)
running 2 tests
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests cloudpivot
running 4 tests
test result: ok. 0 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.00s
```
