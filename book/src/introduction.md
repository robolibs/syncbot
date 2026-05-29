# timenav

`timenav` plans robot routes through a shared workspace, then turns those routes
into time-windowed reservations so multiple robots can use the same map without
colliding. It answers four questions for a fleet:

1. **Where can I go?** — policy-aware route planning over a workspace graph.
2. **Can I claim it?** — exclusive / shared claims on zones, nodes, and edges.
3. **When can I go?** — schedule a route into reservations: *Proceed · Queue · Replan*.
4. **Who goes first?** — right-of-way arbitration when robots conflict.

## One core, several doors

The coordination logic lives in one place. Everything else is a **transport** — a
thin adapter that translates an external protocol into calls on the core and
serialises the result back out. No transport reimplements planning, conflict
checks, capacity rules, or arbitration.

```text
                 ┌───────────────────────────────────────┐
                 │              timenav core             │
                 │  route · policy · claims · schedule   │
                 └───────────────────────────────────────┘
                    ▲          ▲          ▲          ▲
                    │          │          │          │
                REST/JSON   REST/XML    Zenoh     quicbit
                    │          │          │          │
               dashboards    PLCs    robots /    SHM / QUIC
               & tooling             bridges     event bus
```

Pick the door that matches the caller. The state machine and decisions behind
each are identical — only the encoding and transport differ.

## How to read this book

- **Core model** explains the resources you address and the decisions the core
  makes. Read it once; the transport pages assume it.
- **Wire transports** is one page per protocol. Each is self-contained: what it's
  for, how to enable it, the address space, a worked example, and its quirks.
- **In-process bindings** covers calling the core without a network (C ABI,
  Python).
- **Reference** collects the feature flags and the shared message types.

## Status

Pre-1.0 (`0.0.x`). Wire formats are not stable between minor releases.
