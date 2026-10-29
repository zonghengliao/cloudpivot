# 批次 C+ · T1 验证证据（工作任务管理：7 表 + CRUD + 可见性 scope∪参与）

- 任务卡：`t_852858cd`（批次 C+ · T1）
- 依据：`docs/批次C+-工作任务管理-核心设计.md`（v2，老大 2026-10-29 批准）§二 / §三 / §七 / §十二 / §十三
- 验证机：`172.18.10.144`（Ubuntu 26.04 / PG 18.6）+ 自建验证库 `cloudpivot_t1m`（迁移往返）/ `cloudpivot_t1v`（真库 + 接口走查）
- 验证时间：2026-10-29（CST）
- 验证方式：`backend/tests/batchT_t1_task.rs`（真库 5 用例）+ 真服务 curl 走查（29 条断言）+ 真库 EXPLAIN + `cargo test` 全量 + `scripts/check.sh`

> **2026-10-29 返工更新（任务卡 `t_4c09fcc7`）**：可见性谓词已从「4 列 OR + `EXISTS`」改为
> **参与 id 数组**（`AND ( (1 = 1 {FILTER}) OR t.id = ANY(?::text::uuid[]) )`，架构决策 D6/D7），
> 20 万/5 万规模实测 `Seq Scan on biz_task` → `BitmapOr`（166 ms → 17 ms）。
> 本文 §5/§7 的**四场景可见性结论仍然有效且逐条复验一致**（1 / 2 / 1 / 3 / 5、协作人 404 + `63001`），
> 但谓词形态、新增迁移 `V0104__task_creator_index` 与 EXPLAIN 对比请看
> **`docs/批次C+-T1-返工-可见性id数组-验证证据.md`**（supersedes 本文关于谓词写法的描述）。

> 下面所有输出都是本次真跑的原文（脚本在 `scripts/verify/`，可原样复现），没有手工修饰。

---

## 0. 一句话结论

| 验收项（票面） | 结果 | 证据 |
|---|---|---|
| 1. 迁移：空库全量跑通 + `down` 回滚 + `up→down→up` 可重放 | ✅ 7 表 / 21 索引 / 回滚全 0 / 重放全回 | §2 |
| 2. CRUD：创建（默认值正确）→ 详情 → 编辑 → 软删 → 列表不含已删 | ✅（29 条断言全通过） | §3、§4 |
| 3. 可见性四场景（a 员工 / b 主管 / c 跨部门协作人 / d 超管） | ✅ 1 / 2 / 1 / 5，且协作人看不到同部门其它任务 | §5、§6 |
| 4. `task_no` 连续创建 5 条不重复且递增 | ✅ `TSK-202610-000001..000005`，软删不复用（跳号到 000007） | §7 |
| 5. 索引生效（我的任务走 `(owner_id,status,due_date)`；逾期走部分索引） | ✅ Bitmap Index Scan `idx_biz_task_owner_status_due` / Index Scan `idx_biz_task_due_open` | §8 |
| 6. `cargo test` 不回归（521+）+ 可见性四场景单测 | ✅ lib **541** + 集成 **41** = **582 passed / 0 failed**（新增 20 个单测 + 5 个真库用例） | §9 |
| 7. `scripts/check.sh` 通过（B 闸①/② = 0/0 + C7 G1–G6） | ✅ 闸①=0 闸②=0；G1–G6 全 PASS；fmt/clippy/check/typecheck 全过 | §10 |

---

## 1. 交付物清单

### 1.1 新增文件

