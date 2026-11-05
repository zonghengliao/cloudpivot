# P0-FIX-3 · 生产库 e2e 残留清理（卡 `t_44766ab3`）— 证据包

执行时间：2026-11-05 13:41–13:46 CST（生产机 172.18.10.144，库 `cloudpivot` @ 127.0.0.1:5432）
结论：**383 行活跃 e2e 标记数据软删、6 个在用测试账号禁用（未删行）；账务/审计行数逐项不变；O1 巡检 ⚠️→✅；
e2e 标记从「靠人眼认前缀」变成「可机读的强约束」**。

---

## 1. 残留清单（改前，逐表 + 逐条）

判据唯一实现：`scripts/verify/e2e_marker.py::TABLE_SPECS`（本次新增）。逐条明细见 `01_before_state.txt`。

| 表 | 活跃命中 | 全量（含已软删） | 判据 |
|---|---|---|---|
| `biz_task` | **9** | 355 | `title like '[E2E]%' or '[E9E2E]%'` |
| `biz_task_comment` | **221** | 221 | 挂在上面 9 个任务下（**不看正文**） |
| `sys_notice` | **105** | 108 | `title ilike '%E2E%'` |
| `sys_file` | **43** | 45 | `original_name ilike 'e2e%' or 'e9-%'` |
| `sys_export_task` | **2** | 2 | `export_name / file_name ilike 'e2e%'` |
| `biz_flow_instance` | **1** | 1 | `title ilike '%E2E%'` |
| `sys_announcement` | **1** | 2 | `title ilike '%E2E%'`（这条是**已发布**公告） |
| `sys_help_category` | **1** | 2 | `code ilike 'e2e%' or name ilike '%E2E%'` |
| `biz_warehouse` | 0 | 4 | 4 个 `E2E-W4A-*` **早已软删** → 确认干净，本包不动 |
| **合计** | **383** | | |

### 9 个活跃 E2E 任务（逐条，全部 2026-10-31 11:57–12:05 由 e2e 套件造出）

| 表 | id | 编号 | 标题 | 创建时间 |
|---|---|---|---|---|
| `biz_task` | `01a1901a-d6b9-769a-b9ce-202ccaa4c31a` | TSK-202610-000880 | `[E2E]详情-e2emvvvdf01` | 2026-10-31 11:57:20.444105+08 |
| `biz_task` | `01a1901a-d6ca-7092-811b-349e073cfe51` | TSK-202610-000881 | `[E2E]详情-e2emvvvdf01-子1` | 2026-10-31 11:57:20.459032+08 |
| `biz_task` | `01a1901a-d6d7-75c3-b37f-914743d2c7cd` | TSK-202610-000882 | `[E2E]详情-e2emvvvdf01-子2` | 2026-10-31 11:57:20.471483+08 |
| `biz_task` | `01a1901a-d6e1-70e4-9452-0ce951ba9c51` | TSK-202610-000883 | `[E2E]详情-e2emvvvdf01-子3` | 2026-10-31 11:57:20.481710+08 |
| `biz_task` | `01a1901a-e3c3-73a7-867d-c8ee81103422` | TSK-202610-000884 | `[E2E]详情-e2emvvvdf01-UI追加` | 2026-10-31 11:57:23.779373+08 |
| `biz_task` | `01a19022-8966-75b1-8879-ee94afae7056` | TSK-202610-000893 | `[E2E]详情-e2emvvvox7o` | 2026-10-31 12:05:44.934687+08 |
| `biz_task` | `01a19022-897a-7114-9c95-6ecb36ad2242` | TSK-202610-000894 | `[E2E]详情-e2emvvvox7o-子1` | 2026-10-31 12:05:44.954191+08 |
| `biz_task` | `01a19022-8989-768b-881b-888bbede7f81` | TSK-202610-000895 | `[E2E]详情-e2emvvvox7o-子2` | 2026-10-31 12:05:44.969548+08 |
| `biz_task` | `01a19022-899d-71ef-acde-0b5274b848f3` | TSK-202610-000896 | `[E2E]详情-e2emvvvox7o-子3` | 2026-10-31 12:05:44.989422+08 |

