# 批次 B · B0 门禁基线 + B1 迁移落地 · 验证证据

> 卡片：`t_fe16eb1f`（批次 B · B0 + B1）｜执行人：manong｜日期：2026-10-28（CST）
> 工作副本：`172.18.10.144:/opt/cloudpivot`（bare：`/opt/git/cloudpivot.git`）
> 起始 HEAD：`cf84231`（`git merge --ff-only origin/main` 后）
> 交付文档：`docs/批次B-DDL.md`（DDL 原文 + 每条 down 说明 + 并发性判断）
>
> **红线自检**：本卡只在**测试库** `cloudpivot_b`（本卡新建）与 `cloudpivot_test` 上执行迁移验证；
> prod 库 `cloudpivot` 只做过两次**只读** `count(*)`（用于索引并发性判断），无任何 DDL/DML。

---

## 1. 空库全量迁移（验收 1）

```bash
# 建全新库（owner=cloudpivot，与服务账号一致）
su postgres -c "createdb -O cloudpivot cloudpivot_b"

# 以 profile=test 的配置启动服务（库名覆盖成 cloudpivot_b，端口 8026），框架启动期自动迁移
B1_DB=cloudpivot_b B1_LISTEN=127.0.0.1:8026 /var/tmp/b1_run.sh
curl -s http://127.0.0.1:8026/health
# {"code":0,"msg":"ok","data":{"status":"ok","app":"cloudpivot","profile":"test", ...}}
```

迁移记录（`schema_migrations`，`kind=versioned`）：

```
 version |   kind    |                name                | success
---------+-----------+------------------------------------+---------
 60      | versioned | alter_sys_role_add_data_scope      | t
 61      | versioned | create_sys_role_dept               | t
 62      | versioned | create_sys_field_mask              | t
 63      | versioned | create_sys_role_mutex              | t
 64      | versioned | batchb_indexes                     | t
 67      | versioned | alter_export_task_add_mask_applied | t
(6 rows)
```

结构探测（`tests/sql/batchb/verify_up.sql`）：

```
################ 1. sys_role：data_scope 列 + CHECK 约束 ################
 column_name | data_type | is_nullable | column_default
-------------+-----------+-------------+----------------
 data_scope  | smallint  | NO          | 1

        conname         |                    definition
------------------------+---------------------------------------------------
 ck_sys_role_data_scope | CHECK ((data_scope = ANY (ARRAY[1, 2, 3, 4, 5])))

--- 默认值验证：V0060 之前已存在的角色，data_scope 必须是 1（全部），不得有 NULL ---
 data_scope | roles
------------+-------
          1 |     1

 null_or_out_of_range
----------------------
                    0

################ 2. sys_field_mask：唯一索引 uk_sys_field_mask_rule ################
        indexname         |                                             indexdef
--------------------------+---------------------------------------------------------------------------------------------------
 idx_sys_field_mask_table | CREATE INDEX idx_sys_field_mask_table ON public.sys_field_mask USING btree (table_name) WHERE (deleted_at IS NULL)
 sys_field_mask_pkey      | CREATE UNIQUE INDEX sys_field_mask_pkey ON public.sys_field_mask USING btree (id)
 uk_sys_field_mask_rule   | CREATE UNIQUE INDEX uk_sys_field_mask_rule ON public.sys_field_mask USING btree (table_name, column_name, role_id) NULLS NOT DISTINCT WHERE (deleted_at IS NULL)

         conname         |                                                    definition
-------------------------+------------------------------------------------------------------------------------------------------------------
 ck_sys_field_mask_ident | CHECK ((((table_name)::text ~ '^[a-z][a-z0-9_]{1,63}$'::text) AND ((column_name)::text ~ '^[a-z][a-z0-9_]{1,63}$'::text)))
 ck_sys_field_mask_type  | CHECK (((mask_type >= 1) AND (mask_type <= 7)))

################ 3. sys_role_mutex：唯一索引 uk_sys_role_mutex_pair ################
       indexname        |                                                    indexdef
------------------------+----------------------------------------------------------------------------------------------------------------
 idx_sys_role_mutex_b   | CREATE INDEX idx_sys_role_mutex_b ON public.sys_role_mutex USING btree (role_b_id) WHERE (deleted_at IS NULL)
 sys_role_mutex_pkey    | CREATE UNIQUE INDEX sys_role_mutex_pkey ON public.sys_role_mutex USING btree (id)
 uk_sys_role_mutex_pair | CREATE UNIQUE INDEX uk_sys_role_mutex_pair ON public.sys_role_mutex USING btree (LEAST(role_a_id, role_b_id), GREATEST(role_a_id, role_b_id)) WHERE (deleted_at IS NULL)

          conname           |            definition
----------------------------+----------------------------------
 ck_sys_role_mutex_distinct | CHECK ((role_a_id <> role_b_id))

################ 4. sys_export_task：mask_applied 列 ################
 column_name  | data_type | is_nullable | column_default
--------------+-----------+-------------+----------------
 mask_applied | boolean   | NO          | false

################ 5. sys_role_dept：表 + 2 索引 + 唯一约束 ################
             indexname             |                                                   indexdef
-----------------------------------+--------------------------------------------------------------------------------------------------------------
 idx_sys_role_dept_dept            | CREATE INDEX idx_sys_role_dept_dept ON public.sys_role_dept USING btree (dept_id)
 idx_sys_role_dept_role            | CREATE INDEX idx_sys_role_dept_role ON public.sys_role_dept USING btree (role_id)
 sys_role_dept_pkey                | CREATE UNIQUE INDEX sys_role_dept_pkey ON public.sys_role_dept USING btree (id)
 sys_role_dept_role_id_dept_id_key | CREATE UNIQUE INDEX sys_role_dept_role_id_dept_id_key ON public.sys_role_dept USING btree (role_id, dept_id)

################ 6. V0064 的三个索引（+ V0063 的 idx_sys_role_mutex_b）################
    tablename    |           indexname
-----------------+-------------------------------
 sys_export_task | idx_sys_export_task_create_by
 sys_import_task | idx_sys_import_task_create_by
 sys_role_mutex  | idx_sys_role_mutex_b
 sys_user        | idx_sys_user_dept_created
(4 rows)
```

