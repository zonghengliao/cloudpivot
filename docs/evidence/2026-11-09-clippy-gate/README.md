# REVIEW-FIX-4【I-6 + I-7】AGENTS.md 框架名 + clippy 门禁 + pre-push（证据）

卡片：`t_9aa1c540`　执行：backend-developer　日期：2026-11-09/10（CST）
基线：`d172367b`（REVIEW-FIX-3 推上去的 HEAD）

## 一、结论速览

| 验收项 | 结果 |
|---|---|
| ① `AGENTS.md` 里 alun → nik | ❌ **被工具层拦下**（受保护文件，见第 3 节）；同源漂移已在 `skill/SKILL.md` + `.githooks/pre-commit` 修完，另开卡 `t_5c919ae4`（blocked，等人工批准） |
| ② `cargo clippy --all-targets -- -D warnings` | ✅ **exit 0**（`03_clippy_after_all_targets.log` 末行 `RC=0`） |
| ③ `cargo fmt -- --check` | ✅ 0 差异（`04_cargo_fmt_check.out`：`RC=0`） |
| ④ pre-push 钩子已装 | ✅ `.githooks/pre-push`（仓内版本化）+ `core.hooksPath=.githooks`；三段自测见 `08_prepush_selftest.out` |
| ⑤ push | ✅ 见第 6 节 commit hash / `git ls-remote` |
| `cargo test --lib` | ✅ 4021 passed / 0 failed（`05_cargo_test_lib.log`） |
| `cargo test --test linkage --no-run` | ✅ 编译通过（`07_linkage_norun.log`）—— 这个 target 在 HEAD 上是**编不过的**，见第 2 节 ⑥ |
| `pnpm typecheck` | ✅ RC=0（`06_pnpm_typecheck.log`；本卡未改前端） |

## 二、实际比卡面多出 22 处（卡面写「3 处」）

卡面只列了 3 处，但 `--all-targets` 真正跑起来一共 **25 处**。原因是
`cargo clippy` **先编 lib**：lib 一红就停下，`#[cfg(test)]` 里的 clippy 告警根本没机会出现。
把卡面那 3 处修完、lib 绿了以后，`lib test` 目标又冒出 20 处。

| 批次 | 处数 | 说明 |
|---|---|---|
| lib（卡面 3 处 + 2 处同族） | 5 | `invoice.rs` needless_lifetimes、`receivable.rs` doc_lazy_continuation、`sales_order_model.rs` / `attachment/model.rs` / `flow_model_inbox.rs` too_many_arguments |
| lib test（`#[cfg(test)]`） | 20 | `pdf/dto.rs` ×7 field_reassign_with_default、`voucher.rs` ×2 clone→from_ref、`self_approval.rs` ×2、`year_end_linkage_tests.rs`、`fx_revaluation.rs`、`attachment/rule.rs`、`asset_linkage.rs`、`budget_model_tests.rs`、`opening.rs`、`purchase_requisition_dto.rs`、`assignee.rs`、`edge_admin_tests.rs` |
| 集成测试 target | 1 | `tests/linkage/common.rs:750` **E0061**：`page_my_tasks` 早就是 8 个参数（多一个 `completed_since`），这个测试辅助函数还在传 7 个 ⇒ **HEAD 上 `cargo test --test linkage` 根本编不过** |

处置口径：

- **纯 lint 等价改写**：`&[x.clone()]` → `std::slice::from_ref(&x)`、`!(a < b)` → `a >= b`、
  `map.get(k).is_none()` → `!map.contains_key(&k)`、`format!` 无占位符 → 字面量 / `.to_string()`、
  `loc != None` → `loc.is_some()`、`for x in [y]` → 直接 `let x = y`、
  `Default::default()` + 逐字段赋值 → struct update 语法、嵌套 `format!` 合并、
  `buckets.iter().all(|(loc,_)| *loc != None)` → `loc.is_some()`。
- **`too_many_arguments`（4 处）**：按卡面建议 `#[allow(clippy::too_many_arguments)]` + 注释写明
  「为什么这 8 个参数就是完整口径」「拆 struct 要改全部调用方，属独立重构」，
  重构另开卡 `t_69e360ad`（`sales_order_model::update_order` / `attachment::model::insert_with_uploader`
  / `flow::model::page_my_tasks` 三处业务函数；`self_approval` 的测试夹具保留 allow）。
- **`edge_admin_tests.rs` 的 `empty_line_after_doc_comments`**：那段 doc 是**已被删除的字面 hash 守卫**
  留下的孤儿注释（HEAD 上就有），改成普通 `//` 注释保留史实，不再挂在 `const APPROVER_ROLE_CODE` 上。
- **`tests/linkage/common.rs`**：补第 8 个参数 `None`，并注释「待办没有 `completed_at`，
  `since` 只对『我的已办』有意义 —— 与生产 `flow::handler::my_todo` 同口径」。

**行为没变**：25 处里没有一处改业务语义（20 处是测试代码，5 处是签名/注释/lint 属性）；
`cargo test --lib` 4021 条照旧全绿。

## 三、① 为什么没改成：AGENTS.md 是受保护文件

`patch` 工具对 `/opt/cloudpivot/AGENTS.md` 返回：

```
BLOCKED: write to protected agent-instruction file(s) (AGENTS.md) approval prompt timed out
without a user response. Silence is not consent. ... Do NOT retry it or attempt the same edit
via another path (terminal, execute_code, etc.).
```

kanban worker 是 headless 的，没有人工应答批准弹窗 ⇒ 这条**在本卡内不可能完成**，
而且工具明确禁止换路径重试。所以：

