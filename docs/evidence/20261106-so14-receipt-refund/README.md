# 【SO-14 遗留】已收款的应收 + 出库作废 → 收款退款单（路线 A）交付证据

- 卡：`t_573bfe5b`（assignee manong），依赖父卡 `t_b33adc45`（SO-14 遗留：出库作废 → 应收冲销）
- 日期：2026-11-06
- 结论：**路线 A（收款退款单）已落地并端到端实测通过**；路线 B（红字应收 / 负金额）按卡面判定**不做**。
- 本目录即复跑入口：`README.md`（口径 + 验收）、`curl-e2e.sh`（51 断言）、`curl-round1.txt` / `curl-round2.txt`、
  `unit-tests.log`（真库单测 12/12）。

---

## 一、口径决定（本卡的核心，不是实现难度）

| # | 问题 | 决定 | 理由（可核对） |
|---|---|---|---|
| D1 | 路线 A（收款退款单）还是路线 B（红字应收 / 负金额） | **路线 A** | 路线 B 要放宽 `ck_biz_ar_amount (amount > 0)`，直接冲击账龄分母、`ck_biz_ar_unsettled` 恒等式与报表取数；父卡已判不推荐。路线 A 不动应收的金额域，只把「已收款」退回去。 |
| D2 | 复用 `biz_receipt` 加 `receipt_type`，还是新建表 | **新建 `biz_receipt_refund` + `biz_receipt_refund_allocation`（结构镜像，语义独立）** | ① V0292 已应用（指纹不可改），同表混进退款要再发迁移 DROP/ADD `ck_biz_receipt_amount` / `ck_biz_rcpt_alloc_amount (amount > 0)`；② SO-17 的收款读侧（列表 / 详情 / 核销 / 过账）全部按「收款」写死，退款行会混进收款台账、还可能被再次核销到应收；③ 复用可复用的是**算法骨架与凭证口径**，不是表。 |
| D3 | 冲减金额存正数还是负数 | **正数**（`amount > 0`），方向由单据类型表达 | 负号会污染 `SUM` / 对账 / 账龄；核销明细 `UNIQUE (refund_id, receivable_id)` ⇒ 同一退款对同一应收只有一行，重复核销 = 改金额（UPSERT）。 |
| D4 | 退款核销的应收侧写法 | `received_amount -= 本次冲减`，`unsettled_amount = amount - received`，状态 `SETTLED → PARTIAL → OPEN` | 恒等式不变（`received = SUM(收款核销) - SUM(退款冲减)`），`ck_biz_ar_unsettled` 继续兜底。 |
| D5 | 应收状态机要不要补反向边 | **补 3 条**：`SETTLED→PARTIAL`、`PARTIAL→OPEN`、`SETTLED→OPEN`（`RECEIVABLE_EDGES` 6 → 9） | 原来「故意没有 `SETTLED → *`」，但退款必须把状态推回来：否则 `status=SETTLED` 而 `received_amount=0`，与恒等式矛盾，且出库作废（要求 `OPEN 且 received=0`）永远走不通。**`PARTIAL/SETTLED → CANCELLED` 仍然没有边**，`CANCELLED ⇔ 一分没收` 的语义一个字没动。 |
| D6 | 退款单状态域 | `DRAFT / CONFIRMED / POSTED / CANCELLED`（与收款单同构），**`POSTED` 是终态** | 钱已付出且凭证已记账；作废会重复冲减。要抹掉只能由客户重新回一张收款单，或在凭证层红冲。 |
| D7 | 过账前置 | 必须**核销满**（`allocated == amount`）+ 期间未结账 | 与收款单同口径：未核销余额若借记 1122 就是凭空造一笔应收（真实的「付了还没抵账」应记 1123 预付，属 ARCH-7 §8.1 预留项）。 |
| D8 | 凭证方向 | **借 1122 应收账款 / 贷 1002 银行存款**，各 = 退款金额（`source_type='RECEIPT_REFUND'`，`PZ` 号段） | 与 SO-17 收款凭证严格镜像。 |
| D9 | 退款原因 | **建单即必填**（去空白 ≥ 4 字），程序 + 库层（`ck_biz_receipt_refund_reason`）双闸 | 退款是「钱往外走」，比收款（只在作废时要求原因）更严。 |
| D10 | 是否接审批流（与 `PAYABLE_PAY` 同级） | **本卡不接审批引擎**，用「原因必填 + 权限码 `business:erp:receipt-refund:*` + 状态机 + 过账前置」四道闸；审批接入另开卡 | 与 SO-17 收款单同级别（收款单本次也没接审批）；审批接入要动流程定义 / 画布 / 待办联动，与本卡的资金域口径混在一张卡里既难评审也难回滚。 |