其余各表样例（完整列表见 `01_before_state.txt` 第 3 节）：

* `sys_notice` 105 条：`待你审批：[E2E]审批单-e2emvugv4z6` ×多、`审批已通过：E2E单签实例` ×4、
  `E2E公告-43870`、`E2E2公告-43911`、`E2E公告测试 085522`、`E2E通知测试 085332/085336`、`定时通知E2E`、
  `E2E测试`、`待你审批：E2E会签实例` …（全部标题带 E2E 或测试账号号段）
* `sys_file` 43 条：`e2e.txt` ×3、`e2e_big.bin`、`e2e-e2emvugheec.txt` …、`e9-e2emvufu76b.txt`…
* `sys_export_task` 2 条：`e2e_export` ×2
* `sys_announcement` 1 条：`E2E公告测试 085522`（**status = 已发布**，客户会在公告栏看到）
* `sys_help_category` 1 条：`e2e_cat_085535` / `E2E分类 085535`
* `biz_flow_instance` 1 条：`TSK-202610-000508 [E2E]探针-parent_progress-5833`
* 测试账号：`cl_43870` / `laoliu_test` / `qa.limited` / `qa.mutex` / `qa.viewer` / `verify-ai`
  六个全部 `deleted_at IS NULL AND status = 1`（另有 17 个已软删账号，含同批 `cl2_43911` 等，本包不动）

### 误伤核查（判据为什么不是 `%E2E%` 一把扫）

生产库里真实内容提到 e2e 的有 6 条：`laoliu` 的 4 条开发汇报（挂在 `T8-A` / `P4-B` 等**真实任务**上）、
`verify-ai` 的 2 条真实阻塞通知。所以评论判据是「父任务带标记」，通知/公告判据只看**标题**。
反例钉在 `e2e_marker.py --self-test` 的 `MUST_NOT_MATCH`（3 条），本轮 self-test 12/12 通过。
软删前又按快照逐表复核了一遍标题/文件名：105 条通知、43 个文件、1 个公告、1 个帮助分类、
1 个流程实例，**没有一条**是真实业务内容。

---

## 2. 处置：软删 + 禁用（前后对比）

做法：`scripts/datafix/e2e_residue3/`（一次性数据修复包，**不是迁移**）。
口径：E2E 行 **软删**（`deleted_at = now()`，行还在、可查可回滚）；测试账号 **禁用**（`status = 0`，行绝不删）；
`update_by` 不改（谁动的库由快照 + 账本 + git 交代）。

前后对比（`01_before_state.txt` ↔ `08_after_state.txt`）：

| 项 | 改前 | 改后 |
|---|---|---|
| `biz_task` 活跃标记 | 9 | **0**（355 行仍在，其中 9 行本轮 `deleted_at`） |
| `biz_task_comment` 活跃 | 221 | **0**（221 行仍在） |
| `sys_notice` 活跃 | 105 | **0**（108 行仍在） |
| `sys_file` 活跃 | 43 | **0**（45 行仍在） |
| `sys_export_task` 活跃 | 2 | **0** |
| `biz_flow_instance` 活跃 | 1 | **0** |
| `sys_announcement` 活跃 | 1 | **0**（2 行仍在） |
| `sys_help_category` 活跃 | 1 | **0**（2 行仍在） |
| `biz_warehouse` 活跃 | 0 | 0（4 行本就软删 = 确认干净） |
| 在用测试账号 | 6 | **0**（6 行仍在，`status=0`、`deleted_at IS NULL`） |

账本（`datafix_e2eres3_guard`，账本口径 = **快照口径**，跑几遍都不变）：
`deleted_total = 383`、`deleted_biz_task = 9`、`deleted_biz_task_comment = 221`、`deleted_sys_notice = 105`、
`deleted_sys_file = 43`、`deleted_sys_export_task = 2`、`deleted_biz_flow_instance = 1`、
`deleted_sys_announcement = 1`、`deleted_sys_help_category = 1`、`disabled_users = 6`、`verify_fail = 0`。
快照（`datafix_e2eres3_snapshot`）383 + 6 = 389 行整行 `to_jsonb`，回滚只认它。

