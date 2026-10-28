#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""云枢(CloudPivot) 03-brand / replace-colors.py —— 组件内硬编码色值批量替换

为什么需要这个脚本
------------------
`theme-final.less` 只接管 CSS 变量（--td-* / --app-*），但上游组件里还散落着
**青蓝色系硬编码**（`#0580a2`、`#3b6dff`、`rgb(5 128 162 / 12%)` …）。
只覆盖 theme.less 的话，横幅、图表、阴影、登录页渐变会继续停留在上游配色。

覆盖的三种写法（第三种是上次踩过的坑，务必别删）
------------------------------------------------
1. 6 位 hex            `#0580a2` / `#0580A2`
2. 8 位 hex（带 alpha） `#0580a21f`  → 保留 alpha，只换 RGB
3. rgb() 系列          `rgb(5 128 162 / 12%)`、`rgb(5, 128, 162)`、`rgba(5,128,162,.12)`
   替换后统一输出**现代语法** `rgb(R G B / A)`，与项目现有写法一致。

另外两类改动单靠换色值做不出来，用 SNIPPETS 精确片段替换（只在上游原文还在时生效）
---------------------------------------------------------------------------
- 登录页渐变 → `var(--app-login-bg)` / `var(--app-login-brand)`（否则深色主题下浅底浅字）
- 头像渐变、选中态、表头底色、统计文案竖线、卡片/滚动条阴影 → 变量化 + 品牌色

设计原则
--------
* **幂等**：重复跑不二次破坏（替换后的值不再是映射键；片段替换带 marker 判重）
* **不猜**：片段锚点找不到 → 打印 `[skip]` 告警而不是报错中断（Pro 版改了结构也能跑）
* **留痕**：未在映射表里的色值 → `[warn]` 列出（Pro 版可能引入新色值，需人工判断）
* **不改语义**：成功/警告/错误只做低饱和调整（绿/橙/绛红仍可辨），不改成灰色

用法
----
  python3 replace-colors.py --root <repo>            # 应用
  python3 replace-colors.py --root <repo> --dry-run  # 只打印，不落盘
  python3 replace-colors.py --root <repo> --check    # 只检查（残留 / 未映射 / 关键色值）
  --paths frontend/src frontend/index.html           # 扫描范围（默认这两个）
  --json out.json                                    # 机器可读结果

退出码：0 干净 / 1 有问题（--check 时：残留或未映射）/ 2 用法错误
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from collections import Counter, OrderedDict

