-- ======================================================
-- V0336：审批降级 ⑤ —— 出差/休假时的审批委托（P3-04 / ARCH-8 §1 场景 E）
--
-- ── 迁移号核对（卡面 D4 写的是 V0335）──────────────────
--   动工时 `ls backend/migrations/*/V*.up.sql | sort | tail -3` 的结果是
--   0334 / 0335 / 0336 —— **V0335 已被并行卡片（MENU-FIX-1 t_95bc00b0
--   `V0335__menu_title_shorten_and_sort`，提交 0872b6a）占用**，
--   所以本卡（P3-04）落 **V0336**。两个不同名的 V0335 会让启动直接报迁移冲突。
--
-- ── 为什么需要这张表 ─────────────────────────────────
--   P3-02/03 解决的是「审批人被自审剔光 / 角色没人」的**结构性**无人；
--   本卡解决的是**临时性**无人：审批人 A 出差 3 天，A 名下的待办会一直堵到 A 回来。
--   委托（delegate）让 A 把「这段时间的审批权」交给 B，B 以「代 A 审批」的身份签。
--
-- ── 不加第二套「代办」概念（卡面决策）────────────────
--   **不新建待办表**：委托只在 `assignee.rs` 的解析期把「派给 A」替换成「派给 B」，
--   落库仍是 `biz_flow_task` 一行 —— 只是
--     `assigned_by_rule = 'DELEGATED'`（V0334 已建该列 + 值域 CHECK，本迁移不再加列）、
--     `comment = '代 A 审批（委托至 YYYY-MM-DD）'`、
--     `assignee_source` 保留**原来源**（`ROLE` / `DEPT_LEADER` / …；
--     审计要能回答「这一票本来该谁审、是按什么规则派下来的」）。
--
-- ── 委托不需要审批（ARCH-8 §8 待确认问题 4 的建议口径）──
--   委托是「本人对自己审批权的临时让渡」，再加一道审批只会把「A 出差」变成「A + 审批人一起出差」。
--   留痕要求：本卡先落在 `biz_flow_task.comment`（逐条待办）；
--   配置侧变更的审计由 P3-05 统一接 `operation_logs`。
--
-- ── 同委托人区间重叠必须拒 ───────────────────────────
--   重叠会让「谁代谁」不确定（A 同时委托给 B 和 C 时，引擎只能随机挑一个 → 两个人互相等）。
--   两道闸：
--     ① 应用层：`delegate::validate_window` 在保存前查同委托人未删区间（不管 enabled，
--        停用的也算 —— 重新启用就会重叠），冲突 → 422 + 65043；
--     ② 库层：`EXCLUDE USING gist`（需要 `btree_gist` 扩展，PG13+ 起是 trusted extension，
--        库属主即可 CREATE）。
--   **btree_gist 不可用时**（极老的库 / 有人手工 DROP 过扩展）本迁移退化成
--   「只有应用层校验」，此时存在**并发窗口**：两个请求同时通过 ① 的 SELECT 检查后再插入，
--   会落下重叠行。窗口宽度 = 两个事务之间的毫秒级，且委托是人工低频操作；
--   真出重复行时引擎按 `ORDER BY start_at DESC, created_at DESC` 取**最后一档**（1 跳、不递归），
--   不会死循环。迁移末尾会 RAISE NOTICE 打印实际走的是哪条路。
--
-- ── 为什么不加外键 ───────────────────────────────────
--   `sys_user` 是软删表。加 FK 会让「受托人被删除」变成插入报错，而这里需要的是
--   运行期一条明确的中文提示（受托人已删/停用 → 不替换，回落到委托人本人）。
--   与 V0333 的 `escalation_role_id`、V0334 的 `fallback_user_id` 同一取舍。
-- ======================================================

-- ---------- 0) 区间排除约束需要的扩展（PG13+ 为 trusted，库属主可建） ----------
DO $$
BEGIN
    BEGIN
        CREATE EXTENSION IF NOT EXISTS btree_gist;
    EXCEPTION WHEN OTHERS THEN
        RAISE NOTICE 'V0336: btree_gist 建不出来（%）→ 区间重叠只靠应用层校验（并发窗口见迁移头注释）', SQLERRM;
    END;
