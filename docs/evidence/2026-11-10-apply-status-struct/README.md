# REFACTOR【P5-补】apply_status_with_instance 8 参数收口为 struct（去遗留 #[allow]）

任务：`t_c5afaedf`｜执行：backend-developer｜日期：2026-11-10

## 0. 结论

- 卡面点名的那一处（`sales_order_model::apply_status_with_instance`，8 参数）已收口为
  `ApplyStatusWithInstanceInput<'a>` 并删掉 `#[allow(clippy::too_many_arguments)]`。
- **卡面"7 处调用点"实际横跨 3 个同名函数**（`sales_order_model` / `stockin_write`(=`stockin_model`) /
  `purchase_model`），各 8 / 8 / 9 参数、各带一处 `#[allow]`。只改 `sales_order_model` 那一处的话，
  卡面列的 7 处调用点里会有 5 处**不需要改也没法改**。因此三处同名函数一并收口，
  7 处调用点全部改成 struct 字面量 —— 这正是卡面"同步改那 7 处调用点"要求的形态。
- **卡面验收的 `grep ... src/business/erp/ # 期望 0 命中` 不可能达成**：改前三处之外的 erp 目录里
  还有 **27 处** 同类 `#[allow(clippy::too_many_arguments)]`（20 个文件，都是别的函数：payable / budget /
  asset / commission / 收货过账 handler …，见 `04_grep_allow.out`）。本卡只清掉 `apply_status_with_instance`
  家族的 3 处（30 → 27）。剩余 27 处置换需要单独的、分批的卡，已开跟进卡（见 §5）。
- 语义零改动：`git diff -U0` 里带 SQL 关键字的行全是新增的 doc 注释（`WHERE id = ?` 之类），
  没有一行真 SQL / 状态机分支 / 事务边界被改。

## 1. 改动

三个同名函数统一形态：`ex` 留在参数位（`&impl SqlExecutor` 泛型会污染结构体），
`purchase_model` 的 `outbox: &mut LinkageOutbox` 同样留参数位；函数体开头
`let ApplyStatusWithInstanceInput { .. } = input;` 解构，**函数体其余部分一行未改**。

| 模块 | struct 位置 | 新签名 |
|---|---|---|
| `sales_order_model` | `sales_order_model.rs:791` | `apply_status_with_instance(ex, input)` |
| `stockin_write`（经 `stockin_model` 再导出） | `stockin_write.rs:260` | `apply_status_with_instance(ex, input)` |
| `purchase_model` | `purchase_model.rs:652` | `apply_status_with_instance(ex, input, outbox)` |

字段一律 `{ id, from, to, reason, operator, approval_instance_id, now }`，`#[derive(Debug, Clone, Copy)]`，
逐字段中文文档（语义写在结构体上，不靠"参数顺序靠记"）。

7 处调用点：

| 调用点 | 函数 |
|---|---|
| `erp/sales_order_handler.rs:1487` | `sales_order_model` |
| `erp/sales_approval_bridge.rs:272` | `sales_order_model` |
| `erp/stockin_write.rs:240`（`apply_status` 包装） | `stockin_write` |
| `erp/stockin_handler_post.rs:331` | `stockin_write`（`stockin_model::`） |
| `erp/stockin_handler_cancel.rs:183` | `stockin_write`（`stockin_model::`） |
| `erp/purchase_model.rs:631`（`apply_status` 包装） | `purchase_model` |
| `erp/purchase_handler.rs:1524` | `purchase_model` |

### 一处类型坑（已修正）

`stockin_handler_post` / `stockin_handler_cancel` / `purchase_handler` 的 `operator` 是
`&state_machine::Operator`，其 `id` 字段本身就是 `Option<Uuid>`。原先按位置传 `operator.id` 恰好对上
`Option<Uuid>` 形参；改成 struct 时若顺手写成 `operator: Some(operator.id)` 会 `E0308`
（`Option<Option<Uuid>>`）。已全部写回 `operator: operator.id`，与原语义一致。

## 2. 验收（实跑输出）

| 项 | 命令 | 结果 | 证据 |
|---|---|---|---|
| clippy | `cargo clippy --all-targets -- -D warnings` | **RC=0**（2m23s，Finished dev profile） | `01_clippy_all_targets.txt` |
| 单测 | `cargo test --lib` | **4021 passed / 0 failed**（与改前基线同数） | `02_cargo_test_lib.txt` |
| 格式化 | `cargo fmt -- --check` | **RC=0**（全仓，含别人 WIP 文件） | `03_cargo_fmt_check.out` |
| allow 清理 | `grep -rn "allow(clippy::too_many_arguments)" src/business/erp/` | **29 行**：3 处 `apply_status_with_instance` 已清零，剩 27 个属性 + 2 行 doc 提及（见下） | `04_grep_allow.out` |

`apply_status_with_instance` 家族已无 `#[allow]`：

```
$ grep -rn -B1 "fn apply_status_with_instance" src/business/erp/
src/business/erp/stockin_write.rs-281-/// （否则 `ck_biz_si_pending_cancel` 当场 23514），只有发起审批那次才带值。
src/business/erp/stockin_write.rs:282:pub async fn apply_status_with_instance(
src/business/erp/purchase_model.rs-673-/// `NULL`（不然驳回打回草稿之后还挂着旧实例，下一次提交的判断会被污染）。
src/business/erp/purchase_model.rs:674:pub async fn apply_status_with_instance(
src/business/erp/sales_order_model.rs-822-/// 调用方必须先过 `state_machine::transition`（边表校验 + 写 `biz_status_log`）。
src/business/erp/sales_order_model.rs:823:pub async fn apply_status_with_instance(
```

## 3. 卡面数字纠正

| 卡面 | 实际 |
|---|---|
| 该函数 8 参数、7 处调用点、6 个文件 | 8 参数本身没错，但 7 处调用点分属 **3 个同名函数**；文件数是 **7**（卡面列了 7 个文件名却说 6 个） |
| `grep ... src/business/erp/` 期望 0 命中 | 改前 30 个属性 / 改后 **27 个属性**（20 文件），全部与 `apply_status_with_instance` 无关 |

## 4. 不是死代码

7 处调用点全在活路径上（销售订单提审 / 审批回调 / 收货单过账提审 / 收货单作废提审 / 采购单提审 + 两个
`apply_status` 包装），无死代码需要报告。

## 5. 跟进（未在本卡做）

`src/business/erp/` 剩余 27 处无理由注释的同类 allow，其中最近的同类是
`purchase_model::apply_status`（8 参数，含 `outbox`，`purchase_model.rs:620`）—— 与被收口的三处同族，
但卡面只点了 `apply_status_with_instance`，未动。已开跟进卡统一处理剩余 27 处（分批）。
