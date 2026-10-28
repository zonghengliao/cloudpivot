# 批次 B · DDL 与迁移落地说明（B1）

> 范围：**V0060–V0064 + V0067**（6 个版本、12 个 up/down 文件），全部落在
> `backend/migrations/business/`。
> 依据：`docs/批次B-技术架构.md` 附录 A（DDL 原文，已在 PG 18.6 实跑）。
> 验收证据（空库迁移、down 往返、约束语义实测）：`docs/批次B-B0B1-验证证据.md`。
>
> **本批不做**：`V0065`（批次B 权限点种子）与 `V0066`（默认脱敏规则种子）—— 它们属 B4/B6
> 的实施步范围，等到那两步落地时再补。号段 `V0053–V0059` 保持空缺（留给批次 A 的线上修复迁移）。

---

## 1. 迁移清单

| 版本 | 文件名 | 内容 | 回滚文件 | 回滚动作 |
|---|---|---|---|---|
| V0060 | `V0060__alter_sys_role_add_data_scope` | `sys_role` 加 `data_scope SMALLINT NOT NULL DEFAULT 1` + `ck_sys_role_data_scope CHECK (1..5)` + 列注释 | `.down.sql` | `DROP CONSTRAINT` + `DROP COLUMN` |
| V0061 | `V0061__create_sys_role_dept` | 建 `sys_role_dept`（`role_id`/`dept_id` 双 FK + `UNIQUE(role_id, dept_id)`）+ 2 索引 | `.down.sql` | `DROP TABLE` |
| V0062 | `V0062__create_sys_field_mask` | 建 `sys_field_mask` + `NULLS NOT DISTINCT` 唯一索引 + 标识符 `CHECK` + 1 索引 | `.down.sql` | `DROP TABLE` |
| V0063 | `V0063__create_sys_role_mutex` | 建 `sys_role_mutex` + `LEAST/GREATEST` 对称唯一索引 + 反自反 `CHECK` | `.down.sql` | `DROP TABLE` |
| V0064 | `V0064__batchb_indexes` | 4 条 `CREATE INDEX IF NOT EXISTS`（实际新增 3 条，见 §2.6） | `.down.sql` | `DROP INDEX` ×3 |
| V0067 | `V0067__alter_export_task_add_mask_applied` | `sys_export_task` 加 `mask_applied BOOLEAN NOT NULL DEFAULT FALSE` + 列注释 | `.down.sql` | `DROP COLUMN` |

**服务端版本**：PostgreSQL 18.6（`V0062` 的 `NULLS NOT DISTINCT` 要求 PG ≥ 15；`V0063`/`V0064`
的表达式索引与部分索引要求 PG ≥ 9.5）。

---

## 2. 逐条 DDL（附录 A 原文）与 down 说明

> 每节的 SQL 与架构文档附录 A **逐字一致**（除每个文件顶部补了「版本 / 依据 / 回滚」头注释，
> 与仓库既有迁移风格对齐）。下面为便于评审而重排，实际文件以
> `backend/migrations/business/V00xx__*.sql` 为准。

### 2.1 V0060 — `alter_sys_role_add_data_scope`

```sql
ALTER TABLE sys_role
    ADD COLUMN IF NOT EXISTS data_scope SMALLINT NOT NULL DEFAULT 1;

ALTER TABLE sys_role
    ADD CONSTRAINT ck_sys_role_data_scope CHECK (data_scope IN (1, 2, 3, 4, 5));

COMMENT ON COLUMN sys_role.data_scope IS
    '数据范围：1 全部 / 2 本部门 / 3 本部门及以下 / 4 仅本人 / 5 自定义部门集合';
```

- **默认值 1（全部）是刻意的**：迁移前不存在数据权限概念，所有角色的实际可见范围都是「全部」。
  默认 1 = 迁移后可见范围与迁移前逐行一致，不产生「无预告的数据消失」。
- **不建索引**：`data_scope` 只在解析当前用户 scope 时按 `role_id` 读取（走主键），不参与过滤条件。
- **`.down.sql`**：先 `DROP CONSTRAINT IF EXISTS ck_sys_role_data_scope`，再 `DROP COLUMN IF EXISTS data_scope`。
  顺序刻意「先约束后列」：语义可审计，且未来若该列被其它对象引用时，报错信息会是依赖对象而非
  「cannot drop column」的笼统提示。数据影响：所有角色的范围配置丢失（回到无数据权限概念的 V0059）。
