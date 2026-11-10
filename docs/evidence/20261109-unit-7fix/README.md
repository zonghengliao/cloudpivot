# t_ee957bf4 前端单测 7 条口径对齐 —— 证据

日期：2026-11-09 22:4x（CST）
基线提交：`0980a24c`（HEAD，含 `42cec90b`）
修复提交：`51e081b9`（只含 6 个 `frontend/tests/unit/*.test.ts`，已 push origin/main）
性质：**漏改测试**（实现是有意的口径变更），**未改任何 `src/` 代码**。

## 1. 复现与结果（同一套源码，只差测试文件）

基线做法：`git archive HEAD`（整棵树，含 backend/migrations/docs）解到临时目录，
`node_modules` 软链到工作区，跑同一条命令，避免共享工作区里他人未提交改动污染基线。

```bash
# 基线（HEAD 0980a24c，整树导出）
tsx --test "tests/unit/**/*.test.ts"   → # tests 1268 / pass 1261 / fail 7 / skipped 0
# 修复后（/opt/cloudpivot/frontend 工作区 = 51e081b9）
tsx --test "tests/unit/**/*.test.ts"   → # tests 1268 / pass 1268 / fail 0 / skipped 0 / todo 0
```

基线 7 条红（test id 与卡面完全一致）：

| id | 用例 |
|---|---|
| 6 | 固定资产：「确认启用」受闸门约束，红字在弹窗里，附件挂详情页签 |
| 145 | 资产分类·②请求体：编码留空发 null（交给后端取号），填了转大写只 trim |
| 149 | 费用类型·②toTypePayload：编码留空发 null（交给后端取号），填了转大写只 trim |
| 244 | 前端状态边 = 后端 commission_dto::LEGAL_EDGES（源码即契约） |
| 507 | create payload keeps dates and folds empty remark to null |
| 728 | validateTypeForm：编码可留空（后端取号 ET-01）、科目必填、上限非负 |
| 729 | toTypePayload：编码大写化、0 上限原样提交、备注空串不发 |

日志：`full_suite_HEAD.txt`（1261/7）、`full_suite_WT.txt`（1268/0）。

## 2. 逐条：现在的代码到底该是什么行为（读实现确认，不是照测试改）

### ① id 6 固定资产启用不再是硬闸（635b07fe E2E05-FIX-2）
- `frontend/src/views/business/erp/asset/index.vue:749-760`：`:confirm-btn` 只剩 content/loading/theme，**没有 disabled**。
- `index.vue:777-793`：`<t-alert v-if="statusChangeKind === 'IN_USE' && !assetGate.canProceed" theme="warning" data-testid="asset-enable-gate-hint">` + `<t-link data-testid="asset-enable-upload-link">`。
- `index.vue:1554-1557`：缺附件 → `confirmEnableWithoutAttachment(target)`（不 return 静默失败）。
- `index.vue:1570-1595`：二次确认按钮文案「无附件启用」，`onConfirm` → `doSubmitStatusChange()`。
- 后端留痕：`backend/src/business/erp/asset_handler.rs:1501 OP_ENABLE_WITHOUT_ATTACHMENT = "ERP_ASSET_ENABLE_NO_ATTACH"`，`1546` 写警告文案、`1577` 写操作日志。
- 测试改法：正向钉「黄字只在缺附件时出现 + theme=warning + 文案来自后端 assetGate.hint + 点名审计码 + 上传入口」，
  并**反向**断言 `!page.includes("disabled: statusChangeKind === 'IN_USE' && !assetGate.canProceed")`
  与 `!/disabled\s*[:=][^\n]*assetGate/`（对象属性、属性绑定两种写法都挡；变异验证过只挡一种会留后门）。

### ② id 145 / 149 / 728 / 729 主数据编码「大小写原样保留」（e53a4937 E2E08-FIX-2 ①）
- 前端唯一口径：`frontend/src/views/business/erp/shared/master-code.ts:42` `MASTER_CODE_PATTERN = /^[A-Za-z0-9._-]{1,40}$/`、
  `:52-55 toMasterCodeInput()` **只 trim、不转大写**；`masterCodeError` 不参与大小写。
