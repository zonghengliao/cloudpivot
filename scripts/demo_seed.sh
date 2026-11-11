#!/usr/bin/env bash
# =============================================================================
# demo_seed.sh —— 一键灌入「能演示完整业务链」的演示数据（幂等 / 可清理）
#
# 客户拿到系统第一次登录看到的是空库：没有供应商、没有物料、没有订单、
# 没有流程实例 —— 首页四张指标卡全是 0，任何页面点进去都是「暂无数据」。
# 本脚本灌一套**能点开、能审批、能对账**的演示数据，覆盖：
#
#   ① 基础数据  3 供应商 / 5 客户 / 12 物料 / 2 仓库 + 6 库位（另有 ★WH-TRANSIT 在途仓，迁移自备）
#   ② 采购链    已完成的采购订单 → 收货入库 → 应付账款 → 付款；另有 1 张审批中的采购订单
#   ③ 销售链    已完成的销售订单 → 发货 → 应收 → 开票（销售发票 + 收入凭证）→ 收款；另有 1 张待审批的销售订单
#   ④ 库存      5 个库存桶有余额（3 个物料）/ 1 张在途调拨单 / 1 张草稿盘点单
#   ⑤ 财务      业务自动生成凭证 7 张 + 手工凭证 3 张 = 10 张；1405/2202 与模块账对得上
#               （其中销售开票那张的来源是销售发票 biz_invoice，source_type = INVOICE；
#                收入在开票时确认，挂账/应收不是凭证来源，见 SO-22 口径）
#   ⑥ 流程任务  3 条真实待办审批（采购/销售/报销，真实 biz_flow_instance + 节点 + 任务）
#               5 个工作任务（1 个已逾期）
#   ⑦ 公告消息  3 条已发布公告 + 5 条站内消息
#
# 三条硬要求（卡面 DELIVERY-2）：
#   · **幂等**：所有行的主键是固定 UUID（前缀 de300000-…），插入一律
#     `ON CONFLICT (id) DO NOTHING`；重复跑不会多一行。
#   · **可清理**：`--clean` 只删 `id::text LIKE 'de300000-%'` 的行，
#     真实数据（含 admin、真实供应商/单据）一行不碰。
#   · **不写死密码**：脚本里没有任何口令字面量。演示账号
#     （zhang.ming / li.na / wang.lei）的 password 列**从 admin 复制** ——
#     谁掌握 admin 口令，谁就能用演示账号登录；要独立口令请登录后改密。
#
# 时间口径：所有业务日期都是 `now() - interval`，不写死年份。为避免把凭证
#   做进「已结账期间」（ERP 有 F1 硬闸），日期统一钳在**本月 1 日**以后：
#   本月首日跑时，几天前的数据会挤在同一天，这是刻意的取舍。
#
# 用法
#   bash scripts/demo_seed.sh                 # 灌演示数据（幂等）
#   bash scripts/demo_seed.sh --dry-run       # 只跑不提交（BEGIN…ROLLBACK），看会不会报错
#   bash scripts/demo_seed.sh --clean         # 只删演示数据，真实数据不动
#   bash scripts/demo_seed.sh --db-url postgres://user:pw@host:5432/db
#
# 连接口径（按优先级）
#   1. --db-url <url> / 环境变量 DEMO_DB_URL
#   2. 环境变量 NIK_DATABASE_*（后端同一套变量），或 /etc/cloudpivot/*.env 里的同名变量
#   3. psql 默认（PGHOST/PGUSER/PGDATABASE 或本地 socket）
#
# 退出码：0 = 成功；1 = 执行失败（psql 报错）；2 = 参数 / 环境问题
# =============================================================================
set -euo pipefail

PROG="$(basename "${BASH_SOURCE[0]}")"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DB_URL="${DEMO_DB_URL:-}"
DO_CLEAN=0
DRY_RUN=0

# 演示数据主键前缀 —— clean / 统计 / 自检都只认这一个前缀，别处不要另起前缀。
readonly DEMO_PREFIX='de300000-'
readonly DEMO_LIKE='de300000-%'

usage() {
    sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

die() { printf '⛔ %s\n' "$*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
    case "$1" in
        --clean) DO_CLEAN=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        --db-url) [ "$#" -ge 2 ] || die "--db-url 需要一个值"; DB_URL="$2"; shift 2 ;;
        --db-url=*) DB_URL="${1#*=}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "未知参数：$1（--help 看用法）" ;;
    esac
done

# ---------------------------------------------------------------- 连接解析
# 把 NIK_DATABASE_* / PGHOST* 归一成一条 URL，交给 psql 单点使用。
build_url_from_env() {
    local host="${NIK_DATABASE_HOST:-${PGHOST:-}}" port="${NIK_DATABASE_PORT:-${PGPORT:-5432}}"
    local name="${NIK_DATABASE_NAME:-${PGDATABASE:-}}" user="${NIK_DATABASE_USER:-${PGUSER:-}}"
    local pass="${NIK_DATABASE_PASSWORD:-${PGPASSWORD:-}}"
    if [ -n "$host" ] && [ -n "$name" ] && [ -n "$user" ]; then
        if [ -n "$pass" ]; then
            printf 'postgres://%s:%s@%s:%s/%s' "$user" "$pass" "$host" "$port" "$name"
        else
            printf 'postgres://%s@%s:%s/%s' "$user" "$host" "$port" "$name"
        fi
    fi
}

if [ -z "$DB_URL" ] && [ -n "${NIK_DATABASE_NAME:-}" ]; then
    DB_URL="$(build_url_from_env)"
fi
if [ -z "$DB_URL" ]; then
    # 145 / 144 的部署习惯：库口令在 /etc/cloudpivot/*.env 里
    for f in /etc/cloudpivot/cloudpivot-dev.env /etc/cloudpivot/cloudpivot.env; do
        if [ -r "$f" ] && grep -q '^NIK_DATABASE_NAME=' "$f" 2>/dev/null; then
            set -a
            # shellcheck disable=SC1090
            . "$f"
            set +a
            DB_URL="$(build_url_from_env)"
            [ -n "$DB_URL" ] && break
        fi
    done
fi
[ -n "$DB_URL" ] || DB_URL="$(build_url_from_env)"
[ -n "$DB_URL" ] || die "连不上数据库：请给 --db-url，或设 NIK_DATABASE_HOST/NAME/USER(/PASSWORD)，或在 /etc/cloudpivot/*.env 里配好"

command -v psql >/dev/null 2>&1 || die "本机没有 psql（apt install postgresql-client）"

# 打印时隐掉口令
mask_url() {
    printf '%s' "$1" | sed -E 's#(//[^:/@]+):[^@]*@#\1:***@#'
}

PSQL=(psql "$DB_URL" -X -q -v ON_ERROR_STOP=1 --no-psqlrc)
probe() { "${PSQL[@]}" -Atc "$1"; }

# ---------------------------------------------------------------- 预检
# 空库（只跑到 system 迁移）时，业务表不存在 —— 必须先跑迁移，否则报错信息
# 会是一堆 "relation does not exist"，看不出该干什么。
preflight() {
    local missing
    missing="$(probe "
        SELECT string_agg(t, ', ')
          FROM unnest(ARRAY['biz_purchase_order','biz_sales_order','biz_stock_in','biz_voucher',
                            'biz_flow_instance','biz_flow_task','sys_announcement','biz_task']) AS t
         WHERE to_regclass('public.' || t) IS NULL")"
    [ -z "$missing" ] || die "库结构不完整（缺表：$missing）。先起一次后端让迁移跑完，或跑 scripts/setup_new_env.sh <库名>"

    # 演示链路依赖三条预置流程（迁移种好、且 is_current 版本唯一）
    local noflow
    noflow="$(probe "
        SELECT string_agg(x.code, ', ')
          FROM (VALUES ('po_approve'),('so_approve'),('expense')) AS x(code)
         WHERE NOT EXISTS (
               SELECT 1 FROM biz_flow_def d
                 JOIN biz_flow_def_version v ON v.def_id = d.id AND v.is_current
                WHERE d.flow_code = x.code AND d.deleted_at IS NULL)")"
    [ -z "$noflow" ] || die "库里的预置流程缺失：$noflow（迁移没跑全，先用 scripts/setup_new_env.sh 建库）"

    # 已有的真实数据不能撞我们的演示单号 / 演示编码
    local clash
    clash="$(probe "
        SELECT string_agg(x, ', ')
          FROM (
            SELECT order_no AS x FROM biz_purchase_order WHERE order_no LIKE 'PO-%' AND order_no LIKE '%-90____' AND id::text NOT LIKE '$DEMO_LIKE'
            UNION ALL SELECT so_no FROM biz_sales_order WHERE so_no LIKE 'SO-%' AND so_no LIKE '%-90____' AND id::text NOT LIKE '$DEMO_LIKE'
            UNION ALL SELECT in_no FROM biz_stock_in WHERE in_no LIKE 'IN-%' AND in_no LIKE '%-90____' AND id::text NOT LIKE '$DEMO_LIKE'
            UNION ALL SELECT supplier_code FROM biz_supplier WHERE supplier_code IN ('SUP-9001','SUP-9002','SUP-9003') AND id::text NOT LIKE '$DEMO_LIKE'
            UNION ALL SELECT code FROM biz_customer WHERE code IN ('CUS-9001','CUS-9002','CUS-9003','CUS-9004','CUS-9005') AND id::text NOT LIKE '$DEMO_LIKE'
            UNION ALL SELECT material_code FROM biz_material WHERE material_code BETWEEN 'MAT-9001' AND 'MAT-9012' AND id::text NOT LIKE '$DEMO_LIKE'
          ) s")"
    [ -z "$clash" ] || die "库里有与演示数据重名的真实单据/编码：$clash。改名后再跑，或先确认这些是不是历史演示残留"
}

# ---------------------------------------------------------------- 灌数据
seed() {
    local tail="COMMIT;"
    [ "$DRY_RUN" -eq 1 ] && tail="ROLLBACK;"
    {
        echo "BEGIN;"
        cat <<'SQL'
-- ===========================================================================
-- DEMO SEED（前缀 de300000-）· 全部幂等
-- ===========================================================================
SET LOCAL client_min_messages = warning;

-- ---- 小工具：按业务编码取主数据 id（取不到 = NULL → NOT NULL 立刻炸，
--      比静默灌一半数据强）---------------------------------------------
CREATE FUNCTION pg_temp.acct(code text) RETURNS uuid LANGUAGE sql STABLE AS
$$ SELECT id FROM biz_account WHERE account_code = $1 AND deleted_at IS NULL LIMIT 1 $$;
CREATE FUNCTION pg_temp.unit(code text) RETURNS uuid LANGUAGE sql STABLE AS
$$ SELECT id FROM biz_unit WHERE unit_code = $1 AND deleted_at IS NULL LIMIT 1 $$;
CREATE FUNCTION pg_temp.uid(login text) RETURNS uuid LANGUAGE sql STABLE AS
$$ SELECT id FROM sys_user WHERE username = $1 AND deleted_at IS NULL LIMIT 1 $$;
CREATE FUNCTION pg_temp.role(code text) RETURNS uuid LANGUAGE sql STABLE AS
$$ SELECT id FROM sys_role WHERE role_code = $1 AND deleted_at IS NULL LIMIT 1 $$;
-- 演示日期：now() - N 天，但钳在本月 1 日之后（不落进已结账期间）
CREATE FUNCTION pg_temp.d(n int) RETURNS date LANGUAGE sql STABLE AS
$$ SELECT GREATEST((now() - (n || ' days')::interval), date_trunc('month', now()))::date $$;
CREATE FUNCTION pg_temp.ts(n int) RETURNS timestamptz LANGUAGE sql STABLE AS
$$ SELECT GREATEST((now() - (n || ' days')::interval), date_trunc('month', now())) $$;
-- 手工时间：N 小时前（待办/消息用，保证“刚刚发生”）
CREATE FUNCTION pg_temp.h(n int) RETURNS timestamptz LANGUAGE sql STABLE AS
$$ SELECT now() - (n || ' hours')::interval $$;

-- ===========================================================================
-- ① 组织 / 账号（演示单据的发起人）—— 口令继承 admin，脚本里不出现任何明文
-- ===========================================================================
INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, dept_path, status, sort_order)
VALUES ('de300000-0000-4000-8000-000000000001', 'D900', '演示事业部', NULL,
        '/de300000-0000-4000-8000-000000000001/', 1, 900)
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_user (id, username, password, real_name, dept_id, status, user_type,
                      must_change_password, token_version, remark)
SELECT v.id::uuid, v.username, a.password, v.real_name,
       'de300000-0000-4000-8000-000000000001'::uuid, 1, 'HUMAN', false, 1,
       '演示账号（口令继承 admin，请登录后改密）'
  FROM (VALUES
        ('de300000-0000-4000-8000-000000000601', 'zhang.ming', '张明'),
        ('de300000-0000-4000-8000-000000000602', 'li.na',      '李娜'),
        ('de300000-0000-4000-8000-000000000603', 'wang.lei',   '王磊'),
        ('de300000-0000-4000-8000-000000000604', 'chen.li',    '陈立')
       ) AS v(id, username, real_name)
  JOIN sys_user a ON a.username = 'admin' AND a.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- 角色矩阵：**每个审批岗都要有人**（首登向导会检查「审批岗是否配人」，
-- 空着的角色会让客户以为系统没配好）。同一行 = 一个人的岗位集合。
-- id 用 gen_random_uuid()：一个人的多个角色不能共用同一个 id（sys_user_role 的 PK 是 id，
-- 复用会被 ON CONFLICT 静默丢掉，只留下第一个角色 —— 踩过）。幂等靠 (user_id, role_id) 唯一键。
INSERT INTO sys_user_role (id, user_id, role_id)
SELECT gen_random_uuid(), v.uid::uuid, pg_temp.role(v.role_code)
  FROM (VALUES
        -- 张明：采购经理 + 部门主管 + 仓管主管 + 员工自助
        ('de300000-0000-4000-8000-000000000601', 'PURCHASE_MANAGER'),
        ('de300000-0000-4000-8000-000000000601', 'DEPT_LEADER'),
        ('de300000-0000-4000-8000-000000000601', 'WAREHOUSE_MANAGER'),
        ('de300000-0000-4000-8000-000000000601', 'EMPLOYEE'),
        -- 李娜：销售主管 + 销售员 + 部门主管 + 员工自助
        ('de300000-0000-4000-8000-000000000602', 'SALES_SUPERVISOR'),
        ('de300000-0000-4000-8000-000000000602', 'SALES_CLERK'),
        ('de300000-0000-4000-8000-000000000602', 'DEPT_LEADER'),
        ('de300000-0000-4000-8000-000000000602', 'EMPLOYEE'),
        -- 王磊：财务经理 + 财务总监 + 员工自助
        ('de300000-0000-4000-8000-000000000603', 'FINANCE_MANAGER'),
        ('de300000-0000-4000-8000-000000000603', 'FINANCE_DIRECTOR'),
        ('de300000-0000-4000-8000-000000000603', 'EMPLOYEE'),
        -- 陈立：总经理 + 流程配置复核 + 员工自助
        ('de300000-0000-4000-8000-000000000604', 'GENERAL_MANAGER'),
        ('de300000-0000-4000-8000-000000000604', 'FLOW_CONFIG_APPROVER'),
        ('de300000-0000-4000-8000-000000000604', 'EMPLOYEE')
       ) AS v(uid, role_code)
 WHERE pg_temp.role(v.role_code) IS NOT NULL
ON CONFLICT (user_id, role_id) DO NOTHING;

-- ===========================================================================
-- ② 基础数据
-- ===========================================================================
INSERT INTO biz_supplier (id, supplier_code, supplier_name, short_name, contact_person,
                          contact_phone, email, tax_no, address, credit_days, credit_limit,
                          payable_account_id, invoice_title, invoice_bank_name,
                          invoice_bank_account, status, remark)
VALUES
 ('de300000-0000-4000-8000-000000000101', 'SUP-9001', '苏州精工机械有限公司', '苏州精工',
  '陈志强', '0512-6688 1234', 'chen.zq@suzhou-jinggong.cn', '91320500MA1K2X8Y3A',
  '江苏省苏州市工业园区星湖街218号A栋', 30, 200000, pg_temp.acct('2202'),
  '苏州精工机械有限公司', '中国银行苏州工业园区支行', '4550 1234 5678 9012', 1, '结构件主供方'),
 ('de300000-0000-4000-8000-000000000102', 'SUP-9002', '东莞恒信电子科技有限公司', '东莞恒信',
  '李文博', '0769-8877 6655', 'liwb@hengxin-elec.com', '91441900MA4WQ7T92K',
  '广东省东莞市松山湖科技九路5号', 45, 150000, pg_temp.acct('2202'),
  '东莞恒信电子科技有限公司', NULL, NULL, 1, '电子料主供方'),
 ('de300000-0000-4000-8000-000000000103', 'SUP-9003', '宁波华联包装材料厂', '宁波华联',
  '王海燕', '0574-8333 2211', 'wanghy@nb-hualian.com', '91330200MA2AF5RB7X',
  '浙江省宁波市北仑区滨海大道88号', 30, 50000, pg_temp.acct('2202'),
  '宁波华联包装材料厂', NULL, NULL, 1, '包材/辅料')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_customer (id, code, name, tax_no, contact_name, contact_phone, address,
                          credit_limit, credit_days, credit_used, status, remark)
VALUES
 ('de300000-0000-4000-8000-000000000201', 'CUS-9001', '上海远洋贸易有限公司', '91310115MA1K3H8N2C',
  '周静', '021-5888 1234', '上海市浦东新区世纪大道1588号', 500000, 30, 0, 'ACTIVE', '战略客户·信用额度已核'),
 ('de300000-0000-4000-8000-000000000202', 'CUS-9002', '杭州天目电子有限公司', '91330106MA2CD9X41F',
  '吴凯', '0571-8765 4321', '浙江省杭州市西湖区文三路 90 号', 0, 0, 0, 'ACTIVE', '现款客户'),
 ('de300000-0000-4000-8000-000000000203', 'CUS-9003', '南京金鼎机电设备有限公司', '91320115MA1MT4YQ7B',
  '刘慧', '025-8333 6677', '江苏省南京市江宁区双龙大道 1698 号', 200000, 30, 0, 'ACTIVE', ''),
 ('de300000-0000-4000-8000-000000000204', 'CUS-9004', '合肥中辰新能源有限公司', '91340100MA2N5C7P8D',
  '张涛', '0551-6555 8899', '安徽省合肥市高新区望江西路 666 号', 300000, 45, 0, 'ACTIVE', ''),
 ('de300000-0000-4000-8000-000000000205', 'CUS-9005', '无锡华光精密制造有限公司', '91320214MA1WQ2E93G',
  '孙倩', '0510-8222 3344', '江苏省无锡市新吴区菱湖大道 200 号', 0, 0, 0, 'ACTIVE', '')
ON CONFLICT (id) DO NOTHING;

-- 12 个物料：MAT-9003 / MAT-9009 = 批次管理（收货带批号），MAT-9005 = 一物一码（序列号）
INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                          default_warehouse_id, safety_stock, costing_method, inventory_account_id,
                          tax_rate, tax_rate_mode, status, serial_required, standard_price, remark)
