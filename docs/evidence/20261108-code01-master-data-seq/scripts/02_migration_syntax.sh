#!/bin/bash
# V0337 迁移的语法 + 幂等自检（不依赖应用）：搭一个只有 biz_seq 的临时库
set -u
DB=code01_syntax
su - postgres -c "psql -Atc \"drop database if exists $DB\"" >/dev/null 2>&1
su - postgres -c "createdb -O alun $DB" || exit 1

PSQL="psql -h 127.0.0.1 -U alun -d $DB -v ON_ERROR_STOP=1"
export PGPASSWORD=alun

# 按 V0137 §1.1 的真实 DDL 建 biz_seq（含部分唯一索引与 CHECK 白名单）
$PSQL -q <<'SQL' || exit 1
CREATE TABLE biz_seq (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    seq_type     VARCHAR(32) NOT NULL,
    seq_name     VARCHAR(50) NOT NULL,
    prefix       VARCHAR(12) NOT NULL,
    date_pattern VARCHAR(8)  NOT NULL DEFAULT 'yyyyMMdd',
    period_key   VARCHAR(8)  NOT NULL DEFAULT '',
    current_no   BIGINT      NOT NULL DEFAULT 0,
    step         INT         NOT NULL DEFAULT 1,
    separator    VARCHAR(4)  NOT NULL DEFAULT '-',
    status       SMALLINT    NOT NULL DEFAULT 1,
    remark       TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by    UUID,
    update_by    UUID,
    deleted_at   TIMESTAMPTZ,
    CONSTRAINT ck_biz_seq_step CHECK (step > 0),
    CONSTRAINT ck_biz_seq_no   CHECK (current_no >= 0),
    CONSTRAINT ck_biz_seq_date CHECK (date_pattern IN ('yyyyMMdd','yyyyMM','none'))
);
CREATE UNIQUE INDEX uk_biz_seq_type ON biz_seq (seq_type) WHERE deleted_at IS NULL;
INSERT INTO biz_seq (seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status)
VALUES ('PO','采购订单号','PO','yyyyMMdd','',0,1,'-',1);
SQL

echo '──── 第一遍 up ────'
$PSQL -q -f /root/code01-src2/backend/migrations/business/V0337__master_data_seq.up.sql || exit 1
echo "行数（六类）= $($PSQL -Atc "select count(*) from biz_seq where seq_type in ('SUP','CUS','MAT','WH','AC','ET') and deleted_at is null")"

echo '──── 第二遍 up（幂等） ────'
$PSQL -q -f /root/code01-src2/backend/migrations/business/V0337__master_data_seq.up.sql || exit 1
echo "行数（六类，应仍为 6）= $($PSQL -Atc "select count(*) from biz_seq where seq_type in ('SUP','CUS','MAT','WH','AC','ET') and deleted_at is null")"

echo '──── 明细 ────'
$PSQL -c "select seq_type, prefix, date_pattern, period_key, current_no, step, separator, status, pad_width from biz_seq order by seq_type"

echo '──── down ────'
$PSQL -q -f /root/code01-src2/backend/migrations/business/V0337__master_data_seq.down.sql || exit 1
echo "行数（down 后，应剩 PO 一条）= $($PSQL -Atc "select count(*) from biz_seq")"
echo "pad_width 列（down 后应为 0）= $($PSQL -Atc "select count(*) from information_schema.columns where table_name='biz_seq' and column_name='pad_width'")"

echo '──── 再 up（down 后可重放） ────'
$PSQL -q -f /root/code01-src2/backend/migrations/business/V0337__master_data_seq.up.sql || exit 1
echo "行数（再 up 后，应为 7 = 6 主数据 + PO）= $($PSQL -Atc "select count(*) from biz_seq")"
