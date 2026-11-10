# 证据包 · svc_selfcheck 生产落地（144，2026-11-10 08:40）

卡：`t_882d48c8`（自查账号专用化，替掉借用的真人 `henry.liao`）。
本目录全部是**生产 144 上的原始输出**（脚本原样重定向，未编辑）。
采集脚本：`/root/svcchk-run/collect_evidence_svcc.sh`（只读 + 自查，不改库 / 不改 env / 不重启）。

## 1. 结论

| 验收项 | 结果 | 证据 |
|---|---|---|
| `svc_selfcheck` 存在、`user_type='HUMAN'`、`must_change_password=false`、与 admin 同部门 | ✅ | `04_db_snapshot.txt` §1 |
| 有且仅有只读角色 `OPS_SELFCHECK`（`data_scope=1`），授权码全是 `:list/:read` | ✅ | `04_db_snapshot.txt` §2–§5 |
| 能登录 + 三个自查列表 200 + `menus` 10 节点齐全 | ✅ | `01_verify_svc_selfcheck.txt` §1–§3 |
| 越权探测 3/3 必须 403（建销售订单 / 建用户 / 查用户列表） | ✅ | `01_verify_svc_selfcheck.txt` §4 |
| **连续 6 次登录 6/6 200** | ✅ | `01_verify_svc_selfcheck.txt` §5；`05_auth_logs.txt`（08:45:53 起 7 条 SUCCESS 逐条带毫秒） |
| `admin` / `svc_selfcheck` 均 `is_locked=false / login_fail_count=0` | ✅ | `01` §6、`02` §5、`04` §1 |
| `prod_selfcheck.sh` 全绿（硬失败 0） | ✅ | `02_prod_selfcheck.txt` |
| `prod_deploy.sh --verify-only` rc=0 | ✅ | `03_prod_deploy_verify_only.txt` |
| `check_cred_convention.sh` ✅ 生产向无违规 | ✅ | 见 §3（在 99 仓库跑，输出贴下） |
| 服务未重启、运行实例未受影响 | ✅ | `06_files_and_service.txt`（PID 2861341，ActiveEnter 06:00:41） |
| `henry.liao` 账号/口令/角色本卡未碰 | ✅ | `05_auth_logs.txt`（他今日最后一次登录 07:23:48，之后无记录） |

## 2. 怎么建的（接口路，没有直写鉴权表）

```bash
python3 scripts/ops/svc_selfcheck/apply_api.py --dry-run          # 体检（只读）
python3 scripts/ops/svc_selfcheck/apply_api.py --apply --allow-admin-session
#   幂等复核：同样命令再跑一遍 → 不新建行，只把菜单授权/角色绑定整批对齐
SVCC_AUTHORIZED=1 python3 scripts/ops/svc_selfcheck/apply_api.py --apply   # 第二遍 Δ=0
```

- 写动作全走接口：`POST /api/system/role` → `PUT /api/system/role/{id}/menus` →
  `POST /api/system/user`（带 `role_ids`）→ `PUT /api/system/user/{id}/roles`。
  权限由后端 guard（`ensure_role_grant_allowed` / `ensure_menus_grantable` / `ensure_no_mutex`）自己判。
- admin 口令只用于**每次脚本运行的一次登录**（`NIK_ADMIN_PASSWORD`，取自 600 的 env）；
  本卡期间 3 次 admin 登录全 SUCCESS，`admin` 的 FAIL 记录今日 10 条全部发生在 07:22 之前
  （历史漂移），**08:00 之后 0 条**。
- 新口令 `secrets.token_urlsafe(24)`（32 字符）只落两处：`sys_user.password`（argon2id PHC）与
  `/etc/cloudpivot/cloudpivot.env` 的 `CLOUDPIVOT_OPS_PASSWORD`（600）；明文另存
  `/root/svc_selfcheck.cred`（600）。**本证据包不含口令，任何日志里也没有。**

## 3. 防回归 lint（在 99 仓库 `/opt/cloudpivot` 跑）

```
$ bash scripts/ops/check_cred_convention.sh
== 扫描 656 个脚本（root=/opt/cloudpivot）
== dev/test 存量（提示，不阻断）：130 个脚本
== ✅ 生产向脚本无违规
rc=0
```

## 4. 回滚

```bash
# 账号 + 角色（接口路；DELETE 是软删）
#   DELETE /api/system/user/01a1c2e6-3ede-707a-9929-ae246d635c06
#   DELETE /api/system/role/01a1c2e6-3eb6-753b-ba16-eb9aac4d9c8d
# 或 DBA 路（等价，账号/角色软删 + 关系表硬删）
bash /root/svcchk-run/apply.sh --down
# env 还原（切换前备份）
cp /etc/cloudpivot/cloudpivot.env.bak-20261110-084031 /etc/cloudpivot/cloudpivot.env && chmod 600 $_
# 四张鉴权表兜底还原（会覆盖，慎用）
psql ... -f /root/svc-selfcheck-backup-20261110-084029/auth_tables.sql
```

## 5. 文件清单

| 文件 | 内容 |
|---|---|
| `01_verify_svc_selfcheck.txt` | 验收：登录 / 三列表 / menus / 越权 403 / **连续 6 次登录** / 库侧 |
| `02_prod_selfcheck.txt` | 生产自查本体（部署 ⑦ 步用的同一个脚本） |
| `03_prod_deploy_verify_only.txt` | `prod_deploy.sh --verify-only`（rc=0，未改代码/库/服务） |
| `04_db_snapshot.txt` | 库侧快照：账号属性 / 角色 / 授权码 / 非只读码 / env 权限 |
| `05_auth_logs.txt` | `auth_logs`：svc_selfcheck 逐条 SUCCESS（毫秒级）、按账号汇总、henry.liao 今日最后登录 |
| `06_files_and_service.txt` | env / README / cred 权限（600）、备份路径、服务 PID 与启动时间（未重启） |

## 6. 遗留（需真人执行）

`henry.liao` 的口令只有 12 位，且长期被自动化脚本当凭证用（本卡已把他从自查链路上摘掉）。
建议他本人轮换到 ≥16 位（或改由密管注入）；轮换后 `bash /root/prod_selfcheck.sh` 仍应全绿 ——
**那时才最终证明「自查与真人账号解耦」**（现在第 1 道验证已成立：自查已完全走 `svc_selfcheck`）。
