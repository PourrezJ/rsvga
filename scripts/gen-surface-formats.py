import re, sys
types, defs, out = sys.argv[1], sys.argv[2], sys.argv[3]
src = open(types).read()
enum = src[src.index("typedef enum SVGA3dSurfaceFormat"):]
enum = enum[:enum.index("} SVGA3dSurfaceFormat;")]
values, cur = {}, -1
for line in enum.splitlines():
    m = re.match(r"\s*(SVGA3D_\w+)\s*(?:=\s*([^,]+))?,", line)
    if not m:
        continue
    name, val = m.groups()
    if val is None:
        cur += 1
    else:
        val = val.strip()
        cur = int(val, 0) if re.fullmatch(r"(0x)?[0-9a-fA-F]+", val) else values[val]
    values.setdefault(name, cur)
d = open(defs).read()
d = d[d.index("g_SVGA3dSurfaceDescs[] = {"):]
rows = []
for m in re.finditer(r"\{\s*(SVGA3D_\w+),\s*(SVGA3DBLOCKDESC_\w+(?:\s*\|\s*SVGA3DBLOCKDESC_\w+)*)?,?\s*\{\s*(\d+),\s*(\d+),\s*(\d+)\s*\},\s*(\d+),\s*(\d+),", d):
    name, _, bw, bh, bd, bpb, pbpb = m.groups()
    if name == "SVGA3D_FORMAT_INVALID":
        continue
    rows.append((values[name], name, int(bw), int(bh), int(bd), int(bpb), int(pbpb)))
rows.sort()
with open(out, "w") as f:
    f.write("//! Surface formats (`SVGA3dSurfaceFormat`) and their block layout.\n//!\n")
    f.write("//! Generated from `g_SVGA3dSurfaceDescs` in the VMware SVGA headers\n")
    f.write("//! (`svga3d_surfacedefs.h`, MIT license); do not edit by hand.\n\n")
    f.write("/// How a format stores its pixels: in blocks of `block_*` pixels\n")
    f.write("/// (1x1x1 for uncompressed formats), each `bytes_per_block` long.\n")
    f.write("#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub struct FormatDesc {\n")
    for field, doc in [("block_width","Block width in pixels."),("block_height","Block height in pixels."),("block_depth","Block depth in pixels."),("bytes_per_block","Bytes per block."),("pitch_bytes_per_block","Bytes per block in a row pitch (differs for planar YUV formats).")]:
        f.write(f"    /// {doc}\n    pub {field}: u32,\n")
    f.write("}\n\n")
    for v, name, *_ in rows:
        f.write(f"#[allow(missing_docs)]\npub const {name[len("SVGA3D_"):].upper()}: u32 = {v};\n")
    f.write("\n/// Layout of `format`, or `None` for an unknown format.\npub const fn desc(format: u32) -> Option<FormatDesc> {\n")
    f.write("    let (block_width, block_height, block_depth, bytes_per_block, pitch_bytes_per_block) = match format {\n")
    for v, name, bw, bh, bd, bpb, pbpb in rows:
        f.write(f"        {name[len("SVGA3D_"):].upper()} => ({bw}, {bh}, {bd}, {bpb}, {pbpb}),\n")
    f.write("        _ => return None,\n    };\n")
    f.write("    Some(FormatDesc {\n        block_width,\n        block_height,\n        block_depth,\n        bytes_per_block,\n        pitch_bytes_per_block,\n    })\n}\n")
print(len(rows), "formats")
