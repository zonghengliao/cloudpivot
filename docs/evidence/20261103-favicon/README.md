# FAVICON 图标集补全 —— 修 Safari 标签页老图标

> 卡：`t_b55bfbc9`｜提交：`e89c8dd fix(brand): FAVICON 图标集补全，修 Safari 标签页老图标`（本次再追加本目录证据）
> 环境：开发机 99 `172.18.10.99`（/opt/cloudpivot）→ 生产 144 `172.18.10.144`（nginx root = `/opt/cloudpivot/frontend/dist`）

## 1. 根因（三条，缺一条修不好）

| # | 现象 | 后果 |
|---|---|---|
| ① | `index.html` 只有 `<link rel="icon" href="/favicon.png" />` | Chrome 认，Safari 仍会**无条件先请求 `/favicon.ico`** |
| ② | 仓库里**没有** `favicon.ico` | Safari 那个请求必然落空 |
| ③ | nginx `location / { try_files $uri $uri/ /index.html; }` | 落空的 `/favicon.ico` 被回退成 `index.html`：`200` + `Content-Type: text/html` + `602` 字节 → Safari 判定无效，改用自身 **Favicon Cache**（清浏览器缓存清不掉） |

补充：`apple-touch-icon` / 多尺寸 png 全缺，加书签、添加到主屏也拿不到图标。

## 2. 做了什么

1. **生成脚本** `scripts/assets/gen_favicons.py`（Pillow，幂等，`--check` 只校验）
   - 源 `frontend/public/logo.png`（512×512 RGBA）；非正方形直接拒绝（防拉伸）
   - 每尺寸独立 `LANCZOS` 缩放后打包 ICO（不是让 Pillow 从大图内部缩，16px 才不糊）
   - 不引入任何 npm 依赖
2. **图标文件**（`frontend/public/` + build 后进 `frontend/dist/`）：

   | 文件 | 尺寸 | 模式 | 字节 | md5 |
   |---|---|---|---|---|
   | `favicon.ico` | 48×48 首帧，内含 **16/32/48** 三帧 | RGBA | 5627 | `28f6d2cd2908884e899e975bae0f2d88` |
   | `favicon-32x32.png` | 32×32 | RGBA | 1762 | `ba9ce23c242c0fd2675244a23b0566ec` |
   | `favicon-16x16.png` | 16×16 | RGBA | 724 | `e95477d6a7274657104405fe5b8b1aad` |
   | `apple-touch-icon.png` | 180×180 | RGBA | 14729 | `4ca892960ff506f044b80a3470a18dc2` |

   `python3 scripts/assets/gen_favicons.py` 跑两遍 md5 完全一致（确定性输出），`--check` PASS。
3. **`frontend/index.html`**：旧的单条 `favicon.png` 声明删掉，换成四件套（全部 `?v=2` 破缓存），
   没有 `mask-icon`（见 §5）。
4. **nginx**（144 `/etc/nginx/sites-available/cloudpivot.conf`，同步改仓库基线 `frontend/deploy/nginx.conf`）：

   ```nginx
   location ~* ^/(favicon|apple-touch-icon|mstile|android-chrome|safari-pinned-tab)[^/]*\.(ico|png|svg|webp)$ {
       add_header Cache-Control "public, max-age=604800";
       try_files $uri =404;
   }
   ```

   ⚠ 卡面给的 `^/(favicon\.ico|favicon-\d+x\d+\.png|...)$` **过不了卡面自己的验收**：
   `/favicon-does-not-exist.png` 不匹配 `favicon-\d+x\d+`，仍然回落到 `index.html`（实测 200 text/html）。
   故改成上面这条更宽的前缀式匹配 —— 所有「图标类」路径一律 `=404`，不再有 HTML 冒充图片的可能。

## 3. 线上响应（`curl-headers.txt` 同目录，逐条实录）

```
$ curl -sI http://172.18.10.144/favicon.ico
HTTP/1.1 200 OK
Content-Type: image/x-icon        ← 修复前是 text/html
Content-Length: 5627              ← 修复前是 602（index.html 的大小）
ETag: "6ae9f246-15fb"             ← 与 index.html 的 "6ae9f246-587" 不同
Cache-Control: public, max-age=604800

$ curl -sI http://172.18.10.144/favicon-32x32.png   → 200 image/png  1762
$ curl -sI http://172.18.10.144/favicon-16x16.png   → 200 image/png  724
$ curl -sI http://172.18.10.144/apple-touch-icon.png→ 200 image/png  14729
$ curl -sI http://172.18.10.144/favicon-does-not-exist.png → 404 Not Found（不再回落 index.html）
$ curl -sI http://172.18.10.144/index.html          → 200 text/html 1415
```

哈希三方一致：`curl | md5sum` == 144 的 dist == 本地 build 的 dist（ICO `28f6d2cd…`、apple-touch `4ca89296…`）。

## 4. 真机证据（本目录）