| 文件 | 职责 |
|---|---|
| `backend/migrations/business/V0100__create_task_tables.{up,down}.sql` | **7 张表**一次建完（设计文档 §十二 逐字：`biz_task` / `biz_task_member` / `biz_task_comment` / `biz_task_attachment` / `biz_task_dependency` / `biz_task_worklog` / `biz_project`） |
| `backend/migrations/business/V0101__task_indexes.{up,down}.sql` | 12 条索引，含**部分索引** `idx_biz_task_due_open`（`WHERE status NOT IN ('DONE','CANCELLED')`） |
| `backend/migrations/system/V0102__add_sys_user_type.{up,down}.sql` | 底座改动：`sys_user.user_type`（`HUMAN`/`ROBOT`，默认 HUMAN，老大已批准的 1 处） |
| `backend/migrations/business/V0103__seed_task_permissions.{up,down}.sql` | `business:task:*` 5 个菜单节点（…0760–…0764） |
| `backend/migrations/seed/S__task_permissions.sql` | 权限**授权**（授给所有未软删角色）+ 自检；放种子里是因为版本化迁移先于角色种子执行（§2.4 有实测依据） |
| `backend/src/business/task/{mod,dto,model,handler}.rs` | 模块四件套：6 个接口 + 可见性谓词 + 编号生成 + 20 个单测 |
| `backend/tests/batchT_t1_task.rs` | 真库集成测试 5 用例（可见性四场景 / CRUD / 编号 / 结构与索引 / 我的任务分组 + 逾期扫描） |
| `scripts/verify/t1_task_migrate_verify.sh` | 迁移往返取证（空库 up → down → up） |
| `scripts/verify/t1_task_run_tests.sh` | 建验证库 + 迁移 + 跑真库集成测试 |
| `scripts/verify/t1_task_api_fixture.sql` | 接口走查夹具（跨部门组织 + 4 个账号） |
| `scripts/verify/t1_task_api_verify.sh` | 真服务 curl 走查（29 条断言，自带夹具与起停） |
| `scripts/verify/t1_task_explain.sql` | 5 万行真实分布 + EXPLAIN 取证 |

### 1.2 修改文件（每处都有理由）

| 文件 | 改动 | 为什么必须改 |
|---|---|---|
| `backend/src/business/mod.rs` | `pub mod task;` + 模块表一行 | 新模块注册（路由由 `App::scan()` 自动收集，无需改 router） |
| `backend/src/shared/error_code.rs` | 新增 **63xxx** 段 9 个码 + 1 条码表自检 | 62xxx 段的既有单测硬断言「24 个码 / 62001–62024」是冻结契约，往里塞要改那条「不得改动」的测试；任务错误语义独立成段更干净 |

**没有动的红线**：`business/shared/scope.rs` 一行未改（B 批门禁核心）；批次 A/B/C 的模块一个未改。

---

## 2. 迁移验收（空库 up → down → up）

命令：`bash scripts/verify/t1_task_migrate_verify.sh`（脚本内 DROP + CREATE 空库 `cloudpivot_t1m`）

### 2.1 空库全量 up

```
0) 建空库 cloudpivot_t1m（DROP + CREATE，确保是空库）
库已创建；当前业务表数 = 0

1) 全量 up（跑 V0001..V0103 + 种子）
2026-10-29T.. INFO alun_web::app: 数据库迁移完成: 应用 61 个迁移

1.1) 7 张表
     table_name
---------------------
 biz_project
 biz_task
 biz_task_attachment
 biz_task_comment
 biz_task_dependency
 biz_task_member
 biz_task_worklog
(7 rows)

1.2) 索引（含部分索引）
           indexname
-------------------------------
 biz_project_pkey
 biz_task_attachment_pkey
 biz_task_comment_pkey
 biz_task_member_pkey
 biz_task_pkey
 biz_task_worklog_pkey
 idx_biz_project_dept
 idx_biz_task_assigner_status
 idx_biz_task_attachment_task
 idx_biz_task_comment_task
 idx_biz_task_dept_status
 idx_biz_task_due_open
 idx_biz_task_member_user
 idx_biz_task_owner_status_due
 idx_biz_task_parent
 idx_biz_task_project
 idx_biz_task_worklog_task
 pk_biz_task_dependency
 uk_biz_task_attachment
 uk_biz_task_member
 uk_biz_task_task_no
(21 rows)
```

部分索引谓词（PG 折叠后的形态，与 `model::scan_overdue` 的 WHERE 逐字对应）：

