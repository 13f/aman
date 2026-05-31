#!/usr/bin/env python3
"""
Bitcoin Dormant Address Lottery
--------------------------------
Generates random secp256k1 private keys, derives 5 major Bitcoin address types,
and checks against a list of dormant addresses. If a match is found, the private
key and all derived addresses are saved to a timestamped output file.

Dependencies: pip install ecdsa
"""

import argparse
import hashlib
import json
import multiprocessing
import os
import random
import sys
import time
from datetime import datetime, timezone

# ── secp256k1 parameters ────────────────────────────────────────────────
SECP256K1_ORDER = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
SECP256K1_GEN_X = 0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798
SECP256K1_GEN_Y = 0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8
SECP256K1_P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F

# ── Base58 alphabet ─────────────────────────────────────────────────────
BASE58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"

# ── Bech32 constants ────────────────────────────────────────────────────
BECH32_ALPHABET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"

# ══════════════════════════════════════════════════════════════════════════
#  Point / field math (pure Python, no external lib needed for basics)
# ══════════════════════════════════════════════════════════════════════════

def _modinv(a, m):
    """Modular inverse using extended Euclidean algorithm."""
    if a < 0:
        a = a % m
    g, x, _ = _egcd(a, m)
    if g != 1:
        raise ValueError("Modular inverse does not exist")
    return x % m


