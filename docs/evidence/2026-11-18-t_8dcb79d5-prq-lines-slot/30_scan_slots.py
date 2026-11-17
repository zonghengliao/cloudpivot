#!/usr/bin/env python3
"""扫描所有 .vue：找出 <t-table> 内「不在该表 colKeys 集合里」的单元格插槽。

策略：
1) 表级解析：:columns="X" → X 是数组字面量 / 本文件同名数组（支持 `: Type = [`、别名 A = B、展开 ...OTHER）
2) 表级解析失败 → 回退「文件级 colKey 池」（本 .vue 的 script + 本地 import 模块的 colKey 全集），标记 POOL
3) 忽略 TDesign 内置插槽
"""
import re, pathlib

ROOT = pathlib.Path('/opt/cloudpivot/frontend/src')
INPUTS = ('t-select', 't-input-number', 't-date-picker')
BUILTIN = {'expandedRow', 'loading', 'empty', 'footer', 'default'}

def match_bracket(text, start):
    depth = 0
    i = start
    while i < len(text):
        if text[i] == '[':
            depth += 1
        elif text[i] == ']':
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return -1

def find_arrays(text):
    out = {}
    for m in re.finditer(r'(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]*?)?=\s*\[', text):
        name = m.group(1)
        pos = m.end() - 1
        end = match_bracket(text, pos)
        if end < 0:
            continue
        body = text[pos:end + 1]
        keys = re.findall(r"colKey\s*:\s*['\"]([^'\"]+)['\"]", body)
        spreads = re.findall(r'\.\.\.\s*([A-Za-z_$][\w$]*)', body)
        out.setdefault(name, {'keys': [], 'spreads': []})
        out[name]['keys'] = keys
        out[name]['spreads'] = spreads
    return out

def find_aliases(text):
    """const A = B;"""
    out = {}
    for m in re.finditer(r'(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]*?)?=\s*([A-Za-z_$][\w$]*)\s*[;\n]', text):
        out[m.group(1)] = m.group(2)
    return out

def imports_of(text, file):
    """返回 {localName: 绝对路径 或 None}"""
    out = {}
    for m in re.finditer(r'import\s+(?:type\s+)?\{([^}]*)\}\s+from\s+[\'"]([^\'"]+)[\'"]', text):
        names, src = m.group(1), m.group(2)
        if src.startswith('@/'):
            base = ROOT / src[2:]
        elif src.startswith('.'):
            base = file.parent / src
        else:
            continue
        cand = None
        for ext in ('', '.ts', '.vue', '/index.ts'):
            p = pathlib.Path(str(base) + ext)
            if p.is_file():
                cand = p
                break
        for n in names.split(','):
            n = n.strip()
            if not n:
                continue
            if ' as ' in n:
                n = n.split(' as ')[-1].strip()
            out[n] = cand
    return out

def script_bodies(text):
    return [text[m.end(): text.find('</script>', m.end())] for m in re.finditer(r'<script[^>]*>', text)]

def colkey_pool(text, file, depth=0):
    """文件级 colKey 全集（含本地 import 一层）"""
    pool = set()
    for b in script_bodies(text):
        pool |= set(re.findall(r"colKey\s*:\s*['\"]([^'\"]+)['\"]", b))
        pool |= set(re.findall(r"\bcolKey\s*:\s*['\"]([^'\"]+)['\"]", text))
    if depth < 2:
        for name, path in imports_of(text, file).items():
            if path and path.exists():
                try:
                    t2 = path.read_text(encoding='utf-8')
                except Exception:
                    continue
                pool |= colkey_pool(t2, path, depth + 1)
    return pool

_file_cache = {}

def _load(path):
    if str(path) not in _file_cache:
        t = path.read_text(encoding='utf-8')
        arrays, aliases = {}, {}
        for b in script_bodies(t) if path.suffix == '.vue' else [t]:
            arrays.update(find_arrays(b))
            aliases.update(find_aliases(b))
        _file_cache[str(path)] = (t, arrays, aliases, imports_of(t, path))
    return _file_cache[str(path)]

def reexports_of(path, text):
    """`export * from './x'` / `export { a, b } from './x'` 的目标模块"""
    out = []
    for m in re.finditer(r'export\s+(?:\*|\{[^}]*\})\s+from\s+[\'"]([^\'"]+)[\'"]', text):
        src = m.group(1)
        if src.startswith('@/'):
            base = ROOT / src[2:]
        elif src.startswith('.'):
            base = path.parent / src
        else:
            continue
        for ext in ('', '.ts', '/index.ts'):
            p = pathlib.Path(str(base) + ext)
            if p.is_file():
                out.append(p)
                break
    return out

