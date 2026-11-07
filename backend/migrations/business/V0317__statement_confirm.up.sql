-- ======================================================
-- V0317：客户对账单（SO-43 · ARCH-7 §15.7）—— 对账确认台账 + 5 个权限码 + 应收台账下的按钮 + 角色授权
--
-- ## 本卡建表口径（与 ARCH-7 §15.7.3 稿面的差异，逐条记在这里）
--
--   稿面 9 列：id / customer_id / period_from / period_to / total_amount /
--               status / confirmed_at / confirmed_by / dispute_reason / attachment_id + 审计
--
--   本迁移**多出 6 列**，每一条都是「少一个就会算错或者对不完账」：
--     ① currency_code    —— 客户可能有多币种往来，对账单按单一币种出具；不记币种，
--                           USD 的 1000 与 CNY 的 1000 会被当成同一张单的对账总额；
--     ② opening_balance  —— 签发时的期初余额（对账等式的一端，必须在文件里可复现）；
--     ③ closing_balance  —— 签发时的期末余额（= total_amount，但语义分开写：
--                           total_amount 是「对账总额」这个业务名，closing_balance 是等式的一端）；
--     ④⑤⑥ total_invoiced / total_received / total_returned —— 本期发生额三件套。
--                           **为什么必须快照**：客户手里的对账单是一份文件。签发后财务再做一笔收款，
--                           若台账只看 `biz_receivable` 现算，回读的数字就与客户手里那份不一致 ——
--                           对账永远对不完。签发即快照，之后账怎么动都与这份文件无关。
--     ⑦ item_count        —— 签发时的明细笔数（客户问「为什么我这 12 笔你只列 11 笔」时能自证）。
--
--   金额精度取 `NUMERIC(18,4)`（稿面写的 (18,2)）：与 biz_receivable / biz_voucher_entry 同型，
--   少一位会静默四舍五入，对账差额会以「分」的形态永远差 0.01。
--
--   状态域 `SENT / CONFIRMED / DISPUTED`（稿面逐字）；本迁移用 CHECK 兜住，不做状态机表：
--   这三个状态是「客户回执」而不是「单据流转」，没有审批、没有作废、没有反向边。
--
--   `customer_id` / `attachment_id` 都是**弱引用**（无外键）：
--     · 客户走软删（V0260），外键会把「客户软删但历史对账单要留」堵死；
--     · 附件在 `sys_file`，对账单签发早于附件上传（客户是先收到文件再回传盖章件），
--       外键会让「先签发后上传」这一步直接失败。
--
-- ## 为什么带迁移（两个硬门禁）
--
--   ① `perm_menu_gate.py`（check.sh 闸⑥）：`#[nik::permission(permission = "x")]` 里的每个码
--      必须在 `sys_menu.auth_code` 有承载行，否则该码授不出去（除超管外谁都调不动，且不报错）。
--      SO-43 新增 6 个接口 ⇒ 5 个码：
--
--        business:erp:statement:view      对账单查询 + 台账分页
--        business:erp:statement:export    对账单导出 xlsx
--        business:erp:statement:send      签发对账单
--        business:erp:statement:confirm   客户确认
--        business:erp:statement:dispute   客户提异议
--
--      与 `statement.rs` 的 `#[nik::permission]` 逐字对应（差一个字 = 接口永远 403）。
--
--   ② `system::menu::tests::n08` 只校验 `menu_type=2` 的 path 能否在前端路由里解析。
--      本卡**不新增页面**（对账单挂在既有「应收台账」页里出具），只加 5 个 `menu_type=3` 按钮
--      ⇒ 不碰 path，也不会让 n08 红。
--
-- ## 菜单树（挂在「应收台账」…2880 下，不新开页面）
--
--   应收账款 /business/erp/receivable（V0267 …2880，menu_type=2，auth_code=receivable:list）
--     ├── 客户对账单（…3175，menu_type=3，statement:view）
--     ├── 对账单导出（…3176，statement:export）
--     ├── 对账单签发（…3177，statement:send）
--     ├── 客户确认（…3178，statement:confirm）
--     └── 客户提异议（…3179，statement:dispute）
--
-- ## 授权（本迁移唯一替人做的决定，写在注释里）
--
--   · FINANCE_MANAGER（财务经理）：5 个全给 —— 对账单是财务对外出具的凭证，签发 / 回执登记都归它；
--   · SALES_SUPERVISOR（销售经理）：5 个全给 —— 销售经理要能代客户催对账、登记回执；
--   · SALES_CLERK（销售员）：只给 `statement:view` + `statement:export` —— 催款时要能给客户发单，
--     但不能替客户「确认」（那是客户的回执，销售员自证等于自己给自己签收）；
--   · GENERAL_MANAGER（总经理）：`statement:view` + `statement:export` —— 看与拿走，不改回执；
--   · WAREHOUSE_MANAGER（仓管）：不给 —— 对账单与仓储无关。
--
--   `ON CONFLICT (role_id, menu_id) DO NOTHING` 幂等；角色不存在时自然插入 0 行
--   （末尾自检会点「应有授权却 0 行」的异常，不让它静默半套上线）。
--
-- ## 幂等与回滚
--
--   表 / 索引 `IF NOT EXISTS`；菜单按固定 uuid `ON CONFLICT (id) DO UPDATE`；
--   授权 `ON CONFLICT (role_id, menu_id) DO NOTHING`。
--   回滚：V0317__statement_confirm.down.sql 先删授权、再删按钮、最后删表。
-- ======================================================

