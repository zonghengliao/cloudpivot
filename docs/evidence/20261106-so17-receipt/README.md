# SO-17 收款后端 API + 核销 —— 证据包

卡号 **t_d071ab1c**；分支 `main`（99 工作仓）；交付物：`backend/src/business/erp/receipt.rs`
+ 迁移 `V0293` + 共享注册表（state_machine / doc_kind）+ 前端词表镜像。

## 1. 交付的文件（改了什么、为什么）

| 文件 | 动作 | 说明 |
|---|---|---|
| `backend/src/business/erp/receipt.rs` | **新增**（约 1.4k 行，含 11 条单测） | 5 个接口：`POST /receipt`、`GET /receipt/{id}`、`GET /receipt/page`、`POST /receipt/{id}/allocate`、`PATCH /receipt/{id}/status`。核销算法照搬 `payable_payment::settle_payment_in_tx` 的「锁 → 校验 → 落记录 → 回写 → 状态机 → 凭证」骨架，方向由「多付款 → 一张应付」翻成「一张收款 → 多张应收」 |
| `backend/src/business/erp/mod.rs` | 改 | 挂 `pub mod receipt;` + 模块表一行说明 |
| `backend/src/business/shared/state_machine.rs` | 改 | 注册实体 `Entity::Receipt`（7 处）+ `RECEIPT_EDGES`(3 条) / `RECEIPT_BASE` / `RECEIPT_REASON_EDGES`(DRAFT→CANCELLED) / `RECEIPT_LOCKED`(POSTED,CANCELLED)；`Entity::ALL` 17 → 18 |
| `backend/src/business/shared/doc_kind.rs` | 改 | `DocKind::Receipt`（ALL 21→22 / as_str / label / table / is_document / route / DOC_KIND_VALUES）+ `VoucherSourceType::Receipt`（ALL 15→16 / as_str / label / doc_kind / VOUCHER_SOURCE_VALUES） |
| `backend/src/business/shared/doc_kind_tests.rs` | 改 | `voucher_source_target_table` 补 `RECEIPT => biz_receipt`（否则 DI 登记表断言红） |
| `frontend/src/api/business/erp/doc-kinds.ts` | 改 | 前端词表镜像四处：`DOC_KINDS` / `DOC_KIND_LABELS` / `DOC_KIND_TABLES` / `DOC_KIND_ROUTES` / `VOUCHER_SOURCE_TYPES` / `VOUCHER_SOURCE_TYPE_LABELS`（后端单测 `frontend_ts_mirror_matches_rust` 直接读它比对） |
| `docs/architecture/snapshot-drift/10_checks.sql` | 改 | 巡检登记两行：`STATUS_ENTITY.RECEIPT → biz_receipt`、`VOUCHER_SOURCE.RECEIPT → biz_receipt` |
| `backend/migrations/business/V0293__sales_receipt_permissions.{up,down}.sql` | **新增** | 5 个权限码承载：页面节点 …2890（`…:list`，path/component 留 NULL、`is_visible=FALSE`，等 SO-18 落地后照 V0291 一条 UPDATE 收尾）+ 4 个按钮 …2891-2894（detail/create/allocate/status）；不写 `sys_role_menu` |

## 2. 口径决策（写进代码注释，这里列摘要）

1. **核销可发生在 `DRAFT` / `CONFIRMED`**，`POSTED` / `CANCELLED` 一律 409（过账后金额与核销被凭证锁定）。
2. **过账要求核销满**（`allocated_amount == amount`），否则 409 点名未核销余额。原因：卡面凭证是两行
   `借 1002 / 贷 1122`，未核销余额贷记 1122 就是凭空冲减应收账款（真实业务应贷 2203 预收账款，ARCH-7 §8.1 列为「预留」）。
   这条同时把不变量 S5（`sum(allocation) = allocated_amount`）钉死。
3. **一单对同一应收只有一行**：`INSERT … ON CONFLICT (receipt_id, receivable_id) DO UPDATE SET amount`
   （唯一索引 `uk_biz_rcpt_alloc_pair`），重复核销 = 改金额，SUM 不会重复计入。
4. **应收可核销余额 = `unsettled_amount + 本收款此前已核过它的金额`**：本收款之前核的那部分已经计入
   `received_amount`，不先加回来会把「改大金额」误判成超额。
5. **锁顺序固定**：先锁收款头，再按 `receivable_id` 升序逐把锁应收 —— 两笔收款并发核同一批应收不会交叉死锁。
6. **`allocated_amount` 以库里 `SUM(明细)` 为准**回写（不信内存累加），并且每次核销都校验
   `SUM ≤ 收款金额`（400 点名单号与两个金额）。
7. 只允许核销**同一客户**名下的应收（客户不一致 400）；终态应收（SETTLED/WRITTEN_OFF/CANCELLED）409。
8. 过账 / 核销都过 `period::ensure_period_writable`（已结账期间 400）。
9. STATE 图故意**没有** `CONFIRMED → CANCELLED`：确认过的收款要作废得先反核销、走红冲单，
   不能一步抹掉（`POSTED → *` 同理）。

## 3. 门禁结果（真实输出，原始文件在本目录）

### 3.1 `cargo check --lib` —— 零警告零错误
```
Checking cloudpivot v0.1.0 (/opt/cloudpivot/backend)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 31.21s
```

### 3.2 单测
| 目标 | 结果 |
|---|---|
| `cargo test --lib business::erp::receipt` | **11 passed / 0 failed** |
| `cargo test --lib business::shared::`（state_machine + doc_kind 注册表漂移） | **113 passed / 0 failed** |
| `cargo test --lib business::erp::receivable`（相邻模块回归） | **45 passed / 0 failed** |

