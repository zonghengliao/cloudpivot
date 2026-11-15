-- ============================================================================
-- V0368：NEAR-FIX-1（卡 t_67dcb9cb）—— 两个「path 为空」的页面菜单落地 + 附件类型 16 → 17
--
-- # 这条迁移解决什么（评审 5.1 / 5.6 的原文：前端有 2 个死菜单，path 为空、无页面、无路由）
--
--   ① 收款退款单（…2950，V0295 建、auth_code = business:erp:receipt-refund:list）
--      后端 API 早就完整（V0294 建表 + E2E-02 验过「退款 → 应收恢复 → 红字凭证」），
--      但 V0295 头注释写明「path/component 故意留 NULL、is_visible = FALSE，
--      前端页面由后续前端卡落地后用一条 UPDATE 补」—— 那张前端卡从来没来，
--      于是侧栏里点不到、深链也进不去。
--   ② 价格查询（…3071，V0307 建、auth_code = business:erp:price:resolve）
--      同上口径，V0307 头注释连补全语句都写好了（原文第 31-36 行）。
--
--   本迁移只做「库侧收尾」：把两行的 path / component / is_visible / parent_id /
--   sort_order 一次补成终态。页面本体在前端仓库里同步落地（同一次提交）：
--     frontend/src/views/business/erp/receipt-refund/index.vue
--     frontend/src/views/business/erp/price/index.vue
--     frontend/src/router/index.ts 的两条路由注册
--   ⇒ `system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router` 是
--     「库 × 前端路由」的联合校验，先填 path 后落路由会让它红，所以两者必须同提交。
--
-- # 为什么「价格查询」是补页面而不是删菜单（判断依据，卡面要求写清）
--
--   · 这一行是 `business:erp:price:resolve` 的**唯一**承载行（V0307 自己的自检断言
--     该 auth_code 恰好 1 行）。`scripts/lint/perm_menu_gate.py` 要求每个
--     `#[nik::permission]` 码都有承载行 —— 删掉这行，门槛变红，而且除 SUPER_ADMIN
--     外**任何人都调不动** `GET /business/erp/price/resolve`（不报错、不打日志，就是 403）；
--   · 后端接口完整可用（SO-33：5 级确定性取价，客户协议价 → 客户专属阶梯 → 通用阶梯
--     → 物料标准售价 → 无价），业务上是销售报价的真实诉求，不是「早期规划没做」；
--   · V0307 头注释第 22-23 行原文：「将来价格维护页落地时可以把它改成那个页面的父节点」
--     ⇒ 设计意图就是「等页面来」，而不是「删掉」。
--   ⇒ 选 B（补页面），并把 path 填成 `/business/erp/price`（V0307 头注释里逐字给出的值）。
--
-- # 为什么 parent_id 要挂到「销售管理」（…2701）
--
--   ① 两行原本都是**一级菜单**（parent_id = NULL）。挂到销售管理下才符合语义：
--      收款退款单紧跟收款单（sort 50 → 55）；取价查询紧跟客户价格（sort 70 → 75）。
--   ② `scripts/verify/menu_invariants.sh` 的断言 ③（/business 归属）要求
--      `/business/%` 的节点上溯到的根落在 /business 命名空间里。一级菜单自身就是根，
--      root_path 会是 `/business/erp/receipt-refund` —— 虽然形如 `/business/...` 能过闸，
--      但侧栏会多出两个莫名其妙的顶级条目；挂到销售管理下才是产品要的样子。
--   ③ 断言 ⑦（同父 sort 唯一，只管 menu_type=2）：销售管理现有 10/20/30/40/50/60/
--      70/72/78/80/82/90/92，**55 与 75 是空位**（插空用中间值，不是末尾追加 ——
--      末尾追加会让「收款退款单」跑到汇率管理后面，与收款单隔了 7 项）。
--
-- # 附件类型 16 → 17（+ RECEIPT_REFUND）：为什么本卡必须一起放开
--
--   卡面要求退款单详情里复用 `DocAttachmentPanel` 挂「退款凭据」。而
--   `ck_biz_doc_attachment_type`（V0350 建，16 值）里**没有** RECEIPT_REFUND ——
--   不收宽约束，上传会直接撞 23514 变成 500（ATT-04 / ATT-06 / ATT-11 三次踩过
--   同一个坑：**界面词表 ≠ 数据库事实**）。同理放开规则表的
--   `ck_biz_attachment_rule_doc_type`（附件规则管理界面的下拉来自 DocKind::ALL，
--   「能选但存不进去」是最糟的一种漂移）。
--   Rust 侧同步：`shared/doc_kind.rs` 的 `ATTACHMENT_DOC_KINDS` 16 → 17；
--   巡检登记表 `docs/architecture/snapshot-drift/10_checks.sql` 的
--   `ATTACHMENT_DOC.RECEIPT_REFUND` 一行 + 悬空分支；前端 `doc-kinds.ts` 的
--   `ATTACHMENT_DOC_TYPES`。四处不一致会被 `doc_kind_tests` 的
--   `attachment_doc_types_match_rust` / `doc_kind_registries_match_doc_kind_table` 判红。
--
-- # 幂等（重放必须 0 行更新、不发生第二次 ALTER）
--
--   · 菜单四列全部带 `IS DISTINCT FROM` 守卫；
--   · 两个约束先读 `pg_get_constraintdef`：**已含 RECEIPT_REFUND 就跳过**
--     （不 DROP 再 ADD，避免每次重放全表校验）；
--   · `COMMENT ON COLUMN` 天然幂等；文件内**不写** BEGIN/COMMIT（迁移器自己在事务里跑）。
--
-- # 回滚
--
--   `V0368__receipt_refund_page_and_price_page.down.sql`：两行菜单还原成
--   V0295 / V0307 的 NULL / NULL / FALSE / 一级菜单 / 原 sort，两个约束收回 16 值。
--   回滚前先断言「两张单据上还没有 RECEIPT_REFUND 附件」，有就 RAISE
--   （不静默删数据、也不留下靠 23514 才发现的坏行）。
-- ============================================================================

