-- ======================================================
-- V0298 回滚：销售发票 `biz_invoice` + IV 取号规则
--
-- 顺序：先删表，再删本迁移种下的那一行 biz_seq（按确定性 id 精确删，不动其它 seq_type）。
-- 回滚边界（给评审的一眼可见断言）：
--   * 只 DROP `biz_invoice`；
--   * `biz_seq` 只删 seq_type='IV' 的种子行（id=d0295000-…-0001），AR/RC/SO/SH/PO/OUT… 一律不碰；
--   * `biz_receivable` 属 V0287，**不许连带删** —— 本表的 receivable_id 是它唯一的外键指向，
--     回滚 V0298 后应收必须还在（否则下游应收模块失去台账）。
-- ⚠ 回滚会**丢开票数据**（发票是收入凭证与税务对账的配套数据），
--   down 仅用于迁移演练 / 隔离库；生产回滚前先 `pg_dump -t biz_invoice`。
-- ======================================================
DO $$
DECLARE
    n BIGINT;
BEGIN
    IF to_regclass('public.biz_invoice') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_invoice' INTO n;
        IF n > 0 THEN
            RAISE WARNING 'V0298 down：biz_invoice 有 % 行数据将被删除（生产回滚前请先 pg_dump）', n;
        END IF;
        RAISE NOTICE 'V0298 down：删除 biz_invoice（% 行）', n;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_invoice;

DELETE FROM biz_seq
 WHERE id = 'd0295000-0000-4000-8000-000000000001'::uuid
   AND seq_type = 'IV';

-- 回滚断言：表零残留 / 取号行零残留 / 应收与其它取号规则未受影响
DO $$
DECLARE
    n      BIGINT;
    d      TEXT;
    others BIGINT;
BEGIN
    IF to_regclass('public.biz_invoice') IS NOT NULL THEN
        RAISE EXCEPTION 'V0298 down 失败：biz_invoice 仍存在';
    END IF;

    SELECT count(*) INTO n FROM biz_seq WHERE seq_type = 'IV';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0298 down 失败：biz_seq(seq_type=IV) 仍残留 % 行', n;
    END IF;

    IF to_regclass('public.biz_receivable') IS NULL THEN
        RAISE EXCEPTION 'V0298 down 失败：biz_receivable 被连带删掉了（回滚边界越界）';
    END IF;

    -- 其它取号规则（AR/RC/SO/SH/PO/OUT…）必须原样还在
    SELECT count(*) INTO others FROM biz_seq WHERE seq_type <> 'IV' AND deleted_at IS NULL;
    IF others = 0 THEN
        RAISE EXCEPTION 'V0298 down 失败：其它 seq_type 的取号规则被误删（剩 0 行）';
    END IF;

    SELECT string_agg(seq_type, ',' ORDER BY seq_type) INTO d
      FROM biz_seq WHERE deleted_at IS NULL;
    RAISE NOTICE 'V0298 down 完成：biz_invoice 零残留、IV 取号行已删、receivable 在位、其它取号规则 %', d;
END $$;
