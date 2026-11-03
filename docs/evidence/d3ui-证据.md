# D3-UI：流程定义页「边配置」入口 —— 交付与真机验收记录

- 卡：`t_a392a5da`（父卡 `t_d765a83c` DEPLOY-B 真机验收第 ⑧ 项 FAIL）
- 日期：2026-11-03（CST）
- 生产：`172.18.10.144:/opt/cloudpivot`，部署后 commit `a1bcaad`，dist 入口
  `assets/index-COHYtSHL.js` → **`assets/index-CURoWZzb.js`**
- 结论：**PASS 17 / FAIL 0**（`scripts/verify/d3ui_state_edge_e2e.mjs`），prod 未留残留
  （验收用的审批单已撤回，`biz_flow_state_edge` 配置仍 0 行）

## 0. 一句话

后端管理面（`backend/src/business/flow/edge_admin.rs`，V0206 + D3 `2e1f90d`）早已可用，
缺的是**前端入口**：`frontend/src` 里原本 0 个文件提到 `state-edge`。本提交补了入口 +
边面板 + 变更弹窗，**没有在前端维护第二份状态机**：边、文案、实体中文名全部来自
`GET /business/flow/state-edge`，改配置只走 `PUT`（后端起 `flow_edge_config` 审批单）。

## 1. 卡面有一处口径要纠正（实测）

| 卡面写的 | 实测 | 依据 |
|---|---|---|
| `POST /business/flow/state-edge/change` | **404** `{"code":404,"msg":"请求的资源不存在"}` | `probe_state_edge.mjs` 打 144 生产 |
| —— | **`PUT /business/flow/state-edge`** 才是改配置 | `edge_admin.rs` 的 `#[nik::put("/business/flow/state-edge")]`；`PUT` 带 `confirm=false` 返回 400/10001「改状态机配置需要二次确认（confirm = true）」，证明路由存在 |

删除配置是 `DELETE /business/flow/state-edge?entity=&from_status=&to_status=&confirm=true&reason=`，
同样只生成审批单。前端 API 层已按实测口径写（`frontend/src/api/business/flow/state-edge.ts` 文件头注明了这一点）。

## 2. 改动清单（commit `a1bcaad`，6 个文件 / +1252 −1）

| 文件 | 作用 |
|---|---|
| `frontend/src/api/business/flow/state-edge.ts`（新，230 行） | 契约层：字段逐字抄 `edge_admin.rs` 的 `StateEdgeListRes / EntityEdgesRes / EdgeRes / ConfigRowRes / PendingChangeRes / EdgeChangeRes / EdgeUpsertReq / EdgeDeleteQuery`；三个函数 `listStateEdges / changeStateEdge / deleteStateEdge` |
| `frontend/src/views/business/flow/def/StateEdgePanel.vue`（新，494 行） | 抽屉面板：实体下拉（文案用后端 `entity_label`）→ 边表（从/到/文案/约束/原因/追加权限码/当前配置/生效/操作）+ 在跑审批区 + 变更/删除弹窗 |
| `frontend/src/views/business/flow/def/state-edge-logic.ts`（新，225 行） | 纯逻辑：三份数据叠加（`mergeBaseEdges`）、`confirm` 恒 true、mandatory 禁不掉、requires_reason 只能加不能减、理由校验、结果文案（「已送审」≠「已生效」） |
| `frontend/src/views/business/flow/def/index.vue`（+36 −1） | 工具条加「边配置」按钮（`v-privilege="[PERM.flowStateEdgeList]"`）；支持 `/flow/def?entity=STOCK_OUT` 深链直接定位（**路由路径没动**） |
| `frontend/src/constants/permissions.ts`（+4） | 补 `business:flow:state-edge:list` / `business:flow:state-edge:manage`（与 `edge_admin.rs::PERM_LIST/PERM_MANAGE`、V0206 逐字一致） |
| `frontend/tests/unit/flow-state-edge.test.ts`（新，264 行） | 11 条单测，全部 11 PASS |

配套验证脚本：`scripts/verify/d3ui_state_edge_e2e.mjs`（真机 17 项断言）、
`scripts/verify/d3ui_evidence.mjs`（接口样例 + DOM 证据 + 撤回清场）。

