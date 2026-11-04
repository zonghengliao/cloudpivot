# P2-5 业务类型词表收敛 —— 证据

> 卡：`t_e35c82ea`【P2-5】业务类型词表收敛
> 盘点文档（卡面最重要的产出之一）：`docs/architecture/P2-5-业务类型词表盘点.md`
> 词表唯一真相：`backend/src/business/shared/doc_kind.rs`（Rust）/ `frontend/src/api/business/erp/doc-kinds.ts`（TS 镜像）

所有数字都是**本机实跑**的产物，不是推断。复跑命令在每节末尾。

---

## 0. 一句话结论

同一张采购单确实有多个名字，但它们分属**七根不同的概念轴**；真正需要改数据的只有
`biz_status_log.entity_type` 的调拨值 `TRANSFER → STOCK_TRANSFER`（生产 1 行）。
词表收敛为一个 Rust 模块 + 一个 TS 镜像 + 一层查库巡检，**编译期 / 单测 / 巡检**三层拦截，
采购单审批全链路在隔离实例上真机跑通（发起 → 通过）。

---

## 1. 改动清单

| 类别 | 文件 |
|---|---|
| 词表（新） | `backend/src/business/shared/doc_kind.rs`（七根轴 + 跨轴映射 + 14 列值域配置） |
| 词表单测（新） | `backend/src/business/shared/doc_kind_tests.rs`（15 条断言） |
| 前端镜像（新） | `frontend/src/api/business/erp/doc-kinds.ts` |
| 巡检（新） | `scripts/verify/biz_type_domain.py`（只读，`--self-check` 自证） |
| 迁移（新） | `backend/migrations/business/V0219__erp_p2_5_biz_type_vocabulary.{up,down}.sql` |
| 后端改 | `shared/state_machine.rs`（实体类型常量改为从枚举取，`TRANSFER`→`STOCK_TRANSFER`）、`erp/cancel_cascade.rs`（路径段走枚举、删别名归一）、`erp/fixture_hygiene.rs`、`erp/transfer_handler.rs`（2 处清场 SQL）、`erp/mod.rs`（`stockin_query` 提为 pub） |
| 前端改 | `api/business/erp/{attachment,cancel-cascade,index}.ts`、`views/business/erp/{purchase,stockin,payable,contract}/index.vue`、`inventory/inventory-logic.ts`、`serial/serial-logic.ts`、`views/business/flow/{flow-business-items.ts,components/TaskPreview.vue,components/notify-node-config.ts}`、`tests/unit/erp-inventory.test.ts` |
| 门禁/脚本改 | `scripts/lint/scope_gate.py`（SKIP_MANIFEST 登记新测试面）、`scripts/verify/testcase_gen/state_machines.py`（`entity_type` + 错表名） |

---

## 2. 验收项逐条对账

| 卡面验收项 | 证据 | 结果 |
|---|---|---|
| □ 盘点清单完整贴进卡 | `docs/architecture/P2-5-业务类型词表盘点.md` §2（七根轴 × 每列 × 生产库 distinct 行数） | ✅ |
| □ 统一枚举定义（Rust + TS 两边） | `doc_kind.rs`（7 枚举 + `COLUMN_DOMAINS`）、`doc-kinds.ts`（9 个 `as const` 数组 + 联合类型 + 标签表） | ✅ |
| □ 映射表 + 数据迁移（带 down） | 盘点文档 §5；`V0219` up/down；往返实测见 §5（本文） | ✅ |
| □ 存量数据全部统一（前后 distinct 对比） | 本文 §3（生产库 before/after）+ §4（14 列全量巡检） | ✅ |
| □ 11 条预置审批流的 biz_type 已同步 | 本文 §3.2（原本就统一，11 条全在 `FlowBizType` 域内；迁移日志留档） | ✅（无需改，已核实） |
| □ 前端所有硬编码已改 | 本文 §6（grep 前后对比） | ✅ |
| □ grep 确认没有第 8 套词表 | 本文 §6（找到并消灭的那一套 + 改完后的 grep 结果） | ✅ |
| □ 加一条巡检：值域校验 | `scripts/verify/biz_type_domain.py`；负例（造脏数据 → FAIL → 清掉 → PASS）见 §4.2 | ✅ |
| □ 不变量全绿 | 本文 §4.3（隔离库 40/40，`BLOCKER 0 · WARN 0`） | ✅ |
| □ cargo test 全绿 | 本文 §7 | ✅ |
| □ 前端 build 通过 | 本文 §8（`vue-tsc --noEmit` + `vite build`） | ✅ |
| □ 真机：采购单审批发起 + 通过 | 本文 §5（14/14 断言，实例 `biz_type=PO_APPROVE`，日志 `entity_type=PURCHASE_ORDER`） | ✅ |
| □ git commit（`refactor: P2-5 …`） | 见提交记录 | ✅ |

