# P1-FIX-2 证据：采购订单详情显示审批进度（照抄收货单 FlowProgress）

- 卡：`t_a2634b7f`（P1-FIX-2）
- 老板要求（2026-11-02 11:38）：「采购订单的详情页应该能看到这个采购单的审批情况」
- 时间：2026-11-02 11:40–12:05 CST
- 提交：`d325e41`（已推 `origin/main`，`git ls-remote` 回读一致）。**本卡未发布 dist 到 144**（卡面未要求发布，发布走发布卡）
- 结论：**已落地，真机 17/17 断言通过、回归 7/7、单测 8 条、全量 456 条通过、build 通过、typecheck 通过。**

---

## 一、改了什么（4 个源文件 + 3 份验证脚本）

| 文件 | 改动 |
|---|---|
| `frontend/src/views/business/erp/purchase/index.vue` | 详情抽屉新增「审批进度」区块（`data-testid="purchase-detail-approval-progress"`）；拉不到节点时降级成单号链接（`purchase-detail-approval-fallback`）；`loadDetail` 里拉节点（第 1333 / 1394 行附近） |
| `frontend/src/views/business/erp/shared/approval-progress.ts` | **新增**：`mapApprovalNodes` / `mapApprovalTasks` / `loadApprovalProgress`（永不抛错）/ `approvalFallbackOf` / `hasApprovalProgress`（纯函数，可单测） |
| `frontend/src/views/business/flow/components/FlowProgress.vue` | 扩展**可选** `tasks` 属性 → 进度条下方列出「审批人 / 状态 / 时间 / 意见」；`nodes` 显示顺序按 `START→…→END` 纠正（后端按 `node_key` 排会把「开始」排到最后） |
| `frontend/src/views/business/flow/components/flow-progress-tasks.ts` | **新增**：分组 / 状态配色 / 状态文案 / 显示顺序（纯函数，被组件与单测共用同一份口径） |
| `frontend/tests/unit/purchase-approval-progress.test.ts` | **新增 8 条单测** |
| `scripts/verify/fix2_purchase_approval_page_check.mjs` | 真机 DOM 验收（17 条断言） |
| `scripts/verify/fix2_flowprogress_regression_check.mjs` | 共享组件回归（7 条断言：实例详情页 + 应付单页 + 收货单页） |

### 关键设计决定（照抄 + 一处必要扩展）

1. **照抄收货单**：详情接口只给 `approval_instance_id`，前端调 `GET /business/flow/instance/{id}`
   拿 `nodes` 交给 `FlowProgress`；**后端零改动**（`handler.rs::purchase_order_detail` 已返回
   `approval_instance_id` + `approval_instance_no`，`fill_approval_no` 回填单号）。
2. **为什么给 `FlowProgress` 加可选 `tasks`**：硬红线要求「复用 FlowProgress、不许另画时间线」，
   而验收又要求「审批人 + 时间 + 意见」——`nodes[]` 里没有审批人（审批人在 `timeline[]`）。
   所以在**同一个组件**里加一个可选块（不传 = 老页面渲染完全不变），而不是在页面里再画一条时间线。
3. **显示顺序纠正**：真接口 `FLW-20261101-02524DBF` 回的节点序是
   `c_low → n_dept → s → e`（「开始」排第三、「结束」排第四）。后端排序是 `round, created_at, node_key`，
   同一秒建行就按 key 排。组件内按 `round 升序 + START 第一 + END 最后 + 其余保序` 纠正，
   对实例详情页（已用 topoOrder）无害。

---

## 二、真机验收（隔离实例，不碰 prod）

- 隔离实例：`cloudpivot_fix2` + 后端 `127.0.0.1:18097` + 前端 vite `127.0.0.1:5197`（`E2E_ISO_SRC_MODE=copy`，数据是 prod 的一份**拷贝**，造数只落副本库）
- 审批中单据：`PO-20261102-000001`（本卡在isolated库造：DRAFT → `submit-approval` →
  `SUBMITTED` / `FLW-20261102-86BB396D`，节点 `部门主管审批` = `ACTIVE`，待审 Henry.Liao / 老六测试号）
- 已通过单据：`PO-20261031-000002`（prod 拷贝自带，`FLW-20261101-02524DBF`）

命令与完整输出：`page-check.txt`（17/17）、`regression-check.txt`（7/7）

| # | 断言 | 结果 |
|---|---|---|
| ① | 详情抽屉出现「审批进度」区块 + 进度条 | PASS |
| ② | 节点顺序 = 图顺序：开始 → 金额分级①（< 1 万） → 部门主管审批 | PASS |
| ③ | 节点状态文案：已通过 / 已通过 / 处理中；进行中节点唯一且高亮 | PASS |
| ④ | 审批人明细：Henry.Liao 待处理、老六测试号 待处理（2 人待处理） | PASS |
| ⑤ | 已通过单据：审批人姓名 + 时间（YYYY-MM-DD HH:mm）+ 意见（内控留痕）都显示 | PASS |
| ⑥ | 拉不到节点（路由拦截 500）→ 降级「审批进度暂时拉不到，审批中 FLW-…」（点开看审批单详情），详情主体照常、不画空条、无异常 | PASS |
| ⑦ | 全程无 console error / 未捕获异常 | PASS |

截图（`evidence/fix2-purchase-approval/`）：
- `submitted-detail.png` — 审批中单据详情（三个节点 + 两人待处理）
- `approved-detail.png` — 已通过单据详情（含「已跳过」留痕 + 意见）
- `fallback-detail.png` — 拉不到节点时的降级形态

