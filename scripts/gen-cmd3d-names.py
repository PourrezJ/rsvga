#!/usr/bin/env python3
"""Generates svga-proto/src/cmd3d_names.rs: the names of the 3D command IDs,
from the SVGAFifo3dCmdId enum of svga3d_cmd.h (VMware SVGA headers, MIT).
usage: gen-cmd3d-names.py svga3d_cmd.h OUT.rs"""
import re, sys
src = open(sys.argv[1]).read()
body = src[src.index("typedef enum SVGAFifo3dCmdId {"):]
body = body[:body.index("} SVGAFifo3dCmdId;")]
names, cur = {}, -1
for line in body.splitlines():
    m = re.match(r"\s*SVGA_3D_CMD_(\w+)\s*(?:=\s*([^,]+))?,", line)
    if not m:
        continue
    name, val = m.groups()
    if val is None:
        cur += 1
    else:
        val = val.strip()
        cur = int(val, 0) if re.fullmatch(r"(0x)?[0-9a-fA-F]+", val) else names[val.replace("SVGA_3D_CMD_", "")]
    names[name] = cur
by_id = {}
for name, value in names.items():
    if name in ("LEGACY_BASE", "BASE", "MAX", "FUTURE_MAX") or name.startswith("DEAD") \
            or name.endswith(("_MIN", "_MAX")):
        continue
    by_id.setdefault(value, name)
with open(sys.argv[2], "w") as f:
    f.write("//! Names of 3D command IDs, for traces.\n//!\n")
    f.write("//! Generated from `SVGAFifo3dCmdId` in the VMware SVGA headers\n")
    f.write("//! (`svga3d_cmd.h`, MIT license) by scripts/gen-cmd3d-names.py.\n\n")
    f.write("/// Name of 3D command `id` (without the `SVGA_3D_CMD_` prefix).\n")
    f.write("pub const fn name(id: u32) -> Option<&'static str> {\n    Some(match id {\n")
    for value in sorted(by_id):
        f.write(f'        {value} => "{by_id[value]}",\n')
    f.write("        _ => return None,\n    })\n}\n")
print(len(by_id), "names")