-- ============================================================
-- §1 收款退款单页（…2950）：补 path / component / 可见性 / 父级 / 排序
-- ============================================================
DO $$
DECLARE
    v_page      uuid := '20000000-0000-0000-0000-000000002950'::uuid;
    v_parent    uuid := '20000000-0000-0000-0000-000000002701'::uuid;  -- 销售管理（/business/erp/sales-mgmt）
    v_path      text := '/business/erp/receipt-refund';
    v_component text := 'business/erp/receipt-refund/index';
    v_title     text := '收款退款单';
    v_sort      int  := 55;   -- 紧跟收款单（50），在销售发票（60）之前
    v_type      int;
    v_code      text;
    v_children  int;
    n_updated   int := 0;
    n_bad       int;
BEGIN
    -- ---------- 0) 前置：页面节点存在、是页面、权限码未被换掉、父级目录在 ----------
    SELECT menu_type, auth_code INTO v_type, v_code
      FROM sys_menu
     WHERE id = v_page AND deleted_at IS NULL;

    IF v_type IS NULL THEN
        RAISE EXCEPTION 'V0368 前置失败：收款退款单页面节点 …2950 不存在或已软删（V0295 没跑或已被回滚）';
    END IF;
    IF v_type <> 2 THEN
        RAISE EXCEPTION 'V0368 前置失败：…2950 的 menu_type = %（应为 2 页面节点）', v_type;
    END IF;
    IF v_code IS DISTINCT FROM 'business:erp:receipt-refund:list' THEN
        RAISE EXCEPTION 'V0368 前置失败：…2950 的 auth_code = %（应为 business:erp:receipt-refund:list）', v_code;
    END IF;

    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE id = v_parent AND deleted_at IS NULL AND menu_type = 1;
    IF n_bad <> 1 THEN
        RAISE EXCEPTION 'V0368 前置失败：父级目录「销售管理」…2701 不存在 / 已软删 / 不是目录节点';
    END IF;

    -- ---------- 1) 五列一次补成终态（幂等） ----------
    UPDATE sys_menu
       SET path       = v_path,
           component  = v_component,
           is_visible = TRUE,
           parent_id  = v_parent,
           sort_order = v_sort,
           title      = v_title,
           updated_at = now()
     WHERE id = v_page
       AND (path       IS DISTINCT FROM v_path
         OR component  IS DISTINCT FROM v_component
         OR is_visible IS DISTINCT FROM TRUE
         OR parent_id  IS DISTINCT FROM v_parent
         OR sort_order IS DISTINCT FROM v_sort
         OR title      IS DISTINCT FROM v_title);
    GET DIAGNOSTICS n_updated = ROW_COUNT;

    -- ---------- 2) 施工后断言 ----------
    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL
       AND (path       IS DISTINCT FROM v_path
         OR component  IS DISTINCT FROM v_component
         OR is_visible IS DISTINCT FROM TRUE
         OR parent_id  IS DISTINCT FROM v_parent
         OR sort_order IS DISTINCT FROM v_sort);
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0368 断言 A 失败：收款退款单页的 path/component/is_visible/parent_id/sort_order 未补成终态';
    END IF;

    -- 2.2 结构不能在这条 UPDATE 里丢：4 个按钮仍挂在本页下（V0295 的成果必须还在）
    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = v_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_children <> 4 THEN
        RAISE EXCEPTION 'V0368 断言 B 失败：收款退款单页下应有 4 个按钮，实际 % 个（父级关系被破坏）', v_children;
    END IF;

    -- 2.3 同父 sort 唯一（menu_invariants.sh 断言 ⑦ 的库内预检，早失败早发现）
    SELECT count(*) INTO n_bad
      FROM (SELECT sort_order FROM sys_menu
             WHERE deleted_at IS NULL AND menu_type = 2 AND parent_id = v_parent
             GROUP BY sort_order HAVING count(*) > 1) t;
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0368 断言 C 失败：销售管理目录下 menu_type=2 的 sort_order 有 % 组重复', n_bad;
    END IF;

    RAISE NOTICE 'V0368 §1 完成：收款退款单页已开（path=% component=% parent=销售管理 sort=%），本次改 % 行；4 个按钮父级完好',
        v_path, v_component, v_sort, n_updated;
