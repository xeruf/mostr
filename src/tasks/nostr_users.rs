use std::collections::HashMap;
use std::str::FromStr;
use nostr_sdk::{Metadata, PublicKey};

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct NostrUsers {
    users: HashMap<PublicKey, Metadata>
}

impl NostrUsers {
    pub(crate) fn find_user_with_displayname(&self, term: &str) -> Option<(PublicKey, String)> {
        self.find_user(term)
            .map(|(k, _)| (*k, self.get_displayname(k)))
    }

    // Find username or key starting with the given term.
    pub(crate) fn find_user(&self, term: &str) -> Option<(&PublicKey, &Metadata)> {
        if let Ok(key) = PublicKey::from_str(term) {
            return self.users.get_key_value(&key);
        }
        self.users.iter().find(|(k, v)|
            // TODO regex word boundary
            v.name.as_ref().is_some_and(|n| n.starts_with(term)) ||
                v.display_name.as_ref().is_some_and(|n| n.starts_with(term)) ||
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

    pub(super) fn insert(&mut self, pubkey: PublicKey, metadata: Metadata) {
        self.users.insert(pubkey, metadata);
    }

    pub(super) fn create(&mut self, pubkey: PublicKey) {
        if !self.users.contains_key(&pubkey) {
            self.users.insert(pubkey, Default::default());
        }
    }
}