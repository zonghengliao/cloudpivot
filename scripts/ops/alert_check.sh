#!/usr/bin/env bash
# 云枢运维告警巡检（NEAR-FIX-3 · 告警接入）—— cron 每 5 分钟一次
#
# =============================== 它解决什么 ===============================
# 评审 5.8：「无指标、无告警规则、无外部告警通道」。指标由后端每分钟采样进 /metrics
# （见 backend/src/task/metrics_sampler.rs），**本脚本是告警规则 + 告警通道**：
#
#   ① health        服务挂了            GET /health 非 200 或 data.status != ok
#   ② deps          依赖挂了            GET /health/deps 任一依赖 ok=false
#   ③ invariant     账已错 / 巡检没跑   最近一次 sys_invariant_result 非 OK、有 BLOCKER、或过期
#                     └ 但若**结论早于最后一次迁移**（修账的迁移落在结论之后），不判「账已错」，
#                       而是报「结论过期、待重跑」：旧结论对它之后的迁移一无所知（NEAR-FIX-4），
#                       这类误报一直是每 5 分钟扣一次「账已错」，直到次日的定时任务才可能自愈。
#   ④ disk          磁盘将满            df 使用率 ≥ 阈值（默认 85%，75% 先 WARN）
#   ⑤ secret_expiry 证书/密钥将过期     TLS 证书剩余天数 < 阈值（没有证书则显式跳过）
#   ⑥ backlog       导入导出积压        未终态任务最早一张已等 > 30 分钟，或总数 ≥ 10
#
# ①②③ 中的 ①③⑥ 就是评审指定的三条核心告警（服务挂 / 巡检 FAIL / 导入导出积压）。
#
# =============================== 告警通道 ===============================
# · ALERT.log（永远写，兜底：kanban 与飞书都挂了还能看）
#   → /var/log/cloudpivot/ALERT.log
# · kanban 建卡（assignee=laoliu，本体看板在 172.18.10.99 的 dev 板）
#   → 本机就是看板机则本地 `hermes kanban create`，否则 ssh 到看板机建卡
#     （144/145 各自的 /root/.hermes/kanban.db 是 10-29 的**死板**，空无一卡；
#      往那里建卡等于没人看得见，所以绝不用本地 CLI）
# · 飞书卡片（有 FEISHU_APP_ID/SECRET 才发；144/145 当前没有该 env，自动跳过）
#
# =============================== 去重 ===============================
# 同一检查项 5 分钟内只发一次（默认 300s，可调）：状态目录里每项一个 `<key>.last`
# 记 epoch 秒。恢复时打一条 RECOVERED 并删掉状态文件（下次再犯立刻报）。
#
# =============================== 用法 ===============================
#   bash scripts/ops/alert_check.sh                # 正常巡检（cron 用这个）
#   bash scripts/ops/alert_check.sh --dry-run      # 只判定不动 sinks（不改状态文件）
#   bash scripts/ops/alert_check.sh --self-test    # 判定逻辑自检（不需要服务/库/网络）
#   bash scripts/ops/alert_check.sh --only health,disk   # 只跑指定检查
#   bash scripts/ops/alert_check.sh --force        # 忽略去重窗口（演练用）
#   bash scripts/ops/alert_check.sh --list-checks  # 列出检查项与当前生效参数
#
# 环境变量（全部有默认值，cron 里可覆盖）：
#   NIK_ALERT_HOST_TAG        告警里的主机标识（默认：本机第一个非回环 IP）
#   NIK_ALERT_BASE_URL        服务地址（默认 http://127.0.0.1:8023）
#   NIK_ALERT_ENV_FILE        库口令 env 文件（默认按 144/145/开发机 三种布局自动探测）
#   NIK_ALERT_LOG             ALERT.log 路径（默认 /var/log/cloudpivot/ALERT.log）
#   NIK_ALERT_STATE_DIR       去重状态目录（默认 /var/log/cloudpivot/alert-state）
#   NIK_ALERT_DEDUP_SECONDS   去重窗口（默认 300）
#   NIK_ALERT_DISK_THRESHOLD  磁盘告警阈值百分比（默认 85）；NIK_ALERT_DISK_WARN 默认 75
#   NIK_ALERT_DISK_PATH       被检查的文件系统（默认 /）
#   NIK_ALERT_SECRET_WARN_DAYS 证书剩余天数阈值（默认 30）
#   NIK_ALERT_BACKLOG_MINUTES 积压判定的「等了多久」（默认 30）
#   NIK_ALERT_BACKLOG_COUNT   积压判定的「多少张」（默认 10）
#   NIK_ALERT_INVARIANT_MAX_AGE_HOURS 巡检过期阈值（默认 36）
#   NIK_ALERT_KANBAN          auto | local | ssh | off（默认 auto）
#   NIK_ALERT_BOARD_SSH       看板机 ssh 目标（默认 root@172.18.10.99）
#   NIK_ALERT_BOARD_IP        看板机 IP（默认 172.18.10.99，用于判断「本机是否看板机」）
#   NIK_ALERT_KANBAN_ASSIGNEE 建卡受理人（默认 laoliu）
#   NIK_ALERT_FEISHU          auto | off（默认 auto：有发送器且有凭据才发）
#   NIK_ALERT_CERT_FILES      额外的证书文件（冒号分隔，可选）
#   NIK_ALERT_CERT_GLOBS      证书 glob（空格分隔；默认 nginx / letsencrypt 两处）
#
# 退出码：0 = 巡检跑完（无论有无告警；cron 不该因为报警就报错）
#         2 = 用法错误   3 = 自检失败
set -uo pipefail

PROG="$(basename "$0")"
NOW_EPOCH() { date +%s; }
STAMP() { date '+%Y-%m-%dT%H:%M:%S%:z'; }

