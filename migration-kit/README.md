# 云枢(CloudPivot) Pro 版迁移准备包

> **一句话**：把我们对着上游基线 `c4e8950` 做的全部改动（改名 52 文件 / 安全整改 9 批 / 莫兰迪配色 / 品牌 / 服务器状态卡）
> 整理成「脚本 + 补丁 + 文档」，Pro 版到手后照着重放一遍，不用 `git merge`（那会撞 247 处改名冲突）。
>
> **已实测**：在干净基线上完整重放 **48.6 秒**跑通，三道 git 逐字节闸门全绿，
> `cargo test` 45 passed / 0 failed，`pnpm typecheck` 零错误，改名残留只剩 `.gitleaks.toml` 3 行。
> 详见文末「重放演练结果」。

---

## 0. 这个包解决什么问题

| | 直接 merge | 用本包重放 |
|---|---|---|
| 改名冲突 | 247 处（52 文件被改名 + Pro 又改了同样的文件） | 0（改名由脚本在新基线上重做） |
| 安全补丁 | 补丁上下文全被改名打断，几乎全冲突 | 补丁序列按「改名前的上下文」生成，打得上 |
| 可验证性 | 靠肉眼 | 三道闸门：打完补丁 == `d5a2b61`、改完名 == `df9f658`、配色品牌完 == `46bbe9a`（逐字节） |

---

## 1. 包内容

```
migration-kit/
├── README.md                     ← 本文（两套操作手册）
├── 00-baseline.md                上游基线信息 c4e8950、22 个改动提交、怎么重拉上游、怎么和 Pro 对比
├── prepare-config.sh             ★ 环境准备：从 *.example.toml 生成未入库的 config-{dev,test}.toml
├── replay.sh                     ★ 演练器：在临时 worktree 上把下面 6 步全跑一遍 + 闸门校验 + 构建测试
├── 01-rename/
│   ├── rename.sh                 改名脚本（幂等 / --dry-run / 自动备份 / 7 个特例）
│   ├── special-cases.md          不能机械替换的 7 处（gitleaks 旧前缀、localStorage 键、外部 URL…）
│   ├── verify.sh                 残留扫描 + 关键文件抽查 + 幂等自检
│   ├── overrides/                两个必须整文件覆盖的文件（patch-alun-web.sh、gitleaks.toml 兜底）
│   ├── post-rename-notes.patch   改名提交里的人工补充（文档横幅 + index.less 注释 4 行）
│   └── post-rename-patch-alun-web-dedupe.patch   改名副作用修正（判重改为代码签名）
├── 02-security/
│   ├── patches/                  15 条补丁（0001~0015，c4e8950..d5a2b61 完整序列，9 条是安全整改）
│   ├── apply.sh                  逐条 git am（--check 预演 / --abort 回滚 / 失败给人工指引）
│   └── design-notes.md           每条补丁：改了什么、为什么、apply 失败怎么手工重做
├── 03-brand/
│   ├── apply.sh                  打配色 + 品牌补丁（幂等，reverse-check 判已应用）
│   ├── 01-theme-morandi.patch    莫兰迪配色（6 文件：theme.less/index.less/BasicLayout/login/dashboard/改密页）
│   ├── 02-brand-assets.patch     新 logo / favicon（二进制补丁）
│   ├── theme.less                配色变量（独立文件，便于只换色）
│   ├── logo-512.png / favicon-64.png
│   └── brand-points.md           品牌替换点清单（14 处）+ 莫兰迪四色取值 + 手工降级路径
├── 04-modules/
│   ├── business-monitor/         服务器状态卡整模块（后端四件套 + 前端 API + V0020 迁移 + wiring.md + copy.sh）
│   └── migrations.md             迁移号段说明 + 撞号重编号步骤 + up→down→up 验证脚本
├── 05-conflicts/
│   ├── changed-upstream-files.md 我们动过的 102 个上游文件清单（含改动性质/涉及提交）
│   └── gen-changed-files.sh      生成器（Pro 到手后对新基线再生成一份）
└── 06-verify/
    ├── verify.sh                 验收执行器：安全断言 + 功能断言 + 改名残留 + cargo test + typecheck
    └── acceptance.md             验收清单（A 静态 26 条 / B 运行时 6 条 / C 功能 / D 改名 / E 构建）
```

