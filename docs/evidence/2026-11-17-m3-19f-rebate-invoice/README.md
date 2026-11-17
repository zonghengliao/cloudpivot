# M3-19F · 销售返利「开红字发票」前端入口 — 验收留痕

卡：`t_8c53043e`（M3-17 后端已交付 `POST …/invoice` + `…/invoice-preview`，M3-19 只做了只读展示，本卡补 UI 入口）

## 一句话结论

返利结算页的**草稿行**多了一个「开票」动作：点开先看**票面试算**（三列拆分 + 凭证分录 + 能不能开 / 拦在哪），
确认后 `POST …/{id}/invoice`，列表刷新成 `SETTLED` 并出现票号 / 转结时间。三条闸的报文（`66093` / `66094` / `66095`）
都在弹窗里以人话 + 出路呈现，重复开票**不产生第二张票**。

## 改变了什么（前端三件，后端零改动、零迁移）

| 文件 | 改动 |
|---|---|
| `frontend/src/api/business/erp/rebate.ts` | `+previewRebateInvoice(id, payload)` / `+createRebateInvoice(id, payload)` + 4 个类型（字段逐字抄 `RebateInvoiceRes` / `RebateInvoicePreviewRes` / `VoucherLineRes` / `RebateInvoiceReq`）；`REBATE_PERMISSIONS` 补 `settleInvoice` |
| `frontend/src/views/business/erp/rebate/invoice-logic.ts` | **新增**纯逻辑：可开判据 / 税率与日期校验 / 入参整形 / 三列与凭证行 / 借贷合计 / 拦在哪与出路 / 行级 66094·66093 提示 |
| `frontend/src/views/business/erp/rebate/index.vue` | 结算 tab 行操作加「开票」+ 新增开票弹窗（试算 → 确认）+ 开票状态机 |
| `frontend/src/views/business/erp/rebate/settle-logic.ts` | 操作列 145 → 195px（四个入口：详情 / 开票 / 重算 / 作废） |
| `frontend/tests/unit/erp-rebate-invoice.test.ts` | **新增** 22 条单测（含 4 条「源码即契约」：权限码、preview 是 POST、票种/税率/备注上限、66093–66095 在 `error_code.rs`） |

**零后端改动 / 零迁移 / 零新增权限码 / 零新增菜单行**：接口、`business:erp:rebate-settlement:invoice`（V0440）、菜单行都在。

## 与卡面不一致的一处（照代码，不照卡面）

卡面把票面试算写成 `GET …/{id}/invoice-preview`，**实际后端是 POST**
（`rebate_invoice_handler.rs` 的 `#[nik::post]`，要收 `invoice_date` / `tax_rate` 才能算「改日期会不会撞已结账期间」）。
前端按 POST 实现，并在 `api/rebate.ts` 与单测 `erp-rebate-invoice.test.ts` 里各钉一道断言，防止后人照卡面改回 GET（那会 405）。

## 一张「可用」的口径说明

卡面目标 ①写「仅 `DRAFT` 且 `rebate_amount > 0` 时可用」，验收要点 ②写「`rebate_amount = 0` 的单点开票 → 弹 `66094` 人话」。
两条要同时成立，落地口径是：

- **入口**：只有 `DRAFT` 渲染「开票」（`SETTLED` 在「红字发票」列显示票号、`CANCELLED` 不渲染）；
- **能不能真的开出去**：由**试算结论** `can_generate` 决定 —— 金额 ≤ 0 时「确认开票」按钮禁用（`不能开票（见上方原因）`），
  但入口仍可点开，弹窗里给出 `66094` 的码表人话 + 本页出路（见 `01-ui-66094-zero-amount.png`）。
  入口点不动或直接消失都会被当成「权限问题」，比看见原因更糟。

## 验收（全部真实执行）

### ① 门禁 / 构建（在 99 上跑）

| 项 | 结果 | 证据 |
|---|---|---|
| `pnpm test:unit`（全量） | **2238 / 2238 pass, 0 fail**（本卡新增 22 条，此前 2216） | `07_unit_full.txt` |
| `npx tsx --test tests/unit/erp-rebate-invoice.test.ts` | **22 / 22 pass** | `06_unit_invoice_logic.txt` |
| `pnpm typecheck`（vue-tsc） | 0 错 | `08_typecheck.txt` |
| `pnpm build` | ✓ built in 55.45s | `09_build.txt` |

### ② API 链路（真后端 `:18023` + 145 演练库 `cp_8c53043e_e2e`，`admin` 登录）

| # | 动作 | 结果 |
|---|---|---|
| ① | 结算列表 | 3 张草稿（`RB-20261117-900001~3`） |
| ② | 试算 `RB-…900001`（1000 含税 @13%） | `can_generate=true`，`-884.96 / -115.04 / -1000.00`，凭证 3 行借贷平衡 |
| ③ | 开票 `RB-…900001` | **200**，`SETTLED`，`IV-20261117-000001` / `PZ-20261117-000001` |
| ④ | 重复开票同一张 | **409 + 66093**（"该结算单已开过红字发票（SETTLED）：不能重复开票…"） |
| ⑤ | 开票应返 0 的单 | **409 + 66094**（"应返金额 0.0000 不大于 0…"） |
| ⑥ | 试算开票日 2026-10-15（该期间已结账） | **200** 且 `can_generate=false`，`blockers=["会计期间 2026-10 已结账…请把开票日期改到未结账期间，或先反结账"]` |
| ⑦ | 开票开票日 2026-10-15 | **409 + 66095** |
| ⑧ | 同一张单把开票日改成 2026-11-17 | `can_generate=true`（出路可行） |
| ⑨ | 结算单详情 | `SETTLED` + `invoice_no` + `settled_at` 落位 |

