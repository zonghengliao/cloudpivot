#!/usr/bin/env bash
# =============================================================================
# V0160 + 批量生成改判（无来源入库）—— 生产发布脚本
# =============================================================================
# 归属：t_421d8aa1（【后端】batch-generate 改判「无来源入库」）
# 规约：本卡**不做**生产部署，但发布动作必须落成脚本再 `bash`（terminal 里直接
#       `systemctl restart` 在无人值守会话会被安全策略拒绝）。
#
# 在 **prod 主机（172.18.10.144）** 上跑：
#   bash scripts/deploy/v0160-restart.sh --check-only     # 只做上线前体检（只读）
#   bash scripts/deploy/v0160-restart.sh --apply          # 体检 → 备份二进制 → 换二进制 → 重启 → 双验
#
# 发布顺序（不可颠倒，规格 v3 §7.2）：
#   ① 体检（下面 §2 的两条 SQL 必须 0 行）；
#   ② 装新二进制并重启 —— V0160 由框架在**启动时自动应用**（第一个被服务的请求之前
#      就已经迁移完成），所以「先迁移再发后端」在这一栈里等价于「同一次启动内完成」；
#   ③ 双验：迁移版本 160 + 1901/6401 在位 + /health 200 + 白名单路由无令牌 401（不是 404）
#      + `md5sum /proc/<MainPID>/exe` == 磁盘二进制（防「换了文件没重启」）；
#   ④ 前端必须**同一次发布**（表单必填单价），否则老前端提交 400 —— 本脚本不管前端，
#      由前端卡 t_324de51b 的发布脚本负责；两边窗口要对齐。
#
# 回滚：本脚本在启动前把旧二进制备份到 $BACKUP_DIR；失败会自动换回去并重启。
# =============================================================================
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
BACKEND_DIR="$REPO/backend"
NEW_BIN=${NEW_BIN:-$BACKEND_DIR/target/release/cloudpivot}
REL_BIN="$BACKEND_DIR/target/release/cloudpivot"
UNIT=${UNIT:-cloudpivot}
PORT=${PORT:-8023}
BASE_URL=${BASE_URL:-http://127.0.0.1:$PORT}
BACKUP_DIR=${BACKUP_DIR:-/root/release-backup/$(date +%Y%m%d-%H%M%S)}
MODE=${1:-}

say() { printf '%s\n' "$*"; }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 1; }

[[ "$MODE" == "--apply" || "$MODE" == "--check-only" ]] || die "用法：$0 --check-only|--apply"

# ---- 0. 库连接（prod 的 env 只在 prod 主机上） -------------------------------
[[ -r /etc/cloudpivot/cloudpivot.env ]] || die "读不到 /etc/cloudpivot/cloudpivot.env（本脚本只在 prod 主机跑）"
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:?}"
PSQL="psql -h ${NIK_DATABASE_HOST:-127.0.0.1} -p ${NIK_DATABASE_PORT:-5432} -U ${NIK_DATABASE_USER} -d ${NIK_DATABASE_NAME} -AtAF|"

# ---- 1. 现状快照 ------------------------------------------------------------
say "== 0) 现状"
$PSQL -c "select 'schema_version', coalesce(max(version::int),0)::text from schema_migrations union all
          select 'accounts_1901_6401', count(*)::text from biz_account
           where account_code in ('1901','6401') and deleted_at is null union all
          select 'vouchers', count(*)::text from biz_voucher"
CUR_MD5=$(md5sum "$(readlink -f "/proc/$(systemctl show -p MainPID --value "$UNIT")/exe" 2>/dev/null)" 2>/dev/null | cut -d' ' -f1 || true)
say "运行中的二进制 md5: ${CUR_MD5:-<未知>}"

