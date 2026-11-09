#!/usr/bin/env python3
"""把「今日已办 vs 全量已办」的对照组补进证据（库里已有一张 5 天前办结的任务）。"""
import json
import urllib.request
from datetime import datetime, timedelta, timezone

BASE = 'http://127.0.0.1:18097'
TOKEN = open('/var/tmp/home5/api/admin.token').read().strip()
OUT = '/opt/cloudpivot/docs/evidence/home5-20261109/api/api-evidence.txt'


def get(path):
    req = urllib.request.Request(BASE + path, headers={'Authorization': 'Bearer ' + TOKEN})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return resp.status, json.loads(resp.read())


tz = timezone(timedelta(hours=8))
since = (
    datetime.now(tz)
    .replace(hour=0, minute=0, second=0, microsecond=0)
    .astimezone(timezone.utc)
    .isoformat(timespec='milliseconds')
    .replace('+00:00', 'Z')
)
status1, today = get(f'/api/business/flow/task/done?page=1&page_size=1&since={since}')
status2, alltime = get('/api/business/flow/task/done?page=1&page_size=1')

lines = [
    '',
    '## 补：since 过滤对照（把一张 5 天前办结的任务留在库里，验「今日」≠「全量」）',
    f'$ GET /api/business/flow/task/done?page=1&page_size=1&since={since}   → HTTP {status1} total={today["data"]["total"]}（今日）',
    f'$ GET /api/business/flow/task/done?page=1&page_size=1                → HTTP {status2} total={alltime["data"]["total"]}（全量，含 5 天前那张）',
]
with open(OUT, 'a') as fh:
    fh.write('\n'.join(lines) + '\n')
print('\n'.join(lines))
