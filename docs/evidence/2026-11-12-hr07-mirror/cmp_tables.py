import re, pathlib
rust = pathlib.Path('/opt/cloudpivot/backend/src/business/shared/doc_kind.rs').read_text()
ts = pathlib.Path('/opt/cloudpivot/frontend/src/api/business/erp/doc-kinds.ts').read_text()

def fn_body(type_name, sig):
    at = rust.index('pub enum %s ' % type_name)
    f = rust.index(sig, at)
    b = rust.index('match self {', f)
    e = rust.index('\n    }\n', b)
    return rust[b:e]

astr = dict(re.findall(r'DocKind::(\w+) => "([A-Z_0-9]+)"', fn_body('DocKind', 'pub const fn as_str')))
tbl = dict(re.findall(r'DocKind::(\w+) => "(\w+)"', fn_body('DocKind', 'pub const fn table')))
tbl_ts = dict(re.findall(r"^\s{2}([A-Z_][A-Z_0-9]*): '(\w+)'", ts[ts.index('export const DOC_KIND_TABLES'):ts.index('export const DOC_KIND_ROUTES')], re.M))
tbl_rust = {astr[k]: v for k, v in tbl.items()}
print('rust table():', len(tbl_rust))
print('ts  tables  :', len(tbl_ts))
print('rust-only:', {k: v for k, v in tbl_rust.items() if k not in tbl_ts})
print('ts-only  :', {k: v for k, v in tbl_ts.items() if k not in tbl_rust})
print('value diff:', {k: (tbl_rust.get(k), tbl_ts.get(k)) for k in set(tbl_rust) & set(tbl_ts) if tbl_rust[k] != tbl_ts[k]})
