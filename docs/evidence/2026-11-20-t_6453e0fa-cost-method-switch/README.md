# M5-31 · 成本方法切换的迁移策略（存量期间）— 证据包

- 卡：`t_6453e0fa`（M5-31，父卡 M5-30 `t_2ce07449` / 提交 `184be921`）
- 日期：2026-11-20 · 主机：99（开发机，PG 在 145）
- 演练库：`cp_m531_dev`（空库 → 实例首启跑全量迁移：**401 迁移 / 203 表**）、
  `cloudpivot_test_m531`（`scripts/verify/make_full_suite_db.sh` 种子完备库，真库用例与 HTTP 冒烟都跑它）
- 结论：**零迁移 / 零新权限码 / 零菜单 / 零前端改动**；新增 2 条只读写接口 + 1 个错误码（`66113`），
  31 条用例（25 纯逻辑 + 6 真库）+ 24 条真实 HTTP 断言全绿；成本域回归 1497 条全绿。

## 一、这张卡要解决什么

M5-01 落地了「核算方法配置」的时间线（`biz_cost_method_config`：`method` + `effective_from/effective_to`），
并留了一句明确的口子：**「切换时的存量期间重算策略（M5-31）」本卡只提供时间线与已生效锁定两块地基**。
M5-03…M5-30 的成本计算 / 结算 / 还原全部按「一期整期一种方法」跑（ARCH-9 D-3），
于是一旦用户改方法，最危险的问题不是「改不了」，而是**改完之后历史期间的口径被悄悄改掉**——
同一张 10 月工资表，昨天按分批法出，今天按品种法重算，报表对不上而没人发现。

本卡把这件事收成一条规则并落成代码：

> **切换只发生在期间边界上；切换日之前的每一个期间，解析出的方法逐日不变，结果表一个字节不动。**

## 二、交付了什么

| 层 | 文件 | 说明 |
|---|---|---|
| 后端 | `app/src/business/erp/cost/cost_method_switch_dto.rs` | 纯逻辑：期间边界（`month_bounds` / `next_month_start` / `shift_month_start`）、三条护栏、建议切换日、「切换前 vs 切换后」逐期间方法推演、两笔写、出参 DTO。**25 条单测** |
| 后端 | `app/src/business/erp/cost/cost_method_switch_model.rs` | 取数：`biz_cost_period` 按期间聚合的口径台账（批次数 / 状态去重）+ 对象号批量补名（单条 `IN`，防 N+1） |
| 后端 | `app/src/business/erp/cost/cost_method_switch_handler.rs` | 两条接口：`GET .../switch-plan`（预演，只读）/ `POST .../switch`（事务内收口旧版 + 插入新版） |
| 后端 | `app/src/business/erp/cost/cost_method_model.rs`（+2 函数） | `insert_in` / `update_in`：把「同一份配置写入口」扩成可吃 `SqlExecutor`，与切换的两笔写共用同一套 SQL（不另开一套） |
| 后端 | `crates/cloudpivot-kernel/src/shared/error_code.rs` | 新码 `66113 ERP_COST_METHOD_SWITCH_GUARD`（66xxx 段接在 66112 后）+ 码表稳定性用例 |
| 测试 | `app/src/business/erp/cost/cost_method_switch_db_tests.rs` | **6 条真库用例**（`ERP_DB_URL` 开关，未设即 SKIP） |
| 证据 | 本目录 | 见 §五 验收表 |

**没做的（刻意）**：没有迁移（时间线在 V0459 就有，切换史 = 配置时间线，M5-01 证据 §五 已定）、
没有新权限码（复用 `cost-method:list` / `cost-method:update`）、没有菜单、没有前端页面
（前端对话框属另一件事，已另开卡 `t_11fe2da1`，挂在本卡下）。

## 三、口径（是决定，不是待办）

### 1. 切换在数据上只有两笔写

```text
旧版本（切换日前一天仍在生效的那条、同范围、启用中）  effective_to := 切换日 − 1
新版本                                              effective_from := 切换日（长期有效）
```