END $$;

-- ---------- 1) 委托配置表 ----------
CREATE TABLE biz_flow_delegate (
    id              uuid         PRIMARY KEY DEFAULT gen_random_uuid(),
    delegator_id    uuid         NOT NULL,          -- 委托人（审批权的让出方）
    delegate_id     uuid         NOT NULL,          -- 受托人（这段时间代他签的人）
    start_at        timestamptz  NOT NULL,
    end_at          timestamptz  NOT NULL,
    scope           varchar(40),                    -- NULL = 全部业务；否则存 biz_type（如 PURCHASE_ORDER）
    reason          varchar(200),
    enabled         boolean      NOT NULL DEFAULT true,
    created_at      timestamptz  NOT NULL DEFAULT now(),
    updated_at      timestamptz  NOT NULL DEFAULT now(),
    deleted_at      timestamptz,
    CONSTRAINT ck_biz_flow_delegate_period CHECK (end_at > start_at),
    CONSTRAINT ck_biz_flow_delegate_self   CHECK (delegator_id <> delegate_id)
);

COMMENT ON TABLE biz_flow_delegate IS
    '审批委托（P3-04 / ARCH-8 第 ⑤ 层）：委托人在这段时间里把自己名下的审批待办让给受托人；分配期替换，不新建待办表';
COMMENT ON COLUMN biz_flow_delegate.delegator_id IS '委托人（谁出差 / 休假）';
COMMENT ON COLUMN biz_flow_delegate.delegate_id IS '受托人（代他审批的人）；不许等于 delegator_id（CHECK）';
COMMENT ON COLUMN biz_flow_delegate.start_at IS '生效起点（含）';
COMMENT ON COLUMN biz_flow_delegate.end_at IS '生效终点（含，引擎按 BETWEEN 判；排除约束按 [start,end) 判，端点相接不算重叠）';
COMMENT ON COLUMN biz_flow_delegate.scope IS
    '生效业务范围：NULL = 全部业务；否则存 biz_type（如 PURCHASE_ORDER / EXPENSE_REIMBURSE），大小写不敏感比对';
COMMENT ON COLUMN biz_flow_delegate.reason IS '事由（出差 / 休假 / 其他），给本人与审计看';
COMMENT ON COLUMN biz_flow_delegate.enabled IS '启用开关：FALSE = 暂停（保留配置与历史，不参与派单）';

-- 生效区间查询的唯一热路径：按委托人找「此刻生效的那一档」
CREATE INDEX idx_biz_flow_delegate_active
    ON biz_flow_delegate (delegator_id, start_at, end_at)
    WHERE deleted_at IS NULL;

-- 受托人反查：排障用「这个人现在在替谁签」（也用于批量停用）
CREATE INDEX idx_biz_flow_delegate_delegate
    ON biz_flow_delegate (delegate_id, start_at, end_at)
    WHERE deleted_at IS NULL;

-- ---------- 2) 库级防重叠（btree_gist 可用时才建） ----------
DO $$
DECLARE
    v_has_gist boolean;
BEGIN
    SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'btree_gist') INTO v_has_gist;
    IF v_has_gist THEN
        ALTER TABLE biz_flow_delegate
            ADD CONSTRAINT ex_biz_flow_delegate_no_overlap
            EXCLUDE USING gist (
                delegator_id WITH =,
                tstzrange(start_at, end_at, '[)') WITH &&
            ) WHERE (deleted_at IS NULL AND enabled);
        RAISE NOTICE 'V0336: 已建 ex_biz_flow_delegate_no_overlap（同一委托人的启用区间不许重叠）';
    ELSE
        RAISE WARNING 'V0336: btree_gist 不可用 ⇒ 未建区间排除约束，重叠只由应用层 65043 拦（存在毫秒级并发窗口）';
    END IF;
END $$;

