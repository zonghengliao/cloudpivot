# T-ac0466d8 · gallery 77 套配色方案接入云枢（3-4 色 → 5 槽位映射）

> 来源：`docs/design/palette-schemes-77.json`（设计 gallery 提取件，77 套 / 264 个源色）。
> 目标：与现有 22 套「云枢精选」一起出现在 **设置 → 外观 → 配色方案**，可搜索、可分组、刷新与换设备都还在。

## 一、做了什么

| 层 | 文件 | 说明 |
|---|---|---|
| 数据 | `frontend/src/style/palettes.ts` | 拆分 `FEATURED_PALETTES`(22) + `GALLERY_PALETTES`(77) = `PALETTES`(99)；`PaletteId = FeaturedPaletteId \| GalleryPaletteId`；`PaletteDef` 新增 `group` / `source` / `sourceColors`；新增 `PALETTE_GROUPS` + `groupOf()` |
| 生成器 | `scripts/verify/gen_palette_schemes.py` | 从 JSON 生成 gallery 段（带 id/名称表、映射规则、merge 对比度兜底），`--check` 可校验有无漂移；同时产出本目录的 `mapping-report.md` |
| 校验器 | `scripts/verify/check_palettes.py` | 验收 ③④⑤ 的机械判据 + 「264 个源色一条不丢」的色值红线 |
| 引擎 | `frontend/src/style/palettes.ts` | 两处**对比度兜底**修复（细节见 §四）：`ensureContrast` 中间调底色方向、渐变终点油墨选择 |
| 页面 | `frontend/src/views/user/preference.vue` | 搜索框（名称/色名/色值/来源文件/id）+ 三段分组折叠 + 每卡 5 色块/N 色原稿标记；对比度报告收窄为「当前方案 × 明暗 2 组」 |
| 顶栏 | `frontend/src/layouts/components/PaletteSwitch.vue` | 快捷面板只放精选 22 套（当前若是 gallery 方案则置顶），高度上限 + 滚动；页脚改为「共 99 套方案 · 去外观设置看全部」 |
| 后端契约 | `backend/migrations/system/V0397__preference_palette_ids_gallery77.{up,down}.sql` | `sys_preference_def.ui.palette` 的 `oneof:` 白名单从 22 扩到 99（列宽 300→2000），否则选新方案会被 `62022` 拒收 |
| 检查脚本 | `frontend/scripts/check-palettes.mjs` | 数量断言 22→99，新增 gallery 数据自检（分组/来源/源色数/brand·merge 必须来自源色）+ merge 正文对比度 ≥4.5 |

## 二、映射规则（按 HSL 亮度 L 升序）

| 源色数 | brand | brandLight | merge | border | accent |
|---|---|---|---|---|---|
| 4（43 套） | 最暗 | 次暗 | 最亮 | merge −8% L | 次亮 |
| 3（28 套） | 最暗 | brand +22% L | 最亮 | merge −8% L | 中间 |
| 2（2 套） | 最暗 | brand +22% L | 最亮 | merge −8% L | brand 补色 +180° |
| 1（4 套） | 该色 | brand +22% L | brand → L95 | brand → L82 | brand 补色 +180° |

**色值红线**：源色一律原样使用（264 个源色 100% 出现在 palettes.ts 里，见 `check_palettes.py` 的「源色保真」项），
派生槽位逐条列在 `mapping-report.md` 的「派生槽位」列。
**merge 兜底**：本次 0 套触发 —— 所有方案的 merge 上正文（#1A1A1A 或 #F5F5F5 取优）最低 7.28:1，无一需要微调。

## 三、验收逐条

| # | 判据 | 结果 | 证据 |
|---|---|---|---|
| ① | `pnpm typecheck` = 0 error | ✅ exit 0 | `00-verification-log.txt` |
| ② | `pnpm build` 成功 | ✅ ✓ built in 56.45s，exit 0 | `00-verification-log.txt` |
| ③ | `grep -c "id: '" palettes.ts` ≥ 99 | ✅ **102**（99 条目 + 3 个分组元信息行）；`^    id: '` = 99 | 同上 |
| ④ | 5 个槽位均为 `#RRGGBB` | ✅ 495 个槽位非法 0 | `check_palettes.py` |
| ⑤ | merge 上正文对比度 ≥ 4.5 | ✅ 最低 **7.28** | `check_palettes.py` / `check-palettes.mjs`（198 组全通过） |
| ⑥ | 浏览器实测 + 截图 | ✅ 见下表与 `01`~`09` 截图 | 本目录 |
| ⑦ | 现有 22 套回归 | ✅ 22 套逐套点选，`data-palette` 与卡片色块一一对上（`regression-featured-22.json`）；另外引擎改动对既有 22 套 × 明暗 2 模式 **85 个变量逐字节一致**（`scripts/verify/palette_engine_parity.mjs`） |

### 浏览器实测明细（真后端 + 真 PG + vite dev，admin 登录）

