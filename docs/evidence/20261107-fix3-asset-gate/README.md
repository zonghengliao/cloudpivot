# FIX-3 资产两连 · 验收证据（145 真机 · 全新库 cloudpivot_fix3n · 隔离实例 8041）

> 卡：`t_0fa8d2c3`「FIX-3 资产两连：FIXED_ASSET 联动规则缺失 + 处置当月漏提折旧」
> 代码 commit：**`93e0ffc`**（运行时行为全在这里；origin/main = github/main = 145 已 pull）
> 后续两个 commit 不含运行时改动：`c896f18`（本证据文档 + 脚本）、`5780949`（+1 条纯逻辑守卫用例）
> 环境：145 测试机，全新库 `cloudpivot_fix3n`（`FRESH=1` 删库重建，跑完整迁移集），实例 `127.0.0.1:8041`（隔离，`target/debug/cloudpivot-fix3`，验完已停）
> 结论：**37/37 断言全绿（FAIL=0）**，`cargo test` 23 个目标 **3548 passed / 0 failed**，本卡资产真库用例 **11/11**。

---

## 一、改了什么（问题 → 修复）

### 问题 1：FIXED_ASSET「接线了但没规则」

| 项 | 内容 |
|---|---|
| 现象 | `spec::TRIGGER_SPECS` 标 `wired=true`，`asset_linkage` 三处 hook（停用/启用、处置、作废处置）都在跑，但迁移全集跑完后 `biz_flow_linkage_rule` 里 FIXED_ASSET 规则 **0 条** ⇒ 一条通知都不发，且**没有任何报错** |
| 修复 | 新增迁移 **`V0323__asset_linkage_rules`**：预置 3 条 `SEND_NOTICE`（`IN_USE` / `IDLE` / `DISPOSED`），`action_params = {"mode": "TAKEOVER"}` |
| 为什么必须 TAKEOVER | 资产通知**没有旧代码在发**（`AssetStatusChange::ctx` 只把草稿塞进联动上下文，投递完全靠引擎），`SHADOW` 只做影子对比、不投递 ⇒ 写 SHADOW 等于规则配了也不发。单测把这条钉死 |
| 触发点覆盖 | 触发状态 = 变更**之后**的目标态：停用→`IDLE`；启用 / **作废处置**→`IN_USE`（同一目标态两个真实入口，一条规则覆盖两者）；处置→`DISPOSED`。建卡（`NULL→IN_USE`）是建行、不接触发点 |
| 幂等 | `ON CONFLICT (trigger_entity, trigger_status, action_code) DO NOTHING`：生产上运维手工配过 / 停用过 / 软删过的同键行**原样保留**（不替运维改配置）；down 段按**同批键**物理删 + 自检 |
| 防漂移 | `asset_linkage.rs` 新增 6 条纯逻辑用例：条数=3 / 状态集 = 资产的合法状态集 / 迁移原文含每条键 / 参数过第 2 层校验且 `mode=TAKEOVER` / 幂等写法（且**不许** `DO UPDATE`）/ down 键一致 / `wired=true` |

### 问题 2：处置当月漏提折旧

| 项 | 内容 |
|---|---|
| 现象 | `asset_compute` 头部写「当月减少当月照提」，扫描 SQL 也确实用 `disposal_date >= 期初`；但 `scan_assets_for_period` 的状态条件是 `status IN ('IN_USE','IDLE')` ⇒ 卡片一旦 `DISPOSED` 就被**整类排除**，月中处置的资产当月一个月折旧提不到；而处置凭证已按处置时点的累计折旧落账 ⇒ 卡片缓存 / 折旧明细 / 凭证三处永久差一个月 |
| 口径 | **按卡面推荐方案 ① 定稿**（写死在 `asset_compute::disposal_dep_gate` 的注释里，含「为什么不做方案 ②」）：动作顺序钉成「**先计提当月 → 再提交处置**」；处置入口前置校验「处置当月的折旧已计提并过账」，不满足 → **409 + `66048`（`ERP_ASSET_DEPRECIATION_PENDING`）** |
| 为什么不做方案 ② | 要让扫描纳入 `DISPOSED` 且把处置凭证延后到期末折旧后：处置凭证时点被绑到「批次什么时候跑」，反结账 / 红冲 / 重提都会让凭证日期与审批通过日脱钩 = 重定义会计口径（需财务确认）。方案 ① 扫描 SQL 一行不改、批次状态机与凭证时序零改动 |
| 闸门位置 | 放在**提交处置**（`asset_handler::asset_dispose`，审批发起之前 fail fast）；不放审批落定后的执行步 —— 那时审批已通过，半路 409 只会把处置单卡在中间态 |
| 两种放行（当月本来不提） | ① 当月购入当月处置（起提日 = 次月 1 日，卡面点名的边界）；② 已提足（累计 ≥ 可折旧总额，扫描也不会再命中）。纯函数 5 条用例覆盖，含跨年边界与「差一分也拦」 |
| 新增错误码 | `ERP_ASSET_DEPRECIATION_PENDING = 66048`（与 66038 期间已结账 / 66040 审批在跑语义分开，报文点明期间与「先计提」）。

