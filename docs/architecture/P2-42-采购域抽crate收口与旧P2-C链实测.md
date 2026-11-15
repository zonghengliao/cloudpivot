# P2-42 采购域抽 crate 收口 —— 旧 P2-C 链（P2-42 ~ P2-58）实测与处置

> 卡：`t_4a7a45bf`（P2-42 抽 `erp-purchase` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 前序同类收口：`P2-41-成本域抽crate可行性分析.md` §8（`t_67fb7f7c`，提交 `11f490f2`）

## 1. 本卡处置：作废（不建 crate、不动生产代码）

不建 `backend/crates/cloudpivot-erp-purchase/`，不搬 16 个文件 / 15 725 行，不改任何
`crate::business::purchase` import，不动 `app/Cargo.toml`。

卡面「验收」`cargo build --workspace` + `cargo test -p cloudpivot-erp-purchase` **随作废一并失效**：
包里没有这个 crate，命令本身不成立。作为替代，本卡给出两条可复核的真实证据：

```bash
$ cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1
exit= 0
workspace_root= /opt/cloudpivot/backend
  package: cloudpivot               /opt/cloudpivot/backend/app/Cargo.toml
  package: cloudpivot-doc           /opt/cloudpivot/backend/crates/cloudpivot-doc/Cargo.toml
members= 2
```

即：workspace 仍是 P2-40（`35a306be`）落地时的 2 个包，**没有** `cloudpivot-erp-purchase`；
本卡对工作区零改动。

## 2. `purchase` 的实测（只读，无库依赖）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain purchase --scope erp-sub
域：purchase（scope=erp-sub）
  规模：16 个文件 / 15725 行
  SCC：51 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（50）：asset, budget, contract, cost, customer, erp#root:approval_gate, …,
                           expense, ext:changelog, ext:flow, ext:hr, ext:impexp, ext:task,
                           linkage, payable, period, price, receivable, report, sales, shipment,
                           stockin, stockout, stocktake, transfer, voucher, year
  回边（留在应用 crate 的代码 → 本域）：61 处代码级
    stockin 25 · erp#root:handler 9 · erp#root:cancel_cascade 5 · linkage 5 · price 3 ·
    erp#root:batch 3 · erp#root:requisition_bridge 2 · erp#root:return_pdf 2 ·
    erp#root:doc_shape_tests 2 · budget 1 · erp#root:dto 1 · erp#root:doc_reader_impl 1 ·
    erp#root:cancel_cascade_tests 1 · erp#root:doc_reader_impl_tests 1
  出边（本域 → 其它）：18 个目标
    ext:flow 19 · erp#root:model 12 · erp#root:doc_shape 8 · erp#root:dto 7 ·
    erp#root:approval_gate 6 · budget 5 · linkage 5 · stockin 4 · voucher 3 ·
    erp#root:form_snapshot 2 · erp#root:unit_guard 2 · stockout 2 · contract 1 ·
    erp#root:fixture_hygiene 1 · erp#root:serial_model 1 · erp#root:requisition_bridge 1 ·
    erp#root:inventory_writer 1 · period 1
  层次判据：本步要切 18 条 / 81 行 —— 出边指向更下层以外：…（同上 18 个目标）
  结论：不可抽（61 处代码级回边，14 个来源）

