"""Four yard shuttles, one weighbridge, stepped in real time into Rerun.

The Python twin of ``examples/packhouse_yard.rs``. Same yard, same rules, same
picture — driven entirely through the ``syncbot`` Python bindings, so it also
serves as the working proof that the binding surface is complete enough to
build a workspace, plan over it, and run the claim loop without dropping to
Rust.

A produce packhouse in the Noordoostpolder at harvest. Shuttles haul full bins
from the two field-side buffers to the packhouse intakes and come back empty.
The yard's two halves are joined by a single-lane weighbridge — every loaded
shuttle must cross it, it fits one machine, and traffic runs both ways over
it. That one span is the whole reason this yard needs a coordinator.

The interesting claim is not the weighbridge *zone*. A shuttle claims the
nodes it stops at and the edges it crosses; the manager derives what that
implies for the zones those sit in. So two shuttles heading for different
intakes share the apron happily, while the bridge deck — one edge inside an
exclusive zone — admits exactly one.

From the library: workspace construction, policy-aware route planning, the
rolling-horizon claim request, claim evaluation, schedule decisions
(*Proceed | Queue | Replan*) and right-of-way arbitration. The shuttle state
machine is application logic and lives here.

Build the extension, start a viewer, then run::

    cd examples/python_binding && make
    rerun
    python3 packhouse_yard.py

``SPEEDUP=20`` to slow it down, ``SYNCBOT_VIZ_RRD=x.rrd`` to record headless.
"""
from __future__ import annotations

import math
import os
import time
from dataclasses import dataclass, field

import rerun as rr

import syncbot

# Packhouse yard, Noordoostpolder. Local ENU is metres off this point.
DATUM = (52.6619, 5.7482, 0.0)

SHUTTLE_SPEED = 2.6
SHUTTLE_LENGTH = 6.5
SHUTTLE_BODY = 2.4
HORIZON = 2

LOAD_SECONDS = 40.0
TIP_SECONDS = 55.0

TICK = 1.0
RUN_FOR = 2.0 * 3600.0

C_YARD = (0xE6, 0xE9, 0xDD)
C_FREE = (0x5A, 0x64, 0x73)
C_BRIDGE = (0xF2, 0xC1, 0x4E)
C_GRAPH = (0x78, 0x82, 0x96)
C_PLAN = (0xF2, 0xC1, 0x4E)
SHUTTLE_RGB = [
    (0x4F, 0xA3, 0xD1),
    (0xE0, 0x6C, 0x5F),
    (0x6F, 0xC2, 0x8B),
    (0xC2, 0x8B, 0xE0),
]


def scaled(rgb, factor):
    return tuple(min(255, max(0, round(c * factor))) for c in rgb)


def hms(seconds):
    total = int(seconds)
    return f"{total // 3600:02}:{total % 3600 // 60:02}:{total % 60:02}"


def box(min_x, min_y, max_x, max_y):
    return [(min_x, min_y), (max_x, min_y), (max_x, max_y), (min_x, max_y)]


# --------------------------------------------------------------------- yard

ZONES = [
    ("buffer_south", 1, box(10, 10, 60, 50), {"traffic.policy": "shared",
                                              "traffic.capacity": "2"}),
    ("buffer_north", 2, box(90, 10, 140, 50), {"traffic.policy": "shared",
                                               "traffic.capacity": "2"}),
    ("weighbridge", 3, box(60, 58, 90, 92), {"traffic.policy": "exclusive",
                                             "traffic.claim_required": "true"}),
    ("apron", 4, box(10, 95, 140, 135), {"traffic.policy": "shared",
                                         "traffic.capacity": "3"}),
    ("intake_1", 5, box(10, 140, 60, 175), {"traffic.policy": "exclusive",
                                            "traffic.claim_required": "true"}),
    ("intake_2", 6, box(90, 140, 140, 175), {"traffic.policy": "exclusive",
                                             "traffic.claim_required": "true"}),
]

