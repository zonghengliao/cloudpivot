"""逐条对比：改前口径（生产只读重放）vs 改后口径（线上视图）的 DI-19 命中行"""
import sys
from collections import Counter


def parse(path):
    rows = {}
    for line in open(path, encoding='utf-8'):
        if not line.startswith(' DI-19'):
            continue
        c = [x.strip() for x in line.split('|')]
        rows[(c[1], c[2])] = {'entity_no': c[3], 'field': c[4], 'snapshot': c[5],
                              'truth': c[6], 'severity': c[9]}
    return rows


old = parse('old-di19-rows.tsv')
new = parse('new-di19-rows.tsv')
print('改前口径命中行 =', len(old))
print('改后口径命中行 =', len(new))
print('改前 - 改后 =', len(set(old) - set(new)), sorted(set(old) - set(new))[:5])
print('改后 - 改前 =', len(set(new) - set(old)), sorted(set(new) - set(old))[:5])
print('entity_id 集合完全一致:', set(old) == set(new))
print()
print('改前 severity 分布 =', Counter(v['severity'] for v in old.values()))
print('改后 severity 分布 =', Counter(v['severity'] for v in new.values()))
print()
print('逐条 severity 变化（仅列出变化行）：')
n = 0
for k in sorted(old, key=lambda x: old[x]['entity_no']):
    if old[k]['severity'] != new[k]['severity']:
        n += 1
        print('  %s %s: %s -> %s' % (k[1], old[k]['entity_no'], old[k]['severity'], new[k]['severity']))
print('变化行数 =', n, '（其余 %d 行级别不变）' % (len(old) - n))
print()
from collections import Counter as _C
print('改后 snapshot_value 分布 =', dict(_C(v['snapshot'] for v in new.values())))