HOST_TAG="${NIK_ALERT_HOST_TAG:-$(hostname -I 2>/dev/null | awk '{print $1}')}"
HOST_TAG="${HOST_TAG:-$(hostname)}"
BASE_URL="${NIK_ALERT_BASE_URL:-http://127.0.0.1:8023}"
ENV_FILE="${NIK_ALERT_ENV_FILE:-}"
ALERT_LOG="${NIK_ALERT_LOG:-/var/log/cloudpivot/ALERT.log}"
STATE_DIR="${NIK_ALERT_STATE_DIR:-/var/log/cloudpivot/alert-state}"
DEDUP_SECONDS="${NIK_ALERT_DEDUP_SECONDS:-300}"
DISK_THRESHOLD="${NIK_ALERT_DISK_THRESHOLD:-85}"
DISK_WARN="${NIK_ALERT_DISK_WARN:-75}"
DISK_PATH="${NIK_ALERT_DISK_PATH:-/}"
SECRET_WARN_DAYS="${NIK_ALERT_SECRET_WARN_DAYS:-30}"
BACKLOG_MINUTES="${NIK_ALERT_BACKLOG_MINUTES:-30}"
BACKLOG_COUNT="${NIK_ALERT_BACKLOG_COUNT:-10}"
INVARIANT_MAX_AGE_HOURS="${NIK_ALERT_INVARIANT_MAX_AGE_HOURS:-36}"
KANBAN_MODE="${NIK_ALERT_KANBAN:-auto}"
BOARD_SSH="${NIK_ALERT_BOARD_SSH:-root@172.18.10.99}"
BOARD_IP="${NIK_ALERT_BOARD_IP:-172.18.10.99}"
KANBAN_ASSIGNEE="${NIK_ALERT_KANBAN_ASSIGNEE:-laoliu}"
FEISHU_MODE="${NIK_ALERT_FEISHU:-auto}"
CERT_FILES="${NIK_ALERT_CERT_FILES:-}"
CERT_GLOBS="${NIK_ALERT_CERT_GLOBS:-/etc/nginx/ssl/*.pem /etc/nginx/ssl/*.crt /etc/letsencrypt/live/*/fullchain.pem}"

DRY_RUN=0
FORCE=0
ONLY=""
CHECKS_ALL="health deps invariant disk secret_expiry backlog"

log()  { printf '[%s] %s\n' "$(STAMP)" "$*"; }
warn() { printf '[%s] WARN  %s\n' "$(STAMP)" "$*" >&2; }
err()  { printf '[%s] ERROR %s\n' "$(STAMP)" "$*" >&2; }
die()  { err "$*"; exit 2; }

usage() {
  sed -n '2,70p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run)     DRY_RUN=1; shift ;;
    --force)       FORCE=1; shift ;;
    --only)        ONLY="${2:?--only 需要参数}"; shift 2 ;;
    --only=*)      ONLY="${1#*=}"; shift ;;
    --self-test)   SELF_TEST=1; shift ;;
    --list-checks) LIST_ONLY=1; shift ;;
    -h|--help)     usage 0 ;;
    *)             die "未知参数：$1（--help 看用法）" ;;
  esac
done

# ══════════════════════════ 判定函数（纯逻辑，可自检）══════════════════════════
# 约定：每个 decide_* 把结果打成一行 `<LEVEL>|<标题>|<详情>`；
#       LEVEL ∈ OK / INFO / WARN / ALERT。OK = 该项正常，其余进通道（INFO 只写日志）。
D() { printf '%s|%s|%s\n' "$1" "$2" "$3"; }

# ① 服务健康。/health 契约：{"code":0,"msg":"ok","data":{"status":"ok",...}}
decide_health() {
  local code="$1" body="$2" status
  if [ "$code" != "200" ]; then
    D ALERT "服务不可用（/health 返回 $code）" "GET ${BASE_URL}/health → HTTP ${code}；进程可能已退出或端口未监听"
    return
  fi
  status="$(printf '%s' "$body" | python3 -c 'import json,sys
try:
    d=json.load(sys.stdin)
except Exception:
    print("PARSE_ERROR"); raise SystemExit
print((d.get("data") or {}).get("status") or d.get("status") or "MISSING")' 2>/dev/null)"
  case "$status" in
    ok)     D OK "服务健康" "/health 200 status=ok" ;;
    PARSE_ERROR) D ALERT "服务不可用（/health 返回体无法解析）" "HTTP 200 但响应不是 JSON：$(printf '%s' "$body" | head -c 200)" ;;
    *)      D ALERT "服务不健康（/health status=${status:-未知}）" "HTTP 200 但 data.status=${status:-未知}：$(printf '%s' "$body" | head -c 200)" ;;
  esac
}

# ② 依赖健康。/health/deps 契约：{"status":"ok","db":{"ok":true,...},"cache":{"ok":true,...}}
decide_deps() {
  local code="$1" body="$2" verdict
  if [ "$code" != "200" ]; then
    D ALERT "依赖探测不可用（/health/deps 返回 $code）" "GET ${BASE_URL}/health/deps → HTTP ${code}"
    return
  fi
  verdict="$(printf '%s' "$body" | python3 -c 'import json,sys
try:
    d=json.load(sys.stdin)
except Exception:
    print("ALERT|依赖探测返回体无法解析|HTTP 200 但不是 JSON"); raise SystemExit
bad=[]
for name in ("db","cache"):
    dep=d.get(name) or {}
    if dep and dep.get("ok") is False:
        bad.append(name+" 挂了（"+str(dep.get("error") or dep.get("latency_ms") or "")+"）")
if (d.get("status") or "ok") != "ok" and not bad:
    bad.append("status="+str(d.get("status")))
print(("ALERT|依赖不可用（"+", ".join(bad)+"）|"+json.dumps(d,ensure_ascii=False)[:300]) if bad else "OK|依赖全通|db/cache 均 ok")' 2>/dev/null)"
  [ -n "$verdict" ] || verdict="ALERT|依赖探测返回体无法解析|$(printf '%s' "$body" | head -c 200)"
  printf '%s\n' "$verdict"
}

