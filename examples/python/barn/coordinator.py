"""3. Drive the machines standing in the barn, as clients of the syncbot server.

    python3 coordinator.py --rolling     # claim about 20 m of road ahead and roll it forward
    python3 coordinator.py --whole       # claim the whole route before setting off

`server.py` first, then `scene.py`, then this. It finds whatever machines
Gearbox is reporting, registers each with the server, heartbeats its position
and attitude ten times a second, asks the server for routes, claims stretches
of road and gives them back — nothing a real robot's client would not do. The
server holds all the state and draws it; this only steers.

    --work SECONDS    how long a machine works at a stand before its next job
    --window METRES   road claimed ahead in --rolling
    --speed M/S       cruise speed
    --debug           trace every claim and twist
"""
from __future__ import annotations

import argparse
import bisect
import json
import math
import os
import pathlib
import random
import sys
import threading
import time
import urllib.request

import cbor2
import zenoh

DEMARKE_JSON = pathlib.Path(__file__).resolve().parent / "demarke.json"
SERVER_URL = os.environ.get("SYNCBOT_URL", "http://127.0.0.1:8080")
ROBOT_KEY = os.environ.get("SYNCBOT_KEY", "0")

CONTROL_HZ = 10.0
W_MAX = 1.2
LOOKAHEAD = 0.9
ARRIVE_TOL = 0.30
# A machine turns on the spot before it drives off when it points this far
# from where the route goes, so it does not swing wide out of its lane.
TURN_FIRST_RAD = math.radians(50.0)
# Rolling mode asks for the next window once this much of the held road is
# left; a refused machine asks again this often.
REFILL_M = 8.0
RETRY_S = 0.5
TRIES = 4


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--rolling", action="store_const", dest="mode", const="rolling")
    mode.add_argument("--whole", action="store_const", dest="mode", const="whole")
    parser.add_argument("--work", type=float, default=8.0, metavar="SECONDS")
    parser.add_argument("--window", type=float, default=20.0, metavar="METRES")
    parser.add_argument("--speed", type=float, default=1.2, metavar="M/S")
    parser.add_argument("--debug", action="store_true")
    parser.set_defaults(mode="rolling")
    return parser.parse_args()


ARGS = parse_args()


def trace(message):
    if ARGS.debug:
        print(f"    {message}", flush=True)


def wrap(a):
    return (a + math.pi) % (2.0 * math.pi) - math.pi


# ----------------------------------------------------------------- the barn


class Barn:
    """The workspace as `server.py` wrote it: nodes with numeric ids and ENU
    places, edges, and which nodes are stands. Read from the document rather
    than rebuilt, so ids agree with the server's."""

    def __init__(self, document):
        self.place, self.numeric, self.stand = {}, {}, {}
        for uid, node in document["nodes"].items():
            self.place[uid] = (node["latlon"]["lon"], node["latlon"]["lat"])
            self.numeric[uid] = int(node["properties"]["external.numeric_id"])
            stand = node.get("properties", {}).get("barn.stand")
            if stand:
                self.stand[stand] = uid
        self.edge_numeric = {uid: int(e["properties"]["external.numeric_id"])
                             for uid, e in document["edges"].items()}

    def nearest(self, at):
        return min(self.place, key=lambda uid: math.dist(self.place[uid], at))


