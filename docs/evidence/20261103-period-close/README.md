# PERIOD-CLOSE · 结账检查清单升级 —— 证据包（2026-11-03）

卡：`t_1d3a6fce` ｜ 分支 `p3b`（基线 `a64e8b8` = origin/main，含 DEPLOY-B 后的出库单 / 状态机配置）
｜ 主题：把挡路单据列成表 + 标清该谁处理 + 一键催办直达。

## 用户原话 → 落地

| 原话 | 这一版怎么答 |
|------|--------------|
| 「不光要显示出来有什么问题」 | 原有 `blockers` / `warnings` 两级结构**没动**（库存漂移不该封死所有期间） |
| 「还要把要处理的单据列出来？」 | 每条 blocker 带 `items`（≤50 条）：单号 / 类型 / 金额 / 制单人 / 状态 / 已等 |
| 「方便跳转处理」 | 每行「去处理」→ 后端算好的 `route`（凭证 `?voucher_no=`、采购 `?po_id=`、收货 `?in_id=`） |
| 「写清楚下一步需要谁处理」 | `owner_role`（按「下一个动作需要的权限码」反查角色）+ `owner_name`（该角色恰好一个启用用户时才给）；**定位不到就是「待分配」** |
| 「发消息给相关人员」 | `POST /business/erp/period/{id}/remind`（每家）→ 复用 `notice::dispatch` 发站内信；同一张单 24h 不重复催 |

## 本轮（第二次执行）做了什么

第一次执行（09:44 那个 run）在写完全部代码 + 8 张截图后**被手工回收**（生产落后 bare 35 个提交，
要先部署再继续），工作树里的改动没提交。本轮做的：

1. 把未提交的改动对齐到**最新基线** `a64e8b8`（origin/main），清掉混在工作树里的 D3-UI 卡残留文件
   （它们已在 `c1dac3 / 42f671e / a1bcaad` 里），补丁重放后重新编译、重跑单测、重跑真机。
2. **修掉一个与验收口径不符的点**：三类未完成单据（采购 / 收货 / 调拨）合并成一个 blocker，
   原来每类各取 50 条 → 明细最多 150 行，而 `message` 却写「仅显示前 50 张」。
   现在三类混排后**统一截断到 50**（`unfinished_items_are_capped_at_fifty_across_types` 锁死）。
3. 补 9 张真机截图（新增「凭证行去处理」的直达断言）与 4 份 phase JSON，全部在本轮基线的
   新二进制 + 新 dist 上重跑。

## 文件清单

### 后端

| 文件 | 行数 | 作用 |
|------|------|------|
| `backend/src/business/erp/period_items.rs` | 891 | **新**：`BlockingItem` / 四类挡路单据明细（条数 + 明细同源）/ 上限 50 条 / warnings 明细 |
| `backend/src/business/erp/period_owner.rs` | 172 | **新**：「下一个动作的权限码 → 责任角色 →（唯一）具体人」的唯一实现（`OwnerResolver`，按请求缓存） |
| `backend/src/business/erp/period_remind.rs` | 425 | **新**：催办接口（收件人服务端现算 + 24h 防重 + 幂等键） |
| `backend/src/business/erp/period.rs` | 2317 | `Blocker` 增 `items`；四类校验改用明细同源取数；`can_close()` 一行未改 |
| `backend/src/business/erp/period_test_support.rs` | 416 | **新**（`cfg(test)`）：`2033-xx` / `P2-` 前缀的公共夹具（与 `period.rs` 的 `2031-xx` 互不相干） |
| `backend/src/business/erp/period_items_tests.rs` | 535 | **新**：13 条真库用例 |
| `backend/src/business/erp/period_remind_tests.rs` | 506 | **新**：11 条真库用例 |
| `backend/migrations/business/V0207__erp_period_remind.{up,down}.sql` | — | **新**：`business:erp:period:remind` 权限节点（id `…1212`，原计划 `…1022` 已被 V0195 的「期初余额」占用） |

