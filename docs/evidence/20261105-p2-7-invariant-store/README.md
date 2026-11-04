# P2-7 账务不变量巡检：结果落库 + 每日自动跑 + 只读看板

> 卡片：`t_e0567319` · 评审结论「巡检结果不落库：无趋势、无看板」
> 证据时间：2026-11-05 02:19–02:23（北京时间）· 隔离实例 `127.0.0.1:18095` + 隔离库 `cloudpivot_p27_inv`

## 1. 表结构（`sys_invariant_result`，一次运行一行）

```sql
CREATE TABLE sys_invariant_result (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_at TIMESTAMPTZ NOT NULL,          -- 本次巡检开始时间（趋势按此列切北京时间自然日）
    trigger_type VARCHAR(16) NOT NULL,    -- SCHEDULED（内置 cron）/ MANUAL（看板或定时任务页）
    status VARCHAR(16) NOT NULL,          -- OK / FAIL / ERROR
    total INT NOT NULL DEFAULT 0,         -- 检查总数
    passed INT NOT NULL DEFAULT 0,        -- 通过数
    failed INT NOT NULL DEFAULT 0,        -- 失败数（ok=false 合计）
    blockers INT NOT NULL DEFAULT 0,      -- 失败项里 BLOCKER 级
    warns INT NOT NULL DEFAULT 0,         -- 失败项里 WARN 级
    duration_ms INT NOT NULL DEFAULT 0,   -- 本次耗时
    database_name VARCHAR(128) NOT NULL DEFAULT '', -- 目标库快照
    script_path TEXT NOT NULL DEFAULT '',           -- 实际执行的脚本快照
    detail JSONB,                          -- 全量明细 [{name,group,level,ok,detail}]
    failures JSONB,                        -- 失败项子集（看板直出）
    error TEXT,                            -- status=ERROR 时的原因
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_invariant_status  CHECK (status IN ('OK','FAIL','ERROR')),
    CONSTRAINT ck_invariant_trigger CHECK (trigger_type IN ('SCHEDULED','MANUAL'))
);
CREATE INDEX idx_invariant_run_at ON sys_invariant_result (run_at DESC);
CREATE INDEX idx_invariant_not_ok ON sys_invariant_result (run_at DESC) WHERE status <> 'OK';
```

迁移：`backend/migrations/system/V0235__create_sys_invariant_result.{up,down}.sql`
（同一迁移里幂等种入 `sys_job` 可见行 `invariant_audit`，`status=0` 只是「查看/手动执行」入口，
真正调度走编译期内置任务，避免双触发）。

隔离库实测（`psql` 对照，见 §4 探针 A 组）：

| 列数 | 索引 | CHECK | sys_job 行 |
|------|------|-------|-----------|
| 16 | `idx_invariant_run_at` + `idx_invariant_not_ok` + pkey | `ck_invariant_status` / `ck_invariant_trigger` | `invariant_audit / 0 20 2 * * * / invariant_audit / status=0` |

## 2. 每日自动跑（内置 cron）

`backend/src/task/mod.rs::register_builtin` 注册（与 03:00–03:50 日志清理、03:30 快照自检错峰）：

```rust
self.register_cron(
    runner::TASK_NAME,          // "invariant_audit"
    runner::CRON,               // "0 20 2 * * *"  → 每天 02:20
    "系统任务：账务不变量巡检（结果落 sys_invariant_result）",
    || tokio::spawn(async move {
        match runner::run_and_store(runner::Trigger::Scheduled).await { ... }
    }),
);
```

启动日志实证（`/tmp/p27-inv/backend.log`）：

```
INFO cloudpivot::task: 账务不变量巡检任务已注册（P2-7） invoke_target="invariant_audit" cron="0 20 2 * * *"
```

同一任务体也挂在 `task::run_handler("invariant_audit")` 上（`is_registered_target` 同步放行），
所以「定时任务」页对 `sys_job.invariant_audit` 点「执行一次」走的是同一条落库链路。

## 3. 看板（只读）

