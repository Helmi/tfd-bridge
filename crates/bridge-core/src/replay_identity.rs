//! Replay-file recorder identity evidence.
//!
//! The recorder account is deliberately read only from the replay header's
//! `ReplayMeta.playerID`. Results blobs, roster rows, names, filenames and
//! scene-selection state are not ownership evidence. Realm is a best-effort
//! narrowing claim only: failure to observe a usable realm never rejects a
//! valid header account.

use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The only normalized realm values the Engine contract accepts as an optional
/// narrowing claim.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RecorderRealm {
    Eu,
    Na,
    Asia,
    Cis,
}

impl RecorderRealm {
    fn normalize(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "eu" => Some(Self::Eu),
            "na" => Some(Self::Na),
            "asia" => Some(Self::Asia),
            "cis" => Some(Self::Cis),
            _ => None,
        }
    }
}

/// A usable recorder identity. `realm` is intentionally optional: its absence
/// is permitted by contract v5 and can never deny an otherwise valid account.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecorderIdentity {
    pub account_id: u64,
    pub realm: Option<RecorderRealm>,
}

/// The immutable local replay artifact that produced an evidence claim.
/// `source_sha256` hashes the exact bytes passed to the header reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayArtifact {
    pub path: PathBuf,
    pub source_sha256: [u8; 32],
}

impl ReplayArtifact {
    pub fn source_sha256_hex(&self) -> String {
        hex::encode(self.source_sha256)
    }
}

