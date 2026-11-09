-- P17 清理：只清「孤儿库位账」与「批次流水已不存在的 SERIAL_BATCH 凭证」（t_45bda7ad）
--
-- 为什么是它们：E2E-08 的老清场（e2e08_master_common.sh::cleanup_own）删物料/仓库时
-- 不删库位账、也不删 batch-generate 写出的凭证 ⇒ 每跑一次多一行 qty=3 孤儿库位账
-- + 一张 1405/1901 各 30 元的无主凭证 + 一条 VOUCHER 状态日志。
-- 清场逻辑已在同一提交里修好；本脚本只清**存量**。
--
-- 三道门禁（任一命中整单回滚，不做静默取舍）：
--   G1 目标凭证不得有红冲/被红冲关系（is_red、reversed_by、reverse_of）
--   G2 目标凭证不得被任何业务表以 *_voucher_id 引用（动态扫 information_schema）
--   G3 目标凭证分录科目只能是 1405/1901（batch-generate 固定口径）；库位账行不得被
--      biz_inventory / biz_material_serial / biz_stock_transfer_item 引用
-- 留痕：每一行被删的原始数据都写进 datafix_p17_doomed（jsonb），可回滚/复查。
\set ON_ERROR_STOP 1
\pset pager off

BEGIN;

CREATE TABLE IF NOT EXISTS datafix_p17_doomed (
    tbl       text        NOT NULL,
    row       jsonb       NOT NULL,
    doomed_at timestamptz NOT NULL DEFAULT now()
);

-- ── 目标清单（先落临时表，门禁与留痕都基于它）────────────────────────────────
CREATE TEMP TABLE p17_vch ON COMMIT DROP AS
SELECT v.*
  FROM biz_voucher v
 WHERE v.source_type = 'SERIAL_BATCH'
   AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = v.source_id);

CREATE TEMP TABLE p17_loc ON COMMIT DROP AS
SELECT l.*
  FROM biz_inventory_location l
 WHERE (NOT EXISTS (SELECT 1 FROM biz_material  m WHERE m.id = l.material_id)
     OR NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.id = l.warehouse_id));

-- ── G1 红冲关系 ────────────────────────────────────────────────────────────
DO $$
DECLARE n bigint;
BEGIN
    SELECT count(*) INTO n FROM p17_vch
     WHERE is_red <> 0 OR reversed_by IS NOT NULL OR reverse_of IS NOT NULL;
    IF n > 0 THEN
        RAISE EXCEPTION 'G1 门禁：% 张目标凭证有红冲/反冲关系，拒绝整单清理', n;
    END IF;
END $$;

-- ── G2 被业务表引用（动态扫所有 *_voucher_id 列）─────────────────────────────
DO $$
DECLARE r record; n bigint; ids uuid[];
BEGIN
    SELECT array_agg(id) INTO ids FROM p17_vch;
    IF ids IS NULL THEN RETURN; END IF;
    FOR r IN
        SELECT c.table_name
          FROM information_schema.columns c
         WHERE c.table_schema = 'public'
           AND c.column_name LIKE '%voucher_id'
           AND c.table_name <> 'biz_voucher_entry'
    LOOP
        EXECUTE format('SELECT count(*) FROM %I WHERE voucher_id = ANY($1)', r.table_name)
           INTO n USING ids;
        IF n > 0 THEN
            RAISE EXCEPTION 'G2 门禁：目标凭证被 % 引用 % 行，拒绝清理', r.table_name, n;
        END IF;
    END LOOP;
END $$;

-- ── G3 分录科目口径 + 库位账被引用 ──────────────────────────────────────────
DO $$
DECLARE n bigint;
BEGIN
    SELECT count(*) INTO n
      FROM biz_voucher_entry e JOIN p17_vch v ON v.id = e.voucher_id
     WHERE e.account_code NOT IN ('1405', '1901');
    IF n > 0 THEN
        RAISE EXCEPTION 'G3 门禁：目标凭证有 % 行分录不是 1405/1901，不像 batch-generate 口径，拒绝清理', n;
    END IF;

    SELECT count(*) INTO n FROM biz_inventory i JOIN p17_loc l ON l.id = i.location_id;
    IF n > 0 THEN
        RAISE EXCEPTION 'G3 门禁：目标库位账被成本账引用 % 行（先修成本账），拒绝清理', n;
    END IF;
    SELECT count(*) INTO n FROM biz_material_serial s JOIN p17_loc l ON l.id = s.location_id;
    IF n > 0 THEN
        RAISE EXCEPTION 'G3 门禁：目标库位账被序列号引用 % 行（先解引用），拒绝清理', n;
    END IF;
    SELECT count(*) INTO n FROM biz_stock_transfer_item t
      JOIN p17_loc l ON l.id IN (t.from_location_id, t.to_location_id);
    IF n > 0 THEN
        RAISE EXCEPTION 'G3 门禁：目标库位账被调拨明细引用 % 行（先解引用），拒绝清理', n;
    END IF;
END $$;

-- ── 留痕（逐行 jsonb）──────────────────────────────────────────────────────
INSERT INTO datafix_p17_doomed (tbl, row)
SELECT 'biz_inventory_location', to_jsonb(t) FROM p17_loc t;

INSERT INTO datafix_p17_doomed (tbl, row)
SELECT 'biz_status_log', to_jsonb(s)
  FROM biz_status_log s
 WHERE s.entity_type = 'VOUCHER' AND s.entity_id IN (SELECT id FROM p17_vch);

INSERT INTO datafix_p17_doomed (tbl, row)
SELECT 'biz_voucher_entry', to_jsonb(e)
  FROM biz_voucher_entry e WHERE e.voucher_id IN (SELECT id FROM p17_vch);

INSERT INTO datafix_p17_doomed (tbl, row)
SELECT 'biz_voucher', to_jsonb(v) FROM p17_vch v;

-- ── 删除（状态日志 → 分录 → 凭证；库位账最后）──────────────────────────────
\echo '── P17 待删行数（逐类）──'
SELECT 'biz_inventory_location' AS tbl, count(*) AS rows FROM p17_loc
UNION ALL SELECT 'biz_voucher',       count(*) FROM p17_vch
UNION ALL SELECT 'biz_voucher_entry', count(*) FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM p17_vch)
UNION ALL SELECT 'biz_status_log',    count(*) FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN (SELECT id FROM p17_vch)
ORDER BY 1;

DELETE FROM biz_status_log
 WHERE entity_type = 'VOUCHER' AND entity_id IN (SELECT id FROM p17_vch);
\echo '  ② 状态日志删除：' :ROW_COUNT ' 行'

DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM p17_vch);
\echo '  ③ 凭证分录删除：' :ROW_COUNT ' 行'

DELETE FROM biz_voucher       WHERE id IN (SELECT id FROM p17_vch);
\echo '  ④ 凭证删除：' :ROW_COUNT ' 张'

DELETE FROM biz_inventory_location WHERE id IN (SELECT id FROM p17_loc);
\echo '  ⑤ 孤儿库位账删除：' :ROW_COUNT ' 行'

COMMIT;

\echo 'P17 清理完成（留痕表：datafix_p17_doomed）'
