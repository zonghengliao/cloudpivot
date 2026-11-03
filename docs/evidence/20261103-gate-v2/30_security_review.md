# 门禁 v2 安全复核（卡 t_459b6ca0）

- **复核人**：information-security（安全工程师）
- **复核对象**：`5408859`（卡 A：闸② 收窄到生产面 + G5 词法判定改写）、`2a9db18`（卡 B：生产面 10 文件 25 处 SCOPE-EXEMPT）
- **基线**：`main = 2a9db18`（与卡 B 交接一致：origin bare 144 / github 均为 `2a9db18`）
- **方式**：只读复核 + 自建负例 + 门禁脚本副本实验（未改 main 上的实现，仅提交本证据目录）
- **verdict**：**pass-with-notes**（两条放宽未使当前树上的真实违规漏检；但闸② 新引入 3 条「需刻意写法」的假绿通道、G5 新引入 1 条提示级放宽，见 §11 残留风险 R1–R7）

---

## 0. 结论速览

| 复核项 | 要求 | 实测 | 结论 |
|---|---|---|---|
| ① 函数顶层 `changelog::record(db(), …)` | 报红 | 报红 1 条 | ✅ |
| ② `tokio::spawn(async { changelog::record(&tx, …) })` | 报红 | 报红 1 条 | ✅ |
| ③ `transaction(\|tx\| async move { changelog::record(db(), …) })` | 报红 | 报红 1 条 | ✅ |
| ④ 本地 `fn record` 遮蔽裸调 + 同文件事务外 `changelog::record(ex,…)` | 裸调不红、真调用红 | 裸调 2 处 skip、真调用报红 1 条（case04:8） | ✅ |
| ⑤ `fn f(ex: &Transaction)` 里 `changelog::record(ex, …)` | 判绿 | 判绿（类型证据命中） | ✅ |
| 闸② 生产文件里裸 `#[cfg(test)]`（不给 mod） | 不得误当测试区 | 后文生产查询**仍报红** | ✅ |
| 闸② `#[cfg(test)] mod tests{…}` 之后再有生产函数 | 区间收尾正确 | 只报后文表，测试区内表不报 | ✅ |
| 闸② 同一文件被普通 `mod` 与 `#[cfg(test)] mod` 双指向 | 不得误标测试面 | **被标为测试面 → 静默变绿**（两个父模块，Rust 合法） | ❌ R2 |
| 闸② `#[cfg(test)] #[path=…]` 凭空挂载生产文件 | 不得静默跳过 | **红 → 绿**（一行即可藏文件） | ❌ R1 |
| D1 硬断言（剔除 `task_model_query.rs`） | 必须变红 | 两条路径均 FAIL | ✅ |
| 抽检 3 个豁免文件（flow / hierarchy / schedule） | 豁免真实、不掩盖用户可见行集 | 3/3 通过 | ✅ |
| 手法检查（allow / `\|\| true` / `set +e` / 默认值 / 提前 exit） | 无篡改 | 无篡改 | ✅ |
| 「G5 报 37 = 28+7+2、0 真违规」 | 独立复现 | 28 方法调用 + 7 本地遮蔽 + 2 类型形参 = 37，真违规 0 | ✅ |
| 闸② 跳过清单 50 个文件 | 不得误跳生产文件 | 50/50 可按 Rust 语义追溯 | ✅（当前树无误跳） |

---

## 1. 复核环境与基线（可复跑）

```
仓库   /opt/cloudpivot
main   2a9db18 fix(scope): 闸② 生产面 10 文件逐条裁定 —— 25 处函数级 SCOPE-EXEMPT
       ^ 5408859 fix(lint): 门禁口径 v2 落地 —— 闸② 只判生产面 + G5 只认 changelog 限定名
python 3.14.9
TMPDIR /root/.hermes/profiles/information-security/cache/scratch   （/tmp 是 100% 满的 tmpfs）
```

**基线数字（在 `git archive 2a9db18` 的干净树上实测，见 `32_gate_runs_on_committed_tree.txt`）**

```
$ python3 scripts/lint/scope_gate.py --root <clean>      → exit 0
闸② 判定面（v2）：扫描生产面 311 个文件 / 跳过测试面 50 个文件
  PASS：0 处
$ python3 scripts/lint/changelog_in_tx.py --root <clean>  → exit 0
G5 PASS：检查了 29 个调用点（changelog::record / changelog::record_in），全部有事务证据（扫了 361 个文件）
```

