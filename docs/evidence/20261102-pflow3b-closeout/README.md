# P-FLOW-3B 补漏·前端收尾证据（删 pending 兜底 + 接 PayableRes 新字段 + 发布 dist 到 144）

> 卡：`t_8303617c`　父卡：`t_a7fde8f7`（后端补漏，已部署 144）
> 提交：`77fbaf0`（功能）+ `35706c3`（testid 收口），均已推 `origin/main`
> 生产：`http://172.18.10.144`，dist `sha256 = b8bdac9d…`，入口 chunk `assets/index-C8Pwg9Wa.js`

---

## 一、卡面两件事 → 落地对照

| 卡面要求 | 落地 | 关键文件 |
|---|---|---|
| ① 删掉 `loadPendingReverseFallback` 及其触发的 `GET /business/flow/instance/page?status=PENDING&biz_type=VOUCHER_REVERSE` | 整层删除：`loadPendingReverseFallback` / `injectPendingReverse` / `pendingReverseMap`，连同 `voucher-logic.ts` 的 `pendingReverseInstances()` + `VOUCHER_REVERSE_BIZ_TYPE`（唯一消费者没了）。列表直接 `rows.value = data.list as VoucherRowEx[]`，详情走 `normalizePendingReverse()`（只做 `undefined → null` 归一） | `voucher/index.vue`、`voucher/voucher-logic.ts` |
| ① 字段由「可选」收紧为「恒在」 | `VoucherFlowState.pending_reverse_instance_id` / `reverse_reason` 从 `?: string \| null` 改为 `string \| null`（后端列表 / 详情 / 红冲回执三处都下发） | `voucher/voucher-write-api.ts` |
| ① 抽屉「作废审批进度 + 作废原因」改读 `PayableRes::cancel_instance_id` / `cancel_reason`，不再依赖「送审那一刻」的 `approval_instance_id` | `PayableRow` 补声明两字段；新增纯函数 `approvalTargetOf()`（选作废/付款进度与标题）、`cancelReasonText()`；`openDetail` / 送审 / 撤回三处统一走 `refreshApprovalProgress()`；抽屉新增「作废原因」一行（`data-testid="payable-pending-cancel-reason"`） | `api/business/erp/index.ts`、`payable/payable-logic.ts`、`payable/index.vue` |
| ② 重新 build + 发布 dist 到 144 | 见 §三（sha 回读一致，入口 chunk 变了） | —— |
| 单测口径同步 | 原「⑧ 兜底映射」改写成「⑧ 兜底已删 + 新字段必填 + 后端 DTO 真的下发」（源码扫描）；新增「⑨ 抽屉进度选择 / 原因渲染」2 条 | `tests/unit/pflow3b-cancel-reverse.test.ts` |

删除后的判据：`rows.every(r => r.pending_reverse_instance_id !== undefined)` 这个「兜底开关」本身也一并消失 —— 现在只有一条数据源（后端字段），
不再存在「双源打架」。**只要能看到凭证列表就能看到徽标**，不再受 `business:flow:instance:read` 的数据可见性限制。

> grep 口径：`loadPendingReverseFallback` / `pendingReverseInstances` / `VOUCHER_REVERSE_BIZ_TYPE` 在 `frontend/src` 与
> `frontend/src/views/**` 里 **0 命中**；唯一出现处是 `tests/unit/pflow3b-cancel-reverse.test.ts` 的**反向断言**
> （`assert.ok(!page.includes('loadPendingReverseFallback'))` 等），那是钉住「不许有人再加回来」，不是引用。

## 二、门禁（本地，`cp-src/frontend`，见 `raw/01-gates.txt`）

```text
pnpm typecheck      → 0 error
pnpm test:unit      → 448 passed / 0 failed（基线 445 + 本卡净增 3：⑧×2 改写 + ⑨×2）
pnpm check:palettes → 色阶断言 330 条 0 失败，全部通过 ✓
pnpm build          → 成功；dist/index.html sha256 = b8bdac9d8520affabc3f6aecfe9ceb47a7cf7c33f2d669ddee359a0f98e6b56c
```

产物自检：`pending_reverse_instance_id` 在凭证 chunk、`payable-pending-cancel-reason` 在应付 chunk，
全量 `dist/assets/*.js` 里 **`VOUCHER_REVERSE` 0 命中**（兜底真的没了，不是「留着没调用」）。

## 三、发布到 144（`raw/02-publish.txt`）

| 项 | 发布前（01:42 旧包） | 发布后 |
|---|---|---|
| 入口 chunk | `assets/index-DH4mKvbW.js` | **`assets/index-C8Pwg9Wa.js`** |
| `index.html` sha256 | `add6cbe4e7e8571937852c3f095d2a709668abae8dc41b373829727a6493bd6d` | **`b8bdac9d8520affabc3f6aecfe9ceb47a7cf7c33f2d669ddee359a0f98e6b56c`** |