VALUES
 ('de300000-0000-4000-8000-000000000301', 'MAT-9001', '精密铝合金外壳', 'AL6061 200×120×15mm',
  pg_temp.unit('PCS'), 'RAW', 'de300000-0000-4000-8000-000000000401', 20, 'MOVING_AVG', pg_temp.acct('1405'), 13, 'PERCENT', 1, false, 12.00, ''),
 ('de300000-0000-4000-8000-000000000302', 'MAT-9002', '不锈钢内六角螺丝', 'M4×12 SUS304',
  pg_temp.unit('PCS'), 'RAW', 'de300000-0000-4000-8000-000000000401', 100, 'MOVING_AVG', pg_temp.acct('1405'), 13, 'PERCENT', 1, false, 22.00, ''),
 ('de300000-0000-4000-8000-000000000303', 'MAT-9003', '工业润滑油', '46# 抗磨液压油 18L/桶',
  pg_temp.unit('L'), 'RAW', 'de300000-0000-4000-8000-000000000401', 10, 'MOVING_AVG', pg_temp.acct('1405'), 13, 'PERCENT', 1, false, 26.00, '批次管理：按入库批号追溯'),
 ('de300000-0000-4000-8000-000000000304', 'MAT-9004', '控制主板', 'PCB-4L 双面贴装',
  pg_temp.unit('PCS'), 'SEMI', 'de300000-0000-4000-8000-000000000401', 30, 'MOVING_AVG', pg_temp.acct('1405'), 13, 'PERCENT', 1, false, 48.00, '批次管理（关键件）'),
 ('de300000-0000-4000-8000-000000000305', 'MAT-9005', '伺服电机', '750W 带刹车',
  pg_temp.unit('SET'), 'SEMI', 'de300000-0000-4000-8000-000000000401', 5, 'MOVING_AVG', pg_temp.acct('1405'), 13, 'PERCENT', 1, true, 3600.00, '一物一码：序列号管控'),
 ('de300000-0000-4000-8000-000000000306', 'MAT-9006', '信号线缆', 'RVV 4×0.75',
  pg_temp.unit('M'), 'RAW', 'de300000-0000-4000-8000-000000000401', 200, 'MOVING_AVG', pg_temp.acct('1405'), 13, 'PERCENT', 1, false, 13.00, ''),
 ('de300000-0000-4000-8000-000000000307', 'MAT-9007', '工业触摸屏', '10.1 寸 电容式',
  pg_temp.unit('PCS'), 'SEMI', 'de300000-0000-4000-8000-000000000401', 10, 'MOVING_AVG', pg_temp.acct('1405'), 13, 'PERCENT', 1, false, 420.00, ''),
 ('de300000-0000-4000-8000-000000000308', 'MAT-9008', '锂电池组', '48V 20Ah 带 BMS',
  pg_temp.unit('SET'), 'FINISHED', 'de300000-0000-4000-8000-000000000402', 5, 'MOVING_AVG', pg_temp.acct('1405'), 13, 'PERCENT', 1, false, 3200.00, ''),
 ('de300000-0000-4000-8000-000000000309', 'MAT-9009', '瓦楞纸箱', '600×400×300 五层',
  pg_temp.unit('PCS'), 'PACKAGING', 'de300000-0000-4000-8000-000000000402', 200, 'MOVING_AVG', pg_temp.acct('1405'), 13, 'PERCENT', 1, false, 3.50, '批次管理：按纸板批号'),
 ('de300000-0000-4000-8000-000000000310', 'MAT-9010', '产品说明书', '彩色印刷 8 页',
  pg_temp.unit('PCS'), 'CONSUMABLE', 'de300000-0000-4000-8000-000000000402', 500, 'MOVING_AVG', pg_temp.acct('1405'), 13, 'PERCENT', 1, false, 1.20, ''),
 ('de300000-0000-4000-8000-000000000311', 'MAT-9011', 'A4 复印纸', '70g 500 张/包',
  pg_temp.unit('BAG'), 'OFFICE', 'de300000-0000-4000-8000-000000000402', 20, 'MOVING_AVG', pg_temp.acct('1405'), 13, 'PERCENT', 1, false, 25.00, ''),
 ('de300000-0000-4000-8000-000000000312', 'MAT-9012', '安装调试服务', '现场安装/调试（按工时）',
  pg_temp.unit('HOUR'), 'SERVICE', 'de300000-0000-4000-8000-000000000402', 0, 'MOVING_AVG', pg_temp.acct('1405'), 6, 'PERCENT', 1, false, 300.00, '服务类，不建库存')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, manager,
                           allow_negative, address, status, sort_order, level, is_leaf, warehouse_path)
VALUES
 ('de300000-0000-4000-8000-000000000401', 'WH-91', '苏州主仓', 'NORMAL', '张明', 0,
  '苏州市工业园区星湖街 218 号 1 号库', 1, 10, 1, 1, '/de300000-0000-4000-8000-000000000401/'),
 ('de300000-0000-4000-8000-000000000402', 'WH-92', '合肥分仓', 'NORMAL', '李娜', 0,
  '合肥市高新区望江西路 666 号 3 号库', 1, 20, 1, 1, '/de300000-0000-4000-8000-000000000402/')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                          is_default, status, sort_order)
VALUES
 ('de300000-0000-4000-8000-000000000501', 'A-01-01', 'A 区 01 排 01 位', 'de300000-0000-4000-8000-000000000401', 'STORAGE', 1, 1, 10),
 ('de300000-0000-4000-8000-000000000502', 'A-01-02', 'A 区 01 排 02 位', 'de300000-0000-4000-8000-000000000401', 'STORAGE', 0, 1, 20),
 ('de300000-0000-4000-8000-000000000503', 'A-02-01', 'A 区 02 排 01 位', 'de300000-0000-4000-8000-000000000401', 'STORAGE', 0, 1, 30),
 ('de300000-0000-4000-8000-000000000504', 'A-03-01', 'A 区 03 排 01 位（待检）', 'de300000-0000-4000-8000-000000000401', 'QUARANTINE', 0, 1, 40),
 ('de300000-0000-4000-8000-000000000505', 'B-01-01', 'B 区 01 排 01 位', 'de300000-0000-4000-8000-000000000402', 'STORAGE', 1, 1, 10),
 ('de300000-0000-4000-8000-000000000506', 'B-02-01', 'B 区 02 排 01 位', 'de300000-0000-4000-8000-000000000402', 'STORAGE', 0, 1, 20)
ON CONFLICT (id) DO NOTHING;

-- ===========================================================================
-- ③ 采购链：PO-1（已完成→收货→应付→付款）/ PO-2（审批中）/ PO-3（已完成→收货）
-- ===========================================================================
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, expect_date, warehouse_id,
        status, currency_code, total_qty, total_amount, total_tax, total_with_tax, credit_days,
        remark, price_mode, purchase_type, supplier_name_snapshot, warehouse_name_snapshot,
        over_credit_limit, credit_exposure, credit_limit_snapshot,
        submitted_at, submitted_by, approved_at, approved_by, create_by)
VALUES
 ('de300000-0000-4000-8000-000000001001',
  'PO-' || to_char(pg_temp.d(14), 'YYYYMMDD') || '-900001',
  'de300000-0000-4000-8000-000000000101', pg_temp.d(14), pg_temp.d(7),
  'de300000-0000-4000-8000-000000000401', 'RECEIVED', 'CNY',
  150, 2000, 260, 2260, 30, '年度框架下的结构件补货',
  'EXCL', 'SPOT', '苏州精工机械有限公司', '苏州主仓', false, 2260, 200000,
  pg_temp.ts(14) + interval '30 minutes', pg_temp.uid('zhang.ming'),
  pg_temp.ts(14) + interval '2 hours', pg_temp.uid('admin'),
  pg_temp.uid('zhang.ming')),
 ('de300000-0000-4000-8000-000000001002',
  'PO-' || to_char(pg_temp.d(3), 'YYYYMMDD') || '-900002',
  'de300000-0000-4000-8000-000000000102', pg_temp.d(3), pg_temp.d(-4),
  'de300000-0000-4000-8000-000000000401', 'SUBMITTED', 'CNY',
  700, 15000, 1950, 16950, 45, '主板与线缆季度备货（等待采购经理审批）',
  'EXCL', 'SPOT', '东莞恒信电子科技有限公司', '苏州主仓', false, 16950, 150000,
  pg_temp.h(26), pg_temp.uid('zhang.ming'), NULL, NULL,
  pg_temp.uid('zhang.ming')),
 ('de300000-0000-4000-8000-000000001003',
  'PO-' || to_char(pg_temp.d(20), 'YYYYMMDD') || '-900003',
  'de300000-0000-4000-8000-000000000103', pg_temp.d(20), pg_temp.d(13),
  'de300000-0000-4000-8000-000000000402', 'RECEIVED', 'CNY',
  1000, 3200, 416, 3616, 30, '纸箱包装年度采购',
  'EXCL', 'SPOT', '宁波华联包装材料厂', '合肥分仓', false, 3616, 50000,
  pg_temp.ts(20) + interval '40 minutes', pg_temp.uid('zhang.ming'),
  pg_temp.ts(20) + interval '3 hours', pg_temp.uid('admin'),
  pg_temp.uid('zhang.ming'))
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_purchase_order_item (id, order_id, line_no, material_id, unit_id, qty, price,
                                     price_incl, tax_rate, amount, tax_amount, with_tax_amount, remark)
VALUES
 ('de300000-0000-4000-8000-000000001011', 'de300000-0000-4000-8000-000000001001', 1,
  'de300000-0000-4000-8000-000000000301', pg_temp.unit('PCS'), 100, 10.00, 11.30, 13, 1000, 130, 1130, ''),
 ('de300000-0000-4000-8000-000000001012', 'de300000-0000-4000-8000-000000001001', 2,
  'de300000-0000-4000-8000-000000000302', pg_temp.unit('PCS'), 50, 20.00, 22.60, 13, 1000, 130, 1130, ''),
 ('de300000-0000-4000-8000-000000001021', 'de300000-0000-4000-8000-000000001002', 1,
  'de300000-0000-4000-8000-000000000304', pg_temp.unit('PCS'), 200, 45.00, 50.85, 13, 9000, 1170, 10170, ''),
 ('de300000-0000-4000-8000-000000001022', 'de300000-0000-4000-8000-000000001002', 2,
  'de300000-0000-4000-8000-000000000306', pg_temp.unit('M'), 500, 12.00, 13.56, 13, 6000, 780, 6780, ''),
 ('de300000-0000-4000-8000-000000001031', 'de300000-0000-4000-8000-000000001003', 1,
  'de300000-0000-4000-8000-000000000309', pg_temp.unit('PCS'), 1000, 3.20, 3.616, 13, 3200, 416, 3616, '')
ON CONFLICT (id) DO NOTHING;

-- 收货入库（3 张，全部已过账；发货/收货/应付/凭证/流水由下面的段落配对）
INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date,
        status, currency_code, total_qty, total_amount, total_tax, total_with_tax,
        invoice_received, remark, is_sourceless, posted_at, posted_by, create_by)
VALUES
 ('de300000-0000-4000-8000-000000001101',
  'IN-' || to_char(pg_temp.d(13), 'YYYYMMDD') || '-900001', 'PURCHASE',
  'de300000-0000-4000-8000-000000001001', 'de300000-0000-4000-8000-000000000101',
  'de300000-0000-4000-8000-000000000401', pg_temp.d(13), 'POSTED', 'CNY',
  150, 2000, 260, 2260, true, '采购订单 PO 到货验收合格', false,
  pg_temp.ts(13) + interval '1 hour', pg_temp.uid('zhang.ming'), pg_temp.uid('zhang.ming')),
 ('de300000-0000-4000-8000-000000001102',
  'IN-' || to_char(pg_temp.d(12), 'YYYYMMDD') || '-900002', 'SOURCELESS',
  NULL, 'de300000-0000-4000-8000-000000000101',
  'de300000-0000-4000-8000-000000000401', pg_temp.d(12), 'POSTED', 'CNY',
  40, 1000, 130, 1130, true, '车间退库润滑油（无源入库），补登应付', true,
  pg_temp.ts(12) + interval '2 hours', pg_temp.uid('zhang.ming'), pg_temp.uid('zhang.ming')),
 ('de300000-0000-4000-8000-000000001103',
  'IN-' || to_char(pg_temp.d(18), 'YYYYMMDD') || '-900003', 'PURCHASE',
  'de300000-0000-4000-8000-000000001003', 'de300000-0000-4000-8000-000000000103',
  'de300000-0000-4000-8000-000000000402', pg_temp.d(18), 'POSTED', 'CNY',
  1000, 3200, 416, 3616, true, '纸箱到货，入合肥分仓', false,
  pg_temp.ts(18) + interval '1 hour', pg_temp.uid('zhang.ming'), pg_temp.uid('zhang.ming'))
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_stock_in_item (id, stock_in_id, line_no, source_item_id, material_id, unit_id,
        qty, price, price_incl, tax_rate, amount, tax_amount, with_tax_amount, cost_price, lot_no, remark)
VALUES
 ('de300000-0000-4000-8000-000000001111', 'de300000-0000-4000-8000-000000001101', 1,
  'de300000-0000-4000-8000-000000001011', 'de300000-0000-4000-8000-000000000301', pg_temp.unit('PCS'),
  100, 10.00, 11.30, 13, 1000, 130, 1130, 10.00, NULL, ''),
 ('de300000-0000-4000-8000-000000001112', 'de300000-0000-4000-8000-000000001101', 2,
  'de300000-0000-4000-8000-000000001012', 'de300000-0000-4000-8000-000000000302', pg_temp.unit('PCS'),
  50, 20.00, 22.60, 13, 1000, 130, 1130, 20.00, NULL, ''),
 ('de300000-0000-4000-8000-000000001121', 'de300000-0000-4000-8000-000000001102', 1,
  NULL, 'de300000-0000-4000-8000-000000000303', pg_temp.unit('L'),
  40, 25.00, 28.25, 13, 1000, 130, 1130, 25.00, 'LUB-20261015-A', '批号 LUB-20261015-A'),
 ('de300000-0000-4000-8000-000000001131', 'de300000-0000-4000-8000-000000001103', 1,
  'de300000-0000-4000-8000-000000001031', 'de300000-0000-4000-8000-000000000309', pg_temp.unit('PCS'),
  1000, 3.20, 3.616, 13, 3200, 416, 3616, 3.20, 'BOX-20261022', '批号 BOX-20261022')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_payable (id, payable_no, supplier_id, source_type, source_id, source_no,
        invoice_no, invoice_date, currency_code, amount, paid_amount, unpaid_amount, credit_days,
        due_date, period_code, status, settled_at, remark, create_by)
VALUES
 ('de300000-0000-4000-8000-000000001201',
  'AP-' || to_char(pg_temp.d(13), 'YYYYMMDD') || '-900001',
  'de300000-0000-4000-8000-000000000101', 'STOCK_IN', 'de300000-0000-4000-8000-000000001101',
  'IN-' || to_char(pg_temp.d(13), 'YYYYMMDD') || '-900001',
  'FP-2026-0912', pg_temp.d(13), 'CNY', 2260, 2260, 0, 30,
  pg_temp.d(13) + 30, to_char(pg_temp.d(13), 'YYYY-MM'), 'SETTLED',
  pg_temp.ts(9), '结构件补货货款（已付清）', pg_temp.uid('admin')),
 ('de300000-0000-4000-8000-000000001202',
  'AP-' || to_char(pg_temp.d(12), 'YYYYMMDD') || '-900002',
  'de300000-0000-4000-8000-000000000101', 'STOCK_IN', 'de300000-0000-4000-8000-000000001102',
  'IN-' || to_char(pg_temp.d(12), 'YYYYMMDD') || '-900002',
  'FP-2026-0918', pg_temp.d(12), 'CNY', 1130, 0, 1130, 30,
  pg_temp.d(12) + 30, to_char(pg_temp.d(12), 'YYYY-MM'), 'OPEN',
  NULL, '车间退库补登应付（账期内）', pg_temp.uid('admin')),
 ('de300000-0000-4000-8000-000000001203',
  'AP-' || to_char(pg_temp.d(18), 'YYYYMMDD') || '-900003',
  'de300000-0000-4000-8000-000000000103', 'STOCK_IN', 'de300000-0000-4000-8000-000000001103',
  'IN-' || to_char(pg_temp.d(18), 'YYYYMMDD') || '-900003',
  'FP-2026-0887', pg_temp.d(18), 'CNY', 3616, 0, 3616, 30,
  pg_temp.d(18) + 30, to_char(pg_temp.d(18), 'YYYY-MM'), 'OPEN',
  NULL, '包材货款（待付）', pg_temp.uid('admin'))
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_payment (id, payment_no, payable_id, amount, pay_date, pay_method, remark,
                         approval_status, create_by)
