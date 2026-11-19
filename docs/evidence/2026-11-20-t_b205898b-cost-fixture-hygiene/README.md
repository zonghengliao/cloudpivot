# 成本域真库套件「第二遍红 5 条」收口：m510.* 夹具账号未进污染守卫白名单（卡 `t_b205898b`）

> 结论先说：**同一个新种子库上连跑两遍成本域套件，两遍都是 1497 passed / 0 failed**（第二遍带着第一遍
> 留下的常驻夹具）。修的是**测试夹具判据**，业务代码零改动；守卫的**判据强度没有放宽**，并新增
> 三条**反样本**证明它仍然有牙。

## 一、验收结论（真库，可复现）

| 项 | 结果 |
|---|---|
| 库 | `cloudpivot_test_b205898b`（`scripts/verify/make_full_suite_db.sh` 先删后建：401 迁移 / V0472 / 203 表 / `FINANCE_MANAGER` 在职 2 人） |
| 被测树 | `/opt/cloudpivot`，HEAD `f08bcda0`（M5-31 成本方法切换）+ 本卡 3 个测试文件的未提交改动 |
| 第 1 遍 | `running 1497 tests` → **1497 passed / 0 failed**（12.8–13.3 s） |
| 第 2 遍 | `running 1497 tests` → **1497 passed / 0 failed**（12.9–13.3 s） |
| 两遍差异 | 除 `finished in x.xxs` 外逐字节相同（`diff` 已核） |
| 历史 5 条红 | `submit_starts_instance_and_freezes_batch` / `two_levels_then_approved_freezes_batch` / `reject_returns_batch_to_draft_and_clears_traces` / `withdraw_terminates_instance_and_returns_draft` / `withdraw_self_heals_when_callback_failed` —— 第 2 遍全部 `ok` |
| 门禁 | `cargo clippy --all-targets -- -D warnings` **exit=0**；`rustfmt --edition 2024 --check` 三个文件 clean |

证据文件：

| 文件 | 内容 |
|---|---|
| `run1.log.txt` / `run2.log.txt` | 两遍完整输出（`run1.start/.end`、`run2.start/.end` 为墙钟） |
| `compile.log.txt` | 测试二进制编译记录（最终代码态） |
| `pre-run-role-members.txt` | **跑之前**库里的状态：`FINANCE_MANAGER → demo_finance,demo_finance_mgr`，无任何 `m510*` / `fixture.*` |
| `after-run1-role-members.txt` | 第一遍留下的残留：`fixture.m510.finance-manager` 已在岗位上（**这正是过去第二遍红的触发条件**） |
| `probe-a-stranger.log.txt` / `probe-a-verdict.txt` | 负样本：无前缀临时账号 → 守卫**红**（点名 `temp_probe_stranger`），且红的正是那 5 条 |
| `probe-b-legacy.log.txt` | 向后兼容：历史前缀 `m510.finance-manager`（派生 id）→ 守卫**绿**（7 passed） |
| `db-audit-after-two-passes.txt` | 库侧独立复查（不经测试代码） |
| `test-seed-unit.log.txt` | 新增判据单测：5 passed / 0 failed / 1 ignored（既有的真库耗时基准） |
| `clippy-all-targets.log.txt` | `--all-targets -D warnings` 干净 |

复现命令：

```bash
cd /opt/cloudpivot
ERP_DB=cloudpivot_test_b205898b PORT=18079 bash scripts/verify/make_full_suite_db.sh
export ERP_DB_URL='postgres://cloudpivot:<口令>@172.18.10.145:5432/cloudpivot_test_b205898b'
export SYS_DB_URL="$ERP_DB_URL" TASK_DB_URL="$ERP_DB_URL" P0FIX_DB_URL="$ERP_DB_URL"
cd backend/app
cargo test -p cloudpivot --lib business::erp::cost:: -- --test-threads=1   # 第 1 遍
cargo test -p cloudpivot --lib business::erp::cost:: -- --test-threads=1   # 第 2 遍（带第 1 遍残留）
```

## 二、根因（与卡面一致，已在代码里复核）

1. `cost_standard_publish_db_tests.rs` 用 `test_seed::ensure_user` 建两个**常驻**账号（幂等 + `OnceCell`
   缓存 id），其中 `m510.finance-manager` 被 `bind_role` 挂进共享角色 `FINANCE_MANAGER`（发布审批夹具
   需要一个**非制单人**审批）；
2. `cost_period_approval_db_tests.rs::role_tasks` 的「夹具污染」守卫白名单**只有 `demo_*` 前缀**，
   于是第二遍把上一遍正常留下的 `m510.finance-manager` 判成污染 ⇒ 同一条守卫把 5 条用例一起打红。

关键点：**不是断言太严**（它要抓的是「别的用例把角色绑给临时账号却没清场」），而是「**夹具身份**」
过去没有单一判据 —— 每个卡各自起前缀（`m510.` / `m214.` / `hr12.` / `tst02b.`），只有造它的那个文件
知道它是夹具。

## 三、修法（选卡面方案 1「收窄到已知夹具前缀」，并把它变成单一判据）

方案 2（让 `cost_standard_publish` teardown 清账号）被否掉，理由两条（都在代码里可验证）：

- 该模块内部 `OnceCell` 缓存账号 id，**同模块后续用例还要用**；逐例 teardown 会自断后路；
- 「模块末尾清」没有可靠挂点 —— 最后一个用例的残留不会被任何「下一个 setup」收走（§23 的老教训）。

改动 3 个文件（**只动测试代码**，`git diff --numstat`：+111 / −13）：

