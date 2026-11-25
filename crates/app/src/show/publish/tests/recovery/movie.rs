use super::*;

#[tokio::test]
async fn movie_recovery_retains_playable_prefix_beyond_the_annotation_journal() -> anyhow::Result<()>
{
    let parent = tempfile::tempdir()?;
    let interrupted = checkpoint(parent.path()).await?.interrupted();
    store::movie::tests::checkpoint(interrupted.directory(), true)?;
    let (_input, permit) = permit();
    let Published::Recovered { show, .. } = recover_with(interrupted.clone(), &permit, || {
        Ok(parent.path().to_owned())
    })?
    else {
        bail!("Movie recovery didn't publish");
    };
    let entry = store::recall::Entry::open(show.directory(), None)?;
    let movie = entry.show?.movie.unwrap().metadata;
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
    enum Fault {
        Missing,
        Empty,
        Truncated,
    }
    for fault in [Fault::Missing, Fault::Empty, Fault::Truncated] {
        let parent = tempfile::tempdir()?;
        let interrupted = checkpoint(parent.path()).await?.interrupted();
        store::movie::tests::checkpoint(interrupted.directory(), false)?;
        let path = interrupted.directory().join(store::movie::FILE);
        match fault {
            Fault::Missing => fs::remove_file(&path)?,
            Fault::Empty => fs::write(&path, [])?,
            Fault::Truncated => {
                let mut bytes = fs::read(&path)?;
                let first = bytes.windows(4).position(|bytes| bytes == b"moof").unwrap();
                bytes.truncate(first + 12);
                fs::write(&path, bytes)?;
            },
        }
        let (_input, permit) = permit();
        let Published::Recovered {
            show,
            preservation: Preservation { movie_lost, .. },
            ..
        } = recover_with(interrupted.clone(), &permit, || {
            Ok(parent.path().to_owned())
        })?
        else {
            bail!("Still recovery didn't publish");
        };
        assert!(movie_lost);
        assert!(
            store::recall::Entry::open(show.directory(), None)?
                .show
                .is_ok()
        );
        assert!(!show.directory().join(store::movie::FILE).exists());
        assert!(!interrupted.directory().exists());
        assert!(
            fs::read_to_string(show.directory().join(speakeasy_show::Bundle::SUMMARY))?
                .contains("Video couldn't be saved")
        );
    }
    Ok(())
}