### 与 SO-17（收款单 V0292）/ SO-16 的关系

- 退款单是**收款单的反向单据**：字段镜像（`refund_no / customer_id / refund_date / amount / allocated_amount / method / bank_account / status / voucher_id / 审计五件套`），多一列 `source_receipt_id`（弱引用，记录「退的是哪一笔收款」，不参与核销计算）与 `reason`（必填）。
- 核销明细镜像 `biz_receipt_allocation`（只是不全带 `updated_at/deleted_at`：核销行随单头级联删）。
- **不碰** `biz_receipt` 的任何既有约束 / 任何 SO-17 在途文件的语义（本次改动只加自己的模块与注册表条目）。

---

## 二、改动清单

### 迁移（`backend/migrations/business/`，`kind=versioned`）

| 文件 | 内容 |
|---|---|
| `V0294__sales_receipt_refund.up.sql` | `biz_receipt_refund`（18 列 / 5 CHECK / 6 索引）+ `biz_receipt_refund_allocation`（7 列 / 1 CHECK / 3 索引）+ `biz_seq(seq_type='RR')`；末尾 DO 块自检：列数逐字、CHECK 定义、域值、索引 10 个、默认值、外键动作（CASCADE / RESTRICT）、20 条行为探针、取号规则 |
| `V0294__sales_receipt_refund.down.sql` | 有 `status='POSTED'` 的退款行 → **主动拒绝**（凭证软引用会悬空，DI-17 会红）；否则删两张表 + 删 RR 取号行 |
| `V0295__sales_receipt_refund_permissions.up.sql` | 5 个权限码（1 页面 + 4 按钮，id …2950–2954），`path/component` 留 NULL 等人读前端卡；末尾自检 |
| `V0295__sales_receipt_refund_permissions.down.sql` | 先删 `sys_role_menu` 再删菜单 |

### 后端（`backend/src/business/`）

| 文件 | 内容 |
|---|---|
| `erp/receipt_refund.rs`（新增） | 模块文档 + 常量 / 纯逻辑 / 行结构 / 查询 / DTO / 三个写入口（create / allocate / change_status）+ 凭证生成 + 5 个 handler + 单测（10 条纯逻辑 + 2 条真库） |
| `shared/state_machine.rs` | `ENTITY_RECEIPT_REFUND`、`RECEIPT_REFUND_EDGES(3)`、`RECEIPT_REFUND_REASON_EDGES(1)`、`RECEIPT_REFUND_LOCKED`、`RECEIPT_REFUND_BASE(3)`、`Entity::ReceiptRefund`（ALL 17→18 + 6 处 match）；`RECEIVABLE_EDGES` 6→9 与 `RECEIVABLE_BASE` 6→9（3 条退款冲减边） |
| `shared/doc_kind.rs` | `DocKind::ReceiptRefund`（ALL 21→22 + 7 处 match）、`VoucherSourceType::ReceiptRefund`（ALL 15→16 + 4 处 match + 两个取值域常量） |
| `shared/doc_kind_tests.rs` | `VOUCHER_SOURCE` 登记域映射：`RECEIPT_REFUND → biz_receipt_refund` |
| `erp/mod.rs` | `pub mod receipt_refund;` + 模块表一行 |
| `erp/receivable.rs` | `LEGAL_EDGES` 6→9（跟着注册表）+ 文档边数说明 |
| `docs/architecture/snapshot-drift/10_checks.sql` | 巡检登记两行：`VOUCHER_SOURCE` / `STATUS_ENTITY` 的 `RECEIPT_REFUND → biz_receipt_refund` |
| `frontend/src/api/business/erp/doc-kinds.ts` | 词表镜像（`DOC_KINDS` / `DOC_KIND_LABELS` / `DOC_KIND_TABLES` / `VOUCHER_SOURCE_TYPES` / 标签）。`DOC_KIND_ROUTES` 故意**不加**：退款页面尚未落地（路由存在性由前端卡负责，`route()` 走 `_ => None`） |

