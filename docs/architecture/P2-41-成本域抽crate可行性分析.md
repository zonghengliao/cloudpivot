# P2-41：抽 `erp-cost` 为独立 crate —— 可行性分析与卡面重切建议

> **结论先说**：按卡面「把 `erp-cost` 抽成 `backend/crates/cloudpivot-erp-cost/`」**做不成** ——
> 不是工作量问题，是 **Cargo 包依赖图必须无环**这一硬约束与当前代码的引用结构直接冲突：
>
> - `cost` 域有 **14 处代码级回边**来自应用 crate（`stockout` 10 / `period` 2 / `doc_reader_impl` 1 / `doc_reader_impl_tests` 1），
>   同时 `cost` 自身有 **15 处出边**指向应用 crate 的 25 个模块 —— 两边都指向对方 ⇒ 抽出即环；
> - 更根本的是：`cost` 与 `erp` 域内 **50 个兄弟节点同属一个强连通分量（SCC=51）**，
>   顶层业务域 **22 个同属一个 SCC**。SCC 内部任意切两半都必然产生 A→B 与 B→A。
>
> 因此本卡**没有搬任何生产代码**（搬了只有两种结果：编译期成环，或做成「域 crate 反向依赖整个
> 单体」的假拆分 —— 后者既拿不到 `cargo tree` 的依赖方向，也不可能让增量编译变快）。
> 本卡交付的是**可复现的度量工具 + 重切建议**，并把卡面改成可执行的切法：
> `scripts/verify/crate_split_readiness.py`（只读体检）+ 本文。
>
> **卡面阻塞原因**：`kanban_block(needs_input)` —— P2-C 的 18 张卡需要先改切法（层次切，而不是域切），
> 这属于排期决策，不是 worker 能自行拍板的实现细节。

---

## 1. 硬约束：Cargo 包图必须无环（实测，不是推理）

两个包互相 `path` 依赖时，`cargo` 在解析阶段就拒绝（实测于本机 cargo 1.94.1）：

```
$ cargo check            # a -> b, b -> a
error: cyclic package dependency: package `a v0.1.0 (.../a)` depends on itself. Cycle:
package `a v0.1.0 (.../a)`
    ... which satisfies path dependency `a` of package `b v0.1.0 (.../b)`
    ... which satisfies path dependency `b` of package `a v0.1.0 (.../a)`
```

所以「能否抽」只等价于一个问题：**抽走之后，留在应用 crate 里的代码是否还引用它**（回边）。
只要有一条代码级回边，抽出即环；除非先把那条引用**倒转成接缝**（port / 注册表，P2-B 的做法）。

## 2. 本卡目标的三个度量（真实输出）

### 2.1 规模

| 项 | 数 |
|---|---|
| `backend/app/src/business/erp/cost/` | **67 个 `.rs` / 50 676 行** |
| 其中真库用例（`*_db_tests.rs`，`#[cfg(test)]`） | 18 个 / 16 999 行 |
| 生产面（非 `_db_tests`） | 33 677 行 |
| 该域注册的 HTTP 路由（`#[nik::get/post/put/delete]`） | 36 条 |
| 整个 `erp` 域（对比：它是一个域，不是 18 个） | 353 个文件 / **296 388 行** |

### 2.2 回边（留在应用 crate 的代码 → `cost`）：**14 处代码级，4 个来源**

```
$ python3 scripts/verify/crate_split_readiness.py --domain cost
  回边（留在应用 crate 的代码 → 本域）：14 处代码级
    erp#root:doc_reader_impl        code=  1  business/erp/doc_reader_impl.rs:122
    erp#root:doc_reader_impl_tests  code=  1  business/erp/doc_reader_impl_tests.rs:29
    period                          code=  2  business/erp/period/mod.rs:3092, business/erp/period/period_items.rs:91
    stockout                        code= 10  business/erp/stockout/stockout_dto.rs:187,204,205,206,247,896…
```

读法（这 14 行各自要变成什么接缝）：

