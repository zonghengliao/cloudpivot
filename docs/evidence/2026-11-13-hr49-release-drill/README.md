# HR-49 · 上线演练（空库首启 + 克隆库升级，含 2211 翻末级）

- 卡：`t_926048d4`（批 5 收口，来自 ARCH-9-2 §8.1）
- 交付物：`scripts/verify/hr/hr_49_release_drill.sh`（一条命令跑四个场景）
  + `backend/src/business/hr/release_drill_contract_tests.rs`（10 条静态契约）
- 演练环境：**库只建在 145（172.18.10.145）**，实例只监听 `127.0.0.1:18149~18152`；
  99 上**没有**建任何库。
- 原始输出：`00-run-all.out`（全量，PASS=92 FAIL=0 rc=0）+ `01~04`（逐场景）

## 一、结论先说

| 验收项（卡面） | 结果 | 证据 |
|---|---|---|
| 空库首启成功 | ✅ 全新空库回放 **323 条版本化迁移 + 12 条种子 = 335 条**，最高版本 **V0403**，`/health` 200 | `01-S1.out` |
| 克隆库升级成功（生产快照） | ✅ 生产 144 快照（V0391 / 310 版本化 + 11 种子）→ 引擎只应用 **14 条**（13 版本化 + 1 种子）→ V0403、`/health` 200 | `02-S2.out` |
| 克隆库升级成功（老库，含 2211 翻末级） | ✅ 老库 V0376（2211 还是末级）→ 应用 **28 条** → 2211 `is_leaf` **1→0**，下长出 2211.01/02/03，9 个工资末级科目齐 | `03-S3.out` |
| 两次都不丢数据 | ✅ 生产克隆：**198 张存量表行数一条不少**；老库：**166 张存量表一条不少**，凭证/分录/金额/科目快照逐条不变 | `S2-before/after-counts.txt`、`S3-before/after-counts.txt` |
| （额外）升级护栏有牙 | ✅ 老库父科目 2211 上有分录时，升级被 V0377 护栏**拦下**，库停在 376、一条迁移都没记账、分录原样 | `04-S4.out` |

## 二、四个场景怎么造

### S1 空库首启（`fresh`）

145 上建 `hr49_fresh_drill` 空库 → 起实例（profile=test，端口 18149）→ 引擎自动回放全部迁移。

断言 26 条，关键几条：

- 引擎日志「数据库迁移完成: 应用 **335** 个迁移」= 磁盘 323 版本化 + 12 种子（**条数从磁盘现算**，
  不写死，加迁移自动跟上）
- `schema_migrations`：versioned 323 / seed 12 / 最高版本 403
- HR 10 张表齐；薪资项种子 17 项；工资末级科目 9 个；2211 是非末级；HR 目录 1 + 页面 5 +
  权限码行 40；HR 四套审批流；2 条工资凭证规则
- **真 HTTP 建一个员工**（`EMP-0001`）→ 停实例 → 再起 → 「应用 **0** 个迁移」→
  该员工详情仍 200 且工号一致 ⇒ 「不丢数据」在空库路径上也成立（不只是行数对拍）
- `hr_invariants.sh` 8 条 BLOCKER 零违规

### S2 克隆库升级 · 真实生产快照（`prod`）

`ssh 144 → pg_dump --no-owner --no-privileges`（**生产库只读**）→ 灌进 145 的 `hr49_prod_drill`
（4.6 MB）。

快照落地后实测：生产 **V0391 / 310 条版本化 + 11 条种子**，与架构师在
`docs/evidence/2026-11-13-v0397-migration-collision/11_prod_144_migration_state.out`
里记的 `max_applied_version = 391` 一致。

升级前先做两条「会不会炸」的预检（架构师提醒过的风险点）：

- 「同 org+period 多张未删薪资单」= **0** ⇒ V0397 重建 `uk_biz_sal_sheet_period` 不会失败
- 存量 198 张表行数快照

引擎应用的 14 条（= 13 版本化 + 种子里 HEAD 才有的 `hr_employee_grants`）：
`cost_menu_group` / `attendance_lock_guard` / `hr_leave_flow` / `hr_salary_flow` /
`hr_adjust_resign_flow` / `hr_salary_items_seed` / `hr_salary_period_after_reverse` /
`hr_selfcheck` / `preference_palette_ids_gallery77` / `hr_flow_approver_role_grants` /
`hr_page_code_split` / `sales_clerk_receivable_page` / `hr_manager_approval_center_page`。

升级后：V0403 / `/health` 200 / `uk_biz_sal_sheet_period` 在位 / 17 薪资项 / 9 科目 /
4 套流程 / 5 个 HR 页面 / 40 行 HR 权限码 / V0398 收口自检通过。

**逐表行数对拍（198 张存量表）**：

```
[INFO] 白名单内增长：
  biz_flow_def(41→45) biz_flow_def_version(62→66) biz_flow_edge(263→284)
  biz_flow_node(278→300) biz_flow_node_assignee(104→113) biz_salary_item(0→17)
  schema_migrations(321→335) sys_menu(499→504) sys_role_menu(1272→1288)
```

一张表都没有减少（`biz_account` / `biz_voucher_rule` / `sys_config` / `sys_field_mask` /
`biz_attachment_rule` 在生产上早就有了，本次零增长 —— 说明种子是幂等的，没有重灌）。

