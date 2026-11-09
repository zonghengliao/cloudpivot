# ARCH-9-4（卡 t_2cd4336a）· CI/CD 与发布节奏设计 —— 实测证据

采集时间：**2026-11-10 01:04–01:25（CST）**
采集主机：99 开发机（4 核 / 15 G），PG 18.6 本地（`alun/alun`）
代码树：`/opt/cloudpivot`，HEAD `b252db2e`（工作区另有 46 个他人未提交文件）
目标库：`cloudpivot_arch9_ci4`（由 `scripts/verify/erp_clean_db.sh` 新建；298 条迁移，最高 V0367）
设计文档：`docs/architecture/ARCH-9-CICD与发布节奏设计.md`

## 文件清单

| 文件 | 内容 | 结论 |
|---|---|---|
| `01_ci_ddl_check.sql` / `.out` | §3.1 三张新表的 DDL，整段在事务里跑 + 14 组断言 + `ROLLBACK` | **14 组断言全过，`should_be_0=0`（零残留）** |
| `02_layer_timings.out` | L0/L1 层耗时（fmt / check / clippy / test / typecheck），含"假绿"对照 | L1 合计 **125.2 s**；无库单测 **4021 passed / 1.78 s**（假绿） |
| `03_gate_timings.out` | 21 项门禁 + menu_invariants + icon 逐项耗时与红绿 | **3 项红**：`biz_type_domain`(0.76 s) / `orphan_button`(0.12 s) / `scope_gate`(2.55 s) |
| `04_transferability.out` | `scripts/verify` 脚本盘点（类型/13 域/可移植性 grep 计数） | 484 个可执行脚本；**只有 35 个（7.2%）接受 `BASE_URL`** |
| `05_hosts.out` | 99 / 145 / 144 的 CPU、内存、磁盘、工具链、crontab、systemd | 99=`rustc 1.94.1`，145/144=`1.99.1`；**144 的 `erp-health-daily.sh` cron 指向不存在的脚本，静默 5 天** |

## 复跑命令（逐条）

```bash
# ① DDL 验证（整段 ROLLBACK，零残留；换库名即可）
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_arch9_ci4 \
  -v ON_ERROR_STOP=1 -f docs/evidence/2026-11-10-arch9-4-cicd/01_ci_ddl_check.sql

# ② 干净库（298 迁移 / V0367）
ERP_DB=cloudpivot_arch9_ci4 RECREATE=1 bash scripts/verify/erp_clean_db.sh

# ③ 单层门禁
cd backend && time cargo fmt -- --check
cd backend && time cargo clippy --all-targets -- -D warnings
cd backend && time cargo test --lib                      # 无库 → 假绿
cd frontend && time pnpm typecheck

# ④ 静态/DB 门禁（需库环境变量）
export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \
       NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
       NIK_DATABASE_NAME=cloudpivot_arch9_ci4
python3 scripts/verify/erp_invariants.py
bash    scripts/verify/menu_invariants.sh cloudpivot_arch9_ci4
python3 scripts/verify/biz_type_domain.py --strict        # rc=1（3 个域外值）
python3 scripts/verify/snapshot_audit.py --install-views && python3 scripts/verify/snapshot_audit.py
python3 scripts/verify/perm_menu_gate.py
python3 scripts/verify/orphan_button_gate.py             # rc=1（2 对孤儿）
python3 scripts/verify/role_code_gate.py
python3 scripts/lint/scope_gate.py --root . --baseline 21 # rc=1（5 文件 + 2 未登记）
python3 scripts/lint/palette_def_sync.py --root .
python3 scripts/verify/inventory_write_gate.py
python3 scripts/lint/changelog_in_tx.py --root .
DB=cloudpivot_arch9_ci4 bash scripts/verify/icon_invariant_teeth.sh

# ⑤ HTTP 回归（需活实例；干净库要先关掉首登改密）
#    UPDATE sys_user SET must_change_password=false WHERE username='admin';  # 有 60s 缓存，改后重启实例
cd scripts/verify && BASE_URL=http://127.0.0.1:18096 TEST_USER=admin TEST_PASS='Admin@123456' \
  DB_ENABLED=1 DB_HOST=127.0.0.1 DB_NAME=cloudpivot_arch9_ci4 DB_USER=alun DB_PASS=alun \
  python3 verify.py smoke        # 39 项 / 0.83 s / 全绿
  python3 verify.py all          # 163 pass / 8 fail / 2 skip / 34.5 s

# ⑥ 真库全量单测（本卡实测，唯一有内存的机器是 99）
URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_arch9_ci4'
ERP_DB_URL=$URL SYS_DB_URL=$URL TASK_DB_URL=$URL \
  cargo test --lib --features test-hooks -- --test-threads=1 --nocapture \
  --skip business::flow::internal_control::db_tests
```

## 本卡跑真库时抓出来的 3 个设计错误（已修正，留档）

1. **`uk_ci_run_daily` 少了 `host`** → L2 分两段（145 跑门禁、99 跑单测）时第二段会被唯一索引直接挡死。已改成 `(layer, host, 日期)`，并补断言 C-1/C-2（拒绝同主机重复 / 允许换主机）。
2. **`sys_ci_baseline.reason` 的 ≥8 字 CHECK 会把测试断言自己的短理由也拒掉** → 说明这条约束确实在生效（`第二份重复认领` 7 字被拒）；断言里已改成长理由。
3. **干净库上没有可用登录账号**：`sys_user` 里只有 `admin` 且 `must_change_password=true`，`verify.py` 第一次跑 **39 项全 403「首次登录必须先修改初始密码」**。这是"干净库跑 HTTP 回归"必踩的第一块砖 —— 设计里必须有一条「L2-A 起实例后先翻 `must_change_password`（或种一个测试账号）」，已写入 §4.1/§7 的批 3 卡。

## 未在本卡实测的项（诚实标注，确认方式写进设计 §8.4）

| 项 | 为什么没测 | 确认方式 |
|---|---|---|
| 145 上 `cargo clippy --all-targets` 耗时 | 145 上没跑过 clippy（只有 `cargo build`），本卡不往 145 加压 | 145 上 `time cargo clippy --all-targets -- -D warnings` |
| 前端 e2e 全量（424 例）耗时 | 需先起隔离实例（`e2e_isolated_instance.sh up`，含 pg_dump 副本 + 后端 + vite），本轮时间不够 | 145 上跑一次，取 `frontend/e2e-report/junit.xml` 的 `testsuite time` |
| 真库全量单测**完整一轮**的墙钟 | 共享机器上被外部 SIGTERM 打断两次（第一次 1760 例 / 1054 s；`dmesg` 无 OOM、0 条用例失败、rc=101 说明是外部信号，不是用例挂）。已拿到实测速率 1.670 例/s ⇒ 全量 ≈ 40 min | 空闲时段 `time cargo test --lib --features test-hooks -- --test-threads=1 --nocapture` 跑到底 |
| 145 的 L2-A 端到端 | 同上（依赖上面两项） | 批 3 上线后第一次 `run_layer.sh L2` 的 `sys_ci_run.duration_ms` |
