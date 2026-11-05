# P0-FIX-4A · 凭证三态（DRAFT → AUDITED → POSTED）— 证据包

执行时间：2026-11-05 14:00–14:45 CST
结论：**凭证不再是「只有 POSTED 一态」。手工凭证落 `DRAFT`，经**他人**审核（`AUDITED`）再记账（`POSTED`）才进总账；
制单人自审被 403 + 65019 拦下；反审核必须给原因且留痕；21 张生产 POSTED 凭证一张未动；不变量全绿。**

> 生产部署未在本卡执行（144 仍检出 `7d96692` 一代的代码）。本证据全部来自**隔离库 + 隔离实例**，
> 唯一的生产访问是只读快照（`01_prod_before.txt`）。

---

## 1. 卡面验收逐条对照

| # | 验收项 | 结论 | 证据 |
|---|---|---|---|
| 1 | 三态实现，状态机注册 | ✅ 三条边进 `VOUCHER_EDGES/VOUCHER_BASE`，`transition()` 核对注册表 | `03/04_e2e_*.txt` ①③⑤；单测 `p4a_voucher_state_machine_registry` / `p4a_only_un_audit_needs_reason` |
| 2 | 制单 ≠ 审核（自审被拒，贴实测） | ✅ 403 + `65019`，文案点名开关与放行办法；状态/日志都不动 | `03_e2e_run1.txt` ②；单测 `p4a_self_audit_is_forbidden` / `p4a_self_audit_gate_matrix` |
| 3 | 反审核 `AUDITED → DRAFT` 留痕 + 原因 | ✅ 无原因 400；带原因回草稿，`biz_status_log.reason` 落原文 | `03_e2e_run1.txt` ④；单测 `p4a_un_audit_requires_reason` / `p4a_un_audit_back_to_draft_keeps_reason` |
| 4 | 总账只算 POSTED（DRAFT 不进余额） | ✅ 同一张凭证：DRAFT 0 行 → AUDITED 0 行 → POSTED 才进；余额表 1405 借 / 2202 贷 各 +100 | `03_e2e_run1.txt` ⑤；单测 `p4a_only_posted_enters_ledger_and_trial_balance`（走生产 `ledger::ledger_page` / `trial_balance`） |
| 5 | 单测 ≥10 条 | ✅ `voucher_write::tests` 25 条（P0-FIX-4A 新增 15 条），连跑两轮全绿 | `05/06_unit_voucher_write_run1|2.txt` |
| 6 | 迁移 up/down/up 通过 | ✅ 12/12（含 `posted_at` 放开 NOT NULL、DRAFT 能落库、down 还原 NOT NULL 且填 NULL） | `02_migration_drill.txt` |
| 7 | 不变量全绿 | ✅ A 组（借贷恒等 A1–A5）在**含三态凭证的库**上 5/5；迁移后干净库全量 65/65；生产只读 63/63（生产版本旧、少 2 条新表检查） | 见 `09_invariants.txt` |
| 8 | git commit | ✅ 见卡内汇报 commit 号 | — |

---

## 2. 改了什么（后端）

| 文件 | 改动 |
|---|---|
| `backend/src/business/shared/state_machine.rs` | `VOUCHER_EDGES` 0 → 3 条（`DRAFT→AUDITED` / `AUDITED→DRAFT` / `AUDITED→POSTED`）；`VOUCHER_BASE` 三条 mandatory（反审核 `requires_reason`）；`VOUCHER_REASON_EDGES` = `[(AUDITED, DRAFT)]`；`VOUCHER_LOCKED` `[POSTED]` → `[POSTED, AUDITED]` |
| `backend/src/business/erp/voucher.rs` | 新增 `STATUS_DRAFT` / `STATUS_AUDITED` / `LEGAL_EDGES`；`PostVoucher` 增 `status` 字段（只许 `POSTED`/`DRAFT`，`AUDITED` 落库直接拒）；`post_voucher` 按状态决定是否盖 `posted_at/posted_by`；新增 `creator_of()`（自审闸取制单人） |
| `backend/src/business/erp/voucher_write.rs` | 手工建单改落 **DRAFT**；新增审核链：`audit_voucher` / `un_audit_voucher` / `post_audited_voucher` + 事务体 `transition_voucher_in_tx`（行锁 → 状态核对 → 自审闸 → 期间闸 → `state_machine::transition` → 回写状态列）；自审闸 `check_self_audit`（403 + 65019，复用 `erp.separation_of_duties.approve`）；三个 handler（`POST /voucher/{id}/audit|un-audit|post`，权限码 `business:erp:voucher:audit|un-audit|post`） |
| `backend/migrations/business/V0250__erp_voucher_three_state.{up,down}.sql` | `posted_at` 放开 NOT NULL（草稿没有记账时间）+ 3 个权限码 + 状态白名单自检 + 存量 POSTED 计数留证；down 还原 NOT NULL（先按 `created_at` 填 NULL）并删权限码 |
| `backend/migrations/seed/S__voucher_three_state_permissions.sql` | 授权种子（排在 `S__role_data` 之后）：跟着「能制单的人」授三码 + SUPER_ADMIN；自检三码齐全 |
| `docs/manual/10-凭证.md` | 用户手册改成三态口径（保存为草稿 → 审核 → 记账），标注截图仍是 P7 时代文案 |
| `backend/src/business/erp/{aux_tests,opening,period,pflow3b_tests,payable_payment,stockout_tests,stocktake_model,voucher_multi,year_end}.rs` | 19 处 `PostVoucher{…}` 调用点补 `status`（自动凭证 / 红冲 = `POSTED`；仅手工建单 = `DRAFT`） |

