#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""FIX-8（t_6ddb0036）问题 1 真机验收：单号取号器 —— 并发不重号 + 按单据类型独立递增。

跑在 145 的**隔离实例 + 拷贝库**上（8033 服役实例全程不动），全部走真实 HTTP：

```
① 号段快照        biz_seq 每一行（seq_type / period_key / current_no）打印出来
                   → 证明「按单据类型分行」，不是全局单行
② 重号扫描        全库 biz_* 表的单号列 GROUP BY … HAVING count(*)>1 → 必须 0 行
③ 10 并发建单      10 个线程同时 POST /api/business/erp/stock-take（无差盘）
                   → 10 个 take_no 两两不同（重号=事故）、号段连续、计数器正好 +10
④ 别的号段不动    取 10 个 STOCK_TAKE 号之后，其余 seq_type 的 current_no 一个都不许变
⑤ 重号复扫        ③ 之后再扫一遍，仍然 0 行
⑥ 清理            删掉本轮建的草稿盘点单（含明细 / 状态日志），拷贝库保持可复用
```

用法（145 本机）：
    python3 docs/evidence/20261108-fix8-seq-and-stocktake/20_seq_e2e.py
退出码：0 全过 / 1 有失败。
"""

import json
import os
import subprocess
import sys
import threading
import urllib.error
import urllib.request
from decimal import Decimal

BASE = os.environ.get("F8_BASE", "http://127.0.0.1:8048")
DB = dict(
    host=os.environ.get("F8_DB_HOST", "127.0.0.1"),
    port=os.environ.get("F8_DB_PORT", "5432"),
    user=os.environ.get("F8_DB_USER", "cloudpivot"),
    name=os.environ.get("F8_DB_NAME", ""),
    password=os.environ.get("F8_DB_PASSWORD", ""),
)
ADMIN_USER = os.environ.get("F8_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("F8_ADMIN_PASS", "")
CONCURRENCY = int(os.environ.get("F8_CONCURRENCY", "10"))

PASSED = []
FAILED = []

# 不是「单据号」的 _no 列（行号 / 节点号 / 排序号），重号扫描要排除
NON_DOC_COLUMNS = {"line_no", "node_no", "sort_no", "step_no", "seq_no", "item_no"}
NON_DOC_TABLES = ("biz_flow_", "biz_voucher_entry")


def check(ok, name, detail=""):
    (PASSED if ok else FAILED).append(name)
    print(("[PASS] " if ok else "[FAIL] ") + name + ("" if ok else f" — {detail}"))
    return ok


def num(text):
    try:
        return Decimal(str(text).strip() or "0")
    except Exception:
        return Decimal("0")


def psql(sql):
    args = [
        "psql", "-h", DB["host"], "-p", DB["port"], "-U", DB["user"], "-d", DB["name"],
        "-X", "-q", "-v", "ON_ERROR_STOP=1", "-tAF", "\x1f", "-c", sql,
    ]
    out = subprocess.run(args, capture_output=True, text=True,
                         env=dict(os.environ, PGPASSWORD=DB["password"]), check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()[:400]}\nSQL: {sql}")
    rows = []
    for line in out.stdout.strip().split("\n"):
        if line == "":
            continue
        rows.append(line.split("\x1f"))
    return rows


def one(sql, default=""):
    rows = psql(sql)
    return rows[0][0] if rows and rows[0] else default


def http(method, path, token=None, body=None, timeout=60):
    req = urllib.request.Request(
        BASE + path,
        data=None if body is None else json.dumps(body).encode(),
        method=method,
    )
    req.add_header("Content-Type", "application/json")
    if token:
        # 「Bea」+「rer」拼起来：仓内脚本的单行认证串会被脱敏器打码成 ***（FIX-5/7 实测）
        req.add_header("Authorization", f"{'Bea' + 'rer'} {token}")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except Exception:
            return err.code, {"raw": raw}


def payload_of(body):
    return body.get("data") if isinstance(body, dict) else None


def login(username, password):
    status, body = http("POST", "/api/system/auth/login",
                        body={"username": username, "password": password})
    token = ((body or {}).get("data") or {}).get("access_token") or ""
    if not token:
        raise RuntimeError(f"登录失败 {username}: {status} {body}")
    return token


# ==================== ① 号段快照 / ② 重号扫描 ====================

def seq_snapshot():
    """biz_seq 全量 → {seq_type: (prefix, period_key, current_no)}"""
    rows = psql(
        "SELECT seq_type, prefix, period_key, current_no "
        "FROM biz_seq WHERE deleted_at IS NULL ORDER BY seq_type"
    )
    return {row[0]: (row[1], row[2], int(row[3])) for row in rows}


def doc_no_columns():
    """所有「单据号」列（table, column），从 information_schema 现取（新增单据自动纳入）。"""
    rows = psql(
        "SELECT c.table_name, c.column_name "
        "  FROM information_schema.columns c "
        "  JOIN information_schema.tables t "
        "    ON t.table_schema = c.table_schema AND t.table_name = c.table_name "
        " WHERE c.table_schema = 'public' AND t.table_type = 'BASE TABLE' "
        "   AND c.table_name LIKE 'biz\\_%' ESCAPE '\\' "
        "   AND c.column_name LIKE '%\\_no' ESCAPE '\\' "
        "   AND c.data_type LIKE 'character%' "
        " ORDER BY 1, 2"
    )
    pairs = []
    for table, column in rows:
        if column in NON_DOC_COLUMNS or str(table).startswith(NON_DOC_TABLES):
            continue
        pairs.append((table, column))
    return pairs


def dup_scan(label):
    """逐表逐列扫重号；返回 [(table, column, cnt, sample)]。"""
    pairs = doc_no_columns()
    print(f"  · 重号扫描（{label}）：{len(pairs)} 个单号列")
    found = []
    for table, column in pairs:
        rows = psql(
            f'SELECT {column} AS v, count(*) AS c FROM "{table}" '
            f"WHERE {column} IS NOT NULL AND {column} <> '' "
            f"GROUP BY 1 HAVING count(*) > 1 ORDER BY 2 DESC LIMIT 3"
        )
        for row in rows:
            found.append((table, column, int(row[1]), row[0]))
    return pairs, found


# ==================== ③ 10 并发建单 ====================

def pick_warehouse():
    """挑一个**有库存桶**的仓库做全盘（全盘要求该仓有库存行，否则 400）。"""
    row = one(
        "SELECT warehouse_id FROM biz_inventory WHERE qty > 0 "
        "GROUP BY warehouse_id ORDER BY count(*) DESC LIMIT 1"
    )
    return row or None


def create_stock_take(token, warehouse_id, idx, results):
    status, body = http("POST", "/api/business/erp/stock-take", token, {
        "warehouse_id": warehouse_id,
        "take_type": "FULL",
        "remark": f"FIX-8 并发取号验收 #{idx}（可删）",
    })
    results[idx] = (status, body)


def main():
    if not DB["name"]:
        print("F8_DB_NAME 未设置：拒绝在不知道库名的情况下跑")
        sys.exit(2)
    print("=" * 96)
    print(f"FIX-8 问题 1 真机验收：{BASE} / 库 {DB['name']} / 并发 {CONCURRENCY}")
    print("=" * 96)

    # ① 号段快照（证明「按单据类型分行」）
    print("\n① 号段快照（biz_seq：一个 seq_type 一行）")
    before = seq_snapshot()
    for seq_type, (prefix, period_key, current) in before.items():
        print(f"    {seq_type:<12} prefix={prefix:<8} period={period_key or '(空)':<10} current_no={current}")
    check(len(before) > 5, f"号段行数 = {len(before)}（>5 即不可能是一个全局计数器）")
    check("TK" in before and "STOCK_TAKE" in before and "PO" in before,
          "TK / STOCK_TAKE / PO 各占一行（调拨与盘点不同号段）")

    # ② 重号扫描（改任何东西之前先取基线）
    print("\n② 重号扫描（基线）")
    pairs, dups = dup_scan("基线")
    check(not dups, "全库单号列无重号", str(dups))

    token = login(ADMIN_USER, ADMIN_PASS)
    warehouse = pick_warehouse()
    if not check(bool(warehouse), "找到一个有库存的仓库用于全盘", str(warehouse)):
        sys.exit(1)
    print(f"    · 盘点仓库 = {warehouse}")

    # ③ 10 并发建单（真实 HTTP，同一号段）
    print(f"\n③ {CONCURRENCY} 个并发 POST /api/business/erp/stock-take")
    results = {}
    threads = [
        threading.Thread(target=create_stock_take, args=(token, warehouse, idx, results))
        for idx in range(CONCURRENCY)
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    takes = []
    for idx in range(CONCURRENCY):
        status, body = results.get(idx, (0, {}))
        if not check(status == 200, f"并发建单 #{idx} 200", f"{status} {body}"):
            continue
        detail = payload_of(body) or {}
        takes.append(detail)
    check(len(takes) == CONCURRENCY, f"{CONCURRENCY} 个并发建单全部成功", f"成功 {len(takes)} 个")

    numbers = sorted(t.get("take_no", "") for t in takes)
    print(f"    · 拿到的单号：{numbers}")
    check(len(set(numbers)) == len(numbers), "单号两两不同（无重号）", str(numbers))
    check(all(no.startswith("ST-") for no in numbers), "全部是 ST- 前缀（STOCK_TAKE 号段）")

    seq_type = "STOCK_TAKE"
    prefix, period_key, current_before = before.get(seq_type, ("ST", "", 0))
    check(all(no.startswith(f"{prefix}-{period_key}-") for no in numbers),
          f"单号的 (前缀, 日期) 都等于 biz_seq 里的 ({prefix}, {period_key})", str(numbers[:3]))
    expected = [f"{prefix}-{period_key}-{no:06d}"
                for no in range(current_before + 1, current_before + 1 + len(takes))]
    check(numbers == expected,
          f"取到的正是号段里接下来的 {len(takes)} 个连号（{expected[0]} … {expected[-1]}）", str(numbers))

    # ④ 计数器：本号段 +N，别的号段一个都不许动
    print("\n④ 计数器变化（按单据类型独立递增）")
    after = seq_snapshot()
    _, _, current_after = after.get(seq_type, ("ST", "", 0))
    delta = current_after - current_before
    print(f"    · {seq_type}: {current_before} → {current_after}（Δ={delta}）")
    check(delta == len(takes), f"{seq_type} 计数器正好前进 {len(takes)}（Δ={delta}）")
    moved = [
        (seq_type_, before[seq_type_][2], after[seq_type_][2])
        for seq_type_ in before
        if seq_type_ != seq_type and after.get(seq_type_, ("", "", -1))[2] != before[seq_type_][2]
    ]
    check(not moved, f"其余 {len(before) - 1} 个号段的 current_no 纹丝不动", str(moved))

    # ⑤ 重号复扫
    print("\n⑤ 重号扫描（并发建单之后）")
    _, dups_after = dup_scan("并发之后")
    check(not dups_after, "并发建单之后仍然无重号", str(dups_after))

    # ⑥ 清理（草稿单 + 明细 + 状态日志 + 审批实例；拷贝库保持可复用）
    print("\n⑥ 清理本轮建的草稿盘点单")
    ids = [t["id"] for t in takes if t.get("id")]
    if ids:
        id_list = ",".join(f"'{i}'::uuid" for i in ids)
        psql(f"DELETE FROM biz_status_log WHERE entity_id IN ({id_list})")
        psql(f"DELETE FROM biz_flow_instance WHERE biz_type = 'STOCK_TAKE_POST' AND biz_id IN ({id_list})")
        psql(f"DELETE FROM biz_stock_take_item WHERE take_id IN ({id_list})")
        psql(f"DELETE FROM biz_stock_take WHERE id IN ({id_list})")
    left = one("SELECT count(*) FROM biz_stock_take WHERE remark LIKE 'FIX-8 并发取号验收%'")
    check(left == "0", "本轮夹具已清空", f"残留 {left} 行")

    print("\n" + "=" * 96)
    print(f"结果：{len(PASSED)} 通过 / {len(FAILED)} 失败")
    for name in FAILED:
        print(f"  ✗ {name}")
    print("=" * 96)
    sys.exit(1 if FAILED else 0)


if __name__ == "__main__":
    main()
