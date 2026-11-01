# ERP-P1 会计期间开账 / 结账 / 反结账 —— 交付与验收留痕

> 卡：`t_436ef985`｜负责人：backend-developer｜日期：2026-11-01
>
> **老板的问题**：「会计期间为什么不能添加新的期间？」
>
> **答案（改之前）**：后端只有 `GET /period/page` 与 `GET /period/{id}` 两个只读接口，
> 前端页头写着「只读：开账 / 结账由财务主管操作」，但**那个入口从来没做**。
> 设计意图（防误操作）是对的，缺的是执行者要用的功能。
>
> **答案（改之后）**：财务主管在「会计期间」页点「开账」就能建当月期间，能批量开一整年，
> 能结账（先预检、点名阻塞项）、能反结账（必填原因 ≥ 10 字）。生产上已经把 **2026-11
> 开出来了**（当月，见 §7）。

---

## 1. 交付清单

| 层 | 文件 | 内容 |
|---|---|---|
| 迁移 | `backend/migrations/business/V0188__erp_period_close.{up,down}.sql` | 三个权限节点（menu 1019/1020/1021，挂在「会计期间」…0951 下）+ 授权 + 自检块 |
| 后端 | `backend/src/business/erp/period.rs`（新增，约 2.3k 行含单测） | 开账 / 批量开账 / 结账预检 / 结账 / 反结账 + 15 条单测 |
| 后端 | `backend/src/business/erp/voucher.rs` | `post_voucher` 加「期间已结账 → 400」唯一收口（自动 / 手工 / 付款凭证全走它） |
| 后端 | `backend/src/business/erp/voucher_write.rs` | 手工凭证入口对 `CLOSED` 单独返 400（与 `post_voucher` 同文案），`NOT_OPENED` 仍 409 |
| 后端 | `backend/src/business/shared/state_machine.rs` | 注册 `Entity::Period`：边表 `NOT_OPENED→OPEN→CLOSED→OPEN`，反结账边强制 reason |
| 后端 | `backend/src/business/erp/mod.rs` | `pub mod period;` |
| 前端 | `frontend/src/api/business/erp/index.ts` | 5 个接口封装 + 类型（`OpenPeriodPayload` / `BatchOpenPeriodResult` / `PeriodCloseCheck` / `PeriodCloseBlocker`） |
| 前端 | `frontend/src/views/business/erp/period/index.vue` | 只读 → 可操作：顶部开账 / 批量开账，行内按状态出结账 / 反结账，按钮按权限码 `v-if` |
| 前端 | `frontend/src/views/business/erp/period/period-logic.ts` | 纯逻辑（`periodActionOf` / `canRunPeriodAction` / `reopenReasonError` / `closeCheckSummary` / `batchOpenError`） |
| 前端 | `frontend/tests/unit/erp-period-actions.test.ts`（新增） | 7 条纯逻辑单测 |
| 验收 | `scripts/verify/erp1_period_prod_e2e.py`（新增） | 生产真机 25 项（自清理 + 基线核对） |
| 验收 | `scripts/verify/erp1_period_page_check.mjs`（新增） | 生产页面 6 项（Playwright，只读：列表 / 按钮 / 动作 / 预检弹窗 / disabled / console） |
| 文档 | `docs/evidence/20261101-erpp1/README.md` | 本文件 |

提交：`1272d85`（功能 + 迁移 + 15 单测 + 前端）、`f61275c`（真机与全量套件暴露的两处修复）、
`docs(evidence)` 留痕提交（本文件 + 页面验收脚本）。

---

