# B2 逐页「有意变化」核对表（探针归因 + 像素 diff）

> 生成方式：`shell-probe-b2.mjs`（before/after 干净 worktree 各一份 dist）→ 逐元素 40 计算属性 + rect；
> `pixels.mjs` 逐页 before/after 与对照组 after/after2。判定「有意义」= mask(0,0,64,64) 之外且单通道差 > 8。

| # | 页面 | 模板追加 token | 探针：样式值变化 | 探针：几何变化 | 像素：有意义差/总差 | 像素 y 区间 | 应有变化（卡面口径） |
|--:|---|---|---|---|---|---|---|
| 1 | asset-category | — | 1× panel-head__tip.color rgb(76, 82, 92)→rgb(121, 130, 144) | — | 1919 / 2912（对照 0/14） | `[[19,29],[31,32],[41,41],[168,181]]` | tip 色变淡 |
| 2 | asset | — | 1× panel-head__tip.color rgb(76, 82, 92)→rgb(121, 130, 144) | — | 2683 / 3975 | `[[167,180]]` | tip 色变淡（gap:8px 是死代码，删了不变） |
| 3 | budget | `panel-head__tip → panel-head__tip panel-head__tip--right` | 1× panel-head__tip.color rgb(76, 82, 92)→rgb(121, 130, 144) | — | 2071 / 3064（对照 0/14） | `[[19,29],[31,32],[41,41],[168,180]]` | tip 色变淡 + 右对齐由 --right 接管 |
| 4 | budget/mapping | `tool-row → tool-row tool-row--dense`<br>`panel-head__main → panel-head__main panel-head__main--nowrap`<br>`panel-head__tip → panel-head__tip panel-head__tip--right` | 1× list-card.height 298px→296px<br>1× panel-head.margin-bottom 12px→10px<br>1× panel-head__main.gap 8px→10px<br>1× panel-head__main.row-gap 8px→10px<br>1× panel-head__main.column-gap 8px→10px<br>1× panel-head__main.width 140px→142px<br>1× panel-head__tip.color rgb(76, 82, 92)→rgb(121, 130, 144) | 1× list-card.h 298→296<br>1× panel-head__main.w 140→142<br>1× panel-head__meta.x 376→378 | 10044 / 20580 | `[[19,29],[31,32],[41,41],[169,181],[197,198],[212,228],[243,245],[296,312],[380,386],[388,403],[407,413],[434,449]]` | 卡片头 mb 12→10（卡高 −2）、__main gap 8→10（宽 +2 / meta x +2）、tool-row 仍 8（--dense）、tip 变淡 + 右对齐、nowrap 由变体保住 |
| 5 | budget/report | `tool-row → tool-row tool-row--dense` | 1× list-card.height 184px→182px<br>1× panel-head.margin-bottom 12px→10px<br>1× panel-head__main.gap 8px→10px<br>1× panel-head__main.row-gap 8px→10px<br>1× panel-head__main.column-gap 8px→10px<br>1× panel-head__main.width 120px→122px | 1× list-card.h 184→182<br>1× panel-head__main.w 120→122<br>1× panel-head__meta.x 344→346 | 9955 / 18446 | `[[287,298],[316,323],[332,357],[366,373],[384,389],[394,409],[416,421],[438,453]]` | 卡片头 mb 12→10（卡高 −2）、__main gap 8→10（宽 +2 / meta x +2）、tool-row 仍 8（--dense） |
| 6 | cost-period | — | 1× panel-head__meta.color rgb(76, 82, 92)→rgb(121, 130, 144) | — | 866 / 1381 | `[[78,78],[80,80],[216,227]]` | UncollectedPanel 的 meta 色变淡 |
| 7 | depreciation | — | 1× panel-head__tip.color rgb(76, 82, 92)→rgb(121, 130, 144) | — | 2489 / 3824 | `[[296,309]]` | tip 色变淡 |
| 8 | expense-type | — | 1× panel-head__tip.color rgb(76, 82, 92)→rgb(121, 130, 144) | — | 2170 / 3255（对照 0/3） | `[[19,29],[31,32],[41,41],[88,88],[167,180]]` | tip 色变淡 |
| 9 | expense | — | 1× panel-head__tip.color rgb(76, 82, 92)→rgb(121, 130, 144) | — | 2147 / 3235 | `[[190,202]]` | tip 色变淡 |
| 10 | expense/pay | — | 1× panel-head__tip.color rgb(76, 82, 92)→rgb(121, 130, 144) | — | 1725 / 2664 | `[[19,29],[31,32],[41,41],[168,180]]` | tip 色变淡 |
| 11 | invoice | `panel-head__tip → panel-head__tip panel-head__tip--end panel-head__tip--right` | — | — | 0 / 14 | `[[19,29],[31,32],[41,41]]` | 零变化（--end + --right 精确接管 auto + right，颜色本来就同色） |
| 12 | ledger?probe=trial | — | 1× list-card.height 364px→344px<br>1× tool-row.margin-top 8px→0px<br>1× tool-row.margin-bottom 12px→0px | 1× list-card.h 364→344<br>1× tool-row.y 144→136<br>1× tool-row__period.y 144→136<br>1× panel-head.y 188→168<br>1× panel-head__main.y 188→168<br>1× panel-head__title.y 188→168<br>1× panel-head__meta.y 188→168<br>1× panel-head__tip.y 188→168<br>1× panel-empty.y 267→247<br>1× panel-empty__icon.y 291→271<br>1× panel-empty__title.y 323→303<br>1× panel-empty__desc.y 349→329 | 25465 / 127173（对照 0/2） | `[[118,118],[136,185],[190,228],[235,266],[272,319],[326,344],[353,364],[375,402],[407,443],[451,483]]` | 工具行外边距消失（上移 8px，卡高 −20，其下整体上移 20px）—— 卡面明列的有意变化 |
| 13 | price | — | 2× panel-head__main.gap 8px→10px<br>2× panel-head__main.row-gap 8px→10px<br>2× panel-head__main.column-gap 8px→10px<br>1× panel-head__main.width 509px→511px<br>1× panel-head__meta.color rgb(76, 82, 92)→rgb(121, 130, 144)<br>1× panel-head__tip.color rgb(76, 82, 92)→rgb(121, 130, 144) | 1× panel-head__main.w 509→511<br>1× panel-head__meta.x 340→342 | 3642 / 3980 | `[[92,105]]` | 第一个头 __main gap 8→10（宽 509→511 / meta x +2）、meta+tip 变淡 |
| 14 | receipt-refund | — | 1× panel-head__tip.color rgb(76, 82, 92)→rgb(121, 130, 144) | — | 4354 / 6638（对照 0/14） | `[[19,29],[31,32],[41,41],[211,224]]` | meta+tip 变淡 |
| 15 | receipt | `panel-head__tip → panel-head__tip panel-head__tip--end` | — | — | 0 / 0 | — | 零变化（--end 接管 margin-left:auto） |
| 16 | receivable | `panel-head__tip → panel-head__tip panel-head__tip--end` | — | — | 0 / 0 | — | 零变化（--end 接管） |
| 17 | receivable?probe=aging | `tool-row → tool-row tool-row--dense` | — | — | 0 / 1（对照 0/1） | `[[134,134]]` | 零变化（--dense 接管 gap:8px） |
| 18 | report/balance-sheet | — | 1× tool-row.margin-bottom 12px→0px | — | 3370 / 4097 | `[[163,189]]` | tool-row 外边距消失 → 与报表头间距 12→8px（相邻 bs-head 自带 margin-top:8px） |
| 19 | report/cash-flow | — | 1× tool-row.margin-bottom 12px→0px | — | 3501 / 4199 | `[[163,189]]` | 同上 |
| 20 | report/income-statement | — | 1× tool-row.margin-bottom 12px→0px | — | 3099 / 3674 | `[[163,189]]` | 同上 |
| 21 | report/payable-aging | — | 1× tool-row.margin-bottom 12px→0px | — | 34302 / 61023 | `[[19,29],[31,32],[41,41],[127,152],[160,175],[190,198],[204,220],[226,249],[253,261],[270,274],[279,297],[304,308]]` | 同上（波及整张报表：KPI 与表体上移 4px） |
| 22 | report/report-item | — | 1× panel-head__meta.color rgb(76, 82, 92)→rgb(121, 130, 144)<br>1× panel-head__tip.color rgb(76, 82, 92)→rgb(121, 130, 144) | — | 2768 / 4061 | `[[19,29],[31,32],[41,41],[279,291]]` | meta+tip 变淡 |
| 23 | sales-order | `panel-head__hint → panel-head__hint panel-head__hint--end` | — | — | 0 / 0 | — | 零变化（--hint--end 接管 auto；__main gap:8px 是死代码） |
| 24 | sales-return | `panel-head__hint → panel-head__hint panel-head__hint--end` | — | — | 0 / 0 | — | 同上 |
| 25 | shipment | `panel-head__hint → panel-head__hint panel-head__hint--end` | — | — | 0 / 0 | — | 同上 |

合计：25 条采集项 / 191 元素；有意义的像素差异 18 页，零有意义差异 7 页；对照组噪声 48 px（全部 maxΔ≤6）。
