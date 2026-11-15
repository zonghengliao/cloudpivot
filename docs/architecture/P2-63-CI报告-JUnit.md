# P2-63【P2-D】CI 报告：libtest → JUnit XML

> 卡：`t_0406d7e8`　父卡：`t_efb5b7f1`（P2-62 统一夹具）　机器：99（开发机）→ PG 145　2026-11-15
> 证据：`docs/evidence/2026-11-15-p263-junit/`

## 1. 一句话

`cargo test`（stable libtest）**不吐任何机器可读格式** —— `--format=junit` / `--format=json` /
`--report-time` 全是 nightly-only，机器上也没有 nightly 或 nextest。所以本卡自己把 libtest 的
pretty 输出解析成 **JUnit XML**：一条用例一个 `<testcase>`、失败挂 `<failure>`（带 panic 正文）、
`#[ignore]` 挂 `<skipped>`、**逐条耗时是真量出来的**；并且顺手接进 `scripts/test_domain.py`，
`--package all --junit` 一条命令产出 CI 可直接吃的报告。

## 2. 改前 / 改后（耗时，`time` 实测）

| 口径 | 命令 | 实测 | 结果 |
|---|---|---|--:|
| **改前**（字面口径，单进程全量、一张库） | `cargo test -p cloudpivot --lib -- --test-threads=1`（cwd=`backend/app`，四条 `*_DB_URL` 同库） | **162.20 s**（harness）/ `real 2m42.3s` | 5440 passed / 12 failed / 1 ignored |
| **改后**（分域并行 + JUnit 报告） | `python3 scripts/test_domain.py run --package all --junit <dir>` | **36.0 s**（逐域 harness 合计）/ `real 41.0s` | 5711 条跑 / 5699 passed / 12 failed |
| 同并行度，**关**报告 | `… --package all`（不带 `--junit`） | `real 41.9 s` | 同上 |
| **JUnit 开关的增量** | 上两行相减 | **+0.2 s / +0.5%**（噪声级） | — |
| 报告生成本身的成本 | 解析 5712 条 + 写 730 KB XML | **24 ms**（占 43 s 的 **0.06%**） | — |
| 逐行读 + 打时间戳增量 | 803 行日志 ×100 次取最好 | **+0.03 ms** | — |

- 提速主体来自 P2-60（夹具复用）/ P2-61（分域并行）：162 s → 36 s（**4.5×**，harness 口径）。
  **本卡只负责把报告接进这条链路，代价 0.06%。**
- 单域 A/B（`hr`，695 条，关/开报告各 3 轮，首轮含编译已剔除）：关 9.20 / 9.20 s；
  开 9.00 / 8.19 s —— 报告开销在 ±5% 噪声内不可测（`ab-hr-nojunit.txt` / `ab-hr-junit.txt`）。

## 3. 为什么不能只用 grep（本卡实测的两个真坑）

### 3.1 `test <name> ... ` 与判定词会被**用例自己的日志**拆成两行

```
test shared::observability::tests::metrics_endpoint_rejects_untrusted_client ... 2026-11-15T07:51:08Z  INFO nik_web::app: 请求体上限就绪 …
ok
```

只认单行 `... ok` 的解析器会**静默少算**：本卡首轮实测 `kernel` 237/240、`platform` 43/48
（且它的 XML「结构自洽」，CI 界面一片绿）。对策：

1. `parse()` 用 pending 状态机：`test x ...` 之后若判定词不在本行，就等下一行（中间的行收进该用例的
   `<system-out>`，不丢）；
2. 耗时锚点用**判定行**（`running N` → 首条；判定 → 判定），跨行时仍是真耗时；
3. **兜底断言**：`running N tests` 的 N 必须等于解析出的用例数，否则 `<error type="report-incomplete">`
   且 `junit_report.py check` 判红。

反证（真实产物）：删掉一条 `<testcase>` 并把 suite/root 的 `tests` 改自洽（模拟静默漏报）⇒
`check` 报 `<testsuite name='attachment'> running 174 tests 但只有 173 条真用例` 并 exit=1
（`gate-reverse.txt`）。

### 3.2 `grep -c '^test .* FAILED$'` 会少数红