> 与验收书逐条对照：`\d sys_role` 有 `data_scope` + CHECK ✅；`\d sys_field_mask` 有
> `uk_sys_field_mask_rule` ✅；`\d sys_role_mutex` 有 `uk_sys_role_mutex_pair` ✅；
> `\d sys_export_task` 有 `mask_applied` ✅（用 `pg_indexes` / `pg_constraint` / `information_schema`
> 取同一信息，便于逐行比对，输出比交互式 `\d` 更可留档）。

---

## 2. down 可逆 + 往返 1 次（验收 2）

框架只在 `App::start()` 里自动向上迁移，没有暴露 rollback 的 CLI，因此用一个**临时** example
（`backend/examples/b1_migrate.rs`，验收后已删除、未提交；源码见本文件附录 A）调用框架同一套
`alun_db::Migrator`（同一来源目录 `migrations/`、同一默认 options）执行 `rollback`。

> **一个必须说明的操作**：`Migrator::rollback` 是「按 `applied_seq` LIFO 回滚最近一次成功的迁移」，
> 而种子迁移（`S__*.sql`，如 `role_data`/`menu_data`）**每次 `run` 都会重新应用**，其历史行永远排在
> `applied_seq` 最末位且没有 down 脚本。因此直接 rollback 会先撞到种子并报
> `migration 'role_data' has no down script`。做法是：先 `DELETE FROM schema_migrations WHERE kind='seed'`
> （种子是 rerunnable 迁移，下一次 `run` 会按 `S__` 语义重新应用，不改变任何结构状态），
> 再执行 6 次 `rollback` —— 每次落的都是真实的版本化迁移。

```
=== 回滚前 schema_migrations 尾部（按 applied_seq 倒序）===
42|seed||role_data
41|seed||menu_data
40|seed||dict_data
39|seed||config_data
38|versioned|67|alter_export_task_add_mask_applied
37|versioned|64|batchb_indexes
36|versioned|63|create_sys_role_mutex
35|versioned|62|create_sys_field_mask
DELETE 4                      ← 清掉 4 条 seed 历史行（rerunnable，下次 run 重新应用）

=== 6 次 rollback（真实 Migrator::rollback，LIFO）===
rollback #1 -> 已回滚版本 v67
rollback #2 -> 已回滚版本 v64
rollback #3 -> 已回滚版本 v63
rollback #4 -> 已回滚版本 v62
rollback #5 -> 已回滚版本 v61
rollback #6 -> 已回滚版本 v60

=== 回滚后 status ===
APPLIED  v52     seed_impexp_export_retry                   applied
versioned 已应用条数 = 32
对象探测: 新表=0/3  sys_role.data_scope=0  sys_export_task.mask_applied=0  uk_sys_field_mask_rule=0  uk_sys_role_mutex_pair=0
```

