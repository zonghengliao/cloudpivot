# P1-FIX-11 凭证详情审批进度（红冲场景补充验证）

卡：`t_066e9f2d`【P1-FIX-11】凭证页未接审批进度（已并入 P0-FIX-4B）
结论：**P0-FIX-4B 已覆盖，本卡不改业务代码，交付独立实测验收（红冲专项）。**
时间：2026-11-06 01:0x–01:2x +0800 · 树 HEAD `80e05a1`（`main`，未提交任何前端/后端源码改动）

---

## 1. 结论：已被 P0-FIX-4B（`t_c6ff9249`）覆盖

| 检查点 | 证据 |
| --- | --- |
| 何时引入 | `git log -S voucherApprovalInstanceId` → `d37302d` / `7f243d4`（2026-11-04 22:11/22:26，`t_c6ff9249`）；见 `raw/02-code-proof.txt` |
| 取实例 id 的判据 | `frontend/src/views/business/erp/voucher/voucher-audit-logic.ts:232 voucherApprovalInstanceId(row)`：`pending_reverse_instance_id`（红冲）优先 → `approval_instance_id`（4A 审核链）→ 都没有返回 `null` |
| 页面接线 | `frontend/src/views/business/erp/voucher/index.vue`：`:468 <template v-if="hasApprovalProgress(approvalProgress)">`、`:471/:472 <ApprovalProgressFlow :nodes :tasks />`、`data-testid="voucher-approval-progress"`；`:1039 approvalProgress` 状态、`:1043 approvalProgress.value = await loadApprovalProgress(voucherApprovalInstanceId(row), getFlowInstance)` |
| 装载器复用 | `frontend/src/views/business/erp/shared/approval-progress.ts:118 loadApprovalProgress` / `:166 hasApprovalProgress` —— 与采购 / 收货 / 应付 / 调拨共用一份，不是凭证专用副本 |
| 单测覆盖组合 | `frontend/tests/unit/p0-fix-4b-voucher-audit.test.ts` 第 12 例「审批实例 id：红冲实例优先，其次审核实例，都没有 → null（进度块不渲染）」 |

即：本卡要求的「凭证详情有审批进度」在 4B 已落地，本卡范围内唯一增量是**红冲场景的独立实测**（4B 的截图只覆盖了一张单的提交瞬间，本卡补：列表徽标 → 抽屉进度 → 请求的实例 id → 对照组不发空请求 → 第二张单真链路复跑）。

## 2. 实测（4 轮，全部 15/15 PASS，两两连跑）

真机链路：隔离后端（`cloudpivot_fix11b`，`127.0.0.1:18156`，NIK_PROFILE=test）+ `vite dev`（`127.0.0.1:5186`，源码即产物）+ Playwright 无头浏览器（2048×1150）。造数走真接口 `POST /api/business/erp/voucher/{uuid}/submit-reverse`，不是打桩。

| 轮次 | 目标凭证 | 结果 | 日志 |
| --- | --- | --- | --- |
| r1 / r2（上一进程，01:16） | `PZ-20261103-000540`（20.00 采购入库凭证） | 15 PASS / 0 FAIL | `raw/round1.txt`、`raw/round2.txt` |
| **a1 / a2（本轮）** | `PZ-20261103-000540` | 15 PASS / 0 FAIL ×2 | `raw/round-a1.txt`、`raw/round-a2.txt` |
| **b1 / b2（本轮）** | `0201`（ERPR1 夹具，700.00，**本轮新提交红冲**） | 15 PASS / 0 FAIL ×2 | `raw/round-b1.txt`、`raw/round-b2.txt`、造数回执 `raw/06-live-reverse-a.json` |

每轮的断言（同一脚本 `scripts/verify.mjs`）：

```
PASS  列表行显示「红冲审批中」徽标
PASS  红冲凭证详情出现「审批进度」块                     # data-testid=voucher-approval-progress, count=1, visible
PASS  抽屉里有「审批进度」标题
PASS  抽屉里有「红冲审批中」提示
PASS  进度块含「开始」/「金额分级」/「财务经理审批」/「李四」/「处理中」
PASS  进度块在视口内（截图能拍到）                        # y=610 h=131
PASS  详情请求了本凭证的审批实例                          # GET /api/business/flow/instance/<本单实例>
PASS  详情状态仍是「已记账」（红冲审批不落业务态）
PASS  对照凭证（无审批实例）不渲染进度块
PASS  对照凭证抽屉里没有「审批进度」标题
PASS  对照凭证不发 flow/instance 请求（省一次空请求）
```

渲染文本（真数据，非 mock）：`开始 已通过 → 金额分级（> 1 万会签）已通过 → 财务经理审批 处理中`，下方 `财务经理审批 · 李四 待处理 <创建时间>`。