VALUES
 ('de300000-0000-4000-8000-000000001301',
  'PM-' || to_char(pg_temp.d(9), 'YYYYMMDD') || '-900001',
  'de300000-0000-4000-8000-000000001201', 2260, pg_temp.d(9), 'BANK_TRANSFER',
  '电汇付款（中国银行 4550***9012）', 'APPROVED', pg_temp.uid('admin'))
ON CONFLICT (id) DO NOTHING;

-- ===========================================================================
-- ④ 销售链：SO-1（已完成→发货→应收→收款）/ SO-2（待审批）
-- ===========================================================================
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, delivery_date, status,
        total_amount, total_tax, total_with_tax, currency_code, exchange_rate,
        base_total_amount, base_total_tax, base_total_with_tax, salesperson_id, remark, create_by)
VALUES
 ('de300000-0000-4000-8000-000000001401',
  'SO-' || to_char(pg_temp.d(11), 'YYYYMMDD') || '-900001',
  'de300000-0000-4000-8000-000000000201', pg_temp.d(11), pg_temp.d(10), 'OUTBOUND',
  3000, 390, 3390, 'CNY', 1, 3000, 390, 3390, pg_temp.uid('li.na'),
  '远洋贸易 11 月补单（已发货并收款）', pg_temp.uid('li.na')),
 ('de300000-0000-4000-8000-000000001402',
  'SO-' || to_char(pg_temp.d(1), 'YYYYMMDD') || '-900002',
  'de300000-0000-4000-8000-000000000202', pg_temp.d(1), pg_temp.d(-6), 'SUBMITTED',
  23000, 2990, 25990, 'CNY', 1, 23000, 2990, 25990, pg_temp.uid('li.na'),
  '天目电子新线体配套（等待销售主管审批）', pg_temp.uid('li.na'))
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_sales_order_item (id, so_id, line_no, material_id, qty, price, amount,
                                  tax_rate, tax_amount, delivered_qty)
VALUES
 ('de300000-0000-4000-8000-000000001411', 'de300000-0000-4000-8000-000000001401', 1,
  'de300000-0000-4000-8000-000000000301', 60, 30.00, 1800, 13, 234, 60),
 ('de300000-0000-4000-8000-000000001412', 'de300000-0000-4000-8000-000000001401', 2,
  'de300000-0000-4000-8000-000000000303', 10, 120.00, 1200, 13, 156, 10),
 ('de300000-0000-4000-8000-000000001421', 'de300000-0000-4000-8000-000000001402', 1,
  'de300000-0000-4000-8000-000000000307', 20, 400.00, 8000, 13, 1040, 0),
 ('de300000-0000-4000-8000-000000001422', 'de300000-0000-4000-8000-000000001402', 2,
  'de300000-0000-4000-8000-000000000308', 5, 3000.00, 15000, 13, 1950, 0)
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status,
                          total_qty, remark, create_by)
VALUES
 ('de300000-0000-4000-8000-000000001501',
  'SH-' || to_char(pg_temp.d(10), 'YYYYMMDD') || '-900001',
  'de300000-0000-4000-8000-000000001401', 'de300000-0000-4000-8000-000000000201',
  'de300000-0000-4000-8000-000000000401', pg_temp.d(10), 'POSTED', 70,
  '整车发运（德邦物流 沪A·8823）', pg_temp.uid('li.na'))
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_shipment_item (id, shipment_id, so_item_id, material_id, qty, remark)
VALUES
 ('de300000-0000-4000-8000-000000001511', 'de300000-0000-4000-8000-000000001501',
  'de300000-0000-4000-8000-000000001411', 'de300000-0000-4000-8000-000000000301', 60, ''),
 ('de300000-0000-4000-8000-000000001512', 'de300000-0000-4000-8000-000000001501',
  'de300000-0000-4000-8000-000000001412', 'de300000-0000-4000-8000-000000000303', 10, '出库批号 LUB-20261015-A')
ON CONFLICT (id) DO NOTHING;

-- 销售出库单：out_type=OTHER（销售性质）+ source_type=SALES，与 shipment_posting.rs 一致
INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, total_qty,
        total_amount, reason, source_type, source_id, posted_at, posted_by, create_by, remark)
VALUES
 ('de300000-0000-4000-8000-000000001601',
  'OUT-' || to_char(pg_temp.d(10), 'YYYYMMDD') || '-900001', 'OTHER',
  'de300000-0000-4000-8000-000000000401', pg_temp.d(10), 'POSTED', 70, 850,
  '销售发货', 'SALES', 'de300000-0000-4000-8000-000000001501',
  pg_temp.ts(10) + interval '2 hours', pg_temp.uid('li.na'), pg_temp.uid('li.na'),
  'SO 发货自动生成')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_stock_out_item (id, stock_out_id, line_no, material_id, unit_id, qty,
                                unit_cost, amount, lot_no, remark)
VALUES
 ('de300000-0000-4000-8000-000000001611', 'de300000-0000-4000-8000-000000001601', 1,
  'de300000-0000-4000-8000-000000000301', pg_temp.unit('PCS'), 60, 10.00, 600, NULL, ''),
 ('de300000-0000-4000-8000-000000001612', 'de300000-0000-4000-8000-000000001601', 2,
  'de300000-0000-4000-8000-000000000303', pg_temp.unit('L'), 10, 25.00, 250, 'LUB-20261015-A', '按批号出库')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no,
        invoice_no, invoice_date, currency_code, exchange_rate, amount, received_amount,
        unsettled_amount, credit_days, due_date, period_code, status, settled_at, remark, create_by)
VALUES
 ('de300000-0000-4000-8000-000000001701',
  'AR-' || to_char(pg_temp.d(10), 'YYYYMMDD') || '-900001',
  'de300000-0000-4000-8000-000000000201', 'SALES_OUT', 'de300000-0000-4000-8000-000000001601',
  'OUT-' || to_char(pg_temp.d(10), 'YYYYMMDD') || '-900001',
  'IV-2026-1042', pg_temp.d(10), 'CNY', 1, 3390, 3390, 0, 30,
  pg_temp.d(10) + 30, to_char(pg_temp.d(10), 'YYYY-MM'), 'SETTLED', pg_temp.ts(7),
  '已开票并全额收款', pg_temp.uid('li.na'))
ON CONFLICT (id) DO NOTHING;

-- 销售发票：应收「已开票」的那个号在这里才是权威来源（biz_receivable.invoice_no 只是拷贝列）。
-- 没有这一行，销售开票的收入凭证就没有可挂的 source_id（商品代码里 SOURCE_INVOICE 的
-- source_id = biz_invoice.id、source_no = invoice_no，见 erp/voucher.rs::create_for_sales_revenue）。
-- voucher_id 在下面「单据 ↔ 凭证」的指针段回填（与商品代码「过账时回填」同序）。
INSERT INTO biz_invoice (id, invoice_no, invoice_type, invoice_code, is_electronic, customer_id,
        invoice_date, amount, tax_amount, total_with_tax, tax_rate, tax_rate_mode, status,
        receivable_id, source_type, source_no, remark,
        currency_code, exchange_rate, base_total_amount, base_total_tax, base_total_with_tax,
        create_by)
VALUES
 ('de300000-0000-4000-8000-000000002301', 'IV-2026-1042', 'OUT', NULL, TRUE,
  'de300000-0000-4000-8000-000000000201', pg_temp.d(10), 3000, 390, 3390, 13, 'PERCENT', 'POSTED',
  'de300000-0000-4000-8000-000000001701', 'SALES_OUT',
  (SELECT out_no FROM biz_stock_out WHERE id = 'de300000-0000-4000-8000-000000001601'),
  '数电票（手填票号与应收的 invoice_no 一致）', 'CNY', 1, 3000, 390, 3390,
  pg_temp.uid('li.na'))
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_receipt (id, receipt_no, customer_id, receipt_date, amount, allocated_amount,
        method, bank_account, status, currency_code, exchange_rate,
        base_total_amount, base_total_tax, base_total_with_tax, remark, create_by)
VALUES
 ('de300000-0000-4000-8000-000000001801',
  'RC-' || to_char(pg_temp.d(7), 'YYYYMMDD') || '-900001',
  'de300000-0000-4000-8000-000000000201', pg_temp.d(7), 3390, 3390,
  'BANK_TRANSFER', '招商银行上海分行 6214****6688', 'POSTED', 'CNY', 1,
  3390, 0, 3390, '远洋贸易货款（全额）', pg_temp.uid('admin'))
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_receipt_allocation (id, receipt_id, receivable_id, amount, remark)
VALUES
 ('de300000-0000-4000-8000-000000001811', 'de300000-0000-4000-8000-000000001801',
  'de300000-0000-4000-8000-000000001701', 3390, '核销 AR 全额')
ON CONFLICT (id) DO NOTHING;

-- ===========================================================================
-- ⑤ 库存：调拨（在途）/ 盘点（草稿）/ 库存桶 / 库存流水
-- ===========================================================================
-- 调拨单：WH-91 → WH-92，货已发出在途（transit_warehouse = 迁移种好的 WH-TRANSIT）
INSERT INTO biz_stock_transfer (id, transfer_no, from_warehouse_id, to_warehouse_id,
        transit_warehouse_id, status, transfer_date, reason, applicant_id, applicant_name,
        approver_id, total_qty, total_amount, submitted_at, approved_at, shipped_at, remark)
SELECT 'de300000-0000-4000-8000-000000001901',
       'TK-' || to_char(pg_temp.d(2), 'YYYYMMDD') || '-900001',
       'de300000-0000-4000-8000-000000000401', 'de300000-0000-4000-8000-000000000402',
       w.id, 'IN_TRANSIT', pg_temp.d(2), '合肥分仓补货（螺丝）',
       pg_temp.uid('zhang.ming'), '张明', pg_temp.uid('admin'),
       10, 200, pg_temp.ts(2), pg_temp.ts(2) + interval '1 hour',
       pg_temp.ts(2) + interval '3 hours', '在途：货已发出，等分仓收货'
  FROM biz_warehouse w
 WHERE w.warehouse_code = 'WH-TRANSIT' AND w.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_stock_transfer_item (id, transfer_id, line_no, material_id, qty, shipped_qty,
                                     received_qty, unit_cost, amount, remark)
VALUES
 ('de300000-0000-4000-8000-000000001911', 'de300000-0000-4000-8000-000000001901', 1,
  'de300000-0000-4000-8000-000000000302', 10, 10, 0, 20.00, 200, '')
ON CONFLICT (id) DO NOTHING;

-- 盘点单：WH-91 部分盘点，草稿（演示“账实差异待录入”）
INSERT INTO biz_stock_take (id, take_no, warehouse_id, take_date, take_type, status,
        total_book_qty, total_actual_qty, total_gain_qty, total_loss_qty,
        total_gain_amount, total_loss_amount, remark, create_by)
VALUES
 ('de300000-0000-4000-8000-000000002001',
  'ST-' || to_char(now(), 'YYYYMMDD') || '-900001',
  'de300000-0000-4000-8000-000000000401', CURRENT_DATE, 'PARTIAL', 'DRAFT',
  70, 69, 0, 1, 0, 10, '11 月 A 区抽盘（待录入实盘数并过账）', pg_temp.uid('zhang.ming'))
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_stock_take_item (id, take_id, line_no, material_id, unit_id, book_qty,
                                 actual_qty, unit_cost, diff_amount, remark)
VALUES
 ('de300000-0000-4000-8000-000000002011', 'de300000-0000-4000-8000-000000002001', 1,
  'de300000-0000-4000-8000-000000000301', pg_temp.unit('PCS'), 40, 39, 10.00, 10, '搬运磕碰报废 1 件'),
 ('de300000-0000-4000-8000-000000002012', 'de300000-0000-4000-8000-000000002001', 2,
  'de300000-0000-4000-8000-000000000303', pg_temp.unit('L'), 30, 30, 25.00, 0, '')
ON CONFLICT (id) DO NOTHING;

-- 库存桶（biz_inventory 是「桶表」：location_id 指向 biz_inventory_location 的行）
INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, last_txn_at)
VALUES
 ('de300000-0000-4000-8000-000000000701', 'de300000-0000-4000-8000-000000000301',
  'de300000-0000-4000-8000-000000000401', 'de300000-0000-4000-8000-000000000501', 40, pg_temp.ts(10) + interval '2 hours'),
 ('de300000-0000-4000-8000-000000000702', 'de300000-0000-4000-8000-000000000302',
  'de300000-0000-4000-8000-000000000401', 'de300000-0000-4000-8000-000000000502', 40, pg_temp.ts(2) + interval '3 hours'),
 ('de300000-0000-4000-8000-000000000703', 'de300000-0000-4000-8000-000000000303',
  'de300000-0000-4000-8000-000000000401', 'de300000-0000-4000-8000-000000000503', 30, pg_temp.ts(10) + interval '2 hours'),
 ('de300000-0000-4000-8000-000000000704', 'de300000-0000-4000-8000-000000000309',
  'de300000-0000-4000-8000-000000000402', 'de300000-0000-4000-8000-000000000505', 1000, pg_temp.ts(18) + interval '1 hour'),
 ('de300000-0000-4000-8000-000000000705', 'de300000-0000-4000-8000-000000000302',
  (SELECT id FROM biz_warehouse WHERE warehouse_code = 'WH-TRANSIT' AND deleted_at IS NULL),
  NULL, 10, pg_temp.ts(2) + interval '3 hours')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty,
                           avg_cost, amount, last_txn_at)
VALUES
 ('de300000-0000-4000-8000-000000000801', 'de300000-0000-4000-8000-000000000301',
  'de300000-0000-4000-8000-000000000401', 'de300000-0000-4000-8000-000000000701',
  40, 0, 10.000000, 400.0000, pg_temp.ts(10) + interval '2 hours'),
 ('de300000-0000-4000-8000-000000000802', 'de300000-0000-4000-8000-000000000302',
  'de300000-0000-4000-8000-000000000401', 'de300000-0000-4000-8000-000000000702',
  40, 0, 20.000000, 800.0000, pg_temp.ts(2) + interval '3 hours'),
 ('de300000-0000-4000-8000-000000000803', 'de300000-0000-4000-8000-000000000303',
  'de300000-0000-4000-8000-000000000401', 'de300000-0000-4000-8000-000000000703',
  30, 0, 25.000000, 750.0000, pg_temp.ts(10) + interval '2 hours'),
 ('de300000-0000-4000-8000-000000000804', 'de300000-0000-4000-8000-000000000309',
  'de300000-0000-4000-8000-000000000402', 'de300000-0000-4000-8000-000000000704',
  1000, 0, 3.200000, 3200.0000, pg_temp.ts(18) + interval '1 hour'),
 ('de300000-0000-4000-8000-000000000805', 'de300000-0000-4000-8000-000000000302',
  (SELECT id FROM biz_warehouse WHERE warehouse_code = 'WH-TRANSIT' AND deleted_at IS NULL),
  'de300000-0000-4000-8000-000000000705',
  10, 0, 20.000000, 200.0000, pg_temp.ts(2) + interval '3 hours')
ON CONFLICT (id) DO NOTHING;

-- 库存流水：before/after 按 (物料, 仓库) 累计链，与上面的库存桶严格对得上
INSERT INTO biz_inventory_txn (id, material_id, warehouse_id, txn_type, biz_type, doc_type,
        doc_id, doc_no, line_no, qty, unit_cost, amount, before_qty, before_amount, before_cost,
        after_qty, after_amount, after_cost, period_code, operator_id, remark, created_at)