```
CREATE INDEX idx_biz_task_due_open ON public.biz_task USING btree (due_date)
  WHERE ((status)::text <> ALL ((ARRAY['DONE'::character varying, 'CANCELLED'::character varying])::text[]))
```

底座改动 + 权限种子：

```
1.4) sys_user.user_type
 column_name |     data_type     |     column_default
-------------+-------------------+-------------------------
 user_type   | character varying | 'HUMAN'::character varying
存量行 user_type 分布：HUMAN=1

1.5) 权限种子（5 个菜单节点 + 全员授权）
 20000000-...-000000000760 | 工作任务             | 2 | business:task:read   | /task | business/task/index
 20000000-...-000000000761 | business:task:read   | 3 | business:task:read
 20000000-...-000000000762 | business:task:create | 3 | business:task:create
 20000000-...-000000000763 | business:task:update | 3 | business:task:update
 20000000-...-000000000764 | business:task:delete | 3 | business:task:delete
未软删角色数 = 1
缺 business:task:* 授权的角色数（应为 0）= 0
```

### 2.2 down 回滚（V0103 → V0102 → V0101 → V0100，逆序）

```
--- 执行 backend/migrations/business/V0103__seed_task_permissions.down.sql ---
  OK
--- 执行 backend/migrations/system/V0102__add_sys_user_type.down.sql ---
  OK
--- 执行 backend/migrations/business/V0101__task_indexes.down.sql ---
  OK
--- 执行 backend/migrations/business/V0100__create_task_tables.down.sql ---
  OK
已清除 4 条版本化迁移记录 + 1 条种子记录（task_permissions）

2.1) 回滚后残留检查（全部应为 0）
biz_* 表数            = 0
biz_* 索引数          = 0
sys_user.user_type 列 = 0
任务菜单节点数        = 0
任务菜单授权行数      = 0
本卡迁移记录数        = 0
```

### 2.3 再 up 重放

```
3) 再 up 重放（同一空结构库，验证可重放）
迁移已应用 migration=task_permissions statements=2 elapsed_ms=1
INFO alun_web::app: 数据库迁移完成: 应用 5 个迁移
biz_* 表数            = 7
biz_* 索引数          = 21
sys_user.user_type 列 = 1
任务菜单节点数        = 5
本卡迁移记录数        = 4
```

### 2.4 一个必须记下来的坑：**授权不能写在版本化迁移里**

第一版把「授给所有角色」写进了 `V0103`，空库验证时发现 `缺授权的角色数 = 1`：版本化迁移
**先**跑、种子（`S__role_data.sql`）**后**跑，全新库里 V0103 执行时 `sys_role` 是空表（
`schema_migrations.applied_seq` 最后两条是 `menu_data=60` / `role_data=61`）。

于是改成：**节点**留在 `V0103`（版本化，跟表一起回滚），**授权**移到种子
`migrations/seed/S__task_permissions.sql`（按文件名排在 `role_data` 之后执行）。
改完重跑，`缺授权的角色数 = 0`。这个缺口只在**全新环境**复现 —— 老库升级永远看不到它。

---

## 3. CRUD 实测（真服务 + 真登录，脚本 29 条断言全通过）

命令：`bash scripts/verify/t1_task_api_verify.sh`（自起服务 127.0.0.1:8027，库 `cloudpivot_t1v`）

### 3.1 创建（默认值证据）

```json
POST /api/business/task  {"title":"T1走查-员工自己的任务","owner_id":"7d00...0001"}
{
    "code": 0,
    "msg": "ok",
    "data": {
        "id": "01a185f9-0173-76db-a808-9e8b4d11442c",
        "task_no": "TSK-202610-000001",
        "status": "TODO",
        "priority": "P2",
        "progress": 0,
        "due_date": "2026-10-30",
        "owner_id": "7d000000-0000-0000-0000-000000000001"
    }
}
5 条编号：TSK-202610-000001 TSK-202610-000002 TSK-202610-000003 TSK-202610-000004 TSK-202610-000005
  PASS  编号不重复：5
  PASS  第 1 条状态默认 TODO：TODO
  PASS  第 1 条优先级默认 P2：P2
  PASS  第 1 条进度默认 0：0
  PASS  第 1 条截止日默认本周五：2026-10-30
```

