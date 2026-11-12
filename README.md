# Passback program

Source of the Solana program behind [100isthenew50.com](https://100isthenew50.com): a pay-to-rank daily board paid in USDC, where part of each payment is passed back to the projects it overtakes. Written in Anchor 0.32.1.

This repository is the public source for the program's verifiable build. It contains only the build inputs (`Cargo.toml`, `Cargo.lock`, `programs/`), the Anchor toolchain pins (`Anchor.toml`) and the generated IDL (`onchain/idl/passback.json`). The web app, indexer and tests live in a private repository; every file here is byte-identical to its reviewed source commit there.

## Release

| Item | Value |
|---|---|
| Program id (mainnet-beta) | `C9NtyYCuGLZTPr6H9wLE2hhTsfZPmVdfiNucQxy2u3JB` |
| Source commit (private repository) | `939fe305a023171243bcb5d1f873139d77fb5926` |
| Verifiable executable hash | `11d47f2fc70d8dbb79afb4698f9286c84a06c6188b815121634072fbca4af885` |
| solana-verify | 0.5.2 |
| Base image | `solanafoundation/solana-verifiable-build@sha256:12fd4c0a0790f0fc41ef74b0cdb6bccc167ba6137eb1adb749bac649481c86bd` (`linux/amd64`) |
| Library | `passback` |

Until the program is deployed, the hash above describes the release candidate, not an on-chain program.

## Build and verify

Docker with a running daemon is required.

```sh
cargo install solana-verify --version 0.5.2 --locked
solana-verify build --library-name passback \
  --base-image solanafoundation/solana-verifiable-build@sha256:12fd4c0a0790f0fc41ef74b0cdb6bccc167ba6137eb1adb749bac649481c86bd
solana-verify get-executable-hash target/deploy/passback.so
```

Against the deployed program:

```sh
solana-verify verify-from-repo https://github.com/adinvadim/passback-program \
  --program-id C9NtyYCuGLZTPr6H9wLE2hhTsfZPmVdfiNucQxy2u3JB \
  --library-name passback \
  --commit-hash <commit> \
  --base-image solanafoundation/solana-verifiable-build@sha256:12fd4c0a0790f0fc41ef74b0cdb6bccc167ba6137eb1adb749bac649481c86bd \
  --url https://api.mainnet-beta.solana.com
```

Use the IDL published in this repository. Any on-chain IDL account for this program is not part of the release and should be treated as untrusted metadata.

## License

All rights reserved. The source is published so that anyone can audit and reproduce the deployed program.