> **与卡 B 交接数字「312 / 50」的差异已查明**：卡 B 跑的时候工作树里还有本卡之外的未跟踪文件
> `backend/src/test_db_guard.rs`（+1 个生产面文件）。`2a9db18` **已提交**的树是 **311 + 50 = 361**。
> 复核期间并行卡片又加了 `backend/src/business/erp/cancel_cascade.rs`，工作树因此变成 313 + 50 = 363，
> 两条门禁在工作树上同样全绿。这不是回归，是「未跟踪文件也被扫」的保守行为。

**复跑方式**：本目录 `probes/` 下 7 个脚本即本次全部探针（可移植，支持 `PROBE_DIR` / `CP_SRC` 环境变量）：

```bash
export PROBE_DIR=$HOME/probe_work CP_SRC=/opt/cloudpivot/backend/src && mkdir -p $PROBE_DIR
cd docs/evidence/20261103-gate-v2/probes
python3 run_all_probes.py ../31_probe_outputs.txt     # 一条命令重放 §2–§9 的全部原始输出
```

本次全部原始输出：`31_probe_outputs.txt`（427 行）、`32_gate_runs_on_committed_tree.txt`、`33_anticheat_grep.txt`、`34_check_sh_run.txt`。

---

## 2. G5 自建负例（5 条必核，**自写样例、不引用仓库 fixture**）

样例在 `probes/g5_build_and_run.py` 里内联生成（`case01…case12`），跑的是脚本**真实 CLI**
（`--root/--src` 指向我的临时树），并额外灌 30 个合规调用点把判定面撑过 `MIN_CALL_SITES=27`，
避免防假绿下限掩盖真问题。

```
$ changelog_in_tx.py --root <g5fix> --list
G5 FAIL：5 个调用点不合规（检查了 37 个调用点，扫了 12 个文件）
  case01_top_level_db.rs:2:  record(db()) 第一个实参含 db()——这会开第二个事务，回滚时留下假日志
  case02_spawn.rs:3:        record(&tx) 不在任何 transaction(|tx| ...) 闭包内，首参也不是 Transaction 类型的形参
  case03_tx_but_db.rs:3:    record(db()) 第一个实参含 db()——这会开第二个事务，回滚时留下假日志
  case04_shadow.rs:8:       record(conn) 不在任何 transaction(|tx| ...) 闭包内 …
  case11_tx_but_other_handle.rs:3: record(other_pool) 第一个实参里看不到 tx / Transaction 类型，无法证明是同事务执行器
  skip case04_shadow.rs:6  裸调 record(…)（本地 fn record 遮蔽，D4.4）
  skip case04_shadow.rs:7  裸调 record(…)（本地 fn record 遮蔽，D4.4）
  ok   case05_typed_param.rs:3  record(ex) 形参声明为 Transaction：cancel(ex: &Transaction)
  ok   case10_crate_begin.rs:3   record(…) 使用 begin() 创建的显式事务句柄
```

逐条对照期望：

| # | 样例（自写） | 期望 | 实测 | 判定 |
|---|---|---|---|---|
| ① | `pub async fn top_level(db: &Db…) { changelog::record(db(), ctx, …) }` | 报红 | `record(db()) 第一个实参含 db()` | ✅ |
| ② | `tokio::spawn(async move { changelog::record(&tx, …) })`，`tx: &Transaction` 形参 | 报红 | `不在任何 transaction(...) 闭包内` | ✅ |
| ③ | `db.transaction(\|tx\| async move { changelog::record(db(), …) })` | 报红 | `第一个实参含 db()`（`db()` 硬拒优先于 in_tx） | ✅ |
| ④ | 本地 `fn record(rule,&str)→u8` + 2 处裸调 + 事务外 `changelog::record(conn, …)` | 裸调不红、真调用红 | 2 处 skip + 1 处报红（行 8），**恰好 1 条** | ✅ |
| ⑤ | `pub async fn cancel(ex: &Transaction, …) { changelog::record(ex, …) }` | 判绿 | `形参声明为 Transaction：cancel(ex: &Transaction)` | ✅ |

**5/5 与期望一致 → 本项复核通过。**

### 2.1 我加的压力探针（⑥–⑫）