| 来源 | 用了 `cost` 的什么 | 倒转方式（P2-B 范式） |
|---|---|---|
| `stockout/stockout_dto.rs`（10 行） | `cost_object_dto::{STATUS_OPEN, STATUS_CLOSED, STATUS_CANCELLED, STATUSES, parse_uuid}` | 状态词表提到中立处（core 或 kernel crate），出库只认词表不认 `cost` |
| `stockout/stockout_handler.rs`（经 `super::super::cost::cost_object_model`） | `cost_object::find_by_id(...)`（真库查询 `biz_cost_object`） | 新增 `cost_object_port`（trait + 注册表），实现留在 `cost` 侧；先例：`business::shared::voucher_port`（P2-B 补） |
| `period/period_items.rs`（1 行）+ `period/mod.rs`（1 行，用例） | `cost_period_dto::{STATUSES, STATUS_*}` | 同上：状态词表上提 |
| `erp/doc_reader_impl.rs`（1 行）+ 其用例 | `cost_period_bridge`（`COST_SETTLE` 的 DocReader）+ `ErpDoc::CostSettle` 的 3 条 match 臂 | `cost` 侧自己实现 `flow::doc_reader::DocReader` 并由「域初始化器」自注册；`erp/doc_reader_impl` 的 `readers()` 减 1 项（16 → 15 + 域自注册 1） |

### 2.3 出边（`cost` → 应用 crate）：**15 个目标 / 169 行**

```
ext:impexp 7   voucher 7   erp#root:ledger 5   ext:flow 5   stockin 4   erp#root:model_base 3
erp#root:model_master 3   ext:hr 3   ext:changelog 3   erp#root:inventory_writer 3
erp#root:model 2   erp#root:fixture_hygiene 1（cfg(test)）   erp#root:doc_reader_impl 1   payable 1   report 1
```

这 169 行**不是**「搬的时候一起改路径」那么简单：`cost` 依赖的对象里，`voucher` / `ledger` /
`inventory_writer` / `stockin` / `payable` / `report` 全是**别的业务域**，`impexp` / `flow` / `hr` /
`changelog` 也是别的域。抽出后这些必须变成 `cloudpivot-erp-cost` 的**包依赖**，也就是说：
**`cost` 不在依赖图的底部，它在中间。** 一个「在中间」的包不可能只靠搬文件抽出来。

### 2.4 SCC：这才是致命的

```
$ python3 scripts/verify/crate_split_readiness.py --domain cost
  SCC：51 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（50）：asset, budget, contract, customer, expense, linkage, payable, period,
    price, purchase, receivable, report, sales, shipment, stockin, stockout, stocktake, transfer,
    voucher, year, erp#root:{approval_gate,cash_flow,code_policy,doc_reader_impl,dto,handler,
    income_statement,inventory_audit,invoice,ledger,model_master,opening,order_pdf,…}

$ python3 scripts/verify/crate_split_readiness.py            # 顶层域
  size=22：aiagent, announcement, attachment, changelog, docpdf, erp, flow, help, hr, impexp,
           masking, middleware, mutex, notice, pdf, permission, shared, storage, support, system, task, utils
```

读法：

- **`erp` 子域之间不是 DAG**。51 个节点塌成一个分量，意味着这 51 个节点**两两之间都存在有向回路**
  （不必直接互引，经传递即可）。`cost` 的直接互引伙伴只有 `erp/doc_reader_impl`
  （`cost → 它` 1 行、`它 → cost` 1 行）；其余 49 个是传递回路，例如
  `cost → stockin`（4 行）`→ … → stockout → cost`（`stockout` 有 10 行读 `biz_cost_object`）。
  所以**「每个 erp 子域抽一个 crate」这件事在上游就不成立**：
  P2-41 `erp-cost`、P2-42 `erp-purchase`、…… 逐个子域抽，第一张抽出后第二张就会顶在环上
  —— 无论先抽哪个，剩下的兄弟域里总有代码要 import 它。
- **顶层域之间也不是 DAG**。`erp → shared`（230 行）与 `shared → erp`（1 行，`shared/permission_alias.rs:111`）；
  `erp → task`（8 行）与 `task → erp`（17 行）—— 所以 `erp` 单独抽也成环，要切 23 处回边
  （`bootstrap.rs` 5 + `shared/permission_alias.rs` 1 + `task/mod.rs` 17）。
- 全仓**唯一**「零回边」的业务域只有 `dashboard` / `monitor` / `project`（erp 子域里是 `commission` / `depreciation`）——
  也就是说，按「域」切、按卡号顺序切，**第一张卡就落在 SCC 内部**。

## 3. 两条技术路线，各自的真实代价

### 路线 A（正解）：「内核 crate 先行」——域 crate 依赖内核，内核不依赖域

