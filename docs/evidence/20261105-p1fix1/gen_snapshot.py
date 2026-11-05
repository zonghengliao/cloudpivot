"""P1-FIX-1：为 stock_in_cancel 生成新快照 + canonical hash（与 graph.rs::canonical_json 同算法）。

用法：python3 gen_snapshot.py <入库角色uuid>
输出：新快照（canonical 序）+ 新 hash；同时用 V0174 原快照自证算法正确（应等于 78d34c…）。
"""
import hashlib
import json
import re
import sys

MIG = "/opt/cloudpivot/backend/migrations/business/V0174__stock_in_cancel_flow.up.sql"


def canonical(value) -> str:
    """递归按键升序 + 紧凑分隔符 + 非 ASCII 原样（对齐 serde_json::to_string）。"""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def hash_of(value) -> str:
    return hashlib.sha256(canonical(value).encode("utf-8")).hexdigest()


def load_seed_snapshot() -> dict:
    text = open(MIG, encoding="utf-8").read()
    m = re.search(r"'(\{\"edges\".*?\})'::jsonb", text, re.S)
    if not m:
        raise SystemExit("找不到 V0174 的快照 jsonb 字面量")
    return json.loads(m.group(1))


def main() -> None:
    role_id = sys.argv[1] if len(sys.argv) > 1 else "20000000-0000-4000-8000-0000000f0004"
    snap = load_seed_snapshot()
    old_hash = hash_of(snap)
    print("== 自证：V0174 种子快照 canonical hash ==")
    print("  计算值:", old_hash)
    print("  迁移里:", "78d34c4557630fd8a57c45f901b427649db06ee380baf1868d257813f6e4e56c")
    print("  一致  :", old_hash == "78d34c4557630fd8a57c45f901b427649db06ee380baf1868d257813f6e4e56c")

    review = [n for n in snap["nodes"] if n["key"] == "n_review"][0]
    print("\n== 改前 n_review ==")
    print("  assignee_type =", review["assignee_type"], "| empty_assignee =", review["empty_assignee"],
          "| self_approval =", review["self_approval"], "| assignees =", review["assignees"])

    review["assignee_type"] = "ROLE"
    review["assignees"] = [{"assignee_type": "ROLE", "ref_id": role_id, "sort_order": 0}]
    review["empty_assignee"] = "ERROR"

    # 快照内数组必须按 canonical 规则排序：nodes 按 key，edges 按 (from,to,key)
    snap["nodes"].sort(key=lambda n: n["key"])
    snap["edges"].sort(key=lambda e: (e["from"], e["to"], e["key"]))

    new_hash = hash_of(snap)
    print("\n== 改后 n_review ==")
    print("  assignee_type =", review["assignee_type"], "| empty_assignee =", review["empty_assignee"],
          "| self_approval =", review["self_approval"], "| assignees =", review["assignees"])
    print("\n== 新 hash ==")
    print(" ", new_hash)
    print("\n== 新快照（canonical，可直接粘进迁移的 $snapshot$ 里）==")
    print(canonical(snap))


if __name__ == "__main__":
    main()
