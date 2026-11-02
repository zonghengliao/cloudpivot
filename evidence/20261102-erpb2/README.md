# ERP-B2 制单 ≠ 审批 / 过账 / 付款（内控可配开关）— 交付与验收证据

> 卡 t_31f0d857。评审发现「收货单制单人 = 过账人 8 条」。内控上应当分离，
> 小团队却常常只有一个人能干活 —— 因此做成**三个独立开关 + 一个总闸**，默认值与改动前逐字一致。

## 一、关键设计决策（先看这一段，后面才有意义）

### 1. 为什么是 3 个开关 × 4 个参数

| 开关键 | 默认 | 打开后拦什么 | 默认值的依据 |
|---|---|---|---|
| `erp.separation_of_duties.enabled` | `false` | 总闸：把**未单独配置**的项默认成「开」 | 不影响存量 |
| `erp.separation_of_duties.post` | `false` | 制单人不能自己过账（收货过账 / 调拨发货收货） | 改动前本来就不拦 |
| `erp.separation_of_duties.approve` | `true` | 制单人 / 发起人不能审批自己那一票 | 改动前**已有** 403 内控闸 |
| `erp.separation_of_duties.pay` | `true` | 制单人不能审批自己提交的付款 | 改动前**已有** P17 闸 |

`approve` / `pay` 默认 `true` 不是「新开内控」，而是**记录既有行为**：
这两条闸改动前就在跑，开关化之后必须默认仍然拦，否则升级即静默放宽内控。

### 2. 总闸「只升不降」

种子把 `enabled` 写成 `false`。如果总闸能「降级」，那这一行会把默认开启的审批 / 付款内控
一起关掉（= 静默放宽内控）。所以解析规则是：

```text
① 本项自己的键（post / approve / pay）存在且能解析成 bool  → 用它
② 总闸 enabled 存在且为 true                             → true
③ 否则                                                   → 代码默认值
```

要单独关某一项，就显式把该项设成 `false`。

### 3. 非法值回退「默认」而不是 `false`

`yes` / `ture` 这类值回退**代码默认**并打 `warn`，绝不静默当 `false` ——
「配置写错 = 内控被关」是更危险的方向。

### 4. 开关落在**真正动账的那个函数**上，而不是每个入口各写一遍

改之前只拦「没配流程时的降级直过」是**不够的**：种子流程 `stock_in_post` 对 **< 1000 元**的单据
「免审直过」（只有 NOTIFY 节点），过账人就是**提交人本人** —— 这才是评审实测
「收货单制单人 = 过账人 8 条」的真正来源。所以闸门加在两条路**都会汇入**的
`stockin_handler::post_after_approval`（六本账真正落库前），并对「免审直过被拦」做了
**打回草稿**处置（否则单据会停在 `PENDING_POST` 且没有任何待办能推进它 = 死单）。

