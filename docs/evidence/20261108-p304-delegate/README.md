# P3-04 委托审批：`biz_flow_delegate` + 分配时替换 + 「代 X 审批」留痕

卡面：`t_46052db9`（ARCH-8 五层降级第 ⑤ 层 · 场景 E「出差 / 休假时审批委托他人，不堵流程」）。
前置：P3-03 已交付节点兜底人（V0334），`biz_flow_task.assigned_by_rule` 列已在（值域含 `DELEGATED`）。

- 日期：2026-11-08（CST）
- 分支：`main`（工作区 `/opt/cloudpivot`，bare 远端 `172.18.10.145:/opt/git/cloudpivot.git`）
- 迁移：**V0336**（卡面 D4 写的是 V0335 —— 见下方「迁移号核对」）
- 新增错误码：**65043** `FLOW_DELEGATE_INVALID`
- 结论：**真 HTTP 69 断言 + 迁移 18 断言 + 回归 61 断言 + 门禁 13 项 全绿；本卡 19 条单测全绿**

---

## 1. 迁移号核对（卡面 D4 必须让步于事实）

`ls backend/migrations/*/V*.up.sql | sort | tail -3` 动工时给出的最大号是：

```
V0334__flow_node_fallback.up.sql          ← P3-03（已交付）
V0335__menu_title_shorten_and_sort.up.sql ← 并行卡片 MENU-FIX-1（t_95bc00b0，提交 0872b6a）
```

⇒ 本卡落 **V0336**。两个不同名的 `V0335` 会让启动直接报迁移冲突，所以**不能**照卡面写 V0335。
（P3-03 的完成留言已经预告过这件事，本卡按预警处理。）

## 2. 做了什么（四块）

### 2.1 迁移 `V0336__flow_delegate`（up / down）

| 对象 | 说明 |
|------|------|
| `biz_flow_delegate` | 11 列：`delegator_id` / `delegate_id` / `start_at` / `end_at` / `scope` / `reason` / `enabled` + 时间戳 + 软删 |
| `ck_biz_flow_delegate_period` | `end_at > start_at`（库级，绕过接口也拦） |
| `ck_biz_flow_delegate_self` | `delegator_id <> delegate_id`（自委托在库级就不存在） |
| `idx_biz_flow_delegate_active` | `(delegator_id, start_at, end_at) WHERE deleted_at IS NULL` —— 派单热路径 |
| `idx_biz_flow_delegate_delegate` | `(delegate_id, ...)` —— 「这人现在在替谁签」反查 |
| `ex_biz_flow_delegate_no_overlap` | `EXCLUDE USING gist (delegator_id WITH =, tstzrange(start_at, end_at, '[)') WITH &&) WHERE (deleted_at IS NULL AND enabled)` —— 需要 `btree_gist`（PG13+ trusted，库属主可建）；不可用时迁移走降级分支并 `RAISE WARNING`，重叠只由应用层 65043 拦（并发窗口由事务级 advisory lock 收口） |
| 菜单 | `审批委托`（`/flow/delegate`，挂在「审批流」…0860 下）+ 2 个按钮码 `business:flow:delegate:save|delete` |
| 授权 | 凡是持有 `business:flow:read` 的角色自动获得 3 个委托码（`:save`/`:delete` 的行级范围由 handler 强制「只能动自己的」） |

> 菜单 id 用 `…1240/1241/1242`：**实测** `…1221`–`…1223` 已被 `business:attachment:upload|list|delete` 占用，
> 抢占会让 `ON CONFLICT (id) DO NOTHING` **静默不插**，随后迁移自检直接炸（这是本轮唯一一次返工，已写进迁移头注释）。

### 2.2 分配时替换（`assignee.rs` 第 ③.5 步）

```
① 计划 → ② 查人 → ③ filter_candidates → **③.5 apply_delegation** → ④ 升级链 → ⑤ 兜底人 → ⑥ empty_assignee
```

- **位置理由**：放在 filter 之后 = 只作用于「已过停用 / ROBOT / 自审 / 去重」的最终名单；
  放在升级链之前 = 委托把名单清空时（受托人是发起人 / 制单人），后面照常走 P3-02/03 降级链。