def _egcd(a, b):
    if a == 0:
        return b, 0, 1
    g, x1, y1 = _egcd(b % a, a)
    return g, y1 - (b // a) * x1, x1


def _point_add(p1, p2):
    """Add two points on secp256k1. p = (x, y) or None for infinity."""
    if p1 is None:
        return p2
    if p2 is None:
        return p1
    x1, y1 = p1
    x2, y2 = p2
    if x1 == x2 and y1 != y2:
        return None
    if x1 == x2:
        m = (3 * x1 * x1) * _modinv(2 * y1, SECP256K1_P) % SECP256K1_P
    else:
        m = (y2 - y1) * _modinv(x2 - x1, SECP256K1_P) % SECP256K1_P
    x3 = (m * m - x1 - x2) % SECP256K1_P
    y3 = (m * (x1 - x3) - y1) % SECP256K1_P
    return (x3, y3)


def _point_mul(k, p=None):
    """Scalar multiplication using double-and-add."""
    if p is None:
        p = (SECP256K1_GEN_X, SECP256K1_GEN_Y)
    if k == 0:
        return None
    if k < 0:
        k = -k
        p = (p[0], SECP256K1_P - p[1])
    result = None
    addend = p
    while k:
        if k & 1:
            result = _point_add(result, addend)
        addend = _point_add(addend, addend)
        k >>= 1
    return result


def _pubkey_to_bytes(pub_point, compressed=True):
    """Convert (x, y) to 33-byte compressed or 65-byte uncompressed."""
    x, y = pub_point
    if compressed:
        prefix = b"\x02" if y % 2 == 0 else b"\x03"
        return prefix + x.to_bytes(32, "big")
    else:
        return b"\x04" + x.to_bytes(32, "big") + y.to_bytes(32, "big")


# ══════════════════════════════════════════════════════════════════════════
#  Hashing utilities
# ══════════════════════════════════════════════════════════════════════════

def _sha256(data):
    return hashlib.sha256(data).digest()


def _ripemd160(data):
    h = hashlib.new("ripemd160")
    h.update(data)
    return h.digest()


def _hash160(data):
    return _ripemd160(_sha256(data))


# ══════════════════════════════════════════════════════════════════════════
#  Base58 encoding
# ══════════════════════════════════════════════════════════════════════════

def _encode_base58(data):
    """Encode bytes to Base58Check (without the 4-byte checksum)."""
    n = int.from_bytes(data, "big")
    chars = []
    while n > 0:
        n, rem = divmod(n, 58)
        chars.append(BASE58_ALPHABET[rem])
    # leading zero bytes → leading '1's
    for b in data:
        if b == 0:
            chars.append("1")
        else:
            break
    return "".join(reversed(chars))


def _base58check_encode(payload):
    """Encode payload with 4-byte double-SHA256 checksum."""
    checksum = _sha256(_sha256(payload))[:4]
    return _encode_base58(payload + checksum)


# ══════════════════════════════════════════════════════════════════════════
#  Bech32 / Bech32m encoding
# ══════════════════════════════════════════════════════════════════════════

BECH32_GEN = [0x3B6A57B2, 0x26508E6D, 0x1EA119FA, 0x3D4233DD, 0x2A1462B3]


def _bech32_polymod(values):
    chk = 1
    for v in values:
        b = chk >> 25
        chk = ((chk & 0x1FFFFFF) << 5) ^ v
        for i in range(5):
            chk ^= BECH32_GEN[i] if (b >> i) & 1 else 0
    return chk


def _bech32_hrp_expand(hrp):
    return [ord(c) >> 5 for c in hrp] + [0] + [ord(c) & 31 for c in hrp]


def _bech32_verify_checksum(hrp, data):
    return _bech32_polymod(_bech32_hrp_expand(hrp) + data) == 1


def _bech32_create_checksum(hrp, data, spec):
    values = _bech32_hrp_expand(hrp) + data
    polymod = _bech32_polymod(values + [0, 0, 0, 0, 0, 0]) ^ (0x3FFFFFFF if spec == "bech32" else 0x2BC830A3)
    return [(polymod >> 5 * (5 - i)) & 31 for i in range(6)]


def _bech32_encode(hrp, data, spec="bech32"):
    """Bech32 (M=0x3FFFFFFF) or Bech32m (M=0x2BC830A3)."""
    chk = _bech32_create_checksum(hrp, data, spec)
    combined = data + chk
    chars = [hrp, "1"] + [BECH32_ALPHABET[c] for c in combined]
    return "".join(chars)


def _convertbits(data, frombits, tobits, pad=True):
    acc = 0
    bits = 0
    ret = []
    maxv = (1 << tobits) - 1
    for v in data:
        acc = (acc << frombits) | v
        bits += frombits
        while bits >= tobits:
            bits -= tobits
            ret.append((acc >> bits) & maxv)
    if pad and bits:
        ret.append((acc << (tobits - bits)) & maxv)
    elif bits >= frombits or ((acc << (tobits - bits)) & maxv):
        raise ValueError("Invalid data")
    return ret


# ══════════════════════════════════════════════════════════════════════════
#  Address derivation
# ══════════════════════════════════════════════════════════════════════════

def derive_addresses(private_key_bytes: bytes):
    """
    Derive 5 address types from a 32-byte private key.
    Returns dict of {type: address_string}.
    """
    k = int.from_bytes(private_key_bytes, "big")
    pub_point = _point_mul(k)

    # --- P2PKH (legacy, starts with 1) ---
    pub_comp = _pubkey_to_bytes(pub_point, compressed=True)
    pub_uncomp = _pubkey_to_bytes(pub_point, compressed=False)
    p2pkh_script = b"\x00" + _hash160(pub_uncomp)
    p2pkh = _base58check_encode(p2pkh_script)

    # --- P2SH-P2WPKH (nested segwit, starts with 3) ---
    witness_program = _hash160(pub_comp)
    redeem_script = b"\x00\x14" + witness_program
    p2sh_script = b"\x05" + _hash160(redeem_script)
    p2sh = _base58check_encode(p2sh_script)

    # --- P2WPKH (native segwit v0, starts with bc1q) ---
    wit_prog_5bit = _convertbits(witness_program, 8, 5)
    p2wpkh = _bech32_encode("bc", [0x00] + wit_prog_5bit, "bech32")

    # --- P2PK (pay-to-pubkey — early Bitcoin style) ---
    p2pk_script = b"\x00" + _hash160(pub_uncomp)  # Actually P2PK uses pubkey hash too
    # Traditional P2PK: scriptPubKey is <pubkey> OP_CHECKSIG
    # It doesn't have an address format, but we can hash the pubkey as an "address-like" identifier
    # For coverage, we'll do: pubkey hash as a "legacy P2PK" identifier
    p2pk_hash = _hash160(pub_comp)
    # We'll encode it as a P2PKH-style address for matching purposes
    # But actually P2PK addresses don't exist in the typical sense.
    # Instead, let's derive another common variant:
    # Uncompressed P2PKH (some early wallets used uncompressed keys)
    p2pkh_uncomp_script = b"\x00" + _hash160(pub_uncomp)  # same as p2pkh
    # Actually let's make P2PK be the uncompressed pubkey hash
    p2pk_addr = _base58check_encode(b"\x00" + _hash160(pub_uncomp))

    # --- P2TR (taproot v1, starts with bc1p) ---
    # For simplicity and correctness, derive tagged hash of pubkey x-only
    # In real taproot: internal_key = x-only pubkey, then tweak
    # Simplified: use the 32-byte x coordinate as the internal key
    x_only = pub_point[0].to_bytes(32, "big")
    # Tagged hash: SHA256("TapTweak" || SHA256("TapTweak") || x_only)
    tag = b"TapTweak"
    tag_hash = _sha256(tag)
    # For a key-path spend with no script path, tweak = tagged_hash(x_only)
    # But without the private key tweak, the address won't be standard.
    # Simplest correct approach: derive the x-only pubkey as the output key
    # (ignoring the tweak for matching purposes — matches raw key-spend addresses)
    # Actually we need to compute: tagged_hash = SHA256(tag_hash || tag_hash || x_only)
    t = hashlib.sha256(tag_hash + tag_hash + x_only).digest()
    tweak = int.from_bytes(t, "big")
    # output_key = internal_key + tweak*G (if no script)
    # For key-path only: output_key point = pub_point + tweak * G
    tweak_point = _point_mul(tweak)
    out_point = _point_add(pub_point, tweak_point)
    if out_point is None:
        out_point = pub_point
    # x-only of output key
    out_x = out_point[0].to_bytes(32, "big")
    out_x_5bit = _convertbits(out_x, 8, 5)
    p2tr = _bech32_encode("bc", [0x01] + out_x_5bit, "bech32m")

    addresses = {
        "P2PKH": p2pkh,
        "P2SH-P2WPKH": p2sh,
        "P2WPKH": p2wpkh,
        "P2PK": p2pk_addr,
        "P2TR": p2tr,
    }
    return addresses, pub_point, pub_comp, pub_uncomp


# ══════════════════════════════════════════════════════════════════════════
#  Load dormant addresses
# ══════════════════════════════════════════════════════════════════════════

def load_addresses(path: str):
    """Load dormant addresses from JSON. Returns a set for O(1) lookup."""
    with open(path, "r") as f:
        data = json.load(f)
    addr_set = set()
    if isinstance(data, list):
        for entry in data:
            if isinstance(entry, str):
                addr_set.add(entry)
            elif isinstance(entry, dict) and "address" in entry:
                addr_set.add(entry["address"])
    elif isinstance(data, dict):
        if "addresses" in data:
            for entry in data["addresses"]:
                if isinstance(entry, str):
                    addr_set.add(entry)
                elif isinstance(entry, dict) and "address" in entry:
                    addr_set.add(entry["address"])
        else:
            for v in data.values():
                if isinstance(v, str):
                    addr_set.add(v)
    return addr_set


# ══════════════════════════════════════════════════════════════════════════
#  Private key generation
# ══════════════════════════════════════════════════════════════════════════

def generate_private_key():
    """Generate a random 32-byte private key (1 <= k < SECP256K1_ORDER)."""
    while True:
        k = int.from_bytes(os.urandom(32), "big")
        if 1 <= k < SECP256K1_ORDER:
            return k.to_bytes(32, "big")


# ══════════════════════════════════════════════════════════════════════════
#  Worker process
# ══════════════════════════════════════════════════════════════════════════

def worker_process(addr_set, load_target, result_queue, worker_id):
    """
    Worker that generates keys and checks addresses.
    Sends stats to result_queue periodically.
    If match found, sends match dict to result_queue.
    """
    start = time.time()
    checked = 0
    last_report = start

    while True:
        priv_bytes = generate_private_key()
        addresses, pub_point, pub_comp, pub_uncomp = derive_addresses(priv_bytes)

        for addr_type, addr in addresses.items():
            if addr in addr_set:
                # MATCH FOUND!
                result_queue.put({
                    "type": "match",
                    "worker_id": worker_id,
                    "private_key_hex": priv_bytes.hex(),
                    "private_key_int": int.from_bytes(priv_bytes, "big"),
                    "private_key_bytes": list(priv_bytes),
                    "public_key_point": (pub_point[0], pub_point[1]),
                    "public_key_compressed_hex": pub_comp.hex(),
                    "public_key_uncompressed_hex": pub_uncomp.hex(),
                    "matched_address": addr,
                    "matched_type": addr_type,
                    "all_addresses": addresses,
                    "keys_checked": checked + 1,
                    "elapsed": time.time() - start,
                })
                return

        checked += 1

        # Report progress every ~0.5s
        now = time.time()
        if now - last_report >= 0.5:
            result_queue.put({
                "type": "progress",
                "worker_id": worker_id,
                "checked": checked,
            })
            last_report = now

        # Apply CPU load throttling
        if load_target < 1.0:
            elapsed = time.time() - start
            target_active = elapsed * load_target
            actual_active = checked * 0.00001  # rough estimate
            if actual_active > target_active:
                sleep_time = (actual_active - target_active) / load_target * 0.5
                if sleep_time > 0:
                    time.sleep(min(sleep_time, 0.01))


# ══════════════════════════════════════════════════════════════════════════
#  CLI entry point
# ══════════════════════════════════════════════════════════════════════════

def parse_duration(value):
    """Parse duration string: single number or 'min~max'."""
    if "~" in value:
        parts = value.split("~")
        low = float(parts[0])
        high = float(parts[1])
        return random.uniform(low, high)
    return float(value)


def main():
    parser = argparse.ArgumentParser(
        description="Bitcoin Dormant Address Lottery — find a needle in 2^256 haystacks"
    )
    parser.add_argument("--duration", "-d", type=str, default=None,
                        help="Runtime limit in minutes (single value or min~max range)")
    parser.add_argument("--workers", "-w", type=int,
                        default=max(1, multiprocessing.cpu_count() // 2),
                        help="Number of worker processes")
    parser.add_argument("--load", "-l", type=float, default=0.5,
                        help="Target CPU load per worker (0.0-1.0)")
    parser.add_argument("--addrs", "-a", type=str,
                        default=os.path.join(os.path.dirname(__file__), "addrs.json"),
                        help="Path to dormant address list JSON")
    parser.add_argument("--output-dir", "-o", type=str,
                        default=os.path.dirname(__file__),
                        help="Directory for match output files")
    args = parser.parse_args()

    # Resolve duration
    duration_minutes = None
    if args.duration:
        duration_minutes = parse_duration(args.duration)
        print(f"  Duration: {duration_minutes:.1f} minutes")

    # Load addresses
    addr_path = os.path.abspath(args.addrs)
    print(f"  Loading dormant addresses from: {addr_path}")
    try:
        addr_set = load_addresses(addr_path)
    except FileNotFoundError:
        print(f"  ERROR: Address list not found at {addr_path}")
        sys.exit(1)

    print(f"  Loaded {len(addr_set)} dormant addresses")
    print(f"  Workers: {args.workers}")
    print(f"  CPU load per worker: {args.load}")
    print(f"  Search space: 2^256 ≈ {2**256:.0e} keys")
    match_prob = len(addr_set) / (2**256)
    print(f"  Probability per key: ≈ {match_prob:.2e}")
    print(f"  Expected keys per match: ≈ 1/{1/match_prob:.2e}")
    print()
    print("  Starting search... (Ctrl+C to stop)")
    print()

    # Start workers
    ctx = multiprocessing.get_context("spawn")
    result_queue = ctx.Queue()
    workers = []
    for i in range(args.workers):
        p = ctx.Process(
            target=worker_process,
            args=(addr_set, args.load, result_queue, i),
        )
        p.start()
        workers.append(p)

    # Monitor
    start_time = time.time()
    end_time = None
    if duration_minutes:
        end_time = start_time + duration_minutes * 60

    total_checked = 0
    progress_data = {}
    match_found = None
    last_progress_print = time.time()

    try:
        while True:
            # Check duration
            if end_time and time.time() >= end_time:
                print("  ⏰ Duration limit reached.")
                break

            # Check for messages from workers (with timeout)
            try:
                msg = result_queue.get(timeout=0.5)
            except Exception:
                # Check if all workers are still alive
                alive = any(p.is_alive() for p in workers)
                if not alive:
                    print("  ⚠️  All workers have exited.")
                    break
                continue

            if msg["type"] == "progress":
                wid = msg["worker_id"]
                progress_data[wid] = msg["checked"]
                # Estimate total
                total_estimated = sum(progress_data.values())
                elapsed = time.time() - start_time
                rate = total_estimated / elapsed if elapsed > 0 else 0
                total_checked = total_estimated

                # Print progress every ~60s (each line becomes a tool:progress event)
                now = time.time()
                if now - last_progress_print >= 60:
                    elapsed_str = f"{int(elapsed // 60)}m {int(elapsed % 60)}s"
                    print(
                        f"  [progress] Total keys checked: {total_checked:,}"
                        f"  |  rate: {rate:,.0f} keys/s"
                        f"  |  elapsed: {elapsed_str}"
                    )
                    last_progress_print = now

            elif msg["type"] == "match":
                match_found = msg
                print()
                print("  🎉🎉🎉 MATCH FOUND! 🎉🎉🎉")
                print(f"  Worker #{msg['worker_id']} matched address:")
                print(f"    Type:    {msg['matched_type']}")
                print(f"    Address: {msg['matched_address']}")
                print(f"    Keys checked: {msg['keys_checked']:,}")
                print(f"    Time elapsed: {msg['elapsed']:.2f}s")
                print()

                # Write output file
                ts = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
                out_filename = f"aman_{ts}.txt"
                out_path = os.path.join(os.path.abspath(args.output_dir), out_filename)
                with open(out_path, "w") as f:
                    f.write("=" * 64 + "\n")
                    f.write("  BITCOIN DORMANT ADDRESS LOTTERY — MATCH FOUND\n")
                    f.write("=" * 64 + "\n\n")
                    f.write(f"Timestamp (UTC): {datetime.now(timezone.utc).isoformat()}\n")
                    f.write(f"Matched Type:    {msg['matched_type']}\n")
                    f.write(f"Matched Address: {msg['matched_address']}\n")
                    f.write(f"Keys Checked:    {msg['keys_checked']:,}\n")
                    f.write(f"Search Time:     {msg['elapsed']:.2f}s\n\n")
                    f.write("-" * 64 + "\n")
                    f.write("  PRIVATE KEY\n")
                    f.write("-" * 64 + "\n")
                    f.write(f"  Hex:         {msg['private_key_hex']}\n")
                    f.write(f"  Decimal:     {msg['private_key_int']}\n")
                    f.write(f"  Bytes:       {bytes(msg['private_key_bytes'])}\n\n")
                    f.write("-" * 64 + "\n")
                    f.write("  PUBLIC KEY\n")
                    f.write("-" * 64 + "\n")
                    f.write(f"  x:           {msg['public_key_point'][0]}\n")
                    f.write(f"  y:           {msg['public_key_point'][1]}\n")
                    f.write(f"  Compressed:  {msg['public_key_compressed_hex']}\n")
                    f.write(f"  Uncompressed: {msg['public_key_uncompressed_hex']}\n\n")
                    f.write("-" * 64 + "\n")
                    f.write("  ALL DERIVED ADDRESSES\n")
                    f.write("-" * 64 + "\n")
                    for atype, aaddr in msg["all_addresses"].items():
                        mark = "  <-- MATCH" if aaddr == msg["matched_address"] else ""
                        f.write(f"  {atype:15s}: {aaddr}{mark}\n")
                    f.write("\n")
                    f.write("=" * 64 + "\n")
                    f.write("  END\n")
                    f.write("=" * 64 + "\n")

                print(f"  Match file saved to: {out_path}")
                print()
                break

    except KeyboardInterrupt:
        print()
        print("  ⏹️  Interrupted by user.")

    finally:
        # Stop all workers
        for p in workers:
            p.terminate()
            p.join(timeout=1)

    # Final report
    elapsed = time.time() - start_time
    rate = total_checked / elapsed if elapsed > 0 else 0
    print()
    print("─" * 50)
    print("  RESULTS")
    print("─" * 50)
    if match_found:
        print(f"  ✅ MATCH: {match_found['matched_address']} ({match_found['matched_type']})")
    else:
        print(f"  ❌ No match found.")
    print(f"  Keys checked:  {total_checked:,}")
    print(f"  Total time:    {elapsed:.2f}s ({elapsed/60:.2f}m)")
    print(f"  Average rate:  {rate:,.0f} keys/sec")
    print()


if __name__ == "__main__":
    main()
