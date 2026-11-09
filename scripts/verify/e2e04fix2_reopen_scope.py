#!/usr/bin/env python3
"""E2E04-FIX-2 真机验收：反结账「后续期间」判据收敛到同一会计年度（145 dev 实例）。

卡面验收：
  ① 同一会计年度内后续期间已结账 → 反结账被拦（原有行为）
  ② 其他年度有更晚的已结账期间 → **不再影响**本年度反结账
  ③ 报错文案区分「同年度挡路」和「跨年度挡路」
  ④ 跨年度（下年已年结）→ 提示先反年结

用法（在 145 上跑）：
    python3 e2e04fix2_reopen_scope.py before   # 旧二进制：证明 ② 的病
    python3 e2e04fix2_reopen_scope.py after    # 新二进制：跑全部四条 + 收尾

为什么用 2027 / 2028 / 2029 而不是卡面的 2022：
  145 dev 库的 2022 年度**真的**年结过（`ACCOUNTING_YEAR=2022 CLOSED`，E2E-04 演练留下的），
  所以 2022 任意月份反结账会先撞「本年度已年结」那道闸，测不到本卡的判据。
  2027 / 2028 是只有月份期间、没有年结状态的年度；2029 由本脚本用 batch-open 现开，
  用完即删（`period_year=2029` 的期间行 + 状态日志全部收干净）。

不碰 144 生产 / 不 DROP / 不 TRUNCATE；只动 2027-01 / 2027-02 / 2029-02 三个期间的
OPEN/CLOSED 状态，并在结尾按原状态还原（`assert_state_restored`）。
"""

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

ENV_FILE = "/etc/cloudpivot/cloudpivot-dev.env"
BASE = "http://127.0.0.1:8033/api"
PHASE = (sys.argv[1] if len(sys.argv) > 1 else "after").lower()
REASON = "E2E04-FIX-2 真机验收：反结账后续期间判据收敛到同一会计年度"

results = []


def load_env(path=ENV_FILE):
    env = {}
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            env[key.strip()] = value.strip()
    return env


ENV = load_env()
PG = {
    "PGPASSWORD": ENV["NIK_DATABASE_PASSWORD"],
    "PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"),
}


def sql(query):
    """跑一条 SQL，返回 [[col, col, ...], ...]（`|` 分隔、无表头）。"""
    out = subprocess.run(
        [
            "psql",
            "-h", "127.0.0.1",
            "-U", ENV["NIK_DATABASE_USER"],
            "-d", ENV["NIK_DATABASE_NAME"],
            "-At", "-F", "|", "-P", "pager=off",
            "-c", query,
        ],
        capture_output=True,
        text=True,
        env=PG,
        check=True,
    )
    return [line.split("|") for line in out.stdout.strip().splitlines() if line]


def scalar(query, default=None):
    rows = sql(query)
    return rows[0][0] if rows else default


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(("[PASS] " if ok else "[FAIL] ") + name + (("  ← " + detail) if detail else ""))