接口（权限码与 `V0295` 的 `sys_menu.auth_code` 逐字一致）：

```
POST  /business/erp/receipt-refund                  business:erp:receipt-refund:create    登记（DRAFT）
GET   /business/erp/receipt-refund/{id}             business:erp:receipt-refund:detail    详情 + 冲减明细 + 凭证号
GET   /business/erp/receipt-refund/page             business:erp:receipt-refund:list      分页（状态 / 客户 / 方式 / 日期 / 单号）
POST  /business/erp/receipt-refund/{id}/allocate    business:erp:receipt-refund:allocate  冲减指定应收的已收款
PATCH /business/erp/receipt-refund/{id}/status      business:erp:receipt-refund:status    确认 / 过账 / 草稿作废
```

---

## 三、端到端验收（真实 token curl，连跑两轮）

```bash
BASE=http://127.0.0.1:18121/api DB=cloudpivot_rr14 \
  bash docs/evidence/20261106-so14-receipt-refund/curl-e2e.sh
```

```
curl-round1.txt：==> 汇总：PASS=51 FAIL=0   （rc=0）
curl-round2.txt：==> 汇总：PASS=51 FAIL=0   （rc=0）
```

覆盖的关键断言（原始输出见上面两个文件）：

1. **基线**：应收 `PARTIAL`（已收 100）时作废出库单 → **409**，报文点名「先冲收款」，且整笔回滚（出库单仍 `POSTED` / 库存不退回 / 无红字凭证）。
2. **登记退款**：`RR-yyyyMMdd-000001`，`DRAFT`，已冲减 0，原因落库；分页查得到。
3. **边界**：原因 < 4 字 → **400**；一分没收的 `OPEN` 应收不能退款 → **400/409**；冲减超过可退金额（100.0001 > 已收 100）→ **400**，且**整笔回滚**（应收仍是 `OPEN / 0`）。
4. **核销**：冲减 100 ⇒ 应收 `OPEN | received 0.0000 | unsettled 1130.0000`；状态日志 `PARTIAL>OPEN`。
5. **过账**：`CONFIRMED` → `POSTED`，凭证号回填；分录 **借 1122 = 100 / 贷 1002 = 100**；`biz_voucher.source_type='RECEIPT_REFUND'`、`source_no = 退款单号`；状态日志 3 条。
6. **终态**：已过账的退款再作废 → 409。
7. **目标链路**：再作废出库单 → **200 `CANCELLED`**（走的就是既有 `OPEN → CANCELLED`），库存回滚 20、出库红字凭证 1 张、应收 `CANCELLED | received 0.0000`。
8. **幂等 / 读侧**：重复作废 409；详情 / `page?status=POSTED` 正常；`page?status=PARTIAL`（应收的域值）在退款域里 400。

> 说明：演示库里的「已过账收款单 + 核销明细」是**按 V0292 表结构直接造账**的夹具（`RC-20261106-900001` / `biz_receipt_allocation` 100），因为 SO-17 的收款 API 本卡开工时仍在途未提交；夹具形态与 SO-17 核销后的账完全一致（`received_amount=100` / `PARTIAL` / 核销明细 100）。

---

## 四、门禁