-- ---------- ① 对账确认台账 ----------
CREATE TABLE IF NOT EXISTS biz_statement_confirm (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    customer_id     UUID          NOT NULL,                            -- 客户（弱引用 biz_customer.id）
    period_from     DATE          NOT NULL,                            -- 对账期间起（含当天）
    period_to       DATE          NOT NULL,                            -- 对账期间止（含当天）
    currency_code   CHAR(3)       NOT NULL DEFAULT 'CNY',              -- 币种（对账单按单一币种出具）
    total_amount    NUMERIC(18,4) NOT NULL,                            -- 对账总额（= 签发时的期末余额）
    opening_balance NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 签发时的期初余额
    closing_balance NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 签发时的期末余额
    total_invoiced  NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 签发时的本期挂账合计
    total_received  NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 签发时的本期收款合计
    total_returned  NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 签发时的本期退货合计
    item_count      INT           NOT NULL DEFAULT 0,                  -- 签发时的明细笔数
    status          VARCHAR(16)   NOT NULL DEFAULT 'SENT',             -- SENT / CONFIRMED / DISPUTED
    sent_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 签发时间
    confirmed_at    TIMESTAMPTZ,                                       -- 客户确认时间
    confirmed_by    VARCHAR(64),                                       -- 客户方确认人
    dispute_reason  TEXT,                                              -- 异议原因
    attachment_id   UUID,                                              -- 客户盖章回传的对账单（弱引用 sys_file.id）
    remark          TEXT,                                              -- 备注
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 审计：创建时间
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 审计：更新时间
    create_by       UUID,                                              -- 审计：创建人
    update_by       UUID,                                              -- 审计：更新人
    deleted_at      TIMESTAMPTZ,                                       -- 软删时间（NULL = 存活）

    -- 状态域（稿面逐字）
    CONSTRAINT ck_biz_statement_status
        CHECK (status IN ('SENT','CONFIRMED','DISPUTED')),
    -- 期间不能倒置（倒置的对账单余额必然为负，是无意义数据）
    CONSTRAINT ck_biz_statement_period
        CHECK (period_from <= period_to),
    -- 金额三连：对账总额不许为负（应收余额为负 = 客户多付，属预收，不进对账单），
    -- 发生额不许为负（口径一律正数，方向由字段名表达）
    CONSTRAINT ck_biz_statement_amount
        CHECK (total_amount >= 0 AND total_invoiced >= 0 AND total_received >= 0
               AND total_returned >= 0),
    -- 笔数不许为负
    CONSTRAINT ck_biz_statement_count
        CHECK (item_count >= 0),
    -- 状态与回执字段必须自洽：CONFIRMED 必须有确认人与确认时间；
    -- DISPUTED 必须有异议原因（客户凭什么不认这张单，要能追溯）
    CONSTRAINT ck_biz_statement_confirmed
        CHECK (status <> 'CONFIRMED' OR (confirmed_at IS NOT NULL AND confirmed_by IS NOT NULL)),
    CONSTRAINT ck_biz_statement_disputed
        CHECK (status <> 'DISPUTED' OR dispute_reason IS NOT NULL)
);