# ③ 账务巡检。入参：<status>|<blockers>|<failed>|<age_seconds>[|<migration_newer_seconds>]
#    第 5 段 = max(schema_migrations.applied_at) − 最后一次巡检 run_at（秒）。
#      > 0 ⇒ 结论早于最后一次迁移（修账的迁移落在结论之后），此时**不能**据此断言「账已错」；
#      缺省 / 空 / 非数字 ⇒ 未知，按旧口径判定（不臆测）。
#    NEAR-FIX-4 的病根就是没有这一段：11-09 22:23 的 V0366 修掉了 H6/H5，告警却一直对着
#    11-09 10:20 的旧 FAIL 喊「账已错」，直到次日 10:20 的定时任务才可能自愈。
decide_invariant() {
  local row="$1" status blockers failed age newer limit
  limit=$(( INVARIANT_MAX_AGE_HOURS * 3600 ))
  if [ -z "$row" ]; then
    D INFO "巡检从未跑过（sys_invariant_result 无记录）" "没有结论不等于账是对的：先确认 invariant_audit 任务（每天 02:20）有没有被注册/执行"
    return
  fi
  IFS='|' read -r status blockers failed age newer <<<"$row"
  status="${status:-UNKNOWN}"; blockers="${blockers:-0}"; failed="${failed:-0}"; age="${age:-0}"
  # 非数字 / 负数一律当「未知」（负数可能是时钟回拨，不当成过期也不当成新鲜）
  case "${newer:-}" in ''|*[!0-9]*) newer=0 ;; esac
  if [ "$newer" -gt 0 ] 2>/dev/null; then
    # 结论早于最后一次迁移：先看「过期」这条，再说账对不对 —— 顺序不能反，
    # 否则修账之后的那半天里每 5 分钟都会把「账已错」再扣一次（本次误报）。
    if [ "$age" -gt "$limit" ] 2>/dev/null; then
      D ALERT "巡检结论早于最后一次迁移且已 $(( age / 3600 )) 小时未重跑（账务无人复核）" \
        "最后结论 status=${status} blockers=${blockers} failed=${failed}，已 ${age}s（阈值 ${INVARIANT_MAX_AGE_HOURS}h）；最后一次迁移比它晚 $(( newer / 60 )) 分钟 ⇒ 发布/迁移后没重跑巡检，先跑 scripts/ops/invariant_rerun.sh"
      return
    fi
    if [ "$blockers" -gt 0 ] 2>/dev/null || [ "$status" != "OK" ]; then
      D WARN "巡检结论早于最后一次迁移，待重跑（暂不判「账已错」）" \
        "最后结论 status=${status} blockers=${blockers} failed=${failed}（${age}s 前）；最后一次迁移比它晚 $(( newer / 60 )) 分钟 ⇒ 该结论可能已被那次迁移修掉，先重跑：bash scripts/ops/invariant_rerun.sh（发布/迁移后会自动跑）"
      return
    fi
    D INFO "巡检结论早于最后一次迁移（结论是 OK，但新迁移后的库没被复核过）" \
      "status=OK（${age}s 前）；最后一次迁移比它晚 $(( newer / 60 )) 分钟 ⇒ 迁移动过的数据还没被巡检看过，建议重跑 scripts/ops/invariant_rerun.sh"
    return
  fi
  if [ "$blockers" -gt 0 ] 2>/dev/null; then
    D ALERT "账务巡检有 ${blockers} 个 BLOCKER（账已错）" "最近一次巡检 status=${status} failed=${failed} blockers=${blockers}；明细见「账务巡检看板」或 scripts/verify/erp_invariants.py"
    return
  fi
  if [ "$status" != "OK" ]; then
    D ALERT "账务巡检未通过（status=${status}）" "失败项 ${failed} 个（BLOCKER=0）；detail 见「账务巡检看板」"
    return
  fi
  if [ "$age" -gt "$limit" ] 2>/dev/null; then
    D ALERT "账务巡检已 $(( age / 3600 )) 小时没跑（阈值 ${INVARIANT_MAX_AGE_HOURS}h）" "巡检任务（invariant_audit，每天 02:20）可能没执行；「没跑」不等于「账对」"
    return
  fi
  D OK "账务巡检通过" "status=OK blockers=0 age=${age}s"
}

# ④ 磁盘。<use_pct> 是 df 的 Use% 数字部分
decide_disk() {
  local pct="$1" path="$2"
  if ! [ "$pct" -ge 0 ] 2>/dev/null; then
    D INFO "磁盘使用率读不到" "df -P ${path} 输出无法解析"
    return
  fi
  if [ "$pct" -ge "$DISK_THRESHOLD" ]; then
    D ALERT "磁盘使用率 ${pct}%（阈值 ${DISK_THRESHOLD}%）" "${path} 已用 ${pct}%，写满会导致服务/日志/备份同时失败；先跑 scripts/maintenance 里的清理脚本"
    return
  fi
  if [ "$pct" -ge "$DISK_WARN" ]; then
    D WARN "磁盘使用率 ${pct}%（接近阈值 ${DISK_THRESHOLD}%）" "${path} 已用 ${pct}%，尽早清理（cargo target / 备份 / 日志）"
    return
  fi
  D OK "磁盘正常" "${path} 已用 ${pct}%"
}

# ⑤ 证书。<days> 为剩余天数（负数 = 已过期）
decide_cert() {
  local days="$1" path="$2"
  if ! [ "$days" -ge -100000 ] 2>/dev/null; then
    D INFO "证书有效期读不到" "${path} 解析失败"
    return
  fi
  if [ "$days" -lt 0 ]; then
    D ALERT "证书已过期 ${path}" "已过期 $(( -days )) 天"
    return
  fi
  if [ "$days" -lt "$SECRET_WARN_DAYS" ]; then
    D ALERT "证书 ${days} 天后过期" "${path} 剩余 ${days} 天（阈值 ${SECRET_WARN_DAYS} 天），需尽快续期"
    return
  fi
  D OK "证书有效" "${path} 剩余 ${days} 天"
}

# ⑥ 导入导出积压。入参：<积压总数>|<最早未终态秒数>；-1 = 未知
decide_backlog() {
  local pending="$1" stuck="$2" limit_seconds
  limit_seconds=$(( BACKLOG_MINUTES * 60 ))
  if [ "$pending" -lt 0 ] 2>/dev/null && [ "$stuck" -lt 0 ] 2>/dev/null; then
    D INFO "导入导出积压读不到（指标与库都不可用）" "既没有 cloudpivot_impexp_backlog_* 指标，也连不上库；这一项等于没人看"
    return
  fi
  if [ "$stuck" -ge "$limit_seconds" ] 2>/dev/null; then
    D ALERT "导入导出积压（最早一张已等 $(( stuck / 60 )) 分钟）" "未终态任务 ${pending} 张，最早一张已等 $(( stuck / 60 )) 分钟（阈值 ${BACKLOG_MINUTES} 分钟）；查导出 worker（impexp_export_worker 每 5 秒一轮）与导入确认流程"
    return
  fi
  if [ "$pending" -ge "$BACKLOG_COUNT" ] 2>/dev/null; then
    D ALERT "导入导出积压（未终态 ${pending} 张）" "超过阈值 ${BACKLOG_COUNT} 张，最早一张已等 $(( stuck / 60 )) 分钟"
    return
  fi
  D OK "导入导出无积压" "未终态 ${pending} 张，最早一张 ${stuck}s"
}

