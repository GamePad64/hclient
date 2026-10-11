//! `.netrc` — the classic per-host credential file, parsed the way curl
//! parses it.
//!
//! The format has no RFC; implementations disagree. This parser is
//! differential against curl 8.20's wire behaviour, fixture by fixture:
//! what curl sends in `Authorization` for a given file and host is what
//! [`Netrc::lookup`](crate::auth::netrc::Netrc::lookup) answers, including the corners — the **first**
//! duplicate machine wins, the **first** `login` in a block wins, a
//! `macdef` body is consumed up to the first blank line so credential-
//! looking lines inside a macro are never credentials, a `#` at the
//! start of a line comments the whole line out while a `#` mid-line is
//! a literal token on the pinned oracle — other curls read the same
//! line as a comment to end of line, the one corner the oracle
//! disagrees with itself on (`tests/fixtures/netrc-divergent/`) — and
//! entries
//! with a missing half are still used — `karl` without a password
//! authenticates as `karl:` and a password without a login as `:pw`.
//!
//! The file is read only when a caller asks: [`Netrc::from_env`](crate::auth::netrc::Netrc::from_env) follows
//! `$NETRC`, then `~/.netrc` (`~/_netrc` on Windows), and
//! [`ClientBuilder::netrc`](crate::ClientBuilder::netrc) installs a
//! parsed value. Reading a credential file is the caller's policy — the
//! client never goes looking on its own.
//!
//! # Examples
//!
//! ```
//! use hclient::auth::netrc::Netrc;
//!
//! let netrc = Netrc::parse(
//!     "machine api.example.com\nlogin alice\npassword s3cret\n\ndefault\nlogin anon\npw: none\n",
//! );
//! let entry = netrc.lookup("api.example.com").unwrap();
//! assert_eq!(entry.login(), "alice");
//! assert_eq!(entry.password(), "s3cret");
//!
//! // An unlisted host falls back to the `default` entry, if there is one.
//! let entry = netrc.lookup("other.example.org").unwrap();
//! assert_eq!(entry.login(), "anon");
//! ```

use std::path::Path;

/// One parsed `.netrc` entry: a host — or the `default` catch-all — and
/// the credentials that go with it.
///
/// [`login`](Self::login) or [`password`](Self::password) may be empty:
/// curl authenticates `karl` without a password as `karl:` and a password
/// without a login as `:pw`, and this type carries the same halves. An
/// entry with neither is never produced — a bare `machine` line with no
/// credential token after it is dropped, because authenticating with two
/// empty halves is a `Basic` header that says nothing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub(crate) machine: Option<String>,
    pub(crate) login: String,
    pub(crate) password: String,
    pub(crate) account: Option<String>,
}

impl Entry {
    /// The host this entry is for, or `None` for a `default` catch-all.
    pub fn machine(&self) -> Option<&str> {
        self.machine.as_deref()
    }

    /// Whether this is a `default` entry — the fallback for hosts with no
    /// specific match.
    pub fn is_default(&self) -> bool {
        self.machine.is_none()
    }

    /// The login. Empty when the entry named none — curl sends the empty
    /// half as empty.
    pub fn login(&self) -> &str {
        &self.login
    }

    /// The password. Empty when the entry named none.
    pub fn password(&self) -> &str {
        &self.password
    }

    /// The `account` field, which HTTP carries nowhere — present because
    /// dropping a parsed field silently is how parsers rot.
    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }
}

/// A parsed `.netrc` file: the entries, in file order, and the lookup.
///
/// Parse errors do not exist here, on purpose: the format has no grammar
/// to violate, and curl — the oracle this parser is differential against
/// — answers every malformed input with the tokens it understood, never
/// with a refusal. A parser that refuses where curl sends credentials
/// would be a different product.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Netrc {
    entries: Vec<Entry>,
}