```
L0  cloudpivot-doc            零依赖词表（已有，P2-27/40）
L1  cloudpivot-kernel         shared + utils + business::shared（state_machine/scope/principal/separation/
                              cost_switch/test_helpers）+ 中立接缝（flow::doc_reader 注册表、hr::employee_reader、
                              taskflow、voucher_port…）
L2  cloudpivot-erp / -flow / -hr / -impexp / -task / -attachment / -storage / -notice / -pdf / -changelog …
                              每域一个包（**erp 只能整体一个包**：它内部就是 51 节点 SCC）
L3  cloudpivot-bin            装配层：依赖 L2 全部 + 内核；跑 `App::scan()`（inventory 路由自动收集）
                              + 「域初始化器」（各域自注册 DocReader / port 实现）
```

- **`L1` 必须先做**（原 P2-59 的内容，现在排在 18 张域卡之后 —— 顺序反了）。
  没有内核，域 crate 只能反向依赖单体：编得出来，但 `cargo tree -p <域>` 显示
  `<域> → cloudpivot(其余全部)`，**拿不到任何依赖方向信息，增量编译也不会变快**，属于假拆分。
- **`L3`（bin 拆包）是硬前置**：`app` 这个包现在同时是「lib + bin」。只要域 crate 依赖 `app`（lib），
  `app` 就不能再依赖域 crate；而 `app` 的 bin 必须链接域 crate（否则 36 条路由的 `inventory`
  收集项进不了最终链接单元 ⇒ 接口静默 404）。所以 bin 必须独立成包。
  代价与 P2-40 同级：`scripts/dev.sh`、`scripts/dev-up.sh` 的 `cargo run`（workspace 根多包时
  `cargo run` 需要 `-p`）、`scripts/verify/clean_checkout_gate.sh` 的 `cargo test -p cloudpivot --lib`、
  Dockerfile、AGENTS.md 的常用命令都要跟着改。
- 每域一张卡时，卡面必须带**该域的回边清单**（本脚本直接产出），否则抽卡的人会在 14~128 行
  回边上撞墙；`flow` 128 处、`notice` 68 处、`impexp` 40 处、`hr` 25 处、`changelog` 24 处、`erp` 23 处。

估算（按本仓实测规模）：L1 内核 ≈ 1 张大卡（`shared` 21 503 行 + `utils` + `business::shared`，
回边要先切）；L3 bin 拆包 ≈ 1 张（脚本/文档面与 P2-40 同量级）；L2 每域 1 张 ⇒ **实际是 12~14 张，
不是 18 张域卡 + 1 张收敛卡**，且 L1/L3 必须排在 L2 前面。

### 路线 B（过渡解）：「域 crate 反向依赖单体」

`cloudpivot-erp-cost` 依赖 `cloudpivot`（其余全部）。要落地必须先付三笔账：

1. 切掉本卡的 14 处回边（见 §2.2 表）—— 可做，P2-B 已有 3 个接缝先例；
2. bin 拆包（同上，硬前置）；
3. **`#[cfg(test)]` 夹具跨包不可见**：`cost` 的 18 个真库用例用了
   `crate::shared::test_helpers::{ensure_any_dept, ensure_any_user}`（15 处）与
   `erp::fixture_hygiene::SEED_ROLE_LOCK`（1 处），这两个模块都是 `#[cfg(test)]`（`fixture_hygiene.rs:30`
   是 `#![cfg(test)]`）—— 依赖包在非 test 配置下编译，这些符号根本不存在。要么把夹具特性化
   （`#[cfg(any(test, feature = "test-support"))]` + crate 侧 dev-dependencies 打开 feature），要么把
   18 个用例留在 `app`（那就等于没用例可跑，`cargo test -p cloudpivot-erp-cost` 是个空跑）。

**路线 B 不推荐**：付出 1+2+3 的代价后，得到的仍是「域 → 单体」的反向依赖，
计划要的两个收益（`cargo tree` 表达方向、增量编译下降）一个都拿不到，且会给后面 17 张卡
定下错误范式（第二张卡抽出后，第一张会顶在 `域A → cloudpivot(含域B)` + `域B → cloudpivot(含域A)`
的间接环上，继续做只能不断往单体里加接缝）。

## 4. 建议的重切（给排期用）

