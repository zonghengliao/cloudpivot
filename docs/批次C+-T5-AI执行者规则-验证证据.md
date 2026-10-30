# 批次 C+ · T5「AI 执行者规则细化」验证证据

> 卡面：`t_bb7d2ef3`；依据：`docs/批次C+-工作任务管理-核心设计.md` §六（AI 作为执行者）、
> `docs/云枢设计修正补丁-20261029.md` 修正 1（AI 用户不变式）/ 修正 11（AI 分派按角色授予）。
> 结论：**卡面 13 条验收全部实测通过**（16 条 HTTP 断言 + 6 条真库用例 + 迁移 up→down→up + 生产部署）。

---

## 零、结论速览

| # | 卡面验收 | 结果 | 证据位置 |
|---|----------|------|----------|
| 1 | AI 账号登录网页 → 403 + 63201 + `auth_logs` 留痕 | ✅ | §二 · 1 |
| 2 | 人类账号登录不受影响 | ✅ | §二 · 2 |
| 3 | AI token `DELETE /business/task/{id}` → 403 + 63202 | ✅ | §二 · 3 |
| 4 | AI token 调数据权限接口 → 403 + 63203 | ✅ | §二 · 4 |
| 5 | AI 无 `business:task:assign` → 分派 403 | ✅ | §二 · 5 |
| 6 | 授予带该权限码的角色 → 分派成功 | ✅ | §二 · 6 |
| 7 | AI 即使有角色也不能改数据权限（硬拒） | ✅ | §二 · 4/7 |
| 8 | AI 写操作 → changelog `operator_type='ROBOT'` | ✅ | §二 · 8 |
| 9 | 人类操作 → `operator_type='HUMAN'` | ✅ | §二 · 9 |
| 10 | 启动自检仍生效 | ✅ | §三 |
| 11 | `cargo test` 全绿 + 三类新单测 | ✅ | §四 |
| 12 | `scripts/check.sh` 全绿 | ✅ | §五 |
| 13 | §6.2 部署验收 | ✅ | §六 |
| — | 迁移 `V0111` 空库 up→down→up | ✅ | §七 |

---

## 一、改动清单（15 文件，+1134 / −24）

| 文件 | 改动 |
|------|------|
| `backend/src/shared/ai_actor.rs` | **新增**。四道硬拒的纯函数判定（`login_forbidden` / `task_delete_forbidden` / `data_scope_forbidden` / `aiagent_admin_forbidden`）+ `is_robot` + 3 条单测 |
| `backend/src/shared/error_code.rs` | 新增 `632xx` 子段（63201–63204）+ 稳定性单测；`63016` 标注「已由 63202 取代，值不回收」 |
| `backend/src/system/auth/handler.rs` | 登录流程插入 AI 硬拒（查到用户后、**验密之前**）+ `auth_logs` 留痕 |
| `backend/src/business/task/handler.rs` | 删任务硬拒码 `63016 → 63202`；`is_robot` 改为走 `shared::ai_actor` |
| `backend/src/business/permission/scope_handler.rs` | `role_scope_update` 最前面加 AI 硬拒（63203，先于角色存在性校验） |
| `backend/src/business/aiagent/handler.rs` | create / update / delete 三个写接口加 AI 硬拒（63204） |
| `backend/src/business/changelog/dto.rs` | 新增 `OperatorType` 枚举；`ChangeCtx.operator_type` + `with_operator_type`；列表 / 详情 / 时间线出参加 `operator_type` |
| `backend/src/business/changelog/model.rs` | `NewChangeLog.operator_type` + INSERT 列 + `operator_type_of(ex, id)`（同事务查 `sys_user.user_type`，带 `SCOPE-EXEMPT`） |
| `backend/src/business/changelog/mod.rs` | `record()` 在「空 diff 短路之后」解析操作人类型；导出 `OperatorType` |
| `backend/src/business/changelog/query.rs` | `SELECT_COLUMNS` / `ChangeLogRow` / `from_row` 带 `operator_type` |
| `backend/src/business/changelog/handler.rs` | 导出多一列「操作人类型」 |
| `backend/migrations/business/V0111__changelog_operator_type.{up,down}.sql` | 新增列 + CHECK + 部分索引 + 自检 DO 块 |
| `backend/tests/batchT_t5_ai_rules.rs` | **新增**。6 条真库用例（迁移结构 / ROBOT 写入 / HUMAN 写入 / 解析器默认值 / 显式覆盖 / 结构体必带字段） |

