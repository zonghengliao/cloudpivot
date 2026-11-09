# E2E07-FIX-1 · ALL 会签 + 失效账号 = 流程永久卡死（全系统扫描）

卡：`t_8733a7a0` ｜ 级别 P2（系统性） ｜ 环境：145 `cloudpivot_dev`（真后端 `127.0.0.1:8033`）+ 99 仓

---

## 0. 一句话结论

**「APPROVAL 节点 + 角色 + ALL」是一个系统性错配，不是合同的个案。**
145 上 8 个 PUBLISHED 流程共有 **13 个 ALL 节点**：其中 **7 个**是「角色型审批」被 V0339
按「候选人数 ≥2 ⇒ 冻结成 ALL」误冻成「委员会」（合同 2 / 采购订单 2 / 采购申请 1 / 采购退货 1 / 应付付款 1），
另外 **6 个**是真·跨角色会签（`COUNTERSIGN`，语义正确，未动）。

修法分两层：
1. **纠正错配**：迁移 `V0359` 把 7 个「角色型 + ALL」全部改成 **ANY**（岗位任一人签字即可）——
   合同 `n_pm` / `n_pm_hi` 就此不再需要「3 个采购经理都点一遍」。
2. **防止复发**：发布校验新增 **R28 / `65048`**（ALL 节点必须有超时小时或兜底人），
   环境侧新增**不变量脚本** `scripts/verify/flow_countersign_invariants.sh`（4 条断言，管存量库 +
   账号可用性），清理工具 `scripts/datafix/flow_broken_role_accounts.sh`（先留档再摘除坏号账号的角色授权）。

**关键口径**：`APPROVAL` 的语义是「这道关由一个人签字即可」，多人共识必须显式建 `COUNTERSIGN`。
`timeout_action` 有值但 `timeout_hours` 为空 = 等于没配（`flow::timeout` 的扫描谓词是 `timeout_hours IS NOT NULL`）。

---

## 1. ① 全系统 ALL 会签节点清单（145 扫描，改前）

命令（本卡 ① 的 SQL，列名按真实 schema 调整：`biz_flow_node.version_id` → `biz_flow_def_version.is_current`）：

```sql
SELECT d.flow_code, v.version_no, n.node_key, n.node_name, n.node_type,
       n.assignee_type, n.approval_mode, n.timeout_hours, n.timeout_action,
       (SELECT string_agg(DISTINCT r.role_code, ',') FROM biz_flow_node_assignee a
          JOIN sys_role r ON r.id = a.ref_id WHERE a.node_id = n.id) AS role_codes,
       (SELECT count(DISTINCT ur.user_id) FROM biz_flow_node_assignee a
          JOIN sys_user_role ur ON ur.role_id = a.ref_id
          JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL AND u.status = 1
         WHERE a.node_id = n.id AND a.assignee_type='ROLE') AS usable_members
  FROM biz_flow_def d
  JOIN biz_flow_def_version v ON v.id = d.current_version_id
  JOIN biz_flow_node n ON n.version_id = v.id
 WHERE d.deleted_at IS NULL AND d.status='PUBLISHED' AND n.approval_mode='ALL'
 ORDER BY d.flow_code, n.node_key;
```

