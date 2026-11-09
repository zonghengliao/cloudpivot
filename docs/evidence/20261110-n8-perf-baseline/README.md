# N-8 性能基线 · 证据包（2026-11-10）

卡：`t_dd421965`（NEAR-FIX-7【N-8】k6 脚本 + 10 万行数据集验证）
正文文档：`docs/运维规程-性能基线.md`
脚本：`scripts/verify/perf/`（`gen_perf_data.sql` / `measure.sh` / `explain_100k.sql` / `k6_*.js` / `run_perf_suite.sh`）

## 环境口径

| 项 | 值 |
|---|---|
| 机器 | 145 `ubuntu-svr`（4 核 / 7 GB），本地 PG 14+ |
| 库 | **`cloudpivot_perf`**（从 `cloudpivot_dev` 克隆结构+配置，再灌测试数据；**保留**为性能基线库） |
| 实例 | 145 上隔离实例 `127.0.0.1:18099`（`NIK_DATABASE_NAME=cloudpivot_perf`、`NIK_REDIS_URL=.../5`），**没有重启 `cloudpivot-test`** |
| 账号 | `admin`（SUPER_ADMIN） |
| 压测工具 | `k6 v2.3.0 (linux/arm64)`，装在 145 `/usr/local/bin/k6` |

## 数据集（`gen_perf_data.sql`，10.1 s 灌完）

| 表 | 本卡行数 | 表内总行数 |
|---|---:|---:|
| `biz_task` | 100 000 | 100 006 |
| `biz_voucher` | 100 000 | 100 211 |
| `biz_voucher_entry` | 300 000 | 300 481 |
| `biz_inventory_txn` | 100 000 | 100 047 |
| `operation_logs` | 100 000 | 123 857 |

5 张表均已 `ANALYZE`。

## 文件

| 文件 | 内容 |
|---|---|
| `raw/n8_perf_1000.csv` | 1 千行规模，14 个接口 × 3 次计时（中位见文档 §2） |
| `raw/n8_perf_10000.csv` | 1 万行规模 |
| `raw/n8_perf_100000.csv` | 10 万行规模（**V0369 索引之前**） |
| `raw/n8_perf_100000_indexed.csv` | 10 万行规模（**V0369 索引之后**） |
| `raw/explain_100k_before_index.txt` | 14 条被测 SQL 的 `EXPLAIN (ANALYZE, BUFFERS)`，加索引前（含 3 处 `Seq Scan biz_task` / 全表聚合计划） |
| `raw/explain_100k_after_index.txt` | 同上，加索引后（任务列表变 `Index Scan using idx_biz_task_list_order`） |
| `raw/k6/*.json` | 5 个 k6 脚本的 `--summary-export`（退出码全 0） |
| `raw/k6/*.txt` | 5 个 k6 脚本的完整控制台输出（含 `✓` 阈值行、p95、吞吐；`.log` 被仓库 `.gitignore` 挡，改名 `.txt` 入库） |

## 一句话结论

10 万行下：**没有任何一条 SQL > 100 ms**（最慢 82 ms = 资产负债表期初聚合）；
HTTP 单请求最慢 176.8 ms（资产负债表），k6 并发口径 p95 ≤ 250.8 ms；
`V0369` 两条列表排序索引把任务列表 85.3 → 39.2 ms、凭证列表第 1000 页 78.8 → 22.1 ms；
容量红线：`biz_voucher_entry` 100 万行（≈40 万凭证），到线前要上「期间余额表」或按期间分区，
详见文档 §6。

## 与卡面的偏差（诚实记录）

1. **没有把数据灌进 `cloudpivot_dev`**：卡面允许「备份或独立库」，本卡选独立库
   `cloudpivot_perf`（理由见文档 §0），`cloudpivot_dev` 一行未改；数据集**保留**不清，
   作为性能基线库。
2. **`pg_stat_statements` 未开**：145 的 PG 没预加载该库，重启会打断共用测试库。
   按卡面「或 EXPLAIN ANALYZE」的口径用 `EXPLAIN (ANALYZE, BUFFERS)` 全量取证（`raw/explain_*.txt`），
   开启方法写在文档 §5.3。
3. **1 千 / 1 万行的「第 100 / 1000 页」是空页**（总量 < OFFSET），仍计入成本但不具可比性；
   真正「有数据的深页」只有 10 万行那一列，文档已逐格标注。
4. **登录的 1 万行那轮数据作废**：重复登录撞上「10 次/60 秒」限流，429 被记成 1.2 ms 的假值；
   登录改为单独口径（冷启 264.5 ms / 4 VU p95 302.8 ms），文档 §3.3。
5. 在本机另建了 `cloudpivot_perf_mig`（空库，用来验证 `V0369` 在**全新库**上能被框架正常应用），
   验证完未删（DROP 属禁区）——它只占 24 MB，不含业务数据。
