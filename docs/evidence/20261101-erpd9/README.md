# D9 单据复制（收货单 / 调拨单 / 任务）— 验收证据

> 卡：`t_b83957f1`【D9】单据复制（收货/调拨/任务，10 单测）
> 日期：2026-11-01 · 环境：99 工作仓 + 隔离库 `cloudpivot_d9` / `cloudpivot_d9full`

## 一句话结论

三个单据的「复制为新单」全部落地：**原单只读、新单走 biz_seq 重新取号、状态回 DRAFT、
采购单关联清空**。后端 14 条真库单测 + 真 HTTP e2e **50/50** + 页面 **13/13** + 全量 `cargo test --lib`
与 ERP 不变量审计，逐项见下。**零迁移**（复用建单权限码，不新种码）。

## 一、交付内容

| 单据 | 接口 | 权限码 | 语义 |
|------|------|--------|------|
| 收货单 | `POST /api/business/erp/stock-in/{id}/copy` | `business:erp:stock-in:create` | 单头 + 明细；采购单关联清空；入库日期=今天 |
| 调拨单 | `POST /api/business/erp/transfer/{id}/copy` | `business:erp:transfer:create` | 单头 + 明细；序列号不复制；调拨日期=今天 |
| 任务 | `POST /api/business/task/{id}/copy` | `business:task:create` | 标题+「（副本）」；子任务/评论不复制 |

响应 = 新单的**完整详情**（与 create 同形状），前端拿到直接填表（`form.id` = 新单 id，
所以「保存」是改这张草稿，连点不会多建一张）。

### 三处设计决策（写进了代码注释）

1. **来源采购单清空**（卡面 §二 决策点选 B）：原单过账时已回写 `received_qty`，
   复制单再挂同一采购单会二次占用待收额度（轻则过账超收 400，重则两张单各收一次）。
   前端在复制成功后在编辑弹窗顶部给一条提示：「来源采购单已清空…请重新关联」。
2. **入库 / 调拨日期取今天**：复制是新的业务动作，不是补录原单那天；沿用旧日期会落到
   可能已结账的期间（过账被期间闸拒），也会与原单在「按日期查列表」时混淆。
3. **序列号不复制**（收货单明细 `serial_ids`、调拨单 `item_serial`）：号码属于实物，
   原单那批已经过账占用；复制带上会「同号占两次」或「一件货记两件」。

任务的另外两条：**父任务清空**（副本是独立新任务，否则父任务进度分母悄悄 +1）、
**日期不复制**（照抄旧截止日 = 建出来就逾期，按新建口径取本周五）。

## 二、后端单测（14 条，真库 `cloudpivot_d9`）

命令：`bash scripts/verify/d9_unit_tests.sh`

```
== D9 用例（收货 / 调拨 / 任务 复制） ==
running 13 tests
test business::erp::stockin_handler::tests::d9_stockin_copy_creates_draft_with_new_no_and_same_items ... ok   ← ①
test business::erp::stockin_handler::tests::d9_stockin_copy_clears_purchase_order_id ... ok                  ← ②
test business::erp::stockin_handler::tests::d9_stockin_copy_clears_voucher_payable_and_approval_links ... ok ← ③
test business::erp::transfer_handler::db_tests::d9_transfer_copy_keeps_header_and_items_but_resets_state ... ok ← ④
test business::task::handler::copy_db_tests::d9_task_copy_keeps_fields_but_not_children ... ok               ← ⑤
test business::erp::stockin_handler::tests::d9_stockin_copy_works_when_source_purchase_order_is_gone ... ok  ← ⑥
test business::erp::stockin_handler::tests::d9_stockin_copy_without_items_is_400 ... ok                      ← ⑦
test business::erp::stockin_handler::tests::d9_stockin_copy_three_times_yields_three_distinct_nos ... ok     ← ⑧
test business::erp::stockin_handler::tests::d9_stockin_copy_does_not_touch_origin ... ok                     ← ⑨
test business::erp::stockin_handler::tests::d9_stockin_copy_requires_create_permission ... ok                ← ⑩
test business::erp::transfer_handler::db_tests::d9_transfer_copy_requires_create_permission ... ok           ← ⑩b
test business::task::handler::copy_db_tests::d9_task_copy_requires_create_permission ... ok                  ← ⑩c
test business::permission::scope_model::tests::d9_role_scope_res ... ok                                      （既有用例，过滤串撞名）
test result: ok. 13 passed; 0 failed

== D9 纯逻辑（副本标题） ==
test business::task::handler::tests::copy_title_appends_suffix_and_keeps_limit ... ok
test result: ok. 1 passed; 0 failed
```

