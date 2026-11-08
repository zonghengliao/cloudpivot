# TST-02b 证据：测试自带夹具（A 方案），空库也能全绿

- 卡：`t_9eaa18c9`（父卡 TST-02 `t_8e87e0e6`，B 方案＝前置体检）
- 代码：`backend/src/shared/test_helpers.rs`（A 方案夹具段）+ 6 个真库套件
- 迁移：352（`V0352__attachment_uploader_name`）
- 时间：2026-11-09 04:40–05:20 CST

## 1. 改了哪些文件

| 文件 | 改动 |
|---|---|
| `backend/src/shared/test_helpers.rs` | 新增 A 方案夹具：`fixture_uuid`（SHA-256 派生确定性 UUID）、`ensure_dept` / `ensure_any_dept` / `ensure_any_user` / `ensure_fixture_user` / `ensure_dept_user` / `ensure_two_users` / `bind_role` / `ensure_role_member` / `ensure_approver_roles`、`purge_user` / `purge_dept` / `purge_fixture`；`fixture_uuid_is_deterministic` 单测 |
| `backend/src/business/erp/asset_model_tests.rs` | `any_dept()` 自建；`any_category()` 缺了 panic；4 处 `else { return; }` → 真跑 |
| `backend/src/business/erp/budget_model_tests.rs` | `any_dept()` / `expense_account()` 自建或 panic；`material_type_mapping_roundtrip` 的「第二个部门」补齐 `TST-DEPT-2`（部门覆盖子断言不再跳过） |
| `backend/src/business/erp/expense_model_tests.rs` | `any_dept()` / `any_user()` / `travel_type()` 自建或 panic；16 处 `else { return; }` → 真跑；`some_depts(2)` 不足就补齐 |
| `backend/src/business/erp/asset_linkage_tests.rs` | setup 改为「缺部门就造」；`any_dept()` 走共享夹具 |
| `backend/src/business/erp/expense_linkage_tests.rs` | `require_seed()` → `ensure_fixtures()`（建报销人 + 六个岗位缺才补）；`reporter()` 不再返回 `Option` |
| `backend/src/business/erp/voucher_linkage_tests.rs` | `two_users()` 用 `ensure_two_users()`；5 处 `else { return; }` → 真跑 |
| `backend/src/business/erp/depreciation_linkage_tests.rs` | 原来的「自己插 `LNK10-DEPT`」收敛到 `ensure_any_dept()`（固定 UUID / 幂等） |
| `docs/测试库口径-全量套件.md` | 新增 §9（A 方案口径 + 实测） |

## 2. 一次性空库（`NO_DEMO_SEED=1`）

```bash
ERP_DB=cloudpivot_test_tst02b_empty NO_DEMO_SEED=1 bash scripts/verify/make_full_suite_db.sh
# 建完自检：schema_migrations=285 / 版本=352 / biz_account=66 / biz_flow_def(PUBLISHED)=18
# 夹具前置实测：sys_dept=0  sys_user=1(admin)  sys_user_role=1
```

连跑两轮（每轮 7 个套件，`--test-threads=1 --nocapture`）：

```text
round1 asset_model_tests           ok  4 passed; 0 failed; SKIP=0
round1 budget_model_tests          ok  7 passed; 0 failed; SKIP=0
round1 expense_model_tests         ok 20 passed; 0 failed; SKIP=0
round1 asset_linkage_tests         ok 11 passed; 0 failed; SKIP=0
round1 expense_linkage_tests       ok  6 passed; 0 failed; SKIP=0
round1 voucher_linkage_tests       ok  6 passed; 0 failed; SKIP=0
round1 depreciation_linkage_tests  ok  5 passed; 0 failed; SKIP=0
round2 …（七项同上，全部 ok；SKIP=0）
ALL GREEN (db=cloudpivot_test_tst02b_empty)
```

- 每轮 **59 passed / 0 failed**，`--nocapture` 输出里以 `SKIP` / `[SKIP]` 开头的行 **0**。
- 未使用 `--skip` / `--ignored`。

## 3. 种子库（`make_full_suite_db.sh` 默认口径）

```bash
ERP_DB=cloudpivot_test_tst02b_seed bash scripts/verify/make_full_suite_db.sh
# 自检：sys_user=10 / sys_dept=1(DEPT_DEMO) / 六个关键岗位都有在职成员
```

