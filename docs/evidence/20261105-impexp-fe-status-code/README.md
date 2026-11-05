# 前端：导出状态过滤对齐 status_code 契约（t_f83ae18f）真机证据

日期：2026-11-05 ｜ 上游后端卡：`t_93cba3f4`（commit `dce703b`）

## 改了什么

| 文件 | 改动 |
|------|------|
| `frontend/src/api/business/batch.ts` | `BatchExportPayload` 增 `status_code?: string`；保留 `status?: number`（字典项 / 用户用），注释写清两者语义 |
| `frontend/src/utils/batch-action.ts` | 新增 `exportFilterPayload({ keyword, statusCode })`：空筛选**整键缺席**、有筛选才带 `status_code`，**永不带 `status`** |
| `frontend/src/views/business/erp/{stockin,payable,voucher}/index.vue` | 批量导出 payload 改为 `{ ids, ...exportFilterPayload({ keyword: <单号>, statusCode: filter.status }) }` |
| `frontend/src/api/business/impexp/index.ts` | `createExport` 入参增 `status_code?: string` + 注释 |
| `frontend/tests/unit/impexp-export-status-code.test.ts` | 新增 10 条单测（纯逻辑 / 页面源码契约 / 前端筛选项 ⇄ 后端白名单逐字漂移守卫） |
| `docs/前端二开指南.md` | 新增 §十：批量导出的 `status_code` 口径与范本 |

**没有改后端**（`status_code` 上游已实现），没有动 `plan.sql` / 生产授权。

## 环境

- 前端：`vite dev :5197`，源码即 `/opt/cloudpivot/frontend` 工作树（含本卡改动），
  反代 → `127.0.0.1:18111`；**不碰生产 dist**（`/opt/cloudpivot/frontend/dist` 未 build 未替换）。
- 后端：`HEAD 4127ccc` 的 debug 二进制（17:57 构建，含 `dce703b`），库 `cloudpivot_tf83d`
  （`cloudpivot_deadmin` 的 `pg_dump` 快照，9 张 OPEN 应付 / 21 张 POSTED 凭证 / 9 张 POSTED 收货）。
- 账号：`admin`（超管，跳过权限校验）。

## 结论（7 条用例全绿，数字来自 `真机-report.json`）

| 用例 | payload | HTTP | 导出行数 | 判定 |
|------|---------|------|----------|------|
| 应付单 状态=未付 → 全选导出 | `ids × 9` + `status_code: "OPEN"`，无 `status` | 200 | 9（列表「共 9 条」） | ✅ |
| 应付单 空筛选 → 全选导出 | `ids × 12`，**无** `status_code` / `status` | 200 | — | ✅ |
| 凭证 状态=已记账 → 全选导出 | `ids × 20` + `status_code: "POSTED"` | 200 | 20（列表共 21 条，本页 20） | ✅ |
| 凭证 空筛选 → 全选导出 | `ids × 20`，无状态键 | 200 | — | ✅ |
| 收货单 状态=已过账 → 全选导出 | `ids × 9` + `status_code: "POSTED"` | 200 | 9（列表共 9 条） | ✅ |
| 收货单 空筛选 → 全选导出 | `ids × 14`，无状态键 | 200 | — | ✅ |
| 应付单 手改 payload 为 `status_code: "NOPE"` | 422 | 422 | — | ✅ toast 原文见下 |

非法码时的 toast（**原样**展示后端 `msg`，前端没编文案）：

```
批量操作未执行：状态码「NOPE」不是「erp.payable」的合法取值，
只允许：OPEN / PARTIAL_PAID / SETTLED / CANCELLED / PENDING_CANCEL（不区分大小写）
```

无白屏、无 `pageerror`（`report.json` 里 `pageError` 一条都没有）。

后端契约侧的先验探测（curl，同一台实例）：

```
POST /business/erp/payable/batch-export {"ids":[],"status_code":"NOPE"}
  → 422 / 10001  状态码「NOPE」不是「erp.payable」的合法取值，只允许：OPEN / PARTIAL_PAID / SETTLED / CANCELLED / PENDING_CANCEL（不区分大小写）
POST /business/erp/payable/batch-export {"ids":[],"status":1}
  → 422 / 10001  导出定义「erp.payable」不支持 status=1 过滤；它的状态是文本码，请改用 status_code=…
POST /business/erp/voucher/batch-export   {"ids":[],"status_code":"OPEN"}
  → 422 / 10001  状态码「OPEN」不是「erp.voucher」的合法取值，只允许：POSTED / DRAFT / AUDITED / CANCELLED
```

## 顺带发现的既有缺陷（**不在本卡范围**，已另开卡）

导出成功（200）后**用户看不到任何反馈**：`BatchActionBar` 的根节点是
`v-if="visible"`，而 `visible = selected.length > 0 || running`；`execute()` 成功分支先
`resultVisible = true`，紧接着 `emit('update:selected', [])` 清空勾选 → 整条工具栏连同结果弹窗一起卸载。
本卡 7 条用例里 `resultDialogVisible` 全为 `false`、`.t-message` 里也没有成功提示，
即「导出已提交」这件事只发生在 Network 里。失败路径不受影响（`MessagePlugin` teleport 到 body，toast 照常显示）。

## 复现命令

```bash
# 前端
cd /opt/cloudpivot/frontend && pnpm typecheck && pnpm test:unit   # 817 tests / 815 pass
# 单文件
npx tsx --test "tests/unit/impexp-export-status-code.test.ts"      # 10/10

# 真机（脚本在 workspace，未进仓库）
node <workspace>/tf83/verify.mjs                                   # allOk: true
```

> 注：`pnpm test:unit` 全量 2 条红（`flow-biz-link` 的 `#[nik::get("/business/task/{id}")]` 探测、
> `task-root-only` 的 DTO 归一化）在本卡开工前的 HEAD（`4127ccc` 干净 worktree）上**同样红**，
> 与本卡无关，未改动。
