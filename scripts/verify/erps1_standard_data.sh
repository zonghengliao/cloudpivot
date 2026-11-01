#!/bin/bash
# ======================================================
# ERP-S1 验收脚本：标准基础数据预置（迁移 V0187）
#
# 卡：kanban t_eee6774a
# 用法：
#   bash scripts/verify/erps1_standard_data.sh              # 自建/自毁隔离库 cloudpivot_s1v
#   S1_DB=my_db S1_KEEP=1 bash ...                          # 换库名 / 保留库
#
# 口径：
#   · 库 = 生产 schema 副本（pg_dump 生产库 → 新库），**不碰生产**
#   · 每条断言打印 PASS / FAIL，末尾给 PASS/FAIL 计数，FAIL>0 退出码 1
#   · 覆盖卡面 §四 的四条单测（①数量 ②幂等 ③不覆盖客户改名 ④is_system 区分）
#     + 精度口径 / 物料分类 7 值 / 结算方式字典 / down 往返
# ======================================================
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
DB=${S1_DB:-cloudpivot_s1v}
KEEP=${S1_KEEP:-0}
MIG="$REPO/backend/migrations/business/V0187__erp_standard_master_data"

set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -At -U $NIK_DATABASE_USER -h $NIK_DATABASE_HOST -p $NIK_DATABASE_PORT"
export PGCLIENTENCODING=UTF8

PASS=0
FAIL=0

ok() {
    PASS=$((PASS + 1))
    echo "PASS  $1"
}

no() {
    FAIL=$((FAIL + 1))
    echo "FAIL  $1"
}

# 单值断言：expect_eq <期望> <实际> <说明>
expect_eq() {
    if [ "$1" = "$2" ]; then ok "$3（$2）"; else no "$3：期望 [$1] 实际 [$2]"; fi
}

# 布尔断言：expect_true <条件表达式的结果:0/1> <说明>
expect_true() {
    if [ "$1" -eq 0 ]; then ok "$2"; else no "$2"; fi
}

q() { $PSQL -d "$DB" -c "$1"; }

echo "======================================================"
echo "ERP-S1 验收：V0187 标准基础数据（库 $DB）"
echo "======================================================"

# ---------- 0. 建副本库 ----------
echo "-- 0. 准备生产 schema 副本库"
su - postgres -c "dropdb --if-exists $DB" >/dev/null 2>&1
su - postgres -c "createdb -O $NIK_DATABASE_USER $DB" >/dev/null || { echo "建库失败"; exit 1; }
pg_dump -U "$NIK_DATABASE_USER" -h "$NIK_DATABASE_HOST" -p "$NIK_DATABASE_PORT" "$NIK_DATABASE_NAME" \
    | $PSQL -d "$DB" -q >/dev/null || { echo "pg_dump 失败"; exit 1; }
BASE_UNITS=$(q "select count(*) from biz_unit where deleted_at is null")
BASE_DICT=$(q "select count(*) from sys_dict_type where deleted_at is null")
echo "   副本基线：单位 $BASE_UNITS 个、字典类型 $BASE_DICT 个、迁移 max=$(q "select max(version::int) from schema_migrations")"

# ---------- 1. 迁移 up ----------
echo "-- 1. 应用 V0187 up"
OUT=$($PSQL -d "$DB" -q -v ON_ERROR_STOP=1 -f "$MIG.up.sql" 2>&1)
RC=$?
expect_true $([ $RC -eq 0 ] && echo 0 || echo 1) "迁移 up 执行成功"
echo "$OUT" | grep -q "V0187 自检通过" && ok "迁移内置自检块通过（NOTICE: $(echo "$OUT" | grep -o 'V0187 自检通过.*' | head -1)）" \
    || no "迁移内置自检块没吐成功 NOTICE"