def login():
    payload = json.dumps(
        {"username": "admin", "password": ENV["NIK_ADMIN_PASSWORD"]}
    ).encode()
    req = urllib.request.Request(
        BASE + "/system/auth/login",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        body = json.loads(resp.read())
    token = (body.get("data") or {}).get("access_token")
    if not token:
        raise SystemExit("FATAL: admin 登录失败：%s" % json.dumps(body, ensure_ascii=False)[:300])
    return token


TOKEN = None


def call(method, path, body=None):
    """返回 (http_status, json_dict)。"""
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        BASE + path,
        data=data,
        headers={
            "Content-Type": "application/json",
            "Authorization": "Bearer " + TOKEN,
        },
        method=method,
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as err:
        raw = err.read()
        try:
            return err.code, json.loads(raw or b"{}")
        except json.JSONDecodeError:
            return err.code, {"raw": raw.decode("utf-8", "replace")}


def msg_of(body):
    return str((body or {}).get("msg") or (body or {}).get("message") or "")


def period_id(code):
    pid = scalar("SELECT id FROM biz_accounting_period WHERE period_code = '%s'" % code)
    return pid or ""


def status_of(code):
    return scalar("SELECT status FROM biz_accounting_period WHERE period_code = '%s'" % code)


def close_period(code):
    code_id = period_id(code)
    status, body = call("POST", "/business/erp/period/%s/close" % code_id, {"reason": REASON})
    print("  close  %s -> HTTP %s  %s" % (code, status, msg_of(body)[:120]))
    return status, body


def reopen_period(code):
    code_id = period_id(code)
    status, body = call("POST", "/business/erp/period/%s/reopen" % code_id, {"reason": REASON})
    print("  reopen %s -> HTTP %s  %s" % (code, status, msg_of(body)[:200]))
    return status, body


# ---------------- 夹具 ----------------

OWN_YEAR_TARGET = "2027-01"      # 反结账目标（本该是空的期间）
OWN_YEAR_LATER = "2027-02"       # 同年度后续期间（用来制造「同年度挡路」）
OTHER_YEAR_LATER = "2029-02"     # 其它年度更晚的已结账期间（用来证明 ②）
CROSS_YEAR_FIXTURE = "2029"      # 只用于「更晚年度已年结」的夹具日志
FIXTURE_YEARS = ("2029",)


def ensure_fixture_periods():
    """2029-01 / 2029-02 由本脚本 batch-open 现开（幂等）。"""
    status, body = call("POST", "/business/erp/period/batch-open",
                        {"from": "2029-01", "to": "2029-02", "remark": "E2E04-FIX-2 夹具（用完即删）"})
    created = ((body.get("data") or {}).get("created_count"))
    print("  batch-open 2029-01..2029-02 -> HTTP %s created=%s %s"
          % (status, created, msg_of(body)[:120]))
    check("夹具：2029-01/2029-02 已开账",
          period_id("2029-01") != "" and period_id("2029-02") != "",
          "2029-01=%s 2029-02=%s" % (status_of("2029-01"), status_of("2029-02")))


def drop_fixture_periods():
    sql("DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' AND entity_no LIKE '2029-%'")
    sql("DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_YEAR' AND entity_no IN ('%s')"
        % "','".join(FIXTURE_YEARS))
    sql("DELETE FROM biz_accounting_period WHERE period_year = 2029")
    left = scalar("SELECT count(*) FROM biz_accounting_period WHERE period_year = 2029")
    logs = scalar("SELECT count(*) FROM biz_status_log WHERE entity_no LIKE '2029%'")
    check("收尾：2029 夹具期间与状态日志已清空", left == "0" and logs == "0",
          "period_rows=%s logs=%s" % (left, logs))


def restore_open(codes):
    """把 codes 里仍是 CLOSED 的期间按「晚 → 早」反结账（旧代码下也安全）。"""
    for code in sorted(codes, reverse=True):
        if status_of(code) == "CLOSED":
            reopen_period(code)


def snapshot():
    rows = sql(
        "SELECT period_code, status FROM biz_accounting_period "
        "WHERE period_year IN (2021, 2022, 2026, 2027, 2028, 2029) "
        "AND (period_year <> 2021 OR period_month >= 10) ORDER BY period_code"
    )
    return {code: status for code, status in rows}


def year_end_log_count(year):
    return scalar(
        "SELECT count(*) FROM biz_status_log WHERE entity_type = 'ACCOUNTING_YEAR' AND entity_no = '%s'"
        % year
    )


# ---------------- 两个阶段 ----------------

def phase_before():
    """旧二进制：证明 ② 的病 —— 别的年度更晚的已结账期间把本年度反结账卡死。"""
    print("\n== [before] 旧二进制：全库无界的后续期间查询")
    ensure_fixture_periods()
    restore_open([OWN_YEAR_TARGET, OWN_YEAR_LATER, OTHER_YEAR_LATER])

    close_period(OWN_YEAR_TARGET)
    close_period(OTHER_YEAR_LATER)
    check("前置：2027-01 / 2029-02 都已结账",
          status_of(OWN_YEAR_TARGET) == "CLOSED" and status_of(OTHER_YEAR_LATER) == "CLOSED")

    status, body = reopen_period(OWN_YEAR_TARGET)
    text = msg_of(body)
    check("②（旧行为）其它年度更晚的已结账期间把 2027-01 反结账卡死 → 409", status == 409, text[:160])
    check("②（旧行为）409 文案点名的是 2029-02（跨年度误拦）", "2029-02" in text, text[:160])

    # 还原：先撤更晚的，再撤目标
    reopen_period(OTHER_YEAR_LATER)
    reopen_period(OWN_YEAR_TARGET)
    check("还原：2027-01 / 2029-02 回到 OPEN",
          status_of(OWN_YEAR_TARGET) == "OPEN" and status_of(OTHER_YEAR_LATER) == "OPEN")
    drop_fixture_periods()


def phase_after():
    """新二进制：四条验收 + 真实年结年度（2021 vs 2022）作对照。"""
    print("\n== [after] 新二进制：判据收敛到同一会计年度")
    before_state = snapshot()
    print("  起始状态（2021-10..12 / 2022 / 2026 / 2027 / 2028）: %s" % before_state)
    check("环境前提：2026-09 是别的卡留下的 CLOSED（本卡修的就是它不该挡路）",
          status_of("2026-09") == "CLOSED", "2026-09=%s" % status_of("2026-09"))
    check("环境前提：2022 年度是真的已年结（ACCOUNTING_YEAR=CLOSED）",
          scalar("SELECT to_status FROM biz_status_log WHERE entity_type='ACCOUNTING_YEAR' "
                 "AND entity_no='2022' ORDER BY created_at DESC, id DESC LIMIT 1") == "CLOSED")

    ensure_fixture_periods()
    restore_open([OWN_YEAR_TARGET, OWN_YEAR_LATER, OTHER_YEAR_LATER])

    # ---------- ② 其它年度更晚的已结账期间不再挡路 ----------
    print("\n-- ② 其它年度更晚的已结账期间 → 不再影响本年度反结账")
    close_period(OWN_YEAR_TARGET)
    close_period(OTHER_YEAR_LATER)
    status, body = reopen_period(OWN_YEAR_TARGET)
    text = msg_of(body)
    check("② 2029-02 已结账（别的年度）时，2027-01 反结账成功", status == 200, text[:160])
    check("② 2027-01 真的回到 OPEN", status_of(OWN_YEAR_TARGET) == "OPEN")
    check("② 别的年度的封账没被顺手动掉：2029-02 仍是 CLOSED", status_of(OTHER_YEAR_LATER) == "CLOSED")
    check("② 2026-09（历史残留）原样保留", status_of("2026-09") == "CLOSED")

    # ---------- ①③ 同年度挡路 ----------
    print("\n-- ①③ 同一会计年度内后续期间已结账 → 拦，且文案标明「同年度挡路」")
    close_period(OWN_YEAR_TARGET)
    close_period(OWN_YEAR_LATER)
    status, body = reopen_period(OWN_YEAR_TARGET)
    text = msg_of(body)
    check("① 同年度后续期间 2027-02 已结账 → 2027-01 反结账 409", status == 409, text[:200])
    check("③ 文案以「不能反结账 2027-01：」开头", text.startswith("不能反结账 2027-01："), text[:60])
    check("③ 文案含「同年度挡路」并点名 2027-02", "同年度挡路" in text and "2027-02" in text, text[:200])
    check("③ 一年内没有年结挡路时不提「跨年度挡路」（2029 未年结）", "跨年度挡路" not in text, text[:200])
    check("① 被拦后 2027-01 仍是 CLOSED", status_of(OWN_YEAR_TARGET) == "CLOSED")

    # 同年度：撤掉挡路的 2027-02 → 2027-01 就能反结
    status, body = reopen_period(OWN_YEAR_LATER)
    check("① 反结账 2027-02（该年度最后封账的月份）成功", status == 200, msg_of(body)[:120])
    status, body = reopen_period(OWN_YEAR_TARGET)
    check("① 后续期间都开了之后，2027-01 反结账成功", status == 200, msg_of(body)[:120])

    # ---------- ④ 跨年度：更晚的年度已年结 ----------
    print("\n-- ④ 更晚的年度已年结 → 拦，且文案标明「跨年度挡路」+ 先反年结")
    sql(
        "INSERT INTO biz_status_log (entity_type, entity_id, entity_no, from_status, to_status, "
        "operator_name, reason) VALUES ('ACCOUNTING_YEAR', "
        "'00000000-0000-7000-8000-00000000f2a1'::uuid, '2029', 'NOT_CLOSED', 'CLOSED', "
        "'e2e04fix2', 'E2E04-FIX-2 夹具：2029 年结（用完即删）')"
    )
    close_period(OWN_YEAR_TARGET)
    status, body = reopen_period(OWN_YEAR_TARGET)
    text = msg_of(body)
    check("④ 2029 年已年结 → 2027-01 反结账 409", status == 409, text[:200])
    check("③ 文案含「跨年度挡路」并点名 2029 年", "跨年度挡路" in text and "2029 年" in text, text[:220])
    check("④ 文案给出处置建议「反年结」", "反年结" in text, text[:220])
    check("④ 这一年（2027）没有同年度挡路，文案里不该出现「同年度挡路」",
          "同年度挡路" not in text, text[:220])
    check("④ 被拦后 2027-01 仍是 CLOSED", status_of(OWN_YEAR_TARGET) == "CLOSED")

    # 拆掉年结夹具 → 立刻可以反结（证明挡路的确实是「年结状态」本身）
    sql("DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_YEAR' AND entity_no = '2029'")
    status, body = reopen_period(OWN_YEAR_TARGET)
    check("④ 拆掉 2029 年结夹具后，2027-01 反结账立即成功（挡路判据就是年结状态）",
          status == 200, msg_of(body)[:120])

    # ---------- 真实数据对照：2021（全封、未年结）vs 2022（真年结） ----------
    print("\n-- 真实数据对照：2021 年全封且未年结，但 2022 年已年结")
    status, body = reopen_period("2021-11")
    text = msg_of(body)
    check("真实数据：2021-11 同时被「同年度 2021-12」与「2022 已年结」挡住 → 409",
          status == 409, text[:240])
    check("真实数据：文案同时给出两类挡路",
          "同年度挡路" in text and "跨年度挡路" in text, text[:240])
    check("真实数据：2021-11 仍为 CLOSED", status_of("2021-11") == "CLOSED")

    # ---------- 收尾 ----------
    print("\n-- 收尾：还原状态 + 清夹具")
    restore_open([OWN_YEAR_TARGET, OWN_YEAR_LATER])
    reopen_period(OTHER_YEAR_LATER)
    drop_fixture_periods()
    sql("DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' "
        "AND entity_no IN ('2027-01', '2027-02')")
    after_state = snapshot()
    touched = {code: state for code, state in after_state.items()
               if code in ("2027-01", "2027-02", "2026-09") or code.startswith("2022-")}
    expect_open = {"2027-01": "OPEN", "2027-02": "OPEN", "2026-09": "CLOSED"}
    check("收尾：2027-01 / 2027-02 回到 OPEN、2026-09 仍是 CLOSED",
          all(after_state.get(code) == want for code, want in expect_open.items()), str(touched))
    check("收尾：2022 年度 12 期仍是 CLOSED（真数据没被动）",
          all(after_state.get("2022-%02d" % m) == "CLOSED" for m in range(1, 13)), str(touched))
    check("收尾：没有留下 2029 的年结状态日志", year_end_log_count("2029") == "0",
          "count=%s" % year_end_log_count("2029"))

    print("\n== 起始/结束状态对比（应完全一致）")
    changed = {code: (before_state.get(code), after_state.get(code))
               for code in set(before_state) | set(after_state)
               if before_state.get(code) != after_state.get(code)}
    check("全量状态快照与开工前一致", not changed, str(changed))


def main():
    global TOKEN
    print("== 145 dev 实例：%s（phase=%s）" % (BASE, PHASE))
    TOKEN = login()
    if PHASE == "before":
        phase_before()
    else:
        phase_after()

    passed = sum(1 for _, ok, _ in results if ok)
    failed = [(name, detail) for name, ok, detail in results if not ok]
    print("\n===== PASS=%d FAIL=%d =====" % (passed, len(failed)))
    for name, detail in failed:
        print("  FAIL: %s  ← %s" % (name, detail[:200]))
    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
