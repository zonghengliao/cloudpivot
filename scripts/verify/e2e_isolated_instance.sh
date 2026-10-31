#!/usr/bin/env bash
# =============================================================================
# E2E 隔离实例（副本库 + 隔离端口）—— E2E 唯一允许的默认目标
# =============================================================================
#
# 为什么必须有这个脚本
# --------------------
# 2026-10-31 11:34:11，prod（172.18.10.144）被写进一批
# `POST /api/business/erp/serial/batch-generate` 夹具（M-W1A-004 / WH01 / 10 件，
# 零成本、不写库位账），`erp_invariants.py` 当场 B1（BLOCKER）+ C3（WARN）变红。
# 仓库侧同源的坑是：`playwright.config.ts` / `global-setup.ts` / `support.ts` 三处默认 BASE_URL = prod。
# 现在三处都改成默认 `http://127.0.0.1:5189`（本脚本起的前端口），并用
# `frontend/e2e/target-guard.mjs` 做**运行期硬判据**：非回环隔离端口直接抛错。
#
# 本脚本做的事（一条命令起真家伙）
# -------------------------------
#   1. 源库（默认 prod）`pg_dump` → 本地副本库 `cloudpivot_e2e_iso`（owner 为应用账号）
#   2. 后端：profile=test，库指副本，listen 127.0.0.1:18089，Redis 换 db 9
#      （key：JWT / 存储主密钥分别落在 $RUN_DIR，重启不变，且**不碰** /etc/cloudpivot/*.env）
#   3. 前端：vite dev，`BACKEND_ORIGIN` 指上面那个后端，端口 5189（strictPort）
#   4. 打印可直接 source 的环境：`export E2E_BASE_URL=http://127.0.0.1:5189`
#
# 用法
# ----
#   bash scripts/verify/e2e_isolated_instance.sh up        # 起（默认从 prod 复制一份库）
#   bash scripts/verify/e2e_isolated_instance.sh status
#   bash scripts/verify/e2e_isolated_instance.sh down
#   E2E_ISO_SRC_MODE=none bash ... up                      # 不复制数据（空库，迁移自动跑）
#   E2E_ISO_FRESH=1 bash ... up                            # 重灌副本库（旧副本会被 drop）
#
# 端口/库名的硬约束（脚本自己先查一遍，越界直接退出）：
#   后端端口 ∈ 18000-18999，前端端口 ∈ 5100-5999 —— 和 target-guard.mjs 的判据一致。
# =============================================================================
set -uo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
FRONTEND_DIR="$REPO_ROOT/frontend"
BACKEND_DIR="$REPO_ROOT/backend"

# ---- 可调参数 ---------------------------------------------------------------
E2E_ISO_DB=${E2E_ISO_DB:-cloudpivot_e2e_iso}
E2E_ISO_BACKEND_PORT=${E2E_ISO_BACKEND_PORT:-18089}
E2E_ISO_FRONTEND_PORT=${E2E_ISO_FRONTEND_PORT:-5189}
E2E_ISO_OWNER=${E2E_ISO_OWNER:-e2erole}
E2E_ISO_OWNER_PASSWORD=${E2E_ISO_OWNER_PASSWORD:-e2eiso_2026}
E2E_ISO_PG_HOST=${E2E_ISO_PG_HOST:-127.0.0.1}
E2E_ISO_PG_PORT=${E2E_ISO_PG_PORT:-5432}
E2E_ISO_REDIS_DB=${E2E_ISO_REDIS_DB:-9}
# 源库：ssh://<host> 表示源库在别的机器上（读那台机器的 /etc/cloudpivot/cloudpivot.env）
#       local 表示源库就是本机（读本机 /etc/cloudpivot/cloudpivot.env 或 NIK_DATABASE_*）
E2E_ISO_SRC=${E2E_ISO_SRC:-ssh://root@172.18.10.144}
E2E_ISO_SRC_MODE=${E2E_ISO_SRC_MODE:-copy}   # copy | none
RUN_DIR=${E2E_ISO_RUN_DIR:-/tmp/e2e-iso}
BACKEND_BIN=${E2E_ISO_BACKEND_BIN:-$BACKEND_DIR/target/release/cloudpivot}
mkdir -p "$RUN_DIR"

say() { printf '%s\n' "$*"; }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 1; }

