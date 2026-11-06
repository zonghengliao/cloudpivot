# SO-18 收款单前端页面 —— 证据

卡：`t_4d37c180`（【SO-18】收款单前端页面），承接 `t_d071ab1c`（SO-17 收款后端 API + 核销）。
环境：隔离库 `cloudpivot_so18` @ `127.0.0.1:5432`，后端 `127.0.0.1:8023`，前端 dev `127.0.0.1:5173`。

交付物：
- `frontend/src/views/business/erp/receipt/index.vue`（列表 + 登记弹窗 + 核销面板 + 详情抽屉 + 确认/过账/作废）
- `frontend/src/views/business/erp/receipt/receipt-logic.ts`（纯逻辑，被单测钉住）
- `frontend/tests/unit/erp-receipt.test.ts`（23 条用例）
- `frontend/src/api/business/erp/index.ts`（5 个接口 + 类型）
- `frontend/src/router/index.ts`（`business/erp/receipt` → `BusinessErpReceipt`）
- `backend/migrations/business/V0294__sales_receipt_menu_page.{up,down}.sql`（菜单落地收尾）

## 1. 前端门禁

```
pnpm typecheck   # vue-tsc --noEmit 通过（零输出）
pnpm test:unit   # 964 tests / 0 fail（其中 erp-receipt 23 条）
NODE_OPTIONS=--max-old-space-size=4096 pnpm build   # 通过，产物 dist/index.html + assets/*
```

## 2. 接口实测（真 token curl，连跑两轮）

脚本：`verify-receipt.sh`（只打前端页真实调用的 5 个接口）；种子：`seed-receivables.sql`。

```
psql -v round=1 -f seed-receivables.sql
OLD_PW='So18@Pass123' bash verify-receipt.sh 1   → PASS=30 FAIL=0  (RC-20261106-000003 / PZ-20261106-000002)
psql -v round=2 -f seed-receivables.sql
OLD_PW='So18@Pass123' bash verify-receipt.sh 2   → PASS=30 FAIL=0  (RC-20261106-000004 / PZ-20261106-000003)
```

覆盖点：登记（单号 `RC-yyyyMMdd-000001`、DRAFT、未核销=全额、金额 0 → 400）；核销（核到别家客户 400、
单张超额 400、合计超额 400、一次两张、已核销 900 未核销 100）；**应收状态变化**（600 核满 → `SETTLED` 未收 0，
400 核 300 → `PARTIAL` 未收 100，补核 100 → `SETTLED`）；确认；**未核销满过账 → 409**；
过账生成凭证（1002 借 1000 / 1122 贷 1000，查 `biz_voucher_entry`）；列表筛选 + 分页；
详情核销明细 2 行；过账后再核销 → 409。

日志：`round-1-ui-verify.out.txt`、`round-2-ui-verify.out.txt`。

## 3. 页面手工验收（真浏览器，vite dev + CDP）

序列（截图 `ui-list.png` / `ui-detail.png`）：

1. 登录 `admin / So18@Pass123` → 侧栏「收款单」（V0294 补的 path/component/is_visible 生效）→ 列表
   列头 `收款单号 | 客户 | 收款日期 | 收款金额 | 已核销 | 未核销 | 收款方式 | 状态 | 操作`，底部 `共 3 条` 分页。
2. 「+ 收款登记」→ 选客户（北京云枢）、金额 1000、方式 银行转账 → 提交：
   `RC-20261106-000005 已登记（草稿），下一步核销到应收`；行显示 `1,000.00 | 0.00 0% | 1,000.00 | 草稿`，
   操作 `详情 / 核销 / 确认 / 作废`。
3. 点「核销」→ 面板列出**该客户所有未收应收**（`AR-SO18-001 600`、`AR-SO18-R3-001 600`、
   `AR-SO18-002 400`、`AR-SO18-R3-002 400`），底部 `收款金额 1,000.00 / 本次核销 0.00（0 张）/ 剩余 1,000.00`。