| # | 样例 | 实测 | 意义 |
|---|---|---|---|
| ⑥ | `TREE_CACHE_GATE.record(true)` / `GATE.record(false)` | 0 site / 0 problem | 28 处误报确实消除，且**不再计入**判定面 ✅ |
| ⑦ | `use …changelog::record as audit_write;` 后 `audit_write(db(), …)` | **0 site / 0 problem / 连提示都没有** | ⚠️ 静默漏检（旧版也漏 → pre-existing，R5） |
| ⑧ | `use …changelog::record;` 后裸调 `record(db(), …)` | 只有一行「提示」，**不报红** | ⚠️ **v2 新放宽**（旧版会报红，R4） |
| ⑨ | `changelog::record::<()>(db(), …)`（turbofish） | **0 site / 无提示** | ⚠️ 静默漏检（旧版也漏 → pre-existing，R5） |
| ⑩ | `let tx = conn.begin()…; crate::changelog::record(&tx, …)` | 判绿（begin 句柄） | 原有合规形态未回退 ✅ |
| ⑪ | 事务闭包内但首参是 `other_pool` | 报红 | 保守，不因「在闭包里」就放行 ✅ |
| ⑫ | 业务写用 `biz` 事务、变更日志却挂在**另一个** `side: &Transaction` 上 | **判绿** | D5 固有盲区（静态只看形参类型），见 R6 |

⑫ 的样例（真实残留在 `probes/g5_build_and_run.py` 之外的独立文件，复跑时可复现）：

```rust
async fn write_log(ex: &Transaction, ctx: &ChangeCtx) -> Result<(), ApiError> {
    changelog::record(ex, ctx, target, input).await?;   // ← 判绿
    Ok(())
}
pub async fn outer(biz: &Transaction, side: &Transaction, ctx: &ChangeCtx) -> … {
    biz.exec_sql("UPDATE biz_task SET status = 'DONE'".to_string(), vec![]).await?;  // 业务写 biz
    write_log(side, ctx).await?;                        // 日志写 side —— 静态不可见
    Ok(())
}
```

```
ok  case12_two_tx.rs:4 record(ex) 形参声明为 Transaction：write_log(ex: &Transaction)
```

---

## 3. 「G5 报 37 处、0 处真实违规」的独立复现

不信卡面：我把**旧版脚本**（`git show 5408859^:scripts/lint/changelog_in_tx.py`）在 `2a9db18` 上实跑，
它报 37 处；再用 `probes/classify37.py` 把 37 处逐一回读源码行归类：

```
旧脚本报告的调用点数：37
--- A_method_call: 28 处 ---
    shared/cache_gate.rs:96/107/109/117/118/120/122   gate.record(…)
    shared/session_cache.rs:154/158/160/185/191/195   GATE.record(…)
    system/dept/model.rs:345/348/357/361/363          TREE_CACHE_GATE.record(…)
    system/menu/model.rs:251/254/265/269/271          TREE_CACHE_GATE.record(…)
    system/role/model.rs:415/418/427/431/433          ENABLED_ROLES_GATE.record(…)
--- B_local_shadow: 7 处 ---
    business/flow/linkage/engine.rs:208/220/232/239/242/246/291   runs.push(record(…))   ← 本地 fn record
--- 其余 2 处 ---
    business/flow/model.rs:2802      changelog::record(\n ex,\n …)      ← fn cancel_flow_tasks(ex: &Transaction)
    business/flow/task_node.rs:308   changelog::record(\n ex,\n …)      ← fn spawn(ex: &Transaction)
```

**28 + 7 + 2 = 37，真实违规 0 处——卡面背景事实成立**（我独立复现，未采信对方自测）。

顺带核实这 2 处 `ex` 形态**今天确实与业务写同事务**（不是「随便一个 Transaction」）：

- `model.rs:2756 cancel_flow_tasks(ex: &Transaction, …)`：同一 `ex` 既跑 `UPDATE biz_task …`（:2788）又写日志（:2802）；
  唯一生产调用方 `flow/engine.rs:911` 传的是引擎推进实例状态用的那个 `ex`；
- `task_node.rs:110 spawn(ex: &Transaction, …)`：同一 `ex` 跑 `find_existing` 与日志；
  调用方 `flow/engine.rs:2336 task_node::spawn(self.ex, …)`，是引擎事务句柄。

---

## 4. 闸②「测试面判定」的绕过测试（本次放宽的最大风险面）

在 `backend/src` 的**副本**上做（原始脚本，见 `probes/scope_bypass.py`）：

