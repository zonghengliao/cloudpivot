#!/usr/bin/env python3
"""从 logo.png 生成标准 favicon 图标集（可重复执行，输出确定性字节）。

生成到 frontend/public/：
  · favicon.ico            多尺寸 16/32/48（Safari / Windows 兜底）
  · favicon-32x32.png      高分屏标签页
  · favicon-16x16.png      普屏标签页
  · apple-touch-icon.png   180x180（macOS/iOS 加书签、添加到主屏）

约定（改动前先读）：
  1. 只做等比缩放，绝不拉伸：源必须正方形，非正方形直接报错退出。
  2. 输出保持 RGBA 透明通道（源图四角透明，压平会变成小白边/黑边）。
  3. ICO 用「每尺寸独立 LANCZOS 缩放 + append_images 打包」，而不是交给
     Pillow 从大图内部 resize —— 小尺寸（16px）走 LANCZOS 才不糊。
  4. 不生成 favicon.svg：logo 是双色栅格云涡，没有可用的单色 path，
     mask-icon 要求纯色矢量；描摹（potrace 之类）会改变图形，故不做。
     详见 docs/evidence/20261103-favicon/README.md。

用法：
  python3 scripts/assets/gen_favicons.py                    # 默认源与输出目录
  python3 scripts/assets/gen_favicons.py --check            # 只校验已生成文件，不写盘
  python3 scripts/assets/gen_favicons.py --src X --out-dir Y

依赖：Pillow（项目开发机已装；不加 npm 依赖、不进前端 bundle）。
"""

from __future__ import annotations

import argparse
import hashlib
import sys
from pathlib import Path

from PIL import Image

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SRC = REPO_ROOT / "frontend" / "public" / "logo.png"
DEFAULT_OUT = REPO_ROOT / "frontend" / "public"

ICO_SIZES = (16, 32, 48)
PNG_SIZES = {"favicon-16x16.png": 16, "favicon-32x32.png": 32}
APPLE_TOUCH = ("apple-touch-icon.png", 180)

# 断言的验收门槛（卡面 B 段）：ICO 必须 > 1000 字节，不能是被回退的 602 字节 HTML
MIN_ICO_BYTES = 1000


def md5(path: Path) -> str:
    h = hashlib.md5()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def load_source(src: Path) -> Image.Image:
    if not src.exists():
        sys.exit(f"[FAIL] 源文件不存在: {src}")
    im = Image.open(src)
    if im.width != im.height:
        sys.exit(f"[FAIL] 源图非正方形 {im.size}，等比缩放会变形，拒绝生成")
    rgba = im.convert("RGBA")
    if rgba.size[0] < max(ICO_SIZES + (APPLE_TOUCH[1],)):
        sys.exit(f"[FAIL] 源图 {rgba.size} 小于要生成的尺寸，会被放大糊掉")
    return rgba


def resize(im: Image.Image, size: int) -> Image.Image:
    """等比缩到正方形目标尺寸，保留 alpha。"""
    return im.resize((size, size), Image.Resampling.LANCZOS)


def ico_sizes(im: Image.Image) -> list[tuple[int, int]]:
    """取 ICO 内所有帧尺寸（Pillow 把 ico 插件挂在 ImageFile 上，类型标注里没有）。"""
    plugin = getattr(im, "ico", None)
    if plugin is None:
        return []
    return sorted(plugin.sizes())



def build(src: Path, out_dir: Path) -> dict[str, Path]:
    logo = load_source(src)
    out_dir.mkdir(parents=True, exist_ok=True)
    written: dict[str, Path] = {}

    # 1) 单尺寸 PNG
    for name, size in PNG_SIZES.items():
        path = out_dir / name
        resize(logo, size).save(path, format="PNG", optimize=True)
        written[name] = path

    # 2) apple-touch-icon（180，RGBA 保持透明）
    name, size = APPLE_TOUCH
    path = out_dir / name
    resize(logo, size).save(path, format="PNG", optimize=True)
    written[name] = path

    # 3) 多尺寸 ICO：每尺寸独立 LANCZOS，再把小的作为 append_images 一起打包
    frames = [resize(logo, s) for s in ICO_SIZES]
    ico_path = out_dir / "favicon.ico"
    base = frames[-1]  # 最大的当底图
    base.save(
        ico_path,
        format="ICO",
        sizes=[f.size for f in frames],
        append_images=frames[:-1],
    )
    written["favicon.ico"] = ico_path
    return written


def verify(out_dir: Path) -> bool:
    """回读校验：ICO 内含 16/32/48 且每帧可解码；PNG 尺寸/模式正确。"""
    ok = True
    ico_path = out_dir / "favicon.ico"
    if not ico_path.exists():
        print(f"[FAIL] 缺少 {ico_path}")
        return False
    with Image.open(ico_path) as ico:
        sizes = ico_sizes(ico)
        print(f"  favicon.ico 内含尺寸: {['%dx%d' % s for s in sizes]}")
        missing = [s for s in ICO_SIZES if (s, s) not in sizes]
        if missing:
            print(f"[FAIL] favicon.ico 缺尺寸: {missing}")
            ok = False
        for s in sizes:
            frame = getattr(ico, "ico").getimage(s)
            if frame.size != s:
                print(f"[FAIL] ICO 帧 {s} 回读尺寸不符 {frame.size}")
                ok = False
    if ico_path.stat().st_size <= MIN_ICO_BYTES:
        print(
            f"[FAIL] favicon.ico 仅 {ico_path.stat().st_size} 字节 "
            f"(<= {MIN_ICO_BYTES}，疑似不是真图标)"
        )
        ok = False

    expect = {**PNG_SIZES, APPLE_TOUCH[0]: APPLE_TOUCH[1]}
    for name, size in expect.items():
        p = out_dir / name
        if not p.exists():
            print(f"[FAIL] 缺少 {p}")
            ok = False
            continue
        with Image.open(p) as im:
            if im.size != (size, size):
                print(f"[FAIL] {name} 尺寸 {im.size} != {(size, size)}")
                ok = False
            if im.mode != "RGBA":
                print(f"[FAIL] {name} 模式 {im.mode} != RGBA")
                ok = False
            if im.convert("RGBA").getchannel("A").getextrema()[0] != 0:
                print(f"[WARN] {name} 没有全透明像素（源图四角本应透明）")
    return ok


def report(out_dir: Path) -> None:
    print("\n生成结果（尺寸 / 模式 / 字节数 / md5）:")
    names = ["favicon.ico", "favicon-32x32.png", "favicon-16x16.png", "apple-touch-icon.png"]
    for name in names:
        p = out_dir / name
        with Image.open(p) as im:
            dims = im.size
            mode = im.mode
            extra = ""
            if name.endswith(".ico"):
                extra = f"  帧={['%dx%d' % s for s in ico_sizes(im)]}"
            print(
                f"  {name:<22} {dims[0]}x{dims[1]:<5} {mode:<5} "
                f"{p.stat().st_size:>7} bytes  md5={md5(p)}{extra}"
            )
    print(f"\n输出目录: {out_dir}")


def main() -> int:
    ap = argparse.ArgumentParser(description="生成 favicon 图标集（从 logo.png 等比缩放）")
    ap.add_argument("--src", type=Path, default=DEFAULT_SRC)
    ap.add_argument("--out-dir", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--check", action="store_true", help="只校验，不重新生成")
    args = ap.parse_args()

    if not args.check:
        print(f"源: {args.src}")
        build(args.src, args.out_dir)
    ok = verify(args.out_dir)
    report(args.out_dir)
    print("\n校验:", "PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
