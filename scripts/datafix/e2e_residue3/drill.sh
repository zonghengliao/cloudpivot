#!/usr/bin/env bash
# ============================================================================
# E2ERES3 / drill：在**生产数据的完整副本**上演练 up → verify → down → verify → up
# ----------------------------------------------------------------------------
# 为什么要有它：
#   「在生产上直接跑第一次」是不可接受的 —— 软删 383 行、禁用 6 个账号，改错了
#   就得靠备份回滚。演练库是 prod 的**整库克隆**（2026-11-05 实测 34 MB），
#   所以行形态、约束、触发器、外键全都一样，不存在「演练库能过、生产过不了」。
#
# 验证什么：
#   ① 00_backup.sh 的备份闸能认出**本库**的备份（演练也走同一条路，闸不虚设）
#   ② 10 前置校验在真实数据上成立（没有真实子任务挂在 E2E 父任务下、没有在飞审批）
#   ③ 20 up 后残留 = 0（30_verify 全 PASS + e2e_marker --assert-clean 也认）
#   ④ 20 down 后残留**原样回来**（可逆），且 30_verify 这时候必须**失败**
#      —— 反向对照：证明 30 的断言真的在查东西，不是摆设
#   ⑤ 再跑一遍 20 up：幂等（第二遍命中 0 行，账本按快照口径不变）
#
# 生产已被清干净之后复跑本演练：克隆库本来就没有残留，此时**按本批快照把行写回**重建
# 「改前现场」再炼（快照 = 当时那 383 行 + 6 个账号的原样），所以演练永远可复跑。
#
# 用法（在 144 上，root）：
#   bash scripts/datafix/e2e_residue3/drill.sh
#   KEEP_DRILL_DB=1 bash scripts/datafix/e2e_residue3/drill.sh   # 演练完不删库（查现场用）
# ============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERIFY="$HERE/../../verify"

ENV_FILE="${E2ERES3_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-${ALUN_DATABASE_PASSWORD:-}}"
: "${PGPASSWORD:?需要 NIK_DATABASE_PASSWORD（先 . /etc/cloudpivot/cloudpivot.env）}"

PROD_DB="${NIK_DATABASE_NAME:-cloudpivot}"
DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
DRILL_DB="${E2ERES3_DRILL_DB:-cloudpivot_e2eres3_drill}"

if [ "$DRILL_DB" = "$PROD_DB" ]; then
    echo "!! 演练库名不能等于生产库名（$PROD_DB）" >&2; exit 4
fi

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -v ON_ERROR_STOP=1 -X)
fail=0
step() { echo; echo "======== $* ========"; }
ok()   { echo "  ✅ $*"; }
bad()  { echo "  ❌ $*"; fail=1; }

# 残留行数一览（用唯一判据源；非零退出码用 --json 拿 ok 字段）
residue_alive() {
    NIK_DATABASE_NAME="$1" python3 "$VERIFY/e2e_marker.py" --json 2>/dev/null \
      | python3 -c 'import json,sys; d=json.load(sys.stdin); print(sum(h["alive"] for h in d["residue"]), len(d["enabled_test_accounts"]))'
}

step "0. 克隆生产库 → $DRILL_DB"
sudo -u postgres dropdb --if-exists "$DRILL_DB" 2>/dev/null
sudo -u postgres createdb -O "$DB_USER" "$DRILL_DB" || { echo "!! createdb 失败" >&2; exit 1; }
set -o pipefail
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$PROD_DB" --no-owner --no-acl \
  | "${PSQL[@]}" -q -d "$DRILL_DB"
ok "克隆完成：$(NIK_DATABASE_NAME="$DRILL_DB" "${PSQL[@]}" -d "$DRILL_DB" -At -c 'select pg_size_pretty(pg_database_size(current_database()))')"

read -r before_rows before_accts <<<"$(residue_alive "$DRILL_DB")"
echo "  克隆库残留：$before_rows 行标记数据 / $before_accts 个在用测试账号（应 > 0，否则炼不出东西）"

# 生产已经清干净之后（本包合入后），克隆出来的库**本来就没有残留** —— 演练会失去意义。
# 这时用本批快照把改过的行写回去，重建「改前现场」：快照就是当时那 383 行 + 6 个账号的原样，
# 所以重建出来的现场与首轮演练完全等价（这也顺带证明快照本身够回滚）。
if [ "${before_rows:-0}" -eq 0 ]; then
    echo "  克隆库已无残留（生产已清干净）→ 按快照重建改前现场"
    if E2ERES3_DB="$DRILL_DB" bash "$HERE/apply.sh" --rollback > /tmp/e2eres3_drill_seed.log 2>&1; then
        read -r before_rows before_accts <<<"$(residue_alive "$DRILL_DB")"
        echo "  重建后残留：$before_rows 行 / $before_accts 个在用账号"
        grep -E 'restore|rolled_back' /tmp/e2eres3_drill_seed.log | tail -4 | sed 's/^/    /'
    else
        bad "按快照重建现场失败（见 /tmp/e2eres3_drill_seed.log）"
        tail -20 /tmp/e2eres3_drill_seed.log
    fi
fi

[ "${before_rows:-0}" -gt 0 ] && ok "克隆库带着真实残留（演练有意义）" || bad "克隆库没有残留，演练无意义"

