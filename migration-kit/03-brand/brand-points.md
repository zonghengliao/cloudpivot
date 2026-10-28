# 品牌与配色落点清单（brand-points.md）

> 本文件是 `03-brand/` 的说明手册：**品牌名替换点** + **配色演进史** + **每个颜色的语义与实测对比度** + **手工降级路径**。
> 一键执行：`bash 03-brand/apply.sh`（`--dry-run` 预览 / `--check` 只校验）。

---

## 1. 品牌资产

| 资产 | 仓库位置 | 规格 | 来源 |
|------|----------|------|------|
| 主 logo | `frontend/public/logo.png` | 512×512 PNG RGBA，26,537 B | `03-brand/logo-512.png` |
| favicon | `frontend/public/favicon.png` | 64×64 PNG RGBA，2,358 B | `03-brand/favicon-64.png` |
| 配色变量（整文件覆盖） | `frontend/src/style/theme.less` | 242 行 | `03-brand/theme-final.less` |
| 组件色值替换 | BasicLayout / index.less / login / dashboard / change-password | 5 文件 | `03-brand/replace-colors.py` |

- 上游原始 logo/favicon 是同一张 626,968 B 的图（几乎原图直接放 `public`），替换后体积降到 1/23。
- **落地方式已变**（本轮）：以前靠 `01-theme-morandi.patch` 硬贴，Pro 版改了组件就冲突；
  现在 = **theme.less 整文件直放**（无上下文，永远打得上）+ **按色值替换组件硬编码**（不依赖行号）。
  两个旧 patch（`01-theme-morandi.patch` / `02-brand-assets.patch`）**仅作历史留档**，`apply.sh` 不再调用。

---

## 2. 配色演进史（4 代，含每次被否掉的原因）

| 代 | commit | 内容 | 结局 |
|----|--------|------|------|
| ① 上游青蓝 | `c4e8950` 基线 | 主色 `#0580a2`（H=193），取自旧 logo 的「深蓝→青绿」渐变；品牌色阶 10 级全锁 H=193 | 被 ② 取代 |
| ② 莫兰迪四色 | `7be241f`（原 `b94f8d6` 同批） | 深海蓝 `#51697C` 主色 + 霜月白 `#F2F2EE` + 雾蓝灰 `#8C9BA5` + 漂木棕 `#B08968` | 被 ③ 取代（四色偏灰，主色白字对比度虽高但整体发闷） |
| ③ 雾蓝流金七色板 | `2794c40` | 晨雾蓝 `#6F8AB8` 主色（H≈218）+ 雾散初光 + 金辉交融 + 流金渐染 + 落日流金 + 绛红渐染 + 朱砂绛红；浅/深双模式重建 | 页面底色 `#E5EAE1`（金辉交融）偏绿，被 ④ 修正 |
| ③b 一次尝试（已 revert） | `1172658` → `ad55727` | 试过**五色渐变横幅**（金/红一起上），太重 | **`ad55727` 已 revert**；横幅回到 `--app-gradient-brand` |
| ④ **最终版** | `f616b69` | 底色回**霜月白 `#F2F2EE`**、边框回**雾蓝灰 `#AAB7C2`**、深海蓝 `#51697C` 降级为**辅助色** `--app-brand-secondary`；**横幅保持原品牌渐变不动** | 当前线上态 |

> ⚠ **横幅不要再加五色渐变**：`--app-gradient-brand` = `linear-gradient(135deg, #384c6d 0%, #6f8ab8 55%, #8ba0c7 100%)`（深蓝 → 晨雾蓝 → hover 档）+ 白字。
> `apply.sh` 末尾有一条断言：该变量必须是**三档**，档数不对就报错 —— 防止有人把 `1172658` 那次五色渐变又搬回来。

---

## 3. 最终色板（语义 + 对比度实测）

对比度按 WCAG 2.x 相对亮度公式实测（`contrast.py` 可复算），**全部为脚本算出的真实值**，不是估计：

