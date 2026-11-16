# 外币手工凭证 `source_no` 超 `varchar(32)` → 建单 500（M1-FIX · t_112c3502）

**结论：已修复。** 原币合计 ≥ 1000 的外币手工凭证现在能建出来（HTTP 200，单号逐字落库），
`biz_voucher.source_no` 由 `VARCHAR(32)` 放宽到 `VARCHAR(64)`（迁移 `V0419`），
并在 `voucher::post_voucher_fx` 加了**列宽护栏**（越界 ⇒ 400 + 10001，不再是 500）。

- 修复提交：见 `git log --grep=t112c3502`
- 卡：`t_112c3502`（M1-04 遗留，M1-11 验证时发现）
- 证据文件：本目录 `01`~`05`，全是**真跑出来的**输出（`.out`，非手工摘抄）

---

## 一、缺陷现场（修前）

`MANUAL` + 外币的 `source_no` 是拼出来的：

```
手工凭证 {原币合计} {币种}（本位币 {本位币合计}）
```

字符数（python3 实算，见 §三）：**33 字** 就已经超过 `VARCHAR(32)`。

`05-http-before-vs-after.out` 的 ①（同一个库、同一条 curl 体、只差迁移）：

```
source_no 宽度 = 32
HTTP 500  {"code":10004,"msg":"服务器内部错误"}
落库凭证行数 = 0 （500 时事务整笔回滚，不该留半张）
```

与卡面 PG 日志一致：`ERROR: value too long for type character varying(32)`。

## 二、修法：为什么是「放宽列宽」而不是「把文案写短」

先查了 `biz_voucher.source_no` 的历史用途（卡面要求「别拍脑袋」）：

| 用途 | 位置 | 是否依赖文案格式 |
|:--|:--|:--|
| 列表「业务单号 / 来源单号」列原样显示 | `frontend/src/views/.../voucher/index.vue` | **不解析**，纯文本 |
| 关键词搜索 `source_no LIKE %x%` | `voucher_query.rs::page` | 不解析 |
| `source_doc_no = COALESCE(显式传入, source_no)` 兜底 | `voucher/mod.rs::post_voucher_fx` | 不解析 |
| 来源单据跳转（`source_type → 路由`） | `frontend/.../shared/source-route.ts` | `MANUAL` 分支**不跳**（`label: '手工凭证'`） |
| 辅助核算 / 联动按 `source_no` 归集 | `voucher_linkage.rs`、各 `*_bridge` | 按等值 / 前缀匹配，不看格式 |

**结论：没有任何代码解析这条文案**，所以「改文案」不会被谁拦住；
但**文案里那两段金额是 M1-04 刻意加的** —— 外币单只写「手工凭证 100」在清单页会被读成 100 元人民币。
缩文案就得把「本位币合计」搬走，等于把设计意图再拆一次。

⇒ 选**放宽列宽到 `VARCHAR(64)`**：文案一字不改（前端 / e2e / 证据里的旧断言全部继续有效），
代价为零（PG 的 `varchar(n)→varchar(m)` 是元数据变更，不重写表、不重建索引），
而且列宽**仍然是有界护栏**（不是 TEXT，见 §四的自检二 65 字必被拦）。

## 三、64 是算出来的上限，不是随手取的（python3 实算）

固定部分 `15` 字 = `手工凭证 `(5) + ` `(1) + 币种 3 位(3) + `（本位币 `(5) + `）`(1)；
两个金额串各 ≤ `21` 字（`NUMERIC(20,4)` 的极限：16 位整数 + `.` + 4 位小数）
⇒ 最坏 `15 + 21 + 21 = 57` ≤ 64（余量 7）。

| 原币合计 | 本位币合计 | 长度 | 旧列宽(32) | 新列宽(64) |
|:--|:--|:--:|:--:|:--:|
| 100.0000 | 850.0000 | 31 | ✅ | ✅ |
| 500.0000 | 4250.0000 | **32** | ✅（边界内侧） | ✅ |
| 1000.0000 | 8500.0000 | **33** | ❌ 22001 → 500 | ✅ |
| 120000.0000 | 1020000.0000 | 38 | ❌ | ✅ |
| 1000000000.0000 | 8500000000.0000 | 45 | ❌ | ✅ |
| 1234567890123456.1234 | 1234567890123456.1234 | 57（金额列极限） | ❌ | ✅（余量 7） |

M1-04 的单测夹具是 100 / 7.2（31 字）——**边界值没覆盖**，所以当初红不了。

## 四、改了什么

### ① 迁移 `V0419__voucher_source_no_width`（新增，只追加）

- §1 `source_no VARCHAR(32) → VARCHAR(64)`：**只放宽不收窄**（已是 ≥64 就跳过 ⇒ 重放幂等，不会把别人的更宽值改窄）；
- §2 自检一：`source_no` 与 `source_doc_no` **两列都 ≥ 64**。
  为什么查两列：`post_voucher_fx` 同一次 INSERT 里 `source_doc_no = COALESCE(显式传入, source_no)`
  （`source_doc_no` 由 V0357 建列就是 64）。只放宽一半就等于「靠巧合对齐」，谁把 `source_doc_no` 改窄都会当场翻红；
- §3 自检二（行为探针）：33 字 / 57 字**能写**，65 字**仍被 22001 拦**（证明改的是「到 64」，不是改成 TEXT），探针行清场后残留 0；
- `down.sql`：有 `> 32` 字的 `source_no` 就**拒绝**收窄（收窄=截断业务单号）；
  收窄只动 `source_no`，**绝不碰** `source_doc_no`（V0357 的 64 不能跟着回滚）。

