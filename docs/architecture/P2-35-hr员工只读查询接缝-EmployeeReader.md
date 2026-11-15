# P2-35 · HR 提供员工只读查询接缝（`EmployeeReader`）

> 卡：`t_fe5cbe83`（P2-B 解环第 9 张）· 2026-11-15
> 上游：P2-31（`shared` 无环收口）→ P2-32/33/34（`erp ⇄ flow` 收口）
> 下游：P2-36（`erp` 改走本接缝，把 `biz_employee` 的直连 SQL 摘成 0）

---

## 1. 结论（一屏看完）

| 项 | 改前 | 改后 | 证据 |
|:--|:--|:--|:--|
| 模块级边 `erp → hr` | 存在（代码 0 / **注释 1**） | **不存在** | `python3 scripts/lint/module_cycle_report.py --edge business::erp business::hr` |
| `erp` 生产面对 `biz_employee` 的直连 | 19 行 / 4 文件 | 19 行（本卡**不动**，只立接缝） | 用例 `employee_reader_tests::erp_biz_employee_reference_budget_is_not_exceeded` 钉住基线 19 |
| `hr` 侧跨域读员工的能力 | 无（各消费方自写 SQL） | `EmployeeReader` trait + 注册表 + HR 侧实现 | 本文件 §3 |
| 启动接线 | — | `bootstrap::register_hr_employee_reader()`（prod 失败 `exit(1)`） | `backend/app/src/bootstrap.rs` |
| 测试 | — | 17 条（离线 11 + 真库 6），全绿 | 本文件 §5 |

**这张卡只做接缝，不切换消费方**：`erp` 的 19 行直连在 P2-36 摘掉（那是 P2-36 的验收），
本卡把闸门基线钉在 19，任何「又往 `erp` 里写一条 `biz_employee`」的改动立刻红。

---

## 2. 为什么要解这条边（现状取证，不猜）

`biz_employee` 是 HR 的表（`V0383__hr_tables.up.sql`），但读它的人不止 HR：

```console
$ cd backend/app/src/business/erp
$ grep -rn biz_employee --include=*.rs . | grep -v _tests.rs | sed 's/:.*//' | sort | uniq -c
      2 ./cost/cost_labor_dto.rs
      3 ./cost/cost_labor_import.rs
      8 ./cost/cost_labor_model.rs
      6 ./seq.rs
$ grep -rn biz_employee --include=*.rs . | grep -v _tests.rs | wc -l
19
```

其中两处是**活的跨域 SQL**（另 12 行是注释与 `seq.rs` 的 `EMP` 查重片段）：

- `cost_labor_model::employee_table_present()` —— `to_regclass('public.biz_employee')` 探表；
- `cost_labor_model::employee_ids_by_no()` —— 按工号批量解析 `emp_no → id`。

这两处踩过一个真实事故：工号列被猜成 `employee_no`（真名是 `emp_no`），
表还没上线时永远走不到那条语句，「猜错也看不出来」；HR 上线后任何填了工号的工时导入直接 500
（`cost_labor_model.rs` 的注释里有完整记录）。**SQL 长在消费方身上，这类错误就没有唯一出口。**

另外两条更慢的代价：

1. 多组织（ARCH-9-1）落地时要给**每一处**直连加组织维度，漏一处就是越权读；
2. `erp` 侧「工号能不能解析」的用例必须连真库，因为它测的是自己写的那条 SQL。

---

## 3. 接缝设计（`backend/app/src/business/hr/employee_reader{,impl}.rs`）

### 3.1 形状

```
消费方（P2-36 的 erp/cost_labor）
        │  只调这些函数：employees_by_ids / employees_by_emp_nos / employee_by_id / is_available
        ▼
hr::employee_reader        ← trait EmployeeReader + EmployeeBrief + 进程级注册表（OnceLock<RwLock>）
        ▲  install(Arc<dyn EmployeeReader>)   启动期一次
        │
hr::employee_reader_impl   ← HrEmployeeReader：唯一实现，biz_employee 的跨域只读 SQL 出口
```

