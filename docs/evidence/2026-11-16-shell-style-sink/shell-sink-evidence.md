# 壳样式下沉（t_3a87ffac）实测证据

提交：`060fb85a`（origin/145 与 github 同为 `060fb85a`）

## 1. 改了什么

| 项 | 内容 |
|---|---|
| 新增 | `frontend/src/style/shell.less`（151 行，27 个共享类） |
| 挂载 | `frontend/src/style/index.less` 里 `@import './shell.less';`（+5 行） |
| 删除 | 41 个 `.vue` 的 `<style>` 里 248 个规则块（与基线逐字相同 + 迁移后死样式） |
| 模板 / 脚本 / 组件 | **零改动**（见 §4 的逐字节断言） |

## 2. 行数对比（`git diff --numstat`）

```
43 files changed, 156 insertions(+), 1377 deletions(-)   # 净 −1221
```

| 口径 | 改造前 | 改造后 | 变化 |
|---|---|---|---|
| `views/**` 下 shell 选择器行（`grep -c`） | 472 | 217 | −255（−54%） |
| shell 规则块（同一解析器扫 `<style>`） | 446 块 | 198 块 | **−248 块** |
| 这些块的块体行数 | 3317 行 | 1693 行 | −1624 行 |
| `views/**` 全部 `.vue` 总行数 | 138880 | 137503 | **−1377 行** |
| 新增 `shell.less` + `index.less` | 0 | +156 行 | |

> 卡片预估「≈1712 行可删」，实测删 1377 行（文件级）。差额来自两件事：① 4 个类
> 经实测不能全局化（见 §5），它们的 42 块没删；② 块体行数含块内空行，而文件删除后
> 相邻空行会被合并，所以文件级比块体级略少。

## 3. 验真方法（不靠肉眼）

### 3.1 逐元素计算样式比对（主证据）

`probe-all.mjs`：70 个「模板里用到 shell 类」的已注册路由，用 Playwright 打桩
（`/api/business/**` 返空表、`/api/system/dept/**` 返空、`auth/me` 授 `*:*:*`，
菜单接口走真后端保证侧栏一致），前/后各跑一遍，把**每个带 shell 类的元素**的
标签、宽高、`display / font-size / color / gap / flex-* / margin-bottom / padding /
justify-content / align-items / white-space / font-weight / width / min-width`
全量 dump 成 JSON，再逐元素逐属性 diff。

| 阶段 | 差异元素数 | 说明 |
|---|---|---|
| 第一版基线（全部类都发全局） | **98** | 其中 `.panel-empty__icon` 一条就让 20 个页面的空态图标 28px→16px |
| 修正后（本提交） | **14** | 除 2 处有意收敛外，全部无尺寸变化（只是 `flex`/`min-width` 值形同虚设） |

### 3.2 像素级截图比对（旁证）

22 个代表页（财务 4 页 + 未迁 ListPage 的 12 页 + 报表/单位/预算/成本/询价），
前/后各截全屏 PNG，canvas 逐像素比对（容差：单像素四通道差 ≤8 记为字体抗锯齿噪声）：

| 结果 | 页数 | 页 |
|---|---|---|
| 像素完全一致 | 19 | account / opening / year-end / report-item / inventory / voucher / ledger / purchase / receipt / customer / supplier / location / stockin / transfer / period / price / budget-report / report-balance / unit |
| 有差异（有意为之） | 2 | `cost-period`（见 §6）、`price-inquiry`（见 §6） |
| 有差异（打桩噪声） | 1 | `budget-mapping` 18 px 在顶栏 `NaN` 未读徽标处 —— 打桩接口让未读数算出 NaN，两次抓拍徽标宽度抖动；同页 `tool-card/list-card` 的 flex 变化经几何断言为 0 尺寸影响 |