默认值逐条对应设计文档 §十：负责人=自己（传 `owner_id` 即分派）/ 状态 `TODO` / 优先级 `P2` /
截止=本周五（2026-10-29 是周四 → 10-30）/ 需验收=关（详情里 `need_review=False`）。

### 3.2 详情（含部门名 / 来源 / 协作人）

```
4) 详情 GET /business/task/{id}
  task_no = TSK-202610-000001
  title = T1走查-员工自己的任务
  status = TODO
  priority = P2
  progress = 0
  need_review = False
  owner_name = T1员工（研发一组）
  creator_name = 超级管理员
  dept_name = T1研发一组            ← dept_id 快照 = 负责人所在部门
  due_date = 2026-10-30
  source = ASSIGNED                 ← 创建人≠负责人 → 记为「被分派」
  parent_id = None
  approval_instance_id = None       ← 批次 D 预留，本批恒为 NULL
  children = 0  members = 1  attachments = 0   ← members=1：分派时创建人自动成为协作人
  PASS  详情部门名（普通员工=研发一组）：T1研发一组
  PASS  详情子任务数：0
```

### 3.3 编辑（负责人本人）+ 显式 null 清空

```
5) PUT /business/task/{id}（t1_emp 是负责人 → 允许）
{"title":"T1走查-员工自己的任务（已改）","priority":"P0","need_review":true,"due_date":null}
{ "code": 0, "data": { "task_no": "TSK-202610-000001", "status": "TODO", "priority": "P0",
                       "progress": 0, "due_date": null, ... } }
  PASS  编辑后标题：T1走查-员工自己的任务（已改）
  PASS  编辑后优先级：P0
  PASS  显式 null 清空截止日：None
```

### 3.4 协作人**看得见但改不了**

```
6.1) 编辑越权：t1_cross 不是负责人/创建人 → 403
{ "code": 63002, "msg": "只有任务的负责人或创建人可以编辑" }
  PASS  协作人编辑被拒 HTTP 业务码：63002
```

### 3.5 软删 → 列表不含已删

```
8) 软删除 DELETE /business/task/{id}（admin）
{ "code": 0, "data": { "id": "01a185f9-...", "deleted": true } }
  PASS  删除回执 deleted：True
  PASS  删除后 admin 分页 total：4
  PASS  删除后 t1_other 分页 total（原 3 条剩 2 条）：2
  PASS  已删任务详情 404 码：63001
```

### 3.6 筛选（状态 / 优先级 / 关键词 / 负责人 / 非法值）

```
  PASS  status=DOING（当前无进行中）：0
  PASS  priority=P1（只第 4 条）：1
  PASS  keyword=跨部门：1
  PASS  owner=t1_other（删了 1 条后剩 2 条）：2
  PASS  status=BAD（非法值 → 400）：400
```

### 3.7 我的任务（五分组）

```
9) GET /business/task/my（t1_emp）
  counts = {"overdue": 0, "today": 0, "this_week": 0, "later": 0, "no_due": 1}  total = 1
    [no_due   ] TSK-202610-000001  due=None  T1走查-员工自己的任务（已改）
```

（该条在第 5 步被显式清空了截止日，所以落在「无期限」桶 —— 顺手验证了分组是按 `due_date` 实时算的。）

---

## 4. 真库集成测试（`cargo test --test batchT_t1_task`）

命令：`bash scripts/verify/t1_task_run_tests.sh`（建库 → 迁移 → `TASK_DB_URL=… cargo test`）

```
running 5 tests
test crud_roundtrip ... ok
test my_buckets_and_overdue_scan ... ok
test schema_and_indexes_exist ... ok
test task_no_is_unique_and_increasing ... ok
test visibility_four_scenarios ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.47s
```

各用例实测输出（原文）：

