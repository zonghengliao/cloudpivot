# NEAR-FIX-1（卡 t_67dcb9cb）证据包 —— 收款退款单前端 + 两个死菜单

> 全部数字与结论均来自本机（99）真实执行，命令与输出贴在下面。**没有碰 144 生产**。

## 一、问题与结论（先给判断）

| 评审原文 | 结论 | 依据 |
|---|---|---|
| 「前端有 2 个死菜单（价格查询、收款退款单，path 为空、无页面、无路由）」 | 属实，两个菜单行都是 `path IS NULL` | `SELECT … FROM sys_menu WHERE menu_type=2 AND (path IS NULL OR path='')` → 在 145 真库上返回 2 行（…3071 价格查询 / …2950 收款退款单） |
| 「收款退款单后端 API 完整、前端整块缺失」 | 属实 | `receipt_refund.rs` 5 个接口 + 审批桥 + 过账齐全；`frontend/src/views/business/erp/` 下无 `receipt-refund/`，`router/index.ts` 无该路由 |
| 「价格查询同样无页面、无路由」 | 属实，但**不能删菜单** | 见下 §二 的 B 方案依据 |

## 二、「价格查询」处置：选 B（补页面），不选 A（删菜单）

四条依据，前两条是硬的：

1. **那行菜单是权限码 `business:erp:price:resolve` 的唯一承载行**。`V0307` 自己的自检断言
   「该 auth_code 恰好 1 行」。`scripts/lint/perm_menu_gate.py`（check.sh 闸⑥）要求每个
   `#[nik::permission]` 码都有承载行 —— **删掉这行，门槛变红**，而且除 `SUPER_ADMIN`
   外**任何人都调不动** `GET /business/erp/price/resolve`（不报错、不打日志，就是 403）。
2. **后端接口是完整的、业务上有诉求**：SO-33 的 5 级确定性取价（客户协议价 → 客户专属阶梯 →
   通用阶梯 → 物料标准售价 → 无价），是销售报价的真实需要，不是「早期规划没做」。
3. **V0307 头注释的原始设计意图就是「等页面来」**：原文第 22-23 行「菜单名叫「价格查询」——
   将来价格维护页落地时可以把它改成那个页面的父节点」，第 30-36 行连收尾 UPDATE 都写好了
   （`path = '/business/erp/price'` / `component = 'business/erp/price/index'`）。
4. 判据可验：`GET /api/business/erp/price/resolve?material_id=…&qty=10` 在本机实例上
   返回 `{"code":0,…,"source":"NONE"}` —— 接口活着，缺的只是界面。

⇒ 落地：`frontend/src/views/business/erp/price/index.vue`（取价查询页，只读）+
`api/business/erp/price.ts` + 路由 `business/erp/price` + 菜单 path 补全。

## 三、改了什么

### 3.1 后端

| 文件 | 改动 |
|---|---|
| `backend/migrations/business/V0368__receipt_refund_page_and_price_page.{up,down}.sql` | ① 收款退款单菜单：`path=/business/erp/receipt-refund`、`component=business/erp/receipt-refund/index`、`is_visible=TRUE`、`parent=销售管理(…2701)`、`sort=55`（收款单 50 之后、销售发票 60 之前）；② 价格查询菜单：`path=/business/erp/price`、`component=business/erp/price/index`、`is_visible=TRUE`、`parent=销售管理`、`sort=75`（客户价格 70 之后、销售目标 78 之前）；③ 附件类型 16 → 17（`+RECEIPT_REFUND`，两张约束表都放宽）；④ 5 组施工后断言（含「全库无空 path 页面菜单」） |
| `backend/src/business/erp/receipt_refund.rs` | ① `RefundRes` 补 `source_receipt_no`（AGENTS 约定 13：`*_id` 必须同时给名字），列表由 `LEFT JOIN biz_receipt` 带出、详情现查；②**修一个实测抓到的真 bug**：`method` / `bank_account` 绑的是空串而不是 NULL，撞 `ck_biz_receipt_refund_method` → 「不选退款方式」这个合法操作变 400，且错误文案完全对不上症状；③ 新增 2 条单测 |
| `backend/src/business/shared/doc_kind.rs` | `ATTACHMENT_DOC_KINDS` 16 → 17（追加 `DocKind::ReceiptRefund`） |
| `backend/src/business/attachment/model.rs` | **附件模块有自己的一套 `DocKind` 枚举**（解析 / 表名 / 存在性 SQL / 读码 / 角色组），同步 16 → 17 并在 `/opt` 实测发现：只改 `shared/doc_kind.rs` 不够，上传会被 attachment 自己的白名单拒掉（`单据类型 doc_type 非法：RECEIPT_REFUND`） |
| `backend/src/business/attachment/tests.rs` | 两处 16 → 17 断言 + 新类型的读码表 |
| `docs/architecture/snapshot-drift/10_checks.sql` | `ATTACHMENT_DOC.RECEIPT_REFUND` 登记行 + 悬空检查分支（登记了不查 = DI-22 绿、DI-23 漏报） |

