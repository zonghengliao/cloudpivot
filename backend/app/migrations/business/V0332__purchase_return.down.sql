-- ======================================================
-- V0332 down：采购退货闭环回滚
--
-- 顺序（受外键与业务事实约束）：
--   ① 安全阀：已有确认过账的退货单 / 已有审批实例 → RAISE（回滚会把账务单据结构抹掉）；
--   ② 流程（实例→版本→边→节点→指派→定义）；
--   ③ 菜单与角色授权、取号规则；
--   ④ 三张表（先明细后主表）。
--
-- 说明：`biz_supplier_receivable` 与 `biz_purchase_return` 之间没有硬外键
-- （单据互链一律弱引用，避免过账事务成环），所以 drop 顺序只需照顾 _item → 主表。
-- ======================================================

DO $$
DECLARE
    v_rows integer;
BEGIN
    -- ① 安全阀：功能已被使用就不许回滚（先把业务数据处置掉）
    SELECT count(*) INTO v_rows FROM biz_purchase_return;
    IF v_rows > 0 THEN
        RAISE EXCEPTION 'V0332 down 中止：biz_purchase_return 已有 % 行退货单（含历史单据），'
                        '回滚会删掉业务数据与账务引用。请先导出 / 归档并清理后再回滚', v_rows;
    END IF;
    SELECT count(*) INTO v_rows FROM biz_supplier_receivable;
    IF v_rows > 0 THEN
        RAISE EXCEPTION 'V0332 down 中止：biz_supplier_receivable 已有 % 行', v_rows;
    END IF;
    SELECT count(*) INTO v_rows FROM biz_flow_instance
     WHERE biz_type = 'PRT_APPROVE';
    IF v_rows > 0 THEN
        RAISE EXCEPTION 'V0332 down 中止：已有 % 个采购退货审批实例（version_id 是 RESTRICT 外键）', v_rows;
    END IF;
    SELECT count(*) INTO v_rows
      FROM biz_stock_out
     WHERE source_type = 'PURCHASE_RETURN';
    IF v_rows > 0 THEN
        RAISE EXCEPTION 'V0332 down 中止：已有 % 张 PURCHASE_RETURN 来源的出库单（账已动过）', v_rows;
    END IF;

    RAISE NOTICE 'V0332 down：安全阀全部通过（无退货单 / 无应退款 / 无实例 / 无来源出库单）';
END $$;

-- ② 流程（先删依赖行，再删定义）
DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (SELECT id FROM biz_flow_node
                    WHERE version_id = '20000000-0000-4000-8000-0000000e0332'::uuid);
DELETE FROM biz_flow_edge
 WHERE version_id = '20000000-0000-4000-8000-0000000e0332'::uuid;
DELETE FROM biz_flow_node
 WHERE version_id = '20000000-0000-4000-8000-0000000e0332'::uuid;
DELETE FROM biz_flow_def_version
 WHERE id = '20000000-0000-4000-8000-0000000e0332'::uuid;
DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-0000000e0331'::uuid;

-- ③ 菜单 / 角色授权 / 取号
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000003341'::uuid,
                   '20000000-0000-0000-0000-000000003344'::uuid,
                   '20000000-0000-0000-0000-000000003345'::uuid,
                   '20000000-0000-0000-0000-000000003346'::uuid);
DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000003341'::uuid,
              '20000000-0000-0000-0000-000000003344'::uuid,
              '20000000-0000-0000-0000-000000003345'::uuid,
              '20000000-0000-0000-0000-000000003346'::uuid);
DELETE FROM biz_seq WHERE seq_type IN ('PRT', 'SR');

-- ④ 三张表
DROP TABLE IF EXISTS biz_purchase_return_item;
DROP TABLE IF EXISTS biz_purchase_return;
DROP TABLE IF EXISTS biz_supplier_receivable;
