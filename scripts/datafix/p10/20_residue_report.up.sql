-- ============================================================================
-- P11-followup / 20：全库扫描「批量生成零成本夹具」残留（**只读报告，不改数据**）
-- ----------------------------------------------------------------------------
-- 为什么有这一条分支（P10 的 10_ 只认 M-W1A-004 一个物料）：
--   2026-10-31 11:34:11，prod（8023）被客户端 172.18.10.7 打了一次
--   `POST /api/business/erp/serial/batch-generate`（material=M-W1A-004 / WH01 / count=10），
--   写出一条 `SERIAL_BATCH` 流水 + 10 个 `source_id IS NULL` 的序列号 + 一行
--   `qty=10 / amount=0 / avg_cost=0` 的成本账，却没有库位账 ⇒ B1（BLOCKER）+ C3（WARN）同时红。
--   P10 的 10_ 脚本把它清掉过一次（12:12:58），但**只要有人再点一次「批量生成」，就再来一次**，
--   而且下次可能是另一个物料（W2b/W2c 的 e2e 也换物料）。
--   ⇒ 把判据从「固定物料编码」升级成「批量生成夹具的形态特征」，这样任何物料复发都能一眼看到。
--
-- 判据（四道**同时**成立才算夹具；任何一道不成立就不是本分支的对象）：
--   ① 该物料至少有一条 `biz_inventory_txn` 满足 `biz_type='SERIAL_IN' AND doc_type='SERIAL_BATCH'`
--      —— 只有 `serial_model::batch_generate`（`POST /business/erp/serial/batch-generate`）写这个组合
--      （`BIZ_TYPE_SERIAL_IN` + `DOC_TYPE_SERIAL_BATCH`，见 serial_model.rs:802）；
--   ② 没有业务来源：`biz_stock_in_item` / `biz_purchase_order_item` 里都没有它的行；
--   ③ 该物料**存在**序列号，且**全部**是 `source_type='PURCHASE' AND source_id IS NULL`
--      —— 这是批量生成接口的落库形态（收货过账走 `stockin_model::insert_serials`，必定回填 source_id）；
--   ④ 库存零价值：该物料所有 `biz_inventory` 行的 `amount` 与 `avg_cost` 都是 0
--      （有价值 = 有账载来源，删了就是毁账，一律不碰）。
--
-- 反例（**故意不命中**）：
--   * 正常收货进来的物料：有 stock_in_item ⇒ ② 不成立；
--   * 批量生成后做过收货/调拨、建立了成本：amount/avg_cost ≠ 0 ⇒ ④ 不成立；
--   * 人工补过号、混有收货号：③ 不成立。
--
-- 输出：`public.datafix_residue_report`（每次运行先清空再写，只留本次结果）+ 若干 RAISE NOTICE。
-- 清理：见 `21_residue_clean.up.sql`（默认 dry-run，`P10_GENERIC_APPLY=1` 才动数据）。
-- ============================================================================

CREATE TABLE IF NOT EXISTS public.datafix_residue_report (
    run_at        timestamptz     NOT NULL DEFAULT now(),
    material_id   uuid            NOT NULL,
    material_code text            NOT NULL,
    inv_rows      integer         NOT NULL,
    inv_qty       numeric(18,4)   NOT NULL,
    inv_amount    numeric(18,4)   NOT NULL,
    txn_rows      integer         NOT NULL,
    serial_alive  integer         NOT NULL,
    serial_rows   integer         NOT NULL,
    verdict       text            NOT NULL
);

COMMENT ON TABLE public.datafix_residue_report IS
    'P11-followup：批量生成零成本夹具残留的扫描报告（只读产物，每次运行重写）';

DELETE FROM public.datafix_residue_report;

INSERT INTO public.datafix_residue_report
    (material_id, material_code, inv_rows, inv_qty, inv_amount,
     txn_rows, serial_alive, serial_rows, verdict)