同一次单进程全量跑：libtest 汇总行说 `12 failed`，而 `grep '^test .* FAILED$'` 只数到 **11**
（`system::session_tests::s02_…` 的 `FAILED` 被它自己的 WARN 日志挤到下一行，见 §3.1）。
所以「用 grep 数红」既会漏**用例**也会漏**红** —— 这正是不上 JUnit 的代价。

## 4. 交付物

| 文件 | 作用 |
|---|---|
| `scripts/junit_report.py` | **生成器 + 校验器**（新增，纯 stdlib）：`run_capture()` 逐行打时间戳跑 cargo；`parse()` libtest→Suite；`to_xml()` 写 `<testsuites>`；`check_xml_text()` 结构自洽校验；`selftest` 门禁自检（17 条断言 + 5 类畸变反证）<br>子命令：`selftest` / `check <xml…>` / `from-logs <DIR> [--wall s]` / `run -p <crate>` |
| `scripts/test_domain.py` | `run` 新增 `--junit [DIR]`：逐域产 `<域>.xml` + 合并 `junit.xml`，跑完**自校验**；报告坏了 exit=2（不让 CI 静默当绿） |
| `scripts/verify/junit_report_gate.sh` | **门禁**（新增，L0 秒级、不编译不连库）：调 `junit_report.py selftest` |
| `scripts/check.sh` | L0 增设一档 + `--dry-run` 计划表同步 + 头部档位说明 |
| `backend/app/src/test_seed.rs` | **修一条顺序依赖用例**：`year_seq_walks_from_base_and_wraps` 断言了「起始偏移=0」，而 `SEQ` 是进程全局计数器 ⇒ 单进程全量跑必红（`left: 900001, right: 900000`）、分域跑碰巧绿。改成只断言契约（+1 递增 / 满 span 绕回），与起始偏移无关 |
| `docs/evidence/2026-11-15-p263-junit/` | 三次跑批 summary + 失败清单、`junit/junit.xml`（真实产物）、A/B 计时、门禁与 4 类反证、独立解析器交叉验证 |

## 5. 报告契约（CI 消费方按这个读）

```xml
<testsuites name="cloudpivot lib tests" tests="5712" failures="12" errors="0" skipped="1"
            time="36.022"            <!-- 本次跑批**墙钟**（并行时 < 各 suite 之和） -->
            timestamp="2026-11-15T08:…Z">
  <testsuite name="hr" package="cloudpivot" tests="695" failures="1" errors="0" skipped="0"
             time="11.700"           <!-- 该域 harness：running N → test result -->
             hostname="…" timestamp="…">
    <properties>
      <property name="wall_total_s" value="12.4"/>       <!-- cargo 进程墙钟（含启动） -->
      <property name="cargo_rc" value="101"/>
      <property name="per_testcase_time" value="measured"/>   <!-- 或 unavailable（from-logs） -->
      <property name="libtest_running" value="695"/>     <!-- 兜底断言用：必须 == tests -->
      <property name="filtered_out" value="5000"/>       <!-- 分域跑时被 --exact 排除的条数 -->
      <property name="sum_case_s" value="11.6"/>         <!-- 逐条耗时之和，与 harness 交叉校验 -->
      <property name="interleaved_cases" value="3"/>     <!-- 判定词被日志挤开的条数 -->
    </properties>
    <testcase classname="business::hr::salary::tests" name="calc_rounds_half_up" time="0.077">
      <failure message="assertion `left == right` failed: …" type="panic">完整 panic 块+用例 stdout</failure>
    </testcase>
    <testcase classname="…" name="bench_fixture_data_preparation" time="0.000"><skipped message="ignored (#[ignore])"/></testcase>
  </testsuite>
</testsuites>
```

三条硬约定：

1. **`tests` 必须等于 `libtest_running`**（去掉 `libtest::harness` 那条合成用例）。少算 ⇒ `check` 判红。
2. **失败用例必须有 `<failure message=…>` 且正文非空** —— 只有 `failures` 计数、没有原因的产物等于没有报告。
3. **进程被杀（没有 `test result:` 行）必须落 `<error type="harness">`**，不许当 0 红；`from-logs`
   回填老日志时逐条耗时写 `0.000` 并把 `per_testcase_time` 标 `unavailable` —— **不编假耗时**。

## 6. CI 接线（两行）

```yaml
# GitLab CI
test:
  script: python3 scripts/test_domain.py run --package all --junit reports/junit
  artifacts:
    when: always
    reports:
      junit: reports/junit/junit.xml
```