**用户可见效果**：任务列表 SQL 本身带 `WHERE t.deleted_at IS NULL`
（`backend/src/business/task/task_model_query.rs:163/244`），公告/通知/帮助/上传件的列表同样走软删过滤
—— 客户打开页面看不到这些行了。

---

## 3. 测试账号「禁用」而不是「删除」的证据

`09_login_after.txt` / `10_auth_logs_disabled.txt`：

```
laoliu_test → HTTP 403  code 20002  账号已被禁用，请联系管理员
qa.viewer   → HTTP 403  code 20002  账号已被禁用，请联系管理员
verify-ai   → HTTP 403  code 63201  AI 用户不能登录网页     （ROBOT，比禁用判定更早）
auth_logs: 2026-11-05 13:44:29 | laoliu_test | FAIL | 账号已禁用
           2026-11-05 13:44:29 | qa.viewer   | FAIL | 账号已禁用
```

关键点：后端登录**先判 `status` 再校验密码**（`handler.rs`：403/20002 在第 ④ 步，401/20001 在第 ⑤ 步），
所以「改前 401 用户名或密码错误 → 改后 403 账号已被禁用」是状态位改变的直接后果，不受密码对错影响。
行没删：`08_after_state.txt` 第 3 节显示 6 行 `deleted_at = NULL`、`status = 0` —— 历史 `create_by` / 审计引用继续有效。

---

## 4. 守卫扩展（「E2E 数据必须带标记，能在生产被识别」）

标记从「文档约定」变成**代码里的唯一判据 + 两个强制点**：

| 层 | 文件 | 作用 |
|---|---|---|
| 前端生成 | `frontend/e2e/e2e-marker.ts`（新） | `E2E_MARK = '[E2E]'`、`markTitle` / `markWarehouse` / `markFileName`（幂等） |
| 前端强制 | `frontend/e2e/support.ts`（改） | `newApi().createTask` / `createInstance` 一律过 `markTitle()` —— **用例忘写前缀也造不出未标记的任务**（本次残留的直接根因修复） |
| python 判据 | `scripts/verify/e2e_marker.py`（新） | 9 张表的逐表谓词 + 6 个测试账号 + `--self-test` / `--json` / `--assert-clean`；**唯一实现**，别处只调用不重抄 |
| prod 后置 | `frontend/e2e/target-guard.mjs`（改） | `PROD_POSTRUN_STEPS` 新增 `python3 scripts/verify/e2e_marker.py --assert-clean` —— 打生产后必须复查 |
| 数据修复 | `scripts/datafix/e2e_residue3/apply.sh`（新） | 收尾跑 `--assert-clean` 做**交叉断言**（自己的 SQL 判据 + python 判据两份都认干净才算干净） |
| 持续巡检 | `scripts/verify/erp_invariants.py`（改） | 新增 **O1**（见 §5） |
| 运行时自测 | `frontend/e2e/e2e-marker.selftest.ts`（新） | `./node_modules/.bin/tsx e2e/e2e-marker.selftest.ts` —— 幂等/空标题/仓库名/文件名 11 项断言，输出 `16_marker_selftest.txt` |

self-test 输出（`--self-test`，含跨语言/接线检查）：

```
[ok] 应命中   biz_task / biz_warehouse / sys_file / sys_export_task / sys_notice …
[ok] 不应命中 biz_task_comment  "8 层深度已完成并验证。commit 0f4cad3 … e2e 用例"
[ok] 不应命中 sys_notice        "AI 需要人工协助"
[ok] 不应命中 biz_task          "T8-A 任务父子深度 2 → 8 层"
[ok] 前端 TS 标记一致  E2E_MARK = [E2E]（Python 侧 [E2E]）
[ok] 接线在          support.ts::createTask/createInstance 过 markTitle()
[ok] 接线在          target-guard.mjs 的 prod 后置清单含 --assert-clean
---- self-test：12/12 项通过 ----
```