$ python3 scripts/verify/crate_split_readiness.py --domain purchase --scope erp-sub --layer-check
...（同上，末行）层次判据：本步要切 18 条 / 81 行 ...
exit=1
```

**判据是出边，不是入边**（§P2-C.1 已定稿）：入边（`app → purchase`）是合法方向，装配层必须依赖域 crate；
真正决定「能不能成包」的是 81 行出边。这 81 行里**只有 `ext:flow` 19 行是跨域接缝**，
其余 **62 行 / 17 个目标**（`model / dto / doc_shape / approval_gate / budget / linkage / stockin /
stockout / voucher / contract / period / form_snapshot / unit_guard / serial_model /
requisition_bridge / inventory_writer / fixture_hygiene`）全是 **`erp` 包内兄弟**，只可能在 C13 里切。

## 3. 为什么不能「本卡自己切 81 行、把 purchase 做成小包」

1. **同一个 51 节点 SCC**：`purchase` 与其余 50 个节点互达，先抽出 `purchase` 之后，`sales`/`stockin`/
   `voucher`/`cost` 任何一张再抽都顶在 `A → purchase(在 erp 包外) → …→ A` 的间接环上；先抽哪张都一样
   （与 `cost` 同一结论，见 P2-41 §7）。
2. **切 81 行只换来 296 388 行里的一块 15 725 行**：出边目标的绝大多数在 C13 包内，为了让一个小包
   「干净」而在包内拉 81 行接缝，等于把 `erp` 内部的目录分层提前固化成跨包 API —— 而这 81 行里
   有一半是要在 C13 之后才允许存在的方向（装配层/内核层都还没到位）。
3. **收益方向不对**：`erp` 占全仓 50%，包边界对增量编译的收益来自「内核 / 装配层 / `erp`」这一级；
   在 `erp` 内部再切 18 刀，换不到编译单元的减少。

## 4. 旧 P2-C 链（P2-42 ~ P2-58）17 张卡逐条实测与落点

全部为本次实测（`--layer-check`，2026-11-15，99）：

| 旧卡 | 卡面目标 | 实测 文件 / 行 | SCC | 入边（来源） | 出边目标 | 本步要切 | `--layer-check` | 新落点 |
|:--:|---|--:|:--:|--:|--:|--:|:--:|---|
| P2-42 | `erp-purchase` | 16 / 15 725 | 51 | 61（14） | 18 | **81 行** | 退 1 | C13 `cloudpivot-erp` 包内 |
| P2-43 | `erp-sales` | 11 / 13 247 | 51 | 7（5） | 19 | 41 行 | 退 1 | C13 包内 |
| P2-44 | `erp-stock` | 3 / 2 108 | **1** | 0 | 4 | 5 行 | 退 1 | C13 包内（唯一不在 SCC 的子域，仍不单抽） |
| P2-45 | `erp-finance`（**无此目录**） | 见下注 | 51 | — | — | — | 退 1 | C13 包内 |
| P2-46 | `erp-asset` | 9 / 9 684 | 51 | 17（4） | 7 | 19 行 | 退 1 | C13 包内 |
| P2-47 | `erp-budget` | 9 / 7 150 | 51 | 20（6） | 5 | 10 行 | 退 1 | C13 包内 |
| P2-48 | `erp-contract` | 9 / 8 775 | 51 | 7（7） | 7 | 17 行 | 退 1 | C13 包内 |
| P2-49 | `erp-expense` | 9 / 7 444 | 51 | 2（2） | 9 | 25 行 | 退 1 | C13 包内 |
| P2-50 | `erp-price` | 9 / 6 970 | 51 | 3（2） | 7 | 15 行 | 退 1 | C13 包内 |
| P2-51 | `hr` | 80 / 63 378 | 24 | 25（4） | 4 | 42 行 | 退 1 | **C11 `cloudpivot-hr`** |
| P2-52 | `flow` | 48 / 48 338 | 24 | 128（7） | 7 | 35 行 | 退 1 | **C12 `cloudpivot-flow`** |
| P2-53 | `task`（`business/task`） | 53 / 27 688 | 24 | 19（6） | 10 | 37 行 | 退 1 | **C14 `cloudpivot-task`**（另含 `src/task`） |
| P2-54 | `impexp` | 30 / 21 260 | 24 | 40（4） | 4 | 23 行 | 退 1 | **C9 `cloudpivot-impexp`** |
| P2-55 | `attachment` | 11 / 9 837 | 24 | 23（1） | 2 | 5 行 | 退 1 | **C4 `cloudpivot-attachment`** |
| P2-56 | `notice` | 11 / 8 224 | 24 | 74（7） | 1 | 3 行 | 退 1 | **C5 `cloudpivot-notice`** |
| P2-57 | `pdf` | 5 / 6 243 | 24 | 28（4） | 1（→内核 `shared`） | 0 行 | **退 0** | **C5 `cloudpivot-pdf`** |
| P2-58 | `shared`（`business/shared`） | 18 / 14 384 | 24 | 480（18） | 2 | 3 行 | 退 1 | **C1 `cloudpivot-kernel`**（另含 `src/shared`、`utils`） |

注（P2-45）：仓库里没有 `business/erp/finance/` 目录，卡面目标无法对应单个节点。按财务组实测：
`voucher` 8/12 298（入边 100、要切 25 行）、`period` 9/7 857（34、19）、`year` 8/5 049（3、22）、
`report` 4/5 428（9、8）、`payable` 7/10 131（9、30）、`receivable` 4/5 360（11、23）—— 均在 SCC 内、全部退 1。

三条要点：

1. **17 张里 16 张 `--layer-check` 退 1**：出边不是 0，就是指向比「当前卡序位置」更深的东西。
2. **`pdf` 是唯一退 0 的**（出边只剩内核 `shared` 2 处）—— 但它仍并入 C5（与 `notice`/`docpdf`/`monitor`
   同卡），不是因为不可能，而是**同批搬迁更省**（一次改 bin 依赖 + 一次路由条数核对）。
3. **`erp-stock` 是唯一不在 SCC 的子域**（`stock` 3 文件 / 2 108 行、入边 0），只要切 5 行出边
   （`order_pdf_model 2`、`stockout 1`、`contract 1`、`stocktake 1`）就够 —— 但它换不回任何东西：
   这 4 个目标**全是 `erp` 包内兄弟**，单抽出来照样是反向依赖单体的空壳包，且会让 C13 之后的
   `stockout`/`stocktake` 卡顶在环上。所以按 §P2-C 一并作废。

## 5. 复核命令（全部只读，可对任意机器跑）

```bash
cd /opt/cloudpivot
python3 scripts/verify/crate_split_readiness.py --domain purchase --scope erp-sub              # 本卡：16 文件/15 725 行，SCC 51
python3 scripts/verify/crate_split_readiness.py --domain purchase --scope erp-sub --layer-check # 退 1（要切 18 条/81 行）
python3 scripts/verify/crate_split_readiness.py --scope erp-sub --json                          # 25 个子域机器可读（21 个 SCC=51）
python3 scripts/verify/crate_split_readiness.py --domain erp                                    # 353 文件/296 388 行，SCC 24
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md  # 退 0：15 步 66 行，判定绿
```

`--plan-order` 本次输出摘要（与 §7/P2-41 一致，未变）：

```
C1 cloudpivot-kernel 10/10 OK · C2 cloudpivot-server 0/0 OK · C3 support 6/6 · C4 storage+attachment 1/1
C5 notice+pdf+docpdf 3/3 · C6 changelog+permission+announcement+mutex 4/4 · C7 system 7/7 · C8 masking 0/0
C9 impexp 13/13 · C10 help+aiagent 1/1 · C11 hr 7/7 · C12 flow 2/2 · C13 erp 12/12 · C14 task 0/0 · C15 收尾 0/0
全序累计要切：66 行
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
exit=0
```

## 6. 保留物 / 后继 / 给总管的建议

- **保留**：`scripts/verify/crate_split_readiness.py`（含 `--layer-check` / `--plan-order`）、
  `docs/planning/20261113-新卡清单.md` §P2-C（15 卡卡序）、本文（17 张旧卡的逐条实测，任何一张被派发时可直接引用，不必重扫）。
- **后继**：C1 `cloudpivot-kernel` → C2 `cloudpivot-server` → C3…C13（`purchase` 随 C13 `cloudpivot-erp`
  整包搬迁，包内继续用目录分层）。本链剩余 16 张（P2-43 ~ P2-58）**不需要再开工**。
- **建议**：总管直接把 P2-43 ~ P2-58 从 `todo` 关闭（或改 `blocked` 并注明「并入 Cx」），
  避免 16 个 worker 依次被 dispatch 空跑 16 次。
- **本卡零生产代码改动**：无 Rust 文件变更，故未跑 `cargo fmt`（无格式面），
  `backend/` 下唯一变化是本文（`.md`），`cargo metadata` 仍为 2 个包（见 §1）。
