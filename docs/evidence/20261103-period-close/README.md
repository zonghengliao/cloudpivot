# PERIOD-CLOSE · 结账检查清单升级 —— 证据包（2026-11-03）

卡：`t_1d3a6fce` ｜ 分支 `p3b` ｜ 主题：把挡路单据列成表 + 标清该谁处理 + 一键催办直达。

## 用户原话 → 落地

| 原话 | 这一版怎么答 |
|------|--------------|
| 「不光要显示出来有什么问题」 | 原有 `blockers` / `warnings` 两级结构**没动**（库存漂移不该封死所有期间） |
| 「还要把要处理的单据列出来？」 | 每条 blocker 带 `items`（≤50 条）：单号 / 类型 / 金额 / 制单人 / 状态 / 已等 |
| 「方便跳转处理」 | 每行「去处理」→ 后端算好的 `route`（凭证 `?voucher_no=`、采购 `?po_id=`、收货 `?in_id=`） |
| 「写清楚下一步需要谁处理」 | `owner_role`（按「下一个动作需要的权限码」反查角色）+ `owner_name`（该角色恰好一个启用用户时才给）；**定位不到就是「待分配」** |
| 「发消息给相关人员」 | `POST /business/erp/period/{id}/remind`（每家）→ 复用 `notice::dispatch` 发站内信；同一张单 24h 不重复催 |

## 文件清单

### 后端

| 文件 | 作用 |
|------|------|
| `backend/src/business/erp/period_items.rs` | **新**：`BlockingItem` / 四类挡路单据明细（条数 + 明细同源）/ 上限 50 条 / warnings 明细 |
| `backend/src/business/erp/period_owner.rs` | **新**：「下一个动作的权限码 → 责任角色 →（唯一）具体人」的唯一实现（`OwnerResolver`） |
| `backend/src/business/erp/period_remind.rs` | **新**：催办接口（收件人服务端现算 + 24h 防重 + 幂等键） |
| `backend/src/business/erp/period.rs` | `Blocker` 增 `items`；四类校验改用明细同源取数；`can_close()` 一行未改 |
| `backend/src/business/erp/period_test_support.rs` | **新**（`cfg(test)`）：`2033-xx` / `P2-` 前缀的公共夹具（与 `period.rs` 的 `2031-xx` 互不相干） |
| `backend/src/business/erp/period_items_tests.rs` / `period_remind_tests.rs` | **新**：12 + 11 条用例 |
| `backend/migrations/business/V0207__erp_period_remind.{up,down}.sql` | **新**：`business:erp:period:remind` 权限节点（id `…1212`，原计划 `…1022` 已被 V0195 的「期初余额」占用） |

体量：`period_items.rs` 878 行 / `period_owner.rs` 172 行 / `period_remind.rs` 425 行 /
两个测试文件各 ~500 行（都 < 1000 行，与 TD 系列的文件纪律一致）。

### 前端

| 文件 | 作用 |
|------|------|
| `frontend/src/views/business/erp/period/CloseCheckPanel.vue` | **新**：结账弹窗的检查清单面板（表格 + 每行 [去处理][催办] + 全部催办 + 提醒项） |
| `frontend/src/views/business/erp/period/period-logic.ts` | 新增纯逻辑：`ownerText` / `canRemindItem` / `waitingText` / `blockingMoneyText` / `remindToast` … |
| `frontend/src/views/business/erp/period/index.vue` | 弹窗改用面板组件；`goto`（直达单据）/ `reminded`（toast）两个出口 |
| `frontend/src/api/business/erp/index.ts` | `PeriodBlockingItem` / `PeriodRemindResult` / `remindPeriod()` |
| `frontend/scripts/verify/period-close-ui.mjs` | **新**：真机验证脚本（4 个 phase，产物 = `shots/*.png` + JSON 结论） |

## 单测

| 证据 | 内容 |
|------|------|
| `01-unit-items.txt` | `period_items` **12 passed / 0 failed** |
| `02-unit-remind.txt` | `period_remind` **11 passed / 0 failed** |
| `03-unit-period-regression.txt` | `period`（ERP-P1 原有 15 条）**15 passed / 0 failed** |
| `06-full-lib.txt` | 全量 `cargo test --lib`（见文件尾部 `test result`） |
| `04-invariants-clean-db.txt` | 干净库 `cp_period_close_clean`（迁移到 V0207）：**36/36 不变量通过，0 BLOCKER** |

> ⚠️ 共用测试库 `cloudpivot_test` 上跑 `erp_invariants.py` 会红（23/36，7 BLOCKER）——
> 那是几十张卡跑测试留下的夹具残留（B2 库存金额、B4 流水、D1 已过账无凭证、E4 作废未回滚…），
> **与本卡改动无关**（本卡只读不写账）。干净库的 36/36 才是本卡的可判据证据。