**用哪个入口**：
- 只想确认「这套东西现在还能跑」→ `bash migration-kit/replay.sh`（49 秒，自带清理）
- Pro 到手后真的要落地 → 按下面「场景 B」手把手走（或直接 `replay.sh --baseline <pro> --expect-* none`）

---

## 2. 场景 A：Pro 到手后 **30 分钟评估**

目的：**先决定走哪条路，别急着写代码**。两条路的工作量差 5 倍。

### A1. 拉代码（2 分钟）

```bash
cd /opt/cloudpivot                      # 我们的主仓（26c9040）
git remote add pro /path/to/pro-repo    # 或 pro 是个 tarball 就先 git init 提交一次
git fetch pro --tags
git log --oneline -5 pro/main
```

### A2. 回答三个问题（10 分钟，全部 git 能答）

```bash
# Q1 同源吗？Pro 是不是从我们认识的基线长出来的
git merge-base HEAD pro/main            # 输出 c4e8950（或其祖先）→ 同源；输出别的/空 → 不同源
git log --oneline c4e8950..pro/main | wc -l    # Pro 在基线上新增了多少提交

# Q2 耦合度：Pro 动了多少「我们也动过」的文件
git diff --name-only c4e8950..pro/main | sort > /tmp/pro.txt
git diff --name-only c4e8950..46bbe9a | sort > /tmp/ours.txt
comm -12 /tmp/pro.txt /tmp/ours.txt | tee /tmp/both.txt | wc -l
comm -12 /tmp/pro.txt /tmp/ours.txt | head -40

# Q3 底座文件动没动（这 8 个文件是我们安全整改的落点）
for f in backend/src/lib.rs backend/src/bootstrap.rs backend/src/shared/constants.rs \
         backend/src/middleware/mod.rs backend/src/system/auth/handler.rs \
         backend/src/system/user/handler.rs frontend/src/router/index.ts frontend/src/stores/user.ts; do
  printf "%-46s %s\n" "$f" "$(git diff --numstat c4e8950..pro/main -- "$f" | awk '{print "+"$1" -"$2}' )"
done
```

### A3. 判断清单（勾完就知道走哪条路）

| # | 判据 | 阈值 | 走这条 |
|--:|------|------|--------|
| 1 | 交集文件数 `comm -12` | **≤ 10** 且不含 `auth/user/middleware/lib.rs/constants.rs` | → **功能移植**（只把 Pro 的新模块搬过来） |
| 2 | 交集文件数 | **≥ 25**，或命中上面 8 个底座文件里 ≥3 个 | → **重置基线**（用本包重放，Pro 当新基线） |
| 3 | Pro 改了 `backend/Cargo.toml` 的 alun 版本 | 框架小版本+ 或 大版本变 | → **重置基线**（`patch-alun-web.sh` 要按新版本重打） |
| 4 | Pro 改了迁移号段（`migrations/*/V00xx` 与我们撞号） | 撞号 | 重置基线 + 按 `04-modules/migrations.md` 重编号 |
| 5 | Pro 只在 `business/` 下新增独立模块（如审批流），`system/support` 没动 | 是 | → **功能移植**（把新模块整体搬进来，跑 `06-verify`） |
| 6 | Pro 换了前端框架/工程结构（vite→其它、路由结构变） | 是 | → **重置基线**，且 `03-brand` 大概率要人工重做 |

> **经验值**：Pro 要是「加审批流 + 加对象存储」，第 5 条通常成立 → 功能移植，工作量 1~2 人日。
> Pro 要是「重构 auth / 换 DB 层 / 改权限模型」（Pro 名里有 pro，很可能重写 auth 和 workflow 底座），
> 就是重置基线，工作量按下面「场景 B」估。

### A4. 顺手生成新基线的冲突清单（1 分钟）

```bash
bash migration-kit/05-conflicts/gen-changed-files.sh --head pro/main --out /tmp/pro-conflicts.md
# 这份表就是「Pro 动了哪些我们也动过的文件」，逐条核对用
```

---

## 3. 场景 B：半天落地（**重置基线**路径）

