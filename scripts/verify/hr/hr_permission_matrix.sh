#!/usr/bin/env bash
# =============================================================================
# HR-47 · 人力资源权限矩阵回归（30 码 × 6 角色 + SELF 只读自己薪资）
# =============================================================================
#
# 卡面验收：
#   ① 30 码 × 6 角色矩阵全跑
#   ② SELF 角色只能看自己
#   ③ 越权全部被拒
#
# ─── 矩阵是什么（唯一真源 = `V0387__hr_menu_perms.up.sql`）───────────────────────
#
# 设计 §3.2 ④ 的 30 个按钮码 + 5 个页面 `*:list` 码 = **35 个权限码**；
# 设计 §3.2 ⑤ 的角色范围由 V0387 逐码落成授权数组（all_hr / fin_hr / lead_hr / emp_hr）。
# 本脚本**不手抄**这张表：
#
#     期望 = `hr_perm_expect.py` 解析迁移文件（种子怎么种，就该是什么）
#     实际 = 真实例 `/api/system/auth/me` 的 permissions + 每条路由的 200/403
#
# 6 个角色 = 被授权的 5 个（超管 / 人事 / 财务 / 部门主管 / 员工自助）
#           + **零授权对照** FINANCE_DIRECTOR（真实存在的角色，V0387 一行都没授给它 ——
#             证明「没码就 403」不是账号不存在导致的）。
#
# ─── 三层验证（缺一层都会漏掉一类越权）────────────────────────────────────────
#
#   层 A 授权快照：6 身份 × 35 码 —— `/auth/me` 的权限集合必须**逐个码等于**种子。
#         多一个码 = 授权漂移（越权）；少一个码 = 功能静默不可用。
#   层 B 路由闸  ：6 身份 × 31 码（有后端路由的）—— 有码 → 不是 403；没码 → **必须 403**
#         （并断言报文是「无权限」，不是 401 / 404 —— 后两者会伪装成「接口不存在」）。
#   层 C 数据范围：SELF 只能看自己（/salary/my 只出本人工资条，且 /salary/page、
#         /salary/{别人的单}、/employee/{别人} 一律 403）；
#         DEPT_AND_CHILD（人事经理 data_scope=3）只看本部门子树；
#         ALL（部门主管 data_scope=1）看全部 —— 三条一起证明「不是一刀切拒绝」。
#
# ─── 为什么必须真 HTTP + 真库 + 真实例 ─────────────────────────────────────────
#
# `cargo test` 只跑 handler 内业务函数，`#[nik::permission]` 的码拼错、路由没注册、
# 令牌快照没刷新这四类错它一条都发现不了；而线上「点按钮 403」正是这四类。
#
# ─── 演练库纪律（2026-11-12 用户强制）─────────────────────────────────────────
#
# * 演练库**只建在 145（172.18.10.145）**，绝不在 99 上建（99 是 Agent 主机）；
# * 库名必须含 `drill` / `e2e` / `test`（脚本开头硬校验，不匹配直接退出 2）；
# * `--fresh` 自己建库、跑完自己 DROP（`KEEP_DB=1` 留现场）；
# * 给了 `HR_DB_URL` 则只跑不清（那是别人给的库，脚本不替你删）。
#
# ─── 用法 ────────────────────────────────────────────────────────────────────
#
#   bash scripts/verify/hr/hr_permission_matrix.sh --fresh      # 建库 → 起实例 → 三层矩阵 → DROP
#   HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr47_drill \
#       bash scripts/verify/hr/hr_permission_matrix.sh           # 用现成演练库
#   KEEP=1 KEEP_DB=1 bash ... --fresh                           # 留实例与库（排查用）
#
# 环境变量
#   HR47_PGPASSWORD   145 的库口令（缺省从 145 的 /etc/cloudpivot/cloudpivot-dev.env 经 ssh 读）
#   HR47_DBHOST/PORT  库地址（默认 172.18.10.145:5432）
#   HR47_DBUSER       库账号（默认 cloudpivot，需 CREATE DATABASE）
#   HR47_PORT         后端端口（默认 18147，必须落在 18000-18999 隔离段）
#   HR47_BIN          后端可执行文件（默认 backend/target/debug/cloudpivot）
#   HR47_BACKEND      后端目录（默认 <repo>/backend）
#   HR47_RUN_DIR      运行目录（默认 /root/hr47-matrix-<时间戳>）
#   ADMIN_PASSWORD    超管初始口令（默认 Admin@123456；--fresh 新库由 bootstrap 种下）
#   KEEP=1            不杀实例；KEEP_DB=1 不删库
#
# 退出码：0 = 三层矩阵全绿；1 = 有断言不通过；2 = 环境/前置不满足
# =============================================================================
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$HERE/../../.." && pwd)
BACKEND=${HR47_BACKEND:-$REPO_ROOT/backend}
BIN=${HR47_BIN:-$BACKEND/target/debug/cloudpivot}
EXPECT_PY=$HERE/hr_perm_expect.py
MIGRATION=$REPO_ROOT/backend/migrations/business/V0387__hr_menu_perms.up.sql

TAG=$(date +%m%d-%H%M%S)
PORT=${HR47_PORT:-18147}
BASE="http://127.0.0.1:$PORT"
# 路由宏里写的路径**不含** `/api`（由 `[router] prefix` 注入，AGENTS 约定 8），
# 而 ROUTE 表里存的正是宏里的形态 ⇒ 真正请求要拼 `$API_BASE`。
# （2026-11-13 实测教训：拼成 `$BASE$path` 会全部落到 router 的 404 兜底，而
#   「允许态」判据是「非 403」→ 404 会被当成通过，矩阵悄悄全绿。故本脚本同时断言
#   每条路由的 404 报文不能是「请求的资源不存在」。）
API_BASE="$BASE/api"
DBHOST=${HR47_DBHOST:-172.18.10.145}
DBPORT=${HR47_DBPORT:-5432}
DBUSER=${HR47_DBUSER:-cloudpivot}
DB=${HR47_DB:-hr47_drill_$(date +%m%d%H%M%S)}
ADMIN_USER=admin
ADMIN_PASSWORD=${ADMIN_PASSWORD:-Admin@123456}
KEEP=${KEEP:-0}
KEEP_DB=${KEEP_DB:-0}
FRESH=0

PASS=0
FAIL=0
OUT=$(mktemp)
TOKEN=""
CODE=""
SERVER_PID=""
PROFILE_FILE=""
RUN_DIR=${HR47_RUN_DIR:-/root/hr47-matrix-$(date +%Y%m%d-%H%M%S)}
SERVER_LOG=""
PASSW=${HR47_PGPASSWORD:-}
OWN_DB=0
NONEXIST=00000000-0000-0000-0000-00000000ffff

# 探针行标记（清理与残留自检都认它）
MARK="HR47-MATRIX-$TAG"