### 顺带必须改的夹具（否则真库用例直接红）

`asset_linkage_tests.rs` 的插规则是裸 `INSERT`，V0323 之后同一触发点同一动作码会撞 `uk_flow_linkage_rule`：

* `add_rule_named` 改 `ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE`（用例自己的参数覆盖预置行）；
* `purge()` 收尾加 `restore_presets()`：把 3 条预置行刷回迁移口径 ⇒ 「跑过测试的库」与「刚跑完迁移的库」一致。

---

## 二、改动清单（路径 + 行数）

| 文件 | 改动（`git show --numstat 93e0ffc`） |
|---|---|
| `backend/migrations/business/V0323__asset_linkage_rules.up.sql` | **新增 127 行**（3 条预置规则 + 幂等 + 迁移期自检） |
| `backend/migrations/business/V0323__asset_linkage_rules.down.sql` | **新增 35 行**（按同批键物理删 + 自检） |
| `backend/src/business/erp/asset_compute.rs` | **+247**：`DisposalDepGate` / `DepGateMiss` / `DepGateSkip` / `disposal_dep_gate()` + 口径定稿注释 + 5 条用例 |
| `backend/src/business/erp/asset_model.rs` | **+27**：`depreciation_status_of(asset_id, period_code)`（一台一期一行状态） |
| `backend/src/business/erp/asset_handler.rs` | **+57**：`asset_dispose` 处置前置闸（409 66048 + 两种放行的 info 日志）+ 函数注释 |
| `backend/src/business/erp/asset_linkage.rs` | **+159**（`93e0ffc`）+ **+44**（`5780949`）：7 条纯逻辑用例 —— 6 条钉「迁移原文 ↔ 代码契约」（`include_str!`），1 条钉「处置闸真的接在 handler 提交步且不许挪到执行步」 |
| `backend/src/business/erp/asset_linkage_tests.rs` | **+39/-2**：插规则改 upsert + `restore_presets()` |
| `backend/src/shared/error_code.rs` | **+14**：`ERP_ASSET_DEPRECIATION_PENDING = 66048` + 码值登记表一行 |
| `docs/evidence/20261107-p004-fixed-asset-chain/README.md` | **+22**：新增「六、上线清单（资产模块 · 部署 / 升级必查）」四条 + 升级影响说明 |
| `docs/evidence/20261107-fix3-asset-gate/{run145.sh,verify145.sh,dump_db.sh,README.md,raw/*}` | 本证据目录（可复跑脚本 + 原始输出） |

**未改**：`scan_assets_for_period`（口径不动）、任何已应用的迁移文件、前端、144 生产。

---

## 三、145 实测（37/37 全绿）

原始输出：`raw/verify-145.log`（全量），关键段落摘录：

### ② 全新库跑完全集迁移 → 3 条 FIXED_ASSET 规则

```
数据库迁移完成: 应用 254 个迁移          # 全新库 cloudpivot_fix3n，max(version)=323
DISPOSED|true|TAKEOVER
IDLE|true|TAKEOVER
IN_USE|true|TAKEOVER
✅ FIXED_ASSET 有 3 条 SEND_NOTICE 规则（3）
✅ 3 条全部 enabled + TAKEOVER（3）
✅ 触发状态覆盖 DISPOSED / IDLE / IN_USE
```

### ⑥ 迁移幂等（up.sql 连跑两次）

```
✅ up.sql 连跑两次不报错（自检块通过）（0/0）
✅ 重跑后规则仍是 3 条（ON CONFLICT DO NOTHING，不生出第二份）（3）
NOTICE: V0323 完成：FIXED_ASSET 预置 3 条 SEND_NOTICE 联动规则…当前存活 3 条、其中 enabled+TAKEOVER 3 条
```

另在 99 本地库额外验证「不覆盖手工配置」：把 IDLE 改成 `enabled=false, recipient=OPERATOR` 后重跑迁移 →
规则仍是 `IDLE|false|{"mode":"TAKEOVER","recipient":"OPERATOR"}`（迁移 NOTICE 报「2 条 TAKEOVER」），**没有**被刷回预置值。

### ① 停用 / 启用 / 处置 → 各产生通知（无预置规则时全是 0）

```
✅ 停用 HTTP=200 / ✅ 启用 HTTP=200
通知条数 0 → 2
资产闲置   | LNK:FIXED_ASSET:IDLE:<卡片>:<事件>
资产在役   | LNK:FIXED_ASSET:IN_USE:<卡片>:<事件>
资产已处置 | LNK:FIXED_ASSET:DISPOSED:<卡片>:<事件>      # 处置审批通过后
```

服务端日志同时留痕（联动引擎执行结果）：

```
asset_linkage: 提交后的联动动作执行结果 … status="IDLE"     action_code=SEND_NOTICE outcome="SUCCESS" reason="通知 … 已发送（接收人 1，duplicated=false）"
asset_linkage: 提交后的联动动作执行结果 … status="IN_USE"   action_code=SEND_NOTICE outcome="SUCCESS"
asset_linkage: 提交后的联动动作执行结果 … status="DISPOSED" action_code=SEND_NOTICE outcome="SUCCESS"
```