def resolve(expr, file, arrays, aliases, pool_of_file=None, depth=0, importmap=None):
    expr = expr.strip()
    if expr.startswith('['):
        return re.findall(r"colKey\s*:\s*['\"]([^'\"]+)['\"]", expr)
    name = expr.split('.')[-1].strip()
    if name in arrays:
        d = arrays[name]
        keys = list(d['keys'])
        if depth < 3:
            for sp in d['spreads']:
                sub = resolve(sp, file, arrays, aliases, None, depth + 1, importmap)
                if sub:
                    keys += sub
        return keys
    if name in aliases:
        return resolve(aliases[name], file, arrays, aliases, None, depth + 1, importmap)
    # 从 import 的模块里找同名常量（含 `export *` 再导出）
    if depth < 3:
        targets = []
        if importmap and name in importmap and importmap[name]:
            targets.append(importmap[name])
        t, a, al, im = _load(file)
        targets += reexports_of(file, t)
        for p in targets:
            t2, a2, al2, im2 = _load(p)
            sub = resolve(name, p, a2, al2, None, depth + 1, im2)
            if sub:
                return sub
    return None

def parse_tables(text):
    pat = re.compile(r'<(/?)t-table\b')
    i = 0
    while True:
        m = pat.search(text, i)
        if not m:
            break
        if m.group(1):
            i = m.end()
            continue
        start = m.start()
        j = text.find('>', m.end())
        if j < 0:
            break
        if text[j - 1] == '/':
            yield start, j + 1, ''
            i = j + 1
            continue
        depth = 1
        k = j + 1
        inner_end = len(text)
        while depth and k < len(text):
            mm = pat.search(text, k)
            if not mm:
                break
            if mm.group(1):
                depth -= 1
                if depth == 0:
                    inner_end = mm.start()
                    k = text.find('>', mm.end()) + 1
                    break
            else:
                depth += 1
            k = mm.end()
        content = text[j + 1: inner_end]
        cleaned = re.sub(r'<t-table\b[^>]*>.*?</t-table>', '', content, flags=re.S)
        yield start, k, cleaned
        i = j + 1

hits, pool_hits, unresolved = [], [], []
pool_cache = {}
STATS = {'tables': 0, 'slots': 0, 'resolved': 0, 'pool': 0, 'input_hits': 0}

for f in sorted(ROOT.rglob('*.vue')):
    text = f.read_text(encoding='utf-8')
    bodies = script_bodies(text)
    arrays, aliases = {}, {}
    for b in bodies:
        arrays.update(find_arrays(b))
        aliases.update(find_aliases(b))
    for (s, e, content) in parse_tables(text):
        block = text[s:e]
        cm = re.search(r':columns\s*=\s*"([^"]+)"', block)
        if not cm:
            continue
        slots = [x for x in re.findall(r'<template\s+#([A-Za-z0-9_]+)=', content) if x not in BUILTIN]
        if not slots:
            continue
        STATS['tables'] += 1
        STATS['slots'] += len(slots)
        keys = resolve(cm.group(1), f, arrays, aliases, None, 0, imports_of(text, f))
        used_pool = False
        if keys is None:
            if str(f) not in pool_cache:
                pool_cache[str(f)] = colkey_pool(text, f)
            keys = sorted(pool_cache[str(f)])
            used_pool = True
            STATS['pool'] += 1
        else:
            STATS['resolved'] += 1
        bad = [x for x in slots if x not in keys]
        if not bad:
            continue
        detail = []
        for sname in bad:
            mm = re.search(r'<template\s+#' + re.escape(sname) + r'=[^>]*>', content)
            seg = content[mm.end(): content.find('</template>', mm.end())] if mm else ''
            ctrls = [t for t in INPUTS if '<' + t in seg]
            if ctrls:
                STATS['input_hits'] += 1
            detail.append((sname, ctrls))
        row = (str(f), cm.group(1).strip(), keys, detail)
        (pool_hits if used_pool else hits).append(row)

def dump(title, rows):
    print(f'\n### {title}：{len(rows)}')
    for path, name, keys, detail in rows:
        print('=' * 74)
        print(f'{path}  columns={name}')
        ks = keys if len(keys) < 25 else keys[:25] + ['…共%d' % len(keys)]
        print(f'  colKeys({len(keys)}) = {ks}')
        for sname, ctrls in detail:
            print(f'  BAD #{sname:24s} {"INPUT " + str(ctrls) if ctrls else "format-only"}')

print(f'扫过 {STATS["tables"]} 张「有自定义插槽的 t-table」/ {STATS["slots"]} 个单元格插槽；'
      f'其中 {STATS["resolved"]} 张按表级解析出 colKey、{STATS["pool"]} 张回退文件级池')
print(f'带输入控件且插槽名≠colKey 的槽子：{STATS["input_hits"]} 个')
dump('表级解析命中（确定错误）', hits)
dump('文件级池回退命中（需人工确认）', pool_hits)