# ---------------------------------------------------------------- 6 个身份 ------
# 顺序即矩阵列序。admin 是唯一超管；其余 5 个由本脚本现建（每个只挂一个角色）。
ROLES=(SUPER_ADMIN HR_MANAGER FINANCE_MANAGER DEPT_LEADER EMPLOYEE FINANCE_DIRECTOR)
USERS=(admin hr47_hr hr47_fin hr47_lead hr47_emp hr47_findir)
PASSWORDS=("" "Hr47@hr123" "Hr47@fin123" "Hr47@lead123" "Hr47@emp123" "Hr47@dir123")
DEPT_OF=(A A A B A B)   # 人事/主管/员工在 A（含子部门 A1）；财务在 B

say() { printf '%s\n' "$*"; }
hr_line() { printf '%s\n' "-------------------------------------------------------------------------------"; }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 2; }

pass_line() { PASS=$((PASS + 1)); }
fail_line() { FAIL=$((FAIL + 1)); printf '  [FAIL] %s\n' "$1"; }
expect_eq() { # 名称 期望 实际
  if [ "$2" = "$3" ]; then pass_line; else fail_line "$1：期望 [$2]，实际 [$3]"; locate; fi
}
expect_ne() {
  if [ "$2" != "$3" ]; then pass_line; else fail_line "$1：期望不等于 [$2]"; locate; fi
}
expect_contains() { # 名称 海 针
  case "$2" in
    *"$3"*) pass_line ;;
    *) fail_line "$1：期望含「$3」，实际 [${2:0:300}]"; locate ;;
  esac
}

# 已知未修缺陷：断言「设计期望 vs 实测」。不符时打 [KNOWN]（附卡号与原因）而不是 [FAIL]，
# 修好后自动变 [PASS] 并提示把该条从 KNOWN_OPEN 摘掉 —— 不允许「静默跳过」。
KNOWN=0
known_open() { # 名称 设计期望 实测 卡号 原因
  if [ "$2" = "$3" ]; then
    pass_line
    printf '  [PASS] %s（已修复 → 请把这条从 KNOWN_OPEN 摘掉，卡 %s）\n' "$1" "$4"
  else
    KNOWN=$((KNOWN + 1))
    printf '  [KNOWN] %s：设计期望 [%s]，实测 [%s] —— %s（卡 %s）\n' "$1" "$2" "$3" "$5" "$4"
  fi
}

locate() {
  printf '         ↳ 请求：%s %s\n' "${LAST_METHOD:-?}" "${LAST_PATH:-?}"
  [ -n "${LAST_BODY:-}" ] && printf '         ↳ 请求体：%s\n' "${LAST_BODY:0:300}"
  printf '         ↳ HTTP %s，回执：%s\n' "${CODE:-?}" "$(tr -d '\n' <"$OUT" 2>/dev/null | cut -c1-400)"
  if [ -n "$SERVER_LOG" ] && [ -f "$SERVER_LOG" ]; then
    local err
    err=$(grep -E '(^| )(ERROR|panic)' "$SERVER_LOG" | tail -3 | cut -c1-200)
    [ -n "$err" ] && printf '         ↳ 服务端报错行：\n%s\n' "$(printf '%s\n' "$err" | sed 's/^/            /')"
  fi
  printf '         ↳ 库：%s@%s:%s/%s\n' "$DBUSER" "$DBHOST" "$DBPORT" "$DB"
}

# ---------------------------------------------------------------- HTTP ----------
req() { # METHOD PATH [BODY] [TOKEN]
  local method=$1 path=$2 body=${3:-} token=${4-$TOKEN}
  LAST_METHOD=$method
  LAST_PATH=$path
  LAST_BODY=$body
  local args=(-s -m 120 -o "$OUT" -w '%{http_code}' -X "$method" "$BASE$path"
              -H 'Content-Type: application/json')
  [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
  [ -n "$body" ] && args+=(-d "$body")
  CODE=$(curl "${args[@]}")
}
dat() { jq -r "(.data.$1) // (.$1) // empty" "$OUT" 2>/dev/null; }
err_msg() { jq -r '.msg // empty' "$OUT" 2>/dev/null; }
err_code() { jq -r '.code // empty' "$OUT" 2>/dev/null; }

login() { # 用户 口令 → 回显 token（失败回空）
  local u=$1 p=$2 tok must
  req POST /api/system/auth/login "{\"username\":\"$u\",\"password\":\"$p\"}" ""
  tok=$(jq -r '.data.access_token // empty' "$OUT" 2>/dev/null)
  must=$(jq -r '.data.must_change_password // false' "$OUT" 2>/dev/null)
  if [ -n "$tok" ] && [ "$must" = "true" ]; then
    # 首登必改密（安全审计 CODE-06）会让除「改密 / 登出」外的全部接口 403。
    # 本卡测的是权限矩阵，不掺改密语义 ⇒ 直接对演练库里的探针账号置 false 再重登。
    # （幂等：重跑时这一行是空更新；口令保持不变，避免「改过一次重跑就登不上」）
    q "UPDATE sys_user SET must_change_password = false WHERE username = '$u'" >/dev/null
    req POST /api/system/auth/login "{\"username\":\"$u\",\"password\":\"$p\"}" ""
    tok=$(jq -r '.data.access_token // empty' "$OUT" 2>/dev/null)
  fi
  printf '%s' "$tok"
}

# ---------------------------------------------------------------- 库 ----------
q() { PGPASSWORD="$PASSW" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -tAc "$1" 2>&1; }
qadm() { PGPASSWORD="$PASSW" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d postgres -tAc "$1" 2>&1; }
q_file() { PGPASSWORD="$PASSW" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -X -q -v ON_ERROR_STOP=1 -f "$1"; }
# 从 stdin 读 SQL（夹具用；ON_ERROR_STOP=1 ⇒ 写不进去就整段失败）
q_sql() { PGPASSWORD="$PASSW" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -X -q -v ON_ERROR_STOP=1 -f -; }

# ---------------------------------------------------------------- 参数 ----------
while [ $# -gt 0 ]; do
  case "$1" in
    --fresh) FRESH=1 ;;
    --help|-h) sed -n '2,60p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "未知参数：$1（只支持 --fresh / --help）" ;;
  esac
  shift
done

if ! [[ "$PORT" =~ ^[0-9]+$ ]] || [ "$PORT" -lt 18000 ] || [ "$PORT" -gt 18999 ]; then
  die "后端端口 $PORT 不在隔离段 18000-18999（prod 是 8023）"
fi
if ss -ltn 2>/dev/null | grep -q ":$PORT "; then
  die "端口 $PORT 已被占用，换一个（HR47_PORT=18xxx）"
fi

