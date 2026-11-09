# ARCH-9 · CI/CD 与发布节奏设计（L-5）

| 项目 | 内容 |
|---|---|
| 卡片 | `t_2cd4336a` — ARCH9-4【L-5】CI/CD 与发布节奏设计 |
| 依据 | `docs/review/20261109-系统全面评审.md` §5.7（可维护性 3/5）、§5.8（运维就绪 3/5）、§6.2、§7.3 L-5 |
| 性质 | **纯设计**：不写实现代码、不改现有代码、不建迁移文件。文中 DDL 已在真库跑通（见附录 A / `docs/evidence/2026-11-10-arch9-4-cicd/`） |
| 实测环境 | 99 开发机（4 核 / 15 G）：`nik` 单测、PG 18.6 本地；145 测试机（4 核 / 7 G）；144 生产（4 核 / 7 G） |
| 实测时点 | 2026-11-10 01:04–01:20（树 HEAD `b252db2e`，工作区另有 46 个他人未提交文件） |

> **本文的每一个耗时数字都是本次跑出来的**，命令、退出码、日志路径见附录 A。凡没跑成的，文中标「待实测 + 确认方式」，不写「大概」。
> **本文的一个重要前提**：评审报告写的「HEAD 上 `fmt --check` 红 3 文件 / `clippy -D warnings` 红 3 处」**已经不再是事实** —— 2026-11-09 的 `845618b2`（clippy 清零 25 处 + `pre-push` 门禁）把它修掉了，本次实测 fmt 绿（3.1 s）、clippy 绿（83.6 s）。所以本设计不是「把红的修绿」，而是「把已经绿的东西**变成不可绕过的、每天自己跑的**」。

---

## 0. 结论前置（16 条定稿决策）

> 读者只想要结论就看这一节。每条后面括号里是展开章节。

| # | 决策 | 依据 |
|---|---|---|
| **D-1** | **运行位置**：L0/L1 留在开发机（99），由 git hook 机械强制；**L2 每日回归放 145 测试机**（03:10，用 145 上的 `cloudpivot_dev` + 每次重建的干净库 `cloudpivot_ci`）；**L3 发版前在 145 人工触发**。不引入 Gitea/Woodpecker（§4.1/§4.9） | 145 已跑着 30 分钟一次的 `test_deploy.sh`（实测周期 1 m 16 s–3 m 13 s），同机就有真库和 systemd 服务；99 上 `backend/target` 已 **109 G**、根盘只剩 **8.1 G**（97% used，实测），再压一套 CI 服务会挤爆 |
| **D-2** | **四层门禁的实测耗时**：L0 3.1 s；L1 **2 min 05 s**（clippy 83.6 + 单测 2.1 + typecheck 39.5，实测）；L2 **≈15–20 min**（静态门禁合计 2 min 42 s + 真库全量单测 + 干净库重建 6.4 s + 13 域抽样）；L3 1–2 h（含 e2e 424 例） | 附录 A 逐项实测 |
| **D-3** | **单测必须带库才算数**：不带库 `cargo test --lib` 是 **4021 passed / 1.78 s 的假绿**（真库用例全部 print SKIP 后 return，libtest 记 passed）。L1 只跑「无库 + 受影响模块」，**L2 跑全量真库**（`--test-threads=1`，因为库里有改全局配置的用例） | 实测 2.1 s vs check.sh 注释自述「约 12 分钟」；`scripts/check.sh:60-110` 已写明同一条判据 |
| **D-4** | **工具链必须钉死**：新增 `rust-toolchain.toml`（`1.94.1`）。当前 99 是 `rustc 1.94.1`，**145 / 144 都是 `1.99.1`** ⇒ 同一棵树在两台机器上 clippy 结论可以不同，「本机绿、CI 红」是必然 | 三台机实测 `--version`（附录 A-12） |
| **D-5** | **结果落三张新表**：`sys_ci_run`（一次运行一行）/ `sys_ci_item`（一项一行）/ `sys_ci_baseline`（已知红台账）。**不挤进** `sys_invariant_result`（理由 3 条，§3.2） | 现有 `sys_invariant_result` 是「一次巡检一行」，粒度不同 |
| **D-6** | **已知红台账是这套东西能活的前提**：今天干净库上就有 **3 项门禁是红的**（`biz_type_domain` 3 条域外值、`orphan_button` 2 对孤儿、`scope_gate` 5 文件 + 2 个未登记跳过文件）。不做台账，L2 第一天起就长红，一周后没人看 | 附录 A-6/7/9 实测 rc=1 |
| **D-7** | **日常回归不建 kanban 卡**：只在「新增红」或「同一项连续 3 次红」时建**一张**卡（`--idempotency-key ci:<item_key>:<date>`）；恢复时只在卡上 `comment`，**不自动 complete**（谁认领谁关） | 13 域演练曾一天 13 张卡；日常回归若照此会卡爆炸 |
| **D-8** | **告警通道 = 飞书**（复用 `scripts/cron/feishu_alert_send.py`，凭据留在 99 的 `laoliu/.env`，145 通过 ssh 调它，与 `snapshot_audit_notify.sh` 同一条链路）**+ 哑告警看门狗**：每天检查「L2 最近一次 SCHEDULED 运行是否在 26 小时内」，不在就报警。**实测证据：144 上 02:00 的 `erp-health-daily.sh` cron 指向一个仓库里已不存在的脚本，`/var/log/cloudpivot/erp-health-202611.log` 最后一行停在 2026-11-05 02:00 —— 静默 5 天，没有任何人发现** | 附录 A-11 |
| **D-9** | **绕过得留痕**：`SKIP_PREPUSH=1 git push` 现状只 echo 一行（`.githooks/pre-push:49`）。改造为：写 `~/.cloudpivot/prepush-skips.jsonl`（时间/用户/HEAD/改动文件数）+ 同一条飞书消息 + 一条 kanban 卡；远端 bare 的 `pre-receive` 做兜底（批 4，可选） | 现在「绕过」= 零证据 |
| **D-10** | **13 域抽样只跑「A 档可移植」脚本**：实测 484 个可执行脚本里，**113 个写死 `/etc/cloudpivot/*.env`、82 个写死 `cloudpivot-dev.env`、47 个写死 `:8033`，只有 35 个接受 `BASE_URL`** ⇒ 可移植率 7.2%。B 档（需参数化）先做迁移，C 档（依赖 145 dev 库夹具 UUID）只留 L3 人工 | 附录 A-10 实测计数 |
| **D-11** | **干净库口径唯一**：只认 `scripts/verify/erp_clean_db.sh`（`createdb -O` → 起一次实例让引擎套完迁移 → 打印指纹）。实测：**298 条迁移 / 最高 V0367 / 6.43 s 建完** | 附录 A-5 |
| **D-12** | **前端 e2e 分层**：L3 必跑全量（424 例 / 25 个 spec，`workers: 1`）；L2 每周一次夜间档（周日）。理由：e2e 串行 + 每条 120 s 上限，全量放每日会把 30 min 预算吃满 | `frontend/playwright.config.ts:44-48`（workers=1、timeout 120 s） |
| **D-13** | **不自研「CI Web UI」**：只做一个**只读看板**（跑什么、最近结果、连续红次数、台账到期），复用 `views/support/invariant` 的页面模式。门禁的**执行**仍在 145 上的 cron/hook，不进应用进程 | 应用内跑 cargo/pnpm 会拖垮服务；`sys_invariant_result` 的既有做法值得抄 |
| **D-14** | **145 永远不跑 `cargo test`**：7 G 内存，链接 test 二进制会 OOM SIGKILL；L1 必须在 99（15 G） | 145 `free -g` = 7；145 的 deploy 循环只 `cargo build`（不带 test target） |
| **D-15** | **生产（144）只跑只读巡检**：`sys_invariant_result` 每日 02:20（实测单次 3.98 s）+ `snapshot_audit` 02:00。144 与 145 同为 4 核 / 7 G，生产机还压着 PG + nginx + 应用，L2 的编译/重建一律不落生产 | 附录 A-8/A-11、`05_hosts.out` |
| **D-16** | **发版闸 = 当次 L0+L1+L2 全绿 + L3 抽样全绿**，否则 `scripts/deploy/deploy.sh` 拒跑（`--force` 才放行，且写台账）。发版节奏与 145 的 30 分钟自动部署**解耦**：自动部署只到测试环境，生产发版必须人工 + 闸 | 评审 §5.8 扣分点「发版靠人记得跑」 |

---

## 1. 业务背景与目标

### 1.1 解决什么问题

评审报告的三处原文就是问题陈述：

1. **§5.7（可维护性 3/5）**：「仓库自己写进 `check.sh` 的两条硬门禁（`fmt --check`、`clippy -D warnings`）**在 HEAD 上是红的**……⑤门禁依赖本机库/端口，无 CI workflow 证据（`scripts/check.sh` 靠人跑）」。
2. **§5.8（运维就绪 3/5）**：「无指标、无告警规则、**无外部告警通道**」。
3. **§7.3 L-5**：「干净库自动跑 13 道门禁 + 900+ 演练脚本抽样 + 前端 e2e —— 把「人工演练 13 卡」变成「每日回归」」。

翻成工程语言，现在是四个具体缺口：

| 缺口 | 现状（实测/依据） | 后果 |
|---|---|---|
| **① 门禁不可强制** | `scripts/check.sh` 有 **20 项**硬门禁（+1 项判据自检 = 21 道检查；另有可选档闸⓪），但靠人跑；`pre-push` 只覆盖 fmt/clippy/typecheck，且 `SKIP_PREPUSH=1` 一行就绕过、零留痕 | 「改一处悄悄破坏别处」没有自动挡板 |
| **② 单测会假绿** | 无库 `cargo test --lib` = **4021 passed / 1.78 s**，真库用例全部 SKIP 却被记成 passed | 最贵的那些断言（跨表勾稽、并发、约束）**日常一条都没跑** |
| **③ 演练不可复用** | 13 域 3040 条断言/152 组勾稽是人跑一次就散的；106 个域脚本里 82 个写死 145 的 `cloudpivot-dev.env` | 每次发版靠「人记得」，且**不可重放** |
| **④ 告警是哑的** | 144 的 `erp-health-daily.sh` cron 指向不存在的脚本，静默 5 天；`/metrics` 默认关 | 出事没人知道，直到客户打来 |

### 1.2 不解决什么问题（明确边界）

- ❌ **不做多环境编排**：不上 k8s、不上容器化流水线、不做蓝绿/金丝雀流量切换。当前是三台固定机器（99/145/144）+ systemd + nginx 静态托管，本设计只在这套拓扑内做自动化。
- ❌ **不做代码质量治理本身**：`clippy` 告警怎么修、覆盖率怎么提、`unsafe` 怎么审，不在本卡。本卡只负责「让它每天自动跑并对失败负责」。
- ❌ **不做安全扫描**：依赖漏洞（`cargo audit` / `pnpm audit`）、密钥泄漏、SAST 属独立能力，只在 §8 里留一张卡，不并入 L0–L3 的判据。
- ❌ **不重写 13 域演练脚本**：只做参数化迁移（B 档）+ manifest 登记，脚本本身的断言内容不动。重写是另一个数量级的成本，且现有 3040 条断言是花了 13 张卡换来的资产。
- ❌ **不做性能回归自动化**：k6 基线留在 L3 人工触发，且只在「有性能相关改动」时跑（评审 §5.5 已给 4/5，不是当前瓶颈）。
- ❌ **不做「应用内执行 CI」**：不在 Rust 进程里 `cargo build`，不在 Web 里点按钮触发编译（D-13）。

### 1.3 成功的判据（上线后怎么算成功）

| # | 判据 | 度量方式 | 达标线 |
|---|---|---|---|
| S1 | L2 每日回归在跑，且不是长红 | `sys_ci_run` 按天统计 `status` | 连续 30 天，`status='OK'` 的天数 ≥ 95%，`ERROR`（跑不起来）≤ 1 天 |
| S2 | 红项有人认领 | `sys_ci_item.status='FAIL'` 且无活跃 `sys_ci_baseline` 行的项，按 `first_seen` 排序 | 中位存活时间 ≤ 2 个工作日；无「超 7 天无人认领」的项 |
| S3 | 门禁绕过可审计 | `prepush-skips.jsonl` 行数 vs 对应 kanban 卡数 | 1:1（有绕过必有卡）；连续 30 天绕过 ≤ 2 次 |
| S4 | 发版不靠人记 | 每次生产发版的 `deploy.sh` 输出里都带 `gate=L3 head=… result=OK` | 100% 发版带闸证据；L3 一次通过率 ≥ 80% |
| S5 | 「改一处破别处」类缺陷 | 建卡的 `regression` 标签计数 | 上线后 30 天 = 0 |
| S6 | 新增脚本可被路由 | `scripts/regression/manifest.yml` 的覆盖断言（每个 `scripts/verify/*.sh|py` 必须在 manifest 里有一行，或显式标 `layer: [L3] manual`） | CI 自检 100% 覆盖，缺一行就红 |

> S6 是这套设计的**自守门禁**：manifest 与脚本目录不同步时，「每日回归」会静默漏跑，这比红更危险。做法与 `scripts/lint/scope_gate.py` 的「跳过清单必须登记」同一条思路（那个断言今天正好红了，见附录 A-9）。

---

## 2. 与现状的关系

### 2.1 现有资产盘点（含今天的实测状态）

