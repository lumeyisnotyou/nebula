#!/usr/bin/env python3
"""Nebula's pipeline on this computer, in demo mode (no camera): for ideas, before the real camera.

The real app runs with L16_DEMO=1 (a drawn scene for the preview; no driver), driven through its test
command file, and what it asks of the driver is read from its log. So the app's own logic is exercised
end to end - every mode and lens, the holds of the camera, bursts, the timer, long exposures - and the
screen is captured for each. What it can't show is the camera itself (the driver, the transfers, the
LRI and its rendering): tools/pipeline-test.py in l16-linux does that on the L16.

  checks   cargo test (the unit tests)
  shots    each mode at each lens: the lens set (the module mask), the flags, the ISO and exposure asked for
  rotation each hold: the orientation the photo is tagged with (none, half a turn, a quarter either way)
           and a screenshot of the UI for each (they must differ; look at them)
  burst    three photos from one press
  timer    the 3 s timer waits about 3 s
  long     a 15 s exposure asks for 15 s
  ui       the screen is drawn (not blank) in every mode

usage: tools/pipeline-demo.py [--only case,case] [--no-build] [--keep] [--out DIR]
Screenshots and the app's log go to --out (default ./pipeline-demo-out). The app's settings and runtime
files are in a temporary directory: the real ones are not touched.
"""
import argparse
import hashlib
import math
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LENSES = {28: 0x7FE, 35: 0x7FE, 70: 0x1FFC0, 150: 0x1F800}  # camera n is bit n + 1: A1-B5, B1-C6, C1-C6
MODES = ["auto", "iso", "shutter", "manual"]
QUICK = 3  # NO_PRECAPTURE | NO_STACK: what every shot without a dark scene in auto asks for


def iso_at(pos):
    return round(3200 * (100 / 3200) ** pos)


def secs_at(pos):
    return 15 * (1 / 8000 / 15) ** pos


class App:
    def __init__(self, out, keep):
        self.out = out
        self.tmp = tempfile.mkdtemp(prefix="nebula-demo-")
        self.keep = keep
        self.run = os.path.join(self.tmp, "run")
        os.makedirs(self.run)
        for d in ("config", "cache", "pictures"):
            os.makedirs(os.path.join(self.tmp, d))
        self.logpath = os.path.join(out, "app.log")
        self.proc = None
        self.mark = 0

    def start(self):
        env = dict(os.environ, L16_DEMO="1", XDG_RUNTIME_DIR=self.run, XDG_CONFIG_HOME=os.path.join(self.tmp, "config"),
                   XDG_CACHE_HOME=os.path.join(self.tmp, "cache"), L16_DEMO_OUT=os.path.join(self.tmp, "pictures"),
                   XDG_PICTURES_DIR=os.path.join(self.tmp, "pictures"), L16_DEMO_VIEW="")
        env.pop("L16_DEMO_VIEW")
        self.log = open(self.logpath, "w")
        self.proc = subprocess.Popen([os.path.join(ROOT, "target", "debug", "nebula")], env=env, stderr=self.log, stdout=self.log)
        # ready: the window has been made and the app polls
        return self.wait(r"nebula: activated", 30) is not None

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(8)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if not self.keep:
            shutil.rmtree(self.tmp, ignore_errors=True)

    def text(self):
        self.log.flush()
        with open(self.logpath, errors="replace") as f:
            return f.read()

    def lines_since_mark(self):
        return self.text().splitlines()[self.mark:]

    def set_mark(self):
        self.mark = len(self.text().splitlines())

    def wait(self, pattern, timeout=15):
        end = time.time() + timeout
        while time.time() < end:
            if self.proc.poll() is not None:
                return None
            for line in self.lines_since_mark():
                if re.search(pattern, line):
                    return line
            time.sleep(0.15)
        return None

    def cmd(self, *lines):
        path = os.path.join(self.run, "nebula-test-cmd")
        with open(path + ".tmp", "w") as f:
            f.write("\n".join(lines) + "\n")
        os.replace(path + ".tmp", path)
        # taken by the app's poll (300 ms)
        end = time.time() + 5
        while os.path.exists(path) and time.time() < end:
            time.sleep(0.05)
        time.sleep(0.5)

    def shot(self, name):
        """A screenshot of the window (the app's SIGUSR1), copied to the output; its file."""
        src = os.path.join(self.tmp, "cache", "nebula-shot.png")
        if os.path.exists(src):
            os.remove(src)
        self.proc.send_signal(signal.SIGUSR1)
        end = time.time() + 6
        while not os.path.exists(src) and time.time() < end:
            time.sleep(0.1)
        time.sleep(0.3)
        if not os.path.exists(src):
            return None
        dst = os.path.join(self.out, name + ".png")
        shutil.copy(src, dst)
        return dst

    def press(self, wait=15):
        """Press the shutter; the demo capture's log line (the request), or None."""
        self.set_mark()
        self.cmd("press")
        return self.wait(r"nebula: demo capture", wait)


