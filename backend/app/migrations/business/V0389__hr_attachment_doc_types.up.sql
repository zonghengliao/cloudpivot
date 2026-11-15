-- ============================================================
-- V0389：附件单据类型放宽 17 → 21（+ EMPLOYEE / EMPLOYEE_CONTRACT / LEAVE_REQUEST / SALARY_SHEET）
--        + 2 条附件必传规则（HR-05）
--
-- 依据：docs/architecture/ARCH-9-人力资源模块设计.md
--   §3.2 ⑦（附件类型放宽 + 必传规则）、§2.1 表头（"加 2 条：劳动合同必传扫描件 / 工资表可选（不校验）"）
--   §7.1（迁移清单）、§8.3 R10（4 个 doc_type 新值要同步 **4 处**）
--
-- ── 号段 ────────────────────────────────────────────────────
-- 设计号 V0383 已被成本核算实占（V0383 现为 HR-01 的 hr_tables），HR 全系列两次顺延后本卡落
-- **V0388 hr_mask_config + V0389 hr_attachment_doc_types**（见 V0388 头注释的号段表）。
--
-- ── 这 4 个值必须**四处同步**（漏一处即静默失败，设计 §8.3 R10）──
--   ① 库：`ck_biz_doc_attachment_type`（上传落库的 CHECK）—— 本迁移
--   ② 库：`ck_biz_attachment_rule_doc_type`（规则管理界面「选了要能存」的 CHECK）—— 本迁移
--   ③ Rust：`business/shared/doc_kind.rs` 的 `DocKind` / `ATTACHMENT_DOC_KINDS` —— **HR-06**
--   ④ 前端：`api/.../attachment.ts` 的类型词表 —— **HR-07**
-- 本卡只做 ①②（库侧）。③④ 未落地前，界面上传这 4 类附件仍会被 DTO 白名单拦（400），那是预期的
-- 分批落地顺序，不是本迁移的缺陷；反过来，缺 ①② 的话 ③④ 落地后上传会撞 23514 变 500。
--
-- ── 设计稿的「16 → 20」已经过期：现网是 **17** 值 ──────────────
-- V0350 放宽到 16（+STOCK_TRANSFER/SUPPLIER/CUSTOMER/OPENING_BALANCE），V0368 又放宽到 17
-- （+RECEIPT_REFUND）。**照抄设计稿的 20 值列表会把 RECEIPT_REFUND 从 CHECK 里摘掉** ——
-- 那意味着：存量退款凭据附件在 `ADD CONSTRAINT` 的校验阶段直接失败，整个迁移回滚；
-- 更糟的是若有人「先 DROP 再 ADD」而 ADD 失败，表上就**再没有 doc_type 约束**（脏值随便进）。
-- 所以本迁移的列表 = **现网 17 值 ∪ 新 4 值 = 21 值**，自检里断言「17 个老值一个不少」。
--
-- ── 幂等 ────────────────────────────────────────────────────
-- 照 V0350 / V0368 的既有做法：先读 `pg_get_constraintdef`，**新值已全在就跳过**
-- （不 DROP 再 ADD，避免每次重放都全表校验 + 拿排他锁）；只有缺值时才重建约束。
-- `biz_attachment_rule` 在自己的库上可能不存在（没跑过 V0347 的老库）→ 该段整体跳过并打 NOTICE。
-- 种子规则用 `NOT EXISTS (doc_type, file_role, required_at, deleted_at IS NULL)`
-- （该表**没有**唯一约束，只能靠 NOT EXISTS 幂等）。文件内**不写** BEGIN/COMMIT。
--
-- ── 2 条规则的两处口径纠偏（设计稿的写法在真库上会出事故，已回写设计文档 §3.2 ⑦）──
-- 1. `SALARY_SHEET` 的 `file_role` 用 **NULL（任意角色都算）**，不是设计稿的 `'OTHER'`：
--    `'OTHER'` 是上传接口的**入参老别名**，落库时被归一成 `'DEFAULT'`
--    （`attachment::model::ROLE_OTHER → FileRole::Default`），而规则侧是按**字面值**比较
--    （`rule.rs::file_role_filter`）—— 写 'OTHER' 永远匹配不到任何附件，一旦这条规则被打开，
--    工资表激活会被永久拦死且「传了也说没传」。
-- 2. `SALARY_SHEET` 这条规则 `enabled = FALSE`：设计 §2.1 / §3.2 ⑦ 都写「建议传，**不硬拦**」，
--    而规则表没有「建议」这一档（只有 enabled 布尔）。置 false 才是准确的编码：
--    规则在附件规则页看得见、口径在案，运维要变硬闸时一键打开即可；反之若置 true，
--    任何一个走 `ensure_attachments` 的工资表动作都会被**永久硬拦**（设计明说不硬拦）。
--    `EMPLOYEE_CONTRACT` 那条是设计明确的**必传**（D-5），enabled = TRUE。
--
-- ── 回滚 ────────────────────────────────────────────────────
--   `V0389__hr_attachment_doc_types.down.sql`：有附件/规则用到 4 个新值就 **拒绝回滚**；
--   否则删本迁移的 2 条规则 → 两个约束收回 17 值 → 断言零残留。
-- ============================================================

