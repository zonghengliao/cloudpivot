# P2-34 · `flow` 去掉对 `erp` 的直接依赖（消边）

> 归属批次：P2-B 解环 · 卡 `t_ec2dac70` · 前置 P2-32（接缝）/ P2-33（实现 + 注册）
> 状态：**`business::flow → business::erp` 这条边已不存在**（代码 0 / 注释 0）

## 1. 结论

P2-32 立接缝、P2-33 在 ERP 侧实现并注册，本卡把 `flow` 里剩下的**全部** 135 行直连摘干净：

```text
改前：flow ──131 代码 / 4 注释──→ erp      （双向 = 环）
P2-32：flow ──trait/注册表──→ (抽象)                     ← 接缝就位，仍直连 135
P2-33：                        erp ──实现 + 注册──→ 抽象   ← 实现在业务域，启动期注册
P2-34：flow ──注册表 / 订阅表──→ (抽象)                  ← 直连归零（本卡）
```

本卡的取舍口径只有一条：**代码在哪一层就该住哪个域**。135 行里只有 99 行是「按单据取状态 / 回调」，
能塞进 `DocReader`；剩下 36 行各自有归属 —— 把它们硬塞进一个 trait 会把「单据查询」做成第二个上帝对象
（P2-32 §2 已定此口径）。所以本卡按类各归其位，而不是「一把全塞」。

## 2. 五类处置（36 行怎么消化）

| 类别 | 原直连 | 处置 | 为什么这么处置 |
|:--|:--|:--|:--|
| 终态回调扇出 / 内控闸 / 制单人分派（**99 行**） | `handler.rs` 4 处扇出、`approve_task` 14 道闸、`internal_control::of_biz` 16 段 if/else | 换成 `flow::doc_reader` 的注册表调用（`dispatch_settled` / `guard_self_approval_for_task` / `internal_control_creator_of_code`） | P2-32/33 定好的接缝，本卡只换调用点 |
| `separation`（内控开关，10 行） | `self_approval_max_amount` / `enabled(Duty)` / `is_decimal_literal` / 配置键 | 整块搬到 **`business::shared::separation`** | 那是**配置读取**，不是单据查询；`flow` 的内控排除与 `erp` 的三个职责闸读同一份键，留在业务域 = 边永远摘不掉 |
| 联动规则 / 凭证快照 / `POSTED_RECEIVED_SQL`（11 行） | `flow/linkage/*`（约 9.5 千行）+ `flow/instance_linkage.rs` | 整块搬到 **`business::erp::linkage`**（含 `flow_instance_trigger`）；`flow` 侧只留 `flow::instance_hooks` 订阅接缝 | 触发点与动作**全部**是 ERP 单据（取凭证 / 写库存 / 改状态）；实测 20 个 ERP 文件用它、`flow` 只剩 1 个类型引用 —— 它本来就是 ERP 的代码 |
| 审批闸 DTO（跳过审批的判据，4 行） | `erp::approval_gate::{SkipApprovalReq, ApprovalGate, authorize_missing_flow, ensure_not_skipped}` | 通用闸门搬到 **`flow::approval_gate`**，ERP 侧 `pub use` 再导出（35 个调用点一行不改） | 判据回答的是「审批这件事」的问题，`flow::edge_admin` 与 `hr` 也用同一份；留 ERP 里那两个域就得直连 ERP |
| 单据 PDF 公共件（2 行） | `erp::doc_pdf_model` / `erp::doc_pdf_render` | 独立成域 **`business::docpdf`** | 被 `erp` / `flow` / `task` **三个**域共用（`task` 原先也在直连 ERP）；不塞进 `business::pdf` 是因为 `pdf` 的边界是「不读业务数据的渲染能力」 |

**两条新接缝**（本卡新增，`flow` 侧零业务域引用）：