原始报文：`10_curl_chain.txt`。

库内终态（`11_db_state.txt`）：

```
RB-20261117-900001 SETTLED IV-20261117-000001
RB-20261117-900002 DRAFT   —                    （应返 0，永不落票）
RB-20261117-900003 SETTLED IV-20261117-000002
RB-20261117-900004 SETTLED IV-20261117-000004
RB-20261117-900005 SETTLED IV-20261117-000003
红字发票 4 张 / 红字凭证 4 张 / 分录 12 行；「一张结算单 ≤ 一张票」零违反（uk_biz_srs_invoice 仍在）
```

### ③ 真浏览器（真前端 `:5173` → 真后端 `:18023` → 真库）

| 步骤 | DOM 断言原文 | 截图 |
|---|---|---|
| 结算列表行操作 | `RB-…900001 | 详情`（SETTLED 无开票）、`RB-…900002 | 详情/开票/重算/作废`、`RB-…900003 | 详情/开票/重算/作废` | — |
| 应返 0 的单点「开票」 | 弹窗含 `这张结算单的应返金额是 0 或负数…`（66094 码表人话）+ `出路：…请作废该单…` + `拦在：应返金额 0.0000 不大于 0…`；确认按钮 `不能开票（见上方原因）|disabled=true` | `01-ui-66094-zero-amount.png` |
| 开票日改 2026-10-15 | 三列 `-442.48 / -57.52 / -500.00`，凭证 `借 5001 442.48 / 借 2221.02 57.52 / 贷 2241.02 500.00`，`合计 借 500.00 / 贷 500.00 借贷平衡`，`拦在：会计期间 2026-10 已结账…`，确认禁用 | `02-ui-66095-period-closed.png` |
| 开票日改 2026-11-17 | 拦截消失，确认按钮 `确认开票（价税合计 -500.00）|disabled=false` | `03-ui-preview-ready.png` |
| 点确认开票 | toast：`已开红字发票 IV-20261117-000002（价税合计 -500.00，凭证 PZ-20261117-000002）；结算单 RB-20261117-900003 转为「已开红字发票」`；行变 `已结算 | IV-20261117-000002`，开票入口消失 | `04-ui-invoice-success-toast.png` |
| 页面开着弹窗时另一会话抢先开票，再点确认 | 弹窗内红条：`开票失败：该结算单已开过红字发票（SETTLED）：不能重复开票…（请求 ID：7ebab9b0-…）` + `出路：这张单已经不是草稿了…`；**列表自动刷新**该行变 `已结算 | IV-20261117-000004`（无白屏、无第二张票） | `05-ui-409-duplicate.png` |

## 本轮发现的后端缺陷（**本卡不改，已另立卡**）

**`2241.02` 科目被佣金占用，返利红字凭证实际贷记「其他应付款-佣金」。**

- `V0311`（佣金结算，M3-36）先种了 `2241.02 其他应付款-佣金`；
- `V0440`（本模块）以为 `2241.02` 是空号，`INSERT … WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code='2241.02')`
  **静默跳过**，只把名称写在注释与自检里（自检只验「存在且是 2241 下的末级负债科目」，不验名称）；
- 结果：全新空库首启也一样（迁移顺序 V0311 → V0440 固定），返利凭证贷 `2241.02`，总账里显示为**佣金**应付。
  证据：`11_db_state.txt` 的「科目实时名」列全是 `其他应付款-佣金`；预览弹窗截图里也能直接看到。

影响：返利负债与销售佣金混在一个末级科目里（金额不会错，科目名与对账口径错）。
卡面要求「除非发现缺陷另立卡」，已按此处理（子卡：`V0445` 另种 `2241.03 其他应付款-销售返利` + 改
`rebate_invoice_handler::ACCOUNT_REBATE_PAYABLE` + 对已有凭证的处理口径）。

## 复跑方式

```bash
# 前端门禁
cd frontend && pnpm test:unit && pnpm typecheck && pnpm build

# 真链路（99 上；演练库只建在 145）
export PGPASSWORD=*** && createdb -h 172.18.10.145 -U cloudpivot cp_<卡号>_e2e
# 起后端：NIK_PROFILE=dev / NIK_DATABASE_HOST=172.18.10.145 / NIK_DATABASE_NAME=cp_<卡号>_e2e
#          NIK_SERVER_LISTEN=18023 / NIK_ADMIN_PASSWORD=Admin@123456 / JWT + 存储主密钥
# 起前端：cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18023 pnpm dev
# 夹具：scripts 见下方「夹具 SQL」（3 张草稿：正常 / 应返0 / 开票日撞已结账期间）
```
