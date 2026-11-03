# 拆分超千行文件的方法论（TD-1 定稿）

> 适用对象：本仓 `backend/src/**` 与 `frontend/src/**` 里**单个文件超过 1000 行**的模块。
> 本文件是第一批（`erp/model.rs`、`erp/stockin_model.rs`、`erp/transfer_model.rs`）的
> **复盘 + 可复制流程**，后续批次照做，不要各自发明拆法。

## 0. 现状（以 `HEAD` 实测为准，不是估算）

```bash
# 统计口径：HEAD 里 > 1000 行的源文件（backend/src + frontend/src）
for f in $(git ls-tree -r --name-only HEAD | grep -E '^(backend/src|frontend/src)/.*\.(rs|ts|vue)$'); do
  n=$(git show "HEAD:$f" | wc -l); [ "$n" -gt 1000 ] && echo "$n $f";
done | sort -rn
```

实测结果：**95 个**（后端 Rust 73 个、前端 TS/Vue 22 个）。分布：

| 域 | 文件数 | 代表 |
|---|---|---|
| `backend/erp` | 26 | `stockin_handler.rs` 7427、`transfer_handler.rs` 4390、`dto.rs` 3553 |
| `frontend/views` | 18 | `erp/transfer/index.vue` 2181、`task/detail.vue` 2073 |
| `backend/flow` | 11 | `model.rs` 4452、`graph.rs` 4234、`engine.rs` 3626 |
| `backend/task` | 8 | **`model.rs` 7604（全仓最大）**、`handler.rs` 5593 |
| `backend/impexp` / `storage` / `announcement` / 其余 | 32 | `impexp/handler.rs` 1768、`storage/store.rs` 2065 |

第一批已拆的 3 个文件（拆分前 → 拆分后）见 §6。

---

## 1. 三条原则

### ① 按「职责」拆，不按「行数」拆

```
❌ 前 1000 行一个文件、后 1000 行另一个
   → 拆完仍然是「一个陌生人要读 5 个文件才知道一次过账做了什么」，
     行数达标，认知成本一点没降；而且下一个人加函数时不知道往哪放。
✅ 按「这个函数在干什么」归类（查询 / 写入 / 校验 / 测试）
   → 想看「过账写了哪些表」只看一个文件；想查「为什么这个单不能作废」只看另一个。
```

判据很简单：**新人带着问题来的时候，会去哪个文件找答案？** 那个文件就该存在。

### ② 保持公开 API 不变（facade 重导出）

```
拆完后 use crate::business::erp::stockin_model::post_stock_in 的调用方一行不改。
```

原文件保留为 **facade**：函数体全部搬走，只留常量/状态机 + `pub use`。
这不是「为了好看」，是为了把**改动面收敛到一个文件族**——调用方零改动，
回归测试才敢说「只搬不移」。

### ③ 测试跟着实现走

`#[cfg(test)] mod tests` 不许全堆在原文件（否则原文件仍然 800+ 行）。
测试随被测实现落到对应部件：`stockin_write` 的用例归 `stockin_tests.rs`，
纯函数用例同样归测试部件（**不留在 facade**，facade 只留 `mod tests;` 挂载点）。

---

## 2. 四类职责的划分标准

| 类别 | 判据（满足任一条即归这类） | 典型函数 | 落点命名 |
|---|---|---|---|
| **底座 base** | 纯函数；不含 SQL；被其它三类**共同**引用（取号、状态机、行结构、列清单、舍入策略、`parse_id`） | `normalize_page`、`can_transition`、`CostBasis::of`、`format_stock_in_no` | `<模块>_base.rs` |
| **查询 query** | 只读（`SELECT`）；只吃 `db()` 或 `&impl SqlExecutor` 但不写；返回 `DbResult<...>` 的行/页数据 | `find_stock_in`、`items_of`、`stock_in_page`、`source_lines_of` | `<模块>_query.rs` |
| **写入 write** | 有任何 `INSERT/UPDATE/DELETE`；或在事务里编排多表；或触发下游联动（回写采购单、生成凭证） | `create_stock_in`、`post_in_tx`、`cancel_in_tx`、`apply_cost` | `<模块>_write.rs`（过大再按链路切 `_cancel` / `_purchase` / `_settle`） |
| **校验 validate** | 只判断「能不能做」，不落库；失败返回业务错误码（400/409）供 handler 直接用 | `validate_cancelable`、`check_serial_not_shipped` | `<模块>_validate.rs` |
| **测试 tests** | 所有 `#[test]` / `#[tokio::test]` | — | `<模块>_tests.rs` |

