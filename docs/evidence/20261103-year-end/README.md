# YEAR-END · 年结检查清单升级 —— 证据包（2026-11-03）

卡：`t_f530126c`（父卡 `t_1d3a6fce` = PERIOD-CLOSE）
｜ 主题：把年结检查清单从「一句话汇总」升级成「挡路单据明细表 + 该谁处理 + 一键催办」，
口径**逐条复用**结账那一版，不另发明一套；顺带补调拨页 `?transfer_no=` 深链。

## 用户原话 → 落地（与结账页同一套三句话）

| 原话 | 结账页（上一卡） | 年结页（本卡） |
|------|------------------|----------------|
| 「不光要显示出来有什么问题」 | `blockers[]` 一句话摘要 | 五条 ✅/❌ 保留（五项前置校验），**新增**明细表 |
| 「还要把要处理的单据列出来？」 | `blocker.items`（≤50 条） | `YearEndBlocker.items`（同结构，≤50 条） |
| 「写清楚下一步需要谁处理」 | `owner_role` / `owner_name`（权限码反查） | 同一个 `OwnerResolver`、同一张表 |
| 「方便跳转处理」 | 每行「去处理」→ 后端算好的 `route` | 同；**调拨单那行终于能直达**（本卡补的深链） |
| 「发消息给相关人员」 | `POST /period/{id}/remind` | `POST /year-end/remind`（复用同一套发送循环） |

## 本轮做了什么

### 1. 后端：年结每条 blocker 都带明细（一行 SQL 没复制）

`year_end.rs` 原来自己写了两段汇总 SQL（`unfinished_docs_of_year` / `invariant_failures_of_year`），
只给条数与最多 3 个单号。**删掉它们**，改成复用 `period_items` 的「日期区间」入口：

| 需要 | 期间结账（上一卡已有） | 年结（本卡新增的入口） |
|------|------------------------|------------------------|
| 未完成单据 | `unfinished_doc_items(ex, &PeriodRow, ..)` | `unfinished_doc_items_between(ex, 1/1, 12/31, ..)` |
| 不变量 A1/A2 | `invariant_details(ex, "2036-03", ..)`（`v.period = ?`） | `invariant_details_between(ex, 1/1, 12/31, ..)`（`voucher_date` 区间） |

实现上是**同一段 SQL + 一个 `VoucherScope` 枚举**（`Period(&str)` / `Dates(start,end)`，
只差占位符 1 个还是 2 个），所以两个入口的 where 子句不可能漂移 —— 单测
`year_and_period_invariant_items_agree` 拿同一批坏凭证让两个入口各查一次、比对单号清单。

两条纪律跟着复用：
* **明细合并后统一截断到 50 条**（不是「12 个月 × 50 = 600 行」，也不是「三类各 50 = 150 行」）；
* **处理人一律从权限码反查**，不变量类留 `null`（怎么修账是财务主管的判断，系统不指定人）。

`can_close()` **一行没改**（仍然只看 `blockers` 是否为空）—— 明细是给人看的，不参与判定。

### 2. 后端：催办抽成共用核心 + 年结只写「上下文 + 权限码」

`period_remind.rs` 把两边**真的相同的部分**抽了出来，年结那边（新文件 `year_end_remind.rs`）只剩
「上下文 + 取数 + 权限码」：

```text
RemindScope          标题前缀 / 正文口径 / 摘要前缀 / 幂等键定位 / biz_type / 幂等前缀
  ├─ period("2036-03")   → 「2036-03 月结账待处理：…」   biz_type=period_remind
  └─ year_end(2040)      → 「2040 年结账待处理：…」      biz_type=year_end_remind
dispatch_all()       逐条发送：没处理人 → skipped_no_owner；24h 内催过 → skipped_recent；
                     单条失败 → failed 继续；结果计数 + 一句人话
pick_from()          「哪一类 + 哪几张」的筛选（期间 / 年结共用一份）
```

**没有照抄一份**：`year_end_remind.rs` 全文 207 行里，发送逻辑 0 行 —— 它只是
`collect_blockers(全年)` → `pick_from` → `dispatch_all`。24h 防重按 `biz_type` 分开，
所以同一张单在「月结」和「年结」两个上下文里各能催一次（不是互相吃掉）。

### 3. 迁移：`V0208__erp_year_end_remind`（只种权限节点）