再重启一次：应用 0 条迁移，且重启前后 198 张表行数**完全一致**。`hr_invariants` 零违规。

### S3 克隆库升级 · 老库（`legacy`，含 2211 翻末级）

老库不是「手工改数据造出来的」，而是用**发布过的旧版本迁移集合**回放出来的：
把 `backend/migrations/` 里 **≤V0376** 的文件（296 条版本化）+ 11 条老种子（不含 HEAD 才有的
`hr_employee_grants`）复制成一份迁移树，用临时 profile 的 `[database.migration].path` 指过去，
引擎照常回放 —— 得到的 `schema_migrations` 与当年那个版本的客户库同形（含 checksum）。

选 V0376 的理由：V0377（成本科目）是**第一次**把 2211 翻成非末级的迁移。停在它之前，
2211 就是 `is_leaf = 1` 的末级科目 —— 这才是「2211 翻末级」真正要演练的起点
（到 V0383 或生产 V0391 时早就翻过了，翻不动也证不了）。

升级前基线（实测）：2211 `is_leaf=1`、2211 下 0 个子科目、9 个工资末级科目一个都没有。
再灌入「历史数据」：1 张凭证（借 1002 银行存款 1000 / 贷 5602.01 1000）+ 1 个标记配置行
（`hr49.legacy.marker`）—— 用来证明升级不只是「表还在」，而是**行与金额都在**。

升级（应用 28 条）后断言：

- 2211 `is_leaf` **1 → 0**（这是本卡点名要的那一下）
- 2211 下长出 **3** 个末级子科目（2211.01/02/03）
- 9 个工资末级科目齐；其中 2211.0x + 5602.1x 共 **6** 个带 DEPT 辅助核算，
  2241.03/04、2221.03 共 3 个不带（设计口径：代扣/其他应付不分部门）
- 2241 / 5602 / 2221 三个上级都不是末级；2 条工资凭证规则可解析到科目；17 个薪资项
- **不丢数据**：分录条数不变、借方合计不变（1000.0000）、凭证 id 集合不变、
  历史分录的科目编码快照仍是 `5602.01`、标记配置行原样还在
- 逐表行数对拍：**166 张存量表一条不少**，新增 10 张表（HR 那 10 张：老库在 V0376 还没有）
- 升级后重启：应用 0 条迁移，重启前后行数完全一致；`hr_invariants` 零违规

### S4 升级护栏反证（`guard`）

同 S3 的老库，但把 1 条分录**直接记在父科目 2211** 上（老库时它是末级，记账闸允许）。

结果：升级**必须**失败 —— 实测实例起不来（`FATAL: 服务启动失败 … 数据库迁移失败`），
`schema_migrations` 仍是 307 条（V0376，一条都没记账）、2211 仍是末级、那条分录原样还在。

这条反证的价值：**它是发布 144 时的一条真实风险** —— 若某个客户的老库（≤V0376）曾把分录直接
记在 2211/4001/4101 这类汇总科目上，升级会被 V0377 的护栏拒绝（设计意图：翻成非末级后报表按
末级取数、年结按末级结转会漏结）。这种库必须**先人工把分录改记到新末级科目**再升级，
不能靠脚本硬闯。

## 三、两条实现事实（踩过才知道）

1. **迁移引擎把 PG 的 `RAISE EXCEPTION` 文本吞了**：失败时日志只有
   `数据库迁移失败: query failed: : db error`，护栏的「有 N 条分录」原话看不到
   （`nik-db` 这一层没把 `DbError` 的 detail 打出来）。所以 S4 的判据改成三条不依赖文案的：
   启动 FATAL + **直接跑护栏那条 SQL 证明会命中** + 库状态原地不动。建议后续单开卡修引擎日志。
2. **临时 profile 覆盖迁移目录时不能再设 `seed_path`**：引擎从 `path` 递归装载，
   `migrations/seed/S__*.sql` 会被 `seed_prefix` 认成种子；同时把 `seed_path` 指到同一个
   `seed/` 目录会报 `duplicate up migration for the same name`（本卡第一版实测踩到）。

## 四、残渣

- 145 上 `hr49%` 库计数 = **0**（四个演练库全部 DROP 并复查 `pg_database`）
- 端口 18149~18152 无监听；`backend/config/config-hr49mig.toml`（临时 profile）已删
- 生产快照 `prod-144.sql`（4.6 MB，含生产数据）**没有进仓库**，只在运行目录里，跑完即弃
- 99 上未建任何库

## 五、文件清单

| 文件 | 作用 |
|---|---|
| `scripts/verify/hr/hr_49_release_drill.sh` | 四场景演练主脚本（模式 `all|fresh|prod|legacy|guard`；演练库名护栏；逐表行数对拍；失败自动留库） |
| `backend/src/business/hr/release_drill_contract_tests.rs` | 10 条静态契约（四场景在不在、护栏在不在、口令有没有落盘、迁移条数是不是现算…） |
| `backend/src/business/hr/mod.rs` | 挂 `#[cfg(test)] mod release_drill_contract_tests;` |
| `scripts/verify/README.md` | 新增「上线演练」章节（含两条已知边界） |
| `docs/architecture/ARCH-9-人力资源模块设计.md` | §8.1 HR-49 标已落地 + 落地记录 |