| 资产 | 位置 | 现状（2026-11-10 01:04 实测/直读） | 本设计怎么用 |
|---|---|---|---|
| 提交门禁 | `scripts/check.sh`（843 行） | **20 项**硬门禁（+ 库存写入口的判据自检 = 21 道）+ 可选档闸⓪：fmt / clippy / check / typecheck / B闸① / B闸② / C7-G1..G6 / 色板 / 库存写入口（含自检）/ 闸③④⑤⑥⑦⑧ | 拆成 L0/L1/L2 三层复用，**不重写判据** |
| 本地 hook | `.githooks/pre-commit`（只查暂存 `.rs` 的 rustfmt）、`.githooks/pre-push`（fmt + clippy + typecheck，`SKIP_PREPUSH=1` 可绕） | 已装（`core.hooksPath=.githooks`），**今天刚由 `845618b2` 加** | L0/L1 的执行体；本设计只补「留痕」与「test」 |
| 测试机部署循环 | 145 `/root/test_deploy.sh`（307 行，cron 每 30 min） | 拉代码 → 增量 `cargo build`（实测 58 s–2 m 13 s）→ 前端 build（需 4096 MB 堆）→ 逐条迁移 → 重启 → health + 进程/磁盘 md5 比对 → `menu_invariants.sh`（**只告警不阻断**）→ 全周期实测 1 m 16 s–3 m 13 s | L2 的宿主；**回归任务挂在它同一台机、错峰时段** |
| 内置调度器 | `backend/src/task/mod.rs`（`Scheduler::register_builtin` + `register_cron`）、`sys_job` 表 | 已有 12+ 个内置任务；`invariant_audit` 每天 02:20 起子进程跑 `erp_invariants.py` → 落 `sys_invariant_result` | **不在应用内跑编译**；但 L2 的「干净库迁移 + 巡检」这一段可以复用同一套 runner 形态（§4.1 给出边界） |
| 巡检结果表 | `sys_invariant_result`（V0235） | 生产 144 有 1 行：`2026-11-09 10:20 SCHEDULED FAIL 79 项 / 2 失败 / 1 BLOCKER / duration_ms=3976` | 保留不动；L2 的 erp_invariants 结果继续由它承载 |
| 巡检脚本 | `scripts/verify/erp_invariants.py`（155 KB） | 干净库 80 项 **4.50 s**；生产 3.98 s（79 项） | L2 的闸③ |
| 菜单不变量 | `scripts/verify/menu_invariants.sh`（8 条断言） | 干净库 **0.39 s** 全绿；145 dev 上也是全绿（433 条菜单，`deploy_check` 日志） | L2 的第 9 道静态闸（**不在 check.sh 里**，需并入） |
| 演练脚本库 | `scripts/verify/` **547 个文件**（`.sh` 312 / `.py` 137 / `.mjs` 35 / `.sql` 37 + 目录） | 其中 13 域前缀（`e2e01..e2e14`）**106 个**；113 个写死 `/etc/cloudpivot`，82 个写死 `cloudpivot-dev.env`，47 个写死 `:8033`，**只有 35 个接受 `BASE_URL`** | L2 抽样 + L3 全量（§4.4 三档） |
| 统一接口回归 | `scripts/verify/verify.py`（smoke/a/b/c/task/flow 6 组） | 干净库上 `smoke` **0.83 s / 39 项全绿**；`all` **34.5 s / 163 pass / 8 fail / 2 skip** | **L2 首选的「已可移植」HTTP 回归**（§4.4 A 档核心） |
| 前端 e2e | `frontend/e2e`（25 个 spec / **424 例**）、`playwright.config.ts` | `workers:1`、单例超时 120 s、默认打隔离实例 `127.0.0.1:5189`（`e2e/target-guard.mjs` 硬判据，非回环端口直接抛错） | L3 全量 + L2 每周档 |
| 干净库脚本 | `scripts/verify/erp_clean_db.sh` | 实测 **6.43 s**：建库 + 298 条迁移（最高 V0367）+ bootstrap + 指纹打印 | L2 每次重建库的唯一口径 |
| 告警发送器 | `scripts/cron/feishu_alert_send.py` + `scripts/cron/snapshot_audit_notify.sh` | 144 → ssh 99 → 飞书 v1 卡片；发完 GET 回读校验正文 | L2/L3 失败的投递通道 |
| 生产 cron（144） | `crontab -l` | 3 条：`erp-health-daily.sh`（**脚本已不存在，静默 5 天**）、`snapshot_audit.sh`（正常）、`backup_db.sh --keep 14` | D-8 的看门狗对象 |

### 2.2 今天在 99 上逐门禁实跑的红绿（干净库 `cloudpivot_arch9_ci4`，HEAD `b252db2e`）

| 门禁 | 退出码 | 耗时 | 结论 | 说明 |
|---|---|---|---|---|
| `cargo fmt -- --check` | 0 | 3.10 s | 🟢 | 评审说的「红 3 文件」已被 `845618b2` 修掉 |
| `cargo clippy --all-targets -- -D warnings` | 0 | 83.6 s | 🟢 | 同上（清零 25 处） |
| `cargo check` | 0 | 12.6 s（touch 后） | 🟢 | 纯热 0.2 s |
| `pnpm typecheck` | 0 | 39.5 s | 🟢 | |
| 批次B 闸② `scope_gate.py` | **1** | 2.55 s | 🔴 | **5 个文件**查询未接 scope；另有 **2 个测试面文件未登记 `SKIP_MANIFEST`**（`shared/test_helpers.rs`、`business/task/task_attachment_snapshot_tests.rs`）|
| 批次B 闸① 分页写法 | — | <0.1 s | 🟢 | 0 处 `paginate_with_params` |
| C7 G1–G6 | 0 | 3.38 s（含 G5 词法检查） | 🟢 | |
| 色板一致性 | 0 | 0.04 s | 🟢 | 22 套色板 |
| 库存写入口（+自检） | 0 | 2.18 s | 🟢 | |
| 闸③ `erp_invariants.py` | 0 | 4.50 s | 🟢 | 干净库 80 项 |
| 闸④ `biz_type_domain.py` | **1** | 0.76 s | 🔴 | **`biz_flow_def.biz_type` 3 个域外值：`PR_APPROVE`（V0331）/ `PRT_APPROVE`（V0332）/ `SO_APPROVE`（V0281）三条种子流程定义** |
| 闸⑤ `snapshot_audit.py` | 0 | 1.12 s | 🟢 | 26 项，BLOCKER 0（需先 `--install-views`，1.37 s） |
| 闸⑥ `perm_menu_gate.py` | 0 | 3.50 s | 🟢 | |
| 闸⑦ `orphan_button_gate.py` | **1** | 0.12 s | 🔴 | **`SALES_CLERK` × `statement:view/export` 两对孤儿按钮**（父页 `/business/erp/receivable` 未授） |
| 闸⑧ `role_code_gate.py` | 0 | 0.45 s | 🟢 | |
| `menu_invariants.sh`（8 条） | 0 | 0.39 s | 🟢 | |
| `icon_invariant_teeth.sh` | 0 | 4.12 s | 🟢 | 2356 个图标名 |
| `verify.py smoke`（39 只读） | 0 | 0.83 s | 🟢 | |
| `verify.py all`（163/8/2） | **1** | 34.5 s | 🟡 | 8 条失败**全部是环境前提**（下条） |

**`verify.py all` 的 8 条失败逐条归因**（这决定了 L2 的第一天怎么起步）：

| 类别 | 条数 | 具体 | 处置 |
|---|---|---|---|
| 无数据可导 | 2 | 「同步导出返回文件流」422、「异步导出建任务」10001「没有可导出的数据」 | 脚本自己建数据，或标 `requires_fixture: export_rows`（批 2） |
| 本机无法派 AI 活 | 4 | 「派活到 Hermes kanban 失败：`delegate_task` child contexts cannot mutate Kanban」 | 这 4 条在 CI 机器上**结构性不可跑**，manifest 标 `layer: [L3] manual` |
| 用例落后于新守卫 | 2 | 流程发布被 `65048`（全员会签无超时/无兜底人）拦下 → 后续「会签实例创建」连带红 | 更新用例（把会签配成 1 人或加兜底人），属**用例维护**不是产品缺陷 |

> 结论：**「干净库全绿」今天不成立，差 3 项门禁 + 8 条用例**。所以 D-6（台账）与 §4.5 的「起步三件事」不是可选项。

### 2.3 新增 vs 改造（不破坏任何现有数据）

| 类别 | 内容 | 破坏性 |
|---|---|---|
| **新增表** | `sys_ci_run` / `sys_ci_item` / `sys_ci_baseline`（§3.1，DDL 已真库跑通） | 无（纯新表，无 FK 指向既有表） |
| **新增配置** | `rust-toolchain.toml`（D-4）、`scripts/regression/manifest.yml`（S6） | 无 |
| **新增脚本** | `scripts/regression/run_layer.sh`（调度）、`collect.py`（解析/落库）、`watchdog.py`（哑告警）、`prepush-audit.sh` | 无 |
| **改造（最小）** | `.githooks/pre-push`：`SKIP_PREPUSH` 分支加留痕（3 行）；145 `crontab` 加一条 03:10；145 `/root/test_deploy.sh` **不动** | 无（只加不改语义） |
| **改造（可选，批 4）** | 远端 bare `/opt/git/cloudpivot.git` 的 `hooks/pre-receive` | 有风险（配错会挡住全部 push）→ 单独批 + 演练 |
| **绝不动** | `sys_invariant_result` 结构、`erp_invariants.py` 判据、`check.sh` 的判据逻辑、13 域脚本的断言内容、已应用迁移 | — |

### 2.4 与 ARCH-1~8 已设计/已落地设计的关系

| 设计 | 关系 | 说明 |
|---|---|---|
| **ARCH-9-1 多组织/合并报表** | **依赖**（它依赖本设计） | 它会把 `org_id` 加到 40+ 张表 ⇒ `erp_invariants.py` 与 `scope_gate.py` 必须能覆盖带 org 的查询；本设计的 manifest 需要支持 `required_when: org_enabled`（§4.4）。**不冲突**：本设计不改任何表 |
| **ARCH-9-2 人力资源** | 并行 | 新增 10 张表 ⇒ L2 的「干净库迁移」会自动覆盖；13 域扩到 14 域（新增「人力」域），manifest 预留 `domain: hr` |
| **ARCH-9-3 成本核算** | 并行 | 新增 13 张表 + 5 类新不变量 ⇒ `erp_invariants.py` 要加项（该卡已列 `COST_UNSETTLED` 等）；本设计只提供**执行器**，判据归各设计卡 |
| **ARCH-4 MES/委外** | 复用 | 它的 `TxnBizType::ProductionIn` 等词表值会被闸④校验；MES 上线后 L2 需加「工单链 e2e」域 |
| **ARCH-5 工作流引擎** | 复用 | 13 域的 E2E-10（流程引擎，376 断言）是 L3 的必跑域；`biz_flow_def.biz_type` 那 3 个域外值就在这条链上 |
| **ARCH-3 勾稽规则** | 复用 | 152 组勾稽的 SQL 已落在各域脚本里；本设计不重写，只做 manifest 登记 + 抽样调度 |
| **ARCH-8 职责分离** | 复用 | E2E 里大量「自己审自己被拦」的断言，是本设计最值钱的那部分回归 |

---

## 3. 数据模型

### 3.1 新增表（完整 DDL，已在 PG 18.6 真库跑通并整段 ROLLBACK，零残留）

> 验证证据：`docs/evidence/2026-11-10-arch9-4-cicd/01_ci_ddl_check.sql` + `.out`（**14 组断言 A–J 全过**，`should_be_0 = 0`，零残留）。
> 迁移文件号：**V0368 / V0369**（实测当代最大 `V0367__flow_all_role_approval_to_any_multiversion.up.sql`，`ls backend/migrations/*/` 核对过）。

#### ① `sys_ci_run` —— 一次运行一行