`business:erp:year-end:remind`，id `…1213`，挂「年度结转」页（`…1025`）下。
自检两件事：节点种上了 + **一码一节点**（否则前端/后端/迁移三处串不一致时，403 会很难查）。

> ⚠️ **命名差异（照卡面执行，已写进迁移注释）**：V0190 已有的三个年结码是
> `business:erp:yearend:{read,close,reopen}`（`yearend` 连写），而本卡点名要求
> `business:erp:year-end:remind`（带连字符）。三处（后端常量 / 前端常量 / 迁移）都是
> `year-end`，单测 `scope_is_year_shaped_and_permission_code_is_pinned`（后端）与
> `年结催办权限码与后端 V0208 逐字一致`（前端）各钉一层。要统一改名的话，三处一起改。

### 4. 前端：面板抽成共用组件，年结页直接上表格

* 新 `shared/BlockerCheckPanel.vue`（448 行）：表格 + 每行 `[去处理][催办]` + 全部催办 + 提醒项，
  把 `data-testid` 前缀做成 prop（`period` / `yearend`），**催办函数由调用方传入**
  （`remind: (payload) => Promise<PeriodRemindResult>`），所以面板不认接口地址；
* `period/CloseCheckPanel.vue` 从 439 行缩到 94 行 —— 变成共享面板的薄封装，
  props / 事件 / testid 全保持不变（结账页与它的真机脚本一行没改）；
* `shared/blocker-logic.ts`：单据类型中文 / 角色中文 / 金额 / 已等 / 可催办判定 / toast
  从 `period-logic.ts` **搬**过来，`period-logic.ts` 用 `export *` 再导出（老 import 不变）；
* 年结页：五条 ✅/❌ 保留（回答「五项都过了吗」），下面接明细表（只渲染**有明细**的两类：
  全年未完成单据 / 不变量坏凭证），`show-messages=false` 避免与五行重复；
* **列表仍然不挂在 `t-alert` 的 `#description` 槽上**（TDesign 1.19 会静默丢弃），这条约束跟着搬进共享组件并写明原因；
* 年结页新增 `?year=2040` 深链（预选年度，方便收藏 / 转发 / 真机脚本稳定定位）。

### 5. 顺带：调拨页消费 `?transfer_no=`（一行改动，卡面点名）

voucher / purchase / stock-in 三页早就有 `applyRouteQuery`，唯独调拨页没有 ——
年结/结账给的调拨单明细路由一直是「跳过去但看不懂」的状态。现在：
`applyRouteQuery()` → 填 `filter.transfer_no` → 查一次列表 → 命中的行 `openDetail(row)`；
参数消费后 `router.replace` 掉；查不到就说「没有找到调拨单 X（可能已删除，或没有查看权限）」。
纯逻辑 `transferNoFromQuery()`（数组取首个 / 非法类型返回空串）有单测。

## 文件清单

### 后端

| 文件 | 行数 | 变更 |
|------|------|------|
| `backend/src/business/erp/year_end.rs` | 2646 | `YearEndBlocker` 增 `items`；②④ 改明细同源（删掉自写的两段汇总 SQL） |
| `backend/src/business/erp/year_end_remind.rs` | 207 | **新**：`POST /business/erp/year-end/remind`（上下文 + 取数 + 权限，发送逻辑 0 行） |
| `backend/src/business/erp/period_items.rs` | 993 | 新增日期区间入口 `unfinished_doc_items_between` / `invariant_details_between` + `VoucherScope`（期间路径改走同一实现） |
| `backend/src/business/erp/period_remind.rs` | 562 | 抽 `RemindScope` / `title_of` / `content_of` / `idempotency_key_of` / `build_draft_in` / `summary_in` / `pick_from` / `dispatch_all`；老函数全部保留为薄封装（老单测 11 条一行没改） |
| `backend/src/business/erp/year_end_test_support.rs` | 231 | **新**（cfg(test)）：`2037/2038` + `YE-` 前缀夹具（含**用例结尾清场**，见下） |
| `backend/src/business/erp/year_end_items_tests.rs` | 409 | **新**：8 条真库用例（明细 / 50 上限 / owner null / 空明细 / can_close / 与期间口径一致） |
| `backend/src/business/erp/year_end_remind_tests.rs` | 392 | **新**：6 条（上下文词形 + 挑选 + 发送 + 24h 防重 + 无处理人 + 非法入参） |
| `backend/migrations/business/V0208__erp_year_end_remind.{up,down}.sql` | — | **新**：权限节点 `business:erp:year-end:remind`（id `…1213`） |

