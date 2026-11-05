# TEST-ISO-2：`cargo test` 恒定假红收口（两条点名 + 实测新暴露的两类）

> 库指纹（所有实测共用）：`cloudpivot_test_iso2` / `cloudpivot_iso2_inv`
> `schema_migrations(max|count) = 251|193`，超管人数 = 1，
> 由 `scripts/verify/erp_clean_db.sh` 建库（起一次实例跑完迁移 + bootstrap）。
> 环境变量：`ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<db>` + `NIK_PROFILE=test`
> + 随机 `NIK_MIDDLEWARE_AUTH_JWT_SECRET` / `CLOUDPIVOT_STORAGE_MASTER_KEY`。

## 0. 一句话结论

| 项 | 状态 |
|---|---|
| ① `cash_flow::tests::r3_real_config_covers_all_accounts` | **已不成立**（HEAD 的夹具已按 A 方案自清 + 共用一把锁），本卡实测 `ok` |
| ② `period_items::tests::can_close_is_unaffected_by_items` | **已不成立**（HEAD 已改用例独占期次 2040-MM + 按 id 改状态），本卡实测 `ok` |
| ③ 实测新暴露：`transfer_handler::db_tests` **52 条**全红 | 已修（夹具自愈写死 v1 → 改读 `is_current`），56/56 绿 |
| ④ 实测新暴露：`payable_bridge::tests` **3 条**红（会签/制单人留痕） | 已修（复核人只挂 SUPER_ADMIN → 追加种子节点绑定的全部 ROLE），23/23 绿 |
| ⑤ 实测新暴露：`testd::storage::zz_no_fixture_residue_after_full_suite` 假红 | 已修（并行窗口 → 收敛式判定） |

③④ 的根因是同一个：**V0251（P0-FIX-2B-B 流程去 ADMIN 化）把种子审批人来源从
`ADMIN`（运行时展开全部超管）改成业务角色并把 v1 归档**，而三处夹具仍按旧口径写死。

## 1. 两条点名的假红：根因与现状（本卡没有再改它们）

### ① `business::erp::cash_flow::tests::r3_real_config_covers_all_accounts`

- **根因**：`erp::aux_tests` 的夹具在 `biz_account` 建了 `P21AUXA` / `P21AUXB`
  两个测试科目，但没给它们建 `biz_cashflow_item` 分类行；而 r3 这条断言是
  **遍历 `biz_account` 全表**的强断言（每个非现金科目都必须有分类行）。
  两个模块并行跑 ⇒ 撞出「分类配置漏科目：[P21AUXA …, P21AUXB …]」。
- **HEAD 现状（本卡实测有效）**：两条都落到了 A 方案
  - `aux_tests` 的每个真库用例都走 `case()` 外壳：`catch_unwind` → **无条件 `cleanup()`**
    （正常返回和 panic 两条路径都清），不再依赖 `zz_` 名字排序；
  - 所有会插 `biz_account` 的模块（`aux_tests` / `model_tests` / `pflow3b_tests` /
    `voucher` / `voucher_query`）统一 `use ...::voucher_query::tests::DB_LOCK`，
    与 `cash_flow::tests::setup_fixture()` **同一把锁** ⇒ 全表断言与夹具**互斥**。
- **为什么选 A 而不是 B（排除测试前缀）**：这条断言检查的是**生产配置的完整性**
  （「每个科目都要有现金流分类行」）。B 等于把「测试前缀 P21」写进生产断言，
  从此真有一个 `P21…` 开头的科目漏配分类也查不出来 —— 治标且破坏断言的语义。
  A 让夹具**不留下痕迹**，断言一个字不改，强度不变，也不依赖用例名排序。
  （本卡按卡面要求**没有动** `r3_real_config_covers_all_accounts` 这一条。）

### ② `business::erp::period_items::tests::can_close_is_unaffected_by_items`

- **根因**：用例过去按「把 `2033-10` **整期** UPDATE 成 POSTED」推进，同期间的
  别的用例（`period::tests` / `period_remind_tests` / `stocktake_tests`）并行时
  互踩：它把别人的草稿凭证一起过账，别人的「必须有草稿挡路」断言随机红；
  反过来别人的清理也会把它刚种的凭证删掉。
- **HEAD 现状（本卡实测有效）**：A 方案 + 一条更硬的隔离
  - `period_items` 改用**用例独占期次** `2040-MM`（`support::open_case_period(9)`），
    同进程内重复申请同一序号直接 `panic`（把「两条用例撞同一期次」从约定变成运行期门禁）；
  - 写操作只按 id：`support::set_voucher_status(voucher_id, "POSTED")`（`UPDATE … WHERE id = ?`），
    不再出现整期 UPDATE；
  - `period_test_support::DB_LOCK` 与 `period::tests` / `stocktake_tests` 共用同一把锁。
- **为什么选 A 而不是 B/C**：本项目**没有 per-module DB 机制**（全仓统一
  `ERP_DB_URL` 单库，`db()` 是进程级全局连接池），B 不存在基础设施；C 的事务隔离
  也不成立 —— 引擎的 `close_check` / 过账自己按需开事务并走全局池，测试侧再包一层
  事务只会和引擎的事务打架（且 `#[tokio::test]` 各自独立 runtime，共享连接池）。
  A（期次独占 + 按 id 写）最简单、最稳，也符合本仓「写操作只按 id 集合」的改造规范。
- **没有**用 `--test-threads=1` 绕过：所有实测都是默认线程数下的并行跑。

## 2. 本卡改了什么（commit `78a8a8f`）

