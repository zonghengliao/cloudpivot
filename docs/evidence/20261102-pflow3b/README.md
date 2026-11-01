# P-FLOW-3B 财务链两条标准审批流 · 验收证据

> 卡面：`t_50b3bdc5` —「应付作废 + 凭证红冲审批（财务链两条标准流，14 单测）」
> 日期：2026-11-02 · 执行：backend-developer
> 代码：`5d9e278`（origin/main）· 迁移：`V0191__pflow3b_finance_cancel_reverse_approval`

## 一、交付内容

### 1.1 两条标准流程（开箱即用，阈值与审批人都在**流程定义**里）

| flow_code | 名称 | biz_type | 图 | 审批人 |
|---|---|---|---|---|
| `payable_cancel` | 应付单作废审批（标准） | `PAYABLE_CANCEL` | START → 财务经理审批 → END（3 节点 2 边） | `ROLE = FINANCE_MANAGER` |
| `voucher_reverse` | 凭证红冲审批（标准） | `VOUCHER_REVERSE` | START → 条件 `form.amount > 10000` →（是）财务经理+财务总监**会签** /（否）财务经理 → END（5 节点 5 边） | `ROLE = FINANCE_MANAGER / FINANCE_DIRECTOR` |

两个节点都是 `empty_assignee = FALLBACK_ADMIN`（角色里没人 → 回退超管，**不是静默跳过**）
+ `self_approval = ALLOW`（待办照常可见留痕，真正的否决在内控闸 → 403 + 65019）。

新增角色种子：`FINANCE_MANAGER`（财务经理）、`FINANCE_DIRECTOR`（财务总监）。

`graph_hash`（由 `graph.rs::snapshot()` 的同一套 canonical JSON 生成，迁移里带自检）：

```
payable_cancel  : 4f484777debbea1a0cc1a76db28274c4d9986c8cbe8d3936afa7e08a50adf7fc
voucher_reverse : 742f06d8fbecc6eb2fdfdfaed66571bfff9c1b3b02ff8dd0338f0dc7f3dd17b2
```

### 1.2 状态机改动（L1 / L2）

| 单据 | 改动 |
|---|---|
| `biz_payable` | 补 `PENDING_CANCEL`；`PAYABLE_EDGES` 4 → 7 条（`OPEN→PENDING_CANCEL` / `PENDING_CANCEL→CANCELLED` / `PENDING_CANCEL→OPEN`）；`PAYABLE_REASON_EDGES` 3 条；`PAYABLE_LOCKED` 加 `PENDING_CANCEL`（审批中锁单） |
| `biz_voucher` | **不加状态**（硬红线 3，理由见 1.3）；`VOUCHER_EDGES` 保持 0 条 |

库侧硬约束（不靠代码自觉）：

```sql
ck_biz_payable_status         status IN (…, 'PENDING_CANCEL')
ck_biz_payable_pending_cancel (PENDING_CANCEL ⟺ cancel_instance_id 有值) OR deleted_at
ck_biz_voucher_pending_reverse pending 只许挂在 POSTED 且 is_red=0 且 reversed_by IS NULL 的凭证上
```

> **为什么 `PARTIAL_PAID / SETTLED` 不许作废**：边表里**没有**这两条边，入口再核一次
> `paid_amount = 0`。作废一张已付过钱的应付单必须同时冲回已付款 —— 那是「付款冲销 + 红冲凭证」
> 的活，不是作废单据能表达的（卡面决策，双保险）。

### 1.3 关键取舍：凭证红冲**不改状态**（卡面硬红线 3）

红冲语义是「生成红字凭证、原凭证保持 `POSTED`」。**不能**给凭证加 `PENDING_REVERSE`，因为
`ledger.rs` / `report.rs` / `income_statement.rs` / `cash_flow.rs` 的取数全是
`WHERE v.status = 'POSTED' AND v.deleted_at IS NULL` —— 原凭证一旦改成「审批中」，
这张**已入账**的凭证会在总账 / 余额表 / 三大报表里**同时消失**。

落地口径：

```
送审：status 仍 POSTED，只挂 pending_reverse_instance_id + reverse_reason
通过：清 pending → voucher::reverse_posted（借贷互换 + 独立号段 PZR）→ 回写 reversed_by
驳回：只清 pending（账一分没动）
```

`ck_biz_voucher_pending_reverse` 保证「pending 非空 ⟹ 未红冲」，`reverse_after_approval` 里
**先清 pending 再红冲**（顺序反了会撞该约束）。

