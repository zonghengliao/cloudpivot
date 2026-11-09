# ARCH-9-2 人力资源模块 · DDL 真库校验证据

卡片：`t_c8e263a6`（ARCH9-2【L-3】人力资源模块设计）
设计文档：`docs/architecture/ARCH-9-人力资源模块设计.md`
校验日期：2026-11-10
目标库：`cloudpivot_test`（`PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_test`）

## 怎么复现（两条命令，RC 必须都是 0）

```bash
cd /opt/cloudpivot

# ① 10 张新表：BEGIN + CREATE SCHEMA arch92_hr + 全量 DDL + 45 条断言 + ROLLBACK
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_test -v ON_ERROR_STOP=1 \
  -f docs/evidence/2026-11-10-arch9-2-hr-ddl/01_new_tables_check.sql

# ② 改造表 / 种子：BEGIN + 科目/号段/字典/菜单/角色/脱敏/配置/凭证规则/流程 + 24 条断言 + ROLLBACK
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_test -v ON_ERROR_STOP=1 \
  -f docs/evidence/2026-11-10-arch9-2-hr-ddl/02_alter_tables_check.sql
```

预期输出（`.out` 是本次实跑全文）：

```text
01_new_tables.out   →  NOTICE:  ARCH-9-2 新表断言通过 45 条
02_alter_tables.out →  NOTICE:  本库无 biz_doc_attachment：放宽段按幂等守卫跳过（生产库会执行）
                       NOTICE:  ARCH-9-2 改造表/种子断言通过 24 条
```

## 为什么用「事务 + ROLLBACK」而不是建库

单查询 headless 模式会把 `CREATE DATABASE` / `DROP DATABASE` 判为 dangerous 直接拦掉（无人可批）。
PG 的 DDL 是事务性的，`BEGIN … ROLLBACK` 之后 schema、表、种子全部消失（实测残留复核见下），
等价于"在真库上编译了一遍 DDL"。

## 残留复核（跑完后自查，四条全 0）

```sql
SELECT count(*) FROM information_schema.schemata WHERE schema_name='arch92_hr';          -- 0
SELECT count(*) FROM biz_account WHERE account_code IN ('2211.01','5602.10','2221.03');  -- 0
SELECT count(*) FROM sys_menu    WHERE path LIKE '/business/hr%';                        -- 0
SELECT count(*) FROM biz_seq     WHERE seq_type IN ('EMP','LV','ATT','SAL');              -- 0
```

## 断言是怎么设计的（成对 + 条数锁死）

- **成对**：每个约束都同时验「该拒的拒」与「该放的放」。例：同公司重复工号被拒 / 不同公司同工号放行；
  同一 `user_id` 挂两档被拒 / `user_id IS NULL` 可以有多行；`gross ≠ net + withhold` 被拒 / 正常行放行。
- **条数锁死**：`DO` 块末尾 `IF ok <> 45 THEN RAISE EXCEPTION`（02 号脚本是 24）。
  有人删掉一条断言 → 条数不符 → 整块报错，不会出现"断言被删掉但脚本仍然绿"。
- **兼容分支**：`02` 的号段段用 `DO` + 动态 SQL，检测 `biz_seq` 是否有 `org_id` 列（ARCH-9-1 会加），
  两条分支都实跑过；附件 CHECK 放宽段检测 `biz_doc_attachment` 是否存在。

## 跑真库改掉的 3 个真错误（设计稿已改，别照抄旧稿）

1. **漏 3 个科目**：第一版只种 `2211.*/2221.03/2241.03/2241.04`，忘 `5602.10~12 管理费用-工资/社保/公积金`
   → `PAYROLL_ACCRUAL_DEFAULT` 规则借方科目解析为空 → 断言红（"2 条工资凭证规则未全部解析到科目（实际 1）"）。
2. **父科目翻末级只写了 `2211`**：`cloudpivot_test` 里 `2241`、`5602` 也是末级（该库没跑过 V0226），
   挂子科目后父仍末级 → 自相矛盾。改成"任何有未删子科目且当前是末级的父科目都翻"的条件 UPDATE。
3. **`sys_config` 两列类型写错**：`config_level` 是 `SMALLINT`（1 系统级 / 2 业务级），不是 `'SYSTEM'`；
   `config_type` 用 `STRING/BOOLEAN/NUMBER` 大写枚举。真库直接 `invalid input syntax for type smallint`。

## 未覆盖清单（别以为全验过）

| 未覆盖 | 原因 | 怎么补验 |
|---|---|---|
| `ck_biz_doc_attachment_type` 16 → 20 值放宽、`ck_biz_attachment_rule_doc_type` 同步、2 条必传规则 | `cloudpivot_test` **没有** `biz_doc_attachment` / `biz_attachment_rule` 两张表（本次只验到"守卫分支跳过"，打印 NOTICE） | 在 144/全量库或 `cp_full` 类库上跑一遍 V0383 |
| 4 套审批流程的**图快照与 sha256** | 需要 `flow::graph::snapshot()` 计算，属实现卡 HR-26/HR-38 | 按 `V0243` 做法：迁移头注释写 hash，末尾 `DO` 块复算比对 |
| 17 个薪资项种子 + 公式解析 | 属实现卡 HR-31~HR-35（本次只在 01 号脚本里验了 `biz_salary_item` 的约束语义） | HR-32 单测 + HR-35 迁移自检 |
| `sys_field_mask` 是否对 `biz_employee` **自动生效** | 需要起后端实例打接口 | HR-13 卡第一步实测（风险 R1） |

## 文件

| 文件 | 内容 |
|---|---|
| `01_new_tables_check.sql` | 10 张表 DDL（与设计文档 §3.1 逐字一致）+ 45 条断言 |
| `01_new_tables.out` | 实跑输出（RC=0） |
| `02_alter_tables_check.sql` | 科目/号段/字典/菜单/角色/脱敏/配置/凭证规则/流程/附件放宽 + 24 条断言 |
| `02_alter_tables.out` | 实跑输出（RC=0） |