卡面 §五 ①–⑩ 一一对应（⑩ 拆成三个单据各一条，多出 ④/⑩b/⑩c 三条覆盖）。

- ⑨ 用的是 `to_jsonb(t)` **整行快照**前后逐字比对（以后加列也自动纳入），另加
  「明细行 id 不许变」「`updated_at` 不许变」两条独立断言。
- ⑦ 造「空壳单」的方式是删掉原单明细（库层没有「至少一行」约束），断言 400 且**一张新单都不许建出来**。
- 标题上限用例：200 字的原任务复制后仍 ≤ 200 字且后缀完整（先砍原标题、不砍后缀）。

## 三、真机 e2e（隔离实例 18096 + `cloudpivot_d9`）：50/50 PASS

命令：`bash scripts/verify/d9_instance.sh up && D9_KEEP=1 bash scripts/verify/d9_instance.sh e2e`

```
== ① 收货单复制 ==
[PASS] 原单过账成功（含税 22.6 < 1000，免审直过） —— 200 status=POSTED
[PASS] 原单过账生成了凭证 + 应付
[PASS] 新单号 ≠ 原单号 —— IN-20261101-000019 → IN-20261101-000020
[PASS] 新单号走 biz_seq 取号（IN-yyyyMMdd-NNNNNN）
[PASS] 新单状态 = DRAFT
[PASS] 卡面决策 B：采购单关联被清空 —— source_order_id=None
[PASS] 明细行的 source_item_id 也被清空
[PASS] 凭证 / 应付关联被清空
[PASS] 明细全带出（物料 / 数量 / 单价一致） —— 1 行
== ② 复制单改数量 → 提交 → 过账 ==
[PASS] 复制单过账成功 —— 200 qty=4.0000
[PASS] 原单整行一个字节都没变（含 updated_at）
[PASS] 原单库存流水仍只有 1 条
== ③ 调拨单复制 ==
[PASS] 调拨单新号走 biz_seq —— TK-20261101-000005
[PASS] 复制单状态 = DRAFT / 明细带出 / 序列号不复制 / 审批实例被清空
[PASS] 原调拨单仍是 PENDING（复制不改原单）
== ④ 任务复制 ==
[PASS] 副本标题带「（副本）」 —— D9E2E 月度对账（副本）
[PASS] 优先级 P1 / 预估工时 3.5 / 描述带出 / 状态 TODO
[PASS] 子任务不复制 —— 0 个 ；评论不复制 —— 0 条
[PASS] 原任务的子任务还在 —— 1 个
== ⑤ 权限（复制 = 建单权限） ==
[PASS] 收货单复制：403 无权限访问，需要权限: business:erp:stock-in:create
[PASS] 调拨单复制：403 无权限访问，需要权限: business:erp:transfer:create
[PASS] 任务复制：403 无权限访问，需要权限: business:task:create
== ⑥ 边界 ==
[PASS] 原单不存在 → 404；id 非 UUID → 400（不静默当没传）
== 结果：50 通过 / 0 失败 ==
```

`d9_no_create` 是**非超管**账号（只挂 list/detail 码），所以这三条 403 是中间件真判的结果，
不是超管短路后的假象。

## 四、页面验收（vite dev 5196 → 隔离后端 18096）：13/13 PASS