流程（与历次发布同口径）：`rsync -az --delete dist/ → 144:/opt/cloudpivot/frontend/dist.new/` →
远端 `sha256sum dist.new/index.html` 与本地逐字节比对 → `mv dist /var/tmp/cloudpivot-dist-old-<ts>`（原子换目录）→ `mv dist.new dist` →
从 99 `curl http://172.18.10.144/ | sha256sum` 回读一致；`/health` 200；`/opt/cloudpivot` 源码 ff 到 `35706c3`。
备份：本卡两次发布各留一份 —— `/var/tmp/cloudpivot-dist-old-20261102035651`（`77fbaf0` 那次）与
`/var/tmp/cloudpivot-dist-old-20261102040019`（最终这次），旧包仍在，随时可回滚。

## 四、真机验收

### 4.1 全流程（隔离实例；`raw/03-isolated-e2e.txt`，2 passed 7.9s）

- 前端：**就是上面那个已发布的 dist**（`serve-dist.mjs` 在 5200 上服务它，`index.html` sha 与线上一致），
  用例里显式断言入口 chunk = `assets/index-C8Pwg9Wa.js`（证明验的是线上那个包，不是另编一份）；
- 后端：隔离实例 `127.0.0.1:18096`，库 `cloudpivot_3b_e2e`，代码 = `origin/main`（`35706c3`，与 144 生产二进制同源）；
- 夹具：`AP-20261102-000008`（收货过账自动生成的应付）+ `PZ-20261102-000018`（进项凭证），审批人 `p3bfe_fm` 真在等审（实例真 `PENDING`）。

| 步骤 | 断言（DOM / 接口 / 字段） | 截图 |
|---|---|---|
| 凭证初始 | 状态「已记账」、无「红冲审批中」、有「红冲」 | `01-voucher-posted.png` |
| 点红冲 → 提交 | `POST /voucher/{id}/reverse` 200，`outcome=PENDING_APPROVAL`，**`pending_reverse_instance_id` 非空**、`reverse_reason` = 填写值、`status` 仍是 `POSTED`；toast = `已提交审批，等待财务经理复核（PZ-20261102-000018）：凭证仍是「已记账」，审批通过才生成红字凭证` | —— |
| 送审后列表 | 「红冲审批中」徽标出现，「已记账」**仍在**（并列、不顶替），「撤回红冲」出现、「红冲」消失 | `02-voucher-pending-reverse.png` |
| **整页刷新** | 列表响应行带 `pending_reverse_instance_id` + `reverse_reason`；徽标与撤回入口**仍在**；**本次加载 0 次 `/business/flow/instance/page` 请求** | `03-voucher-pending-after-reload.png` |
| 详情 | `GET /voucher/{id}` 带两个新字段；抽屉显示「红冲审批中」+ 撤回入口 | `04-voucher-drawer-pending.png` |
| 撤回 | `POST …/withdraw-reverse` 200；徽标消失、「红冲」回来；接口回读字段 `null` | `05-voucher-withdrawn.png` |
| 应付初始 | 「未付」、有「作废」、无「撤回作废」 | `06-payable-open.png` |
| 点作废 → 提交 | `POST /payable/{id}/cancel` 200，`outcome=PENDING_APPROVAL`，**`cancel_instance_id` 非空**、`cancel_reason` = 填写值；toast = `…（AP-20261102-000008）：应付单仍是「未付」，审批通过才会真正作废` | —— |
| 送审后列表 | 「作废审批中」，撤回入口出现，付款/作废都消失 | `07-payable-pending-cancel.png` |
| **抽屉首次打开** | 「作废审批进度」标题 + 进度节点（`FlowProgress` 有内容）+ 「作废原因：…」一行（`payable-pending-cancel-reason`） | `08-payable-drawer-progress.png` |
| **关抽屉 → 整页刷新 → 重开抽屉** | 列表行带 `cancel_instance_id` / `cancel_reason`；进度标题 + 节点 + 原因**全部仍在**（= 本卡核心验收点；旧实现这里进度区是空的） | `09-payable-drawer-after-reload.png` |
| 撤回 | `POST …/withdraw-cancel` 200；回「未付」、作废回来、撤回消失；接口回读 `status=OPEN`、`cancel_instance_id=null` | `10-payable-withdrawn.png` |

可反复运行：spec 的 `beforeAll` 先 `heal()`（把上一轮残留的审批中撤回，409 忽略），实测连续两轮 **2 passed**。

### 4.2 生产 144（只读；`raw/04-prod-readonly.txt` + 浏览器直读 DOM）

