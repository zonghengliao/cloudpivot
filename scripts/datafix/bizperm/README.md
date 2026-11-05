# BIZPERM · 业务角色权限覆盖数据修复（P0-FIX-2B-C 跟进 / `t_34cee9ea`）

> 走 `docs/architecture/数据修复与清场SOP.md`：只读复现 → 行级快照 → 幂等 DML → 对平 → 巡检 → 留档。
> **决策与验收在 `docs/architecture/P0-FIX-2B-C-业务角色权限覆盖-决策.md`；
> 执行证据在 `docs/evidence/20261105-bizperm-grant/`。**

## 目标态

5 个既有业务角色按**职能归位**补齐业务页权限码（不改 `data_scope`、不建角色、不碰 `SUPER_ADMIN`）：

| 角色 | 施工前 → 后（授权行） | 施工前 → 后（可见业务页） |
|---|---|---|
| `DEPT_LEADER` | 48 → 95 | 12 → 27 |
| `FINANCE_MANAGER` | 59 → 135 | 13 → 41 |
| `GENERAL_MANAGER` | 13 → 119 | 3 → 54 |
| `PURCHASE_MANAGER` | 13 → 88 | 3 → 29 |
| `WAREHOUSE_MANAGER` | 20 → 92 | 4 → 27 |
| 5 角色并集覆盖 | 12/59 → **51/59** | 未覆盖 8 页（治理页 + 导入导出） |

## 文件

| 文件 | 作用 |
|---|---|
| `plan.sql` | **唯一的授予矩阵**（role × path × mtype × mode）。改矩阵只改这里 |
| `expand.sql` | 把矩阵展开成 `(role, menu)` 集合：后代递归 + 只读白名单 + **祖先闭包** + 硬断言（超管/治理页/空计划） |
| `05_snapshot.up/down.sql` | 整行快照（`datafix_bizperm_snapshot`，四类 batch） |
| `20_grant.up/down.sql` | 新建 3 条凭证按钮码 + 按矩阵插授权 + 推进 `token_version`；down 按**确定性 id** 精确删 |
| `00_backup.sh` | 整库 dump + 5 张相关表 + 施工前可读镜像（`/root/datafix-backup-bizperm-<ts>/`） |
| `apply.sh` | `--check / --snapshot / --grant / --rollback` |
| `cache_invalidate.sh` | epoch INCR（**带 `cloudpivot:` 前缀**）+ 删持有者的 `uv:` / `perm:` 缓存 |
| `probe_bizperm.py` | 四账号真身份探针（自签令牌 → 业务接口 200/403 + 行数） |
| `proof_rows.sql` | 对平巡检（施工前后同一份 SQL） |
| `drill_rollback.sql` | 生产**事务内**回滚演练（测完 `ROLLBACK`，零净变更） |
| `verify_drill.sh` | 隔离副本四步往返（up → 幂等重放 → down → 再 up） |

## 用法

```bash
# 生产（144）
bash 00_backup.sh                    # 必须：--grant 会校验本库备份
bash apply.sh --check                 # 只读现状
bash apply.sh --snapshot              # 阶段 0：快照
bash apply.sh --grant                 # 阶段 1：施工（幂等，可重放）
bash cache_invalidate.sh              # 必须：缓存失效
python3 probe_bizperm.py --all --json # 验收探针

# 回退
bash apply.sh --rollback && bash cache_invalidate.sh

# 演练（隔离副本，FORCE=1 自动开）
BIZPERM_DB=<副本库> bash verify_drill.sh
```

## 三条坑（都踩过，写法已经绕开）

1. **`\i` 的相对路径按 CWD 解析**，不按 `-f` 的文件目录 ⇒ `apply.sh` 先 `cd "$HERE"`；
   手工跑 SQL 时也要先 `cd scripts/datafix/bizperm`。
2. **`psql -1` 每个文件一个事务**：`05_snapshot` 与 `20_grant` 必须分两次调用，不能塞进同一个事务跑
   （快照要在 DML 之前 committed，否则失败时快照一起回滚、事后无从对平）。
3. **回退不能按时间戳删**：`sys_role_menu.created_at` 精度有限，历史上同秒插入的既有行会被误删。
   本卡用「确定性 id」（`md5(role_id|menu_id|bizperm-20261105)` 变形为 uuid）——up 插的 id 可重算，
   down 只删 `id = 该值` 的行，既有授权一行不碰。

## 边界

- 不建角色、不改 `data_scope`、不动 `SUPER_ADMIN`（SQL 里有硬断言）
- 不授平台治理页（`/system/*` 管理页、`/support/*`、存储配置）与批量导入导出（见决策文档 §4.3）
- 不回填 `biz_task` / `biz_project` 的 `dept_id`（D-3）