# =============================================================================
# 1) 色值映射表：上游青蓝色系 → 雾蓝流金最终色板
#    key 一律小写 6 位 hex（不带 #），value 同样。分组只是为了输出好读。
# =============================================================================
COLOR_MAP_GROUPS: "OrderedDict[str, list[tuple[str, str]]]" = OrderedDict([
    # ---- 亮色品牌色阶 brand-1..10（上游 H=193 青蓝 → 晨雾蓝 H≈218）----
    ("品牌色阶（亮）", [
        ("f6fafb", "eff3f9"),   # brand-1 浅底 / 选中底
        ("ecf6f8", "e3eaf4"),   # brand-2 light-hover
        ("ddf0f5", "cdd8eb"),   # brand-3 disabled 底
        ("c6e7f1", "b3c1da"),   # brand-4 浅边框
        ("90d6ea", "a5b3cb"),   # brand-5 深边框（雾散初光）
        ("0398c1", "8ba0c7"),   # brand-6 hover
        ("0580a2", "6f8ab8"),   # brand-7 ★主色（晨雾蓝）
        ("017190", "56729f"),   # brand-8 active（严格 AA 白字底）
        ("035e77", "475f86"),   # brand-9 深底强调
        ("054d61", "384c6d"),   # brand-10 最深强调
    ]),
    # ---- 旧 logo 渐变色（theme 注释 + 可能的装饰场景）----
    ("旧 logo 渐变", [
        ("033c85", "384c6d"),
        ("055e95", "475f86"),
        ("0c93ab", "56729f"),
        ("0db3b5", "6f8ab8"),
        ("0fbdba", "8ba0c7"),
    ]),
    # ---- 图表色（亮）1..10：七色板循环 ----
    ("图表色（亮）", [
        ("03a3cc", "c1ac7b"),
        ("35c0d8", "c17b7e"),
        ("6fd5e8", "a5b3cb"),
        ("a3e5f2", "cfc4aa"),
        ("cdefa9", "8ba0c7"),
        ("e8f5b8", "cfaaab"),
        ("f3e9a6", "475f86"),
        ("f7dfa0", "b3c1da"),
        ("f4d99c", "a8905e"),
    ]),
    # ---- 暗色品牌色阶 1..10 ----
    ("品牌色阶（暗）", [
        ("10333c", "1e2530"),
        ("134553", "26303d"),
        ("16586a", "2e3a49"),
        ("15687f", "374657"),
        ("137996", "45596e"),
        ("0f8bae", "566a86"),
        ("0a7e9e", "6381b4"),   # 暗色主色
        ("1491b3", "7d96c4"),
        ("35aed0", "97acd2"),
        ("81c7da", "b0c1de"),
    ]),
    ("图表色（暗补充）", [
        ("b6dbe8", "ddd6c4"),
    ]),
    # ---- 组件里散落的非品牌硬编码 ----
    ("组件散落色", [
        ("3b6dff", "6f8ab8"),   # 上游蓝：图表 1 号线 / 指标卡蓝 / 横幅阴影
        ("4b8bff", "6f8ab8"),   # 头像渐变起点（片段规则通常已变量化）
        ("7b5cff", "c1ac7b"),   # 头像渐变终点 → 落日流金
        ("6a4bff", "c1ac7b"),   # 图表紫 → 落日流金
        ("f54a45", "c17b7e"),   # 危险红 → 朱砂绛红（语义保留）
        ("12b76a", "3e9b72"),   # 成功绿 → 低饱和绿（语义保留，不变灰）
        ("f5822b", "d98b4a"),   # 警告橙 → 低饱和橙（语义保留，不变灰）
        ("5e70b4", "6f8ab8"),   # rgb(94 112 180 / 14%) 登录卡片投影
        ("7a8eff", "c1ac7b"),   # rgb(122 142 255 / 22%) 登录柔光 → 流金
        ("0f1e46", "6f8ab8"),   # rgb(15 30 70 / 6%) 卡片投影 → 品牌蓝投影
        ("eff4ff", "f2f2ee"),   # 登录页底渐变（片段规则通常已变量化）
        ("f6f2ff", "eaeef0"),
        ("e9f6fb", "f2f2ee"),
        ("e6edff", "e8edf1"),   # 登录品牌区渐变（片段规则通常已变量化）
        ("efeaff", "dce2e6"),
        ("e5f4fa", "eae3db"),
        ("f5f7fa", "f2f2ee"),   # 改密页 var fallback → 霜月白
    ]),
])

COLOR_MAP: "dict[str, str]" = {}
COLOR_GROUP: "dict[str, str]" = {}
for _g, _pairs in COLOR_MAP_GROUPS.items():
    for _k, _v in _pairs:
        COLOR_MAP[_k] = _v
        COLOR_GROUP[_k] = _g

# 反过来：新色值集合（替换结果，不算「未映射告警」）
NEW_PALETTE = set(COLOR_MAP.values()) | {
    # theme-final.less 里出现、但组件也可能直接用到的七色板/中性色
    "ffffff", "f2f2ee", "fafaf7", "f7f7f4", "eaeef0", "eae3db",
    "aab7c2", "dce2e6", "2f3a4d", "5a6377", "7a8a98", "78819b",
    "6f8ab8", "a5b3cb", "c1ac7b", "cfc4aa", "cfaaab", "c17b7e",
    "51697c", "56729f", "475f86", "384c6d", "8ba0c7", "a8905e",
    "b3c1da", "cdd8eb", "e3eaf4", "eff3f9", "e8edf1",
    "6381b4", "7d96c4", "97acd2", "b0c1de", "ddd6c4", "ddbdbd",
}

