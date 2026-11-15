-- ======================================================
-- V0260：销售链主数据 —— 客户档案两张表 + 3 个测试客户种子
--
--   ① biz_customer          客户主档（编码唯一 / 信用额度 / 状态 ACTIVE|FROZEN）
--   ② biz_customer_contact  客户联系人（外键 → biz_customer.id，主联系人唯一）
--
-- ── 号段（开工实测复核）──────────────────────────────────
-- 写入本文件时 backend/migrations/business/ 最大号 = V0259
--   （V0259__flow_instance_button_parentage，孤儿按钮归位卡 t_243bc2aa），
-- 本卡顺延取 V0260。迁移号会被并行卡抢走，**提交前**再 `ls business | tail` 复核一次。
--
-- ── 字段口径的三处判断（与卡面/既有代码对齐，别按别处注释改）──────
--  1) `status` 用 **VARCHAR(20) 域值 'ACTIVE' / 'FROZEN'**，不是既有的 SMALLINT 1/0。
--     卡面点名 ACTIVE/FROZEN（销售链设计要点要求状态机可读、与 LEGAL_EDGES 的
--     大写枚举同风格），供应商那套 `status SMALLINT(1 启用/0 停用)` 与它语义不同
--     （供应商是「启用/停用」，客户是「正常/冻结」，冻结不影响被引用，只影响开新单）。
--     ⚠ 已知口径差异：`backend/src/business/erp/contract_model.rs:31` 的文档注释写着
--     「查 biz_customer(id, deleted_at IS NULL, status = 1)」—— 那是**批次 2 落地前的占位推测**，
--     客户分支（R2）真正打开时必须改成 `status = 'ACTIVE'`。本卡不改那段注释（属 SO-02 之后的卡），
--     但在看板留言里点名，避免下一张卡照着旧注释写出 `status = 1`（int 比 varchar 恒定 0 行）。
--  2) `code` 唯一索引用**部分唯一索引**（`WHERE deleted_at IS NULL`），与
--     `uk_biz_supplier_code` / `uk_biz_unit_code` / `uk_biz_material_*` 全仓同口径：
--     软删后编码可重新启用，同时保留历史行。⇒ 应用层 `ON CONFLICT` 种子/upsert
--     **必须带谓词** `ON CONFLICT (code) WHERE deleted_at IS NULL`（见 skill 种子坑 §2）。
--  3) `customer_id` 是**硬外键**（卡面明确要求），`ON DELETE RESTRICT`：
--     客户只能软删（`deleted_at`），物理删会被联系人行挡住 —— 这正是要的
--     「档案删了联系人变孤儿」防线。其余 ERP 主数据多用弱引用，本表按卡面走硬外键。
--
-- ── 幂等 ──────────────────────────────────────────────
-- CREATE TABLE / INDEX IF NOT EXISTS；种子 `INSERT … SELECT … WHERE NOT EXISTS`
-- （按业务编码判重，重复应用不产生第二份、也**不覆盖**客户改过的名称/额度）。
-- 末尾 DO 块自检：表 / 列 / 索引 / 外键 / CHECK 域值 / 种子条数，任一不符 → 迁移失败。
-- ======================================================

-- ---------- ① biz_customer（客户主档） ----------
CREATE TABLE IF NOT EXISTS biz_customer (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    code          VARCHAR(40)   NOT NULL,                             -- 客户编码（业务唯一）
    name          VARCHAR(200)  NOT NULL,                             -- 客户全称
    tax_no        VARCHAR(50),                                        -- 纳税人识别号
    contact_name  VARCHAR(50),                                        -- 默认联系人姓名
    contact_phone VARCHAR(30),                                        -- 默认联系人电话
    address       VARCHAR(255),                                       -- 地址
    credit_limit  NUMERIC(18,4) NOT NULL DEFAULT 0,                   -- 信用额度（0 = 不控制）
    status        VARCHAR(20)   NOT NULL DEFAULT 'ACTIVE',            -- 状态：ACTIVE 正常 / FROZEN 冻结
    remark        TEXT,                                               -- 备注
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,   -- 审计：创建时间
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,   -- 审计：更新时间（model 层维护）
    create_by     UUID,                                               -- 审计：创建人
    update_by     UUID,                                               -- 审计：更新人
    deleted_at    TIMESTAMPTZ,                                        -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_customer_credit_limit CHECK (credit_limit >= 0),
    CONSTRAINT ck_biz_customer_status       CHECK (status IN ('ACTIVE','FROZEN'))
);

