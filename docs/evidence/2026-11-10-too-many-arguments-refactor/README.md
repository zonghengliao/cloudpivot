# REFACTOR【P5】三处 `too_many_arguments` 收口为 struct 参数（去 `#[allow]`）

任务卡：`t_69e360ad`（上游 `t_9aa1c540` / REVIEW-FIX-4 把 clippy 清零时对 4 处
`clippy::too_many_arguments` 用了 `#[allow] + 理由注释`，本卡把其中 3 处收口成 struct）。

## 改了什么

| 函数 | 新入参结构体 | 定义位置 |
|---|---|---|
| `sales_order_model::update_order` | `UpdateOrderInput<'a>` | `backend/src/business/erp/sales_order_model.rs:664` |
| `attachment::model::insert_with_uploader` | `NewAttachmentBinding<'a>` | `backend/src/business/attachment/model.rs:844` |
| `flow::model::page_my_tasks` | `MyTaskQueryParams<'a>` | `backend/src/business/flow/flow_model_inbox.rs:343` |

- 三处的 `#[allow(clippy::too_many_arguments)]` 与配套理由注释**一并删除**（见 `04_grep_allow.out`）。
- `ex`（`update_order` 的 SQL 执行器）留在参数位：它不是「业务数据」，混进结构体只会让
  `&impl SqlExecutor` 的泛型塞进 struct（要么泛型化 struct、要么 `dyn`），得不偿失。
- 三个结构体都是 `#[derive(Debug, Clone, Copy)]` + 全字段文档（字段语义直接写在结构体上，
  避免「参数顺序靠记」）；函数体开头 `let X { .. } = input;` 解构，函数体一行没动。
- 测试夹具 `flow::self_approval::tests::add_task` 的 `#[allow]` **按要求保留**（拆 struct 只会
  让 20+ 处调用点更难读）。

## 调用方同步改掉（共 15 处）

`backend/src/business/erp/sales_order_handler.rs`、`sales_order_model.rs`（3 处单测）、
`backend/src/business/attachment/handler.rs`、`attachment/model.rs`（`insert` 夹具）、
`attachment/tests.rs`（4 处）、`backend/src/business/flow/handler.rs`（待办 / 已办）、
`flow/internal_control.rs`（2 处）、`backend/src/business/dashboard/model.rs`（2 处）、
`backend/src/business/erp/purchase_handler.rs`、`backend/tests/linkage/common.rs`。

## 验收（本目录日志）

| 门禁 | 命令 | 结果 | 日志 |
|---|---|---|---|
| clippy | `cargo clippy --all-targets -- -D warnings` | `RC=0`（0 告警） | `01_clippy_all_targets.txt` |
| 单测 | `cargo test --lib` | `4021 passed; 0 failed`（与改前同数） | `02_cargo_test_lib.txt` |
| 格式 | `cargo fmt -- --check` | `RC=0` | `03_cargo_fmt_check.out` |
| allow 清零 | `grep '^#\[allow(clippy::too_many_arguments)\]'` | 三处 0 命中（仅测试夹具保留） | `04_grep_allow.out` |

## 两点澄清（避免误判为漏改）

1. `sales_order_model.rs:800` 还有一个 `#[allow(clippy::too_many_arguments)]`，属于
   `apply_status_with_instance`（8 参数：`ex/id/from/to/reason/operator/approval_instance_id/now`），
   **HEAD 上就存在**、不在本卡范围的 3 处之内，未动。
2. 只改签名与调用点，**没动任何 SQL / 业务分支 / 事务边界**；`cargo test --lib` 通过数仍是
   4021（= 改前基线），证明没有用例被静默跳过或删除。