---

## 3. 生产库（144）迁移前后对比

> 只读统计；迁移由 `systemctl restart cloudpivot` 时的自动迁移执行（`V0219`）。

### 3.0 巡检口径在生产库上的「迁移前」实测（`prod-before.txt`）

```
$ （144 上，只含 doc_kind.rs + 巡检脚本的小树）
[FAIL] ① 业务对象      biz_status_log.entity_type               取值 9 种
       ✗ 域外值 'TRANSFER' × 1
...
注：37 行是 e2e 脚本写进本库的测试残留（E2E_*/D2_SMOKE），已单列不计红；要判红请加 --strict
结果：FAIL（1 列有域外值）
```

**唯一一条红就是本卡要改的那一行**（`TRANSFER`）。

> **关于那 37 行 `E2E_*`**：来自**别的卡把 e2e 脚本跑在了生产库上**
> （`biz_flow_instance` 里的 `E2E_AP_*` 实例、`biz_flow_def` 里 132 条软删的 `E2E_*` 流程）。
> 这不是本卡的词表问题，本卡也**没有**改动它们。巡检的处理方式是：
> ① 软删行不参与判定（`deleted_at IS NULL`）；② `E2E_*` / `D2_SMOKE` 单列成
> 「测试残留」逐个点名计数但**不计红**，要严格判定加 `--strict`。
> 这是**具名例外**（不是静默放行）：任何一次运行都会把残留行数和值打出来。
> 数据卫生本身另开卡跟进（见 §10）。

### 3.1 `biz_status_log.entity_type`

| 时点 | distinct 值（行数） |
|---|---|
| 迁移前 | `VOUCHER`(27) `PURCHASE_ORDER`(23) `STOCK_IN`(20) `STOCK_OUT`(15) `CONTRACT`(11) `PAYABLE`(8) `FLOW_EDGE_CFG`(4) `ACCOUNTING_PERIOD`(1) **`TRANSFER`(1)** |
| 迁移后 | 同上，但 **`TRANSFER` 消失，`STOCK_TRANSFER`(1)** |

原始 distinct 全量 dump：`prod-distinct-before.txt`（16 列 / 197 行，14 张表）。

### 3.2 `biz_flow_def` / `biz_flow_instance`

11 条已发布流程的 `biz_type` 迁移前后**逐字未变**（本来就在域内）：

```
contract_approve | CONTRACT_APPROVE | PUBLISHED
flow_edge_config | FLOW_EDGE_CFG     | PUBLISHED
payable_cancel   | PAYABLE_CANCEL   | PUBLISHED
payable_pay      | PAYABLE_PAY      | PUBLISHED
po_approve       | PO_APPROVE       | PUBLISHED
stock_in_cancel  | STOCK_IN_CANCEL  | PUBLISHED
stock_in_post    | STOCK_IN_POST    | PUBLISHED
stock_take_post  | STOCK_TAKE_POST  | PUBLISHED
stock_transfer   | STOCK_TRANSFER   | PUBLISHED
task_accept      | TASK_ACCEPT      | PUBLISHED
voucher_reverse  | VOUCHER_REVERSE  | PUBLISHED
```

