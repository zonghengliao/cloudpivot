-- ============================================================
-- 21_run_once.sql —— 一轮巡检：S1 落库 / S2 收敛 / S3 告警
--
-- 调用方式（run_started 必须是数据库时钟，且在扫描之前取）：
--   psql -v run_started="$(psql -Atc 'select now()')" -f 21_run_once.sql
-- 代码实现：把 :'run_started' 换成绑定参数；三条语句放在同一个事务里。
-- ============================================================
BEGIN;

-- ---------- S1 落库（幂等）----------
-- 要点：
--   · detected_at 只在插入时写 → 首次发现时间不随巡检刷新；
--   · last_seen_at 每次命中刷新 → 收敛判据；
--   · ON CONFLICT 命中「同 issue_code + 同主体 + 同字段 + 未解决」，只更新观测值，不改 first_detected_at；
--   · 已 RESOLVED 的历史行不参与冲突（部分唯一索引带 resolved_at IS NULL），
--     所以「修复后再次发生」会新开一条 —— 这正是审计要的两次独立事件。
INSERT INTO biz_snapshot_drift
       (issue_code, entity_type, entity_id, entity_no, field_name,
        snapshot_value, truth_value, truth_source, drift_kind, severity, affected_count,
        detected_at, last_seen_at, detected_by)
SELECT issue_code, entity_type, entity_id, entity_no, field_name,
       snapshot_value, truth_value, truth_source, drift_kind, severity, affected_count,
       :'run_started'::timestamptz, :'run_started'::timestamptz, 'JOB'
  FROM di.v_drift_now
ON CONFLICT (issue_code, entity_type, entity_id, field_name) WHERE resolved_at IS NULL
DO UPDATE SET last_seen_at   = EXCLUDED.last_seen_at,
              snapshot_value = EXCLUDED.snapshot_value,
              truth_value    = EXCLUDED.truth_value,
              truth_source   = EXCLUDED.truth_source,
              severity       = EXCLUDED.severity,
              drift_kind     = EXCLUDED.drift_kind,
              affected_count = EXCLUDED.affected_count;

-- ---------- S2 收敛：本轮没再检出 ⇒ 自动 RESOLVED ----------
-- 三条纪律：
--   ① 只收敛「本轮确实跑过」的巡检项（SKIPPED 的项一行都不许动，否则 schema 未就绪会把历史真缺陷清空）；
--   ② 只收敛 last_seen_at < run_started 的行（本轮命中过的行时间戳一定 ≥ run_started）；
--   ③ SNAPSHOT 类收敛写 ACCEPTED（合法业务变更本身不是缺陷），DEFECT 类写 REPAIRED。
UPDATE biz_snapshot_drift d
   SET resolution  = CASE WHEN d.drift_kind = 'SNAPSHOT' THEN 'ACCEPTED' ELSE 'REPAIRED' END,
       resolved_at = now(),
       remark      = coalesce(d.remark || ' | ', '') || 'auto-closed: 本轮未再检出'
 WHERE d.resolved_at IS NULL
   AND d.last_seen_at < :'run_started'::timestamptz
   AND d.issue_code IN (SELECT issue_code FROM di.v_check_state WHERE run_state = 'CHECKED');

-- ---------- S3a 告警：P0（severity=ERROR）逐条即时 ----------
-- 幂等键 di:{drift_id}:OPEN ⇒ 同一条缺陷只发一次；重复跑本轮不会二次轰炸
INSERT INTO sys_notice
       (notice_no, notice_type, level, title, content, content_format,
        biz_type, biz_id, link_url, target_type, target_ids, channel_mask, idempotency_key)