### 前端

| 文件 | 行数 | 变更 |
|------|------|------|
| `frontend/src/views/business/erp/shared/BlockerCheckPanel.vue` | 448 | **新**：共用检查清单面板（表格 / 去处理 / 催办 / 全部催办 / 提醒项） |
| `frontend/src/views/business/erp/shared/blocker-logic.ts` | 230 | **新**：从 `period-logic.ts` 搬来的展示逻辑 + `BlockingBlocker` / `BlockerRemindFn` 类型 |
| `frontend/src/views/business/erp/period/CloseCheckPanel.vue` | 439 → 94 | 改为共享面板的薄封装（props/事件/testid 不变） |
| `frontend/src/views/business/erp/period/period-logic.ts` | 499 → 326 | 展示逻辑搬走后 `export *` 再导出 |
| `frontend/src/views/business/erp/year-end/index.vue` | 516 → 609 | 接明细面板 + 催办 + `?year=` 深链 |
| `frontend/src/views/business/erp/year-end/year-end-logic.ts` | 234 → 291 | `remind` 权限码 / `blockersWithItems` / `yearEndItemCount` / `yearEndCheckSummary` / `yearFromQuery` |
| `frontend/src/views/business/erp/transfer/index.vue` | +33 | `applyRouteQuery()`：`?transfer_no=` 直达详情 |
| `frontend/src/views/business/erp/transfer/transfer-logic.ts` | +17 | `transferNoFromQuery()`（纯逻辑，可单测） |
| `frontend/src/api/business/erp/index.ts` | +40 | `YearEndBlocker.items` / `YearEndRemindPayload` / `remindYearEnd()` |
| `frontend/tests/unit/erp-year-end-items.test.ts` | 190 | **新**：10 条纯逻辑用例 |
| `frontend/scripts/verify/year-end-items-ui.mjs` | 274 | **新**：真机验证脚本（5 个 phase） |

## 单测（真库，`--test-threads=1`）

| 证据 | 内容 |
|------|------|
| `02-unit-year-end.txt` | `cargo test --lib business::erp::year_end` → **29 passed / 0 failed**（新增 14 条 + 原有 15 条回归） |
| `03-unit-period-regression.txt` | `cargo test --lib business::erp::period` → **39 passed / 0 failed**（period 15 + period_items 13 + period_remind 11，抽取后一行没改） |
| `06-full-lib.txt` | 全量 `cargo test --lib`（克隆库 `cp_year_end_full`，`--test-threads=1`）：**2125 passed / 0 failed / 0 ignored**（979.43s） |
| `12-frontend-typecheck.txt` | `pnpm typecheck`（vue-tsc --noEmit）**RC=0** |
| `13-frontend-build.txt` | `pnpm build` 成功 |
| `14-frontend-unit.txt` | `pnpm test:unit` **553 pass / 0 fail**（543 + 本卡 10 条） |
| `16-backend-gates.txt` | `cargo check --all-targets` 0 error / 0 warning、`cargo clippy --lib -- -D warnings` 通过、`cargo fmt --check` 只剩既有 `tests/testd/*` 5 个文件的版本漂移（本卡未触碰） |
| `04-invariants-clean-db.txt` | 干净库 `cp_year_end_clean`（迁移到 **V0208**）：**36/36 通过，0 BLOCKER / 0 WARN** |

验收对照（卡面五条，全部有真库用例）：

```text
□ 明细条数上限 50（全年 12 个月合并后仍 50）  → unfinished_items_are_capped_at_fifty_across_twelve_months（60 张 → count=60 / items=50 / 文案「共 60 张，仅显示前 50 张」）
□ owner 反查不到留 null                       → owner_is_null_when_no_role_holds_the_permission
□ owner 反查到就给（角色 + 唯一用户点名）      → items_list_every_blocking_doc_across_the_year
□ 24h 防重                                    → remind_dedupes_within_24_hours（第二次 sent=0 / skipped_recent=2）
□ can_* 判定不变                              → can_close_is_unaffected_by_items
□ 空明细不报错（items 是空数组而非缺字段）      → empty_year_has_no_items_and_no_error / every_year_end_blocker_serializes_items_array
□ 与结账页口径不分裂                          → year_and_period_invariant_items_agree
```

