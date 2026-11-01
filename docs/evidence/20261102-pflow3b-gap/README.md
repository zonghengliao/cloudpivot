# P-FLOW-3B 补漏 · 后端 4 处 REST 契约缺口 · 验收证据

> 卡面：`t_a7fde8f7` —「VoucherRes 下发 pending_reverse_instance_id/reverse_reason + 4 个新权限码按老码矩阵补齐 + payment_capability 拦 PENDING_CANCEL」
> 父卡：`t_c10da409`（P-FLOW-3B 前端配套，`d341027`）· 前序后端卡：`t_50b3bdc5`（`0828a76`）
> 日期：2026-11-02 · 执行：backend-developer
> 迁移：`V0192__pflow3b_grant_new_codes_by_old_matrix`

## 一、交付内容（按卡面 ①②③④）

| # | 缺口 | 落点 | 处置 |
|---|---|---|---|
| ① | `VoucherRes` 未下发红冲审批字段 | `voucher_query.rs`：`HEADER_COLUMNS` 补 2 列、查询行 `VoucherRow` 补 2 字段、`VoucherRes` + `from_row` 补 2 字段 | 列表 / 详情（`VoucherDetailRes` flatten）/ `ReverseSubmitRes`（同 flatten）**三处同时生效** |
| ② | 4 个新权限码只授超管 | 新迁移 `V0192`（up/down） | 按 V0158 老码矩阵补 4 码 × 5 角色；再用「数据驱动」覆盖自定义角色；自检 20 行 + 反向不变量 |
| ③ | `payment_capability` 未拦 `PENDING_CANCEL` | `payable_payment.rs::payment_capability` 新增 ② 分支（判定表从 5 行 → 6 行） | `can_pay=false`、`default_amount=0`、`blocked_reason` 带「正在作废审批中（FLW-…）」；审批单号由 `payment_info` 用现成的 `instance_no_of` 查好传进纯函数 |
| ④ | `PayableRes` 未下发作废审批字段 | `payable_query.rs`：查询行 `PayableRow` 与 `PayableRes` 各补 `cancel_instance_id` / `cancel_reason` | 列表 / 详情都下发（`LIST_COLUMNS` 本来就查了这两列，只是没映射） |

### 1.1 为什么这三处必须由后端补（不是前端偷懒）

- ① 前端只能拿审批引擎兜底：每次凭证列表加载多发一个
  `GET /business/flow/instance/page?status=PENDING&biz_type=VOUCHER_REVERSE`，而且
  **没有 `business:flow:instance:read` 的账号看不到「红冲审批中」徽标**（实例页的数据可见性会过滤）
  —— 这是**可见性缺陷**，不是性能问题。
- ③ 其它消费方（报表 / 批量 / 将来的移动端）没有前端 `canStartPay` 那道闸：它们只看
  `can_pay`，而作废审批中的单账上还没作废（`unpaid_amount` 是原值）→ 判成「能付」，
  真点下去被 `validate_payment_target` 409。
- ④ 前端只能在**送审那一刻**用响应里的 `approval_instance_id` 显示进度，重开详情抽屉后进度区消失。

### 1.2 ③ 判定表的最终口径（6 行）

| # | 条件 | `can_pay` | `blocked_reason` |
|---|---|---|---|
| ① | `status = CANCELLED` | false | 已作废 |
| ② | `status = PENDING_CANCEL` | false | **正在作废审批中（FLW-…），请等审批结束或先撤回** |
| ③ | `status = SETTLED` | false | 已结清 |
| ④ | `unpaid_amount <= 0` | false | 未付金额为 0 |
| ⑤ | 有一笔 `PENDING` 付款 | false | 该笔在审批中 |
| ⑥ | 其余（`OPEN` / `PARTIAL_PAID`） | true | — |

② 放在 ③④⑤ 之前：作废审批中是最有信息量的原因（它同时解释「为什么金额明明还有却付不了」）。
`default_amount` 仍沿用「`can_pay` 才给未付金额」的口径，所以 ② 命中时是 `0`。

### 1.3 ② 的迁移口径（为什么不是「只补 5 个内置角色」）

```sql
① 5 个内置角色（V0158 老码矩阵）：SUPER_ADMIN / ADMIN / TEST_UADMIN / FINANCE / ACCOUNTANT
   → 4 个新码各授 1 行（标准库 4 × 5 = 20 行）
② 数据驱动：**任何持有老码的角色**（business:erp:payable:cancel 或 business:erp:voucher:reverse）
   一律补 4 个新码 —— 覆盖客户自建角色 / 改组后的角色，让「能发起 ⇒ 能撤回」恒成立
```

自检块（失败即迁移失败 → 启动失败）：

1. 4 个权限节点必须都在；
2. `sys_role` 为空（空库首启）→ `RAISE NOTICE` 跳过核对（与 V0158/V0191 同口径，否则空库永远起不来）；
3. 逐角色：5 个老码角色每个必须 **恰好 4** 个新码；
4. 汇总：`新码授权行数 = 4 × 在场角色数`，且 `在场角色 = 5` 时必须 **正好 20 行**；
5. 反向不变量：**持有老码却缺新码的角色数必须为 0**（两条链各查一次）。

