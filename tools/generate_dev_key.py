#!/usr/bin/env python3
"""
Generate an Ed25519 keypair for signing packages.

Two kinds of key exist and conflating them is the failure this script is built to prevent:

* **development key** — generated here, committed to the repository, used by the test suite and
  by anyone who wants to exercise the verification path from a fresh clone. It is not a secret.
* **release key** — generated offline, on hardware that never runs the pipeline, and never
  committed. Its secret half never touches a machine with network access.

`tools/build_registry.py --production` refuses to publish anything signed with the development
key, and the CLI ships the development public key only. So the worst a committed key can do is
make a package look signed to a developer — it cannot make one look signed to a user who has a
real release key.

Usage
-----
    tools/generate_dev_key.py                      # write the development pair
    tools/generate_dev_key.py --out key.pem        # write an UNCOMMITTED keypair
    tools/generate_dev_key.py --print-public       # show the public key of the dev pair
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

#: Where the development keypair lives. Committed on purpose; see the module docstring.
DEV_PRIVATE_KEY = REPO_ROOT / "keys" / "dev-ed25519.pem"
DEV_PUBLIC_KEY = REPO_ROOT / "keys" / "dev-ed25519.pub"

#: The key id the registry and the CLI keyring use for the development key.
DEV_KEY_ID = "terrakit-dev-2026"


def generate(private_path: pathlib.Path, public_path: pathlib.Path) -> str:
    """Write an Ed25519 keypair and return the hex-encoded public key."""
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    key = Ed25519PrivateKey.generate()

    private_path.parent.mkdir(parents=True, exist_ok=True)
    # PKCS#8 rather than the raw OpenSSL "BEGIN PRIVATE KEY" traditional format: one format
    # instead of two, and the only one `load_pem_private_key` needs no password argument for.
    private_path.write_bytes(
        key.private_bytes(
            encoding=serialization.Encoding.PEM,
            format=serialization.PrivateFormat.PKCS8,
            encryption_algorithm=serialization.NoEncryption(),
        )
    )

    public_raw = key.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )
    public_path.write_text(
        json.dumps(
            {
                "algorithm": "ed25519",
                "keyId": DEV_KEY_ID,
                "publicKey": public_raw.hex(),
                "note": (
                    "Development key. Not a secret. "
                    "tools/build_registry.py --production refuses packages signed with it."
                ),
            },
            indent=2,
        )
        + "\n"
    )
    # 0o600: readable only by the owner. A committed key needs no secrecy, but the same
    # script produces release keys via --out, and a habit that writes 0644 secrets is a
    # habit that eventually writes a real one that way.
    private_path.chmod(0o600)
    return public_raw.hex()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument(
        "--out",
        type=pathlib.Path,
        help="write the private key here INSTEAD of the committed development key",
    )
    parser.add_argument(
        "--print-public",
        action="store_true",
        help="print the development public key and exit",
    )
    args = parser.parse_args(argv)

    if args.print_public:
        if not DEV_PUBLIC_KEY.is_file():
            print(f"error: {DEV_PUBLIC_KEY} does not exist; run without --print-public first")
            return 1
        print(json.loads(DEV_PUBLIC_KEY.read_text())["publicKey"])
        return 0

    if args.out:
        public_path = args.out.with_suffix(".pub")
        public_hex = generate(args.out, public_path)
        print(f"private key: {args.out}")
        print(f"public key:  {public_path}")
        print()
        print("This key is NOT committed. Keep it offline: a release key that has been on a")
        print("machine with network access should be considered compromised and rotated.")
        return 0

    public_hex = generate(DEV_PRIVATE_KEY, DEV_PUBLIC_KEY)
    print(f"development private key: {DEV_PRIVATE_KEY} (committed on purpose)")
    print(f"development public key:  {DEV_PUBLIC_KEY}")
    print(f"keyId:                   {DEV_KEY_ID}")
    print(f"publicKey:               {public_hex}")
    return 0


if __name__ == "__main__":
    sys.exit(main())