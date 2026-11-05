#!/usr/bin/env python3
"""V0258 生产零变更预演（只读）：把迁移里 `_brm_todo` 的判重谓词原样拿到生产跑。

矩阵直接从 V0258 up.sql 解析（唯一真相源，避免第二份）。
输出：生产上会新增多少 (角色,码) 与多少授权行（期望 0 / 0）。

用法：
  python3 role_matrix_gate.py --migration <path/to/V0258.up.sql> --remote root@172.18.10.144 --db cloudpivot
  python3 role_matrix_gate.py --migration ... --dsn postgres://...        # 任意库（装配库自检）
"""
import argparse, json, re, subprocess, sys


def parse_matrix(path):
    sql = open(path, encoding='utf-8').read()
    block = sql.split('INSERT INTO _brm_matrix (role_code, auth_code) VALUES', 1)[1]
    block = block.split(';', 1)[0]
    pairs = re.findall(r"\('([A-Z_0-9]+)',\s*'([a-z0-9_:.-]+)'\)", block)
    if not pairs:
        raise SystemExit('矩阵解析失败：没从迁移文件里读到任何 (role, code)')
    return pairs


QUERY = """
WITH matrix (role_code, auth_code) AS (VALUES {VALUES})
SELECT
  (SELECT count(*) FROM matrix) AS matrix_pairs,
  (SELECT count(*) FROM matrix mx
     JOIN sys_role r ON r.role_code = mx.role_code AND r.deleted_at IS NULL
    WHERE NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                        JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
                       WHERE rm.role_id = r.id AND m.auth_code = mx.auth_code)) AS missing_pairs,
  (SELECT count(*) FROM matrix mx
    WHERE NOT EXISTS (SELECT 1 FROM sys_menu m
                       WHERE m.auth_code = mx.auth_code AND m.deleted_at IS NULL)) AS missing_codes,
  (SELECT count(*) FROM matrix mx
    WHERE NOT EXISTS (SELECT 1 FROM sys_role r
                       WHERE r.role_code = mx.role_code AND r.deleted_at IS NULL)) AS missing_roles;
"""

FIVE = """
SELECT m.auth_code || ' -> ' || coalesce(string_agg(r.role_code, ',' ORDER BY r.role_code), '<none>')
  FROM sys_menu m
  LEFT JOIN sys_role_menu rm ON rm.menu_id = m.id
  LEFT JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
 WHERE m.deleted_at IS NULL
   AND m.auth_code IN ('business:erp:period:remind','business:erp:voucher:post',
                       'business:erp:voucher:create','business:erp:stock-in:status',
                       'business:erp:transfer:status')
 GROUP BY m.auth_code ORDER BY 1;
"""


def run(dsn=None, remote=None, db=None, sql=None):
    if remote:
        cmd = ['ssh', '-o', 'BatchMode=yes', remote, f"su - postgres -c 'psql -d {db} -At -F \"|\" -f -'"]
        r = subprocess.run(cmd, input=sql, capture_output=True, text=True)
    else:
        if not dsn:
            raise SystemExit('必须给 --dsn 或 --remote/--db')
        r = subprocess.run(['psql', dsn, '-At', '-F', '|', '-f', '-'], input=sql, capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit('查询失败: ' + r.stderr[:500])
    return r.stdout.strip()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--migration', required=True)
    ap.add_argument('--dsn')
    ap.add_argument('--remote')
    ap.add_argument('--db')
    ap.add_argument('--json', action='store_true')
    a = ap.parse_args()

    pairs = parse_matrix(a.migration)
    values = ",".join(f"('{r}','{c}')" for r, c in pairs)
    out = run(dsn=a.dsn, remote=a.remote, db=a.db, sql=QUERY.replace('{VALUES}', values)).splitlines()[-1]
    matrix_pairs, missing_pairs, missing_codes, missing_roles = (int(x) for x in out.split('|'))
    five = run(dsn=a.dsn, remote=a.remote, db=a.db, sql=FIVE).splitlines()

    rc = 0
    if missing_roles or missing_codes:
        rc = 2                                    # 矩阵本身失效（角色码 / 权限码不存在）
    elif missing_pairs:
        rc = 1                                    # 库里缺授权（装配库未应用 / 新库未补齐）

    result = {'matrix_pairs': matrix_pairs, 'missing_pairs': missing_pairs,
              'missing_codes': missing_codes, 'missing_roles': missing_roles,
              'five_codes': five, 'exit': rc}
    if a.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        print(f"矩阵对: {matrix_pairs}  缺授权对: {missing_pairs}  缺权限码: {missing_codes}  缺角色: {missing_roles}")
        print("5 个验收码现状:")
        for line in five:
            print("  " + line)
        print({"0": "OK（若目标库是生产 ⇒ 执行本迁移零变更）",
               "1": "库里缺授权：装配库需应用 V0258（或该库早于此迁移）",
               "2": "矩阵失效：角色码或权限码在库里不存在，必须修迁移"}[str(rc)])
    sys.exit(rc)


if __name__ == '__main__':
    main()
