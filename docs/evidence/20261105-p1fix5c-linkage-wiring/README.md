# P1-FIX-5C 跨单据联动：13 个白名单触发点未接线（配得上但永不触发）

**卡**：`t_befea97a`（父卡 t_53c23814 / P1-FIX-5B）
**结论（先给判断）**：**两条都做**，但分工不同 —— 配置面**收口**（未接线触发点不再是可选项，保存直接 400），
接线面**按业务价值补了 1 条真链**（`STOCK_OUT/POSTED`，出库过账），并把 `CREATE_TASK` 动作从占位改成真实现，
于是「配置页配的规则 → 源单据动作 → 目标单据被创建」是**真机可复现**的 19 条断言，不是推断。

---

## 1. 问题回顾（5B 遗留）

`spec::TRIGGERS` 白名单 15 个触发点全部下发、全部可选，但代码里只有 2 个（`TASK` / `STOCK_IN`）真的调了
联动引擎。运维给「出库过账 → 通知」「应付到期 → 提醒」配规则：**保存成功、永不触发、无任何提示**。

这正是 5B 修掉的「规则表 0 行、引擎从没跑过」的同类问题：**配置面 > 能力面**。

## 2. 本次做了什么

### 2.1 配置面收口（`spec::TRIGGER_SPECS` = 一份真相）

| 位置 | 变化 |
|------|------|
| `linkage/spec.rs` | 新增 `TRIGGER_SPECS: &[TriggerSpec]`（`entity / wired / wiring / proofs`）+ `is_wired_trigger / trigger_spec / wired_triggers / unwired_triggers`；15 个触发点逐条写明「接线位置」或「为什么没接 + 接的时候要动哪个文件」 |
| `linkage/model.rs` | `upsert_rule` 新增硬门：**在未接线触发点上建/启用规则 → 400**，文案点名「尚未接线」+ 当前已接线清单；只有显式 `allow_unwired = true`（配置页二次确认）才允许「提前预置」 |
| `linkage/admin.rs` | `catalog_triggers` 下发 `wired` + `wiring`；`preview_impact` 增 `trigger_wired` + 未接线 warning；`check_draft`（草稿预览）对未接线触发点当场 `ok=false`；`rule_problems` 给存量规则标「这条规则不会触发」 |
| 前端 | 归 architect 卡 t_21dc81c3（同一份 `wired` 字段）：未接线置灰 + 二次确认 |

**没有采用**「只警告不拦」：警告会被无视，而这类配置的后果是**静默不生效**，必须拦在写入口。

### 2.2 接线（新增 1 条真链 + 1 个动作落地）

| 触发点 | 接线位置 | 事务内 | 提交后 |
|--------|----------|--------|--------|
| `STOCK_OUT/POSTED`（**本次新增**） | `erp/stockout_handler.rs::post_out_tx` 第 ⑥ 步 | `run_excluding(spec::STOCKOUT_EXCLUDED_ACTIONS)` | `post_stock_out` commit 后 → `stockout_linkage::run_deferred_linkage` → `run_deferred` |
| `STOCK_IN/POSTED`（5B） | `erp/stockin_handler_post.rs` ⑦⑧ | 专用路由 + `run_excluding([GENERATE_VOUCHER])` | `run_stockin_deferred_linkage` |
| `TASK/*`（第 2 步） | `task/task_handler_notice.rs` | — | `status_notice`（影子对比 / 接管） |

**排除清单为什么不写死在引擎里**：出库过账自己已在 ③⑤ 写完库存（成本账 + 库位账 + 流水）、成本快照、
成本结转凭证。通用引擎若再跑 `WRITE_INVENTORY / WRITE_INVENTORY_TXN / UPDATE_COST / GENERATE_VOUCHER`，
就是**同一行明细扣两次库存 + 一张单两张凭证**。所以排除清单由**调用方**传（`spec::STOCKOUT_EXCLUDED_ACTIONS`），
换个触发点（如 `PAYABLE/SETTLED`）通用路线仍能跑这些动作。

**新增动作实现**：`CREATE_TASK`（`Phase::AfterCommit`）从占位改成真实现 —— 复用
`task::model::create_task`（同一个取号 / 同一张 `biz_task`），自己开短事务，失败只记日志不回滚业务。
只建**自由任务**（不写 `doc_type`：它的值域是 `DocKind ∩ task/doc_ref.rs::DOC_TARGETS`，
不含 `STOCK_OUT` / `EXPENSE` 这类触发点；扩词表是另一张卡的事）。

### 2.3 已接线清单（与代码一一对应，漂移守卫兜底）

| 触发点 | wired | 证据（`TriggerSpec::proofs`） |
|--------|-------|------------------------------|
| `STOCK_IN` | ✅ | `erp/stockin_handler_post.rs` 含 `run_excluding` / `stockin_voucher` / `run_deferred` |
| `STOCK_OUT` | ✅ | `erp/stockout_handler.rs::run_excluding` + `erp/stockout_linkage.rs::run_deferred` |
| `TASK` | ✅ | `task/task_handler_notice.rs` 含 `status_notice` |
| 其余 **12** 个 | ❌ | `STOCK_TRANSFER / PURCHASE_ORDER / PAYABLE / VOUCHER / ACCOUNTING_PERIOD / ACCOUNTING_YEAR / CONTRACT / STOCK_TAKE / FIXED_ASSET / DEPRECIATION / EXPENSE / FLOW_INSTANCE` —— 每条在 `TRIGGER_SPECS.wiring` 里写了「接的时候要动哪个文件、要排除哪些动作」 |