`biz_flow_instance.biz_type` 也**不迁移** —— 实测本来就是 `PO_APPROVE`（与定义同名），
卡面假设的 `purchase_order` 在生产库里**不存在**（见盘点文档 §3）。

---

## 4. 巡检（值域校验）

### 4.1 正向：干净隔离库

```
$ NIK_DATABASE_NAME=cp_p25_vocab python3 scripts/verify/biz_type_domain.py
业务类型词表值域巡检 —— 14 列 / 12 行（库 alun@127.0.0.1:5432/cp_p25_vocab）
[PASS] ① 业务对象      biz_status_log.entity_type               取值 0 种
...
[PASS] ② 审批动作      biz_flow_def.biz_type                    取值 11 种
...
结果：PASS（所有列的值都在 doc_kind.rs 的词表域内）
```

### 4.2 负例：故意造脏数据 → FAIL → 清掉 → PASS

`dirty_negative.sh` / `dirty_negative.log`：

```
insert ... entity_type='TRANSFER'  →  [FAIL] biz_status_log.entity_type  ✗ 域外值 'TRANSFER' × 1   EXIT=1
delete ...                          →  结果：PASS                                              EXIT=0
```

### 4.3 `--self-check`（自证只读）

```
$ python3 scripts/verify/biz_type_domain.py --self-check
PASS：巡检脚本只读（所有 SQL 字面量都以 SELECT 开头，无 INSERT/UPDATE/DELETE/DDL）
```

### 4.4 不变量套件

```
$ NIK_DATABASE_NAME=cp_p25_vocab python3 scripts/verify/erp_invariants.py
结果：40/40 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。
```

> 卡面写「不变量 32/32」，实际套件已扩到 **40 条**（后续卡新增了 8 条），
> 这里按当前套件的 40/40 报。

---

## 5. 真机：采购单审批全链路（隔离实例）

`e2e_po_approval.py`（库 `cp_p25_vocab`，实例 `127.0.0.1:18145`）：

```
[PASS] ① 采购单建单成功（DRAFT）
[PASS] ② 提交审批 200
[PASS] ② 返回 approval_instance_id
[PASS] ② 审批实例 biz_type == 流程定义 biz_type == PO_APPROVE
[PASS] ② 状态日志 entity_type = PURCHASE_ORDER（业务对象轴）
[PASS] ③ 审批发起后有待办
[PASS] ③ 审批通过 200
[PASS] ③ 采购单落 APPROVED
[PASS] ③ 审批实例落 APPROVED
[PASS] ④ 规范路径段 purchase-order → 影响面 200
[PASS] ④ 历史别名 purchase_order 被拒（400 且点名规范写法）
[PASS] ④ 历史别名 PO 被拒（400 且点名规范写法）
[PASS] ④ 历史别名 purchaseOrder 被拒（400 且点名规范写法）
[PASS] ⑤ 审批跑完后词表巡检仍 PASS
结果：14 通过 / 0 失败
```

这一组断言正好钉住「词表收敛没把路由弄断」：
**审批轴**（`biz_flow_instance.biz_type = PO_APPROVE` = 流程定义值）和
**对象轴**（`biz_status_log.entity_type = PURCHASE_ORDER`）各取其值、各自正确，
而路径段只认 kebab 规范写法。

### 5.1 迁移往返（up → down → up，有真实调拨日志）

`migration_roundtrip.py`：

```
[info] 调拨单 TK-20261104-000001 id=01a1a57c-821c-7599-bc63-f73aee01f12e
[PASS] ① 迁移前调拨日志 = STOCK_TRANSFER
[info] 执行 V0219 down.sql   →  NOTICE: V0219 down：调拨状态日志已回到 entity_type = 'TRANSFER'
[PASS] ② down 之后回到旧值 TRANSFER
[info] 执行 V0219 up.sql     →  NOTICE: V0219：调拨状态日志已统一为 STOCK_TRANSFER
[PASS] ③ up 之后再次统一为 STOCK_TRANSFER
[PASS] ④ 往返之后词表巡检仍 PASS
```

