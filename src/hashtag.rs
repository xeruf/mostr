use nostr_sdk::{Alphabet, Tag};
use std::cmp::Ordering;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};

pub fn is_hashtag(tag: &Tag) -> bool {
    tag.single_letter_tag()
        .is_some_and(|letter| letter.character == Alphabet::T)
}

/// This exists so that Hashtags can easily be matched without caring about case
/// but displayed in their original case
#[derive(Clone, Debug)]
pub struct Hashtag {
    value: String,
    lowercased: String,
}

impl Hashtag {
    pub fn matches(&self, token: &str) -> bool {
        self.lowercased.contains(&token.to_ascii_lowercase())
    }
}

impl Display for Hashtag {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl Hash for Hashtag {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(self.lowercased.as_bytes());
    }
}

impl Eq for Hashtag {}
impl PartialEq<Self> for Hashtag {
    fn eq(&self, other: &Self) -> bool {
        self.lowercased == other.lowercased
    }
}
impl TryFrom<&Tag> for Hashtag {
    type Error = String;

    fn try_from(value: &Tag) -> Result<Self, Self::Error> {
        value.content().take_if(|_| is_hashtag(value))
            .map(|s| Hashtag::from(s))
            .ok_or_else(|| "Tag is not a Hashtag".to_string())
    }
}
impl From<&str> for Hashtag {
    fn from(value: &str) -> Self {
        let val = value.trim().to_string();
        Hashtag {
            lowercased: val.to_ascii_lowercase(),
            value: val,
        }
    }
}
impl From<&Hashtag> for Tag {
    fn from(value: &Hashtag) -> Self {
        Tag::hashtag(&value.lowercased)
    }
}

impl Ord for Hashtag {
    fn cmp(&self, other: &Self) -> Ordering {
        self.lowercased.cmp(&other.lowercased)
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
        Some(self.lowercased.cmp(&other.lowercased))
    }
}

#[test]
fn test_hashtag() {
    assert_eq!("yeah", "YeaH".to_ascii_lowercase());
    assert_eq!("yeah".to_ascii_lowercase().cmp(&"YeaH".to_ascii_lowercase()), Ordering::Equal);

    use itertools::Itertools;
    let strings = vec!["yeah", "YeaH"];
    let mut tags = strings.iter().cloned().map(Hashtag::from).sorted_unstable().collect_vec();
    assert_eq!(strings, tags.iter().map(ToString::to_string).collect_vec());
    tags.sort_unstable();
    assert_eq!(strings, tags.iter().map(ToString::to_string).collect_vec());
}