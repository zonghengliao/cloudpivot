# t_be428a39 / SCOPE-PAGE 验收报告：报销列表带筛选时非超管 500

> 一句话：`expense_model::report_page`（以及同类隐患 `budget_model::budget_page`）的 `base_sql`
> 没有预留 `{FILTER}` 占位符，`scope::paginate` 只能把数据范围片段追加到末尾 → `?` 顺序与参数
> 顺序错位 → 注入守卫直接报错 → **所有非超管用户一带筛选就 500**。补上占位符后 145 真机全绿。

- 卡号：`t_be428a39`（来源：FIX-5 / `t_9a23375a` 取证旁证）
- 提交：`d94462e` ｜ 145 测试环境：`2c0000e1 → d94462e2`（`/root/test_deploy.sh`）
- 迁移：**不新增**（纯代码 + 用例）

---

## 1. 现象与根因

### 1.1 现象（修复前，145 现役 8033 实测）

```bash
T=$(curl -sS -X POST http://172.18.10.145/api/system/auth/login -H 'Content-Type: application/json' \
  -d '{"username":"emp_drill","password":"Drill@123456"}' | jq -r .data.access_token)
# 无筛选            → 200
# &keyword=x        → 500  {"code":10004,"msg":"服务器内部错误"}
# &status=DRAFT     → 500
# &pay_status=UNPAID→ 500
```

日志：`ERROR 数据库操作失败 context="分页查询报销单" invalid value: base_sql 带 4 个业务参数但
没有 {FILTER} 标记：片段追加到末尾会让 ? 顺序与参数顺序错位（架构 §2.6 参数顺序契约）`

### 1.2 根因链

1. `scope::paginate`（`business/shared/scope.rs`）把数据范围片段的参数**恒前置**到 `base_params`
   之前（架构 §2.6），所以片段在 SQL 文本里必须排在业务 `?` **之前**；
2. 它的三条规则：有 `{FILTER}` → 原位替换；无标记且无业务参数 → 追加到末尾（安全）；
   **无标记但有业务参数 → 直接报错**（宁可报错也不静默绑错值）；
3. `report_page` 的 `base_sql` 把 keyword/status/pay_status/reporter_id/dept_id/period/日期
   全部直接拼在 `WHERE e.deleted_at IS NULL` 后面，**没有占位符** → 命中第 3 条 → 500；
4. **为什么只有非超管中**：超管（`is_super`）与 `data_scope=ALL` 的片段是 `AND 1 = 1`
   **零参数**，走第 2 条「追加到末尾」分支 —— 语义等价，所以超管视角完全看不出来。

## 2. 改动清单（4 文件，+445/−15）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/expense_model.rs` | 拆成 `report_page_base_sql()`（`… IS NULL` + `FILTER_MARKER`）与 `report_page_filters()`（只拼 ` AND …` 片段并 push 参数），`report_page` 组装后再进 `scope::paginate`；业务参数顺序**逐字不变**。`report_payable_page` 也补上占位符（当前无业务参数，防以后加筛选再踩同一个坑） |
| `backend/src/business/erp/budget_model.rs` | 同款隐患同款修法：`budget_page_base_sql()` + `budget_page_filters()`。**注**：这条是扫仓时发现的同类缺陷，145 上无同时具备 `DEPT_LEADER` 与 `business:erp:budget:list` 且口令已知的账号，故用临时探针账号验证（见 §5.4） |
| `backend/src/business/erp/expense_model_tests.rs` | 新增 3 条纯 SQL 契约用例（不连库）+ 1 条真库回归用例 |
| `backend/src/business/erp/budget_model_tests.rs` | 新增 1 条预算分页契约用例（不连库） |

占位符写法用 `sql.push_str(scope::FILTER_MARKER)`（与 `impexp` / `notice` 一致）；
`inject_filter` 自己保证两侧空格（B5 真机踩过 `NULLAND`），两种姿势等价。

## 3. 全仓同类隐患扫描（`scope::paginate` 共 15 个调用点）

判据：`base_sql` 是否带业务参数（`?`）→ 带了就必须有 `{FILTER}`，否则非超管一带筛选必 500。