# =============================================================================
# 2) 片段替换：变量化 / 结构化的品牌改动（换色值换不出来）
#    每条：file（相对仓库根）、name、old（上游原文，唯一匹配）、new、marker（判已应用）
#    old 找不到 → 打印 [skip] 告警并继续（Pro 版改了结构也不会中断）
# =============================================================================
SNIPPETS: "list[dict[str, str]]" = [
    # ---------- 布局：头像渐变 + 品牌区下沿 + 顶栏投影 ----------
    dict(file="frontend/src/layouts/BasicLayout.vue", name="头像挂上 brand 类",
         old='<t-avatar  size="medium">',
         new='<t-avatar class="basic-layout__avatar" size="medium">',
         marker='basic-layout__avatar'),
    dict(file="frontend/src/layouts/BasicLayout.vue", name="品牌区下沿改点缀色",
         old="    cursor: pointer;\n    border-bottom: 1px solid var(--td-component-stroke);",
         new=("    cursor: pointer;\n"
              "    // 品牌区下沿用流金半透明细线：全站点缀落点之一（不用大面积色块）\n"
              "    border-bottom: 1px solid var(--app-accent-soft);"),
         marker="border-bottom: 1px solid var(--app-accent-soft);"),
    dict(file="frontend/src/layouts/BasicLayout.vue", name="顶栏投影改品牌调",
         old="box-shadow: 0 1px 4px rgb(0 0 0 / 4%);",
         new=("// 晨雾蓝调的轻投影，替代中性黑投影\n"
              "    box-shadow: 0 1px 4px rgb(111 138 184 / 12%);"),
         marker="box-shadow: 0 1px 4px rgb(111 138 184 / 12%);"),
    dict(file="frontend/src/layouts/BasicLayout.vue", name="头像渐变变量化",
         old="background: linear-gradient(135deg, #4b8bff 0%, #7b5cff 100%);",
         new=("// 晨雾蓝 → 落日流金 的渐变，随明暗主题取不同深浅\n"
              "    background: linear-gradient(135deg, var(--td-brand-color-7) 0%, var(--app-brand-accent) 100%);"),
         marker="var(--td-brand-color-7) 0%, var(--app-brand-accent) 100%"),
    # ---------- 全局样式：卡片投影 / 选中态 / 表头 / 统计竖线 / 滚动条 ----------
    dict(file="frontend/src/style/index.less", name="卡片投影带品牌色",
         old="box-shadow: 0 1px 3px rgb(15 30 70 / 6%);",
         new=("// 投影带一点晨雾蓝调，与雾蓝底色呼应，避免灰黑投影显脏\n"
              "  box-shadow: 0 1px 3px rgb(111 138 184 / 10%);"),
         marker="box-shadow: 0 1px 3px rgb(111 138 184 / 10%);"),
    dict(file="frontend/src/style/index.less", name="文本选中态（点缀浅底）",
         old="// ---------- 列表页通用结构",
         new=("// ---------- 文本选中：流金浅底（点缀色，不抢内容）----------\n"
              "::selection {\n"
              "  color: var(--td-text-color-primary);\n"
              "  background-color: var(--app-accent-soft);\n"
              "}\n"
              "\n"
              "// ---------- 列表页通用结构"),
         marker="::selection {"),
    dict(file="frontend/src/style/index.less", name="表头暖白底",
         old="// 表格工具栏：左右分列",
         new=("// 表头：暖白底（与霜月白同族），避免默认纯白让表头与行内容分层不清\n"
              ".t-table thead th {\n"
              "  background-color: var(--app-table-head-bg);\n"
              "}\n"
              "\n"
              "// 表格工具栏：左右分列"),
         marker=".t-table thead th {"),
    dict(file="frontend/src/style/index.less", name="统计文案流金竖线",
         old=(".table-toolbar-count {\n"
              "  font-size: 13px;\n"
              "  color: var(--td-text-color-secondary);\n"
              "}"),
         new=(".table-toolbar-count {\n"
              "  font-size: 13px;\n"
              "  color: var(--td-text-color-secondary);\n"
              "\n"
              "  // 流金短竖线：全站「少量点缀」的落点之一（不铺色，只做视觉锚点）\n"
              "  &::before {\n"
              "    content: '';\n"
              "    display: inline-block;\n"
              "    width: 3px;\n"
              "    height: 11px;\n"
              "    margin-right: 6px;\n"
              "    vertical-align: -1px;\n"
              "    background-color: var(--app-brand-accent);\n"
              "    border-radius: 2px;\n"
              "  }\n"
              "}"),
         marker="background-color: var(--app-brand-accent);"),
    dict(file="frontend/src/style/index.less", name="滚动条滑块改品牌蓝",
         old="background-color: rgb(0 0 0 / 16%);",
         new="background-color: rgb(111 138 184 / 20%);",
         marker="background-color: rgb(111 138 184 / 20%);"),
    dict(file="frontend/src/style/index.less", name="滚动条 hover 改品牌蓝",
         old="background-color: rgb(0 0 0 / 26%);",
         new="background-color: rgb(111 138 184 / 32%);",
         marker="background-color: rgb(111 138 184 / 32%);"),
    # ---------- 登录页：渐变与边框交给 theme 变量（深色主题才不翻车）----------
    dict(file="frontend/src/views/login/index.vue", name="登录页底渐变变量化",
         old=("  // 整体柔和渐变：淡蓝 → 淡紫 → 淡青，低饱和，长时间看不刺眼\n"
              "  background: linear-gradient(135deg, #eff4ff 0%, #f6f2ff 50%, #e9f6fb 100%);"),
         new=("  // 霜月白主底 + 雾蓝灰轻过渡：低饱和配色，长时间看不刺眼（深色主题走 --app-login-bg）\n"
              "  background: var(--app-login-bg);"),
         marker="background: var(--app-login-bg);"),
    dict(file="frontend/src/views/login/index.vue", name="登录卡片边框改描边色",
         old="border: 1px solid rgb(255 255 255 / 70%);",
         new="border: 1px solid var(--td-component-stroke);",
         marker="border: 1px solid var(--td-component-stroke);"),
    dict(file="frontend/src/views/login/index.vue", name="登录品牌区渐变变量化",
         old=("  // 柔和渐变底，与页面外框同一色系但略深一档，形成层次\n"
              "  background: linear-gradient(155deg, #e6edff 0%, #efeaff 55%, #e5f4fa 100%);"),
         new=("  // 柔和渐变底：霜月白 → 雾蓝灰 → 流金微暖收尾（深色主题走 --app-login-brand）\n"
              "  background: var(--app-login-brand);"),
         marker="background: var(--app-login-brand);"),
    dict(file="frontend/src/views/login/index.vue", name="登录柔光改流金",
         old="radial-gradient(circle, rgb(122 142 255 / 22%) 0%, rgb(122 142 255 / 0%) 70%)",
         new="radial-gradient(circle, rgb(193 172 123 / 26%) 0%, rgb(193 172 123 / 0%) 70%)",
         marker="rgb(193 172 123 / 26%)"),
    dict(file="frontend/src/views/login/index.vue", name="柔光注释更新",
         old="// 柔光装饰：不抢内容，只负责让渐变不死板\n",
         new="// 柔光装饰：不抢内容，只负责让渐变不死板（流金极轻光晕，全站唯一的大面积点缀）\n",
         marker="极轻光晕"),
    # ---------- 首登改密页（0013 补丁新建）：var fallback 对齐最终色板 ----------
    dict(file="frontend/src/views/user/change-password.vue", name="改密页卡片底色 fallback",
         old="var(--app-card-bg, #fff)",
         new="var(--app-card-bg, #ffffff)",
         marker="var(--app-card-bg, #ffffff)"),
    dict(file="frontend/src/views/user/change-password.vue", name="改密页卡片投影改品牌调",
         old="box-shadow: 0 8px 24px rgb(0 0 0 / 8%);",
         new="box-shadow: 0 8px 24px rgb(111 138 184 / 12%);",
         marker="box-shadow: 0 8px 24px rgb(111 138 184 / 12%);"),
    dict(file="frontend/src/views/user/change-password.vue", name="改密页主文字 fallback",
         old="var(--app-text-primary, rgb(0 0 0 / 88%))",
         new="var(--app-text-primary, #2f3a4d)",
         marker="var(--app-text-primary, #2f3a4d)"),
    dict(file="frontend/src/views/user/change-password.vue", name="改密页次文字 fallback",
         old="var(--app-text-secondary, rgb(0 0 0 / 60%))",
         new="var(--app-text-secondary, #5a6377)",
         marker="var(--app-text-secondary, #5a6377)"),
    # ---------- 仪表盘：指标卡蓝改走品牌变量（横幅渐变不在本脚本里动！） ----------
    dict(file="frontend/src/views/dashboard/index.vue", name="指标卡默认色走品牌变量",
         old="background-color: var(--metric-color, #3b6dff);",
         new="background-color: var(--metric-color, var(--td-brand-color-7));",
         marker="var(--metric-color, var(--td-brand-color-7))"),
    dict(file="frontend/src/views/dashboard/index.vue", name="指标卡蓝色档走品牌变量",
         old="--metric-color: #3b6dff;",
         new="--metric-color: var(--td-brand-color-7);",
         marker="--metric-color: var(--td-brand-color-7);"),
]