## 3. 接口返回样例（144 生产，`GET /business/flow/state-edge?entity=STOCK_OUT`）

原始响应全文：`docs/evidence/d3ui-get-state-edge.json`。摘要：

```
status=200 code=0 total=1
entity=STOCK_OUT  entity_label=出库单  base_edges=6  configs=0  effective_edges=6  pending_changes=0

DRAFT        → PENDING_POST   文案=提交待过账   mandatory=true   requires_reason=false  auth_code=null  configured=false enabled=true
PENDING_POST → POSTED         文案=过账         mandatory=true   requires_reason=false  auth_code=null  configured=false enabled=true
PENDING_POST → DRAFT          文案=撤回         mandatory=true   requires_reason=false  auth_code=null  configured=false enabled=true
DRAFT        → POSTED         文案=直接过账     mandatory=false  requires_reason=false  auth_code=null  configured=false enabled=true
DRAFT        → CANCELLED      文案=草稿作废     mandatory=true   requires_reason=false  auth_code=null  configured=false enabled=true
POSTED       → CANCELLED      文案=已过账作废   mandatory=true   requires_reason=true   auth_code=null  configured=false enabled=true
```

不带 `entity` 时返回 9 个实体（面板一次请求全拿，切实体不再发请求）：

```
STOCK_IN 收货入库单 10 条 / PURCHASE_ORDER 采购订单 13 / TRANSFER 库存调拨单 6 /
PAYABLE 应付单 7 / VOUCHER 凭证 0 / ACCOUNTING_PERIOD 会计期间 3 /
ACCOUNTING_YEAR 会计年度 2 / CONTRACT 合同 5 / STOCK_OUT 出库单 6
```

写侧样例（`PUT`，全响应见 `docs/evidence/d3ui-put-state-edge.json`）：

```json
{
  "code": 0,
  "data": {
    "applied": false,
    "instance_id": "01a19f90-8af4-77c1-9a8a-0ba80989c514",
    "instance_no": "FLW-20261103-4C65B63B",
    "instance_status": "PENDING",
    "message": "STOCK_OUT DRAFT→POSTED 禁用 / 要求填原因：已提交审批 FLW-20261103-4C65B63B，审批通过后才会生效（驳回或撤回则配置不变）"
  }
}
```

**`applied = false` 是成功送审**：随后再 GET，`configs` 仍是 0 行、`pending_changes` 是 1 张；
撤回该审批单后再 GET，`configs=0 / pending_changes=0` —— 配置一分未动。

## 4. 真机验收（Playwright 打 144 nginx → 线上 dist → 生产库）

命令：

```bash
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
cd /opt/cloudpivot && node scripts/verify/d3ui_state_edge_e2e.mjs
```

结果 **PASS 17 / FAIL 0**：

```
[PASS] GET /business/flow/state-edge?entity=STOCK_OUT → code=0，BASE 边 6 条
[PASS] `/flow/def` 顶栏出现「边配置」按钮（data-testid=btn-state-edge，全页「边配置」计数 = 1）
[PASS] 实体下拉切到 STOCK_OUT（选项文案 = 「出库单（STOCK_OUT）」）
[PASS] 边表渲染 6 行 == 接口 base_edges 6 条
[PASS] 接口下发的 6 条边（from/to）在面板上全部可见
[PASS] mandatory（硬约束）/ requires_reason（必须填原因）标记渲染正常
[PASS] 硬约束边（mandatory）的「是否启用」开关被锁死（class 含 t-is-disabled）
[PASS] 理由为空时前端拦截（提示「请填写变更理由…」），没有擅自提审批
[PASS] 提交后提示「已提交审批 + 审批单号」（不是直接生效）
[PASS] 提交后 biz_flow_state_edge 配置仍 0 行（未直接落库）
[PASS] 在跑审批单 1 张：FLW-20261103-32DBADFD（action=UPSERT DRAFT→POSTED）
[PASS] 面板上「在跑审批」区域出现（审批通过前不会生效）
[PASS] 验收用审批单已撤回（prod 不留待办）
[PASS] 撤回后配置 0 行、在跑审批 0 张（驳回 / 撤回配置一分不动）
[PASS] 页面无 4xx/5xx 接口噪音 · [PASS] 控制台无 error
```

