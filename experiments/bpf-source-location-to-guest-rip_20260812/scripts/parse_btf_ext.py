#!/usr/bin/env python3
"""Parse .BTF and .BTF.ext from a BPF ELF object and print line_info records.

Layout per Documentation/bpf/btf.rst ("The .BTF.ext section") and
tools/lib/bpf/libbpf_internal.h (struct btf_ext_header / bpf_line_info_min).
"""
import struct, subprocess, sys

obj = sys.argv[1]
limit = int(sys.argv[2]) if len(sys.argv) > 2 else 20

def section(name):
    out = subprocess.run(["llvm-objcopy", "--dump-section", f"{name}=/dev/stdout", obj, "/dev/null"],
                         capture_output=True)
    if out.returncode != 0:
        return None
    return out.stdout

btf = section(".BTF")
ext = section(".BTF.ext")
if not ext:
    print("NO .BTF.ext SECTION"); sys.exit(1)

# --- BTF string table (for file_name_off / line_off) ---
# struct btf_header: magic(u16) version(u8) flags(u8) hdr_len(u32)
#   type_off(u32) type_len(u32) str_off(u32) str_len(u32)
magic, ver, flags, hdr_len = struct.unpack_from("<HBBI", btf, 0)
type_off, type_len, str_off, str_len = struct.unpack_from("<IIII", btf, 8)
strs = btf[hdr_len + str_off : hdr_len + str_off + str_len]
def s(off):
    end = strs.index(b"\0", off)
    return strs[off:end].decode()

print(f".BTF: magic=0x{magic:x} version={ver} hdr_len={hdr_len} type_len={type_len} str_len={str_len}")

# --- BTF.ext header ---
emagic, ever, eflags, ehdr_len = struct.unpack_from("<HBBI", ext, 0)
func_off, func_len, line_off, line_len = struct.unpack_from("<IIII", ext, 8)
print(f".BTF.ext: magic=0x{emagic:x} version={ever} hdr_len={ehdr_len} "
      f"func_info_len={func_len} line_info_len={line_len}", end="")
if ehdr_len >= 32:
    core_off, core_len = struct.unpack_from("<II", ext, 24)
    print(f" core_relo_len={core_len}")
else:
    print()

base = ehdr_len
# line_info sub-section: u32 rec_size, then per-section:
#   u32 sec_name_off, u32 num_info, then num_info * rec_size records
p = base + line_off
rec_size = struct.unpack_from("<I", ext, p)[0]
p += 4
end = base + line_off + line_len
print(f"line_info rec_size={rec_size}")
total = 0
shown = 0
while p < end:
    sec_name_off, num_info = struct.unpack_from("<II", ext, p)
    p += 8
    print(f"\n  section '{s(sec_name_off)}': {num_info} line_info records")
    for i in range(num_info):
        insn_off, file_name_off, line_off_, line_col = struct.unpack_from("<IIII", ext, p)
        p += rec_size
        total += 1
        if shown < limit:
            print(f"    insn_off={insn_off:5d}  {s(file_name_off)}:"
                  f"{line_col >> 10}:{line_col & 0x3ff}   |{s(line_off_).strip()}")
            shown += 1
print(f"\nTOTAL line_info records: {total}")
