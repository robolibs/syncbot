"""2. The barn and some robots, into a running Gearbox. Exits when they stand.

    python3 scene.py            # three Huskies
    python3 scene.py 5
    MACHINE=hunter python3 scene.py

Clears the simulator, loads the flat ground, places the barn, and parks the
machines on the barn's stands — read from the `demarke.json` that `server.py`
wrote, so the two sides agree on where everything is. Nothing drives them;
that is `coordinator.py`. Start Gearbox yourself first.

The barn is placed so that Gearbox's world *is* syncbot's ENU frame: Gearbox
is Y-up and maps a Z-up USD as (x, y, z) -> (x, z, -y), so a point at ENU
(x, y) sits at Gearbox (x, -y), and a machine's position reads straight off as
ENU with no conversion. The barn model is laid out in its own metres, so it
goes in turned and shifted by the same numbers `server.py` used to put the
road network on the real footprint.
"""
from __future__ import annotations

import json
import math
import os
import pathlib
import sys
import threading
import time
import urllib.request

import cbor2
import zenoh

MACHINE = os.environ.get("MACHINE", "husky")
COUNT = int(sys.argv[1]) if len(sys.argv) > 1 else 3
DEMARKE_JSON = pathlib.Path(__file__).resolve().parent / "demarke.json"
SERVER_URL = os.environ.get("SYNCBOT_URL", "http://127.0.0.1:8080")
ROBOT_KEY = os.environ.get("SYNCBOT_KEY", "0")

FLATLAND_USD = "world/flatland.usd"
BARN_USD = "demarke/barn.usdc"

# The model's footprint corner (BARN_X0, BARN_Y0) lands on ANCHOR; its +x runs
# along ACROSS and its +y along ALONG. Same numbers as in server.py.
BARN_X0, BARN_Y0 = 21.5, 0.0
ANCHOR = (24.55, -20.25)
ALONG = (-0.5315, 0.8471)
ACROSS = (0.8471, 0.5315)

# Where machines start: the same three the simulated runs used, then the rest.
STARTS = ["barn_up_1", "east_outer_1", "barn_down_2"]
STATE_TIMEOUT_S = 15.0


class Gearbox:
    def __init__(self):
        opened = zenoh.open(zenoh.Config())
        self.session = opened.wait() if hasattr(opened, "wait") else opened
        self.states, self.lock, self._subs = {}, threading.Lock(), []

    def put(self, key, payload):
        self.session.put(key, cbor2.dumps(payload), congestion_control=zenoh.CongestionControl.BLOCK)

    def watch(self, namespace):
        def remember(sample, ns=namespace):
            with self.lock:
                self.states[ns] = cbor2.loads(bytes(sample.payload))
        self._subs.append(self.session.declare_subscriber(f"gearbox/machines/{namespace}/state", remember))

    def heard(self, namespace):
        with self.lock:
            return namespace in self.states

    def load(self, load_id, category, usd_path, x_enu, y_enu, yaw_deg=0.0, **extra):
        """Load a USD at an ENU position: Gearbox x is east, Gearbox z is south."""
        self.put(f"gearbox/usd/load/{load_id}",
                 {"category": category, "usd_path": usd_path, "x": float(x_enu), "y": 0.0,
                  "z": float(-y_enu), "yaw_deg": float(yaw_deg), "remove": False, **extra})

    def close(self):
        for sub in self._subs:
            sub.undeclare()
        self.session.close()


def barn_pose():
    """Where the barn model's origin goes, and how far it turns, in ENU.

    ENU = ANCHOR + R (p - corner), R = [ACROSS ALONG]; the model origin is
    p = 0. Positive yaw in Gearbox is counter-clockwise seen from above, the
    same sense as an ENU angle, so the turn is ACROSS's own bearing from east.
    """
    x = ANCHOR[0] - (BARN_X0 * ACROSS[0] + BARN_Y0 * ALONG[0])
    y = ANCHOR[1] - (BARN_X0 * ACROSS[1] + BARN_Y0 * ALONG[1])
    return (x, y), math.degrees(math.atan2(ACROSS[1], ACROSS[0]))


def deregister_all():
    """Every robot the server still knows leaves it, with its claims.

    A new scene means a new fleet; a machine registered by an earlier run
    would otherwise sit in the server holding road nobody is on.
    """
    base = SERVER_URL.rstrip("/") + "/ares/v1"
    try:
        with urllib.request.urlopen(f"{base}/fleet/snapshot", timeout=5.0) as response:
            robots = [r["robot_id"] for r in json.loads(response.read())["robots"]]
    except Exception as error:  # noqa: BLE001 — no server is not fatal for a scene
        print(f"  no syncbot server at {SERVER_URL} ({error}); nothing to deregister", flush=True)
        return
    for robot in robots:
        request = urllib.request.Request(
            f"{base}/robots/{robot}/deregister", method="POST",
            data=json.dumps({"key": ROBOT_KEY}).encode(),
            headers={"content-type": "application/json"})
        with urllib.request.urlopen(request, timeout=5.0) as response:
            reply = json.loads(response.read())
        print(f"  robot {robot} deregistered" if reply.get("decision") == 1
              else f"  robot {robot} not deregistered (reason {reply.get('reason')})", flush=True)
    if not robots:
        print("  server had no robots", flush=True)


def stands_from(document):
    """Stand name -> ENU point, out of the node properties server.py wrote."""
    out = {}
    for node in document["nodes"].values():
        stand = node.get("properties", {}).get("barn.stand")
        if stand:
            out[stand] = (node["latlon"]["lon"], node["latlon"]["lat"])
    return out


def main():
    if not DEMARKE_JSON.exists():
        print(f"{DEMARKE_JSON.name} is missing — run server.py first, it writes it")
        return 1
    stands = stands_from(json.loads(DEMARKE_JSON.read_text(encoding="utf-8")))
    order = STARTS + sorted(s for s in stands if s not in STARTS)
    namespaces = [f"{MACHINE}_{i + 1}" for i in range(COUNT)]

    deregister_all()
    link = Gearbox()
    for namespace in namespaces:
        link.watch(namespace)
    link.put("gearbox/sim/clear", {"pause_clock": False})
    # The clear and the loads travel on different topics with no ordering
    # between them; a late clear despawns the first load from under Gearbox.
    time.sleep(1.5)
    link.load("flatland_terrain", "terrain", FLATLAND_USD, 0.0, 0.0)
    time.sleep(0.5)
    (bx, by), yaw = barn_pose()
    link.load("demarke_barn", "world", BARN_USD, bx, by, yaw_deg=yaw)
    print(f"  barn loading in Gearbox at ENU ({bx:.1f}, {by:.1f}), turned {yaw:.1f} deg", flush=True)
    time.sleep(6.0)

    for i, namespace in enumerate(namespaces):
        stand = order[i % len(order)]
        x, y = stands[stand]
        link.load(namespace, "machine", f"bin/gearbox/assets/{MACHINE}.usd", x, y,
                  namespace=namespace, label=f"{MACHINE} ({namespace})")
        # Loading the next machine while this one is still being wired into
        # the physics world takes Gearbox down; its state going live says done.
        deadline = time.time() + STATE_TIMEOUT_S
        while not link.heard(namespace) and time.time() < deadline:
            time.sleep(0.1)
        print(f"  {namespace:<10} at {stand:<14} ENU ({x:6.1f}, {y:6.1f})"
              f"{'' if link.heard(namespace) else '   — no state yet'}", flush=True)

    print(f"\n{COUNT} x {MACHINE} standing in the barn. Now:  python3 coordinator.py --rolling")
    link.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
