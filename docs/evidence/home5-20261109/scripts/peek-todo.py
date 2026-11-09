import json
import sys
import urllib.request

token = open('/var/tmp/home5/api/admin.token').read().strip()
url = sys.argv[1] if len(sys.argv) > 1 else 'http://127.0.0.1:18097/api/business/flow/task/todo?page=1&page_size=10'
req = urllib.request.Request(url, headers={'Authorization': 'Bearer ' + token})
with urllib.request.urlopen(req, timeout=15) as resp:
    payload = json.loads(resp.read())
data = payload['data']
print('total', data['total'], 'len', len(data['list']))
for row in data['list'][:15]:
    print(row['biz_type'], '|', row['biz_no'], '|', row['title'], '|', row['initiator_name'], '|', row['arrived_at'], '| amount=', (row.get('form_data') or {}).get('amount'))
