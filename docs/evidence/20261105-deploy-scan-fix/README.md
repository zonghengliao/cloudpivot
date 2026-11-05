# 【DEPLOY-SCAN-FIX】deploy.sh 只跑 4 个迁移、漏 13 个 —— 根因与修复证据（卡 t_9467ce9f）

日期：2026-11-05　执行：backend-developer

## 一句话结论

`deploy.sh` 选待跑迁移用的是 **`version > max(applied)` 比大小**（旧 L184/L189/L237），
而引擎（nik-db）用的是 **按 `(kind,name)` 做集合差**（`vendor/nik-db/src/migrate/mod.rs:1263-1275`）。
两者只在「库内没有 gap」时等价 —— 2026-11-05 生产库 max 是 V0235 却缺 11 条更低版本，
于是这 11 条**永远选不出来**，V0240 因依赖 V0227 建的 `biz_budget_subject_map` 而失败、部署中断。

已改为集合差（唯一实现在 `scripts/deploy/migrate_plan.sh`），并加了「对账」「applied_via 白名单」「集合复核」三道硬自检；
在预演库上从 V0219 基线复现事故现场并用卡面点名的 13 条做回归，**26/26 断言通过**；真实 `deploy.sh` 端到端在预演库跑通（`RECON_OK 13==13` / `VIA_OK` / `SET_OK`）。

## 一、根因（带行号）

出事的脚本是 commit `7d96692`（2026-11-05 10:51 那次发版）。相关三处**都是「比大小」**：

| 位置（修复前） | 代码 | 问题 |
|---|---|---|
| 第 1 步 L184 | `APPLIED_MAX=$(... select coalesce(max(version::int),0) ...)` | 把「库里最大的已应用版本」当游标 |
| 第 1 步 L188-192 | `disk_mig_files \| awk ... if ((v+0) > applied)` | **只保留版本号 > 游标的文件** ⇒ 游标以下的 gap 全部丢弃 |
| 第 1 步 L228-231 | `if [ "$ver" -le "$APPLIED_MAX" ]; then ... continue` | 再按游标跳一次（同样的错判） |
| 第 5 步 L339-350 | `NEW_MAX` vs `EXPECT_MAX`（都是 max(version)） | 收口判据也是比大小：**跳过一堆 gap 依然能通过** |
| 第 5 步 L349-352 | `pending_versions_in_db`（集合口径） | 检查本身是对的，但它排在「迁移成功之后」；V0240 一失败就 `die`，这段根本没跑到 |

引擎的真实判定（`backend/vendor/nik-db/src/migrate/mod.rs:1263-1275`，`needs_apply`）：

```rust
match applied.get(&(migration.kind, migration.name.clone())) {
    Some(record) if record.is_success => /* 只有可重复类才看指纹变化 */,
    Some(_) => true,   // 上次失败 → 重跑
    None    => true,   // 没记录 → 执行（不看版本号大小！）
}
```

低版本（gap）补迁是**引擎允许**的行为，只打 `out_of_order` WARN（`mod.rs:876-888`）。

### 生产现场证据（`/var/log/cloudpivot-deploy-20261105-release.log`）

```
L13  [10:51:49] 库里已应用的最大迁移版本：V0235
L16  [10:51:49] 后端改动：有　　前端改动：有　　待应用迁移：6 个
L17-22       + V0236 V0237 V0238 V0239 V0240 V0241        ← 只挑了 > 235 的 6 条
L89  ==> [V0240] V0240__erp_budget_mapping_material_type.up.sql
L90      ERROR:  relation "biz_budget_subject_map" does not exist
L105 ############ 部署失败：迁移应用失败（上面的 APPLY_FAILED 行） ############
```

当时磁盘上真正没记账的是 **18 条**：V0220 V0221 V0226..V0234（11 条 gap）+ V0235..V0241（7 条）。
脚本只给了 6 条（V0235 已被引擎启动期补跑过），跑到 V0240 就炸，V0241 连碰都没碰 ——
与卡面「跑了 5 个 / 漏 13 个」完全对应（11 条 gap + V0240 失败 + V0241 未跑）。

