# P2-2b 证据：`po_approve` 加「超信用额度 → 财务复核」（卡 t_c79af27a）

日期：2026-11-04 ｜ 决策文档：`docs/ERP-摆设字段口径.md` §1.3 ｜ 迁移：`V0216__erp_p2_2b_po_credit_branch`
代码基线：`45038a2`（+ 本卡对 `flow::graph::nearest_upstream_fork` 的 3 行修正）

本卡不是「写一个分支」那么简单：**决策是 B（加复核）**，而要把 B 配出来，先得补掉引擎里
一个把这类图判死的假阳性（R8）。下列证据按「先证缺陷、再证图合法、再证真机、最后证可复跑」排列。

| # | 文件 | 证明了什么 |
|---|---|---|
| 1 | `bad-topology-issues.json` | **后置串行**（金额分级之后再复核）被引擎结构校验拒：`R8 汇聚节点 c_credit2 的入边来自不同分支网关（n_board ← c_mid，n_dept ← c_low，n_pm ← c_mid）` —— 这是「前置」而不是「后置」的硬依据 |
| 2 | `r8-false-positive.txt` | 修复前的第二种假阳性：**前置**拓扑也被判「入边 c_low 上游没有分支网关」（存图 warning + validate 的 passed=false 原始报文）。根因：`nearest_upstream_fork` 只往上游找，入边**直接**来自网关时返 `None` |
| 3 | `r8-fix.diff` | 修正（3 行）：`nearest_upstream_fork` 先认 `from_key` 自身 |
| 4 | `unit-tests-flow-module.txt` | `business::flow::*` 全绿 **346 passed / 0 failed**（含新增 `r8_allows_a_bypass_branch_rejoining_the_fork_itself`：OR 汇聚放行、AND 汇聚仍必须拦） |
| 5 | `publish-via-designer-api.txt` | 走**真实设计器 API**（`/version/{id}/validate` → `/def/{id}/publish`）发布 v2：`passed=true`、`nodes=9 edges=11`，`graph_hash=4db3fe57…`，与迁移里写死的 hash **逐字节一致** |
| 6 | `migration-up-down.txt` | 干净克隆库上 `up → 再 up（幂等跳过）→ down（v1 复位）→ 再 up → 有 v3 时跳过`，每步都有状态与 NOTICE 原文 |
| 7 | `down-refused-on-live-instance.txt` | 回滚安全阀：`V0216 down 回滚中止：仍有 4 张审批单跑在 v2 上` |
| 8 | `e2e-run.txt` | 真机 e2e **41/41**（超限 → 财务复核串行第一道 → 采购经理 → APPROVED；额度内单**不**进复核；财务驳回挡住单子；额度 0 不校验；复核岗没人时提交 422+65018 fail-closed） |
| 9 | `rows-vs-graph-hash.txt` | 行↔hash 漂移自检（复刻 `flow::maintain`）：v1 `8f65819e…`、v2 `4db3fe57…` 全部一致，`drifted = 0`（每日自检不会报 RowsEdited） |
| 10 | `v2-graph.json` | 发布态 v2 的 canonical 快照（9 节点 / 11 边），可直接与库内 `graph_snapshot` 对比 |

## 复跑方式

```bash
# ① 迁移（up/幂等/down/再 up）——需要一个空克隆库
bash scripts/verify/p22b_migrate_check.sh <db>

# ② 行 ↔ hash 漂移自检（任意已发布 po_approve 的库）
python3 scripts/verify/p22b_rows_hash.py <db>

# ③ 真机 e2e（隔离实例，自带 P22B- 前缀清场）
python3 scripts/verify/p22b_credit_branch_e2e.py --port 18100 --db <db>
```

## 关键数字

- v2 `graph_hash = 4db3fe5731dc8efc7844d6a51a326df20638d8b1eabd4a553444ead503554b7d`
- v1 `graph_hash = 8f65819ea25e1145d441d9b947b89b21bd4bfdc5075ac321367ea106a06366de`（归档后**未变**）
- 图：9 节点 / 11 边 / 5 条审批人绑定；条件边 `e_credit_over = {"op":"cmp","field":"form.over_credit_limit","cmp":"eq","value":true}`
- 汇聚：`c_low` 2 条入边（都来自 `c_credit`）+ `join_mode = OR`（R8 要求条件分支必须 OR）

## 环境说明（为什么用工作树构建）

验收期间共享工作区 `/opt/cloudpivot` 被另一张卡（P2-1b 按物料拆分录）改到 `cargo build`
失败（`voucher.rs` / `stockin_*.rs` 六处编译错）。为了不干扰别人、也为了拿到一个可信的二进制，
本卡的构建与实例跑在 `git worktree`（HEAD `45038a2` + 本卡 patch）里，二进制复用共享
`target/`（依赖不重编）。证据里的每个数字都来自那次构建，不是共享工作区的中间态。
