#!/usr/bin/env python3
"""B4 隔离实例 e2e（副本库 cloudpivot_deadmin，v2 已发布）。

判据一律以 `biz_flow_task`（引擎真正落库的待办）为准 —— 那才是「谁收到了这一票」。
  1) payable_pay 20 万 → n_board：会签候选 = 财务总监(临时夹具) + 总经理(henry.liao)，两条 PENDING；
     批一条不结束、两条都批才过
  2) 删掉财务总监夹具 → 同一节点只剩总经理一档（**不落 FALLBACK_ADMIN**，按 architect 澄清）
  3) 空角色兜底对照：n_fin 的 FINANCE_MANAGER 0 成员 → assignee_source=ADMIN（FALLBACK_ADMIN 落库形态）
  4) stock_transfer → n_approve 审批人是 WAREHOUSE_MANAGER（不是超管全员）
"""
import json, subprocess, urllib.request, urllib.error, sys, pathlib

BASE = "http://127.0.0.1:18096/api"
DB = "cloudpivot_deadmin"
OUT = pathlib.Path("/opt/cloudpivot/docs/evidence/20261105-p0fix2b-deadmin")
PW = "Admin@123456"
FD_USER = "qa.fd_p0fix2b"
FD_UUID = "20000000-0000-4000-8000-0000002bf001"
lines, fails = [], []


def say(msg):
    print(msg)
    lines.append(msg)


def psql(sql):
    r = subprocess.run(["psql", "-h", "127.0.0.1", "-U", "e2erole", "-d", DB, "-X", "-q", "-t", "-A", "-F", "|", "-c", sql],
                       capture_output=True, text=True, env={"PGPASSWORD": "e2eiso_2026", "PATH": "/usr/bin:/bin"})
    if r.returncode != 0:
        raise SystemExit(f"SQL 失败: {r.stderr}\n{sql}")
    return r.stdout.strip()


def rows(sql):
    out = psql(sql)
    return [line.split("|") for line in out.splitlines() if line] if out else []