边界判例（这些最容易吵，直接照判）：

- **「过账前算一遍库存够不够」= 校验**，但**「过账时真的扣库存」= 写入**。
  同一个业务动作里，判断与执行分家；校验函数不许写库。
- **「作废前置测算（短溢/缺流水/序列号已出库）」= 校验**；
  **「作废反冲成本与库位、写作废流水」= 写入（`_cancel` 子链路）**。
- **取号（`bump_seq`）虽然有 `UPDATE`，仍归底座**：它是所有单据共用的原语，
  放进任一业务链路都会被另一条链路反向依赖。
- **回写采购单已收数量 = 写入（`_purchase` 子链路）**：它属于「入库的副作用」，
  不属于「入库本身」，单独成文件后，采购联动改口径时不用碰过账主链路。

---

## 3. facade 重导出模式

```rust
// stockin_model.rs —— 拆完只剩这些（124 行）
//! <原文件头注释一行不改：表结构、过账口径、SCOPE-EXEMPT 全部留在 facade>

pub use super::stockin_base::*;      // 常量 / 状态机 / 行结构 / 取号
pub use super::stockin_cancel::*;    // 作废链路
pub use super::stockin_purchase::*;  // 采购联动
pub use super::stockin_query::*;     // 只读
pub use super::stockin_validate::*;  // 校验
pub use super::stockin_write::*;     // 写入

#[cfg(test)]
#[path = "stockin_tests.rs"]
mod tests;                            // 测试挂载点：路径指向测试部件
```

配套（`erp/mod.rs`）：部件模块一律**私有** `mod`，只有 facade 对外：

```rust
mod stockin_base;      // 不写 pub，外面只能走 stockin_model::
mod stockin_query;
mod stockin_write;
// ...
```

### facade 模式的四个坑（都实测踩过，不是理论）

1. **私有 `use` 不随 `pub use *` 传递。**
   测试原先写在 `model.rs` 内部，`use super::*` 同时拿到「父模块的项」和「父模块自己的
   import」。搬到独立文件后只剩前者，于是 `db()` / `Uuid` / `Decimal` / `TypeMapper`
   全部「not found」（第一批一上来 79 个编译错误，几乎全是这一类）。
   → **规则**：每个部件文件自带完整 import 头（照抄原文件 import 块），不依赖父模块。

2. **同名常量会被框架 prelude 抢走，且静默改变语义。**
   `nik::prelude` 也导出 `DEFAULT_PAGE_SIZE = 10` / `MAX_PAGE_SIZE = 1000`，
   而 ERP 口径是 20 / 200。拆分前测试模块在 `model.rs` 内部，局部常量优先；
   拆成独立文件后两条 glob 同名，解析落到框架那份 ——
   `normalize_page_clamps_bounds` 当场从绿变红（断言 `(1,20) == (1,10)`）。
   → **规则**：部件文件若同时 `use nik::prelude::*` 和 `use super::*`，
   所有**同名常量/类型**必须显式 `use crate::...::xxx::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};` 钉死。

3. **跨部件互调的私有项必须放宽到 `pub(super)`。**
   `ids_array_param` / `parse_id` / `page_of` / `find_by_id` 原来在 `model.rs` 里是私有
   `fn`，被同文件的查询函数直接调用；搬到 `model_base` 后 `model_master` 调不到。
   → **规则**：可见性只允许 `fn` → `pub(super) fn`（crate 内、不进公开 API），
   **签名（参数/返回类型/函数名）一个字都不许改**；`pub(super)` 是本方法论唯一允许的可见性改动。

4. **测试模块的挂载方式必须写在 facade 里，不能写进 `erp/mod.rs`。**
   `#[cfg(test)] #[path = "x_tests.rs"] mod tests;` 放在 facade 下，
   `use super::*` 才指向 facede 的重导出；放进 `erp/mod.rs` 会变成两份测试、且作用域错位。

---

## 4. 一个函数该放哪个文件：决策树