```sql
CREATE TABLE sys_ci_run (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),           -- 主键
    run_at TIMESTAMPTZ NOT NULL,                            -- 本次运行开始时间
    layer VARCHAR(8) NOT NULL,                              -- L0 提交前 / L1 push 前 / L2 每日 / L3 发版前
    trigger_type VARCHAR(16) NOT NULL,                      -- SCHEDULED / HOOK / MANUAL / DEPLOY
    git_head VARCHAR(40) NOT NULL,                          -- 运行的提交（40 位，short 只用于展示）
    git_branch VARCHAR(128) NOT NULL DEFAULT '',            -- 分支名
    worktree_dirty BOOLEAN NOT NULL DEFAULT false,          -- 工作区是否有未提交改动（脏树结论不可用于发版）
    toolchain VARCHAR(64) NOT NULL DEFAULT '',              -- rustc 版本（D-4：版本漂移必须留证）
    host VARCHAR(64) NOT NULL DEFAULT '',                   -- 运行主机（99/145/144）
    database_name VARCHAR(128) NOT NULL DEFAULT '',         -- 巡检/测试目标库快照
    status VARCHAR(16) NOT NULL,                            -- RUNNING / OK / FAIL / ERROR / ABORTED
    total INT NOT NULL DEFAULT 0,                           -- 项数
    passed INT NOT NULL DEFAULT 0,                          -- 通过数
    failed INT NOT NULL DEFAULT 0,                          -- 失败数
    skipped INT NOT NULL DEFAULT 0,                         -- 跳过数（环境不具备）
    baselined INT NOT NULL DEFAULT 0,                       -- 命中已知红台账数（不算失败，但必须落台账）
    duration_ms INT NOT NULL DEFAULT 0,                     -- 本次总耗时
    log_dir TEXT NOT NULL DEFAULT '',                       -- 日志目录（脚本 stdout/stderr 落盘处）
    note TEXT,                                              -- 备注（例如「--force 绕过」的原因）
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

COMMENT ON TABLE  sys_ci_run IS 'CI/回归一次运行一行（L0/L1/L2/L3；门禁 + 脚本 + e2e 的汇总）';
COMMENT ON COLUMN sys_ci_run.worktree_dirty IS '运行时工作区是否有未提交改动（脏树跑的结论不可用于发版）';
COMMENT ON COLUMN sys_ci_run.baselined IS '命中「已知红台账」的失败项数（不阻断，但必须在 sys_ci_baseline 有行）';
COMMENT ON COLUMN sys_ci_run.log_dir IS '本次运行的日志目录';

ALTER TABLE sys_ci_run
    ADD CONSTRAINT ck_ci_run_layer   CHECK (layer IN ('L0','L1','L2','L3')),
    ADD CONSTRAINT ck_ci_run_trigger CHECK (trigger_type IN ('SCHEDULED','HOOK','MANUAL','DEPLOY')),
    ADD CONSTRAINT ck_ci_run_status  CHECK (status IN ('RUNNING','OK','FAIL','ERROR','ABORTED')),
    ADD CONSTRAINT ck_ci_run_counts  CHECK (passed >= 0 AND failed >= 0 AND skipped >= 0 AND baselined >= 0
                                            AND passed + failed + skipped + baselined <= total);

CREATE INDEX idx_ci_run_at     ON sys_ci_run (run_at DESC);
CREATE INDEX idx_ci_run_not_ok ON sys_ci_run (run_at DESC) WHERE status <> 'OK';

-- 同一台主机同一层同一个自然日（北京时间）只允许一次定时运行：
-- 两次 cron 撞车 / 手工重跑会把趋势图的「每日成功率」打歪（S1 直接依赖它）。
--   ★ host 必须进键：L2 天然分两段跑在两台机上（145 跑门禁与 13 域抽样、99 跑真库全量单测，
--     因为 test 二进制链接实测要 7.7 GiB RSS，145 只有 7 G 内存必然 OOM）。
--     不带 host 时第二段会被唯一索引挡死 —— 这是真库跑断言时抓出来的第一个设计错误
--     （证据：01_ci_ddl_check.out 断言 C-1/C-2）。
CREATE UNIQUE INDEX uk_ci_run_daily
    ON sys_ci_run (layer, host, ((run_at AT TIME ZONE 'Asia/Shanghai')::date))
    WHERE trigger_type = 'SCHEDULED' AND status <> 'ABORTED';
```

- **`host` 由采集器显式取 `hostname`，取不到就写 run 为 `ERROR`，绝不落空串** —— 空串会让两台机的 L2 段撞同一个唯一键（DDL 里的 `DEFAULT ''` 只是为了让存量/测试插入不必改写，正常运行路径必须显式给定值）。
- S1 的「每日成功」口径因此改为：**按天取最差**（同一天两条 L2，任一条 FAIL/ERROR 就算当天不绿）。

**为什么 `status <> 'ABORTED'` 要排除在唯一索引外**：回归跑到一半被人 `Ctrl-C`／机器重启，应当允许当天再跑一次。这与 `biz_cost_period` 的唯一键必须排除 `REVERSED`（ARCH9-3 踩过的坑）是同一类教训 —— **唯一索引漏一个状态，就是一次死单**。

#### ② `sys_ci_item` —— 一项门禁/脚本/用例一行（append-only）

```sql
CREATE TABLE sys_ci_item (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    run_id UUID NOT NULL REFERENCES sys_ci_run (id) ON DELETE CASCADE, -- 所属运行
    item_key VARCHAR(64) NOT NULL,                          -- 稳定标识：fmt / clippy / erp_invariants / e2e04_finance / e2e:05-approval.spec.ts
    domain VARCHAR(24) NOT NULL DEFAULT '',                 -- 所属域（procurement/sales/…/gate）；用于 13 域覆盖率统计
    kind VARCHAR(16) NOT NULL,                              -- GATE / SCRIPT / E2E / DBTEST
    required BOOLEAN NOT NULL DEFAULT true,                 -- 是否必过（false = 抽样轮到的可选脚本，红了只记录）
    status VARCHAR(16) NOT NULL,                            -- PASS / FAIL / SKIP / ERROR
    baseline BOOLEAN NOT NULL DEFAULT false,                -- 是否命中已知红台账
    rc INT,                                                 -- 进程退出码
    duration_ms INT NOT NULL DEFAULT 0,                     -- 本项耗时
    failures INT NOT NULL DEFAULT 0,                        -- 断言失败条数（脚本自报时填）
    first_line TEXT,                                        -- 失败首行（裁剪 500 字，不含 token/口令）
    log_path TEXT,                                          -- 该脚本的完整日志
    log_sha256 CHAR(64),                                    -- 日志指纹（判「两次红是否同一现场」）
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

COMMENT ON TABLE  sys_ci_item IS 'CI 运行里的一项（门禁 / 演练脚本 / e2e 用例 / 真库单测分片）；append-only';
COMMENT ON COLUMN sys_ci_item.item_key IS '稳定标识，用于跨运行比对（同一 key 的历史即趋势）';
COMMENT ON COLUMN sys_ci_item.required IS 'false = 抽样轮到的可选脚本：红了记录但不阻断';
COMMENT ON COLUMN sys_ci_item.first_line IS '失败首行（裁剪到 500 字，不含 token / 口令）';

ALTER TABLE sys_ci_item
    ADD CONSTRAINT ck_ci_item_kind   CHECK (kind IN ('GATE','SCRIPT','E2E','DBTEST')),
    ADD CONSTRAINT ck_ci_item_status CHECK (status IN ('PASS','FAIL','SKIP','ERROR'));

CREATE UNIQUE INDEX uk_ci_item_run_key ON sys_ci_item (run_id, item_key);  -- 同一运行同一项不许两行
CREATE INDEX idx_ci_item_key_time ON sys_ci_item (item_key, created_at DESC); -- 趋势 / 连续红计数
```

#### ③ `sys_ci_baseline` —— 已知红台账（这套东西能不能活的关键）

```sql
CREATE TABLE sys_ci_baseline (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    item_key VARCHAR(64) NOT NULL,                          -- 对应 sys_ci_item.item_key
    reason TEXT NOT NULL,                                   -- 认领理由（≥8 字，禁「暂时」「WIP」这类空话靠长度兜底不了，靠评审兜底）
    owner VARCHAR(64) NOT NULL,                             -- 认领人（profile 名或姓名）
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TIMESTAMPTZ NOT NULL,                        -- 到期即失效（查询侧判 > now()）
    revoked_at TIMESTAMPTZ                                  -- 提前撤销（修好后清账；行保留可审计）
);

COMMENT ON TABLE  sys_ci_baseline IS '已知红台账：门禁长期红的显式认领（有理由 + 有到期 + 有人）';
COMMENT ON COLUMN sys_ci_baseline.expires_at IS '到期时间；查询侧判 expires_at > now()，过期行不参与 baseline 判定';

ALTER TABLE sys_ci_baseline
    ADD CONSTRAINT ck_ci_baseline_reason     CHECK (length(btrim(reason)) >= 8),
    ADD CONSTRAINT ck_ci_baseline_window     CHECK (expires_at > created_at),
    ADD CONSTRAINT ck_ci_baseline_max_window CHECK (expires_at <= created_at + interval '90 days');

CREATE UNIQUE INDEX uk_ci_baseline_key ON sys_ci_baseline (item_key) WHERE revoked_at IS NULL;
CREATE INDEX idx_ci_baseline_active    ON sys_ci_baseline (expires_at) WHERE revoked_at IS NULL;
```

**三条硬约束的理由**（都是「没有它就会退化成关闸」）：

1. `length(btrim(reason)) >= 8` —— 一句「暂缓」就能把门禁关掉，等于没有门禁。8 字是最低限度的「说清是什么」。
2. `expires_at <= created_at + 90 days` —— 无限期台账 = 永久关闸。90 天是「一个季度内必须重新决定」，到期自动重新变红，逼人再看一眼。
3. `uk_ci_baseline_key ... WHERE revoked_at IS NULL` —— 同一项只允许一份活跃认领；修好后 `revoked_at` 清账，历史行留着就能回答「这项红过几次」。

### 3.2 为什么不复用 `sys_invariant_result`

