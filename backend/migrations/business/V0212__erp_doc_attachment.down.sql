-- V0212 down：撤销业务单据附件（ARCH-3 DC-17）。
--
-- ⚠️ 回滚前请先确认业务侧不再依赖附件：
--   · 回滚只删**结构与会话入口**，不动 `sys_file` 与物理对象（文件本体属 storage 模块，
--     它的清理任务是「软删 7 天后回收」，与本次迁移无关）。
--   · 仍有**未软删**的附件绑定行时主动拒绝：那些行是「这张合同的扫描件在这里」的
--     唯一证据，DROP TABLE 会把它悄悄抹掉。要求先把附件逐条删除（业务上留痕）再回滚。
--
-- 顺序：守卫 → 删角色授权 → 删权限节点 → 删表 → 零残留自检。

-- ---------- ⓪ 守卫：未软删的附件绑定不许被 DROP ----------
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.biz_doc_attachment') IS NULL THEN
        RETURN;
    END IF;
    SELECT count(*) INTO n FROM biz_doc_attachment WHERE deleted_at IS NULL;
    IF n > 0 THEN
        RAISE EXCEPTION 'V0212 down 被拒绝：还有 % 条**未删除**的单据附件绑定。'
                        '请先在业务上逐条删除附件（列表里点删除，留审计痕迹）后再回滚本迁移', n;
    END IF;
END $$;

-- ---------- ① 角色授权（必须先删，否则 sys_menu 的外键/孤儿行会留在授权表里） ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001221'::uuid,
                   '20000000-0000-0000-0000-000000001222'::uuid,
                   '20000000-0000-0000-0000-000000001223'::uuid);

-- ---------- ② 权限节点 ----------
DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001221'::uuid,
              '20000000-0000-0000-0000-000000001222'::uuid,
              '20000000-0000-0000-0000-000000001223'::uuid);

-- ---------- ③ 表（索引与约束随表一起走） ----------
DROP TABLE IF EXISTS biz_doc_attachment;

-- ---------- 残留自检（down 后必须是 0） ----------
DO $$
DECLARE
    n_tbl   integer;
    n_menu  integer;
    n_grant integer;
BEGIN
    SELECT count(*) INTO n_tbl FROM information_schema.tables
     WHERE table_schema = 'public' AND table_name = 'biz_doc_attachment';
    SELECT count(*) INTO n_menu FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001221'::uuid,
                  '20000000-0000-0000-0000-000000001222'::uuid,
                  '20000000-0000-0000-0000-000000001223'::uuid);
    SELECT count(*) INTO n_grant FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000001221'::uuid,
                       '20000000-0000-0000-0000-000000001222'::uuid,
                       '20000000-0000-0000-0000-000000001223'::uuid);
    IF n_tbl <> 0 OR n_menu <> 0 OR n_grant <> 0 THEN
        RAISE EXCEPTION 'V0212 down 残留：表 % / 权限节点 % / 授权行 %（应全为 0）',
            n_tbl, n_menu, n_grant;
    END IF;
    RAISE NOTICE 'V0212 down 完成：biz_doc_attachment / 3 个权限节点 / 其角色授权全部撤销，零残留';
END $$;