COMMENT ON TABLE  biz_statement_confirm IS '客户对账确认台账（SO-43 · ARCH-7 §15.7.3）：签发即快照金额，客户回执 SENT→CONFIRMED/DISPUTED';
COMMENT ON COLUMN biz_statement_confirm.customer_id IS '客户 id（弱引用 biz_customer.id，无外键：客户软删后历史对账单仍要可读）';
COMMENT ON COLUMN biz_statement_confirm.currency_code IS '币种（ISO 4217）：对账单按单一币种出具，跨币种混加没有会计意义';
COMMENT ON COLUMN biz_statement_confirm.total_amount IS '对账总额 = 签发时的期末余额（客户签字认的就是这个数）';
COMMENT ON COLUMN biz_statement_confirm.opening_balance IS '签发时的期初余额（对账等式：期初 + 本期发生 = 期末）';
COMMENT ON COLUMN biz_statement_confirm.closing_balance IS '签发时的期末余额（与 total_amount 同值，语义分开：一个是业务名、一个是等式的一端）';
COMMENT ON COLUMN biz_statement_confirm.status IS '客户回执状态：SENT（已签发）/ CONFIRMED（已确认）/ DISPUTED（已提异议）';
COMMENT ON COLUMN biz_statement_confirm.attachment_id IS '客户盖章回传的对账单文件 id（弱引用 sys_file.id，无外键：先签发后上传）';
COMMENT ON COLUMN biz_statement_confirm.deleted_at IS '软删标记；唯一索引带 WHERE deleted_at IS NULL ⇒ 软删后同期间可重新签发';

-- 同一客户 × 同一期间 × 同一币种只许一份（存活行唯一）；重新签发走 UPDATE 覆盖快照
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_statement_period
    ON biz_statement_confirm (customer_id, period_from, period_to, currency_code)
    WHERE deleted_at IS NULL;
-- 客户对账历史（按期间倒序取）
CREATE INDEX IF NOT EXISTS idx_biz_statement_customer
    ON biz_statement_confirm (customer_id, period_from DESC);
-- 待回执看板 / 超期未确认扫描
CREATE INDEX IF NOT EXISTS idx_biz_statement_status
    ON biz_statement_confirm (status, sent_at);

-- ---------- ② 5 个按钮（menu_type=3，挂在「应收台账」下，不新开页面） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003175'::uuid, '客户对账单', 3,
     'business:erp:statement:view', NULL, '20000000-0000-0000-0000-000000002880'::uuid,
     NULL, 10, 1, NULL, '按客户 + 期间出对账单：期初 / 明细 / 期末（ARCH-7 §15.7.2）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003176'::uuid, '对账单导出', 3,
     'business:erp:statement:export', NULL, '20000000-0000-0000-0000-000000002880'::uuid,
     NULL, 20, 1, NULL, '对账单导出 xlsx（发给客户财务核对，复用 impexp 写表）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003177'::uuid, '对账单签发', 3,
     'business:erp:statement:send', NULL, '20000000-0000-0000-0000-000000002880'::uuid,
     NULL, 30, 1, NULL, '签发对账单（服务端重算并快照金额，SENT 可重发）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003178'::uuid, '客户确认', 3,
     'business:erp:statement:confirm', NULL, '20000000-0000-0000-0000-000000002880'::uuid,
     NULL, 40, 1, NULL, '登记客户确认回执（SENT → CONFIRMED）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003179'::uuid, '客户提异议', 3,
     'business:erp:statement:dispute', NULL, '20000000-0000-0000-0000-000000002880'::uuid,
     NULL, 50, 1, NULL, '登记客户异议（SENT → DISPUTED，必须给原因）', TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at  = NULL,
       updated_at  = now();

