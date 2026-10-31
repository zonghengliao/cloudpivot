# 在途调拨超期告警（LNK-F12）

> 一句话：调拨单发货后长期没人点「收货」时，系统现在**查得出来、也推得到人**了。
> 阈值走 `sys_config`（默认 7 天），每小时扫一次，告警走既有站内通知设施。

---

## 1. 这个功能补的是什么洞

调拨单 6 态状态机（`DRAFT → PENDING → APPROVED → IN_TRANSIT → DONE`，另有 `CANCELLED`）里，
`IN_TRANSIT`（已发货未收货）是**唯一两边都不记账**的中间态：

| 环节 | 库存记账 |
|---|---|
| `APPROVED → IN_TRANSIT`（发货） | **不动库存**，只回填 `transit_warehouse_id` |
| `IN_TRANSIT → DONE`（收货） | 同一事务里「调出仓 −qty / 调入仓 +qty」双写（`transfer_model::settle_transfer`） |

于是「发货方点了发货、收货方一直没点收货」的单子会永久停在 `IN_TRANSIT`：
**库存既不在调出仓也不在调入仓**，账实差异只能靠人肉发现。改造前全仓没有任何入口把这类单推给人
（`grep -rn "IN_TRANSIT" --include=*.rs src/ | grep -i "timeout|alert|超时|告警"` 零命中，
`sys_job` 里也没有对应任务）。

改造后四个入口：

| 能力 | 入口 |
|---|---|
| 判定是否超期 | `transfer_model::is_in_transit_overdue`（纯函数）+ 列表 SQL（同一分界点） |
| 查出来 | 列表筛选 `GET /business/erp/transfer/page?in_transit_overdue=true` |
| 看总量 | `GET /business/erp/transfer/overdue/summary` |
| 推给人 | 定时任务 `transfer_overdue_scan`（每小时第 5 分钟）→ 站内告警 |

---

## 2. 判定口径（唯一真源）

```text
超期在途 =
      status = 'IN_TRANSIT'
  AND shipped_at IS NOT NULL
  AND shipped_at < now() - interval '阈值天数'
```

| 项 | 口径 | 为什么 |
|---|---|---|
| 时间基准 | **`shipped_at`（发货时间）** | 压货的起点是「货离开调出仓」；用 `created_at` 会把「审批走了三天」的单当成压货，`approved_at` 同理 |
| 阈值 | `sys_config.erp.transfer.in_transit_overdue_days`，缺省 **7 天** | 业务上「一周没动静就该催」，且可由运维在管理端改（无需发版） |
| 边界 | **严格小于**：`shipped_at` 恰好在 `now - 阈值` 时**不算**超期 | 纯函数与 SQL 用同一个分界点（`transfer_model::cutoff_of`），不允许「函数说超期、SQL 说不超期」 |
| 状态 | 只有 `IN_TRANSIT`；`DONE` / `CANCELLED` / 其余一律不算 | 已完成、已取消的单子不存在压货 |
| 脏数据 | `IN_TRANSIT` 但 `shipped_at` 为空 → 不判超期 | 库约束 `ck_biz_transfer_state` 已保证不会出现；判定层再兜一层，避免把脏数据误报成压货 |
| 已压天数 | `floor((now - shipped_at) / 1 天)`，最少 0 | 只用于展示与文案，不参与判定 |

阈值读不到时（参数被删除 / 停用 / 非数字 / ≤0）**回退 7 天**，不是「不告警」：
读不到就静默失效等于功能整体消失；回退默认值最多是「按默认口径告警」。

---

## 3. 配置项（`sys_config`，V0171 种子）

| 参数键 | 类型 | 缺省 | 说明 |
|---|---|---|---|
| `erp.transfer.in_transit_overdue_days` | NUMBER | `7` | 超期阈值（天），按 `shipped_at` 计算 |
| `erp.transfer.in_transit_realert_hours` | NUMBER | `24` | 同一张单的重提醒间隔（小时） |

- 改完立即生效（参数快照 60s TTL，管理端写入会立即广播；扫描任务每轮重新读）。
- 两者都必须 > 0，否则按缺省值处理（`0` 天会让「刚发货就算超期」，是灾难性误配）。

---

## 4. 数据模型变更（迁移 `V0171__transfer_in_transit_overdue`）