| 文件 | 是什么 | 结论 |
|---|---|---|
| `01-chromium-prod.png` | Chromium 打开 `http://172.18.10.144/` | 页面正常，图标声明全部生效 |
| `02-chromium-prod-incognito.png` | 全新 context（无痕等价，零缓存） | 同上，排除缓存干扰 |
| `03-webkit-prod.png` | **WebKit 引擎**（Safari 同源内核）打开线上 | HTML/图标声明解析正常 |
| `04-icon-set-rendered.png` | 四个图标按真实像素尺寸渲染（棋盘格显示 alpha） | 字节可解码，是同一朵云涡 |
| `05-chromium-tabstrip.png` | Xvfb 里**真实 Chromium 窗口**整屏（标签栏可见） | 标签页图标 = 新图标 |
| `06-chromium-history.png` | `chrome://history` 历史记录列表 | 两条记录的图标 = 新图标 |
| `08-chromium-bookmarks.png` | `chrome://bookmarks` 书签管理页 | 书签行图标 = 新图标 |
| `curl-headers.txt` | 上面 §3 的原始输出 | — |
| `favicon-network.txt` / `favicon-links.txt` | 浏览器网络层实录 / 页面解析出的 link 列表 | — |

> 编号跳过 07：原来还截了一张「新标签页书签栏」，但那一格的图标是 Chromium 自带的占位图形
> （圆形掠影，与我们的图标像素差 44/255），留着会误导，已删。书签证据改用 `chrome://bookmarks` 的列表页（08）。

### 4.1 「标签页图标是不是新的」怎么证的（没有视觉配额，用像素说话）

`scripts/verify/favicon_shot_probe.py` / `favicon_align.py` 在截图里找品牌色 `#51697C` 连通域，
对「尺寸 + 偏移」做全搜索后与 `favicon-16x16.png` 逐像素比：

```
05-chromium-tabstrip.png  最匹配 size=16 box=(48,12)-(64,28)  平均像素差 7.56/255  → PASS
06-chromium-history.png   最匹配 size=16 box=(441,558)-(457,574) 平均像素差 7.56/255 → PASS
08-chromium-bookmarks.png 最匹配 size=16 box=(310,243)-(326,259) 平均像素差 7.56/255 → PASS
```

三处抠出来的 16×16 字符图与 `favicon-16x16.png` 的字符图逐行一致（见卡面贴的 ASCII）。
差异 7.56/255 全部来自 ClearType/亚像素抗锯齿。

## 5. 为什么没做 `favicon.svg`

`mask-icon`（Safari 固定标签页）要求**单色矢量 path**。本项目 logo 是双色（`#51697C` + 浅色云涡）
的栅格图，直接「内嵌 base64 PNG 的 SVG」不是矢量、`mask-icon` 也不认；自动描摹（potrace 等）
会把云涡的边缘改成另一种形状 —— 卡面明确要求「不重新设计图标」，故跳过。
现代浏览器已由 `favicon.ico`（多尺寸）+ 两个 PNG 覆盖，不缺这一个。

## 6. 部署方式与后续注意

- 图标是 `public/` 静态资源：**不需要重编后端**。本次线上做法 = 新图标进 `frontend/dist/` +
  同步 `frontend/public/`，`dist/index.html` 补图标声明（`dist/` 是 `.gitignore` 的产物，不脏仓库）。
- 144 的 git HEAD 仍是 `be1cfee`（本次提交 `e89c8dd` **未** 整仓部署，后端二进制/前端整包不动），
  `frontend/public/` 里新增的 4 个图标是未跟踪文件且与提交逐字节相同 —— 下次 `deploy.sh`
  的 `--ff-only` 能正常合并（脚本会把逐字节相同的未跟踪副本挪开）。
- 生产是 `expires 7d`：图标换了图就必须带新 `?v=`，否则一周内客户端还在用旧文件。
- 改了 logo → `python3 scripts/assets/gen_favicons.py && pnpm build`，`--check` 有一项不过就退出码 1。

## 7. 已知限制（诚实说明）

- **没有真 Safari**：本机是 Linux，Safari 只在 macOS。已做的是 (a) 协议层按 Safari 的挑剔口径验证
  （`/favicon.ico` 必须 200 + `image/x-icon` + 真 ICO），(b) 用 **WebKit 内核**（Playwright WebKit）
  打开线上确认解析无碍。Playwright 的 headless WebKit 不实现 Safari UI 层的 favicon 抓取，
  所以「Safari 标签页看起来对不对」这一步需要在 macOS 上人工确认。
- **老用户可能要手动清一次**：Safari 的 Favicon Cache 独立于浏览器缓存
  （`~/Library/Containers/com.apple.Safari/.../Favicons`），历史上已经缓存了「无效图标」的会话，
  最省事的办法是完全退出 Safari 再开，或删该目录后重开。
- 自动化截图用的是 Xvfb + headful Chromium（headless 没有标签栏）。本机 `/tmp` 是 7.6G tmpfs
  且常被并行卡塞满，Xvfb 必须加 `-nolock`、Chrome 的 `user-data-dir`/`TMPDIR` 放 `/root` 下。

## 8. 复跑命令

```bash
# 生成 + 自校验（幂等）
python3 scripts/assets/gen_favicons.py
python3 scripts/assets/gen_favicons.py --check

# 前端构建（产物里应出现 4 个图标）
cd frontend && pnpm build && ls -la dist/favicon.ico dist/apple-touch-icon.png

# 线上响应
bash -c 'for p in /favicon.ico /favicon-32x32.png /apple-touch-icon.png /favicon-does-not-exist.png; do
  echo "## \$p"; curl -sI http://172.18.10.144\$p | head -6; done'

# 浏览器侧证据（Chromium + WebKit + 图标渲染）
cd frontend && node ../scripts/verify/favicon_evidence.cjs

# 真实窗口（标签栏 / 历史 / 书签）截图 + 像素比对
node ../scripts/verify/favicon_window_probe.cjs
python3 ../scripts/verify/favicon_align.py ../docs/evidence/20261103-favicon/05-chromium-tabstrip.png 56,20
```