### 前端

| 文件 | 作用 |
|------|------|
| `frontend/src/views/business/erp/period/CloseCheckPanel.vue` | **新**：结账弹窗的检查清单面板（表格 + 每行 [去处理][催办] + 全部催办 + 提醒项） |
| `frontend/src/views/business/erp/period/period-logic.ts` | 新增纯逻辑：`ownerText` / `canRemindItem` / `waitingText` / `blockingMoneyText` / `remindToast` … |
| `frontend/src/views/business/erp/period/index.vue` | 弹窗改用面板组件；`goto`（直达单据）/ `reminded`（toast）两个出口 |
| `frontend/src/api/business/erp/index.ts` | `PeriodBlockingItem` / `PeriodRemindResult` / `remindPeriod()` |
| `frontend/tests/unit/erp-period-close-items.test.ts` | **新**：10 条纯逻辑用例（进 `pnpm test:unit` 的 543 条里） |
| `frontend/scripts/verify/period-close-ui.mjs` | **新**：真机验证脚本（4 个 phase，产物 = `shots/*.png` + JSON 结论） |

## 单测（`cargo test --lib`，真库 `cp_period_close_clean`，`--test-threads=1`）

| 证据 | 内容 |
|------|------|
| `01-unit-items.txt` | `period_items` **13 passed / 0 failed** |
| `02-unit-remind.txt` | `period_remind` **11 passed / 0 failed** |
| `03-unit-period-regression.txt` | `period`（ERP-P1 原有）**15 passed / 0 failed** |
| `06-full-lib.txt` | 全量 `cargo test --lib`（克隆库 `cp_period_close_full`，`--test-threads=1`）：**2111 passed / 0 failed / 0 ignored**（931.37s） |
| `13-frontend-typecheck.txt` | `pnpm typecheck`（vue-tsc --noEmit）**RC=0** |
| `14-frontend-build.txt` | `pnpm build`（vue-tsc + vite build）成功，dist 产物列表见文件尾部 |
| `15-frontend-unit.txt` | `pnpm test:unit` **543 pass / 0 fail**（含本卡新增 10 条 + D3-UI 11 条） |
| `16-backend-gates.txt` | `cargo check --all-targets` 0 error / 0 warning、`cargo clippy --lib --all-targets -D warnings` 通过、`cargo fmt --check` 只剩既有 `tests/testd/*.rs` 的 rustfmt 版本漂移 |
| `04-invariants-clean-db.txt` | 干净库 `cp_period_close_clean`（迁移到 V0207）：**36/36 不变量通过，0 BLOCKER / 0 WARN** |

验收口径逐条对应（全部有真库用例）：

```
□ 草稿凭证 3 张 → items 长度 3，每项 doc_no / owner_role / route 都有值  → items_draft_vouchers_list_every_doc
□ 超过 50 张 → items 只返回 50，message 含「仅显示前 50」                → items_are_capped_at_fifty_with_message_note
□ 三类合并超 50 张 → 仍只返回 50（本轮新增）                              → unfinished_items_are_capped_at_fifty_across_types
□ owner_role 反查：要求 post 权限 → 返回有该权限的角色                    → owner_role_is_resolved_from_permission
□ 定位不到 owner → owner_role = null（不瞎猜）                            → owner_role_is_null_when_no_role_holds_permission
□ owner_name 只在角色下唯一用户时给                                        → owner_name_only_when_role_has_single_user
□ 催办：有 owner 发出去、没 owner 记 skipped_no_owner                     → remind_sends_to_owner_and_counts_are_consistent / remind_skips_items_without_owner
□ 催办 24h 内重复 → 被拦                                                    → remind_dedupes_within_24_hours
□ can_close() 不受 items 影响                                               → can_close_is_unaffected_by_items
□ warnings 也带 items（口径一致）                                          → every_blocker_serializes_items_array + 真机 phase 2
□ 空明细时接口不报错                                                       → empty_period_has_no_items_and_no_error
```

