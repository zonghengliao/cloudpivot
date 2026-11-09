# 证据 · svc_selfcheck 最小权限自查账号（卡 t_882d48c8）

> 日期：2026-11-10　机器：145（隔离环境）　生产：144（**未被写入**）
> 目的：证明「建 HUMAN 服务账号 + 只读角色」这条改造在**生产数据的副本**上一次成型、可复跑、可回滚、拦得住越权，
> 从而让生产落地只剩「授权 → 跑一条命令」。

## 0. 一句话

| 项 | 结果 |
|---|---|
| 生产库副本（`pg_dump` 144 → 145 `cloudpivot_svcchk_b`）从零 `apply.sh` | ✅ 一次成型：`3/11/1101/11` → `4/12/1111/12`（新增 1 账号 / 1 角色 / 10 授权节点 / 1 绑定） |
| 第二次 `apply.sh` | ✅ Δ=0（幂等） |
| `verify_svc_selfcheck.sh`（登录 → 3 个列表 200 → menus → 越权 403 → 6 次登录 → 库侧） | ✅ 硬失败 0 / 告警 0 |
| `prod_selfcheck.sh`（生产同一个脚本，指向隔离实例） | ✅ rc=0，`/business/erp/seq` 父=基础数据、`/business/erp/inventory-location` 父=仓库管理 都 ✅ |
| 负例：给只读角色塞 `business:erp:sale-order:create` | ✅ verify 抓出（建单从 403 变 422 + 库侧报非只读码）、apply 自己 `rc=3` 整笔回滚 |
| `--down` → 再 `apply.sh` | ✅ 账号/角色软删、关系表清空、再跑复活**原行**（user/role id 不变） |
| `--rotate`（改密） | ✅ 新口令写库+写 env，改密后 6/6 仍 200 |
| 生产 144 | **未写库、未改 env、未重启**（只做了只读 `pg_dump`） |

## 1. 环境（怎么搭的，便于复跑）

```bash
# ① 生产只读 dump → 145 副本库（两个库：0610 用于 HTTP 验收，b 用于「从零落地」证据）
ssh root@172.18.10.144 'set -a; . /etc/cloudpivot/cloudpivot.env; set +a; \
  PGPASSWORD="$NIK_DATABASE_PASSWORD" pg_dump -h 127.0.0.1 -p "$NIK_DATABASE_PORT" \
  -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" --no-owner --no-privileges' | gzip > prod_dump.sql.gz
su - postgres -c 'psql -c "create database cloudpivot_svcchk_0610 owner cloudpivot"'
gunzip -c prod_dump.sql.gz | psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_svcchk_0610

# ② 隔离实例（端口 18247 / Redis db8 / profile=dev），不碰 145 的 cloudpivot-test
systemd-run --unit=svcc-isolated --property=EnvironmentFile=/root/svcc_test.env \
  --property=WorkingDirectory=/opt/cloudpivot/backend /opt/cloudpivot/backend/target/debug/cloudpivot
```

副本与生产的一致性：副本还原后是 `3 账号 / 436 菜单 / 11 角色`，`admin.dept_id=01a18381-b7c4-753c-8fe6-d39684ead5cd`，
与 144 现状逐项相同（见 `01_` 首行）。隔离实例二进制 = 145 的 `target/debug/cloudpivot`（`f25abaa4` 构建）。

## 2. 逐条证据

| 文件 | 证明什么 |
|---|---|
| `01_fresh_apply_on_prod_copy.txt` | 裸副本库（`(不存在) / (不存在)`）→ 第一次 apply 一次成型 → 第二次 0 变更；落地后账号属性（HUMAN/status=1/must_change=false/locked=false/fails=0/dept 同 admin/`$argon2id$`）、角色（scope=1、menus=10）、10 个授权节点逐行列出 |
| `02_verify_green.txt` | 六段验收全绿：登录 200 → 三个列表 200 → menus 10 节点且 5 个只读页面齐全 → 建单/建用户/查用户列表**均 403** → 连续 6 次登录 6/6 200 → `admin` 与 `svc_selfcheck` 均未锁定、只挂 1 个角色、无非只读码 |
| `03_negative_guard.txt` | **负例（防假绿）**：手工给角色塞 `business:erp:sale-order:create` 后，verify 立刻报两点失败（建单 422≠403、库侧非只读码），apply 自己 `ERROR: OPS_SELFCHECK 里出现非只读权限码` + `rc=3` 整笔回滚 |
| `04_rollback_revive.txt` | `--down`：用户/角色软删（`deleted_at` 有值）、`sys_user_role` 0 行；再 `apply.sh`：**复活软删的原行**（`user=91518a68… role=9427bb57…` 与首次相同），授权节点回到 10 |
| `05_prod_selfcheck_isolated.txt` | 生产那个 `prod_selfcheck.sh` 用新账号跑：health/登录/三个业务接口/菜单定位/库侧锁定 全绿，`rc=0` |
| `06_rotate.txt` | `--rotate` 改密：新口令写库 + 写 env（`env_pw_len=32`、600），改密后登录 200、6/6 全 200、汇总通过 |
| `07_db_grants.txt` | 最终授权面快照：账号 1 行、角色 1 行、绑定 1 个、10 个节点（5 目录无码 + 5 只读页），**写/导出/审批类码数量 = 0** |

## 3. 已知边界（如实写的）

1. **限流**：`LOGIN_RATE_MAX_ATTEMPTS=10 / 60s`（按「用户名 + IP」）。本套验收自己就要 1+6 次登录，
   与 `prod_selfcheck.sh` 贴着连跑会撞 429 —— 脚本把 429 记**告警**（不当口令错误），
   生产验收时按「`apply` → 等 60 秒 → `verify` → `prod_selfcheck` → `prod_deploy --verify-only`」分批跑。
2. **`data_scope=1`（本人范围）**：三个列表接口返回 200 但行数可能为 0（新账号没有单据）。
   自查要的就是「接口通、权限够」，不是「能看到别人数据」；若将来要它看全局，改 `sys_role.data_scope` 属**扩权**，要单独评估。
3. **prod 未执行**：本卡生产落地（建账号 + 改 env）需要 SUPER_ADMIN 会话或 DBA 写库授权，
   仓库侧规则（`docs/待决策.md` 铁律：未获书面确认前 prod 数据与账号一律不动）要求老大书面一句话。
   `apply.sh` 已按「备份 → 单事务 → 断言 → 写 env（备份/600/回读）」写好，授权后一条命令即可。
4. 145 留下两个副本库（`cloudpivot_svcchk_0610` / `cloudpivot_svcchk_b`）与 `svcc-isolated.service` 供复跑，
   清理见 §4。

## 4. 复跑 / 清理（145）

```bash
# 复跑（隔离实例还活着时）
systemctl status svcc-isolated
bash /root/svcchk/scripts/verify_svc_selfcheck.sh --base-url http://127.0.0.1:18247 \
  --env-file /root/svcc_test.env --cred-file /root/svcc_test.cred

# 清理
systemctl stop svcc-isolated
su - postgres -c 'psql -c "drop database cloudpivot_svcchk_0610"'
su - postgres -c 'psql -c "drop database cloudpivot_svcchk_b"'
rm -f /root/svcc_test.env /root/svcc_b.env /root/svcc_test.cred /root/svcc_b.cred
```
