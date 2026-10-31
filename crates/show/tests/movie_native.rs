//! Opt-in container recovery against the platform's synthetic native codec fixtures.

#![forbid(unsafe_code)]

use std::{
    fs::File,
    io::{Read as _, Seek as _},
    path::PathBuf,
    time::Duration,
};

use speakeasy_show::MoviePrefix;

#[test]
#[ignore = "Requires synthetic native codec fixtures and an explicit new output directory"]
fn native_fragments_recover_only_complete_sample_ranges() -> Result<(), Box<dyn std::error::Error>>
{
    let source = PathBuf::from(
        std::env::var_os("SPEAKEASY_MOVIE_FIXTURES").ok_or("Set SPEAKEASY_MOVIE_FIXTURES")?,
    );
    let output = PathBuf::from(
        std::env::var_os("SPEAKEASY_MOVIE_PREFIX_OUT").ok_or("Set SPEAKEASY_MOVIE_PREFIX_OUT")?,
    );
    std::fs::create_dir(&output)?;
    for (name, frames, seconds, truncated) in [
        ("moving.mp4", 120, 4, false),
        ("held.mp4", 180, 6, false),
        ("capped-closed.mp4", 120, 4, true),
    ] {
        let mut input = File::open(source.join(name))?;
        let prefix =
            MoviePrefix::read::<Box<dyn std::error::Error>>(input.metadata()?.len(), |range| {
                let mut bytes = vec![0; usize::try_from(range.end - range.start)?];
                input.seek(std::io::SeekFrom::Start(range.start))?;
                input.read_exact(&mut bytes)?;
                Ok(bytes)
            })?;
        assert_eq!(prefix.frames, frames);
        assert_eq!(prefix.duration, Duration::from_secs(seconds));
        assert_eq!(prefix.truncated, truncated);
        input.rewind()?;
        let mut destination = File::create_new(output.join(name))?;
        assert_eq!(
            std::io::copy(&mut input.take(prefix.bytes), &mut destination)?,
            prefix.bytes
        );
        destination.sync_all()?;
        println!("{name}: {prefix:?}");
    }
    Ok(())
}
