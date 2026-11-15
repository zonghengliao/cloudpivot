-- ============================================================================
-- V0402__sales_clerk_receivable_page（闸⑦ 孤儿按钮收口 · t_c49010bf）
--
-- 【问题】`scripts/verify/orphan_button_gate.py` 在**全量迁移库**上报 2 对真孤儿：
--
--     · SALES_CLERK  business:erp:statement:view    父页 /business/erp/receivable
--     · SALES_CLERK  business:erp:statement:export  父页 /business/erp/receivable
--
--   根因：V0317（SO-43 客户对账单）把 5 个对账单按钮挂在「应收账款」页面
--   （…2880，`auth_code = business:erp:receivable:list`）下，并给 SALES_CLERK 授了
--   `statement:view` / `statement:export` 两个**按钮**码 —— 但**没授父页面码**。
--   角色的菜单树按「授了什么行」渲染，父页不可见 ⇒ 这两个按钮永远点不到：
--   功能等于没上线（接口能调，界面没入口），而且不报错。
--
-- # 修法：① 补授父页面码（不是 ② 改 parent_id 挪按钮）
--
--   两条路在技术上都能让闸变绿，选 ① 的理由：
--     · 对账单**本来就住在应收账款页里**（V0317 的设计：不新开页面，「应收台账」页出单）。
--       改 `sys_menu.parent_id` 把按钮挪到别的页，会让「对账单」脱离它所属的台账页 ——
--       按钮在界面上出现在不相干的位置，属于用结构迁就授权；
--     · 权限语义上，销售员看自己客户的应收/对账是**业务需要**（催款要出对账单），
--       「看得见应收台账」不是越权：`receivable:detail` / `aging` / `create` / `write-off`
--       四个按钮码仍然**不授**（看得到列表 ≠ 能改账、能核销）；
--     · ② 的先例 V0259 修的是「按钮当初挂在临时页、正式页后来才建」的历史错位；
--       本处父页与按钮是同一张卡（V0317）一起种的，不存在错位，补授权才是对准根因。
--
-- # 口径（与 V0259 / V0400 同款）
--   · 角色与菜单**按业务键现查**（`role_code='SALES_CLERK'` / `auth_code=…`），
--     不写死 UUID —— 客户环境里 id 可能不同，写死会静默 0 行；
--   · `NOT EXISTS` 判重 ⇒ 幂等（重跑 0 行）；
--   · 只加**一行**授权，不动 `sys_menu` 任何行（零结构变更，前端/缓存无需配合）；
--   · 末尾自检：该角色必须能到达对账单按钮（父页面码在授权里），否则迁移失败 ——
--     宁可起不来，也不要「迁移报成功、闸还是红」。
--
-- # 号段（开工实测复核）
--   写入本文件时 `backend/migrations/business/` 最大号 = V0401
--   （V0401__hr_page_code_split 由并行的 HR 卡在同一工作区创建），本卡顺延取 **V0402**。
--
-- # 回滚
--   `V0402__sales_clerk_receivable_page.down.sql` 只删这一对
--   （SALES_CLERK × receivable:list），删完闸⑦ 回到原来的红 —— 那是回滚的应有状态。
-- ============================================================================

-- ---------- 前置护栏：角色 / 父页面 / 两个按钮都必须真实存在 ----------
DO $$
DECLARE
    v_role_total  int;
    v_role_alive  int;
    v_page        int;
    v_btn         int;