```
函数里有没有 INSERT/UPDATE/DELETE？
├─ 有 ──► 它属于哪条业务链路？
│          ├─ 建/改/删/过账主链 ──────────► <模块>_write.rs
│          ├─ 作废（反冲）链路 ───────────► <模块>_cancel.rs
│          ├─ 对下游单据的联动（回写/生成）─► <模块>_purchase.rs / _settle.rs
│          └─ 取号原语（全单据共用）─────► <模块>_base.rs
└─ 没有
   ├─ 有 SELECT ──► 它在回答「这张单/这批数据是什么样」吗？
   │               ├─ 是 ─► <模块>_query.rs
   │               └─ 否（是判据：够不够/能不能）─► <模块>_validate.rs
   └─ 无 SQL
      ├─ 纯计算/格式化/状态机 ──► <模块>_base.rs
      ├─ 判断请求是否合法（返回 400/409）──► <模块>_validate.rs
      └─ #[test]/#[tokio::test] ──► <模块>_tests.rs
```

补充两条硬规矩：

- **一个函数只落一个文件**，不许「为了少写 import」复制到两个地方。
- **不许合并函数**（把三个小函数揉成一个「更通用」的）。拆分的目的是降认知成本，
  合并会把它加回去，而且会让「只搬不移」无法用 diff 证明。

---

## 5. 落地流程（4 步，每步都要有工具输出）

```bash
cd backend

# ① 写 plan（谁去哪个文件，逐项点名，不许按行号切）
python3 ../scripts/refactor/split_module.py --plan ../scripts/refactor/plans/stockin_model.json --dry-run

# ② 执行（生成部件文件 + 把原文件改写成 facade）
python3 ../scripts/refactor/split_module.py --plan ../scripts/refactor/plans/stockin_model.json

# ③ 编译 → 补 import 头 → 再编译（/tmp 里看全量错误，别只 tail）
cargo check --all-targets

# ④ 三道验收（缺一不可）
cargo test --lib 2>&1 | grep '^test result:'
cargo clippy --all-targets -- -D warnings 2>&1 | grep -E '^(warning|error)' | head
wc -l src/business/erp/<模块>_*.rs
```

拆分器 `scripts/refactor/split_module.py` 的约束（写在脚本头注释里，改脚本先读它）：
**只搬不移**——不重写函数体、不改 SQL、不改签名；唯一的文本改动是
私有项 `fn` → `pub(super) fn`。plan JSON 用「项名 → 部件」映射，不是行号区间。

**用 diff 证明「只搬不移」**（第一批的实证手段，后续批次照做）：

```python
# A. 条目集合必须完全相同（fn/struct/enum/const/static/type/trait 多重集）
#    第一批：model 174 项 / stockin 117 项 / transfer 116 项，MISSING=none ADDED=none
# B. SQL 字面量多重集必须完全相同
#    第一批：model 51 / stockin 57 / transfer 49 条，missing=0 added=0
```

这两条比「看起来一样」可靠：条目集合抓「函数被漏搬/被合并」，
SQL 多重集抓「顺手改了 WHERE 或列清单」。

---

## 6. 第一批实测结果（TD-1，2026-11-02）

| 文件 | 拆分前 | 拆分后 facade | 部件（行数） | 目标 <1500 |
|---|---|---|---|---|
| `erp/model.rs` | 2975 | **80** | `model_base` 104 / `model_master` 1202 / `model_order` 329 / `model_inventory` 452 / `model_tests` 906 | ✅ |
| `erp/stockin_model.rs` | 2859 | **124** | `stockin_base` 598 / `stockin_query` 316 / `stockin_write` 767 / `stockin_cancel` 670 / `stockin_purchase` 234 / `stockin_validate` 98 / `stockin_tests` 175 | ✅ |
| `erp/transfer_model.rs` | 3035 | **70** | `transfer_base` 641 / `transfer_query` 345 / `transfer_write` 532 / `transfer_settle` 1016 / `transfer_scan` 254 / `transfer_tests` 286 | ✅ |

> 拆分前行数是 `git show HEAD:<file> | wc -l` 的实测值（2975 / 2859 / 3035）。
> 卡面写的 2727 / 2930 / 2984 与实测略有差异（卡面是评审时在工作树上量的，
> 当时工作树混有其它卡的改动）；**以 HEAD 实测为准**，目标线 `< 1500` 两者都满足。

保真度与验收（全部为真实输出）：

- 条目集合：`model` 174 / `stockin_model` 117 / `transfer_model` 116，**MISSING=none、ADDED=none**；
- SQL 字面量：51 / 57 / 49 条，**missing=0、added=0**；
- 用例：26 / 8 / 10 条，拆分前后**用例名逐一相同**（合计 44 → 44）；
- `cargo test --lib -- --test-threads=1`（干净隔离库 `cloudpivot_td1e`，136 迁移、跑一遍）：
  **1892 passed / 0 failed**，`finished in 741.63s`；