| # | flow_code | node_key | node_type | 审批人 | 候选(可用) | timeout | 处置 |
|---|---|---|---|---|---|---|---|
| 1 | contract_approve | n_pm | APPROVAL | ROLE PURCHASE_MANAGER | 3(3) | 24h/REMIND | **改 ANY** |
| 2 | contract_approve | n_pm_hi | APPROVAL | ROLE PURCHASE_MANAGER | 3(3) | 24h/REMIND | **改 ANY** |
| 3 | po_approve | n_dept | APPROVAL | ROLE DEPT_LEADER | 3(3) | 24h/REMIND | **改 ANY** |
| 4 | po_approve | n_pm | APPROVAL | ROLE PURCHASE_MANAGER | 3(3) | 24h/REMIND | **改 ANY** |
| 5 | pr_approve | n_pm | APPROVAL | ROLE PURCHASE_MANAGER | 3(3) | 24h/REMIND | **改 ANY** |
| 6 | prt_approve | n_pm | APPROVAL | ROLE PURCHASE_MANAGER | 3(3) | 24h/REMIND | **改 ANY** |
| 7 | payable_pay | n_dept | APPROVAL | ROLE DEPT_LEADER | 3(3) | 24h/REMIND | **改 ANY** |
| 8 | po_approve | n_board | COUNTERSIGN | ROLE PURCHASE_MANAGER + GENERAL_MANAGER | 6(6) | 48h/REMIND | 保留 ALL（跨角色会签） |
| 9 | payable_pay | n_board | COUNTERSIGN | ROLE FINANCE_DIRECTOR + GENERAL_MANAGER | 5(5) | 48h/REMIND | 保留 ALL |
| 10 | budget_approve | n_fin_gm | COUNTERSIGN | ROLE FINANCE_MANAGER + GENERAL_MANAGER | 7(7) | 48h/REMIND | 保留 ALL |
| 11 | budget_approve | n_gm_fd | COUNTERSIGN | ROLE GENERAL_MANAGER + FINANCE_DIRECTOR | 5(5) | 48h/REMIND | 保留 ALL |
| 12 | expense | n_board | COUNTERSIGN | ROLE FINANCE_MANAGER + GENERAL_MANAGER | 7(7) | 48h/REMIND | 保留 ALL |
| 13 | voucher_reverse | n_cs | COUNTERSIGN | ROLE FINANCE_MANAGER + FINANCE_DIRECTOR | 4(4) | 48h/REMIND | 保留 ALL |

### ② 每个节点的业务判断（判据，不是感觉）

- **#1–#7 必须改 ANY**：`node_type = APPROVAL` 的语义就是「这道关一个人签字即可」。
  「采购经理审批」「部门主管审批」是**岗位**，不是委员会 —— 派给 3 个成员时业务要的是
  「采购经理这边有人审过」，不是「三个采购经理都点头」。它们是 V0339 冻结逻辑的副产品
  （V0339：候选人数 ≥2 ⇒ 写 `ALL` 以「保持历史行为」，而历史行为来自引擎老口径 `NULL → ALL`）。
  → 与 E2E06-FIX-1（V0355）对预算 `n_dept` 的处理同一口径。
- **#8–#13 必须保留 ALL**：它们是 `COUNTERSIGN`，且**每个节点关联的是不同角色**
  （财务经理 + 总经理 / 财务总监 + 总经理 …）—— 这是「两个岗位各投一票」的共识会签，
  改成 ANY 会变成「财务经理一人同意就算总经理也同意」，是审批强度下降，不能动。
  它们的兜底是 `timeout_hours=48 + REMIND`（P3 催办；`max_remind_times=2`），另有
  `empty_assignee=FALLBACK_ADMIN` 防「角色被清空」。

> 说明：本卡**不**顺手改「部门维度错配」（`po_approve.n_dept` / `payable_pay.n_dept` 仍是
> 全局 `DEPT_LEADER` 角色，A 部门下单要所有部门主管签）。那是 V0355 对预算做过的另一件事，
> 涉及「发起人所在部门 vs 全局角色」的业务决策，不在本卡判据里；本卡只保证它们不再因为
> ALL 而卡死。已在「残留」里登记。

---

## 2. ③ 失效账号清单与处置

扫描 SQL（本卡 ② 的口径，`sys_user.password` 长度 < 20 或占位符即「可疑」）：

```sql
SELECT r.role_code, u.username, u.status, length(u.password) AS pwd_len,
       CASE WHEN u.password IN ('x','','CHANGE_ME') OR length(u.password) < 20
            THEN '⚠ 密码可疑' ELSE 'OK' END AS pwd_check
  FROM sys_user u
  JOIN sys_user_role ur ON ur.user_id = u.id
  JOIN sys_role r ON r.id = ur.role_id
 WHERE r.deleted_at IS NULL AND u.deleted_at IS NULL
 ORDER BY r.role_code, u.username;
```

**145 实测（2026-11-09 15:2x）**：坏号 = **0 行**。

| 角色 | 账号 | 当时的实际情况 |
|---|---|---|
| PURCHASE_MANAGER | `p3a_0003` | E2E-07 演练时**为了让会签凑齐票**，把它的口令哈希重置成了 `e2e01_appr` 的哈希（`e2e07_contract_a.sh` 里的夹具 hack）。所以扫描已看不出坏号 —— 但**「为了让流程走通去改账号密码」本身就是这个 bug 的症状** |
| DEPT_LEADER | `p3a_0002` | 同上（E2E-06 时期的占位符 `'x'`，后来被重置） |

