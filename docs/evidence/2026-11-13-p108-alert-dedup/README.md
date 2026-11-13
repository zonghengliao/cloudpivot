# P1-08 证据：站内告警分级 + 10 分钟去重

> 卡面验收：① 连续发 5 条同 `(source,title)` → 只落 1 条，其余计入 count；
> ② 等 11 分钟（mock 时间）→ 再发 1 条，内容含「已重复 4 次」；
> ③ 站内消息带 `level` 字段，前端消息中心按级别着色。**三条全部达成**，实测见本目录 00~06。

## 实测环境

| 项 | 值 |
|----|----|
| 主机 | 172.18.10.99（开发机） |
| 后端 | `./target/debug/cloudpivot`，监听 `127.0.0.1:18099`，profile `dev`（`config-dev.toml` 的 `[middleware.auth] enabled = true`，与生产一致） |
| 库 | 新建空库 `cloudpivot_p108_e2e`（`create database … owner alun`），首启自动跑完 **336 条迁移**（含本卡新增 `V0404__notice_alert_dedup`） |
| 关键 env | `NIK_ADMIN_PASSWORD=Admin@123456`（消息中心要超管登录）、`NIK_ALERT_DEDUP_WINDOW_SECONDS=2`（把 10 分钟窗口压到 2 秒，否则「窗口过期」这段要真等 11 分钟） |
| 触发 | `POST /internal/alert`（回环，免认证）+ 超管登录后 `GET /api/business/notice/mine` |

> 为什么窗口可以压到 2 秒：判据是纯函数（`alert_dedup::evaluate`），时刻是**入参**；
> 线上路径 `deliver_internal_alert` 只是「`Utc::now()` + 环境变量窗口」的薄封装，
> 与验收脚本打的是同一段代码。压窗口只是让真库端到端能在 3 秒内跑完，不改判据。

## 改了什么、为什么

| 文件 | 改动 | 原因 |
|------|------|------|
| `backend/migrations/business/V0404__notice_alert_dedup.{up,down}.sql`（新增） | 新表 `sys_notice_alert_dedup`：`PRIMARY KEY (source, title)` + `window_start` / `repeat_count` / `notice_id` / `last_seen_at` | 去重状态必须**跨重启、跨调用方**成立 ⇒ 落库；主键就是业务键，不用哈希（无碰撞、`psql` 里看得懂）。不放进 `sys_notice.idempotency_key`：那是**永久**唯一，会把"恢复后再犯"永久吞掉 |
| `backend/src/business/notice/alert_dedup.rs`（新增，~300 行 + 8 条单测） | 纯函数判据 `evaluate(now, prev, window) -> DedupVerdict{action, next}`、窗口环境变量 `window()/window_of()`、文案 `repeat_note()/with_repeat_note()` | 「发还是不发」是整张卡的核心判据，必须能脱库、脱时钟单测（卡面要求 mock 时间）；线上与单测跑同一段代码 |
| `backend/src/business/notice/model.rs` | `claim_alert_dedup`（一个事务里 `SELECT … FOR UPDATE` + 判据 + `INSERT … ON CONFLICT DO UPDATE`）、`bind_alert_dedup_notice`、`find_alert_dedup`、`find_notice_by_idem_summary`；`page_my_notices` 新增 `pin_critical` 形参 + `my_notice_order_by()` | 并发下不能丢计数（行锁）；发布后回填 `notice_id`；置顶排序只改 `ORDER BY` 不动筛选与总数 |
| `backend/src/business/notice/dto.rs` | 新响应 `InternalAlertRes`（`published`/`deduped`/`merged_count`/`level`/`window_seconds`…）；`MyNoticeQuery.pin_critical`；`MyNoticeRes.pinned` | `DispatchResult` 表达不了第三种结局（"没发但也没错"）；三个结局必须有各自可判字段 |
| `backend/src/business/notice/handler.rs` | `deliver_internal_alert_at(req, now, window)`（可注入时刻/窗口）+ `deliver_internal_alert` 薄封装；顺序 = ①幂等键预查 ②窗口认领 ③发布+回填；`mine` 传 `pin_critical`（缺省 true） | 幂等重放必须**先于**窗口判定（否则重放拿不到首次 `notice_id`）；发布失败不回填 ⇒ 下轮立刻重发，不留"空窗口" |
| `frontend/src/api/business/notice/index.ts` | `MyNoticeRes.pinned`、`MyNoticeQuery.pin_critical` | 置顶由后端给标记，前端不重复实现"什么算严重" |
| `frontend/src/views/business/notice/message/index.vue` | 置顶「置顶」红标签 + 红色左竖条 + 详情页标签；`pin_critical: true` | 卡面「红色标签 + 消息中心置顶」 |
| `frontend/src/layouts/components/NoticeBell.vue`、`src/stores/notice.ts` | 显式 `pin_critical: false` | 铃铛/浮层要「最新 5 条」而不是「最严重的 5 条」，否则一条陈年 ERROR 会长期占第一格 |
| `scripts/ops/alert_check.sh` | `sink_notice` 成功分支识别 `deduped=true`（日志写"已合并（本窗口第 N 次）"），发布分支带上「上一窗口合并 N 次」；头部「去重」章节重写为两层口径 | 合并是**正常结局**，必须与"调用失败"在日志里区分，否则运维以为通道挂了 |
| `scripts/verify/p108_alert_dedup_acceptance.sh`（新增） | 30 条断言 + 证据落盘（含超管登录、首登改密、消息中心两种排序） | 验收脚本化，换机器可复跑 |
| `docs/运维规程-指标端点与抓取.md` | §8.1 响应体三结局表、§8.4 分级与去重、§8.5 验收 | 契约/排查 SQL 必须留档，不能只活在代码注释里 |