回归（共享组件 `FlowProgress` 的老使用方）：
- 审批中心实例详情页进度条仍在，且首节点是「开始」、不显示审批人明细（未传 tasks）
- 应付单页（P17）、收货单页（P16）打开无 console error，列表正常

---

## 三、单测 8 条（`tests/unit/purchase-approval-progress.test.ts`）

| # | 用例 | 钉住的坏味道 |
|---|---|---|
| ① | 有 `approval_instance_id` → 拉一次实例 → 3 节点、顺序与中文名 | 该拉不拉（老板要的「审批情况」还是看不到） |
| ② | 无实例 id → 一次不拉、进度与降级链接都不渲染 | 草稿单画空区块 |
| ③ | 拉节点抛错 → 不抛异常、退化成单号链接（审批中/审批单 + 单号/id 兜底） | 审批中心抽风把详情页整块打挂 |
| ④ | 节点状态映射 APPROVED→已通过 / ACTIVE→处理中 / PENDING→未到达，未知原样；任务状态另一套 | 配色/文案漂移 |
| ⑤ | 三人挂在「部门主管审批」下：姓名 / 状态 / 时间 / 意见 / 自审标记 | 追溯不了「谁几点说了什么」 |
| ⑤-补 | 节点表里没有的旧轮次节点也要显示审批人 | 退回后看不见上一轮谁驳回的 |
| ⑥ | 提交审批后重开详情 → 新节点；再开草稿 → 清空 | ref 残留上一张单的节点（缓存假象） |
| ⑦ | 后端 node_key 序（开始排第三）→ 显示时 START 第一、END 最后；不改入参 | 进度条把「开始」画到最后 |

---

## 四、其他单据缺不缺审批进度（只报告，未改）

`grep -rl approval_instance_id src/views/business --include=*.vue` 的 5 个文件逐一核对：

| 单据 | 现状 | 判定 |
|---|---|---|
| 收货单 `stockin/index.vue` | P16 作废审批进度（`FlowProgress`，节点行） | 有，但**未传 `tasks`** → 看不到审批人/时间/意见（本卡新增的能力它没吃到） |
| 应付单 `payable/index.vue` | P17 付款审批 + 3B 作废审批（`FlowProgress`，节点行） | 同上：有进度条、无审批人明细 |
| 调拨单 `transfer/index.vue` | 自绘 `<ul class="timeline">`（谁/什么/时间/意见/系统代点/自审） | 有且信息更全，但**不是 `FlowProgress`** → 与全站样式不一致 |
| 凭证 `voucher/index.vue` | 只有「作废/红冲审批中」徽标 + 撤回按钮，无 `FlowProgress` 也无时间线 | **缺**（建议另派卡） |
| 采购订单 `purchase/index.vue` | 本卡交付 | 有（进度条 + 审批人明细 + 降级） |
| 任务（`views/business/task`） | `flow-bar` 只有「已完成 ✓ 审批中（王五）」一句状态，无节点时间线 | 轻量状态，够用；若要时间线另议 |

建议后续（不在本卡范围）：
1. 收货单 / 应付单详情把 `timeline` 也传进 `FlowProgress`（一行参数即可），全站信息密度对齐；
2. 调拨单的 `timeline` 换成 `FlowProgress` + `tasks`，样式统一（现在是两套视觉）；
3. 凭证详情补审批进度（唯一真缺的）。

---

## 五、复跑命令

```bash
# 单测（新增 8 条 + 全量 456 条）
cd frontend && pnpm test:unit && pnpm typecheck && pnpm build

# 隔离实例（副本库，绝不碰 prod）
E2E_ISO_DB=cloudpivot_fix2 E2E_ISO_BACKEND_PORT=18097 E2E_ISO_FRONTEND_PORT=5197 \
E2E_ISO_RUN_DIR=/tmp/e2e-fix2 bash scripts/verify/e2e_isolated_instance.sh up

# 造一张「审批中」采购单（只写副本库）
python3 /tmp/e2e-fix2/seed_submitted_po.py

# 真机验收 + 回归
FIX2_BASE_URL=http://127.0.0.1:5197 FIX2_TOKEN_FILE=/tmp/e2e-fix2/session.txt \
FIX2_ORDER_NO=PO-20261102-000001 node scripts/verify/fix2_purchase_approval_page_check.mjs
FIX2_BASE_URL=http://127.0.0.1:5197 FIX2_TOKEN_FILE=/tmp/e2e-fix2/session.txt \
node scripts/verify/fix2_flowprogress_regression_check.mjs
```

踩坑留痕（给下一位）：
- **登录限流 10005**：`LOGIN_RATE_MAX_ATTEMPTS=10 / 60s`，计数器在**进程内存**里（test profile 不走 Redis），
  撞上后等窗口也不一定清 —— 重启隔离后端即可。验收脚本一律用 `FIX2_TOKEN_FILE` 复用会话，别每次现登。
- **prod 拷贝里的 `admin` 是锁定的**（`is_locked=t`，失败 5 次）。副本库里直接换 Argon2 口令 + 解锁即可：
  `uv run --with argon2-cffi python mk_hash.py` 出 PHC 串 → `update sys_user set password=…, is_locked=false, login_fail_count=0`。
- 抽屉是滑入动画：截图/量宽要在 `waitForTimeout(800+)` 之后，否则会把进度条右端「截」在视口外（假裁切）。