**处置结论**：
1. **根因侧**：`n_pm` 等 7 个节点改 ANY 后，「角色里有个坏号」不再导致卡死 —— 任务 ⑤ 想解决的
   「只能从角色移除」变成了「不再需要为会签凑人头」。
2. **工具侧**：新增 `scripts/datafix/flow_broken_role_accounts.sh`（dry-run 默认 / `--apply` 执行），
   把**停用 / 软删 / 口令占位符**的账号从角色授权里摘掉（只删 `sys_user_role` 一行，
   **不删用户、不碰 SUPER_ADMIN**，删前整行 JSONB 写进 `biz_data_fix_backup`，可一行 SQL 还原）。
   145 现在实跑 = 0 行（无事可做）；**有牙齿的证明**见 `datafix_145_teeth.txt`（临时停用 `p3a_0004`
   → dry-run 列出 → `--apply` 摘除并留档 → 按留档 JSONB 反向还原 → 再跑 0 行）。
3. **不再用「改密码」当夹具**：`e2e07_contract_a.sh` 里那段重置 `p3a_0003` 口令的注释仍保留，
   但它已不是流程走通的必要条件（`n_pm` 是 ANY）。后续演练不要再用这种手法掩盖配置缺陷。

---

## 3. 改动清单

| 文件 | 作用 | 为什么需要 |
|---|---|---|
| `backend/migrations/business/V0359__flow_all_role_approval_to_any.{up,down}.sql` | 7 个「角色型 + ALL」节点改 ANY；旧版归档、发新版（快照 + graph_hash 在库内按 Rust 同口径现算） | 已发布版本只读（`flow/handler.rs` 红线 1），只能「归档旧版 → 发新版」；同时必须让**行**与**快照**一致，否则每天 03:30 的图自检会误报「行被直接修改」 |
| `backend/src/business/flow/graph.rs` | 发布校验 **R28**：ALL 节点必须有 `timeout_hours>0` 或兜底人；+4 条单测 | 发布期挡住「新造一个会永久卡死的节点」。为什么不只靠脚本：脚本是事后巡检，用户点「发布」时就得知道 |
| `backend/src/shared/error_code.rs` | 新错误码 `FLOW_ALL_COUNTERSIGN_NO_GUARD = 65048` + 码表断言 43 个 | 项目硬规矩：65xxx 段码值登记 + 唯一性断言，避免与 ARCH-5 §18.2 预留段冲突 |
| `scripts/verify/flow_countersign_invariants.sh` | 环境侧 4 条不变量（无兜底 / 角色型误配 ALL / 角色坏号 / 候选人为 0） | 发布期查不出「候选人明天离职 / 口令是占位符」，只能按库巡检；也是本卡 ⑥ 的门禁 |
| `scripts/verify/e2e07_fix1_contract_any_probe.sh` | 真 token 真 HTTP 验收：建单→提交→**只批一票**→SIGNED | 卡面 ④ 硬验收 |
| `scripts/verify/e2e07_fix1_flow_checks.sql` | 8 条库内事实检查（快照自检 / 合同 ANY / 预算回归 / 会签仍 ALL / 无残留中间态） | 卡面 ⑤（预算回归）+ 防止迁移把 hash 算歪 |
| `scripts/datafix/flow_broken_role_accounts.sh` | 摘除坏号账号的角色授权（dry-run / --apply / 留档可还原） | 卡面 ⑤ 的「从角色移除」 |
| `scripts/verify/README.md` | 新脚本的用法与判据 | 门禁类脚本必须有文档，否则下一个人不知道 rc=2 是基础设施 |
| `docs/evidence/e2e07-fix1/*` | 证据（见下表） | — |

> 本卡跨 flow 模块 + 迁移 + 脚本，确实 >2 文件：**代码（R28）管新图，迁移管存量图，脚本管存量数据**，
> 三者缺一都会留下「这次修了、下次换个流程再来一遍」的缺口。

---

## 4. 证据（全部是本目录下的真实输出）