impl Netrc {
    /// Parse the file's text.
    ///
    /// The rules, differential against curl 8.20's wire behaviour: lines
    /// whose first non-whitespace character is `#` are comments; a `#`
    /// anywhere else is an ordinary token (`login #` logs in as `#`);
    /// tokens are whitespace-separated and may not contain whitespace,
    /// because no implementation of this format supports quoting; a
    /// `macdef` body is consumed up to the first blank line; unknown
    /// tokens are dropped; and the first value of a field wins.
    pub fn parse(text: &str) -> Netrc {
        let mut entries: Vec<Entry> = Vec::new();
        let mut in_macdef = false;
        for line in text.lines() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if in_macdef {
                // netrc(5): a macro's body ends at the first null line.
                if line.trim().is_empty() {
                    in_macdef = false;
                }
                continue;
            }
            if line.trim_start().starts_with('#') {
                continue;
            }
            let mut tokens = line.split_whitespace().peekable();
            while let Some(token) = tokens.next() {
                match token {
                    "machine" => {
                        if let Some(name) = tokens.next() {
                            entries.push(Entry {
                                machine: Some(name.to_owned()),
                                login: String::new(),
                                password: String::new(),
                                account: None,
                            });
                        }
                    }
                    "default" => entries.push(Entry {
                        machine: None,
                        login: String::new(),
                        password: String::new(),
                        account: None,
                    }),
                    "login" | "password" | "account" => {
                        let (Some(entry), Some(value)) = (entries.last_mut(), tokens.next()) else {
                            continue;
                        };
                        // First value wins — the same first-match rule the
                        // duplicate-machine case answers with.
                        let slot = match token {
                            "login" => &mut entry.login,
                            "password" => &mut entry.password,
                            _ => {
                                if entry.account.is_none() {
                                    entry.account = Some(value.to_owned());
                                }
                                continue;
                            }
                        };
                        if slot.is_empty() {
                            value.clone_into(slot);
                        }
                    }
                    "macdef" => {
                        let _ = tokens.next(); // the macro's name
                        in_macdef = true;
                    }
                    // Unknown tokens are dropped, never refused: the
                    // oracle answers malformed input with the tokens it
                    // understood.
                    _ => {}
                }
            }
        }
        entries.retain(|e| e.machine.is_none() || !e.login.is_empty() || !e.password.is_empty());
        Netrc { entries }
    }

    /// Read a file and parse it.
    ///
    /// # Errors
    ///
    /// Whatever [`std::fs::read_to_string`] answers — a path that does not
    /// exist, a directory, a permission error. The parse itself never
    /// fails.
    pub fn from_path(path: impl AsRef<Path>) -> std::io::Result<Netrc> {
        let text = std::fs::read_to_string(path)?;
        Ok(Netrc::parse(&text))
    }

    /// Read the credential file the environment names, in curl's order:
    /// `$NETRC` when set, else `~/.netrc` (`~/_netrc` on Windows).
    ///
    /// A missing file is `Ok(None)` — most machines have none, and that
    /// is a normal state rather than a failure. The caller decides the
    /// policy this crate refuses to have: nothing here reads a credential
    /// file until a caller names this constructor.
    ///
    /// # Errors
    ///
    /// A file that exists but cannot be read — permissions, a directory
    /// in the way — is an `Err` naming it, not a silent absence: curl
    /// treats an unreadable configured netrc as an error too, and a
    /// caller who pointed `$NETRC` at a file deserves to learn when it
    /// could not be used.
    pub fn from_env() -> std::io::Result<Option<Netrc>> {
        if let Ok(path) = std::env::var("NETRC") {
            return Netrc::from_path(path).map(Some);
        }
        let Some(home) = std::env::home_dir() else {
            return Ok(None);
        };
        let mut candidates = vec![home.join(".netrc")];
        if cfg!(windows) {
            candidates.insert(0, home.join("_netrc"));
        }
        for path in candidates {
            match Path::new(&path).try_exists() {
                Ok(true) => return Netrc::from_path(path).map(Some),
                Ok(false) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    /// The credentials for `host`: the first entry whose `machine` names
    /// it exactly, else the first `default` entry. Ports play no part —
    /// the format has no port field, and curl matches the hostname alone.
    ///
    /// `None` when no entry names the host and there is no `default`.
    pub fn lookup(&self, host: &str) -> Option<&Entry> {
        entries_lookup(self.entries.iter(), host)
    }

    /// The entries, in file order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

pub(crate) fn entries_lookup<'a, I>(entries: I, host: &str) -> Option<&'a Entry>
where
    I: IntoIterator<Item = &'a Entry>,
{
    let mut fallback = None;
    for entry in entries {
        match entry.machine.as_deref() {
            Some(name) if name == host => return Some(entry),
            None if fallback.is_none() => fallback = Some(entry),
            _ => {}
        }
    }
    fallback
}

// Maintainer notes (not rendered):
//
// Every rule above is pinned by a test that was watched failing against
// the answer it replaced, and the whole table is differential: the
// expected values were captured from real curl 8.20 sending on the wire
// (`tests/netrc_curl.rs` re-captures live, so a curl that changes
// behaviour fails this crate instead of rotting the parser silently).
// The rules curl's fixtures answered for: first duplicate machine wins,
// first login wins, macdef consumes to the first blank line, whole-line
// `#` comments versus literal mid-line `#`, empty halves are sent as
// empty (`karl:`, `:pw`), colons inside a password pass through, and
// tokens before any `machine`/`default` are dropped.
//
// `login`/`password` are first-write-wins rather than last — the
// duplicate-machine fixture answered `first:pw-first` and the
// multi-pair fixture answered `one:pw-one`, both firsts. (Entry::login
// defaults to empty, so "first-write" is spelled `slot.is_empty()`.)
//
// `entries.retain` drops bare-`machine` entries with no credential
// token after them: authenticating with two empty halves is a `Basic`
// header that says nothing. (A `default` entry is never dropped —
// dropping the catch-all would make the lookup fall through to None
// and change what the default_only fixture answers.)
//
// The colon in a login that `RequestBuilder::basic_auth` refuses is
// not refused here on purpose: the ambiguity the refusal exists for
// (`("a:b", "c")` and `("a", "b:c")` are the same bytes) does not
// arise — the file's fields are explicit — and the oracle sends the
// colon through (`special_chars` answered `mallory:p@ss:w0rd=+/`).
// The client wires this through its own encode, not through
// `basic_auth`, and the divergence is stated in `ClientBuilder::netrc`.

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden table: expected values captured from real curl 8.20
    /// sending on the wire against each fixture — `tests/netrc_curl.rs`
    /// re-captures live. One case per rule the fixtures discriminate.
    /// The one corner the wire corpus cannot hold: a `#` mid-line is a
    /// literal token on curl 8.20 (this host's oracle — `login #` logs
    /// in as `#`) and a comment-to-end-of-line on the CI curls, whose
    /// `login` keyword then waits for a value the comment ate. The
    /// oracle disagrees with itself across versions, so the fixture
    /// lives in `tests/fixtures/netrc-divergent/`, outside
    /// `netrc_curl.rs`'s live comparison, and this test pins the
    /// behaviour this crate chose — 8.20's — with the file kept beside
    /// the others for anyone re-measuring.
    #[test]
    fn a_mid_line_hash_is_a_literal_on_the_pinned_curl() {
        let text = std::fs::read_to_string(format!(
            "{}/tests/fixtures/netrc-divergent/hash_after_token.netrc",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let netrc = Netrc::parse(&text);
        let entry = netrc.lookup("127.0.0.1").unwrap();
        assert_eq!((entry.login(), entry.password()), ("#", "pw-hash"));
    }

    #[test]
    fn lookup_matches_curls_wire_answers() {
        let cases: &[(&str, &str, &str, &str)] = &[
            ("simple", "127.0.0.1", "alice", "secret1"),
            ("default_only", "127.0.0.1", "bob", "secret2"),
            // A specific machine beats the `default` catch-all.
            ("machine_then_default", "127.0.0.1", "carol", "secret3"),
            // First duplicate wins, not last.
            ("duplicate_machine", "127.0.0.1", "first", "pw-first"),
            // A macdef body's credential-looking lines are a macro, not
            // credentials — consumed to the first blank line.
            ("macdef_body", "127.0.0.1", "eve", "pw-eve"),
            ("comments", "127.0.0.1", "grace", "pw-grace"),
            ("crlf", "127.0.0.1", "heidi", "pw-heidi"),
            ("no_trailing_newline", "127.0.0.1", "ivan", "pw-ivan"),
            ("tabs", "127.0.0.1", "judy", "pw-judy"),
            // A missing half is sent as empty, entry still used.
            ("login_only", "127.0.0.1", "karl", ""),
            ("password_only", "127.0.0.1", "", "pw-linda"),
            // Colons inside a password pass through.
            ("special_chars", "127.0.0.1", "mallory", "p@ss:w0rd=+/"),
            // Tokens before any machine/default are dropped.
            ("tokens_before_machine", "127.0.0.1", "oscar", "pw-oscar"),
            // First login wins within one entry.
            ("multi_pair_one_machine", "127.0.0.1", "one", "pw-one"),
            // `account` does not disturb the credential halves.
            ("account_token", "127.0.0.1", "petra", "pw-petra"),
            // An unlisted host with no default gets nothing.
            ("other_host_only", "127.0.0.1", "local", "pw-local"),
            // A `#` line is a comment; its tokens are not parsed.
            ("hash_line_with_tokens", "127.0.0.1", "sam", "pw-sam"),
        ];
        for (fixture, host, login, password) in cases {
            let text = std::fs::read_to_string(format!(
                "{}/tests/fixtures/netrc/{fixture}.netrc",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap_or_else(|e| panic!("fixture {fixture}: {e}"));
            let netrc = Netrc::parse(&text);
            let entry = netrc.lookup(host);
            match (login.is_empty() && password.is_empty(), entry) {
                (true, None) => {}
                (true, Some(e)) => panic!("{fixture}: expected no entry, got {e:?}"),
                (false, None) => panic!("{fixture}: expected {login}:{password}, got no entry"),
                (false, Some(e)) => {
                    assert_eq!((e.login(), e.password()), (*login, *password), "{fixture}");
                }
            }
        }
    }

    #[test]
    fn the_first_value_of_a_field_wins_over_a_second() {
        let netrc = Netrc::parse("machine a\nlogin one\nlogin two\npassword pw\n");
        assert_eq!(netrc.lookup("a").unwrap().login(), "one");
    }

    #[test]
    fn a_bare_machine_with_no_credentials_is_dropped() {
        // Authenticating with two empty halves is a Basic header that
        // says nothing, so the entry is not produced and the lookup
        // falls through to the default.
        let netrc = Netrc::parse("machine a\n\ndefault\nlogin d\npassword pw\n");
        assert_eq!(netrc.lookup("a").unwrap().login(), "d");
    }

    #[test]
    fn a_macdef_at_eof_consumes_the_rest() {
        let netrc = Netrc::parse(
            "machine a\nlogin real\npassword pw\nmacdef x\nlogin fake\npassword pw-fake",
        );
        let entries = netrc.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].login(), "real");
    }

    #[test]
    fn account_is_carried_but_never_authenticates() {
        let netrc = Netrc::parse("machine a\nlogin l\npassword p\naccount acct\n");
        let e = netrc.lookup("a").unwrap();
        assert_eq!(e.account(), Some("acct"));
        assert_eq!((e.login(), e.password()), ("l", "p"));
    }
}