> ⚠️ 共用测试库 `cloudpivot_test` 上跑 `erp_invariants.py` 会红 —— 那是几十张卡跑测试留下的夹具残留
> （B2 库存金额、B4 流水、D1 已过账无凭证…），**与本卡改动无关**（本卡只读不写账）。
> 干净库的 36/36 才是本卡的可判据证据。另外：**不要在单测跑着的时候跑不变量**
> （本轮踩过：`period_items` 用例的进行中夹具会让 A4 短暂不平，跑完再查即 36/36）。

## 真机验证（1600×1000，Chromium 5173 + 8023，基线 a64e8b8）

演示数据：`05-demo-data.sql`（专用期间 **2036-03**，避免污染 2026-10/11 里别的卡的夹具）：
1 张 DRAFT 凭证 `PC-V-203603-001`（1234.00）+ 1 张 DRAFT 收货单 `PC-IN-203603-001`（560.00）
+ 1 张 SUBMITTED 采购单 `PC-PO-203603-001`（880.00）。
「该谁处理」按真实配置反查：`business:erp:voucher:create` → `FINANCE_MANAGER / 李四`；
收货单 → `WAREHOUSE_MANAGER`（角色下多人 → 只给岗位）；采购单 → `PURCHASE_MANAGER`（同上）。

| 截图 | 对应验收项 | 读数（本轮实测） |
|------|-----------|------|
| `shots/01-close-dialog-blocked.png` | 弹窗全貌：3 行明细 | `rows=3`；`confirmDisabled=true`；3 个单号都在；有「该谁处理」列；有「去过账」「去审批」 |
| `shots/02-single-remind.png` | 单张催办 | toast「2036-03：已催办 1 张」，该行变「已催（今天）」 |
| `shots/03-remind-all.png` | 全部催办 | 按钮「全部催办（2）」→ toast「2036-03：已催办 2 张」 |
| `shots/04-goto-stock-in.png` | 去处理直达收货单 | 跳 `/business/erp/stock-in` 并自动开出 `PC-IN-203603-001` 详情 |
| `shots/04b-goto-voucher.png` | 去处理直达凭证 | 跳 `/business/erp/voucher`，列表里就是 `PC-V-203603-001` |
| `shots/05-close-dialog-cleared.png` | 三张处理完后重新预检 | `okAlert=1 / blockedAlert=0 / confirmDisabled=false`（阻断项清空、确认可点） |
| `shots/06-notice-message-center.png` | 催办真的变成站内信 | 消息中心：「2036-03 月结账待处理：PC-IN-203603-001 ｜ 赵六 的 收货单 PC-IN-203603-001（560）还是 草稿（未过账），挡住了 2036-03 的结账。请去过账。已等待 0 天。」 |
| `shots/07-closed-period.png` | 结账（原有功能不退化） | 状态变「已结账」 |
| `shots/08-reopened-period.png` | 反结账（原有功能不退化） | 填原因后状态回「已开启」 |

phase JSON：`07-ui-phase1-dialog-remind-goto.json`（6/6 step ok）、`08-ui-phase2-cleared.json`、
`09-ui-phase3-notice-center.json`、`10-ui-phase4-close-reopen.json`。

库侧取证：`11-notice-db.txt` —— 3 条 `sys_notice`（标题 / 正文 / link_url / 收件人数逐条贴出）+
24h 内再催一次的返回 `{"sent":0,"skipped_recent":1,...,"message":"2036-03：已催办 0 张，1 张24小时内已催过（不重复打扰）"}`。

复跑方式：

