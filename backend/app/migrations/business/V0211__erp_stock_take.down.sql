-- V0211 down：撤销盘点单（ARCH-3 DC-14）。
--
-- ⚠️ 回滚前请先确认业务侧不再依赖盘点单：
--   · 回滚本迁移**不会**把代码改回「手改库存」——那是二进制的事；正常回滚要连同二进制一起。
--   · 已过账（POSTED）的盘点单**不会**自动回滚三本账：down 只删结构。若库里仍有
--     POSTED 盘点单，本脚本会先终止在跑的审批并把单据置 CANCELLED 之外的状态**不做**
--     —— 这正是下面那条 EXISTS 守卫要拦的事：有已过账盘点单时要求先作废（走业务回滚+红冲），
--     再回滚迁移。**不允许**用 down 把一个动过账的盘点单悄悄抹掉。
--
-- 顺序：在跑的审批实例（终态化）→ 删流程五层（assignee/edge/node/version/def）
--       → 删权限 → 删取号 → 删明细 → 删单头 → 零残留自检。

DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n FROM biz_stock_take WHERE status = 'POSTED' AND deleted_at IS NULL;
    IF n > 0 THEN
        RAISE EXCEPTION 'V0211 down 被拒绝：还有 % 张**已过账**盘点单。请先在业务上逐张作废（会回滚三本账并红冲凭证），再回滚本迁移', n;
    END IF;
END $$;

-- ---------- ⓪ 审批实例守卫：有留档实例就不许回滚（审批留痕不能悄悄删） ----------
DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n FROM biz_flow_instance
     WHERE version_id = '20000000-0000-4000-8000-0000000c2111'::uuid
        OR (biz_type = 'STOCK_TAKE_POST' AND def_id = '20000000-0000-4000-8000-0000000c2110'::uuid);
    IF n > 0 THEN
        RAISE EXCEPTION 'V0211 down 被拒绝：已有 % 条盘点审批实例（审批留档不能删）。'
                        '请先导出 / 归档这些实例（或按运维手册清理审批留痕）后再回滚', n;
    END IF;
END $$;

-- ---------- ① 在跑的盘点审批实例：终止（先待办、后节点、再主表） ----------
UPDATE biz_flow_task t
   SET status = 'CANCELLED',
       comment = COALESCE(t.comment, 'V0211 down：盘点单迁移回滚，审批实例终止'),
       completed_at = now()
  FROM biz_flow_instance i
 WHERE t.instance_id = i.id AND t.status = 'PENDING'
   AND i.status = 'PENDING' AND i.deleted_at IS NULL
   AND i.biz_type = 'STOCK_TAKE_POST';

UPDATE biz_flow_instance_node n
   SET status = 'CANCELLED', ended_at = now(),
       duration_ms = (EXTRACT(EPOCH FROM (now() - COALESCE(n.activated_at, now()))) * 1000)::bigint,
       updated_at = now()
  FROM biz_flow_instance i
 WHERE n.instance_id = i.id AND n.status IN ('ACTIVE', 'PENDING')
   AND i.status = 'PENDING' AND i.deleted_at IS NULL
   AND i.biz_type = 'STOCK_TAKE_POST';

UPDATE biz_flow_instance
   SET status = 'CANCELLED', ended_at = now(),
       result_comment = COALESCE(result_comment, 'V0211 down：盘点单迁移回滚'),
       revision = revision + 1, updated_at = now()
 WHERE status = 'PENDING' AND deleted_at IS NULL AND biz_type = 'STOCK_TAKE_POST';

-- ---------- ② 删流程五层（顺序：审批人 → 边 → 节点 → 版本 → 定义） ----------
DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (SELECT id FROM biz_flow_node
                    WHERE version_id = '20000000-0000-4000-8000-0000000c2111'::uuid);
DELETE FROM biz_flow_edge
 WHERE version_id = '20000000-0000-4000-8000-0000000c2111'::uuid;
DELETE FROM biz_flow_node
 WHERE version_id = '20000000-0000-4000-8000-0000000c2111'::uuid;
DELETE FROM biz_flow_def_version
 WHERE id = '20000000-0000-4000-8000-0000000c2111'::uuid;
DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-0000000c2110'::uuid;

-- ---------- ③ 权限 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001111'::uuid,
                   '20000000-0000-0000-0000-000000001112'::uuid,
                   '20000000-0000-0000-0000-000000001113'::uuid,
                   '20000000-0000-0000-0000-000000001114'::uuid,
                   '20000000-0000-0000-0000-000000001115'::uuid,
                   '20000000-0000-0000-0000-000000001116'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001111'::uuid,
              '20000000-0000-0000-0000-000000001112'::uuid,
              '20000000-0000-0000-0000-000000001113'::uuid,
              '20000000-0000-0000-0000-000000001114'::uuid,
              '20000000-0000-0000-0000-000000001115'::uuid,
              '20000000-0000-0000-0000-000000001116'::uuid);

-- ---------- ④ 取号规则 ----------
DELETE FROM biz_seq WHERE seq_type = 'STOCK_TAKE';

-- ---------- ⑤ 两张表（明细先删，单头后删） ----------
DROP TABLE IF EXISTS biz_stock_take_item;
DROP TABLE IF EXISTS biz_stock_take;

-- ---------- 残留自检（down 后必须是 0） ----------
DO $$
DECLARE
    n_tbl  integer;
    n_menu integer;
    n_seq  integer;
    n_def  integer;
BEGIN
    SELECT count(*) INTO n_tbl FROM information_schema.tables
     WHERE table_schema = 'public' AND table_name IN ('biz_stock_take', 'biz_stock_take_item');
    SELECT count(*) INTO n_menu FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001111'::uuid,
                  '20000000-0000-0000-0000-000000001112'::uuid,
                  '20000000-0000-0000-0000-000000001113'::uuid,
                  '20000000-0000-0000-0000-000000001114'::uuid,
                  '20000000-0000-0000-0000-000000001115'::uuid,
                  '20000000-0000-0000-0000-000000001116'::uuid);
    SELECT count(*) INTO n_seq FROM biz_seq WHERE seq_type = 'STOCK_TAKE';
    SELECT count(*) INTO n_def FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c2110'::uuid;
    IF n_tbl <> 0 OR n_menu <> 0 OR n_seq <> 0 OR n_def <> 0 THEN
        RAISE EXCEPTION 'V0211 down 残留：表 % / 菜单 % / 取号 % / 流程定义 %（应全为 0）',
            n_tbl, n_menu, n_seq, n_def;
    END IF;
    RAISE NOTICE 'V0211 down 完成：两表 / 6 个权限节点 / 1 条取号规则 / 1 个审批流程全部撤销，零残留';
END $$;