前置：工作区干净、`git worktree` 可用、能编译（`cargo` / `pnpm`）、PG 可连（跑迁移需要）。
整条流水线可以一条命令跑完（`replay.sh`），也可以按下面 6 步手走 —— **每一步都有验证点和失败回退**。

### 步骤 1：拿 Pro 做新基线和分支（10 分钟）

```bash
cd /opt/cloudpivot
git worktree add --detach /var/tmp/pro-base pro/main    # 别用 /tmp，见文末坑 1
cd /var/tmp/pro-base
git switch -c pro-migrate                                # 在 worktree 里开分支干活
git log --oneline -1                                     # 记下 Pro 的基线 commit
```

- **验证点**：`git status` 干净；`ls backend/migrations/*/V*.sql | tail` 看清 Pro 的号段
- **失败回退**：`git worktree remove --force /var/tmp/pro-base`，回到主仓，什么都没脏

### 步骤 2：打补丁序列 → 云枢的安全与功能改动（5 分钟）

**顺序很重要：补丁先打，改名最后做。** 补丁是按「改名前的上下文」生成的，
先改名再打补丁必然冲突（这也是最初 247 处冲突的根因）。

```bash
bash /opt/cloudpivot/migration-kit/02-security/apply.sh --check   # 先预演
bash /opt/cloudpivot/migration-kit/02-security/apply.sh           # 逐条 am
```

- **验证点**：最后一行 `应用完成：xxx → yyy（15 个提交）`；`git status` 干净
  （在**本仓库**演练时还应 `git diff --stat d5a2b61` 为空）
- **失败回退**：apply.sh 会自动停住并打印冲突文件 + 人工指引。
  - 只想撤：`git am --abort`（回到起点，一个提交都不留）
  - 想挑着改：`git apply --reject <patch>` 后逐个 `.rej` 手工合
  - 哪条补丁在新基线上没意义（Pro 已经做了同样的整改）→ 跳过它，用 `06-verify` 的断言判定「目标是否达成」
- **常见失败**：`0015`（lint 清理）最容易冲突且**没有安全/功能价值**，冲突了直接跳过

### 步骤 3：改名（1 分钟）

```bash
KIT=/opt/cloudpivot/migration-kit
bash $KIT/01-rename/rename.sh --dry-run   # 先看要改什么
bash $KIT/01-rename/rename.sh             # 真跑（自动备份到 .rename-backup/<ts>）
git apply $KIT/01-rename/post-rename-notes.patch                     # 改名提交里的人工补充（4 行说明 + 1 行注释）
git apply $KIT/01-rename/post-rename-patch-alun-web-dedupe.patch     # 判重改为「补丁代码签名」
```

- **验证点**：`bash 01-rename/verify.sh` → 失败 0 项；`grep -rn "cailu" . | grep -v migration-kit` 只剩
  `.gitleaks.toml`（3 行，有意保留）；再跑一次 `rename.sh --dry-run` 应输出「0 改动」（幂等）
- **失败回退**：`rm -r .rename-backup/<ts>` 之前，用备份目录里的原文件覆盖回来即可
  （`cp -a .rename-backup/<ts>/. .`）。或者 `git checkout -- .`（改名不涉及未跟踪文件时够用）
- **注意**：`.gitleaks.toml` 的旧前缀 `cailu` **必须保留**（历史泄露检测器），
  `localStorage` 键改名会让老用户偏好重置一次（不报错，只是回到默认）—— 见 `01-rename/special-cases.md`

### 步骤 4：配色 + 品牌 + 状态卡模块（10 分钟）

```bash
bash /opt/cloudpivot/migration-kit/03-brand/apply.sh          # 莫兰迪配色 + logo/favicon
bash /opt/cloudpivot/migration-kit/04-modules/business-monitor/copy.sh   # 模块文件 + 4 处接线自检
```

- **验证点**：`03-brand/apply.sh` 输出两个 ✓；`copy.sh` 输出「模块文件: 一致 N, 新增 0」且 4 个接线点全 ✓；
  `file frontend/public/logo.png` = 512×512
- **失败回退**：
  - 配色补丁打不上（Pro 改了页面结构）→ 走 `brand-points.md` 第 4 节「手工最小路径」：
    整文件覆盖 `theme.less` + cp 图片 + 按 14 处品牌点手工改文案。**配色丢了不影响安全与功能**，可最后做
  - 模块接线缺失 → 按 `04-modules/business-monitor/wiring.md` 手工加 4 行