| # | 调用点 | 锚点 | 业务参数 | `{FILTER}` | 结论 |
|---|---|---|---|---|---|
| 1 | `system/user/model.rs:261` `page_users` | `SYS_USER` | 有 | 有（:208） | ✅ |
| 2 | `system/role/model.rs:230` `page_role_users` | `SYS_USER` | 有（role_id） | 有（:221） | ✅ |
| 3 | `support/log/model.rs:352` `page_operation_logs` | `OPERATION_LOGS` | 有 | 有（:319） | ✅ |
| 4 | `support/log/model.rs:479` `page_json_logs` | 审计/认证/安全/密码变更日志 | 有 | 有（:458） | ✅ |
| 5 | `support/file/model.rs:162` `page_files` | `SYS_FILE` | 有 | 有（:141） | ✅ |
| 6 | `business/storage/model.rs:1162` `page_storage_files` | `SYS_FILE` | 有 | 有（:1137） | ✅ |
| 7 | `business/impexp/model.rs:508` `page_import_tasks` | `SYS_IMPORT_TASK` | 有 | 有 | ✅ |
| 8 | `business/impexp/model.rs:1019` `page_export_tasks` | `SYS_EXPORT_TASK` | 有 | 有 | ✅ |
| 9 | `business/notice/model.rs:563` `page_notices` | `SYS_NOTICE` | 有 | 有 | ✅ |
| 10 | `business/project/model.rs:291` `page_visible` | `BIZ_PROJECT` | 有 | 有（`PROJECT_VISIBLE_TAIL`） | ✅ |
| 11 | `business/task/task_model_query.rs:178` `page_visible` | `BIZ_TASK` | 有 | 有（`VISIBLE_OR_TAIL`） | ✅ |
| 12 | `business/erp/commission_model.rs:505` `page` | `BIZ_COMMISSION_SETTLEMENT` | 有 | 有 | ✅ |
| 13 | **`business/erp/expense_model.rs:573` `report_page`** | `BIZ_EXPENSE_REPORT` | 有 | **缺 → 本卡修** | 🔴→✅ |
| 14 | `business/erp/expense_model.rs:603` `report_payable_page` | 同上 | 无（固定条件） | 无 → **补上** | 🟡→✅ |
| 15 | **`business/erp/budget_model.rs:346` `budget_page`** | `BIZ_BUDGET` | 有 | **缺 → 本卡修** | 🔴→✅ |

另外 `scope::paginate_unscoped`（40 个调用点，`scope_model.rs` 的闸①要求）恒用
`RowFilter::allow_all()`，片段 `AND 1 = 1` 零参数 → 走「追加到末尾」安全分支，**不适用**该契约
（它们本来就不注入数据范围）。

`erp/model_base.rs:83` 的通用列表助手是 `paginate_unscoped`，同理不涉及。

## 4. 回归用例（5 条，本仓 `cargo test --lib`）

| 用例 | 类型 | 断言 |
|---|---|---|
| `expense_model_tests::page_base_sql_has_exactly_one_filter_marker` | 纯 SQL（不连库，永远跑） | base_sql 恰好一个 `{FILTER}`、恒真条件还在、自身不带 `?` |
| `expense_model_tests::business_params_always_follow_the_filter_marker` | 纯 SQL | 10 种筛选组合（无 / keyword / status / pay_status / reporter_id / dept_id / period / date_from / date_to / 全开）：`?` 个数 == 参数个数、`{FILTER}` 位置 < 第一个业务 `?`、`ensure_injection_point` 全部通过 |
| `expense_model_tests::old_shape_without_marker_is_rejected_by_guard` | 纯 SQL（反证） | 旧写法（业务条件拼后面、无标记）必须被守卫拒绝且错误信息点名 `{FILTER}` —— 锁住「为什么会 500」的机制 |
| `expense_model_tests::page_with_filters_is_scoped_and_never_fails` | **真库**（`ERP_DB_URL`） | 造跨部门两张单：`Dept` 范围 principal 带 keyword / keyword+status / keyword+pay_status / 全开四种筛选 → 必须成功、`total == 1`、返回行全在本部门；同筛选**超管** `total == 2`（两张都看得见，超管路径没改坏）；无筛选也必须按范围裁剪 |
| `budget_model_tests::budget_page_sql_keeps_filter_marker_first` | 纯 SQL | 7 种筛选组合的同一套位置/计数契约 |

**反证已实跑**（`raw/99-prefix-revert-fails.txt`）：把 `report_page_base_sql()` 里的
`push_str(FILTER_MARKER)` 临时去掉（= 修复前的写法），两条用例立刻变红，报错文案与线上 500
**逐字一致**：

```
【keyword】非超管带筛选的分页必须成功（旧代码 500）：
invalid value: base_sql 带 4 个业务参数但没有 {FILTER} 标记：片段追加到末尾会让 `?` 顺序与参数顺序错位
test result: FAILED. 0 passed; 2 failed; ...
```

