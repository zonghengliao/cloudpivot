-- ======================================================
-- V0292 回滚：收款单头 `biz_receipt` + 核销明细 `biz_receipt_allocation` + RC 取号规则
--
-- 顺序：先删明细（有外键指向收款头，晚删会被 RESTRICT 拦），再删头，最后按确定性 id
--       精确删本迁移种下的一行 `biz_seq(seq_type='RC')`（不动其它 seq_type）。
-- 回滚边界（给评审的一眼可见断言）：
--   * 只 DROP 本卡的两张表；
--   * `biz_receivable`（V0287）/ `biz_voucher` / `biz_payment` **不许连带删** ——
--     receivable 是核销明细的外键目标，误删会让「收款 ↔ 应收」的核对失去参照物；
--   * `biz_seq` 只删 RC 那一行，SO/SH/PO/AR… 一律不碰。
-- ⚠ 回滚会**丢收款单与核销明细数据**：核销明细是「应收已收款」的来源凭据，
--   一旦有账，回滚后 `biz_receivable.received_amount` 会与任何收款记录对不上。
--   down 仅用于迁移演练 / 隔离库；生产回滚前先
--   `pg_dump -t biz_receipt -t biz_receipt_allocation`。
-- ======================================================
DO $$
DECLARE
    n BIGINT;
    m BIGINT;
BEGIN
    IF to_regclass('public.biz_receipt') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_receipt' INTO n;
        m := 0;
        IF to_regclass('public.biz_receipt_allocation') IS NOT NULL THEN
            EXECUTE 'SELECT count(*) FROM biz_receipt_allocation' INTO m;
        END IF;
        IF n > 0 OR m > 0 THEN
            RAISE WARNING 'V0292 down：将删除 biz_receipt % 行 / biz_receipt_allocation % 行（生产回滚前请先 pg_dump）', n, m;
        END IF;
        RAISE NOTICE 'V0292 down：删除收款单头 % 行 / 核销明细 % 行', n, m;
    END IF;
END $$;

-- 先明细后头（外键依赖方向）
DROP TABLE IF EXISTS biz_receipt_allocation;
DROP TABLE IF EXISTS biz_receipt;

DELETE FROM biz_seq
 WHERE id = 'd0292000-0000-4000-8000-000000000001'::uuid
   AND seq_type = 'RC';

-- 回滚断言：两表零残留 / RC 取号行零残留 / receivable 与其它取号规则未受影响
DO $$
DECLARE
    n      BIGINT;
    d      TEXT;
    others BIGINT;
BEGIN
    IF to_regclass('public.biz_receipt') IS NOT NULL THEN
        RAISE EXCEPTION 'V0292 down 失败：biz_receipt 仍存在';
    END IF;
    IF to_regclass('public.biz_receipt_allocation') IS NOT NULL THEN
        RAISE EXCEPTION 'V0292 down 失败：biz_receipt_allocation 仍存在';
    END IF;

    SELECT count(*) INTO n FROM biz_seq WHERE seq_type = 'RC';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0292 down 失败：biz_seq(seq_type=RC) 仍残留 % 行', n;
    END IF;

    -- 回滚边界：应收表（核销明细的外键目标）必须还在
    IF to_regclass('public.biz_receivable') IS NULL THEN
        RAISE EXCEPTION 'V0292 down 失败：biz_receivable 被连带删掉了（回滚边界越界）';
    END IF;

    -- 其它取号规则（SO/SH/PO/OUT/AR…）必须原样还在
    SELECT count(*) INTO others FROM biz_seq WHERE seq_type <> 'RC' AND deleted_at IS NULL;
    IF others = 0 THEN
        RAISE EXCEPTION 'V0292 down 失败：其它 seq_type 的取号规则被误删（剩 0 行）';
    END IF;

    SELECT string_agg(seq_type, ',' ORDER BY seq_type) INTO d
      FROM biz_seq WHERE deleted_at IS NULL;
    RAISE NOTICE 'V0292 down 完成：两张表零残留、RC 取号行已删、receivable 在位、其它取号规则 %', d;
END $$;
