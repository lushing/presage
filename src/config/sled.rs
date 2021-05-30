use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use libsignal_service::{
    models::Contact,
    prelude::protocol::{
        Context, Direction, IdentityKey, IdentityKeyPair, IdentityKeyStore, PreKeyRecord,
        PreKeyStore, ProtocolAddress, SessionRecord, SessionStore, SessionStoreExt,
        SignalProtocolError, SignedPreKeyRecord, SignedPreKeyStore,
    },
};
use log::{trace, warn};
use sled::IVec;

use super::{ConfigStore, ContactsStore};
use crate::{manager::State, Error};

const SLED_KEY_STATE: &str = "state";
const SLED_KEY_CONTACTS: &str = "contacts";

const SLED_TREE_SESSIONS: &str = "sessions";

#[derive(Debug, Clone)]
pub struct SledConfigStore {
    db: Arc<RwLock<sled::Db>>,
}

impl SledConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, Error> {
        Ok(SledConfigStore {
            db: Arc::new(RwLock::new(sled::open(path.into())?)),
        })
    }

    #[cfg(test)]
    fn temporary() -> Result<Self, Error> {
        let db = sled::Config::new().temporary(true).open()?;
        Ok(Self {
            db: Arc::new(RwLock::new(db)),
        })
    }

    pub fn get<K>(&self, key: K) -> Result<Option<IVec>, Error>
    where
        K: AsRef<str>,
    {
        trace!("get {}", key.as_ref());
        Ok(self.db.read().expect("poisoned mutex").get(key.as_ref())?)
    }

    fn get_u32<S>(&self, key: S) -> Result<Option<u32>, Error>
    where
        S: AsRef<str>,
    {
        trace!("getting u32 {}", key.as_ref());
        Ok(self.get(key.as_ref())?.map(|data| {
            let mut a: [u8; 4] = Default::default();
            a.copy_from_slice(&data);
            u32::from_le_bytes(a)
        }))
    }

    fn insert<K, V>(&self, key: K, value: V) -> Result<(), Error>
    where
        K: AsRef<str>,
        IVec: From<V>,
    {
        trace!("inserting {}", key.as_ref());
        let _ = self
            .db
            .try_write()
            .expect("poisoned mutex")
            .insert(key.as_ref(), value)?;
        Ok(())
    }

    fn insert_u32<S>(&self, key: S, value: u32) -> Result<(), Error>
    where
        S: AsRef<str>,
    {
        trace!("inserting u32 {}", key.as_ref());
        self.db
            .try_write()
            .expect("poisoned mutex")
            .insert(key.as_ref(), &value.to_le_bytes())?;
        Ok(())
    }

    fn remove<S>(&self, key: S) -> Result<(), Error>
    where
        S: AsRef<str>,
    {
        trace!("removing {} from db", key.as_ref());
        self.db
            .try_write()
            .expect("poisoned mutex")
            .remove(key.as_ref())?;
        Ok(())
    }

    fn prekey_key(&self, id: u32) -> String {
        format!("prekey-{:09}", id)
    }

    fn signed_prekey_key(&self, id: u32) -> String {
        format!("signed-prekey-{:09}", id)
    }

    fn session_key(&self, addr: &ProtocolAddress) -> String {
        format!("session-{}", addr)
    }

    fn session_prefix(&self, name: &str) -> String {
        format!("session-{}.", name)
    }

    fn identity_key(&self, addr: &ProtocolAddress) -> String {
        format!("identity-remote-{}", addr)
    }

    pub fn keys(&self) -> Result<(Vec<String>, Vec<String>), SignalProtocolError> {
        let db = self.db.read().expect("poisoned mutex");
        let global_keys = db
            .iter()
            .filter_map(|r| {
                let (k, _) = r.ok()?;
                Some(String::from_utf8_lossy(&k).to_string())
            })
            .collect();
        let session_keys = db
            .open_tree(SLED_TREE_SESSIONS)
            .map_err(|e| {
                log::error!("failed to open sessions tree: {}", e);
                SignalProtocolError::InternalError("sled error")
            })?
            .iter()
            .filter_map(|r| {
                let (k, _) = r.ok()?;
                Some(String::from_utf8_lossy(&k).to_string())
            })
            .collect();
        Ok((global_keys, session_keys))
    }
}

impl ConfigStore for SledConfigStore {
    fn state(&self) -> Result<State, Error> {
        let db = self.db.read().expect("poisoned mutex");
        db.get(SLED_KEY_STATE)?.map_or(Ok(State::New), |s| {
            serde_json::from_slice(&s).map_err(Error::from)
        })
    }

