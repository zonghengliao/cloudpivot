# P2-39：`impexp ⇄ masking` 抽接缝（导出定义搬回导出域，`masking → impexp` 归零）

- 卡号：t_780ef3cc（P2-B 解环第 9 张）
- 前置：P2-38（t_69240409，`flow → task` 归零，提交 `f8986867`）；P2-01~26 拆目录已完成
- 结论：**`business::masking → business::impexp` 不存在**（代码 0 / 注释 0，改前 代码 9 / 注释 1）；
  反方向 `business::impexp → business::masking` 仍存在且**只有这一个方向**（代码 9 / 注释 3）；
  双向边从 **6 对降到 5 对**，`impexp ⇄ masking` 已不在其中

## 1. 问题：环是怎么长出来的

两个方向各有一坨东西，**每一坨都「天然长在错的那一边」**：

| 方向 | 命中的东西 | 原落点 | 为什么会这样 |
|---|---|---|---|
| `impexp → masking`（合法，保留） | 导出前取脱敏规则、`MaskError` | `impexp/engine.rs`、`handler.rs`、`worker.rs` | 导出必须脱敏（批次 B 架构 §3.7），这是安全红线，不能摘 |
| `masking → impexp`（违规，本卡摘掉） | `sys_user` 导出定义（实现 `ExportDefinition`） | `masking/user_export.rs` | B6 当年把「导出脱敏的验证载体」当成脱敏域的交付物一起放在 `masking/` |
| `masking → impexp`（违规，本卡摘掉） | 导出链路 e2e 用例（M04，调 `format_export_rows`） | `masking/tests.rs` | 该用例验的是「导出会不会漏明文」，验的其实是导出域的行为 |

因此环的根子不是「谁 import 谁写错了」，而是**「导出定义 / 导出用例该住哪个域」放错了**：

- 导出定义（`ExportDefinition` 的实现）是**导出域的内置定义**，与 `dict_item` / `erp_doc` / `hr`
  同类，住 `masking/` 只是历史巧合；
- 导出链路的 e2e 用例验的是 `format_export_rows` 的 fail-closed 行为，属于导出域的回归。

反方向（导出取脱敏规则）**不该摘**：摘了就是「导出不脱敏」，比环严重得多。所以本卡做两件事：
**① 把放错域的两块搬回导出域；② 把「导出怎么拿规则」抽成 `masking` 提供的接缝 `Masker`**，
让依赖方向只剩一条腿也能站住（导出域认 trait，不认 `MaskSet` 的字段与构造）。

## 2. 改动清单（三件事，行为一行未改）

### 2.1 搬家：`masking/user_export.rs` → `impexp/builtin/user_export.rs`

- 文件内容语义未改（列定义、`scope::filter_for` 取数、`USER_BIZ_TYPE = "system.user"` 全保持）；
- 注册点从 `impexp/registry.rs::global_registry` 的「单独 register」挪进
  `impexp/builtin/mod.rs::register`（与其它内置定义同一个入口，注册失败仍 `panic`）；
- `masking/mod.rs` 的模块表删掉 `user_export` 一行，改为一句「导出定义住在导出域」的说明。

### 2.2 抽接缝：`masking::masker::Masker`

```rust
pub trait Masker {
    fn is_exempt(&self) -> bool;
    fn mask_value(&self, table: &str, column: &str, raw: &str)
        -> std::result::Result<Option<String>, MaskError>;
}
```

- 定义在 `masking/masker.rs`（纯值层，与 `MaskError` / `MaskType` 同处），**不放** `registry`：
  接缝只需要「取值」，与「规则怎么从库里合并」无关；
- `impl Masker for MaskSet` 在 `masking/registry.rs`，方法体委托给同名固有方法
  —— 固有方法（含 fail-closed 口径的文档）只留一份，trait 层不复制行为；
- `impexp/engine.rs::format_export_rows` 第四个参数由 `&MaskSet` 改为 `&dyn Masker`，
  生产面不再出现 `registry::MaskSet`（`handler` / `worker` 仍用 `masking::resolve()` 的返回值
  传给引擎，`&MaskSet` 在调用点自动退化为 `&dyn Masker`）。
- **为什么 trait 定义在脱敏域而不是导出域**：传的是「只读取值」这件能力，不是我方命令；
  定义在导出域就等于 `masking` 必须 import `impexp`，环立刻回来。

### 2.3 搬家：M04 导出用例 → `impexp/export_mask_tests.rs`