VALUES
 ('de300000-0000-4000-8000-000000000901', 'de300000-0000-4000-8000-000000000301',
  'de300000-0000-4000-8000-000000000401', 'IN', 'PURCHASE_IN', 'STOCK_IN',
  'de300000-0000-4000-8000-000000001101', 'IN-' || to_char(pg_temp.d(13), 'YYYYMMDD') || '-900001',
  1, 100, 10.000000, 1000, 0, 0, 0, 100, 1000, 10.000000,
  to_char(pg_temp.d(13), 'YYYY-MM'), pg_temp.uid('zhang.ming'), '',
  pg_temp.ts(13) + interval '1 hour'),
 ('de300000-0000-4000-8000-000000000902', 'de300000-0000-4000-8000-000000000302',
  'de300000-0000-4000-8000-000000000401', 'IN', 'PURCHASE_IN', 'STOCK_IN',
  'de300000-0000-4000-8000-000000001101', 'IN-' || to_char(pg_temp.d(13), 'YYYYMMDD') || '-900001',
  2, 50, 20.000000, 1000, 0, 0, 0, 50, 1000, 20.000000,
  to_char(pg_temp.d(13), 'YYYY-MM'), pg_temp.uid('zhang.ming'), '',
  pg_temp.ts(13) + interval '1 hour'),
 ('de300000-0000-4000-8000-000000000903', 'de300000-0000-4000-8000-000000000303',
  'de300000-0000-4000-8000-000000000401', 'IN', 'PURCHASE_IN', 'STOCK_IN',
  'de300000-0000-4000-8000-000000001102', 'IN-' || to_char(pg_temp.d(12), 'YYYYMMDD') || '-900002',
  1, 40, 25.000000, 1000, 0, 0, 0, 40, 1000, 25.000000,
  to_char(pg_temp.d(12), 'YYYY-MM'), pg_temp.uid('zhang.ming'), '无源入库',
  pg_temp.ts(12) + interval '2 hours'),
 ('de300000-0000-4000-8000-000000000904', 'de300000-0000-4000-8000-000000000309',
  'de300000-0000-4000-8000-000000000402', 'IN', 'PURCHASE_IN', 'STOCK_IN',
  'de300000-0000-4000-8000-000000001103', 'IN-' || to_char(pg_temp.d(18), 'YYYYMMDD') || '-900003',
  1, 1000, 3.200000, 3200, 0, 0, 0, 1000, 3200, 3.200000,
  to_char(pg_temp.d(18), 'YYYY-MM'), pg_temp.uid('zhang.ming'), '',
  pg_temp.ts(18) + interval '1 hour'),
 ('de300000-0000-4000-8000-000000000905', 'de300000-0000-4000-8000-000000000301',
  'de300000-0000-4000-8000-000000000401', 'OUT', 'SALE_OUT', 'STOCK_OUT',
  'de300000-0000-4000-8000-000000001601', 'OUT-' || to_char(pg_temp.d(10), 'YYYYMMDD') || '-900001',
  1, 60, 10.000000, 600, 100, 1000, 10.000000, 40, 400, 10.000000,
  to_char(pg_temp.d(10), 'YYYY-MM'), pg_temp.uid('li.na'), '销售出库',
  pg_temp.ts(10) + interval '2 hours'),
 ('de300000-0000-4000-8000-000000000906', 'de300000-0000-4000-8000-000000000303',
  'de300000-0000-4000-8000-000000000401', 'OUT', 'SALE_OUT', 'STOCK_OUT',
  'de300000-0000-4000-8000-000000001601', 'OUT-' || to_char(pg_temp.d(10), 'YYYYMMDD') || '-900001',
  2, 10, 25.000000, 250, 40, 1000, 25.000000, 30, 750, 25.000000,
  to_char(pg_temp.d(10), 'YYYY-MM'), pg_temp.uid('li.na'), '销售出库',
  pg_temp.ts(10) + interval '2 hours'),
 ('de300000-0000-4000-8000-000000000907', 'de300000-0000-4000-8000-000000000302',
  'de300000-0000-4000-8000-000000000401', 'OUT', 'TRANSFER_OUT', 'STOCK_TRANSFER',
  'de300000-0000-4000-8000-000000001901', 'TK-' || to_char(pg_temp.d(2), 'YYYYMMDD') || '-900001',
  1, 10, 20.000000, 200, 50, 1000, 20.000000, 40, 800, 20.000000,
  to_char(pg_temp.d(2), 'YYYY-MM'), pg_temp.uid('zhang.ming'), '调拨发出',
  pg_temp.ts(2) + interval '3 hours'),
 ('de300000-0000-4000-8000-000000000908', 'de300000-0000-4000-8000-000000000302',
  (SELECT id FROM biz_warehouse WHERE warehouse_code = 'WH-TRANSIT' AND deleted_at IS NULL),
  'IN', 'TRANSFER_IN', 'STOCK_TRANSFER',
  'de300000-0000-4000-8000-000000001901', 'TK-' || to_char(pg_temp.d(2), 'YYYYMMDD') || '-900001',
  1, 10, 20.000000, 200, 0, 0, 0, 10, 200, 20.000000,
  to_char(pg_temp.d(2), 'YYYY-MM'), pg_temp.uid('zhang.ming'), '调拨在途入库',
  pg_temp.ts(2) + interval '3 hours')
ON CONFLICT (id) DO NOTHING;
SQL
        cat <<'SQL2'

-- ===========================================================================
-- ⑥ 财务：业务自动凭证 7 张 + 手工凭证 3 张 = 10 张
--   1405 借贷净额 = 库存模块金额；2202 借贷净额 = 应付未付 —— 由后面的自检兜底
-- ===========================================================================
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
        source_type, source_id, source_no, source_doc_no, summary,
        total_debit, total_credit, entry_count, status, is_red, posted_at, posted_by,
        remark, create_by)
VALUES
 -- 采购入库（3 张）
 ('de300000-0000-4000-8000-000000003001', '记',
  'PZ-' || to_char(pg_temp.d(13), 'YYYYMMDD') || '-900001', to_char(pg_temp.d(13), 'YYYY-MM'),
  pg_temp.d(13), 'AUTO', 'STOCK_IN', 'de300000-0000-4000-8000-000000001101',
  (SELECT in_no FROM biz_stock_in WHERE id = 'de300000-0000-4000-8000-000000001101'),
  (SELECT in_no FROM biz_stock_in WHERE id = 'de300000-0000-4000-8000-000000001101'),
  '采购入库（结构件 2 项，含进项税）', 2260, 2260, 4, 'POSTED', 0,
  pg_temp.ts(13) + interval '1 hour', pg_temp.uid('admin'), '', pg_temp.uid('admin')),
 ('de300000-0000-4000-8000-000000003002', '记',
  'PZ-' || to_char(pg_temp.d(12), 'YYYYMMDD') || '-900002', to_char(pg_temp.d(12), 'YYYY-MM'),
  pg_temp.d(12), 'AUTO', 'STOCK_IN', 'de300000-0000-4000-8000-000000001102',
  (SELECT in_no FROM biz_stock_in WHERE id = 'de300000-0000-4000-8000-000000001102'),
  (SELECT in_no FROM biz_stock_in WHERE id = 'de300000-0000-4000-8000-000000001102'),
  '无源入库（车间退库润滑油）', 1130, 1130, 3, 'POSTED', 0,
  pg_temp.ts(12) + interval '2 hours', pg_temp.uid('admin'), '', pg_temp.uid('admin')),
 ('de300000-0000-4000-8000-000000003003', '记',
  'PZ-' || to_char(pg_temp.d(18), 'YYYYMMDD') || '-900003', to_char(pg_temp.d(18), 'YYYY-MM'),
  pg_temp.d(18), 'AUTO', 'STOCK_IN', 'de300000-0000-4000-8000-000000001103',
  (SELECT in_no FROM biz_stock_in WHERE id = 'de300000-0000-4000-8000-000000001103'),
  (SELECT in_no FROM biz_stock_in WHERE id = 'de300000-0000-4000-8000-000000001103'),
  '采购入库（瓦楞纸箱 1000 只）', 3616, 3616, 3, 'POSTED', 0,
  pg_temp.ts(18) + interval '1 hour', pg_temp.uid('admin'), '', pg_temp.uid('admin')),
 -- 付款
 ('de300000-0000-4000-8000-000000003004', '记',
  'PZ-' || to_char(pg_temp.d(9), 'YYYYMMDD') || '-900004', to_char(pg_temp.d(9), 'YYYY-MM'),
  pg_temp.d(9), 'AUTO', 'PAYMENT', 'de300000-0000-4000-8000-000000001301',
  (SELECT payment_no FROM biz_payment WHERE id = 'de300000-0000-4000-8000-000000001301'),
  (SELECT payment_no FROM biz_payment WHERE id = 'de300000-0000-4000-8000-000000001301'),
  '银行付款（冲减应付账款）', 2260, 2260, 2, 'POSTED', 0,
  pg_temp.ts(9), pg_temp.uid('admin'), '', pg_temp.uid('admin')),
 -- 销售出库成本结转
 ('de300000-0000-4000-8000-000000003005', '记',
  'PZ-' || to_char(pg_temp.d(10), 'YYYYMMDD') || '-900005', to_char(pg_temp.d(10), 'YYYY-MM'),
  pg_temp.d(10), 'AUTO', 'STOCK_OUT', 'de300000-0000-4000-8000-000000001601',
  (SELECT out_no FROM biz_stock_out WHERE id = 'de300000-0000-4000-8000-000000001601'),
  (SELECT out_no FROM biz_stock_out WHERE id = 'de300000-0000-4000-8000-000000001601'),
  '销售出库成本结转（2 项）', 850, 850, 4, 'POSTED', 0,
  pg_temp.ts(10) + interval '2 hours', pg_temp.uid('admin'), '', pg_temp.uid('admin')),
 -- 开票确认收入（SO-22 口径：收入在**开票**时确认，来源是销售发票 biz_invoice.id）
 --   ⚠ source_type 只能是 VoucherSourceType 枚举值（'INVOICE'）；'RECEIVABLE' 是业务对象轴
 --     （DocKind::Receivable）的取值，不在凭证来源轴上，没有任何生产路径会写它。
 --     挂账（出库转应收）只是债权成立、不是收入确认时点，所以收入凭证不挂应收单。
 ('de300000-0000-4000-8000-000000003006', '记',
  'PZ-' || to_char(pg_temp.d(10), 'YYYYMMDD') || '-900006', to_char(pg_temp.d(10), 'YYYY-MM'),
  pg_temp.d(10), 'AUTO', 'INVOICE', 'de300000-0000-4000-8000-000000002301',
  (SELECT invoice_no FROM biz_invoice WHERE id = 'de300000-0000-4000-8000-000000002301'),
  NULL,
  '销售开票（确认收入与销项税）', 3390, 3390, 3, 'POSTED', 0,
  pg_temp.ts(10) + interval '4 hours', pg_temp.uid('admin'), '', pg_temp.uid('admin')),
 -- 收款
 ('de300000-0000-4000-8000-000000003007', '记',
  'PZ-' || to_char(pg_temp.d(7), 'YYYYMMDD') || '-900007', to_char(pg_temp.d(7), 'YYYY-MM'),
  pg_temp.d(7), 'AUTO', 'RECEIPT', 'de300000-0000-4000-8000-000000001801',
  (SELECT receipt_no FROM biz_receipt WHERE id = 'de300000-0000-4000-8000-000000001801'),
  (SELECT receipt_no FROM biz_receipt WHERE id = 'de300000-0000-4000-8000-000000001801'),
  '客户回款（冲减应收账款）', 3390, 3390, 2, 'POSTED', 0,
  pg_temp.ts(7), pg_temp.uid('admin'), '', pg_temp.uid('admin')),
 -- 手工凭证 3 张
 ('de300000-0000-4000-8000-000000003008', '记',
  'PZ-' || to_char(pg_temp.d(6), 'YYYYMMDD') || '-900008', to_char(pg_temp.d(6), 'YYYY-MM'),
  pg_temp.d(6), 'MANUAL', 'MANUAL', NULL, '手工凭证 900008', NULL,
  '从银行提取备用金', 5000, 5000, 2, 'POSTED', 0,
  pg_temp.ts(6), pg_temp.uid('admin'), '手工凭证', pg_temp.uid('admin')),
 ('de300000-0000-4000-8000-000000003009', '记',
  'PZ-' || to_char(pg_temp.d(5), 'YYYYMMDD') || '-900009', to_char(pg_temp.d(5), 'YYYY-MM'),
  pg_temp.d(5), 'MANUAL', 'MANUAL', NULL, '手工凭证 900009', NULL,
  '计提本月职工薪酬', 120000, 120000, 3, 'POSTED', 0,
  pg_temp.ts(5), pg_temp.uid('admin'), '手工凭证', pg_temp.uid('admin')),
 ('de300000-0000-4000-8000-000000003010', '记',
  'PZ-' || to_char(pg_temp.d(4), 'YYYYMMDD') || '-900010', to_char(pg_temp.d(4), 'YYYY-MM'),
  pg_temp.d(4), 'MANUAL', 'MANUAL', NULL, '手工凭证 900010', NULL,
  '支付本月水电费（办公区）', 8600, 8600, 2, 'POSTED', 0,
  pg_temp.ts(4), pg_temp.uid('admin'), '手工凭证', pg_temp.uid('admin'))
ON CONFLICT (id) DO NOTHING;

-- 分录取科目 id / 编码 / 名称一律回库查（不手抄科目名，科目表改了也不会对不上）
INSERT INTO biz_voucher_entry (id, voucher_id, line_no, summary, account_id, account_code,
        account_name, debit_amount, credit_amount, aux_supplier_id, aux_material_id,
        aux_warehouse_id, aux_qty)
SELECT e.id::uuid, e.voucher_id::uuid, e.line_no, e.summary, a.id, a.account_code, a.account_name,
       e.debit, e.credit, e.sup::uuid, e.mat::uuid, e.wh::uuid, e.qty
  FROM (VALUES
   -- 采购入库 1（2260）
   ('de300000-0000-4000-8000-000000310101','de300000-0000-4000-8000-000000003001',1,'采购入库：精密铝合金外壳 100 件','1405','de300000-0000-4000-8000-000000000101',NULL,NULL,1000::numeric,0::numeric,100::numeric),
   ('de300000-0000-4000-8000-000000310102','de300000-0000-4000-8000-000000003001',2,'采购入库：不锈钢内六角螺丝 50 件','1405','de300000-0000-4000-8000-000000000101',NULL,NULL,1000,0,50),
   ('de300000-0000-4000-8000-000000310103','de300000-0000-4000-8000-000000003001',3,'采购入库：进项税额','2221.01','de300000-0000-4000-8000-000000000101',NULL,NULL,260,0,NULL),
   ('de300000-0000-4000-8000-000000310104','de300000-0000-4000-8000-000000003001',4,'采购入库：应付苏州精工','2202','de300000-0000-4000-8000-000000000101',NULL,NULL,0,2260,NULL),
   -- 无源入库（1130）
   ('de300000-0000-4000-8000-000000310201','de300000-0000-4000-8000-000000003002',1,'无源入库：工业润滑油 40 升','1405','de300000-0000-4000-8000-000000000101',NULL,NULL,1000,0,40),
   ('de300000-0000-4000-8000-000000310202','de300000-0000-4000-8000-000000003002',2,'无源入库：进项税额','2221.01','de300000-0000-4000-8000-000000000101',NULL,NULL,130,0,NULL),
   ('de300000-0000-4000-8000-000000310203','de300000-0000-4000-8000-000000003002',3,'无源入库：应付苏州精工','2202','de300000-0000-4000-8000-000000000101',NULL,NULL,0,1130,NULL),
   -- 采购入库 2（3616）
   ('de300000-0000-4000-8000-000000310301','de300000-0000-4000-8000-000000003003',1,'采购入库：瓦楞纸箱 1000 只','1405','de300000-0000-4000-8000-000000000103',NULL,'de300000-0000-4000-8000-000000000402',3200,0,1000),
   ('de300000-0000-4000-8000-000000310302','de300000-0000-4000-8000-000000003003',2,'采购入库：进项税额','2221.01','de300000-0000-4000-8000-000000000103',NULL,NULL,416,0,NULL),
   ('de300000-0000-4000-8000-000000310303','de300000-0000-4000-8000-000000003003',3,'采购入库：应付宁波华联','2202','de300000-0000-4000-8000-000000000103',NULL,NULL,0,3616,NULL),
   -- 付款（2260）
   ('de300000-0000-4000-8000-000000310401','de300000-0000-4000-8000-000000003004',1,'银行付款：冲减应付苏州精工','2202','de300000-0000-4000-8000-000000000101',NULL,NULL,2260,0,NULL),
   ('de300000-0000-4000-8000-000000310402','de300000-0000-4000-8000-000000003004',2,'银行付款：中国银行 4550***9012','1002',NULL,NULL,NULL,0,2260,NULL),
   -- 销售出库成本（850）
   ('de300000-0000-4000-8000-000000310501','de300000-0000-4000-8000-000000003005',1,'销售出库成本：精密铝合金外壳 60 件','6401',NULL,'de300000-0000-4000-8000-000000000301',NULL,600,0,60),
   ('de300000-0000-4000-8000-000000310502','de300000-0000-4000-8000-000000003005',2,'销售出库成本：结转存货（外壳）','1405',NULL,'de300000-0000-4000-8000-000000000301',NULL,0,600,NULL),
   ('de300000-0000-4000-8000-000000310503','de300000-0000-4000-8000-000000003005',3,'销售出库成本：工业润滑油 10 升','6401',NULL,'de300000-0000-4000-8000-000000000303',NULL,250,0,10),
   ('de300000-0000-4000-8000-000000310504','de300000-0000-4000-8000-000000003005',4,'销售出库成本：结转存货（润滑油）','1405',NULL,'de300000-0000-4000-8000-000000000303',NULL,0,250,NULL),
   -- 应收（3390）
   ('de300000-0000-4000-8000-000000310601','de300000-0000-4000-8000-000000003006',1,'开票：确认应收账款（上海远洋）','1122',NULL,NULL,NULL,3390,0,NULL),
   ('de300000-0000-4000-8000-000000310602','de300000-0000-4000-8000-000000003006',2,'开票：确认主营业务收入','5001',NULL,NULL,NULL,0,3000,NULL),
   ('de300000-0000-4000-8000-000000310603','de300000-0000-4000-8000-000000003006',3,'开票：销项税额 13%','2221.02',NULL,NULL,NULL,0,390,NULL),
   -- 收款（3390）
   ('de300000-0000-4000-8000-000000310701','de300000-0000-4000-8000-000000003007',1,'客户回款：招商银行 6214***6688','1002',NULL,NULL,NULL,3390,0,NULL),
   ('de300000-0000-4000-8000-000000310702','de300000-0000-4000-8000-000000003007',2,'客户回款：冲减应收账款','1122',NULL,NULL,NULL,0,3390,NULL),
   -- 手工凭证 1：提现（5000）
   ('de300000-0000-4000-8000-000000310801','de300000-0000-4000-8000-000000003008',1,'提现：库存现金','1001',NULL,NULL,NULL,5000,0,NULL),
   ('de300000-0000-4000-8000-000000310802','de300000-0000-4000-8000-000000003008',2,'提现：银行存款','1002',NULL,NULL,NULL,0,5000,NULL),
   -- 手工凭证 2：计提工资（120000）
   ('de300000-0000-4000-8000-000000310901','de300000-0000-4000-8000-000000003009',1,'计提工资：管理费用','5602.09',NULL,NULL,NULL,80000,0,NULL),
   ('de300000-0000-4000-8000-000000310902','de300000-0000-4000-8000-000000003009',2,'计提工资：销售费用','5601',NULL,NULL,NULL,40000,0,NULL),
   ('de300000-0000-4000-8000-000000310903','de300000-0000-4000-8000-000000003009',3,'计提工资：应付职工薪酬','2211',NULL,NULL,NULL,0,120000,NULL),
   -- 手工凭证 3：支付水电费（8600）—— 贷方走银行，不碰 2241.01（那本账要和报销单未付额对得上）
   ('de300000-0000-4000-8000-000000311001','de300000-0000-4000-8000-000000003010',1,'支付水电费：管理费用-办公费','5602.03',NULL,NULL,NULL,8600,0,NULL),
   ('de300000-0000-4000-8000-000000311002','de300000-0000-4000-8000-000000003010',2,'支付水电费：银行存款','1002',NULL,NULL,NULL,0,8600,NULL)
  ) AS e(id, voucher_id, line_no, summary, acct, sup, mat, wh, debit, credit, qty)
  JOIN biz_account a ON a.account_code = e.acct AND a.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- 单据 ↔ 凭证 / 应付 / 出库 的互相指针（幂等 UPDATE，重复跑结果一样）
