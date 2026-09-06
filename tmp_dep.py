import re

lock = open("Cargo.lock", encoding="utf-8").read()
blocks = lock.split("[[package]]")
for b in blocks:
    name = re.search(r'name = "([^"]+)"', b)
    ver = re.search(r'version = "([^"]+)"', b)
    if not name:
        continue
    if "rustls-platform-verifier" in b and "dependencies" in b:
        print("USER:", name.group(1), ver.group(1) if ver else "")
    if name.group(1) == "rustls-platform-verifier":
        print("VERSION:", ver.group(1) if ver else "")
        m = re.search(r"dependencies = \[(.*?)\]", b, re.S)
        if m:
            print("DEPS:", m.group(1)[:400])