连跑两轮：同样 **每轮 59 passed / 0 failed / SKIP=0**。

跑完后计数：

```text
dept | 2          # DEPT_DEMO（种子）+ TST-DEPT（预算「部门覆盖」用例需要第二个部门）
user | 10         # 与种子一致
tst02b.* 用户 | 0  # 种子库上夹具「缺才补」⇒ 一行没加，断言基线不漂移
```

## 4. 幂等性（固定 UUID，不随轮数增长）

空库连跑 4 轮后：

```text
TST-DEPT 系列部门 | 2   （TST-DEPT / TST-DEPT-2，4 轮不变）
tst02b.* 用户     | 7   （报销人 1 + 六个岗位成员 6）
岗位绑定           | 6
```

`fixture_uuid("dept:TST-DEPT")` 由 SHA-256 派生、版本位固定 8（单测
`shared::test_helpers::fixture_tests::fixture_uuid_is_deterministic` 守着）；
插入一律 `ON CONFLICT DO NOTHING`，重复跑落到同一行。

## 5. 「静默 pass」清点

被改成真跑的 22 处：

- `asset_model_tests` 4 处、`budget_model_tests` 1 处、`expense_model_tests` 16 处、`voucher_linkage_tests` 5 处
  （其中 `budget` 另有一个「第二个部门就跳过」子断言、`expense_model` 另有一个 `some_depts(2)` 的 SKIP）。

全仓复查结论：剩余的 `let Some(_guard) = setup().await else { return; }` 全部是「没设 `ERP_DB_URL`」
的合法 SKIP（各 `setup()` 只有 `!ensure_db()` 这一条返回 `None` 的路径）。另有两处
`period_remind_tests` 的「该环境没有持有过账权限的角色」SKIP，与部门/用户种子无关，不在本卡口径内。

## 6. fmt / clippy

```bash
rustfmt --edition 2024 --check <本卡改的 8 个 .rs 文件>   # 无 diff
cargo clippy --lib --tests                                # 本卡 8 个文件 0 警告
```

> 全仓 `cargo clippy -- -D warnings` 另有 3 处**非本卡文件**的既有报错
> （`receivable.rs` doc_lazy_continuation、`sales_order_model.rs` / `invoice.rs` too_many_arguments），
> 不在本卡范围内。

## 7. 复现脚本

`~/.hermes/profiles/backend-developer/cache/scratch/tst02b/run_a_suites.sh`（两轮 + SKIP 计数），
日志在 `.../scratch/tst02b/{fresh-empty,seed}/`。

## 8. 种子库全量 lib 回归（附加验证，非本卡判据）

同一份代码在 `cloudpivot_test_tst02b_seed` 上跑**全量** `cargo test --lib`：

```text
test result: FAILED. 3848 passed; 4 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1946.55s
```

4 条 red **全部不在本卡覆盖的 7 个套件里**，且都是**既有问题**（本卡提交前就存在）：

| 用例 | 真因 | 归属 |
|---|---|---|
| `report_pdf::tests::pdf07_real_three_reports_agree_on_shape` | 依赖「库里没有 2026-10 凭证」；全量串行跑到它时库里已有别的套件留下的 2026-03/08/09/10 凭证（期初列不是 `-`）；一次性空库/单跑是绿的 | 整库口径残留（`baseline-red-triage.md` 第 2 类），非本卡 |
| `return_pdf::db_tests::purchase_return_context_has_source_numbers_and_reason` | 断言 `<th>价税合计</th>`，模板实际输出 `<th class="c-with-tax">价税合计</th>`（`pdf/templates/return_doc.html.tera:230`）；断言与模板漂移，与数据/库无关，任何库单跑都红 | 既有：`return_pdf.rs` 与模板都不在本卡 diff 内 |
| `return_pdf::db_tests::sales_return_context_has_source_numbers_and_reason` | 同上（`<th>序列号</th>` vs 带 class 的表头） | 同上 |
| `system::menu::permission_audit_tests::n16_every_route_has_permission_gate_or_documented_exception` | `GET /business/erp/master-data/code-policy` 没有 `#[permission]` 也不在白名单（CODE-08 路由） | 既有：`code_policy.rs` 不在本卡 diff 内 |

（`return_pdf` 两条与 `n16` 在空库单跑同样红，`pdf07` 在空库单跑绿 —— 佐证它们与 A 方案夹具无关。）