## 2. 接口（前缀 `/api` 由 `[router].prefix` 注入）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| POST | `/business/erp/period` | `business:erp:period:open` | 开账：不存在则建（`start_date`=当月 1 日、`end_date`=当月最后一天、`status=OPEN`）；已存在且 `NOT_OPENED` → 置 `OPEN`；已 `OPEN` / `CLOSED` → 409；与已有期间日期区间重叠 → 409 |
| POST | `/business/erp/period/batch-open` | `business:erp:period:open` | 批量开账（`from`/`to` = `YYYY-MM`，含两端）：已存在**跳过**（幂等，重复跑不 409），一次 ≤ 120 个月 |
| GET | `/business/erp/period/{id}/close-check` | `business:erp:period:close` | 结账预检（只读）：`{can_close, period, blockers[], warnings[]}` |
| POST | `/business/erp/period/{id}/close` | `business:erp:period:close` | 结账（`reason` 可选，≤200 字，落 `remark` 并进状态日志） |
| POST | `/business/erp/period/{id}/reopen` | `business:erp:period:reopen` | 反结账（`reason` **必填 ≥ 10 字**） |

**权限码三个分开**（卡面硬红线 4）：能看列表（`:list`，V0141 已有）≠ 能封账（`:close`）≠ 能撤封账（`:reopen`）。
`V0188` 授权：open/close → `SUPER_ADMIN / ADMIN / FINANCE / ACCOUNTANT`；reopen → `SUPER_ADMIN / FINANCE`（财务总监）。
无权限时前端**不渲染按钮**（`v-if`，不是 `disabled`）。

### 状态机（写 `biz_status_log`）

```
NULL → OPEN      开账（log_created）
NOT_OPENED → OPEN 开账（把已存在但未开启的期间置为 OPEN）
OPEN → CLOSED     结账（reason 可选）
CLOSED → OPEN     反结账（reason 必填，进日志）
```

`entity_type = 'ACCOUNTING_PERIOD'`、`entity_no = period_code`（如 `2026-11`），
走 `state_machine` 唯一入口 —— 与五张单据共用同一套「边表校验 + reason 校验 + 落日志」。

---

## 3. 结账四项校验（同事务 + 行锁 + CAS）

```text
BEGIN
  SELECT … FROM biz_accounting_period WHERE id = ? FOR UPDATE     ← 锁期间行
  ① 期间内 DRAFT 凭证        → 409「还有 N 张草稿凭证未过账」（附最多 3 个凭证号）
  ② 期间内单据未终态        → 409「还有 N 张单据未完成（采购单 5 张 / 收货单 2 张 …）」
  ③ 试算平衡（分录 Σ借 ≠ Σ贷）→ 409「借贷不平，差 X 元」
  ④ 期间内不变量 A1/A2      → 409「不变量未通过（N 项）：A1 有 2 张凭证分录取和不平（PZ-…）」
  state_machine::transition(OPEN → CLOSED, reason)
  UPDATE … SET status='CLOSED', closed_at=now(), closed_by=? WHERE id=? AND status='OPEN'   ← CAS
COMMIT
```

* **校验与状态变更同一个事务**（卡面 L11）：`FOR UPDATE` 行锁 + `WHERE status='OPEN'` CAS，
  并发两次结账只有一次能过，第二次拿到已 `CLOSED` 的行直接 409；
* **③ 为什么用分录求和而不看单头**：`ck_biz_voucher_balance` 已强制 `total_debit = total_credit`，
  看单头永远「平」；分录侧没有「Σ分录 = 单头」的约束，只有按分录求和才抓得到「分录被改过」的账；
* **④ 的口径分工（本卡的设计判断，见 §8 偏差 1）**：
  * `blockers` = **期间内**的账务问题（A1 每张凭证借贷自洽、A2 单头=分录取和）→ **拦结账**；
  * `warnings` = **全库时点**的账实 / 账账巡检（B4 流水累计≠库存、C1/C2 负库存）→ **只提示不拦**，
    理由：它们不按期间切，任何一处库存漂移都会封死所有期间的结账（含过账事务进行中的瞬时中间态）；
    整本账的巡检仍归 `scripts/verify/erp_invariants.py`（C1/C2 在那里是 BLOCKER）。

