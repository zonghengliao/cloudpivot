# P2-32 · flow 定义业务单据查询接缝（`DocReader`）

> 归属批次：P2-B 解环 · 卡 `t_7e2b769b` · 前置 P2-31（`shared` 无环收口）
> 后续卡：P2-33（`erp` 实现并注册）、P2-34（`flow` 去掉对 `erp` 的直接依赖）

## 1. 结论

本卡把「`flow` → 业务域」那半条边**倒转**成一组 trait + 进程级注册表：

```
改前：flow ──直连 135 行──→ erp            （erp→flow 132 处，双向 = 环）
改后：flow ──trait/注册表──→ (抽象)  ←──实现+注册──  erp / hr
```

- 接缝文件 `backend/src/business/flow/doc_reader.rs`：**零业务域依赖**（只 `use cloudpivot_doc`
  这个零出边叶子 crate + 框架），用例 `seam_source_is_business_agnostic` 钉死；
- trait 与注册表：`DocReader` / `DocRegistry` / `register` / `doc_status_of` /
  `internal_control_creator_of_code` / `guard_self_approval` / `dispatch_settled`；
- **本卡不消边**（消边是 P2-34）。本卡的可测成果是：接缝就位 + 覆盖面对账 + 燃烧率闸门
  （`flow → erp` 直连只许降不许升，基线 135 行）。

## 2. 现状实测（135 行怎么来的、接缝能吃下多少）

判据：`flow/` 下 55 个 `.rs`（排除本卡新增的两个文件）里，含「业务域绝对路径 + `erp`」的行。
分类脚本（一次性取证）：逐行匹配优先级 `settle 回调 → 自审闸 → 制单人 → biz_type 比较 →
separation 配置 → 其它`。

| 类别 | 行数 | 现状代码 | 接缝 |
|:--|:--:|:--|:--|
| 终态回调扇出 | **60** | `handler.rs` 里 4 处「每个桥都调一遍」（`WITHDRAWN` / `CANCELLED` × withdrawn/terminate/decided 各一套） | `dispatch_settled(instance_id, status)` |
| 自审闸 | **17** | 审批入口逐个 `*_bridge::guard_self_approval(task_id, operator)` | `guard_self_approval(code, task_id, operator)` |
| 制单人 | **11** | `internal_control::of_biz` 里逐个 `*_bridge::internal_control_creator(biz_id)`（含 `_of_payable` 变体） | `internal_control_creator_of_code(code, biz_id)` |
| `biz_type` 比较 | **11** | 同上 17 段 `if biz_type == *_bridge::BIZ_TYPE`（顺带得出 `governed` 布尔） | `is_registered(FlowBizType)` |
| **接缝可覆盖小计** | **99** | | |
| `separation` 全局开关 | 10 | `self_approval_max_amount` / `enabled(Duty)` / `SELF_APPROVAL_MAX_AMOUNT_KEY` / `is_decimal_literal` | **不属于**单据查询：那是配置，该搬 `shared` |
| 联动 / SQL / DTO | 26 | `POSTED_RECEIVED_SQL`、`voucher::{VoucherBizCtx, VoucherSnapshot}`、`stockin_model`、`doc_pdf_model/render`、`approval_gate::SkipApprovalReq`、`transfer_{linkage,model}`、`purchase_return_dto::status_label` | **不属于**：各有归属，P2-34 按类处置 |

**为什么只吃 99/135**：一个 trait 里塞进 SQL 片段、凭证快照、PDF 渲染，等于把「单据查询」
做成第二个上帝对象 —— 那正是这次解环要拆掉的东西。分类而不是「一把全塞」，是本卡的取舍。

> 复算命令（分类脚本见本节代码块末尾的说明，脚本本身是一次性的，不进仓库）：
> `python3 <scratch>/p232_classify.py` → `扫描 55 个 .rs，含 crate::business::` + `erp 的行共 135 行`。

## 3. 接缝契约

### 3.1 `trait DocReader`（一条审批业务类型一个实现）

| 方法 | 必需 | 说明 |
|:--|:--:|:--|
| `biz_type() -> FlowBizType` | ✓ | 注册表键，全局唯一 |
| `doc_kind() -> Option<DocKind>` | 默认 | 走词表 crate 的 `FlowBizType::doc_kind()`；`PR_APPROVE` 如实 `None` |
| `doc_status(doc_id) -> DbResult<Option<DocSnapshot>>` | ✓ | **核心**：按 doc_id 取单据状态快照；`None` = 单据不存在（数据事实），`Err` = 故障 |
| `creator_id(doc_id) -> DbResult<Option<Uuid>>` | 默认 | 从快照的 `creator_id` 派生 |
| `internal_control_creator(biz_id: Option<Uuid>)` | 默认 | `None` 时**不查库**；签名与现有各桥逐字一致，P2-33 可零改动适配 |
| `on_settled(instance_id, instance_status)` | ✓ | 终态回调，**幂等 + 自过滤 + 不许 panic**（返回 `()`，没有错误通道） |
| `guard_self_approval(task_id, operator) -> Result<(), ApiError>` | ✓ | 内控闸，事务之前调 |

`DocSnapshot` 四个字段：`doc_no` / `status`（落库原值）/ `status_label`（中文文案）/ `creator_id`。
**金额不放进来**：自审上限的判据取自实例 `form_data`（`self_approval::amount_of`），两个来源迟早不一致。

### 3.2 为什么 `on_settled` / `guard_self_approval` 不给默认实现

Rust 检测不到「实现方有没有覆盖默认方法」。默认 `Ok(())` / 空体意味着：

- 漏实现 → 内控闸**静默失效**（制单人又能批自己的单）；
- 漏实现 → 单据**永久停在中间态**（审批结束了、回调没落地、没有待办能推进它 → 只能改库，
  即 AGENTS.md 约定 11 那一类事故）。

