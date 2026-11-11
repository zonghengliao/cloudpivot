#!/usr/bin/env python3
"""独立核对：Rust 词表 vs 前端镜像（逐值 + 保序）。

不依赖 `cargo test` 的断言：直接从 `doc_kind.rs` 的 `as_str()` / `label()` match 抠值，
与 `frontend/src/api/business/erp/doc-kinds.ts`、`attachment.ts` 比对。
"""
import re
import sys
import pathlib

ROOT = pathlib.Path('/opt/cloudpivot')
rust = (ROOT / 'backend/src/business/shared/doc_kind.rs').read_text()
ts = (ROOT / 'frontend/src/api/business/erp/doc-kinds.ts').read_text()
att = (ROOT / 'frontend/src/api/business/erp/attachment.ts').read_text()


def fn_pairs(type_name: str, fn_sig: str):
    """`impl <Type>` 块内某个 `pub const fn ...` 的 `Type::Variant => "值"` 映射。"""
    enum_at = rust.index('pub enum %s ' % type_name)
    fn_at = rust.index(fn_sig, enum_at)
    body_start = rust.index('match self {', fn_at) + len('match self {')
    body_end = rust.index('\n    }\n', body_start)
    body = rust[body_start:body_end]
    pairs = re.findall(r'%s::([A-Za-z0-9]+)\s*=>\s*"([^"]*)"' % type_name, body)
    if not pairs:
        raise SystemExit('no pairs for %s in %s' % (type_name, fn_sig))
    return dict(pairs)


AS_STR = {t: fn_pairs(t, 'pub const fn as_str') for t in
          ('DocKind', 'FlowBizType', 'VoucherSourceType')}
LABEL = fn_pairs('DocKind', 'pub const fn label')


def rust_all(type_name: str):
    m = re.search(r'pub const ALL: \[%s; (\d+)\] = \[(.*?)\n\s{4}\];' % type_name, rust, re.S)
    return re.findall(r'^\s*%s::([A-Za-z0-9]+),' % type_name, m.group(2), re.M), int(m.group(1))


def rust_attachment_kinds():
    m = re.search(r'pub const ATTACHMENT_DOC_KINDS: \[DocKind; (\d+)\] = \[(.*?)\n\];', rust, re.S)
    return re.findall(r'^\s*DocKind::([A-Za-z0-9]+),', m.group(2), re.M), int(m.group(1))


def ts_arr(name: str, text: str = ts):
    m = re.search(r'export const %s[^=]*= \[(.*?)\n\] as const' % name, text, re.S)
    return re.findall(r"'([A-Z][A-Z_0-9]*)'", m.group(1))


def ts_record_keys(name: str, text: str = att):
    start = text.index('export const %s' % name)
    open_ = text.index('{', start)
    close = text.index('};', open_)
    return re.findall(r'^\s{2}([A-Z][A-Z_0-9]*):', text[open_:close], re.M)


ok = True


def check(label, rust_vals, ts_vals):
    global ok
    same = rust_vals == ts_vals
    ok = ok and same
    print(f'{label:50s} rust={len(rust_vals):3d} ts={len(ts_vals):3d} '
          f'{"ORDERED-MATCH" if same else "MISMATCH"}')
    if not same:
        print('   rust-only:', [x for x in rust_vals if x not in ts_vals])
        print('   ts-only  :', [x for x in ts_vals if x not in rust_vals])
        for i, (a, b) in enumerate(zip(rust_vals, ts_vals)):
            if a != b:
                print(f'   first order diff idx={i}: rust={a} ts={b}')
                break


# ① DocKind::ALL → DOC_KINDS
variants, declared = rust_all('DocKind')
assert len(variants) == declared, (len(variants), declared)
doc_kind_vals = [AS_STR['DocKind'][v] for v in variants]
check('DocKind::ALL(36)          -> DOC_KINDS', doc_kind_vals, ts_arr('DOC_KINDS'))

# ② FlowBizType::ALL → FLOW_BIZ_TYPES
variants, declared = rust_all('FlowBizType')
assert len(variants) == declared, (len(variants), declared)
biz_vals = [AS_STR['FlowBizType'][v] for v in variants]
check('FlowBizType::ALL(20)      -> FLOW_BIZ_TYPES', biz_vals, ts_arr('FLOW_BIZ_TYPES'))

# ③ VoucherSourceType::ALL → VOUCHER_SOURCE_TYPES
variants, declared = rust_all('VoucherSourceType')
assert len(variants) == declared, (len(variants), declared)
src_vals = [AS_STR['VoucherSourceType'][v] for v in variants]
check('VoucherSourceType::ALL(22)-> VOUCHER_SOURCE_TYPES', src_vals, ts_arr('VOUCHER_SOURCE_TYPES'))

# ④ ATTACHMENT_DOC_KINDS（21，DocKind 子集）→ ATTACHMENT_DOC_TYPES
variants, declared = rust_attachment_kinds()
assert len(variants) == declared, (len(variants), declared)
att_vals = [AS_STR['DocKind'][v] for v in variants]
check('ATTACHMENT_DOC_KINDS(21)   -> ATTACHMENT_DOC_TYPES', att_vals, ts_arr('ATTACHMENT_DOC_TYPES'))

# ⑤ attachment.ts 两张 Record<DocType, …> 必须覆盖同一 21 类
for rec in ('DOC_TYPE_LABELS', 'DEFAULT_ROLE_BY_DOC'):
    keys = ts_record_keys(rec)
    same = sorted(keys) == sorted(att_vals)
    ok = ok and same
    print(f'{"attachment.ts " + rec:50s} rust={len(att_vals):3d} ts={len(keys):3d} '
          f'{"COVERS-ALL-21" if same else "MISMATCH"}')
    if not same:
        print('   missing in ts:', [x for x in att_vals if x not in keys])
        print('   extra in ts  :', [x for x in keys if x not in att_vals])

# ⑥ DOC_KIND_LABELS 逐条中文名
labels_block = re.search(r'export const DOC_KIND_LABELS[^=]*= \{(.*?)\n\};', ts, re.S).group(1)
ts_labels = dict(re.findall(r"^\s{2}([A-Z][A-Z_0-9]*): '([^']*)'", labels_block, re.M))
rust_labels = {AS_STR['DocKind'][k]: v for k, v in LABEL.items()}
diffs = {k: (rust_labels.get(k), v) for k, v in ts_labels.items() if rust_labels.get(k) != v}
missing = [k for k in doc_kind_vals if k not in ts_labels]
ok = ok and not diffs and not missing
print(f'{"DOC_KIND_LABELS 逐条":50s} ts={len(ts_labels):3d} '
      f'{"ALL-LABELS-MATCH" if not diffs and not missing else "MISMATCH"}')
if diffs:
    print('   label diffs:', diffs)
if missing:
    print('   missing keys:', missing)

print('RESULT:', 'ALL_MIRRORS_MATCH' if ok else 'DIFF_FOUND')
sys.exit(0 if ok else 1)
