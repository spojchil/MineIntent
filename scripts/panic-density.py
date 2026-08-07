import re, glob, os, sys

MACRO = re.compile(r'\b(panic!|unreachable!|unimplemented!|todo!)\s*\(')
UNWRAP = re.compile(r'\.\s*(unwrap|expect)\s*\(')
ASSERT = re.compile(r'\b(assert|assert_eq|assert_ne|debug_assert|debug_assert_eq|debug_assert_ne)!\s*\(')

def is_test_path(p):
    """路径里任一层带 test 就算测试文件（tests/、*_tests/、*_tests.rs、tests.rs）。"""
    parts = p.replace("\\", "/").split("/")
    return any("test" in seg for seg in parts[:-1]) or "test" in os.path.basename(p)

def test_spans(text):
    """只把 `#[cfg(test)]` 紧跟 `mod ...` 的情形当作测试模块区间。

    facade.rs 里有大量加在结构体字段上的 `#[cfg(test)] scripted: bool,`——
    若把它们也当区间开头，会吞掉整片正常代码，两侧计数都会失真。
    """
    lines = text.split("\n"); spans = []; i = 0
    while i < len(lines):
        if re.match(r'\s*#\[cfg\(test\)\]\s*$', lines[i]):
            k = i + 1
            while k < len(lines) and not lines[k].strip():
                k += 1
            if k < len(lines) and re.match(r'\s*(pub\s+)?mod\s+\w+', lines[k]):
                j = k; depth = 0; started = False
                while j < len(lines):
                    depth += lines[j].count("{") - lines[j].count("}")
                    if "{" in lines[j]: started = True
                    if started and depth <= 0: break
                    j += 1
                spans.append((i, j)); i = j + 1; continue
        i += 1
    return spans

def scan(files):
    loc = mac = unw = asr = 0
    for f in files:
        if is_test_path(f): continue
        try: text = open(f, encoding="utf-8", errors="replace").read()
        except Exception: continue
        spans = test_spans(text)
        for n, line in enumerate(text.split("\n")):
            if any(a <= n <= b for a, b in spans): continue
            s = line.strip()
            if not s or s.startswith("//"): continue
            loc += 1
            if MACRO.search(line): mac += 1
            if UNWRAP.search(line): unw += 1
            if ASSERT.search(line): asr += 1
    return loc, mac, unw, asr

def fmt(name, loc, mac, unw, asr):
    k = (loc / 1000) or 1
    return (f"{name:26} {loc:>8} {mac:>6} {mac/k:>7.2f} {unw:>7} {unw/k:>7.2f} {asr:>7} {asr/k:>7.2f}")

HDR = (f"{'目标':26} {'生产行数':>8} {'宏panic':>6} {'/KLOC':>7} "
       f"{'unwrap':>7} {'/KLOC':>7} {'assert':>7} {'/KLOC':>7}")

mode = sys.argv[1]
print(HDR); print("-" * 88)
if mode == "ours":
    tot = [0, 0, 0, 0]
    for c in ["contracts", "backend", "middle", "toolloop", "app"]:
        fs = glob.glob(f"crates/{c}/src/**/*.rs", recursive=True)
        if not fs: continue
        r = scan(fs); tot = [a + b for a, b in zip(tot, r)]
        print(fmt(f"mineintent-{c}", *r))
    print("-" * 88); print(fmt("我们合计", *tot))
else:
    REG = os.path.expanduser("~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f")
    runtime = set(open(sys.argv[2]).read().split())
    rows = []; tot = [0, 0, 0, 0]
    for d in sorted(glob.glob(f"{REG}/*/")):
        name = re.sub(r'-\d+\.\d+.*$', '', os.path.basename(d.rstrip("/")))
        if name not in runtime: continue
        fs = glob.glob(f"{d}src/**/*.rs", recursive=True)
        if not fs: continue
        r = scan(fs); tot = [a + b for a, b in zip(tot, r)]
        rows.append((name, r))
    for name, r in sorted(rows, key=lambda x: -x[1][1])[:10]:
        print(fmt(name, *r))
    print("-" * 88); print(fmt(f"{len(rows)} 个运行期依赖合计", *tot))