- 原 M04 一分为二：
  - `masking/tests.rs::m04_loaded_ruleset_masks_through_masker_seam`（脱敏域这一半）：
    库里配规则 → `load_user_set` → **以 `&dyn Masker` 调用** → 得到掩码；顺带断言空集合返回 `None`；
  - `impexp/export_mask_tests.rs::export_applies_mask_from_database_rule_and_fails_closed_without_it`
    （导出域那一半）：带真实库规则跑 `format_export_rows` → 单元格无原值、非敏感列不动；
    空规则集 → **422 + 61008**，且错误信息里也不许出现原值。
- 保存了原 M04 的全部断言（原值不出现、fail-closed 码值、错误文案不泄漏）。

## 3. 边归零的证据（可复核命令）

> **`cargo tree` 的局限（同 P2-31~38 口径，如实说明）**：`backend` 目前是**单 crate**
> （workspace 化 = 后续 P2-C 的活），`business::impexp` 与 `business::masking` 是**模块级**依赖，
> `cargo tree` 只能画 crate 级 —— 它既画不出这条边，也就谈不上「贴出它消失」。
> 证据因此分两层：① crate 级证明两域在**同一个 crate 里**（0 条 `business::` 边）；
> ② 模块级用 `scripts/lint/module_cycle_report.py` 出边/双向边/SCC 报告。

```console
# ① crate 级：单 crate，cargo tree 里不存在任何 business:: 依赖边
$ cd backend && cargo tree --depth 1 | head -3
cloudpivot v0.1.0 (/opt/cloudpivot/backend)
├── ammonia v4.2.0
├── async-trait v0.1.92 (proc-macro)
$ cargo tree | grep -c 'business::'
0

# ② 模块级：masking → impexp 已不存在（改前：代码 9 / 注释 1）
$ python3 scripts/lint/module_cycle_report.py --edge business::masking business::impexp
边 business::masking → business::impexp：**不存在**（该域内无任何 crate:: 引用指向 business::impexp）

# ③ 反方向仍在且只单向（改前 代码 8 / 注释 0 → 现 代码 9 / 注释 3，含新搬家文件的 import）
$ python3 scripts/lint/module_cycle_report.py --edge business::impexp business::masking
边 business::impexp → business::masking：存在（代码 9 / 注释 3）
  [code] business/impexp/engine.rs:29            # use ...masking::masker::{MaskError, Masker}
  [code] business/impexp/export_mask_tests.rs:29
  [comment] business/impexp/builtin/user_export.rs:8

# ④ masking 的全部出边只剩 shared（纯业务域依赖归零）
$ python3 scripts/lint/module_cycle_report.py --from business::masking
business::masking 的出边：1 条
  → business::shared  (代码 7 / 注释 0)

# ⑤ 双向边 6 对 → 5 对，`impexp ⇄ masking` 已不在其中
$ python3 scripts/lint/module_cycle_report.py --mutual | sed -n 4,10p
双向边（互为依赖，2 环）：5 对
  ● business::announcement → business::notice ... ⇄ ...
  ● business::changelog → business::erp ... ⇄ ...
  ● business::erp → business::task ... ⇄ ...
  ● business::flow → business::hr ... ⇄ ...
  ● business::hr → business::impexp (代码 18/注释 4) ⇄ business::impexp → business::hr (代码 11/注释 0)

# ⑥ 文字层面（含注释）
$ grep -rn 'crate::business::impexp' backend/app/src/business/masking/ | wc -l
0
$ grep -rn 'masking::user_export' backend/app/src/ | wc -l
0
```

跨域有向边总数：**62 → 61**（扫描面 788 → 790 个 `.rs`，因为新增两个测试文件）。

### 3.1 闸门（进 CI，防回退）

`backend/app/src/business/masking/independence_tests.rs`（`#[cfg(test)]`，3 条）：

| 用例 | 管什么 |
|---|---|
| `masking_source_has_no_impexp_references` | `masking/**/*.rs` 里含 `crate::business::impexp` 的**行数必须 == 0**（注释与 doc link 也算）；扫描面硬断言 `files >= 4` / `bytes >= 100_000`（实测 7 文件 / 152 KB），防「扫空即假绿」 |
| `mask_set_satisfies_the_masker_seam` | `&MaskSet` 可当 `&dyn Masker` 用（对象安全）；trait 层与固有层对「是否豁免」同口径；空集合返回 `None` |
| `impexp_engine_consumes_the_masker_seam` | 反方向必须存在：`engine.rs` **生产面**（`#[cfg(test)] mod tests` 之前）import 了 `masking::masker::*Masker*`、`format_export_rows` 收 `&dyn Masker`、且**不许**出现 `registry::MaskSet` —— 防「解环=把脱敏两边都删掉」这种安全回归 |

红绿反证（两条都实测过，改完已还原）：

