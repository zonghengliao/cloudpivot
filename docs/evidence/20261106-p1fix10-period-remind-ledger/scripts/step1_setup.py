#!/usr/bin/env python3
"""P1-FIX-10 步骤 1：admin 首登改密 + 造一张 2026-11 的草稿凭证（挡路项）。"""
import json
import os
import subprocess
import sys

sys.path.insert(0, '/root/r10-run')
from apilib import Api

OLD = os.environ['R10_ADMIN_PASSWORD_INITIAL']
NEW = os.environ['R10_ADMIN_PASSWORD']
OUT = []


def log(title, payload):
    line = f"### {title}\n{json.dumps(payload, ensure_ascii=False, indent=2) if not isinstance(payload, str) else payload}"
    print(line)
    OUT.append(line)


a = Api()
a.login('admin', OLD)
r = a.call('PUT', '/system/auth/password', {'old_password': OLD, 'new_password': NEW})
log('1) admin 首登改密', {'http': r['_http'], 'ok': r['_ok'], 'resp': r.get('_error') or 'OK'})

a = Api()
tok = a.login('admin', NEW)
log('2) admin 用新口令登录', {'must_change_password': tok.get('must_change_password'),
                        'user': tok.get('user')})

# 造挡路项：2026-11 一张 DRAFT 凭证（借方 1002 / 贷方 3001，各 100 元，平）
sql = """
WITH acc AS (
  SELECT
    (SELECT id FROM biz_account WHERE account_code='1002' AND deleted_at IS NULL) AS d_id,
    (SELECT account_name FROM biz_account WHERE account_code='1002' AND deleted_at IS NULL) AS d_name,
    (SELECT id FROM biz_account WHERE account_code='3001' AND deleted_at IS NULL) AS c_id,
    (SELECT account_name FROM biz_account WHERE account_code='3001' AND deleted_at IS NULL) AS c_name
), uid AS (SELECT id FROM sys_user WHERE username='admin')
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, summary, total_debit, total_credit, entry_count, status,
                         create_by, update_by)
SELECT gen_random_uuid(), '记', 'PZ-R10-0001', '2026-11', DATE '2026-11-10', 'MANUAL',
       'MANUAL', 'R10 结账催办夹具：草稿凭证', 100, 100, 2, 'DRAFT',
       (SELECT id FROM uid), (SELECT id FROM uid)
ON CONFLICT DO NOTHING
RETURNING id;
"""
res = subprocess.run(['psql', 'postgres://alun:alun@127.0.0.1:5432/cloudpivot_r10',
                      '-Atc', sql], capture_output=True, text=True)
log('3) 建草稿凭证', {'rc': res.returncode, 'out': res.stdout.strip(), 'err': res.stderr.strip()})
vid = res.stdout.strip().splitlines()[0] if res.stdout.strip() else ''

if vid:
    sql2 = f"""
    INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount)
    SELECT '{vid}'::uuid, 1, '夹具借方', a.id, '1002', a.account_name, 100
      FROM biz_account a WHERE a.account_code='1002' AND a.deleted_at IS NULL
    ON CONFLICT (voucher_id, line_no) DO NOTHING;
    INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, credit_amount)
    SELECT '{vid}'::uuid, 2, '夹具贷方', a.id, '3001', a.account_name, 100
      FROM biz_account a WHERE a.account_code='3001' AND a.deleted_at IS NULL
    ON CONFLICT (voucher_id, line_no) DO NOTHING;
    """
    res2 = subprocess.run(['psql', 'postgres://alun:alun@127.0.0.1:5432/cloudpivot_r10',
                           '-Atc', sql2], capture_output=True, text=True)
    log('4) 建分录', {'rc': res2.returncode, 'err': res2.stderr.strip()})

check = a.ok('GET', '/business/erp/period/page?page=1&page_size=30')
rows = check.get('list') or check.get('rows') or []
nov = [x for x in rows if (x.get('period_code') or x.get('period')) == '2026-11']
log('5) 2026-11 期间', nov)

with open('/root/r10-run/step1.txt', 'w') as fh:
    fh.write('\n'.join(OUT) + '\n')
