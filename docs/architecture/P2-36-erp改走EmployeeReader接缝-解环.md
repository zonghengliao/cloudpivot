# P2-36 · `erp` 改走 `EmployeeReader` 接缝（去掉 `erp → hr` 的表级直连）

> 上游：**P2-35**（`hr` 提供接缝，`docs/architecture/P2-35-hr员工只读查询接缝-EmployeeReader.md`）
> 本卡：把消费方切过去，并把「`erp` 生产面直连 `biz_employee`」的燃烧率基线从 **19 降到 0**。
> 落点：`backend/app/src/business/erp/cost/cost_labor_{model,import,dto}.rs`、`backend/app/src/business/erp/seq.rs`、
> `backend/app/src/business/hr/employee_no.rs`（表属主自持查重 SQL）。

---

## 1. 结论（一屏看完）

| 事实 | 改前（HEAD `57187ca0`） | 改后 | 用什么钉住 |
|---|---|---|---|
| `erp` 生产面 `biz_employee` 直连行数 | **19 行** | **0 行** | `hr::employee_reader_tests::erp_biz_employee_reference_budget_is_not_exceeded`（基线常量 19 → **0**） |
| `erp` 读员工的方式 | 自写 2 条 SQL（探表 `to_regclass` + 工号批量解析） | `hr::employee_reader::{is_available, employees_by_emp_nos}` | 同上 + `erp_only_depends_on_the_employee_seam` |
| `erp → hr` 模块边 | **不存在**（代码 0 / 注释 0） | **存在，但只指向接缝**（代码 3，其中生产面 1 行 = `use ... employee_reader`） | `erp_only_depends_on_the_employee_seam`（只许 `business::hr::employee_reader`） |
| 号段 `EMP` 的查重 SQL | 长在 `erp::seq::master_code_exists_sql` | 搬回 **HR**（`hr::employee_no::EMP_EXISTS_SQL`，经 `*_with` 入口传入） | `erp::seq::tests::t02_exists_sql_covers_every_erp_owned_master_type` + `hr::employee_no::tests::emp_exists_sql_matches_the_owner_key` |
| 「工号能不能解析」的用例 | **必须连真库** | 可脱库（注入假实现） | `cost_labor_import::tests::employee_numbers_resolve_through_the_seam_without_a_database` 等 3 条 |

**一句话**：`erp` 从此不知道员工主档长什么样（表名 / 工号列名 / 软删口径 / 多组织取法全不认），
只认一个 trait；`erp → hr` 那条边没有消失（消费方总得调接缝），消失的是它的**形状**——
从「任意读表」收窄成「唯一一个接缝模块」，并由用例钉死。

---

## 2. 改了什么（逐处，含为什么）

### 2.1 `erp/cost/cost_labor_model.rs`：删掉两个跨域 SQL 出口

| 删掉的函数 | 它原来做什么 | 现在谁做 |
|---|---|---|
| `employee_table_present()` | `SELECT (to_regclass('public.biz_employee') IS NOT NULL)` 探表 | `employee_reader::is_available()`（表属主给的探针） |
| `employee_ids_by_no(&[String]) -> Vec<(String, Uuid)>` | `SELECT DISTINCT ON (emp_no) emp_no, id FROM biz_employee …` | `employee_reader::employees_by_emp_nos(&[String]) -> Vec<EmployeeBrief>` |

删而不是「改成调接缝」，理由：model 层是**本域**的数据访问层，把跨域接缝调用塞进 model 会让
「谁在读别人的表」重新变得不可见（P2-35 文档 §3 的三条理由之一）。调用点只有 `cost_labor_import`
一个，直接在那里调接缝更清楚。

### 2.2 `erp/cost/cost_labor_import.rs`：`attach_employee_issues` 走接缝

```rust
// 改前
let present = model::employee_table_present().await…?;
refs.employees = model::employee_ids_by_no(&numbers).await…?.into_iter().collect();

// 改后（唯一一处 erp → hr 的生产面引用）
let available = employee_reader::is_available().await…?;
refs.employees = employee_reader::employees_by_emp_nos(&numbers).await…?
    .into_iter().map(|brief| (brief.emp_no, brief.id)).collect();
```

