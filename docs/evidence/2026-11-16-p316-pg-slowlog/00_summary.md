# P3-16 证据：PG 慢查询日志（>1s）落地与验收

- 日期：2026-11-16 00:16–00:24 CST
- 机器：145（测试）/ 144（生产）
- 执行：`bash scripts/ops/postgres/pg_slowlog.sh --host <HOST>`（脚本自身经 `ssh root@HOST bash -s` 送过去，不依赖 scp）
- 结论：**两台都 20/20 断言全绿（检查 16 + 探针 4）**，慢查询行确实进 PG 日志

## 关键样例（卡面要求「日志里有记录」）

145（`cloudpivot_test` 库）：

```
2026-11-16 00:18:50.806 CST [1387947] postgres@cloudpivot_test LOG:  duration: 2009.065 ms  statement: select pg_backend_pid() as pid, pg_sleep(2);
```

144（生产，`postgres` 库 —— 本机没有 `cloudpivot_test`，脚本自动降级，不碰业务库）：

```
2026-11-16 00:19:19.402 CST [1006257] postgres@postgres LOG:  duration: 2004.974 ms  statement: select pg_backend_pid() as pid, pg_sleep(2);
```

反证（0.4s 查询**不**进日志，证明是 1000ms 门限而不是 `log_statement=all`）：见 `07_probe_final_*.txt`。

## 生效参数（两台一致）

```
log_min_duration_statement = 1000
log_line_prefix            = '%m [%p] %q%u@%d '   # 保留 %q，见 docs/运维规程-慢查询日志.md §2
log_destination            = stderr（默认）→ /var/log/postgresql/postgresql-18-main.log
```

落地前基线（用备份文件复现）：`log_min_duration_statement = -1`（默认，未开）、
`log_line_prefix` 已是 Debian 默认 `'%m [%p] %q%u@%d '` → 见 `01_before_from_backup.txt`。

「重启后验证」怎么答的：**没有真重启**（145 上当时有别卡在跑 DB 测试、144 是生产库），
改由断言 ④ `postgres -D <datadir> -c config_file=<conf> -C ...` 回答 —— 它解析的正是重启时读的那份 conf，
输出 `1000` / 前缀正确 ⇒ 重启后结果一致。运行时已生效则由断言 ②（`pg_settings` + `source = configuration file` + `pending_restart = false`）证明。
依据见 `docs/运维规程-慢查询日志.md` §4。

## 生产影响面（144）

| 项 | 落地前 | 落地后 |
|:--|:--|:--|
| `cloudpivot.service` MainPID | 633825 | **633825**（未变） |
| `ActiveEnterTimestamp` | 2026-11-14 16:45:28 | 同（未重启） |
| `GET :8023/health` | 200 | 200 |
| PG `pg_postmaster_start_time()` | 2026-11-14 06:58:19 | 同（**只 reload，未 restart**） |
| PG 连接数 | 21 | 21 |

## 文件清单

| 文件 | 内容 |
|:--|:--|
| `01_before_from_backup.txt` | 落地前基线（用 `.bak-p316.*` 复现 `-1` 与原始两行） |
| `01_before_145.txt` | 145 落地前跑 `--check`：6 项红（证明检查器不是空转） |
| `02_apply_145.txt` | 145 落地 + 16 项检查 + 4 项探针（20/20） |
| `03a_apply_144_first_run.txt` | 144 首次落地（含「托管块外与备份逐字节一致」sha256 断言）；末尾探针因缺 `cloudpivot_test` 报错 |
| `03b_apply_144.txt` | 144 重跑（探针降级到 `postgres` 库）20/20 |
| `04a_check_145.txt` / `04b_probe_145.txt` | 145 只读检查 14/14 / 探针 3/3（加反证前） |
| `05_prod_impact_144.txt` | 生产服务与 PG 启动时间未变 |
| `06_observe_window.txt` | 开阈值后约 2 分钟真实流量：144 只有探针 1 行、145 2 行；后台进程行仍无 `@`（`%q` 生效） |
| `07_probe_final_145.txt` / `07_probe_final_144.txt` | 最终探针（含 0.4s 反证）4/4 全绿 |
