//! UI text for core and render errors (ADR-0017): errors carry data, the app words them.

use mmc_core::{AnchorProblem, Error, FieldProblem, MAX_CHILD_SIZE};

use crate::i18n::{tr, tr_args};

fn layer(l: mmc_core::Layer) -> String {
    tr(&format!("layer-{}", l.as_str()))
}

pub fn text(e: &Error) -> String {
    match e {
        Error::NotAProject => tr("err-not-a-project"),
        Error::NewerVersion { found, supported } => tr_args(
            "err-newer-version",
            &[
                ("found", &found.to_string()),
                ("supported", &supported.to_string()),
            ],
        ),
        Error::Storage(detail) => tr_args("err-storage", &[("detail", detail)]),
        Error::WorkingCopyInUse => tr("err-in-use"),
        Error::UnknownMapKind(k) => tr_args("err-unknown-map-kind", &[("kind", k)]),
        Error::UnknownMap(_) => tr("err-unknown-map"),
        Error::CorruptTile => tr("err-corrupt-tile"),
        Error::DerivedLayer(l) => tr_args("err-derived-layer", &[("layer", &layer(*l))]),
        Error::UnknownEntityKind(k) => tr_args("err-unknown-entity-kind", &[("kind", k)]),
        Error::UnknownEntity(_) => tr("err-unknown-entity"),
        Error::EntityExists(_) => tr("err-entity-exists"),
        Error::InvalidField { key, problem } => {
            let id = match problem {
                FieldProblem::Duplicate => "err-field-duplicate",
                FieldProblem::NoSuchField => "err-field-unknown",
                FieldProblem::NoSuchEntity => "err-field-no-entity",
                FieldProblem::WrongType => "err-field-type",
            };
            tr_args(id, &[("field", key)])
        }
        Error::CorruptEntity => tr("err-corrupt-entity"),
        Error::InvalidAnchor(p) => tr(match p {
            AnchorProblem::NotAnchored => "err-anchor-kind",
            AnchorProblem::WrongShape => "err-anchor-shape",
            AnchorProblem::NoGeometry => "err-anchor-geometry",
        }),
        Error::UnknownFeature(_) => tr("err-unknown-feature"),
        Error::UnknownGenerator(g) => tr_args("err-unknown-generator", &[("generator", g)]),
        Error::NoChildMaps(k) => tr_args(
            "err-no-child-maps",
            &[("kind", &crate::status::map_kind(*k))],
        ),
        Error::NoOutline(_) => tr("err-no-outline"),
        Error::InvalidScale => tr("err-invalid-scale"),
        Error::ChildTooLarge => tr_args(
            "err-child-too-large",
            &[("max", &format!("{MAX_CHILD_SIZE:.0}"))],
        ),
        Error::RootMap(_) => tr("err-root-map"),
        Error::NeverGenerated(_) => tr("err-never-generated"),
        Error::NoParentContext(_) => tr("err-no-parent-context"),
        Error::InvalidOutline => tr("err-invalid-outline"),
        Error::OutsideOutline => tr("err-outside-outline"),
        Error::UnknownObject(_) => tr("err-unknown-object"),
        Error::NotAnObjectLayer(l) => tr_args("err-not-object-layer", &[("layer", &layer(*l))]),
        Error::InvalidObject(_) => tr("err-invalid-object"),
        Error::MapHasChildren(_) => tr("err-map-has-children"),
        Error::OwnMapChoice(_) => tr("err-own-map-choice"),
        Error::StalePlan => tr("err-stale-plan"),
    }
}

pub fn render_text(e: &mmc_render::Error) -> String {
    match e {
        mmc_render::Error::Core(e) => text(e),
        mmc_render::Error::Gpu(detail) => tr_args("err-gpu", &[("detail", detail)]),
        mmc_render::Error::Png(e) => tr_args("err-png", &[("detail", &e.to_string())]),
        mmc_render::Error::Io(e) => tr_args("err-file", &[("detail", &e.to_string())]),
        mmc_render::Error::OutsideMap => tr("err-outside-map"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_string_id_exists() {
        let src = include_str!("errors.rs");
        let ids = src
            .split('"')
            .filter(|s| s.starts_with("err-") && s.len() > 4 && !s.contains(' '));
        for id in ids {
            assert_ne!(tr(id), id, "main.ftl has no {id}");
        }
        assert!(text(&Error::ChildTooLarge).contains("8192"));
    }
}