### 结账后的三道闸

| 动作 | 结果 |
|---|---|
| 在该期间生成凭证（手工 / 自动 / 付款） | **400**「会计期间 X 已结账，不能再生成凭证」—— 闸在 `voucher::post_voucher`（唯一落库收口），手工凭证入口另外提前拦一次同文案 |
| 该期间的单据过账 | **400**（过账要生成凭证，撞同一条闸，整笔事务回滚） |
| 红冲到已结账期间 | **409**（`voucher::reverse_posted` 的既有前置闸，比 400 更严格；既有单测 `p7_reverse_closed_period_is_conflict` 钉死，本卡不改） |

### 反结账

* `status` 必须 `CLOSED`（否则 409）；
* `reason` 必填且 **≥ 10 字**（否则 400，文案点名当前字数）；
* **后续期间不得有已结账的**（否则 409 点名，避免「前一个月开着、后一个月封着」的窟窿）；
* 动作：`status='OPEN'`、`closed_at / closed_by` 清空、状态日志带原因。

---

## 4. 后端单测 15 条（真库 `ERP_DB_URL`，缺省 SKIP）

```bash
export ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_p1v'
cargo test --lib business::erp::period -- --test-threads=1
```

```
running 15 tests
test p1_open_period_computes_month_range ... ok              ①  开 2031-02：起止日 02-01~02-28，闰年 2032-02 是 02-29，写一条 NULL>OPEN 日志
test p1_open_same_period_twice_is_conflict ... ok            ②  重复开账 → 409「已经开账」，库里仍一行
test p1_batch_open_creates_fourteen_periods ... ok           ③  批量开 14 个月 = 14 个期间；重跑 0 新建 / 14 跳过（幂等）
test p1_open_without_permission_is_forbidden ... ok          ④  无 period:open → 403 且点名权限码；三个码在 sys_menu 都有节点
test p1_close_empty_period_succeeds ... ok                   ⑤  空期间结账成功：CLOSED + closed_at + closed_by + 两条日志
test p1_close_blocked_by_draft_vouchers ... ok               ⑥  3 张草稿凭证 → 409「还有 3 张草稿凭证未过账」+ samples 3 个
test p1_close_blocked_by_unbalanced_entries ... ok           ⑦  分录 Σ借 60 / Σ贷 40 → UNBALANCED diff=20 + 409「借贷不平」
test p1_close_blocked_by_unfinished_documents ... ok         ⑧  DRAFT 采购单 → 409「还有 1 张单据未完成（采购单 1 张）」
test p1_post_voucher_to_closed_period_is_bad_request ... ok  ⑨  结账后 post_voucher → InvalidValue（接口层 400），凭证零落库
test p1_reopen_clears_closed_marks ... ok                    ⑩  反结账 → OPEN、closed_at/closed_by 清空、日志三条、原因入日志
test p1_reopen_requires_reason ... ok                        ⑪  不给原因 / 只写「太短」→ 400，期间仍 CLOSED
test p1_reopen_restores_posting ... ok                       ⑫  反结账后同一期间可继续生成凭证（period=2031-09, POSTED）
test p1_reopen_blocked_by_later_closed_period ... ok         ⑬  后续 2031-10 已结账 → 409 点名，前一个月不能反结
test p1_manual_voucher_on_closed_period_is_bad_request ... ok ⑮（真机 e2e 逼出来的）手工凭证入口 → 400「已结账」
test p1_input_guards_are_strict ... ok                       ⑭  YYYY-MM 严格解析 / 月份年份边界 / 试算平衡容差 0.0001
test result: ok. 15 passed; 0 failed
```

> 夹具约定：期间编码一律 `2031-xx` / `2032-xx`，**绝不结账 2026-10**（生产在用的期间，
> 别的模块的用例正往里记凭证）；清理按前缀删期间 → 凭证 → 状态日志，串行执行。

---

## 5. 前端