class Results:
    def __init__(self):
        self.rows = []

    def add(self, case, name, ok, detail=""):
        self.rows.append((case, name, ok, detail))
        print(f"  {'PASS' if ok else 'FAIL'}  {case}: {name}" + (f"   [{detail}]" if detail else ""), flush=True)


def parse(line):
    m = re.search(r"mask=(0x[0-9a-f]+) burst=(\d+) flags=(\d+) zoom=(\d+) args=(.*)$", line or "")
    if not m:
        return None
    args = m.group(5).split()
    kv = {}
    for i, a in enumerate(args):
        if a.startswith("--") and i + 1 < len(args) and not args[i + 1].startswith("--"):
            kv[a[2:]] = args[i + 1]
    return dict(mask=int(m.group(1), 16), burst=int(m.group(2)), flags=int(m.group(3)), zoom=int(m.group(4)), args=kv)


def settle(app):
    app.wait(r"never", 0.2)
    time.sleep(0.6)


def case_checks(app, res):
    r = subprocess.run(["cargo", "test"], cwd=ROOT, capture_output=True, text=True)
    m = re.search(r"test result: (\w+)\. (\d+) passed; (\d+) failed", r.stdout)
    res.add("checks", "cargo test", bool(m) and m.group(1) == "ok", f"{m.group(2)} passed, {m.group(3)} failed" if m else r.stderr[-200:])


def case_shots(app, res):
    for mode in MODES:
        for mm, mask in LENSES.items():
            app.cmd(f"mode {mode}", f"zoom {mm}")
            if mode in ("iso", "manual"):
                app.cmd("iso 0.55")
            if mode in ("shutter", "manual"):
                app.cmd("shutter 0.5")
            time.sleep(1.2)
            req = parse(app.press())
            label = f"{mode} at {mm} mm"
            if not req:
                res.add("shots", label, False, "no capture request")
                continue
            problems = []
            if req["mask"] != mask:
                problems.append(f"lens set {req['mask']:#x}, expected {mask:#x}")
            if req["flags"] != QUICK:
                problems.append(f"flags {req['flags']}, expected {QUICK}")
            if mode in ("iso", "manual") and abs(int(req["args"].get("iso", 0)) - iso_at(0.55)) > 1:
                problems.append(f"ISO {req['args'].get('iso')}, expected {iso_at(0.55)}")
            if mode in ("shutter", "manual"):
                want = secs_at(0.5) * 1e6
                got = float(req["args"].get("exposure-us", 0))
                if not math.isclose(got, want, rel_tol=0.02):
                    problems.append(f"exposure {got:.0f} us, expected {want:.0f}")
            # the photo carries its framing: the zoom as the assembler's crop
            if abs(float(req["args"].get("zoom", 0)) - mm) > 0.1:
                problems.append(f"zoom {req['args'].get('zoom')}, expected {mm}")
            res.add("shots", label, not problems, "; ".join(problems) or f"mask {req['mask']:#x}")
            app.wait(r"nebula: saved", 5)
    app.cmd("mode auto", "zoom 28")