- `cargo clippy --lib -- -D warnings`：**0 warning**；
  `cargo clippy --all-targets -- -D warnings`：拆分前 / 拆分后**错误位置集合完全相同（各 24 处）**
  → 本次拆分新增 0 条（既有 24 处在 `tests/testd/*` 与 `flow/internal_control.rs` 测试模块，见 §9③）；
- 调用方零改动：`handler` / `*_bridge.rs` 里的 `use ...::model::xxx`、`use ...::stockin_model::xxx`
  一行未动（`git diff --stat` 里这些文件没有任何改动）。

---

## 7. 后续 92 个文件的拆分计划（按风险排序）

风险 = 改动面 × 回归代价。评分四个维度，各 1–3 分：

| 维度 | 1 分（低） | 2 分 | 3 分（高） |
|---|---|---|---|
| 层 | `model` / 纯 DTO | `handler` / `bridge` | `engine` / `graph` / 状态机 |
| 库依赖 | 无 SQL | 只读 SQL | 多表写入 + 事务编排 |
| 测试密度 | 无真库用例 | 有真库用例但不串行敏感 | 真库用例 + 全局互斥/夹具卫生 |
| 引用面 | 仅本模块 | 跨 2 个模块 | 跨 3+ 模块 / 被路由宏注册 |

| 批次 | 内容 | 文件数 | 主要风险点 | 前置 |
|---|---|---|---|---|
| **P2（3 分档）** | `erp/payable_payment.rs` 3877、`erp/opening.rs` 2824、`erp/year_end.rs` 2697、`erp/cash_flow.rs` 2562、`erp/report.rs` 2500、`erp/period.rs` 2415、`erp/income_statement.rs` 2017、`erp/voucher.rs` 2697 — 全是财务口径、报告主体 | 8 | 「只搬不移」证明要更严：报表口径的 SQL 多重集必须逐条对齐；这些文件多含**同库互斥用例**（`DB_LOCK`），拆分后测试文件变多，串行保护不能丢 | 第一批方法论 + 拆一个跑一遍全量 |
| **P3** | `erp/dto.rs` 3553、`flow/dto.rs` 2212、`task/dto.rs` 1751 — 按「实体」切（每实体一文件），不按「请求/响应」切 | 3 | DTO 被前端契约直接引用 → 字段名/`serde` 属性动一个都是**前端契约变更**；只允许搬，禁止顺手统一命名 | P2 完成 |
| **P4** | `erp/handler.rs` 3443、`erp/purchase_handler.rs` 2285、`erp/serial_handler.rs` 2808、`storage/handler.rs` 2548、`announcement/handler.rs` 2308、`impexp/handler.rs` 1768 | 6 | handler 挂**路由宏 + 权限码**，拆分后 `authz_tests::o13_every_write_route_requires_permission` 是唯一护栏 → 每拆一个立刻跑该测试；权限码字符串一个字不许动 | P3 |
| **P5（最高风险）** | `task/model.rs` **7604**（全仓最大，先拆它反而最稳：同型 model 层）、`task/handler.rs` 5593、`flow/model.rs` 4452、`flow/graph.rs` 4234、`flow/engine.rs` 3626、`flow/handler.rs` 3138、`flow/task_bridge.rs` 2996 | 7 | 审批引擎是**全系统横切**：`flow/model` 动一个常量可能让所有单据的审批失效。必须先补「审批全链路 e2e」再拆，否则拆完无回归证据 | 独立分支 + 全量 e2e |
| **P6** | 剩余 `backend`（`aiagent/hermes.rs` 3173、`help/model.rs` 1951、`announcement/model.rs` 2267、`storage/store.rs` 2065、`shared/*`、`menu/permission/auth/role` 等 ~20 个） | ~20 | 平台层，被业务模块广泛引用；`shared/` 里的工具函数拆分要防「同名 re-export 冲突」（见 §3 坑 2） | P2–P5 任一档完成即可并行 |
| **P7（前端）** | 22 个 TS/Vue：`api/business/erp/index.ts` 3579、`components/IconSelect/icons.ts` 2378、`views/business/erp/{transfer,stockin,purchase}/index.vue` | 22 | 拆法**不同**：`.ts` 按资源切（每种单据一个文件，`export * from` 聚回 `index.ts`）；`.vue` 按「列表 / 表单 / 抽屉 / 组合式函数」切（`useXxxList()` 抽到 `composables/`），**模板不重排** | 需要 `pnpm typecheck` + 页面自测 |

