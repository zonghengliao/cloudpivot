# 【P1-1-FIX】前端 CONTRACT_EDGES 落后后端状态机 2 条边 —— 交付证据

- 卡号：`t_87588b45`（DEPLOY-C 部署卡 `t_2fb7b576` 跑全量回归时抓到，守卫用例 ⑦ 红：552/1）
- 提交：`a6e7a26` `fix(erp/contract): 前端边表跟上 P1-1 后端状态机（7 条边，补「提交审批/驳回撤回」）`
- 生产：144 `/opt/cloudpivot` 已 ff 到 `a6e7a26`，前端入口 `assets/index-BDXrOjnD.js`
- 结论：**`pnpm test:unit` 553/553 通过（含 ⑦）、`pnpm typecheck` 0 错、真机 23/23 断言全 PASS**

---

## 1. 根因与修法（逐条照后端源码，不手抄卡面）

后端 `a360243`（P1-1 合同审批流）把合同合法边从 5 条改成 7 条，前端没跟。本次逐条对齐
`backend/src/business/shared/state_machine.rs::CONTRACT_EDGES`：

| 后端口径 | 前端改前 | 前端改后 |
|---|---|---|
| `CONTRACT_EDGES` 7 条，无 `DRAFT→SIGNED` | 5 条，含 `DRAFT→SIGNED` | 7 条，顺序一致 |
| `STATUSES` 6 个（含 `PENDING_APPROVAL`） | `ContractStatus` 5 个 | 6 个 |
| `CONTRACT_REASON_EDGES` 2 条终止边 | 2 条终止边 | **不变**（见下） |
| `CONTRACT_BASE` 的 `default_label` | 按目标态取文案 | 边优先、目标态兜底 |

`CONTRACT_REASON_EDGES` 保持 2 条终止边是**对的**，不是漏改：

- `state_machine::transition` 只对 `SIGNED→TERMINATED` / `ACTIVE→TERMINATED` 校验 reason；
- 审批驳回时的「意见」是**流程待办**的必填项（`POST /business/flow/task/{id}/reject`，
  `normalize_reject_comment` ≥ 5 字），与合同状态流转的 reason 是两个东西；
- 所以 `needsReason('PENDING_APPROVAL','DRAFT') === false` 正确 —— 把「撤回」也弹必填框会挡住合法操作。
  （真机已验证：空意见驳回 → `400 65005「驳回必须填写意见」`，2 字 → `400 65005「至少 5 个字」`。）

### 改了哪些文件

| 文件 | 改动 |
|---|---|
| `frontend/src/views/business/erp/contract/contract-logic.ts` | `CONTRACT_STATUSES` 6 态；`CONTRACT_EDGES` 7 条；`TRANSITION_LABELS/THEMES` 补 `PENDING_APPROVAL`；新增 `EDGE_LABELS`（边→文案，抄 `CONTRACT_BASE`）；确认弹窗按边说明副作用 |
| `frontend/src/api/business/erp/index.ts` | `ContractStatus` 联合类型加 `PENDING_APPROVAL` |
| `frontend/src/constants/status-map.ts` | `erp-contract` 徽标加 `PENDING_APPROVAL = 审批中 / primary / 品牌蓝`（与采购单 `SUBMITTED`、流程待办 `PENDING` 同语义） |
| `frontend/src/views/business/erp/contract/index.vue` | 两处文案/注释跟上新状态机（**只提交这 2 行**，该文件同时有 P1-4 同事在途的附件面板改动，未混入） |
| `frontend/tests/unit/erp-contract.test.ts` | ⑥ 收到 6 态；⑦ 收紧：逐条比对 STATUSES + EDGES + reason 边，并加「DRAFT→SIGNED 不许再出现」「审批中不是终态」「驳回不需要 reason」三条断言（**没有放宽/删除用例**） |

### 影响面（这就是卡面担心的那条路径）

- **列表行快捷动作**走的是前端边表回落（列表行没有 `next_statuses`）：改后草稿行给「提交审批」，
  不再给「登记签订」（旧行为点下去是 409）。
- **详情抽屉**本来就用后端 `next_statuses`，改前也能点；改后文案从「登记签订」变「审批通过签订 / 驳回撤回」。

---

## 2. 单测与类型检查

```
$ cd frontend && pnpm test:unit
# tests 553
# pass 553
# fail 0
```

```
$ cd frontend && pnpm typecheck      # vue-tsc --noEmit
（无输出 = 0 错）
```

改前基线复现：`npx tsx --test tests/unit/erp-contract.test.ts` → `# pass 10 / # fail 1`（⑦ 红）。

---

## 3. 真机回归（生产 144，真实 UI + 真实库）

脚本：`scripts/verify/contract_edges_fix_e2e.mjs`（`createRequire` 解析 `frontend/node_modules/@playwright/test`）

```
cd frontend
DC_BASE=http://172.18.10.144 DC_PASS='<admin 口令>' \
  node ../scripts/verify/contract_edges_fix_e2e.mjs
```

结果：**23/23 PASS，0 FAIL，8 张截图**（明细见 `result.json`）