### 3.2 前端

| 文件 | 改动 |
|---|---|
| `frontend/src/views/business/erp/receipt-refund/index.vue`（新，约 1200 行） | 列表（单号/来源收款单/客户/金额/已冲减/方式/状态/审批 + 筛选分页）、详情抽屉（冲减明细 + 审批进度 `FlowProgress` + 附件 `DocAttachmentPanel`）、登记弹窗（选收款单→带出可退上限）、核销弹窗（逐张冲减，本地校验单张 ≤ 可退 / 合计 ≤ 退款金额）、确认 / 提交审批 / 撤回 / 直接过账 / 作废 |
| `frontend/src/views/business/erp/receipt-refund/receipt-refund-logic.ts`（新） | 纯逻辑：状态域、权限码、列定义、按钮判据（`canConfirm`/`canPost`/`canSubmitApproval`/`canWithdraw`/`canCancel`）、冲减校验、原因与金额校验 |
| `frontend/src/views/business/erp/price/index.vue`（新） | 取价查询：客户（选填）+ 物料 + 数量 + 基准日 → 不含税单价（填单用）/ 命中原值 / 税率 / 命中来源 + 「五级取价为什么是这个价」 |
| `frontend/src/api/business/erp/receipt-refund.ts`（新） | page / detail / create / allocate / status / submit-approval |
| `frontend/src/api/business/erp/price.ts`（新） | `resolvePrice()` + 五级来源词表 |
| `frontend/src/router/index.ts` | 注册 `business/erp/receipt-refund` 与 `business/erp/price` |
| `frontend/src/api/business/erp/doc-kinds.ts` / `attachment.ts` | `ATTACHMENT_DOC_TYPES` 16 → 17、`DOC_TYPE_LABELS` / `DEFAULT_ROLE_BY_DOC` 补新类型 |
| `frontend/tests/unit/erp-receipt-refund.test.ts`（新） | 25 条纯逻辑用例 |
| `frontend/tests/unit/attachment-gate-pages.test.ts` | 词表 17 值断言 |

## 四、真实执行输出

### 4.1 迁移幂等（nearfix1 库，跑完再手动重放一次 up.sql）

```
NOTICE:  V0368 §1 完成：收款退款单页已开（path=/business/erp/receipt-refund component=… parent=销售管理 sort=55），本次改 0 行；4 个按钮父级完好
NOTICE:  V0368 §2 完成：价格查询页已开（path=/business/erp/price … sort=75），本次改 0 行
NOTICE:  V0368：ck_biz_doc_attachment_type 已含 RECEIPT_REFUND，跳过
NOTICE:  V0368：ck_biz_attachment_rule_doc_type 已含 RECEIPT_REFUND，跳过
NOTICE:  V0368 完成：收款退款单 / 取价查询两页已开，全库无空 path 页面菜单，附件类型 17 值
```

### 4.2 干净库（nearfix2，299 条迁移全新跑）上的门禁

```
$ bash scripts/verify/menu_invariants.sh nearfix2
① 孤儿 parent:  0 ✅   ② /system/ 归属: 0 ✅   ③ /business/ 归属: 0 ✅
④ path 唯一性:  0 ✅   ⑤ 菜单总数:   433 ✅ (400-460)
⑥ title 长度:   0 ✅   ⑦ 同父 sort 唯一: 0 ✅  ⑧ icon 名合法: 0 ✅
结果: 全绿 ✅

$ SYS_DB_URL=…/nearfix2 cargo test --lib system::menu::tests::n08 -- --nocapture
[N08] 118 条菜单路径全部能在前端路由里解析 ✓      （两条新菜单的 path 都在其中）
```

### 4.3 收款退款单全链路（真实 token curl，profile=test，:8099）