-- ---------- 3) 菜单：挂在「审批流」目录（…0860）下，与 /flow/def、/flow/start、/flow/task 同级 ----------
-- 号段为什么是 …1240/1241/1242：**实测** `…1221`–`…1223` 已被
-- `business:attachment:upload|list|delete` 占用（V0121 之后的附件权限），
-- 抢占会让本迁移的 `ON CONFLICT (id) DO NOTHING` 静默不插，随后自检直接炸。
-- 动手前一定要 `select id,name from sys_menu where id::text like '20000000-…12%'` 探一次。
-- path / component 与前端 `router/index.ts` 的 `flow/delegate` 逐字一致（n08 真库门禁：
-- menu_type=2 的 path 必须能在前端路由里解析）。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001240'::uuid, '审批委托', 2,
     'business:flow:delegate:list', '/flow/delegate',
     '20000000-0000-0000-0000-000000000860'::uuid, 'business/flow/delegate/index',
     40, 1, 'swap', '审批委托', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001241'::uuid, 'business:flow:delegate:save', 3,
     'business:flow:delegate:save', NULL,
     '20000000-0000-0000-0000-000000001240'::uuid, NULL,
     10, 1, NULL, '新增/修改委托', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001242'::uuid, 'business:flow:delegate:delete', 3,
     'business:flow:delegate:delete', NULL,
     '20000000-0000-0000-0000-000000001240'::uuid, NULL,
     20, 1, NULL, '删除委托', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 4) 授权：谁有 `business:flow:read`（能进审批模块）就跟着拿到三个委托码 ----------
--   为什么给所有人而不是只给管理员：委托是**本人对自己审批权的让渡**，
--   `:save` / `:delete` 的行级范围由 handler 强制（「只能是自己的委托，除非有 business:flow:admin」）。
--   只把码给管理员会让「A 出差前自己配委托」这个核心场景根本走不通。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT rm.role_id, m.id
  FROM sys_role_menu rm
  JOIN sys_menu base ON base.id = rm.menu_id AND base.auth_code = 'business:flow:read'
  CROSS JOIN sys_menu m
 WHERE m.id IN ('20000000-0000-0000-0000-000000001240'::uuid,
                '20000000-0000-0000-0000-000000001241'::uuid,
                '20000000-0000-0000-0000-000000001242'::uuid)
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 5) 自检 ----------
DO $$
DECLARE
    v_cols integer;
    v_idx  integer;
    v_menu integer;
BEGIN
    SELECT count(*) INTO v_cols
      FROM information_schema.columns
     WHERE table_name = 'biz_flow_delegate';
    IF v_cols <> 11 THEN
        RAISE EXCEPTION 'V0336 自检失败：biz_flow_delegate 应有 11 列，实际 %', v_cols;
    END IF;

    SELECT count(*) INTO v_idx
      FROM pg_indexes
     WHERE tablename = 'biz_flow_delegate'
       AND indexname IN ('idx_biz_flow_delegate_active', 'idx_biz_flow_delegate_delegate');
    IF v_idx <> 2 THEN
        RAISE EXCEPTION 'V0336 自检失败：局部索引应有 2 个，实际 %', v_idx;
    END IF;

    SELECT count(*) INTO v_menu
      FROM sys_menu
     WHERE auth_code IN ('business:flow:delegate:list',
                         'business:flow:delegate:save',
                         'business:flow:delegate:delete')
       AND deleted_at IS NULL;
    IF v_menu <> 3 THEN
        RAISE EXCEPTION 'V0336 自检失败：委托三个权限码的承载菜单应有 3 行，实际 %', v_menu;
    END IF;

    -- 页面行的 path/component 必须与前端路由一致（n08 门禁的前置条件）
    IF NOT EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id = '20000000-0000-0000-0000-000000001240'::uuid
           AND path = '/flow/delegate'
           AND component = 'business/flow/delegate/index'
           AND parent_id = '20000000-0000-0000-0000-000000000860'::uuid
    ) THEN
        RAISE EXCEPTION 'V0336 自检失败：委托页面菜单的 path/component/父目录与前端路由不一致';
    END IF;

    RAISE NOTICE 'V0336 自检通过：11 列 / 2 局部索引 / 3 个权限码 / 页面挂载正确';
END $$;