```bash
# 后端 8023（profile=test，库 cloudpivot_test）+ 前端 5173 起好后
psql ... -f docs/evidence/20261103-period-close/12-reset-demo.sql   # 现场复原成「三张都挡路」
cd frontend
PC_PERIOD=2036-03 node scripts/verify/period-close-ui.mjs 1   # 弹窗 / 单张催办 / 全部催办 / 去处理
psql ... -f docs/evidence/20261103-period-close/05-demo-data.sql   # 幂等；或手工把三张单处理到终态
PC_PERIOD=2036-03 node scripts/verify/period-close-ui.mjs 2   # 重新预检：阻断项清空
PC_PERIOD=2036-03 node scripts/verify/period-close-ui.mjs 3   # 消息中心（收件人视角）
PC_PERIOD=2036-03 node scripts/verify/period-close-ui.mjs 4   # 结账 → 反结账 往返
```

## 顺带修掉的老 bug（实测发现）

**结账弹窗的阻塞项列表从来没渲染过。** 老代码把列表挂在 `<t-alert>` 的 `#description` 槽上，
而 TDesign 1.19 的 Alert 只消费 `default` / `message` 两个槽
（`node_modules/tdesign-vue-next/es/alert/alert.mjs::renderDescription` 只读 `renderTNodeJSX('default') || renderTNodeJSX('message')`），
`#description` 被**静默丢弃** —— 所以此前弹窗只显示「还有 1 张草稿凭证未过账（另有 1 项）」这一句话，
列表一个字都没出来。本卡把列表放到 alert 外面（`CloseCheckPanel.vue`），并留了注释防止有人搬回去。

## 自己踩到的坑（已修，记在这里）

1. `period_remind_tests::remind_sends_to_owner_and_counts_are_consistent` 里读通知内容用的是
   `WHERE biz_type='period_remind' ORDER BY created_at LIMIT 1` —— 单跑绿、**全量跑红**：
   `period_remind` 是全库共用的 `biz_type`，真机演示留下的 2036-03 通知比本用例的更早，
   断言「标题含 2033-02」就抓到了别人的行。修法是按本用例期间的凭证 `biz_id` 收窄。
   教训：**按 biz_type 扫全表取一条**在共用库里永远是不稳定断言，必须再按自己的业务键收窄。
2. 真机脚本的 phase 3 原来带「必须先有阻断项」的前置，但演示顺序里三张单这时已经处理掉了 →
   会把「消息中心截图」卡死在超时。已改成 phase 3 不要求被挡状态（它只看收件人视角）。

## 同类问题（照实报告，未顺手改）

1. **年结 `year_end.rs`**：`YearEndBlocker` 只有 `kind / periods / count / diff / message / link / samples`，
   **没有明细、没有处理人、没有催办**；五项前置校验（期间未结 / 单据未完成 / 借贷不平 / 不变量 / 下年未开账）
   同样只报汇总。建议另开卡按本卡的 `period_items` 口径复制一份（年结的「挡路单据」就是全年 12 个月的集合）。
2. **期末结转**：本仓的期末结转 = `year_end` 的损益结转凭证（`YEAR_END`），没有独立模块，
   所以它的问题同上，不另有第三处。
3. `transfer` 调拨单的深链：`/business/erp/transfer?transfer_no=…` 目前**页面不消费**该参数
   （voucher / purchase / stock-in 三页都有 `applyRouteQuery` / `openByQuery`，transfer 没有）。
   本卡的路由已带上单号，调拨页补一段深链处理即可直达（建议随年结卡一起做）。

## 数据痕迹（可清理）

- 演示期间 `2036-03`（cloudpivot_test）：结账后又反结账，现为 `OPEN`；
  三张演示单据已到终态（POSTED / CANCELLED / CLOSED）；三条 `period_remind` 站内信保留（属沟通记录）。
- 演示用户 `pc_demo_li/zhao/sun`（李四/赵六/孙七）+ 给 `FINANCE_MANAGER` / `WAREHOUSE_MANAGER` /
  `PURCHASE_MANAGER` 勾的 4 个权限节点、admin 挂 `WAREHOUSE_MANAGER`（为了让 admin 能看到催办消息）。
  都是可删的演示数据，不影响业务表。
- 干净库 `cp_period_close_clean`（不变量用）与克隆库 `cp_period_close_full`（全量单测用）：可保留，DROP 需审批。
