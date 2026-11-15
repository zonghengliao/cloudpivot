# 证据 · P2-63 CI 报告（JUnit XML）

> 卡 `t_0406d7e8`　父卡 `t_efb5b7f1`（P2-62 统一夹具）　机器 99 → PG `172.18.10.145`
> 设计说明：`docs/architecture/P2-63-CI报告-JUnit.md`

| 文件 | 是什么 | 关键数字 |
|---|---|---|
| `junit/junit.xml` | **真实产物**（`--package all --junit` 一次跑批，19 域合并） | `tests=5712 failures=12 skipped=1 errors=0`，root `time=36.022s`（墙钟），**5712 条逐条耗时全部量到** |
| `run-after/summary.tsv` · `run-after/failures.txt` | 改后（分域并行 + JUnit）逐域条数/墙钟 + 12 条失败 | 5711 跑（+1 ignored）/ 5699 passed / 12 failed |
| `run-after.log.txt` | 上面那次跑批的完整终端输出（含 `time`） | `real 0m40.98s` |
| `run-parallel-nojunit/summary.tsv` · `failures.txt` · `.log.txt` | **同一并行度、关掉报告**（A/B 对照） | `real 0m41.92s` |
| `run-before-single/summary.tsv` · `failures.txt` · `.log.txt` | 改前口径：`--package cloudpivot`（单进程全量） | 162.9 s · **13 红**（12 条既有 + `year_seq…` 顺序依赖） |
| `cargo-test-lib-full.time.txt` | **字面口径** `time cargo test -p cloudpivot --lib -- --test-threads=1`（改后树） | harness **162.20 s** / `real 2m42.3s`；5440 passed / **12 failed** / 1 ignored |
| `cargo-test-lib-full.failures.txt` · `.log.tail.txt` | 同上次跑的失败清单与日志尾 | 12 条 = 既有基线；`grep` 只能数到 11（见设计 §3.2） |
| `ab-hr-nojunit.txt` / `ab-hr-junit.txt` | 单域 A/B（`hr`，695 条），关/开 `--junit` 各 3 轮 | 关 9.22/9.20 s；开 9.00/8.19 s（首轮含编译已剔除） |
| `bench-report-cost.txt` | 报告生成微基准（真实 19 份日志 / 5712 条） | 解析 10 ms + 写 XML 14 ms = **24 ms（0.06%）**；逐行打戳增量 0.03 ms |
| `gate-pass.txt` | `bash scripts/verify/junit_report_gate.sh` | 17 条断言全过 → `rc=0` |
| `gate-reverse.txt` | **反证**：对真实产物删 1 条 `<testcase>` 并把计数改自洽 | 被判红（`running 174 tests 但只有 173 条真用例` + root 合计不符）→ `rc=1` |
| `check-real-junit.txt` | `python3 scripts/junit_report.py check junit/junit.xml` | 5712 条 / 12 红 / 1 跳过 / 结构自洽 → `rc=0` |
| `independent-verify.txt` | **独立解析器**（`xml.dom.minidom`）交叉验证 | 逐域 tests/failures/skipped 对齐 `summary.tsv`；失败名单对齐 `failures.txt` |
| `pitfall-dbname-62-reds.txt` | 踩坑：库名不含 `cloudpivot_test` 时的失败名单 | 62 红里 **50 条是库名闸假红**（`hr::*_db_tests` 41 + `flow::internal_control` 9） |

## 三条结论（都能在文件里复核）

1. **报告不拖慢跑批**：同并行度关/开报告 41.92 s vs 40.98 s（±2%，噪声）；生成本身 24 ms = 0.06%。
2. **报告不缺条目不吞红**：5712 条（含 1 ignored）与 `summary.tsv` 逐域对齐、12 条红与
   `failures.txt` 逐条对齐、12 条红与 P2-62 基线 `diff` 为空；用 grep 数只会得 11。
3. **改前/改后**：字面 `time cargo test -p cloudpivot --lib` **162.2 s** → 分域并行 + JUnit **36.0 s**
   （4.5×；提速来自 P2-60/P2-61，本卡只接报告）。单进程全量改后仍是 **12 红**（原有 13 红里的
   `year_seq` 顺序依赖已在本卡修掉）。

## 复现命令

```bash
# 报告 + 门禁
python3 scripts/junit_report.py selftest && bash scripts/verify/junit_report_gate.sh

# 全量（分域并行 + JUnit）
python3 scripts/test_domain.py run --package all --junit /tmp/junit
python3 scripts/junit_report.py check /tmp/junit/junit.xml

# 关掉报告的同口径对照
python3 scripts/test_domain.py run --package all

# 字面口径（单进程全量）——库名必须含 cloudpivot_test，否则 50 条假红
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test_full cloudpivot_test_p263_baseline
cd backend/app && ERP_DB_URL=postgres://cloudpivot:<口令>@172.18.10.145:5432/cloudpivot_test_p263_baseline \
  SYS_DB_URL=$ERP_DB_URL TASK_DB_URL=$ERP_DB_URL P0FIX_DB_URL=$ERP_DB_URL NIK_PROFILE=test \
  time cargo test -p cloudpivot --lib -- --test-threads=1
```

> 跑批结束的克隆库：`cloudpivot_test_p261_*` 19 份（有红时脚本按设计保留现场），已在本卡收尾时 DROP。