-- ============================================================
-- §1 前置条件：放宽对象必须真实存在
-- ============================================================
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.biz_doc_attachment') IS NULL THEN
        RAISE EXCEPTION 'V0389 前置条件失败：biz_doc_attachment 不存在（V0212 未应用？）——本迁移的放宽对象就是它的 doc_type CHECK';
    END IF;

    SELECT count(*) INTO n FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_type'
       AND conrelid = 'biz_doc_attachment'::regclass;
    IF n <> 1 THEN
        RAISE NOTICE 'V0389 前置条件提示：ck_biz_doc_attachment_type 不存在（被手工删过？），本迁移会按 21 值新建';
    END IF;

    IF to_regclass('public.biz_attachment_rule') IS NULL THEN
        RAISE NOTICE 'V0389 前置条件提示：biz_attachment_rule 不存在（老库未跑 V0347），规则表放宽与 2 条种子将跳过';
    END IF;

    RAISE NOTICE 'V0389 前置条件通过：biz_doc_attachment 在位';
END $$;

-- ============================================================
-- §2 `biz_doc_attachment.doc_type`：17 → 21
--
-- 21 值 = 现网 17 值（含 RECEIPT_REFUND）
--        ∪ EMPLOYEE / EMPLOYEE_CONTRACT / LEAVE_REQUEST / SALARY_SHEET
-- ============================================================
DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_type'
       AND conrelid = 'biz_doc_attachment'::regclass;

    -- 4 个新值全在就跳过：EMPLOYEE / EMPLOYEE_CONTRACT / LEAVE_REQUEST / SALARY_SHEET
    IF v_def IS NOT NULL
       AND position('''EMPLOYEE''' IN v_def) > 0
       AND position('''EMPLOYEE_CONTRACT''' IN v_def) > 0
       AND position('''LEAVE_REQUEST''' IN v_def) > 0
       AND position('''SALARY_SHEET''' IN v_def) > 0 THEN
        RAISE NOTICE 'V0389：ck_biz_doc_attachment_type 已含 4 个新值，跳过（不 DROP 再 ADD）';
    ELSE
        ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_type
            CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                                'FIXED_ASSET', 'EXPENSE_REPORT',
                                'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN',
                                'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE',
                                'RECEIPT_REFUND',
                                'EMPLOYEE', 'EMPLOYEE_CONTRACT', 'LEAVE_REQUEST', 'SALARY_SHEET'));
        RAISE NOTICE 'V0389：ck_biz_doc_attachment_type 已放宽到 21 值（旧定义 %）', coalesce(v_def, '<不存在>');
    END IF;
END $$;