| 接缝 | 作用 | 实现方 |
|:--|:--|:--|
| `flow::doc_reader::guard_self_approval_for_task` | 审批入口只有 `task_id` 时，把已注册实现的闸过一遍（原来逐个桥调一遍，判据同源） | `erp::doc_reader_impl`（16 受控 + 1 调拨） |
| `flow::instance_hooks::InstanceSettledHook` | 「审批落终态」的订阅表：`handler` 4 处 + `timeout` 1 处只扇出，不点名任何业务域 | `erp::linkage::flow_instance_trigger`（启动期注册） |

## 3. 三条等价性（改造不许改变行为）

1. **受控集合仍是恰好 16 个**：`flow::internal_control::GOVERNED_CODES` 是**显式常量**，不是
   `doc_reader::is_registered(..)`。理由：调拨单（`STOCK_TRANSFER`）注册了终态回调但**不做内控排除**，
   拿「有没有实现」当「受不受控」会给它凭空加一道闸 = 多拦一批人。
   用例：`governed_codes_are_the_sixteen_approval_lines`、`supported_set_matches_governed_branches`。
2. **`STOCK_IN_CANCEL` 仍然不被拦**：P2-33 §2.4 记录的既有缺口原样沿用（实现委派同一个桥函数，
   桥内部只认 `STOCK_IN_POST`）—— 补闸 = 改变生产行为，不属于解环。
3. **扇出仍是「全部调一遍」**：`dispatch_settled` 调**所有**已注册实现（不是按 `biz_type` 只调一个），
   与改造前「历史实例 `biz_type` 与回调不匹配时靠另一条桥捞回来」一致。
   调拨单的回调因此改成**注册表里的第 17 个实现**（`on_settled` → `transfer_approval_settle`），
   `handler` 里那两行 `task_bridge::on_transfer_settled` 随之删除 —— 不删会跑两遍。

## 4. 依赖边证据

### 4.1 `cargo tree` 为什么看不到这条边（如实说明，同 P2-31/32/33 口径）

后端仍是**单 crate**（workspace 化 = P2-C），`erp` / `flow` 是**模块**不是 package：

```text
$ cargo tree --prefix none --depth 1
cloudpivot v0.1.0 (/opt/cloudpivot/backend)
ammonia v4.2.0
async-trait v0.1.92 (proc-macro)
axum v0.8.9
calamine v0.36.1
chromiumoxide v0.9.1
cloudpivot-doc v0.1.0 (/opt/cloudpivot/backend/crates/cloudpivot-doc)
...
$ cargo tree | grep -c "business::"
0
```

树里**没有任何** `business::erp` / `business::flow` 节点 —— 这条边在这个粒度上无法表达。
模块级边用仓库自带的只读工具取证（Tarjan SCC + 按域聚合，含注释与 intra-doc link）。

### 4.2 改造后（绿）

```text
$ python3 scripts/lint/module_cycle_report.py --edge business::flow business::erp
源码根：/opt/cloudpivot/backend/app/src
扫描面：770 个 .rs / 节点 21 个 / 跨域有向边 62 条 （口径：仅 business::*）

边 business::flow → business::erp：**不存在**（该域内无任何 crate:: 引用指向 business::erp）

$ python3 scripts/lint/module_cycle_report.py --from business::flow
business::flow 的出边：7 条
  → business::changelog  (代码 3 / 注释 0)
  → business::docpdf     (代码 2 / 注释 0)      ← P2-34 新增的公共件域（不是 ERP）
  → business::hr         (代码 10 / 注释 0)
  → business::notice     (代码 17 / 注释 0)
  → business::pdf        (代码 2 / 注释 0)
  → business::shared     (代码 26 / 注释 4)
  → business::task       (代码 7 / 注释 1)

$ python3 scripts/lint/module_cycle_report.py --edge business::erp business::flow
边 business::erp → business::flow：存在（代码 98 / 注释 3）    ← 同向边，现在只剩这半边
```

