# The brand: logo.svg (the lockup, for the README), docs/assets/logo.svg (the mark, for the site
# header), docs/assets/favicon.svg and docs/assets/og.png (the 1200x630 social card), composed
# from the text outlines in outlines/. Run from the repo root (og.png needs rsvg-convert):
#   elixir scripts/brand/logo.exs
# To change a text, outline it again with Inter (https://github.com/rsms/inter, OFL) next to
# outline.py, e.g. `uv run --with fonttools python3 scripts/brand/outline.py 800 76 "Authorization," -0.04
# > scripts/brand/outlines/og_h1.json`: fontTools has no Elixir counterpart.
dir = "scripts/brand/outlines"
d = fn name -> File.read!(Path.join(dir, name <> ".json")) |> JSON.decode!() |> Map.fetch!("d") end

# One isometric slab, its top face's left corner at (x, y0): w wide, faces h thick.
{w, half, h} = {120, 30, 26}
slab = fn x, y0, {top, left, right} ->
  m = x + div(w, 2)
  r = x + w
  """
      <path d="M#{x} #{y0} L#{m} #{y0 + half} L#{m} #{y0 + half + h} L#{x} #{y0 + h} Z" fill="#{left}"/>
      <path d="M#{m} #{y0 + half} L#{r} #{y0} L#{r} #{y0 + h} L#{m} #{y0 + half + h} Z" fill="#{right}"/>
      <path d="M#{x} #{y0} L#{m} #{y0 - half} L#{r} #{y0} L#{m} #{y0 + half} Z" fill="#{top}"/>
  """
end

# The mark: a table of three row-slabs. The middle row, one the subject may not see, is pulled
# out of the stack toward the viewer and greyed; its empty place is outlined.
{top_y, step, pull_x, pull_y} = {223, 34, -32, 16}
mid_y = top_y + step
bottom_y = top_y + 2 * step
m = div(w, 2)
mark = """
#{slab.(0, bottom_y, {"#3ECF8E", "#249361", "#15553A"})}#{slab.(0, top_y, {"#4AE29E", "#2FA873", "#1F7A52"})}    <path d="M0 #{top_y} L#{m} #{top_y - half} L#{w} #{top_y}" fill="none" stroke="#7DF0BC" stroke-width="1.6" stroke-linejoin="round"/>
    <path d="M0 #{mid_y} L#{m} #{mid_y + half} L#{w} #{mid_y} M0 #{mid_y} L0 #{mid_y + h} L#{m} #{mid_y + half + h} L#{w} #{mid_y + h} L#{w} #{mid_y}" fill="none" stroke="#3ECF8E" stroke-opacity="0.7" stroke-width="1.6" stroke-dasharray="4 4" stroke-linejoin="round"/>
#{slab.(pull_x, mid_y + pull_y, {"#5C5C5C", "#3E3E3E", "#2B2B2B"})}    <path d="M#{pull_x} #{mid_y + pull_y} L#{pull_x + m} #{mid_y + pull_y - half} L#{pull_x + w} #{mid_y + pull_y}" fill="none" stroke="#8A8A8A" stroke-width="1.2" stroke-linejoin="round"/>
"""

logo = """
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 520">
  <!-- pg_auth. A table of three isometric row-slabs: the middle row, one the subject may not
       see, is pulled out of the stack and greyed, its place outlined. The wordmark is Inter
       ExtraBold outlined to paths, so it renders the same wherever the SVG is shown. -->
  <defs>
    <radialGradient id="glow" cx="50%" cy="50%" r="50%">
      <stop offset="0%" stop-color="#3ECF8E" stop-opacity="0.2"/>
      <stop offset="100%" stop-color="#3ECF8E" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="800" height="520" fill="#1F1F1F"/>
  <circle cx="175" cy="270" r="180" fill="url(#glow)"/>
  <g transform="translate(131 0)">
#{mark}  </g>
  <g transform="translate(300 292)">
    <path d="#{d.("pg")}" fill="#FFFFFF"/>
    <rect x="133" y="3" width="44" height="11" rx="2" fill="#3ECF8E"/>
    <path transform="translate(184 0)" d="#{d.("auth")}" fill="#FFFFFF"/>
  </g>
  <path transform="translate(302 338)" d="#{d.("tag")}" fill="#8F8F8F"/>
  <rect x="302" y="356" width="60" height="3" fill="#3ECF8E"/>
</svg>
"""

# The header logo: the mark alone, cropped to it.
header = """
<svg xmlns="http://www.w3.org/2000/svg" viewBox="-34 191 156 158">
  <!-- pg_auth mark: three row-slabs, the one the subject may not see pulled out and greyed -->
#{mark}</svg>
"""

# The 1200x630 social card, text outlined like the wordmark.
og = """
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 630">
  <defs>
    <radialGradient id="g1" cx="160" cy="120" r="700" gradientUnits="userSpaceOnUse">
      <stop offset="0%" stop-color="#3ECF8E" stop-opacity="0.18"/>
      <stop offset="70%" stop-color="#3ECF8E" stop-opacity="0"/>
    </radialGradient>
    <radialGradient id="g2" cx="1000" cy="330" r="380" gradientUnits="userSpaceOnUse">
      <stop offset="0%" stop-color="#3ECF8E" stop-opacity="0.16"/>
      <stop offset="100%" stop-color="#3ECF8E" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="1200" height="630" fill="#171717"/>
  <rect width="1200" height="630" fill="url(#g1)"/>
  <rect width="1200" height="630" fill="url(#g2)"/>
  <g transform="translate(91.5 -9.5) scale(0.36)">
#{mark}  </g>
  <g transform="translate(150 104)">
    <path d="#{d.("og_pg")}" fill="#FFFFFF"/>
    <rect x="48.5" y="1" width="16" height="4" rx="1" fill="#3ECF8E"/>
    <path transform="translate(67 0)" d="#{d.("og_auth")}" fill="#FFFFFF"/>
  </g>
  <path transform="translate(80 250)" d="#{d.("og_h1")}" fill="#FFFFFF"/>
  <path transform="translate(80 334)" d="#{d.("og_h2")}" fill="#FFFFFF"/>
  <path transform="translate(80 400)" d="#{d.("og_l1")}" fill="#A8A8A8"/>
  <path transform="translate(80 438)" d="#{d.("og_l2")}" fill="#A8A8A8"/>
  <path transform="translate(80 566)" d="#{d.("og_foot")}" fill="#D0D0D0"/>
  <path transform="translate(866 566)" d="#{d.("og_url")}" fill="#6F6F6F"/>
  <g transform="translate(931 -138) scale(1.6)">
#{mark}  </g>
</svg>
"""

# the mark spans x -32..120 and y 193..349 in its own frame: centred in the 256 tile
favicon = """
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">
  <rect width="256" height="256" rx="56" fill="#1F1F1F"/>
  <g transform="translate(76.6 -241.3) scale(1.3)">
#{mark}  </g>
</svg>
"""

File.write!("logo.svg", String.trim_leading(logo))
File.write!("docs/assets/logo.svg", String.trim_leading(header))
File.write!("docs/assets/favicon.svg", String.trim_leading(favicon))
og_svg = Path.join(System.tmp_dir!(), "pg_auth_og.svg")
File.write!(og_svg, String.trim_leading(og))
{_, 0} = System.cmd("rsvg-convert", ["-w", "1200", "-h", "630", og_svg, "-o", "docs/assets/og.png"])
File.rm!(og_svg)
IO.puts("wrote logo.svg, docs/assets/logo.svg, favicon.svg and og.png")
