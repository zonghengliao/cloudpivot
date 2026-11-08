# P3-06 签名模式：APPROVAL 默认或签（复用 approval_mode，含存量冻结迁移）

日期：2026-11-08 · 卡：`t_8f3284f4` · 分支：main（工作区 `/opt/cloudpivot`）

## 一句话结论

`APPROVAL` 节点的 `approval_mode` 现在是**真配置**：不填 = 或签（`ANY`，引擎兜底），
要全员签必须显式 `ALL`（发布校验放行 `NULL`、拦截「选了按比例却没填比例」）；
存量按决策 D2 **按数据冻结**（候选人数 ≥2 → `ALL` 保持原行为，其余 → `ANY`）。
`cloudpivot_prt03` 实测：本次冻结 **ALL=4 / ANY=34 / 无法判定=0**，幂等重放 `UPDATE 0`，
重启「应用 0 个迁移」；真 HTTP 实测「角色 3 人 + 制单人第 4 人 → 任 1 人签即通过、
另 2 条 `CANCELLED`」「显式 ALL → 3 人都要签」全部通过（59 条断言 0 失败）。

## 1. 关键实现与决策落地

| 决策 | 落地位置 | 说明 |
|------|----------|------|
| D1 不新增 `sign_mode` 列 | 无新列 | 复用 `approval_mode`（`ALL`/`RATIO`/`ANY`），`sign_mode` 留给 L3 有序多签 |
| 引擎 `NULL` 兜底 `ALL` → `ANY` | `flow/graph.rs::ApprovalMode::effective`、`flow/engine.rs` vote 分支 | 兜底口径收口到一个函数：`NULL`/空串/非法值一律 `Any`；注释写明「不填=或签，要全员签必须显式 ALL」 |
| 存量按数据冻结 | `V0339`（草稿）+ `flow/sign_freeze.rs`（已发布/归档，启动期） | 见 §2 |
| 新节点显式写 `ANY` | `frontend/.../graph.ts::makeNode` | `APPROVAL → 'ANY'`、`COUNTERSIGN → 'ALL'`、其余 `null` |
| 发布校验放宽 | `flow/graph.rs` R9 | `COUNTERSIGN` 仍必须选模式；`APPROVAL` 允许 `NULL`；**任何**选 `RATIO` 的节点都必须带 (0,1] 比例（以前只判会签，审批节点选按比例不填阈值会静默按 100% 跑） |

前端：`NodeProps.vue` 的签名模式选择器对 `APPROVAL` 也显示（文案「或签（任一人通过）/
全员签（全部同意）/ 按比例通过」，默认选中 `ANY`）；`flow-node-view.ts` 与 `FlowCanvas.vue`
的节点副标题对 `APPROVAL` 也显示模式（**缺审批人优先**，避免默认 ANY 把「未配置审批人」盖掉）；
`flow-api.ts` 类型注释更新。

## 2. 迁移 V0339 为什么是「SQL + 启动期冻结器」两段

`APPROVAL` 节点存在两处，引擎读的是后者：

1. `biz_flow_node`（编辑期行，草稿编辑器读它）；
2. `biz_flow_def_version.graph_snapshot`（发布快照，**实例起单/投票读它**），
   且 `graph_hash = sha256(行的 canonical JSON，含 approval_mode)`。

因此「只 UPDATE `biz_flow_node`」会有两个后果，都不是 D2 想要的：

- **行为没保住**：已发布流程起新实例仍从快照里拿 `NULL` → 新默认下变或签（悄悄降签核强度）；
- **自检天天误报**：`flow::maintain` 的审批图快照自检（每天 03:30）把「行 hash ≠ 发布时
  hash」报成「发布版本的 node/edge 行被直接修改」，逐条写审计。

所以：

- **`V0339`（SQL）**：只冻结 **DRAFT** 版本的行（草稿没人读，SQL 改它绝对安全），
  并在结尾 `RAISE NOTICE` 打印「草稿冻结 ALL/ANY」与「已发布档待冻结器处理 N 个」；