命令：见 `scripts/verify/d9_page_check.mjs` 头部注释（先 `D9_KEEP=1` 跑 e2e 留夹具）。

```
[PASS] ① 收货单详情抽屉出现「复制为新单」
[PASS] ① 点复制 → 编辑弹窗打开
[PASS] ① 采购单被清空的提示条可见（文案点名要重新关联）
[PASS] ① 明细带出（第 1 行数量 = 10）
[PASS] ① 表单里没有「从采购单带出」提示（关联确实清空了）
[PASS] ① 成功提示：已复制为 IN-20261101-000025（草稿），请核对后提交
[PASS] ② 调拨单详情抽屉出现「复制为新单」
[PASS] ② 点复制 → 编辑弹窗打开
[PASS] ② 原单无序列号 → 不显示序列号提示（不是无脑弹条）
[PASS] ③ 任务详情抽屉出现「复制为新单」
[PASS] ③ 复制后抽屉切到副本，标题 = D9E2E 月度对账（副本）
[PASS] ④ 除 SSE 通知流外没有 4xx/5xx 接口
[PASS] ④ 全程没有 JS 异常（未捕获错误 / 组件报错）
[NOTE] /api/business/notice/stream 401 × 3（既有 SSE 问题，与本卡无关）
== 页面：13 通过 / 0 失败 ==
```

按钮可见性由 `v-privilege="[PERM.create]"` 控制（**没有建单权限就整块不渲染**，不是画出来置灰）；
复制中按钮 `loading + disabled`（卡面 L9：连点只建一张，后端**不查重**）。

### 已知残留（本卡未触碰、非本卡引入）

`GET /api/business/notice/stream?token=…` 用有效 admin 令牌也返回 `401 未授权访问，请先登录`
（curl 实测，见下）。它属于通知 SSE 通道，本卡 diff 里 `notice` 模块一行未改，页面每次导航都会
重连一次 → console 里 3 条 `Failed to load resource: 401`。D9 的判定口径按「除 SSE 外无 4xx/5xx +
无 JS 异常」执行，并在报告里如实记这条残留。

```bash
$ curl -s -o /tmp/sse.out -w 'sse=%{http_code}\n' -m 3 \
    "http://127.0.0.1:18096/api/business/notice/stream?token=$TOKEN"
sse=401
{"code":401,"msg":"未授权访问，请先登录"}
```

## 五、质量闸

| 闸 | 命令 | 结果 |
|----|------|------|
| 格式 | `cargo fmt -p cloudpivot -- --check` | 绿 |
| 静态检查 | `cargo clippy --lib -- -D warnings` | 绿（零告警） |
| 类型检查 | `cargo check --lib --tests` | 绿 |
| 前端类型 | `pnpm typecheck`（vue-tsc） | 绿 |
| 前端单测 | `pnpm test:unit` | **370 passed / 0 failed**（基线 364 + 本卡 6） |
| 全量后端套件 | `bash scripts/verify/d9_full_suite.sh`（干净库 `cloudpivot_d9full`） | **1771 passed / 0 failed**（589s） |
| ERP 不变量 | `bash scripts/verify/d9_invariants_clean.sh`（干净库） | **32/32 · BLOCKER 0 · WARN 0**；跑完本卡收货/任务用例仍 32/32 |

## 六、全量套件与不变量

### 全量后端套件（干净隔离库 `cloudpivot_d9full`，V0137–V0188 全量迁移）

命令：`bash scripts/verify/d9_full_suite.sh`

```
test result: ok. 1771 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 589.06s
```

基线 1758（D5 交付时）+ 本卡 13 条真库用例 = 1771。

### ERP 不变量（技能 §6.48 的两条口径）

**① 干净库（新建 + 迁移完、零夹具）→ 32/32 全绿、BLOCKER 0、WARN 0**

命令：`bash scripts/verify/d9_invariants_clean.sh`