做成 required = **漏了编译不过**，这是编译期唯一能发现漏项的机制。

### 3.3 注册表与行为等价性

| 场景 | 今天的直连 | 接缝（本卡钉住的行为） |
|:--|:--|:--|
| `biz_type` 不在任何桥里（历史 / 自定义流程） | 没有那道闸、没有制单人排除 | 未注册 → `Ok(None)` / `Ok(())`，**逐字一致** |
| `biz_id = None`（审批单没挂单据） | 各桥立刻 `Ok(None)` | 默认实现直接 `Ok(None)`，**不查库**（用例钉住调用次数 0） |
| 同一单据多条审批（`PAYABLE_PAY` / `PAYABLE_CANCEL`） | 两套 `internal_control_creator*` | 两个实现、两个键（一条审批一个实现） |
| 终态扇出 | 所有桥都调一遍，各自查实例自过滤 | `dispatch_settled` 遍历**全部注册项**（有意保留，理由见模块注释：历史实例 `biz_type` 与回调不匹配时，曾被「另一条桥捞回来」） |
| 重复注册 | 编译期不可能（`if/else` 链） | 运行期 `Err` + 点名 `biz_type`（不静默覆盖） |

## 4. 给 P2-33 的实现指引

1. **别放 `doc_reader.rs`**：实现在业务域文件里（`erp/<域>/…`），本文件出现业务域路径即红线；
2. **一条审批一个实现**：`stockin` 的「过账审批」与「作废审批」是两个实现（`STOCK_IN_POST` /
   `STOCK_IN_CANCEL`），因为两者的制单人查询与回调函数都不同；
3. `doc_status` 里取不到的单据返回 `Ok(None)`，**不要**返回 `Err`；
4. `on_settled` 里继续走现有 `settle_guard` 通道（幂等 + 留痕），不要新写一套状态判断；
5. 注册点在 `bootstrap`（启动期、请求之前），重复注册要 `panic` 或记 error 后停止服务 ——
   带着半个注册表跑起来 = 随机几条审批线没有内控闸；
6. 注册后把 `doc_reader_tests::FLOW_TO_ERP_REFS_BASELINE` 按新的实测值下调。

## 5. 给 P2-34 的迁移指引（替换点 → 调用）

```rust
// ① 终态扇出（handler.rs 4 处 × ~15 行 → 1 行）
doc_reader::dispatch_settled(Some(instance_id), "WITHDRAWN").await;

// ② 内控闸（handler.rs 17 处）
doc_reader::guard_self_approval_of_code(&instance.biz_type, task_id, operator).await?;

// ③ 制单人（internal_control::of_biz 的 17 段 if/else）
let creator = doc_reader::internal_control_creator_of_code(biz_type, biz_id).await?;
let governed = doc_reader::reader_of_code(biz_type).is_some();

// ④ 单据状态 / 单号（PDF、详情）
let snapshot = doc_reader::doc_status_of_code(biz_type, doc_id).await?;
```

剩余 36 行（`separation` 配置 10 行 + 联动/SQL/DTO 26 行）不在这条接缝里，P2-34 按类处置：
配置类搬 `shared`，SQL 片段跟着查询走（`POSTED_RECEIVED_SQL` 本就该是 erp 的私有实现细节）。

## 6. 验证证据

| 判据 | 命令 | 实测（2026-11-15，99 开发机） |
|:--|:--|:--|
| 编译 | `cargo check --lib` | `Finished dev profile ... 16.37s`，**零 warning** |
| 静态检查 | `cargo clippy --lib -- -D warnings` | `Finished ... 59.63s`，零告警 |
| 接缝单测（15 条，不需要库） | `cargo test --lib business::flow::doc_reader -- --test-threads=1` | `15 passed; 0 failed; 0 ignored` |
| 分层护栏（P2-30/31 的） | `cargo test --lib business::shared::layering -- --test-threads=1` | `1 passed` |
| `flow → erp` 边数 | `python3 scripts/lint/module_cycle_report.py --from business::flow` | `→ business::erp (代码 131 / 注释 4)` —— **与 P2-31 基线逐字一致**（本卡未新增也未消除该边） |

### 6.1 护栏红绿反证（证明不是「扫空后假绿」）

| 护栏 | 红的证据 | 绿 |
|:--|:--|:--|
| `seam_source_is_business_agnostic` | 本卡补文档时误在 `doc_reader.rs` 第 26 行写入业务域路径 → `FAILED`，报错**点名到行**：`doc_reader.rs:26: 合计 **99 / 135** 行 ...` | 改写该行散文后 `ok` |
| `flow_erp_reference_budget_is_not_exceeded` | 临时塞一个探针文件写 3 行直连 → `FAILED`：`flow → erp 直连引用涨了：138 条 > 基线 135 条（56 个文件 / 2593179 字节）`，并列出前 10 处命中 | 删探针后 `1 passed` |

### 6.2 为什么「`cargo tree` 里该边消失」在本卡做不到

`backend` 仍是单 crate（workspace 化是 P2-C）。`flow → erp` 是**模块级**边，
`cargo tree` / `-i` 永远看不到它，也无从「贴出该边消失」——P2-31 已如实说明并改用
`scripts/lint/module_cycle_report.py`。本卡**不消边**（消边是 P2-34），可测成果是燃烧率闸门
（在 `cargo test` 里，不靠人工）：

- 基线 `FLOW_TO_ERP_REFS_BASELINE = 135`，`refs > 135` 即红（防回退）；
- 扫描面断言（文件数 ≥ 40、字节数 ≥ 300KB），杜绝「扫空 → 假绿」；
- 降到基线以下时打印「可下调到 N」的提示，P2-34 顺手改常量即可。