| 变更 | 内容 | 为什么 |
|---|---|---|
| 列 | `biz_stock_transfer.overdue_alerted_at timestamptz` | 最近一次告警时间（NULL = 从未提醒过）；重提醒间隔靠它判 |
| 列 | `biz_stock_transfer.overdue_alert_count smallint NOT NULL DEFAULT 0` | 累计告警次数：CAS 的期望值 + 通知幂等键的序号段 |
| 索引 | `idx_biz_transfer_in_transit_shipped (shipped_at) WHERE status='IN_TRANSIT' AND deleted_at IS NULL` | 每小时扫一次，只需要在途行；部分索引比全表索引小两个数量级 |
| 参数 | 上面两条 `sys_config` | 阈值可配 |

- `up` / `down` 都幂等（`ADD COLUMN IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS` /
  `INSERT … ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING`）。
- `down` 里有**越界检测**：先断言 `status` / `shipped_at` / `received_at` /
  `transit_warehouse_id` 四列与 `ck_biz_transfer_state` 都还在，再删本迁移的两列 ——
  防止有人把 down 写宽了把状态机打回 V0140 之前。
- 回滚会丢弃告警台账（`overdue_alert_count > 0` 的行数会在 `down` 里打 NOTICE），
  但**不影响单据状态、库存与已发出的通知**。

---

## 5. 查询接口

### 5.1 列表筛选（`GET /business/erp/transfer/page`）

新增查询参数：

| 参数 | 取值 | 说明 |
|---|---|---|
| `in_transit_overdue` | `true` / `false` / 缺省 | `true` = 只看超期在途；`false` = 排除超期；缺省 = 不筛 |

矛盾的组合直接 `400`（**不静默返回空列表**，否则会被当成「没有超期单」）：

- `in_transit_overdue=true` + `status=!=IN_TRANSIT` → 400；
- `in_transit_overdue=false` + `status=IN_TRANSIT` → 400。

响应每行新增 4 个字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `in_transit_overdue` | bool | 是否超期在途（判定口径见 §2） |
| `overdue_days` | int \| null | 已压天数（仅超期为 true 时有值） |
| `overdue_alerted_at` | datetime \| null | 最近一次告警时间 |
| `overdue_alert_count` | int | 累计告警次数（> 0 说明扫描任务真的提醒过） |

### 5.2 超期汇总（`GET /business/erp/transfer/overdue/summary`）

权限码复用 `business:erp:transfer:list`（不新增权限码 / 菜单迁移）。

```json
{
  "code": 0,
  "data": {
    "threshold_days": 7,
    "total": 3,
    "oldest_shipped_at": "2026-09-28T02:14:00+00:00"
  }
}
```

分页接口只回当前页，算不出「一共几张、最老那张压了多久」；这个接口是**全库计数**，
给首页角标 / 告警中心用。

---

## 6. 告警怎么推

| 项 | 取值 | 说明 |
|---|---|---|
| 任务名 | `transfer_overdue_scan` | `src/business/erp/transfer_overdue.rs` |
| 频率 | `0 5 * * * *`（每小时第 5 分钟） | 压货按天计，每分钟扫没意义；单轮 `LIMIT 500` |
| 注册点 | `src/task/mod.rs::Scheduler::register_builtin` | 必须在这里（`bootstrap.rs` 会被随后的 `reload()` 清空） |
| 通道 | `business::notice::dispatch`（站内信 + 既有扇出 / 偏好 / 投递记录） | **不另造一套通知** |
| 类型 / 级别 | `ALERT` / `WARN` | 通知中心按 `ALERT` 分类 |
| 送达强度 | `force = true`（跳过 `notify.inbox` 偏好） | 漏一条就是账实差异，与 `FLOW_URGE` 同分级 |
| 收件人 | 申请人（发货方）+ 审批人（去重） | 两边都没有 → 跳过并打 WARN（不往全员推，压货告警推给全员等于没人负责） |
| 跳转 | `/business/erp/transfer` | 前端暂无调拨详情页路由，先跳列表（可按单号筛） |

### 6.1 不重复轰炸（三层）