| # | 断言 | 实测 |
|---|---|---|
| ⓪-1 | 建出草稿合同 | `HT-20261103-000010 status=DRAFT` |
| ①-1 | **列表行**给出「提交审批」 | 按钮=1 文案=提交审批 |
| ①-2 | **列表行不再给「签订」** | `DRAFT→SIGNED` 按钮数=0 |
| ②-1/②-2 | 详情出现「提交审批」/ 无「登记签订」 | 文案=提交审批；detail→SIGNED=0 |
| ③-1 | 确认弹窗文案「草稿 → 审批中」 | 命中 |
| ③-2 | 提交审批不弹原因框 | 原因框=0 |
| ③-3 | 提交后后端状态 | `PENDING_APPROVAL` |
| ③-4 | 徽标 | 审批中 |
| ③-5/③-6 | 详情出现「审批通过签订」「驳回/撤回」 | 两条都在 |
| ③-7 | 列表行（审批中）同样给这两条 | SIGNED=1 DRAFT=1 |
| ④-1/④-2 | 挂上审批实例 + PENDING 待办 | 节点=采购经理审批（< 10 万）审批人=Henry.Liao（ROLE） |
| ④-3 | 驳回空意见 | `400 65005 驳回必须填写意见` |
| ④-4 | 驳回 2 字意见 | `400 65005 驳回意见至少 5 个字（当前 2 个）` |
| ④-5 | admin 不是审批人 | `403 65028 这条待办不是你的，不能审批` |
| ④-6 | 「驳回/撤回」确认弹窗说明副作用 | 命中（终止审批实例 / 回草稿） |
| ④-7 | 驳回/撤回后状态 | `DRAFT` |
| ④-8 | 列表回到草稿且又能提交审批 | 徽标=草稿，提交审批按钮=1 |
| ⑤-1 | 测试草稿已删除 | `200` |
| ⑥-1/⑥-2 | 无意外 4xx/5xx；无 console error | none / none |

截图（`shots/`）：

1. `01-list-draft-quick-action.png` —— 草稿行快捷动作只有「提交审批」（没有「签订」）
2. `02-detail-draft-submit-approval.png` —— 详情抽屉「状态流转：提交审批」
3. `03-confirm-dialog-submit-approval.png` —— 确认弹窗（草稿 → 审批中，无原因框）
4. `04-detail-pending-approval-actions.png` —— 审批中：徽标「审批中」+「审批通过签订 / 驳回撤回」
5. `05-list-pending-approval.png` —— 列表行（审批中）同样两条动作
6. `06-detail-reject-withdraw-button.png` —— 详情「驳回/撤回」
7. `07-confirm-dialog-reject-withdraw.png` —— 驳回/撤回确认弹窗（说明会终止审批实例）
8. `08-list-rejected-back-to-draft.png` —— 回草稿，又能「提交审批」

> 如实说明：截图由 Playwright 落盘，本卡的断言是**读 DOM `innerText`** 得到的（不是看图）；
> 视觉复核因图像识别配额用尽没做成，文本证据见 `result.json` 的 `results`。

### 生产数据变更（诚实清单）

- 新增并**软删除** 1 张合同：`HT-20261103-000010`（回归用草稿，已 `DELETE`）。
- 连带 5 条审批实例留审计行且全部 `CANCELLED`（`FLW-20261103-7E2D2E17` 等；探针 + 回归各一张）。
  收尾核对：`name~P1-1-FIX / 探针 / 回归草稿` 三组查询 total=0，admin 待办=0，合同总数=1（改动前的种子单）。
- 没有审批掉任何真实合同（只做提交 → 驳回/撤回）。

---

## 4. 部署记录

```
$ bash scripts/deploy/deploy.sh          # 生产 144，22:14:05
[1/7] e35a7c0 → a6e7a26，后端改动：无　前端改动：有　新增迁移：0 个（已应用最大 V0211）
[2/7] 备份 /root/deploy-backup-20261103-221405（db.dump / cloudpivot / dist）
[3/7] git merge --ff-only a6e7a26
[4/7] 本次无新迁移
[5/7] 无后端改动，跳过编译与重启
[6/7] typecheck + vite build → dist-20261103-221405 → 双 rename
      ✅ 新入口 assets/index-BDXrOjnD.js = 线上入口（指纹一致）
[7/7] 主仓 HEAD == origin/main == a6e7a26；服务 active；/health ok；进程==磁盘 yes
```

回滚：`mv dist dist-failed-… && mv dist-prev-20261103-221405 dist`（二进制/库本次未动）。

---

## 5. 遗留 / 需要知道的两件事

1. **真实「审批驳回」只能由审批人执行**：V0209 把 `contract_approve` 的两级审批人派给 ROLE
   （`PURCHASE_MANAGER` → 当前展开为 `Henry.Liao`；≥10 万加 `GENERAL_MANAGER`），admin 不是审批人
   （实测 `403 65028`）。所以本卡用「真接口 + 真实待办 id」证明了意见必填（400 65005），
   再用前端「驳回/撤回」边把单据收回草稿 —— 没有用 admin 硬闯别人的待办。
   要端到端跑「采购经理点驳回」需要该账号口令（或另一个审批人账号），本卡未持有。
2. **部署机磁盘水位**：144 `/` 用 94%（剩 16G）、`/tmp` tmpfs **100% 满（7.6G）**；
   本次构建（dist ≈ 14M）没受影响，但 `/tmp` 满会让后续需要 `/tmp` 的工具（部分 rust/编辑器）失败，
   与前端无关、建议另行清理。

---

## 6. 复跑方式

```bash
# 单测 + 类型检查（本地/CI）
cd frontend && pnpm test:unit && pnpm typecheck

# 真机回归（只建 1 张草稿，收尾自动删除；不会审批任何真实合同）
cd frontend
DC_BASE=http://172.18.10.144 \
DC_PASS=$(ssh root@172.18.10.144 'grep ^NIK_ADMIN_PASSWORD= /etc/cloudpivot/cloudpivot.env | cut -d= -f2-') \
  node ../scripts/verify/contract_edges_fix_e2e.mjs
```
