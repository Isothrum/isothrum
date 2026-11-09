//! The runner's tracing (its runs, steps, drivers and processes) on standard error, when
//! `ISOTHRUM_LOG` asks for it: a level (`debug`) or filter directives
//! (`isothrum_runner=debug,warn`), as text, or as one JSON object per line with
//! `ISOTHRUM_LOG_FORMAT=json` (for a log collector). Never a secret's value: the runner traces
//! programs and arguments, and values reach a tool through its environment.

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{EnvFilter, fmt};

/// A subscriber of either format.
type Subscriber = Box<dyn tracing::Subscriber + Send + Sync>;

/// How the log lines are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// Text for a person.
    Text,
    /// One JSON object per line.
    Json,
}

/// Why the tracing asked for can't be set up.
pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;

/// Traces to standard error as `filter` (`ISOTHRUM_LOG`) and `format` (`ISOTHRUM_LOG_FORMAT`:
/// `text`, the default, or `json`) ask.
///
/// # Errors
///
/// A filter that doesn't parse, a format that isn't one, a subscriber already set.
pub(crate) fn to_stderr(filter: &str, format: Option<&str>) -> Result<(), Error> {
    tracing::subscriber::set_global_default(subscriber(filter, format, std::io::stderr)?)?;
    Ok(())
}

/// The subscriber writing to `writer`.
fn subscriber<W>(filter: &str, format: Option<&str>, writer: W) -> Result<Subscriber, Error>
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let filter = EnvFilter::try_new(filter)?;
    let builder = fmt().with_env_filter(filter).with_writer(writer);
    Ok(match parse_format(format)? {
        Format::Text => Box::new(builder.finish()),
        Format::Json => Box::new(builder.json().finish()),
    })
}

/// The format `ISOTHRUM_LOG_FORMAT` names (text when it names none).
fn parse_format(format: Option<&str>) -> Result<Format, Error> {
    match format.map(str::trim).filter(|f| !f.is_empty()) {
        None => Ok(Format::Text),
        Some(f) if f.eq_ignore_ascii_case("text") => Ok(Format::Text),
        Some(f) if f.eq_ignore_ascii_case("json") => Ok(Format::Json),
        Some(other) => Err(format!("ISOTHRUM_LOG_FORMAT={other}: use `text` or `json`").into()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A writer whose lines a test reads back.
    #[derive(Clone, Default)]
    struct Lines(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Lines {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(b)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'w> MakeWriter<'w> for Lines {
        type Writer = Lines;
        fn make_writer(&'w self) -> Lines {
            self.clone()
        }
    }

    /// What `filter` and `format` write of a debug event of `isothrum_runner` and of `other`.
    fn traced(filter: &str, format: Option<&str>) -> String {
        let lines = Lines::default();
        let subscriber = subscriber(filter, format, lines.clone()).unwrap();
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "isothrum_runner::process", program = "docker", "starting a process");
            tracing::debug!(target: "other", "noise");
        });
        String::from_utf8(lines.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn a_directive_filters_by_crate_and_a_level_still_works() {
        let runner_only = traced("isothrum_runner=debug", None);
        assert!(runner_only.contains("starting a process") && !runner_only.contains("noise"), "{runner_only}");
        let everything = traced("debug", Some("text"));
        assert!(everything.contains("starting a process") && everything.contains("noise"), "{everything}");
        assert_eq!(traced("info", None), "");
    }

    #[test]
    fn the_json_format_writes_an_object_per_line() {
        let text = traced("isothrum_runner=debug", Some("JSON"));
        let line: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(line["fields"]["message"], "starting a process");
        assert_eq!((line["fields"]["program"].as_str(), line["level"].as_str()), (Some("docker"), Some("DEBUG")));
    }

    #[test]
    fn an_unknown_format_or_filter_is_refused() {
        assert!(parse_format(Some("yaml")).is_err());
        assert!(subscriber("isothrum_runner=loud", None, Lines::default()).is_err());
    }
}