BEGIN
    SELECT count(*), count(*) FILTER (WHERE deleted_at IS NULL)
      INTO v_role_total, v_role_alive
      FROM sys_role
     WHERE role_code = 'SALES_CLERK';

    IF v_role_total = 0 THEN
        -- 角色链断掉说明迁移链不完整（V0267 必种此角色），不能静默放过
        RAISE EXCEPTION 'V0402 拒绝执行：找不到 SALES_CLERK 角色（V0267 应当已种）';
    END IF;
    IF v_role_total > 1 THEN
        RAISE EXCEPTION 'V0402 拒绝执行：SALES_CLERK 有 % 行（role_code 无唯一约束，先人工收口）',
            v_role_total;
    END IF;

    SELECT count(*) INTO v_page
      FROM sys_menu
     WHERE auth_code = 'business:erp:receivable:list'
       AND menu_type = 2
       AND deleted_at IS NULL;
    IF v_page <> 1 THEN
        RAISE EXCEPTION 'V0402 拒绝执行：父页面 business:erp:receivable:list 必须恰好 1 行（实际 %）',
            v_page;
    END IF;

    SELECT count(*) INTO v_btn
      FROM sys_menu
     WHERE auth_code IN ('business:erp:statement:view', 'business:erp:statement:export')
       AND menu_type = 3
       AND deleted_at IS NULL;
    IF v_btn <> 2 THEN
        RAISE EXCEPTION 'V0402 拒绝执行：对账单两个按钮码必须各 1 行（实际 % 行）', v_btn;
    END IF;

    IF v_role_alive = 0 THEN
        RAISE NOTICE 'V0402：SALES_CLERK 已被软删（角色下线）→ 不补授权，闸⑦ 对该角色无意义';
    ELSE
        RAISE NOTICE 'V0402：护栏通过（角色 1 行 / 父页 1 行 / 按钮 2 行）';
    END IF;
END $$;

-- ---------- 授权：SALES_CLERK → business:erp:receivable:list（只一行） ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  JOIN sys_menu m
    ON m.auth_code = 'business:erp:receivable:list'
   AND m.menu_type = 2
   AND m.deleted_at IS NULL
 WHERE r.role_code = 'SALES_CLERK'
   AND r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1
             FROM sys_role_menu rm
            WHERE rm.role_id = r.id
              AND rm.menu_id = m.id
       );

-- ---------- 自检：授权的行数必须恰为 1（幂等轮次为 0），孤儿必须消失 ----------
DO $$
DECLARE
    v_grant   int;
    v_orphan  int;
BEGIN
    SELECT count(*) INTO v_grant
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id
      JOIN sys_menu m ON m.id = rm.menu_id
     WHERE r.role_code = 'SALES_CLERK'
       AND r.deleted_at IS NULL
       AND m.auth_code = 'business:erp:receivable:list'
       AND m.deleted_at IS NULL;

    -- 角色软删时本来就不该有授权（护栏分支已 NOTICE 跳过）
    IF v_grant <> 1 THEN
        SELECT count(*) INTO v_orphan
          FROM sys_role r
         WHERE r.role_code = 'SALES_CLERK' AND r.deleted_at IS NULL;
        IF v_orphan = 1 THEN
            RAISE EXCEPTION 'V0402 自检失败：SALES_CLERK 的 receivable:list 授权应为 1 行（实际 %）',
                v_grant;
        END IF;
    END IF;

    -- 反向诊断：该角色**不应**因本迁移拿到应收的写按钮（看列表 ≠ 能改账）。
    -- 这里只 NOTICE 不 EXCEPTION —— 本迁移只 INSERT 一行页面码，不可能凭空给出写权限；
    -- 若客户环境里有人手工授过，迁移不该因此拦启动（拦了就是「修按钮的迁移把库起不来」）。
    SELECT count(*) INTO v_orphan
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id
      JOIN sys_menu m ON m.id = rm.menu_id
     WHERE r.role_code = 'SALES_CLERK'
       AND r.deleted_at IS NULL
       AND m.auth_code IN (
               'business:erp:receivable:create',
               'business:erp:receivable:write-off',
               'business:erp:receivable:detail',
               'business:erp:receivable:aging'
           )
       AND m.deleted_at IS NULL;
    IF v_orphan <> 0 THEN
        RAISE NOTICE 'V0402 诊断：SALES_CLERK 另持有 % 个应收写/明细按钮码（不是本迁移给的，请人工核对授权矩阵）',
            v_orphan;
    END IF;

    RAISE NOTICE 'V0402：授权已就位（SALES_CLERK × receivable:list = 1 行），对账单按钮不再孤儿';
END $$;
