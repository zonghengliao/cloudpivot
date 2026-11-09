# E2E-10 · 流程引擎域演练（定义 / 实例 / 审批 / 委托 / 自审监控 / 联动 / ARCH-8 五层降级）

- 卡：`t_cb6decc3`（父卡 `t_3fa6665a` E2E-09）
- 日期：2026-11-09
- 环境：**145 测试机** `cloudpivot_dev`，真实 HTTP `127.0.0.1:8034`（新二进制，见下「二进制口径」），psql 直查同库
- 结论：**376 条断言全绿（A 228 + B 148）、FAIL=0**；20 组 DB 证据/勾稽 0 差异；演练数据按 E2E10 前缀自清，残留 0
- 当场修 **1 个 P1**（commit `2acd212e`）：驳回「退到指定节点」目标不存在时**先落副作用再校验** → 单据停在 `PENDING` + 节点 `REJECTED` + 0 待办（界面无按钮、只能改库）

---

## 1. 一条命令复跑（在 145 上）

```bash
# 0) 备好夹具（角色/用户/权限/流程定义）→ 6 大页面 + 引擎能力
cd /tmp/e2e10
E2E10_WORK=/tmp/e2e10/run E2E10_BASE=http://127.0.0.1:8034 python3 e2e10_a.py   # S0-S4：定义/发起/审批中心/签法/驳回
E2E10_WORK=/tmp/e2e10/run E2E10_BASE=http://127.0.0.1:8034 python3 e2e10_b.py   # S5-S9：委托/自审/降级/联动/状态边
E2E10_WORK=/tmp/e2e10/run python3 e2e10_db_evidence.py                         # 20 组 DB 证据与勾稽（只读）
E2E10_WORK=/tmp/e2e10/run python3 e2e10_cleanup.py                             # 按外键顺序自清（不 DROP/TRUNCATE）
```

脚本源在仓库 `scripts/verify/e2e10_{lib,a,b,db_evidence,cleanup}.py`（stdlib，无第三方依赖）。断言逐条落 `asserts.log`（`PASS/FAIL<TAB>段号<TAB>断言`）。

### 二进制口径（重要）

145 上 `cloudpivot-test.service` 受控（禁止在无人值守会话里 restart），因此本卡用**同一份 env + 同库**在 `127.0.0.1:8034` 起了一个演练实例：

```
NIK_SERVER_LISTEN=127.0.0.1:8034  /opt/cloudpivot/backend/target/debug/cloudpivot
```

- 二进制 = 145 `HEAD 2acd212e` 的 debug 构建（`cargo build --bin cloudpivot` 1m16s 成功）
- 演练完已停：`8034` 监听行数 = 0（`ss -ltn | grep -c ":8034 "` → 0）
- `cloudpivot_dev` 允许写演练数据；全程未连 144，未 DROP/TRUNCATE，未改已应用迁移，未动他人数据

---

## 2. 演练范围（6 个页面 + 引擎能力）