回滚到 V0059 状态的逐项断言（`tests/sql/batchb/verify_down.sql`）：

```
########## 回滚到 V0059 后的状态：以下每项都应为 0（不存在）##########
 new_tables_left | role_data_scope_left | role_data_scope_check_left | export_mask_applied_left | batchb_indexes_left
-----------------+----------------------+----------------------------+--------------------------+---------------------
               0 |                    0 |                          0 |                        0 |                   0

########## 迁移记录：V0060–V0064 / V0067 应一条不剩 ##########
 version | kind | name | success
---------+------+------+---------
(0 rows)

########## 存量结构未被误伤：这些必须还在 ##########
 sys_role_exists | sys_export_task_exists | v0050_heartbeat_at_kept | sys_role_role_code_kept
-----------------+------------------------+-------------------------+-------------------------
               1 |                      1 |                       1 |                       1
```

再 `run` 回到 V0067（往返 1 次）：

```
run 完成：本次应用 10 个迁移          ← 6 条版本化（V0060–V0064 + V0067）+ 4 条种子（rerunnable）

APPLIED  v67     alter_export_task_add_mask_applied         applied
versioned 已应用条数 = 38
对象探测: 新表=3/3  sys_role.data_scope=1  sys_export_task.mask_applied=1  uk_sys_field_mask_rule=1  uk_sys_role_mutex_pair=1
```

**结论**：up → down（6 步，回到 V0059）→ up（回到 V0067）完整往返 1 次成功；
三张新表、`data_scope`、`mask_applied`、5 条索引全部「建—删—重建」干净，
V0050 的 `heartbeat_at` 与 `sys_role.role_code` 未受牵连。

---

## 3. `scripts/check.sh` 门禁基线（验收 B0-3）

完整命令与输出（`bash scripts/check.sh`，退出码 **1**）：

```
==> 后端：cargo fmt --check
==> 后端：cargo clippy -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.10s
==> 后端：cargo check
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.36s
==> 前端：pnpm typecheck
> vue-tsc --noEmit
==> 批次B 闸①：分页必须走 scope::paginate（paginate_with_params 仅允许出现在白名单）
    白名单：business/shared/scope.rs（+ 注释行不计）
  FAIL：20 处直接调 paginate_with_params（基线 20 处；B5 完成后应为 0）
    backend/src/support/config/model.rs:447
    backend/src/support/dict/model.rs:175 / :297
    backend/src/support/file/model.rs:153
    backend/src/support/job/model.rs:256 / :314
    backend/src/support/log/model.rs:333 / :448
    backend/src/system/role/model.rs:138 / :204
    backend/src/system/user/model.rs:240
    backend/src/business/storage/model.rs:220 / :465 / :991
    backend/src/business/impexp/model.rs:455 / :549 / :913
    backend/src/business/notice/model.rs:556 / :670 / :830
  （实际输出逐行带完整路径与代码行，此处按文件聚合展示）
==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
    登记表：sys_user sys_dept sys_import_task sys_export_task sys_notice sys_file operation_logs audit_logs auth_logs security_logs
  FAIL：21 处（B0 实测基线 21 处；B5 完成后只应剩 SCOPE-EXEMPT 豁免）
    support/dashboard/model.rs 查询 sys_user / sys_dept / operation_logs / auth_logs
    system/auth/model.rs       查询 sys_user
    system/dept/model.rs       查询 sys_user / sys_dept
    system/role/model.rs       查询 sys_user
    system/user/model.rs       查询 sys_user
    business/notice/fanout.rs  查询 sys_user / sys_dept
    business/impexp/cleanup.rs 查询 sys_import_task / sys_export_task
    business/impexp/model.rs   查询 sys_import_task / sys_export_task
    business/notice/cleanup.rs 查询 sys_notice
    business/notice/model.rs   查询 sys_notice
    support/file/model.rs      查询 sys_file
    business/storage/cleanup.rs 查询 sys_file
    business/storage/model.rs  查询 sys_file
    support/log/model.rs       查询 operation_logs
==> 批次B 门禁：2 条未通过。修法（二选一）：
    ① 分页接口改成 scope::paginate；单条/导出查询用 scope::filter_for
    ② 确实不需要数据权限的查询，在函数上方写一行 // SCOPE-EXEMPT: <理由>
    B2–B4 施工期可临时用 BATCHB_GATE=warn bash scripts/check.sh 绕过本门禁。
```