1. **重提醒间隔**：`overdue_alerted_at` 在 `realert_hours`（默认 24 小时）内不再捞出来；
2. **台账 CAS**：`UPDATE … WHERE overdue_alert_count = <读到的旧值>`，多实例并发只有一个能写；
3. **通知幂等键**：`TRANSFER:IN_TRANSIT_OVERDUE:<调拨单 id>:<第几次>`，命中
   `uk_sys_notice_idem` 的第二次 dispatch 直接返回 `duplicated`。

顺序是**先发通知、再记台账**：反过来的话通知发送失败会永久吞掉这张单的告警
（台账已 +1，下一轮不再捞到）。代价是「极小概率重发一条同内容的通知」，由幂等键兜住。

### 6.2 告警文案

标题：`在途调拨超期 30 天未收货：TK-20260930-000123`（重提醒追加「（第 2 次提醒）」）

正文：

```text
调拨单 TK-20260930-000123 于 2026-09-30 09:12 发货，已压 30 天仍未收货（阈值 7 天）。
调出仓：北京中心仓
调入仓：上海分仓
这段时间库存既不在调出仓也不在调入仓（结算时才双写），请尽快让收货方点「收货」，或撤回 / 取消这张单。
```

---

## 7. 文件清单

| 文件 | 变更 |
|---|---|
| `backend/migrations/business/V0171__transfer_in_transit_overdue.up.sql` / `.down.sql` | 新增：两列 + 部分索引 + 两条参数 |
| `backend/src/business/erp/transfer_model.rs` | 新增：阈值读取 / `is_in_transit_overdue` / `cutoff_of` / `overdue_elapsed_days` / `TransferQuery.in_transit_overdue` / `scan_overdue_candidates` / `mark_overdue_alerted` / `overdue_summary` |
| `backend/src/business/erp/transfer_dto.rs` | 新增：`TransferFilter.in_transit_overdue` + 矛盾组合校验、`TransferRes` 4 个超期字段、`TransferOverdueSummaryRes` |
| `backend/src/business/erp/transfer_handler.rs` | 新增：列表筛选接线 + `GET /business/erp/transfer/overdue/summary` |
| `backend/src/business/erp/transfer_overdue.rs` | 新增：扫描任务 / 告警草稿 / 幂等键 / 注册 + 6 条纯逻辑单测 |
| `backend/src/business/erp/mod.rs`、`backend/src/task/mod.rs` | 模块声明 + 任务注册（各 1–2 行） |
| `backend/tests/linkage/transfer_chain.rs` | F12 从 `#[ignore]` 转正并**重写断言**（原断言是裸 SQL 计数，没有实现也是绿的） |
| `scripts/verify/f12_transfer_overdue.sh` | 新增：接口级 e2e（20 项断言，自带夹具与清理） |
| `docs/在途调拨超期告警-LNK-F12.md` | 本文档 |

---

## 8. 验收实测

> 全部在本机（172.18.10.99）实跑，隔离树 `/root/f12-wt`（`origin/main` 之后的一个提交 e6d6ae9 + 本卡改动，
> `CARGO_TARGET_DIR=/root/f12-target`），库 `cloudpivot_test` 与它的克隆 `cloudpivot_f12iso`。

### 8.1 用例（卡面验收标准）

```text
$ LINKAGE_DB_URL='postgres://…@127.0.0.1:5432/cloudpivot_test' LINKAGE_REQUIRE_DB=1 \
  cargo test --test linkage
…
test linkage::transfer_chain::f12_in_transit_stuck_shipment_should_alert ... ok
…
test result: ok. 61 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 284.65s
```

- `61 passed` = 原 60 条 + **F12 转正**；`1 ignored` = 只剩 LNK-P08（等 `t_a34c68b6`）。
- 跑完残留核查：`transfer|0` / `notice|0`（本用例造的告警通知在用例末尾删净）。

单跑这一条（改造前是「绿得毫无意义」，见 §5.2 的报告）：

```text
$ cargo test --test linkage f12_in_transit -- --nocapture
running 1 test
test linkage::transfer_chain::f12_in_transit_stuck_shipment_should_alert ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 61 filtered out; finished in 7.09s
```

### 8.2 变异验证（证明断言不是恒真）

把 `transfer_model::scan_overdue_candidates` 临时短路成 `Ok(Vec::new())`
（= 「根本没有扫描任务」的原始缺陷态），同一用例立刻红：