| 文件 | 内容 |
|---|---|
| `migration_roundtrip_145.txt` | V0359 在 145 的改前（归档版本里 n_pm/n_pm_hi/n_dept = ALL）/ 改后（ANY）/ 会签节点未动 / **同一事务内 down → up → 再 up（幂等）→ ROLLBACK** 往返预演，hash 与提交时逐字一致 |
| `invariants_145_green.txt` | 不变量脚本在 145 的实际输出：①–④ 全绿 rc=0，且打印 ALL 节点清单 |
| `invariants_145_teeth.txt` | **牙齿证明**：故意把 `contract_approve.n_gm` 改成 ALL 且清空超时、把 `GENERAL_MANAGER` 成员 `p3a_0004` 口令改成 `'x'` → ①②③ 同时报红 rc=1 并逐条点名；还原后 rc=0 |
| `contract_any_probe_145.txt` | 卡面 ④：5 万合同 → 提交 → n_pm 生成 **3 条**待办（改造前 3 人全会签）→ e2e01_appr **只批 1 条** → 实例 APPROVED、合同 SIGNED、其余两条 `CANCELLED`，**PASS=15 FAIL=0** |
| `leftover_cleanup_145.txt` | 第一次探针因 `biz_type` 写错（应为 `CONTRACT_APPROVE`）中止，留下一张 `HT-20261109-000053` 停在 PENDING_APPROVAL；用同一路径单票推完 → APPROVED/SIGNED，不留中间态 |
| `datafix_145_teeth.txt` | 清理脚本：临时停用 → 列出 → 摘除 + 留档 → 按留档还原 → 0 行 |
| `flow_checks_145.txt` | 8 条库内事实检查全 PASS（含 ① 所有当前版本 `canonical(snapshot)==graph_hash`、③ 预算审批仍是 v2/INITIATOR_SUPERIOR/ANY） |

### 复现命令（145）

```bash
# 1) 不变量（4 条）
ssh root@172.18.10.145 'set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a; \
  bash /opt/cloudpivot/scripts/verify/flow_countersign_invariants.sh cloudpivot_dev; echo rc=$?'

# 2) 真 token 验收（建单 → 提交 → 只批一票）
ssh root@172.18.10.145 'bash /opt/cloudpivot/scripts/verify/e2e07_fix1_contract_any_probe.sh; echo rc=$?'

# 3) 库内事实检查（含预算回归）
ssh root@172.18.10.145 'set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a; \
  PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -At \
  -f /opt/cloudpivot/scripts/verify/e2e07_fix1_flow_checks.sql'

# 4) 清理坏号账号（默认 dry-run）
ssh root@172.18.10.145 'set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a; \
  bash /opt/cloudpivot/scripts/datafix/flow_broken_role_accounts.sh cloudpivot_dev'
```

---

## 5. 残留 / 已知边界（如实登记）

1. **144 生产未部署**：本卡含新迁移 V0359 + 新错误码，下次发布（`scripts/deploy/deploy.sh`）随
   `origin/main` 生效。生产库若也有 `APPROVAL+ROLE+ALL`，迁移同样会「归档旧版 → 发新版」修掉。
2. **部门维度错配仍在**：`po_approve.n_dept` / `payable_pay.n_dept` 改 ANY 后不再卡死，
   但仍是「全局 DEPT_LEADER 角色」——A 部门的单子任何部门主管都能批。这与 V0355 给预算做的
   「发起人所在部门 + ANY」不同口径，属另一个待评估项（本卡未扩大范围）。
3. **6 个真会签节点的兜底是「48h 催办（REMIND）」**：它能提醒、能把「有人没签」暴露到通知里，
   但**不会自动放行或驳回**（`AUTO_APPROVE` 需要流程级 `allow_auto_approve=true`，而这 6 条流程
   都是 `false`）。若业务要求「超时自动驳回」，需要单独决策（改的是业务后果，不是 bug 修复）。
   本卡把它们纳入不变量 ①②，任何一次「把会签节点改成无超时」都会立刻报红。
4. **145 演练数据**：本卡新增两张 `E2E07FIX1-ANY-*` 合同（均 SIGNED，无中间态）与 V0359 的 5 个新版本行。
   历史实例仍跑在归档版本上（各流程 1–6 张），行为不变。
5. **夹具手法**：`e2e07_contract_a.sh` 里「重置 `p3a_0003` 口令哈希」的 hack 仍留着（它现在只为拿 3 个
   令牌做审批断言，不再是流程走通的前提）。后续别再靠改密码推进会签。
