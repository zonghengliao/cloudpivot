# HR-47 遗留：`GET /business/hr/employee/import-template` 未登记 `ROUTE_CONTRACT`（卡 `t_575aced2`）

**结论：`business::hr` 整模块在 145 演练库（空库全量迁移）上从「672 passed / 1 failed」变成
「673 passed / 0 failed」，SKIP 0、库侧残留 0。** 唯一红是契约表漏登记 HR-47 的模板下载路由，
本次只补这 1 行契约并同步计数口径；`POST /employee/import`（`card: "TBD"`）按卡面要求原样保留。

## 1. 根因

- HR-47（`12d20c7d`）新增 `backend/src/business/hr/employee_import_handler.rs`，其中
  `#[nik::permission(path = "/business/hr/employee/import-template", …)]`
  + `#[nik::get("/business/hr/employee/import-template")]`；
- `handler.rs` 的 `ROUTE_CONTRACT` 当时只有 `POST /business/hr/employee/import`（`card: "TBD"`）
  与 `GET …/export`，**没有**这条新路径；
- 反向扫描用例 `every_macro_path_in_module_is_in_contract` 扫 `src/business/hr/*.rs` 全部
  `#[nik::get/post/…("…")]`，只放行 `HEALTH_PATH` 一条例外 ⇒ 确定性红（A/B 见 `01_ab_red_before.txt`）。

## 2. 改动（2 个文件，业务代码零行为变化）

| 文件 | 改动 |
|---|---|
| `backend/src/business/hr/handler.rs` | `ROUTE_CONTRACT` 新增 1 行：`method: "GET"` / `path: "/business/hr/employee/import-template"` / `permission: Some("business:hr:employee:import")` / `card: "HR-47"`（放在 `POST /employee/import` 之后，紧跟所属分组）；数组长度 `[RouteSpec; 50]` → `51`；`contract_routes_are_unique_and_shaped` 的 `assert_eq!(…, 50, …)` → `51` 并补理由文案「+ HR-47 补的模板下载 1 条」；文件头与 `ROUTE_CONTRACT` 文档注释的「= 50」→「= 51」、「其余 45 条」→「其余 50 条」、「49 条路由空壳」→「51 条」 |
| `backend/src/business/hr/attendance_period_handler.rs` | 仅 1 行文档注释：`ROUTE_CONTRACT` 「从 46 条长到 49 条」后补「（后续 HR-41 / HR-47 又各补 1 条，现为 51 条）」，消掉最后一处过期计数口径 |

**权限码不新增迁移**：`business:hr:employee:import` 已由
`migrations/business/V0387__hr_menu_perms.up.sql:222`（按钮「导入员工」）种下，
所以 `contract_permissions_are_seeded_by_v0387`（断言种子 35 个码）照旧全绿 —— 实测见 §4。

未动：`employee_import_handler.rs`（一行没改）、任何测试文件、`POST /employee/import` 的 `card: "TBD"`。

## 3. 环境与命令

| 项 | 值 |
|---|---|
| 代码 | `/opt/cloudpivot`（工作仓，HEAD `8e299227` + 本卡 2 个文件） |
| 编译缓存 | 私有 `CARGO_TARGET_DIR=/root/t9a-target`（不与共享 `target/` 抢锁） |
| 演练库 | 145 `hr47_drill`：`REPO=/opt/cloudpivot BIN=/opt/cloudpivot/backend/target/debug/cloudpivot NO_DEMO_SEED=1 ERP_DB=hr47_drill bash scripts/verify/make_full_suite_db.sh`（先删后建 + 全量迁移 + bootstrap，不种演示组织 = 卡面「空库全量迁移后」口径） |
| 库指纹 | `migrations_versioned=335` / `latest=V0403` / `public 表 177` / `sys_user=1`（仅 admin+SUPER_ADMIN）/ `biz_voucher=0` / `biz_flow_instance=0` |
| 命令 | `ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr47_drill SYS_DB_URL=$ERP_DB_URL TASK_DB_URL=$ERP_DB_URL P0FIX_DB_URL=$ERP_DB_URL RUST_TEST_THREADS=1 cargo test --lib --features test-hooks -- business::hr -- --nocapture` |
| 串行口径 | cargo 1.94 下 `-- business::hr -- --test-threads=1` 会把 `--test-threads=1` 当**第二个过滤器**；必须 `RUST_TEST_THREADS=1`（同父卡 t_9a4e5c2a 的发现） |

