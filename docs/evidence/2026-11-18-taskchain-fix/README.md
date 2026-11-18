# 证据：`task_chain_valid.py --fix` 在「2 张游离清债类卡」形态下收敛、且不动主链

- 卡：`t_f484849d`【测试卫生·门禁】；基线 HEAD `471e0100`（修改前脚本取自 `git show HEAD:scripts/lint/task_chain_valid.py`）
- DB：**全部在副本库上跑**（`/root/.hermes/kanban/boards/dev/kanban.db` 的 `cp` 快照，连
  `-wal` / `-shm` 一起复制；真库只跑过只读检查模式，见 `07_live_check.out`）
- 形态：running 卡 `t_f484849d` 在链中段，同形态下另立 2 张「游离 ready 清债类」卡
  （`t_pt7d2aa63` / `t_pa5eca17f`，克隆自真实卡行、无任何 `task_links` 边）
- 脚本 sha256（改动后）：`22e81e8c851efef62d1d4b240fb6d30a514201f59654f8501f09a94d8a73e80c`
- 血案原始现场（发现于 `t_1d66ac6d`）：`docs/evidence/2026-11-18-chainfix-probe/`

## 一、修前：确实不收敛，且把主链打散（同一副本形态重放）

| 文件 | 命令 | 结果 |
|:--|:--|:--|
| `00_before_dry_run.out` | `--fix --dry-run` | `共 200 处调整`；同一个卡 `t_pa5eca17f` 反复打印「插在链头」199 次（415 行输出） |
| `00b_before_real_fix.out` | `--fix` | `rc=1`，`修了 200 处，仍有 2 处未解决` |
| `00c_before_after_check.out` | `--verbose` | **`主链 1 张，未完成 112 张`** —— 从链头已走不到其余 111 张 |

## 二、根因（行号是修改前的 HEAD）

1. `Chain.head()`：优先序是「未完成卡里唯一没有父卡的那张」**先于** `running`。修掉第 1 张游离
   卡后只剩 1 张无父卡 → 它成了链头 → `walk()` 只走得到它自己 → `order_wo == []`。
2. `choose_insert` 在 `order` 为空时被钳成 `idx = 0`（`max(0, min(idx, len(order_wo)))`）→
   `prev_node = next_node = None`。
3. `apply_fix` 先 `DELETE FROM task_links WHERE child_id = ?`，再因为 prev/next 皆空**一条边也不建**
   → 状态不变 → 下一轮重来；循环里只有 `guard < 200`，没有「本轮是否真的变好」的判据。
4. 每轮都 `con.commit()` → 破坏落库。

## 三、修法（五条，判据只加不减）

1. **`head()` 只认 `running`**：有 running 卡时一律以它为链头，「唯一无父卡」降为没有 running
   时的兜底。（这条单独就能让本形态收敛）
2. **`choose_insert` 不许静默钳位**：`order` 为空 → 返回 `None`，由调用方报「无法自动修复」。
3. **`apply_fix` 先算后改 + 每轮断言真的变好**：算出 `(prev, card, next)` 之后才许 DELETE；
   每轮 `SAVEPOINT` 包住，改完重新加载图并断言**缺陷重量严格下降**（C1/C2 按"多出来的子卡数"
   计权，3 儿子搬走 1 个也算下降），不降就 `ROLLBACK TO` 该轮并打印可粘贴的人工落边 SQL。
   整笔修复只在最后提交一次。
4. **显式 `BEGIN IMMEDIATE` + `isolation_level = None`**（修的过程中新踩到的第 5 个坑）：
   SQLite 里「`RELEASE` 最外层 SAVEPOINT」会**顺带提交整笔事务**，不显式开事务的话
   `--dry-run` 会写库（实测干跑后副本库 `task_links` 从 1285 变 1287）。
5. **加 `--selftest` 并挂进 `scripts/check.sh` 的 L0 项**：17 条断言在内存库里重放本血案形态。
   这正是本卡的病根——检查模式全绿、只有真跑 `--fix` 的人踩；现在提交阶段就会红。

## 四、验收（逐条对卡面）

