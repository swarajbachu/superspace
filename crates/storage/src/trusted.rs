use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 1;
const NOISE_KEY_BYTES: usize = 32;
const MAX_CERTIFICATE_BYTES: usize = 16 * 1024;

/// One device approved through verified Noise pairing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedDevice {
    /// Stable installation identifier.
    pub id: Uuid,
    /// User-visible peer name.
    pub name: String,
    /// Pinned X25519 Noise public identity.
    pub noise_public_key: [u8; NOISE_KEY_BYTES],
    /// Pinned self-signed TLS certificate used for mutual QUIC authentication.
    pub certificate_der: Vec<u8>,
    /// Unix milliseconds when the current identity was paired.
    pub paired_at: i64,
    /// Unix milliseconds when authenticated traffic was last observed.
    pub last_seen_at: Option<i64>,
    /// Disabled peers remain listed but cannot connect or receive events.
    pub enabled: bool,
}

impl TrustedDevice {
    fn validate(&self) -> Result<(), TrustStoreError> {
        if self.name.trim().is_empty()
            || self.certificate_der.is_empty()
            || self.certificate_der.len() > MAX_CERTIFICATE_BYTES
        {
            return Err(TrustStoreError::InvalidDevice);
        }
        Ok(())
    }
}

/// Dedicated `SQLite` store for paired identities and revocation state.
pub struct TrustedDeviceStore {
    connection: Connection,
}

impl TrustedDeviceStore {
    /// Open or create a trust database and apply numbered migrations.
    ///
    /// # Errors
    ///
    /// Returns [`TrustStoreError`] when `SQLite` cannot open or migrate the database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrustStoreError> {
        let connection = Connection::open(path)?;
        Self::configure(&connection)?;
        Self::migrate(&connection)?;
        Ok(Self { connection })
    }

    /// Create an ephemeral trust database for tests.
    ///
    /// # Errors
    ///
    /// Returns [`TrustStoreError`] when `SQLite` cannot initialize the schema.
    pub fn memory() -> Result<Self, TrustStoreError> {
        let connection = Connection::open_in_memory()?;
        Self::configure(&connection)?;
        Self::migrate(&connection)?;
        Ok(Self { connection })
    }

    /// Insert or replace an explicitly re-paired device identity.
    ///
    /// # Errors
    ///
    /// Returns [`TrustStoreError::InvalidDevice`] for malformed records or a database failure.
    pub fn upsert(&self, device: &TrustedDevice) -> Result<(), TrustStoreError> {
        device.validate()?;
        self.connection.execute(
            "INSERT INTO trusted_devices
             (id, name, noise_public_key, certificate_der, paired_at, last_seen_at, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                noise_public_key = excluded.noise_public_key,
                certificate_der = excluded.certificate_der,
                paired_at = excluded.paired_at,
                last_seen_at = excluded.last_seen_at,
                enabled = excluded.enabled",
            params![
                device.id.to_string(),
                device.name,
                device.noise_public_key.as_slice(),
                device.certificate_der,
                device.paired_at,
                device.last_seen_at,
                device.enabled,
            ],
        )?;
        Ok(())
    }

    /// Find one trusted device by stable identifier.
    ///
    /// # Errors
    ///
    /// Returns a database or persisted-record validation failure.
    pub fn get(&self, id: Uuid) -> Result<Option<TrustedDevice>, TrustStoreError> {
        self.connection
            .query_row(
                "SELECT id, name, noise_public_key, certificate_der, paired_at, last_seen_at, enabled
                 FROM trusted_devices WHERE id = ?1",
                [id.to_string()],
                Self::row,
            )
            .optional()
            .map_err(TrustStoreError::from)
    }

    /// List devices with enabled peers first and most recently seen within each section.
    ///
    /// # Errors
    ///
    /// Returns a database or persisted-record validation failure.
    pub fn list(&self) -> Result<Vec<TrustedDevice>, TrustStoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, noise_public_key, certificate_der, paired_at, last_seen_at, enabled
             FROM trusted_devices
             ORDER BY enabled DESC, last_seen_at IS NULL, last_seen_at DESC, name COLLATE NOCASE",
        )?;
        statement
            .query_map([], Self::row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(TrustStoreError::from)
    }

    /// Enable or revoke a paired device without deleting its display record.
    ///
    /// # Errors
    ///
    /// Returns a database failure.
    pub fn set_enabled(&self, id: Uuid, enabled: bool) -> Result<bool, TrustStoreError> {
        Ok(self.connection.execute(
            "UPDATE trusted_devices SET enabled = ?2 WHERE id = ?1",
            params![id.to_string(), enabled],
        )? == 1)
    }

    /// Record successfully authenticated peer activity.
    ///
    /// # Errors
    ///
    /// Returns a database failure.
    pub fn touch(&self, id: Uuid, seen_at: i64) -> Result<bool, TrustStoreError> {
        Ok(self.connection.execute(
            "UPDATE trusted_devices SET last_seen_at = max(coalesce(last_seen_at, ?2), ?2)
             WHERE id = ?1 AND enabled = 1",
            params![id.to_string(), seen_at],
        )? == 1)
    }

    /// Permanently forget a peer and its pinned certificate.
    ///
    /// # Errors
    ///
    /// Returns a database failure.
    pub fn remove(&self, id: Uuid) -> Result<bool, TrustStoreError> {
        Ok(self.connection.execute(
            "DELETE FROM trusted_devices WHERE id = ?1",
            [id.to_string()],
        )? == 1)
    }

    fn configure(connection: &Connection) -> Result<(), rusqlite::Error> {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "busy_timeout", 5_000)?;
        Ok(())
    }

    fn migrate(connection: &Connection) -> Result<(), rusqlite::Error> {
        let current: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if current < SCHEMA_VERSION {
            connection.execute_batch(
                "BEGIN;
                 CREATE TABLE trusted_devices (
                    id TEXT PRIMARY KEY NOT NULL,
                    name TEXT NOT NULL,
                    noise_public_key BLOB NOT NULL CHECK(length(noise_public_key) = 32),
                    certificate_der BLOB NOT NULL
                        CHECK(length(certificate_der) BETWEEN 1 AND 16384),
                    paired_at INTEGER NOT NULL,
                    last_seen_at INTEGER,
                    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1))
                 );
                 PRAGMA user_version = 1;
                 COMMIT;",
            )?;
        }
        Ok(())
    }

    fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrustedDevice> {
        let id: String = row.get(0)?;
        let noise_key: Vec<u8> = row.get(2)?;
        let noise_public_key = noise_key.try_into().map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Blob,
                Box::new(TrustStoreError::InvalidDevice),
            )
        })?;
        let device = TrustedDevice {
            id: Uuid::parse_str(&id).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
            name: row.get(1)?,
            noise_public_key,
            certificate_der: row.get(3)?,
            paired_at: row.get(4)?,
            last_seen_at: row.get(5)?,
            enabled: row.get(6)?,
        };
        device.validate().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Blob,
                Box::new(error),
            )
        })?;
        Ok(device)
    }
}

