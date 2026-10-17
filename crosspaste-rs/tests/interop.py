"""Layer-2 interop test: two crosspaste-rs peers (`cprs`) against an official
CrossPaste instance running headless, driven through CrossPaste's CLI socket.

  A: LAN address + mDNS. CrossPaste discovers it and starts pairing (A shows the code).
  B: loopback only. B starts pairing with CrossPaste (CrossPaste shows the code).

Covered: v2 pairing in both directions, encrypted heartbeats, text both ways (UTF-8),
image B -> CrossPaste (two chunks, byte-exact), CrossPaste -> A (CrossPaste relays
B's image and serves /pull/file), image A -> CrossPaste.

Usage: python3 interop.py --cprs <cprs binary> --sock <CrossPaste CLI socket> --work <empty dir>
CrossPaste is reached with `curl --unix-socket` (Python on Windows has no AF_UNIX).
"""

import argparse
import filecmp
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from gen_png import write_png  # noqa: E402


class Fail(Exception):
    pass


class Referee:
    def __init__(self, sock: str):
        self.sock = sock

    def call(self, method: str, path: str, body=None):
        cmd = ["curl", "-sS", "--unix-socket", self.sock, "-X", method, f"http://localhost{path}"]
        data = None
        if body is not None:
            cmd += ["-H", "Content-Type: application/json", "--data-binary", "@-"]
            data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        out = subprocess.run(cmd, input=data, capture_output=True, check=True).stdout
        return json.loads(out.decode("utf-8")) if out.strip() else None

    def get(self, path: str):
        return self.call("GET", path)


