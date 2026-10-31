//! Recovery keeps video coverage distinct from the shorter annotation journal.

use super::*;

#[tokio::test]
async fn movie_recovery_retains_playable_prefix_beyond_the_annotation_journal() -> anyhow::Result<()>
{
    let parent = tempfile::tempdir()?;
    let interrupted = checkpoint(parent.path()).await?.interrupted()?;
    store::movie::tests::checkpoint(interrupted.directory(), true)?;
    let (_input, permit) = permit();
    let Published::Recovered { show, .. } = recover_with(interrupted.clone(), &permit, || {
        Ok(parent.path().to_owned())
    })?
    else {
        bail!("Movie recovery did not publish");
    };
    let entry = store::recall::Entry::open(show.directory())?;
    assert_eq!(entry.health(), store::recall::Health::Ok);
    let movie = entry.show.unwrap().movie.unwrap();
    let coverage = movie.coverage.unwrap();
    assert_eq!(coverage.end_ms, 1_250);
    assert_eq!(coverage.annotations_through_ms, 500);
    assert!(coverage.partial);
    assert!(!interrupted.directory().exists());
    assert!(
        fs::read_to_string(show.directory().join(speakeasy_show::Bundle::SUMMARY))?
            .contains("250–1250 ms")
    );
    Ok(())
}

#[tokio::test]
async fn movie_startup_failure_saves_stills_with_an_explicit_video_loss() -> anyhow::Result<()> {
    for mode in 0..4 {
        let parent = tempfile::tempdir()?;
        let interrupted = checkpoint(parent.path()).await?.interrupted()?;
        store::movie::tests::checkpoint(interrupted.directory(), false)?;
        let path = interrupted.directory().join(store::movie::FILE);
        match mode {
            0 => fs::remove_file(&path)?,
            1 => fs::write(&path, [])?,
            2 => {
                let mut bytes = fs::read(&path)?;
                let first = bytes.windows(4).position(|bytes| bytes == b"moof").unwrap();
                bytes.truncate(first + 12);
                fs::write(&path, bytes)?;
            },
            _ => {
                fs::remove_file(&path)?;
                fs::write(interrupted.directory().join(store::movie::HEADER), b"{")?;
            },
        }
        let (_input, permit) = permit();
        let Published::Recovered {
            show, movie_lost, ..
        } = recover_with(interrupted.clone(), &permit, || {
            Ok(parent.path().to_owned())
        })?
        else {
            bail!("Still recovery did not publish");
        };
        assert!(movie_lost);
        assert_eq!(
            store::recall::Entry::open(show.directory())?.health(),
            store::recall::Health::Ok
        );
        assert!(!show.directory().join(store::movie::FILE).exists());
        assert!(!interrupted.directory().exists());
        assert!(
            fs::read_to_string(show.directory().join(speakeasy_show::Bundle::SUMMARY))?
                .contains("Video could not be saved")
        );
    }
    Ok(())
}
