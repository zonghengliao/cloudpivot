# M2-16 安全库存：单测（含边界）

- 卡：`t_72aa8970`（M2-16【M2·仓库】安全库存：单测）
- 上游：M2-13 水位设置（V0428 + `resolve_min_qty`）、M2-14 水位检查（`water_check`）、M2-15 补货建议（`replenish::dto::suggest`）
- 本卡**只加测试**（+ 1 处边界 panic 修复，见 §3）
- 新增用例 **17 条**：15 条纯逻辑（`stock_policy/safety_stock_boundary_tests.rs`）+ 2 条真库（`water_check_db_tests.rs` ⑯⑰）

## 1. 为什么单独一轮「边界」

M2-13/14/15 各自的用例钉的是**主场景**（策略覆盖物料级、回升收口、建议量 = 目标 − 预计可用）。
主场景全绿而刀口被挪半个的情况真实存在，而且挪完主场景**仍然是绿的**：

| 会被挪的半个 | 主场景还绿 | 谁现在会红 |
|:--|:--:|:--|
| 判定 `qty < min` → `qty <= min` | ✅ | `boundary_tests::judge_edges_…`、`tests::judge_is_strictly_less_than_threshold`、`db_boundary_quantity_equal_to_level_is_not_low` |
| 触发 `expected < min` → `expected <= min` | ✅ | `boundary_tests::suggest_triggers_on_expected_not_on_hand`、`expected_equal_to_min_with_low_on_hand_is_covered`、`dto::tests::equal_to_min_does_not_trigger` |
| 阈值 `filter(> 0)` 改 `>= 0` | ✅ | `dto::tests::resolve_min_prefers_policy_then_material_then_unset`（⑤ 策略行 0 阻断回退） |

## 2. 覆盖清单

### 纯逻辑（`stock_policy/safety_stock_boundary_tests.rs`，挂在 `water_check` 下以触达私有实现）

| 用例 | 钉住什么 |
|:--|:--|
| `guards_are_exact_at_zero_and_equal_edges` | `min >= 0`；`max` **严格**大于 min（差 0.0001 合法、相等拒、`0 → 0.0001` 也是合法极小区间） |
| `resolver_keeps_the_fourth_decimal` | `NUMERIC(18,4)` 的 0.0001 不被当成 0（策略 / 物料级两条来源 + 判定） |
| `resolver_treats_dirty_negative_policy_as_no_warning` | 库外混进的负水位 → 不预警 / **不**回退物料级 / 不 panic，来源仍报 `POLICY` |
| `judge_edges_at_four_decimals_and_numeric_upper_bound` | `49.9999` 报、`50.0000` 不报；`99999999999999.9999` 上界相减不溢出、缺口 0.0001 |
| `max_level_never_triggers_on_its_own` | 只有最高水位（`min = 0`）→ 永不触发；`target = max`、`shortage` 照算、`suggested = 0` |
| `title_truncation_is_exact_at_two_hundred_chars` | 正好 200 字原样保留、201 字截到 200 且是多字节安全的前缀 |
| `qty_text_trims_zeros_and_negative_zero` | `12.0000 → 12`、`-0.0000 → 0`、空白单位不留空格 |
| `detail_cap_keeps_all_counts_but_trims_items` | 明细前 200 条保留、第 201 条被截（计数由调用方累加，仍是全量） |
| `realert_window_boundaries_are_strict` | 窗口 `now - last < days`：整点放行、差一秒抑制、未来时间（时钟倒流）也抑制 |
| `suggest_triggers_on_expected_not_on_hand` | 触发看**预计可用**：账面 == 水位、在途 5 一压就触发，而 `below_min = false` |
| `expected_equal_to_min_with_low_on_hand_is_covered` | 预计可用 == 水位且账面已低 → `COVERED`（不是 `OK`），默认不显示、`include_covered` 可见 |
| `open_po_above_max_yields_zero_not_negative` | 未清订单超过最高水位 → 缺口夹 0、建议 0（不给负数） |
| `suggest_preserves_four_decimals` | 0.0010 / 0.0025 级水位 → 建议 0.0016 精确到最后一位 |
| `no_level_means_no_target_and_ok_is_never_listed` | 没水位 = 没目标；`OK` 带 `include_covered=true` 也不进列表 |
| `page_bounds_are_clamped_and_never_panic` | `page = 0` 当第 1 页、`u64::MAX` 给空页、`page_size` 0/200/201 归一化 —— 全不 panic |

