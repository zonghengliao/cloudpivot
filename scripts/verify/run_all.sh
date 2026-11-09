#!/usr/bin/env bash
# ==============================================================================
# 云枢 · 全量体检（一键跑：用例闸 → 单元 → 接口 → 不变量/联动 → E2E）
#
# 用法（在仓根跑）：
#   bash scripts/verify/run_all.sh                  # 全量（默认跳过 fmt/clippy，太慢）
#   bash scripts/verify/run_all.sh --fast           # 只跑「快的那几条」：用例闸 + 后端单测 + 前端单测
#   bash scripts/verify/run_all.sh --only invariants,e2e
#   bash scripts/verify/run_all.sh --with-static    # 额外跑 scripts/check.sh（fmt/clippy/check/typecheck）
#   bash scripts/verify/run_all.sh --list           # 列出所有 stage
#
# 退出码：0 = 全绿（允许 SKIP）；1 = 至少一个 stage FAIL。
#
# 为什么要有这个脚本：
#   仓库里现在有 10+ 个入口（check.sh / verify.py / erp_invariants.py / p10_state_machine.sh /
#   pnpm test:e2e / cargo test …），每次「全量验证」靠人记顺序 → 一定漏跑。
#   漏跑的那一项，就是下一个 TSK-149。
#
# ⚠️ E2E 打的是生产（默认 http://172.18.10.144）：全局限流 300 req/60s，
#    **一轮 ≈ 264 条请求**，所以同一 IP 60 秒内只许跑一轮；连跑请用
#    `bash scripts/run-full-rounds.sh 3 65`，别在本脚本外面再手工连点。
# ==============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${VERIFY_OUT_DIR:-$ROOT/.verify-out}"
mkdir -p "$OUT"
STAMP="$(date +%Y%m%d-%H%M%S)"
SUMMARY="$OUT/summary-$STAMP.txt"

ALL_STAGES="cases,backend-unit,frontend-unit,verify-api,invariants,linkage,e2e,static"
STAGES="$ALL_STAGES"
ONLY=""
FAST=0
WITH_STATIC=0

while [ $# -gt 0 ]; do
  case "$1" in
    --fast)        FAST=1; STAGES="cases,backend-unit,frontend-unit"; shift ;;
    --only)        if [ $# -lt 2 ]; then echo "--only 需要值，如 --only cases,e2e" >&2; exit 2; fi
                   ONLY="$2"; STAGES="$ONLY"; shift 2 ;;
    --only=*)      ONLY="${1#--only=}"; STAGES="$ONLY"; shift ;;
    --with-static) WITH_STATIC=1; shift ;;
    --list)        echo "stages: $ALL_STAGES"; exit 0 ;;
    -h|--help)     sed -n '2,26p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *)             echo "未知参数：$1（--list 看全部阶段）" >&2; exit 2 ;;
  esac
done
# `--only a,b` 两段写法：原来的判断写错（for 循环里的 shift 不生效），
# 实测 `--only cases` 会把 8 个阶段全跑一遍 —— 白等 5 分钟还以为只跑了一条。
if [ -n "$ONLY" ]; then
  bad_stage=""
  for s in ${ONLY//,/ }; do
    case ",$ALL_STAGES," in *",$s,"*) : ;; *) bad_stage="$s" ;; esac
  done
  if [ -n "$bad_stage" ]; then
    echo "未知阶段：$bad_stage（可选：$ALL_STAGES）" >&2; exit 2
  fi
fi
if [ "$WITH_STATIC" = "1" ] && ! printf '%s' "$STAGES" | grep -q static; then
  STAGES="$STAGES,static"
fi

RESULTS=()
FAILED=0
SKIPPED=0

