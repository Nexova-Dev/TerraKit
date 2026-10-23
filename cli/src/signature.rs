//! Signature verification and the keyring.
//!
//! `SECURITY.md` §3 makes the rule specific: a package is trusted if the manifest's Ed25519
//! signature verifies against a key the client already trusts. Nothing else substitutes for that.
//! In particular, a matching checksum proves the download arrived intact, not that it came from
//! TerraKit — an attacker who controls the registry can serve a self-consistent package with
//! correct digests, so checksum-without-signature is not verification.
//!
//! Two independent trust sources exist, and a signature from either is accepted:
//!
//! * the compiled-in development key, present so a fresh clone can exercise the whole path
//! * keys the user added, in `~/.config/terrakit/config.toml`
//!
//! Both are needed. Compiled-in alone means nobody can add a key without a new binary release;
//! file-only means a typo in a config file silently disables verification. A package signed by
//! neither is refused, with the reason naming both places a key could have come from.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::verify::hex_to_bytes;

/// A public key the client trusts, with the name it is trusted under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKey {
    pub key_id: String,
    pub public_key_hex: String,
    /// Where this key came from, for the error message. A refusal that does not say which
    /// keys were consulted sends the reader to the documentation instead of to the fix.
    pub source: KeySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// Compiled into the binary.
    BuiltIn,
    /// From the user's config file.
    Config,
}

impl KeySource {
    pub fn label(self) -> &'static str {
        match self {
            KeySource::BuiltIn => "built-in",
            KeySource::Config => "config",
        }
    }
}

/// The committed development public key.
///
/// Matches the legacy public fixture `keys/dev-ed25519.pub`. No private key is shipped.
/// Registry tests override this public key through an isolated configuration.
/// Production publishing rejects the development key ID.
pub const DEV_KEY_ID: &str = "terrakit-dev-2026";
pub const DEV_PUBLIC_KEY: &str = "4c16e2e2a4c0c57ebe4fe3484c69a7ba61577a22df6e3ec757c376d54f2cdf7b";

/// The subset of `CONFIG.md`'s TOML this file reads.
///
/// A full TOML parser is a dependency for a file with two keys in it. The grammar accepted here
/// is deliberately small and documented rather than approximately correct:
///
/// ```toml
/// [[keys]]
/// id = "terrakit-release-2026"
/// publicKey = "3d40..."
/// ```
///
/// Anything else is an error naming the line, not a silently ignored key. A configuration file
/// that is read but not understood is worse than one that is refused, because the user believes
/// their key is trusted when it is not.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub registry: Option<String>,
    pub cache_dir: Option<PathBuf>,
    pub keys: Vec<TrustedKey>,
}

impl Config {
    /// Load the user's config, or defaults when absent.
    ///
    /// An absent file is not an error: the built-in key still works, and most users never add a
    /// key. A *malformed* file is an error, because it means a key the user believes is trusted
    /// is not.
    pub fn load(path: &Path) -> Result<Config, String> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        parse_config(&raw, path)
    }

    pub fn keyring(&self) -> BTreeMap<String, TrustedKey> {
        let mut keys: BTreeMap<String, TrustedKey> = BTreeMap::new();
        keys.insert(
            DEV_KEY_ID.to_string(),
            TrustedKey {
                key_id: DEV_KEY_ID.to_string(),
                public_key_hex: DEV_PUBLIC_KEY.to_string(),
                source: KeySource::BuiltIn,
            },
        );
        for key in &self.keys {
            // A config key shadows the built-in one with the same id. Refusing to load instead
            // would make it impossible to *replace* a compromised development key, which is
            // the one thing a keyring has to allow.
            keys.insert(
                key.key_id.clone(),
                TrustedKey {
                    source: KeySource::Config,
                    ..key.clone()
                },
            );
        }
        keys
    }
}