两笔写在**同一个事务**里（`begin → 读快照 → 预演护栏全过 → 收口 → 插入 → commit`）。
任何一步失败都回滚：不会出现「旧版收了、新版没插」的空窗（那会让切换月落到代码兜底方法上，
口径无缘无故变一次）。提交之后**没有任何**可能失败的后续动作（出参全用事务里算好的数据拼，
不回读）—— 约定 11 在这里的落地方式就是「提交后没有第二步」。

### 2. 切换日三条护栏（`409 + 66113`，每条报文都给建议日）

| # | 规则 | 为什么 |
|---|---|---|
| 1 | 必须是某月 1 日 | 一期引擎整期按一种方法算（ARCH-9 D-3）；期内切换 = 同一期前后半段两种方法 = 要分段重算（二期） |
| 2 | 不早于「今天所在期间的下月 1 日」 | 不许追溯：本期已经开始了，切它就得重算本期 |
| 3 | 切换日所属期间不许已有成本批次 | 该期间已有归集 / 计算 / 结算数据（`DRAFT` 也算），切它就是改写已算过的口径 |

建议切换日 = 「下月 1 日」与「现有最大期间之后的下一个期间首日」取晚者 —— 用户照着报文改就能过。
`REVERSED` / `CANCELLED` 只剩留痕行的期间不构成「已有批次」（与 V0373 唯一键同口径）。

**不复用** `66105`（已生效版本不许改口径）：那条的出路是「新增一版」；本码的出路是「换一天」。
**不复用** `66102`（生效期重叠）：那条说「同一段区间两条版本」；本码说「切换点本身不合法」。
同一个切换日切第二次才撞 `66102`（报文点明「同一天只能切一次」）。

### 3. 「历史期间不重算」在四个层次上都成立

| 层次 | 机制 | 用例 |
|---|---|---|
| 配置时间线 | 旧版窗口只收窄到切换日前一天 ⇒ 切换日之前的每一天 `resolve` 结果不变 | 5 条纯逻辑用例（含 `plan_closes_old_version_and_keeps_history_unchanged`） |
| 结果表 | 接口只写 `biz_cost_method_config` 两行，`biz_cost_period` / `biz_cost_period_item` / 归集 / 分摊 / 还原 / 凭证**零 SQL** | 真库用例 1（期间行状态 / 总成本逐字快照对比）+ HTTP 第 ⑨ 项 |
| 计算入口 | 老期间重算走 `ensure_methods_runnable_in_tx(老期间)` ⇒ 仍是分批法 ⇒ 放行 | 真库用例 1（老期间 Ok、切换月 66101） |
| 用户可见 | 预演 `history[]` 每条 `recalculated=false` + `unchanged=true`，出参 `history_untouched=true`（自检结论，不是口号） | 纯逻辑 + 真库 + HTTP 第 ③ 项 |

### 4. 目标方法没实现时：允许预演，执行要显式确认

一期只有分批法。切到 `VARIETY` / `STANDARD` 是**二期 / 三期的合法目标**，所以预演照做，
但 `engine_ready=false` + `warnings[]` 写明「新期间点计算会 66101、无法结算」；
真执行必须带 `confirm_unimplemented: true`，否则 `409 + 66101` 且**一行没写**。
理由：静默把默认口径改成一个「算不出来」的方法，等于让下个月没法结算 —— 比报错坏得多。

### 5. 对象级配置优先，切换要让它显形

全公司默认切换时，有自己版本的成本对象**不受影响**（对象级 > 全公司默认，M5-01 判定顺序）。
预演把这类对象列进 `object_overrides[]`（带对象号），并在告警里说清楚「这次切换不影响它们」——
否则用户会以为「切了全局，全公司都换了」。