| 门禁 | 命令 | 结果 |
|---|---|---|
| 格式 | `cargo fmt -- --check` | rc=0 |
| 静态 | `cargo clippy --all-targets --features test-hooks -- -D warnings` | rc=0（0 告警） |
| 编译 | `cargo check --all-targets --features test-hooks` | rc=0 |
| 单测（真库） | `ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_rr14 cargo test --lib business::erp::receipt_refund --features test-hooks -- --nocapture --test-threads=1` | **12 passed / 0 failed**（无 SKIP，含 2 条真库用例），见 `unit-tests.log` |
| 迁移（全新库） | 空库 `cloudpivot_rr14` + 引擎启动 | V0294 / V0295 `success=t`，两份自检 NOTICE 均通过 |
| 死权限码 | `python3 scripts/verify/perm_menu_gate.py` | **PASS**（5 个码都有菜单承载） |
| 闸②（数据权限） | `python3 scripts/lint/scope_gate.py` | 本卡**零新增违规**（`receipt_refund.rs` 全部命中 `SCOPE-EXEMPT` 登记）；仅剩既有 `business/erp/shipment_page_fill_tests.rs` SKIP_MANIFEST 一条 FAIL（与本卡无关，父卡已记） |

真库单测里的两条真库用例（非 SKIP）：

- `refund_restores_receivable_to_open_and_posts_voucher`：登记 → 核销（`PARTIAL 100` → `OPEN 0`）→ 确认 → 过账（分录方向 + 凭证号 + 状态日志 3 条 + POSTED 后作废 409）
- `refund_rejects_over_refund_and_cross_customer`：超冲减 400 / 跨客户 400 / 一分没收 409 / 未核销满过账 409

---

## 五、复跑环境

实例：`/var/tmp/t_573bfe5b-run/cloudpivot` @ `127.0.0.1:18121`，库 `cloudpivot_rr14`（全新库，引擎迁到 V0295）。
本轮验收后实例仍可复跑（`bash /var/tmp/t_573bfe5b-run/start.sh`，然后用例脚本自带清场）。

```bash
# 库
su postgres -c "createdb -O alun cloudpivot_rr14"

# 实例（工作目录要有 config/ 与 migrations/，软链到本卡 worktree 的 backend）
cd /var/tmp/t_573bfe5b-run
NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_rr14 NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_PORT=5432 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:18121 NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./cloudpivot > server.log 2>&1 &
curl -s http://127.0.0.1:18121/health     # {"code":0,...,"profile":"test"}
```

工作树：`/var/tmp/t_573bfe5b-wt`（分支 `fix/t_573bfe5b`，基线 `4bc4694`）。开工时主仓 `/opt/cloudpivot` 有并行卡 SO-17 的未提交改动（`receipt.rs` / `V0293` / `state_machine.rs` / `doc_kind.rs` / `mod.rs` / `10_checks.sql` / `doc-kinds.ts`），**本卡全程在独立 worktree 完成**，没有触碰、没有提交那些在途改动。

---

## 六、已知边界（留档，不是缺陷）

1. **退款页面**未落地：`sys_menu` 的页面节点 `path/component` 留 NULL、`is_visible=FALSE`，`DocKind::ReceiptRefund.route()` 返回 `None`。前端卡落地后一条 UPDATE 收尾（照 V0291 手法）。
2. **审批流**未接（D10）：与 SO-17 收款单同级别。若要接 `PAYABLE_PAY` 同级审批，需要 `FlowBizType::ReceiptRefund` + 画布 + 待办联动，建议另开卡。
3. **退款不能冲「一分没收」的应收**：那是 `OPEN → CANCELLED` 的活（父卡已交付）。报错文案已明说。
4. **POSTED 不可作废**：要撤销已付出的钱，走「客户重新回款（新收款单）」或凭证层红冲。
5. `biz_receipt_refund.source_receipt_id` 是**弱引用**（无外键）：只做审计线索与建单校验（必须是同客户、已过账、金额不超），不参与核销计算。
