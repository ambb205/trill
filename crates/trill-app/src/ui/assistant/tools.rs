//! Tool calls in plain words.
//!
//! The assistant works by calling Trill's tools, and each call is shown in
//! the conversation as one line: what it is doing while it runs, what it did
//! once it has. The words for Trill's own tools are the server's
//! (`trill_mcp::words`), which the journal of a session reads too; here are
//! the words for the two things the assistant does that are not Trill's: a
//! search of the web, and a page read.
//!
//! The arguments and the answer in full are a click away (see the view).

use trill_assistant::WebLookup;
use trill_core::journal::words::quoted;
pub use trill_mcp::words::{Wording, describe};

/// An address as it is shown in a line: without the `https://` every
/// address has, and without a slash that only ends it. The whole of it is
/// kept otherwise: where the assistant went is not abridged.
fn address(url: &str) -> &str {
    let bare = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(url);
    bare.strip_suffix('/').unwrap_or(bare)
}

/// Says in words what a look at the web is: a search by its query, a page
/// by its address. These are the only things the assistant does that are
/// not Trill's own, so the line always says which and what.
pub fn describe_web(lookup: &WebLookup) -> Wording {
    match lookup {
        WebLookup::Search { query } => {
            Wording::about("Searching", "Searched", &format!("the web for {}", quoted(query)))
        }
        WebLookup::Page { url, .. } => Wording::about("Reading", "Read", address(url)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_look_at_the_web_reads_as_the_query_or_the_address() {
        let search = describe_web(&WebLookup::Search { query: "dorian mode chord progressions".into() });
        assert_eq!(search.doing, "Searching the web for \u{201C}dorian mode chord progressions\u{201D}");
        assert_eq!(search.did, "Searched the web for \u{201C}dorian mode chord progressions\u{201D}");

        let page = |url: &str| describe_web(&WebLookup::Page { url: url.into(), question: "What is it?".into() });
        assert_eq!(page("https://example.com/theory/dorian").doing, "Reading example.com/theory/dorian");
        assert_eq!(page("https://example.com/").did, "Read example.com");
        // Anything out of the ordinary about an address stays in sight.
        assert_eq!(page("http://127.0.0.1:7777/mcp").did, "Read 127.0.0.1:7777/mcp");
        assert_eq!(page("ftp://example.com/x").did, "Read ftp://example.com/x");
        assert_eq!(page("https://example.com/a?b=c#d").did, "Read example.com/a?b=c#d");
    }
}
