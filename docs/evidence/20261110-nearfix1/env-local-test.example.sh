# NEAR-FIX-1 本地实测环境（99 · 一次性库 nearfix1）
export NIK_PROFILE=test
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_NAME=nearfix2
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN=8099
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"   # ≥32 字符，实测口令不入库
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"   # 64 hex
export X_CONFIG_SECRET_KEY="$(openssl rand -hex 16)"
export NIK_ADMIN_PASSWORD=<本地实测口令，见卡内汇报>
export NIK_LOG_LEVEL=info
export RUST_LOG=cloudpivot=info,nik_core=warn
export NIK_REDIS_ENABLED=false
export NIK_CACHE_TYPE=local
export NIK_METRICS_ENABLED=false