### 本轮新造的红冲实例（`0201`）
- 接口回执：`code=0, outcome=PENDING_APPROVAL, approval_instance_id=01a1acb8-aed0-70e2-a0e8-9b3229bfc30f`（`raw/06-live-reverse-a.json`）
- 库内：`biz_voucher 0201 | POSTED | pending_reverse_instance_id=01a1acb8-…`；`biz_flow_instance → biz_type=VOUCHER_REVERSE, status=PENDING`；`biz_flow_task n_fm | PENDING | assignee_source=ROLE | 李四 / pc_demo_li`
- 页面：`05-detail-voucher-0201.png`（凭证号 0201、状态已记账、红冲审批中、进度块三节点 + 李四待处理）

## 3. 截图

| 文件 | 内容 |
| --- | --- |
| `01-list.png` | 凭证列表行内的「红冲审批中」徽标 |
| `02-detail-approval-progress.png` | 凭证详情抽屉：红冲提示 + 审批进度（PZ-20261103-000540） |
| `03-progress-block.png` | 进度块放大（本卡核心验收物） |
| `04-control-no-progress.png` | 对照组 `PZ-20261102-001579`（无待办实例）：无进度块、无空请求 |
| `05-detail-voucher-0201.png` | 本轮新造红冲单 0201 的详情进度 |

## 4. 类型与单测（只跑受影响模块）

| 项 | 命令 | 结果 | 日志 |
| --- | --- | --- | --- |
| 前端类型检查 | `cd frontend && ./node_modules/.bin/vue-tsc --noEmit` | exit 0（无输出=无错误） | `raw/07-typecheck-a.txt` |
| 凭证审核逻辑单测 | `npx tsx --test tests/unit/p0-fix-4b-voucher-audit.test.ts` | 12 tests / 12 pass / 0 fail | `raw/08-unit-voucher-audit-a.txt` |
| 早期同项 | 同上 | 12/12 | `raw/03-unit-voucher-audit.txt`、`raw/04-typecheck.txt` |

未跑后端 cargo 全套与其它前端用例：本卡零源码改动，改动面为 0，按卡面「不跑无关的全量回归」。

## 5. 逐条验收

- ☑ **确认 P0-FIX-4B 覆盖情况**：§1 四层证据（提交、判据函数、页面接线、单测），结论=已覆盖。
- ☑ **凭证详情有审批进度（截图）**：`02-detail-approval-progress.png` / `03-progress-block.png`，真库真流程实例。
- ☑ **红冲凭证也能看到审批进度**：两张不同凭证（`PZ-20261103-000540`、本轮新提交的 `0201`）各两轮 15/15 PASS；并含对照组（无实例 → 不渲染、不发请求）。
- ☑ **git commit**：证据与脚本入库（本卡无业务代码改动，故无源码 diff）。

## 6. 环境与复现

```bash
# 隔离后端（库 cloudpivot_fix11b :18156，profile=test）
# 口令/JWT/存储主密钥一律走环境变量（仓库不落口令）：FIX11_ADMIN_PASSWORD=... bash scripts/fix11-db-instance.sh start
bash scripts/fix11-db-instance.sh start        # JWT/存储密钥由脚本内 openssl 随机生成
# 前端
cd /opt/cloudpivot/frontend && BACKEND_ORIGIN=http://127.0.0.1:18156 \
  ./node_modules/.bin/vite --port 5186
# 造数（注意：路径参数是凭证 uuid，不是 voucher_no）
VID=$(PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_fix11b -tAc \
  "select id from biz_voucher where voucher_no='0201'")
curl -X POST "http://127.0.0.1:18156/api/business/erp/voucher/$VID/submit-reverse" \
  -H "Authorization: Bearer $TOK" -H 'Content-Type: application/json' -d '{"reason":"..."}'
# 验收（脚本依赖 /root/fix11-run 的 node_modules 与 token.txt，见 scripts/verify.mjs 头部）
node verify-fix11.mjs     # VNO/VNO2/INST/OUT 可用环境变量覆盖
```

## 7. 偏差与已知边界

1. `submit-reverse` 的路径参数是**凭证 uuid**，卡面/旧注释写成 `0201`（voucher_no）会得到 `66006 id 不是合法的 UUID`；本轮实测并记录（`raw/06-live-reverse-a.json` 之前的一次失败调用）。
2. 进度块**只在** `pending_reverse_instance_id` 或 `approval_instance_id` 存在时渲染；两者都空（已记账、无反审核、无红冲中的普通凭证）不渲染且不发 `flow/instance` 请求——这是设计取舍（省一次空请求），对照轮已断言。
3. 红冲审批**不落业务态**：详情页状态仍是「已记账」，红冲是否在办只由「红冲审批中」徽标 + 进度块表达，避免用户误以为凭证已变红字。
4. 本轮全程隔离库/隔离端口，未触碰生产（144）与 `cloudpivot` 库。