### 面板 DOM 证据（`docs/evidence/d3ui-panel-dom.txt`）

深链 `/flow/def?entity=STOCK_OUT` 打开后的单元格矩阵（6 行 × 9 列，与接口逐条对应）：

```
1. DRAFT | PENDING_POST | 提交待过账 | 硬约束 | 不要求原因 | — | 默认 | 启用 | 变更
2. PENDING_POST | POSTED | 过账 | 硬约束 | 不要求原因 | — | 默认 | 启用 | 变更
3. PENDING_POST | DRAFT | 撤回 | 硬约束 | 不要求原因 | — | 默认 | 启用 | 变更
4. DRAFT | POSTED | 直接过账 | 可选 | 不要求原因 | — | 默认 | 启用 | 变更
5. DRAFT | CANCELLED | 草稿作废 | 硬约束 | 不要求原因 | — | 默认 | 启用 | 变更
6. POSTED | CANCELLED | 已过账作废 | 硬约束 | 必须填原因 | — | 默认 | 启用 | 变更
```

（`当前配置` 列此时全是「默认」= 库里无配置行，`生效` 全「启用」= BASE 行为；
行内有配置后会显示「已配置」并多出「删除配置」链接。）

## 5. 截图（5 张，`docs/evidence/`）

| 文件 | 内容 |
|---|---|
| `d3ui-01-flow-def-entry.png` | `/flow/def` 列表页顶栏「查询 / 重置 / 新建流程 / **边配置** / 刷新」 |
| `d3ui-02-state-edge-panel-stockout.png` | 抽屉面板 + STOCK_OUT 的 6 条 BASE 边（约束 / 原因 / 配置 / 生效列齐全） |
| `d3ui-03-state-edge-change-dialog.png` | 变更弹窗（启用开关、必须填原因、文案、追加权限码、变更理由、待变更摘要） |
| `d3ui-04-state-edge-approval-submitted.png` | 提交后「已提交审批 FLW-20261103-32DBADFD」提示 + 在跑审批区 |
| `d3ui-05-state-edge-after-withdraw.png` | 撤回后：配置仍 0 行、在跑审批 0 张（配置一分未动） |

## 6. 构建与检查

| 检查 | 结果 |
|---|---|
| `pnpm typecheck`（99 与 144 各一次） | **0 错**（`vue-tsc --noEmit`） |
| `pnpm exec vite build`（144） | **rc=0**，入口 `assets/index-CURoWZzb.js` |
| `pnpm test:unit`（99 工作树） | **543 / 543 PASS**（含新增 11 条 `flow-state-edge.test.ts`） |
| dist 原子替换 | `mv dist dist-prev-d3ui-20261103-115913 && mv dist-d3ui dist`；线上入口 == dist 入口 == `index-CURoWZzb.js`，入口 chunk 200 / 115562B |
| prod 残留 | 无：验收审批单已撤回；`biz_flow_state_edge` 配置 0 行；`pending_changes` 0 |

## 7. 边界（刻意不做的事）

- **不写死边表 / 状态名 / 文案**：连实体中文名都用后端 `entity_label`；前端只有「表格怎么画」，
  没有「哪个状态能到哪个状态」的知识。被配置禁用的边在 `effective_edges` 里消失，
  页面靠 `configs` + `effective_edges` 叠加还原（`mergeBaseEdges`，有单测）。
- **改配置不直写库**：前端只有 `PUT` / `DELETE`，后端 dry-run → 起审批单 → 审批通过回调才落库。
- **路由路径没动**：仍是 `/flow/def`（`sys_menu.path`），深链只加 `?entity=` 查询参数。
- **没有做「双人复核」的策略改动**：`flow_edge_config` 的审批人是 `FLOW_CONFIG_APPROVER` 角色，
  0 人绑定时 `FALLBACK_ADMIN` 落到超管（V0206 既有口径，本卡未动图）。

## 8. 回滚

只回滚前端（后端一行没改）：

```bash
cd /opt/cloudpivot/frontend
mv dist dist-broken-$(date +%Y%m%d-%H%M%S)
mv dist-prev-d3ui-20261103-115913 dist    # 回到 index-COHYtSHL.js
curl -s http://127.0.0.1/ | grep -o 'assets/index-[^"]*\.js'
```