| 编号 | 构造 | exit | 实测结论 |
|---|---|---|---|
| T0 | 未改动副本（基线） | 0 | `扫描生产面 313 / 跳过 50`，`PASS：0 处`（副本含并行卡片的 2 个未跟踪 `.rs`；干净树为 **311 / 50**，见 §1） |
| T2 | 生产文件里插一句**裸** `#[cfg(test)]`（不给 mod），后文生产查询 `FROM sys_notice` | 1 | `FAIL: business/task/zz_a.rs 查询 sys_notice …` → **不会**把后文当测试区 ✅ |
| T3 | `#[cfg(test)] mod tests {…}`（内含 `FROM biz_task`）**之后**再有生产函数（`FROM sys_user`） | 1 | 只报 `zz_b.rs (sys_user)`；测试区内的 `biz_task` 不报 → `{}` 配平收尾正确 ✅ |
| T4 | 同一文件被**普通 `mod`**（`task/mod.rs`）与 **`#[cfg(test)] mod`**（`task/model.rs`）双指向 | 0 | **`zz_c.rs` 被判为测试面 → 静默变绿**（`--list-skipped` 显示声明位置 `task/model.rs:145`）❌ |
| T5a | 生产文件 `zz_d.rs`（`FROM sys_notice`）先报红（对照） | 1 | 红 |
| T5b | 在**无关文件**里加一行 `#[cfg(test)] #[path = "../task/zz_d.rs"] mod ghost_copy;` | 0 | **红 → 绿**，`--list-skipped` 里出现 `zz_d.rs ← erp/approval_gate.rs:797` ❌ |
| T6a | 把 `task_model_query.rs` 声明成 `#[cfg(test)]` | 1 | `FAIL：防假绿断言：已知生产文件 … 被判成了测试面（声明位置 task/mod.rs:97）` ✅ |
| T6b | 改脚本副本，把 `task_model_query.rs` 从扫描集合里剔除 | 1 | `FAIL：防假绿断言：已知生产文件 … 不在生产面扫描结果里` ✅ |
| T7 | 一个 SCOPE-EXEMPT 注释 + 同文件里另一处**用户可见列表查询**（`SELECT t.* FROM biz_task t JOIN sys_user … LIMIT 20`） | 0 | 整文件豁免连带放过列表查询（**预存在语义**，但 v2 把 10 个文件全豁免后等于对这 10 文件关闭闸②）⚠️ |

**T4 / T5 的机制**

- **T5（R1）**：`collect_test_surface()` 只要求「存在一条 `#[cfg(test)] mod X;`（或 `#[path]`）」就把目标文件整文件跳过，
  没有任何「跳过清单必须等于已知清单」的硬断言。因此**任意一个生产文件**都能被一行指向它的幽灵声明藏起来，
  而 `REQUIRED_PRODUCTION` 只护住 3 个硬编码文件（`task_model_query.rs` / `project/model.rs` / `shared/scope.rs`）。
- **T4（R2）**：`child_module_paths()` 用 `parent.parent` 当子模块目录，只对 `foo/mod.rs` 与 `src/lib.rs|main.rs` 成立；
  对 `foo.rs` 这类父文件，Rust 语义是 `foo/x.rs`，脚本却算成 `foo 的父目录/x.rs`。
  于是 `task/model.rs` 里一条 `#[cfg(test)] mod zz_c;`（Rust 语义 = `task/model/zz_c.rs`）**误命中了生产文件 `task/zz_c.rs`**。
  T4 的两个 `mod` 声明父模块不同（`task::zz_c` 与 `task::model::zz_c`），是**合法 Rust**，不需要同名双声明。

**T2/T3 属好消息**：这两种「最容易被误伤的写法」都判对了，说明 `{}` 配平与行区间收尾没写歪。

### 4.1 解析器边角（U1–U6，`probes/scope_edges.py`）

| 编号 | 构造 | 期望 | 实测 |
|---|---|---|---|
| U1 | `mod tests {` 花括号**未闭合**（文件被截断），后文生产查询 | 保守报红 | 报红 ✅ |
| U2 | 生产查询与 `#[cfg(test)] mod tests {…}` **写在同一行** | 不得吞掉生产查询 | **整行被跳过 → 静默变绿** ⚠️ R3 |
| U3 | `#[cfg(all(test, feature="db"))] mod tests {…}` | 就是测试面，应跳过 | 不报红 ✅ |
| U4 | `#[cfg(not(test))] mod prod_only {…}` | **不许**跳过（只进生产） | 报红 ✅ |
| U5 | `#[cfg(any(test, feature="db"))]` | 不许跳过（可能进生产） | 报红 ✅ |
| U6 | 测试区内再嵌 `mod inner {…}` + 后续生产函数 | 只报后文 | 只报 `sys_user` ✅ |

---

## 5. 跳过清单 50 个文件：按 Rust 语义逐个反查

`probes/audit_skips.py` 对每个被跳过的文件，反查它的声明方，并用**Rust 真实语义**重算候选路径：