UPDATE biz_stock_in s
   SET voucher_id = (SELECT v.id FROM biz_voucher v WHERE v.source_type = 'STOCK_IN' AND v.source_id = s.id AND v.deleted_at IS NULL LIMIT 1),
       payable_id = (SELECT p.id FROM biz_payable p WHERE p.source_type = 'STOCK_IN' AND p.source_id = s.id AND p.deleted_at IS NULL LIMIT 1)
 WHERE s.id::text LIKE 'de300000-%';
UPDATE biz_payable p
   SET voucher_id = (SELECT v.id FROM biz_voucher v WHERE v.source_type = 'STOCK_IN' AND v.source_id = p.source_id AND v.deleted_at IS NULL LIMIT 1)
 WHERE p.id::text LIKE 'de300000-%';
UPDATE biz_payment y
   SET voucher_id = (SELECT v.id FROM biz_voucher v WHERE v.source_type = 'PAYMENT' AND v.source_id = y.id AND v.deleted_at IS NULL LIMIT 1)
 WHERE y.id::text LIKE 'de300000-%';
UPDATE biz_stock_out o
   SET voucher_id = (SELECT v.id FROM biz_voucher v WHERE v.source_type = 'STOCK_OUT' AND v.source_id = o.id AND v.deleted_at IS NULL LIMIT 1)
 WHERE o.id::text LIKE 'de300000-%';
UPDATE biz_invoice i
   SET voucher_id = (SELECT v.id FROM biz_voucher v WHERE v.source_type = 'INVOICE' AND v.source_id = i.id AND v.deleted_at IS NULL LIMIT 1)
 WHERE i.id::text LIKE 'de300000-%';
-- 应收的凭证指针由**发票**回填（不是自己去当凭证来源）：与商品代码 invoice::writeback_receivable 同口径。
-- 反查凭证一律走 source_type='INVOICE' + source_id=发票 id，「应收 → 发票 → 凭证」两跳。
UPDATE biz_receivable r
   SET voucher_id = (SELECT i.voucher_id FROM biz_invoice i
                      WHERE i.receivable_id = r.id AND i.deleted_at IS NULL AND i.voucher_id IS NOT NULL
                      LIMIT 1)
 WHERE r.id::text LIKE 'de300000-%';
UPDATE biz_receipt r
   SET voucher_id = (SELECT v.id FROM biz_voucher v WHERE v.source_type = 'RECEIPT' AND v.source_id = r.id AND v.deleted_at IS NULL LIMIT 1)
 WHERE r.id::text LIKE 'de300000-%';
UPDATE biz_shipment s
   SET stock_out_id = (SELECT o.id FROM biz_stock_out o WHERE o.source_type = 'SALES' AND o.source_id = s.id AND o.deleted_at IS NULL LIMIT 1)
 WHERE s.id::text LIKE 'de300000-%';

-- ===========================================================================
-- ⑦ 流程实例（真实 biz_flow_instance / 节点 / 任务）
--    已完成 2 条（采购订单、销售订单）+ 待办 3 条（采购、销售、报销）
--    待办任务的审批人是 admin —— 登录就能在「待我审批」里看到并直接批
-- ===========================================================================
INSERT INTO biz_flow_instance (id, instance_no, def_id, version_id, version_no, flow_code,
        flow_name, biz_type, biz_id, biz_no, title, status, initiator_id, initiator_name,
        initiator_dept_id, form_data, round_total, started_at, ended_at, create_by)
SELECT 'de300000-0000-4000-8000-000000005001',
       'FLW-' || to_char(pg_temp.d(14), 'YYYYMMDD') || '-D90001',
       d.id, ver.id, ver.version_no, d.flow_code, d.flow_name, 'PO_APPROVE',
       o.id, o.order_no, o.order_no || ' 采购订单审批', 'APPROVED',
       u.id, u.real_name, u.dept_id,
       jsonb_build_object('order_no', o.order_no, 'amount', 2000, 'total_amount', 2000,
                          'total_with_tax', 2260, 'supplier_name', o.supplier_name_snapshot,
                          'warehouse_name', o.warehouse_name_snapshot, 'over_credit_limit', false),
       1, pg_temp.ts(14) + interval '30 minutes', pg_temp.ts(14) + interval '2 hours', u.id
  FROM biz_flow_def d
  JOIN biz_flow_def_version ver ON ver.def_id = d.id AND ver.is_current
  JOIN biz_purchase_order o ON o.id = 'de300000-0000-4000-8000-000000001001'
  JOIN sys_user u ON u.id = pg_temp.uid('zhang.ming')
 WHERE d.flow_code = 'po_approve' AND d.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_flow_instance (id, instance_no, def_id, version_id, version_no, flow_code,
        flow_name, biz_type, biz_id, biz_no, title, status, initiator_id, initiator_name,
        initiator_dept_id, form_data, round_total, started_at, ended_at, create_by)
SELECT 'de300000-0000-4000-8000-000000005002',
       'FLW-' || to_char(pg_temp.d(3), 'YYYYMMDD') || '-D90002',
       d.id, ver.id, ver.version_no, d.flow_code, d.flow_name, 'PO_APPROVE',
       o.id, o.order_no, o.order_no || ' 采购订单审批', 'PENDING',
       u.id, u.real_name, u.dept_id,
       jsonb_build_object('order_no', o.order_no, 'amount', 15000, 'total_amount', 15000,
                          'total_with_tax', 16950, 'supplier_name', o.supplier_name_snapshot,
                          'warehouse_name', o.warehouse_name_snapshot, 'over_credit_limit', false),
       1, pg_temp.h(26), NULL, u.id
  FROM biz_flow_def d
  JOIN biz_flow_def_version ver ON ver.def_id = d.id AND ver.is_current
  JOIN biz_purchase_order o ON o.id = 'de300000-0000-4000-8000-000000001002'
  JOIN sys_user u ON u.id = pg_temp.uid('zhang.ming')
 WHERE d.flow_code = 'po_approve' AND d.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_flow_instance (id, instance_no, def_id, version_id, version_no, flow_code,
        flow_name, biz_type, biz_id, biz_no, title, status, initiator_id, initiator_name,
        initiator_dept_id, form_data, round_total, started_at, ended_at, create_by)
SELECT 'de300000-0000-4000-8000-000000005003',
       'FLW-' || to_char(pg_temp.d(11), 'YYYYMMDD') || '-D90003',
       d.id, ver.id, ver.version_no, d.flow_code, d.flow_name, 'SO_APPROVE',
       o.id, o.so_no, o.so_no || ' 销售订单审批', 'APPROVED',
       u.id, u.real_name, u.dept_id,
       jsonb_build_object('so_no', o.so_no, 'amount', 3000, 'total_amount', 3000,
                          'total_with_tax', 3390),
       1, pg_temp.ts(11) + interval '20 minutes', pg_temp.ts(11) + interval '1 hour', u.id
  FROM biz_flow_def d
  JOIN biz_flow_def_version ver ON ver.def_id = d.id AND ver.is_current
  JOIN biz_sales_order o ON o.id = 'de300000-0000-4000-8000-000000001401'
  JOIN sys_user u ON u.id = pg_temp.uid('li.na')
 WHERE d.flow_code = 'so_approve' AND d.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_flow_instance (id, instance_no, def_id, version_id, version_no, flow_code,
        flow_name, biz_type, biz_id, biz_no, title, status, initiator_id, initiator_name,
        initiator_dept_id, form_data, round_total, started_at, ended_at, create_by)
SELECT 'de300000-0000-4000-8000-000000005004',
       'FLW-' || to_char(pg_temp.d(1), 'YYYYMMDD') || '-D90004',
       d.id, ver.id, ver.version_no, d.flow_code, d.flow_name, 'SO_APPROVE',
       o.id, o.so_no, o.so_no || ' 销售订单审批', 'PENDING',
       u.id, u.real_name, u.dept_id,
       jsonb_build_object('so_no', o.so_no, 'amount', 23000, 'total_amount', 23000,
                          'total_with_tax', 25990),
       1, pg_temp.h(20), NULL, u.id
  FROM biz_flow_def d
  JOIN biz_flow_def_version ver ON ver.def_id = d.id AND ver.is_current
  JOIN biz_sales_order o ON o.id = 'de300000-0000-4000-8000-000000001402'
  JOIN sys_user u ON u.id = pg_temp.uid('li.na')
 WHERE d.flow_code = 'so_approve' AND d.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- 报销单（费用）—— 待直属上级审批
INSERT INTO biz_expense_type (id, type_code, type_name, account_id, account_code,
                              tax_deductible, budget_controlled, need_invoice_no, sort_order, status, remark)
VALUES ('de300000-0000-4000-8000-000000002102', 'ET-DEMO-01', '差旅费（演示）',
        pg_temp.acct('5602.02'), '5602.02', 1, 0, 1, 90, 1, '演示费用类型')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_expense_report (id, report_no, reporter_id, reporter_name, dept_id, dept_name,
        title, expense_date, period_code, total_amount, total_tax, total_with_tax, item_count,
        status, approval_instance_id, submitted_at, submitted_by, pay_status, paid_amount,
        unpaid_amount, over_budget, missing_budget, remark)
VALUES
 ('de300000-0000-4000-8000-000000002101',
  'EXP-' || to_char(pg_temp.d(2), 'YYYYMMDD') || '-900001',
  pg_temp.uid('wang.lei'), '王磊', 'de300000-0000-4000-8000-000000000001', '演示事业部',
  '合肥分仓现场支持差旅报销', pg_temp.d(2), to_char(pg_temp.d(2), 'YYYY-MM'),
  800, 0, 800, 1, 'SUBMITTED', 'de300000-0000-4000-8000-000000005005',
  pg_temp.h(8), pg_temp.uid('wang.lei'),
  'UNPAID', 0, 800, 0, 0, '高铁往返（二等座）')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_expense_item (id, report_id, line_no, expense_date, expense_type_id, type_code,
        type_name, account_id, account_code, account_name, amount, tax_rate, tax_amount,
        with_tax_amount, invoice_no, invoice_date, invoice_type, summary, remark)
VALUES
 ('de300000-0000-4000-8000-000000002111', 'de300000-0000-4000-8000-000000002101', 1,
  pg_temp.d(2), 'de300000-0000-4000-8000-000000002102', 'ET-DEMO-01', '差旅费（演示）',
  pg_temp.acct('5602.02'), '5602.02', '管理费用-差旅费', 800, 0, 0, 800,
  'TRAVEL-202611-0091', pg_temp.d(2), 'VAT_NORMAL', '合肥现场支持差旅费', '')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_flow_instance (id, instance_no, def_id, version_id, version_no, flow_code,
        flow_name, biz_type, biz_id, biz_no, title, status, initiator_id, initiator_name,
        initiator_dept_id, form_data, round_total, started_at, ended_at, create_by)
SELECT 'de300000-0000-4000-8000-000000005005',
       'FLW-' || to_char(pg_temp.d(2), 'YYYYMMDD') || '-D90005',
       d.id, ver.id, ver.version_no, d.flow_code, d.flow_name, 'EXPENSE',
       e.id, e.report_no, e.report_no || ' 费用报销审批', 'PENDING',
       u.id, u.real_name, u.dept_id,
       jsonb_build_object('report_no', e.report_no, 'amount', 800, 'total_amount', 800,
                          'total_with_tax', 800, 'title', e.title),
       1, pg_temp.h(8), NULL, u.id
  FROM biz_flow_def d
  JOIN biz_flow_def_version ver ON ver.def_id = d.id AND ver.is_current
  JOIN biz_expense_report e ON e.id = 'de300000-0000-4000-8000-000000002101'
  JOIN sys_user u ON u.id = pg_temp.uid('wang.lei')
 WHERE d.flow_code = 'expense' AND d.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- 采购订单 PO-3 的审批实例（已完成采购也要有审批记录，否则详情页的审批轨迹是空的）
INSERT INTO biz_flow_instance (id, instance_no, def_id, version_id, version_no, flow_code,
        flow_name, biz_type, biz_id, biz_no, title, status, initiator_id, initiator_name,
        initiator_dept_id, form_data, round_total, started_at, ended_at, create_by)
SELECT 'de300000-0000-4000-8000-000000005006',
       'FLW-' || to_char(pg_temp.d(20), 'YYYYMMDD') || '-D90006',
       d.id, ver.id, ver.version_no, d.flow_code, d.flow_name, 'PO_APPROVE',
       o.id, o.order_no, o.order_no || ' 采购订单审批', 'APPROVED',
       u.id, u.real_name, u.dept_id,
       jsonb_build_object('order_no', o.order_no, 'amount', 3200, 'total_amount', 3200,
                          'total_with_tax', 3616, 'supplier_name', o.supplier_name_snapshot,
                          'warehouse_name', o.warehouse_name_snapshot, 'over_credit_limit', false),
       1, pg_temp.ts(20) + interval '40 minutes', pg_temp.ts(20) + interval '3 hours', u.id
  FROM biz_flow_def d
  JOIN biz_flow_def_version ver ON ver.def_id = d.id AND ver.is_current
  JOIN biz_purchase_order o ON o.id = 'de300000-0000-4000-8000-000000001003'
  JOIN sys_user u ON u.id = pg_temp.uid('zhang.ming')
 WHERE d.flow_code = 'po_approve' AND d.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_flow_instance_node (id, instance_id, node_key, node_name, node_type, round,
        status, approval_mode, reject_veto, approver_count, approved_count, rejected_count,
        taken_edge_key, activated_at, ended_at, duration_ms)