**行为逐条等价**（这是敢切的前提，不是感觉）：

| 行为点 | 改前 | 改后 | 依据 |
|---|---|---|---|
| 软删 | `deleted_at IS NULL` | 同（SQL 在 `hr::employee_reader_impl`） | P2-35 实现侧逐字对齐 |
| 同一工号多行 | `DISTINCT ON (emp_no) … ORDER BY emp_no, id` | 同 | 同上 |
| 组织维度 | 不按 `org_id` 过滤 | 同 | 同上（多组织落地时只改接缝一处） |
| 批量 | `IN` 切片 500 + 去重 | 同（接缝内部做） | 同上 |
| 主档不可读 | 该行报「工号无法解析，请留空」 | 同（`is_available()==false`） | 见 §5 的脱库用例 |
| 解析不到 | 该行报「在员工主数据里不存在 + 回显工号」 | 同 | `cost_labor_db_tests` ⑮（真库）+ 离线用例 |

### 2.3 `erp/seq.rs`：取号器不再认识 HR 的表

`master_code_exists_sql("EMP")` 原返回 `SELECT 1 FROM biz_employee WHERE emp_no = ? AND deleted_at IS NULL`。
这是 **`erp` 里第二处跨域直连**（本卡之前被 P2-35 的 19 行基线一起盖住了），而且它踩过坑：
工号列被按业务名猜成 `employee_no`，表还没上线时语句永远走不到，**猜错也看不出来**。

处置：`EMP` **仍是主数据号段**（`MASTER_CODE_TYPES` 7 类不变，发号规则一模一样），
但**查重口径交给表属主**。HR 通过取号器本来就有的通用扩展点
`next_master_code_free_with` / `peek_master_code_with` 传入自己的常量 SQL（`EMP_EXISTS_SQL`）。

> **为什么不用「`erp::seq` → `hr::employee_reader`」**（另一条可行路）：
> 那会让 `hr → erp::seq → hr::employee_reader` 成环（HR 取号本来就调 `erp::seq`），
> 且 `erp` 多出一处 `hr` 引用面。把 SQL 交给表属主是**零新增边**的解。
> `*_with` 入口原先就是为「非标准表 / 特殊软删口径」设计的，语义归位。

### 2.4 `hr/employee_no.rs`：表属主自持查重 SQL

新增 `EMP_EXISTS_SQL` 常量（与原 `erp` 里那条逐字一致：`deleted_at IS NULL`、不带 `org_id`、
一个 `?` 占位），`generate_emp_no` / `preview_emp_no` 改走 `*_with`。
断言从 `erp::seq` 搬到本文件（`emp_exists_sql_matches_the_owner_key`）。

### 2.5 测试面

| 文件 | 改动 |
|---|---|
| `erp/cost/cost_labor_import.rs`（内联 `mod tests`） | 新增 3 条**脱库**用例 + 一个 `FakeEmployeeReader`（P2-35 文档 §4 承诺的「消费方能脱库测试」在这里兑现） |
| `erp/cost/cost_labor_db_tests.rs` | ⑮ 的探表断言改成 `employee_reader::is_available()`；新增 `ensure_employee_seam_installed()`（单测无启动链，显式装一次 HR 实现） |
| `hr/employee_reader_tests.rs` | `ERP_BIZ_EMPLOYEE_REFS_BASELINE` **19 → 0**；新增 ⑨ `erp_only_depends_on_the_employee_seam` |
| `erp/seq.rs`（内联 `mod tests`） | `t02_exists_sql_covers_every_master_type` → `…_every_erp_owned_master_type`，并新增「EMP 查重表不在本域」的断言 |
| `hr/employee_no.rs`（内联 `mod tests`） | `emp_is_registered_as_master_code` 反转断言 + 新增 `emp_exists_sql_matches_the_owner_key` |

