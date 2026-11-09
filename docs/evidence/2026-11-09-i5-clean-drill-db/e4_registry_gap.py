import re, glob
from pathlib import Path

reg = Path("scripts/verify/erp_invariants.py").read_text()
m = re.search(r"^E4_BIZ_TYPE_AXIS = \{(.*?)^\}", reg, re.M | re.S)
registered = set(re.findall(r'"([A-Z0-9_]+)"\s*:', m.group(1)))
const_vals = set()
where = {}
for p in glob.glob("backend/src/business/**/*.rs", recursive=True):
    txt = Path(p).read_text()
    for lit in re.findall(r'pub const BIZ_TYPE_[A-Z_]+:\s*&str\s*=\s*"([A-Z][A-Z0-9_]*)"', txt):
        const_vals.add(lit)
        where.setdefault(lit, []).append(p)
print("registered:", len(registered))
print("literal BIZ_TYPE_ consts:", sorted(const_vals))
print("MISSING:", sorted(const_vals - registered))
for k in sorted(const_vals - registered):
    print("   ", k, where[k])
# also check TXN_BIZ_TYPE_VALUES domain
dk = Path("backend/src/business/shared/doc_kind.rs").read_text()
d = re.search(r"const TXN_BIZ_TYPE_VALUES: \[&str; (\d+)\] = \[(.*?)\n\];", dk, re.S)
domain = set(re.findall(r'"([A-Z0-9_]+)"', d.group(2)))
print("TXN_BIZ_TYPE_VALUES:", len(domain), "declared", d.group(1))
print("domain - registered:", sorted(domain - registered))
print("registered - domain:", sorted(registered - domain))
print("literals not in domain:", sorted(const_vals - domain))
