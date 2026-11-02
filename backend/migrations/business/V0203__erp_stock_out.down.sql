-- ============================================================
-- V0203 down：撤掉出库单两表 + 权限码 + 取号规则
--
-- 顺序（外键/引用关系决定）：
--   ① 先删权限码的授权行（sys_role_menu）→ 再删 sys_menu 的 7 个节点
--   ② 删取号规则 biz_seq(STOCK_OUT)
--   ③ 删明细表 → 删单头表（item 有 ON DELETE CASCADE 硬外键，显式按序删更清楚）
--
-- ⚠ 本 down **不删业务数据以外的任何东西**：已过账的出库单在 business 库里是账务证据，
--   一旦 drop 掉两张表，biz_inventory_txn 里那些 OUT 流水就失去来源单据
--   （doc_type='STOCK_OUT' 的行会变成孤儿）。回滚前请先确认库里没有 status='POSTED'
--   的出库单（本文件末尾的检查块会挡住「有已过账单据还硬删表」）。
-- ============================================================

-- ---------- ③a 安全检查：有已过账单据就中止 ----------
DO $$
DECLARE
    n integer;
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.tables
                WHERE table_schema = 'public' AND table_name = 'biz_stock_out') THEN
        EXECUTE 'SELECT count(*) FROM biz_stock_out WHERE status = ''POSTED''' INTO n;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0203 down 中止：还有 % 张已过账（POSTED）的出库单。'
                            ' 已过账单据是账务证据（biz_inventory_txn 里有对应的 OUT 流水），'
                            ' 请先作废这些单据再回滚本迁移', n;
        END IF;
    END IF;
END $$;

-- ---------- ③b 两表 ----------
DROP TABLE IF EXISTS biz_stock_out_item;
DROP TABLE IF EXISTS biz_stock_out;

-- ---------- ② 取号规则 ----------
DELETE FROM biz_seq WHERE seq_type = 'STOCK_OUT';

-- ---------- ① 权限码 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001105'::uuid,
                   '20000000-0000-0000-0000-000000001106'::uuid,
                   '20000000-0000-0000-0000-000000001107'::uuid,
                   '20000000-0000-0000-0000-000000001108'::uuid,
                   '20000000-0000-0000-0000-000000001109'::uuid,
                   '20000000-0000-0000-0000-000000001110'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001105'::uuid,
              '20000000-0000-0000-0000-000000001106'::uuid,
              '20000000-0000-0000-0000-000000001107'::uuid,
              '20000000-0000-0000-0000-000000001108'::uuid,
              '20000000-0000-0000-0000-000000001109'::uuid,
              '20000000-0000-0000-0000-000000001110'::uuid);

-- ---------- 残留自检（down 后必须是 0） ----------
DO $$
DECLARE
    n_tbl  integer;
    n_menu integer;
    n_seq  integer;
BEGIN
    SELECT count(*) INTO n_tbl FROM information_schema.tables
     WHERE table_schema = 'public' AND table_name IN ('biz_stock_out', 'biz_stock_out_item');
    SELECT count(*) INTO n_menu FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001105'::uuid,
                  '20000000-0000-0000-0000-000000001106'::uuid,
                  '20000000-0000-0000-0000-000000001107'::uuid,
                  '20000000-0000-0000-0000-000000001108'::uuid,
                  '20000000-0000-0000-0000-000000001109'::uuid,
                  '20000000-0000-0000-0000-000000001110'::uuid);
    SELECT count(*) INTO n_seq FROM biz_seq WHERE seq_type = 'STOCK_OUT';
    IF n_tbl <> 0 OR n_menu <> 0 OR n_seq <> 0 THEN
        RAISE EXCEPTION 'V0203 down 残留：表 % / 菜单 % / 取号 %（应全为 0）', n_tbl, n_menu, n_seq;
    END IF;
    RAISE NOTICE 'V0203 down 完成：两表 / 7 个权限节点 / 1 条取号规则全部撤销，零残留';
END $$;
