//! RFC 6901 JSON Pointer parsing, resolution, and removal (Task 4.1).
//!
//! Every rule in [`crate::rules::NormalizeRule`] addresses part of a
//! Kubernetes object by JSON Pointer, so this module is the single place
//! that decides what a pointer string means. It is deliberately small
//! and total: parsing either produces a [`JsonPointer`] or a
//! [`PointerError`], and resolution/removal against a document either
//! finds the addressed location or reports that it does not exist. There
//! is no third "sort of matched" outcome anywhere in this file.
//!
//! # Why not `serde_json`'s own `Value::pointer`
//!
//! `serde_json` ships `Value::pointer`/`Value::pointer_mut`, which do
//! implement RFC 6901 escaping, but they are unusable for this crate's
//! job in three specific ways:
//!
//! - They have **no removal** counterpart. Two of this crate's three
//!   rule kinds are removals, and removing requires resolving the
//!   *parent* and then taking the final token off it — a split
//!   `Value::pointer` does not expose.
//! - They **silently accept a malformed pointer** as "not found":
//!   `Value::pointer("spec")` (no leading `/`) and
//!   `Value::pointer("/a~2b")` (an invalid escape) both return `None`,
//!   indistinguishable from a well-formed pointer that simply does not
//!   match this document. Task 4.1 needs those cases separated — a
//!   pointer that cannot be parsed is a broken rule the caller must fix
//!   ([`PointerError`], surfaced as `NormalizeError`), while a
//!   well-formed pointer that matches nothing is an ordinary no-op.
//!   Collapsing the two would let a typo in a user profile look exactly
//!   like a rule that correctly had nothing to do.
//! - Parsing is **re-done on every call**. `normalize_object` validates
//!   every rule's pointer up front (see `object.rs`), and a
//!   [`JsonPointer`] carries its already-unescaped tokens so that
//!   validation is not thrown away and silently repeated.
//!
//! # Array indices
//!
//! RFC 6901 §4 is precise about which tokens address an array element:
//! `"0"`, or a digit string with no leading zero. `"01"`, `"-1"`, `"+1"`,
//! `" 1"`, and the empty token are *not* array indices, and `"-"` refers
//! to the (nonexistent) element after the last one — meaningful for JSON
//! Patch `add`, meaningless for reading or removing. [`array_index`]
//! implements exactly that, and every non-index token against an array
//! resolves to "does not exist" rather than being coerced into a
//! plausible-looking position.

use serde_json::Value;
use thiserror::Error;

/// A JSON Pointer string that is not valid RFC 6901 syntax.
///
/// Distinct, by design, from "this pointer matched nothing": see this
/// module's own documentation for why those two must not collapse into
/// one another.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PointerError {
    /// The pointer was non-empty and did not start with `/`. RFC 6901
    /// allows exactly two shapes: the empty string (the whole document)
    /// or one or more `/`-prefixed reference tokens.
    #[error("JSON Pointer must be empty or begin with `/`, got {pointer:?}")]
    MissingLeadingSlash {
        /// The rejected pointer, exactly as written.
        pointer: String,
    },
    /// The pointer contained a `~` that was not followed by `0` or `1`.
    /// RFC 6901 §3 makes `~` an escape introducer with exactly those two
    /// continuations, so `~2` and a trailing `~` are both syntax errors,
    /// not literal tildes.
    #[error(
        "JSON Pointer {pointer:?} contains an invalid `~` escape: `~` must be followed by `0` or `1`"
    )]
    InvalidEscape {
        /// The rejected pointer, exactly as written.
        pointer: String,
    },
}

/// A parsed RFC 6901 JSON Pointer: the original string plus its
/// reference tokens with `~1`/`~0` already unescaped.
///
/// Constructing one is the only way to prove a pointer string is
/// well-formed, which is why `object.rs` builds every rule's pointer
/// before it mutates anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonPointer {
    raw: String,
    tokens: Vec<String>,
}