class Path:
    """Position along the polyline a route traces, as arc length."""

    def __init__(self, points, node_ids=(), edge_ids=()):
        self.points, self.node_ids, self.edge_ids = list(points), list(node_ids), list(edge_ids)
        self.travelled = 0.0
        self.cum = [0.0]
        for a, b in zip(self.points, self.points[1:]):
            self.cum.append(self.cum[-1] + math.dist(a, b))

    def total(self):
        return self.cum[-1]

    def through(self, legs):
        return self.cum[min(legs, len(self.cum) - 1)]

    def node_index(self):
        if len(self.points) < 2:
            return 0
        return min(max(bisect.bisect_right(self.cum, self.travelled + 1e-9) - 1, 0), len(self.points) - 1)

    def project(self, at):
        """Arc length of the point on the route nearest `at`, never backwards."""
        if len(self.points) < 2:
            return 0.0
        start = max(self.node_index() - 1, 0)
        best, best_d = self.travelled, float("inf")
        for i in range(start, min(start + 8, len(self.points) - 1)):
            (ax, ay), (bx, by) = self.points[i], self.points[i + 1]
            dx, dy = bx - ax, by - ay
            leg2 = dx * dx + dy * dy
            t = 0.0 if leg2 < 1e-12 else min(max(((at[0] - ax) * dx + (at[1] - ay) * dy) / leg2, 0.0), 1.0)
            d = math.hypot(at[0] - (ax + dx * t), at[1] - (ay + dy * t))
            if d < best_d:
                best_d, best = d, self.cum[i] + math.sqrt(leg2) * t
        return max(best, self.travelled) if best_d < 3.0 else self.travelled

    def point_at(self, s):
        if s <= 0.0 or len(self.points) < 2:
            return self.points[0]
        if s >= self.cum[-1]:
            return self.points[-1]
        i = bisect.bisect_right(self.cum, s) - 1
        leg = self.cum[i + 1] - self.cum[i]
        t = (s - self.cum[i]) / leg if leg > 1e-9 else 0.0
        (ax, ay), (bx, by) = self.points[i], self.points[i + 1]
        return (ax + (bx - ax) * t, ay + (by - ay) * t)


# -------------------------------------------------------------- the server


class Denied(Exception):
    REASON = {0: "ok", 1: "mismatched key", 2: "conflict / not registered",
              3: "capacity / bad id", 4: "unknown resource", 5: "bad request"}

    def __init__(self, reply):
        self.reply = reply
        blocked = reply.get("blocked")
        super().__init__(self.REASON.get(reply.get("reason"), str(reply.get("reason")))
                         + (f", blocked on {blocked}" if blocked is not None else ""))


