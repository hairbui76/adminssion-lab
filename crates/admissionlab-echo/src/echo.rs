//! The frozen echo response body, and the header normalization rules
//! that make two echoed responses comparable.
//!
//! [`EchoBody`] is the single definition of ROADMAP Task 6.5's response
//! contract -- see [`crate`]'s own documentation for the shape itself
//! and for why it is frozen rather than merely current.
//!
//! # What `path` is, and what it is not
//!
//! [`EchoBody::path`] is the request target's path component only: a
//! `GET /payments?tier=gold` is echoed as `"/payments"`. Two reasons,
//! and one consequence worth stating outright.
//!
//! The shape has exactly five keys, and Task 6.8's probe parser is
//! written against them; a sixth `"query"` key would be a contract
//! change to that parser rather than a local addition here, and folding
//! the query *into* `path` would make the field mean two different
//! things depending on the request. The ROADMAP's own example
//! (`"path": "/payments"`) is path-shaped.
//!
//! The consequence: a Gateway that rewrites or appends query parameters
//! is invisible in this body. That is deliberate but not silent -- the
//! full request target, query included, is logged for every request
//! ([`crate::serve`]), so a rewrite is still recoverable from
//! `kubectl logs` while a fixture is being debugged. What Phase 6
//! compares is the backend identity; what an operator debugs is the
//! log. Neither is served by widening the frozen contract.
//!
//! # What `host` is
//!
//! [`EchoBody::host`] is the `Host` request header, verbatim and
//! untrimmed -- the thing an `HTTPRoute`'s `hostnames` matched on, which
//! is why it earns a top-level field rather than only appearing among
//! the headers. When a request carries no `Host` header at all (an
//! HTTP/1.0 client), the request target's own authority is used if the
//! target was absolute-form, and otherwise the field is the empty
//! string: "this request carried no host", never a fabricated or
//! guessed one (Global Constraint 15).
//!
//! The `Host` header is *also* present in [`EchoBody::headers`]. The
//! duplication is deliberate: `headers` is an honest record of what
//! arrived, and quietly removing one header from it because it is
//! reported elsewhere would make the map something other than that.
//!
//! # Header normalization, and exactly what it removes
//!
//! [`EchoBody::headers`] is a [`BTreeMap`], so the JSON is emitted in
//! sorted key order, and every name is lowercase (`hyper` normalizes
//! incoming names; the ordering is this map's). Sorted and lowercased
//! because two textually different renderings of the same request must
//! not read as a difference to a byte-comparing comparator: HTTP header
//! order is not semantically meaningful, and neither is `X-Test` versus
//! `x-test`.
//!
//! A header sent more than once is joined with `", "` into one value,
//! the field-order-preserving combination RFC 9110 §5.3 already defines
//! as equivalent -- again so that the transport's choice between one
//! header and two does not read as a routing difference.
//!
//! **Excluded**: the hop-by-hop headers ([`HOP_BY_HOP`]) plus every
//! name listed in the request's own `Connection` header. These describe
//! *this TCP connection* -- whether it will be reused, how the body was
//! framed, what protocol was proposed -- not the request that a Gateway
//! routed. They are rewritten freely and legitimately by every hop
//! along the way, so echoing them would make two otherwise-identical
//! routings differ for reasons that have nothing to do with the Gateway
//! configuration under test. RFC 9110 §7.6.1 requires an intermediary
//! to strip them, so they are transport artifacts by definition.
//!
//! **Not excluded**: everything else, including the headers proxies
//! inject -- `x-forwarded-for`, `x-forwarded-proto`, `x-envoy-*`,
//! `x-request-id`, and whatever a future Gateway implementation adds.
//! These are genuinely observed behavior of the data plane under test,
//! and this component's job is to report what arrived, not to decide
//! what is interesting. They do differ between Gateway implementations,
//! which would make a naive cross-implementation header comparison
//! noisy -- but that is a question for the Phase 6 comparator's own
//! normalization, and answering it here by hiding headers would destroy
//! evidence that the comparator cannot recover. Task 6.9 compares
//! backend identity, not header sets, so nothing downstream is
//! currently sensitive to this noise in the first place.

use std::collections::{BTreeMap, BTreeSet};

use hyper::header::{CONNECTION, HOST};
use hyper::{HeaderMap, Method, Uri};
use serde::{Deserialize, Serialize};