* 顶部：`[+ 开账]`（选年月，起止日实时预览）、`[批量开账]`（`YYYY-MM` 起止，≤120 月）；
* 行内按状态出动作：`NOT_OPENED → 开账`、`OPEN → 结账`、`CLOSED → 反结账`；
* **按钮按权限码 `v-if`**（不是 `disabled`）：无码时整条动作不出现，行内只有「—」；
* 结账两步：先 `GET /close-check`，把「还有 3 张草稿凭证未过账」「借贷不平，差 20 元」
  连同「去处理」链接摆出来，`can_close=false` 时确认按钮 disabled；全绿才允许确认；
* 反结账弹窗必填原因（前端先按 ≥10 字拦，文案与后端逐字一致）；
* 全库巡检告警（`warnings`）灰字提示「另有 N 项全库巡检提醒（不拦结账）」。

```bash
pnpm typecheck     # vue-tsc --noEmit 绿
pnpm test:unit     # 350 passed / 0 failed（含本卡新增 7 条 erp-period-actions.test.ts）
```

新增前端单测覆盖：权限码逐字一致、状态→动作映射、**无权限时不出按钮**、
反结账原因字数（含边界 10 字 / 前后空格 / emoji）、结账备注上限、
预检摘要、批量开账入参（格式 / 顺序 / 120 月上限）。

---

## 6. 生产部署（172.18.10.144）

```
代码：git pull → f61275c（origin/main 一致）
迁移：V0188__erp_period_close（应用 1 个迁移）
日志：NOTICE: V0188：期间三码已种，授权新增 3 行
      NOTICE: V0188 自检通过：开账/结账/反结账 3 个权限节点
      INFO   数据库迁移完成: 应用 1 个迁移
health：200
二进制：/opt/cloudpivot/backend/target/release/cloudpivot  md5 dd37c1416f98d8190bc15a437ac80e96
进程==磁盘：md5(/proc/<MainPID>/exe) = dd37c141… = md5(磁盘)      ← 部署脚本第 6 步核对
旧二进制备份：/root/cloudpivot.bak.<时间戳>（每次部署留一份）
前端 dist：入口 chunk BasicLayout-B5BiL3e0.js → BasicLayout-DH-WQFgr.js
           新包特征逐个 grep 命中：btn-period-open / btn-period-batch-open /
           btn-period-close- / btn-period-reopen- / period-close-dialog /
           period-reopen-dialog / period-close-check-blocked（各 1 个文件）
           首页 HTTP 200；旧 dist 备份 dist_backup_<时间戳>
```

> 部署踩坑（下次直接照做）：把验收脚本 scp 到 144 覆盖了**已跟踪**文件，
> `git merge --ff-only` 会因「local changes would be overwritten」中止 ——
> 先 `git checkout -- <脚本>` 再 merge（内容本来就是同一份，md5 已核）。

---

## 7. 生产真机 e2e（`scripts/verify/erp1_period_prod_e2e.py`，25 项，两轮全绿）

```bash
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
python3 scripts/verify/erp1_period_prod_e2e.py
```

关键输出（节选）：