- **注意**：`ADD CONSTRAINT` 没有 `IF NOT EXISTS`（PG 不支持，PG 18 只有 `ADD COLUMN IF NOT EXISTS`）。
  因此 up 只能在 down 之后重跑 —— 往返测试（回滚 6 步再 `run`）已验证这条链是通的。

### 2.2 V0061 — `create_sys_role_dept`

```sql
CREATE TABLE sys_role_dept (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    role_id UUID NOT NULL REFERENCES sys_role(id),
    dept_id UUID NOT NULL REFERENCES sys_dept(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    UNIQUE (role_id, dept_id)
);
-- 4 条 COMMENT ON TABLE/COLUMN
CREATE INDEX idx_sys_role_dept_role ON sys_role_dept (role_id);
CREATE INDEX idx_sys_role_dept_dept ON sys_role_dept (dept_id);
```

- 双外键与 `sys_user_role`（V0003）同风格：这是**授权数据**，指向不存在的部门就是脏授权。
  `sys_dept` 是软删表，FK 不阻塞正常删部门（软删只是 `UPDATE deleted_at`）。
- 切换 `data_scope` 时**不清空**本表（来回切换不丢配置）；读取时只在 `data_scope = 5` 才取。
- **`.down.sql`**：`DROP TABLE IF EXISTS sys_role_dept`。两个索引与 `UNIQUE` 约束随表消失，不需要
  单独 `DROP INDEX`。数据影响：所有角色的自定义部门集合丢失（`data_scope=5` 的角色回滚后可见部门集合为空）。

### 2.3 V0062 — `create_sys_field_mask`

```sql
CREATE TABLE sys_field_mask (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    table_name VARCHAR(64) NOT NULL,
    column_name VARCHAR(64) NOT NULL,
    mask_type SMALLINT NOT NULL,
    mask_param VARCHAR(128),
    role_id UUID REFERENCES sys_role(id),
    status SMALLINT NOT NULL DEFAULT 1,
    reason VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID, update_by UUID, deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_field_mask_type CHECK (mask_type BETWEEN 1 AND 7),
    CONSTRAINT ck_sys_field_mask_ident CHECK (
        table_name ~ '^[a-z][a-z0-9_]{1,63}$' AND column_name ~ '^[a-z][a-z0-9_]{1,63}$')
);
-- 5 条 COMMENT
CREATE UNIQUE INDEX uk_sys_field_mask_rule
    ON sys_field_mask (table_name, column_name, role_id) NULLS NOT DISTINCT
    WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_field_mask_table
    ON sys_field_mask (table_name) WHERE deleted_at IS NULL;
```

- 三个设计点：① `NULLS NOT DISTINCT` 让 `role_id IS NULL` 的全局规则也受唯一约束（普通唯一索引下
  NULL 互不相等，会存进多条互相覆盖的全局规则）；② 标识符 `CHECK` 是本表唯一可能影响 SQL 标识符的
  字段的防线；③ `role_id` 内嵌在规则表（一条规则只属「全局」或「一个角色」，多角色用户按 OR 命中 ——
  脱敏是限制，取并集才安全）。
- **`.down.sql`**：`DROP TABLE IF EXISTS sys_field_mask`（唯一/普通索引随之消失）。
  数据影响：**全部脱敏规则丢失**。回滚方必须知情：若 B6 的「导出 fail-closed」已上线，删表会让敏感列
  导出返回 422 `FIELD_MASK_RULE_MISSING`（这是刻意的保守行为）；正确顺序是「先回滚 B6 代码、再回滚本迁移」。

### 2.4 V0063 — `create_sys_role_mutex`

```sql
CREATE TABLE sys_role_mutex (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    role_a_id UUID NOT NULL REFERENCES sys_role(id),
    role_b_id UUID NOT NULL REFERENCES sys_role(id),
    reason VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_role_mutex_distinct CHECK (role_a_id <> role_b_id)
);
CREATE UNIQUE INDEX uk_sys_role_mutex_pair
    ON sys_role_mutex (LEAST(role_a_id, role_b_id), GREATEST(role_a_id, role_b_id))
    WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_role_mutex_b ON sys_role_mutex (role_b_id) WHERE deleted_at IS NULL;
```

- 互斥是**对称**关系，(A,B) 与 (B,A) 必须落到同一个键 —— 由表达式唯一索引在库层保证（不靠应用层自觉）。
- **`.down.sql`**：`DROP TABLE IF EXISTS sys_role_mutex`。数据影响：**全部互斥组与理由留痕丢失**
  （合规数据，回滚前必须 `COPY ... TO` 导出）。回滚后若 B7 代码仍在，`ensure_no_mutex` 查不到任何互斥对
  = 互斥静默失效 → 顺序是「先回滚 B7 代码、再回滚本迁移」。