```
15) 新建退款单
{"code":0,"no":"RR-20261110-000001","status":"DRAFT","amount":"400.0000","src_no":"RC-20261110-000001"}
16) 列表（source_receipt_no 由后端 JOIN 带出）
{"refund_no":"RR-20261110-000001","source_receipt_id":"4ec78c86-…","source_receipt_no":"RC-20261110-000001","customer_name":"NEAR-FIX1 退款实测客户",…}
17) 冲减 401（> 退款金额 400）→ 拦
{"code":10001,"msg":"冲减总额 401 超过退款金额 400.0000（退款单 RR-20261110-000001）…"}
    冲减 400 → {"code":0,"alloc":"400.0000","allocs":[{"ar_no":"AR-20261110-000001","amount":"400.0000","ar_received":"600.0000","ar_status":"PARTIAL"}]}
18) 应收：1000 → 600（退款冲减回写生效）
{"ar_no":"AR-20261110-000001","received":"600.0000","unsettled":"400.0000","status":"PARTIAL"}
19) DRAFT 直接过账 → {"code":10003,"msg":"…不能从「草稿」变成「已过账」：合法去向是 「已确认」 / 「已作废」"}
    CONFIRMED 但未审批就过账 → {"code":66044,"msg":"…还没通过审批，不能过账：先调 …/submit-approval …"}
30) 送审 → {"status":"CONFIRMED","approval_status":"PENDING","approval_no":"FLW-20261109-A76E3E77"}
31) finance01（FINANCE_MANAGER）待办 1 条 → approve → {"code":0}
32) 审批通过后**自动过账**
{"status":"POSTED","status_label":"已过账","voucher_no":"PZ-20261110-000002","approval":{"status":"APPROVED","pending_tasks":0}}
34) 退款凭证分录
{"voucher_no":"PZ-20261110-000002","source_type":"RECEIPT_REFUND","source_no":"RR-20261110-000001","total_debit":"400.0000","total_credit":"400.0000"}
{"account":"1122","account_name":"应收账款","debit":"400.0000","credit":"0.0000","summary":"收款退款 RR-20261110-000001 …恢复应收账款"}
{"account":"1002","account_name":"银行存款","debit":"0.0000","credit":"400.0000","summary":"收款退款 RR-20261110-000001 …银行付款"}
35) 退款凭据附件（doc_type=RECEIPT_REFUND —— V0368 放宽生效）
{"code":0,"file_name":"bank-receipt.pdf","file_role":"BANK_RECEIPT"}
41) 附件分栏 + 上传人快照
{"total":1,"roles":[{"code":"BANK_RECEIPT","label":"银行回单","count":1},{"code":"DEFAULT","label":"其他附件","count":0}],
 "items":[{"file_name":"bank-receipt.pdf","file_role_label":"银行回单","uploader_name":"超级管理员","file_size":193}]}
38) 不给 method（可选列→NULL）建单：{"code":0,"no":"RR-20261110-000002","method":null,"method_label":"未指定"}
39) 作废：原因 2 字 → 400；足字 → {"status":"CANCELLED"}；再作废 → {"code":10003,"msg":"…已经是「已作废」，没有状态变化"}
40) 已过账再作废 → {"code":10003,"msg":"…不能从「已过账」变成「已作废」：合法去向是 （终态，没有去向）"}
44) 撤回链路（前端「撤回」按钮）：
    新建 RR-20261110-000003 → 冲减 100 → 确认 → 送审
    {"approval_status":"PENDING","no":"FLW-20261109-0CAC5CBA"}
    POST /business/flow/instance/{id}/withdraw → {"code":0,"msg":"ok"}
    撤回后详情：{"status":"CONFIRMED","voucher_no":"","approval":{"status":"WITHDRAWN","pending_tasks":0}}
    ⇒ 撤回只撤审批，**不推进单据状态、不生成凭证**；此前已做的冲减仍生效
      （应收已收 600 → 500 是那张冲减 100 的结果，不是撤回来的）
47) 撤回后再送审 → {"approval_status":"PENDING","no":"FLW-20261109-56CC044E"}
    ⇒ 与前端 `canSubmitApproval`（`approval_status !== 'PENDING'` 才亮）口径一致
```

前置链路（收款单侧，同一实例）：客户 → 手工挂账 1000 → 收款单 1000 → 核销到应收 →
**上传银行回单**（必传闸门，实测缺附件时确认被 400 拦住）→ 确认 → 过账
`PZ-20261110-000001` → 应收 1000 / SETTLED。**退款单的「来源收款单」要求 POSTED**，
所以这条前置链必须真跑一遍，不能跳过。