# ---- 0. 端口硬约束（与 target-guard.mjs 同一套判据） -------------------------
check_ports() {
  local bp=$1 fp=$2
  [[ "$bp" =~ ^[0-9]+$ ]] || die "后端端口不是数字：$bp"
  [[ "$fp" =~ ^[0-9]+$ ]] || die "前端端口不是数字：$fp"
  (( bp >= 18000 && bp <= 18999 )) || die "后端端口 $bp 不在隔离段 18000-18999（prod 是 8023）"
  (( fp >= 5100 && fp <= 5999 )) || die "前端端口 $fp 不在隔离段 5100-5999（dev 是 5173）"
  ss -ltn 2>/dev/null | grep -q ":$bp " && die "端口 $bp 已被占用，换一个（E2E_ISO_BACKEND_PORT=...）"
  ss -ltn 2>/dev/null | grep -q ":$fp " && die "端口 $fp 已被占用，换一个（E2E_ISO_FRONTEND_PORT=...）"
  say "✅ 端口判据：后端 $bp / 前端 $fp 都在隔离段且空闲"
}

# ---- 1. 副本库 ---------------------------------------------------------------
psql_super() { su - postgres -c "psql -v ON_ERROR_STOP=1 -At $*"; }

prepare_db() {
  [[ "$E2E_ISO_SRC_MODE" == "copy" || "$E2E_ISO_SRC_MODE" == "none" ]] || die "E2E_ISO_SRC_MODE 只能是 copy / none"

  # owner 角色（幂等）
  local role_exists
  role_exists=$(psql_super "-c \"select 1 from pg_roles where rolname='$E2E_ISO_OWNER'\"" 2>/dev/null | tr -d '[:space:]')
  if [[ "$role_exists" != "1" ]]; then
    psql_super "-c \"create role $E2E_ISO_OWNER login password '$E2E_ISO_OWNER_PASSWORD'\"" >/dev/null \
      || die "建角色 $E2E_ISO_OWNER 失败"
    say "✅ 新建库账号 $E2E_ISO_OWNER"
  fi

  # 副本库（E2E_ISO_FRESH=1 时先 drop，重灌一份干净副本）
  local db_exists
  db_exists=$(psql_super "-c \"select 1 from pg_database where datname='$E2E_ISO_DB'\"" 2>/dev/null | tr -d '[:space:]')
  if [[ "$db_exists" == "1" && "${E2E_ISO_FRESH:-0}" == "1" ]]; then
    psql_super "-c \"drop database $E2E_ISO_DB\"" >/dev/null || die "drop 旧副本库失败（有连接？先 down）"
    say "… 已 drop 旧副本库 $E2E_ISO_DB"
    db_exists=""
  fi
  if [[ "$db_exists" != "1" ]]; then
    su - postgres -c "createdb -O $E2E_ISO_OWNER $E2E_ISO_DB" >/dev/null || die "createdb 失败"
    say "✅ 新建副本库 $E2E_ISO_DB（owner=$E2E_ISO_OWNER）"
  else
    say "… 复用已有副本库 $E2E_ISO_DB（要干净副本用 E2E_ISO_FRESH=1）"
  fi

  [[ "$E2E_ISO_SRC_MODE" == "none" ]] && { say "… E2E_ISO_SRC_MODE=none：空库起，迁移自动跑"; return 0; }

  # 复制数据：源库可能在远端（prod），dump 从远端 stdout 直灌本地 copy
  local dump_cmd
  if [[ "$E2E_ISO_SRC" == ssh://* ]]; then
    local host=${E2E_ISO_SRC#ssh://}
    say "… 从 $host 的 prod 库 pg_dump（只读）→ 本地 $E2E_ISO_DB"
    dump_cmd="ssh $host 'set -a; . /etc/cloudpivot/cloudpivot.env; set +a; PGPASSWORD=\"\$NIK_DATABASE_PASSWORD\" pg_dump -h \"\$NIK_DATABASE_HOST\" -p \"\$NIK_DATABASE_PORT\" -U \"\$NIK_DATABASE_USER\" -d \"\$NIK_DATABASE_NAME\" --no-owner --no-privileges'"
  else
    say "… 从本机 /etc/cloudpivot/cloudpivot.env 指定的库 pg_dump → 本地 $E2E_ISO_DB"
    dump_cmd="( set -a; . /etc/cloudpivot/cloudpivot.env; set +a; PGPASSWORD=\"\$NIK_DATABASE_PASSWORD\" pg_dump -h \"\$NIK_DATABASE_HOST\" -p \"\$NIK_DATABASE_PORT\" -U \"\$NIK_DATABASE_USER\" -d \"\$NIK_DATABASE_NAME\" --no-owner --no-privileges )"
  fi
  PGPASSWORD="$E2E_ISO_OWNER_PASSWORD" bash -c "$dump_cmd" 2>"$RUN_DIR/dump.err" \
    | PGPASSWORD="$E2E_ISO_OWNER_PASSWORD" psql -q -h "$E2E_ISO_PG_HOST" -p "$E2E_ISO_PG_PORT" \
        -U "$E2E_ISO_OWNER" -d "$E2E_ISO_DB" -v ON_ERROR_STOP=1 >"$RUN_DIR/restore.log" 2>&1
  local rc=${PIPESTATUS[0]}
  [[ $rc -eq 0 ]] || { tail -5 "$RUN_DIR/dump.err" >&2; die "pg_dump 失败（rc=$rc）"; }
  local rows
  rows=$(PGPASSWORD="$E2E_ISO_OWNER_PASSWORD" psql -At -h "$E2E_ISO_PG_HOST" -p "$E2E_ISO_PG_PORT" \
          -U "$E2E_ISO_OWNER" -d "$E2E_ISO_DB" -c "select count(*) from sys_user" 2>/dev/null)
  say "✅ 副本库就绪：sys_user=$rows 行（源库一份拷贝，之后的造数只落这里）"
}

# ---- 2. 后端 -----------------------------------------------------------------
start_backend() {
  local jwt_file=$RUN_DIR/jwt_secret master_file=$RUN_DIR/storage_master_key
  [[ -s "$jwt_file" ]] || openssl rand -hex 32 > "$jwt_file"
  [[ -s "$master_file" ]] || openssl rand -hex 32 > "$master_file"

  [[ -x "$BACKEND_BIN" ]] || die "找不到后端二进制 $BACKEND_BIN（E2E_ISO_BACKEND_BIN 可指定；或 cd backend && cargo build --release）"

  # profile=test 的覆盖配置不入库（.gitignore），缺了就按模板补一份，避免「换台机器跑不起来」
  if [[ ! -f "$BACKEND_DIR/config/config-test.toml" && -f "$BACKEND_DIR/config/config-test.example.toml" ]]; then
    cp "$BACKEND_DIR/config/config-test.example.toml" "$BACKEND_DIR/config/config-test.toml"
    say "… 已按 config-test.example.toml 生成本地 config-test.toml（未入库）"
  fi

  (
    cd "$BACKEND_DIR" || exit 1
    export NIK_PROFILE=test
    export NIK_DATABASE_HOST="$E2E_ISO_PG_HOST" NIK_DATABASE_PORT="$E2E_ISO_PG_PORT"
    export NIK_DATABASE_NAME="$E2E_ISO_DB" NIK_DATABASE_USER="$E2E_ISO_OWNER"
    export NIK_DATABASE_PASSWORD="$E2E_ISO_OWNER_PASSWORD" NIK_DATABASE_SSL_MODE=prefer
    export NIK_SERVER_LISTEN="127.0.0.1:$E2E_ISO_BACKEND_PORT"
    # Redis：隔离实例换 db 号，避免和开发/prod 的键空间互相踩（test profile 默认不启用 redis，
    # 这里显式给 db 号只是为了「以后启用时也不会串」）
    export NIK_REDIS_URL="redis://127.0.0.1:6379/$E2E_ISO_REDIS_DB"
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(cat "$jwt_file")"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="$(cat "$master_file")"
    exec "$BACKEND_BIN"
  ) >"$RUN_DIR/backend.log" 2>&1 &
  echo $! > "$RUN_DIR/backend.pid"

  local i rc=1
  for i in $(seq 1 40); do
    if curl -fsS -m 2 "http://127.0.0.1:$E2E_ISO_BACKEND_PORT/health" >/dev/null 2>&1; then rc=0; break; fi
    sleep 1
  done
  [[ $rc -eq 0 ]] || { tail -15 "$RUN_DIR/backend.log" >&2; die "后端 40s 内没起来（日志 $RUN_DIR/backend.log）"; }
  say "✅ 后端就绪：http://127.0.0.1:$E2E_ISO_BACKEND_PORT（profile=test，库 $E2E_ISO_DB）"
}

# ---- 3. 前端 -----------------------------------------------------------------
start_frontend() {
  [[ -d "$FRONTEND_DIR/node_modules" ]] || die "缺 frontend/node_modules（cd frontend && pnpm install）"
  (
    cd "$FRONTEND_DIR" || exit 1
    export BACKEND_ORIGIN="http://127.0.0.1:$E2E_ISO_BACKEND_PORT"
    exec pnpm exec vite --port "$E2E_ISO_FRONTEND_PORT" --strictPort
  ) >"$RUN_DIR/frontend.log" 2>&1 &
  echo $! > "$RUN_DIR/frontend.pid"

  local i rc=1
  for i in $(seq 1 60); do
    if curl -fsS -m 2 "http://127.0.0.1:$E2E_ISO_FRONTEND_PORT/" >/dev/null 2>&1; then rc=0; break; fi
    sleep 1
  done
  [[ $rc -eq 0 ]] || { tail -15 "$RUN_DIR/frontend.log" >&2; die "前端 60s 内没起来（日志 $RUN_DIR/frontend.log）"; }
  # 反代真的通到后端了吗（拿一个必须鉴权的接口探一下：401 也算通，5xx/000 才是没通）
  local code
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 5 "http://127.0.0.1:$E2E_ISO_FRONTEND_PORT/api/system/auth/me")
  [[ "$code" == "401" || "$code" == "200" ]] || die "前端 → 后端反代不通（/api/system/auth/me = $code）"
  say "✅ 前端就绪：http://127.0.0.1:$E2E_ISO_FRONTEND_PORT（/api 反代 → 127.0.0.1:$E2E_ISO_BACKEND_PORT，探测码 $code）"
}

# ---- 4. 自证：这个实例确实不是 prod -----------------------------------------
self_check() {
  local has_prod_rows
  has_prod_rows=$(PGPASSWORD="$E2E_ISO_OWNER_PASSWORD" psql -At -h "$E2E_ISO_PG_HOST" -p "$E2E_ISO_PG_PORT" \
    -U "$E2E_ISO_OWNER" -d "$E2E_ISO_DB" -c "select count(*) from biz_material" 2>/dev/null)
  say "✅ 数据判据：副本库 biz_material=$has_prod_rows 行（造数只会落这库）"
  say ""
  say "把目标交给 Playwright："
  say "  export E2E_BASE_URL=http://127.0.0.1:$E2E_ISO_FRONTEND_PORT"
  say "  cd frontend && pnpm test:e2e"
  say ""
  say "（若 E2E_BASE_URL 指到 prod，frontend/e2e/target-guard.mjs 会直接拒跑；"
  say "  线上验收确需打 prod 走 E2E_ALLOW_PROD=1 E2E_PROD_REASON='…'，跑完必须做："
  say "    python3 scripts/verify/e2e_fixture_residue.py && python3 scripts/verify/erp_invariants.py）"
}

stop_all() {
  local f pid
  for f in backend frontend; do
    if [[ -s "$RUN_DIR/$f.pid" ]]; then
      pid=$(cat "$RUN_DIR/$f.pid")
      if kill -0 "$pid" 2>/dev/null; then
        kill -TERM "-$(ps -o pgid= "$pid" | tr -d ' ')" 2>/dev/null || kill -TERM "$pid" 2>/dev/null
        say "… 已停 $f (pid $pid)"
      fi
      rm -f "$RUN_DIR/$f.pid"
    fi
  done
  # 兜底：按端口收
  pkill -f "cloudpivot.*NIK_SERVER_LISTEN" 2>/dev/null || true
  for p in $(ss -ltnp 2>/dev/null | grep -E ":($E2E_ISO_BACKEND_PORT|$E2E_ISO_FRONTEND_PORT) " | grep -oE 'pid=[0-9]+' | cut -d= -f2 | sort -u); do
    kill -TERM "$p" 2>/dev/null && say "… 已停占用隔离端口的进程 $p"
  done
  say "✅ 已停隔离实例（副本库 $E2E_ISO_DB 保留，供事后取证；要删：su - postgres -c 'dropdb $E2E_ISO_DB'）"
}

status_all() {
  say "副本库 : $E2E_ISO_DB @ $E2E_ISO_PG_HOST:$E2E_ISO_PG_PORT (owner $E2E_ISO_OWNER)"
  say "后端   : http://127.0.0.1:$E2E_ISO_BACKEND_PORT  health=$(curl -s -o /dev/null -w '%{http_code}' -m 3 http://127.0.0.1:$E2E_ISO_BACKEND_PORT/health)"
  say "前端   : http://127.0.0.1:$E2E_ISO_FRONTEND_PORT  index=$(curl -s -o /dev/null -w '%{http_code}' -m 3 http://127.0.0.1:$E2E_ISO_FRONTEND_PORT/)"
  say "E2E_BASE_URL=http://127.0.0.1:$E2E_ISO_FRONTEND_PORT"
  say "日志    : $RUN_DIR/{backend,frontend}.log"
}

case "${1:-up}" in
  up)
    check_ports "$E2E_ISO_BACKEND_PORT" "$E2E_ISO_FRONTEND_PORT"
    prepare_db
    start_backend
    start_frontend
    self_check
    ;;
  down) stop_all ;;
  status) status_all ;;
  *) die "用法: $0 up|down|status" ;;
esac
