#!/usr/bin/env bash
# ============================================================================
# 空库重放探针：验证「整条迁移链能不能在空库上按版本号升序跑完」
#
# 为什么需要它（t_d403a60a 的教训，2026-11-07）：
#   `V0267__sales_menu` 的自检要求「它要授的 25 个销售权限码在 sys_menu 有承载」，
#   其中 17 个码由**更晚**的 V0284/288/293/296 才种下 ⇒ 空库重放到 267 必 RAISE，
#   全新安装 / 灾难重建 / `erp_clean_db.sh` 这条干净库路线全断。
#   而当时所有门禁（fmt/clippy/check.sh/单测）都是**在已迁移库上**跑的，谁也发现不了。
#   这条探针在 SQL 层面把「按版本号重放」跑一遍，30 秒内暴露这类**乱序依赖**。
#
# 它能抓到 / 抓不到：
#   ✓ 迁移自检引用「更晚迁移才建的行 / 表 / 角色码 / 菜单码」
#   ✓ 空库上缺前置 DDL（自检里的 count<>N、RAISE EXCEPTION）
#   ✗ 引擎侧行为（seed / repeatable / bootstrap / 运行时守卫）—— 那一档用
#     `bash scripts/verify/erp_clean_db.sh`（起一次实例，日志见「数据库迁移完成: 应用 N 个迁移」）
#
# 用法：
#   bash scripts/verify/mig_replay_fresh.sh                # 临时库名 mig_replay_<时间戳>
#   bash scripts/verify/mig_replay_fresh.sh card123        # 指定后缀，便于复跑对账
#   KEEP_DB=1 bash scripts/verify/mig_replay_fresh.sh      # 跑完不删库（排查用）
#
# 退出码：0 = 全部通过；1 = 有迁移失败（打印第一条失败的文件与 ERROR 行）；2 = 环境问题
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MROOT="${MROOT:-$ROOT/backend/migrations}"
DBHOST="${DBHOST:-127.0.0.1}"
DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-alun}"
DBPASS="${DBPASS:-alun}"
DBOWNER="${DBOWNER:-$DBUSER}"
SUFFIX="${1:-$(date +%m%d%H%M%S)}"
DB="mig_replay_${SUFFIX}"

command -v psql >/dev/null 2>&1 || { echo "FAIL: 没有 psql" >&2; exit 2; }
[ -d "$MROOT" ] || { echo "FAIL: 迁移目录不存在：$MROOT" >&2; exit 2; }

export PGPASSWORD="$DBPASS"
q() { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$1" -tAc "$2"; }

# ---------- 1) 建库（alun 通常没有 CREATEDB 权限，退到 postgres 账号） ----------
if q postgres "select 1 from pg_database where datname='$DB'" | grep -q 1; then
    echo "FAIL: 临时库已存在：$DB（换一个后缀，或先手工处理）" >&2
    exit 2
fi
if ! q postgres "CREATE DATABASE $DB OWNER $DBOWNER" >/dev/null 2>&1; then
    su - postgres -c "createdb -O $DBOWNER $DB" >/dev/null 2>&1 \
        || { echo "FAIL: 建库失败（$DB）；本机需要 root 借 postgres 账号建库" >&2; exit 2; }
fi
echo "==> 空库已建：$DB（owner=$DBOWNER）"

cleanup() {
    if [ "${KEEP_DB:-0}" = "1" ]; then
        echo "==> KEEP_DB=1：保留 $DB"
        return
    fi
    su - postgres -c "dropdb --force $DB" >/dev/null 2>&1 \
        || q postgres "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1 \
        || echo "⚠ 临时库 $DB 没删掉（无权限），请手工清理"
}
trap cleanup EXIT

# ---------- 2) 按「版本号升序」逐条 apply（引擎口径：递归全部子目录） ----------
ok=0
first_bad=""
while IFS= read -r f; do
    ver="$(basename "$f" | sed -E 's/^V([0-9]+)__.*/\1/')"
    if err="$(psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -q -X \
                 -v ON_ERROR_STOP=1 --single-transaction -f "$f" 2>&1 >/dev/null)"; then
        ok=$((ok + 1))
    else
        first_bad="$f"
        echo "✗ 失败：v$ver $(basename "$f")"
        printf '%s\n' "$err" | grep -E "ERROR|FATAL" | head -5 | sed 's/^/    /'
        break
    fi
done < <(find "$MROOT" -name 'V*__*.up.sql' \
         | awk -F'/V' '{split($NF, a, "__"); print a[1] + 0 "\t" $0}' \
         | sort -n -k1,1 | cut -f2-)

total="$(find "$MROOT" -name 'V*__*.up.sql' | wc -l)"
if [ -n "$first_bad" ]; then
    echo "== 结果：FAIL —— 前 $ok/$total 条成功，第一条失败在 $(basename "$first_bad")"
    echo "   典型病因：该迁移的自检引用了**更晚**迁移才建的行（菜单码 / 角色码 / 表）。"
    echo "   修法：新增一条**号段更小**的前置迁移把它依赖的行提前种下（已应用迁移不可改，"
    echo "        改一个注释都会让所有已应用库启动即 checksum 失败），并保持 ON CONFLICT DO NOTHING。"
    exit 1
fi
echo "== 结果：PASS —— $ok/$total 条版本化迁移在空库上按版本号升序全部通过"