## 4. 验收结果

| 判据 | 结果 | 证据 |
|---|---|---|
| `cargo test --lib --features test-hooks -- business::hr`（卡面期望 663/0；父卡修完夹具后模块共 **673** 条） | **673 passed / 0 failed / 0 ignored**，`4881 filtered out`，`SKIP` 行 **0** | `02_module_run_final_same_db_round2.txt`、`03_three_contract_tests_green.txt` |
| 三条契约用例 | `contract_routes_are_unique_and_shaped` / `contract_permissions_are_seeded_by_v0387` / `every_macro_path_in_module_is_in_contract` 全 `ok` | 同上 |
| A/B（把 `handler.rs` 换回 HEAD） | 同一二进制、同一命令 → `FAILED. 0 passed; 1 failed`，panic 文案与卡面一致（`handler.rs:589`） | `01_ab_red_before.txt` |
| `cargo clippy --all-targets --features test-hooks -- -D warnings` | **EXIT=0，零 warning** | `05_clippy_all_targets.txt` |
| `rustfmt --edition 2024 --check`（`handler.rs`、`attendance_period_handler.rs`） | 两者 `exit=0` | 见 §6 命令 |
| 库侧残留（跑完 psql 直查，不经测试代码） | `biz_employee=0 / biz_salary_sheet=0 / biz_leave_request=0 / biz_status_log=0` | 见 §6 命令 |

> 第一次 clippy 打在**文档注释**上有 2 条 `clippy::doc_lazy_continuation`（新增行以 `+ ` 开头，被 markdown 当列表项）：
> 已改成行内用顿号 / 行尾 `+`，重跑全绿。原始红留档 `04_clippy_first_attempt_doc_lazy_continuation.txt`
> （不是业务问题，但「零 warning 才提交」就按它修了）。

## 5. 为什么这条红以前被盖住

HR-48 闸⓪ 的 41 条失败里只有 1 条是它，其余 27 条是薪资引擎输入的夹具顺序依赖（父卡 `t_9a4e5c2a`
用 `fixture_hygiene.rs` 清场根治，已取证于 `docs/evidence/2026-11-13-hr-db-fixture-hygiene/`）。
夹具红清掉后它成为 `business::hr` 唯一残留红 —— 本卡收口后整模块 673/0。

## 6. 复现命令（含本卡用到的直查）

```bash
# 库侧残留与指纹（145）
PGPASSWORD=*** psql -h 172.18.10.145 -U cloudpivot -d hr47_drill -Atc "
  select 'migrations='||count(*)||' latest=V'||lpad(max(version::int)::text,4,'0') from schema_migrations;
  select count(*) from information_schema.tables where table_schema='public';
  select (select count(*) from biz_employee), (select count(*) from biz_salary_sheet),
         (select count(*) from biz_status_log), (select count(*) from biz_leave_request);"

# 格式
cd backend && rustfmt --edition 2024 --check src/business/hr/handler.rs \
  && rustfmt --edition 2024 --check src/business/hr/attendance_period_handler.rs
```

## 7. 收尾

- 演练库 `hr47_drill` 跑完复查后 **DROP**（不留垃圾库；145 上其余 drill 库属别的卡，未动）。
- 本卡只改了 `handler.rs` 的契约表与计数文案，运行期路由注册由 `employee_import_handler.rs`
  的 `#[nik::get]` 完成（HR-47 已交付），**没有**把任何「未实现」的空壳注册进 router。