- **只走 1 跳**：A→B、B→C 同时生效时落在 B 就停，并 `tracing::warn!` + 留 note（避免 A→B→A 环）。
- **委托后仍过内控 / 自审**：受托人 = 发起人 → 不许；受托人被 `internal_control::of_biz` 判为受约束（制单人）→ 不许。
  判据与引擎落待办前那次**同源**（新抽的 `internal_control::of_biz`，`of(instance)` 改成调它）。
  被剔掉的受托人落 `SKIPPED` 留痕行，`comment` 写「代 A 审批被内控剔除：<原因>」。
- **受托人不可用**（已删除 / 已停用 / ROBOT 未开 `allow_robot`）→ **委托整体不生效，仍派给委托人**：
  委托是旁路优化，坏掉时退回原行为比让整张单发不出去伤害更小（与 P3-03 兜底人配错即 422 不同）。
- **留痕四列**（不新建第二套「代办」概念）：

| 列 | 值 |
|----|----|
| `assignee_id` | 受托人 |
| `assignee_source` | **原来源**（`ROLE` / `DEPT_LEADER` / …，审计要能回答「本来按什么派给 A」） |
| `assigned_by_rule` | `DELEGATED` |
| `comment` | `代 张三 审批（委托至 2026-11-11）`（截止日按 Asia/Shanghai 折算） |

- `Finalized` 新增 `row_meta: BTreeMap<Uuid, RowMeta>`（逐行 `assigned_by_rule` + `comment`）：
  会签节点可能「A 的票被委托、C 的没被委托」，节点级单值表达不了。引擎落 PENDING / SKIPPED 行时优先读它。

### 2.3 四个接口（`delegate_handler.rs` + `delegate.rs` + `dto.rs`）

| 方法 | 路径 | 权限码 | 行级 |
|------|------|--------|------|
| GET | `/business/flow/delegate/page` | `business:flow:delegate:list` | 缺省只看自己；`view=all` 需 `business:flow:admin`，否则 403/65004 |
| POST | `/business/flow/delegate` | `business:flow:delegate:save` | 委托人只能是本人（管理员可代设） |
| PUT | `/business/flow/delegate/{id}` | `business:flow:delegate:save` | 只能改自己的（管理员可改任意） |
| DELETE | `/business/flow/delegate/{id}` | `business:flow:delegate:delete` | 只能删自己的（管理员可删任意） |

保存期校验顺序（不可换）：格式（400/65005）→ 区间纯逻辑（422/65043）→ 受托人可用性（400/65005）→ 重叠（422/65043）。
事务内 **`pg_advisory_xact_lock`（按委托人）→ 重叠检查 → 写入**。

> 踩坑记录：`pg_advisory_xact_lock` 返回 `void`，直接 `SELECT` 会让驱动 `error deserializing column 0`（真 HTTP 500）。
> 现在套一层 `SELECT count(*)::bigint FROM (SELECT pg_advisory_xact_lock(...)) AS lock_row`，并有一条单测
> `delegate::lock_tests::advisory_lock_is_readable_by_driver` 钉住它。

### 2.4 前端

- `api/business/flow-api.ts`：`pageFlowDelegates` / `createFlowDelegate` / `updateFlowDelegate` / `deleteFlowDelegate`
- `views/business/flow/delegate/index.vue`：列表（委托人 / 受托人 / 生效时间 / 范围 / 事由 / 状态徽标）+ 新增/编辑弹窗
  （受托人用 `UserSelect`、时间范围 `t-date-range-picker`、范围下拉取 `doc-kinds.ts` 的 `FLOW_BIZ_TYPES`）+ 停用/启用 + 删除
- `router/index.ts`：`flow/delegate` → `business/flow/delegate/index`（与 `sys_menu` 的 path/component 逐字一致）
- `constants/permissions.ts`：新增 `FLOW_DELEGATE_PERMISSIONS`
- `views/business/flow/task/index.vue`：**待办卡片直接显示「代 A 审批（委托至 …）」**
  （`assigned_by_rule === 'DELEGATED'` → 取 `comment` 作徽标，theme=primary 排在最前）

## 3. 验收结果（原始输出在 `raw/`）

### ① 迁移（`scripts/10_upgrade_migration.sh` → `raw/10_upgrade.txt`，18 断言全绿）

