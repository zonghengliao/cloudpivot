# t_977c5189 · p17 夹具残留「在职超管」→ flow::edge_admin_tests 3 条假红（收口证据）

- 卡：`t_977c5189`（父卡 `t_2431e625` 部署回归时开卡）
- 日期：2026-11-06
- 结论：**产品代码零改动**；改的是测试夹具（`payable_bridge` 加 teardown、`edge_admin_tests` 改成
  「确定性单人复核 + 图基线守卫」）与门禁（`scripts/check.sh` 增可选档 `CHECK_WITH_DB=1`）。
- 验收：同一隔离库、同一进程串跑 `payable_bridge::tests` + `flow::edge_admin_tests` → **46/46 绿**，
  库侧复查 **0 残留**；带「2 个残留超管」的污染库上，`edge_admin_tests` **23/23 绿**（修前 3 红）。

---

## 1. 卡面现象（已复现）

同一隔离库、`--test-threads=1` 跑全量真库：

| 用例 | 修前 | 修后 |
|---|---|---|
| `batch_task_invalid_uuid_only_fails_that_item` | `落边 0 行`（left:0 right:1） | 绿 |
| `batch_task_repeated_approval_keeps_single_row` | `落边 0 行`（left:0 right:1） | 绿 |
| `batch_task_writes_edge_after_approval` | 实例 `PENDING`（「还有 2 条 PENDING 待办没批」） | 绿 |

本机复现命令（隔离库 `cloudpivot_iso977`，191 条迁移由被测树二进制首启套完）：

```bash
# 先造出与 p17 夹具等效的残留：2 个「在职 + SUPER_ADMIN」用户
bash <(echo)   # 见 §4 的 t977-pollute.sh（等价 SQL：INSERT sys_user + sys_user_role(SUPER_ADMIN)）
ERP_DB_URL=postgres://alun:...@127.0.0.1:5432/cloudpivot_iso977 \
  cargo test --lib --features test-hooks -- --test-threads=1 \
  business::flow::edge_admin_tests::batch_task
# 修前：FAILED. 6 passed; 3 failed   ← 与卡面三条、两条 left:0 right:1 + 一条 PENDING 完全一致
```

## 2. 根因（**订正卡面的一处描述**）

卡面写的是「p17 夹具**改写了共用已发布流程图**」。实测**图没有被改写**：

```
已发布 flow_edge_config（V0206 种子，指纹 c0ad6193…）：
  节点 s → n_cfg(APPROVAL, assignee_type=ROLE=FLOW_CONFIG_APPROVER,
                 empty_assignee=FALLBACK_ADMIN, join_mode=AND) → e
  审批人行：1 条（n_cfg / ROLE / 20000000-0000-4000-8000-0000000f0015）
```

真实机制是**审批人展开**：

1. `payable_bridge`（P17）的夹具用户 `p17_ap1 / p17_ap2` 被绑 `SUPER_ADMIN`；
2. P17 的清场 `cleanup_fixtures()` **只挂在 `setup()`（下一个用例开头）**，模块跑完不会执行
   ⇒ 库里留下 **3 个在职超管**（admin + p17_ap1 + p17_ap2）；
3. `n_cfg` 的复核岗 0 人 ⇒ `FALLBACK_ADMIN` 兜底到「**全部**在职超管」；
   `join_mode=AND` 是**会签** ⇒ 一张单 **3 条待办**；
4. 批量用例只批 1 条 ⇒ 实例停在 `PENDING`，业务回调不触发 ⇒ `biz_flow_state_edge` 一行不写
   ⇒ `left:0 right:1`。

库侧直接证据（P17 模块**从空库**起跑，跑完后）：

```
p17_ap1|t|1     ← 在职，且仍挂 SUPER_ADMIN
p17_ap2|t|1
p17_creator|t|0
```

即：**不是图被改，而是「谁算审批人」被残留账号放大了**。这一点决定了修法：
把夹具钉成「单人复核」（不依赖库里超管人数）+ 给图加基线守卫 + 让 P17 模块收尾就清场。

## 3. 三处改动（产品代码零改动）

### ① `backend/src/business/flow/edge_admin_tests.rs`：确定性单人复核 + 基线守卫

- `prepare()`（每个真库用例开头）＝ `guard_published_flow_graph()` → `purge()` → `bind_config_approver()`。
- `bind_config_approver()`：把内置 `admin` 绑到复核岗 `FLOW_CONFIG_APPROVER`
  ⇒ `ROLE` 非空、不再走 `FALLBACK_ADMIN` ⇒ **待办恒为 1**，与库里超管人数彻底解耦；
  绑完立刻断言「该角色在职成员恰好只有 admin」，多一个就报**夹具污染**。
- `purge()` 末尾 `unbind_config_approver()`：用例结束即还原（种子角色回到 0 成员）。
- `guard_published_flow_graph()`（卡面第 2 项要求的守卫）：断言**产品实际解析到的那条**
  （`edge_admin::published_flow()`）仍是 V0206 种子：
  定义 id `20000000-…e0206`、`graph_hash == c0ad6193…`、`node_count=3 / edge_count=2`、
  节点 key `[e, n_cfg, s]`、`n_cfg` 的 `APPROVAL/ROLE/FALLBACK_ADMIN/AND`、审批人 `[n_cfg:ROLE:<复核岗>]`。
  任何一项不符 → 直接 panic「**夹具污染（不是业务断言失败）**」并给出两种可能 + 修法。
- `seed_flow_hash_matches_rows` 同步钉住 `graph_hash`（同一口径）。