-- ---------- ③ 角色授权矩阵 ----------
DROP TABLE IF EXISTS _stmt_matrix;
CREATE TEMP TABLE _stmt_matrix (
    role_code text NOT NULL,
    menu_id   uuid NOT NULL,
    PRIMARY KEY (role_code, menu_id)
);

INSERT INTO _stmt_matrix (role_code, menu_id) VALUES
-- FINANCE_MANAGER：5 个全给（对账单是财务对外出具的凭证）
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003175'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003176'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003177'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003178'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003179'::uuid),
-- SALES_SUPERVISOR：5 个全给（催对账 + 登记回执）
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003175'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003176'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003177'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003178'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003179'::uuid),
-- SALES_CLERK：只看 + 导出（催款能发单，但不能替客户签收）
    ('SALES_CLERK', '20000000-0000-0000-0000-000000003175'::uuid),
    ('SALES_CLERK', '20000000-0000-0000-0000-000000003176'::uuid),
-- GENERAL_MANAGER：看 + 导出（不改回执）
    ('GENERAL_MANAGER', '20000000-0000-0000-0000-000000003175'::uuid),
    ('GENERAL_MANAGER', '20000000-0000-0000-0000-000000003176'::uuid);

INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.menu_id
  FROM _stmt_matrix m
  JOIN sys_role r ON r.role_code = m.role_code AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

DROP TABLE _stmt_matrix;

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    v_missing  text;
    v_dupe     text;
    v_rows     integer;
    v_cols     integer;
    v_parent   uuid;
    v_type     smallint;
    v_status   text;
    v_conf     text;
