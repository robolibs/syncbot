"""1. The syncbot server with the de Marke barn loaded, drawing the fleet in rerun.

    python3 server.py

Starts the real syncbot REST server (the `syncbot` binary of this repo, built
if missing), builds the barn workspace, writes it out as `demarke.json` next
to this file, pushes it into the server, and then draws what the server knows
— every robot where the server has it, pointing where the server says, the
road each one holds in its own colour and the zones that implies — to a rerun
viewer that is already running. Ctrl-C stops both.

The server has everything the picture needs: the workspace with its zones and
the robots' positions, attitudes and claims. So this is the first thing to
start and the only thing that talks to rerun. Robots — simulated by `scene.py`
and `coordinator.py`, or real — are clients of it and nothing more.

The barn's geometry was measured out of ``de marke/cow_house.usd`` — 23 m wide,
67 m long — and is laid out in the model's own metres, then rotated onto the
OpenStreetMap footprint of the real barn (way 257914023, bearing 327.9°), all
in ENU metres about DATUM. Around the barn's one through-alley the yard is
laid out in pairs: two lanes through the barn, two roads down each side, two
bars across each end, every run one-way and the two of a pair opposite ways,
with six crossovers per pair. Two of everything, so a machine that claims a
run whole costs the rest a detour and never a deadlock.

``SYNCBOT_URL`` (default http://127.0.0.1:8080), ``RATE`` frames per second.
"""
from __future__ import annotations

import bisect
import json
import math
import os
import pathlib
import shutil
import subprocess
import sys
import time
import urllib.parse
import urllib.request

import rerun as rr
import syncbot

REPO = pathlib.Path(__file__).resolve().parents[3]
HERE = pathlib.Path(__file__).resolve().parent
DEMARKE_JSON = HERE / "demarke.json"

SERVER_URL = os.environ.get("SYNCBOT_URL", "http://127.0.0.1:8080")
ADMIN_KEY = os.environ.get("SYNCBOT_ADMIN_KEY", "4242")
RATE_HZ = float(os.environ.get("RATE", "5"))

# De Marke, Hengelo (Gld). ENU metres are east and north of this point.
DATUM = (52.0384467, 6.348276, 0.0)

# ------------------------------------------------------------------- layout

STRAIGHT_STEP = 2.0
CURVE_STEP = 0.6
# Two points closer than this are the same place, so paths that meet join.
SNAP = 0.9

# The barn's own footprint, from the model.
BARN_X0, BARN_X1 = 21.5, 44.8
BARN_Y0, BARN_Y1 = 0.0, 67.5

# The model laid onto the real footprint. ANCHOR is the OSM corner the model's
# (BARN_X0, BARN_Y0) becomes; ALONG runs up the barn, ACROSS is ALONG turned a
# quarter to the right, so the two make a rotation and not a mirror. Nothing
# is scaled. `scene.py` places the barn in the simulator by these same numbers.
ANCHOR = (24.55, -20.25)
ALONG = (-0.5315, 0.8471)
ACROSS = (0.8471, 0.5315)


def to_enu(x, y):
    """A point in the model's own metres, placed on the real barn."""
    along, across = y - BARN_Y0, x - BARN_X0
    return (ANCHOR[0] + along * ALONG[0] + across * ACROSS[0],
            ANCHOR[1] + along * ALONG[1] + across * ACROSS[1])


UP, DOWN, EAST, WEST = 1, -1, 1, -1