> 接线检查是有意加的：`e2e-marker.ts` 单独存在时一行数据都不会被标记（本轮之前就是这种状态），
> 只有 `support.ts` 真的调、`target-guard` 真的跑，才叫「守卫」。谁把接线删了，self-test 立刻红。

---

## 5. 新增巡检 O1（生产库出现 E2E 标记数据 → WARN）

`erp_invariants.py::group_o`，`--group E2E` 单跑：

* **只在生产库上判**（用 `db_guard.prod_identity / classify_db` 认身份）：测试库/演练库里的 `[E2E]` 行是用例
  预期产物，在那里报 WARN 就成了「永远亮的指标」；非生产库登记为 INFO「跳过」。
* 判据不重抄：直接 import `e2e_marker`，与清理包、前端后置清单**同一份实现**。
* FAIL 时给逐表条数 + 样例 + 定位 SQL（`RESIDUE_SQL`）。

**反向对照（同一个库、同一套代码，只差「清没清」）**：

| 时点 | 输出 | 文件 |
|---|---|---|
| 清理前 13:43 | `⚠️ [E2E] O1 …` 活跃标记行 383 条，分布在 8 张表 + 6 个在用账号 → **结果 64/65，BLOCKER 0，WARN 1** | `05_invariants_before.txt` |
| 清理后 13:44 | `✅ [E2E] O1 …` → **结果 65/65，BLOCKER 0，WARN 0** | `12_invariants_after.txt` |
| 清理后再跑一轮 | 65/65，O1 ✅（连跑两轮结论一致） | `19_invariants_after_r2.txt` |
| 非生产库（`cloudpivot_dummy_check`） | `· 跳过：库 … 不是生产库 …` + ✅（**不误报 WARN**，且「跳过」看得见） | `17_invariant_nonprod_branch.txt` |
| 生产库单跑 `--group E2E` | 1/1 ✅ | `18_invariant_prod_o1.txt` |
| **用已提交脚本（origin/main 379efa8）在 144 上做最终复核** | `e2e_marker.py` ✅ 干净 rc=0；`erp_invariants.py` **65/65 BLOCKER 0 WARN 0**，O1 ✅ | `20_final_verify_committed.txt` |

> `20_final_verify_committed.txt` 里 `--self-test` 的 2 条 FAIL 是**预期**：144 的部署检出停在 `7d96692`，
> `frontend/e2e/{support.ts,target-guard.mjs}` 还是旧版（没有本次接线），接线检查如实报红。
> 同一条命令在 99 主干树（= origin/main）上 **12/12 通过**（`21_marker_selftest_full_tree.txt`），
> 下次 `deploy.sh` 同步后 144 上也会变 12/12。判「生产数据是否干净」只看 marker + 不变量这两行。

---

## 6. 演练（先演练，后动生产）

`drill.sh` 在**生产整库克隆**上跑 `10 → 20 up → 30 → down → 30(应红) → up ×2`：

* `03_drill_round1.txt` / `04_drill_round2.txt`：清前演练，克隆库自带 383 行真实残留，两轮全绿。
* `14_drill_round3.txt`：**合入后的复跑**。生产已清干净 ⇒ 克隆库原本 0 残留，脚本按本批快照把行写回
  重建「改前现场」（383 行 / 6 账号）再炼 → 全绿。这条同时证明**快照本身够回滚**、演练永远可复跑。

三轮共同的结论：

```
✅ 清后残留 = 0 行 / 0 个在用账号
✅ 回滚后残留原样回来：383 行（改前 383）   ← 可逆
✅ 回滚后账号状态回来：6 个在用（改前 6）
✅ 30_verify 在回滚后如期失败              ← 反向对照：断言有牙
✅ 第三次全部命中 0 行（幂等成立）
★★★★ 演练全绿：可逆 / 幂等 / 断言有牙 ★★★★
```

生产应用两轮（`07_apply_prod.txt` 首轮 = 383 行/6 账号；`13_apply_prod_round2.txt` 第二轮 = 全 0 行，
`30_verify 22/22`；`15_apply_prod_round3_ledger.txt` = 账本口径修正后复核，仍 0 行、账本回归 383）。