> 对照实验：同一份 dist 连拍两次，也会在 `purchase`/`ledger`/`budget-*` 等页出现
> 2~2000 像素差异（字体提示 + 错误条里的随机 request-id）。所以上表只把「两次独立
> 构建之间有稳定差」的页算作变化。

### 3.3 等效性静态断言

| 断言 | 结果 |
|---|---|
| 每个页面「改造前本地生效声明」vs「改造后（本地 ∪ 全局基线）」逐类比对，`used=True` 的 LOST / CHANGED / NEW | **全部 0**（除 §6 两处有意收敛，它们不在被删文件里） |
| 43 个改动文件里 **模板/脚本区逐字节未变** | ✅ 只有 4 个 `.ts`（别人在改）有差异，`.vue` 全为纯 `<style>` 变化 |

## 4. 门禁

| 检查 | 结果 |
|---|---|
| `pnpm typecheck` | 0 error |
| `pnpm build` | 通过（产物里确认有 `.panel-empty .panel-empty__icon{font-size:28px}` 等全局规则） |
| `bash scripts/check.sh --fast`（L0+L1） | 全部通过（80s / 79s，pre-push 又跑了两遍） |
| pre-commit 三道闸（secret / toml / 库存写入口） | 全绿 |

## 5. 链路里查出的两个「看起来等效、其实不等效」

**① 特指度输给组件库** —— `.panel-empty__icon` 的裸类 (0,1,0) 压不住 TDesign 的
`.t-icon.t-size-m{font-size:16px}`（0,2,0）。原本能到 28px 是因为 scoped 规则是 (0,2,0)。
所以 `shell.less` 里写成 `.panel-empty .panel-empty__icon`（同样是 0,2,0，靠
`index-*.css` 在 `tdesign-*.css` 之后加载取胜）。**这是本次 98→14 差异的主因。**

**② CSS 按属性级联** —— 页面本地只写「半个变体」时（例 `.tool-row__label{color}`），
基线的 `font-size:13px` 会补进来，观感真的变。所以这 4 个类不进基线：

| 类 | 全局化会怎样 |
|---|---|
| `.panel-empty` | 8 个页面空态从块级 `padding+text-align` 变居中 flex（高度/文字宽度都变） |
| `.panel-empty__title` | 补 `font-size:14px + color:secondary`，改字号与颜色 |
| `.panel-empty__desc` | 补 `margin:0`，改行距 |
| `.tool-row__label` | 3 个报表页标签从 14px 缩到 13px |

它们的本地定义原样保留（`scoped` 覆盖生效），清单另立：
见附件 `shell-drift-inventory.md`（486 行，逐类逐变体逐文件）。

## 6. 两处有意收敛（可回滚，但看起来是对的）

| 页 | 变化 | 判断 |
|---|---|---|
| `cost-period` 成本核算 | `#head-extra` 里的提示文字 14px 深色 → 12px 灰（吃到 `.panel-head__tip` 基线） | 该页从未定义过这个类，以前是「忘了写」；现在是全站统一的提示样式。截图确认排版正常 |
| `price-inquiry` 询价 | 空态图标 16px → 28px，空态高度 72→78px | 该页从未定义过 `.panel-empty*`；现在图标与其它 30 个页面一致。截图确认居中正常 |

若不接受，回滚这两页只需给它们补回原来的本地定义，不影响其它 39 页。

## 7. 顺带清掉的死样式

ListPage 迁移后模板里 0 处 `panel-head` / `tool-row` 的页面（purchase / receipt / invoice /
receivable / payable / transfer / stockin / customer / material / supplier …），
样式里的 shell 定义已随本次一并删除（判据：块内定义的**所有类**在该文件模板与它的
子组件模板里都不出现）。

## 8. 复现命令

```bash
# 块数与行数（同一解析器扫 <style>）：见附件脚本 count_blocks.py
cd /opt/cloudpivot/frontend/src
grep -rh "^\.\(panel-head\|tool-row\|tool-card\|list-card\|panel-empty\)" views | wc -l   # 217（改造前 472）
```