def case_rotation(app, res):
    holds = [("right-up", None), ("left-up", "7"), ("normal", "1"), ("bottom-up", "2")]
    hashes = {}
    app.cmd("mode auto", "zoom 28")
    for name, tag in holds:
        app.cmd(f"orientation {name}")
        time.sleep(1.2)
        png = app.shot("rot-" + name)
        if png:
            hashes[name] = hashlib.md5(open(png, "rb").read()).hexdigest()
        res.add("rotation", f"{name}: the screen is drawn", bool(png) and os.path.getsize(png) > 20000, png or "no screenshot")
        req = parse(app.press())
        got = req["args"].get("orientation") if req else "no request"
        res.add("rotation", f"{name}: photo tagged {tag or 'as taken'}", req is not None and got == tag, f"orientation {got}")
        app.wait(r"nebula: saved", 5)
    # right-up and left-up look the same (the UI does not turn for them: the compositor draws it)
    portrait = {hashes.get("normal"), hashes.get("bottom-up")}
    res.add("rotation", "portrait looks different from landscape (the UI turned)",
            hashes.get("normal") not in (None, hashes.get("right-up")) and hashes.get("bottom-up") not in (None, hashes.get("right-up")))
    res.add("rotation", "the two portraits differ from each other", len(portrait) == 2)
    app.cmd("orientation clear")


def case_burst(app, res):
    app.cmd("mode auto", "burst 1")
    time.sleep(1)
    app.set_mark()
    app.cmd("press")
    app.wait(r"nebula: demo capture", 10)
    time.sleep(3)
    saved = [l for l in app.lines_since_mark() if "nebula: saved" in l]
    res.add("burst", "three photos from one press", len(saved) == 3, f"{len(saved)} saved")
    app.cmd("burst 0")


def case_timer(app, res):
    app.cmd("mode auto", "timer 1")
    time.sleep(1)
    t0 = time.time()
    line = app.press(wait=15)
    waited = time.time() - t0
    res.add("timer", "3 s timer waits about 3 s", line is not None and 2.4 <= waited <= 5, f"{waited:.1f} s")
    app.wait(r"nebula: saved", 5)
    app.cmd("timer 0")


def case_long(app, res):
    app.cmd("mode manual", "iso 0.6", "shutter 0")
    time.sleep(1)
    req = parse(app.press())
    got = float(req["args"].get("exposure-us", 0)) if req else 0
    res.add("long", "a 15 s exposure asks for 15 s", math.isclose(got, 15e6, rel_tol=0.001), f"{got:.0f} us")
    app.wait(r"nebula: saved", 5)
    app.cmd("mode auto")


def case_ui(app, res):
    seen = {}
    for mode in MODES:
        app.cmd(f"mode {mode}")
        time.sleep(1)
        png = app.shot("ui-" + mode)
        ok = bool(png) and os.path.getsize(png) > 20000
        res.add("ui", f"{mode}: the screen is drawn", ok, png or "no screenshot")
        if png:
            seen[mode] = hashlib.md5(open(png, "rb").read()).hexdigest()
    res.add("ui", "each mode looks different", len(set(seen.values())) == len(seen))
    app.cmd("mode auto")


CASES = [("checks", case_checks), ("shots", case_shots), ("rotation", case_rotation), ("burst", case_burst),
         ("timer", case_timer), ("long", case_long), ("ui", case_ui)]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--only")
    ap.add_argument("--no-build", action="store_true")
    ap.add_argument("--keep", action="store_true", help="keep the temporary settings and pictures")
    ap.add_argument("--out", default=os.path.join(os.getcwd(), "pipeline-demo-out"))
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    if not a.no_build:
        b = subprocess.run(["cargo", "build"], cwd=ROOT, capture_output=True, text=True)
        if b.returncode:
            print(b.stderr[-1500:])
            sys.exit(2)
    names = a.only.split(",") if a.only else [n for n, _ in CASES]
    res = Results()
    app = App(a.out, a.keep)
    try:
        if not app.start():
            print("the app did not start:\n" + app.text()[-800:])
            sys.exit(2)
        for name, fn in CASES:
            if name not in names:
                continue
            print(f"== {name}", flush=True)
            try:
                fn(app, res)
            except Exception as e:
                res.add(name, "ran to the end", False, repr(e))
        res.add("end", "the app is still running", app.proc.poll() is None)
    finally:
        app.stop()
    bad = [r for r in res.rows if not r[2]]
    print(f"\n{len(res.rows) - len(bad)} of {len(res.rows)} checks passed" + (f"; {len(bad)} failed:" if bad else ""))
    for c, n, _, d in bad:
        print(f"  FAIL  {c}: {n}  [{d}]")
    print(f"screenshots and the log: {a.out}")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