| 角色 | 名称 | 色值 | 用在哪 | 白字对比度 | 对霜月白 #F2F2EE |
|------|------|------|--------|-----------|------------------|
| 品牌主色 | 晨雾蓝 | `#6F8AB8` | `--td-brand-color-7`、主按钮、链接、选中态、图表第 1 色 | **3.50**（大文本 ✔ ≥3:1，正文级 ✘ <4.5） | 3.12 |
| 严格 AA 白字底 | — | `#56729F` | `--app-brand-strong` / `--td-brand-color-8`、小号白字按钮 | **4.88 ✔ AA** | — |
| 品牌深边框 | 雾散初光 | `#A5B3CB` | `--td-brand-color-5`、深边框、暗色模式次要文字 | 2.12（**不可作白字底**） | — |
| 页面底色 | 霜月白 | `#F2F2EE` | `--td-bg-color-page`、body / 布局内容区 | — | — |
| 卡片底 | 纯白 | `#FFFFFF` | `--td-bg-color-container`（与霜月白拉开一档） | — | 1.16 |
| 次级容器 | 暖白 | `#FAFAF7` | `--td-bg-color-secondarycontainer`、表头 `--app-table-head-bg` | — | — |
| 描边 | 雾蓝灰 | `#AAB7C2` | `--td-component-border` / `--td-border-level-2-color`、输入框边框 | — | 1.82（描边按图形 3:1 放宽也仅作装饰边框） |
| 分割线 | 雾蓝灰浅阶 | `#DCE2E6` | `--td-component-stroke` / `--td-border-level-1-color` | — | 1.16 |
| 辅助色 | 深海蓝 | `#51697C` | `--app-brand-secondary`（次级操作、深层文字、侧栏强调） | **5.74 ✔ AA** | 5.11 |
| 辅助色 hover | — | `#6B8296` | `--app-brand-secondary-hover` | 3.99 | — |
| 强调 / 点缀 | 落日流金 | `#C1AC7B` | `--app-brand-accent`：标签、徽标、统计竖线、进度强调 | 2.22（**不可作白字底**） | — |
| 强调深档 | — | `#A8905E` | `--app-brand-accent-deep`，需要白字的强调底 | **3.08（未达 AA 4.5，见 §7 已知偏差）** | — |
| 次要警示 | 绛红渐染 | `#CFAAAB` | `--app-danger-soft` / `--td-error-color-hover`、浅绛标签底 | 2.10（**不可作白字底**） | — |
| 危险 / 错误 | 朱砂绛红 | `#C17B7E` | `--app-danger` / `--td-error-color`、错误图形与标签 | 3.28（大文本 ✔） | — |
| 危险深档 | — | `#A86064` | `--app-danger-deep`，白字底 | **4.64 ✔ AA** | — |
| 主文字 | 深蓝灰 | `#2F3A4D` | `--td-text-color-primary` | 对卡片白 **11.46** ✔ / 对霜月白 **10.21** ✔ | — |
| 次要文字 | — | `#5A6377` | `--td-text-color-secondary` | 对卡片白 **6.02** ✔ / 对霜月白 **5.37** ✔ | — |
| 占位文字 | — | `#7A8A98` | `--td-text-color-placeholder`（按 3:1 放宽） | 对卡片白 3.55 / 对霜月白 **3.16** ✔ | — |
| 暗色主色 | — | `#6381B4` | 暗色 `--td-brand-color-7` | 白字 3.94（大文本 ✔） | 对深底 `#171B20` 4.39 |
| 暗色主文 / 次文 | — | `#E8EBEE` / `#A5B3CB` | 暗色 `--td-text-color-{primary,secondary}` | 对 `#171B20` 分别 14.45 / 8.16 ✔ | — |

**语义色规则**（`replace-colors.py` 遵守）：成功/警告/错误**只做低饱和调整、保留可辨色相**，绝不改成灰：

| 语义 | 上游 | 最终 | 说明 |
|------|------|------|------|
| 成功 | `#12B76A` | `#3E9B72` | 仍是绿 |
| 警告 | `#F5822B` | `#D98B4A` | 仍是橙 |
| 危险 | `#F54A45` | `#C17B7E` | 朱砂绛红（白字底用 `#A86064`） |

> 购物车/徽标一类的「大面积色块」一律避免：点缀色只落在统计竖线、标签底、标签页下划线、选中浅底、柔光这类小面积处。

---

## 4. 品牌名替换点（与 `01-rename` 呼应，逐处核对）

改名脚本按 `cailu→cloudpivot / Cailu→CloudPivot / 菜芦→云枢` 全仓替换，以下是**用户可见**的关键点：