4. 勾选 `AR-SO18-001` → 自动填 600（min(可核销余额, 剩余)）；底部实时变
   `本次核销 400.00（1 张）/ 剩余 600.00`（此前已有 400）→ 再勾一张后 `1,000.00（2 张）/ 剩余 0.00`。
5. 取消勾选已核销的行 → 被拦并提示
   `「AR-SO18-R3-002」已核销 400.00：本版本没有反核销接口，不能取消勾选（要冲回请走红冲）`；
   手改小金额同样被拦（`本次不能减少到 250.00`）。
6. 「保存核销」→ `RC-20261106-000005 已核销 1,000.00 / 1,000.00`，列表行变 `1,000.00 | 100% | 0.00`。
7. 「确认」→ `已确认，下一步核销到应收`，操作列变 `详情 / 核销 / 过账`。
8. 「过账」→ 过账确认弹窗（提示借 1002 / 贷 1122、过账后锁定）→「确认过账」→
   `已过账（凭证 PZ-20261106-000004）`，操作列只剩 `详情`。
9. 「详情」抽屉：`已过账 | 客户 | 收款日期 | 收款金额 1,000.00 | 已核销/未核销 1,000.00 / 0.00 |
   关联凭证 PZ-20261106-000004 | 核销明细 共 2 张应收 · 合计 1,000.00（两张均「已收清」）`。

库侧复核（`psql`）：

```
receipt_no          | status | amount    | allocated | voucher
RC-20261106-000005  | POSTED | 1000.0000 | 1000.0000 | t
ar_no          | amount   | received | unsettled | status
AR-SO18-001    | 600.0000 | 600.0000 |  0.0000   | SETTLED
AR-SO18-R3-002 | 400.0000 | 400.0000 |  0.0000   | SETTLED
voucher PZ-20261106-000004：1002 D 1000.0000 / 1122 C 1000.0000
```

## 4. 手工验收捞到的真实缺陷（已在本次修掉）

**「保存核销」会重提未改动的行 → 后端 409。** 第一轮 curl 里 `over-allocate` 之后重提已核满的应收
`AR-SO18-R1-001` 得到
`409 应收单 AR-SO18-R1-001 当前是「已收清」，不能核销`：后端 `allocate` 对 `items` 里**每一条**都先锁应收
再校验 `has_balance(status)`，而面板原先把「已核销过的行」按原金额一起提交。修法：`receipt-logic.ts` 新增
`changedAllocationRows` / `hasAllocationChanges`，`buildAllocateItems` 只发**金额有变化**的行
（后端合计校验用 `untouched + total`，跳过未变行不影响「合计 ≤ 收款金额」）；面板无改动时提示
「核销金额没有变化，无需保存」而不是发一个 `items: []`（后端会 400）。单测
`buildAllocateItems 跳过「金额没变」的行` 钉住这条。

## 5. 菜单迁移 V0294

```
psql -f V0294__sales_receipt_menu_page.up.sql     # 改 path 1 行；4 个按钮父级完好
psql -f V0294__sales_receipt_menu_page.up.sql     # 幂等：改 path 0 行
psql -f V0294__sales_receipt_menu_page.down.sql   # 回滚：path/component=NULL, is_visible=FALSE
psql -f V0294__sales_receipt_menu_page.up.sql     # 再回终态
→ sys_menu …2890: 收款单 / /business/erp/receipt / business/erp/receipt/index / is_visible=t / 4 buttons
```

## 6. 环境

`boot-so18.sh` 为隔离实例启动脚本（三个安全密钥由 `openssl rand` 现生成，**不落盘**）。
验完已停：`pkill -f 'target/debug/cloudpivot'`（按精确路径，勿用 `pkill -f cloudpivot`，会匹配到自身 shell）、
vite dev 进程；库 `cloudpivot_so18` 保留（内含本轮验证数据，便于复看）。
