use config41::FeaturePermissions;

use crate::C1Mode;
use crate::conformance;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ItermAction {
    AcceptedNoop,
    ReportCellSize,
    ReportCapabilities,
}

pub(super) fn parse(rest: &[u8]) -> ItermAction {
    if rest.starts_with(b"ReportCellSize") {
        return ItermAction::ReportCellSize;
    }
    if rest == b"Capabilities" {
        return ItermAction::ReportCapabilities;
    }
    ItermAction::AcceptedNoop
}

pub(super) fn apply(
    action: ItermAction,
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    feature_permissions: &FeaturePermissions,
    cell_width: u32,
    cell_height: u32,
) {
    match action {
        ItermAction::AcceptedNoop => {}
        ItermAction::ReportCellSize => {
            conformance::write_osc(
                pending_output,
                c1_mode,
                format_args!("1337;ReportCellSize={cell_height};{cell_width}"),
            );
        }
        ItermAction::ReportCapabilities => {
            let features = crate::iterm_features::term_features(feature_permissions);
            conformance::write_osc(
                pending_output,
                c1_mode,
                format_args!("1337;Capabilities={features}"),
            );
        }
    }
}
