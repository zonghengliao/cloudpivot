import json
import urllib.request

BASE = 'http://127.0.0.1:18097'
token = open('/var/tmp/home5/api/admin.token').read().strip()


def get(path):
    req = urllib.request.Request(BASE + path, headers={'Authorization': 'Bearer ' + token})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())


def post(path, payload):
    req = urllib.request.Request(
        BASE + path,
        data=json.dumps(payload).encode(),
        headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'},
        method='POST',
    )
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as err:
        return {'http': err.code, 'body': err.read().decode()}


todo = get('/api/business/flow/task/todo?page=1&page_size=50')['data']
target = next(row for row in todo['list'] if row['biz_no'] == 'CT-20261101-000003')
print('approve', target['id'], target['biz_no'], target['current_node_name'])
res = post('/api/business/flow/task/%s/approve' % target['id'], {'comment': '验收演示：先批一张，用于验「今日已办」计数'})
print('result', res)
print('done today', get('/api/business/flow/task/done?page=1&page_size=1&since=2026-11-08T16:00:00.000Z')['data']['total'])
print('todo total', get('/api/business/flow/task/todo?page=1&page_size=10')['data']['total'])
