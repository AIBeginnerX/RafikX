import json
import urllib.request

for name in ["pulldown-cmark", "notify", "notify-debouncer-mini"]:
    url = f"https://crates.io/api/v1/crates/{name}"
    req = urllib.request.Request(url, headers={"User-Agent": "agent-harness-phase4"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        d = json.loads(resp.read().decode("utf-8"))
    c = d.get("crate", {})
    print(f"{name}: max_stable={c.get('max_stable_version')}")
