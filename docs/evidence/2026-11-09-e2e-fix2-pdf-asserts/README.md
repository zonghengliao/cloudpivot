# E2E-FIX-2 附带：PDF 退货单 / 报表 3 条单测断言过期 —— 收口证据（卡 t_5904f851）

> 一句话：卡面点名的 3 条红是**测试断言过期**（不是产品缺陷），已关闭；
> 复跑时又抓到 **2 条只有带真库才会暴露的既有红**（无库模式静默早退 = 假绿），一并收口。
> 最终全量 lib（带真库、单线程）结果见 §五。

| 项 | 值 |
|---|---|
| 被测树 | `origin/main` = `2859a50e`（干净 worktree `/root/pdfassert-v2`，无他人未提交改动） |
| 测试库 | `cloudpivot_test_pdf07v2`，由 `scripts/verify/make_full_suite_db.sh` 建（295 条迁移 / max V362 / `biz_voucher`=0 / 演示组织已种） |
| 跑法 | `ERP_DB_URL=$U SYS_DB_URL=$U TASK_DB_URL=$U P0FIX_DB_URL=$U NIK_PROFILE=test cargo test --lib -- --test-threads=1`（`$U` = 同一个库，四条串必须同库，否则真库用例早退） |

---

## 一、3 条目标红：`return_pdf` ×2 + `report_pdf` ×1

### 1.1 `return_pdf::{purchase,sales}_return_context_has_source_numbers_and_reason`

**现象**：PDF-04 的提交 `2ac33d11` 把 `templates/return_doc.html.tera` 的表头从 `<th>序列号</th>`
改成 `<th class="c-…">序列号</th>`（`table-layout: fixed` 只认首行单元格的 width，列宽 class 必须留在 `th` 上），
而断言还是**逐字**匹配：

```
return_pdf.rs:1150  assert!(html.contains("<th>价税合计</th>"));
return_pdf.rs:1200  assert!(html.contains("<th>序列号</th>"));
return_pdf.rs:1201  assert!(!html.contains("<th>价税合计</th>"));
```

模板里已经没有裸 `<th>`，三处必然失败。

**收口**（改断言，**不动模板**——列宽是 PDF-04 的修复成果）：

```rust
/// 表头文案存在性：**不绑定 `th` 上有没有 class**。
fn th_has_label(html: &str, label: &str) -> bool {
    let pattern = format!(r"<th\b[^>]*>\s*{}\s*</th>", regex::escape(label));
    regex::Regex::new(&pattern).expect("表头正则编译失败").is_match(html)
}
```

只固定三件事：标签名是 `th`、属性随便、正文恰好是该文案。三处调用点改为
`th_has_label(&html, "价税合计")` / `th_has_label(&html, "序列号")` / `!th_has_label(&html, "价税合计")`。

> 顺带的收益：那条**负**断言现在比原来更严。旧的 `!html.contains("<th>价税合计</th>")`
> 在「销售退货模板真渲染了**带 class 的**价税合计 th」时会漏判（假绿）；新写法会抓到。

### 1.2 `report_pdf::pdf07_real_three_reports_agree_on_shape`

**不是断言过期，是夹具期间选错。** 用例钉死了两条绝对值断言：

* `assert_eq!(cash.left.begin, "-", "上期无凭证 → 期初列必须印「-」")`
* `assert_eq!(net.current, "500.00", ...)`

而夹具期是 `2026-11`（上期 `2026-10`）。R1 口径里「期初 = 期间开始日之前的**历史累计**」
（`load_accounts` 的 `net_before`），所以只要库里在 `2026-10` 之前有凭证，期初就不可能是 `-`。
共享测试库实测（`cloudpivot_test_pdf07`，别的卡夹具留下的残渣）：

```
2026-09  1
2026-10  4
2026-11  6
```

于是期初成了 `700.00`。**这不是模板 / PDF 形状的问题，是「上期必须无凭证」这个前置在共享库上不可满足。**

**收口**：

* 夹具期移到 `2001-02`（上期 `2001-01`）。选它的理由写进了代码注释：
  * `2000-12` 是 `income_statement` / `cash_flow` 的「空期间全 0」夹具期，往里记凭证会打红别人；
  * 全库最早凭证在 2021-05，所以 2001-02 的「期初」天然为 0（印 `-`），且不污染任何真实期间；
* 每条真库用例收尾 `purge_fixture()`（`DELETE FROM biz_voucher WHERE voucher_word='PDF07'`），
  `setup_fixture()` 也会先清 —— 重复跑不积累，退出后对后续用例零影响。

---

## 二、只有带真库才会暴露的 2 条既有红（本条卡附带收口）

> 这两条**都不是**卡面点名的 3 条，是在 §五 的全量真库复跑里抓到的。
> 共同点：`let Some(_guard) = setup().await else { return };` —— **不传 `ERP_DB_URL` 时整条用例静默早退**，
> 所以「无库跑全量 0 failed」永远看不到它们（假绿）。