`receipt::tests` 覆盖：状态域 4 值 / 可核销状态 / 边表与共享注册表一致（含 `Entity::parse`、`reason` 边 ⊆ 合法边、
`describe_next`）/ 凭证来源注册（`VoucherSourceType::Receipt → DocKind::Receipt → biz_receipt`）/
金额与余额算术（含 4 位小数）/ 方式白名单 / 单张上限（文案点名单号与金额）/ 汇总上限 / 应收状态推导
（OPEN→PARTIAL→SETTLED，不含 payable 的 `PARTIAL_PAID`）/ 期间编码与日期形状 / 作废原因长度边界。

> 全量 `cargo test --lib`（2800+ 条、25–40 分钟）本卡**没有**跑：单卡 ≤1.5h 的预算内跑不完，
> 且会与并行卡的 cargo 抢 target 锁。已按模块跑了受影响的三组（receipt / shared / receivable）。

### 3.3 真实 token curl 实测（隔离库 `cloudpivot_so17`，端口 8023）
`round-1-curl.out.txt` / `round-2-curl.out.txt`：**连跑两轮，各 61 项 PASS / 0 FAIL**。

关键实测（原文摘录）：
```
==> 4. 核销 POST /receipt/{id}/allocate（一条收款 → 两张应收）
      → {"code":0,"allocated":"1000.0000","unallocated":"0.0000","rows":2}
      明细：[{"ar_no":"AR17-002","amount":"400.0000","ar_status":"PARTIAL","ar_unsettled_amount":"100.0000"},
             {"ar_no":"AR17-001","amount":"600.0000","ar_status":"SETTLED","ar_unsettled_amount":"0.0000"}]
  PASS  库：AR-001 received/unsettled/status（600.0000|0.0000|SETTLED）
  PASS  库：AR-002 received/unsettled/status（400.0000|100.0000|PARTIAL）

==> 5. 超额核销被拒
  PASS  单张超额 → 400（10001）
      文案：本次核销 301 超过应收 AR17-003 的可核销余额 300.0000（未收金额 + 本收款此前已核销的金额）：…
  PASS  核销总额超过收款金额 → 400（10001）
      文案：核销总额 1001.0000 超过收款金额 1000.0000（收款单 RC-20261106-000001）：…
  PASS  核到别家客户的应收 → 400（10001）
  PASS  核到已结清应收 → 409（10003）
  PASS  同一应收重复提交 → 400（10001）

==> 6. 状态迁移 PATCH /receipt/{id}/status
      → {"code":0,"status":"POSTED","voucher_no":"PZ-20261106-000001"}
  PASS  DRAFT→POSTED 跳边 → 409（10003）
  PASS  CONFIRMED→POSTED（POSTED）
  PASS  凭证来源 = RECEIPT（RECEIPT）
  PASS  凭证状态 = POSTED（POSTED）
  PASS  分录：借 1002 / 贷 1122 / 各 1000（1002:D:1000.0000,1122:C:1000.0000）
  PASS  POSTED 后再核销 → 409（10003）
  PASS  未核销满 → 过账 409（10003）
  PASS  biz_status_log(RECEIPT)：NULL→DRAFT , DRAFT→CONFIRMED , CONFIRMED→POSTED
  PASS  应收核销留痕：OPEN→SETTLED , OPEN→PARTIAL
  PASS  汇总：biz_receipt.allocated_amount = SUM(明细)（t）
```

### 3.4 迁移
- `V0293` 在隔离库上是**引擎自动应用**的（首次启动 210 个迁移后应用 220 → 含 V0292/V0293），
  启动日志：`cloudpivot 启动完成, 耗时 10291ms` + `/health = 200`。
- 迁移自带 DO 块自检 5 个权限码 + 4 个按钮父级 + 页面 `is_visible=FALSE` / `path IS NULL`，任一不符即失败。
- ⚠ 踩坑记录：`sys_menu.title` 是 `VARCHAR(100)`，最初的标题文案超长导致首次启动迁移失败
  （引擎只报 `query failed: : db error`，得用 `psql -f` 单跑才看到 `value too long for type character varying(100)`）。

## 4. 复跑方法

```bash
# 1) 隔离库
su - postgres -c "createdb -O alun cloudpivot_so17"
# 2) 起实例（dev 的三个安全密钥必须显式注入，否则启动守卫拒绝）
cd backend && \
  NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_NAME=cloudpivot_so17 \
  NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=so17e2eprobe0123456789abcdef0123456789abcdef \
  NIK_ADMIN_PASSWORD=Admin@123456 \
  CLOUDPIVOT_STORAGE_MASTER_KEY=0123…（64 hex） X_CONFIG_SECRET_KEY=0123…（32 hex） \
  cargo run
# 3) 两轮 curl
BASE=http://127.0.0.1:8023/api DB=cloudpivot_so17 \
  bash docs/evidence/20261106-so17-receipt/curl-5-endpoints.sh
```

## 5. 前端门禁（本卡改了 frontend/src/api/business/erp/doc-kinds.ts）

```
$ cd frontend && pnpm typecheck
> vue-tsc --noEmit        # 无输出 = 通过

$ NODE_OPTIONS=--max-old-space-size=4096 pnpm build
dist/assets/tdesign-B9Yx93ic.js.gz  7994.62kb / gzip: 772.17kb   ← 构建成功
```
`doc-kinds.ts` 是后端 `doc_kind_tests::frontend_ts_mirror_matches_rust` 逐字读取的镜像文件，
新增的 `RECEIPT` 必须与 Rust 枚举同序同行，单测已覆盖（`business::shared::` 113/113 PASS）。

## 6. 环境清理

验证完成后：停掉隔离实例（`:8023` 无监听）、`dropdb cloudpivot_so17`。
复跑按第 4 节即可重建。