与 `flow::doc_reader`（P2-32/33）同款两阶段结构：**空表先就位 → 启动期由属主写满 → 之后只读**。
不用 `OnceLock<Arc<dyn …>>` 的 `get_or_init` 装填，理由同 P2-32：闭包里只能调本模块能看见的东西，
那正好把「接缝 → 实现」的反向依赖装回来。

### 3.2 `EmployeeBrief` 的五个字段（每个都指得到表列与调用点）

| 字段 | 列（`V0383`） | 用途 |
|:--|:--|:--|
| `id` | `id` | 工时台账 `employee_id`、导入把工号解析成它 |
| `emp_no` | `emp_no` | 导入回执 / 报错点名「哪个工号」 |
| `name` | `name` | 展示（主数据 ⇒ 实时取名，AGENTS 约定 13） |
| `dept_id` | `dept_id` | 数据范围裁剪（员工主档按人事归属部门，ARCH-9 D-12） |
| `status` | `status` | 在岗判据（落库**原值**，不是归一布尔） |

`EmployeeBrief::is_on_duty()` = `PROBATION | ACTIVE`：判据放接缝里，省得每个消费方各写一遍
`status == …` 然后漏一个状态。

**敏感列一律不进快照**（`id_card` / `bank_account` 要明文走
`GET /business/hr/employee/{id}/sensitive`，权限码 + 审计）。跨域读没有敏感面，就少一个泄漏渠道。

### 3.3 三个方法的语义

| 方法 | 空入参 | 软删行 | 未安装 reader |
|:--|:--|:--|:--|
| `employees_by_ids` | 不查库，`Ok(vec![])`（`IN ()` 是语法错误） | 不返回 | `Ok(vec![])` |
| `employees_by_emp_nos` | `trim` + 去空 + 去重后为空 ⇒ `Ok(vec![])` | 不返回 | `Ok(vec![])` |
| `is_available` | — | — | `Ok(false)`（唯一能区分「没这能力」与「没这人」的入口） |
| `employee_by_id` | 默认实现走批量 | 不返回 | `Ok(None)` |

**未安装为什么不是 500**：`hr` 与 `erp` 同进程、同一份 `bootstrap`，未安装只可能是启动链漏了一步
（编程错误，`register_hr_employee_reader` 里 prod `exit(1)` 兜住）。让它在请求期变 500，
等于把启动期错误摊到每个用户请求上，还查不出根因。

**为什么两个批量方法不给默认实现**：默认 `Ok(vec![])` 等于「漏实现 ⇒ 静默查无此人」——
工时导入会把每一行都报成「工号不存在」，而根因是漏了一个方法。required ⇒ 漏了编译不过。

### 3.4 实现侧的三条口径（与 `erp` 原 SQL 逐字对齐，P2-36 才敢说「等价」）

| 口径 | 值 | 原来写在哪 |
|:--|:--|:--|
| 软删 | `deleted_at IS NULL` | `cost_labor_model::employee_ids_by_no` |
| 工号多行取法 | `DISTINCT ON (emp_no)` + `ORDER BY emp_no, id`（取最小 id） | 同上（注释写明「不加 DISTINCT 会随机映射到一行」） |
| 组织维度 | **不按 `org_id` 过滤** | 同上（工时导入这一层拿不到组织上下文） |

另外两条本卡新增的纪律（原 SQL 没有，属于「顺手补的确定性」）：

- `IN (...)` 按 `IN_CHUNK = 500` 切片，入参先去重（5000 行导入里同一工号会出现几十次）；
- **跨批结果重新按 `(emp_no, id)` 全序** —— 分片方式不许泄漏成返回顺序（真库用例用 501 行验证）。

---

## 4. 与 P2-36 的接口（切换时照这个写）

```rust
// 生产代码：只认模块级入口，不碰 HrEmployeeReader、不碰 biz_employee
use crate::business::hr::employee_reader;

let briefs = employee_reader::employees_by_emp_nos(&numbers).await?;   // 工号批量解析
let by_id  = employee_reader::employees_by_ids(&ids).await?;          // 按 id 批量
let alive  = employee_reader::is_available().await?;                  // 替换 to_regclass 探针

// 单元测试：注入假实现，不用建库（形状见 employee_reader_tests::FakeReader）
employee_reader::reset_for_tests();
employee_reader::install(Arc::new(FakeReader::new(vec![brief("EMP-0001", 1)]))).ok();
```