## 四、接口一览（真实 HTTP 已验证）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/api/business/erp/cost-method/switch-plan` | `business:erp:cost-method:list` | 预演（只读）：护栏 + 存量期间清单 + 切换后期间 + 两笔写 + 告警 |
| POST | `/api/business/erp/cost-method/switch` | `business:erp:cost-method:update` | 执行（事务内两笔写） |

出参字段与一份真实报文见 `docs/evidence/2026-11-20-t_6453e0fa-cost-method-switch/m531_http_smoke.py`
（24 条断言就是字段清单）。

## 五、真跑的验收

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_fmt.out` | `cargo fmt --check` | rc=0 |
| `02_clippy.out` | `cargo clippy --lib -- -D warnings` | rc=0（零警告） |
| `03_unit_and_db_tests.out` | `cargo test --lib cost_method_switch`（`cloudpivot_test_m531`） | **31 passed / 0 failed**（25 纯逻辑 + 6 真库） |
| `04_cost_domain_regression.out` | `cargo test --lib business::erp::cost::` | **1497 passed / 0 failed** |
| `05_permission_audit.out` | `system::menu::permission_audit_tests`（N14–N21） | **8 passed**（零新码 ⇒ N14 不动；两条新路由都被 N16 认到权限门） |
| `06_error_code_table.out` | `erp_error_codes_are_stable`（66xxx 码表逐值钉死） | 1 passed（`66113` 已入表） |
| `07_empty_db_first_boot.out` | 空库 `cp_m531_dev` 首启 | 401 迁移 / 203 表 / `/health` 200 |
| `08_http_smoke.out` | 真机 HTTP 冒烟（种子完备库 + 隔离实例 18076） | **24 条断言全过** |

真库用例怎么跑（**不要**在 99 上指 `127.0.0.1:5432`）：

```bash
cd /opt/cloudpivot && ERP_DB=cloudpivot_test_m531 PORT=18078 bash scripts/verify/make_full_suite_db.sh
cd backend/app && \
ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cloudpivot_test_m531' \
  cargo test -p cloudpivot --lib cost_method_switch -- --test-threads=1
```

HTTP 冒烟怎么跑：

```bash
bash docs/evidence/2026-11-20-t_6453e0fa-cost-method-switch/m531-start.sh   # 18076（库 cloudpivot_test_m531）
# 先把 erp.cost.enabled 打开再重启实例（空库默认 false ⇒ 403 66079）
psql -h 172.18.10.145 -U cloudpivot -d cloudpivot_test_m531 \
  -c "update sys_config set config_value='true' where config_key='erp.cost.enabled'"