/// The hop-by-hop header names, lowercase -- RFC 9110 §7.6.1's own
/// list. Excluded from [`EchoBody::headers`]; see this module's own
/// documentation for why.
pub const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// The frozen echo response body (ROADMAP Task 6.5). Field order here
/// *is* the JSON key order: `serde_json` emits struct fields in
/// declaration order, and this order is part of the contract that
/// `crates/admissionlab-echo/tests/http.rs` pins.
///
/// `Deserialize` as well as `Serialize` so this crate's own tests parse
/// back exactly the type the server wrote, rather than re-describing
/// the contract in a second place where the two could drift apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EchoBody {
    /// Which backend answered: [`crate::config::BACKEND_ID_ENV`],
    /// verbatim. The field Task 6.8's probe reads and Task 6.9's
    /// comparator turns into `traffic_backend_changed`.
    pub backend: String,
    /// The request method as it arrived (`GET`, `POST`, ...), so a
    /// Gateway that rewrites the method is visible.
    pub method: String,
    /// The request target's path, without the query -- see this
    /// module's own documentation.
    pub path: String,
    /// The `Host` header, verbatim -- see this module's own
    /// documentation, including what an empty string means.
    pub host: String,
    /// Every non-hop-by-hop request header, lowercased and sorted.
    pub headers: BTreeMap<String, String>,
}

impl EchoBody {
    /// Builds the response body for one request.
    ///
    /// Takes the request's parts rather than the request itself so it
    /// stays independent of the body type (and of `hyper` request
    /// ownership), which is what lets both [`crate::serve::handle`] and
    /// this module's own tests call exactly this function.
    #[must_use]
    pub fn build(backend_id: &str, method: &Method, uri: &Uri, headers: &HeaderMap) -> Self {
        Self {
            backend: backend_id.to_owned(),
            method: method.as_str().to_owned(),
            path: uri.path().to_owned(),
            host: host(uri, headers),
            headers: normalize_headers(headers),
        }
    }
}

/// The `Host` header if there is one, else the request target's own
/// authority, else the empty string -- see this module's own
/// documentation.
fn host(uri: &Uri, headers: &HeaderMap) -> String {
    if let Some(value) = headers.get(HOST) {
        return decode(value.as_bytes());
    }
    uri.authority()
        .map(|authority| authority.as_str().to_owned())
        .unwrap_or_default()
}

/// Applies this module's documented normalization to the request's
/// headers.
fn normalize_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    let connection_listed = connection_tokens(headers);
    headers
        .keys()
        .filter(|name| {
            let name = name.as_str();
            !HOP_BY_HOP.contains(&name) && !connection_listed.contains(name)
        })
        .map(|name| {
            let joined = headers
                .get_all(name)
                .iter()
                .map(|value| decode(value.as_bytes()))
                .collect::<Vec<_>>()
                .join(", ");
            (name.as_str().to_owned(), joined)
        })
        .collect()
}