**闸① 20 处与架构文档记载逐行一致**（同样的 20 个 file:line，文件分布
`impexp/model.rs`×3、`notice/model.rs`×3、`storage/model.rs`×3、`config/dict/job/log/file/role/user` 各 1–2）。

### 3.1 闸② 为什么是 21 而不是架构文档写的 17（4 处差异逐条交代）

架构文档 §2.4 的基线输出打印了 11 行、标注「共 17 条」。本次在 `cf84231` 树上用**同一套脚本口径**
（同样的 10 张登记表、同样的 `grep -rl "FROM $t"` + `scope::|SCOPE-EXEMPT` 判定）实测为 **21 处**。
差 4 处，全部是架构文档那次打印清单**漏掉**的 file×table 组合（不是新增代码导致，也不是口径放宽）：

| # | 漏掉的组合 | 为什么该算 | 对应附录 C |
|---|---|---|---|
| 1 | `support/log/model.rs` × `operation_logs` | 操作日志分页，`support/log/model.rs:333/:448` 两处直接分页 | #9（明确要求 B5 接入 `scope::paginate`） |
| 2 | `support/file/model.rs` × `sys_file` | 文件分页 `support/file/model.rs:153` | #11（明确要求 B5 接入，P1） |
| 3 | `support/dashboard/model.rs` × `operation_logs` | 工作台「最近操作」聚合计数（SQL 里带 `, scope` 日期 CTE，易被误看成已接入数据权限） | #24（豁免，但要写 `SCOPE-EXEMPT`） |
| 4 | `support/dashboard/model.rs` × `auth_logs` | 同上，登录统计聚合 | #24（同上） |

→ 因此 `scripts/check.sh` 里把闸② 基线常量写成 **21**（`BATCHB_GATE2_BASELINE=21`），
并在注释里指向本节的差异表；架构文档 §2.4 的 17 属于**测量清单不全**，建议下次修订时更正。

### 3.2 门禁不是哑的

- 两条门禁都**报出预期失败**（20 / 21），`check.sh` 退出码 1 —— 证明断言真的在读代码，而不是恒绿。
- 反证：把 `BATCHB_GATE=warn` 跑一遍，前四项检查（fmt/clippy/check/typecheck）全绿、门禁降级为告警
  且退出码 0，说明失败确实来自这两条断言而不是别的环节。

### 3.3 一处顺手修掉的既有格式债务（与前四项目检相关）

`cargo fmt -- --check` 在**本次改动之前的 HEAD（`cf84231`）上就是红的**：`00e1f90`（批次 A 遗留修复卡）
提交的 `backend/src/business/notice/model.rs:487` 有一段未格式化代码

```
-        vec![
-            TypeMapper::to_value(&max_attempts),
-            Value::from(*id),
-        ],
+        vec![TypeMapper::to_value(&max_attempts), Value::from(*id)],
```

复现：`git show HEAD:backend/src/business/notice/model.rs > /tmp/x.rs && rustfmt --edition 2024 --check /tmp/x.rs` → exit 1。
它会让 `scripts/check.sh` 在第 1 步就退出，**新加的门禁永远打印不出来**，因此本卡对该文件做了
`rustfmt`（纯空白、无语义改动，`git diff` = 1 insertion / 4 deletions），未触碰 `system/`、`support/`。

---

## 4. 约束语义实测（附加证据，超出验收书要求但直接对应 POC-6/7 的结论）

在 `cloudpivot_b` 上单事务 + `SAVEPOINT` 跑 `tests/sql/batchb/verify_constraints.sql`，结束 `ROLLBACK`：