| 卡面要求 | 证据 | 实测 |
|:--|:--|:--|
| `--fix --dry-run`：`共 N 处调整`与终态一致、每卡只出现一次 | `01_dry_run.out` | `共 2 处调整`，两行·每卡一次；`修完主链 112 张、剩余问题 0 处`；干跑前后 `links=1285` 未变 |
| `--fix`：rc=0 | `02_real_fix.out` | `rc=0`，`✓ 已修 2 处，任务链现在结构完整（主链 112 张）` |
| 随后检查模式 `✓ 任务链结构完整`、主链张数不减 | `03_after_check.out` | `rc=0`，`主链 112 张，未完成 112 张`，`✓ 任务链结构完整` |
| 反证：`head()` 去掉 running 优先 → 必须重现「主链 1 张」 | `04_counter_fix.out` / `04b_counter_check.out` | 同一二进制去掉该判据后：修了 1 处就卡死，`⚠ 修了 1 处，仍有 2 处未解决（主链 1 张）`；检查模式 **`主链 1 张，未完成 112 张`** |
| `bash scripts/check.sh --l0` 该项仍绿 | `06_check_l0.out` | `rc=0`，`✓ 自检通过（17/17 条）` + `✓ 任务链结构完整`，`==> 全部检查通过（L0）` |
| 真库 `python3 scripts/lint/task_chain_valid.py` → `✓` | `07_live_check.out` | `rc=0`，`主链 110 张，未完成 110 张`，`✓ 任务链结构完整` |
| `--selftest` 自身可跑 | `05_selftest.out` | `rc=0`，17/17 |
| push 前 pre-push 钩子（L0+L1） | （`git push` 输出） | `✓ pre-push 门禁通过（L0+L1，耗时 98s）`；远端 `origin/main = 05f54dae` |

## 五、反证（`--selftest` 有牙）

把四条判据逐条打回去，自检必须变红；改完用内存备份 `cp` 还原并 sha256 逐字节校验
（输出见 `08_teeth.out`，驱动脚本是一次性的，未入库）：

| 变异 | 自检结果 |
|:--|:--|
| ① `head()` 退回旧优先序 | **RED** rc=1，9 条红 |
| ② `choose_insert(order=[])` 允许钳到 0 | **RED** rc=1，2 条红 |
| ③ 去掉显式 `BEGIN` | **RED** rc=1，1 条红（「干跑不写库」） |
| ④ 去掉「缺陷重量必须严格下降」断言 | **RED** rc=1，3 条红（「⑤ 修不动就不动」抓的） |

## 六、回归（其他形态没被改坏）

除本卡形态外，另在副本库上造了 C1（未完成父卡 2 子）/ C2（已完成父卡 2 未完成子）两种
形态真跑 `--fix`：各 1 处调整、`缺陷 2 → 0`、修后 `✓ 任务链结构完整`，未见回归（`09_regression_c1_c2.out`）。

## 七、复现（想自己验一遍）

```bash
# 一条命令：建副本 → 造 2 张游离 ready 清债卡 → 干跑 / 真跑 / 修后检查 / 自检，
# 四条断言与卡面验收一一对应，真库全程只读（只 cp 快照）。
python3 docs/evidence/2026-11-18-taskchain-fix/repro.py
# 输出见 10_repro_script.out：7/7 ✓、结论「全部符合卡面验收」
```

> `cp` 只复制 `kanban.db` 主文件在 WAL 模式下会拿到**旧快照**（连新卡都不在），
> 必须连 `-wal` / `-shm` 一起复制 —— `repro.py` 已经这么做了。

## 八、边界（本次没做）

- 没有 running 卡、且游离卡多于 1 张时仍无法全自动收口（链头无邻位可挂）：脚本现在会**明确报
  「无法自动修复」并给人工落边 SQL**，而不再是静默转 200 轮。这条属于语义问题（游离卡该挂到
  哪张已完成卡后面，脚本没有权威依据），留给人判断。
- 未改 `C4`（死路卡）的判定口径，也未改 `CHAIN_TAIL_PATTERN`；只是把「修不动」从死循环改成报错。
- 未动检查模式的判据口径：`check.sh` 里新增的只是 `--selftest` 这一步，检查模式本身一行未改。
