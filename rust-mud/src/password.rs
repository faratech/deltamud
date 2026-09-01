// password.rs — full port of C `src/password.c` (DeltaMUD secure password layer).
//
// The C file leans on the libc `crypt(3)` for three on-disk formats:
//   * legacy DES (13-char, or 10-char truncated leftovers from old player files)
//   * `$5$` SHA-256 crypt (the modern format produced by create_secure_password_hash)
//   * `$6$` SHA-512 crypt (accepted on verify; some imported accounts use it)
// plus the DeltaMUD `pwd_new` SHA path stored in `player_main.pwd` when
// `pwd_new == 1`: a bare lowercase-hex SHA-256 of the plaintext (see the C/Rust
// DB layers — database.rs::hash_password and database_compat.rs).
//
// Rust has no libc `crypt` in this build (only `sha2` is on the dependency
// list), so every algorithm is reimplemented here from scratch against the
// glibc/BSD specs so existing player files keep verifying byte-for-byte:
//   * `des_crypt`  — the classic Unix DES password hash (Morris/Thompson),
//                    producing the 13-char `[./0-9A-Za-z]{13}` output.
//   * `sha_crypt`  — Ulrich Drepper's SHA-crypt (`$5$`/`$6$`), built on sha2.
//   * `sha256_hex` — the DeltaMUD pwd_new bare hex digest.
//
// Public API (mirrors the C trio, adapted to the Rust call sites):
//   pub fn check_password(stored: &str, plain: &str) -> bool   (verify_password)
//   pub fn hash_password(plain: &str) -> String                (create_secure_password_hash)
//   pub fn password_needs_upgrade(stored: &str) -> bool        (password_needs_upgrade)
//
// No GameState/Character coupling and no command handlers: this is a pure
// crypto helper module keyed by nothing but its &str arguments.

use sha2::{Digest, Sha256, Sha512};

// ===========================================================================
// Public API
// ===========================================================================

/// Verify `plain` against the on-disk `stored` hash, with backward
/// compatibility for every historical DeltaMUD format. Port of C
/// `verify_password()` (the C signature also took `username`, used only for the
/// crypt-unavailable fallback, which cannot occur here — our crypt is always
/// present — so the parameter is dropped). All comparisons are constant-time.
pub fn check_password(stored: &str, plain: &str) -> bool {
    if stored.is_empty() {
        return false;
    }

    // Legacy DES hash: 13-char full, or 10-char truncated (old player files).
    let len = stored.len();
    if (len == 13 || len == 10) && is_des_hash(stored) {
        // glibc/BSD DES uses the first two output chars as the salt.
        let salt = &stored[..2];
        let computed = des_crypt(plain.as_bytes(), salt);
        if len == 10 {
            // Truncated hash: compare only the first 10 chars (C strncmp 10).
            return computed.len() >= 10 && ct_eq(&computed.as_bytes()[..10], stored.as_bytes());
        }
        return ct_eq(computed.as_bytes(), stored.as_bytes());
    }

    // Modern SHA-crypt: `$5$` (SHA-256) and `$6$` (SHA-512).
    if stored.starts_with("$5$") || stored.starts_with("$6$") {
        if let Some(computed) = sha_crypt(plain.as_bytes(), stored) {
            return ct_eq(computed.as_bytes(), stored.as_bytes());
        }
        return false;
    }

    // DeltaMUD pwd_new path: bare lowercase-hex SHA-256 (64 hex chars).
    if len == 64 && is_lower_hex(stored) {
        return ct_eq(sha256_hex(plain).as_bytes(), stored.as_bytes());
    }

    // Unknown format: C falls back to crypt(). The only crypt format that can
    // round-trip an arbitrary leading string here is DES (salt = first 2
    // chars), matching the C "try legacy verification as fallback" branch.
    if is_des_hash(stored) && stored.len() >= 2 {
        let salt = &stored[..2];
        let computed = des_crypt(plain.as_bytes(), salt);
        return ct_eq(computed.as_bytes(), stored.as_bytes());
    }

    false
}