---

## 6. 「没有第 8 套词表」的 grep 证据

### 6.1 改之前：找到的那一套

```
$ grep -n "BIZ_TYPE_LABELS" -A20 frontend/src/views/business/erp/inventory/inventory-logic.ts
PURCHASE_RETURN_OUT / SALE_RETURN_IN / PRODUCE_IN / MATERIAL_ISSUE_OUT / INIT_IN
```

→ 5 个 key 在后端**一处都没有**（`grep -rn "PURCHASE_RETURN_OUT" backend/src/` = 0 命中），
是前端自造的「永远不会被命中」的中文标签。

### 6.2 改之后：前端剩余字面量

```
$ grep -rn "'PO_APPROVE'\|'PURCHASE_ORDER'\|'STOCK_TRANSFER'\|'PURCHASE_IN'\|'PAYABLE_PAY'" \
    frontend/src --include=*.ts --include=*.vue | grep -v doc-kinds.ts
```
剩余命中只在这几处，全部**合法**：

| 命中 | 为什么合法 |
|---|---|
| `api/business/erp/index.ts:801` / `transfer-logic.ts:17` | 注释里的文档说明 |
| `flow/flow-business-items.ts` 3 个常量声明 | 常量**声明处**，右侧字面量有 `: FlowBizType` 类型注解（写错编译不过）—— 词表引用的唯一出口 |
| `TaskPreview.vue:166` | 注释，说明旧值为什么错 |

### 6.3 后端剩余 `"TRANSFER"` 字面量

```
$ grep -rn '"TRANSFER"' backend/src --include=*.rs
changelog/diff.rs:325        FlowAction::Transfer => "TRANSFER"   ← 变更日志动作名，不是实体类型
erp/serial_model.rs:154      SOURCE_TYPES = ["PURCHASE","TRANSFER",...]  ← 序列号来源轴（保留）
shared/state_machine.rs:67   注释（说明旧值）
shared/doc_kind.rs           SerialSourceType::Transfer / SERIAL_SOURCE_VALUES（序列号来源轴）
shared/doc_kind_tests.rs     负例断言：DocKind::parse("TRANSFER") 必须为 None
```

即：`TRANSFER` 作为**实体类型**已经不存在，只作为**序列号来源**和**变更动作名**保留（不同轴）。

---

## 7. cargo test

见 `cargo-test.log`。跑法（干净库 + 独立进程，`--test-threads=1`）：

```bash
ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cp_p25_test" \
P0FIX_DB_URL="postgres://alun:alun@127.0.0.1:5432/cp_p25_p0fix" \
NIK_PROFILE=test NIK_MIDDLEWARE_AUTH_JWT_SECRET=... CLOUDPIVOT_STORAGE_MASTER_KEY=... \
  cargo test --lib -- --test-threads=1
```

---

## 8. 前端

```
$ npx vue-tsc --noEmit            # 0 error
$ npx tsx --test "tests/unit/**/*.test.ts"
# tests 626 / pass 625 / fail 1     ← 唯一的 fail 是另一张卡（vue-flow 画布）未完成的 WIP
$ pnpm build                      # 见 build.log
```

> 那条 fail 是 `tests/unit/flow-canvas-vueflow-contract.test.ts`（缺 `autoFit` prop），
> 对应文件 `FlowCanvasSwitch.vue` / `FlowCanvasVueFlow.vue` 在本次改动前就是**未跟踪的新文件**
> （别的卡 WIP），与本卡改动无交集。

---

## 9. 复跑

```bash
# 词表单测
cd backend && NIK_PROFILE=test NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  cargo test --lib business::shared::doc_kind -- --test-threads=1

# 巡检（任意库）
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_DATABASE_NAME=<库> python3 scripts/verify/biz_type_domain.py

# 真机（先起隔离实例；脚本自带幂等登录 + 夹具）
python3 docs/evidence/20261104-p2-5-biz-type/e2e_po_approval.py
python3 docs/evidence/20261104-p2-5-biz-type/migration_roundtrip.py
```
