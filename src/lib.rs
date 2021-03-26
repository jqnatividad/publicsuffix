//! A native Rust library for Mozilla's Public Suffix List

#![cfg_attr(not(any(feature = "punycode", feature = "std")), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(feature = "anycase")]
mod anycase;
mod error;
mod fxhash;

use alloc::borrow::ToOwned;
#[cfg(not(feature = "anycase"))]
use alloc::vec::Vec;
#[cfg(feature = "anycase")]
use anycase::AnyCase;
#[cfg(feature = "anycase")]
use core::str;
use core::str::{from_utf8, FromStr};
use fxhash::FxBuildHasher;
use hashbrown::HashMap;

pub use error::Error;
pub use psl_types::{Domain, Info, List as Psl, Suffix, Type};

/// The official URL of the list
pub const LIST_URL: &str = "https://publicsuffix.org/list/public_suffix_list.dat";

#[cfg(not(feature = "anycase"))]
type Children = HashMap<Vec<u8>, Node, FxBuildHasher>;

#[cfg(feature = "anycase")]
type Children = HashMap<AnyCase<'static>, Node, FxBuildHasher>;

const WILDCARD: &str = "*";

#[derive(Debug, Clone, Default, Eq, PartialEq)]
struct Node {
    children: Children,
    leaf: Option<Leaf>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Leaf {
    is_exception: bool,
    typ: Type,
}

/// A dynamic public suffix list
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct List {
    rules: Node,
}

impl List {
    /// Creates a new list with default wildcard rule support
    #[inline]
    pub fn new() -> Self {
        Default::default()
    }

    /// Creates a new list from a byte slice
    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        from_utf8(bytes)
            .map_err(|_| Error::ListNotUtf8Encoded)?
            .parse()
    }

    /// Checks to see if the list is empty, ignoring the wildcard rule
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.rules.children.is_empty()
    }

    #[inline]
    fn append(&mut self, mut rule: &str, typ: Type) -> Result<(), Error> {
        let mut is_exception = false;
        if rule.starts_with('!') {
            if !rule.contains('.') {
                return Err(Error::ExceptionAtFirstLabel(rule.to_owned()));
            }
            is_exception = true;
            rule = &rule[1..];
        }

        let mut current = &mut self.rules;
        for label in rule.rsplit('.') {
            if label.is_empty() {
                return Err(Error::EmptyLabel(rule.to_owned()));
            }

            #[cfg(not(feature = "anycase"))]
            let key = label.as_bytes().to_owned();
            #[cfg(feature = "anycase")]
            let key = AnyCase::from(label.to_owned());

            current = current.children.entry(key).or_insert_with(Default::default);
        }

        current.leaf = Some(Leaf { is_exception, typ });

        Ok(())
    }
}

#[cfg(feature = "anycase")]
macro_rules! anycase_key {
    ($label:ident) => {
        match str::from_utf8($label) {
            Ok(label) => AnyCase::from(label),
            Err(_) => return Info { len: 0, typ: None },
        }
    };
}

impl Psl for List {
    #[inline]
    fn find<'a, T>(&self, mut labels: T) -> Info
    where
        T: Iterator<Item = &'a [u8]>,
    {
        let mut rules = &self.rules;

        // the first label
        // it's special because we always need it whether or not
        // it's in our hash map (because of the implicit wildcard)
        let mut info = match labels.next() {
            Some(label) => {
                let mut info = Info {
                    len: label.len(),
                    typ: None,
                };
                #[cfg(not(feature = "anycase"))]
                let node_opt = rules.children.get(label);
                #[cfg(feature = "anycase")]
                let node_opt = rules.children.get(&anycase_key!(label));
                match node_opt {
                    Some(node) => {
                        info.typ = node.leaf.map(|leaf| leaf.typ);
                        rules = node;
                    }
                    None => return info,
                }
                info
            }
            None => return Info { len: 0, typ: None },
        };

        // the rest of the labels
        let mut len_so_far = info.len;
        for label in labels {
            #[cfg(not(feature = "anycase"))]
            let node_opt = rules.children.get(label);
            #[cfg(feature = "anycase")]
            let node_opt = rules.children.get(&anycase_key!(label));
            match node_opt {
                Some(node) => rules = node,
                None => {
                    #[cfg(not(feature = "anycase"))]
                    let node_opt = rules.children.get(WILDCARD.as_bytes());
                    #[cfg(feature = "anycase")]
                    let node_opt = rules.children.get(&AnyCase::from(WILDCARD));
                    match node_opt {
                        Some(node) => rules = node,
                        None => break,
                    }
                }
            }
            let label_plus_dot = label.len() + 1;
            if let Some(leaf) = rules.leaf {
                info.typ = Some(leaf.typ);
                if leaf.is_exception {
                    info.len = len_so_far;
                    break;
                } else {
                    info.len = len_so_far + label_plus_dot;
                }
            }
            len_so_far += label_plus_dot;
        }

        info
    }
}