```
[CRUD] 新建：task_no=TSK-202610-000006 status=TODO progress=0 owner=7c000000-...-0001
       dept=Some(7e000000-...-0002) due=Some(2026-10-30)
[CRUD] 编辑后：title=T1自测-CRUD-改后标题 priority=P0 need_review=true due=Some(2026-11-05)

[我的任务] [(01a185f9-22ab-…, Overdue), (01a185f9-22ad-…, Today), (01a185f9-22ae-…, ThisWeek), (01a185f9-22b0-…, NoDue)]
[逾期扫描] 命中 1 条：["TSK-202610-000006"]

[结构] 部分索引谓词：((status)::text <> ALL ((ARRAY['DONE'::character varying, 'CANCELLED'::character varying])::text[]))
[结构] sys_user.user_type 默认值：'HUMAN'::character varying
[结构] 自依赖被拒绝：check constraint violated (constraint `ck_biz_task_dependency_not_self`)

[编号] 连续创建 5 条：["TSK-202610-000006", "TSK-202610-000007", "TSK-202610-000008", "TSK-202610-000009", "TSK-202610-000010"]
[编号] 软删最后一条后再建：TSK-202610-000012（软删前最大 10）
```

`task_no` 断言细节：尾段数字严格递增、5 条互不重复、格式 `TSK-YYYYMM-NNNNNN`、
**软删后新建跳过被删的号**（10 → 12，不复用 11）。

---

## 5. 可见性四场景（模型层，真库）

```
test visibility_four_scenarios
[a] 普通员工可见 1 条：[01a185f9-234e-743d-8b7f-86d0ab1d9bf5]
[b] 部门主管可见 2 条：[01a185f9-…, 01a185f9-234e-…]
[c] 跨部门协作人可见 1 条：[01a185f9-…]
[d] 超管可见 …（等于库里未软删任务总数）
```

夹具组织：研发部（根）→ 研发一组（子部门）、市场部（与研发部平级）；
5 条任务：`t1t_emp`（研发一组）/ `t1t_mgr`（研发部）/ `t1t_other`（市场部 ×3，其中一条把
`t1t_cross` 拉成协作人）。断言逐条：

| 场景 | 断言 | 结果 |
|---|---|---|
| a) 普通员工（`data_scope=4`） | 只看到自己的 1 条；看不到同部门主管的任务 | ✅ |
| b) 部门主管（`data_scope=3`） | 看到研发部 **+ 研发一组** 共 2 条；看不到市场部任何任务 | ✅ |
| c) 跨部门协作人（`data_scope=4`） | 看到被拉进的**那一条**；看不到同部门其它 2 条 | ✅ |
| d) 超管（`SUPER_ADMIN`） | 条数 = 库里未软删任务总数 | ✅ |

---

## 6. 可见性四场景（HTTP 层，真服务 + 真登录）

同一批数据，5 个身份各打一次 `GET /api/business/task/page`：

```
--- a) 普通员工 t1_emp（data_scope=4 仅本人） ---
    TSK-202610-000001  owner=T1员工（研发一组）        T1走查-员工自己的任务（已改）
  total = 1
--- b) 部门主管 t1_mgr（data_scope=3 本部门及以下） ---
    TSK-202610-000001  owner=T1员工（研发一组）        T1走查-员工自己的任务（已改）
    TSK-202610-000002  owner=T1主管（研发部）         T1走查-主管的任务
  total = 2
--- c) 跨部门协作人 t1_cross（data_scope=4，被拉进 TSK-202610-000004） ---
    TSK-202610-000004  owner=T1市场同事（市场部）       T1走查-跨部门协作
  total = 1
--- c') 市场部同事 t1_other（同部门，但不是协作人） ---
    TSK-202610-000003  owner=T1市场同事（市场部）       T1走查-市场部无协作
    TSK-202610-000004  owner=T1市场同事（市场部）       T1走查-跨部门协作
    TSK-202610-000005  owner=T1市场同事（市场部）       T1走查-市场部另一条
  total = 3
--- d) 超管 admin（data_scope=1 全部） ---
    TSK-202610-000001 … TSK-202610-000005
  total = 5
  PASS  a) 普通员工只看到自己的：1
  PASS  b) 部门主管看到本部门及下级（研发一组 + 研发部）：2
  PASS  c) 跨部门协作人只看到被拉进的那一条：1
  PASS  c') 同部门同事看到自己名下 3 条（不含协作那条以外）：3
  PASS  d) 超管看到全部：5
```