# ══════════════════════════ 采集层 ══════════════════════════
# HTTP 取：body → HTTP_BODY，状态码 → HTTP_CODE（连不上 = 000）
#
# 为什么不用命令替换回传：`x="$(http_fetch ...)"` 会让 http_fetch 在**子 shell** 里跑，
# 里面设的全局变量（body）出了子 shell 就没了 —— 于是判定函数永远拿到空体，
# 得出「返回体无法解析」这种假告警（第一次自测就是踩这个）。改成写临时文件。
HTTP_BODY=""
HTTP_CODE="000"
http_fetch() {
  local url="$1" tmp code
  tmp="$(mktemp)" || { HTTP_BODY=""; HTTP_CODE="000"; return 1; }
  code="$(curl -sS -m 10 -o "$tmp" -w '%{http_code}' "$url" 2>/dev/null)" || true
  HTTP_BODY="$(cat "$tmp" 2>/dev/null)"
  rm -f "$tmp"
  HTTP_CODE="${code:-000}"
  [ -n "$HTTP_CODE" ] || HTTP_CODE="000"
  return 0
}

# 载入库口令 env 文件（只挑 NIK_DATABASE_* 三五行，不 source 整个文件）
load_db_env() {
  local file="$1" line key value
  [ -r "$file" ] || return 1
  while IFS= read -r line; do
    case "$line" in
      ''|\#*) continue ;;
      NIK_DATABASE_*=*)
        key="${line%%=*}"
        value="${line#*=}"
        value="${value%\"}"; value="${value#\"}"
        value="${value%\'}"; value="${value#\'}"
        case "$key" in
          NIK_DATABASE_HOST|NIK_DATABASE_PORT|NIK_DATABASE_NAME|NIK_DATABASE_USER|NIK_DATABASE_PASSWORD|NIK_DATABASE_SSL_MODE)
            export "$key=$value" ;;
        esac
        ;;
    esac
  done <"$file"
  return 0
}

# 自动探测 env 文件：145 的 base 实例、144 生产、开发机三种布局
detect_env_file() {
  local candidate
  for candidate in /etc/cloudpivot-base/base.env /etc/cloudpivot/cloudpivot.env /etc/cloudpivot/cloudpivot-dev.env; do
    if [ -r "$candidate" ] && grep -q '^NIK_DATABASE_NAME=' "$candidate" 2>/dev/null; then
      printf '%s' "$candidate"
      return 0
    fi
  done
  return 1
}

# 一条只读标量查询（**不做就绪判断**，供探活与查询共用）；失败返回非零
psql_raw() {
  local sql="$1" out
  command -v psql >/dev/null 2>&1 || return 1
  [ -n "${NIK_DATABASE_NAME:-}" ] || return 1
  out="$(PGPASSWORD="${NIK_DATABASE_PASSWORD:-}" psql -X -q -A -t -v ON_ERROR_STOP=1 \
          -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}" \
          -U "${NIK_DATABASE_USER:-postgres}" -d "${NIK_DATABASE_NAME}" \
          -c "$sql" 2>/dev/null)" || return 1
  printf '%s' "$out"
}

# 就绪后才用的查询：没就绪直接失败（避免每次检查都刷一条连不上）
psql_scalar() {
  [ -n "${PSQL_READY:-}" ] || return 1
  psql_raw "$1"
}

init_db() {
  PSQL_READY=""
  if [ -z "$ENV_FILE" ]; then
    if ! ENV_FILE="$(detect_env_file)"; then
      warn "未找到库 env 文件（/etc/cloudpivot-base/base.env、/etc/cloudpivot/cloudpivot.env）；巡检结论/积压只能靠 /metrics"
      ENV_FILE=""
      return 0
    fi
  fi
  load_db_env "$ENV_FILE" || { warn "env 文件不可读：$ENV_FILE"; return 0; }
  command -v psql >/dev/null 2>&1 || { warn "本机没有 psql，巡检结论/积压只能靠 /metrics"; return 0; }
  if [ -n "$(psql_raw 'SELECT 1')" ]; then
    PSQL_READY=1
    log "库连接就绪：${NIK_DATABASE_USER:-?}@${NIK_DATABASE_HOST:-?}:${NIK_DATABASE_PORT:-?}/${NIK_DATABASE_NAME:-?}（env=${ENV_FILE}）"
  else
    warn "psql 连不上 ${NIK_DATABASE_USER:-?}@${NIK_DATABASE_HOST:-?}/${NIK_DATABASE_NAME:-?}（巡检结论/积压退回 /metrics）"
  fi
}

# ══════════════════════════ 检查项 ══════════════════════════
# 结果累积到 FOUND：每项一行 `key|LEVEL|title|detail`
FOUND=()

record() {
  local key="$1" line="$2" level title detail
  IFS='|' read -r level title detail <<<"$line"
  [ -n "${level:-}" ] || { err "检查 $key 没有产出结论"; return; }
  if [ "$level" = "OK" ]; then
    recovered "$key"
    log "$key: OK — $title"
    return
  fi
  if [ "$level" = "INFO" ]; then
    log "$key: INFO — $title（$detail）"
    return
  fi
  log "$key: $level — $title"
  FOUND+=("${key}|${level}|${title}|${detail}")
}

check_health() {
  http_fetch "${BASE_URL}/health"
  record health "$(decide_health "$HTTP_CODE" "$HTTP_BODY")"
}

check_deps() {
  http_fetch "${BASE_URL}/health/deps"
  record deps "$(decide_deps "$HTTP_CODE" "$HTTP_BODY")"
}

# ── 迁移时间口径（NEAR-FIX-4）─────────────────────────────────────────────────
# `schema_migrations.applied_at` 契约是 **epoch 秒**（引擎 `history.rs` 写的就是秒），但历史上
# 手工 psql 补记账的行写成了 **毫秒**（144 实测 4 行；`to_timestamp(applied_at)` 出来是 58826 年）。
# 混单位直接比大小 ⇒ 毫秒值永远大于结论的秒值 ⇒ **每一条结论都被判成「过期」**，
# 于是判定会被永久降级成 WARN/false。所以比较前必须先归一：
#   > 1e11 一律视为毫秒（1e11 秒 = 5138 年、1e11 毫秒 = 1973 年，两种单位不重叠）。
MIG_EPOCH_MS_FLOOR=100000000000
MIG_EPOCH_SQL="CASE WHEN applied_at > ${MIG_EPOCH_MS_FLOOR} THEN applied_at / 1000 ELSE applied_at END"