| # | 页面 / 能力 | 覆盖要点 |
|---|---|---|
| ① | 审批流程定义（设计器 / 节点配置 / 条件分支） | 建定义→存图→版本校验→发布；7 节点/8 边落库；条件边 DSL 与默认边；RATIO 比例；发布校验 5 类负例（环 65011 / 缺 START 65010 / 条件缺默认边 65013 / 审批节点隐式分叉 65012 / RATIO 无比例 65005）；PUBLISHED 版本只读 65022；另存草稿 v2；另存为（复制）；下线 DISABLED→启用；**版本冻结**（老实例旧图、新单新图） |
| ② | 发起审批 | 发起→实例 PENDING、实例号 `FLW-`、绑定版本；表单金额驱动条件分支 |
| ③ | 审批中心（待办/已办/我发起） | todo/done/my/page 四列表真实命中；详情（当前激活节点、发起人实时姓名、timeline）；撤回 200→重复撤回 65024→未结算待办 0；**三种签法** ALL(3 人) / ANY / RATIO(0.66)；串行 3 节点；并行 AND 分叉/汇聚（一臂不汇、两臂才过）；条件三档路由（5000/50000/500000）；**驳回四目标** PREV / 指定节点（admin 200、非 admin 403+65004、不存在 65025 且无副作用）/ INITIATOR（RESUBMIT 待办）/ NONE(REJECTED) |
| ④ | 审批委托（出差委托） | 建委托（本人，3 天窗口，事由）；is_active_now；期内待办派受托人 `assigned_by_rule=DELEGATED` + 「代 X 审批（委托至…）」；受托人审批留痕；改过期窗口→回本人 `ROLE` 且无代审批文案；自委托 65043；起止反了 4xx；同委托区间重叠 65043；非管理员代他人建 403；停用→is_active_now=false；删除→软删（deleted_at 非空）+ 列表消失 |
| ⑤ | 自审监控 | SKIP（发起人在角色内 → SKIPPED 留痕 + 其余 2 人待办）；ALLOW（阈值内可自审 → is_self_approval=t、审计 `FLOW_SELF_APPROVAL_ALLOWED/VOTED` 各 1）；**金额上限**（配置改 1000 后 5000 被 65044 拦，库内 0 半截实例，恢复原值）；FORBID（422/65019，0 半截实例）；月度报表 `stats/self-approval`（instances/amount/by_node/by_person；非法 month 65005）；overview / efficiency / node-bottleneck 四统计接口 |
| ⑥ | 联动规则（跨单据/触发状态/白名单动作） | 触发点白名单 **16**、动作白名单 **11**、接线状态（16/16 wired）；4 类非法配置 400；`confirm` 二次确认；预览（seq 排序 / phase / next_run_hint）；TASK→DOING 真事件：通知落库 1 条（biz_id=任务 id）+ 扇出到负责人；重复置 DOING 被状态机拒 → 通知仍 1 条（幂等）；停用→回落旧逻辑仍 1 条（不丢）；启用→新任务再 1 条；`GENERATE_VOUCHER` 配到 VOUCHER 触发点被拒（防凭证生凭证）；规则软删；**状态流转边**：必需边禁用需 `confirm_critical`（报文点名业务后果）、改非必需边**走审批**（applied=false + 实例号）→ 审批通过后 effective_edges 生效（configured=true / 文案）

## 3. 结果总览（断言分段）

| 段 | 内容 | 断言 | 结果 |
|---|---|---|---|
| S0 | 夹具：14 角色 / 19 用户 / 权限（真实授权接口）/ 基线 | 63 | 全 PASS |
| S1 | ① 流程定义 / 设计器 / 发布校验 / 版本管理 | 48 | 全 PASS |
| S2 | ②③ 发起 + 审批中心（四列表 / 详情 / 撤回） | 35 | 全 PASS |
| S3 | ③ 签法 ALL/ANY/RATIO + 串行 + 并行 + 条件三档 | 55 | 全 PASS |
| S4 | ③ 驳回四目标 + 无副作用负例 | 27 | 全 PASS |
| S5 | ④ 委托（有效期内/外、重叠、越权、停用、删除） | 37 | 全 PASS |
| S6 | ⑤ 自审 SKIP/ALLOW/阈值/FORBID + 报表 | 35 | 全 PASS |
| S7 | ARCH-8 ②③ 升级链 / 兜底人 / 到顶无人 65018 | 19 | 全 PASS |
| S8 | ⑥ 联动规则（白名单/预览/真事件/幂等/启停/软删/防凭证生凭证） | 40 | 全 PASS |
| S9 | ⑥ 状态流转边（必需边保护 + 改配置走审批后生效） | 17 | 全 PASS |
| **合计** | | **376** | **PASS=376 / FAIL=0** |

