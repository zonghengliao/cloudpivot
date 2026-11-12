#!/usr/bin/env bash
# =============================================================================
# HR-46 · 薪资链路端到端 E2E（一条命令跑完 10 步）
# =============================================================================
#
# 卡面：「建员工→导入考勤→请假→试算→审批→过账→红冲→对账（10 步）」
# 设计：`docs/architecture/ARCH-9-人力资源模块设计.md`
#       §3 数据模型 / §4 业务规则（§4.3 审批、§4.6 工资凭证勾稽、§4.9 巡检断言）
#       / §5 接口设计（42+4 条 API）
#
# ─── 十步与断言（每一步都断言，失败打「哪一步 / 哪个请求 / 返回什么 / 服务端日志尾」）───
#
#  #  步骤                                   断言要点
#  1  建员工（档案 + 劳动合同 + 薪资标准）     工号取号 / 合同生效 / 标准 3 项落库
#  2  建考勤期间（2026-11）                   期间 DRAFT、区间为自然月
#  3  导入考勤（真实 xlsx → impexp → 确认）    预检 total=1 error=0 / 入库 1 行 /
#                                            台账 21 出勤 10 加班小时 / **期间头汇总被重算**
#  4  请假（建单 → 提交 → 直属上级审批）        DRAFT→SUBMITTED→APPROVED、实例终态 APPROVED
#  5  锁定考勤期间                            DRAFT→SUBMITTED→LOCKED（过账前置闸）
#  6  试算（dry_run）                         逐人逐项金额（含公式项）/ **库里零行、不耗号段**
#  7  建单（落库）                            CALCULATED / 应发实发与试算一致 / 明细+明细项入库
#  8  审批（提交 → 人事经理 → 财务会签）        SUBMITTED→APPROVED / **审批通过不出凭证**（§4.3）
#  9  过账 + 红冲                             POSTED（两张蓝字，借贷平衡）/ REVERSED（两张红字，
#                                            借贷互换 + reverse_of/reversed_by）/ 二次红冲 409
# 10  对账                                    4 张凭证**逐科目净额 0** + §4.9 的 A6/A7 恒等式
#                                            + `hr_invariants.sh` 的 10 条巡检断言全绿
#
# ─── 为什么是「真 HTTP + 真库 + 真实例」而不是直调函数 ────────────────────────
#
# 直调 handler 函数能证明业务编排，但证明不了这四件事，而它们恰恰是线上故障的来源：
#
#   ① 路由真的注册上了、权限码真的能过（`#[nik::permission]` 拼错只会在线上 403）；
#   ② JSON 字段名与前端/脚本消费的形状一致（DTO 改名不会编译失败）；
#   ③ 审批是真的走**流程引擎**（多节点 / 会签 / 条件边），不是一个回调函数自说自话；
#   ④ 过账写的是 `biz_voucher` 的真实凭证 — 借贷平衡 / 科目 / 部门辅助核算 / 红冲指针，
#      只有落在库里才能对账（本脚本第 10 步就是拿 SQL 对）。
#
# ─── 演练库纪律（卡面 2026-11-12 强制）─────────────────────────────────────────
#
# * 演练库**只建在 145（172.18.10.145）**，绝不在 99 上建 —— 99 是 Agent 主机；
# * 库名必须含 `drill` / `e2e` / `test`（脚本开头硬校验，不匹配直接退出 2）；
# * `--fresh` 模式自己建库、跑完自己 DROP（`KEEP_DB=1` 保留现场排查）；
# * 给了 `HR_DB_URL` 则只跑不清（那是别人给的库，脚本不替你删）。
#
# ─── 用法 ────────────────────────────────────────────────────────────────────
#
#   bash scripts/verify/hr/hr_salary_e2e.sh --fresh        # 一条命令：建库 → 起实例 → 10 步 → DROP
#   HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr46_drill \
#       bash scripts/verify/hr/hr_salary_e2e.sh            # 用现成演练库（跑完不删）
#   KEEP=1 KEEP_DB=1 bash ... --fresh                      # 留实例与库（排查用）
#
# 环境变量
#   HR46_PGPASSWORD   145 的库口令（缺省从 145 的 /etc/cloudpivot/cloudpivot-dev.env 经 ssh 读）
#   HR46_DBHOST/PORT  库地址（默认 172.18.10.145:5432）
#   HR46_DBUSER       库账号（默认 cloudpivot，need CREATE DATABASE）
#   HR46_PORT         后端端口（默认 18146，必须在 18000-18999 隔离段）
#   HR46_BIN          后端可执行文件（默认 backend/target/debug/cloudpivot）
#   HR46_BACKEND      后端目录（默认 <repo>/backend；跑真库迁移用）
#   HR46_RUN_DIR      运行目录（默认 /root/hr46-e2e-<时间戳>；日志、xlsx 都在这）
#   ADMIN_PASSWORD    超管初始口令（默认 Admin@123456；`--fresh` 新库由 bootstrap 种下）
#   KEEP=1            不杀实例；KEEP_DB=1 不删库
#
# 退出码：0 = 10 步全绿；1 = 有断言不通过；2 = 环境/前置不满足（库、端口、二进制、登录）
# =============================================================================
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$HERE/../../.." && pwd)
BACKEND=${HR46_BACKEND:-$REPO_ROOT/backend}
BIN=${HR46_BIN:-$BACKEND/target/debug/cloudpivot}
XLSX_PY=$HERE/hr_e2e_xlsx.py

TAG=$(date +%m%d-%H%M%S)
PORT=${HR46_PORT:-18146}
BASE="http://127.0.0.1:$PORT"
DBHOST=${HR46_DBHOST:-172.18.10.145}
DBPORT=${HR46_DBPORT:-5432}
DBUSER=${HR46_DBUSER:-cloudpivot}
DB=${HR46_DB:-hr46_drill_$(date +%m%d%H%M%S)}
ADMIN_USER=admin
ADMIN_PASSWORD=${ADMIN_PASSWORD:-Admin@123456}
APPROVER=hr46_approver
KEEP=${KEEP:-0}
KEEP_DB=${KEEP_DB:-0}
FRESH=0

# 薪资期间：必须在 V0221 种下的「当年 12 个会计期间」里（新库只有当年），
# 否则过账会被期间闸拦下（那是对的，但会让 E2E 误报）。用当月。
SALARY_PERIOD=$(date +%Y-%m)
ATT_PERIOD=$SALARY_PERIOD
ATT_START="$(date +%Y-%m)-01"
ATT_END="$(date -d "$(date +%Y-%m)-01 +1 month -1 day" +%Y-%m-%d)"
LEAVE_DAY="$(date +%Y-%m)-20"