impl JsonPointer {
    /// Parses `raw` as an RFC 6901 JSON Pointer.
    ///
    /// The empty string is valid and addresses the whole document (see
    /// [`JsonPointer::is_document_root`]). `"/"` is *not* the same
    /// thing: it addresses the member whose key is the empty string,
    /// which is why the split below is on the body after the leading
    /// `/` rather than on the whole pointer.
    ///
    /// # Errors
    ///
    /// [`PointerError::MissingLeadingSlash`] if `raw` is non-empty and
    /// does not begin with `/`; [`PointerError::InvalidEscape`] if any
    /// token contains a `~` not followed by `0` or `1`.
    pub fn parse(raw: &str) -> Result<Self, PointerError> {
        if raw.is_empty() {
            return Ok(Self {
                raw: String::new(),
                tokens: Vec::new(),
            });
        }
        let Some(body) = raw.strip_prefix('/') else {
            return Err(PointerError::MissingLeadingSlash {
                pointer: raw.to_owned(),
            });
        };
        let tokens = body
            .split('/')
            .map(|token| unescape_token(token, raw))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            raw: raw.to_owned(),
            tokens,
        })
    }

    /// Builds a pointer from already-unescaped reference tokens,
    /// escaping each one back into RFC 6901 syntax for
    /// [`JsonPointer::as_str`].
    ///
    /// This is how [`crate::rules::NormalizeRule::RemoveAnnotation`]
    /// reaches an annotation key: an annotation key such as
    /// `kubectl.kubernetes.io/last-applied-configuration` contains a
    /// `/`, so the pointer that addresses it is
    /// `/metadata/annotations/kubectl.kubernetes.io~1last-applied-configuration`.
    /// Building it from tokens means no caller ever has to hand-escape a
    /// key, and a key containing a literal `~` is handled by the same
    /// code path rather than a forgotten special case.
    #[must_use]
    pub fn from_tokens<I, S>(tokens: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let tokens: Vec<String> = tokens.into_iter().map(Into::into).collect();
        let mut raw = String::new();
        for token in &tokens {
            raw.push('/');
            raw.push_str(&escape_token(token));
        }
        Self { raw, tokens }
    }

    /// The pointer in RFC 6901 wire form, with escapes intact. This is
    /// what `NormalizationEvidence` records, so evidence text always
    /// names a pointer a reader could paste back into a profile.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The pointer's reference tokens, already unescaped — that is, a
    /// token here is the literal object key or array index it addresses.
    #[must_use]
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }

    /// Whether this pointer addresses the whole document (the empty
    /// pointer).
    ///
    /// `object.rs` rejects a removal rule with such a pointer outright
    /// rather than applying it; see `NormalizeError::RemovesDocumentRoot`
    /// for why that is an error and not a very effective normalization.
    #[must_use]
    pub fn is_document_root(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// Escapes one reference token: `~` becomes `~0` and `/` becomes `~1`.
///
/// The `~` substitution must run first. Doing `/` first would turn
/// `a/b` into `a~1b` and then the `~` pass would turn that into
/// `a~01b`, which unescapes to `a~1b` — a different key. RFC 6901 §3
/// calls this out explicitly, and [`unescape_token`] undoes the pair in
/// a single left-to-right scan, which is order-independent by
/// construction.
#[must_use]
pub fn escape_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Undoes [`escape_token`] in one left-to-right pass.
fn unescape_token(token: &str, raw: &str) -> Result<String, PointerError> {
    if !token.contains('~') {
        // Overwhelmingly the common case; avoids allocating a char
        // iterator's worth of work per token for every built-in rule.
        return Ok(token.to_owned());
    }
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(character) = chars.next() {
        if character == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                _ => {
                    return Err(PointerError::InvalidEscape {
                        pointer: raw.to_owned(),
                    });
                }
            }
        } else {
            out.push(character);
        }
    }
    Ok(out)
}

/// Interprets one reference token as an index into an array of `len`
/// elements, per RFC 6901 §4.
///
/// Returns `None` — "this token does not address an existing element" —
/// for a non-numeric token, a leading-zero form such as `"01"`, a signed
/// form such as `"-1"`/`"+1"`, the `"-"` end-of-array token, and any
/// in-range-looking index that is actually past the end. Every one of
/// those is a pointer that simply does not resolve, never a coerced
/// position.
fn array_index(token: &str, len: usize) -> Option<usize> {
    if token.is_empty() {
        return None;
    }
    if token != "0" && token.starts_with('0') {
        return None;
    }
    if !token.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let index: usize = token.parse().ok()?;
    (index < len).then_some(index)
}

/// Walks `root` down `tokens`, returning the addressed value if every
/// token resolves.
fn resolve_tokens<'a>(root: &'a Value, tokens: &[String]) -> Option<&'a Value> {
    let mut current = root;
    for token in tokens {
        current = match current {
            Value::Object(map) => map.get(token)?,
            Value::Array(items) => items.get(array_index(token, items.len())?)?,
            // A scalar has no members: any remaining token addresses
            // nothing, which is a no-op and not an error.
            _ => return None,
        };
    }
    Some(current)
}

/// [`resolve_tokens`], but handing back a mutable reference.
fn resolve_tokens_mut<'a>(root: &'a mut Value, tokens: &[String]) -> Option<&'a mut Value> {
    let mut current = root;
    for token in tokens {
        current = match current {
            Value::Object(map) => map.get_mut(token)?,
            Value::Array(items) => {
                let index = array_index(token, items.len())?;
                items.get_mut(index)?
            }
            _ => return None,
        };
    }
    Some(current)
}

/// Resolves `pointer` against `root`.
///
/// `None` means the pointer does not address anything in this document.
/// That is an ordinary outcome, not a failure: normalization profiles
/// are written once and applied to many object kinds, so most rules miss
/// most objects.
#[must_use]
pub fn resolve<'a>(root: &'a Value, pointer: &JsonPointer) -> Option<&'a Value> {
    resolve_tokens(root, pointer.tokens())
}

/// Resolves `pointer` against `root` mutably.
#[must_use]
pub fn resolve_mut<'a>(root: &'a mut Value, pointer: &JsonPointer) -> Option<&'a mut Value> {
    resolve_tokens_mut(root, pointer.tokens())
}

/// Removes whatever `pointer` addresses, returning the removed value.
///
/// `None` means nothing was removed, for any of three reasons that are
/// all no-ops at this level: the pointer addresses the whole document
/// (a document cannot remove itself — `object.rs` rejects that rule
/// before ever reaching here), the parent does not exist, or the parent
/// exists but has no such member. The caller distinguishes
/// removed-something from removed-nothing purely by `Some`/`None`, which
/// is what makes `NormalizationEvidence::applied_rules` a record of
/// rules that actually changed the object rather than rules that were
/// merely configured.
pub fn remove(root: &mut Value, pointer: &JsonPointer) -> Option<Value> {
    let (last, parent_tokens) = pointer.tokens().split_last()?;
    match resolve_tokens_mut(root, parent_tokens)? {
        Value::Object(map) => map.remove(last),
        Value::Array(items) => {
            let index = array_index(last, items.len())?;
            Some(items.remove(index))
        }
        _ => None,
    }
}
