# HR 真库夹具统一清场（卡 `t_9a4e5c2a`）实测证据

**结论：`cargo test --lib --features test-hooks -- business::hr -- --test-threads=1` 在
「空库 + 全量迁移」的演练库上整模块一次跑完 = 672 passed / 1 failed；28 条夹具顺序依赖红全部关闭。**
唯一残留的 1 条红是**别人卡（HR-47 / `t_3389f8aa`）的契约漏登记**，本卡改动无关（A/B 见 04，后续卡 `t_575aced2`）。

## 1. 被测对象与口径

| 项 | 值 |
|---|---|
| 代码基线 | `HEAD = 113c8580`（`test(hr): 上线演练脚本 …（HR-49）`）+ 本卡 9 个文件；修前基线 28 红在 `9c39bd5c` 上取（中间只多 HR-49 的演练脚本与一个静态契约测试文件，不改夹具/薪资引擎） |
| 隔离树 | `/root/t9a-tree`（`git archive HEAD` 快照，不含别人未提交改动），私有 `CARGO_TARGET_DIR=/root/t9a-target` |
| 演练库 | 145 `hr9a_fix_drill`：`scripts/verify/make_full_suite_db.sh` 先删后建 + `NO_DEMO_SEED=1`（只迁移不种演示组织），库名含 `drill` |
| 库指纹 | 迁移 **335** 条 / 最高 **V0403** / `biz_employee=0` / `biz_salary_sheet=0` / `biz_salary_item=17`（全是 `V0396` 种子行）/ `sys_user=1`（仅 admin）/ `sys_status_log=0` |
| 命令 | `cargo test --lib --features test-hooks -- --test-threads=1 --nocapture business::hr` |
| 用例数 | `running 673 tests`（HR 模块全部用例，未 `--skip` 任何一条） |

> 为什么用「只迁移、不种演示组织」的库：卡面判据就是「空库全量迁移后」。种子完备库是**全量 lib 套件**的口径
> （`docs/测试库口径-全量套件.md`），HR 模块自己的夹具会在 `setup()` 里建自己要的部门/账号，不需要演示组织。

## 2. 修前 / 修后

| 轮次 | 结果 | 失败清单 |
|---|---|---|
| 修前（HEAD 原版，撤掉本卡 9 个文件后重编） | **635 passed / 28 failed** | `01_baseline_28_failed.txt`（27 条夹具顺序依赖 + 1 条 HR-47 契约） |
| 修后 第 1 轮（同一份库口径，全新库） | **672 passed / 1 failed** | `02_after_round1.txt` |
| 修后 第 2 轮（**同一个库**，带第 1 轮残留） | **672 passed / 1 failed** | `03_after_round2.txt` |

两轮失败集合完全相同（都只有那 1 条），且都是「单跑也红」的确定性红 ⇒ 不是并发互踩。
两轮跑完库侧复查（psql 直查，不经测试代码）：`biz_employee=0 / biz_attendance_record=0 /
biz_attendance_period=0 / biz_salary_sheet=0 / biz_employee_salary=0 / 非种子薪资项=0 / biz_leave_request=0`。

## 3. 根因与修法

薪资引擎的取数是**全量读**：`salary_compute::calc_with_items` 会把库里
`hire_date <= 期末 AND (leave_date IS NULL OR leave_date >= 期初)` 的**所有**未删员工都算进一张工资单。
各模块原来的 `sweep()` 都只按**自己的 tag 前缀**（`HR34%` / `HR36%` / `HR37%` / `HR41%` / `HR42%`）清，
而上游模块（`employee_model_db_tests` / `employee_seed_db_tests` / `self_service_db_tests` …）留下的员工
不在这条前缀里 ⇒ 下游薪资用例「本期间只有我这两个人」的断言必然漂：

- `salary_compute_db_tests::db::...「缺考勤的人要点名」`：left 有 4 人（3 个 `EMP-0001 HR10 夹具员工` + `HR34B 李四`）；
- 更糟：残留员工身上挂着 `biz_employee_salary` 标准但薪资项已被别人删 ⇒ 引擎当场报
  「应发合计（公式项 GROSS 与按项类型聚合）不一致」，建单直接 400/10001。

**修法（新增 `backend/src/business/hr/fixture_hygiene.rs`，`#[cfg(test)]`）**：

