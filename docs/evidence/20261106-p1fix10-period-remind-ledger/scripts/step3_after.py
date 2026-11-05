#!/usr/bin/env python3
"""P1-FIX-10 步骤 3：修复后验收（真 HTTP）。

用法: python3 step3_after.py <tag>
验收点：
  A 点催办立即发信（sys_notice 记录 + HTTP 响应）
  B biz_reminder_log 有记录（8 列逐列核对）
  C 24h 内重复催办被拦（第二次 skipped_recent）
  D 台账单独就能拦（把通知回拨 25h 后仍然拦得住 —— 证明不依赖通知表/清理任务）
"""
import json
import os
import subprocess
import sys
from datetime import datetime

sys.path.insert(0, '/root/r10-run')
from apilib import Api

TAG = sys.argv[1] if len(sys.argv) > 1 else 'after'
PERIOD_CODE = sys.argv[2] if len(sys.argv) > 2 else '2026-12'
VOUCHER_NO = sys.argv[3] if len(sys.argv) > 3 else 'PZ-R10-0002'
OUT = []


def log(title, payload):
    line = f"### {title}\n{json.dumps(payload, ensure_ascii=False, indent=2) if not isinstance(payload, str) else payload}"
    print(line)
    OUT.append(line)


def psql(sql):
    res = subprocess.run(['psql', 'postgres://alun:alun@127.0.0.1:5432/cloudpivot_r10', '-Atc', sql],
                         capture_output=True, text=True)
    return (res.stdout.strip() or res.stderr.strip())


# ---------- 夹具：2026-12 再造一张草稿凭证 ----------
uid = psql("select id from sys_user where username='admin'")
sql = f"""
WITH v AS (
  INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, voucher_type,
                           source_type, summary, total_debit, total_credit, entry_count, status,
                           create_by, update_by)
  SELECT '记', '{VOUCHER_NO}', '{PERIOD_CODE}', DATE '{PERIOD_CODE}-10', 'MANUAL', 'MANUAL',
         'R10 结账催办夹具（修复后）', 200, 200, 2, 'DRAFT', '{uid}', '{uid}'
  ON CONFLICT DO NOTHING
  RETURNING id
)
SELECT id FROM v
"""
psql(sql)  # 已存在时 ON CONFLICT DO NOTHING，重复执行无副作用
vid = psql(f"select id from biz_voucher where voucher_no='{VOUCHER_NO}' and deleted_at is null")
log('0) 夹具草稿凭证', {'voucher_id': vid})

if vid:
    psql(f"""
    INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount)
    SELECT '{vid}'::uuid, 1, '借方', a.id, '1002', a.account_name, 200
      FROM biz_account a WHERE a.account_code='1002' AND a.deleted_at IS NULL
    ON CONFLICT (voucher_id, line_no) DO NOTHING;
    INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, credit_amount)
    SELECT '{vid}'::uuid, 2, '贷方', a.id, '3001', a.account_name, 200
      FROM biz_account a WHERE a.account_code='3001' AND a.deleted_at IS NULL
    ON CONFLICT (voucher_id, line_no) DO NOTHING;
    """)

pid = psql(f"select id from biz_accounting_period where period_code='{PERIOD_CODE}'")
a = Api()
a.login('admin', os.environ['R10_ADMIN_PASSWORD'])

if not pid:
    # 第二轮换一个没开账的期间：走真实开账接口（幂等：已存在时 409 → 忽略）
    year, month = PERIOD_CODE.split('-')
    opened = a.call('POST', '/business/erp/period',
                    {'period_year': int(year), 'period_month': int(month), 'remark': 'R10 验收开账'})
    log('0b) 开账（新期间）', opened)
    pid = psql(f"select id from biz_accounting_period where period_code='{PERIOD_CODE}'")

log('1) 迁移版本（V0257 是否已应用）', psql(
    "select name from schema_migrations where name like 'erp_period_remind_ledger%'"))
log('2) biz_reminder_log 值域（CHECK）', psql(
    "select conname||' :: '||pg_get_constraintdef(oid) from pg_constraint "
    "where conname in ('ck_biz_reminder_log_doc_type','ck_biz_reminder_log_kind')"))

check = a.ok('GET', f'/business/erp/period/{pid}/close-check')
items = [i for b in check.get('blockers', []) for i in (b.get('items') or [])]
log(f'3) 挡路明细（{PERIOD_CODE}）', [{'doc_type': i['doc_type'], 'doc_no': i['doc_no'],
                              'owner_role': i.get('owner_role'), 'owner_name': i.get('owner_name')}
                             for i in items])