# ---------- 2. 卡面单测①：单位数 ≥ 24 ----------
echo "-- 2. 卡面单测① 迁移跑完单位数 ≥ 24"
UNITS=$(q "select count(*) from biz_unit where deleted_at is null")
[ "$UNITS" -ge 24 ] && ok "单位总数 $UNITS ≥ 24" || no "单位总数只有 $UNITS（期望 ≥ 24）"
expect_eq "28" "$UNITS" "标准单位数量（卡面列了 28 个编码，其中 3 个原有）"
NEW_UNITS=$(q "select count(*) from biz_unit where deleted_at is null and unit_code in
  ('SET','PACK','PAIR','DOZEN','G','T','LB','M','CM','MM','KM','FT','M2','M3','L','ML','GAL',
   'ROLL','BAG','BOTTLE','CAN','TUBE','HOUR','DAY','MONTH')")
expect_eq "25" "$NEW_UNITS" "本次新增单位数量（28 - 原有 3）"
HAVE=$(q "select string_agg(unit_code, ',' order by unit_code) from biz_unit where deleted_at is null
  and unit_code in ('KG','M','BOX')")
expect_eq "BOX,KG,M" "$HAVE" "关键单位（千克 / 米 / 箱）都在，物料下拉能选到"

# ---------- 3. 无重复编码 ----------
echo "-- 3. 无重复编码（卡面验证 SQL）"
DUP=$(q "select count(*) from (select unit_code from biz_unit where deleted_at is null
        group by 1 having count(*) > 1) d")
expect_eq "0" "$DUP" "单位编码无重复"

# ---------- 4. 卡面单测②：重跑幂等 ----------
echo "-- 4. 卡面单测② 重跑迁移 → 无重复、行数不变"
$PSQL -d "$DB" -q -v ON_ERROR_STOP=1 -f "$MIG.up.sql" >/dev/null 2>&1
RC=$?
expect_true $([ $RC -eq 0 ] && echo 0 || echo 1) "重跑 up 不报错（幂等）"
expect_eq "$UNITS" "$(q "select count(*) from biz_unit where deleted_at is null")" "重跑后单位总数不变"
expect_eq "7" "$(q "select count(*) from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id
                       where t.dict_code='pay_method' and i.deleted_at is null")" "重跑后结算方式字典项仍是 7 个"

# ---------- 5. 卡面单测③：不覆盖客户改名 ----------
echo "-- 5. 卡面单测③ 客户改过名的单位不被覆盖"
# 模拟：客户把「千克」改成「公斤」，并把 is_system 关掉（客户自建语义）
q "update biz_unit set unit_name='公斤', is_system=false where unit_code='KG'" >/dev/null
$PSQL -d "$DB" -q -v ON_ERROR_STOP=1 -f "$MIG.up.sql" >/dev/null 2>&1
expect_eq "公斤" "$(q "select unit_name from biz_unit where unit_code='KG'")" "客户改的名字（公斤）没被迁移改回「千克」"
expect_eq "f" "$(q "select is_system from biz_unit where unit_code='KG'")" "改过名的行不会被标成系统预置（仍是客户自建 false）"
# 复原，后面的断言按原种子口径
q "update biz_unit set unit_name='千克', is_system=true where unit_code='KG'" >/dev/null

# ---------- 6. 卡面单测④：is_system 区分系统预置 / 客户自建 ----------
echo "-- 6. 卡面单测④ is_system 区分预置与自建"
expect_eq "28" "$(q "select count(*) from biz_unit where deleted_at is null and is_system")" "标准单位全部标了 is_system = true"
q "insert into biz_unit (unit_code, unit_name, is_base, rate, status, sort_order, precision)
   values ('S1-CUST-1','客户自建单位',1,1,1,999,0)" >/dev/null
expect_eq "f" "$(q "select is_system from biz_unit where unit_code='S1-CUST-1'")" "客户自建单位（列默认值）is_system = false"
q "delete from biz_unit where unit_code='S1-CUST-1'" >/dev/null
expect_eq "28" "$(q "select count(*) from biz_unit where deleted_at is null")" "清掉自建单位后回到 28"

# ---------- 7. 精度口径 ----------
echo "-- 7. precision 口径（个数 0 位、重量/长度/面积体积 3 位）"
expect_eq "0" "$(q "select count(*) from biz_unit where deleted_at is null and unit_code in
    ('PCS','SET','BOX','PACK','PAIR','DOZEN','ROLL','BAG','BOTTLE','CAN','TUBE','HOUR','DAY','MONTH') and precision <> 0")" \
    "数量 / 包装 / 时间类单位 precision = 0"
expect_eq "0" "$(q "select count(*) from biz_unit where deleted_at is null and unit_code in
    ('KG','G','T','LB','M','CM','MM','KM','FT','M2','M3','L','ML','GAL') and precision <> 3")" \
    "重量 / 长度 / 面积体积类单位 precision = 3"
expect_eq "14" "$(q "select count(*) from biz_unit where deleted_at is null and precision = 3")" "3 位小数的单位 14 个"
# CHECK 兜底：越界写不进去
q "savepoint s1p" >/dev/null 2>&1 || true
BAD=$($PSQL -d "$DB" -q -c "begin; insert into biz_unit (unit_code,unit_name,is_base,rate,status,sort_order,precision)
      values ('S1-BAD-PREC','越界',1,1,1,999,9); rollback;" 2>&1)
echo "$BAD" | grep -q "ck_biz_unit_precision" && ok "precision = 9 被 ck_biz_unit_precision 拦住" \
    || no "precision 越界没被 CHECK 拦住：$BAD"

# ---------- 8. 物料分类 7 值 ----------
echo "-- 8. 物料分类 ck_biz_material_type 7 值"
DEF=$(q "select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_material_type'")
for v in RAW SEMI FINISHED PACKAGING CONSUMABLE OFFICE SERVICE; do
    echo "$DEF" | grep -q "'$v'" && ok "分类 $v 在 CHECK 里" || no "分类 $v 不在 CHECK 里"
done
KEPT=$(q "select count(*) from biz_material where deleted_at is null
          and material_type in ('RAW','SEMI','FINISHED','SERVICE')")
expect_eq "$(q "select count(*) from biz_material where deleted_at is null")" "$KEPT" "已有物料的分类值全部仍在枚举内（零影响）"
BADM=$($PSQL -d "$DB" -q -c "begin; update biz_material set material_type='BOGUS'
       where id=(select id from biz_material where deleted_at is null limit 1); rollback;" 2>&1)
echo "$BADM" | grep -q "ck_biz_material_type" && ok "非法分类被 ck_biz_material_type 拦住" \
    || no "非法分类没被拦住：$BADM"

# ---------- 9. 结算方式字典 ----------
echo "-- 9. 结算方式字典 pay_method"
expect_eq "1" "$(q "select count(*) from sys_dict_type where dict_code='pay_method' and deleted_at is null")" "字典类型 pay_method 唯一"
expect_eq "7" "$(q "select count(*) from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id
                     where t.dict_code='pay_method' and i.deleted_at is null")" "字典项 7 个"
expect_eq "7" "$(q "select count(*) from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id
                     where t.dict_code='pay_method' and i.deleted_at is null
                       and i.item_value in ('现金','银行转账','承兑汇票','支票','微信','支付宝','赊账')")" \
    "字典项取值正好是卡面 7 种结算方式（按集合比，不依赖中文排序）"
expect_eq "1" "$(q "select count(*) from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id
                     where t.dict_code='pay_method' and i.is_default and i.item_value='现金'")" "现金是默认项"

# ---------- 10. down 往返 ----------
echo "-- 10. down 回滚"
OUT=$($PSQL -d "$DB" -q -v ON_ERROR_STOP=1 -f "$MIG.down.sql" 2>&1)
RC=$?
expect_true $([ $RC -eq 0 ] && echo 0 || echo 1) "down 执行成功"
expect_eq "3" "$(q "select count(*) from biz_unit where deleted_at is null")" "down 后只剩 V0137 的 3 个单位"
expect_eq "0" "$(q "select count(*) from sys_dict_type where dict_code='pay_method'")" "down 后字典类型已删"
expect_eq "0" "$(q "select count(*) from information_schema.columns where table_name='biz_unit'
                     and column_name in ('is_system','precision')")" "down 后两列已删"
# 分类 CHECK 是否真的收回 4 值：用行为断言（PG 会改写 IN 列表的文本形态，比文本不可靠）
DOWN4=$($PSQL -d "$DB" -q -c "begin; update biz_material set material_type='PACKAGING'
       where id=(select id from biz_material where deleted_at is null limit 1); rollback;" 2>&1)
echo "$DOWN4" | grep -q "ck_biz_material_type" && ok "down 后 7 值分类里的 PACKAGING 被拒（CHECK 收回 4 值）" \
    || no "down 后 PACKAGING 仍能写入，分类 CHECK 没收回去：$DOWN4"

echo "-- 11. down 后再 up（可重放）"
$PSQL -d "$DB" -q -v ON_ERROR_STOP=1 -f "$MIG.up.sql" >/dev/null 2>&1
expect_eq "28" "$(q "select count(*) from biz_unit where deleted_at is null")" "重放后单位回到 28"

# ---------- 收尾 ----------
echo "======================================================"
echo "PASS=$PASS FAIL=$FAIL"
echo "======================================================"
if [ "$KEEP" != "1" ]; then
    su - postgres -c "dropdb --if-exists $DB" >/dev/null 2>&1 && echo "已清理副本库 $DB（S1_KEEP=1 可保留）"
else
    echo "保留副本库 $DB"
fi
[ "$FAIL" -eq 0 ] || exit 1