摘录见 `prod_deploy_log_excerpt.txt`。

## 二、改了什么

| 文件 | 改动 |
|---|---|
| `scripts/deploy/migrate_plan.sh`（新增） | **待跑清单唯一实现**：`disk = backend/migrations/*/V<n>__<name>.up.sql`（递归全部分目录）− `done = schema_migrations 里 kind='versioned' and success 的 name`；按版本号数值升序；同时给出「低于 max 的乱序补迁」清单、`applied_via` 非法值清单、磁盘文件名非法 / 同名冲突检查。文件头逐条写清引擎依据（`kind.rs:84-111`、`history.rs:219-221`、`mod.rs:1263-1275`、`mod.rs:852-888`） |
| `scripts/deploy/deploy.sh` | 删掉 max 比大小的三处选择逻辑，改调 `mp_build_pending`（同一函数，自测脚本也调它）；第 1 步加三种预检（`applied_via` 非法值、文件名不可识别、同名冲突）并打印 gap 预警；第 5 步加三道自检（对账 / `applied_via` / 集合复核），迁移工具改为 `--assert-pending` 调用；第 6 步重启后再校集合差 + `applied_via`；第 8/8 收口增「磁盘 - 库 = 空」结论行 |
| `scripts/verify/deploya_migrate.sh` | 引擎认可的 `applied_via` 写死为 `executed imported baseline`（写记录只用 `imported`），跑前/跑后各校一次；新增 `--assert-pending`（用工具自己独立算出的待跑集合校验传入清单，不对称拒跑）；跑完三道自检：`RECON_OK`（applied 增量 == 本次条数）、`VIA_OK`、`SET_OK`（磁盘 - 库 = 空）；`--allow-partial` 只给预演库建基线/复现现场用 |
| `scripts/deploy/selftest_migrate_plan.sh`（新增） | 预演库一键回归：铺 V0219 基线 → 复现事故现场（只补 V0235..V0239）→ 对比旧法/新法 → 旧法跑 V0240 必失败 → 新法跑完并三道自检 → 负样本（少给一条 / 非法 `applied_via`）。26 条断言 |
| `scripts/deploy/README.md` | 第 1/5 步判据改写为集合差 + 三道自检；「已知的坑」补两条（max 比大小、自造 `applied_via`）；新增「改完清单逻辑怎么验」小节 |

`deploya_migrate.sh` 原来的记账口径没变（`version` 不补零、`checksum=sha256`、`source` 记真实子目录），只把 `applied_via` 从硬编码字符串换成常量并加了校验。

## 三、验收对照（卡面 6 条）

| 卡面要求 | 结果 | 证据 |
|---|---|---|
| ☑ 清单构造逻辑看懂，根因带行号 | 见上「一、根因」 | 本文件 + `prod_deploy_log_excerpt.txt` |
| ☑ 在预演库上验证「磁盘全部 up.sql 都被纳入待跑清单」 | 26/26 断言通过：新法清单条数 == 磁盘文件数 − 已记账数；卡面点名的 13 条一条不少 | `selftest_run2.txt` |
| ☑ 自检：跑完对账（applied 增量 == todo 数） | `RECON_OK applied 增量 13 == 本次条数 13（169 → 182）`；deploy.sh 侧另有 `✅ 对账通过` | `deploy_e2e_rehearsal.txt` L265-269 |
| ☑ 自检：applied_via 合法值校验 | 引擎只认 `executed/imported/baseline`；负样本注入 `deploy-backfill` 后被迁移工具 exit 2 拒跑；跑后 `VIA_OK` | `selftest_run2.txt` ⑥ 段 |
| ☑ 用本次缺失的 13 个迁移做回归（V0219 基线 → V0241） | 预演库从 V0219 基线起：旧法只挑到 2 条且 V0240 必失败（`relation "biz_budget_subject_map" does not exist`，与生产日志逐字一致）；新法挑到 13 条全部跑通，V0227 先于 V0240 ⇒ V0240 成功 | `selftest_run2.txt` ④⑤ 段 |
| ☑ git commit（`fix(deploy):`） | 见卡面交接的 commit id | `git log` |