### ② Rust：文案抽成纯函数 + 列宽护栏

- `voucher_write::manual_source_no(debit_total, base_total, currency)` —— 文案逐字不变，只是可单测，
  并在文档注释里写死长度预算（改文案的人先看它）；
- `voucher::SOURCE_NO_MAX_CHARS = 64` + `check_source_len(field, value)`：`post_voucher_fx`
  **所有凭证写路径的唯一收口**（手工 / 收货 / 应付 / 付款 / 红冲 / 年结 / 期初）里先校验
  `source_no` / `source_doc_no`，越界 ⇒ `DbError::InvalidValue` ⇒ handler 翻 **400 + 10001**，
  文案点名字段 + 实际位数 + 列宽上限。按**字符**计数（`chars()`），对齐 PG `varchar(n)` 口径
  （64 个汉字 = 192 字节也必须放行）。

> 自动凭证路径（收货 / 应付 / 付款 / 退货）的 `source_no` 是业务单号，谁也不能保证永远 ≤ 64 ——
> 这条护栏把「未来某天单据号变长」从 500 变成一句能照着改的 400。

## 五、验证（三层，全部真跑）

| 层 | 内容 | 结果 | 证据 |
|:--|:--|:--|:--|
| 迁移（psql，145 克隆库） | up 两遍幂等 / 自检一·二 / down 数据护栏 / down 收窄 / 再 up | **15 PASS / 0 FAIL** | `01-migration-psql.out` |
| 迁移引擎（真二进制 + 真库） | 模板库 V0404 → 启动 → 应用 V0405..V0419（339→354，405..419 共 15 条）；重启 **0 条重放**；列宽 64 | **11 PASS / 0 FAIL** | `02-migrate-engine-and-http.out` |
| HTTP 真接口 | 外币手工凭证 100 / 500 / 1000 / 120000 都 **200** 且 `source_no`/`source_doc_no` 逐字相等（31/32/33/37 字）；`0.0001` 不被误伤；借贷不平仍 400+10001 | **21 PASS / 0 FAIL** | `02-…out` |
| 真库单测 | `cargo test --lib business::erp::voucher`（= 115 条：voucher_write 51 + mod/query/linkage） | **115 passed / 0 failed** | `03-unit-tests.out` |
| 反证（去掉迁移） | 收窄回 32 后同一条边界用例**红**（`source_no 必须是 VARCHAR(64)`），修前 HTTP **500 + 10004** | 红 | `04-red-without-migration.out` / `05-http-before-vs-after.out` |

新增 4 条单测（`voucher_write.rs` 的 M1-04 段末）：

1. `m1fix_source_no_length_budget_holds_for_worst_case` —— 纯逻辑：31 / 32 / 33 / 38 / 57 字**逐字**断言 + 长度预算 ≤ 64；
2. `m1fix_source_len_guard_boundaries` —— 纯逻辑：64 放行 / 65 拒绝（点名字段与位数）/ 64 个汉字放行（按字符不按字节）/ `None` 不校验；
3. `m1fix_source_no_boundary_cases_are_persisted_verbatim` —— **真库**：31 / 32 / 33 / 38 四档建单成功，
   出参 + 库行 + `source_doc_no` 兜底**逐字**等于预期（不是「只断言不报错」）；
4. `m1fix_over_long_source_no_is_a_param_error_not_500` —— 真库：65 字 ⇒ `DbError::InvalidValue`
   （handler 翻 400 + 10001），且**一行都不落库**。

### 环境噪音（与本次改动无关，已单独反证）

- `business::erp::voucher::tests::account_mapping_uses_voucher_rule` 红：模板库
  `biz_voucher_rule` 是**空表**（V0149⑤ 种子的历史残留）。在自己的克隆库里补那条种子后转绿（`03` 就是补种后的 115/115）。
- `business::erp::fx_revaluation` 6 条红：模板库带一条 2026-11-09 的 USD 应收
  `AR-20261109-000018`（226.00），调汇是全表口径 ⇒ 226×0.7=158.2 混进期望 800（实测 958.2）。
  删掉这条残留后 **14/14 绿**；`fx_ledger` 单独跑 **11/11 绿**。两处都是**演练库数据**，与本卡 diff 无关。
- 并行跑（默认 `--test-threads`）时偶发 `query failed: ... connection closed`：
  本仓 `db()` 是全局连接池 + 每个测试各自 tokio runtime，并行会串；`--test-threads=1` 全绿。
  `03` 就是 **serial** 跑的（115/115）。

## 六、影响面 / 未做

- **前端零改动**：文案没变，列宽只影响能装多长；列表「业务单号」「来源单号」、凭证溯源
  （`source_doc_no`）、关键词搜索都按行原样读，`MANUAL` 本来就不做来源跳转。
  前端/ e2e 里 grep 过 `手工凭证 ` 的断言（`vtrace_*`、`p7_payable_voucher.sh`、`demo_seed.sh`）
  都是「本轮位币」文案或纯文本比较，不涉外币格式。
- **数据回填：无**（0 行改写）。历史 31/32 字的单号在 64 的列里语义不变。
- **演练库已收场**：`cp_t112c3502_t1/t2/t3`、`cp_t112c3502_http` 全部 DROP；
  模板库 `cloudpivot_test` **一个字节都没动**（所有夹具只落在克隆库）。
- **不做（只报）**：`total_debit/total_credit` 是 `NUMERIC(20,4)`，原币 × 汇率超过 16 位整数时
  会由金额列报错（另一类边界，与 `source_no` 无关）；真要收口应给金额加显式上限校验。
