#!/usr/bin/env bash
# HR-45 演练库回放（145）：按版本号升序 apply 全部 V*.up.sql
set -uo pipefail
ROOT=/opt/cloudpivot
MROOT=$ROOT/backend/migrations
DB="${1:-hr45_drill}"
export PGPASSWORD="$(cat ${HR45_PW_FILE:?请指向本机保存 145 口令的文件})"
PSQL=(psql -h 172.18.10.145 -U cloudpivot -d "$DB" -q -X -v ON_ERROR_STOP=1 --single-transaction)

ok=0; bad=""
while IFS= read -r f; do
    if err="$("${PSQL[@]}" -f "$f" 2>&1 >/dev/null)"; then
        ok=$((ok+1))
    else
        bad="$f"
        echo "✗ FAILED: $f"
        printf '%s\n' "$err" | grep -E "ERROR|HINT|DETAIL" | head -6 | sed 's/^/    /'
        break
    fi
done < <(find "$MROOT" -name 'V*__*.up.sql' | awk -F'/V' '{split($NF,a,"__"); print a[1]+0 "\t" $0}' | sort -n -k1,1 | cut -f2-)

total=$(find "$MROOT" -name 'V*__*.up.sql' | wc -l)
if [ -n "$bad" ]; then
    echo "== 结果 FAIL: $ok/$total，第一条失败 $(basename "$bad")"
    exit 1
fi
echo "== 结果 PASS: $ok/$total 条迁移全部应用（含 V0398 收口自检） 库=$DB"