COMMENT ON TABLE  biz_customer IS '客户主档（销售链交易对手真相：合同 party_b_type=CUSTOMER → 本表）';
COMMENT ON COLUMN biz_customer.code         IS '客户编码：业务唯一键（部分唯一索引，软删后可复用）';
COMMENT ON COLUMN biz_customer.credit_limit IS '信用额度（NUMERIC(18,4)，>= 0；0 表示不控制额度）';
COMMENT ON COLUMN biz_customer.status       IS '状态域：ACTIVE=正常（可开新单）/ FROZEN=冻结（禁开新单，历史单据不受影响）';
COMMENT ON COLUMN biz_customer.deleted_at   IS '软删标记；物理删除会被 biz_customer_contact 的外键挡住（ON DELETE RESTRICT）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_customer_code
    ON biz_customer (code) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_customer_name   ON biz_customer (name);
CREATE INDEX IF NOT EXISTS idx_biz_customer_status ON biz_customer (status);

-- ---------- ② biz_customer_contact（客户联系人） ----------
CREATE TABLE IF NOT EXISTS biz_customer_contact (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    customer_id UUID          NOT NULL,                              -- 所属客户（硬外键）
    name        VARCHAR(50)   NOT NULL,                              -- 联系人姓名
    phone       VARCHAR(30),                                         -- 电话
    email       VARCHAR(120),                                        -- 邮箱
    position    VARCHAR(50),                                         -- 职位
    is_primary  SMALLINT      NOT NULL DEFAULT 0,                    -- 是否主联系人：1 是 / 0 否
    created_at  TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by   UUID,
    update_by   UUID,
    deleted_at  TIMESTAMPTZ,

    CONSTRAINT fk_biz_customer_contact_customer
        FOREIGN KEY (customer_id) REFERENCES biz_customer (id) ON DELETE RESTRICT,
    CONSTRAINT ck_biz_customer_contact_primary CHECK (is_primary IN (0,1))
);

COMMENT ON TABLE  biz_customer_contact IS '客户联系人（一个客户多条；主联系人每个客户至多一条且存活）';
COMMENT ON COLUMN biz_customer_contact.is_primary IS
    '主联系人标记：1=主 / 0=普通。唯一性由 uk_biz_customer_contact_primary（部分唯一索引）保证，换主需先清旧主';

CREATE INDEX IF NOT EXISTS idx_biz_customer_contact_customer
    ON biz_customer_contact (customer_id);
-- 每个存活客户至多一个主联系人（软删的行不占用名额）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_customer_contact_primary
    ON biz_customer_contact (customer_id) WHERE is_primary = 1 AND deleted_at IS NULL;

-- ---------- ③ 种子：3 个测试客户 + 各自 1 个主联系人 ----------
-- 编码用 `CUS-0001..0003`；id 用本迁移的确定性号段 `d0260…`（便于单据/巡检引用与回滚定位）。
-- 判重口径 = 业务编码存活行（不是 id），客户若已自建同编码则不插入、也不改它。
INSERT INTO biz_customer (id, code, name, tax_no, contact_name, contact_phone, address,
                          credit_limit, status, remark)
SELECT v.id::uuid, v.code, v.name, v.tax_no, v.contact_name, v.contact_phone, v.address,
       v.credit_limit, 'ACTIVE', v.remark
  FROM (VALUES
        ('d0260000-0000-4000-8000-000000000001', 'CUS-0001', '北京云枢科技有限公司',
         '91110108MA01ABCD1X', '张伟', '010-88880001', '北京市海淀区中关村南大街 1 号',
         500000.0000::numeric, '种子客户（测试）'),
        ('d0260000-0000-4000-8000-000000000002', 'CUS-0002', '上海云枢贸易有限公司',
         '91310115MA1KLMN02Y', '李娜', '021-66660002', '上海市浦东新区世纪大道 88 号',
         200000.0000::numeric, '种子客户（测试）'),
        ('d0260000-0000-4000-8000-000000000003', 'CUS-0003', '广州云枢实业有限公司',
         '91440101MA5OPQR03Z', '王强', '020-88880003', '广州市天河区珠江新城 6 号',
         0.0000::numeric,     '种子客户（测试；额度 0 = 不控制）')
       ) AS v(id, code, name, tax_no, contact_name, contact_phone, address, credit_limit, remark)
 WHERE NOT EXISTS (SELECT 1 FROM biz_customer c WHERE c.code = v.code AND c.deleted_at IS NULL);