log('4) 催办前 台账/通知 计数', {
    'biz_reminder_log': psql(f"select count(*) from biz_reminder_log where doc_id='{vid}'"),
    'sys_notice': psql(f"select count(*) from sys_notice where biz_id='{vid}'"),
})

t0 = datetime.now()
r1 = a.call('POST', f'/business/erp/period/{pid}/remind', {'item_ids': []})
t1 = datetime.now()
log('5) 第 1 次催办（HTTP 应答）', r1)
log('5b) 端到端耗时', {'seconds': round((t1 - t0).total_seconds(), 3),
                   '说明': '同步发信：应答返回时通知与台账都已落库'})

log('6) sys_notice 记录（点催办立即发信的证据）', psql(
    f"select notice_no||' | '||title||' | biz_type='||biz_type||' | biz_id='||biz_id||"
    f"' | receivers='||receiver_count||' | created_at='||created_at "
    f"from sys_notice where biz_id='{vid}' order by created_at desc limit 3"))
log('7) sys_notice_receiver（收件人）', psql(
    f"select u.username||' | read='||nri.read_flag from sys_notice_receiver nri "
    f"join sys_user u on u.id=nri.user_id "
    f"join sys_notice n on n.id=nri.notice_id where n.biz_id='{vid}'"))

log('8) biz_reminder_log 记录（逐列）', psql(
    f"select 'doc_type='||doc_type||' | doc_id='||doc_id||' | doc_no='||coalesce(doc_no,'-')||"
    f"' | remind_kind='||remind_kind||' | stage='||stage||' | remind_date='||remind_date||"
    f"' | notice_id='||coalesce(notice_id::text,'-')||' | receiver_count='||receiver_count||"
    f"' | sent_at='||sent_at from biz_reminder_log where doc_id='{vid}'"))

r2 = a.call('POST', f'/business/erp/period/{pid}/remind', {'item_ids': []})
log('9) 第 2 次催办（24h 内重复，应被拦）', r2)
log('9b) 拦下后 台账/通知 计数（不该增长）', {
    'biz_reminder_log': psql(f"select count(*) from biz_reminder_log where doc_id='{vid}'"),
    'sys_notice': psql(f"select count(*) from sys_notice where biz_id='{vid}'"),
})

# ---------- D 台账 durability：通知回拨 25h（模拟过期清理），只剩台账 ----------
psql(f"update sys_notice set created_at = now() - interval '25 hours' where biz_id='{vid}'")
log('10) 把通知回拨 25 小时（通知层判据失效）', psql(
    f"select count(*)||' 条通知 created_at > now()-24h' from sys_notice "
    f"where biz_id='{vid}' and created_at > now() - interval '24 hours'"))
r3 = a.call('POST', f'/business/erp/period/{pid}/remind', {'item_ids': []})
log('11) 第 3 次催办（只剩台账判据，应仍被拦）', r3)
log('12) 台账计数（仍 1 行）', psql(
    f"select count(*) from biz_reminder_log where doc_id='{vid}' and remind_kind='PERIOD_REMIND'"))

def grep_count(pattern, paths):
    res = subprocess.run(['grep', '-c', pattern] + paths, capture_output=True, text=True)
    return res.stdout.strip().replace('\n', ' ; ')


log('13) 不依赖定时任务的静态证据', {
    'period_remind.rs / year_end_remind.rs 里的调度注册（register_builtin|register_cron|Scheduler）':
        grep_count('register_builtin\\|register_cron\\|Scheduler',
                   ['/opt/cloudpivot/backend/src/business/erp/period_remind.rs',
                    '/opt/cloudpivot/backend/src/business/erp/year_end_remind.rs']),
    '催办唯一入口（HTTP 同步端点）':
        'POST /business/erp/period/{id}/remind → period_remind_handler → dispatch_all → notice::dispatch',
    'sys_job 里与结账/期间有关的任务': psql(
        "select coalesce(string_agg(job_name||'(status='||status||')', ', '), '（无 —— 根本没有这个任务，也就无所谓停用）') "
        "from sys_job where deleted_at is null and (job_name like '%period%' or job_name like '%remind%' "
        "or job_name like '%close%')"),
    '隔离库 sys_job 全量': psql(
        "select string_agg(job_name||'(status='||status||')', ', ' order by job_name) from sys_job where deleted_at is null"),
})

with open(f'/root/r10-run/step3_{TAG}.txt', 'w') as fh:
    fh.write('\n'.join(OUT) + '\n')
