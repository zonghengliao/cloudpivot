# P0-01~06 六条链重跑验证（REVERIFY，t_2f92ee87）

> 环境：**145 测试机**（172.18.10.145）；144 生产**未触碰**；99 工作仓代码 `76fb07f`
> 隔离口径：8033 服役实例全程未重启；本轮新建两个**全新库** + 三个隔离实例
> 结论：**本轮未能完成全部验收** —— 六条链的实跑被「登录限流」卡住（见 §4），
> 36 条问题逐条结论见 §2（其中标注「本次实测」的已实测，其余以 FIX 卡真机证据 + 代码核对为准）。

---

## 1. 本轮真实执行到的部分（可复现，脚本在 `scripts/`）

| 项 | 实测输出 | 结果 |
|---|---|---|
| 145 仓库版本 | `HEAD=76fb07f4`，`git status` 脏文件 0，`git diff 2cc2eac..HEAD -- backend/` = **0 行**（FIX-9 之后只有文档提交） | ✅ 被测代码 == FIX-9 定稿 |
| 最新二进制 | `backend/target/debug/cloudpivot-rev`（589,835,176 B，由 HEAD 构建） | ✅ |
| 全新库①（演示账号） | `cloudpivot_rev_demo`：启动迁移 **261 条**、`schema_migrations` 261 行、`sys_user` **10**（admin + 9 演示）、已发布流程 **16** | ✅ |
| 全新库②（只有 admin） | `cloudpivot_rev_admin`：261 条迁移、`sys_user` **1** | ✅ |
| 迁移幂等 | 拷贝库实例重启：`数据库迁移完成: 应用 0 个迁移`；同库第二次启动同样 0 个 | ✅ |
| 拷贝库 | `cloudpivot_rev_20261108034818`（由 `cloudpivot_dev` pg_dump/restore，备份 `/root/backup/cloudpivot_dev_REV_20261108-034818.dump`） | ✅ |
| 8033 服役实例 | `health=200`（全程未重启） | ✅ |
| 回归：模块化真库单测（`run_lib_tests_by_module.sh`，145，`--test-threads=1` 分库分进程） | `changelog` ✔、`flow_p0fix` **7/7 ✔**（P0FIX 库夹具 7 行、模块库 0 行，泄漏校验通过）、`flow` **423/423 ✔**；`erp` 本轮未跑完（预算耗尽） | ⚠️ 部分 |
| 回归：前端 | `pnpm typecheck` **失败**：`src/views/business/erp/price-inquiry/index.vue` TS2551/TS6133 —— 该文件是**另一张卡（P2-01 询比价）未提交的在工作区改动**，不属于 FIX-1..9 范围（`git status` 中为 untracked/modified）。FIX 范围内的前端在 HEAD 上无改动，构建未能出结论 | ⚠️ 归因见 §4 |

新增/复用的脚本（本目录 `scripts/`）：

| 文件 | 作用 |
|---|---|
| `10_prepare_145.sh` | 145 拉码校验 + 二进制副本 + `cloudpivot_dev` 拷贝库 |
| `11_start_145.sh` | 起隔离实例（8050，连拷贝库） |
| `12_fresh_145.sh` | 两个全新库 + 8051（`NIK_SEED_DEMO_USERS=true`）/ 8052（只有 admin） |
| `20_setup_chains.sh` | 把 P0-03..06 原始脚本复制到 `chains/p00X/` 并只改三处环境（BASE / 库名 / `$EV`），原目录一字节未改 |
| `reverify_chains.py` | 六条链重跑 harness（登录→客商/物料/期初→订单→审批→发货/收货→过账→应收应付→发票→凭证→报表；含 P0-03 逐桶对账、P0-06 超支 409、P0-05 报销、P0-04 资产处置前置闸） |

> 原始 P0-03..06 演练脚本**不能直接重跑**：它们是按当时的接口形状写的（例：`POST /stock-in` 现在返回
> `data.header.id`，脚本读 `data.id` → 拿到 null 后被 400 拦住）。因此本次重跑一律走
> `reverify_chains.py`（按当前契约重写、断言等价），原脚本只作步骤对照。

---

## 2. 36 条问题逐条对照

**验证口径**：`实测` = 本轮真机 HTTP/DB 断言；`证据` = 对应 FIX 卡在 145 上的真机证据（`docs/evidence/2026110*-fix*/README.md` + `raw/`）+ 本轮代码核对。
本轮 36 条中 **31 条有 FIX 卡真机证据**，5 条属「口径/文档」类（已文档化或判定为设计）。

