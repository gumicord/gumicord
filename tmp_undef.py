import struct
import zipfile

with zipfile.ZipFile(r"C:\Users\nennneko5787\AppData\Local\Temp\apkcheck\app.apk") as z:
    raw = z.read("lib/arm64-v8a/libmain.so")

(e_shoff,) = struct.unpack_from("<Q", raw, 0x28)
(e_shentsize, e_shnum, e_shstrndx) = struct.unpack_from("<HHH", raw, 0x3A)
secs = []
for i in range(e_shnum):
    n, stype, flags, addr, off, size, link, info, align, entsize = struct.unpack_from(
        "<IIQQQQIIQQ", raw, e_shoff + i * e_shentsize
    )
    secs.append((n, stype, off, size, link, entsize))
shstr = secs[e_shstrndx]
table = raw[shstr[2] : shstr[2] + shstr[3]]


def sn(o):
    return table[o : table.index(b"\x00", o)].decode()


for n, stype, off, size, link, entsize in secs:
    if sn(n) != ".dynsym":
        continue
    ssec = secs[link]
    s_off = ssec[2]
    print(f"dynsym entries: {size // entsize}")
    undef = []
    for k in range(size // entsize):
        st_name, st_info, st_other, st_shndx, st_val, st_size = struct.unpack_from(
            "<IBBHQQ", raw, off + k * entsize
        )
        if st_info & 0xF == 2 and st_shndx == 0:  # FUNC, undefined
            s = raw[s_off + st_name : raw.index(b"\x00", s_off + st_name)].decode(
                errors="replace"
            )
            undef.append(s)
    print(f"undefined funcs: {len(undef)}")
    keywords = ["GameActivity", "game_activity", "android_main", "NativeCode", "native_app",
                "ANativeActivity", "AMotion", "ALooper", "AInput", "EGL", "GLES"]
    for s in undef:
        if any(k.lower() in s.lower() for k in keywords):
            print("   ", s)
