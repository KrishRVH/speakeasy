use super::*;

#[test]
fn publication_and_export_correct_headers_without_changing_the_recorded_source()
-> anyhow::Result<()> {
    let source = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    checkpoint(source.path(), false)?;
    let (original, flag) = fixture::padded(fs::read(source.path().join(FILE))?, 0, &[0; 4], None);
    fs::write(source.path().join(FILE), &original)?;
    let mut expected = original.clone();
    expected[flag] = 17;
    let mut reading = open(source.path())?.unwrap();
    let mut exported = Vec::new();
    reading.copy_to(&mut exported, MoviePrefix::FILE_LIMIT)?;
    assert_eq!(exported, expected);
    retain(destination.path(), reading, Moment::from_millis(500))?;
    assert_eq!(fs::read(source.path().join(FILE))?, original);
    assert_eq!(fs::read(destination.path().join(FILE))?, expected);
    let mut saved = Reading::open(destination.path(), Moment::from_millis(500))?;
    assert_eq!(saved.movie.prefix.frames, 2);
    assert_eq!(saved.movie.prefix.duration, Duration::from_secs(2));
    let mut exported = Vec::new();
    saved.copy_to(&mut exported, MoviePrefix::FILE_LIMIT)?;
    assert_eq!(exported, expected);
    Ok(())
}
