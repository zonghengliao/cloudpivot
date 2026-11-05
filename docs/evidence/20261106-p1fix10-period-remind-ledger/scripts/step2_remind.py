#!/usr/bin/env python3
"""P1-FIX-10 步骤 2：修复前基线 —— 催办是否即时发信 / 台账是否写。

用法: python3 step2_before.py <tag>
"""
import json
import os
import subprocess
import sys

sys.path.insert(0, '/root/r10-run')
from apilib import Api

TAG = sys.argv[1] if len(sys.argv) > 1 else 'before'
PERIOD_ID = '64fa6ba3-f3f7-1071-d22f-dab5ed2e6fb1'
OUT = []


def log(title, payload):
    line = f"### {title}\n{json.dumps(payload, ensure_ascii=False, indent=2) if not isinstance(payload, str) else payload}"
    print(line)
    OUT.append(line)


def psql(sql):
    res = subprocess.run(['psql', 'postgres://alun:alun@127.0.0.1:5432/cloudpivot_r10', '-Atc', sql],
                         capture_output=True, text=True)
    return res.stdout.strip() or res.stderr.strip()


a = Api()
a.login('admin', os.environ['R10_ADMIN_PASSWORD'])

check = a.ok('GET', f'/business/erp/period/{PERIOD_ID}/close-check')
log('1) close-check blockers', [
    {'kind': b.get('kind'), 'count': len(b.get('items') or []),
     'items': [{'doc_type': i.get('doc_type'), 'doc_no': i.get('doc_no'),
                'owner_role': i.get('owner_role'), 'owner_name': i.get('owner_name')}
               for i in (b.get('items') or [])]}
    for b in check.get('blockers', [])
])
log('2) warnings 条数', len(check.get('warnings') or []))

log('3) 催办前 sys_notice(period_remind) 行数', psql(
    "select count(*) from sys_notice where biz_type='period_remind'"))
log('4) 催办前 biz_reminder_log 行数', psql("select count(*) from biz_reminder_log"))

r1 = a.call('POST', f'/business/erp/period/{PERIOD_ID}/remind', {'item_ids': []})
log('5) 第 1 次催办（HTTP 应答）', r1)
r2 = a.call('POST', f'/business/erp/period/{PERIOD_ID}/remind', {'item_ids': []})
log('6) 第 2 次催办（24h 内重复）', r2)

log('7) 催办后 sys_notice(period_remind) 行数', psql(
    "select count(*) from sys_notice where biz_type='period_remind'"))
log('8) 催办后 sys_notice 明细', psql(
    "select notice_no||'|'||title||'|'||biz_type||'|'||biz_id||'|'||receiver_count||'|'||created_at "
    "from sys_notice where biz_type='period_remind' order by created_at desc limit 5"))
log('9) 催办后 biz_reminder_log 行数', psql("select count(*) from biz_reminder_log"))
log('10) 催办后 biz_reminder_log 明细', psql(
    "select coalesce(doc_type,'-')||'|'||coalesce(doc_no,'-')||'|'||remind_kind||'|'||stage||'|'||"
    "remind_date||'|'||coalesce(receiver_count::text,'-') from biz_reminder_log order by sent_at desc limit 5"))

with open(f'/root/r10-run/step2_{TAG}.txt', 'w') as fh:
    fh.write('\n'.join(OUT) + '\n')