```
--- A. NULLS NOT DISTINCT：role_id 为 NULL 的全局规则插两次，第 2 次必须被拒 ---
INSERT 0 1
ERROR:  duplicate key value violates unique constraint "uk_sys_field_mask_rule"
DETAIL:  Key (table_name, column_name, role_id)=(sys_user, mobile, null) already exists.
--- B. 标识符 CHECK：表名带分号注⼊必须被拒 ---
ERROR:  new row for relation "sys_field_mask" violates check constraint "ck_sys_field_mask_ident"
DETAIL:  Failing row contains (..., sys_user; DROP TABLE x, mobile, 1, null, null, 1, ...)
--- C. mask_type 越界（8）必须被拒 ---
ERROR:  new row for relation "sys_field_mask" violates check constraint "ck_sys_field_mask_type"
--- D. 互斥对称唯一：插 (A,B) 成功，反向 (B,A) 必须被拒 ---
INSERT 0 1
ERROR:  duplicate key value violates unique constraint "uk_sys_role_mutex_pair"
DETAIL:  Key (LEAST(role_a_id, role_b_id), GREATEST(role_a_id, role_b_id))=(...000a, ...000b) already exists.
--- E. 反自反 CHECK：A×A 必须被拒 ---
ERROR:  new row for relation "sys_role_mutex" violates check constraint "ck_sys_role_mutex_distinct"
--- F. data_scope CHECK：非法值 6 必须被拒 ---
ERROR:  new row for relation "sys_role" violates check constraint "ck_sys_role_data_scope"
--- G. 事务内落库结果（期望 mutex=1 行、mask=1 行，其余被拒）---
 mutex_rows | mask_rows
------------+-----------
          1 |         1
--- H. 合法 data_scope=5 必须成功（CHECK 不误伤）---
UPDATE 1
 role_code | data_scope
-----------+------------
 B1_TEST_A |          5
 B1_TEST_B |          1
ROLLBACK
########## 事务已回滚：测试数据不残留 ##########
 mutex_rows_after_rollback | mask_rows_after_rollback
---------------------------+--------------------------
                         0 |                        0
```

---

## 5. 索引并发性判断（验收 3）

见 `docs/批次B-DDL.md` §3。要点：prod `sys_user=2` / `sys_import_task=0` / `sys_export_task=0`，
test `34/4/1`，距 100 万行阈值 5 个数量级 → **保持普通 `CREATE INDEX`**；将来切换方法
（`-- +no-transaction` + `CREATE INDEX CONCURRENTLY`）与该做法的两个代价也写在同处。

---

## 6. 交付清单（本卡改动文件）

```
backend/migrations/business/V0060__alter_sys_role_add_data_scope.{up,down}.sql
backend/migrations/business/V0061__create_sys_role_dept.{up,down}.sql
backend/migrations/business/V0062__create_sys_field_mask.{up,down}.sql
backend/migrations/business/V0063__create_sys_role_mutex.{up,down}.sql
backend/migrations/business/V0064__batchb_indexes.{up,down}.sql
backend/migrations/business/V0067__alter_export_task_add_mask_applied.{up,down}.sql
backend/tests/sql/batchb/verify_up.sql            （验收 1 的断言脚本）
backend/tests/sql/batchb/verify_down.sql          （验收 2 的回滚断言）
backend/tests/sql/batchb/verify_constraints.sql   （§4 的约束语义实测）
backend/src/business/notice/model.rs              （仅 rustfmt：1 insertion / 4 deletions，见 §3.3）
scripts/check.sh                                  （新增批次B 闸①/闸②，见 §3）
docs/批次B-DDL.md                                 （DDL 原文 + down 说明 + 并发性判断）
docs/批次B-B0B1-验证证据.md                        （本文）
```

**未改动**：`system/`、`support/` 的 Rust 代码（`notice/model.rs` 属 `business/`）、`shared/`、`bootstrap.rs`、
`Cargo.toml`、前端全部。V0065/V0066 **不做**（属 B4/B6）。

---

## 附录 A：临时 rollback 工具源码（验收后已删除，未进仓库）

```rust
//! 临时验证工具（批次 B · B1 验收专用，**不提交进仓库**）：打印迁移状态 / 回滚 N 步 / 重跑到最新。
//! 用法（在 backend/ 目录下）：
//!   B1_DB_URL='postgres://user:pass@127.0.0.1:5432/cloudpivot_b' cargo run --example b1_migrate -- status
//!   B1_DB_URL='...' cargo run --example b1_migrate -- rollback 6
//!   B1_DB_URL='...' cargo run --example b1_migrate -- run
use alun::prelude::*;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("B1_DB_URL").map_err(|_| "缺少 B1_DB_URL 环境变量")?;
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "status".to_string());
    let arg = args.next().unwrap_or_default();

    let db = Db::connect(&url).await?;
    let migrator = Migrator::from_dir("migrations");   // 与 [database.migration] path 同源、同默认 options

    match cmd.as_str() {
        "status"        => { /* 打印已应用的版本化迁移 + 新表/列/索引探测 */ }
        "rollback"      => { /* 循环 N 次 migrator.rollback(&db) */ }
        "run"           => { /* migrator.run(&db) */ }
        other           => println!("未知子命令: {other}"),
    }
    Ok(())
}
```

