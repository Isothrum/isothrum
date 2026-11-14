//! What the tools print when a previous run's leftover is in a failed `up`'s way
//! ([`Leftover`]): the manifest names the leftover, the runner owns the tools' words for it. Read
//! whatever their case.

use isoloom_manifest::Leftover;

/// The lines' words that mean `leftover` is in the way, any case; none for a leftover this runner
/// doesn't know yet.
pub(crate) fn markers(leftover: Leftover) -> &'static [&'static str] {
    match leftover {
        // VirtualBox's `VERR_ALREADY_EXISTS`, and `Could not rename the directory`.
        Leftover::VirtualboxNameTaken => &["verr_already_exists", "could not rename"],
        // Vagrant: `… because it is locked!`.
        Leftover::MachineLocked => &["is locked"],
        // Vagrant: `… with a different provider`, `… a single provider at a time`.
        Leftover::OtherProvider => &["different provider", "single provider at a time"],
        // Docker: `… is already in use by container …`.
        Leftover::ContainerNameTaken => &["is already in use by container"],
        // Docker: `network with name … already exists`; Compose: `… has incorrect label …`.
        Leftover::NetworkLeft => &["network with name", "has incorrect label"],
        _ => &[],
    }
}

/// The words of every one of `leftovers` ([`markers`]).
pub(crate) fn markers_of(leftovers: &[Leftover]) -> Vec<String> {
    leftovers.iter().flat_map(|l| markers(*l)).map(|m| (*m).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_leftover_is_read_in_the_words_its_tools_print() {
        let vagrant = markers_of(&[Leftover::VirtualboxNameTaken, Leftover::MachineLocked, Leftover::OtherProvider]);
        assert_eq!(
            vagrant,
            [
                "verr_already_exists",
                "could not rename",
                "is locked",
                "different provider",
                "single provider at a time"
            ]
        );
        let compose = markers_of(&[Leftover::ContainerNameTaken, Leftover::NetworkLeft]);
        assert_eq!(compose, ["is already in use by container", "network with name", "has incorrect label"]);
    }
}