| 文件 | 改动 |
|---|---|
| `app/src/test_seed.rs` | 新增 `FIXTURE_USER_PREFIX = "fixture."` / `DEMO_USER_PREFIX` / `fixture_name(tag)` / `is_fixture_account(id, username)`；新增 1 条带**反样本**的单测 |
| `app/src/business/erp/cost/cost_standard_publish_db_tests.rs` | 夹具账号改用 `test_seed::fixture_name("m510.cost-maker"/"m510.finance-manager")` ⇒ 登录名 `fixture.m510.*`（id 仍由登录名派生） |
| `app/src/business/erp/cost/cost_period_approval_db_tests.rs` | 守卫的「陌生人」判据从手写 `demo_*` 改成调 `test_seed::is_fixture_account`（**判据只此一份**），断言期望值与逐项对账逻辑**一字未改** |

`is_fixture_account` 的三条判据（任一成立）：

1. 登录名以 `demo_` 开头 —— 产品演示种子（`make_full_suite_db.sh` 种的那批）；
2. 登录名以 `fixture.` 开头 —— 本仓夹具约定（新增夹具一律 `fixture_name("<卡号>.<用途>")`）；
3. `id == fixture_uuid("user:<登录名>")` —— `test_seed::ensure_user` 建的账号 id 是**登录名派生**的
   确定性 UUID，所以「名字没按约定起」的历史夹具（`m510.*` / `tst02b.*`）也认得出来 ⇒ **老库无需迁移**
   （探针 B 实测：历史前缀 + 派生 id 仍然绿）。

判据**没有**放宽成「非 `demo_` 都算夹具」：临时账号（手写 `INSERT` + 随机 id + 无前缀）照样被判污染 ——
探针 A 就是这么证明的。

## 四、守卫仍有牙（三条反向证据，这次是重点）

1. **探针 A（负样本）**：手写 `INSERT` 一个 `gen_random_uuid()` id、无前缀的 `temp_probe_stranger` 并绑
   `FINANCE_MANAGER` ⇒ 守卫**红**，且红的正好是卡面那 5 条：
   `FAILED. 2 passed; 5 failed`，panic 文案点名 `["temp_probe_stranger"]`。
   探针造完先**硬断言**「账号确实在岗位上（count=1）」才跑，避免空跑假绿。
2. **探针 B（向后兼容）**：用历史登录名 `m510.finance-manager` + 派生 id 造行并绑定 ⇒ `7 passed / 0 failed`。
   这条钉住「老库/老账号不受影响」，也说明判据 ③ 真在起作用。
3. **单测反样本**：`test_seed::tests::is_fixture_account_recognises_known_prefixes_and_rejects_strangers`
   —— 三条正样本 + 一条「名字无前缀且 id 非派生的临时账号必须**不**算夹具」。

探针跑完都清场，库里清零（`探针残留账号数: 0`）。

## 五、库侧独立复查（`db-audit-after-two-passes.txt`，不经测试代码）

```
fixture.* 账号 = 2 行（fixture.m510.cost-maker / fixture.m510.finance-manager，status=1，未删）
m510.* 账号   = 0（新库不产生历史前缀；老库靠派生 id 兼容）
FINANCE_MANAGER → demo_finance, demo_finance_mgr, fixture.m510.finance-manager
b3a3* 账号 / 角色绑定 = 0 / 0（cost_period_approval 自己的 zz_teardown 收干了）
COST_PERIOD 孤儿日志 = 0，COST_PERIOD 日志总数 = 0，biz_cost_period = 0
COST_SETTLE / COST_STANDARD_PUBLISH 实例 = 0 / 0，biz_flow_task PENDING = 0
```

## 六、踩过的三个坑（留给后来者）

1. **`clippy::doc_lazy_continuation` 会拦全仓 pre-push**：我第一版守卫注释里用了 `<br>` 折段 + 缩进续行，
   `cargo clippy --all-targets -- -D warnings` 直接报 3 条（`doc list item without indentation`），
   把别人的推送也拦了（M5-31 被拦两次）。规则：编号列表里的续行必须**紧跟**列表项（4 空格缩进、中间不留空行），
   或者整段改成不带缩进的独立段落。
2. **`ON CONFLICT (username)` 在 `sys_user` 上不成立**：`username` 是**部分唯一索引**
   （`WHERE deleted_at IS NULL`），`ON CONFLICT (username) DO NOTHING` 会整条 INSERT 报错
   ⇒ 我的第一版探针**账号根本没建出来**，「守卫绿」是空跑出来的假绿。改用 `WHERE NOT EXISTS` +
   插入后断言行数/绑定数。
3. **反样本自己写错了**：第一版反样本用 `fixture_uuid("user:temp_approver_leaked")` 造 id 配同名登录名 ——
   这正好命中判据 ③，用例正确地红了（红的是样本，不是判据）。改成与登录名无关的字面 uuid。

## 七、边界与后续

- 本卡**没改**任何业务代码，也**没改**任何断言的期望值；`role_tasks` 的逐项对账（待办集合 == 角色在职成员）
  原样保留。
- 新增夹具请走 `test_seed::fixture_name("<卡号>.<用途>")`；已存在的老前缀账号不需要改（靠派生 id 兼容），
  但**新增**的账号若继续自造前缀，将来任何通用守卫都无法识别 —— 这是本次要立住的约定。
- 本卡只跑了成本域（1497 条 × 2 遍）。全量套件的红集合会随别卡改动漂移，不在本卡判定范围内；
  `test_seed::` 域归属已在 `backend/test-domains.tsv` 的 `platform` 行登记（无需改表）。