/// The header names the request's own `Connection` header nominates as
/// connection-specific, lowercased. RFC 9110 §7.6.1 makes these
/// hop-by-hop for this connection only, so they are excluded exactly as
/// the fixed list is.
///
/// `close` and `keep-alive` appear here as connection *options* rather
/// than header names; leaving them in the set is harmless (`keep-alive`
/// is already in [`HOP_BY_HOP`], and no header is named `close`) and
/// costs a special case that could only ever be wrong.
fn connection_tokens(headers: &HeaderMap) -> BTreeSet<String> {
    headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

/// Renders a header value's bytes as a JSON-safe string.
///
/// HTTP header values are opaque bytes, JSON strings are Unicode, so
/// something has to give for a value that is not valid UTF-8. Lossy
/// decoding (invalid sequences become U+FFFD) is chosen over dropping
/// the header, over failing the request, and over a `\xNN` escaping
/// scheme of this crate's own invention: dropping or failing would
/// destroy evidence about a request that really did arrive, an invented
/// escape would need every future reader of this contract to learn it,
/// and lossy decoding is deterministic -- the same bytes always produce
/// the same string, which is all a comparator needs. Real Gateway
/// traffic in these fixtures is ASCII; this path exists so that a
/// corrupted header is reported rather than crashing a probe.
fn decode(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use hyper::header::{HeaderName, HeaderValue};
    use hyper::{HeaderMap, Method, Uri};

    use super::{EchoBody, HOP_BY_HOP};

    fn headers(pairs: &[(&str, &[u8])]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).expect("test header names are well-formed"),
                HeaderValue::from_bytes(value).expect("test header values are well-formed"),
            );
        }
        map
    }

    fn build(uri: &str, pairs: &[(&str, &[u8])]) -> EchoBody {
        EchoBody::build(
            "echo-a",
            &Method::GET,
            &uri.parse::<Uri>().expect("test URIs are well-formed"),
            &headers(pairs),
        )
    }

    #[test]
    fn the_query_string_is_not_part_of_the_echoed_path() {
        let body = build("/payments?tier=gold&page=2", &[]);
        assert_eq!(body.path, "/payments");
    }

    #[test]
    fn the_host_header_is_echoed_verbatim_and_also_kept_in_the_headers() {
        let body = build("/", &[("host", b"api.example.test")]);
        assert_eq!(body.host, "api.example.test");
        assert_eq!(
            body.headers.get("host").map(String::as_str),
            Some("api.example.test"),
            "the header map records what arrived, including the host"
        );
    }

    /// An absolute-form request target carries the authority in the URI
    /// rather than in a header; an origin-form one with no `Host` at
    /// all leaves nothing to report, and reports nothing.
    #[test]
    fn a_missing_host_header_falls_back_to_the_target_authority_then_to_empty() {
        assert_eq!(
            build("http://api.example.test/payments", &[]).host,
            "api.example.test"
        );
        assert_eq!(
            build("/payments", &[]).host,
            "",
            "an empty host means the request carried none -- never a guess"
        );
    }

    #[test]
    fn header_names_are_lowercased_and_sorted() {
        let body = build(
            "/",
            &[("Z-Last", b"z"), ("A-First", b"a"), ("M-Middle", b"m")],
        );
        let names: Vec<&str> = body.headers.keys().map(String::as_str).collect();
        assert_eq!(names, vec!["a-first", "m-middle", "z-last"]);
    }

    #[test]
    fn every_hop_by_hop_header_is_excluded() {
        // Values are arbitrary; the point is the name. `connection`
        // deliberately lists no extra tokens here so this test covers
        // the fixed list alone.
        let pairs: Vec<(&str, &[u8])> = HOP_BY_HOP
            .iter()
            .map(|name| (*name, b"irrelevant" as &[u8]))
            .collect();
        let body = build("/", &pairs);
        assert!(
            body.headers.is_empty(),
            "every hop-by-hop header must be excluded, got {:?}",
            body.headers
        );
    }

    #[test]
    fn headers_named_by_the_connection_header_are_excluded_too() {
        let body = build(
            "/",
            &[
                ("connection", b"close, X-Per-Connection"),
                ("x-per-connection", b"transport detail"),
                ("x-kept", b"routing evidence"),
            ],
        );
        assert_eq!(
            body.headers.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["x-kept"],
            "a Connection-nominated header is hop-by-hop for this connection"
        );
    }

    /// Proxy-injected headers are real observed behavior of the data
    /// plane under test -- see this module's own documentation for why
    /// they are echoed rather than filtered as noise.
    #[test]
    fn proxy_injected_headers_are_echoed() {
        let body = build(
            "/",
            &[
                ("x-forwarded-for", b"10.0.0.1"),
                ("x-forwarded-proto", b"https"),
                ("x-envoy-attempt-count", b"1"),
                ("x-request-id", b"3f0c"),
            ],
        );
        assert_eq!(
            body.headers.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "x-envoy-attempt-count",
                "x-forwarded-for",
                "x-forwarded-proto",
                "x-request-id"
            ]
        );
    }

    #[test]
    fn a_repeated_header_is_joined_rather_than_dropped() {
        let body = build(
            "/",
            &[
                ("x-forwarded-for", b"10.0.0.1"),
                ("x-forwarded-for", b"10.0.0.2"),
            ],
        );
        assert_eq!(
            body.headers.get("x-forwarded-for").map(String::as_str),
            Some("10.0.0.1, 10.0.0.2"),
            "two sends of one header are one value, per RFC 9110 5.3"
        );
    }

    #[test]
    fn a_non_utf8_header_value_is_decoded_lossily_not_dropped() {
        let body = build("/", &[("x-binary", &[0x41, 0xff, 0x42])]);
        assert_eq!(
            body.headers.get("x-binary").map(String::as_str),
            Some("A\u{fffd}B"),
            "a corrupted value is still evidence that the header arrived"
        );
    }

    /// The frozen JSON contract, asserted on the serialized bytes:
    /// exactly five keys, in exactly this order.
    #[test]
    fn the_serialized_shape_is_the_frozen_one() {
        let body = EchoBody::build(
            "echo-a",
            &Method::GET,
            &"/payments".parse::<Uri>().expect("well-formed URI"),
            &headers(&[("host", b"api.example.test"), ("x-test", b"value")]),
        );
        let json = serde_json::to_string(&body).expect("the echo body always serializes");
        assert_eq!(
            json,
            r#"{"backend":"echo-a","method":"GET","path":"/payments","host":"api.example.test","headers":{"host":"api.example.test","x-test":"value"}}"#
        );
    }
}
