# t_06c528fc · transfer 真库用例同库二次运行残留（夹具清理 + 唯一索引 409 化）· 验收证据

> 卡面：`t_06c528fc`（来源 P-FLOW-3B `t_50b3bdc5`）
> 日期：2026-11-02 · 执行：backend-developer
> 基线代码：`0828a76`（origin/main）
> 涉及文件：`backend/src/business/erp/transfer_handler.rs`、`task/handler.rs`、`erp/model.rs`、`erp/voucher_query.rs`、`erp/voucher.rs`、`scripts/verify/t06_transfer_twice.sh`

---

## 一、结论先行

| 口径 | 结果 |
|---|---|
| **验收口径**：同一个干净库连跑两遍全量 `cargo test --lib -- --test-threads=1` | 第一遍 **1879 passed / 0 failed**（538.48s）· 第二遍 **1879 passed / 0 failed**（537.99s）· 脚本判定 `PASS` |
| 修前、同一个库的第二遍（基线 `0828a76`） | **1817 passed / 53 failed**（502.16s）—— 完整复现 D7 交接里记的「同一库连跑第二遍会红 53 条」 |
| 失败分布（修前第二遍） | `transfer_handler` 30 · `stockin_handler` 19 · `voucher` 3 · `cash_flow` 1 |
| 最终代码 | `33c21dd`（rebase 到 origin/main `092bcc2`，含 V0192 迁移；库 `cloudpivot_t06d` 已迁到 136 个迁移） |

三件事全部落地：① transfer 夹具清场按「先审批痕迹后主表」重写并补「最后一条用例没人清」的收尾用例；② 提交审批撞 `uk_biz_flow_instance_biz_active` 现在翻成 **409 + 单号 + 「已有审批中实例」**（不再是「单号重复，请联系运维排查 biz_seq」，也不再可能掉成 500）；③ 顺带揪出并修掉造成「53 条」的**真正凶手**（兄弟模块夹具，见 §三）。

---

## 二、改了什么

### 2.1 transfer 夹具清场：顺序 + 范围（`erp/transfer_handler.rs::db_tests::cleanup`）

旧版只有一句 `DELETE FROM biz_flow_instance WHERE def_id = … OR (biz_type='STOCK_TRANSFER' AND biz_id IN …)`，
然后直接删明细/单据头。四个缺口：

| # | 缺口 | 后果 |
|---|---|---|
| 1 | `biz_stock_transfer.approval_instance_id` 是**弱引用**（无外键），删实例后单据仍指着它 | 门闸 `flow_task_bridge::instance_status_of` 查不到实例 → 按「不拦」放行（脏数据静默削弱审批门闸） |
| 2 | `biz_flow_instance.prev_instance_id` 是**自引用、且没有 ON DELETE**（V0122） | 若存在指向本卡实例的外部引用，整批 DELETE 报 `query failed: db error`（PG 不给约束名/表名） |
| 3 | 待办/抄送只靠 `ON DELETE CASCADE`，没有显式先删 | 迁移将来摘掉 CASCADE 就会静默留残留 |
| 4 | `biz_status_log` **没有外键**（日志要比单据活得久是设计），从没清过；且**最后一个用例**的产物没人收（`cleanup()` 只在每个用例开头跑） | 每跑一轮攒几百行永远对不上的孤儿日志；最后一例若停在 PENDING 就留着在跑实例 |

现在的顺序（逐条见代码注释）：

```
① UPDATE biz_stock_transfer SET approval_instance_id = NULL   -- 弱引用先断
② UPDATE biz_flow_instance SET prev_instance_id = NULL        -- 自引用外键先解开
③ DELETE biz_flow_task / biz_flow_cc（按 instance_id）        -- 显式先删子表
④ DELETE biz_flow_instance                                   -- 实例本体
⑤ DELETE biz_status_log（entity_type='TRANSFER' AND entity_id IN 本卡单据）-- 必须在删单据之前
⑥ DELETE 序列号子表 → 明细 → 单据头
```

范围口径也修正了两处：`from_warehouse_id` **或** `to_warehouse_id` 命中（「在途中转」用例的出发仓是在途仓，只看调出仓会漏）；
孤儿实例（`def_id = 迁移种的调拨流程 AND biz_id` 指向已不存在的单据）一起收走 —— 上一轮被 panic 打断留下的正是这类。

### 2.2 收尾清场用例（模块级 teardown）

新增 `zz_teardown_purges_fixture_residue`（名字 `zz_` 排最后 = 给模块加 teardown）：
自造一张**带在跑审批单**的单 → `cleanup()` → 断言「单据 / 实例 / 待办 / 状态日志」按 id 全为 0，
并追加一条硬断言：**全库不许剩 `STOCK_TRANSFER|PENDING` 实例**（顶住唯一索引的就是它）。

### 2.3 唯一索引冲突 → 409 + 单号 + 「已有审批中实例」

判据与 `flow::handler::is_biz_active_duplicate` 同口径（**变体优先 + 文案兜底**）：