VALUES
 -- 节点 key 必须与流程定义图里的 key 一致，否则同意时引擎找不到下一跳
 -- FI-1 采购订单（已完成）：开始 → 额度判定 → 金额分级① → 部门主管 → 结束
 ('de300000-0000-4000-8000-000000005101', 'de300000-0000-4000-8000-000000005001', 's', '开始', 'START', 1, 'APPROVED', NULL, true, 0, 0, 0, NULL, pg_temp.ts(14) + interval '30 minutes', pg_temp.ts(14) + interval '30 minutes', 0),
 ('de300000-0000-4000-8000-000000005102', 'de300000-0000-4000-8000-000000005001', 'c_credit', '超信用额度判定', 'CONDITION', 1, 'APPROVED', NULL, true, 0, 0, 0, 'e_credit_else', pg_temp.ts(14) + interval '30 minutes', pg_temp.ts(14) + interval '30 minutes', 0),
 ('de300000-0000-4000-8000-000000005103', 'de300000-0000-4000-8000-000000005001', 'c_low', '金额分级①（< 1 万）', 'CONDITION', 1, 'APPROVED', NULL, true, 0, 0, 0, 'e_low', pg_temp.ts(14) + interval '30 minutes', pg_temp.ts(14) + interval '30 minutes', 0),
 ('de300000-0000-4000-8000-000000005104', 'de300000-0000-4000-8000-000000005001', 'n_dept', '部门主管审批', 'APPROVAL', 1, 'APPROVED', 'ALL', true, 1, 1, 0, NULL, pg_temp.ts(14) + interval '30 minutes', pg_temp.ts(14) + interval '2 hours', 5400000),
 ('de300000-0000-4000-8000-000000005105', 'de300000-0000-4000-8000-000000005001', 'e', '结束', 'END', 1, 'APPROVED', NULL, true, 0, 0, 0, NULL, pg_temp.ts(14) + interval '2 hours', pg_temp.ts(14) + interval '2 hours', 0),
 -- FI-2 采购订单（审批中）：停在「采购经理审批」
 ('de300000-0000-4000-8000-000000005111', 'de300000-0000-4000-8000-000000005002', 's', '开始', 'START', 1, 'APPROVED', NULL, true, 0, 0, 0, NULL, pg_temp.h(26), pg_temp.h(26), 0),
 ('de300000-0000-4000-8000-000000005112', 'de300000-0000-4000-8000-000000005002', 'c_credit', '超信用额度判定', 'CONDITION', 1, 'APPROVED', NULL, true, 0, 0, 0, 'e_credit_else', pg_temp.h(26), pg_temp.h(26), 0),
 ('de300000-0000-4000-8000-000000005113', 'de300000-0000-4000-8000-000000005002', 'c_low', '金额分级①（< 1 万）', 'CONDITION', 1, 'APPROVED', NULL, true, 0, 0, 0, 'e_low_else', pg_temp.h(26), pg_temp.h(26), 0),
 ('de300000-0000-4000-8000-000000005114', 'de300000-0000-4000-8000-000000005002', 'c_mid', '金额分级②（< 10 万）', 'CONDITION', 1, 'APPROVED', NULL, true, 0, 0, 0, 'e_mid', pg_temp.h(26), pg_temp.h(26), 0),
 ('de300000-0000-4000-8000-000000005115', 'de300000-0000-4000-8000-000000005002', 'n_pm', '采购经理审批', 'APPROVAL', 1, 'ACTIVE', 'ALL', true, 1, 0, 0, NULL, pg_temp.h(26), NULL, NULL),
 -- FI-3 销售订单（已完成）：开始 → 销售主管 → 金额分级① → 结束
 ('de300000-0000-4000-8000-000000005121', 'de300000-0000-4000-8000-000000005003', 's', '开始', 'START', 1, 'APPROVED', NULL, true, 0, 0, 0, NULL, pg_temp.ts(11) + interval '20 minutes', pg_temp.ts(11) + interval '20 minutes', 0),
 ('de300000-0000-4000-8000-000000005122', 'de300000-0000-4000-8000-000000005003', 'n_sales', '销售主管审批', 'APPROVAL', 1, 'APPROVED', 'ALL', true, 1, 1, 0, NULL, pg_temp.ts(11) + interval '20 minutes', pg_temp.ts(11) + interval '1 hour', 2400000),
 ('de300000-0000-4000-8000-000000005123', 'de300000-0000-4000-8000-000000005003', 'c_low', '金额分级①（< 1 万）', 'CONDITION', 1, 'APPROVED', NULL, true, 0, 0, 0, 'e_low', pg_temp.ts(11) + interval '1 hour', pg_temp.ts(11) + interval '1 hour', 0),
 ('de300000-0000-4000-8000-000000005124', 'de300000-0000-4000-8000-000000005003', 'e', '结束', 'END', 1, 'APPROVED', NULL, true, 0, 0, 0, NULL, pg_temp.ts(11) + interval '1 hour', pg_temp.ts(11) + interval '1 hour', 0),
 -- FI-4 销售订单（审批中）：停在「销售主管审批」
 ('de300000-0000-4000-8000-000000005131', 'de300000-0000-4000-8000-000000005004', 's', '开始', 'START', 1, 'APPROVED', NULL, true, 0, 0, 0, NULL, pg_temp.h(20), pg_temp.h(20), 0),
 ('de300000-0000-4000-8000-000000005132', 'de300000-0000-4000-8000-000000005004', 'n_sales', '销售主管审批', 'APPROVAL', 1, 'ACTIVE', 'ALL', true, 1, 0, 0, NULL, pg_temp.h(20), NULL, NULL),
 -- FI-5 费用报销（审批中）：停在「直属上级审批」
 ('de300000-0000-4000-8000-000000005141', 'de300000-0000-4000-8000-000000005005', 's', '开始', 'START', 1, 'APPROVED', NULL, true, 0, 0, 0, NULL, pg_temp.h(8), pg_temp.h(8), 0),
 ('de300000-0000-4000-8000-000000005142', 'de300000-0000-4000-8000-000000005005', 'n_sup', '直属上级审批', 'APPROVAL', 1, 'ACTIVE', 'ANY', true, 1, 0, 0, NULL, pg_temp.h(8), NULL, NULL),
 -- FI-6 采购订单 PO-3（已完成）：开始 → 额度判定 → 金额分级① → 部门主管 → 结束
 ('de300000-0000-4000-8000-000000005151', 'de300000-0000-4000-8000-000000005006', 's', '开始', 'START', 1, 'APPROVED', NULL, true, 0, 0, 0, NULL, pg_temp.ts(20) + interval '40 minutes', pg_temp.ts(20) + interval '40 minutes', 0),
 ('de300000-0000-4000-8000-000000005152', 'de300000-0000-4000-8000-000000005006', 'c_credit', '超信用额度判定', 'CONDITION', 1, 'APPROVED', NULL, true, 0, 0, 0, 'e_credit_else', pg_temp.ts(20) + interval '40 minutes', pg_temp.ts(20) + interval '40 minutes', 0),
 ('de300000-0000-4000-8000-000000005153', 'de300000-0000-4000-8000-000000005006', 'c_low', '金额分级①（< 1 万）', 'CONDITION', 1, 'APPROVED', NULL, true, 0, 0, 0, 'e_low', pg_temp.ts(20) + interval '40 minutes', pg_temp.ts(20) + interval '40 minutes', 0),
 ('de300000-0000-4000-8000-000000005154', 'de300000-0000-4000-8000-000000005006', 'n_dept', '部门主管审批', 'APPROVAL', 1, 'APPROVED', 'ALL', true, 1, 1, 0, NULL, pg_temp.ts(20) + interval '40 minutes', pg_temp.ts(20) + interval '3 hours', 8400000),
 ('de300000-0000-4000-8000-000000005155', 'de300000-0000-4000-8000-000000005006', 'e', '结束', 'END', 1, 'APPROVED', NULL, true, 0, 0, 0, NULL, pg_temp.ts(20) + interval '3 hours', pg_temp.ts(20) + interval '3 hours', 0)
ON CONFLICT (id) DO NOTHING;

-- 审批任务：2 条已办（本周内完成 → 首页「本周已办」不为 0）+ 3 条待办（admin 审批）
INSERT INTO biz_flow_task (id, instance_id, instance_node_id, node_key, round, task_kind,
        assignee_id, assignee_name, assignee_source, assignee_source_ref, status, is_added_sign,
        is_self_approval, is_auto, comment, created_at, completed_at)
SELECT t.id::uuid, t.instance_id::uuid, t.node_id::uuid, t.node_key, 1, 'APPROVAL',
       u.id, t.assignee_name, 'USER', u.id, t.status, false, false, false,
       t.comment, t.created, t.completed
  FROM (VALUES
   ('de300000-0000-4000-8000-000000005201','de300000-0000-4000-8000-000000005001','de300000-0000-4000-8000-000000005104','n_dept',
    'admin','超级管理员','APPROVED','金额与明细核对无误，同意采购',pg_temp.ts(14) + interval '35 minutes', pg_temp.h(3)),
   ('de300000-0000-4000-8000-000000005202','de300000-0000-4000-8000-000000005002','de300000-0000-4000-8000-000000005115','n_pm',
    'admin','超级管理员','PENDING',NULL,pg_temp.h(26),NULL::timestamptz),
   ('de300000-0000-4000-8000-000000005203','de300000-0000-4000-8000-000000005003','de300000-0000-4000-8000-000000005122','n_sales',
    'admin','超级管理员','APPROVED','客户信用与价格均合规，同意发货',pg_temp.ts(11) + interval '25 minutes', pg_temp.h(5)),
   ('de300000-0000-4000-8000-000000005204','de300000-0000-4000-8000-000000005004','de300000-0000-4000-8000-000000005132','n_sales',
    'admin','超级管理员','PENDING',NULL,pg_temp.h(20),NULL::timestamptz),
   ('de300000-0000-4000-8000-000000005205','de300000-0000-4000-8000-000000005005','de300000-0000-4000-8000-000000005142','n_sup',
    'admin','超级管理员','PENDING',NULL,pg_temp.h(8),NULL::timestamptz),
   ('de300000-0000-4000-8000-000000005206','de300000-0000-4000-8000-000000005006','de300000-0000-4000-8000-000000005154','n_dept',
    'admin','超级管理员','APPROVED','包材年度采购价格已比对，同意下单',pg_temp.ts(20) + interval '45 minutes', pg_temp.h(4))
  ) AS t(id, instance_id, node_id, node_key, assignee, assignee_name, status, comment, created, completed)
  JOIN sys_user u ON u.username = t.assignee AND u.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- 单据 ↔ 流程实例
UPDATE biz_purchase_order SET approval_instance_id = 'de300000-0000-4000-8000-000000005001'
 WHERE id = 'de300000-0000-4000-8000-000000001001';
UPDATE biz_purchase_order SET approval_instance_id = 'de300000-0000-4000-8000-000000005002'
 WHERE id = 'de300000-0000-4000-8000-000000001002';
UPDATE biz_purchase_order SET approval_instance_id = 'de300000-0000-4000-8000-000000005006'
 WHERE id = 'de300000-0000-4000-8000-000000001003';
UPDATE biz_sales_order SET approval_instance_id = 'de300000-0000-4000-8000-000000005003'
 WHERE id = 'de300000-0000-4000-8000-000000001401';
UPDATE biz_sales_order SET approval_instance_id = 'de300000-0000-4000-8000-000000005004'
 WHERE id = 'de300000-0000-4000-8000-000000001402';
UPDATE biz_expense_report SET approval_instance_id = 'de300000-0000-4000-8000-000000005005'
 WHERE id = 'de300000-0000-4000-8000-000000002101';

-- ===========================================================================
-- ⑧ 工作任务（5 个，1 个已逾期）/ 公告（3 条）/ 站内消息（5 条）
-- ===========================================================================
INSERT INTO biz_task (id, task_no, title, description, status, priority, progress, need_review,
        owner_id, creator_id, assigner_id, dept_id, start_date, due_date, estimate_hours,
        actual_hours, source, exec_mode)
SELECT t.id::uuid, 'TSK-' || to_char(now(), 'YYYYMM') || '-' || t.seq, t.title, t.descr,
       t.status, t.priority, t.progress, false,
       u.id, u.id, u.id, 'de300000-0000-4000-8000-000000000001'::uuid,
       CURRENT_DATE - t.start_offset, t.due, t.est, t.act, 'SELF', 'HUMAN'
  FROM (VALUES
   ('de300000-0000-4000-8000-000000002201', '900001', '整理第四季度供应商年度评审资料', '按评分表收齐 12 家核心供应商的资质、交付与质量数据', 'DOING',  'P1', 40, 8, CURRENT_DATE - 3, 6, 3),
   ('de300000-0000-4000-8000-000000002202', '900002', '核对 11 月银行对账单', '中行 4550 与招行 6214 两个户，逐笔核对到凭证', 'TODO',   'P1', 0,  1, CURRENT_DATE, 4, 0),
   ('de300000-0000-4000-8000-000000002203', '900003', '输出 10 月销售分析报告', '按客户/物料两个维度做毛利与回款分析', 'TODO',   'P2', 0,  2, CURRENT_DATE + 1, 6, 0),
   ('de300000-0000-4000-8000-000000002204', '900004', '跟进上海远洋年度框架合同续签', '对齐 2027 年价格与账期，走合同审批流程', 'DOING',  'P1', 30, 5, CURRENT_DATE + 4, 10, 2),
   ('de300000-0000-4000-8000-000000002205', '900005', '梳理演示环境权限清单', '按角色整理菜单/权限码，交付前复核一遍', 'TODO',   'P3', 0,  0, CURRENT_DATE + 9, 3, 0)
  ) AS t(id, seq, title, descr, status, priority, progress, start_offset, due, est, act)
  JOIN sys_user u ON u.username = 'admin' AND u.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_announcement (id, ann_no, title, summary, content_md, content_html, content_format,
        level, scope_type, is_top, top_expire_at, status, publish_at, publisher_id, publisher_name,
        receiver_count, read_count, version, remark)
VALUES
 ('de300000-0000-4000-8000-000000006001',
  'AN' || to_char(pg_temp.d(5), 'YYYYMMDD') || 'D90001',
  '11 月系统例行维护通知',
  '本周日 22:00-23:00 例行维护，期间可能出现短暂不可用。',
  E'## 维护窗口\n\n- 时间：本周日 22:00 - 23:00\n- 影响：登录与单据提交可能出现 30 秒内重试\n- 内容：数据库小版本升级、索引重建\n\n请各业务组提前保存草稿。',
  '', 'MARKDOWN', 'IMPORTANT', 'ALL', true, (now() + interval '7 days'), 'PUBLISHED',
  pg_temp.ts(5), pg_temp.uid('admin'), '超级管理员', 20, 12, 2, ''),
 ('de300000-0000-4000-8000-000000006002',
  'AN' || to_char(pg_temp.d(2), 'YYYYMMDD') || 'D90002',
  '新版费用报销流程上线说明',
  '费用报销改为金额三档审批，1 万元以上增加财务经理复核。',
  E'## 变更要点\n\n1. 1,000 元以下：直属上级审批后直接通过\n2. 1,000 - 10,000 元：直属上级 + 财务经理\n3. 10,000 元以上：直属上级 + 财务经理 + 总经理会签\n\n报销附件仍按原规则上传（发票影像必传）。',
  '', 'MARKDOWN', 'NORMAL', 'ALL', false, NULL, 'PUBLISHED',
  pg_temp.ts(2), pg_temp.uid('admin'), '超级管理员', 20, 15, 1, ''),
 ('de300000-0000-4000-8000-000000006003',
  'AN' || to_char(now(), 'YYYYMMDD') || 'D90003',
  '关于第四季度供应商对账的提醒',
  '请采购与财务在本月 20 日前完成前三季度应付对账。',
  E'## 对账要求\n\n- 范围：2026 年 1-9 月全部入库单\n- 交付物：应付余额确认单（供应商盖章）\n- 截止：本月 20 日\n\n对账差异请在「应付账款」页面对照入库单与付款单逐笔核实。',
  '', 'MARKDOWN', 'URGENT', 'ALL', false, NULL, 'PUBLISHED',
  pg_temp.h(6), pg_temp.uid('admin'), '超级管理员', 20, 3, 1, '')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_notice (id, notice_no, notice_type, level, title, content, content_format,
        link_url, biz_type, biz_id, sender_id, sender_name, target_type, target_ids,
        channel_mask, receiver_count, read_count, status, dispatch_status, created_at)
VALUES
 ('de300000-0000-4000-8000-000000006101', 'NT' || to_char(pg_temp.h(2)::date, 'YYYYMMDD') || 'D90001',
  'APPROVAL', 'WARN', '有一张采购订单待您审批',
  '采购订单（东莞恒信电子科技，价税合计 16,950.00）等待您审批。', 'TEXT',
  '/business/flow/task/todo', 'purchase_order', 'de300000-0000-4000-8000-000000001002',
  pg_temp.uid('zhang.ming'), '张明', 'USER', to_jsonb(ARRAY[(SELECT id::text FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL)]),
  1, 1, 0, 1, 'SENT', pg_temp.h(2)),
 ('de300000-0000-4000-8000-000000006102', 'NT' || to_char(pg_temp.h(3)::date, 'YYYYMMDD') || 'D90002',
  'APPROVAL', 'WARN', '有一张销售订单待您审批',
  '销售订单（杭州天目电子，价税合计 25,990.00）等待您审批。', 'TEXT',
  '/business/flow/task/todo', 'sales_order', 'de300000-0000-4000-8000-000000001402',
  pg_temp.uid('li.na'), '李娜', 'USER', to_jsonb(ARRAY[(SELECT id::text FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL)]),
  1, 1, 0, 1, 'SENT', pg_temp.h(3)),
 ('de300000-0000-4000-8000-000000006103', 'NT' || to_char(pg_temp.h(4)::date, 'YYYYMMDD') || 'D90003',
  'APPROVAL', 'INFO', '有一张费用报销待您审批',
  '费用报销（合肥分仓现场支持差旅，1,200.00）等待您审批。', 'TEXT',
  '/business/flow/task/todo', 'expense_report', 'de300000-0000-4000-8000-000000002101',
  pg_temp.uid('wang.lei'), '王磊', 'USER', to_jsonb(ARRAY[(SELECT id::text FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL)]),
  1, 1, 0, 1, 'SENT', pg_temp.h(4)),
 ('de300000-0000-4000-8000-000000006104', 'NT' || to_char(pg_temp.d(7)::date, 'YYYYMMDD') || 'D90004',
  'PAYMENT', 'SUCCESS', '收款已登记并核销应收',
  '上海远洋贸易回款 3,390.00 已核销 AR 全额。', 'TEXT',
  '/business/erp/receivable', 'receivable', 'de300000-0000-4000-8000-000000001701',
  pg_temp.uid('admin'), '超级管理员', 'USER', to_jsonb(ARRAY[(SELECT id::text FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL)]),
  1, 1, 1, 1, 'SENT', pg_temp.ts(7)),
 ('de300000-0000-4000-8000-000000006105', 'NT' || to_char(pg_temp.d(2)::date, 'YYYYMMDD') || 'D90005',
  'STOCK', 'INFO', '调拨单已发出，货物在途',
  'TK 调拨单（苏州主仓 → 合肥分仓，螺丝 10 件）已发出，请在分仓收货后确认。', 'TEXT',
  '/business/erp/stock-transfer', 'stock_transfer', 'de300000-0000-4000-8000-000000001901',
  pg_temp.uid('zhang.ming'), '张明', 'USER', to_jsonb(ARRAY[(SELECT id::text FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL)]),
  1, 1, 0, 1, 'SENT', pg_temp.ts(2))
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_notice_receiver (id, notice_id, user_id, read_flag, read_at, created_at)
SELECT v.id::uuid, v.notice::uuid, u.id, v.is_read,
       CASE WHEN v.is_read THEN v.created ELSE NULL END, v.created
  FROM (VALUES
   ('de300000-0000-4000-8000-000000006201','de300000-0000-4000-8000-000000006101', false, pg_temp.h(2)),
   ('de300000-0000-4000-8000-000000006202','de300000-0000-4000-8000-000000006102', false, pg_temp.h(3)),
   ('de300000-0000-4000-8000-000000006203','de300000-0000-4000-8000-000000006103', false, pg_temp.h(4)),
   ('de300000-0000-4000-8000-000000006204','de300000-0000-4000-8000-000000006104', true,  pg_temp.ts(7)),
   ('de300000-0000-4000-8000-000000006205','de300000-0000-4000-8000-000000006105', false, pg_temp.ts(2))
  ) AS v(id, notice, is_read, created)
  JOIN sys_user u ON u.username = 'admin' AND u.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

-- ===========================================================================
-- ⑨ 状态轨迹（biz_status_log）—— 详情页的「审批/状态记录」靠它
-- ===========================================================================
INSERT INTO biz_status_log (id, entity_type, entity_id, entity_no, from_status, to_status,
        operator_id, operator_name, reason, biz_ref_type, biz_ref_no, action, created_at)