**没有**改框架 `vendor/nik-*`，也没新增依赖。`InternalAlertReq` 的字段没动
（`level`/`title`/`content`/`source`/`idempotency_key`），脚本不用改请求体。

### 五个刻意的决定

| 决定 | 为什么不选另一种 |
|------|------------------|
| **滑动窗口**（从首次发布起算 10 分钟），不是固定时间桶 | 固定桶（如 `floor(now/10min)`）会让 `09:59` 与 `10:01` 相差 2 分钟却落两条，违反卡面「10 分钟内只发一次」 |
| **抑制的前提是「本窗口真的发出去了」**（`notice_id IS NOT NULL`） | 若把"开窗口"和"发出去"当一件事，dispatch 失败就留下空窗口 → 之后 10 分钟同类告警全被吞、消息中心一条都没有，无待办无补偿（AGENTS.md 约定 11 的反例形态）。现在失败即下轮重发：**宁可多响，不可少响** |
| **`idempotency_key` 预查先于窗口判定** | 反过来的话，脚本带键重放会被判成"窗口内重复"而拿不到首次 `notice_id`，脚本会把重放当失败 |
| **置顶做在 SQL 的 `ORDER BY` 上**，且 `pinned` 由后端给 | 两次查询再拼会破坏分页（总数/页码对不上）；让前端自己判断"什么算严重"会两处口径漂移 |
| **窗口可由 env 覆盖（1~86400，非法值回落 600）** | 验收要端到端验"窗口过期补发"，真等 11 分钟不现实；同时禁止 0（去重不能悄悄关掉） |

## 验证结果