**没有新增测试文件** ⇒ `scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST`（155 条）无需改动，实测逐条一致。

---

## 3. 「依赖边消失」的证据（按卡面要求，先说清口径）

### 3.1 `cargo tree` 口径（**先说清楚它证明不了模块边**）

```console
$ cd backend && cargo tree -p cloudpivot --depth 1
cloudpivot v0.1.0 (/opt/cloudpivot/backend)
├── cloudpivot-doc v0.1.0 (/opt/cloudpivot/backend/crates/cloudpivot-doc)
├── nik v0.4.2 (/opt/cloudpivot/backend/vendor/nik)
├── nik-utils v0.4.2 (/opt/cloudpivot/backend/vendor/nik-utils)
└── …（其余为第三方 crate：axum / tokio / serde / rust_decimal / calamine …）

$ cargo tree | grep -c "business::"
0
```

`backend` 目前是**单 crate**（workspace 化是 P2-C 的活），`business::erp` 与 `business::hr` 是
同一 crate 的**模块**，`cargo tree` 画不出这条边 —— 所以 `grep = 0` 证明的是「没有 crate 级边」，
**不能**用来证明「模块边消失」（P2-31/32/33/34/35 同口径，不粉饰）。
模块级用 `scripts/lint/module_cycle_report.py`，表级直连用燃烧率闸门（§3.2、§3.3）。

### 3.2 模块边：从「不存在」变成「只指向接缝」

```console
# 改前（HEAD 57187ca0，脚本 --src 指向 git archive 出来的旧树）
$ python3 scripts/lint/module_cycle_report.py --src …/p236_before/backend/app/src --edge business::erp business::hr
边 business::erp → business::hr：**不存在**（该域内无任何 crate:: 引用指向 business::hr）

# 改后（工作区）
$ python3 scripts/lint/module_cycle_report.py --edge business::erp business::hr
边 business::erp → business::hr：存在（代码 3 / 注释 0）
  [code] business/erp/cost/cost_labor_db_tests.rs:65      ← 测试面（装 HR 实现）
  [code] business/erp/cost/cost_labor_import.rs:1153      ← 内联测试面
  [code] business/erp/cost/cost_labor_import.rs:67        ← **唯一的生产面引用**：use … employee_reader
```

也就是说：**本卡不是把 `erp → hr` 抹掉，而是把它从「读表」改成「调接缝」**。
脚本能证明的是「没有别的引用面」，形状由用例钉住：

```console
$ cargo test --lib business::hr::employee_reader_tests::erp_only_depends_on_the_employee_seam -- --nocapture
erp → hr 接缝引用：2 处，越界 0 处 · 扫描 295 个文件
```

（2 处 = `cost_labor_import.rs:67` 的生产 import + 同文件 `#[cfg(test)] mod tests` 里的 import；
都没有越界。测试面的 import 不进生产编译，但**文本扫描也把内联测试算进来**了 —— 宁可严一点。）

### 3.3 表级直连（这才是本卡真正消除的东西）：19 → 0

```console
$ grep -rn "biz_employee" --include=*.rs src/business/erp/ | grep -v _tests.rs | wc -l
0

$ cargo test --lib business::hr::employee_reader_tests::erp_biz_employee_reference_budget_is_not_exceeded -- --nocapture
erp 生产面 biz_employee 直连：0 行（基线 0）· 扫描 295 个文件
```

### 3.4 红绿反证（闸门真的会红，不是摆设）

往 `erp` 里各塞一行临时文本再跑（随后已删除）：

| 塞什么 | 哪条用例红 | 报错 |
|---|---|---|
| `seq.rs` 末尾加 `// … biz_employee` | `erp_biz_employee_reference_budget_is_not_exceeded` | `FAILED`（回退了：1 行，基线 0 行） |
| `cost_labor_import.rs` 末尾加 `// … business::hr::model` | `erp_only_depends_on_the_employee_seam` | `FAILED`（绕过接缝的引用：…） |

