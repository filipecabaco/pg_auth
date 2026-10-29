# Outline text to an SVG path with Inter at a given weight (fontTools: no Elixir library reads
# fonts). Usage: outline.py <weight> <size> <text>  ->  JSON {"d": ..., "width": ...}
import json
import sys

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

weight, size, text = int(sys.argv[1]), float(sys.argv[2]), sys.argv[3]
tracking = float(sys.argv[4]) if len(sys.argv) > 4 else 0.0  # em
# inter.ttf (the variable font) sits next to this file and is not committed: see logo.exs
font = instancer.instantiateVariableFont(TTFont(__file__.rsplit("/", 1)[0] + "/inter.ttf"), {"wght": weight, "opsz": 32})
glyphs = font.getGlyphSet()
cmap = font.getBestCmap()
scale = size / font["head"].unitsPerEm
pen = SVGPathPen(glyphs, ntos=lambda v: ("%.2f" % v).rstrip("0").rstrip("."))
x = 0.0
for ch in text:
    name = cmap[ord(ch)]
    # y down in SVG: flip, baseline at 0
    glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, x, 0)))
    x += glyphs[name].width * scale + tracking * size
print(json.dumps({"d": pen.getCommands(), "width": round(x, 2)}))