1. `reset_engine_inputs()`：把**薪资引擎的输入集**一次性清成空库口径 —— 按外键从叶子到根删
   `biz_salary_sheet`（明细/明细项随 CASCADE）→ `biz_attendance_record` → `biz_attendance_period` →
   `biz_employee_contract` → `biz_leave_request` → `biz_employee_salary` → 非种子 `biz_salary_item`
   → `biz_employee` → 再收这五类实体的**孤儿 `biz_status_log`**（`biz_status_log` 没有外键，只能按「引用是否还活着」判，
   口径同 `business/erp/fixture_hygiene.rs`）。
   非种子薪资项按 `V0396` 逐字写入的 `remark` 判（`ARCH-9-2 薪资项种子（V0396）`），**不手抄 item_code 清单**。
2. **清场后复查**（不信 `DELETE` 的返回值）：`biz_employee` 与非种子薪资项必须都为 0，否则点名「并发夹具互踩」。
3. `ENGINE_LOCK`（`tokio::sync::Mutex`）：「同一批全局行 = 同一把锁」。用例把 guard 存进自己的 `Fixture`
   一直持到用例结束；默认 4 线程下至少保证薪资类模块之间不互踩。
4. **清场挂在每个用例的 `setup()` 上**（不是尾部）：用例 panic 时尾部那行不执行，但下一个用例的 setup 会把残留一并清掉
   （技能 §3「清场挂在壳上」的口径）。
5. **安全闸**：`reset_engine_inputs()` 先看 `current_database()`，库名不含 `drill` / `test` 直接 **panic**（不是 skip）——
   清场是破坏性的（会删库里所有员工），宁可红也不许清空开发库/生产库。
6. 断言口径**没有放宽**：一条断言都没删，只是让「本期间只有我这几个人」这个前提重新成立。

**接线范围**（只动测试文件 + 一个新夹具助手，业务代码零改动）：
`mod.rs`（声明 `pub(crate) mod fixture_hygiene;`）、
`salary_compute_db_tests.rs` / `salary_sheet_db_tests.rs` / `salary_override_db_tests.rs` /
`salary_bridge_db_tests.rs` / `salary_export_db_tests.rs` / `self_service_db_tests.rs` /
`salary_item_seed_tests.rs`（各自的 `sweep()` 换成 `hygiene::exclusive()`，返回 guard 存进 `Fixture`）。

其余 20 个 `*_db_tests.rs` 一行没改：它们不写薪资引擎的输入集，或者自己已经按 tag 前缀清干净了
（本卡跑完 673 条里它们全绿）。

## 4. 唯一残留的红：不是本卡引入（A/B）

`business::hr::handler::tests::every_macro_path_in_module_is_in_contract` ——
`employee_import_handler.rs:47` 声明了 `GET /business/hr/employee/import-template`（HR-47 的提交 `12d20c7d`），
但没在 `handler.rs` 的 `ROUTE_CONTRACT`（`[RouteSpec; 50]`）里登记。该用例**运行期读磁盘源码**，
所以用同一个测试二进制、只把本卡 9 个文件换回 HEAD 版本再跑，红照旧 ⇒ **与本卡改动无关**（`04_ab_contract_independent.txt`）。
已开卡交 backend-developer（`ROUTE_CONTRACT` 加 1 行 + 数组长度 50→51 + 计数断言/文档同步）。

## 5. 门禁

| 项 | 结果 |
|---|---|
| `cargo test --lib -- -D ...`（见 §2） | 672 passed / 1 failed（唯一红见 §4），**连跑两轮同结果** |
| `rustfmt --edition 2024 --check`（本卡 9 个文件） | exit 0，无 diff |
| `cargo clippy --all-targets --features test-hooks -- -D warnings` | exit 0，0 warning（**并做了负样本：故意写 `&Vec<u8>` 后报 `clippy::ptr_arg` + 「could not compile (lib test)」⇒ 证明 test target 真被 lint**） |
| SKIP 检测（`--nocapture` 下 grep `SKIP: ...DB_URL`） | 0 条（没有「静默早退」的假绿） |
| 演练库清理 | 本卡在 145 建过的三个库（`hr9a_fix_drill` / `hr9a_drill` / `hr9a4e5c2a_drill`）**全部 DROP**，`select datname … like 'hr9a%'` 复查 0 行；99 上一行都没建（本卡只在 145 建库）；145 上其余 drill 库属别的卡，未动 |