```console
# 红①：往 masking/registry.rs 末尾加一行注释 crate::business::impexp::engine::format_export_rows
$ cargo test --lib business::masking::independence_tests::masking_source_has_no_impexp_references
masking → impexp 直连引用**回退**了：1 条 > 基线 0 条（7 个文件 / 151938 字节）
  .../masking/registry.rs:777: // 反证用临时行（马上删除）：crate::business::impexp::engine::format_export_rows
test result: FAILED

# 红②：把 engine.rs 的 mask 参数临时改回具体的 &MaskSet
$ cargo test --lib business::masking::independence_tests::impexp_engine_consumes_the_masker_seam
`format_export_rows` 的 `mask` 参数必须收 `&dyn Masker`（收具体 `MaskSet` 等于把脱敏域的实现细节焊进导出域）
test result: FAILED

# 绿：还原后
$ cargo test --lib business::masking::independence_tests
test result: ok. 3 passed; 0 failed
```

> 门禁登记：新增两个「只进测试编译」的文件已补进 `scripts/lint/scope_gate.py` 的
> `SKIP_MANIFEST`（`business/impexp/export_mask_tests.rs`、`business/masking/independence_tests.rs`），
> 满足该闸「实测跳过集合必须与常量逐条一致」的硬断言。

## 4. 验证（真实执行输出）

```console
# 静态面
$ cd backend && cargo check --lib
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 47.29s      # 零 warning
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile ... in 3m 14s                                     # 零告警
$ cargo fmt --check                                                          # fmt 已跑

# 全量 lib 单测（离线口径，真库用例自行 SKIP）
$ cargo test --lib -- --test-threads=1
test result: ok. 5681 passed; 0 failed; 0 ignored

# 真库：99 → 145:5432，克隆库 cp_p239_mask ⊂ cloudpivot_test
$ SYS_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_p239_mask \
  cargo test --lib business::masking -- --test-threads=1 --nocapture
测试C 已连接验证库（cloudpivot_test）                                     # 真连了库，不是 SKIP
[M04] 库规则 → load_user_set → dyn Masker 得到 "139****5678" ✓
test result: ok. 40 passed; 0 failed

$ SYS_DB_URL=... ERP_DB_URL=... cargo test --lib business::impexp -- --test-threads=1 --nocapture
[X-MASK] 导出单元格 "139****5678" 已打码；无规则时 fail-closed（422 + 61008）✓
test result: ok. 221 passed; 0 failed

# 门禁
$ bash scripts/check.sh --fast
==> 全部检查通过（L0 L1）
$ python3 scripts/lint/scope_gate.py --root .        # 跳过清单与 SKIP_MANIFEST 逐条一致
$ python3 scripts/lint/path_refs_gate.py --root .    # PASS（898 个脚本 / 0 处失效路径）
```

> **踩坑记录（写进结论，避免下一个人误判假绿）**：`business/masking/*` 与
> `business/impexp/export_mask_tests.rs` 用的是 `system::tests_support`，它只认 **`SYS_DB_URL`**；
> 卡里给的 `ERP_DB_URL` 是 ERP 侧用例的口径。只设 `ERP_DB_URL` 时整套用例会打印
> `SKIP: 未设置 SYS_DB_URL` 并**全绿返回**（0.03s 跑完 40 条就是征兆）—— 本卡实测两种环境变量
> 都显式传入，并用 `--nocapture` 确认真的连了库。

## 5. 行为与事务边界

- 导出脱敏的实际行为**一行未改**：同一份规则集、同一个 `format_export_rows`、同一个
  fail-closed 分支（422 + 61008）；改动只是「参数类型从具体 `MaskSet` 变成 `&dyn Masker`」。
- `sys_user` 导出定义注册时机未变（仍是 `global_registry()` 首次调用时的懒装载），
  `biz_type = "system.user"` 未变，前端 / 集成用例（`backend/app/tests/testd/masking.rs` 的 X04）不受影响。
- 无迁移、无权限码、无路由变化；不涉及事务与补偿（本卡只搬文件 + 抽 trait）。

## 6. 遗留 / 后续（不是本卡的遗漏）

- `hr ⇄ impexp` 仍在双向边表里（18 / 11）：HR 的导入走 `impexp` 通用通道、导出走自己的
  `/business/hr/*/export`，两边的形状不同，按 P2-31 的登记「随 P2-39 收」**未在本次一并处理**
  —— 它是另一条边（`hr ⇄ impexp`），不是 `impexp ⇄ masking`，需要单独一张卡（收法大概是
  HR 侧登记 `ImportDefinition` + 经接缝回调，体量明显大于本卡）。
- `cargo tree` 能画出这两条边的前提是 **`backend` 拆成多 crate**（workspace 化）—— 属 P2-C。
  在那之前，模块级边的唯一证据就是 `scripts/lint/module_cycle_report.py`；本卡的闸门用例把
  「下次谁写回一行」变成单测红，不依赖人工跑脚本。
