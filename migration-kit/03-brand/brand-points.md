# 品牌替换点清单（brand-points.md）

## 1. 品牌资产

| 资产 | 位置 | 规格 | 来源 |
|------|------|------|------|
| 主 logo | `frontend/public/logo.png` | 512×512 PNG RGBA，26,537 B | `03-brand/logo-512.png` |
| favicon | `frontend/public/favicon.png` | 64×64 PNG RGBA，2,358 B | `03-brand/favicon-64.png` |
| 配色变量 | `frontend/src/style/theme.less` | 208 行改动 | `03-brand/theme.less` |
| 视觉补丁 | BasicLayout / index.less / dashboard / login / change-password | 6 文件 | `03-brand/01-theme-morandi.patch` |

- 上游原始 logo/favicon 是同一张 626,968 B 的图（几乎是原图直接放 public），替换后体积降到 1/23。
- `apply.sh` 用 patch（`git apply --binary`）而非 cp：**配色 = 6 个文件**，只换 `theme.less`
  会漏掉布局/登录页/改密页里的语义色与阴影值。

## 2. 莫兰迪四色（主题变量的实际取值）

改自 `frontend/src/style/theme.less`（TDesign token 覆盖）：

| 语义 | 名称 | 值 | 用途 |
|------|------|-----|------|
| 主色 | 深海蓝 | `#51697C` | 品牌主色、主按钮、链接、选中态 |
| 辅助底色 | 霜月白 | `#F4F2EE` | 页面底色、卡片底 |
| 中性色 | 雾蓝灰 | `#8C9BA5` | 次级文字、边框、禁用态 |
| 强调色 | 漂木棕 | `#B08968` | 警示/强调、图表辅助色 |

> 与上游原来那套 `#0580a2` 亮蓝相比：饱和度全面下调，长时间盯盘的疲劳感明显低。
> **ERP 新页面直接沿用**，不要按老文档再「降级」回原色（`docs/ERP-批次1-开发计划.md` D9 里
> 那条「莫兰迪在代码里不存在」的结论基于旧参考仓库，已作废）。

## 3. 品牌名替换点（逐处核对）

改名脚本按 `cailu→cloudpivot / Cailu→CloudPivot / 菜芦→云枢` 全仓替换，以下是**用户可见**的关键点：

| # | 位置 | 内容 | 改后 |
|--:|------|------|------|
| 1 | `frontend/index.html` | `<title>菜芦通用后台</title>`、`<link rel="icon">` | `云枢通用后台` |
| 2 | `frontend/src/layouts/BasicLayout.vue` | 侧栏顶部品牌区：logo `alt`、系统名、折叠后只显 icon | `云枢` |
| 3 | `frontend/src/views/login/index.vue` | 登录页大标题 + 副标题 + logo | `云枢(CloudPivot)` |
| 4 | `frontend/src/router/index.ts` | 路由 `meta.title`（面包屑/标签页/浏览器标题） | 继承新名 |
| 5 | `frontend/src/stores/setting.ts` | `SETTING_KEY = 'cloudpivot-setting'` | ⚠ 老用户偏好重置一次（见 special-cases 特例 6） |
| 6 | `frontend/package.json` + `package-lock.json` | `"name": "@cailu/web"` | `@cloudpivot/web` |
| 7 | `backend/Cargo.toml` + `Cargo.lock` | crate `name = "cloudpivot"` | release 产物 `target/release/cloudpivot` |
| 8 | `backend/src/main.rs` / `lib.rs` | `use cailu::...` → 与 crate 名同步 | 编译期强约束 |
| 9 | `backend/config/config.toml` | `app_name`、`[log] file_prefix`、`[database] name` | 日志文件 `cloudpivot.log`、库名 `cloudpivot` |
| 10 | `frontend/.env.development` / `.env.production` | `VITE_APP_TITLE` | 新名 |
| 11 | `frontend/deploy/nginx.conf` / `docker/frontend/nginx.conf` | `server_name`、access_log 路径 | 新名 |
| 12 | `docker/docker-compose.yml` / `.prod.yml` / `Dockerfile*` / `init-db.sql` | 容器名、镜像 tag、库名、用户 | 新名 |
| 13 | `README.md` / `AGENTS.md` / `llms.txt` / `skill/SKILL.md` / `docs/*.md` | 项目自述 | 新名 |
| 14 | `backend/src/shared/startup_guard.rs` | 报错文案里的 env 路径 `/etc/cailu/<env>.env` | `/etc/cloudpivot/<env>.env` |

> **系统层不在仓库里**（阶段 2 已完成，别在仓库里改）：`/etc/systemd/system/cloudpivot.service`、
> `/etc/cloudpivot/*.env`、`/var/log/cloudpivot*`、`/opt/git/cloudpivot.git`、
> nginx `/etc/nginx/conf.d/cloudpivot.conf`、PG 库 `cloudpivot` / `cloudpivot_dev` / `cloudpivot_test`。

## 4. 只换配色/品牌的「手工最小路径」

如果 Pro 的页面结构变化太大、`01-theme-morandi.patch` 打不上，用这条降级路径：

1. 把 `03-brand/theme.less` 整文件覆盖到 `frontend/src/style/theme.less`
   （只依赖 TDesign token 名，与页面结构无关，成功率最高）。
2. 手工把 `#0580a2` 系的硬编码色值替换成新四色：
   ```bash
   grep -rn "0580a2\|0x0580a2" frontend/src | head
   ```
3. `frontend/public/logo.png` / `favicon.png` 直接 cp 覆盖（与结构无关，零风险）。
4. 品牌文案按第 3 节表格逐个改（`grep -rn "菜芦\|cailu" frontend/src` 对照）。
5. 验证：`pnpm build` 通过 + 登录页与工作台肉眼确认水印/标题/主色。

## 5. 与 Pro 的品牌冲突处理

Pro 可能自带新的 logo 与配色。**我们的品牌是最终态**（老大已定云枢视觉），
Pro 的资源一律让位；但如果 Pro 把品牌色写进了**新的页面组件**（不经过 theme.less），
那些硬编码色值需要单独搜一遍替换：

```bash
grep -rnE "#[0-9a-fA-F]{6}" pro/frontend/src/views/<新模块> | grep -i "0580a2\|0052d9" 
```