关键步骤（节选，全量见 `run_a.log` / `run_b.log`）：

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| 存图 7 节点/8 边 | `PUT /business/flow/version/{vid}/graph` | 200 | `biz_flow_node=7`、`biz_flow_edge=8`、assignee=3 | 通 |
| 发布 | `POST /business/flow/def/{id}/publish` | 200 | `def.status=PUBLISHED`、`current_version_id=v1` | 通 |
| 改已发布版本 | `PUT .../{vid}/graph` | 400 | — | 65022 |
| 环/缺 START/缺默认边/隐式分叉 | `POST .../publish` | 400 | — | 65011/65010/65013/65012 |
| 会签 ALL | 3 人逐个 approve | 200 | 1/3、2/3 均 PENDING；3/3 APPROVED，approved_count=3 | 通 |
| 或签 ANY | 1 人 approve | 200 | 其余 2 条 CANCELLED | 通 |
| 定额 RATIO | 2/3 approve | 200 | ratio=0.660，APPROVED，剩 1 条 CANCELLED | 通 |
| 并行 AND | 先签一臂再签另一臂 | 200 | 汇聚 j=PENDING→APPROVED，两个 END 都 APPROVED | 通 |
| 条件三档 | 5000/50000/500000 | 200 | `taken_edge_key=e_low / e_c2+e_mid / e_c2+e_high` | 通 |
| 驳回 PREV | n2 reject | 200 | n1 round=2 ACTIVE、n2 REJECTED、round_total=2 | 通 |
| 退回指定节点（admin） | n2 reject `return_to=n1` | 200 | n1 round=2 ACTIVE | 通 |
| 退回不存在节点 | n2 reject `return_to=n_no_such` | 400 | **n2 仍 ACTIVE、实例仍 PENDING（修复后）** | 65025 且无副作用 |
| 退发起人 | `return_to=INITIATOR` | 200 | RESUBMIT 待办给发起人 + 「重新提交」文案 | 通 |
| 委托期内 | 发起（u_init） | 200 | 待办 assignee=u_pa、rule=DELEGATED、comment「代 … 审批（委托至 …）」 | 通 |
| 自审 ALLOW | 本人审批 | 200 | `is_self_approval=t`，审计 2 条 | 通 |
| 自审超限 | 上限=1000，金额 5000 | 422 | 65044，库内 0 半截实例 | 通 |
| 升级链 | 单人角色=制单人 | 200 | 2 条 ESCALATED（from=E2E10_MAKER，reason=SKIP_SELF_APPROVAL） | 通 |
| 兜底人 | 无上级 + fallback | 200 | rule=FALLBACK → APPROVED | 通 |
| 到顶无人 | 无上级/无兜底 | 423 | 65018，0 半截实例 | 通 |
| 联动真事件 | TASK TODO→DOING | 200 | `sys_notice` 1 条（biz_id=任务 id），receiver 含负责人 | 通 |
| 停用规则后再触发 | toggle off → DOING | 200 | 仍 1 条（回落旧逻辑，通知不丢） | 通 |
| 改状态边 | `PUT /business/flow/state-edge` | 200 | `applied=false` + 变更单 PENDING → 审批后 effective_edges `configured=true` | 通 |

## 4. 勾稽验证（20 组，全量见 `db_evidence.log`）

| # | 勾稽项 | 期望 | 实际 |
|---|---|---|---|
| ① | 定义 / 版本 / 节点 / 边 | 20 / 21 / 88 / 77 | 一致 |
| ②③ | 实例 / 待办状态分布 | 与 376 条断言逐条一致 | 一致 |
| ④ | **终态实例还挂 PENDING 待办**（孤儿） | 0 | **0** |
| ⑤ | 实例节点 `approved_count` vs 待办 APPROVED 行数差异 | 0 | **0** |
| ⑥ | 签法落库：ALL=3、RATIO=0.660/2、ANY=1 | 与断言一致 | 一致 |
| ⑦ | 版本冻结：老/新实例各自 `version_no` + 节点名快照 | 旧单无 `n_extra`，新单有 | 一致 |
| ⑧⑨ | 驳回轮次 / RESUBMIT 待办 | round=2 + 1 条 RESUBMIT | 一致 |
| ⑩⑪ | 委托留痕 DELEGATED + 代审批；配置行软删 | 有值 / deleted=true | 一致 |
| ⑫⑬ | 自审 SKIP/ALLOW 行 + 审计成对 | 各 1 行/单 | 一致 |
| ⑭ | 降级：ESCALATED(from/reason) 与 FALLBACK | 各有留痕 | 一致 |
| ⑮ | 联动通知幂等：每任务恰好 1 条 | 1 | 一致 |
| ⑯⑰⑱ | 规则软删 / 状态边配置行 / 配置变更单 | 一致 | 一致 |
| ⑳ | **ACTIVE 审批节点却 0 条 PENDING 待办** | 0 | **0**（修复后） |

## 5. 问题清单