    fn save(&self, state: &State) -> Result<(), Error> {
        let db = self.db.try_write().expect("poisoned mutex");
        db.clear()?;
        db.insert(SLED_KEY_STATE, serde_json::to_vec(state)?)?;
        Ok(())
    }

    fn pre_keys_offset_id(&self) -> Result<u32, Error> {
        Ok(self.get_u32("pre_keys_offset_id")?.unwrap_or(0))
    }

    fn set_pre_keys_offset_id(&self, id: u32) -> Result<(), Error> {
        self.insert_u32("pre_keys_offset_id", id)
    }

    fn next_signed_pre_key_id(&self) -> Result<u32, Error> {
        Ok(self.get_u32("next_signed_pre_key_id")?.unwrap_or(0))
    }

    fn set_next_signed_pre_key_id(&self, id: u32) -> Result<(), Error> {
        self.insert_u32("next_signed_pre_key_id", id)
    }
}

impl ContactsStore for SledConfigStore {
    fn save_contacts(&mut self, contacts: &[Contact]) -> Result<(), Error> {
        self.db
            .write()
            .expect("poisoned mutex")
            .insert(SLED_KEY_CONTACTS, serde_json::to_vec(contacts)?)?;
        trace!("saved contacts");
        Ok(())
    }

    fn contacts(&self) -> Result<Vec<Contact>, Error> {
        self.db
            .read()
            .expect("poisoned mutex")
            .get(SLED_KEY_CONTACTS)?
            .map_or_else(|| Ok(vec![]), |buf| Ok(serde_json::from_slice(&buf)?))
    }
}

#[async_trait(?Send)]
impl PreKeyStore for SledConfigStore {
    async fn get_pre_key(
        &self,
        prekey_id: u32,
        _ctx: Option<Context>,
    ) -> Result<PreKeyRecord, SignalProtocolError> {
        let buf = self
            .get(self.prekey_key(prekey_id))
            .map_err(|e| {
                log::error!("{}", e);
                SignalProtocolError::InternalError("sled error")
            })?
            .ok_or(SignalProtocolError::InvalidPreKeyId)?;
        PreKeyRecord::deserialize(&buf)
    }

    async fn save_pre_key(
        &mut self,
        prekey_id: u32,
        record: &PreKeyRecord,
        _ctx: Option<Context>,
    ) -> Result<(), SignalProtocolError> {
        self.insert(self.prekey_key(prekey_id), record.serialize()?)
            .expect("failed to store pre-key");
        Ok(())
    }

    async fn remove_pre_key(
        &mut self,
        prekey_id: u32,
        _ctx: Option<Context>,
    ) -> Result<(), SignalProtocolError> {
        self.remove(self.prekey_key(prekey_id))
            .expect("failed to remove pre-key");
        Ok(())
    }
}

#[async_trait(?Send)]
impl SignedPreKeyStore for SledConfigStore {
    async fn get_signed_pre_key(
        &self,
        signed_prekey_id: u32,
        _ctx: Option<Context>,
    ) -> Result<SignedPreKeyRecord, SignalProtocolError> {
        let buf = self
            .get(self.signed_prekey_key(signed_prekey_id))
            .map_err(|e| {
                log::error!("sled error: {}", e);
                SignalProtocolError::InternalError("sled error")
            })?
            .ok_or(SignalProtocolError::InvalidSignedPreKeyId)?;
        SignedPreKeyRecord::deserialize(&buf)
    }

    async fn save_signed_pre_key(
        &mut self,
        signed_prekey_id: u32,
        record: &SignedPreKeyRecord,
        _ctx: Option<Context>,
    ) -> Result<(), SignalProtocolError> {
        self.insert(
            self.signed_prekey_key(signed_prekey_id),
            record.serialize()?,
        )
        .map_err(|e| {
            log::error!("sled error: {}", e);
            SignalProtocolError::InternalError("sled error")
        })
    }
}

#[async_trait(?Send)]
impl SessionStore for SledConfigStore {
    async fn load_session(
        &self,
        address: &ProtocolAddress,
        _ctx: Option<Context>,
    ) -> Result<Option<SessionRecord>, SignalProtocolError> {
        let key = self.session_key(&address);
        trace!("loading session from {}", key);

        let buf = self
            .db
            .try_read()
            .expect("poisoned mutex")
            .open_tree(SLED_TREE_SESSIONS)
            .map_err(|e| {
                log::error!("failed to open sessions tree: {}", e);
                SignalProtocolError::InternalError("sled error")
            })?
            .get(key)
            .map_err(|e| {
                log::error!("sled error: {}", e);
                SignalProtocolError::InternalError("sled error")
            })?;

        buf.map(|buf| SessionRecord::deserialize(&buf)).transpose()
    }