# The barn's alley is x 29.25–32.50; both lanes run inside it, PAIR apart.
BARN_ALLEY = (29.25, 32.50)
_c = sum(BARN_ALLEY) / 2.0
PAIR = 0.95
STANDOFF = 2 * PAIR
_w, _e = 20.6 - STANDOFF, 45.5 + STANDOFF
LANES = [
    ("west_outer", _w - 2 * PAIR, UP),
    ("west_inner", _w, DOWN),
    ("barn_up", _c - PAIR, UP),
    ("barn_down", _c + PAIR, DOWN),
    ("east_inner", _e, UP),
    ("east_outer", _e + 2 * PAIR, DOWN),
]
LANE_X = {name: x for name, x, _ in LANES}
BARN_LANES = ["barn_up", "barn_down"]
_s, _n = -1.4 - STANDOFF, 68.9 + STANDOFF
BARS = [
    ("south_outer", _s - 2 * PAIR, WEST),
    ("south_inner", _s, EAST),
    ("north_inner", _n, WEST),
    ("north_outer", _n + 2 * PAIR, EAST),
]
BAR_Y = {name: y for name, y, _ in BARS}
PAIRS = [("west_outer", "west_inner"), ("barn_up", "barn_down"), ("east_inner", "east_outer")]
CROSSOVERS = 6
CROSS_Y = [5.0 + i * (62.0 - 5.0) / (CROSSOVERS - 1) for i in range(CROSSOVERS)]

ROOT_X0, ROOT_X1 = 4.0, 63.0
ROOT_Y0, ROOT_Y1 = -19.0, 86.0
ZONE_HALF = 0.8

SHARED2 = {"traffic.policy": "shared", "traffic.capacity": "2"}
SLOW = {"traffic.policy": "slow", "traffic.speed_limit": "0.5"}
ONE_WAY = {"traffic.lane_type": "corridor", "traffic.preferred_direction": "forward",
           "traffic.reversible": "false"}
TWO_WAY = {"traffic.lane_type": "corridor"}


def box(x0, y0, x1, y1):
    return [to_enu(*p) for p in ((x0, y0), (x1, y0), (x1, y1), (x0, y1))]


def zones():
    """One zone per run, plus the cubicles: (name, numeric id, polygon, properties)."""
    out, n = [], 0
    for name, x, _way in LANES:
        n += 1
        y0, y1 = ((BARN_Y0, BARN_Y1) if name in BARN_LANES
                  else (BAR_Y["south_outer"] - 1.0, BAR_Y["north_outer"] + 1.0))
        props = {**SHARED2, **SLOW} if name in BARN_LANES else SHARED2
        out.append((name, n, box(x - ZONE_HALF, y0, x + ZONE_HALF, y1), props))
    for name, y, _way in BARS:
        n += 1
        out.append((name, n, box(LANE_X["west_outer"] - ZONE_HALF, y - ZONE_HALF,
                                 LANE_X["east_outer"] + ZONE_HALF, y + ZONE_HALF), SHARED2))
    out.append(("cubicles_west", n + 1, box(BARN_X0, BARN_Y0, BARN_ALLEY[0], BARN_Y1), SLOW))
    out.append(("cubicles_east", n + 2, box(BARN_ALLEY[1], BARN_Y0, BARN_X1, BARN_Y1), SLOW))
    return out


def line(a, b, step=STRAIGHT_STEP):
    (x0, y0), (x1, y1) = a, b
    n = max(int(math.dist(a, b) / step), 1)
    return [(x0 + (x1 - x0) * i / n, y0 + (y1 - y0) * i / n) for i in range(n + 1)]


def chain(*runs):
    out = []
    for run in runs:
        if out and math.dist(out[-1], run[0]) < SNAP:
            run = run[1:]
        out.extend(run)
    return out


def paths():
    """Every run in the yard as ENU points, with whether it is one-way.

    Each run is sampled segment by segment *between its crossings*, so a point
    lands exactly on every junction and the builder can snap them together;
    sampled end to end the verticals and bars would only look like they meet.
    """
    out = []
    stop_ys = sorted({y for _n, y, _w in BARS} | set(CROSS_Y))
    lane_xs = sorted(x for _n, x, _w in LANES)
    for _name, x, way in LANES:
        stops = [(x, y) for y in stop_ys]
        if way == DOWN:
            stops.reverse()
        out.append(([to_enu(*p) for p in chain(*(line(a, b) for a, b in zip(stops, stops[1:])))], True))
    for _name, y, way in BARS:
        stops = [(x, y) for x in lane_xs]
        if way == WEST:
            stops.reverse()
        out.append(([to_enu(*p) for p in chain(*(line(a, b) for a, b in zip(stops, stops[1:])))], True))
    for left, right in PAIRS:
        for y in CROSS_Y:
            out.append(([to_enu(*p) for p in line((LANE_X[left], y), (LANE_X[right], y), CURVE_STEP)], False))
    return out


