#!/usr/bin/env python3
"""HOME-5 证据：真 token 调真实接口（隔离实例 18097 / 库 cloudpivot_home5_v4）。

输出：docs/evidence/home5-20261109/api/api-evidence.txt（token 打码，只留前 8 位）
覆盖：待办列表（biz_type/biz_no/form_data.amount 随行下发）、今日已办 since 过滤、
      驳回意见 < 5 字被后端拦下（400 + 65005）、同意 / 加签真动作回执。
"""
import json
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone

BASE = 'http://127.0.0.1:18097'
TOKEN = open('/var/tmp/home5/api/admin.token').read().strip()
MASK = TOKEN[:8] + '…' + TOKEN[-4:]
OUT = '/opt/cloudpivot/docs/evidence/home5-20261109/api/api-evidence.txt'
lines = []


def log(text=''):
    print(text)
    lines.append(text)


def call(method, path, payload=None):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(
        BASE + path,
        data=data,
        headers={'Authorization': 'Bearer ' + TOKEN, 'Content-Type': 'application/json'},
        method=method,
    )
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            return resp.status, json.loads(resp.read())
    except urllib.error.HTTPError as err:
        body = err.read().decode()
        try:
            return err.code, json.loads(body)
        except json.JSONDecodeError:
            return err.code, {'raw': body}


def brief(payload, limit=900):
    text = json.dumps(payload, ensure_ascii=False)
    return text if len(text) <= limit else text[:limit] + '…（截断）'


log('# HOME-5 待我审批 · 真实接口实测（真 token，取自我方登录会话）')
log(f'# 时间：{datetime.now().astimezone().isoformat(timespec="seconds")}')
log(f'# 实例：{BASE}（profile=test，库 cloudpivot_home5_v4）')
log(f'# Authorization: Bearer {MASK}')
log()

status, todo = call('GET', '/api/business/flow/task/todo?page=1&page_size=10')
log(f'$ GET {BASE}/api/business/flow/task/todo?page=1&page_size=10')
log(f'<- HTTP {status}  total={todo["data"]["total"]}  returned={len(todo["data"]["list"])}')
first = todo['data']['list'][0]
log('   首行字段（新增的 biz_* 与 form_data.amount 都在）：')
log('   ' + brief({
    'biz_type': first['biz_type'],
    'biz_no': first['biz_no'],
    'biz_id': first['biz_id'],
    'title': first['title'],
    'initiator_name': first['initiator_name'],
    'arrived_at': first['arrived_at'],
    'amount': (first.get('form_data') or {}).get('amount'),
}))
log()

tz = timezone(timedelta(hours=8))
today_start = datetime.now(tz).replace(hour=0, minute=0, second=0, microsecond=0).astimezone(timezone.utc)
since = today_start.isoformat(timespec='milliseconds').replace('+00:00', 'Z')
status2, done_today = call('GET', f'/api/business/flow/task/done?page=1&page_size=1&since={since}')
status3, done_all = call('GET', '/api/business/flow/task/done?page=1&page_size=1')
log(f'$ GET /api/business/flow/task/done?page=1&page_size=1&since={since}')
log(f'<- HTTP {status2}  total={done_today["data"]["total"]}   ← 今日已办（卡片底部 M）')
log(f'$ GET /api/business/flow/task/done?page=1&page_size=1')
log(f'<- HTTP {status3}  total={done_all["data"]["total"]}   ← 全量已办（不传 since；两者必须不同才对）')
log()

# 驳回意见过短 → 后端硬拦（400 + 65005）
reject_target = todo['data']['list'][1]
status4, rejected = call('POST', f'/api/business/flow/task/{reject_target["id"]}/reject', {'comment': '太短'})
log(f'$ POST /api/business/flow/task/{reject_target["id"]}/reject  {{"comment":"太短"}}')
log(f'<- HTTP {status4}  code={rejected.get("code")}  msg={rejected.get("msg")}   ← 后端也要求 ≥ 5 字')
log()

# 同意一张（真动作）
approve_target = todo['data']['list'][2]
status5, approved = call('POST', f'/api/business/flow/task/{approve_target["id"]}/approve', {'comment': '验收：同意'})
log(f'$ POST /api/business/flow/task/{approve_target["id"]}/approve')
log(f'<- HTTP {status5}  {brief(approved)}')
log()

# 加签一个（真动作：加给 h5viewer）
sign_target = todo['data']['list'][3]
status6, ok_page = call('GET', '/api/system/user/page?page=1&page_size=20&username=h5clerk')
signer_id = ok_page['data']['list'][0]['id']
status7, signed = call('POST', f'/api/business/flow/task/{sign_target["id"]}/add-sign', {'user_ids': [signer_id]})
log(f'$ POST /api/business/flow/task/{sign_target["id"]}/add-sign  {{"user_ids":["{signer_id}"]}}')
log(f'<- HTTP {status7}  {brief(signed)}')
log()

status8, todo_after = call('GET', '/api/business/flow/task/todo?page=1&page_size=10')
status9, done_after = call('GET', f'/api/business/flow/task/done?page=1&page_size=1&since={since}')
log(f'$ GET /api/business/flow/task/todo?page=1&page_size=10')
log(f'<- HTTP {status8}  total={todo_after["data"]["total"]}（该单同意后还有下一节点 → 任务重新派回本人，总数可能不变；末节点则 -1，见 UI 验收 ④）')
log(f'$ GET /api/business/flow/task/done?page=1&page_size=1&since={since}')
log(f'<- HTTP {status9}  total={done_after["data"]["total"]}（今日已办：刚同意的那张算今天）')
log()
log('## since 过滤的对照组（把一张 5 天前就已办的任务留在库里，证明「今日」不是「全量」）')
status10, done_all2 = call('GET', '/api/business/flow/task/done?page=1&page_size=1')
log(f'$ GET /api/business/flow/task/done?page=1&page_size=1')
log(f'<- HTTP {status10}  total={done_all2["data"]["total"]}   ← 全量已办（含 5 天前那张）')

with open(OUT, 'w') as fh:
    fh.write('\n'.join(lines) + '\n')
print('\nwritten ->', OUT)