```console
test ...::erp_biz_employee_reference_budget_is_not_exceeded ... FAILED
test ...::erp_only_depends_on_the_employee_seam ... FAILED
test result: FAILED. 7 passed; 2 failed; …
```

反证文本删除后：`36 passed; 0 failed`（两文件全部用例）。

---

## 4. 验证（命令 + 实测输出）

```console
$ cd backend && cargo fmt && cargo check --lib
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 14.75s      # 零 warning

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 57.49s      # EXIT=0，零告警

$ env -u ERP_DB_URL cargo test --lib -- --test-threads=1
test result: ok. 5642 passed; 0 failed; 0 ignored      # 改前 5637 → +5（3 脱库 + 1 形状 + 1 EMP 口径）
```

真库（99 → `172.18.10.145`，模板 `cloudpivot_test` clone 出 `cp_p236_seam_drill`，182 表）：

```console
$ ERP_DB_URL=…@172.18.10.145:5432/cp_p236_seam_drill \
    cargo test --lib -- business::hr:: business::erp::cost::cost_labor business::erp::seq -- --test-threads=1
test result: ok. 809 passed; 0 failed; 0 ignored; 0 measured; 4833 filtered out; finished in 243.98s
EXIT=0        # 日志里 SKIP 计数 = 0（全部真跑，没有静默跳过）
```

关键几条（真库）：

```console
$ cargo test --lib -- business::erp::cost::cost_labor_db_tests::db::employee_number_must_resolve …
... ok          # 工号解析走接缝，真库口径不变
$ cargo test --lib -- business::hr::employee_no_db_tests::db::
... taken_number_is_skipped ... ok      # 撞号顺延（HR 自带查重 SQL 生效）
... rollback_does_not_burn_a_number ... ok   # 取号与 INSERT 同事务，回滚不占号
$ cargo test --lib -- business::hr::employee_reader_db_tests::db::
... 6 passed                   # 接缝实现（软删 / DISTINCT ON / 跨 IN_CHUNK 全序）
```

门禁：

```console
$ python3 scripts/lint/scope_gate.py --root .
  PASS：跳过清单与 SKIP_MANIFEST 逐条一致（155 条）
  PASS：0 处
```

演练库用完即删（`dropdb -h 172.18.10.145 -U cloudpivot cp_p236_seam_drill`）。

---

## 5. 如实说明（没做 / 做不到 / 留给下一张卡）

1. **`erp → hr` 的模块边仍然存在**（代码 3，生产面 1）。这是接缝方案的必然结果：
   消费方要调 `hr::employee_reader` 就得引用 `hr`。本卡消除的是**表级直连**（19 → 0）与
   **引用面形状**（只许接缝）。想让「模块边」也归零，唯一办法是把 trait 放到
   `business::shared`（那是另一个设计决定，不在本卡内，也没得到授权）。
2. **`hr → erp` 的 6 处回边没动**（凭证过账 / EMP 号段 / 期间键 / 审批闸）。所以
   `erp ⇄ hr` 在 `module_cycle_report` 里**仍是双向**（`hr → erp` 代码 6 / 注释 0 —— 改前注释 5，
   顺手把 5 处失效的 intra-doc 链接改写掉了，代码一处没动）。接手卡：**`t_3442b4fb`**（P2-35 已开）。
3. **测试面的两处 `hr` 引用**（`cost_labor_db_tests.rs:65`、内联测试 import）依赖
   `hr::employee_reader_impl` —— 真库用例必须装真实现，无法用假实现替代。它们不进生产编译，
   燃烧率闸门只扫生产面（`*_tests.rs` 与 `#[cfg(test)] mod` 内联区不计）。
4. **`EMP_EXISTS_SQL` 在 HR 侧仍是 SQL 文本**（经 `*_with` 传参）。这是取号器既有的扩展点，
   不是新造的耦合；若日后想彻底去掉 SQL 文本传参，可给取号器加「查重回调」参数 ——
   属于取号器自身的演进，与本卡解环无关，未做。
5. **前端零改动**：本卡只动后端依赖方向，接口路径 / 出参 / 错误码全不变。