## 真机验证（1600×1000，Chromium）

演示数据：`05-demo-data.sql`（专用期间 **2036-03**，避免污染 2026-10/11 里别的卡的夹具）：
1 张 DRAFT 凭证 `PC-V-203603-001`（1234.00）+ 1 张 DRAFT 收货单 `PC-IN-203603-001`（560.00）
+ 1 张 SUBMITTED 采购单 `PC-PO-203603-001`（880.00）。
「该谁处理」按真实配置反查：`business:erp:voucher:create` → `FINANCE_MANAGER / 李四`。

| 截图 | 对应验收项 | 读数 |
|------|-----------|------|
| `shots/01-close-dialog-blocked.png` | 弹窗全貌：3 行明细 | `rows=3`；`confirmDisabled=true`；含 3 个单号；有「该谁处理」列；有「去过账」「去审批」 |
| `shots/02-single-remind.png` | 单张催办 | toast「2036-03：已催办 1 张」，该行变「已催（今天）」 |
| `shots/03-remind-all.png` | 全部催办 | 按钮「全部催办（2）」→ toast「2036-03：已催办 2 张」 |
| `shots/04-goto-stock-in.png` | 去处理直达单据 | 跳到 `/business/erp/stock-in` 并自动开出 `PC-IN-203603-001` 详情 |
| `shots/05-close-dialog-cleared.png` | 三张处理完后重新预检 | `okAlert=1 / blockedAlert=0 / confirmDisabled=false`（阻断项清空、确认可点） |
| `shots/06-notice-message-center.png` | 催办真的变成站内信 | 消息中心：「2036-03 月结账待处理：PC-IN-203603-001 ｜ 赵六 的 收货单 PC-IN-203603-001（560）还是 草稿（未过账），挡住了 2036-03 的结账。请去过账。已等待 0 天。」 |
| `shots/07-closed-period.png` | 结账（原有功能不退化） | 状态变「已结账」 |
| `shots/08-reopened-period.png` | 反结账（原有功能不退化） | 填原因后状态回「已开启」 |

复跑方式：

```bash
# 后端 8023 + 前端 5173 起好后
cd frontend
PC_PERIOD=2036-03 node scripts/verify/period-close-ui.mjs 1   # 弹窗 / 单张催办 / 全部催办 / 去处理
PGPASSWORD=… psql … -f 05-demo-data.sql                        # 或手工把三张单处理到终态
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

`period_remind_tests::remind_sends_to_owner_and_counts_are_consistent` 里读通知内容用的是
`WHERE biz_type='period_remind' ORDER BY created_at LIMIT 1` —— 单跑绿、**全量跑红**：
`period_remind` 是全库共用的 `biz_type`，真机演示留下的 2036-03 通知比本用例的更早，
断言「标题含 2033-02」就抓到了别人的行。修法是按本用例期间的凭证 `biz_id` 收窄。
教训：**按 biz_type 扫全表取一条**在共用库里永远是不稳定断言，必须再按自己的业务键收窄。

## 同类问题（照实报告，未顺手改）

1. **年结 `year_end.rs`**：`YearEndBlocker` 只有 `kind / periods / count / diff / message / link / samples`，
   **没有明细、没有处理人、没有催办**；五项前置校验（期间未结 / 单据未完成 / 借贷不平 / 不变量 / 下年未开账）
   同样只报汇总。建议另开卡按本卡的 `period_items` 口径复制一份（年结的「挡路单据」就是全年 12 个月的集合）。
2. **期末结转**：本仓的期末结转 = `year_end` 的损益结转凭证（`YEAR_END`），没有独立模块，
   所以它的问题同上，不另有第三处。
3. `transfer` 调拨单的深链：`/business/erp/transfer?transfer_no=…` 目前**页面不消费**该参数
   （voucher / purchase / stock-in 三页都有 `applyRouteQuery`，transfer 没有）。
   本卡的路由已带上单号，调拨页补一段 `applyRouteQuery` 即可直达（一行改动，建议随年结卡一起做）。

## 数据痕迹（可清理）

- 演示期间 `2036-03`（cloudpivot_test）：结账后又反结账，现为 `OPEN`；
  三张演示单据已到终态（POSTED / CANCELLED / CLOSED）；三条 `period_remind` 站内信保留（属沟通记录）。
- 演示用户 `pc_demo_li/zhao/sun`（李四/赵六/孙七）+ 给 `FINANCE_MANAGER` / `WAREHOUSE_MANAGER` /
  `PURCHASE_MANAGER` 勾的 4 个权限节点、admin 挂 `WAREHOUSE_MANAGER`（为了让 admin 能看到催办消息）。
  都是可删的演示数据，不影响业务表。
- 干净库 `cp_period_close_clean`：建库跑迁移验证不变量用，可保留（DROP 需审批）。