# =============================================================================
# 3) 扫描范围 / 排除
# =============================================================================
DEFAULT_PATHS = ["frontend/src", "frontend/index.html"]
TEXT_EXT = {".vue", ".ts", ".tsx", ".js", ".jsx", ".less", ".scss", ".css",
            ".html", ".htm", ".json", ".svg", ".rs"}
SKIP_EXT = {".md", ".lock", ".png", ".jpg", ".jpeg", ".ico", ".woff", ".woff2",
            ".ttf", ".pdf", ".zip", ".gz"}
EXCLUDE_DIRS = {"node_modules", "dist", ".git", "target", ".brand-backup",
                ".rename-backup", ".worktrees"}

HEX_RE = re.compile(r"#([0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{3,4})\b")
RGB_RE = re.compile(
    r"rgba?\(\s*(\d{1,3})\s*[,\s]\s*(\d{1,3})\s*[,\s]\s*(\d{1,3})\s*"
    r"(?:[,/]\s*([0-9.]+%?)\s*)?\)",
    re.I,
)


def rgb_to_hex(r: int, g: int, b: int) -> str:
    return "%02x%02x%02x" % (r, g, b)


def hex_to_rgb(h: str) -> tuple[int, int, int]:
    return int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)


def expand_hex(tok: str) -> str:
    """把 #abc / #abcd 展开成 6 位（丢弃 alpha 前先展开）。返回不带 # 的小写 hex6。"""
    t = tok.lower()
    if len(t) == 3:
        return "".join(c * 2 for c in t)
    if len(t) == 4:
        return "".join(c * 2 for c in t[:3])
    return t[:6]