```
被跳过的测试面文件：50 个
=== 结论 ===
OK：所有跳过都能按 Rust 语义追溯到真正的 #[cfg(test)] 声明（无误跳）

=== 非 mod.rs/lib.rs/main.rs 里声明 mod 的文件（误解析土壤）===
  business/task/model.rs:89  mod tests;               ← 全部带 #[path = "..."]，故解析正确
  business/erp/stockin_handler.rs:83…131  mod *_tests;
  …共 31 条
```

结论：**当前树上 50 个跳过无一误跳**；T4/T5 是「潜在通道」而非既成事实。
但 31 条「非 mod.rs 父文件里的 mod 声明」正是 R2 的土壤——今天靠 `#[path]` 属性侥幸算对，去掉 `#[path]` 就会错。

---

## 6. 抽检卡 B 的豁免真实性（3 个文件，沿调用链核到 handler）

先用 `probes/sample_exempt.py` 机械核对「每个查询 ↔ 自己的豁免注释」（因为**闸② 的豁免是整文件粒度，
「函数级」只靠注释成立**，所以必须逐个对上）：

```
### task_model_flow.rs        豁免注释 [101]                查询行 [123,127]
### task_model_hierarchy.rs   豁免注释 [31,61,243,410,490,518,604,619]   查询行 [35,68,247,423,494,522,524,527,608,623]
### task_model_schedule.rs    豁免注释 [21,42,63,84,106]     查询行 [26,51,72,93,115]
```

**10 处 / 8 处 / 5 处查询与注释一一毗邻，没有「一处豁免盖住一串查询」的形态。**

### 6.1 `business/task/task_model_flow.rs`（1 处豁免）

| fn（定义行） | 查询 | 产出形态 | 权限在哪校验（我核过） |
|---|---|---|---|
| `flow_ref_of_task` @102 | @123 `biz_task`、@127 `biz_flow_task` | `Option<TaskFlowDetailRow>` 单条，不产出行集 | 端点 `GET /business/task/{id}/flow`：`handler.rs:4505 permission = "business:task:read"`；`handler.rs:4517 model::find_detail(&me, task_id)` → `:4520 .ok_or_else(not_found)`（看不见即 404）；`flow_ref_of_task` 在其后 `:4521` 调用 |

结论：豁免理由回答了两问（不需要数据范围＝按 task_id 单条；权限在 handler 单条可见性），**未掩盖行集** ✅

### 6.2 `business/task/task_model_hierarchy.rs`（8 处豁免，含 1 处返回 `Vec`）

| fn（定义行） | 查询 | 产出形态 | 权限/调用链核实 |
|---|---|---|---|
| `ancestor_chain_sql` @32 | @35 | 只产出 SQL 文本 `String` | 供 `validate_parent` 用，无行集 |
| `validate_parent` @62 | @68 | `DbResult<()>` | handler:365（create）/ :639（update）/ :1628（create_subtask）；:1623 前有 `find_detail` 可见性 |
| `scalar_status` @244 | @247 | `Option<TaskStatus>` 单值 | 只被本文件 `cascade_parent_status` @319 调用 |
| `scan_parent_status_gaps` @411 | @423 | **`Vec<ParentStatusGap>`**（唯一返回行集的） | 唯一生产读者 `handler.rs:4703 repair_parent_status`：`:4684 permission = "business:task:update"` + `:4693 if !claims.is_super_admin() → 403`；另一调用方 `flow/task_bridge.rs:2103` 位于 `#[tokio::test]` 内（非生产） |
| `parent_status` @491 | @494 | `Option<TaskStatus>` 单值 | `handler.rs:1034`（change_status 事务前守卫）/ `:1205`（守卫），前置 `:1005 find_detail` 可见性 |
| `cascade_cancel_sql` @519 | @522/524/527 | 写路径 SQL（`WITH RECURSIVE … UPDATE … RETURNING`） | `handler.rs:1252` change_status 事务内；`flow/task_bridge.rs:2922` 审批回调（系统身份） |
| `parent_of` @605 | @608 | `Option<Uuid>` 单值 | 只在本文件被调用（@156/215/295/315） |
| `scalar_progress` @620 | @623 | `i16` 单值 | 只在本文件 @180 被调用 |

超管诊断接口的关键代码（实测行号）：

```rust
// handler.rs:4683-4699
#[nik::permission(path = "/business/task/admin/repair-parent-status", method = "GET",
                  permission = "business:task:update")]
#[nik::get("/business/task/admin/repair-parent-status")]
pub async fn repair_parent_status(…) -> ResResult<dto::ParentStatusRepairRes> {
    if !claims.is_super_admin() {
        return Err(ApiError::new(403, error_code::TASK_FORBIDDEN,
            "父任务状态巡检是批量修数接口，仅超级管理员可执行"));
    }
```