### 与卡面的一处差异（有意，且已在代码注释里写明）

- 卡面写「`business/permission/handler.rs` 已有接口加校验」；实际数据权限写接口在
  **`business/permission/scope_handler.rs::role_scope_update`**（`business/permission/` 下没有
  `handler.rs`，模块拆分为 `dto / mod / scope_handler / scope_model`）。按真实文件位置落地。
- 卡面写「`business/task/handler.rs` delete 返回 403 + 63202」；T3 当时临时用的是
  `63016 TASK_ROBOT_FORBIDDEN`。**码值改为 63202**（卡面是前端文案与验收脚本的契约），
  `63016` **常量保留、值不变**（码值不回收，历史日志仍可解析），只是不再有代码返回它。

---

## 二、真机 HTTP 实测（16 条断言全过）

实例：`127.0.0.1:18081`，库 `cloudpivot_t5v`（空库，启动时自动应用到 V0111），
二进制 `/root/t5-target/debug/cloudpivot`（与提交内容同一份源码）。
脚本：`/root/t5run/t5_e2e.py`（幂等，可重复执行）。夹具：角色 `T5_EXEC`（无分派权）/
`T5_ASSIGNER`（有分派 + 删 + `scope:write`），两个 AI 账号 `t5_robot` / `t5_exec`。

> **AI 令牌怎么来的**：T5 卡面明确「本卡不实现 Key 管理」，T5b 才做任务令牌。
> 所以验收用**合法 JWT**（先以 HUMAN 登录取到真实 claims，再把账号 `user_type` 翻成
> `ROBOT`）。四个硬拒判定读的都是库里的 `user_type`，与「AI 账号持有长期令牌」等价。

```
准备 tok_perm 的 claims 含 assign / delete / scope:write
[PASS]
       HTTP 200 permissions=['business:permission:scope:write', 'business:task:assign',
       'business:task:create', 'business:task:delete', 'business:task:read',
       'business:task:status', 'business:task:update']

验收 1：AI 账号尝试登录网页
[PASS] 1a AI 账号用正确密码登录 → 403 + 63201
       HTTP 403 body={"code": 63201, "msg": "AI 用户不能登录网页"}
[PASS] 1b auth_logs 有 FAIL 行（error_message=AI 用户禁止网页登录）
       auth_logs 最近 3 条：[['t5_robot', 'FAIL', 'AI 用户禁止网页登录', '15:30:28'],
                            ['t5_robot', 'SUCCESS', '', '15:30:27'],
                            ['t5_robot', 'FAIL', 'AI 用户禁止网页登录', '15:29:37']]

验收 2：人类账号登录
[PASS] 2 人类账号登录成功
       HTTP 200 code=0 msg=ok

验收 5：AI 账号（T5_EXEC，无 business:task:assign）分派
[PASS] 5 无权限码 → 403（普通权限拒绝，不是 632xx 硬拒）
       HTTP 403 body={"code": 63002, "msg": "AI 用户默认不能分派任务，如需请授予角色权限
       （business:task:assign）"}

验收 6：AI 账号（T5_ASSIGNER，有 business:task:assign）分派
[PASS] 6 有权限码 → 200 分派成功（修正 11：AI 分派不硬拒）
       HTTP 200 body={"code": 0, "msg": "ok", "data": {"id": "01a18691-488c-71e9-a0eb-…",
       "task_no": "TSK-202610-000001", "source": "TRANSFERRED", "auto_member": true, …}}
[PASS] 6b 任务负责人已变成 t5_exec
       owner_id=01a18691-3b92-7756-9b0f-bb4178b477a4

验收 3：AI 账号删除任务（claims 里有 business:task:delete）
[PASS] 3 DELETE /business/task/{id} → 403 + 63202
       HTTP 403 body={"code": 63202, "msg": "AI 用户不能删除任务（如需停止执行，请把任务
       转派给人类用户或改为「已取消」）"}
[PASS] 3b 任务没有被删（deleted_at 仍为空）
       deleted_at IS NULL = true

验收 4 + 7：AI 账号改数据权限（claims 里有 business:permission:scope:write）
[PASS] 4/7 PUT /business/permission/scope/role/{id} → 403 + 63203
       HTTP 403 body={"code": 63203, "msg": "AI 用户不能修改数据权限（数据范围只能由人类
       管理员调整）"}
[PASS] 7b 库内数据范围未被改动
       before=1 after=1

验收 8：AI 账号改任务状态（TODO → DOING）→ changelog
[PASS] 8a AI 改状态 → 200
       HTTP 200 body={"code": 0, "msg": "ok", "data": {"from": "TODO", "status": "DOING",
       "progress": 10, …}}
[PASS] 8b changelog 有 ROBOT 行（operator_type=ROBOT）
       最近 5 行：[['t5_exec', 'ROBOT', 'STATUS', '开始执行（前置依赖由 T4 校验：未完成前置
       → 409）', '{progress,status}'], ['t5_exec', 'ROBOT', 'ASSIGN', '分派 / 转派',
       '{assigner_id,owner_id,source}']]

验收 9：人类账号改同一任务状态（DOING → DONE）→ changelog
[PASS] 9a admin 改状态 → 200
       HTTP 200 body={"code": 0, "msg": "ok", "data": {"from": "DOING", "status": "DONE",
       "progress": 100, "done_at": "2026-10-29T07:30:30.813982Z", …}}
[PASS] 9b changelog 有 HUMAN 行（operator_name=admin, operator_type=HUMAN）
       最近 5 行：[['admin', 'HUMAN'], ['t5_exec', 'ROBOT'], ['t5_exec', 'ROBOT']]

验收 8c：changelog 查询接口返回 operator_type（前端「AI」标签的数据源）
[PASS] 8c GET /business/changelog/page 每项都带 operator_type，且两种类型都出现
       HTTP 200 items=[('admin', 'HUMAN'), ('t5_exec', 'ROBOT'), ('t5_exec', 'ROBOT')]

==============================================================================
汇总
==============================================================================
通过 16 条，失败 0 条
[clean] 夹具已清理（只删 t5- 段）
```