### 2.5 V0067 — `alter_export_task_add_mask_applied`

```sql
ALTER TABLE sys_export_task
    ADD COLUMN IF NOT EXISTS mask_applied BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN sys_export_task.mask_applied IS
    '产物是否已应用列级脱敏（TRUE = 安全，FALSE = 请求者持 field-mask:plain 权限导出明文）';
```

- 默认 **FALSE** 是刻意的：历史导出产物确实未经脱敏，默认 FALSE 让审计口径与事实一致，
  而不是给存量数据贴一个「安全」的假标签。
- **`.down.sql`**：`DROP COLUMN IF EXISTS mask_applied`。数据影响：审计标记丢失；
  若 B6 代码仍在，写入该列会直接 500（`column does not exist`）→ 同样先回滚代码。

### 2.6 V0064 — `batchb_indexes`（含一处必须说明的偏差）

```sql
CREATE INDEX IF NOT EXISTS idx_sys_user_dept_created
    ON sys_user (dept_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_sys_import_task_create_by
    ON sys_import_task (create_by) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_sys_export_task_create_by
    ON sys_export_task (create_by) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_sys_role_mutex_b
    ON sys_role_mutex (role_b_id) WHERE deleted_at IS NULL;
```

**偏差一：`idx_sys_role_mutex_b` 是重复定义（`IF NOT EXISTS` 的空操作）。**
它已由 V0063 建（同名、同定义）。保留它是为了与附录 A 原文逐字一致（便于逐条对照评审）；
**相应地把 down 改成只删本迁移独占的 3 个索引** —— 否则「回滚 V0064、保留 V0063」的库会丢掉
V0063 声明的索引，库内不自洽。这是本卡唯一偏离附录 A 的地方，理由是「可逆性优先于逐字复制」。

**偏差二：附录 A 的注释提到「若目标表 > 100 万行则三条 `CREATE INDEX` 改 `CONCURRENTLY`」。**
本卡实测目标环境行数（见 §3），远低于阈值，因此**保持普通 `CREATE INDEX`**。

**`.down.sql`**：`DROP INDEX IF EXISTS` × 3（`idx_sys_user_dept_created` /
`idx_sys_import_task_create_by` / `idx_sys_export_task_create_by`）+ 一行显式注释说明
「`idx_sys_role_mutex_b` 属 V0063，本回滚不删」。索引是纯派生对象，删了不丢数据、可安全重复执行。

---

## 3. `V0064` 索引并发性判断（验收第 3 条）

**结论：当前环境不需要 `CONCURRENTLY`，保持普通 `CREATE INDEX`。**

判断依据（2026-10-28 实测，`pg_class.reltuples` + 精确 `count(*)`）：

| 库 | `sys_user` | `sys_import_task` | `sys_export_task` |
|---|---|---|---|
| `cloudpivot`（prod） | 2 | 0 | 0 |
| `cloudpivot_test` | 34 | 4 | 1 |
| `cloudpivot_b`（本卡新建的空库） | 迁移后 0 行业务数据（仅种子角色/管理员） | 0 | 0 |

- 阈值判断：部分索引 `WHERE deleted_at IS NULL` 在 10 万行级别以下建索引是**毫秒级**，`ACCESS EXCLUSIVE`
  只在建索引的瞬间持有（PG 对普通 `CREATE INDEX` 在本表上禁止并发写，但表容量决定时长）。
- 现实规模距 100 万行阈值差 5 个数量级，因此**没有**改成 `CONCURRENTLY` 的必要；反过来，
  `CONCURRENTLY` 有明显代价：① 不可在事务内执行（要把整个文件改成 `-- +no-transaction`，
  失败后留下 `INVALID` 索引且需手工 `DROP`）；② 迁移失败不再整体回滚，可逆性变差。
- **将来确实超阈值时的切换方法**（框架 `alun-db` 0.4.2 支持 `-- +no-transaction` 指令，
  见 `alun-db-0.4.2/src/migrate/splitter.rs::parse_directives`）：

  ```sql
  -- +no-transaction
  CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_sys_user_dept_created
      ON sys_user (dept_id, created_at DESC) WHERE deleted_at IS NULL;
  ```

  改完必须重跑「回滚 6 步 → 再 `run`」往返一次（本卡验收第 2 条的同一套命令），
  并在 `schema_migrations` 里确认该版本 `success = t`。

