//! Argument parsing, deliberately hand-written.
//!
//! It copies `parseArgs` in `bin/lumberroom.mjs` byte for byte in behaviour, including the parts a real
//! argument library would improve on: `--flag` with no value is `true`, a value is taken from the
//! next argument only when that argument does not itself start with `--`, and `--k=v` wins over
//! both. The two clients share acceptance scripts and a config file, so a flag that parses
//! differently in one of them is a bug the owner finds at 2am rather than in review.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Flag {
    /// `--flag` with nothing usable after it.
    Bare,
    Value(String),
}

#[derive(Debug, Default, Clone)]
pub struct Args {
    pub positional: Vec<String>,
    pub flags: HashMap<String, Flag>,
}

impl Args {
    pub fn parse<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let argv: Vec<String> = argv.into_iter().map(Into::into).collect();
        let mut out = Args::default();
        let mut i = 0;
        while i < argv.len() {
            let arg = &argv[i];
            if let Some(rest) = arg.strip_prefix("--") {
                match rest.split_once('=') {
                    Some((key, inline)) => {
                        out.flags.insert(key.to_string(), Flag::Value(inline.to_string()));
                    }
                    None => {
                        let next = argv.get(i + 1);
                        match next {
                            Some(v) if !v.starts_with("--") => {
                                out.flags.insert(rest.to_string(), Flag::Value(v.clone()));
                                i += 1;
                            }
                            _ => {
                                out.flags.insert(rest.to_string(), Flag::Bare);
                            }
                        }
                    }
                }
            } else {
                out.positional.push(arg.clone());
            }
            i += 1;
        }
        out
    }

    /// The flag's value, or None when it is absent or bare. A bare flag is not a value: node's
    /// `flags.namespace === true` check is the same guard, and it is what turns
    /// `write "fact" --namespace` into an error instead of a namespace called "true".
    pub fn value(&self, key: &str) -> Option<&str> {
        match self.flags.get(key) {
            Some(Flag::Value(v)) => Some(v.as_str()),
            _ => None,
        }
    }

    /// First of several aliases carrying a value, in order.
    pub fn value_any(&self, keys: &[&str]) -> Option<&str> {
        keys.iter().find_map(|k| self.value(k))
    }

    /// Present at all, with or without a value. `--json`, `--hook`, `--dry-run`.
    pub fn present(&self, key: &str) -> bool {
        self.flags.contains_key(key)
    }

    /// Set but with no usable value, which several commands report as a usage error.
    pub fn is_bare(&self, key: &str) -> bool {
        matches!(self.flags.get(key), Some(Flag::Bare))
    }

    pub fn positional_at(&self, index: usize) -> Option<&str> {
        self.positional.get(index).map(String::as_str)
    }

    /// Node's `Number.parseInt(String(flag ?? default), 10)`: a trailing-garbage value keeps its
    /// leading digits rather than failing the command.
    pub fn int(&self, key: &str, default: i64) -> i64 {
        match self.value(key) {
            Some(v) => parse_int_prefix(v).unwrap_or(default),
            None => default,
        }
    }

    pub fn float(&self, key: &str, default: f64) -> f64 {
        match self.value(key) {
            Some(v) => v.trim().parse::<f64>().unwrap_or(default),
            None => default,
        }
    }

    /// A comma-separated list, trimmed, empties dropped. None when the flag is absent or bare.
    pub fn comma_list(&self, keys: &[&str]) -> Option<Vec<String>> {
        let raw = self.value_any(keys)?;
        let list: Vec<String> = raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Some(list)
    }
}

/// `parseInt` semantics: read the leading integer and ignore whatever follows.
pub fn parse_int_prefix(s: &str) -> Option<i64> {
    let s = s.trim();
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if i == 0 && (c == '-' || c == '+') {
            end = i + c.len_utf8();
            continue;
        }
        if c.is_ascii_digit() {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    s[..end].parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separated_value_binds_to_the_flag() {
        let a = Args::parse(["search", "what is my name", "--limit", "5"]);
        assert_eq!(a.positional, vec!["search", "what is my name"]);
        assert_eq!(a.value("limit"), Some("5"));
        assert_eq!(a.int("limit", 8), 5);
    }

    #[test]
    fn inline_value_binds_to_the_flag() {
        let a = Args::parse(["stats", "--hours=24"]);
        assert_eq!(a.value("hours"), Some("24"));
    }

    #[test]
    fn a_flag_followed_by_another_flag_is_bare() {
        let a = Args::parse(["forget", "--dry-run", "--query", "ports"]);
        assert!(a.present("dry-run"));
        assert!(a.is_bare("dry-run"));
        assert_eq!(a.value("dry-run"), None);
        assert_eq!(a.value("query"), Some("ports"));
    }

    #[test]
    fn trailing_flag_is_bare() {
        let a = Args::parse(["write", "a fact", "--namespace"]);
        assert!(a.is_bare("namespace"));
        assert_eq!(a.value_any(&["namespace", "ns"]), None);
    }

    #[test]
    fn ns_is_an_alias_for_namespace() {
        let a = Args::parse(["write", "a fact", "--ns", "user:me"]);
        assert_eq!(a.value_any(&["namespace", "ns"]), Some("user:me"));
    }

    #[test]
    fn comma_list_trims_and_drops_empties() {
        let a = Args::parse(["search", "q", "--namespace", "user:me, global ,"]);
        assert_eq!(
            a.comma_list(&["namespace", "namespaces"]),
            Some(vec!["user:me".to_string(), "global".to_string()])
        );
    }

    #[test]
    fn int_takes_the_leading_digits_like_parseint() {
        assert_eq!(parse_int_prefix("25abc"), Some(25));
        assert_eq!(parse_int_prefix("abc"), None);
        assert_eq!(parse_int_prefix("-3"), Some(-3));
        let a = Args::parse(["stats", "--hours", "12h"]);
        assert_eq!(a.int("hours", 168), 12);
    }

    #[test]
    fn a_negative_number_is_not_read_as_a_flag() {
        // Single-dash arguments are positional, matching node's startsWith('--') test.
        let a = Args::parse(["review", "-5"]);
        assert_eq!(a.positional, vec!["review", "-5"]);
    }
}