step "1. 00_backup.sh（演练库也走备份闸）"
E2ERES3_DB="$DRILL_DB" bash "$HERE/00_backup.sh" > /tmp/e2eres3_drill_backup.log 2>&1
if [ $? -eq 0 ]; then
    ok "备份完成：$(grep -m1 '备份完成' /tmp/e2eres3_drill_backup.log | sed 's/==> 备份完成：//')"
    cp /tmp/e2eres3_drill_backup.log "$(grep -m1 '备份完成' /tmp/e2eres3_drill_backup.log | sed 's/.*：//')/drill_backup.log"
else
    bad "备份失败，见 /tmp/e2eres3_drill_backup.log"; tail -20 /tmp/e2eres3_drill_backup.log
fi

step "2. apply（10 前置 → 20 up → 30 自检 → 判据交叉断言）"
if E2ERES3_DB="$DRILL_DB" bash "$HERE/apply.sh" > /tmp/e2eres3_drill_apply1.log 2>&1; then
    ok "apply 通过（日志 /tmp/e2eres3_drill_apply1.log）"
    grep -E 'soft-delete|disable|PASS|assert-clean|认干净' /tmp/e2eres3_drill_apply1.log | sed 's/^/    /'
else
    bad "apply 失败"; tail -30 /tmp/e2eres3_drill_apply1.log
fi

read -r after_rows after_accts <<<"$(residue_alive "$DRILL_DB")"
[ "${after_rows:-1}" = "0" ] && [ "${after_accts:-1}" = "0" ] \
  && ok "清后残留 = 0 行 / 0 个在用账号" \
  || bad "清后仍有残留：$after_rows 行 / $after_accts 个账号"

step "3. 反向对照：30_verify 在「有残留」时**必须**失败"
# 先回滚，就有残留了；此时跑 30 应该红 —— 证明断言不是摆设
E2ERES3_DB="$DRILL_DB" bash "$HERE/apply.sh" --rollback > /tmp/e2eres3_drill_down.log 2>&1
read -r back_rows back_accts <<<"$(residue_alive "$DRILL_DB")"
[ "${back_rows:-0}" = "${before_rows:-x}" ] \
  && ok "回滚后残留原样回来：$back_rows 行（改前 $before_rows）" \
  || bad "回滚不完整：$back_rows 行 ≠ 改前 $before_rows 行"
[ "${back_accts:-0}" = "${before_accts:-x}" ] \
  && ok "回滚后账号状态回来：$back_accts 个在用（改前 $before_accts）" \
  || bad "账号状态没回来：$back_accts ≠ $before_accts"

if "${PSQL[@]}" -d "$DRILL_DB" -q -f "$HERE/30_verify.sql" > /tmp/e2eres3_drill_verify_neg.log 2>&1; then
    bad "30_verify 在回滚后竟然通过 —— 断言是假的（负向对照没红）"
else
    ok "30_verify 在回滚后如期失败（负向对照成立，见 /tmp/e2eres3_drill_verify_neg.log）"
    grep -m3 'FAIL' /tmp/e2eres3_drill_verify_neg.log | sed 's/^/    /'
fi

step "4. 再跑一遍 apply：幂等（第二遍命中 0 行）"
if E2ERES3_DB="$DRILL_DB" bash "$HERE/apply.sh" > /tmp/e2eres3_drill_apply2.log 2>&1; then
    ok "第二次 apply 通过"
    grep -E 'deleted_total|deleted_biz_task|disabled_users|30_verify OK' /tmp/e2eres3_drill_apply2.log \
      | sed 's/^/    /' | head -6
    grep -qE 'soft-delete biz_task +0 行' /tmp/e2eres3_drill_apply2.log \
      && ok "第二遍 biz_task 命中 0 行（幂等）" \
      || echo "    （注：回滚后重跑会再删一遍，属正常 —— 幂等要看同一状态连跑两次）"
else
    bad "第二次 apply 失败"; tail -20 /tmp/e2eres3_drill_apply2.log
fi
read -r again_rows again_accts <<<"$(residue_alive "$DRILL_DB")"
[ "${again_rows:-1}" = "0" ] && ok "第二遍结束后残留仍为 0" || bad "第二遍结束后残留 $again_rows 行"

step "5. 幂等复核（同一状态连跑两次，第二遍必须全 0）"
if E2ERES3_DB="$DRILL_DB" bash "$HERE/apply.sh" > /tmp/e2eres3_drill_apply3.log 2>&1; then
    ok "第三次 apply 通过"
    sed -n '/soft-delete/p;/disable/p' /tmp/e2eres3_drill_apply3.log | sed 's/^/    /'
    if grep -qE 'soft-delete [a-z_]+ +[1-9]' /tmp/e2eres3_drill_apply3.log; then
        bad "第三次仍有命中行 —— 不幂等"
    else
        ok "第三次全部命中 0 行（幂等成立）"
    fi
else
    bad "第三次 apply 失败"; tail -20 /tmp/e2eres3_drill_apply3.log
fi

step "6. 收尾"
if [ "${KEEP_DRILL_DB:-0}" = "1" ]; then
    echo "  KEEP_DRILL_DB=1 → 保留演练库 $DRILL_DB"
else
    sudo -u postgres dropdb --if-exists "$DRILL_DB" && ok "演练库已删除（$DRILL_DB）"
fi

echo
if [ "$fail" = "0" ]; then
    echo "★★★★ 演练全绿：可逆 / 幂等 / 断言有牙 ★★★★"
else
    echo "✗✗✗ 演练有失败项（见上）✗✗✗"
fi
exit "$fail"
