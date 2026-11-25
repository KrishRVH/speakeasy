use speakeasy_show::{Image, Moment, Size, TakeEvent};

use super::*;

fn surface() -> Size {
    Size {
        width: 800.0,
        height: 600.0,
    }
}

async fn capture(parent: &Path) -> anyhow::Result<Checkpoint> {
    let (checkpoint, mut writer) =
        Keeper::fixture(parent, 1_000, surface(), speakeasy_show::Framing::Automatic)?;
    let mut take = Take::new(1_000, surface(), speakeasy_show::Framing::Automatic);
    let event = take.arrive(Moment::from_millis(100), || {
        Some(Image::from_bgra(2, 2, [3, 2, 1, 255].repeat(4)).unwrap())
    })?;
    assert!(writer.send(event));
    assert!(writer.send(TakeEvent::End {
        at: Moment::from_millis(500),
        ending: speakeasy_show::Ending::Finished
    }));
    drop(writer);
    checkpoint
        .close()
        .await?
        .context("Fixture checkpoint is missing")
}

#[tokio::test]
async fn only_unlocked_interrupted_shows_are_listed_and_discard_removes_them() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let checkpoint = capture(root.path()).await?;
    assert_eq!(list(root.path(), now), []);
    let path = checkpoint.directory.clone();
    assert!(checkpoint.replay()?.ended());
    drop(checkpoint);
    let rows = list(root.path(), now);
    assert_eq!(rows.len(), 1);
    let held = rows[0].lock()?;
    assert!(rows[0].lock().is_err());
    held.discard()?;
    assert!(!path.exists());
    assert_eq!(list(root.path(), now), []);
    Ok(())
}

#[tokio::test]
async fn a_torn_end_recovers_a_prefix_and_expiry_uses_the_original_stamp() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let checkpoint = capture(root.path()).await?;
    {
        let file = OpenOptions::new()
            .write(true)
            .open(checkpoint.directory.join(LOG))?;
        file.set_len(file.metadata()?.len().saturating_sub(1))?;
    }
    let take = checkpoint.replay()?;
    assert!(!take.ended());
    assert_eq!(take.finish().0.duration.as_millis(), 100);
    let path = checkpoint.directory.clone();
    drop(checkpoint);
    let later = now + std::time::Duration::from_hours(192);
    assert_eq!(list(root.path(), later), []);
    assert!(path.exists(), "Listing must not perform deletion");
    sweep(root.path(), later)?;
    assert!(!path.exists());
    Ok(())
}

#[tokio::test]
async fn discard_finds_an_interrupted_show_by_its_id_or_path_only() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let checkpoint = capture(root.path()).await?;
    let path = checkpoint.directory.clone();
    drop(checkpoint);
    let interrupted = list(root.path(), now).remove(0);
    assert_eq!(
        resolve(root.path(), Path::new(&interrupted.id()))?,
        interrupted
    );
    assert_eq!(resolve(root.path(), &path)?, interrupted);
    assert!(resolve(root.path(), Path::new(path.file_name().unwrap())).is_err());
    Ok(())
}
