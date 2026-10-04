"""Outline a string from a variable woff2 font at given axis values into an SVG path.

PYTHONPATH=<fonttools> python3 outline_text.py <font.woff2> "<text>" wght=600 [opsz=72] [tracking_em=-0.02]
Prints JSON: {"d": path, "width": advance, "ascender":..., "descender":..., "upm":...}
"""
import json, sys
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen

path, text, *rest = sys.argv[1:]
axes, tracking = {}, 0.0
for kv in rest:
    k, v = kv.split("=")
    if k == "tracking_em":
        tracking = float(v)
    else:
        axes[k] = float(v)
font = TTFont(path)
if "fvar" in font:
    font = instantiateVariableFont(font, axes)
upm = font["head"].unitsPerEm
cmap = font.getBestCmap()
gs = font.getGlyphSet()
hmtx = font["hmtx"]
kern = {}
# Simple GPOS pair kerning is skipped; tracking is applied uniformly.
x = 0
pen = SVGPathPen(gs)
for ch in text:
    g = cmap[ord(ch)]
    tp = TransformPen(pen, (1, 0, 0, -1, x, 0))  # flip y for SVG
    gs[g].draw(tp)
    x += hmtx[g][0] + tracking * upm
x -= tracking * upm
os2 = font["OS/2"]
print(json.dumps({"d": pen.getCommands(), "width": x, "ascender": os2.sTypoAscender, "descender": os2.sTypoDescender, "capHeight": getattr(os2, "sCapHeight", 0), "xHeight": getattr(os2, "sxHeight", 0), "upm": upm}))
