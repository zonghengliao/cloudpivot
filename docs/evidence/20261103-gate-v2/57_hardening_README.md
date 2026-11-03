# 门禁硬化证据集（卡 t_1b3b5c5c，2026-11-03）

> 口径与结论见 `docs/门禁口径-v2-20261103.md` **附录 B**（D1/D4 的盲区与断言：R1–R7）。
> 本目录（`docs/evidence/20261103-gate-v2/`）同时保留了卡 A/C/D 的证据（`10_`–`45_`），
> 本卡（硬化）的原始输出一律以 `5x_hardening_*.txt` 命名，互不覆盖。

## 一句话结论

三条「需刻意写法才能触发」的假绿/漏检通道（R1 跳过清单无断言 / R2 子模块路径非 Rust 语义 /
R4 G5 裸调降级）已**加严**并带反例自测；R3（行粒度）顺手改成字符级；R5/R6/R7 只登记不改。
**没有放宽任何判据**，没有业务代码改动。

## 文件清单

| 文件 | 内容 |
|---|---|
| `50_hardening_selftest_gate2.txt` | 闸② `--self-test`：**14 项全 PASS**（原 10 项；新增 ⑦' 清单一致不误伤、⑩ 幽灵 `#[path]` → FAIL + 反事实对照、⑪ R2 不连坐、⑫ R3 同行生产查询仍判） |
| `51_hardening_selftest_g5.txt` | G5 `--self-test`：**13 项全 PASS**（原 12 项；新增 ⑨' `use …changelog::record;` 后裸调 → 判红） |
| `52_hardening_gate_direct_clean.txt` | 干净树直跑：闸② `扫描生产面 310 / 跳过测试面 51`、`PASS：0 处`、`EXIT=0`；G5 `29 个调用点`、`EXIT=0` |
| `53_hardening_probes.txt` | 卡 C 三个探针的**完整原始输出**：T0=0 / T2=1 / T3=1 / **T4=1** / T5a=1 / **T5b=1** / T6a=1 / T6b=1 / T7=0；U1/U4/U5/**U2**/U6 报红、U3 不报红；G5 case01·02·03·04·**08**·11 报红、case05·07·09·10 不报红 |
| `54_hardening_check_sh.txt` | 干净树上 `bash scripts/check.sh`：fmt / clippy（`touch lib.rs` 后**真编译 22.81s**）/ check（6.42s）/ pnpm typecheck / 闸① / 闸②（310-51，PASS）/ C7 G1–G6（G5=29）/ 色板 / 闸③（本机无库口令 → 按「无 DB 环境」跳过）→ **EXIT=0** |
| `55_exempt_radius.txt` | R7 实测半径：生产面 310 个文件里 108 个带 `SCOPE-EXEMPT`（283 处），其中 58 个靠标记放行（逐文件明细） |
| `56_dirty_tree_manifest_fired.txt` | SKIP_MANIFEST 在**共享工作区**上的实际拦截：别卡未提交的 `business/erp/cancel_cascade_tests.rs` 被点名（这条红灯是 R1 想要的行为，提交时补一行即可） |

## 两个「为什么」

1. **为什么探针的 `CP_SRC` 用干净树**：探针要验的是**解析器**，基线必须是已提交的树。
   共享工作区里有别卡（P15 作废级联）未提交的测试面文件，会让本卡的 `SKIP_MANIFEST` 断言先报一条
   （见 `56_`）—— 那是对的，但会把探针结论搅浑。被测脚本仍是 `/opt/cloudpivot/scripts/lint/*.py`。
2. **为什么计数从 311/50 变成 310/51**：R2 修正后，`business/flow/linkage.rs:58` 的
   `#[cfg(test)] mod tests;` 才被解析到正确路径 `business/flow/linkage/tests.rs`。
   旧实现去找 `business/flow/tests.rs`（不存在），把真正的测试文件当生产面扫了 —— 那是**漏跳**
   （保守方向，不产生假绿），新实现把它正确归入测试面，已登记进 `SKIP_MANIFEST`。

## 复跑

```bash
export TMPDIR=/root/.hermes/profiles/backend-developer/cache/scratch      # /tmp 是满的 tmpfs
python3 scripts/lint/scope_gate.py --self-test
python3 scripts/lint/changelog_in_tx.py --self-test
python3 scripts/lint/scope_gate.py --root .          # 干净树上：310 / 51，EXIT=0
export PROBE_DIR=$TMPDIR/my_probe CP_SRC=<干净树>/backend/src && mkdir -p $PROBE_DIR
python3 docs/evidence/20261103-gate-v2/probes/scope_bypass.py
python3 docs/evidence/20261103-gate-v2/probes/scope_edges.py
python3 docs/evidence/20261103-gate-v2/probes/g5_build_and_run.py
```
