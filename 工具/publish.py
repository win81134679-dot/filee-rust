# /// script
# requires-python = ">=3.10"
# dependencies = ["cryptography>=42"]
# ///
"""Publish a Filee Rust installer so installed copies update themselves (2026-10-29: 「以後靠程式自動抓取repo最新exe
自動靜默更新」「我希望每台有識別碼讓我可以單獨推送更新」).

  uv run 工具/publish.py new-key                     once: make the signing key (asks for a password)
  uv run 工具/publish.py new-key --generate-password  same, with a random password kept in keys/ (never asks again)
  uv run 工具/publish.py publish                     newest dist/FileeRust-*-Setup.exe → a GitHub pre-release
  uv run 工具/publish.py publish --machines A,B      only these machines take it (ids or names from machines.json)
  uv run 工具/publish.py open v2.2.0-rust.5          open an earlier targeted release to every machine
  uv run 工具/publish.py publish --dry-run           sign into build/publish/ only, nothing is uploaded
(python 工具/update.py --publish … runs this the same way.)

A release carries the installer, update.json (version, installer name, size, SHA-256, machines or null) and update.sig
(ECDSA P-256 / SHA-256 over the exact bytes of update.json, DER, base64). filee-rust.exe takes only a release signed by
the key whose public half is packaging/update.pub (it ships in the installer), newer than itself and meant for it.

The private key is keys/update-signing.pem (not in git), encrypted with a password. Keep a copy of keys/ on a USB stick:
without it, installed copies can never be updated again (only reinstalled by hand). The password comes from
FILEE_RUST_KEY_PASSWORD (tests), else keys/update-signing-password.txt (written by `new-key --generate-password`, not in
git), else the terminal. Machine names: 工具/machines.json (not in git),
{"王老師": "K7Q2-9XMA", …}; a machine shows its id in the tray menu.
"""
import argparse
import base64
import getpass
import secrets
import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec

ROOT = Path(__file__).resolve().parent.parent
KEY = ROOT / "keys" / "update-signing.pem"
PUBLIC = ROOT / "packaging" / "update.pub"
PASSWORD_FILE = ROOT / "keys" / "update-signing-password.txt"
MACHINES = ROOT / "工具" / "machines.json"
OUT = ROOT / "build" / "publish"
REPO = "win81134679-dot/filee-rust"
SETUP = re.compile(r"^FileeRust-(\d+\.\d+\.\d+-rust\.\d+)-Setup\.exe$")
MACHINE_ID = re.compile(r"^[0-9A-Z]{4}-[0-9A-Z]{4}$")


def password(confirm: bool) -> bytes:
    if os.environ.get("FILEE_RUST_KEY_PASSWORD"):
        return os.environ["FILEE_RUST_KEY_PASSWORD"].encode()
    if not confirm and PASSWORD_FILE.exists():
        return PASSWORD_FILE.read_text(encoding="utf-8").strip().encode()
    first = getpass.getpass("Signing key password: ")
    if confirm and getpass.getpass("Again: ") != first:
        raise SystemExit("FAIL: the two passwords differ")
    if len(first) < 8:
        raise SystemExit("FAIL: use at least 8 characters")
    return first.encode()


def new_key(args) -> None:
    key_path, pub_path = Path(args.key), Path(args.public)
    if key_path.exists():
        raise SystemExit(f"FAIL: {key_path} exists; a new key would cut off every installed copy. Nothing changed.")
    if args.generate_password:
        if PASSWORD_FILE.exists():
            raise SystemExit(f"FAIL: {PASSWORD_FILE} exists. Nothing changed.")
        secret = secrets.token_urlsafe(24)
        PASSWORD_FILE.parent.mkdir(parents=True, exist_ok=True)
        PASSWORD_FILE.write_text(secret + "\n", encoding="utf-8")
        pw = secret.encode()
    else:
        pw = password(confirm=True)
    key = ec.generate_private_key(ec.SECP256R1())
    pem = key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                            serialization.BestAvailableEncryption(pw))
    key_path.parent.mkdir(parents=True, exist_ok=True)
    key_path.write_bytes(pem)
    pub_path.write_bytes(key.public_key().public_bytes(serialization.Encoding.PEM,
                                                       serialization.PublicFormat.SubjectPublicKeyInfo))
    print(f"private key: {key_path}  (copy it to a USB stick)\npublic key:  {pub_path}  (commit it; it ships in the installer)")


def load_key(path: Path):
    if not path.exists():
        raise SystemExit(f"FAIL: no signing key at {path}; run: uv run 工具/publish.py new-key")
    try:
        return serialization.load_pem_private_key(path.read_bytes(), password=password(confirm=False))
    except ValueError:
        raise SystemExit("FAIL: wrong password")


