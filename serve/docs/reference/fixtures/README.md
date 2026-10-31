# Shared cryptographic fixtures

[remote-relay-vectors.json](remote-relay-vectors.json) carries the vectors for the `kith/*` labels, `kith-sig-v1` and `Kith-Sig`, what every pairing made today uses.

The file contains deterministic protocol-v1 vectors for host/device keys, handshake, frame encryption, chunking, SAS, pairing offers, mailbox, request signatures, claims, signed ACL and sealed push.

The private values are fixed **test seeds**, not live credentials. Changing these bytes changes the common contract checked by Rust, Swift and TypeScript.

[remote-relay-vectors.py](remote-relay-vectors.py) prints the JSON to stdout and requires Python 3 plus `cryptography`. To compare without overwriting the checked-in fixture, from the repository root:

```sh
python3 docs/reference/fixtures/remote-relay-vectors.py > /tmp/kith-relay-vectors.json
cmp docs/reference/fixtures/remote-relay-vectors.json /tmp/kith-relay-vectors.json
```

Consumers:

- `host/src/remote/crypto.rs` (`include_str!`)
- the iOS Kit `RelayVectorsTests.swift` (apps/ios/Kit/Tests)
- `cloud/test/vectors.test.ts` and `cloud/test/relay.test.ts`

When moving or changing a fixture, update those paths and the cloud, iOS Kit (`kit.yml`) and host CI path filters. Run the affected vector tests; a Markdown link check alone cannot validate executable imports. [Protocol reference](../remote-relay.md).
