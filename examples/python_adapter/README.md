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

The script imports neither `syncbot` nor its coordinator. It manually encodes
the canonical datapod wire schema and uses `peerbus.Node.datapod_req_client`.