---

## 7. 不变量全绿（软删后账务不受影响）

`30_verify.sql` 的 C 组逐表比对「改前 = 改后」，22 条断言全 PASS（`07_apply_prod.txt`）：

```
biz_voucher 21 | biz_voucher_entry 55 | biz_payable 12 | biz_payment 0 | biz_inventory 3
biz_inventory_txn 25 | biz_accounting_period 12 | biz_opening_balance 0 | biz_status_log 115
biz_task(全量) 846
```

生产全量不变量（含会计恒等式 / 账账 / 账实 / 单据链 / 状态 / 期间 / 闭环 / 审批 / 盘点 / 固资 / 报销 / 任务 / 权限 N1）：

```
清理前：64/65 通过   🚨 BLOCKER 0   ⚠️ WARN 1（= O1，本轮要清的就是它）   → 05_invariants_before.txt
清理后：65/65 通过   🚨 BLOCKER 0   ⚠️ WARN 0                            → 12_invariants_after.txt
```

服务状态：`systemctl is-active cloudpivot = active`、`/health = 200`（后端 8023 与 nginx 80 各测一次）。

---

## 8. 怎么复跑

```bash
# 生产机 144
ssh root@172.18.10.144 && cd /opt/cloudpivot
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
# 脚本随 commit ea6a290 落在 main；144 的部署检出还停在 7d96692，
# 下次 deploy.sh 才同步。急用时先取这几个文件本身：
#   git fetch origin && git checkout origin/main -- scripts/verify scripts/datafix/e2e_residue3

# ① 现状（只读；干净 → 退出码 0）
python3 scripts/verify/e2e_marker.py
bash scripts/datafix/e2e_residue3/apply.sh --check

# ② 巡检（O1；单独跑只要 1 条）
python3 scripts/verify/erp_invariants.py --group E2E
python3 scripts/verify/erp_invariants.py            # 65/65 / BLOCKER 0 / WARN 0

# ③ 演练（克隆生产库 → 清 → 回滚 → 幂等；不动生产）
KEEP_DRILL_DB=1 bash scripts/datafix/e2e_residue3/drill.sh

# ④ 真要再清一次（例如有人又打了生产）
bash scripts/datafix/e2e_residue3/00_backup.sh
E2E_ALLOW_PROD=1 E2E_PROD_REASON='<谁批准/为什么>' bash scripts/datafix/e2e_residue3/apply.sh
E2E_ALLOW_PROD=1 E2E_PROD_REASON='<…>' bash scripts/datafix/e2e_residue3/apply.sh --rollback   # 回滚

# ⑤ 判据自测（不连库）
python3 scripts/verify/e2e_marker.py --self-test
```

---

## 9. 遗留 / 后续（不在本卡范围，已另开或已知）

1. **QA 夹具角色**：`QA_LIMITED` / `QA_VIEWER` / `MUTEX_A` / `MUTEX_B` 仍是 `data_scope = 1`（全部数据），
   而持有它们的 3 个账号本轮已禁用 → **这四个角色现在没有活跃持有者**，可以把范围收成 4（仅本人）或直接
   软删角色（P0-FIX-2 交接建议）。改完复跑 `python3 scripts/verify/erp_invariants.py --group 权限`（N1 应保持 ✅）。
   → 已开卡，见本卡 completion 里的 `created_cards`。
2. **数据修复留痕对象**：`datafix_e2eres3_spec` / `_snapshot` / `_guard` 三张表留在生产库（回滚依据 + 审计账）。
   不进迁移、不带业务语义；确认不再需要时可 `DROP VIEW/TABLE`（先确认快照不再被引用）。
3. **e2e 套件默认账号 `laoliu_test` 已禁用**：e2e 只能打隔离实例（`target-guard.mjs` 已硬拒非隔离目标），
   隔离库里应自建账号（`E2E_USERNAME` / `E2E_PASSWORD`）。若将来有人要打生产做线上验收，
   必须走 `E2E_ALLOW_PROD=1 E2E_PROD_REASON=…` 并跑后置清单（含 `e2e_marker.py --assert-clean`）。