```rust
fn is_biz_active_duplicate(error: &DbError) -> bool {
    match error {
        DbError::UniqueViolation { constraint, .. } =>
            constraint.as_deref().map(|n| n.contains(BIZ_ACTIVE_INDEX)).unwrap_or(false),
        other => other.to_string().contains(BIZ_ACTIVE_INDEX),   // 驱动没给 SqlState 时兜底
    }
}
```

两个分支都要，原因分开说清楚（`vendor/nik-db-core/src/error.rs:223`、`:294`）：

- **变体分支**：PG 返回 `23505` 时驱动会翻成 `UniqueViolation`，并从消息里解析出约束名（`unique constraint "…"`），
  这条是**实测走得到**的主路径（真库用例 `submit_twice_is_409_with_transfer_no_not_500` 里断言了真驱动返回的错误能被判据识别）；
- **文案兜底**：拿不到 `SqlState` 时错误会降级成 `DbError::Query`，变体判据失效 → 旧代码会一路掉到
  `utils::db_error` → **500「服务器内部错误」**。这条兜底就是给那个形态留的。

旧代码在这条路径上的**行为（按代码复算）**：`write_error` 命中 `UniqueViolation` 分支后返回
`409 + 66001`，文案是「调拨单号重复（唯一约束 uk_biz_flow_instance_biz_active）…请联系运维排查 biz_seq」
—— **状态码对、文案把人指到取号器那边去**；一旦错误降级成 `Query` 就是 500。
现在两种形态都收敛成同一句可执行提示（新真库用例拿真驱动返回的错误验判据，见上）。

接进三处入口：

| 入口 | 改动 |
|---|---|
| `prepare_start`（应用层预检 `biz_instance_running`） | 原来「该调拨单已有在跑的审批单」→ 现在带**单号** + 「已有审批中实例」 |
| `change_transfer_status` 的 `Err(SettleError::Db)` | 先判 uk 冲突 → `409 + 单号`；其余走 `write_error` |
| `write_error`（通用出口） | 顶部先判 uk → `approval_in_progress(None)`；避免掉进上面那段「单号重复」文案或降级成 500 |

文案：`调拨单 TK-… 已有审批中实例（同一单据同时只能有一个在跑的审批单）：请刷新查看审批进度，或在审批中心撤回后再重新提交`（码 `RESOURCE_CONFLICT` / 409）。

真库用例 `submit_twice_is_409_with_transfer_no_not_500` 造的就是**唯一能触发预检的状态**：
单据停在 `DRAFT` 但已经挂着一张在跑审批单（= 上一轮 panic 没清干净的残留现场）；
「已 PENDING 再提交」会被状态机先 409 掉（非法边），测不到预检这一步。

### 2.4 兄弟模块的同类夹具缺陷（「53 条」的真凶）

| 文件 | 缺陷 | 影响 |
|---|---|---|
| `business/task/handler.rs`（D9 复制用例夹具） | 建了超管夹具用户 `d9_copy_user`（`SUPER_ADMIN`）与它的任务，**从不删** | 审批节点 `assignee_source = ADMIN` 会**动态展开成「当前全部超管」**；第二遍全量时多出一条待办 → `approve_flow_instance` 只批掉一条 → 实例停在 `PENDING` → transfer(30) / stockin(19) / voucher(3) 的「审批通过即落终态」用例集体红 |
| `business/erp/model.rs`（W1B 夹具） | 夹具科目 `W1B1001 / W1B100101` 不清理 | `cash_flow::tests::r3_real_config_covers_all_accounts` 按「`biz_account` 全表」断言分类覆盖 → 第二遍红「分类配置漏科目」 |
| `business/erp/voucher_query.rs`（P2D1 夹具，ledger 共用） | 夹具科目 `P2D1A / P2D1B` 不清理 | 同上 |
| `business/erp/voucher.rs`（P2C1 夹具） | 夹具科目 `P2C1.1405` 不清理 | 同上 |

四处都补了 `zz_teardown_*` 收尾用例（删除 + 断言 0），与 P-FLOW-3B 的 ⑰ 同一范式。
`biz_account` 没有任何外键引用（分录按 `account_code` 冗余存），删除安全；用户夹具按「评论/工时/成员 → 断开父指针 → 任务 → 角色关联 → 用户」顺序删。

---

## 三、根因复盘（对照实验，全部真跑）

| # | 场景（同一个库） | 代码 | 结果 |
|---|---|---|---|
| 1 | 干净库，只跑 `transfer_handler` 第一遍 | 基线 | 50/50 绿 |
| 2 | 干净库，`transfer_handler` 连跑**第二遍** | 基线 | **50/50 绿** —— 说明「transfer 自己的残留」并不是红的原因 |
| 3 | 库里带上一轮残留，跑**全量第一遍** | 基线 | 1867/0 绿 |
| 4 | **同一个库跑全量第二遍** | 基线 | **1817 / 53 红**（复现 D7 记的「53 条」） |
| 5 | 同一个库跑全量第一遍 | 修复后（`00a865e`，V0191 库） | **1874 / 0 绿** |
| 6 | 同一个库跑全量第二遍 | 修复后（`00a865e`，V0191 库） | **1874 / 0 绿** |
| 7 | 同一个库跑全量第一遍 | 修复后 + rebase（`33c21dd`，V0192 库） | **1879 / 0 绿** |
| 8 | 同一个库跑全量第二遍 | 修复后 + rebase（`33c21dd`，V0192 库） | **1879 / 0 绿**（脚本 `t06_transfer_twice.sh` 判 PASS） |

