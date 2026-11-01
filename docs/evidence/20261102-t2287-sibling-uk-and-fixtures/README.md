# t_2287c331 · 兄弟模块审批 uk 冲突文案统一 409 + PO/STOCK_IN 审批夹具残留收口 · 验收证据

> 卡面：`t_2287c331`（来源 `t_06c528fc` §五的残留清单）
> 日期：2026-11-02 · 执行：backend-developer
> 基线代码：`16a8d36`（origin/main，含 V0192；136 个迁移）
> 涉及文件：`backend/src/business/flow/model.rs`、`flow/handler.rs`、
> `erp/transfer_handler.rs`、`erp/stockin_handler.rs`、`erp/purchase_handler.rs`、
> `erp/payable_payment.rs`、`erp/voucher_write.rs`、`scripts/verify/t2287_approval_fixture_twice.sh`

---

## 一、结论先行

| 口径 | 结果 |
|---|---|
| **验收①**：同一个干净库（`cloudpivot_t2287`）连跑两遍全量 `cargo test --lib -- --test-threads=1` | 第一遍 **1885 passed / 0 failed**（562.41s）· 第二遍 **1885 passed / 0 failed**（561.48s）· `scripts/verify/t2287_approval_fixture_twice.sh` 判 `PASS` |
| **验收②**：两遍跑完后的审批实例残留 | `select biz_type,status,count(*) from biz_flow_instance group by 1,2` → **0 行**（`PO_APPROVE` / `STOCK_IN_POST` 一条不剩）；`biz_flow_task` → **0 行** |
| **验收③**：其它模块撞 uk → 409 + 自家单号 | 新增 **4 条纯逻辑单测**（stockin / purchase / payable / voucher，各覆盖「变体」与「只剩 PG 原文」两条判据分支）+ 2 条模块级 `zz_` teardown 用例；`cargo clippy --lib -- -D warnings` 绿、`cargo fmt -p cloudpivot -- --check` 绿 |
| 修前（同一台机、旧代码 `cloudpivot_t06`：**一遍**全量） | `PO_APPROVE` 10 条实例（**PENDING 7**）· `STOCK_IN_POST` 9 条（**PENDING 5**）· `biz_flow_task` 22 行 —— **19 条实例的 `biz_id` 全部指向已不存在的单据**（孤儿） |
| 修前（父卡新代码 `cloudpivot_t06d`：**两遍**全量） | `PO_APPROVE` PENDING **14** · `STOCK_IN_POST` PENDING **10** · `biz_flow_task` 42 行 ← 正是卡面记的「PO 14~21 / STOCK_IN 10~15」 |

一句话：**每跑一遍全量，这个库就固定多出 7 条 `PO_APPROVE|PENDING` + 5 条 `STOCK_IN_POST|PENDING`**（`cloudpivot_t06` 一遍 = 7/5，`cloudpivot_t06d` 两遍 = 14/10，`cloudpivot_t06b` 约四遍 = 28/20，线性累积）。根因不是「用例忘了批」，而是**删单据时实例（弱引用）没跟着删**，于是每一轮的实例都变成 `biz_id` 指向死单据的孤儿，且清理语句按「活单据」定位永远找不到它们。修完两遍后实例数归零。

---

## 二、卡面第 1 条：uk 冲突文案在兄弟模块仍是「单号重复」

### 2.1 改法（把判据与文案收敛成一份实现）

`business/flow/model.rs` 新增三个 `pub` 项（**跨模块唯一实现**）：

```rust
pub const BIZ_ACTIVE_INDEX: &str = "uk_biz_flow_instance_biz_active";
pub fn is_biz_active_duplicate(error: &DbError) -> bool;          // 变体优先 + 文案兜底
pub fn approval_in_progress(biz_label: &str, biz_no: Option<&str>) -> ApiError;  // 409 + 文案
```

`approval_in_progress` 产出的文案与 transfer 完全一致：
`「<单据> <单号> 已有审批中实例（同一单据同时只能有一个在跑的审批单）：请刷新查看审批进度，或在审批中心撤回后再重新提交」`，
拿不到单号时退化成「该<单据>」。

接线（5 个模块，各在自己**写错误出口**加一条前置判据 + 在**拿得到单号的发起入口**用带单号版本）：

| 模块 | 写错误出口（兜底：`该<单据>`） | 带单号的入口 | 单据名词 |
|---|---|---|---|
| `erp/transfer_handler.rs` | `write_error` | `prepare_start` / `change_transfer_status` | 调拨单（原有，改为委托同一实现） |
| `erp/stockin_handler.rs` | `write_error` | `submit_post_tx` / `submit_cancel_tx` | 收货入库单 |
| `erp/purchase_handler.rs` | `write_error` | `submit_po_tx` | 采购订单 |
| `erp/payable_payment.rs` | `write_error` | 发起付款审批 / 发起应付作废审批 | 付款单 / 应付单 |
| `erp/voucher_write.rs` | `write_error` | `submit_reverse_tx` | 凭证 |