| 维度 | `sys_invariant_result`（V0235） | 本设计需要 | 结论 |
|---|---|---|---|
| 粒度 | **一次巡检运行一行**，明细塞 `detail JSONB` | 一次运行 **N 项**，每项要跨运行比对（连续红计数、趋势、`first_seen`） | 粒度不同；挤进去只能塞 JSONB，`WHERE item_key='x' ORDER BY created_at` 就没法走索引 |
| 生产者 | 内置 `invariant_audit`（Rust 起子进程，写库） | **外部脚本**（145 上的 shell/python 跑 clippy/cargo/e2e，机器上根本没有应用进程在跑） | 生产者不同；145 上贴库写要额外开权限，而 145 的 PG 只监听 127.0.0.1（可从本机写，但还要读 145 → 99 的通道） |
| 语义 | `status` 只有 `OK/FAIL/ERROR`，判的是**账务** | 要区分 `SKIP`（环境不具备）/`baselined`（已知红）/**`worktree_dirty`**（脏树结论不可发版） | 语义不够 |

**结论**：`erp_invariants` 的结果**继续**落 `sys_invariant_result`（不动），`sys_ci_item` 里只留一行 `item_key='erp_invariants'` 的汇总（`rc` + 耗时 + `log_path`），两边用 `run_at` 对齐。**一份判据、两处视角**，不制造第二份真相。

### 3.3 数据量与保留

| 表 | 单次行数 | 频次 | 年增行数 | 保留 |
|---|---|---|---|---|
| `sys_ci_run` | 1 | L2 每日 1 + L1 每次 push（估 5/日）+ L3 每次发版 | ≈ 2000–2500 | 365 天 |
| `sys_ci_item` | L2 ≈ 130（21 门禁 + 8 静态 + 真库单测 1 行 + 抽样 ~100）；L3 ≈ 620（含 424 e2e） | 同上 | ≈ 90 000（L2 47 k + L1 10 k + L3 30 k） | 180 天（明细）/ 汇总永久 |
| `sys_ci_baseline` | — | 人工 | < 100 | 永久（含 revoked 行） |

清理策略：复用现有 `sys_job` 内置任务形态（如 `help_version_cleanup` 每天 02:40），新增 `ci_history_cleanup`（每天 03:50，与日志清理错峰）：删 `sys_ci_item.created_at < now() - 180 days`，级联不影响 `sys_ci_run`（父表先算汇总，子表可删）。**注意**：删明细前先把 `sys_ci_run` 的 `total/passed/failed` 固化（它们本来就是汇总列，不受影响）。

### 3.4 与现有表的外键关系

- `sys_ci_item.run_id → sys_ci_run.id`（`ON DELETE CASCADE`）—— 唯一一条 FK，两端都是新表。
- `sys_ci_baseline.item_key` **不加 FK 到 `sys_ci_item`**：item 明细 180 天后会被清理，台账要活 90 天且必须独立存在（这正是 ARCH9-3「`lot_no` 文本列不加物理 FK」的同一条判断：跨生命周期的引用加 FK 只会让历史行改不动）。
- **不引用** `sys_user`：`owner` / `trigger_user_id` 存文本快照。理由按 AGENTS.md 约定 13 —— 审计/历史类**存快照**（改名/删号后要能回溯「当时是谁」）。若要展示「现在是谁」，前端用 `owner` 二次 join 现库（可选，不阻塞）。

## 4. 业务规则与流程

### 4.1 分层定义与触发矩阵

| 层 | 何时 | 哪台机 | 跑什么 | 实测/目标耗时 | 失败动作 | 结果落哪 |
|---|---|---|---|---|---|---|
| **L0** | 提交前（`pre-commit` + 手动 `run_layer.sh L0`） | 99 开发机 | `cargo fmt -- --check`（**实测 3.10 s**）+ `cargo check`（**touch 后 12.6 s / 纯热 0.2 s**） | **≈ 16 s** | 拒绝提交（机械） | 控制台；不落库（太频繁） |
| **L1** | push 前（`pre-push`） | 99 开发机 | ① `cargo clippy --all-targets -- -D warnings`（**实测 83.6 s**，含 crate 重编）② `cargo test --lib`（**实测 2.1 s / 4021 passed**，无库口径见 §4.3）③ `cd frontend && pnpm typecheck`（**实测 39.5 s**） | **热态合计 125.2 s ≈ 2 min 05 s**；**改代码后的真实成本 ≈ 175.6 s ≈ 2 min 56 s**（多了 test 二进制重编 50.4 s，实测 A-18）—— 仍达标（<3 min）但只剩 4 s 余量 | 拒绝 push；`SKIP_PREPUSH=1` 可绕但留痕（§4.6） | `sys_ci_run`（layer=L1, trigger=HOOK）+ item 3 行 |
| **L2-A** | 每日 **03:10**（145，cron） | **145 测试机** | ① 干净库重建（**实测 6.43 s / 298 迁移**）② **21 道**静态+DB 检查（即 `check.sh` 全部，含 fmt/clippy/typecheck）+ `menu_invariants.sh`（8 条）+ `icon_invariant_teeth.sh` ③ `verify.py all`（HTTP 回归，**实测 34.5 s**）④ 13 域抽样（§4.4） | 静态部分实测合计 **2 min 42 s**（99 热 target）；145 上 clippy 无缓存，**待实测**（确认方式：145 跑一次 `cargo clippy --all-targets` 计时，见 §8.4 待确认 2）；+ 抽样 ≈ 10–15 min | 不阻断主干，**建卡 + 飞书**（§4.8） | `sys_ci_run`（host=145）+ 每项一行 |
| **L2-B** | 每日 **03:40**（99，cron） | **99 开发机** | 真库全量单测：重建库 + `cargo test --lib --features test-hooks -- --test-threads=1 --nocapture`（`ERP_DB_URL`/`SYS_DB_URL`/`TASK_DB_URL` 三把都给） | **实测 ≈ 40 min**（见附录 A-15 的两轮实测与外推；全程 4014 例）—— 超「<30 min」目标，处置见下 | 建卡 + 飞书 | `sys_ci_run`（host=99）+ item 1 行（DBTEST） |
| **L3** | 发版前人工触发（`run_layer.sh L3`） | 145 | ① 全部 484 个可执行 verify 脚本 + 37 个 `.sql` 证据脚本 ② 前端 e2e 全量（**424 例 / 25 spec**，`workers:1`）③ k6 性能基线（有性能改动时） | 40–90 min（e2e 串行是主项，**待实测**，见 §8.4 待确认 3） | **阻断发版**（`deploy.sh` 拒跑） | `sys_ci_run`（layer=L3）+ 每项一行 |

**为什么 L2 拆两段（关键决策）**：

- 真库全量单测**只能在 99** 跑：本次实测 `cargo test --lib --features test-hooks` 的 rustc 进程 RSS 峰值 **7.7 GiB**（`ps` 的 `%MEM=51.3%` × 15 G），145 只有 7 G —— 必然 OOM SIGKILL（这也是 145 的部署循环只 `cargo build` 不带 test target 的原因）。
- 门禁与 13 域抽样**应该在 145**：那里有真库 `cloudpivot_dev`、有正在跑的 `cloudpivot-test` 服务、有 30 分钟一次的部署循环保证二进制是最新主干（实测 145 增量编译 58 s–2 m 13 s）。在 99 上跑「HTTP 回归」反而要自己起实例，多一层不确定。
- 两段各自 `sys_ci_run` 一行，靠 `(layer, host, 日期)` 唯一索引并存（§3.1 断言 C-2），每日健康取**最差**。

**为什么 L2 要重跑一遍 clippy/typecheck（L1 已经跑过）**：L1 跑的是**各人的工作树**，L2 跑的是**主干合并后的结果**。两个人各自绿、合起来红（语义冲突、迁移号撞车、同名函数）是这类仓库最常见的故障，只有主干上跑才看得见。代价 2 分钟，值得。

**干净库上跑 HTTP 回归必踩的两个坑（本卡实测，实现时必须提前处理）**：

1. **没有可登录账号**：干净库的 `sys_user` 只有 `admin` 且 `must_change_password=true`；`verify.py smoke` 第一次跑 **39 项全部 403「首次登录必须先修改初始密码」**（0.76 s，全红）。修法二选一：起实例后 `UPDATE sys_user SET must_change_password=false WHERE username='admin'`（**有 60 s 缓存，改完要重启实例**），或 migrate 后种一个 `ci_runner` 账号（推荐，避免动 admin）。写进 B3-1 卡。
2. **没有可导出的数据**：导出类用例（2 条）在空库上必然 422/10001。修法是脚本自建数据（B 档迁移）或 manifest 标 `needs: fixtures:self`。

**L2 的 30 分钟预算：实测打不住了（这条必须写清楚）**

| 段 | 内容 | 实测/估算 | 结论 |
|---|---|---|---|
| L2-A（145） | 干净库 6.4 s + 静态门禁 ≈162 s + `verify.py all` 34.5 s + 抽样 10–15 min | **≈ 14–18 min** | 达标 |
| L2-B（99） | 真库全量单测（4014 例，`--test-threads=1`） | **≈ 40 min**（实测速率 1.670 例/s，见附录 A-15） | **超出「<30 min」目标** |

原因不是脚本慢，是**库测试本身就是串行的**：它们改全局配置（流程定义 / 状态机边 / 期末结账），并行会互踩（`check.sh` 里已经为 `internal_control::db_tests` 单独 `--skip` 就是这个原因）。所以 `--test-threads=1` 不是保守，是必须。

**定稿处置（三档，先做第 1 档）**：

1. **接受 40 分钟、两段并行跑**（L2-A 与 L2-B 同时 03:10 起，各自落一行 run）→ 墙钟 = max(A, B) ≈ 40 min。理由是**凌晨没人等**，「<30 min」这个数字对 L2 没有业务含义（对 L1 才有）。
2. 若要压到 30 min 内：按**模块分片 × 独立库并行**（4 片 × 4 个 `cloudpivot_ci_N` 库），前提是先把「改全局配置的用例」打标隔离（`--skip` 那一组单独串行跑）。**估 12–15 min**，但要额外验证分片之间没有隐式共享（快照表 / 通知表 / 序列）。
3. 不做的选项：`--test-threads=4` 直接并行 —— **会假红**（同库互踩），已在 `check.sh` 的注释里写明代价，别试。

### 4.2 状态机

```
sys_ci_run:   RUNNING ──┬─→ OK        所有 required 项 PASS（或命中未过期的台账）
                        ├─→ FAIL      有 required 项 FAIL（代码问题 → 开发处置）
                        ├─→ ERROR     环境性失败：连不上库 / 机器掉线 / 脚本没跑起来（运维处置）
                        └─→ ABORTED   人工中止 / 超时被强杀
              终态之间不可互转；不回到 RUNNING（append-only，重跑就是新的一行）

sys_ci_item:  PASS / FAIL / SKIP / ERROR —— 一经写入不再修改（append-only）
              SKIP 只在「环境不具备」时用（例如目标机没有 node），**判据缺失不许用 SKIP 掩盖**
```

- **FAIL 与 ERROR 必须分开**：处置人不同、SLA 不同。混在一起会出现「脚本路径写错」被当成「账错了」，把人引到错的方向（ARCH9-3 在 `biz_cost_period` 上也踩过「错误分类不清导致死单」的坑）。
- **没有「WARN」这个状态**：要么过、要么不过、要么环境不行。台账（`baselined=true`）已经承担了「暂时放过」的语义，再加一档 WARN 会让人当它是过。

### 4.3 单测口径（这条不解决，整套东西都是表演）

实测对比，同一棵树、同一个命令，只差「有没有库」：

| 口径 | 命令 | 结果 | 含义 |
|---|---|---|---|
| 无库（现状 `check.sh` 默认） | `cargo test --lib` | **4021 passed / 0 failed / 1.78 s** | 所有真库用例 `println!("SKIP: 未设置 ERP_DB_URL"); return;` → libtest **记 passed**。**一条真库断言都没跑** |
| 无库、单线程 | `cargo test --lib -- --test-threads=1` | 4021 passed / **6.03 s** | 同上，只是慢 4 倍 |
| 有库、全量、单线程 | `cargo test --lib --features test-hooks -- --test-threads=1 --nocapture` | **见附录 A-15** | 真正跑真库断言 |

**L1/L2 的口径分工**（直接抄 `scripts/check.sh:60-110` 已写死的三条硬判据，不重新发明）：

| 层 | 口径 | 为什么 |
|---|---|---|
| L1 | 无库 + `CHECK_WITH_DB_FILTER=<受影响模块>` | push 前要快。改了 `business::erp` 就只跑那一段的真库用例（`CHECK_WITH_DB=1 CHECK_WITH_DB_FILTER=business::erp`），其余留 L2 |
| L2-B | 全量、单线程、`--nocapture`、**三个 URL 都给** | ① `--test-threads=1`：库里有改全局配置的用例（流程定义 / 状态机边 / 期末结账），并行互踩；② `--nocapture`：不加就永远 grep 不到 SKIP；③ 缺一个 URL 就红：只给 `ERP_DB_URL` 会让 `flow::model` / `task_bridge` / `notify_node` 静默 SKIP |

**判红的三条硬判据**（照抄，含「假绿检测」）：

1. 任何一条用例失败 → 红；
2. 出现「库没配 / 连不上」类 SKIP（`未设置 XXX_DB_URL` / `XXX_DB_URL 连接失败` / `set_db 失败`）→ **红**（libtest 把它记 passed，就是假绿）；
3. 三个库 URL 缺一个 → 红。

> 「数据前提缺失」类 SKIP（「本库的 5602 仍是末级科目」）**不判红**，只打 ℹ —— 那是如实报告环境差异。这条已经在 `check.sh` 里分好类了，本设计沿用，不引入第二套判据。

### 4.4 L2 抽样：13 域怎么抽、抽哪些

#### 4.4.1 先说清一个不好听的事实（实测）

484 个可执行脚本里，绝大多数**绑死在 145 的 `cloudpivot_dev`**：

| 特征 | 脚本数 | 含义 |
|---|---|---|
| 引用 `/etc/cloudpivot/*.env`（绝对路径） | **113** | 换台机器直接 `No such file` |
| 写死 `cloudpivot-dev.env` | **82** | 同上 |
| 写死 `:8033`（145 测试端口） | **47** | 换端口即失败 |
| 写死 `172.18.10.145` | 3 | 更难搬 |
| **接受 `BASE_URL` 环境变量** | **35** | **真正可移植的只有这 7.2%** |

还有一层：`e2e01_purchase_chain.sh` 这类脚本用的是**硬编码 UUID 夹具**（`MAT1=${MAT1:-01a1b5d2-5061-75c1-9845-fd0a8a41512b}  # DEMO-P101`）。这些 UUID 只存在于 145 的 `cloudpivot_dev` 里，干净库上一条都对不上。

**所以「把 458 个脚本挂上 cron 就完成回归」是不成立的**。真实路径是三档分层：

#### 4.4.2 A / B / C 三档

| 档 | 定义 | 数量（实盘） | 跑在哪 |
|---|---|---|---|
| **A 档 · 已可移植** | 不依赖 145 dev 夹具/绝对路径；可指定 `BASE_URL`+`DB_*`；自己建自己清 | **21 项门禁** + `menu_invariants` + `icon_invariant_teeth` + `verify.py smoke`(39) + `verify.py all`(163/8/2) ≈ **25 个执行体** | **L2 全跑** |
| **B 档 · 参数化后可移植** | 逻辑是自建夹具的（`step0` 建前置、末尾 `cleanup.sh`），只差把环境变量提到外层 | 82 个写死 `dev.env` 中的一大部分；**目标是每域至少 1 个主脚本**（13 个） | **B 批迁移完成后进 L2** |
| **C 档 · 强绑 145 夹具** | 直接读 145 dev 里既有单据/UUID（如 `e2e01_purchase_chain.sh` 的 `MAT1/MAT2`）；或在本机结构性不可跑（`delegate_task` 4 条） | 其余；manifest 标 `layer: [L3] manual` | **L3 人工触发**（在 145 上、对着 `cloudpivot_dev` 跑，与今天人工演练同口径） |

**每域主脚本候选**（B 档第一批，13 个，实测存在）：

| 域 | 候选主脚本 | 现状 |
|---|---|---|
| E2E-01 采购 | `e2e01_purchase_chain.sh` | C（硬编码 `MAT1/MAT2`）→ 需先做夹具自建 |
| E2E-02 销售 | `e2e02_sales_a.sh` + `e2e02_sales_b.sh` | 需参数化 |
| E2E-03 库存 | `e2e03_inventory_a/b/c.sh` | 需参数化（有 `e2e03_reconcile.sql` 可复用） |
| E2E-04 财务 | `e2e04_finance_a/b/c.sh` | 需参数化 |
| E2E-05 资产 | `e2e05_asset_a/b/c.sh` | 需参数化 |
| E2E-06 预算报销 | `e2e06_budget_a/b.sh` | 需参数化 |
| E2E-07 合同 | `e2e07_contract_a/b.sh` | 需参数化 |
| E2E-08 主数据 | `e2e08_*`（12 个） | 需参数化 |
| E2E-09 系统管理 | `e2e09_run_all.sh`（已有 run_all 入口） | **最接近 A 档**（只需 `BASE`/`PSQLQ` 参数化） |
| E2E-10 流程引擎 | `e2e10_a/b.py` | 需参数化 |
| E2E-11 任务协作 | `e2e11_a/b.py` | 需参数化 |
| E2E-12 附件+PDF | `e2e12_a/b.py` | 需参数化（依赖 chromium-pdf，145 上有该 service） |
| E2E-13 平台功能 | `e2e13_a/b.py` | 需参数化 |

#### 4.4.3 manifest（唯一真相，S6 的自守门禁）

新增 `scripts/regression/manifest.yml`（**唯一声明处**，调度器只读它）：

```yaml
version: 1
# 约定：scripts/verify/ 下每个 .sh/.py 必须在 items 里出现一行，否则 run_layer.sh 直接红（S6）
items:
  - key: gate/fmt
    domain: gate
    layer: [L0, L1, L2]
    run: cargo fmt -- --check
    cwd: backend
    timeout_s: 60
    required: true
  - key: gate/erp_invariants
    domain: gate
    layer: [L2]
    run: python3 scripts/verify/erp_invariants.py
    needs: [db]
    timeout_s: 300
    required: true
    parse: invariants          # 解析器名（collect.py 里注册，取「BLOCKER N」与末行）
  - key: verify/smoke
    domain: platform
    layer: [L2]
    run: python3 scripts/verify/verify.py smoke
    needs: [instance]          # 需要活实例（BASE_URL）
    timeout_s: 120
    required: true
  - key: e2e09/run_all
    domain: sysadmin
    layer: [L3]                # B 档迁移完成前只进 L3
    run: bash scripts/verify/e2e09_run_all.sh
    needs: [instance, db, fixtures:self]
    timeout_s: 900
    required: true
```

字段语义（**实现时按这张表写，不许自创**）：

| 字段 | 取值 | 含义 |
|---|---|---|
| `key` | `<类>/<名>` | 稳定标识，跨运行比对用；进 `sys_ci_item.item_key` |
| `domain` | `gate` / `procurement` / `sales` / `inventory` / `finance` / `asset` / `budget` / `contract` / `masterdata` / `sysadmin` / `flow` / `task` / `attach` / `platform` | 13 域 + gate，用于覆盖率统计 |
| `layer` | `[L0,L1,L2,L3]` 的子集 | 哪几层跑 |
| `needs` | `db` / `instance` / `fixtures:self` / `fixtures:145dev` | 缺任何一项而环境不具备 → `SKIP`（并计入 S2 的「该跑没跑」） |
| `required` | `true/false` | `false` = 可选抽样项（红了记录不阻断） |
| `timeout_s` | 整数 | 超时即 `ERROR`（不是 FAIL），并保留日志前 200 行 |
| `parse` | 解析器名 | `collect.py` 用它把 stdout 归一成 `(status, failures, first_line)`；没写就用退出码 |

#### 4.4.4 抽样算法（确定性、可复现）

```
必跑集合 = manifest 里 layer ∋ L2 且 required = true
抽样集合 = layer ∋ L2 且 required = false 的项里，
           取 sha256(f"{北京日期 YYYY-MM-DD}:{key}") 的前 8 位转整数 % 100 < P 的项
P = 20（约 1/5）
```

- **确定性**：同一天重跑抽到同一批（便于「昨天红今天还红」的对比），不同天自动换批。
- **覆盖率**：任一可选脚本 30 天内至少被抽到一次的概率 = `1 - 0.8^30 ≈ 99.88%`。
- **上限**：单次抽样总超时 15 min（超出即 `ABORTED` 并告警），防止某个卡死脚本吃满窗口。
- **不许随机**：用 `random.randint` 会导致「红项无法复现」，违背 S2（红项要能追）。这里的伪随机以**日期**为种子，人可复算。

### 4.5 起步三件事（不做完，L2 第一天就长红）

今天干净库上已经有 3 项红（§2.2）。这三项必须先有结论，否则「每日回归」上线即失信：

| # | 红项 | 实测细节 | 处置（定稿） | 归谁 |
|---|---|---|---|---|
| 1 | `biz_type_domain` 闸④ | `biz_flow_def.biz_type` 有 3 个域外值：`PR_APPROVE`（V0331）/`PRT_APPROVE`（V0332）/`SO_APPROVE`（V0281） | **补词表**（`business/shared/doc_kind.rs` 的 `COLUMN_DOMAINS` 加上这 3 个值）。**绝不改迁移** —— 已应用迁移指纹不可变（AGENTS.md 约定 6），改迁移会让所有存量库校验失败 | backend 一张卡（≤1 h） |
| 2 | `orphan_button` 闸⑦ | `SALES_CLERK` 持有 `statement:view` / `statement:export`，父页 `/business/erp/receivable` 的码 `business:erp:receivable:list` 没授给它 | **按 V0259 先例改挂父页**（零授权变化）或补授父页码（新迁移，按码判重 + down）。二选一，倾向改挂父页 | backend 一张卡（≤1 h） |
| 3 | `scope_gate` 闸② | 5 个文件查询未接 `scope::` 且无 `SCOPE-EXEMPT`（`erp/payable_payment/tests/*`、`flow/subprocess.rs`、`monitor/model.rs`、`shared/user_name.rs`、`system/setup/model.rs`）+ **2 个测试面文件未登记 `SKIP_MANIFEST`**（`shared/test_helpers.rs`、`business/task/task_attachment_snapshot_tests.rs`） | **逐个裁定**：真生产代码 → 接 `scope::` 或写 `// SCOPE-EXEMPT: <理由>`；测试面 → 补 `SKIP_MANIFEST` 两行（这是门禁的正常维护，不是放宽） | backend 2 张卡（≤3 h） |

**先落台账再修**（顺序很重要）：批 1 上线当天，`sys_ci_baseline` 插 3 行（`expires_at = now() + 30 days`、`owner=backend-developer`、`reason` 写清上表内容）→ L2 能立刻变绿（`baselined` 计数），同时这 3 项每天都会出现在「台账到期倒计时」里。**修完就 `revoked_at` 清账**，30 天没修则自动重新变红。

### 4.6 失败处置（谁能绕过、绕过怎么留痕）

| 层 | 失败时 | 谁能绕过 | 怎么绕 | 留痕（三处） |
|---|---|---|---|---|
| **L0** | 拒绝提交 | 任何人 | `git commit --no-verify` | ① `post-commit` hook（现状已有）记一行 `bypass` ② L1 会再抓一次 fmt/clippy ③ 文件 `~/.cloudpivot/bypass.jsonl` |
| **L1** | 拒绝 push | 任何人 | `SKIP_PREPUSH=1 git push`（现状：只 echo 一行） | ① `prepush-skips.jsonl`（时间/用户/HEAD/改动文件数/`SKIP_PREPUSH` 值）② 飞书一条 ③ **自动建卡**（`--idempotency-key ci-skip:<head>`，assignee=backend-developer） |
| **L2** | 不阻断（主干已合），建卡 + 飞书 | 不用绕 | — | `sys_ci_run.note` + 卡 comment |
| **L3** | **阻断发版** | 只有总管（manong） | `deploy.sh --force --force-reason "<说明>"` | ① `sys_ci_run.note` 写 force 原因 ② `deploy.sh` 日志 ③ 飞书「发版带 force」红卡片 |

**三条元规则**：

1. **绕过必须比绕过之后修更容易被看见**：所有绕过都进 `sys_ci_run` / jsonl / 卡 三处，任意一处都能回答「谁在什么时候绕过了什么」。
2. **不许「改门禁来变绿」**：改 `check.sh` 判据、改 `SCOPE-EXEMPT` 滥用、改 `MENU_TOTAL_MAX` 这类动作，必须走独立的卡 + 在卡的正文里写明「这是放宽不是修」，且 `sys_ci_item.first_line` 保留原报错 —— 与 `scripts/verify/role_code_gate.py` 的「裸白名单 = 关闸，直接判红」同一条思路。
3. **ERROR ≠ FAIL**：连续 3 次 ERROR 视为「回归停了」（比红更严重，因为连红都看不见），直接升级为飞书 + 卡 + 通报。

### 4.7 告警通道（决策 + 为什么）

| 通道 | 用途 | 现状 | 决策 |
|---|---|---|---|
| **飞书卡片** | 一级告警：L2 失败汇总 / L3 阻断 / 绕过 / 台账到期 | `scripts/cron/feishu_alert_send.py` 已可用（v1 卡片，发完 GET 回读校验正文）；凭据在 99 的 `laoliu/.env`；144 通过 ssh 到 99 调用（凭证不进生产机） | **采用**。145 也走「ssh 99 → 发」这条链路，不在 145 另存 bot secret |
| **kanban 卡** | 二级：需要人决策/动手的事 | `hermes kanban create --idempotency-key` 可用（实测 `--help` 有该参数） | **采用**（规则见 §4.8） |
| **文件日志** | 三级：现场 | `sys_ci_run.log_dir` + `/var/log/cloudpivot/ci/` | 强制：每个 item 都留日志 + `sha256` |
| **只读看板** | 人看趋势 | `views/support/invariant` 可作模板 | 批 3 |
| 邮件 / 短信 / 钉钉 | — | 无 | **不做**（多一条通道 = 多一处配置漂移；飞书已覆盖） |

**哑告警看门狗（这是本设计里最便宜、最值钱的一条）**

实测依据：144 的 crontab 里 `0 2 * * * /opt/cloudpivot/scripts/erp-health-daily.sh` —— **这个脚本在仓库里已经不存在**（`ls` 确认），`/var/log/cloudpivot/erp-health-202611.log` 最后一行是 `[2026-11-05 02:00:01] ✓ 全部不变量成立`，**静默 5 天**，没有任何告警。同一天 03:30 的备份 cron 与 02:00 的 `snapshot_audit` 都还正常 —— 说明「巡检在跑」这件事本身没有任何人/任何机制在验证。

看门狗每天 09:00 做 4 项断言，任一项不过就飞书 + 建卡：

| # | 断言 | 判定 | 覆盖的故障 |
|---|---|---|---|
| W1 | `sys_ci_run` 里 `layer='L2' AND trigger_type='SCHEDULED'` 最近一行 `run_at` 距 now ≤ 26 h | 否则 | 定时任务没跑起来 / 被删 / 145 挂了 |
| W2 | 该行 `status ∈ (OK, FAIL)`（**不是 ERROR/ABORTED**） | 否则 | 跑了但没跑成（脚本路径、权限、库连不上） |
| W3 | 144/145/99 的 `crontab -l` 里每一条脚本路径**都存在且可执行** | 否则 | **本次实测的 erp-health-daily 就是这个** |
| W4 | 每个「日更」类 cron 对应的日志文件 `mtime` ≤ 26 h（配置见 manifest `watch:` 段） | 否则 | 脚本在但静默失败 |

> W3/W4 建议做成 `scripts/regression/watchdog.py`，它自己也被 W1 反过来监控（看门狗挂在 145 的 cron 上，失败落文件，再由飞书发送器的「发送成功回读」兜底）。

### 4.8 与 kanban 的关系

现状：13 域演练 = **13 张卡**（人工派、人工看结果），每天要人盯。目标：**演练变回归，卡只在需要人做决定时出现**。

| 场景 | 建卡？ | 卡的样子 |
|---|---|---|
| 日常 L2 有红（**台账未覆盖**） | **建 1 张/项** | 标题「CI 红：`<item_key>`（`<host>`）」；assignee=backend-developer；`--idempotency-key ci:<item_key>:<北京日期>`（同一天同一项只建一次）；body 带 `first_line` + `log_path` + 最近 3 次状态 |
| 同一项**连续 3 次**红（跨天） | **再建 1 张升级卡** | 标题「CI 连续红 3 次：`<item_key>`」；`--idempotency-key ci-esc:<item_key>:<ISO 周>`；assignee=backend-developer，priority 高 |
| 红项恢复 | **不建卡，只 comment** | 在最近一张卡上 `kanban_comment`「已恢复（run#123，head=…）」；**不自动 complete**（谁认领谁关，避免「自动关了但根因没修」） |
| 台账（`sys_ci_baseline`）到期 | **建 1 张** | 「台账到期：`<item_key>` 20 天后自动变红，请决定修/续/删」；`--idempotency-key ci-base:<item_key>:<expires>` |
| L3 发版前失败 | **不建卡**（发版本来就有人在场） | 只在 `deploy.sh` 输出 + 飞书；由发版负责人现场决定 |
| 13 域人工演练（现有 13 卡） | **保留** | 但卡上「复跑命令」改为 `run_layer.sh L3 --only <domain>`，结果自动 `kanban_comment` 到卡上 |
| 绕过（`SKIP_PREPUSH`） | **建 1 张** | 见 §4.6 |

**结果写哪（三处，各有各的读者）**：

| 落点 | 读者 | 内容 |
|---|---|---|
| `sys_ci_run` / `sys_ci_item` | 机器（看板、看门狗、S 指标） | 结构化：状态、耗时、rc、`first_line`、日志路径与 sha |
| kanban 卡 comment | 人（决策） | 摘要 3 行 + 日志尾部 20 行 + 修法建议 |
| `/var/log/cloudpivot/ci/<run_id>/` | 工程师（排障） | 每项的**完整** stdout/stderr（保留 30 天） |

**卡爆炸的定量判据**：日常 L2 一次约 130 项。若「有红就建卡」，最坏一天 130 张。加了台账 + 「同项同天只一张」+ 「连续 3 次才升级」之后，稳态是 **0–2 张/天**（实测今天的 3 项红都会先进台账，一张卡都不建）。

### 4.9 特殊场景

| 场景 | 处理 |
|---|---|
| **工作区脏**（别人未提交的改动） | `sys_ci_run.worktree_dirty=true`；L2 允许（主干上跑的是 HEAD，脏文件只影响 cargo 编译结果 → 因此 L2 必须在**干净检出**里跑：`git archive HEAD \| tar -x -C /tmp/ci-head`，与评审 §5.7.1 的做法一致）；**L3 脏树直接拒跑**（结论不可用于发版） |
| **无新提交** | L2 照跑（防「代码没变但环境漂了」：新迁移、库权限、cron 被删）；`note` 里写 `same_head_as=<上次 run>` |
| **145 掉线/重启** | 该次 `ERROR`；连续 3 次 `ERROR` → 升级告警（§4.6 元规则 3） |
| **与 145 的 30 分钟部署循环撞车** | L2-A 抢 `test_deploy.sh` 的同一把锁 `/tmp/test-deploy.lock`（`flock`），拿不到就等 60 s 重试最多 5 次；仍拿不到 → `ERROR` + 告警（别硬跑，会把 145 的 target 目录踩坏） |
| **迁移新增但库没跑** | 干净库每次重建（6.43 s）自动覆盖；顺带验证「全新库能否一次迁到最新」——这是最容易被绕过、后果最严重的场景 |
| **99 磁盘满**（实测 97% used / 8.1 G 可用 / `target` 109 G） | L2-B 前后做磁盘检查（`>90%` 就在飞书里挂黄条）；**不做 `cargo clean`**（重建一次要 10+ 分钟，代价远大于收益）；由现有 `disk_guard.sh` 负责清理 |
| **同一 item 一天内重复失败** | 飞书一天一条（去重键 `item_key + 北京日期`），不逐次刷屏 |
| **数据库被别的 worker 占用** | L2 用**独立库名**（`cloudpivot_ci`），每次 drop/create；不与任何演练库/开发库复用 |
| **采样脚本卡死** | 单项 `timeout_s` + 全局 15 min 上限；超时记 `ERROR` 并保留日志前 200 行 |

---

## 5. 接口设计

### 5.1 API 清单

> **出参口径**：卡面模板写的是 `header / items / meta`，但本仓库的实际口径是 `Res<T>` 的 `data = PageData<T> = {list, total, page, page_size}`（`frontend/src/api/request.ts:15-24`，后端同一口径的先例见 `backend/src/business/erp/stockout_dto.rs:482-492`）。**本设计服从仓库现状**，与 ARCH9-3 在 §5.1 的处理一致 —— 不为了一份卡面模板造第二套分页形状。

| # | Method + Path | 入参 | 出参关键字段 | 权限码 |
|---|---|---|---|---|
| 1 | `GET /api/support/ci/overview?days=30` | `days` 1–90 | `daily[]`（`date`, `status`, `passed`, `failed`, `duration_ms`）、`consecutive_red[]`（`item_key`, `times`, `since`）、`baseline_expiring[]`（`item_key`, `owner`, `expires_in_days`）、`last_run`（`run_at`, `layer`, `host`, `git_head`, `status`） | `support:ci:list` |
| 2 | `GET /api/support/ci/page` | `layer` `status` `host` `trigger_type` `git_head` `from` `to` `page` `page_size` | `list[]`: `id` `run_at` `layer` `host` `git_head`(short) `status` `total` `passed` `failed` `skipped` `baselined` `duration_ms` `worktree_dirty` `note`；`total` `page` `page_size` | `support:ci:list` |
| 3 | `GET /api/support/ci/{id}` | — | `run`（同 #2 单行）+ `items[]`: `item_key` `domain` `kind` `required` `status` `baseline` `rc` `duration_ms` `failures` `first_line` `log_path` | `support:ci:list` |
| 4 | `GET /api/support/ci/items/{item_key}/history?limit=30` | `limit` 1–200 | `list[]`: `created_at` `status` `duration_ms` `failures` `first_line` `run_id` | `support:ci:list` |
| 5 | `GET /api/support/ci/items/{item_key}/log?tail=200` | `tail` 1–2000 | `text`（正文，按行裁剪）、`truncated`、`log_path` | `support:ci:list` |
| 6 | `POST /api/support/ci/run` | `{ "layer": "L3", "only": ["e2e09/run_all"], "force": false }` | `{ "run_id": "...", "accepted": true, "reason": "" }` | `support:ci:run` |
| 7 | `GET /api/support/ci/baseline` | `active_only=true` | `list[]`: `id` `item_key` `reason` `owner` `created_at` `expires_at` `expires_in_days` | `support:ci:list` |
| 8 | `POST /api/support/ci/baseline` | `{ item_key, reason, owner, days }` | `{ id }` | `support:ci:baseline` |
| 9 | `PUT /api/support/ci/baseline/{id}/revoke` | — | `{ revoked_at }` | `support:ci:baseline` |
| 10 | `DELETE /api/support/ci/{id}` | — | `{ deleted }`（只允许删 `status='ABORTED'` 的行；明细级联） | `support:ci:delete` |

**口径细则（实现时必须照做）**：

1. **`*_id` 必须带 `*_name`**（AGENTS.md 约定 13）：本设计里唯一的 id 引用是 `run_id`（父子，不是人/主数据），所以天然不触发。若后续加 `trigger_user_id`，**同一出参里必须给 `trigger_user_name`**，且批量 join（一次查完，不许 N+1）。`owner` 存的是文本快照（审计语义），要显示「现名」时前端按 `owner` 反查用户列表 —— 这是可选项，不阻塞。
2. **日志预览必须限目录**：`log_path` 只能落在 `/var/log/cloudpivot/ci/` 与 `sys_ci_run.log_dir` 之下；后端 `canonicalize` 后校验前缀，越界直接 403 `61020 CI_LOG_PATH_FORBIDDEN`（新增错误码段，见 §5.2）。**不许**直接 `fs::read` 客户端传来的路径。
3. **`log_sha256` 用来回答「两次红是不是同一现场」**：`history` 出参可选带 `log_sha256`，前端做「与上次相同」标记。
4. **触发接口不直接执行**：`POST /run` 只落一行 `sys_ci_run(status='RUNNING', trigger_type='MANUAL')` 并通过 `sys_job`/`tokio::spawn` 交给机器上的执行器（145 上由 cron 的同一把锁串行化），**接口本身永不阻塞**。理由：L3 要跑 40–90 min，HTTP 请求不可能挂着。
5. **分页上限** `page_size ≤ 200`（与 `PageData` 其他页面一致），默认 20。
6. **导出**：本期不做 Excel 导出（数据量小、看板够用）；若要做，走现有 `biz_type` 词表加 `CI_RUN`（**不许临时拼 SQL**）。

### 5.2 权限码设计

| 权限码 | 动作 | 对应菜单行 |
|---|---|---|
| `support:ci:list` | 看运行记录/详情/历史/日志（只读） | 页面 `/support/ci`（`menu_type=2`） |
| `support:ci:run` | 手动触发一次（L2/L3） | 按钮「立即跑一次」（`menu_type=3`） |
| `support:ci:baseline` | 台账新增/撤销 | 按钮「登记已知红」 |
| `support:ci:delete` | 删 `ABORTED` 运行 | 按钮（默认**不授给任何业务角色**，只超管） |

- 命名遵循 `{模块}:{资源}:{动作}`；与 `support:invariant:list` / `:run` 完全同构（`backend/src/support/invariant/handler.rs:7-8` 明确写了「两个都是功能权限（不看行）」）。
- **不登记数据范围**（`SCOPED_TABLES`）：CI 结果是平台级事实，不是谁的业务数据 —— 与 `biz_flow_def`（流程定义）同一判断，闸② 应写 `// SCOPE-EXEMPT: CI 结果是平台级事实，无按部门裁剪语义`。
- **授权矩阵**：`support:ci:list` 授给与 `support:invariant:list` **完全相同**的角色集合（实测该菜单已授 11 个业务角色 + 超管），不另造一套；`support:ci:run` 只授 `SUPER_ADMIN` + `FINANCE_DIRECTOR`（发版/回归责任人）。

### 5.3 分页 / 筛选 / 导出

- **筛选**（`/page`）：`layer`（L0–L3）、`status`（OK/FAIL/ERROR/RUNNING/ABORTED）、`host`、`trigger_type`、`git_head`（前缀匹配）、`from`/`to`（默认最近 30 天）。
- **排序**：`run_at DESC` 固定（不开放排序参数，避免全表排序）。
- **索引支撑**：`idx_ci_run_at (run_at DESC)`、`idx_ci_run_not_ok (run_at DESC) WHERE status<>'OK'`（看板「只看红」直出）。
- **item 级筛选**：`GET /page?item_key=<key>&status=FAIL` 走 `idx_ci_item_key_time`（真库 `EXPLAIN` 实测 `Index Scan using idx_ci_item_key_time`，见 `01_ci_ddl_check.out` 断言 J）。

---

## 6. 前端设计

### 6.1 页面清单（菜单挂哪、路由 path）

| 项 | 值 | 依据 |
|---|---|---|
| 菜单 `name` | `support:ci` | 与 `support:invariant` 同构（实测该行 `name='support:invariant'`） |
| 菜单 `title` | **质量门禁**（4 字，≤12 硬顶） | `menu_invariants.sh` 断言⑥；`MENU_TITLE_MAX=12` |
| `path` / 路由 | `/support/ci` → `views/support/ci/index.vue`（router 子项写 `support/ci`，无前导 `/`，与 `router/index.ts:106` 的 `support/invariant` 同写法） | 实测 router 现写法 |
| 父节点 | `10000000-0000-0000-0000-000000000022`（支撑功能） | 实测该父下 sort 10–80 已占，新行 `sort_order=90` |
| 按钮（`menu_type=3`） | 「立即跑一次」`support:ci:run`、「登记已知红」`support:ci:baseline` | 权限码与 §5.2 一一对应 |
| 菜单总数影响 | 433 → **435**（+1 页面 +2 按钮 = 438 行；`menu_invariants` 断言⑤上限 460） | 实测 433（干净库/145 dev 一致） |

### 6.2 关键交互

一页三 Tab（不做三个页面，避免菜单膨胀）：

| Tab | 内容 | 关键交互 |
|---|---|---|
| **运行记录** | `t-table`：时间 / 层 / 主机 / HEAD(7 位) / 状态 `t-tag` / 通过-失败 / 耗时 | 点行 → 右侧 `t-drawer` 展示 `items`；`items` 里 `status=FAIL` 行展开显示 `first_line` + 「看日志」按钮（拉 `/log?tail=200`，只读展示，禁下载） |
| **红项看板** | 按 `item_key` 聚合：最近状态 / 连续红次数 / 首次红时间 / 是否有台账 | 「登记已知红」按钮 → 弹窗（`item_key` 只读、`owner` 默认当前用户、`reason` **必填 ≥8 字**、`days` ≤90）；已存在活跃台账时 409 → 提示「先撤销旧台账」 |
| **台账** | 活跃 + 已撤销两条列表，到期倒计时 `t-tag`（<7 天变黄） | 「撤销」按钮，二次确认（文案写清「撤了会立刻重新变红」） |

**按钮态**：

- 「立即跑一次」在 `layer=L3` 且 `worktree_dirty` 时**禁用**并提示「工作区有未提交改动，L3 结论不可用于发版」；
- 同一层已有 `RUNNING` 行时，按钮灰 + 文案「已有运行中（run#xxx）」；
- 无权限（`support:ci:run` 未授）时按钮不渲染（与现有页面按钮同一口径，走 `v-perm`）。

**错误提示**（逐条对应后端码，前端只负责展示 `msg`，不自己编文案）：

| HTTP | code | 场景 | 提示 |
|---|---|---|---|
| 409 | `61011` | 已有活跃台账 | 「该项已有认领（owner=…，到期 …）；要重开请先撤销」 |
| 422 | `61012` | 理由 <8 字 / 窗口 >90 天 | 「理由至少 8 个字，认领期最长 90 天」 |
| 403 | `61020` | 日志路径越界 | 「日志不在允许目录内」 |
| 409 | `61013` | 同层同主机当天已有定时运行 | 「今日已有定时运行，重跑请用手动触发」 |

### 6.3 复用哪些现有组件

| 需求 | 复用 | 说明 |
|---|---|---|
| 列表 / 分页 / 空态 | `t-table` + 现有 `PageData` 分页组件（抄 `views/support/invariant/index.vue` 的用法） | **整页照抄它**：它是「列表 + 详情 + 趋势 + 手动跑一次」的现成同构页 |
| API 层 | 新 `frontend/src/api/support/ci-api.ts`，结构抄 `api/support/invariant-api.ts`（`get`/`post` 包装 + 类型导出） | 路径**必须写全 `/api/...`**（AGENTS.md 约定 9） |
| 状态标签 | `t-tag` 主题色映射：OK=success / FAIL=danger / ERROR=warning / RUNNING=primary / SKIP=default | — |
| 趋势图 | `echarts`（项目已依赖）折线：近 30 天通过率 | 与工作台指标卡同口径 |
| 日志只读展示 | `t-dialog` + `<pre>`（**不引 Monaco**，体积不值） | — |
| 表单 | `t-form` + `t-input(type=textarea)` + `t-input-number` | — |
| 附件面板 / 流程进度 | **不适用**（本模块无附件、无审批） | 明确写出，避免实现者去找 `DocAttachmentPanel` |

---

## 7. 迁移与上线

### 7.1 迁移文件清单

| 文件 | 内容 | 幂等策略 |
|---|---|---|
| `V0368__create_ci_tables.up.sql` | §3.1 三张表 + 4 个索引 + 注释（**原样拷贝，DDL 已真库验证**） | 迁移引擎只跑一次；文件内**不写** `IF NOT EXISTS`（本项目约定：新表用裸 `CREATE TABLE`，漏建立刻报错而不是静默跳过） |
| `V0368__create_ci_tables.down.sql` | `DROP TABLE IF EXISTS sys_ci_item, sys_ci_run, sys_ci_baseline;`（**先子后父**） | `IF EXISTS` |
| `V0369__seed_ci_menu.up.sql` | 1 页面 + 2 按钮 + 3 条权限码文案 + 11 个角色的 `sys_role_menu`（**与 `support:invariant` 同集合**） | 固定 UUID + `WHERE NOT EXISTS` / `ON CONFLICT (id) DO NOTHING`；授权段按**码**判重（V0258 矩阵口径） |
| `V0369__seed_ci_menu.down.sql` | 先删 `sys_role_menu`（FK），再删菜单行 | 按固定 UUID 删 |

> 号段核对：实测当代最大 `V0367__flow_all_role_approval_to_any_multiversion.up.sql`（`find backend/migrations -name '*.up.sql' \| sort \| tail`），且 `.down.sql` 必须与 `.up.sql` 成对。**动手前再 `ls` 一次**（ARCH-9 的分支还有别的卡在加迁移，号段会撞）。

### 7.2 存量数据处理

- **无 backfill**：三张新表从空开始；第一次 L2 运行即有数据。
- **`sys_invariant_result` 保持不动**：历史巡检记录不迁移、不改结构（只读兼容）。
- **不需要 downtime**：纯加表 + 加菜单，`deploy.sh` 的迁移段逐条执行（实测干净库 298 条 6.43 s，单条毫秒级）。
- **菜单增量对 `menu_invariants` 的影响**：433 → 438 行，断言⑤上限 460 仍有 22 行余量；断言⑥新 title「质量门禁」4 字；断言⑦新按钮 `sort_order` 用 10/20（同父唯一）。

### 7.3 回滚方案

| 层 | 回滚动作 | 风险 |
|---|---|---|
| 迁移 | `V0369.down`（删菜单与授权）→ `V0368.down`（删表） | 低：无外部引用；但**回滚会丢运行历史**（可先 `pg_dump` 三张表） |
| 代码 | 后端删模块 / 前端删页面 | 低：独立模块，无别处依赖 |
| cron | 145/99 `crontab` 注释掉 03:10/03:40 两行 | 零风险 |
| hook | `git config core.hooksPath ''` 或临时 `SKIP_PREPUSH=1` | 会退化成今天的状态（靠人跑） |
| **发版闸** | `deploy.sh --force --force-reason` | 已留痕（§4.6） |

### 7.4 分几批上线（能否灰度）

| 批 | 内容 | 能灰度吗 | 验收 |
|---|---|---|---|
| **批 1（0.5 天）** | 三张表（V0368）+ API + 页面（V0369）+ 起步台账 3 行 | 能（菜单只授 SUPER_ADMIN 先验） | 页面能看空列表；手工插一行 run 能看详情 |
| **批 2（0.5 天）** | `run_layer.sh` + `collect.py` + `manifest.yml`（只含 A 档 25 项）+ L0/L1 接进 hook + `SKIP_PREPUSH` 留痕 | 能（先只跑 L0/L1） | **一次真实 push 走完 L1 并落两行 run**；`SKIP_PREPUSH=1` 产生 jsonl + 卡 |
| **批 3（1 天）** | 145 cron 03:10（L2-A）+ 99 cron 03:40（L2-B）+ 飞书 + 看门狗 | 能（先 `--dry-run` 一周只落库不告警） | **连续 7 天 L2 有 OK 行**；看门狗能抓出「人为注释掉 cron」 |
| **批 4（1–2 天）** | 13 域 B 档参数化（每域 1 主脚本）+ L3 + 远端 `pre-receive` + 发版闸接 `deploy.sh` | 能（B 档逐个域并入，manifest 逐行放开） | **一次真发版带 `gate=L3 result=OK`**；至少 5 个域进 L2 抽样 |

---

## 8. 工作量估算与风险

### 8.1 拆成实现卡的清单（每张 ≤1.5 h，标注依赖）

**批 1 · 数据与只读面（6 张，≈7 h）**

| # | 卡 | 依赖 | 估时 |
|---|---|---|---|
| B1-1 | 迁移 `V0368` 三张表（DDL 从本文 §3.1 原样拷，先 `ls` 号段） | — | 1.0 h |
| B1-2 | 迁移 `V0369` 菜单 + 权限码 + 角色授权（固定 UUID、按码判重、down 成对） | B1-1 | 1.5 h |
| B1-3 | 后端 `support/ci` 四件套（mod/handler/dto/model）：overview + page + detail | B1-1 | 1.5 h |
| B1-4 | 后端 history + log（含 `canonicalize` 目录白名单 + `61020`） | B1-3 | 1.0 h |
| B1-5 | 后端 baseline 增/撤（`61011`/`61012` 冲突与校验） | B1-3 | 1.0 h |
| B1-6 | 前端 `ci-api.ts` + `views/support/ci/index.vue`（三 Tab，抄 invariant 页） | B1-3 | 1.5 h |

**批 2 · 执行器与 hook（6 张，≈8 h）**

| # | 卡 | 依赖 | 估时 |
|---|---|---|---|
| B2-1 | `scripts/regression/manifest.yml` 的 A 档骨架（25 项，逐条写 `run`/`needs`/`timeout_s`） | — | 1.5 h |
| B2-2 | `run_layer.sh`：读 manifest → 按层筛 → 串行/并行 → 落 `sys_ci_run`/`sys_ci_item`（psql 写库） | B2-1, B1-1 | 1.5 h |
| B2-3 | `collect.py`：解析器注册表（`invariants`/`cargo`/`python`/`exit_code` 四种），归一 `(status, failures, first_line)` | B2-2 | 1.0 h |
| B2-4 | manifest 覆盖自检（S6）：`scripts/verify/` 每个 `.sh/.py` 必须在 manifest 出现或标 `manual` | B2-1 | 1.0 h |
| B2-5 | L0/L1 接 hook：`pre-commit` 调 `run_layer.sh L0`、`pre-push` 调 L1（**保留现有 fmt/clippy 逻辑，只加落库**） | B2-2 | 1.5 h |
| B2-6 | `SKIP_PREPUSH` 留痕：jsonl + 飞书 + 建卡（`--idempotency-key ci-skip:<head>`） | B2-5 | 1.5 h |

**批 3 · 定时与告警（5 张，≈7 h）**

| # | 卡 | 依赖 | 估时 |
|---|---|---|---|
| B3-1 | 145 cron 03:10 L2-A（抢 `/tmp/test-deploy.lock`，干净检出 `git archive HEAD`） | B2-2 | 1.5 h |
| B3-2 | 99 cron 03:40 L2-B（重建 `cloudpivot_ci` + 三 URL 真库全量单测 + 假绿检测三判据） | B2-2 | 1.5 h |
| B3-3 | 飞书发送器接 CI 事件（L2 汇总 / L3 阻断 / 绕过 / 台账到期），一天一条去重 | B2-2 | 1.5 h |
| B3-4 | `watchdog.py` W1–W4（哑告警看门狗）+ 145 cron 09:00 | B3-1, B3-2 | 1.5 h |
| B3-5 | 起步台账 3 行 + 3 张修红卡（§4.5 的 biz_type_domain / orphan_button / scope_gate） | B1-5 | 1.0 h（本卡只落台账；修红是 3 张独立卡） |

**批 4 · 13 域与发版闸（16 张，≈22 h）**

| # | 卡 | 依赖 | 估时 |
|---|---|---|---|
| B4-1..B4-13 | 13 域各一张：把主脚本参数化（`BASE_URL`/`DB_*`/夹具自建/自清）并放开 manifest 到 L2 | B2-2 + 逐域 | 1.5 h × 13 = 19.5 h |
| B4-14 | L3 入口 `run_layer.sh L3`（全量脚本 + e2e + 汇总） | B2-2 | 1.0 h |
| B4-15 | `deploy.sh` 接发版闸（读最近一次 L3 run，`--force` 走留痕） | B4-14 | 1.0 h |
| B4-16 | 远端 bare `pre-receive` 兜底（**先 dry-run 演练**，配错会挡住全部 push） | B2-5 | 1.5 h |

**合计 33 张卡**（批1 6 + 批2 6 + 批3 5 + 批4 16），**≈44 h ≈ 5.5 人日**（单人串行）；两人并行（一人后端/一人脚本）≈ **3 个工作日**。

> 诚实点出**一定会溢出**的 4 张：B4-1（采购链硬编码 UUID 最多，可能 3 h）、B4-8（主数据 12 个脚本）、B2-5（hook 要保证不拖慢现有 pre-push 的 2 min 05 s）、B4-16（远端 hook 风险最高）。

### 8.2 分期（按 D-1 的短/中/长期）

| 期 | 内容 | 交付判据 |
|---|---|---|
| **短期（1 周）** | 批 1 + 批 2 + 批 3 → L0/L1 强制且留痕、L2 每日两段在跑、飞书 + 看门狗可用、3 项存量红进台账 | S1 连续 7 天有 OK；S3 绕过 1:1；W3 能抓出 erp-health-daily 这类哑 cron |
| **中期（2–4 周）** | 批 4 的 13 域参数化 + L3 + 发版闸 | S4 每次发版带 `gate=L3`；13 域至少 5 个进 L2 |
| **长期（条件触发）** | 若「仓库迁到 GitHub / 要对外开源 / 需要 per-PR 门禁给外部贡献者看」→ 再上 **Gitea + Woodpecker**（复用本设计的三张表与 manifest，执行命令不变，只换调度器）；若始终内网三台机，**不引入 CI 服务**（多一个服务 = 多一处要维护的失败点） | — |

### 8.3 已知风险

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| R1 | **13 域脚本可移植率只有 7.2%**（实测 113/82/47 写死路径端口） | B 档工作量被低估，L2 长期只能覆盖 A 档 | 分 13 张卡逐域做，每域做完立刻放开 manifest（增量交付，不做大爆炸） |
| R2 | **工具链版本漂移**：99 `1.94.1` vs 145/144 `1.99.1` | 「本机绿、CI 红」或反过来 | D-4：`rust-toolchain.toml` 钉 `1.94.1`；145 装同一版本（`rustup toolchain install 1.94.1`） |
| R3 | **145 内存 7 G**，不能跑 test 链接（实测 7.7 GiB RSS） | L2 必须拆两段，架构变复杂 | 已在 §4.1 拆开；备选：给 145 加到 16 G（机器成本 vs 复杂度，见 §8.4 待确认 1） |
| R4 | **e2e 424 例串行**（`workers:1`，单例 120 s 上限） | L3 可能 40–90 min，发版窗口被拉长 | 只在 L3 + 每周一次；若 e2e 成为瓶颈，再评估「按 spec 文件分组、每组独立实例并行」 |
| R5 | **99 磁盘 97%（8.1 G 可用）**，`backend/target` 已 109 G | 磁盘满会让 L2-B 与所有 cargo 门禁一起红 | 不在 CI 里 `cargo clean`；靠 `disk_guard.sh` + 定期清 `target/debug/incremental`（现有 `clean_incremental_when_idle.sh` 已在跑） |
| R6 | **台账会被滥用成「关闸」** | 门禁形同虚设（与今天的处境一样） | 三条硬约束（理由 ≥8 字 / ≤90 天 / 同项仅一份）+ 到期卡 + 看板倒计时 |
| R7 | **in-flight 改动冲突**：本仓库多 worker 共用 `/opt/cloudpivot` 并直接往 main 提交 | L2 的 `worktree_dirty` 常态化，结论被质疑 | L2 在 `git archive HEAD` 的干净检出里跑（dirty 只影响自身 cargo 缓存，不影响判据） |
| R8 | **迁移号段撞车**（ARCH-9 分支多卡并行加迁移） | `V0368` 可能被别人占了 | 动手前 `ls`；撞了就顺延，本文引用处同步改 |
| R9 | **假绿的第二个来源**：真库用例 SKIP 被记 passed | 绿得没有意义 | 三判据里的第 2 条（「库没配类 SKIP 判红」）已在 check.sh 里写好，L2-B 必须启用 |
| R10 | **e2e 的 `E2E_ALLOW_PROD=1` 逃生口** | 有人对着生产跑 e2e 造数（2026-10-31 真实发生过） | CI 里显式 `unset E2E_ALLOW_PROD`；`target-guard.mjs` 已是硬判据，别动它 |
| R11 | **飞书发送链路依赖 ssh 99**（144/145 → 99） | 99 挂了就发不出告警 | 看门狗 W1 本身也走这条链路 → 单点。缓解：145 上留 `feishu_alert_send.py` 的副本 + 独立 bot secret（第二个单点），或接受「99 是唯一入口」并在 99 上做可用性监控 |
| R12 | **『删明细保汇总』的保留策略**会把 `log_path` 指向已删文件 | 详情页看日志 404 | `log` 接口对文件缺失返回 `410`「日志已清理（保留 30 天）」，前端给文案而不是报错 |

### 8.4 待确认（每条带默认取向 + 确认方式）

| # | 待确认 | 默认取向（不问就按这个做） | 确认方式 |
|---|---|---|---|
| 1 | 145 是否加内存到 16 G（能否合一段 H） | **不加**，维持 L2 拆两段 | 找总管拍板（成本 vs 复杂度）；拍板前按拆分实现 |
| 2 | 145 上 `cargo clippy --all-targets` 实测耗时 | 估 3–5 min（无 clippy 元数据缓存） | 在 145 执行一次并 `time`（`cd /opt/cloudpivot/backend && time cargo clippy --all-targets -- -D warnings`），结果回填 §4.1 |
| 3 | 前端 e2e 全量实测耗时 | 估 40–90 min | 145 上 `bash scripts/verify/e2e_isolated_instance.sh up` 后 `pnpm test:e2e`，取 `junit.xml` 的 `testsuite time` |
| 4 | 台账最长认领期 90 天是否够（季度结算类门禁可能更长） | **90 天**，到期重新登记（强制重新看一次） | 首次台账到期时观察是否频繁「续期」，频繁则调 180 天（要改 CHECK） |
| 5 | `pre-receive` 兜底要不要做（远端强制的代价是「所有 push 都要过 clippy」） | **做，但只校验 `fmt` + 迁移号唯一**（不做 clippy，远端没 cargo 环境） | 批 4 先在 bare 上 `dry-run` 一周（只记录不拒） |
| 6 | L2 抽样比例 P=20% 是否合适 | **P=20**（30 天覆盖 99.88%） | 上线 2 周后看 L2 平均耗时：若 >25 min 降到 10，若 <10 min 升到 30 |
| 7 | 真库全量单测**完整一轮**的精确耗时（本卡只拿到 1760/4014 例 1054 s 的实测点与 1.670 例/s 的速率） | 按 **40 min** 做容量规划 | 在一个没人抢机器的时段跑 `time cargo test --lib --features test-hooks -- --test-threads=1 --nocapture` 一轮到底，把 `duration_ms` 回填 A-15；若与 40 min 偏差 >20%，重估 §4.1 的分片方案 |

---

## 附录 A · 实测数据（本卡 2026-11-10 01:04–01:20，99 机上跑）

> 环境：99 开发机，4 核 / 15 G，PG 18.6 本地（`alun/alun`），树 HEAD `b252db2e`（工作区另有 46 个他人未提交文件）。
> 库：`cloudpivot_arch9_ci4`（由 `erp_clean_db.sh` 新建：298 条迁移、最高 V0367）。

| # | 项 | 命令（简） | rc | 耗时 | 日志 |
|---|---|---|---|---|---|
| A-1 | L0 fmt | `cd backend && cargo fmt -- --check` | 0 | **3.10 s** | `/tmp/cg_L0_fmt___check.log` |
| A-2 | L0 check（热） | `cargo check` | 0 | **0.2 s** | `/tmp/incr_check_no_op_warm_.log` |
| A-3 | L0 check（touch lib.rs） | `touch src/lib.rs && cargo check` | 0 | **12.6 s** | `/tmp/incr_check_touch_lib_rs_.log` |
| A-4 | L1 clippy（touch 后） | `cargo clippy --all-targets -- -D warnings` | 0 | **83.6 s** | `/tmp/incr_clippy_touch_lib_rs_.log` |
| A-5 | 干净库重建 | `ERP_DB=cloudpivot_arch9_ci4 RECREATE=1 bash scripts/verify/erp_clean_db.sh` | 0 | **6.43 s**（298 迁移 / V0367） | `/root/erp-clean-arch94.log` |
| A-6 | 闸④ 词表 | `python3 scripts/verify/biz_type_domain.py --strict` | **1** | **0.76 s**（3 个域外值） | `/tmp/gate_biz_type_domain.log` |
| A-7 | 闸⑦ 孤儿按钮 | `python3 scripts/verify/orphan_button_gate.py` | **1** | **0.12 s**（2 对） | `/tmp/gate_orphan_button_gate.log` |
| A-8 | 闸③ 不变量（干净库） | `python3 scripts/verify/erp_invariants.py` | 0 | **4.50 s** | `/tmp/gate_erp_invariants.log` |
| A-8b | 闸③ 不变量（**生产 144**） | 读 `sys_invariant_result` 的 `duration_ms` | — | **3.98 s**（79 项，1 BLOCKER） | 144 `/var/log/cloudpivot/` |
| A-9 | 闸② scope | `python3 scripts/lint/scope_gate.py --root . --baseline 21` | **1** | **2.55 s**（5 文件 + 2 未登记） | `/tmp/gate_scope_gate.log` |
| A-10 | 脚本可移植性盘点 | `grep -rl` 计数（见 §4.4.1） | — | — | 本文 §4.4.1 |
| A-11 | 144 crontab / 哑告警 | `crontab -l` + 日志 mtime | — | erp-health 静默 **5 天** | 144 `/var/log/cloudpivot/erp-health-202611.log` |
| A-12 | 工具链版本 | `rustc --version` × 3 台 | — | 99=`1.94.1`，145=`1.99.1`，144=`1.99.1` | 附录 A 内联 |
| A-13 | 菜单不变量 | `bash scripts/verify/menu_invariants.sh <db>` | 0 | **0.39 s** | `/tmp/gate_menu_invariants.log` |
| A-14 | HTTP 回归 | `verify.py smoke` / `verify.py all` | 0 / **1** | **0.83 s** / **34.5 s**（163/8/2） | `/tmp/smoke.log` / `/tmp/verify_all.log` |
| A-15 | **真库全量单测** | `cargo test --lib --features test-hooks -- --test-threads=1 --nocapture --skip business::flow::internal_control::db_tests` | 见下 | **实测两轮**：① 第一轮跑到 **1760/4014 例**、用时 **1054 s（17 m 34 s）**时被外部 SIGTERM 打断（**不是 OOM、不是用例失败**：`dmesg` 无 OOM 记录、失败清单 0 条、rc=101）② 第二轮从 01:37 起跑，01:55 已过 1919 例。按第一轮的实测速率 **1.670 例/s（100 例/min）**外推全量 **≈ 2404 s ≈ 40 min** | `/tmp/db_test_raw.log` |
| A-16 | L1 单测（无库） | `cargo test --lib` | 0 | **2.1 s**（**4021 passed**，1.78 s） | `/tmp/cg_test_lib.log` |
| A-17 | L1 单测（无库、单线程） | `cargo test --lib -- --test-threads=1` | 0 | **6.2 s**（6.03 s） | `/tmp/cg_test_lib_st1.log` |
| A-18 | L1 打包（仅编译） | `cargo test --lib --no-run`（touch 后） | 0 | **50.4 s** | `/tmp/incr_test__no_run_touch_.log` |
| A-19 | L1 typecheck | `cd frontend && pnpm typecheck` | 0 | **39.5 s** | `/tmp/incr_pnpm_typecheck_warm_.log` |
| A-20 | 闸⑤ 快照 | `snapshot_audit.py --install-views` → `snapshot_audit.py` | 0 | 1.37 s → **1.12 s**（26 项） | `/tmp/sa_install.log` `/tmp/sa.log` |
| A-21 | 闸⑥ 死权限码 | `python3 scripts/verify/perm_menu_gate.py` | 0 | **3.50 s** | `/tmp/gate_perm_menu_gate.log` |
| A-22 | 闸⑧ 角色码 | `python3 scripts/verify/role_code_gate.py` | 0 | **0.45 s** | `/tmp/gate_role_code_gate.log` |
| A-23 | 色板 | `python3 scripts/lint/palette_def_sync.py --root .` | 0 | **0.04 s** | `/tmp/gate_palette_def_sync.log` |
| A-24 | 库存写入口 | `python3 scripts/verify/inventory_write_gate.py` | 0 | **2.18 s** | `/tmp/gate_inventory_write_gate.log` |
| A-25 | C7 G5 | `python3 scripts/lint/changelog_in_tx.py --root .` | 0 | **3.38 s** | `/tmp/gate_changelog_in_tx.log` |
| A-26 | 图标不变量 | `DB=<db> bash scripts/verify/icon_invariant_teeth.sh` | 0 | **4.12 s**（2356 名） | `/tmp/icon.log` |
| A-27 | 145 增量编译 | 读 `/var/log/cloudpivot/test-deploy.log` 时间戳 | — | **58 s – 2 m 13 s** | 145 日志 |
| A-28 | 145 全周期部署 | 同上 | — | **1 m 16 s – 3 m 13 s** | 145 日志 |
| A-29 | test 二进制链接内存 | `ps` 采 rustc `%MEM` | — | **7.7 GiB RSS**（51.3% × 15 G） | 本次运行现场 |
| A-30 | DDL 验证 | `psql -f 01_ci_ddl_check.sql`（整段 ROLLBACK） | 0 | **14 组断言全过 / 0 残留** | `01_ci_ddl_check.out` |

**L1 合计（A-4 + A-16 + A-19）= 83.6 + 2.1 + 39.5 = 125.2 s ≈ 2 min 05 s** ✅ 达标（卡面目标 <3 min）
**L2 静态部分合计（A-1..A-27 中属 check.sh + menu + icon 的项）= 162.4 s ≈ 2 min 42 s**（不含真库单测与抽样）
**干净库全链（A-5 + 静态）= 6.4 + 162.4 ≈ 168.8 s ≈ 2 min 49 s**

---

## 附录 B · 13 域脚本盘点与可移植性（实测）

`scripts/verify/` 实盘（`ls`/`find` 计数，2026-11-10）：

| 类型 | 数量 |
|---|---|
| `.sh` | **312** |
| `.py` | **137** |
| `.mjs` | **35** |
| 可执行脚本合计 | **484** |
| `.sql` 证据脚本 | 37 |
| 目录（`baseline/` / `audit_features_20261104/` / `__pycache__` 等） | 10 |
| **文件总数** | **547** |

> 评审报告写「458 个脚本」是当时（11-09）的数；今天实盘 484 个可执行体。**本设计不引用 458 这个数**，S6 的覆盖断言按实盘动态计数（`find scripts/verify -name '*.sh' -o -name '*.py'`）。

13 域脚本（按 `e2eNN` 前缀，实测 106 个）：

| 域 | 前缀脚本数 | 备注 |
|---|---|---|
| E2E-01 采购 | 5 | 硬编码 `MAT1/MAT2` UUID |
| E2E-02 销售 | 5 | 含 `e2e02_deploy_145.sh`（部署类，非演练） |
| E2E-03 库存 | 5 | 含 `e2e03_reconcile.sql`（可复用的对账 SQL） |
| E2E-04 财务 | 10 | |
| E2E-05 资产 | 11 | |
| E2E-06 预算报销 | 12 | |
| E2E-07 合同 | 14 | |
| E2E-08 主数据 | 12 | |
| E2E-09 系统管理 | 8 | **有 `e2e09_run_all.sh` 统一入口** |
| E2E-10 流程引擎 | 5 | |
| E2E-11 任务协作 | 7 | |
| E2E-12 附件+PDF | 5 | 依赖 `chromium-pdf.service`（145 已装） |
| E2E-13 平台功能 | 6 | |
| E2E-14 本卡评审 | 1 | |
| **合计** | **106** | |

可移植性（实测 grep 计数）：

| 特征 | 命中数 | 占比（/484） |
|---|---|---|
| 引用 `/etc/cloudpivot/*.env` | 113 | 23.3% |
| 写死 `cloudpivot-dev.env` | 82 | 16.9% |
| 写死 `:8033` | 47 | 9.7% |
| 写死 `172.18.10.145` | 3 | 0.6% |
| **接受 `BASE_URL`** | **35** | **7.2%** |

**结论**：L2 的「13 域抽样」在 B 档迁移完成前**不可能覆盖 13 域**，只能覆盖 A 档（门禁 + `verify.py`）。这不是保守，是实测。因此 D-10 定的路线是「先把 13 张 B4 卡做完，再逐域放开 manifest」，而不是「先上 cron 再说」。

---

## 附录 C · 触发矩阵速查（贴在 145 的 crontab 注释里用）

```cron
# ── ARCH-9 CI/CD（卡 t_2cd4336a）────────────────────────────────────────────
# 145（测试机，4 核 / 7 G）：门禁 + 干净库 + 13 域抽样。**不跑 cargo test**（链接要 7.7 GiB）
10 3 * * *  /opt/cloudpivot/scripts/regression/run_layer.sh L2 --host 145 >> /var/log/cloudpivot/ci/l2a.log 2>&1
0  9 * * *  /opt/cloudpivot/scripts/regression/watchdog.py >> /var/log/cloudpivot/ci/watchdog.log 2>&1
# ───────────────────────────────────────────────────────────────────────────

# 99（开发机，4 核 / 15 G）：真库全量单测（唯一有内存的机器）
40 3 * * *  /opt/cloudpivot/scripts/regression/run_layer.sh L2 --host 99 --only dbtest-full >> /var/log/cloudpivot/ci/l2b.log 2>&1

# 144（生产）：**不加任何 CI 任务**，只保留既有只读巡检
# （erp-health-daily.sh 已被删除但 cron 还在 —— 见 §4.7 W3，批 2 清理）
```

| 谁 | 什么时候 | 跑什么 | 挂了谁被叫醒 |
|---|---|---|---|
| 开发者（99） | `git commit` / `git push` | L0 / L1 | 开发者本人（push 被拒） |
| 145 cron 03:10 | 每天 | L2-A（门禁 + 干净库 + 抽样） | 飞书 + 卡（backend-developer） |
| 99 cron 03:40 | 每天 | L2-B（真库全量单测） | 飞书 + 卡 |
| 145 cron 09:00 | 每天 | 看门狗 W1–W4 | 飞书 + 卡（运维/manong） |
| 发版负责人 | 发版前 | L3（全量脚本 + e2e） | 阻断 `deploy.sh` |
| 总管（manong） | 紧急 | `--force` 绕过 L3 | 飞书红卡 + `sys_ci_run.note` |