def sign(key, data: bytes, public: Path) -> bytes:
    signature = key.sign(data, ec.ECDSA(hashes.SHA256()))
    # The signature has to verify with the public key the installers carry, or no copy would accept it.
    pub = serialization.load_pem_public_key(public.read_bytes())
    try:
        pub.verify(signature, data, ec.ECDSA(hashes.SHA256()))
    except InvalidSignature:
        raise SystemExit(f"FAIL: the key does not match {public}; installed copies would reject this release")
    return base64.b64encode(signature) + b"\n"


def resolve_machines(text: str | None) -> list[str] | None:
    if not text:
        return None
    names = json.loads(MACHINES.read_text(encoding="utf-8")) if MACHINES.exists() else {}
    out = []
    for item in (s.strip() for s in text.split(",") if s.strip()):
        mid = names.get(item, item).upper()
        if not MACHINE_ID.match(mid):
            raise SystemExit(f"FAIL: {item!r} is neither a machine id (XXXX-XXXX) nor a name in {MACHINES.name}")
        out.append(mid)
    return sorted(set(out))


def newest_setup(dist: Path) -> tuple[Path, str]:
    found = [(p, SETUP.match(p.name).group(1)) for p in dist.glob("FileeRust-*-Setup.exe") if SETUP.match(p.name)]
    if not found:
        raise SystemExit(f"FAIL: no FileeRust-<version>-Setup.exe in {dist}; run 工具/build_release.py")
    return max(found, key=lambda f: f[0].stat().st_mtime)


def manifest_bytes(version: str, setup: Path, machines: list[str] | None) -> bytes:
    data = setup.read_bytes()
    doc = {"version": version, "file": setup.name, "size": len(data), "sha256": hashlib.sha256(data).hexdigest(),
           "machines": machines}
    return (json.dumps(doc, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def gh(*args: str) -> None:
    subprocess.run(["gh", *args], check=True)


def publish(args) -> None:
    setup, version = newest_setup(Path(args.dist))
    machines = resolve_machines(args.machines)
    public = Path(args.public)
    stage_pub = ROOT / "build" / "stage" / "update.pub"
    if stage_pub.exists() and stage_pub.read_bytes() != public.read_bytes():
        raise SystemExit("FAIL: the installer was built with another public key; rebuild with 工具/build_release.py")
    manifest = manifest_bytes(version, setup, machines)
    signature = sign(load_key(Path(args.key)), manifest, public)
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "update.json").write_bytes(manifest)
    (OUT / "update.sig").write_bytes(signature)
    tag = "v" + version
    who = "every machine" if machines is None else ", ".join(machines)
    print(f"{tag}: {setup.name} ({setup.stat().st_size / 1e6:.1f} MB) for {who}")
    if args.dry_run:
        print(f"dry run: signed files in {OUT}; nothing uploaded")
        return
    gh("release", "create", tag, "--repo", args.repo, "--prerelease", "--title", tag, "--notes", "",
       str(setup), str(OUT / "update.json"), str(OUT / "update.sig"))
    print(f"published {tag}; installed copies pick it up within a day")


def open_release(args) -> None:
    tag = args.tag
    OUT.mkdir(parents=True, exist_ok=True)
    for f in ("update.json", "update.sig"):
        (OUT / f).unlink(missing_ok=True)
    gh("release", "download", tag, "--repo", args.repo, "--pattern", "update.json", "--dir", str(OUT))
    doc = json.loads((OUT / "update.json").read_text(encoding="utf-8"))
    if doc.get("machines") is None:
        print(f"{tag} is already for every machine")
        return
    doc["machines"] = None
    manifest = (json.dumps(doc, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    signature = sign(load_key(Path(args.key)), manifest, Path(args.public))
    (OUT / "update.json").write_bytes(manifest)
    (OUT / "update.sig").write_bytes(signature)
    gh("release", "upload", tag, "--repo", args.repo, "--clobber", str(OUT / "update.json"), str(OUT / "update.sig"))
    print(f"{tag} is now for every machine")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--key", default=str(KEY))
    ap.add_argument("--public", default=str(PUBLIC))
    ap.add_argument("--repo", default=REPO)
    sub = ap.add_subparsers(dest="cmd", required=True)
    n = sub.add_parser("new-key")
    n.add_argument("--generate-password", action="store_true",
                   help="random password saved to keys/update-signing-password.txt; publishing then never asks")
    p = sub.add_parser("publish")
    p.add_argument("--machines", help="ids or machines.json names, comma separated; default every machine")
    p.add_argument("--dist", default=str(ROOT / "dist"))
    p.add_argument("--dry-run", action="store_true")
    o = sub.add_parser("open")
    o.add_argument("tag")
    args = ap.parse_args()
    {"new-key": new_key, "publish": publish, "open": open_release}[args.cmd](args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