| 文件 | 位置 | 改动 |
|---|---|---|
| `backend/src/business/erp/transfer_handler.rs` | `db_tests::ensure_seed_flow()` | 自愈不再把 `current_version_id` 硬写回 v1，改为读库里 `is_current`（号最大）的版本；库里连 `is_current` 行都没有才退回 v1 兜底；返回值改成**真实的当前版本 id**（原来是写死的 `VER_ID`） |
| 同上 | `db_tests::seed_flow_is_published_with_self_consistent_hash` | 种子审批节点断言 `assignee_type == "ADMIN"` → `"ROLE"`（V0251 口径），并**补钉** `empty_assignee == "FALLBACK_ADMIN"`（角色无人必须兜底到超管） |
| 同上 | `db_tests::submit_creates_pending_todo_for_super_admin` → `…_for_seed_assignee` | 从「待办数 = 超管人数、来源 ADMIN」改为**按节点真实绑定推导**期望收件人与来源（ROLE → 该角色的活账号；角色无人 → FALLBACK_ADMIN 兜底到超管）；换种子自动跟随；其他人出现未知来源类型直接 panic（不静默跳过） |
| `backend/src/business/erp/payable_bridge.rs` | `tests::setup()` | 复核人除 SUPER_ADMIN 外，追加绑定**当前种子版本审批节点实际引用的全部 ROLE**（`biz_flow_node_assignee` → `sys_role`，从库里读）；恢复「每层都有两个能批的人」这一前提（n_board 会签凑得齐、⑰ 的制单人留痕行存在） |
| `backend/tests/testd/storage.rs` | `zz_no_fixture_residue_after_full_suite` | 一次查询断言 → **收敛式判定**（200ms × 15s）：并行时 `zz_` 不保证最后执行，会抓到别人「刚断言完 sys_file 还在、自己的 reset 还没跑」的毫秒级窗口（实测 `sys_file=1`，几十秒后归零）。真泄漏等多久都还在 ⇒ 断言强度不变 |

> 生产代码**一行未改**（未动任何 `migrations/`、handler、model）；三处改动都在测试模块 /
> 测试文件内。V0251 本身的自检（`RAISE EXCEPTION` 五条）与迁移内容无关本卡。

## 3. 实测记录

### 3.1 两条点名用例（在干净库上）

```
test business::erp::cash_flow::tests::r3_real_config_covers_all_accounts ... ok
test business::erp::period_items::tests::can_close_is_unaffected_by_items ... ok
```

### 3.2 全量轮次

| 轮 | 命令 | lib 结果 | 说明 |
|---|---|---|---|
| A | `cargo test -j 2`（17:27:30→17:50:00，库：干净 251/193） | `ok. 2621 passed; 0 failed`（1120.02s） | `testd` 目标 66 passed / 1 failed：`zz_no_fixture_residue_after_full_suite`（`sys_file=1` 瞬时窗口，见 2 表第 5 行） |
| B | `cargo test -j 2`（17:56→18:17） | `FAILED. 2628 passed; 3 failed`（1124.88s） | 3 条 `payable_bridge` 会签/制单人用例：V0251 后 `n_board` 只剩 1 条待办（`合计 1 条`），属旧口径假红，见 2 表第 4 行 |
| C | `cargo test -j 2`（18:20:59→18:44:10，含本卡全部修复） | `ok. 2631 passed; 0 failed`（1124.63s），`testd` 67 passed/0 failed，**EXIT=0** | 全量单目标全绿；详见 `runC-test-result-lines.txt` |

修前/修后，按模块（同一干净库）：

```
business::erp::transfer_handler::db_tests   修前 52 failed → 修后 56 passed; 0 failed（145.44s）
business::erp::payable_bridge::tests        修前  3 failed → 修后 23 passed; 0 failed（ 48.83s）
testd（全目标）                              修前  1 failed → 修后 67 passed; 0 failed（142.29s）
```

### 3.3 不变量脚本（干净库 `cloudpivot_iso2_inv`，迁移 251/193）

```
NIK_DATABASE_NAME=cloudpivot_iso2_inv NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  python3 scripts/verify/erp_invariants.py
结果：65/66 通过   🚨 BLOCKER 0   ⚠️ WARN 1        # EXIT=0
```
唯一 WARN 是 `[权限] N2 admin 无部门归属` —— 干净库 bootstrap 出来的 admin 本来就没落部门
（生产 144 已由 P0-FIX-2B-A 落位 HQ），与本卡无关。

> 注意：**不要在跑完单测的库上跑这个脚本**。单测库里有夹具残留（应付/凭证/库存都有），
> 会报一堆 BLOCKER（本卡实测过一次：B4/B6/D1/E4 等），那是测试残留，不是生产问题。

## 4. 复现命令

```bash
# 1) 建干净库（起一次实例跑迁移 + bootstrap，打印 251|193 与超管人数）
ERP_DB=cloudpivot_test_iso2 RECREATE=1 PORT=18196 bash scripts/verify/erp_clean_db.sh

# 2) 全量（默认线程数，不要 --test-threads=1）
cd backend
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test_iso2' \
NIK_PROFILE=test \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  cargo test -j 2
```

## 5. 遗留 / 建议

- `transfer_handler::db_tests::VER_ID` 仍作为「库里没有 is_current 行」时的兜底常量保留；
  等 v1 从所有环境彻底退场（或 `biz_flow_def_version` 加 `NOT NULL is_current` 唯一约束）后可删。
- 同类「写死种子 id / 写死 ADMIN 展开」的夹具可能还有：本次是靠全量跑撞出来的。
  建议后续把 `grep -rn "assignee_type.*ADMIN\|SUPER_ADMIN" backend/src/**/*_tests.rs` 纳入巡检，
  种子口径一变先跑这一条。