| # | 位置 | 内容 | 改后 |
|--:|------|------|------|
| 1 | `frontend/index.html` | `<title>菜芦通用后台</title>`、`<link rel="icon">` | `云枢通用后台` |
| 2 | `frontend/src/layouts/BasicLayout.vue` | 侧栏顶部品牌区：logo `alt`、系统名、折叠后只显 icon | `云枢` |
| 3 | `frontend/src/views/login/index.vue` | 登录页大标题 + 副标题 + logo | `云枢(CloudPivot)` |
| 4 | `frontend/src/router/index.ts` | 路由 `meta.title`（面包屑/标签页/浏览器标题） | 继承新名 |
| 5 | `frontend/src/stores/setting.ts` | `SETTING_KEY = 'cloudpivot-setting'` | ⚠ 老用户偏好重置一次（见 `01-rename/special-cases.md` 特例 6） |
| 6 | `frontend/package.json` + `package-lock.json` | `"name": "@cailu/web"` | `@cloudpivot/web` |
| 7 | `backend/Cargo.toml` + `Cargo.lock` | crate `name = "cloudpivot"` | release 产物 `target/release/cloudpivot` |
| 8 | `backend/src/main.rs` / `lib.rs` | `use cailu::...` → 与 crate 名同步 | 编译期强约束 |
| 9 | `backend/config/config.toml` | `app_name`、`[log] file_prefix`、`[database] name` | 日志 `cloudpivot.log`、库名 `cloudpivot` |
| 10 | `frontend/.env.development` / `.env.production` | `VITE_APP_TITLE` | 新名 |
| 11 | `frontend/deploy/nginx.conf` / `docker/frontend/nginx.conf` | `server_name`、access_log 路径 | 新名 |
| 12 | `docker/docker-compose.yml` / `.prod.yml` / `Dockerfile*` / `init-db.sql` | 容器名、镜像 tag、库名、用户 | 新名 |
| 13 | `README.md` / `AGENTS.md` / `llms.txt` / `skill/SKILL.md` / `docs/*.md` | 项目自述 | 新名 |
| 14 | `backend/src/shared/startup_guard.rs` | 报错文案里的 env 路径 `/etc/cailu/<env>.env` | `/etc/cloudpivot/<env>.env` |

> **系统层不在仓库里**（阶段 2 已完成，别在仓库里改）：`/etc/systemd/system/cloudpivot.service`、
> `/etc/cloudpivot/*.env`、`/var/log/cloudpivot*`、`/opt/git/cloudpivot.git`、
> nginx `/etc/nginx/conf.d/cloudpivot.conf`、PG 库 `cloudpivot` / `cloudpivot_dev` / `cloudpivot_test`。

---

## 5. 组件里到底改了哪些硬编码（`replace-colors.py` 干的活）

三段能力，按顺序执行：

1. **色值映射表（52 条 / 6 组）**：上游青蓝色系 → 最终色板。
   覆盖三种写法：6 位 hex、8 位 hex（带 alpha，只换 RGB 保留 alpha）、
   `rgb(5 128 162 / 12%)` / `rgb(5,128,162)` / `rgba(5,128,162,.12)`（统一输出现代语法 `rgb(R G B / A)`）。
2. **片段替换（20 条）**：换色值换不出来的「变量化 / 结构化」改动，按上游原文精确匹配：
   | 文件 | 片段 | 改成 |
   |------|------|------|
   | `layouts/BasicLayout.vue` | `<t-avatar  size="medium">` | 挂上 `class="basic-layout__avatar"`（原 CSS 是死代码） |
   | 同上 | 品牌区下沿 `--td-component-stroke` | `--app-accent-soft`（流金细线） |
   | 同上 | 顶栏投影 `rgb(0 0 0 / 4%)` | `rgb(111 138 184 / 12%)` |
   | 同上 | 头像渐变 `#4b8bff→#7b5cff` | `var(--td-brand-color-7)→var(--app-brand-accent)` |
   | `style/index.less` | 卡片投影 | `rgb(111 138 184 / 10%)` |
   | 同上 | — | 新增 `::selection`（流金浅底） |
   | 同上 | — | 新增 `.t-table thead th`（`--app-table-head-bg` 暖白表头） |
   | 同上 | — | 新增 `.table-toolbar-count::before`（流金短竖线） |
   | 同上 | 滚动条 `rgb(0 0 0 / 16%,26%)` | `rgb(111 138 184 / 20%,32%)` |
   | `views/login/index.vue` | 页面渐变 135deg | `var(--app-login-bg)` |
   | 同上 | 品牌区渐变 155deg | `var(--app-login-brand)` |
   | 同上 | 卡片边框 `rgb(255 255 255 / 70%)` | `var(--td-component-stroke)` |
   | 同上 | 柔光 `rgb(122 142 255 / 22%)` | `rgb(193 172 123 / 26%)`（流金） |
   | `views/user/change-password.vue` | `var(--app-*, #fff / rgb(0 0 0 / …))` | fallback 对齐最终色板（`#f2f2ee` / `#2f3a4d` / `#5a6377` / `#ffffff`） |
   | 同上 | 卡片投影 `rgb(0 0 0 / 8%)` | `rgb(111 138 184 / 12%)` |
   | `views/dashboard/index.vue` | 指标卡 `#3b6dff` | `var(--td-brand-color-7)` |
