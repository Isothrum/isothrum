//! What a cloud still holds tagged with an expiry, read from its own command line's JSON: the
//! tags Isoloom's modules put on every resource (`isoloom-expires-at = <unix seconds>` and
//! `isoloom-instance = <the environment's name>` as key-value tags; on the clouds whose tags are
//! words, `isoloom-expires-<unix seconds>` and the environment's name). Each cloud's query is a
//! read-only argv; its answer is parsed by a pure function.

use isoloom_manifest::Cloud;
use serde_json::Value;

use crate::process::ProcessSpec;
use isoloom_manifest::contract::{EXPIRES_TAG, EXPIRES_WORD, INSTANCE_TAG, RESERVED_PREFIX};

/// A resource a cloud holds with Isoloom's tags.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct Tagged {
    /// Its id as the cloud names it (an ARN, a resource id, a droplet's number).
    pub id: String,
    /// What it is (`ec2 instance`, `Microsoft.Compute/virtualMachines`, `droplet`).
    pub kind: String,
    /// When its environment ends, in seconds since the Unix epoch, when it says.
    pub expires_at: Option<u64>,
    /// Its environment's name (`isoloom-solo-web-ab12cd34`), when it says.
    pub instance: Option<String>,
}

/// A resource with Isoloom's expiry tag whose value isn't a time: whether it is past its expiry
/// is unknown, so it is said rather than taken for one that isn't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unreadable {
    /// Its id as the cloud names it.
    pub(crate) id: String,
    /// Its environment's name, when it says.
    pub(crate) instance: Option<String>,
    /// The expiry tag's value, as the cloud gives it.
    pub(crate) value: String,
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}`: its expiry tag `{}` isn't a time", self.id, self.value)
    }
}

/// What a query's answer says of one resource: it, or why its expiry can't be read.
pub(crate) type Found = Result<Tagged, Unreadable>;

/// Where a query looks: the regions (AWS) or the project (Google Cloud); the CLI's own default
/// when unset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Place {
    /// AWS regions, each queried.
    pub regions: Vec<String>,
    /// The Google Cloud project.
    pub project: Option<String>,
}

impl Place {
    /// AWS `regions` (each queried) and the Google Cloud `project`.
    #[must_use]
    pub fn new(regions: Vec<String>, project: Option<String>) -> Place {
        Place { regions, project }
    }
}

/// The read-only queries listing `cloud`'s tagged resources in `place`: one per AWS region, one
/// otherwise.
pub(crate) fn queries(cloud: Cloud, place: &Place) -> Vec<ProcessSpec> {
    let cli = cloud_cli(cloud);
    let query = |args: Vec<String>| ProcessSpec::new(cli, args);
    let owned = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<String>>();
    match cloud {
        Cloud::Aws => {
            let filter = format!("Key={EXPIRES_TAG}");
            let base = owned(&["resourcegroupstaggingapi", "get-resources", "--tag-filters", &filter, "--output", "json"]);
            let one = |region: Option<&str>| {
                let tail = region.map(|r| vec!["--region".to_owned(), r.to_owned()]).unwrap_or_default();
                query(base.iter().cloned().chain(tail).collect())
            };
            if place.regions.is_empty() {
                vec![one(None)]
            } else {
                place.regions.iter().map(|r| one(Some(r))).collect()
            }
        }
        Cloud::Azure => vec![query(owned(&["resource", "list", "--tag", EXPIRES_TAG, "--output", "json"]))],
        Cloud::Gcp => {
            let filter = format!("--filter=labels.{EXPIRES_TAG}:*");
            let mut args = owned(&["compute", "instances", "list", &filter, "--format=json"]);
            if let Some(p) = &place.project {
                args.push(format!("--project={p}"));
            }
            vec![query(args)]
        }
        Cloud::Digitalocean => vec![query(owned(&["compute", "droplet", "list", "--output", "json"]))],
        Cloud::Linode => vec![query(owned(&["linodes", "list", "--json"]))],
        Cloud::Oci => {
            let search = format!("query all resources where (freeformTags.key = '{EXPIRES_TAG}')");
            vec![query(owned(&[
                "search",
                "resource",
                "structured-search",
                "--query-text",
                &search,
                "--output",
                "json",
            ]))]
        }
    }
}

/// The tagged resources in `cloud`'s answer `json` to its query (those without Isoloom's expiry
/// tag left out on the clouds whose query can't filter by it), each one whose expiry tag isn't a
/// time said as [`Unreadable`].
///
/// # Errors
///
/// The answer isn't JSON, or isn't the shape the cloud's command line gives.
pub(crate) fn parse(cloud: Cloud, json: &str) -> Result<Vec<Found>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let shape = || format!("not the shape {} gives", cloud_cli(cloud));
    let items: Vec<Found> = match cloud {
        Cloud::Aws => list(v.get("ResourceTagMappingList")).ok_or_else(shape)?.iter().filter_map(aws).collect(),
        Cloud::Azure => list(Some(&v)).ok_or_else(shape)?.iter().filter_map(azure).collect(),
        Cloud::Gcp => list(Some(&v)).ok_or_else(shape)?.iter().filter_map(gcp).collect(),
        Cloud::Digitalocean => list(Some(&v)).ok_or_else(shape)?.iter().filter_map(|d| words(d, "id", "droplet")).collect(),
        Cloud::Linode => list(Some(&v)).ok_or_else(shape)?.iter().filter_map(|l| words(l, "id", "linode")).collect(),
        Cloud::Oci => list(v.get("data").and_then(|d| d.get("items")))
            .ok_or_else(shape)?
            .iter()
            .filter_map(oci)
            .collect(),
    };
    Ok(items)
}

/// The command line `cloud`'s query runs.
pub(super) fn cloud_cli(cloud: Cloud) -> &'static str {
    match cloud {
        Cloud::Aws => "aws",
        Cloud::Azure => "az",
        Cloud::Gcp => "gcloud",
        Cloud::Digitalocean => "doctl",
        Cloud::Linode => "linode-cli",
        Cloud::Oci => "oci",
    }
}

fn list(v: Option<&Value>) -> Option<&Vec<Value>> {
    v?.as_array()
}

fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A resource from its id, kind and key-value tags (an object of strings).
fn keyed(id: Option<String>, kind: String, tags: Option<&Value>) -> Option<Found> {
    let tags = tags?.as_object()?;
    let tagged = Tagged {
        id: id?,
        kind,
        expires_at: None,
        instance: text(tags.get(INSTANCE_TAG)),
    };
    Some(match text(tags.get(EXPIRES_TAG)) {
        Some(at) => expiring(tagged, &at),
        None => Ok(tagged),
    })
}

/// `tagged` expiring at `at`, its expiry tag's value: [`Unreadable`] when it isn't a time.
fn expiring(tagged: Tagged, at: &str) -> Found {
    match at.trim().parse() {
        Ok(at) => Ok(Tagged {
            expires_at: Some(at),
            ..tagged
        }),
        Err(_) => Err(Unreadable {
            id: tagged.id,
            instance: tagged.instance,
            value: at.to_owned(),
        }),
    }
}

/// An AWS resource: its ARN, its service and type (`ec2 instance`), its `Tags` list.
fn aws(r: &Value) -> Option<Found> {
    let arn = text(r.get("ResourceARN"))?;
    let tags: serde_json::Map<String, Value> = list(r.get("Tags"))?
        .iter()
        .filter_map(|t| Some((text(t.get("Key"))?, t.get("Value")?.clone())))
        .collect();
    let mut parts = arn.split(':');
    let service = parts.nth(2).unwrap_or_default().to_owned();
    let kind = parts.nth(2).and_then(|resource| resource.split(['/', ':']).next()).unwrap_or_default();
    keyed(Some(arn.clone()), format!("{service} {kind}").trim().to_owned(), Some(&Value::Object(tags)))
}

fn azure(r: &Value) -> Option<Found> {
    keyed(text(r.get("id")), text(r.get("type")).unwrap_or_default(), r.get("tags"))
}

fn gcp(r: &Value) -> Option<Found> {
    let id = text(r.get("selfLink")).or_else(|| text(r.get("name")));
    keyed(id, "compute instance".into(), r.get("labels"))
}

fn oci(r: &Value) -> Option<Found> {
    keyed(
        text(r.get("identifier")),
        text(r.get("resource-type")).unwrap_or_default(),
        r.get("freeform-tags"),
    )
}

/// A resource whose tags are words (DigitalOcean, Linode): its expiry from
/// `isoloom-expires-<time>`, its environment the other `isoloom-…` word; none without the
/// expiry word.
fn words(r: &Value, id: &str, kind: &str) -> Option<Found> {
    let tags: Vec<String> = list(r.get("tags"))?.iter().filter_map(|t| text(Some(t))).collect();
    let expires = tags.iter().find_map(|t| t.strip_prefix(EXPIRES_WORD))?;
    let tagged = Tagged {
        id: text(r.get(id))?,
        kind: kind.to_owned(),
        expires_at: None,
        instance: tags.iter().find(|t| is_instance_word(t)).cloned(),
    };
    Some(expiring(tagged, expires))
}

/// The environments a Terraform state's resources carry (their `isoloom-instance` tag, or the
/// `isoloom-…` word beside an expiry word): what a record covers.
pub(crate) fn instances_in_state(state: &Value, out: &mut std::collections::BTreeSet<String>) {
    match state {
        Value::Object(o) => {
            if let Some(Value::String(i)) = o.get(INSTANCE_TAG) {
                out.insert(i.clone());
            }
            for v in o.values() {
                instances_in_state(v, out);
            }
        }
        Value::Array(a) => {
            let words: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            if words.iter().any(|w| w.starts_with(EXPIRES_WORD)) {
                out.extend(words.iter().filter(|w| is_instance_word(w)).map(|w| (*w).to_owned()));
            }
            for v in a {
                instances_in_state(v, out);
            }
        }
        _ => {}
    }
}

/// Whether word tag `word` names an environment: Isoloom's, and not the expiry's.
fn is_instance_word(word: &str) -> bool {
    word.starts_with(RESERVED_PREFIX) && !word.starts_with(EXPIRES_WORD)
}
