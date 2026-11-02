# D1 状态机边配置化 · 验收证据（2026-11-03）

隔离库：`cloudpivot_d1`（全量单测）/ `cloudpivot_d1e2e`（真机 HTTP）/ `cloudpivot_d1inv`（不变量，零夹具）
实例：`target/debug/cloudpivot`，`NIK_PROFILE=test`，`NIK_SERVER_LISTEN=127.0.0.1:18096`

| 文件 | 内容 |
|------|------|
| `01-unit-tests.txt` | `state_machine::tests` 29 条（含 17 条 D1 新用例）真库跑绿，含审计原文打印 |
| `02-e2e-http.txt` | 真机 10 条断言：基线 200 → 配禁用 → 409「已被状态机配置禁用」→ 表改名仍 200（fail-safe）→ 软删配置 → 恢复 200 |
| `03-migration.txt` | V0204 应用记录（`schema_migrations` 204 / 148 条）+ 迁移自检 NOTICE |
| `04-psql-config.txt` | 表结构 13 列 / 唯一约束 / 存活行索引 / 空表 / 配一条禁用 / 软删恢复 |
| `05-server-log-warn.txt` | fail-safe 降级实证：`ROLLBACK TO SAVEPOINT` → WARN「读取 biz_flow_state_edge 失败」→ 紧接着 `INSERT INTO biz_status_log` 成功 |
| `06-invariants-clean-db.txt` | `erp_invariants.py` 在零夹具新库上 **36/36 通过，BLOCKER 0 / WARN 0** |
| `07-migration-roundtrip.txt` | V0204 `down`（带「还有存活配置就中止」护栏）→ `up` 回放，列数 13 / 行数 0；实例重启「应用 0 个迁移」= 指纹校验通过 |
| `08-gates.txt` | `cargo fmt -p cloudpivot -- --check` 无差异、`cargo clippy --lib -D warnings` 通过；`--all-targets` 的既有红点集中在 6 个本卡未改动的文件 |
| `09-full-suite.txt` | 全量后端单测（2027 条，真库 cloudpivot_d1，`--test-threads=1`） |

复现命令：

```bash
bash scripts/verify/d1_dbsetup.sh                       # 建 cloudpivot_d1（RECREATE=1 重建）
bash scripts/verify/d1_migrate.sh 18093                 # 跑迁移 + 核对 V0204
bash scripts/verify/d1_unit_tests.sh                    # 状态机单测（真库）
bash scripts/verify/d1_full_suite.sh                    # 全量单测（日志 /root/d1-full-test.log）
D1_DB=cloudpivot_d1inv bash scripts/verify/d1_migrate.sh 18097
NIK_DATABASE_NAME=cloudpivot_d1inv NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  python3 scripts/verify/erp_invariants.py              # 不变量
D1_DB=cloudpivot_d1e2e D1_PORT=18096 bash scripts/verify/d1_instance.sh start
D1_BASE_URL=http://127.0.0.1:18096 D1_DB=cloudpivot_d1e2e python3 scripts/verify/d1_edge_config_e2e.py
bash scripts/verify/d1_psql_evidence.sh                 # 配置表 psql 证据
```