say()  { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
ok()   { RESULTS+=("PASS  $1"); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { RESULTS+=("FAIL  $1"); FAILED=$((FAILED+1)); printf '  \033[31mFAIL\033[0m %s（日志 %s）\n' "$1" "$2"; }
skip() { RESULTS+=("SKIP  $1"); SKIPPED=$((SKIPPED+1)); printf '  \033[33mSKIP\033[0m %s（%s）\n' "$1" "$2"; }
has()  { case ",$STAGES," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }

# ---- 环境探测：区分「环境没起来」和「真的红了」 ----
# 不做这层，psql 没口令会被当成「账错了」的 BLOCKER，闹一场空警报（本卡实测踩过）。
api_ok() { curl -fsS -m 3 "${BASE_URL:-http://127.0.0.1:8023}/health" >/dev/null 2>&1; }
db_ok() {
  command -v psql >/dev/null 2>&1 || return 1
  PGPASSWORD="${NIK_DATABASE_PASSWORD:-}" psql -w -h "${NIK_DATABASE_HOST:-127.0.0.1}" \
    -p "${NIK_DATABASE_PORT:-5432}" -U "${NIK_DATABASE_USER:-cloudpivot}" \
    -d "${NIK_DATABASE_NAME:-cloudpivot}" -tAc 'select 1' >/dev/null 2>&1
}

run_stage() {  # run_stage <name> <logfile> <cmd...>
  local name="$1"; local log="$2"; shift 2
  say "$name"
  ( cd "$ROOT" && "$@" ) >"$log" 2>&1
  local rc=$?
  if [ $rc -eq 0 ]; then ok "$name"
  else bad "$name" "$log"; tail -20 "$log" | sed 's/^/      /'
  fi
}

printf '云枢全量体检  开始 %s\n阶段：%s\n输出：%s\n' "$(date '+%F %T')" "$STAGES" "$OUT" | tee "$SUMMARY"

# ---------------------------------------------------------------- ① 用例表自检
if has cases; then
  if command -v python3 >/dev/null 2>&1; then
    run_stage "① 用例总表一致性闸（≥200 条 / 编号连续 / 期望结果不模糊）" \
      "$OUT/cases-$STAMP.log" python3 scripts/verify/gen_test_cases.py --check
  else
    skip "① 用例总表一致性闸" "没有 python3"
  fi
fi

# ---------------------------------------------------------------- ② 后端单测
if has backend-unit; then
  if command -v cargo >/dev/null 2>&1; then
    run_stage "② 后端 cargo test（单元 + 集成）" \
      "$OUT/backend-unit-$STAMP.log" bash -c 'cd backend && cargo test --quiet'
  else
    skip "② 后端 cargo test" "没有 cargo"
  fi
fi

# ---------------------------------------------------------------- ③ 前端单测
if has frontend-unit; then
  if command -v pnpm >/dev/null 2>&1; then
    run_stage "③ 前端纯函数单测（node --test / tests/unit）" \
      "$OUT/frontend-unit-$STAMP.log" bash -c 'cd frontend && pnpm test:unit'
  else
    skip "③ 前端单测" "没有 pnpm"
  fi
fi

# ---------------------------------------------------------------- ④ 接口验证套件
if has verify-api; then
  if ! api_ok; then
    skip "④ 接口验证套件 verify.py all" "后端未起（先 BASE_URL=... 或起 :8023）"
  else
    run_stage "④ 接口验证套件 verify.py all（smoke+a+b+c+task+flow）" \
      "$OUT/verify-api-$STAMP.log" python3 scripts/verify/verify.py all
  fi
fi

# ---------------------------------------------------------------- ⑤ 不变量审计
if has invariants; then
  if ! db_ok; then
    skip "⑤ ERP 不变量审计" "连不上库（设 NIK_DATABASE_PASSWORD 或先起 PG）；这不是账错"
  else
    run_stage "⑤ ERP 不变量审计（条数以 erp_invariants.py 的 check() 为准）" \
      "$OUT/invariants-$STAMP.log" python3 scripts/verify/erp_invariants.py
    # 【E2E-FIX-1】主链路必需边巡检（只读）：`biz_flow_critical_edge` 里任何一条边
    # 在 `biz_flow_state_edge` 被 `enabled = false` → FAIL（功能会断）。
    # rc=4（白名单表不存在 = V0354 未部署）也算 FAIL：不许假装绿。
    # **本 stage 不会自动改回 enabled**（禁用可能是人工加严），只报警。
    run_stage "⑤ 主链路必需边巡检（被禁用 → 功能断，只读不自动改回）" \
      "$OUT/invariants-flow-critical-$STAMP.log" \
      bash scripts/verify/flow_critical_edges.sh "${NIK_DATABASE_NAME:-cloudpivot_dev}"
  fi
fi

# ---------------------------------------------------------------- ⑥ 联动扫描
# 口径：仓库里没有 scan_all_links.py；联动（单据↔审批↔待办）由这几个脚本承担。
if has linkage; then
  if ! api_ok; then
    skip "⑥ 联动扫描" "接口不可达（联动脚本要先登录）；设 BASE_URL 或起 :8023"
  else
    run_stage "⑥ 联动扫描：任务取消/删除 → 审批实例与待办级联" \
      "$OUT/linkage-cancel-$STAMP.log" bash scripts/verify/t11_cancel_cascade.sh
    run_stage "⑥ 联动扫描：流程删除级联" \
      "$OUT/linkage-flowcascade-$STAMP.log" bash scripts/verify/t13_flow_cascade.sh
  fi
fi

# ---------------------------------------------------------------- ⑦ E2E
if has e2e; then
  if command -v pnpm >/dev/null 2>&1; then
    # 生产限流：单轮 264 请求；如果前面 60s 内已经跑过一轮，这里会 429。
    echo "  （提示：生产限流 300req/60s，一轮 264 请求；连跑请用 scripts/run-full-rounds.sh）"
    run_stage "⑦ 前端 E2E（Playwright，打 ${E2E_BASE_URL:-生产}）" \
      "$OUT/e2e-$STAMP.log" bash -c 'cd frontend && pnpm test:e2e'
  else
    skip "⑦ 前端 E2E" "没有 pnpm"
  fi
fi

# ---------------------------------------------------------------- ⑧ 静态检查
if has static; then
  run_stage "⑧ 提交前静态检查（fmt/clippy/check/typecheck + 数据权限两闸）" \
    "$OUT/static-$STAMP.log" bash scripts/check.sh
fi

# ---------------------------------------------------------------- 汇总
say "汇总"
for line in "${RESULTS[@]}"; do echo "  $line"; done | tee -a "$SUMMARY"
printf '\nFAIL=%d  SKIP=%d  日志目录：%s\n' "$FAILED" "$SKIPPED" "$OUT" | tee -a "$SUMMARY"

if [ "$FAILED" -gt 0 ]; then
  printf '\033[31m结论：有 %d 个 stage 失败，不许提交。\033[0m\n' "$FAILED"
  exit 1
fi
printf '\033[32m结论：全绿（SKIP 项按环境缺失跳过，见上）。\033[0m\n'
exit 0
