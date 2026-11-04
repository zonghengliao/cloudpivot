-- ============================================================================
-- V0234__flow_template_menu_retire.up.sql
--
-- 【P2-6d】软删 `/flow/templates`（流程模板库）菜单节点 —— 两个入口收敛为 1 个。
--
-- # 依据（已拍板，不要再讨论 A/B）
--
--   docs/architecture/P2-6-模板实体-决策.md §3.3 第 2 行：
--     `sys_menu` 节点 `…07a5`（`/flow/templates`）→ **软删**（`deleted_at`），
--     保留 `…/flow/def`；卡片墙并入 def 页 → 菜单从 2 个入口收敛到 1 个。
--   同理 §4「目标态」：菜单 2 个入口 → 1 个入口（审批流程定义，内含 2 个 Tab）。
--
-- # 为什么现在才能删（顺序性）
--
--   删节点等于撤掉用户的「模板」入口。前端 P2-6c（t_4dcfa14b，提交 3ce44c7）已把
--   卡片墙并进 `/flow/def` 的「标准流程」Tab，且老路由 `/flow/templates` 保留
--   redirect → `/flow/def?view=standard`。**只有在那之后再删节点**，用户才不会掉入口。
--   本迁移假定该前提已上线（父卡依赖 t_4dcfa14b 已 done）。
--
-- # 软删而不是硬删
--
--   `deleted_at` 置位即对应用不可见（`system/menu/model.rs` 的查询一律带
--   `WHERE deleted_at IS NULL`），但行仍在库里：
--     · 回滚一条 SQL（见 down）即可恢复，不需要重放 V0194 的 INSERT；
--     · `sys_role_menu` 的角色绑定不用动 —— 删行才要清理引用，软删不需要；
--     · 审计能查到「这个节点是什么时候下线的」。
--
-- # 不新增权限码（决策 A 的硬约束）
--
--   两个节点原本共用 `business:flow:read`；收口后仍只用它。ARCH-5 §13.2 曾规划的
--   `business:flow:template:read` **随决策 A 取消**，本迁移不种任何权限码。
--
-- # 幂等
--
--   `AND deleted_at IS NULL`：重复执行 = 0 行变更（第二轮的 `UPDATE 0`）。
--   自检是**幂等安全**的：它断言的是「软删后的稳定状态」，不是「本次改了 N 行」，
--   所以同一份文件在已应用过的库上再跑一遍仍然通过（隔离库两轮实测见证据）。
--
-- # 号段
--
--   本卡写作时仓内最大号 = V0233（P2-6b-follow 的 `flow_seed_is_system_fix2`，
--   未提交但在工作区；已应用最大 = V0232）。本卡按「被占则顺延、绝不抢号」取 V0234。
--
-- 回滚：`V0234__flow_template_menu_retire.down.sql`（`deleted_at` 置回 NULL）
-- ============================================================================

UPDATE sys_menu
   SET deleted_at = now()
 WHERE path = '/flow/templates'
   AND deleted_at IS NULL;

DO $$
DECLARE
    live_tpl  int;
    live_def  int;
    orphans   int;
BEGIN
    -- ① 软删后：/flow/templates 不许还有「在用」行
    SELECT count(*) INTO live_tpl
      FROM sys_menu
     WHERE path = '/flow/templates'
       AND deleted_at IS NULL;
    IF live_tpl <> 0 THEN
        RAISE EXCEPTION 'V0234 自检失败：软删后仍有 % 个在用的 /flow/templates 菜单节点（应为 0）', live_tpl;
    END IF;

    -- ② 唯一入口必须还在：/flow/def 有且只有 1 个在用行
    --    （= 0 说明删错了/库是空的，= 1 以上说明有重复节点，都算失败）
    SELECT count(*) INTO live_def
      FROM sys_menu
     WHERE path = '/flow/def'
       AND deleted_at IS NULL;
    IF live_def <> 1 THEN
        RAISE EXCEPTION 'V0234 自检失败：/flow/def（审批流程定义）在用节点数 = %（必须为 1）', live_def;
    END IF;

    -- ③ 软删不许留孤儿子节点（本卡前提：…07a5 是顶层节点，实测子节点 = 0；
    --    万一将来有人给它挂了子节点，删父不删子会让子节点变成无入口的孤儿）
    SELECT count(*) INTO orphans
      FROM sys_menu c
      JOIN sys_menu p ON p.id = c.parent_id
     WHERE p.path = '/flow/templates'
       AND c.deleted_at IS NULL;
    IF orphans <> 0 THEN
        RAISE EXCEPTION 'V0234 自检失败：/flow/templates 下仍有 % 个在用子节点（会变成孤儿入口）', orphans;
    END IF;
END $$;