PLACES = [
    ("buf_s", 1001, 35.0, 30.0),
    ("buf_n", 1002, 115.0, 30.0),
    ("weigh_in", 1003, 75.0, 62.0),
    ("weigh_out", 1004, 75.0, 88.0),
    ("apron_mid", 1005, 75.0, 115.0),
    ("intake_1", 1006, 35.0, 157.0),
    ("intake_2", 1007, 115.0, 157.0),
]

# The bridge deck weighs the same as any other link. It is scarce because of
# the zone it sits in, not because it is expensive to cross.
LINKS = [
    ("buf_s", "weigh_in", 34.0, 2001),
    ("buf_n", "weigh_in", 51.0, 2002),
    ("weigh_in", "weigh_out", 26.0, 2003),
    ("weigh_out", "apron_mid", 27.0, 2004),
    ("apron_mid", "intake_1", 55.0, 2005),
    ("apron_mid", "intake_2", 55.0, 2006),
]


class Yard:
    """The workspace plus the lookups the visualiser needs."""

    def __init__(self):
        builder = syncbot.WorkspaceBuilder(
            "packhouse_yard", box(0, 0, 150, 185), DATUM,
            properties={"traffic.policy": "shared"},
        )
        for name, numeric, boundary, props in ZONES:
            builder.add_zone(name, boundary, numeric_id=numeric, properties=props)
        for name, numeric, x, y in PLACES:
            builder.add_node(name, x, y, numeric_id=numeric)
        for source, target, weight, numeric in LINKS:
            builder.add_edge(source, target, weight=weight, numeric_id=numeric,
                             properties={"traffic.lane_type": "corridor"})

        self.index = syncbot.WorkspaceIndex(builder.build())
        self.datum = self.index.datum()
        self.nodes = {n["name"]: n for n in self.index.nodes()}
        self.node_id = {name: n["id"] for name, n in self.nodes.items()}
        self.place = {n["id"]: (n["x"], n["y"]) for n in self.nodes.values()}
        self.zones = [z for z in self.index.zones() if z["boundary"]]
        self.bridge = next(z["id"] for z in self.zones if z["name"] == "weighbridge")
        self.graph_lines = [
            [self.place[self.node_id[a]], self.place[self.node_id[b]]]
            for a, b, _, _ in LINKS
        ]

    def lat_lon(self, x, y):
        geo = self.index.local_to_global(x, y, 0.0)
        return (geo["lat"], geo["lon"])


# ------------------------------------------------------------------ shuttles


@dataclass
class Path:
    """A shuttle's position along the polyline its route plan traces."""

    points: list = field(default_factory=list)
    node_ids: list = field(default_factory=list)
    edge_ids: list = field(default_factory=list)
    travelled: float = 0.0

    @classmethod
    def from_plan(cls, plan, yard):
        return cls(
            points=[yard.place[n] for n in plan["traversed_node_ids"]],
            node_ids=list(plan["traversed_node_ids"]),
            edge_ids=list(plan["traversed_edge_ids"]),
        )

    def legs(self):
        return [math.dist(a, b) for a, b in zip(self.points, self.points[1:])]

    def total(self):
        return sum(self.legs())

    def distance_through(self, legs):
        """How far the shuttle may go before entering ground it has not claimed."""
        return sum(self.legs()[:legs])

    def done(self):
        return len(self.points) < 2 or self.travelled >= self.total() - 1e-6

    def node_index(self):
        walked = 0.0
        for i, leg in enumerate(self.legs()):
            if self.travelled < walked + leg - 1e-6:
                return i
            walked += leg
        return max(len(self.points) - 1, 0)

    def pose(self):
        if not self.points:
            return (0.0, 0.0), 0.0
        if len(self.points) == 1:
            return self.points[0], 0.0
        walked = 0.0
        legs = self.legs()
        for i, leg in enumerate(legs):
            if self.travelled <= walked + leg or i + 2 == len(self.points):
                a, b = self.points[i], self.points[i + 1]
                t = min(max((self.travelled - walked) / leg, 0.0), 1.0) if leg > 1e-9 else 0.0
                at = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t)
                return at, math.atan2(b[1] - a[1], b[0] - a[0])
            walked += leg
        return self.points[-1], 0.0


