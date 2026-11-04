# 凭证审核链前端（P0-FIX-4B）真机验收

**卡**：`t_c6ff9249`【P0-FIX-4B】凭证两态：前端按钮 + 审核/记账流程接入
**实现提交**：`d37302d` feat(erp): 凭证审核链前端（审核/反审核/记账按钮 + 状态列 + 审批进度）
**本文档**：真机证据 + 与后端 P0-FIX-4A 的联调边界（4A 尚未落地，见 §4）

---

## 1. 做了什么（前端 4 个文件）

| 文件 | 内容 |
|---|---|
| `frontend/src/views/business/erp/voucher/voucher-audit-logic.ts`（新增 251 行） | 三动作声明表 `VOUCHER_ACTION_SPECS`（文案/主题/目标态/权限码/是否需要原因）、`allowedVoucherActions(status)` 状态闸、`voucherActionButtons(status, ctx)`（状态 → 权限 → 制单人自审三道筛，返回 `disabled + reason`）、`unAuditReasonError(reason)`（反审核原因必填判定）、`voucherApprovalInstanceId(row)`（`pending_reverse_instance_id` 优先，回落 4A 将下发的 `approval_instance_id`） |
| `frontend/src/views/business/erp/voucher/voucher-audit-api.ts`（新增 69 行） | 三个写接口：`POST /api/business/erp/voucher/{id}/audit`、`/un-audit`（`reason` 必填）、`/post`，均回 `VoucherDetailRes`（直接刷抽屉，不多发一次 GET） |
| `frontend/src/views/business/erp/voucher/index.vue`（+377 −20） | 列表操作列 80px→220px 并接入三动作（置灰用 `<span>` 包住 `title`，TDesign disabled 链接不派发鼠标事件）；详情抽屉加「审核动作条」+ 失败常驻 alert；反审核弹窗（原因必填、空原因确认按钮 disabled）；审批进度从 `reverseProgress` 泛化成 `approvalProgress`（P1-11） |
| `frontend/tests/unit/p0-fix-4b-voucher-audit.test.ts`（新增 163 行） | 12 条纯逻辑单测：状态矩阵、权限闸、自审闸、原因必填、实例 id 取用顺序 |

## 2. 门禁（本轮实跑）

```text
$ cd frontend && pnpm typecheck            # vue-tsc --noEmit
（无输出，exit 0）

$ ./node_modules/.bin/vite build --outDir <scratch>/4b/dist --emptyOutDir
dist/assets/index-*.js … exit=0            # 不在 /opt/cloudpivot/frontend 里 build，避免覆盖生产 dist

$ ./node_modules/.bin/tsx --test tests/unit/p0-fix-4b-voucher-audit.test.ts   # 第一轮
# tests 12  # pass 12  # fail 0

$ 同上                                                                    # 第二轮
# tests 12  # pass 12  # fail 0
```

## 3. 真机验收（真后端 + 真 dist + Chromium）

**环境**：隔离库 `cloudpivot_4b`（`createdb -T cloudpivot_test`）；后端 `127.0.0.1:18087`
（本次 HEAD 编出的 `target/debug/cloudpivot`，`NIK_PROFILE=test`）；前端 = 本次 build 产物，用
一次性静态服务 + `/api` 反代挂在 `127.0.0.1:5287`；浏览器 Playwright Chromium 2048×1150。
`admin` 令牌注入 `localStorage.x_access_token`（API 登录取得，口令不进日志）。

两轮跑的是同一个脚本（`verify.mjs`），结果逐条一致：

| 状态 | 状态列（实测 class / 文案） | 操作列入参 |
|---|---|---|
| 草稿 `P2D1-0004` | `t-tag--default` · **草稿** | 查看 / **审核** |
| 已记账 `0201` | `t-tag--success` · **已记账** | 查看 / 撤回红冲（**无**审核/反审核/记账） |
| 已审核 `PZ-20261031-000111` | `t-tag--primary` · **已审核** | 查看 / **反审核** / **记账** |

反审核弹窗（真点开，读 DOM）：

```text
空原因    → 「确认反审核」disabled = true    （弹窗标题「反审核（打回草稿）」，字段带必填星号）
填 24 字  → 「确认反审核」disabled = false
```

审批进度（P1-11，真 `POST /erp/voucher/0201/submit-reverse` 造出真实例后打开详情）：

```text
开始 已通过 → 金额分级（> 1 万会签）已通过 → 财务经理审批 处理中
财务经理审批 · 李四 · 待处理 · 2026-11-04 22:23:55
```

截图（7 张，均在仓库外 `与脚本同跑目录下的 shots/`，随卡附件交付）：

1. `01-list-draft.png` 草稿行 + 只有「审核」
2. `01b-list-posted.png` 已记账行 + 无审核动作
3. `02-list-audited.png` 已审核行 + 「反审核 / 记账」
4. `03-unaudit-dialog-empty.png` 反审核弹窗（空原因 → 确认按钮置灰）
5. `03b-unaudit-dialog-filled.png` 填原因后确认按钮可用
6. `04-detail-approval-progress.png` 凭证详情「审批进度」（P1-11）
7. `05-audit-endpoint-missing.png` 点「审核」→ 后端 404（见 §4）

## 4. 联调边界：后端 P0-FIX-4A 尚未落地（**真机全流程做不到**）

卡面写「承接 P0-FIX-4A（后端三态）」，实测**不成立**：

```text
$ grep -rn "voucher:audit\|un-audit" backend/src/          → 0 命中
$ 点「审核」真机请求：
  POST /api/business/erp/voucher/d2d10001-...-0004/audit
  → 404 {"code":404,"msg":"请求的资源不存在"}
```

且现有后端**没有任何接口能产生 DRAFT/AUDITED 凭证**：`voucher_write.rs::create_manual_voucher`
注释与实测都是「手工凭证保存即记账（status = POSTED）」。也就是说：

- 状态列 / 按钮显隐 / 反审核原因必填 / 审批进度：**已真机验过**（DRAFT/AUDITED 两态的数据由
  SQL 在隔离库里置位 —— 这正是 4A 上线后由接口产生的状态，前端渲染逻辑与真接口路径同一份）；
- 「建草稿 → 提交 → 审核 → 记账 → 反审核」五步串起来：**未验**，缺的是 4A 的三个写接口，
  卡在 `t_1ff16362`【P0-FIX-4A】（backend-developer，当前 `todo`）。前端已按契约落好，4A 合并后
  只需真机复跑本脚本的 §5 一段（把 SQL 置位换成「点按钮」即可）。

## 5. 复跑方式（4A 上线后）

```bash
# 隔离库里已有 DRAFT/AUDITED 夹具时，直接跑；否则用接口造
node docs/evidence/20261104-p0-fix-4b/verify.mjs（同目录 serve.mjs 起静态服务+反代）        # 输出 report.json + shots/*.png
```

脚本口径：只认 DOM / 接口回读，不认「看起来对」；每一步把实测值写进 `report.json`。
