#!/usr/bin/env python3
"""Draw Nodo's icon and write every size the app and the bundles need.

Usage: python3 script/make-icons.py   (needs Pillow; macOS also runs iconutil for the .icns)
Writes crates/app/assets/logo/{nodo-icon.svg, nodo-icon-1024.png, nodo-icon-512.png,
AppIcon.icns, AppIcon.ico} and crates/app/assets/icons/nodo-logo.svg (the in-app mark).
The motif: a hub with five devices around it, two of them linked, one lit in amber.
"""
import math, pathlib, shutil, subprocess, tempfile
from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/app/assets/logo"
TOP, BOTTOM = (0x22, 0x93, 0xA6), (0x17, 0x47, 0x7A)
CREAM, AMBER, INK = (0xF6, 0xF3, 0xEC), (0xF2, 0xA9, 0x3B), (0x18, 0x55, 0x80)
C = (512, 532)
RING = 262
ANGLES = [-90, -18, 54, 126, 198]
NODES = [(C[0] + RING * math.cos(math.radians(a)), C[1] + RING * math.sin(math.radians(a))) for a in ANGLES]
LIT = 1
LINKS = [(1, 2), (3, 4)]


def svg(mark=False):
    p = []
    if not mark:
        p.append('<defs><linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">'
                 f'<stop offset="0" stop-color="#{TOP[0]:02X}{TOP[1]:02X}{TOP[2]:02X}"/>'
                 f'<stop offset="1" stop-color="#{BOTTOM[0]:02X}{BOTTOM[1]:02X}{BOTTOM[2]:02X}"/></linearGradient></defs>')
        p.append('<rect x="100" y="100" width="824" height="824" rx="185" fill="url(#bg)"/>')
    col = lambda c: "#000" if mark else f"#{c[0]:02X}{c[1]:02X}{c[2]:02X}"
    for a, b in LINKS:
        (x1, y1), (x2, y2) = NODES[a], NODES[b]
        p.append(f'<path d="M{x1:.0f} {y1:.0f} L{x2:.0f} {y2:.0f}" stroke="{col(CREAM)}" stroke-opacity="{1 if mark else 0.55}" stroke-width="22" stroke-linecap="round"/>')
    for x, y in NODES:
        p.append(f'<path d="M{C[0]} {C[1]} L{x:.0f} {y:.0f}" stroke="{col(CREAM)}" stroke-width="{40 if mark else 32}" stroke-linecap="round"/>')
    p.append(f'<circle cx="{C[0]}" cy="{C[1]}" r="96" fill="{col(CREAM)}"/>')
    if not mark:
        p.append(f'<circle cx="{C[0]}" cy="{C[1]}" r="38" fill="{col(INK)}"/>')
    for i, (x, y) in enumerate(NODES):
        p.append(f'<circle cx="{x:.0f}" cy="{y:.0f}" r="{70 if mark else 58}" fill="{col(AMBER if i == LIT else CREAM)}"/>')
    if mark:
        return f'<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="170 190 684 650">' + "".join(p) + "</svg>\n"
    return '<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">' + "".join(p) + "</svg>\n"


def draw(size=1024, ss=4):
    S = size * ss
    k = S / 1024
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    grad = Image.new("RGBA", (S, S))
    gd = ImageDraw.Draw(grad)
    for y in range(S):
        t = min(max((y / k - 100) / 824, 0), 1)
        gd.line([(0, y), (S, y)], fill=tuple(int(TOP[i] + (BOTTOM[i] - TOP[i]) * t) for i in range(3)) + (255,))
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle([100 * k, 100 * k, 924 * k, 924 * k], radius=185 * k, fill=255)
    img.paste(grad, (0, 0), mask)
    over = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    od = ImageDraw.Draw(over)
    for a, b in LINKS:
        (x1, y1), (x2, y2) = NODES[a], NODES[b]
        od.line([(x1 * k, y1 * k), (x2 * k, y2 * k)], fill=CREAM + (140,), width=int(22 * k))
    img = Image.alpha_composite(img, over)
    d = ImageDraw.Draw(img)
    def disc(x, y, r, c):
        d.ellipse([(x - r) * k, (y - r) * k, (x + r) * k, (y + r) * k], fill=c + (255,))
    for x, y in NODES:
        d.line([(C[0] * k, C[1] * k), (x * k, y * k)], fill=CREAM + (255,), width=int(32 * k))
        disc(x, y, 16, CREAM)  # round the line ends under the node
    disc(C[0], C[1], 96, CREAM)
    disc(C[0], C[1], 38, INK)
    for i, (x, y) in enumerate(NODES):
        disc(x, y, 58, AMBER if i == LIT else CREAM)
    return img.resize((size, size), Image.LANCZOS)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "nodo-icon.svg").write_text(svg())
    (ROOT / "crates/app/assets/icons/nodo-logo.svg").write_text(svg(mark=True))
    big = draw(1024)
    big.save(OUT / "nodo-icon-1024.png")
    big.resize((512, 512), Image.LANCZOS).save(OUT / "nodo-icon-512.png")
    big.save(OUT / "AppIcon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    if shutil.which("iconutil"):
        with tempfile.TemporaryDirectory() as t:
            iconset = pathlib.Path(t) / "AppIcon.iconset"
            iconset.mkdir()
            for s in [16, 32, 128, 256, 512]:
                big.resize((s, s), Image.LANCZOS).save(iconset / f"icon_{s}x{s}.png")
                big.resize((s * 2, s * 2), Image.LANCZOS).save(iconset / f"icon_{s}x{s}@2x.png")
            subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(OUT / "AppIcon.icns")], check=True)
    print("icons written to", OUT)


if __name__ == "__main__":
    main()