SELECT 'DI' || to_char(now(), 'YYYYMMDDHH24MISSMS') || '-' || left(d.id::text, 6),
       'DATA_INTEGRITY', 'ERROR',
       '【P0】' || d.issue_code || ' ' || coalesce(r.check_name, ''),
       format('主体：%s / %s；期望=%s；实际=%s；影响 %s 条；真相=%s',
              d.entity_type, d.entity_no, d.truth_value, d.snapshot_value, d.affected_count,
              coalesce(d.truth_source, '-')),
       'TEXT', 'SNAPSHOT_DRIFT', d.entity_id::text,
       '/business/erp/data-integrity/' || d.id,
       'ROLE', '["20000000-0000-4000-8000-0000000f0003"]'::jsonb,   -- 总经理（示例；生产由配置项注入）
       1, 'di:' || d.id::text || ':OPEN'
  FROM biz_snapshot_drift d
  LEFT JOIN di.v_check_registry r ON r.issue_code = d.issue_code
 WHERE d.resolved_at IS NULL AND d.severity = 'ERROR'
ON CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING;

-- ---------- S3b 告警：P1（severity=WARN）按「巡检项 + 小时」合并成一封摘要 ----------
-- 去重键 di:digest:{issue_code}:{YYYYMMDDHH} ⇒ 一小时最多一封
INSERT INTO sys_notice
       (notice_no, notice_type, level, title, content, content_format,
        biz_type, target_type, target_ids, channel_mask, idempotency_key)
SELECT 'DID' || to_char(now(), 'YYYYMMDDHH24MISS') || '-' || left(md5(d.issue_code), 4),
       'DATA_INTEGRITY', 'WARN',
       '【P1】' || d.issue_code || ' 快照巡检摘要：' || count(*) || ' 条',
       format('本轮检出 %s 条。样例：%s（期望 %s）',
              count(*), min(d.entity_no), min(d.truth_value)),
       'TEXT', 'SNAPSHOT_DRIFT',
       'ROLE', '["20000000-0000-4000-8000-0000000f0002"]'::jsonb,      -- 采购主管（示例）
       1, 'di:digest:' || d.issue_code || ':' || to_char(now(), 'YYYYMMDDHH24')
  FROM biz_snapshot_drift d
 WHERE d.resolved_at IS NULL AND d.severity = 'WARN'
   AND d.last_seen_at >= :'run_started'::timestamptz
GROUP BY d.issue_code
ON CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING;

-- ---------- S3c 收敛回执：本轮刚自动关闭的缺陷，给值班一条「已恢复」通知 ----------
-- 为什么必须有：P0 告警发出去之后，如果系统自己不再检出，值班会一直以为还坏着。
-- 只发「本轮新关闭」的（resolved_at >= run_started），幂等键 di:{id}:RESOLVED。
INSERT INTO sys_notice
       (notice_no, notice_type, level, title, content, content_format,
        biz_type, biz_id, link_url, target_type, target_ids, channel_mask, idempotency_key)
SELECT 'DIR' || to_char(now(), 'YYYYMMDDHH24MISSMS') || '-' || left(d.id::text, 6),
       'DATA_INTEGRITY', 'SUCCESS',
       '【已恢复】' || d.issue_code || ' ' || coalesce(r.check_name, ''),
       format('主体：%s / %s 本轮未再检出，已自动置 %s（首次发现 %s）',
              d.entity_type, d.entity_no, d.resolution, to_char(d.detected_at, 'MM-DD HH24:MI')),
       'TEXT', 'SNAPSHOT_DRIFT', d.entity_id::text,
       '/business/erp/data-integrity/' || d.id,
       'ROLE', '["20000000-0000-4000-8000-0000000f0002"]'::jsonb,
       1, 'di:' || d.id::text || ':RESOLVED'
  FROM biz_snapshot_drift d
  LEFT JOIN di.v_check_registry r ON r.issue_code = d.issue_code
 WHERE d.resolved_at IS NOT NULL
   AND d.resolved_at >= :'run_started'::timestamptz
   AND d.resolution IN ('REPAIRED','ACCEPTED')
ON CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING;

COMMIT;