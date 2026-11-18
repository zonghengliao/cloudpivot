# L3 干净检出闸红①：`d8c_wiring_anchors_exist_in_source` 以 CWD 相对路径找 `src/`（卡 t_71d2aa63）

## 结论（一句话）

用例改用**编译期** `env!("CARGO_MANIFEST_DIR")` 定位源码根，不再依赖运行时 CWD；
同一个测试二进制在 `backend/`（workspace 根 = L3 的 CWD）与 `backend/app/`（crate 根）两个 CWD 下都 **ok**；
判据强度不变（挪走任一 wired 锚点仍必然红），提示文案同步改成「按 CARGO_MANIFEST_DIR 定位」。

改动：`backend/app/src/business/permission/scope_model.rs`，+21/−3，只动 `mod tests` 里的该用例与文档注释。
`cargo fmt -p cloudpivot -- --check` = 0，`cargo clippy --all-targets` = 0 warning / 0 error。

## 红→绿的前后对照（同一台机器、同一 target，只换 `scope_model.rs` 新旧版本再重编）

| 阶段 | 源码 | 二进制 | CWD=`backend/app` | CWD=`backend`（L3） | 证据 |
|:--|:--|:--|:--|:--|:--|
| 改前 | HEAD `f09bdcf9` 原版 | 重编 | ok（父卡证据） | **FAILED** `panicked at scope_model.rs:1296: 找不到 src/ 下的 rs 文件（单测须以 crate 根为 CWD）` rc=101 | `02_before_head_version.out`（`backend/` 一列）；`backend/app` 一列见父卡 `docs/evidence/2026-11-18-t1d66ac6d-skip-manifest-38/10_L3_two_reds_cwd_crate_root.out` |
| 改后 | 本卡修法版本 | 重编 | **ok** rc=0 | **ok** rc=0 | `03_after_cargo_manifest_dir.out` |

`02_before_head_version.out` 里 `CWD=backend/app` 那一段是 `rc=127`（不是用例结论）：取证脚本把 cargo 输出的
**相对**二进制路径直接交给了已经 `cd` 到 `backend/app` 的子 shell，`target/...` 在那里不存在 ⇒ command not found。
这条探针 bug 没有重跑（重跑要再重编一次），改前 `CWD=backend/app` = ok 由父卡已落盘的同二进制输出（上面第二条指针）承担。
**关键的那一列（改前 `CWD=backend` 红 → 改后 `CWD=backend` 绿）两次都是真跑出来的**，且 `02` / `03` 两跑的
`sha256` 与源码版本都在文件头写明。

## 反证（改完还有牙）

`04_teeth_anchor_removed.out`：把 `business/impexp/model.rs` 里 2 处 `ScopeTarget::SYS_IMPORT_TASK` 全部改名为
`ScopeTarget::DRIFTED_PROBE_TASK`（刻意不用后缀追加 —— 判据是 `text.contains(...)`，后缀追加仍会被命中）后，
同一二进制、CWD=`backend` 下：

```
sys_import_task 登记为 wired，但 src/ 里没有含 scope:: 调用的文件出现 ScopeTarget::SYS_IMPORT_TASK（登记表已与代码漂移）。
rc=101
```

还原（内存原字节写回 + sha256 校验一致，未用 `git checkout`，避免动到共享工作区里别人的改动）后 rc=0。

## 验收逐条

| 验收项 | 结果 | 证据 |
|:--|:--|:--|
| 同一二进制在 `backend/` 与 `backend/app/` 都 ok | ✅ | `03_after_cargo_manifest_dir.out`（两段 rc=0） |
| 全量 `check.sh` 的 L3 失败清单不再出现本用例 | ✅ `d8c` 已不在失败清单（清单里只剩并行卡 `t_a5eca17f` 的 `contract_aliases_match_handler_permissions`，本卡不管） | `05_L3_gate_after.out`（提交后 HEAD `10d4b3f3` 真跑）、`07_check_sh_full.out`（全量档） |
| 反证：挪走 wired 锚点仍必红 | ✅ rc=101 并点名 `sys_import_task` | `04_teeth_anchor_removed.out` |
| `cargo fmt` / `cargo clippy --all-targets` 不受影响 | ✅ fmt rc=0；clippy 0 warning / 0 error | `06_fmt_clippy.out` |
| 不改判据强度（没删断言、没 `if files.is_empty() { return; }`） | ✅ | `git diff` 只换定位方式与文案；`files.is_empty()` 断言仍在 |

### L3 真跑（提交后，HEAD `10d4b3f3`）

```
==> 干净检出闸：基线 HEAD (10d4b3f3)
  ✓ 无私有配置（backend/config/config-dev.toml … ），模板齐全
  test result: FAILED. 6561 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out
  ✗ 干净检出任有 1 条单测失败：test kernel_axis_tests::contract_aliases_match_handler_permissions ... FAILED
```

改前（父卡落盘，HEAD `fb16fcd5`）是「6560 passed / 2 failed（d8c + contract_aliases）」——
见 `docs/evidence/2026-11-18-t1d66ac6d-skip-manifest-38/08_check_sh_full_L3red.out:303`；
现在是「6561 passed / 1 failed（只剩 contract_aliases）」——差的那一条正是本卡修的 d8c。
L3 仍 rc=1 是**并行卡的账**（`t_a5eca17f`，不在本卡验收范围）。


## 文件

- `01_binaries.txt` — 前后两次重编的二进制路径（脚本产物，含相对路径 bug 的现场）
- `02_before_head_version.out` — 改前版本重编后两 CWD 的跑法
- `03_after_cargo_manifest_dir.out` — 改后版本（当前 HEAD 内容）两 CWD 都 ok
- `04_teeth_anchor_removed.out` — 反证 + 还原
- `05_L3_gate_after.out` — `scripts/verify/clean_checkout_gate.sh` 真跑输出
- `06_fmt_clippy.out` — fmt / clippy