BEGIN
    -- 4.1 表在位 + 卡面列全在 + 多出的 6 列也在
    IF to_regclass('public.biz_statement_confirm') IS NULL THEN
        RAISE EXCEPTION 'SO-43: biz_statement_confirm 未建';
    END IF;
    SELECT count(*) INTO v_cols FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_statement_confirm';
    IF v_cols <> 24 THEN
        RAISE EXCEPTION 'SO-43: biz_statement_confirm 列数 = %（期望 24）', v_cols;
    END IF;
    SELECT count(*) INTO v_cols FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_statement_confirm'
       AND column_name IN ('id','customer_id','period_from','period_to','currency_code',
                           'total_amount','opening_balance','closing_balance',
                           'total_invoiced','total_received','total_returned','item_count',
                           'status','sent_at','confirmed_at','confirmed_by','dispute_reason',
                           'attachment_id','remark','created_at','updated_at',
                           'create_by','update_by','deleted_at');
    IF v_cols <> 24 THEN
        RAISE EXCEPTION 'SO-43: 关键列只齐了 %/24', v_cols;
    END IF;

    -- 4.2 状态域 / 期间 / 回执自洽三条约束的行为探针（真的插一次再回滚）
    BEGIN
        INSERT INTO biz_statement_confirm (customer_id, period_from, period_to, total_amount, status)
        VALUES (gen_random_uuid(), '2026-10-01', '2026-09-01', 0, 'SENT');
        RAISE EXCEPTION 'SO-43: 倒置期间竟然插进去了';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO biz_statement_confirm (customer_id, period_from, period_to, total_amount, status)
        VALUES (gen_random_uuid(), '2026-10-01', '2026-10-31', 0, 'ARCHIVED');
        RAISE EXCEPTION 'SO-43: 非法状态竟然插进去了';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO biz_statement_confirm (customer_id, period_from, period_to, total_amount, status)
        VALUES (gen_random_uuid(), '2026-10-01', '2026-10-31', 0, 'CONFIRMED');
        RAISE EXCEPTION 'SO-43: CONFIRMED 没有确认人/时间竟然插进去了';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO biz_statement_confirm (customer_id, period_from, period_to, total_amount, status)
        VALUES (gen_random_uuid(), '2026-10-01', '2026-10-31', 0, 'DISPUTED');
        RAISE EXCEPTION 'SO-43: DISPUTED 没有异议原因竟然插进去了';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- 4.3 五个权限码各且仅一行承载
    SELECT string_agg(code, ', ') INTO v_missing
      FROM (VALUES
            ('business:erp:statement:view'), ('business:erp:statement:export'),
            ('business:erp:statement:send'), ('business:erp:statement:confirm'),
            ('business:erp:statement:dispute')
           ) AS expected(code)
     WHERE NOT EXISTS (
           SELECT 1 FROM sys_menu m
            WHERE m.auth_code = expected.code AND m.deleted_at IS NULL);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'SO-43: 以下权限码在 sys_menu 里没有承载行：%', v_missing;
    END IF;

    SELECT string_agg(code, ', ') INTO v_dupe
      FROM (SELECT auth_code AS code, count(*) AS n FROM sys_menu
             WHERE deleted_at IS NULL
               AND auth_code IN ('business:erp:statement:view', 'business:erp:statement:export',
                                 'business:erp:statement:send', 'business:erp:statement:confirm',
                                 'business:erp:statement:dispute')
             GROUP BY auth_code HAVING count(*) > 1) AS dup;
    IF v_dupe IS NOT NULL THEN
        RAISE EXCEPTION 'SO-43: 以下权限码在 sys_menu 里有多行承载（授权歧义）：%', v_dupe;
    END IF;

    -- 4.4 5 个按钮挂在「应收台账」下，且都是 menu_type=3 / 无 path
    SELECT count(*) INTO v_rows FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000003175'::uuid,
                  '20000000-0000-0000-0000-000000003176'::uuid,
                  '20000000-0000-0000-0000-000000003177'::uuid,
                  '20000000-0000-0000-0000-000000003178'::uuid,
                  '20000000-0000-0000-0000-000000003179'::uuid)
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000002880'::uuid
       AND deleted_at IS NULL
       AND path IS NULL;
    IF v_rows <> 5 THEN
        RAISE EXCEPTION 'SO-43: 对账单按钮数 = %（期望 5，且父级必须是应收台账 …2880）', v_rows;
    END IF;

    SELECT parent_id, menu_type, status INTO v_parent, v_type, v_status
      FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000003175'::uuid;
    IF v_parent <> '20000000-0000-0000-0000-000000002880'::uuid OR v_type <> 3
       OR v_status <> '1' THEN
        RAISE EXCEPTION 'SO-43: 客户对账单按钮字段不对（parent=%, type=%, status=%）',
            v_parent, v_type, v_status;
    END IF;

    -- 4.5 关键授权在位（角色存在时；角色不存在时该角色 0 授权属正常）
    SELECT count(*) INTO v_rows
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE r.role_code IN ('FINANCE_MANAGER', 'SALES_SUPERVISOR')
       AND rm.menu_id IN ('20000000-0000-0000-0000-000000003175'::uuid,
                          '20000000-0000-0000-0000-000000003176'::uuid,
                          '20000000-0000-0000-0000-000000003177'::uuid,
                          '20000000-0000-0000-0000-000000003178'::uuid,
                          '20000000-0000-0000-0000-000000003179'::uuid);
    IF v_rows = 0 AND EXISTS (SELECT 1 FROM sys_role
                               WHERE role_code = 'FINANCE_MANAGER' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'SO-43: FINANCE_MANAGER / SALES_SUPERVISOR 一个对账单权限都没拿到';
    END IF;

    RAISE NOTICE 'SO-43: 对账确认台账 + 5 个权限码 + 应收台账按钮 + 授权自检通过';
END $$;