`flow/handler.rs` 里原来那份私有 `is_biz_active_duplicate` 删掉，改调 `model::is_biz_active_duplicate`
（它自己的 409 文案是「该业务单据已有正在审批中的审批单（并发提交被数据库挡下）」，属于通用流程接口，
**保持不变**，只换判据）。transfer 侧同理：保留同名薄封装，转发到 `flow_model::*`，行为零变化。

### 2.2 为什么四条分支都要钉住

| 分支 | 触发条件 | 旧行为 |
|---|---|---|
| `DbError::UniqueViolation{constraint}` | 正常路径，驱动从 PG 消息里解出约束名 | 409 + 「XX 单号重复（唯一约束 uk_biz_flow_instance_biz_active）：取号器与库里的号段不一致，请联系运维排查 biz_seq」→ **状态码对、文案把人指到取号器** |
| 只剩 PG 原文（`DbError::Query` / `InvalidValue`） | 事务内执行、驱动拿不到 `SqlState` | 掉到 `utils::db_error` → **500「服务器内部错误」**，可解释的冲突被彻底藏起来 |

单测（`biz_active_conflict_maps_to_409_with_*`，4 条）逐条断言：
`status==409`、`code==RESOURCE_CONFLICT`、**含自家单号**、含「已有审批中实例」、
**不含 `biz_seq`**、**不含「单号重复」**；同时反向断言「别的唯一索引（如 `uk_biz_stock_in_no`）走原分支、
仍然 409 + 取号器提示」——保证新判据没把真号段冲突一起吞掉。

---

## 三、卡面第 2 条：PO/STOCK_IN 审批夹具每轮留 PENDING（真因 + 修法）

### 3.1 真因（实测，不是猜）

`cloudpivot_t06`（旧代码，一遍全量）里 19 条实例的 `biz_id` **全部**指向已不存在的单据：

```sql
select i.status, i.biz_no, i.biz_id,
       (select order_no from biz_purchase_order p where p.id = i.biz_id) as bno
  from biz_flow_instance i where i.biz_type = 'PO_APPROVE' order by i.status;
-- 20 行，bno 列全为 NULL（biz_id 指向的采购单已被删）
```

链条是三段：

1. `biz_flow_instance.biz_id` 是**弱引用**（无外键）→ `DELETE FROM biz_purchase_order` / `biz_stock_in`
   带不走实例；
2. 各模块的清场**先删单据、且清实例时用「活单据」定位**
   （`biz_id IN (SELECT id FROM biz_purchase_order WHERE supplier_id = …)`）——
   单据一删，这条子查询就永远查不到那些实例了，于是每轮新增的实例都变成**孤儿**；
3. 没有任何模块级 teardown，模块**最后一个**用例（`purchase_handler` 的 `update_submitted_is_conflict`、
   `stockin_handler` 的 copy 用例）的产物没人收。

`cloudpivot_t06d`（父卡两遍）残留 38 条实例 / 42 行待办，正是「2 × (7+5) + 8 已结束」。

### 3.2 修法：把「痕迹」与「单据」拆开，并按**外键链**排死顺序

`purchase_handler::tests`：

- `po_instance_where()`：本夹具 `PO_APPROVE` 实例的定位口径 = 「`biz_id` 指向本夹具供应商的单」
  **或**「`biz_id` 为空 / 指向已不存在的单」（孤儿）—— 后者就是累积的那批；
- `purge_approval_traces()`：`approval_instance_id=NULL`（弱引用）→ `prev_instance_id=NULL`（自引用外键，无 ON DELETE）
  → `biz_flow_task` / `biz_flow_cc`（显式先删）→ `biz_flow_instance` → `biz_status_log`（无外键，必须在删单据前）；
- `purge_fixture_docs()` = 上面 + 删单据；**`setup()` 与 `setup_approval()` 都改走它**（旧版 setup 是裸删单据，
  这正是「每轮造一批孤儿」的机器）。

`stockin_handler::tests`：

- `post_instance_where()` 同口径（`STOCK_IN_POST` + `STOCK_IN_CANCEL`）；
- `purge_post_approval_traces()`：同 5 步 —— 但**弱引用那步要绕开 `ck_biz_si_pending_cancel`**：
  该库级约束要求「`PENDING_CANCEL` / `PENDING_POST` ⇒ `approval_instance_id` 非空」，
  无条件 `SET NULL` 直接 `check constraint violated`（第一版实测 33 条红就是这么来的）。
  现在的写法只给「非待审状态」的单解引用，待审的两类单据在 `cleanup_fixtures()` 里整行删掉；