### 步骤 5：处理迁移撞号（20 分钟，含演练）

```bash
ls backend/migrations/business/ backend/migrations/system/ | grep V00 | sort -t V -k2 -n | tail
```

- 撞号了 → 按 `04-modules/migrations.md` 第 2 节重编号（`git mv` + 改版本表 + 核对 `sys_menu` 种子）
- **验证点**：`04-modules/migrations.md` 第 3 节的 up→down→up 在**临时库** `cloudpivot_mig_check` 上跑通；
  `psql -c "select count(*) from sys_menu where auth_code='business:monitor:server-status'"` = 1
- **失败回退**：全程用临时库，删库即可；**绝不碰** `cloudpivot` / `cloudpivot_dev` / `cloudpivot_test`

### 步骤 6：验收（5 分钟 + 构建时间）

```bash
bash /opt/cloudpivot/migration-kit/prepare-config.sh      # 未入库配置从 *.example 生成（少这步 cargo test 必挂一条）
bash /opt/cloudpivot/migration-kit/06-verify/verify.sh    # 安全 26 条 + 功能 6 条 + 改名 4 条 + cargo test + typecheck
```

- **验证点**：`验收结果: 通过 40, 失败 0, 跳过 0`；再做 `06-verify/acceptance.md` 的 **B 段运行时 6 条**
  （伪造令牌 401 / 首登 403 / 复用检测 / 提权 403 / 缓存挂掉 fail-closed）
- **失败回退**：F/E 段失败是编译或断言问题，改代码；B 段失败是**运行时代码没生效**，
  优先怀疑：启动守卫没接进 `lib.rs`、`patch-alun-web.sh` 没重跑（`cargo update` 后必须重跑）

### 全过程一条命令（推荐先跑它，再决定手工做哪步）

```bash
bash /opt/cloudpivot/migration-kit/replay.sh \
  --baseline <Pro 的基线 commit> --dir /var/tmp/pro-replay \
  --expect-patches none --expect-rename none --expect-final none
```

`--expect-* none` = 不做「和我们历史 commit 逐字节比对」的闸门（Pro 基线不同，没有可比对象），
只校验流程能跑通 + `06-verify` 全绿。

---

## 4. 重放演练结果（2026-10-28，本机实测）

**环境**：172.18.10.144 / Ubuntu 26.04 arm64 / 4 核 / `git worktree` 干净基线 `c4e8950`。

| 步骤 | 耗时 | 结果 |
|------|------|------|
| ① 应用 02-security 补丁序列（15 条） | 309 ms | ✅ 15/15 一次成功，闸门 `== d5a2b61` 逐字节一致 |
| ② 执行改名（52 文件 / 240 处） | 39 ms | ✅ 0 冲突 |
| ②b 改名人工补丁 | 3 ms | ✅ |
| ③ patch-alun-web 判重修正 | 1 ms | ✅ |
| ④ 配色 + 品牌 | 42 ms | ✅ 闸门 `== 46bbe9a` 逐字节一致 |
| ⑤ 状态卡模块落地 | 14 ms | ✅ 8 文件一致，4 接线点全绿 |
| ⑤b 生成未入库配置 | 15 ms | ✅ |
| ⑤c `pnpm install`（首次） | 1.7 s | ✅ 走 npmmirror |
| ⑥ 冲突清单生成（102 文件） | 44 ms | ✅ |
| ⑦ 改名残留校验 | 159 ms | ✅ 只剩 `.gitleaks.toml` 3 行 |
| ⑧ 全量验收（含构建） | 45.7 s | ✅ **通过 40 / 失败 0**（`cargo test` 45 passed 0 failed 40s；`typecheck` 6s） |
| **合计** | **48.6 s** | **闸门失败 0，步骤失败 0** |

**需要人工介入的地方（这是最值钱的信息，说明准备包还不完美）**：