结论：8 处豁免理由与代码一致；唯一的行集形态是**超管专用运维诊断**，且注入 scope 对超管是 no-op。**未掩盖用户可见行集** ✅

### 6.3 `business/task/task_model_schedule.rs`（5 处豁免，含 3 处返回 `Vec`）

调用链（`schedule.rs` 实测）：

```rust
// :500-504  #[nik::permission(..., permission = "business:task:update")]
// :506      pub async fn schedule(…)
:517  let current = model::find_detail(&mine, task_id)…ok_or_else(not_found)?;   // ① 可见性（看不见 404）
:523  if current.card.owner_id != me && current.creator_id != me && !mine.is_super() {  // ② 写权限
:524      return Err(ApiError::new(403, …, "只有任务的负责人、创建人或管理员可以改期"));
:535  let outcome = db().transaction(|tx| async move {
:545      let Some(row) = model::find_schedule_row(&tx, task_id).await? …
:566      let prerequisites = model::schedule_prerequisites(&tx, task_id).await?;
:567      let successors    = model::schedule_successors(&tx, task_id).await?;
:569      … model::schedule_parent(&tx, parent_id).await? …
:572      let children      = model::schedule_children(&tx, task_id).await?;
:573      check_all(&span, &row, &prerequisites, &successors, parent.as_ref(), &children)  // ← 全部只喂校验
:645  let fresh = model::find_schedule_row(db(), task_id)  // ④ 事务外回读同一 task_id（出参用）
```

| fn | 产出 | 谁来读 | 是否返回前端 |
|---|---|---|---|
| `find_schedule_row` @22 | `Option<ScheduleRow>` | `:545`（事务内）/ `:645`（事务外回读） | 只有日期字段进 `TaskScheduleRes`（单条，同 task_id），非行集 |
| `schedule_prerequisites` @43 | `Vec<PeerSchedule>` | `:566` → `check_all` | **仅校验**，不出参 ✅ |
| `schedule_successors` @64 | `Vec<PeerSchedule>` | `:567` → `check_all` | **仅校验** ✅ |
| `schedule_parent` @85 | `Option<PeerSchedule>` | `:569` → `check_all` | **仅校验** ✅ |
| `schedule_children` @107 | `Vec<PeerSchedule>` | `:572` → `check_all` | **仅校验** ✅ |

结论：3 处返回 `Vec` 的豁免**确实只作依赖/父子区间校验**，没有出现在响应体里；豁免理由两问均回答（不需要数据范围＝
注入会把看不见的前置/后继过滤掉导致校验静默漏判；权限在 `schedule.rs:517` 可见性 + `:523` 写权限）。**未掩盖用户可见行集** ✅

### 6.4 抽检发现的两处**注释不精确**（不影响安全，建议补正）

1. `task_model_schedule.rs:21` 只写了调用方 `schedule.rs:545`，实际还有 `:645` 的事务外回读（同一个已被可见性校验的 task_id，安全无碍）。
2. `task_model_hierarchy.rs:410` 写「**唯一读者**是批量修数接口 handler.rs:4689」，实际还有 `flow/task_bridge.rs:2103`——
   但该处位于 `#[tokio::test] async fn cascade_failure_does_not_block_approval()` 内，属测试代码，生产语义上「唯一读者」成立；
   建议改成「唯一生产读者」以免后续被当成反例。

---

## 7. 手法检查（反作弊，原始输出见 `33_anticheat_grep.txt`）

| 检查项 | 实测 | 结论 |
|---|---|---|
| `BATCHB_GATE` 默认值是否被改成 `warn` | `check.sh:156 if [ "${BATCHB_GATE:-fail}" = "warn" ]` | 默认仍是 **fail** ✅ |
| `set +e` / 提前 `exit 0` | 无 `set +e`；`exit` 只有 297（色板不一致）与 357（ERP 闸③ BLOCKER）两处，均为失败路径 | ✅ |
| `\|\| true` 是否吞门禁错误 | 10 处全在 `grep` 管道尾部（无匹配不报错），**不在**闸②/G5 的判据分支里；闸② 用 `if gate2_out="$(python3 …)"; then/else failed++`，非 0 即计入失败 | ✅ |
| 被注释掉的门禁调用 | 无。命中的 3 行是说明性注释（提到 `scope_gate.py` 名称）；真实调用在 `check.sh:138`（闸②）与 `:251`（G5），都是硬调用 | ✅ |
| 门禁脚本里的「0 命中放行」 | G5 有 `MIN_CALL_SITES = 27` + `coverage_ok()`，`call_sites<27` 直接 FAIL；闸② 有 `SCAN_DIRS` 覆盖断言 + `REQUIRED_PRODUCTION` 防假绿断言 | ✅ |
| `#[allow(` / `noqa` / `type: ignore` | 门禁脚本 0 处；`backend/src` 37 处全是 `clippy::too_many_arguments` / `dead_code`，A/B 两个提交**未新增** | ✅ |
| A 提交是否夹带业务改动 | `5408859` 只动 `scripts/check.sh`、`scripts/lint/*.py`、`docs/` | ✅ |
| B 提交是否夹带代码改动 | `2a9db18`：10 个 `.rs` **+25 行纯注释**，零代码/SQL/签名变更（`--numstat` 逐文件核对：1+1+1+2+2+1+1+8+5+3=25） | ✅ |