### 2.1 「判定在验密之前」的独立证据（生产二进制上跑的）

AI 账号用**错误口令**登录，仍然拿到 63201 而不是 401 —— 证明拦截发生在密码校验之前：

```
  AI 账号 + 错误口令 → HTTP {"code":63201,"msg":"AI 用户不能登录网页"} (403)
  对照：不存在的账号 + 同一口令 → HTTP {"code":20001,"msg":"用户名或密码错误"} (401)
  auth_logs → t5_probe_nobody | FAIL | 用户不存在
  auth_logs → t5_probe | FAIL | AI 用户禁止网页登录
  清理后残留 → sys_user=0 sys_ai_agent=0 auth_logs=0
```

---

## 三、启动自检（卡面 2.5 / 验收 10：复用 L1 的 `check_ai_agent_invariant`，不重复实现）

### 3.1 空库首次启动（`cloudpivot_t5mig`，自动应用 70 个迁移）

```
NOTICE: V0111 自检通过：operator_type varchar(16) NOT NULL DEFAULT HUMAN + CHECK + 部分索引
INFO   alun_web::app: 数据库迁移完成: 应用 70 个迁移
INFO   cloudpivot::shared::startup_guard: AI 用户不变式自检通过：ROBOT 账号与 sys_ai_agent 一一对应
```

### 3.2 存在真实 ROBOT 账号时的启动（生产库，重启后）

```
NOTICE: V0111 自检通过：operator_type varchar(16) NOT NULL DEFAULT HUMAN + CHECK + 部分索引
INFO   alun_web::app: 数据库迁移完成: 应用 1 个迁移
INFO   cloudpivot::shared::startup_guard: AI 用户不变式自检通过：ROBOT 账号与 sys_ai_agent 一一对应
INFO   alun_web::app: 检测到 215 条 #[permission] 规则 + 0 条配置规则 → 已启用权限校验层
INFO   alun_web::app: cloudpivot 启动 -> http://127.0.0.1:8023
```

> 自检逻辑（`shared/startup_guard.rs::check_ai_agent_invariant`）**一行未改**：
> 本卡只是验证它在新增的 632xx 硬拒之后仍然生效。

---

## 四、单测与集成测试（验收 11）

### 4.1 新增单测（卡面点名三类，逐条对应）

