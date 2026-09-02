# Python toy-wire adapter

Start the core/HTTP adapter from the repository root:

```sh
make run
```

Then run this out-of-process adapter (with the sibling `peerbus` and `datapod`
Python bindings installed) and feed it newline-delimited JSON:

```sh
printf '%s\n' \
  '{"op":"register","robot":7,"key":"1234"}' \
  '{"op":"heartbeat","robot":7,"key":"1234","zone":-1}' \
  | make run
```

A heartbeat may also carry a pose, in either frame but never both:

```sh
printf '%s\n' \
  '{"op":"register","robot":7,"key":"1234"}' \
  '{"op":"heartbeat","robot":7,"key":"1234","lat":52.0,"lon":5.0,"yaw":1.57}' \
  '{"op":"heartbeat","robot":7,"key":"1234","x":12.5,"y":-3.0,"z":0.0}' \
  | make run
```

The script imports neither `syncbot` nor its coordinator. It manually encodes
the canonical datapod wire schema and uses `peerbus.Node.datapod_req_client`.

## Keeping it in step with the core

Because the headers are packed by hand and datapod identity is size+alignment
hashed, a field added to a canonical struct silently rejects every request
this adapter sends. Two guards catch that:

- `make check` (also `make check-python-adapter` from the repository root)
  runs `adapter.py --self-check`, comparing each `struct` layout against the
  byte counts the core pins.
- `canonical_header_sizes_are_frozen` in `src/wire/peerbus.rs` fails the Rust
  test suite if those byte counts move.

Neither needs the peerbus bindings installed.