---

## 8. 残留风险（verdict = pass-with-notes 的全部依据）

> 严重度按「能否让**真实违规**静默通过」评，不按利用难度评。当前树上**没有**既成事实，全部是通道。

### R1（中）闸② 跳过清单无硬断言 → 一行幽灵声明即可藏掉任意生产文件
- 证据：T5b（红 → 绿，`#[cfg(test)] #[path = "../task/zz_d.rs"] mod ghost_copy;`）
- 影响：`REQUIRED_PRODUCTION` 只护 3 个文件；其余 308 个生产面文件均可被这样藏起来。
- 最小修法（建议，不阻断终验）：把跳过清单钉成清单（文件相对路径集合 + 数量），与实测不一致即 FAIL；
  或断言「被跳过的文件只能来自 mod 名为 `tests*`/`*_tests`/`*_support` 且声明方在 `#[cfg(test)]` 之下」。
  照 G5 的 `MIN_CALL_SITES` 风格写即可，约 15 行。

### R2（中）子模块路径解析对非 `mod.rs` 父文件算错 → 合法 Rust 造成误跳
- 证据：T4（`task/model.rs` 的 `#[cfg(test)] mod zz_c;` 把生产文件 `task/zz_c.rs` 判成测试面 → 静默变绿）
- 影响：仓库里已有 31 条「非 mod.rs 父文件的 mod 声明」，今天靠 `#[path]` 侥幸算对；一旦有人省略 `#[path]`，误跳就成立。
- 最小修法：`rust_parent_dir(parent) = parent.parent if parent.name in ("mod.rs","lib.rs","main.rs") else parent.parent/parent.stem`。

### R3（低）行粒度跳过 → 生产查询与 `#[cfg(test)] mod` 同行会被吞
- 证据：U2（整行被跳过，`sys_notice` 未报红）
- 影响：需要刻意把生产代码写在同一行，现实罕见；生成代码/压缩代码风险略高。
- 最小修法：跳过一个区域时，对该区域**起始行的前缀部分**（`mod` 声明之前）仍做一次 `FROM` 判定。

### R4（中低）**G5 新放宽**：非 changelog 模块里的裸调只提示、不判红
- 证据：⑧（`use …changelog::record;` + `record(db(), …)` → 只有「提示」行，exit 0）
- 对比：**旧版会报红**（旧正则匹配任意裸 `record(`），所以这是 v2 唯一一处「真实违规不再被抓」的放宽。
- 缓解：`check.sh` 会把提示打印出来（非阻断）；当前仓库 0 处此类导入。
- 最小修法：脚本已算出 `local_record_def`；在**没有**本地 `fn record(` 的文件里，裸调应直接判红而不是提示。

### R5（低，pre-existing）别名导入 / turbofish 形态完全静默
- 证据：⑦（`use … as audit_write;` → 0 site）、⑨（`changelog::record::<()>(…)` → 0 site）
- 说明：旧版同样漏（旧正则也要求 `record` 后紧跟 `(`），不是本次引入。
- 最小修法：检测 `use .*changelog::(record|record_in)( as (\w+))?` 导入，对别名与原名都纳入判定面；正则放宽 `record(_in)?\s*(::<[^>]*>)?\s*\(`。

### R6（低，静态判据固有）D5 类型证据不辨「是不是同一笔事务」
- 证据：⑫（业务写 `biz`、日志写 `side`，两者都是 `&Transaction` → 判绿）
- 说明：静态只能证明「首参是某个 Transaction 句柄」，运行时是不是同一笔要靠 `tests/batchC_c7_rollback.rs` 的 `0|1|0` 真库断言。
  脚本头部已诚实声明这一点（「能证/不能证」），我确认声明与实现一致。