# ---- 2. 上线前体检（只读；两条都必须 0 行，规格 v3 §7.3） ---------------------
say "== 1) 上线前体检（必须 0 行）"
BAD1=$($PSQL -c "
  SELECT count(*) FROM (
    SELECT s.material_id, s.warehouse_id, s.q
      FROM (SELECT material_id, warehouse_id, count(*)::numeric q
              FROM biz_material_serial
             WHERE status = 'IN_STOCK' AND deleted_at IS NULL
             GROUP BY 1, 2) s
     WHERE coalesce((SELECT sum(qty) FROM biz_inventory i
                      WHERE i.material_id = s.material_id AND i.warehouse_id = s.warehouse_id), 0) = 0
       AND s.q > 0) t")
BAD2=$($PSQL -c "
  SELECT count(*) FROM biz_material_serial s
    JOIN biz_inventory_location il ON il.id = s.location_id
   WHERE s.deleted_at IS NULL
     AND (il.material_id <> s.material_id OR il.warehouse_id <> s.warehouse_id)")
say "  ① 有在库号但成本账无行: ${BAD1:-<空>}"
say "  ② 序列号桶归属脏数据  : ${BAD2:-<空>}"
if [[ "${BAD1:-0}" != "0" || "${BAD2:-0}" != "0" ]]; then
  die "体检没过（非 0 行）⇒ 先按库存巡检修复，不要带病上线"
fi
say "  体检通过"

if [[ "$MODE" == "--check-only" ]]; then
  say "== --check-only：到此为止（未改任何东西）"
  exit 0
fi

# ---- 3. 换二进制 ------------------------------------------------------------
[[ -x "$NEW_BIN" ]] || die "找不到新二进制 $NEW_BIN（先 cargo build --release）"
mkdir -p "$BACKUP_DIR" || die "建备份目录失败"
if [[ -f "$REL_BIN" ]]; then
  cp -a "$REL_BIN" "$BACKUP_DIR/cloudpivot.bak" || die "备份旧二进制失败"
  say "== 2) 已备份旧二进制 → $BACKUP_DIR/cloudpivot.bak"
fi
# 运行中的 exe 不能被直接 cp 覆盖（ETXTBSY）⇒ 先落 .new 再 mv -f（原子替换 inode）
cp -f "$NEW_BIN" "$REL_BIN.new" || die "拷贝新二进制失败"
chmod 755 "$REL_BIN.new"
mv -f "$REL_BIN.new" "$REL_BIN" || die "替换二进制失败"
say "  新二进制就位：$(md5sum "$REL_BIN" | cut -d' ' -f1)"

# ---- 4. 重启（唯一允许出现 restart 的地方） ---------------------------------
say "== 3) 重启 $UNIT"
systemctl restart "$UNIT" || die "systemctl restart $UNIT 失败"

# ---- 5. 双验 ----------------------------------------------------------------
ok=1
for _ in $(seq 1 30); do
  if curl -fsS -m 3 "$BASE_URL/health" >/dev/null 2>&1; then ok=0; break; fi
  sleep 2
done
if [[ $ok -ne 0 ]]; then
  say "  健康检查失败 ⇒ 自动回滚"
  if [[ -f "$BACKUP_DIR/cloudpivot.bak" ]]; then
    cp -f "$BACKUP_DIR/cloudpivot.bak" "$REL_BIN.rollback" && mv -f "$REL_BIN.rollback" "$REL_BIN"
    systemctl restart "$UNIT" || true
  fi
  die "发布失败（已尝试回滚到 $BACKUP_DIR/cloudpivot.bak）"
fi
say "  /health 200"

VER=$($PSQL -c "select coalesce(max(version::int),0)::text from schema_migrations")
ACC=$($PSQL -c "select count(*)::text from biz_account where account_code in ('1901','6401') and deleted_at is null")
say "  schema_migrations max = $VER（应为 160）"
say "  科目 1901/6401 行数 = $ACC（应为 2）"
PROC_MD5=$(md5sum "/proc/$(systemctl show -p MainPID --value "$UNIT")/exe" 2>/dev/null | cut -d' ' -f1 || true)
DISK_MD5=$(md5sum "$REL_BIN" | cut -d' ' -f1)
say "  进程 md5=$PROC_MD5 / 磁盘 md5=$DISK_MD5"
CODE=$(curl -s -o /dev/null -w '%{http_code}' -m 5 "$BASE_URL/api/business/erp/serial/page")
say "  无令牌打 /api/business/erp/serial/page → $CODE（应为 401，404 说明路由没上）"

if [[ "$VER" != "160" || "$ACC" != "2" || "$PROC_MD5" != "$DISK_MD5" || "$CODE" != "401" ]]; then
  say "  ❌ 双验不通过（见上面四个数）—— 人工确认后再决定是否回滚"
  exit 1
fi
say "== ✅ 发布完成：V0160 已应用、科目在位、二进制与进程一致、路由已上"
say "   别忘了：前端（表单必填 unit_cost）必须同一次窗口发布；发布后跑"
say "   python3 scripts/verify/erp_invariants.py（应全绿（通过数=总条数）、BLOCKER 0、WARN 0）"