**为什么生产不做送审/撤回**：144 生产库里凭证 / 应付都是**真实业务单据**；且 `FINANCE_MANAGER` 等审批角色在 prod 未建，
审批节点解析不到人时引擎会 `empty_assignee=FALLBACK_ADMIN` → 发起人自己 → 内控判 `SKIPPED` → 实例直接 `APPROVED`
⇒ 「送审」那一下会真的红冲 / 作废一笔生产账。故生产侧只做只读核对（改动后的写入链路已在隔离实例用**同源码同 dist**跑通）。

| 核对 | 结果 |
|---|---|
| 登录 + 两个列表接口 | `voucher/page` 200（total 11）、`payable/page` 200（total 8） |
| 凭证列表行 ↔ 详情 | `pending_reverse_instance_id` / `reverse_reason` **键存在**（值 `null`），`status=POSTED` |
| 应付列表行 ↔ 详情 | `cancel_instance_id` / `cancel_reason` **键存在**（值 `null`），`status=OPEN` |
| 浏览器直读（新包） | 入口 chunk = `/assets/index-C8Pwg9Wa.js`；凭证页 11 行、应付页 8 行正常渲染；**两页都没有 `/business/flow/instance/page` 请求**；控制台无错 |

## 五、生产不变量 / 残留（`raw/05-prod-invariants.txt`、`raw/06-prod-residue.txt`）

```text
erp_invariants.py        → 32/32 通过   BLOCKER 0   WARN 0
e2e_fixture_residue.py   → ✅ 无残留
```

## 六、踩过的坑（给后来人）

1. **隔离实例的后端二进制可能是旧的**：`127.0.0.1:18096` 上跑的是 02:52 从 `0828a76` 编的 debug 包，
   而新字段是 `f06ea40` 才加的 ⇒ 第一轮真机用例「送审响应里没有 `pending_reverse_instance_id`」直接失败。
   **验新字段前先 `stat -c %y` 二进制 + `git rev-parse` 源码目录**，或直接重建：
   `CARGO_TARGET_DIR=/root/d7-target cargo +1.94.1 build`（热目标目录约 70s），
   再把 `config/config-test.toml`（.gitignore，从既有检出拷）补到 clone 里。
2. **同一个页面里 testid 会撞车**：送审弹窗的原因输入框本来就叫 `payable-cancel-reason`，
   抽屉里新加的「作废原因」一开始也叫这个 ⇒ `getByTestId(...)` strict mode 直接炸。
   抽屉侧改名 `payable-pending-cancel-reason`（第一个提交已发布、发现后补了 `35706c3` 并重新发布 dist）。
3. **一次性静态服务忘了 `import http`**：`import { createServer } from 'node:http'` 之后再写 `http.request(...)`
   会在**第一个 `/api` 请求**时抛 `http is not defined`，症状是首页 200、`/health` 000、随后连接就死。
   要 `import http, { createServer } from 'node:http'`。
4. **`tee` 写 `/tmp` 会被危险命令闸拦**（单查询模式），证据日志一律重定向到工作区目录再 `cat`。

## 七、仍需产品/运维拍板的事项（本卡不替产品决定）

`144 prod` 的 `sys_role` 里只有 `超级管理员` 等少数角色，**没有** `ADMIN / TEST_UADMIN / FINANCE / ACCOUNTANT`，
老码 `payable:cancel` / `voucher:reverse` 在 prod 也只授了 `SUPER_ADMIN` ⇒ `V0192` 在 prod 是**零变更**，
4 个新码（`voucher:withdraw-reverse` / `voucher:submit-reverse` / `payable:withdraw-cancel` / `payable:submit-cancel`）
仍只有超管持有。要让非超管财务自助发起 / 撤回：建 `FINANCE` / `ACCOUNTANT` 角色 → 绑用户 → 重放 `V0192`（幂等）。
在此之前前端**保留**「按权限隐藏撤回入口 + 详情说明缺哪个码」的兜底（`withdrawReverseBlockedReason` / `withdrawCancelBlockedReason`），
没有改成固定文案「请联系管理员撤回」。

## 八、改动清单

```text
frontend/src/views/business/erp/voucher/index.vue            删兜底（函数/映射/inject）+ 详情归一
frontend/src/views/business/erp/voucher/voucher-logic.ts     删 pendingReverseInstances + VOUCHER_REVERSE_BIZ_TYPE
frontend/src/views/business/erp/voucher/voucher-write-api.ts 两字段 可选 → 必填 string | null
frontend/src/api/business/erp/index.ts                       PayableRow + cancel_instance_id / cancel_reason
frontend/src/views/business/erp/payable/payable-logic.ts     approvalTargetOf() / cancelReasonText()
frontend/src/views/business/erp/payable/index.vue            refreshApprovalProgress() + 抽屉「作废原因」
frontend/tests/unit/pflow3b-cancel-reverse.test.ts           ⑧ 改写 2 条 + ⑨ 新增 2 条
```