### ③ 未计提当月折旧 → 处置 409 66048

```
http=409 code=66048
msg=资产 FA-20261107-000001 不能处置：处置当月没有该资产的折旧明细（批次未计提，或批次已过账但不含本资产）。
    请先在「折旧计提」里计提并过账 2026-11 的折旧（手动计提时 period_code 填 2026-11），再回来提交处置
    —— 当月减少的资产当月照提，先处置会让这一个月的折旧永远提不到（处置凭证已按处置时点的累计折旧落账）。
    若该期间的批次已过账但不含本资产、或已被红冲，需先红冲该批次再重新计提。
✅ 报文点名处置当月 2026-11 / ✅ 报文指路「先计提」/ ✅ 被拦时没有起审批实例（0）
```

边界：`③-b 当月购入当月处置（资产 C，购入 2026-11-05 → 起提 2026-12-01）` → **200 放行**（`code=0`），
服务端日志 `处置前置闸：当月无需计提折旧，放行 why="当月购入当月处置：本月本来不提折旧"` —— 闸门没有误杀边界。

### ④ 已计提 → 处置成功，凭证正确（含当月折旧）

```
✅ 2026-11 折旧计提 HTTP=200     批次 DEP-20261107-000001 POSTED / 2 台 / 2850.0000
✅ 资产 A 2026-11 折旧明细 1900 / ✅ 资产 B（未处置）950 / ✅ 资产 C 当月购入 0 行
✅ 已计提当月折旧 → 处置提交 200  审批实例 FLW-20261107-2F7FA1F6
✅ 总经理审批通过 HTTP=200
卡片终态：DISPOSED|SALE|2026-11-20|1900.0000
处置凭证：
  PZ-…-000005 | 1602 借1900.0000        （累计折旧结转）
  PZ-…-000005 | 1606 借118100.0000      （净值 = 120000 − 1900）
  PZ-…-000005 | 1601 贷120000.0000
  PZ-…-000006 | 1002 借113000.0000 / 1606 贷100000.0000 / 2221.02 贷13000.0000
  PZ-…-000007 | 5711 借18100.0000 / 1606 贷18100.0000
✅ 处置凭证全部借贷平衡 / ✅ 凭证借记 1602 = 1900（当月折旧已进处置分录）/ ✅ 1606 净额归零
```

对照 FIX-3 之前的口径：同一动作下 6 月处置 → 6 月批次命中 0 台（漏提 1900）；现在闸门把顺序钉成
「先计提当月 → 再处置」，处置凭证里的 1602 = 1900 就是当月那一笔。

### ⑤ 回归：未处置资产折旧照常

`FA-20261107-000002`（未处置）2026-11 明细 `950.0000` POSTED、卡片累计 950 / 净值 59050 —— 闸门只作用在处置入口，扫描与计提逻辑一行未改。

---

## 四、cargo test

| 项 | 结果 |
|---|---|
| `cargo test --features test-hooks`（99 工作区，最终 commit `5780949`） | **23 个测试目标 / 3548 passed / 0 failed**（11 ignored 全在既有 doctest 目标，非本卡新增） |
| 本卡新增纯逻辑用例 | `asset_compute` 5 条（闸门判据）+ `asset_linkage` 7 条（6 条迁移↔代码契约 + 1 条 handler 调用点漂移守卫）全绿 |
| 资产**真库**用例（`ERP_DB_URL` 指向已跑迁移的新库） | `business::erp::asset_linkage_tests` **11/11 passed**（含 `lnk09_*` 5 条 + `st02_*` 6 条；证明预置规则与用例夹具共存、预置行按迁移口径复原） |
| clippy | 本卡改动文件零告警（仓库基线 `sales_order_model.rs:699 too_many_arguments` 为既有问题，未触碰） |

> 真库用例是**必须**跑的：不设 `ERP_DB_URL` 时它们静默 SKIP（假绿）。本卡用新库
> `cp_fix3_erp`（99 本地，迁移全集 + V0323）跑了一遍：11/11 通过，跑完库里预置规则被
> `restore_presets()` 刷回 `资产停用 → 通知制单人` 等迁移原名。

---

## 五、怎么复跑

```bash
# 145 上（会用 FRESH=1 删库重建，务必确认 DB 名是测试库）
scp docs/evidence/20261107-fix3-asset-gate/*.sh root@172.18.10.145:/tmp/fix3/
ssh root@172.18.10.145 'chmod +x /tmp/fix3/*.sh; FRESH=1 DB=cloudpivot_fix3n PORT=8041 bash /tmp/fix3/run145.sh'
# 只导断言（库已存在时）
ssh root@172.18.10.145 'DB=cloudpivot_fix3n bash /tmp/fix3/dump_db.sh'
```

脚本自带：建库 → 起隔离实例（`/health` 200 + `Address already in use` 计数 0 双判据）→ 37 条断言 →
服务端日志摘录 → 停实例。任一断言 ❌ 则 `exit 1`。