python3 docs/evidence/2026-11-20-t_6453e0fa-cost-method-switch/m531_http_smoke.py
```

## 六、31 条用例（都钉什么）

**纯逻辑 25 条**（`cost_method_switch_dto` 19 + `cost_method_switch_handler` 4 + `cost_method_switch_model` 2）：

| 组 | 用例 | 钉住 |
|---|---|---|
| 期间边界 | `month_bounds_cover_leap_and_year_end` / `next_month_start_handles_year_end` | 闰年 2 月 / 12 月跨年 / 非法形状（`2026-13`、`2026/11`、`26-11`） |
| 护栏 1 | `switch_on_must_be_a_period_start` | 期内切换 ⇒ 66113 + 建议日 |
| 护栏 2 | `switch_on_cannot_be_retroactive_or_in_current_period` | 上月 / 本月 / 去年都拒；恰好下月 1 日放行 |
| 护栏 3 | `switch_on_period_with_batches_is_rejected` / `reversed_only_period_does_not_block_the_switch` | 有批次拒并点名期间/批次/状态；只剩红冲留痕行放行 |
| 幂等 | `second_switch_on_the_same_day_is_an_overlap` | 同一天第二次 ⇒ 66102（不是 66113） |
| 两笔写 | `plan_closes_old_version_and_keeps_history_unchanged` / `empty_timeline_only_inserts` / `closes_the_version_that_still_covers_the_day_before` / `disabled_versions_are_ignored` / `new_version_spec_matches_the_insert_op` | 收口那条对不对 / 没有旧版只插 / 停用不参与 / 落库规格与 ops 逐字一致 |
| 影响面 | `future_periods_with_batches_are_reported` / `object_level_overrides_are_listed_and_untouched` / `object_scope_switch_does_not_touch_global` / `suggested_switch_day_is_the_later_of_the_two` | 将来已有批次提示「下次重算按新方法」/ 对象级覆盖显形 / 对象级切换不动全局 |
| 其他 | `switching_to_an_implemented_method_is_ready` / `parse_target_defaults_and_validates_scope_pairing` / `plan_res_carries_labels_and_the_untouched_flag` / `message_names_switch_day_scope_and_history` / `permissions_are_reused_from_m501` / `plan_is_read_and_switch_is_write` / `today_is_cst` / 2 条 SQL 形状 | 分批法可算 / 范围成对 66103 / 出参标签 / 文案 / 零新码 |

**真库 6 条**（`cost_method_switch_db_tests`）：

| # | 用例 | 钉住 |
|---|---|---|
| 1 | `switch_keeps_history_and_moves_future` | **核心**：切换后老期间方法不变 / 结果表逐字不动 / 老期间照旧能算 / 新期间被 66101 拦 |
| 2 | `global_switch_closes_the_seed_version` | 全公司默认：收口的是 V0459 种子行，收尾还原（先删新版再还原，避开重叠预检） |
| 3 | `switch_day_inside_an_existing_period_is_rejected` | 该期间已有 DRAFT 批次 ⇒ 66113；清掉后同一预演放行 |
| 4 | `unimplemented_target_needs_explicit_confirm` | 未确认 ⇒ 66101 且**零写入**（种子行仍长期有效、配置行数不变）；确认后放行 |
| 5 | `second_switch_on_the_same_day_is_an_overlap` | 409/66102，第一次结果不被破坏 |
| 6 | `period_ledgers_aggregate_by_period` | 台账按期间聚合、状态去重排序、只剩红冲留痕不挡切换 |

## 七、给下游卡的接缝

- **M5-32（切换单测）**：本卡已带 31 条（含 6 条真库）。可继续加的是「跨多个期间的切换序列」
  （10 月 JOB → 12 月 VARIETY → 次年 2 月 STANDARD 三段，逐段验口径与结算）、并发双请求、
  以及「切换后红冲再重算」链路 —— 这些都是**叠加**，不需要改本卡代码。
- **前端（`t_11fe2da1`）**：两个接口的契约、字段含义、判色只用 `engine_ready` / `history_untouched`
  的要求已写进卡片；空库实例要先打开 `erp.cost.enabled` 再重启。
- **二期分段（D-16 / 三期）**：真要「期内切换」，改的是 `build_plan` 的护栏 1 与 `cost_calc_period`
  的取方法方式（按段算），本卡把期间边界函数（`month_bounds` / `next_month_start` / `shift_month_start`）
  抽成纯函数就是为那一步留下的。

## 八、发现的既有问题（不是本卡引入，只报不改）

1. **`cost_standard_publish_db_tests` 的夹具账号不清场**：它用 `test_seed::ensure_user` 建了
   `m510.cost-maker` / `m510.finance-manager` 并挂到 `FINANCE_MANAGER`，而
   `cost_period_approval_db_tests::role_tasks` 的「夹具污染」守卫只白名单 `demo_*` 与自己那条夹具
   ⇒ **同一个库上第二次跑成本域套件会红 5 条**（第一次 1497 绿、第二次 1492/5）。
   证据：本卡首次跑域回归时命中；清掉两个 `m510.*` 账号后同一命令恢复 1497 绿。
   已另开卡给测试岗跟进（守卫白名单或夹具 teardown，二选一）。
2. **`66112` 原先没进 66xxx 码表稳定性用例**（`erp_error_codes_are_stable` 的期望列表止于 `66111`）。
   本卡补 `66113` 时顺手把 `66112` 也补进表（码值一个没改）。