| 卡面要求 | 用例 | 位置 |
|----------|------|------|
| ROBOT 登录拦截 | `ai_actor::tests::robot_is_blocked_with_expected_codes`（63201/403/文案）+ `human_is_never_blocked` + `user_type_match_is_case_sensitive` | `src/shared/ai_actor.rs` |
| 删除拦截 | 同上第 2 条（63202/403/文案）；码值稳定性由 `error_code::tests::ai_executor_error_codes_are_stable` 钉住 | `src/shared/ai_actor.rs` + `src/shared/error_code.rs` |
| operator_type 写入 | `record_writes_robot_operator_type` / `record_writes_human_operator_type` / `operator_type_of_defaults` / `explicit_operator_type_skips_lookup` / `new_change_log_carries_operator_type` | `tests/batchT_t5_ai_rules.rs` |

反作弊设计：ROBOT 与 HUMAN **成对**断言（只测 ROBOT 的话，「写死 ROBOT」也能绿）；
解析器对 `None` / 不存在的账号断言回落到 `HUMAN`（日志是事后凭证，不因账号消失而失败）。

### 4.2 `cargo test -j 1`（带真库 env，库 `cloudpivot_t5v`）

```
test result: ok. 614 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out   (lib 单测)
test result: ok. 1 passed;  0 failed   (batchB_b2_scope)
test result: ok. 1 passed;  0 failed   (batchB_b7_mutex)
test result: ok. 1 passed;  0 failed   (batchC_c3_dispatch)
test result: ok. 11 passed; 0 failed   (batchC_c4_help)
test result: ok. 8 passed;  0 failed   (batchC_c5_changelog)
test result: ok. 4 passed;  0 failed   (batchC_c6_changelog)
test result: ok. 5 passed;  0 failed   (batchT_t1_task)
test result: ok. 6 passed;  0 failed   (batchT_t5_ai_rules)      ← 本卡新增
test result: ok. 7 passed;  0 failed   (impexp_worker)
test result: ok. 1 passed;  0 failed   (pref_preference)
test result: ok. 2 passed;  0 failed   (s3_minio)
```

无库环境（`cargo test` 的 CI 形态）仍全绿：6 条真库用例打印
`SKIP: 未设置 T5_DB_URL / NIK_DATABASE_*（不连真实数据库）` 后返回。

```
SKIP: 未设置 T5_DB_URL / NIK_DATABASE_*（不连真实数据库）   ×5
test result: ok. 6 passed; 0 failed
```

---

## 五、`scripts/check.sh`（验收 12）

```
==> 后端：cargo fmt --check
==> 后端：cargo clippy -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 15.20s
==> 后端：cargo check
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.97s
==> 前端：pnpm typecheck

> @cloudpivot/web@0.1.0 typecheck /opt/cloudpivot/frontend
> vue-tsc --noEmit
==> 批次B 闸①：分页必须走 scope::paginate
  PASS：0 处（分页全部走 scope::paginate）
==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
  PASS：扫描范围已覆盖 backend/src/{business/task,business,system,support,shared}
  PASS：0 处
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> C7 G1：只有 business/changelog/ 能碰 sys_change_log        PASS：0 处
==> C7 G2：changelog 模块内禁止自开事务                        PASS：0 处
==> C7 G3：record 的签名必须带 &impl SqlExecutor               PASS
==> C7 G4：不得存在「无 executor 版本」的 record 函数           PASS
==> C7 G5：record/record_in 调用点必须在 transaction 闭包内
G5 PASS：21 个 record/record_in 调用点全部在 transaction 闭包内且传 &tx（扫了 163 个文件）
==> C7 G6：按 id 查 sys_change_log 必须带 created_at            PASS：0 处
==> 偏好字典与色板一致性                                       PASS：22 套色板逐字一致
==> 全部检查通过
```

补充（C7 经验：`--features test-hooks` 是独立编译组合，check.sh 不覆盖）：

```
cargo clippy --all-targets --features test-hooks -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.98s
```

> 为什么 changelog 的 `operator_type_of` 能过闸②：该函数是「按主键读一列的账号类型」，
> 不涉及行级可见性，已在 `model.rs` 里显式写
> `// SCOPE-EXEMPT: 按主键读 sys_user.user_type（单列账号类型，不涉及行级可见性，非列表/导出查询）`。
> 读 `sys_user` 的**目录级**限定没有放松：闸② 仍是 0 处。

---

## 六、§6.2 部署验收（生产）

