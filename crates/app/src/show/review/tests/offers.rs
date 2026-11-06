use super::*;

#[test]
fn a_refresh_or_deletion_invalidates_old_row_actions() {
    let mut library = Library::default();
    library.replace(vec![row("first"), row("second")]);
    let offered = library.offer(1);
    library.replace(vec![row("newer"), row("first"), row("second")]);
    assert!(library.offered(offered).is_none());
    let offered = library.offer(2);
    library.select("first".into(), 0);
    assert_eq!(
        library.offered(offered).unwrap().directory,
        PathBuf::from("second")
    );
    library.update_retention(Path::new("second"), Some(true));
    assert_eq!(library.offered(offered).unwrap().pinned, Some(true));
    library.forget();
    assert!(library.offered(offered).is_none());
    assert_eq!(
        library.offered(library.offer(1)).unwrap().directory,
        PathBuf::from("second")
    );
    assert!(library.offered(library.offer(99)).is_none());
}