# 薪资夹具金额（第 6/7/8/9 步的期望值都由它们推出来，改这里就够）
BASE_AMT=12000
POST_AMT=2000
MEAL_AMT=600
OT_WD_HOURS=10
SI_PERSONAL=1200
HF_PERSONAL=800
IIT=300
SI_COMPANY=2400
HF_COMPANY=800
# OT_WD = ROUND(BASE/21.75/8*10*1.5, 2) = 1034.48；全勤奖 = 200；应发 = 15834.48；实发 = 13534.48
EXPECT_OT_WD=1034.48
EXPECT_ATT_BONUS=200
EXPECT_GROSS=15834.48
EXPECT_NET=13534.48
EXPECT_ACCRUAL=19034.48

PASS=0
FAIL=0
STEP_NO=0
STEP_NAME=""
STEP_FAILS=0
STEP_FAILED_LIST=()
OUT=$(mktemp)
TOKEN=""
CODE=""
SERVER_PID=""
PROFILE_FILE=""
RUN_DIR=${HR46_RUN_DIR:-/root/hr46-e2e-$(date +%Y%m%d-%H%M%S)}
SERVER_LOG=""
PASSW="${HR46_PGPASSWORD:-}"
OWN_DB=0

say() { printf '%s\n' "$*"; }
hr() { printf '%s\n' "-------------------------------------------------------------------------------"; }
die() {
  printf 'FATAL: %s\n' "$*" >&2
  exit 2
}

# ---------------------------------------------------------------- 断言 ----------
pass_line() { PASS=$((PASS + 1)); printf '  [PASS] %s\n' "$1"; }
fail_line() {
  FAIL=$((FAIL + 1))
  STEP_FAILS=$((STEP_FAILS + 1))
  printf '  [FAIL] %s\n' "$1"
}
expect_eq() { # 名称 期望 实际
  if [ "$2" = "$3" ]; then pass_line "$1（$3）"; else fail_line "$1：期望 [$2]，实际 [$3]"; locate; fi
}
expect_ne() {
  if [ "$2" != "$3" ]; then pass_line "$1（$3）"; else fail_line "$1：期望不等于 [$2]，实际相同"; locate; fi
}
expect_contains() { # 名称 海 针
  case "$2" in
    *"$3"*) pass_line "$1（含「$3」）" ;;
    *) fail_line "$1：期望含「$3」，实际 [${2:0:300}]"; locate ;;
  esac
}
expect_true() { # 名称 值（"t"/"true"/1）
  case "$2" in
    t|true|1) pass_line "$1（$2）" ;;
    *) fail_line "$1：期望真值，实际 [$2]"; locate ;;
  esac
}
expect_numeric() { # 名称 期望 实际（按 2 位小数比，容忍 NUMERIC 尾部零）
  local want act
  want=$(printf '%.2f' "$2" 2>/dev/null) || want=$2
  act=$(printf '%.2f' "$3" 2>/dev/null) || act=$3
  if [ "$want" = "$act" ]; then pass_line "$1（$act）"; else fail_line "$1：期望 $want，实际 [$3]"; locate; fi
}

step() { # 序号 名称
  STEP_NO=$1
  STEP_NAME=$2
  STEP_FAILS=0
  hr
  printf '[STEP %s] %s\n' "$STEP_NO" "$STEP_NAME"
  STEP_FAILED_LIST+=("$STEP_NO|$STEP_NAME|0")
}

