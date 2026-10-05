//! What the pinned decoder says when it cannot make sense of a packet.
//!
//! `auto-artifactarium` answers a packet it cannot decrypt by brute-forcing the
//! seeds of the session it is holding, and then announcing the failure -- once
//! per search, plus a line per packet.  Those lines are a symptom rather than a
//! diagnostic, and a connection whose key no longer fits produces them by the
//! thousand: 2835 searches and 5670 lines over 14 minutes in the run that
//! prompted this, with nothing recorded.
//!
//! So they are dropped from the log and counted here instead.  The count is the
//! only reliable way for the rest of the program to learn that the decoder is
//! *searching and losing*, which is the state worth giving up on.  A packet
//! that decodes to nothing is emphatically not that signal, which is worth
//! spelling out because treating it as one cost a debugging round: a datagram
//! carrying only acknowledgements, a duplicate, or a segment the KCP receive
//! window refused all produce the very same empty batch.
//!
//! Counting happens while the event is on its way to the log, so it is one
//! line of code away from the verdict itself.  The price is that a `RUST_LOG`
//! which silences these messages also silences the count; that only ever costs
//! the automatic recovery described in `monitor`, never correctness.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Filter};

/// Written by the decoder for every seed it searches and cannot use.
const LOST_SEARCH: &str = "Unable to find the encryption key seed";

/// Written when a packet could not be decrypted at all, whether or not there
/// was anything left to search.
const NO_KEY: &str = "Couldn't bruteforce";

/// Searches the decoder has run and lost since this process started.
static LOST_SEARCHES: AtomicU64 = AtomicU64::new(0);

/// Packets the decoder could not decrypt at all since this process started.
static UNREADABLE: AtomicU64 = AtomicU64::new(0);

/// How many searches over the seeds the decoder has run and lost so far.
pub fn lost_searches() -> u64 {
    LOST_SEARCHES.load(Ordering::Relaxed)
}

/// How many packets the decoder has failed to decrypt so far.
///
/// This is the broader of the two counts and the one worth acting on: it covers
/// a session key that no longer fits *and* a connection whose key material was
/// never obtained at all, which look identical from the outside but produce
/// very different numbers of lost searches -- none in the second case, because
/// there is nothing to search.
pub fn unreadable_packets() -> u64 {
    UNREADABLE.load(Ordering::Relaxed)
}

/// Drops the decoder's per-packet complaints, counting the expensive one.
///
/// Matched on the text rather than on the level and target, because those are
/// shared with messages that do matter: `No dispatch key found` and `Didn't get
/// magic in try_new!` are how the pinned decoder announces that the game has
/// outgrown it, and they have to stay visible.
pub struct DropLostSearches;

impl<S: tracing::Subscriber> Filter<S> for DropLostSearches {
    fn enabled(&self, _: &tracing::Metadata<'_>, _: &Context<'_, S>) -> bool {
        true
    }

    fn event_enabled(&self, event: &tracing::Event<'_>, _: &Context<'_, S>) -> bool {
        let mut message = Message::default();
        event.record(&mut message);
        let Some(text) = message.0 else {
            return true;
        };

        if text.contains(LOST_SEARCH) {
            LOST_SEARCHES.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        if text.contains(NO_KEY) {
            UNREADABLE.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        true
    }
}

/// The `message` field of an event, however tracing decided to record it.
#[derive(Default)]
struct Message(Option<String>);

impl Visit for Message {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;

    /// Collects what a subscriber would have written, so a test can assert on
    /// the lines that would have reached the log file.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// The count is process-wide, so the tests that move it have to run one at
    /// a time -- otherwise one test's log line lands in the other's total.
    static COUNTING: Mutex<()> = Mutex::new(());

    fn counting() -> std::sync::MutexGuard<'static, ()> {
        COUNTING.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `events` through the same subscriber the core installs and returns
    /// what would have reached the log file.
    fn logged(events: impl FnOnce()) -> String {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(capture.clone())
                .with_filter(EnvFilter::new("warn,irminsul=info"))
                .with_filter(DropLostSearches),
        );

        tracing::subscriber::with_default(subscriber, events);

        String::from_utf8(capture.0.lock().unwrap().clone()).unwrap()
    }

    /// The decoder's per-packet complaints are dropped and nothing else is: a
    /// game update has to stay visible in the log, because those two remaining
    /// messages are the only announcement it makes.
    #[test]
    fn only_the_decoders_per_packet_complaints_are_dropped() {
        let _counting = counting();

        let log = logged(|| {
            tracing::warn!(target: "auto_artifactarium::crypto", "Unable to find the encryption key seed.");
            tracing::error!(target: "auto_artifactarium", "Couldn't bruteforce from deduced keys");
            tracing::error!(target: "auto_artifactarium", "No dispatch key found");
            tracing::error!(target: "auto_artifactarium", "Didn't get magic in try_new!");
            tracing::warn!(target: "irminsul::monitor", "the game's connection ended");
        });

        assert!(!log.contains("Unable to find the encryption key seed"), "{log}");
        assert!(!log.contains("Couldn't bruteforce"), "{log}");
        assert!(log.contains("No dispatch key found"), "{log}");
        assert!(log.contains("Didn't get magic in try_new!"), "{log}");
        assert!(log.contains("the game's connection ended"), "{log}");
    }

    /// The counts are what the monitor acts on, so they have to mean exactly
    /// what they say: a packet that could not be decrypted, and -- a subset of
    /// those -- a brute-force search over the seeds that was run and lost.  The
    /// gap between them is the connection whose key material was never obtained,
    /// where nothing is searched because there is nothing to search.
    #[test]
    fn the_two_failure_counts_are_kept_apart() {
        let _counting = counting();
        let before = (unreadable_packets(), lost_searches());

        logged(|| {
            tracing::error!(target: "auto_artifactarium", "Couldn't bruteforce from deduced keys");
            tracing::error!(target: "auto_artifactarium", "Couldn't bruteforce from deduced keys");
        });
        assert_eq!(
            unreadable_packets(),
            before.0 + 2,
            "a packet that failed without a search is still a packet that failed"
        );
        assert_eq!(lost_searches(), before.1, "but it is not a search");

        logged(|| {
            tracing::warn!(target: "auto_artifactarium::crypto", "Unable to find the encryption key seed.");
            tracing::warn!(target: "auto_artifactarium::crypto", "Unable to find the encryption key seed.");
            tracing::warn!(target: "auto_artifactarium::crypto", "Unable to find the encryption key seed.");
        });
        assert_eq!(lost_searches(), before.1 + 3);
        assert_eq!(
            unreadable_packets(),
            before.0 + 2,
            "a search that was lost belongs to a packet that was already counted"
        );
    }
}