check_invariant() {
  local ok blockers failed age newer row="" have=0 src=""
  # 优先 /metrics（应用自己采的，不依赖本脚本的库口令）；缺失再退回只读 SQL
  http_fetch "${METRICS_URL}"
  ok="$(printf '%s' "$HTTP_BODY" | awk '$1 ~ /^cloudpivot_invariant_ok/ {print $2; exit}')"
  if [ -n "$ok" ] && [ "$ok" != "-1" ]; then
    blockers="$(printf '%s' "$HTTP_BODY" | awk '$1 ~ /^cloudpivot_invariant_blockers/ {print $2; exit}')"
    failed="$(printf '%s' "$HTTP_BODY" | awk '$1 ~ /^cloudpivot_invariant_failed/ {print $2; exit}')"
    age="$(printf '%s' "$HTTP_BODY" | awk '$1 ~ /^cloudpivot_invariant_age_seconds/ {print $2; exit}')"
    # 指标里没有「最后一次迁移」这一维（不归后端采样器管），另查一次库；查不到就按未知处理
    newer="$(psql_scalar "SELECT GREATEST(COALESCE((SELECT max(${MIG_EPOCH_SQL}) FROM schema_migrations WHERE success), 0) - EXTRACT(EPOCH FROM (SELECT run_at FROM sys_invariant_result ORDER BY run_at DESC LIMIT 1))::bigint, 0)" 2>/dev/null || true)"
    if [ -z "$newer" ]; then
      log "invariant: 无法判断结论是否早于最后一次迁移（psql 不可用）→ 按旧口径判定"
      newer=0
    fi
    row="$([ "$ok" = "1" ] && echo OK || echo FAIL)|${blockers:-0}|${failed:-0}|${age:-0}|${newer:-0}"
    have=1
    src="/metrics（cloudpivot_invariant_*）"
  else
    local raw
    # 一条 SQL 同时取「最后结论」与「它是否早于最后一次迁移」——分两次查会有竞态（期间落一条迁移/结论）。
    if raw="$(psql_scalar "SELECT r.status || '|' || r.blockers || '|' || r.failed || '|' || GREATEST(EXTRACT(EPOCH FROM (CURRENT_TIMESTAMP - r.run_at))::bigint, 0) || '|' || GREATEST(COALESCE((SELECT max(${MIG_EPOCH_SQL}) FROM schema_migrations WHERE success), 0) - EXTRACT(EPOCH FROM r.run_at)::bigint, 0) FROM sys_invariant_result r ORDER BY r.run_at DESC LIMIT 1")"; then
      row="$raw"
      have=1
      src="sys_invariant_result（psql 只读）"
    fi
  fi
  if [ "$have" -eq 0 ]; then
    if [ -n "${PSQL_READY:-}" ]; then
      record invariant "INFO|巡检结论查不到|库连上了（${NIK_DATABASE_NAME}）但 sys_invariant_result 查询失败：该库大概没应用 V0235 巡检迁移（145 base 实例就是这样）→ 巡检在这套部署上还没生效"
    else
      record invariant "INFO|巡检结论读不到|既没有 cloudpivot_invariant_* 指标，也连不上库；这项当前等于没人看"
    fi
    return
  fi
  log "invariant: 数据源 = ${src}${row:+（$row）}"
  record invariant "$(decide_invariant "$row")"
}

check_disk() {
  local pct
  pct="$(df -P "$DISK_PATH" 2>/dev/null | awk 'NR==2 {gsub(/%/,"",$5); print $5}')"
  record disk "$(decide_disk "${pct:-}" "$DISK_PATH")"
}

check_secret_expiry() {
  local found=0 file days end
  local files=()
  if [ -n "$CERT_FILES" ]; then
    IFS=':' read -r -a files <<<"$CERT_FILES"
  fi
  # shellcheck disable=SC2206  # glob 需要分词
  local globbed=( $CERT_GLOBS )
  for file in "${globbed[@]}" "${files[@]:-}"; do
    [ -n "$file" ] || continue
    [ -f "$file" ] || continue
    found=$((found + 1))
    end="$(openssl x509 -enddate -noout -in "$file" 2>/dev/null | cut -d= -f2)"
    if [ -z "$end" ]; then
      record secret_expiry "INFO|证书有效期读不到|${file}（openssl 解析失败）"
      continue
    fi
    days=$(( ( $(date -d "$end" +%s 2>/dev/null || echo 0) - $(NOW_EPOCH) ) / 86400 ))
    record secret_expiry "$(decide_cert "$days" "$file")"
  done
  if [ "$found" -eq 0 ]; then
    record secret_expiry "INFO|没有可检查的证书|当前部署是 HTTP（nginx 无 ssl_certificate）；JWT/存储主密钥是对称密钥，无有效期，靠轮换流程管理"
  fi
}

check_backlog() {
  local pending stuck src
  http_fetch "${METRICS_URL}" >/dev/null 2>&1
  pending="$(printf '%s' "$HTTP_BODY" | awk '$1 ~ /^cloudpivot_impexp_backlog_pending/ {print $2; exit}')"
  stuck="$(printf '%s' "$HTTP_BODY" | awk '$1 ~ /^cloudpivot_impexp_backlog_stuck_seconds/ {print $2; exit}')"
  if [ -n "$pending" ] && [ "$pending" != "-1" ]; then
    src="/metrics"
    [ -n "$stuck" ] || stuck=-1
  else
    if [ -n "${PSQL_READY:-}" ]; then
      src="sys_import_task+sys_export_task（psql 只读）"
    else
      src="psql（未就绪：无 env 文件或无口令）"
    fi
    pending="$(psql_scalar "SELECT (SELECT count(*) FROM sys_import_task WHERE deleted_at IS NULL AND status IN ('PENDING','PARSING','VALIDATED','IMPORTING')) + (SELECT count(*) FROM sys_export_task WHERE deleted_at IS NULL AND status IN ('PENDING','RUNNING'))")"
    stuck="$(psql_scalar "SELECT GREATEST(COALESCE((SELECT EXTRACT(EPOCH FROM (CURRENT_TIMESTAMP - min(created_at)))::bigint FROM sys_import_task WHERE deleted_at IS NULL AND status IN ('PENDING','PARSING','VALIDATED','IMPORTING')),0), COALESCE((SELECT EXTRACT(EPOCH FROM (CURRENT_TIMESTAMP - min(created_at)))::bigint FROM sys_export_task WHERE deleted_at IS NULL AND status IN ('PENDING','RUNNING')),0))")"
    [ -n "$pending" ] || pending=-1
    [ -n "$stuck" ] || stuck=-1
  fi
  log "backlog: 数据源 = ${src}"
  record backlog "$(decide_backlog "$pending" "$stuck")"
}