### 4.3 改造前（红，同一工具同一命令）

```text
边 business::flow → business::erp：存在（代码 131 / 注释 4）
```

### 4.4 闸门（进 CI 的那一条）

`flow::doc_reader_tests::flow_erp_reference_budget_is_not_exceeded` 的基线 **135 → 0**
（`FLOW_TO_ERP_REFS_BASELINE = 0`），判据从「不许涨」改成「必须相等」。
**红绿反证**（在 `flow/timeout.rs` 顶部插一行注释形式的直连，跑完即还原）：

```text
① 闸门用例 exit = 101
   panicked at src/business/flow/doc_reader_tests.rs:614
   test result: FAILED. 0 passed; 1 failed
② 模块级报告： 边 business::flow → business::erp：存在（代码 0 / 注释 1）
反证结论： 红绿成立（两个工具都抓得住）
```

> 注释形式也判红是刻意的：intra-doc link 是 rustdoc 依赖，注释回边一样算（P2-31 的口径）。

## 5. 验证证据（2026-11-15）

| 判据 | 命令 | 实测 |
|:--|:--|:--|
| 编译 | `cargo check --lib` | `Finished dev profile`，**零 warning** |
| 静态检查 | `cargo clippy --all-targets -- -D warnings` | `Finished` 1m40s，**零告警** |
| 全量离线单测 | `cargo test --lib -- --test-threads=1` | **5620 passed / 0 failed** |
| 闸②（数据范围口径） | `python3 scripts/lint/scope_gate.py --root .` | PASS（153 条跳过清单逐条一致） |
| 模块级无环（本边） | `module_cycle_report.py --edge business::flow business::erp` | **不存在** |
| 燃烧率闸门 | `cargo test --lib business::flow::doc_reader_tests` | 通过（0 条 ≤ 基线 0） |
| 真库用例 · 注册表 / 内控 / 联动 / 调拨 | `ERP_DB_URL=…cargo test --lib business::erp::doc_reader_impl`、`business::erp::linkage`、`business::erp::transfer` | **16 / 108 / 101 passed，0 failed** |
| 真库用例 · 内控会签（独占进程） | `P0FIX_DB_URL=…cargo test --lib business::flow::internal_control` | **16 passed，0 failed** |
| 真库用例 · flow 全段 | `ERP_DB_URL=…cargo test --lib "business::flow::" --skip business::flow::internal_control::db_tests` | **470 passed，0 failed** |

### 5.1 一处**必要**的测试夹具调整（不是放宽断言）

`business::flow::internal_control` 的 9 条真库用例原先靠「`of_biz` 直接调
`purchase_bridge::internal_control_creator`」拿到制单人。改造后制单人由注册表提供，
而**这组用例不跑 `bootstrap`**（注册表为空）⇒ 制单人取不到 ⇒ 排除不发生 ⇒ 7 条用例红
（真库实测复现，不是推测）。

处置：夹具在 `Scenario::build` 里装一个「制单人 = 夹具造的那个人」的 `PO_APPROVE` 假实现
（`install_creator_reader`）。断言面（`of_biz` / `partition` / 引擎发起与投票）**一条没减**；
真实实现的委派由 `erp::doc_reader_impl_tests`（16 条真库用例）与 `erp::purchase` 的用例覆盖。
**生产不走这条路**：`bootstrap` 装的是 16 个真实实现 + 调拨单共 17 个。

### 5.2 顺手修掉的一处门禁欠账

`scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST` 里，P2-32/P2-33 新增的两个测试面文件
（`business/flow/doc_reader_tests.rs`、`business/erp/doc_reader_impl_tests.rs`）一直没登记，
闸② 每次都会红（「多一个少一个都不放行」）。本卡搬动文件时必须改这张表，一并补齐。

## 6. 改动文件

**搬移（`git mv`，内容基本原样）**

