# B3 逐页有意变化核对表（机械生成）

> 由 `gen-intentional-table.mjs` 从 `logs/after.diff.json`（逐元素探针前后 diff）、
> `logs/pixels-before-after.log`（逐页像素）与 `logs/wrap-*.json`（换行点）三份原始产物生成，**不手抄数字**。
> 「探针几何差异」列只列**几何**（STYLE-only 的基线补充如 `min-width:auto→0px`、`white-space:normal→nowrap` 不占列，见 README §8）。

| # | 页面 | 删掉的本地声明 | 归到哪 | 探针几何差异（路由） | 像素（viewport / toolrow） | 换行点 |
|--:|---|---|---|---|---|---|
| 1 | budget/mapping | `tool-row__type` 150px | 该页已挂 `--dense`；`__type` 吃基线 140 | tool-row__spacer.w 602→612；tool-row__type.x 1078→1088；tool-row__type.w 150→140 | 739（原 1068） / 739 | 未变 |
| 2 | budget/report | `tool-row__keyword` 200px | 吃基线 220 | tool-row__keyword.w 200→220；tool-row__spacer.x 912→932；tool-row__spacer.w 208.7→188.7 | 2333（原 3623） / 2333 | 未变 |
| 3 | cost-alloc-rule | `tool-row__period` 140px | + `--dense`（140 不变，`gap` 不吃） | 无 | 0 / 0 | 未变 |
| 4 | cost-labor | `tool-row__period` 132px | + `--dense` → 140 | tool-row__period.w 132→140 | 6291（原 8785） / 6291 | 未变 |
| 5 | cost-overhead | `tool-row__period` 132px | + `--dense` → 140 | tool-row__period.w 132→140；tool-row__item.x 578→586 | 6321（原 8838） / 6321 | 未变 |
| 6 | ledger（Tab1 总账） | `tool-row__period` 140px | + `--dense`（140 不变，`gap` 不吃） | 无 | 0 / 0 | 未变 |
| 7 | ledger（Tab2 科目余额表） | 同上（同一页） | + `--dense` → gap 12→8，period 140 不变 | ?.CLASS tool-row→tool-row tool-row--dense；tool-row__period.x 368→364 | 1476（原 1684） / 1476 | 未变 |
| 8 | receipt-refund | `tool-row__date` 140px | + `--dense`（140 不变） | 无 | 4（原 8） / 0 | 未变 |
| 9 | report/payable-aging | `tool-row__period` 200 / `__date` 180 | 吃基线 220 / 150 | tool-row__period.w 200→220；tool-row__label.x 552→572；tool-row__date.x 606→626；tool-row__date.w 180→150 | 4336（原 6157） / 4336 | 未变 |
| 10 | stock-out | `tool-row__range` 240px | 吃基线 250 | tool-row__range.w 240→250 | 5640（原 7856） / 5640 | 未变 |
| 11 | transfer | `tool-row__status` 140px | 吃基线 130 | tool-row__status.w 140→130 | 4002（原 6011） / 4002 | 未变 |
| 12 | warehouse | `panel-head__main/__title/__meta` | 三条提进基线（本地原样删） | 无 | 0 / — | 无工具行 |
| 13 | hr/employee | `panel-head__hint`（max-width/line-height/text-align） | `--end`+`--right`；max-width/line-height 归一删 | 无 | 0（原 2） / 0 | 未变 |
| 14 | hr/salary | `panel-head__meta` 13px | 吃基线 12px | panel-head__main.w 113→110；panel-head__meta.w 39→36 | 159（原 189） / 0 | 未变 |
| 15 | account（对照：基线爆炸半径） | —（模板自带 panel-head） | 只吃基线 min-width/nowrap | 无 | 0 / 0 | 未变 |
| 16 | cost-period（对照：UncollectedPanel） | — | 只吃基线 | 无 | 0（原 5） / 0 | 未变 |
| 17 | inventory（对照） | — | 只吃基线 | 无 | 0 / 0 | 未变 |
| 18 | price（对照） | —（15px 标题归 B4） | 只吃基线 | 无 | 0 / — | 无工具行 |
| 19 | report/report-item（对照） | —（15px 标题归 B4） | 只吃基线 | 无 | 0 / 0 | 未变 |
| 20 | sales-order（对照：`hint--end`） | — | 本批**没动** `--end` | 无 | 0 / 0 | 未变 |
| 21 | sales-return（对照：`hint--end`） | — | 本批**没动** `--end` | 无 | 0 / 0 | 未变 |
| 22 | shipment（对照：`hint--end`） | — | 本批**没动** `--end` | 无 | 0（原 10） / 0 | 未变 |

> 像素列格式：`有意义差异像素数（含 mask 内全部）/ toolrow 裁切图同口径`；`0` = 零差异。

## 判读

- **无工具行的页**（price / warehouse）：本批在它们身上只删了 `panel-head__*` 块，像素零差异说明删的是纯冗余。
- **零差异页里的 3 个对照**（cost-alloc-rule / ledger(Tab1) / receipt-refund）：本地就是 140，`--dense` 也是 140 —— 归一后**一个字都不动**，正是「进变体」而不是「改观感」的证据。
- **receipt-refund 4px**：落在右上角 `y7-8`（通知铃区），对照组同位置也有同等量级差异（`logs/pixels-control.log`）→ 噪声，不是本批改动。
- **hr/employee 0px**：默认（超管）态下那段 `panel-head__hint` 不渲染；无权限态的前后对比见 README §4 与 `shots-narrow/`。