**c) 这一条是本卡的核心**：`t1_cross` 属于市场部、数据范围「仅本人」，按 `scope` 他一条都不该看到；
因为被拉进 `TSK-202610-000004`，他能看到**那一条**，而同部门的 `…000003` / `…000005` 依然看不到
（`c')` 与 `c)` 的对照证明这不是「什么都能看」）。

详情同样守可见性（看不见 → **404 而不是 403**，避免任务 ID 变成可枚举清单）：

```
  PASS  协作人查 TSK-202610-000004 详情 code：0
{ "code": 63001, "msg": "任务不存在或已删除" }
  PASS  协作人查 TSK-202610-000003 详情的业务码：63001
```

### 6.1 谓词怎么拼的（为什么没动 `scope.rs`）

```sql
WHERE t.deleted_at IS NULL
  AND ( (1 = 1 {FILTER})                    -- paginate 把 ` AND <scope 条件>` 原位注入
        OR t.owner_id   = ?::uuid
        OR t.creator_id = ?::uuid
        OR t.assigner_id = ?::uuid
        OR EXISTS (SELECT 1 FROM biz_task_member m
                    WHERE m.task_id = t.id AND m.user_id = ?::uuid) )
```

片段语义不变（仍是 `paginate` 生成、仍由它做 count + 分页），只是被包进了 OR 组的左半边，
于是 `(1 = 1 AND <scope>) OR 参与关系` = 设计文档 §七 的并集。
前提「片段一定以 ` AND ` 开头」由 `model::assert_scope_fragment` **运行时校验**：
若将来 `scope.rs` 改成生成 `OR …`，这里立刻 `500`（而不是静默变成全表可见）。
`business/shared/scope.rs` 一行未改，B 闸①/② 仍 0/0。

---

## 7. `task_no` 生成（并发安全）

`TSK-{YYYYMM}-{NNNNNN}`，月内递增、软删不复用。实现：

1. `pg_advisory_xact_lock(7700000000 + YYYYMM)` —— 事务级咨询锁，按月串行化，提交即释放；
2. `max(substring(task_no from 12)::bigint) + 1`（含软删行，故不复用）+ `substring(...) ~ '^[0-9]+$'` 防脏数据；
3. 与 INSERT 同事务。

真机证据见 §3.1（5 条连续编号）与 §4（软删后跳号 10 → 12）。

---

## 8. 索引生效（5 万行真实分布 EXPLAIN）

命令：`su postgres -c "psql -d cloudpivot_t1v -f scripts/verify/t1_task_explain.sql"`

数据分布（一条 SQL 灌 5 万行；**owner 与 status 用互质模数 3/10 分开分配**，
第一版按同模数分配导致「t1_emp 名下全是 DONE」，`我的任务` 根本走不到 owner 索引 —— 计划里出现的是 due_open）：

```
  status   | count        k                   | count
-----------+-------      --------------------+-------
 CANCELLED |  5000       overdue(未终态且过期) |  9996
 DOING     |  5000       open(未终态)          | 15005
 DONE      | 30000       total                | 50005
 REVIEW    |  5000       t1_emp 名下          | 16667
 TODO      |  5000       t1_emp 名下未终态    |  5001
```

### 8.1 「我的任务」→ `(owner_id, status, due_date)`