## 二、改动清单

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/separation.rs`（新增 510 行） | 开关解析（纯函数 `resolve`）、生效值 `enabled`、闸门 `guard` / `check`、拦截文案 `rejection_text` / `approval_hint`、9 条单测 |
| `backend/migrations/business/V0196__erp_separation_of_duties_config.{up,down}.sql` | 4 条参数 + `config_category` 字典项「ERP 内控」+ up/down 双向自检 |
| `backend/src/business/erp/stockin_handler.rs` | ① `change_stock_in_status` 降级直过前闸；② `post_after_approval` 真正过账前闸（覆盖免审直过 + 审批回调）；③ 免审直过被拦 → 打回 `DRAFT` |
| `backend/src/business/erp/transfer_handler.rs` | 调拨「发货 / 收货」（`IN_TRANSIT` / `DONE`）前闸 |
| `backend/src/business/erp/purchase_bridge.rs` | `guard_self_approval`（采购单审批）改读 `approve` 开关 + 文案追加开关名 |
| `backend/src/business/erp/stockin_bridge.rs` | 收货过账审批 / 作废审批同上 |
| `backend/src/business/erp/voucher_bridge.rs` | 凭证红冲审批同上 |
| `backend/src/business/erp/payable_bridge.rs` | 付款审批读 `pay`、应付作废读 `approve` |
| `backend/src/business/flow/internal_control.rs` | 待办生成时的「制单人排除（落 SKIPPED）」同受 `approve` 管辖 —— 闸关掉时既不排除也不 403，两处同源不矛盾 |
| `backend/src/shared/error_code.rs` | 新增 `ERP_SEPARATION_OF_DUTIES = 66013`（HTTP 409） |

**为什么是 409 不是 403**：403 = 你这个账号没权限；这里是账号**有**权限，只是这次「人不对」，
换个人立刻就能做。前端该提示「换人 / 关开关」，不是「去找管理员要权限」。

**手工凭证刻意不覆盖**：`create_manual_voucher` 是「制单即记账」的单步动作，`create_by` 恒等于
操作人，同一动作内无法分离；要分离必须拆「制单草稿 + 他人记账」两步（新状态 + 新接口）。
自动凭证（收货 / 付款产生）随单据过账一起生成，已由调用方的闸覆盖。

**读性能**：取值走 `support::config::model` 的进程内运行时快照（命中是纯内存读，**不会每次查库**），
TTL 60 秒兜底「别的实例 / 直改库」；管理端改参数会 `after_write → reload + 广播`，**下一枪就生效**。

## 三、单测（9 条，纯逻辑不连库）

`cargo test --lib erp::separation` → `9 passed; 0 failed`

| # | 用例 | 钉住什么 |
|---|---|---|
| ① | `default_off_lets_creator_post` | 参数缺失 → post 默认 `false`，制单人能过账 |
| ② | `switch_on_blocks_creator_with_readable_409` | 409 + 66013 + 文案含开关名 / 单号 / 「不能自己过账」/「关闭该开关」 |
| ③ | `switch_on_lets_others_post` | 别人过账放行；无制单人的历史单也放行 |
| ④ | `approve_duty_blocks_creator_by_default` | 审批默认开、拦自己、放行他人 |
| ⑤ | `pay_duty_blocks_creator_by_default` | 付款默认开；显式关掉后自己也能付 |
| ⑥ | `missing_parameters_fall_back_to_code_defaults` | 三项都用代码默认（false / true / true） |
| ⑦ | `invalid_value_falls_back_to_default_and_reports` | 非法值回退默认且**不把内控关掉**，并上报原文 |
| ⑧ | `changes_take_effect_immediately` | 改完即生效；总闸只升不降；显式配置优先于总闸 |
| + | `rejection_text_is_complete_for_every_duty` | 三项文案恒等约束（空单据描述不留空档） |

## 四、真机验收（隔离库 `erpb2_e2e` / 端口 18097）

### 4.1 接口链路 `scripts/verify/b2_separation_e2e.py` → **PASS=31 FAIL=0**

卡面 §四 四步逐条钉住，外加只能在真机验的三条：

| # | 断言 | 结果 |
|---|---|---|
| ① | 参数页返回 4 条开关、类型 `BOOLEAN`、分组 `erp_internal_control`、默认 false/false/true/true | 10 项 PASS |
| ② | **开关默认关闭**：制单人过账 200，`posted_by` = 制单人本人（存量行为不变） | PASS |
| ③ | **开关开启**：制单人过账 **409 / 66013**，文案含「系统已开启「制单与过账分离」」「不能自己过账」「关闭该开关」+ 开关键名 | PASS |
| ③ | 被拦后单据**回到 DRAFT**（免审分支已打回，不是停在 `PENDING_POST` 的死单），库存流水 0 条 | PASS |
| ④ | 同一张单换 admin 过账 → **200**，`posted_by` = admin（≠ 制单人），库存流水已落库 | PASS |
| ⑤ | 关掉开关 → 制单人**立刻**能过账 200（不调 `/support/config/reload`，改完即生效） | PASS |
| ⑥ | 软删 `post` 参数 → 回退代码默认 `false` → 制单人仍能过账 | PASS |
| ⑦ | 参数值写成 `yes` → 回退默认 → 仍能过账，且服务端日志出现 `ERP 内控开关取值非法` | PASS |

### 4.2 页面（F6 状态可见）`scripts/verify/b2_config_page_check.mjs` → **PASS=12 FAIL=0**

支撑功能 / 参数配置页实测 DOM：

```
分组标题：announcement / erp / ERP 内控 / 认证配置 / 日志配置 / changelog
「ERP 内控」组内 4 条参数，其中：
  enabled  开关=关闭（与库一致）已展示说明
  post     开关=关闭（与库一致）已展示说明
  approve  开关=开启（与库一致）已展示说明
  pay      开关=开启（与库一致）已展示说明
页面可见「小团队可不开 / 建议开启」说明
```

截图：`evidence/20261102-erpb2/config-page-erp-internal-control.png`

> 前端**零改动**：配置页本来就是「按 `category` 分组 + `BOOLEAN` 渲染成 `t-switch` +
> 分组名查 `config_category` 字典」，迁移把字典项种上就自动多出一组。
> 迁移里踩到的坑：`config_category` 字典**类型**是 seed 迁移（`S__dict_data.sql`）种的，
> 而 seed 在全部 V* 迁移**之后**才跑 —— 空库首启时「只插字典项」会命中 0 行，
> 自检把启动直接打挂（实测 `INSERT 0 0` + `V0196 自检失败`）。现在 up 侧先确保字典类型存在。

### 4.3 全量单测 + 不变量

```
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/erpb2_e2e cargo test --lib -- --test-threads=1
  → test result: ok. 1901 passed; 0 failed; 0 ignored  (617.88s)

# 干净库（迁移 + bootstrap，未跑任何夹具）基线
python3 scripts/verify/erp_invariants.py → 32/32 通过  BLOCKER 0  WARN 0
# 跑完 e2e 后再量（证明这条链路不留残留）
python3 scripts/verify/erp_invariants.py → 32/32 通过  BLOCKER 0  WARN 0
```

## 五、复现

```bash
# 1) 隔离库 + 实例（重建 + 迁移 + 保持运行，端口 18097）
bash scripts/verify/b2_dbsetup.sh keep

# 2) 接口链路
B2_LOG=/root/tmp/b2_server.log python3 scripts/verify/b2_separation_e2e.py

# 3) 页面（另开一个终端起前端，代理指向 18097）
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18097 pnpm dev
cd frontend && node ../scripts/verify/b2_config_page_check.mjs

# 4) 全量 + 不变量
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/erpb2_e2e \
NIK_PROFILE=test NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  bash -c 'cd backend && cargo test --lib -- --test-threads=1'
NIK_DATABASE_NAME=erpb2_e2e NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  python3 scripts/verify/erp_invariants.py
```

## 六、已知边界（不藏着）

1. **手工凭证**不覆盖（理由见 §二）。
2. **调拨**只覆盖状态流转（发货 / 收货）；调拨若配了审批流，审批侧仍走 `approve` 开关。
3. 开关是**全局**的（非按单据类型/按组织），与现有 `sys_config` 能力一致；
   按组织细化需要配置中心支持作用域，不在本卡。
4. `enabled` 总闸只升不降是**刻意**的；想关某一项必须显式关那一项，参数说明里已写明。