class Shuttle:
    def __init__(self, robot_id, name, home, yard):
        self.id = robot_id
        self.name = name
        self.home = home
        self.job = "loading"
        self.countdown = LOAD_SECONDS * (0.4 + 0.3 * robot_id)
        self.path = Path(points=[yard.place[yard.node_id[home]]])
        self.granted_legs = 0
        self.bins = 0
        self.waited = 0.0
        self.trail = []
        self.note = "waiting on a bin"

    @property
    def colour(self):
        return SHUTTLE_RGB[(self.id - 1) % len(SHUTTLE_RGB)]

    def at(self):
        return self.path.pose()[0]

    def remaining(self):
        return max(len(self.path.points) - self.path.node_index() - 1, 0)


JOB_LABEL = {
    "loading": "loading at the buffer",
    "hauling": "hauling to the intake",
    "tipping": "tipping",
    "returning": "returning empty",
}


# ------------------------------------------------------------ the library bits


def dispatch(coord, shuttle, yard, goal, tick):
    """Plan a route, hand it to the coordinator, and ask whether it clears.

    ``Queue`` and ``Replan`` are not failures — they are the coordinator
    saying *not yet* and *not this way*, which the shuttle obeys by starting
    with nothing granted.
    """
    start = nearest_node(yard, shuttle.at())
    result = syncbot.plan_route(yard.index, start, yard.node_id[goal], True)
    plan = result.get("plan")
    if not plan:
        shuttle.note = "no route — waiting"
        return

    shuttle.path = Path.from_plan(plan, yard)
    shuttle.granted_legs = 0
    coord.assign_route_plan(shuttle.id, plan, HORIZON, tick)

    decision = coord.schedule_robot_route(shuttle.id, shuttle.id, tick, 1.0, "exclusive")
    kind = decision["kind"]
    if kind == "Proceed":
        shuttle.note = f"cleared for {goal}"
    elif kind == "Queue":
        shuttle.note = f"queued for {goal}, position {decision['queue_position']}"
    else:
        shuttle.note = f"replan for {goal}, {len(decision['conflicts'])} conflicts"
    log_note(shuttle)


def try_extend(coord, shuttle, tick):
    """Ask for the next slice of the route.

    This is the rolling horizon: the request only ever covers the next
    ``HORIZON`` steps, and the upsert replaces the previous slice — so moving
    forward is what releases the ground behind.

    Returns ``"granted"``, ``"denied"`` or ``"complete"``.
    """
    request = coord.claim_request_for_robot(shuttle.id, shuttle.id, "exclusive")
    if not request["targets"]:
        shuttle.granted_legs = max(len(shuttle.path.points) - 1, 0)
        return "complete"

    if coord.evaluate_claim(request)["decision"] != "Grant":
        return "denied"

    edges = sum(1 for t in request["targets"] if t["kind"] == "Edge")
    coord.upsert_claim_request_for_robot(request)
    coord.update_robot_claim_state(shuttle.id, [request["id"]], [], tick)

    was = shuttle.granted_legs
    reached = shuttle.path.node_index()
    shuttle.granted_legs = min(reached + edges, len(shuttle.path.points) - 1)
    if shuttle.granted_legs > was:
        shuttle.note = f"granted {shuttle.granted_legs - was} more leg(s)"
        log_note(shuttle)
    return "granted"


def report_progress(coord, shuttle, node_index, tick):
    """Say which node the shuttle reached, then drop what it has driven past."""
    node = shuttle.path.node_ids[node_index] if node_index < len(shuttle.path.node_ids) else None
    edge = shuttle.path.edge_ids[node_index - 1] if 0 < node_index <= len(shuttle.path.edge_ids) else None
    coord.update_robot_progress(shuttle.id, node, edge, tick)
    coord.release_behind_progress(shuttle.id)