> **实测发现（与卡面描述有出入，如实记录）**：144 prod 库 `sys_role` 里**没有**
> `ADMIN / TEST_UADMIN / FINANCE / ACCOUNTANT` 四个角色（只有 `SUPER_ADMIN` + 审批角色
> `FINANCE_MANAGER/FINANCE_DIRECTOR` + `MUTEX_A/B` + `QA_*` + 部门/采购/仓库角色），
> 且老码在 prod 也只授给了 `SUPER_ADMIN`。所以**在今天的 prod 上这条迁移是「零变更」**
> （新码仍只有超管持有）——不是没生效，而是「老码矩阵里的角色本来就不存在」。
> 影响：prod 上「撤回」对非超管仍然不可用，但这不是产品决定，而是**角色没建**。
> 若产品要求财务角色能自助撤回，正确做法是建 `FINANCE`/`ACCOUNTANT` 角色并重放本迁移
> （幂等，会自动补 8 行）；前端按权限隐藏撤回入口的兜底因此仍然必要。已在本卡评论里同步前端。

## 二、验证

### 2.1 静态门禁

```bash
cd backend
cargo fmt -- --check                 # FMT_CHECK_EXIT=0
cargo clippy --lib -- -D warnings    # CLIPPY_EXIT=0（0 warning / 0 error）
```

### 2.2 单测（新增 6 条，全部纯内存，不需要 DB）

| 用例 | 钉住什么 |
|---|---|
| `voucher_query::tests::pflow3b_res_carries_pending_reverse_instance_id` | ① 行→DTO 透出 + **JSON 键恒在**（`null` 也要有；缺席会让前端兜底继续发请求） |
| `voucher_query::tests::pflow3b_res_carries_reverse_reason` | ① 原因透出 + 空值时为 `null` 而不是缺键 |
| `payable_query::tests::pflow3b_res_carries_cancel_instance_and_reason` | ④ 详情/列表两字段 + JSON 键恒在 |
| `payable_payment::tests::pflow3b_pending_cancel_blocks_payment_with_instance_no` | ③ 分支返回 `false`、文案含「正在作废审批中 / 应付单号 / 审批单号 / 撤回」、缺单号也不漏成「可付」、与「付款审批中」同时成立时②优先 |
| `payable_payment::tests::pflow3b_pending_cancel_default_amount_is_zero` | ③ `default_amount=0`、`unpaid_amount` 照实下发 |
| `d5_capability_table_without_db`（改签名） | 既有 6 行判定表语义不变 |

### 2.3 真机 e2e（真 HTTP + 真库 + 真种子流程）

```bash
P3B_BASE_URL=http://127.0.0.1:18097 P3B_DB_NAME=cloudpivot_p3bgap \
P3B_DB_USER=e2erole P3B_DB_PASSWORD=*** \
  python3 scripts/verify/pflow3b_gap_e2e.py
# → PASS 39 / FAIL 0
```

脚本 `scripts/verify/pflow3b_gap_e2e.py` 复用父卡 `pflow3b_finance_approval_e2e.py` 的
登录/夹具/清理助手（真收货过账 → 自动应付；真手工凭证），逐条打三大缺口：

| 组 | 断言（39 条） |
|---|---|
| ③ 对照 | OPEN 应付单 `can_pay=true` / `blocked_reason=null` / `default_amount=未付`（证明闸是新增而不是恒 false） |
| ④ | 送审后 `/payable/{id}` 与 `/payable/page` 都带 `cancel_instance_id`（== 库值）+ `cancel_reason` |
| ③ | 作废审批中 `/payment-info`：`can_pay=false`、`default_amount=0`、`blocked_reason` 含「正在作废审批中」+ `FLW-…` + 应付单号；`unpaid_amount` 仍 > 0 |
| ③/④ 收口 | 撤回 → `can_pay` 回 `true`、`cancel_instance_id` 回 `null`（键仍在） |
| ① | 送审 `ReverseSubmitRes` / `/voucher/{id}` / `/voucher/page` 三处都带 `pending_reverse_instance_id`（== 库值）+ `reverse_reason`，且 `status` 仍 `POSTED` |
| ① 收口 | 撤回 → `pending_reverse_instance_id` 回 `null`、`reversed_by` 仍空 |
| 清场 | 凭证 / 应付 / 审批实例残留全为 0 |

> 实测踩到的一个坑（已写进脚本注释）：夹具里若**没有真实审批人**（FINANCE_MANAGER 角色 0 人），
> 审批节点会回退超管、而发起人（admin）被内控闸排除 → 节点无人可派 → 实例**立即 APPROVED**，
> 撤回断言全部 409「审批单已结束」。脚本因此先 `ensure_approver` 建两个审批账号，让审批停在 PENDING。

### 2.4 V0192 迁移验收（本地隔离库 `cloudpivot_p3bgap`）

