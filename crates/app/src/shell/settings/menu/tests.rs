use proptest::prelude::*;

use super::*;

fn device(id: &str, name: &str) -> InputDevice {
    InputDevice {
        id: id.into(),
        name: name.into(),
    }
}

#[test]
fn a_scan_keeps_offered_positions_and_marks_disconnected_devices() {
    let mut rows = Vec::new();
    keep_rows(&mut rows, vec![device("a", "A"), device("b", "B")]);
    keep_rows(
        &mut rows,
        vec![
            device("b", "Renamed"),
            device("c", "C"),
            device("b", "Duplicate"),
        ],
    );
    let offered: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row.device.id.as_str(),
                row.device.name.as_str(),
                row.present,
            )
        })
        .collect();
    assert_eq!(
        offered,
        [("a", "A", false), ("b", "Renamed", true), ("c", "C", true)]
    );
    keep_rows(&mut rows, vec![device("a", "Reconnected")]);
    assert_eq!(rows[0].device.name, "Reconnected");
    assert!(rows[0].present);
    assert!(!rows[1].present);
}

fn ids(rows: &[Offered]) -> Vec<String> {
    rows.iter().map(|row| row.device.id.clone()).collect()
}

proptest! {
    #[test]
    fn successive_scans_never_reassign_an_offered_index(scans in prop::collection::vec(prop::collection::vec(0_u8..12, 0..16), 0..20)) {
        let mut rows = Vec::new();
        for scan in scans {
            let scanned: Vec<String> = scan.iter().map(u8::to_string).collect();
            let before = ids(&rows);
            keep_rows(&mut rows, scanned.iter().map(|id| device(id, "Microphone")).collect());
            prop_assert!(ids(&rows).starts_with(&before), "A scan moved or dropped an offered row");
            for id in &scanned {
                prop_assert!(rows.iter().any(|row| row.present && row.device.id == *id), "A scanned device has no present row");
            }
            for row in &rows {
                prop_assert_eq!(row.present, scanned.contains(&row.device.id));
            }
        }
    }
}
