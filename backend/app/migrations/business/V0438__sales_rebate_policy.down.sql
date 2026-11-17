-- ============================================================================
-- V0438__sales_rebate_policy.down.sql（回滚 M3-15 销售返利政策表 + 规则）
--
-- 顺序：先删「引用方向」的外键侧（规则表随政策表 CASCADE，但显式删一遍更清楚）→
--       删菜单授权行 → 删菜单节点 → 删号段行 → 删两张表。
--
-- 护栏：**库里已有未删的返利政策时拒绝回滚**。政策一旦被结算引用（M3-16 起），
--   删表等于把「当时按什么比率算的」从系统里抹掉；快照虽有，源表不该被迁移随手删除。
--   真要下线：先 `UPDATE … SET deleted_at = now()` 逐条软删（有审计），再回滚。
--
-- 幂等：全部 `IF EXISTS` / `DELETE … WHERE id = …`，重复执行零变化。
-- ============================================================================

DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.biz_sales_rebate_policy') IS NOT NULL THEN
        SELECT count(*) INTO n FROM biz_sales_rebate_policy WHERE deleted_at IS NULL;
        IF n <> 0 THEN
            RAISE EXCEPTION 'V0438 down 被拒绝：还有 % 条未删的返利政策（先软删再回滚，否则「当时按什么比率算」在系统里再无痕迹）', n;
        END IF;
    END IF;
END $$;

-- 菜单授权行（本迁移不写授权，但运维可能后来授过；外键指向 sys_menu，必须先删）
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000004380'::uuid,
                   '20000000-0000-0000-0000-000000004381'::uuid,
                   '20000000-0000-0000-0000-000000004382'::uuid,
                   '20000000-0000-0000-0000-000000004383'::uuid);

-- 菜单节点：先按钮后页面（父子关系）
DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000004381'::uuid,
              '20000000-0000-0000-0000-000000004382'::uuid,
              '20000000-0000-0000-0000-000000004383'::uuid);
DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000004380'::uuid;

-- 号段
DELETE FROM biz_seq WHERE seq_type = 'BR' AND id = '20000000-0000-0000-0000-000000004388'::uuid;

-- 两张表（规则先删，虽然 CASCADE 也会带走）
DROP TABLE IF EXISTS biz_sales_rebate_rule;
DROP TABLE IF EXISTS biz_sales_rebate_policy;
