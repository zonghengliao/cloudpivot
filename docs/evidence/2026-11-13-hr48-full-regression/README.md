# HR-48 · 全量回归（既有 verify 脚本 + 新增 HR 脚本）

卡：`t_fb2e95f4`（批 5 收口，来自 ARCH-9-2 §8.1「全量回归」）
验收：`scripts/check.sh` 全绿 · 既有脚本不受影响

## 1. 跑了什么（一条命令：`scripts/check.sh`）

`scripts/check.sh` 分四层，本次四层全跑（`--full`），并把「连库档」真正打开
（`CHECK_WITH_DB=1` + 三个库 URL 指向 145 上的演练库 `hr48_drill`）：

| 层 | 内容 | 结果 |
|---|---|---|
| L0 | C7 G1–G6 / 色板 / 迁移版本号全局唯一 / 库存写入口 / 迁移授权段角色码 | 全绿 |
| L1 | `cargo fmt --check` / `clippy --all-targets -D warnings` / `cargo check` / `pnpm typecheck` | 全绿 |
| L2 | 闸⓪ 真库用例（全量 lib）· 闸① 分页唯一写法 · 闸② scope 登记 · 闸③ ERP 不变量 · 闸④ 词表域 · 闸⑤ 快照 · 闸⑥ 死权限码 · 闸⑦ 孤儿按钮 | 全绿 |
| L3 | 干净检出闸（`git archive HEAD` → lib 单测） | 全绿 |

## 2. 本卡改了什么（都是为了「全绿」）

回归跑出来的**唯一红**是 L2 闸②（`scripts/lint/scope_gate.py`）。两类问题：

### 2.1 解析器漏一类测试面（根因，已修）

`erp/payable_payment/tests/payable_linkage_db_tests.rs` 是**纯测试代码**，但它是在
`payable_payment.rs` 的内联 `#[cfg(test)] mod tests { … mod payable_linkage_db_tests; }`
里挂进来的。旧解析器只跳过「区体本身」，**不解析区体内 `mod y;` 声明的外部文件**，
于是这个测试文件被当成生产文件，闸② 要它写一个 SCOPE-EXEMPT 才放行。

修法（`scripts/lint/scope_gate.py`）：

- 新增 `decls_in_inline_region()`：扫内联 `#[cfg(test)] mod x { … }` 区体内的 `mod y;`；
- `Decl` 增加 `prefix` 字段，按 **Rust 模块语义**把「内联模块本身占一层目录」算进路径
  （`foo.rs` 的 `mod tests { mod y; }` ⇒ `foo/tests/y.rs`，不是 `foo/y.rs`）；
- 被跳过的文件**仍要过 `SKIP_MANIFEST` 逐条登记**（多一个少一个都 FAIL），所以
  「一句 cfg(test) 把生产文件藏起来」的形态照样被拦 —— 自测 ⑩ 就是这条反事实对照。

自测新增 ③'（内联区体内声明的外部文件进测试面），`--self-test` 13 项全通过。

### 2.2 存量：7 个文件缺 SCOPE-EXEMPT 理由 / 19 个测试面文件漏登记

| 文件 | 处理 | 依据 |
|---|---|---|
| `business/impexp/builtin/hr/employee.rs` | 加 `SCOPE-EXEMPT`（HR 本模块） | 员工导入的部门名/路径解析索引：一次性取全量部门树把名称解析成 `dept_id`，只做校验与解析、不产出列表行集；入口权限 `business:hr:employee:import` |
| `business/shared/user_name.rs` | 加 `SCOPE-EXEMPT` | 姓名**快照**助手（AGENTS.md 约定 13）：按主键/给定 id 集合取姓名供审计列落库，调用方已过完数据权限 |
| `business/monitor/model.rs` | 加 `SCOPE-EXEMPT` | 服务器状态页的**系统总量**（在线数/今日登录/今日操作）：按数据范围裁剪会让运维看到错误数字 |
| `business/flow/config_check.rs` | 加 `SCOPE-EXEMPT` | 流程图配置校验：只为「审批人是否停用 / 部门名」按主键取值，产出校验提示不产出行集 |
| `business/flow/subprocess.rs` | 加 `SCOPE-EXEMPT` | 子流程校验：按主键读定义 + 按 `parent_instance_id` 回溯实例链，`biz_flow_*` 是全局配置侧（口径同 `flow/model.rs` 文件头） |
| `system/setup/model.rs` | 加 `SCOPE-EXEMPT` | 初始化向导：空库首启引导期，没有登录主体也没有数据范围可裁 |
| 19 个测试面文件 | 补进 `SKIP_MANIFEST` | 全部是 `#[cfg(test)]` 挂载的 `*_tests.rs` / `tests/*.rs`（12 个 HR、7 个 erp/flow），闸② 自己的报错文案就要求这么做 |

> 上表 6 个生产文件（除 `impexp/builtin/hr/employee.rs`）属**别的模块**的存量债：
> 在 HR-01 的提交 `60befd59`（HR 代码进场之前）上跑同一个门禁，它们同样报红（证据见
> `01_scope_gate_before.txt`）。本卡只补 `SCOPE-EXEMPT` 理由注释，**零行为变更**，
> 理由是否成立请各 owner 复核。

## 3. 新脚本 / 既有脚本

（见下节实测输出）