WITH cand AS (
    SELECT m.id AS material_id, m.material_code
      FROM biz_material m
     WHERE m.deleted_at IS NULL
       -- ① 批量生成接口的流水签名（biz_type=SERIAL_IN + doc_type=SERIAL_BATCH）
       AND EXISTS (SELECT 1 FROM biz_inventory_txn t
                    WHERE t.material_id = m.id
                      AND t.biz_type = 'SERIAL_IN' AND t.doc_type = 'SERIAL_BATCH')
       -- ② 没有任何收货 / 采购明细
       AND NOT EXISTS (SELECT 1 FROM biz_stock_in_item si WHERE si.material_id = m.id)
       AND NOT EXISTS (SELECT 1 FROM biz_purchase_order_item pi WHERE pi.material_id = m.id)
       -- ③ 序列号齐全，且**没有任何一条带真实来源**（批量生成的号没有来源单）
       --    判据写成「来源字段为空」而不是「等于 'PURCHASE'」：
       --    2026-10-31 隔离实例实测，同一接口在不带 source_type 时落的是空串
       --    （serial_model.rs: `params.source_type.unwrap_or_default()`），而 prod 11:34 那批
       --    是 'PURCHASE'。只认 'PURCHASE' 会把空串形态的夹具整批漏掉（假绿）。
       AND EXISTS (SELECT 1 FROM biz_material_serial s WHERE s.material_id = m.id)
       AND NOT EXISTS (SELECT 1 FROM biz_material_serial s
                        WHERE s.material_id = m.id
                          AND ((s.source_type IS NOT NULL AND s.source_type NOT IN ('', 'PURCHASE'))
                               OR s.source_id IS NOT NULL))
       -- ④ 库存零价值（无账载金额 / 无成本基础）
       AND NOT EXISTS (SELECT 1 FROM biz_inventory i
                        WHERE i.material_id = m.id
                          AND (i.amount <> 0 OR i.avg_cost <> 0))
)
SELECT c.material_id,
       c.material_code,
       (SELECT count(*)            FROM biz_inventory i          WHERE i.material_id = c.material_id),
       coalesce((SELECT sum(i.qty)    FROM biz_inventory i          WHERE i.material_id = c.material_id), 0),
       coalesce((SELECT sum(i.amount) FROM biz_inventory i          WHERE i.material_id = c.material_id), 0),
       (SELECT count(*)            FROM biz_inventory_txn t      WHERE t.material_id = c.material_id),
       (SELECT count(*)            FROM biz_material_serial s    WHERE s.material_id = c.material_id
                                     AND s.deleted_at IS NULL),
       (SELECT count(*)            FROM biz_material_serial s    WHERE s.material_id = c.material_id),
       '批量生成零成本夹具（无单据、零价值）—— 命中 B1/C3，用 21_residue_clean 清'
  FROM cand c
 ORDER BY c.material_code;

DO $$
DECLARE
    r      record;
    v_hit  integer := 0;
    v_qty  numeric := 0;
BEGIN
    FOR r IN SELECT * FROM public.datafix_residue_report ORDER BY material_code LOOP
        v_hit := v_hit + 1;
        v_qty := v_qty + r.inv_qty;
        RAISE NOTICE 'P10-20: 命中夹具残留 % —— 库存 % 行/数量 %/金额 %；流水 % 条；存活序列号 % / 全部 % 条',
            r.material_code, r.inv_rows, r.inv_qty, r.inv_amount,
            r.txn_rows, r.serial_alive, r.serial_rows;
    END LOOP;

    IF v_hit = 0 THEN
        RAISE NOTICE 'P10-20: 全库无「批量生成零成本夹具」残留（库存合计未被这类夹具污染）';
    ELSE
        RAISE NOTICE 'P10-20: 合计命中 % 个物料 / 零成本库存数量 % —— 清理请跑 P10_GENERIC_APPLY=1 bash scripts/datafix/p10/apply.sh',
            v_hit, v_qty;
    END IF;
END
$$;
