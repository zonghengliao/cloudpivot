# t_29e8f4ee · tests/ 六处 TaskCreateReq/NewTask 缺 doc_type/doc_id —— 复验与归因

> 卡：`t_29e8f4ee`（父卡 `t_20c290c5`）· 2026-11-05 · 执行者 backend-developer
> 环境：干净 worktree `/root/t29-wt`（detached `c947425`）· 独立 target `/root/t29-target`
> 库：`scripts/verify/erp_clean_db.sh` 新建 `cloudpivot_test_t29e8f4ee`（迁移 max=250 / 192 行）派生的 4 个隔离库

## 一、结论先说

**本卡要求的六处调用点，已经在 main 的 `96d6a42`（2026-11-05 05:39:54）补齐，本卡不产生任何代码改动。**

时间线（可核）：

| 时间 | 事件 |
|---|---|
| 04:26:46 | 本卡由 `t_20c290c5` 创建（`created_at=1793824006`） |
| 05:39:54 | `96d6a42 test(task): 补齐 NewTask/TaskCreateReq 夹具的 doc_type/doc_id —— clippy --all-targets 转绿`（另一张卡顺手收口，已在 `origin/main`） |

`96d6a42` 的改动与卡面要求的**逐字一致**（diffstat：4 文件 +15 行，只有 `doc_type: None` / `doc_id: None`
与注释，无一条断言被改）：

```
 backend/tests/batchT_t1_task.rs            | 6 ++++++   （:269 create_task 夹具 / :470 CRUD 用例，model::NewTask）
 backend/tests/linkage/task_flow_linkage.rs | 3 +++      （:34 make_task 夹具，task_dto::TaskCreateReq）
 backend/tests/pflow1a.rs                   | 3 +++      （:1143 flow_detail_is_null_for_manual_task）
 backend/tests/pflow1b.rs                   | 3 +++      （:1395 plain_task_untouched）
```

所以本卡的交付物 = **独立实测的验收证据 + 9 条红用例的归因**（卡面第 3 条要求「红的话先分清是不是本卡引起」）。

## 二、验收项 ①：`cargo clippy --lib --tests -- -D warnings`

```
cargo clippy --lib --tests -- -D warnings   →  CLIPPY_EXIT=0
Finished `dev` profile [unoptimized + debuginfo] target(s) in 3m 18s
```

原文：`t29-clippy.log`（322 行，无 warning / error，尾部即 EXIT=0）。

补充更硬的一条：`cargo test --tests --no-run` → `NORUN_EXIT=0`，**20 个集成测试目标全部编出可执行文件**，
`Finished test profile in 8m 20s`，全日志 `^warning|^error` 命中 0。原文：`t29-norun.log`。

## 三、验收项 ②：四个受影响目标实跑（真库，非 SKIP）

一律真库（`LINKAGE_REQUIRE_DB=1`，其余三个目标已设 `*_DB_URL`；日志里没有 `SKIP:` 行），
每套一个从同一迁移指纹拷出的隔离库，避免互相污染。原文：`t29-tests.log`。

| 目标 | 结果 | 说明 |
|---|---|---|
| `--test pflow1a` | **12 passed; 0 failed** | 含被改的 `flow_detail_is_null_for_manual_task` → ok |
| `--test pflow1b` | **14 passed; 0 failed** | 含被改的 `plain_task_untouched` → ok |
| `--test batchT_t1_task` | **5 passed; 0 failed** | 含两处 `model::NewTask` 夹具 → crud_roundtrip / task_no_is_unique_and_increasing ok |
| `--test linkage` | **53 passed; 9 failed** | 9 条红与本卡无关，归因见下节 |

## 四、验收项 ③：linkage 那 9 条红是谁引起的（已用 bisect 反证）

9 条红 = `stockin_chain::s06/s07/s08/s09/s10/s11/s12/s15`（8 条，同一句 panic）+ `voucher_chain::v06`。

### 4.1 反证实验（决定性）

另开 worktree `/root/t29-base` 停到 **`a802403`**（含 `96d6a42` 的补字段，但**早于** `ed81995` P1-FIX-1 与
`bdac9f2` P0-FIX-4A），用同一脚本建干净库 `cloudpivot_test_t29baselink`（迁移 max=249 / 189 行），
跑同一批用例：

```
cargo test --test linkage -- --test-threads=1 stockin_chain
  → 16 passed; 0 failed（s06/s07/s08/s09/s10/s11/s12/s15 全 ok）          原文 t29base-stockin.log
cargo test --test linkage -- --test-threads=1 v06_balanced_manual_voucher_is_posted
  → 1 passed; 0 failed                                                    原文 t29base-v06.log
```