/// Create a fresh secure hash for `plain`. Port of C
/// `create_secure_password_hash()`: a `$5$` SHA-256 crypt with a random 16-char
/// salt from the crypt alphabet. (The C version also took a `username`
/// fallback for the never-reached crypt-failure path; omitted here.)
pub fn hash_password(plain: &str) -> String {
    let salt = generate_salt();
    let setting = format!("$5${}$", salt);
    // sha_crypt cannot fail for a well-formed `$5$` setting we just built.
    sha_crypt(plain.as_bytes(), &setting).unwrap_or_else(|| {
        // Theoretically unreachable; degrade to bare SHA-256 hex rather than panic.
        sha256_hex(plain)
    })
}

/// Whether `stored` is in a legacy format that should be re-hashed on the next
/// successful login. Port of C `password_needs_upgrade()`, extended for the
/// DeltaMUD bare-SHA-256 path (treated as up-to-date, like `$5$`/`$6$`).
pub fn password_needs_upgrade(stored: &str) -> bool {
    if stored.is_empty() {
        return true; // No hash = needs upgrade.
    }
    let len = stored.len();
    // DES hashes (10 or 13 chars) always need upgrade.
    if (len == 13 || len == 10) && is_des_hash(stored) {
        return true;
    }
    // Modern crypt formats are current.
    if stored.starts_with("$5$") || stored.starts_with("$6$") {
        return false;
    }
    // DeltaMUD pwd_new bare SHA-256: a modern format, no upgrade required.
    if len == 64 && is_lower_hex(stored) {
        return false;
    }
    // Unknown format - assume needs upgrade (matches C default).
    true
}

// ===========================================================================
// Bare SHA-256 hex (DeltaMUD pwd_new path)
// ===========================================================================