COMMENT ON COLUMN biz_doc_attachment.doc_type IS
    '单据类型：CONTRACT / PURCHASE_ORDER / STOCK_IN / PAYABLE / PAYMENT / FIXED_ASSET / EXPENSE_REPORT / INVOICE / RECEIPT / STOCK_TAKE / PURCHASE_RETURN / SALES_RETURN（必传规则覆盖）/ STOCK_TRANSFER / SUPPLIER / CUSTOMER / OPENING_BALANCE（ATT-11 建议传，不校验；期初的 doc_id = 期初凭证 id）/ RECEIPT_REFUND（V0368：退款凭据）/ EMPLOYEE / EMPLOYEE_CONTRACT / LEAVE_REQUEST / SALARY_SHEET（V0389：人力资源 4 类）——与 attachment::model::DocKind 一一对应，V0389 放宽到 21 值';

-- ============================================================
-- §3 `biz_attachment_rule.doc_type`：同步到 21 值
--
-- 为什么必须同步：附件规则管理界面的「单据类型」下拉来自 `DocKind::ALL`，后端存规则时撞这个 CHECK。
-- 不收宽就是「界面能选、保存 400」—— 最糟的一种前后端漂移（V0350 头注释里记着同一个坑）。
-- ============================================================
DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_attachment_rule_doc_type'
       AND conrelid = 'biz_attachment_rule'::regclass;

    IF v_def IS NULL THEN
        RAISE NOTICE 'V0389：biz_attachment_rule 或它的 doc_type 约束不存在，跳过';
    ELSIF position('''EMPLOYEE''' IN v_def) > 0
       AND position('''EMPLOYEE_CONTRACT''' IN v_def) > 0
       AND position('''LEAVE_REQUEST''' IN v_def) > 0
       AND position('''SALARY_SHEET''' IN v_def) > 0 THEN
        RAISE NOTICE 'V0389：ck_biz_attachment_rule_doc_type 已含 4 个新值，跳过';
    ELSE
        ALTER TABLE biz_attachment_rule DROP CONSTRAINT IF EXISTS ck_biz_attachment_rule_doc_type;
        ALTER TABLE biz_attachment_rule
            ADD CONSTRAINT ck_biz_attachment_rule_doc_type
            CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                                'FIXED_ASSET', 'EXPENSE_REPORT',
                                'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN',
                                'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE',
                                'RECEIPT_REFUND',
                                'EMPLOYEE', 'EMPLOYEE_CONTRACT', 'LEAVE_REQUEST', 'SALARY_SHEET'));
        RAISE NOTICE 'V0389：ck_biz_attachment_rule_doc_type 已放宽到 21 值（旧定义 %）', coalesce(v_def, '<不存在>');
    END IF;
END $$;

-- ============================================================
-- §4 2 条附件规则（设计 §3.2 ⑦）
--
--   EMPLOYEE_CONTRACT  CONTRACT_SCAN  SUBMIT    1..20  enabled=true   —— 劳动合同签订/续签必传扫描件（D-5）
--   SALARY_SHEET       NULL           ACTIVATE  1..20  enabled=false  —— 工资表签字页「建议传」（设计明写不硬拦）
--
-- `allow_exts` 留 NULL = 不限格式：设计没定格式白名单，硬塞 pdf 会把 ofd / xml / jpg 扫描件误拦
-- （考勤/合同扫描件现场既有拍照也有 PDF）。要收格式时改这两行的 allow_exts 即可，不用改代码。
-- `condition_sql` 留 NULL = 恒适用（合同扫描件与工资表签字页没有「按单据行分支」的需求）。
--
-- 为什么整段包在 DO 块里：`biz_attachment_rule` 在老库上可能不存在（没跑过 V0347）。写成
-- `INSERT … WHERE to_regclass(...) IS NOT NULL AND NOT EXISTS (SELECT 1 FROM biz_attachment_rule …)`
-- 是不管用的 —— PG 会把整条语句一起规划，NOT EXISTS 里引用的表不存在就直接报错（守卫来不及生效）。
-- ============================================================
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.biz_attachment_rule') IS NULL THEN
        RAISE NOTICE 'V0389：biz_attachment_rule 不存在（老库未跑 V0347），2 条 HR 附件规则跳过';
        RETURN;
    END IF;

    INSERT INTO biz_attachment_rule (doc_type, file_role, required_at, min_count, max_count,
                                     allow_exts, condition_sql, enabled, remark)
    SELECT 'EMPLOYEE_CONTRACT', 'CONTRACT_SCAN', 'SUBMIT', 1, 20, NULL, NULL, TRUE,
           '劳动合同签订/续签必须上传扫描件（ARCH-9-2 D-5）'
     WHERE NOT EXISTS (SELECT 1 FROM biz_attachment_rule
                        WHERE doc_type = 'EMPLOYEE_CONTRACT' AND file_role = 'CONTRACT_SCAN'
                          AND required_at = 'SUBMIT' AND deleted_at IS NULL);
    GET DIAGNOSTICS n = ROW_COUNT;
    RAISE NOTICE 'V0389：EMPLOYEE_CONTRACT 必传规则插入 % 行（0 = 已存在，幂等跳过）', n;

    INSERT INTO biz_attachment_rule (doc_type, file_role, required_at, min_count, max_count,
                                     allow_exts, condition_sql, enabled, remark)
    SELECT 'SALARY_SHEET', NULL, 'ACTIVATE', 1, 20, NULL, NULL, FALSE,
           '工资表过账前建议上传签字扫描件（建议传，不硬拦）'
     WHERE NOT EXISTS (SELECT 1 FROM biz_attachment_rule
                        WHERE doc_type = 'SALARY_SHEET' AND file_role IS NULL
                          AND required_at = 'ACTIVATE' AND deleted_at IS NULL);
    GET DIAGNOSTICS n = ROW_COUNT;
    RAISE NOTICE 'V0389：SALARY_SHEET 建议规则插入 % 行（0 = 已存在，幂等跳过）', n;