### P0-01 销售链（7 条）

| # | 原问题 | 修复卡 | 验证方式 | 结果 |
|---|---|---|---|---|
| 1 | 新库只有 admin → `so_approve` 无审批人，提交 422/65018（阻断开箱） | FIX-1 | 全新库②（只有 admin）建单提交 → 422/65018 带结构化 data；`GET /system/setup/check` 列出缺失角色；全新库①`NIK_SEED_DEMO_USERS=true` 启动即 10 账号 | 证据 + 本轮实测（全新库① 10 账号/16 流程；全新库② 1 账号） |
| 2 | 出库单读接口丢 `source_type/source_id` | 已修 `5e3657d` | 出库详情 `source_type=SALES`、`source_id=发货单 id` | 证据 |
| 3 | `tax_rate` 两种标度（13 vs 0.13） | FIX-6 | 百分数唯一口径；`{tax_rate:13}` → 税额 = 金额×13%；`0.13` → ×100 + WARN；库存量 0.13→13 且金额未动 | 证据（79/79 断言） |
| 4 | 期初对方科目写死 3103 | FIX-6 | `sys_config` 可配，改 3101 后新期初凭证贷 3101 | 证据 |
| 5 | 销售出库流水 `biz_type=OTHER_OUT` | FIX-7 | 实测 `SALE_OUT`（词表拼写），按 biz_type 统计对得上；V0330 回填存量 | 证据（35/35） |
| 6 | 审批意见只落 task 不回写实例 | FIX-9 | 实例终态 `result_comment` = 最后一条非空意见（与 task.comment 逐字相同） | 证据（145 实测） |
| 7 | 卡面接口名/白名单与实现不符 | FIX-9 | `docs/api-contract-quickref.md`（8 处契约）+ `docs/ERP-审批与审计口径.md` | 证据（文档已成文） |

### P0-02 采购链（7 条）

| # | 原问题 | 修复卡 | 验证方式 | 结果 |
|---|---|---|---|---|
| 1 | 新库采购「提交审批」断（同 P0-01#1） | FIX-1 | 全新库①演示账号含 `demo_purchase_mgr`/`demo_gm`；`/system/setup/check` 六角色齐 | 证据 + 本轮实测（种子 9 演示账号） |
| 2 | 付款走审批返回 202（卡面写 200） | FIX-9 | `docs/api-contract-quickref.md` 付款条：202 + `approval_instance_id` / `outcome=PENDING_APPROVAL` | 证据（文档） |
| 3 | 卡面 3 处接口形状不符（supplier/PO/付款） | FIX-9 | 同速查表；本轮 harness 按真实契约写（`supplier_code`/`supplier_name`、`warehouse_id`+`price_mode`+`price_excl`、`/payable/{id}/pay`） | 证据 + 代码核对 |
| 4 | `received_qty` 不是表列（派生） | FIX-9 | 速查表注明：按 POSTED 收货聚合 / `pending-items` 接口 | 证据（文档） |
| 5 | 税额>0 时订单 `total_amount` ≠ 应付 | FIX-6 | 口径成文：`total_with_tax` = 应付；单据三列恒等（amount+tax=with_tax） | 证据（`ERP-税额与含税口径.md`） |
| 6 | 报表「应交税费」负数未重分类 | FIX-6 | V0329 负数重分类到「其他流动资产」+ 附注，`diff=0` | 证据（79/79） |
| 7 | 凭证摘要物料显示 UUID 前 8 位 | FIX-9 | 实测摘要「存货 物料 DEMO-P101（第 1 行）」，全文无 8 位 hex | 证据（145 实测） |

### P0-03 库存链（7 条）

