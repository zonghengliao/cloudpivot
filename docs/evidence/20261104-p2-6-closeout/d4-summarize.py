import json
from collections import Counter

d = json.load(open('/tmp/d4-out/03-full.json'))
print('keys=', list(d.keys()))
print('summary=', d.get('summary'))
rows = d.get('issues') or d.get('rows') or d.get('findings') or []
print('rows=', len(rows))
if rows:
    print('by_code=', sorted(Counter(r['issue_code'] for r in rows).items()))
    print('by_code_sev=', sorted(Counter((r['issue_code'], r['severity']) for r in rows).items()))
