# 证据包 · 批量导入导出三页的数据范围核实（`t_e464a285`）

> 结论与完整论证：`docs/architecture/P0-FIX-2B-D-导入导出数据范围-核实与结论.md`
> 本目录只放**逐字输出**与**可复跑脚本**，不放结论。

## 一句话

导出内容查询**没有**逐模块注入数据范围（5 个导出定义只有 `system.user` 覆写了带 scope 的入口），
且这三张 ERP 单据表**本身没有 `dept_id`**、列表侧也登记为 `unscoped` ⇒ 导出与列表同口径；
`READ` 授权实测**不含** `export:create` / `import:confirm`，`FULL` 才含。
生产授权本卡**一行未改**。

## 环境

| 项 | 值 |
|---|---|
| 生产 | `172.18.10.144`，库 `cloudpivot`，服务 `systemctl cloudpivot`（`health=200`），代码 `7d96692` |
| 代码树（grep 证据） | 99 工作仓 `/opt/cloudpivot` @ `2b24753` |
| 隔离副本 | 144 上 `cloudpivot_impexp_iso1`（`pg_dump` 逻辑复制生产快照：`sys_menu`=329、`sys_role_menu`=976） |
| 生产写入 | **零**（本卡只跑 `SELECT` / `pg_dump` / `curl`） |

### 为什么不是 `createdb -T`（卡片建议的写法）

```
$ su -s /bin/bash postgres -c "createdb cloudpivot_impexp_iso -T cloudpivot"
createdb: error: database creation failed: ERROR:  source database "cloudpivot" is being accessed by other users
DETAIL:  There are 10 other sessions using the database.
```

生产 10 个活动连接 ⇒ `CREATE DATABASE … TEMPLATE` 不可用。改用等价逻辑复制：

```bash
ssh root@172.18.10.144 '
  set -a; . /etc/cloudpivot/cloudpivot.env; set +a; export PGPASSWORD="$NIK_DATABASE_PASSWORD"
  su -s /bin/bash postgres -c "createdb -O cloudpivot cloudpivot_impexp_iso1"
  pg_dump -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" \
          --no-owner --no-privileges --schema=public > /tmp/impexp_iso.sql
  psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d cloudpivot_impexp_iso1 -X -q -f /tmp/impexp_iso.sql
'
```

> 已知噪音：`pg_dump` 会带一行 `CREATE SCHEMA public`，在已初始化的库上必然报
> `ERROR: schema "public" already exists`（`ON_ERROR_STOP` 时为致命，故还原**不带** `ON_ERROR_STOP`）。
> 这是唯一一条 ERROR，其后 329/976/15 等行数全部对平（见 `prod/01` 与下表）。

| 校验 | 生产 | 副本 |
|---|---|---|
| `sys_menu` | 329 | 329 |
| `sys_role_menu` | 976 | 976 |
| `biz_stock_in`（含软删） | 15 | 15 |
| `biz_voucher` | 21 | 21 |

## 文件

| 文件 | 怎么来的 | 内容 |
|---|---|---|
| `prod/01_menu_tree_and_columns.txt` | `psql -f probe_menu_tree.sql`（生产，只读） | 3 页节点；14 个后代按钮码；现状只有 `SUPER_ADMIN` 持有；三张单据表只有 `create_by`、**无 `dept_id`**；行数 14/12/21 |
| `prod/02_read_vs_full_expansion.txt` | 副本上 `psql -f probe_expand_driver.sql`（`BEGIN…ROLLBACK`） | **A**：3 页 × 3 角色 `mode=READ` 真实展开 → 每个角色 7 个去重码，`create/confirm/delete/retry` 全 0；**B**：`mode=FULL` → 17 个去重码，含 `export:create`+`import:confirm` |
| `prod/03_content_vs_list_scope.txt` | 副本上 `psql -f probe_content_scope.sql` | 导出 count = 列表 count（14/14、12/12、21/21）；单据 100% 属 HQ；按 TECH 子树过滤应得 0；6 张主数据表 `dept_id` 列数 = 0 |
| `prod/04_code_chain_grep.txt` | `grep -n` / `sed -n` | 所有 `file:line` 的原始出处（导出链路、默认实现、唯一 override、登记表、裸 SQL、`SCOPE-EXEMPT`、`ImportCtx`） |
| `prod/05_prod_http_current.txt` | `bash probe_http_403.sh`（生产，真身份令牌） | `admin` 对照 200/200/200/400(multipart)；`laoliu` / `laoliu_test` / `henry.liao` 四个接口全 **403** |
| `prod/06_list_visibility_now.txt` | `bash probe_list_visibility.sh`（生产，真身份令牌） | `laoliu_test`：stock-in 200 total=14、payable 200 total=12、voucher **403**；`admin`：14/12/21 |
| `probe_menu_tree.sql` | 手写只读 SQL | 证据 01 |
| `probe_plan_read.sql` / `probe_plan_full.sql` | 手写计划（3 页 × 3 角色，READ / FULL） | 证据 02 的输入；形状与 `scripts/datafix/bizperm/plan.sql` 的 `_plan` 一致 |
| `probe_expand_driver.sql` | 手写驱动，`\i` 仓库原文 `expand.sql` | 证据 02 |
| `probe_content_scope.sql` | 手写只读 SQL | 证据 03 |
| `probe_http_403.sh` | 手写 | 证据 05 |
| `probe_list_visibility.sh` | 手写 | 证据 06 |