> **踩到的坑（已修）**：本卡夹具把 2037 全年封账来测年结，跑完留在库里 → 下一轮
> `cargo test --lib`（`period` 在前）时 `period::tests::p1_reopen_*` 两条反结账用例 409
> （它们做的是**全库**判据「比我晚的期间不能有已结账的」）。
> 修法：`year_end_test_support::teardown()` 在**每个用例结尾**也清一遍场，
> 并把原因写进夹具文件头 —— 实测 `year_end` → `period` 顺序连跑两次全绿。

## 真机验证（1600×1000，Chromium + 5173 / 8023，新二进制 + V0208）

演示数据 `05-demo-data.sql`：年度 **2040**（12 个月已结账）+ **2041**（12 个月已开账）
+ 三张挡路单据，故意覆盖三种「该谁处理」：

| 单据 | 状态 | 动作 | 「该谁处理」实测 |
|------|------|------|------------------|
| `PC-YE-PO-2040-001` | SUBMITTED | 去审批 | `PURCHASE_MANAGER`（角色下 2 人 → 只给岗位） |
| `PC-YE-IN-2040-001` | DRAFT | 去过账 | `WAREHOUSE_MANAGER`（3 人 → 只给岗位） |
| `PC-YE-TR-2040-001` | PENDING | 去审批 | **无角色持有 `transfer:status` → 待分配**（催办按钮置灰） |

| 截图 | 对应验收项 | 读数（脚本断言，来自真实 DOM） |
|------|-----------|------------------------------|
| `shots/01-year-end-items-table.png` | 年结检查清单的明细表 | `rows=3`；三个单号都在；有「该谁处理」列；出现「待分配」；有「去过账」「去审批」；底部「共 N 张挡路单据」 |
| `shots/02-year-end-remind-one.png` | 单张催办 | toast「2040 年度：已催办 1 张」，该行变「已催（今天）」 |
| `shots/03-year-end-remind-all.png` | 全部催办 | 按钮「全部催办（1）」→ toast「2040 年度：已催办 1 张」（另 1 张待分配、1 张刚催过） |
| `shots/04-goto-transfer-detail.png` | 去处理（调拨单） | 跳 `/business/erp/transfer` 并自动开出 `PC-YE-TR-2040-001` 详情 |
| `shots/05-year-end-cleared.png` | 清空后 | `itemTables=0`；`✅ 无未完成单据`；「年度结转」按钮 `disabled=false` |
| `shots/06-year-end-notice-center.png` | 站内信 | 消息中心出现「2040 年结账待处理：PC-YE-IN-2040-001」+ 全段正文 |
| `shots/07-transfer-deeplink.png` | 调拨页深链（本卡补的） | 直接打开 `?transfer_no=PC-YE-TR-2040-001` → 详情抽屉 `drawer=1`，URL 参数已被消费 |
| `shots/08-year-end-closed.png` | 年结（原有功能不退化） | 状态变「2040 年度已结转」 |
| `shots/09-year-end-reopened.png` | 反年结（原有功能不退化） | 填原因后状态回「2040 年度未结转」 |

phase JSON：`07-ui-phase1-items-remind-goto.json`（4/4 step ok）、`08-ui-phase2-cleared.json`、
`09-ui-phase3-notice-center.json`、`10-ui-phase4-transfer-deeplink.json`、`15-ui-phase5-close-reopen.json`。

库侧取证 `11-notice-db.txt`（标题 / 正文 / link_url / 收件人逐条）+ 三条接口返回值：

```json
// 全部催办：2 张有处理人 → 发出去；1 张待分配 → 跳过
{"sent":2,"failed":0,"skipped_no_owner":1,"skipped_recent":0,"total":3,
 "message":"2040 年度：已催办 2 张，1 张未指定处理人"}
// 24h 内再催一次
{"sent":0,"failed":0,"skipped_no_owner":1,"skipped_recent":2,"total":3,
 "message":"2040 年度：已催办 0 张，2 张24小时内已催过（不重复打扰），1 张未指定处理人"}
// 只催「待分配」的那张
{"sent":0,"failed":0,"skipped_no_owner":1,"skipped_recent":0,"total":1,
 "message":"2040 年度：已催办 0 张，1 张未指定处理人"}
```