class Server:
    """A robot's client for the syncbot REST API. Robots are numbers to the
    server; every call carries the shared bench key."""

    def __init__(self, url=SERVER_URL):
        self.base = url.rstrip("/") + "/ares/v1"

    def _call(self, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(f"{self.base}{path}", data=data, method=method,
                                         headers={"content-type": "application/json"})
        with urllib.request.urlopen(request, timeout=5.0) as response:
            raw = response.read()
        return json.loads(raw) if raw else None

    def _ask(self, path, body):
        reply = self._call("POST", path, body)
        if reply.get("decision") != 1:
            raise Denied(reply)
        return reply

    def health(self):
        return self._call("GET", "/health")

    def register(self, robot):
        try:
            return self._ask("/robots/register", {"robot": str(robot), "key": ROBOT_KEY})
        except Denied as denied:
            if denied.reply.get("reason") != 2:  # already registered by an earlier run: fine
                raise

    def heartbeat(self, robot, x, y, yaw, roll=None, pitch=None, node=None):
        """REP-103 in ENU: metres east/north, yaw counter-clockwise from east,
        roll about the forward axis, pitch about the left axis."""
        body = {"key": ROBOT_KEY, "x": float(x), "y": float(y), "z": 0.0, "yaw": float(yaw)}
        for name, value in (("roll", roll), ("pitch", pitch), ("node", node)):
            if value is not None:
                body[name] = value
        return self._ask(f"/robots/{robot}/heartbeat", body)

    def plan(self, start_numeric, goal_numeric, around_claims=False):
        """Shortest route, or with `around_claims` the route that steers clear
        of road other robots hold — longer, but one that can be granted."""
        reply = self._call("POST", "/routes/plan", {"start_node_id": str(start_numeric),
                                                    "goal_node_id": str(goal_numeric),
                                                    "use_penalties": bool(around_claims)})
        return reply.get("plan") if reply.get("found") else None

    def claim_route(self, robot, nodes, edges):
        """One claim over a stretch of road, all of it or nothing, held until released."""
        return self._ask("/claims/route", {"robot": str(robot), "key": ROBOT_KEY, "node": nodes,
                                           "edge": edges, "access_mode": 1, "lease_time": 0})

    def claim_node(self, robot, node):
        return self._ask("/claims/node", {"robot": str(robot), "key": ROBOT_KEY, "id": [node],
                                          "access_mode": 1, "lease_time": 0})

    def release(self, kind, robot, resource):
        """Give back the claim holding this resource — the whole claim, not one target."""
        return self._ask(f"/leases/release/{kind}", {"robot": str(robot), "key": ROBOT_KEY, "id": resource})


# ----------------------------------------------------------- the simulator


class Gearbox:
    """Gearbox's world is ENU here (scene.py placed the barn that way): its x
    is east, its z is south, and its heading — zero along +z, turning towards
    +x — is the ENU yaw plus a quarter turn."""

    def __init__(self):
        opened = zenoh.open(zenoh.Config())
        self.session = opened.wait() if hasattr(opened, "wait") else opened
        self.states, self.lock, self._subs = {}, threading.Lock(), []

    def put(self, key, payload):
        self.session.put(key, cbor2.dumps(payload), congestion_control=zenoh.CongestionControl.BLOCK)

    def _remember(self, namespace, sample):
        with self.lock:
            self.states[namespace] = cbor2.loads(bytes(sample.payload))

    def watch(self, namespace):
        self._subs.append(self.session.declare_subscriber(
            f"gearbox/machines/{namespace}/state", lambda s, ns=namespace: self._remember(ns, s)))

    def discover(self, timeout=8.0, settle=1.0):
        """Every machine reporting right now. A fresh zenoh session takes a
        moment to find its peers, so wait for the first, then a little more."""
        sub = self.session.declare_subscriber(
            "gearbox/machines/*/state", lambda s: self._remember(str(s.key_expr).split("/")[2], s))
        deadline = time.time() + timeout
        while time.time() < deadline and not self.states:
            time.sleep(0.1)
        time.sleep(settle)
        sub.undeclare()
        with self.lock:
            return sorted(self.states, key=lambda ns: (len(ns), ns))

    def pose(self, namespace):
        """ENU (x, y), yaw, roll, pitch — or None before the machine reports."""
        with self.lock:
            state = self.states.get(namespace)
        if not state:
            return None
        px, _py, pz = state["position"]
        return ((px, -pz), wrap(state["heading_rad"] - math.pi / 2.0),
                state.get("roll_rad"), state.get("pitch_rad"))

    def claim(self, namespace, session_id):
        self.put(f"gearbox/machines/{namespace}/session", {"session_id": session_id})

    def drive(self, namespace, session_id, v, w):
        self.put(f"gearbox/machines/{namespace}/cmd_vel",
                 {"linear": [float(v), 0.0, 0.0], "angular": [0.0, 0.0, float(w)], "session_id": session_id})

    def close(self):
        for sub in self._subs:
            sub.undeclare()
        self.session.close()


# ---------------------------------------------------------------- the robot


class Robot:
    def __init__(self, robot_id, namespace, at_stand, pose):
        self.id, self.namespace, self.at_stand = robot_id, namespace, at_stand
        self.session_id = f"syncbot_{namespace}_{int(time.time())}"
        self.pose = pose
        self.job, self.goal, self.countdown, self.trips = "working", None, 0.0, 0
        self.path = Path([pose[0]])
        self.granted_legs = 0
        # Claims held on the server, oldest first: (first node numeric id, index
        # of the last node covered). Releasing any one target releases the whole
        # claim, so these are given back as units.
        self.windows = []
        self.next_ask = 0.0

    @property
    def at(self):
        return self.pose[0]


def next_job(robot, robots, barn, rng):
    """Somewhere it is not and nobody else is heading for — preferably far."""
    taken = {r.goal for r in robots if r is not robot and r.goal}
    pool = [s for s in barn.stand if s != robot.at_stand and s not in taken] or \
           [s for s in barn.stand if s != robot.at_stand]
    pool.sort(key=lambda s: -math.dist(barn.place[barn.stand[s]], robot.at))
    return rng.choice(pool[:max(len(pool) // 2, 1)])


def window_end(robot, start):
    cum = robot.path.cum
    end = start
    while end + 1 < len(cum) and cum[end + 1] - cum[start] <= ARGS.window:
        end += 1
    return max(end, min(start + 1, len(cum) - 1))


def claim_stretch(server, barn, robot, first, last):
    nodes = [barn.numeric[u] for u in robot.path.node_ids[first:last + 1]]
    edges = [barn.edge_numeric[u] for u in robot.path.edge_ids[first:last]]
    server.claim_route(robot.id, nodes, edges)
    robot.windows.append((nodes[0], last))
    robot.granted_legs = last
    return robot.path.through(last)


def release_passed(server, robot):
    here = robot.path.node_index()
    while len(robot.windows) > 1 and robot.windows[0][1] < here:
        first_node, _last = robot.windows.pop(0)
        try:
            server.release("node", robot.id, first_node)
        except Denied as denied:
            trace(f"{robot.namespace}: release {first_node}: {denied}")


def release_all(server, robot):
    for first_node, _last in robot.windows:
        try:
            server.release("node", robot.id, first_node)
        except Denied:
            pass
    robot.windows = []


def hold_here(server, barn, robot):
    """A parked machine still stands on a node; keep that one claimed."""
    node = barn.numeric[barn.nearest(robot.at)]
    try:
        server.claim_node(robot.id, node)
        robot.windows.append((node, 0))
    except Denied as denied:
        trace(f"{robot.namespace}: cannot hold {node}: {denied}")


def try_route(server, barn, robot, goal, around_claims):
    """Plan to `goal` and claim it (whole, or the first window). True if granted."""
    start = barn.numeric[barn.nearest(robot.at)]
    plan = server.plan(start, barn.numeric[barn.stand[goal]], around_claims)
    if not plan or len(plan["traversed_node_ids"]) < 2:
        return False
    robot.path = Path([barn.place[u] for u in plan["traversed_node_ids"]],
                      plan["traversed_node_ids"], plan["traversed_edge_ids"])
    robot.goal, robot.windows = goal, []
    try:
        last = len(robot.path.points) - 1 if ARGS.mode == "whole" else window_end(robot, 0)
        claim_stretch(server, barn, robot, 0, last)
    except Denied as denied:
        trace(f"{robot.namespace}: {goal}{' around claims' if around_claims else ''} refused: {denied}")
        robot.path, robot.goal = Path([robot.at]), None
        return False
    return True


def take_job(server, barn, robot, robots, rng):
    """Find a job the server will grant.

    The shortest route first. If somebody holds part of it — a parked machine
    on a one-way lane is a wall to everyone behind it — ask the planner for
    the route that keeps off held road instead, which is what the yard's twin
    lanes are for. Only then give up on that goal and try another.
    """
    for _ in range(TRIES):
        goal = next_job(robot, robots, barn, rng)
        if try_route(server, barn, robot, goal, False) or try_route(server, barn, robot, goal, True):
            robot.job = "driving"
            return True
    return False


def twist_towards(robot, allowed):
    """Follow the route up to `allowed` metres along it, slowing to a stop at
    the end of the held stretch; turn on the spot first if pointing away."""
    path = robot.path
    room = allowed - path.travelled
    if room <= 0.03:
        return 0.0, 0.0
    at, heading = robot.pose[0], robot.pose[1]
    target = path.point_at(min(path.travelled + LOOKAHEAD, allowed))
    error = wrap(math.atan2(target[1] - at[1], target[0] - at[0]) - heading)
    w = max(-W_MAX, min(W_MAX, 2.2 * error))
    if abs(error) > TURN_FIRST_RAD:
        return 0.0, w
    return min(ARGS.speed * max(math.cos(error), 0.0), 0.9 * room + 0.08), w


# --------------------------------------------------------------------- main


def main():
    rng = random.Random(7)
    server = Server()
    try:
        server.health()
    except Exception as error:  # noqa: BLE001 — any failure here means "no server"
        print(f"no syncbot server at {SERVER_URL}: {error}\n  run server.py first")
        return 1
    if not DEMARKE_JSON.exists():
        print(f"{DEMARKE_JSON.name} is missing — run server.py first, it writes it")
        return 1
    barn = Barn(json.loads(DEMARKE_JSON.read_text(encoding="utf-8")))

    link = Gearbox()
    namespaces = link.discover()
    if not namespaces:
        print("  no machines are reporting in Gearbox — run scene.py first")
        return 1
    robots = []
    for i, namespace in enumerate(namespaces):
        link.watch(namespace)
        pose = link.pose(namespace)
        home = min(barn.stand, key=lambda s: math.dist(barn.place[barn.stand[s]], pose[0]))
        robot = Robot(i + 1, namespace, home, pose)
        robot.countdown = ARGS.work * (0.3 + 0.3 * robot.id)
        server.register(robot.id)
        link.claim(namespace, robot.session_id)
        hold_here(server, barn, robot)
        robots.append(robot)
        print(f"  {namespace:<10} is robot {robot.id}, standing at {home}", flush=True)
    print(f"\n  {ARGS.mode} claims, {ARGS.work:.0f} s at each stand — Ctrl-C stops them\n", flush=True)

    dt = 1.0 / CONTROL_HZ
    t0 = time.monotonic()
    loops, trips = 0, 0
    try:
        while True:
            loop_started = time.monotonic()
            for robot in robots:
                robot.pose = link.pose(robot.namespace) or robot.pose
                (x, y), yaw, roll, pitch = robot.pose
                here = robot.path.node_index() if len(robot.path.points) > 1 else None
                node = barn.numeric[robot.path.node_ids[here]] if here is not None else None
                try:
                    server.heartbeat(robot.id, x, y, yaw, roll, pitch, node)
                except Denied as denied:
                    trace(f"{robot.namespace}: heartbeat: {denied}")

                if robot.job == "working":
                    link.drive(robot.namespace, robot.session_id, 0.0, 0.0)
                    robot.countdown -= dt
                    if robot.countdown > 0.0:
                        continue
                    release_all(server, robot)
                    if take_job(server, barn, robot, robots, rng):
                        trace(f"{robot.namespace}: -> {robot.goal}, {robot.path.total():.1f} m, "
                              f"held to leg {robot.granted_legs}")
                    else:
                        hold_here(server, barn, robot)
                        robot.countdown = 1.0
                    continue

                robot.path.travelled = robot.path.project(robot.at)
                release_passed(server, robot)
                allowed = robot.path.through(robot.granted_legs)
                if (ARGS.mode == "rolling" and allowed - robot.path.travelled < REFILL_M
                        and robot.granted_legs < len(robot.path.points) - 1
                        and time.monotonic() >= robot.next_ask):
                    try:
                        allowed = claim_stretch(server, barn, robot, robot.granted_legs,
                                                window_end(robot, robot.granted_legs))
                        trace(f"{robot.namespace}: next window to leg {robot.granted_legs}")
                    except Denied as denied:
                        robot.next_ask = time.monotonic() + RETRY_S
                        trace(f"{robot.namespace}: window refused: {denied}")

                if (robot.path.total() - robot.path.travelled < ARRIVE_TOL
                        and math.dist(robot.at, robot.path.points[-1]) < ARRIVE_TOL):
                    link.drive(robot.namespace, robot.session_id, 0.0, 0.0)
                    robot.at_stand, robot.goal, robot.job = robot.goal, None, "working"
                    robot.trips += 1
                    trips += 1
                    robot.countdown = ARGS.work
                    release_all(server, robot)
                    hold_here(server, barn, robot)
                    robot.path = Path([robot.at])
                    continue

                v, w = twist_towards(robot, allowed)
                link.drive(robot.namespace, robot.session_id, v, w)
                if loops % int(CONTROL_HZ) == 0:
                    trace(f"{robot.namespace}: s {robot.path.travelled:5.1f}/{allowed:5.1f} m  "
                          f"v {v:.2f} w {w:+.2f}  rpy {math.degrees(roll or 0):+.1f}/"
                          f"{math.degrees(pitch or 0):+.1f}/{math.degrees(yaw):+.0f}")

            loops += 1
            if loops % (int(CONTROL_HZ) * 30) == 0:
                elapsed = int(time.monotonic() - t0)
                print(f"  {elapsed // 60:02}:{elapsed % 60:02}  {trips:>3} jobs", flush=True)
            time.sleep(max(0.0, loop_started + dt - time.monotonic()))
    except KeyboardInterrupt:
        pass
    finally:
        for robot in robots:
            link.drive(robot.namespace, robot.session_id, 0.0, 0.0)
        time.sleep(0.2)
        link.close()
        print(f"\n{trips} jobs done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