> 为什么不留进仓库：它只是为了调用框架的 `rollback`（框架没有 CLI 暴露）。把它留在 `examples/`
> 会让 `cargo clippy --all-targets` 多背一个非产品目标的维护责任；rollback 的正确产品化位置是
> B9 的运维脚本（届时应做成 `scripts/` 下的独立二进制或复用 `cargo run -- profile=xxx` 的运维子命令）。

---

## 附录 B：一次性复跑（换成自己的库名即可）

```bash
# 0) 前置：库存在、服务账号有权限
su postgres -c "createdb -O cloudpivot cloudpivot_b2"

# 1) 空库迁移（启动即迁移，起完就 kill）
B1_DB=cloudpivot_b2 B1_LISTEN=127.0.0.1:8027 /var/tmp/b1_run.sh

# 2) 结构断言
export PGPASSWORD=$(grep -m1 '^ALUN_DATABASE_PASSWORD' /etc/cloudpivot/cloudpivot-test.env | cut -d= -f2-)
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_b2 -f backend/tests/sql/batchb/verify_up.sql

# 3) 约束语义（事务内，自动 ROLLBACK）
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_b2 -f backend/tests/sql/batchb/verify_constraints.sql

# 4) 门禁基线
bash scripts/check.sh     # 期望 闸①=20 闸②=21、exit 1
```

---

## 附录 C：交付指纹与同步核对（验收「收尾必须自检」）

```
功能提交      c4a8b54d49d28e729e880da18a0e017ae06fc9c9
              feat(batchB): B0 门禁基线 + B1 迁移 V0060–V0064/V0067
父提交        cf84231f09030f8861409540f83acb00d2b0be6e   (docs(batchB): 归档批次B权限增强设计文档)
              ↑ 本附录所在提交紧随功能提交之后（仅补指纹，不改代码/迁移），
                核对同步时以 `git rev-parse HEAD` 的最新值与 `git rev-parse origin/main` 比对。

工作副本 /opt/cloudpivot
  HEAD        = c4a8b54d49d28e729e880da18a0e017ae06fc9c9
  origin/main = c4a8b54d49d28e729e880da18a0e017ae06fc9c9
  IDENTICAL: YES

bare /opt/git/cloudpivot.git  main = c4a8b54d49d28e729e880da18a0e017ae06fc9c9
（`git push` 报 "Everything up-to-date" 是因为 .githooks/post-commit 已把提交自动推到 bare；
  随后 `git fetch origin && git merge --ff-only origin/main` 返回 "Already up to date"，
  三方（本地 HEAD / origin/main / bare main）逐字一致 —— 针对上一张卡「推了 bare 但工作副本没同步」
  的问题，本卡专门把这三条值一起打出来。）
```

**验收自检清单**

| 验收项 | 结果 | 证据位置 |
|---|---|---|
| 1 空库全量迁移（4 类对象） | ✅ 6 条迁移 success=t，`data_scope`/`uk_sys_field_mask_rule`/`uk_sys_role_mutex_pair`/`mask_applied` 全在 | §1 |
| 2 down 可逆（回 V0059 再回 V0067） | ✅ 往返 1 次，32 → 38 条已应用迁移 | §2 |
| 3 索引并发性判断 | ✅ 保持普通 `CREATE INDEX`（prod 2 行 / test 34 行 ≪ 100 万） | `docs/批次B-DDL.md` §3 |
| 4 `docs/批次B-DDL.md`（附录 A 原文 + down 说明） | ✅ 279 行，6 条迁移逐条给出 down 语义与顺序风险 | `docs/批次B-DDL.md` |
| B0 门禁在当前代码报预期失败 | ✅ 闸①=20（与架构一致）、闸②=21（差异 4 处已逐条交代），`check.sh` exit 1 | §3 |
| `cargo test` 全绿 | ✅ 258 passed / 0 failed（+ 7、+ 2 另两个 target，4 ignored 为既有） | 本卡运行日志 |
| 不动 prod | ✅ prod `cloudpivot` 只做过只读 `count(*)`；`data_scope` 列在 prod 仍不存在（=0） | §0 红线自检 |