3. **残留 / 告警检查**：跑完立刻复扫一遍 ——
   - 映射表里的上游色值必须**清零**（hex + rgb 两种形式都查）；
   - 出现**映射表之外**的色值 → `[warn]` 列出（Pro 版可能引入新色，需人工判断）；
   - 灰度（`#fff` `#000` 灰阶）与 theme 里已定义的新色板不算未映射。

**幂等**：替换后的值不再是映射键，片段替换带 marker 判重 → 连跑两次第二次「0 文件 / 0 处」。

---

## 6. 手工降级路径（脚本不好使时）

1. `cp 03-brand/theme-final.less frontend/src/style/theme.less`（与页面结构无关，成功率最高）。
2. `python3 03-brand/replace-colors.py --root . --dry-run` 看要改什么，再真跑。
   若片段替换报 `[skip] 锚点不存在`（Pro 改了结构），按 §5 表格**手工**补那几处 CSS —— 它们只影响观感，
   不影响功能，可以分批做。
3. `cp 03-brand/logo-512.png frontend/public/logo.png`、`cp 03-brand/favicon-64.png frontend/public/favicon.png`。
4. 品牌文案按 §4 表格逐个改（`grep -rn "菜芦\|cailu" frontend/src` 对照）。
5. 验证：`bash 03-brand/apply.sh --check` + `pnpm build` + 肉眼过登录页/工作台/暗色切换。

---

## 7. 已知偏差（诚实记录，本轮**不动线上代码**）

| # | 现象 | 影响 | 处理建议 |
|--:|------|------|----------|
| 1 | `theme.less` 头部注释仍写「金辉交融 #E5EAE1 → 页面底色」（③代遗留），实际 `--td-bg-color-page: #f2f2ee`（④代霜月白） | **纯注释**，无运行时影响 | 下次动 theme 时顺手改注释 |
| 2 | `views/user/change-password.vue` 的 `var(--app-page-bg, #e5eae1)` fallback 是 ③代金辉交融 | 变量永远有定义，fallback 不生效 → **无视觉影响** | 迁移包内已改为 `#f2f2ee`（重放产物是「正确值」），线上待下次配色改动一并修 |
| 3 | `theme.less` 注释里 `#A8905E` 白字 4.52、`#5A6377` 对霜月白 4.94 两个数与实测（3.08 / 5.37）不符 | 注释误差；`--app-brand-accent-deep` 实际**不能**当小号白字底（应改用 `#56729F` / `#51697C`） | 本节表格的实测值为准 |
| 4 | `dashboard/index.vue` 上游是 CRLF 换行，④代提交把整个文件规范化成 LF（945 行 diff 里只有 3 行真实改动） | 无功能影响；重放产物保留上游 CRLF（脚本不擅自改行尾），所以与线上**逐字节不同**但色值一致 | 不处理 |

---

## 8. 与 Pro 的品牌冲突处理

Pro 可能自带新 logo 与新配色。**我们的品牌是最终态**（老大已定云枢视觉），Pro 资源一律让位：

```bash
# 1) Pro 若把颜色写进新组件（不经过 theme.less），用脚本一次性扫出来替换
python3 migration-kit/03-brand/replace-colors.py --root . --dry-run
# 2) 看 [warn] 未映射色值：这些是 Pro 新引入的颜色，人工判断归属
#    - 属于品牌色 → 加进 COLOR_MAP_GROUPS 再跑
#    - 属于语义色（成功/警告/错误）→ 只降饱和，别改成灰度
#    - 属于中性灰 → 不用管
```
