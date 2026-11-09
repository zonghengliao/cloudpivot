# svc_selfcheck —— 最小权限自查账号（HUMAN）+ 只读角色 OPS_SELFCHECK

> 卡：`t_882d48c8`。父卡 `t_d4e92b99` 把「env 里哪个口令属于哪个账号」定死了，
> 但当时自查仍需**借真人账号** `henry.liao`（`GENERAL_MANAGER`，4 个业务角色）：
> ① 他一改密，所有自查立刻 401（本类故障复发）；
> ② `auth_logs` 里分不清「机器自查」与「henry 本人操作」。
> 本目录把这两个问题一起解掉：**专用账号 + 只读角色 + 一条可复跑的命令**。

## 1. 落地结果（生产副本实测，2026-11-10）

| 项 | 值 |
|---|---|
| 账号 | `svc_selfcheck`，`user_type='HUMAN'`（**不能是 ROBOT**：`system/auth/handler.rs` → `ai_actor::login_forbidden` 禁止 ROBOT 网页登录） |
| 姓名 | `自动化自查账号（勿删/勿改密）` |
| 部门 | 与 `admin` 同部门（**按 `admin.dept_id` 现查**，不硬编码 uuid；admin 没部门就 `RAISE EXCEPTION` 停手） |
| 口令 | `secrets.token_urlsafe(24)` = 32 字符，argon2id PHC 入库；明文只进 `CLOUDPIVOT_OPS_PASSWORD` / `/root/svc_selfcheck.cred`（600） |
| 角色 | `OPS_SELFCHECK`「运维自查（只读）」，`data_scope=1`，`status=1` |
| 授权 | 10 个菜单节点：5 个只读页面 + 它们的分组目录；权限码只有 `business:task:read`、`business:erp:sale-order:list`、`business:erp:voucher:list`、`business:erp:seq:list`、`business:erp:inventory-location:list` |
| 明确不给 | 任何 `*:create/update/delete/export/import/approve/post/void`，也不给 `system:*`（`system:*` 只授 SUPER_ADMIN 是本仓既有事实） |

为什么菜单里要带 3 个「目录」节点：目录的 `auth_code` 是空串（不授权任何接口），
但 `GET /api/system/auth/menus` 只返回 `sys_role_menu` 里有的节点，缺了父目录，
前端侧边栏会把子页面当孤儿丢掉，`prod_selfcheck.sh` 第 4 步也就看不到
「`/business/erp/seq` 父=基础数据」这类定位信息。

## 2. 一条命令落地

```bash
# 生产（144）：读 /etc/cloudpivot/cloudpivot.env 的 NIK_DATABASE_*，写 env 的 CLOUDPIVOT_OPS_*
bash scripts/ops/svc_selfcheck/apply.sh

# 只体检（不改库、不改 env）
bash scripts/ops/svc_selfcheck/apply.sh --dry-run

# 隔离库演练（不写 env）
bash scripts/ops/svc_selfcheck/apply.sh --env-file /tmp/iso.env --no-env

# 改密（重新生成口令 → 写库 + 写 env；凭据文件也一起换）
bash scripts/ops/svc_selfcheck/apply.sh --rotate

# 回滚（账号/角色软删、关系表硬删；**env 不会自动改**，要手动指回）
bash scripts/ops/svc_selfcheck/apply.sh --down
```

`apply.sh` 的顺序是固定的，任何一步失败就停：

1. 解析库连接（**只从 env 文件里 grep `NIK_DATABASE_*`**，不 `source` 整个文件）
2. 凭据：复用 `/root/svc_selfcheck.cred`（600）；没有就用 `gen_credential.py` 生成
3. 备份四张鉴权表 → `/root/svc-selfcheck-backup-<ts>/`（`auth_tables.sql` + `pre_state.txt` + `MANIFEST.txt`，dump < 1KB 视为失败）
4. `10_svc_selfcheck.up.sql`（**单事务 + 硬断言**，见 §4）
5. 打印四张表的 Δ（复跑应为 0）
6. 写 env：`CLOUDPIVOT_OPS_USER=svc_selfcheck` / `CLOUDPIVOT_OPS_PASSWORD=<新口令>`（先备份、`install -m 600`、原子替换、回读校验；口令不进日志、不进 argv）

退出码：`0` 成功 / `1` 失败 / `2` 用法或环境 / `3` SQL 断言失败（整笔已回滚）。

## 3. 依赖：算 argon2id 的 python

后端 `Crypto::verify_password` 按 **PHC 串自带参数** 校验，所以只要产出的串合法即可；
本目录刻意用与 `sys_user.admin` 完全相同的参数（`m=19456,t=2,p=1`，RustCrypto argon2 0.6 的默认），
避免出现第二种形态。实测：145 系统 python 自带 `argon2-cffi`；144/99 没有，需要一次性建 venv：

```bash
bash scripts/ops/svc_selfcheck/setup_hash_env.sh      # 默认 /root/.venvs/svcchk
```

