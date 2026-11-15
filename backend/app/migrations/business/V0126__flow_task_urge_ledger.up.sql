-- 批次 D · D4c：审批待办的「催办台账」两列
--
-- 为什么要独立两列：卡面 D4c 要求「同一待办同一批次不重复提醒」，判定需要
-- 「上次催办时间 + 已催办次数」这一对数；只有次数无法判间隔（3 次会被 30 分钟内
-- 连扫打光），只有时间无法判上限。两者都落在 biz_flow_task 行上，扫描时
-- 一次读回，不再额外查通知表。
--
-- 与 V0122 的 remind_count / last_remind_at 的关系：那是 D2 落库的通用「提醒次数」，
-- 已被待办详情 DTO 读取展示。本迁移把历史值回填到新列，D4c 起由
-- business::flow::timeout 的**同一条 UPDATE** 双写两列 —— 单一 CAS 源（urge_count），
-- 展示列（remind_count）跟着同一语句走，不会出现「两套计数各说各话」。

ALTER TABLE biz_flow_task
    ADD COLUMN IF NOT EXISTS urge_count smallint NOT NULL DEFAULT 0;

ALTER TABLE biz_flow_task
    ADD COLUMN IF NOT EXISTS last_urge_at timestamptz;

-- 历史回填（幂等：只在未催办过的行上跑）
UPDATE biz_flow_task
   SET urge_count   = COALESCE(remind_count, 0),
       last_urge_at = last_remind_at
 WHERE urge_count = 0
   AND (COALESCE(remind_count, 0) > 0 OR last_remind_at IS NOT NULL);

-- 催办扫描要按 (status, last_urge_at) 过滤「10 分钟内不再催」，给部分索引兜住
CREATE INDEX IF NOT EXISTS idx_biz_flow_task_urge
    ON biz_flow_task (last_urge_at)
 WHERE status = 'PENDING';

-- 自检：两列必须存在、urge_count 必须 NOT NULL DEFAULT 0
DO $$
DECLARE
    missing int;
    bad     int;
BEGIN
    SELECT count(*) INTO missing
      FROM information_schema.columns
     WHERE table_name = 'biz_flow_task'
       AND column_name IN ('urge_count', 'last_urge_at');
    IF missing <> 2 THEN
        RAISE EXCEPTION 'V0126 自检失败：biz_flow_task 缺催办列（找到 % / 2）', missing;
    END IF;

    SELECT count(*) INTO bad
      FROM biz_flow_task
     WHERE urge_count IS NULL;
    IF bad > 0 THEN
        RAISE EXCEPTION 'V0126 自检失败：urge_count 有 % 行为 NULL', bad;
    END IF;
END $$;