| # | 问题 | 级别 | 复现步骤 | 影响 | 建议 |
|---|---|---|---|---|---|
| 1 | **驳回「退到指定节点」目标不存在时副作用已落库**：`engine::vote` 先 `end_node_row(REJECTED)` + 取消其余待办，`apply_return` 才校验目标；handler 是「事务提交后再 drain 错误」 | **P1（已修）** | 发起→签到 n2→`POST /task/{id}/reject {"return_to":"n_no_such","comment":"..."}` → 400/65025，但 `biz_flow_instance_node.n2=REJECTED`、实例仍 `PENDING`、0 条待办 | 单据卡中间态，界面无按钮，只能改库（AGENTS.md 约定 11） | 投票前用快照预校验节点 key（commit `2acd212e`）；回归断言「n2 仍 ACTIVE / 实例 PENDING」 |
| 2 | 卡片写「15 个触发点」，代码实为 **16**（TST-01 补了 `PURCHASE_RETURN`） | P4（文档） | `GET /business/flow/linkage` → `triggers.length=16` | 卡面口径过期，易误判 | 下次卡片写「以 `spec::TRIGGERS` 为准」 |
| 3 | 卡片写「定额（QUORUM）」，实现是 `RATIO + approve_ratio`（无 QUORUM 枚举） | P4（命名） | 存图 `approval_mode=RATIO, approve_ratio=0.66` | 沟通口径不一致 | 卡片/文档统一叫「按比例（可当定额用）」 |
| 4 | 委托 `PUT` 是全量覆盖且 `reason` 可空 → 只改窗口会把事由清空（本次 ⑪ 证据行 reason=<NULL>） | P4 | 建委托→`PUT` 不带 reason | 审计可读性下降 | 前端总是回填；或后端「缺省保留原值」 |
| 5 | 待办被处理后 `comment` 被审批意见覆盖，「代 X 审批（委托至 …）」文案消失（`assigned_by_rule=DELEGATED` 仍在） | P4 | 委托单审批通过后查 `biz_flow_task.comment` | 事后只能凭 rule 列判断 | 委托标记建议入独立列（如 `delegate_from`），别再复用 comment |
| 6 | 前端守卫 `flow-biz-link.test.ts` 自带期望值过期（写死 15，实际后端/前端都已 16：SO-14 加 `RECEIPT_REFUND`） | **P4（已修）** | `npx tsx --test tests/unit/flow-biz-link.test.ts` → `16 !== 15` | 守卫常态红，真回归被淹没 | 期望值改 16（同步注释），31/31 转绿 |
| 7 | 145 `cargo test --lib` 链接期 **SIGKILL(OOM)**（7G 内存） | P4（环境） | 145 上跑 `cargo test --lib business::flow::linkage` | 真库/单测回归只能放 99 | 与既往口径一致：单测在 99 跑 |

## 6. 修复记录

| 文件 | 改动 | commit |
|---|---|---|
| `backend/src/business/flow/engine.rs` | `vote()` 在**写票之前**用 `load_snapshot` 预校验 `ReturnTo::Node(key)`，不存在直接 `ActionOut::deny(400/65025)`，避免「先 REJECTED 再报错」的卡单 | `2acd212e` |
| `frontend/tests/unit/flow-biz-link.test.ts` | 守卫期望值 15 → 16（SO-14 加 `RECEIPT_REFUND` 后一直漏改），同步注释；3 个守卫文件 31/31 转绿 | 见本卡证据提交 |
| `scripts/verify/e2e10_lib.py` / `e2e10_a.py` / `e2e10_b.py` / `e2e10_db_evidence.py` / `e2e10_cleanup.py` | 本卡演练脚本（376 断言 + 20 组 DB 证据 + 自清） | `2acd212e` + 证据提交 |

修复验证：修复前同一请求实测「n2=REJECTED / 实例 PENDING / 0 待办」（run5 证据），修复后 **S4 27 条断言全绿**，DB 勾稽 ⑳「ACTIVE 节点 0 待办」= 0。

### 历史问题回归（守卫测试 TST-01 修的 3 条 + 引擎单测）

| 命令（99 上，145 内存不足） | 结果 |
|---|---|
| `cargo test --lib business::flow::linkage` | **83 passed / 0 failed**（含 TST-01 修的 `trigger_specs_cover_whitelist_in_order` / `wired_triggers_match_call_sites` / `catalog_triggers_carry_wired_flag`） |
| `cargo test --lib business::flow::engine` | **18 passed / 0 failed**（含 `no_accumulator_on_vote_counts` 等） |
| `npx tsx --test tests/unit/{target-guard,flow-assignee-guard,flow-biz-link}.test.ts` | **31 passed / 0 failed**（修复期望值后） |
| `rustfmt +1.94.1 --edition 2024 --check backend/src/business/flow/engine.rs` | 无 diff |

## 7. 边界与残留

- 只做 **API + DB 层**演练，未做浏览器截图（145 nginx 静态目录为旧构建）；页面契约由响应 JSON 原文（`raw/*.json`）钉住。
- 演练数据按 E2E10 前缀自清：`flow_def / flow_instance / flow_task / delegate / linkage / task / user / role / state_edge` 全 0（`cleanup_after.log`）。
- 保留：`operation_logs`（`FLOW_SELF_APPROVAL_*` 等审计）与 `audit_logs`/`sys_change_log`（按设计不可删）。
- 未 DROP/TRUNCATE、未改已应用迁移、未动他人数据与他人规则。