`apply.sh` 的 python 选择顺序：`$SVCC_PYTHON` → `/root/.venvs/svcchk/bin/python` → `python3`（都要过 `import argon2` 探测）。
都不可用时它**不会**退化成「明文入库」或「猜一个哈希」，直接 `exit 2`。

## 4. SQL 里的硬断言（为什么可以放心复跑）

`10_svc_selfcheck.up.sql` 只有一个事务，断言不成立就 `RAISE EXCEPTION` → 整笔回滚：

| # | 断言 | 失败含义 |
|---|---|---|
| ① | `svc_pwhash` 必须是 `$argon2id$v=19$m=…,t=…,p=…$salt$hash` | 有人想塞明文/短串当口令 |
| ② | `admin.dept_id` 必须存在 | 组织未落位 —— 不猜部门 |
| ③ | 账号属性：`HUMAN` / `status=1` / `must_change_password=false` / `is_locked=false` / 口令是 argon2id | 账号被改坏 |
| ④ | 每条菜单 `path` 必须**恰好命中 1 行** | 菜单结构变了 —— 不猜授权目标 |
| ⑤ | 账号 `sys_user_role` **恰好 1 行**且就是 `OPS_SELFCHECK` | 被挂到 SUPER_ADMIN/业务角色 |
| ⑥ | 授权码只许以 `:list/:view/:read/:page/:detail` 结尾 | 有人给只读角色塞了写权限（负例实测能拦住） |
| ⑦ | 三条自查必需码必须都在 | 少授 → 三个列表接口会 403 |

复跑幂等：第二次执行 `NOTICE` 里不给「插入/复活」，apply.sh 的 Δ = 0。
`down.sql` 之后复跑 up，会**复活原行**（`svc_selfcheck` 账号 + `OPS_SELFCHECK` 角色 id 不变），
不会在 `role_code` 上堆一串软删副本。

## 5. 验收（HTTP + 库侧双口径）

```bash
bash scripts/ops/svc_selfcheck/verify_svc_selfcheck.sh \
  --base-url http://127.0.0.1:8023 \
  --env-file /etc/cloudpivot/cloudpivot.env \
  --cred-file /root/svc_selfcheck.cred
```

六段检查：单次登录 → 三个列表 200 → `menus` 200 且 5 个只读页面节点齐全 →
**越权探测必须 403**（建销售订单 / 建用户 / 查用户列表；空 body 时若权限给多了会是 400/422，绝不会真写库）→
连续 6 次登录 6/6 200 → 库侧（`admin` 与 `svc_selfcheck` 均未锁定、只挂 1 个角色、没有非只读码）。

**限流口径**：`LOGIN_RATE_MAX_ATTEMPTS=10 / 60s`（按「用户名 + IP」）。本脚本自己就要 1+6 次登录，
所以别紧贴着 `prod_selfcheck.sh` / `prod_deploy.sh --verify-only` 连着跑；撞到 429 脚本记**告警**并
提示「60 秒后单独复跑」，不当成口令错误。

## 6. 故障处置

| 现象 | 原因 | 处置 |
|---|---|---|
| `apply.sh` 报 `svc_pwhash 必须是 argon2id PHC 串` | 凭据文件被手工改坏 / 有人塞了明文 | 删 `/root/svc_selfcheck.cred` 后加 `--rotate` 重跑 |
| `找不到带 argon2-cffi 的 python` | 没建 venv | `bash setup_hash_env.sh`，或把 145 上生成的凭据文件拷过来 |
| `菜单 path=… 命中 0 行` | 前端路由改了、菜单迁移没跟上 | **别改白名单凑数**：先确认新 path / 新权限码，再改 SQL 与 `verify_svc_selfcheck.sh` 两处 |
| 自查 401 | env 里 `CLOUDPIVOT_OPS_PASSWORD` 与库内不一致（改密没同步/还原了库） | `apply.sh --rotate` 一条命令同时改库和 env；**不要**手工只改一边 |
| 自查 403 锁定 | 账号被连续错口令锁了 | 等 30 分钟，或 `UPDATE sys_user SET is_locked=false,login_fail_count=0,lock_time=NULL WHERE username='svc_selfcheck'` |
| 想临时退回真人账号 | — | 把 env 的 `CLOUDPIVOT_OPS_USER/PASSWORD` 指回 `henry.liao`（但要注意他随时可能改密，见 `docs/运维规程-凭据与自查账号.md` §7） |

## 7. 回滚

```bash
bash scripts/ops/svc_selfcheck/apply.sh --down          # 账号/角色软删 + 关系表硬删
# 再按需把 env 的 CLOUDPIVOT_OPS_USER/PASSWORD 指回原账号
# 兜底：用 /root/svc-selfcheck-backup-<ts>/auth_tables.sql 还原四张表（会覆盖，慎用）
```

物理删除（`DELETE FROM sys_user`）**不在脚本里**：审计要留痕，真删需要人工在确认无引用后手工执行。
