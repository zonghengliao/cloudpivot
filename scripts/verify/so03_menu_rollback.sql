-- SO-03 回滚演练：在事务里跑 V0263 / V0262 的 down，断言回到 V0261 的状态，然后 ROLLBACK（不留痕）
\set ON_ERROR_STOP on

BEGIN;

SELECT name, title, path, component, is_visible
  FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000002601'::uuid \gset before_

\echo '--- 回滚前 ---'
SELECT name, title, coalesce(path,'(NULL)') AS path, coalesce(component,'(NULL)') AS component, is_visible
  FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000002601'::uuid;

\i /opt/cloudpivot/backend/migrations/business/V0263__sales_customer_menu_label.down.sql
\i /opt/cloudpivot/backend/migrations/business/V0262__sales_customer_menu_page.down.sql

\echo '--- 两个 down 之后（应为 path=NULL / component=NULL / is_visible=f / title=长文案）---'
SELECT name, title, coalesce(path,'(NULL)') AS path, coalesce(component,'(NULL)') AS component, is_visible
  FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000002601'::uuid;

DO $$
DECLARE
    v_path text;
    v_comp text;
    v_vis  boolean;
BEGIN
    SELECT path, component, is_visible INTO v_path, v_comp, v_vis
      FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000002601'::uuid;
    IF v_path IS NOT NULL OR v_comp IS NOT NULL OR v_vis IS NOT FALSE THEN
        RAISE EXCEPTION 'down 断言失败：path=% component=% is_visible=%', v_path, v_comp, v_vis;
    END IF;
    RAISE NOTICE 'down 断言通过：页面已关闭（path/component=NULL，is_visible=FALSE），按钮与权限码保留';
END $$;

\echo '--- 再跑一遍 up（幂等 + 前置校验）---'
\i /opt/cloudpivot/backend/migrations/business/V0262__sales_customer_menu_page.up.sql
\i /opt/cloudpivot/backend/migrations/business/V0263__sales_customer_menu_label.up.sql

SELECT name, title, path, component, is_visible
  FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000002601'::uuid;

\echo '--- 3 个按钮仍在页面下 ---'
SELECT id, name, parent_id IS NOT NULL AS has_parent
  FROM sys_menu WHERE parent_id = '20000000-0000-0000-0000-000000002601'::uuid AND deleted_at IS NULL
 ORDER BY sort_order;

ROLLBACK;

\echo '--- ROLLBACK 之后（应与回滚前一致，证明演练没留痕）---'
SELECT name, title, path, component, is_visible
  FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000002601'::uuid;