```
✅ V0336 已登记 = 1                     ✅ 迁移最大号 = 336
首启迁移日志：应用 269 个迁移
✅ 表 biz_flow_delegate 存在 = 1        ✅ 列数 = 11 = 11
✅ 两条局部索引都在 = 2                 ✅ 区间排除约束在（btree_gist 可用） = 1
✅ 三个权限码菜单都在 = 3               ✅ 委托页面菜单挂在审批流目录下 = …0860|/flow/delegate|business/flow/delegate/index
✅ 自委托被 ck_biz_flow_delegate_self 拒 = 1
✅ 区间非法被 ck_biz_flow_delegate_period 拒 = 1
✅ 同委托人重叠区间被 EXCLUDE 拒 = 1
✅ 重启日志 = 应用 0 个迁移
✅ down 后表已删 / 3 个菜单已删 / 授权行已删 = 0,0,0
✅ 升级库应用迁移数 = 应用 1 个迁移
✅ up 重放后表回来了 = 1                ✅ up 重放后排除约束也在 = 1
```

### ② 场景 E 真 HTTP（`scripts/20_e2e_scenarios.sh` → `raw/20_e2e_scenarios.txt`，**69 断言 0 失败**）

隔离实例 `:18304` + 独立空库 `cloudpivot_p304_e2e`（首启 269 迁移），夹具全部走真 HTTP：

```
✅ A 自己配委托（默认委托人 = 自己）→ 200
✅ 委托落到 biz_flow_delegate（启用）      ✅ 列表回执 is_active_now = 生效中
[委托行] …d4f2  → …d640  至 2026-11-11 09:29
✅ 待办行数 = 1（B 一行，A 没有）
✅ 待办审批人 = 受托人 B                   ✅ assigned_by_rule = DELEGATED
✅ assignee_source 保留原来源 ROLE（审计口径）
✅ comment 标「代 A 审批」 含「代 P3-04 审批人甲 审批（委托至」
✅ 委托期内 A 自己的待办 = 0
✅ B 的待办列表里带了 assigned_by_rule=DELEGATED 与「代 … 审批」comment
✅ 同期 A 的待办列表 total = 0
✅ 受托人审批 → 200  ✅ 实例生效（APPROVED）
✅ 审批留痕记在受托人名下 = P3-04 受托人乙|APPROVED|DELEGATED
✅ 委托过期后待办回到 A + assigned_by_rule=ROLE + 无「代 … 审批」comment
✅ 停用（enabled=false）后待办回到 A
✅ 重叠区间 → 422/65043（报文点名冲突区间，A 名下仍 1 档）
✅ 自委托 → 422/65043     ✅ 区间填反 → 422/65043
✅ scope=P304_SCOPE：P304_E 单据仍派给 A；P304_SCOPE 单据派给 B
✅ 受托人=发起人 → 422/65018（发起人 0 待办、整单回滚、委托人也不接）
✅ 行级：A 看全部 → 403/65004；B 删 A 的委托 → 403/65004；B 看 A 的委托 → 403
✅ 软删：deleted_at 非空、列表不再出现、重复删除 → 404
```

**委托期结束后新单回到 A** 用的是「PUT 把窗口改到过去」这条真接口路径（不是直接改库）。

### ③ 回归 P3-03 场景 A/B/C/F（`scripts/21_regression_p303_scenarios.sh` → `raw/21_regression_p303.txt`，**61 断言 0 失败**）

隔离实例 `:18305` + 独立空库 `cloudpivot_p304_regression`（首启全量迁移，含 V0336）：

```
✅ 场景 C：兜底人收到待办，assigned_by_rule=FALLBACK，单据 APPROVED（留痕 SKIPPED 行仍在）
✅ 兜底角色 3 人 → 3 行 FALLBACK
✅ 场景 A：角色 3 人 / 制单人 1 人 → 待办 2 行（不走升级链、不走兜底）
✅ 场景 B：升级链 → ESCALATED
✅ 场景 F：FORBID → 65019
✅ 场景 C 的 65018/65042 负例、R22 发布期校验 …
```

### ④ 门禁（`scripts/30_checks.sh` → `raw/30_checks.txt`，13 项全绿）

```
✅ cargo fmt 干净（-p cloudpivot -- --check）
✅ clippy --lib 仅有既有基线 error（erp/sales_order_model too_many_arguments，本卡未动该文件）
✅ delegate:: 用例全绿 / ✅ assignee::delegation_db_tests 全绿 / ✅ 本卡单测通过数 = 19 / ✅ 无 SKIP 假绿
✅ vue-tsc --noEmit exit = 0
✅ perm_menu_gate exit = 0（missing_in_menu = 0）；委托三码不在 missing/unused 任一档；库里 3 行承载菜单
✅ system/ + support/ 改动 = 0
```