---

## 4. 与架构文档 §5.2 的一处口径冲突（以附录 A 为准）

架构文档 §5.2 的表格把 V0064 的内容写成 `idx_sys_user_dept_created` / `idx_sys_role_dept_dept` /
`idx_sys_field_mask_table` / `idx_sys_role_mutex_b`；而**附录 A.5 的 V0064 原文**是
`idx_sys_user_dept_created` / `idx_sys_import_task_create_by` / `idx_sys_export_task_create_by` /
`idx_sys_role_mutex_b`。

- §5.2 那一行把 V0061/V0062 里已经建好的索引重复列了一遍（`idx_sys_role_dept_dept`、
  `idx_sys_field_mask_table`），按它落地会得到「重复创建」的报错（无 `IF NOT EXISTS`）。
- **本卡以附录 A.5 为准**：它是「已在 PG 18.6 实跑过」的原文，且任务书注明「按附录 A 原文落地」。
- 建议架构文档下次修订时删掉 §5.2 那一行的旧清单，或改成「见附录 A.5」。

---

## 5. B0 门禁基线（`scripts/check.sh`）

`scripts/check.sh` 末尾新增「批次B 闸① / 闸②」两条断言（架构文档 §2.4 的闸 2），
**放在 fmt / clippy / check / typecheck 之后**：这两条在 B5 完成前必然失败，放最后是为了不让一次
门禁失败遮掉前四项的结果。

| 门禁 | 口径 | B0 实测（本卡） | 架构文档记载 | B5 完成后 |
|---|---|---|---|---|
| 闸① | 直接调 `paginate_with_params` 的处数（白名单 `business/shared/scope.rs` + 注释行不计） | **20** | 20 | 0 |
| 闸② | 登记表查询既无 `scope::` 也无 `// SCOPE-EXEMPT` 的 file×table 处数 | **21** | 17 | 只剩带理由的豁免 |

**闸① 与架构记载完全一致（20 处，逐行对应）。闸② 多了 4 处**，逐条差异与理由见
`docs/批次B-B0B1-验证证据.md` §3 —— 结论：不是口径放宽，而是架构文档那次基线的打印清单漏了
4 个 file×table 组合（`support/log/model.rs`×`operation_logs`、`support/file/model.rs`×`sys_file`、
`support/dashboard/model.rs`×`operation_logs`、×`auth_logs`），而附录 C 的 #9/#11/#24 本来就要求处理它们。

**施工期逃生口**：B2–B4 期间（门禁必然红）用 `BATCHB_GATE=warn bash scripts/check.sh`，
两条门禁降级为告警、不拦提交；B5 完成后默认行为即为全绿。

---

## 6. 验收命令索引

详见 `docs/批次B-B0B1-验证证据.md`，命令一览：

```bash
# 1) 空库全量迁移（全新库 cloudpivot_b；prod 库 cloudpivot 绝不在命令里出现）
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_b -f backend/tests/sql/batchb/verify_up.sql

# 2) down 可逆（框架 Migrator 的 rollback，LIFO）→ 再 run 回到最新
#    工具：临时 example（验收后已删除，源码与理由见 docs/批次B-B0B1-验证证据.md 附录 A）
/var/tmp/b1_migrate.sh rollback 6
/var/tmp/b1_migrate.sh run

# 3) 约束语义（对称唯一 / NULLS NOT DISTINCT / 反自反 / 标识符 CHECK / data_scope CHECK）
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_b -f backend/tests/sql/batchb/verify_constraints.sql

# 4) 门禁基线
bash scripts/check.sh          # 期望：闸① 20、闸② 21，退出码 1
```

---

## 7. 回滚顺序总则（生产回滚前必读）

```
先回滚使用这些结构的代码（B6 脱敏 / B7 互斥 / B3 数据权限），再回滚迁移
```

理由：`mask_applied` 被删后写入该列会 500；`sys_role_mutex` 被删后互斥校验静默失效（比报错更危险）；
`sys_field_mask` 被删后「导出 fail-closed」会把所有敏感列导出打成 422。三类后果里只有第一类是「报错」，
另两类是**静默错误** —— 所以顺序不能反。

数据保全：`sys_role_mutex.reason`、`sys_field_mask.reason`、`sys_role_dept` 的集合都是**配置/合规数据**，
回滚前 `COPY <表> TO STDOUT WITH CSV HEADER > rollback-backup-<date>.csv` 留档。