fn parse_config(raw: &str, path: &Path) -> Result<Config, String> {
    let mut config = Config::default();
    let mut in_key_block = false;
    let mut current: Option<TrustedKey> = None;

    for (number, line) in raw.lines().enumerate() {
        let line_number = number + 1;
        let trimmed = line.split('#').next().unwrap_or("").trim();

        if trimmed.is_empty() {
            continue;
        }

        if trimmed == "[[keys]]" {
            if let Some(key) = current.take() {
                push_key(&mut config, key, path, line_number)?;
            }
            in_key_block = true;
            continue;
        }

        if trimmed.starts_with('[') {
            // Any other table ends the keys block. Silently ignoring it would mean a
            // misspelled `[[key]]` block reads as "no keys configured" and verification
            // silently falls back to the built-in key alone.
            if let Some(key) = current.take() {
                push_key(&mut config, key, path, line_number)?;
            }
            return Err(format!(
                "{}:{line_number}: unsupported table {trimmed}. This build reads only [[keys]].",
                path.display()
            ));
        }

        let Some((name, value)) = trimmed.split_once('=') else {
            return Err(format!(
                "{}:{line_number}: expected key = \"value\", got {trimmed:?}",
                path.display()
            ));
        };
        let name = name.trim();
        let value = unquote(value.trim());

        match (in_key_block, name) {
            (true, "id") => {
                let mut key = current.take().unwrap_or(TrustedKey {
                    key_id: String::new(),
                    public_key_hex: String::new(),
                    source: KeySource::Config,
                });
                key.key_id = value;
                current = Some(key);
            }
            (true, "publicKey") => {
                let mut key = current.take().unwrap_or(TrustedKey {
                    key_id: String::new(),
                    public_key_hex: String::new(),
                    source: KeySource::Config,
                });
                key.public_key_hex = value;
                current = Some(key);
            }
            (true, other) => {
                return Err(format!(
                    "{}:{line_number}: unknown field {other:?} in [[keys]]; this build reads \
                     only `id` and `publicKey`",
                    path.display()
                ))
            }
            (false, "registry") => config.registry = Some(value),
            (false, "cache_dir") | (false, "cacheDir") => config.cache_dir = Some(PathBuf::from(value)),
            (false, other) => {
                return Err(format!(
                    "{}:{line_number}: unknown key {other:?}; this build reads `registry`, \
                     `cache_dir` and `[[keys]]`",
                    path.display()
                ))
            }
        }
    }

    if let Some(key) = current.take() {
        push_key(&mut config, key, path, raw.lines().count())?;
    }
    Ok(config)
}

fn push_key(config: &mut Config, key: TrustedKey, path: &Path, line: usize) -> Result<(), String> {
    if key.key_id.is_empty() || key.public_key_hex.is_empty() {
        return Err(format!(
            "{}:{line}: a [[keys]] block needs both `id` and `publicKey`",
            path.display()
        ));
    }
    if hex_to_bytes(&key.public_key_hex).map(|b| b.len()) != Ok(32) {
        return Err(format!(
            "{}:{line}: publicKey for {:?} must be 32 bytes of hex (64 characters), got {}",
            path.display(),
            key.key_id,
            key.public_key_hex.len()
        ));
    }
    config.keys.push(key);
    Ok(())
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(trimmed)
        .to_string()
}

/// Default config path, honouring `TERRAKIT_CONFIG` for tests and CI.
pub fn config_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os("TERRAKIT_CONFIG") {
        return PathBuf::from(explicit);
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("terrakit").join("config.toml");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".config").join("terrakit").join("config.toml")
}

/// Why a signature was rejected.
///
/// Distinct because the caller's advice differs for each: an unknown key id means "add the key",
/// a known key with a bad signature means "do not add it", and a malformed signature means
/// "the artifact is damaged". Collapsing these into one message produces the worst outcome —
/// a user with a legitimate new release key responds to "unknown key" by adding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureFailure {
    /// No signature at all.
    Missing,
    /// Not a supported algorithm.
    UnsupportedAlgorithm(String),
    /// The key id is not in the keyring.
    UnknownKey {
        key_id: String,
        consulted: String,
    },
    /// The key is known and the signature does not verify.
    BadSignature { key_id: String },
    /// The signature document is structurally wrong.
    Malformed(String),
}

impl std::fmt::Display for SignatureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignatureFailure::Missing => write!(
                f,
                "the package has no signature.json. An unsigned package cannot be verified, and \
                 a checksum alone only proves the download arrived intact — not that TerraKit \
                 published it."
            ),
            SignatureFailure::UnsupportedAlgorithm(algorithm) => write!(
                f,
                "unsupported signature algorithm {algorithm:?}. This build verifies Ed25519 only."
            ),
            SignatureFailure::UnknownKey { key_id, consulted } => write!(
                f,
                "signed by key {key_id:?}, which is not trusted.\n       Keys consulted: {consulted}.\n\
                 \x20      To trust it, add [[keys]] with that id and publicKey to {}.\n\
                 \x20      Verify the public key through a channel independent of this download \
                 before you do — a key delivered alongside the package it signs proves nothing.",
                config_path().display()
            ),
            SignatureFailure::BadSignature { key_id } => write!(
                f,
                "the signature does not verify against key {key_id:?}.\n       The key is \
                 trusted, so either the package was modified after signing or the signature is \
                 corrupt. Either way this package must not be installed."
            ),
            SignatureFailure::Malformed(detail) => {
                write!(f, "signature.json is malformed: {detail}")
            }
        }
    }
}

