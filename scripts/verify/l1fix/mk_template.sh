#!/bin/bash
# 造一个「模板 profile 的 .env 只有注释、没有任何 key」的现场（复刻 2026-10-29 事故）
set -u
HB=/root/.hermes/hermes-agent/hermes
export HERMES_HOME=/root/.hermes
echo "=== 制造无 key 模板 l1fix-tmpl ==="
$HB profile delete l1fix-tmpl -y >/dev/null 2>&1
$HB profile create l1fix-tmpl --no-alias --description "L1修复验收临时模板" 2>&1 | tail -2
printf '# Per-profile secrets for this Hermes profile.\n# API keys and tokens set here override the shell environment.\n# Behavioral settings belong in config.yaml, not here.\n' > /root/.hermes/profiles/l1fix-tmpl/.env
chmod 600 /root/.hermes/profiles/l1fix-tmpl/.env
echo "=== 模板 .env（应无任何 KEY=<值> 行）==="
python3 - <<'PY'
path = "/root/.hermes/profiles/l1fix-tmpl/.env"
keys = []
for line in open(path):
    line = line.strip()
    if line and not line.startswith("#") and "=" in line:
        keys.append(line.split("=", 1)[0])
print("  键名列表:", keys if keys else "（空 —— 没有任何凭据键）")
PY
ls -l /root/.hermes/profiles/l1fix-tmpl/.env