# ══════════════════════════ 去重 ══════════════════════════
state_file() { printf '%s/%s.last' "$STATE_DIR" "$1"; }

# 去重窗口内返回 0（应抑制）；否则记下当前时间并返回 1（应发）
should_fire() {
  local key="$1" file last now
  file="$(state_file "$key")"
  now="$(NOW_EPOCH)"
  [ "$FORCE" -eq 1 ] && { printf '%s' "$now" >"$file" 2>/dev/null; return 1; }
  if [ -r "$file" ]; then
    last="$(cat "$file" 2>/dev/null || echo 0)"
    case "$last" in ''|*[!0-9]*) last=0 ;; esac
    if [ $(( now - last )) -lt "$DEDUP_SECONDS" ]; then
      return 0
    fi
  fi
  printf '%s' "$now" >"$file" 2>/dev/null || warn "写不了去重状态文件 $file"
  return 1
}

recovered() {
  local key="$1" file
  file="$(state_file "$key")"
  [ -f "$file" ] || return 0
  rm -f "$file"
  log "$key: RECOVERED — 已恢复正常（该告警的去重状态已清除）"
  [ "$DRY_RUN" -eq 1 ] && return 0
  write_log_line "RECOVERED" "$key" "OK" "告警解除"
}

# ══════════════════════════ 通道 ══════════════════════════
write_log_line() {
  local level="$1" key="$2" title="$3" detail="$4"
  mkdir -p "$(dirname "$ALERT_LOG")" 2>/dev/null || true
  printf '[%s] %-9s host=%-15s check=%-13s %s | %s\n' \
    "$(STAMP)" "$level" "$HOST_TAG" "$key" "$title" "$detail" >>"$ALERT_LOG" 2>/dev/null \
    || warn "写不了 $ALERT_LOG"
}

# 本机是不是看板机（IP 命中）—— 决定建卡走本地 CLI 还是 ssh
is_board_host() {
  hostname -I 2>/dev/null | tr ' ' '\n' | grep -qx "$BOARD_IP"
}

kanban_available() {
  local mode="$KANBAN_MODE"
  if [ "$mode" = "auto" ]; then
    if is_board_host; then mode=local; else mode=ssh; fi
  fi
  printf '%s' "$mode"
}

sink_kanban() {
  local key="$1" level="$2" title="$3" detail="$4"
  local mode hermes_bin board_title body title_b64 body_b64 out rc
  mode="$(kanban_available)"
  case "$mode" in
    off) log "  ↳ kanban: 已关闭（NIK_ALERT_KANBAN=off）"; return 0 ;;
    local|ssh) ;;
    *) warn "  ↳ kanban: 未知模式 $mode（local|ssh|off）"; return 0 ;;
  esac

  board_title="【ALERT】${HOST_TAG} ${title}"
  body="**主机**：${HOST_TAG}（${BASE_URL}）