```
EXPLAIN 1：model::my_tasks 的 SQL 逐字
 Sort  (cost=44629.24..44641.84 rows=5040) (actual rows=5001.00 loops=1)
   Sort Key: t.due_date, t.created_at DESC, t.id DESC
   ->  Nested Loop Left Join
         ->  Bitmap Heap Scan on biz_task t
               Recheck Cond: ((owner_id = '7d00…0001'::uuid) AND ((status)::text = ANY ('{TODO,DOING,REVIEW}'::text[])))
               Filter: (deleted_at IS NULL)
               ->  Bitmap Index Scan on idx_biz_task_owner_status_due  (actual rows=10001.00 loops=1)
                     Index Cond: ((owner_id = '7d00…0001'::uuid) AND ((status)::text = ANY ('{TODO,DOING,REVIEW}'::text[])))
 Execution Time: 8.617 ms
```

Index Cond 同时含 `owner_id` 与 `status` —— 就是设计文档 §十二 要求的 `(owner_id, status, due_date)`。
（8.6 ms 里约 5 ms 是 `child_count` 相关子查询逐行取子任务数，不是索引问题；同形状只 select id 时 2.2 ms，见 §8.2。）

### 8.2 同一查询用 `NOT IN` 的对照（解释为什么代码里写 `IN`）

```
EXPLAIN 1-b：把 WHERE 改回 status NOT IN ('DONE','CANCELLED')
   ->  Bitmap Heap Scan on biz_task t
         Filter: ((deleted_at IS NULL) AND (owner_id = '7d00…0001'::uuid))
         Rows Removed by Filter: 10004
         ->  Bitmap Index Scan on idx_biz_task_due_open  (actual rows=30005.00 loops=1)
 Execution Time: 3.577 ms   （本行只 select id，故不能与 §8.1 的 8.617 ms 直接比）
```

`NOT IN` 与部分索引 `idx_biz_task_due_open` 的谓词逐字相同，PG 选了它并把 `owner_id` 降级成 Filter
（扫 3 万条索引项 + 过滤掉 1 万行）。所以 `model::my_tasks` 用 `OPEN_STATES`（`TODO/DOING/REVIEW`）
这个等价写法 —— 换来 Index Cond 真正压到 (owner_id, status) 上。

同一列数（只 `select t.id`）的 A/B 对照（另跑的 `EXPLAIN` 三连，见 `model.rs` `OPEN_STATES` 文档里的表）：

| 写法 | 计划 | 耗时 |
|---|---|---|
| `status NOT IN ('DONE','CANCELLED')` | Bitmap Index Scan on `idx_biz_task_due_open` + Filter(owner_id)，4998 行被过滤 | 6.795 ms |
| `status IN ('TODO','DOING','REVIEW')` | Bitmap Index Scan on `idx_biz_task_owner_status_due`，Index Cond 含 owner_id + status | **2.179 ms** |

### 8.3 逾期扫描 → 部分索引 `idx_biz_task_due_open`

```
EXPLAIN 2：model::scan_overdue 的 SQL 逐字
 Limit  (actual rows=50.00 loops=1)
   ->  Result
         ->  Incremental Sort  (Sort Key: t.due_date, t.id)
               ->  Nested Loop Left Join
                     ->  Index Scan using idx_biz_task_due_open on biz_task t  (actual rows=556.00 loops=1)
                           Index Cond: (due_date < CURRENT_DATE)
                           Filter: (deleted_at IS NULL)
 Execution Time: 0.412 ms
```

部分索引生效（`Index Scan using idx_biz_task_due_open`，0.412 ms 取前 50 条）。

---

## 9. `cargo test` 全量（不回归）

```
running 541 tests   → test result: ok. 541 passed; 0 failed
集成测试：1 + 1 + 1 + 11 + 8 + 4 + 5 + 7 + 1 + 2 = 41 passed（7 ignored：需要 MinIO 等外部依赖的既有用例）
合计 582 passed / 0 failed
```

- lib 541 = 本卡新增 **20** 个单测（dto 5 / model 9 / handler 5 / error_code 1）+ 既有 521（票面基线 521+ ✅）；
- 新增真库用例 **5** 个（`tests/batchT_t1_task.rs`），可见性四场景各有专属断言；
- `cargo clippy --all-targets -- -D warnings` 0 warning。

