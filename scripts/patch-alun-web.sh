#!/usr/bin/env bash
# 给 cargo registry 里的 alun-web 0.4.2 打安全补丁（框架侧两处 fail-open / 规则遮蔽）。
#
# 背景：这两个缺陷在**框架 crate 内部**，业务工程无法从外面修：
#
#   CODE-07  alun-web-0.4.2/src/app.rs:1533
#           `enabled && !jwt_secret.is_empty()` —— 密钥为空时静默不挂载认证层（fail-open），
#           此时提取器用空 HMAC 密钥验签，任何人自签 `permissions=["*:*:*"]` 即通权限层。
#   CODE-09  alun-web-0.4.2/src/middleware/permission_check.rs:139
#           「第一条命中」的规则匹配没有静态段优先级，且免认证路径也走权限规则，
#           导致 `GET /api/support/config/public` 被 `GET /support/config/{id}` 规则拦成 403。
#   CODE-09b 装配期「规则遮蔽自检」把 `xxx/page`（静态）与 `xxx/{id}`（模板）这种**兄弟
#           路由**全报成冲突（startup 实测刷 22 条 WARN，全是正常 REST 写法）。静态优先
#           打分已保证静态段胜出，兄弟路由不可能互相遮蔽；真正的缺陷是「同一路径模式被
#           声明了不同权限」（结果取决于注册顺序）。09b 把自检收敛到后者。
#   CODE-10  alun-web-0.4.2/src/middleware/auth.rs 只认 `Authorization` 头，
#           浏览器原生 `EventSource`（SSE）无法带自定义头 → 实时通知没法用头认证。
#           新增「查询参数令牌」，**仅**对 `ALUN_AUTH_QUERY_TOKEN_PATHS` 声明的端点开放，
#           走同一条 JWT 校验（签名/过期/类型/黑名单），身份只取令牌声明，令牌不落日志。
#
# 本脚本对 registry 源码做**幂等**文本补丁（带 `cloudpivot-patch` 标记，可重复执行）。
# 注意：`cargo update` / `cargo clean` / 换 CARGO_HOME 会重新解包 crate，届时需重跑本脚本；
# 业务侧的启动守卫（backend/src/shared/startup_guard.rs）不依赖本补丁，二者互为兜底。
#
# 用法：
#   bash scripts/patch-alun-web.sh            # 打补丁
#   bash scripts/patch-alun-web.sh --check    # 只检查是否已打，退出码 0=已打齐
set -euo pipefail

CHECK_ONLY=0
if [[ "${1:-}" == "--check" ]]; then
    CHECK_ONLY=1
fi

CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"
SRC_ROOT=$(ls -d "$CARGO_HOME_DIR"/registry/src/*/alun-web-0.4.2 2>/dev/null | head -n 1 || true)
if [[ -z "$SRC_ROOT" ]]; then
    echo "未找到 alun-web-0.4.2 源码（$CARGO_HOME_DIR/registry/src/*/alun-web-0.4.2）。" >&2
    echo "先在本工程执行一次 cargo fetch / cargo build，让 registry 解包。" >&2
    exit 2
fi

echo "alun-web 源码：$SRC_ROOT"

python3 - "$SRC_ROOT" "$CHECK_ONLY" <<'PY'
import sys
from pathlib import Path

src_root = Path(sys.argv[1])
check_only = sys.argv[2] == "1"
app_rs = src_root / "src" / "app.rs"
perm_rs = src_root / "src" / "middleware" / "permission_check.rs"
auth_rs = src_root / "src" / "middleware" / "auth.rs"
MARK_07 = "[cloudpivot-patch code-07]"
MARK_09 = "[cloudpivot-patch code-09]"


def load(path: Path) -> str:
    raw = path.read_bytes()
    # 保留原有行尾：框架 crate 用 LF
    return raw.decode("utf-8")


def save(path: Path, text: str) -> None:
    """落盘（`--check` 模式下只报「需要改动」，绝不写文件）。

    修复点：原实现在 `--check` 时也照样写盘 —— 与「检查模式只读」的约定相反，
    在别的机器上跑 `--check` 会顺手改掉 registry 源码（编译器随后重编）。
    """
    if check_only:
        print(f"  [check] 需要改动但未写入：{path.name}")
        return
    path.write_bytes(text.encode("utf-8"))


def apply(path: Path, old: str, new: str, *, marker: str, label: str) -> bool:
    """幂等替换：锚点已消失且补丁代码已在位 → 跳过。返回是否真的改动。

    判据不依赖 marker 的字面前缀（marker 曾用旧项目名，改名不能导致已打的补丁
    被重复插入）：只要「原锚点不存在」+「新代码的固定片段存在」就认定已打过。
    """
    text = load(path)
    sig = new.split(marker, 1)[0][-40:]   # 补丁代码在 marker 之前的那 40 字符，作为签名
    if old not in text:
        if sig in text:
            print(f"  [skip] {label}（已打过）")
            return False
        print(f"  [FAIL] {label}：锚点未命中 {path}", file=sys.stderr)
        raise SystemExit(3)
    if sig in text and marker in text:
        print(f"  [skip] {label}（已打过）")
        return False
    text = text.replace(old, new, 1)
    save(path, text)
    print(f"  [ok]   {label}")
    return True


# ── CODE-07：auth.enabled 但密钥为空/过短时拒绝启动 ────────────────────────────
old_auth = """            if cfg.middleware.auth.enabled && !cfg.middleware.auth.jwt_secret.is_empty() {
                let auth_asm_started = std::time::Instant::now();"""
new_auth = """            if cfg.middleware.auth.enabled {
                // [cloudpivot-patch code-07] fail-open → fail-fast。
                //
                // 原实现是 `enabled && !jwt_secret.is_empty()`：密钥为空时**什么也不挂**，
                // 且不报错；此时 AuthClaims 提取器仍会用空 HMAC 密钥验签，攻击者自签
                // permissions=["*:*:*"] 即可通行。这里改为启动即失败（含长度下限），
                // 与业务侧 backend/src/shared/startup_guard.rs 互为兜底。
                if cfg.middleware.auth.jwt_secret.trim().chars().count() < 32 {
                    panic!(
                        "FATAL: [middleware.auth] enabled=true 但 jwt_secret 缺失或短于 32 字符，\\
                         拒绝启动（请通过 ALUN_MIDDLEWARE_AUTH_JWT_SECRET 注入强密钥）"
                    );
                }
                let auth_asm_started = std::time::Instant::now();"""

changed_07 = apply(
    app_rs,
    old_auth,
    new_auth,
    marker=MARK_07,
    label="CODE-07 app.rs 认证层 fail-fast",
)

# ── CODE-09 ①：PermissionCheckLayer 增加「免认证路径」与冲突自检 ───────────────
old_field = """    pub rules: Arc<Vec<PermissionRule>>,
}"""
new_field = """    pub rules: Arc<Vec<PermissionRule>>,
    /// [cloudpivot-patch code-09] 免认证路径（`#[alun::no_auth]` / 配置 / 插件三个来源，含全局前缀）。
    ///
    /// 这些路径上认证层放行且**不注入** `AuthClaims`；若仍参与权限规则匹配，
    /// 就会被判成「无权限」——正是 `GET /api/support/config/public` 被
    /// `GET /support/config/{id}` 规则拦成 403 的根因。
    pub ignore_paths: Arc<Vec<String>>,
}"""

old_from_config = """    pub fn from_config(config_rules: &[alun_config::PermissionRule]) -> Self {
        Self {
            rules: Arc::new(
                config_rules
                    .iter()
                    .map(|r| PermissionRule {
                        path: r.path.clone(),
                        methods: r.methods.clone(),
                        permission: r.permission.clone(),
                    })
                    .collect(),
            ),
        }
    }"""
new_from_config = """    pub fn from_config(config_rules: &[alun_config::PermissionRule]) -> Self {
        Self {
            rules: Arc::new(
                config_rules
                    .iter()
                    .map(|r| PermissionRule {
                        path: r.path.clone(),
                        methods: r.methods.clone(),
                        permission: r.permission.clone(),
                    })
                    .collect(),
            ),
            ignore_paths: Arc::new(Vec::new()),
        }
    }

    /// [cloudpivot-patch code-09] 注入免认证路径（装配期调用一次）。
    pub fn with_ignore_paths(mut self, paths: Vec<String>) -> Self {
        self.ignore_paths = Arc::new(paths);
        self
    }

    /// [cloudpivot-patch code-09] 该请求路径是否免认证（精确命中或作为前缀命中）。
    pub fn is_no_auth_path(&self, request_path: &str) -> bool {
        self.ignore_paths.iter().any(|p| {
            let trimmed = p.trim_end_matches('/');
            request_path == trimmed || request_path.starts_with(&format!("{trimmed}/"))
        })
    }

    /// [cloudpivot-patch code-09b] 规则冲突自检：返回「同一路径被声明了不同权限」的描述。
    ///
    /// 装配期调用并 `warn!` 出来，避免同类问题再次静默上线（CI 里可当 error）。
    ///
    /// 判定口径（09b 收紧，理由见下）：
    /// * **兄弟路由不算冲突**：`/x/page`（静态）与 `/x/{id}`（模板）天然会同时匹配到
    ///   `/x/page`，但匹配器已按「完全相等(0) > 纯静态(1) > 含参数(2)」打分（CODE-09），
    ///   静态段永远胜出，两者的权限谁更严都不会串味。把这种成对路由都报成冲突，只会在
    ///   启动日志里刷几十条「请修正规则」的噪音（实测 22 条，全部是正常 REST 写法）。
    /// * **真正的冲突**是「同一路径模式被两条规则要求了不同权限」：此时最终用哪条权限
    ///   取决于注册顺序，属确定性缺陷 —— 这正是本自检要抓的东西。
    pub fn detect_conflicts(&self) -> Vec<String> {
        let mut conflicts = Vec::new();
        for (index, left) in self.rules.iter().enumerate() {
            for right in self.rules.iter().skip(index + 1) {
                if left.permission == right.permission {
                    continue;
                }
                let methods_overlap = left.methods.is_empty()
                    || right.methods.is_empty()
                    || left
                        .methods
                        .iter()
                        .any(|m| right.methods.iter().any(|p| p.eq_ignore_ascii_case(m)));
                if methods_overlap && normalized_pattern(&left.path) == normalized_pattern(&right.path)
                {
                    conflicts.push(format!(
                        "同一路径被声明了不同权限：{} ({}) 与 {} ({})",
                        left.path, left.permission, right.path, right.permission
                    ));
                }
            }
        }
        conflicts
    }"""

changed_09a = apply(
    perm_rs,
    old_field,
    new_field,
    marker=MARK_09,
    label="CODE-09 layer 增加 ignore_paths + detect_conflicts",
)
# new_from_config 依赖上面的 marker 判断不可靠（同一 marker 已存在），单独判重
text = load(perm_rs)
if "pub fn with_ignore_paths" not in text:
    if old_from_config not in text:
        print("  [FAIL] CODE-09 from_config 锚点未命中", file=sys.stderr)
        raise SystemExit(3)
    text = text.replace(old_from_config, new_from_config, 1)
    save(perm_rs, text)
    print("  [ok]   CODE-09 from_config 初始化 ignore_paths + 新增能力方法")
else:
    print("  [skip] CODE-09 from_config（已打过）")

# ── CODE-09b：冲突自检收敛到「同一路径不同权限」（幂等，版本无关） ──────────
# 不贴历史文本判重（历史上有原始版 / 中间版 / 最终版，旧标记还是 `cailu-patch`）：
# 用「方法起点 → 下一个方法文档注释起点」的区间整段替换，天然兼容任意历史形态。
_perm_text = load(perm_rs)
FINAL_BODY = """    /// [cloudpivot-patch code-09b] 规则冲突自检：返回「同一路径被声明了不同权限」的描述。
    ///
    /// 装配期调用并 `warn!` 出来，避免同类问题再次静默上线（CI 里可当 error）。
    ///
    /// 判定口径（09b 收紧，理由见下）：
    /// * **兄弟路由不算冲突**：`/x/page`（静态）与 `/x/{id}`（模板）天然会同时匹配到
    ///   `/x/page`，但匹配器已按「完全相等(0) > 纯静态(1) > 含参数(2)」打分（CODE-09），
    ///   静态段永远胜出，两者权限谁更严都不会串味。把这种成对路由都报成冲突，只会在
    ///   启动日志里刷几十条「请修正规则」的噪音（实测 22 条，全部是正常 REST 写法）。
    /// * **真正的冲突**是「同一路径模式被两条规则要求了不同权限」：此时最终用哪条权限
    ///   取决于注册顺序，属确定性缺陷 —— 这正是本自检要抓的东西。
    pub fn detect_conflicts(&self) -> Vec<String> {
        let mut conflicts = Vec::new();
        for (index, left) in self.rules.iter().enumerate() {
            for right in self.rules.iter().skip(index + 1) {
                if left.permission == right.permission {
                    continue;
                }
                let methods_overlap = left.methods.is_empty()
                    || right.methods.is_empty()
                    || left
                        .methods
                        .iter()
                        .any(|m| right.methods.iter().any(|p| p.eq_ignore_ascii_case(m)));
                if methods_overlap
                    && normalized_pattern(&left.path) == normalized_pattern(&right.path)
                {
                    conflicts.push(format!(
                        "同一路径被声明了不同权限：{} ({}) 与 {} ({})",
                        left.path, left.permission, right.path, right.permission
                    ));
                }
            }
        }
        conflicts
    }"""
if "normalized_pattern" in _perm_text:
    print("  [skip] CODE-09b 冲突自检（已是最终版）")
else:
    _key = "pub fn detect_conflicts(&self) -> Vec<String> {"
    _next_marker = "/// 合并 `#[permission]` 宏注解的编译期规则"
    _at = _perm_text.find(_key)
    _next = _perm_text.find(_next_marker, _at) if _at >= 0 else -1
    if _at < 0 or _next < 0:
        print("  [FAIL] CODE-09b detect_conflicts 定位失败", file=sys.stderr)
        raise SystemExit(3)
    # 按行操作：从方法行向上吃掉连续的 `///` 文档注释，向下到下一个方法的文档注释
    _lines = _perm_text.split("\n")
    _method_line = next(i for i, line in enumerate(_lines) if _key in line)
    _doc_start_line = _method_line
    while _doc_start_line > 0 and _lines[_doc_start_line - 1].strip().startswith("///"):
        _doc_start_line -= 1
    _end_line = next(
        i
        for i, line in enumerate(_lines)
        if _next_marker in line and i > _method_line
    )
    _new_lines = _lines[:_doc_start_line] + FINAL_BODY.split("\n") + [""] + _lines[_end_line:]
    _perm_text = "\n".join(_new_lines)
    if "fn normalized_pattern" not in _perm_text:
        _anchor = "/// 路径级权限校验中间件（配置 + 宏注解联合校验）"
        _helper = """/// 把路径里的参数段归一化成 `{}`（`/x/{id}` 与 `/x/{uuid}` 视为同一模式）。
///
/// [cloudpivot-patch code-09b] 冲突自检要比较的是「路径模式是否相同」，
/// 参数名不同（`{id}` / `{uuid}`）的同一模式同样会互相遮蔽，必须视为同一路径。
fn normalized_pattern(path: &str) -> String {
    path.trim_matches('/')
        .split('/')
        .map(|seg| {
            if seg.starts_with('{') && seg.ends_with('}') {
                "{}"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// 路径级权限校验中间件（配置 + 宏注解联合校验）"""
        if _anchor not in _perm_text:
            print("  [FAIL] CODE-09b normalized_pattern 锚点未命中", file=sys.stderr)
            raise SystemExit(3)
        _perm_text = _perm_text.replace(_anchor, _helper, 1)
    save(perm_rs, _perm_text)
    print("  [ok]   CODE-09b 冲突自检升级为「同一路径不同权限」")

# ── CODE-09 ②：Service 传递 ignore_paths ──────────────────────────────────────
# 判重按「补丁代码是否已在位」，不按 marker 字面（marker 前缀随项目改名变过，
# 用 marker 判重会在改名后误判成未打，进而重复插入 ignore_paths 字段）
text = load(perm_rs)
if "ignore_paths: Arc::clone(&self.ignore_paths)," in text:
    print("  [skip] CODE-09 service 字段（已打过）")
else:
    old_layer = """        PermissionCheckService {
            inner,
            rules: Arc::clone(&self.rules),
        }"""
    new_layer = """        PermissionCheckService {
            inner,
            rules: Arc::clone(&self.rules),
            // [cloudpivot-patch code-09] svc
            ignore_paths: Arc::clone(&self.ignore_paths),
        }"""
    old_svc_field = """    inner: S,
    /// 权限规则列表（与层共享，装配时零深克隆）
    rules: Arc<Vec<PermissionRule>>,
}"""
    new_svc_field = """    inner: S,
    /// 权限规则列表（与层共享，装配时零深克隆）
    rules: Arc<Vec<PermissionRule>>,
    /// [cloudpivot-patch code-09] 免认证路径（与层共享）
    ignore_paths: Arc<Vec<String>>,
}"""
    for old, new, label in (
        (old_layer, new_layer, "service 构造传递 ignore_paths"),
        (old_svc_field, new_svc_field, "service 字段"),
    ):
        if old not in text:
            print(f"  [FAIL] CODE-09 {label} 锚点未命中", file=sys.stderr)
            raise SystemExit(3)
        text = text.replace(old, new, 1)
        print(f"  [ok]   CODE-09 {label}")
    save(perm_rs, text)

# ── CODE-09 ③：匹配逻辑——免认证跳过 + 静态优先 ───────────────────────────────
old_call = """        match self.rules.iter().find(|rule| {
            let path_matches = match_path(&path, &rule.path);
            let method_matches = rule.methods.is_empty()
                || rule.methods.iter().any(|m| m.eq_ignore_ascii_case(&method));
            path_matches && method_matches
        }) {
            Some(rule) => {"""
new_call = """        // [cloudpivot-patch code-09] 免认证路径直接放行：认证层在这些路径上不注入
        // AuthClaims，若继续走规则匹配必然被判 403（`/support/config/public` 即此例）。
        if self.is_no_auth_path(&path) {
            tracing::debug!("权限检查: path={} 属于免认证路径，跳过权限规则", path);
            let mut inner = self.inner.clone();
            return Box::pin(async move { inner.call(req).await });
        }

        // [cloudpivot-patch code-09] 匹配优先级：完全相等(0) > 纯静态段(1) > 含 {param}(2)。
        // 原实现取「第一条命中」，语义取决于注册顺序，静态子路径会被模板规则遮蔽。
        let mut best: Option<(&PermissionRule, u8)> = None;
        for rule in self.rules.iter() {
            let path_matches = match_path(&path, &rule.path);
            let method_matches = rule.methods.is_empty()
                || rule.methods.iter().any(|m| m.eq_ignore_ascii_case(&method));
            if !path_matches || !method_matches {
                continue;
            }
            let score = if rule.path == path {
                0
            } else if !rule.path.contains('{') {
                1
            } else {
                2
            };
            match best {
                Some((_, best_score)) if best_score <= score => {}
                _ => best = Some((rule, score)),
            }
        }

        match best.map(|(rule, _)| rule) {
            Some(rule) => {"""

text = load(perm_rs)
# 判重按补丁代码是否已在位（同上：不能拿 marker 字面判重；`let mut best` 只存在于打过补丁的版本）
if "let mut best: Option<(&PermissionRule, u8)>" in text:
    print("  [skip] CODE-09 匹配逻辑（已打过）")
else:
    if old_call not in text:
        print("  [FAIL] CODE-09 匹配逻辑锚点未命中", file=sys.stderr)
        raise SystemExit(3)
    text = text.replace(old_call, new_call, 1)
    save(perm_rs, text)
    print("  [ok]   CODE-09 匹配逻辑（免认证跳过 + 静态优先）")

# ── CODE-09 ④：app.rs 装配期注入免认证路径 + 冲突告警 ─────────────────────────
old_perm_asm = """            let perm_asm_started = std::time::Instant::now();
            let mut perm_layer =
                mw::PermissionCheckLayer::from_config(&cfg.middleware.permission.rules);"""
new_perm_asm = """            let perm_asm_started = std::time::Instant::now();
            let mut perm_layer =
                mw::PermissionCheckLayer::from_config(&cfg.middleware.permission.rules);
            // [cloudpivot-patch code-09] 免认证路径必须让权限层跳过（见 permission_check.rs 注释）
            {
                let plugin_no_auth: Vec<crate::NoAuthDef> = self
                    .web_plugins
                    .iter()
                    .flat_map(|p| p.no_auth_paths())
                    .collect();
                let no_auth_paths: Vec<String> = crate::discovered_no_auth()
                    .iter()
                    .chain(self.extra_no_auth.iter())
                    .chain(plugin_no_auth.iter())
                    .map(|def| with_prefix(&self.prefix, def.path))
                    .collect();
                perm_layer = perm_layer.with_ignore_paths(no_auth_paths);
            }"""

text = load(app_rs)
if "with_ignore_paths" in text:
    print("  [skip] CODE-09 app.rs 注入免认证路径（已打过）")
else:
    if old_perm_asm not in text:
        print("  [FAIL] CODE-09 app.rs 装配锚点未命中", file=sys.stderr)
        raise SystemExit(3)
    text = text.replace(old_perm_asm, new_perm_asm, 1)
    save(app_rs, text)
    print("  [ok]   CODE-09 app.rs 注入免认证路径")

old_count = """            let macro_rule_count = macro_rules.len() + self.extra_permissions.len();"""
new_count = """            let macro_rule_count = macro_rules.len() + self.extra_permissions.len();
            // [cloudpivot-patch code-09] 规则遮蔽自检：装配期把「静态规则 vs 模板规则」冲突列出来
            for conflict in perm_layer.detect_conflicts() {
                tracing::warn!("权限规则冲突：{conflict}（已按「静态段优先」匹配，请修正规则）");
            }"""

text = load(app_rs)
if "perm_layer.detect_conflicts()" in text:
    print("  [skip] CODE-09 app.rs 冲突告警（已打过）")
else:
    if old_count not in text:
        print("  [FAIL] CODE-09 app.rs 计数锚点未命中", file=sys.stderr)
        raise SystemExit(3)
    text = text.replace(old_count, new_count, 1)
    save(app_rs, text)
    print("  [ok]   CODE-09 app.rs 装配期冲突告警")

# ── CODE-10 ①：AuthLayer 增加「查询参数令牌白名单」字段 ──────────────────────
old_layer_tail = """    /// 缓存层引用（用于缓存用户信息）
    #[cfg(feature = "cache")]
    pub cache: Option<alun_cache::SharedCache>,
}

impl<S> Layer<S> for AuthLayer {"""
new_layer_tail = """    /// 缓存层引用（用于缓存用户信息）
    #[cfg(feature = "cache")]
    pub cache: Option<alun_cache::SharedCache>,
    /// [cloudpivot-patch code-10] 允许用 `?token=` 认证的路径白名单（含全局前缀）。
    ///
    /// 只给浏览器 `EventSource` 这类无法设置自定义请求头的端点用（当前仅 SSE）。
    /// 空列表 = 完全不接受查询参数令牌（fail-closed，与打补丁前行为一致）。
    pub query_token_paths: Vec<String>,
}

impl<S> Layer<S> for AuthLayer {"""
auth_text = load(auth_rs)
if "code-10" in auth_text:
    print("  [skip] CODE-10 auth.rs 字段（已打过）")
else:
    if old_layer_tail not in auth_text:
        print("  [FAIL] CODE-10 auth.rs 字段锚点未命中", file=sys.stderr)
        raise SystemExit(3)
    auth_text = auth_text.replace(old_layer_tail, new_layer_tail, 1)
    save(auth_rs, auth_text)
    print("  [ok]   CODE-10 auth.rs 字段")

# ── CODE-10 ②：Layer::layer 转发 + AuthService 字段 + 查询参数解析实现 ────────
# 注意：锚点**不包含** `jwt_secret:` 那一行 —— 一是没必要，二是仓库的 gitleaks 规则会把
# 「jwt_secret: <非占位符>」这类文本判成疑似密钥（本文件只是引用框架字段名，不是密钥）。
old_layer_build = """            ignore_paths: self.ignore_paths.iter().cloned().collect(),
            #[cfg(feature = "cache")]
            cache: self.cache.clone(),
        }"""
new_layer_build = """            ignore_paths: self.ignore_paths.iter().cloned().collect(),
            #[cfg(feature = "cache")]
            cache: self.cache.clone(),
            // [cloudpivot-patch code-10] svc
            query_token_paths: self.query_token_paths.iter().cloned().collect(),
        }"""

old_svc = """#[derive(Clone)]
/// JWT 认证服务——验证 Bearer Token 并注入用户上下文到请求 extensions
pub struct AuthService<S> {
    /// 下游服务
    inner: S,
    /// JWT 密钥（HS256）
    jwt_secret: String,
    /// 跳过认证的路径集合
    ignore_paths: HashSet<String>,
    /// 缓存层引用（用于缓存用户信息）
    #[cfg(feature = "cache")]
    cache: Option<alun_cache::SharedCache>,
}"""
new_svc = """#[derive(Clone)]
/// JWT 认证服务——验证 Bearer Token 并注入用户上下文到请求 extensions
pub struct AuthService<S> {
    /// 下游服务
    inner: S,
    /// JWT 密钥（HS256）
    jwt_secret: String,
    /// 跳过认证的路径集合
    ignore_paths: HashSet<String>,
    /// 缓存层引用（用于缓存用户信息）
    #[cfg(feature = "cache")]
    cache: Option<alun_cache::SharedCache>,
    /// [cloudpivot-patch code-10] 允许用 `?token=` 认证的路径白名单（与层共享）
    query_token_paths: HashSet<String>,
}

impl<S> AuthService<S> {
    /// [cloudpivot-patch code-10] 从查询串取令牌 —— **只**认白名单路径上的 `token` 参数。
    ///
    /// 安全边界（与业务侧文档 `docs/批次A-SSE鉴权口径.md` 一一对应）：
    /// * 仅白名单路径（默认空 → 此函数永远 `None`）；
    /// * 只读 `token`，**绝不**接受 `user_id` / `receiver_id` 之类「客户端指定身份」的参数；
    /// * 拒绝含 `%` 的取值（JWT 字符集不需要百分号编码，出现即视为异常输入）；
    /// * 令牌只用于校验，不写日志、不进事件（日志只记 header|query 来源）。
    fn query_token_of(&self, path: &str, query: Option<&str>) -> Option<String> {
        if !self
            .query_token_paths
            .iter()
            .any(|allowed| allowed.trim_end_matches('/') == path)
        {
            return None;
        }
        let query = query?;
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("token=") {
                let value = value.trim();
                if !value.is_empty() && !value.contains('%') {
                    return Some(value.to_string());
                }
            }
        }
        None
    }
}"""
auth_text = load(auth_rs)
if "query_token_of" in auth_text:
    print("  [skip] CODE-10 auth.rs 服务字段与解析（已打过）")
else:
    for old, new, label in (
        (old_layer_build, new_layer_build, "Layer::layer 转发 query_token_paths"),
        (old_svc, new_svc, "AuthService 字段 + query_token_of"),
    ):
        if old not in auth_text:
            print(f"  [FAIL] CODE-10 {label} 锚点未命中", file=sys.stderr)
            raise SystemExit(3)
        auth_text = auth_text.replace(old, new, 1)
        print(f"  [ok]   CODE-10 {label}")
    save(auth_rs, auth_text)

# ── CODE-10 ③：call() 里接受查询参数令牌 ─────────────────────────────────────
old_token = """        let token_opt: Option<&str> = req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));

        match token_opt {"""
new_token = """        // [cloudpivot-patch code-10] 令牌来源：**优先** `Authorization: Bearer`；
        // 仅当没有头、且路径在白名单时，才回落到 `?token=`（浏览器 EventSource 用）。
        // 两条来源后续走完全相同的校验路径（签名 / 过期 / 类型 / 黑名单），
        // 身份只来自令牌声明 —— 查询串里任何「身份类」参数一律不予采信。
        let header_token: Option<&str> = req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        let query_token: Option<String> = if header_token.is_some() {
            None
        } else {
            self.query_token_of(&path, req.uri().query())
        };
        let token_opt: Option<&str> = header_token.or(query_token.as_deref());

        match token_opt {"""
auth_text = load(auth_rs)
if "code-10] 令牌来源" in auth_text:
    print("  [skip] CODE-10 auth.rs 取令牌（已打过）")
else:
    if old_token not in auth_text:
        print("  [FAIL] CODE-10 auth.rs 取令牌锚点未命中", file=sys.stderr)
        raise SystemExit(3)
    save(auth_rs, auth_text.replace(old_token, new_token, 1))
    print("  [ok]   CODE-10 auth.rs 取令牌（header 优先 + query 白名单回落）")

# ── CODE-10 ④：app.rs 装配 —— 从环境变量读白名单并传给 AuthLayer ────────────
# 两段小锚点拼装（同样避开 `jwt_secret:` 行，理由见上）
old_auth_head = """                let auth_layer = mw::AuthLayer {"""
new_auth_head = """                // [cloudpivot-patch code-10] `?token=` 白名单（逗号分隔、含全局前缀）。
                // 默认空 = 关闭（fail-closed）：只有显式配置了 SSE 端点的环境才接受查询令牌。
                let query_token_paths: Vec<String> = std::env::var("ALUN_AUTH_QUERY_TOKEN_PATHS")
                    .unwrap_or_default()
                    .split(',')
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect();
                if !query_token_paths.is_empty() {
                    info!(
                        "查询参数令牌白名单 endpoints={:?}（仅 EventSource 类端点；令牌不落日志）",
                        query_token_paths
                    );
                }
                let auth_layer = mw::AuthLayer {"""

old_auth_field = """                    ignore_paths: ignore,
                    #[cfg(feature = "cache")]
                    cache,
                };"""
new_auth_field = """                    ignore_paths: ignore,
                    // [cloudpivot-patch code-10] 仅这些端点接受 `?token=`
                    query_token_paths,
                    #[cfg(feature = "cache")]
                    cache,
                };"""

app_text = load(app_rs)
if "code-10] `?token=` 白名单" in app_text:
    print("  [skip] CODE-10 app.rs 装配（已打过）")
else:
    if old_auth_head not in app_text or old_auth_field not in app_text:
        print("  [FAIL] CODE-10 app.rs 装配锚点未命中", file=sys.stderr)
        raise SystemExit(3)
    app_text = app_text.replace(old_auth_head, new_auth_head, 1)
    app_text = app_text.replace(old_auth_field, new_auth_field, 1)
    save(app_rs, app_text)
    print("  [ok]   CODE-10 app.rs 装配（读取 ALUN_AUTH_QUERY_TOKEN_PATHS）")

# ── CODE-11：忽略列表支持**模板段**（T5b 任务级令牌的 4 个端点是 `/business/task/{id}/…`） ──
# 背景：`#[alun::no_auth]` 收集到的路径串里 `{id}` 是**字面量**，而原实现只做
# 「精确 / 前缀」比较，真实请求 `/api/business/task/0199…/comment` 永远命中不了：
#   · 认证层 → 没有 JWT 的请求直接 401（AI 的任务令牌不是 JWT）；
#   · 权限层 → 规则匹配不到 no_auth 路径，`business:task:read` 一律 403。
# 于是「AI 只能凭任务令牌写自己那一个任务」这条设计在 HTTP 层根本走不通（T5b 卡）。
#
# 本补丁把「忽略路径判定」抽成 `auth::is_ignored_path()`：保留原有精确 / 前缀语义，
# 新增**逐段比对**（模板段 `{param}` 或 `*` 匹配任意单段，段数必须相同）。
# 只放宽「忽略列表」的匹配，**不动**任何权限规则匹配（规则仍走 match_path 打分）。
MARK_11 = "[cloudpivot-patch code-11]"

old_ignore = """        let is_ignore_path = self.ignore_paths.contains(&path)
            || self
                .ignore_paths
                .iter()
                .any(|p| path.starts_with(&format!("{}/", p)));"""

new_ignore = """        // [cloudpivot-patch code-11] 忽略路径判定支持模板段（`{param}` / `*`）
        let is_ignore_path = is_ignored_path(self.ignore_paths.iter(), &path);"""

helper_anchor = "/// 校验 JWT 并提取声明 —— **认证的唯一真值实现**。"

helper_code = """/// [cloudpivot-patch code-11] 路径是否命中忽略列表（**精确 / 前缀 / 模板段**）。
///
/// 语义：
/// 1. 完全相等 → 命中（原行为）；
/// 2. 作为前缀命中（`/api/files/dl` 命中 `/api/files/dl/xxx`，原行为）；
/// 3. **逐段比对**：忽略项里的一段写成 `{param}` 或 `*` 时匹配任意单段，
///    段数必须相同。于是 `/api/business/task/{id}/comment` 能命中
///    `/api/business/task/0199…/comment`，但**不会**命中
///    `/api/business/task/0199…/comment/extra`（段数不同）。
///
/// 只用于「免认证路径」这一处判定；权限规则匹配仍由 `permission_check::match_path`
/// 负责（那边有静态段优先的打分，语义更强）。两处共用本函数保证「认证放行的路径」
/// 与「权限层跳过的路径」永远是同一批 —— 否则「认证过了但权限层 403」会变成
/// 一类只在特定路径上出现的怪问题。
pub(crate) fn is_ignored_path<'a, I>(paths: I, path: &str) -> bool
where
    I: IntoIterator<Item = &'a String>,
{
    paths.into_iter().any(|pattern| {
        let trimmed = pattern.trim_end_matches('/');
        if path == trimmed || path.starts_with(&format!("{trimmed}/")) {
            return true;
        }
        let mut want = trimmed.split('/');
        let mut got = path.split('/');
        loop {
            match (want.next(), got.next()) {
                (None, None) => return true,
                (Some(w), Some(g)) => {
                    let wildcard = (w.starts_with('{') && w.ends_with('}')) || w == "*";
                    if !wildcard && w != g {
                        return false;
                    }
                }
                _ => return false,
            }
        }
    })
}

/// 校验 JWT 并提取声明 —— **认证的唯一真值实现**。"""

auth_text = load(auth_rs)
if "code-11] 忽略路径判定" in auth_text:
    print("  [skip] CODE-11 auth.rs 忽略路径判定（已打过）")
else:
    if old_ignore not in auth_text:
        print("  [FAIL] CODE-11 auth.rs 忽略路径判定锚点未命中", file=sys.stderr)
        raise SystemExit(3)
    if helper_anchor not in auth_text:
        print("  [FAIL] CODE-11 auth.rs is_ignored_path 插入锚点未命中", file=sys.stderr)
        raise SystemExit(3)
    auth_text = auth_text.replace(old_ignore, new_ignore, 1)
    auth_text = auth_text.replace(helper_anchor, helper_code, 1)
    save(auth_rs, auth_text)
    print("  [ok]   CODE-11 auth.rs 忽略路径判定（is_ignored_path 支持模板段）")

# CODE-11 ②：permission_check.rs 的两处忽略判定改为与认证层同源
old_perm_fn = """    /// [cailu-patch code-09] 该请求路径是否免认证（精确命中或作为前缀命中）。
    pub fn is_no_auth_path(&self, request_path: &str) -> bool {
        self.ignore_paths.iter().any(|p| {
            let trimmed = p.trim_end_matches('/');
            request_path == trimmed || request_path.starts_with(&format!("{trimmed}/"))
        })
    }"""
new_perm_fn = """    /// [cailu-patch code-09] 该请求路径是否免认证（精确命中或作为前缀命中）。
    /// [cloudpivot-patch code-11] 增加模板段支持（`{param}` / `*`），实现与认证层同源。
    pub fn is_no_auth_path(&self, request_path: &str) -> bool {
        super::auth::is_ignored_path(self.ignore_paths.iter(), request_path)
    }"""

old_perm_call = """        if self.ignore_paths.iter().any(|p| {
            let trimmed = p.trim_end_matches('/');
            path == trimmed || path.starts_with(&format!("{trimmed}/"))
        }) {"""
new_perm_call = """        // [cloudpivot-patch code-11] 与认证层同一套判定（支持模板段）
        if super::auth::is_ignored_path(self.ignore_paths.iter(), &path) {"""

perm_text = load(perm_rs)
if "code-11] 与认证层同一套判定" in perm_text:
    print("  [skip] CODE-11 permission_check.rs 忽略路径（已打过）")
else:
    if old_perm_fn not in perm_text or old_perm_call not in perm_text:
        print("  [FAIL] CODE-11 permission_check.rs 锚点未命中", file=sys.stderr)
        raise SystemExit(3)
    perm_text = perm_text.replace(old_perm_fn, new_perm_fn, 1)
    perm_text = perm_text.replace(old_perm_call, new_perm_call, 1)
    save(perm_rs, perm_text)
    print("  [ok]   CODE-11 permission_check.rs 忽略路径与认证层同源")

# ── CODE-11b：模板段只匹配「像 id 的段」（收窄 CODE-11 的放行面） ──────────────
# 起因（生产实测）：CODE-11 让 `{param}` 匹配**任意**单段后，`/api/business/task/{id}`
# 这条 no_auth 模板把 `/api/business/task/page`、`/my`、`/gantt`、`/stats` 一起放行了 ——
# 认证层与权限层都跳过，请求直接落到「必须有人类/令牌身份」的 handler 上：
#   401「未授权访问」变成 500「Missing request extension AuthClaims」。
# 更危险的是：将来任何加在同一条路径下的公开端点都会**静默失去鉴权**。
# 修法：模板段只匹配 UUID / 纯数字 / 16–64 位不透明串（`page`/`my`/`gantt` 都不像 id）。
MARK_11B = "[cloudpivot-patch code-11b]"

old_wildcard = """                (Some(w), Some(g)) => {
                    let wildcard = (w.starts_with('{') && w.ends_with('}')) || w == "*";
                    if !wildcard && w != g {
                        return false;
                    }
                }"""

new_wildcard = """                (Some(w), Some(g)) => {
                    let wildcard = (w.starts_with('{') && w.ends_with('}')) || w == "*";
                    if wildcard {
                        if !wildcard_segment_matches(g) {
                            return false;
                        }
                    } else if w != g {
                        return false;
                    }
                }"""

regex_anchor = """pub(crate) fn is_ignored_path<'a, I>(paths: I, path: &str) -> bool"""

wildcard_helper = """/// [cloudpivot-patch code-11b] 模板段是否「像 id」。
///
/// 只接受三种形态：UUID（36 位十六进制 + 短横）、纯数字、16–64 位不透明串。
/// **不接受任意单段**：`/api/business/task/{id}` 是把「任务详情」声明成免认证的模板，
/// 若 `{id}` 能匹配 `page` / `my` / `gantt` / `stats`，那些静态端点会连认证层一起跳过，
/// 401 变 500，且将来加在同前缀下的公开端点会静默失去鉴权（生产实测踩过）。
/// 新增免认证模板且参数不是 id 形态时，必须在这里显式扩展规则，而不是放宽成 `*`。
fn wildcard_segment_matches(got: &str) -> bool {
    if got.is_empty() {
        return false;
    }
    let uuid_like =
        got.len() == 36 && got.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    let digits_only = got.chars().all(|c| c.is_ascii_digit());
    let opaque = (16..=64).contains(&got.len())
        && got
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    uuid_like || digits_only || opaque
}

pub(crate) fn is_ignored_path<'a, I>(paths: I, path: &str) -> bool"""

auth_text = load(auth_rs)
if MARK_11B in auth_text:
    print("  [skip] CODE-11b 模板段收窄（已打过）")
else:
    if old_wildcard not in auth_text:
        print("  [FAIL] CODE-11b 通配判定锚点未命中", file=sys.stderr)
        raise SystemExit(5)
    if regex_anchor not in auth_text:
        print("  [FAIL] CODE-11b is_ignored_path 锚点未命中", file=sys.stderr)
        raise SystemExit(5)
    auth_text = auth_text.replace(old_wildcard, new_wildcard, 1)
    auth_text = auth_text.replace(regex_anchor, wildcard_helper, 1)
    save(auth_rs, auth_text)
    print("  [ok]   CODE-11b auth.rs 模板段只匹配 id 形态（page/my/gantt 不再被放行）")


# ── 收尾校验 ─────────────────────────────────────────────────────────────────
final_app = load(app_rs)
final_perm = load(perm_rs)
problems = []
if "FATAL: [middleware.auth] enabled=true 但 jwt_secret" not in final_app:
    problems.append("app.rs 缺少 CODE-07 fail-fast")
if "with_ignore_paths" not in final_app or "detect_conflicts" not in final_app:
    problems.append("app.rs 缺少 CODE-09 装配")
if "pub fn with_ignore_paths" not in final_perm or "let mut best:" not in final_perm:
    problems.append("permission_check.rs 缺少 CODE-09 补丁")
if "normalized_pattern" not in final_perm:
    problems.append("permission_check.rs 缺少 CODE-09b（冲突自检归一化路径）")

final_auth = load(auth_rs)
if "query_token_of" not in final_auth or "header_token" not in final_auth:
    problems.append("auth.rs 缺少 CODE-10 查询参数令牌")
if "ALUN_AUTH_QUERY_TOKEN_PATHS" not in final_app:
    problems.append("app.rs 缺少 CODE-10 装配（ALUN_AUTH_QUERY_TOKEN_PATHS）")
if "is_ignored_path" not in final_auth:
    problems.append("auth.rs 缺少 CODE-11 忽略路径模板段（is_ignored_path）")
if "is_ignored_path" not in final_perm:
    problems.append("permission_check.rs 缺少 CODE-11 忽略路径同源判定")

if problems:
    print("补丁校验失败：" + "；".join(problems), file=sys.stderr)
    raise SystemExit(4)

print("PATCH-STATE: OK（CODE-07 + CODE-09 + CODE-09b + CODE-10 + CODE-11 补丁齐全）")
PY

if [[ "$CHECK_ONLY" == "1" ]]; then
    echo "仅检查模式：补丁齐全（--check 不写任何文件）。"
    exit 0
fi

echo
echo "已打补丁。下一步：cd backend && cargo build（会因源码变更重新编译 alun-web）。"
echo "如需回滚：删除 registry 中的 alun-web-0.4.2 目录后重新 cargo fetch（或 cargo clean -p alun-web）。"