if [ -n "${HR_DB_URL:-}" ]; then
  FRESH=0
  url_no_scheme=${HR_DB_URL#postgres://}
  creds=${url_no_scheme%%@*}
  rest=${url_no_scheme#*@}
  DBUSER=${creds%%:*}
  PASSW=${creds#*:}
  DBHOST=${rest%%:*}
  rest2=${rest#*:}
  DBPORT=${rest2%%/*}
  DB=${rest2#*/}
  DB=${DB%/*}
fi

case "$DB" in
  *drill*|*Drill*|*e2e*|*E2E*|*test*|*Test*) ;;
  *) die "库名「$DB」不像演练库（需含 drill/e2e/test）。本脚本会写夹具/建账号，拒绝对非演练库动刀" ;;
esac

mkdir -p "$RUN_DIR"
SERVER_LOG=$RUN_DIR/server.log
MATRIX_TXT=$RUN_DIR/matrix.txt

cleanup() {
  local rc=$?
  if [ "$KEEP" != "1" ] && [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null
    for _ in $(seq 1 20); do kill -0 "$SERVER_PID" 2>/dev/null || break; sleep 1; done
    say "… 已停隔离实例（pid=$SERVER_PID）"
  fi
  [ -n "$PROFILE_FILE" ] && [ -f "$PROFILE_FILE" ] && rm -f "$PROFILE_FILE"
  if [ "$FRESH" = "1" ] && [ "$OWN_DB" = "1" ] && [ "$KEEP_DB" != "1" ]; then
    qadm "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    local left
    left=$(qadm "SELECT count(*) FROM pg_database WHERE datname = '$DB'" | tr -d '[:space:]')
    say "… 演练库已 DROP：$DB（pg_database 残留计数=${left:-?}）"
  fi
  rm -f "$OUT"
  exit $rc
}
trap cleanup EXIT INT TERM

# ============================================================ 前置：期望矩阵 ===
say "== HR-47 权限矩阵回归（30 码 × 6 角色 + SELF）=="
[ -x "$BIN" ] || die "后端可执行文件不存在：$BIN（先 cargo build --bin cloudpivot）"
[ -f "$EXPECT_PY" ] || die "缺期望矩阵解析器：$EXPECT_PY"
[ -f "$MIGRATION" ] || die "缺迁移文件：$MIGRATION"

EXPECT_JSON=$RUN_DIR/expect.json
python3 "$EXPECT_PY" --migration "$MIGRATION" --json >"$EXPECT_JSON" || die "期望矩阵解析失败"
EXPECT_CODES=$(jq -r '.codes[]' "$EXPECT_JSON")
N_CODES=$(printf '%s\n' "$EXPECT_CODES" | grep -c .)
N_BUTTONS=$(jq -r '.buttons | length' "$EXPECT_JSON")
[ "$N_BUTTONS" = "30" ] || die "期望矩阵解析出来的按钮码是 $N_BUTTONS 个（应 30）"
say "   期望矩阵来源：$MIGRATION"
say "   权限码：$N_CODES（页面 list 5 + 按钮 30）；角色：${ROLES[*]}"

if [ -z "$PASSW" ]; then
  PASSW=$(ssh -o ConnectTimeout=8 "root@$DBHOST" \
    'grep "^NIK_DATABASE_PASSWORD" /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-' 2>/dev/null)
  [ -n "$PASSW" ] || die "拿不到库口令：给 HR47_PGPASSWORD 或让 root@$DBHOST 免密 ssh 读 /etc/cloudpivot/cloudpivot-dev.env"
fi

if [ "$FRESH" = "1" ]; then
  say "==> 建演练库（先 DROP 同名残留）"
  qadm "DROP DATABASE IF EXISTS $DB" >/dev/null
  qadm "CREATE DATABASE $DB" >/dev/null 2>&1
  [ "$(qadm "SELECT count(*) FROM pg_database WHERE datname='$DB'" | tr -d '[:space:]')" = "1" ] \
    || die "建库失败：$DB（检查 $DBUSER 是否有 CREATEDB 权限）"
  OWN_DB=1
  say "   ✅ 库已建：$DB（空库，迁移由服务启动时自动回放）"
elif [ -z "${HR_DB_URL:-}" ]; then
  die "没有演练库：用 --fresh 建一个，或给 HR_DB_URL=postgres://user:***@host:port/<含drill的库名>"
fi

if [ "$DBPORT" != "5432" ]; then
  PROFILE_FILE=$BACKEND/config/config-hr47.toml
  printf '# HR-47 矩阵回归临时 profile（跑完由脚本删除）\n[database]\nport = %s\n' "$DBPORT" >"$PROFILE_FILE"
fi

say "==> 起隔离实例（127.0.0.1:$PORT，profile=$([ -n "$PROFILE_FILE" ] && echo hr47 || echo test)）"
(
  cd "$BACKEND" || exit 1
  NIK_PROFILE=$([ -n "$PROFILE_FILE" ] && echo hr47 || echo test) \
  NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
  NIK_DATABASE_HOST="$DBHOST" \
  NIK_DATABASE_USER="$DBUSER" \
  NIK_DATABASE_PASSWORD="$PASSW" \
  NIK_DATABASE_NAME="$DB" \
  NIK_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET="hr47-matrix-jwt-secret-0123456789abcdefghijklmnopqrstuvwxyz" \
  CLOUDPIVOT_STORAGE_MASTER_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" \
  X_CONFIG_SECRET_KEY="hr47-matrix-config-secret-0123456789" \
    setsid "$BIN" >"$SERVER_LOG" 2>&1 &
  echo $! >"$RUN_DIR/server.pid"
)
SERVER_PID=$(cat "$RUN_DIR/server.pid")
say "   等 /health（空库要先回放 300+ 条迁移，最长 10 分钟）…"
ready=0
for _ in $(seq 1 300); do
  code=$(curl -s -o /dev/null -m 3 -w '%{http_code}' "$BASE/health" || true)
  if [ "$code" = "200" ]; then ready=1; break; fi
  kill -0 "$SERVER_PID" 2>/dev/null || { tail -30 "$SERVER_LOG"; die "实例启动即退出（日志见上）"; }
  sleep 2
done
[ "$ready" = "1" ] || { tail -40 "$SERVER_LOG"; die "/health 300 次探测未就绪"; }
say "   ✅ /health 200（迁移完成）"
N_SEED=$(q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND auth_code LIKE 'business:hr:%'" | tr -d '[:space:]')
[ "$N_SEED" = "35" ] || die "迁移没跑全：sys_menu 里 business:hr:* 应有 35 个码，实际「$N_SEED」"
say "   ✅ 迁移在位：sys_menu 里 35 个人力资源权限码（V0387）"

# ============================================================ STEP 1 夹具 ======
hr_line
say "[STEP 1] 夹具：3 个部门 / 5 个账号 / 4 个员工 / 工资单与明细"
STEP1_FAIL=$FAIL

# --- 部门（A 根 → A1 子；B 根）---
q_sql <<SQL
INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, status, sort_order)
VALUES ('00000000-0000-0000-0000-0000004a0001'::uuid, 'HR47-A-$TAG',  'HR47 矩阵部A', NULL, 1, 900),
       ('00000000-0000-0000-0000-0000004a0002'::uuid, 'HR47-B-$TAG',  'HR47 矩阵部B', NULL, 1, 901),
       ('00000000-0000-0000-0000-0000004a0003'::uuid, 'HR47-A1-$TAG', 'HR47 矩阵科A1',
        '00000000-0000-0000-0000-0000004a0001'::uuid, 1, 902)
ON CONFLICT (id) DO NOTHING;
SQL
DEPT_A=$(q "SELECT id FROM sys_dept WHERE dept_code = 'HR47-A-$TAG' LIMIT 1" | tr -d '[:space:]')
DEPT_A1=$(q "SELECT id FROM sys_dept WHERE dept_code = 'HR47-A1-$TAG' LIMIT 1" | tr -d '[:space:]')
DEPT_B=$(q "SELECT id FROM sys_dept WHERE dept_code = 'HR47-B-$TAG' LIMIT 1" | tr -d '[:space:]')
[ -n "$DEPT_A" ] && [ -n "$DEPT_A1" ] && [ -n "$DEPT_B" ] \
  && { pass_line; say "  [ok] 部门：A=$DEPT_A / A1=$DEPT_A1 / B=$DEPT_B"; } \
  || { fail_line "部门建不出来：A=$DEPT_A A1=$DEPT_A1 B=$DEPT_B"; locate; }

# --- 角色 id（从库里取，不猜 UUID）---
role_id() { q "SELECT id FROM sys_role WHERE role_code = '$1' AND deleted_at IS NULL LIMIT 1" | tr -d '[:space:]'; }
declare -A RID
for r in "${ROLES[@]}"; do RID[$r]=$(role_id "$r"); done
miss=()
for r in "${ROLES[@]}"; do [ -n "${RID[$r]}" ] || miss+=("$r"); done
[ ${#miss[@]} -eq 0 ] && { pass_line; say "  [ok] 6 个角色都在库里（含 FINANCE_DIRECTOR 零授权对照）"; } \
  || { fail_line "库缺角色：${miss[*]}"; locate; }

# --- 5 个账号（admin 是超管，不新建）---
declare -A TOKENS
declare -A UID_MAP
TOKENS[SUPER_ADMIN]=$(login "$ADMIN_USER" "$ADMIN_PASSWORD")
[ -n "${TOKENS[SUPER_ADMIN]}" ] && { pass_line; say "  [ok] 超管登录"; } \
  || { fail_line "超管登录失败"; locate; }

for i in 1 2 3 4 5; do
  role=${ROLES[$i]}
  user=${USERS[$i]}
  pass=${PASSWORDS[$i]}
  dept=${DEPT_OF[$i]}
  case "$dept" in A) dept_uuid=$DEPT_A ;; B) dept_uuid=$DEPT_B ;; *) dept_uuid=$DEPT_A ;; esac
  req POST /api/system/user \
      "{\"username\":\"$user\",\"password\":\"$pass\",\"real_name\":\"$MARK $role\",\"dept_id\":\"$dept_uuid\",\"role_ids\":[\"${RID[$role]}\"],\"remark\":\"$MARK\"}" \
      "${TOKENS[SUPER_ADMIN]}"
  if [ "$CODE" = "200" ]; then
    UID_MAP[$role]=$(dat id)
    pass_line
  else
    # 账号已存在（重复跑）→ 复用
    existing=$(q "SELECT id FROM sys_user WHERE username = '$user' AND deleted_at IS NULL LIMIT 1" | tr -d '[:space:]')
    if [ -n "$existing" ]; then
      UID_MAP[$role]=$existing
      pass_line
      say "  [ok] 账号 $user 已存在 → 复用（$existing）"
    else
      fail_line "建账号 $user 失败（HTTP $CODE $(err_msg)）"
      locate
    fi
  fi
  TOKENS[$role]=$(login "$user" "$pass")
  [ -n "${TOKENS[$role]:-}" ] && pass_line || { fail_line "账号 $user 登录失败"; locate; }
done
say "  [ok] 5 个角色账号就绪（每个只挂 1 个角色）"
for r in "${ROLES[@]}"; do UID_MAP[$r]=${UID_MAP[$r]:-}; TOKENS[$r]=${TOKENS[$r]:-}; done
EMP_USER_ID=${UID_MAP[EMPLOYEE]:-}

# --- 4 个员工（走真接口，org_id / 工号取号 / 部门都按真实路径落库）---
mk_emp() { # 部门 姓名后缀
  local dept=$1 sfx=$2
  req POST /api/business/hr/employee \
      "{\"emp_no\":\"HR47-$TAG-$sfx\",\"name\":\"$MARK-$sfx\",\"gender\":1,\"dept_id\":\"$dept\",\"hire_date\":\"2024-01-01\",\"mobile\":\"1390000${sfx}00\"}" \
      "${TOKENS[SUPER_ADMIN]}"
  if [ "$CODE" = "200" ]; then
    dat id
  else
    # 重跑同一演练库：工号已存在（409/68001）→ 按工号查回 id，夹具保持幂等
    local found
    found=$(q "SELECT id FROM biz_employee WHERE emp_no = 'HR47-$TAG-$sfx' AND deleted_at IS NULL LIMIT 1" | tr -d '[:space:]')
    if [ -n "$found" ] && [ "${#found}" = 36 ]; then
      printf '%s' "$found"
    else
      fail_line "建员工 $sfx 失败（HTTP $CODE $(err_msg)）"; locate; printf ''
    fi
  fi
}
EMP1=$(mk_emp "$DEPT_A" 1)    # 绑定 EMPLOYEE 账号：本人
EMP2=$(mk_emp "$DEPT_A" 2)    # 同部门同事（无账号）
EMP3=$(mk_emp "$DEPT_A1" 3)   # 子部门
EMP4=$(mk_emp "$DEPT_B" 4)    # 别部门（数据范围之外的锚点）
[ -n "$EMP1" ] && [ -n "$EMP2" ] && [ -n "$EMP3" ] && [ -n "$EMP4" ] \
  && { pass_line; say "  [ok] 员工：A=$EMP1 / A=$EMP2 / A1=$EMP3 / B=$EMP4"; } \
  || fail_line "员工没建全：$EMP1 $EMP2 $EMP3 $EMP4"

# EMPLOYEE 账号绑到 EMP1（自助接口靠这个锚点认「我是谁」）
if [ -n "$EMP1" ] && [ -n "${TOKENS[SUPER_ADMIN]:-}" ]; then
  req POST "/api/business/hr/employee/$EMP1/bind-user" "{\"user_id\":\"${EMP_USER_ID}\"}" "${TOKENS[SUPER_ADMIN]}"
  [ "$CODE" = "200" ] && pass_line || { fail_line "绑账号失败（HTTP $CODE $(err_msg)）"; locate; }
fi

# --- 工资单（APPROVED）+ 三条明细：EMP1（本人）/ EMP2（同事）/ EMP4（别部门）---
q_sql <<SQL
BEGIN;
INSERT INTO biz_salary_sheet (id, org_id, sheet_no, period_code, period_start, period_end, status,
                              employee_count, earning_total, pre_deduct_total, gross_total,
                              withhold_total, net_total, remark)
VALUES ('$NONEXIST'::uuid, '00000000-0000-0000-0000-000000000000'::uuid, 'SAL-HR47-$TAG',
        '2026-10', '2026-10-01', '2026-10-31', 'APPROVED', 3,
        30000, 0, 30000, 3000, 27000, '$MARK')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_salary_detail (org_id, sheet_id, employee_id, employee_no, employee_name, dept_id, dept_name,
                               user_id, should_days, actual_days, earning_amount, pre_deduct_amount,
                               gross_amount, social_personal, fund_personal, iit_amount, other_withhold,
                               withhold_amount, net_amount, si_company, hf_company, remark)
VALUES
 ('00000000-0000-0000-0000-000000000000'::uuid, '$NONEXIST'::uuid, '$EMP1'::uuid, 'HR47-$TAG-1', '$MARK-SELF',
  '$DEPT_A'::uuid, 'HR47 矩阵部A', NULLIF('${EMP_USER_ID}','')::uuid, 21, 21, 12000, 0, 12000, 1000, 600, 200, 0, 1800, 10200, 2000, 800, '$MARK'),
 ('00000000-0000-0000-0000-000000000000'::uuid, '$NONEXIST'::uuid, '$EMP2'::uuid, 'HR47-$TAG-2', '$MARK-COLLEAGUE',
  '$DEPT_A'::uuid, 'HR47 矩阵部A', NULL, 21, 21, 10000, 0, 10000, 800, 500, 100, 0, 1400, 8600, 1600, 700, '$MARK'),
 ('00000000-0000-0000-0000-000000000000'::uuid, '$NONEXIST'::uuid, '$EMP4'::uuid, 'HR47-$TAG-4', '$MARK-OTHERDEPT',
  '$DEPT_B'::uuid, 'HR47 矩阵部B', NULL, 21, 21, 8000, 0, 8000, 600, 400, 50, 0, 1050, 6950, 1200, 500, '$MARK')
ON CONFLICT (sheet_id, employee_id) DO NOTHING;
COMMIT;
SQL
N_DET=$(q "SELECT count(*) FROM biz_salary_detail WHERE remark = '$MARK'" | tr -d '[:space:]')
[ "$N_DET" = "3" ] && { pass_line; say "  [ok] 工资单 + 3 条明细（本人 / 同事 / 别部门）"; } \
  || { fail_line "工资明细应有 3 条，实际「$N_DET」"; locate; }

# --- 请假单（EMP1 与 EMP2 各一张）+ 考勤记录（EMP1 一条）---
q_sql <<SQL
BEGIN;
INSERT INTO biz_leave_request (id, org_id, leave_no, employee_id, employee_no, employee_name, dept_id,
                               leave_type, start_time, end_time, duration_days, paid, status, reason)
VALUES (gen_random_uuid(), '00000000-0000-0000-0000-000000000000'::uuid, 'LV-HR47-$TAG-1',
        '$EMP1'::uuid, 'HR47-$TAG-1', '$MARK-SELF', '$DEPT_A'::uuid,
        'ANNUAL', '2026-10-08 09:00:00+08', '2026-10-08 18:00:00+08', 1, 1, 'APPROVED', '$MARK'),
       (gen_random_uuid(), '00000000-0000-0000-0000-000000000000'::uuid, 'LV-HR47-$TAG-2',
        '$EMP2'::uuid, 'HR47-$TAG-2', '$MARK-COLLEAGUE', '$DEPT_A'::uuid,
        'PERSONAL', '2026-10-09 09:00:00+08', '2026-10-09 18:00:00+08', 1, 0, 'APPROVED', '$MARK')
ON CONFLICT DO NOTHING;

INSERT INTO biz_attendance_period (id, org_id, period_code, period_start, period_end, status, remark)
VALUES ('00000000-0000-0000-0000-0000000000ff'::uuid, '00000000-0000-0000-0000-000000000000'::uuid,
        '2026-10', '2026-10-01', '2026-10-31', 'DRAFT', '$MARK')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_attendance_record (org_id, period_id, employee_id, employee_no, employee_name, dept_id,
                                   should_days, actual_days, remark)
VALUES ('00000000-0000-0000-0000-000000000000'::uuid, '00000000-0000-0000-0000-0000000000ff'::uuid,
        '$EMP1'::uuid, 'HR47-$TAG-1', '$MARK-SELF', '$DEPT_A'::uuid, 21, 20, '$MARK')
ON CONFLICT DO NOTHING;
COMMIT;
SQL
N_LV=$(q "SELECT count(*) FROM biz_leave_request WHERE reason = '$MARK'" | tr -d '[:space:]')
[ "$N_LV" = "2" ] && pass_line || { fail_line "请假单应有 2 张，实际「$N_LV」"; locate; }

[ "$FAIL" -eq "$STEP1_FAIL" ] && say "[STEP 1] ✅ 夹具就绪" || say "[STEP 1] ❌ 夹具有问题（后续层会连带失败）"

# ============================================================ STEP 2 层A 授权快照
hr_line
say "[STEP 2] 层 A：6 身份 × $N_CODES 码的授权快照（/api/system/auth/me）"
declare -A GRANTED   # "role|code" → 1/0（实际）
declare -A EXPECT    # "role|code" → 1/0（期望）
for r in "${ROLES[@]}"; do
  while read -r c; do [ -n "$c" ] && EXPECT["$r|$c"]=0; done <<<"$EXPECT_CODES"
  while read -r c; do [ -n "$c" ] && EXPECT["$r|$c"]=1; done <<<"$(jq -r --arg r "$r" '.roles[$r][]' "$EXPECT_JSON")"
done

for r in "${ROLES[@]}"; do
  tok=${TOKENS[$r]:-}
  if [ -z "$tok" ]; then fail_line "$r 没有令牌（登录失败）"; continue; fi
  req GET /api/system/auth/me "" "$tok"
  if [ "$CODE" != "200" ]; then fail_line "$r 的 /auth/me 不是 200（HTTP $CODE）"; locate; continue; fi
  perms=$RUN_DIR/perms-$r.txt
  jq -r '.data.permissions[]?' "$OUT" >"$perms"
  is_super=0
  grep -qx '\*:\*:\*' "$perms" && is_super=1
  for c in $EXPECT_CODES; do
    if [ "$is_super" = "1" ]; then
      GRANTED["$r|$c"]=1
    elif grep -qx "$c" "$perms"; then
      GRANTED["$r|$c"]=1
    else
      GRANTED["$r|$c"]=0
    fi
    if [ "${GRANTED["$r|$c"]}" != "${EXPECT["$r|$c"]}" ]; then
      fail_line "层A 授权漂移：$r 对 $c 期望 $([ "${EXPECT["$r|$c"]}" = 1 ] && echo 有码 || echo 无码)，实际 $([ "${GRANTED["$r|$c"]}" = 1 ] && echo 有码 || echo 无码)"
    else
      pass_line
    fi
  done
  say "  [ok] $r：令牌权限快照 $([ "$is_super" = 1 ] && echo '["*:*:*"]（超管通配）' || echo "$(grep -c . "$perms") 个码")"
done

# ============================================================ STEP 3 层B 路由闸
hr_line
say "[STEP 3] 层 B：6 身份 × 有路由的码（有码→非 403；无码→必须 403）"

# 码 → "METHOD|PATH|BODY"：一律挑**不改数据**的代表路由
# （不存在的 id ⇒ 允许时 404，拒绝时 403；GET 类优先；写类只打 400/404 分支）
declare -A ROUTE
ROUTE[business:hr:employee:list]="GET|/business/hr/employee/page|"
ROUTE[business:hr:employee:detail]="GET|/business/hr/employee/$NONEXIST|"
ROUTE[business:hr:employee:create]="GET|/business/hr/employee/next-no|"
ROUTE[business:hr:employee:update]="PUT|/business/hr/employee/$NONEXIST|{}"
ROUTE[business:hr:employee:delete]="DELETE|/business/hr/employee/$NONEXIST|"
ROUTE[business:hr:employee:sensitive]="GET|/business/hr/employee/$NONEXIST/sensitive?field=id_card|"
ROUTE[business:hr:employee:import]="GET|/business/hr/employee/import-template|"
ROUTE[business:hr:contract:manage]="PUT|/business/hr/contract/$NONEXIST|{}"
ROUTE[business:hr:attendance:list]="GET|/business/hr/attendance/period/page|"
ROUTE[business:hr:attendance:period]="POST|/business/hr/attendance/period/$NONEXIST/recalc|"
ROUTE[business:hr:attendance:record]="GET|/business/hr/attendance/record/page|"
ROUTE[business:hr:attendance:import]="POST|/business/hr/attendance/import|"
ROUTE[business:hr:attendance:export]="GET|/business/hr/attendance/export|"
ROUTE[business:hr:attendance:lock]="POST|/business/hr/attendance/period/$NONEXIST/status|{}"
ROUTE[business:hr:leave:list]="GET|/business/hr/leave/page|"
ROUTE[business:hr:leave:detail]="GET|/business/hr/leave/$NONEXIST|"
ROUTE[business:hr:leave:create]="POST|/business/hr/leave|{}"
ROUTE[business:hr:leave:update]="PUT|/business/hr/leave/$NONEXIST|{}"
ROUTE[business:hr:leave:submit]="POST|/business/hr/leave/$NONEXIST/submit|"
ROUTE[business:hr:leave:cancel]="POST|/business/hr/leave/$NONEXIST/cancel|{\"reason\":\"权限矩阵回归探针\"}"
ROUTE[business:hr:salary:list]="GET|/business/hr/salary/page|"
ROUTE[business:hr:salary:detail]="GET|/business/hr/salary/$NONEXIST|"
ROUTE[business:hr:salary:calc]="POST|/business/hr/salary/calc|{}"
ROUTE[business:hr:salary:submit]="POST|/business/hr/salary/$NONEXIST/submit|"
ROUTE[business:hr:salary:post]="POST|/business/hr/salary/$NONEXIST/post|"
ROUTE[business:hr:salary:reverse]="POST|/business/hr/salary/$NONEXIST/reverse|{\"reason\":\"权限矩阵回归探针\"}"
ROUTE[business:hr:salary:export]="GET|/business/hr/salary/$NONEXIST/export|"
ROUTE[business:hr:salary:my]="GET|/business/hr/salary/my|"
ROUTE[business:hr:salary-item:list]="GET|/business/hr/salary-item/page|"
ROUTE[business:hr:salary-item:create]="POST|/business/hr/salary-item|{}"
ROUTE[business:hr:salary-item:update]="POST|/business/hr/salary-item/validate|{}"

declare -A ROUTE_OK   # "role|code" → 1（行为符合期望）/ 0 / n（无路由）
declare -A NO_ROUTE
for c in $EXPECT_CODES; do
  [ -z "${ROUTE[$c]:-}" ] && NO_ROUTE[$c]=1
done

# 少数路由**光有本码不够**（handler 里还有第二道闸）。登记成 `码 → 还需要的码`，
# 层B 的期望就按「本码 + 第二道闸」一起算，否则矩阵会把合法的 403 判成「有码却被拒」。
#
#   salary:list  → salary:detail
#     页面码复用成全量列表接口码：EMPLOYEE 靠它让「薪资管理」页显形，但全公司薪资单
#     要求能看明细（见 salary_sheet_handler::ensure_full_list 的文档）。HR-47 实测补的闸。
declare -A ROUTE_ALSO_NEEDS
ROUTE_ALSO_NEEDS[business:hr:salary:list]="business:hr:salary:detail"

for r in "${ROLES[@]}"; do
  tok=${TOKENS[$r]:-}
  for c in $(printf '%s\n' "${!ROUTE[@]}" | sort); do
    spec=${ROUTE[$c]}
    IFS='|' read -r method path body <<<"$spec"
    expect=${EXPECT["$r|$c"]}
    downgraded=0
    # 第二道闸：本码在手但缺「配套码」→ 期望也是 403
    if [ "$expect" = "1" ] && [ -n "${ROUTE_ALSO_NEEDS[$c]:-}" ]; then
      extra=${ROUTE_ALSO_NEEDS[$c]}
      if [ "${GRANTED["$r|$extra"]:-0}" != "1" ]; then
        expect=0
        downgraded=1
      fi
    fi
    req "$method" "/api$path" "$body" "$tok"
    printf '%s\t%s\t%s %s\t%s\t%d\n' "$r" "$c" "$method" "$path" "$CODE" "${#tok}" >>"$RUN_DIR/cells.tsv"
    # 路由必须**真的存在**：宏路径漏 `/api` / 写错段 时也会 404，而「允许态」只看「非 403」，
    # 会把「路由不存在」当成通过 —— 这一条把这类假绿钉死（报文点名 请求的资源不存在）。
    if [ "$CODE" = "404" ] && [ "$(err_msg)" = "请求的资源不存在" ]; then
      fail_line "层B 路由不存在：$method /api$path 命中了 404 兜底（宏路径与脚本不一致？）"
      locate
      ROUTE_OK["$r|$c"]=0
      continue
    fi
    got_denied=0
    if [ "$CODE" = "403" ]; then got_denied=1; fi
    if [ "$expect" = "1" ]; then
      if [ "$got_denied" = "1" ]; then
        ROUTE_OK["$r|$c"]=0
        fail_line "层B 越权（反向）：$r 有 $c 却被 403 —— $method $path（$(err_msg)）"
        locate
      else
        ROUTE_OK["$r|$c"]=1
        pass_line
        [ "$CODE" = "500" ] && printf '  [WARN] %s 的 %s 通过权限后被 500（可能是真实缺陷）：%s %s\n' "$r" "$c" "$method" "$path"
      fi
    else
      if [ "$got_denied" = "0" ]; then
        ROUTE_OK["$r|$c"]=0
        if [ "$downgraded" = "1" ]; then
          fail_line "层B 第二道闸失效：$r 有 $c 但缺 $extra，$method $path 仍返回 HTTP $CODE（应 403）"
        else
          fail_line "层B 越权：$r 没有 $c，但 $method $path 返回 HTTP $CODE（应 403）"
        fi
        locate
      else
        ROUTE_OK["$r|$c"]=1
        pass_line
        expect_contains "层B $r 的 403 报文是「无权限」" "$(err_msg)" "无权限"
      fi
    fi
  done
done
say "  [ok] 层B 覆盖 $(( ${#ROUTE[@]} )) 个码 × 6 角色 = $(( ${#ROUTE[@]} * 6 )) 次真实路由"
say "  [note] 无后端路由的码（只由层A 覆盖，属已知缺口）：$(printf '%s ' "${!NO_ROUTE[@]}")"

# ============================================================ STEP 4 层C 数据范围
hr_line
say "[STEP 4] 层 C：SELF 只能看自己 + 部门范围（DEPT_AND_CHILD / ALL）"

# C1 EMPLOYEE 的 /salary/my 只出自己那一条
req GET /api/business/hr/salary/my "" "${TOKENS[EMPLOYEE]}"
expect_eq "C1 员工自助 /salary/my 200" "200" "$CODE"
MY_TOTAL=$(dat total)
MY_NOS=$(jq -r '[.data.list[]?.employee_no] | join(",")' "$OUT" 2>/dev/null)
expect_eq "C1 员工只看得到自己的 1 条工资条" "1" "$MY_TOTAL"
expect_eq "C1 且那条是本人（HR47-$TAG-1）" "HR47-$TAG-1" "$MY_NOS"
expect_ne "C1 不含同事那条" "HR47-$TAG-2" "$MY_NOS"
expect_ne "C1 不含别部门那条" "HR47-$TAG-4" "$MY_NOS"

# C2 传别人的 employee_id 也没用（入参里根本没有这个字段）
req GET "/api/business/hr/salary/my?employee_id=$EMP4&employee_no=HR47-$TAG-4" "" "${TOKENS[EMPLOYEE]}"
expect_eq "C2 带上别人的 employee_id 仍是 200" "200" "$CODE"
expect_eq "C2 仍只返回本人 1 条（参数无法越权）" "1" "$(dat total)"

# C3 员工越权打「全员」入口：一律 403
#     /salary/page 这一条是 HR-47 实测发现并**在 handler 里补了第二道闸**（salary:detail）：
#     V0387 把「薪资管理页」授给了 EMPLOYEE（让「我的薪资」tab 显形），页面行的 auth_code
#     就是 salary:list ⇒ 员工实际持有 list 码，只按码放行会把全公司工资单给员工。
req GET /api/business/hr/salary/page "" "${TOKENS[EMPLOYEE]}"
expect_eq "C3 员工打 /salary/page（全员薪资列表，需 salary:detail）" "403" "$CODE"
expect_contains "C3 报文引导走「我的薪资」" "$(err_msg)" "/business/hr/salary/my"
req GET "/api/business/hr/salary/$NONEXIST" "" "${TOKENS[EMPLOYEE]}"
expect_eq "C3 员工打 /salary/{id}（单头详情）" "403" "$CODE"
req GET "/api/business/hr/employee/$EMP4/salary" "" "${TOKENS[EMPLOYEE]}"
expect_eq "C3 员工看别人的薪资标准" "403" "$CODE"
req GET "/api/business/hr/employee/$EMP4" "" "${TOKENS[EMPLOYEE]}"
expect_eq "C3 员工看别人的档案" "403" "$CODE"
req GET /api/business/hr/employee/page "" "${TOKENS[EMPLOYEE]}"
expect_eq "C3 员工打全员档案列表" "403" "$CODE"

# C3b 员工打「全员请假列表」：**码在**（请假单页 grant），但缺 leave:detail ⇒ handler 强制
#     mine=true（卡 t_be54535c 的修法）⇒ 只出本人 1 条，同部门同事那条不许出现。
#     为什么不是 403：前端「我的请假」tab 走的正是本接口 + mine=true，一刀切 403 会把
#     员工自己的请假也挡掉（对照 /salary/page：它有 /salary/my 这条独立路径，可以 403）。
req GET "/api/business/hr/leave/page?page_size=100" "" "${TOKENS[EMPLOYEE]}"
expect_eq "C3b 员工 /leave/page（缺 leave:detail 被收窄，不是 403）" "200" "$CODE"
LV_TOTAL=$(dat total)
LV_NOS=$(jq -r '[.data.list[]?.employee_no] | join(",")' "$OUT" 2>/dev/null)
expect_contains "C3b 员工 /leave/page 含本人那条" "$LV_NOS" "HR47-$TAG-1"
expect_eq "C3b 员工 /leave/page 只出本人 1 条（缺 leave:detail ⇒ 强制 mine=true）" "1" "$LV_TOTAL"
expect_ne "C3b 且不含同部门同事那条" "HR47-$TAG-2" "$LV_NOS"
say "  [ok] C3b 员工 /leave/page：total=$LV_TOTAL，单号=[$LV_NOS]（只出本人）"

# C4 零授权角色（FINANCE_DIRECTOR）连自助码都没有 → /salary/my 403
req GET /api/business/hr/salary/my "" "${TOKENS[FINANCE_DIRECTOR]}"
expect_eq "C4 零授权角色 /salary/my" "403" "$CODE"
expect_contains "C4 报文点名缺 salary:my" "$(err_msg)" "business:hr:salary:my"

# C5 自助接口（设计 §5.2 #25：登录即可），不因角色不同而 403
for r in HR_MANAGER DEPT_LEADER EMPLOYEE; do
  req GET /api/business/hr/attendance/my "" "${TOKENS[$r]}"
  expect_eq "C5 $r 的 /attendance/my（登录即可）" "200" "$CODE"
done
# /leave/my 曾因「没有自己的权限规则」被兄弟模板规则 `/leave/{id}`（要 leave:detail）
# 以 score 2 命中 ⇒ 唯一该用它的角色（EMPLOYEE）反而 403。修法：路由挂
# `#[nik::auth_only]`（规则值 = AUTH_ONLY，score 0 精确胜出），见卡 t_be54535c。
for r in HR_MANAGER DEPT_LEADER EMPLOYEE; do
  req GET /api/business/hr/leave/my "" "${TOKENS[$r]}"
  expect_eq "C5 $r 的 /leave/my（登录即可）" "200" "$CODE"
done
# 员工自助的 /leave/my 只出本人（自助投影里没有 employee_no，用单号比对；单号前缀 LV- 是服务端取的）
req GET /api/business/hr/leave/my "" "${TOKENS[EMPLOYEE]}"
MY_LV_NOS=$(jq -r '[.data.list[]?.leave_no] | join(",")' "$OUT" 2>/dev/null)
expect_contains "C5 员工 /leave/my 含本人那条（LV-HR47-$TAG-1）" "$MY_LV_NOS" "LV-HR47-$TAG-1"
expect_ne "C5 员工 /leave/my 不含同部门同事那条（LV-HR47-$TAG-2）" "LV-HR47-$TAG-2" "$MY_LV_NOS"
expect_ne "C5 员工 /leave/my 不含别部门那条（LV-HR47-$TAG-4）" "LV-HR47-$TAG-4" "$MY_LV_NOS"
say "  [ok] C5 员工 /leave/my：HTTP $CODE，单号=[$MY_LV_NOS]（登录即可 + 只出本人）"

# C6 人事经理 data_scope=3（本部门及以下）：只见 A 与 A1，不见 B
req GET "/api/business/hr/employee/page?page_size=100" "" "${TOKENS[HR_MANAGER]}"
expect_eq "C6 人事经理 /employee/page 200" "200" "$CODE"
HR_NAMES=$(jq -r '[.data.list[]?.name] | join(",")' "$OUT" 2>/dev/null)
expect_contains "C6 人事经理看得到本部门员工（$MARK-1）" "$HR_NAMES" "$MARK-1"
expect_contains "C6 人事经理看得到子部门员工（$MARK-3）" "$HR_NAMES" "$MARK-3"
case "$HR_NAMES" in
  *"$MARK-4"*) fail_line "C6 人事经理（本部门及以下）看到了别部门员工 $MARK-4 —— 数据范围没生效"; locate ;;
  *) pass_line ;;
esac

# C7 部门主管 data_scope=1（全部）+ 有 employee:list → 全都能看（证明不是一刀切拒绝）
req GET "/api/business/hr/employee/page?page_size=100" "" "${TOKENS[DEPT_LEADER]}"
expect_eq "C7 部门主管 /employee/page 200" "200" "$CODE"
LEAD_NAMES=$(jq -r '[.data.list[]?.name] | join(",")' "$OUT" 2>/dev/null)
expect_contains "C7 部门主管看得到别部门员工（data_scope=1）" "$LEAD_NAMES" "$MARK-4"

# C8 财务经理：能看薪资单列表与单头（有 salary:list/detail），但看不到员工档案
req GET "/api/business/hr/salary/page?page_size=100" "" "${TOKENS[FINANCE_MANAGER]}"
expect_eq "C8 财务经理 /salary/page" "200" "$CODE"
expect_contains "C8 财务经理看得到那张工资单" "$(jq -r '[.data.list[]?.sheet_no] | join(",")' "$OUT" 2>/dev/null)" "SAL-HR47-$TAG"
req GET "/api/business/hr/employee/page" "" "${TOKENS[FINANCE_MANAGER]}"
expect_eq "C8 财务经理没有 employee:list → 403" "403" "$CODE"

# ============================================================ STEP 5 汇总 ======
hr_line
say "[STEP 5] 矩阵汇总（期望 G=有码 / .=无码；实际按层A 授权快照 + 层B 路由行为）"
{
  printf '%-44s' "权限码"
  for r in "${ROLES[@]}"; do printf '%-18s' "$r"; done
  printf '\n'
  printf -- '%.0s-' $(seq 1 $((44 + 18 * ${#ROLES[@]}))); printf '\n'
  n_cell=0; n_ok=0
  for c in $EXPECT_CODES; do
    printf '%-44s' "$c"
    for r in "${ROLES[@]}"; do
      exp=${EXPECT["$r|$c"]}
      act=${GRANTED["$r|$c"]:-0}
      n_cell=$((n_cell + 1))
      if [ "$exp" = "$act" ]; then n_ok=$((n_ok + 1)); mark=$([ "$exp" = 1 ] && echo "G" || echo ".") ; else mark="!"; fi
      if [ -n "${ROUTE[$c]:-}" ]; then
        [ "${ROUTE_OK["$r|$c"]:-0}" = "1" ] && mark="$mark" || mark="$mark(BAD)"
      else
        mark="$mark(-)"
      fi
      printf '%-18s' "$mark"
    done
    printf '\n'
  done
  printf -- '%.0s-' $(seq 1 $((44 + 18 * ${#ROLES[@]}))); printf '\n'
  printf '%-44s' "合计（期望码数/实际码数）"
  for r in "${ROLES[@]}"; do
    e=0; a=0
    for c in $EXPECT_CODES; do
      [ "${EXPECT["$r|$c"]}" = 1 ] && e=$((e + 1))
      [ "${GRANTED["$r|$c"]}" = 1 ] && a=$((a + 1))
    done
    printf '%-18s' "$e/$a"
  done
  printf '\n'
  printf '矩阵格数 %s（%s 码 × %s 角色）；授权一致的格数 %s\n' "$n_cell" "$N_CODES" "${#ROLES[@]}" "$n_ok"
  printf '路由闸格数 %s（%s 码 × 6 角色，含「无码→403」与「有码→非 403」两个方向）\n' "$(( ${#ROUTE[@]} * 6 ))" "${#ROUTE[@]}"
  printf '无后端路由的码：%s\n' "$(printf '%s ' "${!NO_ROUTE[@]}")"
} | tee "$MATRIX_TXT"

# 残留自检：夹具行必须能被 tag 找到（说明清理得干净）
hr_line
say "== 残留自检（$MARK）=="
say "  sys_dept:        $(q "SELECT count(*) FROM sys_dept WHERE dept_code LIKE 'HR47-%' AND deleted_at IS NULL" | tr -d '[:space:]')"
say "  sys_user:        $(q "SELECT count(*) FROM sys_user WHERE username LIKE 'hr47_%' AND deleted_at IS NULL" | tr -d '[:space:]')"
say "  biz_employee:    $(q "SELECT count(*) FROM biz_employee WHERE remark = '$MARK' AND deleted_at IS NULL" | tr -d '[:space:]')"
say "  biz_salary_*:    $(q "SELECT count(*) FROM biz_salary_detail WHERE remark = '$MARK'" | tr -d '[:space:]')"
say "  biz_leave:       $(q "SELECT count(*) FROM biz_leave_request WHERE reason = '$MARK'" | tr -d '[:space:]')"

hr_line
printf '总断言：PASS=%s FAIL=%s KNOWN（已知未修，不计失败）=%s\n' "$PASS" "$FAIL" "$KNOWN"
if [ "$FAIL" -eq 0 ]; then
  say "✅ 人力资源权限矩阵回归全绿（$N_CODES 码 × 6 角色 + SELF/部门范围）"
  [ "$KNOWN" -gt 0 ] && say "   ⚠ 另有 $KNOWN 条 [KNOWN] 已知未修（见上，每条的卡号与原因已打印）—— 它们不是静默跳过"
  say "   矩阵明细：$MATRIX_TXT"
  exit 0
else
  say "❌ 有 $FAIL 条断言不通过（详见上面 [FAIL] 行）"
  exit 1
fi