1. **`cargo test` 有一条用例依赖「未入库的配置文件」** ——
   `shared/startup_guard.rs::merge_semantics_or_base_and_profile` 读磁盘上真实的
   `config-dev.toml` / `config-test.toml`，而这两个文件被 `.gitignore` 掉了。
   干净 checkout 上实测 **44 passed / 1 failed**；补上 `prepare-config.sh`（从 example 生成）后 45 passed。
   → 准备包已内置这一步，但**这同时暴露一个真实缺陷**：这条测试在纯 CI（不带本地配置）上会红，
   建议后续改成读内嵌文本。
2. **`Cargo.lock` 的包块位置** —— 纯文本改名会让 `cloudpivot` 的 package 块停在原 `cailu` 的位置，
   与 cargo 的字母序不一致（语义等价，但下次 `cargo build` 会重写 lock）。
   准备包已内置「按字母序搬块」，否则闸门会失败。
3. **改名提交里有两处人工改动无法用替换推出**（文档横幅 4 行 + `index.less` 一句注释），
   已固化为 `post-rename-notes.patch`；Pro 基线里这两个文件不同时该 patch 打不上，脚本会警告而不中断。
4. **`/tmp` 可能装不下 `backend/target`**（本机 `/tmp` 是 3.7G tmpfs，实测 `No space left on device`）。
   `replay.sh` 默认改到 `/var/tmp`；手工做时也别把 worktree 放 `/tmp`。

---

## 5. Pro 到手后预计工作量

| 路径 | 场景 | 工作量 | 说明 |
|------|------|--------|------|
| 功能移植 | Pro 只加独立模块（审批流等），底座没动 | **1~2 人日** | 搬模块 + 接线 + 权限码 + 跑 `06-verify` |
| 重置基线 | 本包重放一次成型（Pro 底座兼容） | **0.5 人日**（4 小时） | 本机实测流水线 49 秒，剩下是评估、迁移撞号、运行时验收、部署 |
| 重置基线 | Pro 重构了 auth / 权限 / 框架版本 | **2~4 人日** | 9 条安全补丁里 `0006/0012/0014`（启动守卫 / 密钥治理 / CODE-16B）要按新结构重做，这是主要成本 |
| 兜底 | Pro 换框架或换 ORM | **5 人日+** | 相当于重做安全整改，建议直接在新栈上按 `design-notes.md` 的「为什么」重建 |

**关键结论**：重放本身很快（分钟级），**真正的时间花在「Pro 到底动了什么」的判断和冲突裁决上**。
所以场景 A 的 30 分钟评估不要省。

---

## 6. 已知坑（踩过的，别再踩）

1. **`/tmp` 空间**：`backend/target` 2~3G，`/tmp` 常是 tmpfs。worktree 放 `/var/tmp`（或任何真实磁盘）。
2. **`git am` 需要 committer identity**：全新机器/容器里 `git config user.email` 为空 → `am` 直接失败。
   `02-security/apply.sh` 已兜底（用占位身份，Author 仍取补丁原作者）。
3. **hooks 会污染 bare 仓库**：仓库 `core.hooksPath=.githooks`，`post-commit` 会往
   `/opt/git/cloudpivot.git` 推。`apply.sh` 已强制关掉 hooks（`-c core.hooksPath=<空目录>`）。
   手工 `git am` 时请自己加 `-c core.hooksPath=/tmp/.no-hooks`。
4. **`patch-alun-web.sh` 在 `cargo update` / `cargo clean` 后必须重跑**，否则框架层的
   CODE-07/CODE-09 补丁丢失（表现为 `/support/config/public` 被 403、认证 fail-open 回归）。
   `bash scripts/patch-alun-web.sh --check` 应输出 `PATCH-STATE: OK`。
5. **改名不要碰 `.gitleaks.toml` 的旧前缀**（否则历史泄露检测失效），也不要碰外部 URL。
6. **验收的 B 段（运行时）不能省**：静态断言只能证明「代码写了」，证明不了「真的挡住了」。

---

## 7. 红线

- 本包**只读**现有生产代码；演练一律在 `git worktree` 里做，做完 `git worktree remove`。
- 不碰 `cloudpivot.service`、不碰生产库（`cloudpivot` / `_dev` / `_test`）。
  迁移演练只用临时库 `cloudpivot_mig_check`。
- 报告要诚实：闸门没绿就写没绿。「跑通了」必须是 `replay.sh` 的输出，不是推断。
