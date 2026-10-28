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


if problems:
    print("补丁校验失败：" + "；".join(problems), file=sys.stderr)
    raise SystemExit(4)

print("PATCH-STATE: OK（CODE-07 + CODE-09 + CODE-09b 补丁齐全）")
PY

if [[ "$CHECK_ONLY" == "1" ]]; then
    echo "仅检查模式：补丁齐全（--check 不写任何文件）。"
    exit 0
fi

echo
echo "已打补丁。下一步：cd backend && cargo build（会因源码变更重新编译 alun-web）。"
echo "如需回滚：删除 registry 中的 alun-web-0.4.2 目录后重新 cargo fetch（或 cargo clean -p alun-web）。"