def arrive(coord, shuttle, tick, counters):
    """End of a run: tip a bin, or park at the buffer for the next one."""
    if shuttle.job == "hauling":
        shuttle.job = "tipping"
        shuttle.countdown = TIP_SECONDS
        shuttle.bins += 1
        counters["delivered"] += 1
        shuttle.note = "tipping into the intake"
    elif shuttle.job == "returning":
        shuttle.job = "loading"
        shuttle.countdown = LOAD_SECONDS
        shuttle.note = "waiting on a bin"
    else:
        return
    # Nothing is being driven any more, so the claim goes back whole.
    coord.remove_claim_requests_for_robot(shuttle.id)
    coord.update_robot_progress(shuttle.id, None, None, tick)
    log_note(shuttle)


def yield_check(coord, shuttles, me):
    """Would this shuttle lose a right-of-way call against anyone ahead?

    The coordinator has already refused the claim; this only decides how the
    hold is described. A shuttle that yields on merit reads differently from
    one that simply arrived second.
    """
    mine = coord.robot_state(me.id)
    if mine is None:
        return False
    for other in shuttles:
        if other.id == me.id:
            continue
        theirs = coord.robot_state(other.id)
        if theirs is None:
            continue
        decision = syncbot.arbitrate_right_of_way(
            self_state=mine["progress_state"].lower(),
            other_state=theirs["progress_state"].lower(),
            self_wait_ticks=mine["wait_ticks"],
            other_wait_ticks=theirs["wait_ticks"],
            self_remaining_steps=me.remaining(),
            other_remaining_steps=other.remaining(),
        )
        if decision == "yield":
            return True
    return False


def free_intake(shuttles, me, yard):
    """Pick the intake fewest others are committed to, so the fleet spreads."""

    def committed(name):
        goal = yard.node_id[name]
        return sum(
            1
            for s in shuttles
            if s.id != me.id
            and s.job in ("hauling", "tipping")
            and s.path.node_ids
            and s.path.node_ids[-1] == goal
        )

    return "intake_1" if committed("intake_1") <= committed("intake_2") else "intake_2"


def nearest_node(yard, at):
    return min(yard.place, key=lambda nid: math.dist(yard.place[nid], at))


def claimed_zones(yard, coord):
    """Which zone each live claim implies intent on, read out of the ledger."""
    held = {}
    for request in coord.claim_manager_requests():
        robot = request["robot_id"]
        for target in request["targets"]:
            kind, rid = target["kind"], target["resource_id"]
            if kind == "Zone":
                zones = [rid]
            elif kind == "Node":
                zones = yard.index.zones_of_node(rid)
            else:
                zones = yard.index.zones_of_edge(rid)
            for zone in zones:
                held.setdefault(zone, robot)
    return held


# --------------------------------------------------------------------- rerun


def connect(app_id):
    """Record to a file when asked, otherwise attach to or spawn a viewer."""
    rrd = os.environ.get("SYNCBOT_VIZ_RRD")
    if rrd:
        rr.init(app_id)
        rr.save(rrd)
        return
    url = os.environ.get("RERUN_URL")
    rr.init(app_id, spawn=not url)
    if url:
        rr.connect_grpc(url)


def log_polygon(path, points, yard, color, radius):
    strip = list(points) + [points[0]]
    rr.log(f"enu/{path}", rr.LineStrips3D([[(x, y, 0.0) for x, y in strip]],
                                          colors=[color], radii=[radius]))
    rr.log(f"wgs/{path}", rr.GeoLineStrings(
        lat_lon=[[yard.lat_lon(x, y) for x, y in strip]], colors=[color]))


def log_polylines(path, lines, yard, color, radius, geo=True):
    lines = [line for line in lines if len(line) >= 2]
    if not lines:
        return
    rr.log(f"enu/{path}", rr.LineStrips3D(
        [[(x, y, 0.0) for x, y in line] for line in lines],
        colors=[color] * len(lines), radii=[radius] * len(lines)))
    if geo:
        rr.log(f"wgs/{path}", rr.GeoLineStrings(
            lat_lon=[[yard.lat_lon(x, y) for x, y in line] for line in lines],
            colors=[color] * len(lines)))