### 1.4 接口（唯一入口，老 URL 也走审批 —— 换 URL 绕不过去）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| POST | `/business/erp/payable/{id}/cancel` | `business:erp:payable:cancel` | 老入口，**改走审批**（未配流程才直接作废） |
| POST | `/business/erp/payable/{id}/submit-cancel` | `business:erp:payable:submit-cancel` | 显式送审（与上面同一函数） |
| POST | `/business/erp/payable/{id}/withdraw-cancel` | `business:erp:payable:withdraw-cancel` | **反向**：发起人撤回 → 回 `OPEN` |
| POST | `/business/erp/voucher/{id}/reverse` | `business:erp:voucher:reverse` | 老入口，**改走审批** |
| POST | `/business/erp/voucher/{id}/submit-reverse` | `business:erp:voucher:submit-reverse` | 显式送审 |
| POST | `/business/erp/voucher/{id}/withdraw-reverse` | `business:erp:voucher:withdraw-reverse` | **反向**：只清 pending |

出参 `outcome`：`PENDING_APPROVAL`（已送审）/ `CANCELLED`（未配流程直接作废）/
`REVERSED`（未配流程直接红冲），详情字段 `#[serde(flatten)]` 平铺 —— 老前端读
`status` / `payable_no` 不受影响。

### 1.5 回调 / 内控 / 反向（L4 / L6 / L7 / L10 / L11）

| 环节 | 落点 |
|---|---|
| 回调（APPROVED） | `payable_bridge::on_cancel_settled` → 真作废（进项凭证红冲 + 欠款消失 + 摘实例引用）；`voucher_bridge::on_reverse_settled` → 清 pending + 生成红字 + 回写 `reversed_by` |
| 回调（REJECTED / WITHDRAWN / CANCELLED） | 应付 → 回 `OPEN`；凭证 → 只清 pending |
| 注册点 | `flow::handler::{approve_task, reject_task, withdraw_instance}` 三处（与 P16/P17/P-FLOW-3A 并列、互不影响） |
| 内控（L10） | ①生成待办前排除制单人/发起人（`flow::internal_control`，落 `SKIPPED` 留痕）；②审批入口 `guard_self_approval` 403 + `65019`（防转办/加签/老数据绕过）——两层都在 |
| 反向（L4/L6） | 发起人撤回（两个 withdraw 接口 + 审批中心撤回）；收货单作废联动 → 同事务终止在跑的作废/红冲审批；两个**孤儿实例回收**定时任务（每 10 分钟，`payable_cancel_reconcile` / `voucher_reverse_reconcile`） |
| 同事务（L11） | 状态机 + `engine::start` + CAS 回写（状态/实例/原因）同一笔事务 |

## 二、验收结果

### 2.1 本卡真库单测（19 条，`pflow3b_`）

```bash
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_3b \
  cargo test --lib pflow3b_ -- --test-threads=1
# → test result: ok. 19 passed; 0 failed
```

对照卡面 14 条（多出 5 条为种子自检 / 反向撤回 / 夹具清场）：