---

## 10. `scripts/check.sh`（门禁）

```
==> 后端：cargo fmt --check            （无输出 = 通过）
==> 后端：cargo clippy -- -D warnings  Finished dev profile
==> 后端：cargo check                  Finished dev profile
==> 前端：pnpm typecheck               vue-tsc --noEmit（通过）
==> 批次B 闸①：分页必须走 scope::paginate
  PASS：0 处（分页全部走 scope::paginate）
==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
  PASS：0 处
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> C7 G1 … G6
  PASS G1 0 处 / G2 0 处 / G3 PASS / G4 PASS
  G5 PASS：19 个 record/record_in 调用点全部在 transaction 闭包内且传 &tx（扫了 155 个文件）
  PASS G6 0 处
==> C7 门禁汇总：G1–G6 共 6 条，未通过 0 条
==> 偏好字典与色板一致性（V0091 ui.palette oneof ↔ style/palettes.ts）
  PASS：22 套色板与 V0091 的 ui.palette oneof 逐字一致
==> 全部检查通过
```

---

## 11. 本卡刻意**没做**的事（边界，避免被误判为遗漏）

| 项 | 归属 | 说明 |
|---|---|---|
| 状态机 8 条迁移 / 进度自动联动 | T2 | 本卡 `PUT` **不接受** `status` / `progress` / `done_at` / 取消原因 / 打回原因 —— 否则 T2 的「8 条规则」会与一条后门并存 |
| 分派 / 转派 / 加协作人接口 | T3 | 本卡「改负责人」只直接改字段（+ 刷新 `dept_id` 快照）；转派要「原负责人变协作人 + 通知 + `source=TRANSFERRED`」，属 T3。走查里的「拉协作人」是 SQL 直插 `biz_task_member` |
| 依赖接口 / 循环检测 / 阻塞校验 | T4 | 表与反自反 CHECK 已建（§4 有自依赖被拒的证据），接口与算法属 T4 |
| AI 执行者（`user_type=ROBOT` 禁登录 / 专用 API） | T5 | 本卡只加列（`V0102`），不加任何登录闸门逻辑 |
| 前端 4 页 + 甘特图 | T6 / T7 | 权限节点里 `component = business/task/index` 已就位，前端页面交付后即可点开 |
| 变更日志（`sys_change_log`） | T2 随状态机一起接 | `changelog/diff.rs` 的 `REGISTRY` 是批次 C 模块（本卡红线不动）；未登记的目标表写入会报 `62012`。T2 落状态迁移留痕时一并登记 |
| 附件上传接口 | T3 | 本卡只建 `biz_task_attachment` 表；文件本体由 `business/storage` 管 |
| 管理员的 PUT 权限 | 票面口径 | 票面写「编辑仅负责人/创建人」，故**超管也不能编辑**（删除才有超管分支）。若后续需要「管理员强改」，按 T3 的管理入口补，不要在这里开口子 |

## 12. 工作区状态

```
cd /opt/cloudpivot && git rev-parse HEAD origin/main   两行一致（见 summary）
git status --short                                     空
```

---

## 附：复现命令

```bash
# 1) 迁移往返（空库 up → down → up）
bash /opt/cloudpivot/scripts/verify/t1_task_migrate_verify.sh

# 2) 真库集成测试（建 cloudpivot_t1v + 迁移 + 5 个用例）
bash /opt/cloudpivot/scripts/verify/t1_task_run_tests.sh

# 3) 接口走查（自带夹具与起停，29 条断言）
bash /opt/cloudpivot/scripts/verify/t1_task_api_verify.sh

# 4) 索引取证（5 万行 + EXPLAIN，任何时刻都可跑；跑完再跑第 2 步会自动清掉 BULK-* 批量行）
su postgres -c "psql -d cloudpivot_t1v -f /opt/cloudpivot/scripts/verify/t1_task_explain.sql"

# 5) 门禁
cd /opt/cloudpivot && bash scripts/check.sh
```