批次总量：**8 + 3 + 6 + 7 + ~20 + 22 = 66 个**，其余 92 − 66 ≈ 26 个是
`backend` 里 1000–1700 行的中号文件，随所在模块的批次顺手拆（不需要单独批次）。

每条批次的门槛与第一批一致，不打折：
`条目集合相同` + `SQL 多重集相同` + `cargo test --lib 全绿` + `clippy -D warnings 0`。

---

## 8. 反模式清单（评审直接打回）

1. 按行号切（「前 1000 行 / 后 1000 行」）。
2. 拆完改函数签名（参数顺序、返回类型、`&T` → `&mut T`），哪怕「更合理」。
3. 拆完顺手统一命名 / 抽 helper / 合并重复 SQL（这是重构不是拆分，必须另开卡）。
4. 测试留在原文件，说「反正还能跑」。
5. 部件模块写 `pub mod`，让外部绕过 facade → 公开 API 立刻出现第二条路径。
6. 只跑 `cargo check` 就说拆完了（check 不跑用例、不查 clippy 警告）。
7. 拿 `cargo test` 在**没设 `ERP_DB_URL`** 的环境跑出「全绿」——真库用例会静默 SKIP，
   1892 条只用 1 秒就「通过」，这种绿是假绿（第一批实测踩到，必须核对耗时与 `SKIP:` 输出）。

---

## 9. 本机验证环境的三个坑（第一批踩过，后续批次直接绕）

### ① 假绿：没连库的「全绿」

`erp` 的真库用例统一走 `ensure_db()`：**没设 `ERP_DB_URL` 就打印 `SKIP:` 并直接 return**。
结果是 1892 条「全绿、1.04 秒」。判据：**真库全量跑完约 12–13 分钟（实测 764s）**，
看到几秒钟就结束的「全绿」，一律当作没跑。

### ② 假红：同一个库跑第二遍，`cash_flow::tests::r3_real_config_covers_all_accounts` 会红

`voucher_query` 的夹具会往 `biz_account` 插 `P2D1A` / `P2D1B` 两个科目且不回收；
`cash_flow` 的 `r3` 断言「库里每个科目都有现金流分类行」——上一轮残留的两个科目没有分类行，
于是第二轮必红（`report.rs` 的同类用例已经把这种残留当合法输入，`cash_flow` 这条更严）。
→ **规则**：验收要么用**干净库跑一遍**，要么先 `DELETE FROM biz_account WHERE account_code LIKE 'P2D1%'`
再跑第二遍；否则会把**别人的夹具残留**记成你这次拆分的账。这条与本卡拆分无关（`cash_flow.rs` 一行未动）。

### ③ clippy `--all-targets` 在 HEAD 上本来就不为 0

`HEAD` 的 `cargo clippy --all-targets -- -D warnings` 有 **24 处**既有告警
（`tests/testd/impexp.rs`、`tests/testd/linkage.rs`、`tests/testd/mutex_help_ai.rs`、
`tests/testd/project_log.rs`、`src/business/flow/internal_control.rs` 的测试模块）。
→ **判据**：不要看「是否 0」，要看「**拆分前 vs 拆分后错误集合是否完全相同**」
（第一批实测：两边 24 处、位置逐条相同 → 拆分新增 0 条）；
lib 本体用 `cargo clippy --lib -- -D warnings` 单独把关（第一批：0 warning 通过）。

### ④ 长命令会被 180 秒切断，`/tmp` 还是共享的 7.6G tmpfs

- 终端的**前台**调用本机上限 180s，`cargo test --lib` 这种 12 分钟的命令必须
  `setsid nohup ... &` 挂后台再轮询；前台超时会把子进程一起带走（第一批连续三次「跑到一半日志断在块边界」）。
- `/tmp` 是 tmpfs 且被多个 agent 的 target 目录占满（实测 100%）：
  `cargo`/`clippy` 会报 `No space left on device`。跑之前 `export TMPDIR=<root 下的目录>`，
  日志也写到 `/root/...`，不要写 `/tmp`。

---

## 10. 第二批（TD-2，2026-11-03）：全仓最大的两个文件