**所以卡面「只要第二次进入 transfer 用例就会红」这个判断在基线代码上并不成立**（第 2 行）；
真正成立的是「同一个库跑第二遍全量会红 53 条」，而红的原因不在 transfer 自己的残留，
而在 D9 夹具留下的**超管用户**（第 4 行的 52 条）与三处**科目夹具**（第 4 行的 1 条）。
卡面最早看到的「夹具写入失败（清理审批实例）→ query failed: db error」**在当前基线已不可复现**
（用旧 SQL 在残留库上手工执行 `DELETE FROM biz_flow_instance …` 返回 `DELETE 1`，见 §四）。
把清场顺序按 §2.1 写死、并补模块级 teardown，是为了让这类失败**在结构上不可能再出现**，而不是靠运气。

残留计数对照（各跑完一遍全量后直接查库）：

| 库 | 代码 | 调拨单据 | STOCK_TRANSFER 实例 | TRANSFER 状态日志 | 夹具超管用户 | 夹具科目 |
|---|---|---|---|---|---|---|
| `cloudpivot_t06`（旧） | 基线 | 2（含 1 条 P1-FIX 夹具） | 1（CANCELLED） | **486** | 1（`d9_copy_user`） | 5 |
| `cloudpivot_t06b`（新） | 修复后 | 1（P1-FIX 夹具，非本卡） | **0** | **0** | **0** | **0** |

---

## 四、验收命令与输出

### 4.1 验收口径：同一个库连跑两遍全量（脚本）

```bash
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_t06d' \
  bash scripts/verify/t06_transfer_twice.sh
```

真实输出（`33c21dd`，干净库 `cloudpivot_t06d`，已迁到 136 个迁移 / 含 V0192）：

```
==> 第 1 遍：cargo test --lib -j 2 -- --test-threads=1
    test result: ok. 1879 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 538.48s
==> 第 2 遍：cargo test --lib -j 2 -- --test-threads=1
    test result: ok. 1879 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 537.99s
==> PASS：同一个库连跑两遍全量都全绿
```

跑完两遍后直接查库（确认没有边跑边攒残留）：

```
biz_flow_instance(biz_type='STOCK_TRANSFER') = 0
biz_status_log(entity_type='TRANSFER')       = 0
biz_account(W1B% / P2C1% / P2D1%)            = 0
sys_user(d9_copy_user)                       = 0
```

1879 = 基线 1867 + 本卡新增 7 条 + origin/main 上前端/后端的 P-FLOW-3B 补漏 5 条（V0192 相关，rebase 带进来）：
本卡新增的 7 条 = transfer 纯逻辑 1（`biz_active_conflict_maps_to_409_with_transfer_no`）+
transfer 真库 2（`submit_twice_is_409_with_transfer_no_not_500`、`zz_teardown_purges_fixture_residue`）+
收尾清场 4（`task::handler` 超管用户、`erp::model` / `erp::voucher_query` / `erp::voucher` 三处科目夹具）。

### 4.2 本卡模块（同一库连跑两遍）

```
cargo test --lib transfer_handler -- --test-threads=1
  → 54 passed / 0 failed（108.89s）
  → 54 passed / 0 failed（108.97s）   ← 第二遍，同一库，仍全绿
cargo test --lib zz_teardown -- --test-threads=1
  → 6 passed / 0 failed
```

### 4.3 静态检查

```
cargo fmt -- --check / rustfmt +1.94.1 --edition 2024 --check   → 绿
cargo clippy --lib -- -D warnings                               → 绿
cargo clippy --lib --tests -- -D warnings                       → 存的 7 类报错全在存量文件
                                        （internal_control.rs 的 useless_vec 等），本卡 touched 文件 0 条
```

---

## 五、遗留 / 建议单独开卡

1. **兄弟模块同款文案**：`stockin_handler` / `purchase_handler` / `payable_payment` / `voucher_write` 各自的
   `write_error` 仍会把 `uk_biz_flow_instance_biz_active` 说成「单号重复（唯一约束 …）：取号器与库里的号段不一致，
   请联系运维排查 biz_seq」。是 409 不是 500，但文案指错方向；建议按本卡同一判据统一。
2. **PO_APPROVE / STOCK_IN_POST 夹具仍会逐轮累积 PENDING 实例**（跑完一轮后库里 21 + 15 条）。
   实测**不影响**第二次全量（本卡 §三 第 5/6 行），但属同类隐患，建议各自补收尾清场。
