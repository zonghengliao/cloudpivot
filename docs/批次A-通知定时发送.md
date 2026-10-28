# 批次 A · 通知定时发送（`scheduled_at`）实现说明

> 代码：`backend/src/business/notice/{mod,handler,dto,model}.rs` + `scheduled.rs`（巡逻任务）
> 迁移：`V0051__notice_scheduled_dispatch`（新迁移，**未改 V0043**）
> 任务：`notice_scheduled_dispatch`，cron `*/30 * * * * *`（每 30 秒）

## 1. 语义

`POST /api/business/notice` 新增可选字段 `scheduled_at`（RFC3339 / UTC）：

| 取值 | 行为 |
|------|------|
| 缺省 / `null` | 立即发送（原链路不变） |
| `<= now()` | 按**立即发送**处理（容忍时钟偏差与「补发」语义，不报错） |
| `now() < t <= now()+366d` | **定时发送**：只建任务，不扇出、不投递、不推送 |
| `> now()+366d` | `400 + 10001`「定时发送时间最多提前 366 天」 |

定时发送的响应会明确标注：

```json
{"code":0,"data":{"notice_id":"…","scheduled":true,"scheduled_at":"2026-10-28T15:00:46Z","receiver_count":0,"channels":[]}}
```

- `receiver_count = 0`：到点扇出后才回填（管理列表可实时看到真实人数）；
- `channels = []`：到点才产生投递记录。

## 2. 为什么「当时不可见」是数据库级保证

`sys_notice_receiver` 是用户侧「我的通知 / 未读数」的唯一数据源。定时通知在到点前

- `sys_notice.dispatch_status = 'PENDING'`；
- **一行 `sys_notice_receiver` 都不写**。

因此用户侧列表 / 未读数天然查不到它 —— 不是靠前端过滤，也不是靠 `created_at` 判断。

## 3. 数据模型（V0051）

```sql
ALTER TABLE sys_notice ADD COLUMN scheduled_at TIMESTAMPTZ;                                  -- NULL = 立即
ALTER TABLE sys_notice ADD COLUMN dispatch_status VARCHAR(16) NOT NULL DEFAULT 'SENT';       -- PENDING/SENT/FAILED/CANCELED
ALTER TABLE sys_notice ADD COLUMN dispatch_attempts SMALLINT NOT NULL DEFAULT 0;             -- 到点后失败重试计数
CREATE INDEX idx_sys_notice_due ON sys_notice (scheduled_at)
    WHERE dispatch_status = 'PENDING' AND deleted_at IS NULL;                                -- 待发队列专用部分索引
```

状态机：

```
                 ┌───────────── 到点前被软删 ─────────────┐
PENDING ─────────┴──────────────────────────────────────▶ CANCELED
   │
   └── 巡逻任务（每 30s，SKIP LOCKED 抢占）──▶ SENT（扇出 + 计数 + 通道投递 + 实时推送）
                    │
                    └── 失败 → dispatch_attempts+1；达 3 次 → FAILED（不再重试）
```

## 4. 巡逻任务实现要点

1. **抢占**：单条通知一个事务，`SELECT … FOR UPDATE SKIP LOCKED`；多实例并发只有一个真正投递；
2. **失败隔离**：单条失败只回滚该条事务，不影响同批其它通知；
3. **计数器耐久**：失败次数在**失败事务之外**单独累加（回滚会把它一起丢掉，否则同一行会被每个周期重试到天荒地老）；
4. **到点前取消**：`deleted_at` 非空且仍是 `PENDING` 的行，由同一任务顺带置 `CANCELED`（幂等）；
5. **实时推送在事务提交后**：推送失败不影响已落库的投递结果（与立即发送同语义）；
6. **建任务时就做完校验**：模板渲染、目标合法性、接收人上限预演都在创建时执行，不会「到点了才发现模板缺变量」。

## 5. 实测（详见 `docs/批次A-遗留修复-验证证据.md` 项③）

| 检查 | 结果 |
|------|------|
| `scheduled_at = now+400d` | `400 + 10001 定时发送时间最多提前 366 天` |
| 建 `now+70s` 的通知 | `code=0, scheduled=true, scheduled_at=…, receiver_count=0` |
| 当时 DB | `dispatch_status=PENDING`、`receiver_count=0`、`sys_notice_receiver` 行数 **0** |
| 当时 `/notice/mine` | 不含该通知（条数 0） |
| 当时 `/notice/page`（管理侧） | 能看到，`dispatch_status=PENDING` + `scheduled_at` |
| 到点后（+85s） | `dispatch_status=SENT`、`receiver_count=1`、`sys_notice_receiver` 1 行 |
| 到点后 `/notice/mine` | 出现该通知（条数 1，含 `receiver_id`） |
| 巡逻日志 | `notice_scheduled_dispatch: scanned=1 dispatched=1 skipped=0 failed=0 canceled=0` + `定时通知已到点投递` |
| `scheduled_at = now-5m` | `scheduled=false, receiver_count=1`（按立即发送处理） |

## 6. 注册位置（易踩）

巡逻任务必须由 `backend/src/task/mod.rs::Scheduler::register_builtin` 注册。写在 `bootstrap.rs` 会被随后的
`reload()` 清空（A1 已踩过），这是框架调度器的「一次性快照」语义。
