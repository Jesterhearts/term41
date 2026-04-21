#[cfg(feature = "testonly-perf-ctrl-c")]
mod enabled {
    use std::collections::HashMap;
    use std::collections::VecDeque;
    use std::collections::hash_map::Entry;
    use std::sync::LazyLock;
    use std::time::Instant;

    use parking_lot::Mutex;

    use crate::TabId;

    const CTRL_C: u8 = 0x03;
    const MAX_PENDING_HITS: usize = 256;

    #[derive(Default)]
    struct TabCtrlCPerf {
        pending_hits: VecDeque<Instant>,
        previous_byte_was_caret: bool,
    }

    static CTRL_C_HITS: LazyLock<Mutex<HashMap<TabId, TabCtrlCPerf>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(crate) fn record_ctrl_c_hit(tab_id: TabId) {
        let mut tabs = CTRL_C_HITS.lock();
        let tab = tabs.entry(tab_id).or_default();
        if tab.pending_hits.len() == MAX_PENDING_HITS {
            tab.pending_hits.pop_front();
        }
        tab.pending_hits.push_back(Instant::now());
    }

    pub(crate) fn observe_pty_output(
        tab_id: TabId,
        bytes: &[u8],
    ) {
        let mut tabs = CTRL_C_HITS.lock();
        let Entry::Occupied(mut tab_entry) = tabs.entry(tab_id) else {
            return;
        };
        let tab = tab_entry.get_mut();
        let ctrl_c_count = ctrl_c_markers_in_output(tab, bytes);
        for _ in 0..ctrl_c_count {
            let Some(start) = tab.pending_hits.pop_front() else {
                continue;
            };
            let elapsed = start.elapsed();
            log::info!(
                target: "term41::perf",
                "perf ctrl-c: PTY byte stream observed Ctrl-C after {} us ({:.3} ms)",
                elapsed.as_micros(),
                elapsed.as_secs_f64() * 1000.0,
            );
        }
    }

    fn ctrl_c_markers_in_output(
        tab: &mut TabCtrlCPerf,
        bytes: &[u8],
    ) -> usize {
        let mut count = 0;
        for &byte in bytes {
            if byte == CTRL_C {
                count += 1;
                tab.previous_byte_was_caret = false;
                continue;
            }

            if tab.previous_byte_was_caret && byte == b'C' {
                count += 1;
                tab.previous_byte_was_caret = false;
                continue;
            }

            tab.previous_byte_was_caret = byte == b'^';
        }
        count
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn matches_raw_etx() {
            let mut tab = TabCtrlCPerf::default();
            assert_eq!(ctrl_c_markers_in_output(&mut tab, b"\x03"), 1);
        }

        #[test]
        fn matches_cooked_echo_split_across_chunks() {
            let mut tab = TabCtrlCPerf::default();
            assert_eq!(ctrl_c_markers_in_output(&mut tab, b"before ^"), 0);
            assert_eq!(ctrl_c_markers_in_output(&mut tab, b"C after"), 1);
        }

        #[test]
        fn ignores_other_caret_sequences() {
            let mut tab = TabCtrlCPerf::default();
            assert_eq!(ctrl_c_markers_in_output(&mut tab, b"^X"), 0);
            assert_eq!(ctrl_c_markers_in_output(&mut tab, b"C"), 0);
        }
    }
}

#[cfg(not(feature = "testonly-perf-ctrl-c"))]
mod disabled {
    use crate::TabId;

    pub(crate) fn record_ctrl_c_hit(_tab_id: TabId) {}

    pub(crate) fn observe_pty_output(
        _tab_id: TabId,
        _bytes: &[u8],
    ) {
    }
}

#[cfg(not(feature = "testonly-perf-ctrl-c"))]
pub(crate) use disabled::*;
#[cfg(feature = "testonly-perf-ctrl-c")]
pub(crate) use enabled::*;