    async fn store_session(
        &mut self,
        address: &ProtocolAddress,
        record: &SessionRecord,
        _ctx: Option<Context>,
    ) -> Result<(), SignalProtocolError> {
        let key = self.session_key(&address);
        trace!("storing session for {:?} at {:?}", address, key);
        self.db
            .try_write()
            .expect("poisoned mutex")
            .open_tree(SLED_TREE_SESSIONS)
            .map_err(|e| {
                log::error!("failed to open sessions tree: {}", e);
                SignalProtocolError::InternalError("sled error")
            })?
            .insert(key, record.serialize()?)
            .map_err(|e| {
                log::error!("failed to open sessions tree: {}", e);
                SignalProtocolError::InternalError("sled error")
            })?;
        Ok(())
    }
}

#[async_trait]
impl SessionStoreExt for SledConfigStore {
    async fn get_sub_device_sessions(&self, name: &str) -> Result<Vec<u32>, SignalProtocolError> {
        let session_prefix = self.session_prefix(name);
        let session_ids: Vec<u32> = self
            .db
            .read()
            .expect("poisoned mutex")
            .open_tree(SLED_TREE_SESSIONS)
            .map_err(|e| {
                log::error!("failed to open sessions tree: {}", e);
                SignalProtocolError::InternalError("sled error")
            })?
            .scan_prefix(&session_prefix)
            .filter_map(|r| {
                let (key, _) = r.ok()?;
                let key_str = String::from_utf8_lossy(&key);
                let device_id = key_str.strip_prefix(&session_prefix)?;
                device_id.parse().ok()
            })
            .collect();
        Ok(session_ids)
    }

    async fn delete_session(&self, address: &ProtocolAddress) -> Result<(), SignalProtocolError> {
        let key = self.session_key(&address);
        trace!("deleting session with key: {}", key);
        self.db
            .try_write()
            .expect("poisoned mutex")
            .open_tree(SLED_TREE_SESSIONS)
            .map_err(|e| {
                log::error!("failed to open sessions tree: {}", e);
                SignalProtocolError::InternalError("sled error")
            })?
            .remove(key)
            .map_err(|_e| SignalProtocolError::InternalError("failed to delete session"))?;
        Ok(())
    }

    async fn delete_all_sessions(&self, _name: &str) -> Result<usize, SignalProtocolError> {
        let tree = self
            .db
            .try_write()
            .expect("poisoned mutex")
            .open_tree(SLED_TREE_SESSIONS)
            .map_err(|e| {
                log::error!("failed to open sessions tree: {}", e);
                SignalProtocolError::InternalError("sled error")
            })?;
        let len = tree.len();
        tree.clear()
            .map_err(|_e| SignalProtocolError::InternalError("failed to delete all sessions"))?;
        Ok(len)
    }
}

#[async_trait(?Send)]
impl IdentityKeyStore for SledConfigStore {
    async fn get_identity_key_pair(
        &self,
        _ctx: Option<Context>,
    ) -> Result<IdentityKeyPair, SignalProtocolError> {
        trace!("getting identity_key_pair");
        match self.state() {
            Ok(State::Registered {
                private_key,
                public_key,
                ..
            }) => Ok(IdentityKeyPair::new(
                IdentityKey::new(public_key),
                private_key,
            )),
            Ok(_) => Err(SignalProtocolError::InternalError(
                "wrong state: no registration data yet",
            )),
            Err(e) => {
                log::error!("identity key store error: {}", e);
                Err(SignalProtocolError::InternalError("unhandled error"))
            }
        }
    }

    async fn get_local_registration_id(&self, _ctx: Option<Context>) -> Result<u32, SignalProtocolError> {
        trace!("getting local_registration_id");
        match self.state() {
            Ok(State::Registered {
                registration_id, ..
            }) => Ok(registration_id),
            Ok(_) => Err(SignalProtocolError::InternalError(
                "wrong state: no registration data yet",
            )),
            Err(e) => {
                log::error!("identity key store error: {}", e);
                Err(SignalProtocolError::InternalError("unhandled error"))
            }
        }
    }

    async fn save_identity(
        &mut self,
        address: &ProtocolAddress,
        identity_key: &IdentityKey,
        _ctx: Option<Context>,
    ) -> Result<bool, SignalProtocolError> {
        trace!("saving identity");
        self.insert(self.identity_key(&address), identity_key.serialize())
            .map_err(|e| {
                log::error!("error saving identity for {:?}: {}", address, e);
                SignalProtocolError::InternalError("failed to save identity")
            })?;
        trace!("saved identity");
        Ok(false)
    }

