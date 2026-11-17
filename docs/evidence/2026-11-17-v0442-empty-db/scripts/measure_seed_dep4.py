#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""度量（口径 4）：护栏断言依赖检查（含 INSERT ... SELECT ... FROM (VALUES ...) 形态）。"""
import re
import sys
from pathlib import Path

BASE = Path('/opt/cloudpivot/backend/app/migrations')
VERSIONED_DIRS = ['system', 'support', 'business']
UUID = r'[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}'
UUID_ANY = re.compile(UUID, re.I)
INS_HEAD = re.compile(r'insert\s+into\s+([a-z_][a-z_0-9]*)\s*\(', re.I)


def split_statements(sql):
    out, buf, i, n, dollar = [], [], 0, len(sql), None
    while i < n:
        ch = sql[i]
        if dollar:
            if sql.startswith(dollar, i):
                buf.append(dollar); i += len(dollar); dollar = None; continue
            buf.append(ch); i += 1; continue
        if ch == '-' and sql.startswith('--', i):
            j = sql.find('\n', i); j = n if j < 0 else j
            buf.append(sql[i:j]); i = j; continue
        if ch == '/' and sql.startswith('/*', i):
            j = sql.find('*/', i + 2); j = n if j < 0 else j + 2
            buf.append(sql[i:j]); i = j; continue
        if ch == "'":
            j = i + 1
            while j < n:
                if sql[j] == "'":
                    if j + 1 < n and sql[j + 1] == "'":
                        j += 2; continue
                    break
                j += 1
            buf.append(sql[i:j + 1]); i = j + 1; continue
        if ch == '$':
            m = re.match(r'\$[A-Za-z_0-9]*\$', sql[i:])
            if m:
                dollar = m.group(0); buf.append(dollar); i += len(dollar); continue
        if ch == ';':
            out.append(''.join(buf)); buf = []; i += 1; continue
        buf.append(ch); i += 1
    if ''.join(buf).strip():
        out.append(''.join(buf))
    return out


def strip_comments(sql):
    sql = re.sub(r'/\*.*?\*/', ' ', sql, flags=re.S)
    return re.sub(r'--[^\n]*', ' ', sql)


def top_parens(text):
    """产出所有**顶层** ( ... ) 的内容（跳过引号内）。"""
    out, i, n = [], 0, len(text)
    while i < n:
        ch = text[i]
        if ch == "'":
            i += 1
            while i < n:
                if text[i] == "'":
                    if i + 1 < n and text[i + 1] == "'":
                        i += 2; continue
                    break
                i += 1
            i += 1; continue
        if ch == '(':
            depth, j, start = 1, i + 1, i + 1
            while j < n and depth:
                c = text[j]
                if c == "'":
                    j += 1
                    while j < n:
                        if text[j] == "'":
                            if j + 1 < n and text[j + 1] == "'":
                                j += 2; continue
                            break
                        j += 1
                elif c == '(':
                    depth += 1
                elif c == ')':
                    depth -= 1
                    if depth == 0:
                        out.append(text[start:j]); break
                j += 1
            i = j + 1; continue
        i += 1
    return out


def row_id_values(body):
    """从 INSERT 正文抽取每一行的 id 值候选（id 为列序首位时即行 id）。"""
    ids = set()
    stripped = body.lstrip()
    if re.match(r'select\b', stripped, re.I):
        # 形态 A：`INSERT ... SELECT v.id, ... FROM (VALUES (...),(...)) AS v(...)`
        groups = [p for p in top_parens(stripped) if re.match(r'values\b', p.lstrip(), re.I)]
        if groups:
            for g in groups:
                for row in top_parens(g):
                    u = UUID_ANY.search(row)
                    if u:
                        ids.add(u.group(0).lower())
            return ids
        # 形态 B：`INSERT ... SELECT '<id>'::uuid, ...` —— 单行，id 是第一个值
        m = UUID_ANY.search(stripped)
        if m:
            ids.add(m.group(0).lower())
        return ids
    for row in top_parens(body):
        u = UUID_ANY.search(row)
        if u:
            ids.add(u.group(0).lower())
    return ids


def produced_ids(path):
    text = strip_comments(Path(path).read_text(encoding='utf-8'))
    out = {}
    for raw in split_statements(text):
        stmt = raw.lstrip()
        m = INS_HEAD.match(stmt)
        if not m:
            continue
        table = m.group(1).lower()
        col_open = m.end() - 1
        depth, j = 0, col_open
        while j < len(stmt):
            if stmt[j] == '(':
                depth += 1
            elif stmt[j] == ')':
                depth -= 1
                if depth == 0:
                    break
            j += 1
        cols = [c.strip().lower() for c in stmt[col_open + 1:j].split(',')]
        if not cols or cols[0] != 'id':
            continue
        out.setdefault(table, set()).update(row_id_values(stmt[j + 1:]))
    return out


def guard_ids(path):
    text = strip_comments(Path(path).read_text(encoding='utf-8'))
    out = set()
    for stmt in split_statements(text):
        if 'raise exception' not in stmt.lower():
            continue
        for m in re.finditer(r'\bid\s*=\s*\'(' + UUID + r')\'', stmt, re.I):
            out.add(m.group(1).lower())
    return out


def main():
    vf = sorted(p for d in VERSIONED_DIRS for p in (BASE / d).rglob('*.up.sql'))
    seeds = sorted((BASE / 'seed').rglob('*.sql'))
    v_prod, s_prod = {}, {}
    for p in vf:
        for t, s in produced_ids(p).items():
            v_prod.setdefault(t, set()).update(s)
    for p in seeds:
        for t, s in produced_ids(p).items():
            s_prod.setdefault(t, set()).update(s)

    all_v = set().union(*v_prod.values()) if v_prod else set()
    all_s = set().union(*s_prod.values()) if s_prod else set()
    print(f'版本化迁移以 id 列 INSERT 的 uuid：{len(all_v)}；seed：{len(all_s)}；seed 独占：{len(all_s - all_v)}')
    for t in sorted(set(list(v_prod) + list(s_prod))):
        print(f'  {t:22s} versioned={len(v_prod.get(t, ())):4d} seed={len(s_prod.get(t, ())):4d}')

    print("\n== 护栏断言（RAISE EXCEPTION 块里 id = '<uuid>'）引用「版本化迁移从不产出」的 id ==")
    hits = []
    for p in vf:
        missing = sorted(u for u in guard_ids(p) if u not in all_v)
        if missing:
            hits.append((p, missing))
    print(f'文件数：{len(hits)}')
    for p, missing in hits:
        print(f'\n  {p.relative_to(BASE)}')
        for u in missing:
            tag = 'seed 行（空库首启必炸）' if u in all_s else '谁都不产出（幽灵 id / 运行时生成）'
            print(f'    - {u}  ←  {tag}')


if __name__ == '__main__':
    sys.exit(main())