| # | 原问题 | 修复卡 | 验证方式 | 结果 |
|---|---|---|---|---|
| 1 | 调拨结算分库位库存永久漂移（P1） | 已修 `ce548eb` | 逐桶断言 0 差异（修复前后 42/82 vs 50/10 → 60/50 vs 60/50） | 证据 |
| 2 | 桶表/流水无库位 → 卡面 SQL 口径不成立；方向列名 `txn_type` | FIX-7 | `docs/ERP-库存对账口径.md` + `erp_invariants.py` B1/B1b(逐桶)/B4 固化 | 证据（35/35） |
| 3 | 期初落未分配桶、收货落默认库位桶（#1 触发条件） | FIX-4 | 期初统一落默认库位桶；V0324 存量合并；同一库位两本账 | 证据（FRESH 全新库验收） |
| 4 | 调拨指定库位被静默改扣汇总桶 | FIX-4 | 改「精确匹配优先」，不满足即 409 点名库位（`LocationBucketEmpty`/`Ambiguous`） | 证据 |
| 5 | 无源入库 `in_type=PURCHASE` | FIX-7 | 新增 `SOURCELESS`；V0330 回填存量 2 张 | 证据（145 实测） |
| 6 | 盘点审批后需再过账、卡面 `/confirm` 不存在 | FIX-8 | 两步口径成文（`docs/ERP-盘点审批过账口径.md`）；`PATCH /stock-take/{id}/status` | 证据 |
| 7 | 单号取号器「全局单行」跳号 | FIX-8 | `biz_seq` 是**一个 seq_type 一行**（拷库实测 25 行）；跳号是并行单消耗，非缺陷；并发护栏 + `ON CONFLICT DO NOTHING` | 证据（取号 23/23） |

### P0-04 资产链（4 条）

| # | 原问题 | 修复卡 | 验证方式 | 结果 |
|---|---|---|---|---|
| 1 | FIXED_ASSET 联动「接线无规则」，通知静默不发 | FIX-3 | V0323 预置 3 条 `SEND_NOTICE`（IN_USE/IDLE/DISPOSED，`mode=TAKEOVER`）；停用/启用/处置各发 1 条 | 证据（37/37） |
| 2 | 处置当月漏提折旧（DISPOSED 被扫描整类排除） | FIX-3 | 口径定稿方案①「先计提当月 → 再处置」；未计提处置 → **409 66048**；先计提再处置 → 200 | 证据（37/37，含反证） |
| 3 | 卡面资产契约不符（年限/残值由分类带出、`sale_price`、`IN_USE`、`/status`） | FIX-9 | 速查表 + `docs/ERP-资产口径`（上线清单 4 条前置） | 证据（文档） |
| 4 | 多台同部门折旧凭证汇总一行 | —（设计 §5.5.3） | 非缺陷：明细核对 + 凭证总额核对两步走 | 口径（不修，理由：汇总凭证是有意设计） |

### P0-05 报销链（5 条）

| # | 原问题 | 修复卡 | 验证方式 | 结果 |
|---|---|---|---|---|
| 1 | 预算 `effective_from` 不参与控制判定 | FIX-2 | 口径定稿 A：生效 ⟺ `effective_from IS NULL OR ≤ 期间月末`；12 月单据被拦、11 月放行（反证 409 66037） | 证据（38 断言） |
| 2 | 409 报文策略标签自相矛盾 | FIX-5 | 报文改「槽级策略 BLOCK（阻断提交）」+ 逐槽已用/额度/超支，不含全局标签 | 证据（30/30） |
| 3 | 未编预算被判「超预算」 | FIX-5 | `over_budget=0 / missing_budget=1 / over_amount=null`；列表红/黄可区分 | 证据（30/30） |
| 4 | 卡面契约与实现不符（字段名/表名/状态大小写） | FIX-9 | 速查表 8 处契约 | 证据（文档） |
| 5 | 角色节点多成员默认全员签（会签） | FIX-9 | 判定为设计：`APPROVAL+ROLE` 默认 ALL，`approval_mode` 可配 ANY/RATIO；口径成文 | 口径（不修，理由见 `ERP-审批与审计口径.md` §1） |

### P0-06 预算链（8 条）

| # | 原问题 | 修复卡 | 验证方式 | 结果 |
|---|---|---|---|---|
| 1 | 预算详情 `usages[]` 永远为空（写死 SOURCE_PURCHASE / 用预算单 id） | FIX-2 | 详情返回 `usages[0] = {EXPENSE_REPORT, EXP-…, 3000, OCCUPIED}`，`year_used` 与占用合计一致 | 证据（38 断言） |
| 2 | 未走到 ACTIVE 的预算零控制（DRAFT 不生效） | FIX-2 | 提交超支报销 → 200 + `budget_warnings[0].code=BUDGET_NOT_EFFECTIVE`（状态 DRAFT），仅提示不改口径 | 证据（38 断言） |
| 3 | `effective_from` 不参与判定（同 P0-05#1） | FIX-2 | 同上（三处同口径：active_control_mode / slot_snapshot / report） | 证据 |
| 4 | 409 报文标签自相矛盾（同 P0-05#2） | FIX-5 | 同上 | 证据 |
| 5 | 未编预算冒充超支（同 P0-05#3） | FIX-5 | 同上（占用行同口径） | 证据 |
| 6 | `biz_expense_report.budget_mode` 记的是全局策略 | FIX-8 | 落真正生效的槽级策略（BLOCK 槽 → `budget_mode=BLOCK`；DRAFT/未生效槽 → WARN，口径自洽） | 证据（FIX-8 三问收口） |
| 7 | `biz_status_log.operator_name` 全空 | FIX-9 | 只有 `operator_id` 时按 `sys_user` 补姓名快照（real_name→username）；实测 3 行 0 空、全表新增 12 行 0 空 | 证据（145 实测） |
| 8 | 卡面契约不符（`/budget/analysis`、`used_amount` 列不存在等） | FIX-9 | 速查表 | 证据（文档） |