- `cleanup_fixtures()` 开头先跑它（原来只清 `STOCK_IN_CANCEL` 实例，`STOCK_IN_POST` 从没清过）；
- 顺带补两条「孤儿日志」清理：`STOCK_IN` 的状态日志（entity_id 已指向不存在的收货单）、
  以及作废路径产生的 `PAYABLE` / `VOUCHER` 孤儿日志（活行的日志一条不动）。

两个模块各补一条模块级 teardown（`zz_` 开头 → libtest 按名排序必然最后）：

- `purchase_handler::tests::zz_teardown_purges_po_approval_residue`
- `stockin_handler::tests::zz_teardown_purges_post_approval_residue`

用例形态：自造一张**带在跑审批单**的单（断言清场前确实有 PENDING 待办）→ 走模块**真实**的清场路径
（`purge_fixture_docs()` / `cleanup_fixtures()`）→ 断言「单据 / 实例 / 待办 / 状态日志」按 id 全为 0，
并追加一条硬断言：**全库不许剩 `PO_APPROVE`（或 `STOCK_IN_POST`）的 PENDING 实例**。

---

## 四、验收原始输出

### 4.1 单测（新增 6 条）

```
$ cargo test --lib biz_active_conflict -- --test-threads=1
running 5 tests
test business::erp::payable_payment::tests::biz_active_conflict_maps_to_409_with_own_doc_no ... ok
test business::erp::purchase_handler::tests::biz_active_conflict_maps_to_409_with_order_no ... ok
test business::erp::stockin_handler::tests::biz_active_conflict_maps_to_409_with_stock_in_no ... ok
test business::erp::transfer_handler::tests::biz_active_conflict_maps_to_409_with_transfer_no ... ok
test business::erp::voucher_write::tests::biz_active_conflict_maps_to_409_with_voucher_no ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 1878 filtered out; finished in 0.00s
```

（`transfer` 那条是父卡已有，本卡只把它改成委托共享实现。）

### 4.2 模块级（先单独验，避免全量往返）

```
$ ERP_DB_URL=…/cloudpivot_t2287 cargo test --lib business::erp::purchase_handler -- --test-threads=1
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 1861 filtered out; finished in 30.36s
$ ERP_DB_URL=…/cloudpivot_t2287 cargo test --lib business::erp::stockin_handler -- --test-threads=1
test result: ok. 102 passed; 0 failed; 0 ignored; 0 measured; 1783 filtered out; finished in 192.38s
```

### 4.3 验收①：同一个干净库连跑两遍全量（脚本原文）

```
$ ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_t2287' \
    bash scripts/verify/t2287_approval_fixture_twice.sh
==> 第 1 遍：cargo test --lib -- --test-threads=1（日志 /tmp/t2287_twice/pass1.log）
    test result: ok. 1885 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 562.41s
    --- biz_flow_instance（biz_type, status, count）
    审批残留：PO/STOCK_IN 的 PENDING 实例 = 0，biz_flow_task = 0
==> 第 2 遍：cargo test --lib -- --test-threads=1（日志 /tmp/t2287_twice/pass2.log）
    test result: ok. 1885 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 561.48s
    --- biz_flow_instance（biz_type, status, count）
    审批残留：PO/STOCK_IN 的 PENDING 实例 = 0，biz_flow_task = 0
==> PASS：同一库连跑两遍全量都全绿，且 PO_APPROVE / STOCK_IN_POST 不再累积 PENDING
```

（`biz_type,status,count` 那一行下面是**空的**：整库 0 条实例。基线 1879 → 1885 = 新增 6 条用例。）

### 4.4 验收②：两遍跑完的库状态（`cloudpivot_t2287`）

```
instance|0        ← biz_flow_instance 一行不剩（PO_APPROVE / STOCK_IN_POST 零残留）
task|0            ← biz_flow_task
po|3              ← P1FIX-PO-1 / P1FIX-PO-2 / W1B-PO-0001（**别的卡的夹具单据**，非 PO/STOCK_IN 审批残留）
stock_in|1        ← P1FIX-IN-1（同上）
status_log|292    ← 见 §五「本卡范围外的观察」
```

对比（同一行 SQL）：`cloudpivot_t06`（旧代码一遍）= instance 20 / task 22；`cloudpivot_t06d`（父卡两遍）= 38 / 42。

### 4.5 静态检查

```
$ cargo fmt -p cloudpivot -- --check            # 绿（无输出）
$ cargo clippy --lib -- -D warnings             # 绿（Finished in 20.04s）
$ cargo clippy --lib --tests -- -D warnings     # 红，但红点全在存量文件：
    8 × tests/testd/impexp.rs
    7 × src/business/flow/internal_control.rs
    6 × tests/testd/project_log.rs
    3 × tests/testd/mutex_help_ai.rs
    3 × tests/testd/linkage.rs
```
本卡 touched 的 7 个文件 **0 条**（`git status --porcelain` 里这 7 个文件是本卡唯一改动）。