### ⑤ 本卡 19 条单测（`raw/40_flow_suite_regression.txt` 里也可见）

| 用例 | 断言的东西 |
|------|-----------|
| `delegation_in_window_hands_task_to_delegate` | ① 委托期内 → 待办给 B、`assigned_by_rule=DELEGATED`、comment、source 保留 ROLE |
| `delegation_out_of_window_keeps_delegator` | ② 期外（已过期 / 未开始）→ 仍给 A、无 row_meta |
| `delegate_equal_to_initiator_falls_through_to_fallback` | ③ 受托人=发起人 → 被剔掉 + SKIPPED 留痕 + 落兜底链 |
| `delegation_scope_limits_biz_type` / `…_matches_case_insensitively` | ⑤ scope 限定 / 大小写不敏感 |
| `delegation_chain_stops_after_one_hop` | 1 跳 + 「没递归」留痕 |
| `stopped_delegate_keeps_delegator` | 受托人停用 → 回落委托人 |
| `overlap_is_rejected_with_65043` | ④ 重叠 / 相接 / 排除自己 / 停用也拦 / 软删后复用 |
| `gist_exclusion_constraint_exists` | EXCLUDE 约束真的在（btree_gist 可用时） |
| `crud_roundtrip` / `unusable_delegate_reasons` | CRUD + 受托人可用性 |
| `parse_time_*` / `normalize_scope_rules` / `normalize_reason_rules` / `validate_window_*` / `check_overlap_names_conflict_window` / `is_active_now_covers_three_states` / `advisory_lock_is_readable_by_driver` | 纯逻辑 + 驱动可读性 |

全量 flow 套件：**`cargo test --lib business::flow::` → 见 `raw/40_flow_suite_regression.txt`**
（3 条 `business::flow::linkage::*` 红是 P2-03 起的既有基线，与 P3-02/P3-03 记录一致，本卡未改 `linkage/`）。

## 4. 已知边界（如实记录，不算缺陷）

1. **升级链 / 兜底人派下来的人不再二次委托**：那已是降级结果，再套一层会让「最终派给谁」取决于链路深度，排障不可复现（1 跳原则的延伸）。若产品需要，后续卡按「降级结果再过一次委托」显式设计。
2. **受托人是机器人**：保存期直接拒（`400/65005`「受托人不能是机器人（委托要的是能签字的人）」）；运行期若历史数据里有，按「委托不生效，回落委托人」处理。
3. **btree_gist 不可用的库**：重叠只剩应用层闸（advisory lock + 65043）。迁移会 `RAISE WARNING` 说明，`gist_exclusion_constraint_exists` 用例会打印 SKIP 而不是假绿。
4. **`scope` 不做存在性校验**：`scope` 存的是 `biz_type` 字符串，没有可靠的字典表可比（`biz_flow_def.biz_type` 依赖流程配置）。保存期只校验字符集 / 长度，写错就是不生效（不会误生效）。
5. **配置变更审计**仍停在 `biz_flow_task.comment`（逐条待办）+ 本卡的 `tracing::info`；`operation_logs` 统一接入是 P3-05 的范围。

## 5. 复跑方式

```bash
# 依赖：本机 PG（alun/alun）、在跑的 cloudpivot 实例（脚本从它的 /proc/<pid>/environ 抄三把密钥）
export PGPASSWORD=alun
export PATH=/root/.cargo/bin:$PATH

# 二进制（本卡用它做隔离实例）
cd /opt/cloudpivot/backend && cargo build --bin cloudpivot && \
  cp target/debug/cloudpivot /root/p304/bin/cloudpivot

cd /opt/cloudpivot/docs/evidence/20261108-p304-delegate/scripts
bash 10_upgrade_migration.sh        # 迁移 18 断言（:18304，库 cloudpivot_p304_e2e）
bash 20_e2e_scenarios.sh            # 场景 E 69 断言（重建同一个库）
bash 21_regression_p303_scenarios.sh  # 回归 A/B/C/F 61 断言（:18305，库 cloudpivot_p304_regression）
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_p304_e2e bash 30_checks.sh
```

- 环境文件（含 JWT / 主密钥 / admin 口令）只写 `/root/p304/*.env`，**不落仓库**；原始实例日志留在 `/root/p304/`，仓库只留 `raw/*` 摘录。
- `lib.sh` 的 `Authorization` 头用 `$token` 插值（P3-02 那份被脱敏成 `Bearer ***`，直接复跑必 401 —— 这里已修正）。