```text
thread '…f12_in_transit_stuck_shipment_should_alert' panicked at tests/linkage/transfer_chain.rs:878:
F12：扫描必须至少告警一张单（transfer_overdue_scan: scanned=0 alerted=0 skipped=0 failed=0）
test result: FAILED. 0 passed; 1 failed; …
```

还原后复绿（证据里已确认临时短路代码未残留：`grep -rn MUTATION backend/src backend/tests` 零命中）。

### 8.3 单测 / 静态检查

```text
$ cargo test --lib -j 1
test result: ok. 1449 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test --lib -j 1 overdue          # 本卡新增的 9 条
business::erp::transfer_model::tests::overdue_boundary_is_strict_and_status_bound ... ok
business::erp::transfer_model::tests::overdue_elapsed_days_floor_and_clamp ... ok
business::erp::transfer_model::tests::query_defaults_keep_overdue_threshold ... ok
business::erp::transfer_overdue::tests::{idempotency_key_changes_with_seq, realert_is_marked_as_second_round,
    notice_draft_carries_facts, recipients_dedup_and_drop_empty, report_summary_shape, task_metadata_is_stable} ... ok
test result: ok. 10 passed（含 1 条既有用例）; …

$ cargo fmt -- --check                 # 0 diff
$ cargo clippy --all-targets -- -D warnings -A clippy::manual_range_contains
    Finished `dev` profile …           # 0 error（EXIT=0）
```

- `-A clippy::manual_range_contains` 是**仓内既有债**：`impexp/definition.rs`、`task/handler.rs`、
  `task/template.rs` 三处 `a < x || x > b` 写法，与本卡无关（上一张卡的报告里已登记）。
- 共享工作区 `/opt/cloudpivot` 里同时有别的卡的在途改动（`payable_payment.rs` / `state_machine.rs` /
  未跟踪的 `tests/testd.rs`），那里的 `--all-targets` clippy 会被它们打红；本卡的 clippy 结论取自上面的隔离树。

### 8.4 迁移（V0171）往返 —— 在 `cloudpivot_test` 的**非空克隆库** `cloudpivot_f12iso` 上跑

```text
=== ① 现状（克隆库） ===
overdue_alert_count|smallint
overdue_alerted_at|timestamp with time zone
idx_biz_transfer_in_transit_shipped
erp.transfer.in_transit_overdue_days=7
erp.transfer.in_transit_realert_hours=24

=== ② down ===
DELETE 2
NOTICE:  V0171 down 完成：告警台账 / 索引 / 参数 均无残留

=== ③ 残留核查 ===
cols=0   idx=0   cfg=0

=== ④ 状态机既有列/约束仍在 ===
cols_kept=4            （status / shipped_at / received_at / transit_warehouse_id）
ck_kept=1              （ck_biz_transfer_state）

=== ⑤ up（重放） ===
INSERT 0 2
NOTICE:  V0171 完成：告警台账 2 列 + 部分索引 1 个 + 系统参数 2 条

=== ⑥ down 幂等（第二遍）===   NOTICE: …均无残留
=== ⑦ up（回到交付状态）===    NOTICE: …完成
```

**真实引擎已接受本迁移**（`cloudpivot_test` 上被某次实例启动自动应用）：

```text
$ psql -c "select kind, name, version, applied_via, success from schema_migrations where version='171'"
versioned|transfer_in_transit_overdue|171|executed|t
$ sha256sum V0171__transfer_in_transit_overdue.up.sql
834003584aa5c4f9883c052d6e2b3e8cef8067ab47feea192f06a9b03c83e7f4     ← 与库内 checksum 逐字相同
```

### 8.5 定时任务真的在跑（临时把 cron 调成 `*/10` 起隔离实例验证，**已还原**）

```text
INFO cloudpivot::business::erp::transfer_overdue: 在途调拨超期扫描任务已注册 invoke_target="transfer_overdue_scan" cron="0 5 * * * *"
INFO nik_plugin::scheduler: 定时任务 transfer_overdue_scan [0 5 * * * *] 已排程
…
INFO cloudpivot::business::erp::transfer_overdue: 在途调拨超期扫描完成 summary=transfer_overdue_scan: scanned=1 alerted=1 skipped=0 failed=0
INFO cloudpivot::business::erp::transfer_overdue: 在途调拨超期扫描完成 summary=transfer_overdue_scan: scanned=0 alerted=0 skipped=0 failed=0   ← 下一轮：间隔内不重复
```