fn sha256_hex(plain: &str) -> String {
    let digest = Sha256::digest(plain.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn is_lower_hex(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ===========================================================================
// Salt generation (C generate_salt)
// ===========================================================================

/// The crypt base-64 alphabet used by glibc for salts (`./0-9A-Za-z`), and by
/// the C `generate_salt()` (which lists it as A-Za-z0-9./ — same 64 chars).
const SALT_CHARS: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Generate a 16-char random salt. C uses `rand()` seeded once from
/// `time(NULL) ^ getpid()`; we use a lazily-seeded xorshift so the module has
/// no external RNG dependency and stays self-contained.
fn generate_salt() -> String {
    let mut s = String::with_capacity(16);
    for _ in 0..16 {
        let r = next_rand();
        s.push(SALT_CHARS[(r % 64) as usize] as char);
    }
    s
}

use std::cell::Cell;
use std::time::{SystemTime, UNIX_EPOCH};

thread_local! {
    static RNG_STATE: Cell<u64> = const { Cell::new(0) };
}

fn next_rand() -> u64 {
    RNG_STATE.with(|st| {
        let mut x = st.get();
        if x == 0 {
            // Seed from wall clock + a per-process-ish nonce; xorshift never
            // produces 0 from a non-zero seed.
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E3779B97F4A7C15);
            x = nanos
                ^ 0xD1B54A32D192ED03
                ^ (std::process::id() as u64).wrapping_mul(0x2545F491_4F6CDD1D);
            if x == 0 {
                x = 0x9E3779B97F4A7C15;
            }
        }
        // xorshift64
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        st.set(x);
        x
    })
}

// ===========================================================================
// Constant-time comparison
// ===========================================================================

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

// ===========================================================================
// SHA-crypt ($5$ / $6$) — Ulrich Drepper's specification.
//   https://www.akkadia.org/drepper/SHA-crypt.txt
// Implemented generically over Sha256 (32-byte, `$5$`) and Sha512 (64-byte,
// `$6$`). Returns the full crypt string (matching the `setting`'s salt/rounds)
// so it can be compared against `stored`.
// ===========================================================================

/// Parse a `$5$`/`$6$` setting and recompute the hash for `password`.
/// `setting` may be a full stored hash (its trailing hash part is ignored) or a
/// bare `$id$[rounds=N$]salt$` setting (as produced by hash_password).
fn sha_crypt(password: &[u8], setting: &str) -> Option<String> {
    let bytes = setting.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'$' || bytes[2] != b'$' {
        return None;
    }
    let id = bytes[1];
    let (output_len, is512) = match id {
        b'5' => (32usize, false),
        b'6' => (64usize, true),
        _ => return None,
    };

    // After "$N$": optional "rounds=NNN$", then salt up to next '$' or end,
    // capped at 16 bytes.
    let mut rest = &setting[3..];
    let mut rounds: u64 = 5000;
    let mut rounds_custom = false;
    if let Some(after) = rest.strip_prefix("rounds=") {
        // Read decimal digits up to the next '$'.
        let end = after.find('$').unwrap_or(after.len());
        let (digits, tail) = after.split_at(end);
        if let Ok(v) = digits.parse::<u64>() {
            rounds = v.clamp(1000, 999_999_999);
            rounds_custom = true;
            rest = tail.strip_prefix('$').unwrap_or(tail);
        } else {
            return None;
        }
    }
    // Salt runs to the next '$' (start of the stored hash) or end of string.
    let salt_end = rest.find('$').unwrap_or(rest.len());
    let salt = &rest.as_bytes()[..salt_end.min(16)];

    let result = if is512 {
        sha_crypt_inner::<Sha512>(password, salt, rounds, output_len, &SHA512_PERM)
    } else {
        sha_crypt_inner::<Sha256>(password, salt, rounds, output_len, &SHA256_PERM)
    };

    // Reassemble the crypt string with the same prefix the input used.
    let mut out = String::new();
    out.push('$');
    out.push(id as char);
    out.push('$');
    if rounds_custom {
        out.push_str("rounds=");
        out.push_str(&rounds.to_string());
        out.push('$');
    }
    out.push_str(std::str::from_utf8(salt).ok()?);
    out.push('$');
    out.push_str(&result);
    Some(out)
}

/// Drepper SHA-crypt core, generic over the digest D (Sha256 or Sha512).
fn sha_crypt_inner<D: Digest>(
    password: &[u8],
    salt: &[u8],
    rounds: u64,
    out_len: usize,
    perm: &[(usize, usize, usize, u8)],
) -> String {
    let plen = password.len();
    let slen = salt.len();

    // Digest A: password + salt + (digest B repeated)
    // Digest B: password + salt + password
    let mut b = D::new();
    b.update(password);
    b.update(salt);
    b.update(password);
    let alt = b.finalize();
    let alt = alt.as_slice(); // length == out_len

    let mut a = D::new();
    a.update(password);
    a.update(salt);
    // For each block of out_len of password length, add alt; remainder partial.
    let mut cnt = plen;
    while cnt > out_len {
        a.update(&alt[..out_len]);
        cnt -= out_len;
    }
    a.update(&alt[..cnt]);
    // Take the binary representation of plen; for every 1-bit add alt, for 0-bit
    // add the password.
    let mut n = plen;
    while n > 0 {
        if n & 1 != 0 {
            a.update(&alt[..out_len]);
        } else {
            a.update(password);
        }
        n >>= 1;
    }
    let digest_a = a.finalize();
    let digest_a = digest_a.as_slice();

    // DP: P = password repeated, derived from a digest of password*plen.
    let mut dp = D::new();
    for _ in 0..plen {
        dp.update(password);
    }
    let dp_digest = dp.finalize();
    let p = produce_sequence(dp_digest.as_slice(), plen, out_len);

    // DS: S = salt sequence, from a digest of salt repeated (16 + first byte of A).
    let mut ds = D::new();
    let reps = 16 + digest_a[0] as usize;
    for _ in 0..reps {
        ds.update(salt);
    }
    let ds_digest = ds.finalize();
    let s = produce_sequence(ds_digest.as_slice(), slen, out_len);

    // The rounds loop.
    let mut c = digest_a.to_vec();
    for i in 0..rounds {
        let mut ctx = D::new();
        if i & 1 != 0 {
            ctx.update(&p);
        } else {
            ctx.update(&c);
        }
        if i % 3 != 0 {
            ctx.update(&s);
        }
        if i % 7 != 0 {
            ctx.update(&p);
        }
        if i & 1 != 0 {
            ctx.update(&c);
        } else {
            ctx.update(&p);
        }
        let next = ctx.finalize();
        c.clear();
        c.extend_from_slice(next.as_slice());
    }

    // Base-64 encode using the per-algorithm permutation order.
    b64_from_24bit(&c, perm)
}

/// Build the P or S byte sequence: `len` bytes, taken by repeating the
/// `out_len`-byte digest (each repeat copies min(out_len, remaining) bytes).
fn produce_sequence(digest: &[u8], len: usize, out_len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut remaining = len;
    while remaining >= out_len {
        v.extend_from_slice(&digest[..out_len]);
        remaining -= out_len;
    }
    v.extend_from_slice(&digest[..remaining]);
    v
}

/// crypt's base-64 alphabet (note: NOT the standard MIME order).
const CRYPT_B64: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Sentinel meaning "literal zero byte" in a b64 group (glibc passes 0 for the
/// missing high byte of the final 2-byte group).
const ZERO: usize = usize::MAX;

/// Encode the final digest into crypt's base-64, replaying glibc's exact
/// sequence of `b64_from_24bit(B2, B1, B0, N)` calls. Each `calls` entry is
/// `(b2, b1, b0, n)`: byte indices into `buf` (or `ZERO` for a literal 0) and
/// the number of output chars. glibc packs `w = (B2<<16)|(B1<<8)|B0` and emits
/// the low six bits first.
fn b64_from_24bit(buf: &[u8], calls: &[(usize, usize, usize, u8)]) -> String {
    let mut out = String::new();
    let fetch = |idx: usize| -> u32 { if idx == ZERO { 0 } else { buf[idx] as u32 } };
    for &(b2, b1, b0, n) in calls {
        let mut w = (fetch(b2) << 16) | (fetch(b1) << 8) | fetch(b0);
        for _ in 0..n {
            out.push(CRYPT_B64[(w & 0x3f) as usize] as char);
            w >>= 6;
        }
    }
    out
}

// glibc's SHA-256 output encoding sequence (sha256-crypt.c). 10 full
// (B2,B1,B0,4) calls + the final (0, buf[31], buf[30], 3) call. Total 43 chars.
const SHA256_PERM: [(usize, usize, usize, u8); 11] = [
    (0, 10, 20, 4),
    (21, 1, 11, 4),
    (12, 22, 2, 4),
    (3, 13, 23, 4),
    (24, 4, 14, 4),
    (15, 25, 5, 4),
    (6, 16, 26, 4),
    (27, 7, 17, 4),
    (18, 28, 8, 4),
    (9, 19, 29, 4),
    (ZERO, 31, 30, 3),
];

// glibc's SHA-512 output encoding sequence (sha512-crypt.c). 21 full
// (B2,B1,B0,4) calls + the final (0, 0, buf[63], 2) call. Total 86 chars.
const SHA512_PERM: [(usize, usize, usize, u8); 22] = [
    (0, 21, 42, 4),
    (22, 43, 1, 4),
    (44, 2, 23, 4),
    (3, 24, 45, 4),
    (25, 46, 4, 4),
    (47, 5, 26, 4),
    (6, 27, 48, 4),
    (28, 49, 7, 4),
    (50, 8, 29, 4),
    (9, 30, 51, 4),
    (31, 52, 10, 4),
    (53, 11, 32, 4),
    (12, 33, 54, 4),
    (34, 55, 13, 4),
    (56, 14, 35, 4),
    (15, 36, 57, 4),
    (37, 58, 16, 4),
    (59, 17, 38, 4),
    (18, 39, 60, 4),
    (40, 61, 19, 4),
    (62, 20, 41, 4),
    (ZERO, ZERO, 63, 2),
];

// ===========================================================================
// Legacy DES crypt — classic Unix password hashing (Morris & Thompson).
// Faithful port of the libdes/BSD crypt(3) algorithm: 25 rounds of a salt-
// perturbed DES on an all-zero block, output as 13 base-64 chars (2 salt + 11).
// ===========================================================================

fn is_des_hash(s: &str) -> bool {
    // DES hashes use only the crypt base-64 alphabet and never start with '$'.
    !s.is_empty()
        && !s.starts_with('$')
        && s.bytes().all(|b| {
            b == b'.'
                || b == b'/'
                || b.is_ascii_digit()
                || b.is_ascii_uppercase()
                || b.is_ascii_lowercase()
        })
}

/// Compute the 13-char DES crypt of `key` under the 2-char `salt`.
fn des_crypt(key: &[u8], salt: &str) -> String {
    let salt_bytes = salt.as_bytes();
    let c0 = if salt_bytes.is_empty() {
        b'.'
    } else {
        salt_bytes[0]
    };
    let c1 = if salt_bytes.len() < 2 {
        b'.'
    } else {
        salt_bytes[1]
    };

    // Build the 56-bit key schedule input from the (up to 8) low-7-bit chars,
    // each shifted left by one (DES ignores the low bit / parity bit).
    let mut keyblock = [0u8; 8];
    for (i, kb) in keyblock.iter_mut().enumerate() {
        let ch = if i < key.len() { key[i] } else { 0 };
        *kb = (ch << 1) & 0xfe;
    }

    let ks = des_set_key(&keyblock);

    // Decode the salt into a 24-bit perturbation mask (E-box swap bits).
    let saltbits = (a64_to_int(c0) | (a64_to_int(c1) << 6)) as u32;

    // 25 iterations of salted-DES on an all-zero 64-bit block.
    let mut block: u64 = 0;
    for _ in 0..25 {
        block = des_encrypt(block, &ks, saltbits);
    }

    // Output: 2 salt chars then 11 base-64 chars of the 64-bit result, packed
    // big-endian in 6-bit groups (the final char carries only 2 bits).
    let mut out = String::with_capacity(13);
    out.push(c0 as char);
    out.push(c1 as char);

    // Spread the 64-bit block into the 11-char tail. glibc packs the 64 output
    // bits MSB-first into 6-bit chunks; the last chunk uses the low 2 bits.
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&block.to_be_bytes());
    let mut bitpos = 0usize;
    for _ in 0..11 {
        let mut v = 0u8;
        for _ in 0..6 {
            let byte_index = bitpos / 8;
            let bit_index = 7 - (bitpos % 8);
            let bit = if byte_index < 8 {
                (bytes[byte_index] >> bit_index) & 1
            } else {
                0
            };
            v = (v << 1) | bit;
            bitpos += 1;
        }
        out.push(CRYPT_B64[v as usize] as char);
    }
    out
}

/// Map a crypt base-64 character to its 0..63 value.
fn a64_to_int(c: u8) -> u64 {
    match c {
        b'.' => 0,
        b'/' => 1,
        b'0'..=b'9' => (c - b'0' + 2) as u64,
        b'A'..=b'Z' => (c - b'A' + 12) as u64,
        b'a'..=b'z' => (c - b'a' + 38) as u64,
        _ => 0,
    }
}

// ---- DES primitive tables -------------------------------------------------

// Initial permutation.
const IP: [u8; 64] = [
    58, 50, 42, 34, 26, 18, 10, 2, 60, 52, 44, 36, 28, 20, 12, 4, 62, 54, 46, 38, 30, 22, 14, 6,
    64, 56, 48, 40, 32, 24, 16, 8, 57, 49, 41, 33, 25, 17, 9, 1, 59, 51, 43, 35, 27, 19, 11, 3, 61,
    53, 45, 37, 29, 21, 13, 5, 63, 55, 47, 39, 31, 23, 15, 7,
];

// Final permutation (inverse of IP).
const FP: [u8; 64] = [
    40, 8, 48, 16, 56, 24, 64, 32, 39, 7, 47, 15, 55, 23, 63, 31, 38, 6, 46, 14, 54, 22, 62, 30,
    37, 5, 45, 13, 53, 21, 61, 29, 36, 4, 44, 12, 52, 20, 60, 28, 35, 3, 43, 11, 51, 19, 59, 27,
    34, 2, 42, 10, 50, 18, 58, 26, 33, 1, 41, 9, 49, 17, 57, 25,
];

// Expansion (32 -> 48).
const E: [u8; 48] = [
    32, 1, 2, 3, 4, 5, 4, 5, 6, 7, 8, 9, 8, 9, 10, 11, 12, 13, 12, 13, 14, 15, 16, 17, 16, 17, 18,
    19, 20, 21, 20, 21, 22, 23, 24, 25, 24, 25, 26, 27, 28, 29, 28, 29, 30, 31, 32, 1,
];

// Permutation P after S-boxes.
const P: [u8; 32] = [
    16, 7, 20, 21, 29, 12, 28, 17, 1, 15, 23, 26, 5, 18, 31, 10, 2, 8, 24, 14, 32, 27, 3, 9, 19,
    13, 30, 6, 22, 11, 4, 25,
];

// Permuted choice 1 (64 -> 56).
const PC1: [u8; 56] = [
    57, 49, 41, 33, 25, 17, 9, 1, 58, 50, 42, 34, 26, 18, 10, 2, 59, 51, 43, 35, 27, 19, 11, 3, 60,
    52, 44, 36, 63, 55, 47, 39, 31, 23, 15, 7, 62, 54, 46, 38, 30, 22, 14, 6, 61, 53, 45, 37, 29,
    21, 13, 5, 28, 20, 12, 4,
];

// Permuted choice 2 (56 -> 48).
const PC2: [u8; 48] = [
    14, 17, 11, 24, 1, 5, 3, 28, 15, 6, 21, 10, 23, 19, 12, 4, 26, 8, 16, 7, 27, 20, 13, 2, 41, 52,
    31, 37, 47, 55, 30, 40, 51, 45, 33, 48, 44, 49, 39, 56, 34, 53, 46, 42, 50, 36, 29, 32,
];

// Left-shift schedule per round.
const SHIFTS: [u8; 16] = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];

// The eight S-boxes.
const SBOX: [[u8; 64]; 8] = [
    [
        14, 4, 13, 1, 2, 15, 11, 8, 3, 10, 6, 12, 5, 9, 0, 7, 0, 15, 7, 4, 14, 2, 13, 1, 10, 6, 12,
        11, 9, 5, 3, 8, 4, 1, 14, 8, 13, 6, 2, 11, 15, 12, 9, 7, 3, 10, 5, 0, 15, 12, 8, 2, 4, 9,
        1, 7, 5, 11, 3, 14, 10, 0, 6, 13,
    ],
    [
        15, 1, 8, 14, 6, 11, 3, 4, 9, 7, 2, 13, 12, 0, 5, 10, 3, 13, 4, 7, 15, 2, 8, 14, 12, 0, 1,
        10, 6, 9, 11, 5, 0, 14, 7, 11, 10, 4, 13, 1, 5, 8, 12, 6, 9, 3, 2, 15, 13, 8, 10, 1, 3, 15,
        4, 2, 11, 6, 7, 12, 0, 5, 14, 9,
    ],
    [
        10, 0, 9, 14, 6, 3, 15, 5, 1, 13, 12, 7, 11, 4, 2, 8, 13, 7, 0, 9, 3, 4, 6, 10, 2, 8, 5,
        14, 12, 11, 15, 1, 13, 6, 4, 9, 8, 15, 3, 0, 11, 1, 2, 12, 5, 10, 14, 7, 1, 10, 13, 0, 6,
        9, 8, 7, 4, 15, 14, 3, 11, 5, 2, 12,
    ],
    [
        7, 13, 14, 3, 0, 6, 9, 10, 1, 2, 8, 5, 11, 12, 4, 15, 13, 8, 11, 5, 6, 15, 0, 3, 4, 7, 2,
        12, 1, 10, 14, 9, 10, 6, 9, 0, 12, 11, 7, 13, 15, 1, 3, 14, 5, 2, 8, 4, 3, 15, 0, 6, 10, 1,
        13, 8, 9, 4, 5, 11, 12, 7, 2, 14,
    ],
    [
        2, 12, 4, 1, 7, 10, 11, 6, 8, 5, 3, 15, 13, 0, 14, 9, 14, 11, 2, 12, 4, 7, 13, 1, 5, 0, 15,
        10, 3, 9, 8, 6, 4, 2, 1, 11, 10, 13, 7, 8, 15, 9, 12, 5, 6, 3, 0, 14, 11, 8, 12, 7, 1, 14,
        2, 13, 6, 15, 0, 9, 10, 4, 5, 3,
    ],
    [
        12, 1, 10, 15, 9, 2, 6, 8, 0, 13, 3, 4, 14, 7, 5, 11, 10, 15, 4, 2, 7, 12, 9, 5, 6, 1, 13,
        14, 0, 11, 3, 8, 9, 14, 15, 5, 2, 8, 12, 3, 7, 0, 4, 10, 1, 13, 11, 6, 4, 3, 2, 12, 9, 5,
        15, 10, 11, 14, 1, 7, 6, 0, 8, 13,
    ],
    [
        4, 11, 2, 14, 15, 0, 8, 13, 3, 12, 9, 7, 5, 10, 6, 1, 13, 0, 11, 7, 4, 9, 1, 10, 14, 3, 5,
        12, 2, 15, 8, 6, 1, 4, 11, 13, 12, 3, 7, 14, 10, 15, 6, 8, 0, 5, 9, 2, 6, 11, 13, 8, 1, 4,
        10, 7, 9, 5, 0, 15, 14, 2, 3, 12,
    ],
    [
        13, 2, 8, 4, 6, 15, 11, 1, 10, 9, 3, 14, 5, 0, 12, 7, 1, 15, 13, 8, 10, 3, 7, 4, 12, 5, 6,
        11, 0, 14, 9, 2, 7, 11, 4, 1, 9, 12, 14, 2, 0, 6, 10, 13, 15, 3, 5, 8, 2, 1, 14, 7, 4, 10,
        8, 13, 15, 12, 9, 0, 3, 5, 6, 11,
    ],
];

// ---- DES helpers ----------------------------------------------------------

/// Read bit `pos` (1-based, MSB-first) from a 64-bit value.
fn get_bit(v: u64, pos: u8) -> u64 {
    (v >> (64 - pos as u64)) & 1
}

/// Permute `input` (treated MSB-first over 64 bits) using a permutation table,
/// producing an n-bit result packed MSB-first into the low bits.
fn permute(input: u64, table: &[u8]) -> u64 {
    let mut out = 0u64;
    for &p in table {
        out = (out << 1) | get_bit(input, p);
    }
    out
}

/// Compute the 16 round subkeys (each 48 bits) from the 64-bit keyblock.
fn des_set_key(keyblock: &[u8; 8]) -> [u64; 16] {
    let key = u64::from_be_bytes(*keyblock);
    let permuted = permute(key, &PC1); // 56 bits
    // Split into C (left 28) and D (right 28).
    let mut c = (permuted >> 28) & 0x0fff_ffff;
    let mut d = permuted & 0x0fff_ffff;
    let mut subkeys = [0u64; 16];
    for round in 0..16 {
        let s = SHIFTS[round];
        c = rotl28(c, s);
        d = rotl28(d, s);
        let cd = (c << 28) | d; // 56 bits
        subkeys[round] = permute_bits(cd, &PC2, 56);
    }
    subkeys
}

/// Left-rotate a 28-bit value by `n` positions.
fn rotl28(v: u64, n: u8) -> u64 {
    let n = n as u64;
    ((v << n) | (v >> (28 - n))) & 0x0fff_ffff
}

/// Permute treating `input` as a `width`-bit MSB-first value.
fn permute_bits(input: u64, table: &[u8], width: u8) -> u64 {
    let mut out = 0u64;
    for &p in table {
        let bit = (input >> (width as u64 - p as u64)) & 1;
        out = (out << 1) | bit;
    }
    out
}

/// One salted-DES encryption of a 64-bit block with the precomputed subkeys.
/// `saltbits` perturbs the E-box output (the BSD/glibc salt twist).
fn des_encrypt(block: u64, subkeys: &[u64; 16], saltbits: u32) -> u64 {
    let ip = permute(block, &IP); // 64 bits
    let mut l = (ip >> 32) & 0xffff_ffff;
    let mut r = ip & 0xffff_ffff;
    for &subkey in subkeys.iter() {
        let f = des_f(r, subkey, saltbits);
        let new_r = l ^ f;
        l = r;
        r = new_r;
    }
    // Pre-output is R||L (note the swap), then final permutation.
    let preoutput = (r << 32) | l;
    permute(preoutput, &FP)
}

/// The DES f-function with salt perturbation.
fn des_f(r: u64, subkey: u64, saltbits: u32) -> u64 {
    // Expand R (32 -> 48).
    let mut expanded = permute_bits(r, &E, 32); // 48 bits

    // Apply the crypt salt twist: for each set salt bit i (0..24), swap the i-th
    // and (i+24)-th bits of the 48-bit expanded value.
    if saltbits != 0 {
        let mut bits = [0u8; 48];
        for (idx, b) in bits.iter_mut().enumerate() {
            *b = ((expanded >> (47 - idx)) & 1) as u8;
        }
        for i in 0..24 {
            if (saltbits >> i) & 1 != 0 {
                bits.swap(i, i + 24);
            }
        }
        let mut v = 0u64;
        for &b in bits.iter() {
            v = (v << 1) | b as u64;
        }
        expanded = v;
    }

    let xored = expanded ^ subkey; // 48 bits

    // S-box substitution: 8 groups of 6 bits -> 4 bits.
    let mut sout = 0u32;
    for i in 0..8 {
        let six = ((xored >> (42 - i * 6)) & 0x3f) as usize;
        // Row = outer two bits, col = inner four bits.
        let row = ((six & 0x20) >> 4) | (six & 0x01);
        let col = (six >> 1) & 0x0f;
        let val = SBOX[i][row * 16 + col] as u32;
        sout = (sout << 4) | val;
    }

    // P permutation (32 -> 32).
    permute_bits(sout as u64, &P, 32)
}

// ===========================================================================
// Tests — verify against known crypt(3) and Drepper test vectors.
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn des_known_vector() {
        // glibc/BSD crypt("foob", "ba") family. Canonical: crypt("foo","ba") ==
        // "ba4TuD1iozTxw" on standard DES implementations.
        assert_eq!(des_crypt(b"foo", "ba"), "ba4TuD1iozTxw");
        // crypt("password", "ab") == "abJnggxhB/yWI" (verified against libcrypt)
        assert_eq!(des_crypt(b"password", "ab"), "abJnggxhB/yWI");
        // crypt("secret", "Zx") == "ZxzAXQJc1jx/Q" (verified against libcrypt)
        assert_eq!(des_crypt(b"secret", "Zx"), "ZxzAXQJc1jx/Q");
    }

    #[test]
    fn des_check_roundtrip() {
        let stored = des_crypt(b"secret", "Zx");
        assert!(check_password(&stored, "secret"));
        assert!(!check_password(&stored, "Secret"));
        // 10-char truncated form still verifies against the right password.
        let trunc = &stored[..10];
        assert!(check_password(trunc, "secret"));
    }

    #[test]
    fn sha256_crypt_known_vector() {
        // Drepper $5$ test vector.
        let stored = "$5$saltstring$5B8vYYiY.CVt1RlTTf8KbXBH3hsxY/GNooZaBBGWEc5";
        assert_eq!(sha_crypt(b"Hello world!", stored).as_deref(), Some(stored));
        assert!(check_password(stored, "Hello world!"));
        assert!(!check_password(stored, "Hello world?"));
    }

    #[test]
    fn sha256_crypt_rounds_vector() {
        let stored = "$5$rounds=10000$saltstringsaltst$3xv.VbSHBb41AL9AvLeujZkZRBAwqFMz2.opqey6IcA";
        assert_eq!(sha_crypt(b"Hello world!", stored).as_deref(), Some(stored));
        assert!(check_password(stored, "Hello world!"));
    }

    #[test]
    fn sha512_crypt_known_vector() {
        let stored = "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1";
        assert_eq!(sha_crypt(b"Hello world!", stored).as_deref(), Some(stored));
        assert!(check_password(stored, "Hello world!"));
        assert!(!check_password(stored, "wrong"));
    }

    #[test]
    fn bare_sha256_hex_path() {
        // DeltaMUD pwd_new: bare lowercase hex SHA-256.
        let stored = sha256_hex("letmein");
        assert_eq!(stored.len(), 64);
        assert!(check_password(&stored, "letmein"));
        assert!(!check_password(&stored, "letmeout"));
        assert!(!password_needs_upgrade(&stored));
    }

    #[test]
    fn hash_then_check_roundtrip() {
        let h = hash_password("hunter2");
        assert!(h.starts_with("$5$"));
        assert!(check_password(&h, "hunter2"));
        assert!(!check_password(&h, "hunter3"));
        assert!(!password_needs_upgrade(&h));
    }

    #[test]
    fn needs_upgrade_classification() {
        assert!(password_needs_upgrade(""));
        assert!(password_needs_upgrade("ba4TuD1iozTxw")); // 13-char DES
        assert!(password_needs_upgrade("ba4TuD1ioz")); // 10-char DES
        assert!(!password_needs_upgrade("$5$saltstring$abc"));
        assert!(!password_needs_upgrade("$6$saltstring$abc"));
    }
}
