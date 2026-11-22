use std::{
    ffi::OsString,
    fs,
    io::{ErrorKind, Write as _},
    path::PathBuf,
};

pub(super) struct TokenStore {
    directory: PathBuf,
}

impl TokenStore {
    pub(super) fn open() -> Option<Self> {
        Self::at(state_folder(std::env::var_os)?).ok()
    }

    fn at(directory: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&directory)?;
        Ok(Self { directory })
    }

    fn path(&self) -> PathBuf {
        self.directory.join("desktop-token")
    }

    pub(super) fn consume(&self) -> anyhow::Result<Option<String>> {
        let path = self.path();
        let token = match fs::read_to_string(&path) {
            Ok(token) => token,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        fs::remove_file(path)?;
        Ok((!token.is_empty()).then_some(token))
    }

    pub(super) fn save(&self, token: &str) -> anyhow::Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        file.write_all(token.as_bytes())?;
        file.persist(self.path())?;
        Ok(())
    }
}

fn state_folder(var: impl Fn(&'static str) -> Option<OsString>) -> Option<PathBuf> {
    let absolute = |name| {
        var(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let base = absolute("XDG_STATE_HOME")
        .or_else(|| absolute("HOME").map(|home| home.join(".local/state")))?;
    Some(base.join("speakeasy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restore_token_is_replaced_and_consumed_once() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let store = TokenStore::at(directory.path().join("state"))?;
        store.save("fixture-one")?;
        store.save("fixture-two")?;
        assert_eq!(store.consume()?.as_deref(), Some("fixture-two"));
        assert!(store.consume()?.is_none());
        Ok(())
    }

    fn lookup(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            vars.iter()
                .find(|&&(key, _)| key == name)
                .map(|&(_, value)| value.into())
        }
    }

    #[test]
    fn the_state_folder_never_falls_back_to_the_working_directory() {
        let unusable: [&[(&str, &str)]; 3] = [
            &[],
            &[("HOME", "")],
            &[("XDG_STATE_HOME", "state"), ("HOME", "home")],
        ];
        for vars in unusable {
            assert_eq!(state_folder(lookup(vars)), None, "{vars:?}");
        }
        assert_eq!(
            state_folder(lookup(&[
                ("XDG_STATE_HOME", "state"),
                ("HOME", "/home/user")
            ])),
            Some(PathBuf::from("/home/user/.local/state/speakeasy"))
        );
        assert_eq!(
            state_folder(lookup(&[
                ("XDG_STATE_HOME", "/state"),
                ("HOME", "/home/user")
            ])),
            Some(PathBuf::from("/state/speakeasy"))
        );
    }
}