## 复跑（一条命令一份证据）

```bash
cd /opt/cloudpivot/docs/evidence/20261105-impexp-scope

# 1) 生产只读：菜单树 + 列 + 行数
scp -q probe_menu_tree.sql root@172.18.10.144:/tmp/
ssh root@172.18.10.144 'set -a; . /etc/cloudpivot/cloudpivot.env; set +a; export PGPASSWORD="$NIK_DATABASE_PASSWORD";
  psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -X -q -P pager=off -f /tmp/probe_menu_tree.sql'

# 2) 隔离副本：READ vs FULL 展开（务必用仓库原文 expand.sql，md5 应为 372bd316c88222c9edb36aecfbe7cd45）
ssh root@172.18.10.144 'mkdir -p /tmp/impexp_probe'
scp -q probe_plan_read.sql probe_plan_full.sql probe_expand_driver.sql root@172.18.10.144:/tmp/impexp_probe/
ssh root@172.18.10.144 'cp /opt/cloudpivot/scripts/datafix/bizperm/expand.sql /tmp/impexp_probe/expand.sql;
  md5sum /tmp/impexp_probe/expand.sql;
  set -a; . /etc/cloudpivot/cloudpivot.env; set +a; export PGPASSWORD="$NIK_DATABASE_PASSWORD";
  psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d cloudpivot_impexp_iso1 -X -q -P pager=off -f /tmp/impexp_probe/probe_expand_driver.sql'

# 3) 隔离副本：导出内容 vs 列表口径
scp -q probe_content_scope.sql root@172.18.10.144:/tmp/impexp_probe/
ssh root@172.18.10.144 'set -a; . /etc/cloudpivot/cloudpivot.env; set +a; export PGPASSWORD="$NIK_DATABASE_PASSWORD";
  psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d cloudpivot_impexp_iso1 -X -q -P pager=off -f /tmp/impexp_probe/probe_content_scope.sql'

# 4) 生产真身份 403 探针（只 curl，不写库）
scp -q probe_http_403.sh probe_list_visibility.sh root@172.18.10.144:/tmp/impexp_probe/
ssh root@172.18.10.144 'bash /tmp/impexp_probe/probe_http_403.sh; bash /tmp/impexp_probe/probe_list_visibility.sh'
```

## 副本清理（复核完再跑，不阻塞本卡）

```bash
ssh root@172.18.10.144 'su -s /bin/bash postgres -c "dropdb cloudpivot_impexp_iso1"'
```

## 三条防误读

1. **`prod/03` 的"导出 count"用的是定义里的**裸 SQL 原文**（`erp_doc.rs:395/414/433` 的 `from`），
   不是我另写的近似查询**；所以"C1 与 C2 相等"证明的是"定义里的 SQL 没有范围谓词"这件事本身。
2. **`prod/02` 的展开用的是仓库原文 `expand.sql`**（md5 与生产仓同一份），不是手抄的规则；
   副本里 `BEGIN … ROLLBACK`，跑完 `_sel` 等中间表不存在。
3. **`prod/05` 的 403 是"当前生产事实"**，不是"授权后仍会 403"；授权后的行为只能由 `prod/02` 的展开集合推导。