- **`flow::sign_freeze`（启动期，`bootstrap` 新增一步）**：处理 **PUBLISHED/ARCHIVED**，
  用发布期**同一套** `snapshot_json()` / `graph_hash()` 把「快照 + hash + 行 + 运行中实例节点」
  四者同改（同一事务），口径与 V0339 逐字同义。幂等：只认 `approval_mode IS NULL`，
  跑完即无目标；失败不阻断启动（记 error，下次重试）。
  与 `bootstrap` 里「补齐当年会计期间」同款：**写不进迁移的运行期数据修复**。

候选人数口径（SQL 与 Rust 两处同义，单测钉住纯函数 `graph::freeze_sign_mode`）：
`USER` → 去重人数；`ROLE` → 该角色启用成员数（角色未删未停 + 授权未过期 + 用户未删未停）；
`DEPT_LEADER` → 部门 `leader_id` 去重数；`ADMIN` → `SUPER_ADMIN` 启用用户数；
`INITIATOR/INITIATOR_SUPERIOR/TASK_*` → 1；`FORM_VAR/EXPRESSION/没配来源` → 算不出来（按 `ANY` 落库）。

DOWN：只把**草稿**清回 `NULL`（并打印行数）；已发布档的冻结是四者同改，没有一键反向
（要回滚只能「另存为新草稿 → 改回或签 → 重新发布」），文件头写清了这条已知损失。

## 3. 证据（全部为本机真实输出）

脚本（可复跑）与原始输出都在本目录：

| 文件 | 内容 |
|------|------|
| `scripts/probe_candidates.sql` | 只读探针：逐节点候选人数与冻结结论 |
| `scripts/predict_freeze.sql` | 迁移前预测 `pred_all|pred_any|pred_unknown` |
| `scripts/10_migration.sh` | `cloudpivot_prt03` 克隆 → 真启动 → 迁移 + 冻结器 + 幂等 + down |
| `scripts/20_e2e_sign_mode.sh` | 隔离实例（:18307 / `cloudpivot_p306_ut`）真 HTTP 四场景 |
| `scripts/lib.sh` / `jget.py` | 公共函数 / JSON 取值 |
| `raw/10_upgrade.txt` | 迁移证据原始输出 |
| `raw/20_e2e_sign_mode.txt` | e2e 证据原始输出 |
| `raw/20_predict_probe.txt` | 迁移前预测 + 逐节点探针输出 |
| `raw/30_checks.txt` | fmt / clippy / 单测 / typecheck 门禁 |
| `raw/40_git.txt` | 本卡改动文件清单与 commit |

### 3.1 迁移前分布与冻结结果（`cloudpivot_prt03` → 克隆 `cloudpivot_p306_mig`）

```
迁移前（APPROVAL，按版本状态）：
  ARCHIVED  NULL=6 / ALL=0 / ANY=0
  PUBLISHED NULL=32 / ALL=0 / ANY=1      ← 库里唯一一个显式 ANY 是历史数据
预测（D2 口径）：pred_all=4 / pred_any=34 / pred_unknown=0（合计 38 = NULL 节点数）
首启：应用 7 个迁移（V0332 → V0339），V0339 已登记，最大号 = 339
冻结器日志：versions=21 nodes=38 all=4 any=34
冻结后：
  ARCHIVED  ALL=1 / ANY=5
  PUBLISHED ALL=3 / ANY=30            ← APPROVAL 的 NULL 归零
新冻结 ALL=4（= 预测）/ 新冻结 ANY=34（= 预测）/ 无法判定=0
行 vs 快照逐节点对账：不一致的节点数 = 0
重启：应用 0 个迁移；冻结器无目标（无第二行「冻结完成」）
重放 V0339.up.sql：UPDATE 0（幂等）；NOTICE 打印「草稿冻结 ALL=0 / ANY=0」+
  「已发布/归档待启动期冻结器处理：38 个」
down.sql：只清草稿（本次 0 个），已发布档保持冻结；再重放 up.sql 后分布与首启一致
```

> 「无法判定=0」是这份真实数据的结论（该库的审批人来源只有 `ROLE`/`ADMIN`/`TASK_CREATOR`/
> `INITIATOR_SUPERIOR`，都能算出人数）。`FORM_VAR` 这类算不出来的节点由 e2e 的 E4 造出来验证过
> （→ `ANY`）。

### 3.2 真 HTTP 四场景（隔离实例 `:18307` / 空库 `cloudpivot_p306_ut`，PASS=59 FAIL=0）