### 真库（`water_check_db_tests.rs`，`ERP_DB_URL` 开关，串行）

| 用例 | 钉住什么 |
|:--|:--|
| `db_boundary_quantity_equal_to_level_is_not_low` | 结存 `50.0000` 不建单 → `49.9999` 建单（`qty`/`min_qty`/`gap_qty` 文本 4 位精确）→ 回升到**正好** `50.0000` 即自动收口（`AUTO_RECOVERED`） |
| `db_boundary_micro_level_survives_numeric_roundtrip` | 0.0002 级水位经 `NUMERIC::text` 往返不丢精度（阈值/结存/缺口三处文本），补到阈值即收口 |

## 3. 边界用例抓到的一个真 bug：`replenish::dto::page_slice` 溢出 panic

```rust
// 旧实现
let start = ((page - 1) * page_size) as usize;
```

* `page = 0` → `page - 1` **下溢**（debug 构建 panic）；
* `page = u64::MAX` → `(page - 1) * page_size` **上溢**（debug 构建 panic）。

`page` 是 URL 上随手可改的查询参数（`GET /business/erp/replenish/page?page=18446744073709551615`），
debug/测试构建下 panic = 请求 500。用 rustc 复现同一表达式（`overflow-checks` 默认开）：

```text
thread 'main' panicked at overflow_probe.rs:8:47:  attempt to subtract with overflow   # page=0
thread 'main' panicked at overflow_probe.rs:13:46: attempt to multiply with overflow   # page=u64::MAX
```

修复（`replenish/dto.rs`，3 行）：

```rust
let page = page.max(1);                                     // 0 → 第 1 页（与 normalize_page 同口径）
let start = (page - 1).saturating_mul(page_size);           // 超大页码不溢出
let start = usize::try_from(start).unwrap_or(usize::MAX);   // 落到「越界 → 空页」
```

## 4. 变异反证（证明用例真的会红，不是摆设）

| 变异 | 结果 |
|:--|:--|
| `water_check.rs`：`self.qty < threshold` → `<=` | `stock_policy` 套件 **5 红**（含 `judge_edges_…`、`judge_is_strictly_less_than_threshold`、两条 `db_boundary_*`） |
| `replenish/dto.rs`：`expected_qty < min` → `<=` | `replenish` 2 红（`equal_to_min_does_not_trigger`、`expected_equal_to_min_does_not_trigger`）+ `boundary_tests` 2 红（`suggest_triggers_on_expected_not_on_hand`、`expected_equal_to_min_with_low_on_hand_is_covered`） |
| 两次变异均已还原，还原后全绿（见 §5） | — |

## 5. 实测（2026-11-17，99 开发机 · 库在 145）

```bash
# ① 空库首启（服务自己应用 362 条迁移，含 V0428/V0429/V0430）
createdb -h 172.18.10.145 -U cloudpivot cp_m216_drill
bash m216_start.sh          # NIK_SERVER_LISTEN=127.0.0.1:18216，profile=test
#   schema_migrations: 362 条（428 / 429 / 430 在内），public 表 182 张

# ② 模块套件（纯逻辑 + 真库，串行）
ERP_DB_URL="postgres://…@172.18.10.145:5432/cp_m216_drill" \
  cargo test -p cloudpivot --lib business::erp::stock_policy -- --test-threads=1
#   test result: ok. 61 passed; 0 failed

ERP_DB_URL="…" cargo test -p cloudpivot --lib business::erp::replenish -- --test-threads=1
#   test result: ok. 31 passed; 0 failed（page_slice 修复后无回归）

# ③ 门禁
cargo fmt
cargo clippy -p cloudpivot --lib -- -D warnings            # clean
cargo clippy -p cloudpivot --all-targets -- -D warnings    # clean（含新测试代码）
```

真库套件不是静默跳过：`ensure_db()` 拿不到 `ERP_DB_URL` 会 `return`，所以上述 61 条是**真连库**跑出的
（`db_boundary_*` 两条用 `--nocapture` 能看到各自的 `[M2-16] 真库刀口：… ✓` 输出）。