END $$;

-- ============================================================
-- §2 价格查询页（…3071）：同上（取价解析的权限承载行 + 真正的取价查询页）
-- ============================================================
DO $$
DECLARE
    v_page      uuid := '20000000-0000-0000-0000-000000003071'::uuid;
    v_parent    uuid := '20000000-0000-0000-0000-000000002701'::uuid;  -- 销售管理
    v_path      text := '/business/erp/price';
    v_component text := 'business/erp/price/index';
    v_title     text := '取价查询';
    v_sort      int  := 75;   -- 紧跟客户价格（70），在销售目标（78）之前
    v_type      int;
    v_code      text;
    n_updated   int := 0;
    n_bad       int;
BEGIN
    SELECT menu_type, auth_code INTO v_type, v_code
      FROM sys_menu
     WHERE id = v_page AND deleted_at IS NULL;

    IF v_type IS NULL THEN
        RAISE EXCEPTION 'V0368 前置失败：价格查询页面节点 …3071 不存在或已软删（V0307 没跑或已被回滚）';
    END IF;
    IF v_type <> 2 THEN
        RAISE EXCEPTION 'V0368 前置失败：…3071 的 menu_type = %（应为 2 页面节点）', v_type;
    END IF;
    IF v_code IS DISTINCT FROM 'business:erp:price:resolve' THEN
        RAISE EXCEPTION 'V0368 前置失败：…3071 的 auth_code = %（应为 business:erp:price:resolve）', v_code;
    END IF;

    UPDATE sys_menu
       SET path       = v_path,
           component  = v_component,
           is_visible = TRUE,
           parent_id  = v_parent,
           sort_order = v_sort,
           title      = v_title,
           updated_at = now()
     WHERE id = v_page
       AND (path       IS DISTINCT FROM v_path
         OR component  IS DISTINCT FROM v_component
         OR is_visible IS DISTINCT FROM TRUE
         OR parent_id  IS DISTINCT FROM v_parent
         OR sort_order IS DISTINCT FROM v_sort
         OR title      IS DISTINCT FROM v_title);
    GET DIAGNOSTICS n_updated = ROW_COUNT;

    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL
       AND (path       IS DISTINCT FROM v_path
         OR component  IS DISTINCT FROM v_component
         OR is_visible IS DISTINCT FROM TRUE
         OR parent_id  IS DISTINCT FROM v_parent
         OR sort_order IS DISTINCT FROM v_sort);
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0368 断言 D 失败：价格查询页的 path/component/is_visible/parent_id/sort_order 未补成终态';
    END IF;

    -- 权限码承载行必须仍然唯一（删菜单的「A 方案」会让 perm_menu_gate 变红，
    -- 这里把「本卡选 B、承载行保持 1 条」钉成库内断言，防止以后被人手滑删掉）
    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE auth_code = 'business:erp:price:resolve' AND deleted_at IS NULL;
    IF n_bad <> 1 THEN
        RAISE EXCEPTION 'V0368 断言 E 失败：business:erp:price:resolve 的承载行应为 1 条，实际 % 条', n_bad;
    END IF;

    SELECT count(*) INTO n_bad
      FROM (SELECT sort_order FROM sys_menu
             WHERE deleted_at IS NULL AND menu_type = 2 AND parent_id = v_parent
             GROUP BY sort_order HAVING count(*) > 1) t;
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0368 断言 F 失败：销售管理目录下 menu_type=2 的 sort_order 有 % 组重复', n_bad;
    END IF;

    RAISE NOTICE 'V0368 §2 完成：价格查询页已开（path=% component=% parent=销售管理 sort=%），本次改 % 行',
        v_path, v_component, v_sort, n_updated;
END $$;

