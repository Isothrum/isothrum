//! What `osascript` prints when an Apple event fails, read by its error number: the number in
//! parentheses ends the message in every language (`UTM got an error: Application isn't running.
//! (-600)`). UTM's Vagrant provider (`vagrant_utm`) drives UTM through `osascript`, and its
//! failures carry that line.

/// An Apple event's error number the runner acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppleEvent {
    /// `-600`: "Application isn't running", which UTM answers of itself while it runs, right
    /// after an import or while a VM boots.
    AppNotRunning,
    /// `-1712`: the event timed out, UTM busy.
    TimedOut,
    /// Any other number (`-1743`, the Automation permission refused: no wait fixes it).
    Other(i32),
}

impl AppleEvent {
    /// The last error number `lines` name (`(-<number>)`); none when they name none.
    pub(crate) fn of<'a>(lines: impl IntoIterator<Item = &'a String>) -> Option<AppleEvent> {
        let mut last = None;
        for line in lines {
            for (at, _) in line.match_indices("(-") {
                let rest = line.get(at + 1..).unwrap_or_default();
                if let Some(n) = rest.find(')').and_then(|end| rest.get(..end)).and_then(|n| n.parse::<i32>().ok()) {
                    last = Some(n);
                }
            }
        }
        Some(match last? {
            -600 => AppleEvent::AppNotRunning,
            -1712 => AppleEvent::TimedOut,
            other => AppleEvent::Other(other),
        })
    }

    /// Its error number.
    pub(crate) fn number(self) -> i32 {
        match self {
            AppleEvent::AppNotRunning => -600,
            AppleEvent::TimedOut => -1712,
            AppleEvent::Other(n) => n,
        }
    }

    /// Whether it passes: UTM answers the same event a moment later.
    pub(crate) fn passes(self) -> bool {
        matches!(self, AppleEvent::AppNotRunning | AppleEvent::TimedOut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(text: &str) -> Option<AppleEvent> {
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        AppleEvent::of(&lines)
    }

    #[test]
    fn an_apple_event_error_is_read_by_its_number() {
        assert_eq!(
            of(include_str!("fixtures/osascript/vagrant_utm-forwarded-ports-not-running.txt")),
            Some(AppleEvent::AppNotRunning)
        );
        assert_eq!(
            of(include_str!("fixtures/osascript/vagrant_utm-forwarded-ports-timed-out.txt")),
            Some(AppleEvent::TimedOut)
        );
        assert_eq!(
            of("erreur d’exécution : Non autorisé à envoyer des Apple Events à UTM. (-1743)"),
            Some(AppleEvent::Other(-1743))
        );
        assert_eq!(of("exit status 1"), None);
        assert!(AppleEvent::AppNotRunning.passes() && AppleEvent::TimedOut.passes() && !AppleEvent::Other(-1743).passes());
    }
}