- 后端：`backend/src/business/erp/seq.rs:116 is_manual_code_char`（ASCII 字母大小写都收）、
  `:124 manual_code()`（trim + 空折 None）、`:134 validate_manual_code()`（只查长度/字符集，**无 to_ascii_uppercase**）。
- 归类同口径的页面：`asset-category/index.vue:384`、`expense/expense-logic.ts:639`（`type_code` 走同一 helper，`TYPE_CODE_PATTERN = MASTER_CODE_PATTERN`）。
- 测试改法：`'  ac-01 ' → 'ac-01'`、`'cat-building' → 'cat-building'`、`TYPE_CODE_PATTERN.test('travel') === true`、
  `toTypePayload({type_code:' travel '}).type_code === 'travel'`，并保留「本来大写照旧（AC-01 / ET-01）」与「留空 → null」。

### ③ id 244 佣金状态边：**不是真 bug，是排版脆断言**（d9d25d83 全仓 rustfmt）
- `backend/src/business/erp/commission_dto.rs:30` `STATUSES = [DRAFT, APPROVED, PAID]`、
  `:40 LEGAL_EDGES = [(STATUS_DRAFT, STATUS_APPROVED), (STATUS_APPROVED, STATUS_PAID)]`；
  `git show d9d25d83 -- commission_dto.rs` 只有 6+/18-，LEGAL_EDGES 从两行折成一行，**内容与顺序未变**。
- 前端 `commission-logic.ts:41 COMMISSION_SETTLEMENT_STATUSES`、`:48 COMMISSION_STATUS_EDGES` 与后端逐值一致
  → 契约没有不一致，旧断言红是因为它逐字匹配换行（`...;\n    (STATUS_DRAFT...`）。
- 测试改法：改成解析源码语义 —— 取后端 `STATUS_*` 字面值 → 校验 `STATUSES` 长度/顺序 = 前端状态域；
  解析 `LEGAL_EDGES` 的 `(STATUS_A, STATUS_B)` 对 → 与前端边表**逐边同序** deepEqual + 声明的边数 = 字面量条数
  + 业务不变量「无任何边指回 DRAFT」。同步抓「多一条/少一条/换方向」，且不再钉排版。
  **登记结论：244 无需后端改动；若后续后端真的改了边表，新断言会红。**

### ④ id 507 建单 payload 显式带 currency_code（f45e23ed SO-42 / V0316）
- `frontend/src/views/business/erp/sales-order/sales-order-query.ts:737` `currency_code: form.currency_code || null`
  （注释：空 = 账套本位币，外币由后端按单据日锁汇率）。
- 测试改法：`buildCreatePayload` 期望补 `currency_code: null`（与既有 `contract_id/salesperson_id` 显式 null 同风格）。

## 3. typecheck

- 干净 HEAD（`0980a24c` 整树导出，同一 node_modules）：`vue-tsc --noEmit` **exit 0，零输出**（`typecheck_clean_HEAD.txt`）。
- 共享工作区当时 `pnpm typecheck` 会红 2 条：`src/stores/notice.ts(16,1)/(17,1) TS6133`（`useUserStore` / `PERM` 未使用），
  来自**其他卡在途的未提交改动**（`git status` 里 notice/SSE 相关 4 个文件），与本卡无关；
  且 `frontend/tsconfig.json` 的 `include` 只有 `src/**`，`tests/unit` 根本不在类型检查范围 —— 本卡 6 个测试文件**不可能**造成该报错。

## 4. 提交

```
51e081b9 test(frontend): 7 条单测对齐最新口径（附件启用降级 / 主数据编码原样 / 佣金边表 / 币种字段）
 frontend/tests/unit/attachment-gate-pages.test.ts  | 56 ++++++++++++++++----
 frontend/tests/unit/code07-gen-code-button.test.ts | 35 +++++++++-----
 frontend/tests/unit/erp-commission.test.ts         | 51 ++++++++++++++++----
 frontend/tests/unit/erp-sales-order.test.ts        |  2 +
 frontend/tests/unit/fin2-asset.test.ts             |  4 +-
 frontend/tests/unit/fin3-expense.test.ts           | 14 ++++--
```

只 `git add` 这 6 条路径（共享工作区里他人的 backend/src、frontend/src 未提交改动一律未纳入）。
无 `test.skip` / `todo`（全量运行 skipped=0 todo=0）。