def call(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return r.status, json.loads(r.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw or "{}")
        except Exception:
            return e.code, {"raw": raw}


def login(user, pw=PW):
    st, res = call("POST", "/system/auth/login", {"username": user, "password": pw})
    assert st == 200, f"{user} 登录失败 {st} {res}"
    return res["data"]["access_token"]


admin = None


def start(flow, title, amount=None):
    body = {"flow_code": flow, "title": title}
    if amount is not None:
        body["form_data"] = {"amount": amount}
    st, res = call("POST", "/business/flow/instance", body, admin)
    assert st == 200, (st, res)
    return res["data"]["instance_id"]


def tasks_of(iid):
    return rows(f"""select t.id, t.node_key, t.status, coalesce(u.username,'?'), t.assignee_source,
                           coalesce(r.role_code,'-')
                      from biz_flow_task t left join sys_user u on u.id=t.assignee_id
                      left join sys_role r on r.id=t.assignee_source_ref
                     where t.instance_id='{iid}' order by t.created_at, t.id""")


def inst(iid):
    r = rows(f"select instance_no, status, version_no from biz_flow_instance where id='{iid}'")
    return r[0] if r else None


# ---------- 0) 夹具 ----------
say("## 0) 夹具（只在副本库 cloudpivot_deadmin，生产一个字节不动）")
admin_hash = pathlib.Path("/tmp/adminhash.txt").read_text().strip()
# 夹具账号按固定 uuid 复用：上一次 e2e 收尾会软删它，这里先复活再补角色
psql(f"""update sys_user set deleted_at=null, status=1, user_type='HUMAN', password='{admin_hash}',
                is_locked=false, lock_time=null, login_fail_count=0, must_change_password=false
          where id='{FD_UUID}'::uuid""")
psql(f"""insert into sys_user (id, username, password, real_name, status, must_change_password, user_type, dept_id)
         select '{FD_UUID}'::uuid, '{FD_USER}', '{admin_hash}', '财务总监夹具(P0-FIX-2B e2e)', 1, false, 'HUMAN',
                (select id from sys_dept where dept_code='TECH')
          where not exists (select 1 from sys_user where id='{FD_UUID}'::uuid)""")
psql(f"""insert into sys_user_role (user_id, role_id)
         select '{FD_UUID}'::uuid, r.id from sys_role r where r.role_code='FINANCE_DIRECTOR'
           and not exists (select 1 from sys_user_role x where x.user_id='{FD_UUID}'::uuid and x.role_id=r.id)""")
say(f"- 新建夹具账号 {FD_USER}（FINANCE_DIRECTOR 成员、非超管；**仅副本库**）")
# 副本库早前为登录取数把 laoliu_test 打开过，这里复位成 prod 口径（status=0 = 停用，不参与解析）
psql("update sys_user set status=0 where username='laoliu_test'")
say("- 角色在用成员数：")
for code, n, members in rows("""select r.role_code, count(u.id), coalesce(string_agg(u.username, ',' order by u.username), '（0 人）')
      from sys_role r left join sys_user_role ur on ur.role_id=r.id
      left join sys_user u on u.id=ur.user_id and u.deleted_at is null and u.status=1
     where r.role_code in ('SUPER_ADMIN','GENERAL_MANAGER','FINANCE_DIRECTOR','FINANCE_MANAGER','WAREHOUSE_MANAGER','DEPT_LEADER')
     group by 1 order by 1"""):
    say(f"   · {code:18s} {n} 人  {members}")

admin = login("admin")

# ---------- 1) 会签两人 ----------
say("\n## 1) payable_pay 20 万 → n_board 双角色会签（财务总监 + 总经理）")
iid = start("payable_pay", "P0FIX2B 会签对照（20 万）", 200000)
no, status, vno = inst(iid)
ts = tasks_of(iid)
say(f"- 实例 {no}（v{vno}，status={status}），biz_flow_task：")
for t in ts:
    say(f"   · {t[1]:10s} {t[2]:8s} → {t[3]:18s} source={t[4]} ref_role={t[5]}")
pending = [t for t in ts if t[2] == "PENDING"]
ok1 = len(pending) == 2 and {t[3] for t in pending} == {"henry.liao", FD_USER}
say(f"  [{'PASS' if ok1 else 'FAIL'}] 会签候选 = {sorted(t[3] for t in pending)}（2 人；不是 4 个超管会签）")
if not ok1:
    fails.append("会签候选不对")

tok_fd = login(FD_USER)
fd_task = next(t for t in pending if t[3] == FD_USER)
st, res = call("POST", f"/business/flow/task/{fd_task[0]}/approve", {"comment": "财务总监同意（P0-FIX-2B-B e2e）"}, tok_fd)
say(f"- 财务总监批一票：HTTP {st} code={res.get('code')} {res.get('msg','')} 已批={res.get('data',{}).get('approved_count')}/应到={res.get('data',{}).get('approver_count')}")
p2 = [t for t in tasks_of(iid) if t[2] == "PENDING"]
ok2 = inst(iid)[1] == "PENDING" and len(p2) == 1
say(f"  [{'PASS' if ok2 else 'FAIL'}] 批一票后实例 {inst(iid)[1]}，仍待办 {len(p2)} 条 → 会签没齐不结束")
if not ok2:
    fails.append("会签未齐即结束")

tok_hl = login("henry.liao")
st, res = call("POST", f"/business/flow/task/{p2[0][0]}/approve", {"comment": "总经理同意（P0-FIX-2B-B e2e）"}, tok_hl)
say(f"- 总经理批第二票：HTTP {st} code={res.get('code')} {res.get('msg','')}")
ok3 = inst(iid)[1] == "APPROVED"
say(f"  [{'PASS' if ok3 else 'FAIL'}] 两票齐 → 实例 {inst(iid)[1]}")
if not ok3:
    fails.append("两票齐了没过")

# ---------- 2) 财务总监空缺 ----------
say("\n## 2) 删掉财务总监夹具 → 同一节点只剩总经理一档（**不落 FALLBACK_ADMIN**）")
psql(f"delete from sys_user_role where user_id='{FD_UUID}'::uuid "
     f"and role_id=(select id from sys_role where role_code='FINANCE_DIRECTOR')")
psql(f"update sys_user set status=0 where id='{FD_UUID}'::uuid")
iid2 = start("payable_pay", "P0FIX2B 财务总监空缺对照（20 万）", 200000)
ts2 = [t for t in tasks_of(iid2) if t[2] == "PENDING"]
for t in ts2:
    say(f"   · {t[1]:10s} {t[2]:8s} → {t[3]:18s} source={t[4]} ref_role={t[5]}")
ok4 = len(ts2) == 1 and ts2[0][3] == "henry.liao" and ts2[0][4] == "ROLE"
say(f"  [{'PASS' if ok4 else 'FAIL'}] 候选 = {[t[3] for t in ts2]}，source={[t[4] for t in ts2]}"
    f" → 只剩总经理档，**没有**回退成超管/FALLBACK")
if not ok4:
    fails.append("财务总监空缺时的落点不对")
call("POST", f"/business/flow/instance/{iid2}/withdraw", {"comment": "e2e 收尾"}, admin)

# ---------- 3) 空角色兜底 ----------
say("\n## 3) 空角色兜底对照：n_fin 的 FINANCE_MANAGER 0 成员 → FALLBACK_ADMIN（assignee_source=ADMIN）")
say("- 兜底前 FINANCE_MANAGER 在用成员数 = " + psql(
    "select count(*) from sys_user_role ur join sys_role r on r.id=ur.role_id join sys_user u on u.id=ur.user_id "
    "where r.role_code='FINANCE_MANAGER' and u.deleted_at is null and u.status=1"))
iid3 = start("payable_pay", "P0FIX2B 财务经理空角色兜底（5 万）", 50000)
ts3 = [t for t in tasks_of(iid3) if t[2] == "PENDING"]
for t in ts3:
    say(f"   · {t[1]:10s} {t[2]:8s} → {t[3]:18s} source={t[4]} ref_role={t[5]}")
ok5 = bool(ts3) and all(t[1] == "n_fin" and t[4] == "ADMIN" for t in ts3)
say(f"  [{'PASS' if ok5 else 'FAIL'}] 节点 n_fin 角色 0 人 → 兜底派给超管（source=ADMIN），**不是 SKIP 自动通过**")
if not ok5:
    fails.append("空角色兜底不对")
call("POST", f"/business/flow/instance/{iid3}/withdraw", {"comment": "e2e 收尾"}, admin)

# ---------- 4) stock_transfer ----------
say("\n## 4) stock_transfer → n_approve 审批人 = WAREHOUSE_MANAGER")
say("- 仓管主管在用成员：" + psql(
    "select coalesce(string_agg(u.username, ','), '（0 人）') from sys_user_role ur join sys_role r on r.id=ur.role_id "
    "join sys_user u on u.id=ur.user_id where r.role_code='WAREHOUSE_MANAGER' and u.deleted_at is null and u.status=1"))
iid4 = start("stock_transfer", "P0FIX2B 调拨审批（仓管主管）", None)
ts4 = [t for t in tasks_of(iid4) if t[2] == "PENDING"]
for t in ts4:
    say(f"   · {t[1]:10s} {t[2]:8s} → {t[3]:18s} source={t[4]} ref_role={t[5]}")
ok6 = len(ts4) == 1 and ts4[0][1] == "n_approve" and ts4[0][5] == "WAREHOUSE_MANAGER"
if ok6:
    st, res = call("POST", f"/business/flow/task/{ts4[0][0]}/approve", {"comment": "仓管主管同意（P0-FIX-2B-B e2e）"}, tok_hl)
    ok6 = st == 200 and inst(iid4)[1] == "APPROVED"
    say(f"- 仓管主管审批：HTTP {st}，实例 {inst(iid4)[1]}")
say(f"  [{'PASS' if ok6 else 'FAIL'}] 审批人来自 WAREHOUSE_MANAGER 角色且链路走通")
if not ok6:
    fails.append("stock_transfer 不符合预期")

# 收尾：软删夹具账号（副本库；硬删会被 biz_flow_task 外键拦住，且历史待办要留痕）
psql(f"delete from sys_user_role where user_id='{FD_UUID}'::uuid")
psql(f"update sys_user set deleted_at=now(), status=0 where id='{FD_UUID}'::uuid and username='{FD_USER}'")
say("\n- 已删除夹具账号（副本库不留成员归属）")

say(f"\n==== FAIL={len(fails)} ====")
for f in fails:
    say(f"  ✗ {f}")
(OUT / "b4_e2e.txt").write_text("\n".join(lines) + "\n")
print("evidence:", OUT / "b4_e2e.txt")
sys.exit(1 if fails else 0)