def stands():
    """Where a job can start or finish: four down every vertical run, (name, enu)."""
    out = []
    for name, x, _way in LANES:
        y0, y1 = ((BARN_Y0 + 8, BARN_Y1 - 8) if name in BARN_LANES
                  else (BAR_Y["south_inner"] + 8, BAR_Y["north_inner"] - 8))
        for i in range(4):
            out.append((f"{name}_{i}", to_enu(x, y0 + (y1 - y0) * i / 3.0)))
    return out


# ---------------------------------------------------------------- workspace


def build_workspace():
    """The barn as a syncbot workspace. Nodes that are stands say so in a
    property, so `coordinator.py` can read them back out of the document."""
    builder = syncbot.WorkspaceBuilder(
        "cow_barn", box(ROOT_X0, ROOT_Y0, ROOT_X1, ROOT_Y1), DATUM,
        properties={"traffic.policy": "shared"})
    for name, numeric, boundary, props in zones():
        builder.add_zone(name, boundary, numeric_id=numeric, properties=props)

    grid, points, linked = {}, [], set()
    cell = lambda p: (int(p[0] // SNAP), int(p[1] // SNAP))  # noqa: E731

    def snap(point):
        cx, cy = cell(point)
        for dx in (-1, 0, 1):
            for dy in (-1, 0, 1):
                for key in grid.get((cx + dx, cy + dy), ()):
                    if math.dist(points[key], point) < SNAP:
                        return key
        return None

    stand_of = {}
    for run, directed in paths():
        previous = None
        for point in run:
            key = snap(point)
            if key is None:
                key = len(points)
                points.append(point)
                grid.setdefault(cell(point), []).append(key)
            if previous is not None and previous != key:
                pair = (previous, key) if directed else (min(previous, key), max(previous, key))
                if pair not in linked:
                    linked.add(pair)
                    stand_of.setdefault(("edge", pair), None)
            previous = key
    for name, at in stands():
        stand_of[min(range(len(points)), key=lambda k: math.dist(points[k], at))] = name
    for key, point in enumerate(points):
        props = {"barn.stand": stand_of[key]} if stand_of.get(key) else None
        builder.add_node(f"n{key}", point[0], point[1], numeric_id=1000 + key, properties=props)
    n = 0
    for run, directed in paths():
        previous = None
        seen = set()
        for point in run:
            key = snap(point)
            if previous is not None and previous != key:
                pair = (previous, key) if directed else (min(previous, key), max(previous, key))
                if pair not in seen and pair in linked:
                    linked.discard(pair)
                    seen.add(pair)
                    n += 1
                    builder.add_edge(f"n{previous}", f"n{key}", directed=directed,
                                     weight=math.dist(points[previous], points[key]),
                                     numeric_id=100_000 + n,
                                     properties=ONE_WAY if directed else TWO_WAY)
            previous = key
    return builder.build()


# ------------------------------------------------------------------- server


class Server:
    def __init__(self, url=SERVER_URL):
        self.base = url.rstrip("/") + "/ares/v1"

    def _call(self, method, path, data=None, headers=None, timeout=5.0):
        request = urllib.request.Request(
            f"{self.base}{path}", data=data, method=method,
            headers={"content-type": "application/json", **(headers or {})})
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read()
        return json.loads(raw) if raw else None

    def health(self):
        return self._call("GET", "/health")

    def push_workspace(self, document):
        return self._call("POST", "/workspace", document.encode(),
                          {"x-ares-admin-key": ADMIN_KEY}, timeout=30.0)

    def snapshot(self):
        return self._call("GET", "/fleet/snapshot")


def start_server(listen):
    """The `syncbot` server binary from this repo, built if it is missing."""
    binary = shutil.which("syncbot") or str(REPO / "target/debug/syncbot")
    if not os.path.exists(binary):
        print("  building the server (once)", flush=True)
        subprocess.run(["cargo", "build", "--features", "rest xmlt", "--bin", "syncbot"],
                       cwd=REPO, check=True)
    # Info-level logs unless asked otherwise: at debug the HTTP layer prints
    # three lines per request, and this script alone makes five a second.
    env = {**os.environ, "SYNCBOT_ALLOW_DEFAULT_KEY": os.environ.get("SYNCBOT_ALLOW_DEFAULT_KEY", "1"),
           "SYNCBOT_ADMIN_KEY": ADMIN_KEY, "RUST_LOG": os.environ.get("RUST_LOG", "info")}
    return subprocess.Popen([binary, "--no-map", listen], env=env)


# ------------------------------------------------------------------ drawing

C_BARN = (0xB6, 0xC2, 0xD4)
C_GRAPH = (0x8A, 0x99, 0xAD)
C_ZONE = (0x7E, 0x9A, 0xB8)
ZONE_WIDTH = 0.03
BARN_WIDTH = 0.30
MACHINE_LENGTH, MACHINE_BODY = 0.99, 0.67
MACHINE_RGB = [(0x62, 0xB6, 0xEE), (0xF2, 0x86, 0x7A), (0x7F, 0xD4, 0x9C),
               (0xC9, 0x9B, 0xE8), (0xF2, 0xC9, 0x5D), (0x63, 0xD4, 0xD1)]


def colour_of(robot_id):
    return MACHINE_RGB[(robot_id - 1) % len(MACHINE_RGB)]


def scaled(rgb, factor):
    return tuple(min(255, max(0, round(c * factor))) for c in rgb)


def connect(app_id):
    """Stream to a viewer that is already running; never spawn one."""
    rr.init(app_id)
    rrd = os.environ.get("SYNCBOT_VIZ_RRD")
    if rrd:
        rr.save(rrd)
        return
    rr.connect_grpc(os.environ.get("RERUN_URL") or "rerun+http://0.0.0.0:9876/proxy")


def log_lines(path, lines, color, radius, index=None):
    """Lines in scene metres, and on the map at the map's own default weight —
    a metre-wide radius on a map is a road drawn as a motorway."""
    rr.log(f"enu/{path}", rr.LineStrips3D([[(x, y, 0.0) for x, y in line] for line in lines],
                                          colors=[color] * len(lines), radii=[radius] * len(lines)), static=True)
    if index is not None:
        rr.log(f"wgs/{path}", rr.GeoLineStrings(
            lat_lon=[[lat_lon(index, *p) for p in line] for line in lines],
            colors=[color] * len(lines)), static=True)


def lat_lon(index, x, y):
    geo = index.local_to_global(x, y, 0.0)
    return (geo["lat"], geo["lon"])


def log_static(index, zone_polys, graph_lines):
    """Outline, road network and zones: structure, one colour and one weight
    each. Zones are drawn the same however they are claimed; who holds what is
    read off the machines and the road they have been granted."""
    outline = box(BARN_X0, BARN_Y0, BARN_X1, BARN_Y1)
    log_lines("barn/outline", [outline + outline[:1]], C_BARN, BARN_WIDTH, index)
    log_lines("barn/paths", graph_lines, C_GRAPH, 0.05, index)
    log_lines("barn/zones", [poly + poly[:1] for poly in zone_polys.values()], C_ZONE, ZONE_WIDTH, index)


def outline(at, yaw, length, width):
    """A machine as a footprint plus a nose spur, so heading reads at a glance."""
    c, s = math.cos(yaw), math.sin(yaw)
    place = lambda lx, ly: (at[0] + c * lx - s * ly, at[1] + s * lx + c * ly, 0.0)  # noqa: E731
    hl, hw = length / 2.0, width / 2.0
    body = [place(hl, hw), place(hl, -hw), place(-hl, -hw), place(-hl, hw), place(hl, hw)]
    nose = [place(hl, 0.0), place(hl + length * 0.45, 0.0)]
    return body, nose


def draw(snapshot, index, node_place, trails):
    """The fleet as the server reports it: every machine, the road it holds in
    its own colour, and where it has been."""
    held_points = {}
    for request in snapshot["requests"]:
        for target in request["targets"]:
            if target["kind"] == "Node" and target["resource_id"] in node_place:
                held_points.setdefault(request["robot_id"], []).append(node_place[target["resource_id"]])

    bodies, noses, colors, geo, granted, own, trail_lines, trail_colors = [], [], [], [], [], [], [], []
    for state in snapshot["robots"]:
        position = state.get("position")
        if not position:
            continue
        robot = state["robot_id"]
        at = (position["x"], position["y"])
        yaw = (state.get("heading") or {}).get("yaw_rad", 0.0)
        body, nose = outline(at, yaw, MACHINE_LENGTH, MACHINE_BODY)
        bodies.append(body)
        noses.append(nose)
        colors.append(colour_of(robot))
        geo.append(lat_lon(index, *at))
        trail = trails.setdefault(robot, [])
        if not trail or math.dist(trail[-1], at) > 0.05:
            trail.append(at)
            del trail[:-1200]
        if len(trail) >= 2:
            trail_lines.append([(x, y, 0.0) for x, y in trail])
            trail_colors.append(scaled(colour_of(robot), 0.45))
        if len(held_points.get(robot, [])) >= 2:
            granted.append([(x, y, 0.0) for x, y in held_points[robot]])
            own.append(colour_of(robot))
    rr.log("enu/machines/body", rr.LineStrips3D(
        bodies, colors=colors, radii=[MACHINE_BODY * 0.04] * len(bodies)), static=True)
    rr.log("enu/machines/heading", rr.LineStrips3D(
        noses, colors=colors, radii=[MACHINE_BODY * 0.06] * len(noses)), static=True)
    rr.log("wgs/machines", rr.GeoPoints(lat_lon=geo, colors=colors, radii=[3.0] * len(geo)), static=True)
    rr.log("enu/machines/granted", rr.LineStrips3D(granted, colors=own, radii=[0.13] * len(granted)), static=True)
    rr.log("enu/machines/trail", rr.LineStrips3D(trail_lines, colors=trail_colors,
                                                 radii=[0.05] * len(trail_lines)), static=True)


# --------------------------------------------------------------------- main


def main():
    listen = urllib.parse.urlparse(SERVER_URL)
    server = Server()
    process = start_server(f"0.0.0.0:{listen.port or 8080}")
    try:
        for _ in range(150):
            try:
                server.health()
                break
            except Exception:  # noqa: BLE001 — not up yet
                if process.poll() is not None:
                    print("the server exited before it came up")
                    return 1
                time.sleep(0.2)

        workspace = build_workspace()
        document = workspace.to_json()
        DEMARKE_JSON.write_text(document, encoding="utf-8")
        accepted = server.push_workspace(document)
        index = syncbot.WorkspaceIndex(workspace)
        print(f"syncbot at {SERVER_URL}: barn loaded — {accepted['nodes']} nodes, "
              f"{accepted['zones']} zones — written to {DEMARKE_JSON.name}", flush=True)

        node_place = {n["id"]: (n["x"], n["y"]) for n in index.nodes()}
        zone_polys = {z["id"]: [(p["x"], p["y"]) if isinstance(p, dict) else tuple(p[:2]) for p in z["boundary"]]
                      for z in index.zones() if z["boundary"]}
        graph_lines = [[node_place[e["source"]], node_place[e["target"]]] for e in index.edges()]
        connect("syncbot_server")
        log_static(index, zone_polys, graph_lines)
        print("  drawing the fleet — Ctrl-C stops the server\n", flush=True)

        trails = {}
        while process.poll() is None:
            draw(server.snapshot(), index, node_place, trails)
            time.sleep(1.0 / RATE_HZ)
        print("the server exited")
        return process.returncode or 1
    except KeyboardInterrupt:
        return 0
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(5.0)
            except subprocess.TimeoutExpired:
                process.kill()


if __name__ == "__main__":
    sys.exit(main())