def log_machine(path, at, yaw, color, length, width):
    """A machine as a footprint plus a nose spur, so heading reads at a glance."""
    cos, sin = math.cos(yaw), math.sin(yaw)
    cx, cy = at

    def place(lx, ly):
        return (cx + cos * lx - sin * ly, cy + sin * lx + cos * ly, 0.0)

    hl, hw = length * 0.5, width * 0.5
    body = [place(hl, hw), place(hl, -hw), place(-hl, -hw), place(-hl, hw), place(hl, hw)]
    rr.log(f"{path}/body", rr.LineStrips3D([body], colors=[color],
                                           radii=[width * 0.09]))
    nose = [place(hl, 0.0), place(hl + length * 0.45, 0.0)]
    rr.log(f"{path}/heading", rr.LineStrips3D([nose], colors=[color],
                                              radii=[width * 0.12]))


def log_static_yard(yard):
    """Yard outline and the road graph — logged once, they do not move."""
    log_polygon("yard/outline", box(0, 0, 150, 185), yard, C_YARD, 0.6)
    log_polylines("yard/graph", yard.graph_lines, yard, C_GRAPH, 0.35)

    places = [(n["x"], n["y"]) for n in yard.nodes.values()]
    labels = [n["name"] for n in yard.nodes.values()]
    rr.log("enu/yard/nodes", rr.Points3D([(x, y, 0.0) for x, y in places],
                                         colors=[C_GRAPH] * len(places),
                                         radii=[1.1] * len(places), labels=labels))
    rr.log("wgs/yard/nodes", rr.GeoPoints(
        lat_lon=[yard.lat_lon(x, y) for x, y in places],
        colors=[C_GRAPH] * len(places), radii=[2.5] * len(places)))


def log_zone_state(yard, claimed):
    """Zones tinted by who has intent on them.

    The colour comes from the claim ledger, not from where the shuttles happen
    to be — that is the point of deriving zone intent from node and edge
    claims.
    """
    for zone in yard.zones:
        robot = claimed.get(zone["id"])
        if robot is not None:
            color, radius = SHUTTLE_RGB[(robot - 1) % len(SHUTTLE_RGB)], 1.5
        elif zone["policy"] and zone["policy"]["kind"] == "ExclusiveAccess":
            color, radius = scaled(C_BRIDGE, 0.45), 0.6
        else:
            color, radius = C_FREE, 0.5
        log_polygon(f"yard/zones/{zone['name']}", zone["boundary"], yard, color, radius)


def log_shuttle(yard, shuttle):
    at, yaw = shuttle.path.pose()
    color = shuttle.colour
    base = f"enu/shuttle/{shuttle.name}"

    log_machine(base, at, yaw, color, SHUTTLE_LENGTH, SHUTTLE_BODY)
    rr.log(f"wgs/shuttle/{shuttle.name}", rr.GeoPoints(
        lat_lon=[yard.lat_lon(*at)], colors=[color], radii=[3.0]))

    # The granted part of the plan is drawn bright and the ungranted tail dim,
    # so a shuttle held at its claim boundary is a short bright stub with a
    # long dark road ahead of it.
    if len(shuttle.path.points) >= 2:
        cut = min(shuttle.granted_legs + 1, len(shuttle.path.points))
        granted = shuttle.path.points[:cut]
        pending = shuttle.path.points[max(cut - 1, 0):]
        log_polylines(f"shuttle/{shuttle.name}/granted", [granted], yard, C_PLAN, 0.5)
        log_polylines(f"shuttle/{shuttle.name}/pending", [pending], yard,
                      scaled(C_PLAN, 0.35), 0.3, geo=False)

    if len(shuttle.trail) >= 2:
        log_polylines(f"shuttle/{shuttle.name}/trail", [shuttle.trail], yard,
                      scaled(color, 0.5), 0.18, geo=False)


def log_note(shuttle):
    rr.log(f"log/{shuttle.name}",
           rr.TextLog(f"{shuttle.name} — {shuttle.note} ({JOB_LABEL[shuttle.job]})"))