**检查项**：\`${key}\`（${level}）
**现象**：${title}
**详情**：${detail}
**时间**：$(STAMP)

本卡由 \`scripts/ops/alert_check.sh\` 自动创建（NEAR-FIX-3 告警接入）。
去重窗口 ${DEDUP_SECONDS}s：同一检查项在窗口内只建一张卡。处置完请归档本卡。"

  title_b64="$(printf '%s' "$board_title" | base64 -w0)"
  body_b64="$(printf '%s' "$body" | base64 -w0)"

  if [ "$mode" = "local" ]; then
    hermes_bin="${NIK_ALERT_HERMES_BIN:-$(command -v hermes || echo /root/.hermes/hermes-agent/.hermes/bin/hermes)}"
    [ -x "$hermes_bin" ] || { warn "  ↳ kanban: 本机没有 hermes 可执行文件（$hermes_bin），降级只写 ALERT.log"; return 0; }
    printf '%s' "$body" >/tmp/.cp_alert_body.$$ 2>/dev/null || true
    out="$(env -i PATH=/usr/local/bin:/usr/bin:/bin HOME=/root "$hermes_bin" kanban create "$board_title" \
             --assignee "$KANBAN_ASSIGNEE" --body-file /tmp/.cp_alert_body.$$ --json 2>&1)"
    rc=$?
    rm -f /tmp/.cp_alert_body.$$
  else
    command -v ssh >/dev/null 2>&1 || { warn "  ↳ kanban: 本机没有 ssh，降级只写 ALERT.log"; return 0; }
    out="$(timeout 45 ssh -o BatchMode=yes -o ConnectTimeout=8 "$BOARD_SSH" \
      "t=\$(printf %s '$title_b64' | base64 -d); b=\$(printf %s '$body_b64' | base64 -d); \
       printf '%s' \"\$b\" > /tmp/.cp_alert_body.\$\$; \
       env -i PATH=/usr/local/bin:/usr/bin:/bin HOME=/root \
         /root/.hermes/hermes-agent/.hermes/bin/hermes kanban create \"\$t\" \
         --assignee '$KANBAN_ASSIGNEE' --body-file /tmp/.cp_alert_body.\$\$ --json; \
       rc=\$?; rm -f /tmp/.cp_alert_body.\$\$; exit \$rc" 2>&1)"
    rc=$?
  fi
  if [ $rc -eq 0 ]; then
    local card_id
    card_id="$(printf '%s' "$out" | grep -o '"id": *"t_[0-9a-f]*"' | head -1 | grep -o 't_[0-9a-f]*')"
    log "  ↳ kanban: 已建卡 ${card_id:-（未解析到 id）}（${mode}，assignee=${KANBAN_ASSIGNEE}）"
    return 0
  fi
  warn "  ↳ kanban: 建卡失败 rc=$rc：$(printf '%s' "$out" | tail -2 | tr '\n' ' ')"
  return 1
}

sink_feishu() {
  local key="$1" level="$2" title="$3" detail="$4"
  local sender="${NIK_ALERT_FEISHU_SENDER:-/opt/cloudpivot/scripts/cron/feishu_alert_send.py}"
  [ "$FEISHU_MODE" = "off" ] && { log "  ↳ 飞书: 已关闭"; return 0; }
  [ -f "$sender" ] || { log "  ↳ 飞书: 无发送器（$sender），跳过"; return 0; }
  local text
  text="主机：${HOST_TAG}
检查：${key}（${level}）
现象：${title}
详情：${detail}
时间：$(STAMP)"
  if printf '%s' "$text" | python3 "$sender" --subject "【告警】${HOST_TAG} ${key}" --stdin >/dev/null 2>&1; then
    log "  ↳ 飞书: 已发送"
  else
    log "  ↳ 飞书: 未发送（无凭据或发送失败；144/145 无 FEISHU_* env 属预期）"
  fi
}

dispatch() {
  local item key level title detail
  for item in "${FOUND[@]:-}"; do
    [ -n "$item" ] || continue
    IFS='|' read -r key level title detail <<<"$item"
    if [ "$DRY_RUN" -eq 0 ]; then
      # should_fire：0 = 窗口内已告警过（抑制），1 = 应当发送 → 因此 0 才是「该抑制」
      if should_fire "$key"; then
        log "  ↳ ${key}: 去重窗口内已告警过（<${DEDUP_SECONDS}s），本次抑制"
        continue
      fi
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
      log "  ↳ dry-run：本该建卡/发飞书（${level} ${title}）"
      continue
    fi
    write_log_line "$level" "$key" "$title" "$detail"
    sink_kanban "$key" "$level" "$title" "$detail"
    sink_feishu "$key" "$level" "$title" "$detail"
  done
}

# ══════════════════════════ 自检 ══════════════════════════
self_test() {
  local fail=0 total=0
  expect() {
    local desc="$1" want="$2" got="$3"
    total=$((total + 1))
    if [ "$want" = "$got" ]; then
      printf '  [PASS] %s\n' "$desc"
    else
      printf '  [FAIL] %s（期望 %s，实得 %s）\n' "$desc" "$want" "$got"
      fail=$((fail + 1))
    fi
  }
  level_of() { printf '%s' "$1" | cut -d'|' -f1; }

  echo "== ① health =="
  expect "200 + status=ok → OK" OK "$(level_of "$(decide_health 200 '{"code":0,"data":{"status":"ok"}}')")"
  expect "200 + status=degraded → ALERT" ALERT "$(level_of "$(decide_health 200 '{"code":0,"data":{"status":"degraded"}}')")"
  expect "200 + 非 JSON → ALERT" ALERT "$(level_of "$(decide_health 200 '<html>spa fallback</html>')")"
  expect "000（连不上）→ ALERT" ALERT "$(level_of "$(decide_health 000 '')")"
  expect "500 → ALERT" ALERT "$(level_of "$(decide_health 500 '{"msg":"boom"}')")"

  echo "== ② deps =="
  expect "db/cache ok → OK" OK "$(level_of "$(decide_deps 200 '{"status":"ok","db":{"ok":true},"cache":{"ok":true}}')")"
  expect "cache 挂 → ALERT" ALERT "$(level_of "$(decide_deps 200 '{"status":"degraded","db":{"ok":true},"cache":{"ok":false,"error":"redis down"}}')")"
  expect "db 挂 → ALERT" ALERT "$(level_of "$(decide_deps 200 '{"status":"degraded","db":{"ok":false},"cache":{"ok":true}}')")"
  expect "连不上 → ALERT" ALERT "$(level_of "$(decide_deps 000 '')")"

  echo "== ③ invariant =="
  expect "OK/0/0/1h → OK"  OK    "$(level_of "$(decide_invariant 'OK|0|0|3600')")"
  expect "FAIL+BLOCKER → ALERT" ALERT "$(level_of "$(decide_invariant 'FAIL|2|5|3600')")"
  expect "FAIL 无 BLOCKER → ALERT" ALERT "$(level_of "$(decide_invariant 'FAIL|0|3|3600')")"
  expect "ERROR → ALERT（没跑成不是通过）" ALERT "$(level_of "$(decide_invariant 'ERROR|0|0|600')")"
  expect "过期 48h → ALERT"    ALERT "$(level_of "$(decide_invariant 'OK|0|0|172800')")"
  expect "无记录 → INFO"       INFO  "$(level_of "$(decide_invariant '')")"
  # NEAR-FIX-4：第 5 段 = 最后一次迁移比结论晚多少秒。>0 时不能断言「账已错」（本次误报的病根）
  expect "FAIL+BLOCKER 但结论早于最后一次迁移 → WARN（不判账已错）" WARN "$(level_of "$(decide_invariant 'FAIL|1|2|73800|14600')")"
  expect "FAIL 无 BLOCKER + 迁移比结论晚 → WARN"                    WARN "$(level_of "$(decide_invariant 'FAIL|0|3|600|60')")"
  expect "OK + 迁移比结论晚（结论仍新鲜）→ INFO"                    INFO "$(level_of "$(decide_invariant 'OK|0|0|600|60')")"
  expect "OK + 迁移比结论晚 + 已 48h 没重跑 → ALERT（无人复核）"    ALERT "$(level_of "$(decide_invariant 'OK|0|0|172800|60')")"
  expect "迁移不比结论晚（0）→ 旧口径：FAIL+BLOCKER → ALERT"        ALERT "$(level_of "$(decide_invariant 'FAIL|1|2|3600|0')")"
  expect "第 5 段是垃圾 → 当未知，仍判账已错 → ALERT"               ALERT "$(level_of "$(decide_invariant 'FAIL|1|2|3600|abc')")"
  expect "第 5 段是负数（时钟回拨）→ 当未知 → ALERT"                ALERT "$(level_of "$(decide_invariant 'FAIL|1|2|3600|-100')")"

  echo "== ④ disk =="
  expect "52% → OK"    OK    "$(level_of "$(decide_disk 52 /)")"
  expect "75% → WARN"  WARN  "$(level_of "$(decide_disk 75 /)")"
  expect "84% → WARN"  WARN  "$(level_of "$(decide_disk 84 /)")"
  expect "85% → ALERT" ALERT "$(level_of "$(decide_disk 85 /)")"
  expect "99% → ALERT" ALERT "$(level_of "$(decide_disk 99 /)")"

  echo "== ⑤ secret_expiry =="
  expect "剩 400 天 → OK"     OK    "$(level_of "$(decide_cert 400 /tmp/a.pem)")"
  expect "剩 30 天 → OK（边界）" OK  "$(level_of "$(decide_cert 30 /tmp/a.pem)")"
  expect "剩 29 天 → ALERT"   ALERT "$(level_of "$(decide_cert 29 /tmp/a.pem)")"
  expect "已过期 3 天 → ALERT" ALERT "$(level_of "$(decide_cert -3 /tmp/a.pem)")"

  echo "== ⑥ backlog =="
  expect "0 张 0s → OK"          OK    "$(level_of "$(decide_backlog 0 0)")"
  expect "3 张等 60s → OK"       OK    "$(level_of "$(decide_backlog 3 60)")"
  expect "1 张等 40 分钟 → ALERT" ALERT "$(level_of "$(decide_backlog 1 2400)")"
  expect "12 张 → ALERT"         ALERT "$(level_of "$(decide_backlog 12 60)")"
  expect "未知 -1/-1 → INFO"     INFO  "$(level_of "$(decide_backlog -1 -1)")"

  echo "== 去重（5 分钟窗口）=="
  local tmp_state; tmp_state="$(mktemp -d)"
  local saved_state="$STATE_DIR"
  STATE_DIR="$tmp_state"; DEDUP_SECONDS=300; FORCE=0
  should_fire delkey; local first=$?
  should_fire delkey; local second=$?
  [ "$first" -eq 1 ] && [ "$second" -eq 0 ] && r=ok || r=bad
  expect "首次应发、窗口内第二次应抑制" ok "$r"
  DEDUP_SECONDS=0
  should_fire delkey; local third=$?
  [ "$third" -eq 1 ] && r=ok || r=bad
  expect "窗口过期后应再次发送" ok "$r"
  STATE_DIR="$saved_state"; DEDUP_SECONDS="${NIK_ALERT_DEDUP_SECONDS:-300}"
  rm -rf "$tmp_state"

  echo "== 派发：去重 + ALERT.log（端到端，sinks 关掉不建卡）=="
  local tmpdir saved_log saved_state2 saved_kanban saved_feishu saved_dry n1 n2 n3
  tmpdir="$(mktemp -d)"
  saved_log="$ALERT_LOG"; saved_state2="$STATE_DIR"; saved_kanban="$KANBAN_MODE"
  saved_feishu="$FEISHU_MODE"; saved_dry="$DRY_RUN"
  ALERT_LOG="$tmpdir/ALERT.log"; STATE_DIR="$tmpdir/state"; mkdir -p "$STATE_DIR"
  KANBAN_MODE=off; FEISHU_MODE=off; DRY_RUN=0; DEDUP_SECONDS=300; FORCE=0
  FOUND=("demokey|ALERT|演示告警|详情")
  dispatch >/dev/null 2>&1
  n1="$(grep -c demokey "$ALERT_LOG" 2>/dev/null)"; n1="${n1:-0}"
  dispatch >/dev/null 2>&1
  n2="$(grep -c demokey "$ALERT_LOG" 2>/dev/null)"; n2="${n2:-0}"
  expect "首次派发：ALERT.log 写 1 条" 1 "$n1"
  expect "5 分钟内第二次派发：被去重（仍 1 条）" 1 "$n2"
  DRY_RUN=1; FOUND=("demokey2|ALERT|演示告警2|详情"); dispatch >/dev/null 2>&1
  n3="$(grep -c demokey2 "$ALERT_LOG" 2>/dev/null)"; n3="${n3:-0}"
  expect "dry-run 不写 ALERT.log" 0 "$n3"
  ALERT_LOG="$saved_log"; STATE_DIR="$saved_state2"; KANBAN_MODE="$saved_kanban"
  FEISHU_MODE="$saved_feishu"; DRY_RUN="$saved_dry"; FOUND=()
  rm -rf "$tmpdir"

  echo
  if [ "$fail" -eq 0 ]; then
    echo "自检通过：${total}/${total}"
    return 0
  fi
  echo "自检失败：${fail}/${total} 条不通过"
  return 1
}

# ══════════════════════════ 主流程 ══════════════════════════
if [ -n "${SELF_TEST:-}" ]; then
  self_test || exit 3
  exit 0
fi

if [ -n "${LIST_ONLY:-}" ]; then
  printf '检查项：%s\n' "$CHECKS_ALL"
  printf '  BASE_URL=%s\n  METRICS_URL=%s\n  HOST_TAG=%s\n  ALERT_LOG=%s\n  STATE_DIR=%s\n' \
    "$BASE_URL" "${BASE_URL%/}/metrics" "$HOST_TAG" "$ALERT_LOG" "$STATE_DIR"
  printf '  去重窗口=%ss  磁盘=%s%%(warn %s%%)  证书=%s天  积压=>%s分钟或≥%s张  巡检过期=%sh\n' \
    "$DEDUP_SECONDS" "$DISK_THRESHOLD" "$DISK_WARN" "$SECRET_WARN_DAYS" \
    "$BACKLOG_MINUTES" "$BACKLOG_COUNT" "$INVARIANT_MAX_AGE_HOURS"
  printf '  kanban=%s（%s）  feishu=%s\n' "$KANBAN_MODE" "$(kanban_available)" "$FEISHU_MODE"
  printf '  env_file=%s\n' "${ENV_FILE:-（自动探测）}"
  exit 0
fi

METRICS_URL="${NIK_ALERT_METRICS_URL:-${BASE_URL%/}/metrics}"
mkdir -p "$STATE_DIR" 2>/dev/null || warn "建不了状态目录 $STATE_DIR（去重会退化为不去重）"

# 同一时间只允许一个实例（cron 与手工并发会重复建卡）
LOCK_FILE="/tmp/.cloudpivot_alert_check.lock"
exec 9>"$LOCK_FILE" 2>/dev/null || true
if command -v flock >/dev/null 2>&1 && ! flock -n 9; then
  log "另一个 alert_check 正在跑（$LOCK_FILE），本次退出"
  exit 0
fi

init_db

RUN_LIST="$CHECKS_ALL"
[ -n "$ONLY" ] && RUN_LIST="$(printf '%s' "$ONLY" | tr ',' ' ')"

log "=== 告警巡检开始 host=${HOST_TAG} base=${BASE_URL} 检查=[${RUN_LIST}] dry_run=${DRY_RUN} ==="
for c in $RUN_LIST; do
  case "$c" in
    health)        check_health ;;
    deps)          check_deps ;;
    invariant)     check_invariant ;;
    disk)          check_disk ;;
    secret_expiry) check_secret_expiry ;;
    backlog)       check_backlog ;;
    *)             warn "未知检查项：$c（跳过）" ;;
  esac
done

if [ "${#FOUND[@]}" -eq 0 ]; then
  log "=== 巡检结束：无告警（${#FOUND[@]} 项）==="
  exit 0
fi
log "=== 巡检结束：${#FOUND[@]} 项告警，开始派发 ==="
dispatch
exit 0
