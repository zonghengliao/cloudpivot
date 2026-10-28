# 改名不能机械替换的地方（special-cases.md）

> `rename.sh` 做的是「整仓文本替换 + 文件名重命名」，但有 **7 处** 不能按字面改，
> 其中 4 处已内置处理逻辑，3 处必须人工判断。改名的坑都在这里，重放时按本文核对。

替换映射：`cailu → cloudpivot`、`Cailu → CloudPivot`、`菜芦 → 云枢`

---

## 已内置处理（脚本自动做，无需人工）

### 特例 1：`.gitleaks.toml` 规则 3 —— 必须**保留**旧前缀 `cailu`

历史事故里泄露的 JWT 字面量形态是 `cailu-<env>-jwt-secret-*`。扫描器规则如果跟着改名，
等于把**已知的历史泄露**从检测范围里藏掉（历史扫描会由红变绿，属于安全回归）。
密钥本身已经轮换过，这条规则的作用只是「拦住历史泄露被重新引入」，跟项目标识无关。

终态（当前仓库实际内容）：

```toml
# 本仓库历史事故的直接防线：dev/test/prod 的 JWT 密钥前缀特征（cloudpivot-<env>-jwt-secret-*）
# ⚠ 正则里**故意保留旧前缀 cailu**：git 历史里已泄露的字面量就是 cailu-<env>-jwt-secret-* 形态，
#   只写新前缀等于把已知历史泄露从扫描器里藏掉（历史扫描会由红变绿，属于安全回归）。
#   密钥本身已轮换，这条规则只用来拦住「历史泄露被重新引入」，跟项目标识无关。
[[rules]]
id = "cloudpivot-jwt-secret-literal"
description = "形如 {cloudpivot,cailu}-{dev,test,prod}-jwt-secret-... 的历史密钥字面量（含改名前的旧前缀）"
regex = '''(?:cloudpivot|cailu)-(dev|test|prod)-jwt-secret-[A-Za-z0-9_\-]+'''
```

脚本做法：先把这 4 类串（`(?:cloudpivot|cailu)`、`cailu-<env>-jwt-secret-`、`{cloudpivot,cailu}`、
`旧前缀 cailu`）打成令牌保护起来 → 全局替换 → 还原 → 再按锚点补齐注释/描述/正则。
**幂等**：脚本发现正则已是 `(?:cloudpivot|cailu)-` 就整体跳过，二次执行 0 改动。

校验：`grep -n cailu .gitleaks.toml` 应命中 3 行（1 行说明注释 + description + regex），
且 `grep -q '(?:cloudpivot|cailu)-' .gitleaks.toml` 为真。

> 注意 `.gitleaks.toml` 里还有 `id = "cloudpivot-jwt-secret-literal"` 和描述里的正文，
> 这些**是要改的**（它们只是规则命名，不是检测目标）—— 所以不能「整文件跳过 cailu」。

### 特例 2：`.githooks/post-commit` 的本机 bare 白名单

原写法 `case "$ORIGIN_URL" in /opt/git/cailu.git*)` 把 bare 目录名写死。
改名窗口内一旦 bare 目录先改名、钩子后更新，post-commit 就静默失效（26 个提交靠它同步 bare，
失效了没人发现）。脚本改成通配：

```bash
  /opt/git/*.git) ;;
```

既排除 GitHub 远端（只推本机 bare），也不再依赖目录名 —— 以后再改名不用动钩子。

### 特例 3：`scripts/patch-alun-web.sh` 整文件覆盖

这个脚本对 `alun-web` crate 源码做**幂等文本补丁**，判重原本依赖标记字面 `[cailu-patch code-07]`。
改名把标记前缀换成 `[cloudpivot-patch ...]` 后，判重会把「已打过」误判成「没打过」，
进而**重复插入** `ignore_paths` 字段、二次替换匹配逻辑，编译直接坏掉（真实踩过，`f3fc278` 修的）。

因此脚本对 `scripts/patch-alun-web.sh` 走「整文件覆盖」：
`01-rename/overrides/patch-alun-web.sh` = 改名后 + 判重改为「按补丁代码签名」的终态版本
（判据：锚点已消失 且 补丁代码签名 `let mut best: Option<(&PermissionRule, u8)>` 已在位 → 认定已打过）。
该文件 100% 是本项目自研脚本（不是上游文件），整文件覆盖比逐行 sed 安全。

适用条件：目标树里**已经**有该脚本（即 02 补丁序列已应用）。没有就跳过并在输出里标注。

### 特例 4：文件名/目录名重命名

脚本自底向上重命名任何名字里含 `cailu/Cailu/菜芦` 的文件或目录
（如 `cailu.service` → `cloudpivot.service`、`config-cailu.toml` → `config-cloudpivot.toml`）。
本仓库基线里恰好没有这类文件名（systemd unit / env 都在 `/etc/` 下，不入库），
所以这一步在本次重放里是 0 次 —— **但 Pro 基线可能会有**，脚本必须能处理。

