# syncbot — live test report

- **Target:** running server at `http://127.0.0.1:8080/ares/v1` (`make run` → `serve_workspace examples/fixed`)
- **Version reported by `/health`:** `0.1.2`
- **Map:** 7 zones — docks `dock_a=1`, `dock_b=2`, `dock_c=3`; zones `zone_4=4`, `zone_5=5` (shared cap 2), `zone_6=6`, `zone_7=7` (shared cap 2)
- **Result:** **40 / 40 PASS**

Format: `#. [verdict] description → observed response`

## A. Read / connectivity (open, no key)
1. [PASS] `GET /health` → `{"status":"ok","version":"0.1.2"}`
2. [PASS] `GET /zones` → list incl. `"name":"dock_c"` (7 zones + root)
3. [PASS] `GET /zones/3` → `{"numeric_id":3,"name":"dock_c",...}`
4. [PASS] `GET /zones/99` (unknown) → `{"message":"unknown zone id Numeric(99)"}`
5. [PASS] `GET /nodes/1001` → `{"numeric_id":1001,"name":"dock_a_entry",...}`
6. [PASS] `GET /edges` → list with `numeric_id` 2001/2002
7. [PASS] `GET /fleet/snapshot` → `{"robots":[],"requests":[],"leases":[]}` (fresh server)
8. [PASS] `POST /routes/plan` 1001→1003 → `{"found":false,...PolicyBlocked}` *(correct: dock_a is exclusive/blocked — must be claimed to traverse)*

## B. Register
9.  [PASS] `POST /robots` `<reg><robot>91><key>1234><alive>2>` (XML) → `decision 1, reason 0`
10. [PASS] register 91 again → `decision 0, reason 2` (already registered)
11. [PASS] `POST /robots {"robot":"92"}` no key (JSON) → `decision 1, reason 0` (default key)
12. [PASS] register `{"robot":"abc"}` → `decision 0, reason 3` (bad id)
13. [PASS] register `key:"did:key=x"` → `decision 0, reason 4` (unsupported key scheme)
14. [PASS] register UUID robot `aaaaaaaa-…-0001` → `decision 1, reason 0`

## C. Heartbeat
15. [PASS] `POST /robots/91/heartbeat` zone 3, key 1234 (XML) → `decision 1, reason 0`
16. [PASS] heartbeat 91 wrong key → `decision 0, reason 1` (mismatched key)
17. [PASS] heartbeat 999 (unregistered) → `decision 0, reason 2` (not registered)
18. [PASS] heartbeat 91 `<zone>-1</zone>` (unknown location) → `decision 1, reason 0`
19. [PASS] heartbeat 91 `node 1001` (JSON) → `decision 1, reason 0`

## D. Claim
20. [PASS] 91 claim zone 5 (exclusive) → `decision 1, reason 0` (granted)
21. [PASS] 92 claim zone 5 → `decision 0, reason 2, blocked 5` (conflict)
22. [PASS] 92 atomic claim `[6,5]` → `decision 0, reason 2, blocked 5` (all-or-nothing; 5 held)
23. [PASS] 92 claim zone 6 → `decision 1, reason 0` (granted — 6 free)
24. [PASS] 94 claim zone 7 `access_mode:1 lease_time:30` → `decision 1, reason 0`
25. [PASS] claim `access_mode:2` (reserved) → `decision 0, reason 5` (bad request)
26. [PASS] claim unknown zone 99 → `decision 0, reason 4, blocked 99`
27. [PASS] claim wrong key → `decision 0, reason 1`
28. [PASS] XML claim zone 1 by 91 → `<reply><decision>1</decision><reason>0</reason></reply>`

## E. Release
29. [PASS] 91 release zone 5 → `decision 1, reason 0` (released)
30. [PASS] 91 release zone 5 again → `decision 0, reason 2` (nothing held)
31. [PASS] 92 claim zone 5 (now free) → `decision 1, reason 0`

## F. Tier-2 (key enforcement)
32. [PASS] `POST /claims/evaluate` (read-only, no key) → full `ClaimEvaluation` `{"decision":"Grant",...}`
33. [PASS] `POST /claims` submit **without key** → HTTP 400 `{"message":"mismatched key: missing key"}`
34. [PASS] `POST /claims` submit **with key** → `{"decision":"Grant",...}`

## G. Error handling
35. [PASS] malformed XML body → `<ApiError><message>invalid XML body: ill-formed document…`

## H. Auto-release on heartbeat timeout
36. [PASS] register 95 `alive:1` → `decision 1, reason 0`
37. [PASS] 95 claim zone 3 → `decision 1, reason 0`
38. [PASS] 96 register → `decision 1, reason 0`
39. [PASS] 96 claim zone 3 (95 still holds) → `decision 0, reason 2, blocked 3` (conflict)
40. [PASS] *(after 3s, no heartbeat from 95)* 96 claim zone 3 → `decision 1, reason 0`
    → **the sweeper auto-released the inactive robot's claim**, freeing the zone.

## Live state after run
`fleet/snapshot`: 7 robots registered (91, 92, 94, 95, 96 + UUID robot…), claims reflecting the final grants. Inactive robots stay registered but hold no claims.

## Notes
- Test 8 (`PolicyBlocked` route) is correct domain behaviour for this map, not a failure — `dock_a` is `exclusive`/claim-required, so the planner refuses to route through it unclaimed.
- Both JSON and XML transports exercised; both return the flat `decision`/`reason` shape (XML root `<reply>`).
- Separately, the offline suite passes: **87 unit/integration tests, 0 failed**, across `rest`/`xmlt`/`robo`; C ABI demo passes; both protocol PDFs + mdBook compile.
