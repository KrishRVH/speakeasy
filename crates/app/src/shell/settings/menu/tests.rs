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