```yaml
# GitHub Actions
- run: python3 scripts/test_domain.py run --package all --junit reports/junit
  if: always()
- uses: actions/upload-artifact@v4
  if: always()
  with: { name: junit, path: reports/junit/junit.xml }
```

> `--junit` 默认把 XML 写进 `<out>/junit/`（`out` 不给就是 mktemp 目录）；也可以 `--junit DIR` 指定。
> 只想复核一份已有产物：`python3 scripts/junit_report.py check <xml>`（结构 / 计数 / 少算 / 时间全部校验）。
> 只有老日志没有实时时间戳时：`python3 scripts/junit_report.py from-logs <目录> --wall <墙钟秒>`。

## 7. 本卡的第二个发现（已修）

`test_seed::tests::year_seq_walks_from_base_and_wraps` 原来断言 `a == base`。但 `year_seq` 的
`SEQ` 是 **static（进程全局）**，所有 base 共用 ⇒ 这条用例的绿红取决于「本进程此前调用过几次
`year_seq`」的奇偶：

- 单进程全量跑（`cargo test -p cloudpivot --lib`）：此前已调过奇数次 ⇒ **必红**，
  本卡改前实测 13 红（12 条既有红 + 这条），`left: 900001, right: 900000`；
- 分域 `platform` 跑（48 条）：碰巧偶数 ⇒ 绿，所以 P2-62 的证据没抓到它。

已改成只断言契约（相邻 +1、满 span 绕回、与起始偏移无关），并写明为什么不能断言起始值。
改后单进程全量 12 红，与并行跑的 12 条**逐条相同**（`diff` 为空）。

## 8. 坑（都会让人误判，逐条实测）

1. **库名必须含 `cloudpivot_test`**：`flow::internal_control::db_tests`（P0FIX）与 `hr::fixture_hygiene`
   都有库名闸。用卡面模板 `cp_<卡号>_<用途>` 建库跑全量 lib ⇒ **62 红**（其中 50 条是闸拒绝清场造成的假红：
   `hr::*_db_tests` 41 条 + `flow::internal_control` 9 条）。用 `cloudpivot_test_<卡号>` 就只有 12 条既有红。
   → 全量 lib 的库名请用 `cloudpivot_test_*`（`test_domain.py` 的克隆库就叫 `cloudpivot_test_p261_*`）。
2. **`cargo test --lib <filter>` 在 workspace 里对每个 lib target 各打一行 `test result:`** —— 统计条数要跨行求和。
3. **XML 1.0 不许控制字符**：用例日志里的 ANSI / 进度条字符会让 ElementTree 写出「解析不了」的文件
   （实测 `not well-formed (token invalid)`）。`_xml_safe()` 统一替换成 U+FFFD。
4. **`--junit` 会在 Python 侧逐行读 pipe**：`subprocess.run(stdout=file)` 拿不到时间戳。读法唯一，
   别退回重定向（否则逐条耗时全 0）。
5. **JUnit XML 不许「看着对」**：`junit_report.py check` 是唯一判据（门禁 + 每次跑完自动跑一遍）。
   真实产物还额外过了一次**独立解析器**（`xml.dom.minidom`）交叉验证：逐域 tests/failures/skipped 对齐
   `summary.tsv`、失败名单对齐 `failures.txt`。

## 9. 复现

```bash
# ① 报告 + 门禁（秒级，不编译不连库）
python3 scripts/junit_report.py selftest
bash scripts/verify/junit_report_gate.sh

# ② 全量分域并行 + JUnit（99 上连 145；模板库 cloudpivot_test_full）
python3 scripts/test_domain.py run --package all --junit /tmp/junit
python3 scripts/junit_report.py check /tmp/junit/junit.xml

# ③ 字面口径改前（单进程全量，库名必须含 cloudpivot_test）
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test_full cloudpivot_test_p263_baseline
cd backend/app
ERP_DB_URL=postgres://cloudpivot:<口令>@172.18.10.145:5432/cloudpivot_test_p263_baseline \
SYS_DB_URL=$ERP_DB_URL TASK_DB_URL=$ERP_DB_URL P0FIX_DB_URL=$ERP_DB_URL NIK_PROFILE=test \
  time cargo test -p cloudpivot --lib -- --test-threads=1
```
