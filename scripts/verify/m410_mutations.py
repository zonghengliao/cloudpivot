#!/usr/bin/env python3
"""M4-10 框架协议单测的「变异反证」助手。

把 `framework_agreement_ref.rs` 里的一条判据临时改成错的写法（变异），
跑本卡的用例应当**转红**；恢复后必须**转绿**。用来证明用例真的有牙，
而不是「无论代码对错都绿」。

用法：
    python3 scripts/verify/m410_mutations.py <A|B|C|D> apply [backup_dir]
    python3 scripts/verify/m410_mutations.py <A|B|C|D> restore [backup_dir]

四个变异（每个都对应一条用例）：

| id | 变异 | 期望转红的用例 |
|----|------|----------------|
| A | 本单用量按「逐行」而不是按「协议行」聚合 | `split_lines_on_same_material_cannot_bypass_cap` |
| B | 明细上限 / 单头总量上限的 `>` 改成 `>=`（恰等于也拒） | `split_lines_…` / `update_does_not_double_count_own_usage` / `head_total_qty_…` / `stale_booked_…` / `inflated_booked_…` |
| C | 金额上限恒按不含税比（含税协议白送一个税率） | `tax_inclusive_total_amount_cap_counts_with_tax` |
| D | 单头总额上限的 `>` 改成 `>=` | `head_total_amount_cap_allows_exact_boundary` |

备份目录默认是 `$TMPDIR/m410-mutations`；`apply` 会把**原始文件**存成
`ref.<id>.orig.rs`，`restore` 从它复制回去并校验与原文逐字相同。
"""

from __future__ import annotations

import hashlib
import os
import shutil
import sys

TARGET = "backend/app/src/business/erp/purchase/framework_agreement_ref.rs"

# id → [(原文, 变异后的文本)]，每条锚点必须在文件里**恰好出现一次**
MUTATIONS: dict[str, list[tuple[str, str]]] = {
    "A": [
        (
            "        let added = this_qty.get(&item.id).copied().unwrap_or(Decimal::ZERO);",
            "        let added = line.qty; // MUTATION-A：逐行计，不再按协议行聚合",
        ),
    ],
    "B": [
        (
            "        if used + added > max_qty {",
            "        if used + added >= max_qty { // MUTATION-B1：恰等于上限也拒",
        ),
        (
            "        if used + this_qty.values().copied().sum::<Decimal>() > total_max_qty {",
            "        if used + this_qty.values().copied().sum::<Decimal>() >= total_max_qty { // MUTATION-B2",
        ),
    ],
    "C": [
        (
            "            .map(|u| amount_for_cap(u.amount, u.with_tax, agreement.price_includes_tax))",
            "            .map(|u| u.amount) // MUTATION-C1：金额恒按不含税比",
        ),
        (
            "        let this = amount_for_cap(this_amount, this_with_tax, agreement.price_includes_tax);",
            "        let this = this_amount; // MUTATION-C2：金额恒按不含税比",
        ),
    ],
    "D": [
        (
            "        if used + this > total_max_amount {",
            "        if used + this >= total_max_amount { // MUTATION-D：恰等于总额也拒",
        ),
    ],
}


def digest(path: str) -> str:
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    key, action = sys.argv[1].upper(), sys.argv[2].lower()
    if key not in MUTATIONS:
        print(f"未知变异 id：{key}（可选 {'/'.join(MUTATIONS)}）", file=sys.stderr)
        return 2
    root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    target = os.path.join(root, TARGET)
    backup_dir = sys.argv[3] if len(sys.argv) > 3 else os.path.join(
        os.environ.get("TMPDIR", "/tmp"), "m410-mutations"
    )
    backup = os.path.join(backup_dir, f"ref.{key}.orig.rs")
    os.makedirs(backup_dir, exist_ok=True)

    if action == "apply":
        source = open(target, encoding="utf-8").read()
        for old, new in MUTATIONS[key]:
            count = source.count(old)
            if count != 1:
                print(f"锚点不唯一（{count} 次），拒绝变异：{old!r}", file=sys.stderr)
                return 1
            source = source.replace(old, new)
        # 备份只写一次：重复 apply 不许覆盖成「已变异」的版本
        if not os.path.exists(backup):
            shutil.copy(target, backup)
        with open(target, "w", encoding="utf-8") as handle:
            handle.write(source)
        print(f"MUTATION-{key} applied（原文件备份 {backup}，sha256={digest(backup)[:16]}…）")
        return 0

    if action == "restore":
        if not os.path.exists(backup):
            print(f"没有备份可恢复：{backup}", file=sys.stderr)
            return 1
        shutil.copy(backup, target)
        print(f"MUTATION-{key} restored（sha256={digest(target)[:16]}…）")
        return 0

    print(f"未知动作：{action}（apply|restore）", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