`01-api-check-2040.json` 是 `GET /business/erp/year-end/check?year=2040` 的原始响应
（三张明细逐字段：`doc_no / status_label / amount / created_by_name / owner_role /
action_label / route / waiting_since`，调拨单那行**没有** `owner_role`）。

### 回归：结账弹窗换成共用面板后没坏（`18-period-panel-regression.*`）

`CloseCheckPanel.vue` 从 439 行缩成 94 行的薄封装，**这是本卡对老功能的唯一风险点**，
所以单独验一次：期间页按年度筛选到 2041 → 开 `2041-01` 的结账弹窗（库里有 1 张挡路采购单）
→ 表格照旧渲染：

```json
{"step":"period-close-panel","ok":true,"rows":1,"remindButtons":1,
 "hasOwnerColumn":true,"hasAction":true,"hasDocNo":true}
```

截图 `shots/18-period-panel-regression.png`。上一卡那份 `period-close-ui.mjs` **一行没改**
（它按期间编码直接点按钮，本卡往库里加了 2040/2041 期间会让它落在第二页，
所以用这个独立探针补验；探针脚本不入库）。

## 复跑方式

```bash
# 后端 8023（profile=test，库 cloudpivot_test，新二进制）+ 前端 5173 起好后
export PGPASSWORD=alun
psql -h 127.0.0.1 -U alun -d cloudpivot_test -f docs/evidence/20261103-year-end/05-demo-data.sql  # 复原成「三张都挡路」
cd frontend
node scripts/verify/year-end-items-ui.mjs 1   # 明细表 / 单张催办 / 全部催办 / 去处理（调拨单）
node scripts/verify/year-end-items-ui.mjs 3   # 消息中心
node scripts/verify/year-end-items-ui.mjs 4   # 调拨页 ?transfer_no= 深链
psql -h 127.0.0.1 -U alun -d cloudpivot_test -f ../docs/evidence/20261103-year-end/06-clear-demo-docs.sql
node scripts/verify/year-end-items-ui.mjs 2   # 清空后重新预检
node scripts/verify/year-end-items-ui.mjs 5   # 年结 → 反年结
```

单测：

```bash
cd backend
export ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cp_year_end_clean'
cargo test --lib business::erp::year_end -- --test-threads=1     # 29 passed
cargo test --lib business::erp::period   -- --test-threads=1     # 39 passed
```

## 已知边界（照实说）

1. **共用库 `cloudpivot_test` 不适合跑不变量**：几十张卡留下的夹具会让 `erp_invariants.py` 变红
   （本卡只读不写账，与改动无关）。可判据的证据是干净库 `cp_year_end_clean` 的 36/36。
   另外这台机器上多个 agent 共写 `cloudpivot_test`：本轮验证中途观察到上一卡留的 `2036-03`
   演示期间被别的会话删掉、并冒出 2021–2023 的夹具期间（与本卡无关，本卡只增删自己
   `PC-YE-2040-*` / `2040-2041` 那一份数据）。真机截图与 phase JSON 都是**当场**抓的，
   不受后续变动影响。
2. **本卡没做部署**：V0208 与新二进制只在本地 test 实例验证过（`cloudpivot_test` 已应用 V0208），
   生产 144 未触碰 —— 上线需另开部署卡（V0207 + V0208 与两个提交一起发）。
3. **权限码命名不一致是卡面要求**：`year-end:remind` vs 已有的 `yearend:{read,close,reopen}`，
   见上文「迁移」一节；三处已对齐并有单测钉住。
4. **年结页的明细表只覆盖两类**（未完成单据 / 不变量）：另外三类（期间未结 / 借贷不平 /
   下年未开账）没有「某一张单据」能负责，继续用一句话 + 五项 ✅/❌ 表达，不硬造空表。
5. **夹具年度 2037/2038 必须「用完即清」**：`period::tests` 的反结账判据是**全库**的
   「比我晚的期间不能有已结账的」，所以 `year_end_test_support::teardown()` 在每个用例
   结尾也清场（踩过一次：连跑 `year_end` → `period` 时后者红 2 条，已修并复跑全绿）。
