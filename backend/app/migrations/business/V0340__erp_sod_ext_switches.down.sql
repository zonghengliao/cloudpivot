-- ============================================================
-- V0340 down：撤销「SoD 扩展三开关」（P3-07）
--
-- # 撤销范围（只有本迁移写进去的三行，不碰 V0196 的四条老参数）
--
--   erp.separation_of_duties.approve_execute
--   erp.separation_of_duties.stocktake
--   erp.separation_of_duties.purchase_accept
--
-- # 回滚的业务后果（写在这里，避免有人以为「回滚无害」）
--
--   代码侧（`business::erp::separation`）参数缺失时走**代码默认值 false**：
--   三对新的职责分离**全部失效**（审批人可以自己执行、制单人可自己确认盘点、
--   采购员可自己验收），且不能再在系统参数页里打开。这是**放宽**内控的方向，
--   回滚前请确认这是有意的。
--
--   已有业务数据不受影响：这三条只影响判定，没有任何表结构 / 留痕依赖。
--
-- `DELETE ... WHERE config_key IN (...)` ⇒ 重复执行不报错。
-- ============================================================

DO $$
DECLARE
    v_other int;
BEGIN
    -- 回滚前先看清：除本卡三条之外，别的 erp.separation_of_duties.* 参数不该被这条 down 带走
    SELECT count(*) INTO v_other
      FROM sys_config
     WHERE config_key LIKE 'erp.separation_of_duties.%'
       AND deleted_at IS NULL
       AND config_key NOT IN (
               'erp.separation_of_duties.approve_execute',
               'erp.separation_of_duties.stocktake',
               'erp.separation_of_duties.purchase_accept'
           );
    IF v_other > 0 THEN
        RAISE NOTICE 'V0340 down：库内另有 % 条 erp.separation_of_duties.* 参数（V0196 等），本 down 不动它们', v_other;
    END IF;
END $$;

DELETE FROM sys_config
 WHERE config_key IN (
           'erp.separation_of_duties.approve_execute',
           'erp.separation_of_duties.stocktake',
           'erp.separation_of_duties.purchase_accept'
       );

-- 自检：三条确实不在了
DO $$
DECLARE
    v_left int;
BEGIN
    SELECT count(*) INTO v_left
      FROM sys_config
     WHERE config_key IN (
               'erp.separation_of_duties.approve_execute',
               'erp.separation_of_duties.stocktake',
               'erp.separation_of_duties.purchase_accept'
           );
    IF v_left <> 0 THEN
        RAISE EXCEPTION 'V0340 down 自检失败：仍有 % 条扩展开关未删除', v_left;
    END IF;
END $$;