```
基线期间行：['2026-10:OPEN']
[PASS] ① 开账 2027-01 → 200 / 起止日正确 / 状态 OPEN
[PASS] ① 期间列表出现 2027-01
[PASS] ① 开账写一条 NULL>OPEN 状态日志
[PASS] ② 期间内建手工凭证 → 200 / 期间 2027-01 / POSTED
[PASS] ③ 预检发现 1 张草稿凭证（close-check）
[PASS] ③ 有草稿凭证 → 结账 409 + 「还有 1 张草稿凭证未过账：结账前必须全部过账或作废」
[PASS] ③ 结账失败后期间仍是 OPEN
[PASS] ④ 草稿清掉后预检全绿（can_close=true）
[PASS] ④ 结账 → 200 / CLOSED / closed_at 落库
[PASS] ⑤ 结账后建凭证 → 400「会计期间 2027-01 已结账，不能再生成凭证…」
[PASS] ⑥ 反结账原因太短 → 400「至少 10 个字（当前 2 字）」
[PASS] ⑥ 反结账不给原因 → 400「必须填写原因（reason）」
[PASS] ⑥ 反结账 → 200 / OPEN / closed_at 清空
[PASS] ⑥ 状态日志三条（NULL>OPEN / OPEN>CLOSED / CLOSED>OPEN）
[PASS] ⑥ 反结账原因进了状态日志
[PASS] ⑦ 反结账后建凭证 → 200 / 期间 2027-01
[PASS] ⑧ 重复开账同一期间 → 409「已经开账」
[PASS] ⑨ 批量开账 2027-02 ~ 2027-03 → 新建 2 个
[PASS] ⑨ 批量开账重跑幂等 → 新建 0 / 跳过 2
[PASS] ⑩ 无开账权限 → 403（「无权限访问，需要权限: business:erp:period:open」）
[PASS] ⑩ 无权限用户已建（无角色）
[PASS] ⑪ 收尾：期间行回到基线（只剩 2026-10:OPEN）
[PASS] ⑪ 收尾：没有残留凭证 / 没有残留测试用户
合计 25 项：PASS 25 / FAIL 0
```

脚本特性：只用自己开的 `2027-01 ~ 2027-03`（生产原本没有），跑完连同凭证 / 状态日志删除
并**核对基线**；「无权限 → 403」用新建的**无角色账号**验（权限快照为空），验完删号。

### 顺手补开 2026-11（回答老板那句「为什么不能加新期间」）

```bash
POST /api/business/erp/period {"period_year":2026,"period_month":11,"remark":"ERP-P1 上线后补开当月期间"}
→ 200 {"period_code":"2026-11","start_date":"2026-11-01","end_date":"2026-11-30","status":"OPEN"}

biz_accounting_period: 2026-10|OPEN|2026-10-01|2026-10-31|
                       2026-11|OPEN|2026-11-01|2026-11-30|ERP-P1 上线后补开当月期间
biz_status_log:        NULL|OPEN|admin|ERP-P1 上线后补开当月期间
```

这两行是**生产数据变更**（期间行 + 状态日志），不是代码：2026-11 之前没有期间行，
库里却有 2 张记到 2026-11 的凭证（fail-open 归期），开账后归期才有着落。

---

## 7.5 生产页面验收（`scripts/verify/erp1_period_page_check.mjs`，6 项全绿）

```bash
cd /opt/cloudpivot/frontend && node ../scripts/verify/erp1_period_page_check.mjs
```

```
[PASS] 期间列表出现 2026-10 / 2026-11（2026-11 / 2026-10）
[PASS] 顶部「开账」「批量开账」按钮存在
[PASS] 2026-10（OPEN）有「结账」且无「反结账」
[PASS] 结账预检点名阻塞项：还有 8 张单据未完成（采购单 5 张 / 收货单 2 张 / 调拨单 1 张）…
[PASS] 有阻塞项时「确认结账」是 disabled（结不掉）
[PASS] 无 console error / 未捕获异常
合计 6 项：PASS 6 / FAIL 0
```

脚本**只读**：只打开结账弹窗看预检（`GET close-check`），全程不点「确认结账 / 确认反结账」。
这 8 张未完成单据是生产真实数据（2026-10 的 5 张 DRAFT 采购单 + 2 张 DRAFT 收货单 + 1 张 PENDING 调拨），
正好证明「结账拦住 + 说清几张」在真库上成立。

---

## 8. ERP 不变量：31/32 → **32/32 · BLOCKER 0 · WARN 0**

```bash
ssh 144 'set -a; . /etc/cloudpivot/cloudpivot.env; set +a; python3 scripts/verify/erp_invariants.py'
```