1. **P2-41 ~ P2-58 不要再按「域」编号**，改成：
   - `P2-C1`：`cloudpivot-kernel`（原 P2-59 提前；含 `shared` / `utils` / `business::shared` 与中立接缝）；
   - `P2-C2`：bin 拆包（`cloudpivot-bin`）+ 域初始化器（inventory 自注册）基建；
   - `P2-C3…`：按域抽 crate，**每张卡带本脚本产出的回边清单**；`erp` 是**一个**包（不是 18 个）。
2. 每张域卡开工前跑：
   ```bash
   python3 scripts/verify/crate_split_readiness.py --domain <域> --check   # 0=可抽，1=先解环
   ```
   `--check` 退 1 时，卡面必须先写「切哪几处回边」，否则这张卡注定做不完。
3. 想要「小步先赢」的话，现在真正零回边的只有 `dashboard` / `monitor` / `project`
   （erp 子域里 `commission` / `depreciation`）—— 可以先拿它们验证 bin 拆包与域初始化器，代价最小。

## 5. 本卡实际交付物

| 文件 | 说明 |
|---|---|
| `scripts/verify/crate_split_readiness.py` | 只读体检工具：顶层域 / erp 子域的 SCC、回边清单（含 `file:line`）、出边、`--check` 退出码、`--json`。不进门禁（排期工具，不是质量闸）。 |
| `docs/architecture/P2-41-成本域抽crate可行性分析.md` | 本文 |

**没有**交付 `backend/crates/cloudpivot-erp-cost/`：按上文两条路线，此刻建出来的只可能是
（a）编译期成环，或（b）反向依赖单体的空壳包 —— 两者都不该进仓库。

## 6. 复现（全部只读，无库依赖）

```bash
cd /opt/cloudpivot
python3 scripts/verify/crate_split_readiness.py                          # 顶层域全表 + SCC
python3 scripts/verify/crate_split_readiness.py --domain cost            # 本卡目标
python3 scripts/verify/crate_split_readiness.py --domain cost --check    # 退出码 1（旧口径：入边非 0）
python3 scripts/verify/crate_split_readiness.py --domain dashboard --check   # 退出码 0（旧口径对照，注意是假阳性）
python3 scripts/verify/crate_split_readiness.py --scope erp-sub --json   # 机器可读
```

## 7. 决策落定（2026-11-25 · 卡 `t_b287611c`，架构侧拍板）

**结论：P2-41 作废；P2-C 改「层次切」（15 卡），`cost` 留在 `cloudpivot-erp` 内部。**
卡序与每张卡的解环账见 `docs/planning/20261113-新卡清单.md` §P2-C。

本节同时更正本文早先的 3 处，全部是**度量口径**问题（不是结论反转）：

| # | 本文早先写的 | 实测（工作区修复后） | 原因 |
|--:|---|---|---|
| 1 | 顶层 SCC = 22 | **24** | `collect_nodes()` 里 `business/shared` 与 `src/shared`、`business/task` 与 `src/task` 同名，前者先写入、随即被 `TOP_KERNEL` 覆盖 ⇒ **这两个目录（14 384 / 27 688 行）从未进图**。已修 |
| 2 | 全仓最大回边源 = `flow` 128 行 | **`business/shared` 480 行**（18 个来源） | 同上。⇒ L1 内核更要第一个做：它是最大耦合枢纽，不是小助手 |
| 3 | 判据 = 「入边（回边）为 0 才可抽」 | **判据是出边**：出边全落在更下层才可抽。入边（app→域）是合法方向 | 见 §2.2 与 §3 路线 A：装配层必须依赖域 crate（否则 `inventory` 路由项进不了链接单元 ⇒ 静默 404）。旧判据把方向判反了：`dashboard` 入边 0 但出边 6，「可抽」是**假阳性** |

另需修正一处措辞（§2.4「逐子域抽在上游不成立」）：
**「按域抽」不是原理上不可能，而是每张域卡必须先付自己那一笔解环账。**实测 what-if：
把 `cost` 的 14 行回边全切掉后，`cost` 所在 SCC 从 37 个节点（仅代码边口径）降到 **1**，
且不再有节点指向它 —— 也就是说逐子域抽是可行的，只是要付 18 次。
之所以仍然不这么做：`erp` 内部 51 节点、296 388 行，逐子域抽合计上百行接缝 + 18 次搬迁，
而包边界对增量编译的主要收益来自「内核 / 装配层 / erp」这一级的分离（`erp` 占全仓 50%）——
**`erp` 一个包，内部继续用目录分层**，是投入产出比的选择，不是「不可能」。