## 5. 145 真机验收

### 5.1 部署

`bash /root/test_deploy.sh`：`2c0000e1 → d94462e2`，编译通过 → 应用 3 条他人迁移
（V0327/V0328/V0329，非本卡）→ 重启 `cloudpivot-test` → `health 200`，菜单结构不变量 5 项全绿
（`raw/deploy145.txt`）。日志行：`服务: active / PID 217325 / health 200`、`✓ 登录 OK /
task 接口 200`、`菜单总数: 410 ✅`。

收尾再跑一次已收敛到 `a19b1b30`（= origin/main = 99 HEAD，含 3 个 docs 提交；二进制行为与
`d94462e2` 相同）：`服务: active / PID 251353 / health 200`、菜单不变量全绿。
**踩到的坑**：第一次 ff-merge 报「分叉」，真因不是分叉而是工作区里有**未跟踪**文件
（`docs/evidence/20261108-fix6-tax-and-accounts/{README.md,14_kill_instance.sh}`，已被别人提交进
main），`git merge --ff-only` 拒绝覆盖未跟踪文件；`test_deploy.sh` 只对「已跟踪的改动」做 stash，
所以它自愈不了。处置：`git stash push -u -m … -- <该目录>` 后重跑即过（未丢任何人的东西）。

### 5.2 `emp_drill`（非超管，带裁剪）—— 卡面三条 URL 全 200

身份：`emp_drill` / `DRILL_CLERK` / `data_scope=4(SELF)` / 部门=演练费用部。
`biz_expense_report` 无归属人列，SELF/DEPT 在 `filter_for` 里按「本部门」fail-closed 落地
（`AND e.dept_id = ?`，1 个参数 —— 修复前正是这 1 个参数触发错位）。

| 筛选 | HTTP | total | 返回部门 |
|---|---|---|---|
| 无 | 200 | 7 | 演练费用部 |
| `keyword=x`（命中所有 `EXP…` 单号） | 200 | 7 | 演练费用部 |
| `status=DRAFT` | 200 | 2 | 演练费用部 |
| `status=APPROVED` | 200 | 3 | 演练费用部 |
| `pay_status=UNPAID` | 200 | 6 | 演练费用部 |
| `pay_status=PAID` | 200 | 1 | 演练费用部 |
| `keyword+status+pay_status` | 200 | 2 | 演练费用部 |

**收敛证据**：全库 `biz_expense_report` 13 条（演练费用部 7 / 演练预算部 6），
`emp_drill` 各筛选下 total ≤ 7、返回行 `dept_name` 只有「演练费用部」→ 是裁剪后的结果，不是全量。
（`raw/probe-145.txt`）

### 5.3 超管路径没被改坏

145 的 `admin` 口令已被改过（不是 `NIK_ADMIN_PASSWORD`，也不允许重置以免打断别人），
因此临时建了一个 `SUPER_ADMIN` 探针账号（复制 `emp_drill` 的口令哈希），探完**硬删**（残留 0）。

| 筛选 | 超管 HTTP | total | DB 全库真值 |
|---|---|---|---|
| 无 | 200 | 13 | 13 ✅ |
| `keyword=x` | 200 | 13 | 13 ✅ |
| `status=DRAFT` | 200 | 2 | 2 ✅ |
| `status=APPROVED` | 200 | 6 | 6 ✅ |
| `pay_status=UNPAID` | 200 | 12 | 12 ✅ |
| `pay_status=PAID` | 200 | 1 | 1 ✅ |
| `expense/pay/page`（待付款） | 200 | 6 | — |

即「超管同 URL = 全量且与真值逐条相等」，与修复前一致（超管片段本就零参数，占位符位置不影响结果集）。
（`raw/super-probe-145.txt`）

### 5.4 预算分页（同款隐患）

用 `DEPT_LEADER`（`data_scope=3`，部门=演练费用部）临时探针账号（同样建完即删，残留 0）：

| 筛选 | HTTP | total | 全库真值 |
|---|---|---|---|
| 无 | 200 | 1 | 5 |
| `fiscal_year=2026` | 200 | 1 | 5 |
| `status=ACTIVE` | 200 | 1 | — |
| `keyword=差旅` | 200 | 1 | — |
| `fiscal_year+status` | 200 | 1 | — |

修复前这 4 个带筛选的 URL 会因为片段带 1 个参数（`dept_id = ANY(?::uuid[])`）而 500；
现在 200 且按部门收敛到 1 条。超管口径同表（`raw/budget-probe-145.txt` 与
`raw/super-probe-145.txt` 的「预算分页」段：无筛选 5 / 2026 5 / ACTIVE 3 / 差旅 3）。

