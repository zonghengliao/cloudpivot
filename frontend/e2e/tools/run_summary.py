"""把 `playwright test --reporter=list` 的日志解析成 `e2e-probe/run-summary.json`。

为什么要这一步：`docs/test-report-e2e.md` 是「逐页 6 项」的画面，但「整套跑了多少条、
哪几条红」是另一层事实（历史用例、跳过项）。把这两层都放进报告，读报告的人才能一眼
看出「是页面坏了，还是用例期望值过期了」。

用法：
    cd frontend
    E2E_BASE_URL=http://127.0.0.1:5189 pnpm test:e2e --reporter=list > /tmp/e2e-list.log 2>&1
    python3 e2e/tools/run_summary.py /tmp/e2e-list.log
    python3 e2e/tools/e2e_report.py
"""
import io
import json
import os
import re
import sys

OUT = '/opt/cloudpivot/frontend/e2e-probe/run-summary.json'

# 形如：  6 failed / 397 passed / 5 skipped (9.2m)
TAIL = re.compile(r'^\s+(\d+)\s+(passed|failed|skipped|did not run)\s*(?:\(([\d.]+)m?s?\))?\s*$')
# 形如：  1) [chromium] › e2e/08-t8b-task-tree.spec.ts:94:3 › T8-B …（标题）
FAIL = re.compile(r'^\s+(\d+)\)\s+\[chromium\]\s+›\s+(.+?)\s*$')


def main(log_path):
    counts = {'passed': 0, 'failed': 0, 'skipped': 0, 'did_not_run': 0, 'duration': ''}
    failures = []
    with io.open(log_path, encoding='utf-8', errors='replace') as fh:
        for raw in fh:
            line = raw.replace('\x00', '')
            m = TAIL.match(line)
            if m:
                key = m.group(2).replace(' did not run', '_did_not_run').replace(' ', '_')
                if key in counts:
                    counts[key] = int(m.group(1))
                if m.group(3):
                    counts['duration'] = '%sm' % m.group(3)
                continue
            m = FAIL.match(line)
            if m:
                title = re.sub(r'─+\s*$', '', m.group(2)).strip()
                failures.append(title)

    # 去重（失败清单在「首报」和「汇总」两处各出现一次）
    seen = set()
    uniq = []
    for f in failures:
        if f not in seen:
            seen.add(f)
            uniq.append(f)

    summary = {
        'counts': counts,
        'failures': uniq,
        'total': counts['passed'] + counts['failed'] + counts['skipped'],
    }
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    io.open(OUT, 'w', encoding='utf-8').write(json.dumps(summary, ensure_ascii=False, indent=2))
    print('written %s' % OUT)
    print(json.dumps({'counts': counts, 'failures': len(uniq)}, ensure_ascii=False))


if __name__ == '__main__':
    main(sys.argv[1] if len(sys.argv) > 1 else '/tmp/e2e-list.log')
