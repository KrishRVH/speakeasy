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

proptest! {
    #[test]
    fn successive_scans_never_reassign_an_offered_index(scans in prop::collection::vec(prop::collection::vec(0_u8..12, 0..16), 0..20)) {
        let mut rows = Vec::new();
        for scan in scans {
            let before: Vec<_> = rows.iter().map(|row: &Offered| row.device.id.clone()).collect();
            let fresh = scan.iter().map(|id| device(&id.to_string(), "Microphone")).collect();
            keep_rows(&mut rows, fresh);
            for (id, row) in before.iter().zip(&rows) {
                prop_assert_eq!(id, &row.device.id);
            }
            for row in &rows {
                prop_assert_eq!(row.present, scan.iter().any(|id| id.to_string() == row.device.id));
            }
        }
    }
}