END $$;

-- ============================================================
-- §5 自检（up 后必须全为真；重放时也必须成立）
-- ============================================================
DO $$
DECLARE
    v_def     text;
    v_vals    text[];
    n_missing integer;
    n         integer;
BEGIN
    -- 5.1 单据附件约束：21 个值齐全，且 17 个老值一个不少（存量附件仍合法）
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_type'
       AND conrelid = 'biz_doc_attachment'::regclass;
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0389 断言 A 失败：ck_biz_doc_attachment_type 不存在（放宽没落上，或表上已无约束）';
    END IF;

    SELECT array_agg(m[1]) INTO v_vals
      FROM regexp_matches(v_def, '''([A-Z_]{2,40})''', 'g') AS m;

    SELECT count(*) INTO n_missing
      FROM unnest(ARRAY['CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                        'FIXED_ASSET', 'EXPENSE_REPORT', 'INVOICE', 'RECEIPT', 'STOCK_TAKE',
                        'PURCHASE_RETURN', 'SALES_RETURN', 'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER',
                        'OPENING_BALANCE', 'RECEIPT_REFUND',
                        'EMPLOYEE', 'EMPLOYEE_CONTRACT', 'LEAVE_REQUEST', 'SALARY_SHEET']) AS e(v)
     WHERE NOT (e.v = ANY (coalesce(v_vals, ARRAY[]::text[])));
    IF n_missing <> 0 THEN
        RAISE EXCEPTION 'V0389 断言 B 失败：ck_biz_doc_attachment_type 缺 % 个应有取值（21 个值必须齐全）：%', n_missing, v_def;
    END IF;
    IF coalesce(array_length(v_vals, 1), 0) < 21 THEN
        RAISE EXCEPTION 'V0389 断言 C 失败：约束解析出的取值只有 % 个（应 ≥21，可能是约束被写成了别的形态）：%',
            coalesce(array_length(v_vals, 1), 0), v_def;
    END IF;

    -- 5.2 规则表约束（存在的话）也必须含 4 个新值
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_attachment_rule_doc_type'
       AND conrelid = 'biz_attachment_rule'::regclass;
    IF v_def IS NOT NULL THEN
        SELECT count(*) INTO n_missing
          FROM unnest(ARRAY['EMPLOYEE', 'EMPLOYEE_CONTRACT', 'LEAVE_REQUEST', 'SALARY_SHEET']) AS e(v)
         WHERE position('''' || e.v || '''' IN v_def) = 0;
        IF n_missing <> 0 THEN
            RAISE EXCEPTION 'V0389 断言 D 失败：ck_biz_attachment_rule_doc_type 缺 % 个新值：%', n_missing, v_def;
        END IF;
    END IF;

    -- 5.3 文件角色约束**没被本迁移动过**（15 值仍在：放宽 doc_type 不许顺手动 file_role）
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_role'
       AND conrelid = 'biz_doc_attachment'::regclass;
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0389 断言 E 失败：ck_biz_doc_attachment_role 不存在（文件角色约束被谁删了？）';
    END IF;
    SELECT count(*) INTO n_missing
      FROM unnest(ARRAY['CONTRACT_SCAN', 'SUPPLEMENT', 'ACCEPTANCE', 'INVOICE_SCAN', 'BANK_RECEIPT',
                        'SIGN_RECEIPT', 'BIZ_LICENSE', 'BANK_ACCOUNT_PERMIT', 'QUALIFICATION',
                        'CREDIT_FILE', 'QUOTATION', 'CUSTOMER_PO', 'TRANSFER_APPROVAL',
                        'OPENING_EVIDENCE', 'DEFAULT']) AS e(v)
     WHERE position('''' || e.v || '''' IN v_def) = 0;
    IF n_missing <> 0 THEN
        RAISE EXCEPTION 'V0389 断言 E 失败：ck_biz_doc_attachment_role 少了 % 个老角色（本迁移不该动它）：%', n_missing, v_def;
    END IF;

    -- 5.4 存量附件的每一行 doc_type 都必须落在新集合内（放宽失败 = 历史附件变成「不合法的行」）
    SELECT count(*) INTO n
      FROM biz_doc_attachment
     WHERE doc_type NOT IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                            'FIXED_ASSET', 'EXPENSE_REPORT', 'INVOICE', 'RECEIPT', 'STOCK_TAKE',
                            'PURCHASE_RETURN', 'SALES_RETURN', 'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER',
                            'OPENING_BALANCE', 'RECEIPT_REFUND',
                            'EMPLOYEE', 'EMPLOYEE_CONTRACT', 'LEAVE_REQUEST', 'SALARY_SHEET');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0389 断言 F 失败：有 % 行存量附件的 doc_type 不在 21 值集合内（历史附件会变脏数据）', n;
    END IF;

    -- 5.5 两条规则在位且取值精确（enabled 语义见头注释：合同硬闸 true / 工资表建议 false）
    IF to_regclass('public.biz_attachment_rule') IS NOT NULL THEN
        SELECT count(*) INTO n
          FROM biz_attachment_rule
         WHERE deleted_at IS NULL
           AND ((doc_type = 'EMPLOYEE_CONTRACT' AND file_role = 'CONTRACT_SCAN' AND required_at = 'SUBMIT'
                 AND min_count = 1 AND max_count = 20 AND enabled = TRUE)
             OR (doc_type = 'SALARY_SHEET' AND file_role IS NULL AND required_at = 'ACTIVATE'
                 AND min_count = 1 AND max_count = 20 AND enabled = FALSE));
        IF n <> 2 THEN
            RAISE EXCEPTION 'V0389 断言 G 失败：2 条 HR 附件规则应各 1 条且取值精确，实际匹配 % 条（重放不许插重复行）', n;
        END IF;

        -- 5.6 不许出现 file_role = 'OTHER' 的规则（它是入参老别名，规则侧按字面比会永久匹配不到）
        SELECT count(*) INTO n
          FROM biz_attachment_rule
         WHERE file_role = 'OTHER' AND deleted_at IS NULL;
        IF n <> 0 THEN
            RAISE EXCEPTION 'V0389 断言 H 失败：规则表里有 % 条 file_role=OTHER 的规则（老别名不是落库值，永远匹配不到附件）', n;
        END IF;
    END IF;

    RAISE NOTICE 'V0389 完成：附件单据类型 17 → 21 值（存量 17 类全部仍合法），HR 附件规则 2 条（合同硬闸 / 工资表建议不拦）';
END $$;