```
== ① 干净库不变量（期望 32/32、BLOCKER 0、WARN 0）==
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。
```

**② 跑完本卡用例后再巡检 → 与本卡相关的不变量未退**

命令：`bash scripts/verify/d9_invariants_attribution.sh`（在干净库上分三步跑，做归因）

```
== 基线 ==                       结果：32/32 通过   BLOCKER 0   WARN 0
== ① 收货单 D9 用例（8 条）… 任务 D9 用例（3 条）都 ok
== ① 跑完不变量（期望 32/32）==  结果：32/32 通过   BLOCKER 0   WARN 0   ← 本卡收货/任务用例零残留
== ② 调拨 D9 用例（2 条）ok
== ② 跑完不变量 ==               结果：29/32  BLOCKER 3（B2 / B4 / E4，库存模块 1390 无总账对应）
== ③ 只跑既有调拨用例 create_returns_draft_with_card_no_format（本卡之前就有）ok
== ③ 跑完不变量（应与 ② 同形）== 结果：29/32  BLOCKER 3（同上三条，逐字相同）
```

**归因**：那 3 条是 `transfer_handler::db_tests::prepare()` 自己插入的库存基线
（`W3B-M-PLAIN` 100@12.5 + 10@8、`W3B-M-SN` 2@30 = 1390）——**只跑既有调拨用例**（步骤③，
与本卡改动无关）得到的残留与步骤②**逐字相同**，说明这是调拨测试夹具的既有特性
（它每轮重建基线、不来收拾），不是 D9 引入的。D9 的收货 / 任务用例跑完仍是 32/32。

## 七、改动清单

后端：

- `backend/src/business/erp/stockin_model.rs` — `copy_source()`（事务内 `FOR SHARE` 读原单+明细）
- `backend/src/business/erp/stockin_handler.rs` — `copy_stock_in()` + `stock_in_copy()` + `COPY_PERMISSION` + 8 条单测
- `backend/src/business/erp/transfer_model.rs` — `copy_source()`
- `backend/src/business/erp/transfer_handler.rs` — `copy_transfer()` + `transfer_copy()` + `COPY_PERMISSION` + 2 条单测
- `backend/src/business/task/handler.rs` — `copy_task()` + `task_copy()` + `copy_title()` + `ensure_task_create_permission()` + 3 条单测

前端：

- `frontend/src/api/business/erp/index.ts` — `copyStockIn` / `copyTransfer`
- `frontend/src/api/business/task-api.ts` — `copyTask`
- `frontend/src/views/business/erp/stockin/index.vue` — 详情抽屉「复制为新单」+ 复制提示条
- `frontend/src/views/business/erp/stockin/stockin-logic.ts` — `stockinCopyNotice()`
- `frontend/src/views/business/erp/transfer/index.vue` — 抽屉 footer「复制为新单」+ 序列号提示条
- `frontend/src/views/business/erp/transfer/transfer-logic.ts` — `transferCopyNotice()`
- `frontend/src/views/business/task/detail.vue` — 头部「复制为新单」+ `copied` 事件
- `frontend/src/views/business/task/index.vue` — `handleCopied()`（抽屉切到副本 + 刷新视图）
- `frontend/tests/unit/d9-document-copy.test.ts` — 6 条前端单测

脚本：

- `scripts/verify/d9_dbsetup.sh`、`d9_unit_tests.sh`、`d9_full_suite.sh`、`d9_instance.sh`、
  `d9_document_copy_e2e.py`、`d9_page_check.mjs`、`d9_invariants.sh`

## 八、零迁移的理由

复制 = 建单，所以权限直接复用各单据既有的 `:create` 码（硬红线 5）。不新增权限码 ⇒ 不需要
`sys_menu` 新节点、不需要 `V01xx` 迁移。库里三处权限码实测都存在：

```sql
select count(*) from sys_menu where auth_code in
  ('business:erp:stock-in:create','business:erp:transfer:create','business:task:create');
-- 3
```