| 时点 | 结果 | 说明 |
|---|---|---|
| 部署前（卡前基线） | 31/32 通过，BLOCKER 0，WARN 1 | `[期间] F2 两张凭证期间无记录` |
| 部署后、补开 2026-11 之前 | 31/32 通过，BLOCKER 0，WARN 1 | 与本卡改动无关（本卡没动数据） |
| **补开 2026-11 之后** | **32/32 通过，BLOCKER 0，WARN 0** | F2 转绿：那 2 张凭证的期间有了记录 |

---

## 9. 全量后端测试

```bash
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_p1v' \
  cargo test --lib business::erp -- --test-threads=1
```

| 轮次 | 结果 | 备注 |
|---|---|---|
| 第一轮（refactor 前） | 506 passed / 7 failed | 6 条是本卡 period 用例（被全库 B4 漂移误伤）+ 1 条既有 `cash_flow::r3_real_config` |
| **第二轮（refactor 后）** | **513 passed / 1 failed** | 唯一失败是既有的 `cash_flow::tests::r3_real_config_covers_all_accounts`（跨轮同库污染：库里留着别的用例的夹具科目，它们没有现金流分类行）；**本卡 15 条全绿** |

`cargo fmt -p cloudpivot -- --check` / `cargo clippy --lib -- -D warnings` / `cargo check` 全绿。
`cargo clippy --all-targets` 的告警全部在 `tests/testd/*`（仓内既有，与本卡无关）。

---

## 10. 偏差与设计判断（逐条说明）

1. **卡面「期间内不变量全绿（A/B/C 组）」→ 本卡拆成「A 拦 + B/C 提示」**。
   依据：B4/C1/C2 是全库时点数，不是「这个月」的账；让它们拦结账会变成
   「任何一处库存漂移封死所有期间的结账」，且在途事务的瞬时中间态也会被当成账错。
   实测证据：全量套件并行跑时，别的模块的夹具让 B4 出现 5 组漂移 → 本卡 6 条用例被误判 409。
   A1/A2（逐张凭证自洽）仍然是硬阻断项；整本账巡检仍由 `erp_invariants.py` 每天跑。
2. **红冲到已结账期间返 409，不是卡面写的 400**。既有实现（`voucher::reverse_posted`）
   与既有单测 `p7_reverse_closed_period_is_conflict` 都钉在 409，本卡不去松动别人的断言；
   400 留给「生成凭证 / 单据过账」（卡面 §一 ② 的两条）。
3. **手工凭证入口额外提前拦一次 400**：真机第一轮发现它返的是 409
   「手工凭证只能记到已开启期间」，与卡面 400 不符；现在 `CLOSED` 走 400（与 `post_voucher` 同文案），
   `NOT_OPENED` / 期间不存在仍是 409（那是「还没开账」，语义不同）。
4. **开账支持「把已存在的 NOT_OPENED 期间置为 OPEN」**：卡面只写「创建期间」，
   但库里可能有手工插入 / 迁移种下的 `NOT_OPENED` 行，前端行内「开账」按钮需要有落点。
5. **批量开账幂等（已存在的跳过，不 409）**：卡面没说重复跑怎么办；
   一年开一次是常态操作，第二次跑报 409 会让「点两下」变成错误。

## 11. 已知限制（后续建议）

* 期间行**不可删除**（`biz_accounting_period` 无 `deleted_at`）—— 开错月份只能结账 / 反结账，
  不能撤销。若要「删期间」，需要新的产品决策（会影响凭证归期的唯一性）。
* 结账不做**期末结转**（损益类科目结转到本年利润）：卡面未要求，属会计期末处理，建议单独开卡。
* 反结账只能从最后一个封账的月份往回撤（业务上正确），因此「跳月反结」必须先反结后续期间。
* `warnings` 里的 B4/C1/C2 提示目前只在结账弹窗出现；如需常驻告警条，建议接库存巡检页。