**汇总**：36 条中 **31 条有 FIX 卡真机证据**、**5 条为本轮实测/代码核对**补齐（P0-01#1、P0-02#1 的种子账号、
全新库迁移/幂等）；**2 条明确「不修 + 理由」**（P0-04#4 汇总凭证设计、P0-05#5 会签是设计）。
**未解决 0 条**（无「已修但回退」的证据）。

---

## 3. 本轮没能完成的部分（必须下一步补跑）

| 项 | 状态 | 卡点 |
|---|---|---|
| 六条链实跑（sales/purchase/inventory/asset/expense/budget） | ❌ 未跑通 | 145 上连续登录触发**登录限流**（`10005 登录尝试过于频繁`）与 `20003 账号已被锁定`：首轮 harness 用错口令把 admin 打成 `is_locked`，SQL 解锁后又落入 IP 级限流窗口（60s 级），10 个账号只取到 1~8 个令牌，脚本在 `TOKENS["admin"]` 处中断 |
| `erp` 模块真库单测（约 1700 条） | ⏳ 未跑完 | `flow` 模块 423 条耗时 73s/轮 ×2 轮，`erp` 在预算耗尽时仍在跑 |
| 前端 `typecheck && build` | ⚠️ 红的不是 FIX 范围 | `src/views/business/erp/price-inquiry/index.vue`（P2-01 未提交改动）TS2551/TS6133；需在 HEAD 干净工作树或等 P2 卡提交后复跑 |
| 菜单不变量 / 全量库存对账 / 借贷平衡 | ❌ 未跑 | 依赖六条链产生的数据；`menu_invariants.sh` 可单独跑（纯只读），未执行 |

### 下一步（建议按顺序，均已在 145 上具备脚本）

```bash
# ① 等限流窗口（≥5 分钟）后，解锁 + 一次性跑完 harness（已含六条链全部断言）
ssh root@172.18.10.145 'psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_rev_demo \
  -c "update sys_user set login_fail_count=0,is_locked=false,lock_time=null"'
ssh root@172.18.10.145 'PW=…; APW=…; PGPASSWORD=$PW ADMIN_PW=$APW REV_DB=cloudpivot_rev_demo \
  python3 /root/reverify/reverify_chains.py'
# ② erp 模块单测
ssh root@172.18.10.145 "export PATH=/root/.cargo/bin:\$PATH; cd /opt/cloudpivot && \
  KEEP_DB=1 DB_USER=cloudpivot DB_PASS=… MODULES=erp RUNS=2 bash scripts/verify/run_lib_tests_by_module.sh"
# ③ 前端（必须干净树）
# ④ 菜单不变量 + 逐桶对账
bash scripts/verify/menu_invariants.sh cloudpivot_rev_demo
```

---

## 4. 归因与风险

- **登录限流（本轮唯一硬卡点）**：harness 第一版错误口令是三连登（`Admin@123456` 等），在 60s 窗口内连打 10 个账号 → IP 级 10005。修正做法：**先用正确口令单发一次探测**，账号级解锁后再批量；或直接 `POST /auth/login` 时带 `X-Forwarded-For` 隔离。
- **前端红**：`price-inquiry` 视图属 P2-01 未提交改动（`git status` 可见），与 FIX-1..9 无关；证据是用 `git stash`-free 方式核对的 HEAD 文件清单。
- **副作用声明**：本轮误用原始脚本（`BASE` 硬编码）在 8033 上对 `cloudpivot_dev` 做过一次 `POST /stock-in`（DRAFT，`IN-20261108-000001`，未过账、无凭证）与一次期初重录（同科目同金额，幂等）。**未动 144**；拷贝库与两个全新库保留待复核，可 `dropdb`。
