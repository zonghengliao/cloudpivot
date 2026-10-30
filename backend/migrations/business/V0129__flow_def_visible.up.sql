-- V0129：把「流程定义」菜单设为可见
--
-- 背景：V0121（D1）种「流程定义」菜单时设了 `is_visible = FALSE`
--       —— 当时 D1 只交付后端，前端页面还没有，避免侧边栏出现打不开的链接。
--       D5 交付了页面、V0128 把它挂到了「审批流」目录下，
--       但**都没把这个开关打开**，所以老大在侧边栏只看到「发起审批」「审批中心」，
--       「流程定义」一直不显示（URL 直接访问 /flow/def 是能打开的）。
--
-- 老大 2026-10-30 实测：审批流目录下缺「流程定义」。

UPDATE sys_menu
   SET is_visible = TRUE,
       updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000780'
   AND is_visible = FALSE;

-- 自检：流程定义必须可见
DO $$
DECLARE
    v boolean;
BEGIN
    SELECT is_visible INTO v
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000780';
    IF v IS DISTINCT FROM TRUE THEN
        RAISE EXCEPTION 'V0129 自检失败：流程定义菜单 is_visible 应为 true，实际 %', v;
    END IF;
END $$;