### ② `backend/src/business/erp/payable_bridge.rs`：每例 teardown

20 个 `#[tokio::test]` 末尾各加 `cleanup_fixtures().await;`（+ 模块头注释说明为什么两处都要挂）。
修前「模块跑完留 3 个在职夹具账号」，修后「跑完即摘净」（软删 + 解绑，仍保留物理行以避开
`biz_flow_task.assignee_id` 硬外键的并发写入）。

### ③ `scripts/check.sh`：闸⓪ 真库用例（可选档 `CHECK_WITH_DB=1`）

默认门禁不设库 URL ⇒ 真库用例 `println!("SKIP: …") + return`，libtest 记 **passed**
⇒「2910/2910 全绿」其实一条真库断言都没执行（这类缺陷只能靠人肉全量才暴露）。

新增档位（不启用时打印明确警告）：

```bash
CHECK_WITH_DB=1 CHECK_WITH_DB_FILTER=business::flow bash scripts/check.sh   # 局部
CHECK_WITH_DB=1 bash scripts/check.sh                                      # 全量 lib
```

判据：① 有失败 → 红；② 出现「库没配 / 连不上」类 SKIP（`未设置 XXX_DB_URL` / `XXX_DB_URL 连接失败` /
`set_db 失败`）→ 红（假绿）；③ 三个库 URL 缺一个 → 红。全量时自动 `--skip
business::flow::internal_control::db_tests`（那组按设计要求独占库），并加 `--nocapture`
（不加就永远 grep 不到 SKIP —— 实测 libtest 把 SKIP 接在 `test <name> ... ` 同一行）。

### ④ 新增回归脚本 `scripts/verify/flow_fixture_isolation_check.sh`

把「P17 先跑 → flow 后跑」这条链路固化成 5 条判据（同进程全绿 / 0 个在职 p17 账号 /
复核岗 0 绑定 / FLOW_EDGE_CFG 0 残留 / 图仍是基线指纹），带库名白名单（`cloudpivot_*`，拒 prod）。

## 4. 证据一览（本目录文件）

| 文件 | 内容 |
|---|---|
| `01-repro-dirty.log.txt` | 修前：污染库上跑 `batch_task` 子模块 → **3 failed**（与卡面逐字一致） |
| `02-baseline-clean.log.txt` | 修前：干净库上跑整个 `edge_admin_tests` → 23 passed（证明不是代码缺陷） |
| `03-fix-dirty-r1.log.txt` / `r2` | 修后：**同一个污染库**（仍有 3 个在职超管）上连跑两轮 → 23/23 绿 |
| `04-guard-negative.log.txt` | 负样本：给 `n_cfg` 再挂一个审批人 → 首句报「夹具污染」并点出多出来的审批人 |
| `05-p17-residue-before.log.txt` | 修前：P17 模块从空库起跑，跑完留 3 个在职夹具账号 |
| `06-chain-46-green.log.txt` | 修后：同进程串跑 P17 + flow → **46/46 绿** + 库侧 0 残留 |
| `07-check-with-db-negative.log.txt` | 闸⓪ 负样本：缺 `P0FIX_DB_URL` → 判「假绿」exit 1 |
| `08-full-lib-db.log.txt` | 修后全量 lib 真库（`--test-threads=1 --nocapture`，HEAD `8958246` + 本卡改动）：**2675 passed / 1 failed** |
| `09-db-side-check.txt` | 库侧 psql 复查（不经测试代码）：超管名单 / 复核岗 / 图指纹 / 残留计数 |

## 4.1 全量那 1 条红**不是本卡**（已定性、已开卡）

`08` 里唯一的失败是 `system::menu::permission_audit_tests::n16_every_route_has_permission_gate_or_documented_exception`：

```
POST /business/flow/linkage/preview（business/flow/linkage/admin.rs:682）
```

- 该路由**有** `#[nik::permission(...)]`，但属性顺序写成「`#[nik::post]` 在前、permission 在后」，
  而扫描器 `permission_above()` 只**从 `#[nik::post` 往上**找（注释明写「本仓声明顺序恒为
  permission → get → fn」）⇒ 判成裸路由。全仓其余路由都是 permission 在前。
- 与本卡改动无关：本卡只改 `flow/edge_admin_tests.rs` / `erp/payable_bridge.rs`（测试代码），
  `linkage/admin.rs` 在改动前后**逐字节相同**（`git show b75ccc8:…` 同样是这个顺序）。
- 它一直没被发现，正是本卡第 3 项要解决的问题：`n16` 在 `ensure_db()` 为假时打印 SKIP 后 return，
  libtest 记 passed ⇒ 默认门禁「全绿」。已开卡 **t_1b5391e7**（backend-developer）。

## 5. 口径说明（没做的事，别误解）

- **没有放宽任何断言**：三条用例的判据一字未改（落边行数仍要求恰好 1、实例仍要求 `APPROVED`），
  改的是「让前提成立」的夹具。
- **没有改测试线程数**：全程 `--test-threads=1`，与卡面复现口径一致。
- **没有把 P17 夹具账号物理删掉**：仍是「解绑 + 软删」，理由见 `payable_bridge.rs` 清场段注释
  （`biz_flow_task.assignee_id` 是硬外键，硬删会与并发发起审批的模块互撞）。
- 卡面 root-cause 里「p17 夹具改写了共用已发布流程图」这句**不成立**（见 §2），已按实测订正；
  但「flow 用例不该依赖共用已发布图的运行时展开结果」这个结论成立，修法就是按这个结论做的。