| 卡面 | 用例 | 断言要点 |
|---|---|---|
| ① | `pflow3b_payable_submit_cancel_enters_pending` | `PENDING_CANCEL` + `cancel_instance_id` + `cancel_reason` + **欠款一分没动** + `biz_status_log` 1 条 + 制单人待办落 `SKIPPED` |
| ② | `pflow3b_payable_approve_cancels` | `CANCELLED` + 摘实例引用 + 原因进 `remark` + 金额列不动 |
| ③ | `pflow3b_payable_reject_restores_open` | 回 `OPEN` + 可**再次提交**（证明没留孤儿实例/引用） |
| ④ | `pflow3b_payable_partial_paid_rejected` | 409 + 状态不变 + **不产生实例** |
| ⑤ | `pflow3b_payable_settled_rejected` | 409 + 状态不变 + 不产生实例 |
| ⑥ | `pflow3b_payable_self_approval_forbidden` | 待办派给别人（制单人被排除）+ 制单人自批 403 + `65019`，他人放行 |
| ⑦ | `pflow3b_payable_delete_cancels_approval` | 软删 → `reconcile_orphan_payable_cancels` → 实例 `CANCELLED` + 0 条 PENDING 待办 |
| ⑦b | `pflow3b_payable_withdraw_restores_open` | 撤回 → `OPEN` + 实例 `WITHDRAWN` |
| ⑧ | `pflow3b_voucher_submit_reverse_flags_pending_only` | pending 有值 + **status 仍 POSTED** + 未红冲 + 待办 1 条（≤ 1 万单人） |
| ⑨ | `pflow3b_voucher_approve_generates_red` | 红字 `is_red=1` + `reverse_of` 指向原凭证 + **借贷互换** + `reversed_by` 回写 + pending 清空 |
| ⑩ | `pflow3b_voucher_reject_clears_pending` | pending 清空 + 无红字 + 可再次提交 |
| ⑪ | `pflow3b_voucher_already_reversed_rejected` | 409「已经红冲」 |
| ⑫ | `pflow3b_voucher_large_amount_requires_countersign` | 2 万 → 2 条待办；一票通过仍 `PENDING` 且不红冲；两票齐才红冲 |
| ⑬ | `pflow3b_voucher_self_approval_forbidden` | 403 + `65019` |
| ⑭ | `pflow3b_voucher_delete_cancels_approval` | 软删 → 孤儿回收 → 实例 `CANCELLED` + 待办清零 |
| ⑭b | `pflow3b_voucher_withdraw_clears_pending` | 撤回 → pending 清空 + 无红字 + 实例 `WITHDRAWN` |
| ⑮ | `pflow3b_seeded_flows_hash_matches_rows` | **`graph::load_graph` 从行重建后重算 hash = 迁移里写死的值**（两条流都核） |
| ⑯ | `pflow3b_seed_threshold_is_on_edge` | 1 万阈值真的落在**条件边**上（`form.amount` / `gt` / `10000`） |
| ⑰ | `pflow3b_zz_teardown_purges_fixtures` | 收尾清场：P3B 科目 / 供应商 / 用户 / 实例 / 待办 5 项残留断言为 0（防污染兄弟模块用例，见 §2.5） |

### 2.2 真机 e2e（真 HTTP + 真库 + 真种子流程）

```bash
python3 scripts/verify/pflow3b_finance_approval_e2e.py     # P3B_BASE_URL=http://127.0.0.1:18096
# → PASS 42 / FAIL 0
```

覆盖卡面 §六 四条真机链路 + 三条硬红线：

| 组 | 断言（节选） |
|---|---|
| ① 应付作废 13 项 | 收货过账自动生成应付 → `submit-cancel` → `PENDING_CANCEL` + 欠款未动 + 重复点 409 → 财务经理待办 → 批 → `CANCELLED` + 原因进 remark |
| ①b 反向 3 项 | 提交后 `withdraw-cancel` → 回 `OPEN` |
| ②③ 凭证红冲 12 项 | 送审后 `status` 仍 POSTED、pending 有值 → 待办 1 条 → 批 → `reversed_by` 指向红字、红字 `reverse_of` 指向原凭证、**借贷互换** → 已红冲再提交 409 |
| ④ 会签 6 项 | 2 万 → FM + FD 各 1 条待办；一票通过仍不红冲；两票齐才红冲 |
| ⑤ 内控 5 项 | 发起人在**生成待办时**被排除（自己收不到待办）→ 待办派给财务经理 → 发起人自批 **403 + 65019** → 撤回后 pending 清空 |

### 2.3 干净库 ERP 不变量（32 条）

```bash
NIK_DATABASE_NAME=cloudpivot_p3b_inv python3 scripts/verify/erp_invariants.py
# → 结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
```

### 2.4 全量 lib 套件（干净库，`--test-threads=1`）

```bash
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_p3b cargo test --lib -j 2 -- --test-threads=1
# → test result: ok. 1867 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 640.82s
```

1867 = 既有 1848 + 本卡 19。**全绿，含 transfer / stockin / purchase 全部审批类用例。**

### 2.5 过程中查出并修掉的真问题：本卡夹具污染共享主数据

第一版全量跑出来是 `1835 passed / 10 failed`，10 条全在 `transfer_handler::db_tests`。
三步归因：

| 对照 | 结果 |
|---|---|
| 干净库单独跑 `transfer_handler::db_tests` | 50/50 绿 |
| 干净库先跑本卡 18 条、再跑这 50 条 | 50/50 绿（**注意：这时还没暴露**） |
| 同一份代码 `cargo test --lib -- --skip pflow3b_` | 1848/0 全绿 |
| 干净库先跑本卡、再跑整个 `business::erp::` | 揪到真凶：`cash_flow::tests::r3_real_config_covers_all_accounts` 报 **「分类配置漏科目：P3B.1002 / P3B.2202」** |

