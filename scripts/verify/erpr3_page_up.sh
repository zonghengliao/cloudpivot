#!/usr/bin/env bash
# 起 ERP-R3 页面验收用的隔离实例（副本库的副本 + 后端 18097 + vite 15173）
#
#   bash scripts/verify/erpr3_page_up.sh [ok|broken]
#
#   ok     —— 只种 7 张正常凭证（勾稽一致，绿框）
#   broken —— 再补一张「借 1012 / 贷 1002 200」（归「其他」+ 黄条 + 红框，差 −200）
#
# 干的活：
#   ① 用 cloudpivot_e2e_iso（prod 的副本快照）再复制一份 cloudpivot_r3page（**不碰源库**）
#   ② 清空副本的凭证，种上 scripts/verify/erpr3_page_fixture.sql 的固定夹具
#   ③ 起 debug 后端（18097）→ 迁移自动跑到 V0185
#   ④ 起 vite dev（15173，BACKEND_ORIGIN=18097）
set -uo pipefail
MODE=${1:-ok}
SRC_DB=${SRC_DB:-cloudpivot_e2e_iso}
DB=${DB:-cloudpivot_r3page}
BPORT=${BPORT:-18097}
FPORT=${FPORT:-15173}
RUN=/tmp/r3page
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
mkdir -p "$RUN"
say() { printf '%s\n' "$*"; }

# 0) 停掉旧的（**按端口杀**：`pkill -f target/debug/cloudpivot` 会误杀同机别人的 demo 实例）
fuser -k "$BPORT/tcp" 2>/dev/null
pkill -9 -f "port $FPORT" 2>/dev/null
sleep 1

# 1) 副本库（默认从 144 生产库的 dump 还原；**不碰生产**）
DUMP=${DUMP:-/tmp/prod_r3.sql.gz}
su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 \
    && su - postgres -c "dropdb --force $DB" >/dev/null 2>&1
su - postgres -c "createdb -O e2erole $DB" || exit 1
if [ -f "$DUMP" ]; then
    gzip -dc "$DUMP" | PGPASSWORD=e2eiso_2026 psql -q -h 127.0.0.1 -U e2erole -d "$DB" >/dev/null 2>&1 \
        || { say "FATAL: 还原 $DUMP 失败"; exit 1; }
    say "== 副本库 $DB 就绪（源：$DUMP）"
else
    PGPASSWORD=e2eiso_2026 pg_dump -h 127.0.0.1 -U e2erole "$SRC_DB" 2>/dev/null \
        | PGPASSWORD=e2eiso_2026 psql -q -h 127.0.0.1 -U e2erole -d "$DB" >/dev/null 2>&1
    say "== 副本库 $DB 就绪（源库 $SRC_DB，无 $DUMP）"
fi

PSQLF() { PGPASSWORD=e2eiso_2026 psql -h 127.0.0.1 -U e2erole -d "$DB" -q -v ON_ERROR_STOP=1 -f "$1"; }
PSQLA() { PGPASSWORD=e2eiso_2026 psql -h 127.0.0.1 -U e2erole -d "$DB" -Atq -v ON_ERROR_STOP=1 -c "$1"; }

PSQLF "$REPO/scripts/verify/erpr3_page_fixture.sql" || { say "FATAL: 夹具失败"; exit 1; }
if [ "$MODE" = "ok" ]; then
    PSQLA "DELETE FROM biz_voucher WHERE voucher_no = '0009' AND voucher_word = 'ERPR3P'" >/dev/null
fi
say "== 夹具写入完成（模式 $MODE；凭证 $(PSQLA "select count(*) from biz_voucher") 张）"

# 2) 后端
cd "$REPO/backend" || exit 1
export NIK_PROFILE=test
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_NAME=$DB
export NIK_DATABASE_USER=e2erole
export NIK_DATABASE_PASSWORD=e2eiso_2026
export NIK_DATABASE_SSL_MODE=prefer
export NIK_SERVER_LISTEN=127.0.0.1:$BPORT
export NIK_MIDDLEWARE_AUTH_JWT_SECRET=bc23ce5e1a0722dd123d20134ebec277db20793fac8a74f7a305be1ed5ba403d
export CLOUDPIVOT_STORAGE_MASTER_KEY=5f75c324de29e2fe288093977086129e978469bb386998f50a3081bfeb8ef105
setsid nohup ./target/debug/cloudpivot > "$RUN/backend.log" 2>&1 < /dev/null &
BPID=$!
for _ in $(seq 1 180); do
    grep -q "数据库迁移完成" "$RUN/backend.log" 2>/dev/null && break
    kill -0 "$BPID" 2>/dev/null || { say "后端启动失败："; tail -20 "$RUN/backend.log"; exit 1; }
    sleep 1
done
grep -E "V0185 自检通过|数据库迁移完成" "$RUN/backend.log" | tail -2
sleep 2
say "== 后端 http://127.0.0.1:$BPORT（pid $BPID）"

# 3) 前端
cd "$REPO/frontend" || exit 1
BACKEND_ORIGIN="http://127.0.0.1:$BPORT" setsid nohup npx vite --host 127.0.0.1 --port "$FPORT" --strictPort \
    > "$RUN/frontend.log" 2>&1 < /dev/null &
FPID=$!
for _ in $(seq 1 90); do
    grep -q "ready in" "$RUN/frontend.log" 2>/dev/null && break
    sleep 1
done
tail -3 "$RUN/frontend.log"
say "== 前端 http://127.0.0.1:$FPORT（pid $FPID）"
say "E2E_BASE_URL=http://127.0.0.1:$FPORT  MODE=$MODE"
