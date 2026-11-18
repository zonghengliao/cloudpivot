#!/usr/bin/env python3
"""M4-15 供应商评估单测的「变异反证」助手。

把打分 / 评级的判据临时改成错的写法（变异），跑本卡的用例应当**转红**；
恢复后必须**转绿**。用来证明用例真的有牙（尤其是「评级边界」不是
「实现怎么改都绿」）。

用法：
    python3 scripts/verify/m415_mutations.py <A|B|C|D> apply [backup_dir]
    python3 scripts/verify/m415_mutations.py <A|B|C|D> restore [backup_dir]

四个变异（每个都对应一条/一组用例）：

| id | 变异 | 期望转红的用例 |
|----|------|----------------|
| A | 阈值比较 `>=` 改成 `>`（边界点掉进下一档 / 0 分无档可命） | `grade_policy_holds_on_every_hundredth_point` / `rating_boundaries_hold_in_the_database` |
| B | 总分舍入从「中点远离零」改成「向零截断」（.995 不再抬档） | `rounding_happens_before_grading` / `rating_boundaries_hold_in_the_database` |
| C | 阈值表把 B 档下界 80 改成 85（政策被悄悄改动） | `grade_policy_holds_on_every_hundredth_point` / `sql_case_agrees_with_rust_on_every_hundredth_point` |
| D | `grade_case_sql` 里塞一条硬编码 91 分→B 的第二份阈值（打分与 regrade 分叉） | `case_sql_and_rust_share_one_threshold_table` / `sql_case_agrees_with_rust_on_every_hundredth_point` |

备份目录默认是 `$TMPDIR/m415-mutations`；`apply` 会把**原始文件**存成
`<basename>.<id>.orig.rs`，`restore` 从它复制回去并校验与原文逐字相同。
"""

from __future__ import annotations

import hashlib
import os
import shutil
import sys

GRADING = "backend/app/src/business/erp/purchase/supplier_evaluation_grading.rs"
MODEL = "backend/app/src/business/erp/purchase/supplier_evaluation_model.rs"

# id → {相对路径: [(原文, 变异后的文本), …]}；每条锚点必须在文件里**恰好出现一次**
MUTATIONS: dict[str, dict[str, list[tuple[str, str]]]] = {
    "A": {
        GRADING: [
            (
                "        if total >= threshold_dec(min_total) {",
                "        if total > threshold_dec(min_total) { // MUTATION-A：边界点不再含下界",
            ),
        ],
    },
    "B": {
        MODEL: [
            (
                "pub const TOTAL_ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;",
                "pub const TOTAL_ROUNDING: RoundingStrategy = RoundingStrategy::ToZero; // MUTATION-B：截断代替四舍五入",
            ),
        ],
    },
    "C": {
        GRADING: [
            (
                '    (model::GRADE_B, "80"),',
                '    (model::GRADE_B, "85"), // MUTATION-C：B 档下界被悄悄改成 85',
            ),
        ],
    },
    "D": {
        GRADING: [
            (
                '    let mut sql = format!("CASE WHEN {column} IS NULL THEN NULL");',
                """    let mut sql = format!("CASE WHEN {column} IS NULL THEN NULL");
    sql.push_str(&format!(" WHEN {column} >= 91 THEN 'B'")); // MUTATION-D：第二份阈值（91 分 → B）
""",
            ),
        ],
    },
}


def digest(path: str) -> str:
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    key, action = sys.argv[1].upper(), sys.argv[2].lower()
    backup = sys.argv[3] if len(sys.argv) > 3 else os.path.join(
        os.environ.get("TMPDIR", "/tmp"), "m415-mutations"
    )
    if key not in MUTATIONS:
        print(f"未知变异 id：{key}（可选 {sorted(MUTATIONS)}）", file=sys.stderr)
        return 2
    if action not in {"apply", "restore"}:
        print(f"未知动作：{action}（apply / restore）", file=sys.stderr)
        return 2

    root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
    os.makedirs(backup, exist_ok=True)
    for rel, edits in MUTATIONS[key].items():
        path = os.path.join(root, rel)
        orig = os.path.join(backup, f"{os.path.basename(rel)}.{key}.orig.rs")
        if not os.path.exists(path):
            print(f"目标文件不存在：{path}", file=sys.stderr)
            return 2
        if action == "apply":
            if os.path.exists(orig):
                print(f"备份已存在（上一次没恢复？）{orig}", file=sys.stderr)
                return 1
            with open(path, "r", encoding="utf-8") as handle:
                text = handle.read()
            for old, new in edits:
                hits = text.count(old)
                if hits != 1:
                    print(f"锚点在 {rel} 出现 {hits} 次（必须恰好 1 次）：{old!r}", file=sys.stderr)
                    return 1
                text = text.replace(old, new)
            shutil.copyfile(path, orig)
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(text)
            print(f"已施加变异 {key} → {rel}（原文件备份 {orig}）")
        else:
            if not os.path.exists(orig):
                print(f"没有备份可恢复：{orig}", file=sys.stderr)
                return 1
            shutil.copyfile(orig, path)
            if digest(path) != digest(orig):
                print(f"恢复后与备份不一致：{path}", file=sys.stderr)
                return 1
            os.remove(orig)
            print(f"已恢复 {rel}（与备份逐字一致）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
