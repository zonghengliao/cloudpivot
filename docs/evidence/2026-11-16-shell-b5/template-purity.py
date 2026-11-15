#!/usr/bin/env python3
"""模板纯净性断言（B5 卡 §五.5）：`git diff -U0 -- '*.vue'` 里

判定规则：
  · 成对的 `-`/`+` 行（同一 hunk 内按位置配对）→ 去掉追加的 token 后必须与 `-` 行逐字节相同
    （即模板改动**只**是 class 里追加 token，DOM 结构与其它属性一字不动）；
  · 只有 `-` 没有 `+` 的行 → 必须落在该文件 HEAD 版本的 `<style>...</style>` 区间内（只删样式）；
  · 只有 `+` 没有 `-` 的行 → 只允许出现在「白名单页的本地样式补声明」(hr/salary/index) 与
    「页面自有类承接」(storage/file 的 .preview__icon) 两类，且必须在 <style> 段内。

用法： python3 template-purity.py [file ...]
"""
import json
import re
import subprocess
import sys

REPO = '/opt/cloudpivot'
TOKENS = re.compile(r'\b(?:panel-empty--[a-z]+|panel-empty__icon--[a-z]+|tool-row__label--sm|preview__icon)\b')
STYLE_ONLY_ADD_OK = {
    'frontend/src/views/business/hr/salary/index.vue',   # 白名单页：2 条显式声明抵住新基线
    'frontend/src/views/business/storage/file/index.vue',  # 页面自有类承接抽屉图标
}


def sh(args):
    return subprocess.run(args, cwd=REPO, capture_output=True, text=True).stdout


def style_ranges(text):
    out = []
    for m in re.finditer(r'<style\b[^>]*>([\s\S]*?)</style>', text):
        out.append((text[:m.start()].count('\n') + 1, text[:m.end()].count('\n') + 1))
    return out


def norm(s):
    return re.sub(r'\s+', ' ', s).strip()


def main(paths):
    if not paths:
        d = json.load(open('/opt/cloudpivot/docs/evidence/2026-11-16-shell-b5/report-before.json'))
        paths = sorted({'frontend/src/' + b['file'] for b in d['batches']['B5']['detail']})
    bad = plus_total = minus_total = 0
    paired = 0
    for p in paths:
        before = sh(['git', 'show', 'HEAD:' + p])
        ranges = style_ranges(before)
        in_style = lambda ln: any(a <= ln <= b for a, b in ranges)
        diff = sh(['git', 'diff', '-U0', '--', p])
        old_ln = 0
        minus, plus = [], []   # [(lineno, text)]
        def flush():
            nonlocal bad, paired
            for i in range(max(len(minus), len(plus))):
                if i < len(minus) and i < len(plus):
                    ln, old = minus[i]
                    new = plus[i][1]
                    paired += 1
                    stripped = re.sub(r'\s+"', '"', norm(TOKENS.sub('', new)))
                    if stripped != norm(old):
                        bad += 1
                        print(f'✗ {p}:{ln} 追加 token 之外还有别的改动：\n    - {old.strip()[:110]}\n    + {new.strip()[:110]}')
                elif i < len(minus):
                    ln, old = minus[i]
                    if old.strip() and not in_style(ln):
                        bad += 1
                        print(f'✗ {p}:{ln} 纯删除行不在 <style> 段内：{old.strip()[:110]!r}')
                else:
                    ln, new = plus[i]
                    if p not in STYLE_ONLY_ADD_OK or not in_style(ln):
                        bad += 1
                        print(f'✗ {p}:{ln} 纯新增行（模板/DOM/style 都不该出现）：{new.strip()[:110]!r}')
            minus.clear()
            plus.clear()
        for line in diff.split('\n'):
            m = re.match(r'^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@', line)
            if m:
                flush()
                old_ln = int(m.group(1))
                continue
            if line.startswith('---') or line.startswith('+++') or not line:
                continue
            if line.startswith('-'):
                minus.append((old_ln, line[1:]))
                minus_total += 1
                old_ln += 1
            elif line.startswith('+'):
                plus.append((old_ln, line[1:]))
                plus_total += 1
        flush()
    print(f'\n检查 {len(paths)} 个 .vue：成对改动 {paired} 组 / 删除 {minus_total} 行 / 新增 {plus_total} 行；违规 {bad} 处')
    return 0 if bad == 0 else 1


sys.exit(main(sys.argv[1:]))