/// Recorder claim plus the exact local artifact it came from. A later share
/// layer must bind its arena ID separately and recheck this source hash before
/// rendering or uploading; the replay metadata header has no arena ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayEvidence {
    pub artifact: ReplayArtifact,
    pub recorder: RecorderIdentity,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayIdentityError {
    #[error("the replay has no usable recorder account identity")]
    Missing,
    #[error("the replay has ambiguous recorder account identity")]
    Ambiguous,
    #[error("could not read replay evidence: {0}")]
    Read(#[from] std::io::Error),
}

/// The production reader is a singleton because the toolkit's replay header
/// has exactly one `playerID`. The trait exists for deterministic defensive
/// tests and for a future toolkit migration, not as an alternate ownership
/// source.
pub trait RecorderAccountSource {
    fn recorder_account(&self, replay_bytes: &[u8]) -> RecorderAccountObservation;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderAccountObservation {
    Missing,
    Single(i64),
    Ambiguous,
}

struct ReplayMetaAccountSource;

impl RecorderAccountSource for ReplayMetaAccountSource {
    fn recorder_account(&self, replay_bytes: &[u8]) -> RecorderAccountObservation {
        match wows_replays::ReplayFile::meta_from_bytes(replay_bytes) {
            Ok(meta) => RecorderAccountObservation::Single(meta.playerID.0),
            Err(_) => RecorderAccountObservation::Missing,
        }
    }
}

/// One player-row realm observation from an optional packet decoder. The
/// provider must report decoded rows only; this module matches `db_id` to the
/// already-established header account and never uses a row to find the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerRealmObservation {
    pub db_id: u64,
    pub realm: Option<String>,
}

/// Optional integration seam for the existing battle-result-style decoder.
/// Its errors and missing/unknown rows are intentionally swallowed because a
/// realm claim is optional. The current pinned toolkit keeps decoded player
/// realm fields private, so no second parser or toolkit pin change is made
/// here; a later adapter can inject decoded rows through this seam.
pub trait RealmObservationProvider {
    fn player_realms(&self, replay_bytes: &[u8]) -> Result<Vec<PlayerRealmObservation>, ()>;
}

/// Read the file once, hash those exact bytes and establish its recorder claim
/// from `ReplayFile::meta_from_bytes` / `ReplayMeta.playerID` only.
pub fn read_replay_evidence(path: impl AsRef<Path>) -> Result<ReplayEvidence, ReplayIdentityError> {
    read_replay_evidence_with(path, &ReplayMetaAccountSource, None)
}

/// Same production operation with injected header/realm seams for deterministic
/// tests and the future optional decoder adapter. This function still reads the
/// file exactly once; both observers receive that one immutable byte slice.
pub fn read_replay_evidence_with(
    path: impl AsRef<Path>,
    account_source: &dyn RecorderAccountSource,
    realm_provider: Option<&dyn RealmObservationProvider>,
) -> Result<ReplayEvidence, ReplayIdentityError> {
    let path = path.as_ref();
    let replay_bytes = fs::read(path)?;
    let source_sha256: [u8; 32] = Sha256::digest(&replay_bytes).into();

    let raw_account_id = match account_source.recorder_account(&replay_bytes) {
        RecorderAccountObservation::Missing => return Err(ReplayIdentityError::Missing),
        RecorderAccountObservation::Ambiguous => return Err(ReplayIdentityError::Ambiguous),
        RecorderAccountObservation::Single(value) => value,
    };
    let account_id = u64::try_from(raw_account_id)
        .ok()
        .filter(|id| *id > 0)
        .ok_or(ReplayIdentityError::Missing)?;

    let realm = realm_provider
        .and_then(|provider| provider.player_realms(&replay_bytes).ok())
        .and_then(|rows| normalized_recorder_realm(account_id, rows));

    Ok(ReplayEvidence {
        artifact: ReplayArtifact {
            path: path.to_path_buf(),
            source_sha256,
        },
        recorder: RecorderIdentity { account_id, realm },
    })
}

fn normalized_recorder_realm(
    recorder_account_id: u64,
    rows: Vec<PlayerRealmObservation>,
) -> Option<RecorderRealm> {
    let realms: Option<BTreeSet<_>> = rows
        .into_iter()
        .filter(|row| row.db_id == recorder_account_id)
        .map(|row| row.realm.as_deref().and_then(RecorderRealm::normalize))
        .collect();
    let realms = realms?;
    if realms.len() == 1 {
        realms.into_iter().next()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn replay_bytes(player_id: i64) -> Vec<u8> {
        let metadata = serde_json::json!({
            "gameMode": 1,
            "clientVersionFromExe": "0,15,7,0",
            "mapDisplayName": "Test map",
            "mapId": 1,
            "clientVersionFromXml": "0,15,7,0",
            "duration": 1,
            "name": "not ownership evidence",
            "scenario": "test",
            "playerID": player_id,
            "vehicles": [],
            "playersPerTeam": 1,
            "dateTime": "2026-01-01T00:00:00",
            "mapName": "test",
            "playerName": "same display name is irrelevant",
            "scenarioConfigId": 1,
            "teamsCount": 2,
            "playerVehicle": "test",
        });
        let metadata = serde_json::to_vec(&metadata).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&metadata);
        bytes
    }

    fn replay_bytes_without_player_id() -> Vec<u8> {
        let bytes = replay_bytes(42);
        let mut metadata: serde_json::Value = serde_json::from_slice(&bytes[12..]).unwrap();
        metadata.as_object_mut().unwrap().remove("playerID");
        let metadata = serde_json::to_vec(&metadata).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&metadata);
        bytes
    }

    fn write_replay(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(bytes).unwrap();
        file
    }

    #[test]
    fn header_player_id_is_exact_even_above_javascript_safe_integer() {
        let file = write_replay(&replay_bytes(9_007_199_254_740_993));
        let evidence = read_replay_evidence(file.path()).unwrap();
        assert_eq!(evidence.recorder.account_id, 9_007_199_254_740_993);
        assert_eq!(evidence.recorder.realm, None);
    }

    #[test]
    fn invalid_or_corrupt_header_identity_is_missing() {
        for bytes in [
            replay_bytes(0),
            replay_bytes(-1),
            replay_bytes_without_player_id(),
            vec![],
            vec![1, 2, 3],
        ] {
            let file = write_replay(&bytes);
            assert!(matches!(
                read_replay_evidence(file.path()),
                Err(ReplayIdentityError::Missing)
            ));
        }
    }

    struct AmbiguousSource;
    impl RecorderAccountSource for AmbiguousSource {
        fn recorder_account(&self, _: &[u8]) -> RecorderAccountObservation {
            RecorderAccountObservation::Ambiguous
        }
    }

    #[test]
    fn injected_header_ambiguity_is_preserved_without_inventing_it_from_realms() {
        let file = write_replay(&replay_bytes(42));
        assert!(matches!(
            read_replay_evidence_with(file.path(), &AmbiguousSource, None),
            Err(ReplayIdentityError::Ambiguous)
        ));
    }

    struct Rows(Vec<PlayerRealmObservation>);
    impl RealmObservationProvider for Rows {
        fn player_realms(&self, _: &[u8]) -> Result<Vec<PlayerRealmObservation>, ()> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn realm_is_only_a_normalized_nonconflicting_claim_for_the_header_account() {
        let file = write_replay(&replay_bytes(42));
        let cases = [
            (
                vec![PlayerRealmObservation {
                    db_id: 42,
                    realm: Some(" EU ".into()),
                }],
                Some(RecorderRealm::Eu),
            ),
            (
                vec![PlayerRealmObservation {
                    db_id: 42,
                    realm: None,
                }],
                None,
            ),
            (
                vec![PlayerRealmObservation {
                    db_id: 42,
                    realm: Some("unknown".into()),
                }],
                None,
            ),
            (
                vec![
                    PlayerRealmObservation {
                        db_id: 42,
                        realm: Some("eu".into()),
                    },
                    PlayerRealmObservation {
                        db_id: 42,
                        realm: Some("na".into()),
                    },
                ],
                None,
            ),
        ];
        for (rows, expected) in cases {
            let evidence =
                read_replay_evidence_with(file.path(), &ReplayMetaAccountSource, Some(&Rows(rows)))
                    .unwrap();
            assert_eq!(evidence.recorder.realm, expected);
        }

        // A different player with the same display name is not a candidate.
        let evidence = read_replay_evidence_with(
            file.path(),
            &ReplayMetaAccountSource,
            Some(&Rows(vec![PlayerRealmObservation {
                db_id: 99,
                realm: Some("na".into()),
            }])),
        )
        .unwrap();
        assert_eq!(evidence.recorder.realm, None);
    }

    #[test]
    fn hash_is_of_the_exact_single_read_source_bytes() {
        let bytes = replay_bytes(42);
        let file = write_replay(&bytes);
        let evidence = read_replay_evidence(file.path()).unwrap();
        assert_eq!(
            evidence.artifact.source_sha256_hex(),
            "a53176d1bc732a64e24ead2aca935116e14e266edcdea227f50902c5bd08bd6d",
            "update this known digest only when the supported fixture bytes change"
        );
        let expected: [u8; 32] = Sha256::digest(&bytes).into();
        assert_eq!(evidence.artifact.source_sha256, expected);
    }
}
