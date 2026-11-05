# SUPERCONV · 超管收敛数据修复脚本（P0-FIX-2B-C / `t_db2506d2`）

> 走 `docs/architecture/数据修复与清场SOP.md`：只读复现 → 行级快照 → 幂等 DML → 对平 → 巡检 → 留档。
> **执行证据与结论在 `docs/evidence/20261105-p0fix2b-superconv/`（README + prod/ 35 份 + iso/ 6 份）。**

## 目标态
生产只剩 `admin` 一个真超管（决策 D-2）：

| 账号 | 处理 |
|---|---|
| `admin` | 不动（唯一超管） |
| `laoliu` | 摘 `SUPER_ADMIN`，补 5 个业务角色（用户 16:36 批准方案） |
| `laoliu_test` | 摘 `SUPER_ADMIN` + `GENERAL_MANAGER`，留 3 个 3 档角色（范围 = TECH 子树） |
| `henry.liao` | 落位 HQ + 摘 `SUPER_ADMIN`，留 4 个业务角色（用户 16:45 确认是测试号） |

## 阶段（顺序不可颠倒）

| 阶段 | 文件 | 干什么 | 为什么必须在这个位置 |
|---|---|---|---|
| 0 | `05_land_henry_hq.up.sql` | `henry.liao.dept_id → HQ` | 不落位 ⇒ N2 巡检永远 WARN、且他一旦丢掉 `GENERAL_MANAGER` 会被降级 SelfOnly |
| 1.5 | `15_grant_menu_dirs.up.sql` | 5 个业务角色补 **9 行祖先目录** | 生产上 `财务管理`/`采购管理`/`support` 三个目录只授给了超管；不补则摘权后侧边栏是空的（`build_tree` 丢孤儿） |
| 1 | `10_grant_laoliu_roles.up.sql` | `laoliu` 绑 5 个业务角色 | 先授后摘，避免「既没超管也没业务角色」的窗口 |
| 2 | `20_revoke_super.up.sql` | 摘 4 行绑定 + 推进 3 个 `token_version` | 前置硬断言阶段 0/1 已落地，缺一条就 RAISE 回滚 |

## 用法

```bash
# 生产（144）
bash 00_backup.sh                      # 备份（apply 会校验 MANIFEST 里的库名）
bash apply.sh --check                  # 只读现状
bash apply.sh --grant                  # 05 → 15 → 10
bash apply.sh --revoke                 # 20
bash cache_invalidate.sh               # 缓存失效（键名见下，别用裸名）
bash apply.sh --rollback-revoke        # 只回退阶段 2（C6 可逆演练）
bash apply.sh --rollback               # 全退：20.down → 10.down → 15.down → 05.down

# 隔离副本（演练专用）
SUPERCONV_ISO_DB=<副本库> bash verify_drill.sh        # SOP §9 四步演练
SUPERCONV_ISO_DB=<副本库> bash verify_c2_subtree.sh   # C2 子树因果对照（末尾净零）
```

`--grant/--revoke` 要求存在 `/root/datafix-backup-superconv-*/full.dump` 且 MANIFEST 里是本库
（`FORCE=1` 可跳过，仅限演练库）。

## 缓存失效：键名带实例前缀（卡面口径是错的）

应用侧缓存键是 **`cloudpivot:` 前缀**（`cloudpivot:uv:{uid}`、`cloudpivot:perm:user_scope:{epoch}:{uid}`、
`cloudpivot:perm:user_perms|user_roles|user_menus:{uid}`）；裸名 `perm:user_scope_epoch` 应用根本读不到
（存量 scope 键全是 `...:user_scope:0:<uid>` 即证据）。`cache_invalidate.sh` 两个 epoch 都推进，
并删掉三个账号的 `uv:` / `perm:` 四类键 —— **不删 `cloudpivot:uv:{uid}` 的话，推进 `token_version`
后 60s 内连重新登录都会 401/20008**。

## 探针

- `mint_token.py`：按库内真值自签访问令牌（生产口令不可猜）；`ver` 逐请求与库比对、
  通配声明还要过库侧复核（BUG-SYS-04）⇒ **伪造不出超管**，只能复现「该账号当前真有什么」。
- `probe_conv.py`：逐账号断言（角色集 / 权限码数 / 菜单树业务页 / 6 个系统页 / 2 个业务页）；
  隔离副本里加 `--password` 走真登录。
- `probe_scope_c2.py`：任务 / 项目 / 用户的可见行数。

## 边界

不删号、不停用、不动 `expire_time`；不动 `qa.*` / `verify-ai` / `cl_43870`；
不改流程定义；不回填 `biz_task` / `biz_project` 历史 `dept_id`（D-3）；
摘权不回收已生成的待办（D13：`biz_flow_task.assignee_id` 是 user_id）。