目标：`task/model.rs`（7604 行，全仓最大）与 `erp/stockin_handler.rs`（9394 行，
其中 **5990 行是 `#[cfg(test)] mod tests`**）。两者都是「测试比实现还长」的结构，
所以第二批真正的难点不在拆实现，而在**拆测试**。

### 10.1 实测结果（行数为 `wc -l` 实测）

| 文件 | 拆分前 | facade | 代码部件 | 测试部件 |
|---|---|---|---|---|
| `task/model.rs` | 7604 | **142** | `task_model_base` 564 / `rows` 812 / `query` 777 / `write` 526 / `hierarchy` 634 / `dep` 441 / `ai` 333 / `stats` 91 / `flow` 230 / `schedule` 152 | `_tests` 321 / `_display_file_name_tests` 61 / `_depth_tests` 419 / `_root_only_tests` 385 / `_statuses_filter_tests` 393 / `_stats_tests` 55 / `_repair_tests` 331 / `_cascade_tests` 596 / `_cascade_cancel_tests` 529 |
| `erp/stockin_handler.rs` | 9394 | **131** | `stockin_handler_common` 682 / `purchase` 482 / `write` 497 / `post` 847 / `cancel` 612 / `status` 259 / `query` 101 | `_tests_fixtures` 445 / `_tests_helpers` 816 / `_tests_posting` 468 / `_tests_update` 377 / `_tests_location` 328 / `_tests_cancel` 623 / `_tests_p10_locked` 274 / `_tests_dc03` 352 / `_tests_p16` 705 / `_tests_d4_purchase` 426 / `_tests_pflow3a` 448 / `_tests_d9_copy` 377 / `_tests_a4` 547 |

保真度（`git HEAD` vs 拆分后，脚本见 §10.4）：

| 判据 | task/model.rs | erp/stockin_handler.rs |
|---|---|---|
| 条目多重集（fn/struct/enum/const/static/type/impl，递归进 mod） | 310 → 310，MISSING/ADDED=none | 270 → 270，MISSING/ADDED=none |
| SQL 字面量多重集 | 116 → 116，missing=0 added=0 | 168 → 168，missing=0 added=0 |
| 用例名多重集 | 77 → 77 | 122 → 122 |
| **代码 token 多重集**（剔注释/use/mod 结构行后逐 token；逗号单列，见 §10.2 尾注） | 36807 → 36807 | 54942 → 54942 |

「代码 token 多重集」是本批新增的判据：比条目/SQL 更严 —— 只要有一个标识符、
字面量、运算符或括号被改写/合并/删除，多重集立刻对不上。它也证明拆分中唯一的
文本改动只有 `fn`→`pub(super) fn`、`字段`→`pub(super) 字段` 与新增文件头注释
（`pub(super)` 前缀在比对前被剥掉；唯一不计的是逗号 —— rustfmt 会给多行调用补尾逗号，
单独格式化一次就会差 1 个）。

验收：`cargo check --all-targets` 0 error **0 warning**；
`cargo test --lib -- business::task` 210 passed / 0 failed；
`cargo test --lib -- business::erp::stockin` 133 passed / 0 failed（与前一轮基线逐条同数）。

### 10.2 拆测试的四条新坑（都实测踩过）

1. **`#[route(...)]` 这类多行属性会被撕开。**
   TD-1 的 `split_module.py` 只把「以 `#[` 开头的那一行」当属性，属性体
   （`path = ...` / `method = ...` / `)]`）被当成**上一项**的尾巴 →
   拆完立刻 `unexpected closing delimiter: )`（stockin 27 处 route 属性）。
   → 已修在 `scripts/refactor/split_module.py`：属性行括号不配平时继续吃续行。
2. **测试模块顶部的显式 `use` 不能跟着 `#[path]` 走。**
   原 `mod tests` 里的 `use super::*` 拿的是「父模块的项 **+ 父模块私有 `use`**
   （私有 `use` 对子模块可见）」。搬到独立文件、挂到 facade 下之后，
   facade 只有 `pub use`，没有原 import 块 → `db()` / `Uuid` / `Decimal` 全部 not found。
   → 每个测试部件**自带原文件的 import 块**；但这样一来，原 mod 顶部那些显式
   `use crate::...::{A, B};` 会和自带块同名 → `E0252 defined multiple times`，
   要把重复名从自带块或从原句里去掉（测试文件里的显式 use 优先保留）。
