-- ======================================================
-- V0287 回滚：应收账款表 + AR 取号规则
--
-- 顺序：先删表，再删本迁移种下的那一行 biz_seq（按确定性 id 精确删，不动其它 seq_type）。
-- 回滚边界（给评审的一眼可见断言）：
--   * 只 DROP `biz_receivable`；
--   * `biz_seq` 只删 seq_type='AR' 的种子行（id=d0287000-…-0001），SO/SH/PO/OUT… 一律不碰；
--   * `biz_payable` 属 V0148，**不许连带删** —— 回滚 V0287 后 payable 必须还在
--     （否则应收应付的「镜像」校验在下游会失去参照物）。
-- ⚠ 回滚会**丢应收单据数据**（表里一旦有账就是财务凭证的配套数据），
--   down 仅用于迁移演练 / 隔离库；生产回滚前先 `pg_dump -t biz_receivable`。
-- ======================================================
DO $$
DECLARE
    n BIGINT;
BEGIN
    IF to_regclass('public.biz_receivable') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_receivable' INTO n;
        IF n > 0 THEN
            RAISE WARNING 'V0287 down：biz_receivable 有 % 行数据将被删除（生产回滚前请先 pg_dump）', n;
        END IF;
        RAISE NOTICE 'V0287 down：删除 biz_receivable（% 行）', n;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_receivable;

DELETE FROM biz_seq
 WHERE id = 'd0287000-0000-4000-8000-000000000001'::uuid
   AND seq_type = 'AR';

-- 回滚断言：表零残留 / 取号行零残留 / payable 与其它取号规则未受影响
DO $$
DECLARE
    n      BIGINT;
    d      TEXT;
    others BIGINT;
BEGIN
    IF to_regclass('public.biz_receivable') IS NOT NULL THEN
        RAISE EXCEPTION 'V0287 down 失败：biz_receivable 仍存在';
    END IF;

    SELECT count(*) INTO n FROM biz_seq WHERE seq_type = 'AR';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0287 down 失败：biz_seq(seq_type=AR) 仍残留 % 行', n;
    END IF;

    IF to_regclass('public.biz_payable') IS NULL THEN
        RAISE EXCEPTION 'V0287 down 失败：biz_payable 被连带删掉了（回滚边界越界）';
    END IF;

    -- 其它取号规则（SO/SH/PO/OUT…）必须原样还在
    SELECT count(*) INTO others FROM biz_seq WHERE seq_type <> 'AR' AND deleted_at IS NULL;
    IF others = 0 THEN
        RAISE EXCEPTION 'V0287 down 失败：其它 seq_type 的取号规则被误删（剩 0 行）';
    END IF;

    SELECT string_agg(seq_type, ',' ORDER BY seq_type) INTO d
      FROM biz_seq WHERE deleted_at IS NULL;
    RAISE NOTICE 'V0287 down 完成：biz_receivable 零残留、AR 取号行已删、payable 在位、其它取号规则 %', d;
END $$;