| 证据 | 命令 | 结论 |
|------|------|------|
| `00_health.json` | `curl :18099/health` | `200`（空库首启 336 迁移） |
| `01_first_publish.json` | 首发 `critical` | `published=true` / `deduped=false` / `merged_count=0` / `level=ERROR` / `notice_id` 有值 / `window_seconds=2` |
| `02_suppress_1~4.json` | 窗口内再发 4 条同 `(source,title)` | 全部 `published=false` / `deduped=true` / `merged_count` 1→2→3→4 / `notice_id=null` |
| `03_dedup_row.txt` | `psql` 直查窗口行 | `p108acc\|4\|true\|【验收】…磁盘使用率 92%`；`sys_notice` 恰好 **1** 行；窗口行 `notice_id` = 首发返回的那个 |
| `04_republish.json` / `04_second_content.txt` | 等 3s（窗口 2s + 1）再发 | `published=true` / `merged_count=4`；正文含 **「已重复 4 次」**；库里 **2** 行 |
| `05_other_title.json` | 换标题、同 source 发一条 `warning` | 立刻另落一条（键是二元组不是 source）；`level=WARN`，落库 `WARN` |
| `06_login.json` / `06_change_password.json` | 超管登录 + 首登改密 | `code=0`（`must_change_password` 会拦业务接口，脚本自己解锁） |
| `06_mine_pinned.json` | `GET /api/business/notice/mine?pin_critical=true` | 列表首条 `level=ERROR` + `pinned=true`；ERROR 全部排在 WARN 之前 |
| `06_mine_plain.json` | `?pin_critical=false` | 回到纯时间倒序（最新那条 WARN 排第一）；`pinned` 标记仍然正确（级别人的前端着色不受排序影响） |
| `07_acceptance_full.out` | `bash scripts/verify/p108_alert_dedup_acceptance.sh 18099 …` | **30 PASS / 0 FAIL，exit=0** |

### 单测

```
cargo test --lib business::notice::alert_dedup        → 8 passed / 0 failed（纯函数：5 条只落 1、11 分钟补发带 4 次、
                                                        窗口边界、时钟回拨、失败即重发、env 解析、文案单位）
cargo test --lib business::notice::handler::tests     → 8 passed / 0 failed（含 my_notice_order 的 SQL 文本断言）
ERP_DB_URL='postgres://alun:***@…/cloudpivot_p108_test' \
  cargo test --lib business::notice::handler::tests::alert_dedup -- --test-threads=1 --nocapture
                                                      → 1 passed（真库：5 条只落 1 条 + 窗口行 repeat_count=4 +
                                                        11 分钟后第 2 条正文含「近 10 分钟已重复 4 次」，
                                                        无 SKIP 行 ⇒ 确实连库跑过）
cargo test --lib                                      → 5599 passed / 0 failed
cargo fmt --check / cargo clippy --lib --tests -- -D warnings → exit 0（零警告）
cd frontend && pnpm typecheck                         → exit 0
bash scripts/ops/alert_check.sh --self-test           → 43/43（改脚本后仍全绿）
```

> 真库用例「不假绿」自查：`--nocapture` 下若没连库会打印
> `SKIP: 未设置 ERP_DB_URL…` / `SKIP: … 连接失败`；上面那次输出里没有任何 SKIP 行。

## 已知边界

- **144/145 现网需要下次 deploy 才有这次改动**（新表 + 新响应字段）。脚本侧已兼容两种响应：
  老实例没有 `deduped` 字段时走原来的「已送达」分支，不会误判。
- **脚本侧窗口（300s）与服务端窗口（600s）不相等是刻意的**：脚本侧管「别刷爆 ALERT.log / 看板」，
  服务端管「消息中心同类只有一条且知道重复几次」。因此线上同一告警在 10 分钟内通常只被合并 1 次
  （脚本每 5 分钟最多再发一次）；这是降噪口径，不是丢失计数。
- **置顶会长期占据列表头部**：`level = ERROR` 的未读旧消息会一直排在新消息之前（置顶的语义）。
  如果需要"最新优先"，调用方传 `pin_critical=false`（顶栏铃铛与消息浮层就是这么做的）。
- 并发同键（同一瞬间两路调用）最坏会多发一条（判定事务不跨 dispatch），**不会少发**；
  实际调用方是单写者 cron 脚本。跨副本一致性同理：窗口状态在库里，副本之间共享。
- 验收库 `cloudpivot_p108_e2e`（空库首启）与 `cloudpivot_p108_test`（真库单测）留在 99 供复跑。