class Peer:
    def __init__(self, name: str, cprs: str, work: Path, port: int, loopback: bool):
        self.name = name
        self.dir = work / name
        self.dir.mkdir(parents=True, exist_ok=True)
        env = dict(os.environ)
        if loopback:
            env["CPRS_LOOPBACK"] = "1"
        self.err = open(self.dir / "err.log", "wb")
        self.proc = subprocess.Popen(
            [cprs, str(self.dir / "data"), str(port), f"cprs-{name}"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.err,
            env=env,
        )
        self.events = []
        self.lock = threading.Lock()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.proc.stdout:
            try:
                event = json.loads(line.decode("utf-8"))
            except ValueError:
                continue
            with self.lock:
                self.events.append(event)

    def send(self, *words: str):
        self.proc.stdin.write((" ".join(words) + "\n").encode("utf-8"))
        self.proc.stdin.flush()

    def wait(self, pred, what: str, timeout: float = 20):
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self.lock:
                for event in self.events:
                    if pred(event):
                        return event
                errors = [e for e in self.events if e.get("event") == "error"]
            if errors:
                raise Fail(f"{self.name}: {errors[-1]['message']} (while waiting for {what})")
            time.sleep(0.2)
        raise Fail(f"{self.name}: timed out waiting for {what}")

    def stop(self):
        try:
            self.send("quit")
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()
        self.err.close()

    def dump(self):
        print(f"--- {self.name} events", file=sys.stderr)
        for event in self.events[-30:]:
            print(json.dumps(event, ensure_ascii=False), file=sys.stderr)
        print(f"--- {self.name} log", file=sys.stderr)
        print((self.dir / "err.log").read_text(errors="replace")[-4000:], file=sys.stderr)


def poll(fn, what: str, timeout: float = 20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = fn()
        if value:
            return value
        time.sleep(0.5)
    raise Fail(f"CrossPaste: timed out waiting for {what}")


def run(ref: Referee, a: Peer, b: Peer, work: Path):
    status = ref.get("/cli/status")
    ref_id, ref_port, version = status["appInstanceId"], status["port"], status["appVersion"]
    print(f"referee: CrossPaste {version} id={ref_id} port={ref_port}")
    for key in ("enableEncryptSync", "enableClipboardRelay"):
        ref.call("PUT", "/cli/config", {"key": key, "value": "true"})

    big, small = work / "big.png", work / "small.png"
    write_png(big, 800, 600)
    write_png(small, 120, 80)

    a_id = a.wait(lambda e: e.get("event") == "started", "start")["id"]
    b_id = b.wait(lambda e: e.get("event") == "started", "start")["id"]
    print(f"A={a_id} B={b_id}")

    print("== 1. CrossPaste discovers A over mDNS and pairs with it (A shows the code)")
    poll(
        lambda: any(d["appInstanceId"] == a_id for d in ref.get("/cli/pair/nearby?refresh=true")),
        "A in nearby list",
        40,
    )
    session = ref.call("POST", "/cli/pair/initiate", {"appInstanceId": a_id})
    code = a.wait(lambda e: e.get("event") == "pairing", "pairing code")["code"]
    result = ref.call("POST", "/cli/pair/submit", {"sessionId": session["sessionId"], "code": code})
    if not result.get("paired"):
        raise Fail(f"pair submit: {result}")
    a.wait(lambda e: e.get("event") == "paired", "paired")

    print("== 2. B pairs with CrossPaste (CrossPaste shows the code)")
    b.send("add", "127.0.0.1", str(ref_port))
    b.wait(lambda e: e.get("event") == "added", "added")
    b.send("pair", ref_id)
    ours = b.wait(lambda e: e.get("event") == "pair-started", "pair start")["code"]
    theirs = poll(
        lambda: next(
            (r["token"] for r in ref.get("/cli/pair/token")["requests"] if r["appInstanceId"] == b_id),
            None,
        ),
        "pairing token for B",
    )
    if ours != theirs:
        raise Fail(f"SAS mismatch: ours {ours}, CrossPaste shows {theirs}")
    b.send("pair-finish", ref_id, theirs)
    b.wait(lambda e: e.get("event") == "pair-finished", "pair finish")

    print("== 3. Both peers connected (encrypted heartbeat accepted)")
    for peer in (a_id, b_id):
        poll(
            lambda: any(
                d["appInstanceId"] == peer and d["connectState"] == 0 for d in ref.get("/cli/devices")
            ),
            f"{peer} connected",
            30,
        )
    a.send("devices")
    a.wait(lambda e: e.get("device") == ref_id and e.get("state") == "Connected", "A sees referee connected")

    print("== 4. Text CrossPaste -> A")
    t1 = f"CrossPaste → crosspaste-rs: 中文 ✅ {time.time()}"
    ref.call("POST", "/cli/copy", {"text": t1})
    a.wait(lambda e: e.get("event") == "text" and e.get("text") == t1, "text from CrossPaste")

    print("== 5. Text A -> CrossPaste")
    t2 = f"crosspaste-rs → CrossPaste: 日本語 ✅ {time.time()}"
    a.send("text", t2)
    poll(
        lambda: any(
            i["preview"] == t2 and i["remote"] for i in ref.get("/cli/history?limit=10")["items"]
        ),
        "text from A",
    )

    def stored_image_equal_to(expected: Path):
        for item in ref.get("/cli/history?limit=10")["items"]:
            if item["typeName"] != "image" or not item["remote"]:
                continue
            paths = ref.get(f"/cli/paste/{item['id']}").get("filePaths") or []
            if paths and Path(paths[0]).is_file() and filecmp.cmp(paths[0], expected, shallow=False):
                return paths[0]
        return None

    print(f"== 6. Image B -> CrossPaste ({big.stat().st_size} bytes, two chunks)")
    b.send("image", str(big))
    b.wait(lambda e: e.get("event") == "sent", "image sent")
    poll(lambda: stored_image_equal_to(big), "byte-identical image from B", 30)

    print("== 7. CrossPaste relays it to A (A pulls the chunks from CrossPaste)")
    got = a.wait(lambda e: e.get("event") == "images", "relayed image", 40)["paths"][0]
    if not filecmp.cmp(got, big, shallow=False):
        raise Fail("image pulled by A differs")

    print("== 8. Image A -> CrossPaste")
    a.send("image", str(small))
    poll(lambda: stored_image_equal_to(small), "byte-identical image from A", 30)

    print(f"PASS: crosspaste-rs interoperates with CrossPaste {version}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cprs", required=True)
    parser.add_argument("--sock", required=True)
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--a-port", type=int, default=13150)
    parser.add_argument("--b-port", type=int, default=13151)
    args = parser.parse_args()
    args.work.mkdir(parents=True, exist_ok=True)

    ref = Referee(args.sock)
    a = Peer("a", args.cprs, args.work, args.a_port, loopback=False)
    b = Peer("b", args.cprs, args.work, args.b_port, loopback=True)
    try:
        run(ref, a, b, args.work)
    except (Fail, subprocess.CalledProcessError) as e:
        print(f"FAIL: {e}", file=sys.stderr)
        a.dump()
        b.dump()
        sys.exit(1)
    finally:
        a.stop()
        b.stop()


if __name__ == "__main__":
    main()
