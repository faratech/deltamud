// password.rs — full port of C `src/password.c` (DeltaMUD secure password layer).
//
// The C file leans on the libc `crypt(3)` for three legacy on-disk formats:
//   * legacy DES (13-char, or 10-char truncated leftovers from old player files)
//   * `$5$` SHA-256 crypt
//   * `$6$` SHA-512 crypt (accepted on verify; some imported accounts use it)
// plus the DeltaMUD `pwd_new` SHA path stored in `player_main.pwd` when
// `pwd_new == 1`: a bare lowercase-hex SHA-256 of the plaintext (see the C/Rust
// DB layers — database.rs::hash_password and database_compat.rs).
//
// Existing player files keep verifying byte-for-byte via the legacy routines
// below. New hashes use RustCrypto Argon2id in PHC string format with a salt
// supplied by the operating system CSPRNG:
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

use argon2::{
    Argon2, Params, Version,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use sha2::{Digest, Sha256, Sha512};
use std::sync::{Arc, OnceLock};

// Stored hashes are database input at verification time. Bound every
// attacker-influenced work factor before invoking a password KDF so a corrupt
// or imported row cannot turn one login attempt into unbounded CPU/memory use.
const MAX_ARGON2_M_COST: u32 = 65_536;
const MAX_ARGON2_T_COST: u32 = 4;
const MAX_ARGON2_P_COST: u32 = 4;
const MAX_ARGON2_OUTPUT_LEN: usize = 64;
const MAX_ARGON2_SALT_LEN: usize = 64;
const MAX_SHA_CRYPT_ROUNDS: u64 = 100_000;
/// Bound attacker-controlled verifier input before every supported KDF. Legacy
/// SHA-crypt work grows with both rounds and password length, and its
/// synchronous computation cannot be cancelled by an async timeout.
pub const MAX_PASSWORD_INPUT_BYTES: usize = 64;
const MAX_CONCURRENT_PASSWORD_CHECKS: usize = 2;

fn password_check_slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(
        SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PASSWORD_CHECKS))),
    )
}

/// Run an imported-password KDF outside Tokio's world/IO workers. The
/// process-wide semaphore bounds simultaneous memory/CPU, and oversized input
/// is rejected before waiting for a slot or spawning a blocking task.
pub async fn check_password_async(stored: String, plain: String) -> bool {
    if plain.len() > MAX_PASSWORD_INPUT_BYTES {
        return false;
    }
    let permit = match password_check_slots().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return false,
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        check_password(&stored, &plain)
    })
    .await
    .unwrap_or(false)
}

/// Create a new Argon2id credential outside the single-owner Game task. New
/// account creation and authenticated password changes share the same bounded
/// worker budget as verification, so neither path can stall world pulses or
/// multiply Argon2's memory cost without limit.
pub async fn hash_password_async(plain: String) -> Option<String> {
    if plain.len() > MAX_PASSWORD_INPUT_BYTES {
        return None;
    }
    let permit = password_check_slots().acquire_owned().await.ok()?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hash_password(&plain)
    })
    .await
    .ok()
}

// ===========================================================================
// Public API
// ===========================================================================

