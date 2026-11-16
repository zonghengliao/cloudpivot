-- ============================================================================
-- V0419__voucher_source_no_width.down（M1-FIX 回滚：source_no 收窄回 VARCHAR(32)）
--
-- 顺序：① 数据护栏 → ② 收窄 → ③ 拆注释 → ④ 收尾自检
--
-- ① 数据护栏（有 > 32 字就拒绝）：V0419 之后建出来的外币手工凭证，`source_no` 可能是 33~57 字。
--    收窄到 32 会让这些行**要么被截断、要么整表 ALTER 失败** —— 两者都是丢业务数据。
--    要回滚得先把那些外币凭证红冲 / 作废（`length(source_no) > 32` 的那批）。
-- ② **只动 source_no**：`source_doc_no` 是 V0357 建的 VARCHAR(64)，本迁移 up 没改它，
--    down 也绝不碰它（碰了就是「回滚 A 顺手改窄 B」，B 的 64 字业务单号会一起炸）。
-- ③ 注释恢复成 V0419 之前的「无注释」状态（V0149 建列时没写 COMMENT）。
-- ④ 收尾自检：source_no 必须真的回到 32、source_doc_no 必须仍是 64（没被误伤）。
--
-- 全部 `IF EXISTS` / 先查列在不在 ⇒ 重复执行不报错。
-- ============================================================================

-- ① 数据护栏：有 > 32 字的 source_no 就拒绝收窄
DO $$
DECLARE
    n bigint := 0;
BEGIN
    IF to_regclass('public.biz_voucher') IS NOT NULL
       AND EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_schema = 'public' AND table_name = 'biz_voucher'
               AND column_name = 'source_no'
       ) THEN
        SELECT count(*) INTO n
          FROM biz_voucher
         WHERE source_no IS NOT NULL AND length(source_no) > 32;
        IF n > 0 THEN
            RAISE EXCEPTION
                'V0419 down 拒绝执行：biz_voucher 有 % 行的 source_no 超过 32 字（V0419 之后建的外币手工凭证的'
                '单号是 33~57 字）。收窄会把业务单号截断 / 整表 ALTER 失败。请先把那些外币凭证红冲 / 作废，'
                '或保留 VARCHAR(64)（更宽的列不影响任何读路径）', n;
        END IF;
    END IF;
    RAISE NOTICE 'V0419 down: 数据护栏通过（无 > 32 字的 source_no）';
END $$;

-- ② 收窄 source_no（幂等：已经是 ≤ 32 就跳过）
DO $$
DECLARE
    v_len integer;
BEGIN
    SELECT character_maximum_length INTO v_len
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_voucher'
       AND column_name = 'source_no';

    IF v_len IS NULL THEN
        RAISE NOTICE 'V0419 down: biz_voucher.source_no 不存在，跳过';
    ELSIF v_len > 32 THEN
        ALTER TABLE biz_voucher ALTER COLUMN source_no TYPE VARCHAR(32);
        RAISE NOTICE 'V0419 down: biz_voucher.source_no 由 VARCHAR(%) 收窄回 VARCHAR(32)', v_len;
    ELSE
        RAISE NOTICE 'V0419 down: biz_voucher.source_no 已是 VARCHAR(%)，跳过（幂等）', v_len;
    END IF;
END $$;

-- ③ 拆注释（恢复 V0149 建列时的「无注释」状态）
COMMENT ON COLUMN biz_voucher.source_no IS NULL;

-- ④ 收尾自检：source_no 真的回到 32、source_doc_no 没被误伤（仍 64）
DO $$
DECLARE
    v_source    integer;
    v_doc       integer;
BEGIN
    SELECT character_maximum_length INTO v_source
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_voucher' AND column_name = 'source_no';
    SELECT character_maximum_length INTO v_doc
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_voucher' AND column_name = 'source_doc_no';

    IF v_source IS DISTINCT FROM 32 THEN
        RAISE EXCEPTION 'V0419 down 失败：source_no 宽度是 %（应为 32）', coalesce(v_source::text, '不存在');
    END IF;
    IF v_doc IS DISTINCT FROM 64 THEN
        RAISE EXCEPTION 'V0419 down 失败：source_doc_no 宽度是 %（应保持 V0357 的 64，本迁移不该动它）',
            coalesce(v_doc::text, '不存在');
    END IF;

    RAISE NOTICE 'V0419 down 完成：source_no 回到 VARCHAR(32)，source_doc_no 保持 VARCHAR(64) 未动';
END $$;