def iter_files(root: str, paths: list[str]):
    for rel in paths:
        p = os.path.join(root, rel)
        if os.path.isfile(p):
            yield rel, p
            continue
        for dirpath, dirnames, filenames in os.walk(p):
            dirnames[:] = sorted(d for d in dirnames if d not in EXCLUDE_DIRS)
            for fn in sorted(filenames):
                ext = os.path.splitext(fn)[1].lower()
                if ext in SKIP_EXT or (TEXT_EXT and ext not in TEXT_EXT):
                    continue
                full = os.path.join(dirpath, fn)
                yield os.path.relpath(full, root), full


def read_text(path: str) -> str | None:
    try:
        raw = open(path, "rb").read()
    except OSError:
        return None
    if b"\x00" in raw:  # 二进制
        return None
    # newline="" 保留原始行尾（上游 dashboard 是 CRLF，别顺手改掉）
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        return None


def write_text(path: str, text: str) -> None:
    with open(path, "wb") as f:
        f.write(text.encode("utf-8"))


def apply_snippets(rel: str, text: str) -> tuple[str, list[dict], list[dict]]:
    """返回 (新文本, 已应用列表, 跳过列表)"""
    applied, skipped = [], []
    for sn in SNIPPETS:
        if sn["file"] != rel:
            continue
        marker = sn.get("marker") or sn["new"]
        if marker in text:
            applied.append({"name": sn["name"], "state": "already"})
            continue
        if sn["old"] not in text:
            skipped.append({"name": sn["name"], "why": "锚点不存在（Pro 版可能已改）"})
            continue
        text = text.replace(sn["old"], sn["new"], 1)
        applied.append({"name": sn["name"], "state": "applied"})
    return text, applied, skipped