⇒ **同一批用例、同一套建库配方，只要把「补字段」之后的两张卡摘掉就全绿**；补字段（`96d6a42`）本身不是红因。

### 4.2 两条红的真实成因

1. **8 条 stockin 作废链**（`stockin_chain.rs:209` `取消作废审批失败 65018 节点「作废复核」没有任何可用审批人`）
   —— 由 **`ed81995`（P1-FIX-1 / `t_e63b600f`，V0248）** 把 `stock_in_cancel.n_review` 从 `ADMIN` 改成
   `ROLE=WAREHOUSE_MANAGER` + `empty_assignee=ERROR`（不再兜底超管）。
   干净库里 `WAREHOUSE_MANAGER` **0 成员**（实测：`cloudpivot_test_t29e8f4ee` 上
   `select r.role_code,count(ur.user_id)` → `WAREHOUSE_MANAGER|0`），测试夹具也没自建该岗位成员
   ⇒ 必红。生产的库该角色有 2 名成员（见 `docs/evidence/20261105-p1fix1/README.md` §备注 235 行），
   所以这是**「干净库上必红的夹具依赖」**，不是产品缺陷。
2. **`voucher_chain::v06`**（`手工凭证落库即 POSTED`，`left: "DRAFT" / right: "POSTED"`）
   —— 由 **`bdac9f2`（P0-FIX-4A / `t_1ff16362`）** 引入凭证三态（手工凭证落 `DRAFT`，`AUDITED` 后才 `POSTED`），
   用例断言没跟上 ⇒ 陈旧断言。

两条都与 `doc_type` / `doc_id` 无关（它们连任务表都不碰）。

## 五、复跑命令

```bash
# 1) 干净树 + 独立 target（避免与共享工作区抢 target 锁 / 被别人 25 个脏文件干扰）
cd /opt/cloudpivot && git worktree add /root/t29-wt --detach <COMMIT>
export CARGO_TARGET_DIR=/root/t29-target

# 2) 建干净库（一次实例完成迁移 + bootstrap）
cd /root/t29-wt && REPO=/root/t29-wt BIN=/root/t29-target/debug/cloudpivot \
  ERP_DB=cloudpivot_test_t29e8f4ee PORT=18179 RECREATE=1 bash scripts/verify/erp_clean_db.sh

# 3) 每套拷一个库（库名必须含 pflow1a / pflow1b / cloudpivot_test，否则用例自己 assert 拦住）
for t in linkage pflow1a pflow1b task; do
  su - postgres -c "createdb -O alun -T cloudpivot_test_t29e8f4ee cloudpivot_test_t29_$t"
done

# 4) 编译门 + 四个目标
cd /root/t29-wt/backend
cargo clippy --lib --tests -- -D warnings
cargo test --tests --no-run
LINKAGE_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test_t29_linkage' LINKAGE_REQUIRE_DB=1 \
  cargo test --test linkage -- --test-threads=1
PFLOW_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test_t29_pflow1a' cargo test --test pflow1a
PFLOWB_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test_t29_pflow1b' cargo test --test pflow1b
TASK_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test_t29_task' cargo test --test batchT_t1_task
```

## 六、证据文件

| 文件 | 内容 |
|---|---|
| `t29-clippy.log` | `cargo clippy --lib --tests -- -D warnings` 全文（尾部 `CLIPPY_EXIT=0`） |
| `t29-norun.log` | `cargo test --tests --no-run` 全文（20 个 Executable，`NORUN_EXIT=0`） |
| `t29-tests.log` | 四个目标实跑全文（linkage / pflow1a / pflow1b / batchT_t1_task） |
| `t29base-build.log` | 反证用 `a802403` 的 `cargo build --bin cloudpivot` + linkage 目标编译 |
| `t29base-db.log` | 反证库 `cloudpivot_test_t29baselink` 的建库指纹（max=249 / 189 行 / 超管 1 人） |
| `t29base-stockin.log` | 反证：`stockin_chain` 16 passed / 0 failed |
| `t29base-v06.log` | 反证：`v06_balanced_manual_voucher_is_posted` 1 passed / 0 failed |

## 七、口径说明（别被数字骗了）

- 验证树停在 `c947425`；跑完之后共享工作区 main 又前进了 `7ef02f9`，但 `git diff --stat c947425 7ef02f9`
  只有 `scripts/deploy/*` + `docs/evidence/*`，**没有一个 Rust 文件**，对本卡的编译门无影响。
- 共享工作区 `backend/tests/` 在验证前后都是干净的（`git status --porcelain` 无 tests/ 条目），
  本卡没有新增/修改任何测试文件，因此也没有提交代码（`96d6a42` 已在 main）。
