# scripts

External-client demos that hit syncbot over the network — no FFI,
no PyO3.

## rest_demo.py

Two-robot simulation against the REST adapter. Two phases:

- **Phase 1** — robots claim *different* zones in parallel: both granted.
- **Phase 2** — robots claim the *same* zone: one is granted, the other
  polls/waits until the first releases its lease, then succeeds.

### Run it

Terminal 1 — start the demo server (builds a workspace with three
exclusive zones `dock_a` / `dock_b` / `junction` and listens on
`127.0.0.1:8080`):

```sh
cargo run --example rest_server --features rest
```

Terminal 2 — drive the simulation:

```sh
python3 scripts/rest_demo.py
```

The script uses only Python's standard library (`urllib.request`).
No `pip install` needed.

### Expected output

```
connected to syncbot at http://127.0.0.1:8080/ares/v1
registering robots...
  registered robot 1 (robot_id=1)  HTTP 200
  registered robot 2 (robot_id=2)  HTTP 200

================================================================
 Phase 1 — different zones, no conflict
================================================================
  robot 1: GRANTED on dock_a (lease 111)
  robot 2: GRANTED on dock_b (lease 222)
  robot 1: released lease 111
  robot 2: released lease 222

================================================================
 Phase 2 — same zone, one waits
================================================================
  robot 1: GRANTED on junction (lease 333)
  robot 2: DENIED on junction — exclusive access conflict on zone …
  robot 2: waiting for junction to free up...
  robot 2: still waiting (exclusive access conflict …)  poll 1/10
  robot 2: still waiting (exclusive access conflict …)  poll 2/10
  robot 2: still waiting (exclusive access conflict …)  poll 3/10
  robot 1: work complete, releasing junction
  robot 1: released lease 333
  robot 2: junction is free (after 4 polls)
  robot 2: acquired junction (lease 444)
  robot 2: released lease 444

simulation complete.
```

(Exact poll counts vary with timing.)

### How it works

- `POST /ares/v1/claims/evaluate` — read-only check, returns
  `Grant` / `Deny`. Safe to spam.
- `POST /ares/v1/leases` — reserve the zone after a successful
  evaluation.
- `POST /ares/v1/leases/release` — release when done.

Resource IDs are sent as text (`"100"` for `dock_a`'s numeric alias,
or the full UUID). The server resolves both forms via the
`external.numeric_id` workspace property — see `PRESENTATION.md`.
