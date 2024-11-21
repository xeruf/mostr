use std::cmp::Ordering;
use std::fmt::{Display, Formatter};
use std::ops::Deref;
use itertools::Itertools;
use nostr_sdk::{Alphabet, Tag};

pub fn is_hashtag(tag: &Tag) -> bool {
    tag.single_letter_tag()
        .is_some_and(|letter| letter.character == Alphabet::T)
}

#[derive(Clone, Debug)]
pub struct Hashtag(pub String);

impl Display for Hashtag {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Hashtag {
    pub fn content(&self) -> &str { &self.0 }
    pub fn matches(&self, token: &str) -> bool {
        self.0.contains(&token.to_ascii_lowercase())
    }
}

impl Eq for Hashtag {}
impl PartialEq<Self> for Hashtag {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_ascii_lowercase() == other.0.to_ascii_lowercase()
    }
}
impl Deref for Hashtag {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl TryFrom<&Tag> for Hashtag {
    type Error = String;

    fn try_from(value: &Tag) -> Result<Self, Self::Error> {
        value.content().take_if(|_| is_hashtag(value))
            .map(|s| Hashtag(s.trim().to_string()))
            .ok_or_else(|| "Tag is not a Hashtag".to_string())
    }
}
impl From<&str> for Hashtag {
    fn from(value: &str) -> Self {
        Hashtag(value.trim().to_string())
    }
}
impl From<&Hashtag> for Tag {
    fn from(value: &Hashtag) -> Self {
        Tag::hashtag(&value.0)
    }
}

impl Ord for Hashtag {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.to_ascii_lowercase().cmp(&other.0.to_ascii_lowercase()) 
        // Wanted to do this so lowercase tags are preferred,
        // but is technically undefined behaviour
        // because it deviates from Eq implementation
        //match {
        //    Ordering::Equal => self.0.cmp(&other.0),
        //    other => other,
        //}
    }
}
impl PartialOrd for Hashtag {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.0.to_ascii_lowercase().cmp(&other.0.to_ascii_lowercase()))
    }
}

#[test]
fn test_hashtag() {
    assert_eq!("yeah", "YeaH".to_ascii_lowercase());
    assert_eq!("yeah".to_ascii_lowercase().cmp(&"YeaH".to_ascii_lowercase()), Ordering::Equal);
    
    let strings = vec!["yeah", "YeaH"];
    let mut tags = strings.iter().cloned().map(Hashtag::from).sorted_unstable().collect_vec();
    assert_eq!(strings, tags.iter().map(Hashtag::deref).collect_vec());
    tags.sort_unstable();
    assert_eq!(strings, tags.iter().map(Hashtag::deref).collect_vec());
}