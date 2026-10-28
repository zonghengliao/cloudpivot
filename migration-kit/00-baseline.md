# 上游基线（00-baseline.md）

## 1. 基线是什么

| 项 | 值 |
|----|-----|
| 上游项目 | `cailu`（云枢/菜芦通用后台系统，Rust + Alun 框架 + Vue3 + TDesign） |
| **上游基线 commit** | `c4e8950` — `feat: 工作台增加「联系我们」弹窗，展示弘纹公众号二维码` |
| 基线之前的两个提交 | `c46451e`（alun 框架升 0.4.2）、`dc6d86d`（README 加联系方式）、`a97569b`（Apache-2.0 + 运行效果图）、`b00dbe1`（初始提交） |
| 我们的改动 | `c4e8950..HEAD`，26 个提交里 **22 个是我们的**（其余为基线本身） |
| 当前 HEAD | `26c9040`（含 ERP 文档归档）；「云枢改动终态」参照 `46bbe9a` |
| 本机路径（生产/dev） | `/opt/cloudpivot`，bare 备份 `/opt/git/cloudpivot.git` |

> 命名提醒：仓库里所有 `cailu` 都已改名为 `cloudpivot`，但**上游历史 commit 的 hash 不变**
> —— `c4e8950` 就是上游最后一次提交，用它做基线永远有效。

## 2. 我们改动的 22 个提交（时间顺序）

```
c4e8950  ← 上游基线（此后的都是我们的）
ba84d2a  chore(dev-env): 172.18.10.144 dev 环境改动（vite BACKEND_ORIGIN 可覆盖、dev 启停脚本、npm 镜像）
272e4ba  chore(clean): 移除公众号二维码与联系我们推广内容
b08f3a4  chore(clean): 移除 dashboard 关于项目卡片
a37e09c  security(P0): dev JWT 密钥改为环境变量注入 + Vite/code-server 收口 127.0.0.1
2a93007  security(P1): config-test jwt_secret 环境变量注入 + gitleaks 三级防线
560bce3  security(backend): 启动守卫 fail-closed
55d3c5d  feat(dashboard): 右栏改为服务器状态卡（仅超管可见）
6d93101  security(P3): config.toml 去掉 docker 默认库口令 alun
5850463  chore(hooks): post-commit 自动同步到本地 bare 仓库
179f0b2  security(P1): 用户管理加权限上限（提权三连）
af9310b  docs(monitor): 接口契约测试基线 + 线上产物指纹
2140b82  security(P1): 密钥治理 + 首登改密 + 会话吊销 + 框架 fail-fast/规则遮蔽
a23c276  security(P1): 首登强制改密前端闭环
42f7a84  security(P1): CODE-16 B（短 TTL / 绝对上限 / 复用检测）
d5a2b61  chore(lint): 收掉 §6 lint 债务          ← 以上整体 = 02-security/patches 序列
df9f658  refactor(rename): 改名 云枢(CloudPivot)  ← 01-rename 负责
04792dc  style(theme): 莫兰迪配色                 ← 03-brand/01-theme-morandi.patch
f3fc278  fix(scripts): patch-alun-web 判重        ← 01-rename/post-rename-patch-alun-web-dedupe.patch
d2d4775  chore: 验证 bare 同步链路（空提交）
46bbe9a  feat(brand): 新 logo/favicon            ← 03-brand/02-brand-assets.patch
26c9040  docs(erp): ERP 批次1 文档归档            ← 迁移准备包不重放（纯文档，见第 5 节）
```

## 3. 怎么重新拉上游

阶段 3 我们已经**彻底脱离**上游：`origin` 指向本机 bare `/opt/git/cloudpivot.git`，
upstream remote 已删除，仓库里不再记录 GitHub URL。

```bash
# 方案 A（推荐）：本机已有完整历史，直接取基线 commit，无需联网
cd /opt/cloudpivot
git cat-file -t c4e8950                 # 应输出 commit
git worktree add --detach /tmp/pro-base c4e8950

# 方案 B：全新机器 / 仓库丢了，从上游重新拉（需要网络与访问权）
git clone <上游仓库地址> cloudpivot-upstream
git -C cloudpivot-upstream log --oneline | grep c4e8950
git -C cloudpivot-upstream worktree add --detach /tmp/pro-base c4e8950
```

> ⚠ **不要把上游 URL 写回仓库配置**。需要时用临时 remote，用完即删：
> `git remote add tmp-up <url> && git fetch tmp-up && git remote remove tmp-up`。
> 改名脚本对 `github.com/wild1024/cailu` 这类 URL 有保护（见 special-cases 特例 5）。

## 4. Pro 到手后怎么对比

Q：Pro 是基于 `c4e8950` 往前做的，还是把底座重构了？用下面三步回答（全部基于 git，几分钟）。

```bash
cd /opt/cloudpivot
git remote add pro /path/to/pro-repo        # 或直接 worktree add
git fetch pro

# ① Pro 有没有从我们认识的基线长出来？
git merge-base HEAD pro/main                 # 若输出 c4e8950（或其后代）→ 同源
git log --oneline c4e8950..pro/main | wc -l  # Pro 在基线上加了多少提交

# ② Pro 动了多少我们动过的文件？（耦合度）
git diff --name-only c4e8950..pro/main  > /tmp/pro-touched.txt
git diff --name-only c4e8950..46bbe9a   > /tmp/ours-touched.txt
comm -12 <(sort /tmp/pro-touched.txt) <(sort /tmp/ours-touched.txt) | wc -l   # 交集文件数
comm -12 <(sort /tmp/pro-touched.txt) <(sort /tmp/ours-touched.txt) | head -30 # 看是哪些

# ③ Pro 有没有大改我们依赖的底座文件？（是否要重置基线）
for f in backend/src/lib.rs backend/src/bootstrap.rs backend/src/shared/constants.rs \
         backend/src/middleware/mod.rs backend/src/system/auth/handler.rs \
         backend/src/system/user/handler.rs frontend/src/router/index.ts frontend/src/stores/user.ts; do
  printf "%-45s " "$f"; git diff --numstat c4e8950..pro/main -- "$f" | awk '{print "+"$1" -"$2}' ; echo
done
```

判读标准见 `README.md` 的「30 分钟评估」：核心是
**交集文件数**（<10 且都是文档 → 功能移植；>25 或命中 auth/user/middleware → 重置基线）。

## 5. 期望的「重放结果」参照

用干净基线重放后，`replay.sh` 会拿结果和下面这些 commit 逐字节比：

| 重放到 | 树应等于 |
|--------|----------|
| `02-security/apply.sh` 跑完 | `d5a2b61` |
| `01-rename/rename.sh` 跑完 | `df9f658` |
| `01-rename/post-rename-patch-alun-web-dedupe.patch` 跑完 | `f3fc278` |
| `03-brand/apply.sh` 跑完 | `46bbe9a` |

**有意不重放的**：`d2d4775`（空提交，只验证 bare 同步）、`26c9040`（ERP 文档 5219 行）。
这两者与可运行代码无关；需要时从老仓库直接取：

```bash
git show 26c9040:docs/ERP-批次1-技术架构.md > docs/ERP-批次1-技术架构.md   # 以此类推
```