def apply_color_map(text: str) -> tuple[str, Counter]:
    """hex + rgb 三种写法统一替换，返回 (新文本, {old: 次数})"""
    hits: Counter = Counter()

    def hex_sub(m: re.Match) -> str:
        tok = m.group(1)
        base = expand_hex(tok)
        if base not in COLOR_MAP:
            return m.group(0)
        new = COLOR_MAP[base]
        hits[base] += 1
        if len(tok) == 8:  # 保留 alpha
            return "#" + new + tok[6:8].lower()
        if len(tok) == 4:  # #rgba
            return "#" + new + tok[3].lower() * 2
        return "#" + new

    def rgb_sub(m: re.Match) -> str:
        r, g, b = int(m.group(1)), int(m.group(2)), int(m.group(3))
        if max(r, g, b) > 255:
            return m.group(0)
        alpha = m.group(4)
        base = rgb_to_hex(r, g, b)
        if base not in COLOR_MAP:
            return m.group(0)
        hits[base] += 1
        nr, ng, nb = hex_to_rgb(COLOR_MAP[base])
        if alpha:
            return "rgb(%d %d %d / %s)" % (nr, ng, nb, alpha)
        return "rgb(%d %d %d)" % (nr, ng, nb)

    text = RGB_RE.sub(rgb_sub, text)   # 先 rgb，避免 hex 规则把 rgb 里的数字看错（不会，但顺序稳定）
    text = HEX_RE.sub(hex_sub, text)
    return text, hits


def scan_residual(root: str, paths: list[str]) -> dict:
    """只读扫描：残留的映射键 / 未映射色值 / 关键色值是否到位"""
    residual: Counter = Counter()
    residual_files: dict[str, list[str]] = {}
    unmapped: Counter = Counter()
    unmapped_files: dict[str, list[str]] = {}

    for rel, full in iter_files(root, paths):
        text = read_text(full)
        if text is None:
            continue
        for m in HEX_RE.finditer(text):
            base = expand_hex(m.group(1))
            if base in COLOR_MAP:
                residual[base] += 1
                residual_files.setdefault(base, []).append(rel)
            elif not is_known(base):
                unmapped[base] += 1
                unmapped_files.setdefault(base, []).append(rel)
        for m in RGB_RE.finditer(text):
            r, g, b = int(m.group(1)), int(m.group(2)), int(m.group(3))
            if max(r, g, b) > 255:
                continue
            base = rgb_to_hex(r, g, b)
            if base in COLOR_MAP:
                residual[base] += 1
                residual_files.setdefault(base, []).append(rel + "(rgb)")
            elif not is_known(base):
                unmapped[base] += 1
                unmapped_files.setdefault(base, []).append(rel + "(rgb)")

    return {"residual": residual, "residual_files": residual_files,
            "unmapped": unmapped, "unmapped_files": unmapped_files}


def load_theme_palette(root: str) -> None:
    """把 theme.less（= theme-final.less 落盘后的结果）里出现的色值并入已知色板。

    这样「未映射告警」只针对**组件里新冒出来的色**，不会把主题自身的几十个色值全喊一遍。
    theme.less 还是上游版本时（没覆盖过），里面的色值是映射键，仍会算「残留」，告警语义不变。
    """
    p = os.path.join(root, "frontend/src/style/theme.less")
    text = read_text(p) if os.path.isfile(p) else None
    if not text:
        return
    for m in HEX_RE.finditer(text):
        base = expand_hex(m.group(1))
        if base not in COLOR_MAP:
            NEW_PALETTE.add(base)
    for m in RGB_RE.finditer(text):
        r, g, b = int(m.group(1)), int(m.group(2)), int(m.group(3))
        if max(r, g, b) <= 255:
            base = rgb_to_hex(r, g, b)
            if base not in COLOR_MAP:
                NEW_PALETTE.add(base)


