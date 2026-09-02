#!/usr/bin/env python3
"""Toy newline-JSON adapter implemented out of process in Python.

Input is one JSON object per line. The adapter encodes the frozen ARES datapod
schema itself, calls the peerbus core, and prints the decoded reply. This file
does not import syncbot and the running core is not rebuilt when it is added.

Run `python3 adapter.py --self-check` to verify the packed header layouts still
match the sizes the core pins in `canonical_header_sizes_are_frozen`.
"""

from __future__ import annotations

import json
import struct
import sys
from typing import Any

# Optional at import time so --self-check runs anywhere; required to serve.
try:
    import datapod
    import peerbus
except ImportError:  # pragma: no cover
    datapod = None
    peerbus = None


CORE = "ares-core"
TOPICS = {
    "register": "ares/v1/robots/register",
    "heartbeat": "ares/v1/robots/heartbeat",
    "claim_zone": "ares/v1/claims/zone",
    "claim_node": "ares/v1/claims/node",
    "claim_edge": "ares/v1/claims/edge",
    "claim_route": "ares/v1/claims/route",
    "release_zone": "ares/v1/leases/release/zone",
    "release_node": "ares/v1/leases/release/node",
    "release_edge": "ares/v1/leases/release/edge",
}

# Header layouts, then the (offset, length) pair each payload section adds.
# Sizes are pinned on the Rust side by canonical_header_sizes_are_frozen.
REGISTER_HEADER = "<QB7xIIII"
HEARTBEAT_HEADER = "<qQQddddBBBBB3xIIII"
CLAIM_HEADER = "<QBBB5xIIIIII"
CLAIM_ROUTE_HEADER = "<QBBB5xIIIIIIII"
RELEASE_HEADER = "<QIIII"
REPLY_WIRE = "<QBBB5x"

HEADER_SIZES = {
    "ares.v1.register": (REGISTER_HEADER, 32),
    "ares.v1.heartbeat": (HEARTBEAT_HEADER, 80),
    "ares.v1.claim": (CLAIM_HEADER, 40),
    "ares.v1.claim.route": (CLAIM_ROUTE_HEADER, 48),
    "ares.v1.release": (RELEASE_HEADER, 24),
    "ares.v1.reply": (REPLY_WIRE, 16),
}

POS_FRAME_NONE = 0
POS_FRAME_GLOBAL = 1
POS_FRAME_LOCAL = 2


def sections(*parts: bytes) -> tuple[list[int], bytes]:
    fields: list[int] = []
    payload = bytearray()
    for part in parts:
        fields.extend((len(payload), len(part)))
        payload.extend(part)
    return fields, bytes(payload)


def u64_section(values: Any) -> bytes:
    """A payload section of little-endian u64 ids."""
    return b"".join(struct.pack("<Q", int(v)) for v in values or [])


def position(request: dict[str, Any]) -> tuple[int, float, float, float]:
    """Which frame this heartbeat reports a position in, if any.

    Mirrors `FlatHeartbeat::position` on the Rust side: lat/lon or x/y, never
    both, and never half of one.
    """
    has_global = request.get("lat") is not None or request.get("lon") is not None
    has_local = request.get("x") is not None or request.get("y") is not None
    if has_global and has_local:
        raise ValueError("send either lat/lon or x/y, not both")
    if has_global:
        if request.get("lat") is None or request.get("lon") is None:
            raise ValueError("a global position needs both lat and lon")
        return (
            POS_FRAME_GLOBAL,
            float(request["lat"]),
            float(request["lon"]),
            float(request.get("alt") or 0.0),
        )
    if has_local:
        if request.get("x") is None or request.get("y") is None:
            raise ValueError("a local position needs both x and y")
        return (
            POS_FRAME_LOCAL,
            float(request["x"]),
            float(request["y"]),
            float(request.get("z") or 0.0),
        )
    return (POS_FRAME_NONE, 0.0, 0.0, 0.0)


