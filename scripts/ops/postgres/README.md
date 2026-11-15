# scripts/ops/postgres —— PG 侧运维脚本

| 脚本 | 干什么 | 卡片 |
|:--|:--|:--|
| `pg_slowlog.sh` | PG 慢查询日志（`log_min_duration_statement = 1000` + `log_line_prefix`）的落地 / 检查 / 探针 / 回滚 / 自证 | P3-16 |

口径与排查见 [docs/运维规程-慢查询日志.md](../../../docs/运维规程-慢查询日志.md)。

## 快速用法

```bash
# 在 99 上对远端落地（ssh root@HOST，脚本自身作为载荷，不依赖 scp）
bash scripts/ops/postgres/pg_slowlog.sh --host 172.18.10.145     # 落地 + 16 项检查 + 4 项探针断言
bash scripts/ops/postgres/pg_slowlog.sh --host 172.18.10.144

# 只检查（只读，可对生产跑）
bash scripts/ops/postgres/pg_slowlog.sh --host 172.18.10.144 --check

# 只跑慢查询探针（要求已落地）
bash scripts/ops/postgres/pg_slowlog.sh --host 172.18.10.145 --probe

# 本机直接跑（在 145 / 144 上，as root）
bash scripts/ops/postgres/pg_slowlog.sh

# 回滚（用最近一次 .bak-p316.* 还原 postgresql.conf 并 reload）
bash scripts/ops/postgres/pg_slowlog.sh --host 172.18.10.145 --rollback

# 自证（临时目录，不碰真实配置）：幂等 / 块在末尾 / strip 后逐字节还原
bash scripts/ops/postgres/pg_slowlog.sh --self-test
```

退出码：`0` 全绿 / `1` 有断言失败 / `2` 用法错误。

## 它到底改了什么

在 `/etc/postgresql/<VER>/<CLUSTER>/postgresql.conf` 末尾追加一个**托管块**（带 `>>>` / `<<<` 标记）：

```conf
# >>> cloudpivot P3-16 pg slowlog (managed by scripts/ops/postgres/pg_slowlog.sh) >>>
# 卡片 P3-16：慢查询（>=1000ms）进 PG 日志。由 scripts/ops/postgres/pg_slowlog.sh 托管，手改会被覆盖。
log_min_duration_statement = 1000		# 记录执行 >= 1000ms 的语句（-1 = 关闭）
log_line_prefix = '%m [%p] %q%u@%d '	# 会话行 = 时间 [pid] user@db；%q 只抑制后台进程的空 user@db
# <<< cloudpivot P3-16 pg slowlog <<<
```

- **幂等**：重复执行只有一块（先删旧块再追加）。
- **后写者生效**：块在文件末尾，覆盖文件中部任何同名旧值（Debian 交付的 `#log_min_duration_statement = -1` 不用改）。
- **可精确回滚**：`strip_block()` 的输出与落地前**逐字节一致**（`--self-test` 与 apply 阶段各断言一次 sha256）。
- **不用 `ALTER SYSTEM`**：值写在 `postgresql.conf`（卡片口径、可 `git grep`、可被配置管理固化），`postgresql.auto.conf` 里不含这两个参数（脚本断言 ③）。

## 为什么是 reload 不是 restart

两条参数都是 **SIGHUP 级**（`pg_settings.context = superuser`），`pg_ctlcluster <VER> <CLUSTER> reload` 即生效；
脚本断言 `pending_restart = false` + `source = configuration file` 证明已生效。

`restart` 会掐断所有连接（144 上是生产库，实测 21 个连接 / `MainPID` 自 2026-11-14 未变），
所以脚本**只 reload**。reload 前先用 `postgres -C` 预校验配置语法，坏配置不会把下一次重启搞挂。