    async fn is_trusted_identity(
        &self,
        address: &ProtocolAddress,
        identity_key: &IdentityKey,
        _direction: Direction,
        _ctx: Option<Context>,
    ) -> Result<bool, SignalProtocolError> {
        match self.get(self.identity_key(&address)).map_err(|_| {
            SignalProtocolError::InternalError("failed to check if identity is trusted")
        })? {
            None => {
                // when we encounter a new identity, we trust it by default
                warn!("trusting new identity {:?}", address);
                Ok(true)
            }
            Some(contents) => Ok(&IdentityKey::decode(&contents)? == identity_key),
        }
    }

    async fn get_identity(
        &self,
        address: &ProtocolAddress,
        _ctx: Option<Context>,
    ) -> Result<Option<IdentityKey>, SignalProtocolError> {
        let buf = self.get(self.identity_key(&address)).map_err(|e| {
            log::error!("error getting identity of {:?}: {}", address, e);
            SignalProtocolError::InternalError("failed to read identity")
        })?;
        Ok(buf.map(|ref b| IdentityKey::decode(b).unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use core::fmt;

    use libsignal_service::prelude::protocol::{
        self, Direction, IdentityKeyStore, PreKeyRecord, PreKeyStore, SessionRecord, SessionStore,
        SignedPreKeyRecord, SignedPreKeyStore,
    };
    use quickcheck::{Arbitrary, Gen};

    use super::SledConfigStore;

    #[derive(Debug, Clone)]
    struct ProtocolAddress(protocol::ProtocolAddress);

    #[derive(Clone)]
    struct KeyPair(protocol::KeyPair);

    impl fmt::Debug for KeyPair {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            writeln!(f, "{}", base64::encode(self.0.public_key.serialize()))
        }
    }

    impl Arbitrary for ProtocolAddress {
        fn arbitrary(g: &mut Gen) -> Self {
            let name: String = Arbitrary::arbitrary(g);
            let device_id: u8 = Arbitrary::arbitrary(g);
            ProtocolAddress(protocol::ProtocolAddress::new(name, device_id.into()))
        }
    }

    impl Arbitrary for KeyPair {
        fn arbitrary(_g: &mut Gen) -> Self {
            // Gen is not rand::CryptoRng here, see https://github.com/BurntSushi/quickcheck/issues/241
            KeyPair(protocol::KeyPair::generate(&mut rand::thread_rng()))
        }
    }

    #[quickcheck_async::tokio]
    async fn test_save_get_trust_identity(addr: ProtocolAddress, key_pair: KeyPair) -> bool {
        let mut db = SledConfigStore::temporary().unwrap();
        let identity_key = protocol::IdentityKey::new(key_pair.0.public_key);
        db.save_identity(&addr.0, &identity_key, None)
            .await
            .unwrap();
        let id = db.get_identity(&addr.0, None).await.unwrap().unwrap();
        if id != identity_key {
            return false;
        }
        db.is_trusted_identity(&addr.0, &id, Direction::Receiving, None)
            .await
            .unwrap()
    }

    #[quickcheck_async::tokio]
    async fn test_store_load_session(addr: ProtocolAddress) -> bool {
        let session = SessionRecord::new_fresh();

        let mut db = SledConfigStore::temporary().unwrap();
        db.store_session(&addr.0, &session, None).await.unwrap();
        if db.load_session(&addr.0, None).await.unwrap().is_none() {
            return false;
        }
        let loaded_session = db.load_session(&addr.0, None).await.unwrap().unwrap();
        session.serialize().unwrap() == loaded_session.serialize().unwrap()
    }

    #[quickcheck_async::tokio]
    async fn test_prekey_store(id: u32, key_pair: KeyPair) -> bool {
        let mut db = SledConfigStore::temporary().unwrap();
        let pre_key_record = PreKeyRecord::new(id, &key_pair.0);
        db.save_pre_key(id, &pre_key_record, None).await.unwrap();
        if db.get_pre_key(id, None).await.unwrap().serialize().unwrap()
            != pre_key_record.serialize().unwrap()
        {
            return false;
        }

        db.remove_pre_key(id, None).await.unwrap();
        db.get_pre_key(id, None).await.is_err()
    }

    #[quickcheck_async::tokio]
    async fn test_signed_prekey_store(
        id: u32,
        timestamp: u64,
        key_pair: KeyPair,
        signature: Vec<u8>,
    ) -> bool {
        let mut db = SledConfigStore::temporary().unwrap();
        let signed_pre_key_record = SignedPreKeyRecord::new(id, timestamp, &key_pair.0, &signature);
        db.save_signed_pre_key(id, &signed_pre_key_record, None)
            .await
            .unwrap();

        db.get_signed_pre_key(id, None)
            .await
            .unwrap()
            .serialize()
            .unwrap()
            == signed_pre_key_record.serialize().unwrap()
    }
}