另有一次**端到端**验证：把修复后的脚本放进一个干净的 clone（HEAD == origin/main，绕过 99 工作区的分叉/脏树），
对着预演库真跑 `deploy.sh --skip-backend --skip-frontend`：

```
[1/8] 库内已应用 versioned 迁移：169 条，最大版本 V0250
      ⚠ 其中 13 条版本号低于库内最大已应用 V0250：V0220 V0221 V0226 ... V0241
[5/8] 迁移前已记账 169 条，本次待跑 13 条
      PENDING_ASSERT_OK 传入清单 == 独立算出的待跑集合（13 条，按版本升序）
      APPLY_OK V240 ... APPLY_OK V241 ...        ← V0240 这次成功了（V0227 已先跑）
      RECON_OK applied 增量 13 == 本次条数 13（169 → 182）
      VIA_OK / SET_OK
      ✅ 对账通过　✅ applied_via 合法　✅ 集合复核：磁盘上的版本化迁移全部已记账
[8/8] 迁移记账 182 条，最大版本 V0250　✅ 磁盘迁移全部已记账（磁盘 - 库 = 空）
```

日志：`deploy_e2e_rehearsal.txt`。

## 四、证据文件

| 文件 | 内容 |
|---|---|
| `prod_deploy_log_excerpt.txt` | 2026-11-05 10:51 那次失败发版的原始日志摘录（144 的 `/var/log/cloudpivot-deploy-20261105-release.log`） |
| `selftest_run1.txt` | 首轮自测（25 条断言；尚未把断言换成 `mp_build_pending` 版本，留档） |
| `selftest_run2.txt` | **最终自测：26 条断言全 PASS**（走的是 deploy.sh 同一个 `mp_build_pending`） |
| `selftest_run3_keep.txt` | 同上，`KEEP=1` 保留预演库（供随后做 deploy.sh 端到端） |
| `deploy_e2e_rehearsal.txt` | 真实 `deploy.sh` 在预演库上的端到端日志（13 条迁移 + 三道自检 + 收口） |
| `deploy_noop_dryrun.txt` | 同一脚本在「磁盘全已记账」库上的 `--dry-run`：`待应用迁移：0 个` → `[5/8] ✅ 无待应用迁移`（空清单分支不炸） |

复现命令（任何开发机，不碰生产）：

```bash
DEPLOY_ENV_FILE=/tmp/rehearsal.env bash scripts/deploy/selftest_migrate_plan.sh
```

## 五、需要知道的事（不在本卡范围）

1. **生产尚未用新版 deploy.sh 发过版**。下次发版会顺带把当前真实缺口纳入清单：库里 180 条已记账、max=V0249，磁盘上 **V0248 与 V0250 未记账**（V0248 < 249，会被标成「乱序补迁」）—— 这正是修复生效的现场。发版前建议先 `--dry-run` 看一眼清单。
2. 只动了 shell 脚本 + 文档，**没有 Rust / 前端改动**，`cargo fmt/clippy/check`、`pnpm typecheck` 与本卡无关（未跑全量，避免和并行卡的 cargo target 锁打架）。
3. 预演库 `cloudpivot_rehearsal_deploy` 建/删用本机 `su - postgres`（连库账号 `alun` 没有 CREATEDB）；脚本 `KEEP=0`（默认）跑完会自己删库。
4. `backend/migrations/` 下第三个目录 `seed/` 只有 `S__*.sql`（可重复种子），**不是版本化迁移**，不纳入待跑清单（引擎按 `kind` 分开处理），无需改扫目录逻辑。
5. 99 这台机器上留了两个 dry-run 产生的 `/var/log/cloudpivot-deploy-20261105-1521*.log`（端到端验证时生成，非生产日志）。