### 2.1 `contract_handler::tests::manual_sign_from_pending_keeps_approval_instance` —— 夹具把 TEXT 绑到 uuid 列

```
panicked at src/business/erp/contract_handler.rs:2235:10:
造审批中前置: Query { sql: "UPDATE biz_contract SET status = 'PENDING_APPROVAL', approval_instance_id = $1::uuid ...",
  source: Error { kind: ToSql(0), cause: Some("cannot bind parameter to column type uuid") } }
```

夹具写的是 `vec![Value::from(instance.to_string()), ...]`（TEXT），而参数位是 `?::uuid`。
**这条用例从落地起就没在带库模式跑通过**，跟父卡 E2E07-FIX-4 的行为变更无关
（父卡把 `approval_instance_id` 保留作审计，这条断言本身仍然成立）。

**收口**：`Value::from(&instance)`（直接绑 `Uuid`，与同仓 `Value::from(&id)` / 父卡新用例同口径）。

### 2.2 `stocktake_tests::lnk04_excluded_inventory_actions_do_not_double_write` —— 预置规则补齐后判据过宽

```
panicked at src/business/erp/stocktake_tests.rs:1395:5:
排除清单里的动作不许产生任何执行记录，实际：[]
```

`实际：[]` 只打印 `runs`（空），真正非空的是 `deferred`：`0dad364d`（E2E07-FIX-2）
给 wired 触发点补齐预置规则后，`STOCK_TAKE/POSTED` 上合法地多了一条**提交后**动作，
于是 `excluded.runs.is_empty() && excluded.deferred.is_empty()` 不再成立。

**收口**：与同卡已修的两处同款口径（`e38ad85e` 的 payable / purchase）——判据收敛到
「**被排除的那些动作**没有执行记录」，`runs` / `deferred` 里按 `STOCKTAKE_EXCLUDED_ACTIONS` 过滤；
「库存只落一次 / 凭证只有一张」等真判据一条没动；用例下方「不传排除清单 ⇒ 引擎真的会跑它」
的反证保留，所以新断言**不是空转**（排除清单一旦失效，`stray` 立刻非空）。

---

## 三、有牙齿证明（摘掉修复 → 红；还原 → 绿）

| 修复 | 摘掉后 | 还原后 | 证据 |
|---|---|---|---|
| `return_pdf` 的 `th_has_label` | 回到 `d9d25d83^` 的逐字版本 → `assertion failed: html.contains("<th>序列号</th>")`，**2 failed**（正是卡面那两条） | **10 passed** | `teeth_return_pdf.txt` |
| `report_pdf` 的夹具期间 | 回到 `2026-11` 夹具，在**被污染的** `cloudpivot_test_pdf07` 上跑 → `left: "700.00" / right: "-"`（正好是原始根因） | 同一污染库上 **1 passed** | `teeth_pdf07.txt` |
| `manual_sign_…` 的绑定 | 修复前版本 → `ToSql(0) cannot bind parameter to column type uuid` | 修复后 **passed** | `mansign_red.txt` / `targeted_all.txt` |
| `stocktake lnk04` 的判据 | 修复前版本 → `排除清单里的动作不许产生任何执行记录，实际：[]` | 修复后 **passed** | `stocktake_red.txt` / `targeted_all.txt` |

关键点：`pdf07` 的牙齿是在**有 2026-09/10 凭证的污染库**上做的 —— 说明修复不是靠「换了个干净库」蒙过的。

## 四、定向复跑

```
business::erp::{return_pdf, report_pdf}                      → 20 passed / 0 failed
business::erp::stocktake_tests + contract_handler::manual_sign + 上面两个 → 43 passed / 0 failed
```

证据：`targeted_pdf.txt`、`targeted_all.txt`。

## 五、全量 lib（带真库、单线程）

见 `full_suite.txt`（由本目录 README 同批次提交）。跑法同页首表格。

## 六、给流程的两条提醒

1. **3 条 PDF 红的修复代码此前已经进了 main，但是被别的卡的全仓 rustfmt 提交 `d9d25d83`
   裹挟进去的**（该提交标题是 `chore(fmt)`，正文只说格式）。所以本卡看起来「没有代码改动」，
   实际是历史事故：共享工作区里未提交的修复被别人的 `cargo fmt` 一锅端走，
   diff 混在 537 个文件里没人看得见。→ **建议：`cargo fmt` 只提交自己改过的文件**（`git add -p` / 按路径 add）。
2. **「全量跑绿」必须带 `ERP_DB_URL`**。本卡的两条既有红在无库模式下 100% 假绿
   （`setup().await else { return }`），父卡 E2E07-FIX-4 报的「全量无库 3957 passed/0 failed」
   就漏掉了它们。→ **建议：全量门禁脚本用 `make_full_suite_db.sh` 建库再跑，并断言无早退。**