- 同源漂移**能修的都修了**：`skill/SKILL.md` 6 处（含把「框架源码（同级）`../../../alun/`」
  改成实际路径「仓库内 vendored：`backend/vendor/nik/`」）+ `.githooks/pre-commit` 里的
  `ALUN_MIDDLEWARE_AUTH_JWT_SECRET` → `NIK_MIDDLEWARE_AUTH_JWT_SECRET`（真名见
  `backend/src/shared/constants.rs:146` 的 `NIK_ADMIN_PASSWORD` 同族命名）。
- `AGENTS.md` 的 6 处行号 + old→new **已逐条写进卡 `t_5c919ae4`**（initial_status=blocked），
  由一个能过审批的会话一次性落地。

**为什么不是「全仓 alun 清零」**（避免后人误伤）：

- `docs/批次*-验证证据.md` 里的 `alun_web::app` / `alun_plugin::scheduler` 是**当年的启动日志原文**，
  是证据，改了就是伪造证据。
- `ERP_DB_URL='postgres://alun:***@…'` 里的 `alun` 是**数据库用户名**（本机 PG 就是 alun/alun），
  不是框架名。
- 判据：凡是 `alun` 作为 **crate / 宏 / 模块路径 / 环境变量前缀 / 框架名**出现 → 改；
  作为 **DB 用户名 / 历史日志** 出现 → 留。

## 四、④ pre-push 钩子（机械强制，替代「靠人记得跑 check.sh」）

- 文件：`.githooks/pre-push`（**仓内版本化**，不是 `.git/hooks` 里的一次性件），
  与既有 `.githooks/pre-commit` 同一套安装方式：`git config core.hooksPath .githooks`
  （本仓已设，见 `08_prepush_selftest.out` 末行）。
- 跑什么：`cargo fmt -- --check` → `cargo clippy --all-targets -- -D warnings` → `pnpm typecheck`
  （后者在 `frontend/node_modules` 存在时才跑；CI 里没有依赖树属正常，会打印 ℹ 并跳过）。
- 不跑什么：`cargo test` / 真库用例 / ERP 不变量 / 快照巡检 —— 那些要库、要几分钟，
  仍归 `bash scripts/check.sh`（`CHECK_WITH_DB=1`）与 CI。
- 三段自测（`08_prepush_selftest.out`）：
  A 只删远端分支 → 跳过；B `SKIP_PREPUSH=1` → 告警放行；C 正常推送 → 真跑三道闸，`RC=0`。
- 文档：`docs/编码规范.md` §3.1（表格 + 安装 + 绕过 + 职责边界）。

## 五、证据文件

| 文件 | 内容 |
|---|---|
| `01_clippy_lib_before.txt` | 修前 lib 5 条（`RC=101`） |
| `02_clippy_test_target_before.txt` | lib 绿后暴露的 `lib test` 20 条（`RC=101`） |
| `03_clippy_after_all_targets.txt` | 修后 `--all-targets` **`RC=0`** |
| `04_cargo_fmt_check.out` | `cargo fmt -- --check` → `RC=0` |
| `05_cargo_test_lib.txt` | `cargo test --lib`：4021 passed / 0 failed |
| `06_pnpm_typecheck.txt` | `pnpm typecheck` → `RC=0` |
| `07_linkage_norun.txt` | `cargo test --test linkage --no-run` → `RC=0` |
| `08_prepush_selftest.out` | pre-push 三段自测 + `core.hooksPath` 实测 |
| `09_push.txt` | `git ls-remote`（远端 = 本卡 commit）+ 真实 push 时 pre-push 的实跑输出 |
| `10_145_health.txt` | 145 dev 环境存活探针（`/health` code=0；`/api/system/auth/login` 对假账号回 401 ⇒ 路由在、鉴权在） |

> **为什么没做「真实 token 登录 + 业务 curl」**：本卡没有改任何 handler / DTO / SQL / 迁移
> —— 25 处全是 lint 等价改写（20 处在 `#[cfg(test)]`）+ 2 处 `#[allow]` + 1 处测试辅助函数补参数，
> 另加文档与钩子。行为的证据是 `cargo test --lib 4021 passed / 0 failed`（覆盖全部 lib 用例）
> 与 `pnpm typecheck`；`10_145_health.txt` 只用来确认演练环境本身活着，不构成业务回归证据。


## 六、提交说明

本卡与其它 worker **共用同一个工作区**（`/opt/cloudpivot`，都直接推 main），
而 `voucher.rs` / `edge_admin_tests.rs` 里同时躺着别人的在途改动。为不把别人的半成品提交上去，
这两个文件按「HEAD 原文 + 本卡改动」重建 blob 后 `git update-index --cacheinfo`：

- `backend/src/business/erp/voucher.rs`：只含本卡的 2 处 `clone → from_ref`（别人的 rustfmt 重排留在工作区）
- `backend/src/business/flow/edge_admin_tests.rs`：只含本卡的孤儿 doc 注释改写（别人的 243 行改写留在工作区）

提交后校验：`git diff --cached` 里不含别人的关键词，`git ls-remote origin main` = 本卡 commit。

### 落库结果

| 项 | 值 |
|---|---|
| commit | `845618b2`（`fix(lint): clippy --all-targets 清零（25 处）+ 框架名同源纠偏 + pre-push 门禁`） |
| 远端 | `git ls-remote origin main` → `845618b2a333bc6b20ac64e2d69b10d7a7660244`（见 `09_push.txt`） |
| push 前的钩子 | 真实 push 时 `.githooks/pre-push` **自己跑了一遍**：fmt / clippy --all-targets / pnpm typecheck 全绿才放行（同一份输出在 `09_push.txt`） |
| 与远端的同步 | 基座是 `d172367b`；他人随后推了 6 个 commit（HOME-5/HOME-6/E4b），本卡 commit 被同步机制 rebase 到 `cb97961` 之上后线性推入，**无 merge commit、无 --amend** |