-- ============================================================
-- §3 附件类型 16 → 17（+ RECEIPT_REFUND）
-- ============================================================
DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_type'
       AND conrelid = 'biz_doc_attachment'::regclass;

    IF v_def IS NULL OR position('RECEIPT_REFUND' IN v_def) = 0 THEN
        ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_type
            CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                                'FIXED_ASSET', 'EXPENSE_REPORT',
                                'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN',
                                'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE',
                                'RECEIPT_REFUND'));
        RAISE NOTICE 'V0368：ck_biz_doc_attachment_type 已放宽到 17 值（旧定义 %）', coalesce(v_def, '<不存在>');
    ELSE
        RAISE NOTICE 'V0368：ck_biz_doc_attachment_type 已含 RECEIPT_REFUND，跳过';
    END IF;
END $$;

COMMENT ON COLUMN biz_doc_attachment.doc_type IS
    '单据类型：CONTRACT / PURCHASE_ORDER / STOCK_IN / PAYABLE / PAYMENT / FIXED_ASSET / EXPENSE_REPORT / INVOICE / RECEIPT / STOCK_TAKE / PURCHASE_RETURN / SALES_RETURN（必传规则覆盖）/ STOCK_TRANSFER / SUPPLIER / CUSTOMER / OPENING_BALANCE（ATT-11 建议传，不校验）/ RECEIPT_REFUND（V0368：退款凭据）——与 attachment::model::DocKind 一一对应，V0368 放宽到 17 值';

DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_attachment_rule_doc_type'
       AND conrelid = 'biz_attachment_rule'::regclass;

    -- 表不存在（老库没跑过 V0347）时本段直接跳过
    IF v_def IS NULL THEN
        RAISE NOTICE 'V0368：biz_attachment_rule 或它的 doc_type 约束不存在，跳过';
    ELSIF position('RECEIPT_REFUND' IN v_def) = 0 THEN
        ALTER TABLE biz_attachment_rule DROP CONSTRAINT IF EXISTS ck_biz_attachment_rule_doc_type;
        ALTER TABLE biz_attachment_rule
            ADD CONSTRAINT ck_biz_attachment_rule_doc_type
            CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                                'FIXED_ASSET', 'EXPENSE_REPORT',
                                'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN',
                                'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE',
                                'RECEIPT_REFUND'));
        RAISE NOTICE 'V0368：ck_biz_attachment_rule_doc_type 已放宽到 17 值';
    ELSE
        RAISE NOTICE 'V0368：ck_biz_attachment_rule_doc_type 已含 RECEIPT_REFUND，跳过';
    END IF;
END $$;

-- ============================================================
-- §4 自检（up 后必须全为真）
-- ============================================================
DO $$
DECLARE
    v_type  text;
    v_rule  text;
    n_bad   integer;
BEGIN
    -- 4.1 全库再无 path 为空的页面菜单（卡面验收 ⑤ 的库内口径）
    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 2 AND (path IS NULL OR path = '');
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0368 断言 G 失败：仍有 % 条 menu_type=2 的菜单 path 为空', n_bad;
    END IF;

    -- 4.2 两行终态（幂等重放时也必须成立）
    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND ((id = '20000000-0000-0000-0000-000000002950'::uuid AND path = '/business/erp/receipt-refund')
         OR (id = '20000000-0000-0000-0000-000000003071'::uuid AND path = '/business/erp/price'));
    IF n_bad <> 2 THEN
        RAISE EXCEPTION 'V0368 断言 H 失败：两个页面节点的 path 终态只有 % 条成立（应 2）', n_bad;
    END IF;

    -- 4.3 附件约束必须已含 RECEIPT_REFUND
    SELECT pg_get_constraintdef(oid) INTO v_type
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_type'
       AND conrelid = 'biz_doc_attachment'::regclass;
    IF v_type IS NULL OR position('RECEIPT_REFUND' IN v_type) = 0 THEN
        RAISE EXCEPTION 'V0368 断言 I 失败：ck_biz_doc_attachment_type 未含 RECEIPT_REFUND：%', coalesce(v_type, '<不存在>');
    END IF;

    SELECT pg_get_constraintdef(oid) INTO v_rule
      FROM pg_constraint
     WHERE conname = 'ck_biz_attachment_rule_doc_type'
       AND conrelid = 'biz_attachment_rule'::regclass;
    IF v_rule IS NOT NULL AND position('RECEIPT_REFUND' IN v_rule) = 0 THEN
        RAISE EXCEPTION 'V0368 断言 J 失败：规则表 doc_type 约束未含 RECEIPT_REFUND：%', v_rule;
    END IF;

    RAISE NOTICE 'V0368 完成：收款退款单 / 取价查询两页已开，全库无空 path 页面菜单，附件类型 17 值';
END $$;