3. **测试夹具必须单独成部件，并且双向 glob。**
   5990 行的 `mod tests` 是按「用例语义」分组的，但夹具（`FIXTURES` / `ensure_db` /
   `setup` / 造单/造采购单/造库位的小工具）是各组共用的：
   `stockin_handler_tests_fixtures`（常量 + 连库 + 清理）与
   `stockin_handler_tests_helpers`（造数据）两个部件，每组 `use super::<夹具>::*;`。
   夹具部件之间也要互相 glob（fixtures 用到 helpers 的造数函数，反之亦然）。
4. **私有结构体字段跨部件访问。** `LinePlan { new }` / `ParentPointer { supplier_id, is_sourceless }`
   原来同名文件内可见，拆开后要放宽成 `pub(super)` 字段 ——
   与 TD-1 的 `fn → pub(super) fn` 是同一类可见性改动（crate 内、不进公开 API），
   `vis_fix` 只处理顶层声明，impl/struct 内部要单独改。

另外两个**警告级**坑：

- 某个部件里**一个 `pub` 项都没有**（如 `stockin_handler_purchase` 全是内部辅助）时，
  facade 的 `pub use super::<它>::*;` 会触发
  `glob import doesn't reexport anything with visibility pub`
  → 这类部件**不进 facade**，跨部件调用各自 `use super::<它>::*;`（对外可见性不变，
  因为它们本来就是模块私有）。
- 拆完每个部件都带着原文件的整块 import → 先 `cargo check --all-targets` 收集
  `unused import` 警告，再跑 `scripts/refactor/fix_unused_imports.py --log ... --root backend`
  （**测试代码也要跑一遍**：`--lib` 不编译 `#[cfg(test)]`，必须用 `--all-targets`）；
  剩下 `use super::xxx::*;` 这类整语句未用（脚本跳过 glob）与 `nik::prelude::*`
  要手工删。最后 `rustfmt --edition 2024 <新文件>` 收敛格式（不要全仓 `cargo fmt`，
  会把别人的在途改动一起格式化）。

### 10.3 测试模块路径会变（唯一允许的「名字变化」）

单个测试 mod 超过 1000 行时必须按语义分组拆开，模块路径因此变化：

```
before: business::erp::stockin_handler::tests::cancel_refuses_when_serial_already_shipped
after : business::erp::stockin_handler::stockin_handler_tests_cancel::cancel_refuses_when_serial_already_shipped

before: business::task::model::cascade_tests::t11_cancel::cancel_parent_cancels_doing_child
after : business::task::model::cascade_cancel_tests::cancel_parent_cancels_doing_child
```

**用例函数名、断言、夹具调用一个字都没改**（用例名多重集 77/122 逐条相同）；
变的只有 `mod` 名。文档/注释里出现旧路径的地方（如 `cargo test --lib ...::cascade_tests`
的用法示例）要顺手改成新路径。同一文件内不超限的测试 mod 仍然走
`#[path] mod <原名>;`，路径完全不变（如 `task_model_tests` 仍叫 `model::tests`）。

### 10.4 第二批的脚本与证据

| 文件 | 用途 |
|---|---|
| `scripts/refactor/split_module.py` | TD-1 拆分器（本批修了多行属性 bug） |
| `scripts/refactor/td2_split.py` | 在 TD-1 拆分器之上加：任意路径、每部件 `extra_uses`、测试整 mod 搬离、**测试 mod 内部分组**、`drop` / `unwrap_mod` |
| `scripts/refactor/plans/task_model.json`、`plans/stockin_handler.json` | 第二批两份 plan（项名 → 部件映射，含测试分组与目标文件名） |
| `scripts/refactor/verify_move.py` | 保真度校验器（条目 / SQL / 用例名 / **代码行** 四重多重集，对比 `git HEAD` 与拆分后文件族） |
| `docs/evidence/20261103-td2/` | 基线 / 拆后 / 保真度 / wc -l / git 证据 |

用法（第二批原样）：

```bash
python3 scripts/refactor/td2_split.py --plan scripts/refactor/plans/task_model.json \
        --root backend --dry-run          # 先看每个文件行数，超 1000 会标 !!
python3 scripts/refactor/td2_split.py --plan scripts/refactor/plans/task_model.json --root backend
python3 scripts/refactor/verify_move.py 52f1ccf   # 拆分前 ref；四重多重集必须全 none
```