step_end() {
  local idx=$((${#STEP_FAILED_LIST[@]} - 1))
  STEP_FAILED_LIST[$idx]="${STEP_NO}|${STEP_NAME}|${STEP_FAILS}"
  if [ "$STEP_FAILS" -eq 0 ]; then
    printf '[STEP %s] ✅ %s\n' "$STEP_NO" "$STEP_NAME"
  else
    printf '[STEP %s] ❌ %s（%s 条断言失败）\n' "$STEP_NO" "$STEP_NAME" "$STEP_FAILS"
  fi
}

# 失败定位：把「当前请求 / 回执 / 服务端日志尾」一次打全，省掉手工再跑一遍
locate() {
  printf '         ↳ 请求：%s %s\n' "${LAST_METHOD:-?}" "${LAST_PATH:-?}"
  if [ -n "${LAST_BODY:-}" ]; then printf '         ↳ 请求体：%s\n' "${LAST_BODY:0:400}"; fi
  printf '         ↳ HTTP %s，回执：%s\n' "${CODE:-?}" "$(tr -d '\n' <"$OUT" 2>/dev/null | cut -c1-500)"
  if [ -n "$SERVER_LOG" ] && [ -f "$SERVER_LOG" ]; then
    local err
    err=$(grep -E '(^| )(ERROR|panic)' "$SERVER_LOG" | tail -4 | cut -c1-200)
    [ -n "$err" ] && printf '         ↳ 服务端报错行：\n%s\n' "$(printf '%s\n' "$err" | sed 's/^/            /')"
    printf '         ↳ 服务端日志尾：%s\n' "$SERVER_LOG"
  fi
  [ -n "${EMP_ID:-}" ] && printf '         ↳ 夹具：员工=%s 期间=%s 单据=%s\n' "$EMP_ID" "${PERIOD_ID:-}" "${SHEET_ID:-}"
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

req_anon() { req "$1" "$2" "${3:-}" ""; }

jqv() { # jq 表达式（先 .data 再回落顶层：信封是 {"code":0,"data":{…}}）
  jq -r "$1 // empty" "$OUT" 2>/dev/null
}
dat() { # 取 data 下的字段（自动回落顶层）
  jq -r "(.data.$1) // (.$1) // empty" "$OUT" 2>/dev/null
}
err_msg() { jq -r '.msg // empty' "$OUT" 2>/dev/null; }

login() { # 用户名 口令 → 设置 TOKEN
  req POST /api/system/auth/login "{\"username\":\"$1\",\"password\":\"$2\"}"
  TOKEN=$(jq -r '.data.access_token // empty' "$OUT" 2>/dev/null)
  [ -n "$TOKEN" ]
}

# ---------------------------------------------------------------- 库 ----------
q() { PGPASSWORD="$PASSW" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -tAc "$1" 2>&1; }
qadm() { PGPASSWORD="$PASSW" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d postgres -tAc "$1" 2>&1; }
q_file() { PGPASSWORD="$PASSW" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -X -q -v ON_ERROR_STOP=1 -f "$1" >/dev/null 2>&1; }

# ============================================================== 前置：参数 =====
while [ $# -gt 0 ]; do
  case "$1" in
    --fresh) FRESH=1 ;;
    --help|-h) sed -n '2,70p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "未知参数：$1（只支持 --fresh / --help）" ;;
  esac
  shift
done

# 端口隔离段（与 scripts/verify/e2e_isolated_instance.sh 同一判据）
if ! [[ "$PORT" =~ ^[0-9]+$ ]] || [ "$PORT" -lt 18000 ] || [ "$PORT" -gt 18999 ]; then
  die "后端端口 $PORT 不在隔离段 18000-18999（prod 是 8023）"
fi
if ss -ltn 2>/dev/null | grep -q ":$PORT "; then
  die "端口 $PORT 已被占用，换一个（HR46_PORT=18xxx）"
fi

# 给了 HR_DB_URL 就整条链路都用它（不建库、不删库）
if [ -n "${HR_DB_URL:-}" ]; then
  FRESH=0
  url_no_scheme=${HR_DB_URL#postgres://}
  creds=${url_no_scheme%%@*}
  rest=${url_no_scheme#*@}
  DBUSER_CFG=${creds%%:*}
  PASSW=${creds#*:}
  DBHOST_CFG=${rest%%:*}
  rest2=${rest#*:}
  DBPORT_CFG=${rest2%%/*}
  DB=${rest2#*/}
  DB=${DB%/*}
  DBUSER=$DBUSER_CFG
  DBHOST=$DBHOST_CFG
  DBPORT=$DBPORT_CFG
fi

# 演练库护栏：名称必须含 drill / e2e / test —— 这条先于任何写操作
case "$DB" in
  *drill*|*Drill*|*e2e*|*E2E*|*test*|*Test*) ;;
  *) die "库名「$DB」不像演练库（需含 drill/e2e/test）。本脚本会建库/写数据，拒绝对非演练库动刀" ;;
esac

mkdir -p "$RUN_DIR"
SERVER_LOG=$RUN_DIR/server.log

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
  exit $rc
}
trap cleanup EXIT INT TERM

# ====================================================== 前置：库 / 二进制 / 实例 ===
say "== HR-46 薪资链路 E2E（10 步）=="
say "   演练库：$DBUSER@$DBHOST:$DBPORT/$DB    后端：$BASE"
say "   运行目录：$RUN_DIR"

[ -x "$BIN" ] || die "后端可执行文件不存在：$BIN（先 cargo build --bin cloudpivot）"
[ -f "$XLSX_PY" ] || die "缺 xlsx 生成器：$XLSX_PY"

if [ -z "$PASSW" ]; then
  # 口令不写进卡面/仓库：从 145 的私有 env 读（root 免密 ssh）
  PASSW=$(ssh -o ConnectTimeout=8 root@"$DBHOST" \
    'grep "^NIK_DATABASE_PASSWORD" /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-' 2>/dev/null)
  [ -n "$PASSW" ] || die "拿不到库口令：给 HR46_PGPASSWORD 或让 root@$DBHOST 免密可 ssh 读 /etc/cloudpivot/cloudpivot-dev.env"
fi

if [ "$FRESH" = "1" ]; then
  say "==> 建演练库（先 DROP 同名残留）"
  qadm "DROP DATABASE IF EXISTS $DB" >/dev/null
  qadm "CREATE DATABASE $DB" >/dev/null 2>&1
  if [ "$(qadm "SELECT count(*) FROM pg_database WHERE datname='$DB'" | tr -d '[:space:]')" != "1" ]; then
    die "建库失败：$DB（检查 $DBUSER 是否有 CREATEDB 权限）"
  fi
  OWN_DB=1
  say "   ✅ 库已建：$DB（空库，迁移由服务启动时自动回放）"
elif [ -z "${HR_DB_URL:-}" ]; then
  die "没有演练库：用 --fresh 让脚本在 $DBHOST 上建一个，或给 HR_DB_URL=postgres://user:pw@host:port/<含drill的库名>"
fi

# 端口不在 5432 时用临时 profile 覆盖（nik-config 不映射 NIK_DATABASE_PORT）
if [ "$DBPORT" != "5432" ]; then
  PROFILE_FILE=$BACKEND/config/config-hr46.toml
  printf '# HR-46 E2E 临时 profile（跑完由脚本删除）\n[database]\nport = %s\n' "$DBPORT" >"$PROFILE_FILE"
fi

say "==> 起隔离实例（127.0.0.1:$PORT，profile=$([ -n "$PROFILE_FILE" ] && echo hr46 || echo test)）"
JWT_SECRET="hr46-e2e-jwt-secret-0123456789abcdefghijklmnopqrstuvwxyz"
STORAGE_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
(
  cd "$BACKEND" || exit 1
  NIK_PROFILE=$([ -n "$PROFILE_FILE" ] && echo hr46 || echo test) \
  NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
  NIK_DATABASE_HOST="$DBHOST" \
  NIK_DATABASE_USER="$DBUSER" \
  NIK_DATABASE_PASSWORD="$PASSW" \
  NIK_DATABASE_NAME="$DB" \
  NIK_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET="$JWT_SECRET" \
  CLOUDPIVOT_STORAGE_MASTER_KEY="$STORAGE_KEY" \
  X_CONFIG_SECRET_KEY="hr46-e2e-config-secret-0123456789" \
    setsid "$BIN" >"$SERVER_LOG" 2>&1 &
  echo $! >"$RUN_DIR/server.pid"
)
SERVER_PID=$(cat "$RUN_DIR/server.pid")

say "   等 /health（空库要先回放 300+ 条迁移，最长 10 分钟）…"
ready=0
for i in $(seq 1 300); do
  code=$(curl -s -o /dev/null -m 3 -w '%{http_code}' "$BASE/health" || true)
  if [ "$code" = "200" ]; then ready=1; break; fi
  kill -0 "$SERVER_PID" 2>/dev/null || { tail -30 "$SERVER_LOG"; die "实例启动即退出（日志见上）"; }
  sleep 2
done
[ "$ready" = "1" ] || { tail -40 "$SERVER_LOG"; die "/health 300 次探测未就绪"; }
say "   ✅ /health 200（迁移完成）"

# 迁移回放必须真的跑过：17 个薪资项种子 = V0396 + V0398 自检通过（否则启动就失败了）
seeds=$(q "SELECT count(*) FROM biz_salary_item WHERE deleted_at IS NULL" | tr -d '[:space:]')
if [ "$seeds" != "17" ]; then
  die "迁移没跑全：biz_salary_item 应有 17 个种子项，实际「$seeds」（看 $SERVER_LOG）"
fi
say "   ✅ 迁移在位：17 个薪资项种子（V0396）+ V0398 收口自检已通过"

# ---------------------------------------------------------------- 夹具（SQL）---
# 清掉首登改密（演练库夹具，不经改密接口 —— 与 HR-23 HTTP 冒烟同一做法）
q "UPDATE sys_user SET must_change_password = false, is_locked = false, login_fail_count = 0 WHERE username = '$ADMIN_USER'" >/dev/null

# 审批人：一个夹具账号同时持 HR_MANAGER / FINANCE_MANAGER / FINANCE_DIRECTOR
# （薪资流 n_hr 用 HR_MANAGER，n_fin 是 COUNTERSIGN ALL 绑财务经理+财务总监；
#   三层去重后候选人只有一个 → 两签都由它投，会签语义不退化）
# 密码哈希直接抄 admin 的，所以口令与超管一致（不动任何密文逻辑）。
q "INSERT INTO sys_user (username, password, real_name, status)
   SELECT '$APPROVER', password, 'HR46 审批人（人事/财务经理/财务总监）', 1
     FROM sys_user WHERE username = '$ADMIN_USER' AND deleted_at IS NULL
    AND NOT EXISTS (SELECT 1 FROM sys_user WHERE username = '$APPROVER' AND deleted_at IS NULL)" >/dev/null
q "UPDATE sys_user SET must_change_password = false, is_locked = false, login_fail_count = 0, status = 1
    WHERE username = '$APPROVER'" >/dev/null
roles=$(q "SELECT count(*) FROM sys_role WHERE role_code IN ('HR_MANAGER','FINANCE_MANAGER','FINANCE_DIRECTOR') AND deleted_at IS NULL" | tr -d '[:space:]')
[ "$roles" = "3" ] || die "审批角色种子不全：HR_MANAGER/FINANCE_MANAGER/FINANCE_DIRECTOR 只找到 $roles 个"
q "INSERT INTO sys_user_role (user_id, role_id)
   SELECT u.id, r.id FROM sys_user u, sys_role r
    WHERE u.username = '$APPROVER' AND r.role_code IN ('HR_MANAGER','FINANCE_MANAGER','FINANCE_DIRECTOR')
   ON CONFLICT DO NOTHING" >/dev/null
q "INSERT INTO sys_role_menu (role_id, menu_id)
   SELECT r.id, m.id FROM sys_role r, sys_menu m
    WHERE r.role_code IN ('HR_MANAGER','FINANCE_MANAGER','FINANCE_DIRECTOR')
      AND m.auth_code IN ('business:flow:approve','business:flow:instance:read')
   ON CONFLICT DO NOTHING" >/dev/null
# 请假流的「直属上级」= 发起人所在部门负责人 → 把根部门负责人设成审批人
q "UPDATE sys_dept SET leader_id = (SELECT id FROM sys_user WHERE username = '$APPROVER')
    WHERE dept_code = 'HQ' AND deleted_at IS NULL" >/dev/null
say "   ✅ 审批夹具：$APPROVER（3 个审批角色 + flow:approve）+ HQ 部门负责人"

# ---------------------------------------------------------------- 登录 ----------
if ! login "$ADMIN_USER" "$ADMIN_PASSWORD"; then
  tail -40 "$SERVER_LOG"
  die "admin 登录失败（口令对不对？日志见上）"
fi
say "   ✅ admin 登录（SUPER_ADMIN 短路全部权限码）"

# 取根部门（V0366 在空库种下「总部」HQ，bootstrap 把 admin 落进去）
req GET /api/system/dept/tree
DEPT_ID=$(jq -r '(.data[0].id) // (.data[0].children[0].id) // empty' "$OUT" 2>/dev/null)
[ -n "$DEPT_ID" ] || die "库里没有部门，无法建员工（$(tr -d '\n' <"$OUT" | cut -c1-200)）"

# 审批一组待办：按库里真实的 assignee 逐个登录投票（不假设节点数 / 是否会签）
approve_pending() { # 实例 id → 0 全部签完 / 1 卡住
  local instance=$1 round left
  for round in $(seq 1 10); do
    left=$(q "SELECT count(*) FROM biz_flow_task
               WHERE instance_id = '$instance'::uuid AND status = 'PENDING'" | tr -d '[:space:]')
    [ "$left" = "0" ] && return 0
    # 待办 → (task_id, 用户名)：一条查询，避免 N+1
    while IFS='|' read -r task_id uname; do
      [ -z "$task_id" ] && continue
      local tok
      req POST /api/system/auth/login "{\"username\":\"$uname\",\"password\":\"$ADMIN_PASSWORD\"}"
      tok=$(jq -r '.data.access_token // empty' "$OUT" 2>/dev/null)
      if [ -z "$tok" ]; then
        fail_line "审批人 $uname 登录失败"
        return 1
      fi
      req POST "/api/business/flow/task/$task_id/approve" '{"comment":"HR-46 E2E 自动审批"}' "$tok"
      if [ "$CODE" != "200" ]; then
        fail_line "待办 $task_id（$uname）审批失败：HTTP $CODE $(err_msg)"
        return 1
      fi
      pass_line "待办已签：$uname"
    done <<EOF
$(q "SELECT t.id || '|' || u.username FROM biz_flow_task t JOIN sys_user u ON u.id = t.assignee_id
      WHERE t.instance_id = '$instance'::uuid AND t.status = 'PENDING' ORDER BY t.created_at")
EOF
  done
  return 1
}

# ============================================================ STEP 1 建员工 ====
step 1 "建员工（档案 + 劳动合同 + 薪资标准）"
EMP_NAME="HR46工资员工$TAG"
req POST /api/business/hr/employee "{\"name\":\"$EMP_NAME\",\"gender\":1,\"dept_id\":\"$DEPT_ID\",\"hire_date\":\"$(date +%Y)-01-01\"}"
expect_eq "1. 建员工 HTTP" 200 "$CODE"
EMP_ID=$(dat id)
EMP_NO=$(dat emp_no)
expect_ne "1. 自动取号（工号非空）" "" "$EMP_NO"
case "$EMP_NO" in
  EMP-*) pass_line "1. 工号号段 EMP-（$EMP_NO）" ;;
  *) fail_line "1. 工号号段不对：$EMP_NO"; locate ;;
esac
[ -n "$EMP_ID" ] || die "建员工没返回 id：$(tr -d '\n' <"$OUT" | cut -c1-300)"

req POST "/api/business/hr/employee/$EMP_ID/contract" "{\"contract_no\":\"HT-HR46-$TAG\",\"contract_type\":\"FIXED_TERM\",\"sign_date\":\"$(date +%Y)-01-01\",\"start_date\":\"$(date +%Y)-01-01\",\"end_date\":\"$(( $(date +%Y) + 3 ))-12-31\"}"
expect_eq "1. 签劳动合同 HTTP" 200 "$CODE"
expect_eq "1. 合同生效中（§4.9 A4 要求在职员工有覆盖今天的合同）" "ACTIVE" "$(dat status)"

req POST "/api/business/hr/employee/$EMP_ID/salary" "{\"effective_from\":\"$(date +%Y)-01-01\",\"items\":[{\"item_code\":\"BASE\",\"amount\":$BASE_AMT},{\"item_code\":\"POST\",\"amount\":$POST_AMT},{\"item_code\":\"MEAL\",\"amount\":$MEAL_AMT}]}"
expect_eq "1. 配薪资标准 HTTP" 200 "$CODE"
expect_eq "1. 标准写入 3 项" 3 "$(jq -r '.data.written | length' "$OUT" 2>/dev/null)"
expect_eq "1. 现行标准可读回" 3 "$(jq -r '.data.current | length' "$OUT" 2>/dev/null)"
expect_eq "1. 员工状态在职（PROBATION/ACTIVE）" "t" \
  "$(q "SELECT status IN ('PROBATION','ACTIVE') FROM biz_employee WHERE id = '$EMP_ID'::uuid")"
step_end

# ====================================================== STEP 2 建考勤期间 ====
step 2 "建考勤期间 $ATT_PERIOD"
req POST /api/business/hr/attendance/period "{\"period_code\":\"$ATT_PERIOD\",\"period_start\":\"$ATT_START\",\"period_end\":\"$ATT_END\",\"remark\":\"HR-46 E2E\"}"
expect_eq "2. 建期间 HTTP" 200 "$CODE"
PERIOD_ID=$(dat id)
expect_ne "2. 期间 id" "" "$PERIOD_ID"
expect_eq "2. 新期间状态 DRAFT" "DRAFT" "$(dat status)"
expect_eq "2. 区间=自然月" "$ATT_START~$ATT_END" "$(dat period_start)~$(dat period_end)"
# 已锁定的期间不许导入 —— 这一步在锁之前，先证明「现在能导」
expect_eq "2. 期间未锁定（locked_at 为空）" "" "$(q "SELECT locked_at FROM biz_attendance_period WHERE id = '$PERIOD_ID'::uuid")"
step_end

# ========================================================== STEP 3 导入考勤 ====
step 3 "导入考勤（真 xlsx → impexp 预检 → 确认入库 → 期间汇总重算）"
ATT_JSON=$RUN_DIR/attendance.json
ATT_XLSX=$RUN_DIR/attendance.xlsx
cat >"$ATT_JSON" <<EOF
[["考勤月","工号","姓名","应出勤天数","实际出勤天数","带薪假天数","无薪假天数","病假天数","旷工天数","平时加班小时","休息日加班小时","法定节假日加班小时","迟到次数","早退次数","备注"],
 ["$ATT_PERIOD","$EMP_NO","$EMP_NAME",21,21,0,0,0,0,$OT_WD_HOURS,0,0,0,0,"HR-46 E2E"]]
EOF
python3 "$XLSX_PY" "$ATT_XLSX" "$ATT_JSON" >/dev/null || die "生成考勤 xlsx 失败"
LAST_METHOD=POST
LAST_PATH="/api/business/impexp/import/upload?biz_type=hr.attendance&mode=STRICT&duplicate=UPSERT"
CODE=$(curl -s -m 120 -o "$OUT" -w '%{http_code}' -X POST \
  "$BASE$LAST_PATH" -H "Authorization: Bearer $TOKEN" -F "file=@$ATT_XLSX;type=application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
expect_eq "3. 上传预检 HTTP" 200 "$CODE"
TASK_ID=$(dat task_id)
expect_ne "3. 预检任务 id" "" "$TASK_ID"
expect_eq "3. 预检 total=1" 1 "$(jq -r '.data.report.total' "$OUT" 2>/dev/null)"
expect_eq "3. 预检 error=0" 0 "$(jq -r '.data.report.error' "$OUT" 2>/dev/null)"

req POST "/api/business/impexp/import/$TASK_ID/confirm"
expect_eq "3. 确认入库 HTTP" 200 "$CODE"
expect_eq "3. 入库行数=1" 1 "$(dat imported_rows)"
expect_eq "3. 任务终态" "SUCCESS" "$(dat status)"
expect_eq "3. 台账明细 1 行" 1 "$(q "SELECT count(*) FROM biz_attendance_record WHERE period_id = '$PERIOD_ID'::uuid AND employee_id = '$EMP_ID'::uuid")"
expect_eq "3. 台账出勤天数" "21.00" "$(q "SELECT to_char(actual_days,'FM999990.00') FROM biz_attendance_record WHERE period_id = '$PERIOD_ID'::uuid")"
expect_eq "3. 台账加班小时（10）" "10.00" "$(q "SELECT to_char(ot_hours_workday,'FM999990.00') FROM biz_attendance_record WHERE period_id = '$PERIOD_ID'::uuid")"
# 导入后处理必须**在不调 recalc 接口**的情况下把期间头重算好（HR-22 的验收口径）
expect_eq "3. 期间头人数被重算" "1" "$(q "SELECT employee_count FROM biz_attendance_period WHERE id = '$PERIOD_ID'::uuid")"
expect_eq "3. 期间头应出勤合计" "21.00" "$(q "SELECT to_char(should_days_total,'FM999990.00') FROM biz_attendance_period WHERE id = '$PERIOD_ID'::uuid")"
step_end

# ============================================================== STEP 4 请假 ====
step 4 "请假（建单 → 提交 → 直属上级审批）"
req POST /api/business/hr/leave "{\"employee_id\":\"$EMP_ID\",\"leave_type\":\"ANNUAL\",\"start_time\":\"$LEAVE_DAY 09:00\",\"end_time\":\"$LEAVE_DAY 18:00\",\"duration_days\":1,\"paid\":1,\"reason\":\"HR-46 E2E 年假一天\"}"
expect_eq "4. 建请假单 HTTP" 200 "$CODE"
LEAVE_ID=$(dat id)
expect_eq "4. 新单状态 DRAFT" "DRAFT" "$(dat status)"

req POST "/api/business/hr/leave/$LEAVE_ID/submit" '{"remark":"HR-46 E2E 提交"}'
expect_eq "4. 提交审批 HTTP" 200 "$CODE"
expect_eq "4. 单据进入 SUBMITTED" "SUBMITTED" "$(dat status)"
LEAVE_INST=$(q "SELECT approval_instance_id FROM biz_leave_request WHERE id = '$LEAVE_ID'::uuid")
expect_ne "4. 已发起审批实例" "" "$LEAVE_INST"
if approve_pending "$LEAVE_INST"; then
  pass_line "4. 直属上级签完（流程引擎派单）"
else
  fail_line "4. 审批没有签完"; locate
fi
expect_eq "4. 单据 APPROVED" "APPROVED" "$(q "SELECT status FROM biz_leave_request WHERE id = '$LEAVE_ID'::uuid")"
expect_eq "4. 实例终态 APPROVED（§4.9 A10）" "APPROVED" "$(q "SELECT status FROM biz_flow_instance WHERE id = '$LEAVE_INST'::uuid")"
step_end

# ==================================================== STEP 5 锁定考勤期间 ====
step 5 "锁定考勤期间（过账前置闸：未锁定不许发钱）"
req POST "/api/business/hr/attendance/period/$PERIOD_ID/status" '{"to_status":"SUBMITTED"}'
expect_eq "5. 提交期间 HTTP" 200 "$CODE"
expect_eq "5. 期间 SUBMITTED" "SUBMITTED" "$(dat status)"
req POST "/api/business/hr/attendance/period/$PERIOD_ID/status" '{"to_status":"LOCKED"}'
expect_eq "5. 锁定期间 HTTP" 200 "$CODE"
expect_eq "5. 期间 LOCKED" "LOCKED" "$(dat status)"
expect_eq "5. 库内 locked_at 已写" "1" \
  "$(q "SELECT count(*) FROM biz_attendance_period WHERE id = '$PERIOD_ID'::uuid AND locked_at IS NOT NULL")"
step_end

# ============================================================== STEP 6 试算 ====
step 6 "试算（dry_run：算全公司，库里零行，不耗号段）"
CALC_BODY=$(cat <<EOF
{"period_code":"$SALARY_PERIOD","attendance_period_id":"$PERIOD_ID","manual":[
 {"employee_id":"$EMP_ID","item_code":"SI_PERSONAL","amount":$SI_PERSONAL},
 {"employee_id":"$EMP_ID","item_code":"HF_PERSONAL","amount":$HF_PERSONAL},
 {"employee_id":"$EMP_ID","item_code":"IIT","amount":$IIT},
 {"employee_id":"$EMP_ID","item_code":"SI_COMPANY","amount":$SI_COMPANY},
 {"employee_id":"$EMP_ID","item_code":"HF_COMPANY","amount":$HF_COMPANY}]}
EOF
)
req POST /api/business/hr/salary/calc "$CALC_BODY"
expect_eq "6. 试算 HTTP" 200 "$CODE"
expect_eq "6. dry_run=true" "true" "$(dat dry_run)"
expect_eq "6. 不返回单头（没落库）" "" "$(dat sheet)"
expect_eq "6. 参与人数=1" 1 "$(dat totals.employee_count)"
expect_numeric "6. 应发合计（$BASE_AMT+$POST_AMT+$MEAL_AMT+加班+全勤）" "$EXPECT_GROSS" "$(dat totals.gross_total)"
expect_numeric "6. 实发合计" "$EXPECT_NET" "$(dat totals.net_total)"
expect_numeric "6. 计提合计（应发+单位社保+单位公积金）" "$EXPECT_ACCRUAL" "$(dat totals.accrual_total)"
expect_numeric "6. 公式项 OT_WD（ROUND(BASE/21.75/8*10*1.5,2)）" "$EXPECT_OT_WD" \
  "$(jq -r --arg e "$EMP_ID" '.data.employees[] | select(.employee_id == $e) | .items[] | select(.item_code == "OT_WD") | .amount' "$OUT" 2>/dev/null)"
expect_numeric "6. 公式项 ATT_BONUS（全勤 200）" "$EXPECT_ATT_BONUS" \
  "$(jq -r --arg e "$EMP_ID" '.data.employees[] | select(.employee_id == $e) | .items[] | select(.item_code == "ATT_BONUS") | .amount' "$OUT" 2>/dev/null)"
expect_eq "6. 试算没有落库（0 张薪资单）" "0" "$(q "SELECT count(*) FROM biz_salary_sheet WHERE deleted_at IS NULL")"
step_end

# ============================================================== STEP 7 建单 ====
step 7 "建单（落库：CALCULATED + 明细 + 明细项）"
req POST /api/business/hr/salary "$CALC_BODY"
expect_eq "7. 建单 HTTP" 200 "$CODE"
SHEET_ID=$(jq -r '.data.sheet.id // empty' "$OUT" 2>/dev/null)
SHEET_NO=$(jq -r '.data.sheet.sheet_no // empty' "$OUT" 2>/dev/null)
expect_ne "7. 单头 id" "" "$SHEET_ID"
expect_eq "7. 新单状态 CALCULATED" "CALCULATED" "$(jq -r '.data.sheet.status // empty' "$OUT" 2>/dev/null)"
case "$SHEET_NO" in
  SAL-*) pass_line "7. 单号号段 SAL-（$SHEET_NO）" ;;
  *) fail_line "7. 单号号段不对：$SHEET_NO"; locate ;;
esac
expect_eq "7. 库里 1 张单" 1 "$(q "SELECT count(*) FROM biz_salary_sheet WHERE id = '$SHEET_ID'::uuid AND deleted_at IS NULL")"
expect_eq "7. 明细 1 行" 1 "$(q "SELECT count(*) FROM biz_salary_detail WHERE sheet_id = '$SHEET_ID'::uuid")"
expect_eq "7. 明细项 = 15 项（17 项种子去掉 2 个汇总项）" 15 \
  "$(q "SELECT count(*) FROM biz_salary_detail_item i JOIN biz_salary_detail d ON d.id = i.detail_id WHERE d.sheet_id = '$SHEET_ID'::uuid")"
expect_numeric "7. 单头应发 = 试算应发" "$EXPECT_GROSS" "$(q "SELECT to_char(gross_total,'FM9999990.00') FROM biz_salary_sheet WHERE id = '$SHEET_ID'::uuid")"
expect_numeric "7. 单头实发 = 试算实发" "$EXPECT_NET" "$(q "SELECT to_char(net_total,'FM9999990.00') FROM biz_salary_sheet WHERE id = '$SHEET_ID'::uuid")"
step_end

# ============================================================== STEP 8 审批 ====
step 8 "审批（提交 → 人事经理 → 财务会签；§4.3：审批通过不出凭证）"
req POST "/api/business/hr/salary/$SHEET_ID/submit"
expect_eq "8. 提交审批 HTTP" 200 "$CODE"
SHEET_INST=$(q "SELECT approval_instance_id FROM biz_salary_sheet WHERE id = '$SHEET_ID'::uuid")
expect_ne "8. 已发起审批实例" "" "$SHEET_INST"
expect_eq "8. 提交后 SUBMITTED" "SUBMITTED" "$(q "SELECT status FROM biz_salary_sheet WHERE id = '$SHEET_ID'::uuid")"
if approve_pending "$SHEET_INST"; then
  pass_line "8. 流程节点全部签完（人事经理 + 财务会签）"
else
  fail_line "8. 审批没有签完"; locate
fi
expect_eq "8. 单据 APPROVED" "APPROVED" "$(q "SELECT status FROM biz_salary_sheet WHERE id = '$SHEET_ID'::uuid")"
expect_eq "8. 实例终态 APPROVED" "APPROVED" "$(q "SELECT status FROM biz_flow_instance WHERE id = '$SHEET_INST'::uuid")"
expect_eq "8. 审批通过**不出凭证**（§4.3 硬口径）" "0" \
  "$(q "SELECT count(*) FROM biz_voucher WHERE source_no = '$SHEET_NO' AND deleted_at IS NULL")"
expect_eq "8. 审批通过没有占用号段" "0" "$(q "SELECT count(*) FROM biz_voucher WHERE period = '$SALARY_PERIOD' AND deleted_at IS NULL")"
step_end

# ====================================================== STEP 9 过账 + 红冲 ====
step 9 "过账（两张凭证）→ 红冲（两张红字）→ 二次红冲被拒"
req POST "/api/business/hr/salary/$SHEET_ID/post"
expect_eq "9. 过账 HTTP" 200 "$CODE"
expect_eq "9. 单据 POSTED" "POSTED" "$(dat status)"
ACC_NO=$(dat accrual_voucher_no)
PAY_NO=$(dat payment_voucher_no)
expect_ne "9. 计提凭证号" "" "$ACC_NO"
expect_ne "9. 发放凭证号" "" "$PAY_NO"
expect_eq "9. 两张蓝字凭证" 2 \
  "$(q "SELECT count(*) FROM biz_voucher WHERE source_no = '$SHEET_NO' AND is_red = 0 AND deleted_at IS NULL")"
expect_eq "9. 计提凭证 source_type" "PAYROLL_ACCRUAL" \
  "$(q "SELECT source_type FROM biz_voucher WHERE voucher_no = '$ACC_NO' AND deleted_at IS NULL")"
expect_eq "9. 发放凭证 source_type" "PAYROLL_PAYMENT" \
  "$(q "SELECT source_type FROM biz_voucher WHERE voucher_no = '$PAY_NO' AND deleted_at IS NULL")"
expect_eq "9. 两张都借贷平衡（库 CHECK 之外再算一遍）" "0" \
  "$(q "SELECT count(*) FROM biz_voucher WHERE source_no = '$SHEET_NO' AND total_debit <> total_credit")"
expect_numeric "9. 计提凭证借方 5602.10 = 应发（净额法）" "$EXPECT_GROSS" \
  "$(q "SELECT to_char(SUM(e.debit_amount),'FM9999990.00') FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id WHERE v.voucher_no = '$ACC_NO' AND e.account_code = '5602.10'")"
expect_numeric "9. 发放凭证贷 1002 = 实发" "$EXPECT_NET" \
  "$(q "SELECT to_char(SUM(e.credit_amount),'FM9999990.00') FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id WHERE v.voucher_no = '$PAY_NO' AND e.account_code = '1002'")"
expect_eq "9. 借方带部门辅助核算（5602.10 的 aux_dept_id 非空）" "0" \
  "$(q "SELECT count(*) FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id WHERE v.voucher_no = '$ACC_NO' AND e.account_code = '5602.10' AND e.aux_dept_id IS NULL")"

req POST "/api/business/hr/salary/$SHEET_ID/reverse" '{"reason":"HR-46 E2E：金额算错，整单红冲重发"}'
expect_eq "9. 红冲 HTTP" 200 "$CODE"
expect_eq "9. 单据 REVERSED" "REVERSED" "$(dat status)"
expect_eq "9. 共 4 张凭证（2 蓝 + 2 红）" 4 \
  "$(q "SELECT count(*) FROM biz_voucher WHERE source_no = '$SHEET_NO' AND deleted_at IS NULL")"
expect_eq "9. 两张红字" 2 \
  "$(q "SELECT count(*) FROM biz_voucher WHERE source_no = '$SHEET_NO' AND is_red = 1 AND reverse_of IS NOT NULL AND deleted_at IS NULL")"
expect_eq "9. 原凭证都被回写 reversed_by" 2 \
  "$(q "SELECT count(*) FROM biz_voucher WHERE source_no = '$SHEET_NO' AND is_red = 0 AND reversed_by IS NOT NULL AND deleted_at IS NULL")"
expect_eq "9. 红字借贷互换（红借 = 原贷，逐科目）" "0" \
  "$(q "SELECT count(*) FROM (SELECT b.account_code, SUM(b.debit_amount) AS bd, SUM(b.credit_amount) AS bc,
             SUM(r.debit_amount) AS rd, SUM(r.credit_amount) AS rc
        FROM biz_voucher_entry b
        JOIN biz_voucher vb ON vb.id = b.voucher_id AND vb.is_red = 0
        JOIN biz_voucher vr ON vr.reverse_of = vb.id
        JOIN biz_voucher_entry r ON r.voucher_id = vr.id AND r.account_code = b.account_code
       WHERE vb.source_no = '$SHEET_NO'
       GROUP BY b.account_code) z
      WHERE z.bd <> z.rc OR z.bc <> z.rd")"
expect_eq "9. 红冲原因落库" "1" \
  "$(q "SELECT count(*) FROM biz_salary_sheet WHERE id = '$SHEET_ID'::uuid AND reverse_reason LIKE 'HR-46 E2E%'")"
req POST "/api/business/hr/salary/$SHEET_ID/reverse" '{"reason":"HR-46 E2E：再冲一次应该被拒"}'
expect_eq "9. 二次红冲被状态机拒绝 → 409" 409 "$CODE"
expect_eq "9. 二次红冲没有多出凭证" 4 \
  "$(q "SELECT count(*) FROM biz_voucher WHERE source_no = '$SHEET_NO' AND deleted_at IS NULL")"
step_end

# ============================================================== STEP 10 对账 ====
step 10 "对账（凭证逐科目净额 0 + §4.9 恒等式 + 10 条巡检断言）"
# ① 四张凭证逐科目净额必须为 0（红冲之后整条工资链在账上不留痕）
expect_eq "10. 工资链 4 张凭证逐科目净额全 0" "0" \
  "$(q "SELECT count(*) FROM (SELECT e.account_code, SUM(e.debit_amount) - SUM(e.credit_amount) AS net
        FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
       WHERE v.source_no = '$SHEET_NO' AND v.deleted_at IS NULL
       GROUP BY e.account_code HAVING SUM(e.debit_amount) - SUM(e.credit_amount) <> 0) z")"
# ② 涉及科目清单（对不上时能一眼看出漏了哪个科目）
ACCOUNTS=$(q "SELECT string_agg(DISTINCT e.account_code, ',' ORDER BY e.account_code)
  FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
 WHERE v.source_no = '$SHEET_NO' AND v.deleted_at IS NULL")
expect_eq "10. 用到 10 个工资科目（5602.10/11/12 + 2211.01/02/03 + 2241.03/04 + 2221.03 + 1002）" \
  "1002,2211.01,2211.02,2211.03,2221.03,2241.03,2241.04,5602.10,5602.11,5602.12" "$ACCOUNTS"
# ③ §4.9 A6：单头 = Σ 明细
expect_eq "10. A6 单头应发 = Σ 明细应发" "0" \
  "$(q "SELECT count(*) FROM biz_salary_sheet s WHERE s.id = '$SHEET_ID'::uuid
         AND (s.gross_total <> (SELECT COALESCE(SUM(d.gross_amount),0) FROM biz_salary_detail d WHERE d.sheet_id = s.id)
           OR s.net_total   <> (SELECT COALESCE(SUM(d.net_amount),0)   FROM biz_salary_detail d WHERE d.sheet_id = s.id))")"
# ④ §4.9 A7：明细 = Σ 明细项
expect_eq "10. A7 应发 = Σ收入−Σ应发前扣减；代扣 = ΣWITHHOLD" "0" \
  "$(q "SELECT count(*) FROM biz_salary_detail d WHERE d.sheet_id = '$SHEET_ID'::uuid
         AND (d.gross_amount <> (SELECT COALESCE(SUM(CASE WHEN i.item_type='EARNING' THEN i.amount ELSE 0 END),0)
                                      - COALESCE(SUM(CASE WHEN i.item_type='PRE_DEDUCT' THEN i.amount ELSE 0 END),0)
                                   FROM biz_salary_detail_item i WHERE i.detail_id = d.id)
           OR d.withhold_amount <> (SELECT COALESCE(SUM(i.amount),0) FROM biz_salary_detail_item i
                                     WHERE i.detail_id = d.id AND i.item_type='WITHHOLD'))")"
# ⑤ §4.9 A8：POSTED/REVERSED 必有两张凭证且平衡（本单已 REVERSED，等价断言在上面已覆盖，这里查全库）
expect_eq "10. A8 全库无「有单无凭证」的薪资单" "0" \
  "$(q "SELECT count(*) FROM biz_salary_sheet s WHERE s.status IN ('POSTED','REVERSED') AND s.deleted_at IS NULL
         AND (s.accrual_voucher_id IS NULL OR s.payment_voucher_id IS NULL)")"
# ⑥ 状态轨迹（提交 / 通过 / 过账 / 红冲都在 biz_status_log 里，审计可回溯）
expect_eq "10. 状态日志含 SUBMITTED→APPROVED→POSTED→REVERSED 轨迹" "t" \
  "$(q "SELECT count(DISTINCT to_status) >= 4 FROM biz_status_log WHERE entity_type = 'SALARY_SHEET' AND entity_id = '$SHEET_ID'::uuid")"
# ⑦ 10 条巡检断言（HR-45 的脚本，只读）：本库跑完必须 BLOCKER 全 0
INVARIANT_OUT=$RUN_DIR/hr_invariants.out
if [ -x "$HERE/../hr_invariants.sh" ] || [ -f "$HERE/../hr_invariants.sh" ]; then
  HR_DB_URL="postgres://$DBUSER:$PASSW@$DBHOST:$DBPORT/$DB" \
    bash "$HERE/../hr_invariants.sh" >"$INVARIANT_OUT" 2>&1
  inv_rc=$?
  expect_eq "10. hr_invariants.sh 退出码（BLOCKER 全 0 才 0）" 0 "$inv_rc"
  expect_eq "10. 巡检 BLOCKER 失败数" 0 \
    "$(sed -n 's/.*blocker_fail=\([0-9]*\).*/\1/p' "$INVARIANT_OUT" | tail -1)"
  expect_eq "10. 巡检覆盖 12 行（A1~A10 + W1/W2）" 12 \
    "$(sed -n 's/.*total=\([0-9]*\).*/\1/p' "$INVARIANT_OUT" | tail -1)"
else
  fail_line "10. 找不到 scripts/verify/hr_invariants.sh（HR-45 的巡检脚本）"; locate
fi
step_end

# ================================================================ 汇总 =======
hr
say "== 断言汇总：通过 $PASS 项 / 失败 $FAIL 项 =="
say "== 步骤明细 =="
for row in "${STEP_FAILED_LIST[@]}"; do
  IFS='|' read -r no name cnt <<<"$row"
  if [ "$cnt" = "0" ]; then printf '  ✅ %2s. %s\n' "$no" "$name"; else printf '  ❌ %2s. %s（%s 条失败）\n' "$no" "$name" "$cnt"; fi
done
say "== 产物 =="
say "   服务端日志：$SERVER_LOG"
say "   巡检输出：  $RUN_DIR/hr_invariants.out"
say "   导入文件：  $RUN_DIR/attendance.xlsx"
say "   演练库：    $DBUSER@$DBHOST:$DBPORT/$DB（$([ "$KEEP_DB" = "1" ] && echo '已保留' || echo '脚本退出时 DROP')）"
say "   夹具：员工=$EMP_NO($EMP_ID) 期间=$PERIOD_ID 请假=$LEAVE_ID 薪资单=$SHEET_NO($SHEET_ID)"

if [ "$FAIL" -eq 0 ]; then
  say "== 结果：10 步全绿（$PASS 项断言） =="
  exit 0
fi
say "== 结果：$FAIL 项断言失败 =="
exit 1