# ---------------------------------------------------------------------- main


def main():
    speedup = float(os.environ.get("SPEEDUP", "120"))

    yard = Yard()
    connect("syncbot_packhouse_yard_py")

    coord = syncbot.Coordinator(yard.index)
    shuttles = [
        Shuttle(1, "shuttle_1", "buf_s", yard),
        Shuttle(2, "shuttle_2", "buf_n", yard),
        Shuttle(3, "shuttle_3", "buf_s", yard),
        Shuttle(4, "shuttle_4", "buf_n", yard),
    ]
    for shuttle in shuttles:
        coord.register_robot({"robot_id": shuttle.id, "horizon": HORIZON})

    print(f"packhouse yard, Noordoostpolder — {len(shuttles)} shuttles (python)")
    print("  one single-lane weighbridge joins the two halves of the yard")
    print("  claims are on nodes and edges; zone intent is derived from them")
    print(f"  running at {speedup:.0f}x real time\n")

    log_static_yard(yard)

    clock = 0.0
    counters = {"delivered": 0}
    bridge_busy = 0.0

    while clock < RUN_FOR:
        tick = int(clock)
        blocked_now = 0

        for i, shuttle in enumerate(shuttles):
            if shuttle.job in ("loading", "tipping"):
                shuttle.countdown -= TICK
                if shuttle.countdown <= 0.0:
                    if shuttle.job == "loading":
                        goal = free_intake(shuttles, shuttle, yard)
                        dispatch(coord, shuttle, yard, goal, tick)
                        shuttle.job = "hauling"
                    else:
                        dispatch(coord, shuttle, yard, shuttle.home, tick)
                        shuttle.job = "returning"
                continue

            allowed = shuttle.path.distance_through(shuttle.granted_legs)
            if shuttle.path.travelled >= allowed - 1e-6 and not shuttle.path.done():
                if try_extend(coord, shuttle, tick) == "denied":
                    blocked_now += 1
                    shuttle.waited += TICK
                    shuttle.note = (
                        "yielding — another machine holds the ground ahead"
                        if yield_check(coord, shuttles, shuttle)
                        else "held at the claim boundary"
                    )

            allowed = shuttle.path.distance_through(shuttle.granted_legs)
            before = shuttle.path.node_index()
            step = min(SHUTTLE_SPEED * TICK, allowed - shuttle.path.travelled)
            if step > 0.0:
                shuttle.path.travelled += step
                shuttle.trail.append(shuttle.at())
            after = shuttle.path.node_index()
            if after != before:
                report_progress(coord, shuttle, after, tick)

            if shuttle.path.done():
                arrive(coord, shuttle, tick, counters)

        # --- log the tick
        claimed = claimed_zones(yard, coord)
        if yard.bridge in claimed:
            bridge_busy += TICK

        rr.set_time("sim", duration=clock)
        log_zone_state(yard, claimed)
        for shuttle in shuttles:
            log_shuttle(yard, shuttle)
        rr.log("stats/bins_delivered", rr.Scalars(counters["delivered"]))
        rr.log("stats/shuttles_held", rr.Scalars(blocked_now))
        rr.log("stats/claims_active", rr.Scalars(coord.claim_request_count()))

        if int(clock) % 600 == 0:
            print(f"  {hms(clock)}  {counters['delivered']} bins in   "
                  f"{blocked_now} held   {coord.claim_request_count()} claims live")

        clock += TICK
        if speedup > 0.0:
            time.sleep(TICK / speedup)

    print(f"\n{hms(clock)} simulated, {counters['delivered']} bins delivered")
    for shuttle in shuttles:
        print(f"  {shuttle.name:<10} {shuttle.bins:>3} bins, "
              f"{shuttle.waited:>5.0f}s held at a claim boundary")
    print(f"\nweighbridge claimed {100.0 * bridge_busy / clock:.0f}% of the run — "
          "the yard is bridge-limited, and")
    print("a fifth shuttle would add waiting, not bins. That is the finding: the")
    print("coordinator does not create the bottleneck, it makes it measurable.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