VALUES
 -- 采购订单 PO-1
 ('de300000-0000-4000-8000-000000004301', 'PURCHASE_ORDER', 'de300000-0000-4000-8000-000000001001',
  (SELECT order_no FROM biz_purchase_order WHERE id = 'de300000-0000-4000-8000-000000001001'),
  NULL, 'DRAFT', pg_temp.uid('zhang.ming'), '张明', '创建采购订单', NULL, NULL, 'CREATE', pg_temp.ts(14)),
 ('de300000-0000-4000-8000-000000004302', 'PURCHASE_ORDER', 'de300000-0000-4000-8000-000000001001',
  (SELECT order_no FROM biz_purchase_order WHERE id = 'de300000-0000-4000-8000-000000001001'),
  'DRAFT', 'SUBMITTED', pg_temp.uid('zhang.ming'), '张明', '提交审批', NULL, NULL, 'SUBMIT', pg_temp.ts(14) + interval '30 minutes'),
 ('de300000-0000-4000-8000-000000004303', 'PURCHASE_ORDER', 'de300000-0000-4000-8000-000000001001',
  (SELECT order_no FROM biz_purchase_order WHERE id = 'de300000-0000-4000-8000-000000001001'),
  'SUBMITTED', 'APPROVED', pg_temp.uid('admin'), '超级管理员', '采购订单审批通过', 'PO_APPROVE', 'FLW-D90001', 'APPROVE', pg_temp.ts(14) + interval '2 hours'),
 ('de300000-0000-4000-8000-000000004304', 'PURCHASE_ORDER', 'de300000-0000-4000-8000-000000001001',
  (SELECT order_no FROM biz_purchase_order WHERE id = 'de300000-0000-4000-8000-000000001001'),
  'APPROVED', 'RECEIVED', pg_temp.uid('zhang.ming'), '张明', '收货过账自动推进',
  'STOCK_IN', (SELECT in_no FROM biz_stock_in WHERE id = 'de300000-0000-4000-8000-000000001101'), 'RECEIVE', pg_temp.ts(13) + interval '1 hour'),
 -- 采购订单 PO-2（审批中）
 ('de300000-0000-4000-8000-000000004305', 'PURCHASE_ORDER', 'de300000-0000-4000-8000-000000001002',
  (SELECT order_no FROM biz_purchase_order WHERE id = 'de300000-0000-4000-8000-000000001002'),
  NULL, 'DRAFT', pg_temp.uid('zhang.ming'), '张明', '创建采购订单', NULL, NULL, 'CREATE', pg_temp.ts(3)),
 ('de300000-0000-4000-8000-000000004306', 'PURCHASE_ORDER', 'de300000-0000-4000-8000-000000001002',
  (SELECT order_no FROM biz_purchase_order WHERE id = 'de300000-0000-4000-8000-000000001002'),
  'DRAFT', 'SUBMITTED', pg_temp.uid('zhang.ming'), '张明', '提交审批（采购经理）', NULL, NULL, 'SUBMIT', pg_temp.h(26)),
 -- 收货入库
 ('de300000-0000-4000-8000-000000004307', 'STOCK_IN', 'de300000-0000-4000-8000-000000001101',
  (SELECT in_no FROM biz_stock_in WHERE id = 'de300000-0000-4000-8000-000000001101'),
  NULL, 'DRAFT', pg_temp.uid('zhang.ming'), '张明', '由采购订单生成收货单', NULL, NULL, 'CREATE', pg_temp.ts(13)),
 ('de300000-0000-4000-8000-000000004308', 'STOCK_IN', 'de300000-0000-4000-8000-000000001101',
  (SELECT in_no FROM biz_stock_in WHERE id = 'de300000-0000-4000-8000-000000001101'),
  'DRAFT', 'POSTED', pg_temp.uid('zhang.ming'), '张明', '收货过账（库存/应付/凭证一次写全）', NULL, NULL, 'POST', pg_temp.ts(13) + interval '1 hour'),
 -- 销售订单 SO-1 / SO-2
 ('de300000-0000-4000-8000-000000004309', 'SALE_ORDER', 'de300000-0000-4000-8000-000000001401',
  (SELECT so_no FROM biz_sales_order WHERE id = 'de300000-0000-4000-8000-000000001401'),
  NULL, 'DRAFT', pg_temp.uid('li.na'), '李娜', '创建销售订单', NULL, NULL, 'CREATE', pg_temp.ts(11)),
 ('de300000-0000-4000-8000-000000004310', 'SALE_ORDER', 'de300000-0000-4000-8000-000000001401',
  (SELECT so_no FROM biz_sales_order WHERE id = 'de300000-0000-4000-8000-000000001401'),
  'DRAFT', 'SUBMITTED', pg_temp.uid('li.na'), '李娜', '提交审批', NULL, NULL, 'SUBMIT', pg_temp.ts(11) + interval '20 minutes'),
 ('de300000-0000-4000-8000-000000004311', 'SALE_ORDER', 'de300000-0000-4000-8000-000000001401',
  (SELECT so_no FROM biz_sales_order WHERE id = 'de300000-0000-4000-8000-000000001401'),
  'SUBMITTED', 'APPROVED', pg_temp.uid('admin'), '超级管理员', '销售订单审批通过', 'SO_APPROVE', 'FLW-D90003', 'APPROVE', pg_temp.ts(11) + interval '1 hour'),
 ('de300000-0000-4000-8000-000000004312', 'SALE_ORDER', 'de300000-0000-4000-8000-000000001401',
  (SELECT so_no FROM biz_sales_order WHERE id = 'de300000-0000-4000-8000-000000001401'),
  'APPROVED', 'OUTBOUND', pg_temp.uid('li.na'), '李娜', '发货过账自动推进',
  'SHIPMENT', (SELECT ship_no FROM biz_shipment WHERE id = 'de300000-0000-4000-8000-000000001501'), 'SHIP', pg_temp.ts(10) + interval '2 hours'),
 ('de300000-0000-4000-8000-000000004313', 'SALE_ORDER', 'de300000-0000-4000-8000-000000001402',
  (SELECT so_no FROM biz_sales_order WHERE id = 'de300000-0000-4000-8000-000000001402'),
  NULL, 'DRAFT', pg_temp.uid('li.na'), '李娜', '创建销售订单', NULL, NULL, 'CREATE', pg_temp.ts(1)),
 ('de300000-0000-4000-8000-000000004314', 'SALE_ORDER', 'de300000-0000-4000-8000-000000001402',
  (SELECT so_no FROM biz_sales_order WHERE id = 'de300000-0000-4000-8000-000000001402'),
  'DRAFT', 'SUBMITTED', pg_temp.uid('li.na'), '李娜', '提交审批（销售主管）', NULL, NULL, 'SUBMIT', pg_temp.h(20)),
 -- 发货 / 出库 / 应收 / 收款 / 付款 / 调拨 / 盘点 / 报销
 ('de300000-0000-4000-8000-000000004315', 'SHIPMENT', 'de300000-0000-4000-8000-000000001501',
  (SELECT ship_no FROM biz_shipment WHERE id = 'de300000-0000-4000-8000-000000001501'),
  NULL, 'POSTED', pg_temp.uid('li.na'), '李娜', '发货并生成销售出库单', NULL, NULL, 'POST', pg_temp.ts(10) + interval '2 hours'),
 ('de300000-0000-4000-8000-000000004316', 'STOCK_OUT', 'de300000-0000-4000-8000-000000001601',
  (SELECT out_no FROM biz_stock_out WHERE id = 'de300000-0000-4000-8000-000000001601'),
  NULL, 'POSTED', pg_temp.uid('li.na'), '李娜', '销售出库过账（成本结转）', NULL, NULL, 'POST', pg_temp.ts(10) + interval '2 hours'),
 ('de300000-0000-4000-8000-000000004317', 'RECEIVABLE', 'de300000-0000-4000-8000-000000001701',
  (SELECT ar_no FROM biz_receivable WHERE id = 'de300000-0000-4000-8000-000000001701'),
  NULL, 'OPEN', pg_temp.uid('li.na'), '李娜', '发货生成应收', NULL, NULL, 'CREATE', pg_temp.ts(10) + interval '3 hours'),
 ('de300000-0000-4000-8000-000000004318', 'RECEIVABLE', 'de300000-0000-4000-8000-000000001701',
  (SELECT ar_no FROM biz_receivable WHERE id = 'de300000-0000-4000-8000-000000001701'),
  'OPEN', 'SETTLED', pg_temp.uid('admin'), '超级管理员', '收款核销全额', 'RECEIPT', (SELECT receipt_no FROM biz_receipt WHERE id = 'de300000-0000-4000-8000-000000001801'), 'SETTLE', pg_temp.ts(7)),
 ('de300000-0000-4000-8000-000000004319', 'RECEIPT', 'de300000-0000-4000-8000-000000001801',
  (SELECT receipt_no FROM biz_receipt WHERE id = 'de300000-0000-4000-8000-000000001801'),
  NULL, 'POSTED', pg_temp.uid('admin'), '超级管理员', '收款登记并核销', NULL, NULL, 'POST', pg_temp.ts(7)),
 ('de300000-0000-4000-8000-000000004320', 'PAYABLE', 'de300000-0000-4000-8000-000000001201',
  (SELECT payable_no FROM biz_payable WHERE id = 'de300000-0000-4000-8000-000000001201'),
  NULL, 'OPEN', pg_temp.uid('admin'), '超级管理员', '收货过账生成应付', NULL, NULL, 'CREATE', pg_temp.ts(13) + interval '1 hour'),
 ('de300000-0000-4000-8000-000000004321', 'PAYABLE', 'de300000-0000-4000-8000-000000001201',
  (SELECT payable_no FROM biz_payable WHERE id = 'de300000-0000-4000-8000-000000001201'),
  'OPEN', 'SETTLED', pg_temp.uid('admin'), '超级管理员', '付款结清', 'PAYMENT', (SELECT payment_no FROM biz_payment WHERE id = 'de300000-0000-4000-8000-000000001301'), 'SETTLE', pg_temp.ts(9)),
 ('de300000-0000-4000-8000-000000004322', 'PAYMENT', 'de300000-0000-4000-8000-000000001301',
  (SELECT payment_no FROM biz_payment WHERE id = 'de300000-0000-4000-8000-000000001301'),
  NULL, 'APPROVED', pg_temp.uid('admin'), '超级管理员', '付款审批通过并记账', NULL, NULL, 'POST', pg_temp.ts(9)),
 ('de300000-0000-4000-8000-000000004323', 'STOCK_TRANSFER', 'de300000-0000-4000-8000-000000001901',
  (SELECT transfer_no FROM biz_stock_transfer WHERE id = 'de300000-0000-4000-8000-000000001901'),
  'APPROVED', 'IN_TRANSIT', pg_temp.uid('zhang.ming'), '张明', '调拨发出，货物在途',
  'STOCK_TRANSFER', (SELECT transfer_no FROM biz_stock_transfer WHERE id = 'de300000-0000-4000-8000-000000001901'), 'SHIP', pg_temp.ts(2) + interval '3 hours'),
 ('de300000-0000-4000-8000-000000004324', 'STOCK_TAKE', 'de300000-0000-4000-8000-000000002001',
  (SELECT take_no FROM biz_stock_take WHERE id = 'de300000-0000-4000-8000-000000002001'),
  NULL, 'DRAFT', pg_temp.uid('zhang.ming'), '张明', '创建抽盘单（A 区）', NULL, NULL, 'CREATE', CURRENT_TIMESTAMP),
 ('de300000-0000-4000-8000-000000004325', 'EXPENSE_REPORT', 'de300000-0000-4000-8000-000000002101',
  (SELECT report_no FROM biz_expense_report WHERE id = 'de300000-0000-4000-8000-000000002101'),
  'DRAFT', 'SUBMITTED', pg_temp.uid('wang.lei'), '王磊', '提交报销审批', NULL, NULL, 'SUBMIT', pg_temp.h(8))
ON CONFLICT (id) DO NOTHING;

-- ===========================================================================
-- ⑩ 自检：任何一条不成立就整笔回滚（宁可没有演示数据，也不留半套）
-- ===========================================================================
DO $$
DECLARE bad int; detail text;
BEGIN
    -- 1) 演示凭证借贷必须平，且 entry_count 与分录条数一致
    SELECT count(*), string_agg(voucher_no, ', ') INTO bad, detail
      FROM (SELECT v.id, v.voucher_no, v.entry_count,
                   (SELECT count(*) FROM biz_voucher_entry e WHERE e.voucher_id = v.id) AS n,
                   (SELECT coalesce(sum(e.debit_amount), 0) - coalesce(sum(e.credit_amount), 0)
                      FROM biz_voucher_entry e WHERE e.voucher_id = v.id) AS diff
              FROM biz_voucher v WHERE v.id::text LIKE 'de300000-%') s
     WHERE s.diff <> 0 OR s.n <> s.entry_count OR s.n < 2;
    IF bad > 0 THEN RAISE EXCEPTION '演示凭证不平/条数不符：% 张（%）', bad, detail; END IF;

    -- 2) 库存模块金额 == 演示凭证 1405 借贷净额
    SELECT round(coalesce(sum(i.amount), 0) - coalesce((
               SELECT sum(e.debit_amount - e.credit_amount)
                 FROM biz_voucher_entry e
                 JOIN biz_voucher v ON v.id = e.voucher_id AND v.deleted_at IS NULL
                WHERE v.id::text LIKE 'de300000-%' AND e.account_code LIKE '1405%'), 0), 4)
      INTO bad FROM biz_inventory i WHERE i.id::text LIKE 'de300000-%';
    IF bad <> 0 THEN RAISE EXCEPTION '演示库存金额与 1405 科目净额差 %', bad; END IF;

    -- 3) 应付未付 == 演示凭证 2202 贷方净额
    SELECT round(coalesce((SELECT sum(unpaid_amount) FROM biz_payable
                            WHERE id::text LIKE 'de300000-%' AND deleted_at IS NULL
                              AND status <> 'CANCELLED'), 0) - coalesce((
               SELECT sum(e.credit_amount - e.debit_amount)
                 FROM biz_voucher_entry e
                 JOIN biz_voucher v ON v.id = e.voucher_id AND v.deleted_at IS NULL
                WHERE v.id::text LIKE 'de300000-%' AND e.account_code LIKE '2202%'), 0), 4)
      INTO bad;
    IF bad <> 0 THEN RAISE EXCEPTION '演示应付未付与 2202 科目净额差 %', bad; END IF;

    -- 4) 逐 (物料, 仓库)：库存流水累计 == 库存数量
    SELECT count(*), string_agg(material_id::text, ', ') INTO bad, detail
      FROM (
        WITH txn AS (SELECT material_id, warehouse_id,
                            sum(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END) AS q
                       FROM biz_inventory_txn WHERE id::text LIKE 'de300000-%'
                      GROUP BY 1, 2),
             inv AS (SELECT material_id, warehouse_id, sum(qty) AS q
                       FROM biz_inventory WHERE id::text LIKE 'de300000-%' GROUP BY 1, 2)
        SELECT coalesce(t.material_id, i.material_id) AS material_id
          FROM txn t FULL JOIN inv i ON t.material_id = i.material_id AND t.warehouse_id = i.warehouse_id
         WHERE abs(coalesce(t.q, 0) - coalesce(i.q, 0)) > 0.0001
      ) s;
    IF bad > 0 THEN RAISE EXCEPTION '演示库存流水与库存不符：% 组（%）', bad, detail; END IF;

    -- 5) 流程闭环：PENDING 实例必须有且只有 1 条 PENDING 待办；PENDING 待办的实例必须 PENDING
    SELECT count(*) INTO bad
      FROM biz_flow_instance i
     WHERE i.id::text LIKE 'de300000-%' AND i.status = 'PENDING'
       AND (SELECT count(*) FROM biz_flow_task t
             WHERE t.instance_id = i.id AND t.status = 'PENDING') <> 1;
    IF bad > 0 THEN RAISE EXCEPTION '待办审批条数与实例不符：% 条实例', bad; END IF;

    SELECT count(*) INTO bad
      FROM biz_flow_task t JOIN biz_flow_instance i ON i.id = t.instance_id
     WHERE t.id::text LIKE 'de300000-%' AND t.status = 'PENDING' AND i.status <> 'PENDING';
    IF bad > 0 THEN RAISE EXCEPTION '存在实例已结束但待办仍 PENDING 的孤儿待办：% 条', bad; END IF;

    -- 6) 凭证来源必须是 VoucherSourceType::ALL 的值域（唯一真相 backend/src/business/shared/doc_kind.rs）
    --    这条是「演示数据造出商品永远不会写的来源值」那类事故的兜底：源轴写成业务对象轴的值
    --    （例如 RECEIVABLE），前端来源单据就永远不可点，巡检也会判非法值。
    --    ⚠ 增删枚举时本清单要同步（doc_kind 的单测会盯住这 20 个值）。
    SELECT count(*), string_agg(DISTINCT v.source_type, ', ') INTO bad, detail
      FROM biz_voucher v
     WHERE v.deleted_at IS NULL
       AND v.source_type NOT IN ('STOCK_IN', 'STOCK_OUT', 'PAYMENT', 'STOCK_TAKE', 'SERIAL_BATCH',
                                 'SERIAL_OUT', 'SERIAL_SCRAP', 'OPENING', 'YEAR_END', 'MANUAL',
                                 'FIXED_ASSET_ACQ', 'FIXED_ASSET_DISPOSAL', 'DEPRECIATION',
                                 'EXPENSE_REPORT', 'EXPENSE_PAYMENT', 'RECEIPT', 'RECEIPT_REFUND',
                                 'INVOICE', 'COMMISSION', 'COST_PERIOD',
                                 'PAYROLL_ACCRUAL', 'PAYROLL_PAYMENT');
    IF bad > 0 THEN RAISE EXCEPTION '凭证来源出现 VoucherSourceType 枚举外的值：% 张（%）', bad, detail; END IF;

    -- 7) 销售开票的收入凭证三处指针必须闭合：
    --    恰好 1 张 source_type='INVOICE'，且 voucher.source_id = 发票 id、source_no = 发票号、
    --    发票 voucher_id 指回这张凭证
    SELECT count(*) INTO bad
      FROM biz_voucher v
      JOIN biz_invoice i ON i.id = 'de300000-0000-4000-8000-000000002301'::uuid
     WHERE v.id::text LIKE 'de300000-%' AND v.deleted_at IS NULL AND v.source_type = 'INVOICE';
    IF bad <> 1 THEN RAISE EXCEPTION '销售开票的收入凭证应恰好 1 张（source_type=INVOICE），实际 % 张', bad; END IF;

    SELECT count(*), string_agg(v.voucher_no, ', ') INTO bad, detail
      FROM biz_voucher v
      JOIN biz_invoice i ON i.id = 'de300000-0000-4000-8000-000000002301'::uuid
     WHERE v.id::text LIKE 'de300000-%' AND v.deleted_at IS NULL AND v.source_type = 'INVOICE'
       AND (v.source_id IS DISTINCT FROM i.id
            OR v.source_no IS DISTINCT FROM i.invoice_no
            OR i.voucher_id IS DISTINCT FROM v.id);
    IF bad > 0 THEN RAISE EXCEPTION '销售开票凭证与发票的指针不闭合：% 张（%）', bad, detail; END IF;

    -- 8) 应收的 voucher_id 必须由发票回填（应收不再自己当凭证来源）
    SELECT count(*) INTO bad
      FROM biz_receivable r
     WHERE r.id = 'de300000-0000-4000-8000-000000001701'::uuid
       AND (SELECT i.voucher_id FROM biz_invoice i WHERE i.receivable_id = r.id AND i.deleted_at IS NULL) IS DISTINCT FROM r.voucher_id;
    IF bad > 0 THEN RAISE EXCEPTION '应收的 voucher_id 未由发票回填（应收不该自己当凭证来源）'; END IF;