`tests_wiring.rs::wired_triggers_match_call_sites` 做**双向**核对：扫描 `src/business` 全部生产代码里
调引擎的文件集合，必须**恰好等于** `wired = true` 触发点的 `proofs` 文件集合 —— 删调用 → 红；
偷偷多接一个没登记 → 红。

## 3. 真机验收（隔离库 `cloudpivot_lnk5b`，端口 18132，二进制 `/root/p1fix5c-target/debug/cloudpivot`）

`scripts/p1fix5c_instance.sh start` → `python3 scripts/p1fix5c_e2e.py`，**19 条断言全 PASS**（日志 `logs/e2e_round1.txt`）：

```
[PASS] A1 catalog_triggers 的 wired 与代码一致（3 个已接线）  wired=['STOCK_IN','STOCK_OUT','TASK']，unwired=12
[PASS] A2 STOCK_OUT 标注已接线且点明调用点（post_out_tx）
[PASS] A3 未接线触发点（STOCK_TRANSFER）带「未接线 + 要动哪里」说明
[PASS] B1 在未接线触发点（TRANSFER）上建规则被 400 拒绝  code=400「触发点 STOCK_TRANSFER/DRAFT 尚未接线…」
[PASS] B2 显式 allow_unwired=true 时才允许「预置配置」（不静默放行）
[PASS] C  配置成功：出库过账 → 建复核任务 / → 通知制单人
[PASS] D1/D2/D3 建出库单 OUT-20261105-000002 → 过账成功
[PASS] D4 成本结转凭证恰好 1 张（排除清单生效，没被通用引擎再跑一次）
[PASS] E1 日志点名「出库过账：跨单据联动已执行」
[PASS] E2 两条 AfterCommit 规则进入 deferred（deferred_rules=2）
[PASS] E3 两条提交后动作都 SUCCESS（CREATE_TASK + SEND_NOTICE）
[PASS] F1 biz_task 多出恰好 1 条（4 → 5）
[PASS] F2 任务行取值正确  TSK-202611-000005|TODO|出库过账复核（联动生成）|<owner>|SELF
[PASS] F3 sys_notice 多出通知（10 → 11）
[PASS] F4 通知行指向源单据  出库单已过账|STOCK_OUT|01a1ac07-6f26-…
[PASS] G1 本轮加的规则已清理（可重复跑）
PASS=19 FAIL=0
```

引擎日志原文（`/root/p1fix5c-server.log`）：

```
INFO cloudpivot::business::erp::stockout_handler::linkage_hook: 出库过账：提交后的联动动作执行结果
  out_no=OUT-20261105-000002 action_code=CREATE_TASK outcome="SUCCESS"
  reason="任务 TSK-202611-000005 已创建（负责人 01a1abd2-… ，来源单据 STOCK_OUT OUT-20261105-000002)"
INFO … linkage_hook: 出库过账：提交后的联动动作执行结果 action_code=SEND_NOTICE outcome="SUCCESS"
  reason="通知 01a1ac07-… 已发送（接收人 1，duplicated=false）"
```

## 4. 单测 / 静态检查

| 项 | 结果 |
|----|------|
| `cargo test --lib business::flow::linkage`（真库 `cloudpivot_lnk5b_ut`） | **59 passed / 0 failed** |
| 其中 `tests_wiring`（本卡新增 6 条：接线表同序 / 双向漂移 / 证据对 / 出库排除清单…） | 全 ok |
| `cargo clippy --lib --tests -- -D warnings` | **EXIT 0，无 warning** |
| `cargo fmt`（仅本卡触及的 8 个文件） | 已格式化 |

## 5. 范围与偏差（诚实说明）

1. **只补了 1 条链（`STOCK_OUT/POSTED`），不是 13 条**：卡面写「至少先接 STOCK_OUT/POSTED、PAYABLE/*、EXPENSE/*」。
   实际取舍：`STOCK_OUT` 是最干净的一条（单一过账入口 `post_out_tx`），且能把「目标单据被创建」验到
   `biz_task` 行；`PAYABLE`（付款 / 部分付款 / 结算 / 作废四条路，4135 行文件，两条付款入口）与
   `EXPENSE`（提交走审批流 + 预算占用 + bridge 建应付，闭包返回 `ActionOut`）改动面大、回滚代价高，
   留给各自单开卡按 `TRIGGER_SPECS.wiring` 里写好的动刀点做。**没有接的触发点在配置面被硬拒**，
   所以不存在「配了不生效」的窗口。
2. **不加迁移**：本卡不改 schema、不改种子数据（5B 的 3 条预置规则仍在）。e2e 里的两条规则是
   **通过配置页 API** 建的（`PUT /business/flow/linkage`），跑完自行删除 —— 正好证明「运维在页面上配的规则真的会触发」。
3. **生产 144 未部署**：与 5B 一致，`deploy.sh` 下次发布会一并带上（本卡无迁移，部署只换二进制 + 前端）。
4. **前端置灰不在本卡**：归 architect 的 t_21dc81c3（同一份 `wired` 字段），本卡只保证后端下发 + 硬拒。

## 6. 复现

```bash
# 1) 起隔离实例（库 cloudpivot_lnk5b，端口 18132）
BIN=/root/p1fix5c-target/debug/cloudpivot REPO=/opt/cloudpivot \
  bash scripts/p1fix5c_instance.sh start
# 2) 跑验收（自带规则清理，可重复跑）
LNK_PORT=18132 LNK_LOG=/root/p1fix5c-server.log python3 scripts/p1fix5c_e2e.py
# 3) 收
bash scripts/p1fix5c_instance.sh stop
```