/// Verify `plain` against the on-disk `stored` hash, with backward
/// compatibility for every historical DeltaMUD format. Port of C
/// `verify_password()` (the C signature also took `username`, used only for the
/// crypt-unavailable fallback, which cannot occur here — our crypt is always
/// present — so the parameter is dropped). All comparisons are constant-time.
pub fn check_password(stored: &str, plain: &str) -> bool {
    if stored.is_empty() || plain.len() > MAX_PASSWORD_INPUT_BYTES {
        return false;
    }

    // New hashes are PHC-encoded Argon2id. Restrict verification to Argon2id:
    // other Argon2 variants are not formats this application has emitted.
    if stored.starts_with("$argon2id$") {
        return PasswordHash::new(stored).ok().is_some_and(|hash| {
            argon2id_verification_is_bounded(&hash)
                && Argon2::default()
                    .verify_password(plain.as_bytes(), &hash)
                    .is_ok()
        });
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

    // Legacy SHA-crypt: `$5$` (SHA-256) and `$6$` (SHA-512).
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

/// Create a fresh Argon2id PHC hash for `plain` using the RustCrypto defaults
/// and a 16-byte salt from the operating system CSPRNG.
pub fn hash_password(plain: &str) -> String {
    Argon2::default()
        .hash_password(plain.as_bytes())
        .expect("operating-system CSPRNG and Argon2id hashing must be available")
        .to_string()
}

/// Whether `stored` should be re-hashed on the next successful login. Every
/// historical format is legacy; only an Argon2id v19 hash meeting the current
/// minimum cost, salt, and output-size policy is current. An in-policy hash
/// whose cost dimensions are all at least the defaults remains current.
pub fn password_needs_upgrade(stored: &str) -> bool {
    argon2id_needs_upgrade(stored)
}

fn argon2id_needs_upgrade(stored: &str) -> bool {
    if !stored.starts_with("$argon2id$") {
        return true;
    }

    let Ok(hash) = PasswordHash::new(stored) else {
        return true;
    };
    if !argon2id_verification_is_bounded(&hash) {
        return true;
    }
    if hash.algorithm.as_str() != "argon2id"
        || hash.version != Some(Version::V0x13 as u32)
        || hash.params.get_decimal("m").is_none()
        || hash.params.get_decimal("t").is_none()
        || hash.params.get_decimal("p").is_none()
    {
        return true;
    }

    let Ok(params) = Params::try_from(&hash) else {
        return true;
    };
    let Some(salt) = hash.salt else {
        return true;
    };
    let Some(output) = hash.hash else {
        return true;
    };

    params.m_cost() < Params::DEFAULT_M_COST
        || params.t_cost() < Params::DEFAULT_T_COST
        || params.p_cost() < Params::DEFAULT_P_COST
        || salt.len() < 16
        || output.len() < Params::DEFAULT_OUTPUT_LEN
}

fn argon2id_verification_is_bounded(hash: &PasswordHash) -> bool {
    if hash.algorithm.as_str() != "argon2id" || hash.version != Some(Version::V0x13 as u32) {
        return false;
    }
    let Ok(params) = Params::try_from(hash) else {
        return false;
    };
    let (Some(salt), Some(output)) = (hash.salt, hash.hash) else {
        return false;
    };
    params.m_cost() <= MAX_ARGON2_M_COST
        && params.t_cost() <= MAX_ARGON2_T_COST
        && params.p_cost() <= MAX_ARGON2_P_COST
        && salt.len() <= MAX_ARGON2_SALT_LEN
        && output.len() <= MAX_ARGON2_OUTPUT_LEN
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
            if !(1000..=MAX_SHA_CRYPT_ROUNDS).contains(&v) {
                return None;
            }
            rounds = v;
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
        let stored = "1c8bfe8f801d79745c4631d09fff36c82aa37fc4cce4fc946683d7b336b63032";
        assert_eq!(sha256_hex("letmein"), stored);
        assert_eq!(stored.len(), 64);
        assert!(check_password(stored, "letmein"));
        assert!(!check_password(stored, "letmeout"));
        assert!(password_needs_upgrade(stored));
    }

    #[test]
    fn new_hashes_are_argon2id_with_os_random_salts() {
        let first = hash_password("hunter2");
        let second = hash_password("hunter2");

        assert!(first.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(second.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert_ne!(
            first, second,
            "independent CSPRNG salts must change the hash"
        );
        assert!(check_password(&first, "hunter2"));
        assert!(!check_password(&first, "hunter3"));
        assert!(!password_needs_upgrade(&first));

        let parsed = PasswordHash::new(&first).unwrap();
        assert_eq!(parsed.version, Some(Version::V0x13 as u32));
        assert_eq!(parsed.salt.unwrap().len(), 16);
        assert_eq!(parsed.hash.unwrap().len(), Params::DEFAULT_OUTPUT_LEN);
    }

    #[tokio::test]
    async fn bounded_async_workers_hash_and_verify_credentials() {
        let stored = hash_password_async("worker-secret".to_string())
            .await
            .expect("bounded hashing worker");
        assert!(check_password_async(stored.clone(), "worker-secret".to_string()).await);
        assert!(!check_password_async(stored, "wrong-secret".to_string()).await);
        assert!(
            hash_password_async("x".repeat(MAX_PASSWORD_INPUT_BYTES + 1))
                .await
                .is_none()
        );
    }

    #[test]
    fn official_argon2id_fixture_verifies_but_weak_parameters_require_rehash() {
        // RustCrypto's PHC-string test vector, adapted from the reference
        // Argon2 implementation: password="password", salt="somesalt".
        let stored =
            "$argon2id$v=19$m=256,t=2,p=1$c29tZXNhbHQ$nf65EOgLrQMR/uIPnA4rEsF5h7TKyQwu9U1bMCHGi/4";
        assert!(check_password(stored, "password"));
        assert!(!check_password(stored, "sassword"));
        assert!(password_needs_upgrade(stored));
    }

    #[test]
    fn imported_hash_work_factors_are_bounded_before_verification() {
        let current = hash_password("bounded-work");
        let too_much_memory = current.replacen("m=19456", "m=65537", 1);
        let too_many_iterations = current.replacen("t=2", "t=5", 1);
        let too_much_parallelism = current.replacen("p=1", "p=5", 1);

        for oversized in [
            too_much_memory.as_str(),
            too_many_iterations.as_str(),
            too_much_parallelism.as_str(),
        ] {
            assert!(!check_password(oversized, "bounded-work"));
            assert!(password_needs_upgrade(oversized));
        }

        let excessive_sha = "$5$rounds=100001$salt$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert_eq!(sha_crypt(b"password", excessive_sha), None);
        assert!(!check_password(excessive_sha, "password"));
    }

    #[test]
    fn verifier_rejects_oversized_plaintext_before_every_kdf() {
        let oversized = "x".repeat(MAX_PASSWORD_INPUT_BYTES + 1);
        let argon2 = hash_password(&oversized);
        let sha_crypt = sha_crypt(oversized.as_bytes(), "$5$rounds=1000$salt$").unwrap();
        let bare_sha = sha256_hex(&oversized);

        assert!(!check_password(&argon2, &oversized));
        assert!(!check_password(&sha_crypt, &oversized));
        assert!(!check_password(&bare_sha, &oversized));
    }

    #[test]
    fn every_legacy_fixture_verifies_and_requires_rehash() {
        let fixtures = [
            ("DES", "ba4TuD1iozTxw", "foo"),
            ("truncated DES", "ba4TuD1ioz", "foo"),
            (
                "SHA-256 crypt",
                "$5$saltstring$5B8vYYiY.CVt1RlTTf8KbXBH3hsxY/GNooZaBBGWEc5",
                "Hello world!",
            ),
            (
                "SHA-256 crypt with rounds",
                "$5$rounds=10000$saltstringsaltst$3xv.VbSHBb41AL9AvLeujZkZRBAwqFMz2.opqey6IcA",
                "Hello world!",
            ),
            (
                "SHA-512 crypt",
                "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1",
                "Hello world!",
            ),
            (
                "bare SHA-256",
                "1c8bfe8f801d79745c4631d09fff36c82aa37fc4cce4fc946683d7b336b63032",
                "letmein",
            ),
        ];

        for (label, stored, password) in fixtures {
            assert!(check_password(stored, password), "{label} fixture");
            assert!(
                !check_password(stored, "definitely-wrong"),
                "{label} fixture"
            );
            assert!(password_needs_upgrade(stored), "{label} fixture");
        }
    }

    #[test]
    fn rehash_policy_rejects_stale_or_malformed_argon2id_without_downgrading() {
        let current = hash_password("policy-test");
        assert!(!password_needs_upgrade(&current));

        let stronger_params = Params::new(
            Params::DEFAULT_M_COST,
            Params::DEFAULT_T_COST + 1,
            Params::DEFAULT_P_COST,
            Some(Params::DEFAULT_OUTPUT_LEN),
        )
        .unwrap();
        let stronger = Argon2::new(argon2::Algorithm::Argon2id, Version::V0x13, stronger_params)
            .hash_password_with_salt(b"policy-test", b"0123456789abcdef")
            .unwrap()
            .to_string();
        assert!(!password_needs_upgrade(&stronger));

        let weak_memory = current.replacen("m=19456", "m=19455", 1);
        let weak_iterations = current.replacen("t=2", "t=1", 1);
        let old_version = current.replacen("v=19", "v=16", 1);
        let mut fields: Vec<&str> = current.split('$').collect();
        fields[4] = "c29tZXNhbHQ"; // "somesalt": valid but only 8 bytes.
        let short_salt = fields.join("$");
        fields = current.split('$').collect();
        fields[5] = "AAAAAAAAAAAAAAAAAAAAAA"; // valid 16-byte output.
        let short_output = fields.join("$");
        let missing_parallelism = current.replacen(",p=1", "", 1);

        for stale in [
            weak_memory.as_str(),
            weak_iterations.as_str(),
            old_version.as_str(),
            short_salt.as_str(),
            short_output.as_str(),
            missing_parallelism.as_str(),
            "$argon2id$malformed",
            "$argon2i$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "$argon2d$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "",
            "not-a-password-hash",
        ] {
            assert!(
                password_needs_upgrade(stale),
                "unexpectedly current: {stale}"
            );
        }

        assert!(!check_password(
            "$argon2i$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "policy-test"
        ));
    }
}