| 步骤 | 结果 |
|------|------|
| `cargo build --release` | 成功（46.82 s）；`backend/target/release/cloudpivot`，sha256 `68e9cfa5d3ad397bbdac37462877ff5c8ddcbef5f0fa5ffa82fbbd64fb974363` |
| `systemctl restart cloudpivot` | `active`；启动日志「应用 1 个迁移」「cloudpivot 启动 -> http://127.0.0.1:8023」 |
| 生产库迁移 | `schema_migrations` 含 `111`；`operator_type varchar(16) NOT NULL DEFAULT HUMAN`；部分索引 1；**分区同构列 3**（2026_10/11/12） |
| **存量行语义** | 迁移前的 13 条历史日志行**全部**取默认值 `HUMAN`（`存量日志行 operator_type 分布 → HUMAN:13`） |
| 二进制含新逻辑 | `grep -ac 'AI 用户不能登录网页'` → 1；`grep -ac 'AI 用户不能修改数据权限'` → 1（`strings` 只认 ASCII，中文须用 `grep -a`） |
| 接口冒烟 | 匿名 `GET /api/business/changelog/page` → **401**；空体登录 → **400** |
| 人类账号生产登录（验收 2 生产侧） | `admin` → `HTTP 200 code=0 msg=ok has_access_token=True user=admin`；nginx 首页/登录页均 **200** |
| **生产库上的功能实测** | 造 `t5_probe`（ROBOT）探针 → `POST /api/system/auth/login`（**admin 的正确口令**）→ `403 {"code":63201}`；`auth_logs` 记下 `FAIL / AI 用户禁止网页登录`；再以**错误口令**登录仍 `63201` → 判定确在验密之前 |
| 收尾 | 探针账号 / `sys_ai_agent` 行 / 探针自己的 `auth_logs` 全部删除，残留计数 `0 / 0 / 0`；生产未留任何业务夹具 |

---

## 七、迁移 `V0111` 空库 up→down→up（云枢迁移纪律）

脚本：`/root/t5run/verify_migration.sh`（应用账号 `cloudpivot` 执行，与启动迁移同口径）。

```
== 0) 应用启动时已把空库推进到 V0111 ==
schema_migrations 含 111 → 1
operator_type: character varying(16) nullable=NO default='HUMAN'::character varying
CHECK 约束 → 1
部分索引 → 1

== 1) down（DROP 索引 + 约束 + 列）==
down 后 operator_type 列数 → 0
down 后 CHECK 数 → 0
down 后 部分索引数 → 0
down 后 分区残留 operator_type 列数 → 0

== 2) up（重新建列 + 约束 + 索引，含自检 DO 块）==
NOTICE:  V0111 自检通过：operator_type varchar(16) NOT NULL DEFAULT HUMAN + CHECK + 部分索引
up 后 operator_type: character varying(16) nullable=NO default='HUMAN'::character varying
up 后 分区同构列数（应为分区数 3）→ 3
up 后 部分索引（父表+分区）→ 1

== 3) 幂等：再跑一次 up（只打 NOTICE，不报错）==
    NOTICE:  column "operator_type" of relation "sys_change_log" already exists, skipping
    NOTICE:  relation "idx_sys_change_log_ai_operator" already exists, skipping
    NOTICE:  V0111 自检通过：…

== 5) 分区表约束继承（子分区上有同名 CHECK）==
分区上的 CHECK 数 → 3
```

> 分区表上的 `ADD COLUMN` / `CREATE INDEX` / `DROP` 在父表执行会级联到全部现存分区与
> 将来分区，因此迁移只操作父表；上面第 2、5 行就是这条结论的实测。

---

## 八、设计说明（评审最容易问的四点）

### 8.1 为什么判定收在 `shared/ai_actor.rs`，而不是各 handler 各写一遍

四个入口分散在 `system/auth` / `business/task` / `business/permission` / `business/aiagent`。
各写一遍 `user_type == "ROBOT"` 的结果必然是「有的地方漏、有的地方文案不同、有的地方
写成 400」。收成**纯函数**（入参是 `&str`，不碰 DB）后：① 四个入口各调一次，漏不掉；
② 单测不需要数据库；③ 文案只有一份。

### 8.2 为什么分派**没有** 632xx 码（修正 11 的红线）