| 步 | 断言 | 结果 |
|---|---|---|
| 前置 | 建齐 V0158 矩阵 5 角色 + 老码 10 行，清掉非超管新码（回到 V0191 形态：4 行） | 5 角色 / 老码 10 / 新码 4 |
| ① | 重放 up → 补齐 **4 × 5 = 20 行**，逐角色各 4 | `NOTICE: V0192 自检通过：4 个新权限码 × 5 个老码角色 = 20 行` |
| ② | 幂等：再跑一次 | `INSERT 0 0`，仍 20 行 |
| ③ | 数据驱动：只持老码的自定义角色（`P3BGAP_CUSTOM`） | 补前 0 → 补后 4，总 24 行 |
| ④ | **自检有牙**：删掉 FINANCE 的 `withdraw-reverse` 一行，只跑自检块 | `ERROR: V0192 自检失败：角色 FINANCE 只有 3/4 个新权限码` |
| ⑤ | 恢复 → down → up 往返 | down 后 4 行（只剩超管）+ NOTICE；再 up 回 24 行 |
| ⑥ | 收尾 | 夹具角色 / 老码 / 新码残留均回到基线（4 行超管） |

空库首启路径单独验过：全新库启动时 `sys_role` 为空 → `NOTICE` 跳过核对，迁移照常成功
（`schema_migrations` 记 192），bootstrap/seeds 建好角色后重放即补齐。

### 2.5 全量 lib 套件（干净隔离库，`--test-threads=1`）

```bash
ERP_DB_URL=postgres://e2erole:***@127.0.0.1:5432/cloudpivot_p3bgap2 \
  cargo test --lib -j 2 -- --test-threads=1
# → test result: ok. 1872 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 532.34s
```

1872 = 父卡基线 1867 + 本卡新增 5（① 2 条 + ④ 1 条 + ③ 2 条）。库是**全新**的
`cloudpivot_p3bgap2`（`max(version::int)=192`，`biz_voucher=0` 行），不是复用上一轮跑过的库。

> **首轮是红的，原因是夹具污染主数据（已修，原始输出留在 `raw/05b-*`）**：
> 第一次跑出 `1871 passed; 1 failed`，唯一红点
> `business::erp::cash_flow::tests::r3_real_config_covers_all_accounts`。
> 根因：本卡的真机 e2e 夹具建了 `P3BE2E.1002 / P3BE2E.2202` 两个科目（真收货过账/手工凭证要用），
> 而父卡 `pflow3b_finance_approval_e2e.py::cleanup()` **不删科目/单位/供应商/仓库/物料** ——
> 「按全部科目断言分类配置覆盖」的用例因此在全量套件里变红（父卡 P-FLOW-3B 踩过同一个坑）。
> 修法：`pflow3b_gap_e2e.py` 收尾时按外键链清主数据（`biz_opening_balance → biz_account →
> biz_material → biz_warehouse → biz_supplier → biz_unit`），并把「残留全为 0」落成一条断言；
> 换全新库复跑 → **1872/0 全绿**。

### 2.6 生产不变量（干净库，32 条）

```bash
NIK_DATABASE_NAME=cloudpivot_p3bgap_inv python3 scripts/verify/erp_invariants.py
# → 结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
```

## 三、生产部署（172.18.10.144）

（见 §四 原始输出）

## 四、原始输出

（见同目录 `raw/` 下的日志与 §五 命令清单）

## 五、复跑命令

```bash
# 1) 静态门禁（在干净 clone 里）
cd /root/p3bgap-src/backend && export PATH=/root/.cargo/bin:$PATH \
  && export CARGO_TARGET_DIR=/root/d7-target \
  && cargo fmt -- --check && cargo clippy --lib -- -D warnings

# 2) 单测（干净隔离库，先起一次实例把迁移跑完）
ERP_DB_URL=postgres://e2erole:***@127.0.0.1:5432/cloudpivot_p3bgap \
  cargo test --lib -j 2 -- --test-threads=1

# 3) 真机 e2e（隔离实例 18097 + 隔离库）
P3B_BASE_URL=http://127.0.0.1:18097 P3B_DB_NAME=cloudpivot_p3bgap \
P3B_DB_USER=e2erole P3B_DB_PASSWORD=*** python3 scripts/verify/pflow3b_gap_e2e.py

# 4) 不变量（干净库）
NIK_DATABASE_NAME=cloudpivot_p3bgap_inv python3 scripts/verify/erp_invariants.py
```

## 六、已知边界

1. **prod 上没有 FINANCE / ACCOUNTANT 角色**（见 §1.3 注）：本迁移在 prod 是零变更；
   要让财务角色自助撤回，需要先建角色再重放迁移（幂等）。
2. 迁移的 down 会收回「非超管」的 4 个新码授权（无法区分本迁移插入的与本迁移之后手工加的，
   `sys_role_menu` 没有来源列），down 文件头已注明。
3. `payable_cancel` / `voucher_reverse` 的审批节点角色为 `FINANCE_MANAGER` /
   `FINANCE_DIRECTOR`，这两个角色**没有** `business:erp:payable:submit-cancel` 等发起码
   ——按设计它们是**审批人**不是发起人；发起码跟随老码矩阵（见 §1.3）。