---

## 五、本卡范围外的观察（记录清楚，不认领也不掩盖）

1. **`biz_status_log` 里 `PAYABLE` / `VOUCHER` 的行仍在长**：pass1 结束时 18 + 61，pass2 结束时 56 + 154
   （其中孤儿 = 20 + 33）。它们由 `stockin_handler` **之后**才跑的模块产生（凭证/调拨/审批链路），
   本卡的两条 teardown 按设计只收「本模块删掉的单据」的孤儿，不越界改别人的夹具。
   属于同类隐患，建议由凭证/应付侧的卡按同一范式补 `zz_` teardown。
2. **`STOCK_IN` 孤儿日志已经稳定**：pass1 结束有存量，pass2 的 `cleanup_fixtures()` 开头会把它们收干净
   （实测 pass2 期间新建 40 条、跑完后 `entity_type='STOCK_IN'` 且 pre-pass2 的存量 = 0），
   即「不再累积」，这是本卡修好的那部分。
3. `ACCOUNTING_PERIOD` / `ACCOUNTING_YEAR` 的状态日志（42 行）是会计期间用例的正常产物，不在本卡范围。

---

## 六、与卡面的差异（逐条说明）

| # | 卡面写法 | 实际落地 | 原因 |
|---|---|---|---|
| 1 | 「把 `is_biz_active_duplicate` 抽到 `business/flow` 里公开（例如 `flow::model::…`）」 | 落在 `flow::model::{BIZ_ACTIVE_INDEX, is_biz_active_duplicate, approval_in_progress}` | 与卡面建议一致；顺带把 `flow/handler.rs` 的私有副本删掉（避免两份判据漂移） |
| 2 | 「四个模块的写错误出口各加一条前置判据，措辞与 transfer 对齐（各带自家单号）」 | 写错误出口用兜底文案（「该<单据>」），**另外**在拿得到单号的发起入口用带单号版本 | 写错误出口是通用出口，多数调用点拿不到单号；transfer 也是这么分的（`write_error` 传 `None`、`prepare_start` 传 `Some(no)`）。两个出口都已 409 + 「已有审批中实例」 |
| 3 | 「各模块按同一范式补 `zz_` teardown：先 task/cc → 解锁 prev_instance_id → 删实例 → 删状态日志 → 删单据」 | 完全按这个顺序落地（多一步「先断单据的 `approval_instance_id` 弱引用」） | 弱引用不先断，单据会指着不存在的实例，`instance_status_of` 查不到就按「不拦」放行（门闸静默失效） |
| 4 | 卡面残留数字「PO_APPROVE PENDING 14~21 / STOCK_IN_POST PENDING 10~15」 | 实测**每遍**固定 +7 / +5：一遍 7/5（`cloudpivot_t06`）、两遍 14/10（`cloudpivot_t06d`）、约四遍 28/20（`cloudpivot_t06b`） | 卡面数字是在同一库上跑 2~4 遍后的快照；本卡把「每遍增量」量了出来 |

---

## 七、部署口径（未上生产，如实说明）

本卡验收口径是**测试**，落地改动里只有 409 文案那一块是运行时可见的：

| 改动 | 是否需要发版才生效 |
|---|---|
| `flow::model` 的判据/文案 + 5 个模块的 409 映射 | **是**（用户点「提交审批」撞在跑实例时看到的话术） |
| 夹具清场 / teardown / 验收脚本 | 否（只在测试进程里跑，不进 release 二进制路径） |

按父卡 `t_06c528fc` 的口径（同一类改动「随下次发版生效」），本卡**未部署 144 生产**，也没有重编 release 二进制。
下次发版时无需迁移、无需开关，`grep -ac "已有审批中实例" target/release/cloudpivot` 应 >0（现有二进制里只有
transfer 那一处）。

## 八、复现方式

```bash
# 1) 干净库（owner 必须是应用账号 alun，否则启动只有一行「数据库迁移失败」没有细节）
runuser -u postgres -- psql -qc "DROP DATABASE IF EXISTS cloudpivot_t2287"
runuser -u postgres -- psql -qc "CREATE DATABASE cloudpivot_t2287 OWNER alun"
cd /opt/cloudpivot/backend
NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_t2287 NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun NIK_SERVER_LISTEN=127.0.0.1:18187 NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./target/debug/cloudpivot > /tmp/t2287_server.log 2>&1 < /dev/null &
# 等「数据库迁移完成」+ /health 200 + SUPER_ADMIN 有 admin 之后再停实例
# 2) 两遍验收（脚本自带残留断言）
cd /opt/cloudpivot
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_t2287' \
  bash scripts/verify/t2287_approval_fixture_twice.sh
```