impl FromStr for List {
    type Err = Error;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut typ = None;
        let mut list = List::new();
        for line in s.lines() {
            match line {
                line if line.contains("BEGIN ICANN DOMAINS") => {
                    typ = Some(Type::Icann);
                }
                line if line.contains("BEGIN PRIVATE DOMAINS") => {
                    typ = Some(Type::Private);
                }
                line if line.starts_with("//") => {
                    continue;
                }
                line => match typ {
                    Some(typ) => {
                        let rule = match line.split_whitespace().next() {
                            Some(rule) => rule,
                            None => continue,
                        };
                        list.append(rule, typ)?;
                        #[cfg(feature = "punycode")]
                        {
                            let ascii = idna::domain_to_ascii(rule)
                                .map_err(|_| Error::InvalidRule(rule.to_owned()))?;
                            list.append(&ascii, typ)?;
                        }
                    }
                    None => {
                        continue;
                    }
                },
            }
        }
        if list.is_empty() {
            return Err(Error::InvalidList);
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &[u8] = b"
        // BEGIN ICANN DOMAINS
        com.uk
        ";

    #[test]
    fn list_construction() {
        let list = List::from_bytes(LIST).unwrap();
        let expected = List {
            rules: Node {
                children: {
                    let mut children = Children::default();
                    children.insert(
                        #[cfg(not(feature = "anycase"))]
                        b"uk".to_vec(),
                        #[cfg(feature = "anycase")]
                        AnyCase::from("uk"),
                        Node {
                            children: {
                                let mut children = Children::default();
                                children.insert(
                                    #[cfg(not(feature = "anycase"))]
                                    b"com".to_vec(),
                                    #[cfg(feature = "anycase")]
                                    AnyCase::from("com"),
                                    Node {
                                        children: Default::default(),
                                        leaf: Some(Leaf {
                                            is_exception: false,
                                            typ: Type::Icann,
                                        }),
                                    },
                                );
                                children
                            },
                            leaf: None,
                        },
                    );
                    children
                },
                leaf: None,
            },
        };
        assert_eq!(list, expected);
    }

    #[test]
    fn find_localhost() {
        let list = List::from_bytes(LIST).unwrap();
        let labels = b"localhost".rsplit(|x| *x == b'.');
        assert_eq!(list.find(labels), Info { len: 9, typ: None });
    }

    #[test]
    fn find_uk() {
        let list = List::from_bytes(LIST).unwrap();
        let labels = b"uk".rsplit(|x| *x == b'.');
        assert_eq!(list.find(labels), Info { len: 2, typ: None });
    }

    #[test]
    fn find_com_uk() {
        let list = List::from_bytes(LIST).unwrap();
        let labels = b"com.uk".rsplit(|x| *x == b'.');
        assert_eq!(
            list.find(labels),
            Info {
                len: 6,
                typ: Some(Type::Icann)
            }
        );
    }

    #[test]
    fn find_ide_kyoto_jp() {
        let list = List::from_bytes(b"// BEGIN ICANN DOMAINS\nide.kyoto.jp").unwrap();
        let labels = b"ide.kyoto.jp".rsplit(|x| *x == b'.');
        assert_eq!(
            list.find(labels),
            Info {
                len: 12,
                typ: Some(Type::Icann)
            }
        );
    }
}
