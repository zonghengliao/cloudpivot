"""收口D ②：生产复跑 改前 vs 改后 逐条对账（只用两份原始输出：证据基线 txt/json + 本次复跑 json/txt）"""
import json
import re
from collections import Counter, OrderedDict

OLD_TXT = '../20261104-p2-6/prod-snapshot-audit-full.txt'
old = json.load(open('../20261104-p2-6/prod-snapshot-audit-full.json'))
new = json.load(open('prod-full-new.json'))


def checkmap(doc):
    return OrderedDict((c['id'], c) for c in doc['checks'])


oc, nc = checkmap(old), checkmap(new)

print('### 1) 汇总对照')
print('| 口径 | 改前（%s） | 改后（本次复跑） |' % old['generated_at'])
o, n = old['summary'], new['summary']
for k in ('blocker', 'warn', 'info', 'checks_total', 'checks_passed', 'exit_code'):
    print('| %s | %s | %s |' % (k, o.get(k), n.get(k)))
print()

print('### 2) 有命中的检查项逐项对照')
print('| 检查项 | 改前 hit / 级别 | 改后 hit / 级别 | 行数变化 |')
print('|---|---|---|---|')
hit_ids = [i for i in oc if oc[i]['hit'] or nc.get(i, {}).get('hit')]
for i in hit_ids:
    a, b = oc[i], nc[i]
    print('| %s | %d %s | %d %s | %s |' % (
        i, a['hit'], a['by_severity'] or '-', b['hit'], b['by_severity'] or '-',
        '持平' if a['hit'] == b['hit'] else '变化 %+d' % (b['hit'] - a['hit'])))
print()

print('### 3) 总行数守恒')
print('改前命中行合计 =', sum(c['hit'] for c in old['checks']))
print('改后命中行合计 =', sum(c['hit'] for c in new['checks']))
print()

print('### 4) 两份 JSON 里已展开的行：id 集合对比（同一条 = entity_type+entity_id）')


def keys(doc):
    out = set()
    for c in doc['checks']:
        for r in c['rows']:
            out.add((c['id'], r['entity_type'], r['entity_id']))
    return out


ko, kn = keys(old), keys(new)
print('改前展开行 %d 条，改后展开行 %d 条' % (len(ko), len(kn)))
print('改后新增的行：', sorted(kn - ko) or '（无）')
print('改后消失的行：', sorted(ko - kn) or '（无）')
print()
print('### 5) 同一条目的 severity / snapshot_value 变化')
rows_o = {(c['id'], r['entity_type'], r['entity_id']): r for c in old['checks'] for r in c['rows']}
rows_n = {(c['id'], r['entity_type'], r['entity_id']): r for c in new['checks'] for r in c['rows']}
for k in sorted(rows_o, key=lambda x: (x[0], x[2])):
    if k not in rows_n:
        continue
    a, b = rows_o[k], rows_n[k]
    if a['severity'] != b['severity'] or a['snapshot_value'] != b['snapshot_value']:
        print('- %s %s (%s)' % (k[0], k[2], a['entity_no']))
        print('    改前: [%s] %s' % (a['severity'], a['snapshot_value']))
        print('    改后: [%s] %s' % (b['severity'], b['snapshot_value']))
print()

print('### 6) 基线 txt 里逐条列出的行 vs 本次复跑（文本层面对照）')
txt = open(OLD_TXT, encoding='utf-8').read()
listed = re.findall(r'▸ (\S+) ([0-9a-f\-]{36}) \(([^)]+)\)', txt)
print('基线 txt 展开的行数 =', len(listed))
for t, eid, no in listed:
    cid = None
    for c in old['checks']:
        for r in c['rows']:
            if r['entity_id'] == eid:
                cid = c['id']
    sev_new = None
    for c in new['checks']:
        for r in c['rows']:
            if r['entity_id'] == eid:
                sev_new = r['severity']
    print('- %s %s %s -> 本次仍在（severity=%s）' % (t, eid, no, sev_new))