### 4.4 145（真实 dev 环境）落地情况

```
$ ssh 145 'cd /opt/cloudpivot && git pull --ff-only origin main'   → 5f8a1596
$ ssh 145 'export PATH=/root/.cargo/bin:$PATH && cd backend && cargo build'
Finished `dev` profile … in 1m 15s

# 迁移直接落到 145 的真库 cloudpivot_dev（幂等脚本，up.sql 可重放）
NOTICE:  V0368 §1 完成：收款退款单页已开（… 本次改 0 行 / §2 本次改 1 行）
NOTICE:  V0368：ck_biz_doc_attachment_type 已放宽到 17 值
NOTICE:  V0368 完成：收款退款单 / 取价查询两页已开，全库无空 path 页面菜单，附件类型 17 值

$ ssh 145 'bash scripts/verify/menu_invariants.sh cloudpivot_dev'
① 0 ✅ ② 0 ✅ ③ 0 ✅ ④ 0 ✅ ⑤ 433 ✅ ⑥ 0 ✅ ⑦ 0 ✅ ⑧ 0 ✅   结果: 全绿 ✅

# 两个新路由在 145 的常驻实例上确实挂载（401=路由在但没带 token；不存在的路由给 404 做对照）
/api/business/erp/receipt-refund/page                       → 401
/api/business/erp/price/resolve?material_id=…&qty=1        → 401
/api/business/erp/__no_such_route                           → 404
```

⚠ **145 的常驻进程还没重启**：`systemctl restart cloudpivot-test` 在本会话被危险命令闸门
拦下（无人在场，不允许重启系统服务）。当前进程启动于 02:30:04，而新二进制是 02:39:44
⇒ **库侧（菜单 / 附件 CHECK）已经生效，二进制侧（`source_receipt_no` 字段、
`attachment::model::DocKind` 的 17 值解析）要等一次 `systemctl restart cloudpivot-test` 才生效**。
在重启之前，145 上「退款凭据」上传仍会被旧进程按 16 值拒掉（99 的新二进制已验通，见 §4.3 第 35 项）。
这一步留给有系统权限的人执行，一条命令。

### 4.5 静/动检查

```
backend:  cargo check --lib ✓   cargo clippy --lib -- -D warnings ✓   cargo fmt ✓
          cargo test --lib business::erp::receipt_refund  → 24 passed
          cargo test --lib business::attachment            → 95 passed
          cargo test --lib business::shared::doc_kind      → 21 passed
frontend: pnpm test:unit   → 1297 passed / 0 failed
          pnpm typecheck   ✓
          pnpm build       ✓
```

## 五、本卡实测发现的额外缺陷（已修）

**「不填退款方式」建单会 400，且文案与症状无关。**
`create()` 把 `method` 用 `Value::Text(method.unwrap_or_default())` 绑定 → 空串，
而 `ck_biz_receipt_refund_method` 只允许 `NULL` 或 4 个白名单值 → 23514，
`write_error` 再把它翻译成「退款金额 > 0；0 ≤ 已冲减 ≤ 退款金额；退款原因 >= 4 字…」
一句和「没选方式」毫无关系的话，用户照着改也改不好。

修法：`method` / `bank_account` 与既有的 `source_receipt_id` / `create_by` / `update_by`
一样走 `NULLIF(?::text,'')`；INSERT 提取成 `INSERT_SQL_TEMPLATE` 常量，
单测 `insert_template_turns_optional_text_into_null` 数 NULLIF 的个数（5 个）钉住，
少一个就红。

## 六、复现方式

```bash
# 1) 空库（本机 99，alun/alun）
su - postgres -c "psql -c 'create database nearfix2 owner alun'"
# 2) 起实例（三把密钥 + 初始 admin 口令都要给）
source <本目录同源的 env2.sh>   # NIK_PROFILE=test / :8099 / nearfix2 / NIK_ADMIN_PASSWORD
./backend/target/debug/cloudpivot
# 3) 依次跑 step1.sh → step2.sh → step3.sh → step4.sh → step5.sh → step6.sh
#    （step3 起 token 会 15 分钟过期，重跑前先重新登录刷新 token.txt）
```

⚠ `profile=dev` 会被缓存安全守卫拒绝启动（`[cache] type = "local"` 必须为 redis，
`shared/startup_guard.rs`）；本机没有 redis，所以实测用 `profile=test`（该守卫只管 prod/dev）。