/// Verify `signature_json` over `manifest_bytes` against the keyring.
pub fn verify(
    signature_json: &str,
    manifest_bytes: &[u8],
    keyring: &BTreeMap<String, TrustedKey>,
) -> Result<String, SignatureFailure> {
    // An empty or absent signature document is reported as Missing rather than Malformed,
    // because the advice differs: one means "this package was never signed", the other means
    // "the signature arrived damaged". serde would otherwise report both as a parse error and
    // the caller would suggest re-fetching an artifact that simply was never signed.
    if signature_json.trim().is_empty() {
        return Err(SignatureFailure::Missing);
    }

    let document: serde_json::Value =
        serde_json::from_str(signature_json).map_err(|e| SignatureFailure::Malformed(e.to_string()))?;

    let algorithm = document
        .get("algorithm")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SignatureFailure::Malformed("no `algorithm` field".into()))?;
    if algorithm != "ed25519" {
        return Err(SignatureFailure::UnsupportedAlgorithm(algorithm.to_string()));
    }

    let key_id = document
        .get("keyId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SignatureFailure::Malformed("no `keyId` field".into()))?;

    let signature_hex = document
        .get("signature")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SignatureFailure::Malformed("no `signature` field".into()))?;

    // The document may carry the public key so a user can see whose key signed it, but the
    // decision is made against the keyring. A package that supplied its own key would be
    // verified against itself, which is not verification at all.
    let Some(trusted) = keyring.get(key_id) else {
        let consulted = if keyring.is_empty() {
            "none (the keyring is empty)".to_string()
        } else {
            keyring
                .keys()
                .map(|id| format!("{id} [{}]", trusted_source(keyring, id)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(SignatureFailure::UnknownKey {
            key_id: key_id.to_string(),
            consulted,
        });
    };

    let signature_bytes = hex_to_bytes(signature_hex)
        .map_err(|e| SignatureFailure::Malformed(format!("signature is not hex: {e}")))?;
    let signature_len = signature_bytes.len();
    let signature_array: [u8; 64] = signature_bytes.try_into().map_err(|_| {
        SignatureFailure::Malformed(format!(
            "signature is {signature_len} bytes, expected 64"
        ))
    })?;

    let verifying_key = VerifyingKey::from_bytes(
        &hex_to_bytes(&trusted.public_key_hex)
            .map_err(|e| SignatureFailure::Malformed(format!("publicKey is not hex: {e}")))?
            .try_into()
            .map_err(|_| {
                SignatureFailure::Malformed(format!(
                    "publicKey for {key_id} is not 32 bytes"
                ))
            })?,
    )
    .map_err(|e| SignatureFailure::Malformed(format!("publicKey for {key_id} is invalid: {e}")))?;

    verifying_key
        .verify(manifest_bytes, &Signature::from_bytes(&signature_array))
        .map_err(|_| SignatureFailure::BadSignature {
            key_id: key_id.to_string(),
        })?;

    Ok(key_id.to_string())
}

fn trusted_source(keyring: &BTreeMap<String, TrustedKey>, id: &str) -> &'static str {
    keyring
        .get(id)
        .map(|k| k.source.label())
        .unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    use ed25519_dalek::{Signer, SigningKey};

    fn keyring_with(key_id: &str, public: &str) -> BTreeMap<String, TrustedKey> {
        let mut map = BTreeMap::new();
        map.insert(
            key_id.to_string(),
            TrustedKey {
                key_id: key_id.to_string(),
                public_key_hex: public.to_string(),
                source: KeySource::Config,
            },
        );
        map
    }

    fn signed_document(signing: &SigningKey, key_id: &str, payload: &[u8]) -> String {
        let signature = signing.sign(payload);
        format!(
            r#"{{"algorithm":"ed25519","keyId":"{key_id}","publicKey":"{}","signature":"{}"}}"#,
            hex(&signing.verifying_key().to_bytes()),
            hex(&signature.to_bytes())
        )
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn dev_signing_key() -> SigningKey {
        SigningKey::from_bytes(&[0x42; 32])
    }

    #[test]
    fn a_signature_from_the_keyring_verifies() {
        let signing = dev_signing_key();
        let public = hex(&signing.verifying_key().to_bytes());
        let keyring = keyring_with("release-2026", &public);
        let manifest = b"{\"name\":\"terrakit-data-tr\"}";

        let document = signed_document(&signing, "release-2026", manifest);
        assert_eq!(
            verify(&document, manifest, &keyring).unwrap(),
            "release-2026"
        );
    }

    #[test]
    fn a_single_changed_byte_breaks_verification() {
        let signing = dev_signing_key();
        let public = hex(&signing.verifying_key().to_bytes());
        let keyring = keyring_with("release-2026", &public);
        let manifest = b"{\"name\":\"terrakit-data-tr\"}";
        let document = signed_document(&signing, "release-2026", manifest);

        // The highest-value single-byte mutation: someone edits the version field, or swaps a
        // checksum, and leaves the signature in place.
        let mut tampered = manifest.to_vec();
        let last = tampered.len() - 2;
        tampered[last] = b' ';
        assert_eq!(
            verify(&document, &tampered, &keyring),
            Err(SignatureFailure::BadSignature {
                key_id: "release-2026".into()
            })
        );
    }

    #[test]
    fn a_different_key_with_the_same_key_id_is_rejected() {
        // The attack the "publicKey is carried in the document" convenience invites: reuse a
        // trusted key id with an attacker's own key. The keyring decides, so this fails.
        let real = dev_signing_key();
        let attacker = SigningKey::from_bytes(&[0x11; 32]);
        let keyring = keyring_with("release-2026", &hex(&real.verifying_key().to_bytes()));
        let manifest = b"payload";

        let document = signed_document(&attacker, "release-2026", manifest);
        assert!(matches!(
            verify(&document, manifest, &keyring),
            Err(SignatureFailure::BadSignature { .. })
        ));
    }

    #[test]
    fn an_unknown_key_id_names_the_keys_that_were_consulted() {
        let signing = dev_signing_key();
        let keyring = keyring_with("known-key", &hex(&signing.verifying_key().to_bytes()));
        let manifest = b"payload";
        let document = signed_document(&signing, "stranger", manifest);

        match verify(&document, manifest, &keyring) {
            Err(SignatureFailure::UnknownKey { key_id, consulted }) => {
                assert_eq!(key_id, "stranger");
                assert!(
                    consulted.contains("known-key"),
                    "the message must say which keys were checked, got {consulted:?}"
                );
            }
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    #[test]
    fn a_self_supplied_public_key_does_not_help() {
        // A package that carries its own key and names itself in a keyring-less client would
        // verify against itself. With an empty keyring it must not.
        let signing = dev_signing_key();
        let manifest = b"payload";
        let document = signed_document(&signing, "self-signed", manifest);
        assert!(matches!(
            verify(&document, manifest, &BTreeMap::new()),
            Err(SignatureFailure::UnknownKey { .. })
        ));
    }

    #[test]
    fn a_missing_signature_is_not_the_same_as_a_malformed_one() {
        // Distinct outcomes with distinct advice: "never signed" versus "damaged in transit".
        assert_eq!(verify("", b"x", &BTreeMap::new()).unwrap_err(), SignatureFailure::Missing);
        assert!(
            matches!(
                verify("{not json", b"x", &BTreeMap::new()),
                Err(SignatureFailure::Malformed(_))
            ),
            "unparseable JSON is malformed, not a verification failure"
        );
        assert_eq!(
            verify(r#"{"algorithm":"ed25519"}"#, b"x", &BTreeMap::new()).unwrap_err(),
            SignatureFailure::Malformed("no `keyId` field".into())
        );
    }

    #[test]
    fn a_signature_of_the_wrong_length_is_malformed_not_invalid() {
        // Two bytes cannot be an Ed25519 signature. Passing them to the verifier would
        // report "does not verify", which reads as tampering rather than as a truncated
        // document, and points at the publisher instead of at the transport.
        let document = r#"{"algorithm":"ed25519","keyId":"k","signature":"aabb"}"#;
        assert!(matches!(
            verify(document, b"x", &keyring_with("k", &"aa".repeat(32))),
            Err(SignatureFailure::Malformed(detail)) if detail.contains("64")
        ));
    }

    #[test]
    fn unsupported_algorithms_are_refused_rather_than_skipped() {
        // Silently ignoring an algorithm the client does not know would downgrade verification
        // to nothing while reporting success.
        let document = r#"{"algorithm":"rsa-sha512","keyId":"k","signature":"00"}"#;
        assert_eq!(
            verify(document, b"x", &BTreeMap::new()).unwrap_err(),
            SignatureFailure::UnsupportedAlgorithm("rsa-sha512".into())
        );
    }

    #[test]
    fn config_parses_the_documented_subset() {
        let raw = r#"
# a comment
registry = "https://registry.terrakit.dev"
cache_dir = "~/.cache/terrakit"

[[keys]]
id = "release-2026"
publicKey = "3d4010f7e1a0f4a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a"

[[keys]]
id = "mirror-2026"
publicKey = "aa4010f7e1a0f4a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f70819ab"
"#;
        let config = parse_config(raw, Path::new("config.toml")).unwrap();
        assert_eq!(config.registry.as_deref(), Some("https://registry.terrakit.dev"));
        assert_eq!(config.keys.len(), 2);
        assert_eq!(config.keys[0].key_id, "release-2026");
        assert_eq!(config.keys[1].source, KeySource::Config);
    }

    #[test]
    fn a_malformed_config_is_an_error_not_a_silent_default() {
        // The failure this prevents: a config the client cannot read makes it fall back to the
        // built-in key alone, and the user believes their release key is trusted.
        for (raw, why) in [
            ("[[key]]\nid = \"x\"\n", "a near-miss table name"),
            ("[[keys]]\nid = \"x\"\n", "a block missing publicKey"),
            (
                "[[keys]]\nid = \"x\"\npublicKey = \"abcd\"\n",
                "a publicKey of the wrong length",
            ),
            ("[[keys]]\nid = \"x\"\npublicKey = \"3d4010f7e1a0f4a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a\"\nextra = 1\n", "an unknown field"),
            ("[registry]\nurl = \"x\"\n", "an unsupported table"),
        ] {
            let result = parse_config(raw, Path::new("config.toml"));
            assert!(result.is_err(), "{why} must be rejected, got {result:?}");
        }
    }

    #[test]
    fn a_config_key_shadows_a_builtin_of_the_same_id() {
        // Otherwise there is no way to replace a compromised development key short of a new
        // binary release, which is precisely what a keyring is for.
        let mut config = Config::default();
        config.keys.push(TrustedKey {
            key_id: DEV_KEY_ID.to_string(),
            public_key_hex: "aa".repeat(32),
            source: KeySource::Config,
        });
        let ring = config.keyring();
        let entry = ring.get(DEV_KEY_ID).unwrap();
        assert_eq!(entry.source, KeySource::Config);
        assert_eq!(entry.public_key_hex, "aa".repeat(32));
    }

    #[test]
    fn the_builtin_keyring_is_never_empty() {
        let ring = Config::default().keyring();
        assert!(ring.contains_key(DEV_KEY_ID));
        assert_eq!(ring[DEV_KEY_ID].source, KeySource::BuiltIn);
        assert_eq!(ring[DEV_KEY_ID].public_key_hex.len(), 64);
    }

    #[test]
    fn the_committed_dev_public_key_matches_its_private_half() {
        // `keys/dev-ed25519.pub` and this constant are edited in two places. If they drift, the
        // CLI trusts nothing and the failure surfaces as "unknown key" for every package,
        // which reads as a registry problem rather than a stale constant.
        let committed = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../keys/dev-ed25519.pub"
        ))
        .expect("keys/dev-ed25519.pub should be committed");
        let value: serde_json::Value =
            serde_json::from_str(&committed).expect("dev public key should be JSON");
        assert_eq!(
            value["publicKey"].as_str().unwrap(),
            DEV_PUBLIC_KEY,
            "keys/dev-ed25519.pub and DEV_PUBLIC_KEY have drifted apart"
        );
        assert_eq!(
            value["keyId"].as_str().unwrap(),
            DEV_KEY_ID,
            "the key id in keys/dev-ed25519.pub and DEV_KEY_ID have drifted apart"
        );
    }
}