| 截图 | 场景 | 关键观测 |
|---|---|---|
| `01-palette-groups.png` | 设置 → 外观 | 「共 99 套」；云枢精选 22 / 古典釉色 20 / 时尚配色 57，后两组默认折叠 |
| `02-search-jiutao.png` | 搜索「九桃」 | 「匹配 1 套」，自动只剩古典釉色分组且展开，卡片「粉彩九桃天球瓶 · 3 色原稿」 |
| `03-select-guaci-jiutao.png` | 选中该方案 | `--td-brand-color-7` `#6f8ab8→#b02a46`、`--td-bg-color-page` `#f2f2ee→#e8e1d6`、`--td-component-border` `#aab7c2→#d9cebc`、`--app-brand-accent` `#b19658→#7a9661`；localStorage 写入 `guci-jiutao` |
| `04-reload-persist.png` | 刷新页面 | 仍是 `guci-jiutao`（`data-palette=guci-jiutao`，`--td-brand-color-7` 仍 `#b02a46`）；后端偏好同时落库（`sys_user_preference.ui.palette`） |
| `05-group-porcelain.png` | 展开全部分组 | 古典釉色 20 套全部渲染；分组头「20 套 · 瓷器/釉色提取」 |
| `06-dark-mode.png` | 切深色 | 页面底 `#231518`、主色提亮为 `#d44d69`、正文 `#eee7e9`、边框 `#68454d`，暗底可读 |
| `07-applied-dashboard-light.png` | 工作台（浅色） | 横幅渐变/按钮/图标/卡片边框全部跟方案走（胭脂红 × 瓷胎白） |
| `08-applied-dashboard-dark.png` | 工作台（深色） | 深底 + 提亮主色，图表与横幅仍协调 |
| `09-quick-switch-popup-dark.png` | 顶栏快捷面板 | 23 项 = 当前 gallery 方案置顶 + 精选 22；页脚「共 99 套方案」 |

## 四、两处引擎兜底修复（为什么必须改）

映射本身按卡片规则执行，但 99 套里 40 套在引擎自检里挂红，根因是引擎的**兜底方向/油墨**假设只对「品牌浅色一定比主色亮」的老 22 套成立：

1. **`ensureContrast` 的中间调盲区**：老实现按 `bgLum > 0.4` 选方向。浅驼 `#BFA475`（相对亮度 0.38）被判为「暗底」→ 只会往白走 → 白字落在驼色上 **2.39:1**（正文/图表/品牌字全挂）。改成「首选方向走不通就反向再走一遍，都走不通取纯黑/纯白里更优的」。
   影响面：`gallery` 里 4 套中间调配色（橡金橘探戈等）。
2. **渐变终点油墨写死深色**：`--app-on-gradient-soft` 恒为「主色 L12 深油墨」，但 4 色方案的渐变终点 = **次暗**源色，可能本身很暗（`#B36D28`）→ 深油墨 **1.11:1**。改成「深油墨达标就保持，不达标才换白字/纯黑」。
   影响面：`gallery` 里 64 组（多数 4 色方案）。

两处都是「先验后取」：**原本达标的组合返回值一个字节都不变** —— 已用 `scripts/verify/palette_engine_parity.mjs` 对 HEAD 版逐变量比对证明（22 套 × 2 模式 × 85 变量 = 3740 个值全等）。
色阶算法（`brandScaleLight` / `brandScaleDark`）、深色模式推导、基准色语义均未改。

## 五、数据疑点（必须说清，未擅自改值）

第 **50 / 52 / 55 / 63** 套源数据只有 1 个色值，且四套都是 `#DDDDDD`
（色名分别是 蜜瓜色 / 深水蓝 / 烤果馅饼 / 树皮色，PANTONE 参考号各不相同）：

| index | id | 名称 | PANTONE | 源 hex |
|---|---|---|---|---|
| 50 | `pms-migua` | 蜜瓜色 | 13-0814 TCX | `#DDDDDD` |
| 52 | `pms-shenshuilan` | 深水蓝 | 18-4032 TCX | `#DDDDDD` |
| 55 | `pms-kaoguo` | 烤果馅饼 | 13-0716 TCX | `#DDDDDD` |
| 63 | `pms-shanxi` | 山系色会 | 16-1506 TCX | `#DDDDDD` |

判断为提取件的**占位色**（一个色卡不可能四套完全同一个近白灰）。按卡面「色值原样保留，不优化」的红线**没有擅自编造色值**，
四套照原样入库（界面上是四张很接近的浅灰方案）。需求方给出真实色值后，改 JSON 重跑
`python3 scripts/verify/gen_palette_schemes.py` 即可，无需改代码。

## 六、复跑命令

```bash
python3 scripts/verify/check_palettes.py                    # ③④⑤ + 源色保真
python3 scripts/verify/gen_palette_schemes.py --check       # gallery 段与 JSON 是否漂移
cd frontend && pnpm check:palettes                          # 99 套 × 2 模式全量对比度 + 色阶断言
cd frontend && pnpm typecheck && pnpm build                 # ①②
cd frontend && node ../scripts/verify/palette_engine_parity.mjs  # 引擎改动对既有 22 套零影响
```