| 接口 | 权限码 | 说明 |
|------|--------|------|
| `GET /api/support/invariant/overview?days=30` | `support:invariant:list` | 最近一次结论 + 近 N 天趋势 + 当前失败项 + 连续非 OK 次数 |
| `GET /api/support/invariant/page` | `support:invariant:list` | 运行历史分页（`status?` 过滤） |
| `GET /api/support/invariant/{id}` | `support:invariant:list` | 单次运行全量明细 |
| `POST /api/support/invariant/run` | `support:invariant:run` | 立即跑一次并落库（同步，最长 300s） |

前端：`frontend/src/views/support/invariant/index.vue`（指标卡 + 30 天 EChart 趋势 + 当前失败项 +
运行历史抽屉），路由 `/support/invariant`，菜单种子 `migrations/seed/S__support_invariant_menu.sql`
（读权限授予所有未软删角色；「立即执行一次」只授 SUPER_ADMIN）。

截图：

- `board-fail.png` —— 注入一条违反 K10 的固资卡后的真实失败态：`有失败项 50/52`、
  `连续非 OK 1 次`、当前失败项两条（K1 WARN / K10 BLOCKER）、运行历史里同时有 `全绿` 与 `有失败项` 行。
- `board-ok.png` —— 删掉注入行再跑一次后的恢复态：`全绿 52/52`、`连续非 OK 0 次`、
  当前失败项为空、运行历史保留全部行（含一条 `未跑成`，见 §5）。

## 4. 探针（HTTP + DB 双侧对账，31/31）

`scripts/verify/p27_invariant_store_probe.py`（隔离实例：`scripts/verify/p27_instance.sh`）

```
A1..A4  表结构 / 索引 / CHECK / sys_job 行          OK
B1..B7  POST /run 两轮：200 + 结论 + 落库逐字段一致   OK
C1..C8  overview: latest / failures / trend / streak  OK
D1..D8  page / detail / 400 坏 id                    OK
结果：31/31 通过   FAIL=0
```

关键实测值（两轮）：
`status=OK total=52 passed=52 duration≈1530ms`，`count 0 -> 2`，
`db 52/52/0 == api 52/52/0`，趋势最后一点 `2026-11-05`（北京时间今天），`runs=2`。

原始输出：`probe-run1.txt`、`probe-run2.txt`。

## 5. 失败与「没跑成」两条路径也实测了（不是只测绿）

| 场景 | 做法（隔离库内） | 落库结果 |
|------|------------------|----------|
| 账真的错了 | 插一条 `status=DISPOSED` 但无 `DISPOSAL` 变动留痕的固资卡 | `status=FAIL total=52 passed=50 failed=2 blockers=1 warns=1`，`failures` = K1(WARN) + K10(BLOCKER) |
| 脚本没跑成 | 临时改名 `scripts/verify/erp_invariants.py` 再调 `/run` | `status=ERROR total=0 error="找不到巡检脚本 …（工作目录 /opt/cloudpivot/backend；可用环境变量 CLOUDPIVOT_INVARIANT_SCRIPT 指定绝对路径）"`；**没有写成 OK** |

两种注入都已清理：固资卡删除、脚本名恢复（`ls -l` 已核对 74482 字节原位）。

## 6. 口径与已知取舍

- **执行器直接跑既有脚本**（`erp_invariants.py --json`），不在 Rust 里重写检查：闸③ 判红用的是
  同一个脚本，重写会导致「看板说绿、闸③说红」。
- **趋势按北京时间切自然日，取当天最后一次运行**；没跑的天**不补零**（补零会画出「当天通过 0 条」
  这种假数据）。因此窗口内只有一天数据时，图上只有一个点。
- **`sys_invariant_result` 不做行级数据权限（SCOPE-EXEMPT）**：它是运维类全局事实表，无部门/归属人列，
  看板访问靠功能权限 `support:invariant:list` 控制。
- 单次巡检当前 52 条检查、耗时 ~1.5–1.9s；接口同步等待，上限 300s（超时落 ERROR 行）。
