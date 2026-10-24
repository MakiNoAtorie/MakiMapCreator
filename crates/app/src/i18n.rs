//! UI string lookup (ADR-0017). Every UI string goes through `tr`.

use std::sync::LazyLock;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};

static BUNDLE: LazyLock<FluentBundle<FluentResource>> = LazyLock::new(|| {
    let src = include_str!("../i18n/en-US/main.ftl");
    let res = FluentResource::try_new(src.to_owned()).expect("main.ftl parses");
    let mut b = FluentBundle::new_concurrent(vec!["en-US".parse().expect("valid locale")]);
    // Isolation marks show as boxes in egui.
    b.set_use_isolating(false);
    b.add_resource(res).expect("main.ftl has no duplicate ids");
    b
});

/// The UI string `id` with `args` filled in. A missing id returns the id, so it shows in the UI.
pub fn tr_args(id: &str, args: &[(&str, &str)]) -> String {
    let Some(pattern) = BUNDLE.get_message(id).and_then(|m| m.value()) else {
        return id.to_owned();
    };
    let mut fa = FluentArgs::new();
    for (k, v) in args {
        fa.set(*k, *v);
    }
    let mut errors = Vec::new();
    BUNDLE
        .format_pattern(pattern, Some(&fa), &mut errors)
        .into_owned()
}

pub fn tr(id: &str) -> String {
    tr_args(id, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_id_resolves() {
        assert_eq!(tr("menu-file"), "File");
    }

    #[test]
    fn args_fill_in_without_isolation_marks() {
        assert_eq!(tr_args("status-elevation", &[("h", "-12")]), "-12 m");
    }

    #[test]
    fn unit_suffixes_keep_their_leading_space() {
        assert_eq!((tr("unit-px"), tr("unit-m")), (" px".into(), " m".into()));
    }

    #[test]
    fn every_kind_layer_and_option_has_a_string() {
        use mmc_core::MapKindModule;
        let kinds: [&dyn MapKindModule; 2] =
            [&mmc_kind_region::RegionKind, &mmc_kind_world::WorldKind];
        for k in kinds {
            let layers = k.layers().iter().map(|l| format!("layer-{}", l.as_str()));
            let options = k.options().iter().map(|p| format!("option-{}", p.id));
            for id in layers.chain(options) {
                assert_ne!(tr(&id), id);
            }
        }
    }

    #[test]
    fn missing_id_returns_the_id() {
        assert_eq!(tr("no-such-string"), "no-such-string");
    }
}