`632xx` 段只有 4 个码，稳定性单测里断言 `codes.len() == 4` 并附言
「给分派加硬拒码违反修正 11」。分派 / 协作人只查权限码
（`business:task:assign` / `business:task:member`），验收 5（403 63002）与验收 6（200 成功）
就是这条设计的两侧证据。

### 8.3 为什么 `operator_type` 由 `record()` 自己查库，而不是让调用方传

changelog 有 21 个 `record` 调用点（`check.sh` G5 实测）。让每个调用点自己填 `ROBOT`，
漏一个就在审计里显示成「人」。改成「默认按 `operator_id` 在**同一业务事务**里
`SELECT user_type FROM sys_user WHERE id = ?`」：① 一处生效、全局正确；② 读的是事务快照，
日志记的类型与业务写入时的账号类型一致；③ 代价是一次主键查找，且放在「空 diff 短路」之后，
没有实际变化的写不付这次查询。`with_operator_type` 留给「操作人已不在库里」的极端场景。

### 8.4 为什么 63016 保留而不删除

码值一旦发给前端就不回收（前端可能缓存过文案映射，历史日志里也带着它）。
`63016 TASK_ROBOT_FORBIDDEN` 保留常量与值，文档标注「已由 63202 取代，不再返回」；
稳定性单测里仍钉住 `63016`，并额外断言 `AI_TASK_DELETE_FORBIDDEN != TASK_ROBOT_FORBIDDEN`。

---

## 九、清理与残留

- 验收实例 `127.0.0.1:18081`（库 `cloudpivot_t5v`）与迁移验证库 `cloudpivot_t5mig` 用完即删；
- 隔离编译目录 `/root/t5-target`、运行目录 `/root/t5run` 用完即删；
- 可复现脚本已入库：
  - `scripts/batchc/t5_e2e.py`（真机 HTTP 验收，幂等，`seed` / `all` / `clean` 三档）；
  - `scripts/batchc/t5_verify_migration.sh`（V0111 空库 up→down→up）；
  - 生产探针用的两个一次性脚本（造 ROBOT 探针 → 登录 → 清理）**不入库**：它们直接改生产库，
    入库容易被误执行；其输出已原样贴进 §2.1 与 §6；
- 生产库只留结构性变更（V0111），无业务夹具残留（§6 末行）；
- 一过性临时文件（本地 `t5work/`）不进入仓库。

---

## 十、产物指纹

| 项 | 值 |
|---|---|
| 功能提交 | `f8ab6ff` — `feat(ai): T5 AI 执行者硬拒规则（632xx）+ changelog operator_type` |
| 提交内容 | 15 files changed, 1134 insertions(+), 24 deletions(-) |
| 三方一致 | `HEAD` == `origin/main` == `bare main` == `f8ab6ff8b6108bfaf2414a9501782ea35cb563c5` |
| 生产后端产物 | `backend/target/release/cloudpivot`，sha256 `68e9cfa5d3ad397bbdac37462877ff5c8ddcbef5f0fa5ffa82fbbd64fb974363` |
| 生产库 | `schema_migrations` 含 `111`；`sys_change_log.operator_type` + CHECK + 部分索引 |
| 前端 | **本卡不含前端改动**：`operator_type` 已进 4 个查询出口（列表 / 详情 / 时间线 / 导出），「AI」徽章由 T6 页面卡消费 |

---

## 十一、遗留与边界（诚实声明）

1. **AI 用户的令牌发放属 T5b**：本卡只保证「登录被拒」，没有实现 API Key / 任务令牌。
   验收里用的令牌是「先以 HUMAN 登录取合法 JWT，再把 `user_type` 翻成 ROBOT」，
   与「AI 持有长期令牌」在服务端判定上等价（四个硬拒都读库里的 `user_type`），
   但**不是** T5b 的任务令牌链路。
2. **`operator_type` 不区分 `SYSTEM`**：系统任务（`operator_id = None`）落 `HUMAN`，
   来源由 `trace_source = SYSTEM/JOB` 表达。若将来要在列表里单独筛「系统」，需要扩 CHECK 与枚举。
3. **前端标签未验证**：本卡只保证接口返回 `operator_type`（HUMAN/ROBOT），
   「AI」徽章的渲染属 T6，验收 8c 只到 API 层。
4. **任务 `update` 接口不写 changelog**（T1/T3 的既有边界）：只有状态流转 / 分派写。
   所以验收 8/9 选了「改状态」作为 AI / 人类的对照写操作，未改动该边界。