P2-36 收尾时把 `employee_reader_tests::ERP_BIZ_EMPLOYEE_REFS_BASELINE` 从 **19 改成 0**
（常量注释里写明「P2-36 降为 0」）。

> **P2-36 已完成**（`docs/architecture/P2-36-erp改走EmployeeReader接缝-解环.md`）：
> 基线已改 0，`erp` 生产面 `biz_employee` 直连实测 **0 行**；`erp → hr` 只剩接缝这一处引用，
> 形状由 `erp_only_depends_on_the_employee_seam` 钉死。**仍未消除**的是 `hr → erp` 的 6 处回边（`t_3442b4fb`）。

---

## 5. 验证（命令 + 实测输出）

### 5.1 编译 / 格式 / 告警

```console
$ cd backend && cargo check --lib
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 34.14s      # 零 warning

$ cargo fmt --check && echo FMT_OK
FMT_OK

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile …                                                  # 零告警
```

### 5.2 边消失的证据

```console
$ python3 scripts/lint/module_cycle_report.py --edge business::erp business::hr
扫描面：774 个 .rs / 节点 21 个 / 跨域有向边 61 条 （口径：仅 business::*）

边 business::erp → business::hr：**不存在**（该域内无任何 crate:: 引用指向 business::hr）
```

改前是「存在（代码 0 / 注释 1）」，唯一一条命中是 `business/erp/seq.rs:27` 的文档链接
`[crate::business::hr::employee_no]` —— 本卡顺手改写成人话（说明「取号入口在 HR 侧」，
不再写跨域路径），于是这条边**归零**。

反方向的 `hr → erp` 仍是 **代码 6 / 注释 5**（薪资凭证过账、`EMP` 号段、期间键、审批闸），
**本卡不动**，见 §7 的如实说明。

**crate 级 `cargo tree` 的口径（与 P2-31/32/33/34 一致）**：整个后端是**单 crate**，
`cargo tree` 只到 `cloudpivot → cloudpivot-doc / nik` 一层，模块级边在这个粒度**无法表达**：

```console
$ cargo tree -p cloudpivot --depth 1
cloudpivot v0.1.0 (/opt/cloudpivot/backend)
├── cloudpivot-doc v0.1.0 (/opt/cloudpivot/backend/crates/cloudpivot-doc)
├── nik v0.4.2 (/opt/cloudpivot/backend/vendor/nik)
└── …（其余为第三方依赖）
$ cargo tree | grep -c "business::"
0
```

⇒ 模块级「边消失」的正式证据是上面那条 `module_cycle_report`（唯一能表达模块粒度的工具）
＋ §5.3 的燃烧率闸门。crate 级无环要等 P2-C（拆 workspace）才可能出现在 `cargo tree` 里。

### 5.3 燃烧率闸门（防止「解了又长回来」）

```console
$ cargo test --lib business::hr::employee_reader_tests::erp_biz_employee_reference_budget -- --nocapture
running 1 test
erp 生产面 biz_employee 直连：19 行（基线 19；P2-36 切到接缝后应为 0）· 扫描 295 个文件
test ... ok
```

闸门带**扫描面自检**（`ERP_SOURCE_MIN_FILES = 250`）：路径写错导致「扫到 0 个文件、0 条引用」
会立刻红，不会静默变绿。

### 5.4 测试

```console
# 离线（11 条：注册表语义 / 未安装降级 / 单条默认实现 / 接缝零业务域依赖 / 燃烧率闸门）
$ cargo test --lib business::hr::employee_reader -- --test-threads=1
running 17 tests … test result: ok. 17 passed; 0 failed          # 含 6 条真库用例的 SKIP

# 真库（6 条，145 演练库 cp_p235_reader_drill，模板 cloudpivot_test clone）
$ ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_p235_reader_drill \
    cargo test --lib business::hr::employee_reader_db -- --test-threads=1
running 6 tests … test result: ok. 6 passed; 0 failed

# HR 全模块回归（同一演练库）
$ ERP_DB_URL=…/cp_p235_reader_drill cargo test --lib business::hr:: -- --test-threads=1
test result: ok. 690 passed; 0 failed; 0 ignored; 0 measured; 4947 filtered out; finished in 213.46s
```

