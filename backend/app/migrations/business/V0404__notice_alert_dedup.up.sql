-- ============================================================================
-- V0404__notice_alert_dedup（P1-08 · 站内告警分级 + 10 分钟去重 · t_03571844）
--
-- 【为什么要有这张表】
--   P1-07 把「巡检告警 → 站内消息」打通了（`alert_check.sh` → `POST /internal/alert`
--   → `sys_notice`），但**每次调用都新落一条**：同一台机器的「磁盘 92%」在 cron
--   每轮都发一条，消息中心被同类告警刷屏，超管要滑两屏才看见第二条真正的异常
--   ⇒ 告警通道把信噪比做反了。
--   本表是「去重窗口」的**唯一权威状态**：同一 `(source, title)` 在窗口内只允许
--   落一条 `sys_notice`，窗口内的重复只累加计数；窗口过后再发一条时，把上一窗口
--   的合并次数写进正文（「近 10 分钟已重复 4 次」）。
--
-- 【为什么不是复用 sys_notice.idempotency_key】
--   `uk_sys_notice_idem` 是**永久**唯一：同一条告警"恢复后再犯"会被这个键永久吞掉
--   （P1-07 的决策记录里已经写明，并且明确把去重交给本卡）。本卡要的是**滑动时间窗**
--   —— 窗口过后必须能再发，所以状态必须单独一张表，不能塞进 `sys_notice` 的约束。
--
-- 【为什么不放进程内存 / 不放脚本状态目录】
--   · 进程内存：后端重启（deploy / OOM）窗口归零 ⇒ 重启即刷屏；多副本时各算各的；
--   · 脚本状态目录：只有 `alert_check.sh` 一条调用方受益，任何新的告警来源
--     （业务侧 `POST /internal/alert`、其它巡检）都得自己再实现一遍。
--   放库上：重启后窗口继续生效，所有调用方共用同一份状态。
--
-- 【列语义】
--   window_start  当前窗口起点（**本窗口发布那条通知的时刻**；窗口内抑制时不更新）
--   repeat_count  本窗口内被**合并掉**的次数（不含首次发布的那条）
--   notice_id     本窗口里实际落下的那条 `sys_notice`；**NULL = 本窗口还没发成功**
--                 （发布失败后必须能立刻重试，见 `alert_dedup::evaluate` 的判据）
--   last_seen_at  最近一次收到该告警的时刻（排障用：区分「还在犯」与「已自愈」）
--
-- 【没有 FK 指向 sys_notice】
--   告警通知会被清理任务（`notice/cleanup.rs`）硬删；加了 FK 就得级联删窗口行，
--   窗口状态会跟着丢。这里是**弱引用**：`notice_id` 只作排障线索，不承担完整性。
--
-- 【主键就是业务键】
--   `PRIMARY KEY (source, title)` —— 去重身份就是这两列（P1-07 的接口契约里
--   `source` 缺省 `alert_check`、`title` ≤ 200 字符），不需要再算哈希：
--   没有哈希就没有碰撞，`psql` 里也直接看得懂。btree 索引项最大 ≈305 字符，
--   远小于索引行上限（2704 字节）。
--
-- 【号段】写入本文件时 `backend/migrations/*/` 最大号 = V0403，本文件顺延 V0404。
--
-- 【回滚】
--   `V0404__notice_alert_dedup.down.sql` 直接 DROP 本表。回滚只丢"窗口状态"，
--   已经落下的 `sys_notice` 一条不动（去重是**降噪**，不是数据一致性约束）。
-- ============================================================================

-- ---------- 去重窗口状态（每 (source,title) 一行） ----------
CREATE TABLE sys_notice_alert_dedup (
    source       VARCHAR(50) NOT NULL,
    title        VARCHAR(255) NOT NULL,
    window_start TIMESTAMPTZ NOT NULL,
    repeat_count INTEGER NOT NULL DEFAULT 0,
    notice_id    UUID,
    last_seen_at TIMESTAMPTZ NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT pk_sys_notice_alert_dedup PRIMARY KEY (source, title),
    -- 计数只会加不会减；负数说明逻辑写反了（窗口重置忘了归零），直接拒绝
    CONSTRAINT ck_sys_notice_alert_dedup_repeat CHECK (repeat_count >= 0)
);

COMMENT ON TABLE sys_notice_alert_dedup IS
    '告警去重窗口：同一 (source,title) 在窗口内只落一条站内消息（P1-08）';
COMMENT ON COLUMN sys_notice_alert_dedup.source IS
    '告警来源（与 sys_notice.sender_name 同口径，如 alert_check / 业务模块名）';
COMMENT ON COLUMN sys_notice_alert_dedup.title IS '告警标题（去重键的另一半，窗口内外都按原样匹配）';
COMMENT ON COLUMN sys_notice_alert_dedup.window_start IS
    '当前窗口起点 = 本窗口发布那条通知的时刻；窗口内抑制时保持不变';
COMMENT ON COLUMN sys_notice_alert_dedup.repeat_count IS
    '本窗口内被合并掉的次数（不含首次发布的那条）；窗口重置时归零';
COMMENT ON COLUMN sys_notice_alert_dedup.notice_id IS
    '本窗口实际落下的通知主键；NULL = 本窗口尚未成功发布（下一次调用会立刻重发）';
COMMENT ON COLUMN sys_notice_alert_dedup.last_seen_at IS
    '最近一次收到该告警的时刻（排障用：还在犯 / 已自愈）';

-- 排障与将来的清理任务（按"多久没再犯"回收行）都按这一列扫
CREATE INDEX idx_sys_notice_alert_dedup_last_seen
    ON sys_notice_alert_dedup (last_seen_at);