| 原位置 | 新位置 |
|:--|:--|
| `erp/separation.rs` | `business/shared/separation.rs` |
| `erp/doc_pdf_model.rs` / `erp/doc_pdf_render.rs` | `business/docpdf/doc_pdf_model.rs` / `doc_pdf_render.rs`（新域） |
| `flow/linkage.rs` + `flow/linkage/*.rs`（9 个） | `erp/linkage.rs` + `erp/linkage/*.rs` |
| `flow/instance_linkage.rs` | `erp/linkage/flow_instance_trigger.rs` |
| `flow/instance_linkage_db_tests.rs` | `erp/linkage/flow_instance_trigger_db_tests.rs` |
| `flow/task_bridge.rs` 的调拨回调段 | `erp/transfer/transfer_approval_settle.rs`（新） |

**新增**

- `flow/instance_hooks.rs`（订阅接缝：trait + 注册表 + 扇出）
- `flow/approval_gate.rs`（通用闸门，从 ERP 侧搬来；ERP 侧 `pub use` 再导出）
- `docpdf/mod.rs`（新域的模块文档与边界说明）

**改动**

- `flow/handler.rs`：4 处扇出 → `dispatch_settled`，14 道闸 → `guard_self_approval_for_task`，
  2 处 `task_bridge::on_transfer_settled` 删除（改为注册表第 17 个实现）
- `flow/internal_control.rs`：16 段 if/else → `GOVERNED_CODES` + `internal_control_creator_of_code`
- `flow/doc_reader.rs`：+`guard_self_approval_for_task`，模块注释口径更新到「已归零」
- `flow/doc_reader_tests.rs`：基线 135 → 0，+2 条用例（task 级闸 / 受控码表自检）
- `erp/doc_reader_impl.rs`：+第 17 个实现 `StockTransfer`（表 `biz_stock_transfer`）
- `erp/approval_gate.rs`：通用闸门移出 + `pub use` 再导出
- `bootstrap.rs`：+`register_flow_hooks()`（订阅者注册，prod 失败 `exit(1)`）
- `scripts/lint/scope_gate.py`：`SKIP_MANIFEST` 随文件搬移更新 + 补两个漏登记项
- 各域的 import 路径（约 40 个文件，机械改动）

## 6.1 如实沿用的一处既有缺口（**不修**，只记录）

调拨审批的「撤回 / 行政终止」不会把调拨单从 `PENDING` 打回草稿：`settle_transfer` 只认
`APPROVED` / `REJECTED`，其余状态直接 `return None`（**与改造前逐字一致** —— 原来
`handler` 的 withdraw / terminate 分支就根本没调这个回调，现在虽然进了扇出，函数体仍然是 no-op）。

补它 = 多推进一批单据 = 改变生产行为，不属于「解环」。要修请单独开卡（并把
`transfer_approval_settle` 的状态映射一并补上 `REJECTED | WITHDRAWN | CANCELLED`）。

## 7. 没做的事（如实说明）

- **`erp → flow` 那条同向边还在**（98 代码 / 3 注释）：它是「业务单据提交审批 / 读实例」的正常方向，
  不是本卡的目标；环的另一半已经断掉（`flow → erp` 归零 = 两域之间不再互为可达的那种形态，
  但 ERP 仍依赖 flow，见 `module_cycle_report.py --from business::erp`）。
- **整体 SCC 仍有 10 个域**（`aiagent / announcement / changelog / erp / flow / hr / impexp /
  masking / notice / task`）：本卡只摘 `flow → erp` 这一条，其余边属别的卡。
- **`STOCK_TRANSFER` 的 `doc_status` 是新写的第 17 行表驱动查询**（`biz_stock_transfer`）：
  它是注册的副作用（trait 要求实现 `doc_status`），行为上只让审批详情多显示一个单据状态，
  不参与任何判据（`governed` 判定读的是 `GOVERNED_CODES`，不是注册表）。
