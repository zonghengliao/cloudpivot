-- ============================================================================
-- V0319：菜单结构基线迁移化（MENU-01，卡 t_0b14ecac）
--
-- ## 为什么要有这条迁移
--
-- 145 测试环境侧栏「乱」的根因不是种子写错，而是**修正是直接 UPDATE 生产库做的，
-- 从未落进任何迁移**：
--   · /system/data-scope  parent = NULL   → 侧栏顶级孤立一个「数据权限」
--   · /system/field-mask  parent = NULL   → 顶级孤立「字段脱敏」
--   · /system/role-mutex  parent = NULL   → 顶级孤立「角色互斥」
--   · /system/change-log  parent = 20000000-…790（/content 目录）→ 挂在「内容」下
-- 任何新机器跑完全部迁移后拿到的都是这个坏结构，所以必须把「生产库里那套正确
-- 结构」写成迁移。本迁移的语义就是：**把 dev/新库对齐到 prod 基线**。
-- 唯一例外是 ⑥ 的 is_visible：卡面 16:50 修正明确这 4 条是**漏掉的**，
-- 正确值是 true，生产库同样要改（见下）。
--
-- ## 数据来源（2026-11-07 实测，不是推测）
--
--   对 144 `cloudpivot`（prod，被认定正确）与 145 `cloudpivot_dev` 的 sys_menu
--   做了 deleted_at IS NULL 全表 15 列逐行比对，共 409 行/库，差异恰好 13 组
--   （见下）。卡面给的 13 项与实测逐项吻合。
--
-- ## 13 项处理（字段名按真实列名：sort_order / is_visible）
--
--   ① parent → system 目录（10000000-0000-0000-0000-000000000001）
--        /system/data-scope   NULL                        → system
--        /system/field-mask   NULL                        → system
--        /system/role-mutex   NULL                        → system
--        /system/change-log   20000000-…790（/content）   → system
--   ② /help          parent 20000000-…790（/content）→ NULL；sort_order 74 → 78
--        「帮助中心」是顶级页面（与 /dashboard 同为 parent NULL 的 type=2），
--        不该挂在「内容」目录下 —— 按 prod 走。
--   ③ /business      sort_order 70 → 75
--   ④ /flow/def      sort_order 10 → 68
--   ⑤ 名称（name 是唯一标识、title 是侧栏展示名；dev 两列原本相等，必须同改，
--      否则侧栏仍显示旧名 —— MenuRes::from 直接取 m.title，没有 name 兜底）：
--        /business/erp/payable  name/title 应付单   → 应付账款
--        /business/erp/voucher  name/title 凭证查询 → 记账凭证
--   ⑥ is_visible → **true**（卡面「重要修正 2026-11-07 16:50」：这 4 个是二级明细页，
--      用户明确确认**是漏掉的、不是故意隐藏**，前端页面与路由早已全部实现，
--      只是菜单没打开。生产库当前也是 false，同样是漏掉的 ⇒ 生产也要改）：
--        /business/erp/inventory-location  router/index.ts:852  → inventory/index.vue(tab=location)
--        /business/erp/inventory-txn       router/index.ts:858  → inventory/index.vue(tab=txn)
--        /business/erp/purchase-order-item router/index.ts:868  → purchase-order-item/index.vue
--        /business/erp/seq                 router/index.ts:877  → seq/index.vue
--      ⚠ 这是本迁移**唯一会真正改动生产库**的部分（4 行），其余 6 条 UPDATE 在生产上
--      都是 0 行。卡面上半段表格里「同步为 false（以生产为准）」那一行已被修正作废。
--
-- ## 卡面提的「prod 有、dev 缺的 3 条」——实测结论：一条都不需要 INSERT
--
--   全表 path 集合比对：prod 比 dev 只多 `/business/erp/purchase` 一条，dev 比
--   prod 只多 `/business/erp/purchase-order` 一条，其余 path 完全相同。
--   而这俩是**同一条逻辑菜单的两种路径**：
--     frontend/src/router/index.ts:545-546
--       path: 'business/erp/purchase-order',
--       alias: ['business/erp/purchase'],
--   两条都解析到 @/views/business/erp/purchase/index.vue。seed 历史路径是
--   `purchase-order`（V0141 种的那条），prod 现存的 `purchase` 是别名路径。
--   所以：
--     · `/business/erp/payable`、`/business/erp/voucher` —— dev 已有同一 path 的行，
--       只是名字不同 ⇒ 走 ⑤ 的 UPDATE，**不 INSERT**（否则同 path 两条）。
--     · `/business/erp/purchase` —— dev 已有等价的 purchase-order 行 ⇒ 若 INSERT
--       会出现两个「采购订单」菜单（component/auth_code/parent 全同）⇒ **不 INSERT**，
--       否则正是本卡明令禁止的重复插入。
--
-- ## 幂等
--
--   每条 UPDATE 都带 `字段 IS DISTINCT FROM 目标值` 守卫：
--     · 生产（已经是目标值）→ 0 行受影响，updated_at 也不动，零副作用；
--     · 已修过的库重跑 → 0 行；
--     · 从没修过的库 → 只动该动的那几行。
--   本迁移**不 DELETE 任何菜单**，也**不碰 sys_role_menu**（授权不在本卡范围）。
--   末尾 DO 块自检：任一断言不成立就抛异常让迁移失败，不留「看起来跑过了」的假绿。
--
-- ## 明确不在本卡范围（实测存在，但卡面限定只动 parent/name/is_visible/sort_order）
--
--   · icon 差异 5 处（/business、/flow/def、/help、/system/field-mask、
--     /task/list、/business/announcement）：两库的 icon 名在
--     tdesign-icons-vue-next 里**都存在**（已逐个核对 esm/components/*.js），
--     属审美差异不是缺陷 ⇒ 不动。
--   · title 差异 2 处（/flow/def、/business/erp/ledger）：dev 的描述更详细 ⇒ 不动。
--   · hidden_expression：dev 为 ''、prod 为 NULL，语义等价 ⇒ 不动。
-- ============================================================================

-- ---------- ① 4 条 /system/* 页面归位到 system 目录 ----------
-- 一起写便于审计：只修 parent，不碰其它字段。
UPDATE sys_menu
   SET parent_id = '10000000-0000-0000-0000-000000000001'::uuid,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path IN ('/system/data-scope', '/system/field-mask',
                '/system/role-mutex', '/system/change-log')
   AND parent_id IS DISTINCT FROM '10000000-0000-0000-0000-000000000001'::uuid;

-- ---------- ② 帮助中心回顶级（parent NULL）＋排序 78 ----------
UPDATE sys_menu
   SET parent_id = NULL,
       sort_order = 78,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/help'
   AND (parent_id IS NOT NULL OR sort_order IS DISTINCT FROM 78);

-- ---------- ③ 通用底座排序 70 → 75 ----------
UPDATE sys_menu
   SET sort_order = 75,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/business'
   AND sort_order IS DISTINCT FROM 75;

-- ---------- ④ 流程定义排序 10 → 68 ----------
UPDATE sys_menu
   SET sort_order = 68,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/flow/def'
   AND sort_order IS DISTINCT FROM 68;

-- ---------- ⑤ 两个页面改名（name 唯一标识 + title 展示名同步） ----------
UPDATE sys_menu
   SET name = '应付账款',
       title = '应付账款',
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/business/erp/payable'
   AND (name IS DISTINCT FROM '应付账款' OR title IS DISTINCT FROM '应付账款');

UPDATE sys_menu
   SET name = '记账凭证',
       title = '记账凭证',
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/business/erp/voucher'
   AND (name IS DISTINCT FROM '记账凭证' OR title IS DISTINCT FROM '记账凭证');

-- ---------- ⑥ 4 个明细页打开显示（用户确认是漏掉的，不是故意隐藏） ----------
-- 生产库当前是 false、dev 也是 false（dev 被上一个版本的迁移误设为 false 过），
-- 一律打开；这不是「以生产为准」的字段，是按用户确认的正确值来。
UPDATE sys_menu
   SET is_visible = true,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path IN ('/business/erp/inventory-location', '/business/erp/inventory-txn',
                '/business/erp/purchase-order-item', '/business/erp/seq')
   AND is_visible IS DISTINCT FROM true;

-- ---------- ⑦ 自检：结构不符就让迁移失败 ----------
-- 口径：对「库里存在的行」断言其值正确（不要求行必须存在）——
-- 这样全新库（种子较少的机器）不会因为某条菜单尚未种下而假失败，
-- 而任何一台机器只要行在，就必须是 prod 基线值。
DO $$
DECLARE
    sys_dir   CONSTANT uuid := '10000000-0000-0000-0000-000000000001';
    bad       bigint;
BEGIN
    -- ① /system 下的**页面**（menu_type=2）一律挂 system 目录（不放宽到按钮：
    --   按钮的 parent 是其所属页面，见 MENU-02 的断言 ②）
    SELECT count(*) INTO bad
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 2 AND path LIKE '/system/%'
       AND parent_id IS DISTINCT FROM sys_dir;
    IF bad > 0 THEN
        RAISE EXCEPTION 'V0319 自检失败：% 条 /system/* 页面未挂在 system 目录下', bad;
    END IF;

    -- ② 帮助中心必须是顶级
    SELECT count(*) INTO bad
      FROM sys_menu
     WHERE deleted_at IS NULL AND path = '/help' AND parent_id IS NOT NULL;
    IF bad > 0 THEN
        RAISE EXCEPTION 'V0319 自检失败：/help 仍挂在目录下';
    END IF;

    -- ③ 排序基线
    SELECT count(*) INTO bad
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND (   (path = '/business' AND sort_order <> 75)
            OR (path = '/help'     AND sort_order <> 78)
            OR (path = '/flow/def'  AND sort_order <> 68));
    IF bad > 0 THEN
        RAISE EXCEPTION 'V0319 自检失败：% 条菜单排序与基线不符', bad;
    END IF;

    -- ④ 名称基线（name 与 title 必须同为基线值）
    SELECT count(*) INTO bad
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND (   (path = '/business/erp/payable'
                AND (name <> '应付账款' OR title <> '应付账款'))
            OR (path = '/business/erp/voucher'
                AND (name <> '记账凭证' OR title <> '记账凭证')));
    IF bad > 0 THEN
        RAISE EXCEPTION 'V0319 自检失败：% 条菜单名称与基线不符', bad;
    END IF;

    -- ⑤ 可见性基线：4 个明细页必须打开（用户确认它们是漏掉的，不是故意隐藏）
    SELECT count(*) INTO bad
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND path IN ('/business/erp/inventory-location', '/business/erp/inventory-txn',
                    '/business/erp/purchase-order-item', '/business/erp/seq')
       AND is_visible IS DISTINCT FROM true;
    IF bad > 0 THEN
        RAISE EXCEPTION 'V0319 自检失败：% 条明细页仍不可见', bad;
    END IF;

    RAISE NOTICE 'V0319 自检通过：菜单结构已对齐基线（parent / 名称 / 可见性 / 排序）';
END
$$;