def encode(request: dict[str, Any]) -> tuple[str, int, bytes]:
    op = str(request["op"])
    robot = str(request.get("robot", "")).encode()
    key = str(request.get("key", "")).encode()

    if op == "register":
        sec, payload = sections(robot, key)
        alive = request.get("alive")
        header = struct.pack(
            REGISTER_HEADER, int(alive or 0), int(alive is not None), *sec
        )
        name = "ares.v1.register"
    elif op == "heartbeat":
        sec, payload = sections(robot, key)
        zone, node, edge = (request.get(k) for k in ("zone", "node", "edge"))
        pos_frame, pos_a, pos_b, pos_c = position(request)
        yaw = request.get("yaw")
        header = struct.pack(
            HEARTBEAT_HEADER,
            int(zone or 0),
            int(node or 0),
            int(edge or 0),
            pos_a,
            pos_b,
            pos_c,
            float(yaw or 0.0),
            int(zone is not None),
            int(node is not None),
            int(edge is not None),
            pos_frame,
            int(yaw is not None),
            *sec,
        )
        name = "ares.v1.heartbeat"
    elif op == "claim_route":
        sec, payload = sections(
            robot,
            key,
            u64_section(request.get("node")),
            u64_section(request.get("edge")),
        )
        access = request.get("access_mode")
        lease = request.get("lease_time")
        header = struct.pack(
            CLAIM_ROUTE_HEADER,
            int(lease or 0),
            int(access or 0),
            int(access is not None),
            int(lease is not None),
            *sec,
        )
        name = "ares.v1.claim.route"
    elif op.startswith("claim_"):
        sec, payload = sections(robot, key, u64_section(request.get("id")))
        access = request.get("access_mode")
        lease = request.get("lease_time")
        header = struct.pack(
            CLAIM_HEADER,
            int(lease or 0),
            int(access or 0),
            int(access is not None),
            int(lease is not None),
            *sec,
        )
        name = "ares.v1.claim"
    elif op.startswith("release_"):
        sec, payload = sections(robot, key)
        header = struct.pack(RELEASE_HEADER, int(request["id"]), *sec)
        name = "ares.v1.release"
    else:
        raise ValueError(f"unknown op {op!r}")

    return TOPICS[op], datapod.type_hash_name(name), header + payload


def decode_reply(message: peerbus.DatapodMessage) -> dict[str, Any]:
    expected = datapod.type_hash_name("ares.v1.reply")
    if message.type_hash != expected:
        raise ValueError(f"unexpected reply type hash {message.type_hash}, expected {expected}")
    wire = bytes(message.wire)
    if len(wire) != 16:
        raise ValueError(f"invalid reply size {len(wire)}")
    blocked, decision, reason, has_blocked = struct.unpack(REPLY_WIRE, wire)
    reply: dict[str, Any] = {"decision": decision, "reason": reason}
    if has_blocked:
        reply["blocked"] = blocked
    return reply


def self_check() -> int:
    """Fail loudly if a packed header no longer matches the frozen schema."""
    failures = []
    for name, (layout, expected) in HEADER_SIZES.items():
        actual = struct.calcsize(layout)
        if actual != expected:
            failures.append(f"{name}: packs {actual} bytes, core expects {expected}")
    for failure in failures:
        print(f"SELF-CHECK FAIL: {failure}", file=sys.stderr)
    if failures:
        print(
            "the canonical datapod schema in src/wire/peerbus.rs has changed; "
            "update the header layouts above",
            file=sys.stderr,
        )
        return 1
    print(f"self-check ok: {len(HEADER_SIZES)} canonical header layouts match")
    return 0


def main() -> int:
    if "--self-check" in sys.argv[1:]:
        return self_check()
    if peerbus is None or datapod is None:
        print(
            "the peerbus and datapod Python bindings are required to serve; "
            "only --self-check works without them",
            file=sys.stderr,
        )
        return 1

    node = peerbus.Node(identity="ares-python-adapter", no_relay=True)
    clients: dict[str, peerbus.DatapodReqClient] = {}
    for line in sys.stdin:
        try:
            request = json.loads(line)
            topic, type_hash, wire = encode(request)
            client = clients.get(topic)
            if client is None:
                client = node.datapod_req_client(CORE, topic)
                clients[topic] = client
            reply = decode_reply(client.call_wire(type_hash, wire))
            print(json.dumps(reply), flush=True)
        except Exception as error:  # adapter boundary: errors are wire replies
            print(json.dumps({"error": str(error)}), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