/// Trusted-device validation and persistence failures.
#[derive(Debug, Error)]
pub enum TrustStoreError {
    /// `SQLite` operation failed.
    #[error("trusted-device database operation failed")]
    Database(#[from] rusqlite::Error),
    /// A supplied or persisted device record is malformed.
    #[error("trusted-device identity is invalid")]
    InvalidDevice,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: Uuid, name: &str) -> TrustedDevice {
        TrustedDevice {
            id,
            name: name.into(),
            noise_public_key: [7; 32],
            certificate_der: vec![1, 2, 3],
            paired_at: 10,
            last_seen_at: None,
            enabled: true,
        }
    }

    #[test]
    fn lifecycle_supports_revoke_touch_and_forget() {
        let store = TrustedDeviceStore::memory().expect("store");
        let id = Uuid::new_v4();
        store.upsert(&device(id, "MacBook")).expect("pair");
        assert_eq!(store.get(id).expect("get").expect("device").name, "MacBook");
        assert!(store.touch(id, 20).expect("touch"));
        assert_eq!(
            store.get(id).expect("get").expect("device").last_seen_at,
            Some(20)
        );
        assert!(store.set_enabled(id, false).expect("revoke"));
        assert!(!store.touch(id, 30).expect("disabled touch"));
        assert!(!store.get(id).expect("get").expect("device").enabled);
        assert!(store.remove(id).expect("forget"));
        assert!(store.get(id).expect("get").is_none());
    }

    #[test]
    fn repair_replaces_pinned_identity_and_list_orders_enabled_first() {
        let store = TrustedDeviceStore::memory().expect("store");
        let first_id = Uuid::from_u128(1);
        let second_id = Uuid::from_u128(2);
        let mut first = device(first_id, "First");
        first.enabled = false;
        store.upsert(&first).expect("first");
        store.upsert(&device(second_id, "Second")).expect("second");
        let mut repaired = device(second_id, "Second renamed");
        repaired.noise_public_key = [9; 32];
        repaired.certificate_der = vec![4, 5, 6];
        store.upsert(&repaired).expect("repair");
        let listed = store.list().expect("list");
        assert_eq!(listed[0], repaired);
        assert_eq!(listed[1], first);
    }

    #[test]
    fn malformed_records_are_rejected_before_sql() {
        let store = TrustedDeviceStore::memory().expect("store");
        let mut invalid = device(Uuid::new_v4(), " ");
        assert!(matches!(
            store.upsert(&invalid),
            Err(TrustStoreError::InvalidDevice)
        ));
        invalid.name = "Valid".into();
        invalid.certificate_der.clear();
        assert!(store.upsert(&invalid).is_err());
    }
}