END $$;
SQL2
        echo "$tail"
    } | "${PSQL[@]}"
}

# ---------------------------------------------------------------- 统计
# 只数 id 前缀命中的行 —— 与 --clean 的判据是同一个前缀，不会「统计说有、清理删不掉」
summary() {
    echo
    echo "── 演示数据清点（前缀 ${DEMO_PREFIX}）────────────────────────────────"
    "${PSQL[@]}" -At <<'SQL'
SELECT rpad(label, 22) || lpad(n::text, 6) || coalesce('   ' || note, '')
  FROM (
    SELECT 1 AS ord, '供应商' AS label, count(*) AS n,
           (SELECT count(*)::text FROM biz_supplier WHERE id::text LIKE 'de300000-%' AND invoice_bank_account IS NOT NULL) || ' 家有银行账户' AS note
      FROM biz_supplier WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 2, '客户', count(*), '1 家有信用额度' FROM biz_customer WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 3, '物料', count(*), '2 个批次管理 / 1 个序列号管控' FROM biz_material WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 4, '仓库 / 库位', (SELECT count(*) FROM biz_warehouse WHERE id::text LIKE 'de300000-%'),
              (SELECT count(*)::text FROM biz_location WHERE id::text LIKE 'de300000-%') || ' 个库位'
    UNION ALL SELECT 5, '采购订单', count(*), '含 1 张审批中' FROM biz_purchase_order WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 6, '收货入库单', count(*), NULL FROM biz_stock_in WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 7, '应付账款', count(*), NULL FROM biz_payable WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 8, '付款单', count(*), NULL FROM biz_payment WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 9, '销售订单', count(*), '含 1 张待审批' FROM biz_sales_order WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 10, '发货单', count(*), NULL FROM biz_shipment WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 11, '出库单', count(*), NULL FROM biz_stock_out WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 12, '应收 / 发票 / 收款', (SELECT count(*) FROM biz_receivable WHERE id::text LIKE 'de300000-%'),
              (SELECT count(*)::text FROM biz_invoice WHERE id::text LIKE 'de300000-%') || ' 张发票 / '
              || (SELECT count(*)::text FROM biz_receipt WHERE id::text LIKE 'de300000-%') || ' 张收款单'
    UNION ALL SELECT 13, '库存桶 / 流水', (SELECT count(*) FROM biz_inventory WHERE id::text LIKE 'de300000-%'),
              (SELECT count(*)::text FROM biz_inventory_txn WHERE id::text LIKE 'de300000-%') || ' 条流水'
    UNION ALL SELECT 14, '调拨单 / 盘点单', (SELECT count(*) FROM biz_stock_transfer WHERE id::text LIKE 'de300000-%'),
              (SELECT count(*)::text FROM biz_stock_take WHERE id::text LIKE 'de300000-%') || ' 张盘点单'
    UNION ALL SELECT 15, '凭证 / 分录', (SELECT count(*) FROM biz_voucher WHERE id::text LIKE 'de300000-%'),
              (SELECT count(*)::text FROM biz_voucher_entry WHERE id::text LIKE 'de300000-%') || ' 条分录'
    UNION ALL SELECT 16, '审批实例', count(*),
              (SELECT count(*)::text FROM biz_flow_instance WHERE id::text LIKE 'de300000-%' AND status = 'PENDING') || ' 条待审批'
      FROM biz_flow_instance WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 17, '审批任务（待办/已办）',
              (SELECT count(*) FROM biz_flow_task WHERE id::text LIKE 'de300000-%' AND status = 'PENDING'),
              (SELECT count(*)::text FROM biz_flow_task WHERE id::text LIKE 'de300000-%' AND status <> 'PENDING') || ' 条已办'
    UNION ALL SELECT 18, '工作任务', count(*),
              (SELECT count(*)::text FROM biz_task WHERE id::text LIKE 'de300000-%' AND due_date < CURRENT_DATE) || ' 个已逾期'
      FROM biz_task WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 19, '公告 / 站内消息', (SELECT count(*) FROM sys_announcement WHERE id::text LIKE 'de300000-%'),
              (SELECT count(*)::text FROM sys_notice WHERE id::text LIKE 'de300000-%') || ' 条消息'
    UNION ALL SELECT 20, '状态轨迹', count(*), NULL FROM biz_status_log WHERE id::text LIKE 'de300000-%'
    UNION ALL SELECT 21, '演示账号', count(*), 'zhang.ming / li.na / wang.lei / chen.li（口令继承 admin）'
      FROM sys_user WHERE id::text LIKE 'de300000-%'
  ) s
 ORDER BY ord;
SQL
}

# ---------------------------------------------------------------- 清理
# 只删 id 前缀命中的演示行；删之前先按表数一遍，删完再确认归零。
# 顺序按外键依赖从叶子往根走（子表 → 父表），不依赖 CASCADE。
clean() {
    {
        echo "BEGIN;"
        cat <<'SQL'
SET LOCAL client_min_messages = warning;
CREATE TEMP TABLE demo_before(tbl text, n bigint) ON COMMIT DROP;
INSERT INTO demo_before VALUES
 ('收款核销明细',  (SELECT count(*) FROM biz_receipt_allocation WHERE id::text LIKE 'de300000-%')),
 ('销售发票',      (SELECT count(*) FROM biz_invoice           WHERE id::text LIKE 'de300000-%')),
 ('应收账款',      (SELECT count(*) FROM biz_receivable         WHERE id::text LIKE 'de300000-%')),
 ('收款单',        (SELECT count(*) FROM biz_receipt            WHERE id::text LIKE 'de300000-%')),
 ('付款单',        (SELECT count(*) FROM biz_payment            WHERE id::text LIKE 'de300000-%')),
 ('出库单/明细',   (SELECT count(*) FROM biz_stock_out          WHERE id::text LIKE 'de300000-%')),
 ('发货单/明细',   (SELECT count(*) FROM biz_shipment           WHERE id::text LIKE 'de300000-%')),
 ('销售订单/明细', (SELECT count(*) FROM biz_sales_order        WHERE id::text LIKE 'de300000-%')),
 ('应付账款',      (SELECT count(*) FROM biz_payable            WHERE id::text LIKE 'de300000-%')),
 ('收货单/明细',   (SELECT count(*) FROM biz_stock_in           WHERE id::text LIKE 'de300000-%')),
 ('采购订单/明细', (SELECT count(*) FROM biz_purchase_order     WHERE id::text LIKE 'de300000-%')),
 ('调拨单/明细',   (SELECT count(*) FROM biz_stock_transfer     WHERE id::text LIKE 'de300000-%')),
 ('盘点单/明细',   (SELECT count(*) FROM biz_stock_take         WHERE id::text LIKE 'de300000-%')),
 ('审批实例/任务', (SELECT count(*) FROM biz_flow_instance      WHERE id::text LIKE 'de300000-%')),
 ('凭证/分录',     (SELECT count(*) FROM biz_voucher            WHERE id::text LIKE 'de300000-%')),
 ('报销单/明细',   (SELECT count(*) FROM biz_expense_report     WHERE id::text LIKE 'de300000-%')),
 ('库存/流水',     (SELECT count(*) FROM biz_inventory          WHERE id::text LIKE 'de300000-%')),
 ('状态轨迹',      (SELECT count(*) FROM biz_status_log         WHERE id::text LIKE 'de300000-%')),
 ('工作任务',      (SELECT count(*) FROM biz_task               WHERE id::text LIKE 'de300000-%')),
 ('公告',          (SELECT count(*) FROM sys_announcement       WHERE id::text LIKE 'de300000-%')),
 ('站内消息',      (SELECT count(*) FROM sys_notice             WHERE id::text LIKE 'de300000-%')),
 ('供应商',        (SELECT count(*) FROM biz_supplier           WHERE id::text LIKE 'de300000-%')),
 ('客户',          (SELECT count(*) FROM biz_customer           WHERE id::text LIKE 'de300000-%')),
 ('物料',          (SELECT count(*) FROM biz_material           WHERE id::text LIKE 'de300000-%')),
 ('仓库/库位',     (SELECT count(*) FROM biz_warehouse          WHERE id::text LIKE 'de300000-%')),
 ('演示账号',      (SELECT count(*) FROM sys_user               WHERE id::text LIKE 'de300000-%'));

DELETE FROM biz_receipt_allocation WHERE receipt_id IN (SELECT id FROM biz_receipt WHERE id::text LIKE 'de300000-%');
-- 级联清理：**引擎按演示单据生成的**产物（行 id 是随机 UUID，只能靠 source_id / biz_id 反查）。
-- 不删这一批，"清理"会留下挂空的凭证/实例/流水（实测：审批演示单后过账出的凭证就是这样残留的）。
DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_id::text LIKE 'de300000-%');
DELETE FROM biz_voucher      WHERE source_id::text LIKE 'de300000-%';
DELETE FROM biz_flow_task    WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_id::text LIKE 'de300000-%');
DELETE FROM biz_flow_cc      WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_id::text LIKE 'de300000-%');
DELETE FROM biz_flow_instance_node WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_id::text LIKE 'de300000-%');
DELETE FROM biz_flow_instance WHERE biz_id::text LIKE 'de300000-%';
DELETE FROM biz_status_log   WHERE entity_id::text LIKE 'de300000-%';
DELETE FROM biz_inventory_txn WHERE doc_id::text LIKE 'de300000-%';
DELETE FROM biz_doc_attachment WHERE doc_id::text LIKE 'de300000-%';
DELETE FROM sys_notice       WHERE biz_id LIKE 'de300000-%';
-- 发票必须先于应收删：biz_invoice.receivable_id 是 FK ON DELETE RESTRICT（不是 CASCADE）
DELETE FROM biz_invoice      WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_receivable   WHERE source_id::text LIKE 'de300000-%';
DELETE FROM biz_receipt      WHERE id IN (SELECT receipt_id FROM biz_receipt_allocation WHERE receivable_id::text LIKE 'de300000-%');
DELETE FROM biz_payment      WHERE payable_id::text LIKE 'de300000-%';
DELETE FROM biz_payable      WHERE source_id::text LIKE 'de300000-%';
DELETE FROM biz_stock_in_item WHERE stock_in_id IN (SELECT id FROM biz_stock_in WHERE source_order_id::text LIKE 'de300000-%');
DELETE FROM biz_stock_in     WHERE source_order_id::text LIKE 'de300000-%';
DELETE FROM biz_stock_out_item WHERE stock_out_id IN (SELECT id FROM biz_stock_out WHERE source_id::text LIKE 'de300000-%');
DELETE FROM biz_stock_out    WHERE source_id::text LIKE 'de300000-%';
DELETE FROM biz_shipment_item WHERE shipment_id IN (SELECT id FROM biz_shipment WHERE so_id::text LIKE 'de300000-%');
DELETE FROM biz_shipment     WHERE so_id::text LIKE 'de300000-%';
DELETE FROM biz_receivable        WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_receipt           WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_payment           WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_stock_out_item    WHERE stock_out_id IN (SELECT id FROM biz_stock_out WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_stock_out         WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_shipment_item     WHERE shipment_id IN (SELECT id FROM biz_shipment WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_shipment          WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_sales_order_item  WHERE so_id IN (SELECT id FROM biz_sales_order WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_sales_order       WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_payable           WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_stock_in_item     WHERE stock_in_id IN (SELECT id FROM biz_stock_in WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_stock_in          WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_purchase_order_item WHERE order_id IN (SELECT id FROM biz_purchase_order WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_purchase_order    WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_stock_transfer_item WHERE transfer_id IN (SELECT id FROM biz_stock_transfer WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_stock_transfer    WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_stock_take_item   WHERE take_id IN (SELECT id FROM biz_stock_take WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_stock_take        WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_flow_task         WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_flow_cc           WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_flow_instance_node WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_flow_instance     WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_voucher_entry     WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_voucher           WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_expense_item      WHERE report_id IN (SELECT id FROM biz_expense_report WHERE id::text LIKE 'de300000-%');
DELETE FROM biz_expense_report    WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_expense_type      WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_material_serial   WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_inventory         WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_inventory_txn     WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_inventory_location WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_status_log        WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_task              WHERE id::text LIKE 'de300000-%';
DELETE FROM sys_announcement_read  WHERE announcement_id IN (SELECT id FROM sys_announcement WHERE id::text LIKE 'de300000-%');
DELETE FROM sys_announcement_scope WHERE announcement_id IN (SELECT id FROM sys_announcement WHERE id::text LIKE 'de300000-%');
DELETE FROM sys_announcement      WHERE id::text LIKE 'de300000-%';
DELETE FROM sys_notice_receiver   WHERE notice_id IN (SELECT id FROM sys_notice WHERE id::text LIKE 'de300000-%');
DELETE FROM sys_notice_delivery   WHERE notice_id IN (SELECT id FROM sys_notice WHERE id::text LIKE 'de300000-%');
DELETE FROM sys_notice            WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_supplier          WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_customer          WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_material          WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_location          WHERE id::text LIKE 'de300000-%';
DELETE FROM biz_warehouse         WHERE id::text LIKE 'de300000-%';
DELETE FROM sys_user_role         WHERE user_id IN (SELECT id FROM sys_user WHERE id::text LIKE 'de300000-%');
DELETE FROM sys_user              WHERE id::text LIKE 'de300000-%';
DELETE FROM sys_dept              WHERE id::text LIKE 'de300000-%';

\echo ''
\echo '已删除的演示数据（按表）：'
SELECT rpad(tbl, 22) || lpad(n::text, 6) FROM demo_before WHERE n > 0 ORDER BY n DESC;

\echo ''
\echo '清理后残留自检（应全部为 0）：'
SELECT rpad('残留行数合计', 22) || lpad((
   SELECT sum(c)::text FROM (
     SELECT (SELECT count(*) FROM biz_purchase_order WHERE id::text LIKE 'de300000-%') AS c
     UNION ALL SELECT (SELECT count(*) FROM biz_sales_order      WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_stock_in         WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_stock_out        WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_voucher          WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_flow_instance    WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_inventory        WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_task             WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM sys_announcement     WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM sys_notice           WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_supplier         WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_customer         WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_invoice          WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_material         WHERE id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM sys_user             WHERE id::text LIKE 'de300000-%')
     -- 反向：引擎按演示单据生成的挂空行（id 不是前缀，靠 source_id / biz_id 找）
     UNION ALL SELECT (SELECT count(*) FROM biz_voucher       WHERE source_id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_flow_instance WHERE biz_id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_inventory_txn WHERE doc_id::text LIKE 'de300000-%')
     UNION ALL SELECT (SELECT count(*) FROM biz_status_log    WHERE entity_id::text LIKE 'de300000-%')
   ) x), 6);
SQL
        echo "COMMIT;"
    } | "${PSQL[@]}"
}

# ---------------------------------------------------------------- 入口
main() {
    printf '『云枢演示数据』\n'
    printf '  目标库：%s\n' "$(mask_url "$DB_URL")"
    if [ "$DO_CLEAN" -eq 1 ]; then
        printf '  模式：--clean（只删演示数据）\n'
    elif [ "$DRY_RUN" -eq 1 ]; then
        printf '  模式：--dry-run（跑完整事务后回滚）\n'
    else
        printf '  模式：灌入 / 补足（幂等）\n'
    fi

    preflight
    if [ "$DO_CLEAN" -eq 1 ]; then
        clean
        echo
        echo "✅ 演示数据已清理（真实数据未动）"
        return 0
    fi

    seed
    summary
    echo
    if [ "$DRY_RUN" -eq 1 ]; then
        echo "✅ dry-run 通过（已回滚，库里没有留下任何行）"
    else
        echo "✅ 演示数据已就绪。登录后可看到："
        echo "   · 首页四张卡都有数：待办审批 3 / 逾期 1 / 今日任务 / 本周已办"
        echo "   · 审批中心 3 条待办（采购 / 销售 / 报销，可直接同意或驳回）"
        echo "   · 工作任务 5 个（1 个逾期）、公告 3 条、站内消息 5 条"
        echo "   · 凭证 10 张（采购入库 3 / 付款 1 / 销售成本 1 / 销售开票 1 / 收款 1 / 手工 3）"
        echo "   · 销售发票 1 张（IV-2026-1042，来源单据 = 销售出库单；开票的收入凭证在凭证页的「来源单据」可点回发票）"
        echo "   · 演示账号 zhang.ming / li.na / wang.lei / chen.li 的口令 = 当前 admin 的口令（登录后请改密）"
        echo "   撤销全部演示数据：bash scripts/demo_seed.sh --clean"
    fi
}

main "$@"
