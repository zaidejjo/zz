#!/usr/bin/env python3
"""Deterministic TOML benchmark corpus generator (seed 7).

Produces flat-key and realistic-mixed files at 1 KB, 8 KB, 32 KB,
128 KB and 1 MB under corpus/. Mixed files interleave bare keys,
[table] sections, [[array-of-tables]] entries and long strings so
the parse proxy exercises table, AoT and string append paths.

Sizes are targets: files are padded with flat keys to land within
~2% of target. Reruns are byte-identical (fixed seed + no timestamps).
"""

import hashlib
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
CORPUS = os.path.join(HERE, "corpus")
SEED = 7

SIZES = {
    "1k": 1024,
    "8k": 8 * 1024,
    "32k": 32 * 1024,
    "128k": 128 * 1024,
    "1m": 1024 * 1024,
}

WORDS = [
    "alpha",
    "beta",
    "gamma",
    "delta",
    "epsilon",
    "zeta",
    "eta",
    "theta",
    "server",
    "client",
    "cache",
    "queue",
    "owner",
    "title",
]


def flat_block(rng, i):
    return f"k{i:06d} = {rng.randrange(0, 1000000)}\n"


def mixed_blocks(rng, i):
    """One round-robin cycle of realistic shapes."""
    w = rng.choice(WORDS)
    table = f"[t{i:04d}_{w}]\n"
    table += f'host = "h{i}.example.com"\nport = {8000 + (i % 1000)}\n'
    aot = f'[[items]]\nname = "{w}-{i}"\nqty = {1 + (i % 99)}\n'
    long_s = "x" * (120 + rng.randrange(0, 120))
    strang = f'desc{i:04d} = "{long_s}"\n'
    flat = f"f{i:06d} = {rng.randrange(0, 1000000)}\n"
    return table + aot + strang + flat


def build(target, kind):
    rng = random.Random(SEED)
    parts = []
    size = 0
    i = 0
    # Head: a few flat keys so every file starts on the root table.
    while size < target:
        if kind == "flat":
            blk = flat_block(rng, i)
        else:
            blk = mixed_blocks(rng, i)
        # Stop before overshooting by more than one block, then pad
        # precisely with flat keys.
        if size + len(blk) > target and size > 0:
            break
        parts.append(blk)
        size += len(blk)
        i += 1
    # Precise pad with flat keys to land within ~2% of target.
    j = 0
    while size < target:
        blk = f"pad{j:06d} = {j}\n"
        if size + len(blk) > target + 64:
            break
        parts.append(blk)
        size += len(blk)
        j += 1
    return "".join(parts)


def main():
    os.makedirs(CORPUS, exist_ok=True)
    manifest = []
    for name, target in SIZES.items():
        for kind in ("flat", "mixed"):
            text = build(target, kind)
            fname = f"{kind}_{name}.toml"
            path = os.path.join(CORPUS, fname)
            with open(path, "w") as f:
                f.write(text)
            sha = hashlib.sha256(text.encode()).hexdigest()[:16]
            manifest.append((fname, len(text), sha))
            print(f"{fname}: {len(text)} bytes sha={sha}")
    with open(os.path.join(CORPUS, "MANIFEST.txt"), "w") as f:
        for fname, size, sha in manifest:
            f.write(f"{sha}  {size:8d}  {fname}\n")


if __name__ == "__main__":
    sys.exit(main())