**根因**：本卡夹具用 `P3B.1002 / P3B.2202` 两个自建科目 + `P3B-SUP` 供应商，
但清场只删了单据/实例/用户，**没删科目和供应商**。全量套件是同一个库串行跑，
本模块排在 `cash_flow` / `transfer_handler` 之前，留下的科目让「按科目全集断言」的用例
（分类配置覆盖、账簿平衡）连带变红 —— 也是 D7 交接里「同一个库连跑第二遍会红」的同源问题。

**修法**（本卡内已修，提交 `fix(test): P-FLOW-3B 夹具清场补科目/供应商`）：

1. 清场 SQL 抽出 `purge_fixtures()`，补 `biz_opening_balance → biz_account(P3B.%) → biz_supplier(P3B-SUP)`；
2. 每个用例开头都调（入口干净）+ 新增 **⑰ `pflow3b_zz_teardown_purges_fixtures`**
   （名字排在模块最后）收尾清场，并断言 5 类残留全为 0；
3. 复跑：本卡 19/19 绿、全量 **1867/0** 绿。

> 另有一条与本卡无关的存量问题已单开卡：`transfer_handler` 用例自身「同库二次运行残留
> + 唯一索引冲突被翻成 500（`服务器内部错误`）」——见子卡「【FIX】transfer 真库用例同库二次运行残留」。

### 2.6 静态检查

```bash
cargo fmt -- --check                    # FMT_OK
cargo clippy --lib -- -D warnings       # 0 warning / 0 error
```

`cargo clippy --all-targets -- -D warnings` 仍有报错，但**全部是存量**：
`tests/testd/*.rs`（集成测试）与 `internal_control.rs` 的测试段（P0-FIX 提交 `4d4efdf` 引入的
`useless_vec`），本卡 diff 一行未触及。

## 三、生产部署（172.18.10.144）

```bash
bash /tmp/p3b_deploy_144.sh
```

| 项 | 结果 |
|---|---|
| 代码 | 首发 `5d9e278`；补齐测试/证据后复发 `7559286`（ff-only 合并，与 origin/main 一致） |
| 迁移 | `数据库迁移完成: 应用 1 个迁移` + `NOTICE: V0191 自检通过：应付作废 + 凭证红冲两条标准流程已就绪` |
| 二进制 | 首发 `029aa2d20bac836dbecfdc54f10d4bc8`；补齐测试/证据后复发 `2075b13c4ca28ee90594df7c423929f3`（`PROC_EQ_DISK=OK`，主进程 md5 == 磁盘 md5） |
| 健康 | `/health` → 200 |
| 结构自检 | 迁移 191=1 · 应付新增列 2 · 凭证新增列 2 · 两个 pending 约束各 1 · 角色 `FINANCE_MANAGER,FINANCE_DIRECTOR` · 两条流 `PAYABLE_CANCEL/VOUCHER_REVERSE` 均 PUBLISHED · 节点数 `payable_cancel:3 / voucher_reverse:5` · 4 个权限码 |
| 路由冒烟 | 4 条新路由未登录全部 **401**（已注册，不是 404） |
| 生产不变量 | **32/32 通过 · BLOCKER 0 · WARN 0** |

回滚：`git revert 5d9e278` + 执行 `V0191…down.sql`（down 里先清 `PENDING_CANCEL` 行与 pending 引用，
再删约束/列/流程/角色/菜单）。

## 四、已知边界 / 后续

1. **前端未在本卡范围**：`/cancel` `/reverse` 的响应新增 `outcome` / `message` /
   `approval_instance_id`（详情字段已 flatten，老页面不会报错），但页面上还看不到
   「作废审批中 / 红冲审批中」的徽标与「撤回」按钮 —— 建议前端补：应付列表状态徽标加
   `PENDING_CANCEL`（`status_label` 后端已给「作废审批中」）、凭证详情展示 pending 标记与
   「撤回红冲审批」按钮、审批中禁用「作废/红冲」按钮。
2. **作废审批不做金额分级**：卡面只要求财务经理单人复核；要分级就在流程定义里加条件边
   （本卡已在 `cancel_form_data` 里冻结了 `form.amount`，加节点不用改代码）。
3. **凭证红冲审批的「已红冲」判断在读侧**：并发两个红冲审批会被
   `uk_biz_flow_instance_biz_active` + `pending_reverse_instance_id` CAS 双闸挡住（单测 ⑪ 覆盖）。
