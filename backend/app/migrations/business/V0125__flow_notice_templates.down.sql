-- ======================================================
-- V0125 down：审批通知 4 个模板（批次 D · D3c）
--
-- 只删本迁移插入的行（按固定 UUID），末尾断言零残留。
-- 不删通知数据（`sys_notice` 里已发出的历史通知是审计资产，删模板不动通知）。
-- ======================================================

DO $$
DECLARE
    ids uuid[] := ARRAY['40000000-0000-0000-0000-0000000000f1',
                        '40000000-0000-0000-0000-0000000000f2',
                        '40000000-0000-0000-0000-0000000000f3',
                        '40000000-0000-0000-0000-0000000000f4']::uuid[];
    left_rows int;
BEGIN
    DELETE FROM sys_notice_template WHERE id = ANY(ids);

    SELECT count(*) INTO left_rows FROM sys_notice_template WHERE id = ANY(ids);
    IF left_rows <> 0 THEN
        RAISE EXCEPTION 'V0125 down 自检失败：仍残留 % 行模板', left_rows;
    END IF;

    -- 编码也不许残留（否则重跑 up 会命中 uk_sys_notice_template_code 而静默跳过）
    SELECT count(*) INTO left_rows
      FROM sys_notice_template
     WHERE template_code IN ('FLOW_TODO', 'FLOW_RESULT', 'FLOW_CC', 'FLOW_URGE')
       AND deleted_at IS NULL;
    IF left_rows <> 0 THEN
        RAISE EXCEPTION 'V0125 down 自检失败：仍残留 % 个审批模板编码', left_rows;
    END IF;
END $$;
