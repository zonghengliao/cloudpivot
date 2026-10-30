-- 路径 B 夹具：批次1 §2.1.2 / §2.1.4 / §2.1.7 / §2.3.3 原样的基线表（**没有**层级四列）
CREATE TABLE biz_seq (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    seq_type VARCHAR(32) NOT NULL, seq_name VARCHAR(50) NOT NULL, prefix VARCHAR(12) NOT NULL,
    date_pattern VARCHAR(8) NOT NULL DEFAULT 'yyyyMMdd', period_key VARCHAR(8) NOT NULL DEFAULT '',
    current_no BIGINT NOT NULL DEFAULT 0, step INT NOT NULL DEFAULT 1, separator VARCHAR(4) NOT NULL DEFAULT '-',
    status SMALLINT NOT NULL DEFAULT 1, remark TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID, update_by UUID, deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_biz_seq_step CHECK (step > 0), CONSTRAINT ck_biz_seq_no CHECK (current_no >= 0)
);

CREATE TABLE biz_warehouse (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    warehouse_code VARCHAR(40) NOT NULL, warehouse_name VARCHAR(120) NOT NULL,
    warehouse_type VARCHAR(20) NOT NULL DEFAULT 'NORMAL', manager VARCHAR(50),
    allow_negative SMALLINT NOT NULL DEFAULT 0, address VARCHAR(255), status SMALLINT NOT NULL DEFAULT 1,
    sort_order INT NOT NULL DEFAULT 0, remark TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID, update_by UUID, deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_biz_warehouse_type CHECK (warehouse_type IN ('NORMAL','RAW','FINISHED','SCRAP','IN_TRANSIT')),
    CONSTRAINT ck_biz_warehouse_neg  CHECK (allow_negative IN (0, 1))
);

CREATE TABLE biz_material (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_code VARCHAR(40) NOT NULL, material_name VARCHAR(120) NOT NULL, spec VARCHAR(200),
    unit_id UUID NOT NULL, material_type VARCHAR(20) NOT NULL DEFAULT 'RAW',
    default_warehouse_id UUID, safety_stock NUMERIC(18,4) NOT NULL DEFAULT 0,
    costing_method VARCHAR(20) NOT NULL DEFAULT 'MOVING_AVG', inventory_account_id UUID,
    tax_rate NUMERIC(7,4) NOT NULL DEFAULT 0, status SMALLINT NOT NULL DEFAULT 1, remark TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID, update_by UUID, deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_biz_material_type    CHECK (material_type IN ('RAW','SEMI','FINISHED','SERVICE')),
    CONSTRAINT ck_biz_material_costing CHECK (costing_method IN ('MOVING_AVG','FIFO','STANDARD')),
    CONSTRAINT ck_biz_material_safety  CHECK (safety_stock >= 0),
    CONSTRAINT ck_biz_material_tax     CHECK (tax_rate >= 0 AND tax_rate <= 1)
);

CREATE TABLE biz_inventory (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_id UUID NOT NULL, warehouse_id UUID NOT NULL,
    qty NUMERIC(18,4) NOT NULL DEFAULT 0, locked_qty NUMERIC(18,4) NOT NULL DEFAULT 0,
    avg_cost NUMERIC(18,6) NOT NULL DEFAULT 0, amount NUMERIC(18,4) NOT NULL DEFAULT 0,
    last_txn_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_inv_locked CHECK (locked_qty >= 0)
);

CREATE TABLE biz_inventory_txn (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_id UUID NOT NULL, warehouse_id UUID NOT NULL,
    txn_type VARCHAR(8) NOT NULL, biz_type VARCHAR(24) NOT NULL, doc_type VARCHAR(24) NOT NULL,
    doc_id UUID NOT NULL, doc_no VARCHAR(32) NOT NULL, line_no INT NOT NULL DEFAULT 1,
    qty NUMERIC(18,4) NOT NULL, unit_cost NUMERIC(18,6) NOT NULL DEFAULT 0, amount NUMERIC(18,4) NOT NULL DEFAULT 0,
    before_qty NUMERIC(18,4) NOT NULL DEFAULT 0, before_amount NUMERIC(18,4) NOT NULL DEFAULT 0,
    before_cost NUMERIC(18,6) NOT NULL DEFAULT 0, after_qty NUMERIC(18,4) NOT NULL DEFAULT 0,
    after_amount NUMERIC(18,4) NOT NULL DEFAULT 0, after_cost NUMERIC(18,6) NOT NULL DEFAULT 0,
    period_code VARCHAR(7) NOT NULL, operator_id UUID, remark VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_inv_txn_type CHECK (txn_type IN ('IN','OUT')),
    CONSTRAINT ck_biz_inv_txn_qty  CHECK (qty > 0),
    CONSTRAINT ck_biz_inv_txn_amt  CHECK (amount >= 0)
);

-- 存量仓库一行：无 warehouse_path，靠 V0137 的 UPDATE 回填
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name)
VALUES ('aaaaaaaa-0000-4000-8000-000000000001', 'LEGACY-01', '存量仓库（批次1 时代建的）');
