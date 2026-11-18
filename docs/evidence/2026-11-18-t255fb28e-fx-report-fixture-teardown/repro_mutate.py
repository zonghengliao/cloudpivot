#!/usr/bin/env python3
"""对 fx_report_tests.rs 做单点变异（严格唯一匹配），供反证用。

用法：python3 t255_mutate.py <A|B|restore> <仓库文件路径>
  A: 把「先删 VOUCHER 状态日志」那条腿的定位词改成永不匹配 ⇒ 只有凭证被删、日志留下
  B: 把「删凭证」那条腿的定位词改成永不匹配 ⇒ 凭证删不掉
  restore: 从备份原样还原（并校验 sha256）
"""
import hashlib
import pathlib
import shutil
import sys

BAK = pathlib.Path("/root/t255_bak.rs")

MUT_A = (
    """          AND entity_id IN (SELECT id FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}')\"""",
    """          AND entity_id IN (SELECT id FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}变异A')\"""",
)
MUT_B = (
    """    let vouchers_sql = format!("DELETE FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}'");""",
    """    let vouchers_sql = format!("DELETE FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}变异B'");""",
)


def sha(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    action, target = sys.argv[1], pathlib.Path(sys.argv[2])
    if action == "restore":
        shutil.copyfile(BAK, target)
        assert sha(target) == sha(BAK), "还原后 sha256 与备份不一致"
        print(f"restore OK sha256={sha(target)}")
        return 0

    old, new = {"A": MUT_A, "B": MUT_B}[action]
    text = target.read_text()
    hits = text.count(old)
    assert hits == 1, f"变异点必须唯一命中，实际 {hits} 次"
    target.write_text(text.replace(old, new))
    print(f"mutate {action} OK（命中 1 处）sha256={sha(target)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