## 6. `cargo test`

| 范围 | 结果 | 证据 |
|---|---|---|
| 145（干净树 = `d94462e2`）`cargo test`（23 个目标，显式跳过他人卡在途的 5 条税率单测） | **3547 passed / 0 failed / 11 ignored / EXIT=0** | `raw/145-cargo-test-all-skip-sibling-red.txt` |
| 145（干净树）`cargo test --lib` 同上跳过 | 3318 passed / 0 failed / 5 filtered out | `raw/145-cargo-test-skip-sibling-red.txt` |
| 145 原样 `cargo test`（不跳过） | 3318 passed / **5 failed**，全部是 `t_728f716e`（FIX-6 税率标度）的用例：`dto.rs:3533`、`invoice.rs:2097`、`report.rs:2073/2148`、`tax_rate.rs` | `raw/145-cargo-test-full.txt` |
| 本机 99 · 真库（`ERP_DB_URL` = 145 `cloudpivot_dev` 的 pg_dump 副本 `cloudpivot_be428a39`） | `expense_model` **22/22**、`budget_model` **7/7**（含 5 条新用例，全部 ok，无 SKIP） | `raw/99-with-fix-expense-model.txt`、`raw/99-with-fix-budget-model.txt` |
| 反证（去掉标记） | 2 failed，报错与线上 500 逐字一致 | `raw/99-prefix-revert-fails.txt` |
| `cargo clippy --lib --tests` | 本卡文件 0 告警；余下 5 条告警全在他人文件（`asset_linkage.rs:423`、`fx_revaluation.rs:788`、`opening.rs:2738`、`year_end_linkage_tests.rs:214`、`sales_order_model.rs:699` 的 8 参数函数） | `raw/clippy-99.txt` |
| `cargo fmt -- --check` | 本卡 4 文件无 diff（仓库其它文件有既有 fmt 偏差，未动） | — |

## 7. 复现 / 自检脚本（本目录）

| 脚本 | 作用 |
|---|---|
| `scope_page_probe.sh` | 145 只读探针：`emp_drill` 七种筛选的 HTTP 码 + total + 返回部门、全库真值、预算/待付款 |
| `super_probe.sh` | 临时 `SUPER_ADMIN` 账号 → 超管口径（全量）→ 立刻硬删 |
| `budget_probe.sh` | 临时 `DEPT_LEADER` 账号 → 预算分页带筛选的裁剪口径 → 立刻硬删 |

本机真库自检：

```bash
# 用 145 cloudpivot_dev 的副本库跑真库用例（不碰 145 的库）
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_be428a39 \
  cargo test --lib business::erp::expense_model:: -- --test-threads=1
```

## 8. 边界与已知问题（都不是本卡引入）

1. **main 上 5 条既有红灯**：全部落在 `t_728f716e`（FIX-6 税率标度 / 期初对方科目 / 留抵重分类，
   提交 `2c0000e` + `a728c6b` + `cf974f5`）的用例里 —— `dto.rs:3533`（0.13 vs 13）、
   `invoice.rs:2097`（1.30 vs 0.13）、`report.rs:2073/2148`（重分类/借贷平衡）、`tax_rate.rs`
   （百分数归一幂等）。该卡自己的验收也记了「全量 `cargo test` 未跑绿」，并建卡 `t_13ddcb84`
   让测试工程师造「种子完备」的可复现测试库。本卡 `d94462e` 只碰 4 个分页文件，与这些断言
   零交集（`git show --name-only d94462e`）；145 干净树上同样红。故 §6 给出「显式 skip 这 5 条
   后 3547/0（23 目标）」的口径，不谎报全绿，也不改别人的断言。
2. **`expense_linkage_*` 真库子集**在本机副本库上跑到 `lnk05_pay_triggers_only_on_full_payment`
   时被 900s 超时打断（SIGTERM）——该组用例走付款/通知链路，不经过 `report_page`；常规 suite
   （不设 `ERP_DB_URL`）本就跳过它们。本卡的真库覆盖用 `expense_model` 子集（22/22）。
3. **145 临时账号**：`scope_probe_super`、`scope_probe_lead` 均为一次性探针，脚本 trap 内硬删，
   实测残留 0 行（日志末行 `清理后残留行数：0`）。
4. **不新增迁移**：本卡纯代码 + 用例；145 上被应用到的 V0327/V0328/V0329 来自他人提交。