- 建议：在 `docs/门禁口径-v2-20261103.md` 的 D5 小节把这条盲区显式登记（现在只在脚本 docstring 里）。

### R7（低，预存在语义）SCOPE-EXEMPT 是**整文件**粒度
- 证据：T7（一处豁免 + 同文件另一处用户可见列表查询 → 全绿）
- 现状：卡 B 把 10 个生产面文件全部豁免（25 处），因此闸② 对这 10 个文件**等于关闭**；这 10 个文件的地基只剩「人工复核」。
  本次抽检了 3 个（flow / hierarchy / schedule，覆盖 25 处豁免中的 1 + 8 + 5 = 14 处），未发现掩盖；
  剩余 7 个文件（approval_gate / contract_handler / reminder / stockin_bridge ×2 / task_node ×2 / template_vars / task_model_write ×3）
  的依据是卡 B 的 `21_*_证据.md`，**本卡未逐个复核**（超出我卡面点名的 3 个抽样）。
- 最小修法（建议后续卡）：把豁免收敛成函数级白名单（脚本按 `fn 名` 匹配而不是 `"SCOPE-EXEMPT" in text`），
  或在口径文档登记 25 处豁免的逐条清单，避免「一处注释盖住将来新增的第二个查询」。

### 非安全类小问题（一并记录）
- `changelog_in_tx.py --src` 不在 `--root` 之下时抛 `ValueError: … is not in the subpath of …`（`:352 relative_to`）。
  行为是 fail-safe（非 0 退出，不会假绿），但报错不友好；建议 `--src` 缺省由 `--root` 推导并在跨根时给清晰错误。

---

## 9. 本卡的诚实边界

1. **未跑真库回归**：本卡是静态复核，运行时语义（`0|1|0` 回滚断言、业务用例）由卡 D 负责；
   卡 B 交接里的真库数字我**没有**复跑（task 210 / flow 345 等），只核了它们与本次改动无冲突（B 只加注释）。
2. **静态判据的证明上限**：G5 只能证「有静态可读的事务证据」，不能证「运行时是同一笔事务」（R6）；
   闸② 只能证「有 `scope::` 或豁免注释」，不能证「注入的 scope 真的挡住了越权行」（那靠真库用例）。
3. **工作树非纯净**：复核期间并行卡片在 `/opt/cloudpivot` 放入了未跟踪文件
   `backend/src/test_db_guard.rs`、`backend/src/business/erp/cancel_cascade.rs`。
   所有权威数字都取自 `git archive 2a9db18` 的干净树（§1）；工作树数字单独标注。
4. **豁免只抽了 3 个文件**（卡面点名的），剩余 7 个生产面文件未逐个复核（见 R7）。
5. **我没有改任何实现**：本卡只落 `docs/evidence/20261103-gate-v2/30_*`、`31_*`、`32_*`、`33_*`、`34_*` 与 `probes/`。

---

## 10. 结论与建议

**verdict：pass-with-notes。**

- 5 条必核负例 **5/5** 与期望一致；D1 防假绿硬断言**双路径**（改声明 / 改扫描集合）均生效；
- 「37 = 28 + 7 + 2、0 真违规」**独立复现成立**；两处 `ex: &Transaction` 今天确实是业务事务本身；
- 闸② 跳过的 **50 个文件 50/50** 可按 Rust 语义追溯，当前树**无误跳**；
- 抽检的 **3 个豁免文件**（14 处豁免）理由真实、调用链上的权限校验我逐个核对过（handler 中间件码 + `find_detail` 可见性 404 +
  负责人/创建人/超管写权限 + 超管 `is_super_admin`），**没有**用豁免掩盖用户可见行集；
- 手法检查无篡改：`BATCHB_GATE` 默认仍 `fail`、无 `set +e` / 提前 `exit 0` / 被注释掉的门禁调用、A/B 提交无夹带业务代码。

**因此不阻断终验（卡 D）**。但两条放宽确实留下了通道，建议后续（不阻断）按 R1 → R2 → R4 的顺序补硬断言：
R1（跳过清单硬断言）与 R2（子模块路径按 Rust 语义）都只改 `scope_gate.py`，约 20 行，能把 T4/T5 两条「红变绿」堵死；
R4 改 `changelog_in_tx.py` 一处分支（约 3 行），可把 v2 唯一一处真实放宽收回到与旧版同等严格。
R5/R6/R7 建议以「口径文档登记 + 真库用例兜底」的方式管理，不必现在改脚本。