def is_known(hex6: str) -> bool:
    """已知且无需处理的色值：新色板 / 灰度（纯白纯黑灰阶）。"""
    r, g, b = hex_to_rgb(hex6)
    if hex6 in NEW_PALETTE:
        return True
    if r == g == b:  # #fff / #000 / 任意灰阶：中性色不算未映射
        return True
    return False


KEY_VARS = [
    ("--td-brand-color-7", "6f8ab8"),
    ("--td-brand-color-8", "56729f"),
    ("--td-bg-color-page", "f2f2ee"),
    ("--td-component-border", "aab7c2"),
    ("--app-brand-secondary", "51697c"),
    ("--app-brand-strong", "56729f"),
]


def check_theme_vars(root: str) -> list[tuple[str, str, str | None]]:
    p = os.path.join(root, "frontend/src/style/theme.less")
    if not os.path.isfile(p):
        return [("theme.less", "文件存在", None)]
    text = read_text(p) or ""
    # 只看亮色块（`\n:root.dark,` 之前）—— 文件头部注释里也出现过 ":root.dark" 字面，不能用它直接 split
    light = text.split("\n:root.dark,")[0]
    out = []
    for var, want in KEY_VARS:
        m = re.search(re.escape(var) + r"\s*:\s*([^;\n]+);", light)
        got = (m.group(1).strip().lower() if m else None)
        out.append((var, want, got))
    return out


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(add_help=True, description="组件内硬编码色值替换（雾蓝流金）")
    ap.add_argument("--root", default=None, help="仓库根（默认 git rev-parse --show-toplevel）")
    ap.add_argument("--paths", nargs="*", default=DEFAULT_PATHS, help="扫描范围（相对根）")
    ap.add_argument("--dry-run", action="store_true", help="只打印，不落盘")
    ap.add_argument("--check", action="store_true", help="只检查残留/未映射/关键色值")
    ap.add_argument("--json", default=None, help="把结果写到 json 文件")
    ap.add_argument("-q", "--quiet", action="store_true")
    args = ap.parse_args(argv)

    root = args.root
    if not root:
        try:
            import subprocess
            root = subprocess.check_output(["git", "rev-parse", "--show-toplevel"],
                                           text=True).strip()
        except Exception:
            print("找不到仓库根，请用 --root 指定", file=sys.stderr)
            return 2
    root = os.path.abspath(root)
    load_theme_palette(root)

    GRN, YEL, RED, RST = "\033[32m", "\033[33m", "\033[31m", "\033[0m"
    if not sys.stdout.isatty():
        GRN = YEL = RED = RST = ""

    result: dict = {"root": root, "paths": args.paths, "mode": "check" if args.check else
                    ("dry-run" if args.dry_run else "apply"),
                    "map_entries": len(COLOR_MAP), "snippets": len(SNIPPETS)}

    if args.check:
        res = scan_residual(root, args.paths)
        vars_res = check_theme_vars(root)
        bad = 0
        print("== 残留检查（映射表里的上游色值应为 0）==")
        if res["residual"]:
            bad += 1
            for k, c in res["residual"].most_common():
                print("  %s✘%s #%s ×%d  %s" % (RED, RST, k, c,
                                              ",".join(sorted(set(res["residual_files"][k])))[:110]))
        else:
            print("  %s✓%s 无残留" % (GRN, RST))
        print("== 未映射色值（上游没有的色，需人工判断是否新引入）==")
        if res["unmapped"]:
            for k, c in res["unmapped"].most_common(30):
                print("  %s!%s #%s ×%d  %s" % (YEL, RST, k, c,
                                              ",".join(sorted(set(res["unmapped_files"][k])))[:110]))
        else:
            print("  %s✓%s 无" % (GRN, RST))
        print("== 关键变量（theme.less 亮色块）==")
        for var, want, got in vars_res:
            ok = (got is not None and got.replace("#", "") == want)
            print("  %s%s%s %-24s 期望 #%s 实际 %s" % (GRN if ok else RED, "✓" if ok else "✘",
                                                      RST, var, want, got))
            if not ok:
                bad += 1
        result.update({
            "residual": dict(res["residual"]),
            "unmapped": dict(res["unmapped"]),
            "theme_vars": {v: {"want": w, "got": g} for v, w, g in vars_res},
            "ok": bad == 0,
        })
        if args.json:
            open(args.json, "w", encoding="utf-8").write(
                json.dumps(result, ensure_ascii=False, indent=2))
        print("\n检查结果: %s" % ("通过" if bad == 0 else "有 %d 项问题" % bad))
        return 0 if bad == 0 else 1

    # ---------- apply / dry-run ----------
    files_changed = 0
    total_hits: Counter = Counter()
    snippet_applied, snippet_skipped = [], []
    changed_files = []

    for rel, full in iter_files(root, args.paths):
        text = read_text(full)
        if text is None:
            continue
        orig = text
        text, s_applied, s_skipped = apply_snippets(rel, text)
        for a in s_applied:
            snippet_applied.append({"file": rel, **a})
        for s in s_skipped:
            snippet_skipped.append({"file": rel, **s})
        text, hits = apply_color_map(text)
        total_hits.update(hits)
        if text != orig:
            files_changed += 1
            changed_files.append({"file": rel, "hits": sum(hits.values()),
                                  "snippets": len([a for a in s_applied if a["state"] == "applied"])})
            if not args.dry_run:
                write_text(full, text)

    print("== %s ==" % ("预览（--dry-run，未落盘）" if args.dry_run else "替换结果"))
    print("  映射表条目      : %d 条（%d 组）" % (len(COLOR_MAP), len(COLOR_MAP_GROUPS)))
    print("  片段规则        : %d 条，已应用 %d 条，已存在 %d 条，跳过 %d 条" % (
        len(SNIPPETS),
        len([a for a in snippet_applied if a["state"] == "applied"]),
        len([a for a in snippet_applied if a["state"] == "already"]),
        len(snippet_skipped)))
    print("  改动文件        : %d 个" % files_changed)
    print("  色值替换        : %d 处" % sum(total_hits.values()))
    if total_hits and not args.quiet:
        for k, c in total_hits.most_common():
            print("      #%s → #%s  ×%d   [%s]" % (k, COLOR_MAP[k], c, COLOR_GROUP[k]))
    if snippet_skipped and not args.quiet:
        for s in snippet_skipped:
            print("  %s[skip]%s %s :: %s —— %s" % (YEL, RST, s["file"], s["name"], s["why"]))
    if changed_files and not args.quiet:
        for c in changed_files:
            print("      M %-52s 色值 %2d 处 / 片段 %d 条" % (c["file"], c["hits"], c["snippets"]))

    # 替换后残留检查（dry-run 时检查磁盘现状，价值有限，仍打印提示）
    if not args.dry_run:
        res = scan_residual(root, args.paths)
        print("== 替换后残留检查 ==")
        if res["residual"]:
            for k, c in res["residual"].most_common():
                print("  %s✘%s #%s ×%d  %s" % (RED, RST, k, c,
                                              ",".join(sorted(set(res["residual_files"][k])))[:110]))
        else:
            print("  %s✓%s 映射表内的上游色值已清零" % (GRN, RST))
        if res["unmapped"]:
            print("== %s未映射色值告警%s（Pro 版可能新引入，需人工判断）==" % (YEL, RST))
            for k, c in res["unmapped"].most_common(30):
                print("  %s!%s #%s ×%d  %s" % (YEL, RST, k, c,
                                              ",".join(sorted(set(res["unmapped_files"][k])))[:110]))
        result.update({"residual": dict(res["residual"]), "unmapped": dict(res["unmapped"])})

    result.update({
        "files_changed": files_changed,
        "color_hits": sum(total_hits.values()),
        "hits": dict(total_hits),
        "changed_files": changed_files,
        "snippets_applied": snippet_applied,
        "snippets_skipped": snippet_skipped,
    })
    if args.json:
        open(args.json, "w", encoding="utf-8").write(
            json.dumps(result, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