（读数补充：第一次跑把库名起成 `cp_p235_reader` —— **不含 `drill`/`test`** —— 被 `fixture_hygiene`
拦下 41 条全红，报错原文「拒绝在库 `cp_p235_reader` 上清场：…库名含 `drill` 或 `test`」。
改名 `cp_p235_reader_drill` 后同一批用例全绿。这条闸本身是对的：HR 薪资引擎是全量取数，
在非演练库上清场会删掉真实员工。）

真库用例覆盖的六件事：软删过滤 + 字段逐列对得上 + `(emp_no, id)` 全序；空入参 / 重复入参；
按工号批量（trim、未知、空串、软删、离职状态原值）；**501 行跨 `IN_CHUNK` 的确定性全序**；
`is_available` 探针；**注册表路径与直调实现逐字段一致**（钉住 `bootstrap` 的接线形状）。

真库纪律：库名必须含 `drill` / `test`（HR 的 `fixture_hygiene` 会拒绝在别的库上清场 ——
第一次跑就是因为库名叫 `cp_p235_reader` 被 41 条用例集体拒绝，已改名重跑）；夹具按 id 精确删，
唯一例外是 501 行那组按「带唯一后缀的工号前缀」删。

---

## 6. 改动的文件

| 文件 | 说明 |
|:--|:--|
| `backend/app/src/business/hr/employee_reader.rs` | **新**：trait / `EmployeeBrief` / 注册表 / 模块级入口 / 单测串行锁 |
| `backend/app/src/business/hr/employee_reader_impl.rs` | **新**：`HrEmployeeReader`（唯一实现，跨域只读 SQL 出口）+ 3 条离线单测 |
| `backend/app/src/business/hr/employee_reader_tests.rs` | **新**：11 条离线用例（含燃烧率闸门、接缝零业务域依赖） |
| `backend/app/src/business/hr/employee_reader_db_tests.rs` | **新**：6 条真库用例 |
| `backend/app/src/business/hr/mod.rs` | 挂 4 个模块（2 生产 + 2 测试面） |
| `backend/app/src/bootstrap.rs` | `register_hr_employee_reader()` + 调用（prod 失败 `exit(1)`） |
| `backend/app/src/business/erp/seq.rs` | 文档里那处跨域路径改成人话（`erp → hr` 注释边归零） |
| `scripts/lint/scope_gate.py` | `SKIP_MANIFEST` 补两个新测试面文件（闸② 硬断言，多一条少一条都红） |

---

## 7. 如实说明（没做 / 做不到 / 留给下一张卡）

1. **`hr → erp` 的 6 处代码回边没动**：`salary_bridge`（凭证过账）、`employee_no`（`EMP` 号段）、
   `leave_model`（期间键）、`leave_handler` / `salary_sheet_handler`（审批闸）。
   这意味着 P2-36 把消费方切到接缝之后，`erp ⇄ hr` 这对**在模块报告里仍是双向边**
   （`erp → hr` 会从 0 变成「接缝调用」的代码引用）。
   要真正把这对收成单向，还得摘 `hr → erp` 这 6 处（凭证要改成 erp 订阅 hr 事件、
   号段/期间键要下移到 `shared`）—— **那是另一张卡的量，不在 P2-35/36 的卡面里**，此处登记。
2. **多组织未落地**：实现侧刻意**不按 `org_id` 过滤**（与原 SQL 等价）；多组织卡落地时
   这个文件是唯一要加 `org_id = ?` 的地方。
3. **P2-36 才会让 `erp` 侧收益落地**：本卡不改变任何现有行为 —— `biz_employee` 的直连仍在 `erp`，
   本卡只是把接缝、实现、注册、闸门准备好（19 → 0 由 P2-36 完成）。
4. **crate 级无环不在本卡范围**：单 crate 下 `cargo tree` 表达不出模块边（见 §5.2），
   要等 P2-C 拆 workspace。