### 为什么手工建单必须落 DRAFT（而不是「可选」）

卡面要求「制单 ≠ 审核」。若建单可以直接落 `POSTED`，双人复核就有一条一人即可走完的旁路
—— 那条旁路存在一天，内控就是装饰。所以：**手工凭证只能从 DRAFT 开始**；
自动凭证（收货 / 付款 / 盘盈亏 / 年结 / 红冲）仍然直接 `POSTED`，它们的复核在各自的上游
审批链（过账审批 / 付款审批 / 作废审批）上，不是在凭证这一步。

### 为什么 `POSTED` 没有出边

已记账凭证的更正语义是「红冲开新单」。允许 `POSTED → DRAFT` 会让总账 / 余额表 / 四大报表
（`WHERE status='POSTED'`）少掉一笔**已经对外出过报表**的金额 —— 一次误点就能改掉报表口径。
`DRAFT → POSTED` 同样不存在（制单 ≠ 审核）。

---

## 3. 实测输出（原始文件）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_prod_before.txt` | 生产只读快照：21 POSTED / 0 DRAFT / 0 AUDITED；`posted_at` 仍 NOT NULL；CHECK 已允许 4 态；3 个在用账号；生产不变量 63/63 | 基线 |
| `02_migration_drill.txt` | 隔离库 `cloudpivot_p4a_drill`：up → 重复 up（幂等）→ down → 再 up | 全绿 |
| `03_e2e_run1.txt` / `04_e2e_run2.txt` | 隔离实例 `127.0.0.1:18095`（库 `cloudpivot_p4a_4a`）真 HTTP：建单 / 自审 403 / 换人审核 / 反审核 400+200 / 记账 / 终态 409 / 草稿直记 409 / 封账期 400 | **26/26 × 2 轮** |
| `05/06_unit_voucher_write_run*.txt` | `cargo test --lib business::erp::voucher_write::tests`（真库 `cloudpivot_p4a_ut`）两轮 | **25 passed / 0 failed × 2** |
| `07/08_unit_wide_run*.txt` | `state_machine` + `ledger` + `report` + `voucher`（受影响面）两轮 | **104 passed / 0 failed**（两轮同数） |
| `09_invariants.txt` | A 组（借贷恒等）在含三态凭证的库上；迁移后干净库全量；生产只读 | 5/5、65/65、63/63，BLOCKER 0 / WARN 0 |
| `10_ledger_vs_status.txt` | **DRAFT 不进余额的查询对比**：同库 `DRAFT` 2 张 / 4 行分录 / 借 2.0000，总账谓词（`status='POSTED'`）只认 200.0000；外加同一张凭证的 5 行状态轨迹 | 账实对应 |

---

## 4. 已知边界（不做 / 留给后续卡）

1. **生产未部署**：V0250 + 代码随下次 `deploy.sh` 上线；`posted_at DROP NOT NULL` 会在那次部署生效。
   在部署前，生产库仍是 `posted_at NOT NULL`，与本代码不兼容（写 DRAFT 会 23502）——
   **代码与迁移必须同批上线**，这是本卡对发布顺序的唯一硬要求。
2. **前端文案**：新增弹窗按钮仍写「保存并记账」（实际只存草稿）。接口契约无需改，
   文案另开卡 `t_248c4ea8`（frontend-developer）。
3. **不做的事**：凭证作废（`CANCELLED`）；草稿删除；审核链挂流程引擎（4B 说「若挂，请下发
   `approval_instance_id`」—— 本卡**没有**把审核链挂流程引擎：双人复核是「换一个账号点审核」，
   不需要审批实例，也不该为此多一层待办）。
4. **授权口径**：新三码授给「已经能制单的角色 + SUPER_ADMIN」。生产实测只有 `SUPER_ADMIN` 持有
   `voucher:create`，所以生产上就是超管独有；财务经理 / 财务总监要用这三动作，由管理员在角色页勾选
   （迁移不替人做「扩权」决定）。