落库结果（探针单 `F12-CRON-PROBE`，压 30 天）：

```text
biz_stock_transfer: overdue_alert_count=1, overdue_alerted_at 非空 → true
sys_notice:  ALERT/WARN | 在途调拨超期 30 天未收货：F12-CRON-PROBE
             idem=TRANSFER:IN_TRANSIT_OVERDUE:<uuid>:1 | receiver_count=1
sys_notice_receiver: 1 行（收件人 = 申请人）
```

实例日志里那条 `INSERT` 的参数可逐字看到告警正文（含单号 / 发货时间 / 已压天数 / 两个仓名 / 阈值）。
探针与通知在验证后已删净（`leftover=0`）。

### 8.6 接口级 e2e（`scripts/verify/f12_transfer_overdue.sh`，20 项断言全过）

```text
$ BASE=http://127.0.0.1:18099 … bash scripts/verify/f12_transfer_overdue.sh
[PASS] 登录成功（admin）
[PASS] summary 接口可达（200）（200）
[PASS] 阈值来自 sys_config（默认 7 天）（7）
[PASS] 库里的阈值参数与接口一致（7）
[PASS] 夹具就位（超期 F12-STUCK-… / 对照 F12-FRESH-…）
[PASS] 列表 in_transit_overdue=true（200）（200）
[PASS] 超期单出现在「超期在途」筛选结果里
[PASS] 刚发货的对照单不在超期筛选里
[PASS] 行内 in_transit_overdue=true
[PASS] 行内带已压天数 overdue_days
[PASS] 列表 in_transit_overdue=false（200）（200）
[PASS] 对照单出现在「排除超期」结果里
[PASS] 超期单不在「排除超期」结果里
[PASS] 矛盾组合 in_transit_overdue=true + status=DONE → 400（400）
[PASS] 400 文案点明矛盾
[PASS] 详情接口（200）（200）
[PASS] 详情回 in_transit_overdue / overdue_days（True 30）
[PASS] 汇总 total ≥ 1（1）
[PASS] 汇总带最早发货时间（2026-10-01T06:10:34Z）
[PASS] 夹具清理（残留 0）（0）
==> 汇总：PASS=20 FAIL=0
```

无令牌打新路由是 **401（不是 404）**，证明路由已注册：

```text
$ curl -o /dev/null -w '%{http_code}' http://127.0.0.1:18099/api/business/erp/transfer/overdue/summary
401
```

---

## 9. 前端待办

卡面第 4 条要求「前端给一个『超期在途』筛选项」——本卡（后端）只交付接口与口径，
前端改动已另建卡 **`t_655b6d82`**（assignee `frontend-developer`，父卡 = 本卡）交给前端：
`transfer-filter` 加一个「超期在途」开关 + 列表行「超期 N 天」标记 + 页头角标。

前端接线要点：

- 筛选项：`GET /business/erp/transfer/page?in_transit_overdue=true`（三态：不过滤 / 只看超期 / 排除超期，最省事是只做「只看超期」一个开关）；
- 行标记：`res.in_transit_overdue` + `res.overdue_days`；
- 角标：`GET /business/erp/transfer/overdue/summary` 的 `total`（阈值文案用 `threshold_days`）。

---

## 10. 已知边界（本卡不做）

| 不做的事 | 原因 |
|---|---|
| 超期自动收货 / 自动取消 | 「替业务做决定」有法律与账务后果，与 `flow::timeout` 的 `AUTO_APPROVE` 同一取舍：默认只提醒，自动处置要产品拍板 |
| 在途仓真实持仓（发货即扣减调出仓） | 那是结算时点前移的另一套记账模型（W3c 已定「结算才双写」），属新需求 |
| 调拨详情页路由 | 前端目前只有列表页；告警跳列表 |
| 短信 / 邮件 / Webhook 专用通道 | 站内信已覆盖；`notice::dispatch` 支持多通道，需要时给 `NoticeDraft.channel_mask` 加位即可 |