INSERT INTO biz_customer_contact (id, customer_id, name, phone, email, position, is_primary)
SELECT v.id::uuid, v.customer_id::uuid, v.name, v.phone, v.email, v.position, 1
  FROM (VALUES
        ('d0260000-0000-4000-8000-000000000101', 'd0260000-0000-4000-8000-000000000001',
         '张伟', '010-88880001', 'zhangwei@example.com', '采购经理'),
        ('d0260000-0000-4000-8000-000000000102', 'd0260000-0000-4000-8000-000000000002',
         '李娜', '021-66660002', 'lina@example.com',     '财务总监'),
        ('d0260000-0000-4000-8000-000000000103', 'd0260000-0000-4000-8000-000000000003',
         '王强', '020-88880003', 'wangqiang@example.com', '总经理')
       ) AS v(id, customer_id, name, phone, email, position)
 WHERE NOT EXISTS (SELECT 1 FROM biz_customer_contact x WHERE x.id = v.id::uuid);

-- ---------- ④ 自检：结构 + 域值 + 种子条数 ----------
DO $$
DECLARE
    n INT;
    d TEXT;
BEGIN
    -- 4.1 两张表都在
    IF to_regclass('public.biz_customer') IS NULL
       OR to_regclass('public.biz_customer_contact') IS NULL THEN
        RAISE EXCEPTION 'V0260 自检失败：biz_customer / biz_customer_contact 未建全';
    END IF;

    -- 4.2 卡面点名的列必须齐全（信息模式，列名逐字）
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_customer'
       AND column_name IN ('id','code','name','tax_no','contact_name','contact_phone','address',
                           'credit_limit','status','remark','created_at','updated_at',
                           'create_by','update_by','deleted_at');
    IF n <> 15 THEN
        RAISE EXCEPTION 'V0260 自检失败：biz_customer 列只齐了 %/15', n;
    END IF;

    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_customer_contact'
       AND column_name IN ('id','customer_id','name','phone','email','position','is_primary',
                           'created_at','updated_at','create_by','update_by','deleted_at');
    IF n <> 12 THEN
        RAISE EXCEPTION 'V0260 自检失败：biz_customer_contact 列只齐了 %/12', n;
    END IF;

    -- 4.3 唯一索引 uk_biz_customer_code 必须在位，且是 deleted_at IS NULL 的部分索引
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_customer' AND indexname = 'uk_biz_customer_code';
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0260 自检失败：唯一索引 uk_biz_customer_code 缺失';
    END IF;
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0260 自检失败：uk_biz_customer_code 不是「存活行唯一」的部分索引（def=%）', d;
    END IF;

    -- 4.4 外键 customer_id → biz_customer(id)，且是 RESTRICT（软删语义）
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'fk_biz_customer_contact_customer' AND contype = 'f') THEN
        RAISE EXCEPTION 'V0260 自检失败：外键 fk_biz_customer_contact_customer 缺失';
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname = 'fk_biz_customer_contact_customer';
    IF d NOT LIKE '%REFERENCES biz_customer(id)%' THEN
        RAISE EXCEPTION 'V0260 自检失败：外键未指向 biz_customer(id)（def=%）', d;
    END IF;

    -- 4.5 三条 CHECK 在位，并按**行为**断言域值边界
    --     ⚠ 不能拿 `pg_get_constraintdef()` 的文本比数值边界：PG 会把 `credit_limit >= 0`
    --       归一成 `CHECK ((credit_limit >= (0)::numeric))`（实测首版迁移就栽在这，
    --       报「口径不对 def=CHECK ((credit_limit >= (0)::numeric))」）。
    --       文本只用来判「域值字面量在不在」，边界一律用「插一条必须被拒」来证。
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_customer_credit_limit') THEN
        RAISE EXCEPTION 'V0260 自检失败：ck_biz_customer_credit_limit 缺失';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_customer_contact_primary') THEN
        RAISE EXCEPTION 'V0260 自检失败：ck_biz_customer_contact_primary 缺失';
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint WHERE conname = 'ck_biz_customer_status';
    IF d IS NULL OR d NOT LIKE '%ACTIVE%' OR d NOT LIKE '%FROZEN%' THEN
        RAISE EXCEPTION 'V0260 自检失败：ck_biz_customer_status 域值不是 ACTIVE/FROZEN（def=%）', d;
    END IF;

    -- 4.5a credit_limit = -1 必须被拒（探针插入在子事务里回滚，不留行）
    BEGIN
        INSERT INTO biz_customer (code, name, credit_limit)
        VALUES ('__V0260_PROBE_NEG__', '自检探针（应被拒）', -1);
        RAISE EXCEPTION 'V0260 自检失败：credit_limit = -1 未被 ck_biz_customer_credit_limit 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.5b status 越界值必须被拒
    BEGIN
        INSERT INTO biz_customer (code, name, status)
        VALUES ('__V0260_PROBE_STATUS__', '自检探针（应被拒）', 'BOGUS');
        RAISE EXCEPTION 'V0260 自检失败：status = BOGUS 未被 ck_biz_customer_status 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.5c is_primary 越界值必须被拒
    BEGIN
        INSERT INTO biz_customer_contact (customer_id, name, is_primary)
        SELECT id, '自检探针（应被拒）', 2 FROM biz_customer WHERE code = 'CUS-0001';
        RAISE EXCEPTION 'V0260 自检失败：is_primary = 2 未被 ck_biz_customer_contact_primary 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.5d 外键必须真的拦孤儿联系人（SQLSTATE 23503）
    BEGIN
        INSERT INTO biz_customer_contact (customer_id, name)
        VALUES ('00000000-0000-4000-8000-0000000fffff'::uuid, '自检探针（应被拒）');
        RAISE EXCEPTION 'V0260 自检失败：customer_id 指向不存在的客户却插入成功（外键未生效）';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.5e 同一客户第二个主联系人必须被拒（部分唯一索引）
    BEGIN
        INSERT INTO biz_customer_contact (customer_id, name, is_primary)
        SELECT customer_id, '自检探针（应被拒）', 1
          FROM biz_customer_contact WHERE id = 'd0260000-0000-4000-8000-000000000101'::uuid;
        RAISE EXCEPTION 'V0260 自检失败：同一客户能有两个主联系人（uk_biz_customer_contact_primary 未生效）';
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 期望路径
    END;

    -- 4.6 status 默认值必须是 'ACTIVE'（漏 DEFAULT 会让「只传 name/code 的插入」直接 23502）
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_customer' AND column_name = 'status';
    IF d IS NULL OR d NOT LIKE '''ACTIVE''%' THEN
        RAISE EXCEPTION 'V0260 自检失败：biz_customer.status 默认值不是 ACTIVE（default=%）', d;
    END IF;

    -- 4.7 种子：3 个客户 + 3 个主联系人，且联系人无孤儿
    SELECT count(*) INTO n FROM biz_customer
     WHERE deleted_at IS NULL AND code IN ('CUS-0001','CUS-0002','CUS-0003');
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0260 自检失败：客户种子存活 % 条（期望 3）', n;
    END IF;
    SELECT count(*) INTO n
      FROM biz_customer_contact x JOIN biz_customer c ON c.id = x.customer_id
     WHERE c.code IN ('CUS-0001','CUS-0002','CUS-0003')
       AND x.is_primary = 1 AND x.deleted_at IS NULL AND c.deleted_at IS NULL;
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0260 自检失败：客户种子的主联系人 % 条（期望 3）', n;
    END IF;
    SELECT count(*) INTO n
      FROM biz_customer_contact x LEFT JOIN biz_customer c ON c.id = x.customer_id
     WHERE c.id IS NULL;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0260 自检失败：存在 % 条无主客户联系人（外键应拦住，检查是否手工插过）', n;
    END IF;
END $$;