| 场景 | 夹具 | 断言要点 |
|------|------|----------|
| **E1** `APPROVAL` + `approval_mode` 不填 | 角色 3 人（B/C/D）+ 制单人第 4 人（A） | 发布成功（R9 放行 NULL）；实例节点行落 `NULL`；3 条待办；**B 一人同意 → 实例 `APPROVED`**，C/D 待办 `CANCELLED`（原因「无需再投」），剩余 PENDING=0 |
| **E2** 同角色 + 显式 `ALL` | 同上 | B 签 → 实例/节点仍 PENDING，剩 2 条；C 签 → 仍 PENDING；D 签完 → `APPROVED`（3 人都要签） |
| **E3** ARCH-8 场景 A | 角色 3 人**含制单人**（A/B/C），节点 `ALL` | A 那票 `SKIPPED`（self_approval=SKIP）→ 剩 2；B 签后仍 PENDING，C 签完才 `APPROVED` |
| **E4** D2 口径 + 幂等 | 草稿三节点：ROLE 3 人 / ROLE 1 人 / FORM_VAR | 冻结后 `ALL` / `ANY` / `ANY`；重放 `UPDATE 0`；down 三个都回 `NULL`；再 up 复原为 `ALL` |

**ARCH-8 场景 A 的答案（卡面点名要写清）**：冻结后该节点（ROLE 3 人）是 **`ALL`**，
所以「SKIP 掉制单人后剩 2 人」是**2 人都要签**；若某节点在库里是 `NULL`/`ANY`（或签），
则 SKIP 后剩下**任 1 人签即通过**。判定只看该节点自己的 `approval_mode`，与是否发生过 SKIP 无关。

### 3.3 门禁（`raw/30_checks.txt`）

- `cargo fmt`（本卡 7 个 Rust 文件）干净；
- `cargo clippy --lib -- -D warnings`：只剩既有基线 `erp/sales_order_model.rs update_order too_many_arguments(8/7)`（P2-02 起），本卡文件零告警；
- 单测：`business::flow::graph::tests` 39 passed / `flow::engine` 15 passed（含新增 4 条）/ `flow::sign_freeze` 1 passed；
- `pnpm typecheck`（`vue-tsc --noEmit`）exit 0；
- 前端 `pnpm test:unit`：**1051 tests / 1049 pass / 2 fail**，两条失败均为**他人在本工作区的在途改动**
  （`erp-sales-order` 的 `currency_code` 字段、`flow-biz-link` 的 `FlowBizType` 15→16 条），与本卡文件无关（见 `raw/40_git.txt` 的改动面）。

## 4. 已知限制 / 后续

1. **`V0339` down 不可逆到「已发布快照」**：已发布/归档档的冻结是快照 + hash + 行 + 实例节点同改，
   反向需要重新发布（down 文件头写明了）。草稿档的 down 是逐行清回 `NULL`。
2. **运行中实例**：冻结器会同步把 `biz_flow_instance_node.approval_mode` 从 `NULL` 写成冻结值，
   所以在跑流程的签法不被打断；**已经冻结后**再手工改节点也不会影响已起实例（它们读自己的实例节点行）。
3. **`RATIO` 的审批节点**：本轮把 R9 扩到 `APPROVAL`（选了按比例就必须有比例），但**没有**新增
   「审批节点按比例」的专门测试（行为与会签共用 `judge`，单测已覆盖 `RATIO` 三种临界）。
4. `sign_mode`（有序多签 SEQUENTIAL）仍不在本轮范围（决策 D1），留给 ARCH-5 L3。

## 5. 复跑方式

```bash
export PGPASSWORD=<本机测试库口令>            # 库账号 alun
bash docs/evidence/20261108-p306-sign-mode/scripts/10_migration.sh   # 迁移/冻结/幂等/down
bash docs/evidence/20261108-p306-sign-mode/scripts/20_e2e_sign_mode.sh  # 四个真 HTTP 场景
```

两个脚本都需要 `/root/p306/instance.env`（600；含三把密钥 + 初始超管口令，仓库不落口令）
与 `/root/p306/bin/cloudpivot`（本卡构建的二进制）。端口 18306/18307 与库
`cloudpivot_p306_mig` / `cloudpivot_p306_ut` 都与其它卡分开。
