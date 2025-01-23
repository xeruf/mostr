use itertools::Itertools;
use nostr_sdk::{Keys, Metadata, PublicKey, Tag, Timestamp};
use std::collections::HashMap;
use std::str::FromStr;
use log::debug;

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct NostrUsers {
    users: HashMap<PublicKey, Metadata>,
    user_times: HashMap<PublicKey, Timestamp>,
}

impl NostrUsers {
    pub(crate) fn find_user_with_displayname(&self, term: &str) -> Option<(PublicKey, String)> {
        self.find_user(term)
            .map(|(k, _)| (*k, self.get_displayname(k)))
    }

    // Find username or key starting with the given term.
    pub(crate) fn find_user(&self, term: &str) -> Option<(&PublicKey, &Metadata)> {
        let lowered = term.trim().to_ascii_lowercase();
        let term = lowered.as_str();
        if term.is_empty() {
            return None;
        }
        if let Ok(key) = PublicKey::from_str(term) {
            return self.users.get_key_value(&key);
        }
        self.users.iter()
            .sorted_unstable_by_key(|(k, v)| self.get_user_time(k))
            .rev()
            .find(|(k, v)|
                // TODO regex word boundary
                v.name.as_ref().is_some_and(|n| n.to_ascii_lowercase().starts_with(term)) ||
                    v.display_name.as_ref().is_some_and(|n| n.to_ascii_lowercase().starts_with(term)) ||
                    (term.len() > 4 && k.to_string().starts_with(term)))
    }

    pub(crate) fn get_displayname(&self, pubkey: &PublicKey) -> String {
        self.users.get(pubkey)
            .and_then(|m| m.display_name.clone().or(m.name.clone()))
            .unwrap_or_else(|| pubkey.to_string())
    }

    pub(crate) fn get_username(&self, pubkey: &PublicKey) -> String {
        self.users.get(pubkey)
            .and_then(|m| m.name.clone())
            .unwrap_or_else(|| format!("{:.6}", pubkey.to_string()))
    }

    fn get_user_time(&self, pubkey: &PublicKey) -> u64 {
        match self.user_times.get(pubkey) {
            Some(t) => t.as_u64(),
            None => Timestamp::zero().as_u64(),
        }
    }

    pub(super) fn insert(&mut self, pubkey: PublicKey, metadata: Metadata, timestamp: Timestamp) {
        if self.get_user_time(&pubkey) < timestamp.as_u64() {
            debug!("Inserting user metadata for {}", pubkey);
            self.users.insert(pubkey, metadata);
            self.user_times.insert(pubkey, timestamp);
        } else {
            debug!("Skipping older user metadata for {}", pubkey);
        }
    }

    pub(super) fn create(&mut self, pubkey: PublicKey) {
        if !self.users.contains_key(&pubkey) {
            self.users.insert(pubkey, Default::default());
        }
    }
}

#[test]
fn test_user_extract() {
    let keys = Keys::generate();
    let mut users = NostrUsers::default();
    users.insert(keys.public_key, Metadata::new().display_name("Tester Jo"), Timestamp::now());
    assert_eq!(crate::kinds::extract_tags("Hello @test", &users),
               ("Hello".to_string(), vec![Tag::public_key(keys.public_key)]));
}