---

## 需要人工判断（脚本有意不动）

### 特例 5：外部上游仓库 URL `github.com/wild1024/cailu`

这是**别人的仓库地址**，改了就是指向不存在的地址。脚本把
`github.com/wild1024/cailu` 与 `wild1024/cailu` 列为保护串，永远不改。

历史变化：阶段 3 已经把 upstream remote 和仓库内所有该 URL 记录清理干净
（现在 `grep -rn "wild1024" .` 在仓库里应为空），所以本次重放不会命中；
但如果 **Pro 包/补丁说明/README 里带了这个 URL，必须原样保留**。

### 特例 6：`cailu-src` —— 另一台机器上的参考仓目录名

文档里引用的 `/root/.hermes/workspace/information-security/cailu-src` 是一个**独立的参考工程**
（同技术栈的旧版源码，用来做二开核查），不是本仓库，不在本次改名范围内。
脚本把 `cailu-src` 列为保护串。别问「为什么改名了还留着 cailu」—— 那是别人的目录名。

### 特例 7：`docs/ERP-批次1-*.md` —— 历史文档，整文件跳过

这三篇 ERP 设计文档是对**旧基线**的描述性记录（"crate `cailu` v0.1.0"、"基线 `/root/work/c144`"、
"项目库 `cloudpivot_test` / `cailu`"）。把里面的 `cailu` 改掉等于**篡改历史记录**，
而且会让「当时到底基于什么」无法追溯。脚本按 `SKIP_FILE_GLOBS` 整文件跳过。

因此 `01-rename/verify.sh` 的残留白名单是**两类**：
`.gitleaks.toml`（1~3 行）+ `docs/ERP-批次1-*.md`（历史文档）。其它任何命中都算失败。

---

## 还有几个隐性的「标识耦合」点（改名前逐个人工确认）

| 位置 | 内容 | 改名影响 | 处理 |
|------|------|----------|------|
| `frontend/src/stores/setting.ts` | `SETTING_KEY = 'cloudpivot-setting'` | **老用户 localStorage 键变了**：主题/侧栏折叠等偏好会「回到默认」（不报错、不丢数据，只是重置一次） | 已在改名提交里改；若有真实用户，可加一次性迁移：读 `cailu-setting`→写新键→删旧键 |
| `backend/config/config.toml` | `app_name` / `file_prefix` / `database.name` | 日志文件名前缀、库名跟着变 | 与系统层改名（`/etc/cloudpivot/`、DB `cloudpivot`）必须**同步**，否则连不上库 |
| `backend/config/config-*.toml` | 库名 / env 路径注释 | 同上 | 系统层（阶段2）已按新名建库建 env，代码里不能留旧名 |
| `docker/docker-compose*.yml`、`docker/init/init-db.sql` | 容器名 / 库名 / 用户 | `docker compose up` 会新建一套名字不同的容器与卷 | 想复用旧数据，得改 compose 名字或导数据 |
| `frontend/deploy/nginx.conf`、`docker/frontend/nginx.conf` | 站点名 / 日志路径 / upstream | 生产 nginx 是系统层文件（`/etc/nginx/conf.d/cloudpivot.conf`），仓库里这份要和它对齐 | 阶段2 已对齐 |
| `.githooks/post-commit`、`scripts/*.sh` | `/opt/git/*.git`、`/var/log/cloudpivot*`、`/etc/cloudpivot/*.env` | 指向系统层真实路径 | 阶段2 已建；重放后按 `05-conflicts` 清单核对 |
| `backend/Cargo.toml` + `Cargo.lock` | crate 名 `cloudpivot` | release 产物变成 `target/release/cloudpivot`；**systemd unit / 部署脚本里的二进制名必须同步** | 阶段2 的 `cloudpivot.service` 已按新名；重放后编译一次确认产物名 |
| `frontend/package.json` + `package-lock.json` | `@cloudpivot/web` | 锁文件里的包名要一起改，否则 `pnpm install` 报锁定不一致 | 改名提交里已改 `package-lock.json`；用 pnpm 的话要 `pnpm install --lockfile-only` 重生成 |
| `llms.txt` / `skill/SKILL.md` / `AGENTS.md` | 项目自述 | 只是文案 | 直接替换 |

## 重放后的自检清单

```bash
bash 01-rename/rename.sh --dry-run     # 应输出「内容改动文件: 0 替换次数: 0 重命名: 0」
bash 01-rename/verify.sh               # 残留只允许 .gitleaks.toml + docs/ERP-*，失败项应为 0
grep -n cailu .gitleaks.toml           # 必须仍有 3 行（旧前缀检测器）
grep -rn "wild1024" . --include=*.md   # 外部 URL 若出现，必须原样保留
```
